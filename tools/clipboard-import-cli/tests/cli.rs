use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use clipboard_store::CasStore;
use rusqlite::{Connection, params};
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

fn run_verify(data_dir: &Path, expected_records: u64) -> Output {
    cli(&[
        "verify",
        "--data-dir",
        data_dir.to_str().unwrap(),
        "--expect-records",
        &expected_records.to_string(),
    ])
}

fn imported_two_source_store() -> (tempfile::TempDir, PathBuf) {
    let sandbox = tempfile::tempdir().unwrap();
    let raycast = sandbox.path().join("synthetic-raycast");
    let supercmd = sandbox.path().join("synthetic-supercmd");
    let data_dir = sandbox.path().join("synthetic-data");
    valid_raycast_export(&raycast);
    fs::create_dir_all(&supercmd).unwrap();
    fs::write(
        supercmd.join("clipboard.json"),
        serde_json::to_vec(&json!([{
            "copied_at": "2026-08-22T12:01:00Z",
            "type": "text",
            "source_app": "Synthetic App",
            "bundle_id": "com.example.synthetic",
            "text": "second sanitized fixture payload",
            "has_image": false
        }]))
        .unwrap(),
    )
    .unwrap();
    assert!(run_import(&raycast, &data_dir).status.success());
    assert!(run_import(&supercmd, &data_dir).status.success());
    assert!(run_verify(&data_dir, 2).status.success());
    (sandbox, data_dir)
}

fn imported_store_with_cas() -> (tempfile::TempDir, PathBuf) {
    let sandbox = tempfile::tempdir().unwrap();
    let raycast = sandbox.path().join("synthetic-raycast");
    let supercmd = sandbox.path().join("synthetic-supercmd");
    let data_dir = sandbox.path().join("synthetic-data");
    valid_raycast_export(&raycast);
    fs::create_dir_all(supercmd.join("images")).unwrap();
    fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/clipboard-import/tests/fixtures/supercmd/images/sample.png"),
        supercmd.join("images/synthetic-available.png"),
    )
    .unwrap();
    fs::write(
        supercmd.join("clipboard.json"),
        serde_json::to_vec(&json!([{
            "copied_at": "2026-08-22T12:01:00Z",
            "type": "image",
            "source_app": "Synthetic App",
            "bundle_id": "com.example.synthetic",
            "file_url": "images/synthetic-available.png",
            "text": "",
            "has_image": true
        }]))
        .unwrap(),
    )
    .unwrap();
    assert!(run_import(&raycast, &data_dir).status.success());
    assert!(run_import(&supercmd, &data_dir).status.success());
    assert!(run_verify(&data_dir, 2).status.success());
    (sandbox, data_dir)
}

fn imported_two_source_store_with_zstd() -> (tempfile::TempDir, PathBuf) {
    let sandbox = tempfile::tempdir().unwrap();
    let raycast = sandbox.path().join("synthetic-raycast-zstd");
    let supercmd = sandbox.path().join("synthetic-supercmd-zstd");
    let data_dir = sandbox.path().join("synthetic-data-zstd");
    fs::create_dir(&raycast).unwrap();
    fs::create_dir(&supercmd).unwrap();
    fs::write(
        raycast.join("clipboard.json"),
        serde_json::to_vec(&json!([{
            "createdAt": "2026-08-22T12:00:00Z",
            "modifiedAt": "2026-08-22T12:00:00Z",
            "category": "text",
            "copyCount": 1,
            "text": "z".repeat(5_000)
        }]))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        supercmd.join("clipboard.json"),
        serde_json::to_vec(&json!([{
            "copied_at": "2026-08-22T12:00:01Z",
            "type": "text",
            "text": "synthetic companion",
            "has_image": false
        }]))
        .unwrap(),
    )
    .unwrap();
    assert!(run_import(&raycast, &data_dir).status.success());
    assert!(run_import(&supercmd, &data_dir).status.success());
    assert!(run_verify(&data_dir, 2).status.success());
    (sandbox, data_dir)
}

fn ignore_schema_checks(connection: &Connection) {
    connection
        .execute_batch("PRAGMA ignore_check_constraints = ON;")
        .unwrap();
}

fn synthetic_uuid_v7(seed: u64) -> [u8; 16] {
    let mut value = [0_u8; 16];
    value[..8].copy_from_slice(&seed.to_be_bytes());
    value[6] = (value[6] & 0x0f) | 0x70;
    value[8] = (value[8] & 0x3f) | 0x80;
    value
}

fn synthetic_digest(seed: u64) -> [u8; 32] {
    let mut value = [0_u8; 32];
    value[..8].copy_from_slice(&seed.to_be_bytes());
    value[8..16].copy_from_slice(&seed.rotate_left(17).to_be_bytes());
    value[16..24].copy_from_slice(&seed.rotate_left(31).to_be_bytes());
    value[24..].copy_from_slice(&seed.rotate_left(47).to_be_bytes());
    value
}

fn first_cas_blob_path(data_dir: &Path) -> PathBuf {
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    let relpath: String = connection
        .query_row(
            "SELECT blob_relpath FROM raw_payload
             WHERE storage_kind = 'cas' ORDER BY raw_payload_id LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    data_dir.join("blobs").join(relpath)
}

#[test]
fn verify_audits_primary_representation_presence_and_content_ownership() {
    let (_sandbox, data_dir) = imported_two_source_store();
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    connection
        .execute(
            "DELETE FROM event_representation
             WHERE event_id = (SELECT min(event_id) FROM history_event) AND ordinal = 0",
            [],
        )
        .unwrap();

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "logicalStatus");
}

#[test]
fn verify_audits_inline_raw_digest_and_domain_content_hash() {
    let (_sandbox, data_dir) = imported_two_source_store();
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    let raw_payload_id: i64 = connection
        .query_row(
            "SELECT raw_payload_id FROM raw_payload WHERE storage_kind = 'inline' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    connection
        .execute(
            "UPDATE raw_payload SET inline_payload = zeroblob(original_byte_size)
             WHERE raw_payload_id = ?1",
            [raw_payload_id],
        )
        .unwrap();

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "logicalStatus");
}

#[test]
fn verify_audits_content_size_hash_and_flags() {
    let (_sandbox, data_dir) = imported_two_source_store();
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    connection
        .execute(
            "UPDATE content
             SET byte_size = byte_size + 1,
                 content_hash = zeroblob(32),
                 flags = flags | ?1
             WHERE content_id = (SELECT min(content_id) FROM content)",
            [i64::from(
                clipboard_core::ContentFlags::MISSING_PAYLOAD.bits(),
            )],
        )
        .unwrap();

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "logicalStatus");
}

#[test]
fn verify_rejects_zstd_trailing_data_and_declared_size_bombs() {
    let sandbox = tempfile::tempdir().unwrap();
    let raycast = sandbox.path().join("synthetic-raycast-zstd");
    let supercmd = sandbox.path().join("synthetic-supercmd-zstd");
    let data_dir = sandbox.path().join("synthetic-data-zstd");
    fs::create_dir(&raycast).unwrap();
    fs::create_dir(&supercmd).unwrap();
    fs::write(
        raycast.join("clipboard.json"),
        serde_json::to_vec(&json!([{
            "createdAt": "2026-08-22T12:00:00Z",
            "modifiedAt": "2026-08-22T12:00:00Z",
            "category": "text",
            "copyCount": 1,
            "text": "z".repeat(5_000)
        }]))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        supercmd.join("clipboard.json"),
        serde_json::to_vec(&json!([{
            "copied_at": "2026-08-22T12:00:01Z",
            "type": "text",
            "text": "synthetic companion",
            "has_image": false
        }]))
        .unwrap(),
    )
    .unwrap();
    assert!(run_import(&raycast, &data_dir).status.success());
    assert!(run_import(&supercmd, &data_dir).status.success());
    assert!(run_verify(&data_dir, 2).status.success());

    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    connection
        .execute(
            "UPDATE raw_payload
             SET inline_payload = CAST(inline_payload || x'00' AS BLOB),
                 stored_byte_size = stored_byte_size + 1,
                 original_byte_size = 4096
             WHERE storage_kind = 'inline_zstd'",
            [],
        )
        .unwrap();
    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "logicalStatus");
}

fn imported_store_with_a_file_source_reference() -> (tempfile::TempDir, PathBuf) {
    let sandbox = tempfile::tempdir().unwrap();
    let raycast = sandbox.path().join("synthetic-raycast-reference");
    let supercmd = sandbox.path().join("synthetic-supercmd-reference");
    let data_dir = sandbox.path().join("synthetic-data-reference");
    fs::create_dir(&raycast).unwrap();
    fs::create_dir(&supercmd).unwrap();
    fs::write(
        raycast.join("clipboard.json"),
        serde_json::to_vec(&json!([{
            "createdAt": "2026-08-22T12:00:00Z",
            "modifiedAt": "2026-08-22T12:00:00Z",
            "category": "file",
            "copyCount": 1,
            "text": "synthetic.pdf",
            "textContent": "",
            "filePath": "/synthetic/outside/synthetic.pdf"
        }]))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        supercmd.join("clipboard.json"),
        serde_json::to_vec(&json!([{
            "copied_at": "2026-08-22T12:00:01Z",
            "type": "text",
            "text": "synthetic companion",
            "has_image": false
        }]))
        .unwrap(),
    )
    .unwrap();
    for source in [&raycast, &supercmd] {
        let output = cli(&[
            "import",
            "--source",
            source.to_str().unwrap(),
            "--data-dir",
            data_dir.to_str().unwrap(),
        ]);
        assert!(output.status.success());
    }
    (sandbox, data_dir)
}

#[test]
fn a_file_entry_keeps_its_inline_source_reference_and_still_verifies() {
    let (_sandbox, data_dir) = imported_store_with_a_file_source_reference();
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    let storage_kind = connection
        .query_row(
            "SELECT rp.storage_kind
             FROM event_representation er
             JOIN raw_payload rp ON rp.raw_payload_id = er.raw_payload_id
             WHERE er.format_id = 'text/uri-list'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    drop(connection);

    // A fifty-byte reference belongs inline even though the entry is a file.
    assert_eq!(storage_kind, "inline");

    let output = run_verify(&data_dir, 2);

    assert!(output.status.success());
    assert_eq!(json_output(&output)["logicalStatus"], "ok");
}

#[test]
fn a_binary_representation_of_a_file_entry_must_still_live_in_the_blob_store() {
    let (_sandbox, data_dir) = imported_store_with_a_file_source_reference();
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    ignore_schema_checks(&connection);
    connection
        .execute(
            "UPDATE event_representation
             SET format_id = 'application/octet-stream'
             WHERE format_id = 'text/uri-list'",
            [],
        )
        .unwrap();
    drop(connection);

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "logicalStatus");
}

#[test]
fn bounded_payload_verification_rejects_oversized_inline_metadata_without_disclosure() {
    let (_sandbox, data_dir) = imported_two_source_store_with_zstd();
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    ignore_schema_checks(&connection);
    connection
        .execute(
            "UPDATE raw_payload
             SET inline_payload = zeroblob(4096), original_byte_size = 1, stored_byte_size = 1
             WHERE storage_kind = 'inline'",
            [],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE raw_payload
             SET inline_payload = zeroblob(263169), original_byte_size = 262144,
                 stored_byte_size = 263168
             WHERE storage_kind = 'inline_zstd'",
            [],
        )
        .unwrap();

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "logicalStatus");
}

#[test]
fn bounded_payload_verification_handles_the_exact_zstd_bound_and_rejects_one_byte_above() {
    for stored_size in [263_168_i64, 263_169_i64] {
        let (_sandbox, data_dir) = imported_two_source_store_with_zstd();
        let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
        ignore_schema_checks(&connection);
        connection
            .execute(
                "UPDATE raw_payload
                 SET inline_payload = zeroblob(?1), original_byte_size = 262144,
                     stored_byte_size = ?1
                 WHERE storage_kind = 'inline_zstd'",
                [stored_size],
            )
            .unwrap();

        let output = run_verify(&data_dir, 2);

        assert_failed_verification(&output, "logicalStatus");
    }
}

#[test]
fn bounded_payload_verification_rejects_oversized_text_and_path_metadata_without_disclosure() {
    let (_sandbox, data_dir) = imported_store_with_cas();
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    ignore_schema_checks(&connection);
    let mime_sentinel = format!("oversized-mime-private-sentinel-{}", "m".repeat(1_024));
    let missing_sentinel = format!("oversized-missing-private-sentinel-{}", "r".repeat(4_096));
    let path_sentinel = format!("oversized-path-private-sentinel-{}", "p".repeat(80));
    let artifact_sentinel = format!("oversized-artifact-private-sentinel-{}", "a".repeat(64));
    connection
        .execute(
            "UPDATE content SET primary_mime = ?1
             WHERE content_id = (SELECT min(content_id) FROM content)",
            [&mime_sentinel],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE event_representation
             SET raw_payload_id = NULL, missing_ref = ?1
             WHERE event_id = (SELECT min(event_id) FROM history_event) AND ordinal = 0",
            [&missing_sentinel],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE raw_payload SET blob_relpath = ?1 WHERE storage_kind = 'cas'",
            [&path_sentinel],
        )
        .unwrap();
    let (content_id, blob_relpath, byte_size, raw_digest) = connection
        .query_row(
            "SELECT (SELECT min(content_id) FROM content), blob_relpath,
                    original_byte_size, raw_digest
             FROM raw_payload WHERE storage_kind = 'cas' LIMIT 1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                ))
            },
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO artifact(
               content_id, artifact_kind, blob_relpath, byte_size, raw_digest, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, 1)",
            params![
                content_id,
                artifact_sentinel,
                blob_relpath,
                byte_size,
                raw_digest
            ],
        )
        .unwrap();

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "logicalStatus");
    assert_redacted(
        &output,
        &[
            "oversized-mime-private-sentinel",
            "oversized-missing-private-sentinel",
            "oversized-path-private-sentinel",
            "oversized-artifact-private-sentinel",
        ],
    );
}

#[test]
fn bounded_payload_verification_rejects_unknown_content_kind_as_aggregate_status() {
    let (_sandbox, data_dir) = imported_two_source_store();
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    ignore_schema_checks(&connection);
    let sentinel = "unknown-kind-private-sentinel";
    connection
        .execute(
            "UPDATE content SET kind = ?1 WHERE content_id = (SELECT min(content_id) FROM content)",
            [sentinel],
        )
        .unwrap();

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "logicalStatus");
    assert_redacted(&output, &[sentinel]);
}

#[test]
fn verify_pages_more_than_256_blob_references_without_collecting_them() {
    let (_sandbox, data_dir) = imported_two_source_store();
    let cas = CasStore::new(data_dir.join("blobs"));
    let blob = cas.put(b"synthetic shared artifact").unwrap();
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    let content_id: i64 = connection
        .query_row("SELECT min(content_id) FROM content", [], |row| row.get(0))
        .unwrap();
    for index in 0..300 {
        connection
            .execute(
                "INSERT INTO artifact(
                   content_id, artifact_kind, blob_relpath, byte_size, raw_digest, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 1)",
                params![
                    content_id,
                    format!("synthetic-{index}"),
                    blob.relpath,
                    i64::try_from(blob.byte_size).unwrap(),
                    blob.hash.as_slice(),
                ],
            )
            .unwrap();
    }

    let output = run_verify(&data_dir, 2);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn source_summary_returns_at_most_two_rows_across_many_fingerprints() {
    let (_sandbox, data_dir) = imported_two_source_store();
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    for (source_index, source_kind) in ["raycast", "supercmd"].into_iter().enumerate() {
        for fingerprint_index in 0_u64..300 {
            let seed = 10_000 + (source_index as u64 * 1_000) + fingerprint_index;
            connection
                .execute(
                    "INSERT INTO import_run(
                       external_id, source_kind, source_fingerprint,
                       initial_failure_fingerprint, status, total_records,
                       candidate_records, next_candidate_offset, imported_records,
                       already_present_records, skipped_records, failed_records,
                       started_at_ms, finished_at_ms
                     ) VALUES (?1, ?2, ?3, ?4, 'completed', 0, 0, 0, 0, 0, 0, 0, ?5, ?5)",
                    params![
                        synthetic_uuid_v7(seed).as_slice(),
                        source_kind,
                        synthetic_digest(seed).as_slice(),
                        synthetic_digest(seed.wrapping_add(90_000)).as_slice(),
                        i64::try_from(seed).unwrap(),
                    ],
                )
                .unwrap();
        }
    }

    let output = run_verify(&data_dir, 2);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let value = json_output(&output);
    assert_eq!(value["status"], "ok");
    assert!(value["sources"].as_array().unwrap().len() <= 2);
}

#[test]
fn source_summary_rejects_an_unknown_run_without_collecting_or_disclosing_it() {
    let (_sandbox, data_dir) = imported_two_source_store();
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    ignore_schema_checks(&connection);
    let sentinel = "unexpected-source-private-sentinel";
    connection
        .execute(
            "INSERT INTO import_run(
               external_id, source_kind, source_fingerprint,
               initial_failure_fingerprint, status, total_records,
               candidate_records, next_candidate_offset, imported_records,
               already_present_records, skipped_records, failed_records,
               started_at_ms, finished_at_ms
             ) VALUES (?1, ?2, ?3, ?4, 'completed', 0, 0, 0, 0, 0, 0, 0, 9, 9)",
            params![
                synthetic_uuid_v7(99_001).as_slice(),
                sentinel,
                synthetic_digest(99_002).as_slice(),
                synthetic_digest(99_003).as_slice(),
            ],
        )
        .unwrap();

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "logicalStatus");
    assert!(json_output(&output)["sources"].as_array().unwrap().len() <= 2);
    assert_redacted(&output, &[sentinel]);
}

#[test]
fn source_summary_rejects_an_unknown_import_record_without_collecting_it() {
    let (_sandbox, data_dir) = imported_two_source_store();
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    ignore_schema_checks(&connection);
    let sentinel = "unexpected-record-source-private-sentinel";
    connection
        .execute(
            "UPDATE import_record SET source_kind = ?1
             WHERE import_record_id = (SELECT min(import_record_id) FROM import_record)",
            [sentinel],
        )
        .unwrap();

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "logicalStatus");
    assert!(json_output(&output)["sources"].as_array().unwrap().len() <= 2);
    assert_redacted(&output, &[sentinel]);
}

#[test]
fn transaction_boundary_epoch_change_discards_all_aggregate_claims() {
    let (_sandbox, data_dir) = imported_two_source_store();
    let database = data_dir.join("clipboard.db");
    let keep_writing = Arc::new(AtomicBool::new(true));
    let writer_flag = Arc::clone(&keep_writing);
    let (first_commit_sender, first_commit_receiver) = mpsc::sync_channel(1);
    let writer = thread::spawn(move || {
        let connection = Connection::open(database).unwrap();
        let mut first = true;
        while writer_flag.load(Ordering::Relaxed) {
            connection
                .execute(
                    "UPDATE history_event SET paste_count = paste_count + 1
                     WHERE event_id = (SELECT min(event_id) FROM history_event)",
                    [],
                )
                .unwrap();
            if first {
                first_commit_sender.send(()).unwrap();
                first = false;
            }
            thread::sleep(Duration::from_millis(1));
        }
    });
    first_commit_receiver
        .recv_timeout(Duration::from_secs(1))
        .unwrap();

    let output = run_verify(&data_dir, 2);
    keep_writing.store(false, Ordering::Relaxed);
    writer.join().unwrap();

    assert!(!output.status.success());
    let value = json_output(&output);
    assert_eq!(value["status"], "failed");
    assert_eq!(value["latestSourceTotal"], 0);
    assert_eq!(value["physicalContentCount"], 0);
    assert_eq!(value["eventCount"], 0);
    assert_eq!(value["indexedDocumentCount"], 0);
    assert_eq!(value["missingPayloadCount"], 0);
    assert_eq!(value["countsByKind"], json!([]));
    assert_eq!(value["sources"], json!([]));
    for field in [
        "integrityStatus",
        "runStatus",
        "logicalStatus",
        "blobStatus",
        "ftsStatus",
    ] {
        assert_eq!(value[field], "failed");
    }
}

fn insert_unfinished_run(data_dir: &Path, status: &str, discriminator: u8) {
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    let mut external_id = [0_u8; 16];
    external_id[0] = discriminator;
    external_id[6] = 0x70;
    external_id[8] = 0x80;
    let source_fingerprint = [discriminator; 32];
    let initial_failure_fingerprint = [discriminator.wrapping_add(1); 32];
    let finished_at_ms = (status == "failed").then_some(2_i64);
    connection
        .execute(
            "INSERT INTO import_run(
               external_id, source_kind, source_fingerprint,
               initial_failure_fingerprint, status, total_records,
               candidate_records, next_candidate_offset, imported_records,
               already_present_records, skipped_records, failed_records,
               started_at_ms, finished_at_ms, error_code
             ) VALUES (?1, 'raycast', ?2, ?3, ?4, 0, 0, 0, 0, 0, 0, 0, 1, ?5, ?6)",
            params![
                external_id.as_slice(),
                source_fingerprint.as_slice(),
                initial_failure_fingerprint.as_slice(),
                status,
                finished_at_ms,
                (status == "failed").then_some("synthetic_failure")
            ],
        )
        .unwrap();
}

fn assert_failed_verification(output: &Output, failed_field: &str) {
    assert!(!output.status.success());
    let value = json_output(output);
    assert_eq!(value["status"], "failed");
    assert_eq!(value[failed_field], "failed");
    assert_redacted(
        output,
        &[
            "synthetic-raycast",
            "synthetic-supercmd",
            "synthetic-data",
            "sanitized fixture payload",
        ],
    );
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
fn bounded_analyze_rejects_an_oversized_manifest_before_parsing() {
    let sandbox = tempfile::tempdir().unwrap();
    let source = sandbox.path().join("bounded-analysis-sentinel");
    fs::create_dir_all(&source).unwrap();
    let manifest = source.join("clipboard.json");
    fs::File::create(&manifest)
        .unwrap()
        .set_len((clipboard_import::MAX_IMPORT_MANIFEST_BYTES as u64) + 1)
        .unwrap();

    let output = cli(&["analyze", "--source", source.to_str().unwrap()]);

    assert!(!output.status.success());
    let (_, stderr) = utf8_output(&output);
    assert_eq!(
        serde_json::from_str::<Value>(stderr).unwrap()["code"],
        "analysis_too_large"
    );
    assert_redacted(&output, &["bounded-analysis-sentinel", "clipboard.json"]);
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
fn verify_fails_when_any_import_run_is_still_running() {
    let (_sandbox, data_dir) = imported_two_source_store();
    insert_unfinished_run(&data_dir, "running", 41);

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "runStatus");
}

#[test]
fn verify_fails_when_any_import_run_has_failed() {
    let (_sandbox, data_dir) = imported_two_source_store();
    insert_unfinished_run(&data_dir, "failed", 42);

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "runStatus");
}

#[test]
fn verify_fails_when_a_non_image_import_record_is_deleted() {
    let (_sandbox, data_dir) = imported_two_source_store();
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    connection
        .execute(
            "DELETE FROM import_record
             WHERE import_record_id = (
               SELECT import_record_id FROM import_record
               WHERE source_kind = 'raycast' ORDER BY import_record_id LIMIT 1
             )",
            [],
        )
        .unwrap();

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "logicalStatus");
}

#[test]
fn verify_fails_when_an_import_record_source_disagrees_with_its_run() {
    let (_sandbox, data_dir) = imported_two_source_store();
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    connection
        .execute(
            "UPDATE import_record
             SET source_kind = 'supercmd'
             WHERE import_record_id = (
               SELECT import_record_id FROM import_record
               WHERE source_kind = 'raycast' ORDER BY import_record_id LIMIT 1
             )",
            [],
        )
        .unwrap();

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "logicalStatus");
}

#[test]
fn verify_fails_when_an_import_record_event_is_null() {
    let (_sandbox, data_dir) = imported_two_source_store();
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    connection
        .execute(
            "UPDATE import_record SET event_id = NULL
             WHERE import_record_id = (
               SELECT import_record_id FROM import_record ORDER BY import_record_id LIMIT 1
             )",
            [],
        )
        .unwrap();

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "logicalStatus");
}

#[test]
fn verify_fails_when_two_import_records_point_to_one_event() {
    let (_sandbox, data_dir) = imported_two_source_store();
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    connection
        .execute(
            "UPDATE import_record
             SET event_id = (SELECT min(event_id) FROM history_event)
             WHERE event_id = (SELECT max(event_id) FROM history_event)",
            [],
        )
        .unwrap();

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "logicalStatus");
}

#[test]
fn verify_fails_foreign_key_check_without_disclosing_database_details() {
    let (_sandbox, data_dir) = imported_two_source_store();
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    connection
        .execute(
            "INSERT INTO artifact(
               content_id, artifact_kind, blob_relpath, byte_size, raw_digest, created_at_ms
             ) VALUES (
               9223372036854775806, 'synthetic',
               '00/0000000000000000000000000000000000000000000000000000000000000000',
               0, zeroblob(32), 1
             )",
            [],
        )
        .unwrap();

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "logicalStatus");
}

#[test]
fn verify_reports_failed_blob_status_when_the_exact_blob_root_is_missing() {
    let (_sandbox, data_dir) = imported_two_source_store();
    fs::remove_dir(data_dir.join("blobs")).unwrap();

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "blobStatus");
}

#[cfg(unix)]
#[test]
fn verify_reports_private_storage_with_only_the_stable_json_code() {
    use std::os::unix::fs::PermissionsExt;

    let (sandbox, data_dir) = imported_two_source_store();
    let database = data_dir.join("clipboard.db");
    fs::set_permissions(&database, fs::Permissions::from_mode(0o644)).unwrap();

    let output = run_verify(&data_dir, 2);

    assert!(!output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stderr).unwrap(),
        json!({"status": "error", "code": "private_storage_unavailable"})
    );
    assert_redacted(
        &output,
        &[
            "clipboard.db",
            "synthetic-data",
            "sanitized fixture payload",
        ],
    );
    drop(sandbox);
}

#[cfg(unix)]
#[test]
fn verify_preserves_private_storage_error_from_a_real_cas_object() {
    use std::os::unix::fs::PermissionsExt;

    let (sandbox, data_dir) = imported_store_with_cas();
    let blob_path = first_cas_blob_path(&data_dir);
    let blob_name = blob_path.file_name().unwrap().to_str().unwrap().to_owned();
    fs::set_permissions(&blob_path, fs::Permissions::from_mode(0o644)).unwrap();

    let output = run_verify(&data_dir, 2);

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stderr).unwrap(),
        json!({"status": "error", "code": "private_storage_unavailable"})
    );
    assert_redacted(
        &output,
        &[&blob_name, "synthetic-data", "synthetic-available.png"],
    );
    drop(sandbox);
}

#[cfg(unix)]
#[test]
fn verify_preserves_private_storage_error_from_a_real_cas_shard() {
    use std::os::unix::fs::PermissionsExt;

    let (sandbox, data_dir) = imported_store_with_cas();
    let blob_path = first_cas_blob_path(&data_dir);
    let shard_path = blob_path.parent().unwrap();
    let shard_name = shard_path.file_name().unwrap().to_str().unwrap().to_owned();
    let blob_name = blob_path.file_name().unwrap().to_str().unwrap().to_owned();
    fs::set_permissions(shard_path, fs::Permissions::from_mode(0o755)).unwrap();

    let output = run_verify(&data_dir, 2);

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stderr).unwrap(),
        json!({"status": "error", "code": "private_storage_unavailable"})
    );
    assert_redacted(
        &output,
        &[
            &shard_name,
            &blob_name,
            "synthetic-data",
            "synthetic-available.png",
        ],
    );
    drop(sandbox);
}

#[test]
fn verify_reports_failed_blob_status_for_a_missing_cas_blob() {
    let (_sandbox, data_dir) = imported_store_with_cas();
    let blob_path = first_cas_blob_path(&data_dir);
    fs::remove_file(blob_path).unwrap();

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "blobStatus");
}

#[test]
fn verify_reports_failed_blob_status_for_corrupt_cas_bytes() {
    let (_sandbox, data_dir) = imported_store_with_cas();
    let blob_path = first_cas_blob_path(&data_dir);
    fs::write(blob_path, b"synthetic corrupt bytes").unwrap();

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "blobStatus");
}

#[cfg(unix)]
#[test]
fn verify_reports_failed_blob_status_for_a_symlinked_cas_blob() {
    use std::os::unix::fs::symlink;

    let (sandbox, data_dir) = imported_store_with_cas();
    let blob_path = first_cas_blob_path(&data_dir);
    let bytes = fs::read(&blob_path).unwrap();
    fs::remove_file(&blob_path).unwrap();
    let outside = sandbox.path().join("synthetic-outside-blob");
    fs::write(&outside, bytes).unwrap();
    symlink(outside, blob_path).unwrap();

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "blobStatus");
}

#[test]
fn verify_checks_artifact_blob_sizes_through_the_cas_boundary() {
    let (_sandbox, data_dir) = imported_two_source_store();
    let cas = CasStore::new(data_dir.join("blobs"));
    let blob = cas.put(b"synthetic artifact bytes").unwrap();
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    let content_id: i64 = connection
        .query_row(
            "SELECT content_id FROM content ORDER BY content_id LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO artifact(
               content_id, artifact_kind, blob_relpath, byte_size, raw_digest, created_at_ms
             ) VALUES (?1, 'synthetic-artifact', ?2, ?3, ?4, 1)",
            params![
                content_id,
                blob.relpath,
                i64::try_from(blob.byte_size).unwrap() + 1,
                blob.hash.as_slice(),
            ],
        )
        .unwrap();

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "blobStatus");
}

#[test]
fn verify_checks_artifact_digests_against_content_addressed_storage() {
    let (_sandbox, data_dir) = imported_two_source_store();
    let cas = CasStore::new(data_dir.join("blobs"));
    let blob = cas.put(b"synthetic artifact digest bytes").unwrap();
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    let content_id: i64 = connection
        .query_row(
            "SELECT content_id FROM content ORDER BY content_id LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut mismatched_digest = blob.hash;
    mismatched_digest[1] ^= 0x01;
    connection
        .execute(
            "INSERT INTO artifact(
               content_id, artifact_kind, blob_relpath, byte_size, raw_digest, created_at_ms
             ) VALUES (?1, 'synthetic-digest', ?2, ?3, ?4, 1)",
            params![
                content_id,
                blob.relpath,
                i64::try_from(blob.byte_size).unwrap(),
                mismatched_digest.as_slice(),
            ],
        )
        .unwrap();

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "blobStatus");
}

#[test]
fn verify_reads_artifact_references_through_the_cas_boundary() {
    let (_sandbox, data_dir) = imported_two_source_store();
    let blob_root = data_dir.join("blobs");
    let cas = CasStore::new(&blob_root);
    let blob = cas.put(b"synthetic artifact boundary bytes").unwrap();
    let blob_path = blob_root.join(&blob.relpath);
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    let content_id: i64 = connection
        .query_row(
            "SELECT content_id FROM content ORDER BY content_id LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO artifact(
               content_id, artifact_kind, blob_relpath, byte_size, raw_digest, created_at_ms
             ) VALUES (?1, 'synthetic-boundary', ?2, ?3, ?4, 1)",
            params![
                content_id,
                blob.relpath,
                i64::try_from(blob.byte_size).unwrap(),
                blob.hash.as_slice(),
            ],
        )
        .unwrap();
    fs::remove_file(blob_path).unwrap();

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "blobStatus");
}

#[test]
fn verify_detects_an_empty_external_content_fts_index() {
    let (_sandbox, data_dir) = imported_two_source_store();
    let connection = Connection::open(data_dir.join("clipboard.db")).unwrap();
    connection
        .execute(
            "INSERT INTO search_fts(search_fts) VALUES('delete-all')",
            [],
        )
        .unwrap();
    let source_documents: i64 = connection
        .query_row("SELECT count(*) FROM search_doc", [], |row| row.get(0))
        .unwrap();
    assert_eq!(source_documents, 2);

    let output = run_verify(&data_dir, 2);

    assert_failed_verification(&output, "ftsStatus");
    let matches_after_verify: i64 = connection
        .query_row(
            "SELECT count(*) FROM search_fts WHERE search_fts MATCH 'sanitized'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(matches_after_verify, 0);
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
    assert_eq!(value["runStatus"], "ok");
    assert_eq!(value["logicalStatus"], "ok");
    assert_eq!(value["blobStatus"], "ok");
    assert_eq!(value["ftsStatus"], "ok");
    assert_eq!(
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "blobStatus",
            "countsByKind",
            "eventCount",
            "expectedRecords",
            "ftsStatus",
            "indexedDocumentCount",
            "integrityStatus",
            "latestSourceTotal",
            "logicalStatus",
            "missingPayloadCount",
            "physicalContentCount",
            "runStatus",
            "sources",
            "status",
        ])
    );
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
    assert_eq!(
        supercmd_summary
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "alreadyPresent",
            "availableImageEvents",
            "failed",
            "imported",
            "missingImageEvents",
            "skipped",
            "sourceKind",
            "total",
        ])
    );
    assert_redacted(
        &verify,
        &["shared sanitized payload", "available.png", "missing.png"],
    );
}
