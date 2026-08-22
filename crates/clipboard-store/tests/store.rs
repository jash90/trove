use clipboard_core::{
    CaptureInput, ContentFlags, ContentKind, EventFlags, RepresentationInput, SourceConfidence,
};
use clipboard_store::{
    BeginImportRun, ImportSourceKind, StoreConfig, StoreError, StoreHandle, StoreImportCandidate,
    WRITER_QUEUE_CAPACITY, migrations,
};

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
    }
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
                 FROM content_representation WHERE content_id = ?1",
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
                "SELECT content.byte_size, content_representation.storage_kind,
                        content_representation.inline_payload,
                        content_representation.original_byte_size,
                        content_representation.stored_byte_size
                 FROM content JOIN content_representation USING(content_id)
                 WHERE content.content_id = ?1",
                [first.content_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<Vec<u8>>>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                },
            )
        })
        .unwrap();
    assert_eq!(byte_size, 0);
    assert_eq!(storage_kind, "missing");
    assert_eq!(inline_payload, None);
    assert_eq!(original_size, 0);
    assert_eq!(stored_size, 0);
}

#[tokio::test]
async fn replaying_a_committed_same_run_offset_cannot_advance_or_double_count_it() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    let run = store
        .begin_import(BeginImportRun {
            source_kind: ImportSourceKind::Raycast,
            source_fingerprint: [7; 32],
            total_records: 2,
            candidate_records: 2,
            initial_failures: Vec::new(),
        })
        .await
        .unwrap();
    let candidate = || StoreImportCandidate {
        candidate_offset: 0,
        record_fingerprint: [9; 32],
        capture: text_capture("synthetic imported value", 1_000),
        search_text: Some("synthetic imported value".to_owned()),
        source_app_original: None,
    };

    store
        .import_batch(run.run_id, run.generation, vec![candidate()])
        .await
        .unwrap();
    let error = store
        .import_batch(run.run_id, run.generation, vec![candidate()])
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
async fn import_run_deletion_cannot_erase_global_idempotency_claims() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("history.sqlite");
    let store = StoreHandle::open(StoreConfig::new(&database_path)).unwrap();
    let first_run = store
        .begin_import(BeginImportRun {
            source_kind: ImportSourceKind::Raycast,
            source_fingerprint: [31; 32],
            total_records: 1,
            candidate_records: 1,
            initial_failures: Vec::new(),
        })
        .await
        .unwrap();
    let candidate = || StoreImportCandidate {
        candidate_offset: 0,
        record_fingerprint: [41; 32],
        capture: text_capture("synthetic retained claim", 1_000),
        search_text: Some("synthetic retained claim".to_owned()),
        source_app_original: None,
    };
    store
        .import_batch(first_run.run_id, first_run.generation, vec![candidate()])
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
            source_kind: ImportSourceKind::Raycast,
            source_fingerprint: [32; 32],
            total_records: 1,
            candidate_records: 1,
            initial_failures: Vec::new(),
        })
        .await
        .unwrap();
    store
        .import_batch(second_run.run_id, second_run.generation, vec![candidate()])
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
            source_kind: ImportSourceKind::SuperCmd,
            source_fingerprint: [51; 32],
            total_records: 1,
            candidate_records: 1,
            initial_failures: Vec::new(),
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

#[test]
fn writer_queue_capacity_is_bounded_to_256_commands() {
    assert_eq!(WRITER_QUEUE_CAPACITY, 256);
}
