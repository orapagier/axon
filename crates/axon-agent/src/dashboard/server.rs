use super::{api, media, ws};
use crate::state::AppState;
use axum::{routing::get, Router};
use tower_http::cors::CorsLayer;
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
        .route("/ws", get(ws::ws_handler))
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
        .layer(CorsLayer::permissive())
        .layer(axum::extract::DefaultBodyLimit::max(50 * 1024 * 1024))
        .with_state(state)
}
