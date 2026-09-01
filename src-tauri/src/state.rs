use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::Manager;
use trove_import::ImportService;
use trove_launcher::AppBundle;
use trove_store::{StoreConfig, StoreHandle};

/// How long a launcher catalog stays answerable without a rescan. Scanning a
/// few hundred plists costs tens of milliseconds the palette should not pay
/// on every entry; five minutes bounds how long a freshly installed
/// application can stay invisible.
pub const LAUNCHER_CACHE_TTL_MS: i64 = 5 * 60 * 1000;

/// One rendered application icon, in the shape the bridge carries images.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppIconDto {
    pub mime_type: String,
    pub base64: String,
}

/// The application catalog and the roots it was scanned from.
///
/// `now_ms` is a parameter of [`LauncherState::catalog`], not a wall-clock
/// read inside it: staleness is a fact a test can state directly instead of
/// one it has to sleep and hope for.
#[derive(Clone)]
pub struct LauncherState {
    roots: Arc<[PathBuf]>,
    cached: Arc<Mutex<Option<CachedCatalog>>>,
    /// Rendered icons, remembered per canonical path. An icon is a fact
    /// about a bundle that changes only with the bundle, so it outlives the
    /// catalog's TTL; the map grows with the distinct applications actually
    /// shown, which the virtualized list keeps to a handful at a time.
    icons: Arc<Mutex<HashMap<String, AppIconDto>>>,
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
            icons: Arc::new(Mutex::new(HashMap::new())),
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
        let catalog = trove_launcher::scan_applications(&self.roots);
        *cached = Some(CachedCatalog {
            scanned_at_ms: now_ms,
            catalog: catalog.clone(),
        });
        catalog
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
    /// The launcher's application catalog. Scanned lazily and cached with a
    /// TTL, because the palette is hidden most of the time and a startup
    /// scan would cost every launch for a window nobody opened.
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

/// Picks the directory to open, moving the old one across when it is the only
/// one that exists.
///
/// A rename within a volume is atomic, so this either happens completely or not
/// at all. When it cannot happen — a different volume, a permission, anything —
/// the legacy path is returned as it stands. Reading the history from its old
/// home is worth more than a tidy directory name, and copying gigabytes to get
/// the name would risk the copy failing halfway.
fn adopt_legacy_data_dir(current: PathBuf, legacy: Option<PathBuf>) -> PathBuf {
    if current.exists() {
        return current;
    }
    let Some(legacy) = legacy else {
        return current;
    };
    if !legacy.is_dir() {
        return current;
    }
    match std::fs::rename(&legacy, &current) {
        Ok(()) => current,
        Err(_) => legacy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn an_existing_directory_is_never_replaced_by_the_old_one() {
        // Both present means the move already happened, or the user has run
        // both versions. Touching the live directory here could only lose data.
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("pl.local.trove");
        let legacy = root.path().join("pl.local.clipboard-history");
        std::fs::create_dir(&current).unwrap();
        std::fs::create_dir(&legacy).unwrap();
        std::fs::write(legacy.join("clipboard.db"), b"old").unwrap();

        assert_eq!(
            adopt_legacy_data_dir(current.clone(), Some(legacy.clone())),
            current
        );
        assert!(legacy.join("clipboard.db").exists());
    }

    #[test]
    fn the_history_moves_across_when_only_the_old_directory_exists() {
        // The rename this whole function exists for: the application was called
        // something else yesterday, and the database is not backed up anywhere.
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("pl.local.trove");
        let legacy = root.path().join("pl.local.clipboard-history");
        std::fs::create_dir(&legacy).unwrap();
        std::fs::write(legacy.join("clipboard.db"), b"history").unwrap();

        let chosen = adopt_legacy_data_dir(current.clone(), Some(legacy.clone()));
        assert_eq!(chosen, current);
        assert_eq!(
            std::fs::read(current.join("clipboard.db")).unwrap(),
            b"history"
        );
        assert!(!legacy.exists());
    }

    #[test]
    fn an_unmovable_history_is_read_where_it_lies_rather_than_abandoned() {
        // The move is a convenience; the data is not. When the rename cannot
        // happen the old directory is still the one holding the history.
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("pl.local.clipboard-history");
        std::fs::create_dir(&legacy).unwrap();
        // A destination whose parent does not exist cannot be renamed onto.
        let current = root.path().join("missing").join("pl.local.trove");

        assert_eq!(adopt_legacy_data_dir(current, Some(legacy.clone())), legacy);
    }
}
