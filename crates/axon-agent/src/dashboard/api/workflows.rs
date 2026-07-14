use super::*;

// ── Workflow API ──────────────────────────────────────────────────────────────

pub async fn get_workflows(State(state): State<AppState>) -> Json<Value> {
    let conn = match state.db.get() {
        Ok(c) => c,
        Err(_) => return Json(json!({"workflows": []})),
    };
    let mut stmt = match conn.prepare(
        // Most-recently added or edited workflow first: updated_at is bumped on
        // every save; COALESCE falls back to created_at for rows predating it.
        "SELECT id FROM workflows ORDER BY COALESCE(updated_at, created_at) DESC, created_at DESC",
    ) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Failed to prepare workflows query: {}", e);
            return Json(json!({"workflows": []}));
        }
    };
    let ids: Vec<String> = match stmt.query_map([], |r| r.get::<_, String>(0)) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(e) => {
            tracing::error!("Failed to query workflows: {}", e);
            return Json(json!({"workflows": []}));
        }
    };
    let workflows: Vec<Value> = ids
        .iter()
        .filter_map(|id| load_workflow_detail(&conn, id))
        .collect();
    Json(json!({"workflows": workflows}))
}

/// One workflow with its nodes and edges, in exactly the JSON shape that
/// `upsert_workflow` accepts back — so get → modify → save round-trips.
/// Shared by GET /api/workflows and the agent's `get_workflow` internal tool.
pub fn load_workflow_detail(conn: &rusqlite::Connection, wf_id: &str) -> Option<Value> {
    let mut wf = conn
        .query_row(
            "SELECT id, name, description, enabled, trigger_type, trigger_config, last_run_at, last_status, created_at, error_workflow_id FROM workflows WHERE id = ?1",
            rusqlite::params![wf_id],
            |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "description": r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    "enabled": r.get::<_, i64>(3)? != 0,
                    "trigger_type": r.get::<_, Option<String>>(4)?.unwrap_or_else(|| "manual".to_string()),
                    "trigger_config": serde_json::from_str::<Value>(&r.get::<_, Option<String>>(5)?.unwrap_or_else(|| "{}".to_string())).unwrap_or(json!({})),
                    "last_run_at": r.get::<_, Option<String>>(6)?,
                    "last_status": r.get::<_, Option<String>>(7)?.unwrap_or_else(|| "idle".to_string()),
                    "created_at": r.get::<_, Option<String>>(8)?.unwrap_or_default(),
                    // Error workflow (A3): the handler to run when this workflow fails.
                    "error_workflow_id": r.get::<_, Option<String>>(9)?,
                }))
            },
        )
        .ok()?;

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
        }).map(|it| it.filter_map(|r| r.ok()).collect()).unwrap_or_default();
        if let Some(obj) = wf.as_object_mut() {
            obj.insert("nodes".to_string(), json!(nodes));
        }
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
        }).map(|it| it.filter_map(|r| r.ok()).collect()).unwrap_or_default();
        if let Some(obj) = wf.as_object_mut() {
            obj.insert("edges".to_string(), json!(edges));
        }
    }

    Some(wf)
}

pub async fn upsert_workflow(
    State(state): State<AppState>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    Json(upsert_workflow_core(&state, payload).await)
}

/// Create-or-update core shared by POST /api/workflows and the agent's
/// `upsert_workflow` internal tool. Omitted `id` creates a new workflow;
/// `nodes`/`edges` are replace-all when present and left untouched when absent.
pub async fn upsert_workflow_core(state: &AppState, payload: Value) -> Value {
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
    // Error workflow (A3): the handler id to run when this workflow fails. Empty/
    // null clears it (falls back to the global default at runtime).
    let error_workflow_id: Option<String> = payload
        .get("error_workflow_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let nodes = payload.get("nodes").and_then(|v| v.as_array());
    let edges = payload.get("edges").and_then(|v| v.as_array());

    if let Ok(conn) = state.db.get() {
        // B1: snapshot the prior persisted state before this edit overwrites it.
        // No-op on first save (nothing to snapshot) and on unchanged/throttled
        // saves; bounded by the per-workflow version cap.
        snapshot_workflow_version(&conn, &state.settings, &id, false);

        let _ = conn.execute(
            // Stamp updated_at on every save so the workflow list can order
            // most-recently added/edited first (see get_workflows ORDER BY).
            "INSERT INTO workflows (id, name, description, enabled, trigger_type, trigger_config, error_workflow_id, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))
             ON CONFLICT(id) DO UPDATE SET name=?2, description=?3, enabled=?4, trigger_type=?5, trigger_config=?6, error_workflow_id=?7, updated_at=datetime('now')",
            rusqlite::params![id, name, description, enabled as i64, trigger_type, trigger_config, error_workflow_id],
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
                    .and_then(|v| {
                        v.as_i64()
                            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                    })
                    .unwrap_or(0)
                    .clamp(0, 100);
                let node_retry_wait = node
                    .get("retry_wait_ms")
                    .and_then(|v| {
                        v.as_i64()
                            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                    })
                    .unwrap_or(0)
                    .max(0);
                let node_retry_backoff = node
                    .get("retry_backoff")
                    .and_then(|v| v.as_str())
                    .filter(|s| *s == "exponential")
                    .unwrap_or("fixed");
                // Pinned output (A4): persist the saved value as a JSON string, or
                // NULL when absent/null/empty so the engine treats the node as not
                // pinned. Round-trips through get_workflows so a normal save keeps
                // an existing pin instead of wiping it on the DELETE+reinsert.
                let node_pinned: Option<String> = match node.get("pinned_data") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(s)) if s.trim().is_empty() => None,
                    Some(v) => Some(v.to_string()),
                };

                let _ = conn.execute(
                    "INSERT INTO workflow_nodes (id, workflow_id, position, position_x, position_y, node_type, name, config, enabled, continue_on_fail, retries, retry_wait_ms, retry_backoff, pinned_data) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                    rusqlite::params![node_id, id, i as i64, position_x, position_y, node_type, node_name, config, node_enabled as i64, node_continue as i64, node_retries, node_retry_wait, node_retry_backoff, node_pinned],
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

        json!({"ok": true, "id": id})
    } else {
        json!({"ok": false, "error": "DB error"})
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
        let _ = conn.execute(
            "DELETE FROM workflow_versions WHERE workflow_id = ?1",
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
    // ?entry=true → this node is a Stimulus/trigger entry point; run its whole
    // downstream chain but start from ONLY this node, leaving sibling triggers
    // dormant (the play button on a Stimulus in a multi-trigger workflow).
    // ?single=true → run ONLY this node, reusing cached upstream results from the
    // last run (the "Execute Step" button when upstream nodes already have data).
    // Without either, the node plus all its ancestors are re-run (the play button
    // on a mid-chain node).
    let entry = matches!(params.get("entry").map(String::as_str), Some("true" | "1"));
    let single = matches!(params.get("single").map(String::as_str), Some("true" | "1"));
    let result = if entry {
        crate::tools::workflow::WorkflowEngine::run_from_entry_node(&id, &state, node_id)
    } else {
        crate::tools::workflow::WorkflowEngine::run_node_in_background(&id, &state, node_id, single)
    };
    match result {
        Ok(run_id) => Json(json!({"ok": true, "run_id": run_id})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

/// Pin a node's output (A4). Body is the JSON value to pin — typically a prior
/// run's node output. On manual/editor runs the engine then returns this value
/// for the node WITHOUT executing it (deterministic builds, no side-effects).
/// The pin is also round-tripped through get_workflows → upsert so a later save
/// keeps it. Capped so pins stay editor-sized, not a binary dumping ground.
pub async fn pin_workflow_node(
    State(state): State<AppState>,
    Path((id, node_id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Json<Value> {
    const PIN_MAX_BYTES: usize = 256 * 1024;
    let serialized = body.to_string();
    if serialized.len() > PIN_MAX_BYTES {
        return Json(json!({
            "ok": false,
            "error": format!(
                "Pinned data too large ({} bytes, max {}). Use a real run instead.",
                serialized.len(),
                PIN_MAX_BYTES
            )
        }));
    }
    if let Ok(conn) = state.db.get() {
        match conn.execute(
            "UPDATE workflow_nodes SET pinned_data = ?1 WHERE workflow_id = ?2 AND id = ?3",
            rusqlite::params![serialized, id, node_id],
        ) {
            Ok(n) if n > 0 => Json(json!({"ok": true})),
            Ok(_) => Json(json!({"ok": false, "error": "Node not found"})),
            Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
        }
    } else {
        Json(json!({"ok": false, "error": "DB error"}))
    }
}

/// Clear a node's pin (A4): the node executes normally again on the next run.
pub async fn unpin_workflow_node(
    State(state): State<AppState>,
    Path((id, node_id)): Path<(String, String)>,
) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let _ = conn.execute(
            "UPDATE workflow_nodes SET pinned_data = NULL WHERE workflow_id = ?1 AND id = ?2",
            rusqlite::params![id, node_id],
        );
        Json(json!({"ok": true}))
    } else {
        Json(json!({"ok": false, "error": "DB error"}))
    }
}

/// Build the self-contained workflow bundle (A5 format) for a workflow id, or
/// `None` if the workflow doesn't exist. Shared by `export_workflow` (download)
/// and `snapshot_workflow_version` (B1 history) so export and history use one
/// format. Node configs reference credentials only by `credential_id` — secret
/// material never enters the bundle.
fn build_workflow_bundle(conn: &rusqlite::Connection, id: &str) -> Option<Value> {
    let wf = conn
        .query_row(
            "SELECT name, description, trigger_type, trigger_config, error_workflow_id FROM workflows WHERE id = ?1",
            [id],
            |r| {
                Ok(json!({
                    "name": r.get::<_, String>(0)?,
                    "description": r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    "trigger_type": r.get::<_, Option<String>>(2)?.unwrap_or_else(|| "manual".into()),
                    "trigger_config": serde_json::from_str::<Value>(&r.get::<_, Option<String>>(3)?.unwrap_or_else(|| "{}".into())).unwrap_or(json!({})),
                    "error_workflow_id": r.get::<_, Option<String>>(4)?,
                }))
            },
        )
        .ok()?;

    let nodes: Vec<Value> = match conn.prepare(
        "SELECT id, node_type, name, config, enabled, continue_on_fail, retries, retry_wait_ms, retry_backoff, position_x, position_y, pinned_data FROM workflow_nodes WHERE workflow_id = ?1 ORDER BY position ASC",
    ) {
        Ok(mut stmt) => stmt
            .query_map([id], |r| {
                Ok(json!({
                    "local_id": r.get::<_, String>(0)?,
                    "node_type": r.get::<_, String>(1)?,
                    "name": r.get::<_, String>(2)?,
                    "config": serde_json::from_str::<Value>(&r.get::<_, Option<String>>(3)?.unwrap_or_else(|| "{}".into())).unwrap_or(json!({})),
                    "enabled": r.get::<_, i64>(4)? != 0,
                    "continue_on_fail": r.get::<_, i64>(5)? != 0,
                    "retries": r.get::<_, i64>(6).unwrap_or(0),
                    "retry_wait_ms": r.get::<_, i64>(7).unwrap_or(0),
                    "retry_backoff": r.get::<_, Option<String>>(8)?.unwrap_or_else(|| "fixed".into()),
                    "position_x": r.get::<_, f64>(9)?,
                    "position_y": r.get::<_, f64>(10)?,
                    "pinned_data": r.get::<_, Option<String>>(11)?.filter(|s| !s.trim().is_empty()).and_then(|s| serde_json::from_str::<Value>(&s).ok()),
                }))
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    let edges: Vec<Value> = match conn.prepare(
        "SELECT source_id, target_id, source_handle, target_handle FROM workflow_edges WHERE workflow_id = ?1 \
         ORDER BY source_id, target_id, IFNULL(source_handle, ''), IFNULL(target_handle, '')",
    ) {
        Ok(mut stmt) => stmt
            .query_map([id], |r| {
                Ok(json!({
                    "source_local_id": r.get::<_, String>(0)?,
                    "target_local_id": r.get::<_, String>(1)?,
                    "source_handle": r.get::<_, Option<String>>(2)?,
                    "target_handle": r.get::<_, Option<String>>(3)?,
                }))
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    // Distinct credential references (ids only — service/name are metadata).
    let mut cred_ids: Vec<String> = Vec::new();
    for n in &nodes {
        if let Some(cid) = n
            .get("config")
            .and_then(|c| c.get("credential_id"))
            .and_then(|v| v.as_str())
        {
            if !cid.is_empty() && !cred_ids.iter().any(|c| c == cid) {
                cred_ids.push(cid.to_string());
            }
        }
    }
    let credentials_required: Vec<Value> = cred_ids
        .iter()
        .map(|cid| {
            let meta = conn
                .query_row(
                    "SELECT service, name FROM credentials WHERE id = ?1",
                    [cid],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .ok();
            json!({
                "credential_id": cid,
                "service": meta.as_ref().map(|m| m.0.clone()),
                "name": meta.as_ref().map(|m| m.1.clone()),
            })
        })
        .collect();

    Some(json!({
        "axon_format": 1,
        "exported_at": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "workflow": wf,
        "nodes": nodes,
        "edges": edges,
        "credentials_required": credentials_required,
    }))
}

/// Export a workflow as a self-contained JSON bundle (A5): metadata + nodes +
/// edges + pins. Secrets never leave the box — see `build_workflow_bundle`.
pub async fn export_workflow(State(state): State<AppState>, Path(id): Path<String>) -> Json<Value> {
    let Ok(conn) = state.db.get() else {
        return Json(json!({"ok": false, "error": "DB error"}));
    };
    match build_workflow_bundle(&conn, &id) {
        Some(bundle) => Json(bundle),
        None => Json(json!({"ok": false, "error": "Workflow not found"})),
    }
}

/// Every workflow, each as a full `import_workflow`-compatible bundle, wrapped in
/// one restorable envelope. Used by the scheduled Google-Drive backup
/// (`crate::maintenance::run_workflow_drive_backup`) and the manual endpoint.
/// Each element of `workflows` can be POSTed back to `/api/workflows/import`
/// as-is. Secrets never leave the box — bundles carry credential *references*,
/// not values (see `build_workflow_bundle`).
pub(crate) fn build_all_workflows_backup(conn: &rusqlite::Connection) -> Value {
    let ids: Vec<String> = conn
        .prepare(
            "SELECT id FROM workflows ORDER BY COALESCE(updated_at, created_at) DESC, created_at DESC",
        )
        .and_then(|mut stmt| {
            stmt.query_map([], |r| r.get::<_, String>(0))
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .unwrap_or_default();

    let bundles: Vec<Value> = ids
        .iter()
        .filter_map(|id| build_workflow_bundle(conn, id))
        .collect();

    json!({
        "axon_backup_format": 1,
        "backed_up_at": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "count": bundles.len(),
        "workflows": bundles,
    })
}

/// Manually trigger an off-instance backup of all workflow definitions to Google
/// Drive (POST /api/workflows/backup). Runs regardless of the `workflow_backup.
/// enabled` schedule flag — a manual click is always an explicit request. Needs
/// Google connected; returns the same stats the scheduled sweep logs.
pub async fn backup_workflows_to_drive(State(state): State<AppState>) -> Json<Value> {
    match crate::maintenance::run_workflow_drive_backup(&state).await {
        Ok(stats) => Json(json!({
            "ok": true,
            "workflows": stats.workflows,
            "file_name": stats.file_name,
            "drive_file_id": stats.drive_file_id,
            "web_view_link": stats.web_view_link,
            "pruned_local": stats.pruned_local,
            "pruned_drive": stats.pruned_drive,
        })),
        Err(e) => Json(json!({ "ok": false, "error": format!("{e:#}") })),
    }
}

/// B1: snapshot the CURRENT persisted state of a workflow as a new version row.
/// Called at the top of `upsert_workflow` BEFORE the incoming edit overwrites the
/// live rows, so versions hold the chain of prior states (single-operator undo).
///
/// No-ops when there is nothing or no point to snapshot:
///   * the workflow doesn't exist yet (first save) → `build_workflow_bundle` None
///   * content is byte-identical to the latest snapshot (content-hash dedupe)
///   * the latest snapshot is younger than the throttle window, unless `force`
///     (set by restore so the pre-restore state is always preserved)
///
/// After inserting, prunes to the per-workflow cap, always keeping labeled rows.
fn snapshot_workflow_version(
    conn: &rusqlite::Connection,
    settings: &crate::config::RuntimeSettings,
    workflow_id: &str,
    force: bool,
) {
    let Some(bundle) = build_workflow_bundle(conn, workflow_id) else {
        return;
    };

    // Hash the content only (exclude the volatile `exported_at`) so an unchanged
    // workflow re-saved repeatedly never spawns duplicate versions.
    let content = json!({
        "workflow": bundle.get("workflow"),
        "nodes": bundle.get("nodes"),
        "edges": bundle.get("edges"),
    });
    let hash = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(content.to_string().as_bytes());
        format!("{:x}", h.finalize())
    };

    // Latest version for this workflow: (version, content_hash, age in seconds).
    let latest: Option<(i64, Option<String>, i64)> = conn
        .query_row(
            "SELECT version, content_hash,
                    CAST((julianday('now') - julianday(created_at)) * 86400 AS INTEGER)
             FROM workflow_versions WHERE workflow_id = ?1
             ORDER BY version DESC LIMIT 1",
            [workflow_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();

    let next_version = match &latest {
        Some((v, last_hash, age_secs)) => {
            // Dedupe: identical content as the most recent snapshot → skip.
            if last_hash.as_deref() == Some(hash.as_str()) {
                return;
            }
            // Throttle autosave storms (restore bypasses this with `force`).
            if !force && *age_secs < settings.workflow_version_min_interval_secs() {
                return;
            }
            v + 1
        }
        None => 1,
    };

    let _ = conn.execute(
        "INSERT INTO workflow_versions (id, workflow_id, version, content_hash, snapshot)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            Uuid::new_v4().to_string(),
            workflow_id,
            next_version,
            hash,
            bundle.to_string(),
        ],
    );

    // Prune: keep the newest N overall plus any labeled rows beyond the cap.
    let cap = settings.retention_workflow_versions_per_workflow().max(1);
    let _ = conn.execute(
        "DELETE FROM workflow_versions WHERE id IN (
             SELECT id FROM (
                 SELECT id, label, ROW_NUMBER() OVER (
                     PARTITION BY workflow_id ORDER BY version DESC
                 ) AS rn
                 FROM workflow_versions WHERE workflow_id = ?1
             ) WHERE rn > ?2 AND (label IS NULL OR label = '')
         )",
        rusqlite::params![workflow_id, cap],
    );
}

/// Write a snapshot bundle back onto an EXISTING workflow id (B1 restore),
/// preserving node ids so pins and `$node` references survive. Replaces the
/// workflow's nodes/edges and metadata; leaves `enabled` untouched (the bundle
/// doesn't carry it). Mirrors the node/edge columns written by `upsert_workflow`.
fn restore_bundle_into_workflow(
    conn: &rusqlite::Connection,
    workflow_id: &str,
    bundle: &Value,
) -> Result<(), String> {
    let wf = bundle.get("workflow").cloned().unwrap_or_else(|| json!({}));
    let name = wf
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled");
    let description = wf.get("description").and_then(|v| v.as_str()).unwrap_or("");
    let trigger_type = wf
        .get("trigger_type")
        .and_then(|v| v.as_str())
        .unwrap_or("manual");
    let trigger_config = wf
        .get("trigger_config")
        .cloned()
        .unwrap_or_else(|| json!({}))
        .to_string();
    let error_workflow_id: Option<String> = wf
        .get("error_workflow_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    conn.execute(
        "UPDATE workflows SET name=?2, description=?3, trigger_type=?4, trigger_config=?5, error_workflow_id=?6, updated_at=datetime('now') WHERE id=?1",
        rusqlite::params![workflow_id, name, description, trigger_type, trigger_config, error_workflow_id],
    )
    .map_err(|e| e.to_string())?;

    conn.execute(
        "DELETE FROM workflow_nodes WHERE workflow_id = ?1",
        [workflow_id],
    )
    .map_err(|e| e.to_string())?;
    if let Some(nodes) = bundle.get("nodes").and_then(|v| v.as_array()) {
        for (i, node) in nodes.iter().enumerate() {
            let node_id = node.get("local_id").and_then(|v| v.as_str()).unwrap_or("");
            if node_id.is_empty() {
                continue;
            }
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
            let node_retries = node
                .get("retries")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                .clamp(0, 100);
            let node_retry_wait = node
                .get("retry_wait_ms")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                .max(0);
            let node_retry_backoff = node
                .get("retry_backoff")
                .and_then(|v| v.as_str())
                .filter(|s| *s == "exponential")
                .unwrap_or("fixed");
            let node_pinned: Option<String> = match node.get("pinned_data") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) if s.trim().is_empty() => None,
                Some(v) => Some(v.to_string()),
            };
            let _ = conn.execute(
                "INSERT INTO workflow_nodes (id, workflow_id, position, position_x, position_y, node_type, name, config, enabled, continue_on_fail, retries, retry_wait_ms, retry_backoff, pinned_data) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                rusqlite::params![node_id, workflow_id, i as i64, position_x, position_y, node_type, node_name, config, node_enabled as i64, node_continue as i64, node_retries, node_retry_wait, node_retry_backoff, node_pinned],
            );
        }
    }

    conn.execute(
        "DELETE FROM workflow_edges WHERE workflow_id = ?1",
        [workflow_id],
    )
    .map_err(|e| e.to_string())?;
    if let Some(edges) = bundle.get("edges").and_then(|v| v.as_array()) {
        for (i, edge) in edges.iter().enumerate() {
            let source_id = edge
                .get("source_local_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let target_id = edge
                .get("target_local_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if source_id.is_empty() || target_id.is_empty() {
                continue;
            }
            let source_handle = edge.get("source_handle").and_then(|v| v.as_str());
            let target_handle = edge.get("target_handle").and_then(|v| v.as_str());
            let _ = conn.execute(
                "INSERT INTO workflow_edges (id, workflow_id, source_id, target_id, source_handle, target_handle) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![format!("edge_{}_{}", workflow_id, i), workflow_id, source_id, target_id, source_handle, target_handle],
            );
        }
    }
    Ok(())
}

/// B1: list a workflow's version snapshots (metadata only — no snapshot blob).
pub async fn get_workflow_versions(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<Value> {
    let Ok(conn) = state.db.get() else {
        return Json(json!({"ok": false, "error": "DB error"}));
    };
    let versions: Vec<Value> = match conn.prepare(
        "SELECT version, label, content_hash, created_at FROM workflow_versions WHERE workflow_id = ?1 ORDER BY version DESC",
    ) {
        Ok(mut stmt) => stmt
            .query_map([&id], |r| {
                Ok(json!({
                    "version": r.get::<_, i64>(0)?,
                    "label": r.get::<_, Option<String>>(1)?,
                    "content_hash": r.get::<_, Option<String>>(2)?,
                    "created_at": r.get::<_, String>(3)?,
                }))
            })
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };
    Json(json!({"ok": true, "versions": versions}))
}

/// B1: full snapshot bundle for one version (preview / diff source).
pub async fn get_workflow_version(
    State(state): State<AppState>,
    Path((id, version)): Path<(String, i64)>,
) -> Json<Value> {
    let Ok(conn) = state.db.get() else {
        return Json(json!({"ok": false, "error": "DB error"}));
    };
    match conn.query_row(
        "SELECT snapshot FROM workflow_versions WHERE workflow_id = ?1 AND version = ?2",
        rusqlite::params![id, version],
        |r| r.get::<_, String>(0),
    ) {
        Ok(snap) => Json(json!({
            "ok": true,
            "version": version,
            "snapshot": serde_json::from_str::<Value>(&snap).unwrap_or(json!({})),
        })),
        Err(_) => Json(json!({"ok": false, "error": "Version not found"})),
    }
}

/// B1: label (or rename) a version. Labeled versions survive pruning. An empty
/// label clears it (the version becomes prunable again).
pub async fn label_workflow_version(
    State(state): State<AppState>,
    Path((id, version)): Path<(String, i64)>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let label = body
        .get("label")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Ok(conn) = state.db.get() else {
        return Json(json!({"ok": false, "error": "DB error"}));
    };
    match conn.execute(
        "UPDATE workflow_versions SET label = ?1 WHERE workflow_id = ?2 AND version = ?3",
        rusqlite::params![label, id, version],
    ) {
        Ok(n) if n > 0 => Json(json!({"ok": true})),
        Ok(_) => Json(json!({"ok": false, "error": "Version not found"})),
        Err(e) => Json(json!({"ok": false, "error": e.to_string()})),
    }
}

/// B1: restore a workflow to a prior version. Re-versions the CURRENT state first
/// (forced past the throttle) so restoring is itself undoable and never silently
/// discards the live state, then writes the chosen snapshot back onto the live
/// rows (node ids preserved).
pub async fn restore_workflow_version(
    State(state): State<AppState>,
    Path((id, version)): Path<(String, i64)>,
) -> Json<Value> {
    let Ok(conn) = state.db.get() else {
        return Json(json!({"ok": false, "error": "DB error"}));
    };
    // Load the target snapshot first so a bad version number changes nothing.
    let snap = match conn.query_row(
        "SELECT snapshot FROM workflow_versions WHERE workflow_id = ?1 AND version = ?2",
        rusqlite::params![id, version],
        |r| r.get::<_, String>(0),
    ) {
        Ok(s) => s,
        Err(_) => return Json(json!({"ok": false, "error": "Version not found"})),
    };
    let bundle = match serde_json::from_str::<Value>(&snap) {
        Ok(b) => b,
        Err(e) => return Json(json!({"ok": false, "error": format!("Corrupt snapshot: {e}")})),
    };
    snapshot_workflow_version(&conn, &state.settings, &id, true);
    match restore_bundle_into_workflow(&conn, &id, &bundle) {
        Ok(()) => Json(json!({"ok": true, "id": id, "restored_version": version})),
        Err(e) => Json(json!({"ok": false, "error": e})),
    }
}

/// Import a workflow bundle (A5). Generates a fresh workflow id and fresh node
/// ids, rewrites edges onto the new ids, and drops edges whose endpoints didn't
/// import. The new workflow is created DISABLED so an imported trigger can't
/// fire before the operator reviews it and maps credentials. `credential_id`
/// references are preserved as-is (they resolve on the same box; on a different
/// box the node UI prompts to re-select). Returns the new workflow id.
pub async fn import_workflow(
    State(state): State<AppState>,
    Json(bundle): Json<Value>,
) -> Json<Value> {
    let fmt = bundle
        .get("axon_format")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if fmt != 1 {
        return Json(json!({
            "ok": false,
            "error": "Unsupported or missing axon_format (expected 1)"
        }));
    }

    let wf = bundle.get("workflow").cloned().unwrap_or_else(|| json!({}));
    let name = wf
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Imported Workflow");
    let description = wf.get("description").and_then(|v| v.as_str()).unwrap_or("");
    let trigger_type = wf
        .get("trigger_type")
        .and_then(|v| v.as_str())
        .unwrap_or("manual");
    let trigger_config = wf
        .get("trigger_config")
        .cloned()
        .unwrap_or_else(|| json!({}))
        .to_string();

    let new_id = Uuid::new_v4().to_string();
    let nodes = bundle
        .get("nodes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let edges = bundle
        .get("edges")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // local_id → fresh id remap for nodes and edge endpoints.
    let mut id_map: HashMap<String, String> = HashMap::new();
    for n in &nodes {
        if let Some(local) = n.get("local_id").and_then(|v| v.as_str()) {
            id_map.insert(local.to_string(), Uuid::new_v4().to_string());
        }
    }

    let Ok(conn) = state.db.get() else {
        return Json(json!({"ok": false, "error": "DB error"}));
    };

    // Keep error_workflow_id only if the referenced handler still exists here.
    let error_wf_id: Option<String> = wf
        .get("error_workflow_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .filter(|eid| {
            conn.query_row("SELECT 1 FROM workflows WHERE id = ?1", [eid], |_| Ok(()))
                .is_ok()
        })
        .map(str::to_string);

    if let Err(e) = conn.execute(
        "INSERT INTO workflows (id, name, description, enabled, trigger_type, trigger_config, error_workflow_id, updated_at) VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6, datetime('now'))",
        rusqlite::params![new_id, name, description, trigger_type, trigger_config, error_wf_id],
    ) {
        return Json(json!({"ok": false, "error": format!("Insert failed: {e}")}));
    }

    for (i, n) in nodes.iter().enumerate() {
        let local = n.get("local_id").and_then(|v| v.as_str()).unwrap_or("");
        let nid = id_map
            .get(local)
            .cloned()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let node_type = n
            .get("node_type")
            .and_then(|v| v.as_str())
            .unwrap_or("http");
        let nname = n.get("name").and_then(|v| v.as_str()).unwrap_or("Step");
        let config = n
            .get("config")
            .cloned()
            .unwrap_or_else(|| json!({}))
            .to_string();
        let enabled = n.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
        let continue_on_fail = n
            .get("continue_on_fail")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let retries = n
            .get("retries")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .clamp(0, 100);
        let retry_wait = n
            .get("retry_wait_ms")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .max(0);
        let retry_backoff = n
            .get("retry_backoff")
            .and_then(|v| v.as_str())
            .filter(|s| *s == "exponential")
            .unwrap_or("fixed");
        let position_x = n.get("position_x").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let position_y = n.get("position_y").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let pinned: Option<String> = match n.get("pinned_data") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s.trim().is_empty() => None,
            Some(v) => Some(v.to_string()),
        };
        let _ = conn.execute(
            "INSERT INTO workflow_nodes (id, workflow_id, position, position_x, position_y, node_type, name, config, enabled, continue_on_fail, retries, retry_wait_ms, retry_backoff, pinned_data) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            rusqlite::params![nid, new_id, i as i64, position_x, position_y, node_type, nname, config, enabled as i64, continue_on_fail as i64, retries, retry_wait, retry_backoff, pinned],
        );
    }

    for (i, e) in edges.iter().enumerate() {
        let src_local = e
            .get("source_local_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let tgt_local = e
            .get("target_local_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let (Some(src), Some(tgt)) = (id_map.get(src_local), id_map.get(tgt_local)) else {
            continue; // endpoint didn't import — drop the dangling edge
        };
        let edge_id = format!("edge_{}_{}", new_id, i);
        let source_handle = e.get("source_handle").and_then(|v| v.as_str());
        let target_handle = e.get("target_handle").and_then(|v| v.as_str());
        let _ = conn.execute(
            "INSERT INTO workflow_edges (id, workflow_id, source_id, target_id, source_handle, target_handle) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![edge_id, new_id, src, tgt, source_handle, target_handle],
        );
    }

    let cred_count = bundle
        .get("credentials_required")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    Json(json!({"ok": true, "id": new_id, "name": name, "credentials_required": cred_count}))
}

pub async fn get_workflow_runs(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Json<Value> {
    if let Ok(conn) = state.db.get() {
        let mut stmt = try_json!(conn.prepare(
            "SELECT id, workflow_id, status, trigger_type, started_at, finished_at, node_results FROM workflow_runs WHERE workflow_id = ?1 ORDER BY started_at DESC LIMIT 10"
        ));
        let runs: Vec<Value> = try_json!(stmt.query_map(rusqlite::params![id], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "workflow_id": r.get::<_, String>(1)?,
                "status": r.get::<_, String>(2)?,
                "trigger_type": r.get::<_, Option<String>>(3)?,
                "started_at": r.get::<_, String>(4)?,
                "finished_at": r.get::<_, Option<String>>(5)?,
                "node_results": rehydrated_node_results(&r.get::<_, String>(6)?),
            }))
        }))
        .filter_map(|r| r.ok())
        .collect();
        Json(json!({"runs": runs}))
    } else {
        Json(json!({"runs": []}))
    }
}

/// Parse a stored `node_results` JSON string and rehydrate any B2 binary
/// descriptors back to their full values, so the UI never sees a `_axon_binary`
/// placeholder. Falls back to an empty array on parse failure.
fn rehydrated_node_results(stored: &str) -> Value {
    let mut v = serde_json::from_str::<Value>(stored).unwrap_or(json!([]));
    crate::tools::workflow::binary::rehydrate_value(&mut v);
    v
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
                    "node_results": rehydrated_node_results(&r.get::<_, String>(6)?),
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

#[cfg(test)]
mod version_tests {
    //! B1 workflow versioning: exercises the snapshot helpers directly (no
    //! AppState harness exists yet), covering dedupe, throttle, force, the
    //! per-workflow cap, labeled-row retention, and restore round-trip.
    use super::*;
    use crate::config::RuntimeSettings;
    use r2d2::Pool;
    use r2d2_sqlite::SqliteConnectionManager;

    fn temp_pool() -> (Arc<Pool<SqliteConnectionManager>>, std::path::PathBuf) {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "axon_versions_{}_{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let manager = SqliteConnectionManager::file(&path);
        let pool = Pool::new(manager).unwrap();
        {
            let conn = pool.get().unwrap();
            crate::db::init(&conn).unwrap();
        }
        (Arc::new(pool), path)
    }

    /// Create/overwrite a workflow with a single node whose name encodes the
    /// content, so changing `node_name` produces a distinct content hash.
    fn seed_workflow(conn: &rusqlite::Connection, id: &str, node_name: &str) {
        conn.execute(
            "INSERT INTO workflows (id, name, trigger_type) VALUES (?1, 'WF', 'manual')
             ON CONFLICT(id) DO UPDATE SET name='WF'",
            [id],
        )
        .unwrap();
        conn.execute("DELETE FROM workflow_nodes WHERE workflow_id = ?1", [id])
            .unwrap();
        conn.execute(
            "INSERT INTO workflow_nodes (id, workflow_id, position, position_x, position_y, node_type, name, config, enabled)
             VALUES ('n1', ?1, 0, 0, 0, 'http', ?2, '{}', 1)",
            rusqlite::params![id, node_name],
        )
        .unwrap();
    }

    fn version_count(conn: &rusqlite::Connection, id: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM workflow_versions WHERE workflow_id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn snapshot_dedupes_throttles_prunes_and_restores() {
        let (pool, path) = temp_pool();
        let settings = RuntimeSettings::new(Arc::clone(&pool));
        settings
            .set("workflow.version_min_interval_secs", "0")
            .unwrap();
        settings
            .set("retention.workflow_versions_per_workflow", "3")
            .unwrap();
        let conn = pool.get().unwrap();

        // Nothing to snapshot before the workflow exists.
        snapshot_workflow_version(&conn, &settings, "wf1", false);
        assert_eq!(version_count(&conn, "wf1"), 0);

        // First content → one version.
        seed_workflow(&conn, "wf1", "Alpha");
        snapshot_workflow_version(&conn, &settings, "wf1", false);
        assert_eq!(version_count(&conn, "wf1"), 1);

        // Identical content → deduped.
        snapshot_workflow_version(&conn, &settings, "wf1", false);
        assert_eq!(version_count(&conn, "wf1"), 1, "identical content deduped");

        // Changed content → second version.
        seed_workflow(&conn, "wf1", "Beta");
        snapshot_workflow_version(&conn, &settings, "wf1", false);
        assert_eq!(version_count(&conn, "wf1"), 2);

        // Throttle blocks a changed save inside the interval; force overrides it.
        settings
            .set("workflow.version_min_interval_secs", "3600")
            .unwrap();
        seed_workflow(&conn, "wf1", "Gamma");
        snapshot_workflow_version(&conn, &settings, "wf1", false);
        assert_eq!(version_count(&conn, "wf1"), 2, "throttled within interval");
        snapshot_workflow_version(&conn, &settings, "wf1", true);
        assert_eq!(version_count(&conn, "wf1"), 3, "force bypasses throttle");

        // Cap = 3: a fourth distinct version prunes the oldest (unlabeled) one.
        settings
            .set("workflow.version_min_interval_secs", "0")
            .unwrap();
        seed_workflow(&conn, "wf1", "Delta");
        snapshot_workflow_version(&conn, &settings, "wf1", false);
        assert_eq!(version_count(&conn, "wf1"), 3, "capped at 3");
        let min_v: i64 = conn
            .query_row(
                "SELECT MIN(version) FROM workflow_versions WHERE workflow_id='wf1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(min_v, 2, "oldest version pruned");

        // Restore version 2 (content 'Beta') back onto the live workflow.
        let snap: String = conn
            .query_row(
                "SELECT snapshot FROM workflow_versions WHERE workflow_id='wf1' AND version=2",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let bundle: Value = serde_json::from_str(&snap).unwrap();
        restore_bundle_into_workflow(&conn, "wf1", &bundle).unwrap();
        let live_name: String = conn
            .query_row(
                "SELECT name FROM workflow_nodes WHERE workflow_id='wf1' AND id='n1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(live_name, "Beta", "restored node content");

        drop(conn);
        drop(pool);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn labeled_versions_survive_the_cap() {
        let (pool, path) = temp_pool();
        let settings = RuntimeSettings::new(Arc::clone(&pool));
        settings
            .set("workflow.version_min_interval_secs", "0")
            .unwrap();
        settings
            .set("retention.workflow_versions_per_workflow", "2")
            .unwrap();
        let conn = pool.get().unwrap();

        // Two versions within the cap, then label the oldest.
        seed_workflow(&conn, "wf2", "A");
        snapshot_workflow_version(&conn, &settings, "wf2", true);
        seed_workflow(&conn, "wf2", "B");
        snapshot_workflow_version(&conn, &settings, "wf2", true);
        conn.execute(
            "UPDATE workflow_versions SET label='keep' WHERE workflow_id='wf2' AND version=1",
            [],
        )
        .unwrap();

        // A third version would normally evict v1, but its label exempts it.
        seed_workflow(&conn, "wf2", "C");
        snapshot_workflow_version(&conn, &settings, "wf2", true);
        let labeled_present: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM workflow_versions WHERE workflow_id='wf2' AND version=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(labeled_present, 1, "labeled version retained past cap");

        drop(conn);
        drop(pool);
        let _ = std::fs::remove_file(&path);
    }
}
