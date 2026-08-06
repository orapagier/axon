//! Per-node Google account selection.
//!
//! A Gmail node (and any other Google-backed node) may pick a `credential_id`
//! naming a Google account connected through the node's own Connect button.
//! Leaving it unset keeps the node on the account signed in on the Credentials
//! page — that is deliberately the default, so existing workflows are unaffected.
//!
//! This module owns the half of the multi-account story that needs the SQLite
//! `credentials` table: turning a credential id into a *live* access token,
//! refreshing it when stale and writing the result back. The scope it hands to
//! `axon_core::google_account` is then picked up by
//! `axon_google::auth::access_token`, which every Google helper authenticates
//! through.
//!
//! Unlike a Facebook Page token, a Google access token expires in an hour, so a
//! stored credential is only useful alongside its refresh token — see
//! [`resolve`] for how that is kept current.

use crate::state::AppState;
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// The `service` value under which connected Google accounts are stored.
pub const SERVICE: &str = "google";

/// Refresh a token this long before it actually expires, so a call that takes a
/// moment to reach Google doesn't arrive with a just-expired token.
const EXPIRY_SKEW_MS: i64 = 60_000;

/// One lock per credential id, so two workflow runs that notice the same
/// expired token at the same moment don't both refresh it and race each other's
/// write-back. Entries are never removed: they are two pointers each, and the
/// key set is bounded by the number of connected accounts.
static REFRESH_LOCKS: Lazy<Mutex<HashMap<String, Arc<Mutex<()>>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

async fn refresh_lock(credential_id: &str) -> Arc<Mutex<()>> {
    let mut locks = REFRESH_LOCKS.lock().await;
    locks.entry(credential_id.to_string()).or_default().clone()
}

/// A connected account's stored secrets, as held in the credential's encrypted
/// `data` blob.
struct StoredAccount {
    access_token: String,
    refresh_token: String,
    expires_at: i64,
    /// Everything else in the blob (`email`, `name`, …), preserved verbatim on
    /// write-back so a refresh never drops fields this module doesn't know about.
    rest: serde_json::Map<String, Value>,
}

impl StoredAccount {
    fn is_fresh(&self) -> bool {
        !self.access_token.is_empty()
            && chrono::Utc::now().timestamp_millis() < self.expires_at - EXPIRY_SKEW_MS
    }
}

/// Read and decrypt one Google credential. `Ok(None)` means "no such row".
fn load(state: &AppState, credential_id: &str) -> Result<Option<StoredAccount>, String> {
    let conn = state.db.get().map_err(|e| e.to_string())?;
    let row = conn
        .query_row(
            "SELECT service, data FROM credentials WHERE id = ?1",
            [credential_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .ok();
    let (service, data) = match row {
        Some(v) => v,
        None => return Ok(None),
    };
    if !service.eq_ignore_ascii_case(SERVICE) {
        return Err(format!(
            "Credential '{credential_id}' is a '{service}' credential, not a Google account."
        ));
    }

    // Encrypted at rest; decrypt_key passes legacy plaintext through.
    let parsed: Value = serde_json::from_str(&crate::crypto::decrypt_key(&data))
        .map_err(|e| format!("Google credential '{credential_id}' is unreadable: {e}"))?;
    let mut rest = match parsed {
        Value::Object(map) => map,
        _ => {
            return Err(format!(
                "Google credential '{credential_id}' is not an object."
            ))
        }
    };

    let take_str = |map: &mut serde_json::Map<String, Value>, key: &str| -> String {
        map.remove(key)
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default()
    };
    let access_token = take_str(&mut rest, "access_token");
    let refresh_token = take_str(&mut rest, "refresh_token");
    let expires_at = rest
        .remove("expires_at")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        })
        .unwrap_or(0);

    Ok(Some(StoredAccount {
        access_token,
        refresh_token,
        expires_at,
        rest,
    }))
}

/// Persist a freshly refreshed token back onto the credential.
fn store(state: &AppState, credential_id: &str, account: &StoredAccount) -> Result<(), String> {
    let mut data = account.rest.clone();
    data.insert("access_token".into(), json!(account.access_token));
    data.insert("refresh_token".into(), json!(account.refresh_token));
    data.insert("expires_at".into(), json!(account.expires_at));

    let blob = crate::crypto::encrypt_key(&Value::Object(data).to_string());
    let conn = state.db.get().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE credentials SET data = ?1 WHERE id = ?2",
        rusqlite::params![blob, credential_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Turn a selected `credential_id` into a live access token.
///
/// An empty id means "no account picked" and yields `Ok(None)` — the caller then
/// runs on the globally signed-in account. An id that names a missing or broken
/// credential is an error rather than a silent fallback: quietly sending mail
/// from the wrong inbox is worse than failing the node.
pub async fn resolve(state: &AppState, credential_id: &str) -> Result<Option<String>, String> {
    let credential_id = credential_id.trim();
    if credential_id.is_empty() {
        return Ok(None);
    }

    let missing = || {
        format!(
            "Google account credential '{credential_id}' not found. Pick an account in this node, \
             or click Connect to add one."
        )
    };

    let account = load(state, credential_id)?.ok_or_else(missing)?;
    if account.is_fresh() {
        return Ok(Some(account.access_token));
    }
    if account.refresh_token.is_empty() {
        return Err(format!(
            "Google account credential '{credential_id}' has no refresh token. \
             Reconnect the account from this node's Connect button."
        ));
    }

    // Serialize concurrent refreshes of this credential, then re-read: whoever
    // held the lock first may already have refreshed it for us.
    let lock = refresh_lock(credential_id).await;
    let _guard = lock.lock().await;
    let account = load(state, credential_id)?.ok_or_else(missing)?;
    if account.is_fresh() {
        return Ok(Some(account.access_token));
    }

    let core = state
        .mcp
        .inprocess_state()
        .await
        .ok_or_else(|| "Google backend is not available".to_string())?;
    let refreshed = axon_google::auth::refresh_account(&core, &account.refresh_token)
        .await
        .map_err(|e| {
            format!(
                "Could not refresh Google account credential '{credential_id}': {e}. \
                 Reconnect the account if access was revoked."
            )
        })?;

    let account = StoredAccount {
        access_token: refreshed.access_token,
        // Google usually omits refresh_token on refresh; oauth::refresh_token
        // carries the existing one forward, so this never blanks it.
        refresh_token: refreshed
            .refresh_token
            .unwrap_or_else(|| account.refresh_token.clone()),
        expires_at: refreshed.expires_at,
        rest: account.rest,
    };
    // A failed write-back only costs an extra refresh next time, so it must not
    // fail the node that is otherwise ready to run.
    if let Err(e) = store(state, credential_id, &account) {
        tracing::warn!("Google account '{credential_id}': token refresh not persisted: {e}");
    }
    Ok(Some(account.access_token))
}

/// Run `fut` authenticated as the account named by `credential_id`.
///
/// An empty id runs `fut` on the globally signed-in account, so callers can pass
/// whatever the node config holds without branching.
///
/// Note that the scope does not cross `tokio::spawn`; anything spawned inside
/// `fut` reverts to the global account.
pub async fn scoped<T, F>(state: &AppState, credential_id: &str, fut: F) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, String>>,
{
    let token = resolve(state, credential_id).await?;
    axon_core::google_account::scoped(token, fut).await
}

/// The Google account selected on a node, as a plain string (`""` when unset).
pub fn credential_id_of(config: &Value) -> String {
    config
        .get("credential_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}
