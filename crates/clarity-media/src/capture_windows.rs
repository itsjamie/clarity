//! Windows screen capture: the source list for the in-app picker, and the
//! capture the broadcast turns into a `d3d11screencapturesrc`.
//!
//! Windows offers desktop applications no system picker comparable to the
//! Linux ScreenCast portal, so the application lists displays and windows
//! itself ([`capture_sources`]) and names the chosen one in
//! [`CaptureRequest::target`]. Capture runs on Windows Graphics Capture, which
//! handles whole displays and single windows alike, works whichever GPU drives
//! the display, and can draw the cursor into the frames.
//!
//! The Win32 calls here are plain queries on handles the system just handed
//! out. A display or window that disappears between enumeration and use makes
//! a call fail rather than misbehave, so every call is checked and failures
//! drop the source.
#![allow(unsafe_code)]

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::{
    DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW,
    HDC, HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MONITORINFOEXW, MonitorFromPoint,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetClassNameW, GetWindow, GetWindowLongPtrW,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, MONITORINFOF_PRIMARY, WS_EX_TOOLWINDOW,
};
use windows_sys::core::BOOL;

/// One thing that can be shared: a whole display or a single window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaptureTarget {
    /// A whole display, by its `HMONITOR`.
    Display(u64),
    /// One top-level window, by its `HWND`.
    Window(u64),
}

/// A capture target as the picker presents it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSource {
    pub target: CaptureTarget,
    /// The primary label: "Display 1", or the window's title.
    pub name: String,
    /// The secondary label: a display's mode ("2560 × 1440 · 144 Hz ·
    /// primary"), or the application that owns a window.
    pub detail: String,
}

#[derive(Debug, Clone, Default)]
pub struct CaptureRequest {
    pub show_cursor: bool,
    /// Accepted for parity with the portal-based platforms. Windows capture
    /// holds no grant, so there is nothing to restore.
    pub restore_token: Option<String>,
    /// Accepted for parity, as above: there is no grant to remember.
    pub remember: bool,
    /// The display or window to capture, chosen from [`capture_sources`].
    /// `None` captures the primary display.
    pub target: Option<CaptureTarget>,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("screen capture is not available on this system")]
    Unavailable,
    #[error("the screen selection was cancelled")]
    Cancelled,
    #[error("screen capture could not start: {0}")]
    Failed(String),
}

/// A resolved capture target. Unlike the portal's grant on Linux, nothing is
/// held open while this value exists: the broadcast's capture element opens
/// the display or window itself and releases it when it stops.
#[derive(Debug)]
pub struct CaptureStream {
    pub(crate) target: CaptureTarget,
    pub(crate) show_cursor: bool,
}

impl CaptureStream {
    /// Resolves the request's target, the primary display when none was
    /// chosen, and checks that it still exists. Returns at once: on Windows
    /// the picker is the caller's own UI.
    pub async fn open(request: CaptureRequest) -> Result<Self, CaptureError> {
        let target = request.target.unwrap_or_else(primary_display);
        if !target.is_live() {
            return Err(CaptureError::Failed(match target {
                CaptureTarget::Display(_) => "the selected display is no longer connected".into(),
                CaptureTarget::Window(_) => "the selected window has closed".into(),
            }));
        }
        Ok(Self {
            target,
            show_cursor: request.show_cursor,
        })
    }

    /// Always `None`: Windows capture has no restorable grant.
    pub fn restore_token(&self) -> Option<&str> {
        None
    }
}

impl CaptureTarget {
    fn is_live(self) -> bool {
        match self {
            Self::Display(handle) => monitor_info(from_bits(handle)).is_some(),
            // SAFETY: IsWindow accepts any value and reports whether it names
            // a live window.
            Self::Window(handle) => unsafe { IsWindow(from_bits(handle)) != 0 },
        }
    }
}

/// Everything the picker can offer, displays first (the primary display as
/// "Display 1"), then windows front to back. This process's own windows are
/// left out, as are hidden, cloaked, owned, and tool windows that a user would
/// not recognise as something to share.
pub fn capture_sources() -> Vec<CaptureSource> {
    let mut sources = displays();
    sources.extend(windows());
    sources
}

fn displays() -> Vec<CaptureSource> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _context: HDC,
        _bounds: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        // SAFETY: `data` is the `Vec` passed to EnumDisplayMonitors below,
        // which outlives the enumeration; the callback runs synchronously on
        // the calling thread.
        let monitors = unsafe { &mut *(data as *mut Vec<HMONITOR>) };
        monitors.push(monitor);
        1
    }
    let mut monitors: Vec<HMONITOR> = Vec::new();
    // SAFETY: the callback only pushes into `monitors`, alive for the call.
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(collect),
            &raw mut monitors as LPARAM,
        );
    }

    let mut found: Vec<(bool, CaptureTarget, String)> = monitors
        .into_iter()
        .filter_map(|monitor| {
            let info = monitor_info(monitor)?;
            let primary = info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0;
            let mut detail = match display_mode(&info.szDevice) {
                Some((width, height, hertz)) if hertz > 1 => {
                    format!("{width} × {height} · {hertz} Hz")
                }
                Some((width, height, _)) => format!("{width} × {height}"),
                None => {
                    let bounds = info.monitorInfo.rcMonitor;
                    format!(
                        "{} × {}",
                        bounds.right - bounds.left,
                        bounds.bottom - bounds.top
                    )
                }
            };
            if primary {
                detail.push_str(" · primary");
            }
            Some((primary, CaptureTarget::Display(to_bits(monitor)), detail))
        })
        .collect();
    // The primary display first: it is what "share my screen" usually means.
    found.sort_by_key(|(primary, _, _)| !primary);
    found
        .into_iter()
        .enumerate()
        .map(|(index, (_, target, detail))| CaptureSource {
            target,
            name: format!("Display {}", index + 1),
            detail,
        })
        .collect()
}

fn windows() -> Vec<CaptureSource> {
    unsafe extern "system" fn collect(window: HWND, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the `Vec` passed to EnumWindows below, which
        // outlives the synchronous enumeration.
        let windows = unsafe { &mut *(data as *mut Vec<HWND>) };
        windows.push(window);
        1
    }
    let mut handles: Vec<HWND> = Vec::new();
    // SAFETY: the callback only pushes into `handles`, alive for the call.
    unsafe {
        EnumWindows(Some(collect), &raw mut handles as LPARAM);
    }
    // SAFETY: a plain query with no arguments.
    let own_process = unsafe { GetCurrentProcessId() };
    handles
        .into_iter()
        .filter_map(|window| shareable_window(window, own_process))
        .collect()
}

/// The picker entry for a top-level window, or `None` for windows a user
/// would not think of as shareable.
fn shareable_window(window: HWND, own_process: u32) -> Option<CaptureSource> {
    // SAFETY: every call below is a query on a handle EnumWindows just
    // produced; a window that has since closed makes the calls fail, which
    // the checks turn into `None`.
    unsafe {
        if IsWindowVisible(window) == 0 || !GetWindow(window, GW_OWNER).is_null() {
            return None;
        }
        let extended_style = GetWindowLongPtrW(window, GWL_EXSTYLE) as u32;
        if extended_style & WS_EX_TOOLWINDOW != 0 {
            return None;
        }
        // Cloaked windows are "visible" but not shown: suspended UWP apps,
        // windows on other virtual desktops.
        let mut cloaked: u32 = 0;
        let result = DwmGetWindowAttribute(
            window,
            DWMWA_CLOAKED as u32,
            (&raw mut cloaked).cast::<c_void>(),
            size_of::<u32>() as u32,
        );
        if result >= 0 && cloaked != 0 {
            return None;
        }
        let mut process = 0u32;
        GetWindowThreadProcessId(window, &mut process);
        // Sharing Clarity's own window would show the share inside itself.
        if process == own_process {
            return None;
        }
        // The desktop and the taskbar are part of every display share, not
        // windows in their own right.
        if matches!(
            class_name(window).as_deref(),
            Some("Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd")
        ) {
            return None;
        }
        let name = window_title(window)?;
        let application = process_name(process);
        // Windows Graphics Capture delivers no frames from a minimized
        // window, so say so up front.
        let minimized = IsIconic(window) != 0;
        let detail = match (application, minimized) {
            (Some(application), true) => format!("{application} · minimized"),
            (Some(application), false) => application,
            (None, true) => "minimized".to_owned(),
            (None, false) => "window".to_owned(),
        };
        Some(CaptureSource {
            target: CaptureTarget::Window(to_bits(window)),
            name,
            detail,
        })
    }
}

fn primary_display() -> CaptureTarget {
    // SAFETY: a plain query; MONITOR_DEFAULTTOPRIMARY always yields a monitor.
    let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    CaptureTarget::Display(to_bits(monitor))
}

fn monitor_info(monitor: HMONITOR) -> Option<MONITORINFOEXW> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `info` is a MONITORINFOEXW whose cbSize says so, which is how
    // GetMonitorInfoW knows it may also fill in the device name.
    let found = unsafe { GetMonitorInfoW(monitor, (&raw mut info).cast::<MONITORINFO>()) };
    (found != 0).then_some(info)
}

/// The display's current mode: width, height, and refresh rate.
fn display_mode(device: &[u16; 32]) -> Option<(u32, u32, u32)> {
    let mut mode = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    // SAFETY: `device` is the NUL-terminated name GetMonitorInfoW filled in,
    // and `mode` declares its own size.
    let found = unsafe { EnumDisplaySettingsW(device.as_ptr(), ENUM_CURRENT_SETTINGS, &mut mode) };
    (found != 0).then_some((mode.dmPelsWidth, mode.dmPelsHeight, mode.dmDisplayFrequency))
}

fn window_title(window: HWND) -> Option<String> {
    // SAFETY: queries on a live handle, writing at most `buffer.len()` UTF-16
    // units into `buffer`.
    let length = unsafe { GetWindowTextLengthW(window) };
    if length <= 0 {
        return None;
    }
    let mut buffer = vec![0u16; length as usize + 1];
    let copied = unsafe { GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as i32) };
    let title = String::from_utf16_lossy(&buffer[..copied.max(0) as usize]);
    let title = title.trim();
    (!title.is_empty()).then(|| title.to_owned())
}

fn class_name(window: HWND) -> Option<String> {
    let mut buffer = [0u16; 256];
    // SAFETY: writes at most `buffer.len()` UTF-16 units into `buffer`.
    let copied = unsafe { GetClassNameW(window, buffer.as_mut_ptr(), buffer.len() as i32) };
    (copied > 0).then(|| String::from_utf16_lossy(&buffer[..copied as usize]))
}

/// The executable's file name without extension ("chrome", "Code"). `None`
/// for processes this user may not inspect, such as elevated ones.
fn process_name(process: u32) -> Option<String> {
    // SAFETY: a query-only handle, closed before returning; the name is
    // written into `buffer`, whose length `size` declares.
    let path = unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process);
        if handle.is_null() {
            return None;
        }
        let mut buffer = [0u16; 1024];
        let mut size = buffer.len() as u32;
        let found =
            QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut size);
        CloseHandle(handle);
        if found == 0 {
            return None;
        }
        String::from_utf16_lossy(&buffer[..size as usize])
    };
    std::path::Path::new(&path)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
}

fn to_bits(handle: *mut c_void) -> u64 {
    handle as usize as u64
}

fn from_bits(bits: u64) -> *mut c_void {
    bits as usize as *mut c_void
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_primary_display_is_listed_first_and_resolves() {
        let sources = capture_sources();
        let first = sources.first().expect("at least one display");
        assert_eq!(first.name, "Display 1");
        assert!(first.detail.ends_with("primary"), "{}", first.detail);
        assert_eq!(first.target, primary_display());
    }

    #[test]
    fn an_unknown_window_is_reported_closed() {
        assert!(!CaptureTarget::Window(0).is_live());
    }
}
