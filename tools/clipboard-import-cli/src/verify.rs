use std::{collections::BTreeMap, path::Path, time::Duration};

use clipboard_core::ContentFlags;
use clipboard_store::{CasStore, ReadOnlyStore};
use rusqlite::{Connection, backup::Backup};
use serde::Serialize;

use crate::{
    CliFailure, KindCount,
    path_policy::{exact_blob_root_is_valid, verified_read_only_config},
    store_failure,
};

const REAL_EXPECTED_RECORDS: u64 = 6_503;
const REAL_RAYCAST_RECORDS: u64 = 5_509;
const REAL_SUPERCMD_RECORDS: u64 = 994;
const REAL_SUPERCMD_IMAGES: u64 = 15;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VerifyOutput {
    status: &'static str,
    expected_records: u64,
    latest_source_total: u64,
    physical_content_count: u64,
    event_count: u64,
    indexed_document_count: u64,
    missing_payload_count: u64,
    counts_by_kind: Vec<KindCount>,
    sources: Vec<SourceSummary>,
    integrity_status: &'static str,
    run_status: &'static str,
    logical_status: &'static str,
    blob_status: &'static str,
    fts_status: &'static str,
}

impl VerifyOutput {
    pub(crate) fn is_success(&self) -> bool {
        self.status == "ok"
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceSummary {
    source_kind: &'static str,
    total: u64,
    imported: u64,
    already_present: u64,
    skipped: u64,
    failed: u64,
    available_image_events: u64,
    missing_image_events: u64,
}

pub(crate) fn verify(data_dir: &Path, expect_records: u64) -> Result<VerifyOutput, CliFailure> {
    let config = verified_read_only_config(data_dir)?;
    let blob_root = config.blob_root().to_path_buf();
    let canonical_data_dir = config
        .database_path()
        .parent()
        .ok_or_else(|| CliFailure::new("unsafe_storage_layout"))?
        .to_path_buf();
    let store = ReadOnlyStore::open_existing(config).map_err(store_failure)?;
    let mut snapshot = store
        .with_reader(read_verification_snapshot)
        .map_err(store_failure)?;
    snapshot.blob_ok = exact_blob_root_is_valid(&canonical_data_dir, &blob_root)
        && blob_references_are_coherent(&blob_root, &snapshot.blob_references);
    verification_output(snapshot, expect_records)
}

struct VerificationSnapshot {
    physical_content_count: i64,
    event_count: i64,
    indexed_document_count: i64,
    missing_payload_count: i64,
    kinds: Vec<RawKindCount>,
    sources: Vec<RawSourceSummary>,
    image_counts: BTreeMap<String, (i64, i64)>,
    integrity_ok: bool,
    run_status_ok: bool,
    logical_ok: bool,
    blob_ok: bool,
    blob_references: Vec<BlobReference>,
    fts_ok: bool,
}

struct BlobReference {
    relpath: String,
    stored_byte_size: i64,
    original_byte_size: Option<i64>,
}

struct RawKindCount {
    kind: String,
    event_count: i64,
    missing_payload_count: i64,
}

struct RawSourceSummary {
    source_kind: String,
    total: i64,
    imported: i64,
    already_present: i64,
    skipped: i64,
    failed: i64,
    materialized_records: i64,
    declared_imported_records: i64,
}

fn read_verification_snapshot(connection: &Connection) -> rusqlite::Result<VerificationSnapshot> {
    connection.execute_batch("BEGIN DEFERRED")?;
    let snapshot = read_verification_snapshot_inner(connection);
    let rollback = connection.execute_batch("ROLLBACK");
    match (snapshot, rollback) {
        (Ok(snapshot), Ok(())) => Ok(snapshot),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

fn read_verification_snapshot_inner(
    connection: &Connection,
) -> rusqlite::Result<VerificationSnapshot> {
    let physical_content_count =
        connection.query_row("SELECT count(*) FROM content", [], |row| row.get(0))?;
    let event_count =
        connection.query_row("SELECT count(*) FROM history_event", [], |row| row.get(0))?;
    let indexed_document_count =
        connection.query_row("SELECT count(*) FROM search_doc", [], |row| row.get(0))?;
    let missing_mask = i64::from(ContentFlags::MISSING_PAYLOAD.bits());
    let missing_payload_count = connection.query_row(
        "SELECT count(*)
         FROM history_event he
         JOIN content c ON c.content_id = he.content_id
         WHERE (c.flags & ?1) != 0",
        [missing_mask],
        |row| row.get(0),
    )?;
    let run_status_ok = connection.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM import_run WHERE status != 'completed')",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    let kinds = read_kind_counts(connection, missing_mask)?;
    let sources = read_latest_sources(connection)?;
    let image_counts = read_source_image_counts(connection, missing_mask)?;
    let logical_ok = logical_relations_are_coherent(connection, event_count)?;
    let blob_references = read_blob_references(connection)?;
    let integrity_ok = connection
        .query_row("PRAGMA integrity_check(1)", [], |row| {
            row.get::<_, String>(0)
        })
        .is_ok_and(|status| status == "ok");
    let fts_ok = fts_is_coherent(connection, indexed_document_count);
    Ok(VerificationSnapshot {
        physical_content_count,
        event_count,
        indexed_document_count,
        missing_payload_count,
        kinds,
        sources,
        image_counts,
        integrity_ok,
        run_status_ok,
        logical_ok,
        blob_ok: false,
        blob_references,
        fts_ok,
    })
}

fn read_blob_references(connection: &Connection) -> rusqlite::Result<Vec<BlobReference>> {
    let mut statement = connection.prepare(
        "SELECT blob_relpath, stored_byte_size, original_byte_size
         FROM content_representation
         WHERE storage_kind = 'cas'
         UNION ALL
         SELECT blob_relpath, byte_size, NULL
         FROM artifact",
    )?;
    statement
        .query_map([], |row| {
            Ok(BlobReference {
                relpath: row.get(0)?,
                stored_byte_size: row.get(1)?,
                original_byte_size: row.get(2)?,
            })
        })?
        .collect()
}

fn blob_references_are_coherent(blob_root: &Path, references: &[BlobReference]) -> bool {
    let cas = CasStore::new(blob_root);
    references.iter().all(|reference| {
        let Ok(bytes) = cas.read(&reference.relpath) else {
            return false;
        };
        let Ok(actual_size) = u64::try_from(bytes.len()) else {
            return false;
        };
        let Ok(stored_byte_size) = u64::try_from(reference.stored_byte_size) else {
            return false;
        };
        let original_matches = reference.original_byte_size.is_none_or(|size| {
            u64::try_from(size).is_ok_and(|original_byte_size| original_byte_size == actual_size)
        });
        stored_byte_size == actual_size && original_matches
    })
}

fn read_kind_counts(
    connection: &Connection,
    missing_mask: i64,
) -> rusqlite::Result<Vec<RawKindCount>> {
    let mut statement = connection.prepare(
        "SELECT c.kind, count(*),
                SUM(CASE WHEN (c.flags & ?1) != 0 THEN 1 ELSE 0 END)
         FROM history_event he
         JOIN content c ON c.content_id = he.content_id
         GROUP BY c.kind
         ORDER BY c.kind",
    )?;
    statement
        .query_map([missing_mask], |row| {
            Ok(RawKindCount {
                kind: row.get(0)?,
                event_count: row.get(1)?,
                missing_payload_count: row.get(2)?,
            })
        })?
        .collect()
}

fn read_latest_sources(connection: &Connection) -> rusqlite::Result<Vec<RawSourceSummary>> {
    let mut statement = connection.prepare(
        "WITH completed AS MATERIALIZED (
           SELECT import_run_id, source_kind, source_fingerprint, total_records, imported_records,
                  already_present_records, skipped_records, failed_records,
                  ROW_NUMBER() OVER (
                    PARTITION BY source_kind, source_fingerprint
                    ORDER BY COALESCE(finished_at_ms, started_at_ms) DESC, import_run_id DESC
                  ) AS position
           FROM import_run
           WHERE status = 'completed'
         )
         SELECT source_kind, total_records, imported_records, already_present_records,
                skipped_records, failed_records,
                (
                  SELECT count(*)
                  FROM import_record ir
                  JOIN import_run owner ON owner.import_run_id = ir.import_run_id
                  WHERE owner.status = 'completed'
                    AND owner.source_kind = selected.source_kind
                    AND owner.source_fingerprint = selected.source_fingerprint
                ),
                (
                  SELECT COALESCE(sum(owner.imported_records), 0)
                  FROM import_run owner
                  WHERE owner.status = 'completed'
                    AND owner.source_kind = selected.source_kind
                    AND owner.source_fingerprint = selected.source_fingerprint
                )
         FROM completed selected
         WHERE position = 1
         ORDER BY source_kind, source_fingerprint",
    )?;
    statement
        .query_map([], |row| {
            Ok(RawSourceSummary {
                source_kind: row.get(0)?,
                total: row.get(1)?,
                imported: row.get(2)?,
                already_present: row.get(3)?,
                skipped: row.get(4)?,
                failed: row.get(5)?,
                materialized_records: row.get(6)?,
                declared_imported_records: row.get(7)?,
            })
        })?
        .collect()
}

fn logical_relations_are_coherent(
    connection: &Connection,
    event_count: i64,
) -> rusqlite::Result<bool> {
    let mut foreign_keys = connection.prepare("PRAGMA foreign_key_check")?;
    let foreign_keys_ok = foreign_keys.query([])?.next()?.is_none();
    let relations_ok = connection.query_row(
        "SELECT
           NOT EXISTS(SELECT 1 FROM import_record WHERE event_id IS NULL)
           AND (SELECT count(*) FROM import_record) = ?1
           AND (SELECT count(DISTINCT event_id) FROM import_record) = ?1
           AND NOT EXISTS(
             SELECT 1
             FROM history_event he
             LEFT JOIN import_record ir ON ir.event_id = he.event_id
             WHERE ir.import_record_id IS NULL
           )
           AND NOT EXISTS(
             SELECT 1
             FROM import_record ir
             JOIN import_run run ON run.import_run_id = ir.import_run_id
             WHERE ir.source_kind != run.source_kind
           )",
        [event_count],
        |row| row.get::<_, bool>(0),
    )?;
    Ok(foreign_keys_ok && relations_ok)
}

fn read_source_image_counts(
    connection: &Connection,
    missing_mask: i64,
) -> rusqlite::Result<BTreeMap<String, (i64, i64)>> {
    let mut statement = connection.prepare(
        "SELECT ir.source_kind,
                SUM(CASE WHEN c.kind = 'image' AND (c.flags & ?1) = 0 THEN 1 ELSE 0 END),
                SUM(CASE WHEN c.kind = 'image' AND (c.flags & ?1) != 0 THEN 1 ELSE 0 END)
         FROM import_record ir
         JOIN history_event he ON he.event_id = ir.event_id
         JOIN content c ON c.content_id = he.content_id
         GROUP BY ir.source_kind
         ORDER BY ir.source_kind",
    )?;
    statement
        .query_map([missing_mask], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (row.get::<_, i64>(1)?, row.get::<_, i64>(2)?),
            ))
        })?
        .collect()
}

fn fts_is_coherent(connection: &Connection, _document_count: i64) -> bool {
    (|| -> rusqlite::Result<()> {
        let mut copy = Connection::open_in_memory()?;
        {
            let backup = Backup::new(connection, &mut copy)?;
            backup.run_to_completion(128, Duration::ZERO, None)?;
        }
        copy.execute(
            "INSERT INTO search_fts(search_fts, rank) VALUES('integrity-check', 1)",
            [],
        )?;
        Ok(())
    })()
    .is_ok()
}

fn verification_output(
    snapshot: VerificationSnapshot,
    expect_records: u64,
) -> Result<VerifyOutput, CliFailure> {
    let physical_content_count = checked_count(snapshot.physical_content_count)?;
    let event_count = checked_count(snapshot.event_count)?;
    let indexed_document_count = checked_count(snapshot.indexed_document_count)?;
    let missing_payload_count = checked_count(snapshot.missing_payload_count)?;
    let counts_by_kind = snapshot
        .kinds
        .into_iter()
        .map(|count| {
            Ok(KindCount {
                kind: known_kind(&count.kind)?,
                event_count: checked_count(count.event_count)?,
                missing_payload_count: checked_count(count.missing_payload_count)?,
            })
        })
        .collect::<Result<Vec<_>, CliFailure>>()?;
    let mut sources = Vec::with_capacity(snapshot.sources.len());
    let mut latest_source_total = 0_u64;
    let mut selected_materialized_total = 0_u64;
    let mut selected_sources_are_materialized = true;
    for source in snapshot.sources {
        let source_kind = known_source(&source.source_kind)?;
        let total = checked_count(source.total)?;
        let imported = checked_count(source.imported)?;
        let already_present = checked_count(source.already_present)?;
        let skipped = checked_count(source.skipped)?;
        let failed = checked_count(source.failed)?;
        let materialized_records = checked_count(source.materialized_records)?;
        let declared_imported_records = checked_count(source.declared_imported_records)?;
        latest_source_total = latest_source_total
            .checked_add(total)
            .ok_or_else(|| CliFailure::new("count_overflow"))?;
        selected_materialized_total = selected_materialized_total
            .checked_add(materialized_records)
            .ok_or_else(|| CliFailure::new("count_overflow"))?;
        selected_sources_are_materialized &= materialized_records == total
            && declared_imported_records == materialized_records
            && skipped == 0
            && failed == 0;
        let (available_images, missing_images) = snapshot
            .image_counts
            .get(source_kind)
            .copied()
            .unwrap_or_default();
        sources.push(SourceSummary {
            source_kind,
            total,
            imported,
            already_present,
            skipped,
            failed,
            available_image_events: checked_count(available_images)?,
            missing_image_events: checked_count(missing_images)?,
        });
    }

    let source_accounting_ok = sources.iter().all(|source| {
        source
            .imported
            .checked_add(source.already_present)
            .and_then(|count| count.checked_add(source.skipped))
            .and_then(|count| count.checked_add(source.failed))
            == Some(source.total)
    });
    let one_source_each = sources.len() == 2
        && sources
            .iter()
            .filter(|source| source.source_kind == "raycast")
            .count()
            == 1
        && sources
            .iter()
            .filter(|source| source.source_kind == "supercmd")
            .count()
            == 1;
    let real_shape_ok = if expect_records == REAL_EXPECTED_RECORDS {
        let raycast = sources
            .iter()
            .find(|source| source.source_kind == "raycast");
        let supercmd = sources
            .iter()
            .find(|source| source.source_kind == "supercmd");
        raycast.is_some_and(|source| source.total == REAL_RAYCAST_RECORDS)
            && supercmd.is_some_and(|source| {
                source.total == REAL_SUPERCMD_RECORDS
                    && source
                        .available_image_events
                        .checked_add(source.missing_image_events)
                        == Some(REAL_SUPERCMD_IMAGES)
            })
    } else {
        true
    };
    let kinds_total = counts_by_kind
        .iter()
        .try_fold(0_u64, |total, kind| total.checked_add(kind.event_count));
    let logical_accounting_ok = snapshot.logical_ok
        && selected_sources_are_materialized
        && selected_materialized_total == event_count;
    let healthy = snapshot.integrity_ok
        && snapshot.run_status_ok
        && logical_accounting_ok
        && snapshot.blob_ok
        && snapshot.fts_ok
        && source_accounting_ok
        && one_source_each
        && real_shape_ok
        && latest_source_total == expect_records
        && event_count == expect_records
        && kinds_total == Some(event_count)
        && physical_content_count <= event_count;
    Ok(VerifyOutput {
        status: if healthy { "ok" } else { "failed" },
        expected_records: expect_records,
        latest_source_total,
        physical_content_count,
        event_count,
        indexed_document_count,
        missing_payload_count,
        counts_by_kind,
        sources,
        integrity_status: if snapshot.integrity_ok {
            "ok"
        } else {
            "failed"
        },
        run_status: if snapshot.run_status_ok {
            "ok"
        } else {
            "failed"
        },
        logical_status: if logical_accounting_ok {
            "ok"
        } else {
            "failed"
        },
        blob_status: if snapshot.blob_ok { "ok" } else { "failed" },
        fts_status: if snapshot.fts_ok { "ok" } else { "failed" },
    })
}

fn known_kind(value: &str) -> Result<&'static str, CliFailure> {
    match value {
        "text" => Ok("text"),
        "link" => Ok("link"),
        "image" => Ok("image"),
        "file" => Ok("file"),
        "color" => Ok("color"),
        "code" => Ok("code"),
        "html" => Ok("html"),
        _ => Err(CliFailure::new("invalid_database_data")),
    }
}

fn known_source(value: &str) -> Result<&'static str, CliFailure> {
    match value {
        "raycast" => Ok("raycast"),
        "supercmd" => Ok("supercmd"),
        _ => Err(CliFailure::new("invalid_database_data")),
    }
}

fn checked_count(value: i64) -> Result<u64, CliFailure> {
    u64::try_from(value).map_err(|_| CliFailure::new("invalid_database_data"))
}
