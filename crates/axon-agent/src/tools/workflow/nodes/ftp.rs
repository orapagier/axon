//! FTP / FTPS workflow action node (n8n gap-closure: n8n ships FTP as a core
//! node; SFTP is covered by the SSH node here). Lists, transfers, and manages
//! files on a classic FTP server, with optional explicit FTPS (AUTH TLS) on
//! the workspace's single rustls+ring stack.
//!
//! Connection details arrive inline in `config`, or via a saved credential
//! (service "ftp") that `interpolate_config` merges in — the same path the
//! Database node uses. Fields: `host` (required), `port` (21), `user`
//! (defaults to anonymous), `password`, `secure` (FTPS via AUTH TLS),
//! `allowInvalidCerts` (accept self-signed FTPS certs — common on FTP boxes;
//! signatures are still checked, only the chain is skipped).
//!
//! Operations (`operation` config key):
//!   - `list`     — parsed directory listing of `remotePath` (or the login
//!                  dir). Emits an ARRAY of `{name, type, size, modified}`
//!                  items, ready for Loop/for_each fan-out.
//!   - `download` — RETR `remotePath`; stages the bytes and emits the standard
//!                  binary descriptor (`binary.local_path`, both key
//!                  spellings) so it feeds Telegram send / Gmail attach /
//!                  Myelin, exactly like SSH download.
//!   - `upload`   — STOR to `remotePath` from `localPath`, or from the primary
//!                  input's binary descriptor when omitted (Myelin/HTTP/SSH
//!                  download upstream feeds straight in).
//!   - `delete`   — DELE a file, or RMD when `directory` is true.
//!   - `rename`   — RNFR/RNTO `remotePath` → `newPath` (doubles as move).
//!   - `mkdir`    — MKD `remotePath`.
//!
//! Transfers are always binary (TYPE I). Passive mode with the NAT workaround
//! on by default (`natWorkaround: false` to trust PASV addresses verbatim) —
//! the single most common FTP failure is a server advertising its private IP.
//! The whole operation runs under a `timeout` (seconds, default 120).

use crate::tools::telegram::binary_descriptor;
use crate::tools::workflow::nodes::ssh::resolve_local_path;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use suppaftp::tokio::{AsyncRustlsConnector, AsyncRustlsFtpStream};
use suppaftp::types::FileType;
use tokio_rustls::rustls;

/// Config string accessor: trimmed, None when absent/blank.
fn cfg_str(config: &Value, key: &str) -> Option<String> {
    config
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn cfg_bool(config: &Value, key: &str, default: bool) -> bool {
    config.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

/// Connection parameters, resolved from config (post-credential-merge).
#[derive(Debug)]
struct ConnSpec {
    host: String,
    port: u16,
    user: String,
    password: String,
    secure: bool,
    allow_invalid_certs: bool,
    nat_workaround: bool,
}

fn conn_spec(config: &Value) -> Result<ConnSpec, String> {
    let host = cfg_str(config, "host")
        .ok_or_else(|| "FTP node needs a 'host' (inline or from a saved credential)".to_string())?;
    let port = config
        .get("port")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        })
        .filter(|p| (1..=65535).contains(p))
        .unwrap_or(21) as u16;
    // RFC 1635 anonymous convention: user "anonymous", an email as password.
    let user = cfg_str(config, "user")
        .or_else(|| cfg_str(config, "username"))
        .unwrap_or_else(|| "anonymous".to_string());
    let password = cfg_str(config, "password").unwrap_or_else(|| {
        if user == "anonymous" {
            "anonymous@axon".to_string()
        } else {
            String::new()
        }
    });
    Ok(ConnSpec {
        host,
        port,
        user,
        password,
        secure: cfg_bool(config, "secure", false),
        allow_invalid_certs: cfg_bool(config, "allowInvalidCerts", false),
        nat_workaround: cfg_bool(config, "natWorkaround", true),
    })
}

// ── FTPS trust ────────────────────────────────────────────────────────────────

/// Chain-skipping verifier for `allowInvalidCerts`: the handshake signature is
/// still verified against the presented cert (via the ring provider), only the
/// path to a webpki root is skipped — the standard "self-signed OK" posture,
/// mirroring reqwest's `danger_accept_invalid_certs` used by the HTTP tool.
#[derive(Debug)]
struct AcceptAnyCert(rustls::crypto::CryptoProvider);

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn tls_connector(allow_invalid_certs: bool) -> AsyncRustlsConnector {
    let builder = rustls::ClientConfig::builder();
    let config = if allow_invalid_certs {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(
                rustls::crypto::ring::default_provider(),
            )))
            .with_no_client_auth()
    } else {
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        builder.with_root_certificates(roots).with_no_client_auth()
    };
    AsyncRustlsConnector::from(tokio_rustls::TlsConnector::from(Arc::new(config)))
}

// ── Session ───────────────────────────────────────────────────────────────────

async fn connect(spec: &ConnSpec) -> Result<AsyncRustlsFtpStream, String> {
    let addr = format!("{}:{}", spec.host, spec.port);
    let mut ftp = AsyncRustlsFtpStream::connect(&addr)
        .await
        .map_err(|e| format!("FTP connect to {addr} failed: {e}"))?;
    if spec.secure {
        ftp = ftp
            .into_secure(tls_connector(spec.allow_invalid_certs), &spec.host)
            .await
            .map_err(|e| format!("FTPS (AUTH TLS) handshake with {} failed: {e}", spec.host))?;
    }
    ftp.set_passive_nat_workaround(spec.nat_workaround);
    ftp.login(&spec.user, &spec.password)
        .await
        .map_err(|e| format!("FTP login as '{}' failed: {e}", spec.user))?;
    ftp.transfer_type(FileType::Binary)
        .await
        .map_err(|e| format!("FTP TYPE I failed: {e}"))?;
    Ok(ftp)
}

/// One LIST line → a workflow item, or None for non-entries. Servers emit
/// POSIX, DOS, or MLSD formats — suppaftp handles all three; the POSIX
/// `total N` header and blank lines are dropped (they aren't files), and an
/// unparseable line degrades to `{name: <raw>, raw: true}` instead of failing
/// the listing.
fn list_entry(line: &str) -> Option<Value> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("total ") {
        if rest.trim().parse::<u64>().is_ok() {
            return None;
        }
    }
    Some(match suppaftp::list::File::try_from(line) {
        Ok(f) => {
            let kind = if f.is_directory() {
                "directory"
            } else if f.is_symlink() {
                "symlink"
            } else {
                "file"
            };
            json!({
                "name": f.name(),
                "type": kind,
                "size": f.size(),
                "modified": chrono::DateTime::<chrono::Utc>::from(f.modified()).to_rfc3339(),
            })
        }
        Err(_) => json!({ "name": trimmed, "raw": true }),
    })
}

async fn run_operation(operation: &str, config: &Value, input: &Value) -> Result<Value, String> {
    let spec = conn_spec(config)?;
    let mut ftp = connect(&spec).await?;

    let result = match operation {
        "list" => {
            let path = cfg_str(config, "remotePath");
            let lines = ftp
                .list(path.as_deref())
                .await
                .map_err(|e| format!("FTP list failed: {e}"))?;
            Ok(Value::Array(
                lines.iter().filter_map(|l| list_entry(l)).collect(),
            ))
        }

        "download" => {
            let remote = cfg_str(config, "remotePath")
                .ok_or_else(|| "FTP download needs a 'remotePath'".to_string())?;
            let mut stream = ftp
                .retr_as_stream(&remote)
                .await
                .map_err(|e| format!("FTP download of {remote} failed: {e}"))?;
            let mut data = Vec::new();
            tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut data)
                .await
                .map_err(|e| format!("FTP download of {remote} failed mid-transfer: {e}"))?;
            ftp.finalize_retr_stream(stream)
                .await
                .map_err(|e| format!("FTP download of {remote} did not complete cleanly: {e}"))?;
            let file_name = remote
                .rsplit('/')
                .find(|s| !s.is_empty())
                .unwrap_or("download");
            let staged = crate::files::stage_bytes(&data, file_name)
                .map_err(|e| format!("Failed to stage downloaded file: {e}"))?;
            let mime = mime_guess::from_path(file_name)
                .first_or_octet_stream()
                .to_string();
            Ok(json!({
                "binary": binary_descriptor(
                    &staged.display().to_string(), file_name, &mime, data.len(),
                ),
                "remote_path": remote,
                "size": data.len(),
            }))
        }

        "upload" => {
            let remote = cfg_str(config, "remotePath")
                .ok_or_else(|| "FTP upload needs a 'remotePath'".to_string())?;
            let local = resolve_local_path(config, input).ok_or_else(|| {
                "FTP upload needs a 'localPath' (or a binary file from the previous node)"
                    .to_string()
            })?;
            let data = tokio::fs::read(&local)
                .await
                .map_err(|e| format!("Failed to read local file {local}: {e}"))?;
            let written = ftp
                .put_file(&remote, &mut data.as_slice())
                .await
                .map_err(|e| format!("FTP upload to {remote} failed: {e}"))?;
            Ok(json!({
                "output": format!("Uploaded {written} bytes to {remote}"),
                "remote_path": remote,
                "size": written,
            }))
        }

        "delete" => {
            let remote = cfg_str(config, "remotePath")
                .ok_or_else(|| "FTP delete needs a 'remotePath'".to_string())?;
            if cfg_bool(config, "directory", false) {
                ftp.rmdir(&remote)
                    .await
                    .map_err(|e| format!("FTP rmdir of {remote} failed: {e}"))?;
            } else {
                ftp.rm(&remote)
                    .await
                    .map_err(|e| format!("FTP delete of {remote} failed: {e}"))?;
            }
            Ok(json!({ "output": format!("Deleted {remote}"), "deleted": remote }))
        }

        "rename" => {
            let from = cfg_str(config, "remotePath")
                .ok_or_else(|| "FTP rename needs a 'remotePath'".to_string())?;
            let to = cfg_str(config, "newPath")
                .ok_or_else(|| "FTP rename needs a 'newPath'".to_string())?;
            ftp.rename(&from, &to)
                .await
                .map_err(|e| format!("FTP rename {from} → {to} failed: {e}"))?;
            Ok(json!({ "output": format!("Renamed {from} to {to}"), "from": from, "to": to }))
        }

        "mkdir" => {
            let remote = cfg_str(config, "remotePath")
                .ok_or_else(|| "FTP mkdir needs a 'remotePath'".to_string())?;
            ftp.mkdir(&remote)
                .await
                .map_err(|e| format!("FTP mkdir of {remote} failed: {e}"))?;
            Ok(json!({ "output": format!("Created directory {remote}"), "created": remote }))
        }

        other => Err(format!(
            "Unknown FTP operation '{other}' (expected list, download, upload, delete, rename, or mkdir)"
        )),
    };

    // Best-effort polite close; the op result stands regardless.
    let _ = ftp.quit().await;
    result
}

pub(crate) async fn execute(config: &Value, input: &Value) -> Result<Value, String> {
    let operation = config
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("list")
        .to_string();
    // Reject unknown operations before dialing the server — the match arm
    // below only exists as an unreachable fallback once past this gate.
    if !["list", "download", "upload", "delete", "rename", "mkdir"].contains(&operation.as_str()) {
        return Err(format!(
            "Unknown FTP operation '{operation}' (expected list, download, upload, delete, rename, or mkdir)"
        ));
    }
    let timeout_secs = config
        .get("timeout")
        .and_then(|v| v.as_u64())
        .filter(|t| *t > 0)
        .unwrap_or(120);
    tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        run_operation(&operation, config, input),
    )
    .await
    .map_err(|_| format!("FTP {operation} timed out after {timeout_secs}s"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    // Missing host is a config error naming the field.
    #[test]
    fn missing_host_errors() {
        let err = conn_spec(&json!({})).unwrap_err();
        assert!(err.contains("host"), "got: {err}");
    }

    // Defaults: port 21, anonymous user with the RFC 1635 email-ish password,
    // plain FTP, NAT workaround on.
    #[test]
    fn defaults_apply() {
        let spec = conn_spec(&json!({ "host": "ftp.example.com" })).unwrap();
        assert_eq!(spec.port, 21);
        assert_eq!(spec.user, "anonymous");
        assert_eq!(spec.password, "anonymous@axon");
        assert!(!spec.secure);
        assert!(!spec.allow_invalid_certs);
        assert!(spec.nat_workaround);
    }

    // Explicit values (port arriving as a string — expression results — too).
    #[test]
    fn explicit_values_and_string_port() {
        let spec = conn_spec(&json!({
            "host": "h", "port": "2121", "user": "bob", "password": "pw",
            "secure": true, "allowInvalidCerts": true, "natWorkaround": false,
        }))
        .unwrap();
        assert_eq!(spec.port, 2121);
        assert_eq!(spec.user, "bob");
        assert_eq!(spec.password, "pw");
        assert!(spec.secure && spec.allow_invalid_certs && !spec.nat_workaround);
    }

    // A named user with no password gets an empty one, not the anonymous email.
    #[test]
    fn named_user_empty_password() {
        let spec = conn_spec(&json!({ "host": "h", "user": "bob" })).unwrap();
        assert_eq!(spec.password, "");
    }

    // POSIX LIST lines parse into structured entries.
    #[test]
    fn list_entry_parses_posix() {
        let e = list_entry("-rw-r--r-- 1 user group 1234 Jan 05 10:00 report.pdf").unwrap();
        assert_eq!(e["name"], json!("report.pdf"));
        assert_eq!(e["type"], json!("file"));
        assert_eq!(e["size"], json!(1234));
        assert!(e["modified"].is_string());

        let d = list_entry("drwxr-xr-x 2 user group 4096 Jan 05 10:00 invoices").unwrap();
        assert_eq!(d["type"], json!("directory"));
    }

    // The POSIX `total N` header and blank lines are not files — dropped.
    #[test]
    fn list_entry_drops_headers_and_blanks() {
        assert_eq!(list_entry("total 42"), None);
        assert_eq!(list_entry("   "), None);
    }

    // A line every parser rejects (bad MLSD fact) degrades to a raw entry
    // instead of failing the whole listing.
    #[test]
    fn list_entry_keeps_raw_line() {
        let e = list_entry("size=not-a-number; broken.bin").unwrap();
        assert_eq!(e["raw"], json!(true));
        assert_eq!(e["name"], json!("size=not-a-number; broken.bin"));
    }

    // Unknown operations error with the accepted list.
    #[tokio::test]
    async fn unknown_operation_errors() {
        let err = execute(&json!({ "host": "h", "operation": "chmod" }), &Value::Null)
            .await
            .unwrap_err();
        assert!(err.contains("Unknown FTP operation"), "got: {err}");
    }

    // Full live roundtrip: upload → list → download → rename → mkdir → delete.
    // Run explicitly with `cargo test -p axon --lib nodes::ftp -- --ignored`
    // after starting a local server:
    //   python3 -m pyftpdlib -p 2121 -w -d <dir>
    // Set FTP_LIVE_SECURE=1 to drive the FTPS (AUTH TLS + self-signed cert)
    // path instead, against a pyftpdlib TLS_FTPHandler server.
    #[tokio::test]
    #[ignore = "needs a writable anonymous FTP server on 127.0.0.1:2121"]
    async fn live_roundtrip() {
        let secure = std::env::var("FTP_LIVE_SECURE").as_deref() == Ok("1");
        let base = json!({
            "host": "127.0.0.1", "port": 2121,
            "secure": secure, "allowInvalidCerts": secure,
        });
        let cfg = |extra: Value| {
            let mut m = base.clone();
            m.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            m
        };

        // Upload a known payload from a temp file.
        let payload = b"axon ftp live roundtrip \xf0\x9f\x93\x81";
        let local = std::env::temp_dir().join("axon_ftp_live.bin");
        std::fs::write(&local, payload).unwrap();
        let up = execute(
            &cfg(json!({
                "operation": "upload", "remotePath": "up.bin",
                "localPath": local.display().to_string(),
            })),
            &Value::Null,
        )
        .await
        .unwrap();
        assert_eq!(up["size"], json!(payload.len()));

        // List shows it with the right size.
        let list = execute(&cfg(json!({ "operation": "list" })), &Value::Null)
            .await
            .unwrap();
        let entry = list
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == json!("up.bin"))
            .expect("uploaded file missing from listing");
        assert_eq!(entry["size"], json!(payload.len()));
        assert_eq!(entry["type"], json!("file"));

        // Download roundtrips the exact bytes via the binary descriptor.
        let down = execute(
            &cfg(json!({ "operation": "download", "remotePath": "up.bin" })),
            &Value::Null,
        )
        .await
        .unwrap();
        let staged = down["binary"]["local_path"].as_str().unwrap();
        assert_eq!(std::fs::read(staged).unwrap(), payload);

        // Rename, mkdir, then clean both up.
        execute(
            &cfg(
                json!({ "operation": "rename", "remotePath": "up.bin", "newPath": "renamed.bin" }),
            ),
            &Value::Null,
        )
        .await
        .unwrap();
        execute(
            &cfg(json!({ "operation": "mkdir", "remotePath": "livedir" })),
            &Value::Null,
        )
        .await
        .unwrap();
        execute(
            &cfg(json!({ "operation": "delete", "remotePath": "renamed.bin" })),
            &Value::Null,
        )
        .await
        .unwrap();
        execute(
            &cfg(json!({ "operation": "delete", "remotePath": "livedir", "directory": true })),
            &Value::Null,
        )
        .await
        .unwrap();
        let after = execute(&cfg(json!({ "operation": "list" })), &Value::Null)
            .await
            .unwrap();
        assert!(after
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["name"] != json!("renamed.bin") && e["name"] != json!("livedir")));
    }
}
