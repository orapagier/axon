//! Plane A — the Windows service.
//!
//! Running as a service under LocalSystem is what makes the machine reachable
//! when it is locked or sitting at the login screen. The previous `HKCU\...\Run`
//! model tied the whole process to an interactive logon: log out and the tunnel
//! itself went down, not just the desktop routes.
//!
//! This module owns the SCM plumbing — registration, the control handler, and
//! install/uninstall. The actual work is `crate::run_plane_a`.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tracing::{error, info, warn};

pub const SERVICE_NAME: &str = "WindowsAPI";
pub const SERVICE_DISPLAY_NAME: &str = "Windows Automation API";
pub const SERVICE_DESCRIPTION: &str =
    "Remote automation API with an embedded Cloudflare Tunnel. Runs as LocalSystem so \
     shell, file and system routes stay available while the machine is locked.";

/// Where an installed service lives. Not %LOCALAPPDATA% — that resolves to
/// SYSTEM's profile when the service runs, and would be invisible to the user
/// editing config.toml.
pub fn install_dir() -> PathBuf {
    let base = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".to_string());
    PathBuf::from(base).join("WindowsAPI")
}

pub fn installed_exe() -> PathBuf {
    install_dir().join("windowsapi.exe")
}

// ──────────────────────────────────────────────────────────────────────────────
// Service entry point
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(windows)]
mod imp {
    use super::*;
    use windows_service::{
        define_windows_service,
        service::{
            ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceInfo,
            ServiceStartType, ServiceState, ServiceStatus, ServiceType,
        },
        service_control_handler::{self, ServiceControlHandlerResult},
        service_dispatcher,
        service_manager::{ServiceManager, ServiceManagerAccess},
    };

    define_windows_service!(ffi_service_main, service_main);

    /// Hands control to the SCM. Only valid when the process was actually
    /// launched as a service; from a console this fails with error 1063.
    pub fn run() -> anyhow::Result<()> {
        service_dispatcher::start(SERVICE_NAME, ffi_service_main)
            .map_err(|e| anyhow::anyhow!("service_dispatcher::start failed: {}", e))
    }

    fn service_main(_args: Vec<OsString>) {
        if let Err(e) = run_service() {
            let msg = format!("Service failed: {e}");
            error!("{}", msg);
            crate::log_error_to_file(&msg);
        }
    }

    /// Reports a definitive Stopped-with-error to the SCM.
    ///
    /// Returning from `service_main` without ever setting a status leaves the
    /// service in StartPending until the SCM times it out — 30 seconds of
    /// "Starting" instead of an immediate, accurate failure. A bad config is
    /// the most likely first-run outcome, so it has to fail loudly and fast.
    fn report_failed_start(reason: &str) {
        let msg = format!("Service cannot start: {reason}");
        error!("{}", msg);
        crate::log_error_to_file(&msg);

        if let Ok(handle) =
            service_control_handler::register(SERVICE_NAME, |_| ServiceControlHandlerResult::NoError)
        {
            let _ = handle.set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: ServiceState::Stopped,
                controls_accepted: ServiceControlAccept::empty(),
                // ERROR_BAD_CONFIGURATION — shows up in `sc query` and the
                // Event Log rather than a generic timeout.
                exit_code: windows_service::service::ServiceExitCode::Win32(1610),
                checkpoint: 0,
                wait_hint: Duration::default(),
                process_id: None,
            });
        }
    }

    fn run_service() -> anyhow::Result<()> {
        // Load config before reporting Running — a bad config should surface as
        // a failed start in the SCM, not a service that is up but broken.
        let config = match crate::config::Config::load() {
            Ok(c) => Arc::new(c),
            Err(e) => {
                report_failed_start(&e.to_string());
                return Err(e);
            }
        };

        let agent = Arc::new(crate::session::SessionAgent::new(
            config.session_port(),
            crate::server::loopback_token(),
            config.public_url.clone(),
        ));

        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let handler_shutdown = shutdown_tx.clone();
        let handler_agent = agent.clone();

        let status_handle = service_control_handler::register(SERVICE_NAME, move |control| {
            match control {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    info!("SCM requested stop");
                    let _ = handler_shutdown.send(true);
                    ServiceControlHandlerResult::NoError
                }
                // The whole reason this service accepts SESSION_CHANGE: logon,
                // unlock and console-connect are exactly when a desktop agent
                // becomes launchable (or needs to follow the console to another
                // user). Without this we would wait out the poll interval.
                ServiceControl::SessionChange(param) => {
                    info!("Session change: {:?} (session {})", param.reason, param.notification.session_id);
                    handler_agent.kick();
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        })?;

        let running = ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP
                | ServiceControlAccept::SHUTDOWN
                | ServiceControlAccept::SESSION_CHANGE,
            exit_code: windows_service::service::ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        };
        status_handle.set_service_status(running)?;
        info!("Service reported Running");

        // A dedicated runtime rather than #[tokio::main]: service_main runs on
        // an SCM-owned thread, so we build and drive the runtime ourselves.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;

        rt.block_on(crate::run_plane_a(config, agent, shutdown_rx));

        let stopped = ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: windows_service::service::ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        };
        status_handle.set_service_status(stopped)?;
        info!("Service reported Stopped");
        Ok(())
    }

    // ──────────────────────────────────────────────────────────────────────────
    // Install / uninstall
    // ──────────────────────────────────────────────────────────────────────────

    pub fn install() -> anyhow::Result<()> {
        let current = std::env::current_exe()?;
        let dir = install_dir();
        let target = installed_exe();

        std::fs::create_dir_all(&dir)?;

        // Copy the binary unless we are already running from the install path.
        if current != target {
            // A running service holds a lock on its image, so stop first.
            let _ = stop_if_running();
            std::fs::copy(&current, &target).map_err(|e| {
                anyhow::anyhow!(
                    "Could not copy {} to {}: {}. Stop the service first if it is running.",
                    current.display(),
                    target.display(),
                    e
                )
            })?;
            info!("Installed binary to {}", target.display());

            // A config.toml sitting next to the binary being installed is taken
            // as deliberate and wins, including over an existing install — that
            // is the whole "drop the exe and your config in a folder, run it"
            // flow. Only when there is no source config do we fall back to
            // seeding a placeholder, and that never overwrites a live install.
            let src_cfg = current.with_file_name("config.toml");
            let dst_cfg = dir.join("config.toml");
            if src_cfg.exists() {
                std::fs::copy(&src_cfg, &dst_cfg)?;
                info!("Installed config.toml from {}", src_cfg.display());
            } else if !dst_cfg.exists() {
                std::fs::write(&dst_cfg, include_str!("../config.example.toml"))?;
                info!("Wrote starter config to {}", dst_cfg.display());
            }
        }

        lock_down_install_dir(&dir);

        let manager =
            ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CREATE_SERVICE)?;

        let info = ServiceInfo {
            name: OsString::from(SERVICE_NAME),
            display_name: OsString::from(SERVICE_DISPLAY_NAME),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: target.clone(),
            launch_arguments: vec![OsString::from("--service")],
            dependencies: vec![],
            // LocalSystem holds SeTcbPrivilege, which WTSQueryUserToken requires
            // to hand us the interactive user's token in session.rs.
            account_name: None,
            account_password: None,
        };

        // START is not for starting the service — `start()` opens its own
        // handle. It is there because a failure action of SC_ACTION_RESTART
        // means the SCM will start the service on our behalf, and
        // ChangeServiceConfig2 refuses to configure that from a handle that
        // could not have started it itself. Without START, setting recovery
        // actions fails with access denied.
        let access = ServiceAccess::CHANGE_CONFIG | ServiceAccess::START;

        let service = match manager.create_service(&info, access) {
            Ok(s) => s,
            Err(e) => {
                // Already installed — update the existing registration instead
                // so a reinstall picks up a moved binary or changed arguments.
                warn!("create_service failed ({}), trying to update existing", e);
                let existing = manager
                    .open_service(SERVICE_NAME, access | ServiceAccess::QUERY_CONFIG)?;
                existing.change_config(&info)?;
                existing
            }
        };

        service.set_description(SERVICE_DESCRIPTION)?;
        set_recovery_actions(&service);

        info!("Service '{}' installed", SERVICE_NAME);
        Ok(())
    }

    /// Tells the SCM to restart us if we ever die.
    ///
    /// Without this a crash is permanent: the SCM's default for a failed
    /// service is to take no action at all, so the machine stays unreachable
    /// until somebody reboots it or starts the service by hand — which, for a
    /// box whose whole purpose is remote access, means it is gone until
    /// somebody walks over to it.
    ///
    /// `on_non_crash_failures` matters as much as the delays. Exiting with a
    /// non-zero code is not a "crash" by the SCM's reckoning, and a config or
    /// bind failure exits cleanly, so without it the most likely failures are
    /// exactly the ones that would not be retried.
    ///
    /// Best-effort: a service that is installed but has no recovery policy is
    /// still a working service, so a failure here is logged, not fatal.
    fn set_recovery_actions(service: &windows_service::service::Service) {
        use windows_service::service::{ServiceAction, ServiceActionType, ServiceFailureActions};

        let restart = |secs| ServiceAction {
            action_type: ServiceActionType::Restart,
            delay: Duration::from_secs(secs),
        };

        let actions = ServiceFailureActions {
            // Forget the failure count after a day without incident, so a
            // once-a-month blip never lands on the long delay.
            reset_period: windows_service::service::ServiceFailureResetPeriod::After(
                Duration::from_secs(86_400),
            ),
            reboot_msg: None,
            command: None,
            // Quick, then quick, then back off — a transient port conflict
            // clears in seconds, and anything still failing after two tries is
            // waiting on something slower.
            actions: Some(vec![restart(5), restart(15), restart(60)]),
        };

        // {:?} rather than {}: the crate's Display for a winapi failure is the
        // bare string "IO error in winapi call", which says nothing. The Debug
        // form carries the actual OS error code.
        if let Err(e) = service.update_failure_actions(actions) {
            warn!("Could not set service recovery actions: {:?}", e);
            return;
        }
        if let Err(e) = service.set_failure_actions_on_non_crash_failures(true) {
            warn!("Could not enable recovery on non-crash failures: {:?}", e);
            return;
        }
        info!("Service recovery configured — restart after 5s, 15s, then 60s");
    }

    pub fn uninstall() -> anyhow::Result<()> {
        let _ = stop_if_running();

        let manager =
            ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
        let service = manager.open_service(SERVICE_NAME, ServiceAccess::DELETE)?;
        service.delete()?;
        info!("Service '{}' deleted", SERVICE_NAME);
        Ok(())
    }

    pub fn start() -> anyhow::Result<()> {
        let manager =
            ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
        let service = manager.open_service(SERVICE_NAME, ServiceAccess::START)?;
        service.start(&[] as &[&std::ffi::OsStr])?;
        info!("Service '{}' started", SERVICE_NAME);
        Ok(())
    }

    /// Polls the service until it settles into Running or Stopped.
    ///
    /// `start()` returning Ok only means the SCM accepted the request. A service
    /// that starts and then dies on a bad config still looks like a successful
    /// start from there, which is how "installed and started" ends up on screen
    /// for a service that is not running. This is the authoritative check.
    pub fn wait_until_settled(timeout: Duration) -> anyhow::Result<String> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
        let service = manager.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS)?;

        let deadline = std::time::Instant::now() + timeout;
        loop {
            let state = service.query_status()?.current_state;
            match state {
                ServiceState::Running => return Ok("Running".into()),
                ServiceState::Stopped => return Ok("Stopped".into()),
                other => {
                    if std::time::Instant::now() >= deadline {
                        return Ok(format!("{other:?}"));
                    }
                    std::thread::sleep(Duration::from_millis(300));
                }
            }
        }
    }

    pub fn stop_if_running() -> anyhow::Result<()> {
        let manager =
            ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
        let service = manager
            .open_service(SERVICE_NAME, ServiceAccess::STOP | ServiceAccess::QUERY_STATUS)?;
        let _ = service.stop();
        // Give the SCM a moment to release the image lock before any copy.
        std::thread::sleep(Duration::from_millis(1500));
        Ok(())
    }
}

#[cfg(windows)]
pub use imp::{install, run, start, stop_if_running, uninstall, wait_until_settled};

/// Last line of the on-disk error log, used to explain a service that installed
/// but would not stay running. There is no console, so this is where the real
/// reason lives.
pub fn last_error_line() -> Option<String> {
    let log = install_dir().join("windowsapi_error.log");
    let content = std::fs::read_to_string(log).ok()?;
    let line = content.lines().rev().find(|l| !l.trim().is_empty())?;
    // Strip the "[unix_timestamp] " prefix that log_error_to_file adds.
    Some(
        line.split_once("] ")
            .map(|(_, rest)| rest)
            .unwrap_or(line)
            .to_string(),
    )
}

/// Restricts the install directory to SYSTEM and Administrators.
///
/// config.toml holds `api_secret`, and the directory is where cloudflared.exe
/// gets extracted — a SYSTEM service executing a binary from a path a standard
/// user can write to is a straightforward privilege escalation.
fn lock_down_install_dir(dir: &std::path::Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let path = dir.to_string_lossy().to_string();

        // /inheritance:r drops inherited ACEs, which is what would otherwise
        // leave Users with write access inherited from C:\ProgramData.
        let out = std::process::Command::new("icacls")
            .args([
                &path,
                "/inheritance:r",
                "/grant:r",
                "*S-1-5-18:(OI)(CI)F", // LocalSystem
                "/grant:r",
                "*S-1-5-32-544:(OI)(CI)F", // Administrators
            ])
            .creation_flags(0x08000000)
            .output();

        match out {
            Ok(o) if o.status.success() => {
                info!("Locked down {} to SYSTEM and Administrators", path)
            }
            Ok(o) => warn!(
                "icacls on {} returned {}: {}",
                path,
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            ),
            Err(e) => warn!("Could not run icacls on {}: {}", path, e),
        }
    }
}

#[cfg(not(windows))]
pub fn run() -> anyhow::Result<()> {
    anyhow::bail!("Windows service mode is Windows-only")
}
#[cfg(not(windows))]
pub fn install() -> anyhow::Result<()> {
    anyhow::bail!("Windows service mode is Windows-only")
}
#[cfg(not(windows))]
pub fn uninstall() -> anyhow::Result<()> {
    anyhow::bail!("Windows service mode is Windows-only")
}
#[cfg(not(windows))]
pub fn start() -> anyhow::Result<()> {
    anyhow::bail!("Windows service mode is Windows-only")
}
#[cfg(not(windows))]
pub fn stop_if_running() -> anyhow::Result<()> {
    anyhow::bail!("Windows service mode is Windows-only")
}
#[cfg(not(windows))]
pub fn wait_until_settled(_timeout: std::time::Duration) -> anyhow::Result<String> {
    anyhow::bail!("Windows service mode is Windows-only")
}

/// True when the current process holds an elevated (high-integrity) token.
/// Installing a service needs it, and failing early with a clear message beats
/// an opaque access-denied from the SCM.
pub fn is_elevated() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION};
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        use windows_sys::Win32::Security::TOKEN_QUERY;

        unsafe {
            let mut token = 0;
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return false;
            }
            let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
            let mut size = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                &mut elevation as *mut _ as *mut std::ffi::c_void,
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut size,
            );
            CloseHandle(token);
            ok != 0 && elevation.TokenIsElevated != 0
        }
    }
    #[cfg(not(windows))]
    {
        false
    }
}
