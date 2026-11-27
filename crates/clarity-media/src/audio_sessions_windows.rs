//! What is playing audio on Windows, for the per-application mix.
//!
//! Every active output device keeps an audio session per process that plays
//! (or recently played) through it; [`audio_sessions`] lists them. Process
//! loopback captures a process *tree*, so callers also need the process table
//! ([`running_processes`]): each process's executable, parent, and start time,
//! the last of which tells a live parent from a recycled process id.
//!
//! COM is entered for the duration of each call (multithreaded apartment, or
//! whatever apartment the calling thread already joined) and left on return.
#![allow(unsafe_code)]

use windows::Win32::Media::Audio::{
    AudioSessionStateExpired, DEVICE_STATE_ACTIVE, IAudioSessionControl2, IAudioSessionManager2,
    IMMDeviceEnumerator, MMDeviceEnumerator, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::core::Interface;
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// One process's audio session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSession {
    pub process_id: u32,
    /// The name the application gave its session, when it set a readable one.
    pub display_name: Option<String>,
}

/// One running process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub process_id: u32,
    pub parent_id: u32,
    /// The executable's file name without extension ("Discord", "chrome").
    pub executable: String,
    /// When the process started (FILETIME ticks), if this user may ask. A
    /// "parent" that started after its child is a recycled id, not a parent.
    pub created: Option<u64>,
}

/// The application audio sessions on every active output device, one per
/// process. Expired sessions and the shared system-sounds session are left
/// out; sessions that exist but are momentarily silent are kept, so an
/// application that pauses does not drop out of the mix and back.
pub fn audio_sessions() -> Result<Vec<AudioSession>, String> {
    let _apartment = ComApartment::enter();
    let mut sessions: Vec<AudioSession> = Vec::new();
    // SAFETY: COM calls on interfaces created and released inside this block,
    // which ends before the apartment guard leaves COM. Every string COM
    // hands back is copied and then freed with CoTaskMemFree.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(|error| error.to_string())?;
        let devices = enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .map_err(|error| error.to_string())?;
        for device_index in 0..devices.GetCount().map_err(|error| error.to_string())? {
            let Ok(device) = devices.Item(device_index) else {
                continue;
            };
            let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
                continue;
            };
            let Ok(list) = manager.GetSessionEnumerator() else {
                continue;
            };
            for session_index in 0..list.GetCount().unwrap_or(0) {
                let Ok(control) = list.GetSession(session_index) else {
                    continue;
                };
                let Ok(control) = control.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                // S_OK means this is the system-sounds session.
                if control.IsSystemSoundsSession().0 == 0 {
                    continue;
                }
                if control
                    .GetState()
                    .is_ok_and(|state| state == AudioSessionStateExpired)
                {
                    continue;
                }
                let Ok(process_id) = control.GetProcessId() else {
                    continue;
                };
                if process_id == 0
                    || sessions
                        .iter()
                        .any(|session| session.process_id == process_id)
                {
                    continue;
                }
                let display_name = control.GetDisplayName().ok().and_then(|name| {
                    let text = name.to_string().ok();
                    CoTaskMemFree(Some(name.0 as *const _));
                    text
                });
                sessions.push(AudioSession {
                    process_id,
                    display_name: display_name
                        .map(|name| name.trim().to_owned())
                        // "@%SystemRoot%\…,-123" is an unresolved resource
                        // reference, not a name anyone would recognise.
                        .filter(|name| !name.is_empty() && !name.starts_with('@')),
                });
            }
        }
    }
    Ok(sessions)
}

/// Every running process this user can see.
pub fn running_processes() -> Vec<ProcessInfo> {
    let mut found = Vec::new();
    // SAFETY: a process snapshot read entry by entry into a correctly sized
    // PROCESSENTRY32W, then closed.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return found;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let length = entry
                .szExeFile
                .iter()
                .position(|&unit| unit == 0)
                .unwrap_or(entry.szExeFile.len());
            let file = String::from_utf16_lossy(&entry.szExeFile[..length]);
            let executable = std::path::Path::new(&file)
                .file_stem()
                .map_or_else(|| file.clone(), |stem| stem.to_string_lossy().into_owned());
            found.push(ProcessInfo {
                process_id: entry.th32ProcessID,
                parent_id: entry.th32ParentProcessID,
                executable,
                created: creation_time(entry.th32ProcessID),
            });
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
    }
    found
}

fn creation_time(process: u32) -> Option<u64> {
    // SAFETY: a query-only handle, closed before returning; the four
    // FILETIMEs are plain out-parameters.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process);
        if handle.is_null() {
            return None;
        }
        let empty = || FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut created, mut exited, mut kernel, mut user) = (empty(), empty(), empty(), empty());
        let found = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user);
        CloseHandle(handle);
        (found != 0)
            .then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }
}

/// Enters COM for the current thread and leaves it on drop — but only if
/// this call was the one that entered. A thread already in a single-threaded
/// apartment refuses the multithreaded one, and COM stays usable as it is.
struct ComApartment {
    entered: bool,
}

impl ComApartment {
    fn enter() -> Self {
        // SAFETY: balanced by `Drop` whenever it succeeds (S_OK or S_FALSE).
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self {
            entered: result.is_ok(),
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.entered {
            // SAFETY: pairs with the successful CoInitializeEx in `enter`.
            unsafe { CoUninitialize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_in_the_process_table() {
        let own = std::process::id();
        let processes = running_processes();
        let me = processes
            .iter()
            .find(|process| process.process_id == own)
            .expect("the test process is running");
        assert!(
            me.created.is_some(),
            "a process may query its own start time"
        );
        assert!(!me.executable.is_empty());
    }

    #[test]
    fn audio_sessions_can_be_listed() {
        // Whatever is playing varies; listing must simply work.
        audio_sessions().expect("the audio sessions are readable");
    }
}
