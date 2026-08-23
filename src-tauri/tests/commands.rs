use clipboard_history_app::{
    commands::{self, AppSettingsDto},
    state::AppState,
};
use clipboard_search::SearchRequest;
use clipboard_store::{StoreConfig, StoreHandle};
use serde_json::json;
use std::{
    fs,
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use tauri::Manager;

fn ipc_request(cmd: &str, body: serde_json::Value) -> tauri::webview::InvokeRequest {
    tauri::webview::InvokeRequest {
        cmd: cmd.to_owned(),
        callback: tauri::ipc::CallbackFn(0),
        error: tauri::ipc::CallbackFn(1),
        url: "tauri://localhost".parse().unwrap(),
        body: tauri::ipc::InvokeBody::Json(body),
        headers: Default::default(),
        invoke_key: tauri::test::INVOKE_KEY.to_owned(),
    }
}

fn command_test_app(state: AppState) -> tauri::App<tauri::test::MockRuntime> {
    tauri::test::mock_builder()
        .manage(state)
        .invoke_handler(commands::invoke_handler())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap()
}

#[tokio::test]
async fn search_command_returns_camel_case_page_without_payload_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    state
        .store
        .ingest(text_capture("bezpieczny tekst", 1_725_000_000_000))
        .await
        .unwrap();

    let page = commands::search_history_service(&state, SearchRequest::default())
        .await
        .unwrap();
    let json = serde_json::to_value(page).unwrap();

    assert!(json["items"][0].get("capturedAtMs").is_some());
    assert!(json["items"][0].get("inlinePayload").is_none());
    assert!(json["items"][0].get("blobRelpath").is_none());
}

fn text_capture(value: &str, captured_at_ms: i64) -> clipboard_core::CaptureInput {
    clipboard_core::CaptureInput {
        captured_at_ms,
        kind: clipboard_core::ContentKind::Text,
        primary_mime: "text/plain".to_owned(),
        representations: vec![clipboard_core::RepresentationInput {
            format_id: "text/plain".to_owned(),
            bytes: Some(value.as_bytes().to_vec()),
            missing_ref: None,
        }],
        source_app_id: Some("com.example.synthetic".to_owned()),
        source_app_name: Some("Synthetic".to_owned()),
        source_confidence: clipboard_core::SourceConfidence::Declared,
        pinned: false,
        occurrence_count: 1,
        content_flags: clipboard_core::ContentFlags::empty(),
        event_flags: clipboard_core::EventFlags::LOCAL_ONLY,
    }
}

#[test]
fn existing_revision_six_database_upgrades_to_settings_without_losing_data() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("history.sqlite");
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch(include_str!(
            "../../crates/clipboard-store/src/migrations/001_initial.sql"
        ))
        .unwrap();
    connection
        .pragma_update(None, "user_version", 1_i64)
        .unwrap();
    connection
        .execute(
            "INSERT INTO content
             (content_hash, kind, primary_mime, byte_size, preview_text, flags, created_at_ms)
             VALUES (?1, 'text', 'text/plain', 9, 'sentinel', 0, 1725000000000)",
            rusqlite::params![vec![7_u8; 32]],
        )
        .unwrap();
    drop(connection);
    secure_existing_database(directory.path(), &database_path);

    let config = StoreConfig::new(&database_path).with_blob_root(directory.path().join("blobs"));
    let store = StoreHandle::open(config.clone()).unwrap();
    assert_eq!(schema_version(&store), 2);
    assert_eq!(schema_revision(&store), 6);
    assert!(settings_table_exists(&store));
    assert_eq!(content_count(&store), 1);
    drop(store);

    let reopened = StoreHandle::open(config).unwrap();
    assert_eq!(schema_version(&reopened), 2);
    assert_eq!(schema_revision(&reopened), 6);
    assert!(settings_table_exists(&reopened));
    assert_eq!(content_count(&reopened), 1);
}

#[test]
fn fresh_database_opens_at_settings_schema_and_reopens_idempotently() {
    let directory = tempfile::tempdir().unwrap();
    let config = StoreConfig::new(directory.path().join("history.sqlite"))
        .with_blob_root(directory.path().join("blobs"));

    let store = StoreHandle::open(config.clone()).unwrap();
    assert_eq!(schema_version(&store), 2);
    assert_eq!(schema_revision(&store), 6);
    assert!(settings_table_exists(&store));
    drop(store);

    let reopened = StoreHandle::open(config).unwrap();
    assert_eq!(schema_version(&reopened), 2);
    assert_eq!(schema_revision(&reopened), 6);
    assert!(settings_table_exists(&reopened));
}

#[tokio::test]
async fn settings_use_valid_defaults_and_persist_one_versioned_json_object() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();

    let defaults = commands::get_settings_service(&state).await.unwrap();
    assert_eq!(defaults.schema_version, 1);
    assert_eq!(defaults.hotkey, "CommandOrControl+Shift+V");
    assert!(!defaults.autostart);
    assert_eq!(defaults.retention_days, None);
    assert!(defaults.denylisted_apps.is_empty());

    let requested = AppSettingsDto {
        schema_version: 1,
        hotkey: "CommandOrControl+Shift+Space".to_owned(),
        autostart: true,
        retention_days: Some(365),
        denylisted_apps: vec!["com.example.synthetic".to_owned()],
    };
    let saved = commands::save_settings_service(&state, requested.clone())
        .await
        .unwrap();
    assert_eq!(saved, requested);
    let (count, value_json): (i64, String) = state
        .store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT count(*), value_json FROM app_setting WHERE key = 'app'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
        })
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&value_json).unwrap()["schemaVersion"],
        1
    );

    drop(state);
    let reopened = AppState::open_data_dir(directory.path()).unwrap();
    assert_eq!(
        commands::get_settings_service(&reopened).await.unwrap(),
        requested
    );
}

#[tokio::test]
async fn invalid_settings_are_rejected_without_overwriting_the_last_valid_value() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    let defaults = commands::get_settings_service(&state).await.unwrap();
    let invalid = AppSettingsDto {
        retention_days: Some(0),
        ..defaults.clone()
    };

    let error = commands::save_settings_service(&state, invalid)
        .await
        .unwrap_err();

    assert_eq!(error, "invalid_settings");
    assert_eq!(
        commands::get_settings_service(&state).await.unwrap(),
        defaults
    );
}

#[tokio::test]
async fn preview_pin_delete_and_storage_commands_expose_only_selected_bounded_data() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    let text = state
        .store
        .ingest(text_capture("wybrany podgląd", 1_725_000_000_100))
        .await
        .unwrap();
    state
        .store
        .ingest(missing_image_capture(1_725_000_000_200))
        .await
        .unwrap();

    let preview = commands::get_preview_service(&state, text.event_id)
        .await
        .unwrap();
    assert_eq!(preview.event_id, text.event_id);
    assert_eq!(preview.text.as_deref(), Some("wybrany podgląd"));
    assert_eq!(preview.mime_type, "text/plain");
    assert!(!preview.missing_payload);
    let preview_json = serde_json::to_value(&preview).unwrap();
    assert!(preview_json.get("blobRelpath").is_none());
    assert!(preview_json.get("filesystemPath").is_none());

    commands::set_pinned_service(&state, text.event_id, true)
        .await
        .unwrap();
    let page = commands::search_history_service(&state, SearchRequest::from_text("is:pinned"))
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].event_id, text.event_id);

    let stats = commands::get_storage_stats_service(&state).await.unwrap();
    assert_eq!(stats.content_count, 2);
    assert_eq!(stats.event_count, 2);
    assert_eq!(stats.missing_payload_count, 1);
    assert!(stats.database_bytes > 0);

    commands::delete_event_service(&state, text.event_id)
        .await
        .unwrap();
    let error = commands::get_preview_service(&state, text.event_id)
        .await
        .unwrap_err();
    assert_eq!(error, "history_event_not_found");
}

#[tokio::test]
async fn copy_preparation_restores_text_and_sanitizes_missing_payload_errors() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    let text = state
        .store
        .ingest(text_capture("tekst do skopiowania", 1_725_000_000_400))
        .await
        .unwrap();
    let missing = state
        .store
        .ingest(missing_image_capture(1_725_000_000_500))
        .await
        .unwrap();

    assert_eq!(
        commands::prepare_copy_text_service(&state, text.event_id, false)
            .await
            .unwrap(),
        "tekst do skopiowania"
    );
    assert_eq!(
        commands::prepare_copy_text_service(&state, missing.event_id, true)
            .await
            .unwrap_err(),
        "missing_payload"
    );
}

#[tokio::test]
async fn plain_text_copy_rejects_present_utf8_image_payload_before_any_clipboard_write() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    let image = state
        .store
        .ingest(image_capture(vec![b'\t'; 32], 1_725_000_000_450))
        .await
        .unwrap();

    let error = commands::prepare_copy_text_service(&state, image.event_id, true)
        .await
        .unwrap_err();

    assert_eq!(error, "copy_format_unavailable");
    assert!(!error.contains("\t"));
    assert!(!error.contains("image"));
}

#[test]
fn generated_command_handler_registers_each_desktop_command_once_and_accepts_camel_case_arguments()
{
    macro_rules! collect_command_names {
        ($( $name:ident => $command:path, )*) => {
            &[$(stringify!($name)),*]
        };
    }

    let names = clipboard_history_app::clipboard_history_command_registry!(collect_command_names);
    assert_eq!(
        names.as_slice(),
        &[
            "search_history",
            "get_preview",
            "set_pinned",
            "delete_event",
            "copy_event",
            "analyze_import",
            "start_import",
            "discard_import_analysis",
            "get_import_status",
            "get_settings",
            "save_settings",
            "get_storage_stats",
            "get_thumbnail",
        ]
    );

    let directory = tempfile::tempdir().unwrap();
    let app = command_test_app(AppState::open_data_dir(directory.path()).unwrap());
    let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();

    let saved = tauri::test::get_ipc_response(
        &window,
        ipc_request(
            "save_settings",
            json!({
                "settings": {
                    "schemaVersion": 1,
                    "hotkey": "CommandOrControl+Shift+Space",
                    "autostart": true,
                    "retentionDays": 365,
                    "denylistedApps": ["com.example.synthetic"]
                }
            }),
        ),
    )
    .unwrap()
    .deserialize::<serde_json::Value>()
    .unwrap();
    assert_eq!(saved["schemaVersion"], 1);
    assert_eq!(saved["retentionDays"], 365);
    assert!(saved.get("schema_version").is_none());

    let state = app.state::<AppState>();
    let image = state
        .store
        .ingest(image_capture(vec![b'\t'; 32], 1_725_000_000_451));
    let image = tauri::async_runtime::block_on(image).unwrap();
    let error = tauri::test::get_ipc_response(
        &window,
        ipc_request(
            "copy_event",
            json!({ "eventId": image.event_id, "plainText": true }),
        ),
    )
    .unwrap_err();
    assert_eq!(error, json!("copy_format_unavailable"));
}

#[cfg(unix)]
#[tokio::test]
async fn command_boundary_preserves_the_stable_private_storage_error() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    let text = state
        .store
        .ingest(text_capture("synthetic", 1_725_000_000_600))
        .await
        .unwrap();
    let blob_root = directory.path().join("blobs");
    fs::set_permissions(&blob_root, fs::Permissions::from_mode(0o755)).unwrap();

    let error = commands::get_preview_service(&state, text.event_id)
        .await
        .unwrap_err();

    assert_eq!(error, "private_storage_unavailable");
    fs::set_permissions(&blob_root, fs::Permissions::from_mode(0o700)).unwrap();
}

#[tokio::test]
async fn writer_mutations_report_zero_rows_and_concurrent_delete_has_one_winner() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    assert!(matches!(
        state.store.set_pinned(99_999, true).await,
        Err(clipboard_store::StoreError::HistoryEventNotFound)
    ));
    assert!(matches!(
        state.store.delete_event(99_999).await,
        Err(clipboard_store::StoreError::HistoryEventNotFound)
    ));

    let event = state
        .store
        .ingest(text_capture("concurrent delete", 1_725_000_000_700))
        .await
        .unwrap();
    let first = state.store.clone();
    let second = state.store.clone();
    let first_delete = tokio::spawn(async move { first.delete_event(event.event_id).await });
    let second_delete = tokio::spawn(async move { second.delete_event(event.event_id).await });
    let outcomes = [first_delete.await.unwrap(), second_delete.await.unwrap()];

    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(
                outcome,
                Err(clipboard_store::StoreError::HistoryEventNotFound)
            ))
            .count(),
        1
    );
}

#[tokio::test(flavor = "current_thread")]
async fn blocked_reader_command_yields_the_tokio_worker() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    let occupied = Arc::new(Barrier::new(clipboard_store::MAX_STORE_READERS + 1));
    let release = Arc::new(Barrier::new(clipboard_store::MAX_STORE_READERS + 1));
    let mut readers = Vec::new();
    for _ in 0..clipboard_store::MAX_STORE_READERS {
        let store = state.store.clone();
        let occupied = Arc::clone(&occupied);
        let release = Arc::clone(&release);
        readers.push(thread::spawn(move || {
            store
                .with_reader(|_| {
                    occupied.wait();
                    release.wait();
                    Ok::<_, rusqlite::Error>(())
                })
                .unwrap();
        }));
    }
    occupied.wait();
    let releaser = {
        let release = Arc::clone(&release);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            release.wait();
        })
    };
    let heartbeat = Arc::new(AtomicBool::new(false));
    let heartbeat_task = {
        let heartbeat = Arc::clone(&heartbeat);
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            heartbeat.store(true, Ordering::Release);
        })
    };

    commands::search_history_service(&state, SearchRequest::default())
        .await
        .unwrap();

    assert!(heartbeat.load(Ordering::Acquire));
    heartbeat_task.await.unwrap();
    releaser.join().unwrap();
    for reader in readers {
        reader.join().unwrap();
    }
}

#[tokio::test]
async fn importer_commands_keep_analysis_owner_scoped_and_start_only_by_analysis_id() {
    let export = tempfile::tempdir().unwrap();
    fs::write(
        export.path().join("clipboard.json"),
        serde_json::to_vec(&vec![json!({
            "createdAt": "2026-08-22T12:00:00.000Z",
            "modifiedAt": "2026-08-22T12:00:00.000Z",
            "category": "text",
            "copyCount": 1,
            "applicationPath": "/Applications/Synthetic.app",
            "text": "synthetic command fixture"
        })])
        .unwrap(),
    )
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();

    let analysis = commands::analyze_import_service(&state, export.path().to_path_buf())
        .await
        .unwrap();
    assert_eq!(analysis.total, 1);
    assert_eq!(analysis.candidate_records, 1);
    assert_eq!(analysis.failed, 0);

    let foreign = AppState {
        store: state.store.clone(),
        importer: clipboard_import::ImportService::new(state.store.clone()).unwrap(),
    };
    let error = commands::start_import_service(&foreign, &analysis.analysis_id.to_string())
        .await
        .unwrap_err();
    assert_eq!(error, "analysis_not_found");

    let handle = commands::start_import_service(&state, &analysis.analysis_id.to_string())
        .await
        .unwrap();
    assert_eq!(handle.run_id, analysis.analysis_id);
    let progress = wait_for_import(&state, handle.run_id).await;
    assert_eq!(progress.total, 1);
    assert_eq!(progress.processed, 1);
    assert_eq!(progress.imported, 1);

    let second_analysis = commands::analyze_import_service(&state, export.path().to_path_buf())
        .await
        .unwrap();
    commands::discard_import_analysis_service(&state, &second_analysis.analysis_id.to_string())
        .await
        .unwrap();
    let error = commands::start_import_service(&state, &second_analysis.analysis_id.to_string())
        .await
        .unwrap_err();
    assert_eq!(error, "analysis_not_found");
    assert_eq!(
        commands::start_import_service(&state, "not-a-token")
            .await
            .unwrap_err(),
        "invalid_analysis_id"
    );
}

#[tokio::test]
async fn thumbnail_is_event_scoped_and_rejects_encoded_responses_over_256_kib() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    let image = state
        .store
        .ingest(image_capture(vec![9_u8; 32], 1_725_000_000_300))
        .await
        .unwrap();
    let content_id = state
        .store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT content_id FROM history_event WHERE event_id = ?1",
                [image.event_id],
                |row| row.get::<_, i64>(0),
            )
        })
        .unwrap();
    drop(state);

    install_thumbnail(directory.path(), content_id, &[1, 2, 3]);
    let state = AppState::open_data_dir(directory.path()).unwrap();
    let thumbnail = commands::get_thumbnail_service(&state, image.event_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(thumbnail.mime_type, "image/png");
    assert_eq!(thumbnail.base64, "AQID");
    assert_eq!(
        commands::get_thumbnail_service(&state, image.event_id + 10_000)
            .await
            .unwrap_err(),
        "history_event_not_found"
    );
    drop(state);

    install_thumbnail(directory.path(), content_id, &vec![4_u8; 196_609]);
    let state = AppState::open_data_dir(directory.path()).unwrap();
    assert_eq!(
        commands::get_thumbnail_service(&state, image.event_id)
            .await
            .unwrap_err(),
        "thumbnail_too_large"
    );
}

async fn wait_for_import(state: &AppState, run_id: uuid::Uuid) -> clipboard_import::ImportProgress {
    for _ in 0..50_000 {
        let progress = commands::get_import_status_service(state, &run_id.to_string())
            .await
            .unwrap();
        if progress.state != clipboard_import::ImportRunState::Running {
            return progress;
        }
        tokio::task::yield_now().await;
    }
    panic!("synthetic import did not finish");
}

fn missing_image_capture(captured_at_ms: i64) -> clipboard_core::CaptureInput {
    clipboard_core::CaptureInput {
        captured_at_ms,
        kind: clipboard_core::ContentKind::Image,
        primary_mime: "image/png".to_owned(),
        representations: vec![clipboard_core::RepresentationInput {
            format_id: "image/png".to_owned(),
            bytes: None,
            missing_ref: Some("synthetic/missing.png".to_owned()),
        }],
        source_app_id: None,
        source_app_name: Some("Synthetic".to_owned()),
        source_confidence: clipboard_core::SourceConfidence::Declared,
        pinned: false,
        occurrence_count: 1,
        content_flags: clipboard_core::ContentFlags::MISSING_PAYLOAD,
        event_flags: clipboard_core::EventFlags::IMPORTED,
    }
}

fn image_capture(bytes: Vec<u8>, captured_at_ms: i64) -> clipboard_core::CaptureInput {
    clipboard_core::CaptureInput {
        captured_at_ms,
        kind: clipboard_core::ContentKind::Image,
        primary_mime: "image/png".to_owned(),
        representations: vec![clipboard_core::RepresentationInput {
            format_id: "image/png".to_owned(),
            bytes: Some(bytes),
            missing_ref: None,
        }],
        source_app_id: None,
        source_app_name: Some("Synthetic".to_owned()),
        source_confidence: clipboard_core::SourceConfidence::Declared,
        pinned: false,
        occurrence_count: 1,
        content_flags: clipboard_core::ContentFlags::empty(),
        event_flags: clipboard_core::EventFlags::LOCAL_ONLY,
    }
}

fn install_thumbnail(data_dir: &std::path::Path, content_id: i64, bytes: &[u8]) {
    let cas = clipboard_store::CasStore::new(data_dir.join("blobs"));
    let blob = cas.put(bytes).unwrap();
    let connection = rusqlite::Connection::open(data_dir.join("history.sqlite")).unwrap();
    connection
        .execute(
            "INSERT INTO artifact
               (content_id, artifact_kind, blob_relpath, byte_size, raw_digest, created_at_ms)
             VALUES (?1, 'thumbnail', ?2, ?3, ?4, 1725000000000)
             ON CONFLICT(content_id, artifact_kind) DO UPDATE SET
               blob_relpath = excluded.blob_relpath,
               byte_size = excluded.byte_size,
               raw_digest = excluded.raw_digest",
            rusqlite::params![
                content_id,
                blob.relpath,
                i64::try_from(bytes.len()).unwrap(),
                blob.hash.as_slice()
            ],
        )
        .unwrap();
}

fn schema_version(store: &StoreHandle) -> i64 {
    store
        .with_reader(|connection| {
            connection.pragma_query_value(None, "user_version", |row| row.get(0))
        })
        .unwrap()
}

fn schema_revision(store: &StoreHandle) -> i64 {
    store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT revision FROM schema_identity WHERE identity = 'clipboard-store'",
                [],
                |row| row.get(0),
            )
        })
        .unwrap()
}

fn settings_table_exists(store: &StoreHandle) -> bool {
    store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM sqlite_schema
                   WHERE type = 'table' AND name = 'app_setting'
                 )",
                [],
                |row| row.get(0),
            )
        })
        .unwrap()
}

fn content_count(store: &StoreHandle) -> i64 {
    store
        .with_reader(|connection| {
            connection.query_row("SELECT count(*) FROM content", [], |row| row.get(0))
        })
        .unwrap()
}

#[cfg(unix)]
fn secure_existing_database(data_root: &std::path::Path, database: &std::path::Path) {
    use std::{fs, os::unix::fs::PermissionsExt};

    fs::set_permissions(data_root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(database, fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(not(unix))]
fn secure_existing_database(_: &std::path::Path, _: &std::path::Path) {}
