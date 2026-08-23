use rusqlite::{Connection, TransactionBehavior};

use crate::StoreError;

const INITIAL_MIGRATION: &str = include_str!("migrations/001_initial.sql");
const LATEST_SCHEMA_VERSION: i64 = 1;
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
        let version =
            connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
        if version > LATEST_SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchemaVersion(version));
        }
        if version == LATEST_SCHEMA_VERSION {
            return validate_schema_identity(connection);
        }

        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(INITIAL_MIGRATION)?;
        transaction.pragma_update(None, "user_version", LATEST_SCHEMA_VERSION)?;
        transaction.commit()?;
        validate_schema_identity(connection)
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
