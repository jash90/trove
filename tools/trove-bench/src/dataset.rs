//! A synthetic history that is large, varied, and reproducible.
//!
//! Nothing here comes from a real clipboard. Every payload is generated from a
//! seed, so a run can be repeated exactly, and a benchmark can be checked into
//! a repository that must never carry personal data.

use trove_core::{
    CaptureInput, ContentFlags, ContentKind, EventFlags, RepresentationInput, SourceConfidence,
};

/// The seed a whole dataset is derived from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Seed(pub u64);

/// A dataset described rather than held.
///
/// A million records do not fit in memory at once, and holding them would
/// measure the generator's allocator instead of the store. Each record is
/// derived from its index on demand, which also makes any single record
/// reproducible without replaying the ones before it.
pub struct Dataset {
    seed: Seed,
    records: u64,
}

/// The applications a synthetic capture claims to come from.
const SOURCE_APPS: [(&str, &str); 4] = [
    ("dev.synthetic.editor", "Synthetic Editor"),
    ("dev.synthetic.browser", "Synthetic Browser"),
    ("dev.synthetic.terminal", "Synthetic Terminal"),
    ("dev.synthetic.notes", "Synthetic Notes"),
];

/// Words with Polish diacritics, so folded search is exercised at scale.
const POLISH_WORDS: [&str; 12] = [
    "Łódź",
    "zażółć",
    "gęślą",
    "jaźń",
    "ćwierć",
    "źdźbło",
    "poniedziałek",
    "wrzesień",
    "książka",
    "ścieżka",
    "wyjątek",
    "różnica",
];

/// The instant the newest synthetic record claims to have been captured.
const NEWEST_CAPTURE_MS: i64 = 1_775_000_000_000;

/// How far apart two consecutive synthetic captures are.
const CAPTURE_SPACING_MS: i64 = 7_000;

/// How large a synthetic image payload is.
///
/// Above the inline threshold on purpose: an image entry must land in the
/// content-addressed store, or a benchmark would never touch the blob path it
/// is supposed to measure.
const IMAGE_PAYLOAD_BYTES: usize = 6 * 1024;

impl Dataset {
    pub fn new(seed: Seed, records: u64) -> Self {
        Self { seed, records }
    }

    #[cfg(test)]
    pub fn records(&self) -> u64 {
        self.records
    }

    /// Builds the record at one index. Same seed and index, same record.
    pub fn record(&self, index: u64) -> CaptureInput {
        let mut rng = SplitMix64::new(self.seed.0 ^ index.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let kind = pick_kind(rng.next());
        let (app_id, app_name) = SOURCE_APPS[(rng.next() % SOURCE_APPS.len() as u64) as usize];
        let (primary_mime, representation) = payload_for(kind, index, &mut rng);
        CaptureInput {
            captured_at_ms: NEWEST_CAPTURE_MS - (index as i64) * CAPTURE_SPACING_MS,
            kind,
            primary_mime: primary_mime.to_owned(),
            representations: vec![representation],
            source_app_id: Some(app_id.to_owned()),
            source_app_name: Some(app_name.to_owned()),
            source_confidence: SourceConfidence::Declared,
            // One record in a thousand is pinned, so retention has something to
            // refuse to delete.
            pinned: index.is_multiple_of(1_000),
            occurrence_count: 1,
            content_flags: ContentFlags::empty(),
            event_flags: EventFlags::LOCAL_ONLY,
            display_label: match kind {
                ContentKind::Image => Some(format!("Obraz {index}")),
                _ => None,
            },
        }
    }

    /// A fingerprint of the whole dataset, for proving reproducibility.
    pub fn digest(&self) -> u64 {
        let mut digest = FNV_OFFSET;
        for index in 0..self.records {
            let record = self.record(index);
            digest = fnv_mix(digest, record.kind.as_str().as_bytes());
            digest = fnv_mix(digest, record.primary_mime.as_bytes());
            digest = fnv_mix(digest, &record.captured_at_ms.to_le_bytes());
            for representation in &record.representations {
                digest = fnv_mix(digest, representation.format_id.as_bytes());
                if let Some(bytes) = &representation.bytes {
                    digest = fnv_mix(digest, bytes);
                }
            }
        }
        digest
    }

    #[cfg(test)]
    pub fn contains_text(&self, needle: &str) -> bool {
        (0..self.records).any(|index| {
            self.record(index)
                .representations
                .iter()
                .filter_map(|representation| representation.bytes.as_ref())
                .any(|bytes| std::str::from_utf8(bytes).is_ok_and(|text| text.contains(needle)))
        })
    }

    #[cfg(test)]
    pub fn type_count(&self, kind: ContentKind) -> u64 {
        (0..self.records)
            .filter(|index| self.record(*index).kind == kind)
            .count() as u64
    }
}

/// The mix of kinds a real history has: mostly text, some links and code, a
/// handful of images.
fn pick_kind(draw: u64) -> ContentKind {
    match draw % 100 {
        0..=59 => ContentKind::Text,
        60..=79 => ContentKind::Link,
        80..=94 => ContentKind::Code,
        _ => ContentKind::Image,
    }
}

fn payload_for(
    kind: ContentKind,
    index: u64,
    rng: &mut SplitMix64,
) -> (&'static str, RepresentationInput) {
    match kind {
        ContentKind::Link => (
            "text/plain",
            text_representation(
                "text/plain",
                format!("https://example.invalid/{}/{index}", word(rng)),
            ),
        ),
        ContentKind::Code => (
            "text/plain",
            text_representation(
                "text/plain",
                format!(
                    "fn synthetic_{index}() -> usize {{\n    // {}\n    {index}\n}}",
                    word(rng)
                ),
            ),
        ),
        ContentKind::Image => (
            "image/png",
            RepresentationInput {
                format_id: "image/png".to_owned(),
                // Not a decodable PNG, and not meant to be: what is measured is
                // the storage path a binary payload takes, not an image decoder.
                bytes: Some(synthetic_binary(index)),
                missing_ref: None,
            },
        ),
        _ => (
            "text/plain",
            text_representation(
                "text/plain",
                format!(
                    "{} {} — notatka {index} o {}",
                    word(rng),
                    word(rng),
                    word(rng)
                ),
            ),
        ),
    }
}

fn text_representation(format_id: &str, text: String) -> RepresentationInput {
    RepresentationInput {
        format_id: format_id.to_owned(),
        bytes: Some(text.into_bytes()),
        missing_ref: None,
    }
}

fn word(rng: &mut SplitMix64) -> &'static str {
    POLISH_WORDS[(rng.next() % POLISH_WORDS.len() as u64) as usize]
}

/// A payload that is distinct per record, so records never deduplicate into one
/// stored object and the benchmark measures a million of them rather than one.
fn synthetic_binary(index: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(IMAGE_PAYLOAD_BYTES);
    let mut rng = SplitMix64::new(index ^ 0xA5A5_5A5A_A5A5_5A5A);
    while bytes.len() < IMAGE_PAYLOAD_BYTES {
        bytes.extend_from_slice(&rng.next().to_le_bytes());
    }
    bytes.truncate(IMAGE_PAYLOAD_BYTES);
    bytes
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv_mix(mut digest: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        digest ^= u64::from(*byte);
        digest = digest.wrapping_mul(FNV_PRIME);
    }
    digest
}

/// A small deterministic generator, so a dataset needs no random-number crate
/// and repeats exactly on any machine.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generator_is_deterministic_and_contains_polish_code_urls_and_images() {
        let first = Dataset::new(Seed(42), 10_000);
        let second = Dataset::new(Seed(42), 10_000);

        assert_eq!(first.digest(), second.digest());
        assert!(first.contains_text("Łódź"));
        assert!(first.type_count(ContentKind::Image) > 0);
        assert!(first.type_count(ContentKind::Code) > 0);
        assert!(first.type_count(ContentKind::Link) > 0);
    }

    #[test]
    fn a_different_seed_produces_a_different_history() {
        assert_ne!(
            Dataset::new(Seed(42), 1_000).digest(),
            Dataset::new(Seed(43), 1_000).digest()
        );
    }

    #[test]
    fn every_record_is_distinct_so_nothing_collapses_into_one_stored_object() {
        let dataset = Dataset::new(Seed(42), 500);
        let payloads: std::collections::HashSet<_> = (0..dataset.records())
            .map(|index| {
                dataset.record(index).representations[0]
                    .bytes
                    .clone()
                    .expect("synthetic records always carry a payload")
            })
            .collect();

        assert_eq!(payloads.len(), dataset.records() as usize);
    }

    #[test]
    fn captures_run_backwards_in_time_without_collisions() {
        let dataset = Dataset::new(Seed(7), 100);

        for index in 1..dataset.records() {
            assert!(
                dataset.record(index).captured_at_ms < dataset.record(index - 1).captured_at_ms
            );
        }
    }
}
