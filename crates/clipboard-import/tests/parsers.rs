use std::{
    fs,
    path::{Path, PathBuf},
};

use clipboard_core::{ContentFlags, ContentKind};
use clipboard_import::{ImportSource, detect_export, parse_export, parse_raycast, parse_supercmd};
use tempfile::TempDir;

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path)
}

#[test]
fn maps_raycast_copy_count_and_missing_image() {
    let records = parse_raycast(fixture("raycast/clipboard.json")).unwrap();

    assert_eq!(records[0].capture.occurrence_count, 3);
    assert!(
        records
            .iter()
            .any(|record| { record.capture.kind == ContentKind::Image && record.missing_payload })
    );
}

#[test]
fn maps_supercmd_ocr_without_replacing_original_text() {
    let records = parse_supercmd(fixture("supercmd"), fixture("supercmd/clipboard.json")).unwrap();

    assert_eq!(records[0].search_ocr.as_deref(), Some("invoice 2026"));
    assert_eq!(records[0].primary_text.as_deref(), Some("original caption"));
    assert!(records[0].capture.pinned);
    assert_eq!(
        records[0].capture.source_app_id.as_deref(),
        Some("com.example.synthetic")
    );
}

#[test]
fn resolves_a_declared_in_root_supercmd_image_to_owned_bytes() {
    let records = parse_supercmd(fixture("supercmd"), fixture("supercmd/clipboard.json")).unwrap();

    let image = &records[1];
    assert_eq!(image.capture.kind, ContentKind::Image);
    assert!(!image.missing_payload);
    assert!(
        image.capture.representations[0]
            .bytes
            .as_deref()
            .is_some_and(|bytes| !bytes.is_empty())
    );
}

#[test]
fn parses_multiline_csv_with_rfc4180_rules() {
    let records = parse_export(fixture("supercmd/clipboard.csv")).unwrap();

    assert_eq!(
        records[0].primary_text.as_deref(),
        Some("first line\nsecond line")
    );
}

#[test]
fn detect_export_identifies_json_and_csv_sources() {
    assert_eq!(
        detect_export(fixture("raycast/clipboard.json"))
            .unwrap()
            .source,
        ImportSource::Raycast,
    );
    assert_eq!(
        detect_export(fixture("supercmd/clipboard.csv"))
            .unwrap()
            .source,
        ImportSource::SuperCmd,
    );
}

#[test]
fn detect_and_parse_accept_a_raycast_export_directory() {
    let directory = fixture("raycast");

    assert_eq!(
        detect_export(&directory).unwrap().source,
        ImportSource::Raycast
    );
    assert_eq!(
        parse_export(directory).unwrap()[0].capture.occurrence_count,
        3
    );
}

#[test]
fn detect_and_parse_accept_a_supercmd_export_directory() {
    let directory = fixture("supercmd");

    assert_eq!(
        detect_export(&directory).unwrap().source,
        ImportSource::SuperCmd
    );
    assert_eq!(
        parse_export(directory).unwrap()[0].primary_text.as_deref(),
        Some("original caption")
    );
}

#[test]
fn directory_without_one_manifest_has_a_path_free_stable_error() {
    let directory = TempDir::new().unwrap();

    assert_eq!(
        detect_export(directory.path()).unwrap_err().to_string(),
        "detection export: manifest_not_found"
    );
}

#[test]
fn directory_with_multiple_unnamed_manifests_is_rejected() {
    let directory = TempDir::new().unwrap();
    fs::write(directory.path().join("first.json"), "[]").unwrap();
    fs::write(directory.path().join("second.csv"), "copied_at,type\n").unwrap();

    assert_eq!(
        detect_export(directory.path()).unwrap_err().to_string(),
        "detection export: ambiguous_manifest"
    );
}

#[test]
fn preserves_whitespace_only_text_without_indexing() {
    let records = parse_supercmd(fixture("supercmd"), fixture("supercmd/clipboard.json")).unwrap();

    assert!(records[1].primary_text.is_none());
    assert!(records[2].missing_payload);

    let dir = TempDir::new().unwrap();
    let path = write_export(dir.path(), "[{
      \"copied_at\": \"2026-01-02T03:04:05Z\", \"type\": \"text\", \"text\": \"   \", \"has_image\": false
    }]");
    let record = parse_supercmd(dir.path(), path).unwrap().remove(0);
    assert_eq!(record.primary_text.as_deref(), Some("   "));
    assert!(
        record
            .capture
            .content_flags
            .contains(ContentFlags::DO_NOT_INDEX)
    );
}

#[test]
fn refuses_supercmd_image_traversal_and_keeps_stable_missing_reference() {
    let records = parse_supercmd(fixture("supercmd"), fixture("supercmd/clipboard.json")).unwrap();

    let escaped = &records[2];
    assert!(escaped.missing_payload);
    assert!(escaped.capture.representations[0].bytes.is_none());
    assert!(
        escaped.capture.representations[0]
            .missing_ref
            .as_deref()
            .unwrap()
            .starts_with("supercmd-missing:")
    );
}

#[test]
fn rejects_a_traversal_reference_even_when_an_in_root_basename_exists() {
    let root = TempDir::new().unwrap();
    fs::create_dir(root.path().join("images")).unwrap();
    fs::copy(
        fixture("supercmd/images/sample.png"),
        root.path().join("images/outside.png"),
    )
    .unwrap();
    let path = write_export(root.path(), "[{
      \"copied_at\": \"2026-01-02T03:04:05Z\", \"type\": \"image\", \"file_url\": \"../outside.png\", \"has_image\": true
    }]");

    let record = parse_supercmd(root.path(), path).unwrap().remove(0);
    assert!(record.missing_payload);
    assert!(record.capture.representations[0].bytes.is_none());
}

#[test]
fn record_fingerprints_are_independent_of_export_root() {
    let first = TempDir::new().unwrap();
    let second = TempDir::new().unwrap();
    let original = fixture("supercmd/clipboard.json");
    fs::copy(&original, first.path().join("clipboard.json")).unwrap();
    fs::copy(&original, second.path().join("clipboard.json")).unwrap();

    let a = parse_supercmd(first.path(), first.path().join("clipboard.json")).unwrap();
    let b = parse_supercmd(second.path(), second.path().join("clipboard.json")).unwrap();
    assert_eq!(a[0].record_fingerprint, b[0].record_fingerprint);
}

#[test]
fn source_fingerprints_are_independent_of_export_root() {
    let first = TempDir::new().unwrap();
    let second = TempDir::new().unwrap();
    let original = fixture("supercmd/clipboard.json");
    fs::copy(&original, first.path().join("clipboard.json")).unwrap();
    fs::copy(&original, second.path().join("clipboard.json")).unwrap();

    let a = detect_export(first.path().join("clipboard.json")).unwrap();
    let b = detect_export(second.path().join("clipboard.json")).unwrap();
    assert_eq!(a.source_fingerprint, b.source_fingerprint);
}

#[cfg(unix)]
#[test]
fn rejects_an_in_root_symlink_to_an_external_image() {
    use std::os::unix::fs::symlink;

    let root = TempDir::new().unwrap();
    let external = TempDir::new().unwrap();
    let external_image = external.path().join("outside.png");
    fs::write(&external_image, b"not-an-imported-payload").unwrap();
    fs::create_dir(root.path().join("images")).unwrap();
    symlink(&external_image, root.path().join("images/linked.png")).unwrap();
    let path = write_export(root.path(), "[{
      \"copied_at\": \"2026-01-02T03:04:05Z\", \"type\": \"image\", \"file_url\": \"images/linked.png\", \"has_image\": true
    }]");

    let record = parse_supercmd(root.path(), path).unwrap().remove(0);
    assert!(record.missing_payload);
    assert!(record.capture.representations[0].bytes.is_none());
}

#[test]
fn parse_errors_are_sanitized_and_numbered() {
    let dir = TempDir::new().unwrap();
    let path = write_export(dir.path(), "[{
      \"copied_at\": \"not-a-timestamp-secret-value\", \"type\": \"text\", \"text\": \"private-field-content\"
    }]");

    let error = parse_supercmd(dir.path(), path).unwrap_err().to_string();
    assert!(error.contains("supercmd record 1: invalid_timestamp"));
    assert!(!error.contains("secret-value"));
    assert!(!error.contains("private-field-content"));
}

fn write_export(root: &Path, contents: &str) -> PathBuf {
    let path = root.join("clipboard.json");
    fs::write(&path, contents).unwrap();
    path
}
