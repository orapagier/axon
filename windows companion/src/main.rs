// No console window in release builds — runs as a silent background process.
// CLI modes re-attach to the parent console explicitly, see `notify_user`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod auth;
mod config;
mod indexer;
mod proxy;
mod routes;
mod server;
mod service;
mod session;
mod session_server;
mod tunnel;

use std::sync::Arc;
use tokio::sync::watch;
use tracing::{error, info};

const USAGE: &str = "\
Windows Automation API

  windowsapi --install            Install and start the service (needs admin)
  windowsapi --uninstall          Stop and remove the service (needs admin)
  windowsapi --start              Start the installed service
  windowsapi --stop               Stop the installed service
  windowsapi --user-mode          Run in this console as the current user
                                  (no service, no lock-screen access)

The service runs as LocalSystem, so /shell, /files/*, /system/*, /processes and
/registry/* stay available while the machine is locked or at the login screen.
Screenshot, clipboard, keyboard, mouse and window routes are served by a helper
launched into the interactive session, and return 503 when nobody is logged on.
";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);

    init_logging();

    // Launched by the SCM. Must hand control to the dispatcher promptly or
    // Windows fails the start with error 1053.
    if has("--service") {
        if let Err(e) = service::run() {
            let msg = format!("Service dispatcher failed: {e}");
            error!("{}", msg);
            log_error_to_file(&msg);
        }
        return;
    }

    // Launched by the service into the interactive session.
    if has("--session-agent") {
        run_session_agent(&args);
        return;
    }

    if has("--install") {
        require_admin("--install");
        match service::install().and_then(|_| service::start()) {
            Ok(_) => notify_user(&format!(
                "Service installed and started.\n\nInstall directory: {}\n\nEdit config.toml \
                 there, then run: windowsapi --stop && windowsapi --start",
                service::install_dir().display()
            )),
            Err(e) => notify_user(&format!("Install failed: {e}")),
        }
        return;
    }

    if has("--uninstall") {
        require_admin("--uninstall");
        match service::uninstall() {
            Ok(_) => notify_user(&format!(
                "Service removed. {} was left in place — delete it manually if you also \
                 want the config and cached files gone.",
                service::install_dir().display()
            )),
            Err(e) => notify_user(&format!("Uninstall failed: {e}")),
        }
        return;
    }

    if has("--start") {
        require_admin("--start");
        match service::start() {
            Ok(_) => notify_user("Service started."),
            Err(e) => notify_user(&format!("Could not start service: {e}")),
        }
        return;
    }

    if has("--stop") {
        require_admin("--stop");
        match service::stop_if_running() {
            Ok(_) => notify_user("Service stopped."),
            Err(e) => notify_user(&format!("Could not stop service: {e}")),
        }
        return;
    }

    if has("--user-mode") {
        run_user_mode();
        return;
    }

    notify_user(USAGE);
}

// ──────────────────────────────────────────────────────────────────────────────
// Plane A — everything the service runs
// ──────────────────────────────────────────────────────────────────────────────

/// Drives the four long-running pieces of the service: the tunnel, the public
/// API, the desktop-agent supervisor, and the public/ file expiry sweep.
///
/// The tunnel lives here rather than in the user session on purpose. Under the
/// old `HKCU\...\Run` model, logging out took cloudflared down with it — the
/// machine did not just lose its desktop routes, it fell off the internet.
pub async fn run_plane_a(
    config: Arc<config::Config>,
    agent: Arc<session::SessionAgent>,
    shutdown_rx: watch::Receiver<bool>,
) {
    info!(
        "Plane A starting — API on 127.0.0.1:{}, desktop agent on 127.0.0.1:{}",
        config.port,
        config.session_port()
    );

    tokio::join!(
        tunnel::start(config.tunnel_token.clone(), shutdown_rx.clone()),
        server::start(config.clone(), agent.clone(), shutdown_rx.clone()),
        session::supervise(agent.clone(), shutdown_rx.clone()),
        indexer::start(shutdown_rx.clone()),
    );

    info!("Plane A stopped");
}

// ──────────────────────────────────────────────────────────────────────────────
// Plane B — the desktop agent
// ──────────────────────────────────────────────────────────────────────────────

fn run_session_agent(args: &[String]) {
    let port = arg_value(args, "--session-port")
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(0);

    if port == 0 {
        let msg = "Desktop agent started without a valid --session-port".to_string();
        error!("{}", msg);
        log_error_to_file(&msg);
        return;
    }

    let public_url = arg_value(args, "--public-url").unwrap_or_default();

    // The shared token arrives on stdin rather than argv so it never shows up
    // in the process list, where any local user could read it.
    let token = match session::read_token_from_stdin() {
        Ok(t) => t,
        Err(e) => {
            let msg = format!("Desktop agent could not read its session token: {e}");
            error!("{}", msg);
            log_error_to_file(&msg);
            return;
        }
    };

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            log_error_to_file(&format!("Desktop agent runtime build failed: {e}"));
            return;
        }
    };

    rt.block_on(async {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            let _ = shutdown_tx.send(true);
        });

        session_server::start(port, token, public_url, shutdown_rx).await;
    });
}

// ──────────────────────────────────────────────────────────────────────────────
// Legacy single-process mode
// ──────────────────────────────────────────────────────────────────────────────

/// Runs both planes in one process as the current user, the way the app worked
/// before the service split. Kept as an escape hatch for machines where a
/// service cannot be installed, and for debugging without the SCM in the way.
///
/// The tradeoff is the original one: no LocalSystem, and everything dies with
/// the session, so nothing is reachable once you log out.
fn run_user_mode() {
    let config = Arc::new(match config::Config::load() {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("Config error: {e}");
            error!("{}", msg);
            log_error_to_file(&msg);
            notify_user(&msg);
            std::process::exit(1);
        }
    });

    notify_user(
        "Running in user mode. The API is up, but the machine is NOT reachable while \
         locked or logged out — install the service with --install for that.",
    );

    let agent = Arc::new(session::SessionAgent::new(
        config.session_port(),
        server::loopback_token(),
        config.public_url.clone(),
    ));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("failed to build runtime");

    rt.block_on(async {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            info!("Shutdown signal received");
            let _ = shutdown_tx.send(true);
        });

        run_plane_a(config, agent, shutdown_rx).await;
    });
}

// ──────────────────────────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────────────────────────

fn init_logging() {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_ids(false)
        .compact()
        .init();
}

fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn require_admin(what: &str) {
    if !service::is_elevated() {
        notify_user(&format!(
            "{what} needs an elevated prompt.\n\nOpen PowerShell or Terminal as \
             Administrator and run it again — registering a LocalSystem service is not \
             something a standard token can do."
        ));
        std::process::exit(1);
    }
}

/// Gets a message in front of the user regardless of how the exe was launched.
///
/// Release builds are GUI-subsystem, so `println!` goes nowhere. Attaching to
/// the parent console covers the "ran it from a terminal" case; the message box
/// covers double-clicking.
fn notify_user(msg: &str) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{
            AttachConsole, GetConsoleWindow, ATTACH_PARENT_PROCESS,
        };

        // Debug builds are console-subsystem and already own a console, and
        // AttachConsole fails with ERROR_ACCESS_DENIED in that case — without
        // this check every dev run would pop a message box instead of printing.
        let have_console = unsafe { GetConsoleWindow() != 0 }
            || unsafe { AttachConsole(ATTACH_PARENT_PROCESS) != 0 };

        if have_console {
            println!("\n{msg}\n");
            use std::io::Write;
            let _ = std::io::stdout().flush();
            return;
        }

        use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
        let wide = |s: &str| {
            use std::os::windows::ffi::OsStrExt;
            std::ffi::OsStr::new(s)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect::<Vec<u16>>()
        };
        let text = wide(msg);
        let title = wide("Windows Automation API");
        unsafe {
            MessageBoxW(
                0,
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONINFORMATION,
            );
        }
    }
    #[cfg(not(windows))]
    {
        println!("{msg}");
    }
}

/// Write an error to a log file next to the executable (useful when there's no console)
pub(crate) fn log_error_to_file(msg: &str) {
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
