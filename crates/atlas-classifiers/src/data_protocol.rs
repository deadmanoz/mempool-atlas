//! Data-carrying-protocol fingerprints of one parsed transaction.
//!
//! This pack owns the `data_protocol` taxonomy: which known data-embedding
//! protocol a transaction carries, detected from the transaction's own bytes.
//! Every detection is a protocol *fingerprint* — a documented byte- or
//! script-level pattern match — not an authoritative protocol decode. A
//! firing fingerprint is a definitive statement that the pattern is present;
//! false negatives are expected for exotic or deliberately obfuscated
//! encodings, and a `none` verdict means no registered fingerprint fired,
//! never that the transaction provably carries no data.
//!
//! Detected carriers, in verdict (and precedence) order:
//!
//! 1. `brc20` — an ordinals inscription envelope whose content-type is
//!    JSON-ish (contains `json` or starts with `text/plain`) and whose
//!    reassembled payload contains `"p":"brc-20"` after stripping ASCII
//!    whitespace. Full JSON validity is deliberately not required.
//! 2. `inscription` — the ordinals envelope `OP_FALSE OP_IF <push "ord">`
//!    decoded from an inferred tapscript when the input is Taproot
//!    script-path-shaped, or — as a weaker, lower-confidence signal — the
//!    raw byte pattern `0x0063036f7264` inside any witness element.
//! 3. `runes` — an output scriptPubKey opening `OP_RETURN OP_13`, the
//!    runestone marker.
//! 4. `stamps` — a bare-multisig output (`OP_1..OP_3 <33-byte pushes>...
//!    OP_N OP_CHECKMULTISIG`, at least two key pushes) where at least one
//!    "pubkey" lacks a plausible compressed-key prefix (`0x02`/`0x03`): the
//!    classic data-in-keys pattern.
//! 5. `counterparty` — an `OP_RETURN` or bare-multisig payload opening with
//!    the 8-byte `CNTRPRTY` prefix, checked both in plaintext and after
//!    ARC4 decryption keyed by the transaction's first-input previous txid.
//!    Real-world implementations key ARC4 with the txid's display-order
//!    (reversed-hex) bytes; both display and internal byte orders are
//!    checked and the matching order is recorded in evidence.
//! 6. `omni` — an `OP_RETURN` payload opening with the ASCII `omni` prefix.
//! 7. `op_return_other` — any other `OP_RETURN`-bearing output: a data
//!    carrier whose protocol is unrecognized.
//!
//! A transaction with multiple carriers receives the highest-precedence
//! verdict with the complete detection list in evidence. `none` states that
//! no fingerprint fired; carrier-capable structures that matched no protocol
//! (an envelope-less inferred tapscript, a bare multisig with plausible
//! keys) are listed separately in evidence rather than guessed at.

use atlas_model::{TaxonomyDescriptor, VerdictDescriptor};
use bitcoin::script::Instruction;
use bitcoin::{Script, Transaction, Witness, opcodes};
use serde_json::{Value, json};

use crate::{
    ClassificationInput, ClassificationResult, ClassificationStatus, Classifier,
    ClassifierManifest, witness,
};

const CLASSIFIER_ID: &str = "data-protocol-fingerprints";
const CLASSIFIER_VERSION: &str = "0.1.0";

/// `OP_FALSE OP_IF OP_PUSHBYTES_3 "ord"`: the byte pattern opening an
/// ordinals inscription envelope, used as the weak fallback signal.
const ORD_ENVELOPE_BYTES: [u8; 6] = [0x00, 0x63, 0x03, 0x6f, 0x72, 0x64];
/// The envelope's protocol identifier push.
const ORD_PROTOCOL_ID: &[u8] = b"ord";
/// The envelope field tag carrying the content type.
const ORD_CONTENT_TYPE_TAG: &[u8] = &[0x01];
/// The Counterparty payload prefix.
const CNTRPRTY_PREFIX: &[u8] = b"CNTRPRTY";
/// The Omni Layer payload prefix.
const OMNI_PREFIX: &[u8] = b"omni";
/// Length of one bare-multisig "pubkey" push.
const MULTISIG_KEY_LEN: usize = 33;
/// Plausible compressed-pubkey prefixes; anything else marks a data key.
const PLAUSIBLE_KEY_PREFIXES: [u8; 2] = [0x02, 0x03];
/// Evidence caps the recorded unrecognized-payload head at this many bytes.
const PAYLOAD_HEAD_CAP: usize = 16;

/// The protocols this pack fingerprints, in verdict and precedence order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Protocol {
    Brc20,
    Inscription,
    Runes,
    Stamps,
    Counterparty,
    Omni,
    OpReturnOther,
}

impl Protocol {
    const ALL: [Self; 7] = [
        Self::Brc20,
        Self::Inscription,
        Self::Runes,
        Self::Stamps,
        Self::Counterparty,
        Self::Omni,
        Self::OpReturnOther,
    ];

    const fn verdict_key(self) -> &'static str {
        match self {
            Self::Brc20 => "brc20",
            Self::Inscription => "inscription",
            Self::Runes => "runes",
            Self::Stamps => "stamps",
            Self::Counterparty => "counterparty",
            Self::Omni => "omni",
            Self::OpReturnOther => "op_return_other",
        }
    }

    const fn verdict_label(self) -> &'static str {
        match self {
            Self::Brc20 => "BRC-20",
            Self::Inscription => "Inscription",
            Self::Runes => "Runes",
            Self::Stamps => "Stamps",
            Self::Counterparty => "Counterparty",
            Self::Omni => "Omni",
            Self::OpReturnOther => "Other OP_RETURN",
        }
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|protocol| *protocol == self)
            .expect("every protocol is in ALL")
    }
}

/// The data-carrying-protocol pack, owner of the `data_protocol` taxonomy.
/// Without a parsed transaction it reports [`ClassificationStatus::Unknown`];
/// with one it always completes: fingerprints are definitive pattern matches,
/// and a fingerprint-less transaction receives the explicit `none`.
#[derive(Clone, Copy, Debug, Default)]
pub struct DataProtocolFingerprints;

impl Classifier for DataProtocolFingerprints {
    fn manifest(&self) -> ClassifierManifest {
        ClassifierManifest {
            id: CLASSIFIER_ID.to_owned(),
            version: CLASSIFIER_VERSION.to_owned(),
            required_facts: vec!["raw_transaction".to_owned()],
        }
    }

    fn taxonomy(&self) -> TaxonomyDescriptor {
        let mut verdicts: Vec<VerdictDescriptor> = Protocol::ALL
            .into_iter()
            .map(|protocol| VerdictDescriptor {
                key: protocol.verdict_key().to_owned(),
                label: protocol.verdict_label().to_owned(),
            })
            .collect();
        verdicts.push(VerdictDescriptor {
            key: "none".to_owned(),
            label: "No data carrier".to_owned(),
        });
        verdicts.push(VerdictDescriptor {
            key: "unknown".to_owned(),
            label: "Unknown".to_owned(),
        });
        TaxonomyDescriptor {
            key: "data_protocol".to_owned(),
            label: "Data protocol".to_owned(),
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
        let verdict = analysis.verdict().map_or("none", Protocol::verdict_key);
        result(
            ClassificationStatus::Complete,
            Some(verdict),
            analysis.evidence(),
        )
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

/// Minimal RC4 (KSA plus PRGA). RC4 is symmetric, so the same function
/// encrypts and decrypts. Counterparty keys it with the transaction's
/// first-input previous txid; this exists only to recognize that prefix and
/// must never be used as actual cryptography.
#[must_use]
pub fn arc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    assert!(!key.is_empty(), "RC4 requires a non-empty key");
    #[allow(clippy::cast_possible_truncation)]
    let mut state: [u8; 256] = std::array::from_fn(|index| index as u8);
    let mut j: u8 = 0;
    for i in 0..256 {
        j = j.wrapping_add(state[i]).wrapping_add(key[i % key.len()]);
        state.swap(i, usize::from(j));
    }
    let (mut i, mut j) = (0_u8, 0_u8);
    let mut out = Vec::with_capacity(data.len());
    for byte in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(state[usize::from(i)]);
        state.swap(usize::from(i), usize::from(j));
        let keystream =
            state[usize::from(state[usize::from(i)].wrapping_add(state[usize::from(j)]))];
        out.push(byte ^ keystream);
    }
    out
}

/// An owned, decoded script instruction.
enum Instr {
    Push(Vec<u8>),
    Op(u8),
}

/// Decodes `script` into owned instructions, stopping at the first
/// undecodable instruction. The boolean states whether the whole script
/// decoded.
fn decoded_instructions(script: &[u8]) -> (Vec<Instr>, bool) {
    let mut instructions = Vec::new();
    for instruction in Script::from_bytes(script).instructions() {
        match instruction {
            Ok(Instruction::PushBytes(push)) => {
                instructions.push(Instr::Push(push.as_bytes().to_vec()));
            }
            Ok(Instruction::Op(opcode)) => instructions.push(Instr::Op(opcode.to_u8())),
            Err(_) => return (instructions, false),
        }
    }
    (instructions, true)
}

/// The fields of one decoded ordinals envelope.
struct OrdEnvelope {
    content_type: Option<Vec<u8>>,
    body: Vec<u8>,
    /// Whether parsing reached the closing `OP_ENDIF` with well-formed
    /// fields. An incomplete envelope is still an inscription detection;
    /// its fields are only what parsed.
    complete: bool,
}

/// Decodes an ordinals envelope from an inferred tapscript's instructions:
/// `OP_FALSE OP_IF <push "ord">`, then `(tag push, value push)` field pairs
/// until an empty push starts the body, whose pushes concatenate until
/// `OP_ENDIF`.
fn parse_ord_envelope(tapscript: &[u8]) -> Option<OrdEnvelope> {
    let (instructions, _complete) = decoded_instructions(tapscript);
    let op_if = opcodes::all::OP_IF.to_u8();
    let start = instructions.windows(3).position(|window| {
        matches!(&window[0], Instr::Push(bytes) if bytes.is_empty())
            && matches!(&window[1], Instr::Op(opcode) if *opcode == op_if)
            && matches!(&window[2], Instr::Push(bytes) if bytes.as_slice() == ORD_PROTOCOL_ID)
    })?;

    let mut envelope = OrdEnvelope {
        content_type: None,
        body: Vec::new(),
        complete: false,
    };
    let mut in_body = false;
    let mut index = start + 3;
    while index < instructions.len() {
        match &instructions[index] {
            Instr::Op(opcode) if *opcode == opcodes::all::OP_ENDIF.to_u8() => {
                envelope.complete = true;
                break;
            }
            Instr::Push(bytes) if in_body => {
                envelope.body.extend_from_slice(bytes);
                index += 1;
            }
            Instr::Push(bytes) if bytes.is_empty() => {
                in_body = true;
                index += 1;
            }
            Instr::Push(tag) => {
                let Some(Instr::Push(value)) = instructions.get(index + 1) else {
                    break;
                };
                if tag.as_slice() == ORD_CONTENT_TYPE_TAG {
                    envelope.content_type = Some(value.clone());
                }
                index += 2;
            }
            Instr::Op(_) => break,
        }
    }
    Some(envelope)
}

/// Whether an envelope content-type plausibly carries JSON: it contains
/// `json` or opens with `text/plain`. A heuristic gate, not a MIME parse.
fn json_ish_content_type(content_type: &[u8]) -> bool {
    let lowered = String::from_utf8_lossy(content_type).to_lowercase();
    lowered.contains("json") || lowered.starts_with("text/plain")
}

/// Whether the envelope body contains `"p":"brc-20"` after stripping ASCII
/// whitespace. Deliberately weaker than JSON validation.
fn body_has_brc20_marker(body: &[u8]) -> bool {
    let stripped: Vec<u8> = body
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    let marker = br#""p":"brc-20""#;
    stripped
        .windows(marker.len())
        .any(|window| window == &marker[..])
}

/// One bare-multisig scriptPubKey: `OP_m <33-byte pushes>... OP_n
/// OP_CHECKMULTISIG` with at least two key pushes.
struct BareMultisig {
    required: u8,
    keys: Vec<Vec<u8>>,
}

fn parse_bare_multisig(script: &[u8]) -> Option<BareMultisig> {
    let (instructions, complete) = decoded_instructions(script);
    if !complete || instructions.len() < 5 {
        return None;
    }
    let pushnum_base = opcodes::all::OP_PUSHNUM_1.to_u8() - 1;
    let Instr::Op(required_op) = instructions[0] else {
        return None;
    };
    let required = required_op.checked_sub(pushnum_base)?;
    if !(1..=3).contains(&required) {
        return None;
    }
    let Instr::Op(last) = instructions[instructions.len() - 1] else {
        return None;
    };
    if last != opcodes::all::OP_CHECKMULTISIG.to_u8() {
        return None;
    }
    let Instr::Op(total_op) = instructions[instructions.len() - 2] else {
        return None;
    };
    let total = total_op.checked_sub(pushnum_base)?;
    let keys: Vec<Vec<u8>> = instructions[1..instructions.len() - 2]
        .iter()
        .map(|instruction| match instruction {
            Instr::Push(bytes) if bytes.len() == MULTISIG_KEY_LEN => Some(bytes.clone()),
            _ => None,
        })
        .collect::<Option<_>>()?;
    (keys.len() >= 2 && usize::from(total) == keys.len() && required <= total)
        .then_some(BareMultisig { required, keys })
}

/// The concatenated data pushes after `OP_RETURN`, or the raw remaining
/// bytes when they do not decode as plain pushes.
fn op_return_payload(script: &[u8]) -> Vec<u8> {
    let remainder = &script[1..];
    let (instructions, complete) = decoded_instructions(remainder);
    if !complete
        || instructions
            .iter()
            .any(|instruction| matches!(instruction, Instr::Op(_)))
    {
        return remainder.to_vec();
    }
    let mut payload = Vec::new();
    for instruction in instructions {
        if let Instr::Push(bytes) = instruction {
            payload.extend_from_slice(&bytes);
        }
    }
    payload
}

/// The ARC4 key candidates derived from the first input's previous txid:
/// display-order (reversed-hex, the order real implementations key on)
/// first, then internal byte order.
fn arc4_key_candidates(transaction: &Transaction) -> Vec<(&'static str, [u8; 32])> {
    let Some(first_input) = transaction.input.first() else {
        return Vec::new();
    };
    let internal: [u8; 32] = *first_input.previous_output.txid.as_ref();
    let mut display = internal;
    display.reverse();
    vec![("display", display), ("internal", internal)]
}

/// Whether `payload` opens with `CNTRPRTY` in plaintext or under any ARC4
/// key candidate; the matching encoding travels in the returned detail.
fn counterparty_match(payload: &[u8], keys: &[(&'static str, [u8; 32])]) -> Option<Value> {
    if payload.starts_with(CNTRPRTY_PREFIX) {
        return Some(json!({ "encoding": "plaintext" }));
    }
    if payload.len() < CNTRPRTY_PREFIX.len() {
        return None;
    }
    for (order, key) in keys {
        if arc4(key, &payload[..CNTRPRTY_PREFIX.len()]) == CNTRPRTY_PREFIX {
            return Some(json!({ "encoding": "arc4", "arc4_key_order": order }));
        }
    }
    None
}

/// Accumulated detections and unmatched carrier-capable structures for one
/// transaction.
#[derive(Default)]
struct Analysis {
    detections: Vec<(Protocol, Value)>,
    unmatched_structures: Vec<Value>,
}

impl Analysis {
    fn of(transaction: &Transaction) -> Self {
        let mut analysis = Self::default();
        let arc4_keys = arc4_key_candidates(transaction);
        for (index, txin) in transaction.input.iter().enumerate() {
            analysis.analyze_witness(index, &txin.witness);
        }
        for (index, txout) in transaction.output.iter().enumerate() {
            analysis.analyze_output(index, txout.script_pubkey.as_bytes(), &arc4_keys);
        }
        analysis
    }

    fn detect(&mut self, protocol: Protocol, mut detail: Value) {
        if let Some(object) = detail.as_object_mut() {
            object.insert("protocol".to_owned(), json!(protocol.verdict_key()));
        }
        self.detections.push((protocol, detail));
    }

    /// The highest-precedence detected protocol, if any fingerprint fired.
    fn verdict(&self) -> Option<Protocol> {
        self.detections
            .iter()
            .map(|(protocol, _)| *protocol)
            .min_by_key(|protocol| protocol.index())
    }

    fn evidence(&self) -> Value {
        let detections: Vec<&Value> = self.detections.iter().map(|(_, detail)| detail).collect();
        json!({
            "detections": detections,
            "unmatched_structures": self.unmatched_structures,
        })
    }

    /// Inscription and BRC-20 detection over one witness: the strong path
    /// decodes an envelope from the inferred tapscript of a Taproot
    /// script-path-shaped witness; otherwise the documented byte pattern
    /// inside any element is a weaker, lower-confidence signal.
    fn analyze_witness(&mut self, input: usize, witness_data: &Witness) {
        let elements: Vec<&[u8]> = witness_data.iter().collect();
        if elements.is_empty() {
            return;
        }
        let (stack, _annex_candidate) = witness::split_annex_candidate(&elements);
        let mut envelope_detected = false;
        if let Some(shape) = witness::script_path_shape(stack) {
            if let Some(envelope) = parse_ord_envelope(shape.tapscript) {
                envelope_detected = true;
                self.detect_envelope(input, &envelope);
            } else {
                self.unmatched_structures.push(json!({
                    "input": input,
                    "structure": "taproot_script_path",
                }));
            }
        }
        if !envelope_detected
            && elements.iter().any(|element| {
                element
                    .windows(ORD_ENVELOPE_BYTES.len())
                    .any(|window| window == ORD_ENVELOPE_BYTES)
            })
        {
            self.detect(
                Protocol::Inscription,
                json!({
                    "input": input,
                    "detection": "witness_byte_pattern",
                    "confidence": "low",
                }),
            );
        }
    }

    fn detect_envelope(&mut self, input: usize, envelope: &OrdEnvelope) {
        let content_type = envelope
            .content_type
            .as_ref()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned());
        let brc20 = envelope
            .content_type
            .as_ref()
            .is_some_and(|bytes| json_ish_content_type(bytes))
            && body_has_brc20_marker(&envelope.body);
        if brc20 {
            self.detect(
                Protocol::Brc20,
                json!({
                    "input": input,
                    "detection": "tapscript_envelope",
                    "content_type": content_type,
                    "payload_check": "whitespace_stripped_substring",
                    "note": "json-ish content-type heuristic; full JSON validity not required",
                }),
            );
        } else {
            self.detect(
                Protocol::Inscription,
                json!({
                    "input": input,
                    "detection": "tapscript_envelope",
                    "content_type": content_type,
                    "body_len": envelope.body.len(),
                    "envelope_complete": envelope.complete,
                }),
            );
        }
    }

    /// Output-side carriers: runestones, Counterparty, Omni, and other
    /// `OP_RETURN` data over `OP_RETURN` outputs; Stamps and Counterparty
    /// over bare-multisig outputs.
    fn analyze_output(
        &mut self,
        output: usize,
        script: &[u8],
        arc4_keys: &[(&'static str, [u8; 32])],
    ) {
        if script.first() == Some(&opcodes::all::OP_RETURN.to_u8()) {
            self.analyze_op_return(output, script, arc4_keys);
        } else if let Some(multisig) = parse_bare_multisig(script) {
            self.analyze_bare_multisig(output, &multisig, arc4_keys);
        }
    }

    fn analyze_op_return(
        &mut self,
        output: usize,
        script: &[u8],
        arc4_keys: &[(&'static str, [u8; 32])],
    ) {
        if script.get(1) == Some(&opcodes::all::OP_PUSHNUM_13.to_u8()) {
            self.detect(
                Protocol::Runes,
                json!({
                    "output": output,
                    "detection": "op_return_op_13",
                    "payload_len": script.len().saturating_sub(2),
                }),
            );
            return;
        }
        let payload = op_return_payload(script);
        if let Some(detail) = counterparty_match(&payload, arc4_keys) {
            let mut detail = detail;
            if let Some(object) = detail.as_object_mut() {
                object.insert("output".to_owned(), json!(output));
                object.insert("carrier".to_owned(), json!("op_return"));
            }
            self.detect(Protocol::Counterparty, detail);
            return;
        }
        if payload.starts_with(OMNI_PREFIX) {
            self.detect(
                Protocol::Omni,
                json!({
                    "output": output,
                    "carrier": "op_return",
                    "payload_len": payload.len(),
                }),
            );
            return;
        }
        let head = &payload[..payload.len().min(PAYLOAD_HEAD_CAP)];
        self.detect(
            Protocol::OpReturnOther,
            json!({
                "output": output,
                "payload_len": payload.len(),
                "payload_head_hex": hex_string(head),
            }),
        );
    }

    fn analyze_bare_multisig(
        &mut self,
        output: usize,
        multisig: &BareMultisig,
        arc4_keys: &[(&'static str, [u8; 32])],
    ) {
        let implausible_key_count = multisig
            .keys
            .iter()
            .filter(|key| {
                !key.first()
                    .is_some_and(|byte| PLAUSIBLE_KEY_PREFIXES.contains(byte))
            })
            .count();
        let mut matched = false;
        if implausible_key_count >= 1 {
            matched = true;
            self.detect(
                Protocol::Stamps,
                json!({
                    "output": output,
                    "detection": "bare_multisig_data_keys",
                    "required": multisig.required,
                    "key_count": multisig.keys.len(),
                    "implausible_key_count": implausible_key_count,
                }),
            );
        }
        // Counterparty multisig data: every key but the (presumed real)
        // last one carries data in its middle bytes; the reassembled data
        // may open with a length byte before the prefix.
        let mut payload = Vec::new();
        for key in &multisig.keys[..multisig.keys.len() - 1] {
            payload.extend_from_slice(&key[1..MULTISIG_KEY_LEN - 1]);
        }
        for (offset, candidate) in [
            (0_usize, payload.as_slice()),
            (1, payload.get(1..).unwrap_or_default()),
        ] {
            if let Some(detail) = counterparty_match(candidate, arc4_keys) {
                let mut detail = detail;
                if let Some(object) = detail.as_object_mut() {
                    object.insert("output".to_owned(), json!(output));
                    object.insert("carrier".to_owned(), json!("bare_multisig"));
                    object.insert("data_offset".to_owned(), json!(offset));
                }
                matched = true;
                self.detect(Protocol::Counterparty, detail);
                break;
            }
        }
        if !matched {
            self.unmatched_structures.push(json!({
                "output": output,
                "structure": "bare_multisig",
            }));
        }
    }
}

fn hex_string(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(out, "{byte:02x}").expect("writing to a String cannot fail");
    }
    out
}

#[cfg(test)]
mod tests {
    use bitcoin::hashes::Hash as _;
    use bitcoin::{OutPoint, ScriptBuf, Sequence, TxIn, Txid, Witness};

    use super::*;
    use crate::test_support::{
        op_return_script, output, p2tr_script, p2wpkh_script, transaction_from, transaction_with,
        witness_input,
    };

    const TXID: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    fn classify(transaction: &Transaction) -> ClassificationResult {
        DataProtocolFingerprints.classify(&ClassificationInput {
            txid: TXID,
            wtxid: None,
            transaction: Some(transaction),
        })
    }

    fn verdict(transaction: &Transaction) -> String {
        classify(transaction).verdict.expect("verdict present")
    }

    fn push(script: &mut Vec<u8>, bytes: &[u8]) {
        let len = u8::try_from(bytes.len()).expect("push fits one length byte");
        assert!(len < 0x4c, "test pushes stay below OP_PUSHDATA1");
        script.push(len);
        script.extend_from_slice(bytes);
    }

    /// A full inscription envelope tapscript: `<key> OP_CHECKSIG OP_FALSE
    /// OP_IF "ord" <tag 1> <content-type> OP_0 <body> OP_ENDIF`.
    fn envelope_tapscript(content_type: &[u8], body: &[u8]) -> Vec<u8> {
        let mut script = vec![0x20];
        script.extend_from_slice(&[0x07; 32]);
        script.push(0xac);
        script.extend_from_slice(&[0x00, 0x63]);
        push(&mut script, b"ord");
        push(&mut script, &[0x01]);
        push(&mut script, content_type);
        script.push(0x00);
        push(&mut script, body);
        script.push(0x68);
        script
    }

    /// A control-block-shaped element with `depth` merkle-path steps.
    fn control_block(depth: usize) -> Vec<u8> {
        let mut block = vec![0xc0];
        block.extend_from_slice(&vec![0x11; 32 + 32 * depth]);
        block
    }

    fn script_path_transaction(tapscript: Vec<u8>) -> Transaction {
        transaction_from(
            vec![witness_input(&[
                vec![0x01; 64],
                tapscript,
                control_block(0),
            ])],
            vec![output(546, p2tr_script())],
        )
    }

    fn input_with_prevout(txid_bytes: [u8; 32]) -> TxIn {
        TxIn {
            previous_output: OutPoint {
                txid: Txid::from_byte_array(txid_bytes),
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::from_slice(&[vec![0xab_u8; 107]]),
        }
    }

    /// `OP_1 <keys> OP_n OP_CHECKMULTISIG` from 33-byte keys.
    fn bare_multisig_script(keys: &[Vec<u8>]) -> ScriptBuf {
        let mut script = vec![0x51];
        for key in keys {
            assert_eq!(key.len(), 33);
            push(&mut script, key);
        }
        script.push(0x50 + u8::try_from(keys.len()).expect("small key count"));
        script.push(0xae);
        ScriptBuf::from_bytes(script)
    }

    fn data_key(prefix: u8, fill: u8) -> Vec<u8> {
        let mut key = vec![prefix];
        key.extend_from_slice(&[fill; 32]);
        key
    }

    #[test]
    fn manifest_declares_identity_and_required_facts() {
        let manifest = DataProtocolFingerprints.manifest();
        assert_eq!(manifest.id, "data-protocol-fingerprints");
        assert_eq!(manifest.version, "0.1.0");
        assert_eq!(manifest.required_facts, vec!["raw_transaction".to_owned()]);
    }

    #[test]
    fn taxonomy_declares_the_data_protocol_vocabulary_in_precedence_order() {
        let taxonomy = DataProtocolFingerprints.taxonomy();
        assert_eq!(taxonomy.key, "data_protocol");
        assert_eq!(taxonomy.label, "Data protocol");
        let keys: Vec<&str> = taxonomy
            .verdicts
            .iter()
            .map(|verdict| verdict.key.as_str())
            .collect();
        assert_eq!(
            keys,
            vec![
                "brc20",
                "inscription",
                "runes",
                "stamps",
                "counterparty",
                "omni",
                "op_return_other",
                "none",
                "unknown",
            ]
        );
    }

    #[test]
    fn missing_transaction_yields_unknown_status_without_verdict() {
        let result = DataProtocolFingerprints.classify(&ClassificationInput {
            txid: TXID,
            wtxid: None,
            transaction: None,
        });
        assert_eq!(result.status, ClassificationStatus::Unknown);
        assert_eq!(result.verdict, None);
        assert_eq!(result.evidence, json!({ "missing": "raw_transaction" }));
    }

    #[test]
    fn a_plain_payment_transaction_is_positively_none() {
        let transaction = transaction_with(
            2,
            vec![
                output(90_000, p2wpkh_script()),
                output(9_000, p2tr_script()),
            ],
        );
        let result = classify(&transaction);
        assert_eq!(result.status, ClassificationStatus::Complete);
        assert_eq!(result.verdict.as_deref(), Some("none"));
        assert_eq!(
            result.evidence,
            json!({ "detections": [], "unmatched_structures": [] })
        );
    }

    #[test]
    fn a_runestone_op_return_is_runes() {
        let mut script = vec![0x6a, 0x5d];
        push(&mut script, &[0xaa; 12]);
        let transaction = transaction_with(
            1,
            vec![
                output(0, ScriptBuf::from_bytes(script)),
                output(50_000, p2wpkh_script()),
            ],
        );
        let result = classify(&transaction);
        assert_eq!(result.status, ClassificationStatus::Complete);
        assert_eq!(result.verdict.as_deref(), Some("runes"));
        assert_eq!(
            result.evidence["detections"],
            json!([{
                "protocol": "runes",
                "output": 0,
                "detection": "op_return_op_13",
                "payload_len": 13,
            }])
        );
    }

    #[test]
    fn a_bare_multisig_with_implausible_keys_is_stamps() {
        let script = bare_multisig_script(&[
            data_key(0x00, 0xd1),
            data_key(0xff, 0xd2),
            data_key(0x02, 0x77),
        ]);
        let transaction = transaction_with(
            1,
            vec![output(546, script), output(50_000, p2wpkh_script())],
        );
        let result = classify(&transaction);
        assert_eq!(result.status, ClassificationStatus::Complete);
        assert_eq!(result.verdict.as_deref(), Some("stamps"));
        assert_eq!(
            result.evidence["detections"],
            json!([{
                "protocol": "stamps",
                "output": 0,
                "detection": "bare_multisig_data_keys",
                "required": 1,
                "key_count": 3,
                "implausible_key_count": 2,
            }])
        );
    }

    #[test]
    fn a_plausible_bare_multisig_is_none_with_the_structure_recorded() {
        let script = bare_multisig_script(&[data_key(0x02, 0x71), data_key(0x03, 0x72)]);
        let transaction = transaction_with(1, vec![output(546, script)]);
        let result = classify(&transaction);
        assert_eq!(result.verdict.as_deref(), Some("none"));
        assert_eq!(
            result.evidence["unmatched_structures"],
            json!([{ "output": 0, "structure": "bare_multisig" }])
        );
    }

    #[test]
    fn a_plaintext_cntrprty_op_return_is_counterparty() {
        let mut payload = b"CNTRPRTY".to_vec();
        payload.extend_from_slice(&[0x00, 0x00, 0x00, 0x14, 0x99]);
        let mut script = vec![0x6a];
        push(&mut script, &payload);
        let transaction = transaction_with(
            1,
            vec![
                output(0, ScriptBuf::from_bytes(script)),
                output(40_000, p2wpkh_script()),
            ],
        );
        let result = classify(&transaction);
        assert_eq!(result.verdict.as_deref(), Some("counterparty"));
        assert_eq!(
            result.evidence["detections"],
            json!([{
                "protocol": "counterparty",
                "output": 0,
                "carrier": "op_return",
                "encoding": "plaintext",
            }])
        );
    }

    #[test]
    fn an_arc4_cntrprty_op_return_matches_on_the_display_key_order() {
        let prevout_bytes: [u8; 32] =
            std::array::from_fn(|index| u8::try_from(index).expect("index fits") ^ 0x5a);
        // Real implementations key on the reversed-hex (display) txid bytes.
        let mut display_key = prevout_bytes;
        display_key.reverse();
        let mut plaintext = b"CNTRPRTY".to_vec();
        plaintext.extend_from_slice(&[0x00, 0x00, 0x00, 0x02, 0x42]);
        let payload = arc4(&display_key, &plaintext);
        assert!(!payload.starts_with(b"CNTRPRTY"), "must be encrypted");
        let mut script = vec![0x6a];
        push(&mut script, &payload);
        let transaction = transaction_from(
            vec![input_with_prevout(prevout_bytes)],
            vec![
                output(0, ScriptBuf::from_bytes(script)),
                output(40_000, p2wpkh_script()),
            ],
        );
        let result = classify(&transaction);
        assert_eq!(result.verdict.as_deref(), Some("counterparty"));
        assert_eq!(
            result.evidence["detections"],
            json!([{
                "protocol": "counterparty",
                "output": 0,
                "carrier": "op_return",
                "encoding": "arc4",
                "arc4_key_order": "display",
            }])
        );
    }

    #[test]
    fn an_arc4_cntrprty_op_return_keyed_internally_is_recorded_as_internal() {
        let prevout_bytes: [u8; 32] =
            std::array::from_fn(|index| u8::try_from(index).expect("index fits") ^ 0xc3);
        let payload = arc4(&prevout_bytes, b"CNTRPRTYtail");
        let mut script = vec![0x6a];
        push(&mut script, &payload);
        let transaction = transaction_from(
            vec![input_with_prevout(prevout_bytes)],
            vec![output(0, ScriptBuf::from_bytes(script))],
        );
        let result = classify(&transaction);
        assert_eq!(result.verdict.as_deref(), Some("counterparty"));
        assert_eq!(
            result.evidence["detections"][0]["arc4_key_order"],
            json!("internal")
        );
    }

    #[test]
    fn an_arc4_cntrprty_bare_multisig_is_counterparty() {
        let prevout_bytes = [0x1b_u8; 32];
        let mut display_key = prevout_bytes;
        display_key.reverse();
        // Counterparty multisig data: a length byte, then the encrypted
        // CNTRPRTY-prefixed payload, spread over the data keys' middle
        // bytes (each key contributes key[1..32]).
        let mut plaintext = b"CNTRPRTY".to_vec();
        plaintext.extend_from_slice(&[0x00; 22]);
        let mut data = vec![0x1e];
        data.extend_from_slice(&arc4(&display_key, &plaintext));
        assert_eq!(data.len(), 31);
        let mut data_key_bytes = vec![0x02];
        data_key_bytes.extend_from_slice(&data);
        data_key_bytes.push(0x00);
        let script = bare_multisig_script(&[data_key_bytes, data_key(0x03, 0x66)]);
        let transaction = transaction_from(
            vec![input_with_prevout(prevout_bytes)],
            vec![output(546, script)],
        );
        let result = classify(&transaction);
        assert_eq!(result.verdict.as_deref(), Some("counterparty"));
        assert_eq!(
            result.evidence["detections"],
            json!([{
                "protocol": "counterparty",
                "output": 0,
                "carrier": "bare_multisig",
                "encoding": "arc4",
                "arc4_key_order": "display",
                "data_offset": 1,
            }])
        );
    }

    #[test]
    fn an_omni_op_return_is_omni() {
        let mut payload = b"omni".to_vec();
        payload.extend_from_slice(&[0x00, 0x00, 0x00, 0x32]);
        let mut script = vec![0x6a];
        push(&mut script, &payload);
        let transaction = transaction_with(
            1,
            vec![
                output(0, ScriptBuf::from_bytes(script)),
                output(40_000, p2wpkh_script()),
            ],
        );
        let result = classify(&transaction);
        assert_eq!(result.verdict.as_deref(), Some("omni"));
        assert_eq!(
            result.evidence["detections"],
            json!([{
                "protocol": "omni",
                "output": 0,
                "carrier": "op_return",
                "payload_len": 8,
            }])
        );
    }

    #[test]
    fn an_unrecognized_op_return_is_op_return_other_with_a_capped_head() {
        let transaction = transaction_with(
            1,
            vec![output(0, op_return_script()), output(900, p2wpkh_script())],
        );
        let result = classify(&transaction);
        assert_eq!(result.verdict.as_deref(), Some("op_return_other"));
        assert_eq!(
            result.evidence["detections"],
            json!([{
                "protocol": "op_return_other",
                "output": 0,
                "payload_len": 5,
                "payload_head_hex": "61746c6173",
            }])
        );

        let mut script = vec![0x6a];
        push(&mut script, &[0xee; 40]);
        let long = transaction_with(1, vec![output(0, ScriptBuf::from_bytes(script))]);
        let result = classify(&long);
        assert_eq!(result.verdict.as_deref(), Some("op_return_other"));
        assert_eq!(
            result.evidence["detections"][0]["payload_head_hex"],
            json!("ee".repeat(16))
        );
        assert_eq!(result.evidence["detections"][0]["payload_len"], json!(40));
    }

    #[test]
    fn a_decoded_envelope_inscription_is_high_confidence() {
        let transaction =
            script_path_transaction(envelope_tapscript(b"image/png", &[0x89, 0x50, 0x4e, 0x47]));
        let result = classify(&transaction);
        assert_eq!(result.status, ClassificationStatus::Complete);
        assert_eq!(result.verdict.as_deref(), Some("inscription"));
        assert_eq!(
            result.evidence["detections"],
            json!([{
                "protocol": "inscription",
                "input": 0,
                "detection": "tapscript_envelope",
                "content_type": "image/png",
                "body_len": 4,
                "envelope_complete": true,
            }])
        );
    }

    #[test]
    fn a_brc20_envelope_takes_precedence_over_plain_inscription() {
        for content_type in [b"application/json".as_slice(), b"text/plain;charset=utf-8"] {
            let body = br#"{ "p" : "brc-20", "op": "transfer", "tick": "atls", "amt": "5" }"#;
            let transaction = script_path_transaction(envelope_tapscript(content_type, body));
            let result = classify(&transaction);
            assert_eq!(
                result.verdict.as_deref(),
                Some("brc20"),
                "content type {}",
                String::from_utf8_lossy(content_type),
            );
            assert_eq!(
                result.evidence["detections"][0]["payload_check"],
                json!("whitespace_stripped_substring")
            );
        }
    }

    #[test]
    fn a_brc20_body_under_a_non_json_content_type_stays_inscription() {
        let body = br#"{"p":"brc-20","op":"mint"}"#;
        let transaction = script_path_transaction(envelope_tapscript(b"image/png", body));
        assert_eq!(verdict(&transaction), "inscription");
    }

    #[test]
    fn the_byte_pattern_without_a_script_path_shape_is_a_low_confidence_inscription() {
        let mut element = vec![0xaa, 0xbb];
        element.extend_from_slice(&ORD_ENVELOPE_BYTES);
        element.push(0xcc);
        let transaction = transaction_from(
            vec![witness_input(&[element])],
            vec![output(546, p2tr_script())],
        );
        let result = classify(&transaction);
        assert_eq!(result.verdict.as_deref(), Some("inscription"));
        assert_eq!(
            result.evidence["detections"],
            json!([{
                "protocol": "inscription",
                "input": 0,
                "detection": "witness_byte_pattern",
                "confidence": "low",
            }])
        );
    }

    #[test]
    fn an_envelope_less_tapscript_records_the_structure_and_stays_none() {
        // `<key> OP_CHECKSIG` alone: script-path shaped, no envelope.
        let mut tapscript = vec![0x20];
        tapscript.extend_from_slice(&[0x07; 32]);
        tapscript.push(0xac);
        let transaction = script_path_transaction(tapscript);
        let result = classify(&transaction);
        assert_eq!(result.verdict.as_deref(), Some("none"));
        assert_eq!(
            result.evidence["unmatched_structures"],
            json!([{ "input": 0, "structure": "taproot_script_path" }])
        );
    }

    #[test]
    fn multi_carrier_transactions_take_the_highest_precedence_verdict() {
        // An inscription envelope plus a runestone output: inscription
        // outranks runes, and both detections stay in evidence.
        let mut runestone = vec![0x6a, 0x5d];
        push(&mut runestone, &[0x14; 8]);
        let transaction = transaction_from(
            vec![witness_input(&[
                vec![0x01; 64],
                envelope_tapscript(b"image/png", &[0x42; 6]),
                control_block(0),
            ])],
            vec![
                output(0, ScriptBuf::from_bytes(runestone.clone())),
                output(546, p2tr_script()),
            ],
        );
        let result = classify(&transaction);
        assert_eq!(result.verdict.as_deref(), Some("inscription"));
        let detections = result.evidence["detections"]
            .as_array()
            .expect("detections array");
        assert_eq!(detections.len(), 2);
        assert_eq!(detections[0]["protocol"], json!("inscription"));
        assert_eq!(detections[1]["protocol"], json!("runes"));

        // With a BRC-20 envelope the verdict climbs to brc20.
        let body = br#"{"p":"brc-20","op":"transfer"}"#;
        let transaction = transaction_from(
            vec![witness_input(&[
                vec![0x01; 64],
                envelope_tapscript(b"application/json", body),
                control_block(0),
            ])],
            vec![output(0, ScriptBuf::from_bytes(runestone))],
        );
        assert_eq!(verdict(&transaction), "brc20");
    }

    #[test]
    fn arc4_round_trips_and_matches_the_known_vector() {
        // The classic RC4 test vector: key "Key", plaintext "Plaintext".
        let ciphertext = arc4(b"Key", b"Plaintext");
        assert_eq!(
            hex_string(&ciphertext),
            "bbf316e8d940af0ad3",
            "RC4 KSA/PRGA must match the published vector"
        );
        assert_eq!(arc4(b"Key", &ciphertext), b"Plaintext");
    }
}
