//! Shared Mempool Atlas domain and wire types.

mod source_replica;
mod summary;

use std::fmt;
use std::str::FromStr;

use bitcoin::{Txid, Wtxid};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use source_replica::*;
pub use summary::*;

pub const SCHEMA_VERSION: u16 = 3;
pub const MAX_INGEST_BATCH_EVENTS: usize = 512;
pub const MAX_INGEST_BATCH_BODY_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_SAFE_JSON_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ModelError {
    #[error("{field} must not be empty")]
    EmptyField { field: &'static str },
    #[error("{field} contains unsupported characters")]
    InvalidIdentifier { field: &'static str },
    #[error("invalid txid: {0}")]
    InvalidTxid(String),
    #[error("invalid wtxid: {0}")]
    InvalidWtxid(String),
    #[error("invalid package hash: {0}")]
    InvalidPackageHash(String),
    #[error("mempool entry vsize must be greater than zero")]
    ZeroVsize,
    #[error("mempool entry {field} value {value} exceeds the exact JSON integer maximum {maximum}")]
    MempoolEntryFactTooLarge {
        field: &'static str,
        value: u64,
        maximum: u64,
    },
    #[error("unsupported schema version {found}; expected {expected}")]
    UnsupportedSchemaVersion { found: u16, expected: u16 },
    #[error("event_id does not match source, session, and local sequence")]
    EventIdMismatch,
    #[error("ingest batch must contain at least one event")]
    EmptyIngestBatch,
    #[error("ingest batch contains {found} events; maximum is {maximum}")]
    IngestBatchTooLarge { found: usize, maximum: usize },
    #[error("ingest batch mixes source {found} with source {expected}")]
    IngestBatchSourceMismatch { expected: String, found: String },
    #[error("filter facet {facet} must select at least one value")]
    EmptyFilterFacet { facet: &'static str },
    #[error("taxonomy filter {taxonomy} must select at least one verdict")]
    EmptyTaxonomyFilter { taxonomy: String },
    #[error("taxonomy filter {taxonomy} appears more than once")]
    DuplicateTaxonomyFilter { taxonomy: String },
    #[error("unknown taxonomy {key}")]
    UnknownTaxonomy { key: String },
    #[error("unknown verdict {verdict} for taxonomy {taxonomy}")]
    UnknownTaxonomyVerdict { taxonomy: String, verdict: String },
    #[error("filter bound {field} must be a finite, non-negative number")]
    InvalidFilterBound { field: &'static str },
    #[error("feerate_min must not exceed feerate_max")]
    InvertedFeerateBounds,
    #[error("unknown {facet} filter value {value}")]
    UnknownFilterValue { facet: &'static str, value: String },
    #[error("unknown detail selection {value}; expected ecdf or joint")]
    UnknownDetailSelection { value: String },
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SourceId(String);

impl SourceId {
    pub fn new(value: impl Into<String>) -> Result<Self, ModelError> {
        let value = value.into();
        ensure_nonempty("source_id", &value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct SourceSessionId(String);

impl SourceSessionId {
    pub fn new(value: impl Into<String>) -> Result<Self, ModelError> {
        let value = value.into();
        ensure_nonempty("source_session_id", &value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SourceSessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NormalizedEvent {
    pub schema_version: u16,
    pub event_id: String,
    pub source_id: SourceId,
    pub source_session_id: SourceSessionId,
    pub local_sequence: u64,
    pub observed_at_ms: u64,
    pub received_at_ms: u64,
    pub evidence: Evidence,
}

impl NormalizedEvent {
    pub fn new(
        source_id: SourceId,
        source_session_id: SourceSessionId,
        local_sequence: u64,
        observed_at_ms: u64,
        received_at_ms: u64,
        evidence: Evidence,
    ) -> Result<Self, ModelError> {
        evidence.validate()?;
        let event_id = format!("{source_id}/{source_session_id}/{local_sequence}");
        Ok(Self {
            schema_version: SCHEMA_VERSION,
            event_id,
            source_id,
            source_session_id,
            local_sequence,
            observed_at_ms,
            received_at_ms,
            evidence,
        })
    }

    pub fn validate(&self) -> Result<(), ModelError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ModelError::UnsupportedSchemaVersion {
                found: self.schema_version,
                expected: SCHEMA_VERSION,
            });
        }
        ensure_nonempty("event_id", &self.event_id)?;
        ensure_nonempty("source_id", self.source_id.as_str())?;
        ensure_nonempty("source_session_id", self.source_session_id.as_str())?;
        let expected_event_id = format!(
            "{}/{}/{}",
            self.source_id, self.source_session_id, self.local_sequence
        );
        if self.event_id != expected_event_id {
            return Err(ModelError::EventIdMismatch);
        }
        self.evidence.validate()
    }

    #[must_use]
    pub fn membership_mutations(&self) -> Vec<MembershipMutation> {
        match &self.evidence {
            Evidence::MempoolAdded { txid } => vec![MembershipMutation::pending(txid)],
            Evidence::MempoolReconciled { txid, membership } => match membership {
                ReconciledMembership::Absent => vec![MembershipMutation::absent(txid)],
                ReconciledMembership::Present { facts } => {
                    vec![MembershipMutation::available(txid, facts.clone())]
                }
            },
            Evidence::MempoolRemoved { txid, .. } => vec![MembershipMutation::absent(txid)],
            Evidence::MempoolReplaced {
                replaced_txid,
                replacement,
            } => {
                let mut mutations = vec![MembershipMutation::absent(replaced_txid)];
                if let Replacement::Transaction { txid } = replacement {
                    mutations.push(MembershipMutation::pending(txid));
                }
                mutations
            }
            Evidence::MempoolRejected { .. }
            | Evidence::P2pTransaction { .. }
            | Evidence::CaptureGap { .. } => Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Evidence {
    MempoolAdded {
        txid: String,
    },
    MempoolReconciled {
        txid: String,
        membership: ReconciledMembership,
    },
    MempoolRemoved {
        txid: String,
        reason: Option<String>,
    },
    MempoolRejected {
        txid: String,
        reason: String,
    },
    MempoolReplaced {
        replaced_txid: String,
        replacement: Replacement,
    },
    P2pTransaction {
        txid: String,
        wtxid: String,
        raw_transaction_hex: Option<String>,
        peer_id: Option<u64>,
        inbound: Option<bool>,
    },
    CaptureGap {
        input: String,
        reason: String,
        certainty: CaptureGapCertainty,
    },
}

impl Evidence {
    pub fn validate(&self) -> Result<(), ModelError> {
        match self {
            Self::MempoolAdded { txid }
            | Self::MempoolRemoved { txid, .. }
            | Self::MempoolRejected { txid, .. } => validate_txid(txid),
            Self::MempoolReconciled { txid, membership } => {
                validate_txid(txid)?;
                membership.validate()
            }
            Self::MempoolReplaced {
                replaced_txid,
                replacement,
            } => {
                validate_txid(replaced_txid)?;
                replacement.validate()
            }
            Self::P2pTransaction { txid, wtxid, .. } => {
                validate_txid(txid)?;
                validate_wtxid(wtxid)
            }
            Self::CaptureGap { input, reason, .. } => {
                ensure_nonempty("input", input)?;
                ensure_nonempty("reason", reason)
            }
        }
    }

    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::MempoolAdded { .. } => "mempool_added",
            Self::MempoolReconciled { .. } => "mempool_reconciled",
            Self::MempoolRemoved { .. } => "mempool_removed",
            Self::MempoolRejected { .. } => "mempool_rejected",
            Self::MempoolReplaced { .. } => "mempool_replaced",
            Self::P2pTransaction { .. } => "p2p_transaction",
            Self::CaptureGap { .. } => "capture_gap",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MempoolEntryFacts {
    pub vsize: u64,
    pub fee_sats: u64,
    pub entered_at_ms: u64,
}

impl MempoolEntryFacts {
    pub fn validate(&self) -> Result<(), ModelError> {
        if self.vsize == 0 {
            return Err(ModelError::ZeroVsize);
        }
        for (field, value) in [
            ("vsize", self.vsize),
            ("fee_sats", self.fee_sats),
            ("entered_at_ms", self.entered_at_ms),
        ] {
            if value > MAX_SAFE_JSON_INTEGER {
                return Err(ModelError::MempoolEntryFactTooLarge {
                    field,
                    value,
                    maximum: MAX_SAFE_JSON_INTEGER,
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReconciledMembership {
    Absent,
    Present {
        #[serde(flatten)]
        facts: MempoolEntryFacts,
    },
}

impl ReconciledMembership {
    pub fn validate(&self) -> Result<(), ModelError> {
        match self {
            Self::Absent => Ok(()),
            Self::Present { facts } => facts.validate(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureGapCertainty {
    PossibleLoss,
    KnownLoss,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Replacement {
    Transaction { txid: String },
    PackageHash { hash: String },
}

impl Replacement {
    pub fn validate(&self) -> Result<(), ModelError> {
        match self {
            Self::Transaction { txid } => validate_txid(txid),
            Self::PackageHash { hash } => validate_package_hash(hash),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "membership", rename_all = "snake_case")]
pub enum MembershipMutation {
    Absent {
        txid: String,
    },
    Present {
        txid: String,
        facts: Option<MempoolEntryFacts>,
    },
}

impl MembershipMutation {
    fn pending(txid: &str) -> Self {
        Self::Present {
            txid: txid.to_owned(),
            facts: None,
        }
    }

    fn available(txid: &str, facts: MempoolEntryFacts) -> Self {
        Self::Present {
            txid: txid.to_owned(),
            facts: Some(facts),
        }
    }

    fn absent(txid: &str) -> Self {
        Self::Absent {
            txid: txid.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MempoolEntry {
    pub txid: String,
    pub facts: MempoolEntryFactsStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MempoolEntryFactsStatus {
    AwaitingRpc,
    Available {
        #[serde(flatten)]
        facts: MempoolEntryFacts,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MempoolSnapshot {
    pub source_id: SourceId,
    pub health: SourceHealth,
    pub memberships: Vec<MempoolEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceHealth {
    /// Exact active SourceReplica epoch and revision represented by the read.
    pub state_cursor: ReplicaCursor,
    /// Agent completion time for the RPC observation represented by the
    /// active SourceReplica generation.
    pub state_observed_at_ms: u64,
    pub capture: CaptureStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CaptureStatus {
    /// Peer evidence is not being collected for this source. This makes no
    /// statement about the completeness of its history.
    NotCollected,
    NoReportedGaps,
    ContainsGaps {
        first_gap_at_ms: u64,
        latest_gap_at_ms: u64,
        marker_count: u64,
        strongest_certainty: CaptureGapCertainty,
        latest_input: String,
        latest_reason: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestStatus {
    Applied,
    Duplicate,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IngestResponse {
    pub event_id: String,
    pub status: IngestStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IngestBatchRequest {
    pub events: Vec<NormalizedEvent>,
}

impl IngestBatchRequest {
    pub fn validate(&self) -> Result<(), ModelError> {
        let Some(first) = self.events.first() else {
            return Err(ModelError::EmptyIngestBatch);
        };
        if self.events.len() > MAX_INGEST_BATCH_EVENTS {
            return Err(ModelError::IngestBatchTooLarge {
                found: self.events.len(),
                maximum: MAX_INGEST_BATCH_EVENTS,
            });
        }

        for event in &self.events {
            event.validate()?;
            if event.source_id != first.source_id {
                return Err(ModelError::IngestBatchSourceMismatch {
                    expected: first.source_id.to_string(),
                    found: event.source_id.to_string(),
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IngestBatchResponse {
    pub acknowledgements: Vec<IngestResponse>,
}

fn ensure_nonempty(field: &'static str, value: &str) -> Result<(), ModelError> {
    if value.trim().is_empty() {
        return Err(ModelError::EmptyField { field });
    }
    if matches!(field, "source_id" | "source_session_id")
        && !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
    {
        return Err(ModelError::InvalidIdentifier { field });
    }
    Ok(())
}

fn validate_txid(value: &str) -> Result<(), ModelError> {
    Txid::from_str(value)
        .map(|_| ())
        .map_err(|_| ModelError::InvalidTxid(value.to_owned()))
}

fn validate_wtxid(value: &str) -> Result<(), ModelError> {
    Wtxid::from_str(value)
        .map(|_| ())
        .map_err(|_| ModelError::InvalidWtxid(value.to_owned()))
}

fn validate_package_hash(value: &str) -> Result<(), ModelError> {
    if value.len() == 64 && value.chars().all(|character| character.is_ascii_hexdigit()) {
        return Ok(());
    }
    Err(ModelError::InvalidPackageHash(value.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TXID_A: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const TXID_B: &str = "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f";

    fn facts() -> MempoolEntryFacts {
        MempoolEntryFacts {
            vsize: 141,
            fee_sats: 1_200,
            entered_at_ms: 1_721_234_000_000,
        }
    }

    fn event(evidence: Evidence) -> NormalizedEvent {
        event_for("source-a", "session-a", 7, evidence)
    }

    fn event_for(
        source_id: &str,
        source_session_id: &str,
        local_sequence: u64,
        evidence: Evidence,
    ) -> NormalizedEvent {
        NormalizedEvent::new(
            SourceId::new(source_id).expect("source"),
            SourceSessionId::new(source_session_id).expect("session"),
            local_sequence,
            1_000,
            1_001,
            evidence,
        )
        .expect("event")
    }

    #[test]
    fn event_id_is_source_session_and_sequence() {
        let event = event(Evidence::MempoolAdded {
            txid: TXID_A.to_owned(),
        });
        assert_eq!(event.event_id, "source-a/session-a/7");
    }

    #[test]
    fn ingest_batch_requires_events() {
        assert_eq!(
            IngestBatchRequest { events: Vec::new() }.validate(),
            Err(ModelError::EmptyIngestBatch)
        );
    }

    #[test]
    fn ingest_batch_enforces_event_count_bound() {
        let event = event(Evidence::MempoolAdded {
            txid: TXID_A.to_owned(),
        });
        let request = IngestBatchRequest {
            events: vec![event; MAX_INGEST_BATCH_EVENTS + 1],
        };

        assert_eq!(
            request.validate(),
            Err(ModelError::IngestBatchTooLarge {
                found: MAX_INGEST_BATCH_EVENTS + 1,
                maximum: MAX_INGEST_BATCH_EVENTS,
            })
        );
    }

    #[test]
    fn ingest_batch_accepts_multiple_sessions_for_one_source() {
        let request = IngestBatchRequest {
            events: vec![
                event_for(
                    "source-a",
                    "session-a",
                    1,
                    Evidence::MempoolAdded {
                        txid: TXID_A.to_owned(),
                    },
                ),
                event_for(
                    "source-a",
                    "session-b",
                    1,
                    Evidence::MempoolAdded {
                        txid: TXID_B.to_owned(),
                    },
                ),
            ],
        };

        assert_eq!(request.validate(), Ok(()));
    }

    #[test]
    fn ingest_batch_rejects_mixed_sources_and_invalid_events() {
        let valid = event_for(
            "source-a",
            "session-a",
            1,
            Evidence::MempoolAdded {
                txid: TXID_A.to_owned(),
            },
        );
        let other_source = event_for(
            "source-b",
            "session-b",
            1,
            Evidence::MempoolAdded {
                txid: TXID_B.to_owned(),
            },
        );
        assert!(matches!(
            IngestBatchRequest {
                events: vec![valid.clone(), other_source],
            }
            .validate(),
            Err(ModelError::IngestBatchSourceMismatch { .. })
        ));

        let mut invalid = valid.clone();
        invalid.event_id = "wrong/session/identity".to_owned();
        assert_eq!(
            IngestBatchRequest {
                events: vec![valid, invalid],
            }
            .validate(),
            Err(ModelError::EventIdMismatch)
        );
    }

    #[test]
    fn transaction_replacement_closes_and_opens_membership() {
        let event = event(Evidence::MempoolReplaced {
            replaced_txid: TXID_A.to_owned(),
            replacement: Replacement::Transaction {
                txid: TXID_B.to_owned(),
            },
        });
        assert_eq!(
            event.membership_mutations(),
            vec![
                MembershipMutation::Absent {
                    txid: TXID_A.to_owned(),
                },
                MembershipMutation::Present {
                    txid: TXID_B.to_owned(),
                    facts: None,
                },
            ]
        );
    }

    #[test]
    fn reconciliation_sets_the_requested_membership_state() {
        let present = event(Evidence::MempoolReconciled {
            txid: TXID_A.to_owned(),
            membership: ReconciledMembership::Present { facts: facts() },
        });
        assert_eq!(
            present.membership_mutations(),
            vec![MembershipMutation::Present {
                txid: TXID_A.to_owned(),
                facts: Some(facts()),
            }]
        );

        let absent = event(Evidence::MempoolReconciled {
            txid: TXID_A.to_owned(),
            membership: ReconciledMembership::Absent,
        });
        assert_eq!(
            absent.membership_mutations(),
            vec![MembershipMutation::Absent {
                txid: TXID_A.to_owned(),
            }]
        );
    }

    #[test]
    fn reconciliation_remains_distinct_from_removal_and_rejection() {
        let reconciled = Evidence::MempoolReconciled {
            txid: TXID_A.to_owned(),
            membership: ReconciledMembership::Absent,
        };
        let removed = Evidence::MempoolRemoved {
            txid: TXID_A.to_owned(),
            reason: Some("expired".to_owned()),
        };
        let rejected = Evidence::MempoolRejected {
            txid: TXID_A.to_owned(),
            reason: "policy".to_owned(),
        };

        assert_eq!(reconciled.kind(), "mempool_reconciled");
        assert_eq!(removed.kind(), "mempool_removed");
        assert_eq!(rejected.kind(), "mempool_rejected");
        assert_ne!(reconciled, removed);
        assert_ne!(reconciled, rejected);
        assert!(event(rejected).membership_mutations().is_empty());
    }

    #[test]
    fn reconciliation_validates_its_txid() {
        assert!(matches!(
            NormalizedEvent::new(
                SourceId::new("source-a").expect("source"),
                SourceSessionId::new("session-a").expect("session"),
                7,
                1_000,
                1_001,
                Evidence::MempoolReconciled {
                    txid: "not-a-txid".to_owned(),
                    membership: ReconciledMembership::Present { facts: facts() },
                },
            ),
            Err(ModelError::InvalidTxid(_))
        ));
    }

    #[test]
    fn reconciliation_rejects_zero_vsize() {
        let mut invalid_facts = facts();
        invalid_facts.vsize = 0;
        assert_eq!(
            NormalizedEvent::new(
                SourceId::new("source-a").expect("source"),
                SourceSessionId::new("session-a").expect("session"),
                7,
                1_000,
                1_001,
                Evidence::MempoolReconciled {
                    txid: TXID_A.to_owned(),
                    membership: ReconciledMembership::Present {
                        facts: invalid_facts,
                    },
                },
            ),
            Err(ModelError::ZeroVsize)
        );
    }

    #[test]
    fn reconciliation_rejects_facts_that_cannot_be_exact_json_integers() {
        for (field, invalid_facts) in [
            (
                "vsize",
                MempoolEntryFacts {
                    vsize: MAX_SAFE_JSON_INTEGER + 1,
                    ..facts()
                },
            ),
            (
                "fee_sats",
                MempoolEntryFacts {
                    fee_sats: MAX_SAFE_JSON_INTEGER + 1,
                    ..facts()
                },
            ),
            (
                "entered_at_ms",
                MempoolEntryFacts {
                    entered_at_ms: MAX_SAFE_JSON_INTEGER + 1,
                    ..facts()
                },
            ),
        ] {
            assert_eq!(
                invalid_facts.validate(),
                Err(ModelError::MempoolEntryFactTooLarge {
                    field,
                    value: MAX_SAFE_JSON_INTEGER + 1,
                    maximum: MAX_SAFE_JSON_INTEGER,
                })
            );
        }
    }

    #[test]
    fn fact_statuses_have_explicit_wire_shapes() {
        assert_eq!(
            serde_json::to_value(MempoolEntryFactsStatus::AwaitingRpc).expect("pending facts"),
            serde_json::json!({ "status": "awaiting_rpc" })
        );
        assert_eq!(
            serde_json::to_value(MempoolEntryFactsStatus::Available { facts: facts() })
                .expect("available facts"),
            serde_json::json!({
                "status": "available",
                "vsize": 141,
                "fee_sats": 1_200,
                "entered_at_ms": 1_721_234_000_000_u64,
            })
        );
    }

    #[test]
    fn not_collected_capture_status_is_explicit_on_the_wire() {
        assert_eq!(
            serde_json::to_value(CaptureStatus::NotCollected).expect("capture status"),
            serde_json::json!({ "status": "not_collected" })
        );

        let health = SourceHealth {
            state_cursor: ReplicaCursor::new(SourceEpochId::new("epoch-a").expect("epoch"), 7)
                .expect("cursor"),
            state_observed_at_ms: 1_721_234_000_000,
            capture: CaptureStatus::NotCollected,
        };
        assert_eq!(
            serde_json::to_value(health).expect("source health"),
            serde_json::json!({
                "state_cursor": { "epoch_id": "epoch-a", "revision": 7 },
                "state_observed_at_ms": 1_721_234_000_000_u64,
                "capture": { "status": "not_collected" },
            })
        );
    }

    #[test]
    fn package_replacement_never_opens_phantom_membership() {
        let event = event(Evidence::MempoolReplaced {
            replaced_txid: TXID_A.to_owned(),
            replacement: Replacement::PackageHash {
                hash: TXID_B.to_owned(),
            },
        });
        assert_eq!(
            event.membership_mutations(),
            vec![MembershipMutation::Absent {
                txid: TXID_A.to_owned(),
            }]
        );
    }

    #[test]
    fn rejection_does_not_mutate_membership() {
        let event = event(Evidence::MempoolRejected {
            txid: TXID_A.to_owned(),
            reason: "policy".to_owned(),
        });
        assert!(event.membership_mutations().is_empty());
    }

    #[test]
    fn capture_gap_is_valid_non_membership_evidence() {
        let event = event(Evidence::CaptureGap {
            input: "peer_observer_nats".to_owned(),
            reason: "slow_consumer".to_owned(),
            certainty: CaptureGapCertainty::KnownLoss,
        });

        assert_eq!(event.schema_version, 3);
        assert_eq!(event.evidence.kind(), "capture_gap");
        assert!(event.membership_mutations().is_empty());
        assert_eq!(
            serde_json::to_value(&event.evidence).expect("serialize evidence"),
            serde_json::json!({
                "kind": "capture_gap",
                "input": "peer_observer_nats",
                "reason": "slow_consumer",
                "certainty": "known_loss"
            })
        );
    }

    #[test]
    fn capture_gap_requires_input_and_reason() {
        for evidence in [
            Evidence::CaptureGap {
                input: " ".to_owned(),
                reason: "disconnected".to_owned(),
                certainty: CaptureGapCertainty::PossibleLoss,
            },
            Evidence::CaptureGap {
                input: "peer_observer_nats".to_owned(),
                reason: String::new(),
                certainty: CaptureGapCertainty::PossibleLoss,
            },
        ] {
            assert!(matches!(
                NormalizedEvent::new(
                    SourceId::new("source-a").expect("source"),
                    SourceSessionId::new("session-a").expect("session"),
                    7,
                    1_000,
                    1_001,
                    evidence,
                ),
                Err(ModelError::EmptyField { .. })
            ));
        }
    }

    #[test]
    fn source_ids_cannot_make_event_ids_ambiguous() {
        assert!(matches!(
            SourceId::new("source/with/slashes"),
            Err(ModelError::InvalidIdentifier { field: "source_id" })
        ));
    }

    #[test]
    fn package_hashes_must_be_32_byte_hex() {
        assert!(matches!(
            Replacement::PackageHash {
                hash: "not-a-package-hash".to_owned()
            }
            .validate(),
            Err(ModelError::InvalidPackageHash(_))
        ));
    }
}
