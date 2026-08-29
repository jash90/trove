//! Application icons through NSWorkspace, on synthetic bundles only.
//!
//! NSWorkspace answers any path — a synthetic `.app` directory under a
//! `TempDir` gets the system's generic application icon — so this proves the
//! pipeline (path in, TIFF bytes out) without depending on what is installed
//! on the machine that runs the test.

#[cfg(target_os = "macos")]
#[test]
fn application_icon_tiff_returns_tiff_bytes_for_any_bundle_path() {
    let directory = tempfile::tempdir().unwrap();
    let bundle = directory.path().join("Synthetic.app");
    std::fs::create_dir_all(bundle.join("Contents")).unwrap();

    let tiff = platform_macos::application_icon_tiff(bundle.to_str().unwrap());

    let bytes = tiff.expect("NSWorkspace owes an icon for any path, generic at worst");
    // A TIFF begins with the byte-order mark: II (little) or MM (big).
    assert!(
        bytes.starts_with(b"II") || bytes.starts_with(b"MM"),
        "the icon bytes are not a TIFF"
    );
    assert!(bytes.len() > 8);
}
