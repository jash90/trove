use rusqlite::{Connection, TransactionBehavior};

use crate::StoreError;

const INITIAL_MIGRATION: &str = include_str!("migrations/001_initial.sql");
const LATEST_SCHEMA_VERSION: i64 = 1;

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
            return Ok(());
        }

        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(INITIAL_MIGRATION)?;
        transaction.pragma_update(None, "user_version", LATEST_SCHEMA_VERSION)?;
        transaction.commit()?;
        Ok(())
    }
}
