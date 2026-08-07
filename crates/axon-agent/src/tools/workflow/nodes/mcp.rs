use crate::state::AppState;
use serde_json::Value;

/// Tool-name prefixes served by the Google backend. A node running one of these
/// honours the Google account picked in its `credential_id`; everything else
/// ignores the field (it stays plain workflow plumbing).
///
/// Mirrors `mcp::inprocess::is_google` — the routing that decides which service
/// actually handles the call.
const GOOGLE_TOOL_PREFIXES: &[&str] = &[
    "google_",
    "gmail_",
    "gcal_",
    "gdrive_",
    "gdocs_",
    "gsheets_",
    "gcon_",
    "gmeet_",
    "gtasks_",
    "gslides_",
    "gforms_",
    "gchat_",
    "gyoutube_",
    "gplaces_",
];

fn is_google_tool(tool_name: &str) -> bool {
    // Registry names can be namespaced (`server:gmail_send`); match the bare tail
    // the same way the UI groups tools into per-service nodes.
    let bare = tool_name
        .rsplit([':', '.', '/'])
        .next()
        .unwrap_or(tool_name);
    GOOGLE_TOOL_PREFIXES.iter().any(|p| bare.starts_with(p))
}

pub(crate) async fn execute(config: &Value, state: &AppState) -> Result<Value, String> {
    let tool_name = config
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if tool_name.is_empty() {
        return Err("MCP node: tool_name is required".into());
    }

    // A Gmail/Calendar/Drive node may run as a specific connected account; with
    // none picked it stays on the globally signed-in one.
    if is_google_tool(tool_name) {
        let credential_id = crate::google_accounts::credential_id_of(config);
        if !credential_id.is_empty() {
            return crate::google_accounts::scoped(state, &credential_id, run(config, state)).await;
        }
    }
    run(config, state).await
}

async fn run(config: &Value, state: &AppState) -> Result<Value, String> {
    let tool_name = config
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_backed_tools_are_recognised() {
        for name in [
            "gmail_send",
            "gmail_list",
            "gcal_create_event",
            "gdrive_upload",
            "gsheets_read",
            "google_auth_status",
            "gyoutube_activities_list",
            "gplaces_search_text",
        ] {
            assert!(is_google_tool(name), "{name} should be Google-backed");
        }
    }

    #[test]
    fn namespaced_registry_names_still_match() {
        for name in [
            "axon-mcp:gmail_send",
            "server.gmail_send",
            "server/gmail_send",
        ] {
            assert!(is_google_tool(name), "{name} should be Google-backed");
        }
    }

    #[test]
    fn other_services_are_left_alone() {
        // These reach a different backend, so a Google account selection on them
        // would be a no-op — `is_google` in mcp/inprocess.rs does not route them
        // to the Google service either.
        for name in ["outlook_send", "facebook_post", "crm_lead_create", ""] {
            assert!(!is_google_tool(name), "{name} should not be Google-backed");
        }
    }

    /// This list must not drift from the routing in `mcp::inprocess::is_google`:
    /// a prefix here that is not routed to Google would show an account picker
    /// that does nothing, and one missing here would silently ignore the
    /// account the user picked.
    #[test]
    fn prefixes_match_the_inprocess_google_routing() {
        for name in GOOGLE_TOOL_PREFIXES {
            let sample = format!("{name}probe");
            assert!(
                crate::mcp::inprocess::is_google(&sample),
                "{sample} is offered an account picker but is not routed to Google"
            );
        }
    }

    #[test]
    fn prefix_match_is_not_a_substring_match() {
        // A tool that merely *contains* a Google prefix must not be captured.
        assert!(!is_google_tool("send_gmail_summary"));
    }
}
