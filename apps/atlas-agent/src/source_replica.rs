//! Bounded, RPC-authoritative source-state persistence.
//!
//! `SourceReplica` deliberately does not know about HTTP or the peer-observer
//! evidence stream. It persists one current RPC snapshot, coalesces the net
//! changes by txid, and freezes at most one replayable delta or checkpoint for
//! a delivery layer to send. This keeps outage growth proportional to current
//! mempool state rather than observation rate or outage duration.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use atlas_model::{
    CheckpointBegin, CheckpointChunk, CheckpointCommit, CheckpointDigest, CheckpointId,
    MAX_CHECKPOINT_CHUNK_ENTRIES, MAX_CHECKPOINT_CHUNKS, MAX_CHECKPOINT_ENTRIES,
    MAX_SAFE_JSON_INTEGER, MAX_STATE_DELTA_MUTATIONS, MempoolEntryFacts, ReplicaCursor,
    SourceEpochId, SourceId, SourceReplicaEntry, StateDelta, StateHeartbeat, StateMutation,
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use thiserror::Error;
use uuid::Uuid;

use crate::schema::LATEST_SCHEMA_VERSION;

const DEFAULT_MAX_DIRTY_BYTES: u64 = 1024 * 1024;
const DEFAULT_MAX_DATABASE_BYTES: u64 = 1024 * 1024 * 1024;
const DEFAULT_DATABASE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const WAL_JOURNAL_SIZE_LIMIT_BYTES: i64 = 64 * 1024 * 1024;
const WAL_AUTOCHECKPOINT_PAGES: i64 = 1_000;
const ABSENT_MUTATION_ESTIMATED_BYTES: u64 = 96;
const PRESENT_MUTATION_ESTIMATED_BYTES: u64 = 160;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceReplicaLimits {
    pub max_membership_entries: u64,
    pub max_dirty_mutations: usize,
    pub max_dirty_bytes: u64,
    pub checkpoint_chunk_entries: usize,
    /// Hard limit for the main SQLite database file. WAL peak headroom is a
    /// separate deployment concern and is not included in this value.
    pub max_database_bytes: u64,
}

impl Default for SourceReplicaLimits {
    fn default() -> Self {
        Self {
            max_membership_entries: MAX_CHECKPOINT_ENTRIES,
            max_dirty_mutations: MAX_STATE_DELTA_MUTATIONS,
            max_dirty_bytes: DEFAULT_MAX_DIRTY_BYTES,
            checkpoint_chunk_entries: MAX_CHECKPOINT_CHUNK_ENTRIES,
            max_database_bytes: DEFAULT_MAX_DATABASE_BYTES,
        }
    }
}

impl SourceReplicaLimits {
    pub fn validate(self) -> Result<Self, SourceReplicaStoreError> {
        validate_limit(
            "max_membership_entries",
            self.max_membership_entries,
            MAX_CHECKPOINT_ENTRIES,
        )?;
        validate_limit(
            "max_dirty_mutations",
            u64::try_from(self.max_dirty_mutations).map_err(|_| {
                SourceReplicaStoreError::NumericOverflow {
                    field: "max_dirty_mutations",
                }
            })?,
            MAX_STATE_DELTA_MUTATIONS as u64,
        )?;
        if self.max_dirty_bytes == 0 {
            return Err(SourceReplicaStoreError::InvalidLimit {
                field: "max_dirty_bytes",
                value: 0,
                maximum: u64::MAX,
            });
        }
        validate_limit(
            "checkpoint_chunk_entries",
            u64::try_from(self.checkpoint_chunk_entries).map_err(|_| {
                SourceReplicaStoreError::NumericOverflow {
                    field: "checkpoint_chunk_entries",
                }
            })?,
            MAX_CHECKPOINT_CHUNK_ENTRIES as u64,
        )?;
        validate_limit(
            "max_database_bytes",
            self.max_database_bytes,
            MAX_SAFE_JSON_INTEGER,
        )?;
        let checkpoint_capacity = u64::try_from(self.checkpoint_chunk_entries)
            .map_err(|_| SourceReplicaStoreError::NumericOverflow {
                field: "checkpoint_chunk_entries",
            })?
            .checked_mul(u64::from(MAX_CHECKPOINT_CHUNKS))
            .ok_or(SourceReplicaStoreError::NumericOverflow {
                field: "checkpoint_capacity",
            })?;
        if self.max_membership_entries > checkpoint_capacity {
            return Err(SourceReplicaStoreError::InvalidLimit {
                field: "max_membership_entries",
                value: self.max_membership_entries,
                maximum: checkpoint_capacity,
            });
        }
        Ok(self)
    }
}

#[derive(Clone, Debug)]
pub struct SourceReplica {
    path: Arc<PathBuf>,
    source_id: SourceId,
    limits: SourceReplicaLimits,
    database_busy_timeout: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceReplicaStatus {
    pub source_id: SourceId,
    pub epoch_id: SourceEpochId,
    pub local_revision: u64,
    pub acknowledged_revision: u64,
    pub has_acknowledged_cursor: bool,
    pub has_observed_snapshot: bool,
    pub checkpoint_required: bool,
    pub checkpoint_replacement_known: bool,
    pub checkpoint_supersedes_id: Option<CheckpointId>,
    pub checkpoint_replaces: Option<ReplicaCursor>,
    pub state_observed_at_ms: Option<u64>,
    pub acknowledged_state_observed_at_ms: Option<u64>,
    pub last_rpc_success_at_ms: Option<u64>,
}

impl SourceReplicaStatus {
    #[must_use]
    pub fn local_cursor(&self) -> Option<ReplicaCursor> {
        self.has_observed_snapshot.then(|| ReplicaCursor {
            epoch_id: self.epoch_id.clone(),
            revision: self.local_revision,
        })
    }

    #[must_use]
    pub fn acknowledged_cursor(&self) -> Option<ReplicaCursor> {
        self.has_acknowledged_cursor.then(|| ReplicaCursor {
            epoch_id: self.epoch_id.clone(),
            revision: self.acknowledged_revision,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObserveRpcOutcome {
    Baseline {
        revision: u64,
        entry_count: u64,
    },
    Changed {
        revision: u64,
        mutation_count: u64,
        checkpoint_required: bool,
    },
    Unchanged {
        revision: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceReplicaAction {
    Delta(StateDelta),
    Checkpoint(CheckpointBegin),
    Heartbeat(StateHeartbeat),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcknowledgeOutcome {
    Applied,
    Duplicate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceReplicaStorageStats {
    pub membership_rows: u64,
    pub dirty_rows: u64,
    pub dirty_estimated_bytes: u64,
    pub frozen_delta_rows: u64,
    pub frozen_checkpoint_rows: u64,
}

#[derive(Debug, Error)]
pub enum SourceReplicaStoreError {
    #[error("database error: {0}")]
    Database(rusqlite::Error),
    #[error("agent database is temporarily busy or locked: {0}")]
    DatabaseContention(rusqlite::Error),
    #[error("agent database reached its configured page capacity: {0}")]
    DatabaseCapacity(rusqlite::Error),
    #[error("source replica model error: {0}")]
    Model(#[from] atlas_model::SourceReplicaError),
    #[error("agent schema is at version {found}, expected {expected}")]
    SchemaVersion { found: i64, expected: i64 },
    #[error("source replica is bound to source {bound}, not configured source {configured}")]
    SourceBinding { bound: String, configured: String },
    #[error("{field} limit {value} must be between 1 and {maximum}")]
    InvalidLimit {
        field: &'static str,
        value: u64,
        maximum: u64,
    },
    #[error("RPC snapshot contains {found} entries; configured maximum is {maximum}")]
    SnapshotTooLarge { found: u64, maximum: u64 },
    #[error(
        "agent database already uses {current_bytes} bytes of pages; configured maximum is {maximum_bytes} bytes"
    )]
    DatabaseBudgetTooSmall {
        current_bytes: u64,
        maximum_bytes: u64,
    },
    #[error("numeric field {field} is too large for SQLite")]
    NumericOverflow { field: &'static str },
    #[error("database contains a negative value for {field}")]
    NegativeStoredInteger { field: &'static str },
    #[error("local revision has reached the exact wire integer maximum")]
    RevisionExhausted,
    #[error("source replica has not observed its first RPC snapshot")]
    NoObservedSnapshot,
    #[error("acknowledgement epoch {provided} does not match local epoch {expected}")]
    AcknowledgementEpoch { expected: String, provided: String },
    #[error(
        "acknowledgement revision {provided} does not match frozen target {expected:?} or acknowledged revision {acknowledged}"
    )]
    UnexpectedAcknowledgement {
        provided: u64,
        expected: Option<u64>,
        acknowledged: u64,
    },
    #[error(
        "heartbeat acknowledgement ({revision}, {observed_at_ms}) does not match the pending local freshness"
    )]
    UnexpectedHeartbeat { revision: u64, observed_at_ms: u64 },
    #[error("checkpoint {provided} does not match frozen checkpoint {expected:?}")]
    CheckpointMismatch {
        expected: Option<String>,
        provided: String,
    },
    #[error("checkpoint chunk index {index} is outside the frozen range of {expected_chunks}")]
    CheckpointChunkIndex { index: u32, expected_chunks: u32 },
    #[error("stored source replica state violates invariant: {0}")]
    StoredInvariant(&'static str),
}

impl From<rusqlite::Error> for SourceReplicaStoreError {
    fn from(error: rusqlite::Error) -> Self {
        match &error {
            rusqlite::Error::SqliteFailure(details, _)
                if details.code == rusqlite::ErrorCode::DiskFull =>
            {
                Self::DatabaseCapacity(error)
            }
            rusqlite::Error::SqliteFailure(details, _)
                if matches!(
                    details.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) =>
            {
                Self::DatabaseContention(error)
            }
            _ => Self::Database(error),
        }
    }
}

impl SourceReplicaStoreError {
    #[must_use]
    pub const fn is_retryable_contention(&self) -> bool {
        matches!(self, Self::DatabaseContention(_))
    }
}

#[derive(Clone, Debug)]
struct StoredState {
    status: SourceReplicaStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FrozenKind {
    Delta,
    Checkpoint,
}

#[derive(Clone, Debug)]
struct FrozenAction {
    kind: FrozenKind,
    checkpoint_id: Option<CheckpointId>,
    supersedes_checkpoint_id: Option<CheckpointId>,
    base_revision: Option<u64>,
    replaces: Option<ReplicaCursor>,
    target_revision: u64,
    state_observed_at_ms: u64,
    expected_entries: u64,
    checkpoint_chunk_entries: Option<usize>,
    content_sha256: String,
}

#[derive(Clone, Debug)]
struct MembershipChange {
    txid: String,
    before: Option<MempoolEntryFacts>,
    after: Option<MempoolEntryFacts>,
}

#[derive(Clone, Copy, Debug)]
struct DetailedChangeBudget {
    max_changes: usize,
    max_bytes: u64,
}

#[derive(Debug)]
struct CollectedMembershipChanges {
    detailed: Vec<MembershipChange>,
    mutation_count: u64,
    requires_checkpoint: bool,
    peak_buffered_changes: usize,
    peak_buffered_bytes: u64,
    buffered_bytes: u64,
    budget: Option<DetailedChangeBudget>,
}

impl CollectedMembershipChanges {
    fn new(budget: Option<DetailedChangeBudget>) -> Self {
        Self {
            detailed: Vec::new(),
            mutation_count: 0,
            requires_checkpoint: budget.is_none(),
            peak_buffered_changes: 0,
            peak_buffered_bytes: 0,
            buffered_bytes: 0,
            budget,
        }
    }

    fn record(
        &mut self,
        txid: &str,
        before: Option<&MempoolEntryFacts>,
        after: Option<&MempoolEntryFacts>,
    ) -> Result<(), SourceReplicaStoreError> {
        self.mutation_count =
            self.mutation_count
                .checked_add(1)
                .ok_or(SourceReplicaStoreError::NumericOverflow {
                    field: "snapshot_mutations",
                })?;
        if self.requires_checkpoint {
            return Ok(());
        }
        let budget = self.budget.ok_or(SourceReplicaStoreError::StoredInvariant(
            "detailed collection is missing its budget",
        ))?;
        let estimated_bytes = if after.is_some() {
            PRESENT_MUTATION_ESTIMATED_BYTES
        } else {
            ABSENT_MUTATION_ESTIMATED_BYTES
        };
        let next_bytes = self.buffered_bytes.checked_add(estimated_bytes).ok_or(
            SourceReplicaStoreError::NumericOverflow {
                field: "dirty_estimated_bytes",
            },
        )?;
        if self.detailed.len() >= budget.max_changes || next_bytes > budget.max_bytes {
            self.requires_checkpoint = true;
            self.detailed.clear();
            self.buffered_bytes = 0;
            return Ok(());
        }
        self.detailed.push(MembershipChange {
            txid: txid.to_owned(),
            before: before.cloned(),
            after: after.cloned(),
        });
        self.buffered_bytes = next_bytes;
        self.peak_buffered_changes = self.peak_buffered_changes.max(self.detailed.len());
        self.peak_buffered_bytes = self.peak_buffered_bytes.max(self.buffered_bytes);
        Ok(())
    }
}

impl SourceReplica {
    /// Opens an already migrated agent database and creates a durable epoch on
    /// first use. Reopening the same database retains that epoch.
    pub fn open(
        path: impl AsRef<Path>,
        source_id: SourceId,
        limits: SourceReplicaLimits,
    ) -> Result<Self, SourceReplicaStoreError> {
        Self::open_with_database_busy_timeout(
            path,
            source_id,
            limits,
            DEFAULT_DATABASE_BUSY_TIMEOUT,
        )
    }

    fn open_with_database_busy_timeout(
        path: impl AsRef<Path>,
        source_id: SourceId,
        limits: SourceReplicaLimits,
        database_busy_timeout: Duration,
    ) -> Result<Self, SourceReplicaStoreError> {
        let limits = limits.validate()?;
        let path = path.as_ref().to_path_buf();
        let mut connection =
            open_existing_connection(&path, limits.max_database_bytes, database_busy_timeout)?;
        require_latest_schema(&connection)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let bound_source = transaction
            .query_row(
                "SELECT source_id FROM agent_database WHERE singleton = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(bound) = bound_source.as_deref()
            && bound != source_id.as_str()
        {
            return Err(SourceReplicaStoreError::SourceBinding {
                bound: bound.to_owned(),
                configured: source_id.to_string(),
            });
        }

        let replica_epoch = transaction
            .query_row(
                "SELECT epoch_id FROM source_replica_state WHERE singleton = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if bound_source.is_none() {
            transaction.execute(
                "INSERT INTO agent_database (singleton, source_id) VALUES (1, ?1)",
                [source_id.as_str()],
            )?;
        }
        if replica_epoch.is_none() {
            let epoch_id = SourceEpochId::new(Uuid::new_v4().to_string())?;
            transaction.execute(
                "INSERT INTO source_replica_state (singleton, epoch_id)
                 VALUES (1, ?1)",
                [epoch_id.as_str()],
            )?;
        }
        transaction.commit()?;

        Ok(Self {
            path: Arc::new(path),
            source_id,
            limits,
            database_busy_timeout,
        })
    }

    #[cfg(test)]
    pub(crate) fn open_with_test_busy_timeout(
        path: impl AsRef<Path>,
        source_id: SourceId,
        limits: SourceReplicaLimits,
        database_busy_timeout: Duration,
    ) -> Result<Self, SourceReplicaStoreError> {
        Self::open_with_database_busy_timeout(path, source_id, limits, database_busy_timeout)
    }

    #[must_use]
    pub const fn source_id(&self) -> &SourceId {
        &self.source_id
    }

    #[must_use]
    pub const fn limits(&self) -> SourceReplicaLimits {
        self.limits
    }

    pub fn status(&self) -> Result<SourceReplicaStatus, SourceReplicaStoreError> {
        let connection = self.connect()?;
        Ok(load_state(&connection)?.status)
    }

    /// Applies one complete, fact-bearing RPC snapshot atomically.
    ///
    /// The first successful observation always establishes revision 1 and
    /// requires a full checkpoint, even when the snapshot is empty. Later
    /// unchanged observations update freshness only. Changed observations
    /// advance once per complete snapshot and coalesce each txid into one dirty
    /// marker.
    pub fn observe_rpc_snapshot(
        &self,
        snapshot: &BTreeMap<String, MempoolEntryFacts>,
        completed_at_ms: u64,
    ) -> Result<ObserveRpcOutcome, SourceReplicaStoreError> {
        let entry_count = u64::try_from(snapshot.len()).map_err(|_| {
            SourceReplicaStoreError::NumericOverflow {
                field: "snapshot_entries",
            }
        })?;
        if entry_count > self.limits.max_membership_entries {
            return Err(SourceReplicaStoreError::SnapshotTooLarge {
                found: entry_count,
                maximum: self.limits.max_membership_entries,
            });
        }
        validate_snapshot(snapshot)?;
        StateHeartbeat::new(1, completed_at_ms)?;

        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = load_state(&transaction)?;
        if !state.status.has_observed_snapshot {
            replace_membership(&transaction, snapshot)?;
            transaction.execute("DELETE FROM source_replica_dirty", [])?;
            transaction.execute("DELETE FROM source_replica_frozen_delta", [])?;
            transaction.execute("DELETE FROM source_replica_frozen_checkpoint", [])?;
            transaction.execute("DELETE FROM source_replica_frozen_action", [])?;
            transaction.execute(
                "UPDATE source_replica_state
                 SET local_revision = 1,
                     has_observed_snapshot = 1,
                     checkpoint_required = 1,
                     state_observed_at_ms = ?1,
                     last_rpc_success_at_ms = ?1
                 WHERE singleton = 1",
                [to_sqlite_integer(completed_at_ms, "completed_at_ms")?],
            )?;
            transaction.commit()?;
            return Ok(ObserveRpcOutcome::Baseline {
                revision: 1,
                entry_count,
            });
        }

        let (dirty_rows_before, dirty_bytes_before) = dirty_pressure(&transaction)?;
        let detailed_budget = if !state.status.checkpoint_required
            && dirty_rows_before <= self.limits.max_dirty_mutations as u64
            && dirty_bytes_before <= self.limits.max_dirty_bytes
        {
            let dirty_rows_before = usize::try_from(dirty_rows_before).map_err(|_| {
                SourceReplicaStoreError::NumericOverflow {
                    field: "dirty_rows",
                }
            })?;
            Some(DetailedChangeBudget {
                max_changes: self
                    .limits
                    .max_dirty_mutations
                    .saturating_sub(dirty_rows_before),
                max_bytes: self
                    .limits
                    .max_dirty_bytes
                    .saturating_sub(dirty_bytes_before),
            })
        } else {
            None
        };
        let changes = collect_membership_changes(&transaction, snapshot, detailed_budget)?;
        let observed_at_ms = state
            .status
            .state_observed_at_ms
            .map_or(completed_at_ms, |previous| previous.max(completed_at_ms));
        if changes.mutation_count == 0 {
            if changes.requires_checkpoint && !state.status.checkpoint_required {
                transaction.execute("DELETE FROM source_replica_dirty", [])?;
                transaction.execute(
                    "UPDATE source_replica_state
                     SET checkpoint_required = 1
                     WHERE singleton = 1",
                    [],
                )?;
            }
            transaction.execute(
                "UPDATE source_replica_state
                 SET state_observed_at_ms = ?1, last_rpc_success_at_ms = ?1
                 WHERE singleton = 1",
                [to_sqlite_integer(observed_at_ms, "state_observed_at_ms")?],
            )?;
            transaction.commit()?;
            return Ok(ObserveRpcOutcome::Unchanged {
                revision: state.status.local_revision,
            });
        }

        let revision = state
            .status
            .local_revision
            .checked_add(1)
            .filter(|revision| *revision <= MAX_SAFE_JSON_INTEGER)
            .ok_or(SourceReplicaStoreError::RevisionExhausted)?;

        if changes.requires_checkpoint {
            // The validated RPC map is already the one unavoidable full
            // in-memory representation. Install it directly without retaining
            // a second full diff or writing transient dirty tombstones.
            transaction.execute("DELETE FROM source_replica_dirty", [])?;
            replace_membership(&transaction, snapshot)?;
            transaction.execute(
                "UPDATE source_replica_state
                 SET local_revision = ?1,
                     checkpoint_required = 1,
                     state_observed_at_ms = ?2,
                     last_rpc_success_at_ms = ?2
                 WHERE singleton = 1",
                params![
                    to_sqlite_integer(revision, "local_revision")?,
                    to_sqlite_integer(observed_at_ms, "state_observed_at_ms")?,
                ],
            )?;
            transaction.commit()?;
            return Ok(ObserveRpcOutcome::Changed {
                revision,
                mutation_count: changes.mutation_count,
                checkpoint_required: true,
            });
        }

        let frozen = load_frozen_action(&transaction)?;
        for change in &changes.detailed {
            let base = effective_dirty_base(&transaction, change, frozen.as_ref())?;
            apply_current_membership(&transaction, &change.txid, change.after.as_ref())?;
            record_dirty_divergence(
                &transaction,
                &change.txid,
                base.as_ref(),
                change.after.as_ref(),
                revision,
            )?;
        }

        transaction.execute(
            "UPDATE source_replica_state
             SET local_revision = ?1,
                 state_observed_at_ms = ?2,
                 last_rpc_success_at_ms = ?2
             WHERE singleton = 1",
            params![
                to_sqlite_integer(revision, "local_revision")?,
                to_sqlite_integer(observed_at_ms, "state_observed_at_ms")?,
            ],
        )?;

        let (dirty_rows, dirty_bytes) = dirty_pressure(&transaction)?;
        let pressure = dirty_rows > self.limits.max_dirty_mutations as u64
            || dirty_bytes > self.limits.max_dirty_bytes;
        if pressure {
            return Err(SourceReplicaStoreError::StoredInvariant(
                "dirty persistence exceeded its preflight budget",
            ));
        }
        let uncovered_revision = dirty_rows == 0
            && if let Some(frozen) = frozen.as_ref() {
                revision > frozen.target_revision
            } else {
                state.status.has_acknowledged_cursor
                    && revision > state.status.acknowledged_revision
            };
        if uncovered_revision {
            // The local revision advanced but its net state returned to the
            // remote base. A checkpoint publishes that otherwise uncovered
            // revision without inventing an empty delta.
            transaction.execute("DELETE FROM source_replica_dirty", [])?;
            transaction.execute(
                "UPDATE source_replica_state SET checkpoint_required = 1 WHERE singleton = 1",
                [],
            )?;
        }

        let checkpoint_required = uncovered_revision || state.status.checkpoint_required;
        transaction.commit()?;
        Ok(ObserveRpcOutcome::Changed {
            revision,
            mutation_count: changes.mutation_count,
            checkpoint_required,
        })
    }

    /// Returns the one stable action the delivery layer should attempt next.
    /// Frozen actions are returned byte-for-byte equivalently across retries
    /// and process restarts.
    pub fn next_action(&self) -> Result<Option<SourceReplicaAction>, SourceReplicaStoreError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = load_state(&transaction)?;
        if !state.status.has_observed_snapshot {
            transaction.commit()?;
            return Ok(None);
        }
        if state.status.local_revision == 0 {
            return Err(SourceReplicaStoreError::StoredInvariant(
                "an observed snapshot cannot remain at revision zero",
            ));
        }

        if let Some(frozen) = load_frozen_action(&transaction)? {
            let action = load_frozen_wire_action(&transaction, &frozen)?;
            transaction.commit()?;
            return Ok(Some(action));
        }

        if state.status.checkpoint_required {
            let begin = freeze_checkpoint(&transaction, &state, self.limits)?;
            transaction.commit()?;
            return Ok(Some(SourceReplicaAction::Checkpoint(begin)));
        }

        let dirty_rows = row_count(&transaction, "source_replica_dirty")?;
        if dirty_rows > 0 {
            if !state.status.has_acknowledged_cursor {
                return Err(SourceReplicaStoreError::StoredInvariant(
                    "a delta requires an acknowledged base cursor",
                ));
            }
            let delta = freeze_delta(&transaction, &state)?;
            transaction.commit()?;
            return Ok(Some(SourceReplicaAction::Delta(delta)));
        }

        if !state.status.has_acknowledged_cursor
            || state.status.acknowledged_revision != state.status.local_revision
        {
            return Err(SourceReplicaStoreError::StoredInvariant(
                "an idle replica requires its local cursor to be acknowledged",
            ));
        }
        let observed_at_ms =
            state
                .status
                .state_observed_at_ms
                .ok_or(SourceReplicaStoreError::StoredInvariant(
                    "an observed snapshot requires an observation time",
                ))?;
        if state
            .status
            .acknowledged_state_observed_at_ms
            .is_some_and(|acknowledged| acknowledged >= observed_at_ms)
        {
            transaction.commit()?;
            return Ok(None);
        }
        let heartbeat = StateHeartbeat::new(state.status.local_revision, observed_at_ms)?;
        transaction.commit()?;
        Ok(Some(SourceReplicaAction::Heartbeat(heartbeat)))
    }

    /// Materializes one deterministic chunk from the frozen checkpoint.
    pub fn checkpoint_chunk(
        &self,
        checkpoint_id: &CheckpointId,
        chunk_index: u32,
    ) -> Result<CheckpointChunk, SourceReplicaStoreError> {
        let connection = self.connect()?;
        let frozen = load_frozen_action(&connection)?;
        let expected = frozen
            .as_ref()
            .and_then(|action| action.checkpoint_id.as_ref())
            .map(ToString::to_string);
        let Some(frozen) = frozen.filter(|action| action.kind == FrozenKind::Checkpoint) else {
            return Err(SourceReplicaStoreError::CheckpointMismatch {
                expected,
                provided: checkpoint_id.to_string(),
            });
        };
        if frozen.checkpoint_id.as_ref() != Some(checkpoint_id) {
            return Err(SourceReplicaStoreError::CheckpointMismatch {
                expected: frozen.checkpoint_id.map(|value| value.to_string()),
                provided: checkpoint_id.to_string(),
            });
        }
        let chunk_entries =
            frozen
                .checkpoint_chunk_entries
                .ok_or(SourceReplicaStoreError::StoredInvariant(
                    "checkpoint is missing its chunk size",
                ))?;
        let expected_chunks = checkpoint_chunk_count(frozen.expected_entries, chunk_entries)?;
        if chunk_index >= expected_chunks {
            return Err(SourceReplicaStoreError::CheckpointChunkIndex {
                index: chunk_index,
                expected_chunks,
            });
        }
        let offset = u64::from(chunk_index)
            .checked_mul(u64::try_from(chunk_entries).map_err(|_| {
                SourceReplicaStoreError::NumericOverflow {
                    field: "checkpoint_chunk_entries",
                }
            })?)
            .ok_or(SourceReplicaStoreError::NumericOverflow {
                field: "checkpoint_chunk_offset",
            })?;
        let entries = load_checkpoint_entries_page(&connection, offset, chunk_entries)?;
        let expected_entries = usize::try_from(
            (frozen.expected_entries - offset).min(chunk_entries as u64),
        )
        .map_err(|_| SourceReplicaStoreError::NumericOverflow {
            field: "checkpoint_chunk_entries",
        })?;
        if entries.len() != expected_entries {
            return Err(SourceReplicaStoreError::StoredInvariant(
                "frozen checkpoint contains an incomplete ordinal range",
            ));
        }
        Ok(CheckpointChunk::new(
            checkpoint_id.clone(),
            chunk_index,
            entries,
        )?)
    }

    pub fn checkpoint_commit(
        &self,
        checkpoint_id: &CheckpointId,
    ) -> Result<CheckpointCommit, SourceReplicaStoreError> {
        let connection = self.connect()?;
        let frozen = load_frozen_action(&connection)?;
        let expected = frozen
            .as_ref()
            .and_then(|action| action.checkpoint_id.as_ref())
            .map(ToString::to_string);
        let Some(frozen) = frozen.filter(|action| action.kind == FrozenKind::Checkpoint) else {
            return Err(SourceReplicaStoreError::CheckpointMismatch {
                expected,
                provided: checkpoint_id.to_string(),
            });
        };
        if frozen.checkpoint_id.as_ref() != Some(checkpoint_id) {
            return Err(SourceReplicaStoreError::CheckpointMismatch {
                expected: frozen.checkpoint_id.map(|value| value.to_string()),
                provided: checkpoint_id.to_string(),
            });
        }
        Ok(CheckpointCommit::new(
            checkpoint_id.clone(),
            frozen.target_revision,
            frozen.content_sha256,
        )?)
    }

    /// Persists activation of a frozen delta or checkpoint. Deleting dirty rows
    /// by revision, rather than wholesale, preserves changes observed after the
    /// action was frozen.
    pub fn acknowledge(
        &self,
        active_cursor: &ReplicaCursor,
    ) -> Result<AcknowledgeOutcome, SourceReplicaStoreError> {
        active_cursor.validate()?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = load_state(&transaction)?;
        if active_cursor.epoch_id != state.status.epoch_id {
            return Err(SourceReplicaStoreError::AcknowledgementEpoch {
                expected: state.status.epoch_id.to_string(),
                provided: active_cursor.epoch_id.to_string(),
            });
        }
        let frozen = load_frozen_action(&transaction)?;
        let matches_frozen = frozen
            .as_ref()
            .is_some_and(|action| action.target_revision == active_cursor.revision);
        if !matches_frozen
            && state.status.has_acknowledged_cursor
            && active_cursor.revision == state.status.acknowledged_revision
        {
            transaction.commit()?;
            return Ok(AcknowledgeOutcome::Duplicate);
        }
        let expected = frozen.as_ref().map(|action| action.target_revision);
        if expected != Some(active_cursor.revision) {
            return Err(SourceReplicaStoreError::UnexpectedAcknowledgement {
                provided: active_cursor.revision,
                expected,
                acknowledged: state.status.acknowledged_revision,
            });
        }

        let frozen = frozen.expect("matching frozen target checked above");
        transaction.execute(
            "UPDATE source_replica_state
             SET acknowledged_revision = ?1,
                 has_acknowledged_cursor = 1,
                 acknowledged_state_observed_at_ms = MAX(
                     COALESCE(acknowledged_state_observed_at_ms, 0), ?2
                 ),
                 checkpoint_replacement_known = CASE
                     WHEN ?3 = 'checkpoint' THEN 0
                     ELSE checkpoint_replacement_known
                 END,
                 checkpoint_supersedes_id = CASE
                     WHEN ?3 = 'checkpoint' THEN NULL
                     ELSE checkpoint_supersedes_id
                 END,
                 checkpoint_replaces_epoch_id = CASE
                     WHEN ?3 = 'checkpoint' THEN NULL
                     ELSE checkpoint_replaces_epoch_id
                 END,
                 checkpoint_replaces_revision = CASE
                     WHEN ?3 = 'checkpoint' THEN NULL
                     ELSE checkpoint_replaces_revision
                 END
             WHERE singleton = 1",
            params![
                to_sqlite_integer(active_cursor.revision, "acknowledged_revision")?,
                to_sqlite_integer(
                    frozen.state_observed_at_ms,
                    "acknowledged_state_observed_at_ms",
                )?,
                match frozen.kind {
                    FrozenKind::Delta => "delta",
                    FrozenKind::Checkpoint => "checkpoint",
                },
            ],
        )?;
        transaction.execute(
            "DELETE FROM source_replica_dirty WHERE dirty_revision <= ?1",
            [to_sqlite_integer(
                active_cursor.revision,
                "acknowledged_revision",
            )?],
        )?;
        rebase_surviving_dirty(&transaction, &frozen)?;
        let remaining_dirty = row_count(&transaction, "source_replica_dirty")?;
        if remaining_dirty == 0 && state.status.local_revision > active_cursor.revision {
            transaction.execute(
                "UPDATE source_replica_state SET checkpoint_required = 1 WHERE singleton = 1",
                [],
            )?;
        }
        clear_frozen_action(&transaction)?;
        transaction.commit()?;
        Ok(AcknowledgeOutcome::Applied)
    }

    /// Acknowledges freshness only. Heartbeats never advance membership
    /// revision and are emitted at most once per newly observed RPC time.
    pub fn acknowledge_heartbeat(
        &self,
        heartbeat: &StateHeartbeat,
        active_cursor: &ReplicaCursor,
    ) -> Result<AcknowledgeOutcome, SourceReplicaStoreError> {
        heartbeat.validate()?;
        active_cursor.validate()?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = load_state(&transaction)?;
        if active_cursor.epoch_id != state.status.epoch_id {
            return Err(SourceReplicaStoreError::AcknowledgementEpoch {
                expected: state.status.epoch_id.to_string(),
                provided: active_cursor.epoch_id.to_string(),
            });
        }
        let cursor_matches = state.status.has_acknowledged_cursor
            && active_cursor.revision == state.status.acknowledged_revision
            && heartbeat.revision == active_cursor.revision;
        let freshness_is_current = state
            .status
            .state_observed_at_ms
            .is_some_and(|current| heartbeat.state_observed_at_ms <= current);
        if !cursor_matches || !freshness_is_current {
            return Err(SourceReplicaStoreError::UnexpectedHeartbeat {
                revision: heartbeat.revision,
                observed_at_ms: heartbeat.state_observed_at_ms,
            });
        }
        if state.status.acknowledged_state_observed_at_ms == Some(heartbeat.state_observed_at_ms) {
            transaction.commit()?;
            return Ok(AcknowledgeOutcome::Duplicate);
        }
        if state
            .status
            .acknowledged_state_observed_at_ms
            .is_some_and(|acknowledged| heartbeat.state_observed_at_ms < acknowledged)
        {
            return Err(SourceReplicaStoreError::UnexpectedHeartbeat {
                revision: heartbeat.revision,
                observed_at_ms: heartbeat.state_observed_at_ms,
            });
        }
        transaction.execute(
            "UPDATE source_replica_state
             SET acknowledged_state_observed_at_ms = ?1
             WHERE singleton = 1",
            [to_sqlite_integer(
                heartbeat.state_observed_at_ms,
                "acknowledged_state_observed_at_ms",
            )?],
        )?;
        transaction.commit()?;
        Ok(AcknowledgeOutcome::Applied)
    }

    /// Abandons a completed or rejected frozen attempt and requires the next
    /// fresh action to be a full checkpoint. Current membership is retained.
    pub fn require_checkpoint(&self) -> Result<(), SourceReplicaStoreError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        clear_frozen_action(&transaction)?;
        transaction.execute("DELETE FROM source_replica_dirty", [])?;
        transaction.execute(
            "UPDATE source_replica_state
             SET checkpoint_required = 1,
                 checkpoint_replacement_known = 0,
                 checkpoint_supersedes_id = NULL,
                 checkpoint_replaces_epoch_id = NULL,
                 checkpoint_replaces_revision = NULL
             WHERE singleton = 1",
            [],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Requires a checkpoint using the server's exact active cursor as the
    /// compare-and-swap replacement. A same-epoch cursor at or ahead of this
    /// local database cannot be advanced by a checkpoint in the same epoch, so
    /// the local epoch is rotated and the current snapshot becomes revision 1.
    pub fn require_checkpoint_against(
        &self,
        active_cursor: Option<&ReplicaCursor>,
        supersedes_checkpoint_id: Option<&CheckpointId>,
    ) -> Result<(), SourceReplicaStoreError> {
        if let Some(cursor) = active_cursor {
            cursor.validate()?;
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = load_state(&transaction)?;
        let rollback = active_cursor.is_some_and(|cursor| {
            cursor.epoch_id == state.status.epoch_id
                && cursor.revision >= state.status.local_revision
        });
        let replacement = active_cursor.cloned();
        clear_frozen_action(&transaction)?;
        transaction.execute("DELETE FROM source_replica_dirty", [])?;

        if rollback {
            let epoch_id = SourceEpochId::new(Uuid::new_v4().to_string())?;
            let revision = u64::from(state.status.has_observed_snapshot);
            transaction.execute(
                "UPDATE source_replica_state
                 SET epoch_id = ?1,
                     local_revision = ?2,
                     acknowledged_revision = 0,
                     has_acknowledged_cursor = 0,
                     acknowledged_state_observed_at_ms = NULL,
                     checkpoint_required = 1,
                     checkpoint_replacement_known = 1,
                     checkpoint_supersedes_id = ?3,
                     checkpoint_replaces_epoch_id = ?4,
                     checkpoint_replaces_revision = ?5
                 WHERE singleton = 1",
                params![
                    epoch_id.as_str(),
                    to_sqlite_integer(revision, "local_revision")?,
                    supersedes_checkpoint_id.map(CheckpointId::as_str),
                    replacement.as_ref().map(|cursor| cursor.epoch_id.as_str()),
                    replacement
                        .as_ref()
                        .map(|cursor| {
                            to_sqlite_integer(cursor.revision, "checkpoint_replaces_revision")
                        })
                        .transpose()?,
                ],
            )?;
        } else {
            transaction.execute(
                "UPDATE source_replica_state
                 SET checkpoint_required = 1,
                     checkpoint_replacement_known = 1,
                     checkpoint_supersedes_id = ?1,
                     checkpoint_replaces_epoch_id = ?2,
                     checkpoint_replaces_revision = ?3
                 WHERE singleton = 1",
                params![
                    supersedes_checkpoint_id.map(CheckpointId::as_str),
                    replacement.as_ref().map(|cursor| cursor.epoch_id.as_str()),
                    replacement
                        .as_ref()
                        .map(|cursor| {
                            to_sqlite_integer(cursor.revision, "checkpoint_replaces_revision")
                        })
                        .transpose()?,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn storage_stats(&self) -> Result<SourceReplicaStorageStats, SourceReplicaStoreError> {
        let connection = self.connect()?;
        let (dirty_rows, dirty_estimated_bytes) = dirty_pressure(&connection)?;
        Ok(SourceReplicaStorageStats {
            membership_rows: row_count(&connection, "source_replica_membership")?,
            dirty_rows,
            dirty_estimated_bytes,
            frozen_delta_rows: row_count(&connection, "source_replica_frozen_delta")?,
            frozen_checkpoint_rows: row_count(&connection, "source_replica_frozen_checkpoint")?,
        })
    }

    fn connect(&self) -> Result<Connection, SourceReplicaStoreError> {
        open_existing_connection(
            &self.path,
            self.limits.max_database_bytes,
            self.database_busy_timeout,
        )
    }
}

fn validate_limit(
    field: &'static str,
    value: u64,
    maximum: u64,
) -> Result<(), SourceReplicaStoreError> {
    if value == 0 || value > maximum {
        return Err(SourceReplicaStoreError::InvalidLimit {
            field,
            value,
            maximum,
        });
    }
    Ok(())
}

fn open_existing_connection(
    path: &Path,
    max_database_bytes: u64,
    database_busy_timeout: Duration,
) -> Result<Connection, SourceReplicaStoreError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(database_busy_timeout)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "temp_store", "MEMORY")?;
    connection.pragma_update(None, "journal_size_limit", WAL_JOURNAL_SIZE_LIMIT_BYTES)?;
    connection.pragma_update(None, "wal_autocheckpoint", WAL_AUTOCHECKPOINT_PAGES)?;
    configure_database_page_limit(&connection, max_database_bytes)?;
    Ok(connection)
}

fn configure_database_page_limit(
    connection: &Connection,
    max_database_bytes: u64,
) -> Result<(), SourceReplicaStoreError> {
    let page_size = connection.pragma_query_value(None, "page_size", |row| row.get::<_, i64>(0))?;
    let page_count =
        connection.pragma_query_value(None, "page_count", |row| row.get::<_, i64>(0))?;
    let page_size = from_sqlite_integer(page_size, "page_size")?;
    let page_count = from_sqlite_integer(page_count, "page_count")?;
    let current_bytes =
        page_count
            .checked_mul(page_size)
            .ok_or(SourceReplicaStoreError::NumericOverflow {
                field: "database_page_bytes",
            })?;
    if current_bytes > max_database_bytes {
        return Err(SourceReplicaStoreError::DatabaseBudgetTooSmall {
            current_bytes,
            maximum_bytes: max_database_bytes,
        });
    }
    let max_pages = max_database_bytes / page_size;
    if max_pages == 0 {
        return Err(SourceReplicaStoreError::DatabaseBudgetTooSmall {
            current_bytes,
            maximum_bytes: max_database_bytes,
        });
    }
    connection.pragma_update(
        None,
        "max_page_count",
        to_sqlite_integer(max_pages, "max_page_count")?,
    )?;
    let applied =
        connection.pragma_query_value(None, "max_page_count", |row| row.get::<_, i64>(0))?;
    let applied = from_sqlite_integer(applied, "max_page_count")?;
    if applied != max_pages {
        return Err(SourceReplicaStoreError::StoredInvariant(
            "SQLite did not apply the configured maximum page count",
        ));
    }
    Ok(())
}

fn require_latest_schema(connection: &Connection) -> Result<(), SourceReplicaStoreError> {
    let found = connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
    if found == LATEST_SCHEMA_VERSION {
        return Ok(());
    }
    Err(SourceReplicaStoreError::SchemaVersion {
        found,
        expected: LATEST_SCHEMA_VERSION,
    })
}

fn validate_snapshot(
    snapshot: &BTreeMap<String, MempoolEntryFacts>,
) -> Result<(), SourceReplicaStoreError> {
    for (txid, facts) in snapshot {
        SourceReplicaEntry::new(txid.clone(), facts.clone())?;
    }
    Ok(())
}

fn load_state(connection: &Connection) -> Result<StoredState, SourceReplicaStoreError> {
    let stored = connection.query_row(
        "SELECT d.source_id, s.epoch_id, s.local_revision, s.acknowledged_revision,
                s.has_acknowledged_cursor, s.has_observed_snapshot,
                s.checkpoint_required, s.checkpoint_replacement_known,
                s.checkpoint_supersedes_id, s.checkpoint_replaces_epoch_id,
                s.checkpoint_replaces_revision,
                s.state_observed_at_ms, s.acknowledged_state_observed_at_ms,
                s.last_rpc_success_at_ms
         FROM source_replica_state AS s
         JOIN agent_database AS d ON d.singleton = 1
         WHERE s.singleton = 1",
        [],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<i64>>(10)?,
                row.get::<_, Option<i64>>(11)?,
                row.get::<_, Option<i64>>(12)?,
                row.get::<_, Option<i64>>(13)?,
            ))
        },
    )?;
    let (
        source_id,
        epoch_id,
        local_revision,
        acknowledged_revision,
        has_acknowledged_cursor,
        has_observed_snapshot,
        checkpoint_required,
        checkpoint_replacement_known,
        checkpoint_supersedes_id,
        checkpoint_replaces_epoch_id,
        checkpoint_replaces_revision,
        state_observed_at_ms,
        acknowledged_state_observed_at_ms,
        last_rpc_success_at_ms,
    ) = stored;
    let checkpoint_replaces = match (checkpoint_replaces_epoch_id, checkpoint_replaces_revision) {
        (None, None) => None,
        (Some(epoch_id), Some(revision)) => Some(ReplicaCursor {
            epoch_id: SourceEpochId::new(epoch_id)?,
            revision: from_sqlite_integer(revision, "checkpoint_replaces_revision")?,
        }),
        _ => {
            return Err(SourceReplicaStoreError::StoredInvariant(
                "checkpoint replacement cursor is incomplete",
            ));
        }
    };
    Ok(StoredState {
        status: SourceReplicaStatus {
            source_id: SourceId::new(source_id).map_err(atlas_model::SourceReplicaError::from)?,
            epoch_id: SourceEpochId::new(epoch_id)?,
            local_revision: from_sqlite_integer(local_revision, "local_revision")?,
            acknowledged_revision: from_sqlite_integer(
                acknowledged_revision,
                "acknowledged_revision",
            )?,
            has_acknowledged_cursor: stored_bool(
                has_acknowledged_cursor,
                "has_acknowledged_cursor",
            )?,
            has_observed_snapshot: stored_bool(has_observed_snapshot, "has_observed_snapshot")?,
            checkpoint_required: stored_bool(checkpoint_required, "checkpoint_required")?,
            checkpoint_replacement_known: stored_bool(
                checkpoint_replacement_known,
                "checkpoint_replacement_known",
            )?,
            checkpoint_supersedes_id: checkpoint_supersedes_id
                .map(CheckpointId::new)
                .transpose()?,
            checkpoint_replaces,
            state_observed_at_ms: optional_stored_u64(
                state_observed_at_ms,
                "state_observed_at_ms",
            )?,
            acknowledged_state_observed_at_ms: optional_stored_u64(
                acknowledged_state_observed_at_ms,
                "acknowledged_state_observed_at_ms",
            )?,
            last_rpc_success_at_ms: optional_stored_u64(
                last_rpc_success_at_ms,
                "last_rpc_success_at_ms",
            )?,
        },
    })
}

fn collect_membership_changes(
    connection: &Connection,
    snapshot: &BTreeMap<String, MempoolEntryFacts>,
    budget: Option<DetailedChangeBudget>,
) -> Result<CollectedMembershipChanges, SourceReplicaStoreError> {
    let mut statement = connection.prepare(
        "SELECT txid, vsize, fee_sats, entered_at_ms
         FROM source_replica_membership
         ORDER BY txid",
    )?;
    let mut rows = statement.query([])?;
    let mut current = rows.next()?.map(decode_entry_row).transpose()?;
    let mut desired = snapshot.iter().peekable();
    let mut changes = CollectedMembershipChanges::new(budget);

    loop {
        match (current.as_ref(), desired.peek()) {
            (None, None) => break,
            (Some(existing), None) => {
                changes.record(&existing.txid, Some(&existing.facts), None)?;
                current = rows.next()?.map(decode_entry_row).transpose()?;
            }
            (None, Some((txid, facts))) => {
                changes.record(txid, None, Some(facts))?;
                desired.next();
            }
            (Some(existing), Some((txid, facts))) => {
                match existing.txid.as_str().cmp(txid.as_str()) {
                    std::cmp::Ordering::Less => {
                        changes.record(&existing.txid, Some(&existing.facts), None)?;
                        current = rows.next()?.map(decode_entry_row).transpose()?;
                    }
                    std::cmp::Ordering::Greater => {
                        changes.record(txid, None, Some(facts))?;
                        desired.next();
                    }
                    std::cmp::Ordering::Equal => {
                        if existing.facts != **facts {
                            changes.record(&existing.txid, Some(&existing.facts), Some(facts))?;
                        }
                        current = rows.next()?.map(decode_entry_row).transpose()?;
                        desired.next();
                    }
                }
            }
        }
    }
    Ok(changes)
}

fn replace_membership(
    transaction: &Transaction<'_>,
    snapshot: &BTreeMap<String, MempoolEntryFacts>,
) -> Result<(), SourceReplicaStoreError> {
    transaction.execute("DELETE FROM source_replica_membership", [])?;
    for (txid, facts) in snapshot {
        upsert_membership(transaction, txid, facts)?;
    }
    Ok(())
}

fn upsert_membership(
    transaction: &Transaction<'_>,
    txid: &str,
    facts: &MempoolEntryFacts,
) -> Result<(), SourceReplicaStoreError> {
    transaction.execute(
        "INSERT INTO source_replica_membership (txid, vsize, fee_sats, entered_at_ms)
         VALUES (?1, ?2, ?3, ?4)
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
    Ok(())
}

fn apply_current_membership(
    transaction: &Transaction<'_>,
    txid: &str,
    facts: Option<&MempoolEntryFacts>,
) -> Result<(), SourceReplicaStoreError> {
    match facts {
        Some(facts) => upsert_membership(transaction, txid, facts)?,
        None => {
            transaction.execute(
                "DELETE FROM source_replica_membership WHERE txid = ?1",
                [txid],
            )?;
        }
    }
    Ok(())
}

fn effective_dirty_base(
    connection: &Connection,
    change: &MembershipChange,
    frozen: Option<&FrozenAction>,
) -> Result<Option<MempoolEntryFacts>, SourceReplicaStoreError> {
    if let Some(frozen) = frozen
        && let Some(delivered) = frozen_membership_for_txid(connection, frozen, &change.txid)?
    {
        return Ok(delivered);
    }
    if let Some(base) = load_dirty_base(connection, &change.txid)? {
        return Ok(base);
    }
    Ok(change.before.clone())
}

/// Returns `None` when a delta does not cover the txid. Checkpoints cover the
/// entire keyspace, so an absent entry is returned as `Some(None)`.
fn frozen_membership_for_txid(
    connection: &Connection,
    frozen: &FrozenAction,
    txid: &str,
) -> Result<Option<Option<MempoolEntryFacts>>, SourceReplicaStoreError> {
    match frozen.kind {
        FrozenKind::Delta => {
            let stored = connection
                .query_row(
                    "SELECT vsize, fee_sats, entered_at_ms
                     FROM source_replica_frozen_delta
                     WHERE txid = ?1",
                    [txid],
                    |row| {
                        Ok((
                            row.get::<_, Option<i64>>(0)?,
                            row.get::<_, Option<i64>>(1)?,
                            row.get::<_, Option<i64>>(2)?,
                        ))
                    },
                )
                .optional()?;
            stored.map(decode_optional_facts).transpose()
        }
        FrozenKind::Checkpoint => {
            let stored = connection
                .query_row(
                    "SELECT vsize, fee_sats, entered_at_ms
                     FROM source_replica_frozen_checkpoint
                     WHERE txid = ?1",
                    [txid],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()?;
            Ok(Some(
                stored
                    .map(
                        |(vsize, fee_sats, entered_at_ms)| -> Result<_, SourceReplicaStoreError> {
                            Ok(MempoolEntryFacts {
                                vsize: from_sqlite_integer(vsize, "vsize")?,
                                fee_sats: from_sqlite_integer(fee_sats, "fee_sats")?,
                                entered_at_ms: from_sqlite_integer(entered_at_ms, "entered_at_ms")?,
                            })
                        },
                    )
                    .transpose()?,
            ))
        }
    }
}

fn load_dirty_base(
    connection: &Connection,
    txid: &str,
) -> Result<Option<Option<MempoolEntryFacts>>, SourceReplicaStoreError> {
    let stored = connection
        .query_row(
            "SELECT base_vsize, base_fee_sats, base_entered_at_ms
             FROM source_replica_dirty
             WHERE txid = ?1",
            [txid],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            },
        )
        .optional()?;
    stored.map(decode_optional_facts).transpose()
}

fn decode_optional_facts(
    stored: (Option<i64>, Option<i64>, Option<i64>),
) -> Result<Option<MempoolEntryFacts>, SourceReplicaStoreError> {
    match stored {
        (None, None, None) => Ok(None),
        (Some(vsize), Some(fee_sats), Some(entered_at_ms)) => Ok(Some(MempoolEntryFacts {
            vsize: from_sqlite_integer(vsize, "vsize")?,
            fee_sats: from_sqlite_integer(fee_sats, "fee_sats")?,
            entered_at_ms: from_sqlite_integer(entered_at_ms, "entered_at_ms")?,
        })),
        _ => Err(SourceReplicaStoreError::StoredInvariant(
            "stored membership facts are incomplete",
        )),
    }
}

fn record_dirty_divergence(
    transaction: &Transaction<'_>,
    txid: &str,
    base: Option<&MempoolEntryFacts>,
    current: Option<&MempoolEntryFacts>,
    revision: u64,
) -> Result<(), SourceReplicaStoreError> {
    if base == current {
        transaction.execute("DELETE FROM source_replica_dirty WHERE txid = ?1", [txid])?;
        return Ok(());
    }
    let (base_membership, base_vsize, base_fee_sats, base_entered_at_ms) = match base {
        Some(facts) => (
            "present",
            Some(to_sqlite_integer(facts.vsize, "base_vsize")?),
            Some(to_sqlite_integer(facts.fee_sats, "base_fee_sats")?),
            Some(to_sqlite_integer(
                facts.entered_at_ms,
                "base_entered_at_ms",
            )?),
        ),
        None => ("absent", None, None, None),
    };
    let estimated_bytes = if current.is_some() {
        PRESENT_MUTATION_ESTIMATED_BYTES
    } else {
        ABSENT_MUTATION_ESTIMATED_BYTES
    };
    transaction.execute(
        "INSERT INTO source_replica_dirty (
             txid, base_membership, base_vsize, base_fee_sats,
             base_entered_at_ms, dirty_revision, estimated_bytes
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (txid) DO UPDATE SET
             base_membership = excluded.base_membership,
             base_vsize = excluded.base_vsize,
             base_fee_sats = excluded.base_fee_sats,
             base_entered_at_ms = excluded.base_entered_at_ms,
             dirty_revision = excluded.dirty_revision,
             estimated_bytes = excluded.estimated_bytes",
        params![
            txid,
            base_membership,
            base_vsize,
            base_fee_sats,
            base_entered_at_ms,
            to_sqlite_integer(revision, "dirty_revision")?,
            to_sqlite_integer(estimated_bytes, "estimated_bytes")?,
        ],
    )?;
    Ok(())
}

fn load_frozen_action(
    connection: &Connection,
) -> Result<Option<FrozenAction>, SourceReplicaStoreError> {
    let row = connection
        .query_row(
            "SELECT action_kind, checkpoint_id, supersedes_checkpoint_id,
                    base_revision, replaces_epoch_id, replaces_revision,
                    target_revision, state_observed_at_ms, expected_entries,
                    checkpoint_chunk_entries, content_sha256
             FROM source_replica_frozen_action
             WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                    row.get::<_, String>(10)?,
                ))
            },
        )
        .optional()?;
    let Some((
        kind,
        checkpoint_id,
        supersedes_checkpoint_id,
        base_revision,
        replaces_epoch_id,
        replaces_revision,
        target_revision,
        state_observed_at_ms,
        expected_entries,
        checkpoint_chunk_entries,
        content_sha256,
    )) = row
    else {
        return Ok(None);
    };
    let kind = match kind.as_str() {
        "delta" => FrozenKind::Delta,
        "checkpoint" => FrozenKind::Checkpoint,
        _ => {
            return Err(SourceReplicaStoreError::StoredInvariant(
                "unknown frozen action kind",
            ));
        }
    };
    let replaces = match (replaces_epoch_id, replaces_revision) {
        (None, None) => None,
        (Some(epoch_id), Some(revision)) => Some(ReplicaCursor {
            epoch_id: SourceEpochId::new(epoch_id)?,
            revision: from_sqlite_integer(revision, "replaces_revision")?,
        }),
        _ => {
            return Err(SourceReplicaStoreError::StoredInvariant(
                "frozen replacement cursor is incomplete",
            ));
        }
    };
    Ok(Some(FrozenAction {
        kind,
        checkpoint_id: checkpoint_id.map(CheckpointId::new).transpose()?,
        supersedes_checkpoint_id: supersedes_checkpoint_id
            .map(CheckpointId::new)
            .transpose()?,
        base_revision: optional_stored_u64(base_revision, "base_revision")?,
        replaces,
        target_revision: from_sqlite_integer(target_revision, "target_revision")?,
        state_observed_at_ms: from_sqlite_integer(state_observed_at_ms, "state_observed_at_ms")?,
        expected_entries: from_sqlite_integer(expected_entries, "expected_entries")?,
        checkpoint_chunk_entries: checkpoint_chunk_entries
            .map(|value| {
                usize::try_from(from_sqlite_integer(value, "checkpoint_chunk_entries")?).map_err(
                    |_| SourceReplicaStoreError::NumericOverflow {
                        field: "checkpoint_chunk_entries",
                    },
                )
            })
            .transpose()?,
        content_sha256,
    }))
}

fn load_frozen_wire_action(
    connection: &Connection,
    frozen: &FrozenAction,
) -> Result<SourceReplicaAction, SourceReplicaStoreError> {
    match frozen.kind {
        FrozenKind::Delta => {
            let mutations = load_frozen_delta(connection)?;
            let delta = StateDelta {
                base_revision: frozen.base_revision.ok_or(
                    SourceReplicaStoreError::StoredInvariant(
                        "frozen delta is missing its base revision",
                    ),
                )?,
                target_revision: frozen.target_revision,
                state_observed_at_ms: frozen.state_observed_at_ms,
                mutations,
                content_sha256: frozen.content_sha256.clone(),
            };
            delta.validate()?;
            Ok(SourceReplicaAction::Delta(delta))
        }
        FrozenKind::Checkpoint => {
            if row_count(connection, "source_replica_frozen_checkpoint")? != frozen.expected_entries
            {
                return Err(SourceReplicaStoreError::StoredInvariant(
                    "frozen checkpoint row count does not match its declaration",
                ));
            }
            let chunk_entries =
                frozen
                    .checkpoint_chunk_entries
                    .ok_or(SourceReplicaStoreError::StoredInvariant(
                        "checkpoint is missing its chunk size",
                    ))?;
            let expected_chunks = checkpoint_chunk_count(frozen.expected_entries, chunk_entries)?;
            let begin = CheckpointBegin {
                checkpoint_id: frozen.checkpoint_id.clone().ok_or(
                    SourceReplicaStoreError::StoredInvariant(
                        "frozen checkpoint is missing its identifier",
                    ),
                )?,
                supersedes_checkpoint_id: frozen.supersedes_checkpoint_id.clone(),
                replaces: frozen.replaces.clone(),
                target_revision: frozen.target_revision,
                state_observed_at_ms: frozen.state_observed_at_ms,
                expected_entries: frozen.expected_entries,
                expected_chunks,
                content_sha256: frozen.content_sha256.clone(),
            };
            begin.validate()?;
            Ok(SourceReplicaAction::Checkpoint(begin))
        }
    }
}

fn freeze_delta(
    transaction: &Transaction<'_>,
    state: &StoredState,
) -> Result<StateDelta, SourceReplicaStoreError> {
    let mutations = load_dirty_mutations(transaction)?;
    let observed_at =
        state
            .status
            .state_observed_at_ms
            .ok_or(SourceReplicaStoreError::StoredInvariant(
                "an observed snapshot requires an observation time",
            ))?;
    let delta = StateDelta::new(
        state.status.acknowledged_revision,
        state.status.local_revision,
        observed_at,
        mutations,
    )?;
    transaction.execute(
        "INSERT INTO source_replica_frozen_action (
             singleton, action_kind, base_revision, target_revision,
             state_observed_at_ms, expected_entries, content_sha256
         ) VALUES (1, 'delta', ?1, ?2, ?3, ?4, ?5)",
        params![
            to_sqlite_integer(delta.base_revision, "base_revision")?,
            to_sqlite_integer(delta.target_revision, "target_revision")?,
            to_sqlite_integer(delta.state_observed_at_ms, "state_observed_at_ms")?,
            to_sqlite_integer(delta.mutations.len() as u64, "expected_entries")?,
            &delta.content_sha256,
        ],
    )?;
    for mutation in &delta.mutations {
        match mutation {
            StateMutation::Absent { txid } => {
                transaction.execute(
                    "INSERT INTO source_replica_frozen_delta (txid, membership)
                     VALUES (?1, 'absent')",
                    [txid],
                )?;
            }
            StateMutation::Present { txid, facts } => {
                transaction.execute(
                    "INSERT INTO source_replica_frozen_delta (
                         txid, membership, vsize, fee_sats, entered_at_ms
                     ) VALUES (?1, 'present', ?2, ?3, ?4)",
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
    Ok(delta)
}

fn freeze_checkpoint(
    transaction: &Transaction<'_>,
    state: &StoredState,
    limits: SourceReplicaLimits,
) -> Result<CheckpointBegin, SourceReplicaStoreError> {
    let entry_count = row_count(transaction, "source_replica_membership")?;
    if entry_count > limits.max_membership_entries {
        return Err(SourceReplicaStoreError::SnapshotTooLarge {
            found: entry_count,
            maximum: limits.max_membership_entries,
        });
    }
    let checkpoint_id = CheckpointId::new(Uuid::new_v4().to_string())?;
    let expected_chunks = checkpoint_chunk_count(entry_count, limits.checkpoint_chunk_entries)?;
    let mut digest = CheckpointDigest::new(entry_count)?;
    let mut statement = transaction.prepare(
        "SELECT txid, vsize, fee_sats, entered_at_ms
         FROM source_replica_membership
         ORDER BY txid",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        digest.push(&decode_entry_row(row)?)?;
    }
    drop(rows);
    drop(statement);
    let content_sha256 = digest.finish()?;
    let observed_at =
        state
            .status
            .state_observed_at_ms
            .ok_or(SourceReplicaStoreError::StoredInvariant(
                "an observed snapshot requires an observation time",
            ))?;
    let replaces = if state.status.checkpoint_replacement_known {
        state.status.checkpoint_replaces.clone()
    } else {
        state.status.acknowledged_cursor()
    };

    transaction.execute(
        "INSERT INTO source_replica_frozen_action (
             singleton, action_kind, checkpoint_id, supersedes_checkpoint_id,
             replaces_epoch_id, replaces_revision, target_revision,
             state_observed_at_ms, expected_entries, checkpoint_chunk_entries,
             content_sha256
         ) VALUES (1, 'checkpoint', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            checkpoint_id.as_str(),
            state
                .status
                .checkpoint_supersedes_id
                .as_ref()
                .map(CheckpointId::as_str),
            replaces.as_ref().map(|cursor| cursor.epoch_id.as_str()),
            replaces
                .as_ref()
                .map(|cursor| to_sqlite_integer(cursor.revision, "replaces_revision"))
                .transpose()?,
            to_sqlite_integer(state.status.local_revision, "target_revision")?,
            to_sqlite_integer(observed_at, "state_observed_at_ms")?,
            to_sqlite_integer(entry_count, "expected_entries")?,
            to_sqlite_integer(
                limits.checkpoint_chunk_entries as u64,
                "checkpoint_chunk_entries",
            )?,
            &content_sha256,
        ],
    )?;
    transaction.execute(
        "INSERT INTO source_replica_frozen_checkpoint (
             entry_index, txid, vsize, fee_sats, entered_at_ms
         )
         SELECT ROW_NUMBER() OVER (ORDER BY txid) - 1,
                txid, vsize, fee_sats, entered_at_ms
         FROM source_replica_membership
         ORDER BY txid",
        [],
    )?;
    transaction.execute(
        "UPDATE source_replica_state SET checkpoint_required = 0 WHERE singleton = 1",
        [],
    )?;

    let begin = CheckpointBegin {
        checkpoint_id,
        supersedes_checkpoint_id: state.status.checkpoint_supersedes_id.clone(),
        replaces,
        target_revision: state.status.local_revision,
        state_observed_at_ms: observed_at,
        expected_entries: entry_count,
        expected_chunks,
        content_sha256,
    };
    begin.validate()?;
    Ok(begin)
}

fn load_dirty_mutations(
    connection: &Connection,
) -> Result<Vec<StateMutation>, SourceReplicaStoreError> {
    let mut statement = connection.prepare(
        "SELECT d.txid, m.vsize, m.fee_sats, m.entered_at_ms
         FROM source_replica_dirty AS d
         LEFT JOIN source_replica_membership AS m ON m.txid = d.txid
         ORDER BY d.txid",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<i64>>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<i64>>(3)?,
        ))
    })?;
    rows.map(|row| decode_mutation(row?))
        .collect::<Result<Vec<_>, _>>()
}

fn load_frozen_delta(
    connection: &Connection,
) -> Result<Vec<StateMutation>, SourceReplicaStoreError> {
    let mut statement = connection.prepare(
        "SELECT txid, vsize, fee_sats, entered_at_ms
         FROM source_replica_frozen_delta
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
    rows.map(|row| decode_mutation(row?))
        .collect::<Result<Vec<_>, _>>()
}

fn decode_mutation(
    row: (String, Option<i64>, Option<i64>, Option<i64>),
) -> Result<StateMutation, SourceReplicaStoreError> {
    let (txid, vsize, fee_sats, entered_at_ms) = row;
    match (vsize, fee_sats, entered_at_ms) {
        (None, None, None) => Ok(StateMutation::Absent { txid }),
        (Some(vsize), Some(fee_sats), Some(entered_at_ms)) => Ok(StateMutation::Present {
            txid,
            facts: MempoolEntryFacts {
                vsize: from_sqlite_integer(vsize, "vsize")?,
                fee_sats: from_sqlite_integer(fee_sats, "fee_sats")?,
                entered_at_ms: from_sqlite_integer(entered_at_ms, "entered_at_ms")?,
            },
        }),
        _ => Err(SourceReplicaStoreError::StoredInvariant(
            "a stored mutation contains incomplete facts",
        )),
    }
}

fn load_checkpoint_entries_page(
    connection: &Connection,
    start: u64,
    limit: usize,
) -> Result<Vec<SourceReplicaEntry>, SourceReplicaStoreError> {
    let end = start
        .checked_add(u64::try_from(limit).map_err(|_| {
            SourceReplicaStoreError::NumericOverflow {
                field: "checkpoint_chunk_entries",
            }
        })?)
        .ok_or(SourceReplicaStoreError::NumericOverflow {
            field: "checkpoint_chunk_end",
        })?;
    let mut statement = connection.prepare(
        "SELECT txid, vsize, fee_sats, entered_at_ms
         FROM source_replica_frozen_checkpoint
         WHERE entry_index >= ?1 AND entry_index < ?2
         ORDER BY entry_index",
    )?;
    let rows = statement.query_map(
        params![
            to_sqlite_integer(start, "checkpoint_chunk_start")?,
            to_sqlite_integer(end, "checkpoint_chunk_end")?,
        ],
        decode_entry_row,
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn decode_entry_row(row: &rusqlite::Row<'_>) -> Result<SourceReplicaEntry, rusqlite::Error> {
    let vsize = row.get::<_, i64>(1)?;
    let fee_sats = row.get::<_, i64>(2)?;
    let entered_at_ms = row.get::<_, i64>(3)?;
    let vsize = u64::try_from(vsize)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let fee_sats = u64::try_from(fee_sats)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let entered_at_ms = u64::try_from(entered_at_ms)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    Ok(SourceReplicaEntry {
        txid: row.get(0)?,
        facts: MempoolEntryFacts {
            vsize,
            fee_sats,
            entered_at_ms,
        },
    })
}

fn checkpoint_chunk_count(
    entry_count: u64,
    chunk_entries: usize,
) -> Result<u32, SourceReplicaStoreError> {
    if entry_count == 0 {
        return Ok(0);
    }
    let chunk_entries =
        u64::try_from(chunk_entries).map_err(|_| SourceReplicaStoreError::NumericOverflow {
            field: "checkpoint_chunk_entries",
        })?;
    let chunks = entry_count.div_ceil(chunk_entries);
    let chunks = u32::try_from(chunks).map_err(|_| SourceReplicaStoreError::NumericOverflow {
        field: "checkpoint_chunks",
    })?;
    if chunks > MAX_CHECKPOINT_CHUNKS {
        return Err(SourceReplicaStoreError::InvalidLimit {
            field: "checkpoint_chunks",
            value: u64::from(chunks),
            maximum: u64::from(MAX_CHECKPOINT_CHUNKS),
        });
    }
    Ok(chunks)
}

fn dirty_pressure(connection: &Connection) -> Result<(u64, u64), SourceReplicaStoreError> {
    let (rows, bytes) = connection.query_row(
        "SELECT COUNT(*), COALESCE(SUM(estimated_bytes), 0)
         FROM source_replica_dirty",
        [],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    Ok((
        from_sqlite_integer(rows, "dirty_rows")?,
        from_sqlite_integer(bytes, "dirty_estimated_bytes")?,
    ))
}

fn row_count(connection: &Connection, table: &'static str) -> Result<u64, SourceReplicaStoreError> {
    let sql = match table {
        "source_replica_membership" => "SELECT COUNT(*) FROM source_replica_membership",
        "source_replica_dirty" => "SELECT COUNT(*) FROM source_replica_dirty",
        "source_replica_frozen_delta" => "SELECT COUNT(*) FROM source_replica_frozen_delta",
        "source_replica_frozen_checkpoint" => {
            "SELECT COUNT(*) FROM source_replica_frozen_checkpoint"
        }
        _ => {
            return Err(SourceReplicaStoreError::StoredInvariant(
                "unknown count table",
            ));
        }
    };
    let count = connection.query_row(sql, [], |row| row.get::<_, i64>(0))?;
    from_sqlite_integer(count, "row_count")
}

fn rebase_surviving_dirty(
    transaction: &Transaction<'_>,
    frozen: &FrozenAction,
) -> Result<(), SourceReplicaStoreError> {
    let mut statement = transaction.prepare(
        "SELECT txid, dirty_revision
         FROM source_replica_dirty
         WHERE dirty_revision > ?1
         ORDER BY txid",
    )?;
    let rows = statement.query_map(
        [to_sqlite_integer(
            frozen.target_revision,
            "target_revision",
        )?],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
    )?;
    let pending = rows.collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    for (txid, dirty_revision) in pending {
        let Some(delivered) = frozen_membership_for_txid(transaction, frozen, &txid)? else {
            continue;
        };
        let current = current_membership_for_txid(transaction, &txid)?;
        record_dirty_divergence(
            transaction,
            &txid,
            delivered.as_ref(),
            current.as_ref(),
            from_sqlite_integer(dirty_revision, "dirty_revision")?,
        )?;
    }
    Ok(())
}

fn current_membership_for_txid(
    connection: &Connection,
    txid: &str,
) -> Result<Option<MempoolEntryFacts>, SourceReplicaStoreError> {
    let stored = connection
        .query_row(
            "SELECT vsize, fee_sats, entered_at_ms
             FROM source_replica_membership
             WHERE txid = ?1",
            [txid],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    stored
        .map(|(vsize, fee_sats, entered_at_ms)| {
            Ok(MempoolEntryFacts {
                vsize: from_sqlite_integer(vsize, "vsize")?,
                fee_sats: from_sqlite_integer(fee_sats, "fee_sats")?,
                entered_at_ms: from_sqlite_integer(entered_at_ms, "entered_at_ms")?,
            })
        })
        .transpose()
}

fn clear_frozen_action(transaction: &Transaction<'_>) -> Result<(), SourceReplicaStoreError> {
    transaction.execute("DELETE FROM source_replica_frozen_delta", [])?;
    transaction.execute("DELETE FROM source_replica_frozen_checkpoint", [])?;
    transaction.execute("DELETE FROM source_replica_frozen_action", [])?;
    Ok(())
}

fn stored_bool(value: i64, field: &'static str) -> Result<bool, SourceReplicaStoreError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(SourceReplicaStoreError::StoredInvariant(match field {
            "has_acknowledged_cursor" => "invalid acknowledged-cursor flag",
            "has_observed_snapshot" => "invalid observed-snapshot flag",
            "checkpoint_required" => "invalid checkpoint-required flag",
            _ => "invalid boolean flag",
        })),
    }
}

fn optional_stored_u64(
    value: Option<i64>,
    field: &'static str,
) -> Result<Option<u64>, SourceReplicaStoreError> {
    value
        .map(|value| from_sqlite_integer(value, field))
        .transpose()
}

fn to_sqlite_integer(value: u64, field: &'static str) -> Result<i64, SourceReplicaStoreError> {
    i64::try_from(value).map_err(|_| SourceReplicaStoreError::NumericOverflow { field })
}

fn from_sqlite_integer(value: i64, field: &'static str) -> Result<u64, SourceReplicaStoreError> {
    u64::try_from(value).map_err(|_| SourceReplicaStoreError::NegativeStoredInteger { field })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::schema;

    fn source() -> SourceId {
        SourceId::new("core-a").expect("source")
    }

    fn txid(number: u64) -> String {
        format!("{number:064x}")
    }

    fn facts(seed: u64) -> MempoolEntryFacts {
        MempoolEntryFacts {
            vsize: 100 + seed,
            fee_sats: 1_000 + seed,
            entered_at_ms: 10_000 + seed,
        }
    }

    fn snapshot(entries: &[(u64, u64)]) -> BTreeMap<String, MempoolEntryFacts> {
        entries
            .iter()
            .map(|(number, seed)| (txid(*number), facts(*seed)))
            .collect()
    }

    fn test_replica_with_limits(limits: SourceReplicaLimits) -> (TempDir, PathBuf, SourceReplica) {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("agent.db");
        schema::migrate(&path).expect("migrate temporary agent database");
        let replica = SourceReplica::open(&path, source(), limits).expect("open source replica");
        (temporary, path, replica)
    }

    fn test_replica() -> (TempDir, PathBuf, SourceReplica) {
        test_replica_with_limits(SourceReplicaLimits::default())
    }

    #[test]
    fn sqlite_busy_and_locked_errors_are_retryable_contention() {
        for result_code in [rusqlite::ffi::SQLITE_BUSY, rusqlite::ffi::SQLITE_LOCKED] {
            let error = rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(result_code),
                Some("test contention".to_owned()),
            );
            assert!(SourceReplicaStoreError::from(error).is_retryable_contention());
        }
    }

    fn next_checkpoint(replica: &SourceReplica) -> CheckpointBegin {
        match replica.next_action().expect("next action") {
            Some(SourceReplicaAction::Checkpoint(begin)) => begin,
            other => panic!("expected checkpoint, got {other:?}"),
        }
    }

    fn next_delta(replica: &SourceReplica) -> StateDelta {
        match replica.next_action().expect("next action") {
            Some(SourceReplicaAction::Delta(delta)) => delta,
            other => panic!("expected delta, got {other:?}"),
        }
    }

    fn cursor(replica: &SourceReplica, revision: u64) -> ReplicaCursor {
        ReplicaCursor {
            epoch_id: replica.status().expect("status").epoch_id,
            revision,
        }
    }

    fn acknowledge_checkpoint(replica: &SourceReplica, begin: &CheckpointBegin) {
        replica
            .checkpoint_commit(&begin.checkpoint_id)
            .expect("checkpoint commit");
        assert_eq!(
            replica
                .acknowledge(&cursor(replica, begin.target_revision))
                .expect("acknowledge checkpoint"),
            AcknowledgeOutcome::Applied
        );
    }

    #[test]
    fn epoch_survives_reopen_and_source_binding_is_authoritative() {
        let (_temporary, path, replica) = test_replica();
        let epoch = replica.status().expect("status").epoch_id;
        drop(replica);

        let reopened = SourceReplica::open(&path, source(), SourceReplicaLimits::default())
            .expect("reopen source replica");
        assert_eq!(reopened.status().expect("reopened status").epoch_id, epoch);
        assert!(matches!(
            SourceReplica::open(
                &path,
                SourceId::new("knots-a").expect("other source"),
                SourceReplicaLimits::default(),
            ),
            Err(SourceReplicaStoreError::SourceBinding { .. })
        ));
    }

    #[test]
    fn limits_reject_membership_that_cannot_fit_in_declared_checkpoint_chunks() {
        assert!(matches!(
            SourceReplicaLimits {
                max_membership_entries: u64::from(MAX_CHECKPOINT_CHUNKS) + 1,
                max_dirty_mutations: 1,
                max_dirty_bytes: 1,
                checkpoint_chunk_entries: 1,
                max_database_bytes: DEFAULT_MAX_DATABASE_BYTES,
            }
            .validate(),
            Err(SourceReplicaStoreError::InvalidLimit {
                field: "max_membership_entries",
                maximum,
                ..
            }) if maximum == u64::from(MAX_CHECKPOINT_CHUNKS)
        ));
    }

    #[test]
    fn connections_enforce_main_database_and_retained_wal_limits() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("agent.db");
        crate::schema::migrate(&path).expect("migrate");
        let budget = 8 * 1024 * 1024;
        let limits = SourceReplicaLimits {
            max_database_bytes: budget,
            ..SourceReplicaLimits::default()
        };
        let replica = SourceReplica::open(&path, source(), limits).expect("open replica");
        let connection = replica.connect().expect("connection");
        let page_size = connection
            .pragma_query_value(None, "page_size", |row| row.get::<_, i64>(0))
            .expect("page size") as u64;
        let max_pages = connection
            .pragma_query_value(None, "max_page_count", |row| row.get::<_, i64>(0))
            .expect("max page count") as u64;
        let journal_limit = connection
            .pragma_query_value(None, "journal_size_limit", |row| row.get::<_, i64>(0))
            .expect("journal size limit");
        let autocheckpoint = connection
            .pragma_query_value(None, "wal_autocheckpoint", |row| row.get::<_, i64>(0))
            .expect("WAL autocheckpoint");

        assert!(max_pages * page_size <= budget);
        assert!(budget - max_pages * page_size < page_size);
        assert_eq!(journal_limit, WAL_JOURNAL_SIZE_LIMIT_BYTES);
        assert_eq!(autocheckpoint, WAL_AUTOCHECKPOINT_PAGES);
    }

    #[test]
    fn opening_rejects_a_page_budget_below_existing_database_size() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("agent.db");
        crate::schema::migrate(&path).expect("migrate");
        let limits = SourceReplicaLimits {
            max_database_bytes: 1,
            ..SourceReplicaLimits::default()
        };
        assert!(matches!(
            SourceReplica::open(&path, source(), limits),
            Err(SourceReplicaStoreError::DatabaseBudgetTooSmall {
                maximum_bytes: 1,
                ..
            })
        ));
    }

    #[test]
    fn empty_baseline_is_revision_one_then_freshness_heartbeats_once() {
        let (_temporary, _path, replica) = test_replica();
        assert_eq!(replica.next_action().expect("pre-baseline action"), None);
        assert_eq!(
            replica
                .observe_rpc_snapshot(&BTreeMap::new(), 100)
                .expect("empty baseline"),
            ObserveRpcOutcome::Baseline {
                revision: 1,
                entry_count: 0,
            }
        );
        let begin = next_checkpoint(&replica);
        assert_eq!(begin.target_revision, 1);
        assert_eq!(begin.expected_entries, 0);
        assert_eq!(begin.expected_chunks, 0);
        assert!(begin.replaces.is_none());
        assert!(matches!(
            replica.checkpoint_chunk(&begin.checkpoint_id, 0),
            Err(SourceReplicaStoreError::CheckpointChunkIndex { .. })
        ));
        acknowledge_checkpoint(&replica, &begin);
        assert_eq!(replica.next_action().expect("idle after checkpoint"), None);

        assert_eq!(
            replica
                .observe_rpc_snapshot(&BTreeMap::new(), 200)
                .expect("fresh unchanged snapshot"),
            ObserveRpcOutcome::Unchanged { revision: 1 }
        );
        let heartbeat = match replica.next_action().expect("freshness action") {
            Some(SourceReplicaAction::Heartbeat(heartbeat)) => heartbeat,
            other => panic!("expected heartbeat, got {other:?}"),
        };
        let active = cursor(&replica, 1);
        assert_eq!(
            replica
                .acknowledge_heartbeat(&heartbeat, &active)
                .expect("acknowledge heartbeat"),
            AcknowledgeOutcome::Applied
        );
        assert_eq!(
            replica
                .acknowledge_heartbeat(&heartbeat, &active)
                .expect("duplicate heartbeat acknowledgement"),
            AcknowledgeOutcome::Duplicate
        );
        assert_eq!(replica.next_action().expect("idle after heartbeat"), None);

        replica
            .observe_rpc_snapshot(&BTreeMap::new(), 150)
            .expect("regressed wall clock observation");
        let status = replica.status().expect("monotonic status");
        assert_eq!(status.state_observed_at_ms, Some(200));
        assert_eq!(status.last_rpc_success_at_ms, Some(200));
        assert_eq!(replica.next_action().expect("no regressed heartbeat"), None);
    }

    #[test]
    fn frozen_checkpoint_survives_restart_while_later_changes_become_a_delta() {
        let (_temporary, path, replica) = test_replica();
        replica
            .observe_rpc_snapshot(&snapshot(&[(1, 1)]), 100)
            .expect("baseline");
        let begin = next_checkpoint(&replica);
        replica
            .observe_rpc_snapshot(&snapshot(&[(1, 1), (2, 2)]), 200)
            .expect("post-freeze change");
        let stats = replica.storage_stats().expect("storage stats");
        assert_eq!(stats.membership_rows, 2);
        assert_eq!(stats.frozen_checkpoint_rows, 1);
        assert_eq!(stats.dirty_rows, 1);
        let epoch = replica.status().expect("status").epoch_id;
        drop(replica);

        let reopened =
            SourceReplica::open(&path, source(), SourceReplicaLimits::default()).expect("reopen");
        assert_eq!(reopened.status().expect("reopened status").epoch_id, epoch);
        assert_eq!(
            reopened.next_action().expect("replayed checkpoint"),
            Some(SourceReplicaAction::Checkpoint(begin.clone()))
        );
        let chunk = reopened
            .checkpoint_chunk(&begin.checkpoint_id, 0)
            .expect("checkpoint chunk");
        assert_eq!(
            chunk
                .entries
                .iter()
                .map(|entry| &entry.txid)
                .collect::<Vec<_>>(),
            vec![&txid(1)]
        );
        acknowledge_checkpoint(&reopened, &begin);

        let delta = next_delta(&reopened);
        assert_eq!(delta.base_revision, 1);
        assert_eq!(delta.target_revision, 2);
        assert_eq!(
            delta.mutations,
            vec![StateMutation::Present {
                txid: txid(2),
                facts: facts(2),
            }]
        );
    }

    #[test]
    fn repeated_changes_coalesce_and_delta_ack_preserves_only_later_divergence() {
        let (_temporary, _path, replica) = test_replica();
        replica
            .observe_rpc_snapshot(&snapshot(&[(1, 1)]), 100)
            .expect("baseline");
        let baseline = next_checkpoint(&replica);
        acknowledge_checkpoint(&replica, &baseline);

        replica
            .observe_rpc_snapshot(&snapshot(&[(1, 2)]), 200)
            .expect("first change");
        replica
            .observe_rpc_snapshot(&snapshot(&[(1, 3)]), 300)
            .expect("coalesced change");
        assert_eq!(replica.storage_stats().expect("stats").dirty_rows, 1);
        let first = next_delta(&replica);
        assert_eq!(first.base_revision, 1);
        assert_eq!(first.target_revision, 3);
        assert_eq!(
            first.mutations,
            vec![StateMutation::Present {
                txid: txid(1),
                facts: facts(3),
            }]
        );

        replica
            .observe_rpc_snapshot(&snapshot(&[(1, 4)]), 400)
            .expect("change after frozen delta");
        assert_eq!(
            replica.next_action().expect("same frozen delta"),
            Some(SourceReplicaAction::Delta(first.clone()))
        );
        replica
            .acknowledge(&cursor(&replica, 3))
            .expect("acknowledge first delta");
        assert_eq!(
            replica.storage_stats().expect("post-ack stats").dirty_rows,
            1
        );

        let second = next_delta(&replica);
        assert_eq!(second.base_revision, 3);
        assert_eq!(second.target_revision, 4);
        assert_eq!(
            second.mutations,
            vec![StateMutation::Present {
                txid: txid(1),
                facts: facts(4),
            }]
        );
        replica
            .acknowledge(&cursor(&replica, 4))
            .expect("acknowledge second delta");
        assert_eq!(
            replica
                .acknowledge(&cursor(&replica, 4))
                .expect("duplicate delta acknowledgement"),
            AcknowledgeOutcome::Duplicate
        );
        assert!(matches!(
            replica.acknowledge(&cursor(&replica, 3)),
            Err(SourceReplicaStoreError::UnexpectedAcknowledgement { .. })
        ));
    }

    #[test]
    fn unique_enter_leave_churn_cancels_tombstones_in_constant_space() {
        let (_temporary, _path, replica) = test_replica();
        replica
            .observe_rpc_snapshot(&BTreeMap::new(), 1)
            .expect("empty baseline");
        let baseline = next_checkpoint(&replica);
        acknowledge_checkpoint(&replica, &baseline);

        for number in 1..=200 {
            replica
                .observe_rpc_snapshot(&snapshot(&[(number, number)]), number * 2)
                .expect("temporary entry");
            replica
                .observe_rpc_snapshot(&BTreeMap::new(), number * 2 + 1)
                .expect("temporary removal");
        }
        let stats = replica.storage_stats().expect("bounded churn stats");
        assert_eq!(stats.membership_rows, 0);
        assert_eq!(stats.dirty_rows, 0);
        assert_eq!(stats.frozen_delta_rows, 0);
        assert_eq!(stats.frozen_checkpoint_rows, 0);
        let checkpoint = next_checkpoint(&replica);
        assert_eq!(checkpoint.expected_entries, 0);
        assert_eq!(checkpoint.target_revision, 401);
    }

    #[test]
    fn net_zero_churn_after_a_frozen_delta_requires_a_followup_checkpoint() {
        let (_temporary, _path, replica) = test_replica();
        replica
            .observe_rpc_snapshot(&BTreeMap::new(), 1)
            .expect("empty baseline");
        let baseline = next_checkpoint(&replica);
        acknowledge_checkpoint(&replica, &baseline);

        replica
            .observe_rpc_snapshot(&snapshot(&[(1, 1)]), 2)
            .expect("delta change");
        let delta = next_delta(&replica);
        replica
            .observe_rpc_snapshot(&snapshot(&[(1, 1), (2, 2)]), 3)
            .expect("temporary post-freeze entry");
        replica
            .observe_rpc_snapshot(&snapshot(&[(1, 1)]), 4)
            .expect("temporary post-freeze removal");
        replica
            .acknowledge(&cursor(&replica, delta.target_revision))
            .expect("acknowledge frozen delta");

        let checkpoint = next_checkpoint(&replica);
        assert_eq!(checkpoint.target_revision, 4);
        assert_eq!(checkpoint.expected_entries, 1);
    }

    #[test]
    fn dirty_count_and_byte_pressure_switch_to_a_checkpoint() {
        let count_limits = SourceReplicaLimits {
            max_membership_entries: 10,
            max_dirty_mutations: 1,
            max_dirty_bytes: DEFAULT_MAX_DIRTY_BYTES,
            checkpoint_chunk_entries: 2,
            max_database_bytes: DEFAULT_MAX_DATABASE_BYTES,
        };
        let (_temporary, _path, replica) = test_replica_with_limits(count_limits);
        replica
            .observe_rpc_snapshot(&BTreeMap::new(), 1)
            .expect("baseline");
        let baseline = next_checkpoint(&replica);
        acknowledge_checkpoint(&replica, &baseline);
        let outcome = replica
            .observe_rpc_snapshot(&snapshot(&[(1, 1), (2, 2)]), 2)
            .expect("pressure snapshot");
        assert!(matches!(
            outcome,
            ObserveRpcOutcome::Changed {
                checkpoint_required: true,
                ..
            }
        ));
        assert_eq!(
            replica.storage_stats().expect("pressure stats").dirty_rows,
            0
        );
        assert_eq!(next_checkpoint(&replica).expected_entries, 2);

        let byte_limits = SourceReplicaLimits {
            max_membership_entries: 10,
            max_dirty_mutations: 10,
            max_dirty_bytes: 1,
            checkpoint_chunk_entries: 2,
            max_database_bytes: DEFAULT_MAX_DATABASE_BYTES,
        };
        let (_temporary, _path, replica) = test_replica_with_limits(byte_limits);
        replica
            .observe_rpc_snapshot(&BTreeMap::new(), 1)
            .expect("baseline");
        let baseline = next_checkpoint(&replica);
        acknowledge_checkpoint(&replica, &baseline);
        replica
            .connect()
            .expect("trigger connection")
            .execute_batch(
                "CREATE TRIGGER dirty_byte_peak_guard
                 BEFORE INSERT ON source_replica_dirty
                 WHEN COALESCE((
                     SELECT SUM(estimated_bytes) FROM source_replica_dirty
                 ), 0) + NEW.estimated_bytes > 1
                 BEGIN
                     SELECT RAISE(ABORT, 'dirty byte cap exceeded transitionally');
                 END;",
            )
            .expect("install dirty byte peak guard");
        replica
            .observe_rpc_snapshot(&snapshot(&[(1, 1)]), 2)
            .expect("byte pressure snapshot");
        assert!(replica.status().expect("status").checkpoint_required);
        assert_eq!(replica.storage_stats().expect("byte stats").dirty_rows, 0);
    }

    #[test]
    fn disjoint_snapshot_switches_before_dirty_cardinality_can_cross_its_cap() {
        let limits = SourceReplicaLimits {
            max_membership_entries: 6,
            max_dirty_mutations: 2,
            max_dirty_bytes: DEFAULT_MAX_DIRTY_BYTES,
            checkpoint_chunk_entries: 2,
            max_database_bytes: DEFAULT_MAX_DATABASE_BYTES,
        };
        let (_temporary, _path, replica) = test_replica_with_limits(limits);
        let baseline_snapshot = (1..=6)
            .map(|number| (txid(number), facts(number)))
            .collect::<BTreeMap<_, _>>();
        replica
            .observe_rpc_snapshot(&baseline_snapshot, 1)
            .expect("maximum baseline");
        let baseline = next_checkpoint(&replica);
        acknowledge_checkpoint(&replica, &baseline);
        replica
            .connect()
            .expect("trigger connection")
            .execute_batch(
                "CREATE TRIGGER dirty_row_peak_guard
                 BEFORE INSERT ON source_replica_dirty
                 WHEN (SELECT COUNT(*) FROM source_replica_dirty) >= 2
                 BEGIN
                     SELECT RAISE(ABORT, 'dirty row cap exceeded transitionally');
                 END;",
            )
            .expect("install dirty row peak guard");

        let disjoint_snapshot = (101..=106)
            .map(|number| (txid(number), facts(number)))
            .collect::<BTreeMap<_, _>>();
        let connection = replica.connect().expect("collector connection");
        let collected = collect_membership_changes(
            &connection,
            &disjoint_snapshot,
            Some(DetailedChangeBudget {
                max_changes: limits.max_dirty_mutations,
                max_bytes: limits.max_dirty_bytes,
            }),
        )
        .expect("bounded disjoint diff");
        assert_eq!(collected.mutation_count, 12);
        assert!(collected.requires_checkpoint);
        assert!(collected.detailed.is_empty());
        assert!(collected.peak_buffered_changes <= limits.max_dirty_mutations);
        assert!(collected.peak_buffered_bytes <= limits.max_dirty_bytes);
        drop(connection);
        let outcome = replica
            .observe_rpc_snapshot(&disjoint_snapshot, 2)
            .expect("disjoint snapshot transitions directly to checkpoint");
        assert!(matches!(
            outcome,
            ObserveRpcOutcome::Changed {
                mutation_count: 12,
                checkpoint_required: true,
                ..
            }
        ));
        let stats = replica.storage_stats().expect("bounded storage stats");
        assert_eq!(stats.membership_rows, 6);
        assert_eq!(stats.dirty_rows, 0);
        assert_eq!(stats.dirty_estimated_bytes, 0);
        assert_eq!(next_checkpoint(&replica).expected_entries, 6);
    }

    #[test]
    fn over_limit_snapshot_preserves_frozen_action_then_requires_followup_checkpoint() {
        let limits = SourceReplicaLimits {
            max_membership_entries: 6,
            max_dirty_mutations: 2,
            max_dirty_bytes: DEFAULT_MAX_DIRTY_BYTES,
            checkpoint_chunk_entries: 2,
            max_database_bytes: DEFAULT_MAX_DATABASE_BYTES,
        };
        let (_temporary, _path, replica) = test_replica_with_limits(limits);
        replica
            .observe_rpc_snapshot(&snapshot(&[(1, 1)]), 1)
            .expect("baseline");
        let baseline = next_checkpoint(&replica);
        acknowledge_checkpoint(&replica, &baseline);

        replica
            .observe_rpc_snapshot(&snapshot(&[(1, 1), (2, 2)]), 2)
            .expect("delta snapshot");
        let frozen_delta = next_delta(&replica);
        let disjoint_snapshot = (101..=106)
            .map(|number| (txid(number), facts(number)))
            .collect::<BTreeMap<_, _>>();
        let outcome = replica
            .observe_rpc_snapshot(&disjoint_snapshot, 3)
            .expect("over-limit snapshot");
        assert_eq!(
            outcome,
            ObserveRpcOutcome::Changed {
                revision: 3,
                mutation_count: 8,
                checkpoint_required: true,
            }
        );
        assert_eq!(
            replica.next_action().expect("frozen action survives"),
            Some(SourceReplicaAction::Delta(frozen_delta.clone()))
        );

        replica
            .acknowledge(&cursor(&replica, frozen_delta.target_revision))
            .expect("acknowledge frozen delta");
        let checkpoint = next_checkpoint(&replica);
        assert_eq!(checkpoint.target_revision, 3);
        assert_eq!(checkpoint.expected_entries, 6);
        assert_eq!(replica.storage_stats().expect("stats").dirty_rows, 0);
    }

    #[test]
    fn checkpoint_replaces_an_exact_other_epoch_cursor() {
        let (_temporary, _path, replica) = test_replica();
        replica
            .observe_rpc_snapshot(&snapshot(&[(1, 1)]), 100)
            .expect("baseline");
        let baseline = next_checkpoint(&replica);
        acknowledge_checkpoint(&replica, &baseline);
        let local_epoch = replica.status().expect("status").epoch_id;
        let remote = ReplicaCursor {
            epoch_id: SourceEpochId::new("remote-epoch").expect("remote epoch"),
            revision: 77,
        };
        replica
            .require_checkpoint_against(Some(&remote), None)
            .expect("checkpoint against remote cursor");
        let replacement = next_checkpoint(&replica);
        assert_eq!(replacement.replaces, Some(remote));
        assert_eq!(replica.status().expect("status").epoch_id, local_epoch);
        assert_eq!(replacement.target_revision, 1);
        assert_eq!(
            replica
                .acknowledge(&cursor(&replica, replacement.target_revision))
                .expect("acknowledge same-revision replacement checkpoint"),
            AcknowledgeOutcome::Applied
        );
        let status = replica.status().expect("post-replacement status");
        assert!(!status.checkpoint_replacement_known);
        let stats = replica.storage_stats().expect("post-replacement stats");
        assert_eq!(stats.frozen_checkpoint_rows, 0);
        assert_eq!(
            replica.next_action().expect("replacement is complete"),
            None
        );
    }

    #[test]
    fn central_reset_checkpoint_at_acknowledged_revision_is_applied_not_duplicated() {
        let (_temporary, _path, replica) = test_replica();
        replica
            .observe_rpc_snapshot(&snapshot(&[(1, 1)]), 100)
            .expect("baseline");
        let baseline = next_checkpoint(&replica);
        acknowledge_checkpoint(&replica, &baseline);

        replica
            .require_checkpoint_against(None, None)
            .expect("checkpoint after central reset");
        let replacement = next_checkpoint(&replica);
        assert_eq!(replacement.target_revision, 1);
        assert!(replacement.replaces.is_none());
        assert_eq!(
            replica
                .acknowledge(&cursor(&replica, replacement.target_revision))
                .expect("acknowledge reset checkpoint"),
            AcknowledgeOutcome::Applied
        );
        assert_eq!(
            replica
                .storage_stats()
                .expect("post-reset stats")
                .frozen_checkpoint_rows,
            0
        );
        assert_eq!(
            replica.next_action().expect("reset checkpoint complete"),
            None
        );
    }

    #[test]
    fn same_epoch_remote_at_local_revision_rotates_and_persists_staging_fence() {
        let (_temporary, path, replica) = test_replica();
        replica
            .observe_rpc_snapshot(&snapshot(&[(1, 1)]), 100)
            .expect("baseline");
        let baseline = next_checkpoint(&replica);
        acknowledge_checkpoint(&replica, &baseline);
        let prior_epoch = replica.status().expect("prior status").epoch_id;
        let remote = ReplicaCursor {
            epoch_id: prior_epoch.clone(),
            revision: 1,
        };
        let staging_checkpoint_id =
            CheckpointId::new("server-staging").expect("staging checkpoint");
        replica
            .require_checkpoint_against(Some(&remote), Some(&staging_checkpoint_id))
            .expect("detect non-advancing same-epoch recovery");
        let status = replica.status().expect("rotated status");
        assert_ne!(status.epoch_id, prior_epoch);
        assert_eq!(status.local_revision, 1);
        assert!(!status.has_acknowledged_cursor);
        assert_eq!(
            status.checkpoint_supersedes_id,
            Some(staging_checkpoint_id.clone())
        );
        drop(replica);

        let reopened = SourceReplica::open(&path, source(), SourceReplicaLimits::default())
            .expect("reopen source replica");
        let replacement = next_checkpoint(&reopened);
        assert_eq!(
            replacement.supersedes_checkpoint_id,
            Some(staging_checkpoint_id)
        );
        assert_eq!(replacement.replaces, Some(remote));
        assert_eq!(replacement.target_revision, 1);
        assert_eq!(replacement.expected_entries, 1);
    }

    #[test]
    fn snapshot_capacity_rejection_is_atomic() {
        let limits = SourceReplicaLimits {
            max_membership_entries: 1,
            max_dirty_mutations: 10,
            max_dirty_bytes: DEFAULT_MAX_DIRTY_BYTES,
            checkpoint_chunk_entries: 1,
            max_database_bytes: DEFAULT_MAX_DATABASE_BYTES,
        };
        let (_temporary, _path, replica) = test_replica_with_limits(limits);
        replica
            .observe_rpc_snapshot(&snapshot(&[(1, 1)]), 100)
            .expect("baseline");
        let before = replica.status().expect("status before rejection");
        assert!(matches!(
            replica.observe_rpc_snapshot(&snapshot(&[(1, 1), (2, 2)]), 200),
            Err(SourceReplicaStoreError::SnapshotTooLarge {
                found: 2,
                maximum: 1,
            })
        ));
        assert_eq!(replica.status().expect("status after rejection"), before);
        assert_eq!(replica.storage_stats().expect("stats").membership_rows, 1);
    }

    #[test]
    fn checkpoint_chunks_use_stable_ordinals_and_bounded_cardinality() {
        let limits = SourceReplicaLimits {
            max_membership_entries: 10,
            max_dirty_mutations: 10,
            max_dirty_bytes: DEFAULT_MAX_DIRTY_BYTES,
            checkpoint_chunk_entries: 2,
            max_database_bytes: DEFAULT_MAX_DATABASE_BYTES,
        };
        let (_temporary, path, replica) = test_replica_with_limits(limits);
        replica
            .observe_rpc_snapshot(&snapshot(&[(5, 5), (1, 1), (3, 3), (2, 2), (4, 4)]), 100)
            .expect("baseline");
        let begin = next_checkpoint(&replica);
        assert_eq!(begin.expected_entries, 5);
        assert_eq!(begin.expected_chunks, 3);
        let chunks = (0..3)
            .map(|index| {
                replica
                    .checkpoint_chunk(&begin.checkpoint_id, index)
                    .expect("checkpoint chunk")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            chunks
                .iter()
                .flat_map(|chunk| chunk.entries.iter().map(|entry| entry.txid.clone()))
                .collect::<Vec<_>>(),
            (1..=5).map(txid).collect::<Vec<_>>()
        );
        let stats = replica.storage_stats().expect("storage stats");
        assert_eq!(stats.membership_rows, 5);
        assert_eq!(stats.frozen_checkpoint_rows, 5);
        assert_eq!(stats.dirty_rows, 0);
        drop(replica);

        let reopened = SourceReplica::open(&path, source(), limits).expect("reopen");
        assert_eq!(
            reopened
                .checkpoint_chunk(&begin.checkpoint_id, 1)
                .expect("stable chunk"),
            chunks[1]
        );
    }
}
