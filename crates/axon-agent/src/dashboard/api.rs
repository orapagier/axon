use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::{http::HeaderMap, Json};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;
pub async fn get_google_sheets(State(state): State<AppState>) -> Json<Value> {
    if let Ok(res) = state
        .tools
        .run("gsheets_list", json!({"max_results": 100}))
        .await
    {
        Json(res)
    } else {
        Json(json!({"files": []}))
    }
}

pub async fn get_google_sheet_tabs(
    State(state): State<AppState>,
    Path(spreadsheet_id): Path<String>,
) -> Json<Value> {
    let res = match state
        .tools
        .run("gsheets_get", json!({"spreadsheet_id": spreadsheet_id}))
        .await
    {
        Ok(value) => value,
        Err(e) => {
            return Json(json!({
                "tabs": [],
                "sheet_id_map": {},
                "error": e.to_string(),
            }))
        }
    };

    let tabs: Vec<Value> = res
        .get("sheets")
        .and_then(|v| v.as_array())
        .map(|sheets| {
            sheets
                .iter()
                .filter_map(|sheet| {
                    let props = sheet.get("properties")?;
                    let title = props
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Untitled")
                        .to_string();
                    let sheet_id = props.get("sheetId").and_then(|v| v.as_i64())?;
                    Some(json!({
                        "title": title,
                        "sheet_id": sheet_id,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut sheet_id_map = serde_json::Map::new();
    for tab in &tabs {
        if let (Some(title), Some(sheet_id)) = (
            tab.get("title").and_then(|v| v.as_str()),
            tab.get("sheet_id"),
        ) {
            sheet_id_map.insert(title.to_string(), sheet_id.clone());
        }
    }

    Json(json!({
        "tabs": tabs,
        "sheet_id_map": sheet_id_map,
    }))
}

pub async fn get_google_calendars(State(state): State<AppState>) -> Json<Value> {
    if let Ok(res) = state.tools.run("gcal_list_calendars", json!({})).await {
        // gcal_list_calendars returns { calendars: [ { id, summary, primary, ... }, ... ] }
        let calendars: Vec<Value> = res
            .get("calendars")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|cal| {
                        let id = cal.get("id").and_then(|v| v.as_str())?.to_string();
                        let name = cal
                            .get("summary")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&id)
                            .to_string();
                        let primary = cal
                            .get("primary")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        Some(json!({ "name": name, "value": id, "primary": primary }))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Json(json!({ "calendars": calendars }))
    } else {
        Json(json!({ "calendars": [] }))
    }
}

pub async fn get_settings(State(state): State<AppState>) -> Json<Value> {
    let mut settings = state.settings.all().unwrap_or_default();
    // Hide legacy "providers" category — Models page handles API keys now
    settings.retain(|s| s.category.as_deref() != Some("providers"));
    Json(json!({"settings": settings}))
}

fn instagram_setting_to_mcp_env(key: &str) -> Option<&'static str> {
    match key {
        "instagram.public_base_url" => Some("AXON_PUBLIC_BASE_URL"),
        "instagram.bind_addr" => Some("AXON_BIND_ADDR"),
        "instagram.media_url_ttl_secs" => Some("AXON_MEDIA_URL_TTL_SECS"),
        "instagram.image_poll_interval_secs" => Some("AXON_IG_IMAGE_POLL_INTERVAL_SECS"),
        "instagram.image_poll_timeout_secs" => Some("AXON_IG_IMAGE_POLL_TIMEOUT_SECS"),
        "instagram.video_poll_interval_secs" => Some("AXON_IG_VIDEO_POLL_INTERVAL_SECS"),
        "instagram.video_poll_timeout_secs" => Some("AXON_IG_VIDEO_POLL_TIMEOUT_SECS"),
        _ => None,
    }
}

/// The Instagram/MCP settings that map to process env vars the in-process
/// integration services (and OAuth redirect-URI builder) read.
const MCP_ENV_SETTING_KEYS: &[&str] = &[
    "instagram.public_base_url",
    "instagram.bind_addr",
    "instagram.media_url_ttl_secs",
    "instagram.image_poll_interval_secs",
    "instagram.image_poll_timeout_secs",
    "instagram.video_poll_interval_secs",
    "instagram.video_poll_timeout_secs",
];

/// Apply these settings to the agent's own process environment at startup, so
/// the in-process MCP services see the operator's configuration. Must be called
/// before the in-process backend is constructed. Explicit `.env`/environment
/// values win, and the placeholder base URL is ignored.
pub fn apply_mcp_env_from_settings(settings: &crate::config::RuntimeSettings) {
    for key in MCP_ENV_SETTING_KEYS {
        let Some(env_key) = instagram_setting_to_mcp_env(key) else {
            continue;
        };
        if std::env::var(env_key).is_ok() {
            continue; // respect values already set via .env / environment
        }
        let val = settings.get_str(key, "");
        let val = val.trim();
        if val.is_empty() || val == "https://mcp.yourdomain.com" {
            continue;
        }
        std::env::set_var(env_key, val);
    }
}

fn sync_instagram_setting_to_mcp_env(key: &str, value: &str) {
    // Integrations run in-process now, so apply the change to our own
    // environment instead of writing a separate server's .env file. Values read
    // per-request (OAuth redirect URI, media base URL) take effect immediately;
    // values read at service construction still need an agent restart.
    if let Some(env_key) = instagram_setting_to_mcp_env(key) {
        std::env::set_var(env_key, value.trim());
    }
}

pub async fn update_setting(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    if let (Some(k), Some(v)) = (
        payload.get("key").and_then(|v| v.as_str()),
        payload.get("value").and_then(|v| v.as_str()),
    ) {
        let _ = state.settings.set(k, v);
        sync_instagram_setting_to_mcp_env(k, v);
    }
    Json(json!({"ok":true}))
}

pub async fn update_setting_by_key(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    if let Some(v) = payload.get("value").and_then(|v| v.as_str()) {
        let _ = state.settings.set(&key, v);
        sync_instagram_setting_to_mcp_env(&key, v);
    }
    Json(json!({"ok":true}))
}

// ── AUTHENTICATION ────────────────────────────────────────────────────────────

pub async fn get_auth_status(State(state): State<AppState>) -> Json<Value> {
    let mut results = serde_json::Map::new();

    // Google
    if let Ok(res) = state.tools.run("google_auth_status", json!({})).await {
        results.insert("google".to_string(), res);
    } else {
        results.insert("google".to_string(), json!({"status": "error"}));
    }

    // Microsoft
    if let Ok(res) = state.tools.run("microsoft_auth_status", json!({})).await {
        results.insert("microsoft".to_string(), res);
    } else {
        results.insert("microsoft".to_string(), json!({"status": "error"}));
    }

    // Facebook
    let fb_status = state
        .tools
        .run("facebook_auth_status", json!({}))
        .await
        .unwrap_or(json!({"authenticated": false}));
    results.insert("facebook".to_string(), fb_status.clone());

    // Instagram (extract from Facebook response)
    let ig_connected = fb_status
        .get("instagram_connected")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    results.insert(
        "instagram".to_string(),
        json!({
            "authenticated": ig_connected,
            "user": if ig_connected { Some("Connected") } else { None }
        }),
    );

    Json(json!({"auth_status": results}))
}

pub async fn get_auth_url(
    State(state): State<AppState>,
    Path(platform): Path<String>,
) -> Json<Value> {
    let tool_name = if platform == "instagram" {
        "facebook_instagram_auth_url".to_string()
    } else {
        format!("{}_auth_url", platform)
    };
    if let Ok(res) = state.tools.run(&tool_name, json!({})).await {
        return Json(res);
    }
    Json(json!({"error": format!("Failed to get {} auth URL", platform)}))
}

/// Facebook node "Connect a Page" button — returns the OAuth URL whose callback
/// saves each managed Page as a credential (state=fbcred).
pub async fn get_facebook_connect_url(State(state): State<AppState>) -> Json<Value> {
    match state.tools.run("facebook_connect_url", json!({})).await {
        Ok(res) => Json(res),
        Err(e) => Json(json!({ "error": format!("Failed to get Facebook connect URL: {e}") })),
    }
}

pub async fn disconnect_auth(
    State(state): State<AppState>,
    Path(platform): Path<String>,
) -> Json<Value> {
    let tool_name = if platform == "instagram" || platform == "facebook" {
        "facebook_revoke".to_string()
    } else {
        format!("{}_revoke", platform)
    };
    if let Ok(res) = state.tools.run(&tool_name, json!({})).await {
        return Json(res);
    }
    Json(json!({"error": format!("Failed to disconnect {}", platform)}))
}

/// OAuth callback handler — Google/Microsoft/Facebook redirect here after login.
/// Exchanges the authorization code via the MCP server tools.
pub async fn oauth_callback(
    State(state): State<AppState>,
    Path(service): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> axum::response::Html<String> {
    let code = params.get("code").cloned();
    let error = params
        .get("error_description")
        .or_else(|| params.get("error"))
        .cloned();

    // "Connect a Page as a credential" flow (the Facebook node's Connect button).
    // Marked by state=fbcred; saves one credential per managed Page instead of
    // overwriting the global Page token.
    let connect_creds = service == "facebook" && params.get("state").map(String::as_str) == Some("fbcred");

    match (code, error) {
        (Some(code), _) if connect_creds => {
            return facebook_connect_callback(&state, &code).await;
        }
        (Some(code), _) => {
            let tool_name = format!("{}_exchange_code", service);
            let mut args = json!({"code": code});

            // Facebook/Instagram need the service name
            if service == "facebook" || service == "instagram" {
                args["service"] = json!(service);
            }

            match state.tools.run(&tool_name, args).await {
                Ok(_) => {
                    let label = match service.as_str() {
                        "google" => "Google Workspace",
                        "microsoft" => "Microsoft 365",
                        "facebook" => "Facebook Page & Instagram",
                        "instagram" => "Instagram",
                        _ => &service,
                    };
                    axum::response::Html(format!(
                        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>Axon</title>
<style>body{{font-family:system-ui,sans-serif;display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;background:#f0fdf4}}
.card{{background:#fff;border-radius:16px;padding:48px;box-shadow:0 10px 25px rgba(0,0,0,.1);text-align:center;max-width:480px}}
h1{{color:#16a34a;margin:0 0 12px}}p{{color:#6b7280;margin:0;line-height:1.5}}</style>
<script>setTimeout(()=>{{ window.close(); }}, 3000);</script></head>
<body><div class="card"><h1>✅ {label} Authenticated</h1>
<p>Tokens saved successfully. This tab will close automatically in 3 seconds...</p></div></body></html>"#
                    ))
                }
                Err(e) => axum::response::Html(format!(
                    r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>Axon Error</title>
<style>body{{font-family:system-ui,sans-serif;display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;background:#fef2f2}}
.card{{background:#fff;border-radius:16px;padding:48px;box-shadow:0 10px 25px rgba(0,0,0,.1);text-align:center;max-width:480px}}
h1{{color:#dc2626;margin:0 0 12px}}p{{color:#6b7280;margin:0;word-break:break-word;line-height:1.5}}</style></head>
<body><div class="card"><h1>❌ Error</h1><p>{}</p></div></body></html>"#,
                    e
                )),
            }
        }
        (None, Some(err)) => axum::response::Html(format!(
            r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>Axon Error</title>
<style>body{{font-family:system-ui,sans-serif;display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;background:#fef2f2}}
.card{{background:#fff;border-radius:16px;padding:48px;box-shadow:0 10px 25px rgba(0,0,0,.1);text-align:center;max-width:480px}}
h1{{color:#dc2626;margin:0 0 12px}}p{{color:#6b7280;margin:0;word-break:break-word;line-height:1.5}}</style></head>
<body><div class="card"><h1>❌ Error</h1><p>{err}</p></div></body></html>"#
        )),
        _ => axum::response::Html(
            r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>Axon Error</title>
<style>body{font-family:system-ui,sans-serif;display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;background:#fef2f2}
.card{background:#fff;border-radius:16px;padding:48px;box-shadow:0 10px 25px rgba(0,0,0,.1);text-align:center;max-width:480px}
h1{color:#dc2626;margin:0 0 12px}p{color:#6b7280;margin:0;line-height:1.5}</style></head>
<body><div class="card"><h1>❌ Error</h1><p>Missing code parameter.</p></div></body></html>"#.to_string()
        ),
    }
}

/// Facebook "Connect a Page" callback: exchanges the OAuth code for every Page
/// the user manages and saves each as its own credential (service "facebook").
/// The credential id is derived from the Page id so reconnecting refreshes the
/// token in place instead of creating duplicates.
async fn facebook_connect_callback(state: &AppState, code: &str) -> axum::response::Html<String> {
    let result = state
        .tools
        .run("facebook_exchange_code_pages", json!({ "code": code }))
        .await;

    let pages = match result {
        Ok(v) => v
            .get("pages")
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default(),
        Err(e) => return connect_error_html(&e.to_string()),
    };

    if pages.is_empty() {
        return connect_error_html("No Pages were returned for this account.");
    }

    let mut saved: Vec<String> = Vec::new();
    if let Ok(conn) = state.db.get() {
        for page in &pages {
            let page_id = page.get("page_id").and_then(|v| v.as_str()).unwrap_or("");
            if page_id.is_empty() {
                continue;
            }
            let page_name = page
                .get("page_name")
                .and_then(|v| v.as_str())
                .unwrap_or(page_id);
            let cred_id = format!("fb-{page_id}");
            let data = json!({
                "page_access_token": page.get("page_access_token").and_then(|v| v.as_str()).unwrap_or(""),
                "page_id": page_id,
                "page_name": page_name,
                "instagram_id": page.get("instagram_id").cloned().unwrap_or(Value::Null),
            });
            let data_str = serde_json::to_string(&data).unwrap_or_else(|_| "{}".to_string());
            let res = conn.execute(
                "INSERT OR REPLACE INTO credentials (id, name, service, data, created_at)
                 VALUES (?1, ?2, 'facebook', ?3, datetime('now'))",
                rusqlite::params![cred_id, page_name, data_str],
            );
            match res {
                Ok(_) => {
                    // `webhooks_subscribed` is set by exchange_code_pages when it
                    // calls subscribed_apps for this Page. Show it so the user knows
                    // the Page will actually receive events, not just post.
                    let subscribed = page
                        .get("webhooks_subscribed")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let mark = if subscribed { "✅ webhooks active" } else { "⚠️ webhooks not subscribed" };
                    saved.push(format!("{page_name} — {mark}"));
                }
                Err(e) => tracing::error!("FB connect: failed to save credential for {page_name}: {e}"),
            }
        }
    } else {
        return connect_error_html("Database unavailable while saving credentials.");
    }

    let list = saved
        .iter()
        .map(|n| format!("<li>{n}</li>"))
        .collect::<String>();
    axum::response::Html(format!(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>Axon</title>
<style>body{{font-family:system-ui,sans-serif;display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;background:#f0fdf4}}
.card{{background:#fff;border-radius:16px;padding:48px;box-shadow:0 10px 25px rgba(0,0,0,.1);text-align:center;max-width:480px}}
h1{{color:#16a34a;margin:0 0 12px}}p{{color:#6b7280;margin:0 0 8px;line-height:1.5}}ul{{text-align:left;color:#374151;margin:12px auto;display:inline-block}}</style>
<script>setTimeout(()=>{{ window.close(); }}, 3500);</script></head>
<body><div class="card"><h1>✅ {} Page(s) connected</h1>
<p>Saved as credentials you can pick in the Facebook node:</p><ul>{}</ul>
<p>This tab will close automatically...</p></div></body></html>"#,
        saved.len(),
        list
    ))
}

fn connect_error_html(msg: &str) -> axum::response::Html<String> {
    axum::response::Html(format!(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>Axon Error</title>
<style>body{{font-family:system-ui,sans-serif;display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;background:#fef2f2}}
.card{{background:#fff;border-radius:16px;padding:48px;box-shadow:0 10px 25px rgba(0,0,0,.1);text-align:center;max-width:480px}}
h1{{color:#dc2626;margin:0 0 12px}}p{{color:#6b7280;margin:0;word-break:break-word;line-height:1.5}}</style></head>
<body><div class="card"><h1>❌ Connect failed</h1><p>{msg}</p></div></body></html>"#
    ))
}

// ── NEW API ENDPOINTS ─────────────────────────────────────────────────────────

pub async fn get_runs(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let job_id = params.get("job_id");
        let query = if job_id.is_some() {
            "SELECT id, task, status, iterations, total_tokens, platform, models_used, tools_used, result, created_at, job_id, parent_run_id FROM runs WHERE job_id = ?1 ORDER BY created_at DESC LIMIT 10"
        } else {
            "SELECT id, task, status, iterations, total_tokens, platform, models_used, tools_used, result, created_at, job_id, parent_run_id FROM runs ORDER BY created_at DESC LIMIT 10"
        };
        let mut s = match conn.prepare(query) {
            Ok(stmt) => stmt,
            Err(e) => {
                tracing::error!("Failed to prepare get_runs query: {}", e);
                return Json(json!({"runs": []}));
            }
        };

        // Helper to map a row safely ignoring potential NULLs on numerics
        let map_row = |r: &rusqlite::Row| -> rusqlite::Result<Value> {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "task": r.get::<_, String>(1)?,
                "status": r.get::<_, String>(2)?,
                "iterations": r.get::<_, Option<u32>>(3)?.unwrap_or(0),
                "total_tokens": r.get::<_, Option<u32>>(4)?.unwrap_or(0),
                "platform": r.get::<_, Option<String>>(5)?,
                "models_used": r.get::<_, Option<String>>(6)?,
                "tools_used": r.get::<_, Option<String>>(7)?,
                "result": r.get::<_, Option<String>>(8)?,
                "created_at": r.get::<_, String>(9)?,
                "job_id": r.get::<_, Option<String>>(10)?,
                "parent_run_id": r.get::<_, Option<String>>(11)?,
            }))
        };

        let runs: Vec<Value> = if let Some(jid) = job_id {
            match s.query_map(rusqlite::params![jid], map_row) {
                Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                Err(e) => {
                    tracing::error!("Failed to query_map get_runs (job_id): {}", e);
                    vec![]
                }
            }
        } else {
            match s.query_map([], map_row) {
                Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                Err(e) => {
                    tracing::error!("Failed to query_map get_runs: {}", e);
                    vec![]
                }
            }
        };
        return Json(json!({"runs": runs}));
    }
    Json(json!({"runs": []}))
}

pub async fn get_run_detail(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let mut s_iter = conn.prepare("SELECT id, iteration, model_name, tokens, tier, duration_ms, created_at FROM run_iterations WHERE run_id=?1 ORDER BY iteration ASC").unwrap();
        let iterations: Vec<Value> = s_iter
            .query_map([&id], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "iteration": r.get::<_, u32>(1)?,
                    "model_name": r.get::<_, String>(2)?,
                    "tokens": r.get::<_, u32>(3)?,
                    "tier": r.get::<_, String>(4)?,
                    "duration_ms": r.get::<_, u64>(5)?,
                    "created_at": r.get::<_, String>(6)?,
                }))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        let mut s_tools = conn.prepare("SELECT id, run_id, tool_name, args, result, error, duration_ms, parallel, created_at FROM tool_calls WHERE run_id=?1 ORDER BY created_at ASC").unwrap();
        let calls: Vec<Value> = s_tools
            .query_map([&id], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "run_id": r.get::<_, String>(1)?,
                    "tool_name": r.get::<_, String>(2)?,
                    "args": r.get::<_, Option<String>>(3)?,
                    "result": r.get::<_, Option<String>>(4)?,
                    "error": r.get::<_, Option<String>>(5)?,
                    "duration_ms": r.get::<_, Option<u64>>(6)?,
                    "parallel": r.get::<_, bool>(7)?,
                    "created_at": r.get::<_, String>(8)?,
                }))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        return Json(json!({"iterations": iterations, "tool_calls": calls}));
    }
    Json(json!({"iterations": [], "tool_calls": []}))
}

pub async fn get_models(State(state): State<AppState>) -> Json<Value> {
    let models = crate::router::get_status(&state.router).await;
    Json(json!({"models": models}))
}

pub async fn reset_model(State(state): State<AppState>, Path(name): Path<String>) -> Json<Value> {
    let _ = crate::router::reset_model(&state.router, &name).await;
    Json(json!({"ok": true}))
}

pub async fn add_model(State(state): State<AppState>, Json(m): Json<Value>) -> Json<Value> {
    let name = m.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let provider = crate::providers::normalize_provider_name(
        m.get("provider").and_then(|v| v.as_str()).unwrap_or(""),
    );
    let model_id = m.get("model_id").and_then(|v| v.as_str());
    let raw_api_key = m.get("api_key").and_then(|v| v.as_str()).unwrap_or("");
    let base_url = crate::providers::normalize_base_url(
        m.get("base_url")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
    );
    let timeout_secs = m
        .get("timeout_secs")
        .and_then(|v| v.as_i64())
        .filter(|v| *v > 0);
    let priority = m.get("priority").and_then(|v| v.as_i64()).unwrap_or(99);
    let max_tokens = m.get("max_tokens").and_then(|v| v.as_i64()).unwrap_or(4096);
    let role = m.get("role").and_then(|v| v.as_str()).unwrap_or("");

    if name.is_empty() || provider.is_empty() {
        return Json(json!({"ok": false, "error": "Name and provider are required"}));
    }

    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "INSERT INTO models (name, provider, model_id, api_key, base_url, timeout_secs, priority, max_tokens, role, enabled) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1)",
            rusqlite::params![name, provider, model_id, crate::crypto::encrypt_key(raw_api_key), base_url, timeout_secs, priority, max_tokens, role],
        );
        let new_models = crate::config::load_models_from_db(&conn).unwrap_or_default();
        crate::router::update_models(&state.router, new_models).await;
        return Json(json!({"ok": true}));
    }
    Json(json!({"ok": false, "error": "DB error"}))
}

pub async fn update_model(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(m): Json<Value>,
) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        if let Some(enabled) = m.get("enabled").and_then(|v| v.as_bool()) {
            let _ = conn.execute(
                "UPDATE models SET enabled=?1 WHERE name=?2",
                rusqlite::params![if enabled { 1 } else { 0 }, name],
            );
        }
        if let Some(priority) = m.get("priority").and_then(|v| v.as_i64()) {
            let _ = conn.execute(
                "UPDATE models SET priority=?1 WHERE name=?2",
                rusqlite::params![priority, name],
            );
        }
        if let Some(role) = m.get("role").and_then(|v| v.as_str()) {
            let _ = conn.execute(
                "UPDATE models SET role=?1 WHERE name=?2",
                rusqlite::params![role, name],
            );
        }
        if let Some(raw_api_key) = m.get("api_key").and_then(|v| v.as_str()) {
            if !raw_api_key.trim().is_empty() {
                let _ = conn.execute(
                    "UPDATE models SET api_key=?1 WHERE name=?2",
                    rusqlite::params![crate::crypto::encrypt_key(raw_api_key), name],
                );
            }
        }
        if let Some(provider) = m.get("provider").and_then(|v| v.as_str()) {
            let provider = crate::providers::normalize_provider_name(provider);
            let _ = conn.execute(
                "UPDATE models SET provider=?1 WHERE name=?2",
                rusqlite::params![provider, name],
            );
        }
        if let Some(model_id) = m.get("model_id").and_then(|v| v.as_str()) {
            let _ = conn.execute(
                "UPDATE models SET model_id=?1 WHERE name=?2",
                rusqlite::params![model_id, name],
            );
        }
        if let Some(base_url) = m.get("base_url").and_then(|v| v.as_str()) {
            let base_url = crate::providers::normalize_base_url(Some(base_url.to_string()));
            let _ = conn.execute(
                "UPDATE models SET base_url=?1 WHERE name=?2",
                rusqlite::params![base_url, name],
            );
        }
        if m.get("timeout_secs").is_some() {
            let timeout_secs = m
                .get("timeout_secs")
                .and_then(|v| v.as_i64())
                .filter(|v| *v > 0);
            let _ = conn.execute(
                "UPDATE models SET timeout_secs=?1 WHERE name=?2",
                rusqlite::params![timeout_secs, name],
            );
        }
        if let Some(max_tokens) = m.get("max_tokens").and_then(|v| v.as_i64()) {
            let _ = conn.execute(
                "UPDATE models SET max_tokens=?1 WHERE name=?2",
                rusqlite::params![max_tokens, name],
            );
        }

        let new_models = crate::config::load_models_from_db(&conn).unwrap_or_default();
        crate::router::update_models(&state.router, new_models).await;
        return Json(json!({"ok": true}));
    }
    Json(json!({"ok": false, "error": "DB error"}))
}

pub async fn update_models_bulk(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let names = payload
        .get("names")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let enabled = payload
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    if names.is_empty() {
        return Json(json!({"ok": true}));
    }

    if let Ok(mut conn) = state.db.get() {
        let res: Result<(), rusqlite::Error> = (|| {
            let tx = conn.transaction()?;
            for name_val in names {
                if let Some(name) = name_val.as_str() {
                    tx.execute(
                        "UPDATE models SET enabled=?1 WHERE name=?2",
                        rusqlite::params![if enabled { 1 } else { 0 }, name],
                    )?;
                }
            }
            tx.commit()?;
            Ok(())
        })();

        if let Err(e) = res {
            tracing::error!("Bulk update models failed: {}", e);
            return Json(json!({"ok": false, "error": e.to_string()}));
        }

        let new_models = crate::config::load_models_from_db(&conn).unwrap_or_default();
        crate::router::update_models(&state.router, new_models).await;
        return Json(json!({"ok": true}));
    }
    Json(json!({"ok": false, "error": "DB error"}))
}

pub async fn delete_model(State(state): State<AppState>, Path(name): Path<String>) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute("DELETE FROM models WHERE name=?1", rusqlite::params![name]);
        let new_models = crate::config::load_models_from_db(&conn).unwrap_or_default();
        crate::router::update_models(&state.router, new_models).await;
        return Json(json!({"ok": true}));
    }
    Json(json!({"ok": false, "error": "DB error"}))
}

pub async fn get_tools(State(state): State<AppState>) -> Json<Value> {
    // ToolRegistry already contains MCP tools (registered during connect).
    // Do NOT also add mcp.all_tools() — that would duplicate them.
    let tools = state.tools.all().await;
    Json(json!({"tools": tools}))
}

pub async fn get_fonts() -> Json<Value> {
    let dir = crate::tools::image_tool::app_data_files_dir();
    let dir_str = dir
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "None".to_string());
    let fonts = crate::tools::image_tool::discover_fonts().unwrap_or_default();
    tracing::info!("get_fonts: dir={}, count={}", dir_str, fonts.len());
    Json(json!({"fonts": fonts}))
}

pub async fn get_fovea_folders() -> Json<Value> {
    let folders =
        crate::tools::image_tool::discover_image_folders().unwrap_or_else(|| vec![".".to_string()]);
    Json(json!({ "folders": folders }))
}

pub async fn toggle_tool(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let enabled = payload
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    state.tools.set_enabled(&name, enabled).await;
    Json(json!({"ok": true}))
}

pub async fn reload_tools(State(state): State<AppState>) -> Json<Value> {
    match state.tools.reload("tools").await {
        Ok(count) => Json(json!({"ok":true, "count": count})),
        Err(e) => Json(json!({"ok":false, "error": e.to_string()})),
    }
}

/// Run the database retention sweep on demand (same routine as the daily one).
/// Blocking SQLite work runs off the async runtime.
pub async fn run_retention_now(State(state): State<AppState>) -> Json<Value> {
    let db = state.db.clone();
    let settings = state.settings.clone();
    match tokio::task::spawn_blocking(move || crate::maintenance::run_retention(&db, &settings))
        .await
    {
        Ok(Ok(stats)) => {
            let summary = stats.to_string();
            Json(json!({"ok": true, "summary": summary, "stats": stats}))
        }
        Ok(Err(e)) => Json(json!({"ok": false, "error": e.to_string()})),
        Err(e) => Json(json!({"ok": false, "error": format!("task join error: {e}")})),
    }
}

pub async fn get_patterns(State(state): State<AppState>) -> Json<Value> {
    let pats = state.tool_router.all_patterns().await.unwrap_or_default();
    Json(json!({"patterns": pats}))
}

pub async fn add_pattern(State(state): State<AppState>, Json(p): Json<Value>) -> Json<Value> {
    let tool = p.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
    let pat = p.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
    let desc = p.get("description").and_then(|v| v.as_str());
    match state.tool_router.add_pattern(tool, pat, desc).await {
        Ok(_) => Json(json!({"ok":true})),
        Err(e) => Json(json!({"ok":false, "error": e.to_string()})),
    }
}

pub async fn update_patterns_bulk(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let items = payload
        .get("patterns")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    match state.tool_router.update_bulk_patterns(items).await {
        Ok(_) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn toggle_pattern(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let enabled = payload
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let _ = state.tool_router.set_enabled(id, enabled).await;
    Json(json!({"ok": true}))
}

pub async fn delete_pattern(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let _ = state.tool_router.delete_pattern(id).await;
    Json(json!({"ok": true}))
}

pub async fn test_routing(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let msg = payload
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tools = state.tools.all_enabled_for_agent().await;
    let (matched, info) = state.tool_router.filter_tools(msg, &tools, &[]).await;
    Json(json!({
        "matched_tools": matched.iter().map(|t| &t.name).collect::<Vec<_>>(),
        "routing_info": info
    }))
}

pub async fn get_memory_recent(State(state): State<AppState>) -> Json<Value> {
    let entries = state.memory.recent_memories(30, None).unwrap_or_default();
    Json(json!({"entries": entries}))
}

pub async fn search_memory(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let q = payload.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let k = payload.get("top_k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
    let results = state.memory.search(q, k, None).await.unwrap_or_default();
    Json(json!({"results": results}))
}

pub async fn delete_memory(State(state): State<AppState>, Path(id): Path<i64>) -> Json<Value> {
    let _ = state.memory.forget(id);
    Json(json!({"ok": true}))
}

pub async fn get_jobs(State(state): State<AppState>) -> Json<Value> {
    let jobs = state.scheduler.get_all().await.unwrap_or_default();
    Json(json!({"jobs": jobs}))
}

pub async fn update_job(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let task = payload.get("task").and_then(|v| v.as_str()).unwrap_or("");
    let sched = payload
        .get("schedule_nl")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if name.is_empty() || task.is_empty() || sched.is_empty() {
        return Json(json!({"ok": false, "error": "Name, task, and schedule are required"}));
    }

    match state.scheduler.update(&id, name, task, sched).await {
        Ok(_) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn run_job(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    match state.scheduler.run_once(&id).await {
        Ok(result) => Json(json!({"ok": true, "result": result})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn run_api(State(state): State<AppState>, Json(payload): Json<Value>) -> Json<Value> {
    let task = payload.get("task").and_then(|v| v.as_str()).unwrap_or("");
    let session_id = payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .or(Some("owner")); // Default to 'owner' if missing
    let platform = payload
        .get("platform")
        .and_then(|v| v.as_str())
        .unwrap_or("api");
    let chat_id = payload.get("chat_id").and_then(|v| v.as_str());

    let job_id = payload.get("job_id").and_then(|v| v.as_str());

    let user_time = payload.get("user_time").and_then(|v| v.as_str());
    let context =
        crate::agent::RunContext::new(task, platform, session_id, chat_id, job_id, user_time, None);
    match crate::agent::run_task(task, &state, context).await {
        Ok(result) => Json(json!({"ok": true, "result": result})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn create_job(State(state): State<AppState>, Json(j): Json<Value>) -> Json<Value> {
    let name = j.get("name").and_then(|v| v.as_str()).unwrap_or("job");
    let task = j.get("task").and_then(|v| v.as_str()).unwrap_or("");
    let sched = j
        .get("schedule_nl")
        .and_then(|v| v.as_str())
        .unwrap_or("every day");
    match state
        .scheduler
        .create(
            name,
            task,
            sched,
            "dashboard",
            None,
            Some("dashboard"),
            None,
            None,
        )
        .await
    {
        Ok(job) => Json(json!({"ok":true, "id": job.id})),
        Err(e) => Json(json!({"ok":false, "error": e.to_string()})),
    }
}

pub async fn pause_job(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    match state.scheduler.pause(&id).await {
        Ok(_) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn resume_job(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    match state.scheduler.resume(&id).await {
        Ok(_) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn delete_job(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    match state.scheduler.delete(&id).await {
        Ok(_) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn get_mcp(State(state): State<AppState>) -> Json<Value> {
    let servers = state.mcp.server_names().await;
    let tools = state.mcp.all_tools().await;
    Json(json!({"servers": servers, "tools": tools}))
}

pub async fn connect_mcp(State(state): State<AppState>, Json(payload): Json<Value>) -> Json<Value> {
    let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let url = payload.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let raw_api_key = payload
        .get("api_key")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    if name.is_empty() || url.is_empty() {
        return Json(json!({"ok": false, "error": "Name and URL required"}));
    }
    match state.mcp.connect(name, url, raw_api_key.clone()).await {
        Ok(tools) => {
            for t in tools.clone() {
                state.tools.register(t).await;
            }
            let conn = state.db.get().unwrap();
            let id = Uuid::new_v4().to_string();
            let enc_key = raw_api_key.as_deref().map(crate::crypto::encrypt_key);
            let _ = conn.execute(
                "INSERT OR REPLACE INTO mcp_servers (id, name, url, api_key, status, last_ping) VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'))",
                rusqlite::params![id, name, url, enc_key, "connected"],
            );
            Json(json!({"ok": true, "tool_count": tools.len()}))
        }
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn disconnect_mcp(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Json<Value> {
    // Remove tools belonging to this MCP server from the ToolRegistry
    let all = state.tools.all().await;
    for t in all {
        if let crate::tools::schema::ToolSource::Mcp { server_name, .. } = &t.source {
            if server_name == &name {
                state.tools.remove(&t.name).await;
            }
        }
    }
    state.mcp.disconnect(&name).await;
    let conn = state.db.get().unwrap();
    let _ = conn.execute(
        "UPDATE mcp_servers SET status = 'disconnected' WHERE name = ?1",
        rusqlite::params![name],
    );
    Json(json!({"ok": true}))
}

/// Returns MCP tools with their full input schemas for the workflow editor.
/// Groups tools by service prefix (e.g., gsheets_*, gmail_*) for the node picker palette.
pub async fn get_mcp_tools(State(state): State<AppState>) -> Json<Value> {
    let tools = state.tools.all().await;
    let items: Vec<Value> = tools
        .iter()
        .filter(|t| t.name != "image_tool")
        .map(|t| {
            let (server, tool) = match &t.source {
                crate::tools::schema::ToolSource::Mcp {
                    server_name,
                    tool_name,
                } => (server_name.clone(), tool_name.clone()),
                crate::tools::schema::ToolSource::Internal => {
                    // Expose each internal tool as its own "server" so they
                    // appear as distinct, draggable nodes in the UI palette.
                    (t.name.clone(), t.name.clone())
                }
                _ => (t.name.clone(), t.name.clone()),
            };
            json!({
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
                "required": t.required,
                "server": server,
                "tool_name": tool,
            })
        })
        .collect();
    Json(json!({ "tools": items }))
}

pub async fn get_files(State(state): State<AppState>, Path(dir): Path<String>) -> Json<Value> {
    match state.files.list(&dir) {
        Ok(files) => Json(json!({"files": files})),
        Err(e) => Json(json!({"ok":false, "error": e.to_string()})),
    }
}

pub async fn delete_file(
    State(state): State<AppState>,
    Path((_dir, id)): Path<(String, String)>,
) -> Json<Value> {
    match state.files.delete(&id).await {
        Ok(_) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn delete_all_files(State(state): State<AppState>) -> Json<Value> {
    match state.files.delete_all(None).await {
        Ok(deleted) => Json(json!({"ok": true, "deleted": deleted})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn get_messaging_status(State(state): State<AppState>) -> Json<Value> {
    let status = state.messaging.get_status().await;
    Json(status)
}

pub async fn reconnect_messaging(
    State(state): State<AppState>,
    Path(platform): Path<String>,
) -> Json<Value> {
    let s2 = Arc::new(state.clone());
    match platform.as_str() {
        "telegram" => {
            let token = state.settings.get_str("messaging.telegram_token", "");
            if !token.is_empty() {
                let gw = Arc::new(crate::messaging::TelegramGateway::new(token));
                {
                    let mut lock = state.messaging.telegram.lock().await;
                    *lock = Some(Arc::clone(&gw));
                }
                tokio::spawn(async move { gw.start_polling(s2).await });
            }
        }
        "discord" => {
            let token = state.settings.get_str("messaging.discord_token", "");
            if !token.is_empty() {
                let gw = Arc::new(crate::messaging::DiscordGateway::new(token));
                {
                    let mut lock = state.messaging.discord.lock().await;
                    *lock = Some(Arc::clone(&gw));
                }
                tokio::spawn(async move { gw.start_gateway(s2).await });
            }
        }
        "slack" => {
            let token = state.settings.get_str("messaging.slack_token", "");
            if !token.is_empty() {
                let gw = Arc::new(crate::messaging::SlackGateway::new(token));
                {
                    let mut lock = state.messaging.slack.lock().await;
                    *lock = Some(Arc::clone(&gw));
                }
            }
        }
        _ => return Json(json!({"ok": false, "error": "Unknown platform"})),
    }
    Json(json!({"ok": true}))
}
pub async fn slack_events(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    if let Some(challenge) = payload.get("challenge").and_then(|v| v.as_str()) {
        return Json(json!({ "challenge": challenge }));
    }

    let state2 = state.clone();
    tokio::spawn(async move {
        let slack_opt = {
            let lock = state2.messaging.slack.lock().await;
            lock.clone()
        };
        if let Some(slack) = slack_opt {
            let _ = slack.handle_event(payload, Arc::new(state2)).await;
        }
    });

    Json(json!({ "ok": true }))
}

pub async fn download_file(
    Query(params): Query<HashMap<String, String>>,
) -> impl axum::response::IntoResponse {
    use axum::http::header;
    use axum::response::IntoResponse;
    use std::path::PathBuf;

    let path_str = params.get("path").cloned().unwrap_or_default();
    if path_str.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            "Missing path parameter".to_string(),
        )
            .into_response();
    }

    // Security: only allow downloads from the staging directory
    if !crate::files::is_valid_staged_path(&path_str) {
        return (
            axum::http::StatusCode::FORBIDDEN,
            "Access denied: file is not in the staging directory".to_string(),
        )
            .into_response();
    }

    let path = PathBuf::from(&path_str);
    if let Ok(bytes) = std::fs::read(&path) {
        let filename = path.file_name().unwrap_or_default().to_string_lossy();
        let mime = mime_guess::from_path(&path)
            .first_or_octet_stream()
            .to_string();
        (
            [
                (header::CONTENT_TYPE, mime),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{}\"", filename),
                ),
            ],
            bytes,
        )
            .into_response()
    } else {
        (
            axum::http::StatusCode::NOT_FOUND,
            "File not found or unreadable".to_string(),
        )
            .into_response()
    }
}

pub async fn upload_file(
    State(state): State<AppState>,
    mut multipart: axum::extract::Multipart,
) -> Json<Value> {
    let mut saved_files = vec![];
    while let Ok(Some(field)) = multipart.next_field().await {
        let filename = field.file_name().unwrap_or("upload.bin").to_string();

        if let Ok(bytes) = field.bytes().await {
            let size = bytes.len();
            let mime = mime_guess::from_path(&filename)
                .first_or_octet_stream()
                .to_string();

            let agent_file = crate::tools::file_handler::AgentFile {
                id: "".to_string(), // Handler generates this from hash
                filename: filename.clone(),
                mime_type: mime.clone(),
                size_bytes: size,
                bytes: bytes.to_vec(),
                platform: Some("dashboard".to_string()),
                chat_id: None,
            };

            match state.files.store_incoming(agent_file).await {
                Ok((id, _path)) => {
                    saved_files.push(json!({
                        "id": id,
                        "original_name": filename,
                        "mime_type": mime,
                        "size": size
                    }));
                }
                Err(e) => {
                    tracing::error!("Failed to store uploaded file: {}", e);
                }
            }
        }
    }
    Json(json!({ "ok": true, "files": saved_files }))
}

// ── Watcher (Smart Notifications) ─────────────────────────────────────────────

pub async fn get_watchers(State(state): State<AppState>) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let mut s = conn
            .prepare("SELECT id, service, tool_name, tool_args, label, enabled, poll_mins, last_check, last_seen_ids, created_at, trigger_condition FROM watchers ORDER BY created_at")
            .unwrap();
        let watchers: Vec<Value> = s
            .query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "service": r.get::<_, String>(1)?,
                    "tool_name": r.get::<_, String>(2)?,
                    "tool_args": r.get::<_, String>(3)?,
                    "label": r.get::<_, String>(4)?,
                    "enabled": r.get::<_, i32>(5)? != 0,
                    "poll_mins": r.get::<_, f64>(6)?,
                    "last_check": r.get::<_, Option<String>>(7)?,
                    "last_seen_count": serde_json::from_str::<Vec<String>>(
                        &r.get::<_, String>(8).unwrap_or_else(|_| "[]".to_string())
                    ).map(|v| v.len()).unwrap_or(0),
                    "created_at": r.get::<_, String>(9)?,
                    "trigger_condition": r.get::<_, String>(10).unwrap_or_else(|_| "on_change".to_string()),
                }))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        return Json(json!({ "watchers": watchers }));
    }
    Json(json!({ "watchers": [] }))
}

pub async fn upsert_watcher(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let service = payload
        .get("service")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tool_name = payload
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tool_args = payload
        .get("tool_args")
        .and_then(|v| v.as_str())
        .unwrap_or("{}");
    let label = payload.get("label").and_then(|v| v.as_str()).unwrap_or("");
    let poll_mins = payload
        .get("poll_mins")
        .and_then(|v| v.as_f64())
        .unwrap_or(5.0);
    let enabled = payload
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let trigger_condition = payload
        .get("trigger_condition")
        .and_then(|v| v.as_str())
        .unwrap_or("on_change");

    // For custom or command watchers, tool_name/command is required
    if (service == "custom" || service == "command") && tool_name.is_empty() {
        return Json(
            json!({"ok": false, "error": if service == "command" { "Command text is required" } else { "Custom watchers require a tool_name" }}),
        );
    }

    if service.is_empty() {
        return Json(
            json!({"ok": false, "error": "Service is required (gmail, outlook, facebook, gcal, mscal, command, or custom)"}),
        );
    }

    if let Ok(conn) = state.db.get() {
        let id = payload
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        let _ = conn.execute(
            "INSERT INTO watchers (id, service, tool_name, tool_args, label, enabled, poll_mins, trigger_condition) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
             ON CONFLICT(id) DO UPDATE SET service=excluded.service, tool_name=excluded.tool_name, \
             tool_args=excluded.tool_args, label=excluded.label, enabled=excluded.enabled, poll_mins=excluded.poll_mins, \
             trigger_condition=excluded.trigger_condition",
            rusqlite::params![id, service, tool_name, tool_args, label, if enabled { 1 } else { 0 }, poll_mins, trigger_condition],
        );
        return Json(json!({"ok": true, "id": id}));
    }
    Json(json!({"ok": false, "error": "DB error"}))
}

pub async fn toggle_watcher(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let enabled = payload
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "UPDATE watchers SET enabled = ?1 WHERE id = ?2",
            rusqlite::params![if enabled { 1 } else { 0 }, id],
        );
    }
    Json(json!({"ok": true}))
}

pub async fn delete_watcher(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute("DELETE FROM watchers WHERE id = ?1", rusqlite::params![id]);
        let _ = conn.execute(
            "DELETE FROM watcher_log WHERE watcher_id = ?1",
            rusqlite::params![id],
        );
    }
    Json(json!({"ok": true}))
}

pub async fn run_watcher(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    // Load watcher config from DB
    let watcher = match state.db.get() {
        Ok(conn) => {
            match conn.query_row(
                "SELECT id, service, tool_name, tool_args, label, enabled, poll_mins, last_check, last_seen_ids, trigger_condition FROM watchers WHERE id = ?1",
                rusqlite::params![&id],
                |r| {
                    let last_seen_json: String = r.get(8).unwrap_or_else(|_| "[]".to_string());
                    let last_seen_ids: Vec<String> = serde_json::from_str(&last_seen_json).unwrap_or_default();
                    let trigger_condition: String = r.get::<_, String>(9).unwrap_or_else(|_| "on_change".to_string());
                    Ok(crate::watcher::engine::WatcherConfig {
                        id: r.get(0)?,
                        service: r.get(1)?,
                        tool_name: r.get(2)?,
                        tool_args: r.get(3)?,
                        label: r.get(4)?,
                        enabled: r.get::<_, i32>(5)? != 0,
                        poll_mins: r.get(6)?,
                        last_check: r.get(7).ok(),
                        last_seen_ids,
                        trigger_condition,
                    })
                },
            ) {
                Ok(w) => w,
                Err(e) => {
                    return Json(json!({"ok": false, "error": format!("Watcher not found: {}", e) }));
                }
            }
        }
        Err(e) => {
            return Json(json!({"ok": false, "error": format!("DB error: {}", e) }));
        }
    };

    if !watcher.enabled {
        return Json(json!({"ok": false, "error": "Watcher is disabled" }));
    }

    // Execute the watcher immediately using the engine
    let engine = crate::watcher::engine::WatcherEngine::new(
        Arc::clone(&state.db),
        Arc::clone(&state.router),
        Arc::clone(&state.settings),
        Arc::clone(&state.messaging),
        Arc::clone(&state.memory),
        state.tools.clone(),
    );

    // Run the watcher poll directly, ignoring last_seen_ids so UI gets full feedback
    let items = engine.poll_service(&watcher, true).await;

    let new_count = items.len();
    let now = chrono::Utc::now();

    // Update last_check
    let _ = engine.update_last_check(&watcher.id, &now, &items);

    // Log the poll
    let _ = engine.log_poll(&watcher.id, new_count);

    // If items found and not first poll, send notification
    let is_first_poll = watcher.last_seen_ids.is_empty() && watcher.last_check.is_none();
    if new_count > 0 && !is_first_poll {
        engine.triage_and_notify(items.clone()).await;
    }

    let summaries: Vec<String> = items.into_iter().map(|i| i.summary).collect();

    Json(json!({
        "ok": true,
        "items_found": new_count,
        "is_first_poll": is_first_poll,
        "items": summaries,
        "message": if is_first_poll {
            format!("First poll - stored {} baseline items", new_count)
        } else if new_count == 0 {
            "No new items found".to_string()
        } else {
            format!("Found {} new items", new_count)
        }
    }))
}

pub async fn get_watcher_log(State(state): State<AppState>) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let mut s = conn
            .prepare("SELECT wl.id, wl.watcher_id, w.service, w.label, wl.new_count, wl.created_at FROM watcher_log wl LEFT JOIN watchers w ON w.id = wl.watcher_id ORDER BY wl.created_at DESC LIMIT 100")
            .unwrap();
        let logs: Vec<Value> = s
            .query_map([], |r| {
                let srv: Option<String> = r.get(2)?;
                let lbl: Option<String> = r.get(3)?;
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "watcher_id": r.get::<_, String>(1)?,
                    "service": srv.clone(),
                    "label": lbl.unwrap_or(srv.unwrap_or_else(|| "Unknown".to_string())),
                    "new_count": r.get::<_, i32>(4)?,
                    "created_at": r.get::<_, String>(5)?,
                }))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        return Json(json!({ "log": logs }));
    }
    Json(json!({ "log": [] }))
}

// ── SSH Servers ───────────────────────────────────────────────────────────────

pub async fn get_ssh_servers(State(state): State<AppState>) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        // Exclude passwords/private keys in the GET response for security
        let mut s = conn.prepare("SELECT id, name, ip, port, username, auth_type, created_at FROM ssh_servers ORDER BY name").unwrap();
        let servers: Vec<Value> = s
            .query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "ip": r.get::<_, String>(2)?,
                    "port": r.get::<_, i64>(3)?,
                    "username": r.get::<_, String>(4)?,
                    "auth_type": r.get::<_, String>(5)?,
                    "created_at": r.get::<_, String>(6)?,
                }))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        return Json(json!({"servers": servers}));
    }
    Json(json!({"servers": []}))
}

pub async fn add_ssh_server(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let ip = payload.get("ip").and_then(|v| v.as_str()).unwrap_or("");
    let port = payload.get("port").and_then(|v| v.as_i64()).unwrap_or(22);
    let username = payload
        .get("username")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let auth_type = payload
        .get("auth_type")
        .and_then(|v| v.as_str())
        .unwrap_or("key");
    let password = payload.get("password").and_then(|v| v.as_str());
    let private_key = payload.get("private_key").and_then(|v| v.as_str());
    let public_key = payload.get("public_key").and_then(|v| v.as_str());

    if name.is_empty() || ip.is_empty() || username.is_empty() {
        return Json(json!({"ok": false, "error": "Name, IP, and Username are required"}));
    }

    if let Ok(conn) = state.db.get() {
        let enc_pass = password.map(|p| {
            if p.is_empty() {
                String::new()
            } else {
                crate::crypto::encrypt_key(p)
            }
        });
        let enc_priv = private_key.map(|p| {
            if p.is_empty() {
                String::new()
            } else {
                crate::crypto::encrypt_key(p)
            }
        });

        let _ = conn.execute(
            "INSERT INTO ssh_servers (name, ip, port, username, auth_type, password, private_key, public_key) 
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(name) DO UPDATE SET 
             ip=excluded.ip, port=excluded.port, username=excluded.username, auth_type=excluded.auth_type, 
             password=COALESCE(excluded.password, password), private_key=COALESCE(excluded.private_key, private_key), public_key=COALESCE(excluded.public_key, public_key)",
            rusqlite::params![name, ip, port, username, auth_type, enc_pass, enc_priv, public_key],
        );
        return Json(json!({"ok": true}));
    }
    Json(json!({"ok": false, "error": "DB error"}))
}

pub async fn delete_ssh_server(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "DELETE FROM ssh_servers WHERE name=?1",
            rusqlite::params![name],
        );
        return Json(json!({"ok": true}));
    }
    Json(json!({"ok": false, "error": "DB error"}))
}

// ── Web Search Tavily Accounts ───────────────────────────────────────────────────

pub async fn get_websearch_accounts(State(state): State<AppState>) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let mut s = conn
            .prepare("SELECT id, name, api_key, queries_this_month, enabled, priority FROM web_search_accounts ORDER BY priority, name")
            .unwrap();
        let accounts: Vec<Value> = s
            .query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "api_key_preview": format!("{}... seed={}", &r.get::<_, String>(2)?.chars().take(8).collect::<String>(), r.get::<_, String>(0)?),
                    "queries_this_month": r.get::<_, i64>(3)?,
                    "enabled": r.get::<_, i64>(4)? != 0,
                    "priority": r.get::<_, i64>(5)?,
                }))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        return Json(json!({ "accounts": accounts }));
    }
    Json(json!({ "accounts": [] }))
}

pub async fn upsert_websearch_account(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let id = payload
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let api_key = payload
        .get("api_key")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let priority = payload
        .get("priority")
        .and_then(|v| v.as_i64())
        .unwrap_or(1);
    let enabled = payload
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    if name.is_empty() || api_key.is_empty() {
        return Json(json!({"ok": false, "error": "Name and Tavily API Key are required"}));
    }

    if let Ok(conn) = state.db.get() {
        // Check if this is an update (existing id) and api_key is masked
        let existing_key: Option<String> = conn
            .query_row(
                "SELECT api_key FROM web_search_accounts WHERE id = ?1",
                rusqlite::params![id],
                |r| r.get(0),
            )
            .ok();

        let final_api_key = if api_key.contains("...") && existing_key.is_some() {
            // Keep existing key if masked
            existing_key.unwrap()
        } else {
            api_key.to_string()
        };

        if let Err(e) = conn.execute(
            "INSERT INTO web_search_accounts (id, name, api_key, enabled, priority)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
             name=excluded.name,
             api_key=COALESCE(NULLIF(excluded.api_key, ''), web_search_accounts.api_key),
             enabled=excluded.enabled,
             priority=excluded.priority",
            rusqlite::params![
                id,
                name,
                final_api_key,
                if enabled { 1 } else { 0 },
                priority
            ],
        ) {
            return Json(json!({"ok": false, "error": format!("DB Insert error: {}", e)}));
        }
        return Json(json!({"ok": true, "id": id}));
    }
    Json(json!({"ok": false, "error": "DB error"}))
}

pub async fn delete_websearch_account(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "DELETE FROM web_search_accounts WHERE id = ?1",
            rusqlite::params![id],
        );
        return Json(json!({"ok": true}));
    }
    Json(json!({"ok": false, "error": "DB error"}))
}

pub async fn reset_websearch_quotas(State(state): State<AppState>) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "UPDATE web_search_accounts SET queries_this_month = 0, enabled = 1",
            [],
        );
        return Json(json!({"ok": true}));
    }
    Json(json!({"ok": false, "error": "DB error"}))
}

// ── Saved Synapses ───────────────────────────────────────────────────────

pub async fn get_synapses(State(state): State<AppState>) -> Json<Value> {
    let conn = match state.db.get() {
        Ok(c) => c,
        Err(e) => return Json(json!({"error": e.to_string()})),
    };
    let mut stmt = match conn.prepare("SELECT id, name, method, url, headers, body, \"limit\", created_at, proxy, next_request_id FROM http_requests ORDER BY created_at DESC") {
        Ok(s) => s,
        Err(e) => return Json(json!({"error": e.to_string()})),
    };
    let rows: Vec<Value> = stmt.query_map([], |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "name": r.get::<_, String>(1)?,
            "method": r.get::<_, String>(2)?,
            "url": r.get::<_, String>(3)?,
            "headers": serde_json::from_str::<Value>(&r.get::<_, String>(4)?).unwrap_or(json!({})),
            "body": r.get::<_, String>(5)?,
            "limit": r.get::<_, Option<i64>>(6)?,
            "created_at": r.get::<_, String>(7)?,
            "proxy": r.get::<_, Option<String>>(8)?,
            "next_request_id": r.get::<_, Option<String>>(9)?,
        }))
    }).unwrap().filter_map(|r| r.ok()).collect();
    Json(json!({ "requests": rows }))
}

pub async fn upsert_synapse(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let conn = state.db.get().unwrap();
    let id = payload
        .get("id")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let name = payload
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled Synapse");
    let method = payload
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET");
    let url = payload.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let headers = payload
        .get("headers")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "{}".into());
    let body = payload.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let limit = payload.get("limit").and_then(|v| v.as_i64());
    let proxy = payload.get("proxy").and_then(|v| v.as_str());
    let next_id = payload.get("next_request_id").and_then(|v| v.as_str());

    let res = conn.execute(
        "INSERT INTO http_requests (id, name, method, url, headers, body, \"limit\", proxy, next_request_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(id) DO UPDATE SET 
            name=excluded.name, method=excluded.method, url=excluded.url, 
            headers=excluded.headers, body=excluded.body, \"limit\"=excluded.\"limit\",
            proxy=excluded.proxy, next_request_id=excluded.next_request_id",
        rusqlite::params![id, name, method, url, headers, body, limit, proxy, next_id],
    );

    match res {
        Ok(_) => Json(json!({"ok": true, "id": id})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn delete_synapse(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    let conn = state.db.get().unwrap();
    match conn.execute("DELETE FROM http_requests WHERE id = ?1", [id]) {
        Ok(_) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn run_saved_synapse(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<Value> {
    let conn = state.db.get().unwrap();
    let req = conn.query_row(
        "SELECT method, url, headers, body, \"limit\", proxy, next_request_id FROM http_requests WHERE id = ?1",
        [id],
        |r| {
            let method: String = r.get(0)?;
            let url: String = r.get(1)?;
            let headers_json: String = r.get(2)?;
            let body_str: String = r.get(3)?;
            let limit: Option<i64> = r.get(4)?;
            let proxy: Option<String> = r.get(5)?;
            let next_request_id: Option<String> = r.get(6)?;

            let body_json = if body_str.trim().starts_with('{') || body_str.trim().starts_with('[') {
                serde_json::from_str(&body_str).unwrap_or(json!(body_str))
            } else {
                json!(body_str)
            };

            Ok((crate::tools::http::HttpRequestParams {
                method,
                url,
                headers: Some(serde_json::from_str(&headers_json).unwrap_or(json!({}))),
                query: None,
                body: Some(body_json),
                auth: None,
                timeout_seconds: Some(30),
                response_format: None,
                limit: limit.map(|n| n as usize),
                proxy,
                send_binary_data: None,
                binary_property: None,
                body_content_type: None,
                stealth_headers: None,
                raw_content_type: None,
                allow_unauthorized_certs: None,
                full_response: None,
                data_cleaner: None,
                always_output_binary: None,
                json_body: None,
                specify_body: None,
                header_parameters: None,
                ..Default::default()
            }, next_request_id))

        }
    );

    let (params, next_request_id) = match req {
        Ok(p) => p,
        Err(e) => return Json(json!({"ok": false, "error": format!("Synapse not found: {}", e)})),
    };

    let tool = crate::tools::http::HttpRequestTool::new();
    match tool.request(params).await {
        Ok(resp) => {
            // Register in DB if download
            let val = serde_json::to_value(&resp).unwrap_or(json!({}));
            state
                .files
                .register_from_json(&val, Some("synapse".to_string()))
                .await;
            Json(json!({"ok": true, "result": resp, "next_request_id": next_request_id}))
        }
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn run_synapse_adhoc(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let method = payload
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("GET")
        .to_string();
    let url = payload
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let headers = payload.get("headers").cloned();
    let body_str = payload.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let body = if body_str.trim().starts_with('{') || body_str.trim().starts_with('[') {
        serde_json::from_str(body_str).unwrap_or(json!(body_str))
    } else {
        json!(body_str)
    };

    if url.is_empty() {
        return Json(json!({"ok": false, "error": "URL is required"}));
    }

    let params = crate::tools::http::HttpRequestParams {
        method,
        url,
        headers,
        query: None,
        body: Some(body),
        auth: None,
        timeout_seconds: Some(30),
        response_format: None,
        limit: payload
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize),
        proxy: payload
            .get("proxy")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        send_binary_data: None,
        binary_property: None,
        body_content_type: None,
        stealth_headers: None,
        raw_content_type: None,
        always_output_binary: None,

        allow_unauthorized_certs: payload
            .get("allowUnauthorizedCerts")
            .and_then(|v| v.as_bool()),
        full_response: payload.get("fullResponse").and_then(|v| v.as_bool()),
        data_cleaner: payload
            .get("options")
            .and_then(|o| o.get("dataCleaner"))
            .and_then(|v| v.as_bool())
            .or_else(|| payload.get("dataCleaner").and_then(|v| v.as_bool())),
        json_body: None,
        specify_body: None,
        header_parameters: None,
        ..Default::default()
    };

    let tool = crate::tools::http::HttpRequestTool::new();
    match tool.request(params).await {
        Ok(resp) => {
            let val = serde_json::to_value(&resp).unwrap_or(json!({}));
            state
                .files
                .register_from_json(&val, Some("synapse_adhoc".to_string()))
                .await;
            Json(json!({"ok": true, "result": resp}))
        }
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

// ── Workflow API ──────────────────────────────────────────────────────────────

pub async fn get_workflows(State(state): State<AppState>) -> Json<Value> {
    let conn = match state.db.get() {
        Ok(c) => c,
        Err(_) => return Json(json!({"workflows": []})),
    };
    let mut stmt = match conn.prepare(
        // Most-recently added or edited workflow first: updated_at is bumped on
        // every save; COALESCE falls back to created_at for rows predating it.
        "SELECT id, name, description, enabled, trigger_type, trigger_config, last_run_at, last_status, created_at FROM workflows ORDER BY COALESCE(updated_at, created_at) DESC, created_at DESC"
    ) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Failed to prepare workflows query: {}", e);
            return Json(json!({"workflows": []}));
        }
    };
    let rows = match stmt.query_map([], |r| {
        let wf_id: String = r.get(0)?;
        Ok((wf_id, json!({
            "id": r.get::<_, String>(0)?,
            "name": r.get::<_, String>(1)?,
            "description": r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            "enabled": r.get::<_, i64>(3)? != 0,
            "trigger_type": r.get::<_, Option<String>>(4)?.unwrap_or_else(|| "manual".to_string()),
            "trigger_config": serde_json::from_str::<Value>(&r.get::<_, Option<String>>(5)?.unwrap_or_else(|| "{}".to_string())).unwrap_or(json!({})),
            "last_run_at": r.get::<_, Option<String>>(6)?,
            "last_status": r.get::<_, Option<String>>(7)?.unwrap_or_else(|| "idle".to_string()),
            "created_at": r.get::<_, Option<String>>(8)?.unwrap_or_default(),
        })))
    }) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Failed to query workflows: {}", e);
            return Json(json!({"workflows": []}));
        }
    };
    let workflows: Vec<Value> = rows.filter_map(|r| r.ok()).map(|(wf_id, mut wf)| {
        // Load nodes for this workflow
        if let Ok(mut nstmt) = conn.prepare(
            "SELECT id, workflow_id, position, node_type, name, config, enabled, position_x, position_y, continue_on_fail, retries, retry_wait_ms, retry_backoff, pinned_data FROM workflow_nodes WHERE workflow_id = ?1 ORDER BY position ASC"
        ) {
            let nodes: Vec<Value> = nstmt.query_map(rusqlite::params![wf_id], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "workflow_id": r.get::<_, String>(1)?,
                    "position": r.get::<_, i64>(2)?,
                    "node_type": r.get::<_, String>(3)?,
                    "name": r.get::<_, String>(4)?,
                    "config": serde_json::from_str::<Value>(&r.get::<_, Option<String>>(5)?.unwrap_or_else(|| "{}".to_string())).unwrap_or(json!({})),
                    "enabled": r.get::<_, i64>(6)? != 0,
                    "position_x": r.get::<_, f64>(7)?,
                    "position_y": r.get::<_, f64>(8)?,
                    "continue_on_fail": r.get::<_, i64>(9)? != 0,
                    "retries": r.get::<_, i64>(10).unwrap_or(0),
                    "retry_wait_ms": r.get::<_, i64>(11).unwrap_or(0),
                    "retry_backoff": r.get::<_, Option<String>>(12)?.unwrap_or_else(|| "fixed".to_string()),
                    // Pinned output (A4): parsed JSON value, or null when not pinned.
                    "pinned_data": r.get::<_, Option<String>>(13)?
                        .filter(|s| !s.trim().is_empty())
                        .and_then(|s| serde_json::from_str::<Value>(&s).ok()),
                }))
            }).unwrap().filter_map(|r| r.ok()).collect();
            wf.as_object_mut().unwrap().insert("nodes".to_string(), json!(nodes));
        }

        // Load edges for this workflow
        if let Ok(mut estmt) = conn.prepare(
            "SELECT id, workflow_id, source_id, target_id, source_handle, target_handle FROM workflow_edges WHERE workflow_id = ?1"
        ) {
            let edges: Vec<Value> = estmt.query_map(rusqlite::params![wf_id], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "workflow_id": r.get::<_, String>(1)?,
                    "source_id": r.get::<_, String>(2)?,
                    "target_id": r.get::<_, String>(3)?,
                    "source_handle": r.get::<_, Option<String>>(4)?,
                    "target_handle": r.get::<_, Option<String>>(5)?,
                }))
            }).unwrap().filter_map(|r| r.ok()).collect();
            wf.as_object_mut().unwrap().insert("edges".to_string(), json!(edges));
        }

        wf
    }).collect();
    Json(json!({"workflows": workflows}))
}

pub async fn upsert_workflow(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let id = payload
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let name = payload
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled");
    let description = payload
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let enabled = payload
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let trigger_type = payload
        .get("trigger_type")
        .and_then(|v| v.as_str())
        .unwrap_or("manual");

    let mut trigger_config_val = payload
        .get("trigger_config")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    if trigger_type == "cron" || trigger_type == "watcher" || trigger_type == "gmail" {
        if let Some(schedules) = trigger_config_val
            .get_mut("schedules")
            .and_then(|s| s.get_mut("parameters"))
            .and_then(|p| p.as_array_mut())
        {
            for param in schedules.iter_mut() {
                if let Some(cron_nl) = param.get("cron_nl").and_then(|v| v.as_str()) {
                    if param.get("cron").is_none() {
                        if let Ok(cron_expr) = crate::scheduler::nl_parser::parse_schedule(
                            cron_nl,
                            state.router.clone(),
                            &state.settings,
                        )
                        .await
                        {
                            if let Some(obj) = param.as_object_mut() {
                                obj.insert("cron".to_string(), serde_json::json!(cron_expr));
                            }
                        }
                    }
                }
            }
        }
    }
    let trigger_config = trigger_config_val.to_string();
    let nodes = payload.get("nodes").and_then(|v| v.as_array());
    let edges = payload.get("edges").and_then(|v| v.as_array());

    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            // Stamp updated_at on every save so the workflow list can order
            // most-recently added/edited first (see get_workflows ORDER BY).
            "INSERT INTO workflows (id, name, description, enabled, trigger_type, trigger_config, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))
             ON CONFLICT(id) DO UPDATE SET name=?2, description=?3, enabled=?4, trigger_type=?5, trigger_config=?6, updated_at=datetime('now')",
            rusqlite::params![id, name, description, enabled as i64, trigger_type, trigger_config],
        );

        // Replace all nodes for this workflow
        if let Some(nodes) = nodes {
            let _ = conn.execute(
                "DELETE FROM workflow_nodes WHERE workflow_id = ?1",
                rusqlite::params![id],
            );
            for (i, node) in nodes.iter().enumerate() {
                let node_id = node
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("{}_{}", id, i));
                let node_type = node
                    .get("node_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("http");
                let node_name = node.get("name").and_then(|v| v.as_str()).unwrap_or("Step");
                let config = node
                    .get("config")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "{}".to_string());
                let node_enabled = node
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let position_x = node
                    .get("position_x")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let position_y = node
                    .get("position_y")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let node_continue = node
                    .get("continue_on_fail")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                // Retry-on-fail config. Accept numbers arriving as JSON numbers or
                // strings (UI widgets emit both); clamp to sane bounds.
                let node_retries = node
                    .get("retries")
                    .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
                    .unwrap_or(0)
                    .clamp(0, 100);
                let node_retry_wait = node
                    .get("retry_wait_ms")
                    .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
                    .unwrap_or(0)
                    .max(0);
                let node_retry_backoff = node
                    .get("retry_backoff")
                    .and_then(|v| v.as_str())
                    .filter(|s| *s == "exponential")
                    .unwrap_or("fixed");

                let _ = conn.execute(
                    "INSERT INTO workflow_nodes (id, workflow_id, position, position_x, position_y, node_type, name, config, enabled, continue_on_fail, retries, retry_wait_ms, retry_backoff) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    rusqlite::params![node_id, id, i as i64, position_x, position_y, node_type, node_name, config, node_enabled as i64, node_continue as i64, node_retries, node_retry_wait, node_retry_backoff],
                );
            }
        }

        // Replace all edges for this workflow
        if let Some(edges) = edges {
            let _ = conn.execute(
                "DELETE FROM workflow_edges WHERE workflow_id = ?1",
                rusqlite::params![id],
            );
            for (i, edge) in edges.iter().enumerate() {
                let edge_id = edge
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("edge_{}_{}", id, i));

                let source_id = edge.get("source_id").and_then(|v| v.as_str()).unwrap_or("");
                let target_id = edge.get("target_id").and_then(|v| v.as_str()).unwrap_or("");
                let source_handle = edge.get("source_handle").and_then(|v| v.as_str());
                let target_handle = edge.get("target_handle").and_then(|v| v.as_str());

                if !source_id.is_empty() && !target_id.is_empty() {
                    let _ = conn.execute(
                        "INSERT INTO workflow_edges (id, workflow_id, source_id, target_id, source_handle, target_handle) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        rusqlite::params![edge_id, id, source_id, target_id, source_handle, target_handle],
                    );
                }
            }
        }

        Json(json!({"ok": true, "id": id}))
    } else {
        Json(json!({"ok": false, "error": "DB error"}))
    }
}

pub async fn delete_workflow(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "DELETE FROM workflow_nodes WHERE workflow_id = ?1",
            rusqlite::params![id],
        );
        let _ = conn.execute(
            "DELETE FROM workflow_edges WHERE workflow_id = ?1",
            rusqlite::params![id],
        );
        let _ = conn.execute(
            "DELETE FROM workflow_runs WHERE workflow_id = ?1",
            rusqlite::params![id],
        );
        let _ = conn.execute("DELETE FROM workflows WHERE id = ?1", rusqlite::params![id]);
        Json(json!({"ok": true}))
    } else {
        Json(json!({"ok": false, "error": "DB error"}))
    }
}

pub async fn run_workflow(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    match crate::tools::workflow::WorkflowEngine::run_in_background(&id, &state, None) {
        Ok(run_id) => Json(json!({"ok": true, "run_id": run_id})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn run_workflow_node(
    State(state): State<AppState>,
    Path((id, node_id)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<Value> {
    // ?single=true → run ONLY this node, reusing cached upstream results from the
    // last run (the "Execute Step" button when upstream nodes already have data).
    // Without it, the node plus all its ancestors are re-run (the play button).
    let single = matches!(params.get("single").map(String::as_str), Some("true" | "1"));
    match crate::tools::workflow::WorkflowEngine::run_node_in_background(
        &id, &state, node_id, single,
    ) {
        Ok(run_id) => Json(json!({"ok": true, "run_id": run_id})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn get_workflow_runs(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let mut stmt = conn.prepare(
            "SELECT id, workflow_id, status, trigger_type, started_at, finished_at, node_results FROM workflow_runs WHERE workflow_id = ?1 ORDER BY started_at DESC LIMIT 10"
        ).unwrap();
        let runs: Vec<Value> = stmt.query_map(rusqlite::params![id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "workflow_id": r.get::<_, String>(1)?,
                "status": r.get::<_, String>(2)?,
                "trigger_type": r.get::<_, Option<String>>(3)?,
                "started_at": r.get::<_, String>(4)?,
                "finished_at": r.get::<_, Option<String>>(5)?,
                "node_results": serde_json::from_str::<Value>(&r.get::<_, String>(6)?).unwrap_or(json!([])),
            }))
        }).unwrap().filter_map(|r| r.ok()).collect();
        Json(json!({"runs": runs}))
    } else {
        Json(json!({"runs": []}))
    }
}

/// Lightweight single-run poll endpoint — direct primary-key lookup.
/// Returns just this one run, avoiding the heavy ORDER BY + LIMIT 10 query
/// that get_workflow_runs performs.  Designed for fast frontend polling.
pub async fn get_workflow_run_by_id(
    State(state): State<AppState>,
    Path(run_id): Path<String>,
) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        match conn.query_row(
            "SELECT id, workflow_id, status, trigger_type, started_at, finished_at, node_results FROM workflow_runs WHERE id = ?1",
            rusqlite::params![run_id],
            |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "workflow_id": r.get::<_, String>(1)?,
                    "status": r.get::<_, String>(2)?,
                    "trigger_type": r.get::<_, Option<String>>(3)?,
                    "started_at": r.get::<_, String>(4)?,
                    "finished_at": r.get::<_, Option<String>>(5)?,
                    "node_results": serde_json::from_str::<Value>(&r.get::<_, String>(6)?).unwrap_or(json!([])),
                }))
            },
        ) {
            Ok(run) => Json(run),
            Err(_) => Json(json!({ "error": "Run not found" })),
        }
    } else {
        Json(json!({ "error": "DB error" }))
    }
}
pub async fn stop_workflow(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<Value> {
    // Prefer cancelling the specific run: precise, and it can't poison the
    // workflow or affect other concurrent/future runs. Fall back to the
    // workflow_id only when no run_id is supplied. Either entry is cleared when
    // the run finishes (see CancellationCleanup), so the set never accumulates
    // stale flags that would silently cancel later runs.
    let mut set = state.workflow_cancellations.lock().await;
    match params.get("run_id") {
        Some(run_id) if !run_id.is_empty() => {
            set.insert(run_id.clone());
        }
        _ => {
            set.insert(id);
        }
    }
    Json(json!({"ok": true}))
}

pub async fn get_credentials(State(state): State<AppState>) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let mut s = conn
            .prepare("SELECT id, name, service FROM credentials ORDER BY created_at")
            .unwrap();
        let creds: Vec<Value> = s
            .query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "service": r.get::<_, String>(2)?,
                    "has_data": true
                }))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        return Json(json!({ "credentials": creds }));
    }
    Json(json!({ "credentials": [] }))
}

pub async fn upsert_credential(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let id = payload
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| "");
    let name = payload
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Unnamed Credential");
    let service = payload
        .get("service")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let data = payload
        .get("data")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    let id = if id.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        id.to_string()
    };

    let data_str = serde_json::to_string(&data).unwrap_or_else(|_| "{}".to_string());

    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "INSERT OR REPLACE INTO credentials (id, name, service, data, created_at) VALUES (?1, ?2, ?3, ?4, datetime('now'))",
            rusqlite::params![id, name, service, data_str],
        );
        return Json(json!({"ok": true}));
    }
    Json(json!({"ok": false, "error": "DB Error"}))
}

pub async fn delete_credential(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "DELETE FROM credentials WHERE id = ?1",
            rusqlite::params![id],
        );
        return Json(json!({"ok": true}));
    }
    Json(json!({"ok": false, "error": "DB Error"}))
}

pub async fn telegram_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> impl axum::response::IntoResponse {
    let secret = headers
        .get("x-telegram-bot-api-secret-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // 1. Find all enabled telegram-trigger workflows (used for secret validation).
    let workflows = {
        let Ok(conn) = state.db.get() else {
            return axum::http::StatusCode::INTERNAL_SERVER_ERROR;
        };
        conn.prepare(
            "SELECT id, trigger_config FROM (
                SELECT DISTINCT w.id,
                    COALESCE(json_extract(wn.config, '$.type'), w.trigger_type) as trigger_type,
                    COALESCE(wn.config, w.trigger_config) as trigger_config
                FROM workflows w
                LEFT JOIN workflow_nodes wn ON wn.workflow_id = w.id AND wn.node_type IN ('trigger', 'circadian', 'stimulus')
                WHERE w.enabled = 1
            ) WHERE trigger_type = 'telegram'"
        )
        .and_then(|mut s| s.query_map([], |r| Ok((
            r.get::<_, String>(0)?,
            serde_json::from_str::<Value>(&r.get::<_, String>(1)?).unwrap_or(json!({}))
        ))).map(|i| i.filter_map(|r| r.ok()).collect::<Vec<_>>())).unwrap_or_default()
    };

    // 2. Validate the secret against any matching trigger and act on the routing result.
    //
    // Routing is encoded in the callback_data prefix (set when the button was built):
    //
    //   trig:<workflow_name>  (route_to_trigger = true)
    //     → Find the workflow whose name equals <workflow_name> and run it.
    //       The main AI agent is NOT invoked for this click.
    //
    //   agent:<instruction>   (route_to_trigger = false, default)
    //     → Pass <instruction> as a task to the main AI agent.
    //       No trigger workflow is fired.
    //
    //   <no prefix>  (plain message or legacy callback_data)
    //     → Existing behaviour: run the trigger's own workflow.
    //
    // We break out of the loop after the first workflow that accepts the request
    // so the update is processed exactly once.
    'secret_check: for (wf_id, config) in workflows {
        let res =
            crate::tools::telegram::handle_telegram_webhook(secret, payload.clone(), &config).await;

        match res {
            // Secret mismatch or filtered out — try the next registered trigger.
            crate::tools::telegram::TriggerResult::Rejected { reason } => {
                tracing::debug!("[TELEGRAM] Trigger {} rejected: {}", wf_id, reason);
                continue 'secret_check;
            }

            // ── Toggle ON (trig: prefix) ──────────────────────────────────────
            // handle_telegram_webhook has already stripped the "trig:" prefix from
            // callback_data, so data["/callback_query/data"] == the workflow name.
            //
            // If there is NO callback_data (plain message, photo, voice, etc.) the
            // update was not triggered by a button at all — route to the main agent
            // just like the agent: path below.  Trigger workflows only fire when a
            // button explicitly carries a trig: prefix.
            crate::tools::telegram::TriggerResult::AcceptedForTrigger(ref data) => {
                let workflow_name = data
                    .pointer("/callback_query/data")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                // ── No callback data → plain message → main agent ─────────────
                if workflow_name.is_empty() {
                    // Extract text from message or caption; fall back to a generic task.
                    let text = data
                        .pointer("/message/text")
                        .or_else(|| data.pointer("/message/caption"))
                        .or_else(|| data.pointer("/channel_post/text"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    let chat_id_str = data
                        .pointer("/message/chat/id")
                        .or_else(|| data.pointer("/channel_post/chat/id"))
                        .and_then(|v| v.as_i64())
                        .map(|id| id.to_string());

                    let session_id_str = data
                        .pointer("/message/from/id")
                        .or_else(|| data.pointer("/channel_post/from/id"))
                        .and_then(|v| v.as_i64())
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| "telegram".to_string());

                    let task = if text.is_empty() {
                        "Handle Telegram message".to_string()
                    } else {
                        text
                    };

                    tracing::info!(
                        "[TELEGRAM] plain message → main agent task '{}' (chat={})",
                        task,
                        chat_id_str.as_deref().unwrap_or("?")
                    );

                    let state_clone = state.clone();
                    let task_clone = task.clone();
                    tokio::spawn(async move {
                        let context = crate::agent::RunContext::new(
                            &task_clone,
                            "telegram",
                            Some(session_id_str.as_str()),
                            chat_id_str.as_deref(),
                            None,
                            None,
                            None,
                        );
                        if let Err(e) =
                            crate::agent::run_task(&task_clone, &state_clone, context).await
                        {
                            tracing::error!(
                                "[TELEGRAM] Agent task failed for plain message '{}': {}",
                                task_clone,
                                e
                            );
                        }
                    });

                    break 'secret_check;
                }

                // ── Has callback data → trig: button → resolve + set trigger data + run ──
                tracing::info!(
                    "[TELEGRAM] trig: button → resolving and running workflow '{}'",
                    workflow_name
                );

                let wf_name = workflow_name.clone();
                let state_clone = state.clone();

                // Build trigger data so the workflow's stimulus node has the full context.
                let trigger_payload = json!({
                    "trigger": "telegram",
                    "events": [{
                        "type": "callback_query",
                        "chat_id": data.pointer("/callback_query/message/chat/id")
                            .and_then(|v| v.as_i64())
                            .map(|id| id.to_string())
                            .unwrap_or_default(),
                        "data": wf_name,
                        "from": data.pointer("/callback_query/from").cloned().unwrap_or(json!({})),
                        "message": data.pointer("/callback_query/message").cloned().unwrap_or(json!({}))
                    }]
                });

                tokio::spawn(async move {
                    // Resolve name → ID (same helper used by polling path).
                    let Ok(conn) = state_clone.db.get() else {
                        tracing::error!("[TELEGRAM] trig: DB unavailable resolving '{}'", wf_name);
                        return;
                    };
                    let resolved: Option<String> = conn.prepare(
                        "SELECT id FROM workflows WHERE LOWER(name) = LOWER(?1) AND enabled = 1 LIMIT 1"
                    )
                    .and_then(|mut s| s.query_row(
                        rusqlite::params![wf_name],
                        |r| r.get::<_, String>(0),
                    ))
                    .ok();

                    let Some(wf_id) = resolved else {
                        tracing::error!(
                            "[TELEGRAM] trig: workflow named '{}' not found or not enabled",
                            wf_name
                        );
                        return;
                    };

                    crate::tools::workflow::WorkflowEngine::set_telegram_trigger_data(
                        wf_id.clone(),
                        trigger_payload,
                    )
                    .await;

                    if let Err(e) = crate::tools::workflow::WorkflowEngine::run_in_background(
                        &wf_id,
                        &state_clone,
                        None,
                    ) {
                        tracing::error!(
                            "[TELEGRAM] WorkflowEngine failed for '{}' (id={}): {}",
                            wf_name,
                            wf_id,
                            e
                        );
                    }
                });

                break 'secret_check;
            }

            // ── Toggle OFF (agent: prefix, default) ───────────────────────────
            // Send the raw callback_data straight to the main agent as the task.
            // The agent receives it exactly as the button author wrote it.
            crate::tools::telegram::TriggerResult::AcceptedForAgent(ref body) => {
                let task = body
                    .pointer("/callback_query/data")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let chat_id_str = body
                    .pointer("/callback_query/message/chat/id")
                    .and_then(|v| v.as_i64())
                    .map(|id| id.to_string());

                let session_id_str = body
                    .pointer("/callback_query/from/id")
                    .and_then(|v| v.as_i64())
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "telegram".to_string());

                tracing::info!(
                    "[TELEGRAM] agent: button → task '{}' (chat={})",
                    task,
                    chat_id_str.as_deref().unwrap_or("?")
                );

                let state_clone = state.clone();
                let task_clone = task.clone();
                tokio::spawn(async move {
                    let context = crate::agent::RunContext::new(
                        &task_clone,
                        "telegram",
                        Some(session_id_str.as_str()),
                        chat_id_str.as_deref(),
                        None,
                        None,
                        None,
                    );
                    if let Err(e) = crate::agent::run_task(&task_clone, &state_clone, context).await
                    {
                        tracing::error!("[TELEGRAM] Agent task failed for callback: {}", e);
                    }
                });

                break 'secret_check;
            }
        }
    }

    axum::http::StatusCode::OK
}

pub async fn whatsapp_webhook_verify(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl axum::response::IntoResponse {
    let hub_mode = query.get("hub.mode").map(|s| s.as_str()).unwrap_or("");
    let hub_challenge = query.get("hub.challenge").map(|s| s.as_str()).unwrap_or("");
    let hub_verify_token = query
        .get("hub.verify_token")
        .map(|s| s.as_str())
        .unwrap_or("");

    let workflows = {
        let Ok(conn) = state.db.get() else {
            return axum::response::Response::builder()
                .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                .body(axum::body::Body::empty())
                .unwrap();
        };
        conn.prepare(
            "SELECT id, trigger_config FROM (
                SELECT DISTINCT w.id, 
                    COALESCE(json_extract(wn.config, '$.type'), w.trigger_type) as trigger_type,
                    COALESCE(wn.config, w.trigger_config) as trigger_config
                FROM workflows w
                LEFT JOIN workflow_nodes wn ON wn.workflow_id = w.id AND wn.node_type IN ('trigger', 'circadian', 'stimulus')
                WHERE w.enabled = 1
            ) WHERE trigger_type = 'whatsapp'"
        )
            .and_then(|mut s| s.query_map([], |r| Ok((
                r.get::<_, String>(0)?,
                serde_json::from_str::<Value>(&r.get::<_, String>(1)?).unwrap_or(json!({}))
            ))).map(|i| i.filter_map(|r| r.ok()).collect::<Vec<_>>())).unwrap_or_default()
    };

    for (_wf_id, config) in workflows {
        let res = crate::tools::whatsapp::verify_whatsapp_webhook(
            hub_mode,
            hub_challenge,
            hub_verify_token,
            &config,
        );
        match res {
            crate::tools::whatsapp::WebhookVerifyResult::Challenge(challenge) => {
                return axum::response::Response::builder()
                    .status(axum::http::StatusCode::OK)
                    .body(axum::body::Body::from(challenge))
                    .unwrap();
            }
            crate::tools::whatsapp::WebhookVerifyResult::Forbidden { .. } => {}
        }
    }

    axum::response::Response::builder()
        .status(axum::http::StatusCode::FORBIDDEN)
        .body(axum::body::Body::empty())
        .unwrap()
}

pub async fn whatsapp_webhook_messages(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> impl axum::response::IntoResponse {
    let workflows = {
        let Ok(conn) = state.db.get() else {
            return axum::http::StatusCode::INTERNAL_SERVER_ERROR;
        };
        conn.prepare(
            "SELECT id, trigger_config FROM (
                SELECT DISTINCT w.id, 
                    COALESCE(json_extract(wn.config, '$.type'), w.trigger_type) as trigger_type,
                    COALESCE(wn.config, w.trigger_config) as trigger_config
                FROM workflows w
                LEFT JOIN workflow_nodes wn ON wn.workflow_id = w.id AND wn.node_type IN ('trigger', 'circadian', 'stimulus')
                WHERE w.enabled = 1
            ) WHERE trigger_type = 'whatsapp'"
        )
            .and_then(|mut s| s.query_map([], |r| Ok((
                r.get::<_, String>(0)?,
                serde_json::from_str::<Value>(&r.get::<_, String>(1)?).unwrap_or(json!({}))
            ))).map(|i| i.filter_map(|r| r.ok()).collect::<Vec<_>>())).unwrap_or_default()
    };

    for (wf_id, config) in workflows {
        let res = crate::tools::whatsapp::handle_whatsapp_webhook(payload.clone(), &config).await;
        if let crate::tools::whatsapp::TriggerResult::Accepted(data) = res {
            // Store the data for the stimulus node
            crate::tools::workflow::WorkflowEngine::set_whatsapp_trigger_data(
                wf_id.clone(),
                json!({ "trigger": "whatsapp", "events": data }),
            )
            .await;

            if let Err(e) =
                crate::tools::workflow::WorkflowEngine::run_in_background(&wf_id, &state, None)
            {
                tracing::error!("Failed to trigger background whatsapp workflow: {}", e);
            }
        }
    }

    axum::http::StatusCode::OK
}
