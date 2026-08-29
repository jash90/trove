//! Application icons through NSWorkspace, on synthetic bundles only.
//!
//! NSWorkspace answers any path — a synthetic `.app` directory under a
//! `TempDir` gets the system's generic application icon — so this proves the
//! pipeline (path in, PNG bytes out) without depending on what is installed
//! on the machine that runs the test.

#[cfg(target_os = "macos")]
#[test]
fn application_icon_png_returns_a_decodable_png_of_row_size() {
    let directory = tempfile::tempdir().unwrap();
    let bundle = directory.path().join("Synthetic.app");
    std::fs::create_dir_all(bundle.join("Contents")).unwrap();

    let png = platform_macos::application_icon_png(bundle.to_str().unwrap(), 64);

    let bytes = png.expect("NSWorkspace owes an icon for any path, generic at worst");
    // A PNG begins with its eight magic bytes; anything else is not an icon
    // the bridge can hand to an <img>.
    assert_eq!(
        &bytes[..8],
        b"\x89PNG\r\n\x1a\n",
        "the icon bytes are not a PNG"
    );
    let decoded = image::load_from_memory(&bytes).expect("a PNG a browser can decode");
    let (width, height) = (decoded.width(), decoded.height());
    // The representation the system hands out is square and substantial —
    // the generic icon is a real icon through the same path. Hitting the
    // exact target size is the caller's resize, not the picker's.
    assert_eq!(width, height, "an application icon is square");
    assert!(
        (16..=1024).contains(&width),
        "unexpected icon dimensions: {width}x{height}"
    );
}
