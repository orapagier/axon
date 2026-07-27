use super::*;

// ── Companion Devices ────────────────────────────────────────────────────────

pub async fn get_devices(State(state): State<AppState>) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        // Exclude bearer_token in the GET response for security
        let mut s = try_json!(conn.prepare(
            "SELECT id, name, kind, base_url, notes, created_at FROM devices ORDER BY name"
        ));
        let devices: Vec<Value> = try_json!(s.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?,
                "name": r.get::<_, String>(1)?,
                "kind": r.get::<_, String>(2)?,
                "base_url": r.get::<_, String>(3)?,
                "notes": r.get::<_, Option<String>>(4)?,
                "created_at": r.get::<_, String>(5)?,
            }))
        }))
        .filter_map(|r| r.ok())
        .collect();
        return Json(json!({"devices": devices}));
    }
    Json(json!({"devices": []}))
}

pub async fn add_device(State(state): State<AppState>, Json(payload): Json<Value>) -> Json<Value> {
    let name = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let kind = payload
        .get("kind")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("androidcompanion");
    let base_url = payload
        .get("base_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim_end_matches('/');
    let bearer_token = payload.get("bearer_token").and_then(|v| v.as_str());
    let notes = payload.get("notes").and_then(|v| v.as_str());

    if name.is_empty() || base_url.is_empty() {
        return Json(json!({"ok": false, "error": "Name and Base URL are required"}));
    }

    if let Ok(conn) = state.db.get() {
        // Same COALESCE trick as ssh_servers: encrypt_key("") == "" so a blank
        // token field on edit doesn't clobber the stored one.
        let enc_token = bearer_token.map(|t| {
            if t.is_empty() {
                String::new()
            } else {
                crate::crypto::encrypt_key(t)
            }
        });
        let res = conn.execute(
            "INSERT INTO devices (name, kind, base_url, bearer_token, notes)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(name) DO UPDATE SET
             kind=excluded.kind, base_url=excluded.base_url,
             bearer_token=COALESCE(excluded.bearer_token, bearer_token),
             notes=excluded.notes",
            rusqlite::params![name, kind, base_url, enc_token, notes],
        );
        return match res {
            Ok(_) => Json(json!({"ok": true})),
            Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
        };
    }
    Json(json!({"ok": false, "error": "DB error"}))
}

pub async fn delete_device(State(state): State<AppState>, Path(name): Path<String>) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute("DELETE FROM devices WHERE name=?1", rusqlite::params![name]);
        return Json(json!({"ok": true}));
    }
    Json(json!({"ok": false, "error": "DB error"}))
}

// Reachability + auth check for the dashboard's "Test connection" button.
pub async fn test_device(State(state): State<AppState>, Path(name): Path<String>) -> Json<Value> {
    match crate::tools::DeviceTool::test_connection(&name, state).await {
        Ok(v) => Json(v),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

// Server-side proxy of GET {base_url}/agent/tools — the frontend never sees the raw token.
pub async fn get_device_tools(State(state): State<AppState>, Path(name): Path<String>) -> Json<Value> {
    match crate::tools::DeviceTool::list_device_tools(&name, state).await {
        Ok(v) => Json(v),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}
