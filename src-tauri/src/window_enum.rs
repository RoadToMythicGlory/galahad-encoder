//! Top-level window enumeration.
//!
//! Lists visible, titled, top-level windows the player can pick as a capture
//! source. Each entry carries the HWND (as a stable `isize` id), title and owning
//! process name so the UI can show "cs2.exe — Counter-Strike 2" and the capture
//! engine can bind a WGC target to the HWND.

use serde::Serialize;

/// Stable identifier for a window across the Tauri boundary (the raw HWND value).
pub type WindowId = isize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    pub id: WindowId,
    pub title: String,
    pub process_name: String,
    pub pid: u32,
}

#[cfg(windows)]
mod imp {
    use super::{WindowInfo, WindowId};
    use std::ffi::c_void;

    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, MAX_PATH, TRUE};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindow, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
        IsWindowVisible, GW_OWNER,
    };

    pub fn list_windows() -> Vec<WindowInfo> {
        let mut windows: Vec<WindowInfo> = Vec::new();
        let ptr = &mut windows as *mut Vec<WindowInfo> as isize;
        unsafe {
            // EnumWindows returns Err if the callback ever returns FALSE; we
            // always return TRUE, so ignore the result.
            let _ = EnumWindows(Some(enum_proc), LPARAM(ptr));
        }
        windows.sort_by(|a, b| a.process_name.to_lowercase().cmp(&b.process_name.to_lowercase()));
        windows
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let windows = &mut *(lparam.0 as *mut Vec<WindowInfo>);

        // Only visible, top-level (no owner) windows with a title.
        if !IsWindowVisible(hwnd).as_bool() {
            return TRUE;
        }
        if !GetWindow(hwnd, GW_OWNER).unwrap_or(HWND(std::ptr::null_mut())).0.is_null() {
            // Has an owner -> tool/child window, skip.
            return TRUE;
        }
        let len = GetWindowTextLengthW(hwnd);
        if len == 0 {
            return TRUE;
        }
        let mut buf = vec![0u16; (len + 1) as usize];
        let copied = GetWindowTextW(hwnd, &mut buf);
        if copied == 0 {
            return TRUE;
        }
        let title = String::from_utf16_lossy(&buf[..copied as usize]);
        if title.trim().is_empty() {
            return TRUE;
        }

        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let process_name = process_name_for_pid(pid).unwrap_or_else(|| "unknown".to_string());

        windows.push(WindowInfo {
            id: hwnd.0 as WindowId,
            title,
            process_name,
            pid,
        });
        TRUE
    }

    fn process_name_for_pid(pid: u32) -> Option<String> {
        if pid == 0 {
            return None;
        }
        unsafe {
            let handle =
                OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = vec![0u16; MAX_PATH as usize];
            let mut size = buf.len() as u32;
            let result = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut size,
            );
            let _ = CloseHandle(handle);
            result.ok()?;
            let full = String::from_utf16_lossy(&buf[..size as usize]);
            Some(
                full.rsplit(['\\', '/'])
                    .next()
                    .unwrap_or(&full)
                    .to_string(),
            )
        }
    }

    /// Resolve an HWND id back to a live handle, validating it still exists.
    pub fn resolve(id: WindowId) -> Option<HWND> {
        let hwnd = HWND(id as *mut c_void);
        unsafe {
            if IsWindowVisible(hwnd).as_bool() {
                Some(hwnd)
            } else {
                None
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::WindowInfo;

    pub fn list_windows() -> Vec<WindowInfo> {
        Vec::new()
    }
}

/// List capturable top-level windows.
pub fn list_windows() -> Vec<WindowInfo> {
    imp::list_windows()
}

#[cfg(windows)]
pub use imp::resolve;
