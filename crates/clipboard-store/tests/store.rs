use clipboard_core::{
    CaptureInput, ContentFlags, ContentKind, EventFlags, RepresentationInput, SourceConfidence,
};
use clipboard_store::{
    BeginImportRun, ImportSourceKind, MAX_SEARCH_DERIVATION_BYTES,
    MAX_SEARCH_DERIVATIONS_PER_CONTENT, MAX_SEARCH_DOCUMENT_BYTES, ReadOnlyStore,
    StorageBoundaryLease, StoreConfig, StoreError, StoreHandle, StoreImportCandidate,
    WRITER_QUEUE_CAPACITY, migrations,
};

use std::{fs, sync::Arc};

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

async fn import_search_derivations(store: &StoreHandle, source_seed: u8, derivations: &[String]) {
    let run = store
        .begin_import(BeginImportRun {
            run_id: uuid::Uuid::now_v7(),
            source_kind: ImportSourceKind::SuperCmd,
            source_fingerprint: [source_seed; 32],
            total_records: derivations.len() as u64,
            candidate_records: derivations.len() as u64,
            initial_failures: Vec::new(),
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
                search_text: Some(derivation.clone()),
                source_app_original: None,
            }
        })
        .collect::<Vec<_>>();
    store
        .import_batch(run.run_id, run.generation, candidates)
        .await
        .unwrap();
    store
        .finish_import(run.run_id, run.generation)
        .await
        .unwrap();
}

fn search_derivation_test_hash(value: &str) -> [u8; 32] {
    let normalized = clipboard_core::normalize_search_text(value);
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
    assert!(retained.chars().all(|character| character == 'z'));
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
            run_id: uuid::Uuid::now_v7(),
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
            first.run_id,
            first.generation,
            vec![StoreImportCandidate {
                candidate_offset: 0,
                record_fingerprint: [19; 32],
                capture: text_capture("synthetic terminal lease", 1_000),
                search_text: Some("synthetic terminal lease".to_owned()),
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
            run_id: uuid::Uuid::now_v7(),
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
            run_id: uuid::Uuid::now_v7(),
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
