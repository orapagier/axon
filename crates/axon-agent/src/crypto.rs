//! Credential-at-rest encryption.
//!
//! Secrets (model API keys, SSH passwords/keys, MCP keys) are encrypted with
//! AES-256-GCM before they touch SQLite. The key is derived from
//! `AXON_MASTER_KEY`.
//!
//! ## Schemes
//! * **v2 (current):** key = SHA-256(`AXON_MASTER_KEY`) → always 32 bytes for any
//!   input length. Ciphertext is stored tagged with a `v2:` prefix.
//! * **v1 (legacy):** key = the master string truncated/zero-padded to 32 bytes.
//!   Ciphertext is untagged base64. Still *readable* (so upgrades are seamless)
//!   but never written anymore.
//!
//! ## Fail-closed behavior
//! * Boot refuses to run without a real `AXON_MASTER_KEY` unless `AXON_DEV=1`
//!   (see [`validate_master_key`]).
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

/// Marks a value encrypted under the current (v2, KDF-derived) scheme. Untagged
/// blobs are treated as legacy v1 ciphertext or genuine plaintext.
const V2_PREFIX: &str = "v2:";

/// The public development key used when `AXON_MASTER_KEY` is unset. It protects
/// nothing — boot refuses it outside `AXON_DEV=1`.
const DEV_DEFAULT_KEY: &str = "00000000000000000000000000000000";

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

/// Encrypt a secret for storage. Empty in → empty out. Output is `v2:`-tagged.
pub fn encrypt_key(plain: &str) -> String {
    if plain.is_empty() {
        return String::new();
    }
    match aes_encrypt(plain, &derive_key()) {
        Some(b64) => format!("{V2_PREFIX}{b64}"),
        None => {
            // Encryption effectively never fails; fail closed (never persist
            // plaintext masquerading as an encrypted value).
            tracing::error!("Encryption failed; refusing to store the secret as plaintext");
            String::new()
        }
    }
}

/// Decrypt a stored secret. Backward compatible across schemes:
/// * `v2:` tagged → KDF key; on failure returns `""` (fail-closed, "re-enter").
/// * untagged → try the legacy key, then the KDF key; if neither authenticates,
///   the value was never our ciphertext, so it is returned as-is (a genuine
///   plaintext key stored before encryption existed).
pub fn decrypt_key(encoded: &str) -> String {
    if encoded.is_empty() {
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

/// One-shot boot upgrade: rewrite every legacy (v1, untagged) ciphertext in the
/// known secret columns to the v2 (KDF) scheme. Idempotent — already-`v2:`
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
             WHERE {col} IS NOT NULL AND {col} != '' AND {col} NOT LIKE '{V2_PREFIX}%'"
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
         WHERE data IS NOT NULL AND data != '' AND data NOT LIKE '{V2_PREFIX}%'"
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
mod key_strength_tests {
    use super::weak_key_reason;

    #[test]
    fn rejects_short_keys() {
        assert!(weak_key_reason("short").is_some());
        assert!(weak_key_reason("123456789012345").is_some(), "15 chars");
    }

    #[test]
    fn rejects_shipped_placeholders() {
        // The literal value in crates/axon-agent/.env.example before this change.
        assert!(weak_key_reason("changeme-master-key").is_some());
        assert!(weak_key_reason("your-secret-key-here").is_some());
    }

    #[test]
    fn rejects_low_entropy_padding() {
        assert!(weak_key_reason(&"a".repeat(40)).is_some());
        assert!(weak_key_reason(&"abab".repeat(10)).is_some());
    }

    #[test]
    fn accepts_a_generated_key() {
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
        assert!(weak_key_reason("日本語日本語日本語").is_some(), "9 chars");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_round_trip() {
        let enc = encrypt_key("super-secret-token");
        assert!(enc.starts_with(V2_PREFIX), "new writes are v2-tagged");
        assert_eq!(decrypt_key(&enc), "super-secret-token");
    }

    #[test]
    fn empty_is_passthrough() {
        assert_eq!(encrypt_key(""), "");
        assert_eq!(decrypt_key(""), "");
    }

    #[test]
    fn plaintext_untagged_value_is_returned_asis() {
        // A raw key that was stored before encryption existed and is not valid
        // base64 ciphertext must survive verbatim.
        assert_eq!(
            decrypt_key("sk-legacy-plaintext-KEY"),
            "sk-legacy-plaintext-KEY"
        );
    }

    #[test]
    fn legacy_v1_ciphertext_still_decrypts() {
        // Simulate a value written by the old truncate/pad scheme (untagged).
        let secret = master_secret();
        let legacy_blob = aes_encrypt("old-value", &legacy_key_from(&secret)).unwrap();
        assert!(!legacy_blob.starts_with(V2_PREFIX));
        assert_eq!(decrypt_key(&legacy_blob), "old-value");
    }

    #[test]
    fn v2_wrong_key_fails_closed() {
        // Encrypt under a different master, then try to read under the current
        // one: must return "" (re-enter), never the ciphertext.
        let blob = aes_encrypt("secret", &derive_key_from("a-totally-different-master")).unwrap();
        let tagged = format!("{V2_PREFIX}{blob}");
        assert_eq!(decrypt_key(&tagged), "");
    }

    #[test]
    fn reencrypt_upgrades_legacy_only() {
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

        // Legacy row is now v2 and still decrypts to the same plaintext.
        let a: String = conn
            .query_row("SELECT api_key FROM models WHERE name='a'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(a.starts_with(V2_PREFIX));
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

        // Plaintext row is now v2-tagged and round-trips back to the same JSON.
        let p: String = conn
            .query_row("SELECT data FROM credentials WHERE id='p'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(p.starts_with(V2_PREFIX));
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
