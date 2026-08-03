use axum::Json;
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use tokio::process::Command;
use tracing::info;

use crate::routes::AppError;

/// Maximum output size per stream (stdout/stderr) to prevent memory exhaustion.
const MAX_OUTPUT_BYTES: usize = 10 * 1024 * 1024; // 10 MB

#[derive(Deserialize)]
pub struct ShellRequest {
    /// The command/script to run
    pub command: String,
    /// "powershell" (default) or "cmd"
    #[serde(default = "default_shell")]
    pub shell: String,
    /// Max seconds to wait (default 30)
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Working directory (optional)
    pub cwd: Option<String>,
}

fn default_shell() -> String {
    "powershell".to_string()
}
fn default_timeout() -> u64 {
    30
}

#[derive(Serialize)]
pub struct ShellResponse {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}

pub async fn run_command(Json(req): Json<ShellRequest>) -> Result<Json<ShellResponse>, AppError> {
    info!(shell = %req.shell, "Executing shell command");

    // PowerShell on Windows outputs UTF-16LE by default which corrupts when
    // read as UTF-8 bytes.  Prepend a one-liner that switches both the console
    // and the pipeline to UTF-8 before running the user's command.  This is
    // safe to prepend even when the user's command already sets encoding.
    let ps_command;
    let (prog, args): (&str, Vec<&str>) = match req.shell.as_str() {
        "cmd" => ("cmd.exe", vec!["/C", &req.command]),
        _ => {
            ps_command = format!(
                "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; \
                 $OutputEncoding = [System.Text.Encoding]::UTF8; \
                 {}",
                req.command
            );
            (
                "powershell.exe",
                vec!["-NoProfile", "-NonInteractive", "-Command", &ps_command],
            )
        }
    };

    let mut cmd = Command::new(prog);
    cmd.args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x08000000); // CREATE_NO_WINDOW

    if let Some(ref cwd) = req.cwd {
        cmd.current_dir(cwd);
    }

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(req.timeout_secs),
        cmd.output(),
    )
    .await
    .map_err(|_| {
        AppError::timeout(format!(
            "Command timed out after {} seconds",
            req.timeout_secs
        ))
    })?
    .map_err(|e| AppError::internal(e.to_string()))?;

    let exit_code = result.status.code().unwrap_or(-1);

    // Use from_utf8_lossy so invalid byte sequences (e.g. remaining legacy
    // code-page output from external tools called by PowerShell) are replaced
    // with the Unicode replacement character rather than causing an error.
    let stdout_raw = String::from_utf8_lossy(&result.stdout);
    let stderr_raw = String::from_utf8_lossy(&result.stderr);
    let truncated = stdout_raw.len() > MAX_OUTPUT_BYTES || stderr_raw.len() > MAX_OUTPUT_BYTES;

    Ok(Json(ShellResponse {
        stdout: truncate_output(&stdout_raw),
        stderr: truncate_output(&stderr_raw),
        success: result.status.success(),
        exit_code,
        truncated: if truncated { Some(true) } else { None },
    }))
}

fn truncate_output(s: &str) -> String {
    if s.len() > MAX_OUTPUT_BYTES {
        format!(
            "{}...\n[truncated: {} bytes total, showing first {}]",
            &s[..MAX_OUTPUT_BYTES],
            s.len(),
            MAX_OUTPUT_BYTES
        )
    } else {
        s.to_string()
    }
}
