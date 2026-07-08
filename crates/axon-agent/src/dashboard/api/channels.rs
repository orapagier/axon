use super::*;

pub async fn telegram_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> impl axum::response::IntoResponse {
    let secret = headers
        .get("x-telegram-bot-api-secret-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // 1. Find all enabled telegram-trigger workflows (used for secret validation).
    let workflows = {
        let Ok(conn) = state.db.get() else {
            return axum::http::StatusCode::INTERNAL_SERVER_ERROR;
        };
        conn.prepare(
            "SELECT id, trigger_config FROM (
                SELECT DISTINCT w.id,
                    COALESCE(json_extract(wn.config, '$.type'), w.trigger_type) as trigger_type,
                    COALESCE(wn.config, w.trigger_config) as trigger_config
                FROM workflows w
                LEFT JOIN workflow_nodes wn ON wn.workflow_id = w.id AND wn.node_type IN ('trigger', 'circadian', 'stimulus')
                WHERE w.enabled = 1
            ) WHERE trigger_type = 'telegram'"
        )
        .and_then(|mut s| s.query_map([], |r| Ok((
            r.get::<_, String>(0)?,
            serde_json::from_str::<Value>(&r.get::<_, String>(1)?).unwrap_or(json!({}))
        ))).map(|i| i.filter_map(|r| r.ok()).collect::<Vec<_>>())).unwrap_or_default()
    };

    // 2. Validate the secret against any matching trigger and act on the routing result.
    //
    // Routing is encoded in the callback_data prefix (set when the button was built):
    //
    //   trig:<workflow_name>  (route_to_trigger = true)
    //     → Find the workflow whose name equals <workflow_name> and run it.
    //       The main AI agent is NOT invoked for this click.
    //
    //   agent:<instruction>   (route_to_trigger = false, default)
    //     → Pass <instruction> as a task to the main AI agent.
    //       No trigger workflow is fired.
    //
    //   <no prefix>  (plain message or legacy callback_data)
    //     → Existing behaviour: run the trigger's own workflow.
    //
    // We break out of the loop after the first workflow that accepts the request
    // so the update is processed exactly once.
    'secret_check: for (wf_id, config) in workflows {
        let res =
            crate::tools::telegram::handle_telegram_webhook(secret, payload.clone(), &config).await;

        match res {
            // Secret mismatch or filtered out — try the next registered trigger.
            crate::tools::telegram::TriggerResult::Rejected { reason } => {
                tracing::debug!("[TELEGRAM] Trigger {} rejected: {}", wf_id, reason);
                continue 'secret_check;
            }

            // ── Toggle ON (trig: prefix) ──────────────────────────────────────
            // handle_telegram_webhook has already stripped the "trig:" prefix from
            // callback_data, so data["/callback_query/data"] == the workflow name.
            //
            // If there is NO callback_data (plain message, photo, voice, etc.) the
            // update was not triggered by a button at all — route to the main agent
            // just like the agent: path below.  Trigger workflows only fire when a
            // button explicitly carries a trig: prefix.
            crate::tools::telegram::TriggerResult::AcceptedForTrigger(ref data) => {
                let workflow_name = data
                    .pointer("/callback_query/data")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                // ── No callback data → plain message → main agent ─────────────
                if workflow_name.is_empty() {
                    // Extract text from message or caption; fall back to a generic task.
                    let text = data
                        .pointer("/message/text")
                        .or_else(|| data.pointer("/message/caption"))
                        .or_else(|| data.pointer("/channel_post/text"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    let chat_id_str = data
                        .pointer("/message/chat/id")
                        .or_else(|| data.pointer("/channel_post/chat/id"))
                        .and_then(|v| v.as_i64())
                        .map(|id| id.to_string());

                    let session_id_str = data
                        .pointer("/message/from/id")
                        .or_else(|| data.pointer("/channel_post/from/id"))
                        .and_then(|v| v.as_i64())
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| "telegram".to_string());

                    let task = if text.is_empty() {
                        "Handle Telegram message".to_string()
                    } else {
                        text
                    };

                    tracing::info!(
                        "[TELEGRAM] plain message → main agent task '{}' (chat={})",
                        task,
                        chat_id_str.as_deref().unwrap_or("?")
                    );

                    let state_clone = state.clone();
                    let task_clone = task.clone();
                    tokio::spawn(async move {
                        let context = crate::agent::RunContext::new(
                            &task_clone,
                            "telegram",
                            Some(session_id_str.as_str()),
                            chat_id_str.as_deref(),
                            None,
                            None,
                            None,
                        );
                        if let Err(e) =
                            crate::agent::run_task(&task_clone, &state_clone, context).await
                        {
                            tracing::error!(
                                "[TELEGRAM] Agent task failed for plain message '{}': {}",
                                task_clone,
                                e
                            );
                        }
                    });

                    break 'secret_check;
                }

                // ── Has callback data → trig: button → resolve + set trigger data + run ──
                tracing::info!(
                    "[TELEGRAM] trig: button → resolving and running workflow '{}'",
                    workflow_name
                );

                let wf_name = workflow_name.clone();
                let state_clone = state.clone();

                // Build trigger data so the workflow's stimulus node has the full context.
                let trigger_payload = json!({
                    "trigger": "telegram",
                    "events": [{
                        "type": "callback_query",
                        "chat_id": data.pointer("/callback_query/message/chat/id")
                            .and_then(|v| v.as_i64())
                            .map(|id| id.to_string())
                            .unwrap_or_default(),
                        "data": wf_name,
                        "from": data.pointer("/callback_query/from").cloned().unwrap_or(json!({})),
                        "message": data.pointer("/callback_query/message").cloned().unwrap_or(json!({}))
                    }]
                });

                tokio::spawn(async move {
                    // Resolve name → ID (same helper used by polling path).
                    let Ok(conn) = state_clone.db.get() else {
                        tracing::error!("[TELEGRAM] trig: DB unavailable resolving '{}'", wf_name);
                        return;
                    };
                    let resolved: Option<String> = conn.prepare(
                        "SELECT id FROM workflows WHERE LOWER(name) = LOWER(?1) AND enabled = 1 LIMIT 1"
                    )
                    .and_then(|mut s| s.query_row(
                        rusqlite::params![wf_name],
                        |r| r.get::<_, String>(0),
                    ))
                    .ok();

                    let Some(wf_id) = resolved else {
                        tracing::error!(
                            "[TELEGRAM] trig: workflow named '{}' not found or not enabled",
                            wf_name
                        );
                        return;
                    };

                    // Fire as a real "telegram" trigger (matches the long-poll path
                    // in messaging/telegram.rs): scopes the run to the telegram
                    // trigger node and stays on the production path so A4 pinned data
                    // is not applied to this live button callback. The callback
                    // context rides the call, staged for this run.
                    if let Err(e) =
                        crate::tools::workflow::WorkflowEngine::run_in_background_with_payload(
                            &wf_id,
                            &state_clone,
                            "telegram",
                            None,
                            Some(trigger_payload),
                        )
                    {
                        tracing::error!(
                            "[TELEGRAM] WorkflowEngine failed for '{}' (id={}): {}",
                            wf_name,
                            wf_id,
                            e
                        );
                    }
                });

                break 'secret_check;
            }

            // ── Toggle OFF (agent: prefix, default) ───────────────────────────
            // Send the raw callback_data straight to the main agent as the task.
            // The agent receives it exactly as the button author wrote it.
            crate::tools::telegram::TriggerResult::AcceptedForAgent(ref body) => {
                let task = body
                    .pointer("/callback_query/data")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let chat_id_str = body
                    .pointer("/callback_query/message/chat/id")
                    .and_then(|v| v.as_i64())
                    .map(|id| id.to_string());

                let session_id_str = body
                    .pointer("/callback_query/from/id")
                    .and_then(|v| v.as_i64())
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "telegram".to_string());

                tracing::info!(
                    "[TELEGRAM] agent: button → task '{}' (chat={})",
                    task,
                    chat_id_str.as_deref().unwrap_or("?")
                );

                let state_clone = state.clone();
                let task_clone = task.clone();
                tokio::spawn(async move {
                    let context = crate::agent::RunContext::new(
                        &task_clone,
                        "telegram",
                        Some(session_id_str.as_str()),
                        chat_id_str.as_deref(),
                        None,
                        None,
                        None,
                    );
                    if let Err(e) = crate::agent::run_task(&task_clone, &state_clone, context).await
                    {
                        tracing::error!("[TELEGRAM] Agent task failed for callback: {}", e);
                    }
                });

                break 'secret_check;
            }
        }
    }

    axum::http::StatusCode::OK
}

pub async fn whatsapp_webhook_verify(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl axum::response::IntoResponse {
    let hub_mode = query.get("hub.mode").map(|s| s.as_str()).unwrap_or("");
    let hub_challenge = query.get("hub.challenge").map(|s| s.as_str()).unwrap_or("");
    let hub_verify_token = query
        .get("hub.verify_token")
        .map(|s| s.as_str())
        .unwrap_or("");

    let workflows = {
        let Ok(conn) = state.db.get() else {
            return axum::response::Response::builder()
                .status(axum::http::StatusCode::INTERNAL_SERVER_ERROR)
                .body(axum::body::Body::empty())
                .unwrap();
        };
        conn.prepare(
            "SELECT id, trigger_config FROM (
                SELECT DISTINCT w.id, 
                    COALESCE(json_extract(wn.config, '$.type'), w.trigger_type) as trigger_type,
                    COALESCE(wn.config, w.trigger_config) as trigger_config
                FROM workflows w
                LEFT JOIN workflow_nodes wn ON wn.workflow_id = w.id AND wn.node_type IN ('trigger', 'circadian', 'stimulus')
                WHERE w.enabled = 1
            ) WHERE trigger_type = 'whatsapp'"
        )
            .and_then(|mut s| s.query_map([], |r| Ok((
                r.get::<_, String>(0)?,
                serde_json::from_str::<Value>(&r.get::<_, String>(1)?).unwrap_or(json!({}))
            ))).map(|i| i.filter_map(|r| r.ok()).collect::<Vec<_>>())).unwrap_or_default()
    };

    for (_wf_id, config) in workflows {
        let res = crate::tools::whatsapp::verify_whatsapp_webhook(
            hub_mode,
            hub_challenge,
            hub_verify_token,
            &config,
        );
        match res {
            crate::tools::whatsapp::WebhookVerifyResult::Challenge(challenge) => {
                return axum::response::Response::builder()
                    .status(axum::http::StatusCode::OK)
                    .body(axum::body::Body::from(challenge))
                    .unwrap();
            }
            crate::tools::whatsapp::WebhookVerifyResult::Forbidden { .. } => {}
        }
    }

    axum::response::Response::builder()
        .status(axum::http::StatusCode::FORBIDDEN)
        .body(axum::body::Body::empty())
        .unwrap()
}

pub async fn whatsapp_webhook_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl axum::response::IntoResponse {
    let creds = crate::webhook::facebook::load_fb_creds();
    let sig_header = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !crate::webhook::signature::verify_meta_signature(
        "WhatsApp",
        &creds.app_secret,
        &body,
        sig_header,
    ) {
        tracing::warn!("WhatsApp webhook: invalid HMAC signature");
        return axum::http::StatusCode::UNAUTHORIZED;
    }

    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("WhatsApp webhook: invalid JSON: {}", e);
            return axum::http::StatusCode::BAD_REQUEST;
        }
    };

    let workflows = {
        let Ok(conn) = state.db.get() else {
            return axum::http::StatusCode::INTERNAL_SERVER_ERROR;
        };
        conn.prepare(
            "SELECT id, trigger_config FROM (
                SELECT DISTINCT w.id, 
                    COALESCE(json_extract(wn.config, '$.type'), w.trigger_type) as trigger_type,
                    COALESCE(wn.config, w.trigger_config) as trigger_config
                FROM workflows w
                LEFT JOIN workflow_nodes wn ON wn.workflow_id = w.id AND wn.node_type IN ('trigger', 'circadian', 'stimulus')
                WHERE w.enabled = 1
            ) WHERE trigger_type = 'whatsapp'"
        )
            .and_then(|mut s| s.query_map([], |r| Ok((
                r.get::<_, String>(0)?,
                serde_json::from_str::<Value>(&r.get::<_, String>(1)?).unwrap_or(json!({}))
            ))).map(|i| i.filter_map(|r| r.ok()).collect::<Vec<_>>())).unwrap_or_default()
    };

    for (wf_id, config) in workflows {
        let res = crate::tools::whatsapp::handle_whatsapp_webhook(payload.clone(), &config).await;
        if let crate::tools::whatsapp::TriggerResult::Accepted(data) = res {
            // Fire as a real "whatsapp" trigger (these workflows were selected
            // precisely because they have a whatsapp trigger): scopes the run to it
            // and stays on the production path so A4 pinned data is not applied.
            // The event data rides the call, staged for the stimulus node of
            // this specific run.
            if let Err(e) = crate::tools::workflow::WorkflowEngine::run_in_background_with_payload(
                &wf_id,
                &state,
                "whatsapp",
                None,
                Some(json!({ "trigger": "whatsapp", "events": data })),
            ) {
                tracing::error!("Failed to trigger background whatsapp workflow: {}", e);
            }
        }
    }

    axum::http::StatusCode::OK
}
