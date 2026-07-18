//! Required MVP contract for in-process classifier rule packs.
//!
//! Classifiers are compiled into the server, not loaded dynamically. The
//! server parses observed raw transaction bytes once and hands every
//! classifier the parsed [`bitcoin::Transaction`]; classifiers never receive
//! raw bytes. A classifier states what it could derive instead of guessing:
//! absent facts produce a non-`Complete` status, never a fabricated verdict.
//!
//! Each pack owns one taxonomy: a declared verdict vocabulary published as
//! [`TaxonomyDescriptor`] data. Verdicts travel as keys into that
//! vocabulary, so new taxonomies (like the BIP-110 conformance pack and the
//! data-carrying-protocol pack) add packs instead of widening a shared enum.

mod baseline;
mod bip110;
mod data_protocol;
mod shape;
mod witness;

#[cfg(test)]
mod test_support;

use atlas_model::TaxonomyDescriptor;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use baseline::BaselineHeuristics;
pub use bip110::Bip110Conformance;
pub use data_protocol::{DataProtocolFingerprints, arc4};
pub use shape::TransactionShape;

/// Identity and declared input facts of one classifier rule pack.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClassifierManifest {
    pub id: String,
    pub version: String,
    pub required_facts: Vec<String>,
}

/// How far one classification run got. `Unknown` states that required facts
/// were unavailable; a classifier that ran to completion without matching
/// reports `Complete` with its taxonomy's explicit `unknown` verdict instead.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationStatus {
    Complete,
    Partial,
    Unknown,
    NotApplicable,
    Error,
}

/// Facts offered to one classification run for one transaction.
#[derive(Clone, Copy, Debug)]
pub struct ClassificationInput<'a> {
    pub txid: &'a str,
    pub wtxid: Option<&'a str>,
    /// The transaction the server parsed once from observed raw bytes, when
    /// any were observed for this membership.
    pub transaction: Option<&'a bitcoin::Transaction>,
}

/// One classifier's statement about one transaction. `verdict` is `Some` iff
/// `status` is [`ClassificationStatus::Complete`] or
/// [`ClassificationStatus::Partial`] (a `Partial` verdict is honest but
/// derived from incomplete evidence, e.g. BIP-110's "indeterminate"),
/// and its value must be one of the verdict keys the pack's
/// [`TaxonomyDescriptor`] declares. Every other status explains itself
/// through `evidence` instead of guessing a verdict.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ClassificationResult {
    pub classifier_id: String,
    pub classifier_version: String,
    pub status: ClassificationStatus,
    pub verdict: Option<String>,
    pub evidence: Value,
}

/// One in-process rule pack. Implementations must be pure over their input:
/// the same input always yields the same result for one classifier version.
/// Each pack owns exactly one taxonomy and only ever emits verdict keys that
/// taxonomy declares.
pub trait Classifier: Send + Sync {
    fn manifest(&self) -> ClassifierManifest;
    fn taxonomy(&self) -> TaxonomyDescriptor;
    fn classify(&self, input: &ClassificationInput<'_>) -> ClassificationResult;
}

/// Every classifier pack compiled into this build, in stable registration
/// order. The server derives one classification row per pack per
/// transaction, so each transaction receives one verdict per taxonomy.
#[must_use]
pub fn registered_packs() -> Vec<Box<dyn Classifier>> {
    vec![
        Box::new(BaselineHeuristics),
        Box::new(Bip110Conformance),
        Box::new(DataProtocolFingerprints),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_statuses_round_trip_as_snake_case() {
        for (status, wire) in [
            (ClassificationStatus::Complete, "complete"),
            (ClassificationStatus::Partial, "partial"),
            (ClassificationStatus::Unknown, "unknown"),
            (ClassificationStatus::NotApplicable, "not_applicable"),
            (ClassificationStatus::Error, "error"),
        ] {
            assert_eq!(
                serde_json::to_value(status).expect("serialize"),
                serde_json::json!(wire)
            );
            assert_eq!(
                serde_json::from_value::<ClassificationStatus>(serde_json::json!(wire))
                    .expect("deserialize"),
                status
            );
        }
    }

    #[test]
    fn classification_results_have_explicit_wire_shapes() {
        let result = ClassificationResult {
            classifier_id: "baseline-heuristics".to_owned(),
            classifier_version: "0.1.0".to_owned(),
            status: ClassificationStatus::Complete,
            verdict: Some("payment".to_owned()),
            evidence: serde_json::json!({ "rule": "payment" }),
        };
        assert_eq!(
            serde_json::to_value(&result).expect("serialize"),
            serde_json::json!({
                "classifier_id": "baseline-heuristics",
                "classifier_version": "0.1.0",
                "status": "complete",
                "verdict": "payment",
                "evidence": { "rule": "payment" },
            })
        );
    }

    #[test]
    fn registration_order_defines_the_wire_taxonomy_order() {
        let keys: Vec<String> = registered_packs()
            .iter()
            .map(|pack| pack.taxonomy().key)
            .collect();
        assert_eq!(
            keys,
            vec![
                "behavior".to_owned(),
                "bip110".to_owned(),
                "data_protocol".to_owned(),
            ]
        );
    }

    #[test]
    fn every_registered_pack_declares_a_well_formed_taxonomy() {
        let packs = registered_packs();
        assert!(!packs.is_empty());
        for pack in packs {
            let taxonomy = pack.taxonomy();
            let mut seen = std::collections::HashSet::new();
            for verdict in &taxonomy.verdicts {
                assert!(
                    !verdict.key.is_empty()
                        && verdict.key.chars().all(|character| {
                            character.is_ascii_lowercase()
                                || character.is_ascii_digit()
                                || character == '_'
                        }),
                    "taxonomy {} verdict key {:?} is not lowercase [a-z0-9_]+",
                    taxonomy.key,
                    verdict.key,
                );
                assert!(
                    seen.insert(verdict.key.as_str()),
                    "taxonomy {} declares duplicate verdict key {:?}",
                    taxonomy.key,
                    verdict.key,
                );
            }
            assert!(
                seen.contains("unknown"),
                "taxonomy {} is missing the required unknown verdict",
                taxonomy.key,
            );
        }
    }
}
