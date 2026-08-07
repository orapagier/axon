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

/// Gmail node "Connect account" button — returns the OAuth URL whose callback
/// saves the chosen Google account as a credential (state=gcred).
pub async fn get_google_connect_url(State(state): State<AppState>) -> Json<Value> {
    match state.tools.run("google_connect_url", json!({})).await {
        Ok(res) => Json(res),
        Err(e) => Json(json!({ "error": format!("Failed to get Google connect URL: {e}") })),
    }
}

/// Settings dashboard: read the current Facebook App credentials
/// (app_id/page_id/verify_token, plus whether app_secret is set — the secret
/// itself is never sent to the browser).
pub async fn get_facebook_app_credentials(State(state): State<AppState>) -> Json<Value> {
    match state
        .tools
        .run("facebook_get_app_credentials", json!({}))
        .await
    {
        Ok(res) => Json(res),
        Err(e) => Json(json!({ "error": format!("Failed to load Facebook app credentials: {e}") })),
    }
}

/// Settings dashboard: update app_id/app_secret/verify_token/page_id. An
/// empty/omitted `app_secret` leaves the stored secret untouched.
pub async fn update_facebook_app_credentials(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    match state
        .tools
        .run("facebook_set_app_credentials", payload)
        .await
    {
        Ok(res) => Json(res),
        Err(e) => Json(
            json!({ "ok": false, "error": format!("Failed to update Facebook app credentials: {e}") }),
        ),
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

    // The Google equivalent (the Gmail node's Connect button): saves the account
    // as its own credential and leaves the globally signed-in account alone.
    let connect_google = service == "google"
        && params.get("state").map(String::as_str) == Some(axon_google::auth::CONNECT_STATE);

    match (code, error) {
        (Some(code), _) if connect_creds => {
            return facebook_connect_callback(&state, &code).await;
        }
        (Some(code), _) if connect_google => {
            return google_connect_callback(&state, &code).await;
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

/// Run a connect-flow tool and surface the real failure reason.
///
/// `ToolRegistry::run` only returns `Err` when the tool could not be *invoked*;
/// a tool that ran and failed comes back as `Ok({error: true, message: …})`
/// (see `normalize_mcp_output`). Without unwrapping that, a failed OAuth
/// exchange reaches the caller as a success-shaped value whose fields are all
/// missing, and the user is shown "no data returned" instead of what Google or
/// Meta actually said.
async fn run_connect_tool(
    state: &AppState,
    tool: &str,
    code: &str,
) -> Result<serde_json::Value, String> {
    let value = state
        .tools
        .run(tool, json!({ "code": code }))
        .await
        .map_err(|e| e.to_string())?;
    if value
        .get("error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Err(value
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("the OAuth exchange failed")
            .to_string());
    }
    Ok(value)
}

/// Facebook "Connect a Page" callback: exchanges the OAuth code for every Page
/// the user manages and saves each as its own credential (service "facebook").
/// The credential id is derived from the Page id so reconnecting refreshes the
/// token in place instead of creating duplicates.
async fn facebook_connect_callback(state: &AppState, code: &str) -> axum::response::Html<String> {
    let result = run_connect_tool(state, "facebook_exchange_code_pages", code).await;

    let pages = match result {
        Ok(v) => v
            .get("pages")
            .and_then(|p| p.as_array())
            .cloned()
            .unwrap_or_default(),
        Err(e) => return connect_error_html(&e),
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

/// Google "Connect account" callback: exchanges the OAuth code for one extra
/// account and saves it as a credential (service "google") that any Gmail /
/// Calendar / Drive node can select. The globally signed-in account on the
/// Credentials page is untouched and stays the default for nodes that pick none.
///
/// The credential id is derived from the account's email so reconnecting — to
/// re-grant a scope or replace a revoked refresh token — updates that account in
/// place instead of piling up duplicates.
async fn google_connect_callback(state: &AppState, code: &str) -> axum::response::Html<String> {
    let account = match run_connect_tool(state, "google_exchange_code_account", code).await {
        Ok(v) => v,
        Err(e) => return connect_error_html(&e),
    };

    let field = |key: &str| account.get(key).and_then(|v| v.as_str()).unwrap_or("");
    let email = field("email");
    if email.is_empty() {
        return connect_error_html("Google did not return an email address for this account.");
    }
    let name = Some(field("name"))
        .filter(|s| !s.is_empty())
        .unwrap_or(email);

    let data = json!({
        "email": email,
        "name": name,
        "access_token": field("access_token"),
        "refresh_token": field("refresh_token"),
        "expires_at": account.get("expires_at").cloned().unwrap_or(Value::Null),
    });
    // Encrypt the token blob at rest (the read seams decrypt).
    let data_str = crate::crypto::encrypt_key(&data.to_string());

    let conn = match state.db.get() {
        Ok(c) => c,
        Err(_) => return connect_error_html("Database unavailable while saving the account."),
    };
    // The dropdown shows `name`, so label it with the address — that is what
    // tells two connected inboxes apart.
    if let Err(e) = conn.execute(
        "INSERT OR REPLACE INTO credentials (id, name, service, data, created_at)
         VALUES (?1, ?2, 'google', ?3, datetime('now'))",
        rusqlite::params![format!("google-{email}"), email, data_str],
    ) {
        tracing::error!("Google connect: failed to save credential for {email}: {e}");
        return connect_error_html(&format!("Failed to save the account: {e}"));
    }

    axum::response::Html(format!(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>Axon</title>
<style>body{{font-family:system-ui,sans-serif;display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;background:#f0fdf4}}
.card{{background:#fff;border-radius:16px;padding:48px;box-shadow:0 10px 25px rgba(0,0,0,.1);text-align:center;max-width:480px}}
h1{{color:#16a34a;margin:0 0 12px}}p{{color:#6b7280;margin:0 0 8px;line-height:1.5}}code{{color:#374151}}</style>
<script>setTimeout(()=>{{ window.close(); }}, 3500);</script></head>
<body><div class="card"><h1>✅ Google account connected</h1>
<p>Saved as a credential you can pick in any Gmail node:</p><p><code>{email}</code></p>
<p>This tab will close automatically...</p></div></body></html>"#
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
