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
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

/// The label of the window the shortcut toggles.
const PALETTE_WINDOW: &str = "main";

/// The label of the settings window.
///
/// Declared in the configuration and started hidden, so showing it is a matter
/// of asking rather than of building one — and it keeps its own size and
/// position between openings.
const SETTINGS_WINDOW: &str = "settings";

/// The label of the chat window. The same shape as settings: declared in
/// the configuration, started hidden, shown rather than rebuilt — and its
/// conversation stays in memory between openings, because hiding the
/// window keeps the document alive.
const CHAT_WINDOW: &str = "chat";

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
    // The palette stays open — it only stops floating: the window just
    // opened is focused and renders above it.
    put_palette_below(app);
}

/// Brings the chat window up, wherever it was last left.
pub fn show_chat<R: Runtime>(app: &AppHandle<R>) {
    let Some(window) = app.get_webview_window(CHAT_WINDOW) else {
        return;
    };
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
    put_palette_below(app);
}

/// Puts the palette at the bottom of this application's windows.
///
/// The palette floats above everything — that is its job as a summon
/// overlay. But the windows it opens (chat, settings) are working windows,
/// and a palette still floating over them covers the thing the user asked
/// for. So it stops floating: it sinks to the ordinary level, stays open
/// and visible, and the focused destination renders above it. Summoning
/// the palette re-floats it — an overlay asked for is an overlay again.
fn put_palette_below<R: Runtime>(app: &AppHandle<R>) {
    if let Some(palette) = app.get_webview_window(PALETTE_WINDOW) {
        let _ = palette.set_always_on_top(false);
    }
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

/// Cmd+Space on macOS, Ctrl+Space elsewhere.
///
/// The chord a launcher-style palette is reached for by reflex. It is also the
/// one Spotlight holds, which is why freeing it is part of setting this
/// application up rather than an afterthought: a chord the system claims is
/// dispatched above the table this application registers into, so the shortcut
/// registers cleanly and then never fires. An earlier version added Shift to
/// step around that. Stepping around it was the thing to stop doing.
///
/// Written once, here, because the settings row and the registration used to
/// carry their own copies and disagreed: the screen offered Cmd+Shift+V while
/// Cmd+Shift+Space was what answered.
pub const DEFAULT_HOTKEY: &str = "CommandOrControl+Space";

pub fn default_shortcut() -> Shortcut {
    parse_shortcut(DEFAULT_HOTKEY).expect("the built-in default must parse")
}

/// Which shortcut a launch should try, given what was saved.
///
/// An unreadable saved value falls back rather than refusing to register. A row
/// written by an older version, or by hand, must not leave the palette with no
/// way in at all — and the settings screen, which is where the value gets
/// fixed, is reached through the palette.
pub fn shortcut_for_launch(saved: Option<&str>) -> Shortcut {
    saved
        .and_then(parse_shortcut)
        .unwrap_or_else(default_shortcut)
}

/// Registers a shortcut and wires it to the palette.
///
/// A failure here is not fatal: the application still works from its window and
/// from the menu bar, so the caller records the degraded state rather than
/// refusing to start. The most common cause is another application holding the
/// same combination — and on Cmd+Space there are several candidates.
pub fn install<R: Runtime>(
    app: &AppHandle<R>,
    shortcut: Shortcut,
) -> Result<(), tauri_plugin_global_shortcut::Error> {
    register(app, shortcut)
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
///
/// It also carries whether the system accepted the registration at all. That
/// used to end at an `eprintln!`, which was survivable while the shortcut was
/// an unusual chord nobody else wanted; on Cmd+Space it is not. A shortcut the
/// system refused looks exactly like a shortcut that works until the user
/// presses it, so the one place that knows the answer has to be able to say so.
#[derive(Clone, Debug)]
pub struct ActiveShortcut {
    current: Arc<std::sync::Mutex<Shortcut>>,
    registered: Arc<std::sync::atomic::AtomicBool>,
}

impl ActiveShortcut {
    pub fn new(shortcut: Shortcut) -> Self {
        Self {
            current: Arc::new(std::sync::Mutex::new(shortcut)),
            registered: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Whether the system is actually answering the shortcut.
    pub fn is_registered(&self) -> bool {
        self.registered.load(Ordering::Relaxed)
    }

    pub fn set_registered(&self, registered: bool) {
        self.registered.store(registered, Ordering::Relaxed);
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

/// The system shortcuts this application turned off to free its own chord.
///
/// Remembered for one reason: so they can be turned back on. Disabling a
/// machine-wide shortcut from inside an application and then offering no way
/// back would leave the user hunting through System Settings for a change they
/// did not make by hand.
///
/// Held in memory rather than written to the settings row. The system's table
/// is the truth about what is disabled and it is read live; a second copy on
/// disk could only ever go stale, and acting on a stale one would mean handing
/// back a shortcut the user had since turned off themselves.
#[derive(Clone, Debug, Default)]
pub struct ReleasedSystemHotkeys {
    ids: Arc<std::sync::Mutex<Vec<i64>>>,
}

impl ReleasedSystemHotkeys {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self) -> Vec<i64> {
        self.ids.lock().map(|ids| ids.clone()).unwrap_or_default()
    }

    pub fn remember(&self, ids: &[i64]) {
        if let Ok(mut held) = self.ids.lock() {
            held.extend_from_slice(ids);
            held.sort_unstable();
            held.dedup();
        }
    }

    pub fn forget(&self) {
        if let Ok(mut held) = self.ids.lock() {
            held.clear();
        }
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
    // Summoning is asking for the overlay: it floats again, over whatever
    // it was sunk beneath when a working window opened.
    let _ = window.set_always_on_top(true);
    let _ = window.show();
    let _ = window.set_focus();
}

/// Clears the recorded paste target, for when the palette goes away.
pub fn forget_paste_target<R: Runtime>(app: &AppHandle<R>) {
    if let Some(target) = app.try_state::<PasteTarget>() {
        target.forget();
    }
}

/// Remembers the application the palette took the front from.
///
/// Called whenever the palette comes forward — not only when its shortcut
/// summons it — because a click into the floating palette owes its snaps
/// the same target a summon would record: the window the user was last
/// working in, whoever they touched most recently before this window rose.
pub fn remember_target_on_focus<R: Runtime>(app: &AppHandle<R>) {
    if let Some(target) = app.try_state::<PasteTarget>() {
        target.remember(last_active_before_us());
    }
}

/// The most recently active application that is not this one, by the
/// system's own ledger of who was in front when.
#[cfg(target_os = "macos")]
fn last_active_before_us() -> Option<i32> {
    platform_macos::window_snap::previous_active_pid()
}

#[cfg(not(target_os = "macos"))]
fn last_active_before_us() -> Option<i32> {
    None
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
    matches!(label, PALETTE_WINDOW | SETTINGS_WINDOW | CHAT_WINDOW)
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
        assert!(should_hide_instead_of_closing(CHAT_WINDOW));

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
