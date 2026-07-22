//! Durable node-local delivery queue and effective mempool projection.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use atlas_model::{
    Evidence, MAX_INGEST_BATCH_BODY_BYTES, MAX_INGEST_BATCH_EVENTS, MembershipMutation,
    MempoolEntryFacts, NormalizedEvent, ReconciledMembership, SourceId, SourceSessionId,
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use thiserror::Error;

use crate::schema::{self, AgentSchemaError, LATEST_SCHEMA_VERSION};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentIdentity {
    pub source_id: SourceId,
    pub source_session_id: SourceSessionId,
}

impl AgentIdentity {
    #[must_use]
    pub const fn new(source_id: SourceId, source_session_id: SourceSessionId) -> Self {
        Self {
            source_id,
            source_session_id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingEvent {
    pub outbox_id: i64,
    pub event: NormalizedEvent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PendingDelivery {
    Single(PendingEvent),
    ReconciliationBatch(Vec<PendingEvent>),
}

#[derive(Clone, Debug)]
pub struct Outbox {
    path: Arc<PathBuf>,
    source_id: SourceId,
}

#[derive(Debug, Error)]
pub enum OutboxError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("event model error: {0}")]
    Model(#[from] atlas_model::ModelError),
    #[error("event serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("outbox schema is at version {found}, expected {expected}")]
    SchemaVersion { found: i64, expected: i64 },
    #[error("outbox is bound to source {bound}, not configured source {configured}")]
    SourceBinding { bound: String, configured: String },
    #[error("agent identity source {provided} does not match bound source {bound}")]
    IdentitySource { bound: String, provided: String },
    #[error("pending event {requested} is not the outbox head (expected {expected:?})")]
    NotHead {
        requested: i64,
        expected: Option<i64>,
    },
    #[error("delivered outbox prefix must not be empty")]
    EmptyDeliveryPrefix,
    #[error("pending events {requested:?} are not the exact outbox prefix {expected:?}")]
    NotPrefix {
        requested: Vec<i64>,
        expected: Vec<i64>,
    },
    #[error("numeric field {field} is too large for SQLite")]
    NumericOverflow { field: &'static str },
    #[error("database contains a negative value for {field}")]
    NegativeStoredInteger { field: &'static str },
    #[error("stored event source {stored} does not match bound source {bound}")]
    StoredSource { bound: String, stored: String },
    #[error("projected membership {txid} contains incomplete mempool entry facts")]
    IncompleteProjectedFacts { txid: String },
}

impl Outbox {
    /// Opens an already migrated outbox and binds it to one logical node source.
    pub fn open(path: impl AsRef<Path>, source_id: SourceId) -> Result<Self, OutboxError> {
        let path = path.as_ref().to_path_buf();
        let mut connection = open_existing_connection(&path)?;
        require_latest_schema(&connection)?;

        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing_source = transaction
            .query_row(
                "SELECT source_id FROM agent_database WHERE singleton = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match existing_source {
            Some(bound) if bound != source_id.as_str() => {
                return Err(OutboxError::SourceBinding {
                    bound,
                    configured: source_id.to_string(),
                });
            }
            Some(_) => {}
            None => {
                transaction.execute(
                    "INSERT INTO agent_database (singleton, source_id) VALUES (1, ?1)",
                    [source_id.as_str()],
                )?;
            }
        }
        transaction.commit()?;

        Ok(Self {
            path: Arc::new(path),
            source_id,
        })
    }

    /// Applies schema migrations. Persistent deployments must call this through
    /// the project's backup-first migration wrapper.
    pub fn migrate(path: impl AsRef<Path>) -> Result<(), OutboxError> {
        schema::migrate(path).map_err(|error| match error {
            AgentSchemaError::Database(error) => OutboxError::Database(error),
            AgentSchemaError::Io(error) => OutboxError::Io(error),
            AgentSchemaError::SchemaVersion { found, expected } => {
                OutboxError::SchemaVersion { found, expected }
            }
        })
    }

    #[must_use]
    pub const fn source_id(&self) -> &SourceId {
        &self.source_id
    }

    /// Persists evidence before its caller acknowledges the upstream message.
    /// Membership changes are reflected in the effective local projection in
    /// the same transaction, even while delivery remains pending.
    pub fn enqueue_observation(
        &self,
        identity: &AgentIdentity,
        observed_at_ms: u64,
        received_at_ms: u64,
        evidence: Evidence,
        nats_subject: Option<&str>,
        raw_payload: Option<&[u8]>,
    ) -> Result<PendingEvent, OutboxError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.require_identity(identity)?;
        let pending = enqueue_in_transaction(
            &transaction,
            identity,
            observed_at_ms,
            received_at_ms,
            evidence,
            nats_subject,
            raw_payload,
        )?;
        apply_projection(&transaction, &pending.event)?;
        transaction.commit()?;
        Ok(pending)
    }

    /// Reconciles an RPC snapshot against the effective local projection.
    /// Corrections are ordered as sorted removals followed by sorted additions.
    pub fn reconcile_rpc_snapshot(
        &self,
        identity: &AgentIdentity,
        snapshot: BTreeMap<String, MempoolEntryFacts>,
        completed_at_ms: u64,
    ) -> Result<usize, OutboxError> {
        for (txid, facts) in &snapshot {
            Evidence::MempoolReconciled {
                txid: txid.clone(),
                membership: ReconciledMembership::Present {
                    facts: facts.clone(),
                },
            }
            .validate()?;
        }

        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        self.require_identity(identity)?;
        let projected = projected_membership_in(&transaction)?;
        let mut correction_count = 0_usize;

        for txid in projected
            .keys()
            .filter(|txid| !snapshot.contains_key(*txid))
        {
            let pending = enqueue_in_transaction(
                &transaction,
                identity,
                completed_at_ms,
                completed_at_ms,
                Evidence::MempoolReconciled {
                    txid: txid.clone(),
                    membership: ReconciledMembership::Absent,
                },
                None,
                None,
            )?;
            apply_projection(&transaction, &pending.event)?;
            correction_count += 1;
        }

        for (txid, facts) in &snapshot {
            if projected.get(txid).and_then(Option::as_ref) == Some(facts) {
                continue;
            }
            let pending = enqueue_in_transaction(
                &transaction,
                identity,
                completed_at_ms,
                completed_at_ms,
                Evidence::MempoolReconciled {
                    txid: txid.clone(),
                    membership: ReconciledMembership::Present {
                        facts: facts.clone(),
                    },
                },
                None,
                None,
            )?;
            apply_projection(&transaction, &pending.event)?;
            correction_count += 1;
        }

        transaction.execute(
            "UPDATE outbox_state SET last_rpc_success_at_ms = ?1 WHERE singleton = 1",
            [to_sqlite_integer(completed_at_ms, "completed_at_ms")?],
        )?;
        transaction.commit()?;
        Ok(correction_count)
    }

    /// Returns either the FIFO head or a bounded contiguous prefix of RPC
    /// reconciliation events. A non-reconciliation event always terminates a
    /// batch so later live evidence can never overtake it.
    pub fn next_delivery(&self) -> Result<Option<PendingDelivery>, OutboxError> {
        self.next_delivery_with_limits(MAX_INGEST_BATCH_EVENTS, MAX_INGEST_BATCH_BODY_BYTES)
    }

    pub fn next_pending(&self) -> Result<Option<PendingEvent>, OutboxError> {
        let connection = self.connect()?;
        let stored = connection
            .query_row(
                "SELECT outbox_id, event_json FROM outbox_event ORDER BY outbox_id LIMIT 1",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((outbox_id, event_json)) = stored else {
            return Ok(None);
        };
        let event: NormalizedEvent = serde_json::from_str(&event_json)?;
        event.validate()?;
        if event.source_id != self.source_id {
            return Err(OutboxError::StoredSource {
                bound: self.source_id.to_string(),
                stored: event.source_id.to_string(),
            });
        }
        Ok(Some(PendingEvent { outbox_id, event }))
    }

    fn next_delivery_with_limits(
        &self,
        max_events: usize,
        max_body_bytes: usize,
    ) -> Result<Option<PendingDelivery>, OutboxError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT outbox_id, event_json
             FROM outbox_event
             ORDER BY outbox_id
             LIMIT ?1",
        )?;
        let limit = to_sqlite_integer(
            u64::try_from(max_events).map_err(|_| OutboxError::NumericOverflow {
                field: "delivery_batch_events",
            })?,
            "delivery_batch_events",
        )?;
        let mut rows = statement.query([limit])?;
        let mut batch = Vec::new();
        let mut encoded_body_bytes = br#"{"events":[]}"#.len();

        while let Some(row) = rows.next()? {
            let outbox_id = row.get::<_, i64>(0)?;
            let event_json = row.get::<_, String>(1)?;
            let event = decode_stored_event(&self.source_id, &event_json)?;
            let pending = PendingEvent { outbox_id, event };

            if !matches!(pending.event.evidence, Evidence::MempoolReconciled { .. }) {
                if batch.is_empty() {
                    return Ok(Some(PendingDelivery::Single(pending)));
                }
                break;
            }

            let separator_bytes = usize::from(!batch.is_empty());
            let Some(candidate_body_bytes) = encoded_body_bytes
                .checked_add(separator_bytes)
                .and_then(|size| size.checked_add(event_json.len()))
            else {
                if batch.is_empty() {
                    return Ok(Some(PendingDelivery::Single(pending)));
                }
                break;
            };
            if candidate_body_bytes > max_body_bytes {
                if batch.is_empty() {
                    return Ok(Some(PendingDelivery::Single(pending)));
                }
                break;
            }

            encoded_body_bytes = candidate_body_bytes;
            batch.push(pending);
        }

        if batch.is_empty() {
            Ok(None)
        } else {
            Ok(Some(PendingDelivery::ReconciliationBatch(batch)))
        }
    }

    /// Deletes an acknowledged row only when it is the current FIFO head.
    pub fn mark_delivered(&self, outbox_id: i64) -> Result<(), OutboxError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_head(&transaction, outbox_id)?;
        transaction.execute("DELETE FROM outbox_event WHERE outbox_id = ?1", [outbox_id])?;
        transaction.commit()?;
        Ok(())
    }

    /// Atomically verifies and deletes an exact FIFO prefix.
    pub fn mark_delivered_prefix(&self, outbox_ids: &[i64]) -> Result<(), OutboxError> {
        if outbox_ids.is_empty() {
            return Err(OutboxError::EmptyDeliveryPrefix);
        }

        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let expected = pending_prefix(&transaction, outbox_ids.len())?;
        if expected != outbox_ids {
            return Err(OutboxError::NotPrefix {
                requested: outbox_ids.to_vec(),
                expected,
            });
        }

        let last = *outbox_ids.last().expect("non-empty prefix checked above");
        let deleted =
            transaction.execute("DELETE FROM outbox_event WHERE outbox_id <= ?1", [last])?;
        if deleted != outbox_ids.len() {
            return Err(OutboxError::NotPrefix {
                requested: outbox_ids.to_vec(),
                expected,
            });
        }
        transaction.commit()?;
        Ok(())
    }

    /// Records a failed attempt without advancing the FIFO head.
    pub fn mark_failed(&self, outbox_id: i64, error: &str) -> Result<u64, OutboxError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_head(&transaction, outbox_id)?;
        let attempts = transaction.query_row(
            "UPDATE outbox_event
             SET delivery_attempts = delivery_attempts + 1, last_delivery_error = ?2
             WHERE outbox_id = ?1
             RETURNING delivery_attempts",
            params![outbox_id, error],
            |row| row.get::<_, i64>(0),
        )?;
        transaction.commit()?;
        from_sqlite_integer(attempts, "delivery_attempts")
    }

    pub fn pending_count(&self) -> Result<u64, OutboxError> {
        let connection = self.connect()?;
        let count = connection.query_row("SELECT COUNT(*) FROM outbox_event", [], |row| {
            row.get::<_, i64>(0)
        })?;
        from_sqlite_integer(count, "pending_count")
    }

    pub fn projected_membership(
        &self,
    ) -> Result<BTreeMap<String, Option<MempoolEntryFacts>>, OutboxError> {
        let connection = self.connect()?;
        projected_membership_in(&connection)
    }

    pub fn last_rpc_success_at_ms(&self) -> Result<Option<u64>, OutboxError> {
        let connection = self.connect()?;
        let value = connection.query_row(
            "SELECT last_rpc_success_at_ms FROM outbox_state WHERE singleton = 1",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        value
            .map(|value| from_sqlite_integer(value, "last_rpc_success_at_ms"))
            .transpose()
    }

    fn connect(&self) -> Result<Connection, OutboxError> {
        open_existing_connection(&self.path)
    }

    fn require_identity(&self, identity: &AgentIdentity) -> Result<(), OutboxError> {
        if identity.source_id == self.source_id {
            return Ok(());
        }
        Err(OutboxError::IdentitySource {
            bound: self.source_id.to_string(),
            provided: identity.source_id.to_string(),
        })
    }
}

fn open_existing_connection(path: &Path) -> Result<Connection, OutboxError> {
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
    connection.pragma_update(None, "temp_store", "MEMORY")?;
    Ok(())
}

fn schema_version(connection: &Connection) -> Result<i64, rusqlite::Error> {
    connection.pragma_query_value(None, "user_version", |row| row.get(0))
}

fn require_latest_schema(connection: &Connection) -> Result<(), OutboxError> {
    let version = schema_version(connection)?;
    if version == LATEST_SCHEMA_VERSION {
        return Ok(());
    }
    Err(OutboxError::SchemaVersion {
        found: version,
        expected: LATEST_SCHEMA_VERSION,
    })
}

fn enqueue_in_transaction(
    transaction: &Transaction<'_>,
    identity: &AgentIdentity,
    observed_at_ms: u64,
    received_at_ms: u64,
    evidence: Evidence,
    nats_subject: Option<&str>,
    raw_payload: Option<&[u8]>,
) -> Result<PendingEvent, OutboxError> {
    let local_sequence = allocate_sequence(transaction, &identity.source_session_id)?;
    let event = NormalizedEvent::new(
        identity.source_id.clone(),
        identity.source_session_id.clone(),
        local_sequence,
        observed_at_ms,
        received_at_ms,
        evidence,
    )?;
    let event_json = serde_json::to_string(&event)?;
    transaction.execute(
        "INSERT INTO outbox_event (
            source_session_id, local_sequence, event_json, nats_subject, raw_payload
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            identity.source_session_id.as_str(),
            to_sqlite_integer(local_sequence, "local_sequence")?,
            event_json,
            nats_subject,
            raw_payload,
        ],
    )?;
    Ok(PendingEvent {
        outbox_id: transaction.last_insert_rowid(),
        event,
    })
}

fn allocate_sequence(
    transaction: &Transaction<'_>,
    source_session_id: &SourceSessionId,
) -> Result<u64, OutboxError> {
    let sequence = transaction.query_row(
        "INSERT INTO session_sequence (source_session_id, next_sequence)
         VALUES (?1, 2)
         ON CONFLICT (source_session_id) DO UPDATE SET
             next_sequence = session_sequence.next_sequence + 1
         RETURNING next_sequence - 1",
        [source_session_id.as_str()],
        |row| row.get::<_, i64>(0),
    )?;
    from_sqlite_integer(sequence, "local_sequence")
}

fn apply_projection(
    transaction: &Transaction<'_>,
    event: &NormalizedEvent,
) -> Result<(), OutboxError> {
    for mutation in event.membership_mutations() {
        match mutation {
            MembershipMutation::Absent { txid } => {
                transaction.execute("DELETE FROM projected_membership WHERE txid = ?1", [&txid])?;
            }
            MembershipMutation::Present { txid, facts: None } => {
                transaction.execute(
                    "INSERT INTO projected_membership (txid) VALUES (?1)
                     ON CONFLICT (txid) DO NOTHING",
                    [&txid],
                )?;
            }
            MembershipMutation::Present {
                txid,
                facts: Some(facts),
            } => {
                transaction.execute(
                    "INSERT INTO projected_membership (
                        txid, vsize, fee_sats, entered_at_ms
                     ) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (txid) DO UPDATE SET
                        vsize = excluded.vsize,
                        fee_sats = excluded.fee_sats,
                        entered_at_ms = excluded.entered_at_ms",
                    params![
                        txid,
                        to_sqlite_integer(facts.vsize, "vsize")?,
                        to_sqlite_integer(facts.fee_sats, "fee_sats")?,
                        to_sqlite_integer(facts.entered_at_ms, "entered_at_ms")?,
                    ],
                )?;
            }
        }
    }
    Ok(())
}

fn projected_membership_in(
    connection: &Connection,
) -> Result<BTreeMap<String, Option<MempoolEntryFacts>>, OutboxError> {
    let mut statement = connection.prepare(
        "SELECT txid, vsize, fee_sats, entered_at_ms
         FROM projected_membership
         ORDER BY txid",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<i64>>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<i64>>(3)?,
        ))
    })?;
    let mut membership = BTreeMap::new();
    for row in rows {
        let (txid, vsize, fee_sats, entered_at_ms) = row?;
        let facts = match (vsize, fee_sats, entered_at_ms) {
            (None, None, None) => None,
            (Some(vsize), Some(fee_sats), Some(entered_at_ms)) => Some(MempoolEntryFacts {
                vsize: from_sqlite_integer(vsize, "vsize")?,
                fee_sats: from_sqlite_integer(fee_sats, "fee_sats")?,
                entered_at_ms: from_sqlite_integer(entered_at_ms, "entered_at_ms")?,
            }),
            _ => return Err(OutboxError::IncompleteProjectedFacts { txid }),
        };
        membership.insert(txid, facts);
    }
    Ok(membership)
}

fn decode_stored_event(
    source_id: &SourceId,
    event_json: &str,
) -> Result<NormalizedEvent, OutboxError> {
    let event: NormalizedEvent = serde_json::from_str(event_json)?;
    event.validate()?;
    if event.source_id != *source_id {
        return Err(OutboxError::StoredSource {
            bound: source_id.to_string(),
            stored: event.source_id.to_string(),
        });
    }
    Ok(event)
}

fn pending_prefix(connection: &Connection, count: usize) -> Result<Vec<i64>, OutboxError> {
    let limit = to_sqlite_integer(
        u64::try_from(count).map_err(|_| OutboxError::NumericOverflow {
            field: "delivery_prefix_events",
        })?,
        "delivery_prefix_events",
    )?;
    let mut statement = connection.prepare(
        "SELECT outbox_id
         FROM outbox_event
         ORDER BY outbox_id
         LIMIT ?1",
    )?;
    let rows = statement.query_map([limit], |row| row.get::<_, i64>(0))?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn require_head(transaction: &Transaction<'_>, requested: i64) -> Result<(), OutboxError> {
    let expected = transaction
        .query_row(
            "SELECT outbox_id FROM outbox_event ORDER BY outbox_id LIMIT 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if expected == Some(requested) {
        return Ok(());
    }
    Err(OutboxError::NotHead {
        requested,
        expected,
    })
}

fn to_sqlite_integer(value: u64, field: &'static str) -> Result<i64, OutboxError> {
    i64::try_from(value).map_err(|_| OutboxError::NumericOverflow { field })
}

fn from_sqlite_integer(value: i64, field: &'static str) -> Result<u64, OutboxError> {
    u64::try_from(value).map_err(|_| OutboxError::NegativeStoredInteger { field })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;

    use atlas_model::{CaptureGapCertainty, IngestBatchRequest};
    use tempfile::TempDir;

    use super::*;

    const TXID_A: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const TXID_B: &str = "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f";
    const TXID_C: &str = "404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f";

    fn source(value: &str) -> SourceId {
        SourceId::new(value).expect("source")
    }

    fn identity(session: &str) -> AgentIdentity {
        AgentIdentity::new(
            source("core-a"),
            SourceSessionId::new(session).expect("session"),
        )
    }

    fn test_outbox() -> (TempDir, PathBuf, Outbox) {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("outbox.db");
        Outbox::migrate(&path).expect("migrate temporary outbox");
        let outbox = Outbox::open(&path, source("core-a")).expect("open outbox");
        (temporary, path, outbox)
    }

    #[test]
    fn opened_connections_use_memory_temp_store() {
        let (_temporary, _path, outbox) = test_outbox();
        let connection = outbox.connect().expect("connect");
        let temp_store = connection
            .pragma_query_value(None, "temp_store", |row| row.get::<_, i64>(0))
            .expect("read temp store");

        assert_eq!(temp_store, 2);
    }

    fn added(txid: &str) -> Evidence {
        Evidence::MempoolAdded {
            txid: txid.to_owned(),
        }
    }

    fn facts() -> MempoolEntryFacts {
        MempoolEntryFacts {
            vsize: 141,
            fee_sats: 1_200,
            entered_at_ms: 1_721_234_000_000,
        }
    }

    fn snapshot(txids: &[&str]) -> BTreeMap<String, MempoolEntryFacts> {
        txids
            .iter()
            .map(|txid| ((*txid).to_owned(), facts()))
            .collect()
    }

    fn reconciliation_batch(delivery: PendingDelivery) -> Vec<PendingEvent> {
        match delivery {
            PendingDelivery::ReconciliationBatch(pending) => pending,
            PendingDelivery::Single(pending) => {
                panic!(
                    "expected reconciliation batch, got {}",
                    pending.event.event_id
                )
            }
        }
    }

    fn batch_body_bytes(pending: &[PendingEvent]) -> usize {
        serde_json::to_vec(&IngestBatchRequest {
            events: pending
                .iter()
                .map(|pending| pending.event.clone())
                .collect(),
        })
        .expect("serialize batch request")
        .len()
    }

    #[test]
    fn pending_state_and_projection_survive_reopen() {
        let (_temporary, path, outbox) = test_outbox();
        let pending = outbox
            .enqueue_observation(
                &identity("session-a"),
                100,
                101,
                added(TXID_A),
                Some("mempool"),
                Some(b"protobuf"),
            )
            .expect("enqueue");
        drop(outbox);

        let reopened = Outbox::open(&path, source("core-a")).expect("reopen");
        assert_eq!(reopened.next_pending().expect("next"), Some(pending));
        assert_eq!(
            reopened.projected_membership().expect("projection"),
            BTreeMap::from([(TXID_A.to_owned(), None)])
        );
        assert!(matches!(
            Outbox::open(&path, source("knots-a")),
            Err(OutboxError::SourceBinding { .. })
        ));
    }

    #[test]
    fn rpc_enriches_a_pending_live_membership() {
        let (_temporary, _path, outbox) = test_outbox();
        let identity = identity("session-a");
        let live = outbox
            .enqueue_observation(&identity, 100, 101, added(TXID_A), None, None)
            .expect("live addition");
        assert_eq!(
            outbox.projected_membership().expect("pending projection"),
            BTreeMap::from([(TXID_A.to_owned(), None)])
        );

        assert_eq!(
            outbox
                .reconcile_rpc_snapshot(&identity, snapshot(&[TXID_A]), 110)
                .expect("enrichment"),
            1
        );
        assert_eq!(
            outbox.projected_membership().expect("enriched projection"),
            BTreeMap::from([(TXID_A.to_owned(), Some(facts()))])
        );
        outbox
            .mark_delivered(live.outbox_id)
            .expect("deliver live addition");
        let enrichment = reconciliation_batch(
            outbox
                .next_delivery()
                .expect("next")
                .expect("pending enrichment"),
        );
        assert_eq!(
            enrichment[0].event.evidence,
            Evidence::MempoolReconciled {
                txid: TXID_A.to_owned(),
                membership: ReconciledMembership::Present { facts: facts() },
            }
        );
    }

    #[test]
    fn rpc_emits_changed_facts_for_an_existing_membership() {
        let (_temporary, _path, outbox) = test_outbox();
        let identity = identity("session-a");
        assert_eq!(
            outbox
                .reconcile_rpc_snapshot(&identity, snapshot(&[TXID_A]), 100)
                .expect("initial snapshot"),
            1
        );
        let initial = reconciliation_batch(
            outbox
                .next_delivery()
                .expect("next")
                .expect("initial event"),
        );
        outbox
            .mark_delivered_prefix(&[initial[0].outbox_id])
            .expect("deliver initial event");

        let mut changed = facts();
        changed.fee_sats += 1;
        changed.entered_at_ms += 1_000;
        assert_eq!(
            outbox
                .reconcile_rpc_snapshot(
                    &identity,
                    BTreeMap::from([(TXID_A.to_owned(), changed.clone())]),
                    200,
                )
                .expect("changed snapshot"),
            1
        );
        let correction = reconciliation_batch(
            outbox
                .next_delivery()
                .expect("next")
                .expect("changed event"),
        );
        assert_eq!(
            correction[0].event.evidence,
            Evidence::MempoolReconciled {
                txid: TXID_A.to_owned(),
                membership: ReconciledMembership::Present {
                    facts: changed.clone(),
                },
            }
        );
        assert_eq!(
            outbox.projected_membership().expect("changed projection"),
            BTreeMap::from([(TXID_A.to_owned(), Some(changed))])
        );
    }

    #[test]
    fn capture_gap_survives_reopen_and_is_delivered_once_as_fifo_head() {
        let (_temporary, path, outbox) = test_outbox();
        let pending = outbox
            .enqueue_observation(
                &identity("session-a"),
                100,
                100,
                Evidence::CaptureGap {
                    input: "peer_observer_nats".to_owned(),
                    reason: "slow_consumer".to_owned(),
                    certainty: CaptureGapCertainty::KnownLoss,
                },
                None,
                None,
            )
            .expect("enqueue capture gap");
        assert!(
            outbox
                .projected_membership()
                .expect("projection")
                .is_empty()
        );
        drop(outbox);

        let reopened = Outbox::open(&path, source("core-a")).expect("reopen");
        assert_eq!(
            reopened.next_delivery().expect("next delivery"),
            Some(PendingDelivery::Single(pending.clone()))
        );
        reopened
            .mark_delivered(pending.outbox_id)
            .expect("deliver capture gap");
        assert_eq!(
            reopened.next_delivery().expect("empty after delivery"),
            None
        );
    }

    #[test]
    fn failure_keeps_head_and_out_of_order_delivery_is_rejected() {
        let (_temporary, path, outbox) = test_outbox();
        let first = outbox
            .enqueue_observation(&identity("session-a"), 100, 101, added(TXID_A), None, None)
            .expect("first");
        let second = outbox
            .enqueue_observation(&identity("session-b"), 102, 103, added(TXID_B), None, None)
            .expect("second");

        assert!(first.outbox_id < second.outbox_id);
        assert!(matches!(
            outbox.mark_delivered(second.outbox_id),
            Err(OutboxError::NotHead {
                expected: Some(expected),
                ..
            }) if expected == first.outbox_id
        ));
        assert_eq!(
            outbox
                .mark_failed(first.outbox_id, "server offline")
                .expect("fail"),
            1
        );
        let failure = Connection::open(&path)
            .expect("inspect outbox")
            .query_row(
                "SELECT delivery_attempts, last_delivery_error
                 FROM outbox_event WHERE outbox_id = ?1",
                [first.outbox_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .expect("failure state");
        assert_eq!(failure, (1, "server offline".to_owned()));
        assert_eq!(
            outbox
                .next_pending()
                .expect("next")
                .expect("head")
                .outbox_id,
            first.outbox_id
        );
        assert_eq!(outbox.pending_count().expect("count"), 2);

        outbox
            .mark_delivered(first.outbox_id)
            .expect("deliver first");
        assert_eq!(
            outbox
                .next_pending()
                .expect("next")
                .expect("head")
                .outbox_id,
            second.outbox_id
        );
        assert_eq!(
            outbox.projected_membership().expect("projection"),
            BTreeMap::from([(TXID_A.to_owned(), None), (TXID_B.to_owned(), None)])
        );
    }

    #[test]
    fn concurrent_enqueues_preserve_sequence_and_fifo_order() {
        let (_temporary, _path, outbox) = test_outbox();
        let outbox = Arc::new(outbox);
        let barrier = Arc::new(Barrier::new(8));
        let mut workers = Vec::new();
        for worker in 0..8 {
            let outbox = Arc::clone(&outbox);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                outbox
                    .enqueue_observation(
                        &identity("session-a"),
                        100 + worker,
                        200 + worker,
                        Evidence::MempoolRejected {
                            txid: TXID_A.to_owned(),
                            reason: worker.to_string(),
                        },
                        None,
                        None,
                    )
                    .expect("concurrent enqueue")
            }));
        }
        let mut pending = workers
            .into_iter()
            .map(|worker| worker.join().expect("worker"))
            .collect::<Vec<_>>();
        pending.sort_by_key(|event| event.outbox_id);
        assert_eq!(
            pending
                .iter()
                .map(|event| event.event.local_sequence)
                .collect::<Vec<_>>(),
            (1..=8).collect::<Vec<_>>()
        );
    }

    #[test]
    fn reconciliation_batch_honours_count_and_exact_body_limits() {
        let (_temporary, _path, outbox) = test_outbox();
        let snapshot = (0_u64..513)
            .map(|value| (format!("{value:064x}"), facts()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            outbox
                .reconcile_rpc_snapshot(&identity("session-a"), snapshot, 500)
                .expect("baseline"),
            513
        );

        let count_limited = reconciliation_batch(
            outbox
                .next_delivery()
                .expect("next")
                .expect("pending delivery"),
        );
        assert_eq!(count_limited.len(), MAX_INGEST_BATCH_EVENTS);
        assert!(batch_body_bytes(&count_limited) <= MAX_INGEST_BATCH_BODY_BYTES);

        let first_two = &count_limited[..2];
        let exact_two_event_bytes = batch_body_bytes(first_two);
        let byte_limited = reconciliation_batch(
            outbox
                .next_delivery_with_limits(MAX_INGEST_BATCH_EVENTS, exact_two_event_bytes)
                .expect("byte limited next")
                .expect("byte limited delivery"),
        );
        assert_eq!(byte_limited.len(), 2);
        assert_eq!(batch_body_bytes(&byte_limited), exact_two_event_bytes);

        let one_event_bytes = batch_body_bytes(&count_limited[..1]);
        assert!(matches!(
            outbox
                .next_delivery_with_limits(MAX_INGEST_BATCH_EVENTS, one_event_bytes - 1)
                .expect("oversize next")
                .expect("oversize delivery"),
            PendingDelivery::Single(_)
        ));
    }

    #[test]
    fn reconciliation_batch_stops_before_live_evidence_and_survives_reopen() {
        let (_temporary, path, outbox) = test_outbox();
        let identity = identity("session-a");
        assert_eq!(
            outbox
                .reconcile_rpc_snapshot(&identity, snapshot(&[TXID_A, TXID_B]), 500,)
                .expect("baseline"),
            2
        );
        let live = outbox
            .enqueue_observation(
                &identity,
                501,
                502,
                Evidence::MempoolRemoved {
                    txid: TXID_A.to_owned(),
                    reason: Some("replaced".to_owned()),
                },
                Some("mempool"),
                Some(b"live"),
            )
            .expect("live event");
        let expected = outbox
            .next_delivery()
            .expect("next")
            .expect("pending delivery");
        let batch = reconciliation_batch(expected.clone());
        assert_eq!(batch.len(), 2);
        assert!(
            batch.iter().all(|pending| matches!(
                &pending.event.evidence,
                Evidence::MempoolReconciled { .. }
            ))
        );
        assert!(batch.last().expect("last baseline").outbox_id < live.outbox_id);
        drop(outbox);

        let reopened = Outbox::open(&path, source("core-a")).expect("reopen");
        assert_eq!(
            reopened.next_delivery().expect("next after reopen"),
            Some(expected)
        );
        let batch_ids = batch
            .iter()
            .map(|pending| pending.outbox_id)
            .collect::<Vec<_>>();
        reopened
            .mark_delivered_prefix(&batch_ids)
            .expect("deliver baseline prefix");
        assert_eq!(
            reopened.next_delivery().expect("live next"),
            Some(PendingDelivery::Single(live))
        );
    }

    #[test]
    fn delivered_prefix_must_match_exact_current_fifo_prefix() {
        let (_temporary, _path, outbox) = test_outbox();
        assert_eq!(
            outbox
                .reconcile_rpc_snapshot(
                    &identity("session-a"),
                    snapshot(&[TXID_A, TXID_B, TXID_C]),
                    500,
                )
                .expect("baseline"),
            3
        );
        let batch = reconciliation_batch(
            outbox
                .next_delivery()
                .expect("next")
                .expect("pending delivery"),
        );
        let ids = batch
            .iter()
            .map(|pending| pending.outbox_id)
            .collect::<Vec<_>>();

        assert!(matches!(
            outbox.mark_delivered_prefix(&[ids[0], ids[2]]),
            Err(OutboxError::NotPrefix { .. })
        ));
        assert_eq!(outbox.pending_count().expect("count after rejection"), 3);
        assert_eq!(
            reconciliation_batch(
                outbox
                    .next_delivery()
                    .expect("next after rejection")
                    .expect("pending after rejection")
            )
            .iter()
            .map(|pending| pending.outbox_id)
            .collect::<Vec<_>>(),
            ids
        );

        outbox
            .mark_delivered_prefix(&ids[..2])
            .expect("deliver exact prefix");
        assert_eq!(outbox.pending_count().expect("remaining count"), 1);
        assert_eq!(
            outbox
                .next_pending()
                .expect("remaining")
                .expect("tail")
                .outbox_id,
            ids[2]
        );
    }

    #[test]
    fn rpc_reconciliation_updates_projection_and_baseline_durably() {
        let (_temporary, path, outbox) = test_outbox();
        let baseline_count = outbox
            .reconcile_rpc_snapshot(&identity("session-a"), snapshot(&[TXID_B, TXID_A]), 500)
            .expect("baseline");
        assert_eq!(baseline_count, 2);
        let baseline = reconciliation_batch(
            outbox
                .next_delivery()
                .expect("next baseline")
                .expect("pending baseline"),
        );
        assert_eq!(baseline[0].event.local_sequence, 1);
        assert_eq!(baseline[1].event.local_sequence, 2);
        assert_eq!(
            baseline
                .iter()
                .map(|pending| &pending.event.evidence)
                .collect::<Vec<_>>(),
            vec![
                &Evidence::MempoolReconciled {
                    txid: TXID_A.to_owned(),
                    membership: ReconciledMembership::Present { facts: facts() },
                },
                &Evidence::MempoolReconciled {
                    txid: TXID_B.to_owned(),
                    membership: ReconciledMembership::Present { facts: facts() },
                },
            ]
        );
        drop(outbox);

        let reopened = Outbox::open(&path, source("core-a")).expect("reopen");
        assert_eq!(
            reopened.projected_membership().expect("projection"),
            BTreeMap::from([
                (TXID_A.to_owned(), Some(facts())),
                (TXID_B.to_owned(), Some(facts())),
            ])
        );
        assert_eq!(
            reopened.last_rpc_success_at_ms().expect("rpc time"),
            Some(500)
        );
        let baseline_ids = baseline
            .iter()
            .map(|pending| pending.outbox_id)
            .collect::<Vec<_>>();
        reopened
            .mark_delivered_prefix(&baseline_ids)
            .expect("deliver baseline");

        let correction_count = reopened
            .reconcile_rpc_snapshot(&identity("session-a"), snapshot(&[TXID_B, TXID_C]), 600)
            .expect("second reconciliation");
        assert_eq!(correction_count, 2);
        let corrections = reconciliation_batch(
            reopened
                .next_delivery()
                .expect("next corrections")
                .expect("pending corrections"),
        );
        assert_eq!(corrections[0].event.local_sequence, 3);
        assert_eq!(corrections[1].event.local_sequence, 4);
        assert_eq!(
            corrections[0].event.evidence,
            Evidence::MempoolReconciled {
                txid: TXID_A.to_owned(),
                membership: ReconciledMembership::Absent,
            }
        );
        assert_eq!(
            corrections[1].event.evidence,
            Evidence::MempoolReconciled {
                txid: TXID_C.to_owned(),
                membership: ReconciledMembership::Present { facts: facts() },
            }
        );
        assert_eq!(
            reopened.projected_membership().expect("projection"),
            BTreeMap::from([
                (TXID_B.to_owned(), Some(facts())),
                (TXID_C.to_owned(), Some(facts())),
            ])
        );
        assert_eq!(
            reopened.last_rpc_success_at_ms().expect("rpc time"),
            Some(600)
        );

        assert_eq!(
            reopened
                .reconcile_rpc_snapshot(&identity("session-a"), snapshot(&[TXID_B, TXID_C]), 700,)
                .expect("unchanged reconciliation"),
            0
        );
        assert_eq!(
            reopened.last_rpc_success_at_ms().expect("rpc time"),
            Some(700)
        );
    }

    #[test]
    fn rpc_restores_transaction_after_peer_observer_removal() {
        let (_temporary, _path, outbox) = test_outbox();
        let identity = identity("session-a");
        assert_eq!(
            outbox
                .reconcile_rpc_snapshot(&identity, snapshot(&[TXID_A]), 100)
                .expect("initial snapshot"),
            1
        );
        let initial = reconciliation_batch(
            outbox
                .next_delivery()
                .expect("initial next")
                .expect("initial pending"),
        );
        outbox
            .mark_delivered_prefix(&[initial[0].outbox_id])
            .expect("deliver initial snapshot");
        let removal = outbox
            .enqueue_observation(
                &identity,
                110,
                111,
                Evidence::MempoolRemoved {
                    txid: TXID_A.to_owned(),
                    reason: Some("unknown".to_owned()),
                },
                Some("mempool"),
                Some(b"removed"),
            )
            .expect("peer-observer removal");
        outbox
            .mark_delivered(removal.outbox_id)
            .expect("deliver removal");
        assert!(
            outbox
                .projected_membership()
                .expect("projection")
                .is_empty()
        );

        let correction_count = outbox
            .reconcile_rpc_snapshot(&identity, snapshot(&[TXID_A]), 120)
            .expect("corrective snapshot");
        assert_eq!(correction_count, 1);
        let corrections = reconciliation_batch(
            outbox
                .next_delivery()
                .expect("corrective next")
                .expect("corrective pending"),
        );
        assert_eq!(
            corrections[0].event.evidence,
            Evidence::MempoolReconciled {
                txid: TXID_A.to_owned(),
                membership: ReconciledMembership::Present { facts: facts() },
            }
        );
        assert_eq!(
            outbox.projected_membership().expect("projection"),
            BTreeMap::from([(TXID_A.to_owned(), Some(facts()))])
        );
    }

    #[test]
    fn open_requires_current_schema_version() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("outbox.db");
        Connection::open(&path).expect("create database");
        assert!(matches!(
            Outbox::open(&path, source("core-a")),
            Err(OutboxError::SchemaVersion {
                found: 0,
                expected: 5
            })
        ));

        let connection = Connection::open(&path).expect("open database");
        connection
            .pragma_update(None, "user_version", 2)
            .expect("set stale version");
        drop(connection);
        assert!(matches!(
            Outbox::migrate(&path),
            Err(OutboxError::SchemaVersion {
                found: 2,
                expected: 5
            })
        ));

        let connection = Connection::open(&path).expect("open database");
        connection
            .pragma_update(None, "user_version", 6)
            .expect("set future version");
        drop(connection);
        assert!(matches!(
            Outbox::migrate(&path),
            Err(OutboxError::SchemaVersion {
                found: 6,
                expected: 5
            })
        ));
    }
}
