//! Low-level parsing of witness programs and Taproot witness stacks, mirroring
//! the enforcement client so the rule evaluation matches its exact boundaries.

use crate::bip110::context::{ANNEX_TAG, TAPROOT_LEAF_MASK};
use bitcoin::Script;

const OP_0: u8 = 0x00;
const OP_1: u8 = 0x51;
const OP_16: u8 = 0x60;

/// A decoded witness program: `(version, program_bytes)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitnessProgram {
    /// Witness version (0-16).
    pub version: u8,
    /// Program bytes (2-40).
    pub program: Vec<u8>,
}

/// Parse a scriptPubKey as a witness program, byte-for-byte as the client does.
///
/// Client: `src/script/script.cpp:244` `CScript::IsWitnessProgram`. Size must be
/// 4..=42, the first byte must be `OP_0` or `OP_1..=OP_16`, and the second byte
/// (the push length) must equal `size - 2`.
pub fn parse_witness_program(spk: &Script) -> Option<WitnessProgram> {
    let bytes = spk.as_bytes();
    if bytes.len() < 4 || bytes.len() > 42 {
        return None;
    }
    let op = bytes[0];
    if op != OP_0 && !(OP_1..=OP_16).contains(&op) {
        return None;
    }
    // `bytes[1]` is the push-length opcode; it must push exactly `size - 2`.
    if usize::from(bytes[1]) + 2 != bytes.len() {
        return None;
    }
    let version = if op == OP_0 { 0 } else { op - (OP_1 - 1) };
    Some(WitnessProgram {
        version,
        program: bytes[2..].to_vec(),
    })
}

/// The role each witness element plays in a Taproot script-path spend, after
/// the client's ordered popping: optional annex (last), control block, script,
/// and the remaining leading elements as script arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaprootScriptPath<'a> {
    /// Script-argument elements (everything before the script). Subject to the
    /// rule 2 item-size limit.
    pub args: Vec<&'a [u8]>,
    /// The revealed leaf script.
    pub script: &'a [u8],
    /// The control block.
    pub control: &'a [u8],
    /// The annex, if present (rule 4).
    pub annex: Option<&'a [u8]>,
}

impl TaprootScriptPath<'_> {
    /// The masked leaf version (`control[0] & 0xfe`). `None` if the control
    /// block is empty (structurally malformed; handled by the size rule).
    pub fn leaf_version(&self) -> Option<u8> {
        self.control.first().map(|b| b & TAPROOT_LEAF_MASK)
    }
}

/// How a Taproot (witness v1, 32-byte program) spend decomposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaprootSpend<'a> {
    /// Empty witness stack: structurally invalid for reasons outside RDTS
    /// (`SCRIPT_ERR_WITNESS_PROGRAM_WITNESS_EMPTY`). Rule 4 still needs to know
    /// there is no annex.
    Empty,
    /// Key-path spend: a single element (the signature) remains after removing
    /// the optional annex.
    KeyPath {
        /// The annex, if present (rule 4).
        annex: Option<&'a [u8]>,
    },
    /// Script-path spend.
    ScriptPath(TaprootScriptPath<'a>),
}

/// Decompose a Taproot witness stack the way the client does in
/// `VerifyWitnessProgram` (`src/script/interpreter.cpp:1957-2003`):
///
/// 1. Empty stack is rejected (outside RDTS).
/// 2. If there are `>= 2` items and the last is non-empty starting with the
///    annex tag `0x50`, the last item is the annex and is removed.
/// 3. One remaining item is a key-path spend; more than one is a script-path
///    spend with `control` last and `script` second-to-last.
pub fn decompose_taproot<'a>(items: &[&'a [u8]]) -> TaprootSpend<'a> {
    if items.is_empty() {
        return TaprootSpend::Empty;
    }
    // Annex detection: needs at least two items so that at least one non-annex
    // element remains (client: `stack.size() >= 2`).
    let (stack, annex): (&[&'a [u8]], Option<&'a [u8]>) = {
        let last = *items.last().unwrap();
        if items.len() >= 2 && !last.is_empty() && last[0] == ANNEX_TAG {
            (&items[..items.len() - 1], Some(last))
        } else {
            (items, None)
        }
    };

    if stack.len() == 1 {
        return TaprootSpend::KeyPath { annex };
    }
    // Script path: control is last, script is second-to-last, the rest are args.
    let control = stack[stack.len() - 1];
    let script = stack[stack.len() - 2];
    let args = stack[..stack.len() - 2].to_vec();
    TaprootSpend::ScriptPath(TaprootScriptPath {
        args,
        script,
        control,
        annex,
    })
}
