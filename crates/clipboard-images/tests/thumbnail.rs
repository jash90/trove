use std::io::Cursor;

use clipboard_images::{
    ImageError, MAX_IMAGE_INPUT_BYTES, MAX_THUMBNAIL_DIMENSION, MAX_THUMBNAIL_PIXELS,
    make_thumbnail,
};

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
    assert_eq!(MAX_THUMBNAIL_DIMENSION, 320);
    assert_eq!(MAX_THUMBNAIL_PIXELS, 320 * 320);
    let thumbnail = make_thumbnail(ONE_PIXEL_PNG, MAX_THUMBNAIL_DIMENSION).unwrap();

    assert_eq!(&thumbnail[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(&thumbnail[16..20], 320_u32.to_be_bytes());
    assert_eq!(&thumbnail[20..24], 320_u32.to_be_bytes());
}

#[test]
fn thumbnail_rejects_321_before_probing_invalid_input() {
    let error = make_thumbnail(b"not an image", MAX_THUMBNAIL_DIMENSION + 1).unwrap_err();

    assert!(matches!(error, ImageError::OutputTooLarge));
    assert_eq!(error.to_string(), "thumbnail_output_too_large");
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

/// A red square, the smallest thing that proves drawing happened.
const RED_SQUARE_SVG: &[u8] =
    br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="red"/></svg>"#;

/// Reads the width and height a PNG declares in its header.
fn png_dimensions(png: &[u8]) -> (u32, u32) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    (
        u32::from_be_bytes(png[16..20].try_into().unwrap()),
        u32::from_be_bytes(png[20..24].try_into().unwrap()),
    )
}

/// Whether any pixel was drawn at all.
fn has_visible_pixels(png: &[u8]) -> bool {
    let decoded = image::load_from_memory(png).unwrap().to_rgba8();
    decoded.pixels().any(|pixel| pixel.0[3] != 0)
}

#[test]
fn an_svg_becomes_a_png_thumbnail() {
    let thumbnail = make_thumbnail(RED_SQUARE_SVG, MAX_THUMBNAIL_DIMENSION).unwrap();

    assert_eq!(png_dimensions(&thumbnail), (320, 320));
    assert!(has_visible_pixels(&thumbnail));
}

#[test]
fn an_svg_declaring_absurd_dimensions_still_produces_a_bounded_thumbnail() {
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="100000" height="100000"><rect width="100000" height="100000" fill="blue"/></svg>"#;

    let thumbnail = make_thumbnail(svg, MAX_THUMBNAIL_DIMENSION).unwrap();

    // The document asked for ten billion pixels. It gets ours.
    assert_eq!(png_dimensions(&thumbnail), (320, 320));
}

#[test]
fn an_svg_with_an_xml_declaration_is_recognised() {
    let svg = br#"<?xml version="1.0" encoding="UTF-8"?>
<!-- a leading comment -->
<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="10" height="10" fill="green"/></svg>"#;

    assert!(has_visible_pixels(
        &make_thumbnail(svg, MAX_THUMBNAIL_DIMENSION).unwrap()
    ));
}

#[test]
fn an_svg_over_the_input_limit_is_refused_before_parsing() {
    let mut svg = RED_SQUARE_SVG.to_vec();
    svg.resize(MAX_IMAGE_INPUT_BYTES + 1, b' ');

    assert!(matches!(
        make_thumbnail(&svg, MAX_THUMBNAIL_DIMENSION),
        Err(ImageError::InputTooLarge { .. })
    ));
}

#[test]
fn an_svg_expanding_entities_is_refused() {
    // "Billion laughs": each level multiplies the one below it. A parser that
    // expands this allocates gigabytes from a few hundred bytes.
    let svg = br#"<?xml version="1.0"?>
<!DOCTYPE svg [
  <!ENTITY a "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa">
  <!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;">
  <!ENTITY c "&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;">
  <!ENTITY d "&c;&c;&c;&c;&c;&c;&c;&c;&c;&c;">
  <!ENTITY e "&d;&d;&d;&d;&d;&d;&d;&d;&d;&d;">
  <!ENTITY f "&e;&e;&e;&e;&e;&e;&e;&e;&e;&e;">
  <!ENTITY g "&f;&f;&f;&f;&f;&f;&f;&f;&f;&f;">
]>
<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><text>&g;</text></svg>"#;

    assert!(matches!(
        make_thumbnail(svg, MAX_THUMBNAIL_DIMENSION),
        Err(ImageError::InvalidSvg)
    ));
}

#[test]
fn an_svg_referencing_a_remote_image_draws_nothing_and_asks_no_one() {
    use std::io::ErrorKind;
    use std::net::TcpListener;

    // `usvg` has no network path of its own — this holds it to that, so a
    // future version that grows one fails here rather than in the wild. The
    // reference that genuinely had to be closed is the local file below.
    // A real listener, so a request would be observable rather than assumed.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><image href="http://{address}/logo.png" width="10" height="10"/></svg>"#
    );

    let thumbnail = make_thumbnail(svg.as_bytes(), MAX_THUMBNAIL_DIMENSION).unwrap();

    assert!(
        !has_visible_pixels(&thumbnail),
        "an external reference must not be drawn"
    );
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == ErrorKind::WouldBlock),
        "rasterising an svg must not open a connection"
    );
}

#[test]
fn an_svg_referencing_a_local_file_draws_nothing() {
    // A real, decodable picture on disk. Pointing at something unreadable would
    // make this test pass whether or not the resolver was ever disabled.
    let directory = std::env::temp_dir().join("clipboard-images-svg-href");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("secret.png");
    let mut opaque = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
        8,
        8,
        image::Rgba([0, 255, 0, 255]),
    ))
    .write_to(&mut opaque, image::ImageFormat::Png)
    .unwrap();
    std::fs::write(&path, opaque.get_ref()).unwrap();
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><image href="{}" width="10" height="10"/></svg>"#,
        path.display()
    );

    let thumbnail = make_thumbnail(svg.as_bytes(), MAX_THUMBNAIL_DIMENSION).unwrap();

    assert!(
        !has_visible_pixels(&thumbnail),
        "an svg must not be able to read a file off this machine"
    );
    std::fs::remove_dir_all(&directory).ok();
}

#[test]
fn an_svg_embedding_its_own_image_still_draws_it() {
    // The counterweight to the two tests above: they would also pass if the
    // resolver were simply broken. Embedded data is meant to work.
    // Built here rather than reusing the constant above: this test turns on
    // whether a pixel is opaque, so the source has to be known opaque.
    let mut opaque = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
        1,
        1,
        image::Rgba([255, 0, 0, 255]),
    ))
    .write_to(&mut opaque, image::ImageFormat::Png)
    .unwrap();
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(opaque.get_ref());
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><image href="data:image/png;base64,{encoded}" width="10" height="10"/></svg>"#
    );

    let thumbnail = make_thumbnail(svg.as_bytes(), MAX_THUMBNAIL_DIMENSION).unwrap();

    assert!(has_visible_pixels(&thumbnail));
}
