use super::nl_parser::parse_schedule;
use super::store::{Job, JobStore, StopCondition};
use crate::config::RuntimeSettings;
use crate::messaging::MessageGateway;
use crate::router::model_router::SharedRouter;
use anyhow::Context;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_cron_scheduler::{Job as CronJob, JobScheduler};
use uuid::Uuid;

pub struct SchedulerEngine {
    store: Arc<JobStore>,
    sched: Arc<Mutex<JobScheduler>>,
    // FIX: removed unused `cron_ids` field that was being redundantly cloned
    cron_ids: Arc<Mutex<HashMap<String, Uuid>>>,
    started: std::sync::atomic::AtomicBool,
    // Deduplicates accidental double-fires for the same logical job.
    fire_guard: Arc<Mutex<HashMap<String, i64>>>,
    router: SharedRouter,
    settings: Arc<RuntimeSettings>,
    messaging: Arc<crate::messaging::MessagingHub>,
}

impl SchedulerEngine {
    pub async fn new(
        store: Arc<JobStore>,
        router: SharedRouter,
        settings: Arc<RuntimeSettings>,
        messaging: Arc<crate::messaging::MessagingHub>,
    ) -> anyhow::Result<Self> {
        let sched = JobScheduler::new().await.context("Create scheduler")?;
        Ok(SchedulerEngine {
            store,
            sched: Arc::new(Mutex::new(sched)),
            cron_ids: Arc::new(Mutex::new(HashMap::new())),
            started: std::sync::atomic::AtomicBool::new(false),
            fire_guard: Arc::new(Mutex::new(HashMap::new())),
            router,
            settings,
            messaging,
        })
    }

    pub async fn start(&self) -> anyhow::Result<()> {
        if self.started.swap(true, std::sync::atomic::Ordering::SeqCst) {
            tracing::warn!("Scheduler start requested but it is already running");
            return Ok(());
        }

        let active_jobs = self.store.active()?;
        let count = active_jobs.len();
        for job in active_jobs {
            if let Err(e) = self.register_cron(&job).await {
                tracing::error!("Failed to register job {}: {}", job.id, e);
            }
        }
        self.sched
            .lock()
            .await
            .start()
            .await
            .context("Start scheduler")?;
        tracing::info!("Scheduler started with {} active jobs", count);
        Ok(())
    }

    pub async fn create(
        &self,
        name: &str,
        task: &str,
        schedule_nl: &str,
        created_by: &str,
        parent_run_id: Option<&str>,
        platform: Option<&str>,
        chat_id: Option<&str>,
        stop_condition: Option<StopCondition>,
    ) -> anyhow::Result<Job> {
        let cron_expr =
            parse_schedule(schedule_nl, Arc::clone(&self.router), &self.settings).await?;
        let (final_platform, final_chat_id) = if platform.unwrap_or("dashboard") == "dashboard" {
            // Auto-detect last active messaging platform
            match self.store.get_last_active_messaging_context() {
                Ok(Some((p, c))) => (p, Some(c)),
                _ => (
                    platform.unwrap_or("dashboard").to_string(),
                    chat_id.map(|s| s.to_string()),
                ),
            }
        } else {
            (
                platform.unwrap_or("dashboard").to_string(),
                chat_id.map(|s| s.to_string()),
            )
        };

        let job = Job {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            task: task.to_string(),
            schedule_nl: schedule_nl.to_string(),
            cron_expr: cron_expr.clone(),
            status: "active".into(),
            created_by: created_by.to_string(),
            platform: final_platform,
            chat_id: final_chat_id,
            parent_run_id: parent_run_id.map(|s| s.to_string()),
            max_runs: None,
            run_count: 0,
            last_run_at: None,
            next_run_at: None,
            last_result: None,
            stop_condition,
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        self.store.create(&job)?;
        self.register_cron(&job).await?;
        tracing::info!("Scheduled '{}' — {}", name, cron_expr);
        Ok(job)
    }

    pub async fn pause(&self, id: &str) -> anyhow::Result<()> {
        self.store.set_status(id, "paused")?;
        self.remove_cron(id).await;
        Ok(())
    }

    pub async fn resume(&self, id: &str) -> anyhow::Result<()> {
        let job = self.store.get(id)?.context("Job not found")?;
        self.store.set_status(id, "active")?;
        self.register_cron(&job).await
    }

    pub async fn delete(&self, id: &str) -> anyhow::Result<()> {
        self.store.delete(id)?;
        self.remove_cron(id).await;
        Ok(())
    }

    async fn register_cron(&self, job: &Job) -> anyhow::Result<()> {
        // Idempotency: ensure we never keep a stale schedule for the same job id.
        // This prevents duplicate fires after repeated resume/update/register paths.
        self.remove_cron(&job.id).await;

        let job_id = job.id.clone();
        let task = job.task.clone();
        let store = Arc::clone(&self.store);
        let stop_cond = job.stop_condition.clone();
        let cron_str = job.cron_expr.clone();
        let cron_slot_source = cron_str.clone();
        let dedupe_window_ms = dedupe_window_ms_for_cron(&cron_str);
        let fire_guard = Arc::clone(&self.fire_guard);

        let messaging = Arc::clone(&self.messaging);
        let platform = job.platform.clone();
        let chat_id = job.chat_id.clone();
        let job_name = job.name.clone();
        let settings = Arc::clone(&self.settings);

        // Convert the operator-local cron expression to UTC so we can use the
        // standard new_async scheduler, which avoids known timezone-handling
        // issues in some tokio-cron-scheduler builds.
        let utc_cron_str = local_cron_to_utc(&cron_str, settings.agent_utc_offset_hours());
        tracing::info!(
            "Scheduling job {} ('{}') | local: {} | UTC: {}",
            job_id,
            job_name,
            cron_str,
            utc_cron_str
        );

        let cron_job = CronJob::new_async(&utc_cron_str, move |_uuid, _lock| {
            let job_id = job_id.clone();
            let task = task.clone();
            let store = Arc::clone(&store);
            let stop_cond = stop_cond.clone();
            let messaging = Arc::clone(&messaging);
            let platform = platform.clone();
            let chat_id = chat_id.clone();
            let job_name = job_name.clone();
            let settings = Arc::clone(&settings);
            let fire_guard = Arc::clone(&fire_guard);
            let dedupe_window_ms = dedupe_window_ms;
            let cron_slot_source = cron_slot_source.clone();

            Box::pin(async move {
                let now = chrono::Utc::now();
                let slot_key = fire_slot_key_for_cron(&cron_slot_source, now);
                match store.claim_fire_slot(&job_id, &slot_key) {
                    Ok(false) => {
                        tracing::warn!(
                            "Skipping duplicate cron fire for job {} ('{}') in slot {}",
                            job_id,
                            job_name,
                            slot_key
                        );
                        return;
                    }
                    Ok(true) => {}
                    Err(e) => {
                        tracing::warn!(
                            "Unable to claim cron fire slot for job {}: {}. Continuing with in-memory guard.",
                            job_id,
                            e
                        );
                    }
                }

                // Hard duplicate protection: if duplicate scheduler entries fire very close
                // together for the same logical job, execute only once.
                let now_ms = now.timestamp_millis();
                {
                    let mut guard = fire_guard.lock().await;
                    if let Some(last_ms) = guard.get(&job_id).copied() {
                        let delta_ms = now_ms.saturating_sub(last_ms);
                        if dedupe_window_ms > 0 && delta_ms <= dedupe_window_ms {
                            tracing::warn!(
                                "Skipping duplicate cron fire for job {} ('{}') at t={} (delta={}ms, window={}ms)",
                                job_id,
                                job_name,
                                now_ms,
                                delta_ms,
                                dedupe_window_ms
                            );
                            return;
                        }
                    }
                    guard.insert(job_id.clone(), now_ms);
                }

                tracing::info!("Job {} ('{}') firing task: {}", job_id, job_name, task);
                let user_name = settings.get_str("watcher.user_name", "User");
                let user_title = settings.get_str("watcher.user_title", "");
                let prompt = settings
                    .get_str("scheduler.nudge_prompt", "")
                    .replace("{job_name}", &job_name)
                    .replace("{task}", &task)
                    .replace("{user_name}", &user_name)
                    .replace("{user_title}", &user_title);

                let result = execute_job_task(&prompt, &platform, chat_id.as_deref(), &job_id)
                    .await
                    .unwrap_or_else(|e| format!("Error: {}", e));
                let _ = store.record_run(&job_id, &result);

                // Send notifications
                let (target_platform, target_cid) = if platform == "dashboard" {
                    // For dashboard jobs, use watcher settings
                    (
                        settings.get_str("watcher.notify_platform", "telegram"),
                        settings.get_str("watcher.notify_chat_id", ""),
                    )
                } else if let Some(ref cid) = chat_id {
                    // For messaging-platform originated jobs, use the job's original platform/chat
                    (platform.clone(), cid.clone())
                } else {
                    ("".to_string(), "".to_string())
                };

                if !target_cid.is_empty() && !target_platform.is_empty() {
                    let msg = result.clone();
                    tracing::info!(
                        "Sending cron job {} result to '{}' chat ID: {}",
                        job_id,
                        target_platform,
                        target_cid
                    );
                    match target_platform.as_str() {
                        "telegram" => {
                            if let Some(tg) = messaging.telegram.lock().await.as_ref() {
                                if let Err(e) = tg.send_text(&target_cid, &msg).await {
                                    tracing::error!("Failed to send Telegram notification: {}", e);
                                }
                            }
                        }
                        "discord" => {
                            if let Some(dc) = messaging.discord.lock().await.as_ref() {
                                if let Err(e) = dc.send_text(&target_cid, &msg).await {
                                    tracing::error!("Failed to send Discord notification: {}", e);
                                }
                            }
                        }
                        "slack" => {
                            if let Some(sl) = messaging.slack.lock().await.as_ref() {
                                if let Err(e) = sl.send_text(&target_cid, &msg).await {
                                    tracing::error!("Failed to send Slack notification: {}", e);
                                }
                            }
                        }
                        _ => {}
                    }
                } else {
                    tracing::warn!("Job {} finished, but watcher.notify_chat_id or platform is empty. Discarding output.", job_id);
                }

                if should_stop(&store, &job_id, &stop_cond, &result) {
                    tracing::info!("Job {} stop condition met — completing", job_id);
                    let _ = store.set_status(&job_id, "completed");
                }
            })
        })
        .context("Create cron job")?;

        let cron_uuid = self
            .sched
            .lock()
            .await
            .add(cron_job)
            .await
            .context("Add cron job")?;
        self.cron_ids.lock().await.insert(job.id.clone(), cron_uuid);
        Ok(())
    }

    async fn remove_cron(&self, id: &str) {
        let mut map = self.cron_ids.lock().await;
        if let Some(uuid) = map.remove(id) {
            let sched = self.sched.lock().await;
            let _ = sched.remove(&uuid).await;
        }
    }

    pub async fn get_all(&self) -> anyhow::Result<Vec<Job>> {
        self.store.all()
    }

    pub async fn run_once(&self, id: &str) -> anyhow::Result<String> {
        let job = self.store.get(id)?.context("Job not found")?;
        let user_name = self.settings.get_str("watcher.user_name", "User");
        let user_title = self.settings.get_str("watcher.user_title", "");
        let prompt = self
            .settings
            .get_str("scheduler.nudge_prompt", "")
            .replace("{job_name}", &job.name)
            .replace("{task}", &job.task)
            .replace("{user_name}", &user_name)
            .replace("{user_title}", &user_title);

        let result =
            execute_job_task(&prompt, &job.platform, job.chat_id.as_deref(), &job.id).await?;
        let _ = self.store.record_run(&job.id, &result);

        // Send the result via the job's messaging platform (same as scheduled fires)
        // Determine notification target
        let (target_platform, target_cid) = if job.platform == "dashboard" {
            // For dashboard jobs, use watcher settings
            (
                self.settings.get_str("watcher.notify_platform", "telegram"),
                self.settings.get_str("watcher.notify_chat_id", ""),
            )
        } else if let Some(ref cid) = job.chat_id {
            // For messaging-platform originated jobs, use the job's original platform/chat
            (job.platform.clone(), cid.clone())
        } else {
            ("".to_string(), "".to_string())
        };

        tracing::info!(
            "run_once: job platform={}, target={}, target_cid={}",
            job.platform,
            target_platform,
            target_cid
        );

        if !target_cid.is_empty() && !target_platform.is_empty() {
            let msg = result.clone();
            tracing::info!(
                "Sending message to '{}' chat ID: {}",
                target_platform,
                target_cid
            );
            match target_platform.as_str() {
                "telegram" => {
                    if let Some(tg) = self.messaging.telegram.lock().await.as_ref() {
                        if let Err(e) = tg.send_text(&target_cid, &msg).await {
                            tracing::error!(
                                "Failed to send Telegram notification (run_once): {}",
                                e
                            );
                        }
                    }
                }
                "discord" => {
                    if let Some(dc) = self.messaging.discord.lock().await.as_ref() {
                        if let Err(e) = dc.send_text(&target_cid, &msg).await {
                            tracing::error!(
                                "Failed to send Discord notification (run_once): {}",
                                e
                            );
                        }
                    }
                }
                "slack" => {
                    if let Some(sl) = self.messaging.slack.lock().await.as_ref() {
                        if let Err(e) = sl.send_text(&target_cid, &msg).await {
                            tracing::error!("Failed to send Slack notification (run_once): {}", e);
                        }
                    }
                }
                _ => {}
            }
        } else {
            tracing::warn!("run_once: job {} finished, but watcher.notify_chat_id or platform is empty. Discarding output.", job.id);
        }

        Ok(result)
    }

    pub async fn update(
        &self,
        id: &str,
        name: &str,
        task: &str,
        schedule_nl: &str,
    ) -> anyhow::Result<()> {
        let cron_expr =
            parse_schedule(schedule_nl, Arc::clone(&self.router), &self.settings).await?;
        self.store.update(id, name, task, schedule_nl, &cron_expr)?;

        // If the job is active, re-register it to update the cron schedule
        let job = self.store.get(id)?.context("Job not found")?;
        if job.status == "active" {
            self.remove_cron(id).await;
            self.register_cron(&job).await?;
        }
        Ok(())
    }
}

fn should_stop(store: &JobStore, id: &str, cond: &Option<StopCondition>, result: &str) -> bool {
    let Some(c) = cond else {
        return false;
    };
    match c.condition_type.as_str() {
        "result_contains" => result.contains(&c.value),
        "run_count" => store
            .get(id)
            .ok()
            .flatten()
            .and_then(|j| c.value.parse::<i64>().ok().map(|max| j.run_count >= max))
            .unwrap_or(false),
        "date_after" => chrono::Utc::now().to_rfc3339() > c.value,
        _ => false,
    }
}

async fn execute_job_task(
    final_task: &str,
    platform: &str,
    chat_id: Option<&str>,
    job_id: &str,
) -> anyhow::Result<String> {
    let port = std::env::var("AXON_PORT").unwrap_or_else(|_| "3000".into());
    let master_key = std::env::var("AXON_MASTER_KEY").unwrap_or_default();

    // Use an isolated session ID for cron jobs so they don't pollute the dashboard chat
    let session_id = chat_id
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("cron:{}", job_id));

    let mut req = reqwest::Client::new().post(format!("http://127.0.0.1:{}/api/run", port));

    if !master_key.is_empty() {
        req = req.bearer_auth(master_key);
    }

    let resp = req
        .json(&serde_json::json!({
            "task": final_task,
            "session_id": session_id,
            "platform": platform,
            "chat_id": chat_id,
            "job_id": job_id
        }))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;
    Ok(resp
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or("completed")
        .to_string())
}

/// Convert a 6-field cron expression written in operator-local time (a fixed
/// UTC offset in whole hours, see `agent.utc_offset_hours`) to UTC.
///
/// Format: `sec min hour dom month dow`
///
/// If the hour field is a plain integer we subtract the offset and handle the
/// midnight wrap-around in either direction (dom/dow adjusted by ±1; a month
/// boundary is not attempted, matching the prior behavior). Wildcard / step /
/// range expressions in the hour field are left unchanged because they span
/// the full day anyway.
pub fn local_cron_to_utc(cron: &str, offset_hours: i32) -> String {
    let parts: Vec<&str> = cron.split_whitespace().collect();
    if parts.len() != 6 {
        return cron.to_string(); // can't parse — return as-is
    }

    let (sec, min, hour_str, dom, month, dow) =
        (parts[0], parts[1], parts[2], parts[3], parts[4], parts[5]);

    // Only convert a plain numeric hour value; leave wildcards / ranges / steps alone.
    if let Ok(h) = hour_str.parse::<i32>() {
        let utc_hour = h - offset_hours;
        if utc_hour < 0 {
            // Crosses midnight backwards → previous UTC day.
            let wrapped = utc_hour + 24; // e.g. hour 7 at UTC+8 → 23 UTC
            let utc_dow = shift_dow(dow, -1);
            let utc_dom = if dom == "*" {
                "*".to_string()
            } else if let Ok(d) = dom.parse::<i32>() {
                if d > 1 {
                    (d - 1).to_string()
                } else {
                    dom.to_string()
                }
            } else {
                dom.to_string()
            };

            return format!(
                "{} {} {} {} {} {}",
                sec, min, wrapped, utc_dom, month, utc_dow
            );
        } else if utc_hour >= 24 {
            // Negative offset crossing midnight forwards → next UTC day.
            let wrapped = utc_hour - 24;
            let utc_dow = shift_dow(dow, 1);
            let utc_dom = if dom == "*" {
                "*".to_string()
            } else if let Ok(d) = dom.parse::<i32>() {
                if d < 28 {
                    (d + 1).to_string()
                } else {
                    dom.to_string()
                }
            } else {
                dom.to_string()
            };

            return format!(
                "{} {} {} {} {} {}",
                sec, min, wrapped, utc_dom, month, utc_dow
            );
        } else {
            return format!("{} {} {} {} {} {}", sec, min, utc_hour, dom, month, dow);
        }
    }

    // Fallback: return unchanged (e.g. "0 */5 * * * *")
    cron.to_string()
}

/// Shift a day-of-week field by `delta` (only -1 supported for midnight wrap).
/// Handles: `*`, `MON`, `MON-FRI`, `MON,WED,FRI`, bare numbers.
pub fn shift_dow(dow: &str, delta: i32) -> String {
    const DAYS: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];

    if dow == "*" {
        return "*".to_string();
    }

    // Try numeric
    if let Ok(n) = dow.parse::<i32>() {
        let shifted = ((n + delta).rem_euclid(7)) as usize;
        return shifted.to_string();
    }

    // Try named day (single, no range/list)
    let upper = dow.to_uppercase();
    if let Some(idx) = DAYS.iter().position(|&d| d == upper.as_str()) {
        let shifted = ((idx as i32 + delta).rem_euclid(7)) as usize;
        return DAYS[shifted].to_string();
    }

    // Range or list — too complex to shift safely, return as-is
    dow.to_string()
}

fn dedupe_window_ms_for_cron(cron_expr: &str) -> i64 {
    let sec_field = cron_expr.split_whitespace().next().unwrap_or("0").trim();
    // If seconds field is a fixed value (e.g. "0" or "15"), the schedule is at
    // most once per minute. Allow a wide guard window so 1-30s duplicate jitter
    // from scheduler internals is still filtered while not blocking the next minute run.
    if sec_field.parse::<u32>().is_ok() {
        45_000
    } else {
        0
    }
}

fn fire_slot_key_for_cron(cron_expr: &str, now: chrono::DateTime<chrono::Utc>) -> String {
    let sec_field = cron_expr.split_whitespace().next().unwrap_or("0").trim();
    if sec_field.parse::<u32>().is_ok() {
        now.format("%Y-%m-%dT%H:%M").to_string()
    } else {
        now.format("%Y-%m-%dT%H:%M:%S").to_string()
    }
}
