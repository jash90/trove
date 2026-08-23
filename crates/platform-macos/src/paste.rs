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
    use objc2_core_graphics::{
        CGEvent, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
    };

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

    unsafe extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }

    /// Sends Command-V to one process.
    ///
    /// Posted to the target's own event queue rather than to the system tap, so
    /// the keystroke cannot land in a window that came forward in between.
    pub fn post_paste_to(pid: i32) -> bool {
        let Some(source) = CGEventSource::new(CGEventSourceStateID::HIDSystemState) else {
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
        let _ = CGEventTapLocation::HIDEventTap;
        true
    }
}

#[cfg(target_os = "macos")]
pub use platform::{is_trusted, post_paste_to};

#[cfg(not(target_os = "macos"))]
pub fn is_trusted() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn post_paste_to(_pid: i32) -> bool {
    false
}
