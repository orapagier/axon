use serde_json::json;
use std::time::Duration;
use tokio::io::AsyncReadExt;

/// Hard cap on captured output, per stream. The reader tasks have no other
/// backpressure, so without this a command that prints without stopping (`yes`,
/// `cat /dev/urandom`) grows the buffer until the agent process is OOM-killed.
/// 1 MiB is far more than any output an LLM can usefully consume.
const MAX_STREAM_BYTES: usize = 1024 * 1024;

/// How long to keep collecting output after a timeout kill. Bounded on purpose:
/// see the loop in [`ShellTool::run_command`] for why waiting for EOF is not an
/// option.
const DRAIN_GRACE: Duration = Duration::from_millis(200);

/// Append up to the per-stream cap. Returns `true` if anything was dropped.
fn append_capped(buf: &mut Vec<u8>, data: &[u8]) -> bool {
    let room = MAX_STREAM_BYTES.saturating_sub(buf.len());
    if room == 0 {
        return true;
    }
    let take = room.min(data.len());
    buf.extend_from_slice(&data[..take]);
    take < data.len()
}

pub struct ShellTool;

impl ShellTool {
    pub async fn run_command(cmd: &str, timeout_seconds: u64) -> anyhow::Result<serde_json::Value> {
        // Prevent obvious destructive commands
        let blocked_patterns = [
            "rm -rf /",
            "rm -rf /*",
            "mkfs",
            "dd if=",
            "chmod -R",
            "chown -R",
            "iptables",
            "ufw",
            "passwd",
            "userdel",
            "groupdel",
        ];

        for pattern in blocked_patterns.iter() {
            if cmd.contains(pattern) {
                return Ok(json!({
                    "error": format!("Command execution blocked: '{}' matches restricted pattern '{}'. Destructive or permission-altering commands are prohibited.", cmd, pattern)
                }));
            }
        }

        let mut child = match tokio::process::Command::new("bash")
            .arg("-c")
            .arg(cmd)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return Ok(json!({"error": format!("Failed to spawn process: {}", e)})),
        };

        let mut stdout_stream = child.stdout.take().unwrap();
        let mut stderr_stream = child.stderr.take().unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let tx_out = tx.clone();
        tokio::spawn(async move {
            let mut buf = [0; 8192];
            while let Ok(n) = stdout_stream.read(&mut buf).await {
                // A send error means the collector has already returned. Stop
                // here: otherwise this task keeps reading a pipe nobody drains,
                // staying alive as long as whatever process holds the write end
                // — which, after a timeout kill, can be an orphaned grandchild.
                if n == 0 || tx_out.send(("out", buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        });

        let tx_err = tx.clone();
        tokio::spawn(async move {
            let mut buf = [0; 8192];
            while let Ok(n) = stderr_stream.read(&mut buf).await {
                if n == 0 || tx_err.send(("err", buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        });

        drop(tx);

        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();
        let mut truncated = false;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_seconds);
        // `Some(_)` once the deadline has passed and the child has been killed;
        // it holds the (short) window for collecting output already in flight.
        let mut drain_until: Option<tokio::time::Instant> = None;

        loop {
            // Every wait is bounded — first by the caller's timeout, then by the
            // drain grace period. Waiting for the readers to hit EOF instead
            // (the obvious "wait until the channel closes" shape) does not
            // terminate: killing `bash` does not close a stdout pipe that a
            // surviving grandchild still holds open, so `sleep 300 &` or any
            // backgrounded pipeline member keeps the command running long past
            // the timeout it was given.
            let until = drain_until.unwrap_or(deadline);
            match tokio::time::timeout_at(until, rx.recv()).await {
                Ok(Some(("out", data))) => truncated |= append_capped(&mut stdout_buf, &data),
                Ok(Some(("err", data))) => truncated |= append_capped(&mut stderr_buf, &data),
                Ok(Some(_)) => {}
                // Both readers reached EOF: the command finished on its own.
                Ok(None) => break,
                Err(_) if drain_until.is_none() => {
                    let _ = child.kill().await;
                    drain_until = Some(tokio::time::Instant::now() + DRAIN_GRACE);
                }
                // Grace window is over; stop regardless of what is still open.
                Err(_) => break,
            }
        }

        let mut timed_out = drain_until.is_some();

        let exit_code = if timed_out {
            -1
        } else {
            // Also bounded by the deadline. Reaching EOF on both pipes does not
            // prove the child exited — it can close its own stdout/stderr and
            // keep running — and an unbounded wait here would reintroduce
            // exactly the overrun the loop above exists to prevent.
            match tokio::time::timeout_at(deadline, child.wait()).await {
                Ok(Ok(status)) => status.code().unwrap_or(-1),
                Ok(Err(_)) => -1,
                Err(_) => {
                    let _ = child.kill().await;
                    timed_out = true;
                    -1
                }
            }
        };

        let stdout = String::from_utf8_lossy(&stdout_buf).to_string();
        let stderr = String::from_utf8_lossy(&stderr_buf).to_string();
        let note = if truncated {
            format!("\n\n[output truncated at {MAX_STREAM_BYTES} bytes per stream]")
        } else {
            String::new()
        };

        if timed_out {
            Ok(json!({
                "output": format!("Process timed out after {}s. Partial output captured:\n\nSTDOUT:\n{}\n\nSTDERR:\n{}{}", timeout_seconds, stdout.trim(), stderr.trim(), note),
                "timeout": true,
                "truncated": truncated,
                "exit_code": exit_code
            }))
        } else if truncated {
            Ok(json!({
                "stdout": stdout,
                "stderr": stderr,
                "truncated": true,
                "note": note.trim(),
                "exit_code": exit_code
            }))
        } else {
            Ok(json!({
                "stdout": stdout,
                "stderr": stderr,
                "exit_code": exit_code
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// The timeout must bound wall-clock time even when the command leaves a
    /// background process holding the inherited stdout pipe. Killing `bash`
    /// does not close that pipe, so a reader-driven loop would keep waiting on
    /// the grandchild long after the deadline.
    #[tokio::test]
    async fn timeout_is_honored_when_a_grandchild_holds_the_pipe() {
        let started = Instant::now();
        let out = ShellTool::run_command("sleep 5 & echo hi", 1).await.unwrap();
        let elapsed = started.elapsed();

        assert_eq!(out.get("timeout").and_then(|v| v.as_bool()), Some(true));
        assert!(
            elapsed < Duration::from_secs(3),
            "returned after {elapsed:?}; the 1s timeout was not enforced"
        );
    }

    #[tokio::test]
    async fn captures_output_and_exit_code() {
        let out = ShellTool::run_command("echo hello; echo oops >&2", 10)
            .await
            .unwrap();
        assert_eq!(out.get("exit_code").and_then(|v| v.as_i64()), Some(0));
        assert!(out["stdout"].as_str().unwrap().contains("hello"));
        assert!(out["stderr"].as_str().unwrap().contains("oops"));
        assert!(out.get("timeout").is_none(), "not a timeout");
    }

    #[tokio::test]
    async fn reports_nonzero_exit_code() {
        let out = ShellTool::run_command("exit 7", 10).await.unwrap();
        assert_eq!(out.get("exit_code").and_then(|v| v.as_i64()), Some(7));
    }

    /// A command that never stops printing must not be able to grow the buffer
    /// until the agent process is OOM-killed.
    #[tokio::test]
    async fn runaway_output_is_capped() {
        let out = ShellTool::run_command("yes axon", 2).await.unwrap();
        let captured = out["output"].as_str().unwrap();
        assert_eq!(out.get("truncated").and_then(|v| v.as_bool()), Some(true));
        assert!(
            captured.len() < MAX_STREAM_BYTES * 3,
            "captured {} bytes; the per-stream cap is {MAX_STREAM_BYTES}",
            captured.len()
        );
    }

    #[tokio::test]
    async fn destructive_commands_stay_blocked() {
        let out = ShellTool::run_command("rm -rf /", 10).await.unwrap();
        assert!(out["error"].as_str().unwrap().contains("blocked"));
    }
}
