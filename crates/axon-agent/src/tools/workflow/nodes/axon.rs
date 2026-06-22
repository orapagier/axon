use crate::state::AppState;
use serde_json::Value;

pub(crate) fn execute_axon_node<'a>(
    config: &'a Value,
    state: &'a AppState,
    workflow_id: &'a str,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send + 'a>> {
    Box::pin(async move {
        // Extract stimulus — handle any value type since interpolation may return Object/Array
        let stimulus = match config.get("stimulus") {
            Some(Value::String(s)) => s.trim().to_string(),
            Some(Value::Null) | None => String::new(),
            Some(other) => serde_json::to_string_pretty(other).unwrap_or_default(),
        };

        // Extract cortex — same resilient extraction
        let cortex = match config.get("cortex") {
            Some(Value::String(s)) => s.trim().to_string(),
            Some(Value::Null) | None => String::new(),
            Some(other) => serde_json::to_string_pretty(other).unwrap_or_default(),
        };

        if stimulus.is_empty() {
            return Err("Axon node: stimulus (User Prompt) is empty".to_string());
        }

        // Extract optional model selection
        let selected_model = config
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);

        // Per-node memory toggle (default ON for backwards compatibility).
        let memory_enabled = config
            .get("memory_enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        // Extract optional tools selection
        let selected_tools: Vec<String> = config
            .get("tools")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // Build system prompt with optional tool injection
        let mut system_prompt_text = cortex.clone();
        if !selected_tools.is_empty() {
            let tool_section = format!(
                "\n\n[TOOL RESTRICTION]\nYou have access ONLY to the following tools: {}. \
                 Use only these tools to fulfill the user's request. Do not attempt to use any other tools.",
                selected_tools.join(", ")
            );
            system_prompt_text.push_str(&tool_section);
        }

        if let Some(ref model) = selected_model {
            tracing::info!(
                "Axon node: user selected model '{}' for workflow {}",
                model,
                workflow_id
            );
        }

        let system_prompt = if system_prompt_text.is_empty() {
            None
        } else {
            Some(system_prompt_text.as_str())
        };

        // Run through the Axon agent with an isolated session per workflow
        let session = format!("wf:{}", workflow_id);
        let mut ctx = crate::agent::RunContext::new(
            &stimulus,
            "workflow",
            Some(&session),
            None,
            None,
            None,
            system_prompt,
        );
        ctx.preferred_model = selected_model;
        ctx.memory_enabled = memory_enabled;

        if !selected_tools.is_empty() {
            ctx.allowed_tools = Some(selected_tools);
        }

        let agent_response = crate::agent::run_task(&stimulus, state, ctx)
            .await
            .map_err(|e| format!("Axon agent error: {}", e))?;

        Ok(serde_json::json!({
            "output": agent_response
        }))
    })
}
