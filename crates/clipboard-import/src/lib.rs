#![forbid(unsafe_code)]

mod detect;
mod raycast;
mod rayconfig;
mod service;
mod supercmd;

#[cfg(test)]
#[path = "../tests/parsers.rs"]
mod parser_test_definitions;

#[cfg(test)]
mod relocated_parser_tests {
    crate::parser_test_definitions::relocated_parser_tests!();
}

use std::{
    collections::HashMap,
    fmt, fs,
    io::{BufReader, Read},
    mem::size_of,
    path::Path,
};

use clipboard_core::{
    CaptureInput, ContentKind, CoreError, canonical_byte_len, update_canonical_bytes,
};
use clipboard_store::ImportOperationPermit;
use thiserror::Error;
use zeroize::Zeroize;

pub use rayconfig::RayconfigSecret;

const JSON_READER_BUFFER_BYTES: usize = 8 * 1024;
const MAX_JSON_DELIMITER_DEPTH: usize = 128;
pub(crate) const CSV_INPUT_BUFFER_BYTES: usize = 8 * 1024;
pub(crate) const CSV_DRAIN_BUFFER_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy)]
pub(crate) struct ImportParseLimits {
    pub(crate) manifest_bytes: usize,
    pub(crate) record_bytes: usize,
    pub(crate) header_bytes: usize,
    pub(crate) source_bytes: usize,
    pub(crate) source_control_bytes: usize,
    pub(crate) auxiliary_bytes: usize,
}

impl ImportParseLimits {
    fn ensure_json_operation(self, permit: &ImportOperationPermit) -> Result<(), ImportError> {
        let required = self
            .record_bytes
            .checked_add(JSON_READER_BUFFER_BYTES)
            .ok_or_else(|| ImportError::service("analysis_too_large"))?;
        if required > permit.reserved_bytes() {
            return Err(ImportError::service("analysis_too_large"));
        }
        Ok(())
    }

    /// Proves a rayconfig parse fits the operation budget before anything is
    /// read.
    ///
    /// The ciphertext is held whole because CBC needs the previous block and
    /// PKCS7 needs the last one; everything downstream of it streams. Note the
    /// two caps interact: the ciphertext cap bounds what comes off disk and
    /// `manifest_bytes` bounds what comes out of the decompressor, so a bomb is
    /// stopped by the second even though the first let the file in. Moving one
    /// without the other leaves a gap.
    fn ensure_rayconfig_operation(self, permit: &ImportOperationPermit) -> Result<(), ImportError> {
        let required = crate::service::MAX_RAYCONFIG_CIPHERTEXT_BYTES
            .checked_add(self.record_bytes)
            .and_then(|bytes| bytes.checked_add(JSON_READER_BUFFER_BYTES))
            .and_then(|bytes| bytes.checked_add(rayconfig::SCAN_BUFFER_BYTES))
            .and_then(|bytes| bytes.checked_add(rayconfig::INFLATE_STATE_BYTES))
            .ok_or_else(|| ImportError::service("analysis_too_large"))?;
        if required > permit.reserved_bytes() {
            return Err(ImportError::service("analysis_too_large"));
        }
        Ok(())
    }

    fn ensure_csv_operation(self, permit: &ImportOperationPermit) -> Result<(), ImportError> {
        let output_bytes = self.record_bytes.max(self.header_bytes);
        let ends_bytes = self
            .header_bytes
            .checked_add(1)
            .and_then(|count| count.checked_mul(size_of::<usize>()))
            .ok_or_else(|| ImportError::service("analysis_too_large"))?;
        let required = output_bytes
            .checked_add(ends_bytes)
            .and_then(|bytes| bytes.checked_add(CSV_INPUT_BUFFER_BYTES))
            .and_then(|bytes| bytes.checked_add(CSV_DRAIN_BUFFER_BYTES))
            .ok_or_else(|| ImportError::service("analysis_too_large"))?;
        if required > permit.reserved_bytes() {
            return Err(ImportError::service("analysis_too_large"));
        }
        Ok(())
    }
}

impl Default for ImportParseLimits {
    fn default() -> Self {
        Self {
            manifest_bytes: MAX_IMPORT_MANIFEST_BYTES,
            record_bytes: MAX_IMPORT_RECORD_BYTES,
            header_bytes: MAX_IMPORT_HEADER_BYTES,
            source_bytes: MAX_PREPARED_SOURCE_BYTES - MAX_PREPARED_SOURCE_CONTROL_BYTES,
            source_control_bytes: MAX_PREPARED_SOURCE_CONTROL_BYTES,
            auxiliary_bytes: MAX_IMPORT_AUXILIARY_BYTES,
        }
    }
}

pub use detect::MAX_MANIFEST_DISCOVERY_ENTRIES;
pub use service::{
    IMPORT_BATCH_SIZE, ImportAnalysis, ImportProgress, ImportRunHandle, ImportRunState,
    ImportService, ImportSummary, ImportWorkerPolicy, MAX_IMPORT_AUXILIARY_BYTES,
    MAX_IMPORT_BATCH_BYTES, MAX_IMPORT_HEADER_BYTES, MAX_IMPORT_MANIFEST_BYTES,
    MAX_IMPORT_OPERATION_BYTES, MAX_IMPORT_RECORD_BYTES, MAX_IMPORT_RUNTIME_BYTES,
    MAX_PREPARED_CACHE_BYTES, MAX_PREPARED_SOURCE_BYTES, MAX_PREPARED_SOURCE_CONTROL_BYTES,
    PREPARED_SESSION_CAPACITY, PREPARED_SESSION_TTL,
};
pub use supercmd::{MAX_AUXILIARY_TRAVERSAL_DEPTH, MAX_AUXILIARY_TRAVERSAL_ENTRIES};

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
pub(crate) struct ImportCandidate {
    pub source: ImportSource,
    /// One-based position in the source file, kept so a record left out later
    /// can be reported against the record it actually came from.
    pub source_record: usize,
    pub record_fingerprint: [u8; 32],
    pub capture: CaptureInput,
    pub search_ocr: Option<String>,
    pub missing_payload: bool,
    /// Private import metadata consumed by the Task 6 pre-release schema work.
    pub source_application_path: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ImportRecordFailure {
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
pub(crate) struct ImportParseReport {
    pub total: usize,
    pub candidates: Vec<ImportCandidate>,
    pub failures: Vec<ImportRecordFailure>,
    /// Records left out on purpose, with the reason. Kept apart from failures
    /// so a clean import does not report hundreds of errors.
    pub skips: Vec<ImportRecordFailure>,
    encounter_ordinals: HashMap<[u8; 32], u64>,
    source_limit: usize,
}

impl ImportParseReport {
    pub(crate) fn new(total: usize) -> Self {
        Self {
            total,
            candidates: Vec::new(),
            failures: Vec::new(),
            skips: Vec::new(),
            encounter_ordinals: HashMap::new(),
            source_limit: MAX_PREPARED_SOURCE_BYTES,
        }
    }

    pub(crate) fn with_source_limit(total: usize, source_limit: usize) -> Self {
        Self {
            source_limit,
            ..Self::new(total)
        }
    }

    pub(crate) fn ensure_transient_capacity(
        &self,
        transient_bytes: usize,
    ) -> Result<(), ImportError> {
        if self
            .retained_capacity_bytes()
            .saturating_add(transient_bytes)
            > self.source_limit
        {
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
                let candidate_slots = allocation_for_next_vec_growth(
                    self.candidates.len(),
                    self.candidates.capacity(),
                    size_of::<ImportCandidate>(),
                );
                let new_fingerprint = !self.encounter_ordinals.contains_key(&base_fingerprint);
                let ordinal_table = if new_fingerprint
                    && self.encounter_ordinals.len() == self.encounter_ordinals.capacity()
                {
                    let next_capacity = self
                        .encounter_ordinals
                        .capacity()
                        .saturating_add(1)
                        .saturating_mul(2)
                        .max(4);
                    hash_table_capacity_bytes(next_capacity)
                } else {
                    0
                };
                let additional = candidate_nested_capacity_bytes(&candidate)
                    .saturating_add(candidate_slots)
                    .saturating_add(ordinal_table);
                self.ensure_transient_capacity(additional)?;
                let ordinal = self.encounter_ordinals.entry(base_fingerprint).or_insert(0);
                candidate.record_fingerprint =
                    duplicate_event_fingerprint(base_fingerprint, *ordinal);
                *ordinal += 1;
                self.candidates.push(candidate);
            }
            Err(failure) => {
                let additional = allocation_for_next_vec_growth(
                    self.failures.len(),
                    self.failures.capacity(),
                    size_of::<ImportRecordFailure>(),
                );
                self.ensure_transient_capacity(additional)?;
                self.failures.push(failure);
            }
        }
        Ok(())
    }

    /// Records a source record the parser deliberately left out.
    pub(crate) fn push_skip(&mut self, skip: ImportRecordFailure) -> Result<(), ImportError> {
        let additional = allocation_for_next_vec_growth(
            self.skips.len(),
            self.skips.capacity(),
            size_of::<ImportRecordFailure>(),
        );
        self.ensure_transient_capacity(additional)?;
        self.skips.push(skip);
        Ok(())
    }

    fn retained_capacity_bytes(&self) -> usize {
        let mut bytes = size_of::<Self>()
            .saturating_add(
                self.candidates
                    .capacity()
                    .saturating_mul(size_of::<ImportCandidate>()),
            )
            .saturating_add(
                self.failures
                    .capacity()
                    .saturating_mul(size_of::<ImportRecordFailure>()),
            )
            .saturating_add(
                self.skips
                    .capacity()
                    .saturating_mul(size_of::<ImportRecordFailure>()),
            )
            .saturating_add(hash_table_capacity_bytes(
                self.encounter_ordinals.capacity(),
            ));
        for candidate in &self.candidates {
            bytes = bytes.saturating_add(candidate_nested_capacity_bytes(candidate));
        }
        bytes
    }

    #[cfg(test)]
    pub(crate) fn into_strict(self) -> Result<Vec<ImportCandidate>, ImportError> {
        match self.failures.into_iter().next() {
            Some(failure) => Err(ImportError::from_failure(failure)),
            None => Ok(self.candidates),
        }
    }
}

fn candidate_nested_capacity_bytes(candidate: &ImportCandidate) -> usize {
    let capture = &candidate.capture;
    let mut bytes = capture
        .primary_mime
        .capacity()
        .saturating_add(
            capture
                .representations
                .capacity()
                .saturating_mul(size_of::<clipboard_core::RepresentationInput>()),
        )
        .saturating_add(option_string_bytes(&capture.source_app_id))
        .saturating_add(option_string_bytes(&capture.source_app_name))
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

fn allocation_for_next_vec_growth(length: usize, capacity: usize, item_size: usize) -> usize {
    if length < capacity {
        return 0;
    }
    let next_capacity = if capacity == 0 {
        4
    } else {
        capacity.saturating_mul(2)
    };
    next_capacity.saturating_mul(item_size)
}

fn hash_table_capacity_bytes(capacity: usize) -> usize {
    const CONTROL_AND_ALIGNMENT_BYTES_PER_BUCKET: usize = 16;
    capacity.saturating_mul(
        size_of::<([u8; 32], u64)>().saturating_add(CONTROL_AND_ALIGNMENT_BYTES_PER_BUCKET),
    )
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

    #[cfg(test)]
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportKindCount {
    pub kind: ContentKind,
    pub event_count: u64,
    pub missing_payload_count: u64,
}

pub(crate) const MAX_IMPORT_ANALYSIS_CONTROL_BYTES: usize = 7 * size_of::<ImportKindCount>();

const IMPORT_KIND_ORDER: [ContentKind; 7] = [
    ContentKind::Code,
    ContentKind::Color,
    ContentKind::File,
    ContentKind::Html,
    ContentKind::Image,
    ContentKind::Link,
    ContentKind::Text,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportExportAnalysis {
    pub source: ImportSource,
    pub total: u64,
    pub candidate_records: u64,
    pub failed: u64,
    pub counts_by_kind: Vec<ImportKindCount>,
    pub available_image_records: u64,
    pub missing_image_records: u64,
}

/// Performs a bounded analysis and returns only aggregate, non-retained data.
///
/// Retained parser reports are deliberately not part of the public API:
///
/// ```compile_fail
/// use clipboard_import::parse_export_report;
/// ```
pub fn analyze_export(path: impl AsRef<Path>) -> Result<ImportExportAnalysis, ImportError> {
    analyze_export_with_password(path, None)
}

/// Whether reading this export needs a password, without reading its records.
///
/// Detection alone answers it, so a caller can ask for a password before doing
/// any work — and can avoid asking at all when the export is a plain manifest.
pub fn export_requires_password(path: impl AsRef<Path>) -> Result<bool, ImportError> {
    let runtime = service::ImportRuntime::process_wide();
    let permit = runtime.acquire_operation_blocking()?;
    let limits = ImportParseLimits::default();
    Ok(detect::detect_export_with_permit(path.as_ref(), &permit, limits)?.encrypted)
}

/// Analyses an export, decrypting it first when it is a `.rayconfig`.
///
/// The secret is used here and nowhere else: the analysis keeps parsed records,
/// not a way back to the file, so nothing later needs the password again.
pub fn analyze_export_with_password(
    path: impl AsRef<Path>,
    secret: Option<&RayconfigSecret>,
) -> Result<ImportExportAnalysis, ImportError> {
    let runtime = service::ImportRuntime::process_wide();
    let permit = runtime.acquire_operation_blocking()?;
    let _reservation = runtime.reserve_preparation()?;
    let limits = ImportParseLimits::default();
    let detected = detect::detect_export_with_permit(path.as_ref(), &permit, limits)?;
    let report = parse_detected_export_report_with_permit(&detected, secret, &permit, limits)?;

    let total =
        u64::try_from(report.total).map_err(|_| ImportError::service("source_too_large"))?;
    let candidate_records = u64::try_from(report.candidates.len())
        .map_err(|_| ImportError::service("source_too_large"))?;
    let failed = u64::try_from(report.failures.len())
        .map_err(|_| ImportError::service("source_too_large"))?;
    let mut available_image_records = 0_u64;
    let mut missing_image_records = 0_u64;
    for candidate in &report.candidates {
        if candidate.capture.kind == ContentKind::Image {
            if candidate.missing_payload {
                missing_image_records = missing_image_records
                    .checked_add(1)
                    .ok_or_else(|| ImportError::service("source_too_large"))?;
            } else {
                available_image_records = available_image_records
                    .checked_add(1)
                    .ok_or_else(|| ImportError::service("source_too_large"))?;
            }
        }
    }
    let counts_by_kind =
        aggregate_kind_counts_with_hook(&report, limits.source_control_bytes, |_, _| {})?;

    Ok(ImportExportAnalysis {
        source: detected.source,
        total,
        candidate_records,
        failed,
        counts_by_kind,
        available_image_records,
        missing_image_records,
    })
}

fn aggregate_kind_counts_with_hook(
    report: &ImportParseReport,
    source_control_bytes: usize,
    before_allocation: impl FnOnce(usize, usize),
) -> Result<Vec<ImportKindCount>, ImportError> {
    let mut counts = [(0_u64, 0_u64); IMPORT_KIND_ORDER.len()];
    for candidate in &report.candidates {
        let index = IMPORT_KIND_ORDER
            .iter()
            .position(|kind| *kind == candidate.capture.kind)
            .ok_or_else(|| ImportError::service("source_too_large"))?;
        counts[index].0 = counts[index]
            .0
            .checked_add(1)
            .ok_or_else(|| ImportError::service("source_too_large"))?;
        if candidate.missing_payload {
            counts[index].1 = counts[index]
                .1
                .checked_add(1)
                .ok_or_else(|| ImportError::service("source_too_large"))?;
        }
    }
    let reserved = source_control_bytes.min(MAX_IMPORT_ANALYSIS_CONTROL_BYTES);
    if reserved < MAX_IMPORT_ANALYSIS_CONTROL_BYTES {
        return Err(ImportError::service("analysis_too_large"));
    }
    before_allocation(MAX_IMPORT_ANALYSIS_CONTROL_BYTES, reserved);
    let mut result = Vec::with_capacity(IMPORT_KIND_ORDER.len());
    result.extend(IMPORT_KIND_ORDER.into_iter().zip(counts).filter_map(
        |(kind, (event_count, missing_payload_count))| {
            (event_count > 0).then_some(ImportKindCount {
                kind,
                event_count,
                missing_payload_count,
            })
        },
    ));
    Ok(result)
}

pub(crate) fn parse_detected_export_report_with_permit(
    detected: &detect::DetectedExport,
    secret: Option<&RayconfigSecret>,
    permit: &ImportOperationPermit,
    limits: ImportParseLimits,
) -> Result<ImportParseReport, ImportError> {
    match detected.source {
        ImportSource::Raycast if detected.encrypted => {
            let Some(secret) = secret else {
                return Err(ImportError::export(
                    ImportSource::Raycast.as_str(),
                    "rayconfig_password_required",
                ));
            };
            let mut container = read_bounded_container(&detected.export_path)?;
            let report =
                raycast::parse_rayconfig_report_with_permit(&mut container, secret, permit, limits);
            // The plaintext was decrypted in place, so the buffer still holds
            // the user's clipboard history. Clear it rather than handing it
            // back to the allocator intact.
            container.zeroize();
            report
        }
        ImportSource::Raycast => {
            raycast::parse_raycast_report_with_permit(&detected.export_path, permit, limits)
        }
        ImportSource::SuperCmd => {
            if detected
                .export_path
                .extension()
                .and_then(|extension| extension.to_str())
                == Some("csv")
            {
                return supercmd::parse_supercmd_csv_report_with_permit(
                    &detected.export_path,
                    permit,
                    limits,
                );
            }
            let root = detected
                .export_path
                .parent()
                .unwrap_or_else(|| Path::new("."));
            supercmd::parse_supercmd_report_with_permit(root, &detected.export_path, permit, limits)
        }
    }
}

/// Reads a whole `.rayconfig` under its own cap.
///
/// The size is checked before the read and again against what was actually
/// read: a file can grow between the two, and the second check is what makes
/// the budget a fact rather than a hope.
fn read_bounded_container(path: &Path) -> Result<Vec<u8>, ImportError> {
    let source_kind = ImportSource::Raycast.as_str();
    let metadata =
        fs::metadata(path).map_err(|_| ImportError::export(source_kind, "unreadable_export"))?;
    rayconfig::validate_container_length(metadata.len())?;
    let file =
        fs::File::open(path).map_err(|_| ImportError::export(source_kind, "unreadable_export"))?;
    let mut container = Vec::new();
    file.take(service::MAX_RAYCONFIG_CIPHERTEXT_BYTES as u64 + 1)
        .read_to_end(&mut container)
        .map_err(|_| ImportError::export(source_kind, "unreadable_export"))?;
    rayconfig::validate_container_length(container.len() as u64)?;
    Ok(container)
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

pub(crate) fn stream_json_records_path_with_limits(
    path: &Path,
    source_kind: &'static str,
    manifest_limit: usize,
    record_limit: usize,
    record: impl FnMut(usize, JsonRecord<'_>) -> Result<bool, ImportError>,
) -> Result<usize, ImportError> {
    let metadata =
        fs::metadata(path).map_err(|_| ImportError::export(source_kind, "unreadable_export"))?;
    if metadata.len() > manifest_limit as u64 {
        return Err(ImportError::service("analysis_too_large"));
    }
    let file =
        fs::File::open(path).map_err(|_| ImportError::export(source_kind, "unreadable_export"))?;
    stream_json_records_from_reader_with_limits(
        file,
        source_kind,
        manifest_limit,
        record_limit,
        record,
    )
}

pub(crate) enum JsonRecord<'a> {
    Complete(&'a [u8]),
    TooLarge,
}

/// Whether anything may follow the record array.
///
/// A plain manifest is the whole document, so bytes after the closing bracket
/// mean it is not the document we were promised. A rayconfig's array sits in
/// the middle of a larger object, and its siblings follow it legitimately.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Trailing {
    Forbidden,
    Ignored,
}

pub(crate) fn stream_json_records_from_reader_with_limits<R: Read>(
    reader: R,
    source_kind: &'static str,
    manifest_limit: usize,
    record_limit: usize,
    record: impl FnMut(usize, JsonRecord<'_>) -> Result<bool, ImportError>,
) -> Result<usize, ImportError> {
    stream_json_records_from_reader_with_trailing(
        reader,
        source_kind,
        manifest_limit,
        record_limit,
        Trailing::Forbidden,
        record,
    )
}

pub(crate) fn stream_json_records_from_reader_with_trailing<R: Read>(
    reader: R,
    source_kind: &'static str,
    manifest_limit: usize,
    record_limit: usize,
    trailing: Trailing,
    mut record: impl FnMut(usize, JsonRecord<'_>) -> Result<bool, ImportError>,
) -> Result<usize, ImportError> {
    if manifest_limit == 0 || record_limit == 0 {
        return Err(ImportError::service("analysis_too_large"));
    }
    let read_limit = u64::try_from(manifest_limit)
        .unwrap_or(u64::MAX - 1)
        .saturating_add(1);
    let mut reader = BoundedJsonReader {
        reader: BufReader::with_capacity(JSON_READER_BUFFER_BYTES, reader.take(read_limit)),
        bytes_read: 0,
        manifest_limit,
        source_kind,
    };
    if reader.next_non_whitespace()? != Some(b'[') {
        return Err(ImportError::export(source_kind, "invalid_document"));
    }
    let Some(mut first) = reader.next_non_whitespace()? else {
        return Err(ImportError::export(source_kind, "invalid_document"));
    };
    if first == b']' {
        if trailing == Trailing::Forbidden {
            reader.require_end()?;
        }
        return Ok(0);
    }

    let mut total = 0_usize;
    let mut bytes = Vec::with_capacity(record_limit);
    loop {
        if first != b'{' {
            return Err(ImportError::export(source_kind, "invalid_document"));
        }
        bytes.clear();
        bytes.push(first);
        let mut too_large = false;
        let mut expected_delimiters = [0_u8; MAX_JSON_DELIMITER_DEPTH];
        expected_delimiters[0] = b'}';
        let mut depth = 1_usize;
        let mut in_string = false;
        let mut escaped = false;
        while depth != 0 {
            let byte = reader
                .next_byte()?
                .ok_or_else(|| ImportError::export(source_kind, "invalid_document"))?;
            if !too_large {
                if bytes.len() < record_limit {
                    bytes.push(byte);
                } else {
                    too_large = true;
                }
            }
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
                opening @ (b'{' | b'[') => {
                    if depth == expected_delimiters.len() {
                        return Err(ImportError::export(source_kind, "invalid_document"));
                    }
                    expected_delimiters[depth] = if opening == b'{' { b'}' } else { b']' };
                    depth += 1;
                }
                closing @ (b'}' | b']') => {
                    if expected_delimiters[depth - 1] != closing {
                        return Err(ImportError::export(source_kind, "invalid_document"));
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        total = total
            .checked_add(1)
            .ok_or_else(|| ImportError::service("source_too_large"))?;
        let framed = if too_large {
            JsonRecord::TooLarge
        } else {
            JsonRecord::Complete(&bytes)
        };
        if !record(total, framed)? {
            return Ok(total);
        }
        match reader.next_non_whitespace()? {
            Some(b']') => {
                if trailing == Trailing::Forbidden {
                    reader.require_end()?;
                }
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

struct BoundedJsonReader<R> {
    reader: BufReader<std::io::Take<R>>,
    bytes_read: usize,
    manifest_limit: usize,
    source_kind: &'static str,
}

impl<R: Read> BoundedJsonReader<R> {
    fn next_byte(&mut self) -> Result<Option<u8>, ImportError> {
        let mut byte = [0_u8; 1];
        match self.reader.read(&mut byte) {
            Ok(0) => Ok(None),
            Ok(_) => {
                self.bytes_read = self
                    .bytes_read
                    .checked_add(1)
                    .ok_or_else(|| ImportError::service("analysis_too_large"))?;
                if self.bytes_read > self.manifest_limit {
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
) -> Result<[u8; 32], CoreError> {
    let mut hasher = FramedHasher::new();
    hasher.add_optional(Some(source.as_str().as_bytes()));
    hasher.add_optional(Some(captured_at_ms.to_string().as_bytes()));
    hasher.add_optional(Some(kind.as_str().as_bytes()));
    for field in stable_fields {
        hasher.add_optional(field.map(str::as_bytes));
    }
    hasher.add_canonical(kind, primary_payload)?;
    Ok(hasher.finish())
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

    pub(crate) fn add_canonical(
        &mut self,
        kind: ContentKind,
        bytes: &[u8],
    ) -> Result<(), CoreError> {
        self.0.update(&[1]);
        self.0
            .update(&(canonical_byte_len(kind, bytes)? as u64).to_be_bytes());
        update_canonical_bytes(kind, bytes, |chunk| {
            self.0.update(chunk);
        })?;
        Ok(())
    }

    pub(crate) fn finish(self) -> [u8; 32] {
        *self.0.finalize().as_bytes()
    }
}

/// Builds the `text/uri-list` reference kept alongside a file or image entry.
///
/// The importer never reads bytes from outside the selected export root, so a
/// source path is metadata rather than a payload: it lets the interface show
/// where the item came from and offer to reveal it. A value that already
/// carries a scheme is kept verbatim; an absolute filesystem path becomes a
/// `file:` URI. Anything relative points inside the export, not at a location
/// the user could open later, so it yields no reference at all.
pub fn source_reference_uri(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if has_uri_scheme(trimmed) {
        return Some(trimmed.to_owned());
    }
    if !trimmed.starts_with('/') {
        return None;
    }
    let mut uri = String::with_capacity("file://".len() + trimmed.len());
    uri.push_str("file://");
    for byte in trimmed.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(char::from(*byte));
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    Some(uri)
}

/// Turns a `file:` URI back into a local path.
///
/// Any other scheme names something this application will not open, so it
/// yields nothing rather than a path a caller would wrongly act on.
pub fn file_uri_to_path(reference: &str) -> Option<String> {
    let encoded = reference.strip_prefix("file://")?;
    if !encoded.starts_with('/') {
        return None;
    }
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            let text = std::str::from_utf8(hex).ok()?;
            decoded.push(u8::from_str_radix(text, 16).ok()?);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    let path = String::from_utf8(decoded).ok()?;
    // A NUL byte would truncate the path once it reaches the operating system.
    (!path.contains('\0')).then_some(path)
}

/// True when a candidate has no payload and no reachable source either.
///
/// Such a record adds a row the user can neither read nor open — the evicted
/// Raycast image cache and deleted attachments are entirely this. Keeping them
/// buried the entries that still work, so the importer leaves them out.
pub(crate) fn candidate_leads_nowhere(candidate: &ImportCandidate) -> bool {
    if !candidate.missing_payload {
        return false;
    }
    candidate
        .capture
        .representations
        .iter()
        .filter(|representation| representation.format_id == "text/uri-list")
        .filter_map(|representation| representation.bytes.as_deref())
        .filter_map(|bytes| std::str::from_utf8(bytes).ok())
        .filter_map(file_uri_to_path)
        .all(|path| std::fs::metadata(path).is_err())
}

fn has_uri_scheme(value: &str) -> bool {
    let Some(position) = value.find(':') else {
        return false;
    };
    let scheme = &value[..position];
    !scheme.is_empty()
        && scheme.starts_with(|character: char| character.is_ascii_alphabetic())
        && scheme.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
        })
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
    let mut identifier = String::with_capacity("raycast-app:".len() + name.len());
    identifier.push_str("raycast-app:");
    for character in name.chars() {
        match character {
            ' ' => identifier.push('-'),
            character if character.is_ascii_uppercase() => {
                identifier.push(character.to_ascii_lowercase());
            }
            character => identifier.push(character),
        }
    }
    (Some(identifier), Some(name))
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, io::Cursor, rc::Rc};

    use clipboard_core::{
        CaptureInput, ContentFlags, ContentKind, EventFlags, RepresentationInput, SourceConfidence,
    };

    use super::{
        ImportCandidate, ImportParseReport, ImportSource, JsonRecord,
        aggregate_kind_counts_with_hook, stream_json_records_from_reader_with_limits,
    };

    #[test]
    fn analysis_kind_capacity_is_authorized_before_growth() {
        let report = ImportParseReport::with_source_limit(0, 4096);
        let authorized = Cell::new(false);

        let counts = aggregate_kind_counts_with_hook(&report, 4096, |required, reserved| {
            assert!(required > 0);
            assert!(required <= reserved);
            authorized.set(true);
        })
        .unwrap();

        assert!(authorized.get());
        assert!(counts.is_empty());
    }

    #[test]
    fn bounded_import_candidate_owns_primary_text_only_in_its_representation() {
        let candidate = ImportCandidate {
            source: ImportSource::Raycast,
            source_record: 1,
            record_fingerprint: [7; 32],
            capture: CaptureInput {
                captured_at_ms: 1_000,
                kind: ContentKind::Text,
                primary_mime: "text/plain".to_owned(),
                representations: vec![RepresentationInput {
                    format_id: "text/plain".to_owned(),
                    bytes: Some(b"one owned payload".to_vec()),
                    missing_ref: None,
                }],
                source_app_id: None,
                source_app_name: None,
                source_confidence: SourceConfidence::Unknown,
                pinned: false,
                occurrence_count: 1,
                content_flags: ContentFlags::empty(),
                event_flags: EventFlags::IMPORTED,
                display_label: None,
            },
            search_ocr: None,
            missing_payload: false,
            source_application_path: None,
        };

        assert_eq!(
            candidate.capture.representations[0].bytes.as_deref(),
            Some(b"one owned payload".as_slice())
        );
    }

    #[test]
    fn bounded_json_drains_one_oversized_nested_record_and_resumes() {
        let exact = br#"{"outer":[{"text":"brace } and escaped quote \" ok"}],"tail":true}"#;
        let record_limit = exact.len();
        let mut oversized = exact.to_vec();
        oversized.insert(oversized.len() - 1, b' ');
        assert_eq!(oversized.len(), record_limit + 1);
        let later = br#"{"later":"valid"}"#;
        let mut document = Vec::new();
        document.push(b'[');
        document.extend_from_slice(exact);
        document.push(b',');
        document.extend_from_slice(&oversized);
        document.push(b',');
        document.extend_from_slice(later);
        document.push(b']');
        let mut observed = Vec::new();

        let total = stream_json_records_from_reader_with_limits(
            Cursor::new(document.clone()),
            "synthetic",
            document.len(),
            record_limit,
            |index, record| {
                observed.push(match record {
                    JsonRecord::Complete(bytes) => (index, Some(bytes.to_vec())),
                    JsonRecord::TooLarge => (index, None),
                });
                Ok(true)
            },
        )
        .unwrap();

        assert_eq!(total, 3);
        assert_eq!(observed[0], (1, Some(exact.to_vec())));
        assert_eq!(observed[1], (2, None));
        assert_eq!(observed[2], (3, Some(later.to_vec())));
    }

    #[test]
    fn bounded_json_reuses_one_record_buffer_across_the_stream() {
        let document = br#"[{"a":1},{"b":2},{"c":3}]"#;
        let mut total = None;
        let allocations = allocation_counter::measure(|| {
            total = Some(stream_json_records_from_reader_with_limits(
                Cursor::new(document),
                "synthetic",
                document.len(),
                16,
                |_, record| {
                    assert!(matches!(record, JsonRecord::Complete(_)));
                    Ok(true)
                },
            ));
        });

        assert_eq!(total.unwrap().unwrap(), 3);
        assert_eq!(
            allocations.count_total, 2,
            "the reader and one reusable record buffer are the only expected allocations: {allocations:?}"
        );
    }

    #[test]
    fn bounded_json_framing_rejects_mismatched_delimiters_before_callback() {
        let document = br#"[{"nested":[1}}]"#;
        let mut callback_count = 0;

        let error = stream_json_records_from_reader_with_limits(
            Cursor::new(document),
            "synthetic",
            document.len(),
            document.len(),
            |_, _| {
                callback_count += 1;
                Ok(true)
            },
        )
        .unwrap_err();

        assert_eq!(callback_count, 0);
        assert_eq!(error.to_string(), "synthetic export: invalid_document");
    }

    struct GrowingReader {
        bytes: Cursor<Vec<u8>>,
        consumed: Rc<Cell<usize>>,
    }

    impl std::io::Read for GrowingReader {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            let read = std::io::Read::read(&mut self.bytes, output)?;
            self.consumed.set(self.consumed.get() + read);
            Ok(read)
        }
    }

    #[test]
    fn bounded_growing_json_reader_stops_at_the_manifest_cap() {
        let manifest_limit = 128;
        let consumed = Rc::new(Cell::new(0));
        let reader = GrowingReader {
            bytes: Cursor::new(
                format!("[{{\"text\":\"{}\"}}]", "x".repeat(manifest_limit * 2)).into_bytes(),
            ),
            consumed: consumed.clone(),
        };

        let error = stream_json_records_from_reader_with_limits(
            reader,
            "synthetic",
            manifest_limit,
            64,
            |_, _| Ok(true),
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "import service: analysis_too_large");
        assert_eq!(consumed.get(), manifest_limit + 1);
    }
}
