//! Pure, independently specified transaction classifier lenses.
//!
//! The behavioral contract lives in `docs/classification.md`. This module is
//! intentionally implemented from that contract and does not incorporate a
//! third-party explorer's classifier source.

use std::collections::{BTreeMap, BTreeSet};

use crate::bip110::PrevoutSet;
use bitcoin::script::Instruction;
use bitcoin::{Script, Transaction, opcodes};
use serde_json::{Value, json};

use crate::model::{
    Bip110Assessment, Bip110Status, ClassificationResult, ClassificationResultState,
    DATA_PROTOCOLS_CLASSIFIER_ID, KNOTS_BIP110_CLASSIFIER_ID, ModelError,
    TRANSACTION_PROPERTIES_CLASSIFIER_ID, TRANSACTION_SHAPE_CLASSIFIER_ID, TransactionStructure,
};

const ORD_ENVELOPE_BYTES: [u8; 6] = [0x00, 0x63, 0x03, 0x6f, 0x72, 0x64];
const ORD_PROTOCOL_ID: &[u8] = b"ord";
const BRC20_PROTOCOL_MARKER: &[u8] = br#""p":"brc-20""#;
const CNTRPRTY_PREFIX: &[u8] = b"CNTRPRTY";
const OMNI_PREFIX: &[u8] = b"omni";
const COINJOIN_MIN_INPUTS: usize = 5;
const COINJOIN_MIN_OUTPUTS: usize = 5;
const COINJOIN_MIN_EQUAL_OUTPUTS: usize = 3;
const SHAPE_RATIO: usize = 5;
const MAX_DETECTION_EVIDENCE: usize = 16;

const PROPERTY_LABEL_ORDER: [&str; 18] = [
    "version_1",
    "version_2",
    "version_3",
    "version_other",
    "signals_rbf",
    "has_witness",
    "has_taproot_annex",
    "p2pk",
    "bare_multisig",
    "p2pkh",
    "p2sh",
    "p2wpkh",
    "p2wsh",
    "p2tr",
    "p2a",
    "unknown_witness_program",
    "op_return",
    "unknown_script",
];

const SHAPE_LABEL_ORDER: [&str; 4] = [
    "possible_coinjoin",
    "consolidation",
    "batch_payout",
    "other_shape",
];

const DATA_LABEL_ORDER: [&str; 8] = [
    "inscription",
    "brc20",
    "runes",
    "stamps",
    "counterparty",
    "omni",
    "other_op_return",
    "no_detected_protocol",
];

/// Derives exact structural facts from one decoded raw transaction. Uses no
/// spent-output facts, so the result is available whenever the raw
/// transaction is.
pub fn transaction_structure(
    transaction: &Transaction,
) -> Result<TransactionStructure, ModelError> {
    let input_count = u64::try_from(transaction.input.len()).unwrap_or(u64::MAX);
    let output_count = u64::try_from(transaction.output.len()).unwrap_or(u64::MAX);
    let mut op_return_bytes = 0_u64;
    let mut output_sats = 0_u64;
    for output in &transaction.output {
        output_sats = output_sats.saturating_add(output.value.to_sat());
        if output.script_pubkey.is_op_return() {
            let payload = op_return_payload(output.script_pubkey.as_bytes());
            op_return_bytes =
                op_return_bytes.saturating_add(u64::try_from(payload.len()).unwrap_or(u64::MAX));
        }
    }
    let witness_bytes = u64::try_from(
        transaction
            .total_size()
            .saturating_sub(transaction.base_size()),
    )
    .unwrap_or(u64::MAX);
    TransactionStructure::new(
        input_count,
        output_count,
        op_return_bytes,
        output_sats,
        witness_bytes,
    )
}

/// Classifies one exact witness variant after the shared fact resolver has
/// reached a terminal state for all of its spent-output scripts.
pub fn classify_transaction(
    transaction: &Transaction,
    prevouts: &PrevoutSet,
    bip110: &Bip110Assessment,
) -> Vec<ClassificationResult> {
    let data = data_protocols(transaction);
    vec![
        transaction_properties(transaction, prevouts),
        transaction_shape(transaction, prevouts, &data),
        data,
        bip110_result(bip110),
    ]
}

fn result(
    classifier_id: &str,
    state: ClassificationResultState,
    primary_label: Option<&str>,
    labels: Vec<String>,
    missing_facts: Vec<String>,
    evidence: Value,
) -> ClassificationResult {
    ClassificationResult {
        classifier_id: classifier_id.to_owned(),
        state,
        primary_label: primary_label.map(str::to_owned),
        labels,
        missing_facts,
        evidence: Some(evidence),
    }
}

fn ordered_labels<const N: usize>(found: &BTreeSet<&'static str>, order: [&str; N]) -> Vec<String> {
    order
        .into_iter()
        .filter(|label| found.contains(label))
        .map(str::to_owned)
        .collect()
}

fn transaction_properties(
    transaction: &Transaction,
    prevouts: &PrevoutSet,
) -> ClassificationResult {
    let mut found = BTreeSet::new();
    found.insert(match transaction.version.0 {
        1 => "version_1",
        2 => "version_2",
        3 => "version_3",
        _ => "version_other",
    });
    let rbf_input_count = transaction
        .input
        .iter()
        .filter(|input| input.sequence.is_rbf())
        .count();
    if rbf_input_count > 0 {
        found.insert("signals_rbf");
    }
    let witness_input_count = transaction
        .input
        .iter()
        .filter(|input| !input.witness.is_empty())
        .count();
    if witness_input_count > 0 {
        found.insert("has_witness");
    }

    let mut family_counts = BTreeMap::<&'static str, usize>::new();
    for output in &transaction.output {
        let family = script_family(&output.script_pubkey);
        found.insert(family);
        *family_counts.entry(family).or_default() += 1;
    }

    let mut known_input_scripts = 0_usize;
    let mut missing_input_scripts = 0_usize;
    let mut annex_input_count = 0_usize;
    for (index, input) in transaction.input.iter().enumerate() {
        let Some(prevout) = prevouts.get(index) else {
            missing_input_scripts += 1;
            continue;
        };
        known_input_scripts += 1;
        let family = script_family(&prevout.script_pubkey);
        found.insert(family);
        *family_counts.entry(family).or_default() += 1;
        if prevout.script_pubkey.is_p2tr() && has_annex_candidate(input) {
            annex_input_count += 1;
            found.insert("has_taproot_annex");
        }
    }

    let (state, missing_facts) = if missing_input_scripts == 0 {
        (ClassificationResultState::Complete, Vec::new())
    } else {
        (
            ClassificationResultState::Partial,
            vec!["input_script_pubkeys".to_owned()],
        )
    };
    result(
        TRANSACTION_PROPERTIES_CLASSIFIER_ID,
        state,
        None,
        ordered_labels(&found, PROPERTY_LABEL_ORDER),
        missing_facts,
        json!({
            "version": transaction.version.0,
            "input_count": transaction.input.len(),
            "output_count": transaction.output.len(),
            "known_input_script_count": known_input_scripts,
            "missing_input_script_count": missing_input_scripts,
            "script_family_counts": family_counts,
            "rbf_input_count": rbf_input_count,
            "witness_input_count": witness_input_count,
            "taproot_annex_input_count": annex_input_count,
        }),
    )
}

fn transaction_shape(
    transaction: &Transaction,
    prevouts: &PrevoutSet,
    data: &ClassificationResult,
) -> ClassificationResult {
    let input_count = transaction.input.len();
    let output_count = transaction.output.len();
    let mut found = BTreeSet::new();
    let consolidation = input_count >= SHAPE_RATIO
        && output_count > 0
        && input_count >= output_count.saturating_mul(SHAPE_RATIO);
    let batch_payout = output_count >= SHAPE_RATIO
        && input_count > 0
        && output_count >= input_count.saturating_mul(SHAPE_RATIO);
    if consolidation {
        found.insert("consolidation");
    }
    if batch_payout {
        found.insert("batch_payout");
    }

    let repeated = most_repeated_nonzero_output(transaction);
    let all_input_scripts_known = input_count == prevouts.len()
        && (0..input_count).all(|index| prevouts.get(index).is_some());
    let unique_outputs = all_unique(
        transaction
            .output
            .iter()
            .map(|output| output.script_pubkey.as_bytes()),
    );
    let unique_inputs = all_input_scripts_known
        && all_unique((0..input_count).filter_map(|index| {
            prevouts
                .get(index)
                .map(|prevout| prevout.script_pubkey.as_bytes())
        }));
    let has_data_protocol = data
        .labels
        .iter()
        .any(|label| label != "no_detected_protocol");
    let possible_coinjoin = input_count >= COINJOIN_MIN_INPUTS
        && output_count >= COINJOIN_MIN_OUTPUTS
        && repeated.is_some_and(|(_, count)| count >= COINJOIN_MIN_EQUAL_OUTPUTS)
        && unique_outputs
        && unique_inputs
        && !has_data_protocol;
    if possible_coinjoin {
        found.insert("possible_coinjoin");
    }
    if found.is_empty() {
        found.insert("other_shape");
    }
    let primary = SHAPE_LABEL_ORDER
        .into_iter()
        .find(|label| found.contains(label));

    let (state, missing_facts) = if all_input_scripts_known {
        (ClassificationResultState::Complete, Vec::new())
    } else {
        (
            ClassificationResultState::Partial,
            vec!["input_script_pubkeys".to_owned()],
        )
    };
    result(
        TRANSACTION_SHAPE_CLASSIFIER_ID,
        state,
        primary,
        ordered_labels(&found, SHAPE_LABEL_ORDER),
        missing_facts,
        json!({
            "input_count": input_count,
            "output_count": output_count,
            "shape_ratio": SHAPE_RATIO,
            "most_repeated_nonzero_output": repeated.map(|(value_sats, count)| json!({
                "value_sats": value_sats,
                "count": count,
            })),
            "output_scripts_unique": unique_outputs,
            "known_input_scripts_unique": all_input_scripts_known.then_some(unique_inputs),
            "known_data_protocol": has_data_protocol,
        }),
    )
}

fn most_repeated_nonzero_output(transaction: &Transaction) -> Option<(u64, usize)> {
    let mut counts = BTreeMap::<u64, usize>::new();
    for output in &transaction.output {
        let value_sats = output.value.to_sat();
        if value_sats > 0 {
            *counts.entry(value_sats).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|(value_sats, count)| (*count, *value_sats))
}

fn all_unique<'a>(mut values: impl Iterator<Item = &'a [u8]>) -> bool {
    let mut seen = BTreeSet::new();
    values.all(|value| seen.insert(value))
}

fn script_family(script: &Script) -> &'static str {
    if script.is_p2pk() {
        "p2pk"
    } else if script.is_multisig() {
        "bare_multisig"
    } else if script.is_p2pkh() {
        "p2pkh"
    } else if script.is_p2sh() {
        "p2sh"
    } else if script.is_p2wpkh() {
        "p2wpkh"
    } else if script.is_p2wsh() {
        "p2wsh"
    } else if script.is_p2tr() {
        "p2tr"
    } else if script.as_bytes() == [0x51, 0x02, 0x4e, 0x73] {
        "p2a"
    } else if script.is_witness_program() {
        "unknown_witness_program"
    } else if script.is_op_return() {
        "op_return"
    } else {
        "unknown_script"
    }
}

fn has_annex_candidate(input: &bitcoin::TxIn) -> bool {
    input.witness.len() >= 2
        && input
            .witness
            .last()
            .is_some_and(|element| element.first() == Some(&0x50))
}

#[derive(Default)]
struct DataAnalysis {
    labels: BTreeSet<&'static str>,
    detections: Vec<Value>,
}

impl DataAnalysis {
    fn detect(&mut self, label: &'static str, evidence: Value) {
        self.labels.insert(label);
        if self.detections.len() < MAX_DETECTION_EVIDENCE {
            self.detections.push(evidence);
        }
    }
}

fn data_protocols(transaction: &Transaction) -> ClassificationResult {
    let mut analysis = DataAnalysis::default();
    for (input_index, input) in transaction.input.iter().enumerate() {
        analyze_witness(input_index, &input.witness, &mut analysis);
    }
    for (output_index, output) in transaction.output.iter().enumerate() {
        analyze_output(output_index, &output.script_pubkey, &mut analysis);
    }
    if analysis.labels.is_empty() {
        analysis.labels.insert("no_detected_protocol");
    }
    let primary = DATA_LABEL_ORDER
        .into_iter()
        .find(|label| analysis.labels.contains(label));
    result(
        DATA_PROTOCOLS_CLASSIFIER_ID,
        ClassificationResultState::Complete,
        primary,
        ordered_labels(&analysis.labels, DATA_LABEL_ORDER),
        Vec::new(),
        json!({ "detections": analysis.detections }),
    )
}

fn analyze_witness(input: usize, witness: &bitcoin::Witness, analysis: &mut DataAnalysis) {
    let elements = witness.iter().collect::<Vec<_>>();
    let stack = if elements.len() >= 2
        && elements
            .last()
            .is_some_and(|element| element.first() == Some(&0x50))
    {
        &elements[..elements.len() - 1]
    } else {
        elements.as_slice()
    };
    let mut strong_envelope = false;
    let tapscript = (stack.len() >= 2)
        .then(|| {
            (
                stack.len() - 2,
                stack[stack.len() - 2],
                stack[stack.len() - 1],
            )
        })
        .filter(|(_, _, control)| {
            control.len() >= 33
                && (control.len() - 33) % 32 == 0
                && control.first().is_some_and(|leaf| leaf & 0xfe == 0xc0)
        });
    if let Some((element_index, element, _control)) = tapscript
        && let Some(envelope) = parse_ord_envelope(element)
    {
        strong_envelope = true;
        analysis.detect(
            "inscription",
            json!({
                "label": "inscription",
                "carrier": "witness",
                "input": input,
                "element": element_index,
                "detection": "decoded_envelope",
            }),
        );
        if envelope.is_brc20() {
            analysis.detect(
                "brc20",
                json!({
                    "label": "brc20",
                    "carrier": "witness",
                    "input": input,
                    "element": element_index,
                    "detection": "decoded_envelope",
                }),
            );
        }
    }
    if !strong_envelope
        && witness.iter().any(|element| {
            element
                .windows(ORD_ENVELOPE_BYTES.len())
                .any(|window| window == ORD_ENVELOPE_BYTES)
        })
    {
        analysis.detect(
            "inscription",
            json!({
                "label": "inscription",
                "carrier": "witness",
                "input": input,
                "detection": "byte_pattern",
                "confidence": "low",
            }),
        );
    }
}

fn analyze_output(output: usize, script: &Script, analysis: &mut DataAnalysis) {
    let bytes = script.as_bytes();
    if script.is_op_return() {
        let payload = op_return_payload(bytes);
        if bytes.starts_with(&[opcodes::all::OP_RETURN.to_u8(), 0x5d]) {
            analysis.detect(
                "runes",
                json!({ "label": "runes", "carrier": "op_return", "output": output }),
            );
        } else if contains_marker(&payload, CNTRPRTY_PREFIX) {
            analysis.detect(
                "counterparty",
                json!({
                    "label": "counterparty",
                    "carrier": "op_return",
                    "output": output,
                    "detection": "ascii_marker",
                }),
            );
        } else if contains_marker(&payload, OMNI_PREFIX) {
            analysis.detect(
                "omni",
                json!({ "label": "omni", "carrier": "op_return", "output": output }),
            );
        } else {
            analysis.detect(
                "other_op_return",
                json!({ "label": "other_op_return", "carrier": "op_return", "output": output }),
            );
        }
    }

    if let Some(keys) = bare_multisig_keys(script) {
        let data_key_count = keys
            .iter()
            .filter(|key| !matches!(key.first(), Some(0x02 | 0x03)))
            .count();
        if data_key_count > 0 {
            analysis.detect(
                "stamps",
                json!({
                    "label": "stamps",
                    "carrier": "bare_multisig",
                    "output": output,
                    "key_count": keys.len(),
                    "data_key_count": data_key_count,
                }),
            );
        }
    }
}

#[derive(Default)]
struct OrdEnvelope {
    body_window: Vec<u8>,
    brc20_marker: bool,
}

impl OrdEnvelope {
    fn push_body(&mut self, bytes: &[u8]) {
        for byte in bytes
            .iter()
            .copied()
            .filter(|byte| !byte.is_ascii_whitespace())
        {
            if self.body_window.len() == BRC20_PROTOCOL_MARKER.len() {
                self.body_window.remove(0);
            }
            self.body_window.push(byte);
            if self.body_window == BRC20_PROTOCOL_MARKER {
                self.brc20_marker = true;
            }
        }
    }

    fn is_brc20(&self) -> bool {
        self.brc20_marker
    }
}

enum DecodedInstruction<'a> {
    Push(&'a [u8]),
    Op(u8),
}

fn decoded_instructions(script: &[u8]) -> Option<Vec<DecodedInstruction<'_>>> {
    Script::from_bytes(script)
        .instructions()
        .map(|instruction| match instruction.ok()? {
            Instruction::PushBytes(push) => Some(DecodedInstruction::Push(push.as_bytes())),
            Instruction::Op(opcode) => Some(DecodedInstruction::Op(opcode.to_u8())),
        })
        .collect()
}

fn parse_ord_envelope(script: &[u8]) -> Option<OrdEnvelope> {
    let instructions = decoded_instructions(script)?;
    let start = instructions.windows(3).position(|window| {
        matches!(&window[0], DecodedInstruction::Push(bytes) if bytes.is_empty())
            && matches!(&window[1], DecodedInstruction::Op(opcode) if *opcode == opcodes::all::OP_IF.to_u8())
            && matches!(&window[2], DecodedInstruction::Push(bytes) if *bytes == ORD_PROTOCOL_ID)
    })?;
    let mut envelope = OrdEnvelope::default();
    let mut in_body = false;
    let mut index = start + 3;
    while index < instructions.len() {
        match &instructions[index] {
            DecodedInstruction::Op(opcode) if *opcode == opcodes::all::OP_ENDIF.to_u8() => {
                break;
            }
            DecodedInstruction::Push(bytes) if in_body => {
                envelope.push_body(bytes);
                index += 1;
            }
            DecodedInstruction::Push([]) => {
                in_body = true;
                index += 1;
            }
            DecodedInstruction::Push(_) => {
                let Some(DecodedInstruction::Push(_)) = instructions.get(index + 1) else {
                    break;
                };
                index += 2;
            }
            DecodedInstruction::Op(_) => break,
        }
    }
    Some(envelope)
}

fn op_return_payload(script: &[u8]) -> Vec<u8> {
    let Some(remainder) = script.get(1..) else {
        return Vec::new();
    };
    let Some(instructions) = decoded_instructions(remainder) else {
        return remainder.to_vec();
    };
    let mut payload = Vec::new();
    for instruction in instructions {
        let DecodedInstruction::Push(bytes) = instruction else {
            return remainder.to_vec();
        };
        payload.extend_from_slice(bytes);
    }
    payload
}

fn contains_marker(payload: &[u8], marker: &[u8]) -> bool {
    payload.windows(marker.len()).any(|window| window == marker)
}

fn bare_multisig_keys(script: &Script) -> Option<Vec<&[u8]>> {
    if !script.is_multisig() {
        return None;
    }
    let instructions = decoded_instructions(script.as_bytes())?;
    let keys = instructions
        .into_iter()
        .filter_map(|instruction| match instruction {
            DecodedInstruction::Push(bytes) if bytes.len() == 33 => Some(bytes),
            _ => None,
        })
        .collect::<Vec<_>>();
    (keys.len() >= 2).then_some(keys)
}

fn bip110_result(assessment: &Bip110Assessment) -> ClassificationResult {
    let primary = match assessment.status {
        Bip110Status::Compatible => "compatible",
        Bip110Status::Violating => "violating",
        Bip110Status::Indeterminate => "indeterminate",
    };
    let partial = !assessment.unknown_rules.is_empty();
    result(
        KNOTS_BIP110_CLASSIFIER_ID,
        if partial {
            ClassificationResultState::Partial
        } else {
            ClassificationResultState::Complete
        },
        Some(primary),
        vec![primary.to_owned()],
        if partial {
            vec!["policy_facts".to_owned()]
        } else {
            Vec::new()
        },
        json!({ "assessment": assessment }),
    )
}

#[cfg(test)]
mod tests {
    use bitcoin::absolute;
    use bitcoin::amount::Amount;
    use bitcoin::hashes::Hash;
    use bitcoin::{
        OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness,
        transaction::Version,
    };

    use super::*;
    use crate::model::{Bip110RuleId, classifier_catalog};

    fn input(sequence: Sequence, witness: Witness) -> TxIn {
        TxIn {
            previous_output: OutPoint {
                txid: Txid::from_byte_array([1; 32]),
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence,
            witness,
        }
    }

    fn output(value: u64, script_pubkey: ScriptBuf) -> TxOut {
        TxOut {
            value: Amount::from_sat(value),
            script_pubkey,
        }
    }

    fn transaction(inputs: Vec<TxIn>, outputs: Vec<TxOut>) -> Transaction {
        Transaction {
            version: Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: inputs,
            output: outputs,
        }
    }

    fn p2tr(tag: u8) -> ScriptBuf {
        let mut bytes = vec![0x51, 0x20];
        bytes.extend(std::iter::repeat_n(tag, 32));
        ScriptBuf::from_bytes(bytes)
    }

    fn p2wpkh(tag: u8) -> ScriptBuf {
        let mut bytes = vec![0x00, 0x14];
        bytes.extend(std::iter::repeat_n(tag, 20));
        ScriptBuf::from_bytes(bytes)
    }

    fn compatible() -> Bip110Assessment {
        Bip110Assessment {
            status: Bip110Status::Compatible,
            primary_rule: None,
            violated_rules: Vec::new(),
            unknown_rules: Vec::new(),
        }
    }

    #[test]
    fn derives_exact_transaction_structure() {
        let mut witness = Witness::new();
        witness.push([0_u8; 71]);
        witness.push([0_u8; 33]);
        let mut op_return = vec![0x6a, 0x04];
        op_return.extend_from_slice(b"data");
        let subject = transaction(
            vec![input(Sequence::MAX, witness)],
            vec![
                output(40_000, p2wpkh(1)),
                output(9_000, p2tr(2)),
                output(0, ScriptBuf::from_bytes(op_return)),
            ],
        );

        let structure = transaction_structure(&subject).expect("structure");

        assert_eq!(structure.input_count, 1);
        assert_eq!(structure.output_count, 3);
        assert_eq!(structure.op_return_bytes, 4);
        assert_eq!(structure.output_sats, 49_000);
        assert_eq!(
            structure.witness_bytes,
            u64::try_from(subject.total_size() - subject.base_size()).expect("witness bytes"),
        );
        assert!(structure.witness_bytes > 0);
    }

    #[test]
    fn structure_without_witness_or_data_outputs_is_zeroed() {
        let subject = transaction(
            vec![input(Sequence::MAX, Witness::new())],
            vec![output(1_000, p2wpkh(1))],
        );

        let structure = transaction_structure(&subject).expect("structure");

        assert_eq!(structure.op_return_bytes, 0);
        assert_eq!(structure.witness_bytes, 0);
        assert_eq!(structure.output_sats, 1_000);
    }

    #[test]
    fn structure_rejects_transactions_without_inputs_or_outputs() {
        let no_outputs = transaction(vec![input(Sequence::MAX, Witness::new())], Vec::new());
        assert!(transaction_structure(&no_outputs).is_err());
    }

    #[test]
    fn registration_order_matches_result_order() {
        let tx = transaction(
            vec![input(Sequence::ENABLE_RBF_NO_LOCKTIME, Witness::default())],
            vec![output(1_000, p2wpkh(1))],
        );
        let prevouts =
            PrevoutSet::from_vec(vec![Some(crate::bip110::PrevoutFacts::new(p2tr(2), None))]);
        let results = classify_transaction(&tx, &prevouts, &compatible());
        assert_eq!(
            results
                .iter()
                .map(|result| result.classifier_id.as_str())
                .collect::<Vec<_>>(),
            classifier_catalog()
                .iter()
                .map(|descriptor| descriptor.id.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn properties_are_exact_but_partial_when_an_input_script_is_missing() {
        let tx = transaction(
            vec![input(Sequence::ENABLE_RBF_NO_LOCKTIME, Witness::default())],
            vec![output(1_000, p2wpkh(1))],
        );
        let result = transaction_properties(&tx, &PrevoutSet::from_vec(vec![None]));
        assert_eq!(result.state, ClassificationResultState::Partial);
        assert_eq!(result.labels, ["version_2", "signals_rbf", "p2wpkh"]);
        assert_eq!(result.missing_facts, ["input_script_pubkeys"]);
    }

    #[test]
    fn taproot_annex_requires_a_known_p2tr_prevout() {
        let witness = Witness::from_slice(&[vec![1; 64], vec![0x50, 0x01]]);
        let tx = transaction(
            vec![input(Sequence::MAX, witness)],
            vec![output(1_000, p2wpkh(1))],
        );
        let p2tr_prevout =
            PrevoutSet::from_vec(vec![Some(crate::bip110::PrevoutFacts::new(p2tr(2), None))]);
        assert!(
            transaction_properties(&tx, &p2tr_prevout)
                .labels
                .contains(&"has_taproot_annex".to_owned())
        );
        assert!(
            !transaction_properties(&tx, &PrevoutSet::from_vec(vec![None]))
                .labels
                .contains(&"has_taproot_annex".to_owned())
        );
    }

    #[test]
    fn properties_distinguish_p2a_and_unknown_witness_programs() {
        let p2a = ScriptBuf::from_bytes(vec![0x51, 0x02, 0x4e, 0x73]);
        let mut witness_v2 = vec![0x52, 0x20];
        witness_v2.extend(std::iter::repeat_n(0x42, 32));
        let tx = transaction(
            vec![input(Sequence::MAX, Witness::default())],
            vec![
                output(240, p2a),
                output(1_000, ScriptBuf::from_bytes(witness_v2)),
            ],
        );
        let prevouts = PrevoutSet::from_vec(vec![Some(crate::bip110::PrevoutFacts::new(
            p2wpkh(1),
            None,
        ))]);
        let result = transaction_properties(&tx, &prevouts);
        assert!(result.labels.contains(&"p2a".to_owned()));
        assert!(
            result
                .labels
                .contains(&"unknown_witness_program".to_owned())
        );
    }

    #[test]
    fn shape_labels_are_independent_and_explainable() {
        let consolidation = transaction(
            (0..10)
                .map(|_| input(Sequence::MAX, Witness::default()))
                .collect(),
            vec![output(1_000, p2wpkh(1)), output(2_000, p2wpkh(2))],
        );
        let prevouts = PrevoutSet::from_vec(
            (0..10)
                .map(|index| {
                    Some(crate::bip110::PrevoutFacts::new(
                        p2wpkh(u8::try_from(index + 10).expect("tag")),
                        None,
                    ))
                })
                .collect(),
        );
        let data = data_protocols(&consolidation);
        let result = transaction_shape(&consolidation, &prevouts, &data);
        assert_eq!(result.labels, ["consolidation"]);
    }

    #[test]
    fn possible_coinjoin_requires_equal_values_and_no_script_reuse() {
        let tx = transaction(
            (0..5)
                .map(|_| input(Sequence::MAX, Witness::default()))
                .collect(),
            vec![
                output(10_000, p2wpkh(1)),
                output(10_000, p2wpkh(2)),
                output(10_000, p2wpkh(3)),
                output(4_000, p2wpkh(4)),
                output(3_000, p2wpkh(5)),
            ],
        );
        let prevouts = PrevoutSet::from_vec(
            (10..15)
                .map(|tag| Some(crate::bip110::PrevoutFacts::new(p2wpkh(tag), None)))
                .collect(),
        );
        let result = transaction_shape(&tx, &prevouts, &data_protocols(&tx));
        assert!(result.labels.contains(&"possible_coinjoin".to_owned()));

        let reused_prevouts = PrevoutSet::from_vec(
            (0..5)
                .map(|_| Some(crate::bip110::PrevoutFacts::new(p2wpkh(10), None)))
                .collect(),
        );
        let reused = transaction_shape(&tx, &reused_prevouts, &data_protocols(&tx));
        assert!(!reused.labels.contains(&"possible_coinjoin".to_owned()));
    }

    #[test]
    fn protocol_fingerprints_are_multi_label() {
        let content_type = b"application/json";
        let body = br#"{"p":"brc-20"}"#;
        let mut ord_bytes = vec![0x00, 0x63, 0x03, b'o', b'r', b'd', 0x01, 0x01];
        ord_bytes.push(u8::try_from(content_type.len()).expect("content type length"));
        ord_bytes.extend_from_slice(content_type);
        ord_bytes.push(0x00);
        ord_bytes.push(u8::try_from(body.len()).expect("body length"));
        ord_bytes.extend_from_slice(body);
        ord_bytes.push(0x68);
        let ord_script = ScriptBuf::from_bytes(ord_bytes);
        let mut control = vec![0xc0];
        control.extend(std::iter::repeat_n(0x02, 32));
        let witness = Witness::from_slice(&[ord_script.into_bytes(), control]);
        let runes = ScriptBuf::from_bytes(vec![0x6a, 0x5d]);
        let tx = transaction(vec![input(Sequence::MAX, witness)], vec![output(0, runes)]);
        let result = data_protocols(&tx);
        assert!(result.labels.contains(&"inscription".to_owned()));
        assert!(result.labels.contains(&"brc20".to_owned()));
        assert!(result.labels.contains(&"runes".to_owned()));
        assert!(!result.labels.contains(&"other_op_return".to_owned()));
    }

    #[test]
    fn unknown_op_return_is_not_a_claim_that_no_protocol_exists() {
        let tx = transaction(
            vec![input(Sequence::MAX, Witness::default())],
            vec![output(0, ScriptBuf::from_bytes(vec![0x6a, 0x01, 0xaa]))],
        );
        assert_eq!(data_protocols(&tx).labels, ["other_op_return"]);
    }

    #[test]
    fn registered_output_protocol_fingerprints_are_independent() {
        let mut counterparty = vec![0x6a, 0x08];
        counterparty.extend_from_slice(CNTRPRTY_PREFIX);
        let mut omni = vec![0x6a, 0x04];
        omni.extend_from_slice(OMNI_PREFIX);
        let mut stamps = vec![0x51, 0x21];
        stamps.extend(std::iter::repeat_n(0x02, 33));
        stamps.push(0x21);
        stamps.extend(std::iter::repeat_n(0x01, 33));
        stamps.extend([0x52, 0xae]);
        let tx = transaction(
            vec![input(Sequence::MAX, Witness::default())],
            vec![
                output(0, ScriptBuf::from_bytes(counterparty)),
                output(0, ScriptBuf::from_bytes(omni)),
                output(1_000, ScriptBuf::from_bytes(stamps)),
            ],
        );
        let result = data_protocols(&tx);
        assert!(result.labels.contains(&"counterparty".to_owned()));
        assert!(result.labels.contains(&"omni".to_owned()));
        assert!(result.labels.contains(&"stamps".to_owned()));
        assert!(!result.labels.contains(&"other_op_return".to_owned()));
    }

    #[test]
    fn bip110_result_preserves_proven_violation_with_missing_facts() {
        let assessment = Bip110Assessment {
            status: Bip110Status::Violating,
            primary_rule: Some(Bip110RuleId::OutputSize),
            violated_rules: vec![Bip110RuleId::OutputSize],
            unknown_rules: vec![Bip110RuleId::ElementSize],
        };
        let result = bip110_result(&assessment);
        assert_eq!(result.state, ClassificationResultState::Partial);
        assert_eq!(result.primary_label.as_deref(), Some("violating"));
        assert_eq!(result.missing_facts, ["policy_facts"]);
    }
}
