use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use atlas_model::{
    Evidence, IngestBatchRequest, IngestStatus, MembershipRow, MempoolSnapshot, NormalizedEvent,
    SourceId,
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
        let mut statuses = self.ingest_validated(std::slice::from_ref(event))?;
        Ok(statuses
            .pop()
            .expect("single-event ingest must return exactly one status"))
    }

    pub fn ingest_batch(
        &self,
        request: &IngestBatchRequest,
    ) -> Result<Vec<IngestStatus>, StoreError> {
        request.validate()?;
        self.ingest_validated(&request.events)
    }

    fn ingest_validated(
        &self,
        events: &[NormalizedEvent],
    ) -> Result<Vec<IngestStatus>, StoreError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        let mut statuses = Vec::with_capacity(events.len());
        for event in events {
            statuses.push(ingest_in_transaction(&transaction, event)?);
        }
        transaction.commit()?;
        Ok(statuses)
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

fn ingest_in_transaction(
    transaction: &Transaction<'_>,
    event: &NormalizedEvent,
) -> Result<IngestStatus, StoreError> {
    upsert_source(transaction, event)?;

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
        return Ok(IngestStatus::Duplicate);
    }

    apply_evidence(transaction, event)?;
    Ok(IngestStatus::Applied)
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
    const TXID_B: &str = "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f";

    fn test_store() -> (TempDir, Store) {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("atlas.db");
        Store::migrate(&path).expect("migrate");
        let store = Store::open(path).expect("open");
        (temporary, store)
    }

    fn added_event() -> NormalizedEvent {
        event_for(
            "session-a",
            1,
            Evidence::MempoolAdded {
                txid: TXID.to_owned(),
            },
        )
    }

    fn event_for(
        source_session_id: &str,
        local_sequence: u64,
        evidence: Evidence,
    ) -> NormalizedEvent {
        NormalizedEvent::new(
            SourceId::new("source-a").expect("source"),
            SourceSessionId::new(source_session_id).expect("session"),
            local_sequence,
            100,
            101,
            evidence,
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
    fn batch_ingest_returns_ordered_applied_then_duplicate_statuses() {
        let (_temporary, store) = test_store();
        let request = IngestBatchRequest {
            events: vec![
                event_for(
                    "session-a",
                    1,
                    Evidence::MempoolAdded {
                        txid: TXID.to_owned(),
                    },
                ),
                event_for(
                    "session-b",
                    1,
                    Evidence::MempoolAdded {
                        txid: TXID_B.to_owned(),
                    },
                ),
            ],
        };

        assert_eq!(
            store.ingest_batch(&request).expect("first batch"),
            vec![IngestStatus::Applied, IngestStatus::Applied]
        );
        assert_eq!(
            store.ingest_batch(&request).expect("duplicate batch"),
            vec![IngestStatus::Duplicate, IngestStatus::Duplicate]
        );
    }

    #[test]
    fn batch_ingest_applies_membership_mutations_in_request_order() {
        let (temporary, store) = test_store();
        let present = event_for(
            "session-a",
            1,
            Evidence::MempoolReconciled {
                txid: TXID.to_owned(),
                present: true,
            },
        );
        let absent = event_for(
            "session-a",
            2,
            Evidence::MempoolReconciled {
                txid: TXID.to_owned(),
                present: false,
            },
        );
        let absent_event_id = absent.event_id.clone();

        store
            .ingest_batch(&IngestBatchRequest {
                events: vec![present, absent],
            })
            .expect("ordered batch");

        assert!(store.mempool(None).expect("mempool").memberships.is_empty());
        let membership = Connection::open(temporary.path().join("atlas.db"))
            .expect("inspect database")
            .query_row(
                "SELECT present, evidence_event_id FROM current_membership
                 WHERE source_id = 'source-a' AND txid = ?1",
                [TXID],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .expect("membership");
        assert_eq!(membership, (0, absent_event_id));
    }

    #[test]
    fn conflicting_late_batch_event_rolls_back_earlier_events() {
        let (temporary, store) = test_store();
        let existing = event_for(
            "existing-session",
            1,
            Evidence::MempoolRejected {
                txid: TXID.to_owned(),
                reason: "policy".to_owned(),
            },
        );
        store.ingest(&existing).expect("existing event");
        let mut conflicting = existing;
        conflicting.received_at_ms += 1;
        let request = IngestBatchRequest {
            events: vec![
                event_for(
                    "batch-session",
                    1,
                    Evidence::MempoolAdded {
                        txid: TXID.to_owned(),
                    },
                ),
                event_for(
                    "batch-session",
                    2,
                    Evidence::MempoolAdded {
                        txid: TXID_B.to_owned(),
                    },
                ),
                conflicting,
            ],
        };

        assert!(matches!(
            store.ingest_batch(&request),
            Err(StoreError::ConflictingEvent { .. })
        ));
        assert!(store.mempool(None).expect("mempool").memberships.is_empty());
        let event_count = Connection::open(temporary.path().join("atlas.db"))
            .expect("inspect database")
            .query_row("SELECT COUNT(*) FROM event", [], |row| row.get::<_, i64>(0))
            .expect("event count");
        assert_eq!(event_count, 1);
    }

    #[test]
    fn invalid_late_batch_event_prevents_any_application() {
        let (temporary, store) = test_store();
        let valid = added_event();
        let mut invalid = event_for(
            "session-a",
            2,
            Evidence::MempoolAdded {
                txid: TXID_B.to_owned(),
            },
        );
        invalid.event_id = "wrong/session/identity".to_owned();

        assert!(matches!(
            store.ingest_batch(&IngestBatchRequest {
                events: vec![valid, invalid],
            }),
            Err(StoreError::Model(atlas_model::ModelError::EventIdMismatch))
        ));
        let event_count = Connection::open(temporary.path().join("atlas.db"))
            .expect("inspect database")
            .query_row("SELECT COUNT(*) FROM event", [], |row| row.get::<_, i64>(0))
            .expect("event count");
        assert_eq!(event_count, 0);
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
