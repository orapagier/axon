//! Crypto — Task 2.2 (*Enzyme*). Hash, HMAC, and UUID with zero new
//! dependencies — `sha2`, `hmac`, `hex`, `base64`, and `uuid` are all already in
//! the tree (they back the master-key crypto and the GitHub/Facebook webhook
//! signature checks). The point is **webhook signature verification** and
//! **idempotency keys** without dropping to a JavaScript node.
//!
//! Three operations, one per `operation` config key:
//!   - `hash`         — digest a value (SHA-224/256/384/512).
//!   - `hmac`         — keyed HMAC of a value with a secret (this is the "sign"
//!                      side of webhook verification: compute the HMAC, then
//!                      compare it to the provider's header with an IF node).
//!   - `generateUuid` — a fresh v4 UUID.
//!
//! Digest output encodes as `hex` (default — what GitHub/Stripe/Shopify-hex use),
//! `base64`, or `base64url`. Asymmetric-key signing (RSA/ECDSA) is deliberately
//! out of scope: it needs a new crate, and the plan pins this node to zero deps.
//!
//! Output mirrors Soma/`dateTime`: the result lands under `outputField` and
//! `includeInputFields` decides whether the incoming item's other fields ride
//! along.

use crate::tools::workflow::val_to_string;
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use serde_json::{Map, Value};

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
}
