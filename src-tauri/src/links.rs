//! Answering "what is this link" for the palette.
//!
//! A cached answer is returned as it stands. A missing one is fetched once and
//! remembered — including when it fails, so a site that has gone is not asked
//! again every time the list scrolls past it.
//!
//! Nothing here fetches unless the setting says so. With it off this still
//! answers, with what the address itself says and no request at all.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use clipboard_store::{LinkPreviewRecord, LinkPreviewStatus, StoreHandle};
use rusqlite::OptionalExtension;
use serde::Serialize;
use tauri::{Emitter, Manager, Runtime};

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

/// How many consecutive failures close a host's circuit.
const FAILURE_LIMIT: u32 = 3;

/// How long a closed circuit stays closed.
///
/// A dead site is asked at most this often; a living one answers long before
/// the first closure, because successes reset the count.
const CIRCUIT_COOLDOWN: Duration = Duration::from_secs(30 * 60);

/// Decides whether a fetch may start, and remembers how hosts behave.
///
/// Two protections live here rather than in the fetching crate because both
/// are about *asking again*, which is the application's habit and not the
/// network's. One stops the same entry from being fetched twice at once when
/// the list scrolls past it faster than the site answers. The other stops a
/// host that has stopped answering from being asked on every selection for
/// the rest of the session.
#[derive(Default)]
pub struct FetchCoordinator {
    in_flight: HashSet<i64>,
    open_circuits: HashMap<String, Instant>,
    consecutive_failures: HashMap<String, u32>,
}

/// What the coordinator said about starting one fetch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BeginDecision {
    /// Nothing stands in the way; the caller fetches and settles later.
    Proceed,
    /// A fetch for this entry is already running; its event will arrive.
    AlreadyRunning,
    /// This host has failed too recently to be asked again yet.
    CircuitOpen,
}

impl FetchCoordinator {
    pub fn begin(&mut self, content_id: i64, host: &str, now: Instant) -> BeginDecision {
        if let Some(&until) = self.open_circuits.get(host)
            && now < until
        {
            return BeginDecision::CircuitOpen;
        }
        // An expired circuit opens again only through fresh evidence.
        self.open_circuits.remove(host);
        if !self.in_flight.insert(content_id) {
            return BeginDecision::AlreadyRunning;
        }
        BeginDecision::Proceed
    }

    /// Records that an entry's fetch ended, and what became of it.
    ///
    /// Only genuine network failures count towards a host's circuit: a refusal
    /// or an empty answer means the server was reached, and a success or a
    /// policy refusal means there is nothing to protect anybody from.
    pub fn settle(&mut self, content_id: i64, host: &str, outcome: FetchOutcome, now: Instant) {
        self.in_flight.remove(&content_id);
        match outcome {
            FetchOutcome::ReachedServer => {
                self.consecutive_failures.remove(host);
            }
            FetchOutcome::NetworkFailed => {
                let failures = self
                    .consecutive_failures
                    .entry(host.to_owned())
                    .or_default();
                *failures += 1;
                if *failures >= FAILURE_LIMIT {
                    self.open_circuits
                        .insert(host.to_owned(), now + CIRCUIT_COOLDOWN);
                    // The counter starts over when the circuit reopens.
                    self.consecutive_failures.remove(host);
                }
            }
            FetchOutcome::NeverContacted => {}
        }
    }
}

/// How one fetch ended, reduced to what the coordinator learns from it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FetchOutcome {
    /// The site answered with something or nothing, but it answered.
    ReachedServer,
    /// The attempt died before an answer — timeouts, unreachable hosts.
    NetworkFailed,
    /// Nothing left this machine, so no host learned anything.
    NeverContacted,
}

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
    /// True only when a fetch is under way and its result will be announced.
    ///
    /// The window cannot work this out for itself: a page still being asked and
    /// a page that answered without a picture look identical from there — both
    /// carry no image and are not local-only. Anything shown while waiting has
    /// to be driven from here, or it would sit spinning forever on the many
    /// pages that simply have no `og:image`.
    pub fetching: bool,
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
        // A stored answer is the end of the road, however it turned out.
        return Ok(Some(render(&store, host, rest, cached, false, false)));
    }

    let enabled = crate::commands::link_previews_enabled(&store);
    let mut fetching = false;
    if enabled {
        // One lock, held for the decision only: the fetch itself runs without
        // it, and settling happens on its own short acquisition.
        let decision = state.previews.lock().expect("preview coordinator").begin(
            content_id,
            &host,
            Instant::now(),
        );
        if decision == BeginDecision::Proceed {
            start_fetch(app, store, content_id, event_id, url.clone(), &host);
        }
        // Only a run we started. `AlreadyRunning` looks like waiting and is
        // not: the coordinator tracks a fetch by content, while the event that
        // ends the wait carries the entry that started it. The same address
        // copied twice is two entries over one content — real histories have
        // ten — so the event would announce the other entry and the wait here
        // would never end. An open circuit ends in nothing either.
        fetching = decision == BeginDecision::Proceed;
    }
    Ok(Some(LinkPreviewDto {
        host,
        rest,
        title: None,
        icon_mime: None,
        icon_base64: None,
        image_mime: None,
        image_base64: None,
        local_only: !enabled,
        fetching,
    }))
}

/// Fetches one link behind the answer that already went back.
///
/// The outcome is settled under the coordinator's lock before anything is
/// announced, so a host that has just fallen over is not asked again by the
/// very next selection that scrolls past.
fn start_fetch<R: Runtime>(
    app: Option<tauri::AppHandle<R>>,
    store: StoreHandle,
    content_id: i64,
    event_id: i64,
    url: String,
    host: &str,
) {
    let host = host.to_owned();
    tauri::async_runtime::spawn(async move {
        let (record, outcome) = fetch_once(&url).await;
        // Stored even when it failed, so a site that is gone is asked once.
        let stored = store.store_link_preview(content_id, record).await.is_ok();
        if stored
            && let Some(app) = &app
            && let Some(state) = app.try_state::<crate::state::AppState>()
        {
            let mut coordinator = state.previews.lock().expect("preview coordinator");
            coordinator.settle(content_id, &host, outcome, Instant::now());
        }
        if stored && let Some(app) = app {
            let _ = app.emit(LINK_PREVIEW_READY_EVENT, event_id);
        }
    });
}

/// Fetches one link, turning every outcome into something rememberable.
///
/// Alongside the record goes what the attempt proved about the host: whether
/// it answered, died before answering, or was never contacted at all. Only the
/// middle one is evidence that asking again soon is pointless.
async fn fetch_once(url: &str) -> (LinkPreviewRecord, FetchOutcome) {
    let Ok(fetcher) = clipboard_link_preview::LinkPreviewFetcher::new() else {
        return (
            LinkPreviewRecord {
                status: LinkPreviewStatus::Failed,
                ..LinkPreviewRecord::default()
            },
            FetchOutcome::NeverContacted,
        );
    };
    match fetcher.fetch(url).await {
        Ok(preview) if preview.is_empty() => (
            LinkPreviewRecord {
                status: LinkPreviewStatus::Empty,
                ..LinkPreviewRecord::default()
            },
            FetchOutcome::ReachedServer,
        ),
        Ok(preview) => (
            LinkPreviewRecord {
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
            FetchOutcome::ReachedServer,
        ),
        Err(clipboard_link_preview::LinkPreviewError::NotFetchable) => (
            LinkPreviewRecord {
                status: LinkPreviewStatus::Refused,
                ..LinkPreviewRecord::default()
            },
            // The address was judged without anything leaving this machine.
            FetchOutcome::NeverContacted,
        ),
        Err(clipboard_link_preview::LinkPreviewError::Refused) => (
            LinkPreviewRecord {
                status: LinkPreviewStatus::Refused,
                ..LinkPreviewRecord::default()
            },
            // A status came back, so somebody was home to send it.
            FetchOutcome::ReachedServer,
        ),
        Err(_) => (
            LinkPreviewRecord {
                status: LinkPreviewStatus::Failed,
                ..LinkPreviewRecord::default()
            },
            FetchOutcome::NetworkFailed,
        ),
    }
}

fn render(
    store: &StoreHandle,
    host: String,
    rest: String,
    cached: CachedPreview,
    local_only: bool,
    fetching: bool,
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
        fetching,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn coordinator() -> FetchCoordinator {
        FetchCoordinator::default()
    }

    #[test]
    fn the_same_entry_is_never_fetched_twice_at_once() {
        let mut coordinator = coordinator();
        let now = Instant::now();

        assert_eq!(
            coordinator.begin(1, "example.com", now),
            BeginDecision::Proceed
        );
        // A second selection of the same row, before the first fetch answered.
        assert_eq!(
            coordinator.begin(1, "example.com", now),
            BeginDecision::AlreadyRunning
        );
        // A different entry on the same host proceeds independently.
        assert_eq!(
            coordinator.begin(2, "example.com", now),
            BeginDecision::Proceed
        );

        coordinator.settle(1, "example.com", FetchOutcome::ReachedServer, now);
        assert_eq!(
            coordinator.begin(1, "example.com", now),
            BeginDecision::Proceed
        );
    }

    #[test]
    fn a_host_that_keeps_failing_stops_being_asked() {
        let mut coordinator = coordinator();
        let start = Instant::now();

        for _ in 0..FAILURE_LIMIT - 1 {
            assert_eq!(
                coordinator.begin(1, "dead.example.com", start),
                BeginDecision::Proceed
            );
            coordinator.settle(1, "dead.example.com", FetchOutcome::NetworkFailed, start);
        }
        // One failure short of the limit: still willing.
        assert_eq!(
            coordinator.begin(3, "dead.example.com", start),
            BeginDecision::Proceed
        );
        coordinator.settle(3, "dead.example.com", FetchOutcome::NetworkFailed, start);

        assert_eq!(
            coordinator.begin(4, "dead.example.com", start + Duration::from_secs(60)),
            BeginDecision::CircuitOpen
        );
        // The cooldown passes and the host is asked again.
        assert_eq!(
            coordinator.begin(
                5,
                "dead.example.com",
                start + CIRCUIT_COOLDOWN + Duration::from_secs(1)
            ),
            BeginDecision::Proceed
        );
    }

    #[test]
    fn an_answered_host_never_closes_its_circuit() {
        let mut coordinator = coordinator();
        let now = Instant::now();

        for content_id in 1..=i64::from(FAILURE_LIMIT) + 2 {
            assert_eq!(
                coordinator.begin(content_id, "slow.example.com", now),
                BeginDecision::Proceed,
                "{content_id}"
            );
            // Empty answers and refusals mean somebody was home; neither is a
            // reason to stop asking.
            coordinator.settle(
                content_id,
                "slow.example.com",
                FetchOutcome::ReachedServer,
                now,
            );
        }
    }

    #[test]
    fn one_success_after_failures_resets_the_count() {
        let mut coordinator = coordinator();
        let now = Instant::now();

        for content_id in 1..i64::from(FAILURE_LIMIT) {
            coordinator.begin(content_id, "flaky.example.com", now);
            coordinator.settle(
                content_id,
                "flaky.example.com",
                FetchOutcome::NetworkFailed,
                now,
            );
        }
        coordinator.begin(9, "flaky.example.com", now);
        coordinator.settle(9, "flaky.example.com", FetchOutcome::ReachedServer, now);

        for content_id in 10..10 + i64::from(FAILURE_LIMIT) {
            assert_eq!(
                coordinator.begin(content_id, "flaky.example.com", now),
                BeginDecision::Proceed,
                "{content_id}"
            );
            coordinator.settle(
                content_id,
                "flaky.example.com",
                FetchOutcome::NetworkFailed,
                now,
            );
        }
    }

    #[test]
    fn hosts_are_judged_separately() {
        let mut coordinator = coordinator();
        let now = Instant::now();

        for content_id in 1..=i64::from(FAILURE_LIMIT) {
            coordinator.begin(content_id, "one.example.com", now);
            coordinator.settle(
                content_id,
                "one.example.com",
                FetchOutcome::NetworkFailed,
                now,
            );
        }
        assert_eq!(
            coordinator.begin(99, "one.example.com", now),
            BeginDecision::CircuitOpen
        );
        assert_eq!(
            coordinator.begin(99, "two.example.com", now),
            BeginDecision::Proceed
        );
    }

    #[test]
    fn a_fetch_that_never_left_does_not_count_for_or_against_a_host() {
        let mut coordinator = coordinator();
        let now = Instant::now();

        coordinator.begin(1, "example.com", now);
        coordinator.settle(1, "example.com", FetchOutcome::NeverContacted, now);

        for content_id in 2..=i64::from(FAILURE_LIMIT) {
            coordinator.begin(content_id, "example.com", now);
            coordinator.settle(content_id, "example.com", FetchOutcome::NeverContacted, now);
        }
        assert_eq!(
            coordinator.begin(50, "example.com", now),
            BeginDecision::Proceed
        );
    }
}
