#![allow(unused_imports, unused_macros)]

macro_rules! relocated_parser_tests {
() => {
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use clipboard_core::{ContentFlags, ContentKind};
use crate::{ImportParseLimits, ImportParseReport, ImportSource};
use crate::detect::{DetectedExport, detect_export_with_permit};
use tempfile::TempDir;

fn with_parser_permit<T>(
    operation: impl FnOnce(&clipboard_store::ImportOperationPermit) -> Result<T, crate::ImportError>,
) -> Result<T, crate::ImportError> {
    let gate = clipboard_store::ImportOperationGate::with_capacity(
        crate::MAX_IMPORT_OPERATION_BYTES,
    )
    .unwrap();
    let permit = gate.acquire_blocking().unwrap();
    operation(&permit)
}

fn detect_export(path: impl AsRef<Path>) -> Result<DetectedExport, crate::ImportError> {
    with_parser_permit(|permit| {
        detect_export_with_permit(path.as_ref(), permit, ImportParseLimits::default())
    })
}

fn parse_export_report(path: impl AsRef<Path>) -> Result<ImportParseReport, crate::ImportError> {
    with_parser_permit(|permit| {
        let detected =
            detect_export_with_permit(path.as_ref(), permit, ImportParseLimits::default())?;
        crate::parse_detected_export_report_with_permit(
            &detected,
            None,
            permit,
            ImportParseLimits::default(),
        )
    })
}

fn parse_export(path: impl AsRef<Path>) -> Result<Vec<crate::ImportCandidate>, crate::ImportError> {
    parse_export_report(path)?.into_strict()
}

fn parse_raycast_report(
    path: impl AsRef<Path>,
) -> Result<ImportParseReport, crate::ImportError> {
    with_parser_permit(|permit| {
        crate::raycast::parse_raycast_report_with_permit(
            path,
            permit,
            ImportParseLimits::default(),
        )
    })
}

fn parse_raycast(
    path: impl AsRef<Path>,
) -> Result<Vec<crate::ImportCandidate>, crate::ImportError> {
    parse_raycast_report(path)?.into_strict()
}

fn parse_supercmd_report(
    export_root: impl AsRef<Path>,
    path: impl AsRef<Path>,
) -> Result<ImportParseReport, crate::ImportError> {
    with_parser_permit(|permit| {
        crate::supercmd::parse_supercmd_report_with_permit(
            export_root,
            path.as_ref(),
            permit,
            ImportParseLimits::default(),
        )
    })
}

fn parse_supercmd(
    export_root: impl AsRef<Path>,
    path: impl AsRef<Path>,
) -> Result<Vec<crate::ImportCandidate>, crate::ImportError> {
    parse_supercmd_report(export_root, path)?.into_strict()
}

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path)
}

fn candidate_primary_text(candidate: &crate::ImportCandidate) -> Option<&str> {
    candidate
        .capture
        .kind
        .is_textual()
        .then(|| candidate.capture.representations.first()?.bytes.as_deref())
        .flatten()
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
}

fn uri_list_reference(candidate: &crate::ImportCandidate) -> Option<&str> {
    candidate
        .capture
        .representations
        .iter()
        .find(|representation| representation.format_id == "text/uri-list")
        .and_then(|representation| representation.bytes.as_deref())
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
}

#[test]
fn an_entry_without_a_readable_payload_carries_the_name_its_source_showed() {
    let raycast = parse_raycast(fixture("raycast/clipboard.json")).unwrap();
    let file = raycast
        .iter()
        .find(|record| record.capture.kind == ContentKind::File)
        .expect("the fixture must contain a file record");
    let text = raycast
        .iter()
        .find(|record| record.capture.kind == ContentKind::Text)
        .expect("the fixture must contain a text record");

    assert_eq!(file.capture.display_label.as_deref(), Some("report card.pdf"));
    // A textual entry speaks for itself, so it needs no separate label.
    assert_eq!(text.capture.display_label, None);
}

#[test]
fn raycast_file_record_keeps_its_source_path_as_a_uri_list_reference() {
    let records = parse_raycast(fixture("raycast/clipboard.json")).unwrap();
    let file = records
        .iter()
        .find(|record| record.capture.kind == ContentKind::File)
        .expect("the fixture must contain a file record");

    assert_eq!(
        uri_list_reference(file),
        Some("file:///fixtures/synthetic%20dir/report%20card.pdf")
    );
    assert!(file.missing_payload);
    assert!(
        file.capture
            .content_flags
            .contains(ContentFlags::MISSING_PAYLOAD)
    );
}

#[test]
fn raycast_record_without_a_source_path_gains_no_uri_list_reference() {
    let records = parse_raycast(fixture("raycast/clipboard.json")).unwrap();
    let image = records
        .iter()
        .find(|record| record.capture.kind == ContentKind::Image)
        .expect("the fixture must contain an image record");

    assert_eq!(uri_list_reference(image), None);
}

#[test]
fn supercmd_keeps_only_an_absolute_source_url_as_a_uri_list_reference() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("clipboard.json");
    fs::write(
        &path,
        br#"[{"copied_at":"2026-01-02T03:04:05Z","type":"file","file_url":"file:///fixtures/synthetic%20dir/report.pdf","has_image":false},
             {"copied_at":"2026-01-02T03:05:05Z","type":"file","file_url":"nested/report.pdf","has_image":false}]"#,
    )
    .unwrap();

    let records = parse_supercmd(root.path(), &path).unwrap();

    assert_eq!(
        uri_list_reference(&records[0]),
        Some("file:///fixtures/synthetic%20dir/report.pdf")
    );
    // A relative reference points inside the export, not at a location the
    // user could open later, so it is not a source reference.
    assert_eq!(uri_list_reference(&records[1]), None);
}

#[test]
fn a_source_reference_never_exceeds_the_writer_representation_limit() {
    let raycast = parse_raycast(fixture("raycast/clipboard.json")).unwrap();
    let supercmd = parse_supercmd(fixture("supercmd"), fixture("supercmd/clipboard.json")).unwrap();

    for record in raycast.iter().chain(supercmd.iter()) {
        assert!(
            record.capture.representations.len()
                <= clipboard_store::MAX_IMPORT_REPRESENTATIONS,
            "{:?} produced {} representations",
            record.capture.kind,
            record.capture.representations.len()
        );
    }
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
    assert_eq!(candidate_primary_text(&records[0]), Some("original caption"));
    assert!(records[0].capture.pinned);
    assert_eq!(
        records[0].capture.source_app_id.as_deref(),
        Some("com.example.synthetic")
    );
}

#[test]
fn parses_supercmd_zero_one_flags_and_nullable_text() {
    let root = TempDir::new().unwrap();
    let path = write_export(
        root.path(),
        r#"[
          {"copied_at":"2026-01-02T03:04:05Z","type":"text","pinned":0,"text":null,"has_image":0},
          {"copied_at":"2026-01-02T03:05:05Z","type":"image","pinned":1,"file_url":"images/missing.png","text":null,"has_image":1}
        ]"#,
    );

    let report = parse_supercmd_report(root.path(), path).unwrap();

    assert_eq!(report.total, 2);
    assert_eq!(report.candidates.len(), 2);
    assert!(report.failures.is_empty());
    assert!(!report.candidates[0].capture.pinned);
    assert_eq!(candidate_primary_text(&report.candidates[0]), Some(""));
    assert!(
        report.candidates[0]
            .capture
            .content_flags
            .contains(ContentFlags::DO_NOT_INDEX)
    );
    assert!(report.candidates[1].capture.pinned);
    assert_eq!(report.candidates[1].capture.kind, ContentKind::Image);
    assert!(report.candidates[1].missing_payload);
}

#[test]
fn parses_supercmd_naive_timestamp_as_utc_deterministically() {
    let root = TempDir::new().unwrap();
    let path = write_export(
        root.path(),
        r#"[
          {"copied_at":"2026-01-02 03:04:05","type":"text","pinned":0,"text":null,"has_image":0}
        ]"#,
    );

    let report = parse_supercmd_report(root.path(), path).unwrap();

    assert!(report.failures.is_empty());
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(
        report.candidates[0].capture.captured_at_ms,
        chrono::DateTime::parse_from_rfc3339("2026-01-02T03:04:05Z")
            .unwrap()
            .timestamp_millis()
    );
}

#[test]
fn rejects_supercmd_numeric_flags_outside_zero_and_one() {
    let root = TempDir::new().unwrap();
    let path = write_export(
        root.path(),
        r#"[
          {"copied_at":"2026-01-02T03:04:05Z","type":"text","pinned":2,"text":"fixture","has_image":0},
          {"copied_at":"2026-01-02T03:05:05Z","type":"text","pinned":0,"text":"fixture","has_image":-1}
        ]"#,
    );

    let report = parse_supercmd_report(root.path(), path).unwrap();

    assert_eq!(report.total, 2);
    assert!(report.candidates.is_empty());
    assert_eq!(report.failures.len(), 2);
    assert!(
        report
            .failures
            .iter()
            .all(|failure| failure.reason == "invalid_record")
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
        candidate_primary_text(&records[0]),
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
        candidate_primary_text(&parse_export(directory).unwrap()[0]),
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

    assert!(candidate_primary_text(&records[1]).is_none());
    assert!(records[2].missing_payload);

    let dir = TempDir::new().unwrap();
    let path = write_export(dir.path(), "[{
      \"copied_at\": \"2026-01-02T03:04:05Z\", \"type\": \"text\", \"text\": \"   \", \"has_image\": false
    }]");
    let record = parse_supercmd(dir.path(), path).unwrap().remove(0);
    assert_eq!(candidate_primary_text(&record), Some("   "));
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
    assert_eq!(candidate_primary_text(&report.candidates[1]), Some("later"));
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
    assert_eq!(candidate_primary_text(&report.candidates[1]), Some("later"));
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
    assert_eq!(candidate_primary_text(&report.candidates[1]), Some("later"));
}

fn candidate_fingerprints(report: &crate::ImportParseReport) -> Vec<[u8; 32]> {
    report
        .candidates
        .iter()
        .map(|candidate| candidate.record_fingerprint)
        .collect()
}

fn fingerprints_for_text(
    report: &crate::ImportParseReport,
    text: &str,
) -> BTreeSet<[u8; 32]> {
    report
        .candidates
        .iter()
        .filter(|candidate| candidate_primary_text(candidate) == Some(text))
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

/// A password that exists only in these tests. The real export's password is
/// never written down anywhere in this repository.
const SYNTHETIC_RAYCONFIG_PASSWORD: &str = "synthetic-parser-password";

/// Builds a `.rayconfig` around a records array, the way Raycast does.
fn write_rayconfig_export(root: &Path, records: &str) -> PathBuf {
    use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
    use flate2::{Compression, write::GzEncoder};
    use sha2::{Digest, Sha256};
    use std::io::Write;

    let document = format!(
        concat!(
            r#"{{"raycast_version":"1.104.25","builtin_package_clipboardHistory":"#,
            r#"{{"clipboardHistoryLengthKey":"threeMonths","clipboardHistoryRecords":{},"#,
            r#""clipboardHistoryDisabledApplications":["com.example.one"],"#,
            r#""provider_schemaVersion":1}}}}"#
        ),
        records
    );
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(document.as_bytes()).unwrap();
    let compressed = encoder.finish().unwrap();

    let mut key = [0_u8; 32];
    key.copy_from_slice(&Sha256::digest(SYNTHETIC_RAYCONFIG_PASSWORD.as_bytes()));
    let iv = [11_u8; 16];
    let mut buffer = vec![0_u8; compressed.len() + 16];
    let written = cbc::Encryptor::<aes::Aes256>::new(&key.into(), &iv.into())
        .encrypt_padded_b2b_mut::<Pkcs7>(&compressed, &mut buffer)
        .unwrap()
        .len();
    buffer.truncate(written);

    let path = root.join("Raycast 2026-08-22 14.39.05.rayconfig");
    let mut container = iv.to_vec();
    container.extend_from_slice(&buffer);
    fs::write(&path, container).unwrap();
    path
}

fn parse_rayconfig_export(
    path: impl AsRef<Path>,
    password: Option<&str>,
) -> Result<ImportParseReport, crate::ImportError> {
    with_parser_permit(|permit| {
        let detected =
            detect_export_with_permit(path.as_ref(), permit, ImportParseLimits::default())?;
        let secret = password.map(crate::RayconfigSecret::new);
        crate::parse_detected_export_report_with_permit(
            &detected,
            secret.as_ref(),
            permit,
            ImportParseLimits::default(),
        )
    })
}

const EQUIVALENCE_RECORDS: &str = r#"[
    {"createdAt":"2026-08-22T12:00:00Z","modifiedAt":"2026-08-22T12:00:00Z",
     "category":"text","copyCount":3,"applicationPath":"/Applications/Synthetic.app",
     "text":"synthetic entry one"},
    {"createdAt":"2026-08-22T12:01:00Z","modifiedAt":"2026-08-22T12:01:00Z",
     "category":"link","copyCount":1,"text":"https://example.invalid/synthetic"}
]"#;

#[test]
fn an_encrypted_export_yields_exactly_what_the_plain_one_does() {
    // The records array inside a .rayconfig is the same array clipboard.json
    // holds, which is why the Raycast mapper needed no changes at all. This is
    // that claim as a test rather than a note.
    let plain_root = TempDir::new().unwrap();
    let encrypted_root = TempDir::new().unwrap();
    let plain_path = write_export(plain_root.path(), EQUIVALENCE_RECORDS);
    let encrypted_path = write_rayconfig_export(encrypted_root.path(), EQUIVALENCE_RECORDS);

    let plain = parse_export_report(&plain_path).unwrap();
    let encrypted =
        parse_rayconfig_export(&encrypted_path, Some(SYNTHETIC_RAYCONFIG_PASSWORD)).unwrap();

    assert_eq!(plain.total, encrypted.total);
    assert_eq!(plain.candidates.len(), encrypted.candidates.len());
    for (plain, encrypted) in plain.candidates.iter().zip(encrypted.candidates.iter()) {
        assert_eq!(plain.record_fingerprint, encrypted.record_fingerprint);
        assert_eq!(plain.capture.kind, encrypted.capture.kind);
        assert_eq!(plain.capture.captured_at_ms, encrypted.capture.captured_at_ms);
        assert_eq!(plain.capture.source_app_name, encrypted.capture.source_app_name);
    }
}

#[test]
fn an_encrypted_export_without_a_password_says_which_one_is_missing() {
    let root = TempDir::new().unwrap();
    let path = write_rayconfig_export(root.path(), EQUIVALENCE_RECORDS);

    let Err(error) = parse_rayconfig_export(&path, None) else {
        panic!("an encrypted export must not parse without a password");
    };

    assert_eq!(
        error.to_string(),
        crate::ImportError::export(ImportSource::Raycast.as_str(), "rayconfig_password_required")
            .to_string()
    );
}

#[test]
fn an_encrypted_export_with_the_wrong_password_imports_nothing() {
    let root = TempDir::new().unwrap();
    let path = write_rayconfig_export(root.path(), EQUIVALENCE_RECORDS);

    let Err(error) = parse_rayconfig_export(&path, Some("not-the-password")) else {
        panic!("a wrong password must never parse");
    };

    assert_eq!(
        error.to_string(),
        crate::ImportError::export(ImportSource::Raycast.as_str(), "rayconfig_password_invalid")
            .to_string()
    );
}

#[test]
fn a_directory_prefers_a_plain_manifest_over_an_encrypted_one() {
    // Both present means the password is not needed, so it is not asked for.
    let root = TempDir::new().unwrap();
    write_export(root.path(), EQUIVALENCE_RECORDS);
    write_rayconfig_export(root.path(), EQUIVALENCE_RECORDS);

    let detected = detect_export(root.path()).unwrap();

    assert_eq!(detected.source, ImportSource::Raycast);
    assert!(!detected.encrypted);
}

#[test]
fn a_directory_holding_only_an_encrypted_export_selects_it() {
    let root = TempDir::new().unwrap();
    write_rayconfig_export(root.path(), EQUIVALENCE_RECORDS);

    let detected = detect_export(root.path()).unwrap();

    assert_eq!(detected.source, ImportSource::Raycast);
    assert!(detected.encrypted);
}

#[test]
fn an_unnamed_json_manifest_still_beats_an_encrypted_export() {
    // Precedence is by class, not by which file the directory listed first.
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("exported.json"), EQUIVALENCE_RECORDS).unwrap();
    write_rayconfig_export(root.path(), EQUIVALENCE_RECORDS);

    let detected = detect_export(root.path()).unwrap();

    assert!(!detected.encrypted);
}

#[test]
fn two_encrypted_exports_in_one_directory_are_refused_rather_than_guessed() {
    let root = TempDir::new().unwrap();
    write_rayconfig_export(root.path(), EQUIVALENCE_RECORDS);
    fs::copy(
        root.path().join("Raycast 2026-08-22 14.39.05.rayconfig"),
        root.path().join("Raycast 2026-08-23 09.00.00.rayconfig"),
    )
    .unwrap();

    let error = detect_export(root.path()).unwrap_err();

    assert_eq!(
        error.to_string(),
        crate::ImportError::export("detection", "ambiguous_manifest").to_string()
    );
}
};
}

pub(crate) use relocated_parser_tests;
