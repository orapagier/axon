use super::*;

pub async fn get_models(State(state): State<AppState>) -> Json<Value> {
    let models = crate::router::get_status(&state.router).await;
    Json(json!({"models": models}))
}

pub async fn reset_model(State(state): State<AppState>, Path(name): Path<String>) -> Json<Value> {
    let _ = crate::router::reset_model(&state.router, &name).await;
    Json(json!({"ok": true}))
}

/// Prefetched list of a provider's currently-available model ids, for the
/// ModelsPage "Model ID" dropdown. Body: `{provider, base_url?}`.
///
/// Fast path: the `provider_model_cache` populated by the daily background sweep
/// (`model_cache::refresh_all`), so opening the modal for an already-configured
/// provider is instant. Cache miss (e.g. adding a provider you don't have a model
/// for yet, like OpenRouter) falls back to a single live catalogue fetch and
/// warms the cache with it. An empty list means "nothing available"; the UI falls
/// back to free text.
pub async fn get_available_models(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let provider = payload
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if provider.is_empty() {
        return Json(json!({"ok": false, "error": "provider is required"}));
    }
    let base_url = payload
        .get("base_url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    // Fast path: the daily-swept cache.
    if let Ok(conn) = state.db.get() {
        let cached = crate::model_cache::read_cached(&conn, &provider, base_url.as_deref());
        if !cached.is_empty() {
            return Json(json!({"ok": true, "models": cached}));
        }
    }

    // Cache miss → one live fetch so a not-yet-configured provider still lists.
    // Resolve a key from an existing model of this provider when there is one;
    // providers whose /models is public (e.g. OpenRouter) list fine without one.
    let api_key = resolve_provider_key(&state, &provider);
    match crate::providers::list_available_models(&provider, base_url.as_deref(), &api_key).await {
        Ok(choices) if !choices.is_empty() => {
            if let Ok(conn) = state.db.get() {
                let _ = crate::model_cache::store(&conn, &provider, base_url.as_deref(), &choices);
            }
            Json(json!({"ok": true, "models": choices}))
        }
        Ok(_) => Json(json!({"ok": true, "models": []})),
        Err(e) => {
            tracing::warn!(
                "models/available live fallback for '{}' failed: {:#}",
                provider,
                e
            );
            Json(json!({"ok": true, "models": []}))
        }
    }
}

/// Resolve a usable API key for a provider from the first configured model that
/// has one, applying `${VAR}` resolution. Returns `""` when none is configured —
/// which is fine for providers whose model list is a public endpoint.
fn resolve_provider_key(state: &AppState, provider: &str) -> String {
    let provider = crate::providers::normalize_provider_name(provider);
    let Ok(conn) = state.db.get() else {
        return String::new();
    };
    let models = crate::config::load_models_from_db(&conn).unwrap_or_default();
    for m in models {
        if crate::providers::normalize_provider_name(&m.provider) != provider {
            continue;
        }
        let resolved = state.settings.resolve(&m.api_key);
        let k = resolved.trim();
        if !k.is_empty() && !(k.starts_with("${") && k.ends_with('}')) {
            return resolved;
        }
    }
    String::new()
}

/// Path to the boot-time models config; the same relative path `main.rs` loads.
/// Used to write dashboard model_id changes back to the file (see below).
const MODELS_TOML_PATH: &str = "config/models.toml";

/// Best-effort write-through of a chosen `model_id` into `config/models.toml`,
/// so the file (boot Source of Truth) tracks the DB. No-op when the model has no
/// block in the file (runtime/dashboard-only rows live in the DB alone). Only
/// called from the HTTP handlers — never from the shared `apply_*` helpers, so
/// the Homeostasis workflow node never writes to disk.
fn write_through_model_id(payload: &Value, name: &str) {
    let Some(model_id) = payload.get("model_id").and_then(|v| v.as_str()) else {
        return;
    };
    if model_id.trim().is_empty() {
        return;
    }
    match crate::config::set_model_id_in_toml(MODELS_TOML_PATH, name, model_id) {
        Ok(true) => tracing::info!("models.toml: model_id for '{}' set to '{}'", name, model_id),
        Ok(false) => {} // no block by this name in the file — DB-only model.
        Err(e) => tracing::warn!("models.toml write-through for '{}' failed: {:#}", name, e),
    }
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
        // Harmless for brand-new models (no matching block in models.toml → no-op);
        // updates the file when re-adding a name the file already defines.
        if let Some(name) = m.get("name").and_then(|v| v.as_str()) {
            write_through_model_id(&m, name);
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
        // A dashboard model_id change is written through to models.toml so it
        // survives the next boot's toml→DB sync instead of being reverted.
        write_through_model_id(&m, &name);
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
