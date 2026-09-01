//! Reading the macOS general pasteboard.
//!
//! Only this module talks to AppKit. Everything it returns is owned Rust data,
//! so no Objective-C object escapes into the rest of the application and the
//! whole crate above this line stays testable without a clipboard.

use trove_core::{
    ClipboardSnapshot, ContentKind, PlatformError, RepresentationInput, SourceConfidence,
};

use crate::markers;

/// Largest payload copied out of the pasteboard.
///
/// Above this the item is recorded by type and size alone. A clipboard is a
/// place people paste hundred-megabyte images through; reading one on every
/// change would stall the poll loop and blow the store's payload budget.
pub const MAX_CAPTURED_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

const TEXT_TYPE: &str = "public.utf8-plain-text";
const HTML_TYPE: &str = "public.html";
const PNG_TYPE: &str = "public.png";
const TIFF_TYPE: &str = "public.tiff";
const FILE_URL_TYPE: &str = "public.file-url";
const URL_TYPE: &str = "public.url";

/// What one poll of the pasteboard found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PollOutcome {
    /// Nothing has been copied since the last look.
    Unchanged,
    /// Something was copied but must not be recorded.
    Rejected {
        change_count: i64,
        reason: &'static str,
    },
    /// Something was copied and may be recorded.
    Captured {
        change_count: i64,
        snapshot: Box<ClipboardSnapshot>,
    },
}

/// Builds a snapshot from an already-read set of pasteboard representations.
///
/// Split from the AppKit call so the mapping — which type wins, how a URL
/// becomes a link, what happens when nothing readable is there — can be tested
/// on plain data.
pub fn snapshot_from_types(
    types: &[String],
    read: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
    source_app: Option<(String, String)>,
    observed_at_ms: i64,
) -> Result<ClipboardSnapshot, PlatformError> {
    if markers::classify_types(types).is_rejected() {
        return Err(PlatformError::ReadFailed);
    }

    let mut representations = Vec::new();
    let mut kind = ContentKind::Text;
    let mut primary_mime = "text/plain".to_owned();

    if types
        .iter()
        .any(|value| value == PNG_TYPE || value == TIFF_TYPE)
    {
        let image_type = if types.iter().any(|value| value == PNG_TYPE) {
            PNG_TYPE
        } else {
            TIFF_TYPE
        };
        let mime = if image_type == PNG_TYPE {
            "image/png"
        } else {
            "image/tiff"
        };
        let bytes = read(image_type).filter(|bytes| bytes.len() <= MAX_CAPTURED_PAYLOAD_BYTES);
        if let Some(bytes) = bytes {
            kind = ContentKind::Image;
            primary_mime = mime.to_owned();
            representations.push(RepresentationInput {
                format_id: mime.to_owned(),
                bytes: Some(bytes),
                missing_ref: None,
            });
        }
    }

    if representations.is_empty() {
        let text_type = types
            .iter()
            .find(|value| value.as_str() == TEXT_TYPE)
            .or_else(|| types.iter().find(|value| value.as_str() == FILE_URL_TYPE))
            .or_else(|| types.iter().find(|value| value.as_str() == URL_TYPE));
        let Some(text_type) = text_type else {
            return Err(PlatformError::ReadFailed);
        };
        let bytes = read(text_type)
            .filter(|bytes| !bytes.is_empty() && bytes.len() <= MAX_CAPTURED_PAYLOAD_BYTES)
            .ok_or(PlatformError::ReadFailed)?;
        let text = String::from_utf8(bytes).map_err(|_| PlatformError::ReadFailed)?;
        kind = classify_text(text_type.as_str(), &text);
        primary_mime = match kind {
            ContentKind::Link | ContentKind::File => "text/uri-list".to_owned(),
            _ => "text/plain".to_owned(),
        };
        representations.push(RepresentationInput {
            format_id: primary_mime.clone(),
            bytes: Some(text.into_bytes()),
            missing_ref: None,
        });
    }

    // Rich text rides along so a paste keeps its formatting, but only when it
    // is small enough to be worth storing beside the plain text.
    if kind == ContentKind::Text
        && types.iter().any(|value| value == HTML_TYPE)
        && let Some(html) =
            read(HTML_TYPE).filter(|bytes| bytes.len() <= MAX_CAPTURED_PAYLOAD_BYTES)
    {
        representations.push(RepresentationInput {
            format_id: "text/html".to_owned(),
            bytes: Some(html),
            missing_ref: None,
        });
    }

    let (source_app_id, source_app_name, source_confidence) = match source_app {
        Some((id, name)) => (Some(id), Some(name), SourceConfidence::Inferred),
        None => (None, None, SourceConfidence::Unknown),
    };

    Ok(ClipboardSnapshot {
        kind,
        primary_mime,
        representations,
        source_app_id,
        source_app_name,
        source_confidence,
        observed_at_ms,
    })
}

/// Tells a link and a file reference apart from ordinary text.
fn classify_text(pasteboard_type: &str, value: &str) -> ContentKind {
    if pasteboard_type == FILE_URL_TYPE || value.starts_with("file://") {
        return ContentKind::File;
    }
    let trimmed = value.trim();
    if pasteboard_type == URL_TYPE
        || ((trimmed.starts_with("http://") || trimmed.starts_with("https://"))
            && !trimmed.contains(char::is_whitespace))
    {
        return ContentKind::Link;
    }
    ContentKind::Text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reader(entries: Vec<(&'static str, Vec<u8>)>) -> impl FnMut(&str) -> Option<Vec<u8>> {
        move |wanted: &str| {
            entries
                .iter()
                .find(|(name, _)| *name == wanted)
                .map(|(_, bytes)| bytes.clone())
        }
    }

    #[test]
    fn plain_text_becomes_a_text_snapshot() {
        let mut read = reader(vec![(TEXT_TYPE, b"skopiowany tekst".to_vec())]);
        let snapshot =
            snapshot_from_types(&[TEXT_TYPE.to_owned()], &mut read, None, 1_725_000_000_000)
                .unwrap();

        assert_eq!(snapshot.kind, ContentKind::Text);
        assert_eq!(snapshot.primary_mime, "text/plain");
        assert_eq!(snapshot.representations.len(), 1);
        assert_eq!(snapshot.source_confidence, SourceConfidence::Unknown);
    }

    #[test]
    fn a_url_becomes_a_link_and_a_file_url_becomes_a_file() {
        let mut read = reader(vec![(TEXT_TYPE, b"https://example.invalid/a".to_vec())]);
        let link = snapshot_from_types(&[TEXT_TYPE.to_owned()], &mut read, None, 1).unwrap();
        assert_eq!(link.kind, ContentKind::Link);

        let mut read = reader(vec![(FILE_URL_TYPE, b"file:///tmp/a.pdf".to_vec())]);
        let file = snapshot_from_types(&[FILE_URL_TYPE.to_owned()], &mut read, None, 1).unwrap();
        assert_eq!(file.kind, ContentKind::File);
    }

    #[test]
    fn text_that_merely_mentions_a_url_stays_text() {
        let mut read = reader(vec![(
            TEXT_TYPE,
            b"see https://example.invalid/a for details".to_vec(),
        )]);
        let snapshot = snapshot_from_types(&[TEXT_TYPE.to_owned()], &mut read, None, 1).unwrap();

        assert_eq!(snapshot.kind, ContentKind::Text);
    }

    #[test]
    fn an_image_wins_over_the_text_the_application_also_offered() {
        let mut read = reader(vec![
            (PNG_TYPE, vec![1, 2, 3]),
            (TEXT_TYPE, b"Image (10x10)".to_vec()),
        ]);
        let snapshot = snapshot_from_types(
            &[PNG_TYPE.to_owned(), TEXT_TYPE.to_owned()],
            &mut read,
            None,
            1,
        )
        .unwrap();

        assert_eq!(snapshot.kind, ContentKind::Image);
        assert_eq!(snapshot.primary_mime, "image/png");
    }

    #[test]
    fn rich_text_rides_along_with_plain_text() {
        let mut read = reader(vec![
            (TEXT_TYPE, b"tekst".to_vec()),
            (HTML_TYPE, b"<p>tekst</p>".to_vec()),
        ]);
        let snapshot = snapshot_from_types(
            &[TEXT_TYPE.to_owned(), HTML_TYPE.to_owned()],
            &mut read,
            None,
            1,
        )
        .unwrap();

        assert_eq!(snapshot.representations.len(), 2);
        assert_eq!(snapshot.representations[1].format_id, "text/html");
    }

    #[test]
    fn a_concealed_item_is_refused_before_anything_is_read() {
        let mut reads = 0_usize;
        let mut read = |_: &str| {
            reads += 1;
            Some(b"sekret".to_vec())
        };
        let result = snapshot_from_types(
            &[TEXT_TYPE.to_owned(), markers::CONCEALED_TYPE.to_owned()],
            &mut read,
            None,
            1,
        );

        assert!(result.is_err());
        assert_eq!(reads, 0, "a secret must not even be read");
    }

    #[test]
    fn an_oversized_payload_is_refused_rather_than_stored() {
        let mut read = reader(vec![(
            TEXT_TYPE,
            vec![b'a'; MAX_CAPTURED_PAYLOAD_BYTES + 1],
        )]);

        assert!(snapshot_from_types(&[TEXT_TYPE.to_owned()], &mut read, None, 1).is_err());
    }

    #[test]
    fn a_named_source_application_is_recorded_as_inferred() {
        let mut read = reader(vec![(TEXT_TYPE, b"tekst".to_vec())]);
        let snapshot = snapshot_from_types(
            &[TEXT_TYPE.to_owned()],
            &mut read,
            Some(("com.example.app".to_owned(), "Example".to_owned())),
            1,
        )
        .unwrap();

        // The frontmost application is a guess, not a declaration: the item
        // itself did not say where it came from.
        assert_eq!(snapshot.source_confidence, SourceConfidence::Inferred);
        assert_eq!(snapshot.source_app_name.as_deref(), Some("Example"));
    }
}
