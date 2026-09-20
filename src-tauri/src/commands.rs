use std::{collections::HashSet, path::PathBuf};

use base64::Engine;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};
use tauri_plugin_clipboard_manager::ClipboardExt;
use trove_core::ContentFlags;
use trove_import::{ImportAnalysis, ImportError, ImportProgress, ImportRunHandle};
use trove_launcher::AppBundle;
use trove_search::{HistoryPage, SearchError, SearchRequest, SearchStoreExt};
use trove_store::{CasError, StoreError, StoreHandle};
use uuid::Uuid;

use crate::state::AppState;

use crate::state::AppIconDto;

const APP_SETTINGS_KEY: &str = "app";
const MAX_HOTKEY_BYTES: usize = 128;
const MAX_DENYLISTED_APPS: usize = 200;
const MAX_DENYLISTED_APP_BYTES: usize = 256;
const MAX_PREVIEW_PAYLOAD_BYTES: usize = 1024 * 1024;
const MAX_COPY_TEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_THUMBNAIL_BASE64_BYTES: usize = 256 * 1024;
const MAX_THUMBNAIL_RAW_BYTES: usize = MAX_THUMBNAIL_BASE64_BYTES / 4 * 3;

#[macro_export]
macro_rules! trove_command_registry {
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
            list_apps => $crate::commands::list_apps,
            launch_app => $crate::commands::launch_app,
            get_app_icon => $crate::commands::get_app_icon,
            open_settings_window => $crate::commands::open_settings_window,
            open_accessibility_settings_window => $crate::commands::open_accessibility_settings_window,
            open_keyboard_settings_window => $crate::commands::open_keyboard_settings_window,
            get_shortcut_status => $crate::commands::get_shortcut_status,
            free_summoning_shortcut => $crate::commands::free_summoning_shortcut,
            restore_system_shortcut => $crate::commands::restore_system_shortcut,
            export_history => $crate::commands::export_history,
            get_link_preview => $crate::commands::get_link_preview,
            keyvault_list => $crate::commands::keyvault_list,
            keyvault_copy_secret => $crate::commands::keyvault_copy_secret,
            keyvault_pair_start => $crate::commands::keyvault_pair_start,
            keyvault_pair_poll => $crate::commands::keyvault_pair_poll,
            keyvault_pair_cancel => $crate::commands::keyvault_pair_cancel,
            keyvault_identity => $crate::commands::keyvault_identity,
            keyvault_reset_pairing => $crate::commands::keyvault_reset_pairing,
            chat_send => $crate::commands::chat_send,
            chat_stop => $crate::commands::chat_stop,
            chat_list_models => $crate::commands::chat_list_models,
            save_generated_file => $crate::commands::save_generated_file,
            open_external_url => $crate::commands::open_external_url,
            copy_chat_text => $crate::commands::copy_chat_text,
            get_chat_settings => $crate::commands::get_chat_settings,
            save_chat_settings => $crate::commands::save_chat_settings,
            open_chat_window => $crate::commands::open_chat_window,
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

    crate::trove_command_registry!(generate_command_handler)
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
    /// The one setting that decides whether this application uses the
    /// network at all. Defaulted through serde so a settings row written
    /// before it existed still reads.
    #[serde(default = "default_link_previews")]
    pub link_previews: bool,
    /// Whether the palette splits into History and Applications modes.
    ///
    /// On, the palette opens on the history and `Tab` flips to the
    /// applications; off, one list answers both at once. Defaulted through
    /// serde so a settings row written before it existed still reads.
    #[serde(default = "default_palette_modes")]
    pub palette_modes: bool,
    /// Whether the application shows a Dock tile.
    ///
    /// Off by default, which is what `LSUIElement` in `Info.plist` already
    /// says: this is a menu bar application whose window spends its life
    /// hidden. The setting exists because that is a preference and not a
    /// law — a Dock tile also buys a Cmd-Tab entry, and some people would
    /// rather have both. Defaulted through serde so a settings row written
    /// before it existed still reads.
    #[serde(default = "default_dock_icon")]
    pub dock_icon: bool,
    /// Where the keyvault pane looks, when it is configured at all.
    ///
    /// Defaulted through serde so a settings row written before it existed
    /// still reads. All three fields or none: a half-configured vault is a
    /// settings error, not a surprise at copy time.
    #[serde(default)]
    pub keyvault: KeyvaultSettingsDto,
}

/// What stands between the user and the shortcut they configured.
///
/// Two separate failures, kept separate because they look identical from the
/// keyboard and have different fixes. `registered` false means the system
/// refused the binding outright. `held_by_system` means the binding took and a
/// system shortcut still eats every press before this application sees it —
/// which is the ordinary state of Cmd+Space on a machine nobody has changed.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutStatusDto {
    pub hotkey: String,
    pub registered: bool,
    pub held_by_system: bool,
    /// The system shortcuts this application turned off to free the chord.
    ///
    /// Kept so the change can be given back: taking a system-wide shortcut away
    /// from inside an application and offering no way to return it would be bad
    /// manners.
    pub released_ids: Vec<i64>,
}

/// How a request to change the system's shortcut table actually went.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ShortcutReleaseDto {
    /// Nothing held the chord; nothing needed doing.
    AlreadyFree,
    /// Done, and live now.
    Applied,
    /// Written, but the system did not reload its table. It takes effect at the
    /// next login, and saying so is better than reporting a success the user
    /// will not observe.
    NeedsLogout,
    /// Nothing was written. The manual route is all that is left.
    Refused,
}

/// Optional overrides for the vault connection.
///
/// The connection itself — URL, token and the private key that opens what the
/// vault seals — comes from the device's shared identity file, so no key is
/// stored in this database at all. These fields exist only to point one
/// install somewhere else, at a local Convex backend for instance, without
/// editing the identity every other consumer reads.
///
/// `private_jwk` is retained for one reason: rows written before the identity
/// file existed carry it, and `deny_unknown_fields` would refuse to parse them
/// if the field vanished. It is ignored on read and blanked on the next save,
/// so an old row's key stops sitting in the database as soon as anything
/// touches the settings.
#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeyvaultSettingsDto {
    /// The vault address, remembered so the pane can show it and Connect can start from it.
    ///
    /// Remembered, not obeyed: reads go to the paired identity. It used to override that, which
    /// is how a web address typed here sent reads to a host that answers 405 and reported it as
    /// unreachable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Written by versions in which this overrode the paired identity. Ignored on read and
    /// dropped on the next save: a stale one outranked a working pairing and made a pairing that
    /// had just succeeded report a refused token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Written by versions that kept the key here. Skipped on serialize, so
    /// the first save after upgrading drops it from the row outright rather
    /// than leaving a `null` where a key used to be; `default` is what lets
    /// those shorter rows parse on the way back in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    /// The remembered vault address, if one was ever typed.
    pub(crate) fn address(&self) -> Option<String> {
        self.url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    }
}

/// Link previews are on by default, which was a deliberate choice: it is the
/// only thing here that sends anything anywhere, and the person who asked for
/// it wanted it on. Turning it off makes the application silent again.
fn default_link_previews() -> bool {
    true
}

/// Palette modes are on by default: the palette opens on the history it
/// exists to keep, and applications are one `Tab` away. Turning the setting
/// off restores the single combined list.
fn default_palette_modes() -> bool {
    true
}

/// The Dock tile is off by default. Every version before this one had none,
/// so anything else would hand a tile to every existing install on upgrade.
fn default_dock_icon() -> bool {
    false
}

impl Default for AppSettingsDto {
    fn default() -> Self {
        Self {
            schema_version: 1,
            hotkey: crate::hotkey::DEFAULT_HOTKEY.to_owned(),
            autostart: false,
            retention_days: None,
            denylisted_apps: Vec::new(),
            link_previews: default_link_previews(),
            palette_modes: default_palette_modes(),
            dock_icon: default_dock_icon(),
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
    // The list shows one row per distinct content, so removing the row the
    // user sees removes the whole group behind it, not one occurrence.
    let store = state.store.clone();
    let content_id = run_blocking("history_write_failed", move || {
        store
            .with_reader(|connection| {
                connection
                    .query_row(
                        "SELECT content_id FROM history_event WHERE event_id = ?1",
                        [event_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()
            })
            .map_err(|error| store_error_code(&error, "history_read_failed"))
    })
    .await?
    .ok_or_else(|| "history_event_not_found".to_owned())?;
    state
        .store
        .delete_events_for_content(content_id)
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

/// The application catalog for the palette's launcher mode. Returns the whole
/// list at once — a few hundred entries, tens of kilobytes — because the
/// palette filters as the user types, and an IPC round trip per keystroke
/// would reintroduce exactly the latency the history list debounces away.
///
/// The first call in a run has nothing cached and pays the scan on the
/// spot. Every later call answers from the cache immediately — the palette
/// never waits and never blanks — while a background rescan runs beside it;
/// when that scan finds a catalog different from the one before it,
/// `on_refreshed` says so and the palette refetches. A scan that changed
/// nothing says nothing, which is what keeps the refetch chain from
/// feeding itself.
pub async fn list_apps_service(
    state: &AppState,
    on_refreshed: impl FnOnce(bool) + Send + 'static,
) -> Result<Vec<AppBundle>, String> {
    if let Some(cached) = state.launcher.snapshot() {
        state.launcher.refresh_in_background(on_refreshed);
        return Ok(cached);
    }
    let launcher = state.launcher.clone();
    let catalog = run_blocking("apps_unavailable", move || Ok(launcher.refresh())).await?;
    Ok(catalog)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn list_apps<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<AppBundle>, String> {
    let emitter = move |changed: bool| {
        if changed {
            let _ = app.emit(crate::state::APPS_CATALOG_CHANGED_EVENT, ());
        }
    };
    list_apps_service(state.inner(), emitter).await
}

pub async fn launch_app_service(state: &AppState, path: String) -> Result<(), String> {
    let roots = state.launcher.roots().to_vec();
    run_blocking("apps_unavailable", move || {
        // The string arrives from the webview; the scanner's own validation
        // decides whether it names a bundle this application ever listed.
        let canonical = trove_launcher::validate_launch_path(&path, &roots)
            .map_err(|error| error.code().to_owned())?;
        launch_application(&canonical)
    })
    .await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn launch_app<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<(), String> {
    launch_app_service(state.inner(), path).await?;
    // The application the user picked is starting; leaving the palette in
    // front of it would put a window between them and what they asked for.
    // Hidden, not closed — closing is the Quit item's job and only its.
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
    Ok(())
}

/// The side an application icon is rendered to. Double the ~31 CSS pixels
/// the row slot shows, so a retina screen gets real pixels instead of a
/// stretched guess.
const APP_ICON_DIMENSION: u32 = 64;

/// Renders one application's icon as a small PNG, or `None` when there is
/// nothing to draw. Validation refusals surface as the launcher's stable
/// codes — an icon request for a path the scanner never listed is a
/// question about a path, not about an icon.
pub async fn get_app_icon_service(
    state: &AppState,
    path: String,
) -> Result<Option<AppIconDto>, String> {
    let launcher = state.launcher.clone();
    let roots = state.launcher.roots().to_vec();
    run_blocking("icon_unavailable", move || {
        let canonical = trove_launcher::validate_launch_path(&path, &roots)
            .map_err(|error| error.code().to_owned())?;
        let canonical = canonical.to_str().ok_or("launch_invalid")?.to_owned();
        // No icon to draw is an answer, not a failure — the row falls back
        // to its glyph and carries on.
        let Some(icon) = launcher.icon(&canonical, || render_app_icon(&canonical)) else {
            return Ok(None);
        };
        Ok(Some(icon))
    })
    .await
}

fn render_app_icon(canonical: &str) -> Option<AppIconDto> {
    #[cfg(target_os = "macos")]
    {
        // AppKit rasterises the icon at the row's size and hands back a PNG;
        // the resize below decodes that PNG — bytes we just encoded — never
        // Apple's icon-services artwork, which no third-party decoder should
        // be trusted to read back. It is a near-identity pass on an image
        // already at the target, kept because the DTO's shape is ours to
        // guarantee rather than AppKit's to promise.
        let apple_png =
            platform_macos::application_icon_png(canonical, APP_ICON_DIMENSION as usize)?;
        let png = trove_images::make_thumbnail(&apple_png, APP_ICON_DIMENSION).ok()?;
        let base64 = base64::engine::general_purpose::STANDARD.encode(&png);
        Some(AppIconDto {
            mime_type: "image/png".to_owned(),
            base64,
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        // No workspace to ask; the row falls back to its glyph rather than
        // pretending every application shares one picture.
        let _ = canonical;
        None
    }
}

#[tauri::command(rename_all = "camelCase")]
pub async fn get_app_icon(
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<Option<AppIconDto>, String> {
    get_app_icon_service(state.inner(), path).await
}

/// Starts an application bundle. The validated path is passed as a single
/// argument, never through a shell, exactly like revealing a source — a
/// crafted webview message cannot turn this into command execution, because
/// by the time it runs the path has already been proven to be a bundle
/// directory under a scanned root.
fn launch_application(path: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/bin/open")
            .arg("-a")
            .arg(path)
            .status()
            .map_err(|_| "launch_failed".to_owned())
            .and_then(|status| {
                if status.success() {
                    Ok(())
                } else {
                    Err("launch_failed".to_owned())
                }
            })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err("launch_unsupported".to_owned())
    }
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
    // Kept in the IPC contract; plainness now follows the payload — an
    // image is never a plain-text copy, a text entry always is.
    _plain_text: bool,
    paste: bool,
) -> Result<CopyResultDto, String> {
    let payload = prepare_copy_payload_service(state.inner(), event_id).await?;
    // Putting an entry back changes the clipboard, and the monitor would
    // otherwise record our own paste as a fresh copy. Arm the suppression
    // before the write so the change cannot land first.
    if let Some(control) = app.try_state::<crate::monitor::MonitorControl>() {
        control.suppress_next_change(current_time_ms());
    }
    let is_plain_text = matches!(payload, CopyPayload::Text(_));
    match payload {
        CopyPayload::Text(text) => {
            app.clipboard()
                .write_text(text)
                .map_err(|_| "clipboard_unavailable".to_owned())?;
        }
        CopyPayload::Image(bytes) => {
            // The clipboard takes pixels, not the PNG/TIFF file the history
            // keeps: decoded off the async runtime, because a screenshot
            // pasted back is real decoding work.
            let image = run_blocking("copy_unavailable", move || {
                let (rgba, width, height) =
                    trove_images::decode_rgba(&bytes).map_err(|_| "copy_format_unavailable")?;
                Ok::<_, String>(tauri::image::Image::new_owned(rgba, width, height))
            })
            .await?;
            app.clipboard()
                .write_image(&image)
                .map_err(|_| "clipboard_unavailable".to_owned())?;
        }
    }
    let mode = if paste {
        paste_into_previous_window(&app).await
    } else {
        CopyModeDto::Copied
    };
    Ok(CopyResultDto {
        mode,
        plain_text: is_plain_text,
    })
}

/// How long the target is given to come forward before the keystroke is posted.
///
/// Activation is a request to the window server, not a function call that has
/// finished when it returns: post immediately and the keystroke arrives while
/// the target still has no key window, which routes it nowhere. Long enough to
/// win that race, short enough that nobody sees a pause.
const ACTIVATION_SETTLE: std::time::Duration = std::time::Duration::from_millis(90);

/// Sends the entry to the window the user was in before the palette appeared.
///
/// The entry is already on the clipboard, so every refusal below still leaves
/// the user able to paste by hand. Saying which refusal happened is the point:
/// a silent no-op is indistinguishable from a broken application.
async fn paste_into_previous_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> CopyModeDto {
    let target = app
        .try_state::<crate::hotkey::PasteTarget>()
        .and_then(|target| target.current());
    // Unconditionally, and before the readiness check rather than after it: the
    // entry is on the clipboard whatever happens next, and a palette left
    // standing in front of the window the user meant to paste into is a worse
    // answer than a refusal they can read once they come back.
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
    match paste_readiness(target) {
        Some(mode) => {
            if mode == CopyModeDto::CopiedOnlyPermissionRequired {
                ask_for_paste_permission(app);
            }
            mode
        }
        None => {
            let pid = target.unwrap_or_default();
            // Hiding our window does not hand activation back — an application
            // with no window on screen stays the active one — and a process
            // that does not own the key window has nowhere to route Command-V.
            // So the target is asked for the front explicitly and given a
            // moment to take it.
            //
            // A refusal here is the same fact `readiness` refuses a missing pid
            // for: the application we meant to paste into has quit or cannot be
            // activated, and firing Command-V anyway would type into whatever
            // happened to be in front instead. Better to say the target is gone
            // than to report a paste that landed somewhere nobody asked for.
            if !activate_paste_target(pid) {
                return CopyModeDto::CopiedOnlyTargetLost;
            }
            tokio::time::sleep(ACTIVATION_SETTLE).await;
            if paste_keystroke(pid) {
                CopyModeDto::Pasted
            } else {
                CopyModeDto::CopiedOnlyPlatformLimit
            }
        }
    }
}

/// Asks for Accessibility permission, at most once per run.
///
/// The system dialog only appears while it has no answer on record for this
/// application, and never twice in one launch — so on the machine that has
/// already answered "no", the ask the user would see is nothing at all. That is
/// why a refused prompt falls through to opening the settings pane, which works
/// whatever the system has recorded.
///
/// Once per run rather than once per refusal: bringing System Settings forward
/// on every Enter would be nagging, and after the first time the palette's own
/// message carries a button to the same place.
fn ask_for_paste_permission<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let Some(prompt) = app.try_state::<crate::hotkey::PastePrompt>() else {
        return;
    };
    if !prompt.claim_first_ask() {
        return;
    }
    if !request_paste_trust() {
        open_accessibility_settings();
    }
}

#[cfg(target_os = "macos")]
fn request_paste_trust() -> bool {
    platform_macos::request_trust()
}

#[cfg(not(target_os = "macos"))]
fn request_paste_trust() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn open_accessibility_settings() -> bool {
    platform_macos::open_accessibility_settings()
}

#[cfg(not(target_os = "macos"))]
fn open_accessibility_settings() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn activate_paste_target(pid: i32) -> bool {
    platform_macos::activate_pid(pid)
}

#[cfg(not(target_os = "macos"))]
fn activate_paste_target(_pid: i32) -> bool {
    false
}

/// Which system shortcuts hold the chord the palette is bound to.
///
/// Empty on every platform but macOS, and empty on macOS whenever the chord is
/// free. Only the system's own table is visible here: another application
/// holding the same chord is not recorded anywhere this can read, which is why
/// nothing derives "the shortcut works" from an empty answer.
#[cfg(target_os = "macos")]
fn system_holders(shortcut: &tauri_plugin_global_shortcut::Shortcut) -> Vec<i64> {
    let Some(chord) = symbolic_chord(shortcut) else {
        return Vec::new();
    };
    platform_macos::holders_of(chord)
}

#[cfg(not(target_os = "macos"))]
fn system_holders(_shortcut: &tauri_plugin_global_shortcut::Shortcut) -> Vec<i64> {
    Vec::new()
}

/// The chord as the system's own shortcut table spells it.
///
/// Only the combinations that table can express are worth translating; for
/// anything else there is nothing to collide with there, and answering `None`
/// says exactly that.
#[cfg(target_os = "macos")]
fn symbolic_chord(
    shortcut: &tauri_plugin_global_shortcut::Shortcut,
) -> Option<platform_macos::Chord> {
    use platform_macos::symbolic_hotkeys::{
        MODIFIER_COMMAND, MODIFIER_CONTROL, MODIFIER_OPTION, MODIFIER_SHIFT,
    };
    use tauri_plugin_global_shortcut::{Code, Modifiers};

    let key_code = match shortcut.key {
        Code::Space => 49,
        _ => return None,
    };
    let mut modifiers = 0;
    for (flag, mask) in [
        (Modifiers::SUPER, MODIFIER_COMMAND),
        (Modifiers::CONTROL, MODIFIER_CONTROL),
        (Modifiers::ALT, MODIFIER_OPTION),
        (Modifiers::SHIFT, MODIFIER_SHIFT),
    ] {
        if shortcut.mods.contains(flag) {
            modifiers |= mask;
        }
    }
    Some(platform_macos::Chord {
        key_code,
        modifiers,
    })
}

#[cfg(target_os = "macos")]
fn release_system_holders(ids: &[i64]) -> platform_macos::SetOutcome {
    platform_macos::symbolic_hotkeys::set_enabled(ids, false)
}

#[cfg(not(target_os = "macos"))]
fn release_system_holders(_ids: &[i64]) -> ShortcutReleaseDto {
    ShortcutReleaseDto::Refused
}

#[cfg(target_os = "macos")]
fn restore_system_holders(ids: &[i64]) -> platform_macos::SetOutcome {
    platform_macos::symbolic_hotkeys::set_enabled(ids, true)
}

#[cfg(not(target_os = "macos"))]
fn restore_system_holders(_ids: &[i64]) -> ShortcutReleaseDto {
    ShortcutReleaseDto::Refused
}

#[cfg(target_os = "macos")]
fn open_keyboard_settings() -> bool {
    platform_macos::symbolic_hotkeys::open_keyboard_shortcut_settings()
}

#[cfg(not(target_os = "macos"))]
fn open_keyboard_settings() -> bool {
    false
}

/// Opens the Accessibility list in System Settings, on request.
///
/// The palette offers this next to the refusal it reports, so the fix is one
/// click from the message explaining why pasting did not happen.
#[tauri::command(rename_all = "camelCase")]
pub fn open_accessibility_settings_window() -> Result<(), String> {
    if open_accessibility_settings() {
        Ok(())
    } else {
        Err("accessibility_settings_unavailable".to_owned())
    }
}

/// What the summoning shortcut is doing, as opposed to what it was asked to do.
#[tauri::command(rename_all = "camelCase")]
pub fn get_shortcut_status(
    active: tauri::State<'_, crate::hotkey::ActiveShortcut>,
    released: tauri::State<'_, crate::hotkey::ReleasedSystemHotkeys>,
) -> ShortcutStatusDto {
    let shortcut = active.get();
    ShortcutStatusDto {
        hotkey: shortcut.into_string(),
        registered: active.is_registered(),
        held_by_system: !system_holders(&shortcut).is_empty(),
        released_ids: released.get(),
    }
}

/// Turns off the system shortcuts standing on the configured chord.
///
/// Deliberate and user-initiated, never automatic: this changes a setting that
/// belongs to the whole machine, not to this application.
#[tauri::command(rename_all = "camelCase")]
pub fn free_summoning_shortcut(
    active: tauri::State<'_, crate::hotkey::ActiveShortcut>,
    released: tauri::State<'_, crate::hotkey::ReleasedSystemHotkeys>,
) -> ShortcutReleaseDto {
    let holders = system_holders(&active.get());
    if holders.is_empty() {
        return ShortcutReleaseDto::AlreadyFree;
    }
    let outcome = release_outcome(release_system_holders(&holders));
    if matches!(
        outcome,
        ShortcutReleaseDto::Applied | ShortcutReleaseDto::NeedsLogout
    ) {
        released.remember(&holders);
    }
    outcome
}

/// Hands the system back what `free_summoning_shortcut` took.
#[tauri::command(rename_all = "camelCase")]
pub fn restore_system_shortcut(
    released: tauri::State<'_, crate::hotkey::ReleasedSystemHotkeys>,
) -> ShortcutReleaseDto {
    let ids = released.get();
    if ids.is_empty() {
        return ShortcutReleaseDto::AlreadyFree;
    }
    let outcome = release_outcome(restore_system_holders(&ids));
    if matches!(
        outcome,
        ShortcutReleaseDto::Applied | ShortcutReleaseDto::NeedsLogout
    ) {
        released.forget();
    }
    outcome
}

/// Opens the Keyboard shortcut list in System Settings, on request.
///
/// The manual route, offered whenever the automatic one is refused.
#[tauri::command(rename_all = "camelCase")]
pub fn open_keyboard_settings_window() -> Result<(), String> {
    if open_keyboard_settings() {
        Ok(())
    } else {
        Err("keyboard_settings_unavailable".to_owned())
    }
}

#[cfg(target_os = "macos")]
fn release_outcome(outcome: platform_macos::SetOutcome) -> ShortcutReleaseDto {
    match outcome {
        platform_macos::SetOutcome::Applied => ShortcutReleaseDto::Applied,
        platform_macos::SetOutcome::NeedsLogout => ShortcutReleaseDto::NeedsLogout,
        platform_macos::SetOutcome::Failed => ShortcutReleaseDto::Refused,
    }
}

#[cfg(not(target_os = "macos"))]
fn release_outcome(outcome: ShortcutReleaseDto) -> ShortcutReleaseDto {
    outcome
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

/// The ceiling on an image put back on the clipboard. Capture refuses
/// payloads past this size on the way in, so the pair of bounds means an
/// entry that was recordable is also pasteable — nothing sits in the
/// history as an exhibit.
const MAX_COPY_IMAGE_BYTES: usize = 32 * 1024 * 1024;

/// What one entry contributes to the clipboard.
#[derive(Debug, Eq, PartialEq)]
pub enum CopyPayload {
    /// UTF-8 text, for every textual representation — including the
    /// `file-url` list a file entry carries as text.
    Text(String),
    /// Encoded image bytes (PNG or TIFF), as captured.
    Image(Vec<u8>),
}

pub async fn prepare_copy_payload_service(
    state: &AppState,
    event_id: i64,
) -> Result<CopyPayload, String> {
    let store = state.store.clone();
    run_blocking("copy_unavailable", move || {
        prepare_copy_payload_blocking(&store, event_id)
    })
    .await
}

fn prepare_copy_payload_blocking(
    store: &StoreHandle,
    event_id: i64,
) -> Result<CopyPayload, String> {
    let metadata = read_primary_metadata(store, event_id, "copy_unavailable")?;
    // An image returns to the clipboard as an image — pixels the receiving
    // application pastes, not a path or a refusal. Only its own bytes back;
    // an image entry whose payload is gone (an import whose source file
    // vanished) has nothing to give and says so.
    if metadata.kind == "image" {
        let bytes = read_primary_bytes(
            store,
            &metadata,
            MAX_COPY_IMAGE_BYTES,
            "copy_too_large",
            "copy_unavailable",
        )?
        .ok_or_else(|| "missing_payload".to_owned())?;
        return Ok(CopyPayload::Image(bytes));
    }
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
    let text = String::from_utf8(bytes).map_err(|_| "copy_format_unavailable".to_owned())?;
    Ok(CopyPayload::Text(text))
}

/// One exchange with the model: answers with the turn id, the answer
/// itself streams as `chat-delta` events and settles with `chat-done` or
/// `chat-error`.
#[tauri::command(rename_all = "camelCase")]
pub async fn chat_send<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, AppState>,
    messages: Vec<crate::chat::ChatMessageDto>,
) -> Result<crate::chat::ChatTurnDto, String> {
    crate::chat::chat_send_service(app, state.inner(), messages).await
}

/// Stops the turn in flight; its stream then settles as done.
#[tauri::command(rename_all = "camelCase")]
pub fn chat_stop(id: String) -> Result<bool, String> {
    crate::chat::chat_stop_service(&id)
}

/// The models the active provider offers, fetched live from its list
/// endpoint — the picker in the window fills from this.
#[tauri::command(rename_all = "camelCase")]
pub async fn chat_list_models(state: tauri::State<'_, AppState>) -> Result<Vec<String>, String> {
    crate::chat::chat_list_models_service(state.inner()).await
}

/// Writes one file the chat produced, to the path the save dialog chose.
#[tauri::command(rename_all = "camelCase")]
pub fn save_generated_file(path: String, contents: String) -> Result<(), String> {
    crate::chat::save_generated_file_blocking(&path, &contents)
}

/// Opens one http(s) link from a markdown answer in the user's browser.
#[tauri::command(rename_all = "camelCase")]
pub fn open_external_url(url: String) -> Result<(), String> {
    crate::chat::open_external_url(&url)
}

/// Puts a copied code block on the clipboard — the same plugin write the
/// history's copy path uses, bounded the same way.
#[tauri::command(rename_all = "camelCase")]
pub fn copy_chat_text<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    text: String,
) -> Result<(), String> {
    if text.len() > 8 * 1024 * 1024 {
        return Err("chat_copy_too_large".to_owned());
    }
    match app.try_state::<tauri_plugin_clipboard_manager::Clipboard<R>>() {
        Some(clipboard) => clipboard
            .write_text(&text)
            .map_err(|_| "clipboard_unavailable".to_owned()),
        None => Err("clipboard_unavailable".to_owned()),
    }
}

#[tauri::command(rename_all = "camelCase")]
pub async fn get_chat_settings(
    state: tauri::State<'_, AppState>,
) -> Result<crate::chat::ChatSettingsDto, String> {
    crate::chat::get_chat_settings_service(state.inner()).await
}

#[tauri::command(rename_all = "camelCase")]
pub async fn save_chat_settings(
    state: tauri::State<'_, AppState>,
    settings: crate::chat::ChatSettingsDto,
) -> Result<crate::chat::ChatSettingsDto, String> {
    crate::chat::save_chat_settings_service(state.inner(), settings).await
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
    let secret = password.map(trove_import::RayconfigSecret::new);
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

/// Brings the chat window up. Its own window for the same reason settings
/// have one: a conversation is read and typed slowly, on top of nothing.
#[tauri::command(rename_all = "camelCase")]
pub fn open_chat_window<R: tauri::Runtime>(app: tauri::AppHandle<R>) {
    crate::hotkey::show_chat(&app);
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
        trove_images::MAX_IMAGE_INPUT_BYTES,
        "thumbnail_too_large",
        "thumbnail_unavailable",
    )?
    else {
        return Ok(None);
    };
    let thumbnail = trove_images::make_thumbnail(&bytes, trove_images::MAX_THUMBNAIL_DIMENSION)
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

/// The shortcut the user chose, for a launch that has to register one.
///
/// Startup used to register a hardcoded default and never look here, so a
/// shortcut changed in settings answered until the application was closed and
/// then quietly reverted, while the settings screen went on showing the value
/// that no longer worked.
pub fn stored_hotkey(store: &StoreHandle) -> Option<String> {
    get_settings_blocking(store)
        .ok()
        .map(|settings| settings.hotkey)
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

/// Whether the user asked for a Dock tile.
///
/// An unreadable settings row means no tile: that is the state every version
/// before this one shipped, so it is the one that cannot surprise anyone.
pub fn dock_icon_enabled(store: &StoreHandle) -> bool {
    get_settings_blocking(store)
        .map(|settings| settings.dock_icon)
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
/// The shortcut is registered with the system and the Dock tile is a property
/// of the running process, not rows in a table, so saving has to apply both.
/// Doing that only on the next launch means the settings screen shows one
/// state while another one is true.
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
        // answering, which is better than leaving none — but the screen must
        // not go on presenting the new one as though it took.
        match crate::hotkey::rebind(app, previous, next) {
            Ok(()) => {
                active.set(next);
                active.set_registered(true);
            }
            Err(_) => active.set_registered(false),
        }
    }
    // The Dock tile lives in the system's activation policy, not in a row, so
    // saving has to apply it — for the same reason the shortcut is rebound
    // here. Applying it only at the next launch would leave the checkbox
    // saying one thing while the Dock said another.
    //
    // Called straight from this command thread on purpose: `AppHandle::
    // set_dock_visibility` posts `Message::SetDockVisibility` to the event
    // loop rather than touching `NSApp` here, so the activation-policy change
    // lands on the main thread without a `run_on_main_thread` wrapper.
    #[cfg(target_os = "macos")]
    let _ = app.set_dock_visibility(stored.dock_icon);
    Ok(stored)
}

pub async fn save_settings_service(
    state: &AppState,
    mut settings: AppSettingsDto,
) -> Result<AppSettingsDto, String> {
    // A cleared keyvault field arrives as an empty string from the form;
    // storing it as absent keeps the row saying what it means.
    // None of it is stored any more: the vault is configured by pairing, which writes the device
    // identity file. Saving is also the migration for a row an older version left values in.
    let keyvault = KeyvaultSettingsDto {
        url: normalize_keyvault_field(&settings.keyvault.url),
        // Never stored: the token comes from pairing, and a pasted one only ever shadowed it.
        token: None,
        // Never stored: the key belongs to the device identity file. Writing
        // None here is also the migration — the first save after upgrading
        // clears whatever an older version left in the row.
        private_jwk: None,
    };
    settings.keyvault = keyvault;
    validate_settings_for_save(&settings)?;
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
pub async fn keyvault_list() -> Result<Vec<trove_keyvault::SecretRef>, String> {
    crate::keyvault::throttle(current_time_ms())?;
    crate::keyvault::list_service().await
}

/// Whether this device is paired, and which vault it knows.
#[tauri::command]
pub async fn keyvault_identity() -> Result<crate::keyvault::IdentityDto, String> {
    Ok(crate::keyvault::identity_service())
}

/// Forgets the pairing so the device can pair again. Local only — see the service.
#[tauri::command]
pub async fn keyvault_reset_pairing(state: tauri::State<'_, AppState>) -> Result<(), String> {
    crate::keyvault::reset_pairing_service(state.inner()).await
}

/// Starts pairing with a vault and returns the fingerprint the interface must show.
///
/// Not throttled like the read path: this is one deliberate click, and the two seconds it spends
/// generating a keypair are their own rate limit.
#[tauri::command]
pub async fn keyvault_pair_start(
    url: String,
) -> Result<crate::keyvault::PairingStartedDto, String> {
    crate::keyvault::pair_start_service(url).await
}

/// Asks whether the browser has approved yet. Called on a timer by the interface.
#[tauri::command]
pub async fn keyvault_pair_poll(
    state: tauri::State<'_, AppState>,
) -> Result<crate::keyvault::PairingStatusDto, String> {
    crate::keyvault::pair_poll_service(state.inner()).await
}

/// Abandons a pairing in flight, discarding the key generated for it.
#[tauri::command]
pub async fn keyvault_pair_cancel() -> Result<(), String> {
    crate::keyvault::pair_cancel_service();
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
pub async fn keyvault_copy_secret<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    slug: String,
) -> Result<(), String> {
    crate::keyvault::throttle(current_time_ms())?;
    let config = crate::keyvault::config_from()?;
    let transport =
        trove_keyvault::ReqwestSecretTransport::new(&config).map_err(|error| error.to_string())?;
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

/// The checks a row must pass to be written.
///
/// Stricter than the read path on purpose. A shortcut that does not parse can
/// never be registered, so refusing it belongs here, where the user sees the
/// refusal and picks another one. Putting the same check on the read path
/// instead would mean one bad row — written by an older version, or by hand —
/// makes `get_settings` fail, and the screen holding the fix stops opening.
fn validate_settings_for_save(settings: &AppSettingsDto) -> Result<(), String> {
    validate_settings(settings)?;
    if crate::hotkey::parse_shortcut(&settings.hotkey).is_none() {
        return Err("invalid_settings".to_owned());
    }
    Ok(())
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
    // The address is remembered, not obeyed, but a value that cannot be an address is still
    // worth refusing here rather than at the moment someone presses Connect.
    let keyvault_is_valid = settings
        .keyvault
        .address()
        .as_deref()
        .is_none_or(|url| trove_keyvault::validate_base_url(url).is_ok());
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
