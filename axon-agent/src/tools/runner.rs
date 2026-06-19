use anyhow::Context;
use futures::{stream::FuturesUnordered, StreamExt};
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::time::timeout;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallResult {
    pub tool_use_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
    pub output: serde_json::Value,
    pub error: Option<String>,
    pub duration_ms: u64,
}

/// Cached path to a verified working Python interpreter.
static PYTHON_PATH: OnceCell<String> = OnceCell::new();

/// Discover a real, working Python interpreter.
/// Probes multiple candidates and verifies each one actually works
/// (not a Windows Store stub that returns "Python was not found").
async fn find_python() -> anyhow::Result<String> {
    if let Some(cached) = PYTHON_PATH.get() {
        return Ok(cached.clone());
    }

    // Candidates to try, in priority order
    let candidates: Vec<String> = {
        let mut c = vec![];
        // 1. PYTHON_PATH env override (user can set this)
        if let Ok(p) = std::env::var("AXON_PYTHON_PATH") {
            if !p.is_empty() {
                c.push(p);
            }
        }
        // 2. Common Windows install locations
        if cfg!(windows) {
            // Try to find Python via `where python` output — but skip WindowsApps stubs
            if let Ok(out) = std::process::Command::new("where.exe")
                .arg("python")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
            {
                let paths = String::from_utf8_lossy(&out.stdout);
                for line in paths.lines() {
                    let line = line.trim();
                    if !line.is_empty() && !line.to_lowercase().contains("windowsapps") {
                        c.push(line.to_string());
                    }
                }
            }
            // Common Windows Python install paths
            for ver in &["314", "313", "312", "311", "310", "39", "38"] {
                c.push(format!("C:\\Python{}\\python.exe", ver));
            }
            // User local installs
            if let Ok(local) = std::env::var("LOCALAPPDATA") {
                for ver in &["3.14", "3.13", "3.12", "3.11", "3.10", "3.9", "3.8"] {
                    c.push(format!(
                        "{}\\Programs\\Python\\Python{}\\python.exe",
                        local,
                        ver.replace('.', "")
                    ));
                }
            }
        }
        // 3. Generic names (unix and fallback)
        c.push("python3".into());
        c.push("python".into());
        c
    };

    for candidate in &candidates {
        match std::process::Command::new(candidate)
            .args(["--version"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
        {
            Ok(out) => {
                let combined = format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                // Verify it's a real Python (not the Windows Store stub)
                if out.status.success()
                    && combined.contains("Python")
                    && !combined.contains("not found")
                {
                    let resolved = candidate.clone();
                    tracing::info!("Python interpreter resolved: {}", resolved);
                    let _ = PYTHON_PATH.set(resolved.clone());
                    return Ok(resolved);
                }
            }
            Err(_) => continue,
        }
    }

    anyhow::bail!(
        "No working Python interpreter found. Tried: {:?}. \
         Install Python or set AXON_PYTHON_PATH env var to the python executable path.",
        candidates
    )
}

pub async fn run_python(
    path: &str,
    args: serde_json::Value,
    timeout_sec: u64,
) -> anyhow::Result<serde_json::Value> {
    let python = find_python().await?;
    let args_json = serde_json::to_string(&args)?;

    let output = timeout(
        Duration::from_secs(timeout_sec),
        Command::new(&python)
            .arg(path)
            .arg(&args_json)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Tool timed out after {}s", timeout_sec))?
    .with_context(|| format!("Failed to spawn Python ({}) for tool {}", python, path))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        anyhow::bail!(
            "Tool failed ({}): {}{}",
            output.status,
            stderr,
            if stderr.is_empty() {
                stdout.to_string()
            } else {
                String::new()
            }
        );
    }
    let stdout = String::from_utf8(output.stdout)
        .context("Tool stdout not UTF-8")?
        .trim()
        .to_string();
    Ok(serde_json::from_str(&stdout).unwrap_or(serde_json::json!({ "output": stdout })))
}

pub async fn run_single(
    name: &str,
    id: &str,
    args: serde_json::Value,
    registry: Arc<crate::tools::registry::ToolRegistry>,
) -> ToolCallResult {
    let t0 = Instant::now();
    match registry.run(name, args.clone()).await {
        Ok(output) => ToolCallResult {
            tool_use_id: id.to_string(),
            tool_name: name.to_string(),
            input: args,
            output,
            error: None,
            duration_ms: t0.elapsed().as_millis() as u64,
        },
        Err(e) => ToolCallResult {
            tool_use_id: id.to_string(),
            tool_name: name.to_string(),
            input: args,
            output: serde_json::json!({ "error": e.to_string() }),
            error: Some(e.to_string()),
            duration_ms: t0.elapsed().as_millis() as u64,
        },
    }
}

pub async fn run_parallel(
    calls: Vec<crate::providers::types::ToolCall>,
    registry: Arc<crate::tools::registry::ToolRegistry>,
    max_parallel: usize,
) -> Vec<ToolCallResult> {
    if calls.is_empty() {
        return vec![];
    }
    if calls.len() == 1 {
        let c = &calls[0];
        return vec![run_single(&c.name, &c.id, c.input.clone(), registry).await];
    }
    let mut pending = FuturesUnordered::new();
    let mut results = vec![];
    let mut iter = calls.into_iter();
    while pending.len() < max_parallel {
        if let Some(c) = iter.next() {
            let reg = Arc::clone(&registry);
            pending.push(tokio::spawn(async move {
                run_single(&c.name, &c.id, c.input, reg).await
            }));
        } else {
            break;
        }
    }
    while let Some(res) = pending.next().await {
        if let Ok(r) = res {
            results.push(r);
        }
        if let Some(c) = iter.next() {
            let reg = Arc::clone(&registry);
            pending.push(tokio::spawn(async move {
                run_single(&c.name, &c.id, c.input, reg).await
            }));
        }
    }
    results
}
