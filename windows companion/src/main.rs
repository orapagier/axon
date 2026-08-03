// No console window in release builds — runs as a silent background process
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod auth;
mod config;
mod indexer;
mod routes;
mod server;
mod tunnel;

use std::sync::Arc;
use tokio::sync::watch;
use tracing::{error, info, warn};

#[tokio::main(worker_threads = 2)]
async fn main() {
    // Init logging — writes to stdout in debug; silent in release (no console)
    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_ids(false)
        .compact()
        .init();

    info!("Windows API starting (headless)...");

    // Self-install to a persistent location and register for autostart
    self_install();

    // Load config — log and exit on failure
    let config = Arc::new(match config::Config::load() {
        Ok(c) => c,
        Err(e) => {
            error!("Config error: {}", e);
            log_error_to_file(&format!("Config error: {}", e));
            std::process::exit(1);
        }
    });

    info!("Config loaded — port {}", config.port);

    // Shutdown signal
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let token = config.tunnel_token.clone();
    let config_server = config.clone();

    let tunnel_rx  = shutdown_rx.clone();
    let server_rx  = shutdown_rx.clone();
    let indexer_rx = shutdown_rx;

    // Listen for Ctrl+C (or service stop signal) to trigger graceful shutdown
    let shutdown_tx_signal = shutdown_tx.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        info!("Shutdown signal received — stopping services...");
        let _ = shutdown_tx_signal.send(true);
    });

    // Run tunnel and server concurrently
    info!("Starting background services...");
    tokio::join!(
        tunnel::start(token, tunnel_rx),
        server::start(config_server, server_rx),
        indexer::start(indexer_rx),
    );

    info!("All services stopped. Exiting.");
}

/// Install the app to %LOCALAPPDATA%\WindowsAPI\ and register it in startup.
///
/// - Copies the exe (and config.toml if present) to a persistent location
/// - Registers the installed copy in HKCU\...\Run so it survives reboots
/// - If already running from the install dir, just ensures the registry key is set
/// - Updates the installed exe if the running binary is newer/different
fn self_install() {
    #[cfg(windows)]
    {
        use std::path::PathBuf;

        let current_exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                warn!("Could not determine exe path: {}", e);
                return;
            }
        };

        // Install directory: %LOCALAPPDATA%\WindowsAPI
        let install_dir = match std::env::var("LOCALAPPDATA") {
            Ok(dir) => PathBuf::from(dir).join("WindowsAPI"),
            Err(_) => {
                warn!("LOCALAPPDATA not set, skipping self-install");
                return;
            }
        };

        let installed_exe = install_dir.join("windowsapi.exe");
        let installed_config = install_dir.join("config.toml");

        // Create install directory if it doesn't exist
        if let Err(e) = std::fs::create_dir_all(&install_dir) {
            warn!("Failed to create install dir: {}", e);
            return;
        }

        let running_from_install_dir = current_exe == installed_exe;

        if !running_from_install_dir {
            // Copy the exe to the install location (update if size/content changed)
            let should_copy = if installed_exe.exists() {
                // Update if file size differs (new build)
                match (
                    std::fs::metadata(&current_exe),
                    std::fs::metadata(&installed_exe),
                ) {
                    (Ok(src), Ok(dst)) => src.len() != dst.len(),
                    _ => true,
                }
            } else {
                true
            };

            if should_copy {
                match std::fs::copy(&current_exe, &installed_exe) {
                    Ok(_) => info!("Installed exe to {}", installed_exe.display()),
                    Err(e) => {
                        // The installed copy might be running — that's fine, it's already there
                        warn!("Could not update installed exe (may be in use): {}", e);
                    }
                }
            }

            // Copy config.toml if it exists next to the source exe and not yet in install dir
            let source_dir = current_exe.parent().unwrap_or(std::path::Path::new("."));
            let source_config = source_dir.join("config.toml");
            if source_config.exists() {
                // Always update config so changes are picked up
                match std::fs::copy(&source_config, &installed_config) {
                    Ok(_) => info!("Copied config.toml to {}", install_dir.display()),
                    Err(e) => warn!("Could not copy config.toml: {}", e),
                }
            }
        }

        let install_path_str = installed_exe.to_string_lossy().to_string();
        let install_dir_str = install_dir.to_string_lossy().to_string();

        // --- Register in startup (HKCU\...\Run) ---
        reg_set(
            r"HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Run",
            "WindowsAPI",
            "REG_SZ",
            &install_path_str,
        );

        // --- Register in Installed Apps (HKCU\...\Uninstall\WindowsAPI) ---
        let uninstall_key = r"HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\WindowsAPI";
        let uninstall_cmd = format!(
            "cmd /c taskkill /im windowsapi.exe /f & reg delete \"HKCU\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run\" /v WindowsAPI /f & reg delete \"HKCU\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\WindowsAPI\" /f & rmdir /s /q \"{}\"",
            install_dir_str
        );

        reg_set(uninstall_key, "DisplayName", "REG_SZ", "Windows API");
        reg_set(
            uninstall_key,
            "DisplayVersion",
            "REG_SZ",
            env!("CARGO_PKG_VERSION"),
        );
        reg_set(uninstall_key, "Publisher", "REG_SZ", "WindowsAPI");
        reg_set(uninstall_key, "InstallLocation", "REG_SZ", &install_dir_str);
        reg_set(uninstall_key, "DisplayIcon", "REG_SZ", &install_path_str);
        reg_set(uninstall_key, "UninstallString", "REG_SZ", &uninstall_cmd);
        reg_set(uninstall_key, "NoModify", "REG_DWORD", "1");
        reg_set(uninstall_key, "NoRepair", "REG_DWORD", "1");

        info!("App registered in Installed Apps");
    }
}

/// Helper: set a single registry value quietly
#[cfg(windows)]
fn reg_set(key: &str, name: &str, reg_type: &str, value: &str) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("reg")
        .args(["add", key, "/v", name, "/t", reg_type, "/d", value, "/f"])
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output();
}

/// Write error to a log file next to the executable (useful when there's no console)
fn log_error_to_file(msg: &str) {
    if let Ok(exe) = std::env::current_exe() {
        let log_path = exe.with_file_name("windowsapi_error.log");
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let entry = format!("[{}] {}\n", timestamp, msg);
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, entry.as_bytes()));
    }
}
