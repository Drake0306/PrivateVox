//! Windows elevated-window warning (design §12). User Interface Privilege Isolation stops a normal process from
//! sending keystrokes into a window owned by an elevated (run as administrator) process, so the paste chord is
//! dropped there while the clipboard write still works. We poll the foreground window, warn in the pill while such a
//! window is in front (as the Mac does for secure input), and report a paste into it as "copied only".

use crate::pipeline::Cmd;
use std::sync::mpsc::Sender;
use std::time::Duration;

/// Pure rule: a paste is blocked when the target is elevated and we are not (an elevated PrivateVox can paste anywhere).
pub fn blocks_paste(target_elevated: bool, self_elevated: bool) -> bool {
    target_elevated && !self_elevated
}

/// Whether the window in front right now would refuse our paste chord. Always false off Windows.
#[cfg(windows)]
pub fn foreground_blocks_paste() -> bool {
    match (foreground_elevated(), self_elevated()) {
        (Some(target), Some(me)) => blocks_paste(target, me),
        _ => false,
    }
}

#[cfg(not(windows))]
pub fn foreground_blocks_paste() -> bool {
    false
}

/// Polls every second and reports changes to the coordinator, which shows or clears the pill warning.
#[cfg(windows)]
pub fn spawn_watch(tx: Sender<Cmd>) {
    std::thread::Builder::new()
        .name("privatevox-elevation".into())
        .spawn(move || {
            let mut last = false;
            loop {
                let now = foreground_blocks_paste();
                if now != last {
                    last = now;
                    log::info!("front window {}", if now { "runs as administrator: pasting into it is blocked" } else { "accepts pastes again" });
                    if tx.send(Cmd::ElevatedFront(now)).is_err() {
                        break;
                    }
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        })
        .expect("elevation watch thread");
}

#[cfg(not(windows))]
pub fn spawn_watch(_tx: Sender<Cmd>) {}

#[cfg(windows)]
fn self_elevated() -> Option<bool> {
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: the pseudo-handle needs no closing.
    unsafe { token_elevated(GetCurrentProcess()) }
}

/// Elevation of the process owning the foreground window. `None` when there is no foreground window or it cannot be
/// read; a token we are refused to open counts as elevated, since that refusal is itself the integrity barrier.
#[cfg(windows)]
fn foreground_elevated() -> Option<bool> {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    // SAFETY: plain Win32 calls; every handle we open is closed, every result is checked.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 || pid == std::process::id() {
            return Some(false);
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return Some(GetLastError() == ERROR_ACCESS_DENIED);
        }
        let result = token_elevated(process);
        CloseHandle(process);
        match result {
            Some(v) => Some(v),
            None => Some(GetLastError() == ERROR_ACCESS_DENIED),
        }
    }
}

/// `TokenElevation` of a process handle; `None` when the token cannot be opened or read.
#[cfg(windows)]
unsafe fn token_elevated(process: windows_sys::Win32::Foundation::HANDLE) -> Option<bool> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
    use windows_sys::Win32::System::Threading::OpenProcessToken;
    let mut token = std::ptr::null_mut();
    if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
        return None;
    }
    let mut info = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut returned: u32 = 0;
    let ok = GetTokenInformation(token, TokenElevation, &mut info as *mut TOKEN_ELEVATION as *mut _, std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut returned);
    CloseHandle(token);
    if ok == 0 {
        return None;
    }
    Some(info.TokenIsElevated != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_is_blocked_only_into_a_more_elevated_window() {
        assert!(blocks_paste(true, false), "normal PrivateVox, administrator window: UIPI drops the chord");
        assert!(!blocks_paste(true, true), "an elevated PrivateVox may paste into an elevated window");
        assert!(!blocks_paste(false, false));
        assert!(!blocks_paste(false, true));
    }

    #[test]
    fn this_process_can_read_its_own_elevation() {
        // Off Windows the probe is a constant; on Windows it must at least answer for ourselves.
        #[cfg(windows)]
        assert!(self_elevated().is_some());
        #[cfg(not(windows))]
        assert!(!foreground_blocks_paste());
    }
}
