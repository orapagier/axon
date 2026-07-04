//! CRM dashboard API (Phase 5): thin wrappers over the in-process CRM tools.
//!
//! Every handler forwards to `state.mcp.call("axon-mcp", "crm_*", args)` so the
//! UI reuses the exact tool logic — validation, teaching errors, duplicate
//! guards — with zero duplicated SQL. The dashboard is the human operator, so
//! these routes are not subject to the agent-side CRM write gating
//! (`crm.agent_write_tools`); they sit behind `require_auth` like every other
//! `/api` route. Hard deletes are deliberately not exposed — the UI offers
//! archive/restore only.

use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

/// Server name the in-process MCP backend registers under (main.rs).
const MCP_SERVER: &str = "axon-mcp";

async fn call_crm(state: &AppState, tool: &str, args: Value) -> Json<Value> {
    match state.mcp.call(MCP_SERVER, tool, args).await {
        Ok(v) => Json(v),
        Err(e) => {
            tracing::error!("CRM API '{tool}' failed: {e}");
            Json(json!({ "error": true, "message": format!("{e}") }))
        }
    }
}

/// Plural URL segment → the singular entity name the CRM tools use.
fn singular(entity: &str) -> Option<&'static str> {
    match entity {
        "leads" => Some("lead"),
        "deals" => Some("deal"),
        "orgs" => Some("org"),
        "activities" => Some("activity"),
        _ => None,
    }
}

fn unknown_entity(entity: &str) -> Json<Value> {
    Json(json!({
        "error": true,
        "message": format!("unknown CRM entity '{entity}' (expected leads, deals, orgs, or activities)"),
    }))
}

/// Ensures a request body is a JSON object we can feed to a CRM tool,
/// optionally forcing the `id` from the URL path over anything in the body.
fn body_with_id(body: Value, id: Option<&str>) -> Value {
    let mut map = match body {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    if let Some(id) = id {
        map.insert("id".into(), Value::String(id.to_string()));
    }
    Value::Object(map)
}

#[derive(Deserialize)]
pub struct CrmListQuery {
    /// Free-text search; switches list → search for leads/deals/orgs.
    q: Option<String>,
    status: Option<String>,
    stage: Option<String>,
    industry: Option<String>,
    entity_id: Option<String>,
    entity_type: Option<String>,
    kind: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

pub async fn crm_list_records(
    State(state): State<AppState>,
    Path(entity): Path<String>,
    Query(p): Query<CrmListQuery>,
) -> Json<Value> {
    let Some(kind) = singular(&entity) else {
        return unknown_entity(&entity);
    };

    let mut args = serde_json::Map::new();
    if let Some(limit) = p.limit {
        args.insert("limit".into(), limit.into());
    }
    if let Some(offset) = p.offset {
        args.insert("offset".into(), offset.into());
    }

    // A non-empty `q` routes to the entity's search tool (activities have none).
    let query = p.q.as_deref().map(str::trim).filter(|q| !q.is_empty());
    let tool = match (kind, query) {
        ("lead", Some(q)) => {
            args.insert("query".into(), q.into());
            "crm_lead_search"
        }
        ("deal", Some(q)) => {
            args.insert("query".into(), q.into());
            "crm_deal_search"
        }
        ("org", Some(q)) => {
            args.insert("query".into(), q.into());
            "crm_org_search"
        }
        ("lead", None) => {
            if let Some(status) = p.status.filter(|s| !s.is_empty()) {
                args.insert("status".into(), status.into());
            }
            "crm_lead_list"
        }
        ("deal", None) => {
            if let Some(stage) = p.stage.filter(|s| !s.is_empty()) {
                args.insert("stage".into(), stage.into());
            }
            "crm_deal_list"
        }
        ("org", None) => {
            if let Some(industry) = p.industry.filter(|s| !s.is_empty()) {
                args.insert("industry".into(), industry.into());
            }
            "crm_org_list"
        }
        ("activity", _) => {
            if let Some(entity_id) = p.entity_id.filter(|s| !s.is_empty()) {
                args.insert("entity_id".into(), entity_id.into());
            }
            if let Some(entity_type) = p.entity_type.filter(|s| !s.is_empty()) {
                args.insert("entity_type".into(), entity_type.into());
            }
            if let Some(activity_kind) = p.kind.filter(|s| !s.is_empty()) {
                args.insert("kind".into(), activity_kind.into());
            }
            "crm_activity_list"
        }
        _ => unreachable!("singular() only returns the four entities"),
    };

    call_crm(&state, tool, Value::Object(args)).await
}

pub async fn crm_create_record(
    State(state): State<AppState>,
    Path(entity): Path<String>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let tool = match entity.as_str() {
        "leads" => "crm_lead_create",
        "deals" => "crm_deal_create",
        "orgs" => "crm_org_create",
        "activities" => "crm_activity_log",
        _ => return unknown_entity(&entity),
    };
    call_crm(&state, tool, body_with_id(body, None)).await
}

pub async fn crm_get_record(
    State(state): State<AppState>,
    Path((entity, id)): Path<(String, String)>,
) -> Json<Value> {
    let tool = match entity.as_str() {
        "leads" => "crm_lead_get",
        "deals" => "crm_deal_get",
        "orgs" => "crm_org_get",
        "activities" => "crm_activity_get",
        _ => return unknown_entity(&entity),
    };
    call_crm(&state, tool, json!({ "id": id })).await
}

pub async fn crm_update_record(
    State(state): State<AppState>,
    Path((entity, id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let tool = match entity.as_str() {
        "leads" => "crm_lead_update",
        "deals" => "crm_deal_update",
        "orgs" => "crm_org_update",
        "activities" => "crm_activity_update",
        _ => return unknown_entity(&entity),
    };
    call_crm(&state, tool, body_with_id(body, Some(&id))).await
}

pub async fn crm_archive_record(
    State(state): State<AppState>,
    Path((entity, id)): Path<(String, String)>,
) -> Json<Value> {
    let Some(kind) = singular(&entity) else {
        return unknown_entity(&entity);
    };
    call_crm(
        &state,
        "crm_record_archive",
        json!({ "entity_type": kind, "id": id }),
    )
    .await
}

pub async fn crm_restore_record(
    State(state): State<AppState>,
    Path((entity, id)): Path<(String, String)>,
) -> Json<Value> {
    let Some(kind) = singular(&entity) else {
        return unknown_entity(&entity);
    };
    call_crm(
        &state,
        "crm_record_restore",
        json!({ "entity_type": kind, "id": id }),
    )
    .await
}

pub async fn crm_get_overview(
    State(state): State<AppState>,
    Path((entity, id)): Path<(String, String)>,
) -> Json<Value> {
    let Some(kind) = singular(&entity) else {
        return unknown_entity(&entity);
    };
    call_crm(
        &state,
        "crm_record_overview",
        json!({ "entity_type": kind, "id": id }),
    )
    .await
}

pub async fn crm_get_pipeline(State(state): State<AppState>) -> Json<Value> {
    call_crm(&state, "crm_pipeline_summary", json!({})).await
}

pub async fn crm_get_dashboard(State(state): State<AppState>) -> Json<Value> {
    call_crm(&state, "crm_dashboard_summary", json!({})).await
}

#[derive(Deserialize)]
pub struct CrmArchivedQuery {
    entity_type: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

pub async fn crm_get_archived(
    State(state): State<AppState>,
    Query(p): Query<CrmArchivedQuery>,
) -> Json<Value> {
    let mut args = serde_json::Map::new();
    if let Some(entity_type) = p.entity_type.filter(|s| !s.is_empty()) {
        args.insert("entity_type".into(), entity_type.into());
    }
    if let Some(limit) = p.limit {
        args.insert("limit".into(), limit.into());
    }
    if let Some(offset) = p.offset {
        args.insert("offset".into(), offset.into());
    }
    call_crm(&state, "crm_archived_list", Value::Object(args)).await
}

#[derive(Deserialize)]
pub struct CrmSearchQuery {
    q: String,
    limit_per_type: Option<i64>,
}

pub async fn crm_search_all_records(
    State(state): State<AppState>,
    Query(p): Query<CrmSearchQuery>,
) -> Json<Value> {
    let mut args = serde_json::Map::new();
    args.insert("query".into(), p.q.into());
    if let Some(limit) = p.limit_per_type {
        args.insert("limit_per_type".into(), limit.into());
    }
    call_crm(&state, "crm_search_all", Value::Object(args)).await
}
