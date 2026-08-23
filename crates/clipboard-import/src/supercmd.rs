use std::{
    collections::BTreeSet,
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
    FramedHasher, ImportCandidate, ImportError, ImportParseReport, ImportRecordFailure,
    ImportSource, MAX_IMPORT_AUXILIARY_BYTES, MAX_IMPORT_RECORD_BYTES, MAX_PREPARED_SOURCE_BYTES,
    canonical_fingerprint, record_failure, stream_json_records,
};

/// The auxiliary image search is intentionally finite even for adversarial directory trees.
pub const MAX_AUXILIARY_TRAVERSAL_DEPTH: usize = 64;
/// Entry allowance is comfortably above normal exports while bounding directory I/O per record.
pub const MAX_AUXILIARY_TRAVERSAL_ENTRIES: usize = 32_768;

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
struct SuperCmdRecord {
    #[serde(alias = "copiedAt")]
    copied_at: String,
    #[serde(rename = "type")]
    content_type: String,
    #[serde(default, alias = "sourceApp")]
    source_app: Option<String>,
    #[serde(default, alias = "bundleId")]
    bundle_id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_zero_one_bool")]
    pinned: bool,
    #[serde(default, alias = "fileUrl")]
    file_url: Option<String>,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    text: String,
    #[serde(default, alias = "ocrText")]
    ocr_text: Option<String>,
    #[serde(
        default,
        alias = "hasImage",
        deserialize_with = "deserialize_zero_one_bool"
    )]
    has_image: bool,
    #[serde(default, alias = "imageHash", alias = "hash")]
    image_hash: Option<String>,
}

fn deserialize_nullable_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Option::unwrap_or_default)
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
    parse_supercmd_report_with_limits(export_root, path, TraversalLimits::default())
}

fn parse_supercmd_report_with_limits(
    export_root: impl AsRef<Path>,
    path: impl AsRef<Path>,
    traversal_limits: TraversalLimits,
) -> Result<ImportParseReport, ImportError> {
    let root = open_export_root(export_root.as_ref())?;
    let mut auxiliary_bytes_remaining = auxiliary_budget(path.as_ref())?;
    let mut report = ImportParseReport::new(0);
    stream_json_records(
        path.as_ref(),
        ImportSource::SuperCmd.as_str(),
        |record, bytes| {
            report.total = record;
            report.ensure_transient_capacity(
                bytes.len().saturating_add(MAX_IMPORT_AUXILIARY_BYTES),
            )?;
            let result = serde_json::from_slice(bytes)
                .map_err(|_| record_failure(ImportSource::SuperCmd, record, "invalid_record"))
                .and_then(|record_value| {
                    map_record(
                        &root,
                        record_value,
                        record,
                        &mut auxiliary_bytes_remaining,
                        traversal_limits,
                    )
                });
            report.push(result)?;
            Ok(true)
        },
    )?;
    Ok(report)
}

pub(crate) fn parse_supercmd_csv_report(path: &Path) -> Result<ImportParseReport, ImportError> {
    let root_path = path.parent().unwrap_or_else(|| Path::new("."));
    let root = open_export_root(root_path)?;
    let file = std::fs::File::open(path)
        .map_err(|_| ImportError::export(ImportSource::SuperCmd.as_str(), "unreadable_export"))?;
    let mut auxiliary_bytes_remaining = auxiliary_budget(path)?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(false)
        .from_reader(file);
    let headers = reader
        .byte_headers()
        .map_err(|_| ImportError::export(ImportSource::SuperCmd.as_str(), "invalid_document"))?
        .clone();
    let mut report = ImportParseReport::new(0);
    for (index, result) in reader.byte_records().enumerate() {
        report.total += 1;
        let result = match result {
            Ok(record) => {
                let record_bytes = record.iter().map(<[u8]>::len).sum::<usize>();
                if record_bytes > MAX_IMPORT_RECORD_BYTES {
                    Err(record_failure(
                        ImportSource::SuperCmd,
                        index + 1,
                        "record_too_large",
                    ))
                } else {
                    report.ensure_transient_capacity(
                        record_bytes.saturating_add(MAX_IMPORT_AUXILIARY_BYTES),
                    )?;
                    record
                        .deserialize(Some(&headers))
                        .map_err(|_| {
                            record_failure(ImportSource::SuperCmd, index + 1, "invalid_record")
                        })
                        .and_then(|record| {
                            map_record(
                                &root,
                                record,
                                index + 1,
                                &mut auxiliary_bytes_remaining,
                                TraversalLimits::default(),
                            )
                        })
                }
            }
            Err(_) => Err(record_failure(
                ImportSource::SuperCmd,
                index + 1,
                "invalid_record",
            )),
        };
        report.push(result)?;
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
    auxiliary_bytes_remaining: &mut usize,
    traversal_limits: TraversalLimits,
) -> Result<ImportCandidate, ImportRecordFailure> {
    let captured_at_ms = parse_timestamp_ms(&record.copied_at)
        .ok_or_else(|| record_failure(ImportSource::SuperCmd, index, "invalid_timestamp"))?;
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
    auxiliary_bytes_remaining: &mut usize,
    traversal_limits: TraversalLimits,
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
    let mut bytes = Vec::new();
    (&mut file)
        .take((MAX_IMPORT_AUXILIARY_BYTES as u64) + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_IMPORT_AUXILIARY_BYTES {
        return None;
    }
    *auxiliary_bytes_remaining = auxiliary_bytes_remaining.checked_sub(bytes.len())?;
    Some(bytes)
}

fn auxiliary_budget(path: &Path) -> Result<usize, ImportError> {
    let manifest_bytes = usize::try_from(
        std::fs::metadata(path)
            .map_err(|_| ImportError::export(ImportSource::SuperCmd.as_str(), "unreadable_export"))?
            .len(),
    )
    .map_err(|_| ImportError::service("analysis_too_large"))?;
    Ok(MAX_PREPARED_SOURCE_BYTES.saturating_sub(manifest_bytes))
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

fn find_auxiliary(root: &Dir, names: &BTreeSet<&str>, limits: TraversalLimits) -> AuxiliaryLookup {
    let mut directories = ["images", "images-external"]
        .into_iter()
        .filter_map(|directory| root.open_dir_nofollow(directory).ok())
        .map(|directory| (directory, 0_usize))
        .collect::<Vec<_>>();
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
                    directories.push((child, depth + 1));
                }
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn auxiliary_lookup_stops_after_the_second_ambiguous_match() {
        let export = tempfile::tempdir().unwrap();
        fs::create_dir(export.path().join("images")).unwrap();
        fs::write(export.path().join("images/shared.png"), b"first").unwrap();
        fs::write(export.path().join("images/shared.jpg"), b"second").unwrap();
        let root = open_export_root(export.path()).unwrap();
        let names = BTreeSet::from(["shared"]);

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

        let report = parse_supercmd_report_with_limits(
            export.path(),
            &manifest,
            TraversalLimits {
                max_depth: 8,
                max_entries: 1_024,
            },
        )
        .unwrap();

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

        let report = parse_supercmd_report_with_limits(
            export.path(),
            &manifest,
            TraversalLimits {
                max_depth: 8,
                max_entries: 0,
            },
        )
        .unwrap();

        assert_eq!(report.total, 1);
        assert_eq!(report.candidates.len(), 1);
        assert!(report.failures.is_empty());
        assert!(report.candidates[0].missing_payload);
    }
}
