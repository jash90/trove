//! Reading the file Raycast actually exports.
//!
//! Raycast writes one file — `Raycast <date>.rayconfig` — and encrypts it.
//! Until now the importer could only read a `clipboard.json` that somebody had
//! decrypted by hand, so a person who simply exported their history was told
//! the export was an unsupported format.
//!
//! The container is `IV(16) ‖ AES-256-CBC-PKCS7( gzip( JSON ) )`, keyed by
//! `SHA-256(password)`. Inside, the clipboard records sit under
//! `builtin_package_clipboardHistory.clipboardHistoryRecords` and are the same
//! array a plain `clipboard.json` holds, so once the bytes are in hand the
//! existing Raycast parser reads them unchanged.
//!
//! AES-CBC carries no authentication tag, so nothing here can prove a password
//! was right. What it can do is refuse what is demonstrably wrong: the padding,
//! then the gzip signature. Together those are the whole of wrong-password
//! detection, which is why neither may be skipped.
//!
//! The plaintext is never written to disk. Decrypting to a temporary file would
//! have let the existing path read it unchanged, at the price of leaving the
//! user's entire clipboard history lying around in the clear; that is the one
//! shortcut this module exists to refuse.

use std::io::{BufReader, Chain, Read};

use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::{
    ImportError, ImportSource, JsonRecord, MAX_JSON_DELIMITER_DEPTH, Trailing,
    service::MAX_RAYCONFIG_CIPHERTEXT_BYTES,
};

type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

const KEY_BYTES: usize = 32;
const BLOCK_BYTES: usize = 16;

/// The smallest file that could decrypt to anything: an IV and one block.
const MIN_CONTAINER_BYTES: usize = BLOCK_BYTES + BLOCK_BYTES;

/// The first two bytes of every gzip member.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// The object key whose array holds the clipboard records.
const RECORDS_KEY: &[u8] = b"clipboardHistoryRecords";

/// Buffer between the decompressor and the structural scan.
pub(crate) const SCAN_BUFFER_BYTES: usize = 8 * 1024;

/// Budget for the inflate state. `miniz_oxide` keeps a 32 KiB window; this
/// doubles it so the proof does not sit exactly on the implementation.
pub(crate) const INFLATE_STATE_BYTES: usize = 64 * 1024;

/// Longest key the scanner compares. Fixed so the scan allocates nothing.
const MAX_SCANNED_KEY_BYTES: usize = 64;

const _: () = assert!(RECORDS_KEY.len() <= MAX_SCANNED_KEY_BYTES);

fn export_error(reason: &'static str) -> ImportError {
    ImportError::export(ImportSource::Raycast.as_str(), reason)
}

/// A password, held only as long as it takes to derive a key from it.
///
/// `Debug` says nothing and the bytes are zeroed on drop. That covers this
/// process's own heap; it does not cover the copy the desktop shell already
/// made, because the password arrives over IPC as an immutable JavaScript
/// string that cannot be wiped. Worth having, not worth overclaiming.
pub struct RayconfigSecret {
    password: Vec<u8>,
}

impl RayconfigSecret {
    pub fn new(password: impl Into<Vec<u8>>) -> Self {
        Self {
            password: password.into(),
        }
    }

    fn derive_key(&self) -> RayconfigKey {
        let mut key = [0_u8; KEY_BYTES];
        key.copy_from_slice(&Sha256::digest(&self.password));
        RayconfigKey { key }
    }
}

impl std::fmt::Debug for RayconfigSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RayconfigSecret")
            .finish_non_exhaustive()
    }
}

impl Drop for RayconfigSecret {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

/// The 32 bytes AES takes. Same discipline as the password.
struct RayconfigKey {
    key: [u8; KEY_BYTES],
}

impl Drop for RayconfigKey {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

/// Whether a file could be a rayconfig container at all, judged by length.
///
/// A `.rayconfig` is named rather than sniffed — unlike `.json` and `.csv`,
/// whose schema is read before anything else — so this is the one check
/// available before a password exists.
pub(crate) fn validate_container_length(len: u64) -> Result<(), ImportError> {
    let len = usize::try_from(len).unwrap_or(usize::MAX);
    if len > MAX_RAYCONFIG_CIPHERTEXT_BYTES {
        return Err(ImportError::service("analysis_too_large"));
    }
    if len < MIN_CONTAINER_BYTES || !(len - BLOCK_BYTES).is_multiple_of(BLOCK_BYTES) {
        return Err(export_error("rayconfig_truncated"));
    }
    Ok(())
}

/// Decrypts a container in place and returns the gzip member inside it.
///
/// Decrypting in place matters: a second buffer would double the largest
/// allocation this path makes, and it would leave a copy of the plaintext for
/// the allocator to hand to somebody else.
pub(crate) fn decrypt_container<'a>(
    container: &'a mut [u8],
    secret: &RayconfigSecret,
) -> Result<&'a [u8], ImportError> {
    validate_container_length(container.len() as u64)?;
    let key = secret.derive_key();
    let (iv, body) = container.split_at_mut(BLOCK_BYTES);
    let plain = Aes256CbcDec::new(&key.key.into(), (&*iv).into())
        .decrypt_padded_mut::<Pkcs7>(body)
        // Padding the cipher itself rejects. A wrong password lands here about
        // 255 times in 256; the gzip check below catches the rest.
        .map_err(|_| export_error("rayconfig_password_invalid"))?;
    if plain.len() < GZIP_MAGIC.len() || plain[..GZIP_MAGIC.len()] != GZIP_MAGIC {
        // Padding passed by chance, or this is not a rayconfig. Both are the
        // same thing to the person holding the password, so they get the same
        // answer rather than one that depends on a one-in-256 coin flip.
        return Err(export_error("rayconfig_password_invalid"));
    }
    Ok(plain)
}

/// The reader the Raycast record streamer consumes, positioned at the records.
type RecordsReader<'a> = Chain<&'static [u8], BufReader<GzDecoder<&'a [u8]>>>;

/// Decompresses far enough to reach the records array and hands back a reader
/// positioned at its opening bracket.
///
/// The bracket is put back in front rather than un-read, because the streamer
/// reads it itself and a `Read` cannot rewind.
pub(crate) fn records_reader<'a>(
    gzip_member: &'a [u8],
    manifest_bytes: usize,
) -> Result<RecordsReader<'a>, ImportError> {
    let mut reader = BufReader::with_capacity(SCAN_BUFFER_BYTES, GzDecoder::new(gzip_member));
    seek_records_array(&mut reader, manifest_bytes)?;
    Ok((&b"["[..]).chain(reader))
}

/// Parses the decompressed prefix until the records array opens.
///
/// This walks structure rather than searching for text. The difference is not
/// academic: a full Raycast export carries snippets and quicklinks whose values
/// are arbitrary user text, and one of them may contain the very key we are
/// looking for. A key is only a key when it sits where a key can sit, so the
/// scan tracks which container it is inside and whether it is at a member
/// position. Values are skipped, never read as structure.
fn seek_records_array<R: Read>(reader: &mut R, budget: usize) -> Result<(), ImportError> {
    let mut scanner = StructuralScan::default();
    let mut consumed = 0_usize;
    let mut byte = [0_u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => return Err(export_error("rayconfig_records_missing")),
            Ok(_) => {}
            Err(_) => return Err(export_error("rayconfig_corrupt")),
        }
        consumed += 1;
        if consumed > budget {
            return Err(ImportError::service("analysis_too_large"));
        }
        if scanner.accept(byte[0])? {
            return Ok(());
        }
    }
}

/// Where the scan is, relative to the document's structure.
struct StructuralScan {
    /// Closing delimiter expected for each open container, innermost last.
    expected: [u8; MAX_JSON_DELIMITER_DEPTH],
    depth: usize,
    /// True when the next string would be an object member's key.
    at_member: bool,
    in_string: bool,
    escaped: bool,
    /// Bytes of the current string, while it is still a key candidate.
    key: [u8; MAX_SCANNED_KEY_BYTES],
    key_len: usize,
    key_overflowed: bool,
    /// Set when the last key read was the one we want.
    key_matched: bool,
    /// True between that key's colon and its value.
    awaiting_value: bool,
}

impl Default for StructuralScan {
    fn default() -> Self {
        Self {
            expected: [0; MAX_JSON_DELIMITER_DEPTH],
            depth: 0,
            at_member: false,
            in_string: false,
            escaped: false,
            key: [0; MAX_SCANNED_KEY_BYTES],
            key_len: 0,
            key_overflowed: false,
            key_matched: false,
            awaiting_value: false,
        }
    }
}

impl StructuralScan {
    /// Feeds one byte. Returns true once the records array's `[` is consumed.
    fn accept(&mut self, byte: u8) -> Result<bool, ImportError> {
        if self.in_string {
            self.accept_in_string(byte);
            return Ok(false);
        }
        match byte {
            b'"' => {
                self.in_string = true;
                self.key_len = 0;
                self.key_overflowed = false;
                // Only a string in member position can be a key; a value that
                // spells the key out is still just a value.
                self.key_matched = false;
                self.awaiting_value = false;
            }
            b':' => {
                self.awaiting_value = self.key_matched;
                self.at_member = false;
            }
            b',' => {
                self.at_member = self.innermost_is_object();
                self.key_matched = false;
                self.awaiting_value = false;
            }
            b'{' | b'[' => {
                if byte == b'[' && self.awaiting_value {
                    return Ok(true);
                }
                if self.depth == self.expected.len() {
                    return Err(export_error("rayconfig_records_missing"));
                }
                self.expected[self.depth] = if byte == b'{' { b'}' } else { b']' };
                self.depth += 1;
                self.at_member = byte == b'{';
                self.awaiting_value = false;
                self.key_matched = false;
            }
            b'}' | b']' => {
                if self.depth == 0 || self.expected[self.depth - 1] != byte {
                    return Err(export_error("rayconfig_records_missing"));
                }
                self.depth -= 1;
                self.at_member = false;
                self.awaiting_value = false;
                self.key_matched = false;
            }
            _ => {
                // A scalar value; it cannot open the array we are after.
                self.awaiting_value = false;
            }
        }
        Ok(false)
    }

    fn accept_in_string(&mut self, byte: u8) {
        if self.escaped {
            self.escaped = false;
            self.key_overflowed = true;
            return;
        }
        match byte {
            b'\\' => self.escaped = true,
            b'"' => {
                self.in_string = false;
                // A string only counts as our key when it was in member
                // position, fitted the window, and carried no escapes.
                self.key_matched = self.at_member
                    && !self.key_overflowed
                    && self.key[..self.key_len] == *RECORDS_KEY;
            }
            _ => {
                if self.key_len < self.key.len() {
                    self.key[self.key_len] = byte;
                    self.key_len += 1;
                } else {
                    self.key_overflowed = true;
                }
            }
        }
    }

    fn innermost_is_object(&self) -> bool {
        self.depth > 0 && self.expected[self.depth - 1] == b'}'
    }
}

/// Streams the records out of a decrypted container into the Raycast mapper.
pub(crate) fn stream_rayconfig_records(
    gzip_member: &[u8],
    manifest_bytes: usize,
    record_bytes: usize,
    record: impl FnMut(usize, JsonRecord<'_>) -> Result<bool, ImportError>,
) -> Result<usize, ImportError> {
    let reader = records_reader(gzip_member, manifest_bytes)?;
    crate::stream_json_records_from_reader_with_trailing(
        reader,
        ImportSource::Raycast.as_str(),
        manifest_bytes,
        record_bytes,
        // The array has siblings after it inside the enclosing object, so the
        // bytes past its closing bracket are legitimate rather than a sign the
        // document is not what it claimed to be.
        Trailing::Ignored,
        record,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::{BlockEncryptMut, block_padding::Pkcs7 as Pkcs7Pad};
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;

    type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;

    /// A password that exists only in this file. The real export's password is
    /// never written down anywhere in this repository.
    const TEST_PASSWORD: &str = "synthetic-test-password";

    /// Builds a container the way Raycast does, so the tests exercise the real
    /// shape rather than a convenient one.
    fn build_container(json: &str, password: &str) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(json.as_bytes()).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut key = [0_u8; KEY_BYTES];
        key.copy_from_slice(&Sha256::digest(password.as_bytes()));
        // A fixed IV keeps the fixtures reproducible; the production path reads
        // whatever IV the file carries.
        let iv = [7_u8; BLOCK_BYTES];

        let mut buffer = vec![0_u8; compressed.len() + BLOCK_BYTES];
        let cipher = Aes256CbcEnc::new(&key.into(), &iv.into());
        let encrypted = cipher
            .encrypt_padded_b2b_mut::<Pkcs7Pad>(&compressed, &mut buffer)
            .unwrap()
            .len();
        buffer.truncate(encrypted);

        let mut container = iv.to_vec();
        container.extend_from_slice(&buffer);
        container
    }

    fn records_document(records: &str) -> String {
        format!(
            r#"{{"raycast_version":"1.104.25","builtin_package_clipboardHistory":{{"clipboardHistoryLengthKey":"threeMonths","clipboardHistoryRecords":{records},"clipboardHistoryDisabledApplications":["com.example.one"],"provider_schemaVersion":1}}}}"#
        )
    }

    fn scan_to_array(document: &str) -> Result<String, ImportError> {
        let mut reader = document.as_bytes();
        seek_records_array(&mut reader, crate::service::MAX_IMPORT_MANIFEST_BYTES)?;
        let mut rest = String::new();
        std::io::Read::read_to_string(&mut reader, &mut rest).unwrap();
        Ok(rest)
    }

    #[test]
    fn the_right_password_returns_the_gzip_member() {
        let mut container = build_container(&records_document("[]"), TEST_PASSWORD);
        let secret = RayconfigSecret::new(TEST_PASSWORD);

        let plain = decrypt_container(&mut container, &secret).unwrap();

        assert_eq!(plain[..2], GZIP_MAGIC);
    }

    #[test]
    fn every_wrong_password_is_refused_the_same_way() {
        // Some of these pass PKCS7 by chance — roughly one in 256 — and are
        // caught by the gzip signature instead. The person holding the password
        // must not be told two different stories depending on which one fired.
        let document = records_document("[]");
        for attempt in [
            "",
            "wrong",
            "synthetic-test-passwore",
            "Synthetic-Test-Password",
            "synthetic-test-password ",
            "0",
            "………",
            "synthetic-test-passwordsynthetic-test-password",
        ] {
            let mut container = build_container(&document, TEST_PASSWORD);
            let error = decrypt_container(&mut container, &RayconfigSecret::new(attempt))
                .expect_err("a wrong password must never decrypt");

            assert_eq!(
                error.to_string(),
                ImportError::export(ImportSource::Raycast.as_str(), "rayconfig_password_invalid")
                    .to_string()
            );
        }
    }

    #[test]
    fn a_container_that_cannot_hold_a_block_is_refused_before_any_key_is_derived() {
        for len in [0_usize, 1, 15, 16, 31] {
            let mut container = vec![0_u8; len];
            assert!(decrypt_container(&mut container, &RayconfigSecret::new("x")).is_err());
        }
    }

    #[test]
    fn a_misaligned_container_is_named_rather_than_decrypted() {
        let mut container = vec![0_u8; 16 + 16 + 1];

        let error = decrypt_container(&mut container, &RayconfigSecret::new("x")).unwrap_err();

        assert_eq!(
            error.to_string(),
            ImportError::export(ImportSource::Raycast.as_str(), "rayconfig_truncated").to_string()
        );
    }

    #[test]
    fn a_container_larger_than_the_budget_is_refused_by_length_alone() {
        let over = (MAX_RAYCONFIG_CIPHERTEXT_BYTES + BLOCK_BYTES) as u64;

        assert!(validate_container_length(over).is_err());
        assert!(validate_container_length(MAX_RAYCONFIG_CIPHERTEXT_BYTES as u64).is_ok());
    }

    #[test]
    fn a_secret_never_prints_what_it_holds() {
        let secret = RayconfigSecret::new("sentinel-password-value");

        assert!(!format!("{secret:?}").contains("sentinel-password-value"));
    }

    #[test]
    fn the_scan_finds_the_records_array_nested_inside_a_package() {
        let rest = scan_to_array(&records_document(r#"[{"a":1}]"#)).unwrap();

        assert!(rest.starts_with(r#"{"a":1}]"#));
    }

    #[test]
    fn a_value_that_spells_the_key_out_is_not_mistaken_for_it() {
        // A snippet's text is arbitrary user content. One of them containing
        // the key we are looking for must not reroute the import.
        let document = r#"{"builtin_package_snippets":{"items":[{"text":"{\"clipboardHistoryRecords\":[{\"decoy\":true}]}"}]},"builtin_package_clipboardHistory":{"clipboardHistoryRecords":[{"real":true}]}}"#;

        let rest = scan_to_array(document).unwrap();

        assert!(rest.starts_with(r#"{"real":true}]"#));
        assert!(!rest.contains("decoy"));
    }

    #[test]
    fn a_string_value_equal_to_the_key_is_not_mistaken_for_it() {
        let document =
            r#"{"favourite":"clipboardHistoryRecords","clipboardHistoryRecords":[{"real":true}]}"#;

        let rest = scan_to_array(document).unwrap();

        assert!(rest.starts_with(r#"{"real":true}]"#));
    }

    #[test]
    fn the_key_is_found_whatever_order_its_siblings_come_in() {
        for document in [
            r#"{"p":{"clipboardHistoryRecords":[1],"after":2}}"#,
            r#"{"p":{"before":1,"clipboardHistoryRecords":[1]}}"#,
            r#"{"p":{"before":1,"clipboardHistoryRecords":[1],"after":2}}"#,
        ] {
            assert!(scan_to_array(document).is_ok(), "{document}");
        }
    }

    #[test]
    fn escapes_in_earlier_values_do_not_derail_the_scan() {
        let document =
            r#"{"p":{"quoted":"a \" b \\ c","clipboardHistoryRecords":[{"real":true}]}}"#;

        let rest = scan_to_array(document).unwrap();

        assert!(rest.starts_with(r#"{"real":true}]"#));
    }

    #[test]
    fn a_document_without_the_key_is_refused_rather_than_read_as_empty() {
        let error = scan_to_array(r#"{"p":{"other":[1,2,3]}}"#).unwrap_err();

        assert_eq!(
            error.to_string(),
            ImportError::export(ImportSource::Raycast.as_str(), "rayconfig_records_missing")
                .to_string()
        );
    }

    #[test]
    fn a_key_holding_something_other_than_an_array_does_not_stop_the_scan() {
        let document = r#"{"a":{"clipboardHistoryRecords":null},"b":{"clipboardHistoryRecords":[{"real":true}]}}"#;

        let rest = scan_to_array(document).unwrap();

        assert!(rest.starts_with(r#"{"real":true}]"#));
    }

    #[test]
    fn bounded_rayconfig_prefix_scan_stops_at_its_budget() {
        let padding = "x".repeat(4_096);
        let document = format!(r#"{{"pad":"{padding}","clipboardHistoryRecords":[1]}}"#);
        let mut reader = document.as_bytes();

        let error = seek_records_array(&mut reader, 512).unwrap_err();

        assert_eq!(
            error.to_string(),
            ImportError::service("analysis_too_large").to_string()
        );
    }

    #[test]
    fn bounded_rayconfig_prefix_scan_does_not_allocate() {
        let document = records_document("[]");
        let info = allocation_counter::measure(|| {
            let mut reader = document.as_bytes();
            let _ = seek_records_array(&mut reader, crate::service::MAX_IMPORT_MANIFEST_BYTES);
        });

        assert_eq!(info.count_total, 0);
    }

    #[test]
    fn trailing_siblings_after_the_array_do_not_look_like_a_broken_document() {
        // The real export carries more keys after the records array. Reading it
        // as if the array were the whole document reports a corrupt file.
        let mut container =
            build_container(&records_document(r#"[{"a":1},{"b":2}]"#), TEST_PASSWORD);
        let secret = RayconfigSecret::new(TEST_PASSWORD);
        let plain = decrypt_container(&mut container, &secret).unwrap();

        let mut seen = 0;
        let total = stream_rayconfig_records(plain, 1 << 20, 1 << 16, |_, framed| {
            assert!(matches!(framed, JsonRecord::Complete(_)));
            seen += 1;
            Ok(true)
        })
        .unwrap();

        assert_eq!((total, seen), (2, 2));
    }
}
