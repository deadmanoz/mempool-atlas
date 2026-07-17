//! Order-dependent baseline shape heuristics.
//!
//! Every rule in this pack is a documented heuristic tell over transaction
//! shape, not ground truth: a matching shape is consistent with the verdict,
//! never proof of intent. Rules apply in a fixed order, the first match wins,
//! and every verdict names exactly one rule in its evidence.

use std::collections::HashMap;

use atlas_model::Classification;
use bitcoin::Transaction;
use serde_json::{Value, json};

use crate::{
    ClassificationInput, ClassificationResult, ClassificationStatus, Classifier,
    ClassifierManifest, shape::TransactionShape,
};

const CLASSIFIER_ID: &str = "baseline-heuristics";
const CLASSIFIER_VERSION: &str = "0.1.0";

/// `OP_FALSE OP_IF OP_PUSHBYTES_3 "ord"`, the byte sequence opening an
/// inscription envelope inside a witness element.
const INSCRIPTION_ENVELOPE_MARKER: [u8; 6] = [0x00, 0x63, 0x03, 0x6f, 0x72, 0x64];

/// Commitment-transaction anchor outputs carry exactly 330 satoshis.
const ANCHOR_OUTPUT_SATS: u64 = 330;

const COINJOIN_MIN_INPUTS: u64 = 5;
const COINJOIN_MIN_OUTPUTS: u64 = 5;
const COINJOIN_MIN_EQUAL_OUTPUTS: u64 = 3;
const CONSOLIDATION_MIN_INPUTS: u64 = 10;
const CONSOLIDATION_MAX_OUTPUTS: u64 = 2;
const BATCH_MIN_OUTPUTS: u64 = 20;
const PAYMENT_MAX_INPUTS: u64 = 3;
const PAYMENT_MAX_OUTPUTS: u64 = 2;

/// The built-in shape rule pack. Without a parsed transaction it reports
/// [`ClassificationStatus::Unknown`]; with one it always completes, falling
/// back to the explicit verdict [`Classification::Unknown`] when no rule
/// matches.
#[derive(Clone, Copy, Debug, Default)]
pub struct BaselineHeuristics;

impl Classifier for BaselineHeuristics {
    fn manifest(&self) -> ClassifierManifest {
        ClassifierManifest {
            id: CLASSIFIER_ID.to_owned(),
            version: CLASSIFIER_VERSION.to_owned(),
            required_facts: vec!["raw_transaction".to_owned()],
        }
    }

    fn classify(&self, input: &ClassificationInput<'_>) -> ClassificationResult {
        let Some(transaction) = input.transaction else {
            return result(
                ClassificationStatus::Unknown,
                None,
                json!({ "missing": "raw_transaction" }),
            );
        };
        let (verdict, evidence) =
            first_matching_rule(transaction, TransactionShape::derive(transaction));
        result(ClassificationStatus::Complete, Some(verdict), evidence)
    }
}

fn first_matching_rule(
    transaction: &Transaction,
    shape: TransactionShape,
) -> (Classification, Value) {
    if shape.input_count >= COINJOIN_MIN_INPUTS
        && shape.output_count >= COINJOIN_MIN_OUTPUTS
        && let Some((value_sats, count)) = most_repeated_nonzero_output_value(transaction)
        && count >= COINJOIN_MIN_EQUAL_OUTPUTS
    {
        return (
            Classification::Coinjoin,
            json!({
                "rule": "coinjoin",
                "input_count": shape.input_count,
                "output_count": shape.output_count,
                "equal_output_value_sats": value_sats,
                "equal_output_count": count,
            }),
        );
    }
    if shape.input_count >= CONSOLIDATION_MIN_INPUTS
        && shape.output_count <= CONSOLIDATION_MAX_OUTPUTS
    {
        return (
            Classification::Consolidation,
            json!({
                "rule": "consolidation",
                "input_count": shape.input_count,
                "output_count": shape.output_count,
            }),
        );
    }
    if shape.output_count >= BATCH_MIN_OUTPUTS {
        return (
            Classification::Batch,
            json!({ "rule": "batch", "output_count": shape.output_count }),
        );
    }
    let op_return_output_count = transaction
        .output
        .iter()
        .filter(|output| output.script_pubkey.is_op_return())
        .count();
    if op_return_output_count > 0 {
        return (
            Classification::Data,
            json!({ "rule": "data", "op_return_output_count": op_return_output_count }),
        );
    }
    if let Some(input_index) = inscription_envelope_input(transaction) {
        return (
            Classification::Data,
            json!({ "rule": "data", "inscription_envelope_input": input_index }),
        );
    }
    if let Some(output_index) = anchor_output(transaction) {
        return (
            Classification::Lightning,
            json!({
                "rule": "lightning",
                "anchor_output_index": output_index,
                "anchor_value_sats": ANCHOR_OUTPUT_SATS,
            }),
        );
    }
    if shape.input_count <= PAYMENT_MAX_INPUTS && shape.output_count <= PAYMENT_MAX_OUTPUTS {
        return (
            Classification::Payment,
            json!({
                "rule": "payment",
                "input_count": shape.input_count,
                "output_count": shape.output_count,
            }),
        );
    }
    (Classification::Unknown, json!({ "rule": "no_match" }))
}

/// The most repeated non-zero output value and its repetition count. Ties
/// break deterministically toward the larger value.
fn most_repeated_nonzero_output_value(transaction: &Transaction) -> Option<(u64, u64)> {
    let mut counts: HashMap<u64, u64> = HashMap::new();
    for output in &transaction.output {
        let value_sats = output.value.to_sat();
        if value_sats > 0 {
            *counts.entry(value_sats).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|&(value_sats, count)| (count, value_sats))
}

fn inscription_envelope_input(transaction: &Transaction) -> Option<usize> {
    transaction.input.iter().position(|input| {
        input.witness.iter().any(|element| {
            element
                .windows(INSCRIPTION_ENVELOPE_MARKER.len())
                .any(|window| window == INSCRIPTION_ENVELOPE_MARKER)
        })
    })
}

fn anchor_output(transaction: &Transaction) -> Option<usize> {
    transaction.output.iter().position(|output| {
        output.value.to_sat() == ANCHOR_OUTPUT_SATS
            && (output.script_pubkey.is_p2wsh() || output.script_pubkey.is_p2tr())
    })
}

fn result(
    status: ClassificationStatus,
    verdict: Option<Classification>,
    evidence: Value,
) -> ClassificationResult {
    ClassificationResult {
        classifier_id: CLASSIFIER_ID.to_owned(),
        classifier_version: CLASSIFIER_VERSION.to_owned(),
        status,
        verdict,
        evidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        op_return_script, output, p2tr_script, p2wpkh_script, p2wsh_script, transaction_from,
        transaction_with, witness_input,
    };

    const TXID: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    fn classify(transaction: &Transaction) -> ClassificationResult {
        BaselineHeuristics.classify(&ClassificationInput {
            txid: TXID,
            wtxid: None,
            transaction: Some(transaction),
        })
    }

    #[test]
    fn manifest_declares_identity_and_required_facts() {
        let manifest = BaselineHeuristics.manifest();
        assert_eq!(manifest.id, "baseline-heuristics");
        assert_eq!(manifest.version, "0.1.0");
        assert_eq!(manifest.required_facts, vec!["raw_transaction".to_owned()]);
    }

    #[test]
    fn missing_transaction_yields_unknown_status_without_verdict() {
        let result = BaselineHeuristics.classify(&ClassificationInput {
            txid: TXID,
            wtxid: None,
            transaction: None,
        });
        assert_eq!(result.status, ClassificationStatus::Unknown);
        assert_eq!(result.verdict, None);
        assert_eq!(result.evidence, json!({ "missing": "raw_transaction" }));
    }

    #[test]
    fn coinjoin_matches_repeated_equal_output_values() {
        let transaction = transaction_with(
            5,
            vec![
                output(10_000, p2wpkh_script()),
                output(10_000, p2wpkh_script()),
                output(10_000, p2wpkh_script()),
                output(4_400, p2wpkh_script()),
                output(3_300, p2wpkh_script()),
            ],
        );
        let result = classify(&transaction);
        assert_eq!(result.status, ClassificationStatus::Complete);
        assert_eq!(result.verdict, Some(Classification::Coinjoin));
        assert_eq!(
            result.evidence,
            json!({
                "rule": "coinjoin",
                "input_count": 5,
                "output_count": 5,
                "equal_output_value_sats": 10_000,
                "equal_output_count": 3,
            })
        );
    }

    #[test]
    fn coinjoin_ignores_zero_valued_equal_outputs() {
        let transaction = transaction_with(
            5,
            vec![
                output(0, p2wpkh_script()),
                output(0, p2wpkh_script()),
                output(0, p2wpkh_script()),
                output(4_400, p2wpkh_script()),
                output(3_300, p2wpkh_script()),
            ],
        );
        assert_eq!(
            classify(&transaction).verdict,
            Some(Classification::Unknown)
        );
    }

    #[test]
    fn rule_order_prefers_coinjoin_over_batch() {
        let transaction =
            transaction_with(5, (0..20).map(|_| output(5_000, p2wpkh_script())).collect());
        assert_eq!(
            classify(&transaction).verdict,
            Some(Classification::Coinjoin)
        );
    }

    #[test]
    fn consolidation_matches_many_inputs_into_few_outputs() {
        let transaction = transaction_with(10, vec![output(1_000_000, p2wpkh_script())]);
        let result = classify(&transaction);
        assert_eq!(result.verdict, Some(Classification::Consolidation));
        assert_eq!(
            result.evidence,
            json!({ "rule": "consolidation", "input_count": 10, "output_count": 1 })
        );
    }

    #[test]
    fn batch_matches_many_distinct_outputs() {
        let transaction = transaction_with(
            2,
            (0..20)
                .map(|index| output(1_000 + index, p2wpkh_script()))
                .collect(),
        );
        let result = classify(&transaction);
        assert_eq!(result.verdict, Some(Classification::Batch));
        assert_eq!(
            result.evidence,
            json!({ "rule": "batch", "output_count": 20 })
        );
    }

    #[test]
    fn data_matches_an_op_return_output_before_payment() {
        let transaction = transaction_with(
            1,
            vec![output(900, p2wpkh_script()), output(0, op_return_script())],
        );
        let result = classify(&transaction);
        assert_eq!(result.verdict, Some(Classification::Data));
        assert_eq!(
            result.evidence,
            json!({ "rule": "data", "op_return_output_count": 1 })
        );
    }

    #[test]
    fn data_matches_an_inscription_envelope_inside_a_witness_element() {
        let mut element = vec![0xaa, 0xbb];
        element.extend_from_slice(&INSCRIPTION_ENVELOPE_MARKER);
        element.push(0xcc);
        let transaction = transaction_from(
            vec![witness_input(&[element])],
            vec![output(546, p2tr_script())],
        );
        let result = classify(&transaction);
        assert_eq!(result.verdict, Some(Classification::Data));
        assert_eq!(
            result.evidence,
            json!({ "rule": "data", "inscription_envelope_input": 0 })
        );
    }

    #[test]
    fn lightning_matches_a_330_sat_anchor_output() {
        let p2tr_anchor = transaction_with(
            1,
            vec![output(250_000, p2wpkh_script()), output(330, p2tr_script())],
        );
        let result = classify(&p2tr_anchor);
        assert_eq!(result.verdict, Some(Classification::Lightning));
        assert_eq!(
            result.evidence,
            json!({
                "rule": "lightning",
                "anchor_output_index": 1,
                "anchor_value_sats": 330,
            })
        );

        let p2wsh_anchor = transaction_with(
            1,
            vec![
                output(330, p2wsh_script()),
                output(250_000, p2wpkh_script()),
            ],
        );
        assert_eq!(
            classify(&p2wsh_anchor).verdict,
            Some(Classification::Lightning)
        );
    }

    #[test]
    fn near_anchor_values_and_non_anchor_scripts_stay_payments() {
        let off_by_one = transaction_with(
            1,
            vec![output(250_000, p2wpkh_script()), output(331, p2tr_script())],
        );
        assert_eq!(classify(&off_by_one).verdict, Some(Classification::Payment));

        let wrong_script = transaction_with(1, vec![output(330, p2wpkh_script())]);
        assert_eq!(
            classify(&wrong_script).verdict,
            Some(Classification::Payment)
        );
    }

    #[test]
    fn payment_matches_small_transactions() {
        let transaction = transaction_with(
            2,
            vec![
                output(90_000, p2wpkh_script()),
                output(9_000, p2tr_script()),
            ],
        );
        let result = classify(&transaction);
        assert_eq!(result.status, ClassificationStatus::Complete);
        assert_eq!(result.verdict, Some(Classification::Payment));
        assert_eq!(
            result.evidence,
            json!({ "rule": "payment", "input_count": 2, "output_count": 2 })
        );
    }

    #[test]
    fn unmatched_shapes_fall_back_to_an_explicit_unknown_verdict() {
        let transaction = transaction_with(
            4,
            (0..4)
                .map(|index| output(1_000 + index, p2wpkh_script()))
                .collect(),
        );
        let result = classify(&transaction);
        assert_eq!(result.status, ClassificationStatus::Complete);
        assert_eq!(result.verdict, Some(Classification::Unknown));
        assert_eq!(result.evidence, json!({ "rule": "no_match" }));
    }
}
