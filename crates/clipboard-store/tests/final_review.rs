use std::{
    collections::BTreeSet,
    process::Command,
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use clipboard_core::{
    CaptureInput, ContentFlags, ContentKind, EventFlags, RepresentationInput, SourceConfidence,
};
use clipboard_store::{
    CAS_VERIFY_BUFFER_BYTES, CasError, CasStore, GcStepBudget, MAX_CAS_OBJECT_BYTES,
    MAX_STORE_READERS, ReadOnlyStore, StoreConfig, StoreError, StoreHandle,
};

fn capture(primary: &[u8], captured_at_ms: i64) -> CaptureInput {
    CaptureInput {
        captured_at_ms,
        kind: ContentKind::Text,
        primary_mime: "text/plain".to_owned(),
        representations: vec![RepresentationInput {
            format_id: "public.utf8-plain-text".to_owned(),
            bytes: Some(primary.to_vec()),
            missing_ref: None,
        }],
        source_app_id: None,
        source_app_name: None,
        source_confidence: SourceConfidence::Unknown,
        pinned: false,
        occurrence_count: 1,
        content_flags: ContentFlags::empty(),
        event_flags: EventFlags::empty(),
    }
}

fn open_store() -> (tempfile::TempDir, StoreHandle) {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    (directory, store)
}

#[tokio::test]
async fn canonical_events_retain_exact_ordered_representations() {
    let (_directory, store) = open_store();
    let mut first = capture(b"line one\r\nline two", 1_000);
    first.representations.push(RepresentationInput {
        format_id: "text/html".to_owned(),
        bytes: Some(b"<p>synthetic one</p>".to_vec()),
        missing_ref: None,
    });
    let mut second = capture(b"line one\nline two", 2_000);
    second.representations.push(RepresentationInput {
        format_id: "text/html".to_owned(),
        bytes: Some(b"<p>synthetic two</p>".to_vec()),
        missing_ref: None,
    });

    let first = store.ingest(first).await.unwrap();
    let second = store.ingest(second).await.unwrap();
    assert_eq!(first.content_id, second.content_id);

    let representations = store
        .with_reader(|connection| {
            let mut statement = connection.prepare(
                "SELECT er.event_id, er.ordinal, er.format_id, rp.inline_payload
                 FROM event_representation er
                 JOIN raw_payload rp ON rp.raw_payload_id = er.raw_payload_id
                 ORDER BY er.event_id, er.ordinal",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap();

    assert_eq!(
        representations,
        vec![
            (
                first.event_id,
                0,
                "public.utf8-plain-text".to_owned(),
                b"line one\r\nline two".to_vec(),
            ),
            (
                first.event_id,
                1,
                "text/html".to_owned(),
                b"<p>synthetic one</p>".to_vec(),
            ),
            (
                second.event_id,
                0,
                "public.utf8-plain-text".to_owned(),
                b"line one\nline two".to_vec(),
            ),
            (
                second.event_id,
                1,
                "text/html".to_owned(),
                b"<p>synthetic two</p>".to_vec(),
            ),
        ]
    );
}

#[tokio::test]
async fn equivalent_unicode_events_share_content_but_not_raw_storage_identity() {
    let (_directory, store) = open_store();
    let composed = store.ingest(capture("é".as_bytes(), 1_000)).await.unwrap();
    let decomposed = store
        .ingest(capture("e\u{301}".as_bytes(), 2_000))
        .await
        .unwrap();

    assert_eq!(composed.content_id, decomposed.content_id);
    let (raw_payloads, links): (i64, i64) = store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT (SELECT count(*) FROM raw_payload),
                        (SELECT count(*) FROM event_representation)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
        })
        .unwrap();
    assert_eq!((raw_payloads, links), (2, 2));
}

#[tokio::test]
async fn identical_event_bytes_reuse_one_raw_payload() {
    let (_directory, store) = open_store();
    store
        .ingest(capture(b"same synthetic bytes", 1_000))
        .await
        .unwrap();
    store
        .ingest(capture(b"same synthetic bytes", 2_000))
        .await
        .unwrap();

    let (raw_payloads, links): (i64, i64) = store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT (SELECT count(*) FROM raw_payload),
                        (SELECT count(*) FROM event_representation)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
        })
        .unwrap();
    assert_eq!((raw_payloads, links), (1, 2));
}

#[tokio::test]
async fn missing_auxiliary_representation_is_event_local() {
    let (_directory, store) = open_store();
    let mut first = capture(b"shared synthetic primary", 1_000);
    first.representations.push(RepresentationInput {
        format_id: "text/html".to_owned(),
        bytes: None,
        missing_ref: Some("synthetic-reference".to_owned()),
    });
    let second = capture(b"shared synthetic primary", 2_000);
    let first = store.ingest(first).await.unwrap();
    let second = store.ingest(second).await.unwrap();

    assert_eq!(first.content_id, second.content_id);
    let (missing_first, missing_second, raw_payloads): (i64, i64, i64) = store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT
                   (SELECT count(*) FROM event_representation
                    WHERE event_id = ?1 AND missing_ref IS NOT NULL AND raw_payload_id IS NULL),
                   (SELECT count(*) FROM event_representation
                    WHERE event_id = ?2 AND missing_ref IS NOT NULL),
                   (SELECT count(*) FROM raw_payload)",
                [first.event_id, second.event_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
        })
        .unwrap();
    assert_eq!((missing_first, missing_second, raw_payloads), (1, 0, 1));
}

#[test]
fn repeated_opens_share_one_runtime_and_reject_blob_configuration_mismatch() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("history.sqlite");
    let first = StoreHandle::open(StoreConfig::new(&database)).unwrap();
    let second = StoreHandle::open(StoreConfig::new(&database)).unwrap();
    assert!(first.shares_runtime_with(&second));

    let alternate_blob_root = directory.path().join("alternate.blobs");
    std::fs::create_dir(&alternate_blob_root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&alternate_blob_root, std::fs::Permissions::from_mode(0o700))
            .unwrap();
    }
    let error =
        match StoreHandle::open(StoreConfig::new(&database).with_blob_root(&alternate_blob_root)) {
            Ok(_) => panic!("mismatched runtime configuration unexpectedly opened"),
            Err(error) => error,
        };
    assert!(matches!(error, StoreError::RuntimeConfigurationMismatch));
    assert_eq!(error.to_string(), "store_runtime_configuration_mismatch");
}

#[test]
fn concurrent_opens_share_one_runtime_and_last_drop_allows_a_clean_reopen() {
    const OPENERS: usize = 8;

    let directory = tempfile::tempdir().unwrap();
    let database = Arc::new(directory.path().join("history.sqlite"));
    drop(StoreHandle::open(StoreConfig::new(database.as_path())).unwrap());
    let ready = Arc::new(Barrier::new(OPENERS + 1));
    let mut workers = Vec::new();
    for _ in 0..OPENERS {
        let database = Arc::clone(&database);
        let ready = Arc::clone(&ready);
        workers.push(thread::spawn(move || {
            ready.wait();
            StoreHandle::open(StoreConfig::new(database.as_path())).unwrap()
        }));
    }
    ready.wait();
    let handles = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    for handle in &handles[1..] {
        assert!(handles[0].shares_runtime_with(handle));
    }
    drop(handles);

    let reopened = StoreHandle::open(StoreConfig::new(database.as_path())).unwrap();
    assert_eq!(reopened.stats().unwrap().event_count, 0);
}

#[test]
fn open_racing_with_last_handle_drop_never_observes_a_partial_runtime() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("history.sqlite");

    for _ in 0..32 {
        let store = StoreHandle::open(StoreConfig::new(&database)).unwrap();
        let release = Arc::new(Barrier::new(2));
        let drop_release = Arc::clone(&release);
        let closer = thread::spawn(move || {
            drop_release.wait();
            drop(store);
        });
        release.wait();
        thread::yield_now();
        let reopened = StoreHandle::open(StoreConfig::new(&database)).unwrap();
        closer.join().unwrap();
        assert_eq!(reopened.stats().unwrap().event_count, 0);
        drop(reopened);
    }
}

#[test]
fn reader_gate_caps_concurrency_releases_permits_and_allows_reentrancy() {
    let (_directory, store) = open_store();
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let arrivals = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Barrier::new(MAX_STORE_READERS + 1));
    let mut workers = Vec::new();
    for _ in 0..(MAX_STORE_READERS * 2) {
        let store = store.clone();
        let active = Arc::clone(&active);
        let peak = Arc::clone(&peak);
        let arrivals = Arc::clone(&arrivals);
        let release = Arc::clone(&release);
        workers.push(thread::spawn(move || {
            store
                .with_reader(|connection| {
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(current, Ordering::SeqCst);
                    let position = arrivals.fetch_add(1, Ordering::SeqCst);
                    if position < MAX_STORE_READERS {
                        release.wait();
                    }
                    connection.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))?;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
                .unwrap();
        }));
    }
    release.wait();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(peak.load(Ordering::SeqCst), MAX_STORE_READERS);

    let nested = store.clone();
    store
        .with_reader(|_| {
            assert_eq!(
                nested
                    .with_reader(|connection| {
                        connection.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                    })
                    .unwrap(),
                1
            );
            Ok(())
        })
        .unwrap();
    let error = store
        .with_reader::<()>(|_| Err(rusqlite::Error::InvalidQuery))
        .unwrap_err();
    assert!(matches!(error, StoreError::Database(_)));
    assert_eq!(store.stats().unwrap().event_count, 0);
}

#[cfg(unix)]
#[test]
fn unsafe_existing_private_storage_is_rejected_without_repair_or_path_disclosure() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("history.sqlite");
    drop(StoreHandle::open(StoreConfig::new(&database)).unwrap());
    std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o644)).unwrap();

    let writer_error = match StoreHandle::open(StoreConfig::new(&database)) {
        Ok(_) => panic!("unsafe writer storage unexpectedly opened"),
        Err(error) => error,
    };
    let reader_error = match ReadOnlyStore::open_existing(StoreConfig::new(&database)) {
        Ok(_) => panic!("unsafe read-only storage unexpectedly opened"),
        Err(error) => error,
    };
    for error in [writer_error, reader_error] {
        assert!(matches!(error, StoreError::PrivateStorageUnavailable));
        assert_eq!(error.to_string(), "private_storage_unavailable");
        assert!(!error.to_string().contains("history.sqlite"));
    }
    assert_eq!(
        std::fs::metadata(&database).unwrap().permissions().mode() & 0o777,
        0o644
    );
}

#[cfg(unix)]
#[test]
fn only_a_same_owner_completely_empty_root_is_hardened_during_provisioning() {
    use std::os::unix::fs::PermissionsExt;

    let empty = tempfile::tempdir().unwrap();
    std::fs::set_permissions(empty.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let store = StoreHandle::open(StoreConfig::new(empty.path().join("history.sqlite"))).unwrap();
    assert_eq!(
        std::fs::metadata(empty.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    drop(store);

    let nonempty = tempfile::tempdir().unwrap();
    std::fs::write(nonempty.path().join("unmanaged-entry"), b"synthetic").unwrap();
    std::fs::set_permissions(nonempty.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let error = match StoreHandle::open(StoreConfig::new(nonempty.path().join("history.sqlite"))) {
        Ok(_) => panic!("nonempty unsafe root unexpectedly provisioned"),
        Err(error) => error,
    };
    assert!(matches!(error, StoreError::PrivateStorageUnavailable));
    assert_eq!(error.to_string(), "private_storage_unavailable");
    assert_eq!(
        std::fs::metadata(nonempty.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[cfg(unix)]
#[test]
fn private_creation_requests_owner_only_modes_under_a_permissive_umask() {
    use std::os::unix::fs::PermissionsExt;

    const CHILD_MARKER: &str = "CLIPBOARD_PRIVATE_MODE_CHILD";
    if std::env::var_os(CHILD_MARKER).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "private_creation_requests_owner_only_modes_under_a_permissive_umask",
                "--test-threads=1",
            ])
            .env(CHILD_MARKER, "1")
            .output()
            .unwrap();
        assert!(output.status.success());
        return;
    }

    struct RestoreUmask(rustix::fs::Mode);
    impl Drop for RestoreUmask {
        fn drop(&mut self) {
            rustix::process::umask(self.0);
        }
    }

    let _restore = RestoreUmask(rustix::process::umask(rustix::fs::Mode::empty()));
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let database = directory.path().join("history.sqlite");
    let store = StoreHandle::open(StoreConfig::new(&database)).unwrap();
    let blob_root = database.with_extension("blobs");
    assert_eq!(
        std::fs::metadata(&database).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(&blob_root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let sidecars = ["-wal", "-shm", "-journal"]
        .into_iter()
        .filter_map(|suffix| {
            let mut name = database.as_os_str().to_os_string();
            name.push(suffix);
            std::fs::metadata(std::path::PathBuf::from(name)).ok()
        })
        .collect::<Vec<_>>();
    assert!(!sidecars.is_empty());
    assert!(
        sidecars
            .iter()
            .all(|metadata| metadata.permissions().mode() & 0o777 == 0o600)
    );

    let direct = CasStore::new(directory.path().join("direct-blobs"));
    let blob = direct.put(b"synthetic mode bytes").unwrap();
    let shard = direct.root().join(&blob.relpath[..2]);
    let object = direct.root().join(&blob.relpath);
    for private_directory in [direct.root(), shard.as_path()] {
        assert_eq!(
            std::fs::metadata(private_directory)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    assert_eq!(
        std::fs::metadata(object).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(store);
}

#[test]
fn cas_verification_is_streaming_and_rejects_oversized_objects() {
    assert_eq!(CAS_VERIFY_BUFFER_BYTES, 64 * 1024);
    let directory = tempfile::tempdir().unwrap();
    let cas = CasStore::new(directory.path().join("blobs"));
    let bytes = vec![7_u8; CAS_VERIFY_BUFFER_BYTES * 3 + 17];
    let blob = cas.put(&bytes).unwrap();
    let verified = cas.verify(&blob.relpath, bytes.len() as u64).unwrap();
    assert_eq!(verified.byte_size, bytes.len() as u64);

    let oversized = vec![0_u8; MAX_CAS_OBJECT_BYTES + 1];
    let error = cas.put(&oversized).unwrap_err();
    assert!(matches!(error, CasError::ObjectTooLarge));
    assert_eq!(error.to_string(), "cas_object_too_large");
}

#[test]
fn gc_session_charges_invalid_entries_and_resumes_with_a_strict_step_budget() {
    let directory = tempfile::tempdir().unwrap();
    let cas = CasStore::new(directory.path().join("blobs"));
    let live = cas.put(b"synthetic live").unwrap();
    let orphan = cas.put(b"synthetic orphan").unwrap();
    std::fs::write(cas.root().join("unmanaged-entry"), b"synthetic unmanaged").unwrap();
    let live_paths = BTreeSet::from([live.relpath.clone()]);
    let mut session = cas.start_gc().unwrap();
    let mut steps = Vec::new();
    let mut total_orphan_candidates = 0;
    loop {
        let step = session
            .step(GcStepBudget::new(1), |relpath| {
                Ok(live_paths.contains(relpath))
            })
            .unwrap();
        assert!(step.examined_entries <= 1);
        total_orphan_candidates += step.orphan_candidates;
        let complete = step.complete;
        steps.push(step);
        if complete {
            break;
        }
    }
    assert!(steps.len() >= 3);
    assert_eq!(total_orphan_candidates, 1);
    assert_eq!(cas.read(&live.relpath).unwrap(), b"synthetic live");
    assert_eq!(cas.read(&orphan.relpath).unwrap(), b"synthetic orphan");
    assert!(cas.root().join("unmanaged-entry").exists());
}
