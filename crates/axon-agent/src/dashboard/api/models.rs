use super::*;

pub async fn get_models(State(state): State<AppState>) -> Json<Value> {
    let models = crate::router::get_status(&state.router).await;
    Json(json!({"models": models}))
}

pub async fn reset_model(State(state): State<AppState>, Path(name): Path<String>) -> Json<Value> {
    let _ = crate::router::reset_model(&state.router, &name).await;
    Json(json!({"ok": true}))
}

/// Insert a new model row from a JSON payload. Shared by the `add_model` HTTP
/// handler and the Homeostasis workflow node so both encrypt/normalize
/// identically. Requires `name` + `provider`; `api_key` is encrypted at rest.
pub(crate) fn apply_add_model(conn: &rusqlite::Connection, m: &Value) -> Result<(), String> {
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
        return Err("Name and provider are required".into());
    }

    // origin='runtime' marks the row as dashboard/node-owned so the boot-time
    // models.toml sync never prunes or overwrites it.
    conn.execute(
        "INSERT INTO models (name, provider, model_id, api_key, base_url, timeout_secs, priority, max_tokens, role, enabled, origin) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, 'runtime')",
        rusqlite::params![name, provider, model_id, crate::crypto::encrypt_key(raw_api_key), base_url, timeout_secs, priority, max_tokens, role],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Apply a partial update: only keys present in `m` are written (the dashboard's
/// "change only what you send" behavior). An `api_key` that is blank/whitespace
/// is skipped so it can't wipe an existing key. Shared with the Homeostasis node.
pub(crate) fn apply_update_model(
    conn: &rusqlite::Connection,
    name: &str,
    m: &Value,
) -> Result<(), String> {
    if let Some(enabled) = m.get("enabled").and_then(|v| v.as_bool()) {
        conn.execute(
            "UPDATE models SET enabled=?1 WHERE name=?2",
            rusqlite::params![if enabled { 1 } else { 0 }, name],
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(priority) = m.get("priority").and_then(|v| v.as_i64()) {
        conn.execute(
            "UPDATE models SET priority=?1 WHERE name=?2",
            rusqlite::params![priority, name],
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(role) = m.get("role").and_then(|v| v.as_str()) {
        conn.execute(
            "UPDATE models SET role=?1 WHERE name=?2",
            rusqlite::params![role, name],
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(raw_api_key) = m.get("api_key").and_then(|v| v.as_str()) {
        if !raw_api_key.trim().is_empty() {
            conn.execute(
                "UPDATE models SET api_key=?1 WHERE name=?2",
                rusqlite::params![crate::crypto::encrypt_key(raw_api_key), name],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    if let Some(provider) = m.get("provider").and_then(|v| v.as_str()) {
        let provider = crate::providers::normalize_provider_name(provider);
        conn.execute(
            "UPDATE models SET provider=?1 WHERE name=?2",
            rusqlite::params![provider, name],
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(model_id) = m.get("model_id").and_then(|v| v.as_str()) {
        conn.execute(
            "UPDATE models SET model_id=?1 WHERE name=?2",
            rusqlite::params![model_id, name],
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(base_url) = m.get("base_url").and_then(|v| v.as_str()) {
        let base_url = crate::providers::normalize_base_url(Some(base_url.to_string()));
        conn.execute(
            "UPDATE models SET base_url=?1 WHERE name=?2",
            rusqlite::params![base_url, name],
        )
        .map_err(|e| e.to_string())?;
    }
    if m.get("timeout_secs").is_some() {
        let timeout_secs = m
            .get("timeout_secs")
            .and_then(|v| v.as_i64())
            .filter(|v| *v > 0);
        conn.execute(
            "UPDATE models SET timeout_secs=?1 WHERE name=?2",
            rusqlite::params![timeout_secs, name],
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(max_tokens) = m.get("max_tokens").and_then(|v| v.as_i64()) {
        conn.execute(
            "UPDATE models SET max_tokens=?1 WHERE name=?2",
            rusqlite::params![max_tokens, name],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Delete a model by name, returning the number of rows removed. Shared helper.
pub(crate) fn apply_delete_model(conn: &rusqlite::Connection, name: &str) -> Result<usize, String> {
    conn.execute("DELETE FROM models WHERE name=?1", rusqlite::params![name])
        .map_err(|e| e.to_string())
}

/// Disable a model by name, but only when it's currently enabled — returns the
/// number of rows changed (0 when the model is missing or already disabled). The
/// `AND enabled=1` guard is what lets Homeostasis' Health Check auto-disable count
/// only models it actually parked this run, so a repeated run doesn't keep
/// "re-disabling" the same rows.
pub(crate) fn apply_disable_model(
    conn: &rusqlite::Connection,
    name: &str,
) -> Result<usize, String> {
    conn.execute(
        "UPDATE models SET enabled=0 WHERE name=?1 AND enabled=1",
        rusqlite::params![name],
    )
    .map_err(|e| e.to_string())
}

pub async fn add_model(State(state): State<AppState>, Json(m): Json<Value>) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        if let Err(e) = apply_add_model(&conn, &m) {
            return Json(json!({"ok": false, "error": e}));
        }
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
        if let Err(e) = apply_update_model(&conn, &name, &m) {
            return Json(json!({"ok": false, "error": e}));
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
        let _ = apply_delete_model(&conn, &name);
        let new_models = crate::config::load_models_from_db(&conn).unwrap_or_default();
        crate::router::update_models(&state.router, new_models).await;
        return Json(json!({"ok": true}));
    }
    Json(json!({"ok": false, "error": "DB error"}))
}
