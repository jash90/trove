use clipboard_core::{
    CaptureInput, ContentFlags, ContentKind, EventFlags, RepresentationInput, SourceConfidence,
};
use clipboard_store::{
    BeginImportRun, ImportOperationGate, ImportSourceKind, MAX_SEARCH_DERIVATION_BYTES,
    MAX_SEARCH_DERIVATIONS_PER_CONTENT, MAX_SEARCH_DOCUMENT_BYTES, ReadOnlyStore,
    StorageBoundaryLease, StoreConfig, StoreError, StoreHandle, StoreImportCandidate,
    WRITER_QUEUE_CAPACITY, migrations,
};

use std::{
    fs,
    sync::{Arc, Barrier, mpsc},
    thread,
    time::Duration,
};

fn import_operation_permit() -> clipboard_store::ImportOperationPermit {
    ImportOperationGate::process_wide()
        .acquire_blocking()
        .unwrap()
}

#[test]
fn import_operation_permit_serializes_and_releases_its_reserved_capacity() {
    let gate = ImportOperationGate::with_capacity(4 * 1024).unwrap();
    let first = gate.acquire_blocking().unwrap();
    assert_eq!(gate.active_bytes(), 4 * 1024);

    let waiting_gate = gate.clone();
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let waiter = thread::spawn(move || {
        let second = waiting_gate.acquire_blocking().unwrap();
        acquired_tx.send(()).unwrap();
        second
    });

    assert!(matches!(
        acquired_rx.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    drop(first);
    acquired_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let second = waiter.join().unwrap();
    assert_eq!(gate.active_bytes(), 4 * 1024);
    drop(second);
    assert_eq!(gate.active_bytes(), 0);
}

#[tokio::test]
async fn reclamation_removes_only_what_the_writer_reconfirms_as_unused() {
    let directory = tempfile::tempdir().unwrap();
    let store = StoreHandle::open(StoreConfig::in_data_dir(directory.path())).unwrap();
    let cas = store.cas_store().unwrap();
    let orphan = cas.put(b"nikt do mnie nie odsyla").unwrap();

    let outcome = store
        .reclaim_orphans(vec![orphan.relpath.clone()])
        .await
        .unwrap();

    assert_eq!(outcome.examined, 1);
    assert_eq!(outcome.removed_objects, 1);
    assert_eq!(outcome.skipped_now_live, 0);
    assert!(outcome.reclaimed_bytes > 0);
    assert!(cas.read(&orphan.relpath).is_err());
}

#[tokio::test]
async fn a_candidate_referenced_again_since_the_scan_is_not_deleted() {
    let directory = tempfile::tempdir().unwrap();
    let store = StoreHandle::open(StoreConfig::in_data_dir(directory.path())).unwrap();
    let payload = vec![b'x'; 300 * 1024];
    store
        .ingest(binary_capture("image/png", &payload, 1_000))
        .await
        .unwrap();
    let relpath: String = store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT blob_relpath FROM raw_payload WHERE storage_kind = 'cas'",
                [],
                |row| row.get(0),
            )
        })
        .unwrap();

    // The scan runs outside any transaction, so a blob it saw as unused may be
    // referenced by the time reclamation gets to it. Liveness is re-checked
    // here, under the writer, rather than trusted from the scan.
    let outcome = store.reclaim_orphans(vec![relpath.clone()]).await.unwrap();

    assert_eq!(outcome.removed_objects, 0);
    assert_eq!(outcome.skipped_now_live, 1);
    assert!(store.cas_store().unwrap().read(&relpath).is_ok());
}

#[tokio::test]
async fn retention_deletes_only_what_aged_out_and_never_a_pinned_entry() {
    let directory = tempfile::tempdir().unwrap();
    let store = StoreHandle::open(StoreConfig::in_data_dir(directory.path())).unwrap();
    let day_ms = 24 * 60 * 60 * 1_000_i64;
    let now_ms = 100 * day_ms;

    let old = store
        .ingest(text_capture("stary wpis", now_ms - 40 * day_ms))
        .await
        .unwrap();
    let pinned = store
        .ingest(text_capture("an old pinned entry", now_ms - 41 * day_ms))
        .await
        .unwrap();
    store.set_pinned(pinned.event_id, true).await.unwrap();
    store
        .ingest(text_capture("a fresh entry", now_ms - day_ms))
        .await
        .unwrap();

    let policy = clipboard_store::RetentionPolicy::from_days(Some(30));
    let cutoff = policy.cutoff_ms(now_ms).unwrap();
    let outcome = store
        .run_retention_batch(cutoff, clipboard_store::MAX_RETENTION_BATCH)
        .await
        .unwrap();

    assert_eq!(outcome.deleted_events, 1);
    assert!(!outcome.more_remaining);
    let remaining = store.stats().unwrap().event_count;
    assert_eq!(remaining, 2, "the pinned and the fresh entry both stay");
    let _ = old;
}

#[tokio::test]
async fn retention_stops_at_its_batch_and_says_more_remains() {
    let directory = tempfile::tempdir().unwrap();
    let store = StoreHandle::open(StoreConfig::in_data_dir(directory.path())).unwrap();

    for index in 0..5 {
        store
            .ingest(text_capture(&format!("wpis {index}"), 1_000 + index))
            .await
            .unwrap();
    }

    // Deleting is a write on the queue capture shares, so a long cleanup must
    // yield rather than hold it.
    let outcome = store.run_retention_batch(i64::MAX, 2).await.unwrap();

    assert_eq!(outcome.deleted_events, 2);
    assert!(outcome.more_remaining);
    assert_eq!(store.stats().unwrap().event_count, 3);
}

#[tokio::test]
async fn import_operation_permit_is_consumed_by_store_batch() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    let run = store
        .begin_import(BeginImportRun {
            run_id: uuid::Uuid::now_v7(),
            source_kind: ImportSourceKind::Raycast,
            source_fingerprint: [91; 32],
            total_records: 0,
            candidate_records: 0,
            initial_failures: Vec::new(),
            initial_skips: Vec::new(),
        })
        .await
        .unwrap();
    let gate = ImportOperationGate::process_wide();
    let permit = gate.acquire_blocking().unwrap();

    let outcome = store
        .import_batch(permit, run.run_id, run.generation, Vec::new())
        .await
        .unwrap();

    assert_eq!(outcome.processed_candidates, 0);
    assert_eq!(gate.capacity(), clipboard_store::MAX_IMPORT_OPERATION_BYTES);
}

#[tokio::test]
async fn custom_import_operation_permit_is_rejected_before_enqueue() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    let run = store
        .begin_import(BeginImportRun {
            run_id: uuid::Uuid::now_v7(),
            source_kind: ImportSourceKind::Raycast,
            source_fingerprint: [90; 32],
            total_records: 0,
            candidate_records: 0,
            initial_failures: Vec::new(),
            initial_skips: Vec::new(),
        })
        .await
        .unwrap();
    let custom_gate = ImportOperationGate::with_capacity(4 * 1024).unwrap();
    let forged_permit = custom_gate.acquire_blocking().unwrap();

    let error = store
        .import_batch(forged_permit, run.run_id, run.generation, Vec::new())
        .await
        .unwrap_err();

    assert!(matches!(error, StoreError::InvalidImportInput));
    assert_eq!(custom_gate.active_bytes(), 0);
    assert_eq!(
        store
            .import_status(run.run_id)
            .unwrap()
            .next_candidate_offset,
        0
    );
}

#[tokio::test]
async fn import_operation_permit_rejects_unproven_candidate_shape_before_enqueue() {
    assert_eq!(
        clipboard_store::MAX_IMPORT_BATCH_BYTES
            .checked_add(clipboard_store::MAX_IMPORT_WRITER_SCRATCH_BYTES),
        Some(clipboard_store::MAX_IMPORT_OPERATION_BYTES)
    );
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    let run = store
        .begin_import(BeginImportRun {
            run_id: uuid::Uuid::now_v7(),
            source_kind: ImportSourceKind::Raycast,
            source_fingerprint: [92; 32],
            total_records: 1,
            candidate_records: 1,
            initial_failures: Vec::new(),
            initial_skips: Vec::new(),
        })
        .await
        .unwrap();
    let mut capture = text_capture("bounded", 1_000);
    capture.representations.push(RepresentationInput {
        format_id: "text/html".to_owned(),
        bytes: Some(b"bounded html".to_vec()),
        missing_ref: None,
    });
    capture.representations.push(RepresentationInput {
        format_id: "text/rtf".to_owned(),
        bytes: Some(b"bounded rtf".to_vec()),
        missing_ref: None,
    });
    let candidates = vec![StoreImportCandidate {
        candidate_offset: 0,
        record_fingerprint: [93; 32],
        capture,
        search_ocr: None,
        source_app_original: None,
    }];
    let nested_capacity = candidates[0].owned_allocation_bytes().unwrap();
    assert!(nested_capacity > 0);
    let permit = import_operation_permit();

    let error = store
        .import_batch(permit, run.run_id, run.generation, candidates)
        .await
        .unwrap_err();

    assert!(matches!(error, StoreError::ImportBatchTooLarge));
    let status = store.import_status(run.run_id).unwrap();
    assert_eq!(status.next_candidate_offset, 0);
}

#[cfg(unix)]
fn secure_existing_database(data_root: &std::path::Path, database: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(data_root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(database, fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(not(unix))]
fn secure_existing_database(_: &std::path::Path, _: &std::path::Path) {}

fn binary_capture(mime: &str, payload: &[u8], captured_at_ms: i64) -> CaptureInput {
    CaptureInput {
        captured_at_ms,
        kind: ContentKind::Image,
        primary_mime: mime.to_owned(),
        representations: vec![RepresentationInput {
            format_id: mime.to_owned(),
            bytes: Some(payload.to_vec()),
            missing_ref: None,
        }],
        source_app_id: Some("com.example.viewer".to_owned()),
        source_app_name: Some("Example Viewer".to_owned()),
        source_confidence: SourceConfidence::Declared,
        pinned: false,
        occurrence_count: 1,
        content_flags: ContentFlags::empty(),
        event_flags: EventFlags::empty(),
        display_label: None,
    }
}

fn text_capture(value: &str, captured_at_ms: i64) -> CaptureInput {
    CaptureInput {
        captured_at_ms,
        kind: ContentKind::Text,
        primary_mime: "text/plain".to_owned(),
        representations: vec![RepresentationInput {
            format_id: "public.utf8-plain-text".to_owned(),
            bytes: Some(value.as_bytes().to_vec()),
            missing_ref: None,
        }],
        source_app_id: Some("com.example.editor".to_owned()),
        source_app_name: Some("Example Editor".to_owned()),
        source_confidence: SourceConfidence::Declared,
        pinned: false,
        occurrence_count: 1,
        content_flags: ContentFlags::empty(),
        event_flags: EventFlags::empty(),
        display_label: None,
    }
}

async fn import_search_derivations(store: &StoreHandle, source_seed: u8, derivations: &[String]) {
    let run = store
        .begin_import(BeginImportRun {
            run_id: uuid::Uuid::now_v7(),
            source_kind: ImportSourceKind::SuperCmd,
            source_fingerprint: [source_seed; 32],
            total_records: derivations.len() as u64,
            candidate_records: derivations.len() as u64,
            initial_failures: Vec::new(),
            initial_skips: Vec::new(),
        })
        .await
        .unwrap();
    let candidates = derivations
        .iter()
        .enumerate()
        .map(|(offset, derivation)| {
            let mut record_fingerprint = [source_seed; 32];
            record_fingerprint[0] = source_seed.wrapping_add(offset as u8);
            record_fingerprint[1] = source_seed;
            StoreImportCandidate {
                candidate_offset: offset as u64,
                record_fingerprint,
                capture: text_capture("shared bounded payload", 1_000 + offset as i64),
                search_ocr: Some(derivation.clone()),
                source_app_original: None,
            }
        })
        .collect::<Vec<_>>();
    store
        .import_batch(
            import_operation_permit(),
            run.run_id,
            run.generation,
            candidates,
        )
        .await
        .unwrap();
    store
        .finish_import(run.run_id, run.generation)
        .await
        .unwrap();
}

#[tokio::test]
async fn import_search_derives_primary_and_ocr_inside_the_writer() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();

    import_search_derivations(&store, 61, &["secondary ocr phrase".to_owned()]).await;

    let matches = store
        .with_reader(|connection| {
            Ok((
                connection.query_row(
                    "SELECT count(*) FROM search_fts WHERE search_fts MATCH 'bounded'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
                connection.query_row(
                    "SELECT count(*) FROM search_fts WHERE search_fts MATCH 'secondary'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
            ))
        })
        .unwrap();
    assert_eq!(matches, (1, 1));
}

fn search_derivation_test_hash(value: &str) -> [u8; 32] {
    let normalized =
        clipboard_core::normalize_search_text(&format!("shared bounded payload\n{value}"));
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"clipboard-store.search-derivation-v1");
    hasher.update(&(normalized.len() as u64).to_be_bytes());
    hasher.update(normalized.as_bytes());
    *hasher.finalize().as_bytes()
}

#[tokio::test]
async fn ingest_deduplicates_content_but_preserves_events() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();

    let first = store.ingest(text_capture("Łódź", 1_000)).await.unwrap();
    let second = store.ingest(text_capture("Łódź", 2_000)).await.unwrap();

    assert_eq!(first.content_id, second.content_id);
    assert_ne!(first.event_id, second.event_id);
    assert_eq!(store.stats().unwrap().content_count, 1);
    assert_eq!(store.stats().unwrap().event_count, 2);
}

#[test]
fn migrations_are_valid() {
    migrations().validate().unwrap();
}

#[test]
fn exact_prior_pre_release_v1_schema_is_rejected_with_a_stable_error() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("history.sqlite");
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch(include_str!("fixtures/001_acd4d31.sql"))
        .unwrap();
    connection.pragma_update(None, "user_version", 1).unwrap();
    drop(connection);
    secure_existing_database(directory.path(), &database_path);

    let error = match StoreHandle::open(StoreConfig::new(&database_path)) {
        Ok(_) => panic!("the prior pre-release schema unexpectedly opened"),
        Err(error) => error,
    };

    assert!(matches!(error, StoreError::IncompatibleSchema));
    assert_eq!(
        error.to_string(),
        "database schema is incompatible; development reset required"
    );
    assert!(!error.to_string().contains("history.sqlite"));
    assert!(!error.to_string().contains("schema_identity"));
}

#[test]
fn immediately_prior_pre_release_schema_revision_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("history.sqlite");
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch(include_str!("fixtures/001_fb372cb.sql"))
        .unwrap();
    connection.pragma_update(None, "user_version", 1).unwrap();
    drop(connection);
    secure_existing_database(directory.path(), &database_path);

    let error = match StoreHandle::open(StoreConfig::new(&database_path)) {
        Ok(_) => panic!("the immediately prior pre-release schema unexpectedly opened"),
        Err(error) => error,
    };

    assert!(matches!(error, StoreError::IncompatibleSchema));
    assert!(!error.to_string().contains("history.sqlite"));
    assert!(!error.to_string().contains("revision"));
}

#[test]
fn exact_c2be0c9_pre_release_schema_revision_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("history.sqlite");
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch(include_str!("fixtures/001_c2be0c9.sql"))
        .unwrap();
    connection.pragma_update(None, "user_version", 1).unwrap();
    drop(connection);
    secure_existing_database(directory.path(), &database_path);

    let error = match StoreHandle::open(StoreConfig::new(&database_path)) {
        Ok(_) => panic!("the c2be0c9 pre-release schema unexpectedly opened"),
        Err(error) => error,
    };

    assert!(matches!(error, StoreError::IncompatibleSchema));
    assert_eq!(
        error.to_string(),
        "database schema is incompatible; development reset required"
    );
    assert!(!error.to_string().contains("history.sqlite"));
    assert!(!error.to_string().contains("revision"));
}

#[test]
fn exact_revision_four_schema_is_rejected_by_writer_and_read_only_opens() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("history.sqlite");
    let blob_root = database_path.with_extension("blobs");
    std::fs::create_dir(&blob_root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&blob_root, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch(include_str!("fixtures/001_9021c70.sql"))
        .unwrap();
    connection.pragma_update(None, "user_version", 1).unwrap();
    drop(connection);
    secure_existing_database(directory.path(), &database_path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&database_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    let database_path = Arc::new(database_path);
    let release = Arc::new(Barrier::new(5));
    let mut openers = Vec::new();
    for _ in 0..4 {
        let database_path = Arc::clone(&database_path);
        let release = Arc::clone(&release);
        openers.push(thread::spawn(move || {
            release.wait();
            match StoreHandle::open(StoreConfig::new(database_path.as_path())) {
                Ok(_) => panic!("revision four unexpectedly opened for writing"),
                Err(error) => error,
            }
        }));
    }
    release.wait();
    let writer_errors = openers
        .into_iter()
        .map(|opener| opener.join().unwrap())
        .collect::<Vec<_>>();
    let reader_error = match ReadOnlyStore::open_existing(StoreConfig::new(database_path.as_path()))
    {
        Ok(_) => panic!("revision four unexpectedly opened read-only"),
        Err(error) => error,
    };
    for error in writer_errors.into_iter().chain([reader_error]) {
        assert!(matches!(error, StoreError::IncompatibleSchema));
        assert_eq!(
            error.to_string(),
            "database schema is incompatible; development reset required"
        );
        assert!(!error.to_string().contains("history.sqlite"));
        assert!(!error.to_string().contains("revision"));
    }
}

#[test]
fn exact_revision_five_schema_is_rejected_by_concurrent_writer_and_read_only_opens() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("history.sqlite");
    let blob_root = database_path.with_extension("blobs");
    fs::create_dir(&blob_root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&blob_root, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch(include_str!("fixtures/001_3a0fdf4.sql"))
        .unwrap();
    connection.pragma_update(None, "user_version", 1).unwrap();
    drop(connection);
    secure_existing_database(directory.path(), &database_path);

    let database_path = Arc::new(database_path);
    let release = Arc::new(Barrier::new(5));
    let mut openers = Vec::new();
    for _ in 0..4 {
        let database_path = Arc::clone(&database_path);
        let release = Arc::clone(&release);
        openers.push(thread::spawn(move || {
            release.wait();
            match StoreHandle::open(StoreConfig::new(database_path.as_path())) {
                Ok(_) => panic!("revision five unexpectedly opened for writing"),
                Err(error) => error,
            }
        }));
    }
    release.wait();
    let writer_errors = openers
        .into_iter()
        .map(|opener| opener.join().unwrap())
        .collect::<Vec<_>>();
    let reader_error = match ReadOnlyStore::open_existing(StoreConfig::new(database_path.as_path()))
    {
        Ok(_) => panic!("revision five unexpectedly opened read-only"),
        Err(error) => error,
    };

    for error in writer_errors.into_iter().chain([reader_error]) {
        assert!(matches!(error, StoreError::IncompatibleSchema));
        assert_eq!(
            error.to_string(),
            "database schema is incompatible; development reset required"
        );
        assert!(!error.to_string().contains("history.sqlite"));
        assert!(!error.to_string().contains("revision"));
        assert!(!error.to_string().contains('5'));
    }
}

#[test]
fn fresh_schema_reopens_and_is_validated_on_every_open() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("history.sqlite");
    drop(StoreHandle::open(StoreConfig::new(&database_path)).unwrap());
    drop(StoreHandle::open(StoreConfig::new(&database_path)).unwrap());

    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute("DELETE FROM schema_identity", [])
        .unwrap();
    drop(connection);
    let error = match StoreHandle::open(StoreConfig::new(&database_path)) {
        Ok(_) => panic!("a schema without its identity unexpectedly reopened"),
        Err(error) => error,
    };
    assert!(matches!(error, StoreError::IncompatibleSchema));
}

#[tokio::test]
async fn sql_enforces_bounded_semantic_audit_values() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("history.sqlite");
    let store = StoreHandle::open(StoreConfig::new(&database_path)).unwrap();
    store
        .ingest(text_capture("inline constraint anchor", 1_000))
        .await
        .unwrap();
    let mut compressed = text_capture("compressed constraint anchor", 2_000);
    compressed.representations[0].bytes = Some(pseudo_random_bytes(5_000));
    store.ingest(compressed).await.unwrap();
    let mut missing = text_capture("unused", 3_000);
    missing.kind = ContentKind::Image;
    missing.primary_mime = "image/png".to_owned();
    missing.representations = vec![RepresentationInput {
        format_id: "image/png".to_owned(),
        bytes: None,
        missing_ref: Some("bounded-missing-reference".to_owned()),
    }];
    missing.content_flags = ContentFlags::MISSING_PAYLOAD;
    store.ingest(missing).await.unwrap();
    drop(store);

    let connection = rusqlite::Connection::open(&database_path).unwrap();
    let content_id: i64 = connection
        .query_row("SELECT min(content_id) FROM content", [], |row| row.get(0))
        .unwrap();
    let event_id: i64 = connection
        .query_row(
            "SELECT event_id FROM event_representation WHERE missing_ref IS NULL LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let missing_event_id: i64 = connection
        .query_row(
            "SELECT event_id FROM event_representation WHERE missing_ref IS NOT NULL LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let inline_id: i64 = connection
        .query_row(
            "SELECT raw_payload_id FROM raw_payload WHERE storage_kind = 'inline' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let compressed_id: i64 = connection
        .query_row(
            "SELECT raw_payload_id FROM raw_payload WHERE storage_kind = 'inline_zstd' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let rejected = |label: &str, result: rusqlite::Result<usize>| {
        assert!(result.is_err(), "{label} unexpectedly satisfied the schema")
    };

    for kind in ["text", "link", "image", "file", "color", "code", "html"] {
        connection
            .execute(
                "UPDATE content SET kind = ?1 WHERE content_id = ?2",
                rusqlite::params![kind, content_id],
            )
            .unwrap();
    }
    rejected(
        "unknown content kind",
        connection.execute(
            "UPDATE content SET kind = 'binary' WHERE content_id = ?1",
            [content_id],
        ),
    );
    rejected(
        "blob content kind",
        connection.execute(
            "UPDATE content SET kind = ?1 WHERE content_id = ?2",
            rusqlite::params![b"text".as_slice(), content_id],
        ),
    );

    let mime_at_limit = "ą".repeat(512);
    connection
        .execute(
            "UPDATE content SET primary_mime = ?1 WHERE content_id = ?2",
            rusqlite::params![mime_at_limit, content_id],
        )
        .unwrap();
    rejected(
        "empty primary MIME",
        connection.execute(
            "UPDATE content SET primary_mime = '' WHERE content_id = ?1",
            [content_id],
        ),
    );
    rejected(
        "oversized primary MIME",
        connection.execute(
            "UPDATE content SET primary_mime = ?1 WHERE content_id = ?2",
            rusqlite::params!["ą".repeat(513), content_id],
        ),
    );
    rejected(
        "blob primary MIME",
        connection.execute(
            "UPDATE content SET primary_mime = ?1 WHERE content_id = ?2",
            rusqlite::params![b"text/plain".as_slice(), content_id],
        ),
    );

    connection
        .execute(
            "UPDATE event_representation SET format_id = ?1 WHERE event_id = ?2",
            rusqlite::params!["ą".repeat(512), event_id],
        )
        .unwrap();
    for (label, value) in [
        ("empty format ID", String::new()),
        ("oversized format ID", "ą".repeat(513)),
    ] {
        rejected(
            label,
            connection.execute(
                "UPDATE event_representation SET format_id = ?1 WHERE event_id = ?2",
                rusqlite::params![value, event_id],
            ),
        );
    }
    rejected(
        "blob format ID",
        connection.execute(
            "UPDATE event_representation SET format_id = ?1 WHERE event_id = ?2",
            rusqlite::params![b"text/plain".as_slice(), event_id],
        ),
    );

    connection
        .execute(
            "UPDATE event_representation SET missing_ref = ?1 WHERE event_id = ?2",
            rusqlite::params!["ą".repeat(2_048), missing_event_id],
        )
        .unwrap();
    for (label, value) in [
        ("empty missing reference", String::new()),
        ("oversized missing reference", "ą".repeat(2_049)),
    ] {
        rejected(
            label,
            connection.execute(
                "UPDATE event_representation SET missing_ref = ?1 WHERE event_id = ?2",
                rusqlite::params![value, missing_event_id],
            ),
        );
    }
    rejected(
        "blob missing reference",
        connection.execute(
            "UPDATE event_representation SET missing_ref = ?1 WHERE event_id = ?2",
            rusqlite::params![b"missing".as_slice(), missing_event_id],
        ),
    );

    connection
        .execute(
            "UPDATE raw_payload
             SET inline_payload = zeroblob(4095), original_byte_size = 4095,
                 stored_byte_size = 4095
             WHERE raw_payload_id = ?1",
            [inline_id],
        )
        .unwrap();
    rejected(
        "text inline payload",
        connection.execute(
            "UPDATE raw_payload
             SET inline_payload = 'raw', original_byte_size = 3, stored_byte_size = 3
             WHERE raw_payload_id = ?1",
            [inline_id],
        ),
    );
    rejected(
        "oversized inline payload",
        connection.execute(
            "UPDATE raw_payload
             SET inline_payload = zeroblob(4096), original_byte_size = 1,
                 stored_byte_size = 4096
             WHERE raw_payload_id = ?1",
            [inline_id],
        ),
    );

    connection
        .execute(
            "UPDATE raw_payload
             SET inline_payload = zeroblob(263168), original_byte_size = 262144,
                 stored_byte_size = 263168
             WHERE raw_payload_id = ?1",
            [compressed_id],
        )
        .unwrap();
    rejected(
        "text inline-zstd payload",
        connection.execute(
            "UPDATE raw_payload
             SET inline_payload = 'compressed', original_byte_size = 4096,
                 stored_byte_size = 10
             WHERE raw_payload_id = ?1",
            [compressed_id],
        ),
    );
    rejected(
        "inline-zstd payload above compress bound",
        connection.execute(
            "UPDATE raw_payload
             SET inline_payload = zeroblob(263169), original_byte_size = 262144,
                 stored_byte_size = 263169
             WHERE raw_payload_id = ?1",
            [compressed_id],
        ),
    );

    let digest = [0x0a_u8; 32];
    let valid_relpath = format!("0a/{}", "0a".repeat(32));
    connection
        .execute(
            "INSERT INTO raw_payload(
               raw_digest, storage_kind, inline_payload, blob_relpath,
               original_byte_size, stored_byte_size
             ) VALUES (?1, 'cas', NULL, ?2, 1, 1)",
            rusqlite::params![digest.as_slice(), valid_relpath],
        )
        .unwrap();
    let cas_id = connection.last_insert_rowid();
    let wrong_path_cases = [
        ("long CAS path", format!("{valid_relpath}0")),
        ("uppercase CAS path", valid_relpath.to_uppercase()),
        ("non-hex CAS path", format!("0a/{}", "g0".repeat(32))),
        ("wrong CAS separator", format!("0a-{}", "0a".repeat(32))),
        ("CAS shard mismatch", format!("0b/{}", "0a".repeat(32))),
    ];
    for (label, value) in &wrong_path_cases {
        rejected(
            label,
            connection.execute(
                "UPDATE raw_payload SET blob_relpath = ?1 WHERE raw_payload_id = ?2",
                rusqlite::params![value, cas_id],
            ),
        );
    }
    rejected(
        "blob-typed CAS path",
        connection.execute(
            "UPDATE raw_payload SET blob_relpath = ?1 WHERE raw_payload_id = ?2",
            rusqlite::params![valid_relpath.as_bytes(), cas_id],
        ),
    );
    rejected(
        "CAS payload size above the storage bound",
        connection.execute(
            "UPDATE raw_payload
             SET original_byte_size = 134217729, stored_byte_size = 134217729
             WHERE raw_payload_id = ?1",
            [cas_id],
        ),
    );

    let artifact_kind_at_limit = "ą".repeat(32);
    connection
        .execute(
            "INSERT INTO artifact(
               content_id, artifact_kind, blob_relpath, byte_size, raw_digest, created_at_ms
             ) VALUES (?1, ?2, ?3, 1, ?4, 1)",
            rusqlite::params![
                content_id,
                artifact_kind_at_limit,
                valid_relpath,
                digest.as_slice()
            ],
        )
        .unwrap();
    let artifact_id = connection.last_insert_rowid();
    for (label, value) in [
        ("empty artifact kind", String::new()),
        ("oversized artifact kind", "ą".repeat(33)),
    ] {
        rejected(
            label,
            connection.execute(
                "UPDATE artifact SET artifact_kind = ?1 WHERE artifact_id = ?2",
                rusqlite::params![value, artifact_id],
            ),
        );
    }
    rejected(
        "blob artifact kind",
        connection.execute(
            "UPDATE artifact SET artifact_kind = ?1 WHERE artifact_id = ?2",
            rusqlite::params![b"thumbnail".as_slice(), artifact_id],
        ),
    );
    for (label, value) in &wrong_path_cases {
        rejected(
            label,
            connection.execute(
                "UPDATE artifact SET blob_relpath = ?1 WHERE artifact_id = ?2",
                rusqlite::params![value, artifact_id],
            ),
        );
    }
    rejected(
        "blob-typed artifact path",
        connection.execute(
            "UPDATE artifact SET blob_relpath = ?1 WHERE artifact_id = ?2",
            rusqlite::params![valid_relpath.as_bytes(), artifact_id],
        ),
    );
    rejected(
        "artifact size above the storage bound",
        connection.execute(
            "UPDATE artifact SET byte_size = 134217729 WHERE artifact_id = ?1",
            [artifact_id],
        ),
    );
}

#[tokio::test]
async fn writer_preflight_rejects_unbounded_semantic_metadata_and_payloads() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    let mut invalid = Vec::new();

    let mut empty_mime = text_capture("empty MIME", 1_000);
    empty_mime.primary_mime.clear();
    invalid.push(empty_mime);

    let mut long_mime = text_capture("long MIME", 2_000);
    long_mime.primary_mime = "ą".repeat(513);
    invalid.push(long_mime);

    let mut empty_format = text_capture("empty format", 3_000);
    empty_format.representations[0].format_id.clear();
    invalid.push(empty_format);

    let mut long_format = text_capture("long format", 4_000);
    long_format.representations[0].format_id = "ą".repeat(513);
    invalid.push(long_format);

    let mut empty_missing_ref = text_capture("unused", 5_000);
    empty_missing_ref.kind = ContentKind::Image;
    empty_missing_ref.primary_mime = "image/png".to_owned();
    empty_missing_ref.representations[0] = RepresentationInput {
        format_id: "image/png".to_owned(),
        bytes: None,
        missing_ref: Some(String::new()),
    };
    empty_missing_ref.content_flags = ContentFlags::MISSING_PAYLOAD;
    invalid.push(empty_missing_ref);

    let mut long_missing_ref = text_capture("unused", 6_000);
    long_missing_ref.kind = ContentKind::Image;
    long_missing_ref.primary_mime = "image/png".to_owned();
    long_missing_ref.representations[0] = RepresentationInput {
        format_id: "image/png".to_owned(),
        bytes: None,
        missing_ref: Some("ą".repeat(2_049)),
    };
    long_missing_ref.content_flags = ContentFlags::MISSING_PAYLOAD;
    invalid.push(long_missing_ref);

    for capture in invalid {
        let error = store.ingest(capture).await.unwrap_err();
        assert!(matches!(error, StoreError::PayloadStorageUnavailable));
        assert_eq!(
            error.to_string(),
            "payload storage is unavailable until CAS storage is configured"
        );
    }

    let mut oversized_payload = text_capture("unused", 7_000);
    oversized_payload.kind = ContentKind::Image;
    oversized_payload.primary_mime = "image/png".to_owned();
    oversized_payload.representations[0] = RepresentationInput {
        format_id: "image/png".to_owned(),
        bytes: Some(vec![0_u8; clipboard_store::MAX_CAS_OBJECT_BYTES + 1]),
        missing_ref: None,
    };
    let error = store.ingest(oversized_payload).await.unwrap_err();
    assert!(matches!(error, StoreError::PayloadStorageUnavailable));
    assert_eq!(store.stats().unwrap().event_count, 0);
}

#[tokio::test]
async fn writer_maps_every_content_kind_to_the_exact_schema_literal() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    let cases = [
        (ContentKind::Text, "text"),
        (ContentKind::Link, "link"),
        (ContentKind::Image, "image"),
        (ContentKind::File, "file"),
        (ContentKind::Color, "color"),
        (ContentKind::Code, "code"),
        (ContentKind::Html, "html"),
    ];
    for (index, (kind, _)) in cases.iter().enumerate() {
        let mut capture = text_capture(&format!("kind-{index}"), 10_000 + index as i64);
        capture.kind = *kind;
        store.ingest(capture).await.unwrap();
    }

    let stored = store
        .with_reader(|connection| {
            let mut statement =
                connection.prepare("SELECT kind FROM content ORDER BY content_id")?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap();

    assert_eq!(
        stored,
        cases
            .into_iter()
            .map(|(_, literal)| literal.to_owned())
            .collect::<Vec<_>>()
    );
}

#[test]
fn read_only_open_of_a_missing_database_does_not_create_it() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("missing.sqlite");

    let error = match ReadOnlyStore::open_existing(StoreConfig::new(&database_path)) {
        Ok(_) => panic!("missing database unexpectedly opened"),
        Err(error) => error,
    };

    assert!(matches!(error, StoreError::DatabaseMissing));
    assert!(!database_path.exists());
    assert_eq!(error.to_string(), "database does not exist");
}

#[test]
fn read_only_store_validates_schema_and_rejects_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("history.sqlite");
    drop(StoreHandle::open(StoreConfig::new(&database_path)).unwrap());

    let store = ReadOnlyStore::open_existing(StoreConfig::new(&database_path)).unwrap();
    let query_only = store
        .with_reader(|connection| {
            connection.query_row("PRAGMA query_only", [], |row| row.get::<_, i64>(0))
        })
        .unwrap();
    let mutation = store
        .with_reader(|connection| {
            connection.execute(
                "INSERT INTO content
                   (content_hash, kind, primary_mime, byte_size, preview_text, flags, created_at_ms)
                 VALUES (zeroblob(32), 'text', 'text/plain', 0, '', 0, 0)",
                [],
            )
        })
        .unwrap_err();

    assert_eq!(query_only, 1);
    assert!(matches!(mutation, StoreError::Database(_)));
}

#[tokio::test]
async fn opened_connections_apply_the_required_sqlite_policy() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();

    let (journal_mode, foreign_keys, synchronous, busy_timeout, cache_size, query_only) = store
        .with_reader(|connection| {
            Ok::<_, rusqlite::Error>((
                connection.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))?,
                connection.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))?,
                connection.query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))?,
                connection.query_row("PRAGMA busy_timeout", [], |row| row.get::<_, i64>(0))?,
                connection.query_row("PRAGMA cache_size", [], |row| row.get::<_, i64>(0))?,
                connection.query_row("PRAGMA query_only", [], |row| row.get::<_, i64>(0))?,
            ))
        })
        .unwrap();

    assert_eq!(journal_mode, "wal");
    assert_eq!(foreign_keys, 1);
    assert_eq!(synchronous, 1);
    assert_eq!(busy_timeout, 5_000);
    assert_eq!(cache_size, -65_536);
    assert_eq!(query_only, 1);
}

#[tokio::test]
async fn ingest_transaction_writes_normalized_search_document_and_uuid_blob() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    let outcome = store.ingest(text_capture("Łódź", 1_000)).await.unwrap();

    let (search_text, global_id_size, fts_count) = store
        .with_reader(|connection| {
            Ok::<_, rusqlite::Error>((
                connection.query_row(
                    "SELECT normalized_text FROM search_doc WHERE content_id = ?1",
                    [outcome.content_id],
                    |row| row.get::<_, String>(0),
                )?,
                connection.query_row(
                    "SELECT length(global_id) FROM history_event WHERE event_id = ?1",
                    [outcome.event_id],
                    |row| row.get::<_, i64>(0),
                )?,
                connection.query_row(
                    "SELECT count(*) FROM search_fts WHERE search_fts MATCH 'lodz'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
            ))
        })
        .unwrap();

    assert_eq!(search_text, "lodz");
    assert_eq!(global_id_size, 16);
    assert_eq!(fts_count, 1);
}

#[tokio::test]
async fn ingest_transaction_stores_a_bounded_original_utf8_preview() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    let original = format!("Łódź {}", "ą".repeat(300));
    let outcome = store.ingest(text_capture(&original, 1_000)).await.unwrap();

    let preview = store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT preview_text FROM content WHERE content_id = ?1",
                [outcome.content_id],
                |row| row.get::<_, String>(0),
            )
        })
        .unwrap();

    assert_eq!(preview, format!("Łódź {}", "ą".repeat(252)));
    assert_eq!(preview.len(), 512);
}

#[tokio::test]
async fn ingest_uses_inline_zstd_for_a_moderately_incompressible_payload() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    let bytes = pseudo_random_bytes(12_288);
    let mut capture = text_capture("small", 1_000);
    capture.representations[0].bytes = Some(bytes.clone());

    let outcome = store.ingest(capture).await.unwrap();
    let (storage_kind, original_size, stored_size, round_trip) = store
        .with_reader(|connection| {
            let (storage_kind, original_size, stored_size, compressed) = connection.query_row(
                "SELECT storage_kind, original_byte_size, stored_byte_size, inline_payload
                 FROM history_event he
                 JOIN event_representation er
                   ON er.event_id = he.event_id AND er.ordinal = 0
                 JOIN raw_payload rp ON rp.raw_payload_id = er.raw_payload_id
                 WHERE he.content_id = ?1
                 ORDER BY he.event_id DESC LIMIT 1",
                [outcome.content_id],
                |row| {
                    Ok::<_, rusqlite::Error>((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                    ))
                },
            )?;
            Ok::<_, rusqlite::Error>((
                storage_kind,
                original_size,
                stored_size,
                zstd::bulk::decompress(&compressed, bytes.len()).unwrap(),
            ))
        })
        .unwrap();

    assert_eq!(storage_kind, "inline_zstd");
    assert_eq!(original_size, bytes.len() as i64);
    assert!(stored_size >= 4_096);
    assert_eq!(round_trip, bytes);
}

fn pseudo_random_bytes(length: usize) -> Vec<u8> {
    let mut state = 0x6a09_e667_f3bc_c909_u64;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

#[tokio::test]
async fn mutations_update_and_remove_the_selected_event() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    let outcome = store.ingest(text_capture("one", 1_000)).await.unwrap();

    store.set_pinned(outcome.event_id, true).await.unwrap();
    let pinned = store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT pinned FROM history_event WHERE event_id = ?1",
                [outcome.event_id],
                |row| row.get::<_, i64>(0),
            )
        })
        .unwrap();
    assert_eq!(pinned, 1);

    store.delete_event(outcome.event_id).await.unwrap();
    assert_eq!(store.stats().unwrap().event_count, 0);
}

#[tokio::test]
async fn reader_connections_reject_mutation_while_writer_mutations_succeed() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    let outcome = store.ingest(text_capture("one", 1_000)).await.unwrap();

    let reader_error = store
        .with_reader(|connection| {
            connection.execute(
                "UPDATE history_event SET pinned = 1 WHERE event_id = ?1",
                [outcome.event_id],
            )
        })
        .unwrap_err();
    assert!(matches!(reader_error, StoreError::Database(_)));

    store.set_pinned(outcome.event_id, true).await.unwrap();
    let pinned = store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT pinned FROM history_event WHERE event_id = ?1",
                [outcome.event_id],
                |row| row.get::<_, i64>(0),
            )
        })
        .unwrap();
    assert_eq!(pinned, 1);
}

#[tokio::test]
async fn deduplicating_do_not_index_removes_existing_search_document() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    let first = store.ingest(text_capture("Łódź", 1_000)).await.unwrap();
    let mut restricted = text_capture("Łódź", 2_000);
    restricted.content_flags = ContentFlags::DO_NOT_INDEX;
    store.ingest(restricted).await.unwrap();

    let (content_flags, documents, derivations, fts_matches) = store
        .with_reader(|connection| {
            Ok::<_, rusqlite::Error>((
                connection.query_row(
                    "SELECT flags FROM content WHERE content_id = ?1",
                    [first.content_id],
                    |row| row.get::<_, i64>(0),
                )?,
                connection.query_row("SELECT count(*) FROM search_doc", [], |row| {
                    row.get::<_, i64>(0)
                })?,
                connection.query_row("SELECT count(*) FROM search_derivation", [], |row| {
                    row.get::<_, i64>(0)
                })?,
                connection.query_row(
                    "SELECT count(*) FROM search_fts WHERE search_fts MATCH 'lodz'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
            ))
        })
        .unwrap();

    assert_ne!(
        content_flags & i64::from(ContentFlags::DO_NOT_INDEX.bits()),
        0
    );
    assert_eq!(documents, 0);
    assert_eq!(derivations, 0);
    assert_eq!(fts_matches, 0);
    assert_eq!(store.stats().unwrap().event_count, 2);
}

#[tokio::test]
async fn deduplicating_indexable_content_never_recreates_a_do_not_index_document() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    let mut restricted = text_capture("Łódź", 1_000);
    restricted.content_flags = ContentFlags::DO_NOT_INDEX;
    let first = store.ingest(restricted).await.unwrap();
    store.ingest(text_capture("Łódź", 2_000)).await.unwrap();

    let (content_flags, documents, derivations, fts_matches) = store
        .with_reader(|connection| {
            Ok::<_, rusqlite::Error>((
                connection.query_row(
                    "SELECT flags FROM content WHERE content_id = ?1",
                    [first.content_id],
                    |row| row.get::<_, i64>(0),
                )?,
                connection.query_row("SELECT count(*) FROM search_doc", [], |row| {
                    row.get::<_, i64>(0)
                })?,
                connection.query_row("SELECT count(*) FROM search_derivation", [], |row| {
                    row.get::<_, i64>(0)
                })?,
                connection.query_row(
                    "SELECT count(*) FROM search_fts WHERE search_fts MATCH 'lodz'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
            ))
        })
        .unwrap();

    assert_ne!(
        content_flags & i64::from(ContentFlags::DO_NOT_INDEX.bits()),
        0
    );
    assert_eq!(documents, 0);
    assert_eq!(derivations, 0);
    assert_eq!(fts_matches, 0);
    assert_eq!(store.stats().unwrap().event_count, 2);
}

#[tokio::test]
async fn duplicate_search_derivation_does_not_rebuild_the_search_document() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("history.sqlite");
    let store = StoreHandle::open(StoreConfig::new(&database_path)).unwrap();
    store
        .ingest(text_capture("stable term", 1_000))
        .await
        .unwrap();
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE search_update_audit(marker INTEGER NOT NULL);
             CREATE TRIGGER test_search_update_audit AFTER UPDATE ON search_doc BEGIN
               INSERT INTO search_update_audit(marker) VALUES (1);
             END;",
        )
        .unwrap();
    drop(connection);

    store
        .ingest(text_capture("stable term", 2_000))
        .await
        .unwrap();

    let updates = store
        .with_reader(|connection| {
            connection.query_row("SELECT count(*) FROM search_update_audit", [], |row| {
                row.get::<_, i64>(0)
            })
        })
        .unwrap();
    assert_eq!(updates, 0);
}

#[tokio::test]
async fn bounded_search_retention_is_identical_in_forward_and_reverse_order() {
    let derivations = (0..(MAX_SEARCH_DERIVATIONS_PER_CONTENT + 8))
        .map(|index| format!("bounded derivation {index:03}"))
        .collect::<Vec<_>>();
    let mut reverse = derivations.clone();
    reverse.reverse();

    let forward_directory = tempfile::tempdir().unwrap();
    let forward = StoreHandle::open(StoreConfig::new(
        forward_directory.path().join("history.sqlite"),
    ))
    .unwrap();
    import_search_derivations(&forward, 71, &derivations).await;
    let reverse_directory = tempfile::tempdir().unwrap();
    let reversed = StoreHandle::open(StoreConfig::new(
        reverse_directory.path().join("history.sqlite"),
    ))
    .unwrap();
    import_search_derivations(&reversed, 91, &reverse).await;

    let read_retained = |store: &StoreHandle| {
        store
            .with_reader(|connection| {
                let mut statement = connection.prepare(
                    "SELECT hex(derivation_hash), normalized_text
                     FROM search_derivation ORDER BY derivation_hash",
                )?;
                let rows = statement
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                let document =
                    connection.query_row("SELECT normalized_text FROM search_doc", [], |row| {
                        row.get::<_, String>(0)
                    })?;
                Ok((rows, document))
            })
            .unwrap()
    };
    let forward_retained = read_retained(&forward);
    let reverse_retained = read_retained(&reversed);

    assert_eq!(forward_retained, reverse_retained);
    assert_eq!(forward_retained.0.len(), MAX_SEARCH_DERIVATIONS_PER_CONTENT);
    assert!(forward_retained.1.len() <= MAX_SEARCH_DOCUMENT_BYTES);
}

#[tokio::test]
async fn long_multibyte_derivation_is_truncated_only_at_a_utf8_boundary() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    let long = "Ż".repeat((MAX_SEARCH_DERIVATION_BYTES / 'Ż'.len_utf8()) + 200);

    import_search_derivations(&store, 111, &[long]).await;

    let retained = store
        .with_reader(|connection| {
            connection.query_row("SELECT normalized_text FROM search_derivation", [], |row| {
                row.get::<_, String>(0)
            })
        })
        .unwrap();
    assert!(retained.len() <= MAX_SEARCH_DERIVATION_BYTES);
    let suffix = retained
        .strip_prefix("shared bounded payload\n")
        .expect("the imported search document keeps the bounded primary prefix");
    assert!(suffix.chars().all(|character| character == 'z'));
}

#[tokio::test]
async fn deterministically_pruned_derivation_does_not_rebuild_the_document() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("history.sqlite");
    let store = StoreHandle::open(StoreConfig::new(&database_path)).unwrap();
    let mut derivations = (0..=MAX_SEARCH_DERIVATIONS_PER_CONTENT)
        .map(|index| format!("pruning derivation {index:03}"))
        .collect::<Vec<_>>();
    derivations.sort_by_key(|value| search_derivation_test_hash(value));
    let pruned = derivations.pop().unwrap();
    import_search_derivations(&store, 131, &derivations).await;
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE search_update_audit(marker INTEGER NOT NULL);
             CREATE TRIGGER test_search_update_audit AFTER UPDATE ON search_doc BEGIN
               INSERT INTO search_update_audit(marker) VALUES (1);
             END;",
        )
        .unwrap();
    drop(connection);

    import_search_derivations(&store, 151, &[pruned]).await;

    let (count, updates) = store
        .with_reader(|connection| {
            Ok((
                connection.query_row("SELECT count(*) FROM search_derivation", [], |row| {
                    row.get::<_, i64>(0)
                })?,
                connection.query_row("SELECT count(*) FROM search_update_audit", [], |row| {
                    row.get::<_, i64>(0)
                })?,
            ))
        })
        .unwrap();
    assert_eq!(count as usize, MAX_SEARCH_DERIVATIONS_PER_CONTENT);
    assert_eq!(updates, 0);
}

#[tokio::test]
async fn many_distinct_search_derivations_remain_within_count_and_document_budgets() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    let derivations = (0..200)
        .map(|index| format!("stress derivation {index:03} {}", "x".repeat(4_096)))
        .collect::<Vec<_>>();

    import_search_derivations(&store, 171, &derivations).await;

    let (count, max_derivation_bytes, document_bytes) = store
        .with_reader(|connection| {
            Ok((
                connection.query_row("SELECT count(*) FROM search_derivation", [], |row| {
                    row.get::<_, i64>(0)
                })?,
                connection.query_row(
                    "SELECT max(length(CAST(normalized_text AS BLOB))) FROM search_derivation",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
                connection.query_row(
                    "SELECT length(CAST(normalized_text AS BLOB)) FROM search_doc",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
            ))
        })
        .unwrap();
    assert!(count as usize <= MAX_SEARCH_DERIVATIONS_PER_CONTENT);
    assert!(max_derivation_bytes as usize <= MAX_SEARCH_DERIVATION_BYTES);
    assert!(document_bytes as usize <= MAX_SEARCH_DOCUMENT_BYTES);
}

#[tokio::test]
async fn ingest_persists_a_missing_primary_without_inventing_payload_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    let mut capture = text_capture("unused", 1_000);
    capture.kind = ContentKind::Image;
    capture.primary_mime = "image/png".to_owned();
    capture.representations = vec![RepresentationInput {
        format_id: "image/png".to_owned(),
        bytes: None,
        missing_ref: Some("synthetic-missing-reference".to_owned()),
    }];
    capture.content_flags = ContentFlags::MISSING_PAYLOAD;

    let first = store.ingest(capture.clone()).await.unwrap();
    let second = store.ingest(capture.clone()).await.unwrap();
    let mut other_mime = capture.clone();
    other_mime.primary_mime = "image/jpeg".to_owned();
    other_mime.representations[0].format_id = "image/jpeg".to_owned();
    let other_mime = store.ingest(other_mime).await.unwrap();
    let mut other_kind = capture;
    other_kind.kind = ContentKind::File;
    let other_kind = store.ingest(other_kind).await.unwrap();

    assert_eq!(first.content_id, second.content_id);
    assert_ne!(first.event_id, second.event_id);
    assert_ne!(first.content_id, other_mime.content_id);
    assert_ne!(first.content_id, other_kind.content_id);
    let (byte_size, storage_kind, inline_payload, original_size, stored_size) = store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT c.byte_size, rp.storage_kind, rp.inline_payload,
                        rp.original_byte_size, rp.stored_byte_size
                 FROM content c
                 JOIN history_event he ON he.content_id = c.content_id
                 JOIN event_representation er
                   ON er.event_id = he.event_id AND er.ordinal = 0
                 LEFT JOIN raw_payload rp ON rp.raw_payload_id = er.raw_payload_id
                 WHERE c.content_id = ?1
                 ORDER BY he.event_id DESC LIMIT 1",
                [first.content_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<Vec<u8>>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                    ))
                },
            )
        })
        .unwrap();
    assert_eq!(byte_size, 0);
    assert_eq!(storage_kind, None);
    assert_eq!(inline_payload, None);
    assert_eq!(original_size, None);
    assert_eq!(stored_size, None);
}

#[tokio::test]
async fn replaying_a_committed_same_run_offset_cannot_advance_or_double_count_it() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    let run = store
        .begin_import(BeginImportRun {
            run_id: uuid::Uuid::now_v7(),
            source_kind: ImportSourceKind::Raycast,
            source_fingerprint: [7; 32],
            total_records: 2,
            candidate_records: 2,
            initial_failures: Vec::new(),
            initial_skips: Vec::new(),
        })
        .await
        .unwrap();
    let candidate = || StoreImportCandidate {
        candidate_offset: 0,
        record_fingerprint: [9; 32],
        capture: text_capture("synthetic imported value", 1_000),
        search_ocr: None,
        source_app_original: None,
    };

    store
        .import_batch(
            import_operation_permit(),
            run.run_id,
            run.generation,
            vec![candidate()],
        )
        .await
        .unwrap();
    let error = store
        .import_batch(
            import_operation_permit(),
            run.run_id,
            run.generation,
            vec![candidate()],
        )
        .await
        .unwrap_err();

    assert!(matches!(error, StoreError::ImportCheckpointMismatch));
    let status = store.import_status(run.run_id).unwrap();
    assert_eq!(status.next_candidate_offset, 1);
    assert_eq!(status.imported_records, 1);
    assert_eq!(status.already_present_records, 0);
    assert_eq!(store.stats().unwrap().event_count, 1);
}

#[tokio::test]
async fn preallocated_import_run_id_is_exactly_idempotent_and_conflicts_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    let run_id = uuid::Uuid::now_v7();
    let input = |source_fingerprint, reason_code: &str| BeginImportRun {
        run_id,
        source_kind: ImportSourceKind::Raycast,
        source_fingerprint,
        total_records: 2,
        candidate_records: 1,
        initial_failures: vec![clipboard_store::ImportFailureCount {
            reason_code: reason_code.to_owned(),
            count: 1,
        }],
        initial_skips: Vec::new(),
    };

    let first = store
        .begin_import(input([17; 32], "invalid_record"))
        .await
        .unwrap();
    let retry = store
        .begin_import(input([17; 32], "invalid_record"))
        .await
        .unwrap();
    let source_conflict = store
        .begin_import(input([18; 32], "invalid_record"))
        .await
        .unwrap_err();
    let failure_conflict = store
        .begin_import(input([17; 32], "invalid_timestamp"))
        .await
        .unwrap_err();

    assert_eq!(first, retry);
    assert_eq!(first.run_id, run_id);
    assert_eq!(first.state, clipboard_store::StoreImportRunState::Running);
    assert!(matches!(source_conflict, StoreError::ImportRunConflict));
    assert!(matches!(failure_conflict, StoreError::ImportRunConflict));
    let run_count = store
        .with_reader(|connection| {
            connection.query_row("SELECT count(*) FROM import_run", [], |row| {
                row.get::<_, i64>(0)
            })
        })
        .unwrap();
    assert_eq!(run_count, 1);

    store
        .import_batch(
            import_operation_permit(),
            first.run_id,
            first.generation,
            vec![StoreImportCandidate {
                candidate_offset: 0,
                record_fingerprint: [19; 32],
                capture: text_capture("synthetic terminal lease", 1_000),
                search_ocr: None,
                source_app_original: None,
            }],
        )
        .await
        .unwrap();
    store
        .finish_import(first.run_id, first.generation)
        .await
        .unwrap();
    let terminal_retry = store
        .begin_import(input([17; 32], "invalid_record"))
        .await
        .unwrap();
    assert_eq!(
        terminal_retry.state,
        clipboard_store::StoreImportRunState::Completed
    );
}

#[tokio::test]
async fn import_run_deletion_cannot_erase_global_idempotency_claims() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("history.sqlite");
    let store = StoreHandle::open(StoreConfig::new(&database_path)).unwrap();
    let first_run = store
        .begin_import(BeginImportRun {
            run_id: uuid::Uuid::now_v7(),
            source_kind: ImportSourceKind::Raycast,
            source_fingerprint: [31; 32],
            total_records: 1,
            candidate_records: 1,
            initial_failures: Vec::new(),
            initial_skips: Vec::new(),
        })
        .await
        .unwrap();
    let candidate = || StoreImportCandidate {
        candidate_offset: 0,
        record_fingerprint: [41; 32],
        capture: text_capture("synthetic retained claim", 1_000),
        search_ocr: None,
        source_app_original: None,
    };
    store
        .import_batch(
            import_operation_permit(),
            first_run.run_id,
            first_run.generation,
            vec![candidate()],
        )
        .await
        .unwrap();
    store
        .finish_import(first_run.run_id, first_run.generation)
        .await
        .unwrap();

    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();
    let deletion = connection.execute(
        "DELETE FROM import_run WHERE external_id = ?1",
        [first_run.run_id.as_bytes().as_slice()],
    );
    drop(connection);

    assert!(deletion.is_err());
    let second_run = store
        .begin_import(BeginImportRun {
            run_id: uuid::Uuid::now_v7(),
            source_kind: ImportSourceKind::Raycast,
            source_fingerprint: [32; 32],
            total_records: 1,
            candidate_records: 1,
            initial_failures: Vec::new(),
            initial_skips: Vec::new(),
        })
        .await
        .unwrap();
    store
        .import_batch(
            import_operation_permit(),
            second_run.run_id,
            second_run.generation,
            vec![candidate()],
        )
        .await
        .unwrap();
    let completed = store
        .finish_import(second_run.run_id, second_run.generation)
        .await
        .unwrap();
    assert_eq!(completed.imported_records, 0);
    assert_eq!(completed.already_present_records, 1);
    assert_eq!(store.stats().unwrap().event_count, 1);
}

#[tokio::test]
async fn import_tables_reject_unknown_sources_and_non_digest_record_fingerprints() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("history.sqlite");
    let store = StoreHandle::open(StoreConfig::new(&database_path)).unwrap();
    let run = store
        .begin_import(BeginImportRun {
            run_id: uuid::Uuid::now_v7(),
            source_kind: ImportSourceKind::SuperCmd,
            source_fingerprint: [51; 32],
            total_records: 1,
            candidate_records: 1,
            initial_failures: Vec::new(),
            initial_skips: Vec::new(),
        })
        .await
        .unwrap();
    let import_run_id = store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT import_run_id FROM import_run WHERE external_id = ?1",
                [run.run_id.as_bytes().as_slice()],
                |row| row.get::<_, i64>(0),
            )
        })
        .unwrap();
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();
    let invalid_run_source = connection.execute(
        "INSERT INTO import_run
           (external_id, source_kind, source_fingerprint, status, total_records,
            candidate_records, started_at_ms)
         VALUES (?1, 'unknown', ?2, 'running', 0, 0, 0)",
        rusqlite::params![[61_u8; 16].as_slice(), [62_u8; 32].as_slice()],
    );
    let invalid_fingerprint = connection.execute(
        "INSERT INTO import_record
           (import_run_id, source_kind, record_fingerprint, created_at_ms)
         VALUES (?1, 'supercmd', ?2, 0)",
        rusqlite::params![import_run_id, [63_u8; 31].as_slice()],
    );
    let invalid_record_source = connection.execute(
        "INSERT INTO import_record
           (import_run_id, source_kind, record_fingerprint, created_at_ms)
         VALUES (?1, 'unknown', ?2, 0)",
        rusqlite::params![import_run_id, [64_u8; 32].as_slice()],
    );

    assert!(invalid_run_source.is_err());
    assert!(invalid_fingerprint.is_err());
    assert!(invalid_record_source.is_err());
}

#[tokio::test]
async fn sql_rejects_text_digests_and_non_uuidv7_blob_identities() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("history.sqlite");
    let store = StoreHandle::open(StoreConfig::new(&database_path)).unwrap();
    let event = store
        .ingest(text_capture("constraint anchor", 1_000))
        .await
        .unwrap();
    let run = store
        .begin_import(BeginImportRun {
            run_id: uuid::Uuid::now_v7(),
            source_kind: ImportSourceKind::Raycast,
            source_fingerprint: [201; 32],
            total_records: 0,
            candidate_records: 0,
            initial_failures: Vec::new(),
            initial_skips: Vec::new(),
        })
        .await
        .unwrap();
    let import_run_id = store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT import_run_id FROM import_run WHERE external_id = ?1",
                [run.run_id.as_bytes().as_slice()],
                |row| row.get::<_, i64>(0),
            )
        })
        .unwrap();
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();

    let text_content_digest = connection.execute(
        "INSERT INTO content
           (content_hash, kind, primary_mime, byte_size, flags, created_at_ms)
         VALUES (?1, 'text', 'text/plain', 0, 0, 0)",
        ["12345678901234567890123456789012"],
    );
    let text_derivation_digest = connection.execute(
        "INSERT INTO search_derivation(content_id, derivation_hash, normalized_text)
         VALUES (?1, ?2, 'synthetic')",
        rusqlite::params![event.content_id, "12345678901234567890123456789012"],
    );
    let text_record_digest = connection.execute(
        "INSERT INTO import_record
           (import_run_id, source_kind, record_fingerprint, created_at_ms)
         VALUES (?1, 'raycast', ?2, 0)",
        rusqlite::params![import_run_id, "12345678901234567890123456789012"],
    );
    let text_source_digest = connection.execute(
        "INSERT INTO import_run
           (external_id, source_kind, source_fingerprint, initial_failure_fingerprint, status,
            total_records, candidate_records, started_at_ms)
         VALUES (?1, 'raycast', ?2, ?3, 'running', 0, 0, 0)",
        rusqlite::params![
            uuid::Uuid::now_v7().as_bytes().as_slice(),
            "12345678901234567890123456789012",
            [202_u8; 32].as_slice()
        ],
    );
    let text_initial_failure_digest = connection.execute(
        "INSERT INTO import_run
           (external_id, source_kind, source_fingerprint, initial_failure_fingerprint, status,
            total_records, candidate_records, started_at_ms)
         VALUES (?1, 'raycast', ?2, ?3, 'running', 0, 0, 0)",
        rusqlite::params![
            uuid::Uuid::now_v7().as_bytes().as_slice(),
            [207_u8; 32].as_slice(),
            "12345678901234567890123456789012"
        ],
    );

    let mut wrong_version = *uuid::Uuid::now_v7().as_bytes();
    wrong_version[6] = (wrong_version[6] & 0x0f) | 0x40;
    let invalid_event_version = connection.execute(
        "INSERT INTO history_event
           (global_id, content_id, captured_at_ms, source_confidence)
         VALUES (?1, ?2, 0, 'unknown')",
        rusqlite::params![wrong_version.as_slice(), event.content_id],
    );
    let mut wrong_variant = *uuid::Uuid::now_v7().as_bytes();
    wrong_variant[8] = (wrong_variant[8] & 0x3f) | 0x40;
    let invalid_run_variant = connection.execute(
        "INSERT INTO import_run
           (external_id, source_kind, source_fingerprint, initial_failure_fingerprint, status,
            total_records, candidate_records, started_at_ms)
         VALUES (?1, 'raycast', ?2, ?3, 'running', 0, 0, 0)",
        rusqlite::params![
            wrong_variant.as_slice(),
            [203_u8; 32].as_slice(),
            [204_u8; 32].as_slice()
        ],
    );
    let text_run_identity = connection.execute(
        "INSERT INTO import_run
           (external_id, source_kind, source_fingerprint, initial_failure_fingerprint, status,
            total_records, candidate_records, started_at_ms)
         VALUES (?1, 'raycast', ?2, ?3, 'running', 0, 0, 0)",
        rusqlite::params![
            "1234567890123456",
            [205_u8; 32].as_slice(),
            [206_u8; 32].as_slice()
        ],
    );

    assert!(text_content_digest.is_err());
    assert!(text_derivation_digest.is_err());
    assert!(text_record_digest.is_err());
    assert!(text_source_digest.is_err());
    assert!(text_initial_failure_digest.is_err());
    assert!(invalid_event_version.is_err());
    assert!(invalid_run_variant.is_err());
    assert!(text_run_identity.is_err());
}

#[tokio::test]
async fn sql_enforces_search_row_count_and_utf8_byte_budgets() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("history.sqlite");
    let store = StoreHandle::open(StoreConfig::new(&database_path)).unwrap();
    let event = store
        .ingest(text_capture("budget anchor", 1_000))
        .await
        .unwrap();
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute(
            "DELETE FROM search_derivation WHERE content_id = ?1",
            [event.content_id],
        )
        .unwrap();

    let oversized_derivation = connection.execute(
        "INSERT INTO search_derivation(content_id, derivation_hash, normalized_text)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![
            event.content_id,
            [211_u8; 32].as_slice(),
            "x".repeat(MAX_SEARCH_DERIVATION_BYTES + 1)
        ],
    );
    let oversized_document = connection.execute(
        "UPDATE search_doc SET normalized_text = ?1 WHERE content_id = ?2",
        rusqlite::params!["x".repeat(MAX_SEARCH_DOCUMENT_BYTES + 1), event.content_id],
    );
    for index in 0..MAX_SEARCH_DERIVATIONS_PER_CONTENT {
        let mut digest = [0_u8; 32];
        digest[0] = index as u8;
        connection
            .execute(
                "INSERT INTO search_derivation(content_id, derivation_hash, normalized_text)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![event.content_id, digest.as_slice(), format!("term {index}")],
            )
            .unwrap();
    }
    let seventeenth = connection.execute(
        "INSERT INTO search_derivation(content_id, derivation_hash, normalized_text)
         VALUES (?1, ?2, 'one too many')",
        rusqlite::params![event.content_id, [255_u8; 32].as_slice()],
    );

    assert!(oversized_derivation.is_err());
    assert!(oversized_document.is_err());
    assert!(seventeenth.is_err());
}

#[test]
fn writer_queue_capacity_is_bounded_to_256_commands() {
    assert_eq!(WRITER_QUEUE_CAPACITY, 256);
}

fn leased_store_config(data_dir: &std::path::Path) -> (StoreConfig, Arc<StorageBoundaryLease>) {
    fs::create_dir(data_dir).unwrap();
    let config =
        StoreConfig::new(data_dir.join("clipboard.db")).with_blob_root(data_dir.join("blobs"));
    let lease = Arc::new(StorageBoundaryLease::create_writer(&config).unwrap());
    (config.with_storage_boundary(Arc::clone(&lease)), lease)
}

#[cfg(unix)]
fn replace_leased_data_directory(data_dir: &std::path::Path, retained: &std::path::Path) {
    fs::rename(data_dir, retained).unwrap();
    fs::create_dir(data_dir).unwrap();
    fs::write(data_dir.join("outside-marker"), b"unchanged").unwrap();
}

#[cfg(unix)]
#[test]
fn leased_boundary_rejects_whole_data_directory_replacement_before_store_open() {
    let sandbox = tempfile::tempdir().unwrap();
    let data_dir = sandbox.path().join("leased-data");
    let retained = sandbox.path().join("retained-data");
    let (config, _lease) = leased_store_config(&data_dir);
    replace_leased_data_directory(&data_dir, &retained);

    let error = match StoreHandle::open(config) {
        Ok(_) => panic!("replacement unexpectedly opened"),
        Err(error) => error,
    };

    assert!(matches!(error, StoreError::StorageBoundary));
    assert_eq!(error.to_string(), "storage boundary changed");
    assert_eq!(
        fs::read(data_dir.join("outside-marker")).unwrap(),
        b"unchanged"
    );
    assert!(!data_dir.join("clipboard.db").exists());
    assert!(!data_dir.join("blobs").exists());
}

#[cfg(unix)]
#[test]
fn leased_boundary_rejects_ancestor_substitution_even_when_data_identity_is_unchanged() {
    use std::os::unix::fs::symlink;

    let sandbox = tempfile::tempdir().unwrap();
    let ancestor = sandbox.path().join("ancestor");
    let retained_ancestor = sandbox.path().join("retained-ancestor");
    fs::create_dir(&ancestor).unwrap();
    let data_dir = ancestor.join("leased-data");
    let (config, _lease) = leased_store_config(&data_dir);
    fs::rename(&ancestor, &retained_ancestor).unwrap();
    symlink(&retained_ancestor, &ancestor).unwrap();

    let error = match StoreHandle::open(config) {
        Ok(_) => panic!("ancestor substitution unexpectedly opened"),
        Err(error) => error,
    };

    assert!(matches!(error, StoreError::StorageBoundary));
    assert_eq!(error.to_string(), "storage boundary changed");
}

#[cfg(unix)]
#[tokio::test]
async fn writer_retains_lease_and_rejects_replacement_before_ingest_or_cas_work() {
    let sandbox = tempfile::tempdir().unwrap();
    let data_dir = sandbox.path().join("leased-data");
    let retained = sandbox.path().join("retained-data");
    let (config, lease) = leased_store_config(&data_dir);
    let store = StoreHandle::open(config).unwrap();
    drop(lease);
    replace_leased_data_directory(&data_dir, &retained);
    let mut capture = text_capture("synthetic image bytes", 1_000);
    capture.kind = ContentKind::Image;
    capture.primary_mime = "image/png".to_owned();

    let error = store.ingest(capture).await.unwrap_err();

    assert!(matches!(error, StoreError::StorageBoundary));
    assert_eq!(
        fs::read(data_dir.join("outside-marker")).unwrap(),
        b"unchanged"
    );
    assert!(!data_dir.join("clipboard.db").exists());
    assert!(!data_dir.join("blobs").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn import_batch_propagates_private_storage_without_recording_a_candidate_failure() {
    use std::os::unix::fs::PermissionsExt;

    let sandbox = tempfile::tempdir().unwrap();
    let data_dir = sandbox.path().join("leased-data");
    let (config, _lease) = leased_store_config(&data_dir);
    let store = StoreHandle::open(config).unwrap();
    let run = store
        .begin_import(BeginImportRun {
            run_id: uuid::Uuid::now_v7(),
            source_kind: ImportSourceKind::SuperCmd,
            source_fingerprint: [42; 32],
            total_records: 1,
            candidate_records: 1,
            initial_failures: Vec::new(),
            initial_skips: Vec::new(),
        })
        .await
        .unwrap();
    let mut capture = text_capture("synthetic image", 1_000);
    capture.kind = ContentKind::Image;
    capture.primary_mime = "image/png".to_owned();
    fs::set_permissions(data_dir.join("blobs"), fs::Permissions::from_mode(0o755)).unwrap();

    let error = store
        .import_batch(
            import_operation_permit(),
            run.run_id,
            run.generation,
            vec![StoreImportCandidate {
                candidate_offset: 0,
                record_fingerprint: [43; 32],
                capture,
                search_ocr: None,
                source_app_original: None,
            }],
        )
        .await
        .unwrap_err();

    assert!(matches!(error, StoreError::PrivateStorageUnavailable));
    fs::set_permissions(data_dir.join("blobs"), fs::Permissions::from_mode(0o700)).unwrap();
    let status = store.import_status(run.run_id).unwrap();
    assert_eq!(status.next_candidate_offset, 0);
    assert_eq!(status.failed_records, 0);
}

#[tokio::test]
async fn a_data_directory_that_does_not_exist_yet_is_created_rather_than_refused() {
    // The state of every machine that has just installed the application: the
    // directory the operating system names for our data has never been made.
    // Refusing it there is refusing the first run.
    let parent = tempfile::tempdir().unwrap();
    let data_dir = parent.path().join("never-created");
    assert!(!data_dir.exists());

    let store = StoreHandle::open(StoreConfig::in_data_dir(&data_dir)).unwrap();

    assert_eq!(store.stats().unwrap().event_count, 0);
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(&data_dir).unwrap().permissions(),
    );
    // Private from the moment it exists, not hardened afterwards: a directory
    // that is briefly world-readable is briefly readable by the world.
    assert_eq!(
        mode & 0o777,
        0o700,
        "a fresh data directory must be private"
    );
}

#[test]
fn a_reader_still_refuses_a_data_directory_that_is_not_there() {
    // Creating on demand is a writer's job. A reader finding nothing has found
    // nothing, and inventing an empty store for it would turn "your history is
    // missing" into "your history is empty".
    let parent = tempfile::tempdir().unwrap();
    let absent = parent.path().join("never-created");

    let error =
        StorageBoundaryLease::open_read_only(&StoreConfig::in_data_dir(&absent)).unwrap_err();

    assert_eq!(error.to_string(), "storage boundary changed");
    assert!(!absent.exists(), "a reader must not create anything");
}

#[test]
fn a_symlink_standing_in_for_the_data_directory_is_still_refused() {
    // The check that creating on demand must not have weakened: whatever is at
    // the path when we look is judged the same way, made by us or not.
    let parent = tempfile::tempdir().unwrap();
    let elsewhere = parent.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let planted = parent.path().join("data");
    std::os::unix::fs::symlink(&elsewhere, &planted).unwrap();

    let error =
        StorageBoundaryLease::create_writer(&StoreConfig::in_data_dir(&planted)).unwrap_err();

    assert_eq!(error.to_string(), "storage boundary changed");
}

// ------------------------------------------------------------ grouping --

/// Every occurrence of one content, newest first: `(event_id, captured_at_ms)`.
fn occurrence_rows(store: &StoreHandle, content_id: i64) -> Vec<(i64, i64)> {
    store
        .with_reader(|connection| {
            let mut statement = connection.prepare(
                "SELECT event_id, captured_at_ms FROM history_event
                 WHERE content_id = ?1
                 ORDER BY captured_at_ms DESC, event_id DESC",
            )?;
            let rows = statement
                .query_map([content_id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .unwrap()
}

#[tokio::test]
async fn repeated_captures_of_one_content_collapse_to_five_newest_events() {
    let directory = tempfile::tempdir().unwrap();
    let store = StoreHandle::open(StoreConfig::in_data_dir(directory.path())).unwrap();

    let first = store
        .ingest(text_capture("ta sama treść", 1_000))
        .await
        .unwrap();
    for index in 1..7 {
        store
            .ingest(text_capture("ta sama treść", 1_000 + index))
            .await
            .unwrap();
    }

    let rows = occurrence_rows(&store, first.content_id);
    assert_eq!(
        rows.len(),
        clipboard_store::MAX_OCCURRENCES_PER_CONTENT as usize
    );
    let timestamps: Vec<i64> = rows.iter().map(|(_, at)| *at).collect();
    assert_eq!(timestamps, vec![1_006, 1_005, 1_004, 1_003, 1_002]);
    assert_eq!(store.stats().unwrap().event_count, 5);
    assert_eq!(store.stats().unwrap().content_count, 1);
}

#[tokio::test]
async fn pruning_never_deletes_a_pinned_occurrence() {
    let directory = tempfile::tempdir().unwrap();
    let store = StoreHandle::open(StoreConfig::in_data_dir(directory.path())).unwrap();

    let pinned = store
        .ingest(text_capture("przypięta treść", 1_000))
        .await
        .unwrap();
    store.set_pinned(pinned.event_id, true).await.unwrap();
    for index in 1..7 {
        store
            .ingest(text_capture("przypięta treść", 1_000 + index))
            .await
            .unwrap();
    }

    // The pinned occurrence survives alongside the five newest unpinned ones:
    // pinning is the user saying "keep this", which outranks the occurrence cap.
    let rows = occurrence_rows(&store, pinned.content_id);
    assert_eq!(rows.len(), 6);
    assert!(
        rows.iter()
            .any(|(event_id, _)| *event_id == pinned.event_id)
    );
}

#[tokio::test]
async fn ingest_spares_the_event_it_just_inserted_even_when_older_than_the_cap() {
    let directory = tempfile::tempdir().unwrap();
    let store = StoreHandle::open(StoreConfig::in_data_dir(directory.path())).unwrap();

    for index in 0..5 {
        store
            .ingest(text_capture("starsze wpisy", 2_000 + index))
            .await
            .unwrap();
    }
    // A historical import lands with a timestamp older than everything kept so
    // far; the prune must not delete the row whose id the caller just received.
    let historical = store
        .ingest(text_capture("starsze wpisy", 1_000))
        .await
        .unwrap();

    let rows = occurrence_rows(&store, historical.content_id);
    assert_eq!(rows.len(), 6);
    assert!(
        rows.iter()
            .any(|(event_id, _)| *event_id == historical.event_id),
        "the just-inserted event must survive its own prune"
    );
}

#[tokio::test]
async fn occurrence_sweep_shrinks_legacy_duplicates_and_reports_more() {
    let directory = tempfile::tempdir().unwrap();
    let store = StoreHandle::open(StoreConfig::in_data_dir(directory.path())).unwrap();

    let content = store
        .ingest(text_capture("legacy content", 10_000))
        .await
        .unwrap();
    // A database written before the cap existed: rows beyond the newest five
    // per content are already sitting in history_event.
    let database_path = directory.path().join(clipboard_store::DATABASE_FILENAME);
    let legacy = rusqlite::Connection::open(&database_path).unwrap();
    for index in 0..10u8 {
        let mut global_id = [0u8; 16];
        global_id[6] = 0x70; // UUIDv7
        global_id[7] = index;
        global_id[8] = 0x80; // variant
        legacy
            .execute(
                "INSERT INTO history_event (global_id, content_id, captured_at_ms, occurrence_count)
                 VALUES (?1, ?2, ?3, 1)",
                rusqlite::params![
                    global_id.as_slice(),
                    content.content_id,
                    9_000 + i64::from(index),
                ],
            )
            .unwrap();
    }
    drop(legacy);

    // Deleting is a write on the queue capture shares, so the sweep yields
    // rather than hold it.
    let outcome = store.prune_occurrence_events(3).await.unwrap();
    assert_eq!(outcome.deleted_events, 3);
    assert!(outcome.more_remaining);

    let rest = store.prune_occurrence_events(u32::MAX).await.unwrap();
    assert_eq!(rest.deleted_events, 3);
    assert!(!rest.more_remaining);

    let rows = occurrence_rows(&store, content.content_id);
    assert_eq!(
        rows.len(),
        clipboard_store::MAX_OCCURRENCES_PER_CONTENT as usize
    );
    // The five newest survive: the ingested row and the four newest legacy rows.
    let timestamps: Vec<i64> = rows.iter().map(|(_, at)| *at).collect();
    assert_eq!(timestamps, vec![10_000, 9_009, 9_008, 9_007, 9_006]);
}

#[tokio::test]
async fn deleting_a_grouped_entry_removes_every_occurrence_of_its_content() {
    let directory = tempfile::tempdir().unwrap();
    let store = StoreHandle::open(StoreConfig::in_data_dir(directory.path())).unwrap();

    let grouped = store.ingest(text_capture("grupa", 1_000)).await.unwrap();
    store.ingest(text_capture("grupa", 2_000)).await.unwrap();
    store.ingest(text_capture("grupa", 3_000)).await.unwrap();
    let other = store
        .ingest(text_capture("inny wpis", 1_500))
        .await
        .unwrap();

    store
        .delete_events_for_content(grouped.content_id)
        .await
        .unwrap();

    assert_eq!(occurrence_rows(&store, grouped.content_id).len(), 0);
    assert_eq!(occurrence_rows(&store, other.content_id).len(), 1);
    assert_eq!(store.stats().unwrap().event_count, 1);
}

#[tokio::test]
async fn unpinning_clears_every_occurrence_of_the_group() {
    let directory = tempfile::tempdir().unwrap();
    let store = StoreHandle::open(StoreConfig::in_data_dir(directory.path())).unwrap();

    // The oldest occurrence pinned first, then a newer capture takes over the
    // row the interface shows. Unpinning that row must clear the group: a
    // pinned member the interface cannot reach would keep the entry pinned —
    // and exempt from retention — forever.
    let pinned = store
        .ingest(text_capture("grupa do odpięcia", 1_000))
        .await
        .unwrap();
    store.set_pinned(pinned.event_id, true).await.unwrap();
    let representative = store
        .ingest(text_capture("grupa do odpięcia", 2_000))
        .await
        .unwrap();

    store
        .set_pinned(representative.event_id, false)
        .await
        .unwrap();

    let pinned_rows = store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT count(*) FROM history_event WHERE content_id = ?1 AND pinned = 1",
                [pinned.content_id],
                |row| row.get::<_, i64>(0),
            )
        })
        .unwrap();
    assert_eq!(pinned_rows, 0);
}

#[tokio::test]
async fn imported_duplicates_respect_the_occurrence_cap() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();

    let run = store
        .begin_import(BeginImportRun {
            run_id: uuid::Uuid::now_v7(),
            source_kind: ImportSourceKind::SuperCmd,
            source_fingerprint: [77; 32],
            total_records: 7,
            candidate_records: 7,
            initial_failures: Vec::new(),
            initial_skips: Vec::new(),
        })
        .await
        .unwrap();
    // One source record per copy of the same payload: the fingerprint is what
    // makes them distinct records, the payload is what makes them one group.
    let candidates = (0..7u8)
        .map(|offset| {
            let mut record_fingerprint = [77u8; 32];
            record_fingerprint[0] = 77u8.wrapping_add(offset);
            StoreImportCandidate {
                candidate_offset: u64::from(offset),
                record_fingerprint,
                capture: text_capture("importowana grupa", 1_000 + i64::from(offset)),
                search_ocr: None,
                source_app_original: None,
            }
        })
        .collect::<Vec<_>>();
    store
        .import_batch(
            import_operation_permit(),
            run.run_id,
            run.generation,
            candidates,
        )
        .await
        .unwrap();
    store
        .finish_import(run.run_id, run.generation)
        .await
        .unwrap();

    let content_id = store
        .with_reader(|connection| {
            connection.query_row("SELECT content_id FROM content", [], |row| {
                row.get::<_, i64>(0)
            })
        })
        .unwrap();
    assert_eq!(
        occurrence_rows(&store, content_id).len(),
        clipboard_store::MAX_OCCURRENCES_PER_CONTENT as usize
    );
}
