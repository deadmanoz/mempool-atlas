//! Neutral agent database schema generation.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, TransactionBehavior};
use thiserror::Error;

pub const LATEST_SCHEMA_VERSION: i64 = 5;

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

/// Creates a fresh generation-5 agent database. Persistent deployments must
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

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use super::*;

    #[test]
    fn fresh_schema_enforces_identifier_byte_limits() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("atlas-agent.db");
        migrate(&path).expect("migrate fresh database");
        let connection = Connection::open(path).expect("open migrated database");
        connection
            .pragma_update(None, "foreign_keys", true)
            .expect("enable foreign keys");

        let at_limit = "a".repeat(atlas_model::MAX_IDENTIFIER_BYTES);
        let over_limit = "a".repeat(atlas_model::MAX_IDENTIFIER_BYTES + 1);

        connection
            .execute(
                "INSERT INTO agent_database (singleton, source_id) VALUES (1, ?1)",
                [&at_limit],
            )
            .expect("64-byte source ID");
        assert!(
            connection
                .execute(
                    "UPDATE agent_database SET source_id = ?1 WHERE singleton = 1",
                    [&over_limit],
                )
                .is_err(),
            "65-byte source ID must violate the schema"
        );

        connection
            .execute(
                "INSERT INTO session_sequence (source_session_id, next_sequence) VALUES (?1, 1)",
                [&at_limit],
            )
            .expect("64-byte source session ID");
        assert!(
            connection
                .execute(
                    "UPDATE session_sequence SET source_session_id = ?1",
                    [&over_limit],
                )
                .is_err(),
            "65-byte source session ID must violate the schema"
        );

        connection
            .execute(
                "INSERT INTO source_replica_state (singleton, epoch_id) VALUES (1, ?1)",
                [&at_limit],
            )
            .expect("64-byte epoch ID");
        assert!(
            connection
                .execute(
                    "UPDATE source_replica_state SET epoch_id = ?1 WHERE singleton = 1",
                    [&over_limit],
                )
                .is_err(),
            "65-byte epoch ID must violate the schema"
        );
        assert!(
            connection
                .execute(
                    "UPDATE source_replica_state
                     SET checkpoint_replacement_known = 1,
                         checkpoint_supersedes_id = ?1
                     WHERE singleton = 1",
                    [&over_limit],
                )
                .is_err(),
            "65-byte superseded checkpoint ID must violate the schema"
        );
        assert!(
            connection
                .execute(
                    "UPDATE source_replica_state
                     SET checkpoint_replacement_known = 1,
                         checkpoint_replaces_epoch_id = ?1,
                         checkpoint_replaces_revision = 1
                     WHERE singleton = 1",
                    [&over_limit],
                )
                .is_err(),
            "65-byte replacement epoch ID must violate the schema"
        );

        assert!(
            connection
                .execute(
                    "INSERT INTO source_replica_frozen_action (
                         singleton, action_kind, checkpoint_id, target_revision,
                         state_observed_at_ms, expected_entries,
                         checkpoint_chunk_entries, content_sha256
                     ) VALUES (1, 'checkpoint', ?1, 1, 0, 0, 1, 'digest')",
                    [&over_limit],
                )
                .is_err(),
            "65-byte frozen checkpoint ID must violate the schema"
        );
        connection
            .execute(
                "INSERT INTO source_replica_frozen_action (
                     singleton, action_kind, checkpoint_id,
                     supersedes_checkpoint_id, replaces_epoch_id,
                     replaces_revision, target_revision, state_observed_at_ms,
                     expected_entries, checkpoint_chunk_entries, content_sha256
                 ) VALUES (1, 'checkpoint', ?1, ?1, ?1, 1, 1, 0, 0, 1, 'digest')",
                params![at_limit],
            )
            .expect("64-byte frozen identifiers");
    }
}
