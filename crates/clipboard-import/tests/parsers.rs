use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use clipboard_core::{ContentFlags, ContentKind};
use clipboard_import::{
    ImportSource, detect_export, parse_export, parse_export_report, parse_raycast,
    parse_raycast_report, parse_supercmd, parse_supercmd_report,
};
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
fn preserves_the_exact_synthetic_raycast_application_path_for_task_six() {
    let records = parse_raycast(fixture("raycast/clipboard.json")).unwrap();

    assert_eq!(
        records[0].source_application_path.as_deref(),
        Some("/Applications/Synthetic.app")
    );
}

#[test]
fn raycast_report_rejects_zero_copy_count_and_keeps_later_records() {
    let report = parse_raycast_report(fixture("raycast/invalid-copy-count.json")).unwrap();

    assert_eq!(report.total, 3);
    assert_eq!(report.candidates.len(), 2);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].record, 2);
    assert_eq!(report.failures[0].reason, "invalid_copy_count");
    assert_eq!(report.candidates[1].capture.occurrence_count, 4);
    let strict_error = match parse_raycast(fixture("raycast/invalid-copy-count.json")) {
        Ok(_) => panic!("the strict parser must reject the invalid record"),
        Err(error) => error,
    };
    assert_eq!(
        strict_error.to_string(),
        "raycast record 2: invalid_copy_count"
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

    let error = match parse_supercmd(dir.path(), path) {
        Ok(_) => panic!("the strict parser must reject the invalid record"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("supercmd record 1: invalid_timestamp"));
    assert!(!error.contains("secret-value"));
    assert!(!error.contains("private-field-content"));
}

#[test]
fn supercmd_json_report_keeps_good_records_after_a_bad_record() {
    let root = TempDir::new().unwrap();
    let path = write_export(
        root.path(),
        "[
      {\"copied_at\":\"2026-01-02T03:04:05Z\",\"type\":\"text\",\"text\":\"first\"},
      {\"copied_at\":\"invalid-private-value\",\"type\":\"text\",\"text\":\"bad\"},
      {\"copied_at\":\"2026-01-02T03:06:05Z\",\"type\":\"text\",\"text\":\"later\"}
    ]",
    );

    let report = parse_supercmd_report(root.path(), path).unwrap();
    assert_eq!(report.total, 3);
    assert_eq!(report.candidates.len(), 2);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].record, 2);
    assert_eq!(report.failures[0].reason, "invalid_timestamp");
    assert_eq!(report.candidates[1].primary_text.as_deref(), Some("later"));
}

#[test]
fn supercmd_csv_report_keeps_good_records_after_a_bad_row() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("clipboard.csv");
    fs::write(&path, "copied_at,type,source_app,bundle_id,pinned,file_url,text,ocr_text,has_image\n2026-01-02T03:04:05Z,text,,,false,,first,,false\ninvalid-private-value,text,,,false,,bad,,false\n2026-01-02T03:06:05Z,text,,,false,,later,,false\n").unwrap();

    let report = parse_export_report(path).unwrap();
    assert_eq!(report.total, 3);
    assert_eq!(report.candidates.len(), 2);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].record, 2);
    assert_eq!(report.failures[0].reason, "invalid_timestamp");
    assert_eq!(report.candidates[1].primary_text.as_deref(), Some("later"));
}

#[test]
fn fingerprints_distinguish_absent_and_empty_optional_fields() {
    let root = TempDir::new().unwrap();
    let path = write_export(root.path(), "[
      {\"copied_at\":\"2026-01-02T03:04:05Z\",\"type\":\"text\",\"text\":\"same\"},
      {\"copied_at\":\"2026-01-02T03:04:05Z\",\"type\":\"text\",\"source_app\":\"\",\"text\":\"same\"}
    ]");

    let records = parse_supercmd(root.path(), path).unwrap();
    assert_ne!(records[0].record_fingerprint, records[1].record_fingerprint);
}

#[test]
fn raycast_missing_identities_distinguish_full_paths_with_one_basename() {
    let root = TempDir::new().unwrap();
    let path = write_export(root.path(), "[
      {\"createdAt\":\"2026-01-02T03:04:05Z\",\"modifiedAt\":\"2026-01-02T03:04:05Z\",\"category\":\"image\",\"filePath\":\"/one/shared.png\",\"imageHash\":\"same\"},
      {\"createdAt\":\"2026-01-02T03:04:05Z\",\"modifiedAt\":\"2026-01-02T03:04:05Z\",\"category\":\"image\",\"filePath\":\"/two/shared.png\",\"imageHash\":\"same\"}
    ]");

    let records = parse_raycast(path).unwrap();
    assert_ne!(records[0].record_fingerprint, records[1].record_fingerprint);
    assert_ne!(
        records[0].capture.representations[0].missing_ref,
        records[1].capture.representations[0].missing_ref
    );
}

#[test]
fn fingerprints_distinguish_embedded_nul_field_boundaries() {
    let root = TempDir::new().unwrap();
    let path = write_export(root.path(), "[
      {\"copied_at\":\"2026-01-02T03:04:05Z\",\"type\":\"text\",\"bundle_id\":\"alpha\\u0000beta\",\"source_app\":\"gamma\",\"text\":\"same\"},
      {\"copied_at\":\"2026-01-02T03:04:05Z\",\"type\":\"text\",\"bundle_id\":\"alpha\",\"source_app\":\"beta\\u0000gamma\",\"text\":\"same\"}
    ]");

    let records = parse_supercmd(root.path(), path).unwrap();
    assert_ne!(records[0].record_fingerprint, records[1].record_fingerprint);
}

#[test]
fn fingerprints_use_canonical_timestamps_not_their_source_spelling() {
    let first = TempDir::new().unwrap();
    let second = TempDir::new().unwrap();
    let first_path = write_export(first.path(), "[{
      \"createdAt\":\"2026-01-02T03:04:05Z\",\"modifiedAt\":\"2026-01-02T03:04:05Z\",\"category\":\"text\",\"text\":\"same\"
    }]");
    let second_path = write_export(second.path(), "[{
      \"createdAt\":\"2026-01-02T03:04:05+00:00\",\"modifiedAt\":\"2026-01-02T03:04:05+00:00\",\"category\":\"text\",\"text\":\"same\"
    }]");

    let first = parse_raycast(first_path).unwrap();
    let second = parse_raycast(second_path).unwrap();
    assert_eq!(first[0].record_fingerprint, second[0].record_fingerprint);
}

#[test]
fn raycast_duplicate_events_have_stable_distinct_fingerprints() {
    let root = TempDir::new().unwrap();
    let path = write_export(root.path(), "[
      {\"createdAt\":\"2026-01-02T03:04:05Z\",\"modifiedAt\":\"2026-01-02T03:04:05Z\",\"category\":\"text\",\"copyCount\":1,\"text\":\"duplicate\"},
      {\"createdAt\":\"2026-01-02T03:04:05Z\",\"modifiedAt\":\"2026-01-02T03:04:05Z\",\"category\":\"text\",\"copyCount\":1,\"text\":\"duplicate\"}
    ]");

    let first = parse_raycast_report(&path).unwrap();
    let second = parse_raycast_report(path).unwrap();
    assert_eq!(first.candidates.len(), 2);
    assert_ne!(
        first.candidates[0].record_fingerprint,
        first.candidates[1].record_fingerprint
    );
    assert_eq!(
        candidate_fingerprints(&first),
        candidate_fingerprints(&second)
    );
}

#[test]
fn raycast_duplicate_fingerprints_ignore_unrelated_row_reordering() {
    let first_root = TempDir::new().unwrap();
    let second_root = TempDir::new().unwrap();
    let first_path = write_export(first_root.path(), "[
      {\"createdAt\":\"2026-01-02T03:04:05Z\",\"modifiedAt\":\"2026-01-02T03:04:05Z\",\"category\":\"text\",\"text\":\"duplicate\"},
      {\"createdAt\":\"2026-01-02T03:04:05Z\",\"modifiedAt\":\"2026-01-02T03:04:05Z\",\"category\":\"text\",\"text\":\"duplicate\"},
      {\"createdAt\":\"2026-01-02T03:06:05Z\",\"modifiedAt\":\"2026-01-02T03:06:05Z\",\"category\":\"text\",\"text\":\"other\"}
    ]");
    let second_path = write_export(second_root.path(), "[
      {\"createdAt\":\"2026-01-02T03:06:05Z\",\"modifiedAt\":\"2026-01-02T03:06:05Z\",\"category\":\"text\",\"text\":\"other\"},
      {\"createdAt\":\"2026-01-02T03:04:05Z\",\"modifiedAt\":\"2026-01-02T03:04:05Z\",\"category\":\"text\",\"text\":\"duplicate\"},
      {\"createdAt\":\"2026-01-02T03:04:05Z\",\"modifiedAt\":\"2026-01-02T03:04:05Z\",\"category\":\"text\",\"text\":\"duplicate\"}
    ]");

    let first = parse_raycast_report(first_path).unwrap();
    let second = parse_raycast_report(second_path).unwrap();
    assert_eq!(fingerprints_for_text(&first, "duplicate").len(), 2);
    assert_eq!(
        fingerprints_for_text(&first, "duplicate"),
        fingerprints_for_text(&second, "duplicate")
    );
}

#[test]
fn supercmd_duplicate_events_have_stable_distinct_fingerprints() {
    let root = TempDir::new().unwrap();
    let path = write_export(
        root.path(),
        "[
      {\"copied_at\":\"2026-01-02T03:04:05Z\",\"type\":\"text\",\"text\":\"duplicate\"},
      {\"copied_at\":\"2026-01-02T03:04:05Z\",\"type\":\"text\",\"text\":\"duplicate\"}
    ]",
    );

    let first = parse_supercmd_report(root.path(), &path).unwrap();
    let second = parse_supercmd_report(root.path(), path).unwrap();
    assert_eq!(first.candidates.len(), 2);
    assert_ne!(
        first.candidates[0].record_fingerprint,
        first.candidates[1].record_fingerprint
    );
    assert_eq!(
        candidate_fingerprints(&first),
        candidate_fingerprints(&second)
    );
}

#[test]
fn supercmd_duplicate_fingerprints_ignore_unrelated_row_reordering() {
    let first_root = TempDir::new().unwrap();
    let second_root = TempDir::new().unwrap();
    let first_path = write_export(
        first_root.path(),
        "[
      {\"copied_at\":\"2026-01-02T03:04:05Z\",\"type\":\"text\",\"text\":\"duplicate\"},
      {\"copied_at\":\"2026-01-02T03:04:05Z\",\"type\":\"text\",\"text\":\"duplicate\"},
      {\"copied_at\":\"2026-01-02T03:06:05Z\",\"type\":\"text\",\"text\":\"other\"}
    ]",
    );
    let second_path = write_export(
        second_root.path(),
        "[
      {\"copied_at\":\"2026-01-02T03:06:05Z\",\"type\":\"text\",\"text\":\"other\"},
      {\"copied_at\":\"2026-01-02T03:04:05Z\",\"type\":\"text\",\"text\":\"duplicate\"},
      {\"copied_at\":\"2026-01-02T03:04:05Z\",\"type\":\"text\",\"text\":\"duplicate\"}
    ]",
    );

    let first = parse_supercmd_report(first_root.path(), first_path).unwrap();
    let second = parse_supercmd_report(second_root.path(), second_path).unwrap();
    assert_eq!(fingerprints_for_text(&first, "duplicate").len(), 2);
    assert_eq!(
        fingerprints_for_text(&first, "duplicate"),
        fingerprints_for_text(&second, "duplicate")
    );
}

#[test]
fn supercmd_csv_duplicate_events_have_stable_distinct_fingerprints() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("clipboard.csv");
    fs::write(&path, "copied_at,type,source_app,bundle_id,pinned,file_url,text,ocr_text,has_image\n2026-01-02T03:04:05Z,text,,,false,,duplicate,,false\n2026-01-02T03:04:05Z,text,,,false,,duplicate,,false\n").unwrap();

    let first = parse_export_report(&path).unwrap();
    let second = parse_export_report(path).unwrap();
    assert_eq!(first.candidates.len(), 2);
    assert_ne!(
        first.candidates[0].record_fingerprint,
        first.candidates[1].record_fingerprint
    );
    assert_eq!(
        candidate_fingerprints(&first),
        candidate_fingerprints(&second)
    );
}

#[test]
fn supercmd_csv_reports_an_unequal_column_row_and_keeps_later_rows() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("clipboard.csv");
    fs::write(&path, "copied_at,type,source_app,bundle_id,pinned,file_url,text,ocr_text,has_image\n2026-01-02T03:04:05Z,text,,,false,,first,,false\n2026-01-02T03:05:05Z,text,,,false,,bad,false\n2026-01-02T03:06:05Z,text,,,false,,later,,false\n").unwrap();

    let report = parse_export_report(path).unwrap();
    assert_eq!(report.total, 3);
    assert_eq!(report.candidates.len(), 2);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].record, 2);
    assert_eq!(report.failures[0].reason, "invalid_record");
    assert_eq!(report.candidates[1].primary_text.as_deref(), Some("later"));
}

fn candidate_fingerprints(report: &clipboard_import::ImportParseReport) -> Vec<[u8; 32]> {
    report
        .candidates
        .iter()
        .map(|candidate| candidate.record_fingerprint)
        .collect()
}

fn fingerprints_for_text(
    report: &clipboard_import::ImportParseReport,
    text: &str,
) -> BTreeSet<[u8; 32]> {
    report
        .candidates
        .iter()
        .filter(|candidate| candidate.primary_text.as_deref() == Some(text))
        .map(|candidate| candidate.record_fingerprint)
        .collect()
}

#[test]
fn image_fingerprint_hashes_owned_primary_bytes() {
    let root = TempDir::new().unwrap();
    fs::create_dir(root.path().join("images")).unwrap();
    let image = root.path().join("images/sample.png");
    fs::write(&image, b"first-owned-image").unwrap();
    let path = write_export(root.path(), "[
      {\"copied_at\":\"2026-01-02T03:04:05Z\",\"type\":\"image\",\"file_url\":\"images/sample.png\",\"has_image\":true}
    ]");

    let first = parse_supercmd(root.path(), &path).unwrap().remove(0);
    fs::write(&image, b"second-owned-image").unwrap();
    let second = parse_supercmd(root.path(), path).unwrap().remove(0);
    assert_ne!(first.record_fingerprint, second.record_fingerprint);
}

#[cfg(unix)]
#[test]
fn rejects_a_final_component_symlink_even_when_its_target_is_in_root() {
    use std::os::unix::fs::symlink;

    let root = TempDir::new().unwrap();
    fs::create_dir(root.path().join("images")).unwrap();
    fs::write(root.path().join("images/owned.png"), b"owned-image").unwrap();
    symlink(
        root.path().join("images/owned.png"),
        root.path().join("images/linked.png"),
    )
    .unwrap();
    let path = write_export(root.path(), "[
      {\"copied_at\":\"2026-01-02T03:04:05Z\",\"type\":\"image\",\"file_url\":\"images/linked.png\",\"has_image\":true}
    ]");

    let record = parse_supercmd(root.path(), path).unwrap().remove(0);
    assert!(record.missing_payload);
    assert!(record.capture.representations[0].bytes.is_none());
}

fn write_export(root: &Path, contents: &str) -> PathBuf {
    let path = root.join("clipboard.json");
    fs::write(&path, contents).unwrap();
    path
}
