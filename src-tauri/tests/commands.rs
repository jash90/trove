use clipboard_history_app::{
    commands::{self, AppSettingsDto},
    state::{AppState, LAUNCHER_CACHE_TTL_MS, LauncherState},
};
use clipboard_search::SearchRequest;
use clipboard_store::{StoreConfig, StoreHandle};
use serde_json::json;
use std::{
    fs,
    future::Future,
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

fn file_capture(reference_uri: &str, captured_at_ms: i64) -> clipboard_core::CaptureInput {
    clipboard_core::CaptureInput {
        captured_at_ms,
        kind: clipboard_core::ContentKind::File,
        primary_mime: "application/octet-stream".to_owned(),
        representations: vec![
            clipboard_core::RepresentationInput {
                format_id: "application/octet-stream".to_owned(),
                bytes: None,
                missing_ref: Some("synthetic-missing:reference".to_owned()),
            },
            clipboard_core::RepresentationInput {
                format_id: "text/uri-list".to_owned(),
                bytes: Some(reference_uri.as_bytes().to_vec()),
                missing_ref: None,
            },
        ],
        source_app_id: None,
        source_app_name: None,
        source_confidence: clipboard_core::SourceConfidence::Unknown,
        pinned: false,
        occurrence_count: 1,
        content_flags: clipboard_core::ContentFlags::MISSING_PAYLOAD,
        event_flags: clipboard_core::EventFlags::IMPORTED,
        display_label: None,
    }
}

#[test]
fn a_saved_shortcut_is_parsed_back_into_the_one_that_will_be_registered() {
    use clipboard_history_app::hotkey;

    // The settings screen stores the shortcut as text; registering it means
    // parsing that text back. A round trip that silently failed would leave
    // the screen showing one shortcut while another answered.
    let parsed = hotkey::parse_shortcut("CommandOrControl+Shift+Space")
        .expect("the default must survive the round trip");
    assert_eq!(parsed, hotkey::default_shortcut());

    assert!(hotkey::parse_shortcut("Control+Alt+7").is_some());
    // Nonsense yields nothing rather than the default: quietly registering a
    // different shortcut leaves the user pressing keys that do nothing.
    assert!(hotkey::parse_shortcut("not-a-shortcut").is_none());
    assert!(hotkey::parse_shortcut("").is_none());
}

#[test]
fn the_active_shortcut_remembers_what_to_take_down_on_a_rebind() {
    use clipboard_history_app::hotkey;

    let active = hotkey::ActiveShortcut::default();
    assert_eq!(active.get(), hotkey::default_shortcut());

    let next = hotkey::parse_shortcut("Control+Alt+7").unwrap();
    active.set(next);
    assert_eq!(active.get(), next);
}

#[test]
fn the_summoning_shortcut_is_command_shift_space() {
    use tauri_plugin_global_shortcut::{Code, Modifiers};

    let shortcut = clipboard_history_app::hotkey::default_shortcut();

    // Space is what a launcher-style palette answers to; the added Shift keeps
    // it clear of the input-source switcher and of Spotlight.
    assert_eq!(shortcut.key, Code::Space);
    assert_eq!(shortcut.mods, Modifiers::SUPER | Modifiers::SHIFT);
}

#[tokio::test]
async fn the_application_opens_the_same_database_file_the_importer_writes() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    state
        .store
        .ingest(text_capture("wpis kontrolny", 1_725_000_000_000))
        .await
        .unwrap();
    drop(state);

    // The importer resolves the same layout from clipboard-store, so a data
    // directory written by one half must open in the other.
    assert!(
        directory
            .path()
            .join(clipboard_store::DATABASE_FILENAME)
            .is_file()
    );
    assert!(
        directory
            .path()
            .join(clipboard_store::BLOB_DIRECTORY_NAME)
            .is_dir()
    );
}

#[tokio::test]
async fn preview_reports_an_existing_source_path_as_revealable() {
    let elsewhere = tempfile::tempdir().unwrap();
    let present = elsewhere.path().join("synthetic report.pdf");
    fs::write(&present, b"synthetic").unwrap();
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    let outcome = state
        .store
        .ingest(file_capture(
            &format!("file://{}", present.display()).replace(' ', "%20"),
            1_725_000_000_000,
        ))
        .await
        .unwrap();

    let preview = commands::get_preview_service(&state, outcome.event_id)
        .await
        .unwrap();
    let json = serde_json::to_value(&preview).unwrap();

    assert_eq!(
        json["sourcePath"].as_str(),
        Some(present.to_str().unwrap()),
        "the preview shows the decoded path, not the percent-encoded reference"
    );
    assert_eq!(json["sourceExists"], serde_json::Value::Bool(true));
}

#[tokio::test]
async fn preview_marks_a_vanished_source_path_as_unavailable() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    let outcome = state
        .store
        .ingest(file_capture(
            "file:///synthetic/never/existed.pdf",
            1_725_000_000_000,
        ))
        .await
        .unwrap();

    let preview = commands::get_preview_service(&state, outcome.event_id)
        .await
        .unwrap();
    let json = serde_json::to_value(&preview).unwrap();

    assert_eq!(
        json["sourcePath"].as_str(),
        Some("/synthetic/never/existed.pdf")
    );
    assert_eq!(json["sourceExists"], serde_json::Value::Bool(false));
}

#[tokio::test]
async fn revealing_a_vanished_source_reports_a_stable_code_without_the_path() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    let outcome = state
        .store
        .ingest(file_capture(
            "file:///synthetic/never/existed.pdf",
            1_725_000_000_000,
        ))
        .await
        .unwrap();

    let error = commands::reveal_source_service(&state, outcome.event_id)
        .await
        .unwrap_err();

    assert_eq!(error, "source_missing");
}

#[tokio::test]
async fn a_text_entry_has_no_source_path_to_reveal() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    let outcome = state
        .store
        .ingest(text_capture("bez odniesienia", 1_725_000_000_000))
        .await
        .unwrap();

    let preview = commands::get_preview_service(&state, outcome.event_id)
        .await
        .unwrap();
    let json = serde_json::to_value(&preview).unwrap();

    assert_eq!(json["sourcePath"], serde_json::Value::Null);
    assert_eq!(json["sourceExists"], serde_json::Value::Bool(false));
    assert_eq!(
        commands::reveal_source_service(&state, outcome.event_id)
            .await
            .unwrap_err(),
        "source_unavailable"
    );
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
        display_label: None,
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
    assert_eq!(schema_version(&store), 5);
    assert_eq!(schema_revision(&store), 6);
    assert!(settings_table_exists(&store));
    assert_eq!(content_count(&store), 1);
    drop(store);

    let reopened = StoreHandle::open(config).unwrap();
    assert_eq!(schema_version(&reopened), 5);
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
    assert_eq!(schema_version(&store), 5);
    assert_eq!(schema_revision(&store), 6);
    assert!(settings_table_exists(&store));
    drop(store);

    let reopened = StoreHandle::open(config).unwrap();
    assert_eq!(schema_version(&reopened), 5);
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
        link_previews: false,
        keyvault: Default::default(),
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
        .ingest(text_capture("the selected preview", 1_725_000_000_100))
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
    assert_eq!(preview.text.as_deref(), Some("the selected preview"));
    assert_eq!(preview.mime_type, "text/plain");
    let preview_json = serde_json::to_value(&preview).unwrap();
    // The wire contract no longer carries a missing-payload flag: the importer
    // leaves such records out, so nothing downstream has to represent them.
    assert!(preview_json.get("missingPayload").is_none());
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
async fn deleting_a_listed_group_removes_every_occurrence_behind_it() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    for offset in 0..3 {
        state
            .store
            .ingest(text_capture(
                "powtarzana treść",
                1_725_000_100_000 + offset * 1_000,
            ))
            .await
            .unwrap();
    }
    let other = state
        .store
        .ingest(text_capture("inna treść", 1_725_000_200_000))
        .await
        .unwrap();

    let page = commands::search_history_service(&state, SearchRequest::from_text(""))
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2, "one row per distinct content");
    let grouped_index = page
        .items
        .iter()
        .position(|item| item.preview == "powtarzana treść")
        .unwrap();
    let grouped = &page.items[grouped_index];
    assert_eq!(grouped.occurrence_count, 3);
    assert_eq!(grouped.occurrences.len(), 3);
    let page_json = serde_json::to_value(&page).unwrap();
    assert_eq!(
        page_json["items"][grouped_index]["occurrenceCount"],
        serde_json::json!(3)
    );
    assert_eq!(
        page_json["items"][grouped_index]["occurrences"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    commands::delete_event_service(&state, grouped.event_id)
        .await
        .unwrap();

    let after = commands::search_history_service(&state, SearchRequest::from_text(""))
        .await
        .unwrap();
    assert_eq!(after.items.len(), 1);
    assert_eq!(after.items[0].event_id, other.event_id);
    let stats = commands::get_storage_stats_service(&state).await.unwrap();
    assert_eq!(stats.event_count, 1);
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
            "reveal_source",
            "list_apps",
            "launch_app",
            "get_app_icon",
            "open_settings_window",
            "export_history",
            "get_link_preview",
            "keyvault_list",
            "keyvault_copy_secret",
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
            json!({ "eventId": image.event_id, "plainText": true, "paste": false }),
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

/// A throwaway `.app` under a throwaway root, with a handwritten XML plist —
/// the same shape the launcher crate's own fixtures build, kept here so these
/// command tests never depend on what is installed on the machine.
fn make_synthetic_app(
    root: &std::path::Path,
    dir_name: &str,
    bundle_name: &str,
) -> std::path::PathBuf {
    let contents = root.join(dir_name).join("Contents");
    fs::create_dir_all(&contents).unwrap();
    let info = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\
         <plist version=\"1.0\"><dict><key>CFBundleName</key><string>{bundle_name}</string></dict></plist>"
    );
    fs::write(contents.join("Info.plist"), info).unwrap();
    root.join(dir_name)
}

#[tokio::test]
async fn list_apps_returns_a_cached_camel_case_catalog_from_the_injected_roots() {
    let directory = tempfile::tempdir().unwrap();
    let mut state = AppState::open_data_dir(directory.path()).unwrap();
    let apps_root = tempfile::tempdir().unwrap();
    make_synthetic_app(apps_root.path(), "Synthetic.app", "Synthetic");
    state.launcher = LauncherState::scanning(vec![apps_root.path().to_path_buf()]);

    let catalog = commands::list_apps_service(&state, 1_000).await.unwrap();

    assert_eq!(catalog.len(), 1);
    let json = serde_json::to_value(&catalog).unwrap();
    assert_eq!(json[0]["name"], "Synthetic");
    assert!(json[0].get("bundleId").is_some());
    assert!(json[0].get("bundle_id").is_none());
}

#[tokio::test]
async fn the_launcher_catalog_is_rescanned_only_after_the_ttl_expires() {
    let directory = tempfile::tempdir().unwrap();
    let mut state = AppState::open_data_dir(directory.path()).unwrap();
    let apps_root = tempfile::tempdir().unwrap();
    make_synthetic_app(apps_root.path(), "Early.app", "Early");
    state.launcher = LauncherState::scanning(vec![apps_root.path().to_path_buf()]);

    let first = commands::list_apps_service(&state, 1_000).await.unwrap();
    assert_eq!(first.len(), 1);

    // Installed after the scan: invisible until the cache ages out, so an
    // open palette does not pay for a directory walk on every keystroke.
    make_synthetic_app(apps_root.path(), "Late.app", "Late");
    let still_cached = commands::list_apps_service(&state, 1_500).await.unwrap();
    assert_eq!(still_cached.len(), 1);

    let refreshed = commands::list_apps_service(&state, 1_000 + LAUNCHER_CACHE_TTL_MS + 1)
        .await
        .unwrap();
    assert_eq!(refreshed.len(), 2);
}

#[tokio::test]
async fn launch_app_refuses_paths_the_scanner_did_not_find() {
    let directory = tempfile::tempdir().unwrap();
    let mut state = AppState::open_data_dir(directory.path()).unwrap();
    let apps_root = tempfile::tempdir().unwrap();
    make_synthetic_app(apps_root.path(), "Inside.app", "Inside");
    fs::write(apps_root.path().join("File.app"), "not a bundle").unwrap();
    state.launcher = LauncherState::scanning(vec![apps_root.path().to_path_buf()]);
    let outside_root = tempfile::tempdir().unwrap();
    make_synthetic_app(outside_root.path(), "Outside.app", "Outside");

    let too_long = "a".repeat(2_048);
    let missing = apps_root
        .path()
        .join("Missing.app")
        .to_str()
        .unwrap()
        .to_owned();
    let file_path = apps_root
        .path()
        .join("File.app")
        .to_str()
        .unwrap()
        .to_owned();
    let outside = outside_root
        .path()
        .join("Outside.app")
        .to_str()
        .unwrap()
        .to_owned();
    let cases: &[(&str, &str)] = &[
        ("", "launch_invalid"),
        ("relative/No.app", "launch_invalid"),
        (too_long.as_str(), "launch_invalid"),
        (missing.as_str(), "app_not_found"),
        (file_path.as_str(), "app_not_launchable"),
        (outside.as_str(), "app_outside_roots"),
    ];
    for (candidate, expected) in cases {
        let error = commands::launch_app_service(&state, candidate.to_string())
            .await
            .unwrap_err();
        // A stable code, and never the refused path played back. (The empty
        // candidate is contained in everything; there is nothing to leak.)
        assert_eq!(error, *expected);
        if !candidate.is_empty() {
            assert!(!error.contains(candidate));
        }
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn get_app_icon_returns_a_decodable_png_for_a_scanned_bundle() {
    let directory = tempfile::tempdir().unwrap();
    let mut state = AppState::open_data_dir(directory.path()).unwrap();
    let apps_root = tempfile::tempdir().unwrap();
    make_synthetic_app(apps_root.path(), "Iconed.app", "Iconed");
    state.launcher = LauncherState::scanning(vec![apps_root.path().to_path_buf()]);
    let path = apps_root
        .path()
        .join("Iconed.app")
        .to_str()
        .unwrap()
        .to_owned();

    let icon = commands::get_app_icon_service(&state, path.clone())
        .await
        .unwrap()
        .expect("a scanned bundle owes an icon, the generic one at worst");

    assert_eq!(icon.mime_type, "image/png");
    let raw = base64_decode_prefix(&icon.base64);
    // A PNG announces itself in eight fixed bytes.
    assert_eq!(&raw[..8], b"\x89PNG\r\n\x1a\n", "the icon is not a PNG");
    let decoded = image::load_from_memory(&raw).expect("a decodable PNG");
    assert_eq!(
        (decoded.width(), decoded.height()),
        (64, 64),
        "the icon ships at the row's size, not the artwork's"
    );

    // The same path asked again answers from the cache: the same bytes,
    // no second round through NSWorkspace and the resizer.
    let again = commands::get_app_icon_service(&state, path)
        .await
        .unwrap()
        .expect("the cached icon must still be there");
    assert_eq!(again.base64, icon.base64);
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn get_app_icon_refuses_paths_the_scanner_did_not_find() {
    let directory = tempfile::tempdir().unwrap();
    let mut state = AppState::open_data_dir(directory.path()).unwrap();
    let apps_root = tempfile::tempdir().unwrap();
    make_synthetic_app(apps_root.path(), "Inside.app", "Inside");
    state.launcher = LauncherState::scanning(vec![apps_root.path().to_path_buf()]);
    let outside_root = tempfile::tempdir().unwrap();
    make_synthetic_app(outside_root.path(), "Outside.app", "Outside");

    let missing = apps_root
        .path()
        .join("Missing.app")
        .to_str()
        .unwrap()
        .to_owned();
    let outside = outside_root
        .path()
        .join("Outside.app")
        .to_str()
        .unwrap()
        .to_owned();
    let cases: &[(&str, &str)] = &[
        (missing.as_str(), "app_not_found"),
        (outside.as_str(), "app_outside_roots"),
    ];
    for (candidate, expected) in cases {
        let error = commands::get_app_icon_service(&state, candidate.to_string())
            .await
            .unwrap_err();
        assert_eq!(error, *expected);
        assert!(!error.contains(candidate));
    }
}

/// Decodes just enough base64 to check a magic prefix.
fn base64_decode_prefix(value: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(value.as_bytes())
        .unwrap()
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

    let analysis = commands::analyze_import_service(&state, export.path().to_path_buf(), None)
        .await
        .unwrap();
    assert_eq!(analysis.total, 1);
    assert_eq!(analysis.candidate_records, 1);
    assert_eq!(analysis.failed, 0);

    let foreign = AppState {
        store: state.store.clone(),
        importer: clipboard_import::ImportService::new(state.store.clone()).unwrap(),
        previews: std::sync::Mutex::new(clipboard_history_app::links::FetchCoordinator::default()),
        launcher: state.launcher.clone(),
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

    let second_analysis =
        commands::analyze_import_service(&state, export.path().to_path_buf(), None)
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

/// A real 4x4 PNG, so the generator has something it can actually decode.
fn synthetic_png() -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    let image = image::RgbaImage::from_fn(4, 4, |x, y| {
        image::Rgba([(x * 60) as u8, (y * 60) as u8, 120, 255])
    });
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut buffer, image::ImageFormat::Png)
        .unwrap();
    buffer.into_inner()
}

fn stored_thumbnail_count(state: &AppState) -> i64 {
    state
        .store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT count(*) FROM artifact WHERE artifact_kind = 'thumbnail'",
                [],
                |row| row.get::<_, i64>(0),
            )
        })
        .unwrap()
}

#[tokio::test]
async fn an_image_without_a_thumbnail_gets_one_the_first_time_it_is_viewed() {
    // Thumbnails were never produced by anything, so every image already in a
    // history has none. Asking for one renders it and keeps it.
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    let image = state
        .store
        .ingest(image_capture(synthetic_png(), 1_725_000_000_400))
        .await
        .unwrap();
    // Capture already rendered one. Remove it to stand in for every image
    // stored before thumbnails existed at all.
    drop(state);
    remove_thumbnails(directory.path());
    let state = AppState::open_data_dir(directory.path()).unwrap();
    assert_eq!(stored_thumbnail_count(&state), 0);

    let rendered = commands::get_thumbnail_service(&state, image.event_id)
        .await
        .unwrap()
        .expect("an image with a payload has a thumbnail to render");

    assert_eq!(rendered.mime_type, "image/png");
    assert!(!rendered.base64.is_empty());
    assert_eq!(stored_thumbnail_count(&state), 1);

    // Second look is served from what the first one stored.
    let again = commands::get_thumbnail_service(&state, image.event_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.base64, rendered.base64);
    assert_eq!(stored_thumbnail_count(&state), 1);
}

#[tokio::test]
async fn an_entry_with_nothing_to_render_reports_no_thumbnail_rather_than_an_error() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    let text = state
        .store
        .ingest(text_capture("synthetic text", 1_725_000_000_500))
        .await
        .unwrap();

    assert!(
        commands::get_thumbnail_service(&state, text.event_id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(stored_thumbnail_count(&state), 0);
}

#[tokio::test]
async fn an_export_can_be_imported_again_as_the_same_history() {
    // The only definition of "correct" this format has: the reader next door
    // accepts what the writer produced, and produces the same entries from it.
    let source_dir = tempfile::tempdir().unwrap();
    let output_dir = tempfile::tempdir().unwrap();
    let reimport_dir = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(source_dir.path()).unwrap();
    state
        .store
        .ingest(text_capture("synthetic, with a comma", 1_725_000_000_100))
        .await
        .unwrap();
    state
        .store
        .ingest(text_capture(
            "synthetic \"quoted\"\nand wrapped",
            1_725_000_000_200,
        ))
        .await
        .unwrap();
    state
        .store
        .ingest(image_capture(synthetic_png(), 1_725_000_000_300))
        .await
        .unwrap();

    let export_dir = output_dir.path().join("export");
    let summary = commands::export_history_service(&state, export_dir.clone())
        .await
        .unwrap();
    assert_eq!(summary.records, 3);
    assert_eq!(summary.images, 1);
    assert!(export_dir.join("clipboard.json").is_file());
    assert!(export_dir.join("clipboard.csv").is_file());

    // Read back through the importer, into a database that has never seen it.
    let reimported = AppState::open_data_dir(reimport_dir.path()).unwrap();
    let analysis = commands::analyze_import_service(&reimported, export_dir.clone(), None)
        .await
        .unwrap();
    assert_eq!(analysis.total, 3);
    assert_eq!(analysis.candidate_records, 3);
    assert_eq!(analysis.failed, 0);

    let handle = commands::start_import_service(&reimported, &analysis.analysis_id.to_string())
        .await
        .unwrap();
    let progress = wait_for_import(&reimported, handle.run_id).await;
    let summary = progress.summary.expect("a finished run reports a summary");
    assert_eq!(summary.imported, 3);
    assert_eq!(summary.failed, 0);

    // The text came back through CSV-hostile characters intact.
    let previews = reimported
        .store
        .with_reader(|connection| {
            let mut statement =
                connection.prepare("SELECT preview_text FROM content ORDER BY content_id")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap();
    assert!(
        previews
            .iter()
            .any(|preview| preview.contains("with a comma"))
    );
    assert!(
        previews
            .iter()
            .any(|preview| preview.contains("and wrapped"))
    );
}

#[tokio::test]
async fn an_export_refuses_a_directory_that_already_holds_something() {
    let source_dir = tempfile::tempdir().unwrap();
    let output_dir = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(source_dir.path()).unwrap();
    let occupied = output_dir.path().join("occupied");
    std::fs::create_dir_all(&occupied).unwrap();
    std::fs::write(occupied.join("something.txt"), b"already here").unwrap();

    // Mixing two exports into one directory would produce a manifest pair that
    // describes neither.
    assert_eq!(
        commands::export_history_service(&state, occupied)
            .await
            .unwrap_err(),
        "export_destination_not_empty"
    );
}

fn link_capture(url: &str, captured_at_ms: i64) -> clipboard_core::CaptureInput {
    clipboard_core::CaptureInput {
        captured_at_ms,
        kind: clipboard_core::ContentKind::Link,
        primary_mime: "text/uri-list".to_owned(),
        representations: vec![clipboard_core::RepresentationInput {
            format_id: "text/uri-list".to_owned(),
            bytes: Some(url.as_bytes().to_vec()),
            missing_ref: None,
        }],
        source_app_id: None,
        source_app_name: Some("Synthetic".to_owned()),
        source_confidence: clipboard_core::SourceConfidence::Declared,
        pinned: false,
        occurrence_count: 1,
        content_flags: clipboard_core::ContentFlags::empty(),
        event_flags: clipboard_core::EventFlags::LOCAL_ONLY,
        display_label: None,
    }
}

#[tokio::test]
async fn a_link_describes_itself_without_contacting_anything() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    // Fetching off: the answer must still say what the address is.
    let settings = clipboard_history_app::commands::AppSettingsDto {
        link_previews: false,
        ..clipboard_history_app::commands::AppSettingsDto::default()
    };
    commands::save_settings_service(&state, settings)
        .await
        .unwrap();
    let link = state
        .store
        .ingest(link_capture(
            "https://www.example.invalid/some/page?q=1",
            1_725_000_000_700,
        ))
        .await
        .unwrap();

    let preview = commands::get_link_preview_service(&state, link.event_id)
        .await
        .unwrap()
        .expect("a link entry describes itself");

    assert_eq!(preview.host, "example.invalid");
    assert_eq!(preview.rest, "/some/page?q=1");
    assert!(preview.local_only);
    assert_eq!(preview.title, None);
}

#[tokio::test]
async fn a_link_is_described_whatever_its_address_looks_like() {
    // Two shapes that behaved differently on a real history: one with a file
    // extension, one a plain path.
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    let settings = clipboard_history_app::commands::AppSettingsDto {
        link_previews: false,
        ..clipboard_history_app::commands::AppSettingsDto::default()
    };
    commands::save_settings_service(&state, settings)
        .await
        .unwrap();

    for (index, url) in [
        "https://i1.kwejk.pl/k/obrazki/2016/02/a25fcbe712fe800a4f304cd8b79056f3.jpg",
        "https://github.com/pingdotgg/t3code/pull/5227",
    ]
    .into_iter()
    .enumerate()
    {
        let link = state
            .store
            .ingest(link_capture(url, 1_725_000_001_000 + index as i64))
            .await
            .unwrap();
        let preview = commands::get_link_preview_service(&state, link.event_id)
            .await
            .unwrap();
        assert!(preview.is_some(), "{url} produced no description");
    }
}

#[tokio::test]
async fn a_link_preview_blob_is_not_collected_as_an_orphan() {
    // A table that stores a blob path has to be named in the liveness check.
    // This one was added without being named, so the cleanup pass deleted
    // every icon and picture it stored, sixty seconds after they arrived.
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    let link = state
        .store
        .ingest(link_capture(
            "https://example.invalid/page",
            1_725_000_001_100,
        ))
        .await
        .unwrap();
    let content_id = state
        .store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT content_id FROM history_event WHERE event_id = ?1",
                [link.event_id],
                |row| row.get::<_, i64>(0),
            )
        })
        .unwrap();
    state
        .store
        .store_link_preview(
            content_id,
            clipboard_store::LinkPreviewRecord {
                status: clipboard_store::LinkPreviewStatus::Ok,
                title: Some("Synthetic page".to_owned()),
                icon: Some(vec![1, 2, 3]),
                icon_mime: Some("image/png".to_owned()),
                image: Some(vec![4, 5, 6, 7]),
                image_mime: Some("image/png".to_owned()),
            },
        )
        .await
        .unwrap();

    let relpaths: Vec<String> = state
        .store
        .with_reader(|connection| {
            let mut statement = connection.prepare(
                "SELECT icon_relpath FROM link_preview WHERE icon_relpath IS NOT NULL
                 UNION ALL
                 SELECT image_relpath FROM link_preview WHERE image_relpath IS NOT NULL",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap();
    assert_eq!(relpaths.len(), 2);

    // Both must be seen as live; anything else and the next pass removes them.
    for relpath in &relpaths {
        let referenced = state
            .store
            .with_reader(|connection| {
                connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM raw_payload WHERE blob_relpath = ?1)
                         OR EXISTS(SELECT 1 FROM artifact WHERE blob_relpath = ?1)
                         OR EXISTS(SELECT 1 FROM link_preview WHERE icon_relpath = ?1)
                         OR EXISTS(SELECT 1 FROM link_preview WHERE image_relpath = ?1)",
                    [relpath],
                    |row| row.get::<_, bool>(0),
                )
            })
            .unwrap();
        assert!(referenced, "{relpath} would be collected as an orphan");
        assert!(directory.path().join("blobs").join(relpath).is_file());
    }
}

#[tokio::test]
async fn asking_twice_for_a_link_answers_twice() {
    // The pane asks again when a fetch lands. A second ask that answered with
    // nothing would make the card appear and then vanish.
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    let settings = clipboard_history_app::commands::AppSettingsDto {
        link_previews: false,
        ..clipboard_history_app::commands::AppSettingsDto::default()
    };
    commands::save_settings_service(&state, settings)
        .await
        .unwrap();
    let link = state
        .store
        .ingest(link_capture(
            "https://example.invalid/page",
            1_725_000_000_900,
        ))
        .await
        .unwrap();

    for attempt in 0..3 {
        let preview = commands::get_link_preview_service(&state, link.event_id)
            .await
            .unwrap();
        assert!(preview.is_some(), "attempt {attempt} answered with nothing");
    }
}

#[tokio::test]
async fn an_entry_that_is_not_a_link_has_nothing_to_describe() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    let text = state
        .store
        .ingest(text_capture("not a link", 1_725_000_000_800))
        .await
        .unwrap();

    assert!(
        commands::get_link_preview_service(&state, text.event_id)
            .await
            .unwrap()
            .is_none()
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
        display_label: None,
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
        display_label: None,
    }
}

fn remove_thumbnails(data_dir: &std::path::Path) {
    rusqlite::Connection::open(data_dir.join(clipboard_store::DATABASE_FILENAME))
        .unwrap()
        .execute("DELETE FROM artifact WHERE artifact_kind = 'thumbnail'", [])
        .unwrap();
}

fn install_thumbnail(data_dir: &std::path::Path, content_id: i64, bytes: &[u8]) {
    let cas = clipboard_store::CasStore::new(data_dir.join("blobs"));
    let blob = cas.put(bytes).unwrap();
    let connection =
        rusqlite::Connection::open(data_dir.join(clipboard_store::DATABASE_FILENAME)).unwrap();
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

/// Live check of the whole preview path against a real site.
///
/// Ignored by default because it speaks to the network; run it by hand after
/// touching the fetching code:
/// `cargo test -p clipboard-history-app --test commands -- --ignored --nocapture`
#[tokio::test]
#[ignore = "talks to the real internet; run manually"]
async fn link_preview_fetches_stores_and_renders_a_real_page() {
    use rusqlite::OptionalExtension;

    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    let event_id = state
        .store
        .ingest(clipboard_core::CaptureInput {
            captured_at_ms: 1_725_000_000_000,
            kind: clipboard_core::ContentKind::Link,
            primary_mime: "text/plain".to_owned(),
            representations: vec![clipboard_core::RepresentationInput {
                format_id: "text/plain".to_owned(),
                bytes: Some(b"https://github.com/rust-lang/rust".to_vec()),
                missing_ref: None,
            }],
            source_app_id: Some("com.example.synthetic".to_owned()),
            source_app_name: Some("Synthetic".to_owned()),
            source_confidence: clipboard_core::SourceConfidence::Declared,
            pinned: false,
            occurrence_count: 1,
            content_flags: clipboard_core::ContentFlags::empty(),
            event_flags: clipboard_core::EventFlags::LOCAL_ONLY,
            display_label: None,
        })
        .await
        .unwrap()
        .event_id;

    // Fetching must be on for anything to leave the machine.
    commands::save_settings_service(
        &state,
        commands::AppSettingsDto {
            link_previews: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // First ask answers at once from the address alone, fetching behind it.
    let first = commands::get_link_preview_service(&state, event_id)
        .await
        .unwrap();
    assert!(first.is_some(), "a link entry always describes itself");

    // The palette asks again when the ready event arrives; here polling takes
    // its place, since no window is listening.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let row = loop {
        let row = state.store.with_reader(|connection| {
            connection
                .query_row(
                    "SELECT status, title, icon_relpath IS NOT NULL, image_relpath IS NOT NULL
                     FROM link_preview WHERE content_id =
                       (SELECT content_id FROM history_event WHERE event_id = ?1)",
                    [event_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, bool>(2)?,
                            row.get::<_, bool>(3)?,
                        ))
                    },
                )
                .optional()
        });
        match row {
            Ok(Some(found)) if found.0 == "ok" => break found,
            _ if std::time::Instant::now() > deadline => {
                let last = row.ok().flatten();
                panic!("fetch never completed; last row: {last:?}");
            }
            _ => tokio::time::sleep(std::time::Duration::from_millis(300)).await,
        }
    };

    assert_eq!(row.0, "ok");
    assert!(row.2, "an icon was stored");
    assert!(row.3, "the page's own picture was stored");
    assert!(
        row.1
            .as_deref()
            .is_some_and(|title| title.to_ascii_lowercase().contains("rust")),
        "title carried the page's name: {:?}",
        row.1
    );

    // The second ask renders the remembered answer: bytes encoded, nothing fetched.
    let rendered = commands::get_link_preview_service(&state, event_id)
        .await
        .unwrap()
        .unwrap();
    assert!(!rendered.local_only);
    assert!(rendered.icon_base64.is_some());
    assert!(rendered.image_base64.is_some());
    assert_eq!(rendered.image_mime.as_deref(), Some("image/png"));
}

// --------------------------------------------------------------- keyvault --

/// A transport that answers from a script, so the command layer is tested
/// without the network — the same discipline the keyvault crate keeps.
struct CannedVault {
    responses: Vec<Result<clipboard_keyvault::SecretResponse, clipboard_keyvault::KeyvaultError>>,
}

impl CannedVault {
    fn with(
        responses: Vec<
            Result<clipboard_keyvault::SecretResponse, clipboard_keyvault::KeyvaultError>,
        >,
    ) -> Self {
        Self { responses }
    }

    fn json(
        status: u16,
        body: &str,
    ) -> Result<clipboard_keyvault::SecretResponse, clipboard_keyvault::KeyvaultError> {
        Ok(clipboard_keyvault::SecretResponse {
            status,
            body: body.to_owned(),
        })
    }
}

impl clipboard_keyvault::SecretTransport for CannedVault {
    #[allow(clippy::manual_async_fn)]
    fn get(
        &self,
        path: &str,
    ) -> impl Future<
        Output = Result<clipboard_keyvault::SecretResponse, clipboard_keyvault::KeyvaultError>,
    > + Send {
        let _ = path;
        async move { self.responses[0].clone() }
    }
}

fn jwk_json_for(key: &rsa::RsaPrivateKey) -> String {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use rsa::traits::{PrivateKeyParts, PublicKeyParts};

    let encode = |value: &rsa::BigUint| URL_SAFE_NO_PAD.encode(value.to_bytes_be());
    serde_json::json!({
        "kty": "RSA",
        "n": encode(key.n()),
        "e": encode(key.e()),
        "d": encode(key.d()),
        "p": encode(&key.primes()[0]),
        "q": encode(&key.primes()[1]),
    })
    .to_string()
}

fn valid_jwk_json() -> String {
    let mut rng = rsa::rand_core::OsRng;
    jwk_json_for(&rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap())
}

fn vault_settings() -> AppSettingsDto {
    let mut settings = AppSettingsDto::default();
    settings.keyvault.url = Some("https://trustworthy-eagle-783.convex.site".to_owned());
    settings.keyvault.token = Some("kv_AbCdEf0123456789-_".to_owned());
    settings.keyvault.private_jwk = Some(valid_jwk_json());
    settings
}

#[tokio::test]
async fn keyvault_settings_default_off_and_old_rows_still_parse() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();

    // A row written before the keyvault section existed.
    let legacy_row = serde_json::json!({
        "schemaVersion": 1,
        "hotkey": "CommandOrControl+Shift+Space",
        "autostart": false,
        "retentionDays": null,
        "denylistedApps": [],
        "linkPreviews": true
    })
    .to_string();
    state.store.save_setting("app", &legacy_row).await.unwrap();

    let settings = commands::get_settings_service(&state).await.unwrap();
    assert_eq!(settings.keyvault.url, None);
    assert_eq!(settings.keyvault.token, None);
    assert_eq!(settings.keyvault.private_jwk, None);
}

#[tokio::test]
async fn keyvault_settings_validate_all_or_nothing_through_the_crate_rules() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();

    let mut partial = vault_settings();
    partial.keyvault.private_jwk = None;
    assert_eq!(
        commands::save_settings_service(&state, partial)
            .await
            .unwrap_err(),
        "invalid_settings"
    );

    let mut broken_url = vault_settings();
    broken_url.keyvault.url = Some("http://vault.example.com".to_owned());
    assert_eq!(
        commands::save_settings_service(&state, broken_url)
            .await
            .unwrap_err(),
        "invalid_settings"
    );

    let complete = vault_settings();
    let saved = commands::save_settings_service(&state, complete)
        .await
        .unwrap();
    let reloaded = commands::get_settings_service(&state).await.unwrap();
    assert_eq!(saved, reloaded);
}

#[tokio::test]
async fn keyvault_commands_refuse_before_anything_is_configured() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();

    let error = clipboard_history_app::keyvault::list_service(&state)
        .await
        .unwrap_err();
    assert_eq!(error, "keyvault_not_configured");
}

#[tokio::test]
async fn copying_a_key_opens_it_suppresses_capture_and_never_returns_it() {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use rsa::rand_core::{OsRng, RngCore};
    use rsa::{Oaep, RsaPrivateKey};
    use sha2::Sha256;

    let mut rng = OsRng;
    let holder = RsaPrivateKey::new(&mut rng, 2048).unwrap();

    // Seal exactly the way the vault's browser does.
    let mut aes_key = [0u8; 32];
    rng.fill_bytes(&mut aes_key);
    let mut iv = [0u8; 12];
    rng.fill_bytes(&mut iv);
    let wrapped = holder
        .to_public_key()
        .encrypt(&mut rng, Oaep::new::<Sha256>(), &aes_key)
        .unwrap();
    use aes_gcm::aead::{Aead, KeyInit};
    let cipher = aes_gcm::Aes256Gcm::new_from_slice(&aes_key).unwrap();
    let sealed = cipher
        .encrypt((&iv).into(), b"sklejka-klucz".as_ref())
        .unwrap();
    let envelope_body = serde_json::json!({
        "slug": "openai",
        "name": "OpenAI",
        "ciphertext": serde_json::json!({
            "v": 1,
            "encKey": STANDARD.encode(wrapped),
            "iv": STANDARD.encode(iv),
            "ct": STANDARD.encode(sealed),
        })
        .to_string(),
    })
    .to_string();

    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();
    // The real application manages a MonitorControl alongside the state; the
    // copy path arms it, so the test app needs one too.
    let app = tauri::test::mock_builder()
        .manage(state)
        .manage(clipboard_history_app::monitor::MonitorControl::new())
        .invoke_handler(commands::invoke_handler())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();

    let config = clipboard_keyvault::KeyvaultConfig {
        base_url: "https://trustworthy-eagle-783.convex.site".to_owned(),
        token: "kv_AbCdEf0123456789-_".to_owned(),
        private_jwk: jwk_json_for(&holder),
    };

    let outcome = clipboard_history_app::keyvault::copy_secret_with(
        app.handle(),
        CannedVault::with(vec![CannedVault::json(200, &envelope_body)]),
        &config,
        "openai",
        |text| {
            // The write is the one place that can prove the ordering this
            // module promises: the suppression must already cover "now" by
            // the time any clipboard write is attempted, so a fetched key can
            // never be recorded as a fresh capture.
            assert!(
                app.state::<clipboard_history_app::monitor::MonitorControl>()
                    .suppression_deadline_active(
                        clipboard_history_app::commands::current_time_ms()
                    )
            );
            assert_eq!(text, "sklejka-klucz");
            Err("clipboard_unavailable".to_owned())
        },
    )
    .await;

    // The refusal is the write's, not the vault's: anything keyvault_* here
    // would mean the envelope or the key failed to open; reaching the write
    // means the plaintext existed and was consumed.
    assert_eq!(outcome.unwrap_err(), "clipboard_unavailable");

    // Nothing in this command's result or the store carries the key: the
    // store never saw a capture for it at all.
    let stats = commands::get_storage_stats_service(&app.state::<AppState>())
        .await
        .unwrap();
    assert_eq!(stats.event_count, 0);
}

#[test]
fn the_vault_read_throttle_spaces_requests() {
    assert_eq!(clipboard_history_app::keyvault::throttle(10_000), Ok(()));
    assert_eq!(
        clipboard_history_app::keyvault::throttle(10_500),
        Err("keyvault_rate_limited".to_owned())
    );
    // A backward wall-clock step passes instead of freezing every read
    // until real time catches up to the stale mark.
    assert_eq!(clipboard_history_app::keyvault::throttle(9_000), Ok(()));
    assert_eq!(
        clipboard_history_app::keyvault::throttle(9_050),
        Err("keyvault_rate_limited".to_owned())
    );
    assert_eq!(clipboard_history_app::keyvault::throttle(20_000), Ok(()));
}

#[test]
fn keyvault_settings_debug_redacts_the_token_and_the_key() {
    let settings = vault_settings();
    let token = settings.keyvault.token.clone().unwrap_or_default();
    let private_jwk = settings.keyvault.private_jwk.clone().unwrap_or_default();
    let rendered = format!("{settings:?}");

    assert!(!rendered.contains(&token), "the token must not print");
    assert!(!rendered.contains(&private_jwk), "the key must not print");
    assert!(rendered.contains("<redacted>"));
    // The address is not a secret; a Debug with nothing in it is useless.
    assert!(rendered.contains("trustworthy-eagle-783"));
}
/// An application whose `/Applications` entry is a firmlink into the cryptex
/// — Safari is the notable one — canonicalizes outside the everyday roots.
/// It must still catalogue, launch-validate, and answer an icon; gated on the
/// bundle existing, so machines without it (and they are rare) skip quietly
/// rather than fail loudly about hardware they never had.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_cryptex_firmlink_application_catalogues_and_answers_an_icon() {
    const SAFARI: &str = "/Applications/Safari.app";
    if !std::path::Path::new(SAFARI).exists() {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open_data_dir(directory.path()).unwrap();

    let catalog = state.launcher.catalog(0);
    assert!(
        catalog.iter().any(|app| app.name == "Safari"),
        "the cryptex firmlink must list Safari with the everyday roots"
    );
    assert!(
        clipboard_launcher::validate_launch_path(SAFARI, state.launcher.roots()).is_ok(),
        "launch validation must accept the firmlink"
    );

    let icon = commands::get_app_icon_service(&state, SAFARI.to_owned())
        .await
        .unwrap()
        .expect("Safari owes an icon like any other bundle");
    assert_eq!(icon.mime_type, "image/png");
    let raw = base64_decode_prefix(&icon.base64);
    assert_eq!(&raw[..8], b"\x89PNG\r\n\x1a\n", "the icon is not a PNG");
    let decoded = image::load_from_memory(&raw).expect("a decodable PNG");
    assert_eq!(
        (decoded.width(), decoded.height()),
        (64, 64),
        "the icon ships at the row's size"
    );
}
