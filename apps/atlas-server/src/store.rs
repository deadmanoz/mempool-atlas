use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use atlas_model::{
    Evidence, IngestStatus, MembershipRow, MempoolSnapshot, NormalizedEvent, SourceId,
};
use rusqlite::{Connection, OpenFlags, Transaction, params};
use thiserror::Error;

const LATEST_SCHEMA_VERSION: i64 = 1;
const MIGRATION_1: &str = include_str!("../migrations/0001_initial.sql");

#[derive(Clone, Debug)]
pub struct Store {
    path: Arc<PathBuf>,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("event model error: {0}")]
    Model(#[from] atlas_model::ModelError),
    #[error("event serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error(
        "database schema is at version {found}, expected {expected}; run the backup-first migration command"
    )]
    SchemaVersion { found: i64, expected: i64 },
    #[error("numeric field {field} is too large for SQLite")]
    NumericOverflow { field: &'static str },
    #[error("invalid raw transaction hex for event {event_id}")]
    InvalidRawTransaction { event_id: String },
    #[error("event {event_id} conflicts with an already ingested event")]
    ConflictingEvent { event_id: String },
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        let connection = open_existing_connection(&path)?;
        let version = schema_version(&connection)?;
        if version != LATEST_SCHEMA_VERSION {
            return Err(StoreError::SchemaVersion {
                found: version,
                expected: LATEST_SCHEMA_VERSION,
            });
        }
        Ok(Self {
            path: Arc::new(path),
        })
    }

    pub fn migrate(path: impl AsRef<Path>) -> Result<(), StoreError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut connection = Connection::open(path)?;
        configure_connection(&connection)?;
        let version = schema_version(&connection)?;
        if version > LATEST_SCHEMA_VERSION {
            return Err(StoreError::SchemaVersion {
                found: version,
                expected: LATEST_SCHEMA_VERSION,
            });
        }
        if version < 1 {
            let transaction = connection.transaction()?;
            transaction.execute_batch(MIGRATION_1)?;
            transaction.pragma_update(None, "user_version", 1)?;
            transaction.commit()?;
        }
        Ok(())
    }

    pub fn ingest(&self, event: &NormalizedEvent) -> Result<IngestStatus, StoreError> {
        event.validate()?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        upsert_source(&transaction, event)?;

        let payload_json = serde_json::to_string(&event.evidence)?;
        let local_sequence = to_sqlite_integer(event.local_sequence, "local_sequence")?;
        let observed_at_ms = to_sqlite_integer(event.observed_at_ms, "observed_at_ms")?;
        let received_at_ms = to_sqlite_integer(event.received_at_ms, "received_at_ms")?;
        let inserted = transaction.execute(
            "INSERT INTO event (
                event_id, source_id, source_session_id, local_sequence,
                observed_at_ms, received_at_ms, event_kind, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (event_id) DO NOTHING",
            params![
                event.event_id,
                event.source_id.as_str(),
                event.source_session_id.as_str(),
                local_sequence,
                observed_at_ms,
                received_at_ms,
                event.evidence.kind(),
                payload_json,
            ],
        )?;

        if inserted == 0 {
            let existing = transaction.query_row(
                "SELECT observed_at_ms, received_at_ms, event_kind, payload_json
                 FROM event WHERE event_id = ?1",
                [&event.event_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )?;
            if existing
                != (
                    observed_at_ms,
                    received_at_ms,
                    event.evidence.kind().to_owned(),
                    payload_json,
                )
            {
                return Err(StoreError::ConflictingEvent {
                    event_id: event.event_id.clone(),
                });
            }
            transaction.commit()?;
            return Ok(IngestStatus::Duplicate);
        }

        apply_evidence(&transaction, event)?;
        transaction.commit()?;
        Ok(IngestStatus::Applied)
    }

    pub fn mempool(&self, source_id: Option<&SourceId>) -> Result<MempoolSnapshot, StoreError> {
        let connection = self.connect()?;
        let mut memberships = Vec::new();
        if let Some(source_id) = source_id {
            let mut statement = connection.prepare(
                "SELECT source_id, txid, present, updated_at_ms, evidence_event_id
                 FROM current_membership
                 WHERE source_id = ?1 AND present = 1
                 ORDER BY txid",
            )?;
            let rows = statement.query_map([source_id.as_str()], membership_from_row)?;
            for row in rows {
                memberships.push(row?);
            }
        } else {
            let mut statement = connection.prepare(
                "SELECT source_id, txid, present, updated_at_ms, evidence_event_id
                 FROM current_membership
                 WHERE present = 1
                 ORDER BY source_id, txid",
            )?;
            let rows = statement.query_map([], membership_from_row)?;
            for row in rows {
                memberships.push(row?);
            }
        }
        Ok(MempoolSnapshot {
            source_id: source_id.cloned(),
            memberships,
        })
    }

    fn connect(&self) -> Result<Connection, StoreError> {
        open_existing_connection(&self.path)
    }
}

fn open_existing_connection(path: &Path) -> Result<Connection, StoreError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    configure_connection(&connection)?;
    Ok(connection)
}

fn configure_connection(connection: &Connection) -> Result<(), rusqlite::Error> {
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    Ok(())
}

fn schema_version(connection: &Connection) -> Result<i64, rusqlite::Error> {
    connection.pragma_query_value(None, "user_version", |row| row.get(0))
}

fn upsert_source(transaction: &Transaction<'_>, event: &NormalizedEvent) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO source (source_id, latest_session_id, last_seen_at_ms)
         VALUES (?1, ?2, ?3)
         ON CONFLICT (source_id) DO UPDATE SET
             latest_session_id = excluded.latest_session_id,
             last_seen_at_ms = MAX(source.last_seen_at_ms, excluded.last_seen_at_ms)",
        params![
            event.source_id.as_str(),
            event.source_session_id.as_str(),
            to_sqlite_integer(event.received_at_ms, "received_at_ms")?,
        ],
    )?;
    Ok(())
}

fn apply_evidence(
    transaction: &Transaction<'_>,
    event: &NormalizedEvent,
) -> Result<(), StoreError> {
    for mutation in event.membership_mutations() {
        upsert_membership(transaction, event, &mutation.txid, mutation.present)?;
    }

    if let Evidence::P2pTransaction {
        txid,
        wtxid,
        raw_transaction_hex,
        ..
    } = &event.evidence
    {
        let raw_transaction = raw_transaction_hex
            .as_deref()
            .map(hex::decode)
            .transpose()
            .map_err(|_| StoreError::InvalidRawTransaction {
                event_id: event.event_id.clone(),
            })?;
        transaction.execute(
            "INSERT INTO transaction_variant (wtxid, txid, raw_transaction, first_event_id)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (wtxid) DO UPDATE SET
                 raw_transaction = COALESCE(transaction_variant.raw_transaction, excluded.raw_transaction)",
            params![wtxid, txid, raw_transaction, event.event_id],
        )?;
    }
    Ok(())
}

fn upsert_membership(
    transaction: &Transaction<'_>,
    event: &NormalizedEvent,
    txid: &str,
    present: bool,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO current_membership (
            source_id, txid, present, updated_at_ms, evidence_event_id
         ) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (source_id, txid) DO UPDATE SET
             present = excluded.present,
             updated_at_ms = excluded.updated_at_ms,
             evidence_event_id = excluded.evidence_event_id",
        params![
            event.source_id.as_str(),
            txid,
            present,
            to_sqlite_integer(event.observed_at_ms, "observed_at_ms")?,
            event.event_id,
        ],
    )?;
    Ok(())
}

fn membership_from_row(row: &rusqlite::Row<'_>) -> Result<MembershipRow, rusqlite::Error> {
    let source_id: String = row.get(0)?;
    let updated_at_ms: i64 = row.get(3)?;
    Ok(MembershipRow {
        source_id: SourceId::new(source_id).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        txid: row.get(1)?,
        present: row.get(2)?,
        updated_at_ms: u64::try_from(updated_at_ms).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Integer,
                Box::new(error),
            )
        })?,
        evidence_event_id: row.get(4)?,
    })
}

fn to_sqlite_integer(value: u64, field: &'static str) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::NumericOverflow { field })
}

#[cfg(test)]
mod tests {
    use atlas_model::{Evidence, NormalizedEvent, SourceSessionId};
    use tempfile::TempDir;

    use super::*;

    const TXID: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    fn test_store() -> (TempDir, Store) {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("atlas.db");
        Store::migrate(&path).expect("migrate");
        let store = Store::open(path).expect("open");
        (temporary, store)
    }

    fn added_event() -> NormalizedEvent {
        NormalizedEvent::new(
            SourceId::new("source-a").expect("source"),
            SourceSessionId::new("session-a").expect("session"),
            1,
            100,
            101,
            Evidence::MempoolAdded {
                txid: TXID.to_owned(),
            },
        )
        .expect("event")
    }

    #[test]
    fn duplicate_delivery_does_not_reapply_event() {
        let (_temporary, store) = test_store();
        let event = added_event();
        assert_eq!(store.ingest(&event).expect("first"), IngestStatus::Applied);
        assert_eq!(
            store.ingest(&event).expect("duplicate"),
            IngestStatus::Duplicate
        );
        let snapshot = store.mempool(None).expect("mempool");
        assert_eq!(snapshot.memberships.len(), 1);
        assert_eq!(snapshot.memberships[0].txid, TXID);
    }

    #[test]
    fn reused_event_identity_with_different_payload_is_rejected() {
        let (_temporary, store) = test_store();
        let event = added_event();
        store.ingest(&event).expect("first");
        let mut conflicting = event;
        conflicting.received_at_ms += 1;
        assert!(matches!(
            store.ingest(&conflicting),
            Err(StoreError::ConflictingEvent { .. })
        ));
    }

    #[test]
    fn rejection_is_evidence_without_membership() {
        let (_temporary, store) = test_store();
        let event = NormalizedEvent::new(
            SourceId::new("source-a").expect("source"),
            SourceSessionId::new("session-a").expect("session"),
            2,
            100,
            101,
            Evidence::MempoolRejected {
                txid: TXID.to_owned(),
                reason: "policy".to_owned(),
            },
        )
        .expect("event");
        store.ingest(&event).expect("ingest");
        assert!(store.mempool(None).expect("mempool").memberships.is_empty());
    }

    #[test]
    fn opening_unmigrated_database_fails() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("atlas.db");
        Connection::open(&path).expect("create database");
        assert!(matches!(
            Store::open(path),
            Err(StoreError::SchemaVersion {
                found: 0,
                expected: 1
            })
        ));
    }
}
