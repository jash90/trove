//! Writing the whole history out as a SuperCmd export.
//!
//! The record shape lives beside the parser that reads it, in
//! `trove-import`; what lives here is the part that needs the store — the
//! walk over every event and the payload bytes behind it.
//!
//! This is the only place the application writes a file the user chose. It
//! writes a directory rather than a file, because an export with images is
//! more than one file and hiding that in a zip would only mean the importer
//! has to learn to open zips.

use std::path::{Path, PathBuf};

use serde::Serialize;
use trove_import::{SuperCmdExportRecord, format_timestamp_ms, render_csv, render_json};
use trove_store::StoreHandle;

/// Largest single image written beside the manifest.
///
/// Matches the importer's own auxiliary cap, so nothing is written that the
/// reader would then refuse.
const MAX_EXPORTED_IMAGE_BYTES: usize = 32 * 1024 * 1024;

/// How many events one query hands back at a time.
///
/// The walk is unbounded in total but bounded in memory: a million-row history
/// must not be assembled in one allocation to be written out.
const EXPORT_PAGE_SIZE: i64 = 500;

#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSummary {
    /// Entries written to the manifest.
    pub records: u64,
    /// Entries whose image bytes were written beside it.
    pub images: u64,
    /// Entries that carry no payload, so only their metadata was written.
    pub without_payload: u64,
}

/// One event as it came out of the database, before it becomes a record.
struct ExportRow {
    event_id: i64,
    kind: String,
    captured_at_ms: i64,
    source_app_id: Option<String>,
    source_app_name: Option<String>,
    pinned: bool,
    missing_payload: bool,
}

pub fn export_supercmd(store: &StoreHandle, destination: &Path) -> Result<ExportSummary, String> {
    let images_directory = destination.join("images");
    std::fs::create_dir_all(&images_directory).map_err(|_| "export_destination_unusable")?;

    let mut summary = ExportSummary::default();
    let mut records: Vec<SuperCmdExportRecord> = Vec::new();
    let mut after_event_id = 0_i64;

    loop {
        let page = read_page(store, after_event_id)?;
        if page.is_empty() {
            break;
        }
        for row in page {
            after_event_id = row.event_id;
            records.push(export_record(store, &row, &images_directory, &mut summary)?);
        }
    }

    summary.records = records.len() as u64;
    std::fs::write(
        destination.join("clipboard.json"),
        render_json(&records).map_err(|_| "export_failed")?,
    )
    .map_err(|_| "export_write_failed")?;
    std::fs::write(destination.join("clipboard.csv"), render_csv(&records))
        .map_err(|_| "export_write_failed")?;
    Ok(summary)
}

/// Reads one page of events, oldest first so the export reads in order.
fn read_page(store: &StoreHandle, after_event_id: i64) -> Result<Vec<ExportRow>, String> {
    store
        .with_reader(|connection| {
            let mut statement = connection.prepare(
                "SELECT he.event_id, c.kind, he.captured_at_ms, he.source_app_id,
                        he.source_app_name, he.pinned, c.flags
                 FROM history_event he
                 JOIN content c ON c.content_id = he.content_id
                 WHERE he.event_id > ?1
                 ORDER BY he.event_id
                 LIMIT ?2",
            )?;
            let rows = statement.query_map((after_event_id, EXPORT_PAGE_SIZE), |row| {
                Ok(ExportRow {
                    event_id: row.get(0)?,
                    kind: row.get(1)?,
                    captured_at_ms: row.get(2)?,
                    source_app_id: row.get(3)?,
                    source_app_name: row.get(4)?,
                    pinned: row.get(5)?,
                    missing_payload: (row.get::<_, i64>(6)?
                        & i64::from(trove_core::ContentFlags::MISSING_PAYLOAD.bits()))
                        != 0,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(|_| "export_unavailable".to_owned())
}

/// Turns one row into a record, writing its image beside the manifest.
fn export_record(
    store: &StoreHandle,
    row: &ExportRow,
    images_directory: &Path,
    summary: &mut ExportSummary,
) -> Result<SuperCmdExportRecord, String> {
    let mut record = SuperCmdExportRecord {
        copied_at: format_timestamp_ms(row.captured_at_ms),
        content_type: row.kind.clone(),
        source_app: row.source_app_name.clone(),
        bundle_id: row.source_app_id.clone(),
        pinned: row.pinned,
        ..SuperCmdExportRecord::default()
    };

    if row.missing_payload {
        // Written with what is known and nothing invented. The reader accounts
        // for these as skipped rather than failed, which is what they are.
        summary.without_payload += 1;
        record.has_image = row.kind == "image";
        return Ok(record);
    }

    if row.kind == "image" {
        if let Some(name) = write_image(store, row.event_id, images_directory)? {
            record.has_image = true;
            // A relative reference: the reader refuses anything that looks like
            // a URL, and resolves this against the export root.
            record.file_url = Some(format!("images/{name}"));
            record.image_hash = Some(name);
            summary.images += 1;
        } else {
            summary.without_payload += 1;
            record.has_image = true;
        }
        return Ok(record);
    }

    record.text = read_text(store, row.event_id);
    Ok(record)
}

/// Writes one image beside the manifest and returns the name it was given.
fn write_image(
    store: &StoreHandle,
    event_id: i64,
    images_directory: &Path,
) -> Result<Option<String>, String> {
    let Some(bytes) =
        crate::commands::export_payload_bytes(store, event_id, MAX_EXPORTED_IMAGE_BYTES)
    else {
        return Ok(None);
    };
    // Named after the event, so two identical images stay two entries and the
    // name says which one it belongs to.
    let name = format!("{event_id:08}.png");
    std::fs::write(images_directory.join(&name), bytes).map_err(|_| "export_write_failed")?;
    Ok(Some(name))
}

fn read_text(store: &StoreHandle, event_id: i64) -> Option<String> {
    let bytes = crate::commands::export_payload_bytes(store, event_id, MAX_EXPORTED_IMAGE_BYTES)?;
    String::from_utf8(bytes).ok()
}

/// Refuses a destination that already holds something.
///
/// An export writes several files; dropping them into a directory that already
/// has content would mix two exports into one that reads as neither.
pub fn prepare_destination(path: &Path) -> Result<PathBuf, String> {
    let path = path.to_path_buf();
    match std::fs::read_dir(&path) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Err("export_destination_not_empty".to_owned());
            }
            Ok(path)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path),
        Err(_) => Err("export_destination_unusable".to_owned()),
    }
}
