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
        // Fail CLOSED: an empty app_secret means signature verification cannot
        // run, so refusing the request is the only safe default — an unsigned
        // production webhook must never be accepted just because the secret is
        // missing. (This previously returned `true`, letting a misconfigured
        // app accept unsigned webhooks silently. Do NOT restore that.)
        //
        // The one escape hatch is explicit + loud: set AXON_DEV_ALLOW_UNSIGNED=1
        // to accept unsigned webhooks in a local dev environment that has not
        // configured an app_secret yet. It is an explicit opt-in and logs at
        // WARN every time it is taken, so it can't slip into production unseen.
        if std::env::var("AXON_DEV_ALLOW_UNSIGNED")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
        {
            tracing::warn!(
                "{source} webhook: app_secret not configured (credentials.json) — accepting an UNSIGNED request because AXON_DEV_ALLOW_UNSIGNED=1. Set facebook.app_secret to enable signature verification.",
            );
            return true;
        }
        tracing::warn!(
            "{source} webhook: REJECTED UNSIGNED request — app_secret is not configured (credentials.json). Set facebook.app_secret to enable signature verification, or set AXON_DEV_ALLOW_UNSIGNED=1 to explicitly allow unsigned webhooks for local development.",
        );
        return false;
    }

    let Some(expected_hex) = sig_header.strip_prefix("sha256=") else {
        return false;
    };

    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(app_secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    let computed_hex = hex::encode(mac.finalize().into_bytes());

    computed_hex
        .as_bytes()
        .ct_eq(expected_hex.as_bytes())
        .into()
}

#[cfg(test)]
mod tests {
    use super::verify_meta_signature;
    // Rust runs #[test] fns in parallel threads; these two share the process
    // env, so they must not interleave.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn sign(secret: &str, body: &[u8]) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn accepts_valid_and_rejects_tampered() {
        let body = br#"{"entry":1}"#;
        assert!(verify_meta_signature(
            "test",
            "s3cret",
            body,
            &sign("s3cret", body)
        ));
        assert!(!verify_meta_signature(
            "test",
            "s3cret",
            body,
            &sign("wrong", body)
        ));
        assert!(!verify_meta_signature(
            "test",
            "s3cret",
            body,
            "not-a-signature"
        ));
        assert!(!verify_meta_signature("test", "s3cret", body, ""));
    }

    #[test]
    fn empty_secret_fails_closed_without_dev_escape() {
        let _g = ENV_LOCK.lock().unwrap();
        // The variable is read on the empty-secret path only; ensure a stray
        // ambient value cannot flip the expectation.
        std::env::remove_var("AXON_DEV_ALLOW_UNSIGNED");
        assert!(!verify_meta_signature("test", "", b"{}", "sha256=whatever"));
    }

    #[test]
    fn empty_secret_honors_explicit_dev_escape() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("AXON_DEV_ALLOW_UNSIGNED", "1");
        let r = verify_meta_signature("test", "", b"{}", "");
        std::env::remove_var("AXON_DEV_ALLOW_UNSIGNED");
        assert!(r);
    }
}
