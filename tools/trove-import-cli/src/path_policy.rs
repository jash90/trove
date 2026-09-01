use std::{
    ffi::OsString,
    fs, io,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use cap_fs_ext::DirExt;
use cap_std::{ambient_authority, fs::Dir};
use trove_store::{StorageBoundaryLease, StoreConfig};

use crate::{CliFailure, boundary_failure};

use trove_store::{BLOB_DIRECTORY_NAME as BLOB_DIRECTORY, DATABASE_FILENAME};

pub(crate) struct ValidatedImportPaths {
    pub(crate) source: PathBuf,
    pub(crate) data_dir: PathBuf,
    pub(crate) database_path: PathBuf,
    pub(crate) blob_root: PathBuf,
    storage_boundary: Arc<StorageBoundaryLease>,
}

impl ValidatedImportPaths {
    pub(crate) fn store_config(&self) -> StoreConfig {
        StoreConfig::new(&self.database_path)
            .with_blob_root(&self.blob_root)
            .with_storage_boundary(Arc::clone(&self.storage_boundary))
    }
}

pub(crate) fn prepare_import_paths(
    source: &Path,
    data_dir: &Path,
) -> Result<ValidatedImportPaths, CliFailure> {
    prepare_import_paths_with_hooks(source, data_dir, |_| {}, |_| {})
}

#[cfg(test)]
fn prepare_import_paths_with_component_hook(
    source: &Path,
    data_dir: &Path,
    before_component_create: impl FnMut(&Path),
) -> Result<ValidatedImportPaths, CliFailure> {
    prepare_import_paths_with_hooks(source, data_dir, before_component_create, |_| {})
}

fn prepare_import_paths_with_hooks(
    source: &Path,
    data_dir: &Path,
    mut before_component_create: impl FnMut(&Path),
    after_lease: impl FnOnce(&ValidatedImportPaths),
) -> Result<ValidatedImportPaths, CliFailure> {
    let source = canonical_source(source)?;
    let resolved = resolve_data_candidate(data_dir)?;
    validate_data_boundary(&resolved.candidate)?;
    validate_nonoverlap(&source, &resolved.candidate)?;
    let _data_directory =
        create_missing_components_no_follow(&resolved, &mut before_component_create)?;
    let data_dir = fs::canonicalize(&resolved.candidate)
        .map_err(|_| CliFailure::new("storage_unavailable"))?;
    if data_dir != resolved.candidate {
        return Err(CliFailure::new("unsafe_storage_layout"));
    }
    validate_data_boundary(&data_dir)?;
    let current_source = canonical_source(&source)?;
    if current_source != source {
        return Err(CliFailure::new("source_changed"));
    }
    validate_nonoverlap(&source, &data_dir)?;

    let database_path = data_dir.join(DATABASE_FILENAME);
    let blob_root = data_dir.join(BLOB_DIRECTORY);
    ensure_optional_child(&data_dir, &database_path, ChildKind::File)?;
    let config = StoreConfig::new(&database_path).with_blob_root(&blob_root);
    let storage_boundary =
        Arc::new(StorageBoundaryLease::create_writer(&config).map_err(boundary_failure)?);
    let paths = ValidatedImportPaths {
        source,
        data_dir,
        database_path,
        blob_root,
        storage_boundary,
    };
    after_lease(&paths);
    verify_created_storage(&paths)?;
    Ok(paths)
}

pub(crate) fn verify_created_storage(paths: &ValidatedImportPaths) -> Result<(), CliFailure> {
    paths
        .storage_boundary
        .validate()
        .map_err(boundary_failure)?;
    ensure_exact_directory(&paths.data_dir)?;
    ensure_existing_child(&paths.data_dir, &paths.database_path, ChildKind::File)?;
    ensure_existing_child(&paths.data_dir, &paths.blob_root, ChildKind::Directory)
}

pub(crate) fn verified_read_only_config(data_dir: &Path) -> Result<StoreConfig, CliFailure> {
    let candidate = resolve_data_candidate(data_dir)?.candidate;
    validate_data_boundary(&candidate)?;
    if !candidate.exists() {
        return Err(CliFailure::new("database_missing"));
    }
    if !candidate.is_dir() {
        return Err(CliFailure::new("unsafe_storage_layout"));
    }
    let data_dir =
        fs::canonicalize(candidate).map_err(|_| CliFailure::new("verification_unavailable"))?;
    validate_data_boundary(&data_dir)?;
    let database_path = data_dir.join(DATABASE_FILENAME);
    let blob_root = data_dir.join(BLOB_DIRECTORY);
    if !ensure_optional_child(&data_dir, &database_path, ChildKind::File)? {
        return Err(CliFailure::new("database_missing"));
    }
    Ok(StoreConfig::new(database_path).with_blob_root(blob_root))
}

pub(crate) fn exact_blob_root_is_valid(data_dir: &Path, blob_root: &Path) -> bool {
    ensure_exact_directory(data_dir).is_ok()
        && ensure_existing_child(data_dir, blob_root, ChildKind::Directory).is_ok()
}

pub(crate) fn canonical_source(path: &Path) -> Result<PathBuf, CliFailure> {
    let canonical = fs::canonicalize(path).map_err(|_| CliFailure::new("source_unavailable"))?;
    let metadata = fs::metadata(&canonical).map_err(|_| CliFailure::new("source_unavailable"))?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(CliFailure::new("source_unavailable"));
    }
    Ok(canonical)
}

struct ResolvedDataCandidate {
    candidate: PathBuf,
    existing_ancestor: PathBuf,
    missing_components: Vec<OsString>,
}

fn resolve_data_candidate(path: &Path) -> Result<ResolvedDataCandidate, CliFailure> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| CliFailure::new("unsafe_data_dir"))?
            .join(path)
    };
    if absolute
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(CliFailure::new("unsafe_data_dir"));
    }

    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::<OsString>::new();
    loop {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                if !metadata.is_dir() && !metadata.file_type().is_symlink() {
                    return Err(CliFailure::new("unsafe_storage_layout"));
                }
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = ancestor
                    .file_name()
                    .ok_or_else(|| CliFailure::new("unsafe_data_dir"))?;
                suffix.push(name.to_os_string());
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| CliFailure::new("unsafe_data_dir"))?;
            }
            Err(_) => return Err(CliFailure::new("unsafe_data_dir")),
        }
    }
    let mut resolved =
        fs::canonicalize(ancestor).map_err(|_| CliFailure::new("unsafe_data_dir"))?;
    if !resolved.is_dir() {
        return Err(CliFailure::new("unsafe_storage_layout"));
    }
    suffix.reverse();
    for component in &suffix {
        resolved.push(component);
    }
    Ok(ResolvedDataCandidate {
        candidate: resolved,
        existing_ancestor: fs::canonicalize(ancestor)
            .map_err(|_| CliFailure::new("unsafe_data_dir"))?,
        missing_components: suffix,
    })
}

fn create_missing_components_no_follow(
    resolved: &ResolvedDataCandidate,
    before_component_create: &mut impl FnMut(&Path),
) -> Result<Dir, CliFailure> {
    let mut directory = Dir::open_ambient_dir(&resolved.existing_ancestor, ambient_authority())
        .map_err(|_| CliFailure::new("storage_unavailable"))?;
    let mut current_path = resolved.existing_ancestor.clone();
    for component in &resolved.missing_components {
        current_path.push(component);
        before_component_create(&current_path);
        let mut builder = cap_std::fs::DirBuilder::new();
        #[cfg(unix)]
        cap_std::fs::DirBuilderExt::mode(&mut builder, 0o700);
        match directory.create_dir_with(Path::new(component), &builder) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(CliFailure::new("storage_unavailable")),
        }
        directory = directory
            .open_dir_nofollow(Path::new(component))
            .map_err(|_| CliFailure::new("unsafe_storage_layout"))?;
    }
    ensure_exact_directory(&resolved.candidate)?;
    Ok(directory)
}

fn ensure_exact_directory(path: &Path) -> Result<(), CliFailure> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| CliFailure::new("unsafe_storage_layout"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CliFailure::new("unsafe_storage_layout"));
    }
    let canonical = fs::canonicalize(path).map_err(|_| CliFailure::new("unsafe_storage_layout"))?;
    if canonical != path {
        return Err(CliFailure::new("unsafe_storage_layout"));
    }
    Ok(())
}

fn validate_data_boundary(data_dir: &Path) -> Result<(), CliFailure> {
    if data_dir.parent().is_none() {
        return Err(CliFailure::new("unsafe_data_dir"));
    }
    let base_dirs =
        directories::BaseDirs::new().ok_or_else(|| CliFailure::new("unsafe_data_dir"))?;
    let home =
        fs::canonicalize(base_dirs.home_dir()).map_err(|_| CliFailure::new("unsafe_data_dir"))?;
    if data_dir == home {
        return Err(CliFailure::new("unsafe_data_dir"));
    }
    Ok(())
}

fn validate_nonoverlap(source: &Path, data_dir: &Path) -> Result<(), CliFailure> {
    if source == data_dir || source.starts_with(data_dir) || data_dir.starts_with(source) {
        return Err(CliFailure::new("overlapping_paths"));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ChildKind {
    File,
    Directory,
}

fn ensure_optional_child(
    data_dir: &Path,
    child: &Path,
    expected: ChildKind,
) -> Result<bool, CliFailure> {
    let metadata = match fs::symlink_metadata(child) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(CliFailure::new("unsafe_storage_layout")),
    };
    if metadata.file_type().is_symlink()
        || match expected {
            ChildKind::File => !metadata.is_file(),
            ChildKind::Directory => !metadata.is_dir(),
        }
    {
        return Err(CliFailure::new("unsafe_storage_layout"));
    }
    let canonical =
        fs::canonicalize(child).map_err(|_| CliFailure::new("unsafe_storage_layout"))?;
    if !canonical.starts_with(data_dir) {
        return Err(CliFailure::new("unsafe_storage_layout"));
    }
    Ok(true)
}

fn ensure_existing_child(
    data_dir: &Path,
    child: &Path,
    expected: ChildKind,
) -> Result<(), CliFailure> {
    if !ensure_optional_child(data_dir, child, expected)? {
        return Err(CliFailure::new("unsafe_storage_layout"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, fs};

    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt, symlink};

    use tempfile::tempdir;

    use super::{
        BLOB_DIRECTORY, DATABASE_FILENAME, prepare_import_paths_with_component_hook,
        prepare_import_paths_with_hooks,
    };

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_substitution_between_resolution_and_component_creation() {
        let temporary = tempdir().expect("temporary directory");
        let source = temporary.path().join("synthetic-source.json");
        fs::write(&source, b"synthetic").expect("synthetic source");
        let existing_parent = temporary.path().join("storage-parent");
        fs::create_dir(&existing_parent).expect("storage parent");
        let outside = temporary.path().join("outside-target");
        fs::create_dir(&outside).expect("outside target");
        let marker = outside.join("must-remain");
        fs::write(&marker, b"unchanged").expect("outside marker");
        let data_dir = existing_parent.join("pending").join("data");
        let substituted = Cell::new(false);

        let result = prepare_import_paths_with_component_hook(&source, &data_dir, |component| {
            if !substituted.replace(true) {
                symlink(&outside, component).expect("insert deterministic substitution");
            }
        });

        let error = match result {
            Ok(_) => panic!("substitution must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.code, "unsafe_storage_layout");
        assert_eq!(
            fs::read(&marker).expect("outside marker remains readable"),
            b"unchanged"
        );
        assert!(!outside.join("data").exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_data_directory_replacement_after_lease_before_store_handoff() {
        let temporary = tempdir().expect("temporary directory");
        let source = temporary.path().join("synthetic-source.json");
        fs::write(&source, b"synthetic").expect("synthetic source");
        let data_dir = temporary.path().join("leased-data");
        let retained = temporary.path().join("retained-data");

        let result = prepare_import_paths_with_hooks(
            &source,
            &data_dir,
            |_| {},
            |_| {
                fs::rename(&data_dir, &retained).expect("retain leased directory");
                fs::create_dir(&data_dir).expect("insert replacement directory");
                fs::write(data_dir.join("outside-marker"), b"unchanged").expect("outside marker");
            },
        );

        let error = match result {
            Ok(_) => panic!("post-lease replacement must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.code, "unsafe_storage_layout");
        assert_eq!(
            fs::read(data_dir.join("outside-marker")).expect("outside marker remains readable"),
            b"unchanged"
        );
        assert!(!data_dir.join(DATABASE_FILENAME).exists());
        assert!(!data_dir.join(BLOB_DIRECTORY).exists());
    }

    #[cfg(unix)]
    #[test]
    fn private_storage_at_writer_lease_revalidation_has_a_stable_code() {
        let temporary = tempdir().expect("temporary directory");
        let source = temporary.path().join("synthetic-source.json");
        fs::write(&source, b"synthetic").expect("synthetic source");
        let data_dir = temporary.path().join("leased-data");

        let error = match prepare_import_paths_with_hooks(
            &source,
            &data_dir,
            |_| {},
            |paths| {
                fs::set_permissions(&paths.blob_root, fs::Permissions::from_mode(0o755))
                    .expect("unsafe synthetic blob permissions");
            },
        ) {
            Ok(_) => panic!("private storage must be rejected"),
            Err(error) => error,
        };

        assert_eq!(error.code, "private_storage_unavailable");
    }
}
