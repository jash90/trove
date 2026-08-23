use std::{
    collections::BTreeMap,
    fs,
    io::{self, Cursor, Read},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use clipboard_core::{ContentFlags, ContentKind, canonical_byte_len, content_hash};
use clipboard_store::{
    CasStore, MAX_CAS_OBJECT_BYTES, ReadOnlyStore, StorageBoundaryError, StorageBoundaryLease,
};
use rusqlite::{
    Connection, MAIN_DB, OpenFlags, OptionalExtension,
    backup::{Backup, StepResult},
};
use serde::Serialize;

use crate::{
    CliFailure, KindCount, boundary_failure,
    path_policy::{exact_blob_root_is_valid, verified_read_only_config},
    store_failure,
};

const REAL_EXPECTED_RECORDS: u64 = 6_503;
const REAL_RAYCAST_RECORDS: u64 = 5_509;
const REAL_SUPERCMD_RECORDS: u64 = 994;
const REAL_SUPERCMD_IMAGES: u64 = 15;
const MAX_FTS_SCRATCH_BYTES: u64 = 4_u64 * 1024 * 1024 * 1024;
const FTS_SCRATCH_CACHE_KIB: i64 = 16 * 1024;
const FTS_BACKUP_DEADLINE: Duration = Duration::from_secs(120);
const FTS_BACKUP_INITIAL_BACKOFF: Duration = Duration::from_millis(5);
const FTS_BACKUP_MAX_BACKOFF: Duration = Duration::from_secs(1);
const FTS_BACKUP_PAGES_PER_STEP: i32 = 256;
const VERIFICATION_REFERENCE_PAGE_SIZE: i64 = 256;

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
    verify_with_boundary(config, expect_records, StorageBoundaryLease::open_read_only)
}

fn verify_with_boundary(
    config: clipboard_store::StoreConfig,
    expect_records: u64,
    open_boundary: impl FnOnce(
        &clipboard_store::StoreConfig,
    ) -> Result<StorageBoundaryLease, StorageBoundaryError>,
) -> Result<VerifyOutput, CliFailure> {
    let blob_root = config.blob_root().to_path_buf();
    let canonical_data_dir = config
        .database_path()
        .parent()
        .ok_or_else(|| CliFailure::new("unsafe_storage_layout"))?
        .to_path_buf();
    if !exact_blob_root_is_valid(&canonical_data_dir, &blob_root) {
        return Ok(blob_root_failure_output(expect_records));
    }
    let boundary = Arc::new(open_boundary(&config).map_err(boundary_failure)?);
    let store = ReadOnlyStore::open_existing(config.with_storage_boundary(boundary))
        .map_err(store_failure)?;
    let cas = store.cas_store().map_err(store_failure)?;
    let snapshot = store
        .with_reader(|connection| read_verification_snapshot(connection, &cas))
        .map_err(store_failure)?;
    verification_output(snapshot, expect_records)
}

fn blob_root_failure_output(expect_records: u64) -> VerifyOutput {
    VerifyOutput {
        status: "failed",
        expected_records: expect_records,
        latest_source_total: 0,
        physical_content_count: 0,
        event_count: 0,
        indexed_document_count: 0,
        missing_payload_count: 0,
        counts_by_kind: Vec::new(),
        sources: Vec::new(),
        integrity_status: "failed",
        run_status: "failed",
        logical_status: "failed",
        blob_status: "failed",
        fts_status: "failed",
    }
}

struct VerificationSnapshot {
    invalidated: bool,
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
    fts_ok: bool,
}

impl VerificationSnapshot {
    fn invalidated() -> Self {
        Self {
            invalidated: true,
            physical_content_count: 0,
            event_count: 0,
            indexed_document_count: 0,
            missing_payload_count: 0,
            kinds: Vec::new(),
            sources: Vec::new(),
            image_counts: BTreeMap::new(),
            integrity_ok: false,
            run_status_ok: false,
            logical_ok: false,
            blob_ok: false,
            fts_ok: false,
        }
    }
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

struct ScalarClaims {
    physical_content_count: i64,
    event_count: i64,
    indexed_document_count: i64,
    missing_payload_count: i64,
    run_status_ok: bool,
}

enum EpochRead<T> {
    Stable(T),
    Changed,
}

fn read_verification_snapshot(
    connection: &Connection,
    cas: &CasStore,
) -> rusqlite::Result<VerificationSnapshot> {
    read_verification_snapshot_with_observer(connection, cas, &mut |_, _| {})
}

fn read_verification_snapshot_with_observer(
    connection: &Connection,
    cas: &CasStore,
    observe: &mut impl FnMut(VerificationPhase, &Connection),
) -> rusqlite::Result<VerificationSnapshot> {
    if !connection.is_autocommit() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let epoch = data_version(connection)?;
    read_verification_snapshot_inner(connection, cas, epoch, observe)
}

fn data_version(connection: &Connection) -> rusqlite::Result<i64> {
    connection.query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
}

fn epoch_is_current(connection: &Connection, epoch: i64) -> rusqlite::Result<bool> {
    Ok(connection.is_autocommit() && data_version(connection)? == epoch)
}

fn read_short_snapshot<T>(
    connection: &Connection,
    epoch: i64,
    read: impl FnOnce(&Connection) -> rusqlite::Result<T>,
) -> rusqlite::Result<EpochRead<T>> {
    if !epoch_is_current(connection, epoch)? {
        return Ok(EpochRead::Changed);
    }
    connection.execute_batch("BEGIN DEFERRED")?;
    let value = read(connection);
    let rollback = connection.execute_batch("ROLLBACK");
    match (value, rollback) {
        (Ok(value), Ok(())) => {
            if epoch_is_current(connection, epoch)? {
                Ok(EpochRead::Stable(value))
            } else {
                Ok(EpochRead::Changed)
            }
        }
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

fn read_verification_snapshot_inner(
    connection: &Connection,
    cas: &CasStore,
    epoch: i64,
    observe: &mut impl FnMut(VerificationPhase, &Connection),
) -> rusqlite::Result<VerificationSnapshot> {
    let missing_mask = i64::from(ContentFlags::MISSING_PAYLOAD.bits());
    let EpochRead::Stable(claims) = read_short_snapshot(connection, epoch, |connection| {
        read_scalar_claims(connection, missing_mask)
    })?
    else {
        return Ok(VerificationSnapshot::invalidated());
    };
    let EpochRead::Stable(kinds) = read_short_snapshot(connection, epoch, |connection| {
        read_kind_counts(connection, missing_mask)
    })?
    else {
        return Ok(VerificationSnapshot::invalidated());
    };
    let EpochRead::Stable(sources) = read_short_snapshot(connection, epoch, read_latest_sources)?
    else {
        return Ok(VerificationSnapshot::invalidated());
    };
    let EpochRead::Stable(image_counts) = read_short_snapshot(connection, epoch, |connection| {
        read_source_image_counts(connection, missing_mask)
    })?
    else {
        return Ok(VerificationSnapshot::invalidated());
    };
    let EpochRead::Stable(relations_ok) = read_short_snapshot(connection, epoch, |connection| {
        logical_relations_are_coherent(connection, claims.event_count)
    })?
    else {
        return Ok(VerificationSnapshot::invalidated());
    };
    let EpochRead::Stable(shape_ok) =
        read_short_snapshot(connection, epoch, semantic_storage_shape_is_coherent)?
    else {
        return Ok(VerificationSnapshot::invalidated());
    };
    let EpochRead::Stable(audit) =
        semantic_storage_is_coherent(connection, cas, epoch, shape_ok, observe)?
    else {
        return Ok(VerificationSnapshot::invalidated());
    };
    let logical_ok = relations_ok && audit.logical_ok;
    observe(VerificationPhase::Integrity, connection);
    if !epoch_is_current(connection, epoch)? {
        return Ok(VerificationSnapshot::invalidated());
    }
    let integrity_ok = connection
        .query_row("PRAGMA integrity_check(1)", [], |row| {
            row.get::<_, String>(0)
        })
        .is_ok_and(|status| status == "ok");
    if !epoch_is_current(connection, epoch)? {
        return Ok(VerificationSnapshot::invalidated());
    }
    observe(VerificationPhase::Fts, connection);
    if !epoch_is_current(connection, epoch)? {
        return Ok(VerificationSnapshot::invalidated());
    }
    let fts_ok = fts_is_coherent(connection, claims.indexed_document_count, observe);
    if !epoch_is_current(connection, epoch)? {
        return Ok(VerificationSnapshot::invalidated());
    }
    Ok(VerificationSnapshot {
        invalidated: false,
        physical_content_count: claims.physical_content_count,
        event_count: claims.event_count,
        indexed_document_count: claims.indexed_document_count,
        missing_payload_count: claims.missing_payload_count,
        kinds,
        sources,
        image_counts,
        integrity_ok,
        run_status_ok: claims.run_status_ok,
        logical_ok,
        blob_ok: audit.blob_ok,
        fts_ok,
    })
}

fn read_scalar_claims(
    connection: &Connection,
    missing_mask: i64,
) -> rusqlite::Result<ScalarClaims> {
    Ok(ScalarClaims {
        physical_content_count: connection.query_row(
            "SELECT count(*) FROM content",
            [],
            |row| row.get(0),
        )?,
        event_count: connection
            .query_row("SELECT count(*) FROM history_event", [], |row| row.get(0))?,
        indexed_document_count: connection.query_row(
            "SELECT count(*) FROM search_doc",
            [],
            |row| row.get(0),
        )?,
        missing_payload_count: connection.query_row(
            "SELECT count(*)
             FROM history_event he
             JOIN content c ON c.content_id = he.content_id
             WHERE (c.flags & ?1) != 0",
            [missing_mask],
            |row| row.get(0),
        )?,
        run_status_ok: connection.query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM import_run WHERE status != 'completed')",
            [],
            |row| row.get(0),
        )?,
    })
}

#[derive(Clone, Copy)]
struct AuditStatus {
    logical_ok: bool,
    blob_ok: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VerificationPhase {
    AfterMetadataPage,
    Cas,
    DecodeHash,
    Fts,
    FtsBackup,
    Integrity,
    PayloadFetch,
}

impl AuditStatus {
    fn healthy() -> Self {
        Self {
            logical_ok: true,
            blob_ok: true,
        }
    }

    fn merge(&mut self, other: Self) {
        self.logical_ok &= other.logical_ok;
        self.blob_ok &= other.blob_ok;
    }
}

fn semantic_storage_shape_is_coherent(connection: &Connection) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT
           NOT EXISTS(
             SELECT 1 FROM history_event he
             WHERE NOT EXISTS(
               SELECT 1 FROM event_representation er
               WHERE er.event_id = he.event_id AND er.ordinal = 0
             )
           )
           AND NOT EXISTS(
             SELECT 1 FROM event_representation
             GROUP BY event_id
             HAVING min(ordinal) != 0 OR max(ordinal) + 1 != count(*)
           )
           AND NOT EXISTS(
             SELECT 1 FROM raw_payload rp
             WHERE NOT EXISTS(
               SELECT 1 FROM event_representation er
               WHERE er.raw_payload_id = rp.raw_payload_id
             )
           )
           AND NOT EXISTS(
             SELECT 1 FROM content c
             WHERE NOT EXISTS(
               SELECT 1 FROM history_event he WHERE he.content_id = c.content_id
             )
           )
           AND NOT EXISTS(
             SELECT 1
             FROM event_representation er
             JOIN history_event he ON he.event_id = er.event_id
             JOIN content c ON c.content_id = he.content_id
             JOIN raw_payload rp ON rp.raw_payload_id = er.raw_payload_id
             WHERE (c.kind IN ('image', 'file') AND rp.storage_kind != 'cas')
                OR (c.kind NOT IN ('image', 'file') AND (
                     (rp.original_byte_size < 4096 AND rp.storage_kind != 'inline')
                  OR (rp.original_byte_size BETWEEN 4096 AND 262144
                      AND rp.storage_kind != 'inline_zstd')
                  OR (rp.original_byte_size > 262144 AND rp.storage_kind != 'cas')
                ))
           )
           AND NOT EXISTS(
             SELECT 1 FROM content
             WHERE typeof(content_hash) != 'blob' OR length(content_hash) != 32
                OR typeof(kind) != 'text'
                OR kind NOT IN ('text', 'link', 'image', 'file', 'color', 'code', 'html')
                OR typeof(primary_mime) != 'text'
                OR length(CAST(primary_mime AS BLOB)) NOT BETWEEN 1 AND 1024
                OR typeof(byte_size) != 'integer' OR byte_size < 0
                OR typeof(flags) != 'integer'
           )
           AND NOT EXISTS(
             SELECT 1 FROM event_representation
             WHERE typeof(format_id) != 'text'
                OR length(CAST(format_id AS BLOB)) NOT BETWEEN 1 AND 1024
                OR (missing_ref IS NOT NULL AND (
                     typeof(missing_ref) != 'text'
                     OR length(CAST(missing_ref AS BLOB)) NOT BETWEEN 1 AND 4096
                ))
           )
           AND NOT EXISTS(
             SELECT 1 FROM raw_payload
             WHERE typeof(raw_digest) != 'blob' OR length(raw_digest) != 32
                OR typeof(storage_kind) != 'text'
                OR storage_kind NOT IN ('inline', 'inline_zstd', 'cas')
                OR typeof(original_byte_size) != 'integer'
                OR original_byte_size NOT BETWEEN 0 AND 134217728
                OR typeof(stored_byte_size) != 'integer'
                OR stored_byte_size NOT BETWEEN 0 AND 134217728
                OR NOT (
                  (storage_kind = 'inline'
                    AND typeof(inline_payload) = 'blob' AND blob_relpath IS NULL
                    AND original_byte_size < 4096
                    AND original_byte_size = stored_byte_size
                    AND stored_byte_size = length(inline_payload)
                    AND length(inline_payload) < 4096)
                  OR (storage_kind = 'inline_zstd'
                    AND typeof(inline_payload) = 'blob' AND blob_relpath IS NULL
                    AND original_byte_size BETWEEN 4096 AND 262144
                    AND stored_byte_size = length(inline_payload)
                    AND length(inline_payload) <= 263168)
                  OR (storage_kind = 'cas'
                    AND inline_payload IS NULL
                    AND typeof(blob_relpath) = 'text'
                    AND length(CAST(blob_relpath AS BLOB)) = 67
                    AND substr(blob_relpath, 3, 1) = '/'
                    AND substr(blob_relpath, 1, 2) NOT GLOB '*[^0-9a-f]*'
                    AND substr(blob_relpath, 4, 64) NOT GLOB '*[^0-9a-f]*'
                    AND substr(blob_relpath, 1, 2) = substr(blob_relpath, 4, 2)
                    AND stored_byte_size = original_byte_size)
                )
           )
           AND NOT EXISTS(
             SELECT 1 FROM artifact
             WHERE typeof(artifact_kind) != 'text'
                OR length(CAST(artifact_kind AS BLOB)) NOT BETWEEN 1 AND 64
                OR typeof(blob_relpath) != 'text'
                OR length(CAST(blob_relpath AS BLOB)) != 67
                OR substr(blob_relpath, 3, 1) != '/'
                OR substr(blob_relpath, 1, 2) GLOB '*[^0-9a-f]*'
                OR substr(blob_relpath, 4, 64) GLOB '*[^0-9a-f]*'
                OR substr(blob_relpath, 1, 2) != substr(blob_relpath, 4, 2)
                OR typeof(byte_size) != 'integer'
                OR byte_size NOT BETWEEN 0 AND 134217728
                OR typeof(raw_digest) != 'blob' OR length(raw_digest) != 32
           )",
        [],
        |row| row.get::<_, bool>(0),
    )
}

fn semantic_storage_is_coherent(
    connection: &Connection,
    cas: &CasStore,
    epoch: i64,
    shape_ok: bool,
    observe: &mut impl FnMut(VerificationPhase, &Connection),
) -> rusqlite::Result<EpochRead<AuditStatus>> {
    let mut status = AuditStatus {
        logical_ok: shape_ok,
        blob_ok: true,
    };
    let EpochRead::Stable(raw_status) =
        audit_raw_payload_pages_with_observer(connection, cas, epoch, observe)?
    else {
        return Ok(EpochRead::Changed);
    };
    status.merge(raw_status);
    let EpochRead::Stable(primary_status) =
        audit_event_primary_pages(connection, cas, epoch, observe)?
    else {
        return Ok(EpochRead::Changed);
    };
    status.merge(primary_status);
    let EpochRead::Stable(artifact_status) = audit_artifact_pages(connection, cas, epoch, observe)?
    else {
        return Ok(EpochRead::Changed);
    };
    status.merge(artifact_status);
    Ok(EpochRead::Stable(status))
}

struct RawPayloadMetadata {
    id: i64,
    digest_hex: Option<String>,
    storage_kind: Option<String>,
    inline_type: String,
    inline_length: Option<i64>,
    blob_path_type: String,
    blob_path_length: Option<i64>,
    blob_relpath: Option<Vec<u8>>,
    original_size: Option<i64>,
    stored_size: Option<i64>,
}

fn audit_raw_payload_pages_with_observer(
    connection: &Connection,
    cas: &CasStore,
    epoch: i64,
    observe: &mut impl FnMut(VerificationPhase, &Connection),
) -> rusqlite::Result<EpochRead<AuditStatus>> {
    let mut status = AuditStatus::healthy();
    let mut last_id = 0_i64;
    loop {
        let EpochRead::Stable(page) = read_short_snapshot(connection, epoch, |connection| {
            read_raw_payload_metadata_page(connection, last_id)
        })?
        else {
            return Ok(EpochRead::Changed);
        };
        observe(VerificationPhase::AfterMetadataPage, connection);
        if !epoch_is_current(connection, epoch)? {
            return Ok(EpochRead::Changed);
        }
        let page_is_full = page.len() == VERIFICATION_REFERENCE_PAGE_SIZE as usize;
        for metadata in page {
            last_id = metadata.id;
            let digest = metadata.digest_hex.as_deref().and_then(decode_digest_hex);
            let original_size = metadata.original_size.and_then(valid_object_size);
            let stored_size = metadata.stored_size.and_then(valid_object_size);
            let (Some(digest), Some(original_size), Some(stored_size)) =
                (digest, original_size, stored_size)
            else {
                status.logical_ok = false;
                continue;
            };
            let relpath = metadata
                .blob_relpath
                .and_then(|bytes| String::from_utf8(bytes).ok());
            match metadata.storage_kind.as_deref() {
                Some("inline") => {
                    let metadata_valid = metadata.inline_type == "blob"
                        && metadata.blob_path_type == "null"
                        && metadata.blob_path_length.is_none()
                        && relpath.is_none()
                        && original_size < 4 * 1024
                        && metadata
                            .inline_length
                            .and_then(|length| usize::try_from(length).ok())
                            == Some(original_size)
                        && stored_size == original_size;
                    let Some(payload) = (if metadata_valid {
                        read_bounded_inline_payload(
                            connection,
                            metadata.id,
                            "inline",
                            original_size,
                            stored_size,
                            observe,
                        )?
                    } else {
                        None
                    }) else {
                        status.logical_ok = false;
                        continue;
                    };
                    observe(VerificationPhase::DecodeHash, connection);
                    if !epoch_is_current(connection, epoch)? {
                        return Ok(EpochRead::Changed);
                    }
                    status.logical_ok &= digest.as_slice() == blake3::hash(&payload).as_bytes();
                }
                Some("inline_zstd") => {
                    let metadata_valid = metadata.inline_type == "blob"
                        && metadata.blob_path_type == "null"
                        && metadata.blob_path_length.is_none()
                        && relpath.is_none()
                        && (4 * 1024..=256 * 1024).contains(&original_size)
                        && stored_size <= 263_168
                        && metadata
                            .inline_length
                            .and_then(|length| usize::try_from(length).ok())
                            == Some(stored_size);
                    let Some(payload) = (if metadata_valid {
                        read_bounded_inline_payload(
                            connection,
                            metadata.id,
                            "inline_zstd",
                            original_size,
                            stored_size,
                            observe,
                        )?
                    } else {
                        None
                    }) else {
                        status.logical_ok = false;
                        continue;
                    };
                    observe(VerificationPhase::DecodeHash, connection);
                    if !epoch_is_current(connection, epoch)? {
                        return Ok(EpochRead::Changed);
                    }
                    let decoded = decode_exact_zstd(&payload, original_size);
                    status.logical_ok &= decoded
                        .as_ref()
                        .is_some_and(|bytes| digest.as_slice() == blake3::hash(bytes).as_bytes());
                }
                Some("cas") => {
                    let valid_shape = metadata.inline_type == "null"
                        && metadata.inline_length.is_none()
                        && original_size == stored_size
                        && metadata.blob_path_type == "text"
                        && metadata.blob_path_length == Some(67)
                        && relpath
                            .as_deref()
                            .is_some_and(|path| digest_matches_relpath(&digest, path));
                    let valid_blob = if let Some(path) = relpath.as_deref().filter(|_| valid_shape)
                    {
                        observe(VerificationPhase::Cas, connection);
                        if !epoch_is_current(connection, epoch)? {
                            return Ok(EpochRead::Changed);
                        }
                        cas.verify(path, original_size as u64).is_ok()
                    } else {
                        false
                    };
                    status.logical_ok &= valid_blob;
                    status.blob_ok &= valid_blob;
                }
                _ => status.logical_ok = false,
            }
        }
        if !page_is_full {
            break;
        }
    }
    Ok(EpochRead::Stable(status))
}

fn read_raw_payload_metadata_page(
    connection: &Connection,
    last_id: i64,
) -> rusqlite::Result<Vec<RawPayloadMetadata>> {
    let mut statement = connection.prepare(
        "SELECT raw_payload_id,
                CASE WHEN typeof(raw_digest) = 'blob' AND length(raw_digest) = 32
                     THEN hex(raw_digest) END,
                CASE WHEN typeof(storage_kind) = 'text'
                               AND storage_kind IN ('inline', 'inline_zstd', 'cas')
                     THEN storage_kind END,
                typeof(inline_payload), length(inline_payload),
                typeof(blob_relpath), length(CAST(blob_relpath AS BLOB)),
                CASE WHEN typeof(blob_relpath) = 'text'
                               AND length(CAST(blob_relpath AS BLOB)) = 67
                     THEN CAST(blob_relpath AS BLOB) END,
                CASE WHEN typeof(original_byte_size) = 'integer'
                     THEN original_byte_size END,
                CASE WHEN typeof(stored_byte_size) = 'integer'
                     THEN stored_byte_size END
         FROM raw_payload
         WHERE raw_payload_id > ?1
         ORDER BY raw_payload_id
         LIMIT ?2",
    )?;
    statement
        .query_map(
            rusqlite::params![last_id, VERIFICATION_REFERENCE_PAGE_SIZE],
            |row| {
                Ok(RawPayloadMetadata {
                    id: row.get(0)?,
                    digest_hex: row.get(1)?,
                    storage_kind: row.get(2)?,
                    inline_type: row.get(3)?,
                    inline_length: row.get(4)?,
                    blob_path_type: row.get(5)?,
                    blob_path_length: row.get(6)?,
                    blob_relpath: row.get(7)?,
                    original_size: row.get(8)?,
                    stored_size: row.get(9)?,
                })
            },
        )?
        .collect()
}

fn read_bounded_inline_payload(
    connection: &Connection,
    raw_payload_id: i64,
    storage_kind: &str,
    original_size: usize,
    stored_size: usize,
    observe: &mut impl FnMut(VerificationPhase, &Connection),
) -> rusqlite::Result<Option<Vec<u8>>> {
    observe(VerificationPhase::PayloadFetch, connection);
    connection
        .query_row(
            "SELECT inline_payload
             FROM raw_payload
             WHERE raw_payload_id = ?1
               AND storage_kind = ?2
               AND typeof(inline_payload) = 'blob'
               AND length(inline_payload) = ?3
               AND blob_relpath IS NULL
               AND original_byte_size = ?4
               AND stored_byte_size = ?3",
            rusqlite::params![
                raw_payload_id,
                storage_kind,
                i64::try_from(stored_size).unwrap_or(i64::MAX),
                i64::try_from(original_size).unwrap_or(i64::MAX),
            ],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
}

fn decode_digest_hex(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 || !value.is_ascii() {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(digest)
}

struct PrimaryMetadata {
    event_id: i64,
    kind: Option<String>,
    primary_mime: Option<Vec<u8>>,
    content_hash_hex: Option<String>,
    content_size: Option<i64>,
    flags: Option<i64>,
    representation_event_id: Option<i64>,
    raw_payload_id: Option<i64>,
    missing_ref_type: String,
    missing_ref_length: Option<i64>,
    missing_ref: Option<Vec<u8>>,
    storage_kind: Option<String>,
    inline_type: String,
    inline_length: Option<i64>,
    blob_path_type: String,
    blob_path_length: Option<i64>,
    blob_relpath: Option<Vec<u8>>,
    original_size: Option<i64>,
    stored_size: Option<i64>,
}

fn audit_event_primary_pages(
    connection: &Connection,
    cas: &CasStore,
    epoch: i64,
    observe: &mut impl FnMut(VerificationPhase, &Connection),
) -> rusqlite::Result<EpochRead<AuditStatus>> {
    let mut status = AuditStatus::healthy();
    let mut last_event_id = 0_i64;
    loop {
        let EpochRead::Stable(page) = read_short_snapshot(connection, epoch, |connection| {
            read_primary_metadata_page(connection, last_event_id)
        })?
        else {
            return Ok(EpochRead::Changed);
        };
        observe(VerificationPhase::AfterMetadataPage, connection);
        if !epoch_is_current(connection, epoch)? {
            return Ok(EpochRead::Changed);
        }
        let page_is_full = page.len() == VERIFICATION_REFERENCE_PAGE_SIZE as usize;
        for metadata in page {
            last_event_id = metadata.event_id;
            let kind = metadata.kind.as_deref().and_then(parse_content_kind);
            let primary_mime = metadata
                .primary_mime
                .as_deref()
                .and_then(|value| std::str::from_utf8(value).ok());
            let stored_content_hash = metadata
                .content_hash_hex
                .as_deref()
                .and_then(decode_digest_hex);
            let flags = metadata.flags.and_then(valid_content_flags);
            let content_size = metadata
                .content_size
                .and_then(|value| usize::try_from(value).ok());
            let (
                Some(kind),
                Some(primary_mime),
                Some(stored_content_hash),
                Some(flags),
                Some(content_size),
            ) = (kind, primary_mime, stored_content_hash, flags, content_size)
            else {
                status.logical_ok = false;
                continue;
            };
            if metadata.representation_event_id.is_none() {
                status.logical_ok = false;
                continue;
            }
            let missing_ref = metadata
                .missing_ref
                .as_deref()
                .and_then(|value| std::str::from_utf8(value).ok());
            if metadata.raw_payload_id.is_none() {
                let metadata_valid = metadata.missing_ref_type == "text"
                    && metadata
                        .missing_ref_length
                        .is_some_and(|length| (1..=4_096).contains(&length))
                    && missing_ref.is_some_and(|value| !value.is_empty())
                    && flags.contains(ContentFlags::MISSING_PAYLOAD)
                    && content_size == 0;
                if !metadata_valid {
                    status.logical_ok = false;
                    continue;
                }
                observe(VerificationPhase::DecodeHash, connection);
                if !epoch_is_current(connection, epoch)? {
                    return Ok(EpochRead::Changed);
                }
                status.logical_ok &= stored_content_hash
                    == missing_content_hash(kind, primary_mime, missing_ref.unwrap());
                continue;
            }
            if metadata.missing_ref_type != "null"
                || metadata.missing_ref_length.is_some()
                || missing_ref.is_some()
                || flags.contains(ContentFlags::MISSING_PAYLOAD)
            {
                status.logical_ok = false;
                continue;
            }
            let storage_kind = metadata.storage_kind.as_deref();
            let Some(bytes) = read_primary_payload(connection, cas, epoch, &metadata, observe)?
            else {
                status.logical_ok = false;
                if storage_kind == Some("cas") {
                    status.blob_ok = false;
                }
                continue;
            };
            observe(VerificationPhase::DecodeHash, connection);
            if !epoch_is_current(connection, epoch)? {
                return Ok(EpochRead::Changed);
            }
            let canonical_size = canonical_byte_len(kind, &bytes);
            status.logical_ok &= content_size == canonical_size
                && stored_content_hash == content_hash(kind, primary_mime, &bytes);
        }
        if !page_is_full {
            break;
        }
    }
    Ok(EpochRead::Stable(status))
}

fn read_primary_metadata_page(
    connection: &Connection,
    last_event_id: i64,
) -> rusqlite::Result<Vec<PrimaryMetadata>> {
    let mut statement = connection.prepare(
        "SELECT he.event_id,
                CASE WHEN typeof(c.kind) = 'text'
                               AND c.kind IN ('text', 'link', 'image', 'file', 'color', 'code', 'html')
                     THEN c.kind END,
                CASE WHEN typeof(c.primary_mime) = 'text'
                               AND length(CAST(c.primary_mime AS BLOB)) BETWEEN 1 AND 1024
                     THEN CAST(c.primary_mime AS BLOB) END,
                CASE WHEN typeof(c.content_hash) = 'blob' AND length(c.content_hash) = 32
                     THEN hex(c.content_hash) END,
                CASE WHEN typeof(c.byte_size) = 'integer' THEN c.byte_size END,
                CASE WHEN typeof(c.flags) = 'integer' THEN c.flags END,
                er.event_id, er.raw_payload_id,
                typeof(er.missing_ref), length(CAST(er.missing_ref AS BLOB)),
                CASE WHEN typeof(er.missing_ref) = 'text'
                               AND length(CAST(er.missing_ref AS BLOB)) BETWEEN 1 AND 4096
                     THEN CAST(er.missing_ref AS BLOB) END,
                CASE WHEN typeof(rp.storage_kind) = 'text'
                               AND rp.storage_kind IN ('inline', 'inline_zstd', 'cas')
                     THEN rp.storage_kind END,
                typeof(rp.inline_payload), length(rp.inline_payload),
                typeof(rp.blob_relpath), length(CAST(rp.blob_relpath AS BLOB)),
                CASE WHEN typeof(rp.blob_relpath) = 'text'
                               AND length(CAST(rp.blob_relpath AS BLOB)) = 67
                     THEN CAST(rp.blob_relpath AS BLOB) END,
                CASE WHEN typeof(rp.original_byte_size) = 'integer'
                     THEN rp.original_byte_size END,
                CASE WHEN typeof(rp.stored_byte_size) = 'integer'
                     THEN rp.stored_byte_size END
         FROM history_event he
         JOIN content c ON c.content_id = he.content_id
         LEFT JOIN event_representation er
           ON er.event_id = he.event_id AND er.ordinal = 0
         LEFT JOIN raw_payload rp ON rp.raw_payload_id = er.raw_payload_id
         WHERE he.event_id > ?1
         ORDER BY he.event_id
         LIMIT ?2",
    )?;
    statement
        .query_map(
            rusqlite::params![last_event_id, VERIFICATION_REFERENCE_PAGE_SIZE],
            |row| {
                Ok(PrimaryMetadata {
                    event_id: row.get(0)?,
                    kind: row.get(1)?,
                    primary_mime: row.get(2)?,
                    content_hash_hex: row.get(3)?,
                    content_size: row.get(4)?,
                    flags: row.get(5)?,
                    representation_event_id: row.get(6)?,
                    raw_payload_id: row.get(7)?,
                    missing_ref_type: row.get(8)?,
                    missing_ref_length: row.get(9)?,
                    missing_ref: row.get(10)?,
                    storage_kind: row.get(11)?,
                    inline_type: row.get(12)?,
                    inline_length: row.get(13)?,
                    blob_path_type: row.get(14)?,
                    blob_path_length: row.get(15)?,
                    blob_relpath: row.get(16)?,
                    original_size: row.get(17)?,
                    stored_size: row.get(18)?,
                })
            },
        )?
        .collect()
}

fn read_primary_payload(
    connection: &Connection,
    cas: &CasStore,
    epoch: i64,
    metadata: &PrimaryMetadata,
    observe: &mut impl FnMut(VerificationPhase, &Connection),
) -> rusqlite::Result<Option<Vec<u8>>> {
    let Some(raw_payload_id) = metadata.raw_payload_id else {
        return Ok(None);
    };
    let Some(original_size) = metadata.original_size.and_then(valid_object_size) else {
        return Ok(None);
    };
    let Some(stored_size) = metadata.stored_size.and_then(valid_object_size) else {
        return Ok(None);
    };
    let relpath = metadata
        .blob_relpath
        .as_ref()
        .and_then(|value| std::str::from_utf8(value).ok());
    match metadata.storage_kind.as_deref() {
        Some("inline")
            if metadata.inline_type == "blob"
                && metadata.inline_length == i64::try_from(original_size).ok()
                && metadata.blob_path_type == "null"
                && metadata.blob_path_length.is_none()
                && relpath.is_none()
                && original_size < 4 * 1024
                && stored_size == original_size =>
        {
            read_bounded_inline_payload(
                connection,
                raw_payload_id,
                "inline",
                original_size,
                stored_size,
                observe,
            )
        }
        Some("inline_zstd")
            if metadata.inline_type == "blob"
                && metadata.inline_length == i64::try_from(stored_size).ok()
                && metadata.blob_path_type == "null"
                && metadata.blob_path_length.is_none()
                && relpath.is_none()
                && (4 * 1024..=256 * 1024).contains(&original_size)
                && stored_size <= 263_168 =>
        {
            let Some(payload) = read_bounded_inline_payload(
                connection,
                raw_payload_id,
                "inline_zstd",
                original_size,
                stored_size,
                observe,
            )?
            else {
                return Ok(None);
            };
            observe(VerificationPhase::DecodeHash, connection);
            if !epoch_is_current(connection, epoch)? {
                return Ok(None);
            }
            Ok(decode_exact_zstd(&payload, original_size))
        }
        Some("cas")
            if metadata.inline_type == "null"
                && metadata.inline_length.is_none()
                && metadata.blob_path_type == "text"
                && metadata.blob_path_length == Some(67)
                && original_size == stored_size =>
        {
            let Some(relpath) = relpath else {
                return Ok(None);
            };
            observe(VerificationPhase::Cas, connection);
            if !epoch_is_current(connection, epoch)? {
                return Ok(None);
            }
            Ok(cas
                .read(relpath)
                .ok()
                .filter(|bytes| bytes.len() == original_size))
        }
        _ => Ok(None),
    }
}

struct ArtifactMetadata {
    id: i64,
    blob_relpath: Option<Vec<u8>>,
    byte_size: Option<i64>,
    digest_hex: Option<String>,
}

fn audit_artifact_pages(
    connection: &Connection,
    cas: &CasStore,
    epoch: i64,
    observe: &mut impl FnMut(VerificationPhase, &Connection),
) -> rusqlite::Result<EpochRead<AuditStatus>> {
    let mut status = AuditStatus::healthy();
    let mut last_id = 0_i64;
    loop {
        let EpochRead::Stable(page) = read_short_snapshot(connection, epoch, |connection| {
            read_artifact_metadata_page(connection, last_id)
        })?
        else {
            return Ok(EpochRead::Changed);
        };
        observe(VerificationPhase::AfterMetadataPage, connection);
        if !epoch_is_current(connection, epoch)? {
            return Ok(EpochRead::Changed);
        }
        let page_is_full = page.len() == VERIFICATION_REFERENCE_PAGE_SIZE as usize;
        for metadata in page {
            last_id = metadata.id;
            let relpath = metadata
                .blob_relpath
                .and_then(|value| String::from_utf8(value).ok());
            let size = metadata.byte_size.and_then(valid_object_size);
            let digest = metadata.digest_hex.as_deref().and_then(decode_digest_hex);
            let valid_shape = relpath
                .as_deref()
                .zip(digest.as_ref())
                .is_some_and(|(path, digest)| digest_matches_relpath(digest, path));
            let valid = if let (Some(path), Some(size)) =
                (relpath.as_deref().filter(|_| valid_shape), size)
            {
                observe(VerificationPhase::Cas, connection);
                if !epoch_is_current(connection, epoch)? {
                    return Ok(EpochRead::Changed);
                }
                cas.verify(path, size as u64).is_ok()
            } else {
                false
            };
            status.logical_ok &= valid;
            status.blob_ok &= valid;
        }
        if !page_is_full {
            break;
        }
    }
    Ok(EpochRead::Stable(status))
}

fn read_artifact_metadata_page(
    connection: &Connection,
    last_id: i64,
) -> rusqlite::Result<Vec<ArtifactMetadata>> {
    let mut statement = connection.prepare(
        "SELECT artifact_id,
                CASE WHEN typeof(blob_relpath) = 'text'
                               AND length(CAST(blob_relpath AS BLOB)) = 67
                     THEN CAST(blob_relpath AS BLOB) END,
                CASE WHEN typeof(byte_size) = 'integer' THEN byte_size END,
                CASE WHEN typeof(raw_digest) = 'blob' AND length(raw_digest) = 32
                     THEN hex(raw_digest) END
         FROM artifact
         WHERE artifact_id > ?1
         ORDER BY artifact_id
         LIMIT ?2",
    )?;
    statement
        .query_map(
            rusqlite::params![last_id, VERIFICATION_REFERENCE_PAGE_SIZE],
            |row| {
                Ok(ArtifactMetadata {
                    id: row.get(0)?,
                    blob_relpath: row.get(1)?,
                    byte_size: row.get(2)?,
                    digest_hex: row.get(3)?,
                })
            },
        )?
        .collect()
}

fn valid_object_size(size: i64) -> Option<usize> {
    usize::try_from(size)
        .ok()
        .filter(|size| *size <= MAX_CAS_OBJECT_BYTES)
}

fn decode_exact_zstd(payload: &[u8], expected_size: usize) -> Option<Vec<u8>> {
    let cursor = Cursor::new(payload);
    let mut decoder = zstd::stream::read::Decoder::with_buffer(cursor)
        .ok()?
        .single_frame();
    let mut decoded = Vec::with_capacity(expected_size);
    decoder
        .by_ref()
        .take((expected_size as u64).saturating_add(1))
        .read_to_end(&mut decoded)
        .ok()?;
    let consumed = decoder.finish().position();
    (decoded.len() == expected_size && consumed == payload.len() as u64).then_some(decoded)
}

fn digest_matches_relpath(digest: &[u8], relpath: &str) -> bool {
    let bytes = relpath.as_bytes();
    if digest.len() != 32
        || bytes.len() != 67
        || bytes.get(2) != Some(&b'/')
        || !bytes[..2]
            .iter()
            .chain(&bytes[3..])
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        || bytes[..2] != bytes[3..5]
    {
        return false;
    }
    let encoded = &relpath[3..];
    digest.iter().enumerate().all(|(index, byte)| {
        u8::from_str_radix(&encoded[index * 2..index * 2 + 2], 16) == Ok(*byte)
    })
}

fn parse_content_kind(value: &str) -> Option<ContentKind> {
    match value {
        "text" => Some(ContentKind::Text),
        "link" => Some(ContentKind::Link),
        "image" => Some(ContentKind::Image),
        "file" => Some(ContentKind::File),
        "color" => Some(ContentKind::Color),
        "code" => Some(ContentKind::Code),
        "html" => Some(ContentKind::Html),
        _ => None,
    }
}

fn valid_content_flags(value: i64) -> Option<ContentFlags> {
    let bits = u32::try_from(value).ok()?;
    ContentFlags::from_bits(bits)
}

fn missing_content_hash(kind: ContentKind, primary_mime: &str, missing_ref: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"clipboard-store.missing-primary-v1");
    for component in [
        kind.as_str().as_bytes(),
        primary_mime.as_bytes(),
        missing_ref.as_bytes(),
    ] {
        hasher.update(&(component.len() as u64).to_be_bytes());
        hasher.update(component);
    }
    *hasher.finalize().as_bytes()
}

fn read_kind_counts(
    connection: &Connection,
    missing_mask: i64,
) -> rusqlite::Result<Vec<RawKindCount>> {
    let mut statement = connection.prepare(
        "WITH expected(kind) AS (
           VALUES ('text'), ('link'), ('image'), ('file'), ('color'), ('code'), ('html')
         )
         SELECT c.kind, count(*),
                SUM(CASE WHEN (c.flags & ?1) != 0 THEN 1 ELSE 0 END)
         FROM history_event he
         JOIN content c ON c.content_id = he.content_id
         JOIN expected e ON e.kind = c.kind
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
        "WITH expected(source_kind) AS (VALUES ('raycast'), ('supercmd')),
         completed AS MATERIALIZED (
           SELECT r.*,
                  ROW_NUMBER() OVER (
                    PARTITION BY r.source_kind
                    ORDER BY COALESCE(r.finished_at_ms, r.started_at_ms) DESC,
                             r.import_run_id DESC
                  ) AS position
           FROM import_run r
           JOIN expected e USING (source_kind)
           WHERE r.status = 'completed'
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
           )
           AND NOT EXISTS(
             SELECT 1 FROM import_run
             WHERE source_kind NOT IN ('raycast', 'supercmd')
           )
           AND NOT EXISTS(
             SELECT 1 FROM import_record
             WHERE source_kind NOT IN ('raycast', 'supercmd')
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
        "WITH expected(source_kind) AS (VALUES ('raycast'), ('supercmd'))
         SELECT ir.source_kind,
                SUM(CASE WHEN c.kind = 'image' AND (c.flags & ?1) = 0 THEN 1 ELSE 0 END),
                SUM(CASE WHEN c.kind = 'image' AND (c.flags & ?1) != 0 THEN 1 ELSE 0 END)
         FROM import_record ir
         JOIN expected e ON e.source_kind = ir.source_kind
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

#[derive(Clone, Copy)]
struct FtsScratchPolicy {
    max_image_bytes: u64,
    cache_kib: i64,
    deadline: Duration,
    initial_backoff: Duration,
    max_backoff: Duration,
    pages_per_step: i32,
}

impl FtsScratchPolicy {
    const fn production() -> Self {
        Self {
            max_image_bytes: MAX_FTS_SCRATCH_BYTES,
            cache_kib: FTS_SCRATCH_CACHE_KIB,
            deadline: FTS_BACKUP_DEADLINE,
            initial_backoff: FTS_BACKUP_INITIAL_BACKOFF,
            max_backoff: FTS_BACKUP_MAX_BACKOFF,
            pages_per_step: FTS_BACKUP_PAGES_PER_STEP,
        }
    }

    #[cfg(test)]
    const fn for_test(max_image_bytes: u64, deadline: Duration) -> Self {
        Self {
            max_image_bytes,
            deadline,
            ..Self::production()
        }
    }
}

enum ScratchEvent<'a> {
    ScratchCreated(&'a Path),
    BackupStarted,
}

#[derive(Clone, Copy)]
struct BackupProgress {
    remaining: u64,
    page_count: u64,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum BackupStep {
    Done,
    More,
    Busy,
    Locked,
    Failed,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum FtsScratchError {
    Verification,
    Cleanup,
}

struct FtsScratchContext<'a> {
    source: &'a Connection,
    policy: FtsScratchPolicy,
    started: Instant,
    page_size: u64,
    page_count: u64,
    selected_data_dir: PathBuf,
}

fn fts_is_coherent(
    connection: &Connection,
    _document_count: i64,
    observe: &mut impl FnMut(VerificationPhase, &Connection),
) -> bool {
    fts_is_coherent_with_policy(
        connection,
        FtsScratchPolicy::production(),
        |event| match event {
            ScratchEvent::ScratchCreated(path) => {
                let _ = path;
            }
            ScratchEvent::BackupStarted => observe(VerificationPhase::FtsBackup, connection),
        },
    )
}

fn fts_is_coherent_with_policy(
    source: &Connection,
    policy: FtsScratchPolicy,
    mut observe: impl FnMut(ScratchEvent<'_>),
) -> bool {
    fts_scratch_check(source, policy, &mut observe).is_ok()
}

fn fts_scratch_check(
    source: &Connection,
    policy: FtsScratchPolicy,
    observe: &mut impl FnMut(ScratchEvent<'_>),
) -> Result<(), FtsScratchError> {
    fts_scratch_check_with_cleanup(source, policy, observe, tempfile::TempDir::close)
}

fn fts_scratch_check_with_cleanup(
    source: &Connection,
    policy: FtsScratchPolicy,
    observe: &mut impl FnMut(ScratchEvent<'_>),
    close_scratch: impl FnOnce(tempfile::TempDir) -> io::Result<()>,
) -> Result<(), FtsScratchError> {
    let started = Instant::now();
    let preflight = (|| -> Result<(u64, u64, PathBuf), ()> {
        if policy.pages_per_step <= 0
            || policy.max_image_bytes == 0
            || policy.cache_kib <= 0
            || policy.initial_backoff.is_zero()
            || policy.max_backoff < policy.initial_backoff
            || policy.deadline.is_zero()
            || !source.is_readonly(MAIN_DB).map_err(|_| ())?
            || !source
                .query_row("PRAGMA query_only", [], |row| row.get::<_, bool>(0))
                .map_err(|_| ())?
        {
            return Err(());
        }
        source.busy_timeout(Duration::ZERO).map_err(|_| ())?;
        let page_size = source
            .query_row("PRAGMA page_size", [], |row| row.get::<_, i64>(0))
            .map_err(|_| ())?;
        let page_count = source
            .query_row("PRAGMA page_count", [], |row| row.get::<_, i64>(0))
            .map_err(|_| ())?;
        let page_size = valid_page_size(page_size)?;
        let page_count = u64::try_from(page_count).map_err(|_| ())?;
        let source_image_bytes = page_count.checked_mul(page_size).ok_or(())?;
        if source_image_bytes > policy.max_image_bytes || started.elapsed() >= policy.deadline {
            return Err(());
        }
        Ok((page_size, page_count, source_data_directory(source)?))
    })()
    .map_err(|_| FtsScratchError::Verification)?;
    let (page_size, page_count, selected_data_dir) = preflight;
    let context = FtsScratchContext {
        source,
        policy,
        started,
        page_size,
        page_count,
        selected_data_dir,
    };
    let scratch_directory = tempfile::Builder::new()
        .prefix("clipboard-fts-check-")
        .tempdir()
        .map_err(|_| FtsScratchError::Verification)?;
    let verification_result = run_fts_scratch_work(&context, scratch_directory.path(), observe)
        .map_err(|_| FtsScratchError::Verification);
    let cleanup_result = close_scratch(scratch_directory);
    if cleanup_result.is_err() {
        return Err(FtsScratchError::Cleanup);
    }
    verification_result
}

fn run_fts_scratch_work(
    context: &FtsScratchContext<'_>,
    scratch_directory: &Path,
    observe: &mut impl FnMut(ScratchEvent<'_>),
) -> Result<(), ()> {
    restrict_scratch_permissions(scratch_directory)?;
    let canonical_scratch = fs::canonicalize(scratch_directory).map_err(|_| ())?;
    if canonical_scratch.starts_with(&context.selected_data_dir)
        || context.started.elapsed() >= context.policy.deadline
    {
        return Err(());
    }
    observe(ScratchEvent::ScratchCreated(&canonical_scratch));

    let scratch_path = canonical_scratch.join("scratch.db");
    let mut scratch = Connection::open_with_flags(
        scratch_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| ())?;
    let verification_result = run_fts_scratch_database_work(context, &mut scratch, observe);
    drop(scratch);
    verification_result
}

fn run_fts_scratch_database_work(
    context: &FtsScratchContext<'_>,
    scratch: &mut Connection,
    observe: &mut impl FnMut(ScratchEvent<'_>),
) -> Result<(), ()> {
    let policy = context.policy;
    let page_size = context.page_size;
    let page_count = context.page_count;
    scratch.busy_timeout(Duration::ZERO).map_err(|_| ())?;
    scratch
        .pragma_update(None, "page_size", i64::try_from(page_size).map_err(|_| ())?)
        .map_err(|_| ())?;
    let configured_page_size = scratch
        .query_row("PRAGMA page_size", [], |row| row.get::<_, i64>(0))
        .map_err(|_| ())?;
    if valid_page_size(configured_page_size)? != page_size {
        return Err(());
    }
    let max_page_count = policy.max_image_bytes.checked_div(page_size).ok_or(())?;
    if max_page_count == 0 || page_count > max_page_count {
        return Err(());
    }
    scratch
        .pragma_update(
            None,
            "max_page_count",
            i64::try_from(max_page_count).map_err(|_| ())?,
        )
        .map_err(|_| ())?;
    let configured_max_pages = scratch
        .query_row("PRAGMA max_page_count", [], |row| row.get::<_, i64>(0))
        .map_err(|_| ())?;
    if u64::try_from(configured_max_pages).map_err(|_| ())? != max_page_count {
        return Err(());
    }
    let cache_size = policy.cache_kib.checked_neg().ok_or(())?;
    scratch
        .pragma_update(None, "cache_size", cache_size)
        .map_err(|_| ())?;
    let configured_cache = scratch
        .query_row("PRAGMA cache_size", [], |row| row.get::<_, i64>(0))
        .map_err(|_| ())?;
    if configured_cache != cache_size || context.started.elapsed() >= policy.deadline {
        return Err(());
    }

    let backup = Backup::new(context.source, scratch).map_err(|_| ())?;
    observe(ScratchEvent::BackupStarted);
    let backup_result = run_backup_loop(
        policy,
        page_size,
        || {
            let step = match backup.step(policy.pages_per_step) {
                Ok(StepResult::Done) => BackupStep::Done,
                Ok(StepResult::More) => BackupStep::More,
                Ok(StepResult::Busy) => BackupStep::Busy,
                Ok(StepResult::Locked) => BackupStep::Locked,
                Ok(_) | Err(_) => BackupStep::Failed,
            };
            let progress = backup.progress();
            let Ok(remaining) = u64::try_from(progress.remaining) else {
                return Err(());
            };
            let Ok(page_count) = u64::try_from(progress.pagecount) else {
                return Err(());
            };
            Ok((
                step,
                BackupProgress {
                    remaining,
                    page_count,
                },
            ))
        },
        || context.started.elapsed(),
        thread::sleep,
    );
    drop(backup);
    backup_result?;
    if context.started.elapsed() >= policy.deadline {
        return Err(());
    }
    scratch
        .execute(
            "INSERT INTO search_fts(search_fts, rank) VALUES('integrity-check', 1)",
            [],
        )
        .map_err(|_| ())?;
    if context.started.elapsed() >= policy.deadline {
        return Err(());
    }
    Ok(())
}

fn run_backup_loop(
    policy: FtsScratchPolicy,
    page_size: u64,
    mut step: impl FnMut() -> Result<(BackupStep, BackupProgress), ()>,
    mut elapsed: impl FnMut() -> Duration,
    mut sleep: impl FnMut(Duration),
) -> Result<(), ()> {
    let mut previous_progress = None::<BackupProgress>;
    let mut backoff = policy.initial_backoff;
    loop {
        if elapsed() >= policy.deadline {
            return Err(());
        }
        let (step_result, progress) = step()?;
        if elapsed() >= policy.deadline
            || progress.remaining > progress.page_count
            || progress
                .page_count
                .checked_mul(page_size)
                .is_none_or(|bytes| bytes > policy.max_image_bytes)
        {
            return Err(());
        }
        match step_result {
            BackupStep::Done if progress.remaining == 0 => {
                if let Some(previous) = previous_progress
                    && (progress.page_count != previous.page_count
                        || progress.remaining >= previous.remaining)
                {
                    return Err(());
                }
                return Ok(());
            }
            BackupStep::Done | BackupStep::Failed => return Err(()),
            BackupStep::More => {
                if progress.remaining == 0 {
                    return Err(());
                }
                if let Some(previous) = previous_progress
                    && (progress.page_count != previous.page_count
                        || progress.remaining >= previous.remaining)
                {
                    return Err(());
                }
                previous_progress = Some(progress);
                backoff = policy.initial_backoff;
            }
            BackupStep::Busy | BackupStep::Locked => {
                if let Some(previous) = previous_progress
                    && (progress.page_count != previous.page_count
                        || progress.remaining != previous.remaining)
                {
                    return Err(());
                }
                let remaining_time = policy.deadline.saturating_sub(elapsed());
                let pause = backoff.min(remaining_time);
                if pause.is_zero() {
                    return Err(());
                }
                sleep(pause);
                backoff = backoff
                    .checked_mul(2)
                    .unwrap_or(policy.max_backoff)
                    .min(policy.max_backoff);
            }
        }
    }
}

fn valid_page_size(value: i64) -> Result<u64, ()> {
    let value = u64::try_from(value).map_err(|_| ())?;
    if !(512..=65_536).contains(&value) || !value.is_power_of_two() {
        return Err(());
    }
    Ok(value)
}

fn source_data_directory(source: &Connection) -> Result<PathBuf, ()> {
    let source_path = source.path().filter(|path| !path.is_empty()).ok_or(())?;
    let canonical_source = fs::canonicalize(source_path).map_err(|_| ())?;
    canonical_source.parent().map(Path::to_path_buf).ok_or(())
}

fn restrict_scratch_permissions(path: &Path) -> Result<(), ()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| ())?;
        let mode = fs::metadata(path).map_err(|_| ())?.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(());
        }
    }
    Ok(())
}

fn verification_output(
    snapshot: VerificationSnapshot,
    expect_records: u64,
) -> Result<VerifyOutput, CliFailure> {
    if snapshot.invalidated {
        return Ok(blob_root_failure_output(expect_records));
    }
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

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        fs, io,
        path::{Path, PathBuf},
        time::Duration,
    };

    use rusqlite::{Connection, OpenFlags};

    use clipboard_core::{
        CaptureInput, ContentFlags, ContentKind, EventFlags, RepresentationInput, SourceConfidence,
    };
    #[cfg(unix)]
    use clipboard_store::StorageBoundaryLease;
    use clipboard_store::{ReadOnlyStore, StoreConfig, StoreHandle};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::{
        BackupProgress, BackupStep, FtsScratchError, FtsScratchPolicy, ScratchEvent,
        VerificationPhase, audit_raw_payload_pages_with_observer, data_version,
        fts_is_coherent_with_policy, fts_scratch_check_with_cleanup, read_short_snapshot,
        read_verification_snapshot_with_observer, run_backup_loop, verification_output,
    };

    #[tokio::test]
    async fn transaction_boundary_heavy_phases_are_observed_only_in_autocommit() {
        let directory = tempfile::tempdir().unwrap();
        let config = StoreConfig::new(directory.path().join("history.sqlite"))
            .with_blob_root(directory.path().join("history.blobs"));
        let store = StoreHandle::open(config.clone()).unwrap();
        store
            .ingest(CaptureInput {
                captured_at_ms: 1_000,
                kind: ContentKind::Text,
                primary_mime: "text/plain".to_owned(),
                representations: vec![RepresentationInput {
                    format_id: "public.utf8-plain-text".to_owned(),
                    bytes: Some(b"transaction boundary payload".to_vec()),
                    missing_ref: None,
                }],
                source_app_id: None,
                source_app_name: None,
                source_confidence: SourceConfidence::Unknown,
                pinned: false,
                occurrence_count: 1,
                content_flags: ContentFlags::empty(),
                event_flags: EventFlags::empty(),
            })
            .await
            .unwrap();
        store
            .ingest(CaptureInput {
                captured_at_ms: 2_000,
                kind: ContentKind::Image,
                primary_mime: "image/png".to_owned(),
                representations: vec![RepresentationInput {
                    format_id: "public.png".to_owned(),
                    bytes: Some(vec![0x5a; 300 * 1024]),
                    missing_ref: None,
                }],
                source_app_id: None,
                source_app_name: None,
                source_confidence: SourceConfidence::Unknown,
                pinned: false,
                occurrence_count: 1,
                content_flags: ContentFlags::empty(),
                event_flags: EventFlags::empty(),
            })
            .await
            .unwrap();
        drop(store);
        let store = ReadOnlyStore::open_existing(config).unwrap();
        let cas = store.cas_store().unwrap();
        let observed = RefCell::new(Vec::new());

        store
            .with_reader(|connection| {
                read_verification_snapshot_with_observer(
                    connection,
                    &cas,
                    &mut |phase, connection| {
                        if matches!(
                            phase,
                            VerificationPhase::Cas
                                | VerificationPhase::DecodeHash
                                | VerificationPhase::Integrity
                                | VerificationPhase::Fts
                                | VerificationPhase::FtsBackup
                                | VerificationPhase::PayloadFetch
                        ) {
                            assert!(connection.is_autocommit());
                            observed.borrow_mut().push(phase);
                        }
                    },
                )
            })
            .unwrap();

        for phase in [
            VerificationPhase::Cas,
            VerificationPhase::DecodeHash,
            VerificationPhase::Integrity,
            VerificationPhase::Fts,
            VerificationPhase::FtsBackup,
            VerificationPhase::PayloadFetch,
        ] {
            assert!(observed.borrow().contains(&phase), "missing {phase:?}");
        }
    }

    #[test]
    fn bounded_payload_metadata_rejects_oversized_inline_values_before_materialization() {
        for (storage_kind, payload_len, original_size, declared_stored_size) in [
            ("inline", 4_096, 1, 1),
            ("inline_zstd", 263_169, 262_144, 263_168),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let database = directory.path().join("history.sqlite");
            let blob_root = directory.path().join("history.blobs");
            drop(
                clipboard_store::StoreHandle::open(
                    clipboard_store::StoreConfig::new(&database).with_blob_root(&blob_root),
                )
                .unwrap(),
            );
            let connection = Connection::open(&database).unwrap();
            connection
                .execute_batch("PRAGMA ignore_check_constraints = ON;")
                .unwrap();
            connection
                .execute(
                    "INSERT INTO raw_payload(
                       raw_digest, storage_kind, inline_payload, blob_relpath,
                       original_byte_size, stored_byte_size
                     ) VALUES (zeroblob(32), ?1, zeroblob(?2), NULL, ?3, ?4)",
                    rusqlite::params![
                        storage_kind,
                        payload_len,
                        original_size,
                        declared_stored_size
                    ],
                )
                .unwrap();
            let cas = clipboard_store::CasStore::new(blob_root);
            let payload_fetches = Cell::new(0_u32);
            let epoch = data_version(&connection).unwrap();

            let audit =
                audit_raw_payload_pages_with_observer(&connection, &cas, epoch, &mut |phase, _| {
                    if phase == VerificationPhase::PayloadFetch {
                        payload_fetches.set(payload_fetches.get() + 1);
                    }
                });

            assert!(
                audit.is_ok(),
                "{storage_kind} metadata query materialized the oversized BLOB"
            );
            let super::EpochRead::Stable(audit) = audit.unwrap() else {
                panic!("unchanged synthetic database unexpectedly invalidated the audit");
            };
            assert!(!audit.logical_ok);
            assert_eq!(
                payload_fetches.get(),
                0,
                "{storage_kind} payload was fetched before its metadata was bounded"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn private_storage_at_read_only_lease_creation_has_a_stable_code() {
        let directory = tempfile::tempdir().unwrap();
        let config = StoreConfig::new(directory.path().join("history.sqlite"))
            .with_blob_root(directory.path().join("blobs"));
        drop(StoreHandle::open(config.clone()).unwrap());
        fs::set_permissions(config.database_path(), fs::Permissions::from_mode(0o644)).unwrap();

        let error = match StorageBoundaryLease::open_read_only(&config) {
            Ok(_) => panic!("private storage must be rejected"),
            Err(error) => crate::boundary_failure(error),
        };

        assert_eq!(error.code, "private_storage_unavailable");
    }

    #[test]
    fn changing_database_version_marks_a_verification_snapshot_unstable() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("synthetic.db");
        let setup = Connection::open(&database).unwrap();
        setup
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 CREATE TABLE synthetic(value INTEGER NOT NULL);
                 INSERT INTO synthetic(value) VALUES (1);",
            )
            .unwrap();
        drop(setup);
        let reader = Connection::open(&database).unwrap();
        let modifier = Connection::open(&database).unwrap();

        let epoch = data_version(&reader).unwrap();
        let result = read_short_snapshot(&reader, epoch, |connection| {
            connection.query_row("SELECT count(*) FROM synthetic", [], |row| {
                row.get::<_, i64>(0)
            })?;
            modifier.execute("INSERT INTO synthetic(value) VALUES (2)", [])?;
            Ok(())
        })
        .unwrap();

        assert!(matches!(result, super::EpochRead::Changed));
    }

    #[tokio::test]
    async fn transaction_boundary_epoch_hooks_discard_every_aggregate_claim() {
        for target_phase in [
            VerificationPhase::AfterMetadataPage,
            VerificationPhase::FtsBackup,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let config = StoreConfig::new(directory.path().join("history.sqlite"))
                .with_blob_root(directory.path().join("history.blobs"));
            let writer = StoreHandle::open(config.clone()).unwrap();
            writer
                .ingest(CaptureInput {
                    captured_at_ms: 1_000,
                    kind: ContentKind::Text,
                    primary_mime: "text/plain".to_owned(),
                    representations: vec![RepresentationInput {
                        format_id: "public.utf8-plain-text".to_owned(),
                        bytes: Some(b"epoch hook payload".to_vec()),
                        missing_ref: None,
                    }],
                    source_app_id: None,
                    source_app_name: None,
                    source_confidence: SourceConfidence::Unknown,
                    pinned: false,
                    occurrence_count: 1,
                    content_flags: ContentFlags::empty(),
                    event_flags: EventFlags::empty(),
                })
                .await
                .unwrap();
            drop(writer);
            let modifier = Connection::open(config.database_path()).unwrap();
            let reader = ReadOnlyStore::open_existing(config).unwrap();
            let cas = reader.cas_store().unwrap();
            let hook_fired = Cell::new(false);

            let snapshot = reader
                .with_reader(|connection| {
                    read_verification_snapshot_with_observer(
                        connection,
                        &cas,
                        &mut |phase, connection| {
                            if phase == target_phase && !hook_fired.replace(true) {
                                assert!(connection.is_autocommit());
                                modifier
                                    .execute(
                                        "UPDATE history_event
                                         SET paste_count = paste_count + 1
                                         WHERE event_id = (SELECT min(event_id) FROM history_event)",
                                        [],
                                    )
                                    .unwrap();
                            }
                        },
                    )
                })
                .unwrap();
            assert!(hook_fired.get(), "missing {target_phase:?} hook");

            let Ok(output) = verification_output(snapshot, 1) else {
                panic!("invalidated snapshot must produce aggregate-only output");
            };
            assert_eq!(output.status, "failed");
            assert_eq!(output.latest_source_total, 0);
            assert_eq!(output.physical_content_count, 0);
            assert_eq!(output.event_count, 0);
            assert_eq!(output.indexed_document_count, 0);
            assert_eq!(output.missing_payload_count, 0);
            assert!(output.counts_by_kind.is_empty());
            assert!(output.sources.is_empty());
            assert_eq!(output.integrity_status, "failed");
            assert_eq!(output.run_status, "failed");
            assert_eq!(output.logical_status, "failed");
            assert_eq!(output.blob_status, "failed");
            assert_eq!(output.fts_status, "failed");
        }
    }

    fn read_only_fixture(data_dir: &Path) -> Connection {
        read_only_fixture_with_unindexed_document(data_dir, false)
    }

    fn read_only_fixture_with_unindexed_document(
        data_dir: &Path,
        with_unindexed_document: bool,
    ) -> Connection {
        fs::create_dir_all(data_dir).unwrap();
        let database_path = data_dir.join("synthetic.db");
        let writer = Connection::open(&database_path).unwrap();
        writer
            .execute_batch(
                "CREATE TABLE search_doc(
                   content_id INTEGER PRIMARY KEY,
                   normalized_text TEXT NOT NULL
                 );
                 CREATE VIRTUAL TABLE search_fts USING fts5(
                   normalized_text,
                   content = 'search_doc',
                   content_rowid = 'content_id'
                 );",
            )
            .unwrap();
        if with_unindexed_document {
            writer
                .execute(
                    "INSERT INTO search_doc(content_id, normalized_text) VALUES(1, 'synthetic')",
                    [],
                )
                .unwrap();
        }
        drop(writer);
        let reader = Connection::open_with_flags(
            fs::canonicalize(database_path).unwrap(),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .unwrap();
        reader.execute_batch("PRAGMA query_only = ON;").unwrap();
        reader
    }

    fn entry_names(directory: &Path) -> Vec<PathBuf> {
        let mut entries = fs::read_dir(directory)
            .unwrap()
            .map(|entry| PathBuf::from(entry.unwrap().file_name()))
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }

    fn ignore_scratch_event(_: ScratchEvent<'_>) {}

    #[test]
    fn oversized_source_image_is_rejected_before_scratch_creation() {
        let selected = tempfile::tempdir().unwrap();
        let reader = read_only_fixture(selected.path());
        let policy = FtsScratchPolicy::for_test(1, Duration::from_secs(1));
        let scratch_created = Cell::new(false);

        let coherent = fts_is_coherent_with_policy(&reader, policy, |event| {
            if matches!(event, ScratchEvent::ScratchCreated(_)) {
                scratch_created.set(true);
            }
        });

        assert!(!coherent);
        assert!(!scratch_created.get());
    }

    #[test]
    fn scratch_is_outside_selected_storage_and_source_directory_is_unchanged() {
        let selected = tempfile::tempdir().unwrap();
        let reader = read_only_fixture(selected.path());
        let before = entry_names(selected.path());
        let observed_scratch = RefCell::new(None::<PathBuf>);

        let coherent =
            fts_is_coherent_with_policy(&reader, FtsScratchPolicy::production(), |event| {
                if let ScratchEvent::ScratchCreated(path) = event {
                    assert!(!path.starts_with(selected.path()));
                    assert!(path.is_dir());
                    *observed_scratch.borrow_mut() = Some(path.to_path_buf());
                }
            });

        assert!(coherent);
        assert_eq!(entry_names(selected.path()), before);
        assert!(!observed_scratch.borrow().as_ref().unwrap().exists());
    }

    #[test]
    fn injected_cleanup_failure_fails_an_otherwise_healthy_check_without_path_data() {
        let selected = tempfile::tempdir().unwrap();
        let reader = read_only_fixture(selected.path());

        let result = fts_scratch_check_with_cleanup(
            &reader,
            FtsScratchPolicy::production(),
            &mut ignore_scratch_event,
            |directory| {
                directory.close()?;
                Err(io::Error::other("synthetic cleanup failure"))
            },
        );

        assert!(matches!(result, Err(FtsScratchError::Cleanup)));
    }

    #[test]
    fn cleanup_failure_takes_precedence_over_an_integrity_failure() {
        let selected = tempfile::tempdir().unwrap();
        let reader = read_only_fixture_with_unindexed_document(selected.path(), true);
        let verification_failure = fts_scratch_check_with_cleanup(
            &reader,
            FtsScratchPolicy::production(),
            &mut ignore_scratch_event,
            tempfile::TempDir::close,
        );
        assert!(matches!(
            verification_failure,
            Err(FtsScratchError::Verification)
        ));

        let result = fts_scratch_check_with_cleanup(
            &reader,
            FtsScratchPolicy::production(),
            &mut ignore_scratch_event,
            |directory| {
                directory.close()?;
                Err(io::Error::other("synthetic cleanup failure"))
            },
        );

        assert!(matches!(result, Err(FtsScratchError::Cleanup)));
    }

    #[test]
    fn busy_backup_retries_use_nonzero_backoff_and_stop_at_deadline() {
        let elapsed = Cell::new(Duration::ZERO);
        let calls = Cell::new(0_u32);
        let sleeps = RefCell::new(Vec::new());
        let policy = FtsScratchPolicy::for_test(4_096, Duration::from_millis(12));

        let result = run_backup_loop(
            policy,
            4_096,
            || {
                calls.set(calls.get() + 1);
                Ok((
                    BackupStep::Busy,
                    BackupProgress {
                        remaining: 1,
                        page_count: 1,
                    },
                ))
            },
            || elapsed.get(),
            |duration| {
                assert!(!duration.is_zero());
                sleeps.borrow_mut().push(duration);
                elapsed.set(elapsed.get() + duration);
            },
        );

        assert!(result.is_err());
        assert_eq!(calls.get(), 2);
        assert_eq!(
            sleeps.into_inner(),
            vec![Duration::from_millis(5), Duration::from_millis(7)]
        );
        assert_eq!(elapsed.get(), Duration::from_millis(12));
    }

    #[test]
    fn backup_progress_restart_or_stall_fails_closed() {
        for second in [
            BackupProgress {
                remaining: 2,
                page_count: 4,
            },
            BackupProgress {
                remaining: 4,
                page_count: 5,
            },
        ] {
            let calls = Cell::new(0_u32);
            let policy = FtsScratchPolicy::for_test(64 * 1024, Duration::from_secs(1));
            let result = run_backup_loop(
                policy,
                4_096,
                || {
                    let call = calls.get();
                    calls.set(call + 1);
                    Ok((
                        BackupStep::More,
                        if call == 0 {
                            BackupProgress {
                                remaining: 2,
                                page_count: 4,
                            }
                        } else {
                            second
                        },
                    ))
                },
                Duration::default,
                |_| {},
            );
            assert!(result.is_err());
            assert_eq!(calls.get(), 2);
        }
    }
}
