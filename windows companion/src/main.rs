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

  windowsapi                      Double-click / no arguments: set up and start
                                  the service, prompting for admin if needed.
                                  A config.toml next to this exe is installed
                                  alongside it.

  windowsapi --install            Same as above, explicitly
  windowsapi --uninstall          Stop and remove the service (needs admin)
  windowsapi --start              Start the installed service
  windowsapi --stop               Stop the installed service
  windowsapi --user-mode          Run in this console as the current user
                                  (no service, no lock-screen access)

  --quiet                         Log results instead of showing a dialog.
                                  Required when calling this exe from a script
                                  or installer.

The service runs as LocalSystem, so /shell, /files/*, /system/*, /processes and
/registry/* stay available while the machine is locked or at the login screen.
Screenshot, clipboard, keyboard, mouse and window routes are served by a helper
launched into the interactive session, and return 503 when nobody is logged on.
";

/// Suppresses the message-box fallback in `notify_user`.
///
/// Set by `--quiet`, which the installer passes to every invocation it makes.
/// Those run with Inno's `runhidden` and `waituntilterminated`: there is no
/// console to attach to, so an unsuppressed message box is both invisible and
/// modal, and setup hangs on it forever.
static QUIET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);

    init_logging();

    if has("--quiet") {
        QUIET.store(true, std::sync::atomic::Ordering::Relaxed);
    }

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

    // Double-clicked with no arguments: set yourself up. Drop the exe and a
    // filled-in config.toml in a folder, run it, approve the UAC prompt, done.
    if args.len() == 1 || has("--install") {
        if !elevate_if_needed("--install") {
            return;
        }
        do_install();
        return;
    }

    if has("--help") || has("-h") || has("/?") {
        notify_user(USAGE);
        return;
    }

    if has("--uninstall") {
        if !elevate_if_needed("--uninstall") {
            return;
        }
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
        if !elevate_if_needed("--start") {
            return;
        }
        let _ = service::start();
        let state = service::wait_until_settled(std::time::Duration::from_secs(15))
            .unwrap_or_else(|e| format!("unknown ({e})"));
        if state == "Running" {
            notify_user("Service started.");
        } else {
            let reason = service::last_error_line()
                .unwrap_or_else(|| "no reason recorded in windowsapi_error.log".to_string());
            notify_user(&format!("Service is {state}.\n\nReason: {reason}"));
        }
        return;
    }

    if has("--stop") {
        if !elevate_if_needed("--stop") {
            return;
        }
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

/// Installs, starts, and then reports what actually happened.
///
/// The status check is the point. `start()` succeeding only means the SCM
/// accepted the request — a placeholder config produces a service that starts
/// and immediately stops, which would otherwise be reported as success.
fn do_install() {
    let dir = service::install_dir();

    if let Err(e) = service::install() {
        notify_user(&format!(
            "Install failed: {e}\n\nInstall directory: {}",
            dir.display()
        ));
        return;
    }

    // May report an error for a service that starts then exits; the state check
    // below is what decides.
    let _ = service::start();

    let state = service::wait_until_settled(std::time::Duration::from_secs(15))
        .unwrap_or_else(|e| format!("unknown ({e})"));

    if state == "Running" {
        notify_user(&format!(
            "Setup complete — the service is running.\n\n\
             Installed to: {}\n\n\
             The machine is now reachable while locked for /shell, /files/*, /system/* \
             and /registry/*. Screenshot, clipboard and input routes need someone \
             logged in.\n\n\
             Check both halves with:  GET /status\n\n\
             If this is a laptop, run scripts\\prepare-laptop.ps1 from an elevated \
             PowerShell so it does not sleep.",
            dir.display()
        ));
    } else {
        let reason = service::last_error_line()
            .unwrap_or_else(|| "no reason recorded in windowsapi_error.log".to_string());
        notify_user(&format!(
            "Installed, but the service is {state}.\n\n\
             Reason: {reason}\n\n\
             Fix it in:  {}\\config.toml\n\
             Then run:   windowsapi --start\n\n\
             A fresh install ships placeholder credentials, so this is expected until \
             tunnel_token and api_secret are filled in.",
            dir.display()
        ));
    }
}

/// Re-launches this exe elevated, carrying `flag`, and returns false if the
/// caller should stop because the elevated instance has taken over (or the user
/// declined the prompt).
///
/// Without this, double-clicking produced "needs an elevated prompt" and left
/// the user to find a terminal — but registering a LocalSystem service is the
/// entire point, so the prompt has to come from us.
fn elevate_if_needed(flag: &str) -> bool {
    if service::is_elevated() {
        return true;
    }

    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                notify_user(&format!("Could not locate own executable: {e}"));
                return false;
            }
        };

        let wide = |s: &str| {
            use std::os::windows::ffi::OsStrExt;
            std::ffi::OsStr::new(s)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect::<Vec<u16>>()
        };

        let verb = wide("runas");
        let file = wide(&exe.to_string_lossy());
        let params = wide(flag);

        // ShellExecuteW returns a fake HINSTANCE; anything > 32 means success.
        let rc = unsafe {
            ShellExecuteW(
                0,
                verb.as_ptr(),
                file.as_ptr(),
                params.as_ptr(),
                std::ptr::null(),
                SW_SHOWNORMAL as i32,
            )
        };

        if rc as isize > 32 {
            // The elevated copy owns the install from here.
            return false;
        }

        notify_user(
            "Administrator approval is required to register the service.\n\n\
             Nothing was changed. Run this again and choose Yes at the prompt, or \
             start it from an elevated PowerShell.",
        );
        return false;
    }

    #[cfg(not(windows))]
    {
        let _ = flag;
        false
    }
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
        tunnel::start(
            config.tunnel_token.clone(),
            config.tunnel_protocol.clone(),
            config.tunnel_metrics_port(),
            shutdown_rx.clone(),
        ),
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

/// Gets a message in front of the user regardless of how the exe was launched.
///
/// Release builds are GUI-subsystem, so `println!` goes nowhere. Attaching to
/// the parent console covers the "ran it from a terminal" case; the message box
/// covers double-clicking.
fn notify_user(msg: &str) {
    // Non-interactive caller (the installer). A message box here would be
    // invisible under `runhidden` and would block setup indefinitely.
    if QUIET.load(std::sync::atomic::Ordering::Relaxed) {
        info!("{}", msg);
        log_error_to_file(msg);
        return;
    }

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
