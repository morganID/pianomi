//! macOS frontmost-app helpers for the menu bar app.
//!
//! Pianomi is an `LSUIElement` / `ActivationPolicy::Accessory` app with no
//! windows, which makes focus unusually fragile:
//!
//! * `tao` calls `activateIgnoringOtherApps:YES` in
//!   `applicationDidFinishLaunching` unless `set_activate_ignoring_other_apps`
//!   is turned off — that alone steals focus from Roblox at launch.
//! * `rfd`'s synchronous `NSOpenPanel` activates our process to show the file
//!   picker. Its `FocusManager` restores `NSApp.keyWindow` afterwards, but a
//!   windowless status-item app has no key window, so nothing gets restored
//!   and Pianomi stays frontmost.
//!
//! Roblox only processes keyboard input while it is the active application, so
//! both cases silently kill playback. This module lets the GUI remember who was
//! frontmost before it interfered and hand focus back on the next event-loop
//! tick (after `NSMenu` has finished tracking).
//!
//! On non-macOS platforms these are no-ops; the other backends have no
//! equivalent per-process activation API.

/// PID of the frontmost application, or `0` when it cannot be determined.
pub fn frontmost_pid() -> i32 {
    #[cfg(target_os = "macos")]
    {
        mac::frontmost_pid()
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

/// Bring `pid` to the front. Returns `false` if the process no longer exists
/// or the activation was refused.
pub fn activate_pid(pid: i32) -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::activate_pid(pid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        false
    }
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)] // objc 0.2's `class!` macro probes a `cargo-clippy` cfg
mod mac {
    use objc::runtime::{Object, BOOL};
    use objc::{class, msg_send, sel, sel_impl};

    // Force AppKit to be linked: `NSRunningApplication` / `NSWorkspace` are
    // looked up by name at runtime, so an unreferenced framework would not be
    // loaded and `class!` would panic.
    #[link(name = "AppKit", kind = "framework")]
    extern "C" {}

    /// `NSApplicationActivateAllWindows | NSApplicationActivateIgnoringOtherApps`
    const ACTIVATE_ALL_WINDOWS_IGNORING_OTHERS: u64 = 1 | 2;

    pub fn frontmost_pid() -> i32 {
        unsafe {
            let ws: *mut Object = msg_send![class!(NSWorkspace), sharedWorkspace];
            if ws.is_null() {
                return 0;
            }
            let app: *mut Object = msg_send![ws, frontmostApplication];
            if app.is_null() {
                return 0;
            }
            msg_send![app, processIdentifier]
        }
    }

    pub fn activate_pid(pid: i32) -> bool {
        if pid <= 0 {
            return false;
        }
        unsafe {
            let app: *mut Object = msg_send![
                class!(NSRunningApplication),
                runningApplicationWithProcessIdentifier: pid
            ];
            if app.is_null() {
                return false;
            }
            let ok: BOOL =
                msg_send![app, activateWithOptions: ACTIVATE_ALL_WINDOWS_IGNORING_OTHERS];
            ok as i32 != 0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_pid_is_a_safe_no_op() {
        assert!(!activate_pid(0));
        assert!(!activate_pid(-1));
    }
}
