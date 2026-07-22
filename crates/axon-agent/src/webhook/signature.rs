//! Shared `X-Hub-Signature-256` verification for Meta webhooks.
//!
//! Facebook and WhatsApp Cloud API webhooks are configured under the same
//! Meta App and are signed identically: HMAC-SHA256 over the raw request
//! body, keyed by the App Secret. Both call sites share this implementation
//! so there's exactly one place that does the crypto and the constant-time
//! comparison.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// Verifies a Meta `X-Hub-Signature-256` header against `body` using
/// `app_secret`. `source` is only used for log messages (e.g. "FB",
/// "WhatsApp").
///
/// If `app_secret` is empty (credentials.json not configured yet), this
/// accepts the request unsigned rather than breaking webhooks for anyone
/// mid-setup — but logs loudly so an unsigned production webhook doesn't go
/// unnoticed.
pub fn verify_meta_signature(
    source: &str,
    app_secret: &str,
    body: &[u8],
    sig_header: &str,
) -> bool {
    if app_secret.is_empty() {
        tracing::warn!(
            "{source} webhook: app_secret not configured (credentials.json) — accepting an UNSIGNED request. Set facebook.app_secret to enable signature verification.",
        );
        return true;
    }

    let Some(expected_hex) = sig_header.strip_prefix("sha256=") else {
        return false;
    };

    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(app_secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    let computed_hex = hex::encode(mac.finalize().into_bytes());

    computed_hex.as_bytes().ct_eq(expected_hex.as_bytes()).into()
}
