use clipboard_core::{
    CaptureInput, ContentFlags, ContentKind, EventFlags, RepresentationInput, SourceConfidence,
};
use clipboard_store::{StoreConfig, StoreError, StoreHandle, WRITER_QUEUE_CAPACITY, migrations};

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

#[tokio::test]
async fn opened_connections_apply_the_required_sqlite_policy() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();

    let (journal_mode, foreign_keys, synchronous, busy_timeout, cache_size) = store
        .with_reader(|connection| {
            Ok::<_, rusqlite::Error>((
                connection.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))?,
                connection.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))?,
                connection.query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))?,
                connection.query_row("PRAGMA busy_timeout", [], |row| row.get::<_, i64>(0))?,
                connection.query_row("PRAGMA cache_size", [], |row| row.get::<_, i64>(0))?,
            ))
        })
        .unwrap();

    assert_eq!(journal_mode, "wal");
    assert_eq!(foreign_keys, 1);
    assert_eq!(synchronous, 1);
    assert_eq!(busy_timeout, 5_000);
    assert_eq!(cache_size, -65_536);
}

#[tokio::test]
async fn ingest_writes_normalized_search_document_and_uuid_blob() {
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
async fn ingest_rejects_payloads_without_available_storage() {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    let mut oversized = text_capture("small", 1_000);
    oversized.representations[0].bytes = Some(vec![0; 4_096]);
    let mut image = text_capture("small", 2_000);
    image.kind = ContentKind::Image;

    let oversized_error = store.ingest(oversized).await.unwrap_err();
    let image_error = store.ingest(image).await.unwrap_err();

    assert!(matches!(
        oversized_error,
        StoreError::PayloadStorageUnavailable
    ));
    assert!(matches!(image_error, StoreError::PayloadStorageUnavailable));
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

#[test]
fn writer_queue_capacity_is_bounded_to_256_commands() {
    assert_eq!(WRITER_QUEUE_CAPACITY, 256);
}
