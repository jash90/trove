use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

use crate::{ImportError, ImportSource, bounded_manifest_bytes};

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

    let manifests = fs::read_dir(directory)
        .map_err(|_| ImportError::export("detection", "unreadable_export"))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|candidate| candidate.is_file())
        .filter(|candidate| {
            matches!(
                candidate
                    .extension()
                    .and_then(|extension| extension.to_str()),
                Some("json" | "csv")
            )
        })
        .collect::<Vec<_>>();
    match manifests.as_slice() {
        [] => Err(ImportError::export("detection", "manifest_not_found")),
        [manifest] => detect_file(manifest),
        _ => Err(ImportError::export("detection", "ambiguous_manifest")),
    }
}

fn detect_file(path: &Path) -> Result<DetectedExport, ImportError> {
    let bytes = bounded_manifest_bytes(path, "detection")?;
    let source = match path.extension().and_then(|extension| extension.to_str()) {
        Some("csv") => detect_csv_source(&bytes)?,
        Some("json") => {
            let document = serde_json::from_slice(&bytes)
                .map_err(|_| ImportError::export("detection", "invalid_document"))?;
            detect_json_source(&document)?
        }
        _ => return Err(ImportError::export("detection", "unsupported_format")),
    };
    Ok(DetectedExport {
        source,
        export_path: path.to_path_buf(),
        source_fingerprint: source_fingerprint(source, &bytes),
    })
}

fn detect_csv_source(bytes: &[u8]) -> Result<ImportSource, ImportError> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(false)
        .from_reader(bytes);
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

fn source_fingerprint(source: ImportSource, manifest_bytes: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(source.as_str().as_bytes());
    hasher.update(&[0]);
    hasher.update(manifest_bytes);
    *hasher.finalize().as_bytes()
}

fn detect_json_source(value: &Value) -> Result<ImportSource, ImportError> {
    let record = value
        .as_array()
        .and_then(|records| records.first())
        .and_then(Value::as_object)
        .ok_or_else(|| ImportError::export("detection", "invalid_document"))?;

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
}
