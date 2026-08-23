use std::{
    borrow::Cow,
    fmt,
    io::Read,
    path::{Component, Path},
};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, DirEntry, OpenOptions},
};
use chrono::{DateTime, NaiveDateTime, Utc};
use clipboard_core::{
    CaptureInput, ContentFlags, ContentKind, EventFlags, RepresentationInput, SourceConfidence,
};
use serde::{
    Deserialize, Deserializer,
    de::{self, Visitor},
};

use crate::{
    CSV_DRAIN_BUFFER_BYTES, CSV_INPUT_BUFFER_BYTES, FramedHasher, ImportCandidate, ImportError,
    ImportParseLimits, ImportParseReport, ImportRecordFailure, ImportSource, JsonRecord,
    MAX_IMPORT_AUXILIARY_BYTES, canonical_fingerprint, record_failure,
    stream_json_records_path_with_limits,
};

/// The auxiliary image search is intentionally finite even for adversarial directory trees.
pub const MAX_AUXILIARY_TRAVERSAL_DEPTH: usize = 64;
/// Entry allowance is comfortably above normal exports while bounding directory I/O per record.
pub const MAX_AUXILIARY_TRAVERSAL_ENTRIES: usize = 32_768;
pub(crate) const MAX_AUXILIARY_TRAVERSAL_CONTROL_BYTES: usize =
    (MAX_AUXILIARY_TRAVERSAL_ENTRIES + 2) * std::mem::size_of::<(Dir, usize)>();

#[derive(Clone, Copy)]
struct TraversalLimits {
    max_depth: usize,
    max_entries: usize,
}

impl Default for TraversalLimits {
    fn default() -> Self {
        Self {
            max_depth: MAX_AUXILIARY_TRAVERSAL_DEPTH,
            max_entries: MAX_AUXILIARY_TRAVERSAL_ENTRIES,
        }
    }
}

#[derive(Deserialize)]
struct SuperCmdRecord<'a> {
    #[serde(alias = "copiedAt")]
    #[serde(borrow)]
    copied_at: Cow<'a, str>,
    #[serde(rename = "type")]
    #[serde(borrow)]
    content_type: Cow<'a, str>,
    #[serde(default, alias = "sourceApp")]
    #[serde(borrow)]
    source_app: Option<Cow<'a, str>>,
    #[serde(default, alias = "bundleId")]
    #[serde(borrow)]
    bundle_id: Option<Cow<'a, str>>,
    #[serde(default, deserialize_with = "deserialize_zero_one_bool")]
    pinned: bool,
    #[serde(default, alias = "fileUrl")]
    #[serde(borrow)]
    file_url: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    text: Option<Cow<'a, str>>,
    #[serde(default, alias = "ocrText")]
    #[serde(borrow)]
    ocr_text: Option<Cow<'a, str>>,
    #[serde(
        default,
        alias = "hasImage",
        deserialize_with = "deserialize_zero_one_bool"
    )]
    has_image: bool,
    #[serde(default, alias = "imageHash", alias = "hash")]
    #[serde(borrow)]
    image_hash: Option<Cow<'a, str>>,
}

fn deserialize_zero_one_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    struct ZeroOneBoolVisitor;

    impl Visitor<'_> for ZeroOneBoolVisitor {
        type Value = bool;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a boolean or zero/one flag")
        }

        fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
            Ok(value)
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            match value {
                0 => Ok(false),
                1 => Ok(true),
                _ => Err(E::invalid_value(de::Unexpected::Signed(value), &self)),
            }
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            match value {
                0 => Ok(false),
                1 => Ok(true),
                _ => Err(E::invalid_value(de::Unexpected::Unsigned(value), &self)),
            }
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            match value {
                "false" | "0" => Ok(false),
                "true" | "1" => Ok(true),
                _ => Err(E::invalid_value(de::Unexpected::Str(value), &self)),
            }
        }
    }

    deserializer.deserialize_any(ZeroOneBoolVisitor)
}

pub(crate) fn parse_supercmd_csv_report_with_permit(
    path: &Path,
    permit: &clipboard_store::ImportOperationPermit,
    limits: ImportParseLimits,
) -> Result<ImportParseReport, ImportError> {
    limits.ensure_csv_operation(permit)?;
    let root_path = path.parent().unwrap_or_else(|| Path::new("."));
    let root = open_export_root(root_path)?;
    let file = std::fs::File::open(path)
        .map_err(|_| ImportError::export(ImportSource::SuperCmd.as_str(), "unreadable_export"))?;
    let mut auxiliary_bytes_remaining = limits.auxiliary_bytes.min(
        limits.source_bytes.saturating_sub(
            usize::try_from(
                file.metadata()
                    .map_err(|_| {
                        ImportError::export(ImportSource::SuperCmd.as_str(), "unreadable_export")
                    })?
                    .len(),
            )
            .map_err(|_| ImportError::service("analysis_too_large"))?,
        ),
    );
    let mut columns = None;
    let mut report = ImportParseReport::with_source_limit(0, limits.source_bytes);
    stream_csv_records_from_reader_with_limits(
        file,
        limits.manifest_bytes,
        limits.header_bytes,
        limits.record_bytes,
        |record_index, framed| {
            if record_index == 0 {
                let CsvFramedRecord::Complete { bytes, ends } = framed else {
                    return Err(ImportError::service("analysis_too_large"));
                };
                columns = Some(CsvColumns::from_header(bytes, ends)?);
                return Ok(true);
            }
            report.total = record_index;
            let result = match framed {
                CsvFramedRecord::TooLarge => Err(record_failure(
                    ImportSource::SuperCmd,
                    record_index,
                    "record_too_large",
                )),
                CsvFramedRecord::InvalidFieldCount => Err(record_failure(
                    ImportSource::SuperCmd,
                    record_index,
                    "invalid_record",
                )),
                CsvFramedRecord::Complete { bytes, ends } => {
                    let columns = columns.as_ref().ok_or_else(|| {
                        ImportError::export(ImportSource::SuperCmd.as_str(), "invalid_document")
                    })?;
                    if ends.len() != columns.field_count {
                        Err(record_failure(
                            ImportSource::SuperCmd,
                            record_index,
                            "invalid_record",
                        ))
                    } else {
                        report.ensure_transient_capacity(
                            bytes
                                .len()
                                .saturating_mul(2)
                                .saturating_add(limits.auxiliary_bytes),
                        )?;
                        parse_csv_supercmd_record(bytes, ends, columns, record_index).and_then(
                            |record| {
                                map_record(
                                    &root,
                                    record,
                                    record_index,
                                    &mut auxiliary_bytes_remaining,
                                    TraversalLimits::default(),
                                )
                            },
                        )
                    }
                }
            };
            report.push(result)?;
            Ok(true)
        },
    )?;
    if columns.is_none() {
        return Err(ImportError::export(
            ImportSource::SuperCmd.as_str(),
            "invalid_document",
        ));
    }
    Ok(report)
}

pub(crate) fn parse_supercmd_report_with_permit(
    export_root: impl AsRef<Path>,
    path: &Path,
    permit: &clipboard_store::ImportOperationPermit,
    limits: ImportParseLimits,
) -> Result<ImportParseReport, ImportError> {
    parse_supercmd_report_with_permit_and_traversal(
        export_root,
        path,
        permit,
        limits,
        TraversalLimits::default(),
    )
}

fn parse_supercmd_report_with_permit_and_traversal(
    export_root: impl AsRef<Path>,
    path: &Path,
    permit: &clipboard_store::ImportOperationPermit,
    limits: ImportParseLimits,
    traversal_limits: TraversalLimits,
) -> Result<ImportParseReport, ImportError> {
    limits.ensure_json_operation(permit)?;
    let root = open_export_root(export_root.as_ref())?;
    let manifest_bytes = usize::try_from(
        std::fs::metadata(path)
            .map_err(|_| ImportError::export(ImportSource::SuperCmd.as_str(), "unreadable_export"))?
            .len(),
    )
    .map_err(|_| ImportError::service("analysis_too_large"))?;
    let mut auxiliary_bytes_remaining = limits
        .auxiliary_bytes
        .min(limits.source_bytes.saturating_sub(manifest_bytes));
    let mut report = ImportParseReport::with_source_limit(0, limits.source_bytes);
    stream_json_records_path_with_limits(
        path,
        ImportSource::SuperCmd.as_str(),
        limits.manifest_bytes,
        limits.record_bytes,
        |record, framed| {
            report.total = record;
            let result = match framed {
                JsonRecord::TooLarge => Err(record_failure(
                    ImportSource::SuperCmd,
                    record,
                    "record_too_large",
                )),
                JsonRecord::Complete(bytes) => {
                    report.ensure_transient_capacity(
                        bytes
                            .len()
                            .saturating_mul(2)
                            .saturating_add(limits.auxiliary_bytes),
                    )?;
                    serde_json::from_slice(bytes)
                        .map_err(|_| {
                            record_failure(ImportSource::SuperCmd, record, "invalid_record")
                        })
                        .and_then(|record_value| {
                            map_record(
                                &root,
                                record_value,
                                record,
                                &mut auxiliary_bytes_remaining,
                                traversal_limits,
                            )
                        })
                }
            };
            report.push(result)?;
            Ok(true)
        },
    )?;
    Ok(report)
}

pub(crate) fn detect_supercmd_csv_with_permit(
    path: &Path,
    permit: &clipboard_store::ImportOperationPermit,
    limits: ImportParseLimits,
) -> Result<bool, ImportError> {
    limits.ensure_csv_operation(permit)?;
    let file = std::fs::File::open(path)
        .map_err(|_| ImportError::export("detection", "unreadable_export"))?;
    let mut detected = false;
    stream_csv_records_from_reader_with_limits(
        file,
        limits.manifest_bytes,
        limits.header_bytes,
        limits.record_bytes,
        |record_index, framed| {
            if record_index != 0 {
                return Ok(false);
            }
            let CsvFramedRecord::Complete { bytes, ends } = framed else {
                return Err(ImportError::service("analysis_too_large"));
            };
            detected = CsvColumns::from_header(bytes, ends).is_ok();
            Ok(false)
        },
    )?;
    Ok(detected)
}

enum CsvFramedRecord<'a> {
    Complete { bytes: &'a [u8], ends: &'a [usize] },
    TooLarge,
    InvalidFieldCount,
}

fn stream_csv_records_from_reader_with_limits<R: Read>(
    mut reader: R,
    manifest_limit: usize,
    header_limit: usize,
    record_limit: usize,
    mut on_record: impl FnMut(usize, CsvFramedRecord<'_>) -> Result<bool, ImportError>,
) -> Result<(), ImportError> {
    if manifest_limit == 0 || header_limit == 0 || record_limit == 0 {
        return Err(ImportError::service("analysis_too_large"));
    }
    let mut parser = csv_core::Reader::new();
    let mut input = vec![0_u8; CSV_INPUT_BUFFER_BYTES];
    let mut output = vec![0_u8; header_limit.max(record_limit)];
    let mut ends = vec![0_usize; header_limit.saturating_add(1)];
    let mut drain_output = vec![0_u8; CSV_DRAIN_BUFFER_BYTES];
    let mut drain_ends = [0_usize; 256];
    let mut input_start = 0_usize;
    let mut input_end = 0_usize;
    let mut bytes_read = 0_usize;
    let mut eof = false;
    let mut record_index = 0_usize;

    loop {
        let current_limit = if record_index == 0 {
            header_limit
        } else {
            record_limit
        };
        let mut output_len = 0_usize;
        let mut ends_len = 0_usize;
        let mut output_at_limit = false;
        let mut too_large = false;
        let mut ends_overflow = false;
        loop {
            if input_start == input_end && !eof {
                let remaining = manifest_limit
                    .checked_add(1)
                    .and_then(|limit| limit.checked_sub(bytes_read))
                    .ok_or_else(|| ImportError::service("analysis_too_large"))?;
                let requested = input.len().min(remaining);
                let read = reader.read(&mut input[..requested]).map_err(|_| {
                    ImportError::export(ImportSource::SuperCmd.as_str(), "unreadable_export")
                })?;
                if read == 0 {
                    eof = true;
                } else {
                    bytes_read = bytes_read
                        .checked_add(read)
                        .ok_or_else(|| ImportError::service("analysis_too_large"))?;
                    if bytes_read > manifest_limit {
                        return Err(ImportError::service("analysis_too_large"));
                    }
                    input_start = 0;
                    input_end = read;
                }
            }

            let input_bytes = if eof && input_start == input_end {
                &[][..]
            } else {
                &input[input_start..input_end]
            };
            let draining_output = output_at_limit || too_large;
            let output_bytes = if draining_output {
                drain_output.as_mut_slice()
            } else {
                &mut output[output_len..current_limit]
            };
            let output_ends = if ends_overflow {
                drain_ends.as_mut_slice()
            } else {
                &mut ends[ends_len..]
            };
            let (result, consumed, written, ended) =
                parser.read_record(input_bytes, output_bytes, output_ends);
            input_start = input_start
                .checked_add(consumed)
                .ok_or_else(|| ImportError::service("analysis_too_large"))?;
            if draining_output {
                if written != 0 {
                    too_large = true;
                }
            } else {
                output_len = output_len
                    .checked_add(written)
                    .ok_or_else(|| ImportError::service("analysis_too_large"))?;
            }
            if !ends_overflow {
                ends_len = ends_len
                    .checked_add(ended)
                    .ok_or_else(|| ImportError::service("analysis_too_large"))?;
            }

            match result {
                csv_core::ReadRecordResult::InputEmpty => {
                    if eof && input_start == input_end {
                        return Err(ImportError::export(
                            ImportSource::SuperCmd.as_str(),
                            "invalid_document",
                        ));
                    }
                }
                csv_core::ReadRecordResult::OutputFull => {
                    if output_at_limit {
                        too_large = true;
                    }
                    output_at_limit = true;
                }
                csv_core::ReadRecordResult::OutputEndsFull => {
                    ends_overflow = true;
                }
                csv_core::ReadRecordResult::Record => {
                    let framed = if too_large {
                        CsvFramedRecord::TooLarge
                    } else if ends_overflow {
                        CsvFramedRecord::InvalidFieldCount
                    } else {
                        CsvFramedRecord::Complete {
                            bytes: &output[..output_len],
                            ends: &ends[..ends_len],
                        }
                    };
                    if !on_record(record_index, framed)? {
                        return Ok(());
                    }
                    record_index = record_index
                        .checked_add(1)
                        .ok_or_else(|| ImportError::service("source_too_large"))?;
                    break;
                }
                csv_core::ReadRecordResult::End => return Ok(()),
            }
        }
    }
}

#[derive(Default)]
struct CsvColumns {
    field_count: usize,
    copied_at: Option<usize>,
    content_type: Option<usize>,
    source_app: Option<usize>,
    bundle_id: Option<usize>,
    pinned: Option<usize>,
    file_url: Option<usize>,
    text: Option<usize>,
    ocr_text: Option<usize>,
    has_image: Option<usize>,
    image_hash: Option<usize>,
}

impl CsvColumns {
    fn from_header(bytes: &[u8], ends: &[usize]) -> Result<Self, ImportError> {
        let mut columns = Self {
            field_count: ends.len(),
            ..Self::default()
        };
        for index in 0..ends.len() {
            let name = std::str::from_utf8(csv_field(bytes, ends, index)).map_err(|_| {
                ImportError::export(ImportSource::SuperCmd.as_str(), "invalid_document")
            })?;
            let slot = match name {
                "copied_at" | "copiedAt" => Some(&mut columns.copied_at),
                "type" => Some(&mut columns.content_type),
                "source_app" | "sourceApp" => Some(&mut columns.source_app),
                "bundle_id" | "bundleId" => Some(&mut columns.bundle_id),
                "pinned" => Some(&mut columns.pinned),
                "file_url" | "fileUrl" => Some(&mut columns.file_url),
                "text" => Some(&mut columns.text),
                "ocr_text" | "ocrText" => Some(&mut columns.ocr_text),
                "has_image" | "hasImage" => Some(&mut columns.has_image),
                "image_hash" | "imageHash" | "hash" => Some(&mut columns.image_hash),
                _ => None,
            };
            if let Some(slot) = slot
                && slot.replace(index).is_some()
            {
                return Err(ImportError::export(
                    ImportSource::SuperCmd.as_str(),
                    "invalid_document",
                ));
            }
        }
        if columns.copied_at.is_none() || columns.content_type.is_none() {
            return Err(ImportError::export(
                ImportSource::SuperCmd.as_str(),
                "unknown_schema",
            ));
        }
        Ok(columns)
    }
}

fn csv_field<'a>(bytes: &'a [u8], ends: &[usize], index: usize) -> &'a [u8] {
    let start = index.checked_sub(1).map_or(0, |previous| ends[previous]);
    &bytes[start..ends[index]]
}

fn csv_string<'a>(
    bytes: &'a [u8],
    ends: &[usize],
    index: Option<usize>,
    record: usize,
) -> Result<Option<&'a str>, ImportRecordFailure> {
    let Some(index) = index else {
        return Ok(None);
    };
    let value = std::str::from_utf8(csv_field(bytes, ends, index))
        .map_err(|_| record_failure(ImportSource::SuperCmd, record, "invalid_record"))?;
    Ok((!value.is_empty()).then_some(value))
}

fn csv_bool(
    bytes: &[u8],
    ends: &[usize],
    index: Option<usize>,
    record: usize,
) -> Result<bool, ImportRecordFailure> {
    let Some(index) = index else {
        return Ok(false);
    };
    match std::str::from_utf8(csv_field(bytes, ends, index)) {
        Ok("") | Ok("false" | "0") => Ok(false),
        Ok("true" | "1") => Ok(true),
        _ => Err(record_failure(
            ImportSource::SuperCmd,
            record,
            "invalid_record",
        )),
    }
}

fn parse_csv_supercmd_record<'a>(
    bytes: &'a [u8],
    ends: &[usize],
    columns: &CsvColumns,
    record: usize,
) -> Result<SuperCmdRecord<'a>, ImportRecordFailure> {
    let copied_at =
        Cow::Borrowed(csv_string(bytes, ends, columns.copied_at, record)?.unwrap_or_default());
    let content_type =
        Cow::Borrowed(csv_string(bytes, ends, columns.content_type, record)?.unwrap_or_default());
    Ok(SuperCmdRecord {
        copied_at,
        content_type,
        source_app: csv_string(bytes, ends, columns.source_app, record)?.map(Cow::Borrowed),
        bundle_id: csv_string(bytes, ends, columns.bundle_id, record)?.map(Cow::Borrowed),
        pinned: csv_bool(bytes, ends, columns.pinned, record)?,
        file_url: csv_string(bytes, ends, columns.file_url, record)?.map(Cow::Borrowed),
        text: csv_string(bytes, ends, columns.text, record)?.map(Cow::Borrowed),
        ocr_text: csv_string(bytes, ends, columns.ocr_text, record)?.map(Cow::Borrowed),
        has_image: csv_bool(bytes, ends, columns.has_image, record)?,
        image_hash: csv_string(bytes, ends, columns.image_hash, record)?.map(Cow::Borrowed),
    })
}

fn open_export_root(path: &Path) -> Result<Dir, ImportError> {
    Dir::open_ambient_dir(path, ambient_authority()).map_err(|_| {
        ImportError::export(ImportSource::SuperCmd.as_str(), "export_root_unavailable")
    })
}

fn map_record(
    root: &Dir,
    record: SuperCmdRecord<'_>,
    index: usize,
    auxiliary_bytes_remaining: &mut usize,
    traversal_limits: TraversalLimits,
) -> Result<ImportCandidate, ImportRecordFailure> {
    let captured_at_ms = parse_timestamp_ms(&record.copied_at)
        .ok_or_else(|| record_failure(ImportSource::SuperCmd, index, "invalid_timestamp"))?;
    let kind = content_kind(&record.content_type, record.has_image, index)?;
    let primary_text = kind
        .is_textual()
        .then(|| record.text.as_deref().unwrap_or_default());
    let mut content_flags = ContentFlags::empty();
    if primary_text.is_some_and(|text| text.trim().is_empty()) {
        content_flags.insert(ContentFlags::DO_NOT_INDEX);
    }

    let image_bytes = matches!(kind, ContentKind::Image | ContentKind::File)
        .then(|| {
            resolve_payload(
                root,
                record.file_url.as_deref(),
                record.image_hash.as_deref(),
                auxiliary_bytes_remaining,
                traversal_limits,
            )
        })
        .flatten();
    let missing_payload =
        matches!(kind, ContentKind::Image | ContentKind::File) && image_bytes.is_none();
    if missing_payload {
        content_flags.insert(ContentFlags::MISSING_PAYLOAD);
    }
    let stable_reference = missing_reference(&record, captured_at_ms, kind, primary_text);
    let pinned = record.pinned.to_string();
    let has_image = record.has_image.to_string();
    let fingerprint = {
        let primary_fingerprint_bytes = match (&image_bytes, missing_payload) {
            (Some(bytes), _) => bytes.as_slice(),
            (None, true) => stable_reference.as_bytes(),
            (None, false) => primary_text.unwrap_or_default().as_bytes(),
        };
        canonical_fingerprint(
            ImportSource::SuperCmd,
            captured_at_ms,
            kind,
            [
                Some(record.content_type.as_ref()),
                record.source_app.as_deref(),
                record.bundle_id.as_deref(),
                Some(pinned.as_str()),
                record.file_url.as_deref(),
                Some(record.text.as_deref().unwrap_or_default()),
                record.ocr_text.as_deref(),
                Some(has_image.as_str()),
                record.image_hash.as_deref(),
            ],
            primary_fingerprint_bytes,
        )
    };
    let representations = if matches!(kind, ContentKind::Image | ContentKind::File) {
        vec![RepresentationInput {
            format_id: primary_mime(kind).to_owned(),
            bytes: image_bytes,
            missing_ref: missing_payload.then(|| format!("supercmd-missing:{stable_reference}")),
        }]
    } else {
        vec![RepresentationInput {
            format_id: primary_mime(kind).to_owned(),
            bytes: Some(
                record
                    .text
                    .map(Cow::into_owned)
                    .unwrap_or_default()
                    .into_bytes(),
            ),
            missing_ref: None,
        }]
    };

    Ok(ImportCandidate {
        source: ImportSource::SuperCmd,
        record_fingerprint: fingerprint,
        capture: CaptureInput {
            captured_at_ms,
            kind,
            primary_mime: primary_mime(kind).to_owned(),
            representations,
            source_app_id: record.bundle_id.map(Cow::into_owned),
            source_app_name: record.source_app.map(Cow::into_owned),
            source_confidence: SourceConfidence::Declared,
            pinned: record.pinned,
            occurrence_count: 1,
            content_flags,
            event_flags: EventFlags::IMPORTED,
        },
        search_ocr: record.ocr_text.map(Cow::into_owned),
        missing_payload,
        source_application_path: None,
    })
}

fn parse_timestamp_ms(value: &str) -> Option<i64> {
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(value) {
        return Some(timestamp.with_timezone(&Utc).timestamp_millis());
    }
    ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M:%S%.f"]
        .into_iter()
        .find_map(|format| NaiveDateTime::parse_from_str(value, format).ok())
        .map(|timestamp| timestamp.and_utc().timestamp_millis())
}

fn content_kind(
    value: &str,
    has_image: bool,
    index: usize,
) -> Result<ContentKind, ImportRecordFailure> {
    if has_image {
        return Ok(ContentKind::Image);
    }
    let kind = if value.eq_ignore_ascii_case("text") {
        ContentKind::Text
    } else if value.eq_ignore_ascii_case("link") || value.eq_ignore_ascii_case("url") {
        ContentKind::Link
    } else if value.eq_ignore_ascii_case("image") {
        ContentKind::Image
    } else if value.eq_ignore_ascii_case("file") {
        ContentKind::File
    } else if value.eq_ignore_ascii_case("color") {
        ContentKind::Color
    } else if value.eq_ignore_ascii_case("code") {
        ContentKind::Code
    } else if value.eq_ignore_ascii_case("html") {
        ContentKind::Html
    } else {
        return Err(record_failure(
            ImportSource::SuperCmd,
            index,
            "invalid_type",
        ));
    };
    Ok(kind)
}

fn primary_mime(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::Link => "text/uri-list",
        ContentKind::Image => "image/png",
        ContentKind::File => "application/octet-stream",
        ContentKind::Html => "text/html",
        _ => "text/plain",
    }
}

fn resolve_payload(
    root: &Dir,
    file_url: Option<&str>,
    image_hash: Option<&str>,
    auxiliary_bytes_remaining: &mut usize,
    traversal_limits: TraversalLimits,
) -> Option<Vec<u8>> {
    resolve_payload_with_hook(
        root,
        file_url,
        image_hash,
        auxiliary_bytes_remaining,
        traversal_limits,
        |_| {},
    )
}

fn resolve_payload_with_hook(
    root: &Dir,
    file_url: Option<&str>,
    image_hash: Option<&str>,
    auxiliary_bytes_remaining: &mut usize,
    traversal_limits: TraversalLimits,
    after_charge: impl FnOnce(usize),
) -> Option<Vec<u8>> {
    if file_url.is_some_and(|value| !is_safe_relative_reference(value)) {
        return None;
    }
    let names = [file_url.and_then(basename), image_hash];
    if names.iter().all(Option::is_none) {
        return None;
    }
    let AuxiliaryLookup::Unique(entry) = find_auxiliary(root, &names, traversal_limits) else {
        return None;
    };
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = entry.open_with(&options).ok()?;
    let byte_size = usize::try_from(file.metadata().ok()?.len()).ok()?;
    if byte_size > MAX_IMPORT_AUXILIARY_BYTES || byte_size > *auxiliary_bytes_remaining {
        return None;
    }
    *auxiliary_bytes_remaining = auxiliary_bytes_remaining.checked_sub(byte_size)?;
    after_charge(*auxiliary_bytes_remaining);
    let mut bytes = vec![0_u8; byte_size];
    if file.read_exact(&mut bytes).is_err() {
        *auxiliary_bytes_remaining = auxiliary_bytes_remaining.checked_add(byte_size)?;
        return None;
    }
    let mut growth_probe = [0_u8; 1];
    match file.read(&mut growth_probe) {
        Ok(0) => {}
        Ok(_) | Err(_) => {
            *auxiliary_bytes_remaining = auxiliary_bytes_remaining.checked_add(byte_size)?;
            return None;
        }
    }
    Some(bytes)
}

fn is_safe_relative_reference(value: &str) -> bool {
    !value.contains("://")
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

enum AuxiliaryLookup {
    Unique(DirEntry),
    Missing,
    Ambiguous,
    BudgetExceeded,
}

fn find_auxiliary(
    root: &Dir,
    names: &[Option<&str>; 2],
    limits: TraversalLimits,
) -> AuxiliaryLookup {
    let directory_capacity = limits
        .max_entries
        .min(MAX_AUXILIARY_TRAVERSAL_ENTRIES)
        .saturating_add(2);
    let mut directories = Vec::with_capacity(directory_capacity);
    directories.extend(
        ["images", "images-external"]
            .into_iter()
            .filter_map(|directory| root.open_dir_nofollow(directory).ok())
            .map(|directory| (directory, 0_usize)),
    );
    let mut examined_entries = 0_usize;
    let mut matched = None;
    while let Some((directory, depth)) = directories.pop() {
        let Ok(entries) = directory.read_dir(".") else {
            continue;
        };
        for entry in entries {
            examined_entries = match examined_entries.checked_add(1) {
                Some(count) if count <= limits.max_entries => count,
                _ => return AuxiliaryLookup::BudgetExceeded,
            };
            let Ok(entry) = entry else {
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let file_name = entry.file_name();
            let path = Path::new(&file_name);
            if file_type.is_dir() {
                if depth >= limits.max_depth {
                    return AuxiliaryLookup::BudgetExceeded;
                }
                if let Ok(child) = directory.open_dir_nofollow(path) {
                    if directories.len() == directory_capacity {
                        return AuxiliaryLookup::BudgetExceeded;
                    }
                    directories.push((child, depth + 1));
                }
                continue;
            }
            if !file_type.is_file() && !file_type.is_symlink() {
                continue;
            }
            let filename = path.file_name().and_then(|name| name.to_str());
            let stem = path.file_stem().and_then(|name| name.to_str());
            if !filename.is_some_and(|name| names.iter().flatten().any(|target| name == *target))
                && !stem.is_some_and(|name| names.iter().flatten().any(|target| name == *target))
            {
                continue;
            }
            if matched.is_some() {
                return AuxiliaryLookup::Ambiguous;
            }
            matched = Some(entry);
        }
    }
    matched.map_or(AuxiliaryLookup::Missing, AuxiliaryLookup::Unique)
}

fn basename(value: &str) -> Option<&str> {
    Path::new(value).file_name()?.to_str()
}

fn missing_reference(
    record: &SuperCmdRecord<'_>,
    captured_at_ms: i64,
    kind: ContentKind,
    primary_text: Option<&str>,
) -> String {
    let pinned = record.pinned.to_string();
    let has_image = record.has_image.to_string();
    let captured_at_ms = captured_at_ms.to_string();
    let mut hasher = FramedHasher::new();
    for field in [
        Some(ImportSource::SuperCmd.as_str()),
        Some(captured_at_ms.as_str()),
        Some(kind.as_str()),
        Some(record.content_type.as_ref()),
        record.source_app.as_deref(),
        record.bundle_id.as_deref(),
        Some(pinned.as_str()),
        record.file_url.as_deref(),
        Some(record.text.as_deref().unwrap_or_default()),
        record.ocr_text.as_deref(),
        Some(has_image.as_str()),
        record.image_hash.as_deref(),
        primary_text,
    ] {
        hasher.add_optional(field.map(str::as_bytes));
    }
    hex_string(hasher.finish())
}

fn hex_string(bytes: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn supercmd_content_kind_classification_is_allocation_free() {
        let mut kind = None;

        let allocations = allocation_counter::measure(|| {
            kind = Some(content_kind("TeXt", false, 1).unwrap());
        });

        assert_eq!(kind, Some(ContentKind::Text));
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
        assert_eq!(allocations.bytes_total, 0, "{allocations:?}");
    }

    fn csv_escape(value: &str) -> String {
        format!("\"{}\"", value.replace('"', "\"\""))
    }

    fn parse_with_traversal(
        root: &Path,
        path: &Path,
        traversal_limits: TraversalLimits,
    ) -> ImportParseReport {
        let gate =
            clipboard_store::ImportOperationGate::with_capacity(crate::MAX_IMPORT_OPERATION_BYTES)
                .unwrap();
        let permit = gate.acquire_blocking().unwrap();
        parse_supercmd_report_with_permit_and_traversal(
            root,
            path,
            &permit,
            ImportParseLimits::default(),
            traversal_limits,
        )
        .unwrap()
    }

    #[test]
    fn bounded_supercmd_csv_drains_one_byte_overflow_and_keeps_a_multiline_later_row() {
        let export = tempfile::tempdir().unwrap();
        let path = export.path().join("clipboard.csv");
        let record_limit = 128;
        let fixed_field_bytes =
            "2026-01-02T03:04:05Z".len() + "text".len() + "false".len() + "false".len();
        assert_eq!(fixed_field_bytes, 34);
        let prefix = "first line\nsecond, \"quoted\" ";
        let exact_text = format!(
            "{prefix}{}",
            "x".repeat(record_limit - fixed_field_bytes - prefix.len())
        );
        assert_eq!(fixed_field_bytes + exact_text.len(), record_limit);
        let oversized_text = format!("{exact_text}x");
        let document = format!(
            "copied_at,type,source_app,bundle_id,pinned,file_url,text,ocr_text,has_image\n2026-01-02T03:04:05Z,text,,,false,,{},,false\n2026-01-02T03:04:05Z,text,,,false,,{},,false\n2026-01-02T03:06:05Z,text,,,false,,{},,false\n",
            csv_escape(&exact_text),
            csv_escape(&oversized_text),
            csv_escape("later\nmultiline"),
        );
        fs::write(&path, document.as_bytes()).unwrap();
        let limits = crate::ImportParseLimits {
            manifest_bytes: document.len(),
            record_bytes: record_limit,
            header_bytes: 1024,
            source_bytes: 64 * 1024,
            source_control_bytes: 4 * 1024,
            auxiliary_bytes: 1024,
        };
        let gate = clipboard_store::ImportOperationGate::with_capacity(64 * 1024).unwrap();
        let permit = gate.acquire_blocking().unwrap();

        let report = parse_supercmd_csv_report_with_permit(&path, &permit, limits).unwrap();

        assert_eq!(report.total, 3);
        assert_eq!(report.candidates.len(), 2);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].record, 2);
        assert_eq!(report.failures[0].reason, "record_too_large");
        assert_eq!(
            report.candidates[1].capture.representations[0]
                .bytes
                .as_deref(),
            Some(b"later\nmultiline".as_slice())
        );
    }

    #[test]
    fn auxiliary_capacity_is_charged_before_allocation_and_source_growth_is_rejected() {
        use std::io::Write as _;

        let export = tempfile::tempdir().unwrap();
        fs::create_dir(export.path().join("images")).unwrap();
        let payload_path = export.path().join("images/synthetic.png");
        fs::write(&payload_path, b"owned-before").unwrap();
        let root = open_export_root(export.path()).unwrap();
        let original_size = b"owned-before".len();
        let mut remaining = 64_usize;

        let rejected = resolve_payload_with_hook(
            &root,
            Some("synthetic.png"),
            None,
            &mut remaining,
            TraversalLimits::default(),
            |charged_remaining| {
                assert_eq!(charged_remaining, 64 - original_size);
                let mut file = fs::OpenOptions::new()
                    .append(true)
                    .open(&payload_path)
                    .unwrap();
                file.write_all(b"-grew").unwrap();
            },
        );

        assert!(rejected.is_none());
        assert_eq!(remaining, 64);

        fs::write(&payload_path, b"stable-snapshot").unwrap();
        let analyzed = resolve_payload(
            &root,
            Some("synthetic.png"),
            None,
            &mut remaining,
            TraversalLimits::default(),
        )
        .unwrap();
        fs::write(&payload_path, b"changed-after-analysis").unwrap();
        assert_eq!(analyzed, b"stable-snapshot");
    }

    #[test]
    fn auxiliary_lookup_stops_after_the_second_ambiguous_match() {
        let export = tempfile::tempdir().unwrap();
        fs::create_dir(export.path().join("images")).unwrap();
        fs::write(export.path().join("images/shared.png"), b"first").unwrap();
        fs::write(export.path().join("images/shared.jpg"), b"second").unwrap();
        let root = open_export_root(export.path()).unwrap();
        let names = [Some("shared"), None];

        let result = find_auxiliary(
            &root,
            &names,
            TraversalLimits {
                max_depth: 4,
                max_entries: 2,
            },
        );

        assert!(matches!(result, AuxiliaryLookup::Ambiguous));
    }

    #[test]
    fn iterative_auxiliary_lookup_bounds_deep_trees_without_losing_record_accounting() {
        let export = tempfile::tempdir().unwrap();
        let mut directory = export.path().join("images");
        fs::create_dir(&directory).unwrap();
        for _ in 0..128 {
            directory = directory.join("d");
            fs::create_dir(&directory).unwrap();
        }
        fs::write(directory.join("target.png"), b"synthetic image").unwrap();
        let manifest = export.path().join("clipboard.json");
        fs::write(
            &manifest,
            br#"[{"copied_at":"2026-08-22T12:00:00Z","type":"image","file_url":"target.png","has_image":true}]"#,
        )
        .unwrap();

        let report = parse_with_traversal(
            export.path(),
            &manifest,
            TraversalLimits {
                max_depth: 8,
                max_entries: 1_024,
            },
        );

        assert_eq!(report.total, 1);
        assert_eq!(report.candidates.len(), 1);
        assert!(report.failures.is_empty());
        assert!(report.candidates[0].missing_payload);
    }

    #[test]
    fn auxiliary_entry_budget_exhaustion_is_a_countable_missing_payload() {
        let export = tempfile::tempdir().unwrap();
        fs::create_dir(export.path().join("images")).unwrap();
        fs::write(export.path().join("images/target.png"), b"synthetic image").unwrap();
        let manifest = export.path().join("clipboard.json");
        fs::write(
            &manifest,
            br#"[{"copied_at":"2026-08-22T12:00:00Z","type":"image","file_url":"target.png","has_image":true}]"#,
        )
        .unwrap();

        let report = parse_with_traversal(
            export.path(),
            &manifest,
            TraversalLimits {
                max_depth: 8,
                max_entries: 0,
            },
        );

        assert_eq!(report.total, 1);
        assert_eq!(report.candidates.len(), 1);
        assert!(report.failures.is_empty());
        assert!(report.candidates[0].missing_payload);
    }
}
