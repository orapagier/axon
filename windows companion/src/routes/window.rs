use axum::Json;
use serde::{Deserialize, Serialize};

use crate::routes::{ActionResponse, AppError};

#[derive(Serialize, Clone)]
pub struct WindowInfo {
    pub hwnd: i64,
    pub title: String,
    pub class: String,
}

#[derive(Deserialize)]
pub struct WindowRequest {
    /// Match window by title (partial match)
    pub title: Option<String>,
    /// Or by exact HWND value from list_windows
    pub hwnd: Option<i64>,
}

#[derive(Deserialize)]
pub struct ResizeRequest {
    pub title: Option<String>,
    pub hwnd: Option<i64>,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::*;

    // Callback for EnumWindows — collects visible windows with titles
    unsafe extern "system" fn enum_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let list = &mut *(lparam.0 as *mut Vec<WindowInfo>);

        // Only visible windows
        if !IsWindowVisible(hwnd).as_bool() {
            return BOOL(1);
        }

        // Get title
        let mut title_buf = [0u16; 512];
        let title_len = GetWindowTextW(hwnd, &mut title_buf);
        if title_len == 0 {
            return BOOL(1);
        }
        let title = String::from_utf16_lossy(&title_buf[..title_len as usize]);

        // Get class name
        let mut class_buf = [0u16; 256];
        let class_len = GetClassNameW(hwnd, &mut class_buf);
        let class = if class_len > 0 {
            String::from_utf16_lossy(&class_buf[..class_len as usize])
        } else {
            String::new()
        };

        list.push(WindowInfo {
            hwnd: hwnd.0 as i64,
            title,
            class,
        });

        BOOL(1) // continue enumeration
    }

    pub fn get_all_windows() -> Vec<WindowInfo> {
        let mut windows: Vec<WindowInfo> = Vec::new();
        unsafe {
            let _ = EnumWindows(
                Some(enum_callback),
                LPARAM(&mut windows as *mut Vec<WindowInfo> as isize),
            );
        }
        windows
    }

    pub fn find_hwnd(title: Option<&str>, hwnd_val: Option<i64>) -> Option<HWND> {
        if let Some(h) = hwnd_val {
            return Some(HWND(h as *mut core::ffi::c_void));
        }
        if let Some(t) = title {
            let windows = get_all_windows();
            let win = windows
                .into_iter()
                .find(|w| w.title.to_lowercase().contains(&t.to_lowercase()))?;
            return Some(HWND(win.hwnd as *mut core::ffi::c_void));
        }
        None
    }

    pub fn do_focus(hwnd: HWND) {
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
    }

    pub fn do_close(hwnd: HWND) {
        unsafe {
            let _ = PostMessageW(hwnd, WM_CLOSE, None, None);
        }
    }

    pub fn do_minimize(hwnd: HWND) {
        unsafe {
            let _ = ShowWindow(hwnd, SW_MINIMIZE);
        }
    }

    pub fn do_maximize(hwnd: HWND) {
        unsafe {
            let _ = ShowWindow(hwnd, SW_MAXIMIZE);
        }
    }

    pub fn do_resize(hwnd: HWND, x: i32, y: i32, width: u32, height: u32) {
        unsafe {
            let _ = MoveWindow(hwnd, x, y, width as i32, height as i32, true);
        }
    }
}

pub async fn list_windows() -> Json<Vec<WindowInfo>> {
    #[cfg(windows)]
    return tokio::task::spawn_blocking(win::get_all_windows)
        .await
        .map(Json)
        .unwrap_or(Json(vec![]));

    #[cfg(not(windows))]
    Json(vec![])
}

pub async fn focus_window(
    Json(req): Json<WindowRequest>,
) -> Result<Json<ActionResponse>, AppError> {
    #[cfg(windows)]
    {
        tokio::task::spawn_blocking(move || {
            let hwnd = win::find_hwnd(req.title.as_deref(), req.hwnd)
                .ok_or_else(|| AppError::not_found("Window not found"))?;
            win::do_focus(hwnd);
            Ok(ActionResponse::ok())
        })
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
    }

    #[cfg(not(windows))]
    Err(AppError::not_implemented("Windows only"))
}

pub async fn close_window(
    Json(req): Json<WindowRequest>,
) -> Result<Json<ActionResponse>, AppError> {
    #[cfg(windows)]
    {
        tokio::task::spawn_blocking(move || {
            let hwnd = win::find_hwnd(req.title.as_deref(), req.hwnd)
                .ok_or_else(|| AppError::not_found("Window not found"))?;
            win::do_close(hwnd);
            Ok(ActionResponse::ok())
        })
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
    }

    #[cfg(not(windows))]
    Err(AppError::not_implemented("Windows only"))
}

pub async fn minimize_window(
    Json(req): Json<WindowRequest>,
) -> Result<Json<ActionResponse>, AppError> {
    #[cfg(windows)]
    {
        tokio::task::spawn_blocking(move || {
            let hwnd = win::find_hwnd(req.title.as_deref(), req.hwnd)
                .ok_or_else(|| AppError::not_found("Window not found"))?;
            win::do_minimize(hwnd);
            Ok(ActionResponse::ok())
        })
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
    }

    #[cfg(not(windows))]
    Err(AppError::not_implemented("Windows only"))
}

pub async fn maximize_window(
    Json(req): Json<WindowRequest>,
) -> Result<Json<ActionResponse>, AppError> {
    #[cfg(windows)]
    {
        tokio::task::spawn_blocking(move || {
            let hwnd = win::find_hwnd(req.title.as_deref(), req.hwnd)
                .ok_or_else(|| AppError::not_found("Window not found"))?;
            win::do_maximize(hwnd);
            Ok(ActionResponse::ok())
        })
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
    }

    #[cfg(not(windows))]
    Err(AppError::not_implemented("Windows only"))
}

pub async fn resize_window(
    Json(req): Json<ResizeRequest>,
) -> Result<Json<ActionResponse>, AppError> {
    #[cfg(windows)]
    {
        tokio::task::spawn_blocking(move || {
            let hwnd = win::find_hwnd(req.title.as_deref(), req.hwnd)
                .ok_or_else(|| AppError::not_found("Window not found"))?;
            win::do_resize(hwnd, req.x, req.y, req.width, req.height);
            Ok(ActionResponse::ok())
        })
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
    }

    #[cfg(not(windows))]
    Err(AppError::not_implemented("Windows only"))
}
