#![forbid(unsafe_code)]

mod error;
mod model;
mod normalize;

pub use error::CoreError;
pub use model::{
    CaptureInput, ContentFlags, ContentHash, ContentKind, EventFlags, RepresentationInput,
    SourceConfidence, canonical_bytes, canonical_text_bytes, content_hash,
};
pub use normalize::normalize_search_text;
