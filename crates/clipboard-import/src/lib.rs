#![forbid(unsafe_code)]

mod detect;
mod raycast;
mod supercmd;

use std::{fs, path::Path};

use clipboard_core::{CaptureInput, ContentKind, canonical_bytes};
use serde_json::Value;
use thiserror::Error;

pub use detect::{DetectedExport, detect_export};
pub use raycast::parse_raycast;
pub use supercmd::parse_supercmd;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportSource {
    Raycast,
    SuperCmd,
}

impl ImportSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Raycast => "raycast",
            Self::SuperCmd => "supercmd",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportCandidate {
    pub source: ImportSource,
    pub record_fingerprint: [u8; 32],
    pub capture: CaptureInput,
    pub primary_text: Option<String>,
    pub search_ocr: Option<String>,
    pub missing_payload: bool,
}

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("{source_kind} export: {reason}")]
    Export {
        source_kind: &'static str,
        reason: &'static str,
    },
    #[error("{source_kind} record {record}: {reason}")]
    Record {
        source_kind: &'static str,
        record: usize,
        reason: &'static str,
    },
}

impl ImportError {
    pub(crate) fn export(source: &'static str, reason: &'static str) -> Self {
        Self::Export {
            source_kind: source,
            reason,
        }
    }

    pub(crate) fn record(source: ImportSource, record: usize, reason: &'static str) -> Self {
        Self::Record {
            source_kind: source.as_str(),
            record,
            reason,
        }
    }
}

pub fn parse_export(path: impl AsRef<Path>) -> Result<Vec<ImportCandidate>, ImportError> {
    let detected = detect_export(path.as_ref())?;
    match detected.source {
        ImportSource::Raycast => parse_raycast(detected.export_path),
        ImportSource::SuperCmd => {
            if detected
                .export_path
                .extension()
                .and_then(|extension| extension.to_str())
                == Some("csv")
            {
                return supercmd::parse_supercmd_csv(&detected.export_path);
            }
            let root = detected
                .export_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf();
            parse_supercmd(root, detected.export_path)
        }
    }
}

pub(crate) fn json_records(path: &Path, source: ImportSource) -> Result<Vec<Value>, ImportError> {
    let bytes =
        fs::read(path).map_err(|_| ImportError::export(source.as_str(), "unreadable_export"))?;
    let document: Value = serde_json::from_slice(&bytes)
        .map_err(|_| ImportError::export(source.as_str(), "invalid_document"))?;
    document
        .as_array()
        .cloned()
        .ok_or_else(|| ImportError::export(source.as_str(), "invalid_document"))
}

pub(crate) fn canonical_fingerprint<'a>(
    source: ImportSource,
    captured_at_ms: i64,
    kind: ContentKind,
    stable_fields: impl IntoIterator<Item = Option<&'a str>>,
    primary_payload: &[u8],
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    for value in [source.as_str(), &captured_at_ms.to_string(), kind.as_str()] {
        update_fingerprint_field(&mut hasher, value.as_bytes());
    }
    for field in stable_fields {
        update_fingerprint_field(&mut hasher, field.unwrap_or_default().as_bytes());
    }
    update_fingerprint_field(&mut hasher, &canonical_bytes(kind, primary_payload));
    *hasher.finalize().as_bytes()
}

pub(crate) fn source_metadata(application_path: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(application_path) = application_path else {
        return (None, None);
    };
    let name = Path::new(application_path)
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("unknown")
        .to_owned();
    let identifier = format!(
        "raycast-app:{}",
        name.to_ascii_lowercase().replace(' ', "-")
    );
    (Some(identifier), Some(name))
}

fn update_fingerprint_field(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}
