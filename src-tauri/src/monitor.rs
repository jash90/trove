//! Recording what the user copies.
//!
//! macOS fires no event when the pasteboard changes, so a small thread looks at
//! its change counter a few times a second. An unchanged clipboard costs one
//! integer read; only an actual copy causes any work.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI64, Ordering},
};

use clipboard_core::{CaptureInput, ClipboardSnapshot, ContentFlags, EventFlags};
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// Emitted after something new is recorded, so an open palette refreshes
/// instead of showing a history that is already out of date.
pub const HISTORY_CHANGED_EVENT: &str = "history-changed";

/// How long a write of ours may account for the next pasteboard change.
///
/// Putting an entry back on the clipboard changes it, and without this the
/// application would record its own paste as a fresh copy. The window is short:
/// if our change never arrives, the suppression must stop mattering rather than
/// swallow the user's next real copy.
const SUPPRESSION_WINDOW_MS: i64 = 2_000;

/// Lets the rest of the application tell the monitor about its own writes and
/// turn recording off.
#[derive(Clone, Debug, Default)]
pub struct MonitorControl {
    inner: Arc<ControlState>,
}

#[derive(Debug, Default)]
struct ControlState {
    paused: AtomicBool,
    /// Wall-clock deadline of an armed suppression, or zero when none is armed.
    suppress_until_ms: AtomicI64,
}

impl MonitorControl {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_paused(&self) -> bool {
        self.inner.paused.load(Ordering::Relaxed)
    }

    pub fn set_paused(&self, paused: bool) {
        self.inner.paused.store(paused, Ordering::Relaxed);
    }

    /// Called just after this application writes to the clipboard.
    pub fn suppress_next_change(&self, now_ms: i64) {
        self.inner.suppress_until_ms.store(
            now_ms.saturating_add(SUPPRESSION_WINDOW_MS),
            Ordering::Relaxed,
        );
    }

    /// Consumes an armed suppression. Returns true when this change was ours.
    fn take_suppression(&self, now_ms: i64) -> bool {
        let deadline = self.inner.suppress_until_ms.swap(0, Ordering::Relaxed);
        deadline != 0 && now_ms <= deadline
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

/// Turns a platform snapshot into something the store can record.
pub fn capture_from_snapshot(snapshot: ClipboardSnapshot) -> CaptureInput {
    let ClipboardSnapshot {
        kind,
        primary_mime,
        representations,
        source_app_id,
        source_app_name,
        source_confidence,
        observed_at_ms,
    } = snapshot;
    CaptureInput {
        captured_at_ms: observed_at_ms,
        kind,
        primary_mime,
        representations,
        source_app_id,
        source_app_name,
        source_confidence,
        pinned: false,
        occurrence_count: 1,
        content_flags: ContentFlags::empty(),
        // Captured here and now, unlike the imported history around it.
        event_flags: EventFlags::LOCAL_ONLY,
        display_label: None,
    }
}

/// True when the copy came from an application the user asked to be left alone.
///
/// Checked before the payload reaches hashing or storage, so a denied
/// application's clipboard never touches the database at all.
pub fn is_denylisted(denylist: &[String], source_app_id: Option<&str>) -> bool {
    let Some(source) = source_app_id else {
        return false;
    };
    let source = source.to_ascii_lowercase();
    denylist
        .iter()
        .any(|entry| entry.to_ascii_lowercase() == source)
}

#[cfg(target_os = "macos")]
pub fn start<R: Runtime>(app: &AppHandle<R>, control: MonitorControl) {
    use platform_macos::{MacPasteboardWatcher, POLL_INTERVAL, PollOutcome};

    let app = app.clone();
    std::thread::Builder::new()
        .name("clipboard-monitor".to_owned())
        .spawn(move || {
            let mut watcher = MacPasteboardWatcher::new();
            loop {
                std::thread::sleep(POLL_INTERVAL);
                if control.is_paused() {
                    // Keep the counter current so resuming does not record
                    // everything copied while monitoring was off.
                    watcher.acknowledge(watcher.change_count());
                    continue;
                }
                let observed_at_ms = now_ms();
                match watcher.poll(observed_at_ms) {
                    PollOutcome::Unchanged => {}
                    PollOutcome::Rejected { .. } => {
                        // Nothing to record and nothing to log: the reason
                        // codes exist for tests, not for a file on disk that
                        // would say what the user copied and when.
                        let _ = control.take_suppression(observed_at_ms);
                    }
                    PollOutcome::Captured { snapshot, .. } => {
                        if control.take_suppression(observed_at_ms) {
                            continue;
                        }
                        record(&app, capture_from_snapshot(*snapshot));
                    }
                }
            }
        })
        .ok();
}

#[cfg(not(target_os = "macos"))]
pub fn start<R: Runtime>(_app: &AppHandle<R>, _control: MonitorControl) {
    // No adapter for this platform yet. The application still works from its
    // window; claiming to monitor would be worse than saying nothing.
}

fn record<R: Runtime>(app: &AppHandle<R>, capture: CaptureInput) {
    let Some(state) = app.try_state::<crate::state::AppState>() else {
        return;
    };
    let store = state.store.clone();
    let denylist = crate::commands::denylisted_apps(&store);
    if is_denylisted(&denylist, capture.source_app_id.as_deref()) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if store.ingest(capture).await.is_ok() {
            let _ = app.emit(HISTORY_CHANGED_EVENT, ());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_of_ours_accounts_for_exactly_one_change() {
        let control = MonitorControl::new();
        control.suppress_next_change(1_000);

        assert!(control.take_suppression(1_100));
        assert!(
            !control.take_suppression(1_200),
            "one write must not swallow a second copy"
        );
    }

    #[test]
    fn a_suppression_whose_change_never_arrived_stops_mattering() {
        let control = MonitorControl::new();
        control.suppress_next_change(1_000);

        assert!(!control.take_suppression(1_000 + SUPPRESSION_WINDOW_MS + 1));
    }

    #[test]
    fn nothing_is_suppressed_when_we_did_not_write() {
        assert!(!MonitorControl::new().take_suppression(1_000));
    }

    #[test]
    fn pausing_and_resuming_is_observable() {
        let control = MonitorControl::new();
        assert!(!control.is_paused());
        control.set_paused(true);
        assert!(control.is_paused());
        control.set_paused(false);
        assert!(!control.is_paused());
    }

    #[test]
    fn a_denied_application_is_matched_regardless_of_spelling() {
        let denylist = vec!["com.apple.Passwords".to_owned()];

        assert!(is_denylisted(&denylist, Some("com.apple.passwords")));
        assert!(is_denylisted(&denylist, Some("com.apple.Passwords")));
        assert!(!is_denylisted(&denylist, Some("com.apple.notes")));
        assert!(!is_denylisted(&denylist, None));
    }
}
