use std::{collections::BTreeMap, path::Path};

use clipboard_core::ContentFlags;
use clipboard_store::ReadOnlyStore;
use rusqlite::Connection;
use serde::Serialize;

use crate::{CliFailure, KindCount, path_policy::verified_read_only_config, store_failure};

const REAL_EXPECTED_RECORDS: u64 = 6_503;
const REAL_RAYCAST_RECORDS: u64 = 5_509;
const REAL_SUPERCMD_RECORDS: u64 = 994;
const REAL_SUPERCMD_IMAGES: u64 = 15;
const FTS_SMOKE_TOKEN: &str = "clipboardverificationsmokeconstant";

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
    let store = ReadOnlyStore::open_existing(config).map_err(store_failure)?;
    let snapshot = store
        .with_reader(read_verification_snapshot)
        .map_err(store_failure)?;
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
    fts_ok: bool,
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
}

fn read_verification_snapshot(connection: &Connection) -> rusqlite::Result<VerificationSnapshot> {
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
    let kinds = read_kind_counts(connection, missing_mask)?;
    let sources = read_latest_sources(connection)?;
    let image_counts = read_source_image_counts(connection, missing_mask)?;
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
        fts_ok,
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
        "WITH completed AS (
           SELECT source_kind, source_fingerprint, total_records, imported_records,
                  already_present_records, skipped_records, failed_records,
                  ROW_NUMBER() OVER (
                    PARTITION BY source_kind, source_fingerprint
                    ORDER BY COALESCE(finished_at_ms, started_at_ms) DESC, import_run_id DESC
                  ) AS position
           FROM import_run
           WHERE status = 'completed'
         )
         SELECT source_kind, total_records, imported_records, already_present_records,
                skipped_records, failed_records
         FROM completed
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
            })
        })?
        .collect()
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

fn fts_is_coherent(connection: &Connection, document_count: i64) -> bool {
    let result = (|| -> rusqlite::Result<bool> {
        let fts_count = connection.query_row("SELECT count(*) FROM search_fts", [], |row| {
            row.get::<_, i64>(0)
        })?;
        let missing_rows = connection.query_row(
            "SELECT count(*)
             FROM search_doc d
             LEFT JOIN search_fts f ON f.rowid = d.content_id
             WHERE f.rowid IS NULL",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        let extra_rows = connection.query_row(
            "SELECT count(*)
             FROM search_fts f
             LEFT JOIN search_doc d ON d.content_id = f.rowid
             WHERE d.content_id IS NULL",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        let _: i64 = connection.query_row(
            "SELECT count(*) FROM search_fts WHERE search_fts MATCH ?1",
            [FTS_SMOKE_TOKEN],
            |row| row.get(0),
        )?;
        Ok(fts_count == document_count && missing_rows == 0 && extra_rows == 0)
    })();
    result.unwrap_or(false)
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
    for source in snapshot.sources {
        let source_kind = known_source(&source.source_kind)?;
        let total = checked_count(source.total)?;
        let imported = checked_count(source.imported)?;
        let already_present = checked_count(source.already_present)?;
        let skipped = checked_count(source.skipped)?;
        let failed = checked_count(source.failed)?;
        latest_source_total = latest_source_total
            .checked_add(total)
            .ok_or_else(|| CliFailure::new("count_overflow"))?;
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
    let healthy = snapshot.integrity_ok
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
