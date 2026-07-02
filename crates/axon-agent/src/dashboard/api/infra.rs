use super::*;

// ── SSH Servers ───────────────────────────────────────────────────────────────

pub async fn get_ssh_servers(State(state): State<AppState>) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        // Exclude passwords/private keys in the GET response for security
        let mut s = try_json!(conn.prepare("SELECT id, name, ip, port, username, auth_type, created_at FROM ssh_servers ORDER BY name"));
        let servers: Vec<Value> = try_json!(s.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "name": r.get::<_, String>(1)?,
                "ip": r.get::<_, String>(2)?,
                "port": r.get::<_, i64>(3)?,
                "username": r.get::<_, String>(4)?,
                "auth_type": r.get::<_, String>(5)?,
                "created_at": r.get::<_, String>(6)?,
            }))
        }))
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
        let mut s = try_json!(conn
            .prepare("SELECT id, name, api_key, queries_this_month, enabled, priority FROM web_search_accounts ORDER BY priority, name"));
        let accounts: Vec<Value> = try_json!(s.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "api_key_preview": format!("{}... seed={}", &r.get::<_, String>(2)?.chars().take(8).collect::<String>(), r.get::<_, String>(0)?),
                "queries_this_month": r.get::<_, i64>(3)?,
                "enabled": r.get::<_, i64>(4)? != 0,
                "priority": r.get::<_, i64>(5)?,
            }))
        }))
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
    let rows: Vec<Value> = try_json!(stmt.query_map([], |r| {
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
    }))
    .filter_map(|r| r.ok())
    .collect();
    Json(json!({ "requests": rows }))
}

pub async fn upsert_synapse(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let conn = try_json!(state.db.get());
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
    let conn = try_json!(state.db.get());
    match conn.execute("DELETE FROM http_requests WHERE id = ?1", [id]) {
        Ok(_) => Json(json!({"ok": true})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

pub async fn run_saved_synapse(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<Value> {
    let conn = try_json!(state.db.get());
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
