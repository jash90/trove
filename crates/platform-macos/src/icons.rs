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
/// representation is resolution-independent — it draws itself on demand
/// and holds no pixels to pick from. So it is *drawn*, into a bitmap of
/// exactly the side the row asked for: the context is created around that
/// bitmap, and the image is asked to fill its rectangle. Drawing (rather
/// than asking a rectangle for a CGImage, which consults the main
/// screen's backing scale when no context is given — and doubles the
/// pixels on a Retina session) keeps the size a contract of this
/// function, not a fact about whoever's display is frontmost. The encode
/// below writes Apple's pixels unchanged: Apple's icons are the one
/// picture no third-party decoder should be handed in its original form.
#[cfg(target_os = "macos")]
pub fn application_icon_png(path: &str, target_px: usize) -> Option<Vec<u8>> {
    use objc2::AnyThread;
    use objc2_app_kit::{NSBitmapImageRep, NSCompositingOperation, NSGraphicsContext, NSWorkspace};
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    let side = target_px.max(SMALLEST_ICON_PX);

    let workspace = NSWorkspace::sharedWorkspace();
    let file = NSString::from_str(path);
    let image = workspace.iconForFile(&file);

    unsafe {
        // A bitmap of exactly `side × side`, allocated here: the rectangle
        // the icon is drawn into and the pixels that come back are the same
        // thing, whatever the display in front happens to weigh.
        let bitmap =
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                std::ptr::null_mut(),
                side as isize,
                side as isize,
                8,
                4,
                true,
                false,
                objc2_app_kit::NSDeviceRGBColorSpace,
                0,
                0,
            )?;
        let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap)?;
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&context));
        let rect = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(side as f64, side as f64),
        );
        // The whole image (a zero source rectangle means all of it), copied
        // into the whole bitmap, respecting the context's flipped origin so
        // the icon does not arrive upside down.
        image.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
            rect,
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)),
            NSCompositingOperation::Copy,
            1.0,
            true,
            None,
        );
        NSGraphicsContext::restoreGraphicsState_class();
        encode_png(&bitmap)
    }
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
