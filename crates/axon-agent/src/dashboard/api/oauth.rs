use super::*;

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
    let connect_creds =
        service == "facebook" && params.get("state").map(String::as_str) == Some("fbcred");

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
            // Encrypt the page access token blob at rest (read seams decrypt).
            let data_str = crate::crypto::encrypt_key(&data_str);
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
                    let mark = if subscribed {
                        "✅ webhooks active"
                    } else {
                        "⚠️ webhooks not subscribed"
                    };
                    saved.push(format!("{page_name} — {mark}"));
                }
                Err(e) => {
                    tracing::error!("FB connect: failed to save credential for {page_name}: {e}")
                }
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
