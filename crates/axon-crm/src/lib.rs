pub mod activities;
pub mod db;
pub mod deals;
pub mod leads;
pub mod orgs;
pub mod records;
pub mod utils;
pub mod views;

use anyhow::Result;
use axon_core::{data_dir, err_json, ok_json, schema, AppState};
use rmcp::model::{CallToolResult, Tool};
use serde_json::{Map, Value};
use sqlx::SqlitePool;
use std::sync::Arc;

use utils::require_str;

pub struct CrmService {
    _state: Arc<AppState>,
    pool: SqlitePool,
}

impl CrmService {
    pub async fn new(state: Arc<AppState>) -> Result<Self> {
        let pool = db::open(&data_dir()).await?;
        Ok(Self {
            _state: state,
            pool,
        })
    }

    pub fn tool_list() -> Vec<Tool> {
        vec![
            // ── Leads (6) ────────────────────────────────────────────────
            Tool {
                name: "crm_lead_create".into(),
                description: "Create a new CRM lead. Status options: Open, Contacted, Qualified, Lost.".into(),
                input_schema: schema!({
                    "name":    { "type": "string" },
                    "email":   { "type": "string" },
                    "phone":   { "type": "string" },
                    "company": { "type": "string" },
                    "org_id":  { "type": "string", "description": "Link to an existing Organization ID" },
                    "status":  { "type": "string", "enum": ["Open", "Contacted", "Qualified", "Lost"], "default": "Open" },
                    "source":  { "type": "string", "description": "Lead source, e.g. Website, Referral, Cold Outreach" },
                    "tags":    { "type": "array", "items": { "type": "string" } },
                    "notes":   { "type": "string" }
                }, ["name"]),
            },
            Tool {
                name: "crm_lead_list".into(),
                description: "List CRM leads, optionally filtered by status. Supports pagination.".into(),
                input_schema: schema!({
                    "status": { "type": "string", "enum": ["Open", "Contacted", "Qualified", "Lost", "All"], "default": "All" },
                    "limit":  { "type": "integer", "default": 50, "maximum": 200 },
                    "offset": { "type": "integer", "default": 0 }
                }, []),
            },
            Tool {
                name: "crm_lead_get".into(),
                description: "Get the full details of a CRM lead by ID.".into(),
                input_schema: schema!({ "id": { "type": "string" } }, ["id"]),
            },
            Tool {
                name: "crm_lead_update".into(),
                description: "Update any field(s) of an existing CRM lead.".into(),
                input_schema: schema!({
                    "id":      { "type": "string" },
                    "name":    { "type": "string" },
                    "email":   { "type": "string" },
                    "phone":   { "type": "string" },
                    "company": { "type": "string" },
                    "org_id":  { "type": "string" },
                    "status":  { "type": "string", "enum": ["Open", "Contacted", "Qualified", "Lost"] },
                    "source":  { "type": "string" },
                    "tags":    { "type": "array", "items": { "type": "string" } },
                    "notes":   { "type": "string" }
                }, ["id"]),
            },
            Tool {
                name: "crm_lead_delete".into(),
                description: "Permanently delete a CRM lead by ID. Prefer archive for safer removal.".into(),
                input_schema: schema!({
                    "id": { "type": "string" },
                    "confirm_permanent": { "type": "boolean", "default": false }
                }, ["id"]),
            },
            Tool {
                name: "crm_lead_search".into(),
                description: "Full-text search across lead names, emails, companies, notes, and tags.".into(),
                input_schema: schema!({
                    "query":  { "type": "string" },
                    "limit":  { "type": "integer", "default": 50 },
                    "offset": { "type": "integer", "default": 0 }
                }, ["query"]),
            },
            Tool {
                name: "crm_lead_convert_to_deal".into(),
                description: "Convert a lead into a deal/opportunity and optionally update the lead status.".into(),
                input_schema: schema!({
                    "lead_id":         { "type": "string" },
                    "title":           { "type": "string", "description": "Optional deal title. Defaults from the lead/company." },
                    "amount":          { "type": "number", "minimum": 0 },
                    "currency":        { "type": "string", "default": "USD" },
                    "stage":           { "type": "string", "enum": ["Prospecting", "Qualified", "Proposal", "Negotiation", "Won", "Lost"], "default": "Prospecting" },
                    "probability":     { "type": "integer", "minimum": 0, "maximum": 100 },
                    "org_id":          { "type": "string", "description": "Optional org override. Defaults to the lead's org_id." },
                    "expected_close":  { "type": "string", "description": "Expected close date (ISO 8601)" },
                    "tags":            { "type": "array", "items": { "type": "string" } },
                    "notes":           { "type": "string" },
                    "lead_status":     { "type": "string", "enum": ["Open", "Contacted", "Qualified", "Lost"], "default": "Qualified" }
                }, ["lead_id"]),
            },

            // ── Deals (7) ────────────────────────────────────────────────
            Tool {
                name: "crm_deal_create".into(),
                description: "Create a new sales deal linked to a lead or contact.".into(),
                input_schema: schema!({
                    "title":          { "type": "string" },
                    "amount":         { "type": "number", "minimum": 0 },
                    "currency":       { "type": "string", "default": "USD" },
                    "stage":          { "type": "string", "enum": ["Prospecting", "Qualified", "Proposal", "Negotiation", "Won", "Lost"], "default": "Prospecting" },
                    "probability":    { "type": "integer", "minimum": 0, "maximum": 100, "description": "Win probability as percentage" },
                    "contact_id":     { "type": "string", "description": "ID of the associated lead or contact" },
                    "org_id":         { "type": "string", "description": "ID of the associated organization" },
                    "expected_close": { "type": "string", "description": "Expected close date (ISO 8601)" },
                    "tags":           { "type": "array", "items": { "type": "string" } },
                    "notes":          { "type": "string" }
                }, ["title", "contact_id"]),
            },
            Tool {
                name: "crm_deal_list".into(),
                description: "List sales deals, optionally filtered by stage. Returns total pipeline value.".into(),
                input_schema: schema!({
                    "stage":  { "type": "string", "enum": ["Prospecting", "Qualified", "Proposal", "Negotiation", "Won", "Lost", "All"], "default": "All" },
                    "limit":  { "type": "integer", "default": 50 },
                    "offset": { "type": "integer", "default": 0 }
                }, []),
            },
            Tool {
                name: "crm_deal_get".into(),
                description: "Get the full details of a deal by ID.".into(),
                input_schema: schema!({ "id": { "type": "string" } }, ["id"]),
            },
            Tool {
                name: "crm_deal_update".into(),
                description: "Update any field(s) of an existing deal (e.g. advance stage, change amount).".into(),
                input_schema: schema!({
                    "id":             { "type": "string" },
                    "title":          { "type": "string" },
                    "amount":         { "type": "number", "minimum": 0 },
                    "currency":       { "type": "string" },
                    "stage":          { "type": "string", "enum": ["Prospecting", "Qualified", "Proposal", "Negotiation", "Won", "Lost"] },
                    "probability":    { "type": "integer", "minimum": 0, "maximum": 100 },
                    "contact_id":     { "type": "string" },
                    "org_id":         { "type": "string" },
                    "expected_close": { "type": "string" },
                    "tags":           { "type": "array", "items": { "type": "string" } },
                    "notes":          { "type": "string" }
                }, ["id"]),
            },
            Tool {
                name: "crm_deal_delete".into(),
                description: "Permanently delete a deal by ID. Prefer archive for safer removal.".into(),
                input_schema: schema!({
                    "id": { "type": "string" },
                    "confirm_permanent": { "type": "boolean", "default": false }
                }, ["id"]),
            },
            Tool {
                name: "crm_deal_search".into(),
                description: "Search deals by title, notes, or tags.".into(),
                input_schema: schema!({
                    "query":  { "type": "string" },
                    "limit":  { "type": "integer", "default": 50 },
                    "offset": { "type": "integer", "default": 0 }
                }, ["query"]),
            },
            Tool {
                name: "crm_pipeline_summary".into(),
                description: "Get a full pipeline overview: deal counts and values grouped by stage, win rate.".into(),
                input_schema: schema!({}, []),
            },

            // ── Organizations (6) ────────────────────────────────────────
            Tool {
                name: "crm_org_create".into(),
                description: "Create a new organization/company in the CRM.".into(),
                input_schema: schema!({
                    "name":     { "type": "string" },
                    "website":  { "type": "string" },
                    "industry": { "type": "string" },
                    "size":     { "type": "string", "description": "Company size, e.g. 1-10, 11-50, 51-200, 201-1000, 1000+" },
                    "country":  { "type": "string" },
                    "phone":    { "type": "string" },
                    "email":    { "type": "string" },
                    "tags":     { "type": "array", "items": { "type": "string" } },
                    "notes":    { "type": "string" }
                }, ["name"]),
            },
            Tool {
                name: "crm_org_list".into(),
                description: "List all organizations, optionally filtered by industry.".into(),
                input_schema: schema!({
                    "industry": { "type": "string" },
                    "limit":    { "type": "integer", "default": 50 },
                    "offset":   { "type": "integer", "default": 0 }
                }, []),
            },
            Tool {
                name: "crm_org_get".into(),
                description: "Get the full details of an organization by ID.".into(),
                input_schema: schema!({ "id": { "type": "string" } }, ["id"]),
            },
            Tool {
                name: "crm_org_update".into(),
                description: "Update any field(s) of an existing organization.".into(),
                input_schema: schema!({
                    "id":       { "type": "string" },
                    "name":     { "type": "string" },
                    "website":  { "type": "string" },
                    "industry": { "type": "string" },
                    "size":     { "type": "string" },
                    "country":  { "type": "string" },
                    "phone":    { "type": "string" },
                    "email":    { "type": "string" },
                    "tags":     { "type": "array", "items": { "type": "string" } },
                    "notes":    { "type": "string" }
                }, ["id"]),
            },
            Tool {
                name: "crm_org_delete".into(),
                description: "Permanently delete an organization by ID. Prefer archive for safer removal.".into(),
                input_schema: schema!({
                    "id": { "type": "string" },
                    "confirm_permanent": { "type": "boolean", "default": false }
                }, ["id"]),
            },
            Tool {
                name: "crm_org_search".into(),
                description: "Search organizations by name, industry, country, website, notes, or tags.".into(),
                input_schema: schema!({
                    "query":  { "type": "string" },
                    "limit":  { "type": "integer", "default": 50 },
                    "offset": { "type": "integer", "default": 0 }
                }, ["query"]),
            },

            // ── Activities (4) ───────────────────────────────────────────
            Tool {
                name: "crm_activity_log".into(),
                description: "Log an activity (note, call, email, meeting, task) on a lead, deal, or org.".into(),
                input_schema: schema!({
                    "entity_id":   { "type": "string", "description": "ID of the lead, deal, or org" },
                    "entity_type": { "type": "string", "enum": ["lead", "deal", "org"] },
                    "kind":        { "type": "string", "enum": ["note", "call", "email", "meeting", "task", "other"], "default": "note" },
                    "title":       { "type": "string", "description": "Short summary of the activity" },
                    "body":        { "type": "string", "description": "Full details or transcript" },
                    "occurred_at": { "type": "string", "description": "ISO 8601 timestamp (defaults to now)" }
                }, ["entity_id", "entity_type", "title"]),
            },
            Tool {
                name: "crm_activity_list".into(),
                description: "List activities for a given entity, or all activities. Sorted most-recent first.".into(),
                input_schema: schema!({
                    "entity_id": { "type": "string", "description": "Filter by entity ID (optional)" },
                    "entity_type": { "type": "string", "enum": ["lead", "deal", "org"], "description": "Optional entity type filter" },
                    "kind":      { "type": "string", "enum": ["note", "call", "email", "meeting", "task", "other"] },
                    "limit":     { "type": "integer", "default": 50 },
                    "offset":    { "type": "integer", "default": 0 }
                }, []),
            },
            Tool {
                name: "crm_activity_get".into(),
                description: "Get the full details of an activity by ID.".into(),
                input_schema: schema!({ "id": { "type": "string" } }, ["id"]),
            },
            Tool {
                name: "crm_activity_update".into(),
                description: "Update an existing activity log entry, including reassignment to another CRM record.".into(),
                input_schema: schema!({
                    "id":          { "type": "string" },
                    "entity_id":   { "type": "string", "description": "If provided, must be paired with entity_type" },
                    "entity_type": { "type": "string", "enum": ["lead", "deal", "org"] },
                    "kind":        { "type": "string", "enum": ["note", "call", "email", "meeting", "task", "other"] },
                    "title":       { "type": "string" },
                    "body":        { "type": "string" },
                    "occurred_at": { "type": "string", "description": "ISO 8601 timestamp" }
                }, ["id"]),
            },
            Tool {
                name: "crm_activity_delete".into(),
                description: "Permanently delete an activity log entry by ID. Prefer archive for safer removal.".into(),
                input_schema: schema!({
                    "id": { "type": "string" },
                    "confirm_permanent": { "type": "boolean", "default": false }
                }, ["id"]),
            },

            // Insights / workflows
            Tool {
                name: "crm_record_archive".into(),
                description: "Archive a CRM record (soft delete) so it no longer appears in normal queries.".into(),
                input_schema: schema!({
                    "entity_type": { "type": "string", "enum": ["org", "lead", "deal", "activity"] },
                    "id": { "type": "string" }
                }, ["entity_type", "id"]),
            },
            Tool {
                name: "crm_record_restore".into(),
                description: "Restore a previously archived CRM record.".into(),
                input_schema: schema!({
                    "entity_type": { "type": "string", "enum": ["org", "lead", "deal", "activity"] },
                    "id": { "type": "string" }
                }, ["entity_type", "id"]),
            },
            Tool {
                name: "crm_archived_list".into(),
                description: "List archived CRM records across entities, or filter by one entity type.".into(),
                input_schema: schema!({
                    "entity_type": { "type": "string", "enum": ["org", "lead", "deal", "activity"] },
                    "limit": { "type": "integer", "default": 50, "maximum": 200 },
                    "offset": { "type": "integer", "default": 0 }
                }, []),
            },
            Tool {
                name: "crm_search_all".into(),
                description: "Search across organizations, leads, and deals in one call.".into(),
                input_schema: schema!({
                    "query":          { "type": "string" },
                    "limit_per_type": { "type": "integer", "default": 10, "maximum": 50 }
                }, ["query"]),
            },
            Tool {
                name: "crm_record_overview".into(),
                description: "Get a 360-degree view of a CRM record with related entities and recent activity.".into(),
                input_schema: schema!({
                    "entity_type":   { "type": "string", "enum": ["lead", "deal", "org"] },
                    "id":            { "type": "string" },
                    "related_limit": { "type": "integer", "default": 10, "maximum": 50 },
                    "activity_limit":{ "type": "integer", "default": 20, "maximum": 100 }
                }, ["entity_type", "id"]),
            },
            Tool {
                name: "crm_dashboard_summary".into(),
                description: "Get an operational CRM dashboard: lead status mix, pipeline health, stale deals, and closing-soon deals.".into(),
                input_schema: schema!({
                    "stale_days":          { "type": "integer", "default": 30 },
                    "closing_within_days": { "type": "integer", "default": 30 },
                    "activity_window_days":{ "type": "integer", "default": 30 },
                    "list_limit":          { "type": "integer", "default": 10, "maximum": 50 }
                }, []),
            },
            Tool {
                name: "crm_export_snapshot".into(),
                description: "Export a full CRM snapshot for backup, migration, or audit.".into(),
                input_schema: schema!({
                    "include_archived": { "type": "boolean", "default": true }
                }, []),
            },
        ]
    }

    pub async fn call(&self, name: &str, args: Map<String, Value>) -> Result<CallToolResult> {
        let a = &args;
        let pool = &self.pool;

        let result: Result<Value> = match name {
            // Leads
            "crm_lead_create" => leads::create(pool, a).await,
            "crm_lead_list" => leads::list(pool, a).await,
            "crm_lead_get" => leads::get(pool, require_str(a, "id")?).await,
            "crm_lead_update" => leads::update(pool, a).await,
            "crm_lead_delete" => {
                records::require_confirmed_delete(a).await?;
                leads::delete(pool, require_str(a, "id")?).await
            }
            "crm_lead_search" => leads::search(pool, a).await,
            "crm_lead_convert_to_deal" => leads::convert_to_deal(pool, a).await,

            // Deals
            "crm_deal_create" => deals::create(pool, a).await,
            "crm_deal_list" => deals::list(pool, a).await,
            "crm_deal_get" => deals::get(pool, require_str(a, "id")?).await,
            "crm_deal_update" => deals::update(pool, a).await,
            "crm_deal_delete" => {
                records::require_confirmed_delete(a).await?;
                deals::delete(pool, require_str(a, "id")?).await
            }
            "crm_deal_search" => deals::search(pool, a).await,
            "crm_pipeline_summary" => deals::pipeline_summary(pool).await,

            // Organizations
            "crm_org_create" => orgs::create(pool, a).await,
            "crm_org_list" => orgs::list(pool, a).await,
            "crm_org_get" => orgs::get(pool, require_str(a, "id")?).await,
            "crm_org_update" => orgs::update(pool, a).await,
            "crm_org_delete" => {
                records::require_confirmed_delete(a).await?;
                orgs::delete(pool, require_str(a, "id")?).await
            }
            "crm_org_search" => orgs::search(pool, a).await,

            // Activities
            "crm_activity_log" => activities::log(pool, a).await,
            "crm_activity_list" => activities::list(pool, a).await,
            "crm_activity_get" => activities::get(pool, require_str(a, "id")?).await,
            "crm_activity_update" => activities::update(pool, a).await,
            "crm_activity_delete" => {
                records::require_confirmed_delete(a).await?;
                activities::delete(pool, require_str(a, "id")?).await
            }

            // Insights / workflows
            "crm_record_archive" => records::archive(pool, a).await,
            "crm_record_restore" => records::restore(pool, a).await,
            "crm_archived_list" => records::archived_list(pool, a).await,
            "crm_search_all" => views::search_all(pool, a).await,
            "crm_record_overview" => views::record_overview(pool, a).await,
            "crm_dashboard_summary" => views::dashboard_summary(pool, a).await,
            "crm_export_snapshot" => records::export_snapshot(pool, a).await,

            other => Err(anyhow::anyhow!("Unknown CRM tool: {other}")),
        };

        Ok(match result {
            Ok(v) => ok_json(v),
            Err(e) => err_json(e),
        })
    }
}

#[cfg(test)]
mod tests;
