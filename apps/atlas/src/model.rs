use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

pub const MAX_SAFE_JSON_INTEGER: u64 = 9_007_199_254_740_991;
pub const MAX_SUPPORTED_MEMPOOL_ENTRIES: u64 = 200_000;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MempoolEntry {
    pub txid: String,
    pub vsize: u64,
    pub fee_sats: u64,
    pub entered_at_ms: u64,
}

impl MempoolEntry {
    pub fn new(
        txid: String,
        vsize: u64,
        fee_sats: u64,
        entered_at_ms: u64,
    ) -> Result<Self, ModelError> {
        if vsize == 0 {
            return Err(ModelError::ZeroVsize);
        }
        for (field, value) in [
            ("vsize", vsize),
            ("fee_sats", fee_sats),
            ("entered_at_ms", entered_at_ms),
        ] {
            if value > MAX_SAFE_JSON_INTEGER {
                return Err(ModelError::UnsafeJsonInteger { field, value });
            }
        }
        Ok(Self {
            txid,
            vsize,
            fee_sats,
            entered_at_ms,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChainTip {
    pub height: u64,
    pub hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MempoolSnapshot {
    pub source_id: String,
    pub source_label: String,
    pub observed_at_ms: u64,
    pub chain_tip: ChainTip,
    pub transaction_count: u64,
    pub total_vsize: u64,
    pub transactions: Vec<MempoolEntry>,
}

impl MempoolSnapshot {
    pub fn new(
        source_id: String,
        source_label: String,
        observed_at_ms: u64,
        chain_tip: ChainTip,
        transactions: Vec<MempoolEntry>,
    ) -> Result<Self, ModelError> {
        validate_source_id(&source_id)?;
        validate_source_label(&source_label)?;
        if observed_at_ms > MAX_SAFE_JSON_INTEGER {
            return Err(ModelError::UnsafeJsonInteger {
                field: "observed_at_ms",
                value: observed_at_ms,
            });
        }
        if chain_tip.height > MAX_SAFE_JSON_INTEGER {
            return Err(ModelError::UnsafeJsonInteger {
                field: "chain_tip.height",
                value: chain_tip.height,
            });
        }
        if transactions
            .windows(2)
            .any(|pair| pair[0].txid >= pair[1].txid)
        {
            return Err(ModelError::TransactionsNotStrictlySorted);
        }
        let transaction_count =
            u64::try_from(transactions.len()).map_err(|_| ModelError::SnapshotTooLarge)?;
        if transaction_count > MAX_SAFE_JSON_INTEGER {
            return Err(ModelError::SnapshotTooLarge);
        }
        let total_vsize = transactions.iter().try_fold(0_u64, |total, entry| {
            total
                .checked_add(entry.vsize)
                .ok_or(ModelError::TotalVsizeOverflow)
        })?;
        if total_vsize > MAX_SAFE_JSON_INTEGER {
            return Err(ModelError::UnsafeJsonInteger {
                field: "total_vsize",
                value: total_vsize,
            });
        }
        Ok(Self {
            source_id,
            source_label,
            observed_at_ms,
            chain_tip,
            transaction_count,
            total_vsize,
            transactions,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAvailability {
    Waiting,
    Ready,
    Stale,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SourceSummary {
    pub source_id: String,
    pub source_label: String,
    pub availability: SourceAvailability,
    pub poll_interval_seconds: u64,
    pub last_poll_started_at_ms: Option<u64>,
    pub snapshot_observed_at_ms: Option<u64>,
    pub chain_tip: Option<ChainTip>,
    pub transaction_count: Option<u64>,
    pub total_vsize: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SourcesResponse {
    pub sources: Vec<SourceSummary>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SourceSnapshotResponse {
    pub source: SourceSummary,
    pub snapshot: Option<Arc<MempoolSnapshot>>,
}

pub fn validate_source_id(value: &str) -> Result<(), ModelError> {
    if value.is_empty()
        || matches!(value, "." | "..")
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(ModelError::InvalidSourceId(value.to_owned()));
    }
    Ok(())
}

pub fn validate_source_label(value: &str) -> Result<(), ModelError> {
    if value.trim().is_empty() || value.len() > 128 {
        return Err(ModelError::InvalidSourceLabel(value.to_owned()));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ModelError {
    #[error(
        "source ID {0:?} must use 1 to 64 ASCII letters, digits, dots, underscores, or hyphens and cannot be a URL dot segment"
    )]
    InvalidSourceId(String),
    #[error("source label {0:?} must contain 1 to 128 characters")]
    InvalidSourceLabel(String),
    #[error("mempool entry vsize must be greater than zero")]
    ZeroVsize,
    #[error("{field} value {value} cannot be represented exactly in JSON")]
    UnsafeJsonInteger { field: &'static str, value: u64 },
    #[error("mempool snapshot has too many transactions")]
    SnapshotTooLarge,
    #[error("mempool snapshot total vsize overflowed")]
    TotalVsizeOverflow,
    #[error("mempool transactions must be strictly sorted by txid")]
    TransactionsNotStrictlySorted,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(txid: &str, vsize: u64) -> MempoolEntry {
        MempoolEntry::new(txid.to_owned(), vsize, 100, 1_700_000_000_000).expect("entry")
    }

    #[test]
    fn snapshot_derives_bounded_totals() {
        let snapshot = MempoolSnapshot::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            1_700_000_000_100,
            ChainTip {
                height: 900_000,
                hash: "00".repeat(32),
            },
            vec![entry("00", 100), entry("01", 250)],
        )
        .expect("snapshot");

        assert_eq!(snapshot.transaction_count, 2);
        assert_eq!(snapshot.total_vsize, 350);
    }

    #[test]
    fn snapshot_rejects_unsorted_or_duplicate_transactions() {
        for transactions in [
            vec![entry("01", 100), entry("00", 100)],
            vec![entry("00", 100), entry("00", 100)],
        ] {
            assert!(matches!(
                MempoolSnapshot::new(
                    "core".to_owned(),
                    "Bitcoin Core".to_owned(),
                    1,
                    ChainTip {
                        height: 1,
                        hash: "00".repeat(32),
                    },
                    transactions,
                ),
                Err(ModelError::TransactionsNotStrictlySorted)
            ));
        }
    }

    #[test]
    fn source_identity_is_small_and_url_safe() {
        validate_source_id("core-node").expect("source ID");
        for value in ["", ".", "..", "core source", "core/one", &"a".repeat(65)] {
            assert!(validate_source_id(value).is_err(), "{value:?}");
        }
    }
}
