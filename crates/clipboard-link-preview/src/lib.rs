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

/// How much of a page is read before the scan gives up on finding a title.
const MAX_DOCUMENT_BYTES: usize = 512 * 1024;

/// How large an icon may be.
const MAX_ICON_BYTES: usize = 256 * 1024;

/// What was learned about a link.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LinkPreview {
    pub title: Option<String>,
    pub icon: Option<Vec<u8>>,
    pub icon_mime: Option<String>,
}

impl LinkPreview {
    /// True when the fetch found nothing worth remembering.
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.icon.is_none()
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
            let document = read_bounded_text(response, MAX_DOCUMENT_BYTES).await?;
            let metadata = html::read_metadata(&document);
            let icon = self
                .fetch_icon(&current, metadata.icon_href.as_deref())
                .await;
            return Ok(LinkPreview {
                title: metadata.title,
                icon: icon.as_ref().map(|(bytes, _)| bytes.clone()),
                icon_mime: icon.map(|(_, mime)| mime),
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
            .header(reqwest::header::ACCEPT, "text/html")
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
        let response = self.request(&candidate).await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let mime = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap_or(value).trim().to_owned())
            .filter(|mime| mime.starts_with("image/"))?;
        let bytes = read_bounded_bytes(response, MAX_ICON_BYTES).await.ok()?;
        (!bytes.is_empty()).then_some((bytes, mime))
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

async fn read_bounded_text(
    response: reqwest::Response,
    maximum: usize,
) -> Result<String, LinkPreviewError> {
    let bytes = read_bounded_bytes(response, maximum).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
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
