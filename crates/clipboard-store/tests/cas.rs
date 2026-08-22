use std::{collections::BTreeSet, fs};

use clipboard_core::ContentKind;
use clipboard_store::{CasStore, classify_payload};

fn test_cas() -> (tempfile::TempDir, CasStore) {
    let directory = tempfile::tempdir().unwrap();
    let cas = CasStore::new(directory.path().join("blobs"));
    (directory, cas)
}

#[test]
fn cas_is_content_addressed_and_idempotent() {
    let (_directory, cas) = test_cas();

    let first = cas.put(b"payload").unwrap();
    let second = cas.put(b"payload").unwrap();

    assert_eq!(first.hash, second.hash);
    assert_eq!(first.relpath, second.relpath);
    assert_eq!(cas.read(&first.relpath).unwrap(), b"payload");
}

#[test]
fn cas_rejects_paths_outside_its_content_addressed_root() {
    let (directory, cas) = test_cas();
    let outside = directory.path().join("outside");
    fs::write(&outside, b"must remain").unwrap();

    assert!(cas.read("../outside").is_err());
    assert_eq!(fs::read(outside).unwrap(), b"must remain");
}

#[test]
fn remove_orphans_only_removes_unreferenced_valid_blobs() {
    let (_directory, cas) = test_cas();
    let live = cas.put(b"live").unwrap();
    let orphan = cas.put(b"orphan").unwrap();

    let live_paths = BTreeSet::from([live.relpath.clone(), "../not-a-blob".to_owned()]);
    cas.remove_orphans(&live_paths).unwrap();

    assert_eq!(cas.read(&live.relpath).unwrap(), b"live");
    assert!(cas.read(&orphan.relpath).is_err());
}

#[test]
fn payload_classifier_uses_the_three_storage_tiers() {
    let (_directory, cas) = test_cas();
    let moderately_large = (0..8_192).map(|value| value as u8).collect::<Vec<_>>();

    assert!(matches!(
        classify_payload(ContentKind::Text, b"small", &cas).unwrap(),
        clipboard_store::StoredPayload::Inline(_)
    ));
    assert!(matches!(
        classify_payload(ContentKind::Text, &moderately_large, &cas).unwrap(),
        clipboard_store::StoredPayload::InlineZstd(_)
    ));
    assert!(matches!(
        classify_payload(ContentKind::Image, b"image", &cas).unwrap(),
        clipboard_store::StoredPayload::Cas { .. }
    ));
}
