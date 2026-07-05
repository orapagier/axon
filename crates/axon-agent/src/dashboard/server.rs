use super::{api, media, ws};
use crate::state::AppState;
use axum::{routing::get, Router};
use tower_http::services::{ServeDir, ServeFile};

async fn health_check() -> &'static str {
    "OK"
}

async fn ready_check() -> &'static str {
    "READY"
}

pub fn build_router(state: AppState) -> Router {
    let protected = Router::new()
        .route(
            "/api/settings",
            get(api::get_settings).post(api::update_setting),
        )
        .route(
            "/api/settings/:key",
            axum::routing::put(api::update_setting_by_key),
        )
        .route("/api/integrations/status", get(api::get_auth_status))
        .route(
            "/api/facebook/connect-url",
            get(api::get_facebook_connect_url),
        )
        .route(
            "/api/integrations/:platform/url",
            axum::routing::post(api::get_auth_url),
        )
        .route(
            "/api/integrations/:platform/disconnect",
            axum::routing::post(api::disconnect_auth),
        )
        .route("/api/runs", get(api::get_runs))
        .route("/api/runs/:id", get(api::get_run_detail))
        .route("/api/models", get(api::get_models).post(api::add_model))
        .route(
            "/api/models/:name",
            axum::routing::put(api::update_model).delete(api::delete_model),
        )
        .route(
            "/api/models/bulk",
            axum::routing::put(api::update_models_bulk),
        )
        .route(
            "/api/models/:name/reset",
            axum::routing::post(api::reset_model),
        )
        .route("/api/tools", get(api::get_tools))
        .route("/api/fonts", get(api::get_fonts))
        .route("/api/fovea/folders", get(api::get_fovea_folders))
        .route("/api/database/list", get(api::get_database_list))
        .route("/api/google/calendars", get(api::get_google_calendars))
        .route("/api/google/sheets", get(api::get_google_sheets))
        .route(
            "/api/google/sheets/:spreadsheet_id/tabs",
            get(api::get_google_sheet_tabs),
        )
        .route("/api/tools/reload", axum::routing::post(api::reload_tools))
        .route(
            "/api/retention/run",
            axum::routing::post(api::run_retention_now),
        )
        .route("/api/tools/:name", axum::routing::put(api::toggle_tool))
        .route(
            "/api/patterns",
            get(api::get_patterns).post(api::add_pattern),
        )
        .route(
            "/api/patterns/bulk",
            axum::routing::put(api::update_patterns_bulk),
        )
        .route(
            "/api/patterns/:id",
            axum::routing::put(api::toggle_pattern).delete(api::delete_pattern),
        )
        .route("/api/patterns/test", axum::routing::post(api::test_routing))
        .route(
            "/api/conversations",
            get(api::list_conversations).post(api::create_conversation),
        )
        .route(
            "/api/conversations/:id",
            axum::routing::put(api::rename_conversation).delete(api::delete_conversation),
        )
        .route(
            "/api/conversations/:id/messages",
            get(api::get_conversation_messages),
        )
        .route("/api/memory/recent", get(api::get_memory_recent))
        .route(
            "/api/memory/search",
            axum::routing::post(api::search_memory),
        )
        .route("/api/memory/:id", axum::routing::delete(api::delete_memory))
        .route("/api/jobs", get(api::get_jobs).post(api::create_job))
        .route("/api/jobs/:id", axum::routing::put(api::update_job))
        .route("/api/jobs/:id/run", axum::routing::post(api::run_job))
        .route("/api/jobs/:id/pause", axum::routing::post(api::pause_job))
        .route("/api/jobs/:id/resume", axum::routing::post(api::resume_job))
        .route(
            "/api/jobs/:id/delete",
            axum::routing::delete(api::delete_job),
        )
        .route("/api/run", axum::routing::post(api::run_api))
        .route("/api/mcp", get(api::get_mcp).post(api::connect_mcp))
        .route("/api/mcp/:name", axum::routing::delete(api::disconnect_mcp))
        .route("/api/messaging/status", get(api::get_messaging_status))
        .route(
            "/api/messaging/reconnect/:platform",
            axum::routing::post(api::reconnect_messaging),
        )
        .route(
            "/api/files/delete-all",
            axum::routing::delete(api::delete_all_files),
        )
        .route("/api/files/:dir", get(api::get_files))
        .route(
            "/api/files/:dir/:id",
            axum::routing::delete(api::delete_file),
        )
        .route("/api/download", get(api::download_file))
        .route("/api/upload", axum::routing::post(api::upload_file))
        .route("/api/slack/events", axum::routing::post(api::slack_events))
        .route(
            "/api/watchers",
            get(api::get_watchers).post(api::upsert_watcher),
        )
        .route(
            "/api/watchers/:id",
            axum::routing::put(api::toggle_watcher).delete(api::delete_watcher),
        )
        .route(
            "/api/watchers/:id/run",
            axum::routing::post(api::run_watcher),
        )
        .route("/api/watchers/log", get(api::get_watcher_log))
        .route(
            "/api/ssh_servers",
            get(api::get_ssh_servers).post(api::add_ssh_server),
        )
        .route(
            "/api/ssh_servers/:name",
            axum::routing::delete(api::delete_ssh_server),
        )
        .route(
            "/api/websearch/accounts",
            get(api::get_websearch_accounts).post(api::upsert_websearch_account),
        )
        .route(
            "/api/websearch/accounts/:id",
            axum::routing::delete(api::delete_websearch_account),
        )
        .route(
            "/api/websearch/reset",
            axum::routing::post(api::reset_websearch_quotas),
        )
        .route(
            "/api/synapses",
            get(api::get_synapses).post(api::upsert_synapse),
        )
        .route(
            "/api/synapses/:id",
            axum::routing::delete(api::delete_synapse),
        )
        .route(
            "/api/synapses/:id/run",
            axum::routing::post(api::run_saved_synapse),
        )
        .route(
            "/api/synapse/adhoc",
            axum::routing::post(api::run_synapse_adhoc),
        )
        .route(
            "/api/workflows",
            get(api::get_workflows).post(api::upsert_workflow),
        )
        .route(
            "/api/workflows/import",
            axum::routing::post(api::import_workflow),
        )
        .route("/api/workflows/:id/export", get(api::export_workflow))
        .route(
            "/api/workflows/:id",
            axum::routing::delete(api::delete_workflow),
        )
        .route(
            "/api/workflows/:id/run",
            axum::routing::post(api::run_workflow),
        )
        .route(
            "/api/workflows/:id/run/:node_id",
            axum::routing::post(api::run_workflow_node),
        )
        .route(
            "/api/workflows/:id/nodes/:node_id/pin",
            axum::routing::post(api::pin_workflow_node).delete(api::unpin_workflow_node),
        )
        .route(
            "/api/workflows/:id/versions",
            get(api::get_workflow_versions),
        )
        .route(
            "/api/workflows/:id/versions/:version",
            get(api::get_workflow_version).post(api::label_workflow_version),
        )
        .route(
            "/api/workflows/:id/versions/:version/restore",
            axum::routing::post(api::restore_workflow_version),
        )
        .route("/api/workflows/:id/runs", get(api::get_workflow_runs))
        .route(
            "/api/workflow-runs/:run_id",
            get(api::get_workflow_run_by_id),
        )
        .route(
            "/api/workflows/:id/stop",
            axum::routing::post(api::stop_workflow),
        )
        .route("/api/mcp/tools", get(api::get_mcp_tools))
        .route(
            "/api/credentials",
            get(api::get_credentials).post(api::upsert_credential),
        )
        .route(
            "/api/credentials/:id",
            axum::routing::delete(api::delete_credential),
        )
        .route(
            "/api/credentials/:id/test",
            axum::routing::post(api::test_credential),
        )
        // CRM page (Phase 5): thin wrappers over the in-process crm_* tools.
        // Static segments (pipeline/dashboard/…) coexist with the `:entity`
        // param routes — matchit gives statics priority (see route test below).
        .route("/api/crm/pipeline", get(api::crm_get_pipeline))
        .route("/api/crm/dashboard", get(api::crm_get_dashboard))
        .route("/api/crm/archived", get(api::crm_get_archived))
        .route("/api/crm/search", get(api::crm_search_all_records))
        .route("/api/crm/overview/:entity/:id", get(api::crm_get_overview))
        .route(
            "/api/crm/:entity",
            get(api::crm_list_records).post(api::crm_create_record),
        )
        .route(
            "/api/crm/:entity/:id",
            get(api::crm_get_record).put(api::crm_update_record),
        )
        .route(
            "/api/crm/:entity/:id/archive",
            axum::routing::post(api::crm_archive_record),
        )
        .route(
            "/api/crm/:entity/:id/restore",
            axum::routing::post(api::crm_restore_record),
        )
        .route("/ws", get(ws::ws_handler))
        // C3: observability. /metrics is the Prometheus scrape target; /api/health
        // is a compact JSON status. Both sit behind require_auth — Prometheus
        // scrapes with `bearer_token: <AXON_MASTER_KEY>`.
        .route("/metrics", get(crate::observability::metrics_endpoint))
        .route("/api/health", get(crate::observability::health_json))
        .layer(axum::middleware::from_fn(super::auth::require_auth));

    Router::new()
        .route("/health", get(health_check))
        .route("/ready", get(ready_check))
        // Public webhook endpoints (no auth — Facebook can't authenticate)
        .route(
            "/webhook/facebook",
            get(crate::webhook::facebook::fb_verify).post(crate::webhook::facebook::fb_event),
        )
        .route(
            "/webhook/telegram",
            axum::routing::post(api::telegram_webhook),
        )
        .route(
            "/webhook/whatsapp",
            get(api::whatsapp_webhook_verify).post(api::whatsapp_webhook_messages),
        )
        .route(
            "/webhook/external/:workflow_id",
            axum::routing::post(crate::webhook::external::handle_external_webhook),
        )
        .route(
            "/webhook/github/:workflow_id",
            axum::routing::post(crate::webhook::github::handle_github_webhook),
        )
        // C1: tokenized resume URLs for Wait-for-webhook / Approval nodes. No auth
        // by necessity — the unguessable single-use token IS the credential. GET is
        // allowed so an approval link clicked from an email resumes the run.
        .route(
            "/webhook/resume/:node_id/:run_id",
            get(crate::webhook::external::handle_resume)
                .post(crate::webhook::external::handle_resume),
        )
        .route(
            "/webhook/approve/:node_id/:run_id",
            get(crate::webhook::external::handle_approve)
                .post(crate::webhook::external::handle_approve),
        )
        .route(
            "/webhook/reject/:node_id/:run_id",
            get(crate::webhook::external::handle_reject)
                .post(crate::webhook::external::handle_reject),
        )
        // OAuth callback — Google/Microsoft/Facebook redirect here after login
        .route("/auth/:service/callback", get(api::oauth_callback))
        // Temporary local-media for Instagram publishing (Meta fetches these;
        // no auth). Served in-process now that integrations are merged.
        .route(
            "/media/local/:token",
            get(media::local_media).head(media::local_media_head),
        )
        .route(
            "/media/local/:token/:name",
            get(media::local_media_named).head(media::local_media_named_head),
        )
        .merge(protected)
        .fallback_service(
            ServeDir::new("static").not_found_service(ServeFile::new("static/index.html")),
        )
        // No CORS layer: the dashboard UI is served same-origin from this
        // binary (and the Vite dev server proxies /api and /ws), webhooks are
        // server-to-server. A permissive policy only invited cross-origin
        // requests from arbitrary sites.
        .layer(axum::extract::DefaultBodyLimit::max(50 * 1024 * 1024))
        .with_state(state)
}

#[cfg(test)]
mod route_conflict_tests {
    use axum::{
        routing::{delete, get, post},
        Router,
    };

    // A5 added `/api/workflows/import` (static) at the same position as the
    // existing `/api/workflows/:id` (param). axum panics at construction on a
    // route conflict, so building this router is the assertion.
    #[test]
    fn workflows_import_and_param_routes_coexist() {
        let _r: Router<()> = Router::new()
            .route("/api/workflows", get(|| async {}).post(|| async {}))
            .route("/api/workflows/import", post(|| async {}))
            .route("/api/workflows/:id/export", get(|| async {}))
            .route("/api/workflows/:id", delete(|| async {}))
            .route("/api/workflows/:id/run", post(|| async {}))
            // B1: versioning routes share the `:id` prefix with the above.
            .route("/api/workflows/:id/versions", get(|| async {}))
            .route(
                "/api/workflows/:id/versions/:version",
                get(|| async {}).post(|| async {}),
            )
            .route(
                "/api/workflows/:id/versions/:version/restore",
                post(|| async {}),
            );
    }

    // Phase 5 CRM routes mix static segments (/pipeline, /dashboard, /archived,
    // /search, /overview) with `:entity` param routes at the same position, and
    // `:entity/:id` under them. Building the router is the assertion.
    #[test]
    fn crm_static_and_entity_param_routes_coexist() {
        let _r: Router<()> = Router::new()
            .route("/api/crm/pipeline", get(|| async {}))
            .route("/api/crm/dashboard", get(|| async {}))
            .route("/api/crm/archived", get(|| async {}))
            .route("/api/crm/search", get(|| async {}))
            .route("/api/crm/overview/:entity/:id", get(|| async {}))
            .route("/api/crm/:entity", get(|| async {}).post(|| async {}))
            .route("/api/crm/:entity/:id", get(|| async {}).put(|| async {}))
            .route("/api/crm/:entity/:id/archive", post(|| async {}))
            .route("/api/crm/:entity/:id/restore", post(|| async {}));
    }
}
