#![forbid(unsafe_code)]

//! Fetching what a link points at, so a clipboard entry can show more than a
//! string of characters.
//!
//! This is the only part of the application that talks to the network, and it
//! is a crate of its own so that fact stays visible and the whole of it can be
//! read in one sitting.
//!
//! Three things shape everything here. A clipboard history is a record of where
//! its owner has been, so every request made from it tells somebody else
//! something private — which is why fetching is a setting, why a result is
//! remembered rather than re-fetched, and why the caller decides when. And the
//! addresses come from a file the user did not write, so every hop is judged
//! before it is taken: the scheme, the host, and the address DNS actually
//! returned, again after each redirect. Without that last part a link preview
//! is a way to make this machine knock on doors inside its own network.
//!
//! What is asked of a page follows the priority chain every serious unfurler
//! converges on, first match winning: oEmbed discovery, Open Graph, Twitter
//! cards, structured data. Each earlier source is the page speaking about the
//! specific thing linked to; each later one is a template's guess.

mod charset;
mod html;
mod jsonld;
mod manifest;
mod normalize;
mod policy;

use std::{
    io::Cursor,
    net::{SocketAddr, ToSocketAddrs},
    time::Duration,
};

pub use html::{ImageCandidate, PageMetadata};
pub use policy::{Refusal, address_is_fetchable, host_is_fetchable, url_is_fetchable};

/// How long one request may take, connection included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);

/// How long the connection alone may take.
///
/// Bounded separately because the two failures are not the same. A host that
/// never completes a handshake — a dropped packet to a firewalled address, a
/// name pointing somewhere unreachable — would otherwise spend the whole
/// budget before a byte was asked for, leaving nothing for the answer. A site
/// that connects promptly and thinks for a while still gets the full time.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// How much of a web app manifest is read.
///
/// A manifest is a short list of names, colours and icons; anything past this
/// is not one.
const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// How many redirects are followed before giving up.
const MAX_REDIRECTS: usize = 4;

/// How much of a page is read before the scan gives up.
///
/// Measured rather than guessed: on YouTube the `<title>` sits at byte 688,630
/// and `og:image` just after it, because the head is padded with inline
/// script. A half-megabyte cap read neither and cached the page as having no
/// title, which is exactly the bug this number exists to avoid.
const MAX_DOCUMENT_BYTES: usize = 3 * 1024 * 1024;

// A page whose head is padded with inline script pushes its title far down;
// YouTube's sits past byte 688,000. A budget below that reads neither the title
// nor the picture and remembers the page as having none.
const _: () = assert!(MAX_DOCUMENT_BYTES > 700_000);

/// Everything worth reading lives in the head, so the body is never fetched
/// when the head has already closed.
const HEAD_CLOSE_TAG: &str = "</head>";

/// How large an oEmbed answer may be.
///
/// It carries a thumbnail address and a few fields of text, not media.
const MAX_OEMBED_BYTES: usize = 256 * 1024;

/// How large an icon may be.
const MAX_ICON_BYTES: usize = 256 * 1024;

/// How large the picture a page nominates may be.
///
/// Bigger than an icon because it is a real photograph or card, and still
/// bounded: this is downscaled before it is kept, so what arrives here only has
/// to be large enough to downscale well.
const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;

/// Shortest side a nominated picture may have and still be a preview.
///
/// Pages nominate tracking pixels and corner logos as their `og:image`; both
/// are smaller than any card worth showing. Applied only to pictures a page
/// nominates — a link straight at an image file is shown whatever its size,
/// because there the image is the content, not a claim about it.
const MIN_NOMINATED_SIDE: u32 = 200;

/// How elongated a nominated picture may be before it stops being one.
///
/// Below three tenths and above four it is a banner, a rule, or a spacer —
/// layout furniture, not a picture of the thing linked to. Held as whole
/// numbers so the comparison never rounds.
const NOMINATED_RATIO_MIN_TENTHS: u32 = 3;
const NOMINATED_RATIO_MAX_FACTOR: u32 = 4;

/// Who this asks as, first.
const OWN_USER_AGENT: &str = "trove-link-preview";

/// Who this asks as when the first name was turned away.
///
/// A meaningful share of sites render their tags only for crawlers they
/// recognise and answer everyone else with a consent wall or a bare shell.
/// One retry under a crawler's name, only after an explicit refusal, recovers
/// those pages without presenting this application as a crawler from the
/// start.
const CRAWLER_USER_AGENT: &str =
    "Mozilla/5.0 (compatible; Discordbot/2.0; +https://discordapp.com)";

/// Statuses that mean "asked and refused", the only ones worth a second name.
const REFUSAL_STATUS_CODES: [u16; 2] = [403, 451];

/// What was learned about a link.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LinkPreview {
    pub title: Option<String>,
    pub icon: Option<Vec<u8>>,
    pub icon_mime: Option<String>,
    /// The picture the page nominated for itself, as fetched.
    pub image: Option<Vec<u8>>,
    pub image_mime: Option<String>,
}

impl LinkPreview {
    /// True when the fetch found nothing worth remembering.
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.icon.is_none() && self.image.is_none()
    }
}

/// Why a link produced no preview.
///
/// Every variant is a fact about the attempt, never about the page's contents,
/// and none of them carries the address.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LinkPreviewError {
    #[error("link_not_fetchable")]
    NotFetchable,
    #[error("link_unreachable")]
    Unreachable,
    #[error("link_refused")]
    Refused,
    #[error("link_too_many_redirects")]
    TooManyRedirects,
    #[error("link_client_unavailable")]
    ClientUnavailable,
}

/// How much of an answer the caller is willing to hold, and how it is read.
enum BodyBudget {
    /// A page: read only as far as its head, within [`MAX_DOCUMENT_BYTES`].
    Document,
    /// Anything else: a fixed cap, read in full up to it.
    Bounded(usize),
}

/// One HTTP exchange, already reduced to what the pipeline reads.
///
/// The body arrives bounded — the transport cuts the stream at the budget it
/// was given, so an endless page costs an endless nothing.
#[derive(Clone, Debug)]
pub(crate) struct WireResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub location: Option<String>,
    pub body: Vec<u8>,
}

impl WireResponse {
    fn is_redirect(&self) -> bool {
        (300..400).contains(&self.status) && self.location.is_some()
    }

    fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The one thing the outside world does, stated so a test can stand in for it.
///
/// Every decision worth testing — which source wins, which picture is refused,
/// which hop is denied — sits above this line; the transport only carries.
pub(crate) trait Transport {
    async fn send(
        &self,
        url: &url::Url,
        user_agent: &'static str,
        max_body: usize,
    ) -> Result<WireResponse, LinkPreviewError>;

    /// Reads what should be a page, and may stop as soon as its head closes.
    ///
    /// The default is simply the bounded read; only the real transport knows
    /// how to put a stream down mid-flight.
    async fn send_for_document(
        &self,
        url: &url::Url,
        user_agent: &'static str,
    ) -> Result<WireResponse, LinkPreviewError> {
        self.send(url, user_agent, MAX_DOCUMENT_BYTES).await
    }
}

/// Fetches previews.
///
/// Every request gets its own client, because the address is pinned per host
/// and a shared client would carry one host's pinning into another's request.
/// That costs a connection pool per fetch, which is the price of the check
/// below actually meaning something.
pub struct LinkPreviewFetcher {
    pipeline: Pipeline<RealTransport>,
}

impl LinkPreviewFetcher {
    pub fn new() -> Result<Self, LinkPreviewError> {
        // Built once here so a broken configuration fails now rather than on
        // the first link somebody selects.
        let _ = client_for(None)?;
        Ok(Self {
            pipeline: Pipeline {
                transport: RealTransport,
            },
        })
    }

    /// Fetches the title, icon and picture a link offers.
    pub async fn fetch(&self, raw_url: &str) -> Result<LinkPreview, LinkPreviewError> {
        self.pipeline.fetch(raw_url).await
    }
}

/// The whole of what a fetch means, carried out over any transport.
pub(crate) struct Pipeline<S: Transport> {
    transport: S,
}

/// One candidate picture, with what was said about it before any byte moved.
#[derive(Clone)]
struct PictureSource {
    href: String,
    declared_width: Option<u32>,
    declared_height: Option<u32>,
}

/// What an oEmbed endpoint answered.
struct OembedAnswer {
    thumbnail: Option<PictureSource>,
    title: Option<String>,
}

impl<S: Transport> Pipeline<S> {
    async fn fetch(&self, raw_url: &str) -> Result<LinkPreview, LinkPreviewError> {
        let mut current = normalize::normalize(raw_url).ok_or(LinkPreviewError::NotFetchable)?;
        for _ in 0..=MAX_REDIRECTS {
            let response = self.exchange(&current, BodyBudget::Document).await?;
            if response.is_redirect() {
                current = self.next_hop(&response, &current)?;
                continue;
            }
            if !response.is_success() {
                return Err(LinkPreviewError::Refused);
            }
            // A link can point straight at a picture rather than at a page
            // describing one. Reading those bytes as markup finds nothing, and
            // the entry ends up remembered as having no preview at all — while
            // the preview was the whole response.
            if let Some(mime) = direct_picture_mime(&response) {
                return Ok(self.picture_only(response.body, mime));
            }
            return self.read_page(&current, response).await;
        }
        Err(LinkPreviewError::TooManyRedirects)
    }

    /// Reads a page: its text, its icons, and the best picture it names.
    ///
    /// Sources are consulted strictly in priority order and the first picture
    /// that survives validation wins; nothing later is fetched once one has.
    async fn read_page(
        &self,
        final_url: &url::Url,
        response: WireResponse,
    ) -> Result<LinkPreview, LinkPreviewError> {
        let text = charset::decode(&response.body, response.content_type.as_deref());
        let metadata = html::read_metadata(&text);
        // Relative references resolve against where the page said they should
        // — a `<base href>` moves that — or against where the page ended up
        // after redirects, which is not necessarily where it was asked for.
        let base = metadata
            .base_href
            .as_deref()
            .and_then(|href| final_url.join(href).ok())
            .unwrap_or_else(|| final_url.clone());

        let icon = self
            .fetch_icon(
                &base,
                metadata.apple_touch_icon_href.as_deref(),
                metadata.icon_href.as_deref(),
                metadata.manifest_href.as_deref(),
            )
            .await;

        let mut oembed_answer = None;
        let mut sources = Vec::new();
        // An oEmbed discovery link outranks everything the page wrote in meta
        // tags: media services describe the exact item there, while the card
        // tags describe the page template.
        if let Some(href) = metadata.oembed_href.as_deref() {
            oembed_answer = self.fetch_oembed(&base, href).await;
            if let Some(answer) = &oembed_answer
                && let Some(thumbnail) = &answer.thumbnail
            {
                sources.push(thumbnail.clone());
            }
        }
        sources.extend(
            metadata
                .image_candidates
                .iter()
                .map(|candidate| PictureSource {
                    href: candidate.href.clone(),
                    declared_width: candidate.declared_width,
                    declared_height: candidate.declared_height,
                }),
        );
        // Twitter's nomination slots in where the chain puts it: ahead of
        // structured data, behind Open Graph — pages fill it precisely when
        // they were built for a world before or without Open Graph.
        if let Some(href) = metadata.twitter_image_href.as_deref() {
            sources.push(PictureSource {
                href: href.to_owned(),
                declared_width: None,
                declared_height: None,
            });
        }
        for href in jsonld::image_hrefs(&text) {
            sources.push(PictureSource {
                href,
                declared_width: None,
                declared_height: None,
            });
        }
        // Last, because it predates everything above it. A page that offers
        // this and nothing else has one picture to give; a page that offers
        // both has already been served by the better source.
        if let Some(href) = metadata.image_src_href.as_deref() {
            sources.push(PictureSource {
                href: href.to_owned(),
                declared_width: None,
                declared_height: None,
            });
        }

        let image = self.first_worthwhile_picture(&base, &sources).await;
        let title = metadata.title.or_else(|| {
            oembed_answer
                .and_then(|answer| answer.title)
                .and_then(|title| cap_text(&title, 200))
        });
        Ok(LinkPreview {
            title,
            icon: icon.as_ref().map(|(bytes, _)| bytes.clone()),
            icon_mime: icon.as_ref().map(|(_, mime)| mime.clone()),
            image: image.as_ref().map(|(bytes, _)| bytes.clone()),
            image_mime: image.as_ref().map(|(_, mime)| mime.clone()),
        })
    }

    /// Sends one request, to an address that was checked before anything left.
    ///
    /// The check runs here as well as inside the transport: refusing early
    /// keeps private addresses from ever reaching DNS, and repeating it per
    /// exchange means a redirect cannot widen what the first one allowed.
    ///
    /// A refusal by status earns exactly one retry under a crawler's name —
    /// sites that gate their tags behind a recognised bot answer the second
    /// time, and nothing else about the request changes when it does.
    async fn exchange(
        &self,
        url: &url::Url,
        body: BodyBudget,
    ) -> Result<WireResponse, LinkPreviewError> {
        policy::url_is_fetchable(url).map_err(|_| LinkPreviewError::NotFetchable)?;
        // The two arms mirror each other because the transports' futures are
        // different types; the retry rule itself is one: a refused request is
        // made once more under the crawler's name.
        match body {
            BodyBudget::Document => {
                let first = self
                    .transport
                    .send_for_document(url, OWN_USER_AGENT)
                    .await?;
                if REFUSAL_STATUS_CODES.contains(&first.status) {
                    return self
                        .transport
                        .send_for_document(url, CRAWLER_USER_AGENT)
                        .await;
                }
                Ok(first)
            }
            BodyBudget::Bounded(maximum) => {
                let first = self.transport.send(url, OWN_USER_AGENT, maximum).await?;
                if REFUSAL_STATUS_CODES.contains(&first.status) {
                    return self.transport.send(url, CRAWLER_USER_AGENT, maximum).await;
                }
                Ok(first)
            }
        }
    }

    fn next_hop(
        &self,
        response: &WireResponse,
        current: &url::Url,
    ) -> Result<url::Url, LinkPreviewError> {
        let location = response
            .location
            .as_deref()
            .ok_or(LinkPreviewError::NotFetchable)?;
        let next = current
            .join(location)
            .map_err(|_| LinkPreviewError::NotFetchable)?;
        // Judged here as well as at send time, so a redirect can never widen
        // what the first check allowed.
        policy::url_is_fetchable(&next).map_err(|_| LinkPreviewError::NotFetchable)?;
        Ok(next)
    }

    /// Asks the discovery endpoint what the page itself recommends showing.
    async fn fetch_oembed(&self, base: &url::Url, href: &str) -> Option<OembedAnswer> {
        let endpoint = base.join(href).ok()?;
        let response = self
            .exchange(&endpoint, BodyBudget::Bounded(MAX_OEMBED_BYTES))
            .await
            .ok()?;
        if !response.is_success() {
            return None;
        }
        let value: serde_json::Value = serde_json::from_slice(&response.body).ok()?;
        let thumbnail = value
            .get("thumbnail_url")
            .and_then(serde_json::Value::as_str)
            .filter(|href| !href.trim().is_empty())
            .map(|href| PictureSource {
                href: href.to_owned(),
                declared_width: number_field(&value, "thumbnail_width"),
                declared_height: number_field(&value, "thumbnail_height"),
            });
        let title = value
            .get("title")
            .and_then(serde_json::Value::as_str)
            .filter(|title| !title.trim().is_empty())
            .map(str::to_owned);
        // An answer with neither a picture nor a name contributes nothing.
        (thumbnail.is_some() || title.is_some()).then_some(OembedAnswer { thumbnail, title })
    }

    /// Fetches the first candidate that turns out to be a real picture.
    ///
    /// Declarations are cheap lies: a page may claim dimensions that disqualify
    /// a candidate before it is fetched, and a fetched image may fail to be
    /// one. Either way the walk continues down the list.
    async fn first_worthwhile_picture(
        &self,
        base: &url::Url,
        sources: &[PictureSource],
    ) -> Option<(Vec<u8>, String)> {
        for source in sources {
            // One unreadable address skips itself rather than ending the walk.
            let Ok(candidate) = base.join(&source.href) else {
                continue;
            };
            if policy::url_is_fetchable(&candidate).is_err() {
                continue;
            }
            if let (Some(width), Some(height)) = (source.declared_width, source.declared_height)
                && !nominated_dimensions_pass(width, height)
            {
                continue;
            }
            let Some((bytes, mime)) = self.fetch_picture(&candidate, MAX_IMAGE_BYTES).await else {
                continue;
            };
            if is_svg(&mime, &bytes) {
                return Some((bytes, mime));
            }
            let (width, height) = dimension_of(&bytes);
            if nominated_dimensions_pass(width, height) {
                return Some((bytes, mime));
            }
        }
        None
    }

    /// Fetches one image and returns it with the type the server declared.
    async fn fetch_picture(&self, url: &url::Url, maximum: usize) -> Option<(Vec<u8>, String)> {
        let response = self
            .exchange(url, BodyBudget::Bounded(maximum))
            .await
            .ok()?;
        if !response.is_success() {
            return None;
        }
        let declared = response
            .content_type
            .as_deref()
            .filter(|mime| mime.starts_with("image/"))
            .map(str::to_owned);
        // Servers that answer a missing file with a styled page and a 200
        // exist; a body that opens like an image is trusted over a header
        // that does not, and a body that opens like neither is dropped.
        let mime = declared.or_else(|| sniffed_image_mime(&response.body).map(str::to_owned))?;
        (!response.body.is_empty()).then_some((response.body, mime))
    }

    /// Fetches the icon a page declared, preferring the large touch icon, or
    /// the one at the conventional path.
    ///
    /// Icons are exempt from the size floor: a favicon is supposed to be
    /// small, and refusing it for being what it is would leave every entry
    /// without a mark.
    async fn fetch_icon(
        &self,
        page: &url::Url,
        touch_icon: Option<&str>,
        plain_icon: Option<&str>,
        manifest: Option<&str>,
    ) -> Option<(Vec<u8>, String)> {
        // A touch icon is drawn to be looked at, so it settles the question
        // outright and nothing further is asked for.
        if let Some(found) = self.first_icon(page, &[touch_icon]).await {
            return Some(found);
        }
        // Then the manifest, ahead of everything below it rather than behind.
        // Behind, it would never run: `/favicon.ico` answers on nearly every
        // site — measured, five of six — so a sixteen-pixel mark would always
        // win the race and the manifest's 192- or 512-pixel icon would never
        // be reached. The extra request buys the difference between those two,
        // and only on pages that offered no touch icon.
        if let Some(href) = manifest
            && let Some(found) = self.fetch_manifest_icon(page, href).await
        {
            return Some(found);
        }
        // Last: the small mark the page declared, then the conventional
        // location. `/favicon.ico` is a guess and belongs after every
        // reference a page actually wrote.
        self.first_icon(page, &[plain_icon, Some("/favicon.ico")])
            .await
    }

    /// Tries each reference in turn and takes the first that yields a picture.
    async fn first_icon(
        &self,
        page: &url::Url,
        references: &[Option<&str>],
    ) -> Option<(Vec<u8>, String)> {
        for href in references.iter().flatten() {
            let Ok(candidate) = page.join(href) else {
                continue;
            };
            if policy::url_is_fetchable(&candidate).is_err() {
                continue;
            }
            if let Some(picture) = self.fetch_picture(&candidate, MAX_ICON_BYTES).await {
                return Some(picture);
            }
        }
        None
    }

    /// Reads a page's manifest and takes the best icon it lists.
    ///
    /// The manifest is fetched through the same door as everything else — the
    /// address is checked, the body is bounded — so this adds a request, never
    /// a way to reach somewhere the rest of the fetcher would refuse.
    ///
    /// The checks written out below are not what makes that true: `exchange`
    /// refuses an unfetchable address before any request, for every caller.
    /// They are here to skip a bad entry and carry on to the next one, and to
    /// keep the rule visible where a stranger's list of addresses is walked.
    async fn fetch_manifest_icon(
        &self,
        page: &url::Url,
        manifest_href: &str,
    ) -> Option<(Vec<u8>, String)> {
        let manifest_url = page.join(manifest_href).ok()?;
        if policy::url_is_fetchable(&manifest_url).is_err() {
            return None;
        }
        let response = self
            .exchange(&manifest_url, BodyBudget::Bounded(MAX_MANIFEST_BYTES))
            .await
            .ok()?;
        if !response.is_success() {
            return None;
        }
        let document = String::from_utf8_lossy(&response.body);
        for href in manifest::icon_hrefs(&document) {
            // Resolved against the manifest, not the page: a manifest served
            // from a subdirectory writes its icon paths relative to itself.
            let Ok(candidate) = manifest_url.join(&href) else {
                continue;
            };
            if policy::url_is_fetchable(&candidate).is_err() {
                continue;
            }
            if let Some(picture) = self.fetch_picture(&candidate, MAX_ICON_BYTES).await {
                return Some(picture);
            }
        }
        None
    }

    /// Wraps up a response that was itself a picture.
    fn picture_only(&self, bytes: Vec<u8>, mime: String) -> LinkPreview {
        LinkPreview {
            image: (!bytes.is_empty()).then_some(bytes),
            image_mime: Some(mime),
            ..LinkPreview::default()
        }
    }
}

/// Whether a nominated picture's declared-or-measured shape qualifies it.
fn nominated_dimensions_pass(width: u32, height: u32) -> bool {
    let (width, height) = (u64::from(width), u64::from(height));
    width >= u64::from(MIN_NOMINATED_SIDE)
        && height >= u64::from(MIN_NOMINATED_SIDE)
        // `width / height` must sit between three tenths and four: written as
        // whole numbers, 10·width ≥ 3·height and 4·height ≥ width.
        && width * 10 >= height * u64::from(NOMINATED_RATIO_MIN_TENTHS)
        && height * u64::from(NOMINATED_RATIO_MAX_FACTOR) >= width
}

/// Reads a fetched image's dimensions from its own bytes.
fn dimension_of(bytes: &[u8]) -> (u32, u32) {
    image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok())
        .unwrap_or((0, 0))
}

/// Whether these bytes are vector art headed for rasterisation.
///
/// Vector pictures carry no pixel dimensions to validate, and they are drawn
/// fresh at preview size before they are ever kept, so the pixel rules do not
/// apply to them.
fn is_svg(mime: &str, bytes: &[u8]) -> bool {
    mime.contains("svg") || looks_like_svg(bytes)
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    let prefix = &bytes[..bytes.len().min(512)];
    let trimmed = String::from_utf8_lossy(prefix);
    let trimmed = trimmed.trim_start();
    trimmed.starts_with("<svg")
        || ((trimmed.starts_with("<?xml") || trimmed.starts_with("<!")) && trimmed.contains("<svg"))
}

/// Names the image format bytes open with, when they name one at all.
///
/// Content-Type headers lie and are sometimes missing altogether; the first
/// few bytes of each common format are distinctive enough to settle it.
fn sniffed_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("image/png")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if looks_like_svg(bytes) {
        Some("image/svg+xml")
    } else {
        None
    }
}

/// The type of a response that is itself a picture, declared or sniffed.
fn direct_picture_mime(response: &WireResponse) -> Option<String> {
    response
        .content_type
        .as_deref()
        .filter(|mime| mime.starts_with("image/"))
        .map(str::to_owned)
        .or_else(|| sniffed_image_mime(&response.body).map(str::to_owned))
}

fn number_field(value: &serde_json::Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
}

/// Cuts text to a character budget without splitting one in half.
///
/// Titles arrive in whatever length a template felt like; the store below
/// bounds what it will hold, so the bound is honoured here, on characters
/// rather than bytes, so a cut title still ends on a whole letter.
fn cap_text(value: &str, maximum_chars: usize) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(maximum_chars).collect())
}

/// Builds a client, optionally bound to one already-checked address.
fn client_for(pinned: Option<(&str, SocketAddr)>) -> Result<reqwest::Client, LinkPreviewError> {
    let mut builder = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        // Redirects are followed by hand, one hop at a time, because each new
        // address has to pass the same checks as the first. Letting the client
        // follow them would mean the second hop was never judged.
        .redirect(reqwest::redirect::Policy::none())
        // No cookie jar: nothing from this machine's browsing should be
        // attached to a request made on a clipboard entry's behalf.
        .user_agent(OWN_USER_AGENT);
    if let Some((host, address)) = pinned {
        builder = builder.resolve(host, address);
    }
    builder
        .build()
        .map_err(|_| LinkPreviewError::ClientUnavailable)
}

/// Carries requests over the real network, to addresses already checked.
struct RealTransport;

impl Transport for RealTransport {
    async fn send(
        &self,
        url: &url::Url,
        user_agent: &'static str,
        max_body: usize,
    ) -> Result<WireResponse, LinkPreviewError> {
        self.exchange(url, user_agent, max_body, false).await
    }

    async fn send_for_document(
        &self,
        url: &url::Url,
        user_agent: &'static str,
    ) -> Result<WireResponse, LinkPreviewError> {
        self.exchange(url, user_agent, MAX_DOCUMENT_BYTES, true)
            .await
    }
}

impl RealTransport {
    /// One request over the wire, with everything the pipeline reads taken off
    /// the response before it is handed up.
    ///
    /// When `stop_at_head` is set the body stops being read as soon as
    /// `</head>` passes by. Everything worth learning from a page lives in its
    /// head, so the usual document costs kilobytes rather than megabytes —
    /// while a page that pads its head with script is still read to the end of
    /// the padding, which is what the byte cap exists for.
    async fn exchange(
        &self,
        url: &url::Url,
        user_agent: &'static str,
        max_body: usize,
        stop_at_head: bool,
    ) -> Result<WireResponse, LinkPreviewError> {
        // Defence in depth: the pipeline checked a moment ago, and checking
        // again costs nothing next to opening a socket.
        policy::url_is_fetchable(url).map_err(|_| LinkPreviewError::NotFetchable)?;
        let host = url.host_str().ok_or(LinkPreviewError::NotFetchable)?;
        let port = url
            .port_or_known_default()
            .ok_or(LinkPreviewError::NotFetchable)?;
        let address = resolve_public_address(host, port)?;
        let client = client_for(Some((host, address)))?;
        let response = client
            .get(url.clone())
            // Html preferred, anything accepted: a link may point at a page or
            // at the picture itself, and refusing the second would be a way of
            // not seeing it.
            .header(reqwest::header::ACCEPT, "text/html,*/*;q=0.8")
            .header(reqwest::header::ACCEPT_LANGUAGE, "pl-PL,pl;q=0.9,en;q=0.8")
            .header(reqwest::header::USER_AGENT, user_agent)
            .send()
            .await
            .map_err(|_| LinkPreviewError::Unreachable)?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| {
                value
                    .split(';')
                    .next()
                    .unwrap_or(value)
                    .trim()
                    .to_ascii_lowercase()
            });
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = match stop_at_head {
            true => read_until_head_close(response, max_body).await?,
            false => read_bounded_bytes(response, max_body).await?,
        };
        Ok(WireResponse {
            status,
            content_type,
            location,
            body,
        })
    }
}

/// Reads chunks until the head has closed or the budget is spent.
///
/// Scanning from one tag's length back means a chunk boundary cannot hide the
/// very tag that ends the read.
async fn read_until_head_close(
    mut response: reqwest::Response,
    maximum: usize,
) -> Result<Vec<u8>, LinkPreviewError> {
    let mut bytes: Vec<u8> = Vec::new();
    while bytes.len() < maximum {
        let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| LinkPreviewError::Unreachable)?
        else {
            break;
        };
        let remaining = maximum - bytes.len();
        bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        let from = bytes.len().saturating_sub(HEAD_CLOSE_TAG.len());
        if find_head_close(&bytes[from..]) {
            break;
        }
    }
    Ok(bytes)
}

fn find_head_close(haystack: &[u8]) -> bool {
    haystack
        .windows(HEAD_CLOSE_TAG.len())
        .any(|window| window.eq_ignore_ascii_case(HEAD_CLOSE_TAG.as_bytes()))
}
/// Resolves a host and returns the first address that may be contacted.
///
/// The check happens on what DNS answered, not on what the name looked like: a
/// perfectly ordinary domain can point at an address inside this network, on
/// purpose.
fn resolve_public_address(host: &str, port: u16) -> Result<SocketAddr, LinkPreviewError> {
    let candidates = (host, port)
        .to_socket_addrs()
        .map_err(|_| LinkPreviewError::Unreachable)?;
    for candidate in candidates {
        if policy::address_is_fetchable(candidate.ip()) {
            return Ok(candidate);
        }
    }
    Err(LinkPreviewError::NotFetchable)
}

/// Reads a response body up to a limit, discarding the rest.
async fn read_bounded_bytes(
    mut response: reqwest::Response,
    maximum: usize,
) -> Result<Vec<u8>, LinkPreviewError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| LinkPreviewError::Unreachable)?
    {
        if bytes.len() + chunk.len() > maximum {
            bytes.extend_from_slice(&chunk[..maximum - bytes.len()]);
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Reads what a link says about itself without contacting anything.
///
/// This is what a preview shows with fetching turned off, and what it shows
/// before a fetch answers.
pub fn describe_locally(raw_url: &str) -> Option<(String, String)> {
    let url = url::Url::parse(raw_url).ok()?;
    if !policy::scheme_is_fetchable(url.scheme()) {
        return None;
    }
    let host = url.host_str()?.trim_start_matches("www.").to_owned();
    let mut rest = url.path().to_owned();
    if let Some(query) = url.query() {
        rest.push('?');
        rest.push_str(query);
    }
    Some((host, rest))
}

#[cfg(test)]
mod tests;
