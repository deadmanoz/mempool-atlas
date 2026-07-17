use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use atlas_classifiers::{
    BaselineHeuristics, ClassificationInput, ClassificationStatus, Classifier, TransactionShape,
};
use atlas_model::{
    CaptureGapCertainty, CaptureStatus, Classification, Evidence, IngestBatchRequest, IngestStatus,
    MembershipMutation, MempoolEntry, MempoolEntryFacts, MempoolEntryFactsStatus, MempoolSnapshot,
    NormalizedEvent, ScriptType, SourceDescriptor, SourceHealth, SourceId,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use thiserror::Error;
use tracing::warn;

use crate::summary::{FactsRow, ShapeRow, SourceMembershipFacts};

const LATEST_SCHEMA_VERSION: i64 = 4;
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
    #[error("database schema is at version {found}, expected {expected}")]
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
        if version == LATEST_SCHEMA_VERSION {
            return Ok(());
        }
        if version != 0 {
            return Err(StoreError::SchemaVersion {
                found: version,
                expected: LATEST_SCHEMA_VERSION,
            });
        }
        let transaction = connection.transaction()?;
        transaction.execute_batch(MIGRATION_1)?;
        transaction.pragma_update(None, "user_version", LATEST_SCHEMA_VERSION)?;
        transaction.commit()?;
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

    pub fn mempool(&self, source_id: &SourceId) -> Result<Option<MempoolSnapshot>, StoreError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        let source = transaction
            .query_row(
                "SELECT
                    source.last_seen_at_ms,
                    source_capture_state.first_gap_at_ms,
                    source_capture_state.latest_gap_at_ms,
                    source_capture_state.marker_count,
                    source_capture_state.strongest_certainty,
                    source_capture_state.latest_input,
                    source_capture_state.latest_reason
                 FROM source
                 LEFT JOIN source_capture_state USING (source_id)
                 WHERE source.source_id = ?1",
                [source_id.as_str()],
                source_health_from_row,
            )
            .optional()?;
        let Some(health) = source else {
            transaction.commit()?;
            return Ok(None);
        };

        let memberships = {
            let mut statement = transaction.prepare(
                "SELECT txid, updated_at_ms, evidence_event_id,
                        vsize, fee_sats, entered_at_ms
                 FROM current_membership
                 WHERE source_id = ?1
                 ORDER BY txid",
            )?;
            let rows = statement.query_map([source_id.as_str()], mempool_entry_from_row)?;
            let mut memberships = Vec::new();
            for row in rows {
                memberships.push(row?);
            }
            memberships
        };
        transaction.commit()?;
        Ok(Some(MempoolSnapshot {
            source_id: source_id.clone(),
            health,
            memberships,
        }))
    }

    /// Reads only what the aggregate summary needs: source health, the fact
    /// triples of fact-bearing memberships, and the awaiting-RPC count.
    pub fn mempool_facts(
        &self,
        source_id: &SourceId,
    ) -> Result<Option<SourceMembershipFacts>, StoreError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        let health = transaction
            .query_row(
                "SELECT
                    source.last_seen_at_ms,
                    source_capture_state.first_gap_at_ms,
                    source_capture_state.latest_gap_at_ms,
                    source_capture_state.marker_count,
                    source_capture_state.strongest_certainty,
                    source_capture_state.latest_input,
                    source_capture_state.latest_reason
                 FROM source
                 LEFT JOIN source_capture_state USING (source_id)
                 WHERE source.source_id = ?1",
                [source_id.as_str()],
                source_health_from_row,
            )
            .optional()?;
        let Some(health) = health else {
            transaction.commit()?;
            return Ok(None);
        };

        let mut available = Vec::new();
        let mut awaiting_rpc_count = 0;
        {
            let mut statement = transaction.prepare(
                "SELECT current_membership.vsize, current_membership.fee_sats,
                        current_membership.entered_at_ms,
                        transaction_shape.total_output_sats, transaction_shape.input_count,
                        transaction_shape.output_count, transaction_shape.script_type,
                        transaction_shape.classification
                 FROM current_membership
                 LEFT JOIN transaction_shape USING (txid)
                 WHERE current_membership.source_id = ?1",
            )?;
            let mut rows = statement.query([source_id.as_str()])?;
            while let Some(row) = rows.next()? {
                match facts_row_from_row(row)? {
                    Some(facts) => available.push(facts),
                    None => awaiting_rpc_count += 1,
                }
            }
        }
        transaction.commit()?;
        Ok(Some(SourceMembershipFacts {
            health,
            available,
            awaiting_rpc_count,
        }))
    }

    /// Lists every known source with its current membership count, ordered by
    /// source ID for stable discovery responses.
    pub fn sources(&self) -> Result<Vec<SourceDescriptor>, StoreError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT source.source_id, source.last_seen_at_ms, COUNT(current_membership.txid)
             FROM source
             LEFT JOIN current_membership USING (source_id)
             GROUP BY source.source_id, source.last_seen_at_ms
             ORDER BY source.source_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                nonnegative_integer_from_row(row.get(1)?, 1)?,
                nonnegative_integer_from_row(row.get(2)?, 2)?,
            ))
        })?;
        let mut sources = Vec::new();
        for row in rows {
            let (source_id, last_seen_at_ms, membership_count) = row?;
            sources.push(SourceDescriptor {
                source_id: SourceId::new(source_id)?,
                last_seen_at_ms,
                membership_count,
            });
        }
        Ok(sources)
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
        apply_membership_mutation(transaction, event, mutation)?;
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
        if let Some(raw_transaction) = raw_transaction {
            derive_transaction_shape(transaction, event, txid, wtxid, &raw_transaction)?;
        }
    }

    if let Evidence::CaptureGap {
        input,
        reason,
        certainty,
    } = &event.evidence
    {
        let gap_at_ms = to_sqlite_integer(event.observed_at_ms, "observed_at_ms")?;
        transaction.execute(
            "INSERT INTO source_capture_state (
                source_id, first_gap_at_ms, latest_gap_at_ms, marker_count,
                strongest_certainty, latest_input, latest_reason, latest_evidence_event_id
             ) VALUES (?1, ?2, ?2, 1, ?3, ?4, ?5, ?6)
             ON CONFLICT (source_id) DO UPDATE SET
                first_gap_at_ms = MIN(source_capture_state.first_gap_at_ms, excluded.first_gap_at_ms),
                latest_gap_at_ms = MAX(source_capture_state.latest_gap_at_ms, excluded.latest_gap_at_ms),
                marker_count = source_capture_state.marker_count + 1,
                strongest_certainty = CASE
                    WHEN source_capture_state.strongest_certainty = 'known_loss'
                        OR excluded.strongest_certainty = 'known_loss'
                    THEN 'known_loss'
                    ELSE 'possible_loss'
                END,
                latest_input = CASE
                    WHEN excluded.latest_gap_at_ms >= source_capture_state.latest_gap_at_ms
                    THEN excluded.latest_input
                    ELSE source_capture_state.latest_input
                END,
                latest_reason = CASE
                    WHEN excluded.latest_gap_at_ms >= source_capture_state.latest_gap_at_ms
                    THEN excluded.latest_reason
                    ELSE source_capture_state.latest_reason
                END,
                latest_evidence_event_id = CASE
                    WHEN excluded.latest_gap_at_ms >= source_capture_state.latest_gap_at_ms
                    THEN excluded.latest_evidence_event_id
                    ELSE source_capture_state.latest_evidence_event_id
                END",
            params![
                event.source_id.as_str(),
                gap_at_ms,
                capture_gap_certainty_as_str(*certainty),
                input,
                reason,
                event.event_id,
            ],
        )?;
    }
    Ok(())
}

/// Derives intrinsic shape facts and a classifier verdict from observed raw
/// transaction bytes, once per txid. Bytes that fail to decode or that do not
/// match the claimed identifiers leave the transaction underived rather than
/// guessing; the evidence row itself is always retained.
fn derive_transaction_shape(
    db: &Transaction<'_>,
    event: &NormalizedEvent,
    txid: &str,
    wtxid: &str,
    raw_transaction: &[u8],
) -> Result<(), StoreError> {
    let already_derived = db.query_row(
        "SELECT EXISTS (SELECT 1 FROM transaction_shape WHERE txid = ?1)",
        [txid],
        |row| row.get::<_, bool>(0),
    )?;
    if already_derived {
        return Ok(());
    }
    let Ok(parsed) = bitcoin::consensus::deserialize::<bitcoin::Transaction>(raw_transaction)
    else {
        warn!(
            event_id = %event.event_id,
            %txid,
            "raw transaction bytes do not decode; shape not derived"
        );
        return Ok(());
    };
    if parsed.compute_txid().to_string() != txid || parsed.compute_wtxid().to_string() != wtxid {
        warn!(
            event_id = %event.event_id,
            %txid,
            "raw transaction bytes do not match their claimed identifiers; shape not derived"
        );
        return Ok(());
    }
    let shape = TransactionShape::derive(&parsed);
    let result = BaselineHeuristics.classify(&ClassificationInput {
        txid,
        wtxid: Some(wtxid),
        transaction: Some(&parsed),
    });
    let classification = result.verdict.unwrap_or(Classification::Unknown);
    db.execute(
        "INSERT INTO transaction_shape (
            txid, total_output_sats, input_count, output_count, script_type,
            classification, classification_status, classifier_id,
            classifier_version, classifier_evidence_json, derived_from_event_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            txid,
            to_sqlite_integer(shape.total_output_sats, "total_output_sats")?,
            to_sqlite_integer(shape.input_count, "input_count")?,
            to_sqlite_integer(shape.output_count, "output_count")?,
            shape.script_type.key(),
            classification.key(),
            classification_status_as_str(result.status),
            result.classifier_id,
            result.classifier_version,
            serde_json::to_string(&result.evidence)?,
            event.event_id,
        ],
    )?;
    Ok(())
}

const fn classification_status_as_str(status: ClassificationStatus) -> &'static str {
    match status {
        ClassificationStatus::Complete => "complete",
        ClassificationStatus::Partial => "partial",
        ClassificationStatus::Unknown => "unknown",
        ClassificationStatus::NotApplicable => "not_applicable",
        ClassificationStatus::Error => "error",
    }
}

fn apply_membership_mutation(
    transaction: &Transaction<'_>,
    event: &NormalizedEvent,
    mutation: MembershipMutation,
) -> Result<(), StoreError> {
    match mutation {
        MembershipMutation::Absent { txid } => {
            transaction.execute(
                "DELETE FROM current_membership
                 WHERE source_id = ?1 AND txid = ?2",
                params![event.source_id.as_str(), txid],
            )?;
        }
        MembershipMutation::Present { txid, facts: None } => {
            transaction.execute(
                "INSERT INTO current_membership (
                    source_id, txid, updated_at_ms, evidence_event_id
                 ) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (source_id, txid) DO UPDATE SET
                    updated_at_ms = excluded.updated_at_ms,
                    evidence_event_id = excluded.evidence_event_id",
                params![
                    event.source_id.as_str(),
                    txid,
                    to_sqlite_integer(event.observed_at_ms, "observed_at_ms")?,
                    event.event_id,
                ],
            )?;
        }
        MembershipMutation::Present {
            txid,
            facts: Some(facts),
        } => {
            transaction.execute(
                "INSERT INTO current_membership (
                    source_id, txid, updated_at_ms, evidence_event_id,
                    vsize, fee_sats, entered_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (source_id, txid) DO UPDATE SET
                    updated_at_ms = excluded.updated_at_ms,
                    evidence_event_id = excluded.evidence_event_id,
                    vsize = excluded.vsize,
                    fee_sats = excluded.fee_sats,
                    entered_at_ms = excluded.entered_at_ms",
                params![
                    event.source_id.as_str(),
                    txid,
                    to_sqlite_integer(event.observed_at_ms, "observed_at_ms")?,
                    event.event_id,
                    to_sqlite_integer(facts.vsize, "vsize")?,
                    to_sqlite_integer(facts.fee_sats, "fee_sats")?,
                    to_sqlite_integer(facts.entered_at_ms, "entered_at_ms")?,
                ],
            )?;
        }
    }
    Ok(())
}

fn mempool_entry_from_row(row: &rusqlite::Row<'_>) -> Result<MempoolEntry, rusqlite::Error> {
    let vsize = row.get::<_, Option<i64>>(3)?;
    let fee_sats = row.get::<_, Option<i64>>(4)?;
    let entered_at_ms = row.get::<_, Option<i64>>(5)?;
    let facts = match (vsize, fee_sats, entered_at_ms) {
        (None, None, None) => MempoolEntryFactsStatus::AwaitingRpc,
        (Some(vsize), Some(fee_sats), Some(entered_at_ms)) => MempoolEntryFactsStatus::Available {
            facts: MempoolEntryFacts {
                vsize: nonnegative_integer_from_row(vsize, 3)?,
                fee_sats: nonnegative_integer_from_row(fee_sats, 4)?,
                entered_at_ms: nonnegative_integer_from_row(entered_at_ms, 5)?,
            },
        },
        _ => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Null,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "mempool entry contains incomplete facts",
                )),
            ));
        }
    };
    Ok(MempoolEntry {
        txid: row.get(0)?,
        updated_at_ms: nonnegative_integer_from_row(row.get(1)?, 1)?,
        evidence_event_id: row.get(2)?,
        facts,
    })
}

fn facts_row_from_row(row: &rusqlite::Row<'_>) -> Result<Option<FactsRow>, rusqlite::Error> {
    let vsize = row.get::<_, Option<i64>>(0)?;
    let fee_sats = row.get::<_, Option<i64>>(1)?;
    let entered_at_ms = row.get::<_, Option<i64>>(2)?;
    match (vsize, fee_sats, entered_at_ms) {
        (None, None, None) => Ok(None),
        (Some(vsize), Some(fee_sats), Some(entered_at_ms)) => Ok(Some(FactsRow {
            vsize: nonnegative_integer_from_row(vsize, 0)?,
            fee_sats: nonnegative_integer_from_row(fee_sats, 1)?,
            entered_at_ms: nonnegative_integer_from_row(entered_at_ms, 2)?,
            shape: shape_row_from_row(row)?,
        })),
        _ => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Null,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "mempool entry contains incomplete facts",
            )),
        )),
    }
}

fn shape_row_from_row(row: &rusqlite::Row<'_>) -> Result<Option<ShapeRow>, rusqlite::Error> {
    let Some(total_output_sats) = row.get::<_, Option<i64>>(3)? else {
        return Ok(None);
    };
    let script_type = row.get::<_, String>(6)?;
    let classification = row.get::<_, String>(7)?;
    Ok(Some(ShapeRow {
        total_output_sats: nonnegative_integer_from_row(total_output_sats, 3)?,
        input_count: nonnegative_integer_from_row(row.get(4)?, 4)?,
        output_count: nonnegative_integer_from_row(row.get(5)?, 5)?,
        script_type: ScriptType::from_key(&script_type).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unsupported script type {script_type}"),
                )),
            )
        })?,
        classification: Classification::from_key(&classification).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                7,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unsupported classification {classification}"),
                )),
            )
        })?,
    }))
}

fn source_health_from_row(row: &rusqlite::Row<'_>) -> Result<SourceHealth, rusqlite::Error> {
    let last_seen_at_ms = nonnegative_integer_from_row(row.get(0)?, 0)?;
    let first_gap_at_ms = row.get::<_, Option<i64>>(1)?;
    let capture = if let Some(first_gap_at_ms) = first_gap_at_ms {
        let certainty = row.get::<_, String>(4)?;
        CaptureStatus::ContainsGaps {
            first_gap_at_ms: nonnegative_integer_from_row(first_gap_at_ms, 1)?,
            latest_gap_at_ms: nonnegative_integer_from_row(row.get(2)?, 2)?,
            marker_count: nonnegative_integer_from_row(row.get(3)?, 3)?,
            strongest_certainty: capture_gap_certainty_from_str(&certainty, 4)?,
            latest_input: row.get(5)?,
            latest_reason: row.get(6)?,
        }
    } else {
        CaptureStatus::NoReportedGaps
    };
    Ok(SourceHealth {
        last_seen_at_ms,
        capture,
    })
}

const fn capture_gap_certainty_as_str(certainty: CaptureGapCertainty) -> &'static str {
    match certainty {
        CaptureGapCertainty::PossibleLoss => "possible_loss",
        CaptureGapCertainty::KnownLoss => "known_loss",
    }
}

fn capture_gap_certainty_from_str(
    certainty: &str,
    column: usize,
) -> Result<CaptureGapCertainty, rusqlite::Error> {
    match certainty {
        "possible_loss" => Ok(CaptureGapCertainty::PossibleLoss),
        "known_loss" => Ok(CaptureGapCertainty::KnownLoss),
        _ => Err(rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unsupported capture gap certainty {certainty}"),
            )),
        )),
    }
}

fn nonnegative_integer_from_row(value: i64, column: usize) -> Result<u64, rusqlite::Error> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn to_sqlite_integer(value: u64, field: &'static str) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::NumericOverflow { field })
}

#[cfg(test)]
mod tests {
    use atlas_model::{
        CaptureGapCertainty, CaptureStatus, Evidence, NormalizedEvent, ReconciledMembership,
        SourceSessionId,
    };
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

    fn source() -> SourceId {
        SourceId::new("source-a").expect("source")
    }

    fn facts() -> MempoolEntryFacts {
        MempoolEntryFacts {
            vsize: 141,
            fee_sats: 1_200,
            entered_at_ms: 1_721_234_000_000,
        }
    }

    fn reconciled_present(txid: &str) -> Evidence {
        Evidence::MempoolReconciled {
            txid: txid.to_owned(),
            membership: ReconciledMembership::Present { facts: facts() },
        }
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
        event_for_source_at(
            "source-a",
            source_session_id,
            local_sequence,
            100,
            101,
            evidence,
        )
    }

    fn event_for_source_at(
        source_id: &str,
        source_session_id: &str,
        local_sequence: u64,
        observed_at_ms: u64,
        received_at_ms: u64,
        evidence: Evidence,
    ) -> NormalizedEvent {
        NormalizedEvent::new(
            SourceId::new(source_id).expect("source"),
            SourceSessionId::new(source_session_id).expect("session"),
            local_sequence,
            observed_at_ms,
            received_at_ms,
            evidence,
        )
        .expect("event")
    }

    fn capture_status(store: &Store, source_id: &SourceId) -> CaptureStatus {
        store
            .mempool(source_id)
            .expect("mempool")
            .expect("known source")
            .health
            .capture
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
        let snapshot = store
            .mempool(&source())
            .expect("mempool")
            .expect("known source");
        assert_eq!(snapshot.memberships.len(), 1);
        assert_eq!(snapshot.memberships[0].txid, TXID);
        assert_eq!(
            snapshot.memberships[0].facts,
            MempoolEntryFactsStatus::AwaitingRpc
        );
    }

    #[test]
    fn reconciliation_exposes_and_updates_available_facts() {
        let (_temporary, store) = test_store();
        store
            .ingest(&event_for("session-a", 1, reconciled_present(TXID)))
            .expect("initial reconciliation");
        let entry = store
            .mempool(&source())
            .expect("mempool")
            .expect("known source")
            .memberships
            .pop()
            .expect("membership");
        assert_eq!(
            entry.facts,
            MempoolEntryFactsStatus::Available { facts: facts() }
        );

        let mut changed = facts();
        changed.fee_sats += 1;
        changed.entered_at_ms += 1_000;
        store
            .ingest(&event_for(
                "session-a",
                2,
                Evidence::MempoolReconciled {
                    txid: TXID.to_owned(),
                    membership: ReconciledMembership::Present {
                        facts: changed.clone(),
                    },
                },
            ))
            .expect("fact update");
        let entry = store
            .mempool(&source())
            .expect("mempool")
            .expect("known source")
            .memberships
            .pop()
            .expect("membership");
        assert_eq!(
            entry.facts,
            MempoolEntryFactsStatus::Available { facts: changed }
        );
    }

    #[test]
    fn live_presence_preserves_known_facts_until_removal_and_readd() {
        let (_temporary, store) = test_store();
        store
            .ingest(&event_for("session-a", 1, reconciled_present(TXID)))
            .expect("initial reconciliation");
        store
            .ingest(&event_for(
                "session-a",
                2,
                Evidence::MempoolAdded {
                    txid: TXID.to_owned(),
                },
            ))
            .expect("duplicate live presence");
        assert_eq!(
            store
                .mempool(&source())
                .expect("mempool")
                .expect("known source")
                .memberships[0]
                .facts,
            MempoolEntryFactsStatus::Available { facts: facts() }
        );

        store
            .ingest(&event_for(
                "session-a",
                3,
                Evidence::MempoolRemoved {
                    txid: TXID.to_owned(),
                    reason: Some("removed".to_owned()),
                },
            ))
            .expect("removal");
        store
            .ingest(&event_for(
                "session-a",
                4,
                Evidence::MempoolAdded {
                    txid: TXID.to_owned(),
                },
            ))
            .expect("readd");
        assert_eq!(
            store
                .mempool(&source())
                .expect("mempool")
                .expect("known source")
                .memberships[0]
                .facts,
            MempoolEntryFactsStatus::AwaitingRpc
        );
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
        let present = event_for("session-a", 1, reconciled_present(TXID));
        let absent = event_for(
            "session-a",
            2,
            Evidence::MempoolReconciled {
                txid: TXID.to_owned(),
                membership: ReconciledMembership::Absent,
            },
        );

        store
            .ingest_batch(&IngestBatchRequest {
                events: vec![present, absent],
            })
            .expect("ordered batch");

        assert!(
            store
                .mempool(&source())
                .expect("mempool")
                .expect("known source")
                .memberships
                .is_empty()
        );
        let membership_count = Connection::open(temporary.path().join("atlas.db"))
            .expect("inspect database")
            .query_row(
                "SELECT COUNT(*) FROM current_membership
                 WHERE source_id = 'source-a' AND txid = ?1",
                [TXID],
                |row| row.get::<_, i64>(0),
            )
            .expect("membership count");
        assert_eq!(membership_count, 0);
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
        assert!(
            store
                .mempool(&source())
                .expect("mempool")
                .expect("known source")
                .memberships
                .is_empty()
        );
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
            source(),
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
        assert!(
            store
                .mempool(&source())
                .expect("mempool")
                .expect("known source")
                .memberships
                .is_empty()
        );
    }

    #[test]
    fn known_source_without_gap_markers_reports_no_reported_gaps() {
        let (_temporary, store) = test_store();
        store.ingest(&added_event()).expect("ingest");

        let snapshot = store
            .mempool(&source())
            .expect("mempool")
            .expect("known source");
        assert_eq!(snapshot.source_id, source());
        assert_eq!(snapshot.health.last_seen_at_ms, 101);
        assert_eq!(snapshot.health.capture, CaptureStatus::NoReportedGaps);
        assert_eq!(snapshot.memberships.len(), 1);
        assert_eq!(
            store
                .mempool(&SourceId::new("unknown").expect("source"))
                .expect("mempool"),
            None
        );
    }

    #[test]
    fn capture_gap_projection_counts_markers_and_keeps_strongest_certainty() {
        let (_temporary, store) = test_store();
        let possible = event_for_source_at(
            "source-a",
            "session-a",
            1,
            200,
            201,
            Evidence::CaptureGap {
                input: "peer_observer_nats".to_owned(),
                reason: "disconnected".to_owned(),
                certainty: CaptureGapCertainty::PossibleLoss,
            },
        );
        let known = event_for_source_at(
            "source-a",
            "session-a",
            2,
            300,
            301,
            Evidence::CaptureGap {
                input: "peer_observer_nats".to_owned(),
                reason: "slow_consumer".to_owned(),
                certainty: CaptureGapCertainty::KnownLoss,
            },
        );

        store.ingest(&possible).expect("possible gap");
        store.ingest(&known).expect("known gap");

        assert_eq!(
            capture_status(&store, &source()),
            CaptureStatus::ContainsGaps {
                first_gap_at_ms: 200,
                latest_gap_at_ms: 300,
                marker_count: 2,
                strongest_certainty: CaptureGapCertainty::KnownLoss,
                latest_input: "peer_observer_nats".to_owned(),
                latest_reason: "slow_consumer".to_owned(),
            }
        );
    }

    #[test]
    fn duplicate_capture_gap_does_not_increment_marker_count() {
        let (_temporary, store) = test_store();
        let gap = event_for_source_at(
            "source-a",
            "session-a",
            1,
            200,
            201,
            Evidence::CaptureGap {
                input: "peer_observer_nats".to_owned(),
                reason: "disconnected".to_owned(),
                certainty: CaptureGapCertainty::PossibleLoss,
            },
        );

        assert_eq!(store.ingest(&gap).expect("first"), IngestStatus::Applied);
        assert_eq!(
            store.ingest(&gap).expect("duplicate"),
            IngestStatus::Duplicate
        );
        assert!(matches!(
            capture_status(&store, &source()),
            CaptureStatus::ContainsGaps {
                marker_count: 1,
                ..
            }
        ));
    }

    #[test]
    fn membership_reconciliation_does_not_change_capture_gap_state() {
        let (_temporary, store) = test_store();
        let gap = event_for_source_at(
            "source-a",
            "session-a",
            1,
            200,
            201,
            Evidence::CaptureGap {
                input: "peer_observer_nats".to_owned(),
                reason: "disconnected".to_owned(),
                certainty: CaptureGapCertainty::PossibleLoss,
            },
        );
        store.ingest(&gap).expect("gap");
        let before = capture_status(&store, &source());
        store
            .ingest(&event_for_source_at(
                "source-a",
                "session-a",
                2,
                400,
                401,
                Evidence::MempoolReconciled {
                    txid: TXID.to_owned(),
                    membership: ReconciledMembership::Present { facts: facts() },
                },
            ))
            .expect("reconcile");

        assert_eq!(capture_status(&store, &source()), before);
    }

    #[test]
    fn gap_only_source_is_visible_empty_and_capture_state_is_source_local() {
        let (_temporary, store) = test_store();
        store
            .ingest(&event_for_source_at(
                "source-a",
                "session-a",
                1,
                200,
                201,
                Evidence::CaptureGap {
                    input: "peer_observer_nats".to_owned(),
                    reason: "slow_consumer".to_owned(),
                    certainty: CaptureGapCertainty::KnownLoss,
                },
            ))
            .expect("gap");
        store
            .ingest(&event_for_source_at(
                "source-b",
                "session-b",
                1,
                300,
                301,
                Evidence::MempoolAdded {
                    txid: TXID.to_owned(),
                },
            ))
            .expect("membership");

        let source_a = store
            .mempool(&source())
            .expect("mempool")
            .expect("source a");
        assert!(source_a.memberships.is_empty());
        assert!(matches!(
            source_a.health.capture,
            CaptureStatus::ContainsGaps { .. }
        ));
        let source_b_id = SourceId::new("source-b").expect("source");
        let source_b = store
            .mempool(&source_b_id)
            .expect("mempool")
            .expect("source b");
        assert_eq!(source_b.memberships.len(), 1);
        assert_eq!(source_b.health.capture, CaptureStatus::NoReportedGaps);
    }

    #[test]
    fn latest_capture_details_follow_latest_marker_timestamp() {
        let (_temporary, store) = test_store();
        for event in [
            event_for_source_at(
                "source-a",
                "session-a",
                1,
                300,
                301,
                Evidence::CaptureGap {
                    input: "newer-input".to_owned(),
                    reason: "newer-reason".to_owned(),
                    certainty: CaptureGapCertainty::PossibleLoss,
                },
            ),
            event_for_source_at(
                "source-a",
                "session-a",
                2,
                100,
                302,
                Evidence::CaptureGap {
                    input: "older-input".to_owned(),
                    reason: "older-reason".to_owned(),
                    certainty: CaptureGapCertainty::KnownLoss,
                },
            ),
        ] {
            store.ingest(&event).expect("gap");
        }

        assert_eq!(
            capture_status(&store, &source()),
            CaptureStatus::ContainsGaps {
                first_gap_at_ms: 100,
                latest_gap_at_ms: 300,
                marker_count: 2,
                strongest_certainty: CaptureGapCertainty::KnownLoss,
                latest_input: "newer-input".to_owned(),
                latest_reason: "newer-reason".to_owned(),
            }
        );
    }

    fn raw_transaction() -> bitcoin::Transaction {
        use bitcoin::absolute::LockTime;
        use bitcoin::hashes::Hash;
        use bitcoin::transaction::Version;
        use bitcoin::{Amount, OutPoint, ScriptBuf, Sequence, TxIn, TxOut, WPubkeyHash, Witness};

        bitcoin::Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: bitcoin::Txid::from_byte_array([0x44; 32]),
                    vout: 0,
                },
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::from_slice(&[vec![0xab; 107]]),
            }],
            output: vec![
                TxOut {
                    value: Amount::from_sat(4_000_000),
                    script_pubkey: ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([0x55; 20])),
                },
                TxOut {
                    value: Amount::from_sat(900_000),
                    script_pubkey: ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([0x66; 20])),
                },
            ],
        }
    }

    #[test]
    fn p2p_raw_bytes_derive_shape_facts_visible_in_membership_facts() {
        let (_temporary, store) = test_store();
        let transaction = raw_transaction();
        let txid = transaction.compute_txid().to_string();
        store
            .ingest(&event_for(
                "session-a",
                1,
                Evidence::P2pTransaction {
                    txid: txid.clone(),
                    wtxid: transaction.compute_wtxid().to_string(),
                    raw_transaction_hex: Some(hex::encode(bitcoin::consensus::encode::serialize(
                        &transaction,
                    ))),
                    peer_id: Some(3),
                    inbound: Some(true),
                },
            ))
            .expect("p2p evidence");
        store
            .ingest(&event_for(
                "session-a",
                2,
                Evidence::MempoolReconciled {
                    txid,
                    membership: ReconciledMembership::Present { facts: facts() },
                },
            ))
            .expect("reconciled facts");

        let membership = store
            .mempool_facts(&source())
            .expect("facts")
            .expect("known source");
        assert_eq!(membership.available.len(), 1);
        let shape = membership.available[0].shape.expect("derived shape");
        assert_eq!(shape.total_output_sats, 4_900_000);
        assert_eq!(shape.input_count, 1);
        assert_eq!(shape.output_count, 2);
        assert_eq!(shape.script_type, atlas_model::ScriptType::P2wpkh);
        assert_eq!(shape.classification, atlas_model::Classification::Payment);
    }

    #[test]
    fn p2p_evidence_without_raw_bytes_leaves_membership_underived() {
        let (_temporary, store) = test_store();
        store
            .ingest(&event_for(
                "session-a",
                1,
                Evidence::P2pTransaction {
                    txid: TXID.to_owned(),
                    wtxid: TXID_B.to_owned(),
                    raw_transaction_hex: None,
                    peer_id: Some(3),
                    inbound: Some(true),
                },
            ))
            .expect("p2p evidence");
        store
            .ingest(&event_for(
                "session-a",
                2,
                Evidence::MempoolReconciled {
                    txid: TXID.to_owned(),
                    membership: ReconciledMembership::Present { facts: facts() },
                },
            ))
            .expect("reconciled facts");

        let membership = store
            .mempool_facts(&source())
            .expect("facts")
            .expect("known source");
        assert_eq!(membership.available.len(), 1);
        assert!(membership.available[0].shape.is_none());
    }

    #[test]
    fn mismatched_raw_bytes_are_kept_as_evidence_but_never_derived() {
        let (_temporary, store) = test_store();
        let transaction = raw_transaction();
        // Claimed identifiers do not match the bytes.
        store
            .ingest(&event_for(
                "session-a",
                1,
                Evidence::P2pTransaction {
                    txid: TXID.to_owned(),
                    wtxid: TXID_B.to_owned(),
                    raw_transaction_hex: Some(hex::encode(bitcoin::consensus::encode::serialize(
                        &transaction,
                    ))),
                    peer_id: None,
                    inbound: Some(true),
                },
            ))
            .expect("p2p evidence");
        store
            .ingest(&event_for(
                "session-a",
                2,
                Evidence::MempoolReconciled {
                    txid: TXID.to_owned(),
                    membership: ReconciledMembership::Present { facts: facts() },
                },
            ))
            .expect("reconciled facts");

        let membership = store
            .mempool_facts(&source())
            .expect("facts")
            .expect("known source");
        assert!(membership.available[0].shape.is_none());
    }

    #[test]
    fn migration_rejects_stale_schema_version() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("atlas.db");
        let connection = Connection::open(&path).expect("create database");
        connection
            .pragma_update(None, "user_version", 3)
            .expect("schema version");
        drop(connection);

        assert!(matches!(
            Store::migrate(path),
            Err(StoreError::SchemaVersion {
                found: 3,
                expected: 4
            })
        ));
    }

    #[test]
    fn migration_rejects_future_schema_version() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("atlas.db");
        let connection = Connection::open(&path).expect("create database");
        connection
            .pragma_update(None, "user_version", 5)
            .expect("schema version");
        drop(connection);

        assert!(matches!(
            Store::migrate(path),
            Err(StoreError::SchemaVersion {
                found: 5,
                expected: 4
            })
        ));
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
                expected: 4
            })
        ));
    }
}
