use std::{fs, path::Path, sync::Arc};

use clipboard_core::{
    CaptureInput, ContentFlags, ContentKind, EventFlags, RepresentationInput, SourceConfidence,
};
use clipboard_import::{
    IMPORT_BATCH_SIZE, ImportError, ImportRunState, ImportService, ImportWorkerPolicy,
};
use clipboard_store::{StoreConfig, StoreHandle};
use serde_json::{Value, json};
use tokio::sync::Notify;

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

fn write_json(path: &Path, records: &[Value]) {
    fs::write(path, serde_json::to_vec(records).unwrap()).unwrap();
}

fn write_raycast_export(directory: &tempfile::TempDir, records: &[Value]) {
    write_json(&directory.path().join("clipboard.json"), records);
}

fn write_supercmd_export(directory: &tempfile::TempDir, records: &[Value]) {
    write_json(&directory.path().join("clipboard.json"), records);
}

async fn wait_for_terminal(service: &ImportService, run_id: uuid::Uuid) {
    for _ in 0..50_000 {
        let progress = service.status(run_id).unwrap();
        if progress.state != ImportRunState::Running {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("import worker did not reach a terminal state");
}

async fn wait_for_processed(service: &ImportService, run_id: uuid::Uuid, processed: u64) {
    for _ in 0..50_000 {
        let progress = service.status(run_id).unwrap();
        if progress.processed == processed {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("import worker did not reach the requested checkpoint");
}

#[tokio::test]
async fn reimport_is_idempotent_and_duplicate_source_rows_remain_distinct_events() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    let duplicate = raycast_record(0);
    write_raycast_export(&export, &[duplicate.clone(), duplicate, raycast_record(1)]);
    let store = open_store(&database);
    let service = ImportService::new(store.clone());

    let first = service.run_to_completion(export.path()).await.unwrap();
    let second = service.run_to_completion(export.path()).await.unwrap();

    assert_eq!(first.total, 3);
    assert_eq!(first.imported, 3);
    assert_eq!(first.already_present, 0);
    assert_eq!(first.total, first.imported + first.skipped + first.failed);
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
    let service = ImportService::new(store.clone());

    let analysis = service.analyze(export.path()).unwrap();
    let summary = service.run_to_completion(export.path()).await.unwrap();

    assert_eq!(analysis.total, 3);
    assert_eq!(analysis.candidate_records, 2);
    assert_eq!(analysis.failed, 1);
    assert_eq!(summary.total, 3);
    assert_eq!(summary.imported, 2);
    assert_eq!(summary.failed, 1);
    assert_eq!(summary.total, summary.imported + summary.failed);
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

    let serialized = serde_json::to_value(analysis).unwrap();
    assert_eq!(
        serialized,
        json!({"total": 3, "candidate_records": 2, "failed": 1})
    );
}

#[tokio::test]
async fn begin_returns_a_handle_and_status_survives_reopening_after_completion() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    write_raycast_export(&export, &[raycast_record(0), raycast_record(1)]);
    let store = open_store(&database);
    let service = ImportService::new(store.clone());

    let handle = service.begin(export.path()).await.unwrap();
    wait_for_terminal(&service, handle.run_id).await;
    let terminal = service.status(handle.run_id).unwrap();

    assert_eq!(handle.run_id.get_version_num(), 7);
    assert_eq!(terminal.state, ImportRunState::Completed);
    assert_eq!(terminal.processed, 2);
    assert_eq!(terminal.total, 2);
    assert_eq!(terminal.summary.as_ref().unwrap().imported, 2);
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

    let reopened = ImportService::new(open_store(&database));
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
    );

    let handle = service.begin(export.path()).await.unwrap();
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
    let reopened = ImportService::new(reopened_store.clone());
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
    );
    let handle = service.begin(export.path()).await.unwrap();
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
async fn overlapping_resumes_supersede_the_stale_worker_without_failing_the_run() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    let records = (0..751).map(raycast_record).collect::<Vec<_>>();
    write_raycast_export(&export, &records);
    let store = open_store(&database);
    let initial = ImportService::with_worker_policy(
        store.clone(),
        ImportWorkerPolicy::interrupt_after_batches(1),
    );
    let handle = initial.begin(export.path()).await.unwrap();
    wait_for_processed(&initial, handle.run_id, 250).await;

    let start_gate = Arc::new(Notify::new());
    let stale_finished = Arc::new(Notify::new());
    let stale = ImportService::with_worker_policy(
        store.clone(),
        ImportWorkerPolicy::wait_before_work(start_gate.clone(), stale_finished.clone()),
    );
    stale.resume(handle.run_id, export.path()).await.unwrap();

    let advancing = ImportService::with_worker_policy(
        store.clone(),
        ImportWorkerPolicy::interrupt_after_batches(1),
    );
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

    let finisher = ImportService::new(store.clone());
    finisher.resume(handle.run_id, export.path()).await.unwrap();
    wait_for_terminal(&finisher, handle.run_id).await;
    let completed = finisher.status(handle.run_id).unwrap();
    assert_eq!(completed.state, ImportRunState::Completed);
    assert_eq!(completed.imported, 751);
    assert_eq!(store.stats().unwrap().event_count, 751);
}

#[tokio::test]
async fn missing_raycast_and_supercmd_payloads_are_preserved_as_missing_content() {
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
    let service = ImportService::new(store.clone());

    let raycast_summary = service.run_to_completion(raycast.path()).await.unwrap();
    let supercmd_summary = service.run_to_completion(supercmd.path()).await.unwrap();

    assert_eq!(raycast_summary.imported, 1);
    assert_eq!(supercmd_summary.imported, 1);
    let (missing_representations, zero_sized, missing_flags) = store
        .with_reader(|connection| {
            Ok((
                connection.query_row(
                    "SELECT count(*) FROM content_representation WHERE storage_kind = 'missing'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
                connection.query_row(
                    "SELECT count(*) FROM content_representation
                     WHERE storage_kind = 'missing' AND original_byte_size = 0
                       AND stored_byte_size = 0 AND inline_payload IS NULL",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
                connection.query_row(
                    "SELECT count(*) FROM content WHERE flags & ?1 != 0",
                    [i64::from(ContentFlags::MISSING_PAYLOAD.bits())],
                    |row| row.get::<_, i64>(0),
                )?,
            ))
        })
        .unwrap();
    assert_eq!(
        (missing_representations, zero_sized, missing_flags),
        (2, 2, 2)
    );
}

#[tokio::test]
async fn ocr_is_search_only_and_combines_with_primary_text_without_replacing_payloads() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    write_supercmd_export(
        &export,
        &[
            json!({
                "copied_at": "2026-08-22T12:00:00Z",
                "type": "image",
                "source_app": "Synthetic Viewer",
                "bundle_id": "com.example.synthetic-viewer",
                "file_url": "absent.png",
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
    let service = ImportService::new(store.clone());

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
                    "SELECT inline_payload FROM content_representation
                     JOIN content USING(content_id)
                     WHERE content.kind = 'text' AND format_id = 'text/plain'",
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
async fn import_preserves_original_application_path_but_live_ingest_writes_null() {
    let export = tempfile::tempdir().unwrap();
    let database = tempfile::tempdir().unwrap();
    write_raycast_export(&export, &[raycast_record(0)]);
    let store = open_store(&database);
    let service = ImportService::new(store.clone());
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
    let service = ImportService::new(store.clone());

    let summary = service.run_to_completion(export.path()).await.unwrap();

    assert_eq!(summary.imported, 1);
    let (documents, matches) = store
        .with_reader(|connection| {
            Ok((
                connection.query_row("SELECT count(*) FROM search_doc", [], |row| {
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
    assert_eq!((documents, matches), (0, 0));
}
