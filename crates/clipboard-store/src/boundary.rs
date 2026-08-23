use std::{
    ffi::{OsStr, OsString},
    fmt, fs, io,
    path::{Path, PathBuf},
};

use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, File, OpenOptions},
};
use thiserror::Error;

use crate::StoreConfig;

#[derive(Debug, Error)]
pub enum StorageBoundaryError {
    #[error("storage boundary changed")]
    Changed,
    #[error("private_storage_unavailable")]
    PrivateStorageUnavailable,
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    links: u64,
}

struct LeasedDirectory {
    path: PathBuf,
    name_from_parent: Option<OsString>,
    directory: Dir,
    identity: FileIdentity,
}

pub struct StorageBoundaryLease {
    data_path: PathBuf,
    database_path: PathBuf,
    blob_path: PathBuf,
    database_name: OsString,
    blob_name: OsString,
    ancestor_chain: Vec<LeasedDirectory>,
    data_directory: Dir,
    blob_directory: Dir,
    database_file: File,
    data_identity: FileIdentity,
    blob_identity: FileIdentity,
    database_identity: FileIdentity,
    writer: bool,
}

impl fmt::Debug for StorageBoundaryLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StorageBoundaryLease")
            .field("writer", &self.writer)
            .finish_non_exhaustive()
    }
}

impl StorageBoundaryLease {
    pub fn create_writer(config: &StoreConfig) -> Result<Self, StorageBoundaryError> {
        Self::open(config, true, true)
    }

    pub fn open_read_only(config: &StoreConfig) -> Result<Self, StorageBoundaryError> {
        Self::open(config, false, true)
    }

    pub(crate) fn create_writer_preflight(
        config: &StoreConfig,
    ) -> Result<Self, StorageBoundaryError> {
        Self::open(config, true, false)
    }

    pub(crate) fn open_read_only_preflight(
        config: &StoreConfig,
    ) -> Result<Self, StorageBoundaryError> {
        Self::open(config, false, false)
    }

    pub fn validate(&self) -> Result<(), StorageBoundaryError> {
        #[cfg(not(unix))]
        return Err(StorageBoundaryError::PrivateStorageUnavailable);
        #[cfg(unix)]
        {
            self.validate_preflight()?;
            self.validate_optional_sidecar("-wal")?;
            self.validate_optional_sidecar("-shm")?;
            self.validate_optional_sidecar("-journal")?;
            self.validate_preflight()
        }
    }

    fn validate_preflight(&self) -> Result<(), StorageBoundaryError> {
        #[cfg(not(unix))]
        return Err(StorageBoundaryError::PrivateStorageUnavailable);
        #[cfg(unix)]
        {
            self.validate_configuration_paths(&self.database_path, &self.blob_path)?;
            validate_directory_chain(&self.ancestor_chain)?;
            validate_private_std_directory(&self.data_path, self.data_identity)?;
            validate_private_cap_directory(&self.data_directory, self.data_identity)?;
            validate_private_std_directory(&self.blob_path, self.blob_identity)?;
            validate_private_cap_directory(&self.blob_directory, self.blob_identity)?;
            let reopened_blob = self
                .data_directory
                .open_dir_nofollow(&self.blob_name)
                .map_err(|_| StorageBoundaryError::Changed)?;
            validate_private_cap_directory(&reopened_blob, self.blob_identity)?;
            validate_private_std_file(&self.database_path, self.database_identity)?;
            let reopened_database = open_existing_file(&self.data_directory, &self.database_name)?;
            validate_private_cap_file(&reopened_database, self.database_identity)?;
            validate_directory_chain(&self.ancestor_chain)?;
            validate_private_std_directory(&self.blob_path, self.blob_identity)?;
            validate_private_cap_directory(&self.blob_directory, self.blob_identity)?;
            validate_private_std_file(&self.database_path, self.database_identity)?;
            validate_private_cap_file(&self.database_file, self.database_identity)
        }
    }

    pub(crate) fn database_path(&self) -> &Path {
        &self.database_path
    }

    pub(crate) fn blob_path(&self) -> &Path {
        &self.blob_path
    }

    pub(crate) fn database_identity_key(&self) -> (u64, u64) {
        (self.database_identity.device, self.database_identity.inode)
    }

    pub(crate) fn blob_identity_key(&self) -> (u64, u64) {
        (self.blob_identity.device, self.blob_identity.inode)
    }

    pub(crate) fn validate_for_config(
        &self,
        config: &StoreConfig,
        require_writer: bool,
    ) -> Result<(), StorageBoundaryError> {
        if require_writer && !self.writer {
            return Err(StorageBoundaryError::Changed);
        }
        self.validate_configuration_paths(config.database_path(), config.blob_root())?;
        self.validate()
    }

    pub(crate) fn validate_preflight_for_config(
        &self,
        config: &StoreConfig,
        require_writer: bool,
    ) -> Result<(), StorageBoundaryError> {
        if require_writer && !self.writer {
            return Err(StorageBoundaryError::Changed);
        }
        self.validate_configuration_paths(config.database_path(), config.blob_root())?;
        self.validate_preflight()
    }

    pub(crate) fn validate_for_cas(&self, blob_root: &Path) -> Result<(), StorageBoundaryError> {
        if blob_root != self.blob_path {
            return Err(StorageBoundaryError::Changed);
        }
        self.validate()
    }

    pub(crate) fn clone_blob_directory(&self) -> Result<Dir, StorageBoundaryError> {
        self.validate()?;
        self.blob_directory
            .try_clone()
            .map_err(|_| StorageBoundaryError::Changed)
    }

    fn open(
        config: &StoreConfig,
        writer: bool,
        validate_sidecars: bool,
    ) -> Result<Self, StorageBoundaryError> {
        #[cfg(not(unix))]
        {
            let _ = (config, writer, validate_sidecars);
            return Err(StorageBoundaryError::PrivateStorageUnavailable);
        }
        #[cfg(unix)]
        {
            Self::open_unix(config, writer, validate_sidecars)
        }
    }

    #[cfg(unix)]
    fn open_unix(
        config: &StoreConfig,
        writer: bool,
        validate_sidecars: bool,
    ) -> Result<Self, StorageBoundaryError> {
        let configured_database_path = config.database_path().to_path_buf();
        let configured_blob_path = config.blob_root().to_path_buf();
        let configured_data_path = configured_database_path
            .parent()
            .ok_or(StorageBoundaryError::Changed)?
            .to_path_buf();
        if configured_blob_path.parent() != Some(configured_data_path.as_path()) {
            return Err(StorageBoundaryError::Changed);
        }
        let database_name = direct_child_name(&configured_data_path, &configured_database_path)?;
        let blob_name = direct_child_name(&configured_data_path, &configured_blob_path)?;
        let configured_metadata = fs::symlink_metadata(&configured_data_path).map_err(changed)?;
        if configured_metadata.file_type().is_symlink() || !configured_metadata.is_dir() {
            return Err(StorageBoundaryError::Changed);
        }
        if writer && !configured_database_path.exists() && !configured_blob_path.exists() {
            harden_fresh_empty_data_directory(&configured_data_path)?;
        }
        let data_path = fs::canonicalize(&configured_data_path).map_err(changed)?;
        let database_path = data_path.join(&database_name);
        let blob_path = data_path.join(&blob_name);
        let ancestor_chain = open_directory_chain(&data_path)?;
        let data_lease = ancestor_chain.last().ok_or(StorageBoundaryError::Changed)?;
        let data_identity = data_lease.identity;
        let data_directory = data_lease
            .directory
            .try_clone()
            .map_err(|_| StorageBoundaryError::Changed)?;
        validate_cap_directory(&data_directory, data_identity)?;

        if writer {
            let mut builder = cap_std::fs::DirBuilder::new();
            cap_std::fs::DirBuilderExt::mode(&mut builder, 0o700);
            match data_directory.create_dir_with(&blob_name, &builder) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(StorageBoundaryError::Changed),
            }
        }
        let blob_directory = data_directory
            .open_dir_nofollow(&blob_name)
            .map_err(|_| StorageBoundaryError::Changed)?;
        let blob_identity = cap_identity(
            &blob_directory
                .dir_metadata()
                .map_err(|_| StorageBoundaryError::Changed)?,
        );

        let database_file = if writer {
            open_or_create_file(&data_directory, &database_name)?
        } else {
            open_existing_file(&data_directory, &database_name)?
        };
        let database_identity = cap_identity(
            &database_file
                .metadata()
                .map_err(|_| StorageBoundaryError::Changed)?,
        );
        let lease = Self {
            data_path,
            database_path,
            blob_path,
            database_name,
            blob_name,
            ancestor_chain,
            data_directory,
            blob_directory,
            database_file,
            data_identity,
            blob_identity,
            database_identity,
            writer,
        };
        if validate_sidecars {
            lease.validate()?;
        } else {
            lease.validate_preflight()?;
        }
        Ok(lease)
    }

    pub(crate) fn harden_sqlite_sidecars(&self) -> Result<(), StorageBoundaryError> {
        if !self.writer {
            return Err(StorageBoundaryError::PrivateStorageUnavailable);
        }
        #[cfg(not(unix))]
        return Err(StorageBoundaryError::PrivateStorageUnavailable);
        #[cfg(unix)]
        {
            for suffix in ["-wal", "-shm", "-journal"] {
                let mut name = self.database_name.clone();
                name.push(suffix);
                match self.data_directory.symlink_metadata(&name) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(_) => return Err(StorageBoundaryError::PrivateStorageUnavailable),
                    Ok(metadata) => {
                        let identity = cap_identity(&metadata);
                        if metadata.file_type().is_symlink()
                            || !metadata.is_file()
                            || MetadataExt::nlink(&metadata) != 1
                        {
                            return Err(StorageBoundaryError::PrivateStorageUnavailable);
                        }
                        use cap_std::fs::MetadataExt as _;
                        if metadata.uid() != rustix::process::geteuid().as_raw() {
                            return Err(StorageBoundaryError::PrivateStorageUnavailable);
                        }
                        let file = open_existing_file(&self.data_directory, &name)?;
                        validate_cap_file(&file, identity)?;
                        use cap_std::fs::PermissionsExt;
                        file.set_permissions(cap_std::fs::Permissions::from_mode(0o600))
                            .map_err(|_| StorageBoundaryError::PrivateStorageUnavailable)?;
                        validate_private_cap_file(&file, identity)?;
                    }
                }
            }
            self.validate()
        }
    }

    fn validate_configuration_paths(
        &self,
        database_path: &Path,
        blob_path: &Path,
    ) -> Result<(), StorageBoundaryError> {
        if database_path != self.database_path || blob_path != self.blob_path {
            return Err(StorageBoundaryError::Changed);
        }
        Ok(())
    }

    fn validate_optional_sidecar(&self, suffix: &str) -> Result<(), StorageBoundaryError> {
        self.validate_optional_sidecar_with_hook(suffix, || {})
    }

    fn validate_optional_sidecar_with_hook(
        &self,
        suffix: &str,
        between_observations: impl FnOnce(),
    ) -> Result<(), StorageBoundaryError> {
        let mut name = self.database_name.clone();
        name.push(suffix);
        let path = self.data_path.join(&name);
        validate_optional_cap_sidecar(self.data_directory.symlink_metadata(&name))?;
        between_observations();
        validate_optional_std_sidecar(fs::symlink_metadata(&path))
    }
}

fn validate_optional_cap_sidecar(
    metadata: io::Result<cap_std::fs::Metadata>,
) -> Result<(), StorageBoundaryError> {
    match metadata {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(StorageBoundaryError::Changed),
        Ok(metadata) if private_cap_file_metadata_is_valid(&metadata) => Ok(()),
        Ok(_) => Err(StorageBoundaryError::PrivateStorageUnavailable),
    }
}

fn validate_optional_std_sidecar(
    metadata: io::Result<fs::Metadata>,
) -> Result<(), StorageBoundaryError> {
    match metadata {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(StorageBoundaryError::Changed),
        Ok(metadata) if private_std_file_metadata_is_valid(&metadata) => Ok(()),
        Ok(_) => Err(StorageBoundaryError::PrivateStorageUnavailable),
    }
}

#[cfg(unix)]
fn private_cap_file_metadata_is_valid(metadata: &cap_std::fs::Metadata) -> bool {
    use cap_std::fs::MetadataExt as _;
    !metadata.file_type().is_symlink()
        && metadata.is_file()
        && MetadataExt::nlink(metadata) == 1
        && metadata.uid() == rustix::process::geteuid().as_raw()
        && metadata.mode() & 0o777 == 0o600
}

#[cfg(not(unix))]
fn private_cap_file_metadata_is_valid(_: &cap_std::fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn private_std_file_metadata_is_valid(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    !metadata.file_type().is_symlink()
        && metadata.is_file()
        && MetadataExt::nlink(metadata) == 1
        && metadata.uid() == rustix::process::geteuid().as_raw()
        && metadata.mode() & 0o777 == 0o600
}

#[cfg(not(unix))]
fn private_std_file_metadata_is_valid(_: &fs::Metadata) -> bool {
    false
}

fn direct_child_name(parent: &Path, child: &Path) -> Result<OsString, StorageBoundaryError> {
    if child.parent() != Some(parent) {
        return Err(StorageBoundaryError::Changed);
    }
    child
        .file_name()
        .filter(|name| !name.is_empty())
        .map(OsStr::to_os_string)
        .ok_or(StorageBoundaryError::Changed)
}

#[cfg(unix)]
fn harden_fresh_empty_data_directory(path: &Path) -> Result<(), StorageBoundaryError> {
    use cap_std::fs::{MetadataExt as _, PermissionsExt};
    use std::os::unix::fs::MetadataExt as _;

    let metadata =
        fs::symlink_metadata(path).map_err(|_| StorageBoundaryError::PrivateStorageUnavailable)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
    {
        return Err(StorageBoundaryError::PrivateStorageUnavailable);
    }
    let identity = std_identity(&metadata);
    let directory = Dir::open_ambient_dir(path, ambient_authority())
        .map_err(|_| StorageBoundaryError::PrivateStorageUnavailable)?;
    validate_cap_directory(&directory, identity)?;
    let capability_metadata = directory
        .dir_metadata()
        .map_err(|_| StorageBoundaryError::PrivateStorageUnavailable)?;
    if capability_metadata.uid() != rustix::process::geteuid().as_raw()
        || directory
            .entries()
            .map_err(|_| StorageBoundaryError::PrivateStorageUnavailable)?
            .next()
            .is_some()
    {
        return Err(StorageBoundaryError::PrivateStorageUnavailable);
    }
    directory
        .set_permissions(".", cap_std::fs::Permissions::from_mode(0o700))
        .map_err(|_| StorageBoundaryError::PrivateStorageUnavailable)?;
    validate_private_cap_directory(&directory, identity)?;
    validate_private_std_directory(path, identity)
}

fn open_or_create_file(directory: &Dir, name: &OsStr) -> Result<File, StorageBoundaryError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .follow(FollowSymlinks::No);
    #[cfg(unix)]
    cap_std::fs::OpenOptionsExt::mode(&mut options, 0o600);
    directory
        .open_with(name, &options)
        .map_err(|_| StorageBoundaryError::Changed)
}

fn open_existing_file(directory: &Dir, name: &OsStr) -> Result<File, StorageBoundaryError> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    directory
        .open_with(name, &options)
        .map_err(|_| StorageBoundaryError::Changed)
}

fn validate_std_directory(path: &Path, expected: FileIdentity) -> Result<(), StorageBoundaryError> {
    let metadata = fs::symlink_metadata(path).map_err(changed)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || !std_identity(&metadata).same_directory(expected)
    {
        return Err(StorageBoundaryError::Changed);
    }
    Ok(())
}

#[cfg(unix)]
fn validate_private_std_directory(
    path: &Path,
    expected: FileIdentity,
) -> Result<(), StorageBoundaryError> {
    use std::os::unix::fs::MetadataExt as _;

    validate_std_directory(path, expected)?;
    let metadata =
        fs::symlink_metadata(path).map_err(|_| StorageBoundaryError::PrivateStorageUnavailable)?;
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o777 != 0o700 {
        return Err(StorageBoundaryError::PrivateStorageUnavailable);
    }
    Ok(())
}

fn validate_std_file(path: &Path, expected: FileIdentity) -> Result<(), StorageBoundaryError> {
    let metadata = fs::symlink_metadata(path).map_err(changed)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || !std_identity(&metadata).same_single_link_file(expected)
    {
        return Err(StorageBoundaryError::Changed);
    }
    Ok(())
}

#[cfg(unix)]
fn validate_private_std_file(
    path: &Path,
    expected: FileIdentity,
) -> Result<(), StorageBoundaryError> {
    use std::os::unix::fs::MetadataExt as _;

    validate_std_file(path, expected)?;
    let metadata =
        fs::symlink_metadata(path).map_err(|_| StorageBoundaryError::PrivateStorageUnavailable)?;
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o777 != 0o600 {
        return Err(StorageBoundaryError::PrivateStorageUnavailable);
    }
    Ok(())
}

fn validate_cap_directory(
    directory: &Dir,
    expected: FileIdentity,
) -> Result<(), StorageBoundaryError> {
    let metadata = directory
        .dir_metadata()
        .map_err(|_| StorageBoundaryError::Changed)?;
    if !metadata.is_dir() || !cap_identity(&metadata).same_directory(expected) {
        return Err(StorageBoundaryError::Changed);
    }
    Ok(())
}

fn validate_cap_file(file: &File, expected: FileIdentity) -> Result<(), StorageBoundaryError> {
    let metadata = file.metadata().map_err(|_| StorageBoundaryError::Changed)?;
    if !metadata.is_file() || !cap_identity(&metadata).same_single_link_file(expected) {
        return Err(StorageBoundaryError::Changed);
    }
    Ok(())
}

#[cfg(unix)]
fn validate_private_cap_directory(
    directory: &Dir,
    expected: FileIdentity,
) -> Result<(), StorageBoundaryError> {
    use cap_std::fs::MetadataExt as _;

    validate_cap_directory(directory, expected)?;
    let metadata = directory
        .dir_metadata()
        .map_err(|_| StorageBoundaryError::PrivateStorageUnavailable)?;
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o777 != 0o700 {
        return Err(StorageBoundaryError::PrivateStorageUnavailable);
    }
    Ok(())
}

#[cfg(unix)]
fn validate_private_cap_file(
    file: &File,
    expected: FileIdentity,
) -> Result<(), StorageBoundaryError> {
    let metadata = file
        .metadata()
        .map_err(|_| StorageBoundaryError::PrivateStorageUnavailable)?;
    validate_private_cap_file_metadata(&metadata, expected)
}

#[cfg(unix)]
fn validate_private_cap_file_metadata(
    metadata: &cap_std::fs::Metadata,
    expected: FileIdentity,
) -> Result<(), StorageBoundaryError> {
    use cap_std::fs::MetadataExt as _;

    if !metadata.is_file()
        || !cap_identity(metadata).same_single_link_file(expected)
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(StorageBoundaryError::PrivateStorageUnavailable);
    }
    Ok(())
}

fn std_identity(metadata: &fs::Metadata) -> FileIdentity {
    FileIdentity {
        device: MetadataExt::dev(metadata),
        inode: MetadataExt::ino(metadata),
        links: MetadataExt::nlink(metadata),
    }
}

fn cap_identity(metadata: &cap_std::fs::Metadata) -> FileIdentity {
    FileIdentity {
        device: MetadataExt::dev(metadata),
        inode: MetadataExt::ino(metadata),
        links: MetadataExt::nlink(metadata),
    }
}

impl FileIdentity {
    fn same_object(self, other: Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }

    fn same_directory(self, expected: Self) -> bool {
        self.links > 0 && expected.links > 0 && self.same_object(expected)
    }

    fn same_single_link_file(self, expected: Self) -> bool {
        self.links == 1 && expected.links == 1 && self.same_object(expected)
    }
}

fn open_directory_chain(path: &Path) -> Result<Vec<LeasedDirectory>, StorageBoundaryError> {
    let mut paths = path.ancestors().collect::<Vec<_>>();
    paths.reverse();
    let root_path = paths.first().ok_or(StorageBoundaryError::Changed)?;
    if !root_path.is_absolute() {
        return Err(StorageBoundaryError::Changed);
    }
    let root_directory = Dir::open_ambient_dir(root_path, ambient_authority())
        .map_err(|_| StorageBoundaryError::Changed)?;
    let root_identity = cap_identity(
        &root_directory
            .dir_metadata()
            .map_err(|_| StorageBoundaryError::Changed)?,
    );
    validate_std_directory(root_path, root_identity)?;
    let mut chain = vec![LeasedDirectory {
        path: (*root_path).to_path_buf(),
        name_from_parent: None,
        directory: root_directory,
        identity: root_identity,
    }];

    for next_path in paths.into_iter().skip(1) {
        let name = next_path
            .file_name()
            .filter(|name| !name.is_empty())
            .map(OsStr::to_os_string)
            .ok_or(StorageBoundaryError::Changed)?;
        let parent = chain.last().ok_or(StorageBoundaryError::Changed)?;
        let directory = parent
            .directory
            .open_dir_nofollow(&name)
            .map_err(|_| StorageBoundaryError::Changed)?;
        let identity = cap_identity(
            &directory
                .dir_metadata()
                .map_err(|_| StorageBoundaryError::Changed)?,
        );
        validate_std_directory(next_path, identity)?;
        chain.push(LeasedDirectory {
            path: next_path.to_path_buf(),
            name_from_parent: Some(name),
            directory,
            identity,
        });
    }
    Ok(chain)
}

fn validate_directory_chain(chain: &[LeasedDirectory]) -> Result<(), StorageBoundaryError> {
    for (index, leased) in chain.iter().enumerate() {
        validate_std_directory(&leased.path, leased.identity)?;
        validate_cap_directory(&leased.directory, leased.identity)?;
        if index == 0 {
            continue;
        }
        let parent = chain.get(index - 1).ok_or(StorageBoundaryError::Changed)?;
        let name = leased
            .name_from_parent
            .as_ref()
            .ok_or(StorageBoundaryError::Changed)?;
        let reopened = parent
            .directory
            .open_dir_nofollow(name)
            .map_err(|_| StorageBoundaryError::Changed)?;
        validate_cap_directory(&reopened, leased.identity)?;
    }
    Ok(())
}

fn changed(_: io::Error) -> StorageBoundaryError {
    StorageBoundaryError::Changed
}

#[cfg(test)]
mod tests {
    use std::fs;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use crate::StoreConfig;

    use super::StorageBoundaryLease;

    #[test]
    fn legal_sidecar_recreation_between_observations_is_accepted() {
        let directory = tempfile::tempdir().unwrap();
        let config = StoreConfig::new(directory.path().join("synthetic.db"));
        let lease = StorageBoundaryLease::create_writer(&config).unwrap();
        let sidecar = lease.data_path.join("synthetic.db-wal");
        let retained = lease.data_path.join("retained-sidecar");
        fs::write(&sidecar, b"old").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o600)).unwrap();

        lease
            .validate_optional_sidecar_with_hook("-wal", || {
                fs::rename(&sidecar, &retained).unwrap();
                fs::write(&sidecar, b"new").unwrap();
                #[cfg(unix)]
                fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o600)).unwrap();
            })
            .unwrap();
    }
}
