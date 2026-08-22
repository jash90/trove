use std::{
    ffi::OsString,
    fs, io,
    path::{Component, Path, PathBuf},
};

use clipboard_store::StoreConfig;

use crate::CliFailure;

const DATABASE_FILENAME: &str = "clipboard.db";
const BLOB_DIRECTORY: &str = "blobs";

pub(crate) struct ValidatedImportPaths {
    pub(crate) source: PathBuf,
    pub(crate) data_dir: PathBuf,
    pub(crate) database_path: PathBuf,
    pub(crate) blob_root: PathBuf,
}

pub(crate) fn prepare_import_paths(
    source: &Path,
    data_dir: &Path,
) -> Result<ValidatedImportPaths, CliFailure> {
    let source = canonical_source(source)?;
    let candidate = resolve_data_candidate(data_dir)?;
    validate_data_boundary(&candidate)?;
    validate_nonoverlap(&source, &candidate)?;
    if candidate.exists() && !candidate.is_dir() {
        return Err(CliFailure::new("unsafe_storage_layout"));
    }
    fs::create_dir_all(&candidate).map_err(|_| CliFailure::new("storage_unavailable"))?;
    let data_dir =
        fs::canonicalize(&candidate).map_err(|_| CliFailure::new("storage_unavailable"))?;
    validate_data_boundary(&data_dir)?;
    let current_source = canonical_source(&source)?;
    if current_source != source {
        return Err(CliFailure::new("source_changed"));
    }
    validate_nonoverlap(&source, &data_dir)?;

    let database_path = data_dir.join(DATABASE_FILENAME);
    let blob_root = data_dir.join(BLOB_DIRECTORY);
    ensure_optional_child(&data_dir, &database_path, ChildKind::File)?;
    let blobs_exist = ensure_optional_child(&data_dir, &blob_root, ChildKind::Directory)?;
    if !blobs_exist {
        fs::create_dir(&blob_root).map_err(|_| CliFailure::new("storage_unavailable"))?;
    }
    ensure_existing_child(&data_dir, &blob_root, ChildKind::Directory)?;
    Ok(ValidatedImportPaths {
        source,
        data_dir,
        database_path,
        blob_root,
    })
}

pub(crate) fn verify_created_storage(paths: &ValidatedImportPaths) -> Result<(), CliFailure> {
    ensure_existing_child(&paths.data_dir, &paths.database_path, ChildKind::File)?;
    ensure_existing_child(&paths.data_dir, &paths.blob_root, ChildKind::Directory)
}

pub(crate) fn verified_read_only_config(data_dir: &Path) -> Result<StoreConfig, CliFailure> {
    let candidate = resolve_data_candidate(data_dir)?;
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
    ensure_optional_child(&data_dir, &blob_root, ChildKind::Directory)?;
    Ok(StoreConfig::new(database_path).with_blob_root(blob_root))
}

pub(crate) fn canonical_source(path: &Path) -> Result<PathBuf, CliFailure> {
    let canonical = fs::canonicalize(path).map_err(|_| CliFailure::new("source_unavailable"))?;
    let metadata = fs::metadata(&canonical).map_err(|_| CliFailure::new("source_unavailable"))?;
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(CliFailure::new("source_unavailable"));
    }
    Ok(canonical)
}

fn resolve_data_candidate(path: &Path) -> Result<PathBuf, CliFailure> {
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
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
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
