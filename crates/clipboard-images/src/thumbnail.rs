use std::io::Cursor;

use image::{ImageFormat, ImageReader, Limits, imageops::FilterType};
use thiserror::Error;

pub const MAX_IMAGE_INPUT_BYTES: usize = 32 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_IMAGE_ALLOCATION_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum ImageError {
    #[error("image input is {actual} bytes, exceeding the {max} byte limit")]
    InputTooLarge { actual: usize, max: usize },
    #[error("thumbnail size must be non-zero")]
    InvalidThumbnailSize,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Image(#[from] image::ImageError),
}

pub fn make_thumbnail(bytes: &[u8], size: u32) -> Result<Vec<u8>, ImageError> {
    if bytes.len() > MAX_IMAGE_INPUT_BYTES {
        return Err(ImageError::InputTooLarge {
            actual: bytes.len(),
            max: MAX_IMAGE_INPUT_BYTES,
        });
    }
    if size == 0 {
        return Err(ImageError::InvalidThumbnailSize);
    }

    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_ALLOCATION_BYTES);
    reader.limits(limits);
    let image = reader.decode()?;
    let thumbnail = image.resize_to_fill(size, size, FilterType::Lanczos3);
    let mut output = Cursor::new(Vec::new());
    thumbnail.write_to(&mut output, ImageFormat::Png)?;
    Ok(output.into_inner())
}
