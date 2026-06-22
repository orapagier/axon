use crate::state::AppState;
use serde_json::Value;

pub(crate) async fn execute(config: &Value, state: &AppState) -> Result<Value, String> {
    let tool_name = config
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if tool_name.is_empty() {
        return Err("MCP node: tool_name is required".into());
    }

    let mut args = serde_json::Map::new();
    if let Some(obj) = config.as_object() {
        for (k, v) in obj {
            // credential_id is workflow plumbing, not a tool argument
            if k != "tool_name" && k != "mcp_server" && k != "credential_id" {
                args.insert(k.clone(), v.clone());
            }
        }
    }

    let server = config
        .get("mcp_server")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let all_tools = state.tools.all().await;
    let is_internal = all_tools
        .iter()
        .find(|t| t.name == tool_name)
        .map(|t| t.source == crate::tools::schema::ToolSource::Internal)
        .unwrap_or(false);

    if is_internal {
        match crate::agent::r#loop::execute_internal_tool_from_workflow(
            tool_name,
            Value::Object(args),
            state.clone(),
        )
        .await
        {
            Ok(v) => Ok(v),
            Err(e) => Err(e.to_string()),
        }
    } else {
        let server_name = if !server.is_empty() {
            server.to_string()
        } else {
            all_tools
                .iter()
                .find(|t| t.name == tool_name)
                .and_then(|t| match &t.source {
                    crate::tools::schema::ToolSource::Mcp { server_name, .. } => {
                        Some(server_name.clone())
                    }
                    _ => None,
                })
                .unwrap_or_else(|| "axon-mcp".to_string())
        };

        match state
            .mcp
            .call(&server_name, tool_name, Value::Object(args))
            .await
        {
            // normalize_mcp_output converts MCP isError responses into
            // {"error":true,"message":...} — surface those as node
            // failures instead of reporting success.
            Ok(v) => {
                if v.get("error").and_then(|b| b.as_bool()).unwrap_or(false) {
                    let msg = v
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("MCP tool returned an error");
                    Err(format!("MCP tool '{}' failed: {}", tool_name, msg))
                } else {
                    Ok(v)
                }
            }
            Err(e) => Err(e.to_string()),
        }
    }
}
