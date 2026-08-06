use crate::tools::http::{HttpAuth, HttpRequestParams, HttpRequestTool};
use crate::tools::schema::levenshtein;
use anyhow::{Context, Result};
use serde_json::{json, Value};

struct DeviceDetails {
    base_url: String,
    token: Option<String>,
}

pub struct DeviceTool;

impl DeviceTool {
    async fn get_device_details(
        state: &crate::state::AppState,
        name: &str,
    ) -> Result<DeviceDetails> {
        if let Ok(conn) = state.db.get() {
            let mut s = conn.prepare("SELECT base_url, bearer_token FROM devices WHERE name=?1")?;
            let mut rows = s.query(rusqlite::params![name])?;
            if let Some(r) = rows.next()? {
                let base_url: String = r.get(0)?;
                let enc_token: Option<String> = r.get(1)?;
                let token = enc_token
                    .filter(|t| !t.is_empty())
                    .map(|t| crate::crypto::decrypt_key(&t));
                return Ok(DeviceDetails {
                    base_url: base_url.trim_end_matches('/').to_string(),
                    token,
                });
            }

            // Not found — suggest the closest registered name (same fuzzy
            // matching used for "unknown tool, did you mean" suggestions),
            // since the LLM is matching a conversational phrase against
            // whatever name was typed into the Devices page.
            let mut names_stmt = conn.prepare("SELECT name FROM devices ORDER BY name")?;
            let names: Vec<String> = names_stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect();
            if !names.is_empty() {
                let target = name.to_ascii_lowercase();
                let mut closest = names.clone();
                closest.sort_by_key(|n| levenshtein(&target, &n.to_ascii_lowercase()));
                anyhow::bail!(
                    "Device '{}' not found. Did you mean '{}'? Use action=list_devices to see all registered devices.",
                    name,
                    closest[0]
                );
            }
        }
        anyhow::bail!(
            "Device '{}' not found. Use action=list_devices to see registered companion devices — add one on the Devices dashboard page first.",
            name
        )
    }

    fn bearer_auth(token: &Option<String>) -> Option<HttpAuth> {
        token.clone().map(|t| HttpAuth {
            auth_type: "bearerAuth".to_string(),
            user: None,
            password: None,
            header_name: None,
            header_value: Some(t),
        })
    }

    pub async fn list_devices(state: crate::state::AppState) -> Result<Value> {
        let conn = state.db.get().context("DB error")?;
        let mut s = conn.prepare("SELECT name, kind, base_url FROM devices ORDER BY name")?;
        let mut rows = s.query([])?;
        let mut lines = Vec::new();
        while let Some(r) = rows.next()? {
            let (name, kind, base_url): (String, String, String) =
                (r.get(0)?, r.get(1)?, r.get(2)?);
            lines.push(format!("- {} ({}): {}", name, kind, base_url));
        }
        if lines.is_empty() {
            return Ok(json!({
                "output": "No companion devices are registered. Add one on the Devices dashboard page, then call action=list_tools with its name to see what it can do."
            }));
        }
        Ok(json!({ "output": lines.join("\n") }))
    }

    pub async fn list_device_tools(name: &str, state: crate::state::AppState) -> Result<Value> {
        let d = Self::get_device_details(&state, name).await?;
        let params = HttpRequestParams {
            method: "GET".into(),
            url: format!("{}/agent/tools", d.base_url),
            auth: Self::bearer_auth(&d.token),
            timeout_seconds: Some(15),
            ..Default::default()
        };
        let resp = HttpRequestTool::new()
            .request(params)
            .await
            .with_context(|| format!("Failed to reach device '{}' at /agent/tools", name))?;
        Ok(serde_json::to_value(resp)?)
    }

    pub async fn call(
        name: &str,
        tool: &str,
        params: Value,
        timeout_seconds: u64,
        state: crate::state::AppState,
    ) -> Result<Value> {
        if tool.is_empty() {
            anyhow::bail!(
                "tool is required for action=call (see action=list_tools for valid names)"
            );
        }
        let d = Self::get_device_details(&state, name).await?;
        let body = json!({ "tool": tool, "params": params });
        let req = HttpRequestParams {
            method: "POST".into(),
            url: format!("{}/agent/tool", d.base_url),
            body: Some(body),
            auth: Self::bearer_auth(&d.token),
            timeout_seconds: Some(timeout_seconds),
            ..Default::default()
        };
        let resp = HttpRequestTool::new()
            .request(req)
            .await
            .with_context(|| format!("Device '{}' call to tool '{}' failed", name, tool))?;
        Ok(serde_json::to_value(resp)?)
    }

    /// Dashboard-only. Two steps because the device's own auth design splits
    /// them: /ping needs no token (confirms the tunnel/base_url is reachable
    /// at all), /status requires the bearer token (confirms the stored token
    /// is actually correct) and returns the device's rich status payload.
    ///
    /// Called both from the dashboard's manual "Test connection" button and
    /// from the Devices page's periodic background poll, so a connectivity
    /// change is reported once — on the connected<->disconnected transition —
    /// regardless of which caller happened to observe it first.
    pub async fn test_connection(name: &str, state: crate::state::AppState) -> Result<Value> {
        let d = Self::get_device_details(&state, name).await?;

        let ping = HttpRequestTool::new()
            .request(HttpRequestParams {
                method: "GET".into(),
                url: format!("{}/ping", d.base_url),
                timeout_seconds: Some(8),
                ..Default::default()
            })
            .await;

        let status = HttpRequestTool::new()
            .request(HttpRequestParams {
                method: "GET".into(),
                url: format!("{}/status", d.base_url),
                auth: Self::bearer_auth(&d.token),
                timeout_seconds: Some(8),
                ..Default::default()
            })
            .await;

        let result = match (&ping, &status) {
            (Ok(p), Ok(s)) if s.status < 300 => {
                json!({"ok": true, "ping": p.body, "status": s.body})
            }
            (Ok(_), Ok(s)) => json!({
                "ok": false,
                "error": format!("Device reachable but /status returned HTTP {} — check the bearer token", s.status),
                "status_body": s.body
            }),
            (Err(e), _) => json!({
                "ok": false,
                "error": format!("Device unreachable at {}: {}", d.base_url, e)
            }),
            (Ok(_), Err(e)) => json!({
                "ok": false,
                "error": format!("Reachable (ping ok) but /status failed: {}", e)
            }),
        };

        Self::notify_on_transition(name, &result, &state).await;
        Ok(result)
    }

    /// Emits to the dashboard bell + configured messaging gateway only when
    /// this check's ok/fail outcome differs from the last one recorded for
    /// `name` — a device that's been offline for hours shouldn't re-notify on
    /// every poll tick, but a fresh disconnect (or recovery) should surface.
    async fn notify_on_transition(name: &str, result: &Value, state: &crate::state::AppState) {
        let ok = result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);

        let was_ok = {
            let mut last = state.device_last_ok.lock().await;
            let previous = last.get(name).copied().unwrap_or(true);
            last.insert(name.to_string(), ok);
            previous
        };

        if was_ok == ok {
            return;
        }

        if ok {
            state
                .notify
                .emit(
                    "devices",
                    "info",
                    &format!("Device '{}' reconnected", name),
                    &format!("'{}' is reachable again.", name),
                )
                .await;
        } else {
            let error = result
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("Connection check failed");
            if let Err(e) = crate::error_reporting::send_global_error_notification(
                state,
                "devices",
                &format!("Device '{}' disconnected", name),
                error,
                None,
                None,
            )
            .await
            {
                tracing::warn!("Device disconnect notification failed: {}", e);
            }
        }
    }
}
