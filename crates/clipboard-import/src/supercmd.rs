use std::{
    collections::BTreeSet,
    io::Read,
    path::{Component, Path},
};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, DirEntry, OpenOptions},
};
use chrono::{DateTime, Utc};
use clipboard_core::{
    CaptureInput, ContentFlags, ContentKind, EventFlags, RepresentationInput, SourceConfidence,
};
use serde::Deserialize;

use crate::{
    FramedHasher, ImportCandidate, ImportError, ImportParseReport, ImportRecordFailure,
    ImportSource, canonical_fingerprint, json_records, record_failure,
};

#[derive(Deserialize)]
struct SuperCmdRecord {
    #[serde(alias = "copiedAt")]
    copied_at: String,
    #[serde(rename = "type")]
    content_type: String,
    #[serde(default, alias = "sourceApp")]
    source_app: Option<String>,
    #[serde(default, alias = "bundleId")]
    bundle_id: Option<String>,
    #[serde(default)]
    pinned: bool,
    #[serde(default, alias = "fileUrl")]
    file_url: Option<String>,
    #[serde(default)]
    text: String,
    #[serde(default, alias = "ocrText")]
    ocr_text: Option<String>,
    #[serde(default, alias = "hasImage")]
    has_image: bool,
    #[serde(default, alias = "imageHash", alias = "hash")]
    image_hash: Option<String>,
}

pub fn parse_supercmd(
    export_root: impl AsRef<Path>,
    path: impl AsRef<Path>,
) -> Result<Vec<ImportCandidate>, ImportError> {
    parse_supercmd_report(export_root, path)?.into_strict()
}

pub fn parse_supercmd_report(
    export_root: impl AsRef<Path>,
    path: impl AsRef<Path>,
) -> Result<ImportParseReport, ImportError> {
    let root = open_export_root(export_root.as_ref())?;
    let records = json_records(path.as_ref(), ImportSource::SuperCmd)?;
    let mut report = ImportParseReport::new(records.len());
    for (index, value) in records.into_iter().enumerate() {
        let result = serde_json::from_value(value)
            .map_err(|_| record_failure(ImportSource::SuperCmd, index + 1, "invalid_record"))
            .and_then(|record| map_record(&root, record, index + 1));
        report.push(result);
    }
    Ok(report)
}

pub(crate) fn parse_supercmd_csv_report(path: &Path) -> Result<ImportParseReport, ImportError> {
    let root_path = path.parent().unwrap_or_else(|| Path::new("."));
    let root = open_export_root(root_path)?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(false)
        .from_path(path)
        .map_err(|_| ImportError::export(ImportSource::SuperCmd.as_str(), "unreadable_export"))?;
    let mut report = ImportParseReport::new(0);
    for (index, result) in reader.deserialize().enumerate() {
        report.total += 1;
        let result = result
            .map_err(|_| record_failure(ImportSource::SuperCmd, index + 1, "invalid_record"))
            .and_then(|record| map_record(&root, record, index + 1));
        report.push(result);
    }
    Ok(report)
}

fn open_export_root(path: &Path) -> Result<Dir, ImportError> {
    Dir::open_ambient_dir(path, ambient_authority()).map_err(|_| {
        ImportError::export(ImportSource::SuperCmd.as_str(), "export_root_unavailable")
    })
}

fn map_record(
    root: &Dir,
    record: SuperCmdRecord,
    index: usize,
) -> Result<ImportCandidate, ImportRecordFailure> {
    let captured_at_ms = DateTime::parse_from_rfc3339(&record.copied_at)
        .map(|timestamp| timestamp.with_timezone(&Utc).timestamp_millis())
        .map_err(|_| record_failure(ImportSource::SuperCmd, index, "invalid_timestamp"))?;
    let kind = content_kind(&record.content_type, record.has_image, index)?;
    let primary_text = kind.is_textual().then_some(record.text.clone());
    let mut content_flags = ContentFlags::empty();
    if primary_text
        .as_deref()
        .is_some_and(|text| text.trim().is_empty())
    {
        content_flags.insert(ContentFlags::DO_NOT_INDEX);
    }

    let image_bytes = matches!(kind, ContentKind::Image | ContentKind::File)
        .then(|| {
            resolve_payload(
                root,
                record.file_url.as_deref(),
                record.image_hash.as_deref(),
            )
        })
        .flatten();
    let missing_payload =
        matches!(kind, ContentKind::Image | ContentKind::File) && image_bytes.is_none();
    if missing_payload {
        content_flags.insert(ContentFlags::MISSING_PAYLOAD);
    }
    let stable_reference =
        missing_reference(&record, captured_at_ms, kind, primary_text.as_deref());
    let pinned = record.pinned.to_string();
    let has_image = record.has_image.to_string();
    let fingerprint = {
        let primary_fingerprint_bytes = match (&image_bytes, missing_payload) {
            (Some(bytes), _) => bytes.as_slice(),
            (None, true) => stable_reference.as_bytes(),
            (None, false) => primary_text.as_deref().unwrap_or_default().as_bytes(),
        };
        canonical_fingerprint(
            ImportSource::SuperCmd,
            captured_at_ms,
            kind,
            [
                Some(record.content_type.as_str()),
                record.source_app.as_deref(),
                record.bundle_id.as_deref(),
                Some(pinned.as_str()),
                record.file_url.as_deref(),
                Some(record.text.as_str()),
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
            bytes: Some(primary_text.clone().unwrap_or_default().into_bytes()),
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
            source_app_id: record.bundle_id,
            source_app_name: record.source_app,
            source_confidence: SourceConfidence::Declared,
            pinned: record.pinned,
            occurrence_count: 1,
            content_flags,
            event_flags: EventFlags::IMPORTED,
        },
        primary_text,
        search_ocr: record.ocr_text,
        missing_payload,
        source_application_path: None,
    })
}

fn content_kind(
    value: &str,
    has_image: bool,
    index: usize,
) -> Result<ContentKind, ImportRecordFailure> {
    if has_image {
        return Ok(ContentKind::Image);
    }
    match value.to_ascii_lowercase().as_str() {
        "text" => Ok(ContentKind::Text),
        "link" | "url" => Ok(ContentKind::Link),
        "image" => Ok(ContentKind::Image),
        "file" => Ok(ContentKind::File),
        "color" => Ok(ContentKind::Color),
        "code" => Ok(ContentKind::Code),
        "html" => Ok(ContentKind::Html),
        _ => Err(record_failure(
            ImportSource::SuperCmd,
            index,
            "invalid_type",
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

fn resolve_payload(
    root: &Dir,
    file_url: Option<&str>,
    image_hash: Option<&str>,
) -> Option<Vec<u8>> {
    if file_url.is_some_and(|value| !is_safe_relative_reference(value)) {
        return None;
    }
    let names = [file_url.and_then(basename), image_hash]
        .into_iter()
        .flatten()
        .collect::<BTreeSet<_>>();
    if names.is_empty() {
        return None;
    }
    let mut matches = Vec::new();
    for directory in ["images", "images-external"] {
        if let Ok(directory) = root.open_dir_nofollow(directory) {
            collect_matching_entries(&directory, &names, &mut matches);
        }
    }
    if matches.len() != 1 {
        return None;
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = matches.pop()?.open_with(&options).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn is_safe_relative_reference(value: &str) -> bool {
    !value.contains("://")
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn collect_matching_entries(directory: &Dir, names: &BTreeSet<&str>, matches: &mut Vec<DirEntry>) {
    let Ok(entries) = directory.read_dir(".") else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let file_name = entry.file_name();
        let path = Path::new(&file_name);
        if file_type.is_dir() {
            if let Ok(child) = directory.open_dir_nofollow(path) {
                collect_matching_entries(&child, names, matches);
            }
            continue;
        }
        if !file_type.is_file() && !file_type.is_symlink() {
            continue;
        }
        let filename = path.file_name().and_then(|name| name.to_str());
        let stem = path.file_stem().and_then(|name| name.to_str());
        if filename.is_some_and(|name| names.contains(name))
            || stem.is_some_and(|name| names.contains(name))
        {
            matches.push(entry);
        }
    }
}

fn basename(value: &str) -> Option<&str> {
    Path::new(value).file_name()?.to_str()
}

fn missing_reference(
    record: &SuperCmdRecord,
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
        Some(record.content_type.as_str()),
        record.source_app.as_deref(),
        record.bundle_id.as_deref(),
        Some(pinned.as_str()),
        record.file_url.as_deref(),
        Some(record.text.as_str()),
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
