//! Application icons through NSWorkspace, on synthetic bundles only.
//!
//! NSWorkspace answers any path — a synthetic `.app` directory under a
//! `TempDir` gets the system's generic application icon — so these prove the
//! pipeline (path in, PNG bytes out, at the size that was asked for) without
//! depending on what is installed on the machine that runs them.

#[cfg(target_os = "macos")]
fn icon_of_a_synthetic_bundle(target_px: usize) -> Vec<u8> {
    let directory = tempfile::tempdir().unwrap();
    let bundle = directory.path().join("Synthetic.app");
    std::fs::create_dir_all(bundle.join("Contents")).unwrap();

    platform_macos::application_icon_png(bundle.to_str().unwrap(), target_px)
        .expect("NSWorkspace owes an icon for any path, generic at worst")
}

#[cfg(target_os = "macos")]
#[test]
fn application_icon_png_renders_at_the_row_size() {
    let bytes = icon_of_a_synthetic_bundle(64);

    // A PNG begins with its eight magic bytes; anything else is not an icon
    // the bridge can hand to an <img>.
    assert_eq!(
        &bytes[..8],
        b"\x89PNG\r\n\x1a\n",
        "the icon bytes are not a PNG"
    );
    let decoded = image::load_from_memory(&bytes).expect("a PNG a browser can decode");
    let (width, height) = (decoded.width(), decoded.height());
    // The rectangle handed to AppKit is in points with no reference context,
    // so one point is one pixel: what comes back is the size that was asked
    // for, not whatever slab the icon happens to carry.
    assert_eq!(
        (width, height),
        (64, 64),
        "unexpected icon dimensions: {width}x{height}"
    );
    // Sixty-four pixels of RGBA is sixteen kibibytes before compression, so
    // a payload near a megabyte can only be the full-size artwork untouched.
    assert!(
        bytes.len() < 64 * 1024,
        "an icon this size cannot weigh {} bytes",
        bytes.len()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn application_icon_png_honours_a_smaller_target() {
    let bytes = icon_of_a_synthetic_bundle(32);

    let decoded = image::load_from_memory(&bytes).expect("a PNG a browser can decode");
    assert_eq!(
        (decoded.width(), decoded.height()),
        (32, 32),
        "the target size is a contract, not a suggestion"
    );
}

/// Sixteen is where the contract stops being the caller's to set: an icon
/// carries no representation below it, and AppKit answers a smaller
/// rectangle with sixteen pixels rather than inventing eight. The floor is
/// documented rather than discovered, so it is pinned here too.
#[cfg(target_os = "macos")]
#[test]
fn application_icon_png_floors_a_target_below_the_smallest_icon() {
    let bytes = icon_of_a_synthetic_bundle(8);

    let decoded = image::load_from_memory(&bytes).expect("a PNG a browser can decode");
    assert_eq!(
        (decoded.width(), decoded.height()),
        (16, 16),
        "the floor the documentation promises is not the one the code keeps"
    );
}
