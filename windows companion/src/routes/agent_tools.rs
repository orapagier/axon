//! Tool-RPC surface: the protocol axon-agent's `DeviceTool` speaks.
//!
//!   GET  /agent/tools  → the catalogue the LLM reads to learn this machine
//!   POST /agent/tool   → { "tool": "shell.run", "params": { … } }
//!
//! The REST routes are still the real API; this is a thin naming layer over
//! them so the agent can drive a Windows companion with the same code path it
//! uses for AndroidCompanion. Each tool maps to one (method, path, body) and
//! goes back through `agent::dispatch`, which means desktop tools are forwarded
//! to the session agent and screenshots come back as URLs without this module
//! knowing anything about either.
//!
//! Tool names mirror the Android companion's dotted namespaces so a model that
//! has seen one reads the other without relearning.

use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::{routes::AppError, server::AppState};

#[derive(Deserialize)]
pub struct ToolCall {
    pub tool: String,
    #[serde(default)]
    pub params: Value,
}

/// Where a tool has to run, which is the one thing about this machine a model
/// cannot guess. Surfaced in the catalogue so it can pick `shell.run` over
/// `shell.run_as_user` when the laptop might be locked, instead of discovering
/// the difference through a 503.
#[derive(Clone, Copy, PartialEq)]
enum Plane {
    /// LocalSystem, session 0. Works locked, logged out, at the login screen.
    Service,
    /// Needs the interactive desktop. 503s when nobody is logged on.
    Desktop,
}

impl Plane {
    fn as_str(self) -> &'static str {
        match self {
            Plane::Service => "service",
            Plane::Desktop => "desktop",
        }
    }

    fn availability(self) -> &'static str {
        match self {
            Plane::Service => "Always available, including while the machine is locked.",
            Plane::Desktop => {
                "Requires a logged-in interactive session. Returns 503 NO_DESKTOP_SESSION \
                 when the machine is locked or at the login screen."
            }
        }
    }
}

struct ToolDef {
    name: &'static str,
    description: &'static str,
    params: &'static [&'static str],
    method: &'static str,
    path: &'static str,
    plane: Plane,
}

/// The catalogue. Adding a route to the REST API does not expose it here — that
/// is deliberate, so what the LLM can reach stays an explicit decision.
const TOOLS: &[ToolDef] = &[
    // ── Shell ────────────────────────────────────────────────────────────────
    ToolDef {
        name: "shell.run",
        description:
            "Run a PowerShell or cmd command as LocalSystem. Works while the machine is \
             locked. Cannot see the user's HKCU, mapped drives, or DPAPI-protected secrets, \
             and authenticates on the network as the machine account — use shell.run_as_user \
             for anything touching the logged-in user's own environment.",
        params: &["command", "shell? (powershell|cmd)", "timeout_secs? (default 30, max 3600)", "cwd?"],
        method: "POST",
        path: "/shell",
        plane: Plane::Service,
    },
    ToolDef {
        name: "shell.run_as_user",
        description:
            "Run a command as the logged-in user, with their real profile, HKCU, mapped \
             drives and environment. Needs someone logged in.",
        params: &["command", "shell? (powershell|cmd)", "timeout_secs?", "cwd?"],
        method: "POST",
        path: "/shell",
        plane: Plane::Desktop,
    },
    // ── Screen ───────────────────────────────────────────────────────────────
    ToolDef {
        name: "screen.capture",
        description:
            "Capture the screen. Returns a download URL, not base64. Screens are indexed \
             from 0; screen_count in the response says how many exist.",
        params: &["screen? (int, default 0)", "format? (png|jpeg)", "crop_x?", "crop_y?", "crop_w?", "crop_h?"],
        method: "POST",
        path: "/screenshot",
        plane: Plane::Desktop,
    },
    // ── Clipboard ────────────────────────────────────────────────────────────
    ToolDef {
        name: "clipboard.get",
        description: "Read the logged-in user's clipboard text.",
        params: &[],
        method: "POST",
        path: "/clipboard",
        plane: Plane::Desktop,
    },
    ToolDef {
        name: "clipboard.set",
        description: "Replace the logged-in user's clipboard text.",
        params: &["text"],
        method: "POST",
        path: "/clipboard/set",
        plane: Plane::Desktop,
    },
    // ── Files ────────────────────────────────────────────────────────────────
    ToolDef {
        name: "files.read",
        description: "Read a text file and return its contents verbatim.",
        params: &["path"],
        method: "POST",
        path: "/files/read",
        plane: Plane::Service,
    },
    ToolDef {
        name: "files.write",
        description: "Write or append text to a file, creating it if needed.",
        params: &["path", "content", "append? (bool)"],
        method: "POST",
        path: "/files/write",
        plane: Plane::Service,
    },
    ToolDef {
        name: "files.list",
        description: "List the entries of a directory.",
        params: &["path"],
        method: "POST",
        path: "/files/list",
        plane: Plane::Service,
    },
    ToolDef {
        name: "files.search",
        description: "Find files by wildcard pattern under a directory.",
        params: &["path", "pattern? (e.g. *.pdf)", "recursive? (bool)", "limit? (default 200)"],
        method: "POST",
        path: "/files/search",
        plane: Plane::Service,
    },
    ToolDef {
        name: "files.delete",
        description: "Delete a file or directory. Irreversible — confirm with the user first.",
        params: &["path"],
        method: "POST",
        path: "/files/delete",
        plane: Plane::Service,
    },
    ToolDef {
        name: "files.exists",
        description: "Check whether a path exists and whether it is a file or a directory.",
        params: &["path"],
        method: "POST",
        path: "/files/exists",
        plane: Plane::Service,
    },
    ToolDef {
        name: "files.link",
        description:
            "Get a temporary public download URL for a local file, valid 30 minutes. Use \
             this to hand a file to the user rather than reading a large or binary file.",
        params: &["path"],
        method: "POST",
        path: "/files/link",
        plane: Plane::Service,
    },
    // ── System ───────────────────────────────────────────────────────────────
    ToolDef {
        name: "system.info",
        description: "OS version, CPU, RAM, hostname and uptime.",
        params: &[],
        method: "POST",
        path: "/system/info",
        plane: Plane::Service,
    },
    ToolDef {
        name: "system.status",
        description:
            "Which halves of this companion are currently answering. Call this first when a \
             desktop tool fails — it distinguishes 'nobody is logged in' from a real fault.",
        params: &[],
        method: "POST",
        path: "/status",
        plane: Plane::Service,
    },
    ToolDef {
        name: "system.power",
        description:
            "Lock, sleep, hibernate, log off, restart or shut down. Destructive — confirm \
             with the user first. Note that sleep and shutdown make this machine \
             unreachable until someone physically wakes it.",
        params: &["action (lock|sleep|hibernate|logoff|restart|shutdown|cancel_shutdown)", "delay_secs?"],
        method: "POST",
        path: "/system/power",
        plane: Plane::Service,
    },
    ToolDef {
        name: "process.list",
        description: "List running processes sorted by CPU usage.",
        params: &[],
        method: "POST",
        path: "/processes",
        plane: Plane::Service,
    },
    ToolDef {
        name: "process.kill",
        description: "Kill a process by pid, or by exact case-insensitive image name.",
        params: &["pid? (int)", "name? (e.g. notepad.exe)"],
        method: "POST",
        path: "/processes/kill",
        plane: Plane::Service,
    },
    // ── Registry ─────────────────────────────────────────────────────────────
    ToolDef {
        name: "registry.read",
        description: "Read a registry value.",
        params: &["key (e.g. HKEY_LOCAL_MACHINE\\\\SOFTWARE\\\\...)", "name"],
        method: "POST",
        path: "/registry/read",
        plane: Plane::Service,
    },
    ToolDef {
        name: "registry.write",
        description: "Write a registry value. Confirm with the user first.",
        params: &["key", "name", "value", "value_type? (string|dword|qword|expand_string|binary|multi_string)"],
        method: "POST",
        path: "/registry/write",
        plane: Plane::Service,
    },
    // ── Input ────────────────────────────────────────────────────────────────
    ToolDef {
        name: "input.type",
        description:
            "Type text into whatever window currently has focus. Use window.focus first, or \
             the keystrokes go to the wrong place.",
        params: &["text", "delay_ms?"],
        method: "POST",
        path: "/keyboard/type",
        plane: Plane::Desktop,
    },
    ToolDef {
        name: "input.key",
        description:
            "Press a key or chord, e.g. [\"ctrl\",\"c\"] or [\"win\",\"d\"] or [\"f5\"].",
        params: &["keys (string array)"],
        method: "POST",
        path: "/keyboard/key",
        plane: Plane::Desktop,
    },
    ToolDef {
        name: "mouse.click",
        description: "Click at a coordinate, or at the current pointer position if x/y are omitted.",
        params: &["x?", "y?", "button? (left|right|middle)", "double? (bool)"],
        method: "POST",
        path: "/mouse/click",
        plane: Plane::Desktop,
    },
    ToolDef {
        name: "mouse.move",
        description: "Move the pointer, absolutely or relative to where it is now.",
        params: &["x", "y", "mode? (abs|rel)"],
        method: "POST",
        path: "/mouse/move",
        plane: Plane::Desktop,
    },
    ToolDef {
        name: "mouse.scroll",
        description: "Scroll the wheel. Positive y scrolls down, negative scrolls up.",
        params: &["y", "mouse_x?", "mouse_y?"],
        method: "POST",
        path: "/mouse/scroll",
        plane: Plane::Desktop,
    },
    ToolDef {
        name: "mouse.drag",
        description: "Press at one point, drag, and release at another.",
        params: &["from_x", "from_y", "to_x", "to_y", "button?"],
        method: "POST",
        path: "/mouse/drag",
        plane: Plane::Desktop,
    },
    // ── Windows ──────────────────────────────────────────────────────────────
    ToolDef {
        name: "window.list",
        description: "List open top-level windows with their titles and handles.",
        params: &[],
        method: "POST",
        path: "/windows",
        plane: Plane::Desktop,
    },
    ToolDef {
        name: "window.focus",
        description:
            "Bring a window to the foreground, matched by title substring or hwnd. Call this \
             before input.type or input.key.",
        params: &["title?", "hwnd?"],
        method: "POST",
        path: "/windows/focus",
        plane: Plane::Desktop,
    },
    ToolDef {
        name: "window.close",
        description: "Close a window by title or hwnd.",
        params: &["title?", "hwnd?"],
        method: "POST",
        path: "/windows/close",
        plane: Plane::Desktop,
    },
    ToolDef {
        name: "window.resize",
        description: "Move and resize a window.",
        params: &["title?", "hwnd?", "x", "y", "width", "height"],
        method: "POST",
        path: "/windows/resize",
        plane: Plane::Desktop,
    },
    // ── Misc ─────────────────────────────────────────────────────────────────
    ToolDef {
        name: "notify.push",
        description: "Show a native Windows toast notification on the user's desktop.",
        params: &["title", "body", "app_id?"],
        method: "POST",
        path: "/notify",
        plane: Plane::Desktop,
    },
    ToolDef {
        name: "launch.open",
        description:
            "Open a file, folder, URL or application by name, as if double-clicked by the user.",
        params: &["target (path, URL, or app name e.g. notepad)"],
        method: "POST",
        path: "/open",
        plane: Plane::Desktop,
    },
];

fn find(name: &str) -> Option<&'static ToolDef> {
    TOOLS.iter().find(|t| t.name == name)
}

/// `GET /agent/tools` — the catalogue.
pub async fn list_tools() -> Json<Value> {
    let tools: Vec<Value> = TOOLS
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "params": t.params,
                "plane": t.plane.as_str(),
                "availability": t.plane.availability(),
            })
        })
        .collect();

    Json(json!({
        "tools": tools,
        "note":
            "This machine answers on two planes. 'service' tools run as LocalSystem and work \
             while it is locked or logged out. 'desktop' tools need a logged-in session and \
             return 503 NO_DESKTOP_SESSION otherwise — call system.status to check before \
             concluding something is broken.",
    }))
}

/// `POST /agent/tool` — execute one tool by name.
pub async fn call_tool(
    State(state): State<AppState>,
    Json(req): Json<ToolCall>,
) -> Result<Json<Value>, AppError> {
    let Some(def) = find(&req.tool) else {
        return Err(AppError::bad_request(format!(
            "Unknown tool '{}'.{} Call GET /agent/tools for the full catalogue.",
            req.tool,
            match closest(&req.tool) {
                Some(s) => format!(" Did you mean '{s}'?"),
                None => String::new(),
            }
        )));
    };

    let mut body = normalise_params(&req.params)?;

    // shell.run and shell.run_as_user are the same endpoint distinguished by
    // run_as, so the tool name decides it. Set unconditionally: honouring a
    // caller-supplied run_as would let shell.run silently become a desktop
    // call, which then 503s on a locked machine for no visible reason.
    if def.path == "/shell" {
        body.insert(
            "run_as".to_string(),
            json!(if def.plane == Plane::Desktop {
                "user"
            } else {
                "system"
            }),
        );
    }

    let result = crate::routes::agent::dispatch(&state, def.method, def.path, Value::Object(body))
        .await
        .map_err(|e| annotate_desktop_failure(e, def))?;

    Ok(Json(result))
}

/// Params may arrive as an object or as a JSON string.
///
/// axon-agent's synapse tool serialises the whole params object to a string
/// before sending; the Android companion handles both for the same reason, and
/// a Windows companion that only accepted objects would fail on every call from
/// that path.
fn normalise_params(raw: &Value) -> Result<Map<String, Value>, AppError> {
    match raw {
        Value::Object(m) => Ok(m.clone()),
        Value::Null => Ok(Map::new()),
        Value::String(s) if s.trim().is_empty() => Ok(Map::new()),
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(Value::Object(m)) => Ok(m),
            Ok(Value::Null) => Ok(Map::new()),
            _ => Err(AppError::bad_request(
                "params was a string but did not parse as a JSON object",
            )),
        },
        _ => Err(AppError::bad_request(
            "params must be a JSON object, or a string containing one",
        )),
    }
}

/// Turns the router's generic 503 into something that names the tool and says
/// what to do about it, rather than leaving the model to infer it.
fn annotate_desktop_failure(e: AppError, def: &ToolDef) -> AppError {
    if def.plane == Plane::Desktop && e.status == axum::http::StatusCode::SERVICE_UNAVAILABLE {
        return AppError {
            status: e.status,
            code: "NO_DESKTOP_SESSION",
            message: format!(
                "'{}' needs a logged-in interactive session and none is available — the \
                 machine is locked, at the login screen, or nobody is logged on. {} Service-plane \
                 tools (shell.run, files.*, system.*, process.*, registry.*) still work.",
                def.name, e.message
            ),
        };
    }
    e
}

/// Nearest tool name by edit distance, for the "did you mean" hint.
fn closest(name: &str) -> Option<&'static str> {
    let target = name.to_ascii_lowercase();
    TOOLS
        .iter()
        .map(|t| (t.name, levenshtein(&target, &t.name.to_ascii_lowercase())))
        // Beyond this the suggestion is noise rather than help.
        .filter(|(_, d)| *d <= 6)
        .min_by_key(|(_, d)| *d)
        .map(|(n, _)| n)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut cur = vec![0usize; b_chars.len() + 1];

    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b_chars.iter().enumerate() {
            let cost = if ca == *cb { 0 } else { 1 };
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b_chars.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_a_unique_name() {
        let mut names: Vec<&str> = TOOLS.iter().map(|t| t.name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "duplicate tool name in the catalogue");
    }

    #[test]
    fn no_tool_points_at_a_blocked_route() {
        // Every tool dispatches through the proxy, so a tool aimed at /agent/*
        // or /public/* would be rejected at runtime instead of at review time.
        for t in TOOLS {
            assert!(
                !t.path.starts_with("/agent") && !t.path.starts_with("/public"),
                "{} targets blocked route {}",
                t.name,
                t.path
            );
        }
    }

    #[test]
    fn shell_tools_are_the_only_shared_path() {
        // Two tool names mapping to one path is fine only because run_as makes
        // them different calls; anything else sharing a path is a mistake.
        let shell: Vec<&str> = TOOLS
            .iter()
            .filter(|t| t.path == "/shell")
            .map(|t| t.name)
            .collect();
        assert_eq!(shell, vec!["shell.run", "shell.run_as_user"]);
    }

    #[test]
    fn params_accept_object_string_and_null() {
        let want = json!({"command": "Get-Date"});

        let from_obj = normalise_params(&want).unwrap();
        assert_eq!(from_obj.get("command").unwrap(), "Get-Date");

        // The synapse-tool path: params arrives already serialised.
        let from_str = normalise_params(&json!(r#"{"command":"Get-Date"}"#)).unwrap();
        assert_eq!(from_str, from_obj);

        assert!(normalise_params(&Value::Null).unwrap().is_empty());
        assert!(normalise_params(&json!("")).unwrap().is_empty());
    }

    #[test]
    fn params_reject_non_objects() {
        assert!(normalise_params(&json!([1, 2, 3])).is_err());
        assert!(normalise_params(&json!(42)).is_err());
        assert!(normalise_params(&json!("not json at all")).is_err());
    }

    #[test]
    fn unknown_tool_suggests_a_near_match() {
        assert_eq!(closest("shell.runn"), Some("shell.run"));
        assert_eq!(closest("screen.captur"), Some("screen.capture"));
        assert_eq!(closest("clipboard.given"), Some("clipboard.get"));
    }

    #[test]
    fn wildly_wrong_names_suggest_nothing() {
        assert_eq!(closest("absolutely-not-a-tool-name-here"), None);
    }
}
