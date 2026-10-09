//! Saying so when the application cannot start.
//!
//! Trove declares `LSUIElement`, so before its menu bar item exists it has no
//! presence on screen at all: no Dock tile, no window, no menu. A launch that
//! fails at that point used to end in a panic nobody saw — the user clicked the
//! icon and nothing happened. A modal alert is the one surface that still works
//! with nothing else built, so a refusal to start is reported through it.

/// What the person chose in the alert.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StartupAlertChoice {
    /// Reveal the data folder in Finder before quitting.
    ShowDataFolder,
    Quit,
}

#[cfg(target_os = "macos")]
mod platform {
    use std::path::Path;

    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplicationActivationOptions,
        NSRunningApplication, NSWorkspace,
    };
    use objc2_foundation::NSString;

    use super::StartupAlertChoice;

    /// Shows a blocking alert explaining why the application is not starting.
    ///
    /// The "Show data folder" button is offered only when there is a folder to
    /// show. Off the main thread there is no alert to show — AppKit refuses —
    /// and the answer is [`StartupAlertChoice::Quit`], which is what the caller
    /// does next anyway.
    pub fn show_startup_failure(
        message: &str,
        detail: &str,
        offer_data_folder: bool,
    ) -> StartupAlertChoice {
        let Some(mtm) = MainThreadMarker::new() else {
            return StartupAlertChoice::Quit;
        };
        // An accessory application is not active when it is launched, and a
        // modal alert from an inactive application opens behind whatever the
        // person was looking at — which is the silent failure this replaces.
        NSRunningApplication::currentApplication()
            .activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
        let alert = NSAlert::new(mtm);
        alert.setAlertStyle(NSAlertStyle::Critical);
        alert.setMessageText(&NSString::from_str(message));
        alert.setInformativeText(&NSString::from_str(detail));
        if offer_data_folder {
            alert.addButtonWithTitle(&NSString::from_str("Show data folder"));
        }
        alert.addButtonWithTitle(&NSString::from_str("Quit"));
        let response = alert.runModal();
        if offer_data_folder && response == NSAlertFirstButtonReturn {
            StartupAlertChoice::ShowDataFolder
        } else {
            StartupAlertChoice::Quit
        }
    }

    /// Selects a file or folder in a Finder window.
    pub fn reveal_in_finder(path: &Path) -> bool {
        let Some(path) = path.to_str() else {
            return false;
        };
        // An empty root asks Finder for a window on the item's own parent,
        // with the item selected — the folder itself, not its contents.
        NSWorkspace::sharedWorkspace().selectFile_inFileViewerRootedAtPath(
            Some(&NSString::from_str(path)),
            &NSString::from_str(""),
        )
    }
}

#[cfg(target_os = "macos")]
pub use platform::{reveal_in_finder, show_startup_failure};
