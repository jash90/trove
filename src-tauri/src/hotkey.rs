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

    pub fn remember(&self, pid: Option<i32>) {
        self.pid.store(pid.unwrap_or_default(), Ordering::Relaxed);
    }

    pub fn take(&self) -> Option<i32> {
        let pid = self.pid.swap(0, Ordering::Relaxed);
        (pid > 0).then_some(pid)
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
        return;
    }
    if let Some(target) = app.try_state::<PasteTarget>() {
        target.remember(current_frontmost_pid());
    }
    let _ = window.show();
    let _ = window.set_focus();
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
    if window.label() == PALETTE_WINDOW {
        api.prevent_close();
        let _ = window.hide();
    }
}
