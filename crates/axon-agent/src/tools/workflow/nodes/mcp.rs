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

/// Turn a node's stored config into the tool's argument map.
///
/// A service node (YouTube, Gmail, …) keeps one config map across every action
/// it can run, so switching action — Videos insert → Channels list, say — leaves
/// the previous action's keys behind. Sending those makes the backend reject an
/// otherwise valid call ("does not support media upload" from a leftover
/// `upload_file_path`), so anything the selected tool doesn't declare is dropped.
///
/// `declared` is the tool's schema properties. A tool that publishes none is left
/// alone: there is nothing to filter against, and its args are passed through.
fn tool_args(
    config: &Value,
    declared: Option<&serde_json::Map<String, Value>>,
) -> serde_json::Map<String, Value> {
    let declared = declared.filter(|props| !props.is_empty());
    let mut args = serde_json::Map::new();
    let Some(obj) = config.as_object() else {
        return args;
    };
    for (k, v) in obj {
        // Workflow plumbing, not tool arguments.
        if k == "tool_name" || k == "mcp_server" || k == "credential_id" {
            continue;
        }
        if declared.is_some_and(|props| !props.contains_key(k)) {
            continue;
        }
        args.insert(k.clone(), v.clone());
    }
    args
}

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

    let server = config
        .get("mcp_server")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let all_tools = state.tools.all().await;
    let tool = all_tools.iter().find(|t| t.name == tool_name);

    let args = tool_args(config, tool.and_then(|t| t.parameters.as_object()));

    let is_internal = tool
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
            tool.and_then(|t| match &t.source {
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
    use serde_json::json;

    fn props(keys: &[&str]) -> serde_json::Map<String, Value> {
        keys.iter()
            .map(|k| ((*k).to_string(), json!({ "type": "string" })))
            .collect()
    }

    #[test]
    fn workflow_plumbing_is_never_sent_as_an_argument() {
        let config = json!({
            "tool_name": "gyoutube_channels_list",
            "mcp_server": "axon-mcp",
            "credential_id": "cred-1",
            "part": ["snippet"],
        });
        let args = tool_args(&config, Some(&props(&["part"])));
        assert_eq!(args.len(), 1);
        assert_eq!(args["part"], json!(["snippet"]));
    }

    #[test]
    fn keys_from_a_previously_selected_action_are_dropped() {
        // The node was on Videos insert, then switched to Channels list; the
        // leftover upload field used to reach the API and fail the call.
        let config = json!({
            "tool_name": "gyoutube_channels_list",
            "part": ["snippet"],
            "upload_file_path": "",
            "title": "an old draft title",
        });
        let args = tool_args(&config, Some(&props(&["part", "params"])));
        assert!(!args.contains_key("upload_file_path"));
        assert!(!args.contains_key("title"));
        assert!(args.contains_key("part"));
    }

    #[test]
    fn declared_fields_survive_even_when_blank() {
        // Blank is meaningful for some tools (clearing a value); only *undeclared*
        // keys are filtered here.
        let config = json!({ "tool_name": "t", "description": "" });
        let args = tool_args(&config, Some(&props(&["description"])));
        assert_eq!(args["description"], json!(""));
    }

    #[test]
    fn a_tool_without_a_published_schema_passes_everything_through() {
        let config = json!({ "tool_name": "t", "anything": 1 });
        assert_eq!(tool_args(&config, None)["anything"], json!(1));
        assert_eq!(
            tool_args(&config, Some(&serde_json::Map::new()))["anything"],
            json!(1)
        );
    }

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
