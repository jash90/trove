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
    #[error("invalid CAS relative path: {0}")]
    InvalidRelativePath(String),
    #[error("CAS filesystem operation failed at {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
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
    fail_after_bytes: Option<usize>,
}

impl CasStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            #[cfg(test)]
            fail_after_bytes: None,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn put(&self, bytes: &[u8]) -> Result<CasBlob, CasError> {
        let hash = *blake3::hash(bytes).as_bytes();
        let hex_hash = blake3::Hash::from(hash).to_hex().to_string();
        let relpath = format!("{}/{hex_hash}", &hex_hash[..2]);
        let final_path = self.path_for(&relpath)?;

        if final_path.exists() {
            return Ok(CasBlob {
                hash,
                relpath,
                byte_size: bytes.len() as u64,
            });
        }

        let final_parent = final_path.parent().expect("CAS blob path has a parent");
        self.create_dir_all(final_parent)?;
        let temp_directory = self.root.join(".tmp");
        self.create_dir_all(&temp_directory)?;
        let temp_path = temp_directory.join(Uuid::now_v7().simple().to_string());

        let result = (|| -> io::Result<()> {
            let mut temporary = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)?;
            self.write_all(&mut temporary, bytes)?;
            temporary.sync_all()?;
            drop(temporary);

            if !final_path.exists() {
                fs::rename(&temp_path, &final_path)?;
            }
            Ok(())
        })();

        if let Err(source) = result {
            let _ = fs::remove_file(&temp_path);
            return Err(CasError::Io {
                path: temp_path,
                source,
            });
        }

        if temp_path.exists() {
            fs::remove_file(&temp_path).map_err(|source| CasError::Io {
                path: temp_path.clone(),
                source,
            })?;
        }

        Ok(CasBlob {
            hash,
            relpath,
            byte_size: bytes.len() as u64,
        })
    }

    pub fn read(&self, relpath: &str) -> Result<Vec<u8>, CasError> {
        let path = self.path_for(relpath)?;
        fs::read(&path).map_err(|source| CasError::Io { path, source })
    }

    pub fn remove_orphans(&self, live_relpaths: &BTreeSet<String>) -> Result<(), CasError> {
        if !self.root.exists() {
            return Ok(());
        }

        let live_relpaths = live_relpaths
            .iter()
            .filter(|relpath| is_valid_relpath(relpath))
            .collect::<BTreeSet<_>>();
        for directory in self.read_dir(&self.root)? {
            let directory = directory.map_err(|source| CasError::Io {
                path: self.root.clone(),
                source,
            })?;
            let file_type = directory.file_type().map_err(|source| CasError::Io {
                path: directory.path(),
                source,
            })?;
            let directory_name = directory.file_name().to_string_lossy().into_owned();
            if !file_type.is_dir() || !is_lower_hex(&directory_name, 2) {
                continue;
            }

            for entry in self.read_dir(&directory.path())? {
                let entry = entry.map_err(|source| CasError::Io {
                    path: directory.path(),
                    source,
                })?;
                let file_type = entry.file_type().map_err(|source| CasError::Io {
                    path: entry.path(),
                    source,
                })?;
                if !file_type.is_file() {
                    continue;
                }
                let filename = entry.file_name().to_string_lossy().into_owned();
                let relpath = format!("{directory_name}/{filename}");
                if is_valid_relpath(&relpath) && !live_relpaths.contains(&relpath) {
                    let path = self.path_for(&relpath)?;
                    fs::remove_file(&path).map_err(|source| CasError::Io { path, source })?;
                }
            }
        }
        Ok(())
    }

    fn path_for(&self, relpath: &str) -> Result<PathBuf, CasError> {
        if !is_valid_relpath(relpath) {
            return Err(CasError::InvalidRelativePath(relpath.to_owned()));
        }
        let path = self.root.join(relpath);
        if !path.starts_with(&self.root) {
            return Err(CasError::InvalidRelativePath(relpath.to_owned()));
        }
        Ok(path)
    }

    fn create_dir_all(&self, path: &Path) -> Result<(), CasError> {
        fs::create_dir_all(path).map_err(|source| CasError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    fn read_dir(&self, path: &Path) -> Result<fs::ReadDir, CasError> {
        fs::read_dir(path).map_err(|source| CasError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    fn write_all(&self, file: &mut File, bytes: &[u8]) -> io::Result<()> {
        #[cfg(test)]
        if let Some(limit) = self.fail_after_bytes
            && bytes.len() > limit
        {
            if limit > 0 {
                file.write_all(&bytes[..limit])?;
            }
            return Err(io::Error::other("deterministic test write failure"));
        }
        file.write_all(bytes)
    }
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
mod tests {
    use std::{fs, path::Path};

    use super::CasStore;

    #[test]
    fn failed_atomic_write_leaves_no_final_or_temporary_blob() {
        let directory = tempfile::tempdir().unwrap();
        let cas = CasStore {
            root: directory.path().join("blobs"),
            fail_after_bytes: Some(0),
        };

        assert!(cas.put(b"payload").is_err());
        assert_eq!(files_under(cas.root()), Vec::<String>::new());
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
