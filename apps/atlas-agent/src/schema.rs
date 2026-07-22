//! Neutral agent database schema generation.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, TransactionBehavior};
use thiserror::Error;

pub const LATEST_SCHEMA_VERSION: i64 = 4;

const MIGRATION_1: &str = include_str!("../migrations/0001_initial.sql");

#[derive(Debug, Error)]
pub enum AgentSchemaError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("agent schema is at version {found}, expected {expected}")]
    SchemaVersion { found: i64, expected: i64 },
}

/// Creates a fresh generation-4 agent database. Persistent deployments must
/// invoke this through the project's backup-first migration wrapper.
pub fn migrate(path: impl AsRef<Path>) -> Result<(), AgentSchemaError> {
    let path = path.as_ref();
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "temp_store", "MEMORY")?;
    let version =
        connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
    if version == LATEST_SCHEMA_VERSION {
        return Ok(());
    }
    if version != 0 {
        return Err(AgentSchemaError::SchemaVersion {
            found: version,
            expected: LATEST_SCHEMA_VERSION,
        });
    }
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(MIGRATION_1)?;
    transaction.pragma_update(None, "user_version", LATEST_SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}
