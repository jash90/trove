use std::path::Path;

use chrono::{DateTime, Utc};
use clipboard_core::{
    CaptureInput, ContentFlags, ContentKind, EventFlags, RepresentationInput, SourceConfidence,
};
use serde::Deserialize;

use crate::{
    ImportCandidate, ImportError, ImportSource, canonical_fingerprint, json_records,
    source_metadata,
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
    json_records(path.as_ref(), ImportSource::Raycast)?
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let record: RaycastRecord = serde_json::from_value(value).map_err(|_| {
                ImportError::record(ImportSource::Raycast, index + 1, "invalid_record")
            })?;
            map_record(record, index + 1)
        })
        .collect()
}

fn map_record(record: RaycastRecord, index: usize) -> Result<ImportCandidate, ImportError> {
    let captured_at_ms = parse_timestamp(&record.created_at, index)?;
    parse_timestamp(&record.modified_at, index)?;
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
    let stable_reference = stable_reference(
        record.file_path.as_deref(),
        record.image_hash.as_deref(),
        &record.category,
    );
    let representations = if missing_payload {
        vec![RepresentationInput {
            format_id: primary_mime(kind).to_owned(),
            bytes: None,
            missing_ref: Some(format!("raycast-missing:{}", stable_reference)),
        }]
    } else {
        let text = primary_text.clone().unwrap_or_default();
        let mut representations = vec![RepresentationInput {
            format_id: primary_mime(kind).to_owned(),
            bytes: Some(text.into_bytes()),
            missing_ref: None,
        }];
        if let Some(rich_text) = record.rich_text.filter(|value| !value.is_empty()) {
            representations.push(RepresentationInput {
                format_id: "text/html".to_owned(),
                bytes: Some(rich_text.into_bytes()),
                missing_ref: None,
            });
        }
        representations
    };
    let fingerprint = canonical_fingerprint(
        ImportSource::Raycast,
        captured_at_ms,
        kind,
        [
            source_app_id.as_deref(),
            source_app_name.as_deref(),
            stable_reference.as_str().into(),
        ],
        primary_text.as_deref().unwrap_or_default().as_bytes(),
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
    })
}

fn one() -> u32 {
    1
}

fn parse_timestamp(value: &str, index: usize) -> Result<i64, ImportError> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc).timestamp_millis())
        .map_err(|_| ImportError::record(ImportSource::Raycast, index, "invalid_timestamp"))
}

fn content_kind(category: &str, index: usize) -> Result<ContentKind, ImportError> {
    match category.to_ascii_lowercase().as_str() {
        "text" => Ok(ContentKind::Text),
        "link" | "url" => Ok(ContentKind::Link),
        "image" => Ok(ContentKind::Image),
        "file" => Ok(ContentKind::File),
        "color" => Ok(ContentKind::Color),
        "code" => Ok(ContentKind::Code),
        "html" => Ok(ContentKind::Html),
        _ => Err(ImportError::record(
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

fn stable_reference(file_path: Option<&str>, image_hash: Option<&str>, category: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    for value in [
        category,
        file_path
            .and_then(|path| Path::new(path).file_name())
            .and_then(|name| name.to_str())
            .unwrap_or_default(),
        image_hash.unwrap_or_default(),
    ] {
        hasher.update(value.as_bytes());
        hasher.update(&[0]);
    }
    hasher.finalize().to_hex().to_string()
}
