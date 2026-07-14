//! Shared Mempool Atlas domain and wire types.

use std::fmt;
use std::str::FromStr;

use bitcoin::{Txid, Wtxid};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ModelError {
    #[error("{field} must not be empty")]
    EmptyIdentifier { field: &'static str },
    #[error("{field} contains unsupported characters")]
    InvalidIdentifier { field: &'static str },
    #[error("invalid txid: {0}")]
    InvalidTxid(String),
    #[error("invalid wtxid: {0}")]
    InvalidWtxid(String),
    #[error("invalid package hash: {0}")]
    InvalidPackageHash(String),
    #[error("unsupported schema version {found}; expected {expected}")]
    UnsupportedSchemaVersion { found: u16, expected: u16 },
    #[error("event_id does not match source, session, and local sequence")]
    EventIdMismatch,
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
            Evidence::MempoolAdded { txid } => vec![MembershipMutation::present(txid)],
            Evidence::MempoolRemoved { txid, .. } => vec![MembershipMutation::absent(txid)],
            Evidence::MempoolReplaced {
                replaced_txid,
                replacement,
            } => {
                let mut mutations = vec![MembershipMutation::absent(replaced_txid)];
                if let Replacement::Transaction { txid } = replacement {
                    mutations.push(MembershipMutation::present(txid));
                }
                mutations
            }
            Evidence::MempoolRejected { .. } | Evidence::P2pTransaction { .. } => Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Evidence {
    MempoolAdded {
        txid: String,
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
}

impl Evidence {
    pub fn validate(&self) -> Result<(), ModelError> {
        match self {
            Self::MempoolAdded { txid }
            | Self::MempoolRemoved { txid, .. }
            | Self::MempoolRejected { txid, .. } => validate_txid(txid),
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
        }
    }

    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::MempoolAdded { .. } => "mempool_added",
            Self::MempoolRemoved { .. } => "mempool_removed",
            Self::MempoolRejected { .. } => "mempool_rejected",
            Self::MempoolReplaced { .. } => "mempool_replaced",
            Self::P2pTransaction { .. } => "p2p_transaction",
        }
    }
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
pub struct MembershipMutation {
    pub txid: String,
    pub present: bool,
}

impl MembershipMutation {
    fn present(txid: &str) -> Self {
        Self {
            txid: txid.to_owned(),
            present: true,
        }
    }

    fn absent(txid: &str) -> Self {
        Self {
            txid: txid.to_owned(),
            present: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MembershipRow {
    pub source_id: SourceId,
    pub txid: String,
    pub present: bool,
    pub updated_at_ms: u64,
    pub evidence_event_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MempoolSnapshot {
    pub source_id: Option<SourceId>,
    pub memberships: Vec<MembershipRow>,
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

fn ensure_nonempty(field: &'static str, value: &str) -> Result<(), ModelError> {
    if value.trim().is_empty() {
        return Err(ModelError::EmptyIdentifier { field });
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

    fn event(evidence: Evidence) -> NormalizedEvent {
        NormalizedEvent::new(
            SourceId::new("source-a").expect("source"),
            SourceSessionId::new("session-a").expect("session"),
            7,
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
                MembershipMutation {
                    txid: TXID_A.to_owned(),
                    present: false,
                },
                MembershipMutation {
                    txid: TXID_B.to_owned(),
                    present: true,
                },
            ]
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
            vec![MembershipMutation {
                txid: TXID_A.to_owned(),
                present: false,
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
