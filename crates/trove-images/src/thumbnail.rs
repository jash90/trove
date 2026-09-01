use std::{io::Cursor, sync::Arc, sync::OnceLock};

use image::{ImageFormat, ImageReader, Limits, imageops::FilterType};
use resvg::{tiny_skia, usvg};
use thiserror::Error;

pub const MAX_IMAGE_INPUT_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_THUMBNAIL_DIMENSION: u32 = 320;
pub const MAX_THUMBNAIL_PIXELS: u32 = MAX_THUMBNAIL_DIMENSION * MAX_THUMBNAIL_DIMENSION;
pub const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_IMAGE_ALLOCATION_BYTES: u64 = 128 * 1024 * 1024;

/// How much of a document is examined to decide whether it is SVG.
///
/// Long enough for a byte-order mark, an XML declaration and a comment or two
/// before the root element; short enough that the question is answered without
/// reading the file.
const SVG_SNIFF_BYTES: usize = 1024;

#[derive(Debug, Error)]
pub enum ImageError {
    #[error("image input is {actual} bytes, exceeding the {max} byte limit")]
    InputTooLarge { actual: usize, max: usize },
    #[error("thumbnail size must be non-zero")]
    InvalidThumbnailSize,
    #[error("thumbnail_output_too_large")]
    OutputTooLarge,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Image(#[from] image::ImageError),
    #[error("svg could not be read")]
    InvalidSvg,
    #[error("svg could not be drawn")]
    SvgRenderFailed,
}

pub fn make_thumbnail(bytes: &[u8], size: u32) -> Result<Vec<u8>, ImageError> {
    if size == 0 {
        return Err(ImageError::InvalidThumbnailSize);
    }
    if size > MAX_THUMBNAIL_DIMENSION || size.saturating_mul(size) > MAX_THUMBNAIL_PIXELS {
        return Err(ImageError::OutputTooLarge);
    }
    if bytes.len() > MAX_IMAGE_INPUT_BYTES {
        return Err(ImageError::InputTooLarge {
            actual: bytes.len(),
            max: MAX_IMAGE_INPUT_BYTES,
        });
    }
    // Decided from the bytes in hand, not from a type a server claimed. A page
    // that mislabels its own picture should still get one.
    if looks_like_svg(bytes) {
        return rasterise_svg(bytes, size);
    }
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_ALLOCATION_BYTES);
    reader.limits(limits);
    let image = reader.decode()?;
    // Fitted, not filled. `resize_to_fill` covers the square and crops the
    // overhang, which on a page's own card — 1200x600 as most sites serve it —
    // discarded half the width before anything could display it.
    let thumbnail = image.resize(size, size, FilterType::Lanczos3);
    let mut output = Cursor::new(Vec::new());
    thumbnail.write_to(&mut output, ImageFormat::Png)?;
    Ok(output.into_inner())
}

/// Whether a document is an SVG, judged from a bounded prefix.
fn looks_like_svg(bytes: &[u8]) -> bool {
    let prefix = &bytes[..bytes.len().min(SVG_SNIFF_BYTES)];
    let text = String::from_utf8_lossy(prefix);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if trimmed.starts_with("<svg") {
        return true;
    }
    // An XML declaration, a doctype or a comment may come first; the root
    // element still has to be an `svg` within the prefix examined.
    (trimmed.starts_with("<?xml") || trimmed.starts_with("<!")) && trimmed.contains("<svg")
}

/// Draws an SVG into a bounded box and encodes it as PNG.
///
/// The *bound* is ours, never the document's: an SVG may declare any dimensions
/// it likes, and a document claiming a hundred thousand pixels a side would ask
/// for an allocation that does not exist. Within that bound the document keeps
/// its own proportions — a wide logo comes out wide rather than as a square
/// with transparent margins nobody asked for.
fn rasterise_svg(bytes: &[u8], size: u32) -> Result<Vec<u8>, ImageError> {
    let tree = usvg::Tree::from_data(bytes, &svg_options()).map_err(|_| ImageError::InvalidSvg)?;

    let declared = tree.size();
    let scale = (size as f32 / declared.width()).min(size as f32 / declared.height());
    // The longer side lands on `size` exactly; the shorter one is whatever the
    // document's shape makes it, and never zero.
    let width = ((declared.width() * scale).round() as u32).clamp(1, size);
    let height = ((declared.height() * scale).round() as u32).clamp(1, size);

    let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or(ImageError::OutputTooLarge)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().map_err(|_| ImageError::SvgRenderFailed)
}

/// The parsing options every SVG is read under.
///
/// The important part is what is switched off. Left at its default, `usvg`
/// resolves an `href` inside the document **by reading that path off this
/// machine's disk** — measured, not assumed: with the default resolver the test
/// below draws a file it was pointed at. An SVG arrives here from whatever page
/// nominated it, so that is a stranger's document naming a local path and
/// getting its contents back as a picture. Nothing outside the document is
/// loaded, ever.
fn svg_options() -> usvg::Options<'static> {
    let mut options = usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: usvg::ImageHrefResolver::default_data_resolver(),
            resolve_string: Box::new(|_href, _options| None),
        },
        ..usvg::Options::default()
    };
    options.fontdb = Arc::clone(system_fonts());
    options
}

/// The system font database, read once.
///
/// Text in an SVG needs real fonts — a logo rendered without its words is not
/// the logo. Enumerating them costs time and touches the filesystem, so it
/// happens on the first SVG anyone looks at rather than at startup.
fn system_fonts() -> &'static Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut database = usvg::fontdb::Database::new();
        database.load_system_fonts();
        Arc::new(database)
    })
}
