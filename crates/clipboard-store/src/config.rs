use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreConfig {
    database_path: PathBuf,
    blob_root: PathBuf,
}

impl StoreConfig {
    pub fn new(database_path: impl Into<PathBuf>) -> Self {
        let database_path = database_path.into();
        let blob_root = database_path.with_extension("blobs");
        Self {
            database_path,
            blob_root,
        }
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    pub fn with_blob_root(mut self, blob_root: impl Into<PathBuf>) -> Self {
        self.blob_root = blob_root.into();
        self
    }

    pub fn blob_root(&self) -> &Path {
        &self.blob_root
    }
}
