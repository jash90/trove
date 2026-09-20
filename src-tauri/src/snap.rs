//! The global shortcuts that snap the frontmost window, Rectangle-style.
//!
//! Ten positions, ten chords, all registered for the whole system exactly the
//! way the summoning shortcut is. The defaults are the ones Rectangle
//! inherited from Spectacle, so a hand trained on either reaches for keys
//! that already work.
//!
//! Off macOS the map still exists — the settings screen is the same
//! everywhere — but the shortcuts have nothing to move and install into a
//! registration that never fires.

use std::collections::BTreeMap;
use std::sync::Mutex;

use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

/// Every snap position and the chord it ships with, in menu order.
///
/// Written once because the settings screen and the registration must agree;
/// a default that drifted between the two would be a shortcut the screen
/// shows and the keyboard ignores.
pub const SNAP_DEFAULTS: &[(&str, &str)] = &[
    ("leftHalf", "CommandOrControl+Alt+ArrowLeft"),
    ("rightHalf", "CommandOrControl+Alt+ArrowRight"),
    ("topHalf", "CommandOrControl+Alt+ArrowUp"),
    ("bottomHalf", "CommandOrControl+Alt+ArrowDown"),
    ("topLeft", "CommandOrControl+Control+ArrowLeft"),
    ("topRight", "CommandOrControl+Control+ArrowRight"),
    ("bottomLeft", "CommandOrControl+Control+Shift+ArrowLeft"),
    ("bottomRight", "CommandOrControl+Control+Shift+ArrowRight"),
    ("maximize", "CommandOrControl+Alt+F"),
    ("center", "CommandOrControl+Alt+C"),
];

/// The map a fresh install snaps with.
pub fn defaults() -> BTreeMap<String, String> {
    SNAP_DEFAULTS
        .iter()
        .map(|(id, chord)| ((*id).to_owned(), (*chord).to_owned()))
        .collect()
}

/// Whether a map of snap shortcuts can be installed as it stands.
///
/// Every position present and named, every chord parseable by the same
/// grammar the summoning shortcut uses, no chord twice, and none colliding
/// with the summoning shortcut itself — a collision there would be a race
/// the keyboard settles by accident rather than by design.
pub fn is_valid_map(map: &BTreeMap<String, String>, summoning: &str) -> bool {
    if map.len() != SNAP_DEFAULTS.len() {
        return false;
    }
    let mut seen: std::collections::HashSet<&str> =
        std::collections::HashSet::with_capacity(map.len() + 1);
    seen.insert(summoning);
    map.iter().all(|(id, chord)| {
        crate::hotkey::parse_shortcut(chord).is_some()
            && SNAP_DEFAULTS.iter().any(|(known, _)| known == id)
            && seen.insert(chord.as_str())
    })
}

/// The map of shortcuts currently answering, so a save knows what to take
/// down — the same job `ActiveShortcut` does for the summoning chord.
#[derive(Default)]
pub struct ActiveSnapShortcuts {
    current: Mutex<BTreeMap<String, String>>,
}

impl ActiveSnapShortcuts {
    pub fn new(map: BTreeMap<String, String>) -> Self {
        Self {
            current: Mutex::new(map),
        }
    }

    pub fn get(&self) -> BTreeMap<String, String> {
        self.current
            .lock()
            .map(|map| map.clone())
            .unwrap_or_default()
    }

    pub fn set(&self, map: BTreeMap<String, String>) {
        if let Ok(mut current) = self.current.lock() {
            *current = map;
        }
    }
}

/// Registers every shortcut in the map.
///
/// A chord another application holds is skipped with a line in the log
/// rather than refused: ten chords means ten chances to collide, and one
/// collision is no reason the other nine stop working.
pub fn install<R: Runtime>(app: &AppHandle<R>, map: &BTreeMap<String, String>) {
    for (id, chord) in map {
        let Some(shortcut) = crate::hotkey::parse_shortcut(chord) else {
            eprintln!("trove: snap shortcut {chord} does not parse");
            continue;
        };
        let action = id.clone();
        let registered =
            app.global_shortcut()
                .on_shortcut(shortcut, move |app, _shortcut, event| {
                    // Press only, as with the summoning shortcut: acting on release
                    // too would snap twice per keystroke.
                    if event.state() == ShortcutState::Pressed {
                        run(app, &action);
                    }
                });
        if let Err(error) = registered {
            eprintln!("trove: snap shortcut {chord} unavailable: {error}");
        }
    }
}

/// Swaps the answering map: the old registrations go first, so no window is
/// a moment where both an old and a new chord claim the same snap.
pub fn rebind<R: Runtime>(
    app: &AppHandle<R>,
    previous: &BTreeMap<String, String>,
    next: &BTreeMap<String, String>,
) {
    for chord in previous.values() {
        if let Some(shortcut) = crate::hotkey::parse_shortcut(chord) {
            let _ = app.global_shortcut().unregister(shortcut);
        }
    }
    install(app, next);
}

/// Runs one snap by its settings id, on the thread AppKit answers from.
fn run<R: Runtime>(app: &AppHandle<R>, id: &str) {
    #[cfg(target_os = "macos")]
    {
        let Some(action) = platform_macos::SnapAction::from_id(id) else {
            return;
        };
        let logged_id = id.to_owned();
        let sent =
            app.run_on_main_thread(move || match platform_macos::window_snap::snap(action) {
                platform_macos::SnapOutcome::Moved => {}
                platform_macos::SnapOutcome::PermissionRequired => {
                    eprintln!("trove: snap refused — Accessibility permission missing")
                }
                platform_macos::SnapOutcome::NoTarget => {}
                platform_macos::SnapOutcome::Refused => {
                    eprintln!("trove: {logged_id} refused by the frontmost application")
                }
            });
        if let Err(error) = sent {
            eprintln!("trove: snap could not reach the main thread: {error}");
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, id);
}

/// Snaps the window the palette's user was working in, by arrangement id.
///
/// The palette itself is frontmost while this runs, so the target is the
/// pid a paste would land in — recorded when the palette was summoned.
/// Resolves false when nothing is there to arrange or the window refused,
/// which is the palette's sentence to say, not the command's.
#[tauri::command(rename_all = "camelCase")]
pub async fn snap_window<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    action_id: String,
) -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        let Some(action) = platform_macos::SnapAction::from_id(&action_id) else {
            return Err("unknown_snap_action".to_owned());
        };
        let Some(pid) = app
            .try_state::<crate::hotkey::PasteTarget>()
            .and_then(|target| target.current())
            .filter(|pid| *pid > 0)
        else {
            return Ok(false);
        };
        // AppKit's window queries answer from the main thread; the channel
        // hands the outcome back to this command's future.
        let (sender, receiver) = std::sync::mpsc::channel();
        let sent = app.run_on_main_thread(move || {
            let outcome = platform_macos::window_snap::snap_pid(pid, action);
            let _ = sender.send(outcome);
        });
        if sent.is_err() {
            return Ok(false);
        }
        Ok(matches!(
            receiver.recv(),
            Ok(platform_macos::SnapOutcome::Moved)
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, action_id);
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_parses_as_a_shortcut() {
        for (id, chord) in SNAP_DEFAULTS {
            assert!(
                crate::hotkey::parse_shortcut(chord).is_some(),
                "{id} default {chord} does not parse"
            );
        }
    }

    #[test]
    fn the_defaults_are_a_valid_map_that_does_not_collide_with_summoning() {
        let map = defaults();
        assert!(is_valid_map(&map, "CommandOrControl+Space"));
    }

    #[test]
    fn a_duplicate_chord_or_a_missing_position_is_invalid() {
        let mut duplicated = defaults();
        duplicated.insert(
            "center".to_owned(),
            "CommandOrControl+Alt+ArrowLeft".to_owned(),
        );
        assert!(!is_valid_map(&duplicated, "CommandOrControl+Space"));

        let mut missing_one = defaults();
        missing_one.remove("topLeft");
        assert!(!is_valid_map(&missing_one, "CommandOrControl+Space"));

        let mut unknown = defaults();
        unknown.insert("diagonal".to_owned(), "CommandOrControl+Alt+D".to_owned());
        unknown.remove("center");
        assert!(!is_valid_map(&unknown, "CommandOrControl+Space"));
    }

    #[test]
    fn a_chord_matching_the_summoning_shortcut_is_invalid() {
        let mut collides = defaults();
        collides.insert("center".to_owned(), "CommandOrControl+Space".to_owned());
        assert!(!is_valid_map(&collides, "CommandOrControl+Space"));
    }
}
