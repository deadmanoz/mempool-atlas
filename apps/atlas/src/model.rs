use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use thiserror::Error;

pub const MAX_SAFE_JSON_INTEGER: u64 = 9_007_199_254_740_991;
pub const MAX_SUPPORTED_MEMPOOL_ENTRIES: u64 = 200_000;
pub const MAX_BIP110_DETAIL_EXEMPLARS_PER_RULE: usize = 1;
pub const BIP110_EVALUATOR_ID: &str = "rdts-rules";
pub const BIP110_EVALUATOR_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Bip110RuleId {
    OutputSize,
    ElementSize,
    UndefinedVersion,
    TaprootAnnex,
    ControlBlockSize,
    OpSuccess,
    TapscriptOpIf,
}

impl Bip110RuleId {
    pub const ALL: [Self; 7] = [
        Self::OutputSize,
        Self::ElementSize,
        Self::UndefinedVersion,
        Self::TaprootAnnex,
        Self::ControlBlockSize,
        Self::OpSuccess,
        Self::TapscriptOpIf,
    ];

    pub fn number(self) -> u8 {
        match self {
            Self::OutputSize => 1,
            Self::ElementSize => 2,
            Self::UndefinedVersion => 3,
            Self::TaprootAnnex => 4,
            Self::ControlBlockSize => 5,
            Self::OpSuccess => 6,
            Self::TapscriptOpIf => 7,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Bip110Status {
    Compatible,
    Violating,
    Indeterminate,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Bip110Assessment {
    pub status: Bip110Status,
    pub primary_rule: Option<Bip110RuleId>,
    pub violated_rules: Vec<Bip110RuleId>,
    pub unknown_rules: Vec<Bip110RuleId>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Bip110Scope {
    KnotsMempoolPolicy,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Bip110Summary {
    pub evaluator_id: String,
    pub evaluator_version: String,
    pub scope: Bip110Scope,
    pub compatible_count: u64,
    pub violating_count: u64,
    pub indeterminate_count: u64,
    pub unclassified_count: u64,
}

impl Bip110Summary {
    fn from_transactions(transactions: &[MempoolEntry]) -> Self {
        let mut compatible_count = 0_u64;
        let mut violating_count = 0_u64;
        let mut indeterminate_count = 0_u64;
        let mut unclassified_count = 0_u64;

        for entry in transactions {
            match entry.bip110.as_ref().map(|assessment| assessment.status) {
                Some(Bip110Status::Compatible) => compatible_count += 1,
                Some(Bip110Status::Violating) => violating_count += 1,
                Some(Bip110Status::Indeterminate) => indeterminate_count += 1,
                None => unclassified_count += 1,
            }
        }

        Self {
            evaluator_id: BIP110_EVALUATOR_ID.to_owned(),
            evaluator_version: BIP110_EVALUATOR_VERSION.to_owned(),
            scope: Bip110Scope::KnotsMempoolPolicy,
            compatible_count,
            violating_count,
            indeterminate_count,
            unclassified_count,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MempoolEntry {
    pub txid: String,
    pub wtxid: String,
    pub vsize: u64,
    pub fee_sats: u64,
    pub entered_at_ms: u64,
    pub bip110: Option<Bip110Assessment>,
}

impl MempoolEntry {
    pub fn new(
        txid: String,
        vsize: u64,
        fee_sats: u64,
        entered_at_ms: u64,
    ) -> Result<Self, ModelError> {
        let wtxid = txid.clone();
        Self::new_variant(txid, wtxid, vsize, fee_sats, entered_at_ms)
    }

    pub fn new_variant(
        txid: String,
        wtxid: String,
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
            wtxid,
            vsize,
            fee_sats,
            entered_at_ms,
            bip110: None,
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
    pub bip110_summary: Bip110Summary,
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
            bip110_summary: Bip110Summary::from_transactions(&transactions),
            transactions,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Bip110RuleVerdict {
    Pass,
    Violate,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Bip110RuleDetail {
    pub rule: Bip110RuleId,
    pub number: u8,
    pub verdict: Bip110RuleVerdict,
    pub evidence_count: u64,
    pub evidence: Vec<rdts_rules::Violation>,
    pub missing_count: u64,
    pub missing: Vec<rdts_rules::Missing>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TransactionClassification {
    pub txid: String,
    pub wtxid: String,
    pub assessment: Bip110Assessment,
    pub rules: Vec<Bip110RuleDetail>,
}

pub type TransactionClassifications = BTreeMap<String, Arc<TransactionClassification>>;

#[derive(Clone, Debug)]
pub struct MempoolObservation {
    pub snapshot: MempoolSnapshot,
    pub classifications: TransactionClassifications,
}

impl MempoolObservation {
    pub fn new(
        snapshot: MempoolSnapshot,
        classifications: TransactionClassifications,
    ) -> Result<Self, ModelError> {
        for (txid, classification) in &classifications {
            validate_classification(classification)?;
            if txid != &classification.txid {
                return Err(ModelError::ClassificationKeyMismatch {
                    key: txid.clone(),
                    txid: classification.txid.clone(),
                });
            }
            let entry = snapshot
                .transactions
                .binary_search_by(|entry| entry.txid.as_str().cmp(txid.as_str()))
                .ok()
                .map(|index| &snapshot.transactions[index])
                .ok_or_else(|| ModelError::ClassificationNotInSnapshot(txid.clone()))?;
            if entry.wtxid != classification.wtxid
                || entry.bip110.as_ref() != Some(&classification.assessment)
            {
                return Err(ModelError::ClassificationVariantMismatch(txid.clone()));
            }
        }
        for entry in &snapshot.transactions {
            if entry.bip110.is_some() && !classifications.contains_key(&entry.txid) {
                return Err(ModelError::ClassificationDetailMissing(entry.txid.clone()));
            }
        }
        Ok(Self {
            snapshot,
            classifications,
        })
    }
}

fn validate_classification(classification: &TransactionClassification) -> Result<(), ModelError> {
    if classification.rules.len() != Bip110RuleId::ALL.len() {
        return Err(ModelError::InvalidClassificationDetail {
            txid: classification.txid.clone(),
            reason: "must contain all seven rules",
        });
    }

    let mut violated_rules = Vec::new();
    let mut unknown_rules = Vec::new();
    for (detail, expected_rule) in classification.rules.iter().zip(Bip110RuleId::ALL) {
        if detail.rule != expected_rule || detail.number != expected_rule.number() {
            return Err(ModelError::InvalidClassificationDetail {
                txid: classification.txid.clone(),
                reason: "rules must use canonical order and numbering",
            });
        }
        if detail.evidence.len() > MAX_BIP110_DETAIL_EXEMPLARS_PER_RULE
            || detail.missing.len() > MAX_BIP110_DETAIL_EXEMPLARS_PER_RULE
        {
            return Err(ModelError::InvalidClassificationDetail {
                txid: classification.txid.clone(),
                reason: "retained rule exemplars exceed the configured bound",
            });
        }
        let evidence_len = u64::try_from(detail.evidence.len()).unwrap_or(u64::MAX);
        let missing_len = u64::try_from(detail.missing.len()).unwrap_or(u64::MAX);
        if detail.evidence_count < evidence_len
            || detail.missing_count < missing_len
            || (detail.evidence_count == 0) != detail.evidence.is_empty()
            || (detail.missing_count == 0) != detail.missing.is_empty()
        {
            return Err(ModelError::InvalidClassificationDetail {
                txid: classification.txid.clone(),
                reason: "rule exemplar counts do not match retained detail",
            });
        }

        match detail.verdict {
            Bip110RuleVerdict::Pass => {
                if detail.evidence_count != 0 || detail.missing_count != 0 {
                    return Err(ModelError::InvalidClassificationDetail {
                        txid: classification.txid.clone(),
                        reason: "a passing rule cannot contain evidence or missing facts",
                    });
                }
            }
            Bip110RuleVerdict::Violate => {
                if detail.evidence_count == 0 {
                    return Err(ModelError::InvalidClassificationDetail {
                        txid: classification.txid.clone(),
                        reason: "a violating rule must contain evidence",
                    });
                }
                violated_rules.push(detail.rule);
                if detail.missing_count > 0 {
                    unknown_rules.push(detail.rule);
                }
            }
            Bip110RuleVerdict::Unknown => {
                if detail.evidence_count != 0 || detail.missing_count == 0 {
                    return Err(ModelError::InvalidClassificationDetail {
                        txid: classification.txid.clone(),
                        reason: "an unknown rule must contain only missing facts",
                    });
                }
                unknown_rules.push(detail.rule);
            }
        }
    }

    let expected_status = if violated_rules.is_empty() {
        if unknown_rules.is_empty() {
            Bip110Status::Compatible
        } else {
            Bip110Status::Indeterminate
        }
    } else {
        Bip110Status::Violating
    };
    if classification.assessment.status != expected_status
        || classification.assessment.violated_rules != violated_rules
        || classification.assessment.unknown_rules != unknown_rules
        || classification
            .assessment
            .primary_rule
            .is_some_and(|rule| !violated_rules.contains(&rule))
    {
        return Err(ModelError::InvalidClassificationDetail {
            txid: classification.txid.clone(),
            reason: "compact assessment does not match rule detail",
        });
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TransactionDetailResponse {
    pub source_id: String,
    pub snapshot_observed_at_ms: u64,
    pub txid: String,
    pub wtxid: String,
    pub assessment: Bip110Assessment,
    pub rules: Vec<Bip110RuleDetail>,
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
    #[error("classification key {key:?} does not match transaction ID {txid:?}")]
    ClassificationKeyMismatch { key: String, txid: String },
    #[error("classification for transaction {0:?} is not in the current snapshot")]
    ClassificationNotInSnapshot(String),
    #[error("classification for transaction {0:?} does not match the current witness variant")]
    ClassificationVariantMismatch(String),
    #[error("classified transaction {0:?} is missing its atomic detail record")]
    ClassificationDetailMissing(String),
    #[error("classification for transaction {txid:?} is invalid: {reason}")]
    InvalidClassificationDetail { txid: String, reason: &'static str },
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
        assert_eq!(snapshot.bip110_summary.unclassified_count, 2);
    }

    #[test]
    fn snapshot_summarizes_policy_assessments() {
        let mut compatible = entry("00", 100);
        compatible.bip110 = Some(Bip110Assessment {
            status: Bip110Status::Compatible,
            primary_rule: None,
            violated_rules: Vec::new(),
            unknown_rules: Vec::new(),
        });
        let mut violating = entry("01", 200);
        violating.bip110 = Some(Bip110Assessment {
            status: Bip110Status::Violating,
            primary_rule: Some(Bip110RuleId::OutputSize),
            violated_rules: vec![Bip110RuleId::OutputSize],
            unknown_rules: Vec::new(),
        });
        let snapshot = MempoolSnapshot::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            1,
            ChainTip {
                height: 1,
                hash: "00".repeat(32),
            },
            vec![compatible, violating, entry("02", 300)],
        )
        .expect("snapshot");

        assert_eq!(snapshot.bip110_summary.compatible_count, 1);
        assert_eq!(snapshot.bip110_summary.violating_count, 1);
        assert_eq!(snapshot.bip110_summary.indeterminate_count, 0);
        assert_eq!(snapshot.bip110_summary.unclassified_count, 1);
        assert_eq!(
            snapshot.bip110_summary.compatible_count
                + snapshot.bip110_summary.violating_count
                + snapshot.bip110_summary.indeterminate_count
                + snapshot.bip110_summary.unclassified_count,
            snapshot.transaction_count
        );
    }

    #[test]
    fn observation_requires_atomic_detail_for_every_classified_entry() {
        let mut classified = entry("00", 100);
        classified.bip110 = Some(Bip110Assessment {
            status: Bip110Status::Compatible,
            primary_rule: None,
            violated_rules: Vec::new(),
            unknown_rules: Vec::new(),
        });
        let snapshot = MempoolSnapshot::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            1,
            ChainTip {
                height: 1,
                hash: "00".repeat(32),
            },
            vec![classified],
        )
        .expect("snapshot");

        assert!(matches!(
            MempoolObservation::new(snapshot, BTreeMap::new()),
            Err(ModelError::ClassificationDetailMissing(txid)) if txid == "00"
        ));
    }

    #[test]
    fn observation_rejects_rule_detail_above_the_exemplar_bound() {
        let assessment = Bip110Assessment {
            status: Bip110Status::Indeterminate,
            primary_rule: None,
            violated_rules: Vec::new(),
            unknown_rules: vec![Bip110RuleId::OutputSize],
        };
        let mut classified = entry("00", 100);
        classified.bip110 = Some(assessment.clone());
        let snapshot = MempoolSnapshot::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            1,
            ChainTip {
                height: 1,
                hash: "00".repeat(32),
            },
            vec![classified],
        )
        .expect("snapshot");
        let rules = Bip110RuleId::ALL
            .into_iter()
            .map(|rule| {
                if rule == Bip110RuleId::OutputSize {
                    Bip110RuleDetail {
                        rule,
                        number: rule.number(),
                        verdict: Bip110RuleVerdict::Unknown,
                        evidence_count: 0,
                        evidence: Vec::new(),
                        missing_count: 2,
                        missing: vec![
                            rdts_rules::Missing::ScriptPubKey { input: 0 },
                            rdts_rules::Missing::ScriptPubKey { input: 1 },
                        ],
                    }
                } else {
                    Bip110RuleDetail {
                        rule,
                        number: rule.number(),
                        verdict: Bip110RuleVerdict::Pass,
                        evidence_count: 0,
                        evidence: Vec::new(),
                        missing_count: 0,
                        missing: Vec::new(),
                    }
                }
            })
            .collect();
        let classification = Arc::new(TransactionClassification {
            txid: "00".to_owned(),
            wtxid: "00".to_owned(),
            assessment,
            rules,
        });

        assert!(matches!(
            MempoolObservation::new(
                snapshot,
                BTreeMap::from([("00".to_owned(), classification)])
            ),
            Err(ModelError::InvalidClassificationDetail { reason, .. })
                if reason.contains("exemplars")
        ));
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
