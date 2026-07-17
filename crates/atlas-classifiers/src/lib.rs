//! Required MVP contract for in-process classifier rule packs.
//!
//! Classifiers are compiled into the server, not loaded dynamically. The
//! server parses observed raw transaction bytes once and hands every
//! classifier the parsed [`bitcoin::Transaction`]; classifiers never receive
//! raw bytes. A classifier states what it could derive instead of guessing:
//! absent facts produce a non-`Complete` status, never a fabricated verdict.

mod baseline;
mod shape;

#[cfg(test)]
mod test_support;

use atlas_model::Classification;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use baseline::BaselineHeuristics;
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
/// reports `Complete` with the explicit verdict
/// [`Classification::Unknown`] instead.
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
/// `status` is [`ClassificationStatus::Complete`]; every other status
/// explains itself through `evidence` instead of guessing a verdict.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ClassificationResult {
    pub classifier_id: String,
    pub classifier_version: String,
    pub status: ClassificationStatus,
    pub verdict: Option<Classification>,
    pub evidence: Value,
}

/// One in-process rule pack. Implementations must be pure over their input:
/// the same input always yields the same result for one classifier version.
pub trait Classifier: Send + Sync {
    fn manifest(&self) -> ClassifierManifest;
    fn classify(&self, input: &ClassificationInput<'_>) -> ClassificationResult;
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
            verdict: Some(Classification::Payment),
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
}
