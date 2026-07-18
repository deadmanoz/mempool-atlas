//! BIP-110 (Reduced Data Temporary Softfork) conformance of one parsed
//! transaction.
//!
//! BIP-110 adds seven temporary consensus rules. This pack checks the rules
//! that are intrinsic to the transaction's own bytes:
//!
//! 1. output scriptPubKeys larger than 34 bytes are invalid unless the first
//!    opcode is `OP_RETURN`, in which case up to 83 bytes are valid;
//! 2. `OP_PUSHDATA*` payloads and script argument witness items larger than
//!    256 bytes are invalid, except the redeemScript push in BIP16
//!    scriptSigs (scripts are not data: witness scripts, tapleaf scripts,
//!    control blocks, and annexes are not script argument witness items);
//! 4. witness stacks with a Taproot annex are invalid;
//! 5. Taproot control blocks larger than 257 bytes are invalid;
//! 6. tapscripts including any `OP_SUCCESS*` opcode anywhere are invalid;
//! 7. tapscripts executing `OP_IF`/`OP_NOTIF` are invalid.
//!
//! Two parts of BIP-110 are deliberately outside this pack's vocabulary.
//! Rule 3 (spending undefined witness or tapleaf versions) needs the prevout
//! scriptPubKey, which mempool evidence does not carry, and the UTXO
//! grandfathering exemption needs the spent outputs' creation heights.
//! Verdicts therefore describe the transaction's intrinsic conformance with
//! the checkable rules, never a full consensus evaluation. Rule 7 is also an
//! over-approximation: the BIP invalidates tapscripts *executing*
//! `OP_IF`/`OP_NOTIF`, but from raw bytes this pack detects *presence*,
//! recorded as `presence_not_execution` in evidence.
//!
//! # Honesty model
//!
//! Without prevout scripts, witness interpretation is inferred from
//! structure, and every check is either definitive or ambiguous:
//!
//! - Rule 1 is always definitive: the outputs are in the transaction.
//! - A scriptSig push over 256 bytes is a definitive rule-2 violation unless
//!   it is the final instruction of its scriptSig, where it might be an
//!   exempt BIP16 redeemScript and is only ambiguous.
//! - A witness whose elements are all within 256 bytes cannot violate rule 2
//!   under any interpretation: a definitive pass.
//! - A witness that ends (after stripping one trailing `0x50`-led annex
//!   candidate when at least two elements are present) with a
//!   control-block-shaped element is inferred to be a Taproot script-path
//!   spend, recorded per input as `"basis": "taproot_script_path"`. On that
//!   basis rules 5, 6, and 7 apply definitively to the control block and the
//!   inferred tapscript, the tapscript itself is exempt from rule 2 while
//!   its internal pushes are checked, and the remaining elements are script
//!   arguments whose oversize is a rule-2 violation.
//! - An annex is only claimed when the witness is plausibly a Taproot spend:
//!   the annex marker plus either a 64/65-byte key-path signature shape or a
//!   valid script-path shape. A `0x50`-led last element in a witness that
//!   cannot be Taproot-shaped is ambiguous, not a violation.
//! - Any other oversized witness element stays ambiguous: it could be an
//!   exempt witness script, or part of an undefined-witness-version stack
//!   that rule 2 does not cover at all.
//!
//! A definitive violation yields its verdict with status `Complete`; the
//! verdict for several violations is the first violated rule in spec order
//! (1, 2, 4, 5, 6, 7), with the complete list in evidence. No definitive
//! violation but at least one ambiguity yields `indeterminate` with status
//! `Partial`. A fully definitive pass yields `conforming`. Evidence always
//! carries the per-rule outcomes, the violation list, the ambiguity list,
//! and the inference basis for every input where one was used.

use atlas_model::{TaxonomyDescriptor, VerdictDescriptor};
use bitcoin::script::Instruction;
use bitcoin::{Script, Transaction, Witness, opcodes};
use serde_json::{Value, json};

use crate::{
    ClassificationInput, ClassificationResult, ClassificationStatus, Classifier,
    ClassifierManifest, witness,
};

const CLASSIFIER_ID: &str = "bip110-conformance";
const CLASSIFIER_VERSION: &str = "0.1.0";

/// Rule 1: the largest conforming non-`OP_RETURN` output scriptPubKey.
const MAX_SCRIPT_PUBKEY_LEN: usize = 34;
/// Rule 1: the largest conforming `OP_RETURN` output scriptPubKey.
const MAX_OP_RETURN_SCRIPT_PUBKEY_LEN: usize = 83;
/// Rule 2: the largest conforming data push or script argument witness item.
const MAX_DATA_PUSH_LEN: usize = 256;
/// Rule 5: the largest conforming Taproot control block (128 script leaves).
const MAX_CONTROL_BLOCK_LEN: usize = 257;
/// BIP-341 Schnorr signature lengths for a key-path spend.
const KEY_PATH_SIGNATURE_LENS: [usize; 2] = [64, 65];

/// The BIP-110 rules this pack can check from raw transaction bytes, in spec
/// order. Rule 3 is absent: it requires the prevout scriptPubKey.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Rule {
    /// Rule 1.
    OversizedScriptPubkey,
    /// Rule 2.
    OversizedDataPush,
    /// Rule 4.
    AnnexPresent,
    /// Rule 5.
    OversizedControlBlock,
    /// Rule 6.
    OpSuccessInTapscript,
    /// Rule 7.
    ConditionalInTapscript,
}

impl Rule {
    const ALL: [Self; 6] = [
        Self::OversizedScriptPubkey,
        Self::OversizedDataPush,
        Self::AnnexPresent,
        Self::OversizedControlBlock,
        Self::OpSuccessInTapscript,
        Self::ConditionalInTapscript,
    ];

    const fn number(self) -> u8 {
        match self {
            Self::OversizedScriptPubkey => 1,
            Self::OversizedDataPush => 2,
            Self::AnnexPresent => 4,
            Self::OversizedControlBlock => 5,
            Self::OpSuccessInTapscript => 6,
            Self::ConditionalInTapscript => 7,
        }
    }

    const fn verdict_key(self) -> &'static str {
        match self {
            Self::OversizedScriptPubkey => "oversized_script_pubkey",
            Self::OversizedDataPush => "oversized_data_push",
            Self::AnnexPresent => "annex_present",
            Self::OversizedControlBlock => "oversized_control_block",
            Self::OpSuccessInTapscript => "op_success_in_tapscript",
            Self::ConditionalInTapscript => "conditional_in_tapscript",
        }
    }

    const fn verdict_label(self) -> &'static str {
        match self {
            Self::OversizedScriptPubkey => "Oversized scriptPubKey",
            Self::OversizedDataPush => "Oversized data push",
            Self::AnnexPresent => "Annex present",
            Self::OversizedControlBlock => "Oversized control block",
            Self::OpSuccessInTapscript => "OP_SUCCESS in tapscript",
            Self::ConditionalInTapscript => "Conditional in tapscript",
        }
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|rule| *rule == self)
            .expect("every rule is in ALL")
    }
}

/// The BIP-110 conformance pack, owner of the `bip110` taxonomy. Without a
/// parsed transaction it reports [`ClassificationStatus::Unknown`]; a
/// definitive result completes, and an ambiguity-only result is the honest
/// `indeterminate` with status [`ClassificationStatus::Partial`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Bip110Conformance;

impl Classifier for Bip110Conformance {
    fn manifest(&self) -> ClassifierManifest {
        ClassifierManifest {
            id: CLASSIFIER_ID.to_owned(),
            version: CLASSIFIER_VERSION.to_owned(),
            required_facts: vec!["raw_transaction".to_owned()],
        }
    }

    fn taxonomy(&self) -> TaxonomyDescriptor {
        let mut verdicts = vec![VerdictDescriptor {
            key: "conforming".to_owned(),
            label: "Conforming".to_owned(),
        }];
        verdicts.extend(Rule::ALL.into_iter().map(|rule| VerdictDescriptor {
            key: rule.verdict_key().to_owned(),
            label: rule.verdict_label().to_owned(),
        }));
        verdicts.push(VerdictDescriptor {
            key: "indeterminate".to_owned(),
            label: "Indeterminate".to_owned(),
        });
        verdicts.push(VerdictDescriptor {
            key: "unknown".to_owned(),
            label: "Unknown".to_owned(),
        });
        TaxonomyDescriptor {
            key: "bip110".to_owned(),
            label: "BIP-110".to_owned(),
            verdicts,
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
        let analysis = Analysis::of(transaction);
        let evidence = analysis.evidence();
        match analysis.first_violated_rule() {
            Some(rule) => result(
                ClassificationStatus::Complete,
                Some(rule.verdict_key()),
                evidence,
            ),
            None if analysis.has_ambiguity() => result(
                ClassificationStatus::Partial,
                Some("indeterminate"),
                evidence,
            ),
            None => result(ClassificationStatus::Complete, Some("conforming"), evidence),
        }
    }
}

fn result(
    status: ClassificationStatus,
    verdict: Option<&str>,
    evidence: Value,
) -> ClassificationResult {
    ClassificationResult {
        classifier_id: CLASSIFIER_ID.to_owned(),
        classifier_version: CLASSIFIER_VERSION.to_owned(),
        status,
        verdict: verdict.map(str::to_owned),
        evidence,
    }
}

/// Accumulated definitive violations, ambiguities, and structural inferences
/// for one transaction.
#[derive(Default)]
struct Analysis {
    violations: Vec<Value>,
    ambiguities: Vec<Value>,
    inferences: Vec<Value>,
    violated: [bool; Rule::ALL.len()],
    ambiguous: [bool; Rule::ALL.len()],
}

impl Analysis {
    fn of(transaction: &Transaction) -> Self {
        let mut analysis = Self::default();
        for (index, output) in transaction.output.iter().enumerate() {
            analysis.analyze_script_pubkey(index, output.script_pubkey.as_bytes());
        }
        for (index, txin) in transaction.input.iter().enumerate() {
            analysis.analyze_script_sig(index, &txin.script_sig);
            analysis.analyze_witness(index, &txin.witness);
        }
        analysis
    }

    fn violation(&mut self, rule: Rule, mut detail: Value) {
        self.violated[rule.index()] = true;
        if let Some(object) = detail.as_object_mut() {
            object.insert("rule".to_owned(), json!(rule.number()));
        }
        self.violations.push(detail);
    }

    fn ambiguity(&mut self, rules: &[Rule], mut detail: Value) {
        for rule in rules {
            self.ambiguous[rule.index()] = true;
        }
        if let Some(object) = detail.as_object_mut() {
            object.insert(
                "rules".to_owned(),
                json!(rules.iter().map(|rule| rule.number()).collect::<Vec<_>>()),
            );
        }
        self.ambiguities.push(detail);
    }

    fn first_violated_rule(&self) -> Option<Rule> {
        Rule::ALL
            .into_iter()
            .find(|rule| self.violated[rule.index()])
    }

    fn has_ambiguity(&self) -> bool {
        !self.ambiguities.is_empty()
    }

    fn evidence(&self) -> Value {
        let rules: Vec<Value> = Rule::ALL
            .into_iter()
            .map(|rule| {
                let outcome = if self.violated[rule.index()] {
                    "violated"
                } else if self.ambiguous[rule.index()] {
                    "ambiguous"
                } else {
                    "pass"
                };
                json!({ "rule": rule.number(), "outcome": outcome })
            })
            .collect();
        json!({
            "rules": rules,
            "violations": self.violations,
            "ambiguities": self.ambiguities,
            "inferences": self.inferences,
        })
    }

    /// Rule 1, always definitive: the outputs are in the transaction.
    fn analyze_script_pubkey(&mut self, output: usize, script_pubkey: &[u8]) {
        let op_return = script_pubkey.first() == Some(&opcodes::all::OP_RETURN.to_u8());
        let limit = if op_return {
            MAX_OP_RETURN_SCRIPT_PUBKEY_LEN
        } else {
            MAX_SCRIPT_PUBKEY_LEN
        };
        if script_pubkey.len() > limit {
            self.violation(
                Rule::OversizedScriptPubkey,
                json!({
                    "output": output,
                    "script_pubkey_len": script_pubkey.len(),
                    "op_return": op_return,
                }),
            );
        }
    }

    /// Rule 2 over scriptSig pushes. Only the final instruction of the
    /// scriptSig can be the exempt BIP16 redeemScript push, and only a P2SH
    /// prevout — which is unavailable — would confirm it, so an oversized
    /// final push is ambiguous while every other oversized push is a
    /// definitive violation.
    fn analyze_script_sig(&mut self, input: usize, script_sig: &Script) {
        if script_sig.is_empty() {
            return;
        }
        let mut push_lens: Vec<Option<usize>> = Vec::new();
        let mut undecodable = false;
        for instruction in script_sig.instructions() {
            match instruction {
                Ok(Instruction::PushBytes(push)) => push_lens.push(Some(push.len())),
                Ok(Instruction::Op(_)) => push_lens.push(None),
                Err(_) => {
                    undecodable = true;
                    break;
                }
            }
        }
        let last_index = push_lens.len().checked_sub(1);
        for (index, push_len) in push_lens.iter().enumerate() {
            let Some(push_len) = push_len else { continue };
            if *push_len <= MAX_DATA_PUSH_LEN {
                continue;
            }
            if !undecodable && Some(index) == last_index {
                self.ambiguity(
                    &[Rule::OversizedDataPush],
                    json!({
                        "input": input,
                        "reason": "final_script_sig_push_may_be_bip16_redeem_script",
                        "push_len": push_len,
                    }),
                );
            } else {
                self.violation(
                    Rule::OversizedDataPush,
                    json!({
                        "input": input,
                        "site": "script_sig_push",
                        "push_len": push_len,
                    }),
                );
            }
        }
        if undecodable {
            self.ambiguity(
                &[Rule::OversizedDataPush],
                json!({ "input": input, "reason": "script_sig_undecodable" }),
            );
        }
    }

    /// Rules 2, 4, 5, 6, and 7 over one witness, using the structural
    /// inference documented on the module.
    fn analyze_witness(&mut self, input: usize, witness: &Witness) {
        let elements: Vec<&[u8]> = witness.iter().collect();
        if elements.is_empty() {
            return;
        }
        let (stack, annex) = witness::split_annex_candidate(&elements);

        if let Some(shape) = witness::script_path_shape(stack) {
            let control_block = shape.control_block;
            let tapscript = shape.tapscript;
            let arguments = &shape.arguments;
            self.inferences.push(json!({
                "input": input,
                "basis": "taproot_script_path",
                "leaf_version": control_block[0] & 0xfe,
            }));
            if let Some(annex) = annex {
                self.violation(
                    Rule::AnnexPresent,
                    json!({
                        "input": input,
                        "annex_len": annex.len(),
                        "spend_shape": "script_path",
                    }),
                );
            }
            if control_block.len() > MAX_CONTROL_BLOCK_LEN {
                self.violation(
                    Rule::OversizedControlBlock,
                    json!({ "input": input, "control_block_len": control_block.len() }),
                );
            }
            for (element, bytes) in arguments.iter().enumerate() {
                if bytes.len() > MAX_DATA_PUSH_LEN {
                    self.violation(
                        Rule::OversizedDataPush,
                        json!({
                            "input": input,
                            "site": "script_path_argument",
                            "element": element,
                            "element_len": bytes.len(),
                        }),
                    );
                }
            }
            self.analyze_tapscript(input, tapscript);
        } else if stack.len() == 1 && KEY_PATH_SIGNATURE_LENS.contains(&stack[0].len()) {
            // A lone 64/65-byte element is at most a key-path signature:
            // within every rule-2 bound under every interpretation. The
            // annex claim is definitive only on this plausible shape.
            if let Some(annex) = annex {
                self.inferences
                    .push(json!({ "input": input, "basis": "taproot_key_path" }));
                self.violation(
                    Rule::AnnexPresent,
                    json!({
                        "input": input,
                        "annex_len": annex.len(),
                        "spend_shape": "key_path",
                    }),
                );
            }
        } else {
            if annex.is_some() {
                self.ambiguity(
                    &[Rule::AnnexPresent],
                    json!({ "input": input, "reason": "annex_marker_without_taproot_shape" }),
                );
            }
            // Without a definitive structural interpretation an oversized
            // element could be an exempt witness script or tapleaf script,
            // or sit in an undefined-witness-version stack that rule 2 does
            // not cover; all smaller elements pass definitively.
            for (element, bytes) in elements.iter().enumerate() {
                if bytes.len() > MAX_DATA_PUSH_LEN {
                    self.ambiguity(
                        &[Rule::OversizedDataPush],
                        json!({
                            "input": input,
                            "reason": "oversized_witness_element_without_definitive_interpretation",
                            "element": element,
                            "element_len": bytes.len(),
                        }),
                    );
                }
            }
        }
    }

    /// Rules 2, 6, and 7 inside one inferred tapscript. Decoding follows
    /// BIP-342: opcodes are scanned in order, and an `OP_SUCCESS*` makes the
    /// remaining bytes irrelevant, so scanning stops there.
    fn analyze_tapscript(&mut self, input: usize, tapscript: &[u8]) {
        let mut conditional_recorded = false;
        for instruction in Script::from_bytes(tapscript).instruction_indices() {
            match instruction {
                Ok((offset, Instruction::Op(opcode))) => {
                    let byte = opcode.to_u8();
                    if is_op_success(byte) {
                        self.violation(
                            Rule::OpSuccessInTapscript,
                            json!({ "input": input, "opcode": byte, "offset": offset }),
                        );
                        return;
                    }
                    let conditional = byte == opcodes::all::OP_IF.to_u8()
                        || byte == opcodes::all::OP_NOTIF.to_u8();
                    if conditional && !conditional_recorded {
                        conditional_recorded = true;
                        self.violation(
                            Rule::ConditionalInTapscript,
                            json!({
                                "input": input,
                                "opcode": byte,
                                "offset": offset,
                                "detection": "presence_not_execution",
                            }),
                        );
                    }
                }
                Ok((offset, Instruction::PushBytes(push))) => {
                    if push.len() > MAX_DATA_PUSH_LEN {
                        self.violation(
                            Rule::OversizedDataPush,
                            json!({
                                "input": input,
                                "site": "tapscript_push",
                                "push_len": push.len(),
                                "offset": offset,
                            }),
                        );
                    }
                }
                Err(_) => {
                    // The inferred tapscript stops decoding before its end
                    // with no OP_SUCCESS reached first. A real tapscript
                    // like this would be invalid under BIP-342 base rules,
                    // so the inference itself is in doubt and the remainder
                    // can hide anything: ambiguous, not violated.
                    self.ambiguity(
                        &[
                            Rule::OversizedDataPush,
                            Rule::OpSuccessInTapscript,
                            Rule::ConditionalInTapscript,
                        ],
                        json!({ "input": input, "reason": "inferred_tapscript_undecodable" }),
                    );
                    return;
                }
            }
        }
    }
}

/// BIP-342 `OP_SUCCESS*` opcodes: 80, 98, 126-129, 131-134, 137-138,
/// 141-142, 149-153, and 187-254.
const fn is_op_success(opcode: u8) -> bool {
    matches!(
        opcode,
        80 | 98 | 126..=129 | 131..=134 | 137..=138 | 141..=142 | 149..=153 | 187..=254
    )
}

#[cfg(test)]
mod tests {
    use bitcoin::ScriptBuf;

    use super::*;
    use crate::BaselineHeuristics;
    use crate::test_support::{
        output, p2tr_script, p2wpkh_script, script_sig_input, transaction_from, transaction_with,
        witness_input,
    };

    const TXID: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    fn classify(transaction: &Transaction) -> ClassificationResult {
        Bip110Conformance.classify(&ClassificationInput {
            txid: TXID,
            wtxid: None,
            transaction: Some(transaction),
        })
    }

    fn verdict(transaction: &Transaction) -> String {
        classify(transaction).verdict.expect("verdict present")
    }

    /// A clean 34-byte tapscript: `OP_PUSHBYTES_32 <key> OP_CHECKSIG`.
    fn clean_tapscript() -> Vec<u8> {
        let mut script = vec![0x20];
        script.extend_from_slice(&[0x07; 32]);
        script.push(0xac);
        script
    }

    /// A control-block-shaped element with `depth` merkle-path steps.
    fn control_block(depth: usize) -> Vec<u8> {
        let mut block = vec![0xc0];
        block.extend_from_slice(&vec![0x11; 32 + 32 * depth]);
        block
    }

    /// `OP_FALSE OP_IF OP_PUSHBYTES_3 "ord" OP_ENDIF`, the inscription-style
    /// envelope: a decodable tapscript containing OP_IF and the marker the
    /// behavior pack's data rule searches for.
    fn inscription_envelope_tapscript() -> Vec<u8> {
        vec![0x00, 0x63, 0x03, 0x6f, 0x72, 0x64, 0x68]
    }

    /// One-input transaction spending through the given witness elements.
    fn witness_transaction(elements: &[Vec<u8>]) -> Transaction {
        transaction_from(
            vec![witness_input(elements)],
            vec![output(50_000, p2wpkh_script())],
        )
    }

    fn script_path_transaction(
        arguments: &[Vec<u8>],
        tapscript: Vec<u8>,
        control_block: Vec<u8>,
    ) -> Transaction {
        let mut elements = arguments.to_vec();
        elements.push(tapscript);
        elements.push(control_block);
        witness_transaction(&elements)
    }

    /// An `OP_RETURN` scriptPubKey of exactly `total_len` bytes, built as
    /// `OP_RETURN OP_PUSHDATA1 <len> <payload>`.
    fn op_return_script_of_len(total_len: usize) -> ScriptBuf {
        let payload_len = total_len - 3;
        let mut bytes = vec![
            0x6a,
            0x4c,
            u8::try_from(payload_len).expect("payload fits u8"),
        ];
        bytes.extend_from_slice(&vec![0x42; payload_len]);
        assert_eq!(bytes.len(), total_len);
        ScriptBuf::from_bytes(bytes)
    }

    /// A scriptSig that is a sequence of pushes of the given payload sizes.
    fn push_script(push_lens: &[usize]) -> ScriptBuf {
        let mut bytes = Vec::new();
        for len in push_lens {
            let len_u16 = u16::try_from(*len).expect("push fits u16");
            bytes.push(0x4d);
            bytes.extend_from_slice(&len_u16.to_le_bytes());
            bytes.extend_from_slice(&vec![0x99; *len]);
        }
        ScriptBuf::from_bytes(bytes)
    }

    #[test]
    fn manifest_declares_identity_and_required_facts() {
        let manifest = Bip110Conformance.manifest();
        assert_eq!(manifest.id, "bip110-conformance");
        assert_eq!(manifest.version, "0.1.0");
        assert_eq!(manifest.required_facts, vec!["raw_transaction".to_owned()]);
    }

    #[test]
    fn taxonomy_declares_the_bip110_vocabulary_in_canonical_order() {
        let taxonomy = Bip110Conformance.taxonomy();
        assert_eq!(taxonomy.key, "bip110");
        assert_eq!(taxonomy.label, "BIP-110");
        let keys: Vec<&str> = taxonomy
            .verdicts
            .iter()
            .map(|verdict| verdict.key.as_str())
            .collect();
        assert_eq!(
            keys,
            vec![
                "conforming",
                "oversized_script_pubkey",
                "oversized_data_push",
                "annex_present",
                "oversized_control_block",
                "op_success_in_tapscript",
                "conditional_in_tapscript",
                "indeterminate",
                "unknown",
            ]
        );
    }

    #[test]
    fn missing_transaction_yields_unknown_status_without_verdict() {
        let result = Bip110Conformance.classify(&ClassificationInput {
            txid: TXID,
            wtxid: None,
            transaction: None,
        });
        assert_eq!(result.status, ClassificationStatus::Unknown);
        assert_eq!(result.verdict, None);
        assert_eq!(result.evidence, json!({ "missing": "raw_transaction" }));
    }

    #[test]
    fn a_plain_payment_transaction_conforms_with_full_evidence() {
        let transaction = transaction_with(
            2,
            vec![
                output(90_000, p2wpkh_script()),
                output(9_000, p2tr_script()),
            ],
        );
        let result = classify(&transaction);
        assert_eq!(result.status, ClassificationStatus::Complete);
        assert_eq!(result.verdict.as_deref(), Some("conforming"));
        assert_eq!(
            result.evidence,
            json!({
                "rules": [
                    { "rule": 1, "outcome": "pass" },
                    { "rule": 2, "outcome": "pass" },
                    { "rule": 4, "outcome": "pass" },
                    { "rule": 5, "outcome": "pass" },
                    { "rule": 6, "outcome": "pass" },
                    { "rule": 7, "outcome": "pass" },
                ],
                "violations": [],
                "ambiguities": [],
                "inferences": [],
            })
        );
    }

    #[test]
    fn rule_1_boundaries_split_at_34_bytes_and_83_byte_op_returns() {
        // p2tr_script() is exactly 34 bytes: the largest conforming
        // non-OP_RETURN scriptPubKey.
        assert_eq!(p2tr_script().len(), 34);
        let at_limit = transaction_with(1, vec![output(1_000, p2tr_script())]);
        assert_eq!(verdict(&at_limit), "conforming");

        let over_limit = transaction_with(
            1,
            vec![output(1_000, ScriptBuf::from_bytes(vec![0x51; 35]))],
        );
        let result = classify(&over_limit);
        assert_eq!(result.status, ClassificationStatus::Complete);
        assert_eq!(result.verdict.as_deref(), Some("oversized_script_pubkey"));
        assert_eq!(
            result.evidence["violations"],
            json!([{ "rule": 1, "output": 0, "script_pubkey_len": 35, "op_return": false }])
        );

        let op_return_at_limit = transaction_with(1, vec![output(0, op_return_script_of_len(83))]);
        assert_eq!(verdict(&op_return_at_limit), "conforming");

        let op_return_over_limit =
            transaction_with(1, vec![output(0, op_return_script_of_len(84))]);
        let result = classify(&op_return_over_limit);
        assert_eq!(result.verdict.as_deref(), Some("oversized_script_pubkey"));
        assert_eq!(
            result.evidence["violations"],
            json!([{ "rule": 1, "output": 0, "script_pubkey_len": 84, "op_return": true }])
        );
    }

    #[test]
    fn rule_2_splits_script_path_arguments_at_256_bytes() {
        let at_limit =
            script_path_transaction(&[vec![0xaa; 256]], clean_tapscript(), control_block(1));
        assert_eq!(verdict(&at_limit), "conforming");

        let over_limit =
            script_path_transaction(&[vec![0xaa; 257]], clean_tapscript(), control_block(1));
        let result = classify(&over_limit);
        assert_eq!(result.status, ClassificationStatus::Complete);
        assert_eq!(result.verdict.as_deref(), Some("oversized_data_push"));
        assert_eq!(
            result.evidence["violations"],
            json!([{
                "rule": 2,
                "input": 0,
                "site": "script_path_argument",
                "element": 0,
                "element_len": 257,
            }])
        );
        assert_eq!(
            result.evidence["inferences"],
            json!([{ "input": 0, "basis": "taproot_script_path", "leaf_version": 0xc0 }])
        );
    }

    #[test]
    fn rule_2_oversized_tapscript_internal_push_is_a_definitive_violation() {
        let mut tapscript = vec![0x4d];
        tapscript.extend_from_slice(&300_u16.to_le_bytes());
        tapscript.extend_from_slice(&vec![0x55; 300]);
        let transaction = script_path_transaction(&[], tapscript, control_block(1));
        let result = classify(&transaction);
        assert_eq!(result.verdict.as_deref(), Some("oversized_data_push"));
        assert_eq!(
            result.evidence["violations"],
            json!([{
                "rule": 2,
                "input": 0,
                "site": "tapscript_push",
                "push_len": 300,
                "offset": 0,
            }])
        );
    }

    #[test]
    fn rule_2_non_final_script_sig_push_is_a_definitive_violation() {
        let transaction = transaction_from(
            vec![script_sig_input(push_script(&[300, 20]))],
            vec![output(50_000, p2wpkh_script())],
        );
        let result = classify(&transaction);
        assert_eq!(result.status, ClassificationStatus::Complete);
        assert_eq!(result.verdict.as_deref(), Some("oversized_data_push"));
        assert_eq!(
            result.evidence["violations"],
            json!([{ "rule": 2, "input": 0, "site": "script_sig_push", "push_len": 300 }])
        );
    }

    #[test]
    fn rule_2_final_script_sig_push_alone_is_indeterminate() {
        let transaction = transaction_from(
            vec![script_sig_input(push_script(&[300]))],
            vec![output(50_000, p2wpkh_script())],
        );
        let result = classify(&transaction);
        assert_eq!(result.status, ClassificationStatus::Partial);
        assert_eq!(result.verdict.as_deref(), Some("indeterminate"));
        assert_eq!(
            result.evidence["ambiguities"],
            json!([{
                "rules": [2],
                "input": 0,
                "reason": "final_script_sig_push_may_be_bip16_redeem_script",
                "push_len": 300,
            }])
        );
    }

    #[test]
    fn rule_2_script_sig_pushes_at_the_256_byte_limit_conform() {
        let transaction = transaction_from(
            vec![script_sig_input(push_script(&[256, 256]))],
            vec![output(50_000, p2wpkh_script())],
        );
        assert_eq!(verdict(&transaction), "conforming");
    }

    #[test]
    fn rule_4_annex_on_a_key_path_spend_is_a_definitive_violation() {
        let bare_key_path = witness_transaction(&[vec![0x01; 64]]);
        assert_eq!(verdict(&bare_key_path), "conforming");

        let with_annex = witness_transaction(&[vec![0x01; 64], vec![0x50, 0x01, 0x02]]);
        let result = classify(&with_annex);
        assert_eq!(result.status, ClassificationStatus::Complete);
        assert_eq!(result.verdict.as_deref(), Some("annex_present"));
        assert_eq!(
            result.evidence["violations"],
            json!([{ "rule": 4, "input": 0, "annex_len": 3, "spend_shape": "key_path" }])
        );
        assert_eq!(
            result.evidence["inferences"],
            json!([{ "input": 0, "basis": "taproot_key_path" }])
        );
    }

    #[test]
    fn rule_4_annex_on_a_script_path_spend_is_a_definitive_violation() {
        let transaction =
            witness_transaction(&[clean_tapscript(), control_block(1), vec![0x50, 0xff]]);
        let result = classify(&transaction);
        assert_eq!(result.verdict.as_deref(), Some("annex_present"));
        assert_eq!(
            result.evidence["violations"],
            json!([{ "rule": 4, "input": 0, "annex_len": 2, "spend_shape": "script_path" }])
        );
    }

    #[test]
    fn annex_marker_without_a_taproot_shape_is_indeterminate() {
        // After stripping the 0x50-led candidate, one 10-byte element
        // remains: neither a key-path signature nor a script-path stack.
        let transaction = witness_transaction(&[vec![0x0a; 10], vec![0x50, 0x00]]);
        let result = classify(&transaction);
        assert_eq!(result.status, ClassificationStatus::Partial);
        assert_eq!(result.verdict.as_deref(), Some("indeterminate"));
        assert_eq!(
            result.evidence["ambiguities"],
            json!([{
                "rules": [4],
                "input": 0,
                "reason": "annex_marker_without_taproot_shape",
            }])
        );
    }

    #[test]
    fn rule_5_splits_control_blocks_at_257_bytes() {
        // 257 bytes = 33 + 32 * 7: the deepest conforming merkle path.
        let at_limit = script_path_transaction(&[], clean_tapscript(), control_block(7));
        assert_eq!(verdict(&at_limit), "conforming");

        let over_limit = script_path_transaction(&[], clean_tapscript(), control_block(8));
        let result = classify(&over_limit);
        assert_eq!(result.status, ClassificationStatus::Complete);
        assert_eq!(result.verdict.as_deref(), Some("oversized_control_block"));
        assert_eq!(
            result.evidence["violations"],
            json!([{ "rule": 5, "input": 0, "control_block_len": 289 }])
        );
    }

    #[test]
    fn rule_6_op_success_anywhere_in_an_inferred_tapscript_violates() {
        for opcode in [0x50_u8, 0xbb, 0xfe] {
            let transaction = script_path_transaction(&[], vec![opcode], control_block(1));
            let result = classify(&transaction);
            assert_eq!(result.status, ClassificationStatus::Complete);
            assert_eq!(
                result.verdict.as_deref(),
                Some("op_success_in_tapscript"),
                "opcode {opcode:#x}"
            );
            assert_eq!(
                result.evidence["violations"],
                json!([{ "rule": 6, "input": 0, "opcode": opcode, "offset": 0 }])
            );
        }
    }

    #[test]
    fn rule_7_inscription_envelope_is_conditional_in_tapscript_and_data_behavior() {
        let transaction =
            script_path_transaction(&[], inscription_envelope_tapscript(), control_block(1));
        let result = classify(&transaction);
        assert_eq!(result.status, ClassificationStatus::Complete);
        assert_eq!(result.verdict.as_deref(), Some("conditional_in_tapscript"));
        assert_eq!(
            result.evidence["violations"],
            json!([{
                "rule": 7,
                "input": 0,
                "opcode": 0x63,
                "offset": 1,
                "detection": "presence_not_execution",
            }])
        );

        // Deliberate, correct overlap: the same inscription envelope is the
        // behavior taxonomy's `data`.
        let behavior = BaselineHeuristics.classify(&ClassificationInput {
            txid: TXID,
            wtxid: None,
            transaction: Some(&transaction),
        });
        assert_eq!(behavior.verdict.as_deref(), Some("data"));
    }

    #[test]
    fn an_undecodable_inferred_tapscript_is_indeterminate() {
        // OP_PUSHDATA2 declaring 65535 bytes with none present.
        let transaction = script_path_transaction(&[], vec![0x4d, 0xff, 0xff], control_block(1));
        let result = classify(&transaction);
        assert_eq!(result.status, ClassificationStatus::Partial);
        assert_eq!(result.verdict.as_deref(), Some("indeterminate"));
        assert_eq!(
            result.evidence["ambiguities"],
            json!([{
                "rules": [2, 6, 7],
                "input": 0,
                "reason": "inferred_tapscript_undecodable",
            }])
        );
    }

    #[test]
    fn oversized_witness_element_without_interpretation_is_indeterminate() {
        // A single 300-byte element could be an exempt witness script or an
        // undefined-witness-version item; never a definitive violation.
        let transaction = witness_transaction(&[vec![0x0b; 300]]);
        let result = classify(&transaction);
        assert_eq!(result.status, ClassificationStatus::Partial);
        assert_eq!(result.verdict.as_deref(), Some("indeterminate"));
        assert_eq!(
            result.evidence["ambiguities"],
            json!([{
                "rules": [2],
                "input": 0,
                "reason": "oversized_witness_element_without_definitive_interpretation",
                "element": 0,
                "element_len": 300,
            }])
        );
    }

    #[test]
    fn small_uninterpreted_witnesses_conform_definitively() {
        // The seed and fixture transactions carry one 107-byte element.
        let transaction = witness_transaction(&[vec![0xab; 107]]);
        assert_eq!(verdict(&transaction), "conforming");
    }

    #[test]
    fn multi_violation_precedence_follows_spec_rule_order() {
        // Rule 1 (oversized scriptPubKey) plus rule 6 (OP_SUCCESS in an
        // inferred tapscript): the verdict is the first violated rule in
        // spec order, and evidence keeps the complete list.
        let transaction = transaction_from(
            vec![witness_input(&[vec![0xbb], control_block(1)])],
            vec![output(1_000, ScriptBuf::from_bytes(vec![0x51; 40]))],
        );
        let result = classify(&transaction);
        assert_eq!(result.status, ClassificationStatus::Complete);
        assert_eq!(result.verdict.as_deref(), Some("oversized_script_pubkey"));
        assert_eq!(
            result.evidence["violations"],
            json!([
                { "rule": 1, "output": 0, "script_pubkey_len": 40, "op_return": false },
                { "rule": 6, "input": 0, "opcode": 0xbb, "offset": 0 },
            ])
        );
        assert_eq!(
            result.evidence["rules"],
            json!([
                { "rule": 1, "outcome": "violated" },
                { "rule": 2, "outcome": "pass" },
                { "rule": 4, "outcome": "pass" },
                { "rule": 5, "outcome": "pass" },
                { "rule": 6, "outcome": "violated" },
                { "rule": 7, "outcome": "pass" },
            ])
        );
    }

    #[test]
    fn a_definitive_violation_outranks_a_coexisting_ambiguity() {
        // An oversized final scriptSig push (ambiguous) next to an oversized
        // scriptPubKey (definitive): the definitive verdict wins with the
        // ambiguity preserved in evidence.
        let transaction = transaction_from(
            vec![script_sig_input(push_script(&[300]))],
            vec![output(1_000, ScriptBuf::from_bytes(vec![0x51; 35]))],
        );
        let result = classify(&transaction);
        assert_eq!(result.status, ClassificationStatus::Complete);
        assert_eq!(result.verdict.as_deref(), Some("oversized_script_pubkey"));
        assert_eq!(
            result.evidence["ambiguities"].as_array().map(Vec::len),
            Some(1)
        );
    }
}
