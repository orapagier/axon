use super::*;

// ── Watcher (Smart Notifications) ─────────────────────────────────────────────

pub async fn get_watchers(State(state): State<AppState>) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let mut s = try_json!(conn
            .prepare("SELECT id, service, tool_name, tool_args, label, enabled, poll_mins, last_check, last_seen_ids, created_at, trigger_condition FROM watchers ORDER BY created_at"));
        let watchers: Vec<Value> = try_json!(s.query_map([], |r| {
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
        }))
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
        let mut s = try_json!(conn
            .prepare("SELECT wl.id, wl.watcher_id, w.service, w.label, wl.new_count, wl.created_at FROM watcher_log wl LEFT JOIN watchers w ON w.id = wl.watcher_id ORDER BY wl.created_at DESC LIMIT 100"));
        let logs: Vec<Value> = try_json!(s.query_map([], |r| {
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
        }))
        .filter_map(|r| r.ok())
        .collect();
        return Json(json!({ "log": logs }));
    }
    Json(json!({ "log": [] }))
}
