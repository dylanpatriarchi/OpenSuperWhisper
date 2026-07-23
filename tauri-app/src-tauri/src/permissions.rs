//! Accessibility permission handling, the minimal port of
//! `PermissionsManager.swift` the Tauri app needs today: on macOS,
//! CGEvent-posted keystrokes (the paste) are silently dropped unless the app
//! is trusted for Accessibility, so the UI must be able to check and prompt.
//! Microphone consent needs no code here — the system prompt fires on first
//! capture, with the usage string from Info.plist.

#[cfg(target_os = "macos")]
pub fn accessibility_trusted() -> bool {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }
    unsafe { AXIsProcessTrusted() }
}

/// Checks trust and, when missing, asks macOS to show the system
/// "grant Accessibility" dialog (once per app per TCC reset).
#[cfg(target_os = "macos")]
pub fn request_accessibility() -> bool {
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;
    use std::ffi::c_void;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
        static kAXTrustedCheckOptionPrompt: *const c_void;
    }

    unsafe {
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt as *const _);
        let options =
            CFDictionary::from_CFType_pairs(&[(key.as_CFType(), CFBoolean::true_value().as_CFType())]);
        AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef() as *const c_void)
    }
}

#[cfg(not(target_os = "macos"))]
pub fn accessibility_trusted() -> bool {
    // Windows needs no special grant for SendInput (UIPI aside).
    true
}

#[cfg(not(target_os = "macos"))]
pub fn request_accessibility() -> bool {
    true
}
