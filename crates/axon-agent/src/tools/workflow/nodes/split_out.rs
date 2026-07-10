//! Split Out — Task 1.4. Explodes a list field into individual items: the inverse
//! of Aggregate. Turns one `{ order: 7, items: [a, b, c] }` into three items so each
//! element can fan out into Loop / Cortex / a per-item branch.
//!
//! Operates over the primary input as a list (the array convention): for EACH
//! source item it reads the array at `fieldToSplitOut`, emits one output per
//! element, and — per `include` — optionally carries the source item's other fields
//! onto each element (so a split line-item keeps its order's context). All per-source
//! results concatenate into one output array.
//!
//! Element shaping: an object element becomes the item directly (unless
//! `destinationFieldName` is set); a scalar element — or any element when a
//! destination name is set — is wrapped as `{ <dest>: element }`, where `dest`
//! defaults to the split field's last path segment. Carried "other fields" are the
//! base and the exploded element merges over them, so the split data stays
//! authoritative on key conflicts.

use crate::tools::workflow::parse_path_pointer;
use serde_json::{Map, Value};

/// Turn the primary input into a source-item list — same convention as Filter /
/// Aggregate (array = items, Null = none, else a single item; `array_path` unwraps
/// a `{ results: [...] }` wrapper).
fn to_items(input: &Value, array_path: Option<&str>) -> Vec<Value> {
    if let Some(path) = array_path.map(str::trim).filter(|p| !p.is_empty()) {
        return match input.pointer(&parse_path_pointer(path)) {
            Some(Value::Array(a)) => a.clone(),
            Some(Value::Null) | None => Vec::new(),
            Some(other) => vec![other.clone()],
        };
    }
    match input {
        Value::Array(a) => a.clone(),
        Value::Null => Vec::new(),
        other => vec![other.clone()],
    }
}

/// Read a dot/bracket path out of `item` (missing → Null).
fn get_path(item: &Value, path: &str) -> Value {
    item.pointer(&parse_path_pointer(path.trim()))
        .cloned()
        .unwrap_or(Value::Null)
}

/// Last path segment: `order.lines` → `lines`, used to name the destination field
/// and the keys of selected carried fields.
fn last_segment(path: &str) -> &str {
    path.rsplit(|c| c == '.' || c == '[' || c == ']')
        .find(|s| !s.is_empty())
        .unwrap_or(path)
}

/// First path segment: `order.lines` → `order`, the top-level key excluded from
/// "all other fields" so the split source array isn't carried onto its own pieces.
fn first_segment(path: &str) -> &str {
    path.split(|c| c == '.' || c == '[' || c == ']')
        .find(|s| !s.is_empty())
        .unwrap_or(path)
}

/// The source item's fields to carry onto each exploded element, per `include`.
fn other_fields(
    source: &Value,
    include: &str,
    selected: &[String],
    split_first_seg: &str,
) -> Map<String, Value> {
    let mut m = Map::new();
    let obj = match source.as_object() {
        Some(o) => o,
        None => return m,
    };
    match include {
        // Every top-level field except the one being split out of.
        "allOtherFields" => {
            for (k, v) in obj {
                if k != split_first_seg {
                    m.insert(k.clone(), v.clone());
                }
            }
        }
        // Only the named fields (paths ok; each lands under its last segment).
        "selectedOtherFields" => {
            for path in selected {
                let v = get_path(source, path);
                if !v.is_null() {
                    m.insert(last_segment(path).to_string(), v);
                }
            }
        }
        // "noOtherFields" (default): carry nothing.
        _ => {}
    }
    m
}

pub(crate) fn execute(config: &Value, input: &Value) -> Result<Value, String> {
    let field = config
        .get("fieldToSplitOut")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if field.is_empty() {
        return Err(
            "Split Out needs a 'Field to Split Out' — the list field to explode into items"
                .to_string(),
        );
    }

    let include = config
        .get("include")
        .and_then(|v| v.as_str())
        .unwrap_or("noOtherFields");
    let selected: Vec<String> = config
        .get("fieldsToInclude")
        .and_then(|v| v.as_str())
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default();
    // Destination key for scalar elements (or any element, when set explicitly).
    let dest = config
        .get("destinationFieldName")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let array_path = config.get("arrayPath").and_then(|v| v.as_str());
    // Stamp each exploded item with `__idx` (its output position) so a later
    // `$ancestor()` can join back to this node even after Filter/Sort
    // reshaping. Off by default: the key is visible data downstream.
    let stamp_index = config
        .get("stampIndex")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let sources = to_items(input, array_path);
    let split_first = first_segment(&field).to_string();
    let dest_key = dest
        .map(str::to_string)
        .unwrap_or_else(|| last_segment(&field).to_string());

    let mut out: Vec<Value> = Vec::new();
    for source in &sources {
        let elements: Vec<Value> = match get_path(source, &field) {
            Value::Array(a) => a,
            // Nothing to split for this source — carries no items forward.
            Value::Null => continue,
            // A non-array, non-null field is treated as a single element.
            other => vec![other],
        };
        let base = other_fields(source, include, &selected, &split_first);
        for el in elements {
            let mut item = base.clone();
            match (&el, dest) {
                // Object element with no explicit destination → merge its fields
                // over the carried base (element wins on conflict).
                (Value::Object(eo), None) => {
                    for (k, v) in eo {
                        item.insert(k.clone(), v.clone());
                    }
                }
                // Scalar element, or an explicit destination name → wrap under it.
                _ => {
                    item.insert(dest_key.clone(), el);
                }
            }
            if stamp_index {
                item.entry("__idx".to_string())
                    .or_insert_with(|| Value::from(out.len()));
            }
            out.push(Value::Object(item));
        }
    }

    Ok(Value::Array(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg(field: &str, extra: Value) -> Value {
        let mut c = json!({ "fieldToSplitOut": field });
        if let (Some(obj), Some(ex)) = (c.as_object_mut(), extra.as_object()) {
            for (k, v) in ex {
                obj.insert(k.clone(), v.clone());
            }
        }
        c
    }

    // Explode a field of objects; with no other fields each element becomes an item.
    #[test]
    fn splits_object_array_into_items() {
        let input = json!({ "order": 7, "items": [{ "sku": "A" }, { "sku": "B" }] });
        let out = execute(&cfg("items", json!({})), &input).unwrap();
        assert_eq!(out, json!([{ "sku": "A" }, { "sku": "B" }]));
    }

    // All Other Fields carries the source's siblings onto each element, excluding
    // the split field itself.
    #[test]
    fn all_other_fields_carries_siblings() {
        let input = json!({ "order": 7, "items": [{ "sku": "A" }, { "sku": "B" }] });
        let out = execute(
            &cfg("items", json!({ "include": "allOtherFields" })),
            &input,
        )
        .unwrap();
        assert_eq!(
            out,
            json!([{ "sku": "A", "order": 7 }, { "sku": "B", "order": 7 }])
        );
    }

    // A scalar array wraps each element under the field's last segment by default.
    #[test]
    fn scalar_array_wraps_under_field_name() {
        let input = json!({ "tags": ["x", "y", "z"] });
        let out = execute(&cfg("tags", json!({})), &input).unwrap();
        assert_eq!(
            out,
            json!([{ "tags": "x" }, { "tags": "y" }, { "tags": "z" }])
        );
    }

    // An explicit destination name overrides the default wrap key.
    #[test]
    fn destination_field_name_overrides_wrap_key() {
        let input = json!({ "tags": ["x", "y"] });
        let out = execute(
            &cfg("tags", json!({ "destinationFieldName": "label" })),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!([{ "label": "x" }, { "label": "y" }]));
    }

    // Selected Other Fields carries only the named fields.
    #[test]
    fn selected_other_fields_carries_only_named() {
        let input = json!({ "id": 1, "region": "PH", "items": [{ "sku": "A" }], "secret": "hide" });
        let out = execute(
            &cfg(
                "items",
                json!({ "include": "selectedOtherFields", "fieldsToInclude": "id, region" }),
            ),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!([{ "sku": "A", "id": 1, "region": "PH" }]));
    }

    // Multiple source items each split and concatenate into one stream.
    #[test]
    fn multiple_sources_concatenate() {
        let input = json!([
            { "g": 1, "items": [{ "n": "a" }, { "n": "b" }] },
            { "g": 2, "items": [{ "n": "c" }] },
        ]);
        let out = execute(
            &cfg("items", json!({ "include": "allOtherFields" })),
            &input,
        )
        .unwrap();
        assert_eq!(
            out,
            json!([
                { "n": "a", "g": 1 },
                { "n": "b", "g": 1 },
                { "n": "c", "g": 2 },
            ])
        );
    }

    // A source whose split field is missing contributes no items (nothing to split).
    #[test]
    fn missing_field_contributes_nothing() {
        let input = json!([{ "items": [{ "n": 1 }] }, { "no_items": true }]);
        let out = execute(&cfg("items", json!({})), &input).unwrap();
        assert_eq!(out, json!([{ "n": 1 }]));
    }

    // A non-array field value is split as a single element.
    #[test]
    fn non_array_field_is_single_element() {
        let input = json!({ "items": { "sku": "solo" } });
        let out = execute(&cfg("items", json!({})), &input).unwrap();
        assert_eq!(out, json!([{ "sku": "solo" }]));
    }

    // On a key conflict the exploded element wins over the carried source field.
    #[test]
    fn element_wins_over_carried_field_on_conflict() {
        let input = json!({ "status": "order-level", "items": [{ "id": 1, "status": "item" }] });
        let out = execute(
            &cfg("items", json!({ "include": "allOtherFields" })),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!([{ "id": 1, "status": "item" }]));
    }

    // Nested split path: excludes the top-level parent from All Other Fields.
    #[test]
    fn nested_path_excludes_top_level_parent() {
        let input = json!({ "order": { "lines": [{ "sku": "A" }] }, "customer": "bob" });
        let out = execute(
            &cfg("order.lines", json!({ "include": "allOtherFields" })),
            &input,
        )
        .unwrap();
        assert_eq!(out, json!([{ "sku": "A", "customer": "bob" }]));
    }

    // A blank field is a clear config error.
    #[test]
    fn blank_field_errors() {
        let err = execute(&cfg("", json!({})), &json!({ "items": [] })).unwrap_err();
        assert!(err.contains("Field to Split Out"), "got: {err}");
    }

    // stampIndex marks each exploded item with its output position (__idx) so
    // $ancestor() can join back after downstream reshaping; an existing __idx
    // on the element is preserved (lineage from an earlier stamp wins).
    #[test]
    fn stamp_index_marks_items() {
        let input = json!({ "items": [{ "sku": "A" }, { "sku": "B" }] });
        let out = execute(&cfg("items", json!({ "stampIndex": true })), &input).unwrap();
        assert_eq!(
            out,
            json!([{ "sku": "A", "__idx": 0 }, { "sku": "B", "__idx": 1 }])
        );
    }

    // arrayPath unwraps a wrapper before iterating source items.
    #[test]
    fn array_path_unwraps_wrapper() {
        let input = json!({ "results": [{ "items": [{ "n": 1 }, { "n": 2 }] }] });
        let out = execute(&cfg("items", json!({ "arrayPath": "results" })), &input).unwrap();
        assert_eq!(out, json!([{ "n": 1 }, { "n": 2 }]));
    }
}
