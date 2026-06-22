use serde_json::{json, Value};
use std::time::Duration;

pub(crate) async fn execute(config: &Value) -> Result<Value, String> {
    let cmd = config.get("command").and_then(|v| v.as_str()).unwrap_or("");
    if cmd.trim().is_empty() {
        return Err("Shell node: command is empty".to_string());
    }
    let (shell, arg) = if cfg!(target_os = "windows") {
        ("cmd", "/C")
    } else {
        ("sh", "-c")
    };
    let fut = tokio::process::Command::new(shell).arg(arg).arg(cmd).output();
    // Bound execution so a hung command can't stall the whole workflow run.
    match tokio::time::timeout(Duration::from_secs(600), fut).await {
        Ok(Ok(o)) => {
            let stdout = String::from_utf8_lossy(&o.stdout).to_string();
            let stderr = String::from_utf8_lossy(&o.stderr).to_string();
            if o.status.success() {
                Ok(json!({
                    "stdout": stdout,
                    "stderr": stderr,
                    "exit_code": o.status.code()
                }))
            } else {
                let detail = if stderr.trim().is_empty() { &stdout } else { &stderr };
                Err(format!(
                    "Command failed (exit code {:?}): {}",
                    o.status.code(),
                    detail
                ))
            }
        }
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("Shell command timed out after 600s".to_string()),
    }
}
