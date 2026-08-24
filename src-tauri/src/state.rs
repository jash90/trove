use std::path::{Path, PathBuf};
use std::sync::Mutex;

use clipboard_import::ImportService;
use clipboard_store::{StoreConfig, StoreHandle};
use tauri::Manager;

pub struct AppState {
    pub store: StoreHandle,
    pub importer: ImportService,
    /// Guards the one network path in the application against asking twice at
    /// once and against asking a host that has stopped answering. Shared
    /// mutable state behind a lock rather than globals, so tests can hold a
    /// coordinator of their own.
    pub previews: Mutex<crate::links::FetchCoordinator>,
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
        })
    }
}

pub fn resolve_data_dir(app: &tauri::AppHandle) -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("CLIPBOARD_HISTORY_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    Ok(app.path().app_data_dir()?)
}
