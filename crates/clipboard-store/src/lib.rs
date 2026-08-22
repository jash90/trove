#![forbid(unsafe_code)]

mod cas;
mod config;
mod migrations;
mod reader;
mod writer;

pub use cas::{CasBlob, CasError, CasStore};
pub use config::StoreConfig;
pub use migrations::migrations;
pub use writer::{
    BeginImportRun, IMPORT_BATCH_SIZE, ImportBatchOutcome, ImportFailureCount, ImportSourceKind,
    ImportWorkerLease, IngestOutcome, MAX_SEARCH_DERIVATION_BYTES,
    MAX_SEARCH_DERIVATIONS_PER_CONTENT, MAX_SEARCH_DOCUMENT_BYTES, ResumeImportRun, StoreError,
    StoreHandle, StoreImportCandidate, StoreImportRunState, StoreImportRunStatus, StoreStats,
    StoredPayload, WRITER_QUEUE_CAPACITY, classify_payload,
};
