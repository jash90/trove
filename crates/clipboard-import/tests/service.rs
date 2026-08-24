use std::{
    fs,
    path::Path,
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

use clipboard_core::{
    CaptureInput, ContentFlags, ContentKind, EventFlags, RepresentationInput, SourceConfidence,
};
use clipboard_import::{
    IMPORT_BATCH_SIZE, ImportError, ImportProgress, ImportRunHandle, ImportRunState, ImportService,
    ImportWorkerPolicy, MAX_IMPORT_AUXILIARY_BYTES, MAX_IMPORT_MANIFEST_BYTES,
};
use clipboard_store::{StoreConfig, StoreHandle};
use serde_json::{Value, json};
use tokio::sync::Notify;

/// The smallest real PNG, so a record that references an image resolves.
fn synthetic_png() -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/supercmd/images/sample.png"),
    )
    .unwrap()
}

fn open_store(directory: &tempfile::TempDir) -> StoreHandle {
    StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap()
}

fn raycast_record(index: usize) -> Value {
    json!({
        "createdAt": format!("2026-08-22T12:{:02}:{:02}.000Z", (index / 60) % 60, index % 60),
        "modifiedAt": format!("2026-08-22T12:{:02}:{:02}.000Z", (index / 60) % 60, index % 60),
        "category": "text",
        "copyCount": 1,
        "applicationPath": "/Applications/Synthetic Editor.app",
        "text": format!("synthetic record {index}"),
    })
}

fn supercmd_text_record(index: usize) -> Value {
    json!({
        "copied_at": format!(
            "2026-08-22T12:{:02}:{:02}.000Z",
            (index / 60) % 60,
            index % 60
        ),
        "type": "text",
        "source_app": "Synthetic Editor",
        "bundle_id": "com.example.synthetic-editor",
        "text": format!("synthetic supercmd record {index}"),
        "has_image": false,
    })
}

fn supercmd_records_with_late_image() -> Vec<Value> {
    let mut records = (0..IMPORT_BATCH_SIZE)
        .map(supercmd_text_record)
        .collect::<Vec<_>>();
    records.push(json!({
        "copied_at": "2026-08-22T12:59:59.000Z",
        "type": "image",
        "source_app": "Synthetic Viewer",
        "bundle_id": "com.example.synthetic-viewer",
        "file_url": "images/late.png",
        "text": "",
        "ocr_text": "late image",
        "has_image": true,
    }));
    records
}

fn write_json(path: &Path, records: &[Value]) {
    fs::write(path, serde_json::to_vec(records).unwrap()).unwrap();
}

fn write_raycast_export(directory: &tempfile::TempDir, records: &[Value]) {
    write_json(&directory.path().join("clipboard.json"), records);
}

fn write_supercmd_export(directory: &tempfile::TempDir, records: &[Value]) {
    write_json(&directory.path().join("clipboard.json"), records);
}

/// Reads a run's progress, treating "not yet written" as not yet.
///
/// `begin` hands the work to a detached task and returns; `status` reads the
/// row that task writes. Between those two moments the run legitimately does
/// not exist yet, and a poll that lands there is early rather than wrong. This
/// machine never lost that race, a CI runner did — so unwrapping here made the
/// helper depend on which machine ran it.
fn poll_progress(service: &ImportService, run_id: uuid::Uuid) -> Option<ImportProgress> {
    match service.status(run_id) {
        Ok(progress) => Some(progress),
        Err(ImportError::Service {
            reason: "run_not_found",
        }) => None,
        Err(error) => panic!("import status failed: {error}"),
    }
}

async fn wait_for_terminal(service: &ImportService, run_id: uuid::Uuid) {
    for _ in 0..50_000 {
        if let Some(progress) = poll_progress(service, run_id)
            && progress.state != ImportRunState::Running
        {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("import worker did not reach a terminal state");
}

/// The run's progress once the worker has stopped moving it.
///
/// Only for a run with nothing left to do. A run interrupted mid-way stays
/// `Running` on purpose, so that it can be resumed — waiting for a terminal
/// state there waits forever.
///
/// Where it does apply, the counter reaches its mark before the state that
/// follows it is written, so a snapshot taken at the checkpoint records a
/// `Running` the worker is about to replace. Comparing a later read against
/// that snapshot then passes on one machine and fails on another; CI found
/// exactly that. Anything asserting "this did not change the run" has to start
/// from a state that had finished changing.
async fn settled_status(service: &ImportService, run_id: uuid::Uuid) -> ImportProgress {
    wait_for_terminal(service, run_id).await;
    service.status(run_id).unwrap()
}

async fn wait_for_processed(service: &ImportService, run_id: uuid::Uuid, processed: u64) {
    for _ in 0..50_000 {
        if let Some(progress) = poll_progress(service, run_id)
            && progress.processed == processed
        {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("import worker did not reach the requested checkpoint");
}

async fn begin_analyzed(service: &ImportService, path: &Path) -> ImportRunHandle {
    let analysis = service.analyze(path).unwrap();
    service.begin(analysis.analysis_id).await.unwrap()
}

#[test]
fn async_run_to_completion_yields_while_operation_admission_is_busy() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    write_raycast_export(&export, &[raycast_record(0)]);
    let service = ImportService::new(open_store(&database)).unwrap();
    let held_permit = clipboard_store::ImportOperationGate::process_wide()
        .acquire_blocking()
        .unwrap();
    let (heartbeat_tx, heartbeat_rx) = mpsc::channel();
    let (result_tx, result_rx) = mpsc::channel();
    let path = export.path().to_path_buf();
    let worker = thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = runtime.block_on(async move {
            tokio::spawn(async move {
                tokio::task::yield_now().await;
                heartbeat_tx.send(()).unwrap();
            });
            service.run_to_completion(path).await
        });
        result_tx.send(result).unwrap();
    });

    let yielded_before_release = heartbeat_rx
        .recv_timeout(Duration::from_millis(250))
        .is_ok();
    drop(held_permit);
    let result = result_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    worker.join().unwrap();

    assert!(
        yielded_before_release,
        "async preparation blocked the single Tokio worker"
    );
    result.unwrap();
}

#[tokio::test]
async fn a_record_whose_source_file_is_gone_is_skipped_not_imported() {
    let sandbox = tempfile::tempdir().unwrap();
    let export = sandbox.path().join("raycast");
    let present = sandbox.path().join("obecny.pdf");
    std::fs::create_dir(&export).unwrap();
    std::fs::write(&present, b"synthetic").unwrap();
    std::fs::write(
        export.join("clipboard.json"),
        serde_json::to_vec(&serde_json::json!([
            {
                "createdAt": "2026-01-02T03:04:05Z", "modifiedAt": "2026-01-02T03:04:05Z",
                "category": "file", "copyCount": 1, "text": "obecny.pdf",
                "filePath": present.to_str().unwrap()
            },
            {
                "createdAt": "2026-01-02T03:05:05Z", "modifiedAt": "2026-01-02T03:05:05Z",
                "category": "file", "copyCount": 1, "text": "znikniety.pdf",
                "filePath": "/synthetic/nigdy/nie/istnial.pdf"
            },
            {
                "createdAt": "2026-01-02T03:06:05Z", "modifiedAt": "2026-01-02T03:06:05Z",
                "category": "text", "copyCount": 1, "text": "zwykly tekst"
            }
        ]))
        .unwrap(),
    )
    .unwrap();

    let database = tempfile::tempdir().unwrap();
    let service = ImportService::new(open_store(&database)).unwrap();
    let analysis = service.analyze(&export).unwrap();

    // Every source record is still accounted for; one of them simply leads
    // nowhere and is left out instead of adding an unusable row.
    assert_eq!(analysis.total, 3);
    assert_eq!(analysis.candidate_records, 2);
    assert_eq!(analysis.skipped, 1);
    assert_eq!(analysis.failed, 0);

    let summary = service.run_to_completion(&export).await.unwrap();
    assert_eq!(summary.total, 3);
    assert_eq!(summary.imported, 2);
    assert_eq!(summary.skipped, 1);
    assert_eq!(summary.failed, 0);
    assert_eq!(
        summary.imported + summary.already_present + summary.skipped + summary.failed,
        summary.total
    );
}

#[tokio::test]
async fn reimport_is_idempotent_and_duplicate_source_rows_remain_distinct_events() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    let duplicate = raycast_record(0);
    write_raycast_export(&export, &[duplicate.clone(), duplicate, raycast_record(1)]);
    let store = open_store(&database);
    let service = ImportService::new(store.clone()).unwrap();

    let first = service.run_to_completion(export.path()).await.unwrap();
    let second = service.run_to_completion(export.path()).await.unwrap();

    assert_eq!(first.total, 3);
    assert_eq!(first.imported, 3);
    assert_eq!(first.already_present, 0);
    assert_eq!(
        first.total,
        first.imported + first.already_present + first.skipped + first.failed
    );
    assert_eq!(second.total, 3);
    assert_eq!(second.imported, 0);
    assert_eq!(second.already_present, 3);
    assert_eq!(
        second.total,
        second.imported + second.already_present + second.skipped + second.failed
    );
    assert_eq!(store.stats().unwrap().event_count, 3);
    assert_eq!(store.stats().unwrap().content_count, 2);
}

#[tokio::test]
async fn parser_failure_in_the_middle_is_counted_once_and_later_candidates_import() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    let mut invalid = raycast_record(1);
    invalid["copyCount"] = json!(0);
    write_raycast_export(&export, &[raycast_record(0), invalid, raycast_record(2)]);
    let store = open_store(&database);
    let service = ImportService::new(store.clone()).unwrap();

    let analysis = service.analyze(export.path()).unwrap();
    let summary = service.run_to_completion(export.path()).await.unwrap();

    assert_eq!(analysis.total, 3);
    assert_eq!(analysis.candidate_records, 2);
    assert_eq!(analysis.failed, 1);
    assert_eq!(summary.total, 3);
    assert_eq!(summary.imported, 2);
    assert_eq!(summary.failed, 1);
    assert_eq!(
        summary.total,
        summary.imported + summary.already_present + summary.skipped + summary.failed
    );
    assert_eq!(store.stats().unwrap().event_count, 2);
    let failure_count = store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT count FROM import_failure_reason WHERE reason_code = 'invalid_copy_count'",
                [],
                |row| row.get::<_, i64>(0),
            )
        })
        .unwrap();
    assert_eq!(failure_count, 1);

    let serialized = serde_json::to_value(&analysis).unwrap();
    assert_eq!(
        serialized,
        json!({
            "analysisId": analysis.analysis_id,
            "total": 3,
            "candidateRecords": 2,
            "skipped": 0,
            "failed": 1
        })
    );
}

#[tokio::test]
async fn begin_consumes_the_exact_prepared_snapshot_once_without_reparsing() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    fs::create_dir(export.path().join("images")).unwrap();
    let image_path = export.path().join("images/synthetic.png");
    fs::write(&image_path, b"analyzed image bytes").unwrap();
    write_supercmd_export(
        &export,
        &[json!({
            "copied_at": "2026-08-22T12:00:00Z",
            "type": "image",
            "source_app": "Synthetic Viewer",
            "bundle_id": "com.example.synthetic-viewer",
            "file_url": "synthetic.png",
            "text": "",
            "ocr_text": "confirmed snapshot",
            "has_image": true,
        })],
    );
    let store = open_store(&database);
    let service = ImportService::new(store.clone()).unwrap();
    let analysis = service.analyze(export.path()).unwrap();
    fs::write(&image_path, b"changed after analysis").unwrap();
    let handle = service.begin(analysis.analysis_id).await.unwrap();
    assert_eq!(handle.run_id, analysis.analysis_id);
    wait_for_terminal(&service, handle.run_id).await;

    let stored_relpath = store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT blob_relpath FROM raw_payload WHERE storage_kind = 'cas'",
                [],
                |row| row.get::<_, String>(0),
            )
        })
        .unwrap();
    let stored_payload = fs::read(store.config().blob_root().join(stored_relpath)).unwrap();
    assert_eq!(stored_payload, b"analyzed image bytes");
    let recovered = service.begin(analysis.analysis_id).await.unwrap();
    assert_eq!(recovered, handle);

    let unknown = service.begin(uuid::Uuid::now_v7()).await.unwrap_err();
    assert!(matches!(
        unknown,
        ImportError::Service {
            reason: "analysis_not_found"
        }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborted_begin_caller_cannot_cancel_durable_handoff_or_worker_startup() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    write_raycast_export(&export, &[raycast_record(0)]);
    let store = open_store(&database);
    let persistence_started = Arc::new(Notify::new());
    let persistence_gate = Arc::new(Notify::new());
    let service = ImportService::with_worker_policy(
        store.clone(),
        ImportWorkerPolicy::wait_before_persistence(
            persistence_started.clone(),
            persistence_gate.clone(),
        ),
    )
    .unwrap();
    let analysis = service.analyze(export.path()).unwrap();

    let caller_service = service.clone();
    let caller = tokio::spawn(async move { caller_service.begin(analysis.analysis_id).await });
    persistence_started.notified().await;
    caller.abort();
    let _ = caller.await;
    persistence_gate.notify_one();

    wait_for_terminal(&service, analysis.analysis_id).await;
    let recovered = service.begin(analysis.analysis_id).await.unwrap();
    assert_eq!(recovered.run_id, analysis.analysis_id);
    assert_eq!(store.stats().unwrap().event_count, 1);
    let run_count = store
        .with_reader(|connection| {
            connection.query_row("SELECT count(*) FROM import_run", [], |row| {
                row.get::<_, i64>(0)
            })
        })
        .unwrap();
    assert_eq!(run_count, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persistence_failure_restores_the_exact_analysis_for_retry() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    write_raycast_export(&export, &[raycast_record(0)]);
    let store = open_store(&database);
    let service = ImportService::with_worker_policy(
        store.clone(),
        ImportWorkerPolicy::fail_next_persistence(),
    )
    .unwrap();
    let analysis = service.analyze(export.path()).unwrap();

    let error = service.begin(analysis.analysis_id).await.unwrap_err();
    assert!(matches!(
        error,
        ImportError::Service {
            reason: "store_failure"
        }
    ));
    assert!(matches!(
        service.status(analysis.analysis_id),
        Err(ImportError::Service {
            reason: "run_not_found"
        })
    ));

    let recovered = service.begin(analysis.analysis_id).await.unwrap();
    assert_eq!(recovered.run_id, analysis.analysis_id);
    wait_for_terminal(&service, recovered.run_id).await;
    assert_eq!(store.stats().unwrap().event_count, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn durable_handoff_uses_the_writer_lease_without_a_post_commit_status_read() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    write_raycast_export(&export, &[raycast_record(0)]);
    let store = open_store(&database);
    let service = ImportService::new(store.clone()).unwrap();
    let analysis = service.analyze(export.path()).unwrap();

    store.set_import_status_available_for_test(false);
    let handoff = service.begin(analysis.analysis_id).await;
    store.set_import_status_available_for_test(true);

    let handle = handoff.unwrap();
    wait_for_terminal(&service, handle.run_id).await;
    assert_eq!(store.stats().unwrap().event_count, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_post_commit_offset_restores_the_exact_analysis_for_retry() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    write_raycast_export(&export, &[raycast_record(0)]);
    let store = open_store(&database);
    let service = ImportService::with_worker_policy(
        store.clone(),
        ImportWorkerPolicy::invalid_lease_offset_once(),
    )
    .unwrap();
    let analysis = service.analyze(export.path()).unwrap();

    let error = service.begin(analysis.analysis_id).await.unwrap_err();
    assert!(matches!(
        error,
        ImportError::Service {
            reason: "invalid_run_state"
        }
    ));
    let persisted = service.status(analysis.analysis_id).unwrap();
    assert_eq!(persisted.state, ImportRunState::Running);
    assert_eq!(persisted.processed, 0);

    let recovered = service.begin(analysis.analysis_id).await.unwrap();
    wait_for_terminal(&service, recovered.run_id).await;
    assert_eq!(store.stats().unwrap().event_count, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_existing_run_returns_its_known_handle_without_starting_a_worker() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    write_raycast_export(&export, &[raycast_record(0)]);
    let store = open_store(&database);
    let service = ImportService::with_worker_policy(
        store.clone(),
        ImportWorkerPolicy::invalid_lease_offset_once(),
    )
    .unwrap();
    let analysis = service.analyze(export.path()).unwrap();
    service.begin(analysis.analysis_id).await.unwrap_err();
    store
        .fail_import(analysis.analysis_id, 1, "synthetic_terminal")
        .await
        .unwrap();

    let recovered = service.begin(analysis.analysis_id).await.unwrap();
    tokio::task::yield_now().await;

    assert_eq!(recovered.run_id, analysis.analysis_id);
    let terminal = service.status(recovered.run_id).unwrap();
    assert_eq!(terminal.state, ImportRunState::Failed);
    assert_eq!(terminal.error_code.as_deref(), Some("synthetic_terminal"));
    assert_eq!(store.stats().unwrap().event_count, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_begin_and_lost_response_retries_share_one_persisted_run() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    write_raycast_export(&export, &[raycast_record(0)]);
    let store = open_store(&database);
    let persistence_started = Arc::new(Notify::new());
    let persistence_gate = Arc::new(Notify::new());
    let service = ImportService::with_worker_policy(
        store.clone(),
        ImportWorkerPolicy::wait_before_persistence(
            persistence_started.clone(),
            persistence_gate.clone(),
        ),
    )
    .unwrap();
    let analysis = service.analyze(export.path()).unwrap();

    let first_service = service.clone();
    let first = tokio::spawn(async move { first_service.begin(analysis.analysis_id).await });
    persistence_started.notified().await;
    let second_service = service.clone();
    let second = tokio::spawn(async move { second_service.begin(analysis.analysis_id).await });
    tokio::task::yield_now().await;
    persistence_gate.notify_one();

    let first_handle = first.await.unwrap().unwrap();
    let second_handle = second.await.unwrap().unwrap();
    assert_eq!(first_handle, second_handle);
    assert_eq!(first_handle.run_id, analysis.analysis_id);
    wait_for_terminal(&service, analysis.analysis_id).await;
    assert_eq!(
        service.begin(analysis.analysis_id).await.unwrap(),
        first_handle
    );
    assert_eq!(store.stats().unwrap().event_count, 1);
    let run_count = store
        .with_reader(|connection| {
            connection.query_row("SELECT count(*) FROM import_run", [], |row| {
                row.get::<_, i64>(0)
            })
        })
        .unwrap();
    assert_eq!(run_count, 1);
}

#[test]
fn oversized_manifest_is_rejected_before_reading_with_a_path_free_error() {
    let export = tempfile::tempdir().unwrap();
    let manifest = export.path().join("clipboard.json");
    let file = fs::File::create(&manifest).unwrap();
    file.set_len((MAX_IMPORT_MANIFEST_BYTES as u64) + 1)
        .unwrap();
    let database = tempfile::tempdir().unwrap();
    let service = ImportService::new(open_store(&database)).unwrap();

    let error = service.analyze(export.path()).unwrap_err();

    assert!(matches!(
        error,
        ImportError::Service {
            reason: "analysis_too_large"
        }
    ));
    let rendered = error.to_string();
    assert!(!rendered.contains("clipboard.json"));
    assert!(!rendered.contains(export.path().to_string_lossy().as_ref()));
}

#[tokio::test]
async fn an_oversized_auxiliary_payload_is_skipped_without_aborting_the_analysis() {
    let export = tempfile::tempdir().unwrap();
    fs::create_dir(export.path().join("images")).unwrap();
    let auxiliary = fs::File::create(export.path().join("images/large.png")).unwrap();
    auxiliary
        .set_len((MAX_IMPORT_AUXILIARY_BYTES as u64) + 1)
        .unwrap();
    write_supercmd_export(
        &export,
        &[json!({
            "copied_at": "2026-08-22T12:00:00Z",
            "type": "image",
            "source_app": "Synthetic Viewer",
            "bundle_id": "com.example.synthetic-viewer",
            "file_url": "large.png",
            "text": "",
            "ocr_text": "bounded auxiliary",
            "has_image": true,
        })],
    );
    let database = tempfile::tempdir().unwrap();
    let store = open_store(&database);
    let service = ImportService::new(store.clone()).unwrap();

    let analysis = service.analyze(export.path()).unwrap();
    // The record is still accounted for, but an entry with no payload and
    // no reachable source would only add a row nobody can read or open.
    assert_eq!(
        (
            analysis.total,
            analysis.candidate_records,
            analysis.skipped,
            analysis.failed
        ),
        (1, 0, 1, 0)
    );
    let handle = service.begin(analysis.analysis_id).await.unwrap();
    wait_for_terminal(&service, handle.run_id).await;

    let events = store
        .with_reader(|connection| {
            connection.query_row("SELECT count(*) FROM history_event", [], |row| {
                row.get::<_, i64>(0)
            })
        })
        .unwrap();
    assert_eq!(events, 0);
}

#[tokio::test]
async fn discarded_analysis_cannot_start_and_does_not_disclose_its_token_or_path() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    write_raycast_export(&export, &[raycast_record(0)]);
    let service = ImportService::new(open_store(&database)).unwrap();
    let analysis = service.analyze(export.path()).unwrap();

    service.discard_analysis(analysis.analysis_id).unwrap();
    let discard_error = service.discard_analysis(analysis.analysis_id).unwrap_err();
    let error = service.begin(analysis.analysis_id).await.unwrap_err();

    assert!(matches!(
        error,
        ImportError::Service {
            reason: "analysis_not_found"
        }
    ));
    let rendered = error.to_string();
    assert!(!rendered.contains(&analysis.analysis_id.to_string()));
    assert!(!rendered.contains(export.path().to_string_lossy().as_ref()));
    assert!(
        !discard_error
            .to_string()
            .contains(&analysis.analysis_id.to_string())
    );
    assert!(
        !discard_error
            .to_string()
            .contains(export.path().to_string_lossy().as_ref())
    );
}

#[tokio::test]
async fn begin_returns_a_handle_and_status_survives_reopening_after_completion() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    write_raycast_export(&export, &[raycast_record(0), raycast_record(1)]);
    let store = open_store(&database);
    let service = ImportService::new(store.clone()).unwrap();

    let handle = begin_analyzed(&service, export.path()).await;
    wait_for_terminal(&service, handle.run_id).await;
    let terminal = service.status(handle.run_id).unwrap();

    assert_eq!(handle.run_id.get_version_num(), 7);
    assert_eq!(terminal.state, ImportRunState::Completed);
    assert_eq!(terminal.processed, 2);
    assert_eq!(terminal.total, 2);
    assert_eq!(terminal.summary.as_ref().unwrap().imported, 2);
    assert_eq!(
        serde_json::to_value(handle).unwrap(),
        json!({"runId": handle.run_id})
    );
    assert_eq!(
        serde_json::to_value(&terminal).unwrap(),
        json!({
            "runId": handle.run_id,
            "state": "completed",
            "processed": 2,
            "total": 2,
            "imported": 2,
            "alreadyPresent": 0,
            "skipped": 0,
            "failed": 0,
            "errorCode": null,
            "summary": {
                "runId": handle.run_id,
                "total": 2,
                "imported": 2,
                "alreadyPresent": 0,
                "skipped": 0,
                "failed": 0
            }
        })
    );
    let (stored_id_size, stored_id_type) = store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT length(external_id), typeof(external_id)
                 FROM import_run WHERE external_id = ?1",
                [handle.run_id.as_bytes().as_slice()],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
        })
        .unwrap();
    assert_eq!((stored_id_size, stored_id_type.as_str()), (16, "blob"));
    drop(service);
    drop(store);

    let reopened = ImportService::new(open_store(&database)).unwrap();
    let persisted = reopened.status(handle.run_id).unwrap();
    assert_eq!(persisted, terminal);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupted_first_batch_resumes_after_reopen_without_duplicate_events() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    let records = (0..501).map(raycast_record).collect::<Vec<_>>();
    write_raycast_export(&export, &records);
    let store = open_store(&database);
    let service = ImportService::with_worker_policy(
        store.clone(),
        ImportWorkerPolicy::interrupt_after_batches(1),
    )
    .unwrap();

    let handle = begin_analyzed(&service, export.path()).await;
    wait_for_processed(&service, handle.run_id, IMPORT_BATCH_SIZE as u64).await;
    let checkpoint = service.status(handle.run_id).unwrap();
    assert_eq!(checkpoint.state, ImportRunState::Running);
    assert_eq!(checkpoint.processed, 250);
    let persisted_offset = store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT next_candidate_offset FROM import_run WHERE external_id = ?1",
                [handle.run_id.as_bytes().as_slice()],
                |row| row.get::<_, i64>(0),
            )
        })
        .unwrap();
    assert_eq!(persisted_offset, 250);
    drop(service);
    drop(store);

    let reopened_store = open_store(&database);
    let reopened = ImportService::new(reopened_store.clone()).unwrap();
    reopened.resume(handle.run_id, export.path()).await.unwrap();
    wait_for_terminal(&reopened, handle.run_id).await;
    let completed = reopened.status(handle.run_id).unwrap();

    assert_eq!(completed.state, ImportRunState::Completed);
    assert_eq!(completed.processed, 501);
    assert_eq!(completed.imported, 501);
    assert_eq!(reopened_store.stats().unwrap().event_count, 501);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_rejects_a_changed_manifest_without_mutating_the_run_or_persisting_its_path() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    let records = (0..251).map(raycast_record).collect::<Vec<_>>();
    write_raycast_export(&export, &records);
    let store = open_store(&database);
    let service = ImportService::with_worker_policy(
        store.clone(),
        ImportWorkerPolicy::interrupt_after_batches(1),
    )
    .unwrap();
    let handle = begin_analyzed(&service, export.path()).await;
    wait_for_processed(&service, handle.run_id, 250).await;
    let before = service.status(handle.run_id).unwrap();

    let mut changed = records;
    changed[0]["text"] = json!("changed synthetic record");
    write_raycast_export(&export, &changed);
    let error = service
        .resume(handle.run_id, export.path())
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        ImportError::Service {
            reason: "source_mismatch"
        }
    ));
    assert!(
        !error
            .to_string()
            .contains(export.path().to_string_lossy().as_ref())
    );
    assert!(!error.to_string().contains("clipboard.json"));
    assert_eq!(service.status(handle.run_id).unwrap(), before);
    assert_eq!(store.stats().unwrap().event_count, 250);

    let columns = store
        .with_reader(|connection| {
            let mut statement = connection.prepare("PRAGMA table_info(import_run)")?;
            statement
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap();
    assert!(columns.iter().all(|column| !column.contains("path")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_rejects_changed_auxiliary_image_bytes_without_mutating_the_run() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    fs::create_dir(export.path().join("images")).unwrap();
    let image_path = export.path().join("images/late.png");
    fs::write(&image_path, b"image bytes at analysis").unwrap();
    let records = supercmd_records_with_late_image();
    write_supercmd_export(&export, &records);
    let store = open_store(&database);
    let service = ImportService::with_worker_policy(
        store.clone(),
        ImportWorkerPolicy::interrupt_after_batches(1),
    )
    .unwrap();
    let handle = begin_analyzed(&service, export.path()).await;
    wait_for_processed(&service, handle.run_id, IMPORT_BATCH_SIZE as u64).await;
    let before = service.status(handle.run_id).unwrap();
    assert_eq!(store.stats().unwrap().event_count, IMPORT_BATCH_SIZE as i64);
    drop(service);
    drop(store);

    fs::write(&image_path, b"image bytes changed after checkpoint").unwrap();
    let reopened_store = open_store(&database);
    let reopened = ImportService::new(reopened_store.clone()).unwrap();
    let error = reopened
        .resume(handle.run_id, export.path())
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        ImportError::Service {
            reason: "source_mismatch"
        }
    ));
    assert!(!error.to_string().contains("late.png"));
    assert_eq!(reopened.status(handle.run_id).unwrap(), before);
    assert_eq!(
        reopened_store.stats().unwrap().event_count,
        IMPORT_BATCH_SIZE as i64
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_rejects_an_auxiliary_image_becoming_available_after_checkpoint() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    write_supercmd_export(&export, &supercmd_records_with_late_image());
    let store = open_store(&database);
    let service = ImportService::with_worker_policy(
        store.clone(),
        ImportWorkerPolicy::interrupt_after_batches(1),
    )
    .unwrap();
    let handle = begin_analyzed(&service, export.path()).await;
    // The image record leads nowhere, so it is skipped before the run starts:
    // one skipped plus a full batch of imported records is the checkpoint.
    wait_for_processed(&service, handle.run_id, IMPORT_BATCH_SIZE as u64 + 1).await;
    let before = settled_status(&service, handle.run_id).await;
    drop(service);
    drop(store);

    fs::create_dir(export.path().join("images")).unwrap();
    fs::write(
        export.path().join("images/late.png"),
        b"newly available image",
    )
    .unwrap();
    let reopened_store = open_store(&database);
    let reopened = ImportService::new(reopened_store.clone()).unwrap();
    let error = reopened
        .resume(handle.run_id, export.path())
        .await
        .unwrap_err();

    // The image is there now, so a reparse would no longer skip that record.
    // A resume must notice the source no longer matches rather than quietly
    // importing a record the first pass had accounted for as skipped.
    assert!(matches!(
        error,
        ImportError::Service {
            reason: "source_mismatch"
        }
    ));
    assert_eq!(reopened.status(handle.run_id).unwrap(), before);
    assert_eq!(
        reopened_store.stats().unwrap().event_count,
        IMPORT_BATCH_SIZE as i64
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overlapping_resumes_supersede_the_stale_worker_without_failing_the_run() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    let records = (0..751).map(raycast_record).collect::<Vec<_>>();
    write_raycast_export(&export, &records);
    let store = open_store(&database);
    let initial = ImportService::with_worker_policy(
        store.clone(),
        ImportWorkerPolicy::interrupt_after_batches(1),
    )
    .unwrap();
    let handle = begin_analyzed(&initial, export.path()).await;
    wait_for_processed(&initial, handle.run_id, 250).await;

    let start_gate = Arc::new(Notify::new());
    let stale_finished = Arc::new(Notify::new());
    let stale = ImportService::with_worker_policy(
        store.clone(),
        ImportWorkerPolicy::wait_before_work(start_gate.clone(), stale_finished.clone()),
    )
    .unwrap();
    stale.resume(handle.run_id, export.path()).await.unwrap();

    let advancing = ImportService::with_worker_policy(
        store.clone(),
        ImportWorkerPolicy::interrupt_after_batches(1),
    )
    .unwrap();
    advancing
        .resume(handle.run_id, export.path())
        .await
        .unwrap();
    wait_for_processed(&advancing, handle.run_id, 500).await;

    start_gate.notify_one();
    stale_finished.notified().await;
    let after_stale_worker = advancing.status(handle.run_id).unwrap();
    assert_eq!(after_stale_worker.state, ImportRunState::Running);
    assert_eq!(after_stale_worker.processed, 500);
    assert_eq!(after_stale_worker.error_code, None);

    let finisher = ImportService::new(store.clone()).unwrap();
    finisher.resume(handle.run_id, export.path()).await.unwrap();
    wait_for_terminal(&finisher, handle.run_id).await;
    let completed = finisher.status(handle.run_id).unwrap();
    assert_eq!(completed.state, ImportRunState::Completed);
    assert_eq!(completed.imported, 751);
    assert_eq!(store.stats().unwrap().event_count, 751);
}

#[tokio::test]
async fn raycast_and_supercmd_records_with_no_reachable_source_are_skipped() {
    let raycast = tempfile::tempdir().unwrap();
    let supercmd = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    write_raycast_export(
        &raycast,
        &[json!({
            "createdAt": "2026-08-22T12:00:00Z",
            "modifiedAt": "2026-08-22T12:00:00Z",
            "category": "image",
            "copyCount": 1,
            "applicationPath": "/Applications/Synthetic Viewer.app",
            "filePath": "missing-raycast.png",
            "imageHash": "synthetic-raycast-ref",
            "text": "",
        })],
    );
    write_supercmd_export(
        &supercmd,
        &[json!({
            "copied_at": "2026-08-22T12:00:01Z",
            "type": "image",
            "source_app": "Synthetic Viewer",
            "bundle_id": "com.example.synthetic-viewer",
            "file_url": "missing-supercmd.png",
            "text": "",
            "ocr_text": "Synthetic missing image",
            "has_image": true,
        })],
    );
    let store = open_store(&database);
    let service = ImportService::new(store.clone()).unwrap();

    let raycast_summary = service.run_to_completion(raycast.path()).await.unwrap();
    let supercmd_summary = service.run_to_completion(supercmd.path()).await.unwrap();

    // Both records point at images that are not there. They are accounted for
    // and left out rather than stored as rows the user can neither read nor
    // open.
    assert_eq!((raycast_summary.imported, raycast_summary.skipped), (0, 1));
    assert_eq!(
        (supercmd_summary.imported, supercmd_summary.skipped),
        (0, 1)
    );
    let (events, missing_flags) = store
        .with_reader(|connection| {
            Ok((
                connection.query_row("SELECT count(*) FROM history_event", [], |row| {
                    row.get::<_, i64>(0)
                })?,
                connection.query_row(
                    "SELECT count(*) FROM content WHERE flags & ?1 != 0",
                    [i64::from(ContentFlags::MISSING_PAYLOAD.bits())],
                    |row| row.get::<_, i64>(0),
                )?,
            ))
        })
        .unwrap();
    assert_eq!((events, missing_flags), (0, 0));
}

#[tokio::test]
async fn ocr_is_search_only_and_combines_with_primary_text_without_replacing_payloads() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    // The image must actually be there: a record pointing at nothing is
    // skipped, and then it would have no search document to assert on.
    fs::create_dir(export.path().join("images")).unwrap();
    fs::write(export.path().join("images/present.png"), synthetic_png()).unwrap();
    write_supercmd_export(
        &export,
        &[
            json!({
                "copied_at": "2026-08-22T12:00:00Z",
                "type": "image",
                "source_app": "Synthetic Viewer",
                "bundle_id": "com.example.synthetic-viewer",
                "file_url": "present.png",
                "text": "",
                "ocr_text": "ŁÓDŹ image words",
                "has_image": true,
            }),
            json!({
                "copied_at": "2026-08-22T12:00:01Z",
                "type": "text",
                "source_app": "Synthetic Editor",
                "bundle_id": "com.example.synthetic-editor",
                "text": "Primary payload",
                "ocr_text": "Secondary OCR phrase",
                "has_image": false,
            }),
        ],
    );
    let store = open_store(&database);
    let service = ImportService::new(store.clone()).unwrap();

    service.run_to_completion(export.path()).await.unwrap();

    let (image_matches, primary_matches, ocr_matches, primary_payload) = store
        .with_reader(|connection| {
            Ok((
                connection.query_row(
                    "SELECT count(*) FROM search_fts WHERE search_fts MATCH 'lodz'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
                connection.query_row(
                    "SELECT count(*) FROM search_fts WHERE search_fts MATCH 'primary'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
                connection.query_row(
                    "SELECT count(*) FROM search_fts WHERE search_fts MATCH 'secondary'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
                connection.query_row(
                    "SELECT rp.inline_payload FROM raw_payload rp
                     JOIN event_representation er ON er.raw_payload_id = rp.raw_payload_id
                     JOIN history_event he ON he.event_id = er.event_id
                     JOIN content c ON c.content_id = he.content_id
                     WHERE c.kind = 'text' AND er.format_id = 'text/plain' AND er.ordinal = 0",
                    [],
                    |row| row.get::<_, Vec<u8>>(0),
                )?,
            ))
        })
        .unwrap();
    assert_eq!((image_matches, primary_matches, ocr_matches), (1, 1, 1));
    assert_eq!(primary_payload, b"Primary payload");
}

#[tokio::test]
async fn equal_primary_payloads_accumulate_distinct_ocr_derivations() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    write_supercmd_export(
        &export,
        &[
            json!({
                "copied_at": "2026-08-22T12:00:00Z",
                "type": "text",
                "source_app": "Synthetic Editor",
                "bundle_id": "com.example.synthetic-editor",
                "text": "Shared primary payload",
                "ocr_text": "amber derivation",
                "has_image": false,
            }),
            json!({
                "copied_at": "2026-08-22T12:00:01Z",
                "type": "text",
                "source_app": "Synthetic Editor",
                "bundle_id": "com.example.synthetic-editor",
                "text": "Shared primary payload",
                "ocr_text": "cobalt derivation",
                "has_image": false,
            }),
        ],
    );
    let store = open_store(&database);
    let service = ImportService::new(store.clone()).unwrap();

    service.run_to_completion(export.path()).await.unwrap();

    let (amber_matches, cobalt_matches, content_count, payload) = store
        .with_reader(|connection| {
            Ok((
                connection.query_row(
                    "SELECT count(*) FROM search_fts WHERE search_fts MATCH 'amber'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
                connection.query_row(
                    "SELECT count(*) FROM search_fts WHERE search_fts MATCH 'cobalt'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
                connection.query_row("SELECT count(*) FROM content", [], |row| {
                    row.get::<_, i64>(0)
                })?,
                connection.query_row(
                    "SELECT inline_payload FROM raw_payload WHERE storage_kind = 'inline'",
                    [],
                    |row| row.get::<_, Vec<u8>>(0),
                )?,
            ))
        })
        .unwrap();
    assert_eq!((amber_matches, cobalt_matches, content_count), (1, 1, 1));
    assert_eq!(payload, b"Shared primary payload");
}

#[tokio::test]
async fn matching_live_ingest_preserves_imported_ocr_without_duplicate_growth() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    write_supercmd_export(
        &export,
        &[json!({
            "copied_at": "2026-08-22T12:00:00Z",
            "type": "text",
            "source_app": "Synthetic Editor",
            "bundle_id": "com.example.synthetic-editor",
            "text": "Shared live payload",
            "ocr_text": "persistent indigo term",
            "has_image": false,
        })],
    );
    let store = open_store(&database);
    let service = ImportService::new(store.clone()).unwrap();
    service.run_to_completion(export.path()).await.unwrap();

    let live = CaptureInput {
        captured_at_ms: 2_000,
        kind: ContentKind::Text,
        primary_mime: "text/plain".to_owned(),
        representations: vec![RepresentationInput {
            format_id: "text/plain".to_owned(),
            bytes: Some(b"Shared live payload".to_vec()),
            missing_ref: None,
        }],
        source_app_id: Some("com.example.live".to_owned()),
        source_app_name: Some("Live Example".to_owned()),
        source_confidence: SourceConfidence::Declared,
        pinned: false,
        occurrence_count: 1,
        content_flags: ContentFlags::empty(),
        event_flags: EventFlags::empty(),
        display_label: None,
    };
    store.ingest(live.clone()).await.unwrap();
    let after_first_live = store
        .with_reader(|connection| {
            connection.query_row("SELECT normalized_text FROM search_doc", [], |row| {
                row.get::<_, String>(0)
            })
        })
        .unwrap();
    store.ingest(live).await.unwrap();

    let (ocr_matches, after_second_live) = store
        .with_reader(|connection| {
            Ok((
                connection.query_row(
                    "SELECT count(*) FROM search_fts WHERE search_fts MATCH 'indigo'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
                connection.query_row("SELECT normalized_text FROM search_doc", [], |row| {
                    row.get::<_, String>(0)
                })?,
            ))
        })
        .unwrap();
    assert_eq!(ocr_matches, 1);
    assert_eq!(after_second_live, after_first_live);
}

#[tokio::test]
async fn import_preserves_original_application_path_but_live_ingest_writes_null() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    write_raycast_export(&export, &[raycast_record(0)]);
    let store = open_store(&database);
    let service = ImportService::new(store.clone()).unwrap();
    service.run_to_completion(export.path()).await.unwrap();

    store
        .ingest(CaptureInput {
            captured_at_ms: 2_000,
            kind: ContentKind::Text,
            primary_mime: "text/plain".to_owned(),
            representations: vec![RepresentationInput {
                format_id: "text/plain".to_owned(),
                bytes: Some(b"live synthetic value".to_vec()),
                missing_ref: None,
            }],
            source_app_id: Some("com.example.live".to_owned()),
            source_app_name: Some("Live Example".to_owned()),
            source_confidence: SourceConfidence::Declared,
            pinned: false,
            occurrence_count: 1,
            content_flags: ContentFlags::empty(),
            event_flags: EventFlags::empty(),
            display_label: None,
        })
        .await
        .unwrap();

    let paths = store
        .with_reader(|connection| {
            let mut statement = connection
                .prepare("SELECT source_app_original FROM history_event ORDER BY event_id ASC")?;
            statement
                .query_map([], |row| row.get::<_, Option<String>>(0))?
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap();
    assert_eq!(
        paths,
        vec![Some("/Applications/Synthetic Editor.app".to_owned()), None]
    );
}

#[tokio::test]
async fn do_not_index_remains_authoritative_over_imported_ocr() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    write_supercmd_export(
        &export,
        &[json!({
            "copied_at": "2026-08-22T12:00:00Z",
            "type": "text",
            "source_app": "Synthetic Editor",
            "bundle_id": "com.example.synthetic-editor",
            "text": "   \n",
            "ocr_text": "must remain hidden",
            "has_image": false,
        })],
    );
    let store = open_store(&database);
    let service = ImportService::new(store.clone()).unwrap();

    let summary = service.run_to_completion(export.path()).await.unwrap();

    assert_eq!(summary.imported, 1);
    let (documents, derivations, matches) = store
        .with_reader(|connection| {
            Ok((
                connection.query_row("SELECT count(*) FROM search_doc", [], |row| {
                    row.get::<_, i64>(0)
                })?,
                connection.query_row("SELECT count(*) FROM search_derivation", [], |row| {
                    row.get::<_, i64>(0)
                })?,
                connection.query_row(
                    "SELECT count(*) FROM search_fts WHERE search_fts MATCH 'hidden'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
            ))
        })
        .unwrap();
    assert_eq!((documents, derivations, matches), (0, 0, 0));
}
