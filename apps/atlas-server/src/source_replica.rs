//! Central persistence for RPC-authoritative source state.
//!
//! The reducer keeps at most one active and one staging generation per source.
//! Deltas mutate only the active generation. Checkpoint chunks mutate only the
//! staging generation, and commit replaces the active generation in one SQLite
//! transaction after verifying the complete canonical digest.

use atlas_model::{
    CheckpointBegin, CheckpointChunk, CheckpointCommit, CheckpointDigest, CheckpointId,
    CheckpointProgress, MempoolEntryFacts, ReplicaCursor, SourceEpochId, SourceId,
    SourceReplicaCommand, SourceReplicaEntry, SourceReplicaRequest, SourceReplicaResponse,
    StateDelta, StateHeartbeat, StateMutation,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use crate::store::{
    Store, StoreError, nonnegative_integer_from_row, storage_error_is_capacity, to_sqlite_integer,
};

const ACTIVE_ROLE: &str = "active";
const STAGING_ROLE: &str = "staging";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveSourceReplica {
    pub source_id: SourceId,
    pub cursor: ReplicaCursor,
    pub state_observed_at_ms: u64,
    pub entries: Vec<SourceReplicaEntry>,
}

#[derive(Clone, Debug)]
struct Generation {
    source_id: SourceId,
    generation_id: i64,
    role: String,
    epoch_id: SourceEpochId,
    revision: u64,
    state_observed_at_ms: u64,
    checkpoint_id: CheckpointId,
    supersedes_checkpoint_id: Option<CheckpointId>,
    replaces: Option<ReplicaCursor>,
    expected_entries: u64,
    expected_chunks: u32,
    content_sha256: String,
    last_delta_target_revision: Option<u64>,
    last_delta_content_sha256: Option<String>,
}

impl Generation {
    fn cursor(&self) -> ReplicaCursor {
        ReplicaCursor {
            epoch_id: self.epoch_id.clone(),
            revision: self.revision,
        }
    }

    fn is_begin_retry(&self, epoch_id: &SourceEpochId, begin: &CheckpointBegin) -> bool {
        self.epoch_id == *epoch_id
            && self.checkpoint_id == begin.checkpoint_id
            && self.supersedes_checkpoint_id == begin.supersedes_checkpoint_id
            && self.replaces == begin.replaces
            && self.revision == begin.target_revision
            && self.state_observed_at_ms == begin.state_observed_at_ms
            && self.expected_entries == begin.expected_entries
            && self.expected_chunks == begin.expected_chunks
            && self.content_sha256 == begin.content_sha256
    }
}

impl Store {
    pub fn apply_source_replica(
        &self,
        request: &SourceReplicaRequest,
    ) -> Result<SourceReplicaResponse, StoreError> {
        request.validate().map_err(|error| match error {
            atlas_model::SourceReplicaError::LimitExceeded { .. } => {
                capacity(error.to_string(), None)
            }
            _ => invalid(error.to_string(), None),
        })?;

        let _write_guard = self.lock_write_gate();
        self.attempt_wal_relief();
        // Acquire the writer reservation before reading a cursor. This makes
        // the cursor checks and subsequent mutation one compare-and-swap even
        // when more than one HTTP request reaches SQLite concurrently.
        let mut connection = self
            .connect()
            .map_err(|error| normalize_write_error(error, None))?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| normalize_write_error(StoreError::Database(error), None))?;
        let active_cursor_before =
            generation_by_role(&transaction, &request.source_id, ACTIVE_ROLE)?
                .map(|generation| generation.cursor());
        self.require_source_allowed(&request.source_id, active_cursor_before.clone())?;
        let result = match &request.command {
            SourceReplicaCommand::Delta(delta) => apply_delta(
                self,
                &transaction,
                &request.source_id,
                &request.epoch_id,
                delta,
            ),
            SourceReplicaCommand::Heartbeat(heartbeat) => apply_heartbeat(
                self,
                &transaction,
                &request.source_id,
                &request.epoch_id,
                heartbeat,
            ),
            SourceReplicaCommand::CheckpointBegin(begin) => begin_checkpoint(
                self,
                &transaction,
                &request.source_id,
                &request.epoch_id,
                begin,
            ),
            SourceReplicaCommand::CheckpointChunk(chunk) => stage_checkpoint_chunk(
                self,
                &transaction,
                &request.source_id,
                &request.epoch_id,
                chunk,
            ),
            SourceReplicaCommand::CheckpointCommit(commit) => commit_checkpoint(
                self,
                &transaction,
                &request.source_id,
                &request.epoch_id,
                commit,
            ),
        };
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                let error = normalize_write_error(error, active_cursor_before.clone());
                return Err(attach_staging_checkpoint_id(
                    &transaction,
                    &request.source_id,
                    error,
                )?);
            }
        };
        transaction.commit().map_err(|error| {
            normalize_write_error(StoreError::Database(error), active_cursor_before)
        })?;
        Ok(response)
    }

    /// Returns only the reader-visible SourceReplica generation. A staging
    /// checkpoint is deliberately invisible until its verified atomic commit.
    pub fn active_source_replica(
        &self,
        source_id: &SourceId,
    ) -> Result<Option<ActiveSourceReplica>, StoreError> {
        let connection = self.connect()?;
        let Some(active) = generation_by_role(&connection, source_id, ACTIVE_ROLE)? else {
            return Ok(None);
        };
        let entries = generation_entries(&connection, &active)?;
        Ok(Some(ActiveSourceReplica {
            source_id: source_id.clone(),
            cursor: active.cursor(),
            state_observed_at_ms: active.state_observed_at_ms,
            entries,
        }))
    }
}

fn apply_delta(
    store: &Store,
    transaction: &Transaction<'_>,
    source_id: &SourceId,
    epoch_id: &SourceEpochId,
    delta: &StateDelta,
) -> Result<SourceReplicaResponse, StoreError> {
    let Some(active) = generation_by_role(transaction, source_id, ACTIVE_ROLE)? else {
        return Err(cursor_mismatch(
            "source has no active generation; an atomic checkpoint is required",
            None,
        ));
    };
    let active_cursor = Some(active.cursor());
    require_epoch(epoch_id, &active, active_cursor.clone())?;

    if delta.target_revision == active.revision {
        if active.last_delta_target_revision == Some(delta.target_revision) {
            if active.last_delta_content_sha256.as_deref() == Some(&delta.content_sha256) {
                return Ok(SourceReplicaResponse::duplicate(active_cursor, None));
            }
            return Err(conflict(
                "conflicting_replay",
                format!(
                    "revision {} was already applied with different delta content",
                    delta.target_revision
                ),
                active_cursor,
            ));
        }
        return Err(cursor_mismatch(
            format!(
                "delta targets active revision {} but is not its last applied delta",
                active.revision
            ),
            active_cursor,
        ));
    }

    reject_active_write_while_staging(transaction, source_id, active_cursor.clone())?;

    if delta.base_revision != active.revision {
        return Err(cursor_mismatch(
            format!(
                "delta base revision {} does not equal active revision {}",
                delta.base_revision, active.revision
            ),
            active_cursor,
        ));
    }
    if delta.state_observed_at_ms < active.state_observed_at_ms {
        return Err(invalid(
            format!(
                "delta observation time {} precedes active observation time {}",
                delta.state_observed_at_ms, active.state_observed_at_ms
            ),
            active_cursor,
        ));
    }

    store.require_write_admission(transaction, active_cursor.clone())?;
    for mutation in &delta.mutations {
        match mutation {
            StateMutation::Absent { txid } => {
                transaction.execute(
                    "DELETE FROM source_replica_membership
                     WHERE source_id = ?1 AND generation_id = ?2 AND txid = ?3",
                    params![source_id.as_str(), active.generation_id, txid],
                )?;
            }
            StateMutation::Present { txid, facts } => {
                insert_or_replace_entry(transaction, &active, txid, facts)?;
            }
        }
    }

    let entry_count = generation_entry_count(transaction, &active)?;
    let maximum = store.limits().max_membership_entries;
    if entry_count > maximum {
        return Err(capacity(
            format!(
                "delta would grow active membership to {entry_count} entries; configured maximum is {maximum}"
            ),
            active_cursor,
        ));
    }

    transaction.execute(
        "UPDATE source_replica_generation
         SET revision = ?3,
             state_observed_at_ms = ?4,
             last_delta_target_revision = ?3,
             last_delta_content_sha256 = ?5
         WHERE source_id = ?1 AND generation_id = ?2 AND role = 'active'",
        params![
            source_id.as_str(),
            active.generation_id,
            to_sqlite_integer(delta.target_revision, "target_revision")?,
            to_sqlite_integer(delta.state_observed_at_ms, "state_observed_at_ms")?,
            delta.content_sha256,
        ],
    )?;

    Ok(SourceReplicaResponse::applied(
        Some(ReplicaCursor {
            epoch_id: epoch_id.clone(),
            revision: delta.target_revision,
        }),
        None,
    ))
}

fn apply_heartbeat(
    store: &Store,
    transaction: &Transaction<'_>,
    source_id: &SourceId,
    epoch_id: &SourceEpochId,
    heartbeat: &StateHeartbeat,
) -> Result<SourceReplicaResponse, StoreError> {
    let Some(active) = generation_by_role(transaction, source_id, ACTIVE_ROLE)? else {
        return Err(cursor_mismatch(
            "source has no active generation; an atomic checkpoint is required",
            None,
        ));
    };
    let active_cursor = Some(active.cursor());
    require_epoch(epoch_id, &active, active_cursor.clone())?;
    if heartbeat.revision != active.revision {
        return Err(cursor_mismatch(
            format!(
                "heartbeat revision {} does not equal active revision {}",
                heartbeat.revision, active.revision
            ),
            active_cursor,
        ));
    }
    if heartbeat.state_observed_at_ms == active.state_observed_at_ms {
        return Ok(SourceReplicaResponse::duplicate(active_cursor, None));
    }
    reject_active_write_while_staging(transaction, source_id, active_cursor.clone())?;
    if heartbeat.state_observed_at_ms < active.state_observed_at_ms {
        return Err(invalid(
            format!(
                "heartbeat observation time {} precedes active observation time {}",
                heartbeat.state_observed_at_ms, active.state_observed_at_ms
            ),
            active_cursor,
        ));
    }

    store.require_write_admission(transaction, active_cursor.clone())?;
    transaction.execute(
        "UPDATE source_replica_generation
         SET state_observed_at_ms = ?3
         WHERE source_id = ?1 AND generation_id = ?2 AND role = 'active'",
        params![
            source_id.as_str(),
            active.generation_id,
            to_sqlite_integer(heartbeat.state_observed_at_ms, "state_observed_at_ms")?,
        ],
    )?;
    Ok(SourceReplicaResponse::applied(active_cursor, None))
}

fn begin_checkpoint(
    store: &Store,
    transaction: &Transaction<'_>,
    source_id: &SourceId,
    epoch_id: &SourceEpochId,
    begin: &CheckpointBegin,
) -> Result<SourceReplicaResponse, StoreError> {
    let active = generation_by_role(transaction, source_id, ACTIVE_ROLE)?;
    let active_cursor = active.as_ref().map(Generation::cursor);

    if let Some(active) = &active
        && active.checkpoint_id == begin.checkpoint_id
    {
        if active.is_begin_retry(epoch_id, begin) {
            return Ok(SourceReplicaResponse::duplicate(active_cursor, None));
        }
        return Err(conflict(
            "checkpoint_conflict",
            format!(
                "checkpoint ID {} was already used with different content",
                begin.checkpoint_id
            ),
            active_cursor,
        ));
    }

    let existing_staging = generation_by_role(transaction, source_id, STAGING_ROLE)?;
    if let Some(staging) = &existing_staging {
        if staging.checkpoint_id == begin.checkpoint_id && staging.is_begin_retry(epoch_id, begin) {
            let progress = checkpoint_progress(transaction, staging)?;
            return Ok(SourceReplicaResponse::duplicate(
                active_cursor,
                Some(progress),
            ));
        }
        if staging.checkpoint_id == begin.checkpoint_id {
            return Err(conflict(
                "checkpoint_conflict",
                format!(
                    "checkpoint ID {} was replayed with different content",
                    begin.checkpoint_id
                ),
                active_cursor,
            ));
        }
        if begin.supersedes_checkpoint_id.as_ref() != Some(&staging.checkpoint_id) {
            return Err(conflict(
                "checkpoint_conflict",
                format!(
                    "checkpoint {} is staging; a different begin must explicitly supersede it",
                    staging.checkpoint_id
                ),
                active_cursor,
            ));
        }
    }

    if existing_staging.is_none() && begin.supersedes_checkpoint_id.is_some() {
        return Err(conflict(
            "checkpoint_conflict",
            format!(
                "checkpoint {} cannot supersede {:?}; no checkpoint is staging",
                begin.checkpoint_id, begin.supersedes_checkpoint_id
            ),
            active_cursor,
        ));
    }

    let replaced_cursor = active.as_ref().map(Generation::cursor);
    if begin.replaces != replaced_cursor {
        return Err(cursor_mismatch(
            format!(
                "checkpoint replacement cursor {:?} does not equal active cursor {:?}",
                begin.replaces, replaced_cursor
            ),
            active_cursor,
        ));
    }
    if let Some(active) = &active
        && begin.state_observed_at_ms < active.state_observed_at_ms
    {
        return Err(invalid(
            format!(
                "checkpoint observation time {} precedes active observation time {}",
                begin.state_observed_at_ms, active.state_observed_at_ms
            ),
            active_cursor,
        ));
    }
    let maximum = store.limits().max_membership_entries;
    if begin.expected_entries > maximum {
        return Err(capacity(
            format!(
                "checkpoint declares {} entries; configured maximum is {maximum}",
                begin.expected_entries
            ),
            active_cursor,
        ));
    }

    if active.is_none() && existing_staging.is_none() {
        store.require_new_source_identity_admission(transaction, source_id)?;
    }
    store.require_write_admission(transaction, active_cursor.clone())?;

    // Replacing an abandoned staging generation is an explicit compare-and-
    // swap. A delayed begin cannot delete whichever checkpoint happens to be
    // staging when it arrives.
    if let Some(staging) = &existing_staging {
        let deleted = transaction.execute(
            "DELETE FROM source_replica_generation
             WHERE source_id = ?1 AND role = 'staging' AND checkpoint_id = ?2",
            params![source_id.as_str(), staging.checkpoint_id.as_str()],
        )?;
        if deleted != 1 {
            return Err(conflict(
                "checkpoint_conflict",
                format!(
                    "checkpoint {} stopped staging before it could be superseded",
                    staging.checkpoint_id
                ),
                active_cursor,
            ));
        }
    }

    let generation_id: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(generation_id), 0) + 1
         FROM source_replica_generation WHERE source_id = ?1",
        [source_id.as_str()],
        |row| row.get(0),
    )?;
    let (replaces_epoch_id, replaces_revision) = match &begin.replaces {
        Some(cursor) => (
            Some(cursor.epoch_id.as_str()),
            Some(to_sqlite_integer(cursor.revision, "replaces_revision")?),
        ),
        None => (None, None),
    };
    transaction.execute(
        "INSERT INTO source_replica_generation (
            source_id, generation_id, role, epoch_id, revision,
            state_observed_at_ms, checkpoint_id, supersedes_checkpoint_id,
            replaces_epoch_id, replaces_revision, expected_entries,
            expected_chunks, content_sha256
         ) VALUES (?1, ?2, 'staging', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            source_id.as_str(),
            generation_id,
            epoch_id.as_str(),
            to_sqlite_integer(begin.target_revision, "target_revision")?,
            to_sqlite_integer(begin.state_observed_at_ms, "state_observed_at_ms")?,
            begin.checkpoint_id.as_str(),
            begin
                .supersedes_checkpoint_id
                .as_ref()
                .map(CheckpointId::as_str),
            replaces_epoch_id,
            replaces_revision,
            to_sqlite_integer(begin.expected_entries, "expected_entries")?,
            i64::from(begin.expected_chunks),
            begin.content_sha256,
        ],
    )?;

    let staging = generation_by_role(transaction, source_id, STAGING_ROLE)?
        .expect("the staging generation was inserted in this transaction");
    let progress = checkpoint_progress(transaction, &staging)?;
    Ok(SourceReplicaResponse::applied(
        active_cursor,
        Some(progress),
    ))
}

fn stage_checkpoint_chunk(
    store: &Store,
    transaction: &Transaction<'_>,
    source_id: &SourceId,
    epoch_id: &SourceEpochId,
    chunk: &CheckpointChunk,
) -> Result<SourceReplicaResponse, StoreError> {
    let active = generation_by_role(transaction, source_id, ACTIVE_ROLE)?;
    let active_cursor = active.as_ref().map(Generation::cursor);
    if active
        .as_ref()
        .is_some_and(|generation| generation.checkpoint_id == chunk.checkpoint_id)
    {
        return completed_chunk_retry_or_conflict(
            transaction,
            active.as_ref(),
            epoch_id,
            chunk,
            active_cursor,
        );
    }
    let Some(staging) = generation_by_role(transaction, source_id, STAGING_ROLE)? else {
        return completed_chunk_retry_or_conflict(
            transaction,
            active.as_ref(),
            epoch_id,
            chunk,
            active_cursor,
        );
    };
    if staging.epoch_id != *epoch_id {
        return Err(epoch_conflict(epoch_id, &staging.epoch_id, active_cursor));
    }
    if staging.checkpoint_id != chunk.checkpoint_id {
        return Err(conflict(
            "checkpoint_conflict",
            format!(
                "chunk names checkpoint {}, but {} is staging",
                chunk.checkpoint_id, staging.checkpoint_id
            ),
            active_cursor,
        ));
    }
    if chunk.chunk_index >= staging.expected_chunks {
        return Err(invalid(
            format!(
                "chunk index {} is outside declared range 0..{}",
                chunk.chunk_index, staging.expected_chunks
            ),
            active_cursor,
        ));
    }

    let existing_digest = transaction
        .query_row(
            "SELECT content_sha256 FROM source_replica_checkpoint_chunk
             WHERE source_id = ?1 AND generation_id = ?2 AND chunk_index = ?3",
            params![
                source_id.as_str(),
                staging.generation_id,
                i64::from(chunk.chunk_index)
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(existing_digest) = existing_digest {
        if existing_digest == chunk.content_sha256 {
            return Ok(SourceReplicaResponse::duplicate(
                active_cursor,
                Some(checkpoint_progress(transaction, &staging)?),
            ));
        }
        return Err(conflict(
            "checkpoint_conflict",
            format!(
                "chunk {} was already staged with different content",
                chunk.chunk_index
            ),
            active_cursor,
        ));
    }

    let next_chunk_index = transaction.query_row(
        "SELECT COUNT(*) FROM source_replica_checkpoint_chunk
         WHERE source_id = ?1 AND generation_id = ?2",
        params![source_id.as_str(), staging.generation_id],
        |row| nonnegative_integer_from_row(row.get(0)?, 0),
    )?;
    let next_chunk_index = u32::try_from(next_chunk_index).map_err(|_| {
        capacity(
            "checkpoint chunk count cannot be represented".to_owned(),
            active_cursor.clone(),
        )
    })?;
    if chunk.chunk_index != next_chunk_index {
        return Err(invalid(
            format!(
                "checkpoint chunk index {} is out of order; next expected index is {next_chunk_index}",
                chunk.chunk_index
            ),
            active_cursor,
        ));
    }

    let received_entries = generation_entry_count(transaction, &staging)?;
    let chunk_entries = u64::try_from(chunk.entries.len()).map_err(|_| {
        capacity(
            "checkpoint chunk entry count cannot be represented".to_owned(),
            active_cursor.clone(),
        )
    })?;
    let staged_after = received_entries.checked_add(chunk_entries).ok_or_else(|| {
        capacity(
            "checkpoint staged entry count overflowed".to_owned(),
            active_cursor.clone(),
        )
    })?;
    let maximum = store.limits().max_membership_entries;
    if staging.expected_entries > maximum
        || staged_after > staging.expected_entries
        || staged_after > maximum
    {
        return Err(capacity(
            format!(
                "chunk would stage {staged_after} entries against declared {} and configured maximum {maximum}",
                staging.expected_entries
            ),
            active_cursor,
        ));
    }

    store.require_write_admission(transaction, active_cursor.clone())?;
    for entry in &chunk.entries {
        if !insert_staged_entry(transaction, &staging, entry)? {
            return Err(invalid(
                format!(
                    "checkpoint contains txid {} in more than one chunk",
                    entry.txid
                ),
                active_cursor,
            ));
        }
    }
    transaction.execute(
        "INSERT INTO source_replica_checkpoint_chunk (
            source_id, generation_id, chunk_index, entry_count, content_sha256
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            source_id.as_str(),
            staging.generation_id,
            i64::from(chunk.chunk_index),
            to_sqlite_integer(chunk_entries, "chunk_entry_count")?,
            chunk.content_sha256,
        ],
    )?;
    Ok(SourceReplicaResponse::applied(
        active_cursor,
        Some(checkpoint_progress(transaction, &staging)?),
    ))
}

fn commit_checkpoint(
    store: &Store,
    transaction: &Transaction<'_>,
    source_id: &SourceId,
    epoch_id: &SourceEpochId,
    commit: &CheckpointCommit,
) -> Result<SourceReplicaResponse, StoreError> {
    let active = generation_by_role(transaction, source_id, ACTIVE_ROLE)?;
    let active_cursor = active.as_ref().map(Generation::cursor);
    if let Some(active) = &active
        && active.checkpoint_id == commit.checkpoint_id
    {
        require_epoch(epoch_id, active, active_cursor.clone())?;
        if active.revision == commit.target_revision
            && active.content_sha256 == commit.content_sha256
        {
            return Ok(SourceReplicaResponse::duplicate(active_cursor, None));
        }
        return Err(conflict(
            "checkpoint_conflict",
            format!(
                "checkpoint ID {} was committed with different content",
                commit.checkpoint_id
            ),
            active_cursor,
        ));
    }

    let Some(staging) = generation_by_role(transaction, source_id, STAGING_ROLE)? else {
        return Err(conflict(
            "checkpoint_conflict",
            format!("checkpoint {} is not staging", commit.checkpoint_id),
            active_cursor,
        ));
    };
    if staging.epoch_id != *epoch_id {
        return Err(epoch_conflict(epoch_id, &staging.epoch_id, active_cursor));
    }
    if staging.checkpoint_id != commit.checkpoint_id
        || staging.revision != commit.target_revision
        || staging.content_sha256 != commit.content_sha256
    {
        return Err(conflict(
            "checkpoint_conflict",
            "checkpoint commit does not match its begin declaration".to_owned(),
            active_cursor,
        ));
    }

    let maximum = store.limits().max_membership_entries;
    if staging.expected_entries > maximum {
        return Err(capacity(
            format!(
                "checkpoint declares {} entries; configured maximum is {maximum}",
                staging.expected_entries
            ),
            active_cursor,
        ));
    }

    // Recheck the checkpoint-begin CAS at activation. This prevents a delta
    // accepted while a checkpoint is staging from being silently overwritten.
    let current_cursor = active.as_ref().map(Generation::cursor);
    if current_cursor != staging.replaces {
        return Err(cursor_mismatch(
            format!(
                "active cursor changed from checkpoint replacement cursor {:?} to {:?}",
                staging.replaces, current_cursor
            ),
            active_cursor,
        ));
    }

    let staged_entries = verify_checkpoint_complete(transaction, &staging, active_cursor.clone())?;
    if staged_entries > maximum {
        return Err(capacity(
            format!(
                "checkpoint contains {staged_entries} entries; configured maximum is {maximum}"
            ),
            active_cursor,
        ));
    }
    let digest = generation_digest(transaction, &staging, active_cursor.clone())?;
    if digest != staging.content_sha256 {
        return Err(invalid(
            format!(
                "checkpoint digest {} does not match declared {}",
                digest, staging.content_sha256
            ),
            active_cursor,
        ));
    }

    store.require_write_admission(transaction, active_cursor.clone())?;
    transaction.execute(
        "DELETE FROM source_replica_generation
         WHERE source_id = ?1 AND role = 'active'",
        [source_id.as_str()],
    )?;
    let updated = transaction.execute(
        "UPDATE source_replica_generation SET role = 'active'
         WHERE source_id = ?1 AND generation_id = ?2 AND role = 'staging'",
        params![source_id.as_str(), staging.generation_id],
    )?;
    if updated != 1 {
        return Err(invalid(
            "staging generation disappeared before activation".to_owned(),
            None,
        ));
    }

    Ok(SourceReplicaResponse::applied(Some(staging.cursor()), None))
}

fn completed_chunk_retry_or_conflict(
    transaction: &Transaction<'_>,
    active: Option<&Generation>,
    epoch_id: &SourceEpochId,
    chunk: &CheckpointChunk,
    active_cursor: Option<ReplicaCursor>,
) -> Result<SourceReplicaResponse, StoreError> {
    let Some(active) = active else {
        return Err(conflict(
            "checkpoint_conflict",
            format!("checkpoint {} is not staging", chunk.checkpoint_id),
            None,
        ));
    };
    require_epoch(epoch_id, active, active_cursor.clone())?;
    if active.checkpoint_id != chunk.checkpoint_id {
        return Err(conflict(
            "checkpoint_conflict",
            format!("checkpoint {} is not staging", chunk.checkpoint_id),
            active_cursor,
        ));
    }
    let existing_digest = transaction
        .query_row(
            "SELECT content_sha256 FROM source_replica_checkpoint_chunk
             WHERE source_id = ?1 AND generation_id = ?2 AND chunk_index = ?3",
            params![
                active.source_id.as_str(),
                active.generation_id,
                i64::from(chunk.chunk_index)
            ],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    match existing_digest {
        Some(digest) if digest == chunk.content_sha256 => {
            Ok(SourceReplicaResponse::duplicate(active_cursor, None))
        }
        Some(_) => Err(conflict(
            "checkpoint_conflict",
            format!(
                "chunk {} was committed with different content",
                chunk.chunk_index
            ),
            active_cursor,
        )),
        None => Err(conflict(
            "checkpoint_conflict",
            format!(
                "chunk {} was not part of committed checkpoint {}",
                chunk.chunk_index, chunk.checkpoint_id
            ),
            active_cursor,
        )),
    }
}

fn verify_checkpoint_complete(
    transaction: &Transaction<'_>,
    staging: &Generation,
    active_cursor: Option<ReplicaCursor>,
) -> Result<u64, StoreError> {
    let (received_chunks, received_entries, minimum_chunk, maximum_chunk) = transaction.query_row(
        "SELECT COUNT(*), COALESCE(SUM(entry_count), 0), MIN(chunk_index), MAX(chunk_index)
         FROM source_replica_checkpoint_chunk
         WHERE source_id = ?1 AND generation_id = ?2",
        params![staging.source_id.as_str(), staging.generation_id],
        |row| {
            Ok((
                nonnegative_integer_from_row(row.get(0)?, 0)?,
                nonnegative_integer_from_row(row.get(1)?, 1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        },
    )?;
    let actual_entries = generation_entry_count(transaction, staging)?;
    let expected_chunks = u64::from(staging.expected_chunks);
    let complete_indices = if staging.expected_chunks == 0 {
        minimum_chunk.is_none() && maximum_chunk.is_none()
    } else {
        minimum_chunk == Some(0) && maximum_chunk == Some(i64::from(staging.expected_chunks) - 1)
    };
    if received_chunks != expected_chunks
        || received_entries != staging.expected_entries
        || actual_entries != staging.expected_entries
        || !complete_indices
    {
        return Err(conflict(
            "checkpoint_conflict",
            format!(
                "checkpoint is incomplete: received {actual_entries}/{} entries and {received_chunks}/{expected_chunks} contiguous chunks",
                staging.expected_entries
            ),
            active_cursor,
        ));
    }
    Ok(actual_entries)
}

fn require_epoch(
    requested: &SourceEpochId,
    generation: &Generation,
    active_cursor: Option<ReplicaCursor>,
) -> Result<(), StoreError> {
    if requested == &generation.epoch_id {
        return Ok(());
    }
    Err(epoch_conflict(
        requested,
        &generation.epoch_id,
        active_cursor,
    ))
}

fn reject_active_write_while_staging(
    transaction: &Transaction<'_>,
    source_id: &SourceId,
    active_cursor: Option<ReplicaCursor>,
) -> Result<(), StoreError> {
    let Some(staging) = generation_by_role(transaction, source_id, STAGING_ROLE)? else {
        return Ok(());
    };
    Err(conflict(
        "checkpoint_conflict",
        format!(
            "checkpoint {} is staging; active state is frozen until commit or supersession",
            staging.checkpoint_id
        ),
        active_cursor,
    ))
}

fn epoch_conflict(
    requested: &SourceEpochId,
    expected: &SourceEpochId,
    active_cursor: Option<ReplicaCursor>,
) -> StoreError {
    conflict(
        "epoch_conflict",
        format!("epoch {requested} does not equal current epoch {expected}"),
        active_cursor,
    )
}

fn cursor_mismatch(message: impl Into<String>, active_cursor: Option<ReplicaCursor>) -> StoreError {
    conflict("cursor_mismatch", message.into(), active_cursor)
}

fn attach_staging_checkpoint_id(
    transaction: &Transaction<'_>,
    source_id: &SourceId,
    error: StoreError,
) -> Result<StoreError, StoreError> {
    let StoreError::SourceReplicaConflict {
        code,
        message,
        active_cursor,
        staging_checkpoint_id: None,
    } = error
    else {
        return Ok(error);
    };
    let staging_checkpoint_id = transaction
        .query_row(
            "SELECT checkpoint_id FROM source_replica_generation
             WHERE source_id = ?1 AND role = 'staging'",
            [source_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(CheckpointId::new)
        .transpose()?;
    Ok(StoreError::SourceReplicaConflict {
        code,
        message,
        active_cursor,
        staging_checkpoint_id,
    })
}

fn conflict(
    code: &'static str,
    message: String,
    active_cursor: Option<ReplicaCursor>,
) -> StoreError {
    StoreError::SourceReplicaConflict {
        code,
        message,
        active_cursor,
        staging_checkpoint_id: None,
    }
}

fn capacity(message: String, active_cursor: Option<ReplicaCursor>) -> StoreError {
    StoreError::SourceReplicaCapacity {
        message,
        active_cursor,
    }
}

fn normalize_write_error(error: StoreError, active_cursor: Option<ReplicaCursor>) -> StoreError {
    let capacity_error = match &error {
        StoreError::Database(rusqlite::Error::SqliteFailure(details, _)) => {
            details.code == rusqlite::ErrorCode::DiskFull
        }
        StoreError::Storage(storage) => storage_error_is_capacity(storage),
        _ => false,
    };
    if capacity_error {
        return capacity(
            format!("SQLite write capacity is exhausted: {error}"),
            active_cursor,
        );
    }
    error
}

fn invalid(message: String, active_cursor: Option<ReplicaCursor>) -> StoreError {
    StoreError::InvalidSourceReplica {
        message,
        active_cursor,
    }
}

fn generation_by_role(
    transaction: &rusqlite::Connection,
    source_id: &SourceId,
    role: &str,
) -> Result<Option<Generation>, StoreError> {
    transaction
        .query_row(
            "SELECT source_id, generation_id, role, epoch_id, revision,
                    state_observed_at_ms, checkpoint_id, supersedes_checkpoint_id,
                    replaces_epoch_id, replaces_revision, expected_entries, expected_chunks,
                    content_sha256, last_delta_target_revision,
                    last_delta_content_sha256
             FROM source_replica_generation
             WHERE source_id = ?1 AND role = ?2",
            params![source_id.as_str(), role],
            generation_from_row,
        )
        .optional()
        .map_err(StoreError::from)
}

fn generation_from_row(row: &rusqlite::Row<'_>) -> Result<Generation, rusqlite::Error> {
    let source_id =
        SourceId::new(row.get::<_, String>(0)?).map_err(|error| conversion_from_sql(0, error))?;
    let epoch_id = SourceEpochId::new(row.get::<_, String>(3)?)
        .map_err(|error| conversion_from_sql(3, error))?;
    let supersedes_checkpoint_id = row
        .get::<_, Option<String>>(7)?
        .map(CheckpointId::new)
        .transpose()
        .map_err(|error| conversion_from_sql(7, error))?;
    let replaces_epoch = row.get::<_, Option<String>>(8)?;
    let replaces_revision = row.get::<_, Option<i64>>(9)?;
    let replaces = match (replaces_epoch, replaces_revision) {
        (None, None) => None,
        (Some(epoch), Some(revision)) => Some(ReplicaCursor {
            epoch_id: SourceEpochId::new(epoch).map_err(|error| conversion_from_sql(8, error))?,
            revision: nonnegative_integer_from_row(revision, 9)?,
        }),
        _ => {
            return Err(invalid_data_from_sql(
                8,
                "generation contains an incomplete replacement cursor",
            ));
        }
    };
    let expected_chunks = u32::try_from(nonnegative_integer_from_row(row.get(11)?, 11)?)
        .map_err(|error| conversion_from_sql(11, error))?;
    let last_delta_target_revision = row
        .get::<_, Option<i64>>(13)?
        .map(|revision| nonnegative_integer_from_row(revision, 13))
        .transpose()?;
    Ok(Generation {
        source_id,
        generation_id: row.get(1)?,
        role: row.get(2)?,
        epoch_id,
        revision: nonnegative_integer_from_row(row.get(4)?, 4)?,
        state_observed_at_ms: nonnegative_integer_from_row(row.get(5)?, 5)?,
        checkpoint_id: CheckpointId::new(row.get::<_, String>(6)?)
            .map_err(|error| conversion_from_sql(6, error))?,
        supersedes_checkpoint_id,
        replaces,
        expected_entries: nonnegative_integer_from_row(row.get(10)?, 10)?,
        expected_chunks,
        content_sha256: row.get(12)?,
        last_delta_target_revision,
        last_delta_content_sha256: row.get(14)?,
    })
}

fn checkpoint_progress(
    transaction: &Transaction<'_>,
    staging: &Generation,
) -> Result<CheckpointProgress, StoreError> {
    debug_assert_eq!(staging.role, STAGING_ROLE);
    let received_entries = generation_entry_count(transaction, staging)?;
    let received_chunks = transaction.query_row(
        "SELECT COUNT(*) FROM source_replica_checkpoint_chunk
         WHERE source_id = ?1 AND generation_id = ?2",
        params![staging.source_id.as_str(), staging.generation_id],
        |row| nonnegative_integer_from_row(row.get(0)?, 0),
    )?;
    let received_chunks = u32::try_from(received_chunks)
        .map_err(|error| StoreError::Database(conversion_from_sql(0, error)))?;
    CheckpointProgress::new(
        staging.checkpoint_id.clone(),
        staging.cursor(),
        received_entries,
        staging.expected_entries,
        received_chunks,
        staging.expected_chunks,
    )
    .map_err(StoreError::from)
}

fn generation_entry_count(
    transaction: &rusqlite::Connection,
    generation: &Generation,
) -> Result<u64, StoreError> {
    transaction
        .query_row(
            "SELECT COUNT(*) FROM source_replica_membership
             WHERE source_id = ?1 AND generation_id = ?2",
            params![generation.source_id.as_str(), generation.generation_id],
            |row| nonnegative_integer_from_row(row.get(0)?, 0),
        )
        .map_err(StoreError::from)
}

fn generation_entries(
    transaction: &rusqlite::Connection,
    generation: &Generation,
) -> Result<Vec<SourceReplicaEntry>, StoreError> {
    let mut statement = transaction.prepare(
        "SELECT txid, vsize, fee_sats, entered_at_ms
         FROM source_replica_membership
         WHERE source_id = ?1 AND generation_id = ?2
         ORDER BY txid",
    )?;
    let rows = statement.query_map(
        params![generation.source_id.as_str(), generation.generation_id],
        |row| {
            Ok(SourceReplicaEntry {
                txid: row.get(0)?,
                facts: MempoolEntryFacts {
                    vsize: nonnegative_integer_from_row(row.get(1)?, 1)?,
                    fee_sats: nonnegative_integer_from_row(row.get(2)?, 2)?,
                    entered_at_ms: nonnegative_integer_from_row(row.get(3)?, 3)?,
                },
            })
        },
    )?;
    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?);
    }
    Ok(entries)
}

fn generation_digest(
    transaction: &rusqlite::Connection,
    generation: &Generation,
    active_cursor: Option<ReplicaCursor>,
) -> Result<String, StoreError> {
    let mut digest = CheckpointDigest::new(generation.expected_entries)
        .map_err(|error| invalid(error.to_string(), active_cursor.clone()))?;
    let mut statement = transaction.prepare(
        "SELECT txid, vsize, fee_sats, entered_at_ms
         FROM source_replica_membership
         WHERE source_id = ?1 AND generation_id = ?2
         ORDER BY txid",
    )?;
    let mut rows = statement.query(params![
        generation.source_id.as_str(),
        generation.generation_id
    ])?;
    while let Some(row) = rows.next()? {
        let entry = SourceReplicaEntry {
            txid: row.get(0)?,
            facts: MempoolEntryFacts {
                vsize: nonnegative_integer_from_row(row.get(1)?, 1)?,
                fee_sats: nonnegative_integer_from_row(row.get(2)?, 2)?,
                entered_at_ms: nonnegative_integer_from_row(row.get(3)?, 3)?,
            },
        };
        digest
            .push(&entry)
            .map_err(|error| invalid(error.to_string(), active_cursor.clone()))?;
    }
    digest
        .finish()
        .map_err(|error| invalid(error.to_string(), active_cursor))
}

fn insert_or_replace_entry(
    transaction: &Transaction<'_>,
    generation: &Generation,
    txid: &str,
    facts: &MempoolEntryFacts,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO source_replica_membership (
            source_id, generation_id, txid, vsize, fee_sats, entered_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (source_id, generation_id, txid) DO UPDATE SET
            vsize = excluded.vsize,
            fee_sats = excluded.fee_sats,
            entered_at_ms = excluded.entered_at_ms",
        params![
            generation.source_id.as_str(),
            generation.generation_id,
            txid,
            to_sqlite_integer(facts.vsize, "vsize")?,
            to_sqlite_integer(facts.fee_sats, "fee_sats")?,
            to_sqlite_integer(facts.entered_at_ms, "entered_at_ms")?,
        ],
    )?;
    Ok(())
}

fn insert_staged_entry(
    transaction: &Transaction<'_>,
    generation: &Generation,
    entry: &SourceReplicaEntry,
) -> Result<bool, StoreError> {
    let inserted = transaction.execute(
        "INSERT INTO source_replica_membership (
            source_id, generation_id, txid, vsize, fee_sats, entered_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (source_id, generation_id, txid) DO NOTHING",
        params![
            generation.source_id.as_str(),
            generation.generation_id,
            entry.txid,
            to_sqlite_integer(entry.facts.vsize, "vsize")?,
            to_sqlite_integer(entry.facts.fee_sats, "fee_sats")?,
            to_sqlite_integer(entry.facts.entered_at_ms, "entered_at_ms")?,
        ],
    )?;
    Ok(inserted == 1)
}

fn conversion_from_sql(
    column: usize,
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(error))
}

fn invalid_data_from_sql(column: usize, message: &str) -> rusqlite::Error {
    conversion_from_sql(
        column,
        std::io::Error::new(std::io::ErrorKind::InvalidData, message.to_owned()),
    )
}
