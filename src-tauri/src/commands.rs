use std::{collections::HashSet, path::PathBuf};

use base64::Engine;
use clipboard_core::ContentFlags;
use clipboard_import::{ImportAnalysis, ImportError, ImportProgress, ImportRunHandle};
use clipboard_search::{HistoryPage, SearchError, SearchRequest, SearchStoreExt};
use clipboard_store::{CasError, StoreError};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use tauri_plugin_clipboard_manager::ClipboardExt;
use uuid::Uuid;

use crate::state::AppState;

const APP_SETTINGS_KEY: &str = "app";
const DEFAULT_HOTKEY: &str = "CommandOrControl+Shift+V";
const MAX_HOTKEY_BYTES: usize = 128;
const MAX_DENYLISTED_APPS: usize = 200;
const MAX_DENYLISTED_APP_BYTES: usize = 256;
const MAX_PREVIEW_PAYLOAD_BYTES: usize = 1024 * 1024;
const MAX_COPY_TEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_THUMBNAIL_BASE64_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewDto {
    pub event_id: i64,
    pub kind: String,
    pub mime_type: String,
    pub text: Option<String>,
    pub byte_size: u64,
    pub source_app_name: Option<String>,
    pub missing_payload: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyModeDto {
    Copied,
    Pasted,
    CopiedOnlyPermissionRequired,
    CopiedOnlyTargetLost,
    CopiedOnlyPlatformLimit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyResultDto {
    pub mode: CopyModeDto,
    pub plain_text: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailDto {
    pub mime_type: String,
    pub base64: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageStatsDto {
    pub content_count: u64,
    pub event_count: u64,
    pub missing_payload_count: u64,
    pub database_bytes: u64,
    pub blob_bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppSettingsDto {
    pub schema_version: u32,
    pub hotkey: String,
    pub autostart: bool,
    pub retention_days: Option<u16>,
    pub denylisted_apps: Vec<String>,
}

impl Default for AppSettingsDto {
    fn default() -> Self {
        Self {
            schema_version: 1,
            hotkey: DEFAULT_HOTKEY.to_owned(),
            autostart: false,
            retention_days: None,
            denylisted_apps: Vec::new(),
        }
    }
}

pub async fn search_history_service(
    state: &AppState,
    request: SearchRequest,
) -> Result<HistoryPage, String> {
    state
        .store
        .search(request)
        .map_err(|error| search_error_code(&error))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn search_history(
    state: tauri::State<'_, AppState>,
    request: SearchRequest,
) -> Result<HistoryPage, String> {
    search_history_service(state.inner(), request).await
}

pub async fn get_preview_service(state: &AppState, event_id: i64) -> Result<PreviewDto, String> {
    let metadata = read_primary_metadata(state, event_id, "preview_unavailable")?;
    let missing_payload = metadata.missing_payload;
    let text = if missing_payload {
        None
    } else if is_text_preview(&metadata.kind, &metadata.mime_type, &metadata.format_id) {
        let bytes = read_primary_bytes(
            state,
            &metadata,
            MAX_PREVIEW_PAYLOAD_BYTES,
            "preview_too_large",
            "preview_unavailable",
        )?
        .ok_or_else(|| "missing_payload".to_owned())?;
        Some(String::from_utf8(bytes).map_err(|_| "invalid_preview_encoding".to_owned())?)
    } else {
        None
    };
    Ok(PreviewDto {
        event_id,
        kind: metadata.kind,
        mime_type: metadata.mime_type,
        text,
        byte_size: metadata.byte_size,
        source_app_name: metadata.source_app_name,
        missing_payload,
    })
}

pub async fn set_pinned_service(
    state: &AppState,
    event_id: i64,
    pinned: bool,
) -> Result<(), String> {
    require_event(state, event_id)?;
    state
        .store
        .set_pinned(event_id, pinned)
        .await
        .map_err(|error| store_error_code(&error, "history_write_failed"))
}

pub async fn delete_event_service(state: &AppState, event_id: i64) -> Result<(), String> {
    require_event(state, event_id)?;
    state
        .store
        .delete_event(event_id)
        .await
        .map_err(|error| store_error_code(&error, "history_write_failed"))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn get_preview(
    state: tauri::State<'_, AppState>,
    event_id: i64,
) -> Result<PreviewDto, String> {
    get_preview_service(state.inner(), event_id).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn set_pinned(
    state: tauri::State<'_, AppState>,
    event_id: i64,
    pinned: bool,
) -> Result<(), String> {
    set_pinned_service(state.inner(), event_id, pinned).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn delete_event(state: tauri::State<'_, AppState>, event_id: i64) -> Result<(), String> {
    delete_event_service(state.inner(), event_id).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn copy_event(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    event_id: i64,
    plain_text: bool,
) -> Result<CopyResultDto, String> {
    let text = prepare_copy_text_service(state.inner(), event_id, plain_text).await?;
    app.clipboard()
        .write_text(text)
        .map_err(|_| "clipboard_unavailable".to_owned())?;
    Ok(CopyResultDto {
        mode: CopyModeDto::Copied,
        plain_text,
    })
}

pub async fn prepare_copy_text_service(
    state: &AppState,
    event_id: i64,
    plain_text: bool,
) -> Result<String, String> {
    let metadata = read_primary_metadata(state, event_id, "copy_unavailable")?;
    if !plain_text && !is_text_preview(&metadata.kind, &metadata.mime_type, &metadata.format_id) {
        return Err("copy_format_unavailable".to_owned());
    }
    let bytes = read_primary_bytes(
        state,
        &metadata,
        MAX_COPY_TEXT_BYTES,
        "copy_too_large",
        "copy_unavailable",
    )?
    .ok_or_else(|| "missing_payload".to_owned())?;
    String::from_utf8(bytes).map_err(|_| "copy_format_unavailable".to_owned())
}

pub async fn analyze_import_service(
    state: &AppState,
    path: PathBuf,
) -> Result<ImportAnalysis, String> {
    let importer = state.importer.clone();
    tokio::task::spawn_blocking(move || importer.analyze(path))
        .await
        .map_err(|_| "import_analysis_failed".to_owned())?
        .map_err(|error| import_error_code(&error, "import_analysis_failed"))
}

pub async fn start_import_service(
    state: &AppState,
    analysis_id: &str,
) -> Result<ImportRunHandle, String> {
    let analysis_id = Uuid::parse_str(analysis_id).map_err(|_| "invalid_analysis_id".to_owned())?;
    state
        .importer
        .begin(analysis_id)
        .await
        .map_err(|error| import_error_code(&error, "import_start_failed"))
}

pub async fn discard_import_analysis_service(
    state: &AppState,
    analysis_id: &str,
) -> Result<(), String> {
    let analysis_id = Uuid::parse_str(analysis_id).map_err(|_| "invalid_analysis_id".to_owned())?;
    state
        .importer
        .discard_analysis(analysis_id)
        .map_err(|error| import_error_code(&error, "import_discard_failed"))
}

pub async fn get_import_status_service(
    state: &AppState,
    run_id: &str,
) -> Result<ImportProgress, String> {
    let run_id = Uuid::parse_str(run_id).map_err(|_| "invalid_run_id".to_owned())?;
    state
        .importer
        .status(run_id)
        .map_err(|error| import_error_code(&error, "import_status_unavailable"))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn analyze_import(
    state: tauri::State<'_, AppState>,
    path: PathBuf,
) -> Result<ImportAnalysis, String> {
    analyze_import_service(state.inner(), path).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn start_import(
    state: tauri::State<'_, AppState>,
    analysis_id: String,
) -> Result<ImportRunHandle, String> {
    start_import_service(state.inner(), &analysis_id).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn discard_import_analysis(
    state: tauri::State<'_, AppState>,
    analysis_id: String,
) -> Result<(), String> {
    discard_import_analysis_service(state.inner(), &analysis_id).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn get_import_status(
    state: tauri::State<'_, AppState>,
    run_id: String,
) -> Result<ImportProgress, String> {
    get_import_status_service(state.inner(), &run_id).await
}

pub async fn get_storage_stats_service(state: &AppState) -> Result<StorageStatsDto, String> {
    let (content_count, event_count, missing_payload_count, blob_bytes) = state
        .store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT
                   (SELECT count(*) FROM content),
                   (SELECT count(*) FROM history_event),
                   (SELECT count(*) FROM content WHERE (flags & ?1) != 0),
                   (SELECT COALESCE(sum(stored_byte_size), 0) FROM raw_payload
                     WHERE storage_kind = 'cas')
                     + (SELECT COALESCE(sum(byte_size), 0) FROM artifact)",
                [i64::from(ContentFlags::MISSING_PAYLOAD.bits())],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
        })
        .map_err(|error| store_error_code(&error, "storage_stats_unavailable"))?;
    let database_bytes = std::fs::metadata(state.store.config().database_path())
        .map_err(|_| "storage_stats_unavailable".to_owned())?
        .len();
    Ok(StorageStatsDto {
        content_count: valid_count(content_count)?,
        event_count: valid_count(event_count)?,
        missing_payload_count: valid_count(missing_payload_count)?,
        database_bytes,
        blob_bytes: valid_count(blob_bytes)?,
    })
}

pub async fn get_thumbnail_service(
    state: &AppState,
    event_id: i64,
) -> Result<Option<ThumbnailDto>, String> {
    let artifact = state
        .store
        .with_reader(|connection| {
            connection
                .query_row(
                    "SELECT a.blob_relpath, a.byte_size
                     FROM history_event he
                     LEFT JOIN artifact a
                       ON a.content_id = he.content_id AND a.artifact_kind = 'thumbnail'
                     WHERE he.event_id = ?1",
                    [event_id],
                    |row| {
                        Ok((
                            row.get::<_, Option<String>>(0)?,
                            row.get::<_, Option<i64>>(1)?,
                        ))
                    },
                )
                .optional()
        })
        .map_err(|error| store_error_code(&error, "thumbnail_unavailable"))?
        .ok_or_else(|| "history_event_not_found".to_owned())?;
    let (Some(relpath), Some(byte_size)) = artifact else {
        return Ok(None);
    };
    let byte_size = usize::try_from(byte_size).map_err(|_| "thumbnail_unavailable".to_owned())?;
    let encoded_size = byte_size
        .checked_add(2)
        .and_then(|size| size.checked_div(3))
        .and_then(|groups| groups.checked_mul(4))
        .ok_or_else(|| "thumbnail_too_large".to_owned())?;
    if encoded_size > MAX_THUMBNAIL_BASE64_BYTES {
        return Err("thumbnail_too_large".to_owned());
    }
    let cas = state
        .store
        .cas_store()
        .map_err(|error| store_error_code(&error, "thumbnail_unavailable"))?;
    cas.verify(&relpath, byte_size as u64)
        .map_err(|error| cas_error_code(&error, "thumbnail_unavailable"))?;
    let bytes = cas
        .read(&relpath)
        .map_err(|error| cas_error_code(&error, "thumbnail_unavailable"))?;
    let base64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    if base64.len() > MAX_THUMBNAIL_BASE64_BYTES {
        return Err("thumbnail_too_large".to_owned());
    }
    Ok(Some(ThumbnailDto {
        mime_type: "image/png".to_owned(),
        base64,
    }))
}

#[tauri::command(rename_all = "camelCase")]
pub async fn get_storage_stats(
    state: tauri::State<'_, AppState>,
) -> Result<StorageStatsDto, String> {
    get_storage_stats_service(state.inner()).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn get_thumbnail(
    state: tauri::State<'_, AppState>,
    event_id: i64,
) -> Result<Option<ThumbnailDto>, String> {
    get_thumbnail_service(state.inner(), event_id).await
}

pub async fn get_settings_service(state: &AppState) -> Result<AppSettingsDto, String> {
    let settings = match state.store.get_setting(APP_SETTINGS_KEY) {
        Ok(Some(value_json)) => {
            serde_json::from_str(&value_json).map_err(|_| "invalid_settings".to_owned())?
        }
        Ok(None) => AppSettingsDto::default(),
        Err(error) => return Err(store_error_code(&error, "settings_unavailable")),
    };
    validate_settings(&settings)?;
    Ok(settings)
}

pub async fn save_settings_service(
    state: &AppState,
    settings: AppSettingsDto,
) -> Result<AppSettingsDto, String> {
    validate_settings(&settings)?;
    let value_json = serde_json::to_string(&settings).map_err(|_| "invalid_settings".to_owned())?;
    state
        .store
        .save_setting(APP_SETTINGS_KEY, &value_json)
        .await
        .map_err(|error| store_error_code(&error, "settings_unavailable"))?;
    Ok(settings)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn get_settings(state: tauri::State<'_, AppState>) -> Result<AppSettingsDto, String> {
    get_settings_service(state.inner()).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn save_settings(
    state: tauri::State<'_, AppState>,
    settings: AppSettingsDto,
) -> Result<AppSettingsDto, String> {
    save_settings_service(state.inner(), settings).await
}

struct PrimaryMetadata {
    kind: String,
    mime_type: String,
    byte_size: u64,
    source_app_name: Option<String>,
    format_id: Option<String>,
    raw_payload_id: Option<i64>,
    missing_payload: bool,
    storage_kind: Option<String>,
    blob_relpath: Option<String>,
    original_byte_size: Option<usize>,
    stored_byte_size: Option<usize>,
}

fn read_primary_metadata(
    state: &AppState,
    event_id: i64,
    unavailable_code: &str,
) -> Result<PrimaryMetadata, String> {
    if event_id <= 0 {
        return Err("history_event_not_found".to_owned());
    }
    let raw = state
        .store
        .with_reader(|connection| {
            connection
                .query_row(
                    "SELECT c.kind, c.primary_mime, c.byte_size, c.flags, he.source_app_name,
                            er.format_id, er.raw_payload_id, er.missing_ref IS NOT NULL,
                            rp.storage_kind, rp.blob_relpath,
                            rp.original_byte_size, rp.stored_byte_size
                     FROM history_event he
                     JOIN content c ON c.content_id = he.content_id
                     LEFT JOIN event_representation er
                       ON er.event_id = he.event_id AND er.ordinal = 0
                     LEFT JOIN raw_payload rp ON rp.raw_payload_id = er.raw_payload_id
                     WHERE he.event_id = ?1",
                    [event_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, Option<String>>(4)?,
                            row.get::<_, Option<String>>(5)?,
                            row.get::<_, Option<i64>>(6)?,
                            row.get::<_, bool>(7)?,
                            row.get::<_, Option<String>>(8)?,
                            row.get::<_, Option<String>>(9)?,
                            row.get::<_, Option<i64>>(10)?,
                            row.get::<_, Option<i64>>(11)?,
                        ))
                    },
                )
                .optional()
        })
        .map_err(|error| store_error_code(&error, unavailable_code))?
        .ok_or_else(|| "history_event_not_found".to_owned())?;
    if !matches!(
        raw.0.as_str(),
        "text" | "link" | "image" | "file" | "color" | "code" | "html"
    ) {
        return Err(unavailable_code.to_owned());
    }
    let byte_size = u64::try_from(raw.2).map_err(|_| unavailable_code.to_owned())?;
    let original_byte_size = raw
        .10
        .map(|value| usize::try_from(value).map_err(|_| unavailable_code.to_owned()))
        .transpose()?;
    let stored_byte_size = raw
        .11
        .map(|value| usize::try_from(value).map_err(|_| unavailable_code.to_owned()))
        .transpose()?;
    Ok(PrimaryMetadata {
        kind: raw.0,
        mime_type: raw.1,
        byte_size,
        source_app_name: raw.4,
        format_id: raw.5,
        raw_payload_id: raw.6,
        missing_payload: raw.7 || (raw.3 & i64::from(ContentFlags::MISSING_PAYLOAD.bits())) != 0,
        storage_kind: raw.8,
        blob_relpath: raw.9,
        original_byte_size,
        stored_byte_size,
    })
}

fn read_primary_bytes(
    state: &AppState,
    metadata: &PrimaryMetadata,
    maximum: usize,
    too_large_code: &str,
    unavailable_code: &str,
) -> Result<Option<Vec<u8>>, String> {
    if metadata.missing_payload {
        return Ok(None);
    }
    let raw_payload_id = metadata
        .raw_payload_id
        .ok_or_else(|| unavailable_code.to_owned())?;
    let original_size = metadata
        .original_byte_size
        .ok_or_else(|| unavailable_code.to_owned())?;
    if original_size > maximum {
        return Err(too_large_code.to_owned());
    }
    match metadata.storage_kind.as_deref() {
        Some("inline") | Some("inline_zstd") => {
            let stored_size = metadata
                .stored_byte_size
                .ok_or_else(|| unavailable_code.to_owned())?;
            if stored_size > maximum.saturating_add(1024) {
                return Err(too_large_code.to_owned());
            }
            let bytes = state
                .store
                .with_reader(|connection| {
                    connection.query_row(
                        "SELECT inline_payload FROM raw_payload
                         WHERE raw_payload_id = ?1
                           AND typeof(inline_payload) = 'blob'
                           AND length(inline_payload) = ?2",
                        rusqlite::params![
                            raw_payload_id,
                            i64::try_from(stored_size).unwrap_or(i64::MAX)
                        ],
                        |row| row.get::<_, Vec<u8>>(0),
                    )
                })
                .map_err(|error| store_error_code(&error, unavailable_code))?;
            if metadata.storage_kind.as_deref() == Some("inline_zstd") {
                let decoded = zstd::bulk::decompress(&bytes, original_size)
                    .map_err(|_| unavailable_code.to_owned())?;
                if decoded.len() != original_size {
                    return Err(unavailable_code.to_owned());
                }
                Ok(Some(decoded))
            } else if bytes.len() == original_size {
                Ok(Some(bytes))
            } else {
                Err(unavailable_code.to_owned())
            }
        }
        Some("cas") => {
            let relpath = metadata
                .blob_relpath
                .as_deref()
                .ok_or_else(|| unavailable_code.to_owned())?;
            let cas = state
                .store
                .cas_store()
                .map_err(|error| store_error_code(&error, unavailable_code))?;
            cas.verify(relpath, original_size as u64)
                .map_err(|error| cas_error_code(&error, unavailable_code))?;
            let bytes = cas
                .read(relpath)
                .map_err(|error| cas_error_code(&error, unavailable_code))?;
            if bytes.len() != original_size {
                return Err(unavailable_code.to_owned());
            }
            Ok(Some(bytes))
        }
        _ => Err(unavailable_code.to_owned()),
    }
}

fn require_event(state: &AppState, event_id: i64) -> Result<(), String> {
    let exists = state
        .store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM history_event WHERE event_id = ?1)",
                [event_id],
                |row| row.get::<_, bool>(0),
            )
        })
        .map_err(|error| store_error_code(&error, "history_unavailable"))?;
    if exists {
        Ok(())
    } else {
        Err("history_event_not_found".to_owned())
    }
}

fn is_text_preview(kind: &str, mime_type: &str, format_id: &Option<String>) -> bool {
    matches!(kind, "text" | "link" | "color" | "code" | "html")
        || mime_type.starts_with("text/")
        || format_id
            .as_deref()
            .is_some_and(|format| format.contains("file-url"))
}

fn import_error_code(error: &ImportError, fallback: &str) -> String {
    match error {
        ImportError::Export { reason, .. }
        | ImportError::Record { reason, .. }
        | ImportError::Service { reason } => {
            if reason.is_empty() {
                fallback.to_owned()
            } else {
                (*reason).to_owned()
            }
        }
    }
}

fn valid_count(value: i64) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| "storage_stats_unavailable".to_owned())
}

fn search_error_code(error: &SearchError) -> String {
    match error {
        SearchError::Store(source) => store_error_code(source, error.code()),
        _ => error.code().to_owned(),
    }
}

fn store_error_code(error: &StoreError, fallback: &str) -> String {
    match error {
        StoreError::PrivateStorageUnavailable
        | StoreError::Cas(CasError::PrivateStorageUnavailable) => {
            "private_storage_unavailable".to_owned()
        }
        _ => fallback.to_owned(),
    }
}

fn cas_error_code(error: &CasError, fallback: &str) -> String {
    match error {
        CasError::PrivateStorageUnavailable => "private_storage_unavailable".to_owned(),
        _ => fallback.to_owned(),
    }
}

fn validate_settings(settings: &AppSettingsDto) -> Result<(), String> {
    let retention_is_valid = settings
        .retention_days
        .is_none_or(|days| (1..=3650).contains(&days));
    let hotkey_is_valid = !settings.hotkey.is_empty()
        && settings.hotkey.len() <= MAX_HOTKEY_BYTES
        && settings.hotkey.trim() == settings.hotkey
        && !settings.hotkey.chars().any(char::is_control);
    let mut unique_apps = HashSet::with_capacity(settings.denylisted_apps.len());
    let denylist_is_valid = settings.denylisted_apps.len() <= MAX_DENYLISTED_APPS
        && settings.denylisted_apps.iter().all(|app| {
            !app.is_empty()
                && app.len() <= MAX_DENYLISTED_APP_BYTES
                && app.trim() == app
                && !app.chars().any(char::is_control)
                && unique_apps.insert(app.as_str())
        });
    if settings.schema_version != 1 || !retention_is_valid || !hotkey_is_valid || !denylist_is_valid
    {
        return Err("invalid_settings".to_owned());
    }
    Ok(())
}
