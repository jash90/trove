#![forbid(unsafe_code)]

mod detect;
mod raycast;
mod service;
mod supercmd;

use std::{
    collections::HashMap,
    fmt, fs,
    io::{BufReader, Read},
    mem::size_of,
    path::Path,
};

use clipboard_core::{CaptureInput, ContentKind, canonical_bytes};
use thiserror::Error;

pub use detect::{DetectedExport, MAX_MANIFEST_DISCOVERY_ENTRIES, detect_export};
pub use raycast::{parse_raycast, parse_raycast_report};
pub use service::{
    IMPORT_BATCH_SIZE, ImportAdmissionLimits, ImportAnalysis, ImportProgress, ImportRunHandle,
    ImportRunState, ImportRuntime, ImportService, ImportSummary, ImportWorkerPolicy,
    MAX_IMPORT_AUXILIARY_BYTES, MAX_IMPORT_BATCH_BYTES, MAX_IMPORT_MANIFEST_BYTES,
    MAX_IMPORT_RECORD_BYTES, MAX_PREPARED_CACHE_BYTES, MAX_PREPARED_SOURCE_BYTES,
    PREPARED_SESSION_CAPACITY, PREPARED_SESSION_TTL,
};
pub use supercmd::{
    MAX_AUXILIARY_TRAVERSAL_DEPTH, MAX_AUXILIARY_TRAVERSAL_ENTRIES, parse_supercmd,
    parse_supercmd_report,
};

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

#[derive(Clone, Eq, PartialEq)]
pub struct ImportCandidate {
    pub source: ImportSource,
    pub record_fingerprint: [u8; 32],
    pub capture: CaptureInput,
    pub primary_text: Option<String>,
    pub search_ocr: Option<String>,
    pub missing_payload: bool,
    /// Private import metadata consumed by the Task 6 pre-release schema work.
    pub source_application_path: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportRecordFailure {
    pub source: ImportSource,
    pub record: usize,
    pub reason: &'static str,
}

impl fmt::Display for ImportRecordFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} record {}: {}",
            self.source.as_str(),
            self.record,
            self.reason
        )
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ImportParseReport {
    pub total: usize,
    pub candidates: Vec<ImportCandidate>,
    pub failures: Vec<ImportRecordFailure>,
    encounter_ordinals: HashMap<[u8; 32], u64>,
    retained_bytes: usize,
}

impl ImportParseReport {
    pub(crate) fn new(total: usize) -> Self {
        Self {
            total,
            candidates: Vec::new(),
            failures: Vec::new(),
            encounter_ordinals: HashMap::new(),
            retained_bytes: size_of::<Self>(),
        }
    }

    pub(crate) fn ensure_transient_capacity(
        &self,
        transient_bytes: usize,
    ) -> Result<(), ImportError> {
        if self.retained_bytes.saturating_add(transient_bytes) > MAX_PREPARED_SOURCE_BYTES {
            return Err(ImportError::service("analysis_too_large"));
        }
        Ok(())
    }

    pub(crate) fn push(
        &mut self,
        result: Result<ImportCandidate, ImportRecordFailure>,
    ) -> Result<(), ImportError> {
        match result {
            Ok(mut candidate) => {
                let base_fingerprint = candidate.record_fingerprint;
                let additional = candidate_retained_bytes(&candidate).saturating_add(
                    usize::from(!self.encounter_ordinals.contains_key(&base_fingerprint)) * 64,
                );
                self.ensure_transient_capacity(additional)?;
                let ordinal = self.encounter_ordinals.entry(base_fingerprint).or_insert(0);
                candidate.record_fingerprint =
                    duplicate_event_fingerprint(base_fingerprint, *ordinal);
                *ordinal += 1;
                self.candidates.push(candidate);
                self.retained_bytes = self.retained_bytes.saturating_add(additional);
            }
            Err(failure) => {
                let additional = size_of::<ImportRecordFailure>();
                self.ensure_transient_capacity(additional)?;
                self.failures.push(failure);
                self.retained_bytes = self.retained_bytes.saturating_add(additional);
            }
        }
        Ok(())
    }

    pub(crate) fn into_strict(self) -> Result<Vec<ImportCandidate>, ImportError> {
        match self.failures.into_iter().next() {
            Some(failure) => Err(ImportError::from_failure(failure)),
            None => Ok(self.candidates),
        }
    }
}

fn candidate_retained_bytes(candidate: &ImportCandidate) -> usize {
    let capture = &candidate.capture;
    let mut bytes = size_of::<ImportCandidate>()
        .saturating_add(capture.primary_mime.capacity())
        .saturating_add(
            capture
                .representations
                .capacity()
                .saturating_mul(size_of::<clipboard_core::RepresentationInput>()),
        )
        .saturating_add(option_string_bytes(&capture.source_app_id))
        .saturating_add(option_string_bytes(&capture.source_app_name))
        .saturating_add(option_string_bytes(&candidate.primary_text))
        .saturating_add(option_string_bytes(&candidate.search_ocr))
        .saturating_add(option_string_bytes(&candidate.source_application_path));
    for representation in &capture.representations {
        bytes = bytes
            .saturating_add(representation.format_id.capacity())
            .saturating_add(representation.bytes.as_ref().map_or(0, Vec::capacity))
            .saturating_add(option_string_bytes(&representation.missing_ref));
    }
    bytes
}

fn option_string_bytes(value: &Option<String>) -> usize {
    value.as_ref().map_or(0, String::capacity)
}

fn duplicate_event_fingerprint(base_fingerprint: [u8; 32], ordinal: u64) -> [u8; 32] {
    let mut hasher = FramedHasher::new();
    hasher.add_optional(Some(b"clipboard-import.record-fingerprint.duplicate-v1"));
    hasher.add_optional(Some(&base_fingerprint));
    hasher.add_optional(Some(&ordinal.to_be_bytes()));
    hasher.finish()
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
    #[error("import service: {reason}")]
    Service { reason: &'static str },
}

impl ImportError {
    pub(crate) fn export(source: &'static str, reason: &'static str) -> Self {
        Self::Export {
            source_kind: source,
            reason,
        }
    }

    pub(crate) fn from_failure(failure: ImportRecordFailure) -> Self {
        Self::Record {
            source_kind: failure.source.as_str(),
            record: failure.record,
            reason: failure.reason,
        }
    }

    pub(crate) fn service(reason: &'static str) -> Self {
        Self::Service { reason }
    }
}

pub fn parse_export(path: impl AsRef<Path>) -> Result<Vec<ImportCandidate>, ImportError> {
    parse_export_report(path)?.into_strict()
}

pub fn parse_export_report(path: impl AsRef<Path>) -> Result<ImportParseReport, ImportError> {
    let detected = detect_export(path.as_ref())?;
    match detected.source {
        ImportSource::Raycast => parse_raycast_report(detected.export_path),
        ImportSource::SuperCmd => {
            if detected
                .export_path
                .extension()
                .and_then(|extension| extension.to_str())
                == Some("csv")
            {
                return supercmd::parse_supercmd_csv_report(&detected.export_path);
            }
            let root = detected
                .export_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf();
            parse_supercmd_report(root, detected.export_path)
        }
    }
}

pub(crate) fn record_failure(
    source: ImportSource,
    record: usize,
    reason: &'static str,
) -> ImportRecordFailure {
    ImportRecordFailure {
        source,
        record,
        reason,
    }
}

pub(crate) fn stream_json_records(
    path: &Path,
    source_kind: &'static str,
    mut record: impl FnMut(usize, &[u8]) -> Result<bool, ImportError>,
) -> Result<usize, ImportError> {
    let metadata =
        fs::metadata(path).map_err(|_| ImportError::export(source_kind, "unreadable_export"))?;
    if metadata.len() > MAX_IMPORT_MANIFEST_BYTES as u64 {
        return Err(ImportError::service("analysis_too_large"));
    }
    let file =
        fs::File::open(path).map_err(|_| ImportError::export(source_kind, "unreadable_export"))?;
    let mut reader = BoundedJsonReader {
        reader: BufReader::new(file),
        bytes_read: 0,
        source_kind,
    };
    if reader.next_non_whitespace()? != Some(b'[') {
        return Err(ImportError::export(source_kind, "invalid_document"));
    }
    let Some(mut first) = reader.next_non_whitespace()? else {
        return Err(ImportError::export(source_kind, "invalid_document"));
    };
    if first == b']' {
        reader.require_end()?;
        return Ok(0);
    }

    let mut total = 0_usize;
    loop {
        if first != b'{' {
            return Err(ImportError::export(source_kind, "invalid_document"));
        }
        let mut bytes = Vec::with_capacity(4 * 1024);
        bytes.push(first);
        let mut depth = 1_usize;
        let mut in_string = false;
        let mut escaped = false;
        while depth != 0 {
            let byte = reader
                .next_byte()?
                .ok_or_else(|| ImportError::export(source_kind, "invalid_document"))?;
            if bytes.len() == MAX_IMPORT_RECORD_BYTES {
                return Err(ImportError::service("record_too_large"));
            }
            bytes.push(byte);
            if in_string {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    in_string = false;
                }
                continue;
            }
            match byte {
                b'"' => in_string = true,
                b'{' | b'[' => {
                    depth = depth
                        .checked_add(1)
                        .ok_or_else(|| ImportError::service("record_too_large"))?;
                }
                b'}' | b']' => depth -= 1,
                _ => {}
            }
        }
        total = total
            .checked_add(1)
            .ok_or_else(|| ImportError::service("source_too_large"))?;
        if !record(total, &bytes)? {
            return Ok(total);
        }
        match reader.next_non_whitespace()? {
            Some(b']') => {
                reader.require_end()?;
                return Ok(total);
            }
            Some(b',') => {
                first = reader
                    .next_non_whitespace()?
                    .ok_or_else(|| ImportError::export(source_kind, "invalid_document"))?;
                if first == b']' {
                    return Err(ImportError::export(source_kind, "invalid_document"));
                }
            }
            _ => return Err(ImportError::export(source_kind, "invalid_document")),
        }
    }
}

struct BoundedJsonReader {
    reader: BufReader<fs::File>,
    bytes_read: usize,
    source_kind: &'static str,
}

impl BoundedJsonReader {
    fn next_byte(&mut self) -> Result<Option<u8>, ImportError> {
        let mut byte = [0_u8; 1];
        match self.reader.read(&mut byte) {
            Ok(0) => Ok(None),
            Ok(_) => {
                self.bytes_read = self
                    .bytes_read
                    .checked_add(1)
                    .ok_or_else(|| ImportError::service("analysis_too_large"))?;
                if self.bytes_read > MAX_IMPORT_MANIFEST_BYTES {
                    return Err(ImportError::service("analysis_too_large"));
                }
                Ok(Some(byte[0]))
            }
            Err(_) => Err(ImportError::export(self.source_kind, "unreadable_export")),
        }
    }

    fn next_non_whitespace(&mut self) -> Result<Option<u8>, ImportError> {
        loop {
            match self.next_byte()? {
                Some(byte) if byte.is_ascii_whitespace() => {}
                byte => return Ok(byte),
            }
        }
    }

    fn require_end(&mut self) -> Result<(), ImportError> {
        match self.next_non_whitespace()? {
            None => Ok(()),
            Some(_) => Err(ImportError::export(self.source_kind, "invalid_document")),
        }
    }
}

pub(crate) fn canonical_fingerprint<'a>(
    source: ImportSource,
    captured_at_ms: i64,
    kind: ContentKind,
    stable_fields: impl IntoIterator<Item = Option<&'a str>>,
    primary_payload: &[u8],
) -> [u8; 32] {
    let mut hasher = FramedHasher::new();
    hasher.add_optional(Some(source.as_str().as_bytes()));
    hasher.add_optional(Some(captured_at_ms.to_string().as_bytes()));
    hasher.add_optional(Some(kind.as_str().as_bytes()));
    for field in stable_fields {
        hasher.add_optional(field.map(str::as_bytes));
    }
    let canonical_payload = canonical_bytes(kind, primary_payload);
    hasher.add_optional(Some(&canonical_payload));
    hasher.finish()
}

pub(crate) struct FramedHasher(blake3::Hasher);

impl FramedHasher {
    pub(crate) fn new() -> Self {
        Self(blake3::Hasher::new())
    }

    pub(crate) fn add_optional(&mut self, value: Option<&[u8]>) {
        match value {
            Some(bytes) => {
                self.0.update(&[1]);
                self.0.update(&(bytes.len() as u64).to_be_bytes());
                self.0.update(bytes);
            }
            None => {
                self.0.update(&[0]);
            }
        }
    }

    pub(crate) fn finish(self) -> [u8; 32] {
        *self.0.finalize().as_bytes()
    }
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
