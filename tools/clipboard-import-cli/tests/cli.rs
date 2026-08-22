use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde_json::{Value, json};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_clipboard-import-cli"))
        .args(args)
        .output()
        .unwrap()
}

fn utf8_output(output: &Output) -> (&str, &str) {
    (
        std::str::from_utf8(&output.stdout).unwrap(),
        std::str::from_utf8(&output.stderr).unwrap(),
    )
}

fn json_output(output: &Output) -> Value {
    let (stdout, _) = utf8_output(output);
    serde_json::from_str(stdout).unwrap()
}

fn assert_redacted(output: &Output, sentinels: &[&str]) {
    let (stdout, stderr) = utf8_output(output);
    for sentinel in sentinels {
        assert!(!stdout.contains(sentinel));
        assert!(!stderr.contains(sentinel));
    }
}

fn write_raycast_export(root: &Path, records: Value) {
    fs::create_dir_all(root).unwrap();
    fs::write(
        root.join("clipboard.json"),
        serde_json::to_vec(&records).unwrap(),
    )
    .unwrap();
}

fn valid_raycast_export(root: &Path) {
    write_raycast_export(
        root,
        json!([{
            "createdAt": "2026-08-22T12:00:00Z",
            "modifiedAt": "2026-08-22T12:00:00Z",
            "category": "text",
            "copyCount": 1,
            "applicationPath": "/Applications/Fixture.app",
            "text": "sanitized fixture payload"
        }]),
    );
}

fn run_import(source: &Path, data_dir: &Path) -> Output {
    cli(&[
        "import",
        "--source",
        source.to_str().unwrap(),
        "--data-dir",
        data_dir.to_str().unwrap(),
    ])
}

#[test]
fn analyze_emits_only_explicit_sanitized_count_fields() {
    let sandbox = tempfile::tempdir().unwrap();
    let source = sandbox.path().join("analysis-source-sentinel");
    valid_raycast_export(&source);

    let output = cli(&["analyze", "--source", source.to_str().unwrap()]);

    assert!(output.status.success());
    let value = json_output(&output);
    assert_eq!(value["status"], "ok");
    assert_eq!(value["sourceKind"], "raycast");
    assert_eq!(value["total"], 1);
    assert_eq!(value["candidateRecords"], 1);
    assert_eq!(value["failed"], 0);
    assert!(value.get("sourcePath").is_none());
    assert!(value.get("sourceFingerprint").is_none());
    assert_redacted(
        &output,
        &["analysis-source-sentinel", "sanitized fixture payload"],
    );
}

#[test]
fn analyze_reports_missing_payload_counts_by_kind() {
    let sandbox = tempfile::tempdir().unwrap();
    let source = sandbox.path().join("analysis-source");
    write_raycast_export(
        &source,
        json!([
            {
                "createdAt": "2026-08-22T12:00:00Z",
                "modifiedAt": "2026-08-22T12:00:00Z",
                "category": "text",
                "copyCount": 1,
                "applicationPath": "/Applications/Fixture.app",
                "text": "sanitized fixture payload"
            },
            {
                "createdAt": "2026-08-22T12:01:00Z",
                "modifiedAt": "2026-08-22T12:01:00Z",
                "category": "image",
                "copyCount": 1,
                "applicationPath": "/Applications/Fixture.app",
                "text": "",
                "imageHash": "sanitized-missing-image"
            }
        ]),
    );

    let output = cli(&["analyze", "--source", source.to_str().unwrap()]);

    assert!(output.status.success());
    let value = json_output(&output);
    let images = value["countsByKind"]
        .as_array()
        .unwrap()
        .iter()
        .find(|count| count["kind"] == "image")
        .unwrap();
    assert_eq!(images["eventCount"], 1);
    assert_eq!(images["missingPayloadCount"], 1);
    assert_eq!(value["missingImageRecords"], 1);
    assert_redacted(
        &output,
        &["sanitized fixture payload", "sanitized-missing-image"],
    );
}

#[cfg(unix)]
#[test]
fn rejects_filesystem_root_without_echoing_source_or_payload() {
    let sandbox = tempfile::tempdir().unwrap();
    let source = sandbox.path().join("private-source-sentinel");
    valid_raycast_export(&source);

    let output = run_import(&source, Path::new("/"));

    assert!(!output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stderr).unwrap()["code"],
        "unsafe_data_dir"
    );
    assert_redacted(
        &output,
        &["private-source-sentinel", "sanitized fixture payload"],
    );
}

#[test]
fn rejects_the_home_directory_itself() {
    let sandbox = tempfile::tempdir().unwrap();
    let source = sandbox.path().join("source");
    valid_raycast_export(&source);
    let home = directories::BaseDirs::new()
        .unwrap()
        .home_dir()
        .to_path_buf();

    let output = run_import(&source, &home);

    assert!(!output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stderr).unwrap()["code"],
        "unsafe_data_dir"
    );
}

#[test]
fn rejects_equal_ancestor_and_descendant_data_directories() {
    let sandbox = tempfile::tempdir().unwrap();
    let source = sandbox.path().join("exports/source");
    valid_raycast_export(&source);
    let cases = [
        source.clone(),
        sandbox.path().join("exports"),
        source.join("nested-data"),
    ];

    for data_dir in cases {
        let output = run_import(&source, &data_dir);
        assert!(!output.status.success());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stderr).unwrap()["code"],
            "overlapping_paths"
        );
    }
}

#[cfg(unix)]
#[test]
fn rejects_a_symlink_alias_that_overlaps_the_source() {
    use std::os::unix::fs::symlink;

    let sandbox = tempfile::tempdir().unwrap();
    let source = sandbox.path().join("source");
    valid_raycast_export(&source);
    let alias = sandbox.path().join("source-alias");
    symlink(&source, &alias).unwrap();

    let output = run_import(&source, &alias);

    assert!(!output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stderr).unwrap()["code"],
        "overlapping_paths"
    );
}

#[test]
fn creates_only_the_validated_database_and_blob_locations_for_a_new_target() {
    let sandbox = tempfile::tempdir().unwrap();
    let source = sandbox.path().join("source");
    let data_dir = sandbox.path().join("generated/new/dev");
    valid_raycast_export(&source);

    let output = run_import(&source, &data_dir);

    assert!(output.status.success());
    assert!(data_dir.join("clipboard.db").is_file());
    assert!(data_dir.join("blobs").is_dir());
    assert!(!data_dir.join("clipboard.blobs").exists());
    let value = json_output(&output);
    assert_eq!(value["status"], "ok");
    assert_eq!(value["total"], 1);
    assert_eq!(value["imported"], 1);
}

#[test]
fn verify_of_a_missing_database_is_logically_read_only_and_noncreating() {
    let sandbox = tempfile::tempdir().unwrap();
    let data_dir = sandbox.path().join("missing-dev");

    let output = cli(&[
        "verify",
        "--data-dir",
        data_dir.to_str().unwrap(),
        "--expect-records",
        "1",
    ]);

    assert!(!output.status.success());
    assert!(!data_dir.exists());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stderr).unwrap()["code"],
        "database_missing"
    );
}

#[test]
fn all_error_channels_redact_source_paths_filenames_and_payloads() {
    let sandbox = tempfile::tempdir().unwrap();
    let source_name = "private-source-path-sentinel";
    let file_name = "private-filename-sentinel.json";
    let payload = "private-payload-sentinel";
    let source = sandbox.path().join(source_name);
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join(file_name), payload).unwrap();

    let output = cli(&["analyze", "--source", source.to_str().unwrap()]);

    assert!(!output.status.success());
    assert_redacted(&output, &[source_name, file_name, payload]);
    let (_, stderr) = utf8_output(&output);
    assert_eq!(
        serde_json::from_str::<Value>(stderr).unwrap()["status"],
        "error"
    );
}

#[test]
fn two_source_import_repeat_and_verify_report_only_sanitized_accounting() {
    let sandbox = tempfile::tempdir().unwrap();
    let raycast = sandbox.path().join("raycast");
    let supercmd = sandbox.path().join("supercmd");
    let data_dir = sandbox.path().join("app-data");
    write_raycast_export(
        &raycast,
        json!([
            {
                "createdAt": "2026-08-22T12:00:00Z",
                "modifiedAt": "2026-08-22T12:00:00Z",
                "category": "text",
                "copyCount": 1,
                "applicationPath": "/Applications/Fixture.app",
                "text": "shared sanitized payload"
            },
            {
                "createdAt": "2026-08-22T12:01:00Z",
                "modifiedAt": "2026-08-22T12:01:00Z",
                "category": "image",
                "copyCount": 1,
                "applicationPath": "/Applications/Fixture.app",
                "text": "",
                "imageHash": "sanitized-missing-image"
            }
        ]),
    );
    fs::create_dir_all(supercmd.join("images")).unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/clipboard-import/tests/fixtures/supercmd/images/sample.png"),
        supercmd.join("images/available.png"),
    )
    .unwrap();
    fs::write(
        supercmd.join("clipboard.json"),
        serde_json::to_vec(&json!([
            {
                "copied_at": "2026-08-22T12:02:00Z",
                "type": "text",
                "source_app": "Fixture App",
                "bundle_id": "com.example.fixture",
                "text": "shared sanitized payload",
                "has_image": false
            },
            {
                "copied_at": "2026-08-22T12:03:00Z",
                "type": "image",
                "source_app": "Fixture App",
                "bundle_id": "com.example.fixture",
                "file_url": "images/available.png",
                "text": "",
                "has_image": true
            },
            {
                "copied_at": "2026-08-22T12:04:00Z",
                "type": "image",
                "source_app": "Fixture App",
                "bundle_id": "com.example.fixture",
                "file_url": "images/missing.png",
                "text": "",
                "has_image": true
            }
        ]))
        .unwrap(),
    )
    .unwrap();

    let first_raycast = run_import(&raycast, &data_dir);
    let first_supercmd = run_import(&supercmd, &data_dir);
    let second_raycast = run_import(&raycast, &data_dir);
    let second_supercmd = run_import(&supercmd, &data_dir);
    for output in [
        &first_raycast,
        &first_supercmd,
        &second_raycast,
        &second_supercmd,
    ] {
        assert!(output.status.success());
        assert_redacted(
            output,
            &["shared sanitized payload", "available.png", "missing.png"],
        );
    }
    assert_eq!(json_output(&first_raycast)["imported"], 2);
    assert_eq!(json_output(&first_supercmd)["imported"], 3);
    assert_eq!(json_output(&second_raycast)["alreadyPresent"], 2);
    assert_eq!(json_output(&second_supercmd)["alreadyPresent"], 3);

    let verify = cli(&[
        "verify",
        "--data-dir",
        data_dir.to_str().unwrap(),
        "--expect-records",
        "5",
    ]);
    assert!(verify.status.success());
    let value = json_output(&verify);
    assert_eq!(value["status"], "ok");
    assert_eq!(value["latestSourceTotal"], 5);
    assert_eq!(value["eventCount"], 5);
    assert_eq!(value["physicalContentCount"], 4);
    assert_eq!(value["integrityStatus"], "ok");
    assert_eq!(value["ftsStatus"], "ok");
    let sources = value["sources"].as_array().unwrap();
    let raycast_summary = sources
        .iter()
        .find(|summary| summary["sourceKind"] == "raycast")
        .unwrap();
    let supercmd_summary = sources
        .iter()
        .find(|summary| summary["sourceKind"] == "supercmd")
        .unwrap();
    assert_eq!(raycast_summary["total"], 2);
    assert_eq!(raycast_summary["alreadyPresent"], 2);
    assert_eq!(supercmd_summary["total"], 3);
    assert_eq!(supercmd_summary["alreadyPresent"], 3);
    assert_eq!(supercmd_summary["availableImageEvents"], 1);
    assert_eq!(supercmd_summary["missingImageEvents"], 1);
    assert_redacted(
        &verify,
        &["shared sanitized payload", "available.png", "missing.png"],
    );
}
