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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepresentationInput {
    pub format_id: String,
    pub bytes: Option<Vec<u8>>,
    pub missing_ref: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
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

pub fn canonical_text_bytes(value: &str) -> Vec<u8> {
    use unicode_normalization::UnicodeNormalization;

    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .nfc()
        .collect::<String>()
        .into_bytes()
}

pub fn canonical_bytes(kind: ContentKind, bytes: &[u8]) -> Vec<u8> {
    if kind.is_textual()
        && let Ok(value) = std::str::from_utf8(bytes)
    {
        return canonical_text_bytes(value);
    }
    bytes.to_vec()
}

pub fn content_hash(kind: ContentKind, primary_mime: &str, bytes: &[u8]) -> ContentHash {
    let bytes = canonical_bytes(kind, bytes);
    let mut hasher = blake3::Hasher::new();
    hasher.update(kind.as_str().as_bytes());
    hasher.update(&[0]);
    hasher.update(primary_mime.as_bytes());
    hasher.update(&[0]);
    hasher.update(&bytes);
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{canonical_text_bytes, content_hash};

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
}
