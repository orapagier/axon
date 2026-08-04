use axum::Json;
use serde::Deserialize;

use crate::routes::{ActionResponse, AppError};

#[derive(Deserialize)]
pub struct NotificationRequest {
    pub title: String,
    pub body: String,
    /// Duration in seconds (default 5)
    #[allow(dead_code)]
    #[serde(default = "five")]
    pub duration_secs: u32,
    /// App ID shown in Action Center (default "Win Automation API")
    #[serde(default = "default_app")]
    pub app_id: String,
}
fn five() -> u32 {
    5
}
fn default_app() -> String {
    "Win Automation API".to_string()
}

pub async fn send_notification(
    Json(req): Json<NotificationRequest>,
) -> Result<Json<ActionResponse>, AppError> {
    // The toast payload is XML, so title/body must be XML-escaped before they
    // are substituted in. Passing them through PowerShell environment variables
    // stops *command* injection, but an unescaped "&" or "<" still makes
    // LoadXml throw and the notification silently never appears.
    //
    // We escape here rather than in PowerShell so the escaping is testable.
    let script = r#"
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType=WindowsRuntime] | Out-Null
[Windows.UI.Notifications.ToastNotification, Windows.UI.Notifications, ContentType=WindowsRuntime] | Out-Null
[Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType=WindowsRuntime] | Out-Null

$ErrorActionPreference = 'Stop'

$template = @"
<toast duration="short">
  <visual>
    <binding template="ToastGeneric">
      <text>$env:TOAST_TITLE</text>
      <text>$env:TOAST_BODY</text>
    </binding>
  </visual>
</toast>
"@

$xml = New-Object Windows.Data.Xml.Dom.XmlDocument
$xml.LoadXml($template)
$toast = New-Object Windows.UI.Notifications.ToastNotification $xml
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($env:TOAST_APPID).Show($toast)
"#;

    // Await the child instead of firing and forgetting. Previously this used
    // .spawn() and dropped the handle, so /notify returned success even when
    // the toast failed outright.
    let output = tokio::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("TOAST_TITLE", xml_escape(&req.title))
        .env("TOAST_BODY", xml_escape(&req.body))
        .env("TOAST_APPID", &req.app_id)
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output()
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::internal(format!(
            "Toast notification failed: {}",
            err.trim()
        )));
    }

    Ok(ActionResponse::ok())
}

/// Escape the five XML predefined entities so arbitrary user text is safe to
/// embed as element content.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_xml_metacharacters() {
        assert_eq!(xml_escape("Tom & Jerry"), "Tom &amp; Jerry");
        assert_eq!(xml_escape("<b>hi</b>"), "&lt;b&gt;hi&lt;/b&gt;");
        assert_eq!(xml_escape(r#"say "hi""#), "say &quot;hi&quot;");
    }

    #[test]
    fn closing_the_toast_tag_is_neutralised() {
        // Without escaping this would terminate the <text> element and inject
        // arbitrary toast markup.
        let injected = xml_escape("</text><text>injected");
        assert!(!injected.contains('<'));
        assert!(!injected.contains('>'));
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        assert_eq!(xml_escape("Workflow completed"), "Workflow completed");
        assert_eq!(xml_escape("émoji ✅"), "émoji ✅");
    }
}
