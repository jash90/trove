//! The macOS clipboard adapter.
//!
//! macOS reports no event when the pasteboard changes, so the only way to
//! notice is to look. `changeCount` makes that cheap: it is a counter the
//! system bumps on every write, so an unchanged clipboard costs one integer
//! read and nothing else.

pub mod alert;
pub mod icons;
pub mod markers;
pub mod paste;
pub mod pasteboard;
pub mod symbolic_hotkeys;
pub mod window_snap;

pub use alert::StartupAlertChoice;
pub use icons::application_icon_png;
pub use markers::{MarkerPolicy, classify_types};
pub use paste::{
    PasteReadiness, is_trusted, open_accessibility_settings, post_paste_to, readiness,
    request_trust,
};
pub use pasteboard::{MAX_CAPTURED_PAYLOAD_BYTES, PollOutcome, snapshot_from_types};
pub use symbolic_hotkeys::{COMMAND_SPACE, Chord, SetOutcome, holders_of};
pub use window_snap::{SnapAction, SnapOutcome};

use std::time::Duration;

/// How often the pasteboard is looked at.
///
/// A person notices a delay of about a fifth of a second, and a poll costs one
/// integer read, so this is fast enough to feel immediate and cheap enough to
/// run all day.
pub const POLL_INTERVAL: Duration = Duration::from_millis(200);

#[cfg(target_os = "macos")]
mod platform {
    use objc2_app_kit::{
        NSApplicationActivationOptions, NSPasteboard, NSRunningApplication, NSWorkspace,
    };

    use trove_core::PlatformError;

    use crate::markers;
    use crate::pasteboard::{PollOutcome, snapshot_from_types};

    /// Watches the general pasteboard for changes.
    pub struct MacPasteboardWatcher {
        pasteboard: objc2::rc::Retained<NSPasteboard>,
        last_change_count: isize,
    }

    impl MacPasteboardWatcher {
        pub fn new() -> Self {
            let pasteboard = NSPasteboard::generalPasteboard();
            let last_change_count = pasteboard.changeCount();
            Self {
                pasteboard,
                last_change_count,
            }
        }

        /// The counter as it stands now, without reading anything else.
        pub fn change_count(&self) -> i64 {
            self.pasteboard.changeCount() as i64
        }

        /// Marks a change as ours so the echo of our own write is not recorded.
        pub fn acknowledge(&mut self, change_count: i64) {
            self.last_change_count = change_count as isize;
        }

        /// Looks once. Cheap when nothing was copied.
        pub fn poll(&mut self, observed_at_ms: i64) -> PollOutcome {
            let change_count = self.pasteboard.changeCount();
            if change_count == self.last_change_count {
                return PollOutcome::Unchanged;
            }
            self.last_change_count = change_count;

            let types = self.advertised_types();
            let policy = markers::classify_types(&types);
            if let Some(reason) = policy.reason() {
                return PollOutcome::Rejected {
                    change_count: change_count as i64,
                    reason,
                };
            }

            let mut read = |wanted: &str| self.read_type(wanted);
            let source = frontmost_application();
            match snapshot_from_types(&types, &mut read, source, observed_at_ms) {
                Ok(snapshot) => PollOutcome::Captured {
                    change_count: change_count as i64,
                    snapshot: Box::new(snapshot),
                },
                Err(PlatformError::ReadFailed) => PollOutcome::Rejected {
                    change_count: change_count as i64,
                    reason: "unreadable",
                },
                Err(_) => PollOutcome::Rejected {
                    change_count: change_count as i64,
                    reason: "unavailable",
                },
            }
        }

        fn advertised_types(&self) -> Vec<String> {
            let Some(types) = self.pasteboard.types() else {
                return Vec::new();
            };
            types.iter().map(|value| value.to_string()).collect()
        }

        fn read_type(&self, wanted: &str) -> Option<Vec<u8>> {
            let name = objc2_foundation::NSString::from_str(wanted);
            let data = self.pasteboard.dataForType(&name)?;
            Some(data.to_vec())
        }
    }

    impl Default for MacPasteboardWatcher {
        fn default() -> Self {
            Self::new()
        }
    }

    /// The process id of whatever is in front right now.
    ///
    /// Recorded before the palette appears so a paste can go back to the window
    /// the user was actually working in, rather than to whatever is frontmost
    /// once the palette has taken focus — which would be the palette itself.
    pub fn frontmost_pid() -> Option<i32> {
        let workspace = NSWorkspace::sharedWorkspace();
        let application = workspace.frontmostApplication()?;
        let pid = application.processIdentifier();
        (pid > 0).then_some(pid)
    }

    /// Brings the application with this process id back to the front.
    ///
    /// A synthesized Command-V goes to a process, but a process only routes it
    /// to a text field when it owns the key window. Hiding our palette does not
    /// reliably hand activation back in time, so the target is asked for it
    /// explicitly before the keystroke is posted.
    pub fn activate_pid(pid: i32) -> bool {
        let Some(application) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        else {
            return false;
        };
        // All windows rather than just the last one: the user is returning to
        // the document they were typing in, not to whichever panel that
        // application happened to raise last.
        application.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows)
    }

    /// The application in front when the copy happened.
    ///
    /// This is a guess, not a declaration — hence `SourceConfidence::Inferred`
    /// downstream. It is right almost always and wrong when something copies
    /// from the background, which is why the distinction is recorded rather
    /// than presented as fact.
    fn frontmost_application() -> Option<(String, String)> {
        let workspace = NSWorkspace::sharedWorkspace();
        let application = workspace.frontmostApplication()?;
        let identifier = application
            .bundleIdentifier()
            .map(|value| value.to_string());
        let name = application.localizedName().map(|value| value.to_string());
        // A command-line tool has no bundle identifier, and an application
        // bundle may withhold a localized name. Either alone still tells the
        // user where something came from, so the pair falls back to whichever
        // one is there rather than discarding both.
        match (identifier, name) {
            (Some(identifier), Some(name)) => Some((identifier, name)),
            (Some(identifier), None) => Some((identifier.clone(), identifier)),
            (None, Some(name)) => Some((name.clone(), name)),
            (None, None) => None,
        }
    }
}

#[cfg(target_os = "macos")]
pub use platform::{MacPasteboardWatcher, activate_pid, frontmost_pid};
