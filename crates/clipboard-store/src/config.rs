use std::{
    fmt,
    path::{Path, PathBuf},
};

#[derive(Clone, Eq, PartialEq)]
pub struct StoreConfig {
    database_path: PathBuf,
    blob_root: PathBuf,
}

impl fmt::Debug for StoreConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoreConfig")
            .finish_non_exhaustive()
    }
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
