use axum::Json;
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use sysinfo::{Pid, System};
use tracing::warn;

use crate::routes::{ActionResponse, AppError};

// ──────────────────────────────────────────────────────────────────────────────
// System Info
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct SystemInfo {
    pub os: String,
    pub os_version: String,
    pub hostname: String,
    pub cpu_name: String,
    pub cpu_cores: usize,
    pub cpu_usage_percent: f32,
    pub total_memory_mb: u64,
    pub used_memory_mb: u64,
    pub total_swap_mb: u64,
    pub used_swap_mb: u64,
    pub uptime_secs: u64,
    pub username: String,
}

pub async fn system_info() -> Json<SystemInfo> {
    tokio::task::spawn_blocking(|| {
        let mut sys = System::new_all();
        sys.refresh_all();

        let cpu = sys
            .cpus()
            .first()
            .map(|c| c.brand().to_string())
            .unwrap_or_default();

        Json(SystemInfo {
            os: System::name().unwrap_or_default(),
            os_version: System::os_version().unwrap_or_default(),
            hostname: System::host_name().unwrap_or_default(),
            cpu_name: cpu,
            cpu_cores: sys.cpus().len(),
            cpu_usage_percent: sys.global_cpu_usage(),
            total_memory_mb: sys.total_memory() / 1024 / 1024,
            used_memory_mb: sys.used_memory() / 1024 / 1024,
            total_swap_mb: sys.total_swap() / 1024 / 1024,
            used_swap_mb: sys.used_swap() / 1024 / 1024,
            uptime_secs: System::uptime(),
            username: {
                let users = sysinfo::Users::new_with_refreshed_list();
                users
                    .iter()
                    .next()
                    .map(|u| u.name().to_string())
                    .unwrap_or_default()
            },
        })
    })
    .await
    .unwrap_or_else(|_| {
        Json(SystemInfo {
            os: String::new(),
            os_version: String::new(),
            hostname: String::new(),
            cpu_name: String::new(),
            cpu_cores: 0,
            cpu_usage_percent: 0.0,
            total_memory_mb: 0,
            used_memory_mb: 0,
            total_swap_mb: 0,
            used_swap_mb: 0,
            uptime_secs: 0,
            username: String::new(),
        })
    })
}

// ──────────────────────────────────────────────────────────────────────────────
// Processes
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub cpu_usage: f32,
    pub memory_mb: u64,
    pub status: String,
}

pub async fn list_processes() -> Json<Vec<ProcessInfo>> {
    tokio::task::spawn_blocking(|| {
        let mut sys = System::new_all();
        sys.refresh_all();

        let mut procs: Vec<ProcessInfo> = sys
            .processes()
            .values()
            .map(|p| ProcessInfo {
                pid: p.pid().as_u32(),
                name: p.name().to_string_lossy().to_string(),
                cpu_usage: p.cpu_usage(),
                memory_mb: p.memory() / 1024 / 1024,
                status: format!("{:?}", p.status()),
            })
            .collect();

        procs.sort_by(|a, b| {
            b.cpu_usage
                .partial_cmp(&a.cpu_usage)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Json(procs)
    })
    .await
    .unwrap_or(Json(vec![]))
}

#[derive(Deserialize)]
pub struct KillRequest {
    pub pid: Option<u32>,
    pub name: Option<String>,
}

pub async fn kill_process(Json(req): Json<KillRequest>) -> Result<Json<ActionResponse>, AppError> {
    tokio::task::spawn_blocking(move || {
        let mut sys = System::new_all();
        sys.refresh_all();

        if let Some(pid) = req.pid {
            let p = sys
                .process(Pid::from_u32(pid))
                .ok_or_else(|| AppError::not_found(format!("PID {} not found", pid)))?;
            p.kill();
            return Ok(ActionResponse::with_message(format!("Killed PID {}", pid)));
        }

        if let Some(ref name) = req.name {
            // sysinfo's processes_by_name is a SUBSTRING match, so asking to
            // kill "chrome" would also take out "chromedriver" and anything
            // else containing the word. Filter down to an exact
            // (case-insensitive) name match before killing anything.
            let wanted = name.to_lowercase();
            let count = sys
                .processes_by_name(OsStr::new(name))
                .filter(|p| p.name().to_string_lossy().to_lowercase() == wanted)
                .map(|p| {
                    p.kill();
                    1u32
                })
                .sum::<u32>();

            if count == 0 {
                return Err(AppError::not_found(format!(
                    "No process named exactly '{}' is running",
                    name
                )));
            }

            return Ok(ActionResponse::with_message(format!(
                "Killed {} process(es) named '{}'",
                count, name
            )));
        }

        Err(AppError::bad_request("Provide pid or name"))
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))?
}

// ──────────────────────────────────────────────────────────────────────────────
// Power Actions
// ──────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct PowerRequest {
    /// "shutdown", "restart", "sleep", "hibernate", "lock", "logoff"
    pub action: String,
    /// Delay in seconds before action (default 0)
    #[serde(default)]
    pub delay_secs: u32,
}

pub async fn power_action(Json(req): Json<PowerRequest>) -> Result<Json<ActionResponse>, AppError> {
    let cmd = match req.action.as_str() {
        "shutdown" => format!("shutdown /s /t {}", req.delay_secs),
        "restart" => format!("shutdown /r /t {}", req.delay_secs),
        "logoff" => "shutdown /l".to_string(),
        "hibernate" => "shutdown /h".to_string(),
        "sleep" => "rundll32.exe powrprof.dll,SetSuspendState 0,1,0".to_string(),
        "lock" => "rundll32.exe user32.dll,LockWorkStation".to_string(),
        "cancel_shutdown" => "shutdown /a".to_string(),
        _ => {
            return Err(AppError::bad_request(format!(
                "Unknown action: {}",
                req.action
            )))
        }
    };

    tokio::process::Command::new("cmd")
        .args(["/C", &cmd])
        .creation_flags(0x08000000)
        .spawn()
        .map_err(|e| AppError::internal(e.to_string()))?;

    Ok(ActionResponse::with_message(format!(
        "Executed: {}",
        req.action
    )))
}

// ──────────────────────────────────────────────────────────────────────────────
// Environment Variables
// ──────────────────────────────────────────────────────────────────────────────

use std::collections::HashMap;

pub async fn list_env() -> Json<HashMap<String, String>> {
    Json(std::env::vars().collect())
}

#[derive(Deserialize)]
pub struct SetEnvRequest {
    pub key: String,
    pub value: String,
}

pub async fn set_env(Json(req): Json<SetEnvRequest>) -> Json<ActionResponse> {
    // NOTE: std::env::set_var is technically unsound in multi-threaded programs
    // (see rust-lang/rust#27970). For this single-user automation tool the risk
    // is negligible, but be aware if porting to a library context.
    #[allow(unused_unsafe)]
    unsafe {
        std::env::set_var(&req.key, &req.value);
    }
    warn!(key = %req.key, "Environment variable set (process-scoped only)");
    ActionResponse::ok()
}
