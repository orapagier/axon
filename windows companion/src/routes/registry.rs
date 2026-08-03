use axum::Json;
use serde::{Deserialize, Serialize};

use crate::routes::{ActionResponse, AppError};

#[derive(Deserialize)]
pub struct RegistryReadRequest {
    /// e.g. "HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows NT\CurrentVersion"
    pub key: String,
    /// Value name (empty string = default value)
    #[serde(default)]
    pub name: String,
}

#[derive(Serialize)]
pub struct RegistryReadResponse {
    pub key: String,
    pub name: String,
    pub value: String,
    pub value_type: String,
}

#[derive(Deserialize)]
pub struct RegistryWriteRequest {
    pub key: String,
    pub name: String,
    pub value: String,
    /// "string" (default), "dword", "qword", "expand_string"
    #[serde(default = "string_type")]
    pub value_type: String,
}

fn string_type() -> String { "string".to_string() }

/// Read a registry value using PowerShell.
/// User-supplied values are passed via environment variables to prevent injection.
pub async fn read_key(
    Json(req): Json<RegistryReadRequest>,
) -> Result<Json<RegistryReadResponse>, AppError> {
    let script = if req.name.is_empty() {
        r#"$val = Get-ItemProperty -Path "Registry::$env:REG_KEY" -ErrorAction Stop; $val.'(default)'"#.to_string()
    } else {
        r#"$val = Get-ItemPropertyValue -Path "Registry::$env:REG_KEY" -Name $env:REG_NAME -ErrorAction Stop; $val"#.to_string()
    };

    let output = tokio::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .env("REG_KEY", &req.key)
        .env("REG_NAME", &req.name)
        .output()
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::not_found(err.trim().to_string()));
    }

    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(Json(RegistryReadResponse {
        key: req.key,
        name: req.name,
        value,
        value_type: "string".to_string(),
    }))
}

/// Write a registry value using PowerShell.
/// User-supplied values are passed via environment variables to prevent injection.
pub async fn write_key(
    Json(req): Json<RegistryWriteRequest>,
) -> Result<Json<ActionResponse>, AppError> {
    let ps_type = match req.value_type.as_str() {
        "dword" => "DWord",
        "qword" => "QWord",
        "expand_string" => "ExpandString",
        "binary" => "Binary",
        "multi_string" => "MultiString",
        _ => "String",
    };

    // Only the validated ps_type is interpolated; all user values come from env vars
    let script = format!(
        r#"
$path = "Registry::$env:REG_KEY"
if (-not (Test-Path $path)) {{
    New-Item -Path $path -Force | Out-Null
}}
Set-ItemProperty -Path $path -Name $env:REG_NAME -Value $env:REG_VALUE -Type {ps_type} -Force
"#,
        ps_type = ps_type
    );

    let output = tokio::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .env("REG_KEY", &req.key)
        .env("REG_NAME", &req.name)
        .env("REG_VALUE", &req.value)
        .output()
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::internal(err.trim().to_string()));
    }

    Ok(ActionResponse::ok())
}
