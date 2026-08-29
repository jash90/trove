use std::{collections::HashSet, path::PathBuf};

use base64::Engine;
use clipboard_core::ContentFlags;
use clipboard_import::{ImportAnalysis, ImportError, ImportProgress, ImportRunHandle};
use clipboard_search::{HistoryPage, SearchError, SearchRequest, SearchStoreExt};
use clipboard_store::{CasError, StoreError, StoreHandle};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use tauri::Manager;
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
const MAX_THUMBNAIL_RAW_BYTES: usize = MAX_THUMBNAIL_BASE64_BYTES / 4 * 3;

#[macro_export]
macro_rules! clipboard_history_command_registry {
    ($consumer:ident) => {
        $consumer! {
            search_history => $crate::commands::search_history,
            get_preview => $crate::commands::get_preview,
            set_pinned => $crate::commands::set_pinned,
            delete_event => $crate::commands::delete_event,
            copy_event => $crate::commands::copy_event,
            analyze_import => $crate::commands::analyze_import,
            start_import => $crate::commands::start_import,
            discard_import_analysis => $crate::commands::discard_import_analysis,
            get_import_status => $crate::commands::get_import_status,
            get_settings => $crate::commands::get_settings,
            save_settings => $crate::commands::save_settings,
            get_storage_stats => $crate::commands::get_storage_stats,
            get_thumbnail => $crate::commands::get_thumbnail,
            reveal_source => $crate::commands::reveal_source,
            open_settings_window => $crate::commands::open_settings_window,
            export_history => $crate::commands::export_history,
            get_link_preview => $crate::commands::get_link_preview,
            keyvault_list => $crate::commands::keyvault_list,
            keyvault_copy_secret => $crate::commands::keyvault_copy_secret,
        }
    };
}

pub fn invoke_handler<R: tauri::Runtime>()
-> impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static {
    macro_rules! generate_command_handler {
        ($( $name:ident => $command:path, )*) => {
            tauri::generate_handler![$($command),*]
        };
    }

    crate::clipboard_history_command_registry!(generate_command_handler)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewDto {
    pub event_id: i64,
    pub kind: String,
    pub mime_type: String,
    pub text: Option<String>,
    pub byte_size: u64,
    pub source_app_name: Option<String>,
    /// Where the entry came from, decoded for display. Present only for entries
    /// whose source recorded a location; the importer never read its bytes.
    pub source_path: Option<String>,
    /// Whether that location still resolves to something on this machine.
    pub source_exists: bool,
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
    /// Whether a link entry's page may be contacted for its title and icon.
    ///
    /// The one setting that decides whether this application uses the network
    /// at all. Defaulted through serde so a settings row written before it
    /// existed still reads.
    #[serde(default = "default_link_previews")]
    pub link_previews: bool,
    /// Where the keyvault pane looks, when it is configured at all.
    ///
    /// Defaulted through serde so a settings row written before it existed
    /// still reads. All three fields or none: a half-configured vault is a
    /// settings error, not a surprise at copy time.
    #[serde(default)]
    pub keyvault: KeyvaultSettingsDto,
}

/// The keyvault connection: base URL, bearer token, and the device-side
/// private key that opens what the vault seals.
///
/// Stored as this application's settings, which is a plaintext row in the
/// local database — the same trust boundary the database itself already sits
/// on. Never echoed into a log, an error, or a Debug print anywhere.
#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeyvaultSettingsDto {
    pub url: Option<String>,
    pub token: Option<String>,
    pub private_jwk: Option<String>,
}

/// Debug by hand, redacting the two fields that must never reach a print:
/// the crate layer refuses `Debug` on the same values for the same reason,
/// and a derived impl here would be the one-step-away version of that leak.
impl std::fmt::Debug for KeyvaultSettingsDto {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KeyvaultSettingsDto")
            .field("url", &self.url)
            .field("token", &"<redacted>")
            .field("private_jwk", &"<redacted>")
            .finish()
    }
}

impl KeyvaultSettingsDto {
    /// The three fields, present and non-blank, or nothing.
    pub(crate) fn resolved(&self) -> Option<(String, String, String)> {
        let field = |value: &Option<String>| {
            value
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        Some((
            field(&self.url)?,
            field(&self.token)?,
            field(&self.private_jwk)?,
        ))
    }

    /// Whether every field is absent or blank: the unconfigured state.
    fn is_blank(&self) -> bool {
        let blank =
            |value: &Option<String>| value.as_deref().map(str::trim).is_none_or(str::is_empty);
        blank(&self.url) && blank(&self.token) && blank(&self.private_jwk)
    }
}

/// Link previews are on by default, which was a deliberate choice: it is the
/// only thing here that sends anything anywhere, and the person who asked for
/// it wanted it on. Turning it off makes the application silent again.
fn default_link_previews() -> bool {
    true
}

impl Default for AppSettingsDto {
    fn default() -> Self {
        Self {
            schema_version: 1,
            hotkey: DEFAULT_HOTKEY.to_owned(),
            autostart: false,
            retention_days: None,
            denylisted_apps: Vec::new(),
            link_previews: default_link_previews(),
            keyvault: KeyvaultSettingsDto::default(),
        }
    }
}

pub async fn search_history_service(
    state: &AppState,
    request: SearchRequest,
) -> Result<HistoryPage, String> {
    let store = state.store.clone();
    run_blocking("search_unavailable", move || {
        store
            .search(request)
            .map_err(|error| search_error_code(&error))
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn search_history(
    state: tauri::State<'_, AppState>,
    request: SearchRequest,
) -> Result<HistoryPage, String> {
    search_history_service(state.inner(), request).await
}

pub async fn get_preview_service(state: &AppState, event_id: i64) -> Result<PreviewDto, String> {
    let store = state.store.clone();
    run_blocking("preview_unavailable", move || {
        get_preview_blocking(&store, event_id)
    })
    .await
}

fn get_preview_blocking(store: &StoreHandle, event_id: i64) -> Result<PreviewDto, String> {
    let metadata = read_primary_metadata(store, event_id, "preview_unavailable")?;
    let missing_payload = metadata.missing_payload;
    let text = if missing_payload {
        None
    } else if is_text_preview(&metadata.kind, &metadata.mime_type, &metadata.format_id) {
        let bytes = read_primary_bytes(
            store,
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
    let source_path = read_source_path(store, event_id)?;
    let source_exists = source_path
        .as_deref()
        .is_some_and(|path| std::fs::metadata(path).is_ok());
    Ok(PreviewDto {
        event_id,
        kind: metadata.kind,
        mime_type: metadata.mime_type,
        text,
        byte_size: metadata.byte_size,
        source_app_name: metadata.source_app_name,
        source_path,
        source_exists,
    })
}

/// Longest source reference kept for display. The schema already caps a
/// representation, but this read must be bounded on its own terms.
const MAX_SOURCE_REFERENCE_BYTES: usize = 4 * 1024;

/// Reads the `text/uri-list` reference a file or image entry carries beside its
/// primary representation, and returns it as a local path when it names one.
fn read_source_path(store: &StoreHandle, event_id: i64) -> Result<Option<String>, String> {
    let reference = store
        .with_reader(|connection| {
            connection
                .query_row(
                    "SELECT rp.inline_payload
                     FROM event_representation er
                     JOIN raw_payload rp ON rp.raw_payload_id = er.raw_payload_id
                     WHERE er.event_id = ?1
                       AND er.format_id = 'text/uri-list'
                       AND er.ordinal > 0
                       AND rp.storage_kind = 'inline'
                       AND length(rp.inline_payload) <= ?2",
                    rusqlite::params![event_id, MAX_SOURCE_REFERENCE_BYTES as i64],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()
        })
        .map_err(|error| store_error_code(&error, "preview_unavailable"))?;
    let Some(reference) = reference else {
        return Ok(None);
    };
    let reference = String::from_utf8(reference).map_err(|_| "preview_unavailable".to_owned())?;
    Ok(file_uri_to_path(&reference))
}

/// Turns a `file:` URI back into a local path. Any other scheme names something
/// this application will not open, so it yields nothing rather than a path the
/// interface would wrongly offer to reveal.
fn file_uri_to_path(reference: &str) -> Option<String> {
    let encoded = reference.strip_prefix("file://")?;
    if !encoded.starts_with('/') {
        return None;
    }
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            let text = std::str::from_utf8(hex).ok()?;
            decoded.push(u8::from_str_radix(text, 16).ok()?);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    let path = String::from_utf8(decoded).ok()?;
    // A NUL byte would truncate the path once it reaches the operating system.
    (!path.contains('\0')).then_some(path)
}

pub async fn reveal_source_service(state: &AppState, event_id: i64) -> Result<(), String> {
    let store = state.store.clone();
    run_blocking("source_unavailable", move || {
        let Some(path) = read_source_path(&store, event_id)? else {
            return Err("source_unavailable".to_owned());
        };
        if std::fs::metadata(&path).is_err() {
            return Err("source_missing".to_owned());
        }
        reveal_in_file_manager(&path)
    })
    .await
}

/// Opens the containing folder and selects the entry. The path is passed as a
/// single argument, never through a shell, and only after it was proven to
/// exist, so a crafted export cannot turn this into command execution.
fn reveal_in_file_manager(path: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/bin/open")
            .arg("-R")
            .arg(path)
            .status()
            .map_err(|_| "reveal_failed".to_owned())
            .and_then(|status| {
                if status.success() {
                    Ok(())
                } else {
                    Err("reveal_failed".to_owned())
                }
            })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err("reveal_unsupported".to_owned())
    }
}

pub async fn set_pinned_service(
    state: &AppState,
    event_id: i64,
    pinned: bool,
) -> Result<(), String> {
    state
        .store
        .set_pinned(event_id, pinned)
        .await
        .map_err(|error| store_error_code(&error, "history_write_failed"))
}

pub async fn delete_event_service(state: &AppState, event_id: i64) -> Result<(), String> {
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
pub async fn reveal_source(state: tauri::State<'_, AppState>, event_id: i64) -> Result<(), String> {
    reveal_source_service(state.inner(), event_id).await
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
pub async fn copy_event<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    event_id: i64,
    plain_text: bool,
    paste: bool,
) -> Result<CopyResultDto, String> {
    let text = prepare_copy_text_service(state.inner(), event_id, plain_text).await?;
    // Putting an entry back changes the clipboard, and the monitor would
    // otherwise record our own paste as a fresh copy. Arm the suppression
    // before the write so the change cannot land first.
    if let Some(control) = app.try_state::<crate::monitor::MonitorControl>() {
        control.suppress_next_change(current_time_ms());
    }
    app.clipboard()
        .write_text(text)
        .map_err(|_| "clipboard_unavailable".to_owned())?;
    let mode = if paste {
        paste_into_previous_window(&app)
    } else {
        CopyModeDto::Copied
    };
    Ok(CopyResultDto { mode, plain_text })
}

/// Sends the entry to the window the user was in before the palette appeared.
///
/// The entry is already on the clipboard, so every refusal below still leaves
/// the user able to paste by hand. Saying which refusal happened is the point:
/// a silent no-op is indistinguishable from a broken application.
fn paste_into_previous_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> CopyModeDto {
    let target = app
        .try_state::<crate::hotkey::PasteTarget>()
        .and_then(|target| target.take());
    match paste_readiness(target) {
        Some(mode) => mode,
        None => {
            // Hide first: the keystroke goes to the window that had focus, and
            // leaving the palette in front would send it into a dead end.
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.hide();
            }
            let pid = target.unwrap_or_default();
            if paste_keystroke(pid) {
                CopyModeDto::Pasted
            } else {
                CopyModeDto::CopiedOnlyPlatformLimit
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn paste_readiness(target: Option<i32>) -> Option<CopyModeDto> {
    use platform_macos::PasteReadiness;

    match platform_macos::readiness(platform_macos::is_trusted(), target) {
        PasteReadiness::Ready => None,
        PasteReadiness::PermissionRequired => Some(CopyModeDto::CopiedOnlyPermissionRequired),
        PasteReadiness::TargetLost => Some(CopyModeDto::CopiedOnlyTargetLost),
        PasteReadiness::PlatformLimit => Some(CopyModeDto::CopiedOnlyPlatformLimit),
    }
}

#[cfg(not(target_os = "macos"))]
fn paste_readiness(_target: Option<i32>) -> Option<CopyModeDto> {
    Some(CopyModeDto::CopiedOnlyPlatformLimit)
}

#[cfg(target_os = "macos")]
fn paste_keystroke(pid: i32) -> bool {
    platform_macos::post_paste_to(pid)
}

#[cfg(not(target_os = "macos"))]
fn paste_keystroke(_pid: i32) -> bool {
    false
}

pub async fn prepare_copy_text_service(
    state: &AppState,
    event_id: i64,
    plain_text: bool,
) -> Result<String, String> {
    let store = state.store.clone();
    run_blocking("copy_unavailable", move || {
        prepare_copy_text_blocking(&store, event_id, plain_text)
    })
    .await
}

fn prepare_copy_text_blocking(
    store: &StoreHandle,
    event_id: i64,
    _plain_text: bool,
) -> Result<String, String> {
    let metadata = read_primary_metadata(store, event_id, "copy_unavailable")?;
    if !metadata.missing_payload
        && !is_text_preview(&metadata.kind, &metadata.mime_type, &metadata.format_id)
    {
        return Err("copy_format_unavailable".to_owned());
    }
    let bytes = read_primary_bytes(
        store,
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
    password: Option<String>,
) -> Result<ImportAnalysis, String> {
    let importer = state.importer.clone();
    // The password is turned into a secret here and dropped with this task.
    // An analysis keeps parsed records rather than a way back to the file, so
    // starting the import it describes never needs the password again.
    let secret = password.map(clipboard_import::RayconfigSecret::new);
    tokio::task::spawn_blocking(move || importer.analyze_with_password(path, secret.as_ref()))
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
    let importer = state.importer.clone();
    run_blocking("import_status_unavailable", move || {
        importer
            .status(run_id)
            .map_err(|error| import_error_code(&error, "import_status_unavailable"))
    })
    .await
}

/// Brings the settings window up.
///
/// The palette asks rather than draws: settings are their own window now, so
/// the list stays readable while they are open.
#[tauri::command(rename_all = "camelCase")]
pub fn open_settings_window<R: tauri::Runtime>(app: tauri::AppHandle<R>) {
    crate::hotkey::show_settings(&app);
}

pub async fn export_history_service(
    state: &AppState,
    directory: PathBuf,
) -> Result<crate::export::ExportSummary, String> {
    let store = state.store.clone();
    run_blocking("export_unavailable", move || {
        let destination = crate::export::prepare_destination(&directory)?;
        crate::export::export_supercmd(&store, &destination)
    })
    .await
}

pub async fn get_link_preview_service(
    state: &AppState,
    event_id: i64,
) -> Result<Option<crate::links::LinkPreviewDto>, String> {
    crate::links::link_preview_service::<tauri::Wry>(None, state, event_id).await
}

/// What a link entry points at, and what its page said about itself.
#[tauri::command(rename_all = "camelCase")]
pub async fn get_link_preview<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    event_id: i64,
) -> Result<Option<crate::links::LinkPreviewDto>, String> {
    crate::links::link_preview_service(Some(app), state.inner(), event_id).await
}

/// Writes the whole history into a directory the user chose.
///
/// The bytes are assembled and written here rather than in the interface: a
/// history of any size would otherwise cross the bridge record by record, and
/// the shell already has the store and the blobs open.
#[tauri::command(rename_all = "camelCase")]
pub async fn export_history(
    state: tauri::State<'_, AppState>,
    directory: PathBuf,
) -> Result<crate::export::ExportSummary, String> {
    export_history_service(state.inner(), directory).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn analyze_import(
    state: tauri::State<'_, AppState>,
    path: PathBuf,
    password: Option<String>,
) -> Result<ImportAnalysis, String> {
    analyze_import_service(state.inner(), path, password).await
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
    let store = state.store.clone();
    run_blocking("storage_stats_unavailable", move || {
        get_storage_stats_blocking(&store)
    })
    .await
}

fn get_storage_stats_blocking(store: &StoreHandle) -> Result<StorageStatsDto, String> {
    let (content_count, event_count, blob_bytes) = store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT
                   (SELECT count(*) FROM content),
                   (SELECT count(*) FROM history_event),
                   (SELECT COALESCE(sum(stored_byte_size), 0) FROM raw_payload
                     WHERE storage_kind = 'cas')
                     + (SELECT COALESCE(sum(byte_size), 0) FROM artifact)",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
        })
        .map_err(|error| store_error_code(&error, "storage_stats_unavailable"))?;
    let database_bytes = std::fs::metadata(store.config().database_path())
        .map_err(|_| "storage_stats_unavailable".to_owned())?
        .len();
    Ok(StorageStatsDto {
        content_count: valid_count(content_count)?,
        event_count: valid_count(event_count)?,
        database_bytes,
        blob_bytes: valid_count(blob_bytes)?,
    })
}

pub async fn get_thumbnail_service(
    state: &AppState,
    event_id: i64,
) -> Result<Option<ThumbnailDto>, String> {
    let store = state.store.clone();
    let stored = run_blocking("thumbnail_unavailable", {
        let store = store.clone();
        move || get_thumbnail_blocking(&store, event_id)
    })
    .await?;
    if let Some(stored) = stored {
        return Ok(Some(stored));
    }

    // Nothing stored yet. Render it from the image itself and keep it, so an
    // image copied before this existed gets a preview the first time anyone
    // looks at it, and only that once.
    let rendered = run_blocking("thumbnail_unavailable", {
        let store = store.clone();
        move || render_thumbnail_blocking(&store, event_id)
    })
    .await?;
    let Some(rendered) = rendered else {
        return Ok(None);
    };
    // A failed write costs the next viewer one more render; it must not cost
    // this one their preview.
    let _ = store
        .store_thumbnail(
            rendered.content_id,
            rendered.bytes.clone(),
            current_time_ms(),
        )
        .await;
    encode_thumbnail(&rendered.bytes).map(Some)
}

/// A thumbnail rendered on demand, with the row it belongs to.
struct RenderedThumbnail {
    content_id: i64,
    bytes: Vec<u8>,
}

/// Renders a thumbnail for an image entry that has one to render.
///
/// Returns nothing rather than an error for the ordinary cases — the entry is
/// not an image, or its payload is gone — because neither is a fault the user
/// can act on and both mean the same thing on screen.
fn render_thumbnail_blocking(
    store: &StoreHandle,
    event_id: i64,
) -> Result<Option<RenderedThumbnail>, String> {
    let content_id = store
        .with_reader(|connection| {
            connection
                .query_row(
                    "SELECT he.content_id FROM history_event he WHERE he.event_id = ?1",
                    [event_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
        })
        .map_err(|error| store_error_code(&error, "thumbnail_unavailable"))?
        .ok_or_else(|| "history_event_not_found".to_owned())?;

    let metadata = read_primary_metadata(store, event_id, "thumbnail_unavailable")?;
    if metadata.kind != "image" || metadata.missing_payload {
        return Ok(None);
    }
    let Some(bytes) = read_primary_bytes(
        store,
        &metadata,
        clipboard_images::MAX_IMAGE_INPUT_BYTES,
        "thumbnail_too_large",
        "thumbnail_unavailable",
    )?
    else {
        return Ok(None);
    };
    let thumbnail =
        clipboard_images::make_thumbnail(&bytes, clipboard_images::MAX_THUMBNAIL_DIMENSION)
            .map_err(|_| "thumbnail_unavailable".to_owned())?;
    Ok(Some(RenderedThumbnail {
        content_id,
        bytes: thumbnail,
    }))
}

/// Wraps raw PNG bytes in the response shape, refusing anything over the cap.
fn encode_thumbnail(bytes: &[u8]) -> Result<ThumbnailDto, String> {
    if bytes.len() > MAX_THUMBNAIL_RAW_BYTES {
        return Err("thumbnail_too_large".to_owned());
    }
    let base64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    if base64.len() > MAX_THUMBNAIL_BASE64_BYTES {
        return Err("thumbnail_too_large".to_owned());
    }
    Ok(ThumbnailDto {
        mime_type: "image/png".to_owned(),
        base64,
    })
}

fn get_thumbnail_blocking(
    store: &StoreHandle,
    event_id: i64,
) -> Result<Option<ThumbnailDto>, String> {
    let artifact = store
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
    let cas = store
        .cas_store()
        .map_err(|error| store_error_code(&error, "thumbnail_unavailable"))?;
    let bytes = cas
        .read_bounded(&relpath, byte_size as u64, MAX_THUMBNAIL_RAW_BYTES)
        .map_err(|error| cas_error_code(&error, "thumbnail_unavailable"))?;
    encode_thumbnail(&bytes).map(Some)
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

/// Reads the applications the user asked to be left out of the history.
///
/// An unreadable settings row yields an empty list, which records everything.
/// The alternative — treating a read failure as "deny all" — would silently
/// stop recording, and a clipboard manager that quietly keeps nothing is worse
/// than one that keeps too much.
pub fn current_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

/// How many days of history the user asked to keep, if they asked at all.
pub fn retention_days(store: &StoreHandle) -> Option<u16> {
    get_settings_blocking(store)
        .ok()
        .and_then(|settings| settings.retention_days)
}

/// Whether link pages may be contacted.
///
/// An unreadable settings row means no fetching: silence is the safe direction
/// for the only network path in the application.
pub fn link_previews_enabled(store: &StoreHandle) -> bool {
    get_settings_blocking(store)
        .map(|settings| settings.link_previews)
        .unwrap_or(false)
}

pub fn denylisted_apps(store: &StoreHandle) -> Vec<String> {
    get_settings_blocking(store)
        .map(|settings| settings.denylisted_apps)
        .unwrap_or_default()
}

pub async fn get_settings_service(state: &AppState) -> Result<AppSettingsDto, String> {
    let store = state.store.clone();
    run_blocking("settings_unavailable", move || {
        get_settings_blocking(&store)
    })
    .await
}

pub(crate) fn get_settings_blocking(store: &StoreHandle) -> Result<AppSettingsDto, String> {
    let settings = match store.get_setting(APP_SETTINGS_KEY) {
        Ok(Some(value_json)) => {
            serde_json::from_str(&value_json).map_err(|_| "invalid_settings".to_owned())?
        }
        Ok(None) => AppSettingsDto::default(),
        Err(error) => return Err(store_error_code(&error, "settings_unavailable")),
    };
    validate_settings(&settings)?;
    Ok(settings)
}

/// Persists settings and applies the parts that live outside the database.
///
/// The shortcut is registered with the system, not stored in a row, so saving
/// has to rebind it. Doing that only on the next launch means the settings
/// screen shows one shortcut while another one answers.
pub async fn save_settings_with_app<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &AppState,
    settings: AppSettingsDto,
) -> Result<AppSettingsDto, String> {
    let stored = save_settings_service(state, settings).await?;
    if let (Some(active), Some(next)) = (
        app.try_state::<crate::hotkey::ActiveShortcut>(),
        crate::hotkey::parse_shortcut(&stored.hotkey),
    ) {
        let previous = active.get();
        // A shortcut another application already holds leaves the previous one
        // answering, which is better than leaving none.
        if crate::hotkey::rebind(app, previous, next).is_ok() {
            active.set(next);
        }
    }
    Ok(stored)
}

pub async fn save_settings_service(
    state: &AppState,
    mut settings: AppSettingsDto,
) -> Result<AppSettingsDto, String> {
    // A cleared keyvault field arrives as an empty string from the form;
    // storing it as absent keeps the row saying what it means.
    let keyvault = KeyvaultSettingsDto {
        url: normalize_keyvault_field(&settings.keyvault.url),
        token: normalize_keyvault_field(&settings.keyvault.token),
        private_jwk: normalize_keyvault_field(&settings.keyvault.private_jwk),
    };
    settings.keyvault = keyvault;
    validate_settings(&settings)?;
    let value_json = serde_json::to_string(&settings).map_err(|_| "invalid_settings".to_owned())?;
    state
        .store
        .save_setting(APP_SETTINGS_KEY, &value_json)
        .await
        .map_err(|error| store_error_code(&error, "settings_unavailable"))?;
    Ok(settings)
}

fn normalize_keyvault_field(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn get_settings(state: tauri::State<'_, AppState>) -> Result<AppSettingsDto, String> {
    get_settings_service(state.inner()).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn keyvault_list(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<clipboard_keyvault::SecretRef>, String> {
    crate::keyvault::throttle(current_time_ms())?;
    crate::keyvault::list_service(state.inner()).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn keyvault_copy_secret<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    slug: String,
) -> Result<(), String> {
    crate::keyvault::throttle(current_time_ms())?;
    let store = state.store.clone();
    let settings = run_blocking("settings_unavailable", move || {
        get_settings_blocking(&store)
    })
    .await?;
    let config = crate::keyvault::config_from(&settings)?;
    let transport = clipboard_keyvault::ReqwestSecretTransport::new(&config)
        .map_err(|error| error.to_string())?;
    crate::keyvault::copy_secret_service(&app, transport, &config, &slug).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn save_settings<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    settings: AppSettingsDto,
) -> Result<AppSettingsDto, String> {
    save_settings_with_app(&app, state.inner(), settings).await
}

pub(crate) async fn run_blocking<T>(
    unavailable_code: &'static str,
    operation: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| unavailable_code.to_owned())?
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
    store: &StoreHandle,
    event_id: i64,
    unavailable_code: &str,
) -> Result<PrimaryMetadata, String> {
    if event_id <= 0 {
        return Err("history_event_not_found".to_owned());
    }
    let raw = store
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

/// Reads one entry's primary payload for the exporter.
///
/// Returns nothing for the ordinary reasons — the payload is gone, or too
/// large to write out — because an export skips those rather than failing.
pub(crate) fn export_payload_bytes(
    store: &StoreHandle,
    event_id: i64,
    maximum: usize,
) -> Option<Vec<u8>> {
    let metadata = read_primary_metadata(store, event_id, "export_unavailable").ok()?;
    read_primary_bytes(
        store,
        &metadata,
        maximum,
        "export_too_large",
        "export_unavailable",
    )
    .ok()
    .flatten()
}

fn read_primary_bytes(
    store: &StoreHandle,
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
            let bytes = store
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
            let cas = store
                .cas_store()
                .map_err(|error| store_error_code(&error, unavailable_code))?;
            let bytes = cas
                .read_bounded(relpath, original_size as u64, maximum)
                .map_err(|error| cas_error_code(&error, unavailable_code))?;
            if bytes.len() != original_size {
                return Err(unavailable_code.to_owned());
            }
            Ok(Some(bytes))
        }
        _ => Err(unavailable_code.to_owned()),
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
        StoreError::HistoryEventNotFound => "history_event_not_found".to_owned(),
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
    // All three fields or none: a half-configured vault is a settings error,
    // not a surprise at copy time.
    let keyvault_is_valid = match settings.keyvault.resolved() {
        None => settings.keyvault.is_blank(),
        Some((url, token, private_jwk)) => clipboard_keyvault::KeyvaultConfig {
            base_url: url,
            token,
            private_jwk,
        }
        .validate()
        .is_ok(),
    };
    if settings.schema_version != 1
        || !retention_is_valid
        || !hotkey_is_valid
        || !denylist_is_valid
        || !keyvault_is_valid
    {
        return Err("invalid_settings".to_owned());
    }
    Ok(())
}
