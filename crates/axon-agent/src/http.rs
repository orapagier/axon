//! Process-wide shared HTTP clients.
//!
//! Every `reqwest::Client` owns its own connection pool and TLS context
//! (including a parsed CA bundle), so building one per call site or per node
//! execution multiplies both RAM and handshakes. A `Client` is an `Arc`
//! internally: cloning these statics shares one pool per configuration.

use once_cell::sync::Lazy;
use std::time::Duration;

static DEFAULT: Lazy<reqwest::Client> = Lazy::new(reqwest::Client::new);

static TIMEOUT_30S: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("build shared 30s HTTP client")
});

static TIMEOUT_60S: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .expect("build shared 60s HTTP client")
});

/// Default client — no overall request timeout, same as `Client::new()`.
pub fn shared() -> reqwest::Client {
    DEFAULT.clone()
}

/// Shared client with a 30s overall request timeout.
pub fn shared_30s() -> reqwest::Client {
    TIMEOUT_30S.clone()
}

/// Shared client with a 60s overall request timeout (file downloads).
pub fn shared_60s() -> reqwest::Client {
    TIMEOUT_60S.clone()
}
