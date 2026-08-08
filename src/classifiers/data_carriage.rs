use std::collections::BTreeSet;

use bitcoin::hashes::{Hash, sha256};
use bitcoin::secp256k1::XOnlyPublicKey;
use bitcoin::{Script, Transaction, TxOut, opcodes};
use serde_json::{Value, json};

use crate::bip110::PrevoutSet;
use crate::model::{
    ClassificationResult, ClassificationResultState, DATA_CARRIAGE_SHAPE_CLASSIFIER_ID,
};

use super::{
    DecodedInstruction, MAX_DETECTION_EVIDENCE, MAX_RDTS_PUSH_BYTES, decoded_instructions,
    ord_pushnum, ordered_labels, result,
};

const MIN_PUSH_DROP_ELEMENTS: usize = 2;
const MIN_PUSH_DROP_BYTES: usize = 64;
const JXL_ENVELOPE_PUSHES: usize = 6;
const JXL_ENVELOPE_PUSH_BYTES: usize = 255;
const OP_PLENTY_MAGIC: [u8; 7] = [0x55; 7];
const OP_PLENTY_LENGTH_NIBBLES: usize = 8;
const OP_PLENTY_ENCODING_ALPHABET: [u8; 28] = [
    0x51, 0x58, 0x59, 0x5a, 0x5b, 0x5c, 0x5d, 0x5e, 0x5f, 0x60, 0x61, 0x77, 0x78, 0x87, 0x8f, 0x90,
    0x91, 0x92, 0x93, 0x9a, 0x9b, 0x9c, 0x9e, 0x9f, 0xa0, 0xa1, 0xa2, 0xa4,
];
const WITNESS_PROGRAM_BYTES: usize = 32;
const OLGA_LENGTH_PREFIX_BYTES: usize = 2;
const OLGA_MIN_OUTPUTS: usize = 2;
const LABEL_ORDER: [&str; 6] = [
    "push_drop_witness",
    "opcode_value_coding",
    "p2wsh_envelope",
    "output_key_carrier",
    "off_curve_p2tr",
    "no_detected_carriage_shape",
];

#[derive(Default)]
struct Analysis {
    labels: BTreeSet<&'static str>,
    detections: Vec<Value>,
}

impl Analysis {
    fn detect(&mut self, label: &'static str, evidence: Value) {
        self.labels.insert(label);
        if self.detections.len() < MAX_DETECTION_EVIDENCE {
            self.detections.push(evidence);
        }
    }
}

pub(super) fn data_carriage_shape(
    transaction: &Transaction,
    prevouts: &PrevoutSet,
) -> ClassificationResult {
    let mut analysis = Analysis::default();
    analyze_output_carriage(transaction, &mut analysis);
    let mut missing_input_scripts = 0_usize;
    for (input_index, input) in transaction.input.iter().enumerate() {
        let Some(prevout) = prevouts.get(input_index) else {
            missing_input_scripts += 1;
            continue;
        };
        let elements = input.witness.iter().collect::<Vec<_>>();
        let revealed = if prevout.script_pubkey.is_p2tr() {
            let stack = if elements.len() >= 2
                && elements
                    .last()
                    .is_some_and(|element| element.first() == Some(&0x50))
            {
                &elements[..elements.len() - 1]
            } else {
                elements.as_slice()
            };
            (stack.len() >= 2)
                .then(|| {
                    (
                        stack.len() - 2,
                        stack[stack.len() - 2],
                        stack[stack.len() - 1],
                        "tapscript",
                    )
                })
                .filter(|(_, _, control, _)| {
                    control.len() >= 33
                        && (control.len() - 33) % 32 == 0
                        && control.first().is_some_and(|leaf| leaf & 0xfe == 0xc0)
                })
        } else if prevout.script_pubkey.is_p2wsh() {
            elements
                .last()
                .filter(|script| p2wsh_script_matches(&prevout.script_pubkey, script))
                .map(|script| (elements.len() - 1, *script, &[][..], "p2wsh"))
        } else {
            None
        };
        let Some((element_index, script, _control, carrier)) = revealed else {
            continue;
        };
        let Some(instructions) = decoded_instructions(script) else {
            continue;
        };
        if let Some(detection) = push_drop_carriage(&instructions) {
            analysis.detect(
                "push_drop_witness",
                json!({
                    "label": "push_drop_witness",
                    "carrier": carrier,
                    "input": input_index,
                    "element": element_index,
                    "instruction": detection.instruction,
                    "pushed_elements": detection.pushed_elements,
                    "pushed_bytes": detection.pushed_bytes,
                }),
            );
        }
        if let Some(detection) = op_plenty_carriage(&instructions) {
            analysis.detect(
                "opcode_value_coding",
                json!({
                    "label": "opcode_value_coding",
                    "carrier": carrier,
                    "input": input_index,
                    "element": element_index,
                    "instruction": detection.instruction,
                    "framing": "op_plenty_v2",
                    "payload_bytes": detection.payload_bytes,
                }),
            );
        }
        if carrier == "p2wsh"
            && let Some(detection) = p2wsh_envelope(&instructions)
        {
            analysis.detect(
                "p2wsh_envelope",
                json!({
                    "label": "p2wsh_envelope",
                    "carrier": carrier,
                    "input": input_index,
                    "element": element_index,
                    "instruction": 0,
                    "framing": "jxl_n_hide",
                    "pushed_elements": detection.pushed_elements,
                    "pushed_bytes": detection.pushed_bytes,
                }),
            );
        }
    }

    let complete = missing_input_scripts == 0;
    if analysis.labels.is_empty() && complete {
        analysis.labels.insert("no_detected_carriage_shape");
    }
    let primary = LABEL_ORDER
        .into_iter()
        .find(|label| analysis.labels.contains(label));
    result(
        DATA_CARRIAGE_SHAPE_CLASSIFIER_ID,
        if complete {
            ClassificationResultState::Complete
        } else {
            ClassificationResultState::Partial
        },
        primary,
        ordered_labels(&analysis.labels, LABEL_ORDER),
        if complete {
            Vec::new()
        } else {
            vec!["input_script_pubkeys".to_owned()]
        },
        json!({
            "missing_input_script_count": missing_input_scripts,
            "detections": analysis.detections,
        }),
    )
}

fn analyze_output_carriage(transaction: &Transaction, analysis: &mut Analysis) {
    let mut output_index = 0_usize;
    while output_index < transaction.output.len() {
        let Some(first_output) = transaction.output.get(output_index) else {
            break;
        };
        if p2wsh_program(&first_output.script_pubkey).is_none() {
            output_index += 1;
            continue;
        }
        let value_sats = first_output.value.to_sat();
        let mut run_end = output_index + 1;
        while transaction
            .output
            .get(run_end)
            .is_some_and(|output| output_matches_olga_run(output, value_sats))
        {
            run_end += 1;
        }
        let outputs = &transaction.output[output_index..run_end];
        if let Some(detection) = olga_output_carriage(outputs) {
            analysis.detect(
                "output_key_carrier",
                json!({
                    "label": "output_key_carrier",
                    "carrier": "olga_p2wsh",
                    "first_output": output_index,
                    "output_count": detection.output_count,
                    "payload_bytes": detection.payload_bytes,
                    "value_sats": detection.value_sats,
                }),
            );
        }
        output_index = run_end;
    }

    for (index, output) in transaction.output.iter().enumerate() {
        let Some(key) = p2tr_output_key(&output.script_pubkey) else {
            continue;
        };
        if XOnlyPublicKey::from_slice(&key).is_err() {
            analysis.detect(
                "off_curve_p2tr",
                json!({
                    "label": "off_curve_p2tr",
                    "carrier": "p2tr_output_key",
                    "output": index,
                    "reason": "invalid_xonly_public_key",
                }),
            );
        }
    }
}

struct OlgaOutputDetection {
    output_count: usize,
    payload_bytes: usize,
    value_sats: u64,
}

fn olga_output_carriage(outputs: &[TxOut]) -> Option<OlgaOutputDetection> {
    let first = outputs.first()?;
    let first_program = p2wsh_program(&first.script_pubkey)?;
    let payload_bytes = usize::from(u16::from_be_bytes([
        *first_program.first()?,
        *first_program.get(1)?,
    ]));
    let framed_bytes = payload_bytes.checked_add(OLGA_LENGTH_PREFIX_BYTES)?;
    let output_count = framed_bytes.div_ceil(WITNESS_PROGRAM_BYTES);
    if output_count < OLGA_MIN_OUTPUTS {
        return None;
    }
    if outputs.len() != output_count {
        return None;
    }
    let value_sats = first.value.to_sat();
    let final_program = p2wsh_program(&outputs.last()?.script_pubkey)?;
    let used_final_bytes = framed_bytes - (output_count - 1) * WITNESS_PROGRAM_BYTES;
    if final_program
        .get(used_final_bytes..)?
        .iter()
        .any(|byte| *byte != 0)
    {
        return None;
    }
    Some(OlgaOutputDetection {
        output_count,
        payload_bytes,
        value_sats,
    })
}

fn output_matches_olga_run(output: &TxOut, value_sats: u64) -> bool {
    output.value.to_sat() == value_sats && p2wsh_program(&output.script_pubkey).is_some()
}

fn p2wsh_program(script: &Script) -> Option<&[u8]> {
    let bytes = script.as_bytes();
    (bytes.len() == WITNESS_PROGRAM_BYTES + 2
        && bytes[0] == 0
        && usize::from(bytes[1]) == WITNESS_PROGRAM_BYTES)
        .then(|| &bytes[2..])
}

fn p2tr_output_key(script: &Script) -> Option<[u8; WITNESS_PROGRAM_BYTES]> {
    let bytes = script.as_bytes();
    (bytes.len() == WITNESS_PROGRAM_BYTES + 2
        && bytes[0] == opcodes::all::OP_PUSHNUM_1.to_u8()
        && usize::from(bytes[1]) == WITNESS_PROGRAM_BYTES)
        .then(|| bytes[2..].try_into().ok())
        .flatten()
}

fn p2wsh_script_matches(script_pubkey: &Script, witness_script: &[u8]) -> bool {
    let program = script_pubkey.as_bytes();
    program.len() == 34
        && program[0] == 0
        && program[1] == 32
        && sha256::Hash::hash(witness_script).as_byte_array() == &program[2..]
}

struct PushDropDetection {
    instruction: usize,
    pushed_elements: usize,
    pushed_bytes: usize,
}

fn push_drop_carriage(instructions: &[DecodedInstruction<'_>]) -> Option<PushDropDetection> {
    for start in 0..instructions.len() {
        let mut depth = 0_usize;
        let mut pushed_elements = 0_usize;
        let mut pushed_bytes = 0_usize;
        let mut saw_drop = false;
        for instruction in &instructions[start..] {
            match instruction {
                DecodedInstruction::Push(bytes) if bytes.len() <= MAX_RDTS_PUSH_BYTES => {
                    depth += 1;
                    pushed_elements += 1;
                    pushed_bytes += bytes.len();
                }
                DecodedInstruction::Op(opcode) if ord_pushnum(*opcode).is_some() => {
                    depth += 1;
                    pushed_elements += 1;
                    pushed_bytes += 1;
                }
                DecodedInstruction::Op(opcode) if *opcode == opcodes::all::OP_DROP.to_u8() => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                    saw_drop = true;
                }
                DecodedInstruction::Op(opcode) if *opcode == opcodes::all::OP_2DROP.to_u8() => {
                    if depth < 2 {
                        break;
                    }
                    depth -= 2;
                    saw_drop = true;
                }
                _ => break,
            }
            if depth == 0 && saw_drop {
                if pushed_elements >= MIN_PUSH_DROP_ELEMENTS && pushed_bytes >= MIN_PUSH_DROP_BYTES
                {
                    return Some(PushDropDetection {
                        instruction: start,
                        pushed_elements,
                        pushed_bytes,
                    });
                }
                break;
            }
        }
    }
    None
}

struct P2wshEnvelopeDetection {
    pushed_elements: usize,
    pushed_bytes: usize,
}

fn p2wsh_envelope(instructions: &[DecodedInstruction<'_>]) -> Option<P2wshEnvelopeDetection> {
    let expected_instructions = JXL_ENVELOPE_PUSHES + 4;
    if instructions.len() != expected_instructions
        || !matches!(
            instructions.first(),
            Some(DecodedInstruction::Op(opcode)) if *opcode == opcodes::all::OP_PUSHNUM_1.to_u8()
        )
        || !matches!(
            instructions.get(1),
            Some(DecodedInstruction::Op(opcode)) if *opcode == opcodes::all::OP_NOTIF.to_u8()
        )
        || !instructions[2..2 + JXL_ENVELOPE_PUSHES].iter().all(
            |instruction| matches!(instruction, DecodedInstruction::Push(bytes) if bytes.len() == JXL_ENVELOPE_PUSH_BYTES),
        )
        || !matches!(
            instructions.get(expected_instructions - 2),
            Some(DecodedInstruction::Op(opcode)) if *opcode == opcodes::all::OP_ENDIF.to_u8()
        )
        || !matches!(
            instructions.last(),
            Some(DecodedInstruction::Op(opcode)) if *opcode == opcodes::all::OP_PUSHNUM_1.to_u8()
        )
    {
        return None;
    }
    Some(P2wshEnvelopeDetection {
        pushed_elements: JXL_ENVELOPE_PUSHES,
        pushed_bytes: JXL_ENVELOPE_PUSHES * JXL_ENVELOPE_PUSH_BYTES,
    })
}

struct OpPlentyDetection {
    instruction: usize,
    payload_bytes: usize,
}

fn op_plenty_carriage(instructions: &[DecodedInstruction<'_>]) -> Option<OpPlentyDetection> {
    let magic_len = OP_PLENTY_MAGIC.len();
    for start in 0..instructions.len().saturating_sub(magic_len - 1) {
        if !instructions[start..start + magic_len]
            .iter()
            .zip(OP_PLENTY_MAGIC)
            .all(|(instruction, expected)| {
                matches!(instruction, DecodedInstruction::Op(opcode) if *opcode == expected)
            })
        {
            continue;
        }
        let header_start = start + magic_len;
        let header_end = header_start + OP_PLENTY_LENGTH_NIBBLES;
        let Some(header) = instructions.get(header_start..header_end) else {
            continue;
        };
        let mut encoded_payload_nibbles = 0_usize;
        let mut valid_header = true;
        for instruction in header {
            let DecodedInstruction::Op(opcode) = instruction else {
                valid_header = false;
                break;
            };
            if !OP_PLENTY_ENCODING_ALPHABET.contains(opcode) {
                valid_header = false;
                break;
            }
            let Some(next) = encoded_payload_nibbles
                .checked_mul(16)
                .and_then(|value| value.checked_add(usize::from(*opcode % 22)))
            else {
                valid_header = false;
                break;
            };
            encoded_payload_nibbles = next;
        }
        if !valid_header || !encoded_payload_nibbles.is_multiple_of(2) {
            continue;
        }
        let Some(payload_end) = header_end.checked_add(encoded_payload_nibbles) else {
            continue;
        };
        let Some(payload) = instructions.get(header_end..payload_end) else {
            continue;
        };
        if !payload.iter().all(|instruction| {
            matches!(instruction, DecodedInstruction::Op(opcode) if OP_PLENTY_ENCODING_ALPHABET.contains(opcode))
        }) {
            continue;
        }
        let Some(footer) = instructions.get(payload_end..payload_end + 3) else {
            continue;
        };
        let footer = footer
            .iter()
            .map(|instruction| match instruction {
                DecodedInstruction::Op(opcode) => Some(*opcode),
                DecodedInstruction::Push(_) => None,
            })
            .collect::<Option<Vec<_>>>();
        let Some(footer) = footer else {
            continue;
        };
        if !matches!(footer.as_slice(), [0x6d, 0x6d, 0x61 | 0x6d | 0x75]) {
            continue;
        }
        return Some(OpPlentyDetection {
            instruction: start,
            payload_bytes: encoded_payload_nibbles / 2,
        });
    }
    None
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

    use crate::bip110::PrevoutFacts;

    use super::*;

    const VALID_XONLY_KEY: [u8; 32] = [
        0xb3, 0x3c, 0xc9, 0xed, 0xc0, 0x96, 0xd0, 0xa8, 0x34, 0x16, 0x96, 0x4b, 0xd3, 0xc6, 0x24,
        0x7b, 0x8f, 0xec, 0xd2, 0x56, 0xe4, 0xef, 0xa7, 0x87, 0x0d, 0x2c, 0x85, 0x4b, 0xde, 0xb3,
        0x33, 0x90,
    ];

    fn p2tr() -> ScriptBuf {
        p2tr_with_key(VALID_XONLY_KEY)
    }

    fn p2tr_with_key(key: [u8; 32]) -> ScriptBuf {
        let mut bytes = vec![0x51, 0x20];
        bytes.extend(key);
        ScriptBuf::from_bytes(bytes)
    }

    fn p2wsh(witness_script: &[u8]) -> ScriptBuf {
        let mut bytes = vec![0x00, 0x20];
        bytes.extend(sha256::Hash::hash(witness_script).to_byte_array());
        ScriptBuf::from_bytes(bytes)
    }

    fn transaction(witness: Witness) -> Transaction {
        Transaction {
            version: Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: Txid::from_byte_array([1; 32]),
                    vout: 0,
                },
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness,
            }],
            output: vec![TxOut {
                value: Amount::from_sat(1_000),
                script_pubkey: p2tr(),
            }],
        }
    }

    fn p2wsh_output(value_sats: u64, program: [u8; 32]) -> TxOut {
        let mut script = vec![0x00, 0x20];
        script.extend(program);
        TxOut {
            value: Amount::from_sat(value_sats),
            script_pubkey: ScriptBuf::from_bytes(script),
        }
    }

    fn olga_outputs(payload: &[u8], value_sats: u64) -> Vec<TxOut> {
        let payload_len = u16::try_from(payload.len()).expect("bounded test payload");
        let mut framed = payload_len.to_be_bytes().to_vec();
        framed.extend(payload);
        framed.resize(framed.len().div_ceil(32) * 32, 0);
        framed
            .chunks_exact(32)
            .map(|chunk| p2wsh_output(value_sats, chunk.try_into().expect("exact P2WSH program")))
            .collect()
    }

    fn transaction_with_outputs(outputs: Vec<TxOut>) -> Transaction {
        let mut transaction = transaction(Witness::new());
        transaction.output = outputs;
        transaction
    }

    fn tapscript_witness(script: Vec<u8>) -> Witness {
        let mut control = vec![0xc0];
        control.extend([3; 32]);
        Witness::from_slice(&[script, control])
    }

    fn jxl_p2wsh_envelope(pushes: usize, push_bytes: usize) -> Vec<u8> {
        let push_bytes = u8::try_from(push_bytes).expect("PUSHDATA1 test length");
        let mut script = vec![
            opcodes::all::OP_PUSHNUM_1.to_u8(),
            opcodes::all::OP_NOTIF.to_u8(),
        ];
        for value in 0..pushes {
            script.extend([opcodes::all::OP_PUSHDATA1.to_u8(), push_bytes]);
            script.extend(vec![
                u8::try_from(value).expect("test byte");
                usize::from(push_bytes)
            ]);
        }
        script.extend([
            opcodes::all::OP_ENDIF.to_u8(),
            opcodes::all::OP_PUSHNUM_1.to_u8(),
        ]);
        script
    }

    #[test]
    fn recognizes_large_balanced_push_drop_runs() {
        let mut script = vec![0x4c, 80];
        script.extend([7; 80]);
        script.extend([3, b'o', b'r', b'd', 0x6d]);
        let transaction = transaction(tapscript_witness(script));
        let result = data_carriage_shape(
            &transaction,
            &PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(p2tr(), None))]),
        );

        assert_eq!(result.labels, ["push_drop_witness"]);
        assert_eq!(result.state, ClassificationResultState::Complete);
    }

    #[test]
    fn recognizes_length_delimited_op_plenty_v2() {
        let script = [
            0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x9a, 0x58, 0x9a, 0x58, 0x9a, 0x58, 0x9a,
            0x5c, 0x9e, 0x60, 0xa0, 0x61, 0x6d, 0x6d, 0x75,
        ];
        let transaction = transaction(tapscript_witness(script.to_vec()));
        let result = data_carriage_shape(
            &transaction,
            &PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(p2tr(), None))]),
        );

        assert_eq!(result.labels, ["opcode_value_coding"]);
        assert_eq!(
            result.evidence.as_ref().unwrap()["detections"][0]["payload_bytes"],
            2
        );
    }

    #[test]
    fn rejects_malformed_op_plenty_v2_framing() {
        let valid = [
            0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x55, 0x9a, 0x58, 0x9a, 0x58, 0x9a, 0x58, 0x9a,
            0x5c, 0x9e, 0x60, 0xa0, 0x61, 0x6d, 0x6d, 0x75,
        ];
        let mut invalid_header = valid;
        invalid_header[7] = opcodes::all::OP_DROP.to_u8();
        let truncated = &valid[..valid.len() - 1];

        for script in [invalid_header.as_slice(), truncated] {
            let transaction = transaction(tapscript_witness(script.to_vec()));
            let result = data_carriage_shape(
                &transaction,
                &PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(p2tr(), None))]),
            );
            assert_eq!(result.labels, ["no_detected_carriage_shape"]);
        }
    }

    #[test]
    fn validates_p2wsh_commitment_before_scanning() {
        let mut witness_script = vec![0x4c, 80];
        witness_script.extend([8; 80]);
        witness_script.extend([3, b'o', b'r', b'd', 0x6d]);
        let transaction = transaction(Witness::from_slice(&[witness_script.clone()]));
        let matching = data_carriage_shape(
            &transaction,
            &PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(p2wsh(&witness_script), None))]),
        );
        let mismatched = data_carriage_shape(
            &transaction,
            &PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(p2wsh(b"different"), None))]),
        );

        assert_eq!(matching.labels, ["push_drop_witness"]);
        assert_eq!(mismatched.labels, ["no_detected_carriage_shape"]);
    }

    #[test]
    fn recognizes_an_exact_olga_p2wsh_output_run() {
        let transaction = transaction_with_outputs(olga_outputs(&[7; 70], 546));
        let result = data_carriage_shape(
            &transaction,
            &PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(p2tr(), None))]),
        );

        assert_eq!(result.labels, ["output_key_carrier"]);
        assert_eq!(
            result.evidence.as_ref().unwrap()["detections"][0],
            json!({
                "label": "output_key_carrier",
                "carrier": "olga_p2wsh",
                "first_output": 0,
                "output_count": 3,
                "payload_bytes": 70,
                "value_sats": 546,
            })
        );
    }

    #[test]
    fn rejects_ambiguous_or_malformed_olga_output_runs() {
        let valid = olga_outputs(&[8; 70], 546);
        let mut mismatched_value = valid.clone();
        mismatched_value[1].value = Amount::from_sat(547);
        let mut nonzero_padding = valid.clone();
        let mut final_script = nonzero_padding[2].script_pubkey.clone().into_bytes();
        *final_script.last_mut().expect("padding byte") = 1;
        nonzero_padding[2].script_pubkey = ScriptBuf::from_bytes(final_script);
        let mut extra_same_run = valid.clone();
        extra_same_run.push(p2wsh_output(546, [9; 32]));

        for outputs in [mismatched_value, nonzero_padding, extra_same_run] {
            let result = data_carriage_shape(
                &transaction_with_outputs(outputs),
                &PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(p2tr(), None))]),
            );
            assert_eq!(result.labels, ["no_detected_carriage_shape"]);
        }
    }

    #[test]
    fn distinguishes_off_curve_and_valid_p2tr_output_keys() {
        let invalid = TxOut {
            value: Amount::from_sat(1_000),
            script_pubkey: p2tr_with_key([0xff; 32]),
        };
        let valid = TxOut {
            value: Amount::from_sat(2_000),
            script_pubkey: p2tr(),
        };
        let result = data_carriage_shape(
            &transaction_with_outputs(vec![invalid, valid]),
            &PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(p2tr(), None))]),
        );

        assert_eq!(result.labels, ["off_curve_p2tr"]);
        assert_eq!(
            result.evidence.as_ref().unwrap()["detections"][0],
            json!({
                "label": "off_curve_p2tr",
                "carrier": "p2tr_output_key",
                "output": 0,
                "reason": "invalid_xonly_public_key",
            })
        );
    }

    #[test]
    fn recognizes_the_exact_committed_p2wsh_envelope() {
        let script = jxl_p2wsh_envelope(JXL_ENVELOPE_PUSHES, JXL_ENVELOPE_PUSH_BYTES);
        let transaction = transaction(Witness::from_slice(std::slice::from_ref(&script)));
        let result = data_carriage_shape(
            &transaction,
            &PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(p2wsh(&script), None))]),
        );

        assert_eq!(result.labels, ["p2wsh_envelope"]);
        assert_eq!(
            result.evidence.as_ref().unwrap()["detections"][0],
            json!({
                "label": "p2wsh_envelope",
                "carrier": "p2wsh",
                "input": 0,
                "element": 0,
                "instruction": 0,
                "framing": "jxl_n_hide",
                "pushed_elements": 6,
                "pushed_bytes": 1530,
            })
        );
    }

    #[test]
    fn rejects_envelope_lookalikes_outside_the_exact_committed_p2wsh_grammar() {
        let exact = jxl_p2wsh_envelope(JXL_ENVELOPE_PUSHES, JXL_ENVELOPE_PUSH_BYTES);
        let wrong_count = jxl_p2wsh_envelope(JXL_ENVELOPE_PUSHES - 1, JXL_ENVELOPE_PUSH_BYTES);
        let short_pushes = jxl_p2wsh_envelope(JXL_ENVELOPE_PUSHES, JXL_ENVELOPE_PUSH_BYTES - 1);
        let cases = [
            (
                transaction(tapscript_witness(exact.clone())),
                PrevoutFacts::new(p2tr(), None),
            ),
            (
                transaction(Witness::from_slice(std::slice::from_ref(&wrong_count))),
                PrevoutFacts::new(p2wsh(&wrong_count), None),
            ),
            (
                transaction(Witness::from_slice(std::slice::from_ref(&short_pushes))),
                PrevoutFacts::new(p2wsh(&short_pushes), None),
            ),
            (
                transaction(Witness::from_slice(&[exact])),
                PrevoutFacts::new(p2wsh(b"different witness script"), None),
            ),
        ];

        for (transaction, prevout) in cases {
            let result =
                data_carriage_shape(&transaction, &PrevoutSet::from_vec(vec![Some(prevout)]));
            assert_eq!(result.labels, ["no_detected_carriage_shape"]);
        }
    }

    #[test]
    fn withholds_a_negative_label_when_input_scripts_are_missing() {
        let transaction = transaction(Witness::new());
        let result = data_carriage_shape(&transaction, &PrevoutSet::from_vec(vec![None]));

        assert_eq!(result.state, ClassificationResultState::Partial);
        assert!(result.labels.is_empty());
        assert_eq!(result.missing_facts, ["input_script_pubkeys"]);
    }

    #[test]
    fn preserves_a_positive_label_when_another_input_script_is_missing() {
        let mut script = vec![0x4c, 80];
        script.extend([7; 80]);
        script.extend([3, b'o', b'r', b'd', 0x6d]);
        let mut transaction = transaction(tapscript_witness(script));
        let mut missing_input = transaction.input[0].clone();
        missing_input.previous_output.vout = 1;
        missing_input.witness = Witness::new();
        transaction.input.push(missing_input);
        let result = data_carriage_shape(
            &transaction,
            &PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(p2tr(), None)), None]),
        );

        assert_eq!(result.state, ClassificationResultState::Partial);
        assert_eq!(result.labels, ["push_drop_witness"]);
        assert_eq!(result.missing_facts, ["input_script_pubkeys"]);
    }

    #[test]
    fn preserves_an_output_only_label_when_input_scripts_are_missing() {
        let transaction = transaction_with_outputs(olga_outputs(&[6; 70], 546));
        let result = data_carriage_shape(&transaction, &PrevoutSet::from_vec(vec![None]));

        assert_eq!(result.state, ClassificationResultState::Partial);
        assert_eq!(result.labels, ["output_key_carrier"]);
        assert_eq!(result.missing_facts, ["input_script_pubkeys"]);
    }
}
