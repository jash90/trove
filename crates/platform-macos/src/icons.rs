//! Application icons, straight from the workspace that owns them.

/// The TIFF bytes of the icon macOS shows for `path`.
///
/// NSWorkspace answers every path — a real bundle gets its own artwork, a
/// synthetic or vanished one gets the generic application icon — so the
/// caller decides what "no icon" means; this only reports what the system
/// would draw. TIFF is the interchange format NSImage speaks natively;
/// turning it into a small PNG for the bridge is pure-Rust work that
/// belongs to `clipboard-images`, not to an FFI boundary.
///
/// The export asks for LZW: the uncompressed TIFF of an icon — whose
/// artwork this macOS rasterises generously — measures in tens of
/// megabytes, and the pure-Rust decoder downstream is bounded at 32 MiB of
/// input by design. Compressed, the same bytes arrive at a few hundred
/// kilobytes without losing a pixel.
#[cfg(target_os = "macos")]
pub fn application_icon_tiff(path: &str) -> Option<Vec<u8>> {
    use objc2_app_kit::{NSTIFFCompression, NSWorkspace};
    use objc2_foundation::NSString;

    let workspace = NSWorkspace::sharedWorkspace();
    let file = NSString::from_str(path);
    let image = workspace.iconForFile(&file);
    let data = image.TIFFRepresentationUsingCompression_factor(NSTIFFCompression::LZW, 0.0)?;
    Some(data.to_vec())
}
