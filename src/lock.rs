//! Whether this computer's session is locked.
//!
//! Polled rather than subscribed to, because the window may be parked in the
//! tray and the event hooks would need a window message loop of their own.

use std::time::Duration;

pub const POLL: Duration = Duration::from_secs(2);

/// `None` when the system cannot say.
pub fn session_locked() -> Option<bool> {
    #[cfg(windows)]
    {
        windows_locked()
    }
    #[cfg(target_os = "linux")]
    {
        linux_locked()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        None
    }
}

#[cfg(windows)]
fn windows_locked() -> Option<bool> {
    use std::ffi::c_void;
    #[link(name = "wtsapi32")]
    unsafe extern "system" {
        fn WTSQuerySessionInformationW(
            server: *mut c_void,
            session: u32,
            class: i32,
            buffer: *mut *mut u8,
            bytes: *mut u32,
        ) -> i32;
        fn WTSFreeMemory(memory: *mut c_void);
    }
    const CURRENT_SESSION: u32 = u32::MAX;
    const SESSION_INFO_EX: i32 = 25;
    const STATE_LOCK: i32 = 0;
    const STATE_UNLOCK: i32 = 1;
    // WTSINFOEXW is Level (u32) and then a union holding WTSINFOEX_LEVEL1_W.
    // The union has LARGE_INTEGER members, so it starts at byte 8. Inside it are
    // SessionId (u32), SessionState (i32), and SessionFlags (i32), at byte 16.
    const FLAGS_OFFSET: usize = 16;
    let mut buffer: *mut u8 = std::ptr::null_mut();
    let mut bytes = 0u32;
    unsafe {
        let ok = WTSQuerySessionInformationW(
            std::ptr::null_mut(),
            CURRENT_SESSION,
            SESSION_INFO_EX,
            &mut buffer,
            &mut bytes,
        );
        if ok == 0 || buffer.is_null() {
            return None;
        }
        let flags = if bytes as usize >= FLAGS_OFFSET + 4 {
            Some(std::ptr::read_unaligned(
                buffer.add(FLAGS_OFFSET).cast::<i32>(),
            ))
        } else {
            None
        };
        WTSFreeMemory(buffer.cast());
        match flags? {
            STATE_LOCK => Some(true),
            STATE_UNLOCK => Some(false),
            _ => None,
        }
    }
}

#[cfg(target_os = "linux")]
fn linux_locked() -> Option<bool> {
    let session = std::env::var("XDG_SESSION_ID")
        .ok()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| "auto".to_string());
    let output = std::process::Command::new("loginctl")
        .args(["show-session", &session, "-p", "LockedHint", "--value"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    match String::from_utf8_lossy(&output.stdout).trim() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}
