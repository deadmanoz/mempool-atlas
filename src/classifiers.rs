//! Pure, independently specified transaction classifier lenses.
//!
//! The behavioral contract lives in `docs/classification.md`. This module is
//! intentionally implemented from that contract and does not incorporate a
//! third-party explorer's classifier source.

mod arc4;
mod data_carriage;

use std::collections::{BTreeMap, BTreeSet};

use crate::bip110::PrevoutSet;
use bitcoin::hashes::Hash;
use bitcoin::script::Instruction;
use bitcoin::{Script, Transaction, opcodes};
use serde_json::{Value, json};

use crate::model::{
    Bip110Assessment, Bip110Status, ClassificationResult, ClassificationResultState,
    DATA_PROTOCOLS_CLASSIFIER_ID, KNOTS_BIP110_CLASSIFIER_ID, ModelError,
    TRANSACTION_PROPERTIES_CLASSIFIER_ID, TRANSACTION_SHAPE_CLASSIFIER_ID, TransactionStructure,
};
use data_carriage::analyze_data_carriage;

const ORD_ENVELOPE_BYTES: [u8; 6] = [0x00, 0x63, 0x03, 0x6f, 0x72, 0x64];
const ORD_PROTOCOL_ID: &[u8] = b"ord";
const BRC20_PROTOCOL_MARKER: &[u8] = br#""p":"brc-20""#;
const CNTRPRTY_PREFIX: &[u8] = b"CNTRPRTY";
const OMNI_PREFIX: &[u8] = b"omni";
/// Bitcoin Stamps writes its marker into a Counterparty issuance description.
/// Both letter cases and the rare plural form occur in deployed transactions.
const STAMP_MARKERS: [&[u8]; 4] = [b"stamp:", b"STAMP:", b"stamps:", b"STAMPS:"];
/// The unspendable third key of a classic Stamps 1-of-3 bare-multisig output.
const STAMPS_BURN_KEYS: [[u8; 33]; 5] = [
    burn_key(0x02, 0x22, 0x22),
    burn_key(0x03, 0x33, 0x33),
    burn_key(0x02, 0x02, 0x02),
    burn_key(0x03, 0x03, 0x02),
    burn_key(0x03, 0x03, 0x03),
];
/// Data bytes carried by one compressed-key slot: the 33-byte key without its
/// leading sign-prefix byte and its trailing byte.
const KEY_PAYLOAD_BYTES: std::ops::Range<usize> = 1..32;
/// Bounded evidence: how many carrier output indexes one detection retains.
const MAX_CARRIER_EVIDENCE: usize = 8;
const COINJOIN_MIN_INPUTS: usize = 5;
const COINJOIN_MIN_OUTPUTS: usize = 5;
const COINJOIN_MIN_EQUAL_OUTPUTS: usize = 3;
const SHAPE_RATIO: usize = 5;
const MAX_DETECTION_EVIDENCE: usize = 16;
const MAX_RDTS_PUSH_BYTES: usize = 256;

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
pub(crate) struct TransactionClassifierOutput {
    pub(crate) results: Vec<ClassificationResult>,
    pub(crate) recognized_non_op_return_bytes: u64,
}

pub(crate) fn classify_transaction_with_metrics(
    transaction: &Transaction,
    prevouts: &PrevoutSet,
    bip110: &Bip110Assessment,
) -> TransactionClassifierOutput {
    let data = data_protocols(transaction);
    let carriage = analyze_data_carriage(transaction, prevouts);
    TransactionClassifierOutput {
        results: vec![
            transaction_properties(transaction, prevouts),
            transaction_shape(transaction, prevouts, &data),
            data,
            carriage.result,
            bip110_result(bip110),
        ],
        recognized_non_op_return_bytes: carriage.recognized_non_op_return_bytes,
    }
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
    // `other_shape` is a negative claim, so it may only be made once every
    // registered heuristic is terminal. The count-based rules always are;
    // `possible_coinjoin` needs every spent-output script. While those are
    // missing and nothing else matched, the lens reports no label at all and
    // names the missing fact class instead of fabricating a negative.
    if found.is_empty() && all_input_scripts_known {
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
    let key = arc4_key(transaction);
    for (input_index, input) in transaction.input.iter().enumerate() {
        analyze_witness(input_index, &input.witness, &mut analysis);
    }
    for (output_index, output) in transaction.output.iter().enumerate() {
        analyze_op_return(
            output_index,
            &output.script_pubkey,
            key.as_ref(),
            &mut analysis,
        );
    }
    analyze_multisig_carriers(transaction, key.as_ref(), &mut analysis);
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
        && let Some(detection) = parse_ord_envelope(element)
    {
        strong_envelope = true;
        let framing = match detection.encoding {
            OrdEnvelopeEncoding::ClassicIf => "classic_if",
            OrdEnvelopeEncoding::PushDrop => "push_drop",
        };
        analysis.detect(
            "inscription",
            json!({
                "label": "inscription",
                "carrier": "witness",
                "input": input,
                "element": element_index,
                "detection": "decoded_envelope",
                "framing": framing,
            }),
        );
        if detection.envelope.is_brc20() {
            analysis.detect(
                "brc20",
                json!({
                    "label": "brc20",
                    "carrier": "witness",
                    "input": input,
                    "element": element_index,
                    "detection": "decoded_envelope",
                    "framing": framing,
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

/// Counterparty and Bitcoin Stamps seed ARC4 with the first input's txid in
/// the byte order of its RPC display form, which is the reverse of the
/// consensus-serialized byte array. Getting this backwards is the classic
/// implementation bug: the keystream still runs, it just never decodes.
fn arc4_key(transaction: &Transaction) -> Option<[u8; 32]> {
    let mut key = transaction
        .input
        .first()?
        .previous_output
        .txid
        .to_byte_array();
    key.reverse();
    Some(key)
}

const fn burn_key(prefix: u8, fill: u8, last: u8) -> [u8; 33] {
    let mut key = [fill; 33];
    key[0] = prefix;
    key[32] = last;
    key
}

fn is_stamps_burn_key(key: &[u8]) -> bool {
    STAMPS_BURN_KEYS.iter().any(|burn| burn == key)
}

/// Both protocols fill a full 33-byte compressed-key slot and read the payload
/// back from between the sign-prefix byte and the trailing byte.
fn compressed_key_payload(key: &[u8]) -> Option<&[u8]> {
    if key.len() != 33 {
        return None;
    }
    key.get(KEY_PAYLOAD_BYTES)
}

fn stamp_marker(data: &[u8]) -> Option<(&'static [u8], usize)> {
    STAMP_MARKERS.into_iter().find_map(|marker| {
        data.windows(marker.len())
            .position(|window| window == marker)
            .map(|offset| (marker, offset))
    })
}

/// Counterparty writes `CNTRPRTY` either at offset 0 or immediately after a
/// one-byte chunk-length prefix, depending on the carrier.
fn cntrprty_offset(data: &[u8], allow_length_prefix: bool) -> Option<usize> {
    if data.starts_with(CNTRPRTY_PREFIX) {
        return Some(0);
    }
    if allow_length_prefix
        && data.len() > CNTRPRTY_PREFIX.len()
        && &data[1..=CNTRPRTY_PREFIX.len()] == CNTRPRTY_PREFIX
    {
        return Some(1);
    }
    None
}

fn analyze_op_return(
    output: usize,
    script: &Script,
    key: Option<&[u8; 32]>,
    analysis: &mut DataAnalysis,
) {
    if !script.is_op_return() {
        return;
    }
    let bytes = script.as_bytes();
    let payload = op_return_payload(bytes);
    if bytes.starts_with(&[opcodes::all::OP_RETURN.to_u8(), 0x5d]) {
        analysis.detect(
            "runes",
            json!({ "label": "runes", "carrier": "op_return", "output": output }),
        );
        return;
    }
    // Counterparty's OP_RETURN transport always obfuscates `CNTRPRTY` plus the
    // message with ARC4, so a plaintext marker is not evidence of the protocol
    // and would only invite incidental substring matches.
    if payload.len() >= CNTRPRTY_PREFIX.len()
        && let Some(key) = key
        && cntrprty_offset(&arc4::apply(key, &payload), false).is_some()
    {
        analysis.detect(
            "counterparty",
            json!({
                "label": "counterparty",
                "carrier": "op_return",
                "output": output,
                "detection": "arc4_decrypted_prefix",
                "payload_bytes": payload.len(),
            }),
        );
        return;
    }
    // Omni's Class C transport places its four-byte marker at the start of the
    // OP_RETURN payload, not anywhere inside it.
    if payload.starts_with(OMNI_PREFIX) {
        analysis.detect(
            "omni",
            json!({
                "label": "omni",
                "carrier": "op_return",
                "output": output,
                "detection": "payload_prefix",
            }),
        );
        return;
    }
    analysis.detect(
        "other_op_return",
        json!({ "label": "other_op_return", "carrier": "op_return", "output": output }),
    );
}

struct MultisigCarrier<'a> {
    output: usize,
    required: u8,
    keys: Vec<&'a [u8]>,
}

impl MultisigCarrier<'_> {
    /// Classic Stamps: 1-of-3 where the third key is a known burn key and the
    /// first two carry payload bytes.
    fn stamps_chunk(&self) -> Option<Vec<u8>> {
        if self.required != 1 || self.keys.len() != 3 || !is_stamps_burn_key(self.keys[2]) {
            return None;
        }
        self.compressed_pair_chunk()
    }

    /// Counterparty packs 31 payload bytes into each of the first two keys of
    /// a three-key output, and a length-prefixed run into the second key of a
    /// two-key output.
    fn counterparty_chunk(&self) -> Option<Vec<u8>> {
        match self.keys.len() {
            3 => self.compressed_pair_chunk(),
            2 => {
                let key = self.keys[1];
                let length = usize::from(*key.first()?);
                key.get(1..=length).map(<[u8]>::to_vec)
            }
            _ => None,
        }
    }

    fn compressed_pair_chunk(&self) -> Option<Vec<u8>> {
        let first = compressed_key_payload(self.keys.first()?)?;
        let second = compressed_key_payload(self.keys.get(1)?)?;
        let mut chunk = Vec::with_capacity(first.len() + second.len());
        chunk.extend_from_slice(first);
        chunk.extend_from_slice(second);
        Some(chunk)
    }
}

fn multisig_carriers(transaction: &Transaction) -> Vec<MultisigCarrier<'_>> {
    transaction
        .output
        .iter()
        .enumerate()
        .filter_map(|(output, txout)| {
            let (required, keys) = parse_bare_multisig(&txout.script_pubkey)?;
            Some(MultisigCarrier {
                output,
                required,
                keys,
            })
        })
        .collect()
}

fn carrier_evidence(outputs: &[usize]) -> Value {
    json!({
        "output_count": outputs.len(),
        "outputs": outputs.iter().take(MAX_CARRIER_EVIDENCE).collect::<Vec<_>>(),
    })
}

fn analyze_multisig_carriers(
    transaction: &Transaction,
    key: Option<&[u8; 32]>,
    analysis: &mut DataAnalysis,
) {
    let carriers = multisig_carriers(transaction);
    if carriers.is_empty() {
        return;
    }
    // Stamps is the more specific fingerprint, so it is evaluated first, but
    // the two are not mutually exclusive. The reference data-carrier
    // implementation assigns one protocol per transaction and therefore drops
    // Counterparty once Stamps matches. These are independent Atlas lenses, so
    // a Stamps payload carried inside a proven Counterparty envelope reports
    // both, and neither label is withheld because the other fired.
    detect_stamps(&carriers, key, analysis);
    detect_multisig_counterparty(&carriers, key, analysis);
}

fn detect_stamps(
    carriers: &[MultisigCarrier<'_>],
    key: Option<&[u8; 32]>,
    analysis: &mut DataAnalysis,
) {
    let mut outputs = Vec::new();
    let mut data = Vec::new();
    for carrier in carriers {
        if let Some(chunk) = carrier.stamps_chunk() {
            outputs.push(carrier.output);
            data.extend_from_slice(&chunk);
        }
    }
    if outputs.is_empty() {
        return;
    }
    // Burn keys alone are a weak signal; require the deobfuscated payload to
    // carry a description that opens with the Stamps marker before claiming
    // the label.
    let Some(key) = key else {
        return;
    };
    let decrypted = arc4::apply(key, &data);
    let Some((marker, offset)) = stamp_marker(&decrypted) else {
        return;
    };
    let transport = if contains_marker(&decrypted, CNTRPRTY_PREFIX) {
        "counterparty"
    } else {
        "pure"
    };
    let mut evidence = carrier_evidence(&outputs);
    evidence["label"] = json!("stamps");
    evidence["carrier"] = json!("bare_multisig");
    evidence["detection"] = json!("arc4_decrypted_marker");
    evidence["transport"] = json!(transport);
    evidence["marker"] = json!(String::from_utf8_lossy(marker));
    evidence["marker_offset"] = json!(offset);
    evidence["payload_bytes"] = json!(data.len());
    analysis.detect("stamps", evidence);
}

fn detect_multisig_counterparty(
    carriers: &[MultisigCarrier<'_>],
    key: Option<&[u8; 32]>,
    analysis: &mut DataAnalysis,
) {
    let mut outputs = Vec::new();
    let mut chunks = Vec::new();
    for carrier in carriers {
        if let Some(chunk) = carrier.counterparty_chunk() {
            outputs.push(carrier.output);
            chunks.push(chunk);
        }
    }
    if chunks.is_empty() {
        return;
    }
    // Payloads longer than one output are concatenated in output order, so the
    // joined stream is tried before each output on its own.
    let mut candidates = Vec::new();
    if chunks.len() > 1 {
        candidates.push((outputs.clone(), chunks.concat()));
    }
    candidates.extend(
        outputs
            .iter()
            .copied()
            .zip(chunks)
            .map(|(output, chunk)| (vec![output], chunk)),
    );

    for (candidate_outputs, data) in candidates {
        if data.len() < CNTRPRTY_PREFIX.len() {
            continue;
        }
        // The 2014-era two-key transport wrote the prefix in the clear; every
        // later transport obfuscates it.
        let (detection, decoded) = if cntrprty_offset(&data, false).is_some() {
            ("plaintext_prefix", data)
        } else if let Some(key) = key {
            let decrypted = arc4::apply(key, &data);
            if cntrprty_offset(&decrypted, true).is_none() {
                continue;
            }
            ("arc4_decrypted_prefix", decrypted)
        } else {
            continue;
        };
        let mut evidence = carrier_evidence(&candidate_outputs);
        evidence["label"] = json!("counterparty");
        evidence["carrier"] = json!("bare_multisig");
        evidence["detection"] = json!(detection);
        evidence["payload_bytes"] = json!(decoded.len());
        analysis.detect("counterparty", evidence);
        // Stamps that predate the burn-key convention ride this envelope with
        // no burn key to key off, so the marker in the decoded stream is the
        // only evidence available for them.
        if let Some((marker, offset)) = stamp_marker(&decoded) {
            let mut evidence = carrier_evidence(&candidate_outputs);
            evidence["label"] = json!("stamps");
            evidence["carrier"] = json!("bare_multisig");
            evidence["detection"] = json!("counterparty_envelope_marker");
            evidence["transport"] = json!("counterparty");
            evidence["marker"] = json!(String::from_utf8_lossy(marker));
            evidence["marker_offset"] = json!(offset);
            analysis.detect("stamps", evidence);
        }
        return;
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

#[derive(Clone, Copy)]
enum OrdEnvelopeEncoding {
    ClassicIf,
    PushDrop,
}

struct OrdEnvelopeDetection {
    envelope: OrdEnvelope,
    encoding: OrdEnvelopeEncoding,
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

fn parse_ord_envelope(script: &[u8]) -> Option<OrdEnvelopeDetection> {
    let instructions = decoded_instructions(script)?;
    parse_classic_ord_envelope(&instructions)
        .or_else(|| parse_push_drop_ord_envelope(&instructions))
}

fn parse_classic_ord_envelope(
    instructions: &[DecodedInstruction<'_>],
) -> Option<OrdEnvelopeDetection> {
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
    Some(OrdEnvelopeDetection {
        envelope,
        encoding: OrdEnvelopeEncoding::ClassicIf,
    })
}

fn parse_push_drop_ord_envelope(
    instructions: &[DecodedInstruction<'_>],
) -> Option<OrdEnvelopeDetection> {
    for marker in 0..instructions.len() {
        if !matches!(instructions[marker], DecodedInstruction::Push(bytes) if bytes == ORD_PROTOCOL_ID)
        {
            continue;
        }
        if marker >= 2
            && matches!(instructions[marker - 2], DecodedInstruction::Push(bytes) if bytes.is_empty())
            && matches!(instructions[marker - 1], DecodedInstruction::Op(opcode) if opcode == opcodes::all::OP_IF.to_u8())
        {
            continue;
        }
        let mut payload = Vec::<Vec<u8>>::new();
        let mut depth = 1_usize;
        for instruction in &instructions[marker + 1..] {
            match instruction {
                DecodedInstruction::Push(bytes) if bytes.len() <= MAX_RDTS_PUSH_BYTES => {
                    payload.push(bytes.to_vec());
                    depth += 1;
                }
                DecodedInstruction::Op(opcode) if *opcode == opcodes::all::OP_2DROP.to_u8() => {
                    if depth < 2 {
                        break;
                    }
                    depth -= 2;
                }
                DecodedInstruction::Op(opcode) if *opcode == opcodes::all::OP_DROP.to_u8() => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                DecodedInstruction::Op(opcode) => {
                    let Some(value) = ord_pushnum(*opcode) else {
                        break;
                    };
                    payload.push(vec![value]);
                    depth += 1;
                }
                DecodedInstruction::Push(_) => break,
            }
            if depth == 0 {
                let mut envelope = OrdEnvelope::default();
                if let Some(body_start) = payload.iter().position(Vec::is_empty) {
                    for body in &payload[body_start + 1..] {
                        envelope.push_body(body);
                    }
                }
                return Some(OrdEnvelopeDetection {
                    envelope,
                    encoding: OrdEnvelopeEncoding::PushDrop,
                });
            }
        }
    }
    None
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

/// Decodes `OP_m <key>.. OP_n OP_CHECKMULTISIG` into its signature threshold
/// and its key slots. Key bytes are returned exactly as pushed, because a
/// data carrier's slots are not required to be valid public keys.
fn parse_bare_multisig(script: &Script) -> Option<(u8, Vec<&[u8]>)> {
    let instructions = decoded_instructions(script.as_bytes())?;
    let (first, rest) = instructions.split_first()?;
    let DecodedInstruction::Op(required_opcode) = first else {
        return None;
    };
    let required = pushnum(*required_opcode)?;
    let (last, head) = rest.split_last()?;
    if !matches!(last, DecodedInstruction::Op(opcode)
        if *opcode == opcodes::all::OP_CHECKMULTISIG.to_u8())
    {
        return None;
    }
    let (tail, keys) = head.split_last()?;
    let DecodedInstruction::Op(total_opcode) = tail else {
        return None;
    };
    let total = pushnum(*total_opcode)?;
    let keys = keys
        .iter()
        .map(|instruction| match instruction {
            DecodedInstruction::Push(bytes) => Some(*bytes),
            DecodedInstruction::Op(_) => None,
        })
        .collect::<Option<Vec<_>>>()?;
    (usize::from(total) == keys.len() && required >= 1 && required <= total)
        .then_some((required, keys))
}

fn pushnum(opcode: u8) -> Option<u8> {
    (0x51..=0x60).contains(&opcode).then(|| opcode - 0x50)
}

fn ord_pushnum(opcode: u8) -> Option<u8> {
    if opcode == 0x4f {
        Some(0x81)
    } else {
        pushnum(opcode)
    }
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
    use crate::model::{Bip110RuleId, DATA_CARRIAGE_SHAPE_CLASSIFIER_ID, classifier_catalog};

    fn input(sequence: Sequence, witness: Witness) -> TxIn {
        spend(Txid::from_byte_array([1; 32]), sequence, witness)
    }

    fn spend(txid: Txid, sequence: Sequence, witness: Witness) -> TxIn {
        TxIn {
            previous_output: OutPoint { txid, vout: 0 },
            script_sig: ScriptBuf::new(),
            sequence,
            witness,
        }
    }

    fn decode_hex(hex: &str) -> Vec<u8> {
        assert!(hex.len().is_multiple_of(2), "odd-length hex");
        (0..hex.len() / 2)
            .map(|index| u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).expect("hex byte"))
            .collect()
    }

    fn script(hex: &str) -> ScriptBuf {
        ScriptBuf::from_bytes(decode_hex(hex))
    }

    fn parse_txid(hex: &str) -> Txid {
        hex.parse().expect("txid")
    }

    fn op_return(payload: &[u8]) -> ScriptBuf {
        let mut bytes = vec![opcodes::all::OP_RETURN.to_u8()];
        bytes.push(u8::try_from(payload.len()).expect("payload length"));
        bytes.extend_from_slice(payload);
        ScriptBuf::from_bytes(bytes)
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
        let results = classify_transaction_with_metrics(&tx, &prevouts, &compatible()).results;
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
    fn data_carriage_catalog_declares_v5_labels() {
        let catalog = classifier_catalog();
        let protocol_descriptor = catalog
            .iter()
            .find(|descriptor| descriptor.id == DATA_PROTOCOLS_CLASSIFIER_ID)
            .expect("data protocols classifier");
        assert_eq!(protocol_descriptor.version, "3");

        let descriptor = catalog
            .iter()
            .find(|descriptor| descriptor.id == DATA_CARRIAGE_SHAPE_CLASSIFIER_ID)
            .expect("data carriage classifier");

        assert_eq!(descriptor.version, "5");
        assert_eq!(
            descriptor
                .labels
                .iter()
                .map(|label| label.key.as_str())
                .collect::<Vec<_>>(),
            [
                "push_drop_witness",
                "opcode_value_coding",
                "p2wsh_envelope",
                "witness_argument_carrier",
                "output_key_carrier",
                "off_curve_p2tr",
                "embedded_file_magic",
                "no_detected_carriage_shape",
            ]
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
    fn other_shape_is_withheld_while_a_shape_rule_is_unresolved() {
        // A CoinJoin candidate: five inputs, five outputs, three equal
        // non-zero amounts, no repeated output script. Only the spent-output
        // scripts are missing.
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
        let data = data_protocols(&tx);

        let unresolved = transaction_shape(
            &tx,
            &PrevoutSet::from_vec((0..5).map(|_| None).collect()),
            &data,
        );
        assert_eq!(unresolved.state, ClassificationResultState::Partial);
        assert!(unresolved.labels.is_empty());
        assert_eq!(unresolved.primary_label, None);
        assert_eq!(unresolved.missing_facts, ["input_script_pubkeys"]);

        let resolved = transaction_shape(
            &tx,
            &PrevoutSet::from_vec(
                (10..15)
                    .map(|tag| Some(crate::bip110::PrevoutFacts::new(p2wpkh(tag), None)))
                    .collect(),
            ),
            &data,
        );
        assert_eq!(resolved.state, ClassificationResultState::Complete);
        assert_eq!(resolved.labels, ["possible_coinjoin"]);
    }

    #[test]
    fn other_shape_is_a_terminal_negative_observation() {
        let tx = transaction(
            vec![input(Sequence::MAX, Witness::default())],
            vec![output(1_000, p2wpkh(1)), output(2_000, p2wpkh(2))],
        );
        let data = data_protocols(&tx);

        let partial = transaction_shape(&tx, &PrevoutSet::from_vec(vec![None]), &data);
        assert_eq!(partial.state, ClassificationResultState::Partial);
        assert!(partial.labels.is_empty());

        let complete = transaction_shape(
            &tx,
            &PrevoutSet::from_vec(vec![Some(crate::bip110::PrevoutFacts::new(
                p2wpkh(9),
                None,
            ))]),
            &data,
        );
        assert_eq!(complete.state, ClassificationResultState::Complete);
        assert_eq!(complete.labels, ["other_shape"]);
        assert_eq!(complete.primary_label.as_deref(), Some("other_shape"));
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

    fn push_data(script: &mut Vec<u8>, data: &[u8]) {
        match data.len() {
            0 => script.push(0),
            1..=75 => script.push(u8::try_from(data.len()).expect("direct push length")),
            76..=255 => {
                script.extend([0x4c, u8::try_from(data.len()).expect("pushdata1 length")]);
            }
            _ => {
                let length = u16::try_from(data.len()).expect("pushdata2 length");
                script.push(0x4d);
                script.extend(length.to_le_bytes());
            }
        }
        script.extend_from_slice(data);
    }

    fn push_drop_ord(payload: &[&[u8]]) -> Vec<u8> {
        let mut script = Vec::new();
        push_data(&mut script, b"ord");
        for element in payload {
            push_data(&mut script, element);
        }
        let mut depth = payload.len() + 1;
        while depth >= 2 {
            script.push(opcodes::all::OP_2DROP.to_u8());
            depth -= 2;
        }
        if depth == 1 {
            script.push(opcodes::all::OP_DROP.to_u8());
        }
        script
    }

    #[test]
    fn rdts_push_drop_ord_envelope_preserves_protocol_labels() {
        let body = br#"{ "p" : "brc-20" }"#;
        let ord_script = push_drop_ord(&[&[1], b"application/json", &[], body]);
        let mut control = vec![0xc0];
        control.extend([2; 32]);
        let witness = Witness::from_slice(&[ord_script, control]);
        let tx = transaction(
            vec![input(Sequence::MAX, witness)],
            vec![output(1_000, p2tr(2))],
        );

        let result = data_protocols(&tx);

        assert_eq!(result.labels, ["inscription", "brc20"]);
        assert_eq!(
            result.evidence.as_ref().unwrap()["detections"][0]["framing"],
            "push_drop"
        );
    }

    #[test]
    fn rdts_push_drop_ord_enforces_the_256_byte_boundary() {
        let accepted = push_drop_ord(&[&[], &[1; 256]]);
        let rejected = push_drop_ord(&[&[], &[1; 257]]);

        assert!(parse_ord_envelope(&accepted).is_some());
        assert!(parse_ord_envelope(&rejected).is_none());
    }

    #[test]
    fn unknown_op_return_is_not_a_claim_that_no_protocol_exists() {
        let tx = transaction(
            vec![input(Sequence::MAX, Witness::default())],
            vec![output(0, ScriptBuf::from_bytes(vec![0x6a, 0x01, 0xaa]))],
        );
        assert_eq!(data_protocols(&tx).labels, ["other_op_return"]);
    }

    // Mainnet carrier fixtures. Each records the first input spent by the
    // transaction, which seeds the ARC4 keystream, and the exact carrier
    // output scripts.
    const STAMPS_FIRST_INPUT: &str =
        "9d4e8bac0b68f2f1dbee1b8b7187971731de6184b193a6c722aa6103818258fd";
    const STAMPS_CARRIER: &str = "5121026a43aa44d05e6318b171b76f2058ddf4fc8a3826099690689d466\
6d0905f11f8210309ddfdbb7584f3221c94561d38fbd136dc8b4ba6b25e6e8d4055a1527b186ede21020202020202020\
20202020202020202020202020202020202020202020202020253ae";
    const STAMPS_OVER_COUNTERPARTY_FIRST_INPUT: &str =
        "668bce85fe6911a712a57ef0e4351e556f70cb07d0404b31b158ed86ce5f10e8";
    const STAMPS_OVER_COUNTERPARTY_CARRIERS: [&str; 3] = [
        "512103cf8acee2752053160b486a951f40f3b3d4f17e4e7c3686a7260c8aea144b572821\
02de07b9e6cfd2adf6fb1b1290c30d80359b5b6b80cdc9839100267be804940822210303\
0303030303030303030303030303030303030303030303030303030303030253ae",
        "512103cf8acee2752053160b6a847433071e6c62691c7d175fcae46765eeac785242bb21\
03c623ff95e9e1b3d7f73733a2c92abd0581682fa0d8eda3ad202458d333963616210303\
0303030303030303030303030303030303030303030303030303030303030253ae",
        "512103de8acee2752053160b1189760e21007557573d070f7fc5ed556ddddb7d774ad221\
03fe038488ebdcaac5f3063499ba5ece4cc2225bf9808acae24965319e67d741ee210303\
0303030303030303030303030303030303030303030303030303030303030253ae",
    ];
    const COUNTERPARTY_FIRST_INPUT: &str =
        "de3dec665a89228593ffa3c0236dd098a6f9ef6ac698db016f8a3303ce728649";
    const COUNTERPARTY_CARRIER_ONE: &str = "5121026e8c99cf905947268385eb5eb08ecd489965fa5e722761\
d0d7c3bd8ac8ee392021020b446132ea04f474e116b900546155e2ff731cfde2b6e984525d18e522feb512210241e401\
603ff07343f84d3dcad01ddbd01385506cf85209d8b150fe6c93f6ee1f53ae";
    const COUNTERPARTY_CARRIER_TWO: &str = "512102498c99cf9059472683a3cb17f0dded08ba23382ad27224\
9e9486ee8ac8ee394821030b446132ea04f474c945f856114100b0df2053a8ae96afd61d1038b66bb09503210241e401\
603ff07343f84d3dcad01ddbd01385506cf85209d8b150fe6c93f6ee1f53ae";
    const LEGACY_COUNTERPARTY_FIRST_INPUT: &str =
        "d44de058ad30854b587c9cb2d75f94e44003614e4a0152af768ed106a480bd4f";
    const LEGACY_COUNTERPARTY_CARRIER: &str = "512102dd842167b625c60c10eda494eadd54df7b30f372d71\
7b946b1912f0ce59dddf6211c434e5452505254590000000000000000000000010000000001312d000000000052ae";

    #[test]
    fn stamps_requires_a_burn_key_carrier_and_a_decrypted_marker() {
        let carrier = script(STAMPS_CARRIER);
        let tx = transaction(
            vec![spend(
                parse_txid(STAMPS_FIRST_INPUT),
                Sequence::MAX,
                Witness::default(),
            )],
            vec![output(796, carrier.clone())],
        );
        let result = data_protocols(&tx);
        assert!(result.labels.contains(&"stamps".to_owned()));

        // The keystream is seeded by the exact first input. A different
        // spend over identical carrier bytes proves nothing.
        let other = transaction(
            vec![input(Sequence::MAX, Witness::default())],
            vec![output(796, carrier)],
        );
        assert!(!data_protocols(&other).labels.contains(&"stamps".to_owned()));
    }

    #[test]
    fn stamps_is_not_claimed_for_bare_multisig_data_shaped_keys() {
        // Two 33-byte slots, one of which is not a well-formed compressed
        // key. Version 1 read that as a Stamps fingerprint; it is not one.
        let mut carrier = vec![0x51, 0x21];
        carrier.extend(std::iter::repeat_n(0x02, 33));
        carrier.push(0x21);
        carrier.extend(std::iter::repeat_n(0x01, 33));
        carrier.extend([0x52, 0xae]);
        let tx = transaction(
            vec![input(Sequence::MAX, Witness::default())],
            vec![output(1_000, ScriptBuf::from_bytes(carrier))],
        );
        assert!(!data_protocols(&tx).labels.contains(&"stamps".to_owned()));
    }

    #[test]
    fn stamps_over_counterparty_reports_both_independent_fingerprints() {
        let tx = transaction(
            vec![spend(
                parse_txid(STAMPS_OVER_COUNTERPARTY_FIRST_INPUT),
                Sequence::MAX,
                Witness::default(),
            )],
            STAMPS_OVER_COUNTERPARTY_CARRIERS
                .into_iter()
                .map(|carrier| output(7_000, script(carrier)))
                .collect(),
        );
        // The deobfuscated stream carries a CNTRPRTY envelope and a Stamps
        // marker. Both are proven, so both are reported.
        assert_eq!(data_protocols(&tx).labels, ["stamps", "counterparty"]);
    }

    #[test]
    fn counterparty_multisig_carriers_are_decrypted_before_matching() {
        let tx = transaction(
            vec![spend(
                parse_txid(COUNTERPARTY_FIRST_INPUT),
                Sequence::MAX,
                Witness::default(),
            )],
            vec![
                output(7_800, script(COUNTERPARTY_CARRIER_ONE)),
                output(7_800, script(COUNTERPARTY_CARRIER_TWO)),
            ],
        );
        let result = data_protocols(&tx);
        assert!(result.labels.contains(&"counterparty".to_owned()));
        assert!(!result.labels.contains(&"stamps".to_owned()));
    }

    #[test]
    fn counterparty_retains_the_plaintext_two_key_transport() {
        let tx = transaction(
            vec![spend(
                parse_txid(LEGACY_COUNTERPARTY_FIRST_INPUT),
                Sequence::MAX,
                Witness::default(),
            )],
            vec![output(10_860, script(LEGACY_COUNTERPARTY_CARRIER))],
        );
        assert!(
            data_protocols(&tx)
                .labels
                .contains(&"counterparty".to_owned())
        );
    }

    #[test]
    fn counterparty_op_return_requires_the_decrypted_prefix() {
        let first_input = parse_txid(COUNTERPARTY_FIRST_INPUT);
        let mut key = first_input.to_byte_array();
        key.reverse();
        let mut message = CNTRPRTY_PREFIX.to_vec();
        message.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03]);
        let obfuscated = arc4::apply(&key, &message);
        let tx = transaction(
            vec![spend(first_input, Sequence::MAX, Witness::default())],
            vec![output(0, op_return(&obfuscated))],
        );
        let result = data_protocols(&tx);
        assert_eq!(result.labels, ["counterparty"]);

        // A plaintext marker is not the on-chain encoding, so it must not
        // fire the fingerprint.
        let plaintext = transaction(
            vec![spend(first_input, Sequence::MAX, Witness::default())],
            vec![output(0, op_return(&message))],
        );
        assert_eq!(data_protocols(&plaintext).labels, ["other_op_return"]);
    }

    #[test]
    fn omni_requires_its_marker_at_the_payload_start() {
        let mut payload = OMNI_PREFIX.to_vec();
        payload.extend_from_slice(&[0x00, 0x00, 0x00, 0x1f]);
        let tx = transaction(
            vec![input(Sequence::MAX, Witness::default())],
            vec![output(0, op_return(&payload))],
        );
        assert_eq!(data_protocols(&tx).labels, ["omni"]);

        let mut embedded = vec![0xaa, 0xbb];
        embedded.extend_from_slice(OMNI_PREFIX);
        let elsewhere = transaction(
            vec![input(Sequence::MAX, Witness::default())],
            vec![output(0, op_return(&embedded))],
        );
        assert_eq!(data_protocols(&elsewhere).labels, ["other_op_return"]);
    }

    #[test]
    fn runes_still_takes_precedence_over_other_op_return_carriers() {
        let tx = transaction(
            vec![input(Sequence::MAX, Witness::default())],
            vec![
                output(0, ScriptBuf::from_bytes(vec![0x6a, 0x5d])),
                output(0, op_return(&[0xaa])),
            ],
        );
        assert_eq!(data_protocols(&tx).labels, ["runes", "other_op_return"]);
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
