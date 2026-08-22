use std::io::Cursor;

use clipboard_images::{ImageError, MAX_IMAGE_INPUT_BYTES, make_thumbnail};

const ONE_PIXEL_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

#[test]
fn thumbnail_rejects_oversized_input_before_decode() {
    let bytes = vec![0_u8; MAX_IMAGE_INPUT_BYTES + 1];

    assert!(matches!(
        make_thumbnail(&bytes, 320),
        Err(ImageError::InputTooLarge { .. })
    ));
}

#[test]
fn thumbnail_is_a_320_pixel_png() {
    let thumbnail = make_thumbnail(ONE_PIXEL_PNG, 320).unwrap();

    assert_eq!(&thumbnail[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(&thumbnail[16..20], 320_u32.to_be_bytes());
    assert_eq!(&thumbnail[20..24], 320_u32.to_be_bytes());
}

#[test]
fn thumbnail_rejects_images_over_the_decode_dimension_limit() {
    let source = image::DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
        16_385,
        1,
        image::Rgba([0, 0, 0, 255]),
    ));
    let mut bytes = Cursor::new(Vec::new());
    source
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();

    assert!(matches!(
        make_thumbnail(bytes.get_ref(), 320),
        Err(ImageError::Image(_))
    ));
}
