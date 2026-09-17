#![forbid(unsafe_code)]

mod thumbnail;

pub use thumbnail::{
    ImageError, MAX_IMAGE_DIMENSION, MAX_IMAGE_INPUT_BYTES, MAX_THUMBNAIL_DIMENSION,
    MAX_THUMBNAIL_PIXELS, make_thumbnail,
};

mod decode;

pub use decode::decode_rgba;
