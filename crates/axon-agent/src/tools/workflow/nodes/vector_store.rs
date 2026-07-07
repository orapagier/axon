//! Vector Store (`vectorStore` / *Neocortex*) — Task 4.2. Makes semantic
//! retrieval a workflow step: embed → upsert, or embed → search, against a
//! **pre-created** Qdrant collection (`documents` / `entities` / a custom one —
//! see `qdrant/create-collections.sh`). This node deliberately does NOT create
//! or configure collections; that's ops/deployment territory, not a canvas
//! action.
//!
//! Reuses the memory system's plumbing rather than inventing new plumbing:
//! - `Embedder::from_settings` — the same `embedder.*`-settings-driven,
//!   provider-agnostic embedder `LongTermMemory`/`ToolRouter` already use.
//! - `QDRANT_URL` / `QDRANT_API_KEY` env vars — the same connection
//!   `LongTermMemory::new` reads. A fresh `Qdrant` client is constructed per
//!   execution (per the `tool_router.rs` precedent) rather than reaching into
//!   `MemoryStore`, whose `qdrant` field is private and hardcoded to one
//!   collection.
//!
//! Three operations:
//!   - `upsert` — embed `text` (falling back to the primary input, like
//!     `htmlExtract`'s `body`/`html`/`data`/`text` probing) and store it as one
//!     point. `id` is optional (auto UUID v4 when blank); `metadata` merges
//!     extra payload fields alongside the stored `text`.
//!   - `search` — embed `query` (same fallback) and return the top `limit`
//!     nearest points as a **bare array** of `{ id, score, ...payload }` (the
//!     list-node convention — composes with Filter/Sort-Limit/Loop directly).
//!     An optional `filter` (equality rows) narrows the search server-side.
//!   - `delete` — remove by `id`, or by `filter` when `id` is blank.
//!
//! Collection existence is checked up front: a missing collection is a
//! teaching error naming `qdrant/create-collections.sh`, never an
//! auto-create.

use crate::memory::embeddings::Embedder;
use crate::state::AppState;
use qdrant_client::qdrant::point_id::PointIdOptions;
use qdrant_client::qdrant::r#match::MatchValue;
use qdrant_client::qdrant::{
    Condition, DeletePointsBuilder, Filter, PointId, PointStruct, PointsIdsList,
    SearchPointsBuilder, UpsertPointsBuilder,
};
use qdrant_client::{Payload, Qdrant};
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// Pull embeddable text out of a value: a non-blank string is taken as-is; an
/// object is probed at the conventional text-carrying fields; an array defers
/// to its first element. Mirrors `htmlExtract`'s `html_from_value`.
fn text_from_value(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Object(m) => ["text", "content", "body", "data"]
            .iter()
            .find_map(|k| m.get(*k).and_then(text_from_value)),
        Value::Array(a) => a.first().and_then(text_from_value),
        _ => None,
    }
}

/// Resolve the point ID for an upsert: an explicit configured value (string or
/// number) is used verbatim — a numeric-looking value becomes Qdrant's `Num`
/// variant, anything else rides as the `Uuid`-oneof string variant (Qdrant
/// validates UUID format server-side; a malformed id surfaces as that error).
/// Blank/absent auto-generates a fresh UUID v4.
fn resolve_point_id(configured: Option<&Value>) -> PointId {
    match configured {
        Some(Value::String(s)) if !s.trim().is_empty() => {
            let s = s.trim();
            match s.parse::<u64>() {
                Ok(n) => PointId::from(n),
                Err(_) => PointId::from(s.to_string()),
            }
        }
        Some(Value::Number(n)) => match n.as_u64() {
            Some(u) => PointId::from(u),
            None => PointId::from(n.to_string()),
        },
        _ => PointId::from(uuid::Uuid::new_v4().to_string()),
    }
}

fn point_id_to_string(id: Option<&PointId>) -> String {
    match id.and_then(|p| p.point_id_options.as_ref()) {
        Some(PointIdOptions::Num(n)) => n.to_string(),
        Some(PointIdOptions::Uuid(s)) => s.clone(),
        None => String::new(),
    }
}

/// Parse the `metadata` config into an object of extra payload fields. Accepts
/// a real object (a pure `{{ expr }}` field preserves JSON type through
/// `interpolate_config`) or a JSON-object string (typed literally); blank/null
/// is no metadata. Anything else is a teaching error rather than silently
/// dropped.
fn metadata_object(v: Option<&Value>) -> Result<Map<String, Value>, String> {
    match v {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(m)) => Ok(m.clone()),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(Map::new()),
        Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Object(m)) => Ok(m),
            _ => Err(
                "Vector Store: Metadata must be a JSON object, e.g. { \"source\": \"kb\" }"
                    .to_string(),
            ),
        },
        Some(other) => Err(format!(
            "Vector Store: Metadata must be a JSON object, got {other}"
        )),
    }
}

/// Build the point payload: the embedded `text` plus any extra metadata
/// fields (metadata wins on a key conflict with `text` itself).
fn build_payload(text: &str, metadata: &Map<String, Value>) -> Payload {
    let mut map = Map::new();
    map.insert("text".to_string(), Value::String(text.to_string()));
    for (k, v) in metadata {
        map.insert(k.clone(), v.clone());
    }
    Payload::try_from(Value::Object(map)).unwrap_or_else(|_| Payload::new())
}

/// Coerce a filter row's `value` into a typed Qdrant match: bare JSON
/// bool/integer keep their type; a string tries bool then integer before
/// falling back to a keyword/text match — the same coercion-friendliness as
/// the IF/Filter condition operators.
fn condition_match_value(raw: &Value) -> Result<MatchValue, String> {
    match raw {
        Value::Bool(b) => Ok(MatchValue::from(*b)),
        Value::Number(n) => n
            .as_i64()
            .map(MatchValue::from)
            .ok_or_else(|| "Vector Store filter: only integers are supported, not floats".into()),
        Value::String(s) => {
            let t = s.trim();
            if let Ok(b) = t.parse::<bool>() {
                Ok(MatchValue::from(b))
            } else if let Ok(n) = t.parse::<i64>() {
                Ok(MatchValue::from(n))
            } else {
                Ok(MatchValue::from(t.to_string()))
            }
        }
        other => Err(format!("Vector Store filter: unsupported value {other}")),
    }
}

/// Build an AND (`must`) filter from `{ field, value }` rows. Empty rows → no
/// filter (an unfiltered search/delete-by-id, never an error).
fn build_filter(conditions: &[Value]) -> Result<Option<Filter>, String> {
    if conditions.is_empty() {
        return Ok(None);
    }
    let mut must = Vec::with_capacity(conditions.len());
    for c in conditions {
        let field = c
            .get("field")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if field.is_empty() {
            return Err("Vector Store filter: each row needs a Field".into());
        }
        let value = c.get("value").cloned().unwrap_or(Value::Null);
        let match_value = condition_match_value(&value)
            .map_err(|e| format!("Vector Store filter on '{field}': {e}"))?;
        must.push(Condition::matches(field, match_value));
    }
    Ok(Some(Filter {
        must,
        ..Default::default()
    }))
}

/// Shape one search hit as `{ id, score, ...payload }` — payload fields spread
/// first so the structural `id`/`score` keys always win a name collision.
fn shape_point(id: String, score: Option<f32>, payload: HashMap<String, qdrant_client::qdrant::Value>) -> Value {
    let mut obj: Map<String, Value> = Payload::from(payload).into();
    obj.insert("id".to_string(), Value::String(id));
    if let Some(s) = score {
        obj.insert("score".to_string(), json!(s));
    }
    Value::Object(obj)
}

fn qdrant_client() -> Result<Qdrant, String> {
    let url = std::env::var("QDRANT_URL").map_err(|_| {
        "Vector Store: QDRANT_URL is not set — Qdrant isn't configured for this deployment"
            .to_string()
    })?;
    let mut builder = Qdrant::from_url(&url);
    if let Ok(api_key) = std::env::var("QDRANT_API_KEY") {
        if !api_key.is_empty() {
            builder = builder.api_key(api_key);
        }
    }
    builder
        .build()
        .map_err(|e| format!("Vector Store: connecting to Qdrant: {e}"))
}

fn embedder(state: &AppState) -> Result<Embedder, String> {
    Embedder::from_settings(&state.settings).ok_or_else(|| {
        "Vector Store: no embedder configured — set embedder.base_url / embedder.model in settings"
            .to_string()
    })
}

fn filter_rows(config: &Value) -> Vec<Value> {
    config
        .get("filter")
        .and_then(|v| v.get("parameters"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

async fn upsert(
    config: &Value,
    state: &AppState,
    qdrant: &Qdrant,
    collection: &str,
    input: &Value,
) -> Result<Value, String> {
    let text = config
        .get("text")
        .and_then(text_from_value)
        .or_else(|| text_from_value(input))
        .ok_or_else(|| {
            "Vector Store Upsert: no text found — set the Text field or feed a node whose \
             output is/contains text"
                .to_string()
        })?;

    let embedder = embedder(state)?;
    let vector = embedder
        .embed_one(&text)
        .await
        .map_err(|e| format!("Vector Store: embedding failed: {e}"))?;

    let metadata = metadata_object(config.get("metadata"))?;
    let id = resolve_point_id(config.get("id"));
    let id_str = point_id_to_string(Some(&id));
    let payload = build_payload(&text, &metadata);

    let point = PointStruct::new(id, vector, payload);
    qdrant
        .upsert_points(UpsertPointsBuilder::new(collection, vec![point]).wait(true))
        .await
        .map_err(|e| format!("Vector Store: upsert failed: {e}"))?;

    Ok(json!({ "operation": "upsert", "collection": collection, "id": id_str, "count": 1 }))
}

async fn search(
    config: &Value,
    state: &AppState,
    qdrant: &Qdrant,
    collection: &str,
    input: &Value,
) -> Result<Value, String> {
    let query = config
        .get("query")
        .and_then(text_from_value)
        .or_else(|| text_from_value(input))
        .ok_or_else(|| {
            "Vector Store Search: no query found — set the Query field or feed a node whose \
             output is/contains text"
                .to_string()
        })?;

    let embedder = embedder(state)?;
    let vector = embedder
        .embed_one(&query)
        .await
        .map_err(|e| format!("Vector Store: embedding failed: {e}"))?;

    let limit = config
        .get("limit")
        .and_then(|v| v.as_u64())
        .filter(|&n| n > 0)
        .unwrap_or(5);
    let score_threshold = config.get("scoreThreshold").and_then(|v| v.as_f64());

    let filter = build_filter(&filter_rows(config))?;

    let mut req = SearchPointsBuilder::new(collection, vector, limit).with_payload(true);
    if let Some(f) = filter {
        req = req.filter(f);
    }
    if let Some(t) = score_threshold {
        req = req.score_threshold(t as f32);
    }

    let resp = qdrant
        .search_points(req)
        .await
        .map_err(|e| format!("Vector Store: search failed: {e}"))?;

    let results: Vec<Value> = resp
        .result
        .into_iter()
        .map(|p| shape_point(point_id_to_string(p.id.as_ref()), Some(p.score), p.payload))
        .collect();

    Ok(Value::Array(results))
}

async fn delete(config: &Value, qdrant: &Qdrant, collection: &str) -> Result<Value, String> {
    let id = config
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let conditions = filter_rows(config);

    if let Some(id_str) = id {
        let pid = match id_str.parse::<u64>() {
            Ok(n) => PointId::from(n),
            Err(_) => PointId::from(id_str.to_string()),
        };
        qdrant
            .delete_points(
                DeletePointsBuilder::new(collection)
                    .points(PointsIdsList { ids: vec![pid] })
                    .wait(true),
            )
            .await
            .map_err(|e| format!("Vector Store: delete failed: {e}"))?;
        return Ok(json!({ "operation": "delete", "collection": collection, "id": id_str }));
    }

    if !conditions.is_empty() {
        let filter = build_filter(&conditions)?.unwrap_or_default();
        qdrant
            .delete_points(
                DeletePointsBuilder::new(collection)
                    .points(filter)
                    .wait(true),
            )
            .await
            .map_err(|e| format!("Vector Store: delete failed: {e}"))?;
        return Ok(json!({ "operation": "delete", "collection": collection, "filtered": true }));
    }

    Err("Vector Store Delete: set an Id or at least one Filter row".into())
}

pub(crate) async fn execute(config: &Value, state: &AppState, input: &Value) -> Result<Value, String> {
    let operation = config
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("upsert");
    let collection = config
        .get("collection")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if collection.is_empty() {
        return Err(
            "Vector Store: set a Collection name (e.g. \"documents\" or \"entities\" — see \
             qdrant/create-collections.sh)"
                .into(),
        );
    }

    let qdrant = qdrant_client()?;
    let exists = qdrant
        .collection_exists(&collection)
        .await
        .map_err(|e| format!("Vector Store: checking collection '{collection}': {e}"))?;
    if !exists {
        return Err(format!(
            "Vector Store: collection '{collection}' does not exist. Create it first (see \
             qdrant/create-collections.sh) — Neocortex does not auto-create collections."
        ));
    }

    match operation {
        "upsert" => upsert(config, state, &qdrant, &collection, input).await,
        "search" => search(config, state, &qdrant, &collection, input).await,
        "delete" => delete(config, &qdrant, &collection).await,
        other => Err(format!("Unknown Vector Store operation: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_from_value_prefers_plain_string() {
        assert_eq!(text_from_value(&json!("hello")), Some("hello".to_string()));
    }

    #[test]
    fn text_from_value_blank_string_is_none() {
        assert_eq!(text_from_value(&json!("   ")), None);
    }

    #[test]
    fn text_from_value_probes_object_fields_in_order() {
        assert_eq!(
            text_from_value(&json!({ "content": "c", "body": "b" })),
            Some("c".to_string())
        );
        assert_eq!(
            text_from_value(&json!({ "body": "b", "data": "d" })),
            Some("b".to_string())
        );
    }

    #[test]
    fn text_from_value_defers_array_to_first_element() {
        assert_eq!(
            text_from_value(&json!(["first", "second"])),
            Some("first".to_string())
        );
    }

    #[test]
    fn text_from_value_missing_is_none() {
        assert_eq!(text_from_value(&json!({ "other": 1 })), None);
        assert_eq!(text_from_value(&json!(42)), None);
        assert_eq!(text_from_value(&Value::Null), None);
    }

    #[test]
    fn resolve_point_id_numeric_string_becomes_num_variant() {
        let id = resolve_point_id(Some(&json!("42")));
        assert_eq!(id.point_id_options, Some(PointIdOptions::Num(42)));
    }

    #[test]
    fn resolve_point_id_number_becomes_num_variant() {
        let id = resolve_point_id(Some(&json!(7)));
        assert_eq!(id.point_id_options, Some(PointIdOptions::Num(7)));
    }

    #[test]
    fn resolve_point_id_non_numeric_string_becomes_uuid_variant() {
        let id = resolve_point_id(Some(&json!("my-key")));
        assert_eq!(
            id.point_id_options,
            Some(PointIdOptions::Uuid("my-key".to_string()))
        );
    }

    #[test]
    fn resolve_point_id_blank_auto_generates_a_uuid() {
        let a = resolve_point_id(None);
        let b = resolve_point_id(Some(&json!("")));
        assert_ne!(a.point_id_options, b.point_id_options);
        for id in [a, b] {
            match id.point_id_options {
                Some(PointIdOptions::Uuid(s)) => assert_eq!(s.len(), 36),
                other => panic!("expected a Uuid variant, got {other:?}"),
            }
        }
    }

    #[test]
    fn point_id_to_string_roundtrips_both_variants() {
        assert_eq!(point_id_to_string(Some(&PointId::from(42u64))), "42");
        assert_eq!(
            point_id_to_string(Some(&PointId::from("abc".to_string()))),
            "abc"
        );
        assert_eq!(point_id_to_string(None), "");
    }

    #[test]
    fn metadata_object_none_and_null_yield_empty() {
        assert_eq!(metadata_object(None).unwrap(), Map::new());
        assert_eq!(metadata_object(Some(&Value::Null)).unwrap(), Map::new());
    }

    #[test]
    fn metadata_object_accepts_a_real_object() {
        let m = metadata_object(Some(&json!({ "source": "kb", "n": 1 }))).unwrap();
        assert_eq!(m.get("source"), Some(&json!("kb")));
        assert_eq!(m.get("n"), Some(&json!(1)));
    }

    #[test]
    fn metadata_object_parses_a_json_object_string() {
        let m = metadata_object(Some(&json!("{\"source\":\"kb\"}"))).unwrap();
        assert_eq!(m.get("source"), Some(&json!("kb")));
    }

    #[test]
    fn metadata_object_blank_string_is_empty() {
        assert_eq!(metadata_object(Some(&json!("   "))).unwrap(), Map::new());
    }

    #[test]
    fn metadata_object_rejects_non_object_json() {
        assert!(metadata_object(Some(&json!("[1,2,3]"))).is_err());
        assert!(metadata_object(Some(&json!("not json"))).is_err());
        assert!(metadata_object(Some(&json!(42))).is_err());
    }

    #[test]
    fn build_payload_merges_text_and_metadata() {
        let mut meta = Map::new();
        meta.insert("source".to_string(), json!("kb"));
        let payload = build_payload("hello world", &meta);
        let obj: Map<String, Value> = payload.into();
        assert_eq!(obj.get("text"), Some(&json!("hello world")));
        assert_eq!(obj.get("source"), Some(&json!("kb")));
    }

    #[test]
    fn build_payload_metadata_wins_over_text_key_conflict() {
        let mut meta = Map::new();
        meta.insert("text".to_string(), json!("overridden"));
        let payload = build_payload("original", &meta);
        let obj: Map<String, Value> = payload.into();
        assert_eq!(obj.get("text"), Some(&json!("overridden")));
    }

    #[test]
    fn condition_match_value_coerces_boolean_strings() {
        assert_eq!(condition_match_value(&json!("true")).unwrap(), MatchValue::from(true));
        assert_eq!(condition_match_value(&json!(false)).unwrap(), MatchValue::from(false));
    }

    #[test]
    fn condition_match_value_coerces_integer_strings() {
        assert_eq!(condition_match_value(&json!("42")).unwrap(), MatchValue::from(42i64));
        assert_eq!(condition_match_value(&json!(7)).unwrap(), MatchValue::from(7i64));
    }

    #[test]
    fn condition_match_value_falls_back_to_keyword_string() {
        assert_eq!(
            condition_match_value(&json!("kb")).unwrap(),
            MatchValue::from("kb".to_string())
        );
    }

    #[test]
    fn condition_match_value_rejects_floats() {
        assert!(condition_match_value(&json!(1.5)).is_err());
    }

    #[test]
    fn build_filter_empty_conditions_yields_none() {
        assert!(build_filter(&[]).unwrap().is_none());
    }

    #[test]
    fn build_filter_requires_a_field_name() {
        let err = build_filter(&[json!({ "field": "", "value": "x" })]).unwrap_err();
        assert!(err.contains("Field"));
    }

    #[test]
    fn build_filter_builds_one_must_condition_per_row() {
        let f = build_filter(&[
            json!({ "field": "source_type", "value": "email" }),
            json!({ "field": "active", "value": true }),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(f.must.len(), 2);
    }

    #[test]
    fn shape_point_spreads_payload_and_adds_id_score() {
        let mut payload = HashMap::new();
        payload.insert(
            "text".to_string(),
            qdrant_client::qdrant::Value::from("hi".to_string()),
        );
        let out = shape_point("42".to_string(), Some(0.87), payload);
        assert_eq!(out["id"], json!("42"));
        assert_eq!(out["score"], json!(0.87));
        assert_eq!(out["text"], json!("hi"));
    }

    #[test]
    fn shape_point_structural_keys_win_over_payload_name_collision() {
        // A payload that happens to carry its own "id"/"score" fields must not
        // shadow the real point id/score.
        let mut payload = HashMap::new();
        payload.insert(
            "id".to_string(),
            qdrant_client::qdrant::Value::from("payload-id".to_string()),
        );
        let out = shape_point("real-id".to_string(), None, payload);
        assert_eq!(out["id"], json!("real-id"));
    }
}
