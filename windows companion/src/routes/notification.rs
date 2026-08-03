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
    // Pass user values via environment variables to prevent PowerShell injection.
    // $env:VAR in PowerShell reads the variable as a plain string — no recursive
    // expression parsing, so $(malicious-command) in the value is harmless.
    let script = r#"
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType=WindowsRuntime] | Out-Null
[Windows.UI.Notifications.ToastNotification, Windows.UI.Notifications, ContentType=WindowsRuntime] | Out-Null
[Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType=WindowsRuntime] | Out-Null

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

    tokio::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", script])
        .env("TOAST_TITLE", &req.title)
        .env("TOAST_BODY", &req.body)
        .env("TOAST_APPID", &req.app_id)
        .creation_flags(0x08000000)
        .spawn()
        .map_err(|e| AppError::internal(e.to_string()))?;

    Ok(ActionResponse::ok())
}
