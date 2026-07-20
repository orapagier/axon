//! Dashboard authentication: the single shared `AXON_MASTER_KEY`.
//!
//! Axon is single-tenant — there are no user accounts, so this one key is the
//! whole authorization model. It is also the KDF input for every stored
//! credential (see `crypto.rs`), which makes guessing it the highest-value
//! attack on the system and is why the failure path is throttled here.

use axum::{
    extract::{ConnectInfo, Request},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{
    collections::HashMap,
    env,
    net::SocketAddr,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

/// Wrong keys allowed from one source before penalties start. Generous enough
/// to absorb a stale key in a phone client retrying a few times.
const FREE_ATTEMPTS: u32 = 10;
/// First penalty; doubles per subsequent failure up to `MAX_PENALTY`.
const BASE_PENALTY: Duration = Duration::from_secs(30);
const MAX_PENALTY: Duration = Duration::from_secs(900);
/// Idle time after which a source is forgotten entirely.
const RECORD_TTL: Duration = Duration::from_secs(900);
/// Hard cap on tracked sources. The map is keyed on a client-controlled header
/// when one is present, so it must never grow unbounded.
const MAX_TRACKED: usize = 4096;
/// Truncation for that key — an attacker could otherwise send a megabyte of
/// `X-Forwarded-For` and have us store it.
const MAX_KEY_LEN: usize = 64;

#[derive(Debug, Clone)]
struct Record {
    failures: u32,
    blocked_until: Option<Instant>,
    last_seen: Instant,
}

#[derive(Default)]
struct Throttle {
    clients: HashMap<String, Record>,
}

impl Throttle {
    /// How much longer `client` stays blocked, if it is blocked right now.
    fn penalty_remaining(&self, client: &str, now: Instant) -> Option<Duration> {
        let until = self.clients.get(client)?.blocked_until?;
        (until > now).then(|| until - now)
    }

    fn record_failure(&mut self, client: &str, now: Instant) {
        self.prune(now);

        if !self.clients.contains_key(client) && self.clients.len() >= MAX_TRACKED {
            // Saturation is itself the signal: a single real deployment never
            // has thousands of distinct sources failing auth. It means the
            // forwarded-for header is being spoofed to evade the per-source
            // penalty, which is only possible when the app port is reachable
            // without going through the reverse proxy.
            tracing::error!(
                "Auth throttle table saturated at {} sources — a new failing source is not \
                 being tracked. This strongly suggests X-Forwarded-For spoofing; verify the \
                 app port is reachable ONLY via the reverse proxy (see deploy/Caddyfile.example).",
                MAX_TRACKED
            );
            return;
        }

        let rec = self.clients.entry(client.to_string()).or_insert(Record {
            failures: 0,
            blocked_until: None,
            last_seen: now,
        });
        rec.failures = rec.failures.saturating_add(1);
        rec.last_seen = now;

        if rec.failures > FREE_ATTEMPTS {
            // `over` is clamped before the shift: an unclamped 1 << over would
            // overflow once a source accumulated ~32 failures.
            let over = (rec.failures - FREE_ATTEMPTS - 1).min(15);
            let penalty = BASE_PENALTY.saturating_mul(1u32 << over).min(MAX_PENALTY);
            rec.blocked_until = Some(now + penalty);
        }
    }

    /// A correct key wipes the source's history — the legitimate operator can
    /// always recover simply by presenting the real key.
    fn record_success(&mut self, client: &str, now: Instant) {
        self.clients.remove(client);
        self.prune(now);
    }

    fn prune(&mut self, now: Instant) {
        self.clients.retain(|_, r| {
            now.duration_since(r.last_seen) < RECORD_TTL
                || r.blocked_until.is_some_and(|until| until > now)
        });
    }
}

static THROTTLE: LazyLock<Mutex<Throttle>> = LazyLock::new(|| Mutex::new(Throttle::default()));

/// Lock the throttle, recovering from a poisoned mutex rather than panicking —
/// a panic here would take down authentication for the whole process.
fn throttle() -> std::sync::MutexGuard<'static, Throttle> {
    THROTTLE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Identify the request source, mirroring the precedence `SmartIpKeyExtractor`
/// uses for the webhook rate limiter so both agree on what "one client" means.
///
/// The forwarded headers are trustworthy only when set by the reverse proxy;
/// `deploy/Caddyfile.example` requires the app port to be unreachable directly
/// for exactly this reason.
fn client_key(req: &Request) -> String {
    let headers = req.headers();

    let from_header = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        });

    let key = match from_header {
        Some(ip) => ip.to_string(),
        None => req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ci| ci.0.ip().to_string())
            .unwrap_or_else(|| "unknown".to_string()),
    };

    key.chars().take(MAX_KEY_LEN).collect()
}

fn too_many_requests(remaining: Duration) -> Response {
    let secs = remaining.as_secs().max(1);
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, secs.to_string())],
        "Too many failed authentication attempts. Try again later.",
    )
        .into_response()
}

pub async fn require_auth(req: Request, next: Next) -> Response {
    let master_key = env::var("AXON_MASTER_KEY").unwrap_or_default();

    // No key configured: local development only. Boot refuses this in
    // production unless AXON_DEV=1 — see crypto::validate_master_key.
    if master_key.is_empty() {
        return next.run(req).await;
    }

    let client = client_key(&req);
    let now = Instant::now();

    // Checked BEFORE the key comparison — that is what actually bounds a
    // brute force. Verifying the key first and only then delaying would still
    // let an attacker test keys as fast as they can open connections.
    let blocked = throttle().penalty_remaining(&client, now);
    if let Some(remaining) = blocked {
        tracing::warn!(
            "Auth attempt from {} rejected: throttled for another {}s",
            client,
            remaining.as_secs()
        );
        return too_many_requests(remaining);
    }

    let auth_header = req
        .headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok());

    let query_key = req.uri().query().and_then(|q| {
        q.split('&')
            .find_map(|p| p.strip_prefix("api_key="))
            // The browser sends the key via encodeURIComponent (see axon-ui ws.js),
            // so it must be percent-decoded before comparison. Keys containing
            // characters such as '+', '/', '=' or spaces — common in base64/random
            // secrets — would otherwise never match, breaking WebSocket auth even
            // though REST (which sends the raw key in the Bearer header) still works.
            .map(|raw| {
                urlencoding::decode(raw)
                    .map(|d| d.into_owned())
                    .unwrap_or_else(|_| raw.to_string())
            })
    });

    let provided = if let Some(h) = auth_header {
        h.strip_prefix("Bearer ").unwrap_or(h).to_string()
    } else if let Some(q) = query_key {
        q
    } else {
        "".to_string()
    };

    // Constant-time comparison: a plain `==` short-circuits on the first
    // differing byte, letting response-time measurements leak key prefixes.
    // (ct_eq still reveals length inequality, which is not secret here.)
    use subtle::ConstantTimeEq;
    let valid: bool = provided.as_bytes().ct_eq(master_key.as_bytes()).into();

    if valid {
        throttle().record_success(&client, now);
        next.run(req).await
    } else {
        throttle().record_failure(&client, now);
        tracing::warn!(
            "Unauthorized access attempt to dashboard from {} (invalid master key)",
            client
        );
        StatusCode::UNAUTHORIZED.into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Throttle {
        Throttle::default()
    }

    #[test]
    fn free_attempts_are_not_penalised() {
        let mut th = t();
        let now = Instant::now();
        for _ in 0..FREE_ATTEMPTS {
            th.record_failure("1.2.3.4", now);
        }
        assert!(th.penalty_remaining("1.2.3.4", now).is_none());
    }

    #[test]
    fn penalty_starts_after_free_attempts_and_escalates() {
        let mut th = t();
        let now = Instant::now();
        for _ in 0..=FREE_ATTEMPTS {
            th.record_failure("1.2.3.4", now);
        }
        let first = th.penalty_remaining("1.2.3.4", now).expect("blocked");
        assert!(first <= BASE_PENALTY && first > Duration::ZERO);

        th.record_failure("1.2.3.4", now);
        let second = th.penalty_remaining("1.2.3.4", now).expect("still blocked");
        assert!(
            second > first,
            "penalty must escalate: {second:?} > {first:?}"
        );
    }

    #[test]
    fn penalty_is_capped() {
        let mut th = t();
        let now = Instant::now();
        for _ in 0..200 {
            th.record_failure("1.2.3.4", now);
        }
        assert!(th.penalty_remaining("1.2.3.4", now).unwrap() <= MAX_PENALTY);
    }

    // The operator must always be able to recover by presenting the real key.
    #[test]
    fn success_clears_history() {
        let mut th = t();
        let now = Instant::now();
        for _ in 0..FREE_ATTEMPTS {
            th.record_failure("1.2.3.4", now);
        }
        th.record_success("1.2.3.4", now);
        assert!(th.clients.get("1.2.3.4").is_none());
    }

    // An attacker hammering from one source must not lock the operator out.
    #[test]
    fn sources_are_isolated() {
        let mut th = t();
        let now = Instant::now();
        for _ in 0..=FREE_ATTEMPTS * 3 {
            th.record_failure("9.9.9.9", now);
        }
        assert!(th.penalty_remaining("9.9.9.9", now).is_some());
        assert!(th.penalty_remaining("1.2.3.4", now).is_none());
    }

    #[test]
    fn expired_penalty_lets_the_source_back_in() {
        let mut th = t();
        let now = Instant::now();
        for _ in 0..=FREE_ATTEMPTS {
            th.record_failure("1.2.3.4", now);
        }
        assert!(th.penalty_remaining("1.2.3.4", now).is_some());
        let later = now + MAX_PENALTY + Duration::from_secs(1);
        assert!(th.penalty_remaining("1.2.3.4", later).is_none());
    }

    #[test]
    fn idle_records_are_pruned() {
        let mut th = t();
        let now = Instant::now();
        th.record_failure("1.2.3.4", now);
        th.record_failure("5.6.7.8", now + RECORD_TTL + Duration::from_secs(1));
        assert!(!th.clients.contains_key("1.2.3.4"), "stale record pruned");
    }

    // The map key comes from a spoofable header, so growth must be bounded.
    #[test]
    fn tracking_table_is_bounded() {
        let mut th = t();
        let now = Instant::now();
        for i in 0..(MAX_TRACKED + 500) {
            th.record_failure(&format!("10.0.{}.{}", i / 256, i % 256), now);
        }
        assert!(th.clients.len() <= MAX_TRACKED);
    }
}
