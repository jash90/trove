use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use clipboard_import::ImportService;
use clipboard_launcher::AppBundle;
use clipboard_store::{StoreConfig, StoreHandle};
use tauri::Manager;

/// How long a launcher catalog stays answerable without a rescan. Scanning a
/// few hundred plists costs tens of milliseconds the palette should not pay
/// on every entry; five minutes bounds how long a freshly installed
/// application can stay invisible.
pub const LAUNCHER_CACHE_TTL_MS: i64 = 5 * 60 * 1000;

/// The application catalog and the roots it was scanned from.
///
/// `now_ms` is a parameter of [`LauncherState::catalog`], not a wall-clock
/// read inside it: staleness is a fact a test can state directly instead of
/// one it has to sleep and hope for.
#[derive(Clone)]
pub struct LauncherState {
    roots: Arc<[PathBuf]>,
    cached: Arc<Mutex<Option<CachedCatalog>>>,
}

struct CachedCatalog {
    scanned_at_ms: i64,
    catalog: Vec<AppBundle>,
}

impl LauncherState {
    pub fn scanning(roots: Vec<PathBuf>) -> Self {
        Self {
            roots: Arc::from(roots),
            cached: Arc::new(Mutex::new(None)),
        }
    }

    /// The roots launch validation accepts, the same ones the scan walked.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Returns the catalog, rescanning only when the cached copy has aged
    /// out. The scan is blocking filesystem work; callers wrap it in
    /// `spawn_blocking` at the command edge.
    pub fn catalog(&self, now_ms: i64) -> Vec<AppBundle> {
        let mut cached = self
            .cached
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = cached
            .as_ref()
            .filter(|entry| now_ms.saturating_sub(entry.scanned_at_ms) < LAUNCHER_CACHE_TTL_MS)
        {
            return entry.catalog.clone();
        }
        let catalog = clipboard_launcher::scan_applications(&self.roots);
        *cached = Some(CachedCatalog {
            scanned_at_ms: now_ms,
            catalog: catalog.clone(),
        });
        catalog
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
    /// The launcher's application catalog. Scanned lazily and cached with a
    /// TTL, because the palette is hidden most of the time and a startup
    /// scan would cost every launch for a window nobody opened.
    pub launcher: LauncherState,
}

impl AppState {
    pub fn open_data_dir(data_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        // The layout comes from clipboard-store so the importer and this
        // application can never disagree about where the database lives.
        let store = StoreHandle::open(StoreConfig::in_data_dir(data_dir))?;
        let importer = ImportService::new(store.clone())
            .map_err(|_| anyhow::anyhow!("import service initialization failed"))?;
        Ok(Self {
            store,
            importer,
            previews: Mutex::new(crate::links::FetchCoordinator::default()),
            launcher: LauncherState::scanning(clipboard_launcher::default_scan_roots()),
        })
    }
}

pub fn resolve_data_dir(app: &tauri::AppHandle) -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("CLIPBOARD_HISTORY_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    Ok(app.path().app_data_dir()?)
}
