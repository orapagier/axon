//! Credential-at-rest encryption.
//!
//! Secrets (model API keys, SSH passwords/keys, MCP keys) are encrypted with
//! AES-256-GCM before they touch SQLite. The key is derived from
//! `AXON_MASTER_KEY`.
//!
//! ## Schemes
//! * **v3 (current):** key = scrypt(`AXON_MASTER_KEY`, salt) under a fresh
//!   random 16-byte salt **per stored value**, so no two credentials ever share
//!   a key. Stored as `v3:<salt_b64>:<nonce||ciphertext b64>`.
//! * **v2:** key = SHA-256(`AXON_MASTER_KEY`) → always 32 bytes for any
//!   input length. Stored tagged `v2:`. Still *readable*; upgraded to v3 on
//!   boot ([`reencrypt_v2_secrets`]) and during rotation.
//! * **v1 (legacy):** key = the master string truncated/zero-padded to 32 bytes.
//!   Ciphertext is untagged base64. Still *readable* (so upgrades are seamless)
//!   but never written anymore.
//!
//! ## Rotation
//! Set `AXON_MASTER_KEY_OLD` to the previous key for one boot and every stored
//! secret is re-encrypted under the new `AXON_MASTER_KEY` (see
//! [`rekey_if_requested`]); then remove the variable. Without it, changing the
//! master key leaves all stored credentials unreadable.
//!
//! ## Fail-closed behavior
//! * Boot refuses to run without a real `AXON_MASTER_KEY`, or on a weak one,
//!   unless `AXON_DEV=1` (see [`validate_master_key`]).
//! * Decryption of a `v2:` value under the wrong key returns `""` ("credential
//!   needs re-entry"), never the raw ciphertext. The old code returned the
//!   ciphertext as if it were the plaintext secret, which silently corrupted
//!   downstream calls.

use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use sha2::{Digest, Sha256};
use std::env;

/// Marks a value encrypted under the current (v3, salted scrypt KDF) scheme.
const V3_PREFIX: &str = "v3:";

/// Salt length in bytes for the v3 per-value KDF.
const V3_SALT_BYTES: usize = 16;

/// Marks a value encrypted under the v2 (KDF-derived) scheme. Untagged
/// blobs are treated as legacy v1 ciphertext or genuine plaintext.
const V2_PREFIX: &str = "v2:";

/// The public development key used when `AXON_MASTER_KEY` is unset. It protects
/// nothing — boot refuses it outside `AXON_DEV=1`.
const DEV_DEFAULT_KEY: &str = "00000000000000000000000000000000";

/// Test-only serialization guard for the process-global `AXON_MASTER_KEY`.
/// Crypto tests run against whatever key the env gives them (usually the dev
/// default); any test that *changes* that variable must hold this for its
/// whole run or it silently re-keys another test mid-flight.
#[cfg(test)]
pub(crate) fn master_key_guard() -> std::sync::MutexGuard<'static, ()> {
    static GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
    GUARD.lock().unwrap_or_else(|e| e.into_inner())
}

/// Secret columns re-encrypted from v1 → v2 on boot: `(table, secret_column)`.
/// Every listed table has an implicit `rowid` (none are `WITHOUT ROWID`).
const SECRET_COLUMNS: &[(&str, &str)] = &[
    ("models", "api_key"),
    ("ssh_servers", "password"),
    ("ssh_servers", "private_key"),
    ("mcp_servers", "api_key"),
];

fn master_secret() -> String {
    env::var("AXON_MASTER_KEY").unwrap_or_else(|_| DEV_DEFAULT_KEY.to_string())
}

/// True when no real master key is configured (running on the dev default).
pub fn is_using_default_key() -> bool {
    env::var("AXON_MASTER_KEY")
        .map(|k| k.trim().is_empty())
        .unwrap_or(true)
}

fn dev_mode_allowed() -> bool {
    env::var("AXON_DEV")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Fail-closed startup guard. In production (`AXON_DEV` unset / not truthy) a
/// missing or empty `AXON_MASTER_KEY` is refused, so stored secrets are never
/// protected by the public development key. Call once at startup before any DB
/// access.
pub fn validate_master_key() -> Result<(), String> {
    if is_using_default_key() {
        return if dev_mode_allowed() {
            tracing::warn!(
                "AXON_MASTER_KEY not set — using the INSECURE development key (AXON_DEV=1). \
                 Stored credentials are not protected. Never do this in production."
            );
            Ok(())
        } else {
            Err(
                "AXON_MASTER_KEY is not set. Refusing to start: stored credentials would be \
                 protected only by a public development key. Set AXON_MASTER_KEY to a strong \
                 secret, or set AXON_DEV=1 to explicitly allow the insecure default for local \
                 development."
                    .to_string(),
            )
        };
    }

    let key = env::var("AXON_MASTER_KEY").unwrap_or_default();
    let key = key.trim();

    if let Some(reason) = weak_key_reason(key) {
        return if dev_mode_allowed() {
            tracing::warn!(
                "AXON_MASTER_KEY is weak ({reason}) — allowed only because AXON_DEV=1. \
                 Never do this in production."
            );
            Ok(())
        } else {
            Err(format!(
                "AXON_MASTER_KEY is weak ({reason}). Refusing to start.\n\
                 \n\
                 This key is the dashboard's only credential AND the encryption key for every \
                 stored secret, so a guessable value exposes all of them.\n\
                 \n\
                 Generate a strong one:  openssl rand -base64 32\n\
                 \n\
                 Changing the key makes existing encrypted credentials unreadable — they must \
                 be re-entered in the dashboard. To start once with the current key instead, \
                 set AXON_DEV=1."
            ))
        };
    }

    Ok(())
}

/// Minimum accepted master-key length. 16 characters is the floor at which an
/// online guessing attack is hopeless even before the dashboard's auth
/// throttle (see `dashboard/auth.rs`) is taken into account.
const MIN_KEY_LEN: usize = 16;

/// Placeholder fragments shipped in docs/examples. A deployment that still has
/// one of these is effectively unauthenticated, since the values are public.
const PLACEHOLDER_FRAGMENTS: &[&str] =
    &["changeme", "your-", "example", "placeholder", "secret123"];

/// `None` when the key is acceptable; otherwise why it was rejected.
fn weak_key_reason(key: &str) -> Option<String> {
    if key.chars().count() < MIN_KEY_LEN {
        return Some(format!(
            "{} characters, minimum is {MIN_KEY_LEN}",
            key.chars().count()
        ));
    }
    let lowered = key.to_ascii_lowercase();
    for fragment in PLACEHOLDER_FRAGMENTS {
        if lowered.contains(fragment) {
            return Some(format!("contains the placeholder text '{fragment}'"));
        }
    }
    // A long run of one repeated character ("aaaaaaaa…") passes a length check
    // while carrying almost no entropy.
    if key.chars().collect::<std::collections::HashSet<_>>().len() < 5 {
        return Some("fewer than 5 distinct characters".to_string());
    }
    None
}

/// v2 key: SHA-256 over the master secret → always exactly 32 bytes.
fn derive_key_from(secret: &str) -> aes_gcm::Key<Aes256Gcm> {
    let digest = Sha256::digest(secret.as_bytes());
    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(&digest);
    key_bytes.into()
}

/// v1 legacy key: truncate/zero-pad the master secret to 32 bytes. Read-only.
fn legacy_key_from(secret: &str) -> aes_gcm::Key<Aes256Gcm> {
    let mut key_bytes = [0u8; 32];
    let bytes = secret.as_bytes();
    let len = std::cmp::min(bytes.len(), 32);
    key_bytes[..len].copy_from_slice(&bytes[..len]);
    key_bytes.into()
}

fn derive_key() -> aes_gcm::Key<Aes256Gcm> {
    derive_key_from(&master_secret())
}

fn legacy_key() -> aes_gcm::Key<Aes256Gcm> {
    legacy_key_from(&master_secret())
}

/// scrypt parameters for v3: log_n=12 → 4 MiB ROMix, ~15 ms per derivation.
/// Slow enough that guessing the master key offline (the only real attack on
/// an AES-GCM ciphertext here) costs ~milliseconds per try per salt, yet fast
/// enough to sit unnoticed in per-request credential reads.
fn v3_params() -> Option<scrypt::Params> {
    scrypt::Params::new(12, 8, 1).ok()
}

/// v3 key: scrypt(master secret, per-value salt) → 32 bytes. `None` only on
/// implausible param/output errors, which the constants above never trigger.
fn v3_key_from(secret: &str, salt: &[u8]) -> Option<aes_gcm::Key<Aes256Gcm>> {
    let params = v3_params()?;
    let mut key = [0u8; 32];
    scrypt::scrypt(secret.as_bytes(), salt, &params, &mut key).ok()?;
    Some(key.into())
}

/// AES-256-GCM encrypt → base64(`nonce(12) || ciphertext`). `None` only if the
/// AEAD itself fails (effectively never for reasonable inputs).
fn aes_encrypt(plain: &str, key: &aes_gcm::Key<Aes256Gcm>) -> Option<String> {
    let cipher = Aes256Gcm::new(key);
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng); // 96-bit, unique per message
    let ciphertext = cipher.encrypt(&nonce, plain.as_bytes()).ok()?;
    let mut combined = nonce.to_vec();
    combined.extend_from_slice(&ciphertext);
    Some(STANDARD.encode(combined))
}

/// Decrypt a base64(`nonce || ciphertext`) blob. `None` on any failure
/// (not base64, too short, wrong key / tampered, or non-UTF-8 plaintext).
fn aes_decrypt(blob: &str, key: &aes_gcm::Key<Aes256Gcm>) -> Option<String> {
    let decoded = STANDARD.decode(blob).ok()?;
    if decoded.len() < 12 {
        return None;
    }
    let cipher = Aes256Gcm::new(key);
    let nonce = Nonce::from_slice(&decoded[..12]);
    let ciphertext = &decoded[12..];
    let plaintext = cipher.decrypt(nonce, ciphertext).ok()?;
    String::from_utf8(plaintext).ok()
}

/// Split a v3 body (`salt_b64:blob_b64`) into its salt bytes and ciphertext
/// blob. `None` on any structural or base64 problem (treated as undecryptable).
fn split_v3(rest: &str) -> Option<(Vec<u8>, &str)> {
    let (salt_b64, blob) = rest.split_once(':')?;
    let salt = STANDARD.decode(salt_b64).ok()?;
    if salt.is_empty() {
        return None;
    }
    Some((salt, blob))
}

/// Encrypt a secret for storage. Empty in → empty out. Output is `v3:`-tagged
/// with a fresh random per-value salt.
pub fn encrypt_key(plain: &str) -> String {
    if plain.is_empty() {
        return String::new();
    }
    let mut salt = [0u8; V3_SALT_BYTES];
    use aes_gcm::aead::rand_core::RngCore;
    OsRng.fill_bytes(&mut salt);
    let Some(key) = v3_key_from(&master_secret(), &salt) else {
        tracing::error!("Encryption failed (KDF); refusing to store the secret as plaintext");
        return String::new();
    };
    match aes_encrypt(plain, &key) {
        Some(blob) => format!("{V3_PREFIX}{}:{blob}", STANDARD.encode(salt)),
        None => {
            // Encryption effectively never fails; fail closed (never persist
            // plaintext masquerading as an encrypted value).
            tracing::error!("Encryption failed; refusing to store the secret as plaintext");
            String::new()
        }
    }
}

/// Encrypt `plain` under an explicit master `secret` with the v3 scheme.
/// Test-only helper: the rotation path needs to fabricate values that only the
/// *old* key can read.
#[cfg(test)]
fn v3_encrypt_under(secret: &str, plain: &str) -> Option<String> {
    let mut salt = [0u8; V3_SALT_BYTES];
    use aes_gcm::aead::rand_core::RngCore;
    OsRng.fill_bytes(&mut salt);
    let key = v3_key_from(secret, &salt)?;
    let blob = aes_encrypt(plain, &key)?;
    Some(format!("{V3_PREFIX}{}:{blob}", STANDARD.encode(salt)))
}

/// Decrypt a stored secret. Backward compatible across all three schemes:
/// * `v3:` → per-value salted scrypt key; on failure returns `""` (fail-closed).
/// * `v2:` → KDF key; on failure returns `""` (fail-closed, "re-enter").
/// * untagged → try the legacy key, then the KDF key; if neither authenticates,
///   the value was never our ciphertext, so it is returned as-is (a genuine
///   plaintext key stored before encryption existed).
pub fn decrypt_key(encoded: &str) -> String {
    if encoded.is_empty() {
        return String::new();
    }

    if let Some(rest) = encoded.strip_prefix(V3_PREFIX) {
        let (salt, blob) = match split_v3(rest) {
            Some(x) => x,
            None => {
                tracing::warn!("Malformed v3 credential (bad salt); needs re-entry.");
                return String::new();
            }
        };
        if let Some(key) = v3_key_from(&master_secret(), &salt) {
            if let Some(plain) = aes_decrypt(blob, &key) {
                return plain;
            }
        }
        tracing::warn!(
            "Credential decryption failed: AXON_MASTER_KEY differs from the key used to encrypt \
             this secret. The credential must be re-entered."
        );
        return String::new();
    }

    if let Some(blob) = encoded.strip_prefix(V2_PREFIX) {
        if let Some(plain) = aes_decrypt(blob, &derive_key()) {
            return plain;
        }
        tracing::warn!(
            "Credential decryption failed: AXON_MASTER_KEY differs from the key used to encrypt \
             this secret. The credential must be re-entered."
        );
        return String::new();
    }

    // Untagged: legacy ciphertext or plaintext.
    if let Some(plain) = aes_decrypt(encoded, &legacy_key()) {
        return plain;
    }
    if let Some(plain) = aes_decrypt(encoded, &derive_key()) {
        return plain;
    }
    encoded.to_string()
}

/// Env var holding the *previous* master key during a rotation. Set it
/// alongside the new `AXON_MASTER_KEY` for one boot, then remove it.
const OLD_KEY_VAR: &str = "AXON_MASTER_KEY_OLD";

/// Every column that stores a value encrypted under the master key.
///
/// Deliberately a superset of [`SECRET_COLUMNS`] (which only lists the columns
/// that ever held *v1* ciphertext). A re-key pass has to cover all of these or
/// rotating the key silently orphans whatever it misses — `credentials.data`
/// holds every Services-page credential, and `settings.value` holds the
/// provider API keys seeded from `.env`.
const ENCRYPTED_COLUMNS: &[(&str, &str)] = &[
    ("models", "api_key"),
    ("ssh_servers", "password"),
    ("ssh_servers", "private_key"),
    ("mcp_servers", "api_key"),
    ("credentials", "data"),
    ("settings", "value"),
    ("devices", "bearer_token"),
];

/// Was this column ever written under the v1 (untagged) scheme? Only those get
/// the untagged-value trial decryption; elsewhere an untagged value is genuine
/// plaintext (a numeric setting, say) and must be left alone.
fn had_legacy_scheme(table: &str, col: &str) -> bool {
    SECRET_COLUMNS.contains(&(table, col))
}

/// Result of a rotation pass, for logging and tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RekeyOutcome {
    /// Re-encrypted from the old key to the current one.
    pub rekeyed: usize,
    /// Already readable under the current key — a re-run is a no-op.
    pub already_current: usize,
    /// Decrypted under neither key. Left untouched; needs manual re-entry.
    pub undecryptable: usize,
}

/// Rotate stored secrets from `old_secret` onto the current `AXON_MASTER_KEY`.
///
/// Runs when `AXON_MASTER_KEY_OLD` is set, before anything reads a credential.
/// Idempotent: values that already decrypt under the current key are counted
/// and skipped, so leaving the variable set for an extra boot does no harm.
///
/// Fail-safe by construction — a value is only overwritten once it has been
/// successfully decrypted and re-encrypted. AES-GCM authenticates, so a wrong
/// guess fails rather than yielding garbage, and anything unreadable is
/// reported and left exactly as it was rather than being destroyed.
pub fn rekey_secrets(conn: &rusqlite::Connection, old_secret: &str) -> RekeyOutcome {
    let current_secret = master_secret();
    let current_v3 = |salt: &[u8]| v3_key_from(&current_secret, salt);
    let old_v3 = |salt: &[u8]| v3_key_from(old_secret, salt);
    let old_v2 = derive_key_from(old_secret);
    let old_v1 = legacy_key_from(old_secret);
    let current = derive_key();
    let mut out = RekeyOutcome::default();

    for (table, col) in ENCRYPTED_COLUMNS {
        // Table may be absent on a partially-migrated DB; skip quietly.
        let rows: Vec<(i64, String)> = match conn.prepare(&format!(
            "SELECT rowid, {col} FROM {table} WHERE {col} IS NOT NULL AND {col} != ''"
        )) {
            Ok(mut stmt) => stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
                .and_then(|it| it.collect::<Result<Vec<_>, _>>())
                .unwrap_or_default(),
            Err(_) => continue,
        };

        for (rowid, value) in rows {
            let plain = if let Some(rest) = value.strip_prefix(V3_PREFIX) {
                // v3: per-value salted scrypt key, current then old.
                let (salt, blob) = match split_v3(rest) {
                    Some(x) => x,
                    None => {
                        out.undecryptable += 1;
                        continue;
                    }
                };
                if current_v3(&salt)
                    .and_then(|k| aes_decrypt(blob, &k))
                    .is_some()
                {
                    out.already_current += 1;
                    continue;
                }
                match old_v3(&salt).and_then(|k| aes_decrypt(blob, &k)) {
                    Some(p) => p,
                    None => {
                        out.undecryptable += 1;
                        tracing::warn!(
                            "Re-key: {table}.{col} rowid {rowid} decrypts under neither the \
                             old nor the current AXON_MASTER_KEY — left untouched, it will \
                             need to be re-entered."
                        );
                        continue;
                    }
                }
            } else if let Some(blob) = value.strip_prefix(V2_PREFIX) {
                if aes_decrypt(blob, &current).is_some() {
                    out.already_current += 1;
                    continue;
                }
                match aes_decrypt(blob, &old_v2) {
                    Some(p) => p,
                    None => {
                        out.undecryptable += 1;
                        tracing::warn!(
                            "Re-key: {table}.{col} rowid {rowid} decrypts under neither the \
                             old nor the current AXON_MASTER_KEY — left untouched, it will \
                             need to be re-entered."
                        );
                        continue;
                    }
                }
            } else if had_legacy_scheme(table, col) {
                // Untagged ciphertext, only meaningful where v1 could exist.
                match aes_decrypt(&value, &old_v1) {
                    Some(p) => p,
                    // Not v1 ciphertext under the old key ⇒ genuine plaintext.
                    // Leave it for reencrypt_legacy_secrets.
                    None => continue,
                }
            } else {
                // Untagged outside the legacy columns: genuine plaintext (a
                // numeric setting, say) — must not be touched.
                continue;
            };

            let reencrypted = encrypt_key(&plain);
            if reencrypted.is_empty() {
                // encrypt_key only yields "" on an AEAD failure; never replace
                // a real secret with an empty value.
                tracing::warn!("Re-key: {table}.{col} rowid {rowid} re-encryption produced empty");
                continue;
            }
            match conn.execute(
                &format!("UPDATE {table} SET {col} = ?1 WHERE rowid = ?2"),
                rusqlite::params![reencrypted, rowid],
            ) {
                Ok(_) => out.rekeyed += 1,
                Err(e) => tracing::warn!("Re-key: {table}.{col} rowid {rowid} update failed: {e}"),
            }
        }
    }

    out
}

/// Boot hook: run a rotation if `AXON_MASTER_KEY_OLD` is present.
pub fn rekey_if_requested(conn: &rusqlite::Connection) {
    let Ok(old) = env::var(OLD_KEY_VAR) else {
        return;
    };
    let old = old.trim().to_string();
    if old.is_empty() {
        return;
    }

    if old == master_secret() {
        tracing::warn!(
            "{OLD_KEY_VAR} matches the current AXON_MASTER_KEY — nothing to rotate. \
             Remove {OLD_KEY_VAR} from the environment."
        );
        return;
    }

    tracing::info!("{OLD_KEY_VAR} is set — rotating stored secrets onto the current key...");
    let out = rekey_secrets(conn, &old);
    tracing::info!(
        "Re-key complete: {} rotated, {} already current, {} unreadable.",
        out.rekeyed,
        out.already_current,
        out.undecryptable
    );
    if out.undecryptable > 0 {
        tracing::warn!(
            "{} stored secret(s) could not be decrypted with either key and must be \
             re-entered in the dashboard.",
            out.undecryptable
        );
    }
    tracing::warn!(
        "Rotation done — REMOVE {OLD_KEY_VAR} from the environment and redeploy. \
         Leaving the old key configured keeps a superseded secret on disk."
    );
}

/// One-shot boot upgrade: rewrite every v2-tagged ciphertext the current key
/// can still read to the v3 (per-value salt) scheme. Idempotent — rows already
/// tagged `v3:` are excluded, and values that do not authenticate (rotated or
/// foreign keys) are left for [`rekey_if_requested`] or manual re-entry.
/// Covers [`ENCRYPTED_COLUMNS`], not just the legacy ones: v2 wrote everywhere.
pub fn reencrypt_v2_secrets(conn: &rusqlite::Connection) -> usize {
    let v2_key = derive_key();
    let mut upgraded = 0usize;

    for (table, col) in ENCRYPTED_COLUMNS {
        // Table may not exist on partially-migrated DBs; skip quietly.
        let rows: Vec<(i64, String)> = match conn.prepare(&format!(
            "SELECT rowid, {col} FROM {table} WHERE {col} LIKE '{V2_PREFIX}%'"
        )) {
            Ok(mut stmt) => stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
                .and_then(|it| it.collect::<Result<Vec<_>, _>>())
                .unwrap_or_default(),
            Err(_) => continue,
        };

        for (rowid, value) in rows {
            let Some(blob) = value.strip_prefix(V2_PREFIX) else {
                continue;
            };
            let Some(plain) = aes_decrypt(blob, &v2_key) else {
                continue;
            };
            let reencrypted = encrypt_key(&plain);
            if reencrypted.is_empty() {
                continue;
            }
            match conn.execute(
                &format!("UPDATE {table} SET {col} = ?1 WHERE rowid = ?2"),
                rusqlite::params![reencrypted, rowid],
            ) {
                Ok(_) => upgraded += 1,
                Err(e) => tracing::warn!("v2→v3 upgrade {table}.{col} rowid {rowid} failed: {e}"),
            }
        }
    }

    if upgraded > 0 {
        tracing::info!("Upgraded {upgraded} stored secret(s) from v2 to the v3 (salted) scheme");
    }
    upgraded
}

/// One-shot boot upgrade: rewrite every legacy (v1, untagged) ciphertext in the
/// known secret columns to the v3 (salted KDF) scheme. Idempotent — already-tagged
/// values and genuine plaintext (which does not authenticate under the legacy
/// key) are left untouched. Best-effort: individual failures are logged, not
/// fatal. Returns the number of values upgraded.
pub fn reencrypt_legacy_secrets(conn: &rusqlite::Connection) -> usize {
    let legacy = legacy_key();
    let mut upgraded = 0usize;

    for (table, col) in SECRET_COLUMNS {
        // Table may not exist on partially-migrated DBs; skip quietly.
        let rows: Vec<(i64, String)> = match conn.prepare(&format!(
            "SELECT rowid, {col} FROM {table} \
             WHERE {col} IS NOT NULL AND {col} != '' \
               AND {col} NOT LIKE '{V2_PREFIX}%' AND {col} NOT LIKE '{V3_PREFIX}%'"
        )) {
            Ok(mut stmt) => stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
                .and_then(|it| it.collect::<Result<Vec<_>, _>>())
                .unwrap_or_default(),
            Err(_) => continue,
        };

        for (rowid, value) in rows {
            // Only upgrade values that authenticate as legacy ciphertext; leave
            // plaintext / foreign values alone.
            let Some(plain) = aes_decrypt(&value, &legacy) else {
                continue;
            };
            let reencrypted = encrypt_key(&plain);
            if reencrypted.is_empty() {
                continue;
            }
            match conn.execute(
                &format!("UPDATE {table} SET {col} = ?1 WHERE rowid = ?2"),
                rusqlite::params![reencrypted, rowid],
            ) {
                Ok(_) => upgraded += 1,
                Err(e) => tracing::warn!("re-encrypt {table}.{col} rowid {rowid} failed: {e}"),
            }
        }
    }

    if upgraded > 0 {
        tracing::info!("Upgraded {upgraded} stored secret(s) to the v2 encryption scheme");
    }
    upgraded
}

/// One-shot boot upgrade for the `credentials` table, whose `data` column holds
/// a JSON object of secret fields that was historically stored **in plaintext**.
///
/// Unlike [`reencrypt_legacy_secrets`], an untagged value here is a genuine
/// plaintext blob (this column was never previously encrypted), so it is
/// encrypted rather than skipped. Idempotent: rows already carrying the `v2:`
/// tag are left alone, so re-running on every boot is a no-op after the first.
/// The read path ([`decrypt_key`]) still accepts either form, so this migration
/// is not required for correctness — it just removes plaintext at rest.
pub fn encrypt_credentials_at_rest(conn: &rusqlite::Connection) -> usize {
    let rows: Vec<(String, String)> = match conn.prepare(&format!(
        "SELECT id, data FROM credentials \
         WHERE data IS NOT NULL AND data != '' \
           AND data NOT LIKE '{V2_PREFIX}%' AND data NOT LIKE '{V3_PREFIX}%'"
    )) {
        Ok(mut stmt) => stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .and_then(|it| it.collect::<Result<Vec<_>, _>>())
            .unwrap_or_default(),
        // Table absent on a partially-migrated DB — nothing to do.
        Err(_) => return 0,
    };

    let mut encrypted = 0usize;
    for (id, data) in rows {
        let ciphertext = encrypt_key(&data);
        // encrypt_key only returns "" on the (effectively impossible) AEAD
        // failure; never overwrite a real blob with an empty one.
        if ciphertext.is_empty() {
            tracing::warn!("skipping credential {id}: encryption produced an empty value");
            continue;
        }
        match conn.execute(
            "UPDATE credentials SET data = ?1 WHERE id = ?2",
            rusqlite::params![ciphertext, id],
        ) {
            Ok(_) => encrypted += 1,
            Err(e) => tracing::warn!("encrypt credential {id} at rest failed: {e}"),
        }
    }

    if encrypted > 0 {
        tracing::info!("Encrypted {encrypted} plaintext credential blob(s) at rest");
    }
    encrypted
}

#[cfg(test)]
mod rekey_tests {
    use super::*;

    /// The key the secrets were encrypted under before the rotation. The
    /// *current* key is whatever `master_secret()` returns, so no test has to
    /// mutate the environment (which would race across parallel tests).
    const OLD: &str = "the-previous-master-key-value";

    fn schema() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE models (name TEXT PRIMARY KEY, api_key TEXT NOT NULL);
             CREATE TABLE ssh_servers (name TEXT PRIMARY KEY, password TEXT, private_key TEXT);
             CREATE TABLE mcp_servers (name TEXT PRIMARY KEY, api_key TEXT);
             CREATE TABLE credentials (id TEXT PRIMARY KEY, data TEXT NOT NULL);
             CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )
        .unwrap();
        conn
    }

    /// A `v2:` value encrypted under an arbitrary master secret.
    fn v2_under(secret: &str, plain: &str) -> String {
        format!(
            "{V2_PREFIX}{}",
            aes_encrypt(plain, &derive_key_from(secret)).unwrap()
        )
    }

    fn get(conn: &rusqlite::Connection, sql: &str) -> String {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    // The whole point: every one of the six encrypted locations must survive a
    // rotation. Missing one silently orphans those credentials.
    #[test]
    fn rotates_every_encrypted_column() {
        let _g = super::master_key_guard();
        let conn = schema();
        conn.execute(
            "INSERT INTO models (name, api_key) VALUES ('m', ?1)",
            [v2_under(OLD, "model-key")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ssh_servers (name, password, private_key) VALUES ('s', ?1, ?2)",
            [v2_under(OLD, "ssh-pass"), v2_under(OLD, "ssh-priv")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mcp_servers (name, api_key) VALUES ('x', ?1)",
            [v2_under(OLD, "mcp-key")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO credentials (id, data) VALUES ('c', ?1)",
            [v2_under(OLD, r#"{"token":"tg-secret"}"#)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('GROQ_API_KEY', ?1)",
            [v2_under(OLD, "gsk-live")],
        )
        .unwrap();

        let out = rekey_secrets(&conn, OLD);
        assert_eq!(out.rekeyed, 6, "all six encrypted values rotated");
        assert_eq!(out.undecryptable, 0);

        // Each now reads back under the CURRENT key.
        assert_eq!(
            decrypt_key(&get(&conn, "SELECT api_key FROM models")),
            "model-key"
        );
        assert_eq!(
            decrypt_key(&get(&conn, "SELECT password FROM ssh_servers")),
            "ssh-pass"
        );
        assert_eq!(
            decrypt_key(&get(&conn, "SELECT private_key FROM ssh_servers")),
            "ssh-priv"
        );
        assert_eq!(
            decrypt_key(&get(&conn, "SELECT api_key FROM mcp_servers")),
            "mcp-key"
        );
        assert_eq!(
            decrypt_key(&get(&conn, "SELECT data FROM credentials")),
            r#"{"token":"tg-secret"}"#
        );
        assert_eq!(
            decrypt_key(&get(&conn, "SELECT value FROM settings")),
            "gsk-live"
        );
    }

    // Leaving AXON_MASTER_KEY_OLD set for an extra boot must be harmless.
    #[test]
    fn is_idempotent() {
        let _g = super::master_key_guard();
        let conn = schema();
        conn.execute(
            "INSERT INTO models (name, api_key) VALUES ('m', ?1)",
            [v2_under(OLD, "model-key")],
        )
        .unwrap();

        assert_eq!(rekey_secrets(&conn, OLD).rekeyed, 1);
        let second = rekey_secrets(&conn, OLD);
        assert_eq!(second.rekeyed, 0, "nothing left to rotate");
        assert_eq!(second.already_current, 1);
        assert_eq!(
            decrypt_key(&get(&conn, "SELECT api_key FROM models")),
            "model-key"
        );
    }

    // A value readable under neither key must be reported and LEFT ALONE —
    // never overwritten with an empty or garbage value.
    #[test]
    fn unreadable_values_are_preserved_not_destroyed() {
        let _g = super::master_key_guard();
        let conn = schema();
        let orphan = v2_under("some-third-unrelated-key", "unrecoverable");
        conn.execute(
            "INSERT INTO models (name, api_key) VALUES ('m', ?1)",
            [orphan.clone()],
        )
        .unwrap();

        let out = rekey_secrets(&conn, OLD);
        assert_eq!(out.undecryptable, 1);
        assert_eq!(out.rekeyed, 0);
        assert_eq!(
            get(&conn, "SELECT api_key FROM models"),
            orphan,
            "value must survive byte-for-byte"
        );
    }

    // settings.value holds ordinary config ("30", "true") alongside encrypted
    // provider keys. Untagged values there must never be touched.
    #[test]
    fn plaintext_settings_are_untouched() {
        let _g = super::master_key_guard();
        let conn = schema();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('router.timeout', '30'), ('a.b', 'true')",
            [],
        )
        .unwrap();

        let out = rekey_secrets(&conn, OLD);
        assert_eq!(out.rekeyed, 0);
        assert_eq!(
            get(
                &conn,
                "SELECT value FROM settings WHERE key='router.timeout'"
            ),
            "30"
        );
        assert_eq!(
            get(&conn, "SELECT value FROM settings WHERE key='a.b'"),
            "true"
        );
    }

    // Untagged v1 ciphertext written under the OLD key still has to come across;
    // it is only trial-decrypted in columns that ever used the v1 scheme.
    #[test]
    fn legacy_v1_under_old_key_is_rotated() {
        let _g = super::master_key_guard();
        let conn = schema();
        let v1 = aes_encrypt("old-legacy-value", &legacy_key_from(OLD)).unwrap();
        assert!(!v1.starts_with(V2_PREFIX));
        conn.execute("INSERT INTO models (name, api_key) VALUES ('m', ?1)", [&v1])
            .unwrap();

        assert_eq!(rekey_secrets(&conn, OLD).rekeyed, 1);
        let stored = get(&conn, "SELECT api_key FROM models");
        assert!(stored.starts_with(V3_PREFIX), "upgraded to v3 as well");
        assert_eq!(decrypt_key(&stored), "old-legacy-value");
    }

    // credentials.data was historically plaintext JSON, never v1 ciphertext, so
    // an untagged blob there must be left for encrypt_credentials_at_rest.
    #[test]
    fn untagged_outside_legacy_columns_is_skipped() {
        let _g = super::master_key_guard();
        let conn = schema();
        conn.execute(
            "INSERT INTO credentials (id, data) VALUES ('c', ?1)",
            [r#"{"plain":"json"}"#],
        )
        .unwrap();

        assert_eq!(rekey_secrets(&conn, OLD).rekeyed, 0);
        assert_eq!(
            get(&conn, "SELECT data FROM credentials"),
            r#"{"plain":"json"}"#
        );
    }

    // A missing table (partially-migrated DB) must not abort the pass.
    #[test]
    fn missing_tables_are_skipped() {
        let _g = super::master_key_guard();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE models (name TEXT PRIMARY KEY, api_key TEXT NOT NULL);")
            .unwrap();
        conn.execute(
            "INSERT INTO models (name, api_key) VALUES ('m', ?1)",
            [v2_under(OLD, "k")],
        )
        .unwrap();
        assert_eq!(rekey_secrets(&conn, OLD).rekeyed, 1);
    }

    // A v3 value written under the OLD key (per-value salt rides in the blob)
    // must rotate onto the current key like any other scheme.
    #[test]
    fn v3_under_old_key_is_rotated() {
        let _g = super::master_key_guard();
        let conn = schema();
        let v3_old = v3_encrypt_under(OLD, "salted-value").unwrap();
        conn.execute(
            "INSERT INTO models (name, api_key) VALUES ('m', ?1)",
            [v3_old],
        )
        .unwrap();

        let out = rekey_secrets(&conn, OLD);
        assert_eq!(out.rekeyed, 1);
        let stored = get(&conn, "SELECT api_key FROM models");
        assert!(stored.starts_with(V3_PREFIX), "stays v3");
        assert_eq!(decrypt_key(&stored), "salted-value");
    }

    // Guards the superset relationship: every legacy column must also be in the
    // re-key list, or a rotation would skip it.
    #[test]
    fn encrypted_columns_covers_secret_columns() {
        let _g = super::master_key_guard();
        for pair in SECRET_COLUMNS {
            assert!(
                ENCRYPTED_COLUMNS.contains(pair),
                "{pair:?} is in SECRET_COLUMNS but missing from ENCRYPTED_COLUMNS"
            );
        }
    }
}

#[cfg(test)]
mod key_strength_tests {
    use super::weak_key_reason;

    #[test]
    fn rejects_short_keys() {
        let _g = super::master_key_guard();
        assert!(weak_key_reason("short").is_some());
        assert!(weak_key_reason("123456789012345").is_some(), "15 chars");
    }

    #[test]
    fn rejects_shipped_placeholders() {
        let _g = super::master_key_guard();
        // The literal value in crates/axon-agent/.env.example before this change.
        assert!(weak_key_reason("changeme-master-key").is_some());
        assert!(weak_key_reason("your-secret-key-here").is_some());
    }

    #[test]
    fn rejects_low_entropy_padding() {
        let _g = super::master_key_guard();
        assert!(weak_key_reason(&"a".repeat(40)).is_some());
        assert!(weak_key_reason(&"abab".repeat(10)).is_some());
    }

    #[test]
    fn accepts_a_generated_key() {
        let _g = super::master_key_guard();
        // Representative `openssl rand -base64 32` output.
        assert_eq!(
            weak_key_reason("Yy1kQ0hLc2VjdXJlUmFuZG9tS2V5MTIzNA=="),
            None
        );
        assert_eq!(weak_key_reason("f3Kq9-Zx71LmPw2v"), None, "16 chars, mixed");
    }

    // Length is counted in characters, not bytes, so a multi-byte key is not
    // wrongly accepted for being byte-long.
    #[test]
    fn counts_characters_not_bytes() {
        let _g = super::master_key_guard();
        assert!(weak_key_reason("日本語日本語日本語").is_some(), "9 chars");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v3_round_trip() {
        let _g = super::master_key_guard();
        let enc = encrypt_key("super-secret-token");
        assert!(enc.starts_with(V3_PREFIX), "new writes are v3-tagged");
        assert_eq!(decrypt_key(&enc), "super-secret-token");
    }

    #[test]
    fn v3_uses_a_fresh_salt_per_value() {
        let _g = super::master_key_guard();
        // Same plaintext twice must not produce the same ciphertext prefix —
        // the per-value salt is the whole point of v3.
        let a = encrypt_key("same-value");
        let b = encrypt_key("same-value");
        assert_ne!(a, b);
        let salt_of = |v: &str| {
            v.strip_prefix(V3_PREFIX)
                .unwrap()
                .split(':')
                .next()
                .unwrap()
                .to_string()
        };
        assert_ne!(salt_of(&a), salt_of(&b));
        assert_eq!(decrypt_key(&a), "same-value");
        assert_eq!(decrypt_key(&b), "same-value");
    }

    #[test]
    fn v2_values_still_decrypt() {
        let _g = super::master_key_guard();
        let blob = format!(
            "{V2_PREFIX}{}",
            aes_encrypt("old-v2-value", &derive_key()).unwrap()
        );
        assert_eq!(decrypt_key(&blob), "old-v2-value");
    }

    #[test]
    fn v3_wrong_key_fails_closed() {
        let _g = super::master_key_guard();
        let blob = v3_encrypt_under("a-totally-different-master", "secret").unwrap();
        assert_eq!(decrypt_key(&blob), "");
    }

    #[test]
    fn v3_malformed_is_rejected_not_garbage() {
        let _g = super::master_key_guard();
        assert_eq!(decrypt_key("v3:not-base64!!:also-not"), "");
        assert_eq!(decrypt_key("v3:single-part-only"), "");
    }

    #[test]
    fn empty_is_passthrough() {
        let _g = super::master_key_guard();
        assert_eq!(encrypt_key(""), "");
        assert_eq!(decrypt_key(""), "");
    }

    #[test]
    fn plaintext_untagged_value_is_returned_asis() {
        let _g = super::master_key_guard();
        // A raw key that was stored before encryption existed and is not valid
        // base64 ciphertext must survive verbatim.
        assert_eq!(
            decrypt_key("sk-legacy-plaintext-KEY"),
            "sk-legacy-plaintext-KEY"
        );
    }

    #[test]
    fn legacy_v1_ciphertext_still_decrypts() {
        let _g = super::master_key_guard();
        // Simulate a value written by the old truncate/pad scheme (untagged).
        let secret = master_secret();
        let legacy_blob = aes_encrypt("old-value", &legacy_key_from(&secret)).unwrap();
        assert!(!legacy_blob.starts_with(V2_PREFIX));
        assert_eq!(decrypt_key(&legacy_blob), "old-value");
    }

    #[test]
    fn v2_wrong_key_fails_closed() {
        let _g = super::master_key_guard();
        // Encrypt under a different master, then try to read under the current
        // one: must return "" (re-enter), never the ciphertext.
        let blob = aes_encrypt("secret", &derive_key_from("a-totally-different-master")).unwrap();
        let tagged = format!("{V2_PREFIX}{blob}");
        assert_eq!(decrypt_key(&tagged), "");
    }

    #[test]
    fn reencrypt_v2_upgrades_only_current_key_readable() {
        let _g = super::master_key_guard();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE models (name TEXT PRIMARY KEY, api_key TEXT NOT NULL);")
            .unwrap();
        let readable = format!("{V2_PREFIX}{}", aes_encrypt("k1", &derive_key()).unwrap());
        let foreign = format!(
            "{V2_PREFIX}{}",
            aes_encrypt("k2", &derive_key_from("other-master")).unwrap()
        );
        conn.execute(
            "INSERT INTO models (name, api_key) VALUES ('a', ?1), ('b', ?2), ('c', 'raw-plaintext')",
            rusqlite::params![readable, foreign],
        )
        .unwrap();

        assert_eq!(reencrypt_v2_secrets(&conn), 1, "only the readable v2 row");
        let a: String = conn
            .query_row("SELECT api_key FROM models WHERE name='a'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(a.starts_with(V3_PREFIX));
        assert_eq!(decrypt_key(&a), "k1");

        let b: String = conn
            .query_row("SELECT api_key FROM models WHERE name='b'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(b, foreign, "unreadable v2 must survive untouched");

        // Second pass is a no-op (already v3 / excluded).
        assert_eq!(reencrypt_v2_secrets(&conn), 0);
    }

    #[test]
    fn reencrypt_upgrades_legacy_only() {
        let _g = super::master_key_guard();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE models (name TEXT PRIMARY KEY, api_key TEXT NOT NULL);")
            .unwrap();

        let secret = master_secret();
        let legacy_blob = aes_encrypt("legacy-key", &legacy_key_from(&secret)).unwrap();
        let v2_blob = encrypt_key("already-v2");
        conn.execute(
            "INSERT INTO models (name, api_key) VALUES ('a', ?1), ('b', ?2), ('c', 'raw-plaintext')",
            rusqlite::params![legacy_blob, v2_blob],
        )
        .unwrap();

        let upgraded = reencrypt_legacy_secrets(&conn);
        assert_eq!(upgraded, 1, "only the legacy ciphertext is upgraded");

        // Legacy row is now v3 and still decrypts to the same plaintext.
        let a: String = conn
            .query_row("SELECT api_key FROM models WHERE name='a'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(a.starts_with(V3_PREFIX));
        assert_eq!(decrypt_key(&a), "legacy-key");

        // Plaintext row is untouched.
        let c: String = conn
            .query_row("SELECT api_key FROM models WHERE name='c'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(c, "raw-plaintext");

        // Second pass is a no-op.
        assert_eq!(reencrypt_legacy_secrets(&conn), 0);
    }

    #[test]
    fn credentials_at_rest_encrypts_plaintext_only() {
        let _g = super::master_key_guard();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE credentials (id TEXT PRIMARY KEY, data TEXT NOT NULL);")
            .unwrap();

        let plaintext_json = r#"{"access_token":"secret-token","page_id":"42"}"#;
        let already_v2 = encrypt_key(r#"{"api_key":"pre-encrypted"}"#);
        conn.execute(
            "INSERT INTO credentials (id, data) VALUES ('p', ?1), ('v', ?2), ('e', '')",
            rusqlite::params![plaintext_json, already_v2],
        )
        .unwrap();

        let n = encrypt_credentials_at_rest(&conn);
        assert_eq!(n, 1, "only the plaintext row is encrypted");

        // Plaintext row is now v3-tagged and round-trips back to the same JSON.
        let p: String = conn
            .query_row("SELECT data FROM credentials WHERE id='p'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(p.starts_with(V3_PREFIX));
        assert_eq!(decrypt_key(&p), plaintext_json);

        // Already-encrypted row is untouched; empty row is skipped.
        let v: String = conn
            .query_row("SELECT data FROM credentials WHERE id='v'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(v, already_v2);

        // Second pass is a no-op.
        assert_eq!(encrypt_credentials_at_rest(&conn), 0);
    }
}
