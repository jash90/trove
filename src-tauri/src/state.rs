use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::Manager;
use trove_import::ImportService;
use trove_launcher::AppBundle;
use trove_store::{DATABASE_FILENAME, StoreConfig, StoreHandle};

/// Announced when a background launcher scan produced a catalog different
/// from the one before it. The palette refetches on this signal — and only
/// on it, which is also the loop guard: a refetch that finds the catalog
/// unchanged announces nothing, so the refresh chain ends by itself.
pub const APPS_CATALOG_CHANGED_EVENT: &str = "apps-catalog-changed";

/// One rendered application icon, in the shape the bridge carries images.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppIconDto {
    pub mime_type: String,
    pub base64: String,
}

/// The application catalog and the roots it was scanned from.
#[derive(Clone)]
pub struct LauncherState {
    /// The roots a scan walks, with each root's menu-bar-agent policy.
    scan_roots: Arc<[trove_launcher::ScanRoot]>,
    /// The same roots as plain paths — what launch and icon validation
    /// accepts, flattened once so neither caller maps per request.
    roots: Arc<[PathBuf]>,
    cached: Arc<Mutex<Option<CachedCatalog>>>,
    /// Whether a background scan is running. A palette that gains focus
    /// repeatedly — the shortcut, the menu bar, Cmd-Tab — must not queue a
    /// scan per focus; the one already running answers for all of them.
    scan_in_flight: Arc<AtomicBool>,
    /// Rendered icons, remembered per canonical path. An icon is a fact
    /// about a bundle that changes only with the bundle, so it outlives any
    /// catalog refresh; the map grows with the distinct applications actually
    /// shown, which the virtualized list keeps to a handful at a time.
    icons: Arc<Mutex<HashMap<String, AppIconDto>>>,
}

struct CachedCatalog {
    catalog: Vec<AppBundle>,
}

impl LauncherState {
    /// Accepts plain paths (strict policy) or annotated [`ScanRoot`]s; the
    /// production roots come from `trove_launcher::default_scan_roots`,
    /// tests inject temporary directories and inherit the strict policy.
    pub fn scanning(roots: Vec<impl Into<trove_launcher::ScanRoot>>) -> Self {
        let scan_roots: Vec<trove_launcher::ScanRoot> = roots.into_iter().map(Into::into).collect();
        let paths = scan_roots
            .iter()
            .map(|root| root.path.clone())
            .collect::<Vec<PathBuf>>();
        Self {
            scan_roots: Arc::from(scan_roots),
            roots: Arc::from(paths),
            cached: Arc::new(Mutex::new(None)),
            scan_in_flight: Arc::new(AtomicBool::new(false)),
            icons: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The roots launch validation accepts, the same ones the scan walked.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// The cached catalog, whatever its age. A refresh swaps the catalog in
    /// only once the fresh one is complete, so this never observes a scan in
    /// progress — the answer is either the whole previous catalog or none.
    pub fn snapshot(&self) -> Option<Vec<AppBundle>> {
        self.cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|entry| entry.catalog.clone())
    }

    /// Scans synchronously and replaces the cache. The cold path: the first
    /// palette opening has nothing to show yet and pays the scan once. The
    /// cache lock is held across the scan, so concurrent cold callers
    /// serialize — and the one that waited on the lock takes the answer the
    /// one that held it already wrote, rather than scanning again.
    pub fn refresh(&self) -> Vec<AppBundle> {
        let mut cached = self
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = cached.as_ref() {
            return entry.catalog.clone();
        }
        let catalog = trove_launcher::scan_applications(&self.scan_roots);
        *cached = Some(CachedCatalog {
            catalog: catalog.clone(),
        });
        catalog
    }

    /// Scans on a background thread and swaps the result into the cache once
    /// complete, leaving [`LauncherState::snapshot`] answerable throughout.
    ///
    /// Returns `false` — without spawning anything — when a scan is already
    /// running: that one will finish this caller's work, and its completion
    /// callback speaks for both. `on_done` receives whether the fresh catalog
    /// differs from the one it replaced, so the caller can announce changes
    /// and stay silent otherwise; the flag is released before the callback
    /// runs, so a refetch the callback triggers can start its own scan.
    pub fn refresh_in_background(&self, on_done: impl FnOnce(bool) + Send + 'static) -> bool {
        if self
            .scan_in_flight
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return false;
        }
        let roots = Arc::clone(&self.scan_roots);
        let cached = Arc::clone(&self.cached);
        let scan_in_flight = Arc::clone(&self.scan_in_flight);
        std::thread::spawn(move || {
            let catalog = trove_launcher::scan_applications(&roots);
            let changed = {
                let mut guard = cached
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let changed = match guard.as_ref() {
                    Some(previous) => previous.catalog != catalog,
                    // Nothing to replace: everything about the catalog is new.
                    None => true,
                };
                *guard = Some(CachedCatalog { catalog });
                changed
            };
            scan_in_flight.store(false, Ordering::Release);
            on_done(changed);
        });
        true
    }

    /// Returns the rendered icon for a catalog path, remembered after the
    /// first answer. Validation refusals are the caller's to report — they
    /// say the path was never listed, which no cache should paper over.
    pub fn icon(
        &self,
        canonical: &str,
        render: impl FnOnce() -> Option<AppIconDto>,
    ) -> Option<AppIconDto> {
        let mut icons = self
            .icons
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(icon) = icons.get(canonical) {
            return Some(icon.clone());
        }
        let rendered = render()?;
        icons.insert(canonical.to_owned(), rendered.clone());
        Some(rendered)
    }
}

pub struct AppState {
    pub store: StoreHandle,
    pub importer: ImportService,
    /// Guards the one network path in the application against asking twice at
    /// once and against a host that has stopped answering. Shared
    /// mutable state behind a lock rather than globals, so tests can hold a
    /// coordinator of their own.
    pub previews: Mutex<crate::links::FetchCoordinator>,
    /// The launcher's application catalog. Scanned lazily on the first ask
    /// and refreshed in the background on every later one, because the
    /// palette is hidden most of the time and a startup scan would cost
    /// every launch for a window nobody opened.
    pub launcher: LauncherState,
}

impl AppState {
    pub fn open_data_dir(data_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        // The layout comes from trove-store so the importer and this
        // application can never disagree about where the database lives.
        let store = StoreHandle::open(StoreConfig::in_data_dir(data_dir))?;
        let importer = ImportService::new(store.clone())
            .map_err(|_| anyhow::anyhow!("import service initialization failed"))?;
        Ok(Self {
            store,
            importer,
            previews: Mutex::new(crate::links::FetchCoordinator::default()),
            launcher: LauncherState::scanning(trove_launcher::default_scan_roots()),
        })
    }
}

pub fn resolve_data_dir(app: &tauri::AppHandle) -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("TROVE_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    let current = app.path().app_data_dir()?;
    Ok(adopt_legacy_data_dir(current, legacy_data_dir(app)))
}

/// Where the history lived when the application was called Clipboard History.
///
/// The data directory is named after the bundle identifier, so renaming the
/// application pointed it at an empty directory beside a full one. The history
/// is the whole point of the application and it is not backed up anywhere, so
/// the old directory is adopted rather than abandoned.
fn legacy_data_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    let current = app.path().app_data_dir().ok()?;
    Some(current.parent()?.join(LEGACY_IDENTIFIER))
}

const LEGACY_IDENTIFIER: &str = "pl.local.clipboard-history";

/// Whether a directory is the one holding a history, rather than merely there.
///
/// The question the migration has to answer is not "does this path exist" —
/// an empty directory is trivially made, by a run that got as far as resolving
/// its data directory and no further — but "is the history in it". Deciding on
/// existence alone opened an empty store beside a full one and reported an
/// empty history, which looks exactly like losing it.
fn holds_history(dir: &Path) -> bool {
    dir.join(DATABASE_FILENAME).is_file()
}

/// Picks the directory to open, moving the old one across when it is the one
/// with the history in it.
///
/// A rename within a volume is atomic, so this either happens completely or not
/// at all. When it cannot happen — a different volume, a permission, a current
/// directory holding something this has no business deleting — the legacy path
/// is returned as it stands. Reading the history from its old home is worth
/// more than a tidy directory name, and copying gigabytes to get the name would
/// risk the copy failing halfway.
fn adopt_legacy_data_dir(current: PathBuf, legacy: Option<PathBuf>) -> PathBuf {
    if holds_history(&current) {
        return current;
    }
    let Some(legacy) = legacy else {
        return current;
    };
    if !holds_history(&legacy) {
        return current;
    }
    // `rename` will not land on an occupied directory, and an empty one left by
    // an earlier run is the whole case this exists for. `remove_dir` refuses
    // anything that is not empty, which is exactly the guard wanted here: if
    // something is in there, it is not ours to delete, and the history is read
    // where it already lies instead.
    if current.exists() && std::fs::remove_dir(&current).is_err() {
        return legacy;
    }
    match std::fs::rename(&legacy, &current) {
        Ok(()) => current,
        Err(_) => legacy,
    }
}

/// How long a starting process waits for the instance lock before giving up.
///
/// An update restarts the application by starting the new process first and
/// only then exiting the old one, so for a moment the old process still holds
/// the lock. Three seconds covers that handoff without leaving a genuine
/// second launch hanging around for long before it steps aside.
pub const INSTANCE_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Proof that this process is the one running against the history.
///
/// The single-instance plugin is what normally turns a second launch away,
/// but it decides by a socket in `/tmp`, which anything can delete, and it
/// gives up quietly when it cannot connect. Two processes writing one history
/// double every entry, and one's blob collection can reclaim a blob the other
/// has written and not yet committed. An advisory lock held for the life of
/// the process is the guarantee that does not depend on the socket: the
/// kernel releases it when the process ends, however it ends, so a crash can
/// never leave it stuck.
///
/// It is taken in the application's setup and nowhere else. Opening a store
/// is not where it belongs: tests reopen one data directory many times over,
/// and the store has no business deciding which process owns the history.
pub struct InstanceLock {
    // Never read: holding the open descriptor is what holds the lock.
    _file: std::fs::File,
}

impl InstanceLock {
    /// Takes the lock at `path`, retrying until `wait` has passed.
    ///
    /// Contention that outlasts the wait is reported as
    /// [`std::io::ErrorKind::WouldBlock`], so the caller can tell another
    /// instance apart from a lock file that could not be opened at all.
    pub fn acquire(path: &Path, wait: std::time::Duration) -> std::io::Result<Self> {
        use rustix::fs::{FlockOperation, flock};
        use std::os::unix::fs::OpenOptionsExt;

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        let deadline = std::time::Instant::now() + wait;
        loop {
            match flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(Self { _file: file }),
                Err(rustix::io::Errno::WOULDBLOCK) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(std::io::ErrorKind::WouldBlock.into());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(errno) => return Err(errno.into()),
            }
        }
    }
}

/// Where the instance lock lives.
///
/// Beside the data directory rather than inside it: adopting the directory
/// left by the application's old name moves the data directory wholesale,
/// and a lock moved out from under the process holding it no longer guards
/// the path the next process looks at. A data directory given through
/// `TROVE_DATA_DIR` is never adopted or moved, so its lock can sit inside it,
/// which also keeps two such directories from blocking each other.
pub fn instance_lock_path(app: &tauri::AppHandle) -> anyhow::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("TROVE_DATA_DIR") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir)?;
        return Ok(dir.join(".trove-instance.lock"));
    }
    let data_dir = app.path().app_data_dir()?;
    let (Some(parent), Some(name)) = (data_dir.parent(), data_dir.file_name()) else {
        anyhow::bail!("data directory has no parent to hold the instance lock");
    };
    std::fs::create_dir_all(parent)?;
    Ok(parent.join(format!("{}.lock", name.to_string_lossy())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A throwaway `.app` with a handwritten XML plist, in the shape the
    /// launcher crate's own fixtures use: these tests never depend on what
    /// is installed on the machine.
    fn synthetic_app(root: &Path, dir_name: &str, bundle_name: &str) {
        let contents = root.join(dir_name).join("Contents");
        std::fs::create_dir_all(&contents).unwrap();
        let info = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\
             <plist version=\"1.0\"><dict><key>CFBundleName</key><string>{bundle_name}</string></dict></plist>"
        );
        std::fs::write(contents.join("Info.plist"), info).unwrap();
    }

    #[test]
    fn snapshot_answers_only_what_a_completed_scan_left_behind() {
        let root = tempfile::tempdir().unwrap();
        synthetic_app(root.path(), "One.app", "One");
        let launcher = LauncherState::scanning(vec![root.path().to_path_buf()]);

        assert!(launcher.snapshot().is_none(), "nothing scanned yet");
        assert_eq!(launcher.refresh().len(), 1);
        assert_eq!(launcher.snapshot().unwrap().len(), 1);
    }

    #[test]
    fn a_refresh_in_flight_refuses_a_second_one_and_speaks_for_it() {
        let root = tempfile::tempdir().unwrap();
        synthetic_app(root.path(), "One.app", "One");
        let launcher = LauncherState::scanning(vec![root.path().to_path_buf()]);
        launcher.refresh();

        // The previous catalog is answerable before the refresh starts.
        assert_eq!(launcher.snapshot().map(|catalog| catalog.len()), Some(1));

        // Hold the cache lock: the background scan finishes its directory
        // walk and then blocks at the swap, keeping `scan_in_flight` set —
        // the exact window a second palette opening would land in.
        let guard = launcher
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (spoken, spoke) = mpsc::channel();
        assert!(
            launcher.refresh_in_background(move |changed| {
                spoken.send(changed).unwrap();
            }),
            "no scan was running yet"
        );
        // The scan thread reaches the lock and waits; give it the chance.
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            !launcher.refresh_in_background(|_| {}),
            "a scan is already in flight"
        );
        drop(guard);

        // The in-flight flag is released before the callback runs, so the
        // scan the callback may trigger is free to start.
        let changed = spoke
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the background scan must finish once unblocked");
        assert!(!changed, "nothing was installed in between");
        assert_eq!(launcher.snapshot().map(|catalog| catalog.len()), Some(1));
        let (spoken_again, spoke_again) = mpsc::channel();
        assert!(
            launcher.refresh_in_background(move |changed| {
                spoken_again.send(changed).unwrap();
            }),
            "the flag is free again after the callback"
        );
        spoke_again
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the second scan must finish too");
    }

    #[test]
    fn a_background_refresh_replaces_the_catalog_only_when_complete() {
        let root = tempfile::tempdir().unwrap();
        synthetic_app(root.path(), "One.app", "One");
        let launcher = LauncherState::scanning(vec![root.path().to_path_buf()]);
        assert_eq!(launcher.refresh().len(), 1);

        synthetic_app(root.path(), "Two.app", "Two");
        let (spoken, spoke) = mpsc::channel();
        assert!(launcher.refresh_in_background(move |changed| {
            spoken.send(changed).unwrap();
        }));

        let changed = spoke
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the scan must complete");
        assert!(changed, "the catalog gained a bundle");
        assert_eq!(launcher.snapshot().unwrap().len(), 2);
    }

    fn with_history(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(DATABASE_FILENAME), b"history").unwrap();
    }

    #[test]
    fn a_fresh_install_uses_the_current_directory() {
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("pl.local.trove");
        let legacy = root.path().join("pl.local.clipboard-history");
        assert_eq!(
            adopt_legacy_data_dir(current.clone(), Some(legacy)),
            current
        );
    }

    #[test]
    fn a_live_history_is_never_replaced_by_the_old_one() {
        // Both present means the move already happened, or both versions have
        // run. Touching the live directory here could only lose data.
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("pl.local.trove");
        let legacy = root.path().join("pl.local.clipboard-history");
        with_history(&current);
        with_history(&legacy);

        assert_eq!(
            adopt_legacy_data_dir(current.clone(), Some(legacy.clone())),
            current
        );
        assert!(legacy.join(DATABASE_FILENAME).exists());
    }

    #[test]
    fn the_history_moves_across_when_only_the_old_directory_has_one() {
        // The rename this whole function exists for: the application was called
        // something else yesterday, and the database is not backed up anywhere.
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("pl.local.trove");
        let legacy = root.path().join("pl.local.clipboard-history");
        with_history(&legacy);

        let chosen = adopt_legacy_data_dir(current.clone(), Some(legacy.clone()));
        assert_eq!(chosen, current);
        assert_eq!(
            std::fs::read(current.join(DATABASE_FILENAME)).unwrap(),
            b"history"
        );
        assert!(!legacy.exists());
    }

    #[test]
    fn an_empty_current_directory_does_not_shadow_the_history() {
        // Deciding on existence alone lost the history to a directory some
        // earlier run had created and left empty: the store opened there,
        // reported nothing, and the real history sat untouched next to it.
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("pl.local.trove");
        let legacy = root.path().join("pl.local.clipboard-history");
        std::fs::create_dir(&current).unwrap();
        with_history(&legacy);

        let chosen = adopt_legacy_data_dir(current.clone(), Some(legacy.clone()));
        assert_eq!(chosen, current);
        assert_eq!(
            std::fs::read(current.join(DATABASE_FILENAME)).unwrap(),
            b"history"
        );
    }

    #[test]
    fn a_current_directory_holding_something_else_is_not_deleted_for_the_move() {
        // No database, but not empty either. Whatever that is, it is not ours
        // to remove, so the history is read where it lies instead.
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("pl.local.trove");
        let legacy = root.path().join("pl.local.clipboard-history");
        std::fs::create_dir(&current).unwrap();
        std::fs::write(current.join("something-else"), b"not ours").unwrap();
        with_history(&legacy);

        assert_eq!(
            adopt_legacy_data_dir(current.clone(), Some(legacy.clone())),
            legacy
        );
        assert!(current.join("something-else").exists());
        assert!(legacy.join(DATABASE_FILENAME).exists());
    }

    #[test]
    fn an_unmovable_history_is_read_where_it_lies_rather_than_abandoned() {
        // The move is a convenience; the data is not. When the rename cannot
        // happen the old directory is still the one holding the history.
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("pl.local.clipboard-history");
        with_history(&legacy);
        // A destination whose parent does not exist cannot be renamed onto.
        let current = root.path().join("missing").join("pl.local.trove");

        assert_eq!(adopt_legacy_data_dir(current, Some(legacy.clone())), legacy);
    }

    #[test]
    fn a_second_instance_lock_on_the_same_path_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pl.local.trove.lock");
        let _first = InstanceLock::acquire(&path, std::time::Duration::ZERO).unwrap();

        let second = InstanceLock::acquire(&path, std::time::Duration::ZERO);

        assert_eq!(
            second.err().map(|error| error.kind()),
            Some(std::io::ErrorKind::WouldBlock),
            "contention must read as another instance, not as a broken lock file"
        );
    }

    #[test]
    fn the_instance_lock_is_free_again_once_its_holder_lets_go() {
        // The kernel releases the lock with the descriptor, which is what
        // keeps a crashed instance from locking the history away for good.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pl.local.trove.lock");
        drop(InstanceLock::acquire(&path, std::time::Duration::ZERO).unwrap());

        assert!(InstanceLock::acquire(&path, std::time::Duration::ZERO).is_ok());
    }

    #[test]
    fn a_waiting_instance_lock_is_taken_when_the_old_process_lets_go() {
        // The shape of an update: the new process starts while the old one
        // still holds the lock, and must wait it out rather than step aside.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pl.local.trove.lock");
        let old = InstanceLock::acquire(&path, std::time::Duration::ZERO).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(old);
        });

        let new = InstanceLock::acquire(&path, INSTANCE_LOCK_WAIT);

        release.join().unwrap();
        assert!(new.is_ok());
    }

    #[test]
    fn instance_locks_in_different_directories_do_not_block_each_other() {
        // Separate data directories are separate histories, as two
        // `TROVE_DATA_DIR` runs side by side are.
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        let _first = InstanceLock::acquire(
            &one.path().join(".trove-instance.lock"),
            std::time::Duration::ZERO,
        )
        .unwrap();

        assert!(
            InstanceLock::acquire(
                &two.path().join(".trove-instance.lock"),
                std::time::Duration::ZERO
            )
            .is_ok()
        );
    }
}
