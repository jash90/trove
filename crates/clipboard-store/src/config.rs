use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::StorageBoundaryLease;

#[derive(Clone)]
pub struct StoreConfig {
    database_path: PathBuf,
    blob_root: PathBuf,
    storage_boundary: Option<Arc<StorageBoundaryLease>>,
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
            storage_boundary: None,
        }
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    pub fn with_blob_root(mut self, blob_root: impl Into<PathBuf>) -> Self {
        self.blob_root = blob_root.into();
        self
    }

    pub fn with_storage_boundary(mut self, boundary: Arc<StorageBoundaryLease>) -> Self {
        self.database_path = boundary.database_path().to_path_buf();
        self.blob_root = boundary.blob_path().to_path_buf();
        self.storage_boundary = Some(boundary);
        self
    }

    pub fn blob_root(&self) -> &Path {
        &self.blob_root
    }

    pub(crate) fn storage_boundary(&self) -> Option<&Arc<StorageBoundaryLease>> {
        self.storage_boundary.as_ref()
    }

    pub(crate) fn set_storage_boundary(&mut self, boundary: Arc<StorageBoundaryLease>) {
        self.database_path = boundary.database_path().to_path_buf();
        self.blob_root = boundary.blob_path().to_path_buf();
        self.storage_boundary = Some(boundary);
    }
}
