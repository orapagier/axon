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

/// Look up a configured model's router role by name, for callers that need to
/// validate a user-selected model before routing (e.g. the Cortex node's
/// Image mode). `None` means no model with that name exists at all — distinct
/// from `Some("")` (an existing general-pool model).
pub async fn model_role_by_name(router: &SharedRouter, name: &str) -> Option<String> {
    let g = router.lock().await;
    g.models
        .iter()
        .find(|m| m.name == name)
        .map(|m| m.role.clone())
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

        // Role fidelity: a sticky model from a different pool must not shadow
        // the requested role while that role has available models (e.g. a
        // general free model staying sticky into a complex_tasks turn once a
        // strong model is configured). Same-role stickiness is preserved —
        // that's what keeps multi-step runs on one model.
        let shadowed_by_role = if !role.is_empty() && role != "paid_model" {
            let g = router.lock().await;
            let sticky_in_role = g
                .models
                .iter()
                .any(|m| m.name == sticky_name && m.role == role);
            !sticky_in_role
                && g.models
                    .iter()
                    .any(|m| m.role == role && m.enabled && m.is_available())
        } else {
            false
        };
        if shadowed_by_role {
            tracing::info!(
                "Sticky model '{}' skipped this turn: requested role '{}' has available models",
                sticky_name,
                role
            );
        }

        if !already_tried && !shadowed_by_role {
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

    if role == "image_model" {
        // Vision requests must never silently fall back to a text-only
        // general/paid model — at best that produces a confusing provider
        // error, at worst (a provider adapter that drops images) it silently
        // answers as if the image was never sent. Fail clearly instead.
        anyhow::bail!(
            "No available model tagged role=\"image_model\" could serve this request \
             (none configured, or all configured image_model entries are \
             disabled/rate-limited/erroring). Add or fix a vision-capable model \
             with role = \"image_model\" on the Models page."
        );
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
        for (mi, name, tokens) in sweep_models {
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

    for (mi, name, tokens) in ordered_models {
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
    threshold: u32,
    default_timeout_secs: u64,
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

    // Flat per-attempt timeout: the model's own `timeout_secs` if set, else the
    // global default (router.model_call_timeout_secs, default 30s). No adaptive
    // or fair-share math — a model either answers within this window or we fail
    // over immediately to the next one, bounded only by the overall run deadline.
    let flat_timeout_secs = timeout_secs.max(1);

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

        let attempt_timeout = if let Some(deadline) = options.deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                anyhow::bail!("Request budget exhausted before model call");
            }
            // Cap the flat timeout by whatever run budget is left so the last
            // attempt before the run deadline doesn't overrun it.
            Duration::from_secs(flat_timeout_secs.min(remaining.as_secs().max(1)))
        } else {
            Duration::from_secs(flat_timeout_secs)
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
                g.models[idx].mark_rate_limited(&hint);
            } else {
                g.models[idx].mark_error(threshold);
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
            // Leave consecutive_rate_limits as-is: it's telemetry-only now (cooldowns
            // are window-based, not escalated by it) and is most useful kept until a
            // genuine success (mark_success) confirms the model actually recovered.
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

/// Actively probe every configured model with a tiny real completion and report
/// which ones actually work *right now*. Unlike `get_status` — which only echoes
/// the cached runtime health — this sends a live one-line "ping" to each
/// provider, so a bad API key, wrong base_url or unreachable endpoint surfaces
/// immediately instead of only after the model is used in anger.
///
/// Read-only with respect to the router: models are cloned out under the lock and
/// each probe runs on its own clone, so live status/telemetry is never mutated by
/// a health check. All probes run concurrently. The result keeps the top-level
/// `checked` / `summary` / `by_status` shape, with `by_status.healthy` a flat list
/// and `by_status.unhealthy` split into the fixed `FAILURE_CATEGORIES` buckets
/// (`rate_limited`, `payment_required`, `invalid_key`, … — see
/// `classify_health_error`); `summary` mirrors that nesting with counts. Every
/// list is sorted alphabetically by name and `api_key` is never included.
///
/// The probe is a faithful copy of a real call: the API key is resolved through
/// `settings.resolve` (`${VAR}` → DB/env) exactly like the live router, and only
/// the primary `model_id` is used — otherwise every model would get its raw
/// `${VAR}` placeholder and 401, reporting healthy models as unhealthy.
pub async fn health_check(router: &SharedRouter, settings: &RuntimeSettings) -> serde_json::Value {
    // Snapshot the model set, then release the lock before any network I/O.
    let models: Vec<ModelRecord> = {
        let g = router.lock().await;
        g.models.clone()
    };

    // Prepare each probe synchronously: resolve the API key the same way the live
    // router does and pick the primary model_id. Fail-fast prechecks mirror the
    // router's own guardrails so an unresolved/empty key reports a clear reason
    // instead of a doomed 401 round-trip.
    let prepared: Vec<(ModelRecord, Result<(), String>)> = models
        .into_iter()
        .map(|mut m| {
            let resolved_key = settings.resolve(&m.api_key);
            m.model_id = m
                .model_id
                .split(',')
                .map(|s| s.trim())
                .find(|s| !s.is_empty())
                .unwrap_or("")
                .to_string();
            let precheck = if resolved_key.trim().is_empty() {
                Err(
                    "no API key after resolution (check AXON_MASTER_KEY / provider env vars)"
                        .to_string(),
                )
            } else if resolved_key.starts_with("${") && resolved_key.ends_with('}') {
                Err(format!(
                    "unresolved API key placeholder {resolved_key} (check .env / server env)"
                ))
            } else if m.model_id.is_empty() {
                Err("model has no model_id".to_string())
            } else {
                m.api_key = resolved_key;
                Ok(())
            };
            (m, precheck)
        })
        .collect();

    // Probe all models concurrently, each on its own clone. A minimal, cheap call
    // — one short user turn, no tools, capped at a few tokens: success means the
    // provider accepted our credentials and returned a valid response; any error
    // (401, wrong endpoint, timeout, unreachable) means unhealthy.
    let probes = prepared.into_iter().map(|(mut m, precheck)| async move {
        let started = Instant::now();
        let result = match precheck {
            Err(e) => Err(e),
            Ok(()) => call_provider_with_options(
                &mut m,
                &[Message::user("ping")],
                "",
                &[],
                16,
                ProviderCallOptions::default(),
            )
            .await
            .map_err(|e| e.to_string()),
        };
        let latency_ms = started.elapsed().as_millis() as u64;

        let mut entry = serde_json::json!({
            "name": m.name,
            "provider": m.provider,
            "model_id": m.model_id,
            "enabled": m.enabled,
            "latency_ms": latency_ms,
        });
        // A success lands in the single `healthy` bucket. A failure is
        // sub-classified from the error text into one of the fixed
        // FAILURE_CATEGORIES (rate_limited, payment_required, invalid_key, …) so a
        // workflow can react to *why* a model is down, not just that it is. The
        // verbatim provider error stays on the entry, so bucketing hides nothing.
        let category = match result {
            Ok(_) => "healthy",
            Err(e) => {
                let category = classify_health_error(&e);
                entry["error"] = serde_json::json!(e);
                category
            }
        };
        (category, entry)
    });

    let results = futures::future::join_all(probes).await;

    // Keep the original output shape — top-level `checked`, `summary` and
    // `by_status.{healthy,unhealthy}` — but split the single `unhealthy` list into
    // fixed, always-present subcategory buckets (both under `by_status.unhealthy`
    // and `summary.unhealthy`), so the schema a workflow branches on never shifts
    // with the run: `by_status.unhealthy.rate_limited` is always a valid array,
    // empty when nothing hit that reason. `healthy` stays a flat list. Entries keep
    // their original fields — the bucket name is the category, so it isn't repeated
    // on the entry.
    let checked = results.len();
    let mut healthy_list: Vec<serde_json::Value> = Vec::new();
    // Seed every failure bucket up front so absent reasons still appear as an empty
    // list / zero count. BTreeMap also keeps the buckets in a stable, sorted order.
    let mut unhealthy_groups: std::collections::BTreeMap<&'static str, Vec<serde_json::Value>> =
        FAILURE_CATEGORIES
            .iter()
            .map(|c| (*c, Vec::new()))
            .collect();
    for (category, entry) in results {
        if category == "healthy" {
            healthy_list.push(entry);
        } else {
            // classify_health_error only ever returns a FAILURE_CATEGORIES member,
            // so this always lands in a seeded bucket; or_default is just belt-and-braces.
            unhealthy_groups.entry(category).or_default().push(entry);
        }
    }

    // Sort every list alphabetically (case-insensitive) by name.
    fn sort_by_name(entries: &mut [serde_json::Value]) {
        entries.sort_by(|a, b| {
            let an = a["name"].as_str().unwrap_or("").to_lowercase();
            let bn = b["name"].as_str().unwrap_or("").to_lowercase();
            an.cmp(&bn)
        });
    }
    sort_by_name(&mut healthy_list);
    for entries in unhealthy_groups.values_mut() {
        sort_by_name(entries);
    }

    // Per-subcategory counts, mirroring the by_status nesting.
    let unhealthy_summary: std::collections::BTreeMap<&'static str, usize> = unhealthy_groups
        .iter()
        .map(|(cat, entries)| (*cat, entries.len()))
        .collect();

    serde_json::json!({
        "checked": checked,
        "summary": {
            "healthy": healthy_list.len(),
            "unhealthy": unhealthy_summary,
        },
        "by_status": {
            "healthy": healthy_list,
            "unhealthy": unhealthy_groups,
        },
    })
}

/// The fixed, exhaustive set of failure buckets a probe can land in. Kept as a
/// constant so `by_status.unhealthy` / `summary.unhealthy` always carry the same
/// keys (empty when unused) and a workflow branching on e.g.
/// `by_status.unhealthy.rate_limited` never hits a missing key. Every non-`healthy`
/// return of `classify_health_error` MUST be one of these.
const FAILURE_CATEGORIES: &[&str] = &[
    "rate_limited",
    "payment_required",
    "invalid_key",
    "forbidden",
    "not_found",
    "bad_request",
    "server_error",
    "timeout",
    "unreachable",
    "misconfigured",
    "error",
];

/// Map a probe's error message to a fixed failure category (a `FAILURE_CATEGORIES`
/// member). The probe only has the provider's error *string* — each provider
/// formats its own (see `anthropic.rs` / `openai_compat.rs`), and both frame HTTP
/// failures with the reason phrase ("401 Unauthorized", "402 Payment Required", …),
/// so we match on those phrases rather than the bare status number: a stray "402"
/// inside a 404's request-id can't misfile it. Matching is case-insensitive
/// substring, ordered most-specific first. The raw error is preserved on the entry
/// regardless, so this only adds a grouping key — it never hides detail. Anything
/// unrecognized falls through to `error`.
fn classify_health_error(err: &str) -> &'static str {
    let e = err.to_ascii_lowercase();

    // Local prechecks from health_check() above — a config problem, no round-trip
    // was ever made. These strings are ours and unambiguous, so match them first.
    if e.contains("no api key after resolution")
        || e.contains("unresolved api key")
        || e.contains("no model_id")
    {
        return "misconfigured";
    }
    // 429 / throttling. Both providers frame these as "rate limit: …"; Gemini
    // surfaces the same as a RESOURCE_EXHAUSTED quota. Checked before the billing
    // buckets because free-tier 429s often also nag about adding credits.
    if e.contains("rate limit")
        || e.contains("too many requests")
        || e.contains("resource_exhausted")
    {
        return "rate_limited";
    }
    // 402 / out of credits or a key's spend cap reached — the key is valid, the
    // account just can't pay for the call.
    if e.contains("payment required")
        || e.contains("insufficient credits")
        || e.contains("requires more credits")
        || e.contains("key limit exceeded")
    {
        return "payment_required";
    }
    // 401 / bad or missing credentials. "valid api key" also catches providers
    // (Gemini) that report a bad key as a 400 INVALID_ARGUMENT "pass a valid API key".
    if e.contains("unauthorized")
        || e.contains("invalid token")
        || e.contains("invalid api key")
        || e.contains("invalid_api_key")
        || e.contains("valid api key")
        || e.contains("authentication")
    {
        return "invalid_key";
    }
    // 403 / authenticated but not permitted to use this model.
    if e.contains("forbidden")
        || e.contains("not allowed")
        || e.contains("permission")
        || e.contains("access denied")
    {
        return "forbidden";
    }
    // 404 / wrong model_id or base_url, or a model that's been retired.
    if e.contains("not found")
        || e.contains("no endpoints found")
        || e.contains("does not exist")
        || e.contains("no longer available")
    {
        return "not_found";
    }
    // 400 / malformed request or unsupported parameter.
    if e.contains("bad request") || e.contains("unsupported") || e.contains("invalid_argument") {
        return "bad_request";
    }
    // 5xx / provider-side outage. "529" is Anthropic's non-standard overload code,
    // which has no reason phrase to match, so keep the number here.
    if e.contains("internal server error")
        || e.contains("service unavailable")
        || e.contains("bad gateway")
        || e.contains("gateway timeout")
        || e.contains("overloaded")
        || e.contains("529")
    {
        return "server_error";
    }
    // Request made but no timely response.
    if e.contains("timed out") || e.contains("timeout") || e.contains("deadline") {
        return "timeout";
    }
    // Never reached the host (DNS / connect / TLS transport failure).
    if e.contains("dns")
        || e.contains("failed to lookup")
        || e.contains("trying to connect")
        || e.contains("connection refused")
        || e.contains("connection reset")
        || e.contains("unreachable")
    {
        return "unreachable";
    }
    "error"
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

#[cfg(test)]
mod tests {
    use super::{classify_health_error, FAILURE_CATEGORIES};

    // Fixtures are verbatim (trimmed) provider errors seen in a real Health Check
    // run, so the classifier is pinned to the wording it actually receives.

    #[test]
    fn classifies_rate_limits() {
        // Both providers frame a 429 as "rate limit: …".
        assert_eq!(
            classify_health_error("rate limit: {\"status\":429,\"title\":\"Too Many Requests\"}"),
            "rate_limited"
        );
        // Gemini's quota-exhausted 429, still framed with the "rate limit" prefix.
        assert_eq!(
            classify_health_error(
                "rate limit: [{ \"error\": { \"code\": 429, \"message\": \"You exceeded your current quota\", \"status\": \"RESOURCE_EXHAUSTED\" }}]"
            ),
            "rate_limited"
        );
        // Free-tier 429 that also nags about credits must stay rate_limited, not payment.
        assert_eq!(
            classify_health_error(
                "rate limit: {\"error\":{\"message\":\"Rate limit exceeded: free-models-per-day. Add 10 credits to unlock 1000 free model requests per day\",\"code\":429}}"
            ),
            "rate_limited"
        );
    }

    #[test]
    fn classifies_billing_and_credentials() {
        assert_eq!(
            classify_health_error(
                "provider error 402 Payment Required at https://x/v1: {\"error\":{\"message\":\"Insufficient credits. This account never purchased credits.\",\"code\":402}}"
            ),
            "payment_required"
        );
        // A 403 that is really a spend cap, not an access problem.
        assert_eq!(
            classify_health_error(
                "provider error 403 Forbidden at https://x/v1: {\"error\":{\"message\":\"Key limit exceeded (total limit).\",\"code\":403}}"
            ),
            "payment_required"
        );
        assert_eq!(
            classify_health_error(
                "provider error 401 Unauthorized at https://x/v1: {\"error\":{\"message\":\"Invalid token\"}}"
            ),
            "invalid_key"
        );
        // Gemini reports a bad key as a 400 INVALID_ARGUMENT, not a 401.
        assert_eq!(
            classify_health_error(
                "provider error 400 Bad Request at https://.../openai/chat/completions: [{ \"error\": { \"code\": 400, \"message\": \"Please pass a valid API key\", \"status\": \"INVALID_ARGUMENT\" }}]"
            ),
            "invalid_key"
        );
        // A genuine access denial stays distinct from a bad key.
        assert_eq!(
            classify_health_error(
                "provider error 403 Forbidden at https://x/v1: {\"error\":{\"message\":\"You are not allowed to sample from this model\"}}"
            ),
            "forbidden"
        );
    }

    #[test]
    fn classifies_availability_and_requests() {
        assert_eq!(
            classify_health_error(
                "provider error 404 Not Found at https://x/v1: {\"error\":{\"message\":\"No endpoints found for baidu/cobuddy:free.\",\"code\":404}}"
            ),
            "not_found"
        );
        assert_eq!(
            classify_health_error(
                "provider error 404 Not Found at https://x/v1: {\"error\":{\"message\":\"Ling-2.6-1T is no longer available as a free model.\",\"code\":404}}"
            ),
            "not_found"
        );
        // A 503 whose body mentions model_not_found (underscore) is still an outage.
        assert_eq!(
            classify_health_error(
                "provider error 503 Service Unavailable at https://x/v1: {\"error\":{\"code\":\"model_not_found\",\"message\":\"No available channel for model claude-opus-4-7\"}}"
            ),
            "server_error"
        );
        assert_eq!(
            classify_health_error("anthropic 529 Overloaded: {\"error\":\"overloaded_error\"}"),
            "server_error"
        );
        assert_eq!(
            classify_health_error(
                "provider error 400 Bad Request at https://x/v1: unsupported parameter"
            ),
            "bad_request"
        );
    }

    #[test]
    fn classifies_local_prechecks_and_transport() {
        assert_eq!(
            classify_health_error(
                "no API key after resolution (check AXON_MASTER_KEY / provider env vars)"
            ),
            "misconfigured"
        );
        assert_eq!(
            classify_health_error(
                "unresolved API key placeholder ${CEREBRAS_API_KEY_ORAPAGIER} (check .env / server env)"
            ),
            "misconfigured"
        );
        assert_eq!(
            classify_health_error("model has no model_id"),
            "misconfigured"
        );
        assert_eq!(
            classify_health_error("HTTP to https://x/v1: operation timed out"),
            "timeout"
        );
        assert_eq!(
            classify_health_error(
                "Anthropic request: error trying to connect: dns error: failed to lookup address"
            ),
            "unreachable"
        );
        assert_eq!(
            classify_health_error("something totally unexpected"),
            "error"
        );
    }

    #[test]
    fn every_category_is_a_declared_bucket() {
        // Guards the invariant health_check() relies on: classify_health_error only
        // ever returns a FAILURE_CATEGORIES member, so every result lands in a seeded
        // bucket. Sample the fixtures above plus the fallback.
        for err in [
            "rate limit: too many requests",
            "provider error 402 Payment Required: insufficient credits",
            "provider error 401 Unauthorized: invalid token",
            "provider error 403 Forbidden: you are not allowed",
            "provider error 404 Not Found: no endpoints found",
            "provider error 400 Bad Request: unsupported",
            "provider error 503 Service Unavailable",
            "operation timed out",
            "error trying to connect: dns error",
            "unresolved API key placeholder ${X}",
            "something totally unexpected",
        ] {
            let cat = classify_health_error(err);
            assert!(
                FAILURE_CATEGORIES.contains(&cat),
                "category {cat:?} for {err:?} is not in FAILURE_CATEGORIES"
            );
        }
    }
}
