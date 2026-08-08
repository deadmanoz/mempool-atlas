use std::collections::BTreeSet;

use bitcoin::hashes::{Hash, sha256};
use bitcoin::{Script, Transaction, opcodes};
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
const OP_PLENTY_MAGIC: [u8; 7] = [0x55; 7];
const OP_PLENTY_LENGTH_NIBBLES: usize = 8;
const OP_PLENTY_ENCODING_ALPHABET: [u8; 28] = [
    0x51, 0x58, 0x59, 0x5a, 0x5b, 0x5c, 0x5d, 0x5e, 0x5f, 0x60, 0x61, 0x77, 0x78, 0x87, 0x8f, 0x90,
    0x91, 0x92, 0x93, 0x9a, 0x9b, 0x9c, 0x9e, 0x9f, 0xa0, 0xa1, 0xa2, 0xa4,
];
const LABEL_ORDER: [&str; 3] = [
    "push_drop_witness",
    "opcode_value_coding",
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

    fn p2tr() -> ScriptBuf {
        let mut bytes = vec![0x51, 0x20];
        bytes.extend([2; 32]);
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

    fn tapscript_witness(script: Vec<u8>) -> Witness {
        let mut control = vec![0xc0];
        control.extend([3; 32]);
        Witness::from_slice(&[script, control])
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
}
