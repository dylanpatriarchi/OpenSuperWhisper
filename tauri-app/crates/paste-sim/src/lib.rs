//! Port of `Utils/ClipboardUtil.swift`: put text on the clipboard, simulate
//! the paste keystroke into the frontmost app, and restore the user's
//! original clipboard afterwards without clobbering anything they copied in
//! the meantime.
//!
//! The OS-agnostic half — restore-delay, burst coalescing ("only the first
//! dictation in a burst captures the original clipboard"), and the
//! took-over guard — lives in [`Paster`], written once against the
//! [`Clipboard`]/[`KeySender`] traits and unit-tested with mocks. The Swift
//! version guards restores with `NSPasteboard.changeCount`; `arboard` exposes
//! no change counter, so the guard compares clipboard text against what we
//! pasted — equivalent unless the user copies the identical string within
//! the restore window. (TODO: swap in a real change-count via objc2-app-kit
//! on macOS / GetClipboardSequenceNumber on Windows when the native layers
//! grow.)
//!
//! The macOS keystroke half posts Cmd+V through a CGEvent, hardcoding ANSI
//! keycode 9 ('V'). The Swift original resolves the keycode through the
//! active keyboard layout (`UCKeyTranslate`) for Dvorak-style layouts; that
//! refinement is deferred (TODO) — Cmd+V at keycode 9 is correct on QWERTY
//! and layout-translated automatically by many apps.

use std::sync::Mutex;
use std::time::Duration;

/// Mirrors `ClipboardUtil.clipboardRestoreDelay`.
pub const RESTORE_DELAY: Duration = Duration::from_millis(1500);

#[derive(Debug, thiserror::Error)]
pub enum PasteError {
    #[error("clipboard access failed: {0}")]
    Clipboard(String),
    #[error("failed to synthesize the paste keystroke: {0}")]
    Keystroke(String),
    #[error("paste simulation is not supported on this platform yet")]
    Unsupported,
}

pub trait Clipboard: Send {
    /// Current clipboard text, if any.
    fn get_text(&mut self) -> Option<String>;
    fn set_text(&mut self, text: &str) -> Result<(), PasteError>;
}

pub trait KeySender: Send {
    fn send_paste(&mut self) -> Result<(), PasteError>;
}

struct Pending {
    /// Clipboard contents from before the first paste of a burst; `None`
    /// means the clipboard was empty/non-text.
    original: Option<String>,
    /// What we last put on the clipboard — the restore guard.
    last_pasted: String,
}

/// The OS-agnostic paste/restore state machine. The caller owns the timing:
/// after each [`Paster::insert_text`], schedule a [`Paster::restore_if_unchanged`]
/// call [`RESTORE_DELAY`] later (each new paste supersedes the previous
/// deadline, matching the Swift `DispatchWorkItem` cancellation).
pub struct Paster<C: Clipboard, K: KeySender> {
    clipboard: Mutex<C>,
    keys: Mutex<K>,
    pending: Mutex<Option<Pending>>,
}

impl<C: Clipboard, K: KeySender> Paster<C, K> {
    pub fn new(clipboard: C, keys: K) -> Self {
        Self {
            clipboard: Mutex::new(clipboard),
            keys: Mutex::new(keys),
            pending: Mutex::new(None),
        }
    }

    /// Puts `text` on the clipboard and synthesizes the paste keystroke.
    /// Only the first paste of a burst captures the pre-burst clipboard, so
    /// back-to-back dictations restore the user's real clipboard, not the
    /// previous dictation.
    pub fn insert_text(&self, text: &str) -> Result<(), PasteError> {
        let mut clipboard = self.clipboard.lock().unwrap();
        let mut pending = self.pending.lock().unwrap();

        let original = match pending.take() {
            Some(p) => p.original, // burst: keep the first capture
            None => clipboard.get_text(),
        };
        clipboard.set_text(text)?;
        *pending = Some(Pending {
            original,
            last_pasted: text.to_string(),
        });
        drop(pending);
        drop(clipboard);

        self.keys.lock().unwrap().send_paste()
    }

    /// Restores the pre-burst clipboard if nothing else has taken the
    /// clipboard over since our paste. Returns whether a restore happened.
    /// A no-op when a newer `insert_text` superseded this deadline's state
    /// only if the caller cancels stale timers; calling it late is still
    /// safe — the guard compares against the *latest* pasted text.
    pub fn restore_if_unchanged(&self) -> Result<bool, PasteError> {
        let mut clipboard = self.clipboard.lock().unwrap();
        let mut pending = self.pending.lock().unwrap();

        let Some(p) = pending.take() else {
            return Ok(false);
        };
        // Someone (or something) replaced the clipboard after our paste:
        // their contents win, the saved original is dropped.
        if clipboard.get_text().as_deref() != Some(p.last_pasted.as_str()) {
            return Ok(false);
        }
        match &p.original {
            Some(text) => {
                clipboard.set_text(text)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

// ---------------------------------------------------------------------------
// Real backends
// ---------------------------------------------------------------------------

pub struct ArboardClipboard(arboard::Clipboard);

impl ArboardClipboard {
    pub fn new() -> Result<Self, PasteError> {
        arboard::Clipboard::new()
            .map(Self)
            .map_err(|e| PasteError::Clipboard(e.to_string()))
    }
}

impl Clipboard for ArboardClipboard {
    fn get_text(&mut self) -> Option<String> {
        self.0.get_text().ok()
    }

    fn set_text(&mut self, text: &str) -> Result<(), PasteError> {
        self.0
            .set_text(text.to_string())
            .map_err(|e| PasteError::Clipboard(e.to_string()))
    }
}

/// Synthesizes the platform paste chord into the frontmost app.
pub struct SystemKeySender;

#[cfg(target_os = "macos")]
impl KeySender for SystemKeySender {
    fn send_paste(&mut self) -> Result<(), PasteError> {
        use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
        use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

        const ANSI_V: u16 = 9;

        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| PasteError::Keystroke("CGEventSource creation failed".into()))?;

        for key_down in [true, false] {
            let event = CGEvent::new_keyboard_event(source.clone(), ANSI_V, key_down)
                .map_err(|_| PasteError::Keystroke("CGEvent creation failed".into()))?;
            event.set_flags(CGEventFlags::CGEventFlagCommand);
            event.post(CGEventTapLocation::HID);
        }
        Ok(())
    }
}

/// Ctrl+V via `SendInput`. Unlike macOS, no keyboard-layout keycode lookup
/// is needed: `VK_V` is a virtual key, which Windows maps through the active
/// layout itself. Compiled and tested by CI on windows-latest; needs a
/// manual smoke test on real Windows (see docs/TAURI_REWRITE.md phase 3).
#[cfg(target_os = "windows")]
impl KeySender for SystemKeySender {
    fn send_paste(&mut self) -> Result<(), PasteError> {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
            KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_V,
        };

        fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: vk,
                        wScan: 0,
                        dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            }
        }

        let inputs = [
            key(VK_CONTROL, false),
            key(VK_V, false),
            key(VK_V, true),
            key(VK_CONTROL, true),
        ];
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent != inputs.len() as u32 {
            return Err(PasteError::Keystroke(format!(
                "SendInput injected {sent}/{} events",
                inputs.len()
            )));
        }
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl KeySender for SystemKeySender {
    fn send_paste(&mut self) -> Result<(), PasteError> {
        Err(PasteError::Unsupported)
    }
}

pub type SystemPaster = Paster<ArboardClipboard, SystemKeySender>;

/// Convenience constructor for the real backends.
pub fn system_paster() -> Result<SystemPaster, PasteError> {
    Ok(Paster::new(ArboardClipboard::new()?, SystemKeySender))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeClipboard {
        text: Option<String>,
    }

    impl Clipboard for FakeClipboard {
        fn get_text(&mut self) -> Option<String> {
            self.text.clone()
        }
        fn set_text(&mut self, text: &str) -> Result<(), PasteError> {
            self.text = Some(text.to_string());
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeKeys {
        pastes: usize,
    }

    impl KeySender for FakeKeys {
        fn send_paste(&mut self) -> Result<(), PasteError> {
            self.pastes += 1;
            Ok(())
        }
    }

    fn paster_with(initial: Option<&str>) -> Paster<FakeClipboard, FakeKeys> {
        Paster::new(
            FakeClipboard {
                text: initial.map(str::to_string),
            },
            FakeKeys::default(),
        )
    }

    fn clipboard_text<K: KeySender>(p: &Paster<FakeClipboard, K>) -> Option<String> {
        p.clipboard.lock().unwrap().text.clone()
    }

    #[test]
    fn paste_and_restore_round_trip() {
        let p = paster_with(Some("user data"));
        p.insert_text("dictation").unwrap();
        assert_eq!(clipboard_text(&p).as_deref(), Some("dictation"));
        assert!(p.restore_if_unchanged().unwrap());
        assert_eq!(clipboard_text(&p).as_deref(), Some("user data"));
        assert_eq!(p.keys.lock().unwrap().pastes, 1);
    }

    #[test]
    fn burst_restores_pre_burst_clipboard_not_first_dictation() {
        let p = paster_with(Some("user data"));
        p.insert_text("first").unwrap();
        p.insert_text("second").unwrap(); // within the restore window
        assert!(p.restore_if_unchanged().unwrap());
        assert_eq!(clipboard_text(&p).as_deref(), Some("user data"));
    }

    #[test]
    fn user_copy_after_paste_wins_over_restore() {
        let p = paster_with(Some("user data"));
        p.insert_text("dictation").unwrap();
        // The user copies something new before the restore fires.
        p.clipboard.lock().unwrap().text = Some("fresh copy".into());
        assert!(!p.restore_if_unchanged().unwrap());
        assert_eq!(clipboard_text(&p).as_deref(), Some("fresh copy"));
    }

    #[test]
    fn empty_original_clipboard_is_not_restored() {
        let p = paster_with(None);
        p.insert_text("dictation").unwrap();
        assert!(!p.restore_if_unchanged().unwrap());
        assert_eq!(clipboard_text(&p).as_deref(), Some("dictation"));
    }

    #[test]
    fn restore_without_paste_is_a_no_op() {
        let p = paster_with(Some("user data"));
        assert!(!p.restore_if_unchanged().unwrap());
        assert_eq!(clipboard_text(&p).as_deref(), Some("user data"));
    }

    #[test]
    fn second_restore_is_a_no_op() {
        let p = paster_with(Some("user data"));
        p.insert_text("dictation").unwrap();
        assert!(p.restore_if_unchanged().unwrap());
        assert!(!p.restore_if_unchanged().unwrap());
        assert_eq!(clipboard_text(&p).as_deref(), Some("user data"));
    }
}
