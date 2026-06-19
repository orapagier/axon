use crate::config::{expand_env, Config};
use std::{
    collections::HashMap,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

// ─────────────────────────────────────────────
//  Cooldown durations per error type (seconds)
// ─────────────────────────────────────────────

const COOLDOWN_RATE_LIMIT: u64 = 60;
const COOLDOWN_AUTH: u64 = 300;
const COOLDOWN_NOT_FOUND: u64 = 120;
const COOLDOWN_SERVER: u64 = 30;
const COOLDOWN_TIMEOUT: u64 = 20;
const COOLDOWN_NETWORK: u64 = 10;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ─────────────────────────────────────────────
//  ModelSlot
// ─────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ModelSlot {
    pub name: String,
    pub provider: String,
    pub model_id: String,
    pub api_key: String,
    pub base_url: String,
    pub auth_style: String,
    pub timeout_secs: u64,
    /// Lower = tried first. Slots are grouped by priority tier.
    /// Within a tier, selection is random (uniform).
    /// Higher-priority tiers are exhausted before falling back.
    pub priority: u32,
}

// ─────────────────────────────────────────────
//  GlobalPool
// ─────────────────────────────────────────────

pub struct GlobalPool {
    pub slots: Vec<ModelSlot>,
    cooldown_until: Vec<AtomicU64>,
    /// Sorted list of unique priority values (ascending), derived at build time.
    pub priority_tiers: Vec<u32>,
}

impl GlobalPool {
    /// Return the next available slot, preferring the lowest priority tier
    /// that has at least one available (non-cooling) slot.
    /// Falls back to the next tier only when all slots in the current tier
    /// are cooling. This means slow models ARE used — just only when fast
    /// ones are all rate-limited/erroring.
    pub fn next(&self) -> Option<(usize, &ModelSlot)> {
        let n = self.slots.len();
        if n == 0 {
            return None;
        }
        let now = now_secs();

        for &tier in &self.priority_tiers {
            // Collect available slots in this tier
            let available: Vec<usize> = self
                .slots
                .iter()
                .enumerate()
                .filter(|(i, s)| {
                    s.priority == tier && now >= self.cooldown_until[*i].load(Ordering::Relaxed)
                })
                .map(|(i, _)| i)
                .collect();

            if available.is_empty() {
                // All slots in this tier are cooling — try next tier
                continue;
            }

            // Random selection within the available slots of this tier
            let pos = rand::random_range(0..available.len());
            let idx = available[pos];
            return Some((idx, &self.slots[idx]));
        }

        // Every slot in every tier is cooling — return the one expiring soonest
        let idx = self
            .cooldown_until
            .iter()
            .enumerate()
            .min_by_key(|(_, c)| c.load(Ordering::Relaxed))
            .map(|(i, _)| i)
            .unwrap_or(0);
        Some((idx, &self.slots[idx]))
    }

    pub fn mark_failed(&self, idx: usize, status: u16) {
        if idx >= self.slots.len() {
            return;
        }
        let cooldown = match status {
            429 => COOLDOWN_RATE_LIMIT,
            401 | 403 => COOLDOWN_AUTH,
            404 => COOLDOWN_NOT_FOUND,
            408 => COOLDOWN_TIMEOUT,
            500..=599 => COOLDOWN_SERVER,
            0 => COOLDOWN_NETWORK,
            _ => COOLDOWN_NETWORK,
        };
        self.cooldown_until[idx].store(now_secs() + cooldown, Ordering::Relaxed);
        tracing::warn!(
            "slot '{}' (priority={}) cooling {}s (status={})",
            self.slots[idx].name,
            self.slots[idx].priority,
            cooldown,
            status
        );
    }

    pub fn mark_timeout(&self, idx: usize) {
        if idx >= self.slots.len() {
            return;
        }
        self.cooldown_until[idx].store(now_secs() + COOLDOWN_TIMEOUT, Ordering::Relaxed);
        tracing::warn!(
            "slot '{}' (priority={}) timed out — cooling {}s",
            self.slots[idx].name,
            self.slots[idx].priority,
            COOLDOWN_TIMEOUT
        );
    }

    pub fn available_count(&self) -> usize {
        let now = now_secs();
        self.cooldown_until
            .iter()
            .filter(|c| now >= c.load(Ordering::Relaxed))
            .count()
    }

    /// Available slots broken down by tier — useful for the dashboard/logs.
    pub fn available_by_tier(&self) -> Vec<(u32, usize, usize)> {
        let now = now_secs();
        self.priority_tiers
            .iter()
            .map(|&tier| {
                let total = self.slots.iter().filter(|s| s.priority == tier).count();
                let avail = self
                    .slots
                    .iter()
                    .enumerate()
                    .filter(|(i, s)| {
                        s.priority == tier && now >= self.cooldown_until[*i].load(Ordering::Relaxed)
                    })
                    .count();
                (tier, avail, total)
            })
            .collect()
    }
}

// ─────────────────────────────────────────────
//  Pool builder
// ─────────────────────────────────────────────

const DEFAULT_TIMEOUT_SECS: u64 = 30;

pub fn build_pool(config: &Config, env_overrides: &HashMap<String, String>) -> GlobalPool {
    let mut slots: Vec<ModelSlot> = config
        .models
        .iter()
        .filter(|m| m.enabled)
        .filter_map(|m| {
            let api_key = expand_env(&m.api_key, env_overrides).filter(|k| !k.is_empty())?;

            let base_url = m
                .base_url
                .as_deref()
                .map(|u| u.trim_end_matches('/').to_string())
                .or_else(|| {
                    config
                        .providers
                        .get(&m.provider)
                        .map(|p| p.base_url.trim_end_matches('/').to_string())
                })?;

            let auth_style = m
                .auth_style
                .clone()
                .or_else(|| {
                    config
                        .providers
                        .get(&m.provider)
                        .map(|p| p.auth_style.clone())
                })
                .unwrap_or_else(|| "bearer".to_string());

            let timeout_secs = m
                .timeout_secs
                .map(|t| t as u64)
                .or_else(|| {
                    config
                        .providers
                        .get(&m.provider)
                        .and_then(|p| p.timeout_secs)
                        .map(|t| t as u64)
                })
                .unwrap_or(DEFAULT_TIMEOUT_SECS);

            Some(ModelSlot {
                name: m.name.clone(),
                provider: m.provider.clone(),
                model_id: m.model_id.clone(),
                api_key,
                base_url,
                auth_style,
                timeout_secs,
                priority: m.priority,
            })
        })
        .collect();

    // Stable sort by priority so lower-numbered tiers come first
    slots.sort_by_key(|s| s.priority);

    // Derive sorted unique tier values
    let mut priority_tiers: Vec<u32> = slots.iter().map(|s| s.priority).collect();
    priority_tiers.sort_unstable();
    priority_tiers.dedup();

    let n = slots.len();
    tracing::info!(
        "Pool built: {} slots across {} tier(s): {:?}",
        n,
        priority_tiers.len(),
        priority_tiers
    );

    GlobalPool {
        cooldown_until: (0..n).map(|_| AtomicU64::new(0)).collect(),
        slots,
        priority_tiers,
    }
}

// ─────────────────────────────────────────────
//  Tests
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModelEntry;

    fn make_entry(name: &str, priority: u32, key: &str) -> ModelEntry {
        ModelEntry {
            name: name.into(),
            provider: "google".into(),
            model_id: "gemini-flash".into(),
            api_key: key.into(),
            role: String::new(),
            priority,
            max_tokens: 4096,
            enabled: true,
            base_url: Some("https://api.example.com/v1".into()),
            auth_style: None,
            timeout_secs: None,
        }
    }

    #[test]
    fn tier1_used_before_tier2() {
        let config = Config {
            providers: HashMap::new(),
            models: vec![
                make_entry("slow-a", 2, "k"),
                make_entry("fast-a", 1, "k"),
                make_entry("slow-b", 2, "k"),
            ],
        };
        let pool = build_pool(&config, &HashMap::new());
        // Both picks should be from tier 1
        assert_eq!(pool.next().unwrap().1.priority, 1);
        assert_eq!(pool.next().unwrap().1.priority, 1);
    }

    #[test]
    fn falls_back_to_tier2_when_tier1_all_cooling() {
        let config = Config {
            providers: HashMap::new(),
            models: vec![
                make_entry("fast-a", 1, "k"),
                make_entry("fast-b", 1, "k"),
                make_entry("slow-a", 2, "k"),
            ],
        };
        let pool = build_pool(&config, &HashMap::new());
        // Cool down all tier-1 slots
        for (i, s) in pool.slots.iter().enumerate() {
            if s.priority == 1 {
                pool.mark_failed(i, 429);
            }
        }
        // Should now pick from tier 2
        let (_, slot) = pool.next().unwrap();
        assert_eq!(slot.priority, 2);
        assert_eq!(slot.name, "slow-a");
    }

    #[test]
    fn random_within_tier_uses_all_slots() {
        let config = Config {
            providers: HashMap::new(),
            models: vec![make_entry("a", 1, "k"), make_entry("b", 1, "k")],
        };
        let pool = build_pool(&config, &HashMap::new());
        // With random selection, both slots should appear across enough draws
        let names: std::collections::HashSet<String> = (0..40)
            .map(|_| pool.next().unwrap().1.name.clone())
            .collect();
        assert!(names.contains("a"), "slot 'a' never selected");
        assert!(names.contains("b"), "slot 'b' never selected");
    }

    #[test]
    fn build_pool_filters_disabled_and_missing_keys() {
        let mut disabled = make_entry("disabled", 1, "key456");
        disabled.enabled = false;
        let config = Config {
            providers: HashMap::new(),
            models: vec![
                make_entry("active", 1, "key123"),
                disabled,
                make_entry("no-key", 1, "${MISSING_VAR_XYZ_99}"),
            ],
        };
        let pool = build_pool(&config, &HashMap::new());
        assert_eq!(pool.slots.len(), 1);
        assert_eq!(pool.slots[0].name, "active");
    }
}
