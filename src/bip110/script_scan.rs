//! Static opcode/push scanning helpers used by rules 2, 6, and 7.
//!
//! These decode a script into opcodes exactly as the client's `GetOp` does:
//! data pushes are consumed as data (their payload bytes are never re-parsed as
//! opcodes), so an `OP_IF` or `OP_SUCCESS` byte that is merely *pushed as data*
//! is correctly ignored. Decoding stops at a malformed trailing push, which the
//! client would reject as `SCRIPT_ERR_BAD_OPCODE` before reaching any later
//! opcode.

use bitcoin::Script;
use bitcoin::opcodes::all::{OP_IF, OP_NOTIF};
use bitcoin::script::Instruction;

/// The `OP_SUCCESSx` opcode set, byte-for-byte from the client.
///
/// Client: `src/script/script.cpp:435` `IsOpSuccess`.
pub fn is_op_success(opcode: u8) -> bool {
    opcode == 80
        || opcode == 98
        || (126..=129).contains(&opcode)
        || (131..=134).contains(&opcode)
        || (137..=138).contains(&opcode)
        || (141..=142).contains(&opcode)
        || (149..=153).contains(&opcode)
        || (187..=254).contains(&opcode)
}

/// Every pushed data element in `script` whose payload exceeds `limit`, as
/// `(opcode_pos, len)` pairs. Used for the in-script side of rule 2.
///
/// Client: the `max_element_size` gate in `EvalScript`
/// (`src/script/interpreter.cpp:436-449`) is applied immediately after decoding
/// each push, before conditional execution gating.
pub fn pushes_over(script: &[u8], limit: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for item in Script::from_bytes(script).instruction_indices() {
        match item {
            Ok((pos, Instruction::PushBytes(pb))) => {
                if pb.len() > limit {
                    out.push((pos, pb.len()));
                }
            }
            Ok((_, Instruction::Op(_))) => {}
            // Malformed trailing push: stop, matching the client's GetOp halt.
            Err(_) => break,
        }
    }
    out
}

/// Every `OP_SUCCESS` opcode in a Tapscript, as `(opcode_pos, opcode)`.
///
/// Client: the OP_SUCCESS pre-pass in `ExecuteWitnessScript`
/// (`src/script/interpreter.cpp:1846-1861`), which scans every opcode
/// regardless of conditional execution ("even if execution would not reach
/// it").
pub fn op_successes(script: &[u8]) -> Vec<(usize, u8)> {
    let mut out = Vec::new();
    for item in Script::from_bytes(script).instruction_indices() {
        match item {
            Ok((pos, Instruction::Op(op))) => {
                let byte = op.to_u8();
                if is_op_success(byte) {
                    out.push((pos, byte));
                }
            }
            Ok((_, Instruction::PushBytes(_))) => {}
            Err(_) => break,
        }
    }
    out
}

/// The first `OP_IF`/`OP_NOTIF` opcode in a Tapscript, as `(opcode_pos,
/// opcode)`. Rule 7.
///
/// Client: the Tapscript branch of the `OP_IF`/`OP_NOTIF` case
/// (`src/script/interpreter.cpp:621-625`). The earliest `OP_IF`/`OP_NOTIF` in
/// the opcode stream is always reached with `fExec` true (nothing opens a
/// conditional branch before it), so the presence of the opcode is equivalent
/// to it being executed. A `0x63`/`0x64` byte inside pushed data is not an
/// opcode and is correctly ignored.
pub fn first_tapscript_op_if(script: &[u8]) -> Option<(usize, u8)> {
    for item in Script::from_bytes(script).instruction_indices() {
        match item {
            Ok((pos, Instruction::Op(op))) => {
                if op == OP_IF || op == OP_NOTIF {
                    return Some((pos, op.to_u8()));
                }
            }
            Ok((_, Instruction::PushBytes(_))) => {}
            Err(_) => break,
        }
    }
    None
}
