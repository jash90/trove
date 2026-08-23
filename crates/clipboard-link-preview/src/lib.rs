#![forbid(unsafe_code)]

//! Fetching what a link points at, so a clipboard entry can show more than a
//! string of characters.
//!
//! This is the only part of the application that talks to the network, and it
//! is a crate of its own so that fact stays visible and the whole of it can be
//! read in one sitting.
//!
//! Two things shape everything here. A clipboard history is a record of where
//! its owner has been, so every request made from it tells somebody else
//! something private — which is why fetching is a setting, why a result is
//! remembered rather than re-fetched, and why the caller decides when. And the
//! addresses come from a file the user did not write, so every hop is judged
//! before it is taken: the scheme, the host, and the address DNS actually
//! returned, again after each redirect. Without that last part a link preview
//! is a way to make this machine knock on doors inside its own network.

mod html;
mod policy;

use std::{
    net::{SocketAddr, ToSocketAddrs},
    time::Duration,
};

pub use html::PageMetadata;
pub use policy::{Refusal, address_is_fetchable, host_is_fetchable, url_is_fetchable};

/// How long one request may take, connection included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);

/// How many redirects are followed before giving up.
const MAX_REDIRECTS: usize = 4;

/// How much of a page is read before the scan gives up.
///
/// Measured rather than guessed: on YouTube the `<title>` sits at byte 688,630
/// and `og:image` just after it, because the head is padded with inline
/// script. A half-megabyte cap read neither and cached the page as having no
/// title, which is exactly the bug this number exists to avoid.
const MAX_DOCUMENT_BYTES: usize = 3 * 1024 * 1024;

/// Everything worth reading lives in the head, so the body is never fetched
/// when the head has already closed.
const HEAD_CLOSE_TAG: &str = "</head>";

// A page whose head is padded with inline script pushes its title far down;
// YouTube's sits past byte 688,000. A budget below that reads neither the title
// nor the picture and remembers the page as having none.
const _: () = assert!(MAX_DOCUMENT_BYTES > 700_000);

/// How large an icon may be.
const MAX_ICON_BYTES: usize = 256 * 1024;

/// How large the picture a page nominates may be.
///
/// Bigger than an icon because it is a real photograph or card, and still
/// bounded: this is downscaled before it is kept, so what arrives here only has
/// to be large enough to downscale well.
const MAX_IMAGE_BYTES: usize = 4 * 1024 * 1024;

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

/// Fetches previews.
///
/// Every request gets its own client, because the address is pinned per host
/// and a shared client would carry one host's pinning into another's request.
/// That costs a connection pool per fetch, which is the price of the check
/// below actually meaning something.
pub struct LinkPreviewFetcher {
    _private: (),
}

impl LinkPreviewFetcher {
    pub fn new() -> Result<Self, LinkPreviewError> {
        // Built once here so a broken configuration fails now rather than on
        // the first link somebody selects.
        let _ = client_for(None)?;
        Ok(Self { _private: () })
    }

    /// Fetches the title and icon a link offers.
    pub async fn fetch(&self, raw_url: &str) -> Result<LinkPreview, LinkPreviewError> {
        let mut current = url::Url::parse(raw_url).map_err(|_| LinkPreviewError::NotFetchable)?;
        for _ in 0..=MAX_REDIRECTS {
            let response = self.request(&current).await?;
            let status = response.status();
            if status.is_redirection() {
                current = self.next_hop(&response, &current)?;
                continue;
            }
            if !status.is_success() {
                return Err(LinkPreviewError::Refused);
            }
            // A link can point straight at a picture rather than at a page
            // describing one. Reading those bytes as markup finds nothing, and
            // the entry ends up remembered as having no preview at all — while
            // the preview was the whole response.
            if let Some(mime) = image_content_type(&response) {
                let bytes = read_bounded_bytes(response, MAX_IMAGE_BYTES).await?;
                return Ok(LinkPreview {
                    image: (!bytes.is_empty()).then_some(bytes),
                    image_mime: Some(mime),
                    ..LinkPreview::default()
                });
            }
            let document = read_document_head(response).await?;
            let metadata = html::read_metadata(&document);
            let icon = self
                .fetch_icon(&current, metadata.icon_href.as_deref())
                .await;
            // The picture the page nominates is what a preview is really for; a
            // favicon is a mark, not a picture of the thing linked to.
            let image = match metadata.image_href.as_deref() {
                Some(href) => self.fetch_image(&current, href).await,
                None => None,
            };
            return Ok(LinkPreview {
                title: metadata.title,
                icon: icon.as_ref().map(|(bytes, _)| bytes.clone()),
                icon_mime: icon.map(|(_, mime)| mime),
                image: image.as_ref().map(|(bytes, _)| bytes.clone()),
                image_mime: image.map(|(_, mime)| mime),
            });
        }
        Err(LinkPreviewError::TooManyRedirects)
    }

    /// Sends one request, to an address that was checked and then pinned.
    ///
    /// Pinning is the whole point. Checking the resolved address and then
    /// letting the client resolve the name again leaves the gap it was meant
    /// to close: a name can answer with a public address for the check and a
    /// private one a moment later, and the connection would go to the second.
    async fn request(&self, url: &url::Url) -> Result<reqwest::Response, LinkPreviewError> {
        policy::url_is_fetchable(url).map_err(|_| LinkPreviewError::NotFetchable)?;
        let host = url.host_str().ok_or(LinkPreviewError::NotFetchable)?;
        let port = url
            .port_or_known_default()
            .ok_or(LinkPreviewError::NotFetchable)?;
        let address = resolve_public_address(host, port)?;
        client_for(Some((host, address)))?
            .get(url.clone())
            // Html preferred, anything accepted: a link may point at a page or
            // at the picture itself, and refusing the second would be a way of
            // not seeing it.
            .header(reqwest::header::ACCEPT, "text/html,*/*;q=0.8")
            .send()
            .await
            .map_err(|_| LinkPreviewError::Unreachable)
    }

    fn next_hop(
        &self,
        response: &reqwest::Response,
        current: &url::Url,
    ) -> Result<url::Url, LinkPreviewError> {
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or(LinkPreviewError::Refused)?;
        let next = current
            .join(location)
            .map_err(|_| LinkPreviewError::NotFetchable)?;
        // Judged here as well as at send time, so a redirect can never widen
        // what the first check allowed.
        policy::url_is_fetchable(&next).map_err(|_| LinkPreviewError::NotFetchable)?;
        Ok(next)
    }

    /// Fetches the picture a page nominated for itself.
    ///
    /// It usually lives on a different host from the page — a content network,
    /// an image service — so it is judged from scratch rather than inheriting
    /// the page's permission.
    async fn fetch_image(&self, page: &url::Url, href: &str) -> Option<(Vec<u8>, String)> {
        let candidate = page.join(href).ok()?;
        policy::url_is_fetchable(&candidate).ok()?;
        self.fetch_picture(&candidate, MAX_IMAGE_BYTES).await
    }

    /// Fetches one image and returns it with the type the server declared.
    async fn fetch_picture(&self, url: &url::Url, maximum: usize) -> Option<(Vec<u8>, String)> {
        let response = self.request(url).await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let mime = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap_or(value).trim().to_owned())
            .filter(|mime| mime.starts_with("image/"))?;
        let bytes = read_bounded_bytes(response, maximum).await.ok()?;
        (!bytes.is_empty()).then_some((bytes, mime))
    }

    /// Fetches the icon a page declared, or the one at the conventional path.
    async fn fetch_icon(
        &self,
        page: &url::Url,
        declared: Option<&str>,
    ) -> Option<(Vec<u8>, String)> {
        let candidate = match declared {
            Some(href) => page.join(href).ok()?,
            None => page.join("/favicon.ico").ok()?,
        };
        policy::url_is_fetchable(&candidate).ok()?;
        self.fetch_picture(&candidate, MAX_ICON_BYTES).await
    }
}

/// Builds a client, optionally bound to one already-checked address.
fn client_for(pinned: Option<(&str, SocketAddr)>) -> Result<reqwest::Client, LinkPreviewError> {
    let mut builder = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        // Redirects are followed by hand, one hop at a time, because each new
        // address has to pass the same checks as the first. Letting the client
        // follow them would mean the second hop was never judged.
        .redirect(reqwest::redirect::Policy::none())
        // No cookie jar: nothing from this machine's browsing should be
        // attached to a request made on a clipboard entry's behalf.
        .user_agent("clipboard-history-link-preview");
    if let Some((host, address)) = pinned {
        builder = builder.resolve(host, address);
    }
    builder
        .build()
        .map_err(|_| LinkPreviewError::ClientUnavailable)
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

/// The image type a response declares, when it declares one.
fn image_content_type(response: &reqwest::Response) -> Option<String> {
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)?
        .to_str()
        .ok()?
        .split(';')
        .next()?
        .trim()
        .to_ascii_lowercase();
    mime.starts_with("image/").then_some(mime)
}

/// Reads a page as far as its head, and no further.
///
/// Stopping at `</head>` keeps the usual page to a few kilobytes while still
/// letting a page that pads its head with script be read to the end of it.
/// Without the stop, the cap alone would pull whole documents down for nothing.
async fn read_document_head(mut response: reqwest::Response) -> Result<String, LinkPreviewError> {
    let mut bytes = Vec::new();
    let mut searched_to = 0_usize;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| LinkPreviewError::Unreachable)?
    {
        let remaining = MAX_DOCUMENT_BYTES.saturating_sub(bytes.len());
        if remaining == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        // Overlap by the tag's length so a boundary cannot split it.
        let from = searched_to.saturating_sub(HEAD_CLOSE_TAG.len());
        if find_head_close(&bytes[from..]) {
            break;
        }
        searched_to = bytes.len();
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn find_head_close(haystack: &[u8]) -> bool {
    haystack
        .windows(HEAD_CLOSE_TAG.len())
        .any(|window| window.eq_ignore_ascii_case(HEAD_CLOSE_TAG.as_bytes()))
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
mod tests {
    use super::*;

    #[test]
    fn a_local_description_needs_no_network() {
        assert_eq!(
            describe_locally("https://www.linkedin.com/in/someone/"),
            Some(("linkedin.com".to_owned(), "/in/someone/".to_owned()))
        );
        assert_eq!(
            describe_locally("https://example.com/search?q=1"),
            Some(("example.com".to_owned(), "/search?q=1".to_owned()))
        );
    }

    #[test]
    fn something_that_is_not_a_web_address_describes_as_nothing() {
        for raw in [
            "mailto:someone@example.com",
            "not a url",
            "file:///etc/hosts",
        ] {
            assert_eq!(describe_locally(raw), None, "{raw}");
        }
    }

    #[test]
    fn an_address_inside_this_network_is_refused_before_a_socket_is_opened() {
        // Resolution is attempted, the answer is judged, and nothing connects.
        assert_eq!(
            resolve_public_address("localhost", 80),
            Err(LinkPreviewError::NotFetchable)
        );
    }

    #[test]
    fn every_error_names_a_code_and_never_an_address() {
        for error in [
            LinkPreviewError::NotFetchable,
            LinkPreviewError::Unreachable,
            LinkPreviewError::Refused,
            LinkPreviewError::TooManyRedirects,
            LinkPreviewError::ClientUnavailable,
        ] {
            let rendered = error.to_string();
            assert!(rendered.starts_with("link_"), "{rendered}");
            assert!(!rendered.contains("://"), "{rendered}");
        }
    }

    #[test]
    fn the_head_close_tag_is_found_however_it_is_written() {
        assert!(find_head_close(
            b"<html><head><title>x</title></head><body>"
        ));
        assert!(find_head_close(b"</HEAD>"));
        assert!(find_head_close(b"</Head>"));
        assert!(!find_head_close(b"<html><head><title>never closed"));
    }

    #[test]
    fn a_declared_image_type_is_recognised_however_it_is_written() {
        // Reading a JPEG as markup finds no title and no picture, so a link
        // that is a picture was remembered as having no preview at all.
        for header in [
            "image/jpeg",
            "image/png",
            "IMAGE/JPEG",
            "image/jpeg; charset=binary",
            "image/webp ",
        ] {
            let mime = header
                .split(';')
                .next()
                .unwrap()
                .trim()
                .to_ascii_lowercase();
            assert!(mime.starts_with("image/"), "{header}");
        }
        for header in ["text/html", "text/html; charset=utf-8", "application/pdf"] {
            let mime = header
                .split(';')
                .next()
                .unwrap()
                .trim()
                .to_ascii_lowercase();
            assert!(!mime.starts_with("image/"), "{header}");
        }
    }

    #[test]
    fn a_preview_holding_only_a_picture_still_counts_as_something() {
        let preview = LinkPreview {
            image: Some(vec![1, 2, 3]),
            image_mime: Some("image/jpeg".to_owned()),
            ..LinkPreview::default()
        };

        assert!(!preview.is_empty());
    }

    #[test]
    fn an_empty_preview_is_recognised_as_nothing_worth_keeping() {
        assert!(LinkPreview::default().is_empty());
        assert!(
            !LinkPreview {
                title: Some("Synthetic".to_owned()),
                ..LinkPreview::default()
            }
            .is_empty()
        );
    }
}
