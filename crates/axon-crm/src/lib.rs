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

/// Operator-configurable default currency for new deals (`crm.default_currency`
/// setting). The host registers a live provider at startup (main.rs) so setting
/// changes apply without a restart; unset (tests, standalone use) falls back to
/// USD. Same global-registration pattern as workflow trigger_data.
static DEFAULT_CURRENCY_PROVIDER: std::sync::OnceLock<Box<dyn Fn() -> String + Send + Sync>> =
    std::sync::OnceLock::new();

pub fn set_default_currency_provider(provider: impl Fn() -> String + Send + Sync + 'static) {
    let _ = DEFAULT_CURRENCY_PROVIDER.set(Box::new(provider));
}

pub(crate) fn default_currency() -> String {
    let value = DEFAULT_CURRENCY_PROVIDER
        .get()
        .map(|f| f().trim().to_ascii_uppercase())
        .unwrap_or_default();
    if utils::validate_currency("crm.default_currency", &value).is_ok() {
        value
    } else {
        "USD".to_owned()
    }
}

/// Set by `CrmService::new` so the host's scheduled-backup task (`axon::maintenance`)
/// can reach the CRM pool without threading it through `AppState`/`McpManager` —
/// same bridge pattern as `DEFAULT_CURRENCY_PROVIDER` above.
static BACKUP_POOL: std::sync::OnceLock<SqlitePool> = std::sync::OnceLock::new();

/// The CRM database pool, if the in-process CRM service has finished
/// initializing. `None` before boot registers it (or if in-process MCP init
/// failed) — callers should treat that as "skip the CRM backup this round."
pub fn backup_pool() -> Option<SqlitePool> {
    BACKUP_POOL.get().cloned()
}

pub struct CrmService {
    _state: Arc<AppState>,
    pool: SqlitePool,
}

impl CrmService {
    pub async fn new(state: Arc<AppState>) -> Result<Self> {
        let pool = db::open(&data_dir()).await?;
        let _ = BACKUP_POOL.set(pool.clone());
        Ok(Self {
            _state: state,
            pool,
        })
    }

    pub fn tool_list() -> Vec<Tool> {
        vec![
            // ── Leads (7) ────────────────────────────────────────────────
            Tool::new("crm_lead_create", "Create a new CRM lead. Status options: Open, Contacted, Qualified, Lost. Rejects a duplicate active email or phone with the existing lead's id — update that lead instead, or pass allow_duplicate: true.", schema!({
                    "name":    { "type": "string" },
                    "email":   { "type": "string" },
                    "phone":   { "type": "string" },
                    "company": { "type": "string" },
                    "org_id":  { "type": "string", "description": "Link to an existing Organization ID" },
                    "status":  { "type": "string", "enum": ["Open", "Contacted", "Qualified", "Lost"], "default": "Open" },
                    "source":  { "type": "string", "description": "Lead source, e.g. Website, Referral, Cold Outreach" },
                    "tags":    { "type": "array", "items": { "type": "string" } },
                    "notes":   { "type": "string" },
                    "allow_duplicate": { "type": "boolean", "default": false, "description": "Create even if an active lead with the same email or phone exists" }
                }, ["name"])),
            Tool::new("crm_lead_list", "List CRM leads, optionally filtered by status. Supports pagination.", schema!({
                    "status": { "type": "string", "enum": ["Open", "Contacted", "Qualified", "Lost", "All"], "default": "All" },
                    "limit":  { "type": "integer", "default": 50, "maximum": 200 },
                    "offset": { "type": "integer", "default": 0 }
                }, [])),
            Tool::new("crm_lead_get", "Get the full details of a CRM lead by ID.", schema!({ "id": { "type": "string" } }, ["id"])),
            Tool::new("crm_lead_update", "Update any field(s) of an existing CRM lead. Changing email or phone to one another active lead already uses is rejected with that lead's id unless allow_duplicate: true.", schema!({
                    "id":      { "type": "string" },
                    "name":    { "type": "string" },
                    "email":   { "type": "string" },
                    "phone":   { "type": "string" },
                    "company": { "type": "string" },
                    "org_id":  { "type": "string" },
                    "status":  { "type": "string", "enum": ["Open", "Contacted", "Qualified", "Lost"] },
                    "source":  { "type": "string" },
                    "tags":    { "type": "array", "items": { "type": "string" } },
                    "notes":   { "type": "string" },
                    "allow_duplicate": { "type": "boolean", "default": false, "description": "Apply even if the new email/phone collides with another active lead" }
                }, ["id"])),
            Tool::new("crm_lead_delete", "Permanently delete a CRM lead by ID. Prefer archive for safer removal.", schema!({
                    "id": { "type": "string" },
                    "confirm_permanent": { "type": "boolean", "default": false }
                }, ["id"])),
            Tool::new("crm_lead_search", "Full-text search across lead names, emails, phone numbers, companies, notes, and tags.", schema!({
                    "query":  { "type": "string" },
                    "limit":  { "type": "integer", "default": 50 },
                    "offset": { "type": "integer", "default": 0 }
                }, ["query"])),
            Tool::new("crm_lead_convert_to_deal", "Convert a lead into a deal/opportunity and optionally update the lead status. Rejects a re-conversion that would duplicate an active deal (same contact + title) with the existing deal's id unless allow_duplicate: true.", schema!({
                    "lead_id":         { "type": "string" },
                    "title":           { "type": "string", "description": "Optional deal title. Defaults from the lead/company." },
                    "amount":          { "type": "number", "minimum": 0 },
                    "currency":        { "type": "string", "description": "3-letter code; defaults to the crm.default_currency setting" },
                    "stage":           { "type": "string", "enum": ["Prospecting", "Qualified", "Proposal", "Negotiation", "Won", "Lost"], "default": "Prospecting" },
                    "probability":     { "type": "integer", "minimum": 0, "maximum": 100 },
                    "org_id":          { "type": "string", "description": "Optional org override. Defaults to the lead's org_id." },
                    "expected_close":  { "type": "string", "description": "Expected close date (ISO 8601 or YYYY-MM-DD)" },
                    "tags":            { "type": "array", "items": { "type": "string" } },
                    "notes":           { "type": "string" },
                    "lead_status":     { "type": "string", "enum": ["Open", "Contacted", "Qualified", "Lost"], "default": "Qualified" },
                    "allow_duplicate": { "type": "boolean", "default": false, "description": "Convert even if an active deal with the same contact and title exists" }
                }, ["lead_id"])),

            // ── Deals (7) ────────────────────────────────────────────────
            Tool::new("crm_deal_create", "Create a new sales deal linked to a lead or contact. Rejects a duplicate active deal (same contact + title) with the existing deal's id — update that deal instead, or pass allow_duplicate: true.", schema!({
                    "title":          { "type": "string" },
                    "amount":         { "type": "number", "minimum": 0 },
                    "currency":       { "type": "string", "description": "3-letter code; defaults to the crm.default_currency setting" },
                    "stage":          { "type": "string", "enum": ["Prospecting", "Qualified", "Proposal", "Negotiation", "Won", "Lost"], "default": "Prospecting" },
                    "probability":    { "type": "integer", "minimum": 0, "maximum": 100, "description": "Win probability as percentage" },
                    "contact_id":     { "type": "string", "description": "ID of the associated lead or contact" },
                    "org_id":         { "type": "string", "description": "ID of the associated organization" },
                    "expected_close": { "type": "string", "description": "Expected close date (ISO 8601 or YYYY-MM-DD)" },
                    "tags":           { "type": "array", "items": { "type": "string" } },
                    "notes":          { "type": "string" },
                    "allow_duplicate": { "type": "boolean", "default": false, "description": "Create even if an active deal with the same contact and title exists" }
                }, ["title", "contact_id"])),
            Tool::new("crm_deal_list", "List sales deals, optionally filtered by stage. Returns total pipeline value per currency.", schema!({
                    "stage":  { "type": "string", "enum": ["Prospecting", "Qualified", "Proposal", "Negotiation", "Won", "Lost", "All"], "default": "All" },
                    "limit":  { "type": "integer", "default": 50 },
                    "offset": { "type": "integer", "default": 0 }
                }, [])),
            Tool::new("crm_deal_get", "Get the full details of a deal by ID.", schema!({ "id": { "type": "string" } }, ["id"])),
            Tool::new("crm_deal_update", "Update any field(s) of an existing deal (e.g. advance stage, change amount).", schema!({
                    "id":             { "type": "string" },
                    "title":          { "type": "string" },
                    "amount":         { "type": "number", "minimum": 0 },
                    "currency":       { "type": "string" },
                    "stage":          { "type": "string", "enum": ["Prospecting", "Qualified", "Proposal", "Negotiation", "Won", "Lost"] },
                    "probability":    { "type": "integer", "minimum": 0, "maximum": 100 },
                    "contact_id":     { "type": "string" },
                    "org_id":         { "type": "string" },
                    "expected_close": { "type": "string", "description": "ISO 8601 or YYYY-MM-DD" },
                    "tags":           { "type": "array", "items": { "type": "string" } },
                    "notes":          { "type": "string" }
                }, ["id"])),
            Tool::new("crm_deal_delete", "Permanently delete a deal by ID. Prefer archive for safer removal.", schema!({
                    "id": { "type": "string" },
                    "confirm_permanent": { "type": "boolean", "default": false }
                }, ["id"])),
            Tool::new("crm_deal_search", "Search deals by title, notes, or tags.", schema!({
                    "query":  { "type": "string" },
                    "limit":  { "type": "integer", "default": 50 },
                    "offset": { "type": "integer", "default": 0 }
                }, ["query"])),
            Tool::new("crm_pipeline_summary", "Get a full pipeline overview: deal counts and per-currency values grouped by stage, win rate.", schema!({}, [])),

            // ── Organizations (6) ────────────────────────────────────────
            Tool::new("crm_org_create", "Create a new organization/company in the CRM. Rejects a duplicate active name (case-insensitive) with the existing org's id — use that record instead, or pass allow_duplicate: true.", schema!({
                    "name":     { "type": "string" },
                    "website":  { "type": "string" },
                    "industry": { "type": "string" },
                    "size":     { "type": "string", "description": "Company size, e.g. 1-10, 11-50, 51-200, 201-1000, 1000+" },
                    "country":  { "type": "string" },
                    "phone":    { "type": "string" },
                    "email":    { "type": "string" },
                    "tags":     { "type": "array", "items": { "type": "string" } },
                    "notes":    { "type": "string" },
                    "allow_duplicate": { "type": "boolean", "default": false, "description": "Create even if an active org with the same name exists" }
                }, ["name"])),
            Tool::new("crm_org_list", "List all organizations, optionally filtered by industry.", schema!({
                    "industry": { "type": "string" },
                    "limit":    { "type": "integer", "default": 50 },
                    "offset":   { "type": "integer", "default": 0 }
                }, [])),
            Tool::new("crm_org_get", "Get the full details of an organization by ID.", schema!({ "id": { "type": "string" } }, ["id"])),
            Tool::new("crm_org_update", "Update any field(s) of an existing organization. Renaming to a name another active org already uses (case-insensitive) is rejected with that org's id unless allow_duplicate: true.", schema!({
                    "id":       { "type": "string" },
                    "name":     { "type": "string" },
                    "website":  { "type": "string" },
                    "industry": { "type": "string" },
                    "size":     { "type": "string" },
                    "country":  { "type": "string" },
                    "phone":    { "type": "string" },
                    "email":    { "type": "string" },
                    "tags":     { "type": "array", "items": { "type": "string" } },
                    "notes":    { "type": "string" },
                    "allow_duplicate": { "type": "boolean", "default": false, "description": "Rename even if the new name collides with another active org" }
                }, ["id"])),
            Tool::new("crm_org_delete", "Permanently delete an organization by ID. Prefer archive for safer removal.", schema!({
                    "id": { "type": "string" },
                    "confirm_permanent": { "type": "boolean", "default": false }
                }, ["id"])),
            Tool::new("crm_org_search", "Search organizations by name, industry, country, website, phone, email, notes, or tags.", schema!({
                    "query":  { "type": "string" },
                    "limit":  { "type": "integer", "default": 50 },
                    "offset": { "type": "integer", "default": 0 }
                }, ["query"])),

            // ── Activities (6) ───────────────────────────────────────────
            Tool::new("crm_activity_log", "Log an activity (note, call, email, meeting, task) on a lead, deal, or org. Tasks may carry a due_at for follow-up tracking (see crm_tasks_due).", schema!({
                    "entity_id":   { "type": "string", "description": "ID of the lead, deal, or org" },
                    "entity_type": { "type": "string", "enum": ["lead", "deal", "org"] },
                    "kind":        { "type": "string", "enum": ["note", "call", "email", "meeting", "task", "other"], "default": "note" },
                    "title":       { "type": "string", "description": "Short summary of the activity" },
                    "body":        { "type": "string", "description": "Full details or transcript" },
                    "occurred_at": { "type": "string", "description": "ISO 8601 or YYYY-MM-DD (defaults to now)" },
                    "due_at":      { "type": "string", "description": "When this is due (ISO 8601 or YYYY-MM-DD). Meant for kind: task" },
                    "done":        { "type": "boolean", "default": false, "description": "Mark completed on creation (e.g. logging an already-finished task)" }
                }, ["entity_id", "entity_type", "title"])),
            Tool::new("crm_activity_list", "List activities for a given entity, or all activities. Sorted most-recent first.", schema!({
                    "entity_id": { "type": "string", "description": "Filter by entity ID (optional)" },
                    "entity_type": { "type": "string", "enum": ["lead", "deal", "org"], "description": "Optional entity type filter" },
                    "kind":      { "type": "string", "enum": ["note", "call", "email", "meeting", "task", "other"] },
                    "done":      { "type": "boolean", "description": "Filter by completion state (mostly useful with kind: task)" },
                    "limit":     { "type": "integer", "default": 50 },
                    "offset":    { "type": "integer", "default": 0 }
                }, [])),
            Tool::new("crm_activity_get", "Get the full details of an activity by ID.", schema!({ "id": { "type": "string" } }, ["id"])),
            Tool::new("crm_activity_update", "Update an existing activity log entry: reassign it, set/clear its due date, or mark a task done.", schema!({
                    "id":          { "type": "string" },
                    "entity_id":   { "type": "string", "description": "If provided, must be paired with entity_type" },
                    "entity_type": { "type": "string", "enum": ["lead", "deal", "org"] },
                    "kind":        { "type": "string", "enum": ["note", "call", "email", "meeting", "task", "other"] },
                    "title":       { "type": "string" },
                    "body":        { "type": "string" },
                    "occurred_at": { "type": "string", "description": "ISO 8601 or YYYY-MM-DD" },
                    "due_at":      { "type": "string", "description": "ISO 8601 or YYYY-MM-DD; pass null or \"\" to clear" },
                    "done":        { "type": "boolean", "description": "true = task completed" }
                }, ["id"])),
            Tool::new("crm_tasks_due", "List open (not done) task activities that are overdue or due within the window, oldest due first, each with the name of the lead/deal/org it belongs to. The follow-up worklist.", schema!({
                    "due_within_days": { "type": "integer", "default": 7, "description": "Include tasks due up to this many days from now" },
                    "include_overdue": { "type": "boolean", "default": true },
                    "include_undated": { "type": "boolean", "default": false, "description": "Also list open tasks that have no due_at" },
                    "limit":  { "type": "integer", "default": 50, "maximum": 200 },
                    "offset": { "type": "integer", "default": 0 }
                }, [])),
            Tool::new("crm_activity_delete", "Permanently delete an activity log entry by ID. Prefer archive for safer removal.", schema!({
                    "id": { "type": "string" },
                    "confirm_permanent": { "type": "boolean", "default": false }
                }, ["id"])),

            // Insights / workflows
            Tool::new("crm_record_archive", "Archive a CRM record (soft delete) so it no longer appears in normal queries.", schema!({
                    "entity_type": { "type": "string", "enum": ["org", "lead", "deal", "activity"] },
                    "id": { "type": "string" }
                }, ["entity_type", "id"])),
            Tool::new("crm_record_restore", "Restore a previously archived CRM record.", schema!({
                    "entity_type": { "type": "string", "enum": ["org", "lead", "deal", "activity"] },
                    "id": { "type": "string" }
                }, ["entity_type", "id"])),
            Tool::new("crm_archived_list", "List archived CRM records across entities, or filter by one entity type.", schema!({
                    "entity_type": { "type": "string", "enum": ["org", "lead", "deal", "activity"] },
                    "limit": { "type": "integer", "default": 50, "maximum": 200 },
                    "offset": { "type": "integer", "default": 0 }
                }, [])),
            Tool::new("crm_search_all", "Search across organizations, leads, and deals in one call.", schema!({
                    "query":          { "type": "string" },
                    "limit_per_type": { "type": "integer", "default": 10, "maximum": 50 }
                }, ["query"])),
            Tool::new("crm_record_overview", "Get a 360-degree view of a CRM record with related entities and recent activity.", schema!({
                    "entity_type":   { "type": "string", "enum": ["lead", "deal", "org"] },
                    "id":            { "type": "string" },
                    "related_limit": { "type": "integer", "default": 10, "maximum": 50 },
                    "activity_limit":{ "type": "integer", "default": 20, "maximum": 100 }
                }, ["entity_type", "id"])),
            Tool::new("crm_dashboard_summary", "Get an operational CRM dashboard: lead status mix, pipeline health, stale deals, closing-soon deals, and open/overdue task counts.", schema!({
                    "stale_days":          { "type": "integer", "default": 30 },
                    "closing_within_days": { "type": "integer", "default": 30 },
                    "activity_window_days":{ "type": "integer", "default": 30 },
                    "list_limit":          { "type": "integer", "default": 10, "maximum": 50 }
                }, [])),
            Tool::new("crm_export_snapshot", "Export a full CRM snapshot for backup, migration, or audit. Datasets over 200 records are written to a JSON file in the Files page by default (returns path + counts); pass to_file: false to force an inline dump.", schema!({
                    "include_archived": { "type": "boolean", "default": true },
                    "to_file": { "type": "boolean", "description": "Write the snapshot to a timestamped JSON file instead of returning it inline. Defaults to true when the dataset exceeds 200 records." }
                }, [])),
            Tool::new("crm_backup_db", "Back up the CRM SQLite database to a timestamped .db file in the Files page directory (online backup via VACUUM INTO; safe while the CRM is in use).", schema!({}, [])),
            Tool::new("crm_changes_since", "Change feed: active leads/deals/orgs created or updated after the 'since' cursor, oldest first, each tagged change: 'created' or 'updated'. Returns a 'cursor' to pass as the next 'since' (has_more: true means the window was cut by 'limit' — poll again). Powers the CRM workflow trigger; also handy for \"what changed today\" checks.", schema!({
                    "since":        { "type": "string", "description": "RFC 3339 timestamp (any offset) or YYYY-MM-DD, or the cursor returned by a previous call" },
                    "entity_types": { "type": "array", "items": { "type": "string", "enum": ["lead", "deal", "org"] }, "description": "Default: all three" },
                    "limit":        { "type": "integer", "default": 50, "maximum": 200 }
                }, ["since"])),
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
            "crm_tasks_due" => activities::tasks_due(pool, a).await,

            // Insights / workflows
            "crm_record_archive" => records::archive(pool, a).await,
            "crm_record_restore" => records::restore(pool, a).await,
            "crm_archived_list" => records::archived_list(pool, a).await,
            "crm_search_all" => views::search_all(pool, a).await,
            "crm_record_overview" => views::record_overview(pool, a).await,
            "crm_dashboard_summary" => views::dashboard_summary(pool, a).await,
            "crm_export_snapshot" => records::export_snapshot(pool, a).await,
            "crm_backup_db" => records::backup_db(pool).await,
            "crm_changes_since" => views::changes_since(pool, a).await,

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
