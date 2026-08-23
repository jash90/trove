use std::{fmt, mem::size_of};

use serde::{Deserialize, Serialize};

use crate::CoreError;

pub type ContentHash = [u8; 32];

/// Maximum canonical-decomposition non-starters accepted between starters.
pub const MAX_CANONICAL_NONSTARTERS: usize = 4_096;
/// Conservative peak heap allowance for the pinned NFC iterator's two `TinyVec` buffers.
pub const MAX_CANONICAL_NORMALIZATION_HEAP_BYTES: usize = 128 * 1024;

const MAX_CANONICAL_NORMALIZER_BUFFER_ITEMS: usize =
    (MAX_CANONICAL_NONSTARTERS + 1).next_power_of_two();
// Include both full buffers plus the old half-capacity decomposition buffer during growth.
const _: () = assert!(
    MAX_CANONICAL_NORMALIZER_BUFFER_ITEMS * (size_of::<(u8, char)>() + size_of::<char>())
        + (MAX_CANONICAL_NORMALIZER_BUFFER_ITEMS / 2) * size_of::<(u8, char)>()
        <= MAX_CANONICAL_NORMALIZATION_HEAP_BYTES
);

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
    /// A short label for an entry whose payload cannot speak for itself.
    ///
    /// A file or image entry has no readable primary payload, so the list would
    /// otherwise show hundreds of identical placeholder rows. The source
    /// application already knows a name for it — a filename, "Image (1290x849)"
    /// — and that name is what the user recognises. Ignored for textual kinds,
    /// which derive their preview from the payload itself.
    pub display_label: Option<String>,
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

pub fn canonical_text_bytes(value: &str) -> Result<Vec<u8>, CoreError> {
    let bytes = value.as_bytes();
    let mut canonical = Vec::with_capacity(canonical_byte_len(ContentKind::Text, bytes)?);
    update_canonical_bytes(ContentKind::Text, bytes, |chunk| {
        canonical.extend_from_slice(chunk);
    })?;
    Ok(canonical)
}

pub fn canonical_bytes(kind: ContentKind, bytes: &[u8]) -> Result<Vec<u8>, CoreError> {
    let mut canonical = Vec::with_capacity(canonical_byte_len(kind, bytes)?);
    update_canonical_bytes(kind, bytes, |chunk| canonical.extend_from_slice(chunk))?;
    Ok(canonical)
}

pub fn canonical_byte_len(kind: ContentKind, bytes: &[u8]) -> Result<usize, CoreError> {
    let mut byte_len = 0_usize;
    update_canonical_bytes(kind, bytes, |chunk| {
        byte_len = byte_len.saturating_add(chunk.len());
    })?;
    Ok(byte_len)
}

pub fn update_canonical_bytes(
    kind: ContentKind,
    bytes: &[u8],
    mut update: impl FnMut(&[u8]),
) -> Result<(), CoreError> {
    use unicode_normalization::UnicodeNormalization;

    let Some(value) = kind
        .is_textual()
        .then(|| std::str::from_utf8(bytes).ok())
        .flatten()
    else {
        update(bytes);
        return Ok(());
    };
    validate_canonical_combining_sequences(value)?;
    let newline_normalized = newline_normalized_chars(value);
    let mut encoded = [0_u8; 4];
    for character in newline_normalized.nfc() {
        update(character.encode_utf8(&mut encoded).as_bytes());
    }
    Ok(())
}

fn newline_normalized_chars(value: &str) -> impl Iterator<Item = char> + '_ {
    let mut previous_was_cr = false;
    value.chars().filter_map(move |character| {
        if character == '\n' && previous_was_cr {
            previous_was_cr = false;
            return None;
        }
        previous_was_cr = character == '\r';
        Some(if character == '\r' { '\n' } else { character })
    })
}

fn validate_canonical_combining_sequences(value: &str) -> Result<(), CoreError> {
    use unicode_normalization::char::{canonical_combining_class, decompose_canonical};

    let mut nonstarters = 0_usize;
    for character in newline_normalized_chars(value) {
        let mut exceeded = false;
        decompose_canonical(character, |decomposed| {
            if canonical_combining_class(decomposed) == 0 {
                nonstarters = 0;
            } else {
                nonstarters += 1;
                exceeded |= nonstarters > MAX_CANONICAL_NONSTARTERS;
            }
        });
        if exceeded {
            return Err(CoreError::CanonicalizationTooComplex);
        }
    }
    Ok(())
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

pub fn content_hash(
    kind: ContentKind,
    primary_mime: &str,
    bytes: &[u8],
) -> Result<ContentHash, CoreError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kind.as_str().as_bytes());
    hasher.update(&[0]);
    hasher.update(primary_mime.as_bytes());
    hasher.update(&[0]);
    update_canonical_bytes(kind, bytes, |chunk| {
        hasher.update(chunk);
    })?;
    Ok(*hasher.finalize().as_bytes())
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

        let byte_len = canonical_byte_len(ContentKind::Text, input).unwrap();
        update_canonical_bytes(ContentKind::Text, input, |chunk| {
            streamed.extend_from_slice(chunk);
        })
        .unwrap();

        assert_eq!(byte_len, expected.len());
        assert_eq!(streamed, expected);
    }

    #[test]
    fn canonical_text_normalizes_newlines_but_preserves_spaces() {
        assert_eq!(canonical_text_bytes("a\r\n b  ").unwrap(), b"a\n b  ");
    }

    #[test]
    fn canonical_bytes_normalizes_textual_nfc_and_newlines() {
        assert_eq!(
            canonical_bytes(ContentKind::Text, "e\u{301}\r\n b  ".as_bytes()).unwrap(),
            "é\n b  ".as_bytes(),
        );
    }

    #[test]
    fn canonical_bytes_preserves_non_textual_bytes() {
        let payload = [0x00, 0xff, 0x0d, 0x0a];
        assert_eq!(
            canonical_bytes(ContentKind::Image, &payload).unwrap(),
            payload
        );
    }

    #[test]
    fn canonical_bytes_preserves_invalid_utf8() {
        let payload = [0x66, 0x80, 0x0d, 0x0a];
        assert_eq!(
            canonical_bytes(ContentKind::Text, &payload).unwrap(),
            payload
        );
    }

    #[test]
    fn equal_canonical_payloads_have_equal_blake3_hashes() {
        assert_eq!(
            content_hash(ContentKind::Text, "text/plain", b"a\n").unwrap(),
            content_hash(
                ContentKind::Text,
                "text/plain",
                canonical_text_bytes("a\r\n").unwrap().as_slice()
            )
            .unwrap(),
        );
    }

    #[test]
    fn canonical_nfc_rejects_before_the_real_normalizer_can_exceed_its_heap_bound() {
        let accepted_input = format!("a{}", "\u{301}".repeat(4_096));
        let mut accepted = None;
        let accepted_allocations = allocation_counter::measure(|| {
            accepted = Some(canonical_byte_len(
                ContentKind::Text,
                accepted_input.as_bytes(),
            ));
        });

        assert_eq!(accepted.unwrap().unwrap(), accepted_input.len() - 1);
        assert!(
            accepted_allocations.bytes_max <= 128 * 1024,
            "the real NFC buffers exceeded the proved heap allowance: {accepted_allocations:?}"
        );
        assert_eq!(accepted_allocations.bytes_current, 0);

        let rejected_input = format!("a{}", "\u{301}".repeat(4_097));
        let mut rejected = None;
        let rejected_allocations = allocation_counter::measure(|| {
            rejected = Some(canonical_byte_len(
                ContentKind::Text,
                rejected_input.as_bytes(),
            ));
        });

        assert_eq!(
            rejected.unwrap().unwrap_err().to_string(),
            "canonicalization_too_complex"
        );
        assert_eq!(
            rejected_allocations.bytes_total, 0,
            "rejection happened after entering the allocating NFC path: {rejected_allocations:?}"
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
            display_label: None,
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
