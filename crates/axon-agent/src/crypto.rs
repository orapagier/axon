use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::env;

fn get_key() -> aes_gcm::Key<Aes256Gcm> {
    let master = env::var("AXON_MASTER_KEY").unwrap_or_else(|_| {
        tracing::warn!("AXON_MASTER_KEY not set, using insecure default key for development");
        "00000000000000000000000000000000".to_string()
    });
    // Truncate or pad to 32 bytes
    let mut key_bytes = [0u8; 32];
    let bytes = master.as_bytes();
    let len = std::cmp::min(bytes.len(), 32);
    key_bytes[..len].copy_from_slice(&bytes[..len]);
    key_bytes.into()
}

pub fn encrypt_key(plain: &str) -> String {
    if plain.is_empty() {
        return String::new();
    }
    let key = get_key();
    let cipher = Aes256Gcm::new(&key);
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng); // 96-bits; unique per message
    match cipher.encrypt(&nonce, plain.as_bytes()) {
        Ok(ciphertext) => {
            let mut combined = nonce.to_vec();
            combined.extend_from_slice(&ciphertext);
            STANDARD.encode(combined)
        }
        Err(e) => {
            tracing::error!("Encryption failed: {}", e);
            plain.to_string()
        }
    }
}

pub fn decrypt_key(encoded: &str) -> String {
    if encoded.is_empty() {
        return String::new();
    }
    let decoded = match STANDARD.decode(encoded) {
        Ok(d) => d,
        Err(_) => return encoded.to_string(), // Might be an old plain text key
    };
    if decoded.len() < 12 {
        return encoded.to_string();
    }
    let key = get_key();
    let cipher = Aes256Gcm::new(&key);
    let nonce = Nonce::from_slice(&decoded[..12]);
    let ciphertext = &decoded[12..];
    match cipher.decrypt(nonce, ciphertext) {
        Ok(plaintext) => String::from_utf8(plaintext).unwrap_or_else(|_| encoded.to_string()),
        Err(_) => {
            tracing::warn!(
                "Credential decryption failed; AXON_MASTER_KEY may be missing or different from the key used to encrypt stored secrets"
            );
            encoded.to_string()
        }
    }
}
