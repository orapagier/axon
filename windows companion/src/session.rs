//! Plane B launcher — runs the desktop agent inside the interactive session.
//!
//! The service (Plane A) lives in session 0 and can never touch the desktop:
//! `SendInput` goes nowhere, screen capture returns black, and the clipboard it
//! sees is not the user's. Those routes only work from a process running on
//! `WinSta0\Default` in the logged-on user's session, which is what this module
//! creates.
//!
//! The launch sequence is the standard service-to-desktop hop:
//!
//!   WTSGetActiveConsoleSessionId  → which session is at the physical console
//!   WTSQueryUserToken             → that user's token (needs SeTcbPrivilege)
//!   GetTokenInformation(Linked)   → swap in the elevated token if UAC split it
//!   DuplicateTokenEx              → convert to a primary token we can spawn with
//!   CreateEnvironmentBlock        → the user's real environment, not SYSTEM's
//!   CreateProcessAsUserW          → spawn onto WinSta0\Default
//!
//! The elevated-token step matters more than it looks. UIPI blocks a
//! medium-integrity process from sending input to a higher-integrity window, so
//! an unelevated agent silently fails to type into anything running as admin —
//! Task Manager, an elevated terminal, most installers.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{watch, Notify};
use tracing::{error, info, warn};

/// Returned by `WTSGetActiveConsoleSessionId` when no user is logged on at the
/// console — the machine is sitting at the login screen. Plane A keeps serving;
/// the desktop routes report 503 until someone logs in.
const NO_SESSION: u32 = 0xFFFF_FFFF;

/// How long to wait before looking for a console session again when there is
/// none. Session arrival also fires an immediate `kick()` from the service's
/// SESSIONCHANGE handler, so this is only a backstop.
const IDLE_POLL_SECS: u64 = 15;

/// Liveness poll interval for a running agent. `WaitForSingleObject` with a 0
/// timeout returns immediately, so this costs nothing but a syscall.
const LIVENESS_POLL_SECS: u64 = 2;

const MIN_BACKOFF_SECS: u64 = 5;
const MAX_BACKOFF_SECS: u64 = 120;

/// Handle to the desktop agent, shared between the supervisor that keeps it
/// alive and the proxy that forwards desktop routes to it.
pub struct SessionAgent {
    /// Loopback port the agent listens on. Chosen once at service startup.
    pub port: u16,
    /// Shared secret for the loopback hop, handed to the agent over a pipe.
    /// Regenerated every service start.
    pub token: String,
    /// Passed through so /screenshot/save can build absolute URLs. Public
    /// hostname, not a secret — the agent deliberately never reads config.toml,
    /// so `api_secret` stays inside the service.
    pub public_url: String,
    /// Whether an agent is currently up and should be able to serve requests.
    ready: AtomicBool,
    /// Lets the service's SESSIONCHANGE handler wake the supervisor the moment
    /// a user logs on or unlocks, instead of waiting out the poll interval.
    kick: Notify,
}

impl SessionAgent {
    pub fn new(port: u16, token: String, public_url: String) -> Self {
        Self {
            port,
            token,
            public_url,
            ready: AtomicBool::new(false),
            kick: Notify::new(),
        }
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    /// Wake the supervisor immediately. Safe to call from anywhere, including
    /// the SCM control handler thread.
    pub fn kick(&self) {
        self.kick.notify_one();
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// Keeps exactly one desktop agent alive for as long as there is a console
/// session, restarting it across logon, logoff, unlock and fast-user-switch.
pub async fn supervise(agent: Arc<SessionAgent>, mut shutdown_rx: watch::Receiver<bool>) {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            error!("Cannot resolve own exe path, desktop agent disabled: {}", e);
            return;
        }
    };

    // Any agent we spawn joins this job, so it dies with the service even if the
    // service is killed rather than stopped cleanly. Same approach tunnel.rs
    // uses for cloudflared.
    let job = create_job_object();

    let mut backoff_secs = MIN_BACKOFF_SECS;

    loop {
        if *shutdown_rx.borrow() {
            info!("Shutdown — desktop agent supervisor stopping");
            return;
        }

        let session_id = active_console_session();

        let Some(session_id) = session_id else {
            agent.ready.store(false, Ordering::Relaxed);
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(IDLE_POLL_SECS)) => {}
                _ = agent.kick.notified() => {
                    info!("Session change signalled — re-checking for a console session");
                }
                _ = shutdown_rx.changed() => continue,
            }
            continue;
        };

        info!("Console session {} active — launching desktop agent", session_id);

        match spawn_in_session(session_id, &exe, &agent, job) {
            Ok(child) => {
                info!("Desktop agent running (pid {}) in session {}", child.pid, session_id);
                agent.ready.store(true, Ordering::Relaxed);
                backoff_secs = MIN_BACKOFF_SECS;

                // Hold here until the agent exits (logoff, crash) or we shut down.
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(LIVENESS_POLL_SECS)) => {
                            if !child.is_running() {
                                warn!("Desktop agent (pid {}) exited — will relaunch", child.pid);
                                break;
                            }
                        }
                        _ = agent.kick.notified() => {
                            // A session change while the agent is up means the
                            // console moved to a different user. Restart so the
                            // agent follows it.
                            if active_console_session() != Some(session_id) {
                                info!("Console session moved away from {} — restarting agent", session_id);
                                child.terminate();
                                break;
                            }
                        }
                        _ = shutdown_rx.changed() => {
                            info!("Shutdown — terminating desktop agent");
                            child.terminate();
                            agent.ready.store(false, Ordering::Relaxed);
                            return;
                        }
                    }
                }

                agent.ready.store(false, Ordering::Relaxed);
            }
            Err(e) => {
                agent.ready.store(false, Ordering::Relaxed);
                error!("Failed to launch desktop agent in session {}: {}", session_id, e);
                backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);

                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)) => {}
                    _ = shutdown_rx.changed() => continue,
                }
            }
        }
    }
}

/// The session currently attached to the physical console, if any.
pub fn active_console_session() -> Option<u32> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
        let id = unsafe { WTSGetActiveConsoleSessionId() };
        // Session 0 is the isolated service session — never an interactive desktop.
        if id == NO_SESSION || id == 0 {
            None
        } else {
            Some(id)
        }
    }
    #[cfg(not(windows))]
    {
        None
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Process handle
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(windows)]
pub struct OwnedProcess {
    handle: windows_sys::Win32::Foundation::HANDLE,
    pub pid: u32,
}

#[cfg(windows)]
impl OwnedProcess {
    pub fn is_running(&self) -> bool {
        use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        unsafe { WaitForSingleObject(self.handle, 0) == WAIT_TIMEOUT }
    }

    pub fn terminate(&self) {
        use windows_sys::Win32::System::Threading::TerminateProcess;
        unsafe {
            TerminateProcess(self.handle, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// The service-to-desktop hop
// ──────────────────────────────────────────────────────────────────────────────

/// Spawns the agent into `session_id` on `WinSta0\Default`, writing `token` to
/// its stdin. Must be called from a process holding SeTcbPrivilege — i.e.
/// LocalSystem.
#[cfg(windows)]
fn spawn_in_session(
    session_id: u32,
    exe: &PathBuf,
    agent: &SessionAgent,
    job: isize,
) -> anyhow::Result<OwnedProcess> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::{
        DuplicateTokenEx, GetTokenInformation, SecurityImpersonation, TokenLinkedToken,
        TokenPrimary, SECURITY_ATTRIBUTES, TOKEN_ALL_ACCESS, TOKEN_LINKED_TOKEN,
    };
    use windows_sys::Win32::Storage::FileSystem::WriteFile;
    use windows_sys::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::RemoteDesktop::WTSQueryUserToken;
    use windows_sys::Win32::System::Threading::{
        CreateProcessAsUserW, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION,
        STARTUPINFOW,
    };

    unsafe {
        // ── 1. The logged-on user's token ────────────────────────────────────
        let mut user_token: HANDLE = 0;
        if WTSQueryUserToken(session_id, &mut user_token) == 0 {
            anyhow::bail!(
                "WTSQueryUserToken failed for session {} (error {}). This requires \
                 SeTcbPrivilege — the service must run as LocalSystem.",
                session_id,
                std::io::Error::last_os_error()
            );
        }
        let _user_guard = HandleGuard(user_token);

        // ── 2. Prefer the elevated half of a UAC split token ─────────────────
        // For an admin user, WTSQueryUserToken hands back the *filtered* token.
        // Without this swap the agent runs at medium integrity and UIPI silently
        // drops its input into any elevated window.
        let mut linked = TOKEN_LINKED_TOKEN { LinkedToken: 0 };
        let mut returned: u32 = 0;
        let elevated_guard = if GetTokenInformation(
            user_token,
            TokenLinkedToken,
            &mut linked as *mut _ as *mut std::ffi::c_void,
            std::mem::size_of::<TOKEN_LINKED_TOKEN>() as u32,
            &mut returned,
        ) != 0
            && linked.LinkedToken != 0
        {
            info!("Using elevated linked token for the desktop agent");
            Some(HandleGuard(linked.LinkedToken))
        } else {
            // Standard user, or UAC disabled — the token we already have is the
            // only one, and is already as privileged as that account gets.
            None
        };
        let source_token = elevated_guard
            .as_ref()
            .map(|g| g.0)
            .unwrap_or(user_token);

        // ── 3. Make it a primary token we can spawn with ─────────────────────
        let mut primary: HANDLE = 0;
        if DuplicateTokenEx(
            source_token,
            TOKEN_ALL_ACCESS,
            std::ptr::null(),
            SecurityImpersonation,
            TokenPrimary,
            &mut primary,
        ) == 0
        {
            anyhow::bail!(
                "DuplicateTokenEx failed: {}",
                std::io::Error::last_os_error()
            );
        }
        let _primary_guard = HandleGuard(primary);

        // ── 4. Pipe for the loopback token ───────────────────────────────────
        // argv would put the secret in the process list for every local user to
        // read; an inherited pipe keeps it in kernel memory only.
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        let mut pipe_read: HANDLE = 0;
        let mut pipe_write: HANDLE = 0;
        if CreatePipe(&mut pipe_read, &mut pipe_write, &sa, 0) == 0 {
            anyhow::bail!("CreatePipe failed: {}", std::io::Error::last_os_error());
        }
        let pipe_read_guard = HandleGuard(pipe_read);
        let pipe_write_guard = HandleGuard(pipe_write);

        // ── 5. The user's environment, not SYSTEM's ──────────────────────────
        let mut env_block: *mut std::ffi::c_void = std::ptr::null_mut();
        let have_env = CreateEnvironmentBlock(&mut env_block, primary, 0) != 0;
        if !have_env {
            warn!(
                "CreateEnvironmentBlock failed ({}) — agent will inherit a minimal environment",
                std::io::Error::last_os_error()
            );
        }

        // ── 6. Spawn onto the interactive desktop ────────────────────────────
        let mut cmdline: Vec<u16> = std::ffi::OsStr::new(&format!(
            "\"{}\" --session-agent --session-port {} --public-url \"{}\"",
            exe.display(),
            agent.port,
            agent.public_url
        ))
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

        // This is the whole point of the exercise: WinSta0\Default is the
        // interactive window station and desktop. A service that omits this
        // lands on its own invisible station where input and capture do nothing.
        let mut desktop: Vec<u16> = std::ffi::OsStr::new("WinSta0\\Default")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        si.lpDesktop = desktop.as_mut_ptr();
        si.dwFlags = windows_sys::Win32::System::Threading::STARTF_USESTDHANDLES;
        si.hStdInput = pipe_read;
        si.hStdOutput = INVALID_HANDLE_VALUE;
        si.hStdError = INVALID_HANDLE_VALUE;

        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();

        let ok = CreateProcessAsUserW(
            primary,
            std::ptr::null(),
            cmdline.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1, // inherit handles — required for the stdin pipe
            CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            if have_env {
                env_block
            } else {
                std::ptr::null_mut()
            },
            std::ptr::null(),
            &si,
            &mut pi,
        );

        if have_env {
            DestroyEnvironmentBlock(env_block);
        }

        if ok == 0 {
            anyhow::bail!(
                "CreateProcessAsUserW failed: {}",
                std::io::Error::last_os_error()
            );
        }

        // The child owns its end now; keeping ours open would stop it seeing EOF.
        drop(pipe_read_guard);
        CloseHandle(pi.hThread);

        // ── 7. Hand over the loopback token ──────────────────────────────────
        let line = format!("{}\n", agent.token);
        let mut written: u32 = 0;
        let wrote = WriteFile(
            pipe_write,
            line.as_ptr(),
            line.len() as u32,
            &mut written,
            std::ptr::null_mut(),
        );
        drop(pipe_write_guard); // EOF for the child

        if wrote == 0 {
            warn!(
                "Failed to write session token to agent stdin: {}",
                std::io::Error::last_os_error()
            );
        }

        if job != 0 {
            assign_process_to_job(pi.hProcess, job);
        }

        Ok(OwnedProcess {
            handle: pi.hProcess,
            pid: pi.dwProcessId,
        })
    }
}

#[cfg(not(windows))]
fn spawn_in_session(
    _session_id: u32,
    _exe: &PathBuf,
    _agent: &SessionAgent,
    _job: isize,
) -> anyhow::Result<OwnedProcess> {
    anyhow::bail!("Session agent launching is Windows-only")
}

#[cfg(not(windows))]
pub struct OwnedProcess {
    pub pid: u32,
}

#[cfg(not(windows))]
impl OwnedProcess {
    pub fn is_running(&self) -> bool {
        false
    }
    pub fn terminate(&self) {}
}

/// Closes a raw handle on drop. Every early `bail!` in `spawn_in_session` would
/// otherwise leak one.
#[cfg(windows)]
struct HandleGuard(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for HandleGuard {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        if self.0 != 0 {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Job object — the agent must not outlive the service
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(windows)]
fn create_job_object() -> isize {
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job == 0 {
            warn!("Failed to create Job Object for the desktop agent");
            return 0;
        }

        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
        {
            warn!("SetInformationJobObject failed for the desktop agent job");
            return 0;
        }

        job as isize
    }
}

#[cfg(not(windows))]
fn create_job_object() -> isize {
    0
}

#[cfg(windows)]
fn assign_process_to_job(process: windows_sys::Win32::Foundation::HANDLE, job: isize) {
    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
    unsafe {
        if AssignProcessToJobObject(job as _, process) == 0 {
            warn!(
                "AssignProcessToJobObject failed for the desktop agent: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}

/// Reads the loopback token the service wrote to our stdin. Called by the agent
/// process at startup, before it binds.
pub fn read_token_from_stdin() -> anyhow::Result<String> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| anyhow::anyhow!("could not read session token from stdin: {}", e))?;
    let token = buf.trim().to_string();
    if token.is_empty() {
        anyhow::bail!("session token from stdin was empty");
    }
    Ok(token)
}
