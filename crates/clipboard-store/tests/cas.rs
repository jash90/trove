use std::{
    fs,
    sync::{Arc, Barrier},
    thread,
};

use clipboard_store::{
    CasBlob, CasError, CasStore, GcStepBudget, ReadOnlyStore, StorageBoundaryLease, StoreConfig,
    StoreError, StoreHandle,
};

fn test_cas() -> (tempfile::TempDir, CasStore) {
    let directory = tempfile::tempdir().unwrap();
    let cas = CasStore::new(directory.path().join("blobs"));
    (directory, cas)
}

#[test]
fn removing_an_object_frees_it_and_leaves_everything_else_alone() {
    let directory = tempfile::tempdir().unwrap();
    let cas = CasStore::new(directory.path().join("blobs"));
    let doomed = cas.put(b"do usuniecia").unwrap();
    let kept = cas.put(b"do zachowania").unwrap();

    let bytes = cas.remove_object(&doomed.relpath).unwrap();

    assert_eq!(bytes, b"do usuniecia".len() as u64);
    assert!(cas.read(&doomed.relpath).is_err());
    assert_eq!(cas.read(&kept.relpath).unwrap(), b"do zachowania");
}

#[test]
fn removing_the_same_object_twice_is_not_an_error() {
    let directory = tempfile::tempdir().unwrap();
    let cas = CasStore::new(directory.path().join("blobs"));
    let blob = cas.put(b"payload").unwrap();

    assert!(cas.remove_object(&blob.relpath).unwrap() > 0);
    // A second pass over the same candidate must not fail the whole run: the
    // file being gone already is the outcome it wanted.
    assert_eq!(cas.remove_object(&blob.relpath).unwrap(), 0);
}

#[test]
fn a_path_outside_the_store_is_refused_before_the_filesystem_is_touched() {
    let directory = tempfile::tempdir().unwrap();
    let cas = CasStore::new(directory.path().join("blobs"));

    for relpath in ["../escape", "ab/../../escape", "not-a-blob", "/absolute"] {
        assert!(
            cas.remove_object(relpath).is_err(),
            "{relpath:?} must never reach remove_file"
        );
    }
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
fn bounded_read_enforces_the_caller_limit_before_payload_sized_allocation() {
    let (_directory, cas) = test_cas();
    let payload = vec![b'x'; 2 * 1024 * 1024];
    let blob = cas.put(&payload).unwrap();

    let allocations = allocation_counter::measure(|| {
        let error = cas
            .read_bounded(&blob.relpath, blob.byte_size, 1024 * 1024)
            .unwrap_err();
        assert!(matches!(error, CasError::ObjectTooLarge));
    });

    assert!(allocations.bytes_max < 128 * 1024, "{allocations:?}");
}

#[test]
fn bounded_read_rejects_oversized_metadata_with_a_stable_limit_error() {
    let (_directory, cas) = test_cas();
    let blob = cas.put(&vec![b'x'; 2 * 1024 * 1024]).unwrap();

    let error = cas
        .read_bounded(&blob.relpath, 32, 1024 * 1024)
        .unwrap_err();

    assert!(matches!(error, CasError::ObjectTooLarge));
}

#[test]
fn bounded_read_rejects_same_size_corruption() {
    let (_directory, cas) = test_cas();
    let blob = cas.put(b"payload").unwrap();
    fs::write(cas.root().join(&blob.relpath), b"corrupt").unwrap();

    let error = cas
        .read_bounded(&blob.relpath, blob.byte_size, 1024)
        .unwrap_err();

    assert!(matches!(error, CasError::CorruptBlob));
}

#[test]
fn leased_bounded_read_rejects_oversized_metadata_before_opening_the_payload() {
    let directory = tempfile::tempdir().unwrap();
    let config = StoreConfig::new(directory.path().join("history.sqlite"))
        .with_blob_root(directory.path().join("blobs"));
    let lease = Arc::new(StorageBoundaryLease::create_writer(&config).unwrap());
    let store = StoreHandle::open(config.with_storage_boundary(lease)).unwrap();
    let cas = store.cas_store().unwrap();
    let blob = cas.put(&vec![b'x'; 2 * 1024 * 1024]).unwrap();

    let error = cas
        .read_bounded(&blob.relpath, 32, 1024 * 1024)
        .unwrap_err();

    assert!(matches!(error, CasError::ObjectTooLarge));
}

#[cfg(unix)]
#[test]
fn bounded_read_rejects_a_replaced_blob_without_following_it() {
    use std::os::unix::fs::symlink;

    let (directory, cas) = test_cas();
    let blob = cas.put(b"payload").unwrap();
    let outside = directory.path().join("outside");
    fs::write(&outside, b"payload").unwrap();
    fs::remove_file(cas.root().join(&blob.relpath)).unwrap();
    symlink(&outside, cas.root().join(&blob.relpath)).unwrap();

    let error = cas
        .read_bounded(&blob.relpath, blob.byte_size, 1024)
        .unwrap_err();

    assert!(matches!(error, CasError::FilesystemBoundary));
}

#[test]
fn cas_rejects_paths_outside_its_content_addressed_root() {
    let (directory, cas) = test_cas();
    let outside = directory.path().join("outside");
    fs::write(&outside, b"must remain").unwrap();

    let error = cas.read("../outside").unwrap_err();
    assert_eq!(error.to_string(), "invalid CAS relative path");
    assert!(!error.to_string().contains("outside"));
    assert_eq!(fs::read(outside).unwrap(), b"must remain");
}

#[test]
fn cas_io_errors_have_stable_path_free_display_text() {
    let (_directory, cas) = test_cas();
    let hash = blake3::hash(b"missing").to_hex().to_string();
    let relpath = format!("{}/{}", &hash[..2], hash);

    let error = cas.read(&relpath).unwrap_err();

    assert_eq!(error.to_string(), "CAS filesystem operation failed");
    assert!(!error.to_string().contains("blobs"));
    assert!(!error.to_string().contains(&relpath));
}

#[test]
fn transparent_store_errors_preserve_path_free_cas_display_text() {
    let error = StoreError::from(CasError::InvalidRelativePath);

    assert_eq!(error.to_string(), "invalid CAS relative path");
}

#[test]
fn cas_and_store_debug_output_omits_paths_hashes_and_blob_names() {
    let sentinel_hash = [173_u8; 32];
    let blob = CasBlob {
        hash: sentinel_hash,
        relpath: "sentinel-shard/sentinel-blob-name".to_owned(),
        byte_size: 42,
    };
    let cas = CasStore::new("/private/sentinel-cas-root");
    let config = StoreConfig::new("/private/sentinel-history.sqlite")
        .with_blob_root("/private/sentinel-config-blobs");

    let rendered = format!("{blob:?} {cas:?} {config:?}");

    assert!(!rendered.contains("sentinel"));
    assert!(!rendered.contains("private"));
    assert!(!rendered.contains(&format!("{sentinel_hash:?}")));
    assert!(!rendered.contains("history.sqlite"));
    assert!(rendered.contains("byte_size: 42"));
}

#[test]
fn cas_rejects_corrupt_existing_blobs_without_leaking_paths_or_hashes() {
    let (_directory, cas) = test_cas();
    let blob = cas.put(b"payload").unwrap();
    fs::write(cas.root().join(&blob.relpath), b"corrupt").unwrap();

    let read_error = cas.read(&blob.relpath).unwrap_err();
    let put_error = cas.put(b"payload").unwrap_err();

    assert_eq!(read_error.to_string(), "CAS blob integrity check failed");
    assert_eq!(put_error.to_string(), "CAS blob integrity check failed");
    assert!(!read_error.to_string().contains(&blob.relpath));
    assert!(!put_error.to_string().contains(&blob.relpath));
}

#[test]
fn leased_put_streaming_validation_rejects_a_corrupt_existing_blob() {
    let directory = tempfile::tempdir().unwrap();
    let config = StoreConfig::new(directory.path().join("history.sqlite"))
        .with_blob_root(directory.path().join("blobs"));
    let writer_lease = Arc::new(StorageBoundaryLease::create_writer(&config).unwrap());
    let store = StoreHandle::open(config.clone().with_storage_boundary(writer_lease)).unwrap();
    drop(store);
    let read_only_lease = Arc::new(StorageBoundaryLease::open_read_only(&config).unwrap());
    let read_only =
        ReadOnlyStore::open_existing(config.clone().with_storage_boundary(read_only_lease))
            .unwrap();
    let cas = read_only.cas_store().unwrap();
    let blob = cas.put(b"synthetic leased payload").unwrap();
    fs::write(cas.root().join(&blob.relpath), b"corrupt").unwrap();

    let error = cas.put(b"synthetic leased payload").unwrap_err();

    assert_eq!(error.to_string(), "CAS blob integrity check failed");
    assert!(!error.to_string().contains(&blob.relpath));
}

#[test]
fn concurrent_puts_are_idempotent() {
    let (_directory, cas) = test_cas();
    let cas = Arc::new(cas);
    let barrier = Arc::new(Barrier::new(8));
    let handles = (0..8)
        .map(|_| {
            let cas = Arc::clone(&cas);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                cas.put(b"concurrent payload").unwrap()
            })
        })
        .collect::<Vec<_>>();
    let blobs = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();

    assert!(blobs.windows(2).all(|pair| pair[0] == pair[1]));
    assert_eq!(cas.read(&blobs[0].relpath).unwrap(), b"concurrent payload");
}

fn assert_gc_reports_unreferenced_valid_blobs_without_removing_them(cas: &CasStore) {
    let live_bytes = b"synthetic live";
    let orphan_bytes = b"synthetic orphan";
    let live = cas.put(live_bytes).unwrap();
    let orphan = cas.put(orphan_bytes).unwrap();

    let mut session = cas.start_gc().unwrap();
    let mut total_orphan_candidates = 0;
    loop {
        let step = session
            .step(GcStepBudget::new(1), |relpath| Ok(relpath == live.relpath))
            .unwrap();
        assert!(step.examined_entries <= 1);
        total_orphan_candidates += step.orphan_candidates;
        if step.complete {
            break;
        }
    }

    assert_eq!(total_orphan_candidates, 1);
    assert_eq!(cas.read(&live.relpath).unwrap(), live_bytes);
    assert_eq!(cas.read(&orphan.relpath).unwrap(), orphan_bytes);
}

#[test]
fn gc_session_reports_unreferenced_valid_blobs_without_removing_them() {
    let (_directory, cas) = test_cas();

    assert_gc_reports_unreferenced_valid_blobs_without_removing_them(&cas);
}

#[test]
fn leased_gc_session_reports_unreferenced_valid_blobs_without_removing_them() {
    let directory = tempfile::tempdir().unwrap();
    let config = StoreConfig::new(directory.path().join("history.sqlite"))
        .with_blob_root(directory.path().join("blobs"));
    let lease = Arc::new(StorageBoundaryLease::create_writer(&config).unwrap());
    let store = StoreHandle::open(config.clone().with_storage_boundary(lease)).unwrap();
    drop(store);
    let read_only_lease = Arc::new(StorageBoundaryLease::open_read_only(&config).unwrap());
    let read_only =
        ReadOnlyStore::open_existing(config.with_storage_boundary(read_only_lease)).unwrap();
    let cas = read_only.cas_store().unwrap();

    assert_gc_reports_unreferenced_valid_blobs_without_removing_them(&cas);
}

#[test]
fn gc_session_preserves_uncertain_corrupt_blobs() {
    let (_directory, cas) = test_cas();
    let blob = cas.put(b"orphan").unwrap();
    fs::write(cas.root().join(&blob.relpath), b"corrupt").unwrap();

    let mut session = cas.start_gc().unwrap();
    while !session
        .step(GcStepBudget::new(2), |_| Ok(false))
        .unwrap()
        .complete
    {}
    assert!(cas.root().join(blob.relpath).exists());
}

#[cfg(unix)]
mod unix_symlink_tests {
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    use clipboard_store::GcStepBudget;

    use super::test_cas;

    #[test]
    fn put_rejects_a_symlinked_cas_root() {
        let (directory, cas) = test_cas();
        let external = directory.path().join("external");
        fs::create_dir(&external).unwrap();
        symlink(&external, cas.root()).unwrap();

        let error = cas.put(b"payload").unwrap_err();

        assert_eq!(error.to_string(), "CAS filesystem boundary is invalid");
        assert!(fs::read_dir(external).unwrap().next().is_none());
    }

    #[test]
    fn put_rejects_a_symlinked_temporary_directory() {
        let (directory, cas) = test_cas();
        fs::create_dir_all(cas.root()).unwrap();
        fs::set_permissions(cas.root(), fs::Permissions::from_mode(0o700)).unwrap();
        let external = directory.path().join("external");
        fs::create_dir(&external).unwrap();
        symlink(&external, cas.root().join(".tmp")).unwrap();

        let error = cas.put(b"payload").unwrap_err();

        assert_eq!(error.to_string(), "CAS filesystem boundary is invalid");
        assert!(fs::read_dir(external).unwrap().next().is_none());
    }

    #[test]
    fn put_rejects_a_symlinked_shard_directory() {
        let (directory, cas) = test_cas();
        let hash = blake3::hash(b"payload").to_hex().to_string();
        fs::create_dir_all(cas.root()).unwrap();
        fs::set_permissions(cas.root(), fs::Permissions::from_mode(0o700)).unwrap();
        let external = directory.path().join("external");
        fs::create_dir(&external).unwrap();
        symlink(&external, cas.root().join(&hash[..2])).unwrap();

        let error = cas.put(b"payload").unwrap_err();

        assert_eq!(error.to_string(), "CAS filesystem boundary is invalid");
        assert!(fs::read_dir(external).unwrap().next().is_none());
    }

    #[test]
    fn read_and_put_reject_a_symlinked_blob_even_when_it_has_matching_bytes() {
        let (directory, cas) = test_cas();
        let blob = cas.put(b"payload").unwrap();
        fs::remove_file(cas.root().join(&blob.relpath)).unwrap();
        let external = directory.path().join("external-blob");
        fs::write(&external, b"payload").unwrap();
        symlink(&external, cas.root().join(&blob.relpath)).unwrap();

        let read_error = cas.read(&blob.relpath).unwrap_err();
        let put_error = cas.put(b"payload").unwrap_err();

        assert_eq!(read_error.to_string(), "CAS filesystem boundary is invalid");
        assert_eq!(put_error.to_string(), "CAS filesystem boundary is invalid");
    }

    #[test]
    fn cleanup_rejects_a_symlinked_blob_without_touching_its_target() {
        let (directory, cas) = test_cas();
        let blob = cas.put(b"orphan").unwrap();
        fs::remove_file(cas.root().join(&blob.relpath)).unwrap();
        let external = directory.path().join("external-blob");
        fs::write(&external, b"must remain").unwrap();
        symlink(&external, cas.root().join(&blob.relpath)).unwrap();

        let mut session = cas.start_gc().unwrap();
        while !session
            .step(GcStepBudget::new(2), |_| Ok(false))
            .unwrap()
            .complete
        {}
        assert_eq!(fs::read(external).unwrap(), b"must remain");
    }
}
