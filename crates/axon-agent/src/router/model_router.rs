use crate::config::RuntimeSettings;
use crate::providers::{call_provider_with_options, types::*, ProviderCallOptions};
use crate::tools::schema::ToolDefinition;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;

#[derive(Debug, Clone)]
pub enum RouterAlert {
    ModelFailed {
        model_name: String,
        model_id: String,
        error: String,
        is_rate_limit: bool,
        is_timeout: bool,
        consecutive_errors: u32,
        threshold: u32,
    },
    ModelIdFailed {
        model_name: String,
        model_id: String,
        error: String,
        is_rate_limit: bool,
        is_timeout: bool,
    },
    UsedPaidFallback {
        model_name: String,
        model_id: String,
    },
}

pub struct RouterState {
    pub models: Vec<ModelRecord>,
    pub global_index: usize,
    pub alerts: Arc<Mutex<Vec<RouterAlert>>>,
}
pub type SharedRouter = Arc<Mutex<RouterState>>;

#[derive(Clone, Default)]
pub struct CallLlmOptions {
    /// Explicit user-selected model (highest priority, from UI or node config).
    pub preferred_model_name: Option<String>,
    /// Last model that succeeded in the current run. Tried before pool routing
    /// so long multi-step runs stay on one model — avoids mid-run provider switches
    /// that can cause tool-format inconsistencies.
    pub sticky_model_name: Option<String>,
    pub stream_sink: Option<StreamSink>,
    pub deadline: Option<Instant>,
    /// Deterministic per-iteration seed derived from run_id ⊕ iteration.
    /// When present, replaces the shared global_index counter so routing is
    /// stateless and reproducible without mutex contention.
    pub route_seed: Option<usize>,
    /// Sampling temperature override. `None` leaves the provider default.
    pub temperature: Option<f32>,
    /// Force/suppress tool use for this call (e.g. `Required` after a false refusal).
    pub tool_choice: Option<crate::providers::ToolChoice>,
    /// Reasoning effort for reasoning-capable models. `None` omits the field.
    pub reasoning_effort: Option<String>,
}

fn is_request_budget_error(err: &anyhow::Error) -> bool {
    err.to_string().contains("Request budget exhausted")
}

pub async fn drain_alerts(router: &SharedRouter) -> Vec<RouterAlert> {
    let g = router.lock().await;
    let mut a = g.alerts.lock().await;
    let v = std::mem::take(&mut *a);
    if !v.is_empty() {
        tracing::info!("Drained {} router alerts", v.len());
    }
    v
}

pub fn format_alerts(alerts: &[RouterAlert]) -> String {
    if alerts.is_empty() {
        return String::new();
    }
    let mut lines = vec!["\n---\n*Router alerts during this run:*".to_string()];
    for alert in alerts {
        match alert {
            RouterAlert::ModelFailed {
                model_name,
                model_id,
                error,
                is_rate_limit,
                is_timeout,
                consecutive_errors,
                threshold,
            } => {
                if *is_rate_limit {
                    lines.push(format!(
                        "- ⚠️ {} ({}) was rate-limited.",
                        model_name, model_id
                    ));
                } else if *is_timeout {
                    lines.push(format!("- ⚠️ {} ({}) timed out.", model_name, model_id));
                } else {
                    lines.push(format!(
                        "- ⚠️ {} ({}) errored: {} [{} consecutive/{} threshold]",
                        model_name, model_id, error, consecutive_errors, threshold
                    ));
                }
            }
            RouterAlert::ModelIdFailed {
                model_name,
                model_id,
                error,
                is_rate_limit,
                is_timeout,
            } => {
                if *is_rate_limit {
                    lines.push(format!(
                        "- ⚠️ {} ({}) was rate-limited (fallback used).",
                        model_name, model_id
                    ));
                } else if *is_timeout {
                    lines.push(format!(
                        "- ⚠️ {} ({}) timed out (fallback used).",
                        model_name, model_id
                    ));
                } else {
                    lines.push(format!(
                        "- ⚠️ {} ({}) errored: {} (fallback used)",
                        model_name, model_id, error
                    ));
                }
            }
            RouterAlert::UsedPaidFallback {
                model_name,
                model_id,
            } => {
                lines.push(format!(
                    "- 💰 Paid fallback used: {} ({})",
                    model_name, model_id
                ));
            }
        }
    }
    lines.join("\n")
}

pub async fn has_available_role(router: &SharedRouter, role: &str) -> bool {
    let g = router.lock().await;
    g.models
        .iter()
        .any(|m| m.role == role && m.enabled && m.is_available())
}

impl RouterState {
    pub fn new(models: Vec<ModelRecord>) -> Self {
        let mut models = models;
        models.sort_by_key(|m| m.priority);
        RouterState {
            models,
            global_index: 0,
            alerts: Arc::new(Mutex::new(Vec::new())),
        }
    }
    fn pool_indices(&self, role: &str) -> Vec<usize> {
        self.models
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == role && m.enabled)
            .map(|(i, _)| i)
            .collect()
    }
    fn general_pool(&self) -> Vec<usize> {
        self.models
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role.is_empty() && m.enabled)
            .map(|(i, _)| i)
            .collect()
    }
}

/// Build a priority-ordered model sequence for a pool, applying round-robin
/// *within* each priority tier across concurrent calls.
///
/// The rotation is keyed on `start` (incremented once per `call_llm` invocation),
/// so distribution is across calls rather than within a single call. Within a
/// single call all models at the same priority level are tried in offset order.
///
/// Within a tier there is no rate-limit-headroom steering: buckets keep their
/// natural order and the `start` offset gives a fair round-robin / pseudo-random
/// pick across calls. Models are only ever skipped when actually unavailable
/// (`is_available()` is false), never demoted for being "close" to a limit.
fn build_priority_order(models: &[ModelRecord], pool: &[usize], start: usize) -> Vec<usize> {
    let mut tiers: std::collections::BTreeMap<i32, Vec<usize>> = std::collections::BTreeMap::new();
    for &mi in pool {
        if models[mi].is_available() {
            let p = models[mi].priority;
            tiers.entry(p).or_default().push(mi);
        }
    }

    let mut order = Vec::new();
    for (_, group) in tiers {
        let mut buckets: std::collections::BTreeMap<(String, String, String), Vec<usize>> =
            std::collections::BTreeMap::new();
        for &mi in &group {
            let model = &models[mi];
            let key = (
                model.provider.clone(),
                model.base_url.clone().unwrap_or_default(),
                model.model_id.clone(),
            );
            buckets.entry(key).or_default().push(mi);
        }

        // Keep buckets in their natural (provider, base_url, model_id) order;
        // the `start` offset below rotates the starting bucket per call so the
        // pick is fairly distributed across calls. No headroom-based reordering.
        let bucket_keys = buckets.keys().cloned().collect::<Vec<_>>();
        let bucket_count = bucket_keys.len();
        if bucket_count == 0 {
            continue;
        }

        let mut emitted = 0usize;
        let target = group.len();
        let mut round = 0usize;
        while emitted < target {
            for offset in 0..bucket_count {
                let key = &bucket_keys[(start + offset) % bucket_count];
                if let Some(bucket) = buckets.get(key) {
                    if round < bucket.len() {
                        // Rotate the starting key *within* the bucket too, keyed
                        // on `start`. Multiple API keys for the same provider +
                        // model land in one bucket; without this, key #0 is
                        // always tried first and gets hammered until it 429s
                        // while the others sit idle. `(start + round) % len` is a
                        // permutation over the bucket, so all keys are still
                        // covered with no duplicates.
                        order.push(bucket[(start + round) % bucket.len()]);
                        emitted += 1;
                    }
                }
            }
            round += 1;
        }
    }
    order
}

tokio::task_local! {
    /// Per-run sink for *auxiliary* LLM token usage (tool router + quality gate).
    /// `run_inner` scopes it and folds the total into the run's reported token
    /// count, so cost telemetry reflects the hidden routing/QC spend rather than
    /// only the main agent calls. Absent outside a run, so writers use try_with.
    pub static RUN_TOKEN_SINK: std::sync::Arc<std::sync::atomic::AtomicU64>;
}

/// Add auxiliary (router/QC) token usage to the current run's sink, if one is in
/// scope. No-op outside a run.
pub fn record_aux_tokens(n: u32) {
    let _ = RUN_TOKEN_SINK.try_with(|s| {
        s.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
    });
}

pub async fn call_llm(
    messages: &[Message],
    system: &str,
    tools: &[ToolDefinition],
    max_tokens: Option<u32>,
    role: &str,
    router: SharedRouter,
    settings: &RuntimeSettings,
    preferred_model_name: Option<&str>,
) -> anyhow::Result<(UnifiedResponse, String, String)> {
    call_llm_with_options(
        messages,
        system,
        tools,
        max_tokens,
        role,
        router,
        settings,
        CallLlmOptions {
            preferred_model_name: preferred_model_name.map(|name| name.to_string()),
            ..CallLlmOptions::default()
        },
    )
    .await
}

pub async fn call_llm_with_options(
    messages: &[Message],
    system: &str,
    tools: &[ToolDefinition],
    max_tokens: Option<u32>,
    role: &str,
    router: SharedRouter,
    settings: &RuntimeSettings,
    options: CallLlmOptions,
) -> anyhow::Result<(UnifiedResponse, String, String)> {
    let threshold = settings.error_threshold();
    let timeout_secs = settings.model_call_timeout_secs();
    // Tracks every model index actually attempted across all passes.
    // Fed to the sweep pass (1.5) for precise dedup.
    let mut attempted_indices: std::collections::HashSet<usize> = std::collections::HashSet::new();

    // Use the caller-supplied deterministic seed when available.
    // This removes mutex contention and makes routing reproducible per (run, iteration).
    // Fall back to the shared global_index counter for callers (e.g. quality_checker)
    // that don't supply a seed — preserves their existing cross-call distribution.
    let start_index = if let Some(seed) = options.route_seed {
        seed
    } else {
        let mut g = router.lock().await;
        let idx = g.global_index;
        g.global_index = g.global_index.wrapping_add(1);
        idx
    };

    // Pass -1: preferred model (user-selected, e.g. from Axon node)
    if let Some(pref_name) = options.preferred_model_name.as_deref() {
        let preferred_models: Vec<(usize, String, u32)> = {
            let g = router.lock().await;
            g.models
                .iter()
                .enumerate()
                .filter(|(_, m)| m.name == pref_name && m.enabled && m.is_available())
                .map(|(i, m)| (i, m.name.clone(), max_tokens.unwrap_or(m.max_tokens)))
                .collect()
        };

        for (mi, name, tokens) in preferred_models {
            attempted_indices.insert(mi);
            match try_call(
                mi,
                messages,
                system,
                tools,
                tokens,
                &router,
                settings,
                threshold,
                timeout_secs,
                &options,
            )
            .await
            {
                Ok(r) => {
                    tracing::info!("Preferred model '{}' succeeded", name);
                    return Ok((r, name, "preferred".to_string()));
                }
                Err(e) if is_request_budget_error(&e) => return Err(e),
                Err(_) => {}
            }
            tracing::warn!(
                "Preferred model '{}' failed, falling back to normal routing",
                pref_name
            );
        }
    }

    // Pass 0: sticky model (last model that succeeded in this run).
    // Tried before pool routing so long multi-step runs stay on one model —
    // mid-run provider switches cause tool-format inconsistencies between turns.
    // Skipped if it is the same as preferred_model_name (already tried above).
    if let Some(sticky_name) = options.sticky_model_name.as_deref() {
        let already_tried = options
            .preferred_model_name
            .as_deref()
            .map(|p| p == sticky_name)
            .unwrap_or(false);

        if !already_tried {
            let sticky_models: Vec<(usize, String, u32)> = {
                let g = router.lock().await;
                g.models
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| m.name == sticky_name && m.enabled && m.is_available())
                    .map(|(i, m)| (i, m.name.clone(), max_tokens.unwrap_or(m.max_tokens)))
                    .collect()
            };

            for (mi, name, tokens) in sticky_models {
                attempted_indices.insert(mi);
                match try_call(
                    mi,
                    messages,
                    system,
                    tools,
                    tokens,
                    &router,
                    settings,
                    threshold,
                    timeout_secs,
                    &options,
                )
                .await
                {
                    Ok(r) => {
                        tracing::info!("Sticky model '{}' succeeded", name);
                        return Ok((r, name, "sticky".to_string()));
                    }
                    Err(e) if is_request_budget_error(&e) => return Err(e),
                    Err(_) => {
                        tracing::info!(
                            "Sticky model '{}' failed, falling through to pool routing",
                            sticky_name
                        );
                    }
                }
            }
        }
    }
    if !role.is_empty() && role != "paid_model" {
        // FIX #6: Collect order AND (name, tokens) in a single lock scope per
        // pass, rather than re-acquiring the lock for each model individually.
        let ordered_models: Vec<(usize, String, u32)> = {
            let g = router.lock().await;
            let pool = g.pool_indices(role);
            let order = build_priority_order(&g.models, &pool, start_index);
            order
                .into_iter()
                // Skip models already tried in the preferred/sticky passes — no
                // point re-hitting a just-failed endpoint within the same call.
                .filter(|mi| !attempted_indices.contains(mi))
                .map(|mi| {
                    let m = &g.models[mi];
                    (mi, m.name.clone(), max_tokens.unwrap_or(m.max_tokens))
                })
                .collect()
        };

        for (mi, name, tokens) in ordered_models {
            attempted_indices.insert(mi);
            match try_call(
                mi,
                messages,
                system,
                tools,
                tokens,
                &router,
                settings,
                threshold,
                timeout_secs,
                &options,
            )
            .await
            {
                Ok(r) => return Ok((r, name, role.to_string())),
                Err(e) if is_request_budget_error(&e) => return Err(e),
                Err(_) => {}
            }
        }
    }

    // Pass 1: general pool
    let ordered_models: Vec<(usize, String, u32)> = {
        let g = router.lock().await;
        let pool = g.general_pool();
        let order = build_priority_order(&g.models, &pool, start_index);
        order
            .into_iter()
            // Skip models already attempted in earlier passes (preferred,
            // sticky, role) so each model is tried at most once per call.
            .filter(|mi| !attempted_indices.contains(mi))
            .map(|mi| {
                let m = &g.models[mi];
                (mi, m.name.clone(), max_tokens.unwrap_or(m.max_tokens))
            })
            .collect()
    };

    let general_total = ordered_models.len();
    for (route_idx, (mi, name, tokens)) in ordered_models.into_iter().enumerate() {
        attempted_indices.insert(mi);
        match try_call(
            mi,
            messages,
            system,
            tools,
            tokens,
            &router,
            settings,
            cooldown,
            threshold,
            timeout_secs,
            route_idx,
            general_total,
            &options,
        )
        .await
        {
            Ok(r) => return Ok((r, name, "general".to_string())),
            Err(e) if is_request_budget_error(&e) => return Err(e),
            Err(_) => {}
        }
    }

    // Pass 1.5: sweep over ALL enabled, non-paid models regardless of role.
    // This catches cases where the user has assigned roles to every model (so
    // the general pool with role="" is empty) but the role-specific pool was
    // exhausted. We use `attempted_indices` — the set of every model index
    // actually called in prior passes — for precise dedup.
    {
        let sweep_models: Vec<(usize, String, u32)> = {
            let g = router.lock().await;
            g.models
                .iter()
                .enumerate()
                .filter(|(i, m)| {
                    m.enabled
                        && m.is_available()
                        && m.role != "paid_model"
                        && !attempted_indices.contains(i)
                })
                .map(|(i, m)| (i, m.name.clone(), max_tokens.unwrap_or(m.max_tokens)))
                .collect()
        };
        let sweep_total = sweep_models.len();
        for (route_idx, (mi, name, tokens)) in sweep_models.into_iter().enumerate() {
            match try_call(
                mi,
                messages,
                system,
                tools,
                tokens,
                &router,
                settings,
                cooldown,
                threshold,
                timeout_secs,
                route_idx,
                sweep_total,
                &options,
            )
            .await
            {
                Ok(r) => {
                    tracing::info!("Pass 1.5 sweep succeeded via '{}'", name);
                    return Ok((r, name, "sweep_fallback".to_string()));
                }
                Err(e) if is_request_budget_error(&e) => return Err(e),
                Err(_) => {}
            }
        }
    }
    let ordered_models: Vec<(usize, String, u32)> = {
        let g = router.lock().await;
        let pool = g.pool_indices("paid_model");
        let order = build_priority_order(&g.models, &pool, start_index);
        order
            .into_iter()
            .map(|mi| {
                let m = &g.models[mi];
                (mi, m.name.clone(), max_tokens.unwrap_or(m.max_tokens))
            })
            .collect()
    };

    let paid_total = ordered_models.len();
    for (route_idx, (mi, name, tokens)) in ordered_models.into_iter().enumerate() {
        match try_call(
            mi,
            messages,
            system,
            tools,
            tokens,
            &router,
            settings,
            cooldown,
            threshold,
            timeout_secs,
            route_idx,
            paid_total,
            &options,
        )
        .await
        {
            Ok(r) => {
                let model_id = {
                    let g = router.lock().await;
                    g.models[mi].model_id.clone()
                };
                router
                    .lock()
                    .await
                    .alerts
                    .lock()
                    .await
                    .push(RouterAlert::UsedPaidFallback {
                        model_name: name.clone(),
                        model_id,
                    });
                return Ok((r, name, "paid_fallback".to_string()));
            }
            Err(e) if is_request_budget_error(&e) => return Err(e),
            Err(_) => {}
        }
    }

    anyhow::bail!("All models exhausted — check API keys or wait for rate limits to reset")
}

async fn try_call(
    idx: usize,
    messages: &[Message],
    system: &str,
    tools: &[ToolDefinition],
    max_tokens: u32,
    router: &SharedRouter,
    settings: &RuntimeSettings,
    cooldown: i64,
    threshold: u32,
    default_timeout_secs: u64,
    route_attempt_index: usize,
    route_attempt_total: usize,
    options: &CallLlmOptions,
) -> anyhow::Result<UnifiedResponse> {
    // FIX #4: Clone the real ModelRecord and override only what differs per
    // model_id, rather than constructing a blank throwaway struct. This
    // ensures call_provider sees correct metadata (base_url, provider, etc.)
    // and avoids silent default-value bugs if it reads any other field.
    let (base_record, model_ids_str, api_key, timeout_secs) = {
        let g = router.lock().await;
        let m = &g.models[idx];
        if !m.is_available() {
            anyhow::bail!("not available");
        }
        let resolved_key = settings.resolve(&m.api_key);
        (
            m.clone(),
            m.model_id.clone(),
            resolved_key,
            m.timeout_secs.unwrap_or(default_timeout_secs),
        )
    };

    let model_ids: Vec<&str> = model_ids_str
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    if api_key.trim().is_empty() {
        anyhow::bail!(
            "Model '{}' has no API key after resolution; check AXON_MASTER_KEY and provider environment variables on the server",
            base_record.name
        );
    }
    if api_key.starts_with("${") && api_key.ends_with("}") {
        anyhow::bail!(
            "Model '{}' has unresolved API key placeholder {}; check .env loading or server environment configuration",
            base_record.name,
            api_key
        );
    }

    if model_ids.is_empty() {
        anyhow::bail!("No model IDs provided");
    }

    let prompt_chars = estimate_prompt_chars(messages, system);
    let min_timeout_secs = settings
        .model_call_timeout_min_secs()
        .max(1)
        .min(timeout_secs.max(1));
    let max_timeout_secs = settings.model_call_timeout_max_secs().max(min_timeout_secs);
    let per_1k_chars_secs = settings.model_call_timeout_per_1k_chars_secs();
    let fair_share_grace_secs = settings.model_call_timeout_fair_share_grace_secs();
    let remaining_route_slots = route_attempt_total
        .saturating_sub(route_attempt_index)
        .max(1);

    let mut last_error: Option<(anyhow::Error, bool, bool, String)> = None; // (error, is_rate_limit, is_timeout, model_id)

    for (i, current_model_id) in model_ids.iter().enumerate() {
        // Clone the real record and override only model_id, api_key, max_tokens.
        // All other fields (provider, base_url, role, etc.) come from the real record.
        let mut tmp = base_record.clone();
        tmp.model_id = current_model_id.to_string();
        tmp.api_key = api_key.clone();
        tmp.max_tokens = max_tokens;

        if i > 0 {
            tracing::info!("→ {} (fallback)", current_model_id);
        } else {
            tracing::info!("→ {}", current_model_id);
        }

        // FIX: Ensure tool list passed to providers is always unique by name.
        // Presence of duplicate tool names causes some providers (like Gemini)
        // to return a 400 Bad Request error.
        let mut unique_tools = Vec::new();
        let mut seen_names = std::collections::HashSet::new();
        for t in tools {
            if seen_names.insert(t.name.clone()) {
                unique_tools.push(t.clone());
            }
        }

        let prompt_bonus_secs =
            ((prompt_chars as u64).saturating_add(999) / 1000).saturating_mul(per_1k_chars_secs);
        let tool_bonus_secs = (unique_tools.len().min(8) as u64) * 2;
        let model_id_fallback_bonus_secs = (i as u64).min(2) * 2;
        let desired_timeout_secs = timeout_secs
            .max(1)
            .saturating_add(prompt_bonus_secs)
            .saturating_add(tool_bonus_secs)
            .saturating_add(model_id_fallback_bonus_secs)
            .clamp(min_timeout_secs, max_timeout_secs);

        // Adaptive + fair-share timeout:
        // - adaptive by prompt/tool complexity
        // - bounded by remaining chain deadline
        // - and fair-shared across remaining fallback attempts.
        let total_remaining_slots = remaining_route_slots
            .saturating_add(model_ids.len().saturating_sub(i + 1))
            .max(1);
        let attempt_timeout = if let Some(deadline) = options.deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                anyhow::bail!("Request budget exhausted before model call");
            }
            let remaining_secs = remaining.as_secs().max(1);
            let fair_share_secs = ((remaining_secs + total_remaining_slots as u64 - 1)
                / total_remaining_slots as u64)
                .max(min_timeout_secs);
            let soft_cap_secs = fair_share_secs.saturating_add(fair_share_grace_secs);
            let final_secs = desired_timeout_secs
                .min(soft_cap_secs)
                .min(remaining_secs)
                .max(1);
            Duration::from_secs(final_secs)
        } else {
            Duration::from_secs(desired_timeout_secs)
        };

        let provider_options = ProviderCallOptions {
            stream_sink: if options.stream_sink.is_some()
                && unique_tools.is_empty()
                && !options
                    .stream_sink
                    .as_ref()
                    .map(|sink| sink.has_started())
                    .unwrap_or(false)
            {
                options.stream_sink.clone()
            } else {
                None
            },
            temperature: options.temperature,
            tool_choice: options.tool_choice,
            reasoning_effort: options.reasoning_effort.clone(),
        };

        let call_result = tokio::time::timeout(
            attempt_timeout,
            call_provider_with_options(
                &mut tmp,
                messages,
                system,
                &unique_tools,
                max_tokens,
                provider_options,
            ),
        )
        .await;

        match call_result {
            Ok(Ok(resp)) => {
                let mut g = router.lock().await;
                g.models[idx].mark_success(resp.usage.input_tokens, resp.usage.output_tokens);
                g.models[idx].rl_snapshot = tmp.rl_snapshot;
                // Fix 3a: remove any ModelIdFailed alerts accumulated during earlier
                // model_ids in this same record — they were transient and the record
                // ultimately succeeded, so they must not reach watchers.
                {
                    let record_model_ids: std::collections::HashSet<String> =
                        model_ids.iter().map(|s| s.to_string()).collect();
                    let mut alert_guard = g.alerts.lock().await;
                    alert_guard.retain(|a| {
                        !matches!(a,
                            RouterAlert::ModelIdFailed { model_id, .. }
                            if record_model_ids.contains(model_id)
                        )
                    });
                }
                tracing::info!(
                    "✓ {} ({}in+{}out tokens)",
                    current_model_id,
                    resp.usage.input_tokens,
                    resp.usage.output_tokens
                );
                return Ok(resp);
            }
            Ok(Err(e)) => {
                if options
                    .stream_sink
                    .as_ref()
                    .map(|sink| sink.has_started())
                    .unwrap_or(false)
                {
                    return Err(anyhow::anyhow!(
                        "Stream interrupted after partial output from {}: {}",
                        current_model_id,
                        e
                    ));
                }
                tracing::warn!("✗ {} failed: {}", current_model_id, e);
                let s = e.to_string().to_lowercase();
                let is_rl = s.contains("rate limit") || s.contains("429") || s.contains("quota");
                // Fix 4a: only push ModelIdFailed when there are genuinely multiple
                // model_ids (i.e., a real sub-model fallback is happening).  A single
                // model_id record will get a record-level ModelFailed below — no need
                // to emit both for the same failure.
                if model_ids.len() > 1 {
                    router
                        .lock()
                        .await
                        .alerts
                        .lock()
                        .await
                        .push(RouterAlert::ModelIdFailed {
                            model_name: base_record.name.clone(),
                            model_id: current_model_id.to_string(),
                            error: e.to_string(),
                            is_rate_limit: is_rl,
                            is_timeout: false,
                        });
                    tracing::debug!("Pushed ModelIdFailed alert for model {}", current_model_id);
                }
                last_error = Some((e, is_rl, false, current_model_id.to_string()));
            }
            Err(_elapsed) => {
                if options
                    .stream_sink
                    .as_ref()
                    .map(|sink| sink.has_started())
                    .unwrap_or(false)
                {
                    return Err(anyhow::anyhow!(
                        "Stream interrupted after partial output from {} due to timeout",
                        current_model_id
                    ));
                }
                // Timeout — treat like a transient error (not a rate limit)
                let e = anyhow::anyhow!("Model timed out after {}s", attempt_timeout.as_secs());
                tracing::warn!(
                    "✗ {} timed out after {}s",
                    current_model_id,
                    attempt_timeout.as_secs()
                );
                if model_ids.len() > 1 {
                    router
                        .lock()
                        .await
                        .alerts
                        .lock()
                        .await
                        .push(RouterAlert::ModelIdFailed {
                            model_name: base_record.name.clone(),
                            model_id: current_model_id.to_string(),
                            error: e.to_string(),
                            is_rate_limit: false,
                            is_timeout: true,
                        });
                }
                last_error = Some((e, false, true, current_model_id.to_string()));
            }
        }
    }

    // All model_ids in this slot failed — update router state and log alert.
    if let Some((e, is_rl, is_timeout, failed_model_id)) = last_error {
        let (consecutive, model_name) = {
            let mut g = router.lock().await;
            if is_rl {
                let hint = parse_rate_limit_hint(&e.to_string());
                g.models[idx].mark_rate_limited(&hint, cooldown, settings.rate_limit_max_cooldown());
            } else {
                g.models[idx].mark_error(threshold, cooldown);
            }
            (g.models[idx].consecutive_errors, g.models[idx].name.clone())
        };

        // Fix 4b: use the explicitly-tracked is_timeout flag rather than
        // re-parsing the error string, which is fragile.
        router
            .lock()
            .await
            .alerts
            .lock()
            .await
            .push(RouterAlert::ModelFailed {
                model_name: model_name.clone(),
                model_id: failed_model_id.clone(),
                error: e.to_string(),
                is_rate_limit: is_rl,
                is_timeout,
                consecutive_errors: consecutive,
                threshold,
            });
        tracing::debug!("Pushed ModelFailed alert for model {}", failed_model_id);

        return Err(e);
    }

    anyhow::bail!("Unhandled fallback logic error")
}

pub async fn get_status(router: &SharedRouter) -> Vec<serde_json::Value> {
    let mut g = router.lock().await;

    // FIX #2: Reordered the condition to check status first, then
    // is_available(). This makes the intent explicit: only touch models
    // that are stuck in a cooldown state whose timer has now expired.
    // Previously the condition was technically correct but the order
    // implied is_available() could fire on healthy "available" models.
    for m in g.models.iter_mut() {
        if (m.status == "rate_limited" || m.status == "unavailable") && m.is_available() {
            m.status = "available".into();
            m.rate_limit_reset_at = None;
            m.consecutive_errors = 0;
            // NOTE: deliberately do NOT reset consecutive_rate_limits here.
            // It must persist across cooldown expiries so the exponential
            // backoff keeps escalating for a model stuck on a daily quota;
            // only a genuine success (mark_success) clears it.
        }
    }

    g.models
        .iter()
        .map(|m| {
            serde_json::json!({
                "name": m.name, "provider": m.provider, "model_id": m.model_id,
                "base_url": m.base_url, "timeout_secs": m.timeout_secs,
                "priority": m.priority, "role": m.role, "status": m.status,
                "max_tokens": m.max_tokens,
                "enabled": m.enabled,
                "rate_limit_reset_at": m.rate_limit_reset_at,
                "consecutive_errors": m.consecutive_errors,
                "consecutive_rate_limits": m.consecutive_rate_limits,
                "total_calls": m.total_calls,
                "total_input_tokens": m.total_input_tokens,
                "total_output_tokens": m.total_output_tokens,
                "rl_snapshot": serde_json::to_value(&m.rl_snapshot).unwrap_or_default(),
            })
        })
        .collect()
}

pub async fn reset_model(router: &SharedRouter, name: &str) -> bool {
    let mut g = router.lock().await;
    if let Some(m) = g.models.iter_mut().find(|m| m.name == name) {
        m.status = "available".into();
        m.rate_limit_reset_at = None;
        m.consecutive_errors = 0;
        m.consecutive_rate_limits = 0;
        return true;
    }
    false
}

pub async fn update_models(router: &SharedRouter, mut new_models: Vec<ModelRecord>) {
    new_models.sort_by_key(|m| m.priority);
    let mut g = router.lock().await;
    g.models = new_models;
    // FIX #3: Do NOT reset global_index to 0 on config reload.
    // Resetting it caused a burst of requests to the first model after
    // every reload, defeating the round-robin distribution. Preserving
    // the existing counter keeps distribution smooth across reloads.
}
