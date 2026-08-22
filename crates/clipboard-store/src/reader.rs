use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::{StoreConfig, StoreError, migrations::validate_current_schema};

pub struct ReadOnlyStore {
    config: StoreConfig,
}

impl ReadOnlyStore {
    pub fn open_existing(config: StoreConfig) -> Result<Self, StoreError> {
        if !config.database_path().is_file() {
            return Err(StoreError::DatabaseMissing);
        }
        let connection = open_reader_connection(&config)?;
        validate_current_schema(&connection)?;
        Ok(Self { config })
    }

    pub fn with_reader<T>(
        &self,
        operation: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, StoreError> {
        let connection = open_reader_connection(&self.config)?;
        Ok(operation(&connection)?)
    }
}

pub(crate) fn open_writer_connection(config: &StoreConfig) -> Result<Connection, StoreError> {
    let connection = Connection::open(config.database_path())?;
    enable_wal(&connection)?;
    configure_connection(&connection)?;
    Ok(connection)
}

pub(crate) fn open_reader_connection(config: &StoreConfig) -> Result<Connection, StoreError> {
    let connection =
        Connection::open_with_flags(config.database_path(), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    configure_connection(&connection)?;
    connection.execute_batch("PRAGMA query_only = ON;")?;
    Ok(connection)
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
