//! macOS Accessibility (TCC) permission helpers.
//!
//! Sending synthetic key events requires the *responsible* process to be
//! trusted for Accessibility. When it is not, `CGEventPost` silently does
//! nothing — no error, no keystrokes, which is a miserable thing to debug.
//!
//! Two things matter here:
//!
//! 1. **Ask properly.** `AXIsProcessTrustedWithOptions` with
//!    `AXTrustedCheckOptionPrompt` makes macOS register the app in
//!    System Settings ▸ Privacy & Security ▸ Accessibility and show the
//!    "would like to control this computer" dialog. Without the prompt the app
//!    is not even listed, so the user has to hunt for it with the `+` button.
//!
//! 2. **Ask once per launch, not per keystroke.** Playback checks are cheap
//!    (`AXIsProcessTrusted` is a cached lookup) but a dialog every note would
//!    be unusable.
//!
//! On other platforms these are no-ops: the permission model differs and
//! enigo reports the problem itself.

/// Whether this process may post synthetic input events.
#[cfg(target_os = "macos")]
pub fn accessibility_trusted() -> bool {
    ax::trusted()
}

/// Ask macOS to show the Accessibility prompt (and register the app in the
/// Settings list). Returns whether access is already granted.
#[cfg(target_os = "macos")]
pub fn request_accessibility_prompt() -> bool {
    ax::request_prompt()
}

#[cfg(not(target_os = "macos"))]
pub fn accessibility_trusted() -> bool {
    true
}

#[cfg(not(target_os = "macos"))]
pub fn request_accessibility_prompt() -> bool {
    true
}

#[cfg(target_os = "macos")]
mod ax {
    use std::ffi::c_void;

    type CFTypeRef = *const c_void;
    type CFAllocatorRef = *const c_void;
    type Boolean = u8;
    type CFIndex = isize;

    /// `kCFStringEncodingUTF8`
    const UTF8: u32 = 0x0800_0100;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(
            alloc: CFAllocatorRef,
            c_str: *const i8,
            encoding: u32,
        ) -> CFTypeRef;
        fn CFDictionaryCreate(
            alloc: CFAllocatorRef,
            keys: *const CFTypeRef,
            values: *const CFTypeRef,
            num_values: CFIndex,
            key_callbacks: *const c_void,
            value_callbacks: *const c_void,
        ) -> CFTypeRef;
        fn CFRelease(cf: CFTypeRef);
        static kCFBooleanTrue: CFTypeRef;
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> Boolean;
        fn AXIsProcessTrustedWithOptions(options: CFTypeRef) -> Boolean;
    }

    pub fn trusted() -> bool {
        unsafe { AXIsProcessTrusted() != 0 }
    }

    pub fn request_prompt() -> bool {
        unsafe {
            let key = CFStringCreateWithCString(
                std::ptr::null(),
                b"AXTrustedCheckOptionPrompt\0".as_ptr() as *const i8,
                UTF8,
            );
            if key.is_null() {
                return trusted();
            }

            let keys = [key];
            let values = [kCFBooleanTrue];
            let options = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &kCFTypeDictionaryKeyCallBacks as *const c_void,
                &kCFTypeDictionaryValueCallBacks as *const c_void,
            );

            let granted = if options.is_null() {
                trusted()
            } else {
                let r = AXIsProcessTrustedWithOptions(options) != 0;
                CFRelease(options);
                r
            };
            CFRelease(key);
            granted
        }
    }
}
