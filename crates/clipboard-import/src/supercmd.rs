use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use clipboard_core::{
    CaptureInput, ContentFlags, ContentKind, EventFlags, RepresentationInput, SourceConfidence,
};
use serde::Deserialize;

use crate::{ImportCandidate, ImportError, ImportSource, canonical_fingerprint, json_records};

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
    let root = export_root.as_ref().canonicalize().map_err(|_| {
        ImportError::export(ImportSource::SuperCmd.as_str(), "export_root_unavailable")
    })?;
    json_records(path.as_ref(), ImportSource::SuperCmd)?
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let record: SuperCmdRecord = serde_json::from_value(value).map_err(|_| {
                ImportError::record(ImportSource::SuperCmd, index + 1, "invalid_record")
            })?;
            map_record(&root, record, index + 1)
        })
        .collect()
}

pub(crate) fn parse_supercmd_csv(path: &Path) -> Result<Vec<ImportCandidate>, ImportError> {
    let root = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()
        .map_err(|_| {
            ImportError::export(ImportSource::SuperCmd.as_str(), "export_root_unavailable")
        })?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(false)
        .from_path(path)
        .map_err(|_| ImportError::export(ImportSource::SuperCmd.as_str(), "unreadable_export"))?;

    reader
        .deserialize()
        .enumerate()
        .map(|(index, result)| {
            let record: SuperCmdRecord = result.map_err(|_| {
                ImportError::record(ImportSource::SuperCmd, index + 1, "invalid_record")
            })?;
            map_record(&root, record, index + 1)
        })
        .collect()
}

fn map_record(
    root: &Path,
    record: SuperCmdRecord,
    index: usize,
) -> Result<ImportCandidate, ImportError> {
    let captured_at_ms = DateTime::parse_from_rfc3339(&record.copied_at)
        .map(|timestamp| timestamp.with_timezone(&Utc).timestamp_millis())
        .map_err(|_| ImportError::record(ImportSource::SuperCmd, index, "invalid_timestamp"))?;
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
    let stable_reference = missing_reference(&record);
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
    let fingerprint = canonical_fingerprint(
        ImportSource::SuperCmd,
        captured_at_ms,
        kind,
        [
            record.bundle_id.as_deref(),
            record.source_app.as_deref(),
            record.file_url.as_deref().and_then(basename),
            record.image_hash.as_deref(),
        ],
        primary_text.as_deref().unwrap_or_default().as_bytes(),
    );

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
    })
}

fn content_kind(value: &str, has_image: bool, index: usize) -> Result<ContentKind, ImportError> {
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
        _ => Err(ImportError::record(
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
    root: &Path,
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
    let mut matches = BTreeSet::new();
    for directory in [root.join("images"), root.join("images-external")] {
        collect_matching_files(root, &directory, &names, &mut matches);
    }
    if matches.len() != 1 {
        return None;
    }
    let candidate = matches.into_iter().next()?;
    fs::read(candidate).ok()
}

fn is_safe_relative_reference(value: &str) -> bool {
    !value.contains("://")
        && Path::new(value)
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn collect_matching_files(
    root: &Path,
    directory: &Path,
    names: &BTreeSet<&str>,
    matches: &mut BTreeSet<PathBuf>,
) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_matching_files(root, &path, names, matches);
            continue;
        }
        if !file_type.is_file() && !file_type.is_symlink() {
            continue;
        }
        let filename = path.file_name().and_then(|name| name.to_str());
        let stem = path.file_stem().and_then(|name| name.to_str());
        if !filename.is_some_and(|name| names.contains(name))
            && !stem.is_some_and(|name| names.contains(name))
        {
            continue;
        }
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        if canonical.starts_with(root) {
            matches.insert(canonical);
        }
    }
}

fn basename(value: &str) -> Option<&str> {
    Path::new(value).file_name()?.to_str()
}

fn missing_reference(record: &SuperCmdRecord) -> String {
    let mut hasher = blake3::Hasher::new();
    for value in [
        record
            .file_url
            .as_deref()
            .and_then(basename)
            .unwrap_or_default(),
        record.image_hash.as_deref().unwrap_or_default(),
        record.copied_at.as_str(),
        record.content_type.as_str(),
    ] {
        hasher.update(value.as_bytes());
        hasher.update(&[0]);
    }
    hasher.finalize().to_hex().to_string()
}
