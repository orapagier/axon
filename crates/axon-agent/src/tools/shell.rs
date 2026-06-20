use serde_json::json;
use tokio::io::AsyncReadExt;

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
            let mut buf = [0; 1024];
            while let Ok(n) = stdout_stream.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let _ = tx_out.send(("out", buf[..n].to_vec()));
            }
        });

        let tx_err = tx.clone();
        tokio::spawn(async move {
            let mut buf = [0; 1024];
            while let Ok(n) = stderr_stream.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let _ = tx_err.send(("err", buf[..n].to_vec()));
            }
        });

        drop(tx);

        let timeout = tokio::time::sleep(std::time::Duration::from_secs(timeout_seconds));
        tokio::pin!(timeout);

        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();
        let mut timed_out = false;

        loop {
            tokio::select! {
                _ = &mut timeout, if !timed_out => {
                    timed_out = true;
                    let _ = child.kill().await;
                }
                msg = rx.recv() => {
                    match msg {
                        Some(("out", data)) => stdout_buf.extend_from_slice(&data),
                        Some(("err", data)) => stderr_buf.extend_from_slice(&data),
                        _ => break,
                    }
                }
            }
        }

        let exit_code = if timed_out {
            -1
        } else {
            match child.wait().await {
                Ok(status) => status.code().unwrap_or(-1),
                Err(_) => -1,
            }
        };

        let stdout = String::from_utf8_lossy(&stdout_buf).to_string();
        let stderr = String::from_utf8_lossy(&stderr_buf).to_string();

        if timed_out {
            Ok(json!({
                "output": format!("Process timed out after {}s. Partial output captured:\n\nSTDOUT:\n{}\n\nSTDERR:\n{}", timeout_seconds, stdout.trim(), stderr.trim()),
                "timeout": true,
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
