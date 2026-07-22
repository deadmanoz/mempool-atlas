use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use atlas_classifiers::{
    ClassificationInput, ClassificationStatus, TransactionShape, registered_packs,
};
use atlas_model::{
    AggregateBin, CaptureGapCertainty, CaptureStatus, CheckpointId, Evidence, IngestBatchRequest,
    IngestStatus, MAX_CHECKPOINT_ENTRIES, MempoolEntry, MempoolEntryFacts, MempoolEntryFactsStatus,
    MempoolSnapshot, NormalizedEvent, ReplicaCursor, ScriptType, SourceDescriptor, SourceHealth,
    SourceId,
};
use atlas_storage::{
    SqlitePhysicalSnapshot, SqliteStorageError, SqliteStorageLimits, SqliteStorageLimitsError,
    check_write_admission, configure_sqlite_connection, relieve_wal_pressure,
    sqlite_physical_snapshot,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params, params_from_iter};
use thiserror::Error;
use tracing::warn;

use crate::comparison::{ComparisonInputs, ComparisonOutcome, SourceTotal, StageInputs};
use crate::rejections::{REJECTION_WINDOW_MAX, RejectionCursor, RejectionInputs, RejectionRow};
use crate::summary::{FactsRow, RegionFacts, ShapeRow, SourceMembershipFacts};

const LATEST_SCHEMA_VERSION: i64 = 8;
const MIGRATION_1: &str = include_str!("../migrations/0001_initial.sql");
const MIGRATION_2: &str = include_str!("../migrations/0002_source_replica.sql");
const MIB: u64 = 1024 * 1024;

/// Bounded library defaults keep tests and embedded development stores
/// independent of production filesystem sizing. The server binary always
/// supplies its explicit production defaults instead.
const DEFAULT_LIBRARY_STORAGE_LIMITS: SqliteStorageLimits = SqliteStorageLimits {
    max_database_bytes: 256 * MIB,
    max_total_sqlite_bytes: 512 * MIB,
    filesystem_reserve_bytes: 1,
    filesystem_reserve_percent: 1,
    retained_wal_high_water_bytes: 64 * MIB,
    wal_autocheckpoint_pages: 1_000,
};
const DEFAULT_MAX_SOURCE_IDENTITIES: usize = 4;
const DEFAULT_MAX_MEMBERSHIP_ENTRIES: u64 = 200_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreLimits {
    pub sqlite: SqliteStorageLimits,
    /// Persistent source identities admitted to this store. SourceReplica has
    /// no source-retirement operation, so this is not a concurrency limit.
    pub max_sources: usize,
    pub max_membership_entries: u64,
    pub allowed_source_ids: Option<BTreeSet<SourceId>>,
}

impl StoreLimits {
    pub fn new(
        sqlite: SqliteStorageLimits,
        max_sources: usize,
        max_membership_entries: u64,
        allowed_source_ids: Option<BTreeSet<SourceId>>,
    ) -> Result<Self, StoreLimitsError> {
        let limits = Self {
            sqlite,
            max_sources,
            max_membership_entries,
            allowed_source_ids,
        };
        limits.validate()?;
        Ok(limits)
    }

    pub fn validate(&self) -> Result<(), StoreLimitsError> {
        self.sqlite.validate()?;
        if self.max_sources == 0 {
            return Err(StoreLimitsError::ZeroMaxSources);
        }
        if self.max_membership_entries == 0 || self.max_membership_entries > MAX_CHECKPOINT_ENTRIES
        {
            return Err(StoreLimitsError::MembershipLimitOutOfRange {
                found: self.max_membership_entries,
                maximum: MAX_CHECKPOINT_ENTRIES,
            });
        }
        if let Some(allowed) = &self.allowed_source_ids
            && allowed.len() > self.max_sources
        {
            return Err(StoreLimitsError::AllowlistExceedsPersistentSourceLimit {
                found: allowed.len(),
                maximum: self.max_sources,
            });
        }
        Ok(())
    }
}

impl Default for StoreLimits {
    fn default() -> Self {
        Self {
            sqlite: DEFAULT_LIBRARY_STORAGE_LIMITS,
            max_sources: DEFAULT_MAX_SOURCE_IDENTITIES,
            max_membership_entries: DEFAULT_MAX_MEMBERSHIP_ENTRIES,
            allowed_source_ids: None,
        }
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum StoreLimitsError {
    #[error(transparent)]
    Storage(#[from] SqliteStorageLimitsError),
    #[error("max_sources persistent identity cap must be greater than zero")]
    ZeroMaxSources,
    #[error("max_membership_entries must be in 1..={maximum}; found {found}")]
    MembershipLimitOutOfRange { found: u64, maximum: u64 },
    #[error(
        "source allowlist contains {found} persistent identities, exceeding max_sources {maximum}"
    )]
    AllowlistExceedsPersistentSourceLimit { found: usize, maximum: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreReadiness {
    pub snapshot: SqlitePhysicalSnapshot,
    pub pressure: Option<String>,
}

impl StoreReadiness {
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.pressure.is_none()
    }
}

#[derive(Clone, Debug)]
pub struct Store {
    path: Arc<PathBuf>,
    limits: Arc<StoreLimits>,
    write_gate: Arc<Mutex<()>>,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("storage policy error: {0}")]
    Storage(#[from] SqliteStorageError),
    #[error("invalid store limits: {0}")]
    Limits(#[from] StoreLimitsError),
    #[error("event model error: {0}")]
    Model(#[from] atlas_model::ModelError),
    #[error("source replica model error: {0}")]
    SourceReplicaModel(#[from] atlas_model::SourceReplicaError),
    #[error("event serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("database schema is at version {found}, expected {expected}")]
    SchemaVersion { found: i64, expected: i64 },
    #[error(
        "database contains {found} persistent source identities, exceeding configured maximum {maximum}"
    )]
    ExistingSourceIdentityLimitExceeded { found: u64, maximum: usize },
    #[error("database source {source_id} is absent from the configured source allowlist")]
    ExistingSourceNotAllowed { source_id: SourceId },
    #[error(
        "database generation {source_id}/{generation_id} declares {declared_entries} and contains {actual_entries} membership entries, exceeding configured maximum {maximum}"
    )]
    ExistingGenerationMembershipLimitExceeded {
        source_id: SourceId,
        generation_id: i64,
        declared_entries: u64,
        actual_entries: u64,
        maximum: u64,
    },
    #[error("numeric field {field} is too large for SQLite")]
    NumericOverflow { field: &'static str },
    #[error("invalid raw transaction hex for event {event_id}")]
    InvalidRawTransaction { event_id: String },
    #[error("event {event_id} conflicts with an already ingested event")]
    ConflictingEvent { event_id: String },
    #[error("{code}: {message}")]
    SourceReplicaConflict {
        code: &'static str,
        message: String,
        active_cursor: Option<ReplicaCursor>,
        staging_checkpoint_id: Option<CheckpointId>,
    },
    #[error("capacity_exceeded: {message}")]
    SourceReplicaCapacity {
        message: String,
        active_cursor: Option<ReplicaCursor>,
    },
    #[error("invalid_state_request: {message}")]
    InvalidSourceReplica {
        message: String,
        active_cursor: Option<ReplicaCursor>,
    },
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_with_limits(path, StoreLimits::default())
    }

    pub fn open_with_limits(
        path: impl AsRef<Path>,
        limits: StoreLimits,
    ) -> Result<Self, StoreError> {
        limits.validate()?;
        let path = path.as_ref().to_path_buf();
        let connection = open_existing_connection(&path, &limits.sqlite)?;
        let version = schema_version(&connection)?;
        if version != LATEST_SCHEMA_VERSION {
            return Err(StoreError::SchemaVersion {
                found: version,
                expected: LATEST_SCHEMA_VERSION,
            });
        }
        validate_existing_store_policy(&connection, &limits)?;
        Ok(Self {
            path: Arc::new(path),
            limits: Arc::new(limits),
            write_gate: Arc::new(Mutex::new(())),
        })
    }

    pub fn migrate(path: impl AsRef<Path>) -> Result<(), StoreError> {
        Self::migrate_with_limits(path, &StoreLimits::default().sqlite)
    }

    pub fn migrate_with_limits(
        path: impl AsRef<Path>,
        limits: &SqliteStorageLimits,
    ) -> Result<(), StoreError> {
        limits.validate().map_err(StoreLimitsError::from)?;
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut connection = Connection::open(path)?;
        configure_connection(&connection, limits)?;
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
        transaction.execute_batch(MIGRATION_2)?;
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
        let _write_guard = self.lock_write_gate();
        // This router is explicitly experimental. Its legacy reducer upserts
        // source freshness before it can resolve an event replay, so the
        // batch is admitted before reduction. SourceReplica production paths
        // place admission after duplicate and conflict resolution instead.
        self.attempt_wal_relief();
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        self.require_write_admission(&transaction, None)?;
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
        let Some(health) = query_source_health(&transaction, source_id)? else {
            transaction.commit()?;
            return Ok(None);
        };

        let memberships = {
            let mut statement = transaction.prepare(
                "SELECT txid, vsize, fee_sats, entered_at_ms
                 FROM active_source_replica_membership
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

    /// Reads only what the aggregate summary needs: source health and the fact
    /// triples of active memberships. The legacy awaiting-RPC count is always
    /// zero because SourceReplica membership comes only from complete RPC
    /// observations. The
    /// fact-row and verdict gathering is shared with the comparison region
    /// paths through [`gather_region_facts`].
    pub fn mempool_facts(
        &self,
        source_id: &SourceId,
    ) -> Result<Option<SourceMembershipFacts>, StoreError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        let Some(health) = query_source_health(&transaction, source_id)? else {
            transaction.commit()?;
            return Ok(None);
        };
        let region = gather_region_facts(&transaction, &RegionSpec::whole(source_id.as_str()))?;
        transaction.commit()?;
        Ok(Some(SourceMembershipFacts {
            health,
            available: region.available,
            awaiting_rpc_count: region.awaiting_rpc_count,
        }))
    }

    /// Lists every known source with its current membership count, ordered by
    /// source ID for stable discovery responses.
    pub fn sources(&self) -> Result<Vec<SourceDescriptor>, StoreError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT replica.source_id, replica.epoch_id, replica.revision,
                    replica.state_observed_at_ms, COUNT(membership.txid)
             FROM active_source_replica AS replica
             LEFT JOIN active_source_replica_membership AS membership
               ON membership.source_id = replica.source_id
              AND membership.generation_id = replica.generation_id
             GROUP BY replica.source_id, replica.epoch_id, replica.revision,
                      replica.state_observed_at_ms
             ORDER BY replica.source_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                nonnegative_integer_from_row(row.get(2)?, 2)?,
                nonnegative_integer_from_row(row.get(3)?, 3)?,
                nonnegative_integer_from_row(row.get(4)?, 4)?,
            ))
        })?;
        let mut sources = Vec::new();
        for row in rows {
            let (source_id, epoch_id, revision, state_observed_at_ms, membership_count) = row?;
            sources.push(SourceDescriptor {
                source_id: SourceId::new(source_id)?,
                state_cursor: ReplicaCursor::new(
                    atlas_model::SourceEpochId::new(epoch_id)?,
                    revision,
                )?,
                state_observed_at_ms,
                membership_count,
            });
        }
        Ok(sources)
    }

    /// Returns whether one reader-visible SourceReplica is active for a
    /// source. Evidence-ledger rows alone do not establish a known source.
    pub fn active_source_exists(&self, source_id: &SourceId) -> Result<bool, StoreError> {
        let connection = self.connect()?;
        Ok(connection.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM active_source_replica WHERE source_id = ?1
             )",
            [source_id.as_str()],
            |row| row.get(0),
        )?)
    }

    /// Reads the rejection read model inputs for one source: the most recent
    /// [`REJECTION_WINDOW_MAX`] rejections for the bounded aggregate, plus the
    /// requested recent page of the same stream. Returns `None` when the
    /// source is unknown, mirroring [`Store::mempool_facts`]. Both slices are
    /// ordered newest first by `(observed_at_ms, event_id)` descending, with
    /// each rejection's stored classifier verdicts folded onto it.
    pub fn rejections(
        &self,
        source_id: &SourceId,
        limit: usize,
        before: Option<RejectionCursor>,
    ) -> Result<Option<RejectionInputs>, StoreError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        let known = transaction.query_row(
            "SELECT EXISTS (
                SELECT 1 FROM active_source_replica WHERE source_id = ?1
             )",
            [source_id.as_str()],
            |row| row.get::<_, bool>(0),
        )?;
        if !known {
            transaction.commit()?;
            return Ok(None);
        }

        let window = {
            let mut statement = transaction.prepare(REJECTION_PAGE_SQL)?;
            let window_limit = to_sqlite_integer(REJECTION_WINDOW_MAX as u64, "window")?;
            let rows = statement.query(params![source_id.as_str(), window_limit])?;
            fold_rejection_rows(rows)?
        };

        // One extra row peeks past the page so `next_cursor` is set only when a
        // further rejection actually exists.
        let peek = to_sqlite_integer(limit.saturating_add(1) as u64, "limit")?;
        let mut recent = match &before {
            None => {
                let mut statement = transaction.prepare(REJECTION_PAGE_SQL)?;
                let rows = statement.query(params![source_id.as_str(), peek])?;
                fold_rejection_rows(rows)?
            }
            Some(cursor) => {
                let cursor_observed_at_ms =
                    to_sqlite_integer(cursor.observed_at_ms, "cursor_observed_at_ms")?;
                let mut statement = transaction.prepare(REJECTION_PAGE_BEFORE_SQL)?;
                let rows = statement.query(params![
                    source_id.as_str(),
                    cursor_observed_at_ms,
                    cursor.event_id,
                    peek,
                ])?;
                fold_rejection_rows(rows)?
            }
        };
        transaction.commit()?;

        let next_cursor = if recent.len() > limit {
            recent.truncate(limit);
            recent.last().map(|row| RejectionCursor {
                observed_at_ms: row.observed_at_ms,
                event_id: row.event_id.clone(),
            })
        } else {
            None
        };

        Ok(Some(RejectionInputs {
            window,
            recent,
            next_cursor,
        }))
    }

    /// Gathers the read-time comparison inputs for two-to-four sources in the
    /// caller's order. Returns
    /// [`ComparisonOutcome::UnknownSource`] naming the first requested source
    /// that does not exist, mirroring the not-found contract of the
    /// single-source read paths. All set math is SQL over the
    /// active SourceReplica memberships; only region aggregates and
    /// per-source totals are gathered, never a combined cross-source mempool.
    pub fn source_comparison(&self, sources: &[SourceId]) -> Result<ComparisonOutcome, StoreError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;

        for source in sources {
            let known = transaction.query_row(
                "SELECT EXISTS (
                    SELECT 1 FROM active_source_replica WHERE source_id = ?1
                 )",
                [source.as_str()],
                |row| row.get::<_, bool>(0),
            )?;
            if !known {
                transaction.commit()?;
                return Ok(ComparisonOutcome::UnknownSource(source.clone()));
            }
        }

        let mut source_totals = Vec::with_capacity(sources.len());
        for source in sources {
            source_totals.push(source_total(&transaction, source.as_str())?);
        }

        // The shared region is the intersection of every source; its facts come
        // from the last source, per the documented rule.
        let last = sources
            .last()
            .expect("the comparison requires at least two sources");
        let others: Vec<&str> = sources[..sources.len() - 1]
            .iter()
            .map(SourceId::as_str)
            .collect();
        let shared = gather_region_facts(
            &transaction,
            &RegionSpec {
                facts_source: last.as_str(),
                present_in: &others,
                absent_from: None,
            },
        )?;

        let mut stages = Vec::with_capacity(sources.len().saturating_sub(1));
        for pair in sources.windows(2) {
            let (from, to) = (&pair[0], &pair[1]);
            // `added`: present in `to`, absent from `from`.
            let added = gather_region_facts(
                &transaction,
                &RegionSpec {
                    facts_source: to.as_str(),
                    present_in: &[],
                    absent_from: Some(from.as_str()),
                },
            )?;
            // `anomaly`: present in `from`, absent from `to`.
            let anomaly = gather_region_facts(
                &transaction,
                &RegionSpec {
                    facts_source: from.as_str(),
                    present_in: &[],
                    absent_from: Some(to.as_str()),
                },
            )?;
            stages.push(StageInputs {
                from: from.clone(),
                to: to.clone(),
                added,
                anomaly,
            });
        }

        transaction.commit()?;
        Ok(ComparisonOutcome::Computed(ComparisonInputs {
            source_totals,
            shared,
            stages,
        }))
    }

    pub(crate) fn connect(&self) -> Result<Connection, StoreError> {
        open_existing_connection(&self.path, &self.limits.sqlite)
    }

    pub(crate) fn lock_write_gate(&self) -> MutexGuard<'_, ()> {
        self.write_gate.lock().unwrap_or_else(|poisoned| {
            warn!(
                "server write gate was poisoned; recovering after the prior transaction rollback"
            );
            poisoned.into_inner()
        })
    }

    /// Best-effort WAL relief runs only on an autocommit connection. A pinned
    /// reader or other pressure does not hide idempotent replies: the command
    /// still enters its transaction and the first actual mutation performs
    /// the authoritative admission check.
    pub(crate) fn attempt_wal_relief(&self) {
        let result = self.connect().and_then(|connection| {
            relieve_wal_pressure(&connection, self.path.as_ref(), &self.limits.sqlite)
                .map(|_| ())
                .map_err(StoreError::from)
        });
        if let Err(error) = result {
            warn!(%error, database = %self.path.display(), "SQLite WAL relief was unavailable");
        }
    }

    pub(crate) fn require_write_admission(
        &self,
        connection: &Connection,
        active_cursor: Option<ReplicaCursor>,
    ) -> Result<SqlitePhysicalSnapshot, StoreError> {
        check_write_admission(connection, self.path.as_ref(), &self.limits.sqlite).map_err(
            |error| {
                warn!(
                    %error,
                    database = %self.path.display(),
                    ?active_cursor,
                    "SQLite write admission rejected"
                );
                if storage_error_is_capacity(&error) {
                    StoreError::SourceReplicaCapacity {
                        message: format!("physical storage admission rejected: {error}"),
                        active_cursor,
                    }
                } else {
                    StoreError::Storage(error)
                }
            },
        )
    }

    pub(crate) fn require_source_allowed(
        &self,
        source_id: &SourceId,
        active_cursor: Option<ReplicaCursor>,
    ) -> Result<(), StoreError> {
        if let Some(allowed) = &self.limits.allowed_source_ids
            && !allowed.contains(source_id)
        {
            return Err(StoreError::SourceReplicaCapacity {
                message: format!(
                    "source {} is absent from the configured source allowlist",
                    source_id
                ),
                active_cursor,
            });
        }
        Ok(())
    }

    /// Admit a genuinely new persistent source identity. SourceReplica has no
    /// source-retirement command, so active and staging generations both count
    /// against the lifetime identity cap.
    pub(crate) fn require_new_source_identity_admission(
        &self,
        connection: &Connection,
        source_id: &SourceId,
    ) -> Result<(), StoreError> {
        let source_count = distinct_source_count(connection)?;
        if source_count >= self.limits.max_sources as u64 {
            return Err(StoreError::SourceReplicaCapacity {
                message: format!(
                    "creating source {} would exceed the configured maximum of {} persistent source identities",
                    source_id, self.limits.max_sources
                ),
                active_cursor: None,
            });
        }
        Ok(())
    }

    pub fn readiness(&self) -> Result<StoreReadiness, StoreError> {
        let _write_guard = self.lock_write_gate();
        let connection = self.connect()?;
        let relief_error =
            relieve_wal_pressure(&connection, self.path.as_ref(), &self.limits.sqlite).err();
        let snapshot =
            sqlite_physical_snapshot(&connection, self.path.as_ref(), &self.limits.sqlite)?;
        let pressure = relief_error
            .or_else(|| {
                check_write_admission(&connection, self.path.as_ref(), &self.limits.sqlite).err()
            })
            .map(|error| error.to_string());
        Ok(StoreReadiness { snapshot, pressure })
    }

    #[must_use]
    pub fn limits(&self) -> &StoreLimits {
        &self.limits
    }
}

/// The txid-membership predicate that defines a comparison region within one
/// designated facts source. `present_in` names sources the txid must also be a
/// member of (the intersection for the shared region); `absent_from` names a
/// source the txid must not be a member of (the difference for an added or
/// anomaly region). The whole-membership case leaves both empty.
struct RegionSpec<'a> {
    facts_source: &'a str,
    present_in: &'a [&'a str],
    absent_from: Option<&'a str>,
}

impl<'a> RegionSpec<'a> {
    /// The whole current membership of one source: no set restriction.
    fn whole(source: &'a str) -> Self {
        Self {
            facts_source: source,
            present_in: &[],
            absent_from: None,
        }
    }

    /// Builds the shared `WHERE` fragment over the `cm` alias and its bound
    /// parameters. The facts source binds as `?1`; each additional source binds
    /// in order after it, so the fragment is reused verbatim for both the
    /// fact-row and the verdict query.
    fn where_clause(&self) -> (String, Vec<&'a str>) {
        let mut clause = String::from("cm.source_id = ?1");
        let mut params: Vec<&'a str> = vec![self.facts_source];
        for source in self.present_in {
            params.push(source);
            let index = params.len();
            clause.push_str(&format!(
                " AND cm.txid IN \
                 (SELECT txid FROM active_source_replica_membership WHERE source_id = ?{index})"
            ));
        }
        if let Some(source) = self.absent_from {
            params.push(source);
            let index = params.len();
            clause.push_str(&format!(
                " AND cm.txid NOT IN \
                 (SELECT txid FROM active_source_replica_membership WHERE source_id = ?{index})"
            ));
        }
        (clause, params)
    }
}

/// Gathers the fact-bearing rows of one region. SourceReplica has complete RPC
/// facts for every membership, so the legacy awaiting-RPC count remains zero.
/// The designated facts source's active rows are restricted by `spec`,
/// each joined to its intrinsic shape facts and its stored classifier verdicts
/// folded on. Shared by [`Store::mempool_facts`] (whole membership) and the
/// comparison region paths so the fact-row and verdict SQL lives in one place.
fn gather_region_facts(
    transaction: &Transaction<'_>,
    spec: &RegionSpec<'_>,
) -> Result<RegionFacts, StoreError> {
    let (where_clause, params) = spec.where_clause();

    let mut available = Vec::new();
    let mut awaiting_rpc_count = 0;
    // Correlates verdict rows onto fact rows by txid internally; the txid
    // itself never leaves this function.
    let mut row_index_by_txid = std::collections::HashMap::new();
    {
        let facts_sql = format!(
            "SELECT cm.vsize, cm.fee_sats, cm.entered_at_ms,
                    transaction_shape.total_output_sats, transaction_shape.input_count,
                    transaction_shape.output_count, transaction_shape.script_type,
                    cm.txid
             FROM active_source_replica_membership AS cm
             LEFT JOIN transaction_shape ON transaction_shape.txid = cm.txid
             WHERE {where_clause}"
        );
        let mut statement = transaction.prepare(&facts_sql)?;
        let mut rows = statement.query(params_from_iter(params.iter()))?;
        while let Some(row) = rows.next()? {
            match facts_row_from_row(row)? {
                Some(facts) => {
                    row_index_by_txid.insert(row.get::<_, String>(7)?, available.len());
                    available.push(facts);
                }
                None => awaiting_rpc_count += 1,
            }
        }
    }
    {
        let verdicts_sql = format!(
            "SELECT cm.txid, transaction_classification.taxonomy,
                    transaction_classification.verdict
             FROM active_source_replica_membership AS cm
             JOIN transaction_classification
                 ON transaction_classification.txid = cm.txid
             WHERE {where_clause}"
        );
        let mut statement = transaction.prepare(&verdicts_sql)?;
        let mut rows = statement.query(params_from_iter(params.iter()))?;
        while let Some(row) = rows.next()? {
            let txid = row.get::<_, String>(0)?;
            if let Some(&index) = row_index_by_txid.get(&txid) {
                available[index]
                    .verdicts
                    .push((row.get::<_, String>(1)?, row.get::<_, String>(2)?));
            }
        }
    }
    Ok(RegionFacts {
        available,
        awaiting_rpc_count,
    })
}

/// One source's whole active-membership totals. SourceReplica members always
/// carry RPC facts, so the legacy awaiting-RPC count is zero.
fn source_total(transaction: &Transaction<'_>, source_id: &str) -> Result<SourceTotal, StoreError> {
    transaction
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(vsize), 0)
             FROM active_source_replica_membership
             WHERE source_id = ?1",
            [source_id],
            |row| {
                Ok(SourceTotal {
                    present: AggregateBin {
                        count: nonnegative_integer_from_row(row.get(0)?, 0)?,
                        vsize: nonnegative_integer_from_row(row.get(1)?, 1)?,
                    },
                    awaiting_rpc_count: 0,
                })
            },
        )
        .map_err(StoreError::from)
}

/// Reads one source's health, or `None` when the source is unknown. Shared by
/// the membership and summary read paths.
fn query_source_health(
    transaction: &Transaction<'_>,
    source_id: &SourceId,
) -> Result<Option<SourceHealth>, StoreError> {
    Ok(transaction
        .query_row(
            "SELECT epoch_id, revision, state_observed_at_ms
             FROM active_source_replica
             WHERE source_id = ?1",
            [source_id.as_str()],
            source_health_from_row,
        )
        .optional()?)
}

/// Selects a newest-first page of rejection events for a source and folds each
/// event's classification rows onto it. The `event_kind = 'mempool_rejected'`
/// literal keeps the partial `event_rejection_idx` applicable, so the scan is
/// bounded by the page rather than the full event log. Removals, replacements,
/// and every other event kind are excluded, so a rejection is never conflated
/// with a removal.
const REJECTION_PAGE_SQL: &str = "\
WITH page AS (
    SELECT event_id, observed_at_ms,
           json_extract(payload_json, '$.txid') AS txid,
           json_extract(payload_json, '$.reason') AS reason
    FROM event
    WHERE source_id = ?1 AND event_kind = 'mempool_rejected'
    ORDER BY observed_at_ms DESC, event_id DESC
    LIMIT ?2
)
SELECT page.event_id, page.observed_at_ms, page.txid, page.reason,
       classification.taxonomy, classification.verdict
FROM page
LEFT JOIN transaction_classification AS classification
    ON classification.txid = page.txid
ORDER BY page.observed_at_ms DESC, page.event_id DESC, classification.taxonomy";

/// The paginated variant of [`REJECTION_PAGE_SQL`]: only rejections strictly
/// older than the `(observed_at_ms, event_id)` cursor in the descending order.
const REJECTION_PAGE_BEFORE_SQL: &str = "\
WITH page AS (
    SELECT event_id, observed_at_ms,
           json_extract(payload_json, '$.txid') AS txid,
           json_extract(payload_json, '$.reason') AS reason
    FROM event
    WHERE source_id = ?1 AND event_kind = 'mempool_rejected'
      AND (observed_at_ms < ?2 OR (observed_at_ms = ?2 AND event_id < ?3))
    ORDER BY observed_at_ms DESC, event_id DESC
    LIMIT ?4
)
SELECT page.event_id, page.observed_at_ms, page.txid, page.reason,
       classification.taxonomy, classification.verdict
FROM page
LEFT JOIN transaction_classification AS classification
    ON classification.txid = page.txid
ORDER BY page.observed_at_ms DESC, page.event_id DESC, classification.taxonomy";

/// Folds the joined rejection rows into one [`RejectionRow`] per rejection
/// event. The outer query orders every classification row of one event
/// contiguously, so a change in `event_id` starts a new rejection; a
/// `LEFT JOIN` NULL taxonomy leaves the verdict list empty (unclassified).
fn fold_rejection_rows(mut rows: rusqlite::Rows<'_>) -> Result<Vec<RejectionRow>, StoreError> {
    let mut result: Vec<RejectionRow> = Vec::new();
    while let Some(row) = rows.next()? {
        let event_id = row.get::<_, String>(0)?;
        if result.last().map(|last| last.event_id.as_str()) != Some(event_id.as_str()) {
            result.push(RejectionRow {
                event_id,
                observed_at_ms: nonnegative_integer_from_row(row.get(1)?, 1)?,
                txid: row.get::<_, String>(2)?,
                reason: row.get::<_, String>(3)?,
                verdicts: Vec::new(),
            });
        }
        if let (Some(taxonomy), Some(verdict)) = (
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
        ) {
            result
                .last_mut()
                .expect("a rejection row was pushed for this event")
                .verdicts
                .push((taxonomy, verdict));
        }
    }
    Ok(result)
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

fn open_existing_connection(
    path: &Path,
    limits: &SqliteStorageLimits,
) -> Result<Connection, StoreError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    configure_connection(&connection, limits)?;
    Ok(connection)
}

fn configure_connection(
    connection: &Connection,
    limits: &SqliteStorageLimits,
) -> Result<(), StoreError> {
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    configure_sqlite_connection(connection, limits)?;
    Ok(())
}

fn validate_existing_store_policy(
    connection: &Connection,
    limits: &StoreLimits,
) -> Result<(), StoreError> {
    let count = distinct_source_count(connection)?;
    if count > limits.max_sources as u64 {
        return Err(StoreError::ExistingSourceIdentityLimitExceeded {
            found: count,
            maximum: limits.max_sources,
        });
    }

    if let Some(allowed) = &limits.allowed_source_ids {
        let mut statement = connection.prepare(
            "SELECT DISTINCT source_id FROM source_replica_generation ORDER BY source_id",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        for row in rows {
            let source_id = SourceId::new(row?)?;
            if !allowed.contains(&source_id) {
                return Err(StoreError::ExistingSourceNotAllowed { source_id });
            }
        }
    }

    let maximum = i64::try_from(limits.max_membership_entries)
        .expect("validated membership limit fits SQLite integer");
    let offending_generation = connection
        .query_row(
            "SELECT generation.source_id, generation.generation_id,
                    generation.expected_entries, COUNT(membership.txid)
             FROM source_replica_generation AS generation
             LEFT JOIN source_replica_membership AS membership
               ON membership.source_id = generation.source_id
              AND membership.generation_id = generation.generation_id
             GROUP BY generation.source_id, generation.generation_id,
                      generation.expected_entries
             HAVING generation.expected_entries > ?1 OR COUNT(membership.txid) > ?1
             ORDER BY generation.source_id, generation.generation_id
             LIMIT 1",
            [maximum],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    nonnegative_integer_from_row(row.get(2)?, 2)?,
                    nonnegative_integer_from_row(row.get(3)?, 3)?,
                ))
            },
        )
        .optional()?;
    if let Some((source_id, generation_id, declared_entries, actual_entries)) = offending_generation
    {
        return Err(StoreError::ExistingGenerationMembershipLimitExceeded {
            source_id: SourceId::new(source_id)?,
            generation_id,
            declared_entries,
            actual_entries,
            maximum: limits.max_membership_entries,
        });
    }
    Ok(())
}

fn distinct_source_count(connection: &Connection) -> Result<u64, StoreError> {
    connection
        .query_row(
            "SELECT COUNT(DISTINCT source_id) FROM source_replica_generation",
            [],
            |row| nonnegative_integer_from_row(row.get(0)?, 0),
        )
        .map_err(StoreError::from)
}

pub(crate) fn storage_error_is_capacity(error: &SqliteStorageError) -> bool {
    match error {
        SqliteStorageError::ExistingDatabaseExceedsPageBudget { .. }
        | SqliteStorageError::MainDatabaseLimitExceeded { .. }
        | SqliteStorageError::TotalSqliteEnvelopeExceeded { .. }
        | SqliteStorageError::WalPressure { .. }
        | SqliteStorageError::FilesystemPressure { .. } => true,
        SqliteStorageError::Sqlite(rusqlite::Error::SqliteFailure(details, _)) => {
            details.code == rusqlite::ErrorCode::DiskFull
        }
        _ => false,
    }
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

/// Derives intrinsic shape facts and one classifier verdict per registered
/// taxonomy from observed raw transaction bytes, once per txid. Bytes that
/// fail to decode or that do not match the claimed identifiers leave the
/// transaction underived rather than guessing; the evidence row itself is
/// always retained.
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
    db.execute(
        "INSERT INTO transaction_shape (
            txid, total_output_sats, input_count, output_count, script_type,
            derived_from_event_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            txid,
            to_sqlite_integer(shape.total_output_sats, "total_output_sats")?,
            to_sqlite_integer(shape.input_count, "input_count")?,
            to_sqlite_integer(shape.output_count, "output_count")?,
            shape.script_type.key(),
            event.event_id,
        ],
    )?;
    let input = ClassificationInput {
        txid,
        wtxid: Some(wtxid),
        transaction: Some(&parsed),
    };
    for pack in registered_packs() {
        let taxonomy = pack.taxonomy();
        let result = pack.classify(&input);
        // A missing verdict stores nothing: absence is the honest `unknown`.
        let Some(verdict) = result.verdict else {
            continue;
        };
        db.execute(
            "INSERT INTO transaction_classification (
                txid, taxonomy, verdict, classifier_id, classifier_version,
                status, evidence_json, derived_from_event_id
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (txid, taxonomy) DO NOTHING",
            params![
                txid,
                taxonomy.key,
                verdict,
                result.classifier_id,
                result.classifier_version,
                classification_status_as_str(result.status),
                serde_json::to_string(&result.evidence)?,
                event.event_id,
            ],
        )?;
    }
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

fn mempool_entry_from_row(row: &rusqlite::Row<'_>) -> Result<MempoolEntry, rusqlite::Error> {
    Ok(MempoolEntry {
        txid: row.get(0)?,
        facts: MempoolEntryFactsStatus::Available {
            facts: MempoolEntryFacts {
                vsize: nonnegative_integer_from_row(row.get(1)?, 1)?,
                fee_sats: nonnegative_integer_from_row(row.get(2)?, 2)?,
                entered_at_ms: nonnegative_integer_from_row(row.get(3)?, 3)?,
            },
        },
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
            verdicts: Vec::new(),
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
    }))
}

fn source_health_from_row(row: &rusqlite::Row<'_>) -> Result<SourceHealth, rusqlite::Error> {
    let epoch_id = atlas_model::SourceEpochId::new(row.get::<_, String>(0)?)
        .map_err(|error| model_conversion_from_sql(0, error))?;
    let revision = nonnegative_integer_from_row(row.get(1)?, 1)?;
    let state_cursor = ReplicaCursor::new(epoch_id, revision)
        .map_err(|error| model_conversion_from_sql(1, error))?;
    Ok(SourceHealth {
        state_cursor,
        state_observed_at_ms: nonnegative_integer_from_row(row.get(2)?, 2)?,
        capture: CaptureStatus::NotCollected,
    })
}

const fn capture_gap_certainty_as_str(certainty: CaptureGapCertainty) -> &'static str {
    match certainty {
        CaptureGapCertainty::PossibleLoss => "possible_loss",
        CaptureGapCertainty::KnownLoss => "known_loss",
    }
}

pub(crate) fn nonnegative_integer_from_row(
    value: i64,
    column: usize,
) -> Result<u64, rusqlite::Error> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn model_conversion_from_sql(
    column: usize,
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(error))
}

pub(crate) fn to_sqlite_integer(value: u64, field: &'static str) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::NumericOverflow { field })
}

#[cfg(test)]
mod tests {
    use atlas_model::{
        CaptureGapCertainty, CaptureStatus, CheckpointBegin, CheckpointChunk, CheckpointCommit,
        CheckpointId, Evidence, MAX_CHECKPOINT_CHUNK_ENTRIES, NormalizedEvent,
        ReconciledMembership, SourceEpochId, SourceReplicaCommand, SourceReplicaEntry,
        SourceReplicaRequest, SourceSessionId,
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

    #[test]
    fn opened_connections_apply_the_shared_sqlite_capacity_policy() {
        let (_temporary, store) = test_store();
        let connection = store.connect().expect("connect");
        let temp_store = connection
            .pragma_query_value(None, "temp_store", |row| row.get::<_, i64>(0))
            .expect("read temp store");
        let page_size = connection
            .pragma_query_value(None, "page_size", |row| row.get::<_, i64>(0))
            .and_then(|value| nonnegative_integer_from_row(value, 0))
            .expect("read page size");
        let max_page_count = connection
            .pragma_query_value(None, "max_page_count", |row| row.get::<_, i64>(0))
            .and_then(|value| nonnegative_integer_from_row(value, 0))
            .expect("read max page count");
        let journal_size_limit = connection
            .pragma_query_value(None, "journal_size_limit", |row| row.get::<_, i64>(0))
            .and_then(|value| nonnegative_integer_from_row(value, 0))
            .expect("read journal size limit");
        let wal_autocheckpoint = connection
            .pragma_query_value(None, "wal_autocheckpoint", |row| row.get::<_, i64>(0))
            .and_then(|value| nonnegative_integer_from_row(value, 0))
            .expect("read WAL auto-checkpoint interval");

        assert_eq!(temp_store, 2);
        assert_eq!(
            max_page_count,
            store.limits().sqlite.max_database_bytes / page_size
        );
        assert_eq!(
            journal_size_limit,
            store.limits().sqlite.retained_wal_high_water_bytes
        );
        assert_eq!(
            wal_autocheckpoint,
            u64::from(store.limits().sqlite.wal_autocheckpoint_pages)
        );
    }

    #[test]
    fn allowlist_cannot_exceed_the_persistent_source_identity_cap() {
        let allowed = ["source-a", "source-b"]
            .into_iter()
            .map(|value| SourceId::new(value).expect("source"))
            .collect();
        let limits = StoreLimits::new(
            DEFAULT_LIBRARY_STORAGE_LIMITS,
            1,
            DEFAULT_MAX_MEMBERSHIP_ENTRIES,
            Some(allowed),
        );

        assert!(matches!(
            limits,
            Err(StoreLimitsError::AllowlistExceedsPersistentSourceLimit {
                found: 2,
                maximum: 1
            })
        ));
    }

    #[test]
    fn membership_limit_must_fit_the_wire_contract() {
        for maximum in [1, MAX_CHECKPOINT_ENTRIES] {
            assert!(StoreLimits::new(DEFAULT_LIBRARY_STORAGE_LIMITS, 1, maximum, None).is_ok());
        }
        for invalid in [0, MAX_CHECKPOINT_ENTRIES + 1] {
            assert!(matches!(
                StoreLimits::new(DEFAULT_LIBRARY_STORAGE_LIMITS, 1, invalid, None),
                Err(StoreLimitsError::MembershipLimitOutOfRange { found, maximum })
                    if found == invalid && maximum == MAX_CHECKPOINT_ENTRIES
            ));
        }
        assert_eq!(StoreLimits::default().max_membership_entries, 200_000);
    }

    #[test]
    fn fresh_schema_enforces_byte_bounds_for_replica_identifiers() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("atlas.db");
        Store::migrate(&path).expect("migrate");
        let connection = Connection::open(&path).expect("open raw database connection");
        let insert = |source_id: &str, epoch_id: &str, checkpoint_id: &str| {
            connection.execute(
                "INSERT INTO source_replica_generation (
                    source_id, generation_id, role, epoch_id, revision,
                    state_observed_at_ms, checkpoint_id, expected_entries,
                    expected_chunks, content_sha256
                 ) VALUES (?1, 1, 'staging', ?2, 1, 0, ?3, 0, 0, ?4)",
                params![source_id, epoch_id, checkpoint_id, "00".repeat(32)],
            )
        };
        let exactly_64_source = "s".repeat(64);
        let exactly_64_epoch = "e".repeat(64);
        let exactly_64_checkpoint = "c".repeat(64);

        insert(
            &exactly_64_source,
            &exactly_64_epoch,
            &exactly_64_checkpoint,
        )
        .expect("64-byte identifiers are accepted");

        for (source_id, epoch_id, checkpoint_id) in [
            (
                "s".repeat(65),
                "epoch-b".to_owned(),
                "checkpoint-b".to_owned(),
            ),
            (
                "source-c".to_owned(),
                "e".repeat(65),
                "checkpoint-c".to_owned(),
            ),
            ("source-d".to_owned(), "epoch-d".to_owned(), "c".repeat(65)),
        ] {
            let error = insert(&source_id, &epoch_id, &checkpoint_id)
                .expect_err("65-byte identifier must violate a schema constraint");
            assert!(matches!(
                error,
                rusqlite::Error::SqliteFailure(details, _)
                    if details.code == rusqlite::ErrorCode::ConstraintViolation
            ));
        }

        let rows: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM source_replica_generation",
                [],
                |row| row.get(0),
            )
            .expect("generation count");
        assert_eq!(rows, 1);
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

    fn state_entry(txid: &str, facts: MempoolEntryFacts) -> SourceReplicaEntry {
        SourceReplicaEntry::new(txid, facts).expect("state entry")
    }

    /// Installs one complete RPC-authoritative state generation through the
    /// same begin/chunk/commit reducer used by the HTTP endpoint. Repeated
    /// calls advance the existing source epoch by one revision.
    fn install_state(store: &Store, source_id: &str, mut entries: Vec<SourceReplicaEntry>) {
        entries.sort_by(|left, right| left.txid.cmp(&right.txid));
        let source_id = SourceId::new(source_id).expect("source");
        let active = store
            .active_source_replica(&source_id)
            .expect("active state read");
        let (epoch_id, revision, observed_at_ms, replaces) = match active {
            Some(active) => (
                active.cursor.epoch_id.clone(),
                active.cursor.revision + 1,
                active.state_observed_at_ms + 1,
                Some(active.cursor),
            ),
            None => (
                SourceEpochId::new(format!("test-epoch-{source_id}")).expect("epoch"),
                1,
                101,
                None,
            ),
        };
        let checkpoint_id = CheckpointId::new(format!("test-checkpoint-{source_id}-{revision}"))
            .expect("checkpoint");
        let expected_chunks = u32::try_from(entries.len().div_ceil(MAX_CHECKPOINT_CHUNK_ENTRIES))
            .expect("checkpoint chunk count");
        let begin = CheckpointBegin::new(
            checkpoint_id.clone(),
            replaces,
            revision,
            observed_at_ms,
            expected_chunks,
            &entries,
        )
        .expect("checkpoint begin");
        let request = |command| {
            SourceReplicaRequest::new(source_id.clone(), epoch_id.clone(), command)
                .expect("state request")
        };
        store
            .apply_source_replica(&request(SourceReplicaCommand::CheckpointBegin(
                begin.clone(),
            )))
            .expect("begin checkpoint");
        for (chunk_index, entries) in entries.chunks(MAX_CHECKPOINT_CHUNK_ENTRIES).enumerate() {
            let chunk = CheckpointChunk::new(
                checkpoint_id.clone(),
                u32::try_from(chunk_index).expect("chunk index"),
                entries.to_vec(),
            )
            .expect("checkpoint chunk");
            store
                .apply_source_replica(&request(SourceReplicaCommand::CheckpointChunk(chunk)))
                .expect("stage checkpoint");
        }
        let commit = CheckpointCommit::new(checkpoint_id, revision, begin.content_sha256)
            .expect("checkpoint commit");
        store
            .apply_source_replica(&request(SourceReplicaCommand::CheckpointCommit(commit)))
            .expect("commit checkpoint");
    }

    fn comparison_txid(index: u64) -> String {
        format!("{index:064x}")
    }

    fn state_entry_with(txid: &str, vsize: u64) -> SourceReplicaEntry {
        state_entry(
            txid,
            MempoolEntryFacts {
                vsize,
                fee_sats: vsize * 2,
                entered_at_ms: 1_000,
            },
        )
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

    fn capture_projection(
        store: &Store,
        source_id: &SourceId,
    ) -> Option<(u64, u64, u64, String, String, String)> {
        let connection = store.connect().expect("connect");
        connection
            .query_row(
                "SELECT first_gap_at_ms, latest_gap_at_ms, marker_count,
                        strongest_certainty, latest_input, latest_reason
                 FROM source_capture_state
                 WHERE source_id = ?1",
                [source_id.as_str()],
                |row| {
                    Ok((
                        nonnegative_integer_from_row(row.get(0)?, 0)?,
                        nonnegative_integer_from_row(row.get(1)?, 1)?,
                        nonnegative_integer_from_row(row.get(2)?, 2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()
            .expect("capture projection")
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
        assert_eq!(store.mempool(&source()).expect("mempool"), None);
    }

    #[test]
    fn state_checkpoint_exposes_and_updates_available_facts() {
        let (_temporary, store) = test_store();
        install_state(&store, "source-a", vec![state_entry(TXID, facts())]);
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
        install_state(&store, "source-a", vec![state_entry(TXID, changed.clone())]);
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
    fn legacy_evidence_cannot_mutate_active_state() {
        let (_temporary, store) = test_store();
        install_state(&store, "source-a", vec![state_entry(TXID, facts())]);
        store
            .ingest(&event_for(
                "session-a",
                1,
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
                2,
                Evidence::MempoolRemoved {
                    txid: TXID.to_owned(),
                    reason: Some("removed".to_owned()),
                },
            ))
            .expect("removal");
        store
            .ingest(&event_for(
                "session-a",
                3,
                Evidence::MempoolAdded {
                    txid: TXID.to_owned(),
                },
            ))
            .expect("readd");
        let snapshot = store
            .mempool(&source())
            .expect("mempool")
            .expect("known source");
        assert_eq!(snapshot.memberships.len(), 1);
        assert_eq!(snapshot.memberships[0].txid, TXID);
        assert_eq!(
            snapshot.memberships[0].facts,
            MempoolEntryFactsStatus::Available { facts: facts() }
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
    fn batch_ingest_does_not_establish_product_state() {
        let (temporary, store) = test_store();
        let present = event_for(
            "session-a",
            1,
            Evidence::MempoolReconciled {
                txid: TXID.to_owned(),
                membership: ReconciledMembership::Present { facts: facts() },
            },
        );
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

        assert_eq!(store.mempool(&source()).expect("mempool"), None);
        let event_count = Connection::open(temporary.path().join("atlas.db"))
            .expect("inspect database")
            .query_row(
                "SELECT COUNT(*) FROM event WHERE source_id = 'source-a'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("event count");
        assert_eq!(event_count, 2);
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
        assert_eq!(store.mempool(&source()).expect("mempool"), None);
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
        install_state(&store, "source-a", vec![]);
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
    fn only_active_state_establishes_a_known_source() {
        let (_temporary, store) = test_store();
        store.ingest(&added_event()).expect("ingest");

        assert_eq!(store.mempool(&source()).expect("mempool"), None);
        install_state(&store, "source-a", vec![state_entry(TXID, facts())]);

        let snapshot = store
            .mempool(&source())
            .expect("mempool")
            .expect("known source");
        assert_eq!(snapshot.source_id, source());
        assert_eq!(snapshot.health.state_cursor.revision, 1);
        assert_eq!(snapshot.health.state_observed_at_ms, 101);
        assert_eq!(snapshot.health.capture, CaptureStatus::NotCollected);
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
            capture_projection(&store, &source()),
            Some((
                200,
                300,
                2,
                "known_loss".to_owned(),
                "peer_observer_nats".to_owned(),
                "slow_consumer".to_owned(),
            ))
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
        assert_eq!(
            capture_projection(&store, &source()),
            Some((
                200,
                200,
                1,
                "possible_loss".to_owned(),
                "peer_observer_nats".to_owned(),
                "disconnected".to_owned(),
            ))
        );
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
        let before = capture_projection(&store, &source());
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

        assert_eq!(capture_projection(&store, &source()), before);
    }

    #[test]
    fn evidence_only_sources_are_hidden_until_state_is_committed() {
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

        let source_b_id = SourceId::new("source-b").expect("source");
        assert_eq!(store.mempool(&source()).expect("mempool"), None);
        assert_eq!(store.mempool(&source_b_id).expect("mempool"), None);

        install_state(&store, "source-a", vec![]);
        install_state(&store, "source-b", vec![state_entry(TXID, facts())]);

        let source_a = store
            .mempool(&source())
            .expect("mempool")
            .expect("source a");
        assert!(source_a.memberships.is_empty());
        assert_eq!(source_a.health.capture, CaptureStatus::NotCollected);
        let source_b = store
            .mempool(&source_b_id)
            .expect("mempool")
            .expect("source b");
        assert_eq!(source_b.memberships.len(), 1);
        assert_eq!(source_b.health.capture, CaptureStatus::NotCollected);
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
            capture_projection(&store, &source()),
            Some((
                100,
                300,
                2,
                "known_loss".to_owned(),
                "newer-input".to_owned(),
                "newer-reason".to_owned(),
            ))
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
        install_state(&store, "source-a", vec![state_entry(&txid, facts())]);

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
        // One stored verdict per registered pack, in registry order.
        assert_eq!(
            membership.available[0].verdicts,
            vec![
                ("behavior".to_owned(), "payment".to_owned()),
                ("bip110".to_owned(), "conforming".to_owned()),
                ("data_protocol".to_owned(), "none".to_owned()),
            ]
        );
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
        install_state(&store, "source-a", vec![state_entry(TXID, facts())]);

        let membership = store
            .mempool_facts(&source())
            .expect("facts")
            .expect("known source");
        assert_eq!(membership.available.len(), 1);
        assert!(membership.available[0].shape.is_none());
        assert!(membership.available[0].verdicts.is_empty());
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
        install_state(&store, "source-a", vec![state_entry(TXID, facts())]);

        let membership = store
            .mempool_facts(&source())
            .expect("facts")
            .expect("known source");
        assert!(membership.available[0].shape.is_none());
        assert!(membership.available[0].verdicts.is_empty());
    }

    #[test]
    fn migration_rejects_stale_schema_version() {
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
                expected: 8
            })
        ));
    }

    #[test]
    fn migration_rejects_future_schema_version() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("atlas.db");
        let connection = Connection::open(&path).expect("create database");
        connection
            .pragma_update(None, "user_version", 9)
            .expect("schema version");
        drop(connection);

        assert!(matches!(
            Store::migrate(path),
            Err(StoreError::SchemaVersion {
                found: 9,
                expected: 8
            })
        ));
    }

    #[test]
    fn source_comparison_computes_regions_over_partitioned_membership() {
        let (_temporary, store) = test_store();
        // A and E shared by all three; B in mid+loose; C in loose only; D in
        // strict only.
        let (a, b, c, d, e) = (
            comparison_txid(1),
            comparison_txid(2),
            comparison_txid(3),
            comparison_txid(4),
            comparison_txid(5),
        );
        install_state(
            &store,
            "strict",
            vec![
                state_entry_with(&a, 100),
                state_entry_with(&d, 400),
                state_entry_with(&e, 150),
            ],
        );
        install_state(
            &store,
            "mid",
            vec![
                state_entry_with(&a, 100),
                state_entry_with(&b, 200),
                state_entry_with(&e, 150),
            ],
        );
        install_state(
            &store,
            "loose",
            vec![
                state_entry_with(&a, 100),
                state_entry_with(&b, 200),
                state_entry_with(&c, 300),
                state_entry_with(&e, 150),
            ],
        );

        let sources = [
            SourceId::new("strict").expect("source"),
            SourceId::new("mid").expect("source"),
            SourceId::new("loose").expect("source"),
        ];
        let ComparisonOutcome::Computed(inputs) =
            store.source_comparison(&sources).expect("comparison")
        else {
            panic!("every requested source exists");
        };

        // Per-source totals in request order: fact-bearing count, summed vsize,
        // and the awaiting-RPC count. SourceReplica state always carries facts.
        let totals: Vec<(u64, u64, u64)> = inputs
            .source_totals
            .iter()
            .map(|total| {
                (
                    total.present.count,
                    total.present.vsize,
                    total.awaiting_rpc_count,
                )
            })
            .collect();
        assert_eq!(totals, vec![(3, 650, 0), (3, 450, 0), (4, 750, 0)]);

        // The shared intersection {A, E} is read from loose (most permissive).
        assert_eq!(inputs.shared.available.len(), 2);
        assert_eq!(
            inputs
                .shared
                .available
                .iter()
                .map(|facts| facts.vsize)
                .collect::<Vec<_>>(),
            vec![100, 150]
        );
        assert_eq!(inputs.shared.awaiting_rpc_count, 0);

        assert_eq!(inputs.stages.len(), 2);
        let first = &inputs.stages[0];
        assert_eq!(first.from.as_str(), "strict");
        assert_eq!(first.to.as_str(), "mid");
        // added = mid \ strict = {B}; anomaly = strict \ mid = {D}.
        assert_eq!(first.added.available.len(), 1);
        assert_eq!(first.added.available[0].vsize, 200);
        assert_eq!(first.anomaly.available.len(), 1);
        assert_eq!(first.anomaly.available[0].vsize, 400);

        let second = &inputs.stages[1];
        assert_eq!(second.from.as_str(), "mid");
        assert_eq!(second.to.as_str(), "loose");
        // added = loose \ mid = {C}; anomaly = mid \ loose = {} under nesting.
        assert_eq!(second.added.available.len(), 1);
        assert_eq!(second.added.available[0].vsize, 300);
        assert!(second.anomaly.available.is_empty());
        assert_eq!(second.anomaly.awaiting_rpc_count, 0);
    }

    #[test]
    fn source_comparison_names_the_first_unknown_source() {
        let (_temporary, store) = test_store();
        install_state(&store, "strict", vec![state_entry(TXID, facts())]);
        let sources = [
            SourceId::new("strict").expect("source"),
            SourceId::new("ghost").expect("source"),
        ];
        match store.source_comparison(&sources).expect("comparison") {
            ComparisonOutcome::UnknownSource(source_id) => assert_eq!(source_id.as_str(), "ghost"),
            ComparisonOutcome::Computed(_) => panic!("ghost is an unknown source"),
        }
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
                expected: 8
            })
        ));
    }
}
