//! Application icons, straight from the workspace that owns them.

#[cfg(target_os = "macos")]
use objc2_app_kit::NSBitmapImageRep;

/// The PNG bytes of the icon macOS shows for `path`, sized for `target_px`.
///
/// NSWorkspace answers every path — a real bundle gets its own artwork, a
/// synthetic or vanished one gets the generic application icon — so the
/// caller decides what "no icon" means; this only reports what the system
/// would draw. The conversion to PNG happens entirely on the Apple side,
/// through the `NSBitmapImageRep` AppKit itself offers: real application
/// icons are multi-representation images whose TIFF a third-party decoder
/// cannot be trusted to read back, and the representation the system hands
/// out needs no trust at all — only a pick and an encode.
#[cfg(target_os = "macos")]
pub fn application_icon_png(path: &str, target_px: usize) -> Option<Vec<u8>> {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSSize, NSString};

    let workspace = NSWorkspace::sharedWorkspace();
    let file = NSString::from_str(path);
    let image = workspace.iconForFile(&file);

    // An icon carries several bitmap representations. Which one to draw from
    // is a fact about widths, so it is settled first and the representation
    // is only borrowed again to encode: the closest one at or above the
    // target keeps the row sharp without shipping 1024-pixel art, and when
    // every representation sits below the target the largest wins — a
    // browser scales down more kindly than up.
    let target = target_px.max(16) as isize;
    let representations = image.representations();
    let chosen_width = representations
        .iter()
        .filter_map(|rep| {
            let bitmap = rep.downcast_ref::<NSBitmapImageRep>()?;
            Some(bitmap.pixelsWide())
        })
        .fold(None, |best, wide| match best {
            None => Some(wide),
            Some(current) => Some(match (wide >= target, current >= target) {
                (true, false) => wide,
                (true, true) => wide.min(current),
                (false, true) => current,
                (false, false) => wide.max(current),
            }),
        });

    // Draw from the chosen representation. `setSize` cannot resample what
    // the icon already carries — the generic icon, for one, holds a single
    // 1024-pixel slab — so the exact target size is the caller's resize to
    // make, on this PNG rather than on Apple's TIFF.
    if let Some(chosen) = chosen_width {
        for rep in representations.iter() {
            let Some(bitmap) = rep.downcast_ref::<NSBitmapImageRep>() else {
                continue;
            };
            if bitmap.pixelsWide() == chosen {
                return encode_png(bitmap);
            }
        }
    }

    // No bitmap representation to draw from: rasterise the whole image at
    // the target size and take the representation AppKit produces for it.
    image.setSize(NSSize::new(target_px as f64, target_px as f64));
    let tiff = image.TIFFRepresentation()?;
    let rasterised = NSBitmapImageRep::imageRepWithData(&tiff)?;
    encode_png(&rasterised)
}

/// Asks a bitmap representation for its PNG bytes.
///
/// The one place this crate touches `unsafe`: the call's contract is the
/// dictionary's typing, and an empty dictionary selects the default PNG
/// encoding.
#[cfg(target_os = "macos")]
fn encode_png(bitmap: &NSBitmapImageRep) -> Option<Vec<u8>> {
    use objc2_app_kit::NSBitmapImageFileType;
    use objc2_foundation::NSDictionary;

    let png = unsafe {
        bitmap
            .representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())?
    };
    Some(png.to_vec())
}
