use std::path::{Path, PathBuf};

use clipboard_import::ImportService;
use clipboard_store::{StoreConfig, StoreHandle};
use tauri::Manager;

pub struct AppState {
    pub store: StoreHandle,
    pub importer: ImportService,
}

impl AppState {
    pub fn open_data_dir(data_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let database_path = data_dir.as_ref().join("history.sqlite");
        let blob_root = data_dir.as_ref().join("blobs");
        let store = StoreHandle::open(StoreConfig::new(database_path).with_blob_root(blob_root))?;
        let importer = ImportService::new(store.clone())
            .map_err(|_| anyhow::anyhow!("import service initialization failed"))?;
        Ok(Self { store, importer })
    }
}

pub fn resolve_data_dir(app: &tauri::AppHandle) -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("CLIPBOARD_HISTORY_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    Ok(app.path().app_data_dir()?)
}
