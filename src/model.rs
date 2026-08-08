use bitcoin::OutPoint;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use thiserror::Error;

pub const MAX_SAFE_JSON_INTEGER: u64 = 9_007_199_254_740_991;
pub const MAX_SUPPORTED_MEMPOOL_ENTRIES: u64 = 200_000;
pub const MAX_BIP110_DETAIL_EXEMPLARS_PER_RULE: usize = 1;
pub const BIP110_EVALUATOR_ID: &str = "rdts-rules";
pub const ATLAS_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BIP110_EVALUATOR_VERSION: &str = ATLAS_VERSION;
pub const TRANSACTION_PROPERTIES_CLASSIFIER_ID: &str = "transaction_properties";
pub const TRANSACTION_SHAPE_CLASSIFIER_ID: &str = "transaction_shape";
pub const DATA_PROTOCOLS_CLASSIFIER_ID: &str = "data_protocols";
pub const DATA_CARRIAGE_SHAPE_CLASSIFIER_ID: &str = "data_carriage_shape";
pub const KNOTS_BIP110_CLASSIFIER_ID: &str = "knots_bip110";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassifierMethodology {
    Exact,
    Heuristic,
    Fingerprint,
    Policy,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassifierSemantics {
    MultiLabel,
    RuleSet,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClassifierLabelDescriptor {
    pub key: String,
    pub label: String,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClassifierDescriptor {
    pub id: String,
    pub version: String,
    pub title: String,
    pub methodology: ClassifierMethodology,
    pub semantics: ClassifierSemantics,
    pub required_facts: Vec<String>,
    pub labels: Vec<ClassifierLabelDescriptor>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationResultState {
    Complete,
    Partial,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClassificationResult {
    pub classifier_id: String,
    pub state: ClassificationResultState,
    pub primary_label: Option<String>,
    pub labels: Vec<String>,
    pub missing_facts: Vec<String>,
    /// Detailed, bounded evidence is omitted from full mempool snapshots and
    /// retained only in the atomic transaction-detail record.
    pub evidence: Option<serde_json::Value>,
}

impl ClassificationResult {
    pub fn compact(&self) -> Self {
        let mut compact = self.clone();
        compact.evidence = None;
        compact
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClassifierSummary {
    pub classifier_id: String,
    pub complete_count: u64,
    pub partial_count: u64,
    pub unclassified_count: u64,
    pub label_counts: BTreeMap<String, u64>,
}

fn label(key: &str, label: &str, description: &str) -> ClassifierLabelDescriptor {
    ClassifierLabelDescriptor {
        key: key.to_owned(),
        label: label.to_owned(),
        description: description.to_owned(),
    }
}

pub fn classifier_catalog() -> Vec<ClassifierDescriptor> {
    vec![
        ClassifierDescriptor {
            id: TRANSACTION_PROPERTIES_CLASSIFIER_ID.to_owned(),
            version: "1".to_owned(),
            title: "Transaction properties".to_owned(),
            methodology: ClassifierMethodology::Exact,
            semantics: ClassifierSemantics::MultiLabel,
            required_facts: vec![
                "raw_transaction".to_owned(),
                "input_script_pubkeys".to_owned(),
            ],
            labels: vec![
                label(
                    "version_1",
                    "Version 1",
                    "Serialized transaction version 1.",
                ),
                label(
                    "version_2",
                    "Version 2",
                    "Serialized transaction version 2.",
                ),
                label(
                    "version_3",
                    "Version 3",
                    "Serialized transaction version 3.",
                ),
                label(
                    "version_other",
                    "Other version",
                    "A transaction version other than 1, 2, or 3.",
                ),
                label(
                    "signals_rbf",
                    "Signals RBF",
                    "At least one input explicitly signals BIP-125 replaceability.",
                ),
                label(
                    "has_witness",
                    "Has witness",
                    "At least one input has witness data.",
                ),
                label(
                    "has_taproot_annex",
                    "Taproot annex",
                    "A P2TR input has a structural annex candidate.",
                ),
                label(
                    "p2pk",
                    "P2PK",
                    "A known input or output uses pay-to-public-key.",
                ),
                label(
                    "bare_multisig",
                    "Bare multisig",
                    "A known input or output uses bare multisig.",
                ),
                label(
                    "p2pkh",
                    "P2PKH",
                    "A known input or output uses pay-to-public-key-hash.",
                ),
                label(
                    "p2sh",
                    "P2SH",
                    "A known input or output uses pay-to-script-hash.",
                ),
                label(
                    "p2wpkh",
                    "P2WPKH",
                    "A known input or output uses native witness public-key-hash.",
                ),
                label(
                    "p2wsh",
                    "P2WSH",
                    "A known input or output uses native witness script-hash.",
                ),
                label("p2tr", "P2TR", "A known input or output uses Taproot."),
                label("p2a", "P2A", "A known input or output uses pay-to-anchor."),
                label(
                    "unknown_witness_program",
                    "Other witness program",
                    "A known input or output uses another syntactically valid witness program.",
                ),
                label("op_return", "OP_RETURN", "An output uses OP_RETURN."),
                label(
                    "unknown_script",
                    "Other script",
                    "A known input or output script is outside the recognized families.",
                ),
            ],
        },
        ClassifierDescriptor {
            id: TRANSACTION_SHAPE_CLASSIFIER_ID.to_owned(),
            version: "2".to_owned(),
            title: "Transaction shape".to_owned(),
            methodology: ClassifierMethodology::Heuristic,
            semantics: ClassifierSemantics::MultiLabel,
            required_facts: vec![
                "raw_transaction".to_owned(),
                "input_script_pubkeys".to_owned(),
            ],
            labels: vec![
                label(
                    "possible_coinjoin",
                    "Possible CoinJoin",
                    "A conservative equal-output, no-script-reuse heuristic.",
                ),
                label(
                    "consolidation",
                    "Consolidation",
                    "At least five times as many inputs as outputs.",
                ),
                label(
                    "batch_payout",
                    "Batch payout",
                    "At least five times as many outputs as inputs.",
                ),
                label(
                    "other_shape",
                    "Other shape",
                    "Every registered transaction-shape heuristic was terminal and none matched.",
                ),
            ],
        },
        ClassifierDescriptor {
            id: DATA_PROTOCOLS_CLASSIFIER_ID.to_owned(),
            version: "3".to_owned(),
            title: "Data protocols".to_owned(),
            methodology: ClassifierMethodology::Fingerprint,
            semantics: ClassifierSemantics::MultiLabel,
            required_facts: vec!["raw_transaction".to_owned()],
            labels: vec![
                label(
                    "inscription",
                    "Inscription",
                    "An Ordinals inscription-envelope fingerprint.",
                ),
                label(
                    "brc20",
                    "BRC-20",
                    "A JSON-like BRC-20 marker inside an inscription envelope.",
                ),
                label("runes", "Runes", "An OP_RETURN OP_13 runestone marker."),
                label(
                    "stamps",
                    "Stamps",
                    "A bare-multisig carrier whose deobfuscated payload holds a Stamps marker.",
                ),
                label(
                    "counterparty",
                    "Counterparty",
                    "A deobfuscated CNTRPRTY envelope in an OP_RETURN or bare-multisig carrier.",
                ),
                label(
                    "omni",
                    "Omni",
                    "An OP_RETURN payload beginning with the Omni Class C marker.",
                ),
                label(
                    "other_op_return",
                    "Other OP_RETURN",
                    "An OP_RETURN carrier with no registered OP_RETURN protocol fingerprint.",
                ),
                label(
                    "no_detected_protocol",
                    "No detected protocol",
                    "No registered data-protocol fingerprint fired.",
                ),
            ],
        },
        ClassifierDescriptor {
            id: DATA_CARRIAGE_SHAPE_CLASSIFIER_ID.to_owned(),
            version: "5".to_owned(),
            title: "Data carriage shapes".to_owned(),
            methodology: ClassifierMethodology::Heuristic,
            semantics: ClassifierSemantics::MultiLabel,
            required_facts: vec![
                "raw_transaction".to_owned(),
                "input_script_pubkeys".to_owned(),
            ],
            labels: vec![
                label(
                    "push_drop_witness",
                    "Push/drop witness carrier",
                    "A revealed witness script contains a large balanced data-push and drop run.",
                ),
                label(
                    "opcode_value_coding",
                    "Opcode-value coding",
                    "A revealed witness script contains a valid self-framed OP_PLENTY opcode sequence.",
                ),
                label(
                    "p2wsh_envelope",
                    "P2WSH conditional envelope",
                    "A committed P2WSH witness script has the exact JXL-n-hide never-taken conditional grammar.",
                ),
                label(
                    "witness_argument_carrier",
                    "Witness-argument carrier",
                    "Large witness arguments are exactly consumed by a drop-only revealed script.",
                ),
                label(
                    "output_key_carrier",
                    "Output-field carrier",
                    "A self-consistent OLGA-style payload spans an exact equal-value P2WSH output run.",
                ),
                label(
                    "off_curve_p2tr",
                    "Off-curve P2TR key",
                    "A P2TR output contains bytes that are not a valid secp256k1 x-only public key.",
                ),
                label(
                    "embedded_file_magic",
                    "Embedded file signature",
                    "Canonical raw transaction bytes contain a strong registered file-format signature.",
                ),
                label(
                    "no_detected_carriage_shape",
                    "No detected carriage shape",
                    "No registered data-carriage shape heuristic fired.",
                ),
            ],
        },
        ClassifierDescriptor {
            id: KNOTS_BIP110_CLASSIFIER_ID.to_owned(),
            version: "1".to_owned(),
            title: "Knots BIP-110 compatibility".to_owned(),
            methodology: ClassifierMethodology::Policy,
            semantics: ClassifierSemantics::RuleSet,
            required_facts: vec![
                "raw_transaction".to_owned(),
                "input_script_pubkeys".to_owned(),
            ],
            labels: vec![
                label(
                    "compatible",
                    "Compatible",
                    "No BIP-110 policy violation or missing fact was found.",
                ),
                label(
                    "violating",
                    "Would violate",
                    "At least one BIP-110 policy violation was proven.",
                ),
                label(
                    "indeterminate",
                    "Indeterminate",
                    "No violation was proven and at least one required fact is missing.",
                ),
            ],
        },
    ]
}

#[cfg(test)]
pub(crate) fn test_classifier_results(assessment: &Bip110Assessment) -> Vec<ClassificationResult> {
    let bip110_label = match assessment.status {
        Bip110Status::Compatible => "compatible",
        Bip110Status::Violating => "violating",
        Bip110Status::Indeterminate => "indeterminate",
    };
    let bip110_partial = !assessment.unknown_rules.is_empty();
    vec![
        ClassificationResult {
            classifier_id: TRANSACTION_PROPERTIES_CLASSIFIER_ID.to_owned(),
            state: ClassificationResultState::Complete,
            primary_label: None,
            labels: vec!["version_2".to_owned(), "p2wpkh".to_owned()],
            missing_facts: Vec::new(),
            evidence: Some(serde_json::json!({ "fixture": true })),
        },
        ClassificationResult {
            classifier_id: TRANSACTION_SHAPE_CLASSIFIER_ID.to_owned(),
            state: ClassificationResultState::Complete,
            primary_label: Some("other_shape".to_owned()),
            labels: vec!["other_shape".to_owned()],
            missing_facts: Vec::new(),
            evidence: Some(serde_json::json!({ "fixture": true })),
        },
        ClassificationResult {
            classifier_id: DATA_PROTOCOLS_CLASSIFIER_ID.to_owned(),
            state: ClassificationResultState::Complete,
            primary_label: Some("no_detected_protocol".to_owned()),
            labels: vec!["no_detected_protocol".to_owned()],
            missing_facts: Vec::new(),
            evidence: Some(serde_json::json!({ "fixture": true })),
        },
        ClassificationResult {
            classifier_id: DATA_CARRIAGE_SHAPE_CLASSIFIER_ID.to_owned(),
            state: ClassificationResultState::Complete,
            primary_label: Some("no_detected_carriage_shape".to_owned()),
            labels: vec!["no_detected_carriage_shape".to_owned()],
            missing_facts: Vec::new(),
            evidence: Some(serde_json::json!({ "fixture": true })),
        },
        ClassificationResult {
            classifier_id: KNOTS_BIP110_CLASSIFIER_ID.to_owned(),
            state: if bip110_partial {
                ClassificationResultState::Partial
            } else {
                ClassificationResultState::Complete
            },
            primary_label: Some(bip110_label.to_owned()),
            labels: vec![bip110_label.to_owned()],
            missing_facts: if bip110_partial {
                vec!["policy_facts".to_owned()]
            } else {
                Vec::new()
            },
            evidence: Some(serde_json::json!({ "fixture": true })),
        },
    ]
}

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

fn classifier_summaries(transactions: &[MempoolEntry]) -> Vec<ClassifierSummary> {
    classifier_catalog()
        .into_iter()
        .map(|descriptor| {
            let mut summary = ClassifierSummary {
                classifier_id: descriptor.id.clone(),
                complete_count: 0,
                partial_count: 0,
                unclassified_count: 0,
                label_counts: descriptor
                    .labels
                    .iter()
                    .map(|label| (label.key.clone(), 0))
                    .collect(),
            };
            for transaction in transactions {
                let Some(result) = transaction
                    .classifications
                    .iter()
                    .find(|result| result.classifier_id == descriptor.id)
                else {
                    summary.unclassified_count += 1;
                    continue;
                };
                match result.state {
                    ClassificationResultState::Complete => summary.complete_count += 1,
                    ClassificationResultState::Partial => summary.partial_count += 1,
                }
                for label in &result.labels {
                    if let Some(count) = summary.label_counts.get_mut(label) {
                        *count += 1;
                    }
                }
            }
            summary
        })
        .collect()
}

/// Membership facts reported by the source node's verbose mempool entry.
/// These are exact source observations, present with every published
/// membership row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MembershipFacts {
    pub weight: u64,
    pub ancestor_count: u64,
    pub ancestor_vsize: u64,
    /// Delta-adjusted ancestor fees, including this transaction. Signed
    /// because `prioritisetransaction` deltas can push the total negative.
    pub ancestor_fee_sats: i64,
    pub descendant_count: u64,
    pub descendant_vsize: u64,
    pub replaceable: bool,
}

impl MembershipFacts {
    /// Minimal self-consistent facts for a transaction with no mempool
    /// relatives, used by construction helpers and tests.
    pub fn solitary(vsize: u64, fee_sats: u64) -> Self {
        Self {
            weight: vsize.saturating_mul(4),
            ancestor_count: 1,
            ancestor_vsize: vsize,
            ancestor_fee_sats: i64::try_from(fee_sats).unwrap_or(i64::MAX),
            descendant_count: 1,
            descendant_vsize: vsize,
            replaceable: false,
        }
    }
}

/// Structural facts derived from the raw transaction during classification.
/// Published progressively: null until the transaction has classifier
/// results, then exact.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransactionStructure {
    pub input_count: u64,
    pub output_count: u64,
    pub op_return_bytes: u64,
    pub recognized_carried_bytes: u64,
    pub output_sats: u64,
    pub witness_bytes: u64,
}

impl TransactionStructure {
    pub fn new(
        input_count: u64,
        output_count: u64,
        op_return_bytes: u64,
        output_sats: u64,
        witness_bytes: u64,
    ) -> Result<Self, ModelError> {
        if input_count == 0 || output_count == 0 {
            return Err(ModelError::EmptyTransactionStructure {
                input_count,
                output_count,
            });
        }
        for (field, value) in [
            ("input_count", input_count),
            ("output_count", output_count),
            ("op_return_bytes", op_return_bytes),
            ("output_sats", output_sats),
            ("witness_bytes", witness_bytes),
        ] {
            if value > MAX_SAFE_JSON_INTEGER {
                return Err(ModelError::UnsafeJsonInteger { field, value });
            }
        }
        Ok(Self {
            input_count,
            output_count,
            op_return_bytes,
            recognized_carried_bytes: op_return_bytes,
            output_sats,
            witness_bytes,
        })
    }

    pub fn with_recognized_carried_bytes(
        mut self,
        recognized_carried_bytes: u64,
    ) -> Result<Self, ModelError> {
        if recognized_carried_bytes < self.op_return_bytes {
            return Err(ModelError::RecognizedCarriedBytesBelowOpReturn {
                recognized_carried_bytes,
                op_return_bytes: self.op_return_bytes,
            });
        }
        if recognized_carried_bytes > MAX_SAFE_JSON_INTEGER {
            return Err(ModelError::UnsafeJsonInteger {
                field: "recognized_carried_bytes",
                value: recognized_carried_bytes,
            });
        }
        self.recognized_carried_bytes = recognized_carried_bytes;
        Ok(self)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MempoolEntry {
    pub txid: String,
    pub wtxid: String,
    pub vsize: u64,
    pub weight: u64,
    pub fee_sats: u64,
    pub entered_at_ms: u64,
    pub ancestor_count: u64,
    pub ancestor_vsize: u64,
    /// Delta-adjusted ancestor fees, including this transaction. Signed
    /// because `prioritisetransaction` deltas can push the total negative.
    pub ancestor_fee_sats: i64,
    pub descendant_count: u64,
    pub descendant_vsize: u64,
    pub replaceable: bool,
    pub structure: Option<TransactionStructure>,
    pub classifications: Vec<ClassificationResult>,
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
        Self::new_variant(
            txid,
            wtxid,
            vsize,
            fee_sats,
            entered_at_ms,
            MembershipFacts::solitary(vsize, fee_sats),
        )
    }

    pub fn new_variant(
        txid: String,
        wtxid: String,
        vsize: u64,
        fee_sats: u64,
        entered_at_ms: u64,
        facts: MembershipFacts,
    ) -> Result<Self, ModelError> {
        if vsize == 0 {
            return Err(ModelError::ZeroVsize);
        }
        for (field, value) in [
            ("vsize", vsize),
            ("fee_sats", fee_sats),
            ("entered_at_ms", entered_at_ms),
            ("weight", facts.weight),
            ("ancestor_count", facts.ancestor_count),
            ("ancestor_vsize", facts.ancestor_vsize),
            ("descendant_count", facts.descendant_count),
            ("descendant_vsize", facts.descendant_vsize),
        ] {
            if value > MAX_SAFE_JSON_INTEGER {
                return Err(ModelError::UnsafeJsonInteger { field, value });
            }
        }
        if facts.ancestor_fee_sats.unsigned_abs() > MAX_SAFE_JSON_INTEGER {
            return Err(ModelError::UnsafeSignedJsonInteger {
                field: "ancestor_fee_sats",
                value: facts.ancestor_fee_sats,
            });
        }
        // Virtual size is at least ceil(weight / 4); sigop-adjusted virtual
        // sizes can exceed it but never fall below it.
        if facts.weight == 0 || facts.weight > vsize.saturating_mul(4) {
            return Err(ModelError::InconsistentWeight {
                vsize,
                weight: facts.weight,
            });
        }
        if facts.ancestor_count == 0
            || facts.descendant_count == 0
            || facts.ancestor_vsize < vsize
            || facts.descendant_vsize < vsize
        {
            return Err(ModelError::InconsistentAncestry {
                vsize,
                ancestor_count: facts.ancestor_count,
                ancestor_vsize: facts.ancestor_vsize,
                descendant_count: facts.descendant_count,
                descendant_vsize: facts.descendant_vsize,
            });
        }
        Ok(Self {
            txid,
            wtxid,
            vsize,
            weight: facts.weight,
            fee_sats,
            entered_at_ms,
            ancestor_count: facts.ancestor_count,
            ancestor_vsize: facts.ancestor_vsize,
            ancestor_fee_sats: facts.ancestor_fee_sats,
            descendant_count: facts.descendant_count,
            descendant_vsize: facts.descendant_vsize,
            replaceable: facts.replaceable,
            structure: None,
            classifications: Vec::new(),
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
    pub collection_started_at_ms: u64,
    pub collection_completed_at_ms: u64,
    pub collection_duration_ms: u64,
    pub observed_at_ms: u64,
    pub classification_revision: u64,
    pub chain_tip: ChainTip,
    pub transaction_count: u64,
    pub total_vsize: u64,
    pub classifier_catalog: Vec<ClassifierDescriptor>,
    pub classification_summaries: Vec<ClassifierSummary>,
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
        Self::new_with_collection_window(
            source_id,
            source_label,
            observed_at_ms,
            observed_at_ms,
            0,
            chain_tip,
            transactions,
        )
    }

    pub fn new_with_collection_window(
        source_id: String,
        source_label: String,
        collection_started_at_ms: u64,
        collection_completed_at_ms: u64,
        collection_duration_ms: u64,
        chain_tip: ChainTip,
        transactions: Vec<MempoolEntry>,
    ) -> Result<Self, ModelError> {
        validate_source_id(&source_id)?;
        validate_source_label(&source_label)?;
        for (field, value) in [
            ("collection_started_at_ms", collection_started_at_ms),
            ("collection_completed_at_ms", collection_completed_at_ms),
            ("collection_duration_ms", collection_duration_ms),
        ] {
            if value > MAX_SAFE_JSON_INTEGER {
                return Err(ModelError::UnsafeJsonInteger { field, value });
            }
        }
        let expected_duration = collection_completed_at_ms
            .checked_sub(collection_started_at_ms)
            .ok_or(ModelError::InvalidCollectionWindow {
                started_at_ms: collection_started_at_ms,
                completed_at_ms: collection_completed_at_ms,
            })?;
        if collection_duration_ms != expected_duration {
            return Err(ModelError::CollectionDurationMismatch {
                started_at_ms: collection_started_at_ms,
                completed_at_ms: collection_completed_at_ms,
                duration_ms: collection_duration_ms,
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
            collection_started_at_ms,
            collection_completed_at_ms,
            collection_duration_ms,
            observed_at_ms: collection_completed_at_ms,
            classification_revision: 0,
            chain_tip,
            transaction_count,
            total_vsize,
            classifier_catalog: classifier_catalog(),
            classification_summaries: classifier_summaries(&transactions),
            bip110_summary: Bip110Summary::from_transactions(&transactions),
            transactions,
        })
    }

    pub fn with_classification_revision(
        mut self,
        classification_revision: u64,
    ) -> Result<Self, ModelError> {
        if classification_revision > MAX_SAFE_JSON_INTEGER {
            return Err(ModelError::UnsafeJsonInteger {
                field: "classification_revision",
                value: classification_revision,
            });
        }
        self.classification_revision = classification_revision;
        Ok(self)
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
    pub evidence: Vec<crate::bip110::Violation>,
    pub missing_count: u64,
    pub missing: Vec<crate::bip110::Missing>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TransactionClassification {
    pub txid: String,
    pub wtxid: String,
    pub structure: TransactionStructure,
    pub results: Vec<ClassificationResult>,
    pub assessment: Bip110Assessment,
    pub rules: Vec<Bip110RuleDetail>,
    /// Exact input outpoints retained only for publication-bound, lazy
    /// conflicting-spend verification. They never enter the ordinary snapshot
    /// or transaction-detail JSON contracts.
    #[serde(skip)]
    pub(crate) input_outpoints: Option<Arc<[OutPoint]>>,
}

pub type TransactionClassifications = BTreeMap<String, Arc<TransactionClassification>>;

#[derive(Clone, Debug)]
pub struct MempoolObservation {
    pub snapshot: MempoolSnapshot,
    pub classifications: TransactionClassifications,
}

impl MempoolObservation {
    /// Materializes one reader-visible observation from classification-free
    /// membership and the exact classifications that still match it.
    pub fn materialize(
        membership: &MempoolSnapshot,
        classification_revision: u64,
        classifications: TransactionClassifications,
    ) -> Result<Self, ModelError> {
        let classifications = membership
            .transactions
            .iter()
            .filter_map(|entry| {
                classifications
                    .get(&entry.txid)
                    .filter(|classification| {
                        classification.txid == entry.txid && classification.wtxid == entry.wtxid
                    })
                    .map(|classification| (entry.txid.clone(), Arc::clone(classification)))
            })
            .collect::<TransactionClassifications>();
        Self::materialize_retained(membership, classification_revision, classifications)
    }

    /// Materializes an observation from a classification map already retained
    /// against this exact membership generation.
    pub(crate) fn materialize_retained(
        membership: &MempoolSnapshot,
        classification_revision: u64,
        classifications: TransactionClassifications,
    ) -> Result<Self, ModelError> {
        let transactions = membership
            .transactions
            .iter()
            .cloned()
            .map(|mut entry| {
                if let Some(classification) = classifications.get(&entry.txid) {
                    entry.classifications = classification
                        .results
                        .iter()
                        .map(ClassificationResult::compact)
                        .collect();
                    entry.bip110 = Some(classification.assessment.clone());
                    entry.structure = Some(classification.structure);
                } else {
                    entry.classifications.clear();
                    entry.bip110 = None;
                    entry.structure = None;
                }
                entry
            })
            .collect();
        let snapshot = MempoolSnapshot::new_with_collection_window(
            membership.source_id.clone(),
            membership.source_label.clone(),
            membership.collection_started_at_ms,
            membership.collection_completed_at_ms,
            membership.collection_duration_ms,
            membership.chain_tip.clone(),
            transactions,
        )?
        .with_classification_revision(classification_revision)?;
        Self::new(snapshot, classifications)
    }

    pub fn new(
        snapshot: MempoolSnapshot,
        classifications: TransactionClassifications,
    ) -> Result<Self, ModelError> {
        if snapshot.classification_revision > MAX_SAFE_JSON_INTEGER {
            return Err(ModelError::UnsafeJsonInteger {
                field: "classification_revision",
                value: snapshot.classification_revision,
            });
        }
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
                || entry.classifications
                    != classification
                        .results
                        .iter()
                        .map(ClassificationResult::compact)
                        .collect::<Vec<_>>()
                || entry.bip110.as_ref() != Some(&classification.assessment)
                || entry.structure != Some(classification.structure)
            {
                return Err(ModelError::ClassificationVariantMismatch(txid.clone()));
            }
        }
        for entry in &snapshot.transactions {
            if (!entry.classifications.is_empty()
                || entry.bip110.is_some()
                || entry.structure.is_some())
                && !classifications.contains_key(&entry.txid)
            {
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
    if let Some(outpoints) = &classification.input_outpoints {
        if u64::try_from(outpoints.len()).ok() != Some(classification.structure.input_count) {
            return Err(ModelError::InvalidClassificationDetail {
                txid: classification.txid.clone(),
                reason: "retained input outpoints do not match the transaction input count",
            });
        }
        if outpoints.iter().any(OutPoint::is_null) {
            return Err(ModelError::InvalidClassificationDetail {
                txid: classification.txid.clone(),
                reason: "retained input outpoints cannot contain a null outpoint",
            });
        }
    }
    validate_classifier_results(&classification.txid, &classification.results)?;
    let policy = classification
        .results
        .iter()
        .find(|result| result.classifier_id == KNOTS_BIP110_CLASSIFIER_ID)
        .expect("canonical classifier results include BIP-110");
    let expected_policy_label = match classification.assessment.status {
        Bip110Status::Compatible => "compatible",
        Bip110Status::Violating => "violating",
        Bip110Status::Indeterminate => "indeterminate",
    };
    let policy_is_partial = !classification.assessment.unknown_rules.is_empty();
    if policy.primary_label.as_deref() != Some(expected_policy_label)
        || policy.labels != [expected_policy_label]
        || policy.state
            != if policy_is_partial {
                ClassificationResultState::Partial
            } else {
                ClassificationResultState::Complete
            }
        || policy.missing_facts
            != if policy_is_partial {
                vec!["policy_facts".to_owned()]
            } else {
                Vec::new()
            }
    {
        return Err(ModelError::InvalidClassificationDetail {
            txid: classification.txid.clone(),
            reason: "BIP-110 classifier result does not match its policy assessment",
        });
    }
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

fn validate_classifier_results(
    txid: &str,
    results: &[ClassificationResult],
) -> Result<(), ModelError> {
    let catalog = classifier_catalog();
    if results.len() != catalog.len() {
        return Err(ModelError::InvalidClassificationDetail {
            txid: txid.to_owned(),
            reason: "must contain every classifier result",
        });
    }
    for (result, descriptor) in results.iter().zip(catalog) {
        if result.classifier_id != descriptor.id {
            return Err(ModelError::InvalidClassificationDetail {
                txid: txid.to_owned(),
                reason: "classifier results must use canonical order",
            });
        }
        // A partial result may prove no label at all: the classifier ran, some
        // required fact was unavailable, and no rule reached a positive
        // terminal state. A complete result must always name what it observed.
        if (result.labels.is_empty() && result.state != ClassificationResultState::Partial)
            || result.evidence.is_none()
            || (result.state == ClassificationResultState::Complete
                && !result.missing_facts.is_empty())
            || (result.state == ClassificationResultState::Partial
                && result.missing_facts.is_empty())
            || result
                .primary_label
                .as_ref()
                .is_some_and(|primary| !result.labels.contains(primary))
        {
            return Err(ModelError::InvalidClassificationDetail {
                txid: txid.to_owned(),
                reason: "classifier result state, labels, or evidence is inconsistent",
            });
        }
        let declared = descriptor
            .labels
            .iter()
            .map(|label| label.key.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let labels = result
            .labels
            .iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        if labels.len() != result.labels.len()
            || !labels.iter().all(|label| declared.contains(label))
        {
            return Err(ModelError::InvalidClassificationDetail {
                txid: txid.to_owned(),
                reason: "classifier result contains duplicate or undeclared labels",
            });
        }
        let missing = result
            .missing_facts
            .iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        if missing.len() != result.missing_facts.len() {
            return Err(ModelError::InvalidClassificationDetail {
                txid: txid.to_owned(),
                reason: "classifier result contains duplicate missing facts",
            });
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TransactionDetailResponse {
    pub source_id: String,
    pub snapshot_observed_at_ms: u64,
    pub classification_revision: u64,
    pub txid: String,
    pub wtxid: String,
    pub classifications: Vec<ClassificationResult>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationState {
    Classifying,
    Complete,
    Paused,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ClassificationProgress {
    pub state: ClassificationState,
    pub revision: u64,
    pub classified_count: u64,
    pub unclassified_count: u64,
}

impl ClassificationProgress {
    pub(crate) fn from_snapshot(state: ClassificationState, snapshot: &MempoolSnapshot) -> Self {
        let summary = &snapshot.bip110_summary;
        Self {
            state,
            revision: snapshot.classification_revision,
            classified_count: summary.compatible_count
                + summary.violating_count
                + summary.indeterminate_count,
            unclassified_count: summary.unclassified_count,
        }
    }
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
    pub classification: Option<ClassificationProgress>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SourcesResponse {
    pub atlas_version: String,
    pub sources: Vec<SourceSummary>,
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
    #[error("mempool entry weight {weight} is inconsistent with virtual size {vsize}")]
    InconsistentWeight { vsize: u64, weight: u64 },
    #[error(
        "mempool entry ancestry (ancestors {ancestor_count} in {ancestor_vsize} vB, descendants {descendant_count} in {descendant_vsize} vB) is inconsistent with virtual size {vsize}"
    )]
    InconsistentAncestry {
        vsize: u64,
        ancestor_count: u64,
        ancestor_vsize: u64,
        descendant_count: u64,
        descendant_vsize: u64,
    },
    #[error(
        "transaction structure must describe at least one input and one output, got {input_count} and {output_count}"
    )]
    EmptyTransactionStructure { input_count: u64, output_count: u64 },
    #[error(
        "recognized carried bytes {recognized_carried_bytes} cannot be below OP_RETURN bytes {op_return_bytes}"
    )]
    RecognizedCarriedBytesBelowOpReturn {
        recognized_carried_bytes: u64,
        op_return_bytes: u64,
    },
    #[error("{field} value {value} cannot be represented exactly in JSON")]
    UnsafeJsonInteger { field: &'static str, value: u64 },
    #[error("{field} value {value} cannot be represented exactly in JSON")]
    UnsafeSignedJsonInteger { field: &'static str, value: i64 },
    #[error(
        "snapshot collection completed at {completed_at_ms} ms before it started at {started_at_ms} ms"
    )]
    InvalidCollectionWindow {
        started_at_ms: u64,
        completed_at_ms: u64,
    },
    #[error(
        "snapshot collection duration {duration_ms} ms does not match the window from {started_at_ms} ms to {completed_at_ms} ms"
    )]
    CollectionDurationMismatch {
        started_at_ms: u64,
        completed_at_ms: u64,
        duration_ms: u64,
    },
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

    fn test_structure() -> TransactionStructure {
        TransactionStructure::new(1, 2, 0, 50_000, 107).expect("structure")
    }

    #[test]
    fn recognized_carriage_cannot_erase_op_return_bytes() {
        let structure = TransactionStructure::new(1, 1, 80, 1_000, 0).expect("structure");
        assert_eq!(structure.recognized_carried_bytes, 80);
        assert!(structure.with_recognized_carried_bytes(79).is_err());
        assert_eq!(
            structure
                .with_recognized_carried_bytes(1_610)
                .expect("recognized carriage")
                .recognized_carried_bytes,
            1_610
        );
    }

    fn compatible_classification(txid: &str, wtxid: &str) -> Arc<TransactionClassification> {
        let assessment = Bip110Assessment {
            status: Bip110Status::Compatible,
            primary_rule: None,
            violated_rules: Vec::new(),
            unknown_rules: Vec::new(),
        };
        Arc::new(TransactionClassification {
            txid: txid.to_owned(),
            wtxid: wtxid.to_owned(),
            structure: test_structure(),
            results: test_classifier_results(&assessment),
            assessment,
            rules: Bip110RuleId::ALL
                .into_iter()
                .map(|rule| Bip110RuleDetail {
                    rule,
                    number: rule.number(),
                    verdict: Bip110RuleVerdict::Pass,
                    evidence_count: 0,
                    evidence: Vec::new(),
                    missing_count: 0,
                    missing: Vec::new(),
                })
                .collect(),
            input_outpoints: None,
        })
    }

    fn retained_outpoint(marker: &str, vout: u32) -> OutPoint {
        OutPoint {
            txid: marker.repeat(32).parse().expect("outpoint txid"),
            vout,
        }
    }

    #[test]
    fn classification_validates_private_input_outpoint_cardinality() {
        let mut classification = compatible_classification("00", "10");
        Arc::make_mut(&mut classification).input_outpoints =
            Some(vec![retained_outpoint("11", 0), retained_outpoint("22", 1)].into());

        assert!(matches!(
            validate_classification(&classification),
            Err(ModelError::InvalidClassificationDetail { reason, .. })
                if reason.contains("input count")
        ));
    }

    #[test]
    fn classification_rejects_a_private_null_input_outpoint() {
        let mut classification = compatible_classification("00", "10");
        Arc::make_mut(&mut classification).input_outpoints = Some(vec![OutPoint::null()].into());

        assert!(matches!(
            validate_classification(&classification),
            Err(ModelError::InvalidClassificationDetail { reason, .. })
                if reason.contains("null outpoint")
        ));
    }

    #[test]
    fn classification_accepts_matching_private_input_outpoints() {
        let mut classification = compatible_classification("00", "10");
        Arc::make_mut(&mut classification).input_outpoints =
            Some(vec![retained_outpoint("33", 7)].into());

        validate_classification(&classification).expect("valid retained outpoints");
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
        assert_eq!(snapshot.collection_started_at_ms, 1_700_000_000_100);
        assert_eq!(snapshot.collection_completed_at_ms, 1_700_000_000_100);
        assert_eq!(snapshot.collection_duration_ms, 0);
        assert_eq!(snapshot.observed_at_ms, snapshot.collection_completed_at_ms);
    }

    #[test]
    fn snapshot_validates_collection_window() {
        let chain_tip = ChainTip {
            height: 900_000,
            hash: "00".repeat(32),
        };
        let snapshot = MempoolSnapshot::new_with_collection_window(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            1_700_000_000_000,
            1_700_000_000_125,
            125,
            chain_tip.clone(),
            vec![entry("00", 100)],
        )
        .expect("snapshot");

        assert_eq!(snapshot.collection_started_at_ms, 1_700_000_000_000);
        assert_eq!(snapshot.collection_completed_at_ms, 1_700_000_000_125);
        assert_eq!(snapshot.collection_duration_ms, 125);
        assert_eq!(snapshot.observed_at_ms, snapshot.collection_completed_at_ms);

        assert!(matches!(
            MempoolSnapshot::new_with_collection_window(
                "core".to_owned(),
                "Bitcoin Core".to_owned(),
                200,
                100,
                0,
                chain_tip.clone(),
                Vec::new(),
            ),
            Err(ModelError::InvalidCollectionWindow {
                started_at_ms: 200,
                completed_at_ms: 100,
            })
        ));
        assert!(matches!(
            MempoolSnapshot::new_with_collection_window(
                "core".to_owned(),
                "Bitcoin Core".to_owned(),
                100,
                200,
                99,
                chain_tip.clone(),
                Vec::new(),
            ),
            Err(ModelError::CollectionDurationMismatch {
                duration_ms: 99,
                ..
            })
        ));
        assert!(matches!(
            MempoolSnapshot::new_with_collection_window(
                "core".to_owned(),
                "Bitcoin Core".to_owned(),
                MAX_SAFE_JSON_INTEGER + 1,
                MAX_SAFE_JSON_INTEGER + 1,
                0,
                chain_tip,
                Vec::new(),
            ),
            Err(ModelError::UnsafeJsonInteger {
                field: "collection_started_at_ms",
                ..
            })
        ));
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
    fn observation_materializes_exact_current_classifications() {
        let membership = MempoolSnapshot::new_with_collection_window(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            1,
            5,
            4,
            ChainTip {
                height: 1,
                hash: "00".repeat(32),
            },
            vec![
                MempoolEntry::new_variant(
                    "00".to_owned(),
                    "10".to_owned(),
                    100,
                    100,
                    1,
                    MembershipFacts::solitary(100, 100),
                )
                .expect("entry"),
                MempoolEntry::new_variant(
                    "01".to_owned(),
                    "11".to_owned(),
                    100,
                    100,
                    1,
                    MembershipFacts::solitary(100, 100),
                )
                .expect("entry"),
            ],
        )
        .expect("membership");
        let matching = compatible_classification("00", "10");
        let stale_variant = compatible_classification("01", "12");

        let observation = MempoolObservation::materialize(
            &membership,
            1,
            BTreeMap::from([
                ("00".to_owned(), Arc::clone(&matching)),
                ("01".to_owned(), stale_variant),
            ]),
        )
        .expect("observation");

        assert_eq!(observation.snapshot.classification_revision, 1);
        assert_eq!(observation.snapshot.collection_started_at_ms, 1);
        assert_eq!(observation.snapshot.collection_completed_at_ms, 5);
        assert_eq!(observation.snapshot.collection_duration_ms, 4);
        assert_eq!(observation.snapshot.observed_at_ms, 5);
        assert!(membership.transactions[0].bip110.is_none());
        assert_eq!(
            observation.snapshot.transactions[0].bip110.as_ref(),
            Some(&matching.assessment)
        );
        assert!(observation.snapshot.transactions[1].bip110.is_none());
        assert_eq!(observation.snapshot.bip110_summary.compatible_count, 1);
        assert_eq!(observation.snapshot.bip110_summary.unclassified_count, 1);
        assert_eq!(
            observation
                .classifications
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["00"]
        );
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
                            crate::bip110::Missing::ScriptPubKey { input: 0 },
                            crate::bip110::Missing::ScriptPubKey { input: 1 },
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
            structure: test_structure(),
            results: test_classifier_results(&assessment),
            assessment,
            rules,
            input_outpoints: None,
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
