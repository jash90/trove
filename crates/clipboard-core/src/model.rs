use std::fmt;

use serde::{Deserialize, Serialize};

pub type ContentHash = [u8; 32];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Text,
    Link,
    Image,
    File,
    Color,
    Code,
    Html,
}

impl ContentKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Link => "link",
            Self::Image => "image",
            Self::File => "file",
            Self::Color => "color",
            Self::Code => "code",
            Self::Html => "html",
        }
    }

    pub const fn is_textual(self) -> bool {
        matches!(
            self,
            Self::Text | Self::Link | Self::Color | Self::Code | Self::Html
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceConfidence {
    Declared,
    Inferred,
    Unknown,
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
    pub struct ContentFlags: u32 {
        const SENSITIVE = 1 << 0;
        const DO_NOT_INDEX = 1 << 1;
        const MISSING_PAYLOAD = 1 << 2;
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
    pub struct EventFlags: u32 {
        const IMPORTED = 1 << 0;
        const LOCAL_ONLY = 1 << 1;
        const NEVER_SYNC = 1 << 2;
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RepresentationInput {
    pub format_id: String,
    pub bytes: Option<Vec<u8>>,
    pub missing_ref: Option<String>,
}

impl fmt::Debug for RepresentationInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RepresentationInput")
            .field("has_payload", &self.bytes.is_some())
            .field(
                "byte_size",
                &self.bytes.as_ref().map_or(0, std::vec::Vec::len),
            )
            .field("missing", &self.missing_ref.is_some())
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct CaptureInput {
    pub captured_at_ms: i64,
    pub kind: ContentKind,
    pub primary_mime: String,
    pub representations: Vec<RepresentationInput>,
    pub source_app_id: Option<String>,
    pub source_app_name: Option<String>,
    pub source_confidence: SourceConfidence,
    pub pinned: bool,
    pub occurrence_count: u32,
    pub content_flags: ContentFlags,
    pub event_flags: EventFlags,
}

impl fmt::Debug for CaptureInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CaptureInput")
            .field("kind", &self.kind)
            .field("representation_count", &self.representations.len())
            .field("source_confidence", &self.source_confidence)
            .field("pinned", &self.pinned)
            .field("occurrence_count", &self.occurrence_count)
            .field("content_flags", &self.content_flags)
            .field("event_flags", &self.event_flags)
            .finish()
    }
}

pub fn canonical_text_bytes(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut canonical = Vec::with_capacity(canonical_byte_len(ContentKind::Text, bytes));
    update_canonical_bytes(ContentKind::Text, bytes, |chunk| {
        canonical.extend_from_slice(chunk);
    });
    canonical
}

pub fn canonical_bytes(kind: ContentKind, bytes: &[u8]) -> Vec<u8> {
    let mut canonical = Vec::with_capacity(canonical_byte_len(kind, bytes));
    update_canonical_bytes(kind, bytes, |chunk| canonical.extend_from_slice(chunk));
    canonical
}

pub fn canonical_byte_len(kind: ContentKind, bytes: &[u8]) -> usize {
    let mut byte_len = 0_usize;
    update_canonical_bytes(kind, bytes, |chunk| {
        byte_len = byte_len.saturating_add(chunk.len());
    });
    byte_len
}

pub fn update_canonical_bytes(kind: ContentKind, bytes: &[u8], mut update: impl FnMut(&[u8])) {
    use unicode_normalization::UnicodeNormalization;

    let Some(value) = kind
        .is_textual()
        .then(|| std::str::from_utf8(bytes).ok())
        .flatten()
    else {
        update(bytes);
        return;
    };
    let mut previous_was_cr = false;
    let newline_normalized = value.chars().filter_map(move |character| {
        if character == '\n' && previous_was_cr {
            previous_was_cr = false;
            return None;
        }
        previous_was_cr = character == '\r';
        Some(if character == '\r' { '\n' } else { character })
    });
    let mut encoded = [0_u8; 4];
    for character in newline_normalized.nfc() {
        update(character.encode_utf8(&mut encoded).as_bytes());
    }
}

/// Normalizes a UTF-8 search prefix without ever growing the result beyond `max_bytes`.
pub fn normalize_search_text_bounded(value: &str, max_bytes: usize) -> String {
    use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

    let mut normalized = String::with_capacity(max_bytes);
    let lowered = value
        .chars()
        .flat_map(char::to_lowercase)
        .map(|character| if character == 'ł' { 'l' } else { character });
    for character in lowered
        .nfd()
        .filter(|character| !is_combining_mark(*character))
    {
        if normalized
            .len()
            .checked_add(character.len_utf8())
            .is_none_or(|length| length > max_bytes)
        {
            break;
        }
        normalized.push(character);
    }
    normalized
}

pub fn content_hash(kind: ContentKind, primary_mime: &str, bytes: &[u8]) -> ContentHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kind.as_str().as_bytes());
    hasher.update(&[0]);
    hasher.update(primary_mime.as_bytes());
    hasher.update(&[0]);
    update_canonical_bytes(kind, bytes, |chunk| {
        hasher.update(chunk);
    });
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        canonical_byte_len, canonical_text_bytes, content_hash, normalize_search_text_bounded,
        update_canonical_bytes,
    };

    #[test]
    fn canonical_streaming_reports_length_before_emitting_matching_chunks() {
        let input = "e\u{301}\r\nbrace { and text".as_bytes();
        let expected = "é\nbrace { and text".as_bytes();
        let mut streamed = Vec::new();

        let byte_len = canonical_byte_len(ContentKind::Text, input);
        update_canonical_bytes(ContentKind::Text, input, |chunk| {
            streamed.extend_from_slice(chunk);
        });

        assert_eq!(byte_len, expected.len());
        assert_eq!(streamed, expected);
    }

    #[test]
    fn canonical_text_normalizes_newlines_but_preserves_spaces() {
        assert_eq!(canonical_text_bytes("a\r\n b  "), b"a\n b  ");
    }

    #[test]
    fn canonical_bytes_normalizes_textual_nfc_and_newlines() {
        assert_eq!(
            canonical_bytes(ContentKind::Text, "e\u{301}\r\n b  ".as_bytes()),
            "é\n b  ".as_bytes(),
        );
    }

    #[test]
    fn canonical_bytes_preserves_non_textual_bytes() {
        let payload = [0x00, 0xff, 0x0d, 0x0a];
        assert_eq!(canonical_bytes(ContentKind::Image, &payload), payload);
    }

    #[test]
    fn canonical_bytes_preserves_invalid_utf8() {
        let payload = [0x66, 0x80, 0x0d, 0x0a];
        assert_eq!(canonical_bytes(ContentKind::Text, &payload), payload);
    }

    #[test]
    fn equal_canonical_payloads_have_equal_blake3_hashes() {
        assert_eq!(
            content_hash(ContentKind::Text, "text/plain", b"a\n"),
            content_hash(
                ContentKind::Text,
                "text/plain",
                canonical_text_bytes("a\r\n").as_slice()
            ),
        );
    }

    #[test]
    fn bounded_search_normalization_never_grows_past_its_preallocated_capacity() {
        let normalized = normalize_search_text_bounded("ŁÓDŹ ÉÉÉ", 7);

        assert_eq!(normalized, "lodz ee");
        assert!(normalized.len() <= 7);
        assert!(normalized.capacity() <= 7);
    }

    #[test]
    fn capture_debug_omits_payloads_missing_references_and_raw_source_apps() {
        let payload = b"sentinel private payload".to_vec();
        let missing_reference = "sentinel-private-filename.png";
        let source_id = "com.private.sentinel";
        let source_name = "Private Sentinel App";
        let input = CaptureInput {
            captured_at_ms: 1_000,
            kind: ContentKind::Image,
            primary_mime: "image/png".to_owned(),
            representations: vec![RepresentationInput {
                format_id: "image/png".to_owned(),
                bytes: Some(payload.clone()),
                missing_ref: Some(missing_reference.to_owned()),
            }],
            source_app_id: Some(source_id.to_owned()),
            source_app_name: Some(source_name.to_owned()),
            source_confidence: SourceConfidence::Declared,
            pinned: false,
            occurrence_count: 1,
            content_flags: ContentFlags::empty(),
            event_flags: EventFlags::empty(),
        };

        let representation_debug = format!("{:?}", input.representations[0]);
        let capture_debug = format!("{input:?}");
        let byte_debug = format!("{payload:?}");
        for rendered in [&representation_debug, &capture_debug] {
            assert!(!rendered.contains(&byte_debug));
            assert!(!rendered.contains(missing_reference));
            assert!(!rendered.contains(source_id));
            assert!(!rendered.contains(source_name));
        }
        assert!(representation_debug.contains("byte_size: 24"));
        assert!(capture_debug.contains("representation_count: 1"));
    }
}
