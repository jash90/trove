use std::{
    ffi::{OsStr, OsString},
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt};
use cap_std::fs::{Dir as CapDir, File as CapFile, OpenOptions as CapOpenOptions};
use clipboard_core::ContentHash;
use thiserror::Error;
use uuid::Uuid;

use crate::{StorageBoundaryError, StorageBoundaryLease};

pub const CAS_VERIFY_BUFFER_BYTES: usize = 64 * 1024;
pub const MAX_CAS_OBJECT_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum CasError {
    #[error("cas_object_too_large")]
    ObjectTooLarge,
    #[error("private_storage_unavailable")]
    PrivateStorageUnavailable,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CasVerification {
    pub byte_size: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GcStepBudget {
    max_entries: usize,
}

impl GcStepBudget {
    pub const fn new(maximum: usize) -> Self {
        Self {
            max_entries: maximum,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GcStep {
    pub examined_entries: usize,
    pub orphan_candidates: usize,
    pub complete: bool,
}

pub struct CasGcSession {
    store: CasStore,
    state: GcState,
}

enum GcState {
    Direct(DirectGcState),
    Leased(LeasedGcState),
    Complete,
}

struct DirectGcState {
    root: PathBuf,
    root_entries: fs::ReadDir,
    shard: Option<DirectGcShard>,
}

struct DirectGcShard {
    name: String,
    directory: PathBuf,
    entries: fs::ReadDir,
}

struct DirectGcFile {
    identity: (u64, u64),
    size: u64,
}

struct LeasedGcState {
    root: CapDir,
    root_entries: cap_std::fs::ReadDir,
    shard: Option<LeasedGcShard>,
}

struct LeasedGcShard {
    name: String,
    directory: CapDir,
    entries: cap_std::fs::ReadDir,
}

#[derive(Clone, Eq, PartialEq)]
pub struct CasBlob {
    pub hash: ContentHash,
    pub relpath: String,
    pub byte_size: u64,
}

impl fmt::Debug for CasBlob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CasBlob")
            .field("byte_size", &self.byte_size)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct CasStore {
    root: PathBuf,
    storage_boundary: Option<Arc<StorageBoundaryLease>>,
    #[cfg(test)]
    test_failures: Vec<TestFailure>,
}

impl fmt::Debug for CasStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("CasStore").finish_non_exhaustive()
    }
}

impl CasStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            storage_boundary: None,
            #[cfg(test)]
            test_failures: Vec::new(),
        }
    }

    pub(crate) fn with_storage_boundary(
        root: impl Into<PathBuf>,
        storage_boundary: Arc<StorageBoundaryLease>,
    ) -> Self {
        Self {
            root: root.into(),
            storage_boundary: Some(storage_boundary),
            #[cfg(test)]
            test_failures: Vec::new(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn put(&self, bytes: &[u8]) -> Result<CasBlob, CasError> {
        if bytes.len() > MAX_CAS_OBJECT_BYTES {
            return Err(CasError::ObjectTooLarge);
        }
        self.with_valid_boundary(|| {
            if self.storage_boundary.is_some() {
                self.put_leased(bytes)
            } else {
                self.put_inner(bytes)
            }
        })
    }

    fn put_inner(&self, bytes: &[u8]) -> Result<CasBlob, CasError> {
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
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let temporary = options.open(&temporary_path).map_err(CasError::Io)?;
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
        self.with_valid_boundary(|| {
            if self.storage_boundary.is_some() {
                self.read_leased(relpath)
            } else {
                self.read_inner(relpath)
            }
        })
    }

    pub fn verify(&self, relpath: &str, expected_size: u64) -> Result<CasVerification, CasError> {
        if expected_size > MAX_CAS_OBJECT_BYTES as u64 {
            return Err(CasError::ObjectTooLarge);
        }
        self.with_valid_boundary(|| {
            if self.storage_boundary.is_some() {
                self.verify_leased(relpath, expected_size)
            } else {
                self.verify_inner(relpath, expected_size)
            }
        })
    }

    fn read_inner(&self, relpath: &str) -> Result<Vec<u8>, CasError> {
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

    fn verify_inner(&self, relpath: &str, expected_size: u64) -> Result<CasVerification, CasError> {
        let (shard_name, blob_name) = split_relpath(relpath)?;
        let root = self.existing_root()?;
        let shard = self.validate_existing_directory(&root, &root.join(shard_name))?;
        let path = shard.join(blob_name);
        let metadata = fs::symlink_metadata(&path).map_err(CasError::Io)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || MetadataExt::nlink(&metadata) != 1
        {
            return Err(CasError::FilesystemBoundary);
        }
        validate_private_direct_file(&metadata)?;
        if metadata.len() > MAX_CAS_OBJECT_BYTES as u64 || metadata.len() != expected_size {
            return Err(CasError::CorruptBlob);
        }
        let identity = (MetadataExt::dev(&metadata), MetadataExt::ino(&metadata));
        let mut file = open_direct_file_nofollow(&path)?;
        let opened = file.metadata().map_err(CasError::Io)?;
        validate_private_direct_file(&opened)?;
        if (MetadataExt::dev(&opened), MetadataExt::ino(&opened)) != identity {
            return Err(CasError::FilesystemBoundary);
        }
        verify_reader(&mut file, relpath, expected_size)?;
        let after = file.metadata().map_err(CasError::Io)?;
        validate_private_direct_file(&after)?;
        if !after.is_file()
            || MetadataExt::nlink(&after) != 1
            || (MetadataExt::dev(&after), MetadataExt::ino(&after)) != identity
        {
            return Err(CasError::FilesystemBoundary);
        }
        Ok(CasVerification {
            byte_size: expected_size,
        })
    }

    pub fn start_gc(&self) -> Result<CasGcSession, CasError> {
        self.with_valid_boundary(|| {
            let state = if self.storage_boundary.is_some() {
                let root = self.leased_root()?;
                let root_entries = root.entries().map_err(CasError::Io)?;
                GcState::Leased(LeasedGcState {
                    root,
                    root_entries,
                    shard: None,
                })
            } else if matches!(
                fs::symlink_metadata(&self.root),
                Err(error) if error.kind() == io::ErrorKind::NotFound
            ) {
                GcState::Complete
            } else {
                let root = self.existing_root()?;
                let root_entries = fs::read_dir(&root).map_err(CasError::Io)?;
                GcState::Direct(DirectGcState {
                    root,
                    root_entries,
                    shard: None,
                })
            };
            Ok(CasGcSession {
                store: self.clone(),
                state,
            })
        })
    }

    fn with_valid_boundary<T>(
        &self,
        operation: impl FnOnce() -> Result<T, CasError>,
    ) -> Result<T, CasError> {
        self.validate_storage_boundary()?;
        let result = operation();
        self.validate_storage_boundary()?;
        result
    }

    fn validate_storage_boundary(&self) -> Result<(), CasError> {
        #[cfg(not(unix))]
        return Err(CasError::PrivateStorageUnavailable);
        if let Some(boundary) = &self.storage_boundary {
            boundary
                .validate_for_cas(&self.root)
                .map_err(map_storage_boundary_error)?;
        }
        Ok(())
    }

    fn leased_root(&self) -> Result<CapDir, CasError> {
        self.storage_boundary
            .as_ref()
            .ok_or(CasError::FilesystemBoundary)?
            .clone_blob_directory()
            .map_err(map_storage_boundary_error)
    }

    fn put_leased(&self, bytes: &[u8]) -> Result<CasBlob, CasError> {
        let hash = *blake3::hash(bytes).as_bytes();
        let hex_hash = blake3::Hash::from(hash).to_hex().to_string();
        let shard_name = &hex_hash[..2];
        let relpath = format!("{shard_name}/{hex_hash}");
        let root = self.leased_root()?;
        let shard = ensure_cap_directory(&root, shard_name)?;
        let temporary_directory = ensure_cap_directory(&root, ".tmp")?;

        if read_cap_blob(&shard, OsStr::new(&hex_hash), &relpath)?.is_some() {
            return Ok(CasBlob {
                hash,
                relpath,
                byte_size: bytes.len() as u64,
            });
        }

        let temporary_name = OsString::from(Uuid::now_v7().simple().to_string());
        let mut options = CapOpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        #[cfg(unix)]
        cap_std::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut temporary = temporary_directory
            .open_with(&temporary_name, &options)
            .map_err(CasError::Io)?;
        let temporary_identity =
            CapFileIdentity::from_metadata(&temporary.metadata().map_err(CasError::Io)?)?;
        let mut guard = CapTempBlob::new(
            temporary_directory.try_clone().map_err(CasError::Io)?,
            temporary_name.clone(),
            temporary_identity,
        );

        let write_result = (|| -> Result<(), CasError> {
            self.write_all(&mut temporary, bytes)
                .map_err(CasError::Io)?;
            self.sync_cap_file(&temporary).map_err(CasError::Io)?;
            validate_named_cap_file(&temporary_directory, &temporary_name, temporary_identity)?;
            drop(temporary);
            self.rename_cap(
                &temporary_directory,
                &temporary_name,
                &shard,
                OsStr::new(&hex_hash),
            )
            .map_err(CasError::Io)
        })();
        match write_result {
            Ok(()) => {
                guard.disarm();
                let stored = read_cap_blob(&shard, OsStr::new(&hex_hash), &relpath)?
                    .ok_or_else(|| CasError::Io(io::Error::other("CAS blob is not present")))?;
                if stored.len() != bytes.len() {
                    return Err(CasError::CorruptBlob);
                }
                Ok(CasBlob {
                    hash,
                    relpath,
                    byte_size: bytes.len() as u64,
                })
            }
            Err(CasError::Io(source)) if source.kind() == io::ErrorKind::AlreadyExists => {
                let result = read_cap_blob(&shard, OsStr::new(&hex_hash), &relpath)?
                    .ok_or(CasError::Io(source))
                    .map(|_| CasBlob {
                        hash,
                        relpath,
                        byte_size: bytes.len() as u64,
                    });
                guard.cleanup()?;
                result
            }
            Err(error) => {
                guard.cleanup()?;
                Err(error)
            }
        }
    }

    fn read_leased(&self, relpath: &str) -> Result<Vec<u8>, CasError> {
        let (shard_name, blob_name) = split_relpath(relpath)?;
        let root = self.leased_root()?;
        let shard = open_cap_directory(&root, shard_name)?;
        read_cap_blob(&shard, OsStr::new(blob_name), relpath)?.ok_or_else(|| {
            CasError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "CAS blob is not present",
            ))
        })
    }

    fn verify_leased(
        &self,
        relpath: &str,
        expected_size: u64,
    ) -> Result<CasVerification, CasError> {
        let (shard_name, blob_name) = split_relpath(relpath)?;
        let root = self.leased_root()?;
        let shard = open_cap_directory(&root, shard_name)?;
        let metadata = shard.symlink_metadata(blob_name).map_err(CasError::Io)?;
        let identity = CapFileIdentity::from_metadata(&metadata)?;
        if metadata.len() > MAX_CAS_OBJECT_BYTES as u64 || metadata.len() != expected_size {
            return Err(CasError::CorruptBlob);
        }
        let mut options = CapOpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let mut file = shard
            .open_with(blob_name, &options)
            .map_err(|_| CasError::FilesystemBoundary)?;
        let handle_identity =
            CapFileIdentity::from_metadata(&file.metadata().map_err(CasError::Io)?)?;
        if handle_identity != identity {
            return Err(CasError::FilesystemBoundary);
        }
        verify_reader(&mut file, relpath, expected_size)?;
        validate_named_cap_file(&shard, OsStr::new(blob_name), identity)?;
        Ok(CasVerification {
            byte_size: expected_size,
        })
    }

    fn ensure_root(&self) -> Result<PathBuf, CasError> {
        match fs::symlink_metadata(&self.root) {
            Ok(_) => self.validate_existing_directory_uncontained(&self.root),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::DirBuilderExt;
                    fs::DirBuilder::new()
                        .recursive(true)
                        .mode(0o700)
                        .create(&self.root)
                        .map_err(CasError::Io)?;
                }
                #[cfg(not(unix))]
                return Err(CasError::PrivateStorageUnavailable);
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
                #[cfg(unix)]
                let create_result = {
                    use std::os::unix::fs::DirBuilderExt;
                    fs::DirBuilder::new().mode(0o700).create(&path)
                };
                #[cfg(not(unix))]
                let create_result = Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "private storage unavailable",
                ));
                match create_result {
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if metadata.uid() != rustix::process::geteuid().as_raw()
                || metadata.mode() & 0o777 != 0o700
            {
                return Err(CasError::PrivateStorageUnavailable);
            }
        }
        #[cfg(not(unix))]
        return Err(CasError::PrivateStorageUnavailable);
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
        validate_private_direct_file(&metadata)?;
        let canonical_path = fs::canonicalize(path).map_err(CasError::Io)?;
        if !canonical_path.starts_with(root) {
            return Err(CasError::FilesystemBoundary);
        }
        if metadata.len() > MAX_CAS_OBJECT_BYTES as u64 {
            return Err(CasError::ObjectTooLarge);
        }
        let identity = (MetadataExt::dev(&metadata), MetadataExt::ino(&metadata));
        let mut file = open_direct_file_nofollow(path)?;
        let opened = file.metadata().map_err(CasError::Io)?;
        validate_private_direct_file(&opened)?;
        if (MetadataExt::dev(&opened), MetadataExt::ino(&opened)) != identity {
            return Err(CasError::FilesystemBoundary);
        }
        let bytes = read_verified_bytes(&mut file, relpath, metadata.len())?;
        let after = fs::symlink_metadata(path).map_err(CasError::Io)?;
        validate_private_direct_file(&after)?;
        if (MetadataExt::dev(&after), MetadataExt::ino(&after)) != identity {
            return Err(CasError::FilesystemBoundary);
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

    fn write_all(&self, file: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
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

    fn sync_cap_file(&self, file: &CapFile) -> io::Result<()> {
        #[cfg(test)]
        if self.should_fail(TestFailure::Sync) {
            return Err(io::Error::other("deterministic test sync failure"));
        }
        file.sync_all()
    }

    fn rename_cap(
        &self,
        temporary_directory: &CapDir,
        temporary_name: &OsStr,
        shard: &CapDir,
        blob_name: &OsStr,
    ) -> io::Result<()> {
        #[cfg(test)]
        if self.should_fail(TestFailure::Rename) {
            return Err(io::Error::other("deterministic test rename failure"));
        }
        #[cfg(any(
            target_vendor = "apple",
            target_os = "linux",
            target_os = "android",
            target_os = "redox"
        ))]
        {
            rustix::fs::renameat_with(
                temporary_directory,
                temporary_name,
                shard,
                blob_name,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(io::Error::from)
        }
        #[cfg(not(any(
            target_vendor = "apple",
            target_os = "linux",
            target_os = "android",
            target_os = "redox"
        )))]
        {
            let _ = (temporary_directory, temporary_name, shard, blob_name);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "exclusive rename is unavailable",
            ))
        }
    }

    fn rename(&self, temporary_path: &Path, final_path: &Path, _bytes: &[u8]) -> io::Result<()> {
        #[cfg(test)]
        if self.should_fail(TestFailure::Rename) {
            return Err(io::Error::other("deterministic test rename failure"));
        }
        #[cfg(test)]
        if self.should_fail(TestFailure::DestinationAppears) {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(final_path)?.write_all(_bytes)?;
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "deterministic test destination race",
            ));
        }
        #[cfg(any(
            target_vendor = "apple",
            target_os = "linux",
            target_os = "android",
            target_os = "redox"
        ))]
        {
            rustix::fs::renameat_with(
                rustix::fs::CWD,
                temporary_path,
                rustix::fs::CWD,
                final_path,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(io::Error::from)
        }
        #[cfg(not(any(
            target_vendor = "apple",
            target_os = "linux",
            target_os = "android",
            target_os = "redox"
        )))]
        {
            let _ = (temporary_path, final_path);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "exclusive rename is unavailable",
            ))
        }
    }

    #[cfg(test)]
    fn should_fail(&self, failure: TestFailure) -> bool {
        self.test_failures.contains(&failure)
    }
}

fn map_storage_boundary_error(error: StorageBoundaryError) -> CasError {
    match error {
        StorageBoundaryError::Changed => CasError::FilesystemBoundary,
        StorageBoundaryError::PrivateStorageUnavailable => CasError::PrivateStorageUnavailable,
    }
}

impl CasGcSession {
    pub fn step(
        &mut self,
        budget: GcStepBudget,
        mut is_live: impl FnMut(&str) -> Result<bool, CasError>,
    ) -> Result<GcStep, CasError> {
        self.store.validate_storage_boundary()?;
        let mut outcome = GcStep {
            examined_entries: 0,
            orphan_candidates: 0,
            complete: matches!(self.state, GcState::Complete),
        };
        while !outcome.complete && outcome.examined_entries < budget.max_entries {
            let progress = match &mut self.state {
                GcState::Direct(state) => next_direct_gc_entry(&self.store, state, &mut is_live)?,
                GcState::Leased(state) => next_leased_gc_entry(state, &mut is_live)?,
                GcState::Complete => GcProgress::Complete,
            };
            match progress {
                GcProgress::Examined { orphan_candidate } => {
                    outcome.examined_entries += 1;
                    outcome.orphan_candidates += usize::from(orphan_candidate);
                }
                GcProgress::Complete => {
                    self.state = GcState::Complete;
                    outcome.complete = true;
                }
            }
        }
        self.store.validate_storage_boundary()?;
        Ok(outcome)
    }
}

enum GcProgress {
    Examined { orphan_candidate: bool },
    Complete,
}

fn next_direct_gc_entry(
    store: &CasStore,
    state: &mut DirectGcState,
    is_live: &mut impl FnMut(&str) -> Result<bool, CasError>,
) -> Result<GcProgress, CasError> {
    loop {
        if let Some(shard) = &mut state.shard {
            match shard.entries.next() {
                Some(Err(error)) => return Err(CasError::Io(error)),
                Some(Ok(entry)) => {
                    let blob_name = match entry.file_name().into_string() {
                        Ok(name) => name,
                        Err(_) => {
                            return Ok(GcProgress::Examined {
                                orphan_candidate: false,
                            });
                        }
                    };
                    let relpath = format!("{}/{blob_name}", shard.name);
                    if !is_valid_relpath(&relpath) {
                        return Ok(GcProgress::Examined {
                            orphan_candidate: false,
                        });
                    }
                    let path = shard.directory.join(&blob_name);
                    let Some(file) = valid_direct_gc_file(&path)? else {
                        return Ok(GcProgress::Examined {
                            orphan_candidate: false,
                        });
                    };
                    if store.verify_inner(&relpath, file.size).is_err() || is_live(&relpath)? {
                        return Ok(GcProgress::Examined {
                            orphan_candidate: false,
                        });
                    }
                    let Some(after) = valid_direct_gc_file(&path)? else {
                        return Ok(GcProgress::Examined {
                            orphan_candidate: false,
                        });
                    };
                    if after.identity != file.identity {
                        return Ok(GcProgress::Examined {
                            orphan_candidate: false,
                        });
                    }
                    return Ok(GcProgress::Examined {
                        orphan_candidate: true,
                    });
                }
                None => state.shard = None,
            }
        } else {
            match state.root_entries.next() {
                Some(Err(error)) => return Err(CasError::Io(error)),
                Some(Ok(entry)) => {
                    let name = match entry.file_name().into_string() {
                        Ok(name) => name,
                        Err(_) => {
                            return Ok(GcProgress::Examined {
                                orphan_candidate: false,
                            });
                        }
                    };
                    if name == ".tmp" || !is_lower_hex(&name, 2) {
                        return Ok(GcProgress::Examined {
                            orphan_candidate: false,
                        });
                    }
                    let path = entry.path();
                    let directory = match store.validate_existing_directory(&state.root, &path) {
                        Ok(directory) => directory,
                        Err(_) => {
                            return Ok(GcProgress::Examined {
                                orphan_candidate: false,
                            });
                        }
                    };
                    let entries = match fs::read_dir(&directory) {
                        Ok(entries) => entries,
                        Err(_) => {
                            return Ok(GcProgress::Examined {
                                orphan_candidate: false,
                            });
                        }
                    };
                    state.shard = Some(DirectGcShard {
                        name,
                        directory,
                        entries,
                    });
                    return Ok(GcProgress::Examined {
                        orphan_candidate: false,
                    });
                }
                None => return Ok(GcProgress::Complete),
            }
        }
    }
}

fn valid_direct_gc_file(path: &Path) -> Result<Option<DirectGcFile>, CasError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(CasError::Io(error)),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || MetadataExt::nlink(&metadata) != 1
    {
        return Ok(None);
    }
    if validate_private_direct_file(&metadata).is_err() {
        return Ok(None);
    }
    Ok(Some(DirectGcFile {
        identity: (MetadataExt::dev(&metadata), MetadataExt::ino(&metadata)),
        size: metadata.len(),
    }))
}

fn validate_private_direct_file(metadata: &fs::Metadata) -> Result<(), CasError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o777 != 0o600
            || std::os::unix::fs::MetadataExt::nlink(metadata) != 1
        {
            return Err(CasError::PrivateStorageUnavailable);
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Err(CasError::PrivateStorageUnavailable)
    }
}

fn open_direct_file_nofollow(path: &Path) -> Result<File, CasError> {
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags};
        let descriptor = rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|_| CasError::FilesystemBoundary)?;
        Ok(File::from(descriptor))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(CasError::PrivateStorageUnavailable)
    }
}

fn next_leased_gc_entry(
    state: &mut LeasedGcState,
    is_live: &mut impl FnMut(&str) -> Result<bool, CasError>,
) -> Result<GcProgress, CasError> {
    loop {
        if let Some(shard) = &mut state.shard {
            match shard.entries.next() {
                Some(Err(error)) => return Err(CasError::Io(error)),
                Some(Ok(entry)) => {
                    let blob_name = match entry.file_name().into_string() {
                        Ok(name) => name,
                        Err(_) => {
                            return Ok(GcProgress::Examined {
                                orphan_candidate: false,
                            });
                        }
                    };
                    let relpath = format!("{}/{blob_name}", shard.name);
                    if !is_valid_relpath(&relpath) {
                        return Ok(GcProgress::Examined {
                            orphan_candidate: false,
                        });
                    }
                    let metadata = match shard.directory.symlink_metadata(&blob_name) {
                        Ok(metadata) => metadata,
                        Err(_) => {
                            return Ok(GcProgress::Examined {
                                orphan_candidate: false,
                            });
                        }
                    };
                    let identity = match CapFileIdentity::from_metadata(&metadata) {
                        Ok(identity) => identity,
                        Err(_) => {
                            return Ok(GcProgress::Examined {
                                orphan_candidate: false,
                            });
                        }
                    };
                    if metadata.len() > MAX_CAS_OBJECT_BYTES as u64 {
                        return Ok(GcProgress::Examined {
                            orphan_candidate: false,
                        });
                    }
                    let mut options = CapOpenOptions::new();
                    options.read(true).follow(FollowSymlinks::No);
                    let mut file = match shard.directory.open_with(&blob_name, &options) {
                        Ok(file) => file,
                        Err(_) => {
                            return Ok(GcProgress::Examined {
                                orphan_candidate: false,
                            });
                        }
                    };
                    let handle_identity = match file.metadata() {
                        Ok(metadata) => CapFileIdentity::from_metadata(&metadata),
                        Err(_) => {
                            return Ok(GcProgress::Examined {
                                orphan_candidate: false,
                            });
                        }
                    };
                    if !matches!(handle_identity, Ok(current) if current == identity)
                        || verify_reader(&mut file, &relpath, metadata.len()).is_err()
                        || is_live(&relpath)?
                    {
                        return Ok(GcProgress::Examined {
                            orphan_candidate: false,
                        });
                    }
                    if validate_named_cap_file(&shard.directory, OsStr::new(&blob_name), identity)
                        .is_err()
                    {
                        return Ok(GcProgress::Examined {
                            orphan_candidate: false,
                        });
                    }
                    return Ok(GcProgress::Examined {
                        orphan_candidate: true,
                    });
                }
                None => state.shard = None,
            }
        } else {
            match state.root_entries.next() {
                Some(Err(error)) => return Err(CasError::Io(error)),
                Some(Ok(entry)) => {
                    let name = match entry.file_name().into_string() {
                        Ok(name) => name,
                        Err(_) => {
                            return Ok(GcProgress::Examined {
                                orphan_candidate: false,
                            });
                        }
                    };
                    if name == ".tmp" || !is_lower_hex(&name, 2) {
                        return Ok(GcProgress::Examined {
                            orphan_candidate: false,
                        });
                    }
                    let directory = match open_cap_directory(&state.root, &name) {
                        Ok(directory) => directory,
                        Err(_) => {
                            return Ok(GcProgress::Examined {
                                orphan_candidate: false,
                            });
                        }
                    };
                    let entries = match directory.entries() {
                        Ok(entries) => entries,
                        Err(_) => {
                            return Ok(GcProgress::Examined {
                                orphan_candidate: false,
                            });
                        }
                    };
                    state.shard = Some(LeasedGcShard {
                        name,
                        directory,
                        entries,
                    });
                    return Ok(GcProgress::Examined {
                        orphan_candidate: false,
                    });
                }
                None => return Ok(GcProgress::Complete),
            }
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct CapFileIdentity {
    device: u64,
    inode: u64,
}

impl CapFileIdentity {
    fn from_metadata(metadata: &cap_std::fs::Metadata) -> Result<Self, CasError> {
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || MetadataExt::nlink(metadata) != 1
        {
            return Err(CasError::FilesystemBoundary);
        }
        #[cfg(unix)]
        {
            use cap_std::fs::MetadataExt as _;
            if metadata.uid() != rustix::process::geteuid().as_raw()
                || metadata.mode() & 0o777 != 0o600
            {
                return Err(CasError::PrivateStorageUnavailable);
            }
        }
        #[cfg(not(unix))]
        return Err(CasError::PrivateStorageUnavailable);
        Ok(Self {
            device: MetadataExt::dev(metadata),
            inode: MetadataExt::ino(metadata),
        })
    }
}

fn ensure_cap_directory(parent: &CapDir, name: &str) -> Result<CapDir, CasError> {
    #[cfg(unix)]
    let create_result = {
        let mut builder = cap_std::fs::DirBuilder::new();
        cap_std::fs::DirBuilderExt::mode(&mut builder, 0o700);
        parent.create_dir_with(name, &builder)
    };
    #[cfg(not(unix))]
    let create_result = Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "private storage unavailable",
    ));
    match create_result {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(CasError::Io(error)),
    }
    open_cap_directory(parent, name)
}

fn open_cap_directory(parent: &CapDir, name: &str) -> Result<CapDir, CasError> {
    let directory = parent
        .open_dir_nofollow(name)
        .map_err(|_| CasError::FilesystemBoundary)?;
    let metadata = directory.dir_metadata().map_err(CasError::Io)?;
    if !metadata.is_dir() || MetadataExt::nlink(&metadata) == 0 {
        return Err(CasError::FilesystemBoundary);
    }
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt as _;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o777 != 0o700
        {
            return Err(CasError::PrivateStorageUnavailable);
        }
    }
    #[cfg(not(unix))]
    return Err(CasError::PrivateStorageUnavailable);
    Ok(directory)
}

fn validate_named_cap_file(
    directory: &CapDir,
    name: &OsStr,
    expected: CapFileIdentity,
) -> Result<(), CasError> {
    let metadata = directory.symlink_metadata(name).map_err(CasError::Io)?;
    let identity = CapFileIdentity::from_metadata(&metadata)?;
    if identity != expected {
        return Err(CasError::FilesystemBoundary);
    }
    let mut options = CapOpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = directory
        .open_with(name, &options)
        .map_err(|_| CasError::FilesystemBoundary)?;
    let handle_identity = CapFileIdentity::from_metadata(&file.metadata().map_err(CasError::Io)?)?;
    if handle_identity != expected {
        return Err(CasError::FilesystemBoundary);
    }
    Ok(())
}

fn read_cap_blob(
    directory: &CapDir,
    name: &OsStr,
    relpath: &str,
) -> Result<Option<Vec<u8>>, CasError> {
    let metadata = match directory.symlink_metadata(name) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(CasError::Io(error)),
    };
    let identity = CapFileIdentity::from_metadata(&metadata)?;
    let mut options = CapOpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = directory
        .open_with(name, &options)
        .map_err(|_| CasError::FilesystemBoundary)?;
    let handle_identity = CapFileIdentity::from_metadata(&file.metadata().map_err(CasError::Io)?)?;
    if handle_identity != identity {
        return Err(CasError::FilesystemBoundary);
    }
    if metadata.len() > MAX_CAS_OBJECT_BYTES as u64 {
        return Err(CasError::ObjectTooLarge);
    }
    let bytes = read_verified_bytes(&mut file, relpath, metadata.len())?;
    validate_named_cap_file(directory, name, identity)?;
    Ok(Some(bytes))
}

fn read_verified_bytes(
    reader: &mut impl Read,
    relpath: &str,
    expected_size: u64,
) -> Result<Vec<u8>, CasError> {
    let capacity = usize::try_from(expected_size).map_err(|_| CasError::ObjectTooLarge)?;
    if capacity > MAX_CAS_OBJECT_BYTES {
        return Err(CasError::ObjectTooLarge);
    }
    let mut bytes = Vec::with_capacity(capacity);
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; CAS_VERIFY_BUFFER_BYTES];
    loop {
        let read = reader.read(&mut buffer).map_err(CasError::Io)?;
        if read == 0 {
            break;
        }
        if bytes.len().saturating_add(read) > capacity {
            return Err(CasError::CorruptBlob);
        }
        hasher.update(&buffer[..read]);
        bytes.extend_from_slice(&buffer[..read]);
    }
    if bytes.len() != capacity || hasher.finalize().to_hex().as_str() != &relpath[3..] {
        return Err(CasError::CorruptBlob);
    }
    Ok(bytes)
}

fn verify_reader(
    reader: &mut impl Read,
    relpath: &str,
    expected_size: u64,
) -> Result<(), CasError> {
    let mut hasher = blake3::Hasher::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; CAS_VERIFY_BUFFER_BYTES];
    loop {
        let read = reader.read(&mut buffer).map_err(CasError::Io)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or(CasError::ObjectTooLarge)?;
        if total > MAX_CAS_OBJECT_BYTES as u64 || total > expected_size {
            return Err(CasError::CorruptBlob);
        }
        hasher.update(&buffer[..read]);
    }
    if total != expected_size || hasher.finalize().to_hex().as_str() != &relpath[3..] {
        return Err(CasError::CorruptBlob);
    }
    Ok(())
}

struct CapTempBlob {
    directory: CapDir,
    name: OsString,
    identity: CapFileIdentity,
    active: bool,
}

impl CapTempBlob {
    fn new(directory: CapDir, name: OsString, identity: CapFileIdentity) -> Self {
        Self {
            directory,
            name,
            identity,
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
        validate_named_cap_file(&self.directory, &self.name, self.identity)
            .map_err(as_cleanup_error)?;
        self.directory
            .remove_file(&self.name)
            .map_err(CasError::CleanupFailed)?;
        self.active = false;
        Ok(())
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
            storage_boundary: None,
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
                storage_boundary: None,
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
            storage_boundary: None,
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
            storage_boundary: None,
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
