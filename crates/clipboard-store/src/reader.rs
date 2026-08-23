use std::{sync::Arc, time::Duration};

use rusqlite::{Connection, OpenFlags};

use crate::{
    StorageBoundaryLease, StoreConfig, StoreError,
    writer::{RuntimeRef, acquire_read_runtime},
};

pub struct ReadOnlyStore {
    config: StoreConfig,
    runtime: RuntimeRef,
}

impl ReadOnlyStore {
    pub fn open_existing(mut config: StoreConfig) -> Result<Self, StoreError> {
        if !config.database_path().is_file() {
            return Err(StoreError::DatabaseMissing);
        }
        let boundary = match config.storage_boundary() {
            Some(boundary) => {
                boundary
                    .validate_preflight_for_config(&config, false)
                    .map_err(StoreError::from)?;
                Arc::clone(boundary)
            }
            None => {
                let boundary = Arc::new(
                    StorageBoundaryLease::open_read_only_preflight(&config)
                        .map_err(StoreError::from)?,
                );
                config.set_storage_boundary(Arc::clone(&boundary));
                boundary
            }
        };
        let runtime = acquire_read_runtime(&config, Arc::clone(&boundary))?;
        boundary
            .validate_for_config(&config, false)
            .map_err(StoreError::from)?;
        Ok(Self { config, runtime })
    }

    pub fn with_reader<T>(
        &self,
        operation: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, StoreError> {
        self.runtime.with_reader(&self.config, operation)
    }

    pub fn cas_store(&self) -> Result<crate::CasStore, StoreError> {
        let boundary = Arc::clone(required_boundary(&self.config)?);
        Ok(crate::CasStore::with_storage_boundary(
            self.config.blob_root().to_path_buf(),
            boundary,
        ))
    }
}

pub(crate) fn open_writer_connection(config: &StoreConfig) -> Result<Connection, StoreError> {
    let boundary = required_boundary(config)?;
    boundary
        .validate_for_config(config, true)
        .map_err(StoreError::from)?;
    let connection = Connection::open_with_flags(
        config.database_path(),
        OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    boundary.validate().map_err(StoreError::from)?;
    enable_wal(&connection)?;
    configure_connection(&connection)?;
    boundary
        .harden_sqlite_sidecars()
        .map_err(StoreError::from)?;
    boundary.validate().map_err(StoreError::from)?;
    Ok(connection)
}

pub(crate) fn open_reader_connection(config: &StoreConfig) -> Result<Connection, StoreError> {
    let boundary = required_boundary(config)?;
    boundary
        .validate_for_config(config, false)
        .map_err(StoreError::from)?;
    let connection = Connection::open_with_flags(
        config.database_path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    boundary.validate().map_err(StoreError::from)?;
    configure_connection(&connection)?;
    connection.execute_batch("PRAGMA query_only = ON;")?;
    boundary.validate().map_err(StoreError::from)?;
    Ok(connection)
}

pub(crate) fn required_boundary(
    config: &StoreConfig,
) -> Result<&Arc<StorageBoundaryLease>, StoreError> {
    config.storage_boundary().ok_or(StoreError::StorageBoundary)
}

fn enable_wal(connection: &Connection) -> Result<(), StoreError> {
    connection.query_row("PRAGMA journal_mode = WAL", [], |row| {
        row.get::<_, String>(0)
    })?;
    Ok(())
}

fn configure_connection(connection: &Connection) -> Result<(), StoreError> {
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;\
         PRAGMA synchronous = NORMAL;\
         PRAGMA cache_size = -65536;",
    )?;
    connection.busy_timeout(Duration::from_millis(5_000))?;

    let journal_mode =
        connection.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::WalUnavailable(journal_mode));
    }
    Ok(())
}
