//! The address a link is fetched as, cleaned of what changes nothing.
//!
//! A clipboard entry holds whatever the copying application put there, and
//! sharing links arrive wearing tracking parameters — `utm_source`, click
//! identifiers, campaign tags. Fetching those addresses asks the tracker's
//! endpoint to record the fetch, and caches one preview per spelling of what
//! is the same page.
//!
//! What is deliberately not done here: reordering query parameters. Some
//! URLs carry signatures over their exact query, and "normalising" those
//! breaks them; removing tracking parameters does not, because no signature
//! covers a parameter whose only purpose is to be reported.

use url::Url;

/// Parameters whose only job is to be reported back to somebody.
const TRACKING_PARAMS: [&str; 4] = ["fbclid", "gclid", "mc_eid", "_ga"];

/// Parameter prefixes with the same job, from every analytics suite at once.
const TRACKING_PARAM_PREFIXES: [&str; 1] = ["utm_"];

/// Parses an address and takes the tracking out of it.
///
/// Everything else — order, encoding, case — is left exactly as it arrived,
/// because the address has to keep working first and read cleanly second.
pub fn normalize(raw_url: &str) -> Option<Url> {
    let mut url = Url::parse(raw_url).ok()?;
    if let Some(query) = url.query() {
        let kept: Vec<&str> = query.split('&').filter(|pair| !is_tracking(pair)).collect();
        match kept.is_empty() {
            // A query made only of trackers is gone entirely rather than
            // replaced by an empty string, which would render as `?`.
            true => url.set_query(None),
            false => url.set_query(Some(&kept.join("&"))),
        };
    }
    // The fragment never reaches a server. On pages where it selects content
    // it does so in the browser, which a preview is not.
    url.set_fragment(None);
    Some(url)
}

fn is_tracking(pair: &str) -> bool {
    let key = pair.split('=').next().unwrap_or(pair);
    TRACKING_PARAMS.contains(&key)
        || TRACKING_PARAM_PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracking_parameters_are_taken_out() {
        assert_eq!(
            normalize("https://example.invalid/a?utm_source=x&utm_medium=y&id=7")
                .unwrap()
                .as_str(),
            "https://example.invalid/a?id=7"
        );
        for param in ["fbclid", "gclid", "mc_eid", "_ga"] {
            let raw = format!("https://example.invalid/a?{param}=1&keep=1");
            assert_eq!(
                normalize(&raw).unwrap().as_str(),
                "https://example.invalid/a?keep=1",
                "{param}"
            );
        }
    }

    #[test]
    fn a_query_of_nothing_but_trackers_disappears_whole() {
        assert_eq!(
            normalize("https://example.invalid/a?fbclid=abc")
                .unwrap()
                .as_str(),
            "https://example.invalid/a"
        );
    }

    #[test]
    fn the_fragment_is_dropped() {
        assert_eq!(
            normalize("https://example.invalid/a#section")
                .unwrap()
                .as_str(),
            "https://example.invalid/a"
        );
    }

    #[test]
    fn everything_else_arrives_exactly_as_it_left() {
        // Order preserved, case preserved, values untouched: this address has
        // to keep working, not just look tidy.
        assert_eq!(
            normalize("https://example.invalid/Path?B=2&A=1&token=Ab%2Bc")
                .unwrap()
                .as_str(),
            "https://example.invalid/Path?B=2&A=1&token=Ab%2Bc"
        );
    }

    #[test]
    fn a_key_that_merely_contains_a_tracker_is_not_one() {
        // `autm_campaign` has a tracker's name inside it but is not one; a
        // genuine `utm_` spelling nobody has heard of still is.
        assert_eq!(
            normalize("https://example.invalid/a?autm_campaign=1&utm_something=2&keep=3")
                .unwrap()
                .as_str(),
            "https://example.invalid/a?autm_campaign=1&keep=3"
        );
    }

    #[test]
    fn something_that_is_not_an_address_normalizes_to_nothing() {
        assert_eq!(normalize("not a url"), None);
    }
}
