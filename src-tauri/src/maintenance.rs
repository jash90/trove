//! Housekeeping that runs while the application is idle.
//!
//! Two jobs share one slow timer: deleting history the user asked to stop
//! keeping, and freeing blobs nothing refers to any more. Both are bounded, so
//! neither can hold the writer while the user is copying, and both are quiet —
//! nothing here is worth interrupting anyone for.

use std::time::Duration;

use clipboard_store::{GcStepBudget, MAX_RETENTION_BATCH, RetentionPolicy, StoreHandle};
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// How long after startup the first pass runs.
///
/// Long enough that opening the application never competes with cleanup for
/// the writer.
const FIRST_PASS_DELAY: Duration = Duration::from_secs(60);

/// How often a pass runs after that.
const PASS_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Most retention batches one pass will run.
///
/// A pass that would delete forever is a pass that holds the writer forever.
/// Whatever is left waits for the next one, fifteen minutes away.
const MAX_BATCHES_PER_PASS: u32 = 20;

/// How many blob directory entries one pass looks at.
const GC_ENTRIES_PER_PASS: usize = 2_000;

pub fn start<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_PASS_DELAY).await;
        loop {
            run_pass(&app).await;
            tokio::time::sleep(PASS_INTERVAL).await;
        }
    });
}

async fn run_pass<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<crate::state::AppState>() else {
        return;
    };
    let store = state.store.clone();
    let deleted = apply_retention(&store).await;
    reclaim_unused_blobs(&store).await;
    if deleted > 0 {
        // The list is showing entries that no longer exist.
        let _ = app.emit(crate::monitor::HISTORY_CHANGED_EVENT, ());
    }
}

/// Deletes aged-out history, in batches, up to this pass's limit.
async fn apply_retention(store: &StoreHandle) -> u64 {
    let policy = RetentionPolicy::from_days(retention_days(store));
    let Some(cutoff_ms) = policy.cutoff_ms(now_ms()) else {
        return 0;
    };
    let mut deleted = 0_u64;
    for _ in 0..MAX_BATCHES_PER_PASS {
        match store
            .run_retention_batch(cutoff_ms, MAX_RETENTION_BATCH)
            .await
        {
            Ok(outcome) => {
                deleted += outcome.deleted_events;
                if !outcome.more_remaining {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    deleted
}

/// Frees blobs nothing refers to any more.
///
/// The scan only observes; whether a blob is really unused is decided by the
/// writer, because an import may have started referring to it in between.
async fn reclaim_unused_blobs(store: &StoreHandle) {
    let Ok(cas) = store.cas_store() else {
        return;
    };
    let Ok(mut session) = cas.start_gc() else {
        return;
    };
    let reader = store.clone();
    let scan = tokio::task::spawn_blocking(move || {
        // One reader for the whole pass. Asking the store per file opens a
        // fresh connection each time, which costs more than the question.
        let mut outcome = None;
        let _ = reader.with_reader(|connection| {
            outcome = Some(
                session.step(GcStepBudget::new(GC_ENTRIES_PER_PASS), |relpath| {
                    Ok(is_referenced(connection, relpath))
                }),
            );
            Ok(())
        });
        outcome
    })
    .await;
    let Ok(Some(Ok(step))) = scan else {
        return;
    };
    if step.candidates.is_empty() {
        return;
    }
    let _ = store.reclaim_orphans(step.candidates).await;
}

/// Whether anything still points at one blob.
///
/// Every table that stores a `blob_relpath` has to be named here. A table that
/// is not gets its files collected as orphans and deleted out from under it —
/// which is exactly what happened to link previews when this list was written
/// before that table existed.
fn is_referenced(connection: &rusqlite::Connection, relpath: &str) -> bool {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM raw_payload WHERE blob_relpath = ?1)
                 OR EXISTS(SELECT 1 FROM artifact WHERE blob_relpath = ?1)
                 OR EXISTS(SELECT 1 FROM link_preview WHERE icon_relpath = ?1)
                 OR EXISTS(SELECT 1 FROM link_preview WHERE image_relpath = ?1)",
            [relpath],
            |row| row.get::<_, bool>(0),
        )
        // An unreadable database must not be taken as "nothing is referenced":
        // that would delete every blob in the store.
        .unwrap_or(true)
}

fn retention_days(store: &StoreHandle) -> Option<u16> {
    crate::commands::retention_days(store)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

// Neither job may run until it finishes: the writer is shared with capture, so
// a pass takes a bounded bite and whatever is left waits for the next one.
const _: () = {
    assert!(MAX_BATCHES_PER_PASS >= 1);
    assert!(MAX_BATCHES_PER_PASS * MAX_RETENTION_BATCH <= 10_000);
    assert!(GC_ENTRIES_PER_PASS >= 1);
    assert!(FIRST_PASS_DELAY.as_secs() < PASS_INTERVAL.as_secs());
};
