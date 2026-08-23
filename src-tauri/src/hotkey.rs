//! The global shortcut that summons the palette.
//!
//! A clipboard manager is only useful if it is one keystroke away from whatever
//! the user is doing, so the shortcut is registered for the whole system rather
//! than for the window. Pressing it again puts the palette away, which keeps
//! the same key in charge of both directions instead of forcing a reach for the
//! mouse or Escape.

use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// The label of the window the shortcut toggles.
const PALETTE_WINDOW: &str = "main";

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
    let shortcut = default_shortcut();
    app.global_shortcut()
        .on_shortcut(shortcut, move |app, _shortcut, event| {
            // Act on press only: acting on release too would toggle twice per
            // keystroke and leave the palette exactly where it started.
            if event.state() == ShortcutState::Pressed {
                toggle_palette(app);
            }
        })
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
    let _ = window.show();
    let _ = window.set_focus();
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
