#![forbid(unsafe_code)]

//! Low-level payload classification is deliberately not a public admission bypass.
//!
//! ```compile_fail
//! use clipboard_store::classify_payload;
//! ```
//!
//! ```compile_fail
//! use clipboard_store::StoredPayload;
//! ```

mod boundary;
mod cas;
mod config;
mod import_operation;
mod migrations;
mod reader;
mod retention;
mod writer;

pub use boundary::{StorageBoundaryError, StorageBoundaryLease};
pub use cas::{
    CAS_VERIFY_BUFFER_BYTES, CasBlob, CasError, CasGcSession, CasStore, GcStep, GcStepBudget,
    MAX_CAS_OBJECT_BYTES,
};
pub use config::{BLOB_DIRECTORY_NAME, DATABASE_FILENAME, StoreConfig};
pub use import_operation::{
    IMPORT_OPERATION_GATE_CONTROL_BYTES, ImportOperationError, ImportOperationGate,
    ImportOperationPermit, MAX_IMPORT_OPERATION_BYTES,
};
pub use migrations::migrations;
pub use reader::ReadOnlyStore;
pub use retention::{
    MAX_RETENTION_BATCH, MAX_RETENTION_DAYS, MIN_RETENTION_DAYS, RetentionOutcome, RetentionPolicy,
};
pub use writer::{
    BeginImportRun, IMPORT_BATCH_SIZE, ImportBatchOutcome, ImportFailureCount, ImportSourceKind,
    ImportWorkerLease, IngestOutcome, MAX_APP_SETTING_JSON_BYTES, MAX_APP_SETTING_KEY_BYTES,
    MAX_IMPORT_BATCH_BYTES, MAX_IMPORT_REPRESENTATIONS, MAX_IMPORT_WRITER_SCRATCH_BYTES,
    MAX_PREVIEW_BYTES, MAX_SEARCH_DERIVATION_BYTES, MAX_SEARCH_DERIVATIONS_PER_CONTENT,
    MAX_SEARCH_DOCUMENT_BYTES, MAX_STORE_READERS, ReclaimOutcome, ResumeImportRun, StoreError,
    StoreHandle, StoreImportCandidate, StoreImportRunState, StoreImportRunStatus, StoreStats,
    WRITER_QUEUE_CAPACITY,
};
