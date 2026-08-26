use crate::config::RuntimeSettings;
use crate::providers::{call_provider_with_options, types::*, ProviderCallOptions};
use crate::tools::schema::ToolDefinition;
use once_cell::sync::Lazy;
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
    /// Resolve a model by `name` — the `models` table primary key, and the only
    /// stable identifier a router entry has.
    ///
    /// Routing picks its candidates under one lock and then makes the (slow)
    /// provider call with the lock released, so a positional index goes stale
    /// the moment the model set changes underneath it: `update_models` replaces
    /// the whole vector and re-sorts it by priority, and the dashboard calls it
    /// on every add/update/delete. A stale index panics outright once the list
    /// has shrunk, and silently books one model's success/error stats onto
    /// another once the list has merely been re-ordered. Every access made after
    /// the lock is re-acquired therefore goes through the name instead, and
    /// treats "no longer present" as an ordinary failure to fail over from.
    fn find(&self, name: &str) -> Option<&ModelRecord> {
        self.models.iter().find(|m| m.name == name)
    }

    fn find_mut(&mut self, name: &str) -> Option<&mut ModelRecord> {
        self.models.iter_mut().find(|m| m.name == name)
    }

    fn pool_indices(&self, role: &str) -> Vec<usize> {
        self.models
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == role && m.enabled)
            .map(|(i, _)| i)
            .collect()
    }
    /// The untagged pool every chat call falls back to.
    ///
    /// A speech model is excluded even when its role is blank. The Models page
    /// tags `role = "tts"` for you when you pick a speech provider, but
    /// `models.toml` and the Homeostasis Upsert node can both write a row
    /// without one — and such a row would otherwise sit in the *general* chat
    /// pool, get POSTed a `/chat/completions` body its host has no route for,
    /// and bank a cooldown that takes a healthy speech key out of the TTS pool.
    fn general_pool(&self) -> Vec<usize> {
        self.models
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role.is_empty() && m.enabled && !is_speech_model(m))
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
                // `start` is reduced BEFORE the add, not after. It is a
                // full-width FNV-1a hash of the run id (see
                // `agent::loop::stable_route_seed`), so it is uniform over all
                // of usize: a plain `start + offset` overflows (a panic under
                // the overflow checks dev and test builds enable), and a
                // `wrapping_add` silently stops being a permutation near the
                // boundary — with 3 buckets and `start = usize::MAX`, offsets 0
                // and 1 both land on bucket 0, so one model is tried twice and
                // another never at all. Reducing first keeps the sum below
                // `2 * bucket_count`, so it is exact and cannot overflow.
                let key = &bucket_keys[(start % bucket_count + offset) % bucket_count];
                if let Some(bucket) = buckets.get(key) {
                    if round < bucket.len() {
                        // Rotate the starting key *within* the bucket too, keyed
                        // on `start`. Multiple API keys for the same provider +
                        // model land in one bucket; without this, key #0 is
                        // always tried first and gets hammered until it 429s
                        // while the others sit idle. `(start + round) % len` is a
                        // permutation over the bucket, so all keys are still
                        // covered with no duplicates.
                        order.push(bucket[(start % bucket.len() + round) % bucket.len()]);
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
    // Tracks every model actually attempted across all passes, by name.
    // Fed to the sweep pass (1.5) for precise dedup. Keyed on the name rather
    // than the position because the model set can be replaced between passes
    // (see `RouterState::find`), which would re-point every stored index.
    let mut attempted: std::collections::HashSet<String> = std::collections::HashSet::new();

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
        let preferred_models: Vec<(String, u32)> = {
            let g = router.lock().await;
            g.models
                .iter()
                .filter(|m| m.name == pref_name && m.enabled && m.is_available())
                .map(|m| (m.name.clone(), max_tokens.unwrap_or(m.max_tokens)))
                .collect()
        };

        for (name, tokens) in preferred_models {
            attempted.insert(name.clone());
            match try_call(
                &name,
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
            let sticky_models: Vec<(String, u32)> = {
                let g = router.lock().await;
                g.models
                    .iter()
                    .filter(|m| m.name == sticky_name && m.enabled && m.is_available())
                    .map(|m| (m.name.clone(), max_tokens.unwrap_or(m.max_tokens)))
                    .collect()
            };

            for (name, tokens) in sticky_models {
                attempted.insert(name.clone());
                match try_call(
                    &name,
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
        let ordered_models: Vec<(String, u32)> = {
            let g = router.lock().await;
            let pool = g.pool_indices(role);
            let order = build_priority_order(&g.models, &pool, start_index);
            order
                .into_iter()
                .map(|mi| {
                    let m = &g.models[mi];
                    (m.name.clone(), max_tokens.unwrap_or(m.max_tokens))
                })
                // Skip models already tried in the preferred/sticky passes — no
                // point re-hitting a just-failed endpoint within the same call.
                .filter(|(name, _)| !attempted.contains(name))
                .collect()
        };

        for (name, tokens) in ordered_models {
            attempted.insert(name.clone());
            match try_call(
                &name,
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
    let ordered_models: Vec<(String, u32)> = {
        let g = router.lock().await;
        let pool = g.general_pool();
        let order = build_priority_order(&g.models, &pool, start_index);
        order
            .into_iter()
            .map(|mi| {
                let m = &g.models[mi];
                (m.name.clone(), max_tokens.unwrap_or(m.max_tokens))
            })
            // Skip models already attempted in earlier passes (preferred,
            // sticky, role) so each model is tried at most once per call.
            .filter(|(name, _)| !attempted.contains(name))
            .collect()
    };

    for (name, tokens) in ordered_models {
        attempted.insert(name.clone());
        match try_call(
            &name,
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
    // exhausted. We use `attempted` — the set of every model name actually
    // called in prior passes — for precise dedup.
    //
    // "Regardless of role" stops at models that cannot answer a chat completion
    // at all. A `role = "tts"` entry (or an ElevenLabs-provider one) would be
    // POSTed a `/chat/completions` body it has no route for, fail, and bank a
    // cooldown on a model that was perfectly healthy for the job it *is* for —
    // taking a speech key out of the pool because a chat request went looking.
    {
        let sweep_models: Vec<(String, u32)> = {
            let g = router.lock().await;
            g.models
                .iter()
                .filter(|m| {
                    m.enabled
                        && m.is_available()
                        && m.role != "paid_model"
                        && !is_speech_model(m)
                        && !attempted.contains(&m.name)
                })
                .map(|m| (m.name.clone(), max_tokens.unwrap_or(m.max_tokens)))
                .collect()
        };
        for (name, tokens) in sweep_models {
            attempted.insert(name.clone());
            match try_call(
                &name,
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
    let ordered_models: Vec<(String, u32)> = {
        let g = router.lock().await;
        let pool = g.pool_indices("paid_model");
        let order = build_priority_order(&g.models, &pool, start_index);
        order
            .into_iter()
            .map(|mi| {
                let m = &g.models[mi];
                (m.name.clone(), max_tokens.unwrap_or(m.max_tokens))
            })
            .collect()
    };

    for (name, tokens) in ordered_models {
        match try_call(
            &name,
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
                    g.find(&name)
                        .map(|m| m.model_id.clone())
                        .unwrap_or_default()
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
    model_name: &str,
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
        // Looked up by name, not by the index the caller selected under an
        // earlier lock: the model set can be replaced between the two (see
        // `RouterState::find`). A model that vanished is just another candidate
        // to fail over from.
        let Some(m) = g.find(model_name) else {
            anyhow::bail!("model '{model_name}' is no longer configured");
        };
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
                if let Some(m) = g.find_mut(model_name) {
                    m.mark_success(resp.usage.input_tokens, resp.usage.output_tokens);
                    m.rl_snapshot = tmp.rl_snapshot;
                }
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
        let consecutive = {
            let mut g = router.lock().await;
            match g.find_mut(model_name) {
                Some(m) => {
                    if is_rl {
                        let hint = parse_rate_limit_hint(&e.to_string());
                        m.mark_rate_limited(&hint);
                    } else {
                        m.mark_error(threshold);
                    }
                    m.consecutive_errors
                }
                // Deleted mid-call: nothing left to book the failure against.
                None => 0,
            }
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
                model_name: model_name.to_string(),
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

/// Call one specific configured model for image generation, with the same
/// api-key resolution, comma-separated model_id failover, and
/// success/rate-limit/error bookkeeping as `try_call` — but no pool routing:
/// image generation is only ever run on an explicitly user-selected model
/// (the Cortex node validates role = "image_model" before calling this).
///
/// The per-attempt timeout is the model's own `timeout_secs` but never less
/// than 120s: chat-tuned timeouts (15–45s) are too short for image synthesis.
pub async fn generate_image_with_model(
    router: &SharedRouter,
    settings: &RuntimeSettings,
    model_name: &str,
    prompt: &str,
    input_image: Option<&ContentBlock>,
) -> anyhow::Result<GeneratedImage> {
    let threshold = settings.error_threshold();
    let (base_record, api_key, timeout_secs) = {
        let g = router.lock().await;
        let m = g
            .find(model_name)
            .ok_or_else(|| anyhow::anyhow!("model '{}' was not found", model_name))?;
        if !m.is_available() {
            anyhow::bail!(
                "model '{}' is currently unavailable (disabled, rate-limited, or erroring); \
                 check the Models page",
                model_name
            );
        }
        let resolved_key = settings.resolve(&m.api_key);
        (
            m.clone(),
            resolved_key,
            m.timeout_secs.unwrap_or(0).max(120),
        )
    };

    if api_key.trim().is_empty() {
        anyhow::bail!(
            "Model '{}' has no API key after resolution; check AXON_MASTER_KEY and provider \
             environment variables on the server",
            base_record.name
        );
    }
    if api_key.starts_with("${") && api_key.ends_with('}') {
        anyhow::bail!(
            "Model '{}' has unresolved API key placeholder {}; check .env loading or server \
             environment configuration",
            base_record.name,
            api_key
        );
    }

    let model_ids: Vec<String> = base_record
        .model_id
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if model_ids.is_empty() {
        anyhow::bail!("Model '{}' has no model IDs configured", base_record.name);
    }

    let mut last_error: Option<(anyhow::Error, bool)> = None; // (error, is_rate_limit)
    for current_model_id in &model_ids {
        let mut tmp = base_record.clone();
        tmp.model_id = current_model_id.clone();
        tmp.api_key = api_key.clone();
        tracing::info!("→ {} (image generation)", current_model_id);

        let call_result = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            crate::providers::generate_image_with_provider(&mut tmp, prompt, input_image),
        )
        .await;

        match call_result {
            Ok(Ok(img)) => {
                let mut g = router.lock().await;
                if let Some(m) = g.find_mut(model_name) {
                    m.mark_success(img.usage.input_tokens, img.usage.output_tokens);
                    m.rl_snapshot = tmp.rl_snapshot;
                }
                tracing::info!(
                    "✓ {} generated {} bytes ({})",
                    current_model_id,
                    img.bytes.len(),
                    img.mime_type
                );
                return Ok(img);
            }
            Ok(Err(e)) => {
                tracing::warn!("✗ {} image generation failed: {}", current_model_id, e);
                let s = e.to_string().to_lowercase();
                let is_rl = s.contains("rate limit") || s.contains("429") || s.contains("quota");
                last_error = Some((e, is_rl));
            }
            Err(_elapsed) => {
                last_error = Some((
                    anyhow::anyhow!("image generation timed out after {}s", timeout_secs),
                    false,
                ));
            }
        }
    }

    let (e, is_rl) = last_error.expect("model_ids is non-empty, so at least one attempt ran");
    {
        let mut g = router.lock().await;
        if let Some(m) = g.find_mut(model_name) {
            if is_rl {
                let hint = parse_rate_limit_hint(&e.to_string());
                m.mark_rate_limited(&hint);
            } else {
                m.mark_error(threshold);
            }
        }
    }
    Err(e)
}

/// Router role that marks a model as a speech synthesizer rather than a chat
/// model. Kept out of every chat pool and out of the chat-shaped health probe.
pub const TTS_ROLE: &str = "tts";

/// True for a model that synthesizes speech and therefore cannot serve a chat
/// completion. Matched on the role *or* the provider so a row that was tagged
/// one way but not the other is still kept out of the chat paths.
pub fn is_speech_model(m: &ModelRecord) -> bool {
    m.role == TTS_ROLE || is_speech_provider(&m.provider)
}

/// How many models one utterance may burn before giving up. Speech providers
/// bill per *character of input*, and a failover re-sends the whole sentence —
/// so sweeping a ten-key pool on a sentence that fails for a non-key reason
/// (bad voice id, malformed text) bills that sentence ten times. Three attempts
/// is enough to ride out a rate-limited key without turning one bad request
/// into a ten-fold charge.
const TTS_MAX_ATTEMPTS: usize = 3;

/// How long the pool stays pinned to the model that last spoke.
///
/// Both clients synthesize a reply *sentence by sentence* (ChatPage's
/// `StreamingSpeech`, the Android `StreamingTts`) — each sentence is a separate
/// request. Rotating per request would therefore change the speaker partway
/// through a single answer: sentence one in Rachel, sentence two in Adam. So
/// the pool is sticky by default and only rotates when a model actually fails,
/// which is also exactly the behavior asked for ("rotate through them when one
/// hits a rate limit"). The window is generous enough to span the gap between
/// consecutive sentences of one reply and short enough that a later, unrelated
/// reply starts fresh.
const TTS_STICKY_SECS: u64 = 90;

/// The model that last synthesized successfully, and when. See
/// [`TTS_STICKY_SECS`] — this is what keeps one spoken reply in one voice.
/// Process-global on purpose: when the dashboard and the phone are both
/// listening, they should hear the same voice, not two.
static TTS_STICKY: Lazy<Mutex<Option<(String, Instant)>>> = Lazy::new(|| Mutex::new(None));

/// Speech synthesized through the pool, plus which model produced it.
pub struct TtsRoute {
    pub audio: crate::tts::SpeechAudio,
    pub model_name: String,
}

/// Build the `tts::TtsConfig` a single router model describes. `voice` falls
/// back to the global `tts.voice` when the row leaves it blank, so a pool added
/// on top of an existing TTS install keeps the voice it already spoke in.
fn tts_config_for(
    model: &ModelRecord,
    model_id: &str,
    api_key: &str,
    settings: &RuntimeSettings,
) -> crate::tts::TtsConfig {
    let base_url = model
        .base_url
        .clone()
        .filter(|b| !b.trim().is_empty())
        .or_else(|| provider_base_url(&model.provider).map(str::to_string))
        .unwrap_or_default();
    let voice = model
        .voice
        .clone()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| settings.resolve(&settings.get_str("tts.voice", "")));
    crate::tts::TtsConfig {
        base_url: base_url.trim().trim_end_matches('/').to_string(),
        model: model_id.to_string(),
        voice: voice.trim().to_string(),
        api_key: api_key.to_string(),
        // Piper is a local binary with no key and no provider — it is
        // configured through `tts.*` only and never appears as a router model.
        piper: crate::tts::PiperOptions::default(),
    }
}

/// Translate a synthesis failure into the router's health bookkeeping.
///
/// Speech providers do not use HTTP status codes the way the chat classifier
/// assumes, and ElevenLabs is the sharp edge: an exhausted *character quota*
/// comes back as `401`, which the generic path reads as "bad credentials".
/// Mishandled, a key that would recover at the next billing reset gets retired
/// permanently by Homeostasis auto-disable instead. See
/// [`crate::tts::elevenlabs_failure_kind`].
fn record_tts_failure(model: &mut ModelRecord, err: &anyhow::Error, threshold: u32) {
    use crate::tts::ElevenLabsFailure;
    let text = err.to_string();
    match crate::tts::elevenlabs_failure_kind(&text) {
        // Characters spent. Park it for the day rather than burning the key on
        // a retry every few seconds; a monthly quota is not coming back sooner.
        ElevenLabsFailure::QuotaExceeded => {
            model.mark_rate_limited(&RateLimitHint {
                window: RateLimitWindow::Daily,
                explicit_secs: None,
            });
        }
        // Concurrency cap (2 on free, 5 on Creator, …) or upstream congestion.
        // Clears in seconds, so a full-minute bench would be self-inflicted.
        ElevenLabsFailure::Transient => {
            model.mark_rate_limited(&RateLimitHint {
                window: RateLimitWindow::PerMinute,
                explicit_secs: Some(2),
            });
        }
        ElevenLabsFailure::Other => {
            let lower = text.to_ascii_lowercase();
            if lower.contains("rate limit")
                || lower.contains("429")
                || lower.contains("too many requests")
                || lower.contains("quota")
            {
                model.mark_rate_limited(&parse_rate_limit_hint(&text));
            } else {
                model.mark_error(threshold);
            }
        }
    }
}

/// Synthesize speech through the `role = "tts"` model pool, rotating to the
/// next model when one is rate-limited, out of quota, or erroring.
///
/// Shares the chat router's machinery rather than reimplementing it: the same
/// `build_priority_order` (which round-robins across priority tiers *and*
/// within a `(provider, base_url, model_id)` bucket, so N keys for one model
/// get spread instead of key #0 being hammered), the same `${VAR}` key
/// resolution, the same comma-separated `model_id` failover, and the same
/// `mark_success` / `mark_rate_limited` / `mark_error` health bookkeeping that
/// the Models page and Homeostasis already read.
///
/// It differs from the chat path in three ways, each for a reason particular to
/// speech:
///   * **Sticky by default** ([`TTS_STICKY_SECS`]) — a reply is synthesized one
///     sentence per request, so per-request rotation would change the speaker
///     mid-answer.
///   * **Capped attempts** ([`TTS_MAX_ATTEMPTS`]) — retries re-bill the whole
///     sentence.
///   * **A 120s timeout floor** — chat-tuned `timeout_secs` (15–45s) are too
///     short for synthesis, the same adjustment `generate_image_with_model`
///     makes for images.
///
/// `Ok(None)` means the pool is empty — no `role = "tts"` model is configured
/// at all — which is the caller's cue to fall back to the `tts.*` settings.
/// `Err` means the pool exists but every candidate failed.
pub async fn speak_with_router(
    router: &SharedRouter,
    settings: &RuntimeSettings,
    text: &str,
) -> anyhow::Result<Option<TtsRoute>> {
    let threshold = settings.error_threshold();

    // Nothing speakable in this payload (an all-emoji reply, a bare code
    // fence). `tts::speak` would reject it, and routing that rejection through
    // the failure bookkeeping below would charge a model for the caller's text
    // — a few such replies in a row would trip the consecutive-error threshold
    // and park a healthy key until midnight. Fail without touching the pool.
    if !crate::tts::has_speakable_text(text) {
        anyhow::bail!("no speakable text in this reply");
    }

    let sticky = {
        let g = TTS_STICKY.lock().await;
        g.as_ref()
            .filter(|(_, at)| at.elapsed().as_secs() < TTS_STICKY_SECS)
            .map(|(name, _)| name.clone())
    };

    // Candidate order, resolved under one lock and released before any I/O.
    let candidates: Vec<(ModelRecord, String, u64)> = {
        let mut g = router.lock().await;
        let start = g.global_index;
        g.global_index = g.global_index.wrapping_add(1);
        let pool = g.pool_indices(TTS_ROLE);
        if pool.is_empty() {
            return Ok(None);
        }
        let mut order = build_priority_order(&g.models, &pool, start);
        // Pin the last speaker to the front when it is still healthy, so one
        // reply keeps one voice. A failure below drops it and the rotation
        // proceeds from wherever `build_priority_order` put the rest.
        if let Some(name) = &sticky {
            if let Some(pos) = order.iter().position(|&mi| &g.models[mi].name == name) {
                let mi = order.remove(pos);
                order.insert(0, mi);
            }
        }
        order
            .into_iter()
            .take(TTS_MAX_ATTEMPTS)
            .map(|mi| {
                let m = &g.models[mi];
                let key = settings.resolve(&m.api_key);
                // Synthesis latency scales with text length; a chat-tuned
                // timeout would abort a perfectly healthy long sentence.
                let timeout = m.timeout_secs.unwrap_or(0).max(120);
                (m.clone(), key, timeout)
            })
            .collect()
    };

    if candidates.is_empty() {
        anyhow::bail!(
            "Every model tagged role=\"tts\" is currently unavailable (disabled, out of quota, \
             rate-limited, or erroring) — check the Models page"
        );
    }

    let mut last_error: Option<anyhow::Error> = None;
    for (model, api_key, timeout_secs) in candidates {
        if api_key.trim().is_empty() {
            last_error = Some(anyhow::anyhow!(
                "TTS model '{}' has no API key after resolution; check AXON_MASTER_KEY and \
                 provider environment variables on the server",
                model.name
            ));
            continue;
        }
        if api_key.starts_with("${") && api_key.ends_with('}') {
            last_error = Some(anyhow::anyhow!(
                "TTS model '{}' has unresolved API key placeholder {}; check .env loading or \
                 server environment configuration",
                model.name,
                api_key
            ));
            continue;
        }

        // Same comma-separated failover convention as the chat path: one row can
        // list several engines and fall through them before the pool rotates.
        let model_ids: Vec<String> = model
            .model_id
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let model_ids = if model_ids.is_empty() {
            vec![String::new()]
        } else {
            model_ids
        };

        let mut failed: Option<anyhow::Error> = None;
        for model_id in &model_ids {
            let cfg = tts_config_for(&model, model_id, &api_key, settings);
            tracing::info!("→ {} / {} (speech)", model.name, cfg.model);
            let attempt = tokio::time::timeout(
                Duration::from_secs(timeout_secs),
                crate::tts::speak(&cfg, text),
            )
            .await;
            match attempt {
                Ok(Ok(audio)) => {
                    {
                        let mut g = router.lock().await;
                        if let Some(m) = g.find_mut(&model.name) {
                            // Speech is billed per character of input, so a
                            // character count — not a token count — is the
                            // figure worth surfacing on the Models page. Booked
                            // as input usage. Counts the payload rather than the
                            // post-markdown-strip text `speak` actually sends,
                            // so it reads slightly high; it is a usage gauge,
                            // not an invoice.
                            m.mark_success(text.chars().count() as u32, 0);
                        }
                    }
                    *TTS_STICKY.lock().await = Some((model.name.clone(), Instant::now()));
                    return Ok(Some(TtsRoute {
                        audio,
                        model_name: model.name.clone(),
                    }));
                }
                Ok(Err(e)) => {
                    tracing::warn!("✗ {} / {} speech failed: {}", model.name, model_id, e);
                    failed = Some(e);
                }
                Err(_elapsed) => {
                    failed = Some(anyhow::anyhow!(
                        "speech synthesis timed out after {}s",
                        timeout_secs
                    ));
                }
            }
        }

        if let Some(e) = failed {
            let mut g = router.lock().await;
            if let Some(m) = g.find_mut(&model.name) {
                record_tts_failure(m, &e, threshold);
            }
            drop(g);
            // The pinned model just failed; let the next request rotate freely
            // rather than pinning to something that is now on cooldown.
            let mut pin = TTS_STICKY.lock().await;
            if pin.as_ref().is_some_and(|(n, _)| n == &model.name) {
                *pin = None;
            }
            last_error = Some(e);
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no TTS model could serve this request")))
}

/// Health probe for a speech model: list what the key can reach, rather than
/// synthesize with it. Costs no characters, and still surfaces the failures a
/// health check is for — a bad key, a wrong base URL, an unreachable host.
async fn probe_speech_model(m: &ModelRecord) -> Result<(), String> {
    let base_url = m
        .base_url
        .clone()
        .filter(|b| !b.trim().is_empty())
        .or_else(|| provider_base_url(&m.provider).map(str::to_string))
        .unwrap_or_default();
    if base_url.is_empty() {
        return Err("speech model has no base_url and its provider has no default".to_string());
    }
    crate::tts::list_tts_models(&base_url, &m.api_key)
        .await
        .map(|_| ())
        .map_err(|e| format!("{:#}", e))
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
                "priority": m.priority, "role": m.role, "voice": m.voice,
                "status": m.status,
                "max_tokens": m.max_tokens,
                "enabled": m.enabled,
                "disabled_reason": m.disabled_reason,
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
            // A speech model has no `/chat/completions` route, so the chat ping
            // below would report every healthy TTS key as unhealthy — and
            // Homeostasis auto-disable would then park them. Probe the identity
            // endpoint instead: it proves the credentials without synthesizing
            // anything, which matters because synthesis is billed per character
            // and a health sweep runs on a schedule.
            Ok(()) if is_speech_model(&m) => probe_speech_model(&m).await,
            Ok(()) => call_provider_with_options(
                &mut m,
                &[Message::user("ping")],
                "",
                &[],
                16,
                ProviderCallOptions::default(),
            )
            .await
            // Only success/failure matters here; discarding the body keeps this
            // arm the same `Result<(), String>` as the speech probe above.
            .map(|_| ())
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
    //
    // ElevenLabs' three shapes are folded in here for a specific reason: it
    // reports a spent character quota as a **401**, and its concurrency cap as a
    // 429 with no "rate limit" wording. Left to the buckets below, the first
    // would land in `invalid_key` — and a Homeostasis auto-disable would then
    // permanently retire a key that recovers at the next billing reset.
    if e.contains("rate limit")
        || e.contains("too many requests")
        || e.contains("resource_exhausted")
        || e.contains("quota_exceeded")
        || e.contains("too_many_concurrent_requests")
        || e.contains("system_busy")
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

/// Does this reload change how the model is actually reached? A key/endpoint
/// edit is the operator deliberately fixing a broken model, so its health state
/// should start clean rather than keep a cooldown earned by the old settings.
/// Everything else (priority, role, max_tokens, …) leaves the connection intact.
fn same_endpoint(a: &ModelRecord, b: &ModelRecord) -> bool {
    a.provider == b.provider
        && a.model_id == b.model_id
        && a.base_url == b.base_url
        && a.api_key == b.api_key
}

pub async fn update_models(router: &SharedRouter, mut new_models: Vec<ModelRecord>) {
    new_models.sort_by_key(|m| m.priority);
    let mut g = router.lock().await;

    // Records are rebuilt from the DB, which stores *configuration* only — every
    // runtime field comes back zeroed (see `config::load_models_from_db`). The
    // dashboard calls this on every add/update/delete/bulk toggle, so adopting
    // the new vector wholesale would, on each such edit, forget every model's
    // 429 quarantine (the router then immediately re-hammers providers that are
    // still rate-limited), zero the lifetime usage counters shown on the Models
    // page, and clear the `no_reasoning` flag so a param already rejected with a
    // 400 gets sent again. Carry that state across by name.
    for m in new_models.iter_mut() {
        let Some(prev) = g.find(&m.name) else {
            continue;
        };
        // Lifetime totals are cumulative telemetry: never a reason to reset.
        m.total_calls = prev.total_calls;
        m.total_input_tokens = prev.total_input_tokens;
        m.total_output_tokens = prev.total_output_tokens;

        if same_endpoint(prev, m) {
            m.status = prev.status.clone();
            m.rate_limit_reset_at = prev.rate_limit_reset_at.clone();
            m.consecutive_errors = prev.consecutive_errors;
            m.consecutive_rate_limits = prev.consecutive_rate_limits;
            m.rl_snapshot = prev.rl_snapshot.clone();
            m.no_reasoning = prev.no_reasoning;
        }
    }

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

#[cfg(test)]
mod reload_tests {
    use super::*;
    use crate::providers::types::{ModelRecord, RateLimitHint, RateLimitWindow};

    fn model(name: &str) -> ModelRecord {
        ModelRecord {
            name: name.to_string(),
            provider: "openai".into(),
            model_id: "gpt-x".into(),
            api_key: "sk-test".into(),
            base_url: None,
            timeout_secs: None,
            priority: 1,
            max_tokens: 1024,
            enabled: true,
            disabled_reason: None,
            role: String::new(),
            voice: None,
            thinking_mode: None,
            no_reasoning: false,
            status: "available".into(),
            rate_limit_reset_at: None,
            consecutive_errors: 0,
            consecutive_rate_limits: 0,
            total_calls: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            rl_snapshot: Default::default(),
        }
    }

    fn router_with(models: Vec<ModelRecord>) -> SharedRouter {
        Arc::new(Mutex::new(RouterState::new(models)))
    }

    /// A dashboard edit rebuilds every record from the DB, which stores no
    /// runtime state. Without carry-over this reload forgets the 429 cooldown
    /// and the router immediately re-hits a provider that is still limited.
    #[tokio::test]
    async fn reload_preserves_rate_limit_quarantine() {
        let router = router_with(vec![model("a"), model("b")]);
        {
            let mut g = router.lock().await;
            g.find_mut("a").unwrap().mark_rate_limited(&RateLimitHint {
                window: RateLimitWindow::Hourly,
                explicit_secs: Some(3600),
            });
            assert!(!g.find("a").unwrap().is_available());
        }

        // Unrelated edit: "b" gets a new priority, "a" is untouched.
        let mut reloaded = vec![model("a"), model("b")];
        reloaded[1].priority = 5;
        update_models(&router, reloaded).await;

        let g = router.lock().await;
        let a = g.find("a").unwrap();
        assert_eq!(a.status, "rate_limited", "cooldown survives the reload");
        assert!(a.rate_limit_reset_at.is_some());
        assert!(!a.is_available(), "still quarantined, so routing skips it");
    }

    /// Lifetime counters back the Models page usage display; a config edit is
    /// not a reason to zero them.
    #[tokio::test]
    async fn reload_preserves_usage_totals() {
        let router = router_with(vec![model("a")]);
        {
            let mut g = router.lock().await;
            g.find_mut("a").unwrap().mark_success(100, 20);
            g.find_mut("a").unwrap().mark_success(50, 10);
        }

        update_models(&router, vec![model("a")]).await;

        let g = router.lock().await;
        let a = g.find("a").unwrap();
        assert_eq!(a.total_calls, 2);
        assert_eq!(a.total_input_tokens, 150);
        assert_eq!(a.total_output_tokens, 30);
    }

    /// Re-keying or re-pointing a model is the operator fixing it, so its
    /// health state starts clean — but the lifetime totals still carry.
    #[tokio::test]
    async fn endpoint_change_clears_health_but_keeps_totals() {
        let router = router_with(vec![model("a")]);
        {
            let mut g = router.lock().await;
            let a = g.find_mut("a").unwrap();
            a.mark_success(10, 5);
            a.mark_error(3);
            a.mark_error(3);
        }

        let mut fixed = model("a");
        fixed.api_key = "sk-a-fresh-key".into();
        update_models(&router, vec![fixed]).await;

        let g = router.lock().await;
        let a = g.find("a").unwrap();
        assert_eq!(a.consecutive_errors, 0, "clean slate after the fix");
        assert_eq!(a.status, "available");
        assert_eq!(a.total_calls, 1, "telemetry is still cumulative");
    }

    /// Deleting a model must leave the survivors' state attached to the right
    /// records — the reload re-sorts and re-indexes the vector.
    #[tokio::test]
    async fn state_follows_the_model_not_its_position() {
        let router = router_with(vec![model("a"), model("b"), model("c")]);
        {
            let mut g = router.lock().await;
            g.find_mut("c").unwrap().mark_success(7, 3);
        }

        // "a" is deleted; "c" moves from index 2 to index 1.
        update_models(&router, vec![model("b"), model("c")]).await;

        let g = router.lock().await;
        assert!(g.find("a").is_none());
        assert_eq!(g.find("b").unwrap().total_calls, 0);
        assert_eq!(g.find("c").unwrap().total_input_tokens, 7);
    }
}

#[cfg(test)]
mod routing_order_tests {
    use super::*;
    use crate::providers::types::ModelRecord;

    fn m(name: &str, provider: &str, priority: i32) -> ModelRecord {
        ModelRecord {
            name: name.into(),
            provider: provider.into(),
            model_id: format!("{provider}-id"),
            api_key: "k".into(),
            base_url: None,
            timeout_secs: None,
            priority,
            max_tokens: 100,
            enabled: true,
            disabled_reason: None,
            role: String::new(),
            voice: None,
            thinking_mode: None,
            no_reasoning: false,
            status: "available".into(),
            rate_limit_reset_at: None,
            consecutive_errors: 0,
            consecutive_rate_limits: 0,
            total_calls: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            rl_snapshot: Default::default(),
        }
    }

    /// The seed is a full-width hash, so values near `usize::MAX` are ordinary
    /// inputs, not edge cases. A plain `start + offset` panics on them under the
    /// overflow checks dev and test profiles turn on; a `wrapping_add` survives
    /// but silently stops being a permutation, trying one model twice and
    /// another not at all.
    #[test]
    fn every_seed_yields_a_permutation() {
        // Distinct providers => one bucket each, which is where the modular
        // arithmetic across buckets has to hold.
        for n in 1..=5usize {
            let models: Vec<ModelRecord> = (0..n)
                .map(|i| m(&format!("m{i}"), &format!("p{i}"), 1))
                .collect();
            let pool: Vec<usize> = (0..n).collect();

            let seeds = (0..n + 2)
                .flat_map(|k| [k, usize::MAX - k])
                .chain([usize::MAX / 2, usize::MAX / 2 + 1]);

            for start in seeds {
                let order = build_priority_order(&models, &pool, start);
                assert_eq!(order.len(), n, "n={n} seed={start} lost candidates");
                let unique: std::collections::HashSet<_> = order.iter().collect();
                assert_eq!(
                    unique.len(),
                    n,
                    "n={n} seed={start} is not a permutation: {order:?}"
                );
            }
        }
    }

    /// Same check for rotation *within* one bucket (several API keys sharing a
    /// provider + model_id), which uses the other modular index.
    #[test]
    fn every_seed_permutes_within_a_bucket() {
        for n in 1..=5usize {
            // Same provider for all => a single bucket with n entries.
            let models: Vec<ModelRecord> =
                (0..n).map(|i| m(&format!("key{i}"), "shared", 1)).collect();
            let pool: Vec<usize> = (0..n).collect();

            for start in (0..n + 2).flat_map(|k| [k, usize::MAX - k]) {
                let order = build_priority_order(&models, &pool, start);
                let unique: std::collections::HashSet<_> = order.iter().collect();
                assert_eq!(
                    unique.len(),
                    n,
                    "n={n} seed={start} reused a key: {order:?}"
                );
            }
        }
    }

    /// Every available model must appear exactly once, tier by tier.
    #[test]
    fn order_is_a_permutation_respecting_priority() {
        let models = vec![
            m("hi-1", "p1", 1),
            m("hi-2", "p2", 1),
            m("lo-1", "p3", 5),
            m("lo-2", "p4", 5),
        ];
        let pool: Vec<usize> = (0..models.len()).collect();

        for start in 0..8 {
            let order = build_priority_order(&models, &pool, start);
            assert_eq!(order.len(), 4);
            let names: Vec<&str> = order.iter().map(|&i| models[i].name.as_str()).collect();
            assert!(
                names[..2].iter().all(|n| n.starts_with("hi-")),
                "priority 1 must come first, got {names:?}"
            );
        }
    }

    /// Multiple API keys for one provider+model share a bucket; the seed has to
    /// rotate within it, or key #0 gets hammered until it 429s.
    #[test]
    fn same_endpoint_keys_rotate_across_seeds() {
        // Same provider and model_id => one bucket, two entries.
        let models = vec![m("key-a", "p1", 1), m("key-b", "p1", 1)];
        let pool: Vec<usize> = (0..models.len()).collect();

        let first_of = |start: usize| build_priority_order(&models, &pool, start)[0];
        assert_ne!(
            first_of(0),
            first_of(1),
            "consecutive seeds must start on different keys"
        );
    }
}

#[cfg(test)]
mod tts_pool_tests {
    use super::*;
    use crate::providers::types::ModelRecord;

    fn speech_model(name: &str, provider: &str, role: &str) -> ModelRecord {
        ModelRecord {
            name: name.to_string(),
            provider: provider.into(),
            model_id: "eleven_multilingual_v2".into(),
            api_key: "sk-test".into(),
            base_url: None,
            timeout_secs: None,
            priority: 1,
            max_tokens: 1024,
            enabled: true,
            disabled_reason: None,
            role: role.to_string(),
            voice: None,
            thinking_mode: None,
            no_reasoning: false,
            status: "available".into(),
            rate_limit_reset_at: None,
            consecutive_errors: 0,
            consecutive_rate_limits: 0,
            total_calls: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            rl_snapshot: Default::default(),
        }
    }

    // A speech model must never be reachable from a chat pool. It has no
    // `/chat/completions` route, so a chat call routed to it fails and banks a
    // cooldown — quietly removing a healthy key from the TTS pool because an
    // unrelated chat request went looking for a fallback.
    #[test]
    fn speech_models_are_kept_out_of_the_general_chat_pool() {
        // The blank role is the case that matters: the Models page tags
        // role="tts" for you, but models.toml and the Homeostasis Upsert node
        // can both write a row without one.
        let state = RouterState::new(vec![
            speech_model("eleven-untagged", "elevenlabs", ""),
            speech_model("eleven-tagged", "elevenlabs", TTS_ROLE),
            speech_model("chat", "openai", ""),
        ]);
        let general: Vec<&str> = state
            .general_pool()
            .into_iter()
            .map(|i| state.models[i].name.as_str())
            .collect();
        assert_eq!(general, vec!["chat"]);
    }

    #[test]
    fn tagged_speech_models_form_the_tts_pool() {
        let state = RouterState::new(vec![
            speech_model("eleven-a", "elevenlabs", TTS_ROLE),
            speech_model("eleven-b", "elevenlabs", TTS_ROLE),
            speech_model("chat", "openai", ""),
        ]);
        let pool: Vec<&str> = state
            .pool_indices(TTS_ROLE)
            .into_iter()
            .map(|i| state.models[i].name.as_str())
            .collect();
        assert_eq!(pool, vec!["eleven-a", "eleven-b"]);
    }

    // The whole point of the pool: several keys for one engine must be spread,
    // not all routed to key #0 until it 429s. `build_priority_order` rotates
    // within a (provider, base_url, model_id) bucket keyed on `start`.
    #[test]
    fn many_keys_for_one_engine_rotate_across_calls() {
        let models = vec![
            speech_model("key-1", "elevenlabs", TTS_ROLE),
            speech_model("key-2", "elevenlabs", TTS_ROLE),
            speech_model("key-3", "elevenlabs", TTS_ROLE),
        ];
        let state = RouterState::new(models);
        let pool = state.pool_indices(TTS_ROLE);

        let first_pick = |start: usize| -> &str {
            let order = build_priority_order(&state.models, &pool, start);
            state.models[order[0]].name.as_str()
        };
        // Three consecutive calls must open on three different keys.
        let picks: Vec<&str> = (0..3).map(first_pick).collect();
        let mut sorted = picks.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 3, "expected a rotation, got {picks:?}");
    }

    // An out-of-quota key must come back eventually, but not on the next
    // sentence; a concurrency collision must come back almost immediately.
    // Getting these two backwards is the difference between a stuttering pool
    // and a pool that benches good keys for a full minute at a time.
    #[test]
    fn quota_and_concurrency_get_very_different_cooldowns() {
        let threshold = 3;

        let mut spent = speech_model("spent", "elevenlabs", TTS_ROLE);
        record_tts_failure(
            &mut spent,
            &anyhow::anyhow!(
                "speech synthesis failed (401 Unauthorized): {{\"detail\":{{\"status\":\"quota_exceeded\"}}}}"
            ),
            threshold,
        );
        assert_eq!(spent.status, "rate_limited");
        assert!(!spent.is_available(), "a spent key must be benched");

        let mut busy = speech_model("busy", "elevenlabs", TTS_ROLE);
        record_tts_failure(
            &mut busy,
            &anyhow::anyhow!(
                "speech synthesis failed (429 Too Many Requests): {{\"detail\":{{\"status\":\"too_many_concurrent_requests\"}}}}"
            ),
            threshold,
        );
        assert_eq!(busy.status, "rate_limited");

        // Both are benched, but the concurrency one for far less time.
        let secs = |m: &ModelRecord| -> i64 {
            let reset = chrono::DateTime::parse_from_rfc3339(
                m.rate_limit_reset_at.as_deref().expect("reset time set"),
            )
            .expect("valid rfc3339");
            (reset.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_seconds()
        };
        assert!(secs(&busy) <= 10, "concurrency bench was {}s", secs(&busy));
        assert!(
            secs(&spent) >= 3600,
            "quota bench was only {}s",
            secs(&spent)
        );
    }

    // A genuinely bad credential must NOT be treated as a spent quota, or a
    // typo'd key would be retried once a day forever instead of surfacing as an
    // error the operator can see and fix.
    #[test]
    fn a_bad_key_still_counts_as_an_error() {
        let mut m = speech_model("typo", "elevenlabs", TTS_ROLE);
        record_tts_failure(
            &mut m,
            &anyhow::anyhow!(
                "speech synthesis failed (401 Unauthorized): {{\"detail\":{{\"status\":\"invalid_api_key\"}}}}"
            ),
            3,
        );
        assert_eq!(m.consecutive_errors, 1);
        assert_ne!(m.status, "rate_limited");
    }
}
