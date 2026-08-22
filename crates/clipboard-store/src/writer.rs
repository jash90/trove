use std::{
    collections::BTreeMap,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use clipboard_core::{
    CaptureInput, ContentFlags, ContentHash, ContentKind, SourceConfidence, content_hash,
    normalize_search_text,
};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::{
    CasError, CasStore, StoreConfig, migrations,
    reader::{open_reader_connection, open_writer_connection},
};

pub const WRITER_QUEUE_CAPACITY: usize = 256;
pub const IMPORT_BATCH_SIZE: usize = 250;
pub const MAX_SEARCH_DERIVATIONS_PER_CONTENT: usize = 16;
pub const MAX_SEARCH_DERIVATION_BYTES: usize = 64 * 1024;
pub const MAX_SEARCH_DOCUMENT_BYTES: usize = 512 * 1024;
pub const MAX_PREVIEW_BYTES: usize = 512;
const MAX_INLINE_PAYLOAD_BYTES: usize = 4 * 1024;
const MAX_INLINE_ZSTD_PAYLOAD_BYTES: usize = 256 * 1024;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    #[error("SQLite WAL mode is unavailable (reported {0:?})")]
    WalUnavailable(String),
    #[error("database schema version {0} is newer than this application supports")]
    UnsupportedSchemaVersion(i64),
    #[error("database schema is incompatible; development reset required")]
    IncompatibleSchema,
    #[error("database does not exist")]
    DatabaseMissing,
    #[error("payload storage is unavailable until CAS storage is configured")]
    PayloadStorageUnavailable,
    #[error(transparent)]
    Cas(#[from] CasError),
    #[error("failed to compress inline payload")]
    PayloadCompression(#[source] io::Error),
    #[error("the database writer is no longer running")]
    WriterClosed,
    #[error("the database writer dropped its response")]
    WriterResponseDropped,
    #[error("failed to start the database writer thread")]
    WriterThreadSpawn(#[source] io::Error),
    #[error("invalid import input")]
    InvalidImportInput,
    #[error("import run not found")]
    ImportRunNotFound,
    #[error("import source does not match the persisted run")]
    ImportSourceMismatch,
    #[error("import run identity conflicts with persisted input")]
    ImportRunConflict,
    #[error("import run is not resumable")]
    ImportRunNotResumable,
    #[error("import checkpoint is inconsistent")]
    ImportCheckpointMismatch,
    #[error("import worker was superseded")]
    ImportWorkerSuperseded,
    #[error("import accounting invariant failed")]
    ImportInvariant,
    #[error("import batch exceeds the configured bound")]
    ImportBatchTooLarge,
    #[doc(hidden)]
    #[error("injected import persistence failure")]
    InjectedImportPersistenceFailure,
    #[doc(hidden)]
    #[error("injected import status failure")]
    InjectedImportStatusFailure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IngestOutcome {
    pub content_id: i64,
    pub event_id: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoreStats {
    pub content_count: i64,
    pub event_count: i64,
}

#[derive(Clone, Eq, PartialEq)]
pub enum StoredPayload {
    Inline(Vec<u8>),
    InlineZstd(Vec<u8>),
    Cas {
        hash: clipboard_core::ContentHash,
        relpath: String,
        byte_size: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportSourceKind {
    Raycast,
    SuperCmd,
}

impl ImportSourceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Raycast => "raycast",
            Self::SuperCmd => "supercmd",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportFailureCount {
    pub reason_code: String,
    pub count: u64,
}

pub struct BeginImportRun {
    pub run_id: Uuid,
    pub source_kind: ImportSourceKind,
    pub source_fingerprint: [u8; 32],
    pub total_records: u64,
    pub candidate_records: u64,
    pub initial_failures: Vec<ImportFailureCount>,
}

pub struct ResumeImportRun {
    pub run_id: Uuid,
    pub source_kind: ImportSourceKind,
    pub source_fingerprint: [u8; 32],
    pub total_records: u64,
    pub candidate_records: u64,
}

pub struct StoreImportCandidate {
    pub candidate_offset: u64,
    pub record_fingerprint: [u8; 32],
    pub capture: CaptureInput,
    pub search_text: Option<String>,
    pub source_app_original: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreImportRunState {
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreImportRunStatus {
    pub run_id: Uuid,
    pub state: StoreImportRunState,
    pub total_records: u64,
    pub candidate_records: u64,
    pub next_candidate_offset: u64,
    pub imported_records: u64,
    pub already_present_records: u64,
    pub skipped_records: u64,
    pub failed_records: u64,
    pub error_code: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImportWorkerLease {
    pub run_id: Uuid,
    pub state: StoreImportRunState,
    pub generation: u64,
    pub next_candidate_offset: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImportBatchOutcome {
    pub processed_candidates: u64,
}

#[derive(Clone)]
pub struct StoreHandle {
    config: StoreConfig,
    tx: mpsc::Sender<WriteCommand>,
    import_status_available: Arc<AtomicBool>,
}

enum WriteCommand {
    Ingest {
        input: CaptureInput,
        reply: oneshot::Sender<Result<IngestOutcome, StoreError>>,
    },
    SetPinned {
        event_id: i64,
        pinned: bool,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    DeleteEvent {
        event_id: i64,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
    BeginImport {
        input: BeginImportRun,
        reply: oneshot::Sender<Result<ImportWorkerLease, StoreError>>,
    },
    InjectBeginImportFailure {
        reply: oneshot::Sender<Result<ImportWorkerLease, StoreError>>,
    },
    ResumeImport {
        input: ResumeImportRun,
        reply: oneshot::Sender<Result<ImportWorkerLease, StoreError>>,
    },
    ImportBatch {
        run_id: Uuid,
        generation: u64,
        candidates: Vec<StoreImportCandidate>,
        reply: oneshot::Sender<Result<ImportBatchOutcome, StoreError>>,
    },
    FinishImport {
        run_id: Uuid,
        generation: u64,
        reply: oneshot::Sender<Result<StoreImportRunStatus, StoreError>>,
    },
    FailImport {
        run_id: Uuid,
        generation: u64,
        error_code: String,
        reply: oneshot::Sender<Result<(), StoreError>>,
    },
}

impl StoreHandle {
    pub fn open(config: StoreConfig) -> Result<Self, StoreError> {
        let cas = CasStore::new(config.blob_root().to_path_buf());
        let mut connection = open_writer_connection(&config)?;
        migrations().apply(&mut connection)?;

        let (tx, mut rx) = mpsc::channel(WRITER_QUEUE_CAPACITY);
        thread::Builder::new()
            .name("clipboard-db-writer".to_owned())
            .spawn(move || {
                while let Some(command) = rx.blocking_recv() {
                    handle_command(&mut connection, &cas, command);
                }
            })
            .map_err(StoreError::WriterThreadSpawn)?;

        Ok(Self {
            config,
            tx,
            import_status_available: Arc::new(AtomicBool::new(true)),
        })
    }

    pub fn config(&self) -> &StoreConfig {
        &self.config
    }

    pub async fn ingest(&self, input: CaptureInput) -> Result<IngestOutcome, StoreError> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(WriteCommand::Ingest { input, reply })
            .await
            .map_err(|_| StoreError::WriterClosed)?;
        response
            .await
            .map_err(|_| StoreError::WriterResponseDropped)?
    }

    pub async fn set_pinned(&self, event_id: i64, pinned: bool) -> Result<(), StoreError> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(WriteCommand::SetPinned {
                event_id,
                pinned,
                reply,
            })
            .await
            .map_err(|_| StoreError::WriterClosed)?;
        response
            .await
            .map_err(|_| StoreError::WriterResponseDropped)?
    }

    pub async fn delete_event(&self, event_id: i64) -> Result<(), StoreError> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(WriteCommand::DeleteEvent { event_id, reply })
            .await
            .map_err(|_| StoreError::WriterClosed)?;
        response
            .await
            .map_err(|_| StoreError::WriterResponseDropped)?
    }

    pub async fn begin_import(
        &self,
        input: BeginImportRun,
    ) -> Result<ImportWorkerLease, StoreError> {
        validate_begin_import(&input)?;
        let (reply, response) = oneshot::channel();
        self.tx
            .send(WriteCommand::BeginImport { input, reply })
            .await
            .map_err(|_| StoreError::WriterClosed)?;
        response
            .await
            .map_err(|_| StoreError::WriterResponseDropped)?
    }

    #[doc(hidden)]
    pub async fn inject_begin_import_failure(&self) -> Result<ImportWorkerLease, StoreError> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(WriteCommand::InjectBeginImportFailure { reply })
            .await
            .map_err(|_| StoreError::WriterClosed)?;
        response
            .await
            .map_err(|_| StoreError::WriterResponseDropped)?
    }

    pub async fn resume_import(
        &self,
        input: ResumeImportRun,
    ) -> Result<ImportWorkerLease, StoreError> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(WriteCommand::ResumeImport { input, reply })
            .await
            .map_err(|_| StoreError::WriterClosed)?;
        response
            .await
            .map_err(|_| StoreError::WriterResponseDropped)?
    }

    pub async fn import_batch(
        &self,
        run_id: Uuid,
        generation: u64,
        candidates: Vec<StoreImportCandidate>,
    ) -> Result<ImportBatchOutcome, StoreError> {
        if candidates.len() > IMPORT_BATCH_SIZE {
            return Err(StoreError::ImportBatchTooLarge);
        }
        let (reply, response) = oneshot::channel();
        self.tx
            .send(WriteCommand::ImportBatch {
                run_id,
                generation,
                candidates,
                reply,
            })
            .await
            .map_err(|_| StoreError::WriterClosed)?;
        response
            .await
            .map_err(|_| StoreError::WriterResponseDropped)?
    }

    pub async fn finish_import(
        &self,
        run_id: Uuid,
        generation: u64,
    ) -> Result<StoreImportRunStatus, StoreError> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(WriteCommand::FinishImport {
                run_id,
                generation,
                reply,
            })
            .await
            .map_err(|_| StoreError::WriterClosed)?;
        response
            .await
            .map_err(|_| StoreError::WriterResponseDropped)?
    }

    pub async fn fail_import(
        &self,
        run_id: Uuid,
        generation: u64,
        error_code: &str,
    ) -> Result<(), StoreError> {
        if !valid_reason_code(error_code) {
            return Err(StoreError::InvalidImportInput);
        }
        let (reply, response) = oneshot::channel();
        self.tx
            .send(WriteCommand::FailImport {
                run_id,
                generation,
                error_code: error_code.to_owned(),
                reply,
            })
            .await
            .map_err(|_| StoreError::WriterClosed)?;
        response
            .await
            .map_err(|_| StoreError::WriterResponseDropped)?
    }

    pub fn import_status(&self, run_id: Uuid) -> Result<StoreImportRunStatus, StoreError> {
        if !self.import_status_available.load(Ordering::Acquire) {
            return Err(StoreError::InjectedImportStatusFailure);
        }
        let connection = open_reader_connection(&self.config)?;
        read_import_status(&connection, run_id)
    }

    #[doc(hidden)]
    pub fn set_import_status_available_for_test(&self, available: bool) {
        self.import_status_available
            .store(available, Ordering::Release);
    }

    pub fn stats(&self) -> Result<StoreStats, StoreError> {
        self.with_reader(|connection| {
            Ok(StoreStats {
                content_count: connection
                    .query_row("SELECT count(*) FROM content", [], |row| row.get(0))?,
                event_count: connection.query_row(
                    "SELECT count(*) FROM history_event",
                    [],
                    |row| row.get(0),
                )?,
            })
        })
    }

    pub fn with_reader<T>(
        &self,
        operation: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, StoreError> {
        let connection = open_reader_connection(&self.config)?;
        Ok(operation(&connection)?)
    }
}

fn handle_command(connection: &mut Connection, cas: &CasStore, command: WriteCommand) {
    match command {
        WriteCommand::Ingest { input, reply } => {
            let _ = reply.send(ingest(connection, cas, &input));
        }
        WriteCommand::SetPinned {
            event_id,
            pinned,
            reply,
        } => {
            let _ = reply.send(set_pinned(connection, event_id, pinned));
        }
        WriteCommand::DeleteEvent { event_id, reply } => {
            let _ = reply.send(delete_event(connection, event_id));
        }
        WriteCommand::BeginImport { input, reply } => {
            let _ = reply.send(begin_import(connection, &input));
        }
        WriteCommand::InjectBeginImportFailure { reply } => {
            let _ = reply.send(Err(StoreError::InjectedImportPersistenceFailure));
        }
        WriteCommand::ResumeImport { input, reply } => {
            let _ = reply.send(resume_import(connection, &input));
        }
        WriteCommand::ImportBatch {
            run_id,
            generation,
            candidates,
            reply,
        } => {
            let _ = reply.send(import_batch(
                connection,
                cas,
                run_id,
                generation,
                &candidates,
            ));
        }
        WriteCommand::FinishImport {
            run_id,
            generation,
            reply,
        } => {
            let _ = reply.send(finish_import(connection, run_id, generation));
        }
        WriteCommand::FailImport {
            run_id,
            generation,
            error_code,
            reply,
        } => {
            let _ = reply.send(fail_import(connection, run_id, generation, &error_code));
        }
    }
}

fn ingest(
    connection: &mut Connection,
    cas: &CasStore,
    input: &CaptureInput,
) -> Result<IngestOutcome, StoreError> {
    let prepared = prepare_ingest(input, cas)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let outcome = write_ingest(&transaction, input, &prepared, None, None)?;
    transaction.commit()?;
    Ok(outcome)
}

fn begin_import(
    connection: &mut Connection,
    input: &BeginImportRun,
) -> Result<ImportWorkerLease, StoreError> {
    validate_begin_import(input)?;
    let initial_failed = input
        .initial_failures
        .iter()
        .try_fold(0_u64, |sum, failure| sum.checked_add(failure.count))
        .ok_or(StoreError::InvalidImportInput)?;
    let initial_failure_fingerprint = initial_failure_fingerprint(&input.initial_failures)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let persisted = transaction.query_row(
        "SELECT source_kind, source_fingerprint, total_records, candidate_records,
                initial_failure_fingerprint, worker_generation, next_candidate_offset, status
         FROM import_run WHERE external_id = ?1",
        [input.run_id.as_bytes().as_slice()],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Vec<u8>>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
            ))
        },
    );
    match persisted {
        Ok(persisted) => {
            if persisted.0 != input.source_kind.as_str()
                || persisted.1.as_slice() != input.source_fingerprint
                || persisted.2 != sql_count(input.total_records)?
                || persisted.3 != sql_count(input.candidate_records)?
                || persisted.4.as_slice() != initial_failure_fingerprint
            {
                return Err(StoreError::ImportRunConflict);
            }
            return Ok(ImportWorkerLease {
                run_id: input.run_id,
                state: parse_import_run_state(&persisted.7)?,
                generation: rust_count(persisted.5)?,
                next_candidate_offset: rust_count(persisted.6)?,
            });
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => {}
        Err(error) => return Err(StoreError::Database(error)),
    }
    transaction.execute(
        "INSERT INTO import_run
           (external_id, source_kind, source_fingerprint, initial_failure_fingerprint, status,
            worker_generation, total_records, candidate_records, next_candidate_offset, imported_records,
            already_present_records, skipped_records, failed_records, started_at_ms)
         VALUES (?1, ?2, ?3, ?4, 'running', 1, ?5, ?6, 0, 0, 0, 0, ?7, ?8)",
        params![
            input.run_id.as_bytes().as_slice(),
            input.source_kind.as_str(),
            input.source_fingerprint.as_slice(),
            initial_failure_fingerprint.as_slice(),
            sql_count(input.total_records)?,
            sql_count(input.candidate_records)?,
            sql_count(initial_failed)?,
            now_ms(),
        ],
    )?;
    let import_run_id = transaction.last_insert_rowid();
    for failure in &input.initial_failures {
        transaction.execute(
            "INSERT INTO import_failure_reason(import_run_id, reason_code, count)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(import_run_id, reason_code)
             DO UPDATE SET count = count + excluded.count",
            params![
                import_run_id,
                failure.reason_code,
                sql_count(failure.count)?
            ],
        )?;
    }
    transaction.commit()?;
    Ok(ImportWorkerLease {
        run_id: input.run_id,
        state: StoreImportRunState::Running,
        generation: 1,
        next_candidate_offset: 0,
    })
}

fn initial_failure_fingerprint(failures: &[ImportFailureCount]) -> Result<[u8; 32], StoreError> {
    let mut canonical = BTreeMap::<&str, u64>::new();
    for failure in failures {
        let count = canonical.entry(&failure.reason_code).or_default();
        *count = count
            .checked_add(failure.count)
            .ok_or(StoreError::InvalidImportInput)?;
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"clipboard-store.import-initial-failures-v1");
    hasher.update(&(canonical.len() as u64).to_be_bytes());
    for (reason, count) in canonical {
        hasher.update(&(reason.len() as u64).to_be_bytes());
        hasher.update(reason.as_bytes());
        hasher.update(&count.to_be_bytes());
    }
    Ok(*hasher.finalize().as_bytes())
}

fn resume_import(
    connection: &mut Connection,
    input: &ResumeImportRun,
) -> Result<ImportWorkerLease, StoreError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let persisted = transaction
        .query_row(
            "SELECT import_run_id, source_kind, source_fingerprint, total_records,
                    candidate_records, status, worker_generation, next_candidate_offset
             FROM import_run WHERE external_id = ?1",
            [input.run_id.as_bytes().as_slice()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            },
        )
        .map_err(map_run_query_error)?;
    if persisted.1 != input.source_kind.as_str()
        || persisted.2.as_slice() != input.source_fingerprint
        || persisted.3 != sql_count(input.total_records)?
        || persisted.4 != sql_count(input.candidate_records)?
    {
        return Err(StoreError::ImportSourceMismatch);
    }
    if persisted.5 != "running" {
        return Err(StoreError::ImportRunNotResumable);
    }
    let generation = persisted
        .6
        .checked_add(1)
        .ok_or(StoreError::ImportInvariant)?;
    let updated = transaction.execute(
        "UPDATE import_run SET worker_generation = ?1
         WHERE import_run_id = ?2 AND worker_generation = ?3 AND status = 'running'",
        params![generation, persisted.0, persisted.6],
    )?;
    if updated != 1 {
        return Err(StoreError::ImportWorkerSuperseded);
    }
    transaction.commit()?;
    Ok(ImportWorkerLease {
        run_id: input.run_id,
        state: StoreImportRunState::Running,
        generation: rust_count(generation)?,
        next_candidate_offset: rust_count(persisted.7)?,
    })
}

fn import_batch(
    connection: &mut Connection,
    cas: &CasStore,
    run_id: Uuid,
    generation: u64,
    candidates: &[StoreImportCandidate],
) -> Result<ImportBatchOutcome, StoreError> {
    if candidates.len() > IMPORT_BATCH_SIZE {
        return Err(StoreError::ImportBatchTooLarge);
    }
    let mut processed_candidates = 0_u64;
    for candidate in candidates {
        match prepare_ingest(&candidate.capture, cas) {
            Ok(prepared) => {
                commit_import_candidate(connection, run_id, generation, candidate, &prepared)?;
            }
            Err(error) => {
                record_import_candidate_failure(
                    connection,
                    run_id,
                    generation,
                    candidate.candidate_offset,
                    candidate_failure_reason(&error),
                )?;
            }
        }
        processed_candidates += 1;
    }
    Ok(ImportBatchOutcome {
        processed_candidates,
    })
}

fn commit_import_candidate(
    connection: &mut Connection,
    run_id: Uuid,
    generation: u64,
    candidate: &StoreImportCandidate,
    prepared: &PreparedIngest<'_>,
) -> Result<(), StoreError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let checkpoint = import_checkpoint(&transaction, run_id, generation)?;
    if checkpoint.next_candidate_offset != sql_count(candidate.candidate_offset)? {
        return Err(StoreError::ImportCheckpointMismatch);
    }
    let inserted = transaction.execute(
        "INSERT INTO import_record
           (import_run_id, source_kind, record_fingerprint, event_id, created_at_ms)
         VALUES (?1, ?2, ?3, NULL, ?4)
         ON CONFLICT(source_kind, record_fingerprint) DO NOTHING",
        params![
            checkpoint.import_run_id,
            checkpoint.source_kind,
            candidate.record_fingerprint.as_slice(),
            now_ms(),
        ],
    )?;
    if inserted == 0 {
        advance_import_counter(&transaction, &checkpoint, ImportCounter::AlreadyPresent)?;
        transaction.commit()?;
        return Ok(());
    }
    let import_record_id = transaction.last_insert_rowid();
    let outcome = write_ingest(
        &transaction,
        &candidate.capture,
        prepared,
        Some(candidate.search_text.as_deref()),
        candidate.source_app_original.as_deref(),
    )?;
    transaction.execute(
        "UPDATE import_record SET event_id = ?1 WHERE import_record_id = ?2",
        params![outcome.event_id, import_record_id],
    )?;
    advance_import_counter(&transaction, &checkpoint, ImportCounter::Imported)?;
    transaction.commit()?;
    Ok(())
}

fn record_import_candidate_failure(
    connection: &mut Connection,
    run_id: Uuid,
    generation: u64,
    candidate_offset: u64,
    reason_code: &'static str,
) -> Result<(), StoreError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let checkpoint = import_checkpoint(&transaction, run_id, generation)?;
    if checkpoint.next_candidate_offset != sql_count(candidate_offset)? {
        return Err(StoreError::ImportCheckpointMismatch);
    }
    advance_import_counter(&transaction, &checkpoint, ImportCounter::Failed)?;
    transaction.execute(
        "INSERT INTO import_failure_reason(import_run_id, reason_code, count)
         VALUES (?1, ?2, 1)
         ON CONFLICT(import_run_id, reason_code)
         DO UPDATE SET count = count + 1",
        params![checkpoint.import_run_id, reason_code],
    )?;
    transaction.commit()?;
    Ok(())
}

fn finish_import(
    connection: &mut Connection,
    run_id: Uuid,
    generation: u64,
) -> Result<StoreImportRunStatus, StoreError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let values = transaction
        .query_row(
            "SELECT candidate_records, next_candidate_offset, total_records,
                    imported_records, already_present_records, skipped_records, failed_records,
                    status, worker_generation
             FROM import_run WHERE external_id = ?1",
            [run_id.as_bytes().as_slice()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            },
        )
        .map_err(map_run_query_error)?;
    if values.8 != sql_count(generation)? {
        return Err(StoreError::ImportWorkerSuperseded);
    }
    if values.7 != "running" {
        return Err(StoreError::ImportRunNotResumable);
    }
    let processed = values
        .3
        .checked_add(values.4)
        .and_then(|count| count.checked_add(values.5))
        .and_then(|count| count.checked_add(values.6))
        .ok_or(StoreError::ImportInvariant)?;
    if values.0 != values.1 || values.2 != processed {
        return Err(StoreError::ImportInvariant);
    }
    transaction.execute(
        "UPDATE import_run
         SET status = 'completed', finished_at_ms = ?1, error_code = NULL
         WHERE external_id = ?2 AND worker_generation = ?3",
        params![
            now_ms(),
            run_id.as_bytes().as_slice(),
            sql_count(generation)?
        ],
    )?;
    transaction.commit()?;
    read_import_status(connection, run_id)
}

fn fail_import(
    connection: &mut Connection,
    run_id: Uuid,
    generation: u64,
    error_code: &str,
) -> Result<(), StoreError> {
    if !valid_reason_code(error_code) {
        return Err(StoreError::InvalidImportInput);
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let updated = transaction.execute(
        "UPDATE import_run
         SET status = 'failed', finished_at_ms = ?1, error_code = ?2
         WHERE external_id = ?3 AND worker_generation = ?4 AND status = 'running'",
        params![
            now_ms(),
            error_code,
            run_id.as_bytes().as_slice(),
            sql_count(generation)?
        ],
    )?;
    if updated == 0 {
        let persisted = transaction.query_row(
            "SELECT status, worker_generation FROM import_run WHERE external_id = ?1",
            [run_id.as_bytes().as_slice()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        );
        return match persisted {
            Err(rusqlite::Error::QueryReturnedNoRows) => Err(StoreError::ImportRunNotFound),
            Err(error) => Err(StoreError::Database(error)),
            Ok((_, persisted_generation)) if persisted_generation != sql_count(generation)? => {
                Err(StoreError::ImportWorkerSuperseded)
            }
            Ok(_) => Err(StoreError::ImportRunNotResumable),
        };
    }
    transaction.commit()?;
    Ok(())
}

fn set_pinned(connection: &mut Connection, event_id: i64, pinned: bool) -> Result<(), StoreError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute(
        "UPDATE history_event SET pinned = ?1 WHERE event_id = ?2",
        params![i64::from(pinned), event_id],
    )?;
    transaction.commit()?;
    Ok(())
}

fn delete_event(connection: &mut Connection, event_id: i64) -> Result<(), StoreError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute("DELETE FROM history_event WHERE event_id = ?1", [event_id])?;
    transaction.commit()?;
    Ok(())
}

struct PreparedIngest<'a> {
    content_hash: ContentHash,
    byte_size: u64,
    preview_text: String,
    primary_payload: Option<&'a [u8]>,
    representations: Vec<StoredRepresentation<'a>>,
}

fn prepare_ingest<'a>(
    input: &'a CaptureInput,
    cas: &CasStore,
) -> Result<PreparedIngest<'a>, StoreError> {
    let primary = input
        .representations
        .first()
        .ok_or(StoreError::PayloadStorageUnavailable)?;
    let (content_hash, byte_size, primary_payload) =
        match (primary.bytes.as_deref(), primary.missing_ref.as_deref()) {
            (Some(bytes), None) => (
                content_hash(input.kind, &input.primary_mime, bytes),
                bytes.len() as u64,
                Some(bytes),
            ),
            (None, Some(missing_ref)) => (
                missing_content_hash(input.kind, &input.primary_mime, missing_ref),
                0,
                None,
            ),
            _ => return Err(StoreError::PayloadStorageUnavailable),
        };
    Ok(PreparedIngest {
        content_hash,
        byte_size,
        preview_text: original_preview(input.kind, primary_payload),
        primary_payload,
        representations: stored_representations(input, cas)?,
    })
}

fn write_ingest(
    transaction: &Transaction<'_>,
    input: &CaptureInput,
    prepared: &PreparedIngest<'_>,
    search_override: Option<Option<&str>>,
    source_app_original: Option<&str>,
) -> Result<IngestOutcome, StoreError> {
    let normalized_text = normalized_text(input, prepared.primary_payload, search_override);
    transaction.execute(
        "INSERT INTO content
           (content_hash, kind, primary_mime, byte_size, preview_text, flags, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(content_hash) DO NOTHING",
        params![
            prepared.content_hash.as_slice(),
            input.kind.as_str(),
            input.primary_mime,
            sql_count(prepared.byte_size)?,
            prepared.preview_text,
            i64::from(input.content_flags.bits()),
            input.captured_at_ms,
        ],
    )?;
    let content_id = transaction.query_row(
        "SELECT content_id FROM content WHERE content_hash = ?1",
        [prepared.content_hash.as_slice()],
        |row| row.get::<_, i64>(0),
    )?;
    transaction.execute(
        "UPDATE content SET flags = flags | ?1 WHERE content_id = ?2",
        params![i64::from(input.content_flags.bits()), content_id],
    )?;
    let merged_content_flags = transaction.query_row(
        "SELECT flags FROM content WHERE content_id = ?1",
        [content_id],
        |row| row.get::<_, i64>(0),
    )?;

    for representation in &prepared.representations {
        let (storage_kind, inline_payload, blob_relpath, missing_ref, stored_byte_size) =
            representation.storage_values();
        transaction.execute(
            "INSERT INTO content_representation
               (content_id, format_id, storage_kind, inline_payload, blob_relpath, missing_ref,
                original_byte_size, stored_byte_size)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(content_id, format_id) DO NOTHING",
            params![
                content_id,
                representation.format_id,
                storage_kind,
                inline_payload,
                blob_relpath,
                missing_ref,
                sql_count(representation.original_byte_size)?,
                sql_count(stored_byte_size)?,
            ],
        )?;
    }

    if merged_content_flags & i64::from(ContentFlags::DO_NOT_INDEX.bits()) != 0 {
        transaction.execute("DELETE FROM search_doc WHERE content_id = ?1", [content_id])?;
        transaction.execute(
            "DELETE FROM search_derivation WHERE content_id = ?1",
            [content_id],
        )?;
    } else if let Some(normalized_text) = normalized_text {
        retain_search_derivation(transaction, content_id, normalized_text)?;
    }

    transaction.execute(
        "INSERT INTO history_event
           (global_id, content_id, captured_at_ms, source_app_id, source_app_name,
            source_app_original, source_confidence, pinned, occurrence_count, flags)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            Uuid::now_v7().as_bytes().as_slice(),
            content_id,
            input.captured_at_ms,
            input.source_app_id,
            input.source_app_name,
            source_app_original,
            source_confidence(input.source_confidence),
            i64::from(input.pinned),
            i64::from(input.occurrence_count),
            i64::from(input.event_flags.bits()),
        ],
    )?;
    Ok(IngestOutcome {
        content_id,
        event_id: transaction.last_insert_rowid(),
    })
}

fn original_preview(kind: ContentKind, primary_payload: Option<&[u8]>) -> String {
    if !kind.is_textual() {
        return String::new();
    }
    let Some(payload) = primary_payload else {
        return String::new();
    };
    let boundary = payload.len().min(MAX_PREVIEW_BYTES);
    match std::str::from_utf8(&payload[..boundary]) {
        Ok(value) => value.to_owned(),
        Err(error) => std::str::from_utf8(&payload[..error.valid_up_to()])
            .unwrap_or_default()
            .to_owned(),
    }
}

fn search_derivation_hash(normalized_text: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"clipboard-store.search-derivation-v1");
    hasher.update(&(normalized_text.len() as u64).to_be_bytes());
    hasher.update(normalized_text.as_bytes());
    *hasher.finalize().as_bytes()
}

fn retain_search_derivation(
    transaction: &Transaction<'_>,
    content_id: i64,
    normalized_text: String,
) -> Result<(), StoreError> {
    let normalized_text = truncate_utf8(normalized_text, MAX_SEARCH_DERIVATION_BYTES);
    let derivation_hash = search_derivation_hash(&normalized_text);
    let already_retained = transaction.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM search_derivation
           WHERE content_id = ?1 AND derivation_hash = ?2
         )",
        params![content_id, derivation_hash.as_slice()],
        |row| row.get::<_, bool>(0),
    )?;
    if already_retained {
        return Ok(());
    }

    let mut retained = {
        let mut statement = transaction.prepare(
            "SELECT derivation_hash, normalized_text FROM search_derivation
             WHERE content_id = ?1 ORDER BY derivation_hash
             LIMIT ?2",
        )?;
        statement
            .query_map(
                params![content_id, MAX_SEARCH_DERIVATIONS_PER_CONTENT as i64],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?
    };
    let new_hash = derivation_hash.to_vec();
    let removed_hash = if retained.len() == MAX_SEARCH_DERIVATIONS_PER_CONTENT {
        let Some((largest_hash, _)) = retained.last() else {
            return Err(StoreError::ImportInvariant);
        };
        if new_hash >= *largest_hash {
            return Ok(());
        }
        Some(retained.pop().ok_or(StoreError::ImportInvariant)?.0)
    } else {
        None
    };
    retained.push((new_hash.clone(), normalized_text.clone()));
    retained.sort_unstable_by(|left, right| left.0.cmp(&right.0));

    if let Some(removed_hash) = removed_hash {
        transaction.execute(
            "DELETE FROM search_derivation
             WHERE content_id = ?1 AND derivation_hash = ?2",
            params![content_id, removed_hash],
        )?;
    }
    transaction.execute(
        "INSERT INTO search_derivation(content_id, derivation_hash, normalized_text)
         VALUES (?1, ?2, ?3)",
        params![content_id, derivation_hash.as_slice(), normalized_text],
    )?;
    let merged_text = merge_search_derivations(&retained);
    transaction.execute(
        "INSERT INTO search_doc(content_id, normalized_text) VALUES (?1, ?2)
         ON CONFLICT(content_id) DO UPDATE
         SET normalized_text = excluded.normalized_text
         WHERE search_doc.normalized_text != excluded.normalized_text",
        params![content_id, merged_text],
    )?;
    Ok(())
}

fn truncate_utf8(mut value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value
}

fn merge_search_derivations(retained: &[(Vec<u8>, String)]) -> String {
    let mut merged = String::with_capacity(
        MAX_SEARCH_DOCUMENT_BYTES.min(
            retained
                .iter()
                .map(|(_, value)| value.len().saturating_add(1))
                .sum(),
        ),
    );
    for (_, value) in retained {
        if !merged.is_empty() && merged.len() < MAX_SEARCH_DOCUMENT_BYTES {
            merged.push('\n');
        }
        let remaining = MAX_SEARCH_DOCUMENT_BYTES.saturating_sub(merged.len());
        if remaining == 0 {
            break;
        }
        let mut boundary = value.len().min(remaining);
        while !value.is_char_boundary(boundary) {
            boundary -= 1;
        }
        merged.push_str(&value[..boundary]);
    }
    merged
}

struct StoredRepresentation<'a> {
    format_id: &'a str,
    original_byte_size: u64,
    payload: PreparedPayload,
}

enum PreparedPayload {
    Stored(StoredPayload),
    Missing(String),
}

impl StoredRepresentation<'_> {
    fn storage_values(&self) -> (&str, Option<&[u8]>, Option<&str>, Option<&str>, u64) {
        match &self.payload {
            PreparedPayload::Stored(StoredPayload::Inline(bytes)) => {
                ("inline", Some(bytes), None, None, bytes.len() as u64)
            }
            PreparedPayload::Stored(StoredPayload::InlineZstd(bytes)) => {
                ("inline_zstd", Some(bytes), None, None, bytes.len() as u64)
            }
            PreparedPayload::Stored(StoredPayload::Cas {
                relpath, byte_size, ..
            }) => ("cas", None, Some(relpath), None, *byte_size),
            PreparedPayload::Missing(missing_ref) => ("missing", None, None, Some(missing_ref), 0),
        }
    }
}

fn stored_representations<'a>(
    input: &'a CaptureInput,
    cas: &CasStore,
) -> Result<Vec<StoredRepresentation<'a>>, StoreError> {
    if input.representations.is_empty() {
        return Err(StoreError::PayloadStorageUnavailable);
    }
    input
        .representations
        .iter()
        .map(|representation| {
            if let Some(bytes) = representation.bytes.as_deref() {
                if representation.missing_ref.is_some() {
                    return Err(StoreError::PayloadStorageUnavailable);
                }
                return Ok(StoredRepresentation {
                    format_id: representation.format_id.as_str(),
                    original_byte_size: bytes.len() as u64,
                    payload: PreparedPayload::Stored(classify_payload(input.kind, bytes, cas)?),
                });
            }
            let missing_ref = representation
                .missing_ref
                .as_deref()
                .ok_or(StoreError::PayloadStorageUnavailable)?;
            Ok(StoredRepresentation {
                format_id: representation.format_id.as_str(),
                original_byte_size: 0,
                payload: PreparedPayload::Missing(missing_ref.to_owned()),
            })
        })
        .collect()
}

pub fn classify_payload(
    kind: ContentKind,
    bytes: &[u8],
    cas: &CasStore,
) -> Result<StoredPayload, StoreError> {
    if matches!(kind, ContentKind::Image | ContentKind::File)
        || bytes.len() > MAX_INLINE_ZSTD_PAYLOAD_BYTES
    {
        let blob = cas.put(bytes)?;
        return Ok(StoredPayload::Cas {
            hash: blob.hash,
            relpath: blob.relpath,
            byte_size: blob.byte_size,
        });
    }
    if bytes.len() >= MAX_INLINE_PAYLOAD_BYTES {
        let compressed = zstd::bulk::compress(bytes, 3).map_err(StoreError::PayloadCompression)?;
        return Ok(StoredPayload::InlineZstd(compressed));
    }
    Ok(StoredPayload::Inline(bytes.to_vec()))
}

fn normalized_text(
    input: &CaptureInput,
    primary_payload: Option<&[u8]>,
    search_override: Option<Option<&str>>,
) -> Option<String> {
    if input.content_flags.contains(ContentFlags::DO_NOT_INDEX) {
        return None;
    }
    if let Some(search_override) = search_override {
        return search_override.map(normalize_search_text_bounded);
    }
    if !input.kind.is_textual() {
        return None;
    }
    primary_payload
        .and_then(bounded_utf8_payload)
        .map(normalize_search_text_bounded)
}

fn normalize_search_text_bounded(value: &str) -> String {
    let boundary = utf8_boundary_at_or_before(value, MAX_SEARCH_DERIVATION_BYTES);
    truncate_utf8(
        normalize_search_text(&value[..boundary]),
        MAX_SEARCH_DERIVATION_BYTES,
    )
}

fn bounded_utf8_payload(payload: &[u8]) -> Option<&str> {
    let boundary = payload.len().min(MAX_SEARCH_DERIVATION_BYTES);
    match std::str::from_utf8(&payload[..boundary]) {
        Ok(value) => Some(value),
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&payload[..error.valid_up_to()]).ok()
        }
        Err(_) => None,
    }
}

fn utf8_boundary_at_or_before(value: &str, max_bytes: usize) -> usize {
    let mut boundary = value.len().min(max_bytes);
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

fn missing_content_hash(kind: ContentKind, primary_mime: &str, missing_ref: &str) -> ContentHash {
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

struct ImportCheckpoint {
    import_run_id: i64,
    source_kind: String,
    next_candidate_offset: i64,
    candidate_records: i64,
}

fn import_checkpoint(
    transaction: &Transaction<'_>,
    run_id: Uuid,
    generation: u64,
) -> Result<ImportCheckpoint, StoreError> {
    let checkpoint = transaction
        .query_row(
            "SELECT import_run_id, source_kind, next_candidate_offset, candidate_records, status,
                    worker_generation
             FROM import_run WHERE external_id = ?1",
            [run_id.as_bytes().as_slice()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .map_err(map_run_query_error)?;
    if checkpoint.5 != sql_count(generation)? {
        return Err(StoreError::ImportWorkerSuperseded);
    }
    if checkpoint.4 != "running" {
        return Err(StoreError::ImportRunNotResumable);
    }
    if checkpoint.2 >= checkpoint.3 {
        return Err(StoreError::ImportCheckpointMismatch);
    }
    Ok(ImportCheckpoint {
        import_run_id: checkpoint.0,
        source_kind: checkpoint.1,
        next_candidate_offset: checkpoint.2,
        candidate_records: checkpoint.3,
    })
}

enum ImportCounter {
    Imported,
    AlreadyPresent,
    Failed,
}

fn advance_import_counter(
    transaction: &Transaction<'_>,
    checkpoint: &ImportCheckpoint,
    counter: ImportCounter,
) -> Result<(), StoreError> {
    if checkpoint.next_candidate_offset >= checkpoint.candidate_records {
        return Err(StoreError::ImportCheckpointMismatch);
    }
    let sql = match counter {
        ImportCounter::Imported => {
            "UPDATE import_run
             SET imported_records = imported_records + 1,
                 next_candidate_offset = next_candidate_offset + 1
             WHERE import_run_id = ?1 AND next_candidate_offset = ?2 AND status = 'running'"
        }
        ImportCounter::AlreadyPresent => {
            "UPDATE import_run
             SET already_present_records = already_present_records + 1,
                 next_candidate_offset = next_candidate_offset + 1
             WHERE import_run_id = ?1 AND next_candidate_offset = ?2 AND status = 'running'"
        }
        ImportCounter::Failed => {
            "UPDATE import_run
             SET failed_records = failed_records + 1,
                 next_candidate_offset = next_candidate_offset + 1
             WHERE import_run_id = ?1 AND next_candidate_offset = ?2 AND status = 'running'"
        }
    };
    let updated = transaction.execute(
        sql,
        params![checkpoint.import_run_id, checkpoint.next_candidate_offset],
    )?;
    if updated != 1 {
        return Err(StoreError::ImportCheckpointMismatch);
    }
    Ok(())
}

fn read_import_status(
    connection: &Connection,
    run_id: Uuid,
) -> Result<StoreImportRunStatus, StoreError> {
    let values = connection
        .query_row(
            "SELECT status, total_records, candidate_records, next_candidate_offset,
                    imported_records, already_present_records, skipped_records, failed_records,
                    error_code
             FROM import_run WHERE external_id = ?1",
            [run_id.as_bytes().as_slice()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            },
        )
        .map_err(map_run_query_error)?;
    let state = parse_import_run_state(&values.0)?;
    Ok(StoreImportRunStatus {
        run_id,
        state,
        total_records: rust_count(values.1)?,
        candidate_records: rust_count(values.2)?,
        next_candidate_offset: rust_count(values.3)?,
        imported_records: rust_count(values.4)?,
        already_present_records: rust_count(values.5)?,
        skipped_records: rust_count(values.6)?,
        failed_records: rust_count(values.7)?,
        error_code: values.8,
    })
}

fn parse_import_run_state(value: &str) -> Result<StoreImportRunState, StoreError> {
    match value {
        "running" => Ok(StoreImportRunState::Running),
        "completed" => Ok(StoreImportRunState::Completed),
        "failed" => Ok(StoreImportRunState::Failed),
        _ => Err(StoreError::ImportInvariant),
    }
}

fn validate_begin_import(input: &BeginImportRun) -> Result<(), StoreError> {
    if input.run_id.get_version_num() != 7
        || input.run_id.get_variant() != uuid::Variant::RFC4122
        || input.candidate_records > input.total_records
    {
        return Err(StoreError::InvalidImportInput);
    }
    let initial_failed = input
        .initial_failures
        .iter()
        .try_fold(0_u64, |sum, failure| {
            if failure.count == 0 || !valid_reason_code(&failure.reason_code) {
                return None;
            }
            sum.checked_add(failure.count)
        })
        .ok_or(StoreError::InvalidImportInput)?;
    if input.candidate_records.checked_add(initial_failed) != Some(input.total_records) {
        return Err(StoreError::InvalidImportInput);
    }
    sql_count(input.total_records)?;
    sql_count(input.candidate_records)?;
    Ok(())
}

fn valid_reason_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn candidate_failure_reason(error: &StoreError) -> &'static str {
    match error {
        StoreError::PayloadStorageUnavailable => "invalid_candidate",
        StoreError::Cas(_) | StoreError::PayloadCompression(_) => "payload_storage_failed",
        _ => "candidate_storage_failed",
    }
}

fn map_run_query_error(error: rusqlite::Error) -> StoreError {
    match error {
        rusqlite::Error::QueryReturnedNoRows => StoreError::ImportRunNotFound,
        other => StoreError::Database(other),
    }
}

fn sql_count(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::InvalidImportInput)
}

fn rust_count(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::ImportInvariant)
}

fn now_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

fn source_confidence(value: SourceConfidence) -> &'static str {
    match value {
        SourceConfidence::Declared => "declared",
        SourceConfidence::Inferred => "inferred",
        SourceConfidence::Unknown => "unknown",
    }
}
