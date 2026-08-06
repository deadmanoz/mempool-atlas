//! Rules 2-7: the per-input, script-scoped rules.
//!
//! Each supported input is inspected for every independently provable RDTS
//! violation. Matches are returned in the enforcement client's check order so
//! the orchestrator can preserve the deterministic first rejection while also
//! exposing later matches in rule detail. Grandfathering is applied by the
//! orchestrator, not here.
//!
//! Scope and faithfulness notes:
//! - Native witness-program spends (v0 P2WSH/P2WPKH, v1 Taproot, P2A) and
//!   legacy/bare spends are modeled.
//! - P2SH spends mirror the client's ordered stack handling: non-redeem
//!   scriptSig items, redeemScript execution, then nested witness dispatch.
//! - The Taproot commitment (BIP341 tweak) is not recomputed. The client
//!   rejects a bad commitment with `WITNESS_PROGRAM_MISMATCH` before reaching
//!   the RDTS sub-rules. This evaluator assumes an otherwise-valid spend.

use crate::bip110::context::{
    MAX_SCRIPT_ELEMENT_SIZE_REDUCED, TAPROOT_CONTROL_BASE_SIZE, TAPROOT_CONTROL_MAX_SIZE_REDUCED,
    TAPROOT_CONTROL_NODE_SIZE, TAPROOT_LEAF_TAPSCRIPT,
};
use crate::bip110::script_scan::{first_tapscript_op_if, op_successes, pushes_over};
use crate::bip110::verdict::{PrimaryViolation, RuleId, ScriptKind, Violation};
use crate::bip110::witness::{
    TaprootScriptPath, TaprootSpend, decompose_taproot, parse_witness_program,
};
use bitcoin::script::Instruction;
use bitcoin::{Script, TxIn};

/// Evaluate rules 2-7 for one non-coinbase input and spent scriptPubKey.
pub fn evaluate_input(input_index: usize, txin: &TxIn, spk: &Script) -> Vec<PrimaryViolation> {
    let items: Vec<&[u8]> = txin.witness.iter().collect();

    // P2SH is a distinct dispatch in the client. The initial scriptSig
    // evaluation clears SCRIPT_VERIFY_REDUCED_DATA, the last resulting stack
    // item is popped as the redeemScript, and only the remaining items are
    // checked against the reduced limit. The redeemScript is then executed
    // with the reduced flag restored before an exact nested-witness dispatch.
    // Deployed client: interpreter.cpp:2033-2035,2086-2117.
    if spk.is_p2sh() {
        return evaluate_p2sh_spend(input_index, txin, &items);
    }

    match parse_witness_program(spk) {
        Some(wp) => evaluate_witness_spend(
            input_index,
            &wp.program,
            wp.version,
            &items,
            /* is_p2sh= */ false,
        ),
        None => evaluate_legacy_spend(input_index, txin, spk),
    }
}

/// Evaluate a P2SH spend in the client's RDTS check order.
///
/// The evaluator's public contract assumes a transaction already admitted to
/// a Core mempool. Consequently the P2SH hash, push-only property, and ordinary
/// script validity have already succeeded. Defensive parse failures here mean
/// no RDTS-specific claim, rather than an invented unknown fact.
fn evaluate_p2sh_spend(
    input_index: usize,
    txin: &TxIn,
    witness_items: &[&[u8]],
) -> Vec<PrimaryViolation> {
    let mut instructions = Vec::new();
    for decoded in txin.script_sig.instruction_indices() {
        let Ok((opcode_pos, instruction)) = decoded else {
            return Vec::new();
        };
        if !is_push_only(instruction) {
            return Vec::new();
        }
        instructions.push((opcode_pos, instruction));
    }

    let Some((_, redeem_instruction)) = instructions.last().copied() else {
        return Vec::new();
    };

    // The client checks the scriptSig stack in push order after popping the
    // final redeemScript item. For push-only scripts this is exactly the
    // non-final instruction order. Small-integer opcodes create at most one
    // byte and therefore cannot violate Rule 2.
    let mut matches = instructions[..instructions.len() - 1]
        .iter()
        .filter_map(|(opcode_pos, instruction)| match instruction {
            Instruction::PushBytes(bytes) if bytes.len() > MAX_SCRIPT_ELEMENT_SIZE_REDUCED => Some(
                push_match(input_index, ScriptKind::ScriptSig, *opcode_pos, bytes.len()),
            ),
            _ => None,
        })
        .collect::<Vec<_>>();

    // An admitted P2SH spend's final stack item is normally a byte push. A
    // small-integer final item yields an ordinarily-invalid redeemScript, but
    // any preceding RDTS matches above still occurred before that failure.
    let Instruction::PushBytes(redeem_script) = redeem_instruction else {
        return matches;
    };
    let redeem_script = redeem_script.as_bytes();

    // The redeemScript blob itself is exempt, but every push decoded while
    // executing it uses the reduced 256-byte limit, even in an unexecuted
    // conditional branch.
    matches.extend(push_violations(
        input_index,
        ScriptKind::RedeemScript,
        redeem_script,
    ));

    // Nested witness execution is reached only when scriptSig is exactly the
    // canonical single push of the redeemScript. This mirrors the client's
    // WITNESS_MALLEATED_P2SH gate and prevents attributing nested RDTS rules to
    // an ordinarily-invalid spend.
    if instructions.len() == 1
        && is_canonical_single_push(txin.script_sig.as_bytes(), redeem_script)
        && let Some(wp) = parse_witness_program(Script::from_bytes(redeem_script))
    {
        matches.extend(evaluate_witness_spend(
            input_index,
            &wp.program,
            wp.version,
            witness_items,
            /* is_p2sh= */ true,
        ));
    }

    matches
}

fn is_push_only(instruction: Instruction<'_>) -> bool {
    match instruction {
        Instruction::PushBytes(_) => true,
        // EvalScript's push-only stack-producing numeric opcodes. OP_RESERVED
        // (0x50) is deliberately excluded because execution fails there.
        Instruction::Op(opcode) => matches!(opcode.to_u8(), 0x4f | 0x51..=0x60),
    }
}

/// Match `CScript() << vector(redeemScript)` byte-for-byte without allocating.
fn is_canonical_single_push(script_sig: &[u8], redeem_script: &[u8]) -> bool {
    let len = redeem_script.len();
    let header_len = if len < 0x4c {
        1
    } else if len <= u8::MAX as usize {
        2
    } else if len <= u16::MAX as usize {
        3
    } else if len <= u32::MAX as usize {
        5
    } else {
        return false;
    };
    if script_sig.len() != header_len + len {
        return false;
    }

    let header_matches = match header_len {
        1 => script_sig[0] == len as u8,
        2 => script_sig[..2] == [0x4c, len as u8],
        3 => script_sig[..3] == [0x4d, len as u8, (len >> 8) as u8],
        5 => {
            script_sig[..5]
                == [
                    0x4e,
                    len as u8,
                    (len >> 8) as u8,
                    (len >> 16) as u8,
                    (len >> 24) as u8,
                ]
        }
        _ => unreachable!("known canonical push header"),
    };
    header_matches && script_sig[header_len..] == redeem_script[..]
}

fn evaluate_witness_spend(
    input_index: usize,
    program: &[u8],
    version: u8,
    items: &[&[u8]],
    is_p2sh: bool,
) -> Vec<PrimaryViolation> {
    match (version, program.len(), is_p2sh) {
        // BIP141 P2WSH.
        (0, 32, _) => evaluate_p2wsh(input_index, items),
        // BIP141 P2WPKH: both witness items are script arguments.
        (0, 20, _) => witness_item_violations(input_index, items),
        // Wrong-length v0 is a pre-existing failure, not an RDTS rule.
        (0, _, _) => Vec::new(),
        // Native BIP341 Taproot.
        (1, 32, false) => evaluate_taproot(input_index, items),
        // P2A is recognized only with an empty witness.
        (1, 2, false) if program == [0x4e, 0x73] => {
            if items.is_empty() {
                Vec::new()
            } else {
                vec![rule_match(
                    RuleId::UndefinedVersion,
                    Violation::UndefinedWitnessVersion {
                        input: input_index,
                        version: 1,
                        p2sh_wrapped: false,
                        p2a_non_empty_witness: true,
                    },
                )]
            }
        }
        // A P2SH-wrapped v1 program is deliberately not Taproot or P2A in the
        // client. It falls through with every v2..=v16 program to the
        // upgradable-witness rejection.
        _ => vec![rule_match(
            RuleId::UndefinedVersion,
            Violation::UndefinedWitnessVersion {
                input: input_index,
                version,
                p2sh_wrapped: is_p2sh,
                p2a_non_empty_witness: false,
            },
        )],
    }
}

fn evaluate_p2wsh(input_index: usize, items: &[&[u8]]) -> Vec<PrimaryViolation> {
    if items.is_empty() {
        // Pre-existing WITNESS_PROGRAM_WITNESS_EMPTY.
        return Vec::new();
    }

    let (script, args) = items.split_last().expect("non-empty witness");
    let mut matches = witness_item_violations(input_index, args);
    matches.extend(push_violations(
        input_index,
        ScriptKind::WitnessScript,
        script,
    ));
    matches
}

/// Taproot client order is annex, control size, commitment, leaf-version
/// dispatch, then the Tapscript checks.
fn evaluate_taproot(input_index: usize, items: &[&[u8]]) -> Vec<PrimaryViolation> {
    match decompose_taproot(items) {
        TaprootSpend::Empty => Vec::new(),
        TaprootSpend::KeyPath { annex } => annex
            .map(|annex| {
                vec![rule_match(
                    RuleId::TaprootAnnex,
                    Violation::TaprootAnnexPresent {
                        input: input_index,
                        annex_len: annex.len(),
                    },
                )]
            })
            .unwrap_or_default(),
        TaprootSpend::ScriptPath(sp) => evaluate_taproot_script_path(input_index, &sp),
    }
}

fn evaluate_taproot_script_path(
    input_index: usize,
    sp: &TaprootScriptPath<'_>,
) -> Vec<PrimaryViolation> {
    let mut matches = Vec::new();

    if let Some(annex) = sp.annex {
        matches.push(rule_match(
            RuleId::TaprootAnnex,
            Violation::TaprootAnnexPresent {
                input: input_index,
                annex_len: annex.len(),
            },
        ));
    }

    // Only an aligned control block is meaningful for RDTS attribution.
    // Malformed controls already fail the pre-existing Taproot size rule.
    let clen = sp.control.len();
    let aligned = clen >= TAPROOT_CONTROL_BASE_SIZE
        && (clen - TAPROOT_CONTROL_BASE_SIZE).is_multiple_of(TAPROOT_CONTROL_NODE_SIZE);
    if !aligned {
        return matches;
    }

    if clen > TAPROOT_CONTROL_MAX_SIZE_REDUCED {
        matches.push(rule_match(
            RuleId::ControlBlockSize,
            Violation::ControlBlockTooLarge {
                input: input_index,
                len: clen,
                depth: Some((clen - TAPROOT_CONTROL_BASE_SIZE) / TAPROOT_CONTROL_NODE_SIZE),
                limit: TAPROOT_CONTROL_MAX_SIZE_REDUCED,
            },
        ));
    }

    let leaf_version = sp
        .leaf_version()
        .expect("aligned control block has a leaf byte");
    if leaf_version != TAPROOT_LEAF_TAPSCRIPT {
        matches.push(rule_match(
            RuleId::UndefinedVersion,
            Violation::UndefinedTapleafVersion {
                input: input_index,
                leaf_version,
            },
        ));
        return matches;
    }

    // OP_SUCCESS is a whole-script pre-pass and precedes every remaining
    // Tapscript check. Record each opcode, with the first remaining primary.
    let op_successes = op_successes(sp.script);
    let has_op_success = !op_successes.is_empty();
    matches.extend(op_successes.into_iter().map(|(opcode_pos, opcode)| {
        rule_match(
            RuleId::OpSuccess,
            Violation::OpSuccessPresent {
                input: input_index,
                opcode,
                opcode_pos,
            },
        )
    }));

    // Initial script-argument stack items are checked before EvalScript.
    matches.extend(witness_item_violations(input_index, &sp.args));

    // During EvalScript, oversized pushes and the first provably executed
    // OP_IF/OP_NOTIF are ordered by opcode position. Any OP_SUCCESS makes the
    // Tapscript pre-pass return before execution, so no conditional executes.
    let mut script_matches: Vec<(usize, PrimaryViolation)> =
        push_violations(input_index, ScriptKind::Tapscript, sp.script)
            .into_iter()
            .map(|m| (violation_opcode_pos(&m.evidence), m))
            .collect();
    if !has_op_success && let Some((opcode_pos, opcode)) = first_tapscript_op_if(sp.script) {
        script_matches.push((
            opcode_pos,
            rule_match(
                RuleId::TapscriptOpIf,
                Violation::TapscriptOpIf {
                    input: input_index,
                    opcode,
                    opcode_pos,
                },
            ),
        ));
    }
    script_matches.sort_by_key(|(opcode_pos, _)| *opcode_pos);
    matches.extend(script_matches.into_iter().map(|(_, m)| m));

    matches
}

fn evaluate_legacy_spend(input_index: usize, txin: &TxIn, spk: &Script) -> Vec<PrimaryViolation> {
    let mut matches = push_violations(
        input_index,
        ScriptKind::ScriptSig,
        txin.script_sig.as_bytes(),
    );
    matches.extend(push_violations(
        input_index,
        ScriptKind::ScriptPubKey,
        spk.as_bytes(),
    ));
    matches
}

fn witness_item_violations(input_index: usize, items: &[&[u8]]) -> Vec<PrimaryViolation> {
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.len() > MAX_SCRIPT_ELEMENT_SIZE_REDUCED)
        .map(|(item_index, item)| {
            rule_match(
                RuleId::ElementSize,
                Violation::WitnessItemTooLarge {
                    input: input_index,
                    item_index,
                    len: item.len(),
                    limit: MAX_SCRIPT_ELEMENT_SIZE_REDUCED,
                },
            )
        })
        .collect()
}

fn push_violations(
    input_index: usize,
    script_kind: ScriptKind,
    script: &[u8],
) -> Vec<PrimaryViolation> {
    pushes_over(script, MAX_SCRIPT_ELEMENT_SIZE_REDUCED)
        .into_iter()
        .map(|(opcode_pos, len)| push_match(input_index, script_kind, opcode_pos, len))
        .collect()
}

fn push_match(
    input_index: usize,
    script_kind: ScriptKind,
    opcode_pos: usize,
    len: usize,
) -> PrimaryViolation {
    rule_match(
        RuleId::ElementSize,
        Violation::PushTooLarge {
            input: input_index,
            script: script_kind,
            opcode_pos,
            len,
            limit: MAX_SCRIPT_ELEMENT_SIZE_REDUCED,
        },
    )
}

fn rule_match(rule: RuleId, evidence: Violation) -> PrimaryViolation {
    PrimaryViolation { rule, evidence }
}

fn violation_opcode_pos(violation: &Violation) -> usize {
    match violation {
        Violation::PushTooLarge { opcode_pos, .. }
        | Violation::OpSuccessPresent { opcode_pos, .. }
        | Violation::TapscriptOpIf { opcode_pos, .. } => *opcode_pos,
        _ => unreachable!("only opcode-position violations are sorted"),
    }
}
