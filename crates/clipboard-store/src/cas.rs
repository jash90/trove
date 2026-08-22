use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use clipboard_core::ContentHash;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum CasError {
    #[error("invalid CAS relative path")]
    InvalidRelativePath,
    #[error("CAS filesystem boundary is invalid")]
    FilesystemBoundary,
    #[error("CAS blob integrity check failed")]
    CorruptBlob,
    #[error("CAS filesystem operation failed")]
    Io(#[source] io::Error),
    #[error("CAS temporary-file cleanup failed")]
    CleanupFailed(#[source] io::Error),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CasBlob {
    pub hash: ContentHash,
    pub relpath: String,
    pub byte_size: u64,
}

#[derive(Clone, Debug)]
pub struct CasStore {
    root: PathBuf,
    #[cfg(test)]
    test_failures: Vec<TestFailure>,
}

impl CasStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            #[cfg(test)]
            test_failures: Vec::new(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn put(&self, bytes: &[u8]) -> Result<CasBlob, CasError> {
        let hash = *blake3::hash(bytes).as_bytes();
        let hex_hash = blake3::Hash::from(hash).to_hex().to_string();
        let relpath = format!("{}/{hex_hash}", &hex_hash[..2]);
        let root = self.ensure_root()?;
        let shard = self.ensure_directory(&root, &hex_hash[..2])?;
        let temporary_directory = self.ensure_directory(&root, ".tmp")?;
        let final_path = shard.join(&hex_hash);

        if self
            .read_existing_blob(&root, &final_path, &relpath)?
            .is_some()
        {
            return Ok(CasBlob {
                hash,
                relpath,
                byte_size: bytes.len() as u64,
            });
        }

        let temporary_path = temporary_directory.join(Uuid::now_v7().simple().to_string());
        let temporary = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
            .map_err(CasError::Io)?;
        let guard = TempBlob::new(
            self,
            temporary_path.clone(),
            root.clone(),
            temporary_directory,
        );
        let write_result =
            self.write_sync_and_rename(temporary, bytes, &temporary_path, &final_path);

        match write_result {
            Ok(()) => {
                guard.disarm();
                Ok(CasBlob {
                    hash,
                    relpath,
                    byte_size: bytes.len() as u64,
                })
            }
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                let result = match self.read_existing_blob(&root, &final_path, &relpath) {
                    Ok(Some(_)) => Ok(CasBlob {
                        hash,
                        relpath,
                        byte_size: bytes.len() as u64,
                    }),
                    Ok(None) => Err(CasError::Io(source)),
                    Err(error) => Err(error),
                };
                self.cleanup_then_return(guard, result)
            }
            Err(source) => self.cleanup_then_return(guard, Err(CasError::Io(source))),
        }
    }

    pub fn read(&self, relpath: &str) -> Result<Vec<u8>, CasError> {
        let (shard_name, blob_name) = split_relpath(relpath)?;
        let root = self.existing_root()?;
        let shard = self.validate_existing_directory(&root, &root.join(shard_name))?;
        let path = shard.join(blob_name);
        self.read_existing_blob(&root, &path, relpath)?
            .ok_or_else(|| {
                CasError::Io(io::Error::new(
                    io::ErrorKind::NotFound,
                    "CAS blob is not present",
                ))
            })
    }

    pub fn remove_orphans(&self, live_relpaths: &BTreeSet<String>) -> Result<(), CasError> {
        if matches!(fs::symlink_metadata(&self.root), Err(error) if error.kind() == io::ErrorKind::NotFound)
        {
            return Ok(());
        }
        let root = self.existing_root()?;
        let live_relpaths = live_relpaths
            .iter()
            .filter(|relpath| is_valid_relpath(relpath))
            .collect::<BTreeSet<_>>();

        for entry in fs::read_dir(&root).map_err(CasError::Io)? {
            let entry = entry.map_err(CasError::Io)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if name == ".tmp" {
                self.validate_existing_directory(&root, &path)?;
                continue;
            }
            if !is_lower_hex(&name, 2) {
                continue;
            }
            let shard = self.validate_existing_directory(&root, &path)?;
            for blob_entry in fs::read_dir(&shard).map_err(CasError::Io)? {
                let blob_entry = blob_entry.map_err(CasError::Io)?;
                let blob_name = blob_entry.file_name().to_string_lossy().into_owned();
                let relpath = format!("{name}/{blob_name}");
                if !is_valid_relpath(&relpath) {
                    continue;
                }
                let path = blob_entry.path();
                self.read_existing_blob(&root, &path, &relpath)?;
                if !live_relpaths.contains(&relpath) {
                    fs::remove_file(path).map_err(CasError::Io)?;
                }
            }
        }
        Ok(())
    }

    fn ensure_root(&self) -> Result<PathBuf, CasError> {
        match fs::symlink_metadata(&self.root) {
            Ok(_) => self.validate_existing_directory_uncontained(&self.root),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir_all(&self.root).map_err(CasError::Io)?;
                self.validate_existing_directory_uncontained(&self.root)
            }
            Err(error) => Err(CasError::Io(error)),
        }
    }

    fn existing_root(&self) -> Result<PathBuf, CasError> {
        self.validate_existing_directory_uncontained(&self.root)
    }

    fn ensure_directory(&self, root: &Path, name: &str) -> Result<PathBuf, CasError> {
        let path = root.join(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => self.validate_existing_directory(root, &path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::create_dir(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(CasError::Io(error)),
                }
                self.validate_existing_directory(root, &path)
            }
            Err(error) => Err(CasError::Io(error)),
        }
    }

    fn validate_existing_directory_uncontained(&self, path: &Path) -> Result<PathBuf, CasError> {
        let metadata = fs::symlink_metadata(path).map_err(CasError::Io)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(CasError::FilesystemBoundary);
        }
        fs::canonicalize(path).map_err(CasError::Io)
    }

    fn validate_existing_directory(&self, root: &Path, path: &Path) -> Result<PathBuf, CasError> {
        let directory = self.validate_existing_directory_uncontained(path)?;
        if !directory.starts_with(root) {
            return Err(CasError::FilesystemBoundary);
        }
        Ok(directory)
    }

    fn read_existing_blob(
        &self,
        root: &Path,
        path: &Path,
        relpath: &str,
    ) -> Result<Option<Vec<u8>>, CasError> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(CasError::Io(error)),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(CasError::FilesystemBoundary);
        }
        let canonical_path = fs::canonicalize(path).map_err(CasError::Io)?;
        if !canonical_path.starts_with(root) {
            return Err(CasError::FilesystemBoundary);
        }
        let bytes = fs::read(path).map_err(CasError::Io)?;
        if blake3::hash(&bytes).to_hex().as_str() != &relpath[3..] {
            return Err(CasError::CorruptBlob);
        }
        Ok(Some(bytes))
    }

    fn write_sync_and_rename(
        &self,
        mut temporary: File,
        bytes: &[u8],
        temporary_path: &Path,
        final_path: &Path,
    ) -> io::Result<()> {
        self.write_all(&mut temporary, bytes)?;
        self.sync_all(&temporary)?;
        drop(temporary);
        self.rename(temporary_path, final_path, bytes)
    }

    fn cleanup_then_return<T>(
        &self,
        mut guard: TempBlob<'_>,
        result: Result<T, CasError>,
    ) -> Result<T, CasError> {
        guard.cleanup()?;
        result
    }

    fn write_all(&self, file: &mut File, bytes: &[u8]) -> io::Result<()> {
        #[cfg(test)]
        if self.should_fail(TestFailure::Write) {
            return Err(io::Error::other("deterministic test write failure"));
        }
        file.write_all(bytes)
    }

    fn sync_all(&self, file: &File) -> io::Result<()> {
        #[cfg(test)]
        if self.should_fail(TestFailure::Sync) {
            return Err(io::Error::other("deterministic test sync failure"));
        }
        file.sync_all()
    }

    fn rename(&self, temporary_path: &Path, final_path: &Path, _bytes: &[u8]) -> io::Result<()> {
        #[cfg(test)]
        if self.should_fail(TestFailure::Rename) {
            return Err(io::Error::other("deterministic test rename failure"));
        }
        #[cfg(test)]
        if self.should_fail(TestFailure::DestinationAppears) {
            fs::write(final_path, _bytes)?;
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "deterministic test destination race",
            ));
        }
        fs::rename(temporary_path, final_path)
    }

    #[cfg(test)]
    fn should_fail(&self, failure: TestFailure) -> bool {
        self.test_failures.contains(&failure)
    }
}

struct TempBlob<'a> {
    store: &'a CasStore,
    path: PathBuf,
    root: PathBuf,
    temporary_directory: PathBuf,
    active: bool,
}

impl<'a> TempBlob<'a> {
    fn new(
        store: &'a CasStore,
        path: PathBuf,
        root: PathBuf,
        temporary_directory: PathBuf,
    ) -> Self {
        Self {
            store,
            path,
            root,
            temporary_directory,
            active: true,
        }
    }

    fn disarm(mut self) {
        self.active = false;
    }

    fn cleanup(&mut self) -> Result<(), CasError> {
        if !self.active {
            return Ok(());
        }
        #[cfg(test)]
        if self.store.should_fail(TestFailure::Cleanup) {
            return Err(CasError::CleanupFailed(io::Error::other(
                "deterministic test cleanup failure",
            )));
        }
        self.store
            .validate_existing_directory(&self.root, &self.temporary_directory)
            .map_err(as_cleanup_error)?;
        let metadata = fs::symlink_metadata(&self.path).map_err(CasError::CleanupFailed)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(as_cleanup_error(CasError::FilesystemBoundary));
        }
        let canonical_path = fs::canonicalize(&self.path).map_err(CasError::CleanupFailed)?;
        if !canonical_path.starts_with(&self.root) {
            return Err(as_cleanup_error(CasError::FilesystemBoundary));
        }
        fs::remove_file(&self.path).map_err(CasError::CleanupFailed)?;
        self.active = false;
        Ok(())
    }
}

fn as_cleanup_error(error: CasError) -> CasError {
    CasError::CleanupFailed(io::Error::other(error.to_string()))
}

fn split_relpath(relpath: &str) -> Result<(&str, &str), CasError> {
    if !is_valid_relpath(relpath) {
        return Err(CasError::InvalidRelativePath);
    }
    Ok((&relpath[..2], &relpath[3..]))
}

fn is_valid_relpath(relpath: &str) -> bool {
    let bytes = relpath.as_bytes();
    bytes.len() == 67
        && bytes[2] == b'/'
        && is_lower_hex(&relpath[..2], 2)
        && is_lower_hex(&relpath[3..], 64)
        && relpath[..2] == relpath[3..5]
}

fn is_lower_hex(value: &str, expected_length: usize) -> bool {
    value.len() == expected_length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TestFailure {
    Write,
    Sync,
    Rename,
    Cleanup,
    DestinationAppears,
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::{CasStore, TestFailure};

    #[test]
    fn failed_atomic_write_leaves_no_final_or_temporary_blob() {
        let directory = tempfile::tempdir().unwrap();
        let cas = CasStore {
            root: directory.path().join("blobs"),
            test_failures: vec![TestFailure::Write],
        };

        assert!(cas.put(b"payload").is_err());
        assert_eq!(files_under(cas.root()), Vec::<String>::new());
    }

    #[test]
    fn failed_sync_and_rename_leave_no_final_or_temporary_blob() {
        for failure in [TestFailure::Sync, TestFailure::Rename] {
            let directory = tempfile::tempdir().unwrap();
            let cas = CasStore {
                root: directory.path().join("blobs"),
                test_failures: vec![failure],
            };

            assert!(cas.put(b"payload").is_err());
            assert_eq!(files_under(cas.root()), Vec::<String>::new());
        }
    }

    #[test]
    fn cleanup_failure_is_reported_without_a_path() {
        let directory = tempfile::tempdir().unwrap();
        let cas = CasStore {
            root: directory.path().join("blobs"),
            test_failures: vec![TestFailure::Write, TestFailure::Cleanup],
        };

        let error = cas.put(b"payload").unwrap_err();

        assert_eq!(error.to_string(), "CAS temporary-file cleanup failed");
        assert!(!error.to_string().contains("blobs"));
    }

    #[test]
    fn destination_appearing_during_rename_returns_the_existing_blob() {
        let directory = tempfile::tempdir().unwrap();
        let cas = CasStore {
            root: directory.path().join("blobs"),
            test_failures: vec![TestFailure::DestinationAppears],
        };

        let blob = cas.put(b"payload").unwrap();

        assert_eq!(cas.read(&blob.relpath).unwrap(), b"payload");
        assert_eq!(files_under(cas.root()).len(), 1);
    }

    fn files_under(root: &Path) -> Vec<String> {
        let mut files = Vec::new();
        if !root.exists() {
            return files;
        }
        for entry in fs::read_dir(root).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                for child in fs::read_dir(entry.path()).unwrap() {
                    let child = child.unwrap();
                    if child.file_type().unwrap().is_file() {
                        files.push(child.path().display().to_string());
                    }
                }
            }
        }
        files.sort();
        files
    }
}
