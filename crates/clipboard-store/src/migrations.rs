use rusqlite::{Connection, TransactionBehavior};

use crate::StoreError;

const INITIAL_MIGRATION: &str = include_str!("migrations/001_initial.sql");
const SETTINGS_MIGRATION: &str = include_str!("migrations/002_settings.sql");
const BLOB_REFERENCE_INDEX_MIGRATION: &str =
    include_str!("migrations/003_blob_reference_indexes.sql");
const LATEST_SCHEMA_VERSION: i64 = 3;
const SCHEMA_IDENTITY: &str = "clipboard-store";
const SCHEMA_REVISION: i64 = 6;

pub struct Migrations;

pub fn migrations() -> Migrations {
    Migrations
}

impl Migrations {
    pub fn validate(&self) -> Result<(), StoreError> {
        let mut connection = Connection::open_in_memory()?;
        self.apply(&mut connection)
    }

    pub(crate) fn apply(&self, connection: &mut Connection) -> Result<(), StoreError> {
        self.apply_with_hook(connection, || {})
    }

    fn apply_with_hook(
        &self,
        connection: &mut Connection,
        before_transaction: impl FnOnce(),
    ) -> Result<(), StoreError> {
        before_transaction();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version =
            transaction.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
        if version > LATEST_SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchemaVersion(version));
        }
        if version == LATEST_SCHEMA_VERSION {
            validate_schema_identity(&transaction)?;
            transaction.commit()?;
            return Ok(());
        }

        if version < 1 {
            transaction.execute_batch(INITIAL_MIGRATION)?;
            transaction.pragma_update(None, "user_version", 1_i64)?;
        }
        if version < 2 {
            validate_schema_identity(&transaction)?;
            transaction.execute_batch(SETTINGS_MIGRATION)?;
            transaction.pragma_update(None, "user_version", 2_i64)?;
        }
        if version < 3 {
            // Indexes only. The schema identity marks the shape of the tables,
            // which this does not touch, so a database that has these and one
            // that does not hold the same data and stay mutually readable.
            transaction.execute_batch(BLOB_REFERENCE_INDEX_MIGRATION)?;
            transaction.pragma_update(None, "user_version", 3_i64)?;
        }
        validate_schema_identity(&transaction)?;
        transaction.commit()?;
        Ok(())
    }
}

pub(crate) fn validate_current_schema(connection: &Connection) -> Result<(), StoreError> {
    let version =
        connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
    if version != LATEST_SCHEMA_VERSION {
        return Err(if version > LATEST_SCHEMA_VERSION {
            StoreError::UnsupportedSchemaVersion(version)
        } else {
            StoreError::IncompatibleSchema
        });
    }
    validate_schema_identity(connection)
}

fn validate_schema_identity(connection: &Connection) -> Result<(), StoreError> {
    let marker = connection.query_row(
        "SELECT identity, revision FROM schema_identity WHERE identity = ?1",
        [SCHEMA_IDENTITY],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
    );
    match marker {
        Ok((identity, revision)) if identity == SCHEMA_IDENTITY && revision == SCHEMA_REVISION => {
            Ok(())
        }
        _ => Err(StoreError::IncompatibleSchema),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Barrier},
        thread,
        time::Duration,
    };

    use rusqlite::Connection;

    use super::{INITIAL_MIGRATION, migrations};

    #[test]
    fn concurrent_independent_connections_upgrade_revision_six_once_without_data_loss() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("history.sqlite");
        let connection = Connection::open(&database_path).unwrap();
        connection.execute_batch(INITIAL_MIGRATION).unwrap();
        connection
            .pragma_update(None, "user_version", 1_i64)
            .unwrap();
        connection
            .execute_batch(
                "CREATE TABLE migration_sentinel(value TEXT NOT NULL);
                 INSERT INTO migration_sentinel(value) VALUES ('preserved');",
            )
            .unwrap();
        drop(connection);

        let rendezvous = Arc::new(Barrier::new(2));
        let mut upgrades = Vec::new();
        for _ in 0..2 {
            let database_path = database_path.clone();
            let rendezvous = Arc::clone(&rendezvous);
            upgrades.push(thread::spawn(move || {
                let mut connection = Connection::open(database_path).unwrap();
                connection.busy_timeout(Duration::from_secs(5)).unwrap();
                migrations().apply_with_hook(&mut connection, || {
                    rendezvous.wait();
                })
            }));
        }

        for upgrade in upgrades {
            upgrade.join().unwrap().unwrap();
        }
        let connection = Connection::open(database_path).unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let revision: i64 = connection
            .query_row(
                "SELECT revision FROM schema_identity WHERE identity = 'clipboard-store'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let sentinel: String = connection
            .query_row("SELECT value FROM migration_sentinel", [], |row| row.get(0))
            .unwrap();

        assert_eq!(version, 3);
        assert_eq!(revision, 6);
        assert_eq!(sentinel, "preserved");
    }
}
