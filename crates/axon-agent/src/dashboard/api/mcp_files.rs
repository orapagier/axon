use super::*;

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
            let conn = try_json!(state.db.get());
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
    let conn = try_json!(state.db.get());
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
                    // Stop the outgoing gateway's poll loop before swapping it out —
                    // otherwise it keeps calling getUpdates in the background forever,
                    // racing the new poller for the same bot token and occasionally
                    // both winning a cycle, which produces duplicate replies.
                    if let Some(old) = lock.take() {
                        old.stop_polling();
                    }
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
    // Streamed, not `std::fs::read`: staged files include tool-generated video
    // and archives with no size ceiling, and buffering one whole file per
    // concurrent download is how this endpoint OOMs the agent. `ReaderStream`
    // also keeps the blocking read off the async worker.
    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(_) => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                "File not found or unreadable".to_string(),
            )
                .into_response()
        }
    };
    let len = file.metadata().await.ok().map(|m| m.len());

    let filename = path.file_name().unwrap_or_default().to_string_lossy();
    let mime = mime_guess::from_path(&path)
        .first_or_octet_stream()
        .to_string();

    // Images are served inline so the chat renderer can display them in the
    // bubble; `attachment` would make the browser download the file even
    // when it is the src of an <img>. Everything else keeps `attachment`,
    // which also stops HTML or SVG from rendering as a same-origin document.
    let inline_ok = matches!(
        mime.as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/bmp"
    );
    let kind = if inline_ok { "inline" } else { "attachment" };
    let disposition = format!("{kind}; {}", content_disposition_filename(&filename));

    let mut headers = axum::http::HeaderMap::new();
    // Defence in depth on nosniff: these bytes come from tool output, so a
    // mislabelled file must never be sniffed into something executable in the
    // dashboard's origin. Content-Length keeps the body from being chunked,
    // which is what gives the client a download progress bar.
    let fields = [
        (header::CONTENT_TYPE, mime),
        (header::CONTENT_DISPOSITION, disposition),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
    ]
    .into_iter()
    .chain(len.map(|n| (header::CONTENT_LENGTH, n.to_string())));
    for (name, value) in fields {
        if let Ok(v) = axum::http::HeaderValue::from_str(&value) {
            headers.insert(name, v);
        }
    }

    let body = axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(file));
    (headers, body).into_response()
}

/// Render the `filename` part of a `Content-Disposition` header for `name`.
///
/// Two things the old `format!("filename=\"{name}\"")` got wrong: a `"` or
/// newline in the name broke out of the quoted string (staged names are
/// sanitized, but files also arrive here written directly by tools, which never
/// pass through `sanitize_filename`), and any non-ASCII name — an emoji, an
/// accent — is simply not representable in that form. Emit a scrubbed ASCII
/// fallback plus the RFC 5987 `filename*` form that modern browsers prefer.
fn content_disposition_filename(name: &str) -> String {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let ascii = if ascii.trim().is_empty() {
        "download".to_string()
    } else {
        ascii
    };
    format!(
        "filename=\"{}\"; filename*=UTF-8''{}",
        ascii,
        urlencoding::encode(name)
    )
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

            match state.files.store_file(agent_file, "incoming").await {
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

#[cfg(test)]
mod content_disposition_tests {
    use super::content_disposition_filename;

    // A quote in the name used to terminate the quoted-string early, letting
    // the rest of the filename be read as header parameters.
    #[test]
    fn quotes_and_newlines_cannot_escape_the_header() {
        let h = content_disposition_filename("evil\".txt");
        let ascii = h.split("; filename*").next().unwrap();
        assert_eq!(ascii, "filename=\"evil_.txt\"");
        assert!(!content_disposition_filename("a\r\nX-Evil: 1").contains('\n'));
    }

    #[test]
    fn non_ascii_survives_in_the_rfc5987_form() {
        let h = content_disposition_filename("résumé.pdf");
        assert!(h.starts_with("filename=\"r_sum_.pdf\""), "{h}");
        assert!(h.ends_with("filename*=UTF-8''r%C3%A9sum%C3%A9.pdf"), "{h}");
    }

    #[test]
    fn ordinary_names_are_left_alone() {
        assert!(content_disposition_filename("chart 1.png")
            .starts_with("filename=\"chart 1.png\""));
    }

    // An all-non-ascii name must not produce an empty quoted string.
    #[test]
    fn always_yields_an_ascii_fallback() {
        assert!(content_disposition_filename("日本語")
            .starts_with("filename=\"___\""));
    }
}
