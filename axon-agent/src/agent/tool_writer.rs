use crate::config::RuntimeSettings;
use crate::providers::types::Message;
use crate::router::{call_llm, SharedRouter};
use anyhow::Context;
use std::sync::Arc;
use uuid::Uuid;

pub async fn write_temporary_tool(
    description: &str,
    input_schema: &serde_json::Value,
    router: SharedRouter,
    settings: &RuntimeSettings,
    max_retries: u32,
) -> anyhow::Result<String> {
    for attempt in 1..=max_retries {
        match attempt_write(description, input_schema, &router, settings).await {
            Ok(name) => {
                tracing::info!("Agent wrote temp tool: {} (attempt {})", name, attempt);
                return Ok(name);
            }
            Err(e) => {
                tracing::warn!("Tool write {}/{} failed: {}", attempt, max_retries, e);
                if attempt == max_retries {
                    return Err(e);
                }
            }
        }
    }
    unreachable!()
}

async fn attempt_write(
    description: &str,
    input_schema: &serde_json::Value,
    router: &SharedRouter,
    settings: &RuntimeSettings,
) -> anyhow::Result<String> {
    let prompt = format!(
        "Write a Python tool with this EXACT docstring header:\n\"\"\"\nTOOL_NAME: <snake_case_name>\nDESCRIPTION: <one sentence>\nPARAMETERS: <json object>\nREQUIRED: <json array>\n\"\"\"\n\nRules:\n1. args = json.loads(sys.argv[1]) if len(sys.argv) > 1 else {{}}\n2. Print ONLY valid JSON to stdout\n3. Print to stderr and sys.exit(1) on failure\n4. Use only stdlib + requests\n\nTool description: {}\nInput schema: {}\n\nWrite ONLY the Python code, no markdown.",
        description, serde_json::to_string_pretty(input_schema).unwrap_or_default());
    let messages = [Message::user(&prompt)];
    let system = "Write Python tool scripts. Output only Python code, no markdown backticks.";
    let (resp, model, _tier) = call_llm(
        &messages,
        system,
        &[],
        None,
        "tool_writer",
        Arc::clone(router),
        settings,
        None,
    )
    .await?;
    let mut code = resp.text_content().trim().to_string();
    for fence in &["```python", "```"] {
        if code.starts_with(fence) {
            code = code[fence.len()..].to_string();
        }
    }
    if code.ends_with("```") {
        code = code[..code.len() - 3].to_string();
    }
    let code = code.trim().to_string();
    tracing::debug!("Tool writer ({}) generated {} chars", model, code.len());
    if !code.contains("TOOL_NAME:") {
        anyhow::bail!("Missing TOOL_NAME in generated code");
    }
    let name = extract_name(&code).context("Cannot extract TOOL_NAME")?;
    let tmp = format!("tools_temp/tmp_{}.py", Uuid::new_v4().simple());
    std::fs::write(&tmp, &code).with_context(|| format!("Write {}", tmp))?;
    let check_fut = tokio::process::Command::new("python3")
        .args(["-m", "py_compile", &tmp])
        .output();
    let check = match check_fut.await {
        Ok(out) => out,
        Err(_) => tokio::process::Command::new("python")
            .args(["-m", "py_compile", &tmp])
            .output()
            .await
            .context("py_compile")?,
    };
    if !check.status.success() {
        std::fs::remove_file(&tmp).ok();
        anyhow::bail!("Syntax error: {}", String::from_utf8_lossy(&check.stderr));
    }
    let final_path = format!("tools_temp/{}.py", name);
    std::fs::rename(&tmp, &final_path).context("Rename temp tool")?;
    Ok(name)
}

fn extract_name(code: &str) -> Option<String> {
    code.lines().find_map(|line| {
        let line = line.trim();
        line.starts_with("TOOL_NAME:").then(|| {
            line["TOOL_NAME:".len()..]
                .trim()
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect()
        })
    })
}
