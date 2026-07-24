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
//! - P2SH-wrapped spends are a `VERIFY(client)` open item and yield
//!   `InputEval::Unsupported` rather than a guessed verdict.
//! - The Taproot commitment (BIP341 tweak) is not recomputed. The client
//!   rejects a bad commitment with `WITNESS_PROGRAM_MISMATCH` before reaching
//!   the RDTS sub-rules. This evaluator assumes an otherwise-valid spend.

use crate::context::{
    MAX_SCRIPT_ELEMENT_SIZE_REDUCED, TAPROOT_CONTROL_BASE_SIZE, TAPROOT_CONTROL_MAX_SIZE_REDUCED,
    TAPROOT_CONTROL_NODE_SIZE, TAPROOT_LEAF_TAPSCRIPT,
};
use crate::script_scan::{first_tapscript_op_if, op_successes, pushes_over};
use crate::verdict::{PrimaryViolation, RuleId, ScriptKind, Violation};
use crate::witness::{TaprootScriptPath, TaprootSpend, decompose_taproot, parse_witness_program};
use bitcoin::{Script, TxIn};

/// Raw per-input evaluation for rules 2-7, before applicability filtering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEval {
    /// The supported input was fully classified. The vector is empty when it
    /// has no RDTS match and otherwise follows client check order.
    Evaluated(Vec<PrimaryViolation>),
    /// Spend form recognized but not modeled (P2SH-wrapped).
    Unsupported(&'static str),
}

/// Evaluate rules 2-7 for one non-coinbase input and spent scriptPubKey.
pub fn evaluate_input(input_index: usize, txin: &TxIn, spk: &Script) -> InputEval {
    let items: Vec<&[u8]> = txin.witness.iter().collect();

    // P2SH is a distinct dispatch in the client. It exempts the redeemScript
    // push, re-checks the other scriptSig elements, and may dispatch a nested
    // witness program. Until that stack model has oracle coverage, refuse to
    // guess. Deployed client: interpreter.cpp:2033-2035,2090-2096,2107-2117.
    if spk.is_p2sh() {
        return InputEval::Unsupported("p2sh_wrapped_spend_not_modeled");
    }

    match parse_witness_program(spk) {
        Some(wp) => evaluate_witness_spend(input_index, &wp.program, wp.version, &items),
        None => evaluate_legacy_spend(input_index, txin, spk),
    }
}

fn evaluate_witness_spend(
    input_index: usize,
    program: &[u8],
    version: u8,
    items: &[&[u8]],
) -> InputEval {
    match (version, program.len()) {
        // BIP141 P2WSH.
        (0, 32) => evaluate_p2wsh(input_index, items),
        // BIP141 P2WPKH: both witness items are script arguments.
        (0, 20) => InputEval::Evaluated(witness_item_violations(input_index, items)),
        // Wrong-length v0 is a pre-existing failure, not an RDTS rule.
        (0, _) => InputEval::Evaluated(Vec::new()),
        // Native BIP341 Taproot.
        (1, 32) => evaluate_taproot(input_index, items),
        // P2A is recognized only with an empty witness.
        (1, 2) if program == [0x4e, 0x73] => {
            if items.is_empty() {
                InputEval::Evaluated(Vec::new())
            } else {
                InputEval::Evaluated(vec![rule_match(
                    RuleId::UndefinedVersion,
                    Violation::UndefinedWitnessVersion {
                        input: input_index,
                        version: 1,
                        p2a_non_empty_witness: true,
                    },
                )])
            }
        }
        // v1 non-32 non-P2A and every v2..=v16.
        _ => InputEval::Evaluated(vec![rule_match(
            RuleId::UndefinedVersion,
            Violation::UndefinedWitnessVersion {
                input: input_index,
                version,
                p2a_non_empty_witness: false,
            },
        )]),
    }
}

fn evaluate_p2wsh(input_index: usize, items: &[&[u8]]) -> InputEval {
    if items.is_empty() {
        // Pre-existing WITNESS_PROGRAM_WITNESS_EMPTY.
        return InputEval::Evaluated(Vec::new());
    }

    let (script, args) = items.split_last().expect("non-empty witness");
    let mut matches = witness_item_violations(input_index, args);
    matches.extend(push_violations(
        input_index,
        ScriptKind::WitnessScript,
        script,
    ));
    InputEval::Evaluated(matches)
}

/// Taproot client order is annex, control size, commitment, leaf-version
/// dispatch, then the Tapscript checks.
fn evaluate_taproot(input_index: usize, items: &[&[u8]]) -> InputEval {
    match decompose_taproot(items) {
        TaprootSpend::Empty => InputEval::Evaluated(Vec::new()),
        TaprootSpend::KeyPath { annex } => {
            let matches = annex
                .map(|annex| {
                    vec![rule_match(
                        RuleId::TaprootAnnex,
                        Violation::TaprootAnnexPresent {
                            input: input_index,
                            annex_len: annex.len(),
                        },
                    )]
                })
                .unwrap_or_default();
            InputEval::Evaluated(matches)
        }
        TaprootSpend::ScriptPath(sp) => {
            InputEval::Evaluated(evaluate_taproot_script_path(input_index, &sp))
        }
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

fn evaluate_legacy_spend(input_index: usize, txin: &TxIn, spk: &Script) -> InputEval {
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
    InputEval::Evaluated(matches)
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
        .map(|(opcode_pos, len)| {
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
        })
        .collect()
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
