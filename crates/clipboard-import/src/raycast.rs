use std::{borrow::Cow, path::Path};

#[cfg(test)]
use std::cell::Cell;

use chrono::{DateTime, Utc};
use clipboard_core::{
    CaptureInput, ContentFlags, ContentKind, EventFlags, RepresentationInput, SourceConfidence,
};
use serde::Deserialize;

use crate::{
    FramedHasher, ImportCandidate, ImportError, ImportParseLimits, ImportParseReport,
    ImportRecordFailure, ImportSource, JsonRecord, canonical_fingerprint, record_failure,
    source_metadata, stream_json_records_path_with_limits,
};

#[cfg(test)]
thread_local! {
    static MAP_RECORD_CALLS: Cell<usize> = const { Cell::new(0) };
}

const MAX_RAYCAST_MAPPING_FIXED_BYTES: usize = 4 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RaycastRecord<'a> {
    #[serde(borrow)]
    created_at: Cow<'a, str>,
    #[serde(borrow)]
    modified_at: Cow<'a, str>,
    #[serde(borrow)]
    category: Cow<'a, str>,
    #[serde(default = "one")]
    copy_count: u32,
    #[serde(borrow)]
    application_path: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    text: Cow<'a, str>,
    #[serde(borrow)]
    text_content: Option<Cow<'a, str>>,
    #[serde(borrow)]
    file_path: Option<Cow<'a, str>>,
    #[serde(borrow)]
    image_hash: Option<Cow<'a, str>>,
    #[serde(borrow)]
    rich_text: Option<Cow<'a, str>>,
}

pub(crate) fn parse_raycast_report_with_permit(
    path: impl AsRef<Path>,
    permit: &clipboard_store::ImportOperationPermit,
    limits: ImportParseLimits,
) -> Result<ImportParseReport, ImportError> {
    limits.ensure_json_operation(permit)?;
    let mut report = ImportParseReport::with_source_limit(0, limits.source_bytes);
    stream_json_records_path_with_limits(
        path.as_ref(),
        ImportSource::Raycast.as_str(),
        limits.manifest_bytes,
        limits.record_bytes,
        |record, framed| {
            report.total = record;
            let result = match framed {
                JsonRecord::TooLarge => Err(record_failure(
                    ImportSource::Raycast,
                    record,
                    "record_too_large",
                )),
                JsonRecord::Complete(bytes) => {
                    let mapping_bytes = bytes
                        .len()
                        .checked_mul(3)
                        .and_then(|bytes| bytes.checked_add(MAX_RAYCAST_MAPPING_FIXED_BYTES))
                        .ok_or_else(|| ImportError::service("analysis_too_large"))?;
                    report.ensure_transient_capacity(mapping_bytes)?;
                    serde_json::from_slice(bytes)
                        .map_err(|_| {
                            record_failure(ImportSource::Raycast, record, "invalid_record")
                        })
                        .and_then(|record_value| map_record(record_value, record))
                }
            };
            report.push(result)?;
            Ok(true)
        },
    )?;
    Ok(report)
}

fn map_record(
    record: RaycastRecord<'_>,
    index: usize,
) -> Result<ImportCandidate, ImportRecordFailure> {
    #[cfg(test)]
    MAP_RECORD_CALLS.with(|calls| calls.set(calls.get() + 1));
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
            .as_deref()
            .unwrap_or(record.text.as_ref())
    });
    let mut content_flags = ContentFlags::empty();
    if primary_text.is_some_and(|text| text.trim().is_empty()) {
        content_flags.insert(ContentFlags::DO_NOT_INDEX);
    }

    let missing_payload = matches!(kind, ContentKind::Image | ContentKind::File);
    if missing_payload {
        content_flags.insert(ContentFlags::MISSING_PAYLOAD);
    }
    let stable_reference =
        missing_reference(&record, captured_at_ms, modified_at_ms, kind, primary_text);
    let primary_fingerprint_bytes = if missing_payload {
        stable_reference.as_bytes()
    } else {
        primary_text.unwrap_or_default().as_bytes()
    };
    let copy_count = record.copy_count.to_string();
    let modified_at_ms = modified_at_ms.to_string();
    let fingerprint = canonical_fingerprint(
        ImportSource::Raycast,
        captured_at_ms,
        kind,
        [
            Some(modified_at_ms.as_str()),
            Some(record.category.as_ref()),
            Some(copy_count.as_str()),
            record.application_path.as_deref(),
            Some(record.text.as_ref()),
            record.text_content.as_deref(),
            record.file_path.as_deref(),
            record.image_hash.as_deref(),
            record.rich_text.as_deref(),
        ],
        primary_fingerprint_bytes,
    )
    .map_err(|_| record_failure(ImportSource::Raycast, index, "canonicalization_too_complex"))?;
    let representations = if missing_payload {
        vec![RepresentationInput {
            format_id: primary_mime(kind).to_owned(),
            bytes: None,
            missing_ref: Some(format!("raycast-missing:{stable_reference}")),
        }]
    } else {
        let text = record
            .text_content
            .map(Cow::into_owned)
            .unwrap_or_else(|| record.text.into_owned());
        let mut representations = vec![RepresentationInput {
            format_id: primary_mime(kind).to_owned(),
            bytes: Some(text.into_bytes()),
            missing_ref: None,
        }];
        if let Some(rich_text) = record.rich_text.filter(|value| !value.is_empty()) {
            representations.push(RepresentationInput {
                format_id: "text/html".to_owned(),
                bytes: Some(rich_text.into_owned().into_bytes()),
                missing_ref: None,
            });
        }
        representations
    };
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
        search_ocr: None,
        missing_payload,
        source_application_path: record.application_path.map(Cow::into_owned),
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
    let kind = if category.eq_ignore_ascii_case("text") {
        ContentKind::Text
    } else if category.eq_ignore_ascii_case("link") || category.eq_ignore_ascii_case("url") {
        ContentKind::Link
    } else if category.eq_ignore_ascii_case("image") {
        ContentKind::Image
    } else if category.eq_ignore_ascii_case("file") {
        ContentKind::File
    } else if category.eq_ignore_ascii_case("color") {
        ContentKind::Color
    } else if category.eq_ignore_ascii_case("code") {
        ContentKind::Code
    } else if category.eq_ignore_ascii_case("html") {
        ContentKind::Html
    } else {
        return Err(record_failure(
            ImportSource::Raycast,
            index,
            "invalid_category",
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

fn missing_reference(
    record: &RaycastRecord<'_>,
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
        Some(record.category.as_ref()),
        Some(copy_count.as_str()),
        record.application_path.as_deref(),
        Some(record.text.as_ref()),
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

#[cfg(test)]
mod tests {
    use std::fs;

    use clipboard_store::ImportOperationGate;

    use super::{MAP_RECORD_CALLS, content_kind, parse_raycast_report_with_permit};
    use crate::ImportParseLimits;

    #[test]
    fn raycast_content_kind_classification_is_allocation_free() {
        let mut kind = None;

        let allocations = allocation_counter::measure(|| {
            kind = Some(content_kind("TeXt", 1).unwrap());
        });

        assert_eq!(kind, Some(clipboard_core::ContentKind::Text));
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
        assert_eq!(allocations.bytes_total, 0, "{allocations:?}");
    }

    #[test]
    fn bounded_raycast_json_counts_one_byte_overflow_and_keeps_the_later_record() {
        let export = tempfile::tempdir().unwrap();
        let path = export.path().join("clipboard.json");
        let exact = br#"{"createdAt":"2026-01-02T03:04:05Z","modifiedAt":"2026-01-02T03:04:05Z","category":"text","text":"nested { brace } and escaped \" quote","ignored":{"array":[1,{"x":2}]}}"#;
        let mut oversized = exact.to_vec();
        oversized.insert(oversized.len() - 1, b' ');
        let later = br#"{"createdAt":"2026-01-02T03:06:05Z","modifiedAt":"2026-01-02T03:06:05Z","category":"text","text":"later"}"#;
        let mut document = Vec::new();
        document.push(b'[');
        document.extend_from_slice(exact);
        document.push(b',');
        document.extend_from_slice(&oversized);
        document.push(b',');
        document.extend_from_slice(later);
        document.push(b']');
        fs::write(&path, &document).unwrap();
        let limits = ImportParseLimits {
            manifest_bytes: document.len(),
            record_bytes: exact.len(),
            header_bytes: 1024,
            source_bytes: 64 * 1024,
            source_control_bytes: 4 * 1024,
            auxiliary_bytes: 1024,
        };
        let gate = ImportOperationGate::with_capacity(64 * 1024).unwrap();
        let permit = gate.acquire_blocking().unwrap();

        let report = parse_raycast_report_with_permit(&path, &permit, limits).unwrap();

        assert_eq!(report.total, 3);
        assert_eq!(report.candidates.len(), 2);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].record, 2);
        assert_eq!(report.failures[0].reason, "record_too_large");
        assert_eq!(
            report.candidates[1].capture.representations[0]
                .bytes
                .as_deref(),
            Some(b"later".as_slice())
        );
    }

    #[test]
    fn fingerprint_normalization_rejects_one_pathological_record_and_keeps_the_later_record() {
        let export = tempfile::tempdir().unwrap();
        let path = export.path().join("clipboard.json");
        let pathological = format!("a{}", "\u{301}".repeat(4_097));
        let document = serde_json::to_vec(&serde_json::json!([
            {
                "createdAt": "2026-01-02T03:04:05Z",
                "modifiedAt": "2026-01-02T03:04:05Z",
                "category": "text",
                "text": pathological,
            },
            {
                "createdAt": "2026-01-02T03:05:05Z",
                "modifiedAt": "2026-01-02T03:05:05Z",
                "category": "text",
                "text": "later",
            }
        ]))
        .unwrap();
        fs::write(&path, &document).unwrap();
        let limits = ImportParseLimits {
            manifest_bytes: document.len(),
            record_bytes: document.len(),
            header_bytes: 1_024,
            source_bytes: 2 * 1024 * 1024,
            source_control_bytes: 4 * 1024,
            auxiliary_bytes: 1_024,
        };
        let gate = ImportOperationGate::with_capacity(2 * 1024 * 1024).unwrap();
        let permit = gate.acquire_blocking().unwrap();

        let report = parse_raycast_report_with_permit(&path, &permit, limits).unwrap();

        assert_eq!(report.total, 2);
        assert_eq!(report.candidates.len(), 1);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].record, 1);
        assert_eq!(report.failures[0].reason, "canonicalization_too_complex");
        assert_eq!(
            report.candidates[0].capture.representations[0]
                .bytes
                .as_deref(),
            Some(b"later".as_slice())
        );
    }

    #[test]
    fn raycast_mapping_is_rejected_before_decoded_fields_can_exceed_source_admission() {
        let export = tempfile::tempdir().unwrap();
        let path = export.path().join("clipboard.json");
        let application_path = format!("/Applications/{}.app", "A".repeat(4 * 1024));
        let record = format!(
            r#"{{"createdAt":"2026-01-02T03:04:05Z","modifiedAt":"2026-01-02T03:04:05Z","category":"text","applicationPath":"{application_path}","text":"synthetic"}}"#
        );
        let document = format!("[{record}]");
        fs::write(&path, document.as_bytes()).unwrap();
        let limits = ImportParseLimits {
            manifest_bytes: document.len(),
            record_bytes: record.len(),
            header_bytes: 1024,
            source_bytes: record.len() * 2 + 1024,
            source_control_bytes: 4 * 1024,
            auxiliary_bytes: 1024,
        };
        let gate = ImportOperationGate::with_capacity(64 * 1024).unwrap();
        let permit = gate.acquire_blocking().unwrap();
        MAP_RECORD_CALLS.with(|calls| calls.set(0));

        let error = match parse_raycast_report_with_permit(&path, &permit, limits) {
            Err(error) => error,
            Ok(_) => panic!("the unprovable record must be rejected"),
        };

        assert!(matches!(
            error,
            crate::ImportError::Service {
                reason: "analysis_too_large"
            }
        ));
        MAP_RECORD_CALLS.with(|calls| assert_eq!(calls.get(), 0));
    }
}
