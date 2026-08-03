use std::path::PathBuf;
use tokio::sync::watch;
use tracing::{error, info, warn};

const CLOUDFLARED_EXE: &[u8] = include_bytes!("../bin/cloudflared.exe");

pub async fn start(token: String, mut shutdown_rx: watch::Receiver<bool>) {
    let exe_path = match extract_exe() {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to extract cloudflared.exe: {}", e);
            return;
        }
    };

    // Create Job Object once — kept alive for entire process lifetime.
    // Any cloudflared process assigned to it is auto-killed when we exit.
    #[cfg(windows)]
    let job_handle = create_job_object();

    info!("Starting Cloudflare tunnel...");

    loop {
        match tokio::process::Command::new(&exe_path)
            .args(["tunnel", "run", "--token", &token])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .spawn()
        {
            Ok(mut child) => {
                let pid = child.id().unwrap_or(0);
                info!("Tunnel started (pid: {})", pid);

                // Assign the cloudflared child process to our Job Object.
                // When our process exits for ANY reason, Windows kills it.
                #[cfg(windows)]
                if job_handle != 0 && pid != 0 {
                    assign_process_to_job(pid, job_handle);
                }

                tokio::select! {
                    status = child.wait() => {
                        match status {
                            Ok(s) => warn!("Tunnel exited: {}. Restarting...", s),
                            Err(e) => error!("Tunnel error: {}. Restarting...", e),
                        }
                    }
                    _ = shutdown_rx.changed() => {
                        info!("Shutdown — killing tunnel...");
                        let _ = child.kill().await;
                        return;
                    }
                }
            }
            Err(e) => {
                error!("Failed to spawn cloudflared: {}", e);
            }
        }

        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
                info!("Reconnecting tunnel...");
            }
            _ = shutdown_rx.changed() => {
                info!("Shutdown during retry wait");
                return;
            }
        }
    }
}

/// Creates a Job Object with KILL_ON_JOB_CLOSE.
/// Returns the raw handle value (kept alive by the caller's stack frame).
/// 0 means creation failed — assignment will be skipped gracefully.
#[cfg(windows)]
fn create_job_object() -> isize {
    use windows::Win32::System::JobObjects::{
        CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    unsafe {
        let job = match CreateJobObjectW(None, None) {
            Ok(h) => h,
            Err(e) => {
                warn!("Failed to create Job Object: {}", e);
                return 0;
            }
        };

        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        if let Err(e) = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &raw const info as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) {
            warn!("SetInformationJobObject failed: {}", e);
            return 0;
        }

        info!("Job Object created — cloudflared will be auto-killed on any exit");
        job.0 as isize
    }
}

/// Opens the cloudflared process by PID and assigns it to the Job Object.
/// After this, if our process exits for any reason, Windows kills cloudflared too.
#[cfg(windows)]
fn assign_process_to_job(pid: u32, job_handle: isize) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::JobObjects::AssignProcessToJobObject;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_ALL_ACCESS};

    unsafe {
        let job = windows::Win32::Foundation::HANDLE(job_handle as *mut _);

        match OpenProcess(PROCESS_ALL_ACCESS, false, pid) {
            Ok(proc) => {
                match AssignProcessToJobObject(job, proc) {
                    Ok(_) => info!("Cloudflared (pid {}) assigned to Job Object", pid),
                    Err(e) => warn!("AssignProcessToJobObject failed: {}", e),
                }
                let _ = CloseHandle(proc);
            }
            Err(e) => warn!("OpenProcess failed for pid {}: {}", pid, e),
        }
    }
}

fn extract_exe() -> anyhow::Result<PathBuf> {
    let path = std::env::temp_dir().join("win_automation_cloudflared.exe");

    let needs_write = if path.exists() {
        match std::fs::metadata(&path) {
            Ok(meta) => meta.len() != CLOUDFLARED_EXE.len() as u64,
            Err(_) => true,
        }
    } else {
        true
    };

    if needs_write {
        std::fs::write(&path, CLOUDFLARED_EXE)?;
        info!("Extracted cloudflared.exe to {:?}", path);
    }

    Ok(path)
}
