use axum::Json;
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use tokio::process::Command;
use tracing::info;

use crate::routes::AppError;

/// Maximum output size per stream (stdout/stderr) returned to the caller.
///
/// NOTE: this is a response-size cap, not a memory guard — `Command::output()`
/// has already buffered the full stream by the time we truncate. It exists so a
/// runaway command does not push a multi-hundred-megabyte JSON body through the
/// tunnel.
const MAX_OUTPUT_BYTES: usize = 10 * 1024 * 1024;

/// Upper bound on `timeout_secs` so a single request cannot pin a child process
/// (and its pipes) indefinitely.
const MAX_TIMEOUT_SECS: u64 = 3600;

#[derive(Deserialize)]
pub struct ShellRequest {
    /// The command/script to run
    pub command: String,
    /// "powershell" (default) or "cmd"
    #[serde(default = "default_shell")]
    pub shell: String,
    /// Max seconds to wait (default 30, max 3600)
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Working directory (optional)
    pub cwd: Option<String>,
    /// Which plane runs the command — see `dispatch`.
    #[serde(default = "default_run_as")]
    pub run_as: String,
}

fn default_shell() -> String {
    "powershell".to_string()
}
fn default_timeout() -> u64 {
    30
}
fn default_run_as() -> String {
    "system".to_string()
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

/// Routes a shell request to the plane that can satisfy it.
///
/// - `run_as: "system"` (default) — runs here, in the service, as LocalSystem.
///   Full machine privilege and available at the lock screen, but it cannot see
///   HKCU, mapped drives, or anything DPAPI-protected for the user, and its
///   network identity is the machine account rather than the user.
/// - `run_as: "user"` — forwarded to the desktop agent, so the command runs as
///   the logged-on user with their real profile and environment. Requires an
///   interactive session; answers 503 when nobody is logged on.
///
/// Getting this wrong is the most likely source of "it works in my terminal but
/// not over the API" confusion, so an unknown value is rejected rather than
/// silently defaulted.
pub async fn dispatch(
    axum::extract::State(state): axum::extract::State<crate::server::AppState>,
    req: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let (parts, body) = req.into_parts();

    let bytes = match axum::body::to_bytes(body, 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return AppError::bad_request(format!("Could not read request body: {e}"))
                .into_response()
        }
    };

    let parsed = match serde_json::from_slice::<ShellRequest>(&bytes) {
        Ok(p) => p,
        Err(e) => {
            return AppError::bad_request(format!("Invalid /shell body: {e}")).into_response()
        }
    };

    match parsed.run_as.as_str() {
        // Forward the original bytes rather than re-serialising, so the desktop
        // agent sees exactly what the caller sent.
        "user" => {
            let rebuilt = axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes));
            crate::proxy::forward(axum::extract::State(state), rebuilt).await
        }
        "system" => run_command(Json(parsed)).await.into_response(),
        other => AppError::bad_request(format!(
            "Unknown run_as {:?}. Use \"system\" (LocalSystem, works at the lock screen) \
             or \"user\" (the logged-on user, needs an interactive session).",
            other
        ))
        .into_response(),
    }
}

pub async fn run_command(Json(req): Json<ShellRequest>) -> Result<Json<ShellResponse>, AppError> {
    info!(shell = %req.shell, "Executing shell command");

    let timeout_secs = req.timeout_secs.clamp(1, MAX_TIMEOUT_SECS);

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
        // Without this, dropping the child on timeout leaves the process running
        // forever, detached, with its pipes leaked. Tokio does NOT kill on drop
        // by default.
        .kill_on_drop(true)
        .creation_flags(0x08000000); // CREATE_NO_WINDOW

    if let Some(ref cwd) = req.cwd {
        cmd.current_dir(cwd);
    }

    let child = cmd.spawn().map_err(|e| AppError::internal(e.to_string()))?;

    let result = match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        child.wait_with_output(),
    )
    .await
    {
        Ok(r) => r.map_err(|e| AppError::internal(e.to_string()))?,
        Err(_) => {
            // `wait_with_output` consumed `child`, so the timeout dropped it and
            // kill_on_drop has already signalled the process. Report the timeout
            // rather than leaving the caller waiting on a command that is gone.
            return Err(AppError::timeout(format!(
                "Command timed out after {} seconds and was terminated",
                timeout_secs
            )));
        }
    };

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
    truncate_at(s, MAX_OUTPUT_BYTES)
}

fn truncate_at(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    // Slicing at a fixed byte offset panics if it lands inside a multi-byte
    // character, so walk back to the nearest character boundary first.
    let mut end = limit;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}...\n[truncated: {} bytes total, showing first {}]",
        &s[..end],
        s.len(),
        end
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_is_a_noop_below_the_limit() {
        assert_eq!(truncate_output("hello"), "hello");
    }

    fn parse(json: &str) -> ShellRequest {
        serde_json::from_str(json).expect("should parse")
    }

    #[test]
    fn run_as_defaults_to_system() {
        // Every pre-split caller omits run_as, and must keep working — landing
        // on the service plane, which is the one that survives a lock screen.
        assert_eq!(parse(r#"{"command":"Get-Date"}"#).run_as, "system");
    }

    #[test]
    fn run_as_user_is_parsed() {
        assert_eq!(
            parse(r#"{"command":"whoami","run_as":"user"}"#).run_as,
            "user"
        );
    }

    #[test]
    fn unknown_run_as_survives_parsing_and_is_rejected_later() {
        // dispatch() rejects this with a 400 rather than silently defaulting,
        // so an unrecognised value must not fail deserialisation first.
        assert_eq!(parse(r#"{"command":"x","run_as":"root"}"#).run_as, "root");
    }

    #[test]
    fn truncate_does_not_split_multibyte_characters() {
        // '€' is 3 bytes wide, so for two of every three limits the cut lands
        // inside a character. Naive byte slicing panicked on exactly this.
        let s = "€".repeat(64);
        for limit in 1..(s.len()) {
            let out = truncate_at(&s, limit);
            assert!(out.starts_with('€') || limit < 3, "limit {}", limit);
            assert!(out.contains("[truncated:"), "limit {}", limit);
        }
    }

    #[test]
    fn truncate_handles_lossy_replacement_characters() {
        // U+FFFD is also 3 bytes, and lossy decoding of binary output emits a
        // solid run of them — the realistic trigger for the panic.
        let lossy = String::from_utf8_lossy(&[0xFF_u8; 300]).to_string();
        let out = truncate_at(&lossy, 100);
        assert!(out.contains("[truncated:"));
    }
}
