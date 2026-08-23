use std::{
    ffi::OsStr,
    fmt, fs,
    io::Read,
    path::{Path, PathBuf},
};

use crate::{
    ImportError, ImportParseLimits, ImportSource, JsonRecord, stream_json_records_path_with_limits,
};

/// Maximum number of top-level directory entries inspected while discovering an unnamed export
/// manifest. Named `clipboard.json` / `clipboard.csv` files bypass discovery entirely.
pub const MAX_MANIFEST_DISCOVERY_ENTRIES: usize = 4_096;
const MAX_CONCURRENT_IMPORT_PATH_CAPACITIES: usize = 8;
const MAX_IMPORT_PATH_BYTES: usize = 64 * 1024;
pub(crate) const MAX_IMPORT_PATH_CONTROL_BYTES: usize =
    MAX_CONCURRENT_IMPORT_PATH_CAPACITIES * MAX_IMPORT_PATH_BYTES;

#[derive(Clone, Eq, PartialEq)]
pub struct DetectedExport {
    pub source: ImportSource,
    pub export_path: PathBuf,
    pub source_fingerprint: [u8; 32],
    /// True when the bytes are encrypted and a password is needed to read
    /// them. Detection itself never needs one.
    pub encrypted: bool,
}

impl fmt::Debug for DetectedExport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DetectedExport")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

pub(crate) fn detect_export_with_permit(
    path: impl AsRef<Path>,
    permit: &clipboard_store::ImportOperationPermit,
    limits: ImportParseLimits,
) -> Result<DetectedExport, ImportError> {
    let path = path.as_ref();
    if path.is_dir() {
        return detect_directory(path, permit, limits);
    }
    detect_file(path, permit, limits)
}

fn detect_directory(
    directory: &Path,
    permit: &clipboard_store::ImportOperationPermit,
    limits: ImportParseLimits,
) -> Result<DetectedExport, ImportError> {
    detect_directory_with_limit(directory, MAX_MANIFEST_DISCOVERY_ENTRIES, permit, limits)
}

fn detect_directory_with_limit(
    directory: &Path,
    max_examined_entries: usize,
    permit: &clipboard_store::ImportOperationPermit,
    limits: ImportParseLimits,
) -> Result<DetectedExport, ImportError> {
    let json = bounded_path_join(directory, OsStr::new("clipboard.json"), limits)?;
    let csv = bounded_path_join(directory, OsStr::new("clipboard.csv"), limits)?;
    if json.is_file() {
        match detect_file(&json, permit, limits) {
            Ok(detected) => return Ok(detected),
            Err(json_error) if csv.is_file() => {
                if let Ok(detected) = detect_file(&csv, permit, limits) {
                    return Ok(detected);
                }
                return Err(json_error);
            }
            Err(error) => return Err(error),
        }
    }
    if csv.is_file() {
        return detect_file(&csv, permit, limits);
    }

    let discovered = select_unnamed_manifest_in_directory(directory, max_examined_entries, limits)?;
    // A plain manifest wins whenever there is one, so a directory holding both
    // never asks for a password it does not need. The encrypted file is a
    // fallback rather than a rival, which is why one of each is not ambiguous.
    match discovered.plain.or(discovered.encrypted) {
        Some(manifest) => detect_file(&manifest, permit, limits),
        None => Err(ImportError::export("detection", "manifest_not_found")),
    }
}

/// The manifests a directory offered, one slot per class.
#[derive(Default)]
struct DiscoveredManifests {
    plain: Option<PathBuf>,
    encrypted: Option<PathBuf>,
}

fn select_unnamed_manifest_in_directory(
    directory: &Path,
    max_examined_entries: usize,
    limits: ImportParseLimits,
) -> Result<DiscoveredManifests, ImportError> {
    let entries = fs::read_dir(directory)
        .map_err(|_| ImportError::export("detection", "unreadable_export"))?;
    let mut examined_entries = 0_usize;
    let mut discovered = DiscoveredManifests::default();
    for entry in entries {
        examined_entries = examined_entries
            .checked_add(1)
            .ok_or_else(|| ImportError::export("detection", "export_too_large"))?;
        if examined_entries > max_examined_entries {
            return Err(ImportError::export("detection", "export_too_large"));
        }
        let Some(entry) = entry.ok() else {
            continue;
        };
        if !entry.file_type().ok().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let file_name = entry.file_name();
        let slot = match Path::new(&file_name)
            .extension()
            .and_then(|extension| extension.to_str())
        {
            Some("json" | "csv") => &mut discovered.plain,
            Some("rayconfig") => &mut discovered.encrypted,
            _ => continue,
        };
        // Ambiguity is judged within a class: two manifests of the same kind
        // give no way to choose, while one of each is answered by precedence.
        if slot.is_some() {
            return Err(ImportError::export("detection", "ambiguous_manifest"));
        }
        *slot = Some(bounded_path_join(directory, &file_name, limits)?);
    }
    Ok(discovered)
}

#[cfg(test)]
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

fn detect_file(
    path: &Path,
    permit: &clipboard_store::ImportOperationPermit,
    limits: ImportParseLimits,
) -> Result<DetectedExport, ImportError> {
    let mut encrypted = false;
    let source = match path.extension().and_then(|extension| extension.to_str()) {
        Some("csv") => detect_csv_source(path, permit, limits)?,
        Some("rayconfig") => {
            // Named rather than sniffed: the schema is behind the encryption,
            // so the file's shape is all detection can check without a
            // password. The real schema check happens when it is parsed.
            let metadata = fs::metadata(path)
                .map_err(|_| ImportError::export("detection", "unreadable_export"))?;
            crate::rayconfig::validate_container_length(metadata.len())?;
            encrypted = true;
            ImportSource::Raycast
        }
        Some("json") => {
            limits.ensure_json_operation(permit)?;
            let mut detected = None;
            stream_json_records_path_with_limits(
                path,
                "detection",
                limits.manifest_bytes,
                limits.record_bytes,
                |_, framed| match framed {
                    JsonRecord::Complete(bytes) => {
                        detected = Some(detect_json_source(bytes)?);
                        Ok(false)
                    }
                    JsonRecord::TooLarge => Err(ImportError::service("record_too_large")),
                },
            )?;
            detected.ok_or_else(|| ImportError::export("detection", "invalid_document"))?
        }
        _ => return Err(ImportError::export("detection", "unsupported_format")),
    };
    Ok(DetectedExport {
        source,
        export_path: bounded_path_copy(path, limits)?,
        source_fingerprint: source_fingerprint(source, path, fingerprint_limit(encrypted, limits))?,
        encrypted,
    })
}

/// How many bytes the fingerprint may read.
///
/// A rayconfig is bounded by its own, smaller cap. Using the manifest limit
/// would let detection accept a file the parser then refuses, which reads as
/// two unrelated failures for one cause.
fn fingerprint_limit(encrypted: bool, limits: ImportParseLimits) -> usize {
    if encrypted {
        crate::service::MAX_RAYCONFIG_CIPHERTEXT_BYTES
    } else {
        limits.manifest_bytes
    }
}

pub(crate) fn bounded_path_copy(
    path: &Path,
    limits: ImportParseLimits,
) -> Result<PathBuf, ImportError> {
    bounded_path_copy_with_hook(path, limits, |_, _| {})
}

fn bounded_path_copy_with_hook(
    path: &Path,
    limits: ImportParseLimits,
    before_allocation: impl FnOnce(usize, usize),
) -> Result<PathBuf, ImportError> {
    let required = path.as_os_str().as_encoded_bytes().len();
    let reserved = bounded_path_slot(limits);
    if required > reserved {
        return Err(ImportError::service("analysis_too_large"));
    }
    before_allocation(required, reserved);
    let mut copied = PathBuf::with_capacity(reserved);
    copied.push(path);
    Ok(copied)
}

fn bounded_path_join(
    parent: &Path,
    child: &OsStr,
    limits: ImportParseLimits,
) -> Result<PathBuf, ImportError> {
    let required = parent
        .as_os_str()
        .as_encoded_bytes()
        .len()
        .checked_add(1)
        .and_then(|bytes| bytes.checked_add(child.as_encoded_bytes().len()))
        .ok_or_else(|| ImportError::service("analysis_too_large"))?;
    let reserved = bounded_path_slot(limits);
    if required > reserved {
        return Err(ImportError::service("analysis_too_large"));
    }
    let mut joined = PathBuf::with_capacity(reserved);
    joined.push(parent);
    joined.push(child);
    Ok(joined)
}

fn bounded_path_slot(limits: ImportParseLimits) -> usize {
    limits
        .source_control_bytes
        .checked_div(MAX_CONCURRENT_IMPORT_PATH_CAPACITIES)
        .unwrap_or(0)
        .min(MAX_IMPORT_PATH_BYTES)
}

fn detect_csv_source(
    path: &Path,
    permit: &clipboard_store::ImportOperationPermit,
    limits: ImportParseLimits,
) -> Result<ImportSource, ImportError> {
    if crate::supercmd::detect_supercmd_csv_with_permit(path, permit, limits)? {
        return Ok(ImportSource::SuperCmd);
    }
    Err(ImportError::export("detection", "unknown_schema"))
}

fn source_fingerprint(
    source: ImportSource,
    path: &Path,
    manifest_limit: usize,
) -> Result<[u8; 32], ImportError> {
    let metadata =
        fs::metadata(path).map_err(|_| ImportError::export("detection", "unreadable_export"))?;
    if metadata.len() > manifest_limit as u64 {
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
        let remaining = manifest_limit
            .checked_add(1)
            .and_then(|limit| limit.checked_sub(read_bytes))
            .ok_or_else(|| ImportError::service("analysis_too_large"))?;
        let requested = buffer.len().min(remaining);
        let read = file
            .read(&mut buffer[..requested])
            .map_err(|_| ImportError::export("detection", "unreadable_export"))?;
        if read == 0 {
            break;
        }
        read_bytes = read_bytes
            .checked_add(read)
            .ok_or_else(|| ImportError::service("analysis_too_large"))?;
        if read_bytes > manifest_limit {
            return Err(ImportError::service("analysis_too_large"));
        }
        hasher.update(&buffer[..read]);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn detect_json_source(bytes: &[u8]) -> Result<ImportSource, ImportError> {
    let fields = JsonSchemaScanner::new(bytes)
        .scan()
        .map_err(|()| ImportError::export("detection", "invalid_document"))?;

    if fields.created_at && fields.category {
        return Ok(ImportSource::Raycast);
    }
    if fields.copied_at {
        return Ok(ImportSource::SuperCmd);
    }
    Err(ImportError::export("detection", "unknown_schema"))
}

#[derive(Default)]
struct SchemaFields {
    created_at: bool,
    category: bool,
    copied_at: bool,
}

#[derive(Clone, Copy)]
enum SchemaKey {
    CreatedAt,
    Category,
    CopiedAt,
}

struct KnownKeyMatcher {
    position: usize,
    candidates: u8,
}

impl KnownKeyMatcher {
    const KEYS: [&'static [u8]; 4] = [b"createdAt", b"category", b"copied_at", b"copiedAt"];

    fn new() -> Self {
        Self {
            position: 0,
            candidates: (1 << Self::KEYS.len()) - 1,
        }
    }

    fn push(&mut self, character: char) {
        for (index, key) in Self::KEYS.iter().enumerate() {
            let bit = 1 << index;
            if self.candidates & bit != 0
                && (!character.is_ascii()
                    || key.get(self.position).copied() != Some(character as u8))
            {
                self.candidates &= !bit;
            }
        }
        self.position = self.position.saturating_add(1);
    }

    fn finish(self) -> Option<SchemaKey> {
        let matched = Self::KEYS
            .iter()
            .enumerate()
            .find(|(index, key)| self.candidates & (1 << index) != 0 && key.len() == self.position)?
            .0;
        match matched {
            0 => Some(SchemaKey::CreatedAt),
            1 => Some(SchemaKey::Category),
            2 | 3 => Some(SchemaKey::CopiedAt),
            _ => None,
        }
    }
}

struct JsonSchemaScanner<'a> {
    bytes: &'a [u8],
    index: usize,
}

impl<'a> JsonSchemaScanner<'a> {
    const MAX_NESTING_DEPTH: usize = 128;

    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, index: 0 }
    }

    fn scan(mut self) -> Result<SchemaFields, ()> {
        let mut fields = SchemaFields::default();
        self.skip_whitespace();
        self.expect(b'{')?;
        self.skip_whitespace();
        if self.consume(b'}') {
            self.finish_document()?;
            return Ok(fields);
        }
        loop {
            let mut matcher = KnownKeyMatcher::new();
            self.parse_string(Some(&mut matcher))?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_value(1)?;
            match matcher.finish() {
                Some(SchemaKey::CreatedAt) => fields.created_at = true,
                Some(SchemaKey::Category) => fields.category = true,
                Some(SchemaKey::CopiedAt) => fields.copied_at = true,
                None => {}
            }
            self.skip_whitespace();
            if self.consume(b'}') {
                self.finish_document()?;
                return Ok(fields);
            }
            self.expect(b',')?;
            self.skip_whitespace();
        }
    }

    fn finish_document(&mut self) -> Result<(), ()> {
        self.skip_whitespace();
        (self.index == self.bytes.len()).then_some(()).ok_or(())
    }

    fn skip_value(&mut self, depth: usize) -> Result<(), ()> {
        self.skip_whitespace();
        match self.peek().ok_or(())? {
            b'"' => self.parse_string(None),
            b'{' => {
                if depth >= Self::MAX_NESTING_DEPTH {
                    return Err(());
                }
                self.skip_object(depth + 1)
            }
            b'[' => {
                if depth >= Self::MAX_NESTING_DEPTH {
                    return Err(());
                }
                self.skip_array(depth + 1)
            }
            b't' => self.consume_literal(b"true"),
            b'f' => self.consume_literal(b"false"),
            b'n' => self.consume_literal(b"null"),
            b'-' | b'0'..=b'9' => self.skip_number(),
            _ => Err(()),
        }
    }

    fn skip_object(&mut self, depth: usize) -> Result<(), ()> {
        self.expect(b'{')?;
        self.skip_whitespace();
        if self.consume(b'}') {
            return Ok(());
        }
        loop {
            self.parse_string(None)?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_value(depth)?;
            self.skip_whitespace();
            if self.consume(b'}') {
                return Ok(());
            }
            self.expect(b',')?;
            self.skip_whitespace();
        }
    }

    fn skip_array(&mut self, depth: usize) -> Result<(), ()> {
        self.expect(b'[')?;
        self.skip_whitespace();
        if self.consume(b']') {
            return Ok(());
        }
        loop {
            self.skip_value(depth)?;
            self.skip_whitespace();
            if self.consume(b']') {
                return Ok(());
            }
            self.expect(b',')?;
            self.skip_whitespace();
        }
    }

    fn parse_string(&mut self, mut matcher: Option<&mut KnownKeyMatcher>) -> Result<(), ()> {
        self.expect(b'"')?;
        loop {
            let byte = self.peek().ok_or(())?;
            let character = match byte {
                b'"' => {
                    self.index += 1;
                    return Ok(());
                }
                b'\\' => self.parse_escape()?,
                0x00..=0x1f => return Err(()),
                0x20..=0x7f => {
                    self.index += 1;
                    byte as char
                }
                _ => self.parse_utf8_character()?,
            };
            if let Some(matcher) = matcher.as_deref_mut() {
                matcher.push(character);
            }
        }
    }

    fn parse_escape(&mut self) -> Result<char, ()> {
        self.expect(b'\\')?;
        let escaped = self.next().ok_or(())?;
        match escaped {
            b'"' => Ok('"'),
            b'\\' => Ok('\\'),
            b'/' => Ok('/'),
            b'b' => Ok('\u{0008}'),
            b'f' => Ok('\u{000c}'),
            b'n' => Ok('\n'),
            b'r' => Ok('\r'),
            b't' => Ok('\t'),
            b'u' => {
                let high = self.parse_hex_quad()?;
                let scalar = if (0xd800..=0xdbff).contains(&high) {
                    self.expect(b'\\')?;
                    self.expect(b'u')?;
                    let low = self.parse_hex_quad()?;
                    if !(0xdc00..=0xdfff).contains(&low) {
                        return Err(());
                    }
                    0x1_0000 + (((high as u32 - 0xd800) << 10) | (low as u32 - 0xdc00))
                } else if (0xdc00..=0xdfff).contains(&high) {
                    return Err(());
                } else {
                    high as u32
                };
                char::from_u32(scalar).ok_or(())
            }
            _ => Err(()),
        }
    }

    fn parse_hex_quad(&mut self) -> Result<u16, ()> {
        let mut value = 0_u16;
        for _ in 0..4 {
            let digit = self.next().and_then(|byte| match byte {
                b'0'..=b'9' => Some((byte - b'0') as u16),
                b'a'..=b'f' => Some((byte - b'a' + 10) as u16),
                b'A'..=b'F' => Some((byte - b'A' + 10) as u16),
                _ => None,
            });
            let digit = digit.ok_or(())?;
            value = value
                .checked_mul(16)
                .and_then(|value| value.checked_add(digit))
                .ok_or(())?;
        }
        Ok(value)
    }

    fn parse_utf8_character(&mut self) -> Result<char, ()> {
        let width = match self.peek().ok_or(())? {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return Err(()),
        };
        let end = self.index.checked_add(width).ok_or(())?;
        let encoded = self.bytes.get(self.index..end).ok_or(())?;
        let decoded = std::str::from_utf8(encoded).map_err(|_| ())?;
        let mut characters = decoded.chars();
        let character = characters.next().ok_or(())?;
        if characters.next().is_some() {
            return Err(());
        }
        self.index = end;
        Ok(character)
    }

    fn skip_number(&mut self) -> Result<(), ()> {
        self.consume(b'-');
        match self.next().ok_or(())? {
            b'0' if self.peek().is_some_and(|byte| byte.is_ascii_digit()) => return Err(()),
            b'0' => {}
            b'1'..=b'9' => {
                while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                    self.index += 1;
                }
            }
            _ => return Err(()),
        }
        if self.consume(b'.') {
            if !self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                return Err(());
            }
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.index += 1;
            }
        }
        if self.consume(b'e') || self.consume(b'E') {
            if !self.consume(b'+') {
                self.consume(b'-');
            }
            if !self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                return Err(());
            }
            while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
                self.index += 1;
            }
        }
        Ok(())
    }

    fn consume_literal(&mut self, literal: &[u8]) -> Result<(), ()> {
        let end = self.index.checked_add(literal.len()).ok_or(())?;
        if self.bytes.get(self.index..end) != Some(literal) {
            return Err(());
        }
        self.index = end;
        Ok(())
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.index += 1;
        }
    }

    fn expect(&mut self, expected: u8) -> Result<(), ()> {
        self.consume(expected).then_some(()).ok_or(())
    }

    fn consume(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.index += 1;
        Some(byte)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.index).copied()
    }
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
            encrypted: false,
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
    fn selected_path_capacity_is_authorized_before_copy() {
        let path = PathBuf::from("synthetic-export.json");
        let limits = ImportParseLimits {
            manifest_bytes: 1024,
            record_bytes: 1024,
            header_bytes: 1024,
            source_bytes: 4096,
            source_control_bytes: 8 * 1024,
            auxiliary_bytes: 1024,
        };
        let authorized = Cell::new(false);

        let copied = bounded_path_copy_with_hook(&path, limits, |required, reserved| {
            assert!(required > 0);
            assert!(required <= reserved);
            authorized.set(true);
        })
        .unwrap();

        assert!(authorized.get());
        assert_eq!(copied, path);
        assert_eq!(copied.capacity(), limits.source_control_bytes / 8);
    }

    #[test]
    fn unnamed_manifest_discovery_has_a_path_free_entry_budget_error() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("synthetic.txt"), b"synthetic").unwrap();
        let gate = clipboard_store::ImportOperationGate::with_capacity(64 * 1024).unwrap();
        let permit = gate.acquire_blocking().unwrap();
        let limits = ImportParseLimits {
            manifest_bytes: 1024,
            record_bytes: 1024,
            header_bytes: 1024,
            source_bytes: 4096,
            source_control_bytes: 1024,
            auxiliary_bytes: 1024,
        };

        let error = detect_directory_with_limit(directory.path(), 0, &permit, limits).unwrap_err();

        assert_eq!(error.to_string(), "detection export: export_too_large");
        assert!(
            !error
                .to_string()
                .contains(directory.path().to_string_lossy().as_ref())
        );
    }

    #[test]
    fn bounded_json_detection_does_not_allocate_schema_keys() {
        let record = br#"{"\ud83d\ude00":{"nested":["brace } and quote \"",2,3]},"\u0063reatedAt":"2026-01-02T03:04:05Z","\u0063ategory":"text"}"#;
        let mut source = None;

        let allocations = allocation_counter::measure(|| {
            source = Some(detect_json_source(record).unwrap());
        });

        assert_eq!(source, Some(ImportSource::Raycast));
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
        assert_eq!(allocations.bytes_total, 0, "{allocations:?}");
    }
}
