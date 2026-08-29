//! Catalog scanning over injected roots, on synthetic bundles only.
//!
//! Nothing here reads `/Applications` or any other real scan root: every
//! fixture is a throwaway directory tree under a `TempDir`, so the tests say
//! what the scanner does to a filesystem rather than what happened to be
//! installed on the machine that ran them.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use clipboard_launcher::{
    AppBundle, LaunchPathError, MAX_APP_NAME_BYTES, MAX_CATALOG_APPS, scan_applications,
    validate_launch_path,
};
use tempfile::TempDir;

/// Everything a synthetic bundle can vary. `None` fields are simply absent
/// from `Contents/Info.plist`, which is a shape real bundles do ship — the
/// scanner has to survive it rather than assume the ideal one.
struct AppSpec<'a> {
    dir_name: &'a str,
    display_name: Option<&'a str>,
    bundle_name: Option<&'a str>,
    bundle_id: Option<&'a str>,
    /// `LSUIElement` — faceless agents that would flood the list.
    ui_element: bool,
    /// `LSBackgroundOnly` — daemons that never show a face at all.
    background_only: bool,
    /// The same flags, written the way some hand-edited plists ship them.
    flags_as_strings: bool,
    /// `/System/Applications` ships binary plists; the reader must not care.
    binary_plist: bool,
    /// Write no `Info.plist` at all.
    without_plist: bool,
}

impl Default for AppSpec<'_> {
    fn default() -> Self {
        Self {
            dir_name: "Synthetic.app",
            display_name: None,
            bundle_name: None,
            bundle_id: None,
            ui_element: false,
            background_only: false,
            flags_as_strings: false,
            binary_plist: false,
            without_plist: false,
        }
    }
}

fn make_app(root: &Path, spec: &AppSpec<'_>) -> PathBuf {
    let contents = root.join(spec.dir_name).join("Contents");
    fs::create_dir_all(&contents).unwrap();
    if spec.without_plist {
        return root.join(spec.dir_name);
    }
    let mut info = plist::Dictionary::new();
    if let Some(name) = spec.display_name {
        info.insert("CFBundleDisplayName".into(), name.into());
    }
    if let Some(name) = spec.bundle_name {
        info.insert("CFBundleName".into(), name.into());
    }
    if let Some(id) = spec.bundle_id {
        info.insert("CFBundleIdentifier".into(), id.into());
    }
    let flag = |set: bool| {
        if set && spec.flags_as_strings {
            plist::Value::from("true")
        } else {
            plist::Value::from(set)
        }
    };
    if spec.ui_element {
        info.insert("LSUIElement".into(), flag(true));
    }
    if spec.background_only {
        info.insert("LSBackgroundOnly".into(), flag(true));
    }
    let value = plist::Value::Dictionary(info);
    let file = fs::File::create(contents.join("Info.plist")).unwrap();
    if spec.binary_plist {
        value.to_writer_binary(file).unwrap();
    } else {
        value.to_writer_xml(file).unwrap();
    }
    root.join(spec.dir_name)
}

fn bundle_named<'a>(catalog: &'a [AppBundle], name: &str) -> &'a AppBundle {
    catalog
        .iter()
        .find(|app| app.name == name)
        .unwrap_or_else(|| panic!("no bundle named {name} in {catalog:?}"))
}

#[test]
fn scan_finds_app_bundles_with_names_and_bundle_ids_from_info_plist() {
    let root = TempDir::new().unwrap();
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "Synthetic.app",
            bundle_name: Some("Synthetic Notes"),
            bundle_id: Some("com.example.synthetic"),
            ..AppSpec::default()
        },
    );

    let catalog = scan_applications(&[root.path().to_path_buf()]);

    assert_eq!(catalog.len(), 1);
    let app = &catalog[0];
    assert_eq!(app.name, "Synthetic Notes");
    assert_eq!(app.bundle_id.as_deref(), Some("com.example.synthetic"));
    // The catalog carries canonical paths — on macOS that means the tempdir
    // resolves through its `/var` → `/private/var` symlink.
    assert_eq!(
        app.path,
        root.path()
            .canonicalize()
            .unwrap()
            .join("Synthetic.app")
            .to_str()
            .unwrap()
    );
}

#[test]
fn scan_reads_binary_plists_the_same_as_xml() {
    let root = TempDir::new().unwrap();
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "Xml.app",
            bundle_name: Some("Xml Bundle"),
            bundle_id: Some("com.example.xml"),
            ..AppSpec::default()
        },
    );
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "Binary.app",
            bundle_name: Some("Binary Bundle"),
            bundle_id: Some("com.example.binary"),
            binary_plist: true,
            ..AppSpec::default()
        },
    );

    let catalog = scan_applications(&[root.path().to_path_buf()]);

    assert_eq!(
        bundle_named(&catalog, "Xml Bundle").bundle_id.as_deref(),
        Some("com.example.xml")
    );
    assert_eq!(
        bundle_named(&catalog, "Binary Bundle").bundle_id.as_deref(),
        Some("com.example.binary")
    );
}

#[test]
fn scan_prefers_display_name_then_bundle_name_then_file_stem() {
    let root = TempDir::new().unwrap();
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "Both.app",
            display_name: Some("Display Wins"),
            bundle_name: Some("Bundle Name Loses"),
            ..AppSpec::default()
        },
    );
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "OnlyBundle.app",
            bundle_name: Some("Bundle Name Wins"),
            ..AppSpec::default()
        },
    );
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "Stem Only.app",
            ..AppSpec::default()
        },
    );
    // A display name past every bound still arrives bounded: the field has a
    // size contract on the bridge, and the whole catalog depends on it.
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "Long.app",
            display_name: Some(&"x".repeat(MAX_APP_NAME_BYTES + 64)),
            ..AppSpec::default()
        },
    );

    let catalog = scan_applications(&[root.path().to_path_buf()]);

    assert!(catalog.iter().any(|app| app.name == "Display Wins"));
    assert!(catalog.iter().any(|app| app.name == "Bundle Name Wins"));
    assert!(catalog.iter().any(|app| app.name == "Stem Only"));
    assert!(
        catalog
            .iter()
            .all(|app| app.name.len() <= MAX_APP_NAME_BYTES)
    );
}

#[test]
fn scan_recurses_into_utility_folders_but_not_into_bundles() {
    let root = TempDir::new().unwrap();
    // `/Applications/Utilities` and `/System/Applications/Utilities` are the
    // reason the scanner descends one level at all.
    make_app(
        &root.path().join("Utilities"),
        &AppSpec {
            dir_name: "Nested.app",
            bundle_name: Some("Nested"),
            ..AppSpec::default()
        },
    );
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "Outer.app",
            bundle_name: Some("Outer"),
            ..AppSpec::default()
        },
    );
    // A bundle hidden inside another bundle's `Contents`: `Outer.app` is a
    // sealed unit, and counting its passengers would double-list it.
    make_app(
        &root.path().join("Outer.app").join("Contents"),
        &AppSpec {
            dir_name: "Stowaway.app",
            bundle_name: Some("Stowaway"),
            ..AppSpec::default()
        },
    );
    // Two plain directories down plus the bundle: past the depth cap.
    make_app(
        &root.path().join("Deep").join("Deeper"),
        &AppSpec {
            dir_name: "TooDeep.app",
            bundle_name: Some("Too Deep"),
            ..AppSpec::default()
        },
    );

    let catalog = scan_applications(&[root.path().to_path_buf()]);

    let names: Vec<&str> = catalog.iter().map(|app| app.name.as_str()).collect();
    assert!(names.contains(&"Nested"));
    assert!(names.contains(&"Outer"));
    assert!(!names.contains(&"Stowaway"));
    assert!(!names.contains(&"Too Deep"));
    assert_eq!(catalog.len(), 2);
}

#[test]
fn scan_skips_ui_element_and_background_only_agents() {
    let root = TempDir::new().unwrap();
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "Agent.app",
            bundle_name: Some("Agent"),
            ui_element: true,
            ..AppSpec::default()
        },
    );
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "Daemon.app",
            bundle_name: Some("Daemon"),
            background_only: true,
            ..AppSpec::default()
        },
    );
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "Visible.app",
            bundle_name: Some("Visible"),
            ..AppSpec::default()
        },
    );

    let catalog = scan_applications(&[root.path().to_path_buf()]);

    let names: Vec<&str> = catalog.iter().map(|app| app.name.as_str()).collect();
    assert_eq!(names, vec!["Visible"]);
}

#[test]
fn scan_skips_agents_whose_flags_ship_as_strings() {
    let root = TempDir::new().unwrap();
    // Hand-edited plists spell the flag as text; a filter that only
    // understands booleans would let these two flood the list.
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "StringAgent.app",
            bundle_name: Some("String Agent"),
            ui_element: true,
            flags_as_strings: true,
            ..AppSpec::default()
        },
    );
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "StringDaemon.app",
            bundle_name: Some("String Daemon"),
            background_only: true,
            flags_as_strings: true,
            ..AppSpec::default()
        },
    );
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "PlainFalse.app",
            bundle_name: Some("Plain False"),
            // A false flag — in either spelling — must not hide the bundle.
            ui_element: false,
            flags_as_strings: true,
            ..AppSpec::default()
        },
    );

    let catalog = scan_applications(&[root.path().to_path_buf()]);

    let names: Vec<&str> = catalog.iter().map(|app| app.name.as_str()).collect();
    assert_eq!(names, vec!["Plain False"]);
}

#[test]
fn scan_falls_through_blank_names_to_the_next_candidate() {
    let root = TempDir::new().unwrap();
    // An empty display name must not win over a real bundle name, and must
    // not survive to the bridge: the validator there rejects one malformed
    // entry by refusing the whole catalog, so this side never emits one.
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "EmptyDisplay.app",
            display_name: Some(""),
            bundle_name: Some("Real Bundle Name"),
            ..AppSpec::default()
        },
    );
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "BlankDisplay.app",
            display_name: Some("   "),
            bundle_name: Some("   "),
            ..AppSpec::default()
        },
    );
    make_app(
        root.path(),
        &AppSpec {
            dir_name: " Pad.app",
            without_plist: true,
            ..AppSpec::default()
        },
    );

    let catalog = scan_applications(&[root.path().to_path_buf()]);

    let names: Vec<&str> = catalog.iter().map(|app| app.name.as_str()).collect();
    assert!(names.contains(&"Real Bundle Name"));
    // Whitespace-only metadata falls back to the directory stem, trimmed.
    assert!(names.contains(&"BlankDisplay"));
    assert!(names.contains(&"Pad"));
    assert_eq!(catalog.len(), 3);
}

#[test]
fn scan_skips_bundles_with_an_oversized_info_plist() {
    let root = TempDir::new().unwrap();
    let app = make_app(
        root.path(),
        &AppSpec {
            dir_name: "Huge.app",
            bundle_name: Some("Huge"),
            ..AppSpec::default()
        },
    );
    // A "plist" measured in megabytes is a payload, not metadata; reading it
    // would hand the file's size to the process's memory.
    let junk = fs::File::create(app.join("Contents").join("Info.plist")).unwrap();
    junk.set_len(4 * 1024 * 1024 + 1).unwrap();
    drop(junk);
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "Small.app",
            bundle_name: Some("Small"),
            ..AppSpec::default()
        },
    );

    let catalog = scan_applications(&[root.path().to_path_buf()]);

    let names: Vec<&str> = catalog.iter().map(|app| app.name.as_str()).collect();
    assert_eq!(names, vec!["Small"]);
}

#[test]
fn scan_does_not_follow_directory_symlinks_or_loop_on_cycles() {
    let root = TempDir::new().unwrap();
    let elsewhere = TempDir::new().unwrap();
    make_app(
        elsewhere.path(),
        &AppSpec {
            dir_name: "Linked.app",
            bundle_name: Some("Linked"),
            ..AppSpec::default()
        },
    );
    // A directory symlink inside a root is not a subdirectory of it, and
    // following it would both loop (a -> b -> a) and quietly change what
    // the catalog covers. Neither happens: the walk stays on real
    // directories and terminates.
    symlink(elsewhere.path(), root.path().join("LinkedTools")).unwrap();
    symlink(root.path().join("LoopB"), root.path().join("LoopA")).unwrap();
    symlink(root.path().join("LoopA"), root.path().join("LoopB")).unwrap();
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "Direct.app",
            bundle_name: Some("Direct"),
            ..AppSpec::default()
        },
    );

    let catalog = scan_applications(&[root.path().to_path_buf()]);

    let names: Vec<&str> = catalog.iter().map(|app| app.name.as_str()).collect();
    assert_eq!(names, vec!["Direct"]);
}

#[test]
fn scan_survives_a_missing_info_plist_by_falling_back_to_the_file_stem() {
    let root = TempDir::new().unwrap();
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "No Plist.app",
            without_plist: true,
            ..AppSpec::default()
        },
    );

    let catalog = scan_applications(&[root.path().to_path_buf()]);

    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].name, "No Plist");
    assert_eq!(catalog[0].bundle_id, None);
}

#[test]
fn scan_ignores_roots_that_do_not_exist() {
    let root = TempDir::new().unwrap();
    let missing = root.path().join("not-installed");

    let catalog = scan_applications(&[missing]);

    assert!(catalog.is_empty());
}

#[test]
fn scan_sorts_by_normalized_name_and_dedupes_canonical_paths() {
    let root = TempDir::new().unwrap();
    for dir_name in ["Zebra.app", "Apple.app", "Łódź.app"] {
        make_app(
            root.path(),
            &AppSpec {
                dir_name,
                ..AppSpec::default()
            },
        );
    }
    // An alias whose symlink resolves onto an already-scanned bundle: one
    // application, listed once, whichever directory entry found it first.
    symlink(
        root.path().join("Apple.app"),
        root.path().join("Apple Alias.app"),
    )
    .unwrap();

    let catalog = scan_applications(&[root.path().to_path_buf()]);

    let names: Vec<&str> = catalog.iter().map(|app| app.name.as_str()).collect();
    // `Łódź` folds with the `l` words, not after `z` — a user typing "lo"
    // must not be told the town is missing because of an alphabet nobody
    // sorts with.
    assert_eq!(names, vec!["Apple", "Łódź", "Zebra"]);
    assert_eq!(catalog.len(), 3);
}

#[test]
fn scan_caps_the_catalog_at_a_bounded_number_of_apps() {
    let root = TempDir::new().unwrap();
    for index in 0..(MAX_CATALOG_APPS + 5) {
        // No plist: the cap is about counting directories, and two thousand
        // tiny plists would make the point at the filesystem's expense.
        make_app(
            root.path(),
            &AppSpec {
                dir_name: &format!("Cap {index:05}.app"),
                without_plist: true,
                ..AppSpec::default()
            },
        );
    }

    let catalog = scan_applications(&[root.path().to_path_buf()]);

    assert_eq!(catalog.len(), MAX_CATALOG_APPS);
}

#[test]
fn validate_launch_path_accepts_a_scanned_bundle_and_returns_its_canonical_path() {
    let root = TempDir::new().unwrap();
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "Launchable.app",
            ..AppSpec::default()
        },
    );

    let path = validate_launch_path(
        root.path().join("Launchable.app").to_str().unwrap(),
        &[root.path().to_path_buf()],
    );

    assert_eq!(
        path.unwrap(),
        root.path().canonicalize().unwrap().join("Launchable.app")
    );
}

#[test]
fn validate_launch_path_rejects_nonsense_without_leaking_it() {
    let root = TempDir::new().unwrap();
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "Inside.app",
            ..AppSpec::default()
        },
    );
    make_app(
        root.path(),
        &AppSpec {
            dir_name: "Plain.app",
            without_plist: true,
            ..AppSpec::default()
        },
    );
    // A regular file wearing the bundle suffix: the name promises a
    // directory the filesystem does not.
    fs::write(root.path().join("File.app"), "not a bundle").unwrap();
    let outside = TempDir::new().unwrap();
    make_app(
        outside.path(),
        &AppSpec {
            dir_name: "Outside.app",
            ..AppSpec::default()
        },
    );

    let too_long = "a".repeat(2_048);
    let missing_path = root.path().join("Missing.app").to_str().unwrap().to_owned();
    let file_path = root.path().join("File.app").to_str().unwrap().to_owned();
    let outside_path = outside
        .path()
        .join("Outside.app")
        .to_str()
        .unwrap()
        .to_owned();
    let cases: &[(&str, LaunchPathError)] = &[
        ("", LaunchPathError::Invalid),
        ("relative/No.app", LaunchPathError::Invalid),
        ("with\0nul", LaunchPathError::Invalid),
        (too_long.as_str(), LaunchPathError::Invalid),
        (missing_path.as_str(), LaunchPathError::NotFound),
        (file_path.as_str(), LaunchPathError::NotAppBundle),
        (outside_path.as_str(), LaunchPathError::OutsideScannedRoots),
    ];
    for (candidate, expected) in cases {
        let error = validate_launch_path(candidate, &[root.path().to_path_buf()])
            .expect_err("the scanner refused this path");
        assert_eq!(error, *expected);
        // Stable machine codes, never the path back: the refusal explains
        // itself without repeating what was refused.
        assert_eq!(error.code(), expected.code());
    }
}
