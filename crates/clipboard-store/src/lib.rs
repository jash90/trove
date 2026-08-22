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
    IngestOutcome, StoreError, StoreHandle, StoreStats, StoredPayload, WRITER_QUEUE_CAPACITY,
    classify_payload,
};
