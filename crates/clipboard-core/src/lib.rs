#![forbid(unsafe_code)]

mod error;
mod model;
mod normalize;

pub use error::CoreError;
pub use model::{
    CaptureInput, ContentFlags, ContentHash, ContentKind, EventFlags, MAX_CANONICAL_NONSTARTERS,
    MAX_CANONICAL_NORMALIZATION_HEAP_BYTES, RepresentationInput, SourceConfidence,
    canonical_byte_len, canonical_bytes, canonical_text_bytes, content_hash,
    normalize_search_text_bounded, update_canonical_bytes,
};
pub use normalize::normalize_search_text;
