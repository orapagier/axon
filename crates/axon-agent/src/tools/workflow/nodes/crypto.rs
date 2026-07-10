//! Crypto — Task 2.2 (*Enzyme*). Hash, HMAC, UUID — plus JWT, TOTP, and
//! AES-256-GCM encryption (n8n gap-closure: n8n ships JWT and TOTP as separate
//! nodes and encrypt inside its Crypto node; here they are operations).
//!
//! Operations, one per `operation` config key:
//!   - `hash`         — digest a value (SHA-224/256/384/512).
//!   - `hmac`         — keyed HMAC of a value with a secret (this is the "sign"
//!                      side of webhook verification: compute the HMAC, then
//!                      compare it to the provider's header with an IF node).
//!   - `generateUuid` — a fresh v4 UUID.
//!   - `jwtSign`      — sign a claims object (HS/RS/ES/PS/EdDSA via
//!                      `jsonwebtoken`; PEM keys for the asymmetric algs).
//!   - `jwtVerify`    — verify + decode; soft-fails to `{valid:false}` so an
//!                      IF node can route, hard-fails only on config errors.
//!   - `totp`         — RFC 6238 code from a base32/plain/hex secret
//!                      (SHA1 default, SHA256/512 supported, 6–8 digits).
//!   - `totpVerify`   — constant-time code check across a ±window of periods.
//!   - `encrypt` / `decrypt` — AES-256-GCM with an SHA-256-derived key from a
//!                      passphrase; blob format matches the master-key store:
//!                      base64(nonce(12) ‖ ciphertext+tag).
//!
//! Digest output encodes as `hex` (default — what GitHub/Stripe/Shopify-hex use),
//! `base64`, or `base64url`.
//!
//! Output mirrors Soma/`dateTime`: the result lands under `outputField` and
//! `includeInputFields` decides whether the incoming item's other fields ride
//! along.

use crate::tools::workflow::val_to_string;
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use serde_json::{json, Map, Value};

/// Canonicalize an algorithm name so "SHA-256", "sha_256", and "SHA256" all map
/// to the same key.
fn normalize_algo(algorithm: &str) -> String {
    algorithm
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Raw digest bytes for one of the supported SHA-2 algorithms.
fn hash_bytes(algorithm: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    use sha2::{Digest, Sha224, Sha256, Sha384, Sha512};
    macro_rules! run {
        ($t:ty) => {{
            <$t>::digest(data).to_vec()
        }};
    }
    Ok(match normalize_algo(algorithm).as_str() {
        "sha224" => run!(Sha224),
        "sha256" => run!(Sha256),
        "sha384" => run!(Sha384),
        "sha512" => run!(Sha512),
        other => return Err(format!("Unsupported hash algorithm: {other}")),
    })
}

/// Raw HMAC bytes for one of the supported SHA-2 algorithms. Any key length is
/// valid for HMAC, so `new_from_slice` won't realistically fail.
fn hmac_bytes(algorithm: &str, key: &[u8], data: &[u8]) -> Result<Vec<u8>, String> {
    use hmac::{Hmac, Mac};
    use sha2::{Sha224, Sha256, Sha384, Sha512};
    macro_rules! run {
        ($t:ty) => {{
            let mut mac = <Hmac<$t>>::new_from_slice(key).map_err(|e| e.to_string())?;
            mac.update(data);
            mac.finalize().into_bytes().to_vec()
        }};
    }
    Ok(match normalize_algo(algorithm).as_str() {
        "sha224" => run!(Sha224),
        "sha256" => run!(Sha256),
        "sha384" => run!(Sha384),
        "sha512" => run!(Sha512),
        other => return Err(format!("Unsupported HMAC algorithm: {other}")),
    })
}

/// Encode digest bytes per the `encoding` config; hex is the default.
fn encode(bytes: &[u8], encoding: &str) -> String {
    match encoding {
        "base64" => STANDARD.encode(bytes),
        "base64url" => URL_SAFE_NO_PAD.encode(bytes),
        _ => hex::encode(bytes),
    }
}

/// Config string accessor: trimmed, None when absent/blank.
fn cfg_str(config: &Value, key: &str) -> Option<String> {
    config
        .get(key)
        .map(|v| val_to_string(v))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ── JWT ───────────────────────────────────────────────────────────────────────

fn jwt_algorithm(name: &str) -> Result<jsonwebtoken::Algorithm, String> {
    use std::str::FromStr;
    jsonwebtoken::Algorithm::from_str(name.trim())
        .map_err(|_| format!("Unsupported JWT algorithm: {name}"))
}

/// HS* take the shared `secret`; every asymmetric family takes a PEM
/// `privateKey` (sign) / `publicKey` (verify).
fn jwt_encoding_key(
    alg: jsonwebtoken::Algorithm,
    config: &Value,
) -> Result<jsonwebtoken::EncodingKey, String> {
    use jsonwebtoken::{Algorithm as A, EncodingKey};
    let secret = || {
        cfg_str(config, "secret").ok_or_else(|| "JWT signing needs a 'secret'".to_string())
    };
    let pem = || {
        cfg_str(config, "privateKey")
            .ok_or_else(|| "JWT signing with an asymmetric algorithm needs a PEM 'privateKey'".to_string())
    };
    match alg {
        A::HS256 | A::HS384 | A::HS512 => Ok(EncodingKey::from_secret(secret()?.as_bytes())),
        A::RS256 | A::RS384 | A::RS512 | A::PS256 | A::PS384 | A::PS512 => {
            EncodingKey::from_rsa_pem(pem()?.as_bytes()).map_err(|e| format!("Bad RSA key: {e}"))
        }
        A::ES256 | A::ES384 => {
            EncodingKey::from_ec_pem(pem()?.as_bytes()).map_err(|e| format!("Bad EC key: {e}"))
        }
        A::EdDSA => {
            EncodingKey::from_ed_pem(pem()?.as_bytes()).map_err(|e| format!("Bad Ed25519 key: {e}"))
        }
    }
}

fn jwt_decoding_key(
    alg: jsonwebtoken::Algorithm,
    config: &Value,
) -> Result<jsonwebtoken::DecodingKey, String> {
    use jsonwebtoken::{Algorithm as A, DecodingKey};
    let secret = || {
        cfg_str(config, "secret").ok_or_else(|| "JWT verification needs a 'secret'".to_string())
    };
    let pem = || {
        cfg_str(config, "publicKey")
            .ok_or_else(|| "JWT verification with an asymmetric algorithm needs a PEM 'publicKey'".to_string())
    };
    match alg {
        A::HS256 | A::HS384 | A::HS512 => Ok(DecodingKey::from_secret(secret()?.as_bytes())),
        A::RS256 | A::RS384 | A::RS512 | A::PS256 | A::PS384 | A::PS512 => {
            DecodingKey::from_rsa_pem(pem()?.as_bytes()).map_err(|e| format!("Bad RSA key: {e}"))
        }
        A::ES256 | A::ES384 => {
            DecodingKey::from_ec_pem(pem()?.as_bytes()).map_err(|e| format!("Bad EC key: {e}"))
        }
        A::EdDSA => {
            DecodingKey::from_ed_pem(pem()?.as_bytes()).map_err(|e| format!("Bad Ed25519 key: {e}"))
        }
    }
}

// ── TOTP (RFC 6238) ───────────────────────────────────────────────────────────

/// RFC 4648 base32 decode — case-insensitive, ignores spaces and `=` padding
/// (the format authenticator apps and provisioning URIs use for secrets).
fn base32_decode(s: &str) -> Result<Vec<u8>, String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut bits: u32 = 0;
    let mut bit_count: u32 = 0;
    let mut out = Vec::new();
    for c in s.bytes() {
        if c == b' ' || c == b'=' || c == b'-' {
            continue;
        }
        let idx = ALPHABET
            .iter()
            .position(|&a| a == c.to_ascii_uppercase())
            .ok_or_else(|| format!("Invalid base32 character: {}", c as char))?;
        bits = (bits << 5) | idx as u32;
        bit_count += 5;
        if bit_count >= 8 {
            bit_count -= 8;
            out.push((bits >> bit_count) as u8);
        }
    }
    Ok(out)
}

/// The TOTP secret bytes per `secretEncoding` (base32 default / plain / hex).
fn totp_secret(config: &Value) -> Result<Vec<u8>, String> {
    let secret =
        cfg_str(config, "secret").ok_or_else(|| "TOTP needs a 'secret'".to_string())?;
    match cfg_str(config, "secretEncoding").as_deref().unwrap_or("base32") {
        "plain" => Ok(secret.into_bytes()),
        "hex" => hex::decode(secret.trim()).map_err(|e| format!("Bad hex secret: {e}")),
        _ => base32_decode(&secret),
    }
}

/// One HOTP code (RFC 4226 dynamic truncation) for a counter value.
fn hotp(algorithm: &str, key: &[u8], counter: u64, digits: u32) -> Result<String, String> {
    use hmac::{Hmac, Mac};
    macro_rules! mac {
        ($t:ty) => {{
            let mut m = <Hmac<$t>>::new_from_slice(key).map_err(|e| e.to_string())?;
            m.update(&counter.to_be_bytes());
            m.finalize().into_bytes().to_vec()
        }};
    }
    let digest = match normalize_algo(algorithm).as_str() {
        "sha1" => mac!(sha1::Sha1),
        "sha256" => mac!(sha2::Sha256),
        "sha512" => mac!(sha2::Sha512),
        other => return Err(format!("Unsupported TOTP algorithm: {other}")),
    };
    let offset = (digest[digest.len() - 1] & 0x0f) as usize;
    let bin = ((digest[offset] as u32 & 0x7f) << 24)
        | ((digest[offset + 1] as u32) << 16)
        | ((digest[offset + 2] as u32) << 8)
        | digest[offset + 3] as u32;
    let code = bin % 10u32.pow(digits);
    Ok(format!("{code:0width$}", width = digits as usize))
}

/// TOTP knobs shared by generate and verify: (key, algorithm, digits, period).
fn totp_params(config: &Value) -> Result<(Vec<u8>, String, u32, u64), String> {
    let key = totp_secret(config)?;
    let algorithm = cfg_str(config, "algorithm").unwrap_or_else(|| "sha1".to_string());
    let digits = config
        .get("digits")
        .and_then(|v| v.as_u64())
        .unwrap_or(6)
        .clamp(6, 8) as u32;
    let period = config
        .get("period")
        .and_then(|v| v.as_u64())
        .filter(|p| *p > 0)
        .unwrap_or(30);
    Ok((key, algorithm, digits, period))
}

// ── AES-256-GCM (encrypt/decrypt) ─────────────────────────────────────────────

/// Same construction as the master-key store (crypto.rs): key = SHA-256 of the
/// passphrase, blob = base64(nonce(12) ‖ ciphertext+tag).
fn passphrase_key(passphrase: &str) -> aes_gcm::Key<aes_gcm::Aes256Gcm> {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(passphrase.as_bytes());
    *aes_gcm::Key::<aes_gcm::Aes256Gcm>::from_slice(&digest)
}

fn aes_encrypt_value(plain: &str, passphrase: &str) -> Result<String, String> {
    use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
    use aes_gcm::Aes256Gcm;
    let cipher = Aes256Gcm::new(&passphrase_key(passphrase));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ct = cipher
        .encrypt(&nonce, plain.as_bytes())
        .map_err(|_| "Encryption failed".to_string())?;
    let mut blob = nonce.to_vec();
    blob.extend(ct);
    Ok(STANDARD.encode(blob))
}

fn aes_decrypt_value(blob: &str, passphrase: &str) -> Result<String, String> {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Nonce};
    let decoded = STANDARD
        .decode(blob.trim())
        .map_err(|e| format!("Bad base64 ciphertext: {e}"))?;
    if decoded.len() < 13 {
        return Err("Ciphertext too short".to_string());
    }
    let cipher = Aes256Gcm::new(&passphrase_key(passphrase));
    let plain = cipher
        .decrypt(Nonce::from_slice(&decoded[..12]), &decoded[12..])
        .map_err(|_| "Decryption failed: wrong passphrase or corrupted data".to_string())?;
    String::from_utf8(plain).map_err(|_| "Decrypted data is not valid UTF-8".to_string())
}

/// Wrap a computed result under `outputField` (defaulting to `default_field`),
/// optionally merged onto the incoming item — identical convention to `dateTime`.
fn wrap(config: &Value, input: &Value, default_field: &str, result: Value) -> Value {
    let field = config
        .get("outputField")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_field)
        .to_string();
    let include = config
        .get("includeInputFields")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut out: Map<String, Value> = match (include, input) {
        (true, Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    out.insert(field, result);
    Value::Object(out)
}

pub(crate) fn execute(config: &Value, input: &Value) -> Result<Value, String> {
    let operation = config
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("hash");

    match operation {
        "hash" => {
            let value = val_to_string(&config.get("value").cloned().unwrap_or(Value::Null));
            let algorithm = config
                .get("algorithm")
                .and_then(|v| v.as_str())
                .unwrap_or("sha256");
            let encoding = config
                .get("encoding")
                .and_then(|v| v.as_str())
                .unwrap_or("hex");
            let digest = hash_bytes(algorithm, value.as_bytes())?;
            Ok(wrap(
                config,
                input,
                "hash",
                Value::String(encode(&digest, encoding)),
            ))
        }

        "hmac" => {
            let value = val_to_string(&config.get("value").cloned().unwrap_or(Value::Null));
            let secret = val_to_string(&config.get("secret").cloned().unwrap_or(Value::Null));
            let algorithm = config
                .get("algorithm")
                .and_then(|v| v.as_str())
                .unwrap_or("sha256");
            let encoding = config
                .get("encoding")
                .and_then(|v| v.as_str())
                .unwrap_or("hex");
            let digest = hmac_bytes(algorithm, secret.as_bytes(), value.as_bytes())?;
            Ok(wrap(
                config,
                input,
                "hmac",
                Value::String(encode(&digest, encoding)),
            ))
        }

        "generateUuid" => {
            let id = uuid::Uuid::new_v4().to_string();
            Ok(wrap(config, input, "uuid", Value::String(id)))
        }

        "jwtSign" => {
            let alg = jwt_algorithm(
                cfg_str(config, "algorithm").as_deref().unwrap_or("HS256"),
            )?;
            // Claims: an object, or a JSON string that parses to one.
            let mut claims = match config.get("payload") {
                Some(Value::Object(m)) => m.clone(),
                Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
                    Ok(Value::Object(m)) => m,
                    _ => return Err("JWT 'payload' must be a JSON object".to_string()),
                },
                _ => Map::new(),
            };
            let now = chrono::Utc::now().timestamp();
            // Convenience claims — only filled when the payload doesn't
            // already carry them, so explicit values always win.
            if let Some(exp_in) = config.get("expiresInSeconds").and_then(|v| v.as_i64()) {
                claims
                    .entry("exp".to_string())
                    .or_insert(Value::from(now + exp_in));
            }
            if config
                .get("issuedAt")
                .and_then(|v| v.as_bool())
                .unwrap_or(true)
            {
                claims.entry("iat".to_string()).or_insert(Value::from(now));
            }
            let mut header = jsonwebtoken::Header::new(alg);
            header.kid = cfg_str(config, "keyId");
            let token = jsonwebtoken::encode(&header, &claims, &jwt_encoding_key(alg, config)?)
                .map_err(|e| format!("JWT signing failed: {e}"))?;
            Ok(wrap(config, input, "token", Value::String(token)))
        }

        "jwtVerify" => {
            let token = cfg_str(config, "token")
                .ok_or_else(|| "JWT verification needs a 'token'".to_string())?;
            // Algorithm: explicit config wins; otherwise read the token header
            // (safe — the key type still has to match it).
            let alg = match cfg_str(config, "algorithm") {
                Some(a) => jwt_algorithm(&a)?,
                None => jsonwebtoken::decode_header(&token)
                    .map(|h| h.alg)
                    .map_err(|e| format!("Unreadable JWT header: {e}"))?,
            };
            let key = jwt_decoding_key(alg, config)?;
            let mut validation = jsonwebtoken::Validation::new(alg);
            // exp/nbf are validated when present, but not required claims.
            validation.required_spec_claims = std::collections::HashSet::new();
            validation.leeway = config
                .get("leewaySeconds")
                .and_then(|v| v.as_u64())
                .unwrap_or(60);
            match cfg_str(config, "audience") {
                Some(aud) => validation.set_audience(&[aud]),
                None => validation.validate_aud = false,
            }
            if let Some(iss) = cfg_str(config, "issuer") {
                validation.set_issuer(&[iss]);
            }
            // Soft-fail: {valid:false, error} routes through an IF node; a bad
            // token is data, not a workflow crash.
            let result = match jsonwebtoken::decode::<Value>(&token, &key, &validation) {
                Ok(data) => json!({
                    "valid": true,
                    "payload": data.claims,
                    "header": { "alg": format!("{:?}", data.header.alg), "kid": data.header.kid },
                }),
                Err(e) => json!({ "valid": false, "error": e.to_string() }),
            };
            Ok(wrap(config, input, "jwt", result))
        }

        "totp" => {
            let (key, algorithm, digits, period) = totp_params(config)?;
            let now = chrono::Utc::now().timestamp() as u64;
            let code = hotp(&algorithm, &key, now / period, digits)?;
            Ok(wrap(
                config,
                input,
                "totp",
                json!({ "code": code, "secondsRemaining": period - (now % period) }),
            ))
        }

        "totpVerify" => {
            let (key, algorithm, digits, period) = totp_params(config)?;
            let code = cfg_str(config, "code")
                .ok_or_else(|| "TOTP verification needs a 'code'".to_string())?;
            let window = config
                .get("window")
                .and_then(|v| v.as_i64())
                .unwrap_or(1)
                .clamp(0, 10);
            let now = chrono::Utc::now().timestamp() as u64;
            let counter = (now / period) as i64;
            let mut valid = false;
            for skew in -window..=window {
                let c = counter + skew;
                if c < 0 {
                    continue;
                }
                let expected = hotp(&algorithm, &key, c as u64, digits)?;
                // Constant-time comparison (subtle is already in tree for the
                // webhook signature checks).
                use subtle::ConstantTimeEq;
                if expected.as_bytes().ct_eq(code.trim().as_bytes()).into() {
                    valid = true;
                }
            }
            Ok(wrap(config, input, "valid", Value::Bool(valid)))
        }

        "encrypt" => {
            let value = val_to_string(&config.get("value").cloned().unwrap_or(Value::Null));
            let secret = cfg_str(config, "secret")
                .ok_or_else(|| "Encryption needs a 'secret' passphrase".to_string())?;
            Ok(wrap(
                config,
                input,
                "encrypted",
                Value::String(aes_encrypt_value(&value, &secret)?),
            ))
        }

        "decrypt" => {
            let value = val_to_string(&config.get("value").cloned().unwrap_or(Value::Null));
            let secret = cfg_str(config, "secret")
                .ok_or_else(|| "Decryption needs a 'secret' passphrase".to_string())?;
            Ok(wrap(
                config,
                input,
                "decrypted",
                Value::String(aes_decrypt_value(&value, &secret)?),
            ))
        }

        other => Err(format!("Unknown Crypto operation: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // SHA-256("abc") — the canonical NIST vector, hex encoded.
    #[test]
    fn hash_sha256_hex() {
        let cfg = json!({ "operation": "hash", "value": "abc", "algorithm": "sha256" });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(
            out,
            json!({ "hash": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad" })
        );
    }

    // Hashing an empty/absent value is valid (SHA-256 of "").
    #[test]
    fn hash_empty_string() {
        let cfg = json!({ "operation": "hash", "algorithm": "sha256" });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(
            out["hash"],
            json!("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
    }

    // SHA-512("abc") — the NIST vector.
    #[test]
    fn hash_sha512_hex() {
        let cfg = json!({ "operation": "hash", "value": "abc", "algorithm": "sha512" });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(
            out["hash"],
            json!("ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f")
        );
    }

    // HMAC-SHA256(key="key", "The quick brown fox…") — the standard RFC vector.
    #[test]
    fn hmac_sha256_rfc_vector() {
        let cfg = json!({
            "operation": "hmac",
            "value": "The quick brown fox jumps over the lazy dog",
            "secret": "key",
            "algorithm": "sha256",
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(
            out["hmac"],
            json!("f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8")
        );
    }

    // base64 output decodes to the same bytes as the hex output.
    #[test]
    fn hash_base64_matches_hex() {
        let hexed = execute(
            &json!({ "operation": "hash", "value": "abc", "algorithm": "sha256", "encoding": "hex" }),
            &Value::Null,
        )
        .unwrap();
        let b64 = execute(
            &json!({ "operation": "hash", "value": "abc", "algorithm": "sha256", "encoding": "base64" }),
            &Value::Null,
        )
        .unwrap();
        let from_hex = hex::decode(hexed["hash"].as_str().unwrap()).unwrap();
        let from_b64 = STANDARD.decode(b64["hash"].as_str().unwrap()).unwrap();
        assert_eq!(from_hex, from_b64);
    }

    // base64url output carries no padding and none of +/ characters.
    #[test]
    fn base64url_encoding() {
        let out = execute(
            &json!({ "operation": "hash", "value": "abc", "algorithm": "sha256", "encoding": "base64url" }),
            &Value::Null,
        )
        .unwrap();
        let s = out["hash"].as_str().unwrap();
        assert!(
            !s.contains('=') && !s.contains('+') && !s.contains('/'),
            "got {s}"
        );
    }

    // Algorithm names normalize: "SHA-256" == "sha256".
    #[test]
    fn algorithm_name_normalizes() {
        let a = execute(
            &json!({ "operation": "hash", "value": "abc", "algorithm": "SHA-256" }),
            &Value::Null,
        )
        .unwrap();
        let b = execute(
            &json!({ "operation": "hash", "value": "abc", "algorithm": "sha256" }),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(a, b);
    }

    // An unsupported algorithm errors rather than silently defaulting.
    #[test]
    fn unsupported_algorithm_errors() {
        let cfg = json!({ "operation": "hash", "value": "abc", "algorithm": "md5" });
        assert!(execute(&cfg, &Value::Null).is_err());
    }

    // generateUuid yields a well-formed v4 UUID.
    #[test]
    fn generate_uuid_v4() {
        let out = execute(&json!({ "operation": "generateUuid" }), &Value::Null).unwrap();
        let id = out["uuid"].as_str().unwrap();
        assert_eq!(id.len(), 36);
        assert_eq!(id.matches('-').count(), 4);
        // The version nibble (15th hex char, position 14) is '4' for v4.
        assert_eq!(id.as_bytes()[14], b'4', "expected v4 UUID, got {id}");
        // Two calls differ.
        let out2 = execute(&json!({ "operation": "generateUuid" }), &Value::Null).unwrap();
        assert_ne!(out2["uuid"], out["uuid"]);
    }

    // A numeric value hashes via its canonical string form (flexidate-style type
    // preservation → val_to_string), not a quoted JSON number.
    #[test]
    fn numeric_value_hashes_as_plain_string() {
        let from_num = execute(
            &json!({ "operation": "hash", "value": 123, "algorithm": "sha256" }),
            &Value::Null,
        )
        .unwrap();
        let from_str = execute(
            &json!({ "operation": "hash", "value": "123", "algorithm": "sha256" }),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(from_num, from_str);
    }

    // includeInputFields merges the digest onto the incoming item.
    #[test]
    fn include_input_fields_merges() {
        let cfg = json!({
            "operation": "hash",
            "value": "abc",
            "algorithm": "sha256",
            "includeInputFields": true,
            "outputField": "sig",
        });
        let input = json!({ "keep": "me" });
        let out = execute(&cfg, &input).unwrap();
        assert_eq!(out["keep"], json!("me"));
        assert_eq!(
            out["sig"],
            json!("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    // An empty HMAC secret still produces a digest (valid, per HMAC).
    #[test]
    fn hmac_empty_secret_ok() {
        let cfg =
            json!({ "operation": "hmac", "value": "data", "secret": "", "algorithm": "sha256" });
        let out = execute(&cfg, &Value::Null).unwrap();
        assert_eq!(out["hmac"].as_str().unwrap().len(), 64); // 32 bytes → 64 hex
    }

    // ── JWT ───────────────────────────────────────────────────────────────

    // HS256 sign → verify roundtrip recovers the claims.
    #[test]
    fn jwt_sign_verify_roundtrip() {
        let signed = execute(
            &json!({
                "operation": "jwtSign",
                "payload": { "sub": "user-1", "role": "admin" },
                "secret": "s3cret",
                "expiresInSeconds": 3600,
            }),
            &Value::Null,
        )
        .unwrap();
        let token = signed["token"].as_str().unwrap();
        assert_eq!(token.matches('.').count(), 2);

        let verified = execute(
            &json!({ "operation": "jwtVerify", "token": token, "secret": "s3cret" }),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(verified["jwt"]["valid"], json!(true));
        assert_eq!(verified["jwt"]["payload"]["sub"], json!("user-1"));
        assert_eq!(verified["jwt"]["payload"]["role"], json!("admin"));
        assert!(verified["jwt"]["payload"]["exp"].is_i64());
        assert!(verified["jwt"]["payload"]["iat"].is_i64());
    }

    // A wrong secret soft-fails to {valid:false} — routable, not a crash.
    #[test]
    fn jwt_wrong_secret_soft_fails() {
        let signed = execute(
            &json!({ "operation": "jwtSign", "payload": { "a": 1 }, "secret": "right" }),
            &Value::Null,
        )
        .unwrap();
        let verified = execute(
            &json!({
                "operation": "jwtVerify",
                "token": signed["token"],
                "secret": "wrong",
            }),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(verified["jwt"]["valid"], json!(false));
        assert!(verified["jwt"]["error"].is_string());
    }

    // An expired token (exp in the past, beyond leeway) soft-fails.
    #[test]
    fn jwt_expired_soft_fails() {
        let past = chrono::Utc::now().timestamp() - 7200;
        let signed = execute(
            &json!({
                "operation": "jwtSign",
                "payload": { "exp": past },
                "secret": "k",
            }),
            &Value::Null,
        )
        .unwrap();
        let verified = execute(
            &json!({
                "operation": "jwtVerify",
                "token": signed["token"],
                "secret": "k",
                "leewaySeconds": 0,
            }),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(verified["jwt"]["valid"], json!(false));
    }

    // An explicit exp in the payload wins over expiresInSeconds.
    #[test]
    fn jwt_explicit_exp_wins() {
        let explicit = 4_000_000_000i64;
        let signed = execute(
            &json!({
                "operation": "jwtSign",
                "payload": { "exp": explicit },
                "secret": "k",
                "expiresInSeconds": 60,
            }),
            &Value::Null,
        )
        .unwrap();
        let verified = execute(
            &json!({ "operation": "jwtVerify", "token": signed["token"], "secret": "k" }),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(verified["jwt"]["payload"]["exp"], json!(explicit));
    }

    // ── TOTP ──────────────────────────────────────────────────────────────

    // RFC 6238 Appendix B test vector: secret "12345678901234567890" (SHA1),
    // T=59s → 8-digit code 94287082. hotp() is exercised directly at the
    // vector's fixed counter (59/30 = 1).
    #[test]
    fn totp_rfc6238_vector() {
        let key = b"12345678901234567890";
        assert_eq!(hotp("sha1", key, 1, 8).unwrap(), "94287082");
        // T=1111111109 → counter 37037036 → 07081804
        assert_eq!(hotp("sha1", key, 37037036, 8).unwrap(), "07081804");
    }

    // base32 decoding matches the plain secret bytes.
    #[test]
    fn totp_base32_matches_plain() {
        // "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ" is base32("12345678901234567890")
        assert_eq!(
            base32_decode("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ").unwrap(),
            b"12345678901234567890".to_vec()
        );
        // Case-insensitive and padding-tolerant.
        assert_eq!(base32_decode("gezdgnbvgy3tqojq====").unwrap(), b"1234567890".to_vec());
    }

    // Generate → verify roundtrip using the node operations end to end.
    #[test]
    fn totp_generate_verify_roundtrip() {
        let cfg = json!({
            "operation": "totp",
            "secret": "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ",
        });
        let out = execute(&cfg, &Value::Null).unwrap();
        let code = out["totp"]["code"].as_str().unwrap().to_string();
        assert_eq!(code.len(), 6);

        let verify = execute(
            &json!({
                "operation": "totpVerify",
                "secret": "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ",
                "code": code,
            }),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(verify["valid"], json!(true));

        let bad = execute(
            &json!({
                "operation": "totpVerify",
                "secret": "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ",
                "code": "000000",
            }),
            &Value::Null,
        )
        .unwrap();
        // (Astronomically unlikely to be the real code; tolerate the freak case.)
        if verify["valid"] == json!(true) && code != "000000" {
            assert_eq!(bad["valid"], json!(false));
        }
    }

    // ── AES-256-GCM ───────────────────────────────────────────────────────

    // encrypt → decrypt roundtrip; wrong passphrase is a hard, explicit error.
    #[test]
    fn encrypt_decrypt_roundtrip() {
        let enc = execute(
            &json!({ "operation": "encrypt", "value": "hello axon", "secret": "pass1" }),
            &Value::Null,
        )
        .unwrap();
        let blob = enc["encrypted"].as_str().unwrap();
        assert!(STANDARD.decode(blob).unwrap().len() > 12);

        let dec = execute(
            &json!({ "operation": "decrypt", "value": blob, "secret": "pass1" }),
            &Value::Null,
        )
        .unwrap();
        assert_eq!(dec["decrypted"], json!("hello axon"));

        let wrong = execute(
            &json!({ "operation": "decrypt", "value": blob, "secret": "pass2" }),
            &Value::Null,
        );
        assert!(wrong.is_err());
        assert!(wrong.unwrap_err().contains("wrong passphrase"));
    }

    // Two encryptions of the same value differ (fresh nonce per message).
    #[test]
    fn encrypt_uses_fresh_nonce() {
        let cfg = json!({ "operation": "encrypt", "value": "x", "secret": "p" });
        let a = execute(&cfg, &Value::Null).unwrap();
        let b = execute(&cfg, &Value::Null).unwrap();
        assert_ne!(a["encrypted"], b["encrypted"]);
    }
}
