//! The global shortcut that summons the palette.
//!
//! A clipboard manager is only useful if it is one keystroke away from whatever
//! the user is doing, so the shortcut is registered for the whole system rather
//! than for the window. Pressing it again puts the palette away, which keeps
//! the same key in charge of both directions instead of forcing a reach for the
//! mouse or Escape.

use std::sync::{
    Arc,
    atomic::{AtomicI32, Ordering},
};

use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// The label of the window the shortcut toggles.
const PALETTE_WINDOW: &str = "main";

/// The label of the settings window.
///
/// Declared in the configuration and started hidden, so showing it is a matter
/// of asking rather than of building one — and it keeps its own size and
/// position between openings.
const SETTINGS_WINDOW: &str = "settings";

/// Brings the settings window up, wherever it was last left.
///
/// Separate from the palette on purpose: settings are read and edited slowly,
/// and doing that on top of the list meant the list could not be consulted
/// while doing it.
pub fn show_settings<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window(SETTINGS_WINDOW) else {
        return;
    };
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
}

/// The window the user was working in before the palette appeared.
///
/// Recorded on the way in, because once the palette has focus the frontmost
/// application is the palette, and pasting into ourselves helps nobody.
#[derive(Clone, Debug, Default)]
pub struct PasteTarget {
    pid: Arc<AtomicI32>,
}

impl PasteTarget {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records where the user was, unless that is us.
    ///
    /// The frontmost application is not always somebody else: a palette shown
    /// from the menu bar while it already had focus would record our own
    /// process, and `readiness` would then happily paste into the palette. The
    /// previous target is worth more than that, so a self-reference is dropped
    /// rather than stored.
    pub fn remember(&self, pid: Option<i32>) {
        if pid == Some(std::process::id() as i32) {
            return;
        }
        self.pid.store(pid.unwrap_or_default(), Ordering::Relaxed);
    }

    /// The target as it stands, left in place.
    ///
    /// Reading used to consume: the first Enter after a summon pasted and every
    /// one after it reported that the target was gone, because the pid had been
    /// swapped out from under it. The target belongs to the summon, not to a
    /// single paste, so it is cleared when the palette goes away instead.
    pub fn current(&self) -> Option<i32> {
        let pid = self.pid.load(Ordering::Relaxed);
        (pid > 0).then_some(pid)
    }

    /// Drops the target, because the palette is no longer standing in front of
    /// anything. The next summon records where the user actually was.
    pub fn forget(&self) {
        self.pid.store(0, Ordering::Relaxed);
    }
}

/// Whether the system has already been asked for Accessibility permission.
///
/// macOS shows its Accessibility dialog at most once per launch and silently
/// skips it afterwards, so asking again would be a no-op the user reads as the
/// application ignoring them. One ask per run, then the settings pane instead.
#[derive(Clone, Debug, Default)]
pub struct PastePrompt {
    asked: Arc<std::sync::atomic::AtomicBool>,
}

impl PastePrompt {
    pub fn new() -> Self {
        Self::default()
    }

    /// True the first time it is called in a process, false forever after.
    pub fn claim_first_ask(&self) -> bool {
        !self.asked.swap(true, Ordering::Relaxed)
    }
}

/// Cmd+Shift+Space on macOS, Ctrl+Shift+Space elsewhere.
///
/// Space is what a launcher-style palette is expected to answer to, and the
/// added Shift keeps it clear of the input-source switcher and of Spotlight.
pub fn default_shortcut() -> Shortcut {
    Shortcut::new(Some(Modifiers::SUPER | Modifiers::SHIFT), Code::Space)
}

/// Registers the shortcut and wires it to the palette.
///
/// A failure here is not fatal: the application still works from its window, so
/// the caller reports the degraded state rather than refusing to start. The
/// most common cause is another application holding the same combination.
pub fn install<R: Runtime>(app: &AppHandle<R>) -> Result<(), tauri_plugin_global_shortcut::Error> {
    register(app, default_shortcut())
}

/// Parses a shortcut written the way the settings screen stores it.
///
/// Returns nothing for anything unparseable rather than falling back to the
/// default: silently registering a different shortcut than the one saved would
/// leave the user pressing keys that do nothing, with no way to tell why.
pub fn parse_shortcut(value: &str) -> Option<Shortcut> {
    value.parse::<Shortcut>().ok()
}

/// Replaces the registered shortcut with another one.
///
/// The old registration goes first: leaving it in place would keep answering
/// keys the user has already changed away from.
pub fn rebind<R: Runtime>(
    app: &AppHandle<R>,
    previous: Shortcut,
    next: Shortcut,
) -> Result<(), tauri_plugin_global_shortcut::Error> {
    if previous != next {
        let _ = app.global_shortcut().unregister(previous);
    }
    register(app, next)
}

fn register<R: Runtime>(
    app: &AppHandle<R>,
    shortcut: Shortcut,
) -> Result<(), tauri_plugin_global_shortcut::Error> {
    app.global_shortcut()
        .on_shortcut(shortcut, move |app, _shortcut, event| {
            // Act on press only: acting on release too would toggle twice per
            // keystroke and leave the palette exactly where it started.
            if event.state() == ShortcutState::Pressed {
                toggle_palette(app);
            }
        })
}

/// The shortcut currently answering, so a rebind knows what to take down.
#[derive(Clone, Debug)]
pub struct ActiveShortcut {
    current: Arc<std::sync::Mutex<Shortcut>>,
}

impl ActiveShortcut {
    pub fn new(shortcut: Shortcut) -> Self {
        Self {
            current: Arc::new(std::sync::Mutex::new(shortcut)),
        }
    }

    pub fn get(&self) -> Shortcut {
        self.current
            .lock()
            .map(|current| *current)
            .unwrap_or_else(|_| default_shortcut())
    }

    pub fn set(&self, shortcut: Shortcut) {
        if let Ok(mut current) = self.current.lock() {
            *current = shortcut;
        }
    }
}

impl Default for ActiveShortcut {
    fn default() -> Self {
        Self::new(default_shortcut())
    }
}

/// Shows and focuses the palette, or hides it when it already has focus.
pub fn toggle_palette<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window(PALETTE_WINDOW) else {
        return;
    };
    // A window can be visible but buried behind other applications. Treating
    // that as "already open" would hide it just as the user asked to see it,
    // so the shortcut only puts it away when it is genuinely in front.
    let is_frontmost = window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false);
    if is_frontmost {
        let _ = window.hide();
        forget_paste_target(app);
        return;
    }
    show_palette(app);
}

/// Brings the palette up, recording where the user was on the way in.
///
/// Every path that puts the palette on screen goes through here. The menu bar
/// item had its own copy that skipped the recording, so a palette opened from
/// the menu had nothing to paste into and Enter could only copy.
pub fn show_palette<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window(PALETTE_WINDOW) else {
        return;
    };
    if let Some(target) = app.try_state::<PasteTarget>() {
        target.remember(current_frontmost_pid());
    }
    let _ = window.show();
    let _ = window.set_focus();
}

/// Clears the recorded paste target, for when the palette goes away.
pub fn forget_paste_target<R: Runtime>(app: &AppHandle<R>) {
    if let Some(target) = app.try_state::<PasteTarget>() {
        target.forget();
    }
}

#[cfg(target_os = "macos")]
fn current_frontmost_pid() -> Option<i32> {
    platform_macos::frontmost_pid()
}

#[cfg(not(target_os = "macos"))]
fn current_frontmost_pid() -> Option<i32> {
    None
}

/// Keeps the process alive when the palette window is closed.
///
/// Closing the window means "put it away", the same as the shortcut does.
/// Quitting is an explicit action; a clipboard manager that stops recording
/// because a window was dismissed would silently lose history.
pub fn hide_instead_of_closing<R: Runtime>(
    window: &tauri::Window<R>,
    api: &tauri::CloseRequestApi,
) {
    if should_hide_instead_of_closing(window.label()) {
        api.prevent_close();
        let _ = window.hide();
        if window.label() == PALETTE_WINDOW {
            forget_paste_target(&window.app_handle().clone());
        }
    }
}

/// Which windows are put away rather than destroyed when they are closed.
///
/// Both of them: they are created once at startup and shown on demand, so a destroyed one cannot
/// come back. Settings was missing from here, and closing it meant the settings never opened
/// again until the application was restarted — `show_settings` looks the window up and quietly
/// gives up when it is gone.
///
/// An unknown label is not covered on purpose. A window added later should decide this for itself
/// rather than inherit it by being adjacent.
fn should_hide_instead_of_closing(label: &str) -> bool {
    matches!(label, PALETTE_WINDOW | SETTINGS_WINDOW)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_windows_are_put_away_rather_than_destroyed() {
        // Settings had been left out. It is created once at startup with visible: false, so
        // closing it destroyed the only one there was and show_settings had nothing to show —
        // the settings simply stopped opening.
        assert!(should_hide_instead_of_closing(PALETTE_WINDOW));
        assert!(should_hide_instead_of_closing(SETTINGS_WINDOW));

        // Not a blanket rule: a window added later should say so itself.
        assert!(!should_hide_instead_of_closing("some-future-window"));
    }

    #[test]
    fn the_paste_target_survives_being_read() {
        // Reading used to consume the pid, so the first Enter after a summon
        // pasted and every one after it reported that the target was gone.
        let target = PasteTarget::new();
        target.remember(Some(4242));
        assert_eq!(target.current(), Some(4242));
        assert_eq!(target.current(), Some(4242));

        target.forget();
        assert_eq!(target.current(), None);
    }

    #[test]
    fn the_palette_is_never_its_own_paste_target() {
        // Shown while it already had focus, the palette would record its own
        // process and then paste Command-V into itself.
        let target = PasteTarget::new();
        target.remember(Some(4242));
        target.remember(Some(std::process::id() as i32));
        assert_eq!(target.current(), Some(4242));
    }

    #[test]
    fn nothing_in_front_clears_the_target_rather_than_keeping_a_stale_one() {
        let target = PasteTarget::new();
        target.remember(Some(4242));
        target.remember(None);
        assert_eq!(target.current(), None);
    }

    #[test]
    fn the_system_is_asked_for_permission_once_per_run() {
        // macOS shows its Accessibility dialog at most once per launch and
        // silently skips it after that, so a second ask would be a no-op the
        // user reads as being ignored.
        let prompt = PastePrompt::new();
        assert!(prompt.claim_first_ask());
        assert!(!prompt.claim_first_ask());
        assert!(!prompt.claim_first_ask());
    }
}
