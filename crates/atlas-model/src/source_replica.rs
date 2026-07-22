//! Wire and domain contract for RPC-authoritative source-state replication.

use std::fmt;
use std::str::FromStr;

use bitcoin::Txid;
use bitcoin::hashes::{Hash, HashEngine, sha256};
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{MAX_IDENTIFIER_BYTES, MAX_SAFE_JSON_INTEGER, MempoolEntryFacts, ModelError, SourceId};

pub const SOURCE_REPLICA_PROTOCOL_VERSION: u16 = 1;
pub const MAX_SOURCE_REPLICA_BODY_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_CHECKPOINT_ENTRIES: u64 = 1_000_000;
pub const MAX_CHECKPOINT_CHUNKS: u32 = 4_096;
pub const MAX_CHECKPOINT_CHUNK_ENTRIES: usize = 512;
pub const MAX_STATE_DELTA_MUTATIONS: usize = 4_096;

const DELTA_DIGEST_DOMAIN: &[u8] = b"mempool-atlas/source-replica/delta/v1";
const CHECKPOINT_DIGEST_DOMAIN: &[u8] = b"mempool-atlas/source-replica/checkpoint/v1";
const CHECKPOINT_CHUNK_DIGEST_DOMAIN: &[u8] = b"mempool-atlas/source-replica/checkpoint-chunk/v1";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SourceReplicaError {
    #[error("{field} must not be empty")]
    EmptyIdentifier { field: &'static str },
    #[error("{field} contains unsupported characters")]
    InvalidIdentifier { field: &'static str },
    #[error("{field} contains {found} bytes; maximum is {maximum}")]
    IdentifierTooLong {
        field: &'static str,
        found: usize,
        maximum: usize,
    },
    #[error("unsupported source replica protocol version {found}; expected {expected}")]
    UnsupportedProtocolVersion { found: u16, expected: u16 },
    #[error("{field} value {value} exceeds the exact JSON integer maximum {maximum}")]
    IntegerTooLarge {
        field: &'static str,
        value: u64,
        maximum: u64,
    },
    #[error("{field} must be at least one")]
    ZeroRevision { field: &'static str },
    #[error("invalid txid: {0}")]
    InvalidTxid(String),
    #[error("txid must use its canonical lowercase representation: {0}")]
    NonCanonicalTxid(String),
    #[error("{field} must be sorted by txid; {current} follows {previous}")]
    UnsortedTxids {
        field: &'static str,
        previous: String,
        current: String,
    },
    #[error("{field} contains duplicate txid {txid}")]
    DuplicateTxid { field: &'static str, txid: String },
    #[error("a state delta must contain at least one mutation")]
    EmptyDelta,
    #[error("target revision {target_revision} must be greater than base revision {base_revision}")]
    InvalidDeltaRevision {
        base_revision: u64,
        target_revision: u64,
    },
    #[error(
        "checkpoint target revision {target_revision} must be greater than replaced revision {replaced_revision} in the same epoch"
    )]
    CheckpointRevisionRegression {
        replaced_revision: u64,
        target_revision: u64,
    },
    #[error("a checkpoint chunk must contain at least one entry")]
    EmptyCheckpointChunk,
    #[error(
        "checkpoint entry/chunk bounds are inconsistent: {expected_entries} entries in {expected_chunks} chunks"
    )]
    InvalidCheckpointBounds {
        expected_entries: u64,
        expected_chunks: u32,
    },
    #[error("{field} contains {found} items; maximum is {maximum}")]
    LimitExceeded {
        field: &'static str,
        found: u64,
        maximum: u64,
    },
    #[error(
        "checkpoint progress exceeds its declaration: {received_entries}/{expected_entries} entries and {received_chunks}/{expected_chunks} chunks"
    )]
    InvalidCheckpointProgress {
        received_entries: u64,
        expected_entries: u64,
        received_chunks: u32,
        expected_chunks: u32,
    },
    #[error("checkpoint digest received {actual_entries} entries; expected {expected_entries}")]
    CheckpointDigestEntryCount {
        expected_entries: u64,
        actual_entries: u64,
    },
    #[error("{field} must be a lowercase SHA-256 digest")]
    InvalidDigest { field: &'static str },
    #[error("{field} does not match the canonical payload digest")]
    DigestMismatch { field: &'static str },
    #[error(transparent)]
    Model(#[from] ModelError),
}

macro_rules! identifier_type {
    ($name:ident, $field:literal) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, SourceReplicaError> {
                let value = value.into();
                validate_identifier($field, &value)?;
                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn validate(&self) -> Result<(), SourceReplicaError> {
                validate_identifier($field, &self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

identifier_type!(SourceEpochId, "epoch_id");
identifier_type!(CheckpointId, "checkpoint_id");

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplicaCursor {
    pub epoch_id: SourceEpochId,
    pub revision: u64,
}

impl ReplicaCursor {
    pub fn new(epoch_id: SourceEpochId, revision: u64) -> Result<Self, SourceReplicaError> {
        let cursor = Self { epoch_id, revision };
        cursor.validate()?;
        Ok(cursor)
    }

    pub fn validate(&self) -> Result<(), SourceReplicaError> {
        self.epoch_id.validate()?;
        validate_established_revision("revision", self.revision)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceReplicaRequest {
    pub protocol_version: u16,
    pub source_id: SourceId,
    pub epoch_id: SourceEpochId,
    pub command: SourceReplicaCommand,
}

impl SourceReplicaRequest {
    pub fn new(
        source_id: SourceId,
        epoch_id: SourceEpochId,
        command: SourceReplicaCommand,
    ) -> Result<Self, SourceReplicaError> {
        let request = Self {
            protocol_version: SOURCE_REPLICA_PROTOCOL_VERSION,
            source_id,
            epoch_id,
            command,
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<(), SourceReplicaError> {
        if self.protocol_version != SOURCE_REPLICA_PROTOCOL_VERSION {
            return Err(SourceReplicaError::UnsupportedProtocolVersion {
                found: self.protocol_version,
                expected: SOURCE_REPLICA_PROTOCOL_VERSION,
            });
        }
        self.source_id.validate()?;
        self.epoch_id.validate()?;
        self.command.validate()?;

        if let SourceReplicaCommand::CheckpointBegin(begin) = &self.command
            && let Some(replaces) = &begin.replaces
            && replaces.epoch_id == self.epoch_id
            && begin.target_revision <= replaces.revision
        {
            return Err(SourceReplicaError::CheckpointRevisionRegression {
                replaced_revision: replaces.revision,
                target_revision: begin.target_revision,
            });
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceReplicaCommand {
    Delta(StateDelta),
    Heartbeat(StateHeartbeat),
    CheckpointBegin(CheckpointBegin),
    CheckpointChunk(CheckpointChunk),
    CheckpointCommit(CheckpointCommit),
}

impl SourceReplicaCommand {
    pub fn validate(&self) -> Result<(), SourceReplicaError> {
        match self {
            Self::Delta(delta) => delta.validate(),
            Self::Heartbeat(heartbeat) => heartbeat.validate(),
            Self::CheckpointBegin(begin) => begin.validate(),
            Self::CheckpointChunk(chunk) => chunk.validate(),
            Self::CheckpointCommit(commit) => commit.validate(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StateDelta {
    pub base_revision: u64,
    pub target_revision: u64,
    pub state_observed_at_ms: u64,
    pub mutations: Vec<StateMutation>,
    pub content_sha256: String,
}

impl StateDelta {
    pub fn new(
        base_revision: u64,
        target_revision: u64,
        state_observed_at_ms: u64,
        mutations: Vec<StateMutation>,
    ) -> Result<Self, SourceReplicaError> {
        let content_sha256 = canonical_delta_sha256(
            base_revision,
            target_revision,
            state_observed_at_ms,
            &mutations,
        )?;
        let delta = Self {
            base_revision,
            target_revision,
            state_observed_at_ms,
            mutations,
            content_sha256,
        };
        delta.validate()?;
        Ok(delta)
    }

    pub fn validate(&self) -> Result<(), SourceReplicaError> {
        validate_delta_payload(
            self.base_revision,
            self.target_revision,
            self.state_observed_at_ms,
            &self.mutations,
        )?;
        validate_digest("content_sha256", &self.content_sha256)?;
        let expected = canonical_delta_sha256(
            self.base_revision,
            self.target_revision,
            self.state_observed_at_ms,
            &self.mutations,
        )?;
        if self.content_sha256 != expected {
            return Err(SourceReplicaError::DigestMismatch {
                field: "content_sha256",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "membership", rename_all = "snake_case")]
pub enum StateMutation {
    Absent {
        txid: String,
    },
    Present {
        txid: String,
        #[serde(flatten)]
        facts: MempoolEntryFacts,
    },
}

impl StateMutation {
    #[must_use]
    pub fn txid(&self) -> &str {
        match self {
            Self::Absent { txid } | Self::Present { txid, .. } => txid,
        }
    }

    pub fn validate(&self) -> Result<(), SourceReplicaError> {
        parse_canonical_txid(self.txid())?;
        if let Self::Present { facts, .. } = self {
            facts.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StateHeartbeat {
    pub revision: u64,
    pub state_observed_at_ms: u64,
}

impl StateHeartbeat {
    pub fn new(revision: u64, state_observed_at_ms: u64) -> Result<Self, SourceReplicaError> {
        let heartbeat = Self {
            revision,
            state_observed_at_ms,
        };
        heartbeat.validate()?;
        Ok(heartbeat)
    }

    pub fn validate(&self) -> Result<(), SourceReplicaError> {
        validate_established_revision("revision", self.revision)?;
        validate_json_integer("state_observed_at_ms", self.state_observed_at_ms)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CheckpointBegin {
    pub checkpoint_id: CheckpointId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes_checkpoint_id: Option<CheckpointId>,
    pub replaces: Option<ReplicaCursor>,
    pub target_revision: u64,
    pub state_observed_at_ms: u64,
    pub expected_entries: u64,
    pub expected_chunks: u32,
    pub content_sha256: String,
}

impl CheckpointBegin {
    pub fn new(
        checkpoint_id: CheckpointId,
        replaces: Option<ReplicaCursor>,
        target_revision: u64,
        state_observed_at_ms: u64,
        expected_chunks: u32,
        entries: &[SourceReplicaEntry],
    ) -> Result<Self, SourceReplicaError> {
        let expected_entries =
            u64::try_from(entries.len()).map_err(|_| SourceReplicaError::IntegerTooLarge {
                field: "expected_entries",
                value: u64::MAX,
                maximum: MAX_SAFE_JSON_INTEGER,
            })?;
        let content_sha256 = canonical_checkpoint_sha256(entries)?;
        let begin = Self {
            checkpoint_id,
            supersedes_checkpoint_id: None,
            replaces,
            target_revision,
            state_observed_at_ms,
            expected_entries,
            expected_chunks,
            content_sha256,
        };
        begin.validate()?;
        Ok(begin)
    }

    pub fn validate(&self) -> Result<(), SourceReplicaError> {
        self.checkpoint_id.validate()?;
        if let Some(checkpoint_id) = &self.supersedes_checkpoint_id {
            checkpoint_id.validate()?;
        }
        if let Some(cursor) = &self.replaces {
            cursor.validate()?;
        }
        validate_established_revision("target_revision", self.target_revision)?;
        validate_json_integer("state_observed_at_ms", self.state_observed_at_ms)?;
        validate_json_integer("expected_entries", self.expected_entries)?;
        validate_checkpoint_bounds(self.expected_entries, self.expected_chunks)?;
        validate_digest("content_sha256", &self.content_sha256)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CheckpointChunk {
    pub checkpoint_id: CheckpointId,
    pub chunk_index: u32,
    pub entries: Vec<SourceReplicaEntry>,
    pub content_sha256: String,
}

impl CheckpointChunk {
    pub fn new(
        checkpoint_id: CheckpointId,
        chunk_index: u32,
        entries: Vec<SourceReplicaEntry>,
    ) -> Result<Self, SourceReplicaError> {
        let content_sha256 =
            canonical_checkpoint_chunk_sha256(&checkpoint_id, chunk_index, &entries)?;
        let chunk = Self {
            checkpoint_id,
            chunk_index,
            entries,
            content_sha256,
        };
        chunk.validate()?;
        Ok(chunk)
    }

    pub fn validate(&self) -> Result<(), SourceReplicaError> {
        self.checkpoint_id.validate()?;
        if self.entries.is_empty() {
            return Err(SourceReplicaError::EmptyCheckpointChunk);
        }
        if self.entries.len() > MAX_CHECKPOINT_CHUNK_ENTRIES {
            return Err(SourceReplicaError::LimitExceeded {
                field: "entries",
                found: sequence_len("entries", self.entries.len())?,
                maximum: MAX_CHECKPOINT_CHUNK_ENTRIES as u64,
            });
        }
        validate_chunk_index(self.chunk_index)?;
        validate_checkpoint_entries(&self.entries)?;
        validate_digest("content_sha256", &self.content_sha256)?;
        let expected = canonical_checkpoint_chunk_sha256(
            &self.checkpoint_id,
            self.chunk_index,
            &self.entries,
        )?;
        if self.content_sha256 != expected {
            return Err(SourceReplicaError::DigestMismatch {
                field: "content_sha256",
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceReplicaEntry {
    pub txid: String,
    #[serde(flatten)]
    pub facts: MempoolEntryFacts,
}

impl SourceReplicaEntry {
    pub fn new(
        txid: impl Into<String>,
        facts: MempoolEntryFacts,
    ) -> Result<Self, SourceReplicaError> {
        let entry = Self {
            txid: txid.into(),
            facts,
        };
        entry.validate()?;
        Ok(entry)
    }

    pub fn validate(&self) -> Result<(), SourceReplicaError> {
        parse_canonical_txid(&self.txid)?;
        self.facts.validate()?;
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CheckpointCommit {
    pub checkpoint_id: CheckpointId,
    pub target_revision: u64,
    pub content_sha256: String,
}

impl CheckpointCommit {
    pub fn new(
        checkpoint_id: CheckpointId,
        target_revision: u64,
        content_sha256: impl Into<String>,
    ) -> Result<Self, SourceReplicaError> {
        let commit = Self {
            checkpoint_id,
            target_revision,
            content_sha256: content_sha256.into(),
        };
        commit.validate()?;
        Ok(commit)
    }

    pub fn validate(&self) -> Result<(), SourceReplicaError> {
        self.checkpoint_id.validate()?;
        validate_established_revision("target_revision", self.target_revision)?;
        validate_digest("content_sha256", &self.content_sha256)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CheckpointProgress {
    pub checkpoint_id: CheckpointId,
    pub target_cursor: ReplicaCursor,
    pub received_entries: u64,
    pub expected_entries: u64,
    pub received_chunks: u32,
    pub expected_chunks: u32,
}

impl CheckpointProgress {
    pub fn new(
        checkpoint_id: CheckpointId,
        target_cursor: ReplicaCursor,
        received_entries: u64,
        expected_entries: u64,
        received_chunks: u32,
        expected_chunks: u32,
    ) -> Result<Self, SourceReplicaError> {
        let progress = Self {
            checkpoint_id,
            target_cursor,
            received_entries,
            expected_entries,
            received_chunks,
            expected_chunks,
        };
        progress.validate()?;
        Ok(progress)
    }

    pub fn validate(&self) -> Result<(), SourceReplicaError> {
        self.checkpoint_id.validate()?;
        self.target_cursor.validate()?;
        validate_json_integer("received_entries", self.received_entries)?;
        validate_json_integer("expected_entries", self.expected_entries)?;
        validate_checkpoint_bounds(self.expected_entries, self.expected_chunks)?;
        if self.received_entries > self.expected_entries
            || self.received_chunks > self.expected_chunks
        {
            return Err(SourceReplicaError::InvalidCheckpointProgress {
                received_entries: self.received_entries,
                expected_entries: self.expected_entries,
                received_chunks: self.received_chunks,
                expected_chunks: self.expected_chunks,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SourceReplicaResponse {
    Applied {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_cursor: Option<ReplicaCursor>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<CheckpointProgress>,
    },
    Duplicate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_cursor: Option<ReplicaCursor>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<CheckpointProgress>,
    },
}

impl SourceReplicaResponse {
    #[must_use]
    pub fn applied(
        active_cursor: Option<ReplicaCursor>,
        progress: Option<CheckpointProgress>,
    ) -> Self {
        Self::Applied {
            active_cursor,
            progress,
        }
    }

    #[must_use]
    pub fn duplicate(
        active_cursor: Option<ReplicaCursor>,
        progress: Option<CheckpointProgress>,
    ) -> Self {
        Self::Duplicate {
            active_cursor,
            progress,
        }
    }

    #[must_use]
    pub const fn active_cursor(&self) -> Option<&ReplicaCursor> {
        match self {
            Self::Applied { active_cursor, .. } | Self::Duplicate { active_cursor, .. } => {
                active_cursor.as_ref()
            }
        }
    }

    #[must_use]
    pub const fn progress(&self) -> Option<&CheckpointProgress> {
        match self {
            Self::Applied { progress, .. } | Self::Duplicate { progress, .. } => progress.as_ref(),
        }
    }

    pub fn validate(&self) -> Result<(), SourceReplicaError> {
        if let Some(active_cursor) = self.active_cursor() {
            active_cursor.validate()?;
        }
        if let Some(progress) = self.progress() {
            progress.validate()?;
        }
        Ok(())
    }
}

pub struct CheckpointDigest {
    engine: sha256::HashEngine,
    expected_entries: u64,
    actual_entries: u64,
    previous_txid: Option<String>,
}

impl CheckpointDigest {
    pub fn new(expected_entries: u64) -> Result<Self, SourceReplicaError> {
        validate_json_integer("expected_entries", expected_entries)?;
        if expected_entries > MAX_CHECKPOINT_ENTRIES {
            return Err(SourceReplicaError::LimitExceeded {
                field: "expected_entries",
                found: expected_entries,
                maximum: MAX_CHECKPOINT_ENTRIES,
            });
        }

        let mut engine = digest_engine(CHECKPOINT_DIGEST_DOMAIN);
        write_u64(&mut engine, expected_entries);
        Ok(Self {
            engine,
            expected_entries,
            actual_entries: 0,
            previous_txid: None,
        })
    }

    pub fn push(&mut self, entry: &SourceReplicaEntry) -> Result<(), SourceReplicaError> {
        entry.validate()?;
        let next_count = self.actual_entries + 1;
        if next_count > self.expected_entries {
            return Err(SourceReplicaError::CheckpointDigestEntryCount {
                expected_entries: self.expected_entries,
                actual_entries: next_count,
            });
        }
        if let Some(previous_txid) = &self.previous_txid {
            if entry.txid == *previous_txid {
                return Err(SourceReplicaError::DuplicateTxid {
                    field: "entries",
                    txid: entry.txid.clone(),
                });
            }
            if entry.txid < *previous_txid {
                return Err(SourceReplicaError::UnsortedTxids {
                    field: "entries",
                    previous: previous_txid.clone(),
                    current: entry.txid.clone(),
                });
            }
        }

        write_checkpoint_entry(&mut self.engine, entry)?;
        self.previous_txid = Some(entry.txid.clone());
        self.actual_entries = next_count;
        Ok(())
    }

    pub fn finish(self) -> Result<String, SourceReplicaError> {
        if self.actual_entries != self.expected_entries {
            return Err(SourceReplicaError::CheckpointDigestEntryCount {
                expected_entries: self.expected_entries,
                actual_entries: self.actual_entries,
            });
        }
        Ok(sha256::Hash::from_engine(self.engine).to_string())
    }

    #[must_use]
    pub const fn expected_entries(&self) -> u64 {
        self.expected_entries
    }

    #[must_use]
    pub const fn actual_entries(&self) -> u64 {
        self.actual_entries
    }
}

pub fn canonical_delta_sha256(
    base_revision: u64,
    target_revision: u64,
    state_observed_at_ms: u64,
    mutations: &[StateMutation],
) -> Result<String, SourceReplicaError> {
    validate_delta_payload(
        base_revision,
        target_revision,
        state_observed_at_ms,
        mutations,
    )?;

    let mut engine = digest_engine(DELTA_DIGEST_DOMAIN);
    write_u64(&mut engine, base_revision);
    write_u64(&mut engine, target_revision);
    write_u64(&mut engine, state_observed_at_ms);
    write_u64(&mut engine, sequence_len("mutations", mutations.len())?);
    for mutation in mutations {
        match mutation {
            StateMutation::Absent { txid } => {
                engine.input(&[0]);
                write_txid(&mut engine, txid)?;
            }
            StateMutation::Present { txid, facts } => {
                engine.input(&[1]);
                write_txid(&mut engine, txid)?;
                write_facts(&mut engine, facts);
            }
        }
    }
    Ok(sha256::Hash::from_engine(engine).to_string())
}

pub fn canonical_checkpoint_sha256(
    entries: &[SourceReplicaEntry],
) -> Result<String, SourceReplicaError> {
    let mut digest = CheckpointDigest::new(sequence_len("expected_entries", entries.len())?)?;
    for entry in entries {
        digest.push(entry)?;
    }
    digest.finish()
}

pub fn canonical_checkpoint_chunk_sha256(
    checkpoint_id: &CheckpointId,
    chunk_index: u32,
    entries: &[SourceReplicaEntry],
) -> Result<String, SourceReplicaError> {
    checkpoint_id.validate()?;
    if entries.is_empty() {
        return Err(SourceReplicaError::EmptyCheckpointChunk);
    }
    if entries.len() > MAX_CHECKPOINT_CHUNK_ENTRIES {
        return Err(SourceReplicaError::LimitExceeded {
            field: "entries",
            found: sequence_len("entries", entries.len())?,
            maximum: MAX_CHECKPOINT_CHUNK_ENTRIES as u64,
        });
    }
    validate_chunk_index(chunk_index)?;
    validate_checkpoint_entries(entries)?;

    let mut engine = digest_engine(CHECKPOINT_CHUNK_DIGEST_DOMAIN);
    write_bytes(&mut engine, checkpoint_id.as_str().as_bytes())?;
    write_u32(&mut engine, chunk_index);
    write_u64(&mut engine, sequence_len("entries", entries.len())?);
    for entry in entries {
        write_checkpoint_entry(&mut engine, entry)?;
    }
    Ok(sha256::Hash::from_engine(engine).to_string())
}

fn validate_delta_payload(
    base_revision: u64,
    target_revision: u64,
    state_observed_at_ms: u64,
    mutations: &[StateMutation],
) -> Result<(), SourceReplicaError> {
    validate_json_integer("base_revision", base_revision)?;
    validate_json_integer("target_revision", target_revision)?;
    validate_json_integer("state_observed_at_ms", state_observed_at_ms)?;
    if target_revision <= base_revision {
        return Err(SourceReplicaError::InvalidDeltaRevision {
            base_revision,
            target_revision,
        });
    }
    if mutations.is_empty() {
        return Err(SourceReplicaError::EmptyDelta);
    }
    if mutations.len() > MAX_STATE_DELTA_MUTATIONS {
        return Err(SourceReplicaError::LimitExceeded {
            field: "mutations",
            found: sequence_len("mutations", mutations.len())?,
            maximum: MAX_STATE_DELTA_MUTATIONS as u64,
        });
    }
    for mutation in mutations {
        mutation.validate()?;
    }
    validate_sorted_txids("mutations", mutations.iter().map(StateMutation::txid))
}

fn validate_checkpoint_entries(entries: &[SourceReplicaEntry]) -> Result<(), SourceReplicaError> {
    let entry_count = sequence_len("entries", entries.len())?;
    if entry_count > MAX_CHECKPOINT_ENTRIES {
        return Err(SourceReplicaError::LimitExceeded {
            field: "entries",
            found: entry_count,
            maximum: MAX_CHECKPOINT_ENTRIES,
        });
    }
    for entry in entries {
        entry.validate()?;
    }
    validate_sorted_txids("entries", entries.iter().map(|entry| entry.txid.as_str()))
}

fn validate_sorted_txids<'a>(
    field: &'static str,
    txids: impl IntoIterator<Item = &'a str>,
) -> Result<(), SourceReplicaError> {
    let mut previous: Option<&str> = None;
    for txid in txids {
        parse_canonical_txid(txid)?;
        if let Some(previous_txid) = previous {
            if txid == previous_txid {
                return Err(SourceReplicaError::DuplicateTxid {
                    field,
                    txid: txid.to_owned(),
                });
            }
            if txid < previous_txid {
                return Err(SourceReplicaError::UnsortedTxids {
                    field,
                    previous: previous_txid.to_owned(),
                    current: txid.to_owned(),
                });
            }
        }
        previous = Some(txid);
    }
    Ok(())
}

fn validate_checkpoint_bounds(
    expected_entries: u64,
    expected_chunks: u32,
) -> Result<(), SourceReplicaError> {
    if expected_entries > MAX_CHECKPOINT_ENTRIES {
        return Err(SourceReplicaError::LimitExceeded {
            field: "expected_entries",
            found: expected_entries,
            maximum: MAX_CHECKPOINT_ENTRIES,
        });
    }
    if expected_chunks > MAX_CHECKPOINT_CHUNKS {
        return Err(SourceReplicaError::LimitExceeded {
            field: "expected_chunks",
            found: u64::from(expected_chunks),
            maximum: u64::from(MAX_CHECKPOINT_CHUNKS),
        });
    }
    let chunks = u64::from(expected_chunks);
    let chunk_capacity = chunks * MAX_CHECKPOINT_CHUNK_ENTRIES as u64;
    let valid = (expected_entries == 0 && expected_chunks == 0)
        || (expected_entries > 0
            && expected_chunks > 0
            && chunks <= expected_entries
            && expected_entries <= chunk_capacity);
    if !valid {
        return Err(SourceReplicaError::InvalidCheckpointBounds {
            expected_entries,
            expected_chunks,
        });
    }
    Ok(())
}

fn validate_chunk_index(chunk_index: u32) -> Result<(), SourceReplicaError> {
    if chunk_index >= MAX_CHECKPOINT_CHUNKS {
        return Err(SourceReplicaError::LimitExceeded {
            field: "chunk_index",
            found: u64::from(chunk_index),
            maximum: u64::from(MAX_CHECKPOINT_CHUNKS - 1),
        });
    }
    Ok(())
}

fn validate_json_integer(field: &'static str, value: u64) -> Result<(), SourceReplicaError> {
    if value > MAX_SAFE_JSON_INTEGER {
        return Err(SourceReplicaError::IntegerTooLarge {
            field,
            value,
            maximum: MAX_SAFE_JSON_INTEGER,
        });
    }
    Ok(())
}

fn validate_established_revision(
    field: &'static str,
    value: u64,
) -> Result<(), SourceReplicaError> {
    validate_json_integer(field, value)?;
    if value == 0 {
        return Err(SourceReplicaError::ZeroRevision { field });
    }
    Ok(())
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), SourceReplicaError> {
    if value.is_empty() {
        return Err(SourceReplicaError::EmptyIdentifier { field });
    }
    if value.len() > MAX_IDENTIFIER_BYTES {
        return Err(SourceReplicaError::IdentifierTooLong {
            field,
            found: value.len(),
            maximum: MAX_IDENTIFIER_BYTES,
        });
    }
    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
    {
        return Err(SourceReplicaError::InvalidIdentifier { field });
    }
    Ok(())
}

fn validate_digest(field: &'static str, value: &str) -> Result<(), SourceReplicaError> {
    let parsed =
        sha256::Hash::from_str(value).map_err(|_| SourceReplicaError::InvalidDigest { field })?;
    if parsed.to_string() != value {
        return Err(SourceReplicaError::InvalidDigest { field });
    }
    Ok(())
}

fn parse_canonical_txid(value: &str) -> Result<Txid, SourceReplicaError> {
    let txid =
        Txid::from_str(value).map_err(|_| SourceReplicaError::InvalidTxid(value.to_owned()))?;
    if txid.to_string() != value {
        return Err(SourceReplicaError::NonCanonicalTxid(value.to_owned()));
    }
    Ok(txid)
}

fn digest_engine(domain: &[u8]) -> sha256::HashEngine {
    let mut engine = sha256::Hash::engine();
    let domain_length = u32::try_from(domain.len()).expect("digest domains fit in u32");
    write_u32(&mut engine, domain_length);
    engine.input(domain);
    engine
}

fn write_checkpoint_entry(
    engine: &mut sha256::HashEngine,
    entry: &SourceReplicaEntry,
) -> Result<(), SourceReplicaError> {
    write_txid(engine, &entry.txid)?;
    write_facts(engine, &entry.facts);
    Ok(())
}

fn write_txid(engine: &mut sha256::HashEngine, value: &str) -> Result<(), SourceReplicaError> {
    let canonical = parse_canonical_txid(value)?.to_string();
    write_bytes(engine, canonical.as_bytes())
}

fn write_facts(engine: &mut sha256::HashEngine, facts: &MempoolEntryFacts) {
    write_u64(engine, facts.vsize);
    write_u64(engine, facts.fee_sats);
    write_u64(engine, facts.entered_at_ms);
}

fn write_bytes(engine: &mut sha256::HashEngine, value: &[u8]) -> Result<(), SourceReplicaError> {
    write_u64(engine, sequence_len("digest_field_length", value.len())?);
    engine.input(value);
    Ok(())
}

fn write_u64(engine: &mut sha256::HashEngine, value: u64) {
    engine.input(&value.to_be_bytes());
}

fn write_u32(engine: &mut sha256::HashEngine, value: u32) {
    engine.input(&value.to_be_bytes());
}

fn sequence_len(field: &'static str, value: usize) -> Result<u64, SourceReplicaError> {
    let value = u64::try_from(value).map_err(|_| SourceReplicaError::IntegerTooLarge {
        field,
        value: u64::MAX,
        maximum: MAX_SAFE_JSON_INTEGER,
    })?;
    validate_json_integer(field, value)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TXID_A: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const TXID_B: &str = "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f";
    const TXID_C: &str = "404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f";

    fn facts(seed: u64) -> MempoolEntryFacts {
        MempoolEntryFacts {
            vsize: 100 + seed,
            fee_sats: 1_000 + seed,
            entered_at_ms: 1_700_000_000_000 + seed,
        }
    }

    fn entry(txid: &str, seed: u64) -> SourceReplicaEntry {
        SourceReplicaEntry::new(txid, facts(seed)).expect("valid checkpoint entry")
    }

    #[test]
    fn identifiers_validate_on_construction_and_deserialization() {
        assert_eq!(
            SourceEpochId::new(""),
            Err(SourceReplicaError::EmptyIdentifier { field: "epoch_id" })
        );
        assert_eq!(
            CheckpointId::new("checkpoint/1"),
            Err(SourceReplicaError::InvalidIdentifier {
                field: "checkpoint_id"
            })
        );
        assert!(serde_json::from_str::<SourceEpochId>(r#""bad epoch""#).is_err());
        assert_eq!(
            serde_json::from_str::<CheckpointId>(r#""checkpoint-1""#)
                .expect("valid identifier")
                .as_str(),
            "checkpoint-1"
        );
    }

    #[test]
    fn source_epoch_id_enforces_identifier_byte_limit_on_every_input_path() {
        let at_limit = "a".repeat(MAX_IDENTIFIER_BYTES);
        let over_limit = "a".repeat(MAX_IDENTIFIER_BYTES + 1);

        assert_eq!(
            SourceEpochId::new(at_limit.clone()).expect("64-byte epoch ID"),
            serde_json::from_value(serde_json::Value::String(at_limit))
                .expect("deserialize 64-byte epoch ID")
        );
        assert_eq!(
            SourceEpochId::new(over_limit.clone()),
            Err(SourceReplicaError::IdentifierTooLong {
                field: "epoch_id",
                found: MAX_IDENTIFIER_BYTES + 1,
                maximum: MAX_IDENTIFIER_BYTES,
            })
        );
        assert_eq!(
            SourceEpochId(over_limit.clone()).validate(),
            Err(SourceReplicaError::IdentifierTooLong {
                field: "epoch_id",
                found: MAX_IDENTIFIER_BYTES + 1,
                maximum: MAX_IDENTIFIER_BYTES,
            })
        );
        let error = serde_json::from_value::<SourceEpochId>(serde_json::Value::String(over_limit))
            .expect_err("65-byte epoch ID must not deserialize");
        assert!(
            error
                .to_string()
                .contains("epoch_id contains 65 bytes; maximum is 64")
        );
    }

    #[test]
    fn checkpoint_id_enforces_identifier_byte_limit_on_every_input_path() {
        let at_limit = "a".repeat(MAX_IDENTIFIER_BYTES);
        let over_limit = "a".repeat(MAX_IDENTIFIER_BYTES + 1);

        assert_eq!(
            CheckpointId::new(at_limit.clone()).expect("64-byte checkpoint ID"),
            serde_json::from_value(serde_json::Value::String(at_limit))
                .expect("deserialize 64-byte checkpoint ID")
        );
        assert_eq!(
            CheckpointId::new(over_limit.clone()),
            Err(SourceReplicaError::IdentifierTooLong {
                field: "checkpoint_id",
                found: MAX_IDENTIFIER_BYTES + 1,
                maximum: MAX_IDENTIFIER_BYTES,
            })
        );
        assert_eq!(
            CheckpointId(over_limit.clone()).validate(),
            Err(SourceReplicaError::IdentifierTooLong {
                field: "checkpoint_id",
                found: MAX_IDENTIFIER_BYTES + 1,
                maximum: MAX_IDENTIFIER_BYTES,
            })
        );
        let error = serde_json::from_value::<CheckpointId>(serde_json::Value::String(over_limit))
            .expect_err("65-byte checkpoint ID must not deserialize");
        assert!(
            error
                .to_string()
                .contains("checkpoint_id contains 65 bytes; maximum is 64")
        );
    }

    #[test]
    fn request_uses_an_independent_tagged_protocol() {
        let request = SourceReplicaRequest::new(
            SourceId::new("core").expect("source"),
            SourceEpochId::new("epoch-1").expect("epoch"),
            SourceReplicaCommand::Heartbeat(
                StateHeartbeat::new(7, 1_700_000_000_000).expect("heartbeat"),
            ),
        )
        .expect("request");
        let json = serde_json::to_value(&request).expect("serialize request");

        assert_eq!(json["protocol_version"], SOURCE_REPLICA_PROTOCOL_VERSION);
        assert_eq!(json["command"]["kind"], "heartbeat");
        assert_eq!(json["command"]["revision"], 7);

        let mut unsupported = request;
        unsupported.protocol_version = SOURCE_REPLICA_PROTOCOL_VERSION + 1;
        assert_eq!(
            unsupported.validate(),
            Err(SourceReplicaError::UnsupportedProtocolVersion {
                found: SOURCE_REPLICA_PROTOCOL_VERSION + 1,
                expected: SOURCE_REPLICA_PROTOCOL_VERSION,
            })
        );
    }

    #[test]
    fn deltas_require_advancing_revisions_and_sorted_unique_txids() {
        let descending = vec![
            StateMutation::Absent {
                txid: TXID_B.to_owned(),
            },
            StateMutation::Absent {
                txid: TXID_A.to_owned(),
            },
        ];
        assert!(matches!(
            StateDelta::new(1, 2, 100, descending),
            Err(SourceReplicaError::UnsortedTxids { .. })
        ));

        let duplicate = vec![
            StateMutation::Absent {
                txid: TXID_A.to_owned(),
            },
            StateMutation::Present {
                txid: TXID_A.to_owned(),
                facts: facts(1),
            },
        ];
        assert!(matches!(
            StateDelta::new(1, 2, 100, duplicate),
            Err(SourceReplicaError::DuplicateTxid { .. })
        ));
        assert!(matches!(
            StateDelta::new(
                2,
                2,
                100,
                vec![StateMutation::Absent {
                    txid: TXID_A.to_owned()
                }]
            ),
            Err(SourceReplicaError::InvalidDeltaRevision { .. })
        ));
    }

    #[test]
    fn delta_digest_is_canonical_and_binds_metadata_and_membership() {
        let mutations = vec![
            StateMutation::Absent {
                txid: TXID_A.to_owned(),
            },
            StateMutation::Present {
                txid: TXID_B.to_owned(),
                facts: facts(2),
            },
        ];
        let delta = StateDelta::new(4, 9, 123, mutations).expect("delta");

        assert_eq!(delta.content_sha256.len(), 64);
        assert_eq!(
            delta.content_sha256,
            "eb7227bb5dcc6ac9a925e130de6c13384931d8fd0eb78d55631a962854de57ca"
        );
        assert_eq!(
            delta.content_sha256,
            canonical_delta_sha256(
                delta.base_revision,
                delta.target_revision,
                delta.state_observed_at_ms,
                &delta.mutations
            )
            .expect("digest")
        );

        let mut tampered = delta.clone();
        tampered.target_revision += 1;
        assert_eq!(
            tampered.validate(),
            Err(SourceReplicaError::DigestMismatch {
                field: "content_sha256"
            })
        );
    }

    #[test]
    fn full_checkpoint_digest_is_independent_of_chunk_partitioning() {
        let checkpoint_id = CheckpointId::new("cp-1").expect("checkpoint");
        let entries = vec![entry(TXID_A, 1), entry(TXID_B, 2), entry(TXID_C, 3)];
        let digest = canonical_checkpoint_sha256(&entries).expect("checkpoint digest");
        assert_eq!(
            digest,
            "e853ea9122f2143da739a47c3943b76fc494dc5159c398518d114f1522a0f552"
        );

        let first_chunk = CheckpointChunk::new(checkpoint_id.clone(), 0, entries[..1].to_vec())
            .expect("first chunk");
        let second_chunk = CheckpointChunk::new(checkpoint_id.clone(), 1, entries[1..].to_vec())
            .expect("second chunk");
        let reassembled = first_chunk
            .entries
            .iter()
            .chain(&second_chunk.entries)
            .cloned()
            .collect::<Vec<_>>();

        let mut streaming = CheckpointDigest::new(3).expect("streaming digest");
        for entry in first_chunk.entries.iter().chain(&second_chunk.entries) {
            streaming.push(entry).expect("stream entry");
        }

        assert_eq!(
            digest,
            canonical_checkpoint_sha256(&reassembled).expect("reassembled digest")
        );
        assert_eq!(digest, streaming.finish().expect("streaming digest"));
        assert_ne!(first_chunk.content_sha256, second_chunk.content_sha256);

        let one_chunk = CheckpointBegin::new(checkpoint_id.clone(), None, 11, 222, 1, &entries)
            .expect("one-chunk declaration");
        let two_chunks = CheckpointBegin::new(checkpoint_id, None, 11, 222, 2, &entries)
            .expect("two-chunk declaration");
        assert_eq!(one_chunk.content_sha256, two_chunks.content_sha256);
        let different_metadata = CheckpointBegin::new(
            CheckpointId::new("cp-other").expect("checkpoint"),
            None,
            12,
            333,
            2,
            &entries,
        )
        .expect("different metadata");
        assert_eq!(one_chunk.content_sha256, different_metadata.content_sha256);
    }

    #[test]
    fn streaming_checkpoint_digest_enforces_declared_count_and_global_order() {
        let mut missing = CheckpointDigest::new(2).expect("digest");
        missing.push(&entry(TXID_A, 1)).expect("first entry");
        assert_eq!(
            missing.finish(),
            Err(SourceReplicaError::CheckpointDigestEntryCount {
                expected_entries: 2,
                actual_entries: 1,
            })
        );

        let mut unordered = CheckpointDigest::new(2).expect("digest");
        unordered.push(&entry(TXID_B, 2)).expect("first entry");
        assert!(matches!(
            unordered.push(&entry(TXID_A, 1)),
            Err(SourceReplicaError::UnsortedTxids { .. })
        ));

        let empty = CheckpointDigest::new(0)
            .expect("empty digest")
            .finish()
            .expect("empty digest");
        assert_eq!(
            empty,
            canonical_checkpoint_sha256(&[]).expect("slice digest")
        );
    }

    #[test]
    fn empty_checkpoint_has_a_canonical_digest_and_zero_chunks() {
        let checkpoint_id = CheckpointId::new("empty").expect("checkpoint");
        let begin = CheckpointBegin::new(checkpoint_id.clone(), None, 1, 0, 0, &[])
            .expect("empty checkpoint");
        assert_eq!(begin.expected_entries, 0);
        assert_eq!(begin.expected_chunks, 0);
        assert_eq!(begin.content_sha256.len(), 64);
        assert_eq!(
            begin.content_sha256,
            canonical_checkpoint_sha256(&[]).expect("empty checkpoint digest")
        );

        assert!(matches!(
            CheckpointBegin::new(checkpoint_id, None, 1, 0, 1, &[]),
            Err(SourceReplicaError::InvalidCheckpointBounds { .. })
        ));
    }

    #[test]
    fn checkpoint_chunks_are_nonempty_and_bind_index_in_their_digest() {
        let checkpoint_id = CheckpointId::new("cp-2").expect("checkpoint");
        assert_eq!(
            CheckpointChunk::new(checkpoint_id.clone(), 0, vec![]),
            Err(SourceReplicaError::EmptyCheckpointChunk)
        );

        let entries = vec![entry(TXID_A, 1)];
        let first =
            CheckpointChunk::new(checkpoint_id.clone(), 0, entries.clone()).expect("first chunk");
        let second = CheckpointChunk::new(checkpoint_id, 1, entries).expect("second chunk");
        assert_eq!(
            first.content_sha256,
            "52ca72b1ce42fbadc90fd670d89cfbb6c1bc8987da34c0bbfeff8bc4e095e8bd"
        );
        assert_ne!(first.content_sha256, second.content_sha256);
    }

    #[test]
    fn delta_and_checkpoint_limits_are_enforced() {
        let too_many_mutations = (0..=MAX_STATE_DELTA_MUTATIONS)
            .map(|index| StateMutation::Absent {
                txid: format!("{index:064x}"),
            })
            .collect();
        assert_eq!(
            StateDelta::new(0, 1, 1, too_many_mutations),
            Err(SourceReplicaError::LimitExceeded {
                field: "mutations",
                found: MAX_STATE_DELTA_MUTATIONS as u64 + 1,
                maximum: MAX_STATE_DELTA_MUTATIONS as u64,
            })
        );

        let digest = "00".repeat(32);
        let too_many_entries = CheckpointBegin {
            checkpoint_id: CheckpointId::new("too-many-entries").expect("checkpoint"),
            supersedes_checkpoint_id: None,
            replaces: None,
            target_revision: 1,
            state_observed_at_ms: 1,
            expected_entries: MAX_CHECKPOINT_ENTRIES + 1,
            expected_chunks: MAX_CHECKPOINT_CHUNKS,
            content_sha256: digest.clone(),
        };
        assert!(matches!(
            too_many_entries.validate(),
            Err(SourceReplicaError::LimitExceeded {
                field: "expected_entries",
                ..
            })
        ));

        let impossible_partition = CheckpointBegin {
            checkpoint_id: CheckpointId::new("impossible-partition").expect("checkpoint"),
            supersedes_checkpoint_id: None,
            replaces: None,
            target_revision: 1,
            state_observed_at_ms: 1,
            expected_entries: MAX_CHECKPOINT_CHUNK_ENTRIES as u64 + 1,
            expected_chunks: 1,
            content_sha256: digest,
        };
        assert!(matches!(
            impossible_partition.validate(),
            Err(SourceReplicaError::InvalidCheckpointBounds { .. })
        ));

        assert!(matches!(
            CheckpointChunk::new(
                CheckpointId::new("bad-index").expect("checkpoint"),
                MAX_CHECKPOINT_CHUNKS,
                vec![entry(TXID_A, 1)]
            ),
            Err(SourceReplicaError::LimitExceeded {
                field: "chunk_index",
                ..
            })
        ));
    }

    #[test]
    fn wire_numbers_are_limited_to_exact_json_integers() {
        assert!(StateHeartbeat::new(MAX_SAFE_JSON_INTEGER, MAX_SAFE_JSON_INTEGER).is_ok());
        assert_eq!(
            StateHeartbeat::new(MAX_SAFE_JSON_INTEGER + 1, 0),
            Err(SourceReplicaError::IntegerTooLarge {
                field: "revision",
                value: MAX_SAFE_JSON_INTEGER + 1,
                maximum: MAX_SAFE_JSON_INTEGER,
            })
        );
        assert!(matches!(
            SourceReplicaEntry::new(
                TXID_A,
                MempoolEntryFacts {
                    vsize: 1,
                    fee_sats: MAX_SAFE_JSON_INTEGER + 1,
                    entered_at_ms: 0,
                }
            ),
            Err(SourceReplicaError::Model(
                ModelError::MempoolEntryFactTooLarge { .. }
            ))
        ));
    }

    #[test]
    fn revision_zero_is_reserved_for_an_unestablished_replica() {
        let epoch = SourceEpochId::new("epoch-zero").expect("epoch");
        assert_eq!(
            ReplicaCursor::new(epoch, 0),
            Err(SourceReplicaError::ZeroRevision { field: "revision" })
        );
        assert_eq!(
            StateHeartbeat::new(0, 1),
            Err(SourceReplicaError::ZeroRevision { field: "revision" })
        );
        assert!(matches!(
            CheckpointBegin::new(
                CheckpointId::new("zero-begin").expect("checkpoint"),
                None,
                0,
                1,
                0,
                &[]
            ),
            Err(SourceReplicaError::ZeroRevision {
                field: "target_revision"
            })
        ));
        assert!(
            StateDelta::new(
                0,
                1,
                1,
                vec![StateMutation::Absent {
                    txid: TXID_A.to_owned()
                }]
            )
            .is_ok()
        );
    }

    #[test]
    fn txids_and_digests_must_use_canonical_lowercase_hex() {
        assert!(matches!(
            SourceReplicaEntry::new(TXID_A.to_uppercase(), facts(1)),
            Err(SourceReplicaError::NonCanonicalTxid(_))
        ));

        let delta = StateDelta::new(
            0,
            1,
            1,
            vec![StateMutation::Absent {
                txid: TXID_A.to_owned(),
            }],
        )
        .expect("delta");
        let mut uppercase_digest = delta;
        uppercase_digest.content_sha256 = uppercase_digest.content_sha256.to_uppercase();
        assert_eq!(
            uppercase_digest.validate(),
            Err(SourceReplicaError::InvalidDigest {
                field: "content_sha256"
            })
        );
    }

    #[test]
    fn checkpoint_cannot_regress_within_an_epoch() {
        let epoch_id = SourceEpochId::new("epoch-1").expect("epoch");
        let request = SourceReplicaRequest::new(
            SourceId::new("core").expect("source"),
            epoch_id.clone(),
            SourceReplicaCommand::CheckpointBegin(
                CheckpointBegin::new(
                    CheckpointId::new("cp-3").expect("checkpoint"),
                    Some(ReplicaCursor::new(epoch_id.clone(), 8).expect("cursor")),
                    7,
                    100,
                    0,
                    &[],
                )
                .expect("begin"),
            ),
        );
        assert_eq!(
            request,
            Err(SourceReplicaError::CheckpointRevisionRegression {
                replaced_revision: 8,
                target_revision: 7,
            })
        );

        let same_revision = SourceReplicaRequest::new(
            SourceId::new("core").expect("source"),
            epoch_id.clone(),
            SourceReplicaCommand::CheckpointBegin(
                CheckpointBegin::new(
                    CheckpointId::new("cp-same").expect("checkpoint"),
                    Some(ReplicaCursor::new(epoch_id, 8).expect("cursor")),
                    8,
                    101,
                    0,
                    &[],
                )
                .expect("begin"),
            ),
        );
        assert_eq!(
            same_revision,
            Err(SourceReplicaError::CheckpointRevisionRegression {
                replaced_revision: 8,
                target_revision: 8,
            })
        );
    }

    #[test]
    fn responses_are_tagged_and_report_optional_checkpoint_progress() {
        let cursor =
            ReplicaCursor::new(SourceEpochId::new("epoch-1").expect("epoch"), 12).expect("cursor");
        let progress = CheckpointProgress::new(
            CheckpointId::new("cp-4").expect("checkpoint"),
            cursor.clone(),
            10,
            20,
            1,
            2,
        )
        .expect("progress");
        let response = SourceReplicaResponse::applied(Some(cursor), Some(progress));
        response.validate().expect("response");

        let json = serde_json::to_value(&response).expect("serialize response");
        assert_eq!(json["status"], "applied");
        assert_eq!(json["active_cursor"]["revision"], 12);
        assert_eq!(json["progress"]["target_cursor"]["revision"], 12);
        assert_eq!(json["progress"]["received_chunks"], 1);

        let duplicate: SourceReplicaResponse = serde_json::from_value(serde_json::json!({
            "status": "duplicate",
            "active_cursor": null
        }))
        .expect("deserialize duplicate");
        assert!(matches!(
            duplicate,
            SourceReplicaResponse::Duplicate { progress: None, .. }
        ));
    }
}
