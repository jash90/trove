//! Application icons, straight from the workspace that owns them.

#[cfg(target_os = "macos")]
use objc2_app_kit::NSBitmapImageRep;

/// The smallest side an icon is drawn at. Asking for less gets this: the
/// artwork holds no representation below it and AppKit will not invent one.
#[cfg(target_os = "macos")]
const SMALLEST_ICON_PX: usize = 16;

/// The PNG bytes of the icon macOS shows for `path`, rendered at `target_px`
/// — or at [`SMALLEST_ICON_PX`] when less than that is asked for.
///
/// NSWorkspace answers every path — a real bundle gets its own artwork, a
/// synthetic or vanished one gets the generic application icon — so the
/// caller decides what "no icon" means; this only reports what the system
/// would draw.
///
/// An icon is not a bitmap. `iconForFile:` hands back an image whose every
/// representation is an `NSISIconImageRep`: a resolution-independent stand-in
/// that draws itself on demand and holds no pixels to pick from. So the size
/// is *asked for* rather than chosen — AppKit is given the rectangle the row
/// wants and rasterises into it. The bitmap that comes back is wrapped once,
/// with no decode and no resample in between, and encoded as PNG on the Apple
/// side: Apple's icons are the one picture no third-party decoder should be
/// handed in its original form.
#[cfg(target_os = "macos")]
pub fn application_icon_png(path: &str, target_px: usize) -> Option<Vec<u8>> {
    use objc2::AnyThread;
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    let workspace = NSWorkspace::sharedWorkspace();
    let file = NSString::from_str(path);
    let image = workspace.iconForFile(&file);

    // The rectangle is in points and the reference context is absent, so
    // there is no backing scale factor to multiply it by: one point is one
    // pixel, and the bitmap arrives at exactly this side. The floor is
    // sixteen because AppKit's own is: an icon carries nothing smaller, and
    // a rectangle below it comes back at sixteen anyway. Saying so here
    // keeps the promise this function makes true.
    let side = target_px.max(SMALLEST_ICON_PX) as f64;
    let mut proposed = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(side, side));

    // SAFETY: `proposed` is a live local for the whole call, so the pointer
    // AppKit writes the rectangle it settled on back through stays valid; the
    // hints dictionary is absent, so it carries no element type to get wrong.
    let drawn =
        unsafe { image.CGImageForProposedRect_context_hints(&raw mut proposed, None, None) }?;

    // `initWithCGImage:` adopts the pixels it is handed rather than copying
    // or resampling them, so the representation's `pixelsWide`/`pixelsHigh`
    // are the ones just rendered and the encode below writes them unchanged.
    let bitmap = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &drawn);
    encode_png(&bitmap)
}

/// Asks a bitmap representation for its PNG bytes.
///
/// An empty dictionary selects the default PNG encoding; the call's contract
/// is that dictionary's typing, which is why it is `unsafe` and this is not.
#[cfg(target_os = "macos")]
fn encode_png(bitmap: &NSBitmapImageRep) -> Option<Vec<u8>> {
    use objc2_app_kit::NSBitmapImageFileType;
    use objc2_foundation::NSDictionary;

    // SAFETY: the properties dictionary is empty, so there is no key whose
    // value type could be mismatched against what AppKit expects.
    let png = unsafe {
        bitmap
            .representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())?
    };
    Some(png.to_vec())
}
