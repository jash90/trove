#![forbid(unsafe_code)]

mod thumbnail;

pub use thumbnail::{ImageError, MAX_IMAGE_INPUT_BYTES, make_thumbnail};
