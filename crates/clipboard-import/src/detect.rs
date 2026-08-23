use std::{
    fmt, fs,
    io::Read,
    path::{Path, PathBuf},
};

use crate::{ImportError, ImportSource, MAX_IMPORT_MANIFEST_BYTES, stream_json_records};

/// Maximum number of top-level directory entries inspected while discovering an unnamed export
/// manifest. Named `clipboard.json` / `clipboard.csv` files bypass discovery entirely.
pub const MAX_MANIFEST_DISCOVERY_ENTRIES: usize = 4_096;

#[derive(Clone, Eq, PartialEq)]
pub struct DetectedExport {
    pub source: ImportSource,
    pub export_path: PathBuf,
    pub source_fingerprint: [u8; 32],
}

impl fmt::Debug for DetectedExport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DetectedExport")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

pub fn detect_export(path: impl AsRef<Path>) -> Result<DetectedExport, ImportError> {
    let path = path.as_ref();
    if path.is_dir() {
        return detect_directory(path);
    }
    detect_file(path)
}

fn detect_directory(directory: &Path) -> Result<DetectedExport, ImportError> {
    detect_directory_with_limit(directory, MAX_MANIFEST_DISCOVERY_ENTRIES)
}

fn detect_directory_with_limit(
    directory: &Path,
    max_examined_entries: usize,
) -> Result<DetectedExport, ImportError> {
    let json = directory.join("clipboard.json");
    let csv = directory.join("clipboard.csv");
    if json.is_file() {
        match detect_file(&json) {
            Ok(detected) => return Ok(detected),
            Err(json_error) if csv.is_file() => {
                if let Ok(detected) = detect_file(&csv) {
                    return Ok(detected);
                }
                return Err(json_error);
            }
            Err(error) => return Err(error),
        }
    }
    if csv.is_file() {
        return detect_file(&csv);
    }

    let entries = fs::read_dir(directory)
        .map_err(|_| ImportError::export("detection", "unreadable_export"))?
        .map(|entry| {
            entry.ok().map(|entry| {
                let is_file = entry.file_type().ok().is_some_and(|kind| kind.is_file());
                (entry.path(), is_file)
            })
        });
    match select_unnamed_manifest(entries, max_examined_entries) {
        Ok(Some(manifest)) => detect_file(&manifest),
        Ok(None) => Err(ImportError::export("detection", "manifest_not_found")),
        Err(reason) => Err(ImportError::export("detection", reason)),
    }
}

fn select_unnamed_manifest(
    entries: impl IntoIterator<Item = Option<(PathBuf, bool)>>,
    max_examined_entries: usize,
) -> Result<Option<PathBuf>, &'static str> {
    let mut examined_entries = 0_usize;
    let mut manifest = None;
    for entry in entries {
        examined_entries = examined_entries.checked_add(1).ok_or("export_too_large")?;
        if examined_entries > max_examined_entries {
            return Err("export_too_large");
        }
        let Some((candidate, true)) = entry else {
            continue;
        };
        if !matches!(
            candidate
                .extension()
                .and_then(|extension| extension.to_str()),
            Some("json" | "csv")
        ) {
            continue;
        }
        if manifest.is_some() {
            return Err("ambiguous_manifest");
        }
        manifest = Some(candidate);
    }
    Ok(manifest)
}

fn detect_file(path: &Path) -> Result<DetectedExport, ImportError> {
    let source = match path.extension().and_then(|extension| extension.to_str()) {
        Some("csv") => detect_csv_source(path)?,
        Some("json") => {
            let mut detected = None;
            stream_json_records(path, "detection", |_, bytes| {
                detected = Some(detect_json_source(bytes)?);
                Ok(false)
            })?;
            detected.ok_or_else(|| ImportError::export("detection", "invalid_document"))?
        }
        _ => return Err(ImportError::export("detection", "unsupported_format")),
    };
    Ok(DetectedExport {
        source,
        export_path: path.to_path_buf(),
        source_fingerprint: source_fingerprint(source, path)?,
    })
}

fn detect_csv_source(path: &Path) -> Result<ImportSource, ImportError> {
    let metadata =
        fs::metadata(path).map_err(|_| ImportError::export("detection", "unreadable_export"))?;
    if metadata.len() > MAX_IMPORT_MANIFEST_BYTES as u64 {
        return Err(ImportError::service("analysis_too_large"));
    }
    let file =
        fs::File::open(path).map_err(|_| ImportError::export("detection", "unreadable_export"))?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(false)
        .from_reader(file);
    let headers = reader
        .headers()
        .map_err(|_| ImportError::export("detection", "invalid_document"))?;
    if headers
        .iter()
        .any(|header| matches!(header, "copied_at" | "copiedAt"))
        && headers.iter().any(|header| header == "type")
    {
        return Ok(ImportSource::SuperCmd);
    }
    Err(ImportError::export("detection", "unknown_schema"))
}

fn source_fingerprint(source: ImportSource, path: &Path) -> Result<[u8; 32], ImportError> {
    let metadata =
        fs::metadata(path).map_err(|_| ImportError::export("detection", "unreadable_export"))?;
    if metadata.len() > MAX_IMPORT_MANIFEST_BYTES as u64 {
        return Err(ImportError::service("analysis_too_large"));
    }
    let mut file =
        fs::File::open(path).map_err(|_| ImportError::export("detection", "unreadable_export"))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(source.as_str().as_bytes());
    hasher.update(&[0]);
    let mut read_bytes = 0_usize;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| ImportError::export("detection", "unreadable_export"))?;
        if read == 0 {
            break;
        }
        read_bytes = read_bytes
            .checked_add(read)
            .ok_or_else(|| ImportError::service("analysis_too_large"))?;
        if read_bytes > MAX_IMPORT_MANIFEST_BYTES {
            return Err(ImportError::service("analysis_too_large"));
        }
        hasher.update(&buffer[..read]);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn detect_json_source(bytes: &[u8]) -> Result<ImportSource, ImportError> {
    let record = serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(bytes)
        .map_err(|_| ImportError::export("detection", "invalid_document"))?;

    if record.contains_key("createdAt") && record.contains_key("category") {
        return Ok(ImportSource::Raycast);
    }
    if record.contains_key("copied_at") || record.contains_key("copiedAt") {
        return Ok(ImportSource::SuperCmd);
    }
    Err(ImportError::export("detection", "unknown_schema"))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn detected_export_debug_omits_selected_path_and_fingerprint() {
        let fingerprint = [173_u8; 32];
        let detected = DetectedExport {
            source: ImportSource::SuperCmd,
            export_path: PathBuf::from("/sentinel/private/export-name.json"),
            source_fingerprint: fingerprint,
        };

        let rendered = format!("{detected:?}");
        assert!(!rendered.contains("sentinel"));
        assert!(!rendered.contains("export-name.json"));
        assert!(!rendered.contains(&format!("{fingerprint:?}")));
        assert!(rendered.contains("SuperCmd"));
    }

    #[test]
    fn unnamed_manifest_selection_stops_immediately_after_two_matches() {
        let examined = Cell::new(0_usize);
        let entries = [
            (PathBuf::from("first.json"), true),
            (PathBuf::from("second.csv"), true),
            (PathBuf::from("must-not-be-examined.json"), true),
        ]
        .into_iter()
        .map(|entry| {
            examined.set(examined.get() + 1);
            Some(entry)
        });

        let result = select_unnamed_manifest(entries, 32);

        assert_eq!(result.unwrap_err(), "ambiguous_manifest");
        assert_eq!(examined.get(), 2);
    }

    #[test]
    fn unnamed_manifest_discovery_has_a_path_free_entry_budget_error() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("synthetic.txt"), b"synthetic").unwrap();

        let error = detect_directory_with_limit(directory.path(), 0).unwrap_err();

        assert_eq!(error.to_string(), "detection export: export_too_large");
        assert!(
            !error
                .to_string()
                .contains(directory.path().to_string_lossy().as_ref())
        );
    }
}
