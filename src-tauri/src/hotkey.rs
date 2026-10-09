//! The global shortcut that summons the palette.
//!
//! A clipboard manager is only useful if it is one keystroke away from whatever
//! the user is doing, so the shortcut is registered for the whole system rather
//! than for the window. Pressing it again puts the palette away, which keeps
//! the same key in charge of both directions instead of forcing a reach for the
//! mouse or Escape.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicI32, Ordering},
};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, Runtime};
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
    // Floating again: it is dropped to the ordinary level whenever the user
    // leaves for another application (`settle_settings_level`), and asking
    // for it is asking for it in front.
    present(app, SETTINGS_WINDOW, Some(Float::Above));
    // The palette stays open — it only stops floating: the window just
    // opened is focused and renders above it.
    put_palette_below(app);
}

/// Brings the settings window up on one particular tab.
///
/// The window is created at launch and only hidden, so its interface is
/// already listening by the time anything in the menu bar can be clicked.
pub fn show_settings_tab<R: Runtime>(app: &AppHandle<R>, tab: &str) {
    show_settings(app);
    let _ = app.emit_to(SETTINGS_WINDOW, OPEN_SETTINGS_TAB_EVENT, tab);
}

/// Carries the name of the tab the settings window should switch to.
pub const OPEN_SETTINGS_TAB_EVENT: &str = "open-settings-tab";

/// Brings the chat window up, wherever it was last left.
pub fn show_chat<R: Runtime>(app: &AppHandle<R>) {
    // Its level is left alone: the chat window is configured not to float,
    // and nothing here changes that.
    present(app, CHAT_WINDOW, None);
    put_palette_below(app);
}

/// What a summoned window's level should be once it is in front.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Float {
    /// Above other applications' windows.
    Above,
    /// The ordinary level, among everyone else's windows.
    Among,
}

/// Brings one of this application's windows forward, on the user's Space and
/// screen, at the level asked for — in that order and in one go.
///
/// One main-thread closure on purpose. The runtime's level setter is queued
/// rather than applied, while showing and focusing are applied at once, so a
/// window raised that way was ordered in at the normal level and floated a
/// beat later: the palette flashed underneath the settings window it had been
/// sunk below. Everything here runs on the main thread in sequence, so the
/// window is already at its level by the time it is ordered in.
fn present<R: Runtime>(app: &AppHandle<R>, label: &'static str, float: Option<Float>) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(window) = handle.get_webview_window(label) else {
            return;
        };
        prepare_for_summon(&window, float);
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    });
}

/// The window-server half of a summon: Space, screen and level.
///
/// Called on the main thread only, from `present`.
#[cfg(target_os = "macos")]
fn prepare_for_summon<R: Runtime>(window: &tauri::WebviewWindow<R>, float: Option<Float>) {
    use platform_macos::summon;
    let Ok(ns_window) = window.ns_window() else {
        return;
    };
    // SAFETY: the pointer is this window's own NSWindow, alive for as long
    // as the window is, and `present` only calls this on the main thread.
    unsafe {
        summon::follow_active_space(ns_window);
        summon::move_to_pointer_screen(ns_window);
        if let Some(float) = float {
            summon::set_level(ns_window, level_for(float));
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn prepare_for_summon<R: Runtime>(window: &tauri::WebviewWindow<R>, float: Option<Float>) {
    if let Some(float) = float {
        let _ = window.set_always_on_top(float == Float::Above);
    }
}

#[cfg(target_os = "macos")]
fn level_for(float: Float) -> platform_macos::summon::Level {
    match float {
        Float::Above => platform_macos::summon::Level::Floating,
        Float::Among => platform_macos::summon::Level::Normal,
    }
}

/// Sets a window's level from any thread, applied on the main thread.
fn set_float<R: Runtime>(app: &AppHandle<R>, label: &'static str, float: Float) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(window) = handle.get_webview_window(label) else {
            return;
        };
        #[cfg(target_os = "macos")]
        if let Ok(ns_window) = window.ns_window() {
            // SAFETY: this window's own NSWindow, on the main thread.
            unsafe {
                platform_macos::summon::set_level(ns_window, level_for(float));
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = window.set_always_on_top(float == Float::Above);
    });
}

/// Answers a click on the Dock tile.
///
/// The tile has to summon something, or it is a button that does nothing —
/// but not always the palette. With the chat or the settings window open, the
/// click is a way back to that window; floating the palette over it instead
/// covered the thing the user was trying to return to.
pub fn reopen<R: Runtime>(app: &AppHandle<R>) {
    let visible = |label| {
        app.get_webview_window(label)
            .is_some_and(|window| window.is_visible().unwrap_or(false))
    };
    if visible(CHAT_WINDOW) {
        show_chat(app);
    } else if visible(SETTINGS_WINDOW) {
        show_settings(app);
    } else {
        show_palette(app);
    }
}

/// How long the window server is given to settle focus before the settings
/// window's level is decided. Focus moving between two of this application's
/// windows passes through a moment where neither is key.
const FOCUS_SETTLE: Duration = Duration::from_millis(200);

/// Reacts to one of this application's windows gaining or losing focus.
pub fn window_focus_changed<R: Runtime>(window: &tauri::Window<R>, focused: bool) {
    let app = window.app_handle();
    match (window.label(), focused) {
        // The palette entered by a click — not by its shortcut — still owes
        // its snaps the window the user was last working in, so coming
        // forward is when the target is remembered.
        (PALETTE_WINDOW, true) => remember_target_on_focus(app),
        (SETTINGS_WINDOW, false) => settle_settings_level(app),
        _ => {}
    }
}

/// Stops the settings window floating once the user has gone elsewhere.
///
/// It floats so that, opened over the palette, it is not hidden behind it.
/// Left floating, it stayed above every other application too — a settings
/// window hovering over the browser the user had switched to. So it sinks to
/// the ordinary level when focus has left this application altogether, and
/// `show_settings` floats it again the next time it is asked for. Focus moving
/// to the palette or the chat window is not leaving, and changes nothing.
fn settle_settings_level<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FOCUS_SETTLE).await;
        let ours_focused = [PALETTE_WINDOW, SETTINGS_WINDOW, CHAT_WINDOW]
            .into_iter()
            .filter_map(|label| app.get_webview_window(label))
            .any(|window| window.is_focused().unwrap_or(false));
        if !ours_focused {
            set_float(&app, SETTINGS_WINDOW, Float::Among);
        }
    });
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
    set_float(app, PALETTE_WINDOW, Float::Among);
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

    /// Records the window-server guess made when the palette gains focus, if
    /// that guess should stand — see `focus_target_update`.
    pub fn remember_on_focus(&self, guess: Option<i32>, since_summon: Option<Duration>) {
        if let Some(pid) = focus_target_update(self.current(), guess, since_summon) {
            self.remember(Some(pid));
        }
    }
}

/// How long after a summon the palette gaining focus is the summon's own doing.
///
/// Generous on purpose: activation is a request the window server answers
/// when it gets to it, and a focus event that lands after this is a click.
const SUMMON_FOCUS_WINDOW: Duration = Duration::from_millis(1000);

/// What, if anything, the palette gaining focus should record as the target.
///
/// The summon records the frontmost application exactly, before the palette
/// takes the front; the focus event that follows can only guess from the
/// window server's z-order, and used to overwrite the exact answer with the
/// guess — or with nothing at all, stored as an empty target. So the guess
/// only stands where there is nothing better: when no target is recorded, or
/// when the summon is long past and this focus is a click into a palette left
/// standing while the user worked somewhere else. And an empty guess never
/// erases a target.
pub fn focus_target_update(
    recorded: Option<i32>,
    guess: Option<i32>,
    since_summon: Option<Duration>,
) -> Option<i32> {
    let guess = guess.filter(|pid| *pid > 0)?;
    let summon_is_fresh = since_summon.is_some_and(|elapsed| elapsed < SUMMON_FOCUS_WINDOW);
    match recorded {
        Some(_) if summon_is_fresh => None,
        _ => Some(guess),
    }
}

/// How soon after a summon the shortcut pressed again means "put it away".
///
/// Activation is not instant: pressed twice quickly, the second press arrived
/// while the palette was on screen but not yet key, read as "buried behind
/// something" and summoned it again instead of hiding it.
const DOUBLE_PRESS: Duration = Duration::from_millis(300);

/// What the shortcut does, given the state of the palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggle {
    Show,
    Hide,
}

/// Decides what a press of the summoning shortcut does.
///
/// A window can be visible but buried behind other applications. Treating
/// that as "already open" would hide it just as the user asked to see it, so
/// the shortcut only puts it away when it is genuinely in front — or when it
/// was summoned so recently that it has not had the chance to be.
pub fn toggle_decision(
    visible: bool,
    focused: bool,
    since_last_summon: Option<Duration>,
) -> Toggle {
    if !visible {
        return Toggle::Show;
    }
    if focused || since_last_summon.is_some_and(|elapsed| elapsed < DOUBLE_PRESS) {
        Toggle::Hide
    } else {
        Toggle::Show
    }
}

/// When the palette was last summoned. Process-wide because there is one
/// palette, and every way of summoning it goes through `show_palette`.
static LAST_SUMMON: Mutex<Option<Instant>> = Mutex::new(None);

fn mark_summoned() {
    if let Ok(mut last) = LAST_SUMMON.lock() {
        *last = Some(Instant::now());
    }
}

fn since_last_summon() -> Option<Duration> {
    LAST_SUMMON
        .lock()
        .ok()
        .and_then(|last| last.map(|at| at.elapsed()))
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
    let decision = toggle_decision(
        window.is_visible().unwrap_or(false),
        window.is_focused().unwrap_or(false),
        since_last_summon(),
    );
    match decision {
        Toggle::Hide => hide_palette(app),
        Toggle::Show => show_palette(app),
    }
}

/// Tells the palette's interface it has just been summoned, as opposed to
/// merely refocused: the query is selected so typing replaces it, and the view
/// goes back to its categories.
pub const PALETTE_SUMMONED_EVENT: &str = "palette-summoned";

/// Brings the palette up, recording where the user was on the way in.
///
/// Every path that puts the palette on screen goes through here. The menu bar
/// item had its own copy that skipped the recording, so a palette opened from
/// the menu had nothing to paste into and Enter could only copy.
pub fn show_palette<R: Runtime>(app: &AppHandle<R>) {
    if app.get_webview_window(PALETTE_WINDOW).is_none() {
        return;
    }
    if let Some(target) = app.try_state::<PasteTarget>() {
        target.remember(current_frontmost_pid());
    }
    mark_summoned();
    // Summoning is asking for the overlay: it floats again, over whatever
    // it was sunk beneath when a working window opened — on the Space and the
    // screen the user is on, not wherever it was last left.
    present(app, PALETTE_WINDOW, Some(Float::Above));
    let _ = app.emit_to(PALETTE_WINDOW, PALETTE_SUMMONED_EVENT, ());
}

/// Puts the palette away and hands the front back to where the user was.
///
/// Hiding our window does not hand activation back — an application with no
/// window on screen stays the active one — so the keyboard went nowhere until
/// the user clicked something. The application recorded on the way in is
/// asked for the front explicitly, the same way a paste asks for it.
pub fn hide_palette<R: Runtime>(app: &AppHandle<R>) {
    let target = app
        .try_state::<PasteTarget>()
        .and_then(|target| target.current());
    put_palette_away(app);
    if let Some(pid) = target.filter(|pid| *pid != std::process::id() as i32) {
        activate(pid);
    }
}

/// Hides the palette and drops its target, without activating anything.
///
/// For when something else is about to take the front on its own — an
/// application being launched — and handing it to the previous target would
/// put that window in front of the one the user asked for.
pub fn put_palette_away<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(PALETTE_WINDOW) {
        let _ = window.hide();
    }
    forget_paste_target(app);
}

#[cfg(target_os = "macos")]
fn activate(pid: i32) {
    platform_macos::activate_pid(pid);
}

#[cfg(not(target_os = "macos"))]
fn activate(_pid: i32) {}

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
///
/// A guess, though, and the summon's own record is not: see
/// `focus_target_update` for when the guess is allowed to stand.
pub fn remember_target_on_focus<R: Runtime>(app: &AppHandle<R>) {
    if let Some(target) = app.try_state::<PasteTarget>() {
        target.remember_on_focus(last_active_before_us(), since_last_summon());
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
        if window.label() == PALETTE_WINDOW {
            hide_palette(window.app_handle());
        } else {
            let _ = window.hide();
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
    fn the_shortcut_hides_a_palette_in_front_and_shows_one_that_is_not() {
        assert_eq!(toggle_decision(false, false, None), Toggle::Show);
        assert_eq!(toggle_decision(true, true, None), Toggle::Hide);
        // Visible but behind another application: the press asks to see it.
        let long_ago = Some(Duration::from_secs(30));
        assert_eq!(toggle_decision(true, false, long_ago), Toggle::Show);
        assert_eq!(toggle_decision(true, false, None), Toggle::Show);
    }

    #[test]
    fn a_quick_second_press_hides_the_palette_before_it_has_become_key() {
        // Pressed twice in quick succession, the second press arrived while the
        // palette was on screen but activation had not landed yet, and it was
        // summoned again instead of put away.
        let just_now = Some(Duration::from_millis(120));
        assert_eq!(toggle_decision(true, false, just_now), Toggle::Hide);
        // Not for a palette that is not on screen at all.
        assert_eq!(toggle_decision(false, false, just_now), Toggle::Show);
    }

    #[test]
    fn the_focus_after_a_summon_does_not_overwrite_what_the_summon_recorded() {
        // The summon records the frontmost application exactly; the focus
        // event right after it can only guess from the z-order.
        let fresh = Some(Duration::from_millis(50));
        assert_eq!(focus_target_update(Some(4242), Some(777), fresh), None);

        let target = PasteTarget::new();
        target.remember(Some(4242));
        target.remember_on_focus(Some(777), fresh);
        assert_eq!(target.current(), Some(4242));
    }

    #[test]
    fn an_empty_guess_never_erases_a_target() {
        let stale = Some(Duration::from_secs(30));
        assert_eq!(focus_target_update(Some(4242), None, stale), None);
        assert_eq!(focus_target_update(Some(4242), Some(0), stale), None);

        let target = PasteTarget::new();
        target.remember(Some(4242));
        target.remember_on_focus(None, stale);
        assert_eq!(target.current(), Some(4242));
    }

    #[test]
    fn the_guess_fills_an_empty_target_and_follows_a_click_long_after_the_summon() {
        // Nothing recorded — a fresh launch that activated itself — so the
        // guess is the best answer there is, however recent the summon.
        assert_eq!(
            focus_target_update(None, Some(777), Some(Duration::from_millis(50))),
            Some(777)
        );
        // A click into a palette left standing while the user worked
        // elsewhere: the summon's record is the stale one now.
        assert_eq!(
            focus_target_update(Some(4242), Some(777), Some(Duration::from_secs(30))),
            Some(777)
        );
        assert_eq!(focus_target_update(Some(4242), Some(777), None), Some(777));
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
