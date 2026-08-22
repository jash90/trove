use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use clipboard_core::ContentFlags;
use clipboard_store::{CasStore, ReadOnlyStore, StorageBoundaryLease};
use rusqlite::{
    Connection, MAIN_DB, OpenFlags,
    backup::{Backup, StepResult},
};
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
const MAX_FTS_SCRATCH_BYTES: u64 = 4_u64 * 1024 * 1024 * 1024;
const FTS_SCRATCH_CACHE_KIB: i64 = 16 * 1024;
const FTS_BACKUP_DEADLINE: Duration = Duration::from_secs(120);
const FTS_BACKUP_INITIAL_BACKOFF: Duration = Duration::from_millis(5);
const FTS_BACKUP_MAX_BACKOFF: Duration = Duration::from_secs(1);
const FTS_BACKUP_PAGES_PER_STEP: i32 = 256;

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
    if !exact_blob_root_is_valid(&canonical_data_dir, &blob_root) {
        return Ok(blob_root_failure_output(expect_records));
    }
    let boundary = Arc::new(
        StorageBoundaryLease::open_read_only(&config)
            .map_err(|_| CliFailure::new("unsafe_storage_layout"))?,
    );
    let store = ReadOnlyStore::open_existing(config.with_storage_boundary(boundary))
        .map_err(store_failure)?;
    let cas = store.cas_store().map_err(store_failure)?;
    let mut snapshot = store
        .with_reader(read_verification_snapshot)
        .map_err(store_failure)?;
    snapshot.blob_ok = blob_references_are_coherent(&cas, &snapshot.blob_references);
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

fn blob_references_are_coherent(cas: &CasStore, references: &[BlobReference]) -> bool {
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

fn fts_is_coherent(connection: &Connection, _document_count: i64) -> bool {
    fts_is_coherent_with_policy(
        connection,
        FtsScratchPolicy::production(),
        |event| match event {
            ScratchEvent::ScratchCreated(path) => {
                let _ = path;
            }
            ScratchEvent::BackupStarted => {}
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

    use super::{
        BackupProgress, BackupStep, FtsScratchError, FtsScratchPolicy, ScratchEvent,
        fts_is_coherent_with_policy, fts_scratch_check_with_cleanup, run_backup_loop,
    };

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
