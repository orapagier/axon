/// Automatic Windows Search Index manager.
///
/// Ensures every attached drive — including USB drives, external HDDs, and SD
/// cards plugged in AFTER startup — is immediately added to the Windows Search
/// Index so that `/files/search` always uses the fast index path.
///
/// Design:
///   • Startup  — enumerate every ready drive (C:–Z:) and add each to the index.
///   • Runtime  — a dedicated Win32 thread creates a message-only window and
///                registers for device-change notifications.  Windows delivers
///                WM_DEVICECHANGE / DBT_DEVICEARRIVAL the instant a new volume
///                mounts.  No polling, no timers, zero CPU cost at idle.
///   • Shutdown — WM_QUIT is posted to the message thread so it exits cleanly.
///
/// Index mutation uses PowerShell → ISearchCrawlScopeManager COM, the same API
/// Windows Explorer uses for "Add location to index". Adding a scope just
/// registers the path; the Search service crawls it in the background.
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;
use tracing::{info, warn};

/// Entry point — spawn from main alongside tunnel and server.
pub async fn start(mut shutdown_rx: watch::Receiver<bool>) {
    #[cfg(not(windows))]
    {
        debug!("Indexer: not on Windows, skipping");
        let _ = shutdown_rx.changed().await;
        return;
    }

    #[cfg(windows)]
    {
        info!("Indexer: starting");

        // Track submitted drives to avoid duplicate work
        let known: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

        // ── 1. Index every drive already attached ─────────────────────────────
        let initial = enumerate_ready_drives();
        {
            let mut k = known.lock().unwrap();
            for d in &initial {
                k.insert(d.clone());
            }
        }
        for d in &initial {
            add_to_index(d).await;
        }
        ensure_user_folders_indexed().await;
        info!(
            "Indexer: startup pass complete — {} drive(s) submitted",
            initial.len()
        );

        // ── 2. Spawn Win32 message thread to watch for new drives ─────────────
        // The thread signals us via an async channel when a new drive arrives.
        let (drive_tx, mut drive_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        {
            let known_clone = known.clone();
            std::thread::spawn(move || {
                device_watcher_thread(drive_tx, known_clone);
            });
        }

        // ── 3. Process drive-arrival events from the Win32 thread ─────────────
        loop {
            tokio::select! {
                maybe = drive_rx.recv() => {
                    match maybe {
                        Some(drive) => {
                            info!("Indexer: new drive '{}' — adding to index", drive);
                            add_to_index(&drive).await;
                        }
                        None => {
                            warn!("Indexer: device watcher thread exited unexpectedly");
                            break;
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        info!("Indexer: shutdown");
                        post_quit_to_watcher();
                        break;
                    }
                }
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Drive enumeration
// ─────────────────────────────────────────────────────────────────────────────

fn enumerate_ready_drives() -> Vec<String> {
    (b'C'..=b'Z')
        .map(|c| format!("{}:\\", c as char))
        .filter(|root| std::path::Path::new(root).exists())
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Win32 device-change notification (message-only window)
// ─────────────────────────────────────────────────────────────────────────────
//
// We create a message-only window (HWND_MESSAGE) — it is invisible, has no
// taskbar entry, and exists purely to receive WM_DEVICECHANGE messages.
// RegisterDeviceNotification with DBT_DEVTYP_VOLUME makes Windows deliver
// DBT_DEVICEARRIVAL the moment any volume is mounted.
//
// The window procedure extracts the drive-letter bitmask from the
// DEV_BROADCAST_VOLUME structure, converts it to root paths, and sends
// them to the async side over an unbounded channel.
//
// Crate dependency (add to Cargo.toml):
//   [target.'cfg(windows)'.dependencies]
//   windows-sys = { version = "0.52", features = [
//     "Win32_Foundation",
//     "Win32_UI_WindowsAndMessaging",
//     "Win32_Devices_DeviceAndDriverInstallation",
//     "Win32_System_Threading",
//   ]}

#[cfg(windows)]
use std::sync::atomic::{AtomicU32, Ordering};

// Watcher thread ID — stored so async code can post WM_QUIT on shutdown
#[cfg(windows)]
static WATCHER_THREAD_ID: AtomicU32 = AtomicU32::new(0);

// Channel sender placed in a OnceLock so the extern "system" wnd_proc can
// reach it without capturing a closure (C callbacks cannot capture).
#[cfg(windows)]
static DRIVE_TX: std::sync::OnceLock<Mutex<Option<tokio::sync::mpsc::UnboundedSender<String>>>> =
    std::sync::OnceLock::new();

#[cfg(windows)]
static KNOWN_DRIVES: std::sync::OnceLock<Arc<Mutex<HashSet<String>>>> = std::sync::OnceLock::new();

#[cfg(windows)]
unsafe extern "system" fn wnd_proc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, DBT_DEVICEARRIVAL, DBT_DEVTYP_VOLUME, DEV_BROADCAST_HDR,
        DEV_BROADCAST_VOLUME, WM_DEVICECHANGE,
    };

    if msg == WM_DEVICECHANGE && wparam as u32 == DBT_DEVICEARRIVAL {
        let hdr = &*(lparam as *const DEV_BROADCAST_HDR);
        if hdr.dbch_devicetype == DBT_DEVTYP_VOLUME {
            let vol = &*(lparam as *const DEV_BROADCAST_VOLUME);
            let mask = vol.dbcv_unitmask;

            for bit in 0u32..26 {
                if mask & (1 << bit) != 0 {
                    let letter = (b'A' + bit as u8) as char;
                    if letter == 'A' || letter == 'B' {
                        continue;
                    }
                    let root = format!("{}:\\", letter);

                    if let Some(known_lock) = KNOWN_DRIVES.get() {
                        let mut known = known_lock.lock().unwrap();
                        if known.insert(root.clone()) {
                            if let Some(tx_lock) = DRIVE_TX.get() {
                                if let Ok(guard) = tx_lock.lock() {
                                    if let Some(tx) = guard.as_ref() {
                                        let _ = tx.send(root);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    DefWindowProcW(hwnd, msg, wparam, lparam)
}

#[cfg(windows)]
fn device_watcher_thread(
    tx: tokio::sync::mpsc::UnboundedSender<String>,
    known: Arc<Mutex<HashSet<String>>>,
) {
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DispatchMessageW, GetMessageW, RegisterClassW,
        RegisterDeviceNotificationW, CS_HREDRAW, CS_VREDRAW, DBT_DEVTYP_VOLUME,
        DEVICE_NOTIFY_WINDOW_HANDLE, DEV_BROADCAST_HDR, HWND_MESSAGE, MSG, WNDCLASSW,
        WS_OVERLAPPED,
    };

    // Store globals for the wnd_proc callback
    let _ = DRIVE_TX.set(Mutex::new(Some(tx)));
    let _ = KNOWN_DRIVES.set(known);
    WATCHER_THREAD_ID.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);

    unsafe {
        let class_name: Vec<u16> = "WinAPIDevWatch\0".encode_utf16().collect();

        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: 0,
            hIcon: 0,
            hCursor: 0,
            hbrBackground: 0,
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };
        RegisterClassW(&wc);

        // Message-only window: HWND_MESSAGE parent, invisible, no taskbar entry
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            std::ptr::null(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            0,
            0,
            std::ptr::null(),
        );

        if hwnd == 0 {
            tracing::error!("Indexer: CreateWindowExW failed");
            return;
        }

        // Tell Windows to send us DBT_DEVICEARRIVAL for volumes
        let filter = DEV_BROADCAST_HDR {
            dbch_size: std::mem::size_of::<DEV_BROADCAST_HDR>() as u32,
            dbch_devicetype: DBT_DEVTYP_VOLUME,
            dbch_reserved: 0,
        };
        RegisterDeviceNotificationW(
            hwnd as _,
            &filter as *const _ as *const _,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        );

        info!(
            "Indexer: device watcher active (thread {})",
            GetCurrentThreadId()
        );

        // Standard Win32 message loop — blocks here until WM_QUIT
        let mut msg = MSG {
            hwnd: 0,
            message: 0,
            wParam: 0,
            lParam: 0,
            time: 0,
            pt: windows_sys::Win32::Foundation::POINT { x: 0, y: 0 },
        };
        while GetMessageW(&mut msg, 0, 0, 0) > 0 {
            DispatchMessageW(&msg);
        }

        info!("Indexer: message loop exited");
    }
}

#[cfg(windows)]
fn post_quit_to_watcher() {
    use windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW;
    use windows_sys::Win32::UI::WindowsAndMessaging::WM_QUIT;
    let tid = WATCHER_THREAD_ID.load(Ordering::Relaxed);
    if tid != 0 {
        unsafe { PostThreadMessageW(tid, WM_QUIT, 0, 0) };
    }
}

#[cfg(not(windows))]
fn device_watcher_thread(
    _tx: tokio::sync::mpsc::UnboundedSender<String>,
    _known: Arc<Mutex<HashSet<String>>>,
) {
}

#[cfg(not(windows))]
fn post_quit_to_watcher() {}

// ─────────────────────────────────────────────────────────────────────────────
// Index mutation via PowerShell → ISearchCrawlScopeManager COM
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(windows)]
async fn add_to_index(path: &str) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let uri = format!("file:///{}", path.replace('\\', "/").trim_end_matches('/'));
    let uri_esc = uri.replace('\'', "''");

    let ps = format!(
        r#"
$ErrorActionPreference = 'Stop'
try {{
    $mgr   = New-Object -ComObject Microsoft.Windows.Search.Indexer
    $cat   = $mgr.GetCatalog('SystemIndex')
    $scope = $cat.GetCrawlScopeManager()
    $uri   = '{uri}'
    if (-not $scope.IncludedInCrawlScope($uri)) {{
        $scope.AddUserScopeRule($uri, $true, $false, $null)
        $scope.SaveAll()
        Write-Host "added"
    }} else {{
        Write-Host "already"
    }}
}} catch {{
    Write-Host "error: $_"; exit 1
}}
"#,
        uri = uri_esc,
    );

    let path_str = path.to_string();
    match tokio::task::spawn_blocking(move || {
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
    })
    .await
    {
        Ok(Ok(out)) => {
            let msg = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if out.status.success() {
                info!("Indexer: '{}' — {}", path_str, msg);
            } else {
                warn!("Indexer: failed to add '{}' to index: {}", path_str, msg);
            }
        }
        Ok(Err(e)) => warn!("Indexer: spawn error '{}': {}", path_str, e),
        Err(e) => warn!("Indexer: task error '{}': {}", path_str, e),
    }
}

/// Ensure the user's home folder tree and OneDrive paths are indexed.
/// Normally indexed by default but can fall out after a Search service reset.
#[cfg(windows)]
async fn ensure_user_folders_indexed() {
    let mut paths: Vec<String> = Vec::new();

    // %LOCALAPPDATA% is e.g. C:\Users\Admin\AppData\Local
    // Go up two levels to reach C:\Users\Admin
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        if let Some(home) = std::path::Path::new(&local)
            .parent() // → AppData
            .and_then(|p| p.parent())
        // → C:\Users\Admin
        {
            paths.push(home.to_string_lossy().to_string());
        }
    }

    for var in &["OneDriveCommercial", "OneDriveConsumer", "OneDrive"] {
        if let Ok(p) = std::env::var(var) {
            if !p.is_empty() {
                paths.push(p);
            }
        }
    }

    let mut seen = HashSet::new();
    for p in paths {
        if seen.insert(p.clone()) {
            add_to_index(&p).await;
        }
    }
}
