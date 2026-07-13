//! SSH workflow action node (n8n gap-closure). Runs a remote command, or
//! uploads / downloads a file over SFTP-style `cat` transfers, against a
//! server saved in the dashboard's `ssh_servers` list.
//!
//! It delegates to the agent's existing, tested [`SshTool`] — so it inherits
//! known-hosts fingerprint pinning (trust-on-first-connect), password AND
//! private-key auth, the destructive-command guardrails, and encrypted
//! credential storage — instead of re-implementing an SSH client. That reuse
//! is the whole point: one connection stack, exercised by both the agent and
//! workflows.
//!
//! Config:
//!   - `server`    — a saved server name (required; add servers in Settings).
//!   - `operation` — `execute` (default) | `upload` | `download`.
//!   - `command`   — shell command, for `execute`.
//!   - `remotePath`— remote file path, for `upload` / `download`.
//!   - `localPath` — source path for `upload`; resolved from the primary
//!                   input's binary descriptor (`local_path` / `localPath` /
//!                   `_axon_file_path`) when omitted, so a Myelin/HTTP/SSH
//!                   download upstream feeds straight in.
//!   - `timeout`   — seconds for `execute` (default 30).
//!
//! `download` emits the standard binary descriptor (`binary.local_path`, both
//! key conventions) so the file feeds Telegram send / Gmail attach / Myelin.

use crate::state::AppState;
use crate::tools::ssh::SshTool;
use crate::tools::telegram::binary_descriptor;
use serde_json::{json, Value};

/// Resolve a source path for upload: explicit `localPath`, else the input's
/// binary descriptor (any of the accepted key spellings). Shared with the FTP
/// node, which uploads from the same descriptor convention.
pub(crate) fn resolve_local_path(config: &Value, input: &Value) -> Option<String> {
    let direct = config
        .get("localPath")
        .or_else(|| config.get("local_path"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(p) = direct {
        return Some(p.to_string());
    }
    // From an upstream binary descriptor (input.binary.* or input.* directly).
    let bin = input.get("binary").unwrap_or(input);
    for key in ["local_path", "localPath", "_axon_file_path", "path"] {
        if let Some(p) = bin
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Some(p.to_string());
        }
    }
    None
}

pub(crate) async fn execute(
    config: &Value,
    state: &AppState,
    input: &Value,
) -> Result<Value, String> {
    let server = config
        .get("server")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "SSH node needs a 'server' — the name of a server saved in Settings › SSH".to_string()
        })?
        .to_string();
    let operation = config
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("execute");

    match operation {
        "execute" => {
            let command = config
                .get("command")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "SSH execute needs a 'command'".to_string())?;
            let timeout = config
                .get("timeout")
                .and_then(|v| v.as_u64())
                .filter(|t| *t > 0)
                .unwrap_or(30);
            SshTool::run_command(&server, command, timeout, state.clone())
                .await
                .map_err(|e| format!("SSH execute failed: {e}"))
        }

        "upload" => {
            let remote_path = config
                .get("remotePath")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "SSH upload needs a 'remotePath'".to_string())?;
            let local_path = resolve_local_path(config, input).ok_or_else(|| {
                "SSH upload needs a 'localPath' (or a binary file from the previous node)"
                    .to_string()
            })?;
            SshTool::upload_file(&server, remote_path, &local_path, state.clone())
                .await
                .map_err(|e| format!("SSH upload failed: {e}"))
        }

        "download" => {
            let remote_path = config
                .get("remotePath")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "SSH download needs a 'remotePath'".to_string())?;
            let result = SshTool::download_file(&server, remote_path, state.clone())
                .await
                .map_err(|e| format!("SSH download failed: {e}"))?;
            // Reshape SshTool's structured fields into the workflow binary
            // descriptor. If the transfer failed, download_file returns only an
            // `output` string (no local_path) — pass that straight through.
            match result.get("local_path").and_then(|v| v.as_str()) {
                Some(path) => {
                    let file_name = result
                        .get("file_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("download");
                    let size = result.get("size").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let mime = mime_guess::from_path(file_name)
                        .first_or_octet_stream()
                        .to_string();
                    Ok(json!({
                        "binary": binary_descriptor(path, file_name, &mime, size),
                        "remote_path": remote_path,
                        "server": server,
                    }))
                }
                None => Ok(result),
            }
        }

        other => Err(format!(
            "Unknown SSH operation '{other}' (expected execute, upload, or download)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // localPath resolution prefers explicit config…
    #[test]
    fn resolves_explicit_local_path() {
        let cfg = json!({ "localPath": "/tmp/a.txt" });
        assert_eq!(
            resolve_local_path(&cfg, &json!({})),
            Some("/tmp/a.txt".to_string())
        );
    }

    // …then falls back to the input's binary descriptor.
    #[test]
    fn resolves_from_binary_descriptor() {
        let input = json!({ "binary": { "local_path": "/staged/file.pdf" } });
        assert_eq!(
            resolve_local_path(&json!({}), &input),
            Some("/staged/file.pdf".to_string())
        );
    }

    // …and a top-level path field on the item.
    #[test]
    fn resolves_from_top_level_path() {
        let input = json!({ "_axon_file_path": "/staged/x.bin" });
        assert_eq!(
            resolve_local_path(&json!({}), &input),
            Some("/staged/x.bin".to_string())
        );
    }

    // Nothing to resolve → None (caller turns this into a clear error).
    #[test]
    fn no_path_returns_none() {
        assert_eq!(resolve_local_path(&json!({}), &json!({ "other": 1 })), None);
    }
}
