use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "source_type", rename_all = "snake_case")]
pub enum ToolSource {
    Python {
        path: String,
    },
    Mcp {
        server_name: String,
        tool_name: String,
    },
    Temp {
        path: String,
    },
    Internal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub required: Vec<String>,
    pub source: ToolSource,
    pub enabled: bool,
    /// True when the tool performs a side effect (sends/creates/deletes/etc.).
    /// This is the authoritative source of truth for mutation classification —
    /// it replaces the old substring-scan heuristic that false-positived read
    /// tools whose names merely *contained* a write marker (e.g. `fb_list_posts`
    /// matching `_post`). Defaults via `derive_is_mutating`, but can be set
    /// explicitly per tool (internal tools, or a `MUTATING:` Python docstring).
    #[serde(default)]
    pub is_mutating: bool,
}

impl ToolDefinition {
    pub fn internal(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
        required: Vec<String>,
    ) -> Self {
        let name = name.into();
        let is_mutating = derive_is_mutating(&name);
        ToolDefinition {
            name,
            description: description.into(),
            parameters,
            required,
            source: ToolSource::Internal,
            enabled: true,
            is_mutating,
        }
    }
    pub fn from_python_file(path: &str) -> anyhow::Result<Self> {
        let src = std::fs::read_to_string(path)?;
        let name = meta(&src, "TOOL_NAME")?;
        let desc = meta(&src, "DESCRIPTION").unwrap_or_else(|_| "No description".into());
        let pstr = meta(&src, "PARAMETERS").unwrap_or_else(|_| "{}".into());
        let rstr = meta(&src, "REQUIRED").unwrap_or_else(|_| "[]".into());
        // Optional `MUTATING: true|false` docstring header overrides the
        // name-based default; otherwise derive it.
        let is_mutating = meta(&src, "MUTATING")
            .ok()
            .and_then(|v| match v.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "1" => Some(true),
                "false" | "no" | "0" => Some(false),
                _ => None,
            })
            .unwrap_or_else(|| derive_is_mutating(&name));
        Ok(ToolDefinition {
            name,
            description: desc,
            parameters: serde_json::from_str(&pstr).unwrap_or(serde_json::json!({})),
            required: serde_json::from_str(&rstr).unwrap_or_default(),
            source: ToolSource::Python {
                path: path.to_string(),
            },
            enabled: true,
            is_mutating,
        })
    }
}

/// Default classification of whether a tool mutates state, derived from its name.
///
/// Tool names follow `[service_]verb_object`, so the **first action verb** in the
/// name decides — not a substring scan. This is what fixes the old false
/// positives: `fb_list_posts` resolves on `list` (read) before ever seeing
/// `posts`, and `fb_get_scheduled_posts` resolves on `get` before `scheduled`.
///
/// Used as the default when a tool does not carry an explicit `is_mutating`
/// flag (e.g. MCP tools, or as a fallback for unknown/temp tools).
pub fn derive_is_mutating(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();

    // Internal tools whose names carry no action verb but which always perform
    // side effects (or are dispatched by an `action` arg that may mutate).
    // Classified conservatively as mutating so the claim guard stays strict.
    const ALWAYS_MUTATING: &[&str] = &[
        "shell_tool",
        "ssh_tool",
        "cron_job_tool",
        "watcher_tool",
        "parallel_worker",
        "image_tool",
    ];
    if ALWAYS_MUTATING.contains(&lower.as_str()) {
        return true;
    }

    // Whole-token verb match. The first action verb encountered wins.
    const READ_VERBS: &[&str] = &[
        "list", "get", "search", "read", "fetch", "show", "view", "recent", "find", "download",
        "freebusy", "insights", "status", "history", "count", "info", "lookup", "check", "preview",
    ];
    const WRITE_VERBS: &[&str] = &[
        "create", "send", "reply", "delete", "update", "upload", "move", "share", "trash",
        "untrash", "mark", "add", "remove", "write", "append", "clear", "set", "edit", "complete",
        "run", "execute", "trigger", "hide", "unhide", "react", "unreact", "merge", "unmerge",
        "sort", "freeze", "bold", "duplicate", "copy", "draft", "compose", "forward", "publish",
        "rename", "insert", "schedule", "resize", "format", "like", "unlike", "remind", "replace",
        "quick",
    ];
    for token in lower.split('_') {
        if READ_VERBS.contains(&token) {
            return false;
        }
        if WRITE_VERBS.contains(&token) {
            return true;
        }
    }

    // No recognizable verb and not a known side-effecting internal tool → read.
    false
}

fn meta(src: &str, key: &str) -> anyhow::Result<String> {
    for line in src.lines() {
        let line = line.trim();
        let prefix = format!("{}:", key);
        if line.starts_with(&prefix) {
            return Ok(line[prefix.len()..].trim().to_string());
        }
    }
    anyhow::bail!("{} not found in docstring", key)
}

#[cfg(test)]
mod tests {
    use super::derive_is_mutating;

    #[test]
    fn read_tools_are_not_mutating() {
        // Regression: these previously matched substring markers (`_post`,
        // `_schedule`, `_set`) and were wrongly classified as mutating — which
        // let a successful read vouch for a fabricated write in the claim guard.
        for name in [
            "fb_list_posts",
            "fb_get_post",
            "fb_get_scheduled_posts",
            "fb_post_insights",
            "fb_page_insights",
            "fb_recent_comments",
            "gmail_list",
            "gmail_get",
            "gcal_get_freebusy",
            "outlook_read_email",
            "gdrive_download_binary",
            "get_settings",
            "list_workflows",
            "list_synapses",
        ] {
            assert!(!derive_is_mutating(name), "{name} should be read-only");
        }
    }

    #[test]
    fn write_tools_are_mutating() {
        for name in [
            "fb_create_post",
            "fb_schedule_post",
            "fb_delete_post",
            "gmail_send",
            "gmail_mark_read",
            "gcal_create_event",
            "onedrive_upload_binary",
            "gtasks_complete_task",
            "gdrive_delete",
            "run_workflow",
            "shell_tool",
            "ssh_tool",
        ] {
            assert!(derive_is_mutating(name), "{name} should be mutating");
        }
    }
}
