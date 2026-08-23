//! Answering "what is this link" for the palette.
//!
//! A cached answer is returned as it stands. A missing one is fetched once and
//! remembered — including when it fails, so a site that has gone is not asked
//! again every time the list scrolls past it.
//!
//! Nothing here fetches unless the setting says so. With it off this still
//! answers, with what the address itself says and no request at all.

use clipboard_store::{LinkPreviewRecord, LinkPreviewStatus, StoreHandle};
use rusqlite::OptionalExtension;
use serde::Serialize;
use tauri::{Emitter, Runtime};

/// Announced when a link's page has answered and been remembered.
///
/// The palette asks again when it hears this, which is why asking never has to
/// wait for the network.
pub const LINK_PREVIEW_READY_EVENT: &str = "link-preview-ready";

/// Largest icon handed to the interface, once encoded.
const MAX_ICON_BASE64_BYTES: usize = 256 * 1024;

/// Largest page picture handed to the interface, once encoded.
///
/// Sized separately from the icon: a favicon is a few kilobytes, while a page's
/// own card downscaled to 320 pixels of lossless PNG runs to a few hundred.
/// Sharing the icon's cap dropped every one of them silently.
const MAX_IMAGE_BASE64_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkPreviewDto {
    /// The host, without a leading `www.`.
    pub host: String,
    /// Path and query, as the address wrote them.
    pub rest: String,
    /// What the page called itself, when it was asked and answered.
    pub title: Option<String>,
    pub icon_mime: Option<String>,
    pub icon_base64: Option<String>,
    /// The picture the page nominates for itself, downscaled.
    pub image_mime: Option<String>,
    pub image_base64: Option<String>,
    /// True when nothing has been fetched for this link and nothing will be
    /// until the setting is turned on.
    pub local_only: bool,
}

/// The stored answer for one link, if there is one.
struct CachedPreview {
    status: String,
    title: Option<String>,
    icon_relpath: Option<String>,
    icon_mime: Option<String>,
    image_relpath: Option<String>,
    image_mime: Option<String>,
}

/// Describes a link, without ever waiting for the network.
///
/// A cached answer comes back as it stands. A missing one comes back as the
/// address alone and a fetch starts behind it; when that finishes the palette
/// is told and asks again. Waiting here instead would leave the pane empty for
/// as long as a slow site takes to answer — and by then the user has usually
/// selected something else.
pub async fn link_preview_service<R: Runtime>(
    app: Option<tauri::AppHandle<R>>,
    state: &crate::state::AppState,
    event_id: i64,
) -> Result<Option<LinkPreviewDto>, String> {
    let store = state.store.clone();
    let Some((content_id, url)) = read_link_target(&store, event_id)? else {
        return Ok(None);
    };
    let Some((host, rest)) = clipboard_link_preview::describe_locally(&url) else {
        return Ok(None);
    };

    if let Some(cached) = read_cached(&store, content_id)? {
        return Ok(Some(render(&store, host, rest, cached, false)));
    }

    let fetching = crate::commands::link_previews_enabled(&store);
    if fetching {
        start_fetch(app, store, content_id, event_id, url);
    }
    Ok(Some(LinkPreviewDto {
        host,
        rest,
        title: None,
        icon_mime: None,
        icon_base64: None,
        image_mime: None,
        image_base64: None,
        local_only: !fetching,
    }))
}

/// Fetches one link behind the answer that already went back.
fn start_fetch<R: Runtime>(
    app: Option<tauri::AppHandle<R>>,
    store: StoreHandle,
    content_id: i64,
    event_id: i64,
    url: String,
) {
    tauri::async_runtime::spawn(async move {
        let record = fetch_once(&url).await;
        // Stored even when it failed, so a site that is gone is asked once.
        if store.store_link_preview(content_id, record).await.is_ok()
            && let Some(app) = app
        {
            let _ = app.emit(LINK_PREVIEW_READY_EVENT, event_id);
        }
    });
}

/// Fetches one link, turning every outcome into something rememberable.
async fn fetch_once(url: &str) -> LinkPreviewRecord {
    let Ok(fetcher) = clipboard_link_preview::LinkPreviewFetcher::new() else {
        return LinkPreviewRecord {
            status: LinkPreviewStatus::Failed,
            ..LinkPreviewRecord::default()
        };
    };
    match fetcher.fetch(url).await {
        Ok(preview) if preview.is_empty() => LinkPreviewRecord {
            status: LinkPreviewStatus::Empty,
            ..LinkPreviewRecord::default()
        },
        Ok(preview) => LinkPreviewRecord {
            status: LinkPreviewStatus::Ok,
            title: preview.title,
            icon: preview.icon,
            icon_mime: preview.icon_mime,
            // Downscaled before it is kept. A page's card is often a megabyte
            // or more, and a history of links would otherwise turn into a
            // picture archive.
            image: preview.image.as_deref().and_then(downscale),
            image_mime: preview.image.as_ref().map(|_| "image/png".to_owned()),
        },
        Err(clipboard_link_preview::LinkPreviewError::NotFetchable) => LinkPreviewRecord {
            status: LinkPreviewStatus::Refused,
            ..LinkPreviewRecord::default()
        },
        Err(_) => LinkPreviewRecord {
            status: LinkPreviewStatus::Failed,
            ..LinkPreviewRecord::default()
        },
    }
}

fn render(
    store: &StoreHandle,
    host: String,
    rest: String,
    cached: CachedPreview,
    local_only: bool,
) -> LinkPreviewDto {
    let icon_base64 = cached
        .icon_relpath
        .as_deref()
        .and_then(|relpath| read_stored_image(store, relpath, MAX_ICON_BASE64_BYTES));
    let image_base64 = cached
        .image_relpath
        .as_deref()
        .and_then(|relpath| read_stored_image(store, relpath, MAX_IMAGE_BASE64_BYTES));
    LinkPreviewDto {
        host,
        rest,
        title: cached.title.filter(|_| cached.status == "ok"),
        icon_mime: cached.icon_mime.filter(|_| icon_base64.is_some()),
        icon_base64,
        image_mime: cached.image_mime.filter(|_| image_base64.is_some()),
        image_base64,
        local_only,
    }
}

fn read_stored_image(store: &StoreHandle, relpath: &str, maximum: usize) -> Option<String> {
    let cas = store.cas_store().ok()?;
    let bytes = cas.read(relpath).ok()?;
    encode_bounded(&bytes, maximum)
}

/// Shrinks a fetched picture to something worth keeping and sending.
///
/// A page that offers no decodable image simply has none: an unreadable card is
/// not a failure the user can act on.
fn downscale(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() > clipboard_images::MAX_IMAGE_INPUT_BYTES {
        return None;
    }
    clipboard_images::make_thumbnail(bytes, clipboard_images::MAX_THUMBNAIL_DIMENSION).ok()
}

fn encode_bounded(bytes: &[u8], maximum: usize) -> Option<String> {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    (encoded.len() <= maximum).then_some(encoded)
}

/// Reads the address a link entry holds, if the entry is a link at all.
fn read_link_target(store: &StoreHandle, event_id: i64) -> Result<Option<(i64, String)>, String> {
    let row = store
        .with_reader(|connection| {
            connection
                .query_row(
                    "SELECT he.content_id, c.kind, c.preview_text
                     FROM history_event he
                     JOIN content c ON c.content_id = he.content_id
                     WHERE he.event_id = ?1",
                    [event_id],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()
        })
        .map_err(|_| "link_preview_unavailable".to_owned())?;
    let Some((content_id, kind, preview_text)) = row else {
        return Ok(None);
    };
    if kind != "link" {
        return Ok(None);
    }
    Ok(Some((content_id, preview_text)))
}

fn read_cached(store: &StoreHandle, content_id: i64) -> Result<Option<CachedPreview>, String> {
    store
        .with_reader(|connection| {
            connection
                .query_row(
                    "SELECT status, title, icon_relpath, icon_mime,
                            image_relpath, image_mime
                     FROM link_preview WHERE content_id = ?1",
                    [content_id],
                    |row| {
                        Ok(CachedPreview {
                            status: row.get(0)?,
                            title: row.get(1)?,
                            icon_relpath: row.get(2)?,
                            icon_mime: row.get(3)?,
                            image_relpath: row.get(4)?,
                            image_mime: row.get(5)?,
                        })
                    },
                )
                .optional()
        })
        .map_err(|_| "link_preview_unavailable".to_owned())
}
