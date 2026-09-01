//! Pasting into whatever the user was working in.
//!
//! Putting an entry on the clipboard is the easy half. The useful half is
//! landing it where the user was typing, which on macOS means synthesizing
//! Command-V. That is a privileged act: without Accessibility permission the
//! system silently discards the keystroke, so the outcome is reported rather
//! than assumed.

use clipboard_core::PasteOutcome;

/// Why a paste could not happen, decided before anything is posted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PasteReadiness {
    Ready,
    /// The system has not granted Accessibility permission.
    PermissionRequired,
    /// The window that had focus is gone, so there is nothing to paste into.
    TargetLost,
    /// This platform cannot synthesize a paste at all.
    PlatformLimit,
}

impl PasteReadiness {
    /// The outcome to report when a paste is not attempted.
    ///
    /// Every one of these still leaves the entry on the clipboard: the user can
    /// paste by hand, and saying so beats a silent no-op.
    pub fn outcome(self) -> Option<PasteOutcome> {
        match self {
            Self::Ready => None,
            Self::PermissionRequired => Some(PasteOutcome::CopiedOnlyPermissionRequired),
            Self::TargetLost => Some(PasteOutcome::CopiedOnlyTargetLost),
            Self::PlatformLimit => Some(PasteOutcome::CopiedOnlyPlatformLimit),
        }
    }
}

/// Decides whether a paste may be attempted.
pub fn readiness(trusted: bool, target_pid: Option<i32>) -> PasteReadiness {
    if !trusted {
        return PasteReadiness::PermissionRequired;
    }
    match target_pid {
        Some(pid) if pid > 0 => PasteReadiness::Ready,
        // Refusing here matters: posting to the whole system when the intended
        // window is gone would type into whatever happens to be in front.
        _ => PasteReadiness::TargetLost,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_outranks_everything_else() {
        assert_eq!(
            readiness(false, Some(42)),
            PasteReadiness::PermissionRequired
        );
        assert_eq!(readiness(false, None), PasteReadiness::PermissionRequired);
    }

    #[test]
    fn a_vanished_target_is_refused_rather_than_pasted_somewhere_else() {
        assert_eq!(readiness(true, None), PasteReadiness::TargetLost);
        assert_eq!(readiness(true, Some(0)), PasteReadiness::TargetLost);
        assert_eq!(readiness(true, Some(-1)), PasteReadiness::TargetLost);
    }

    #[test]
    fn a_trusted_process_with_a_live_target_may_paste() {
        assert_eq!(readiness(true, Some(42)), PasteReadiness::Ready);
    }

    #[test]
    fn every_refusal_still_leaves_the_entry_on_the_clipboard() {
        assert_eq!(PasteReadiness::Ready.outcome(), None);
        assert_eq!(
            PasteReadiness::PermissionRequired.outcome(),
            Some(PasteOutcome::CopiedOnlyPermissionRequired)
        );
        assert_eq!(
            PasteReadiness::TargetLost.outcome(),
            Some(PasteOutcome::CopiedOnlyTargetLost)
        );
        assert_eq!(
            PasteReadiness::PlatformLimit.outcome(),
            Some(PasteOutcome::CopiedOnlyPlatformLimit)
        );
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use objc2::rc::Retained;
    use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID};
    use objc2_foundation::{NSDictionary, NSNumber, NSString};

    /// Virtual key code for `V` on any layout, since the shortcut is defined by
    /// position rather than by the letter printed on the key.
    const KEY_V: u16 = 0x09;

    /// Whether this process may post events to other applications.
    ///
    /// Read-only: prompting belongs to a deliberate user action, not to a paste
    /// that would then be interrupted by a system dialog.
    pub fn is_trusted() -> bool {
        // SAFETY: AXIsProcessTrusted takes no arguments, returns a Boolean and
        // is documented as callable from any thread.
        unsafe { AXIsProcessTrusted() }
    }

    /// Asks the system for Accessibility permission, showing its dialog.
    ///
    /// Best-effort by nature, and the reason `open_accessibility_settings` exists
    /// beside it: macOS shows this dialog only while the application has no
    /// decision recorded against it. Once the user has answered — including by
    /// turning the switch back off — the call answers `false` and shows nothing,
    /// because the system will not ask twice on our behalf. So a caller that
    /// wants the user to actually get somewhere must fall back to opening the
    /// settings pane rather than trusting the prompt to appear.
    pub fn request_trust() -> bool {
        let value = NSNumber::new_bool(true);
        // SAFETY: `kAXTrustedCheckOptionPrompt` is a CFStringRef constant, and
        // CFString is toll-free bridged with NSString, so it may be used as a
        // dictionary key of that type.
        let key: &NSString = unsafe { &*kAXTrustedCheckOptionPrompt.cast::<NSString>() };
        let options: Retained<NSDictionary<NSString, NSNumber>> =
            NSDictionary::from_slices(&[key], &[&*value]);
        // SAFETY: NSDictionary is toll-free bridged with CFDictionary, and the
        // dictionary outlives the call.
        unsafe { AXIsProcessTrustedWithOptions(Retained::as_ptr(&options).cast()) }
    }

    /// Opens the Accessibility list in System Settings.
    ///
    /// The reliable half of asking: unlike the prompt, this works whatever the
    /// system has already recorded, and it lands the user on the one switch
    /// that decides whether pasting works.
    pub fn open_accessibility_settings() -> bool {
        // Passed as a single argument and never through a shell, exactly like
        // every other `open` in this application.
        std::process::Command::new("/usr/bin/open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> bool;
        fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
        static kAXTrustedCheckOptionPrompt: *const std::ffi::c_void;
    }

    /// Sends Command-V to one process.
    ///
    /// Posted to the target's own event queue rather than to the system tap, so
    /// the keystroke cannot land in a window that came forward in between.
    pub fn post_paste_to(pid: i32) -> bool {
        // A private source rather than the HID one: an HID-state source merges
        // the modifier keys that are physically down right now, so the Shift of
        // a Command-Shift-V shortcut rode along into the synthesized chord and
        // the target received Command-Shift-V instead of Command-V.
        let Some(source) = CGEventSource::new(CGEventSourceStateID::Private) else {
            return false;
        };
        let Some(key_down) = CGEvent::new_keyboard_event(Some(&source), KEY_V, true) else {
            return false;
        };
        let Some(key_up) = CGEvent::new_keyboard_event(Some(&source), KEY_V, false) else {
            return false;
        };
        CGEvent::set_flags(Some(&key_down), CGEventFlags::MaskCommand);
        CGEvent::set_flags(Some(&key_up), CGEventFlags::MaskCommand);
        CGEvent::post_to_pid(pid, Some(&key_down));
        CGEvent::post_to_pid(pid, Some(&key_up));
        true
    }
}

#[cfg(target_os = "macos")]
pub use platform::{is_trusted, open_accessibility_settings, post_paste_to, request_trust};

#[cfg(not(target_os = "macos"))]
pub fn is_trusted() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn request_trust() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn open_accessibility_settings() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn post_paste_to(_pid: i32) -> bool {
    false
}
