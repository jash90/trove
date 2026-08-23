use std::path::Path;

use chrono::{DateTime, Utc};
use clipboard_core::{
    CaptureInput, ContentFlags, ContentKind, EventFlags, RepresentationInput, SourceConfidence,
};
use serde::Deserialize;

use crate::{
    FramedHasher, ImportCandidate, ImportError, ImportParseReport, ImportRecordFailure,
    ImportSource, canonical_fingerprint, record_failure, source_metadata, stream_json_records,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RaycastRecord {
    created_at: String,
    modified_at: String,
    category: String,
    #[serde(default = "one")]
    copy_count: u32,
    application_path: Option<String>,
    #[serde(default)]
    text: String,
    text_content: Option<String>,
    file_path: Option<String>,
    image_hash: Option<String>,
    rich_text: Option<String>,
}

pub fn parse_raycast(path: impl AsRef<Path>) -> Result<Vec<ImportCandidate>, ImportError> {
    parse_raycast_report(path)?.into_strict()
}

pub fn parse_raycast_report(path: impl AsRef<Path>) -> Result<ImportParseReport, ImportError> {
    let mut report = ImportParseReport::new(0);
    stream_json_records(
        path.as_ref(),
        ImportSource::Raycast.as_str(),
        |record, bytes| {
            report.total = record;
            report.ensure_transient_capacity(bytes.len())?;
            let result = serde_json::from_slice(bytes)
                .map_err(|_| record_failure(ImportSource::Raycast, record, "invalid_record"))
                .and_then(|record_value| map_record(record_value, record));
            report.push(result)?;
            Ok(true)
        },
    )?;
    Ok(report)
}

fn map_record(record: RaycastRecord, index: usize) -> Result<ImportCandidate, ImportRecordFailure> {
    let captured_at_ms = parse_timestamp(&record.created_at, index)?;
    let modified_at_ms = parse_timestamp(&record.modified_at, index)?;
    if record.copy_count == 0 {
        return Err(record_failure(
            ImportSource::Raycast,
            index,
            "invalid_copy_count",
        ));
    }
    let kind = content_kind(&record.category, index)?;
    let (source_app_id, source_app_name) = source_metadata(record.application_path.as_deref());
    let primary_text = kind.is_textual().then(|| {
        record
            .text_content
            .clone()
            .unwrap_or_else(|| record.text.clone())
    });
    let mut content_flags = ContentFlags::empty();
    if primary_text
        .as_deref()
        .is_some_and(|text| text.trim().is_empty())
    {
        content_flags.insert(ContentFlags::DO_NOT_INDEX);
    }

    let missing_payload = matches!(kind, ContentKind::Image | ContentKind::File);
    if missing_payload {
        content_flags.insert(ContentFlags::MISSING_PAYLOAD);
    }
    let stable_reference = missing_reference(
        &record,
        captured_at_ms,
        modified_at_ms,
        kind,
        primary_text.as_deref(),
    );
    let primary_fingerprint_bytes = if missing_payload {
        stable_reference.as_bytes()
    } else {
        primary_text.as_deref().unwrap_or_default().as_bytes()
    };
    let representations = if missing_payload {
        vec![RepresentationInput {
            format_id: primary_mime(kind).to_owned(),
            bytes: None,
            missing_ref: Some(format!("raycast-missing:{stable_reference}")),
        }]
    } else {
        let text = primary_text.clone().unwrap_or_default();
        let mut representations = vec![RepresentationInput {
            format_id: primary_mime(kind).to_owned(),
            bytes: Some(text.into_bytes()),
            missing_ref: None,
        }];
        if let Some(rich_text) = record.rich_text.clone().filter(|value| !value.is_empty()) {
            representations.push(RepresentationInput {
                format_id: "text/html".to_owned(),
                bytes: Some(rich_text.into_bytes()),
                missing_ref: None,
            });
        }
        representations
    };
    let copy_count = record.copy_count.to_string();
    let modified_at_ms = modified_at_ms.to_string();
    let fingerprint = canonical_fingerprint(
        ImportSource::Raycast,
        captured_at_ms,
        kind,
        [
            Some(modified_at_ms.as_str()),
            Some(record.category.as_str()),
            Some(copy_count.as_str()),
            record.application_path.as_deref(),
            Some(record.text.as_str()),
            record.text_content.as_deref(),
            record.file_path.as_deref(),
            record.image_hash.as_deref(),
            record.rich_text.as_deref(),
        ],
        primary_fingerprint_bytes,
    );

    Ok(ImportCandidate {
        source: ImportSource::Raycast,
        record_fingerprint: fingerprint,
        capture: CaptureInput {
            captured_at_ms,
            kind,
            primary_mime: primary_mime(kind).to_owned(),
            representations,
            source_app_id,
            source_app_name,
            source_confidence: SourceConfidence::Declared,
            pinned: false,
            occurrence_count: record.copy_count,
            content_flags,
            event_flags: EventFlags::IMPORTED,
        },
        primary_text,
        search_ocr: None,
        missing_payload,
        source_application_path: record.application_path,
    })
}

fn one() -> u32 {
    1
}

fn parse_timestamp(value: &str, index: usize) -> Result<i64, ImportRecordFailure> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc).timestamp_millis())
        .map_err(|_| record_failure(ImportSource::Raycast, index, "invalid_timestamp"))
}

fn content_kind(category: &str, index: usize) -> Result<ContentKind, ImportRecordFailure> {
    match category.to_ascii_lowercase().as_str() {
        "text" => Ok(ContentKind::Text),
        "link" | "url" => Ok(ContentKind::Link),
        "image" => Ok(ContentKind::Image),
        "file" => Ok(ContentKind::File),
        "color" => Ok(ContentKind::Color),
        "code" => Ok(ContentKind::Code),
        "html" => Ok(ContentKind::Html),
        _ => Err(record_failure(
            ImportSource::Raycast,
            index,
            "invalid_category",
        )),
    }
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

fn missing_reference(
    record: &RaycastRecord,
    captured_at_ms: i64,
    modified_at_ms: i64,
    kind: ContentKind,
    primary_text: Option<&str>,
) -> String {
    let copy_count = record.copy_count.to_string();
    let captured_at_ms = captured_at_ms.to_string();
    let modified_at_ms = modified_at_ms.to_string();
    let mut hasher = FramedHasher::new();
    for field in [
        Some(ImportSource::Raycast.as_str()),
        Some(captured_at_ms.as_str()),
        Some(kind.as_str()),
        Some(modified_at_ms.as_str()),
        Some(record.category.as_str()),
        Some(copy_count.as_str()),
        record.application_path.as_deref(),
        Some(record.text.as_str()),
        record.text_content.as_deref(),
        record.file_path.as_deref(),
        record.image_hash.as_deref(),
        record.rich_text.as_deref(),
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
