//! Run-scoped tool discovery for the "hybrid" tool scope.
//!
//! With a large registry (hundreds of tools), sending every schema each
//! iteration burns tokens and rate limits on providers without prompt
//! caching. Hybrid scope sends the router's cheap subset instead, plus a
//! `search_tools` meta-tool: when the model needs a capability it can't see,
//! it searches by keyword and the matches join this run's *discovered set*,
//! which is unioned into the tool list from the next iteration on (Anthropic's
//! "tool search with deferred loading" pattern). Unknown-tool teaching errors
//! feed the same set, so a wrong guess self-corrects in one step.
//!
//! State is keyed by run id and cleared in `finalize` on every exit path.

use crate::tools::schema::ToolDefinition;
use once_cell::sync::Lazy;
use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

static DISCOVERED: Lazy<Mutex<HashMap<String, BTreeSet<String>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub fn discover(run_id: &str, names: &[String]) {
    if names.is_empty() {
        return;
    }
    let mut g = DISCOVERED.lock().unwrap();
    let set = g.entry(run_id.to_string()).or_default();
    for n in names {
        set.insert(n.clone());
    }
}

/// Sorted (BTreeSet order) so the resulting tool list is deterministic —
/// keeps provider-side prompt caches stable across iterations.
pub fn discovered(run_id: &str) -> Vec<String> {
    DISCOVERED
        .lock()
        .unwrap()
        .get(run_id)
        .map(|s| s.iter().cloned().collect())
        .unwrap_or_default()
}

pub fn clear(run_id: &str) {
    DISCOVERED.lock().unwrap().remove(run_id);
}

/// Keyword search over tool names + descriptions. A term hit in the name
/// weighs 3× a description hit; ties break alphabetically for determinism.
/// Falls back to fuzzy name matching when nothing scores, so a near-miss
/// query still surfaces something useful.
pub fn search(query: &str, all_tools: &[ToolDefinition], k: usize) -> Vec<ToolDefinition> {
    let terms: Vec<String> = query
        .to_ascii_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(str::to_string)
        .collect();
    if terms.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(i32, &ToolDefinition)> = all_tools
        .iter()
        .filter_map(|t| {
            let name = t.name.to_ascii_lowercase();
            let desc = t.description.to_ascii_lowercase();
            let mut score = 0;
            for term in &terms {
                if name.contains(term.as_str()) {
                    score += 3;
                }
                if desc.contains(term.as_str()) {
                    score += 1;
                }
            }
            (score > 0).then_some((score, t))
        })
        .collect();
    if scored.is_empty() {
        return crate::tools::schema::closest_tool_names(query, all_tools, k)
            .iter()
            .filter_map(|n| all_tools.iter().find(|t| &t.name == n).cloned())
            .collect();
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
    scored.into_iter().take(k).map(|(_, t)| t.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, desc: &str) -> ToolDefinition {
        ToolDefinition::internal(name, desc, serde_json::json!({}), vec![])
    }

    #[test]
    fn search_ranks_name_hits_over_description_hits() {
        let tools = vec![
            tool("gmail_send", "Send a Gmail email."),
            tool("gmail_list", "List Gmail messages."),
            tool("outlook_send_email", "Send an Outlook email."),
            tool("gcal_create_event", "Create a Google Calendar event."),
        ];
        let got = search("send email", &tools, 2);
        let names: Vec<&str> = got.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"gmail_send") && names.contains(&"outlook_send_email"),
            "got {:?}",
            names
        );
        // Fuzzy fallback when no term matches.
        let got = search("gmial", &tools, 1);
        assert!(!got.is_empty(), "fuzzy fallback should surface something");
    }

    #[test]
    fn discovered_set_lifecycle() {
        let rid = "test-run-discovery";
        assert!(discovered(rid).is_empty());
        discover(rid, &["b_tool".into(), "a_tool".into()]);
        discover(rid, &["a_tool".into()]);
        assert_eq!(
            discovered(rid),
            vec!["a_tool".to_string(), "b_tool".to_string()]
        );
        clear(rid);
        assert!(discovered(rid).is_empty());
    }
}
