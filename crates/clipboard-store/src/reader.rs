use std::time::Duration;

use rusqlite::Connection;

use crate::{StoreConfig, StoreError};

pub(crate) fn open_connection(config: &StoreConfig) -> Result<Connection, StoreError> {
    let connection = Connection::open(config.database_path())?;
    configure_connection(&connection)?;
    Ok(connection)
}

fn configure_connection(connection: &Connection) -> Result<(), StoreError> {
    connection.query_row("PRAGMA journal_mode = WAL", [], |row| {
        row.get::<_, String>(0)
    })?;
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
