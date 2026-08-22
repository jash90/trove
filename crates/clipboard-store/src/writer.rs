use std::{io, thread};

use clipboard_core::{
    CaptureInput, ContentFlags, ContentKind, SourceConfidence, content_hash, normalize_search_text,
};
use rusqlite::{Connection, TransactionBehavior, params};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::{
    CasError, CasStore, StoreConfig, migrations,
    reader::{open_reader_connection, open_writer_connection},
};

pub const WRITER_QUEUE_CAPACITY: usize = 256;
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoredPayload {
    Inline(Vec<u8>),
    InlineZstd(Vec<u8>),
    Cas {
        hash: clipboard_core::ContentHash,
        relpath: String,
        byte_size: u64,
    },
}

#[derive(Clone)]
pub struct StoreHandle {
    config: StoreConfig,
    tx: mpsc::Sender<WriteCommand>,
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

        Ok(Self { config, tx })
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
    }
}

fn ingest(
    connection: &mut Connection,
    cas: &CasStore,
    input: &CaptureInput,
) -> Result<IngestOutcome, StoreError> {
    let primary_payload = input
        .representations
        .first()
        .and_then(|representation| representation.bytes.as_deref())
        .ok_or(StoreError::PayloadStorageUnavailable)?;
    let representations = stored_representations(input, cas)?;
    let content_hash = content_hash(input.kind, &input.primary_mime, primary_payload);

    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let normalized_text = normalized_text(input, primary_payload);
    transaction.execute(
        "INSERT INTO content (content_hash, kind, primary_mime, byte_size, flags, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(content_hash) DO NOTHING",
        params![
            content_hash.as_slice(),
            input.kind.as_str(),
            input.primary_mime,
            primary_payload.len() as i64,
            i64::from(input.content_flags.bits()),
            input.captured_at_ms,
        ],
    )?;
    let content_id = transaction.query_row(
        "SELECT content_id FROM content WHERE content_hash = ?1",
        [content_hash.as_slice()],
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

    for representation in representations {
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
                representation.original_byte_size as i64,
                stored_byte_size as i64,
            ],
        )?;
    }

    if merged_content_flags & i64::from(ContentFlags::DO_NOT_INDEX.bits()) != 0 {
        transaction.execute("DELETE FROM search_doc WHERE content_id = ?1", [content_id])?;
    } else if let Some(normalized_text) = normalized_text {
        transaction.execute(
            "INSERT INTO search_doc(content_id, normalized_text) VALUES (?1, ?2)
             ON CONFLICT(content_id) DO NOTHING",
            params![content_id, normalized_text],
        )?;
    }

    transaction.execute(
        "INSERT INTO history_event
           (global_id, content_id, captured_at_ms, source_app_id, source_app_name,
            source_confidence, pinned, occurrence_count, flags)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            Uuid::now_v7().as_bytes().as_slice(),
            content_id,
            input.captured_at_ms,
            input.source_app_id,
            input.source_app_name,
            source_confidence(input.source_confidence),
            i64::from(input.pinned),
            i64::from(input.occurrence_count),
            i64::from(input.event_flags.bits()),
        ],
    )?;
    let event_id = transaction.last_insert_rowid();
    transaction.commit()?;

    Ok(IngestOutcome {
        content_id,
        event_id,
    })
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

fn normalized_text(input: &CaptureInput, primary_payload: &[u8]) -> Option<String> {
    if input.kind.is_textual() && !input.content_flags.contains(ContentFlags::DO_NOT_INDEX) {
        std::str::from_utf8(primary_payload)
            .ok()
            .map(normalize_search_text)
    } else {
        None
    }
}

fn source_confidence(value: SourceConfidence) -> &'static str {
    match value {
        SourceConfidence::Declared => "declared",
        SourceConfidence::Inferred => "inferred",
        SourceConfidence::Unknown => "unknown",
    }
}
