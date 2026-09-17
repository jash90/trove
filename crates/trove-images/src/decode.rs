#![forbid(unsafe_code)]

use crate::thumbnail::ImageError;

/// Decodes stored image bytes into the raw RGBA the clipboard takes.
///
/// The history keeps images as the PNG or TIFF bytes they were captured
/// with; putting one back on the clipboard needs pixel data and
/// dimensions, not the encoded file. Anything the decoder refuses is an
/// `ImageError` for the caller to translate — a payload that cannot be
/// decoded is a fact about those bytes, not a reason to pretend the
/// clipboard refused.
pub fn decode_rgba(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), ImageError> {
    let decoded = image::load_from_memory(bytes)?;
    use image::GenericImageView;
    let (width, height) = decoded.dimensions();
    let rgba = decoded.into_rgba8().into_raw();
    Ok((rgba, width, height))
}

#[cfg(test)]
mod tests {
    use super::decode_rgba;
    use image::{DynamicImage, RgbaImage};

    #[test]
    fn decodes_a_png_into_raw_pixels_and_dimensions() {
        let mut png = Vec::new();
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255])))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();

        let (rgba, width, height) = decode_rgba(&png).unwrap();

        assert_eq!((width, height), (3, 2));
        assert_eq!(rgba.len(), 3 * 2 * 4);
        assert_eq!(&rgba[..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn refuses_bytes_that_are_not_an_image() {
        assert!(decode_rgba(b"not an image at all").is_err());
    }
}
