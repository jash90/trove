use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap},
    io,
    mem::size_of,
    rc::Rc,
    sync::{
        Arc, Condvar, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};

use clipboard_core::{
    CaptureInput, ContentFlags, ContentHash, ContentKind, MAX_CANONICAL_NORMALIZATION_HEAP_BYTES,
    SourceConfidence, canonical_byte_len, content_hash, normalize_search_text_bounded,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::{
    CasError, CasStore, StorageBoundaryError, StorageBoundaryLease, StoreConfig,
    import_operation::ImportOperationPermit,
    migrations,
    reader::{open_reader_connection, open_writer_connection, required_boundary},
};

pub const WRITER_QUEUE_CAPACITY: usize = 256;
pub const MAX_STORE_READERS: usize = 8;
pub const IMPORT_BATCH_SIZE: usize = 250;
pub const MAX_IMPORT_BATCH_BYTES: usize = 60 * 1024 * 1024;
pub const MAX_IMPORT_WRITER_SCRATCH_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_IMPORT_REPRESENTATIONS: usize = 2;
pub const MAX_SEARCH_DERIVATIONS_PER_CONTENT: usize = 16;
pub const MAX_SEARCH_DERIVATION_BYTES: usize = 64 * 1024;
pub const MAX_SEARCH_DOCUMENT_BYTES: usize = 512 * 1024;
pub const MAX_PREVIEW_BYTES: usize = 512;
pub const MAX_APP_SETTING_KEY_BYTES: usize = 128;
pub const MAX_APP_SETTING_JSON_BYTES: usize = 64 * 1024;
const MAX_INLINE_PAYLOAD_BYTES: usize = 4 * 1024;
const MAX_INLINE_ZSTD_PAYLOAD_BYTES: usize = 256 * 1024;
const MAX_PRIMARY_MIME_BYTES: usize = 1024;
const MAX_FORMAT_ID_BYTES: usize = 1024;
const MAX_MISSING_REFERENCE_BYTES: usize = 4096;
const IMPORT_ZSTD_COMPRESSION_LEVEL: i32 = 3;
const MAX_IMPORT_ZSTD_OUTPUT_BYTES_PER_REPRESENTATION: usize = 263_168;
const MAX_IMPORT_ZSTD_OUTPUT_BYTES: usize =
    MAX_IMPORT_REPRESENTATIONS * MAX_IMPORT_ZSTD_OUTPUT_BYTES_PER_REPRESENTATION;
const MAX_IMPORT_ZSTD_CODEC_BYTES: usize = 2 * 1024 * 1024;
const MAX_IMPORT_REPRESENTATION_METADATA_BYTES: usize = 4 * 1024;
const MAX_IMPORT_CAS_TRANSIENT_METADATA_BYTES: usize = 256 * 1024;
const MAX_IMPORT_SEARCH_PEAK_BYTES: usize = MAX_IMPORT_ZSTD_OUTPUT_BYTES
    + MAX_CANONICAL_NORMALIZATION_HEAP_BYTES
    + MAX_SEARCH_DERIVATION_BYTES
    + MAX_SEARCH_DERIVATION_BYTES
    + MAX_SEARCH_DERIVATION_BYTES
    + 32
    + MAX_SEARCH_DERIVATIONS_PER_CONTENT
        * (size_of::<(Vec<u8>, String)>() + 32 + MAX_SEARCH_DERIVATION_BYTES)
    + MAX_SEARCH_DOCUMENT_BYTES
    + MAX_PREVIEW_BYTES
    + crate::CAS_VERIFY_BUFFER_BYTES
    + MAX_IMPORT_REPRESENTATION_METADATA_BYTES
    + MAX_IMPORT_CAS_TRANSIENT_METADATA_BYTES;
const MAX_IMPORT_COMPRESSION_PEAK_BYTES: usize = MAX_IMPORT_ZSTD_OUTPUT_BYTES
    + MAX_CANONICAL_NORMALIZATION_HEAP_BYTES
    + MAX_IMPORT_ZSTD_CODEC_BYTES
    + MAX_PREVIEW_BYTES
    + crate::CAS_VERIFY_BUFFER_BYTES
    + MAX_IMPORT_REPRESENTATION_METADATA_BYTES
    + MAX_IMPORT_CAS_TRANSIENT_METADATA_BYTES;

const _: () = assert!(
    MAX_IMPORT_REPRESENTATIONS * (size_of::<StoredRepresentation<'static>>() + 128)
        <= MAX_IMPORT_REPRESENTATION_METADATA_BYTES
);
const _: () = assert!(MAX_IMPORT_SEARCH_PEAK_BYTES <= MAX_IMPORT_WRITER_SCRATCH_BYTES);
const _: () = assert!(MAX_IMPORT_COMPRESSION_PEAK_BYTES <= MAX_IMPORT_WRITER_SCRATCH_BYTES);

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
    #[error("storage boundary changed")]
    StorageBoundary,
    #[error("private_storage_unavailable")]
    PrivateStorageUnavailable,
    #[error("store_runtime_configuration_mismatch")]
    RuntimeConfigurationMismatch,
    #[error("payload storage is unavailable until CAS storage is configured")]
    PayloadStorageUnavailable,
    #[error("canonicalization_too_complex")]
    CanonicalizationTooComplex,
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
    #[error("invalid_content_flags")]
    InvalidContentFlags,
    #[error("history_event_not_found")]
    HistoryEventNotFound,
    #[error("invalid_app_setting")]
    InvalidAppSetting,
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

impl From<StorageBoundaryError> for StoreError {
    fn from(error: StorageBoundaryError) -> Self {
        match error {
            StorageBoundaryError::Changed => Self::StorageBoundary,
            StorageBoundaryError::PrivateStorageUnavailable => Self::PrivateStorageUnavailable,
        }
    }
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
pub(crate) enum StoredPayload {
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
    /// Records the parser deliberately left out, by reason.
    ///
    /// Separate from failures: nothing went wrong, the record simply had
    /// nothing worth keeping. Counting them as failures would report a healthy
    /// import as broken.
    pub initial_skips: Vec<ImportFailureCount>,
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
    pub search_ocr: Option<String>,
    pub source_app_original: Option<String>,
}

impl StoreImportCandidate {
    /// Returns the heap capacity moved with this candidate, excluding its slot in the batch Vec.
    #[doc(hidden)]
    pub fn owned_allocation_bytes(&self) -> Option<usize> {
        let capture = &self.capture;
        let mut bytes = capture
            .primary_mime
            .capacity()
            .checked_add(option_string_capacity(&capture.source_app_id))?
            .checked_add(option_string_capacity(&capture.source_app_name))?
            .checked_add(option_string_capacity(&self.search_ocr))?
            .checked_add(option_string_capacity(&self.source_app_original))?
            .checked_add(
                capture
                    .representations
                    .capacity()
                    .checked_mul(size_of::<clipboard_core::RepresentationInput>())?,
            )?;
        for representation in &capture.representations {
            bytes = bytes
                .checked_add(representation.format_id.capacity())?
                .checked_add(representation.bytes.as_ref().map_or(0, Vec::capacity))?
                .checked_add(option_string_capacity(&representation.missing_ref))?;
        }
        Some(bytes)
    }
}

fn option_string_capacity(value: &Option<String>) -> usize {
    value.as_ref().map_or(0, String::capacity)
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

pub struct StoreHandle {
    config: StoreConfig,
    runtime: RuntimeRef,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct RuntimeIdentity((u64, u64));

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RuntimeConfiguration {
    blob_identity: (u64, u64),
}

enum RegistryEntry {
    Initializing,
    Ready {
        runtime: Weak<StoreRuntime>,
        clients: usize,
    },
    Closing,
}

struct RuntimeRegistry {
    entries: Mutex<HashMap<RuntimeIdentity, RegistryEntry>>,
    changed: Condvar,
}

impl RuntimeRegistry {
    fn global() -> &'static Self {
        static REGISTRY: OnceLock<RuntimeRegistry> = OnceLock::new();
        REGISTRY.get_or_init(|| Self {
            entries: Mutex::new(HashMap::new()),
            changed: Condvar::new(),
        })
    }
}

struct ReaderGate {
    active: Mutex<usize>,
    changed: Condvar,
}

struct ReaderPermit<'a> {
    gate: &'a ReaderGate,
}

impl ReaderGate {
    fn new() -> Self {
        Self {
            active: Mutex::new(0),
            changed: Condvar::new(),
        }
    }

    fn acquire(&self) -> ReaderPermit<'_> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        while *active >= MAX_STORE_READERS {
            active = self
                .changed
                .wait(active)
                .unwrap_or_else(|error| error.into_inner());
        }
        *active += 1;
        ReaderPermit { gate: self }
    }
}

impl Drop for ReaderPermit<'_> {
    fn drop(&mut self) {
        let mut active = self
            .gate
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *active -= 1;
        self.gate.changed.notify_one();
    }
}

thread_local! {
    static ACTIVE_READERS: RefCell<Vec<(usize, Rc<Connection>)>> = const { RefCell::new(Vec::new()) };
}

struct ActiveReader {
    runtime_id: usize,
}

impl Drop for ActiveReader {
    fn drop(&mut self) {
        ACTIVE_READERS.with(|readers| {
            let mut readers = readers.borrow_mut();
            let index = readers
                .iter()
                .rposition(|(runtime_id, _)| *runtime_id == self.runtime_id)
                .expect("active store reader is registered");
            readers.remove(index);
        });
    }
}

enum WriterSlot {
    Empty,
    Initializing,
    Ready(WriterRuntime),
}

struct WriterRuntime {
    tx: mpsc::Sender<WriteCommand>,
    join: Option<JoinHandle<()>>,
}

struct StoreRuntime {
    id: usize,
    identity: RuntimeIdentity,
    configuration: RuntimeConfiguration,
    readers: ReaderGate,
    writer: Mutex<WriterSlot>,
    writer_changed: Condvar,
    import_status_available: AtomicBool,
}

pub(crate) struct RuntimeRef {
    runtime: Arc<StoreRuntime>,
}

impl RuntimeRef {
    pub(crate) fn with_reader<T, E>(
        &self,
        config: &StoreConfig,
        operation: impl FnOnce(&Connection) -> Result<T, E>,
    ) -> Result<T, StoreError>
    where
        StoreError: From<E>,
    {
        self.runtime.with_reader(config, operation)
    }
}

impl Clone for RuntimeRef {
    fn clone(&self) -> Self {
        let registry = RuntimeRegistry::global();
        let mut entries = registry
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match entries.get_mut(&self.runtime.identity) {
            Some(RegistryEntry::Ready { clients, .. }) => *clients += 1,
            _ => unreachable!("a referenced store runtime must remain registered"),
        }
        drop(entries);
        Self {
            runtime: Arc::clone(&self.runtime),
        }
    }
}

impl Drop for RuntimeRef {
    fn drop(&mut self) {
        let registry = RuntimeRegistry::global();
        let should_close = {
            let mut entries = registry
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match entries.get_mut(&self.runtime.identity) {
                Some(RegistryEntry::Ready { clients, .. }) if *clients > 1 => {
                    *clients -= 1;
                    false
                }
                Some(entry @ RegistryEntry::Ready { .. }) => {
                    *entry = RegistryEntry::Closing;
                    true
                }
                _ => false,
            }
        };
        if should_close {
            self.runtime.shutdown();
            let mut entries = registry
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if matches!(
                entries.get(&self.runtime.identity),
                Some(RegistryEntry::Closing)
            ) {
                entries.remove(&self.runtime.identity);
            }
            registry.changed.notify_all();
        }
    }
}

impl StoreRuntime {
    fn new(identity: RuntimeIdentity, configuration: RuntimeConfiguration) -> Self {
        static NEXT_RUNTIME_ID: AtomicUsize = AtomicUsize::new(1);
        Self {
            id: NEXT_RUNTIME_ID.fetch_add(1, Ordering::Relaxed),
            identity,
            configuration,
            readers: ReaderGate::new(),
            writer: Mutex::new(WriterSlot::Empty),
            writer_changed: Condvar::new(),
            import_status_available: AtomicBool::new(true),
        }
    }

    fn ensure_writer(
        &self,
        config: &StoreConfig,
        boundary: Arc<StorageBoundaryLease>,
    ) -> Result<(), StoreError> {
        loop {
            let mut writer = self
                .writer
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match &*writer {
                WriterSlot::Ready(_) => return Ok(()),
                WriterSlot::Initializing => {
                    drop(
                        self.writer_changed
                            .wait(writer)
                            .unwrap_or_else(|error| error.into_inner()),
                    );
                }
                WriterSlot::Empty => {
                    *writer = WriterSlot::Initializing;
                    break;
                }
            }
        }

        let initialized = start_writer(config, boundary);
        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match initialized {
            Ok(runtime) => {
                *writer = WriterSlot::Ready(runtime);
                self.writer_changed.notify_all();
                Ok(())
            }
            Err(error) => {
                *writer = WriterSlot::Empty;
                self.writer_changed.notify_all();
                Err(error)
            }
        }
    }

    fn writer_sender(&self) -> Result<mpsc::Sender<WriteCommand>, StoreError> {
        let writer = self
            .writer
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match &*writer {
            WriterSlot::Ready(runtime) => Ok(runtime.tx.clone()),
            WriterSlot::Empty | WriterSlot::Initializing => Err(StoreError::WriterClosed),
        }
    }

    fn with_reader<T, E>(
        &self,
        config: &StoreConfig,
        operation: impl FnOnce(&Connection) -> Result<T, E>,
    ) -> Result<T, StoreError>
    where
        StoreError: From<E>,
    {
        let mut operation = Some(operation);
        if let Some(connection) = ACTIVE_READERS.with(|readers| {
            readers
                .borrow()
                .iter()
                .rev()
                .find(|(runtime_id, _)| *runtime_id == self.id)
                .map(|(_, connection)| Rc::clone(connection))
        }) {
            let result = operation.take().expect("reader callback is available")(&connection);
            required_boundary(config)?
                .validate()
                .map_err(<StoreError as From<StorageBoundaryError>>::from)?;
            return result.map_err(StoreError::from);
        }

        let _permit = self.readers.acquire();
        let connection = Rc::new(open_reader_connection(config)?);
        ACTIVE_READERS.with(|readers| {
            readers.borrow_mut().push((self.id, Rc::clone(&connection)));
        });
        let _active = ActiveReader {
            runtime_id: self.id,
        };
        let result = operation.take().expect("reader callback is available")(&connection);
        required_boundary(config)?
            .validate()
            .map_err(<StoreError as From<StorageBoundaryError>>::from)?;
        result.map_err(StoreError::from)
    }

    fn shutdown(&self) {
        let runtime = {
            let mut writer = self
                .writer
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match std::mem::replace(&mut *writer, WriterSlot::Empty) {
                WriterSlot::Ready(runtime) => Some(runtime),
                WriterSlot::Empty | WriterSlot::Initializing => None,
            }
        };
        if let Some(mut runtime) = runtime {
            drop(runtime.tx);
            if let Some(join) = runtime.join.take() {
                let _ = join.join();
            }
        }
    }
}

fn acquire_runtime(
    config: &StoreConfig,
    boundary: Arc<StorageBoundaryLease>,
    writer: bool,
) -> Result<RuntimeRef, StoreError> {
    let identity = RuntimeIdentity(boundary.database_identity_key());
    let configuration = RuntimeConfiguration {
        blob_identity: boundary.blob_identity_key(),
    };
    let registry = RuntimeRegistry::global();
    loop {
        let mut entries = registry
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match entries.get_mut(&identity) {
            Some(RegistryEntry::Ready { runtime, clients }) => {
                let runtime = runtime
                    .upgrade()
                    .expect("registered store runtime has a live client");
                if runtime.configuration != configuration {
                    return Err(StoreError::RuntimeConfigurationMismatch);
                }
                *clients += 1;
                drop(entries);
                let runtime_ref = RuntimeRef { runtime };
                if writer {
                    runtime_ref
                        .runtime
                        .ensure_writer(config, Arc::clone(&boundary))?;
                }
                return Ok(runtime_ref);
            }
            Some(RegistryEntry::Initializing | RegistryEntry::Closing) => {
                drop(
                    registry
                        .changed
                        .wait(entries)
                        .unwrap_or_else(|error| error.into_inner()),
                );
            }
            None => {
                entries.insert(identity, RegistryEntry::Initializing);
                drop(entries);
                let initialized = (|| {
                    let runtime = Arc::new(StoreRuntime::new(identity, configuration));
                    if writer {
                        runtime.ensure_writer(config, Arc::clone(&boundary))?;
                    } else {
                        let connection = open_reader_connection(config)?;
                        crate::migrations::validate_current_schema(&connection)?;
                        boundary.validate().map_err(StoreError::from)?;
                    }
                    Ok::<_, StoreError>(runtime)
                })();
                let mut entries = registry
                    .entries
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                match initialized {
                    Ok(runtime) => {
                        entries.insert(
                            identity,
                            RegistryEntry::Ready {
                                runtime: Arc::downgrade(&runtime),
                                clients: 1,
                            },
                        );
                        registry.changed.notify_all();
                        return Ok(RuntimeRef { runtime });
                    }
                    Err(error) => {
                        entries.remove(&identity);
                        registry.changed.notify_all();
                        return Err(error);
                    }
                }
            }
        }
    }
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
    SaveSetting {
        key: String,
        value_json: String,
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
        operation_permit: ImportOperationPermit,
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
    pub fn open(mut config: StoreConfig) -> Result<Self, StoreError> {
        let boundary = match config.storage_boundary() {
            Some(boundary) => {
                boundary
                    .validate_preflight_for_config(&config, true)
                    .map_err(StoreError::from)?;
                Arc::clone(boundary)
            }
            None => {
                let boundary = Arc::new(
                    StorageBoundaryLease::create_writer_preflight(&config)
                        .map_err(StoreError::from)?,
                );
                config.set_storage_boundary(Arc::clone(&boundary));
                boundary
            }
        };
        let runtime = acquire_runtime(&config, Arc::clone(&boundary), true)?;
        boundary
            .validate_for_config(&config, true)
            .map_err(StoreError::from)?;
        Ok(Self { config, runtime })
    }

    pub fn config(&self) -> &StoreConfig {
        &self.config
    }

    pub fn cas_store(&self) -> Result<CasStore, StoreError> {
        let boundary = Arc::clone(required_boundary(&self.config)?);
        Ok(CasStore::with_storage_boundary(
            self.config.blob_root().to_path_buf(),
            boundary,
        ))
    }

    #[doc(hidden)]
    pub fn shares_runtime_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.runtime.runtime, &other.runtime.runtime)
    }

    pub async fn ingest(&self, input: CaptureInput) -> Result<IngestOutcome, StoreError> {
        let (reply, response) = oneshot::channel();
        self.runtime
            .runtime
            .writer_sender()?
            .send(WriteCommand::Ingest { input, reply })
            .await
            .map_err(|_| StoreError::WriterClosed)?;
        response
            .await
            .map_err(|_| StoreError::WriterResponseDropped)?
    }

    pub async fn set_pinned(&self, event_id: i64, pinned: bool) -> Result<(), StoreError> {
        let (reply, response) = oneshot::channel();
        self.runtime
            .runtime
            .writer_sender()?
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
        self.runtime
            .runtime
            .writer_sender()?
            .send(WriteCommand::DeleteEvent { event_id, reply })
            .await
            .map_err(|_| StoreError::WriterClosed)?;
        response
            .await
            .map_err(|_| StoreError::WriterResponseDropped)?
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        validate_setting_pair(key, "")?;
        self.with_reader(|connection| {
            connection
                .query_row(
                    "SELECT value_json FROM app_setting WHERE key = ?1",
                    [key],
                    |row| row.get(0),
                )
                .optional()
        })
    }

    pub async fn save_setting(&self, key: &str, value_json: &str) -> Result<(), StoreError> {
        validate_setting_pair(key, value_json)?;
        let (reply, response) = oneshot::channel();
        self.runtime
            .runtime
            .writer_sender()?
            .send(WriteCommand::SaveSetting {
                key: key.to_owned(),
                value_json: value_json.to_owned(),
                reply,
            })
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
        self.runtime
            .runtime
            .writer_sender()?
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
        self.runtime
            .runtime
            .writer_sender()?
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
        self.runtime
            .runtime
            .writer_sender()?
            .send(WriteCommand::ResumeImport { input, reply })
            .await
            .map_err(|_| StoreError::WriterClosed)?;
        response
            .await
            .map_err(|_| StoreError::WriterResponseDropped)?
    }

    pub async fn import_batch(
        &self,
        operation_permit: ImportOperationPermit,
        run_id: Uuid,
        generation: u64,
        candidates: Vec<StoreImportCandidate>,
    ) -> Result<ImportBatchOutcome, StoreError> {
        if !operation_permit.has_process_wide_origin() {
            return Err(StoreError::InvalidImportInput);
        }
        if validate_import_batch(&candidates, operation_permit.reserved_bytes()).is_err() {
            return Err(StoreError::ImportBatchTooLarge);
        }
        let (reply, response) = oneshot::channel();
        self.runtime
            .runtime
            .writer_sender()?
            .send(WriteCommand::ImportBatch {
                operation_permit,
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
        self.runtime
            .runtime
            .writer_sender()?
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
        self.runtime
            .runtime
            .writer_sender()?
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
        if !self
            .runtime
            .runtime
            .import_status_available
            .load(Ordering::Acquire)
        {
            return Err(StoreError::InjectedImportStatusFailure);
        }
        self.runtime.with_reader(&self.config, |connection| {
            read_import_status(connection, run_id)
        })
    }

    #[doc(hidden)]
    pub fn set_import_status_available_for_test(&self, available: bool) {
        self.runtime
            .runtime
            .import_status_available
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
        self.runtime.runtime.with_reader(&self.config, operation)
    }
}

impl Clone for StoreHandle {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            runtime: self.runtime.clone(),
        }
    }
}

fn start_writer(
    config: &StoreConfig,
    boundary: Arc<StorageBoundaryLease>,
) -> Result<WriterRuntime, StoreError> {
    let cas =
        CasStore::with_storage_boundary(config.blob_root().to_path_buf(), Arc::clone(&boundary));
    let mut connection = open_writer_connection(config)?;
    migrations().apply(&mut connection)?;
    boundary
        .harden_sqlite_sidecars()
        .map_err(StoreError::from)?;
    boundary.validate().map_err(StoreError::from)?;
    let (tx, mut rx) = mpsc::channel(WRITER_QUEUE_CAPACITY);
    let writer_boundary = Arc::clone(&boundary);
    let join = thread::Builder::new()
        .name("clipboard-db-writer".to_owned())
        .spawn(move || {
            while let Some(command) = rx.blocking_recv() {
                handle_command(&mut connection, &cas, &writer_boundary, command);
            }
        })
        .map_err(StoreError::WriterThreadSpawn)?;
    Ok(WriterRuntime {
        tx,
        join: Some(join),
    })
}

pub(crate) fn acquire_read_runtime(
    config: &StoreConfig,
    boundary: Arc<StorageBoundaryLease>,
) -> Result<RuntimeRef, StoreError> {
    acquire_runtime(config, boundary, false)
}

fn validate_import_batch(
    candidates: &Vec<StoreImportCandidate>,
    reserved_operation_bytes: usize,
) -> Result<usize, StoreError> {
    if candidates.len() > IMPORT_BATCH_SIZE
        || candidates
            .iter()
            .any(|candidate| candidate.capture.representations.len() > MAX_IMPORT_REPRESENTATIONS)
    {
        return Err(StoreError::ImportBatchTooLarge);
    }
    let bytes = candidates
        .capacity()
        .checked_mul(size_of::<StoreImportCandidate>())
        .and_then(|batch_slots| {
            candidates.iter().try_fold(batch_slots, |total, candidate| {
                total.checked_add(candidate.owned_allocation_bytes()?)
            })
        })
        .ok_or(StoreError::ImportBatchTooLarge)?;
    let required = if candidates.is_empty() {
        bytes
    } else {
        bytes
            .checked_add(MAX_IMPORT_WRITER_SCRATCH_BYTES)
            .ok_or(StoreError::ImportBatchTooLarge)?
    };
    if bytes > MAX_IMPORT_BATCH_BYTES || required > reserved_operation_bytes {
        return Err(StoreError::ImportBatchTooLarge);
    }
    Ok(bytes)
}

fn handle_command(
    connection: &mut Connection,
    cas: &CasStore,
    boundary: &StorageBoundaryLease,
    command: WriteCommand,
) {
    match command {
        WriteCommand::Ingest { input, reply } => {
            let _ = reply.send(with_storage_boundary(boundary, || {
                ingest(connection, cas, &input)
            }));
        }
        WriteCommand::SetPinned {
            event_id,
            pinned,
            reply,
        } => {
            let _ = reply.send(with_storage_boundary(boundary, || {
                set_pinned(connection, event_id, pinned)
            }));
        }
        WriteCommand::DeleteEvent { event_id, reply } => {
            let _ = reply.send(with_storage_boundary(boundary, || {
                delete_event(connection, event_id)
            }));
        }
        WriteCommand::SaveSetting {
            key,
            value_json,
            reply,
        } => {
            let _ = reply.send(with_storage_boundary(boundary, || {
                save_setting(connection, &key, &value_json)
            }));
        }
        WriteCommand::BeginImport { input, reply } => {
            let _ = reply.send(with_storage_boundary(boundary, || {
                begin_import(connection, &input)
            }));
        }
        WriteCommand::InjectBeginImportFailure { reply } => {
            let _ = reply.send(with_storage_boundary(boundary, || {
                Err(StoreError::InjectedImportPersistenceFailure)
            }));
        }
        WriteCommand::ResumeImport { input, reply } => {
            let _ = reply.send(with_storage_boundary(boundary, || {
                resume_import(connection, &input)
            }));
        }
        WriteCommand::ImportBatch {
            operation_permit: _operation_permit,
            run_id,
            generation,
            candidates,
            reply,
        } => {
            let _ = reply.send(with_storage_boundary(boundary, || {
                import_batch(connection, cas, run_id, generation, &candidates)
            }));
        }
        WriteCommand::FinishImport {
            run_id,
            generation,
            reply,
        } => {
            let _ = reply.send(with_storage_boundary(boundary, || {
                finish_import(connection, run_id, generation)
            }));
        }
        WriteCommand::FailImport {
            run_id,
            generation,
            error_code,
            reply,
        } => {
            let _ = reply.send(with_storage_boundary(boundary, || {
                fail_import(connection, run_id, generation, &error_code)
            }));
        }
    }
}

fn with_storage_boundary<T>(
    boundary: &StorageBoundaryLease,
    operation: impl FnOnce() -> Result<T, StoreError>,
) -> Result<T, StoreError> {
    boundary.validate().map_err(StoreError::from)?;
    let result = operation();
    boundary.validate().map_err(StoreError::from)?;
    result
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

fn save_setting(
    connection: &mut Connection,
    key: &str,
    value_json: &str,
) -> Result<(), StoreError> {
    validate_setting_pair(key, value_json)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute(
        "INSERT INTO app_setting(key, value_json, updated_at_ms)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET
           value_json = excluded.value_json,
           updated_at_ms = excluded.updated_at_ms",
        params![key, value_json, now_ms()],
    )?;
    transaction.commit()?;
    Ok(())
}

fn validate_setting_pair(key: &str, value_json: &str) -> Result<(), StoreError> {
    if key.is_empty()
        || key.len() > MAX_APP_SETTING_KEY_BYTES
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_' || byte.is_ascii_digit())
        || value_json.len() > MAX_APP_SETTING_JSON_BYTES
    {
        return Err(StoreError::InvalidAppSetting);
    }
    Ok(())
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
    let initial_skipped = input
        .initial_skips
        .iter()
        .try_fold(0_u64, |sum, skip| sum.checked_add(skip.count))
        .ok_or(StoreError::InvalidImportInput)?;
    let initial_failure_fingerprint =
        initial_failure_fingerprint(&input.initial_failures, &input.initial_skips)?;
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
         VALUES (?1, ?2, ?3, ?4, 'running', 1, ?5, ?6, 0, 0, 0, ?7, ?8, ?9)",
        params![
            input.run_id.as_bytes().as_slice(),
            input.source_kind.as_str(),
            input.source_fingerprint.as_slice(),
            initial_failure_fingerprint.as_slice(),
            sql_count(input.total_records)?,
            sql_count(input.candidate_records)?,
            sql_count(initial_skipped)?,
            sql_count(initial_failed)?,
            now_ms(),
        ],
    )?;
    let import_run_id = transaction.last_insert_rowid();
    for failure in input.initial_failures.iter().chain(&input.initial_skips) {
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

fn initial_failure_fingerprint(
    failures: &[ImportFailureCount],
    skips: &[ImportFailureCount],
) -> Result<[u8; 32], StoreError> {
    let mut canonical = BTreeMap::<&str, u64>::new();
    for failure in failures.iter().chain(skips) {
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
    if candidates.len() > IMPORT_BATCH_SIZE
        || candidates
            .iter()
            .any(|candidate| candidate.capture.representations.len() > MAX_IMPORT_REPRESENTATIONS)
    {
        return Err(StoreError::ImportBatchTooLarge);
    }
    let mut processed_candidates = 0_u64;
    for candidate in candidates {
        match prepare_ingest(&candidate.capture, cas) {
            Ok(prepared) => {
                commit_import_candidate(connection, run_id, generation, candidate, &prepared)?;
            }
            Err(error) => {
                if is_private_storage_error(&error) {
                    return Err(error);
                }
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

fn is_private_storage_error(error: &StoreError) -> bool {
    matches!(
        error,
        StoreError::PrivateStorageUnavailable
            | StoreError::Cas(CasError::PrivateStorageUnavailable)
    )
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
        candidate.search_ocr.as_deref(),
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
    let updated = transaction.execute(
        "UPDATE history_event SET pinned = ?1 WHERE event_id = ?2",
        params![i64::from(pinned), event_id],
    )?;
    if updated == 0 {
        return Err(StoreError::HistoryEventNotFound);
    }
    transaction.commit()?;
    Ok(())
}

fn delete_event(connection: &mut Connection, event_id: i64) -> Result<(), StoreError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let deleted =
        transaction.execute("DELETE FROM history_event WHERE event_id = ?1", [event_id])?;
    if deleted == 0 {
        return Err(StoreError::HistoryEventNotFound);
    }
    transaction.commit()?;
    Ok(())
}

struct PreparedIngest<'a> {
    content_hash: ContentHash,
    byte_size: u64,
    preview_text: String,
    primary_payload: Option<&'a [u8]>,
    content_flags: ContentFlags,
    representations: Vec<StoredRepresentation<'a>>,
}

fn prepare_ingest<'a>(
    input: &'a CaptureInput,
    cas: &CasStore,
) -> Result<PreparedIngest<'a>, StoreError> {
    validate_capture_storage_bounds(input)?;
    let primary = input
        .representations
        .first()
        .ok_or(StoreError::PayloadStorageUnavailable)?;
    let (content_hash, byte_size, primary_payload, content_flags) =
        match (primary.bytes.as_deref(), primary.missing_ref.as_deref()) {
            (Some(bytes), None) => {
                if input.content_flags.contains(ContentFlags::MISSING_PAYLOAD) {
                    return Err(StoreError::InvalidContentFlags);
                }
                let byte_size = canonical_byte_len(input.kind, bytes)
                    .map_err(|_| StoreError::CanonicalizationTooComplex)?;
                let hash = content_hash(input.kind, &input.primary_mime, bytes)
                    .map_err(|_| StoreError::CanonicalizationTooComplex)?;
                (hash, byte_size as u64, Some(bytes), input.content_flags)
            }
            (None, Some(missing_ref)) => {
                if !input.content_flags.contains(ContentFlags::MISSING_PAYLOAD) {
                    return Err(StoreError::InvalidContentFlags);
                }
                (
                    missing_content_hash(input.kind, &input.primary_mime, missing_ref),
                    0,
                    None,
                    input.content_flags,
                )
            }
            _ => return Err(StoreError::PayloadStorageUnavailable),
        };
    Ok(PreparedIngest {
        content_hash,
        byte_size,
        preview_text: original_preview(input.kind, primary_payload, input.display_label.as_deref()),
        primary_payload,
        content_flags,
        representations: stored_representations(input, cas)?,
    })
}

fn validate_capture_storage_bounds(input: &CaptureInput) -> Result<(), StoreError> {
    if !bounded_nonempty_text(&input.primary_mime, MAX_PRIMARY_MIME_BYTES)
        || input.representations.iter().any(|representation| {
            !bounded_nonempty_text(&representation.format_id, MAX_FORMAT_ID_BYTES)
                || representation
                    .missing_ref
                    .as_deref()
                    .is_some_and(|value| !bounded_nonempty_text(value, MAX_MISSING_REFERENCE_BYTES))
                || representation
                    .bytes
                    .as_ref()
                    .is_some_and(|bytes| bytes.len() > crate::MAX_CAS_OBJECT_BYTES)
        })
    {
        return Err(StoreError::PayloadStorageUnavailable);
    }
    Ok(())
}

fn bounded_nonempty_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty() && value.len() <= max_bytes
}

fn write_ingest(
    transaction: &Transaction<'_>,
    input: &CaptureInput,
    prepared: &PreparedIngest<'_>,
    search_ocr: Option<&str>,
    source_app_original: Option<&str>,
) -> Result<IngestOutcome, StoreError> {
    let normalized_text = normalized_text(input, prepared.primary_payload, search_ocr);
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
            i64::from(prepared.content_flags.bits()),
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
        params![i64::from(prepared.content_flags.bits()), content_id],
    )?;
    let merged_content_flags = transaction.query_row(
        "SELECT flags FROM content WHERE content_id = ?1",
        [content_id],
        |row| row.get::<_, i64>(0),
    )?;

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
    let event_id = transaction.last_insert_rowid();
    for (ordinal, representation) in prepared.representations.iter().enumerate() {
        let ordinal = i64::try_from(ordinal).map_err(|_| StoreError::PayloadStorageUnavailable)?;
        match &representation.payload {
            PreparedPayload::Missing(missing_ref) => {
                transaction.execute(
                    "INSERT INTO event_representation
                       (event_id, ordinal, format_id, raw_payload_id, missing_ref)
                     VALUES (?1, ?2, ?3, NULL, ?4)",
                    params![event_id, ordinal, representation.format_id, missing_ref],
                )?;
            }
            PreparedPayload::Stored(_) => {
                let (storage_kind, inline_payload, blob_relpath, stored_byte_size) =
                    representation.storage_values();
                transaction.execute(
                    "INSERT INTO raw_payload
                       (raw_digest, storage_kind, inline_payload, blob_relpath,
                        original_byte_size, stored_byte_size)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(raw_digest, storage_kind) DO NOTHING",
                    params![
                        representation.raw_digest.as_slice(),
                        storage_kind,
                        inline_payload,
                        blob_relpath,
                        sql_count(representation.original_byte_size)?,
                        sql_count(stored_byte_size)?,
                    ],
                )?;
                let raw_payload_id = transaction.query_row(
                    "SELECT raw_payload_id FROM raw_payload
                     WHERE raw_digest = ?1 AND storage_kind = ?2",
                    params![representation.raw_digest.as_slice(), storage_kind],
                    |row| row.get::<_, i64>(0),
                )?;
                transaction.execute(
                    "INSERT INTO event_representation
                       (event_id, ordinal, format_id, raw_payload_id, missing_ref)
                     VALUES (?1, ?2, ?3, ?4, NULL)",
                    params![event_id, ordinal, representation.format_id, raw_payload_id],
                )?;
            }
        }
    }
    Ok(IngestOutcome {
        content_id,
        event_id,
    })
}

fn original_preview(
    kind: ContentKind,
    primary_payload: Option<&[u8]>,
    display_label: Option<&str>,
) -> String {
    if !kind.is_textual() {
        // Nothing readable to show, so fall back to the name the source gave
        // it. Truncated on a character boundary: the column is bounded.
        return display_label.map(bounded_preview).unwrap_or_default();
    }
    let Some(payload) = primary_payload else {
        return display_label.map(bounded_preview).unwrap_or_default();
    };
    let boundary = payload.len().min(MAX_PREVIEW_BYTES);
    match std::str::from_utf8(&payload[..boundary]) {
        Ok(value) => value.to_owned(),
        Err(error) => std::str::from_utf8(&payload[..error.valid_up_to()])
            .unwrap_or_default()
            .to_owned(),
    }
}

fn bounded_preview(value: &str) -> String {
    let mut boundary = value.len().min(MAX_PREVIEW_BYTES);
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value[..boundary].to_owned()
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
             WHERE content_id = ?1
               AND length(derivation_hash) = 32
               AND length(CAST(normalized_text AS BLOB)) <= ?3
             ORDER BY derivation_hash
             LIMIT ?2",
        )?;
        statement
            .query_map(
                params![
                    content_id,
                    MAX_SEARCH_DERIVATIONS_PER_CONTENT as i64,
                    MAX_SEARCH_DERIVATION_BYTES as i64,
                ],
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
    raw_digest: ContentHash,
    original_byte_size: u64,
    payload: PreparedPayload<'a>,
}

enum PreparedPayload<'a> {
    Stored(StoredPayload),
    Missing(&'a str),
}

impl StoredRepresentation<'_> {
    fn storage_values(&self) -> (&str, Option<&[u8]>, Option<&str>, u64) {
        match &self.payload {
            PreparedPayload::Stored(StoredPayload::Inline(bytes)) => {
                ("inline", Some(bytes), None, bytes.len() as u64)
            }
            PreparedPayload::Stored(StoredPayload::InlineZstd(bytes)) => {
                ("inline_zstd", Some(bytes), None, bytes.len() as u64)
            }
            PreparedPayload::Stored(StoredPayload::Cas {
                relpath, byte_size, ..
            }) => ("cas", None, Some(relpath), *byte_size),
            PreparedPayload::Missing(_) => ("missing", None, None, 0),
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
    let binary_entry = matches!(input.kind, ContentKind::Image | ContentKind::File);
    let needs_compressor = input.representations.iter().any(|representation| {
        (!binary_entry || is_textual_representation(&representation.format_id))
            && representation.bytes.as_ref().is_some_and(|bytes| {
                (MAX_INLINE_PAYLOAD_BYTES..=MAX_INLINE_ZSTD_PAYLOAD_BYTES).contains(&bytes.len())
            })
    });
    let mut compressor = needs_compressor.then(bounded_zstd_compressor).transpose()?;
    let mut stored = Vec::with_capacity(input.representations.len());
    for representation in &input.representations {
        if let Some(bytes) = representation.bytes.as_deref() {
            if representation.missing_ref.is_some() {
                return Err(StoreError::PayloadStorageUnavailable);
            }
            stored.push(StoredRepresentation {
                format_id: representation.format_id.as_str(),
                raw_digest: *blake3::hash(bytes).as_bytes(),
                original_byte_size: bytes.len() as u64,
                payload: PreparedPayload::Stored(classify_payload(
                    input.kind,
                    representation.format_id.as_str(),
                    bytes,
                    cas,
                    compressor.as_mut(),
                )?),
            });
            continue;
        }
        let missing_ref = representation
            .missing_ref
            .as_deref()
            .ok_or(StoreError::PayloadStorageUnavailable)?;
        stored.push(StoredRepresentation {
            format_id: representation.format_id.as_str(),
            raw_digest: [0; 32],
            original_byte_size: 0,
            payload: PreparedPayload::Missing(missing_ref),
        });
    }
    if compressor
        .as_mut()
        .is_some_and(|compressor| compressor.context_mut().sizeof() > MAX_IMPORT_ZSTD_CODEC_BYTES)
    {
        return Err(payload_compression_error(
            "zstd context exceeded its approved bound",
        ));
    }
    Ok(stored)
}

/// A textual representation is small whatever the entry holds. An image entry
/// carrying a `text/uri-list` source reference must not push a fifty-byte
/// string into the blob store just because the entry itself is binary.
fn is_textual_representation(format_id: &str) -> bool {
    format_id.starts_with("text/")
}

pub(crate) fn classify_payload(
    kind: ContentKind,
    format_id: &str,
    bytes: &[u8],
    cas: &CasStore,
    compressor: Option<&mut zstd::bulk::Compressor<'static>>,
) -> Result<StoredPayload, StoreError> {
    let binary_entry = matches!(kind, ContentKind::Image | ContentKind::File)
        && !is_textual_representation(format_id);
    if binary_entry || bytes.len() > MAX_INLINE_ZSTD_PAYLOAD_BYTES {
        let blob = cas.put(bytes)?;
        return Ok(StoredPayload::Cas {
            hash: blob.hash,
            relpath: blob.relpath,
            byte_size: blob.byte_size,
        });
    }
    if bytes.len() >= MAX_INLINE_PAYLOAD_BYTES {
        let compressor = compressor.ok_or_else(|| {
            payload_compression_error("zstd context was not admitted before compression")
        })?;
        let compressed = compressor
            .compress(bytes)
            .map_err(StoreError::PayloadCompression)?;
        return Ok(StoredPayload::InlineZstd(compressed));
    }
    Ok(StoredPayload::Inline(bytes.to_vec()))
}

fn bounded_zstd_compressor() -> Result<zstd::bulk::Compressor<'static>, StoreError> {
    bounded_zstd_compressor_with(
        || {
            clipboard_zstd_bound::compression_context_size(IMPORT_ZSTD_COMPRESSION_LEVEL)
                .map_err(|_| io::Error::other("zstd context estimate unavailable"))
        },
        || zstd::bulk::Compressor::new(IMPORT_ZSTD_COMPRESSION_LEVEL),
    )
}

fn bounded_zstd_compressor_with(
    estimate: impl FnOnce() -> io::Result<usize>,
    create: impl FnOnce() -> io::Result<zstd::bulk::Compressor<'static>>,
) -> Result<zstd::bulk::Compressor<'static>, StoreError> {
    let required = estimate().map_err(StoreError::PayloadCompression)?;
    if required > MAX_IMPORT_ZSTD_CODEC_BYTES {
        return Err(payload_compression_error(
            "zstd context estimate exceeds the approved bound",
        ));
    }
    create().map_err(StoreError::PayloadCompression)
}

fn payload_compression_error(message: &'static str) -> StoreError {
    StoreError::PayloadCompression(io::Error::other(message))
}

fn normalized_text(
    input: &CaptureInput,
    primary_payload: Option<&[u8]>,
    search_ocr: Option<&str>,
) -> Option<String> {
    if input.content_flags.contains(ContentFlags::DO_NOT_INDEX) {
        return None;
    }
    let primary = input
        .kind
        .is_textual()
        .then(|| primary_payload.and_then(bounded_utf8_payload))
        .flatten();
    match (primary, search_ocr) {
        (Some(primary), Some(ocr)) => Some(normalize_search_text_bounded(
            &bounded_search_prefix(primary, ocr),
            MAX_SEARCH_DERIVATION_BYTES,
        )),
        (Some(primary), None) => Some(normalize_search_text_bounded(
            primary,
            MAX_SEARCH_DERIVATION_BYTES,
        )),
        (None, Some(ocr)) => {
            let boundary = utf8_boundary_at_or_before(ocr, MAX_SEARCH_DERIVATION_BYTES);
            Some(normalize_search_text_bounded(
                &ocr[..boundary],
                MAX_SEARCH_DERIVATION_BYTES,
            ))
        }
        (None, None) => None,
    }
}

fn bounded_search_prefix(primary: &str, ocr: &str) -> String {
    let mut combined = String::with_capacity(MAX_SEARCH_DERIVATION_BYTES);
    push_utf8_prefix(&mut combined, primary, MAX_SEARCH_DERIVATION_BYTES);
    if combined.len() < MAX_SEARCH_DERIVATION_BYTES {
        combined.push('\n');
    }
    push_utf8_prefix(&mut combined, ocr, MAX_SEARCH_DERIVATION_BYTES);
    combined
}

fn push_utf8_prefix(output: &mut String, value: &str, max_bytes: usize) {
    let remaining = max_bytes.saturating_sub(output.len());
    let boundary = utf8_boundary_at_or_before(value, remaining);
    output.push_str(&value[..boundary]);
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
    // Every source record must land in exactly one bucket before the run
    // starts: a candidate to import, a parse failure, or a deliberate skip.
    let accounted = input
        .initial_failures
        .iter()
        .chain(&input.initial_skips)
        .try_fold(0_u64, |sum, reason| {
            if reason.count == 0 || !valid_reason_code(&reason.reason_code) {
                return None;
            }
            sum.checked_add(reason.count)
        })
        .ok_or(StoreError::InvalidImportInput)?;
    if input.candidate_records.checked_add(accounted) != Some(input.total_records) {
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
        StoreError::CanonicalizationTooComplex => "canonicalization_too_complex",
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

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::{fs, os::unix::fs::PermissionsExt};

    use clipboard_core::{
        CaptureInput, ContentFlags, ContentKind, EventFlags, RepresentationInput, SourceConfidence,
    };
    #[cfg(unix)]
    use rusqlite::Connection;

    #[cfg(unix)]
    use super::{
        BeginImportRun, CasError, ImportSourceKind, StoreError, StoreImportCandidate, WriteCommand,
        begin_import, import_batch, read_import_status,
    };
    use super::{CasStore, StoredPayload, classify_payload, normalized_text};

    #[test]
    fn ocr_only_search_uses_the_raw_utf8_prefix_before_normalization() {
        let raw_prefix = "\u{301}".repeat(super::MAX_SEARCH_DERIVATION_BYTES / 2);
        assert_eq!(raw_prefix.len(), super::MAX_SEARCH_DERIVATION_BYTES);
        let ocr = format!("{raw_prefix}must-not-escape-the-raw-prefix");
        let capture = CaptureInput {
            captured_at_ms: 1_000,
            kind: ContentKind::Image,
            primary_mime: "image/png".to_owned(),
            representations: vec![RepresentationInput {
                format_id: "image/png".to_owned(),
                bytes: Some(b"synthetic image".to_vec()),
                missing_ref: None,
            }],
            source_app_id: None,
            source_app_name: None,
            source_confidence: SourceConfidence::Unknown,
            pinned: false,
            occurrence_count: 1,
            content_flags: ContentFlags::empty(),
            event_flags: EventFlags::IMPORTED,
            display_label: None,
        };

        let normalized = normalized_text(&capture, None, Some(&ocr));

        assert_eq!(normalized.as_deref(), Some(""));
    }

    #[test]
    fn payload_classifier_preserves_the_three_storage_tiers() {
        let directory = tempfile::tempdir().unwrap();
        let cas = CasStore::new(directory.path().join("synthetic-blobs"));
        let moderately_large = (0..8_192).map(|value| value as u8).collect::<Vec<_>>();
        let mut compressor = super::bounded_zstd_compressor().unwrap();

        assert!(matches!(
            classify_payload(ContentKind::Text, "text/plain", b"small", &cas, None).unwrap(),
            StoredPayload::Inline(_)
        ));
        assert!(matches!(
            classify_payload(
                ContentKind::Text,
                "text/plain",
                &moderately_large,
                &cas,
                Some(&mut compressor)
            )
            .unwrap(),
            StoredPayload::InlineZstd(_)
        ));
        assert!(matches!(
            classify_payload(ContentKind::Image, "image/png", b"image", &cas, None).unwrap(),
            StoredPayload::Cas { .. }
        ));
    }

    #[test]
    fn a_textual_representation_of_a_binary_entry_stays_out_of_the_blob_store() {
        let directory = tempfile::tempdir().unwrap();
        let cas = CasStore::new(directory.path().join("synthetic-blobs"));

        assert!(matches!(
            classify_payload(
                ContentKind::File,
                "text/uri-list",
                b"file:///synthetic/report.pdf",
                &cas,
                None
            )
            .unwrap(),
            StoredPayload::Inline(_)
        ));
        assert!(matches!(
            classify_payload(
                ContentKind::Image,
                "text/uri-list",
                b"file:///synthetic/screenshot.png",
                &cas,
                None
            )
            .unwrap(),
            StoredPayload::Inline(_)
        ));
    }

    #[test]
    fn queued_import_batch_owns_the_operation_permit_until_the_command_drops() {
        let gate = crate::ImportOperationGate::with_capacity(1024).unwrap();
        let permit = gate.acquire_blocking().unwrap();
        let (reply, _response) = tokio::sync::oneshot::channel();
        let command = WriteCommand::ImportBatch {
            operation_permit: permit,
            run_id: uuid::Uuid::now_v7(),
            generation: 1,
            candidates: Vec::new(),
            reply,
        };
        let waiting_gate = gate.clone();
        let (acquired, observed) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let permit = waiting_gate.acquire_blocking().unwrap();
            acquired.send(permit).unwrap();
        });

        assert!(matches!(
            observed.recv_timeout(std::time::Duration::from_millis(20)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        drop(command);
        drop(
            observed
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap(),
        );
        waiter.join().unwrap();
    }

    #[test]
    fn rejected_or_shutdown_writer_commands_release_the_operation_permit() {
        let rejected_gate = crate::ImportOperationGate::with_capacity(1024).unwrap();
        let rejected_permit = rejected_gate.acquire_blocking().unwrap();
        let (rejected_sender, rejected_receiver) = tokio::sync::mpsc::channel(1);
        drop(rejected_receiver);
        let (reply, _response) = tokio::sync::oneshot::channel();
        let rejected = rejected_sender
            .blocking_send(WriteCommand::ImportBatch {
                operation_permit: rejected_permit,
                run_id: uuid::Uuid::now_v7(),
                generation: 1,
                candidates: Vec::new(),
                reply,
            })
            .map_err(|_| StoreError::WriterClosed);
        assert!(matches!(rejected, Err(StoreError::WriterClosed)));
        assert_eq!(rejected_gate.active_bytes(), 0);

        let shutdown_gate = crate::ImportOperationGate::with_capacity(1024).unwrap();
        let shutdown_permit = shutdown_gate.acquire_blocking().unwrap();
        let (shutdown_sender, shutdown_receiver) = tokio::sync::mpsc::channel(1);
        let (reply, _response) = tokio::sync::oneshot::channel();
        shutdown_sender
            .blocking_send(WriteCommand::ImportBatch {
                operation_permit: shutdown_permit,
                run_id: uuid::Uuid::now_v7(),
                generation: 1,
                candidates: Vec::new(),
                reply,
            })
            .unwrap();
        assert_eq!(shutdown_gate.active_bytes(), 1024);
        drop(shutdown_receiver);
        assert_eq!(shutdown_gate.active_bytes(), 0);
    }

    #[test]
    fn import_writer_scratch_proof_uses_the_upstream_zstd_bound() {
        let context_bytes =
            clipboard_zstd_bound::compression_context_size(super::IMPORT_ZSTD_COMPRESSION_LEVEL)
                .unwrap();
        assert_eq!(context_bytes, 1_303_568);
        assert!(context_bytes <= super::MAX_IMPORT_ZSTD_CODEC_BYTES);
        assert_eq!(
            zstd::zstd_safe::compress_bound(super::MAX_INLINE_ZSTD_PAYLOAD_BYTES),
            super::MAX_IMPORT_ZSTD_OUTPUT_BYTES_PER_REPRESENTATION,
        );
        assert_eq!(
            super::MAX_IMPORT_ZSTD_OUTPUT_BYTES,
            super::MAX_IMPORT_REPRESENTATIONS
                * super::MAX_IMPORT_ZSTD_OUTPUT_BYTES_PER_REPRESENTATION,
        );
        // The peak inequalities are compile-time assertions beside the constants.
    }

    #[test]
    fn bounded_zstd_compressor_rejects_over_budget_before_context_creation() {
        let context_created = std::cell::Cell::new(false);

        let error = match super::bounded_zstd_compressor_with(
            || Ok(super::MAX_IMPORT_ZSTD_CODEC_BYTES + 1),
            || {
                context_created.set(true);
                zstd::bulk::Compressor::new(3)
            },
        ) {
            Ok(_) => panic!("over-budget context estimate must be rejected"),
            Err(error) => error,
        };

        assert!(matches!(error, StoreError::PayloadCompression(_)));
        assert!(!context_created.get());
    }

    #[test]
    fn bounded_zstd_compressor_uses_a_compatible_level_three_frame() {
        let mut compressor = super::bounded_zstd_compressor().unwrap();
        let input = b"synthetic pinned zstd frame".repeat(128);

        let frame = compressor.compress(&input).unwrap();

        assert!(compressor.context_mut().sizeof() <= super::MAX_IMPORT_ZSTD_CODEC_BYTES);
        assert_eq!(zstd::bulk::decompress(&frame, input.len()).unwrap(), input);
    }

    #[cfg(unix)]
    #[test]
    fn nested_private_cas_error_in_import_batch_is_not_recorded_as_a_candidate_failure() {
        let directory = tempfile::tempdir().unwrap();
        let mut connection = Connection::open_in_memory().unwrap();
        crate::migrations().apply(&mut connection).unwrap();
        let run = begin_import(
            &mut connection,
            &BeginImportRun {
                run_id: uuid::Uuid::now_v7(),
                source_kind: ImportSourceKind::Raycast,
                source_fingerprint: [7; 32],
                total_records: 1,
                candidate_records: 1,
                initial_failures: Vec::new(),
                initial_skips: Vec::new(),
            },
        )
        .unwrap();
        let cas = CasStore::new(directory.path().join("synthetic-blobs"));
        cas.put(b"warmup").unwrap();
        fs::set_permissions(cas.root(), fs::Permissions::from_mode(0o755)).unwrap();
        let candidate = StoreImportCandidate {
            candidate_offset: 0,
            record_fingerprint: [8; 32],
            capture: CaptureInput {
                captured_at_ms: 1_000,
                kind: ContentKind::Image,
                primary_mime: "image/png".to_owned(),
                representations: vec![RepresentationInput {
                    format_id: "image/png".to_owned(),
                    bytes: Some(b"synthetic image".to_vec()),
                    missing_ref: None,
                }],
                source_app_id: None,
                source_app_name: None,
                source_confidence: SourceConfidence::Unknown,
                pinned: false,
                occurrence_count: 1,
                content_flags: ContentFlags::empty(),
                event_flags: EventFlags::empty(),
                display_label: None,
            },
            search_ocr: None,
            source_app_original: None,
        };

        let error = import_batch(
            &mut connection,
            &cas,
            run.run_id,
            run.generation,
            &[candidate],
        )
        .unwrap_err();

        assert!(matches!(
            error,
            StoreError::Cas(CasError::PrivateStorageUnavailable)
        ));
        let status = read_import_status(&connection, run.run_id).unwrap();
        assert_eq!(status.next_candidate_offset, 0);
        assert_eq!(status.failed_records, 0);
    }
}
