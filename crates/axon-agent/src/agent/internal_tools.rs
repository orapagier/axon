//! Internal tool dispatch + handlers, extracted from `agent::r#loop`.
//!
//! These are the agent-invoked tools that run inside the process (shell, ssh,
//! web search, synapse/HTTP, workflows, cron jobs, watchers, memory) plus the
//! integration nodes (telegram/whatsapp/myelin/image) that reuse the workflow
//! node executors. `handle_internal` is the dispatcher the main loop calls;
//! `execute_internal_tool_from_workflow` is the entry point used by the
//! workflow engine.

use crate::agent::RunContext;
use crate::scheduler::store::StopCondition;
use crate::state::AppState;
use crate::tools::{ShellTool, SshTool};
use std::sync::Arc;

/// Fill in stored credentials for an agent-invoked tool so the model doesn't
/// have to know secrets (e.g. a Telegram bot token). Resolution order:
///   1. an explicit `credential_id` in the args → that credential's data
///   2. otherwise the most-recently-created credential whose `service` matches
///
/// Credential fields only fill *gaps*: anything the agent passed explicitly is
/// preserved (unlike the workflow path's `interpolate_config`, which overwrites,
/// because there the user picked the credential deliberately). If no credential
/// is found this is a no-op and the downstream tool surfaces its own clear
/// "missing token" error.
fn merge_stored_credentials(
    service: &str,
    args: serde_json::Value,
    state: &AppState,
) -> serde_json::Value {
    let mut map = match args {
        serde_json::Value::Object(m) => m,
        other => return other,
    };

    let data_str = state.db.get().ok().and_then(|conn| {
        let explicit_id = map
            .get("credential_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        match explicit_id {
            Some(id) => conn
                .query_row(
                    "SELECT data FROM credentials WHERE id = ?1",
                    [id.as_str()],
                    |r| r.get::<_, String>(0),
                )
                .ok(),
            None => conn
                .query_row(
                    "SELECT data FROM credentials WHERE service = ?1 ORDER BY created_at DESC LIMIT 1",
                    [service],
                    |r| r.get::<_, String>(0),
                )
                .ok(),
        }
    });

    if let Some(data_str) = data_str {
        // Encrypted at rest; decrypt_key passes legacy plaintext through.
        let data_str = crate::crypto::decrypt_key(&data_str);
        if let Ok(serde_json::Value::Object(cred_map)) = serde_json::from_str(&data_str) {
            for (k, v) in cred_map {
                map.entry(k).or_insert(v);
            }
        }
    }

    serde_json::Value::Object(map)
}

pub(crate) async fn handle_internal(
    name: &str,
    args: serde_json::Value,
    state: AppState,
    ctx: RunContext,
    run_id: String,
) -> anyhow::Result<serde_json::Value> {
    match name {
        "update_plan" => {
            let steps: Vec<crate::agent::plan::PlanStep> =
                serde_json::from_value(args.get("steps").cloned().unwrap_or_default()).map_err(
                    |e| anyhow::anyhow!("steps must be an array of {{step, status}} objects: {e}"),
                )?;
            if steps.is_empty() {
                anyhow::bail!("steps must not be empty");
            }
            crate::agent::plan::set_steps(&run_id, steps);
            let rendered = crate::agent::plan::render(&run_id).unwrap_or_default();
            Ok(serde_json::json!({
                "ok": true,
                "plan": rendered,
                "guidance": "Execute the next open step now. Call update_plan again (full list, statuses updated) as steps complete."
            }))
        }
        "search_tools" => {
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string();
            if query.is_empty() {
                anyhow::bail!("query is required");
            }
            let max = args
                .get("max_results")
                .and_then(|v| v.as_u64())
                .unwrap_or(5)
                .clamp(1, 10) as usize;
            let all = state.tools.all_enabled_for_agent().await;
            let matches = crate::agent::tool_discovery::search(&query, &all, max);
            if matches.is_empty() {
                return Ok(serde_json::json!({
                    "matches": [],
                    "guidance": "No tools matched. Try different keywords: a service name ('gmail', 'facebook', 'drive') or an action ('send', 'upload', 'schedule')."
                }));
            }
            let names: Vec<String> = matches.iter().map(|t| t.name.clone()).collect();
            crate::agent::tool_discovery::discover(&run_id, &names);
            let items: Vec<serde_json::Value> = matches
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "required": t.required,
                    })
                })
                .collect();
            Ok(serde_json::json!({
                "matches": items,
                "guidance": "These tools are now attached to this conversation — call the right one directly in your next step."
            }))
        }
        "cron_job_tool" => handle_job(args, state, ctx, run_id).await,
        "watcher_tool" => handle_watcher(args, state, ctx).await,
        "agent_memory_tool" => handle_memory(args, state).await,
        "ssh_tool" => handle_ssh(args, state).await,
        "shell_tool" => handle_shell(args).await,
        "parallel_worker" => handle_parallel_worker(args, state, ctx).await,
        "web_search" => handle_web_search(args, state).await,
        // NOTE: dispatch keys MUST match the names the tools are registered/exposed
        // under (see tools::http::{tool_definition, list_saved_tool_definition,
        // run_saved_tool_definition}), not the handler fn names. They diverged once
        // and silently 404'd every HTTP tool call as "Unknown internal tool".
        "synapse" => handle_http_request(args, state).await,
        "list_synapses" => handle_list_saved_http_requests(state).await,
        "run_synapse" => handle_run_saved_http_request(args, state).await,
        "list_workflows" => handle_list_workflows(state).await,
        "run_workflow" => handle_run_workflow(args, state).await,
        "image_tool" => crate::tools::image_tool::handle_image(args).await,
        // These reuse the workflow-node executors directly: the agent's tool args
        // share the same shape as the node `config` they read from. We first fill
        // in stored credentials (the agent doesn't carry secrets), then lift the
        // String-typed errors into anyhow.
        name if name.starts_with("telegram_") => {
            let args = merge_stored_credentials("telegram", args, &state);
            crate::tools::telegram::execute_split_tool(name, &args)
                .await
                .map_err(|e| anyhow::anyhow!(e))
        }
        "whatsapp" => {
            let args = merge_stored_credentials("whatsapp", args, &state);
            crate::tools::whatsapp::execute_whatsapp_node(&args)
                .await
                .map_err(|e| anyhow::anyhow!(e))
        }
        // myelin works on staged files, not external credentials.
        "myelin" => crate::tools::myelin::execute_myelin_node(&state, &args)
            .await
            .map_err(|e| anyhow::anyhow!(e)),
        other => anyhow::bail!("Unknown internal tool: {}", other),
    }
}

pub async fn execute_internal_tool_from_workflow(
    name: &str,
    args: serde_json::Value,
    state: AppState,
) -> anyhow::Result<serde_json::Value> {
    let ctx = crate::agent::RunContext::new(
        "workflow_internal_execution",
        "workflow",
        Some("workflow-session"),
        None,
        None,
        None,
        None,
    );
    handle_internal(name, args, state, ctx, uuid::Uuid::new_v4().to_string()).await
}

// ── Tool handlers (unchanged) ────────────────────────────────────────────────

async fn handle_parallel_worker(
    args: serde_json::Value,
    state: AppState,
    ctx: RunContext,
) -> anyhow::Result<serde_json::Value> {
    if ctx.depth >= 2 {
        return Ok(
            serde_json::json!({ "error": "parallel_worker cannot be nested more than 2 levels deep" }),
        );
    }
    let tasks = args
        .get("sub_tasks")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("sub_tasks array required"))?;
    if tasks.is_empty() {
        return Ok(serde_json::json!({"status": "no tasks provided"}));
    }

    let mut futures = vec![];
    for t in tasks {
        let task_str = t.as_str().unwrap_or("").to_string();
        let s2 = state.clone();
        let mut sub_ctx = ctx.clone();
        sub_ctx.run_id = uuid::Uuid::new_v4().to_string();
        sub_ctx.parent_run_id = Some(ctx.run_id.clone());
        sub_ctx.depth = ctx.depth + 1;
        sub_ctx.session_id = uuid::Uuid::new_v4().to_string();
        futures.push(tokio::spawn(async move {
            run_inner_owned(task_str, s2, sub_ctx).await
        }));
    }

    let results = futures::future::join_all(futures).await;
    let output: Vec<serde_json::Value> = results
        .into_iter()
        .map(|r| match r {
            Ok(Ok(res)) => serde_json::json!({"status": "ok", "result": res}),
            Ok(Err(e)) => serde_json::json!({"status": "error", "message": e.to_string()}),
            Err(e) => serde_json::json!({"status": "panic", "message": e.to_string()}),
        })
        .collect();

    Ok(serde_json::json!({ "status": "completed", "parallel_results": output }))
}

fn run_inner_owned(
    task: String,
    state: AppState,
    ctx: RunContext,
) -> futures::future::BoxFuture<'static, anyhow::Result<String>> {
    use futures::FutureExt;
    async move { crate::agent::r#loop::run_inner(&task, &state, ctx, None).await }.boxed()
}

async fn handle_shell(args: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
    let timeout = args
        .get("timeout_seconds")
        .and_then(|v| v.as_u64())
        .unwrap_or(30);
    ShellTool::run_command(cmd, timeout).await
}

async fn handle_web_search(
    args: serde_json::Value,
    state: AppState,
) -> anyhow::Result<serde_json::Value> {
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let top_n = args.get("top_n").and_then(|v| v.as_u64()).unwrap_or(5) as u8;
    if query.is_empty() {
        return Ok(serde_json::json!({"error": "Query is required"}));
    }
    let tool = crate::tools::web_search::WebSearchTool::new(Arc::clone(&state.db));
    match tool.search(query, top_n).await {
        Ok(response) => {
            let formatted =
                crate::tools::web_search::format_for_llm(&response, response.results.len());
            Ok(
                serde_json::json!({ "success": true, "account_used": response.account_used, "result_count": response.results.len(), "formatted": formatted, "results": response.results }),
            )
        }
        Err(e) => Ok(serde_json::json!({ "error": format!("Web search failed: {}", e) })),
    }
}

async fn handle_http_request(
    args: serde_json::Value,
    state: AppState,
) -> anyhow::Result<serde_json::Value> {
    let params: crate::tools::http::HttpRequestParams = serde_json::from_value(args)?;
    let tool = crate::tools::http::HttpRequestTool::new();
    match tool.request(params).await {
        Ok(resp) => {
            if let Some(binary) = &resp.binary {
                if let Ok(bytes) = tokio::fs::read(&binary.local_path).await {
                    use sha2::{Digest, Sha256};
                    let hash = format!("{:x}", Sha256::digest(&bytes));
                    let _ = state
                        .files
                        .store_path(
                            hash,
                            binary.original_name.clone(),
                            binary.local_path.clone(),
                            binary.mime_type.clone(),
                            bytes.len(),
                            Some("synapse".to_string()),
                            "outgoing",
                        )
                        .await;
                }
            }
            Ok(serde_json::to_value(resp)?)
        }
        Err(e) => Ok(serde_json::json!({"error": e.to_string()})),
    }
}

async fn handle_list_saved_http_requests(state: AppState) -> anyhow::Result<serde_json::Value> {
    let items = {
        let conn = state
            .db
            .get()
            .map_err(|e| anyhow::anyhow!("DB error: {}", e))?;
        let mut stmt = conn.prepare("SELECT id, name, url, method FROM http_requests")?;
        let items: Vec<serde_json::Value> = stmt
            .query_map([], |r| Ok(serde_json::json!({ "id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "url": r.get::<_, String>(2)?, "method": r.get::<_, String>(3)? })))?
            .filter_map(|r| r.ok()).collect();
        items
    };
    Ok(serde_json::json!({ "saved_requests": items }))
}

async fn handle_run_saved_http_request(
    args: serde_json::Value,
    state: AppState,
) -> anyhow::Result<serde_json::Value> {
    let name_or_id = args
        .get("name_or_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let (req_id, mut params, next_request_id) = {
        let conn = state
            .db
            .get()
            .map_err(|e| anyhow::anyhow!("DB error: {}", e))?;
        let mut stmt = conn.prepare("SELECT id, url, method, headers, body, proxy, next_request_id FROM http_requests WHERE id=?1 OR name=?1")?;
        let mut rows = stmt.query([&name_or_id])?;
        if let Some(row) = rows.next()? {
            let req_id: String = row.get::<_, String>(0)?;
            let params = crate::tools::http::HttpRequestParams {
                url: row.get::<_, String>(1)?,
                method: row.get::<_, String>(2)?,
                headers: row
                    .get::<_, Option<String>>(3)?
                    .and_then(|s| serde_json::from_str(&s).ok()),
                query: None,
                body: row
                    .get::<_, Option<String>>(4)?
                    .and_then(|s| serde_json::from_str(&s).ok()),
                auth: None,
                timeout_seconds: Some(30),
                response_format: Some("text".to_string()),
                limit: None,
                proxy: row.get::<_, Option<String>>(5)?,
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
            };
            let next_request_id: Option<String> = row.get::<_, Option<String>>(6)?;
            (req_id, params, next_request_id)
        } else {
            anyhow::bail!("Saved request '{}' not found", name_or_id)
        }
    };

    if let Some(bo) = args.get("body_override") {
        params.body = Some(bo.clone());
    }
    if let Some(ho) = args.get("header_overrides").and_then(|v| v.as_object()) {
        let mut current = params
            .headers
            .unwrap_or(serde_json::json!({}))
            .as_object()
            .cloned()
            .unwrap_or_default();
        for (k, v) in ho {
            current.insert(k.clone(), v.clone());
        }
        params.headers = Some(serde_json::Value::Object(current));
    }

    let tool = crate::tools::http::HttpRequestTool::new();
    let first_result = match tool.request(params).await {
        Ok(resp) => {
            if let Some(binary) = &resp.binary {
                if let Ok(bytes) = tokio::fs::read(&binary.local_path).await {
                    use sha2::{Digest, Sha256};
                    let hash = format!("{:x}", Sha256::digest(&bytes));
                    let _ = state
                        .files
                        .store_path(
                            hash,
                            binary.original_name.clone(),
                            binary.local_path.clone(),
                            binary.mime_type.clone(),
                            bytes.len(),
                            Some("synapse".to_string()),
                            "outgoing",
                        )
                        .await;
                }
            }
            serde_json::to_value(resp)?
        }
        Err(e) => return Ok(serde_json::json!({"error": e.to_string()})),
    };

    if let Some(ref next_id) = next_request_id {
        let chain_result = Box::pin(handle_run_saved_http_request(
            serde_json::json!({"name_or_id": next_id}),
            state.clone(),
        ))
        .await;
        let chain_val = chain_result.unwrap_or(serde_json::json!({"error": "chain failed"}));
        Ok(
            serde_json::json!({ "request_id": req_id, "result": first_result, "chained_to": next_id, "chain_result": chain_val }),
        )
    } else {
        Ok(first_result)
    }
}

async fn handle_list_workflows(state: AppState) -> anyhow::Result<serde_json::Value> {
    let items = {
        let conn = state
            .db
            .get()
            .map_err(|e| anyhow::anyhow!("DB error: {}", e))?;
        let mut stmt = conn.prepare("SELECT w.id, w.name, w.trigger_type, w.last_status, (SELECT COUNT(*) FROM workflow_nodes WHERE workflow_id = w.id) as node_count FROM workflows w ORDER BY w.name")?;
        let items: Vec<serde_json::Value> = stmt
            .query_map([], |r| Ok(serde_json::json!({ "id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "trigger_type": r.get::<_, String>(2)?, "last_status": r.get::<_, String>(3)?, "node_count": r.get::<_, i64>(4)? })))?
            .filter_map(|r| r.ok()).collect();
        items
    };
    Ok(serde_json::json!({ "workflows": items }))
}

async fn handle_run_workflow(
    args: serde_json::Value,
    state: AppState,
) -> anyhow::Result<serde_json::Value> {
    let name_or_id = args
        .get("name_or_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let workflow_id = {
        let conn = state
            .db
            .get()
            .map_err(|e| anyhow::anyhow!("DB error: {}", e))?;
        conn.query_row(
            "SELECT id FROM workflows WHERE id = ?1 OR name = ?1 LIMIT 1",
            rusqlite::params![name_or_id],
            |r| r.get::<_, String>(0),
        )
        .ok()
    };
    let wf_id =
        workflow_id.ok_or_else(|| anyhow::anyhow!("Workflow '{}' not found", name_or_id))?;
    match crate::tools::workflow::WorkflowEngine::run_in_background(&wf_id, &state, None) {
        Ok(run_id) => Ok(serde_json::json!({ "ok": true, "run_id": run_id })),
        Err(e) => Ok(serde_json::json!({ "error": e.to_string() })),
    }
}

async fn handle_ssh(args: serde_json::Value, state: AppState) -> anyhow::Result<serde_json::Value> {
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let server = args
        .get("server_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
    let remote = args
        .get("remote_path")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let local_path = args
        .get("local_path")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let timeout = args
        .get("timeout_seconds")
        .and_then(|v| v.as_u64())
        .unwrap_or(30);

    match action {
        "list_servers"                       => SshTool::list_servers(state).await,
        "run" | "exec_command" | "execute" | "exec" => SshTool::run_command(server, cmd, timeout, state).await,
        "upload_file"                        => SshTool::upload_file(server, remote, local_path, state).await,
        "download_file"                      => SshTool::download_file(server, remote, state).await,
        other => anyhow::bail!("Unknown SSH action: '{}'. Valid actions are: 'run', 'upload_file', 'download_file', 'list_servers'.", other),
    }
}

async fn handle_job(
    args: serde_json::Value,
    state: AppState,
    ctx: RunContext,
    run_id: String,
) -> anyhow::Result<serde_json::Value> {
    match args.get("action").and_then(|v| v.as_str()).unwrap_or("") {
        "create" => {
            let stop: Option<StopCondition> = args.get("stop_condition").and_then(|v| serde_json::from_value(v.clone()).ok());
            let j = state.scheduler.create(
                args.get("name").and_then(|v| v.as_str()).unwrap_or("agent-job"),
                args.get("task").and_then(|v| v.as_str()).unwrap_or(""),
                args.get("schedule_nl").and_then(|v| v.as_str()).unwrap_or("every hour"),
                "agent", Some(run_id.as_str()), Some(&ctx.platform), ctx.chat_id.as_deref(), stop,
            ).await?;
            Ok(serde_json::json!({"status":"created","job_id":j.id,"cron":j.cron_expr}))
        }
        "edit" => {
            let id = args.get("job_id").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("job_id required for edit"))?;
            let existing = state.scheduler.get_all().await?.into_iter().find(|j| j.id == id).ok_or_else(|| anyhow::anyhow!("Job '{}' not found", id))?;
            let name     = args.get("new_name").and_then(|v| v.as_str()).unwrap_or(&existing.name);
            let task     = args.get("new_task").and_then(|v| v.as_str()).unwrap_or(&existing.task);
            let schedule = args.get("new_schedule").and_then(|v| v.as_str()).unwrap_or(&existing.schedule_nl);
            state.scheduler.update(id, name, task, schedule).await?;
            Ok(serde_json::json!({"status": "updated", "job_id": id}))
        }
        "pause"  => { let id = args.get("job_id").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("job_id required"))?; state.scheduler.pause(id).await?;  Ok(serde_json::json!({"status":"paused"})) }
        "resume" => { let id = args.get("job_id").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("job_id required"))?; state.scheduler.resume(id).await?; Ok(serde_json::json!({"status":"resumed"})) }
        "delete" => { let id = args.get("job_id").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("job_id required"))?; state.scheduler.delete(id).await?; Ok(serde_json::json!({"status":"deleted"})) }
        "list"   => { let jobs = state.scheduler.get_all().await?; Ok(serde_json::json!({ "jobs": jobs })) }
        other => anyhow::bail!("Unknown job action: '{}'. Valid actions are: 'create', 'edit', 'pause', 'resume', 'delete', 'list'. Make sure to provide the action and relevant arguments like 'task' and 'schedule_nl'.", other),
    }
}

async fn handle_watcher(
    args: serde_json::Value,
    state: AppState,
    _ctx: RunContext,
) -> anyhow::Result<serde_json::Value> {
    fn parse_poll_mins(schedule: &str) -> f64 {
        if schedule.contains("minute") {
            schedule
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(30.0)
        } else if schedule.contains("hour") {
            schedule
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(1.0)
                * 60.0
        } else {
            30.0
        }
    }

    match args.get("action").and_then(|v| v.as_str()).unwrap_or("") {
        "add" => {
            let name = args
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("Watcher");
            let task = args
                .get("watch_command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let schedule = args
                .get("schedule_nl")
                .and_then(|v| v.as_str())
                .unwrap_or("every 30 minutes");
            let poll_mins = parse_poll_mins(schedule);
            let id = uuid::Uuid::new_v4().to_string();
            if let Ok(conn) = state.db.get() {
                let _ = conn.execute("INSERT INTO watchers (id, service, tool_name, label, enabled, poll_mins, created_at) VALUES (?1, 'command', ?2, ?3, 1, ?4, datetime('now'))", rusqlite::params![id, task, name, poll_mins]);
            }
            Ok(serde_json::json!({"status": "created", "watcher_id": id, "poll_mins": poll_mins}))
        }
        "edit" => {
            let id = args
                .get("watcher_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("watcher_id required"))?;
            if let Ok(conn) = state.db.get() {
                if let Some(n) = args.get("name").and_then(|v| v.as_str()) {
                    let _ = conn.execute(
                        "UPDATE watchers SET label=?1 WHERE id=?2",
                        rusqlite::params![n, id],
                    );
                }
                if let Some(t) = args.get("watch_command").and_then(|v| v.as_str()) {
                    let _ = conn.execute(
                        "UPDATE watchers SET tool_name=?1 WHERE id=?2",
                        rusqlite::params![t, id],
                    );
                }
                if let Some(s) = args.get("schedule_nl").and_then(|v| v.as_str()) {
                    let _ = conn.execute(
                        "UPDATE watchers SET poll_mins=?1 WHERE id=?2",
                        rusqlite::params![parse_poll_mins(s), id],
                    );
                }
            }
            Ok(serde_json::json!({"status": "updated", "watcher_id": id}))
        }
        "pause" => {
            let id = args
                .get("watcher_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("watcher_id required"))?;
            if let Ok(conn) = state.db.get() {
                let _ = conn.execute(
                    "UPDATE watchers SET enabled=0 WHERE id=?1",
                    rusqlite::params![id],
                );
            }
            Ok(serde_json::json!({"status": "paused"}))
        }
        "resume" => {
            let id = args
                .get("watcher_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("watcher_id required"))?;
            if let Ok(conn) = state.db.get() {
                let _ = conn.execute(
                    "UPDATE watchers SET enabled=1 WHERE id=?1",
                    rusqlite::params![id],
                );
            }
            Ok(serde_json::json!({"status": "resumed"}))
        }
        "delete" => {
            let id = args
                .get("watcher_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("watcher_id required"))?;
            if let Ok(conn) = state.db.get() {
                let _ = conn.execute("DELETE FROM watchers WHERE id=?1", rusqlite::params![id]);
            }
            Ok(serde_json::json!({"status": "deleted"}))
        }
        "list" => {
            if let Ok(conn) = state.db.get() {
                let mut stmt =
                    conn.prepare("SELECT id, label, tool_name, service, enabled FROM watchers")?;
                let watchers: Vec<serde_json::Value> = stmt
                    .query_map([], |r| Ok(serde_json::json!({ "id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?, "task": r.get::<_, String>(2)?, "service": r.get::<_, String>(3)?, "enabled": r.get::<_, i32>(4)? != 0 })))?
                    .filter_map(|r| r.ok()).collect();
                Ok(serde_json::json!({"watchers": watchers}))
            } else {
                anyhow::bail!("DB error")
            }
        }
        other => anyhow::bail!("Unknown watcher action: '{}'", other),
    }
}

async fn handle_memory(
    args: serde_json::Value,
    state: AppState,
) -> anyhow::Result<serde_json::Value> {
    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
    match args.get("action").and_then(|v| v.as_str()).unwrap_or("") {
        "store" => {
            let tags: Vec<String> = args
                .get("tags")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            let refs: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();
            Ok(
                serde_json::json!({"stored":true,"id":state.memory.remember(content,"agent_note",&refs).await?}),
            )
        }
        "search" => {
            let k = args.get("top_k").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
            Ok(serde_json::json!({"results":state.memory.search(content, k, None).await?}))
        }
        "delete" => {
            let id = args
                .get("memory_id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow::anyhow!("memory_id required"))?;
            state.memory.forget(id)?;
            Ok(serde_json::json!({"deleted":true}))
        }
        other => anyhow::bail!("Unknown memory action: {}", other),
    }
}
