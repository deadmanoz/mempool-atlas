//! Boundary-case tests authored for this evaluator (beyond the official vectors).
//!
//! These cover the exact edges named in the build brief: 33/34/35-byte outputs,
//! 82/83/84-byte OP_RETURN scripts, 255/256/257-byte pushes and control blocks,
//! Merkle depth 6/7/8, annex present/absent, OP_SUCCESS reachable/unreachable,
//! OP_IF executed vs pushed-as-data, empty vs non-empty P2A witness, plus
//! grandfathering, coinbase, inactive-context, and the `Unknown` paths.

use super::support;

use crate::bip110::{
    EvaluationContext, EvaluationMode, Missing, PrevoutFacts, PrevoutSet, RuleId, RuleVerdict,
    Violation, evaluate, evaluate_mempool_policy,
};
use bitcoin::hashes::Hash;
use bitcoin::{
    Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness, absolute,
    transaction,
};
use support::{p2sh_spk, push_data};

const ACTIVATION: u32 = 1000;
const SPEND: u32 = 1500;
const POST: u32 = ACTIVATION; // created at activation => not grandfathered
const PRE: u32 = ACTIVATION - 1; // created below activation => grandfathered

// ---- construction helpers -------------------------------------------------

fn spk_of_len_nonopreturn(n: usize) -> ScriptBuf {
    // First byte OP_1 (0x51), not OP_RETURN.
    let mut v = vec![0x51u8];
    v.resize(n, 0x00);
    ScriptBuf::from_bytes(v)
}

fn spk_op_return(n: usize) -> ScriptBuf {
    let mut v = vec![0x6au8]; // OP_RETURN
    v.resize(n, 0x00);
    ScriptBuf::from_bytes(v)
}

fn p2wsh_spk() -> ScriptBuf {
    let mut v = vec![0x00u8, 0x20];
    v.extend(std::iter::repeat_n(0xabu8, 32));
    ScriptBuf::from_bytes(v)
}

fn p2tr_spk() -> ScriptBuf {
    let mut v = vec![0x51u8, 0x20];
    v.extend(std::iter::repeat_n(0xcdu8, 32));
    ScriptBuf::from_bytes(v)
}

fn witness_v2_spk() -> ScriptBuf {
    let mut v = vec![0x52u8, 0x20]; // OP_2 push32
    v.extend(std::iter::repeat_n(0x42u8, 32));
    ScriptBuf::from_bytes(v)
}

fn p2a_spk() -> ScriptBuf {
    ScriptBuf::from_bytes(vec![0x51, 0x02, 0x4e, 0x73])
}

fn p2sh_tx(
    redeem_script: &[u8],
    script_sig_arguments: &[Vec<u8>],
    witness_items: &[Vec<u8>],
) -> (Transaction, ScriptBuf) {
    let mut tx = make_tx(witness_items, vec![spk_of_len_nonopreturn(1)]);
    let mut script_sig = Vec::new();
    for argument in script_sig_arguments {
        script_sig.extend(push_data(argument));
    }
    script_sig.extend(push_data(redeem_script));
    tx.input[0].script_sig = ScriptBuf::from_bytes(script_sig);
    (tx, p2sh_spk(redeem_script))
}

/// A control block of the given Merkle depth and leaf version:
/// `[leaf_version] ++ 32-byte internal key ++ depth * 32-byte node`.
fn control_block(depth: usize, leaf_version: u8) -> Vec<u8> {
    let mut v = vec![leaf_version];
    v.extend(std::iter::repeat_n(0x99u8, 32));
    v.extend(std::iter::repeat_n(0x77u8, 32 * depth));
    v
}

fn make_tx(witness_items: &[Vec<u8>], outputs: Vec<ScriptBuf>) -> Transaction {
    let refs: Vec<&[u8]> = witness_items.iter().map(|v| v.as_slice()).collect();
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: Txid::from_byte_array([1u8; 32]),
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::from_slice(&refs),
        }],
        output: outputs
            .into_iter()
            .map(|spk| TxOut {
                value: Amount::from_sat(10_000),
                script_pubkey: spk,
            })
            .collect(),
    }
}

fn coinbase_tx(outputs: Vec<ScriptBuf>) -> Transaction {
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::null(),
            script_sig: ScriptBuf::from_bytes(vec![0x51, 0x00]),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: outputs
            .into_iter()
            .map(|spk| TxOut {
                value: Amount::from_sat(10_000),
                script_pubkey: spk,
            })
            .collect(),
    }
}

/// Evaluate a single-input spend of `spk` (created at `creation`) with the given
/// witness and a trivial 1-byte output, at an active RDTS height.
fn eval_spend(
    spk: ScriptBuf,
    creation: Option<u32>,
    witness_items: &[Vec<u8>],
) -> crate::bip110::TxEvidence {
    let tx = make_tx(witness_items, vec![spk_of_len_nonopreturn(1)]);
    let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk, creation))]);
    let ctx = EvaluationContext::new(Some(ACTIVATION), SPEND);
    evaluate(&tx, &ctx, &prevouts)
}

fn verdict(ev: &crate::bip110::TxEvidence, rule: RuleId) -> &RuleVerdict {
    &ev.rule(rule).verdict
}

// ---- rule 1: output size --------------------------------------------------

#[test]
fn rule1_nonopreturn_boundaries_33_34_35() {
    // Output rule is evaluated regardless of inputs; use a trivial witness spend.
    let spk = p2wsh_spk();
    let w = vec![vec![0x51u8]]; // witness script OP_1
    for (n, expect_pass) in [(33usize, true), (34, true), (35, false)] {
        let tx = make_tx(&w, vec![spk_of_len_nonopreturn(n)]);
        let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk.clone(), Some(POST)))]);
        let ev = evaluate(
            &tx,
            &EvaluationContext::new(Some(ACTIVATION), SPEND),
            &prevouts,
        );
        assert_eq!(
            verdict(&ev, RuleId::OutputSize).is_pass(),
            expect_pass,
            "non-OP_RETURN output of {n} bytes"
        );
    }
}

#[test]
fn rule1_opreturn_boundaries_82_83_84() {
    let spk = p2wsh_spk();
    let w = vec![vec![0x51u8]];
    for (n, expect_pass) in [(82usize, true), (83, true), (84, false)] {
        let tx = make_tx(&w, vec![spk_op_return(n)]);
        let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk.clone(), Some(POST)))]);
        let ev = evaluate(
            &tx,
            &EvaluationContext::new(Some(ACTIVATION), SPEND),
            &prevouts,
        );
        assert_eq!(
            verdict(&ev, RuleId::OutputSize).is_pass(),
            expect_pass,
            "OP_RETURN output of {n} bytes"
        );
    }
}

#[test]
fn rule1_empty_scriptpubkey_is_skipped() {
    let spk = p2wsh_spk();
    let w = vec![vec![0x51u8]];
    let tx = make_tx(&w, vec![ScriptBuf::new()]);
    let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk, Some(POST)))]);
    let ev = evaluate(
        &tx,
        &EvaluationContext::new(Some(ACTIVATION), SPEND),
        &prevouts,
    );
    assert!(verdict(&ev, RuleId::OutputSize).is_pass());
}

#[test]
fn rule1_evidence_carries_counts() {
    let spk = p2wsh_spk();
    let w = vec![vec![0x51u8]];
    let tx = make_tx(&w, vec![spk_op_return(84)]);
    let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk, Some(POST)))]);
    let ev = evaluate(
        &tx,
        &EvaluationContext::new(Some(ACTIVATION), SPEND),
        &prevouts,
    );
    match verdict(&ev, RuleId::OutputSize) {
        RuleVerdict::Violate { evidence, .. } => match &evidence[0] {
            Violation::OutputScriptTooLarge {
                vout,
                len,
                is_op_return,
                limit,
            } => {
                assert_eq!((*vout, *len, *is_op_return, *limit), (0, 84, true, 83));
            }
            other => panic!("wrong evidence: {other:?}"),
        },
        other => panic!("expected Violate, got {other:?}"),
    }
    assert_eq!(
        ev.primary_violation.as_ref().map(|primary| primary.rule),
        Some(RuleId::OutputSize)
    );
}

// ---- rule 2: element / witness-item size ----------------------------------

#[test]
fn rule2_witness_item_boundaries_255_256_257() {
    // P2WSH(OP_DROP OP_TRUE): [arg, witnessScript].
    let spk = p2wsh_spk();
    let script = vec![0x75u8, 0x51]; // OP_DROP OP_TRUE
    for (n, expect_pass) in [(255usize, true), (256, true), (257, false)] {
        let arg = vec![0x42u8; n];
        let ev = eval_spend(spk.clone(), Some(POST), &[arg, script.clone()]);
        assert_eq!(
            verdict(&ev, RuleId::ElementSize).is_pass(),
            expect_pass,
            "witness item of {n} bytes"
        );
    }
}

#[test]
fn rule2_evidence_and_index() {
    let spk = p2wsh_spk();
    let script = vec![0x75u8, 0x51];
    let ev = eval_spend(spk, Some(POST), &[vec![0x42u8; 257], script]);
    match verdict(&ev, RuleId::ElementSize) {
        RuleVerdict::Violate { evidence, .. } => match &evidence[0] {
            Violation::WitnessItemTooLarge {
                input,
                item_index,
                len,
                limit,
            } => {
                assert_eq!((*input, *item_index, *len, *limit), (0, 0, 257, 256));
            }
            other => panic!("wrong evidence: {other:?}"),
        },
        other => panic!("expected Violate, got {other:?}"),
    }
}

// ---- rule 3: undefined versions and P2A -----------------------------------

#[test]
fn rule3_undefined_witness_version_v2() {
    let ev = eval_spend(witness_v2_spk(), Some(POST), &[vec![]]);
    assert!(verdict(&ev, RuleId::UndefinedVersion).is_violate());
}

#[test]
fn rule3_undefined_tapleaf_version() {
    // Taproot script-path with leaf version 0xc2 (undefined), depth 0.
    let spk = p2tr_spk();
    let script = vec![0x51u8]; // OP_1
    let control = control_block(0, 0xc2);
    let ev = eval_spend(spk, Some(POST), &[script, control]);
    match verdict(&ev, RuleId::UndefinedVersion) {
        RuleVerdict::Violate { evidence, .. } => match &evidence[0] {
            Violation::UndefinedTapleafVersion { leaf_version, .. } => {
                assert_eq!(*leaf_version, 0xc2)
            }
            other => panic!("wrong evidence: {other:?}"),
        },
        other => panic!("expected Violate, got {other:?}"),
    }
}

#[test]
fn rule3_p2a_empty_witness_is_recognized() {
    let ev = eval_spend(p2a_spk(), Some(POST), &[]);
    assert!(
        verdict(&ev, RuleId::UndefinedVersion).is_pass(),
        "empty-witness P2A is valid"
    );
}

#[test]
fn rule3_p2a_nonempty_witness_falls_through() {
    let ev = eval_spend(p2a_spk(), Some(POST), &[vec![0x01]]);
    match verdict(&ev, RuleId::UndefinedVersion) {
        RuleVerdict::Violate { evidence, .. } => match &evidence[0] {
            Violation::UndefinedWitnessVersion {
                version,
                p2a_non_empty_witness,
                ..
            } => {
                assert_eq!((*version, *p2a_non_empty_witness), (1, true));
            }
            other => panic!("wrong evidence: {other:?}"),
        },
        other => panic!("expected Violate, got {other:?}"),
    }
}

// ---- rule 4: annex --------------------------------------------------------

#[test]
fn rule4_annex_present_and_absent() {
    let spk = p2tr_spk();
    let script = vec![0x51u8];
    let control = control_block(0, 0xc0);
    // absent
    let ev = eval_spend(spk.clone(), Some(POST), &[script.clone(), control.clone()]);
    assert!(verdict(&ev, RuleId::TaprootAnnex).is_pass());
    // present (annex is the last element, starts with 0x50)
    let annex = vec![0x50u8, 0x00, 0x00];
    let ev = eval_spend(spk, Some(POST), &[script, control, annex]);
    assert!(verdict(&ev, RuleId::TaprootAnnex).is_violate());
}

// ---- rule 5: control block size / depth ------------------------------------

#[test]
fn rule5_depth_6_7_8() {
    let spk = p2tr_spk();
    let script = vec![0x51u8];
    for (depth, expect_pass, expect_len) in
        [(6usize, true, 225usize), (7, true, 257), (8, false, 289)]
    {
        let control = control_block(depth, 0xc0);
        assert_eq!(control.len(), expect_len);
        let ev = eval_spend(spk.clone(), Some(POST), &[script.clone(), control]);
        assert_eq!(
            verdict(&ev, RuleId::ControlBlockSize).is_pass(),
            expect_pass,
            "control block depth {depth}"
        );
    }
}

#[test]
fn rule5_evidence_carries_depth() {
    let spk = p2tr_spk();
    let ev = eval_spend(spk, Some(POST), &[vec![0x51u8], control_block(8, 0xc0)]);
    match verdict(&ev, RuleId::ControlBlockSize) {
        RuleVerdict::Violate { evidence, .. } => match &evidence[0] {
            Violation::ControlBlockTooLarge {
                len, depth, limit, ..
            } => {
                assert_eq!((*len, *depth, *limit), (289, Some(8), 257));
            }
            other => panic!("wrong evidence: {other:?}"),
        },
        other => panic!("expected Violate, got {other:?}"),
    }
}

// ---- rule 6: OP_SUCCESS ---------------------------------------------------

#[test]
fn rule6_op_success_reachable() {
    // tapscript = single OP_SUCCESS254 (0xfe).
    let ev = eval_spend(
        p2tr_spk(),
        Some(POST),
        &[vec![0xfeu8], control_block(0, 0xc0)],
    );
    assert!(verdict(&ev, RuleId::OpSuccess).is_violate());
}

#[test]
fn rule6_op_success_unreachable_still_violates() {
    // tapscript = OP_RETURN OP_SUCCESS254; execution never reaches the second
    // opcode, but the pre-pass scans all opcodes.
    let ev = eval_spend(
        p2tr_spk(),
        Some(POST),
        &[vec![0x6au8, 0xfe], control_block(0, 0xc0)],
    );
    assert!(verdict(&ev, RuleId::OpSuccess).is_violate());
}

#[test]
fn rule6_op_success_byte_pushed_as_data_is_not_op_success() {
    // tapscript = OP_PUSHBYTES_1 0xfe : the 0xfe is data, not an opcode.
    let ev = eval_spend(
        p2tr_spk(),
        Some(POST),
        &[vec![0x01u8, 0xfe], control_block(0, 0xc0)],
    );
    assert!(verdict(&ev, RuleId::OpSuccess).is_pass());
}

#[test]
fn overlapping_annex_and_op_success_are_both_reported_with_annex_primary() {
    let ev = eval_spend(
        p2tr_spk(),
        Some(POST),
        &[vec![0xfeu8], control_block(0, 0xc0), vec![0x50u8, 0x01]],
    );

    assert!(verdict(&ev, RuleId::TaprootAnnex).is_violate());
    assert!(verdict(&ev, RuleId::OpSuccess).is_violate());
    let primary = ev.primary_violation.as_ref().expect("primary violation");
    assert_eq!(primary.rule, RuleId::TaprootAnnex);
    assert!(matches!(
        primary.evidence,
        Violation::TaprootAnnexPresent { input: 0, .. }
    ));
}

#[test]
fn op_success_prevents_later_op_if_from_being_reported_as_executed() {
    let ev = eval_spend(
        p2tr_spk(),
        Some(POST),
        &[vec![0xfeu8, 0x63], control_block(0, 0xc0)],
    );

    assert!(verdict(&ev, RuleId::OpSuccess).is_violate());
    assert!(verdict(&ev, RuleId::TapscriptOpIf).is_pass());
}

// ---- rule 7: OP_IF in tapscript -------------------------------------------

#[test]
fn rule7_op_if_executed_violates() {
    // tapscript = OP_1 OP_IF OP_1 OP_ENDIF.
    let ev = eval_spend(
        p2tr_spk(),
        Some(POST),
        &[vec![0x51u8, 0x63, 0x51, 0x68], control_block(0, 0xc0)],
    );
    assert!(verdict(&ev, RuleId::TapscriptOpIf).is_violate());
}

#[test]
fn rule7_op_if_byte_pushed_as_data_passes() {
    // tapscript = OP_PUSHBYTES_1 0x63 : the OP_IF byte is data.
    let ev = eval_spend(
        p2tr_spk(),
        Some(POST),
        &[vec![0x01u8, 0x63], control_block(0, 0xc0)],
    );
    assert!(verdict(&ev, RuleId::TapscriptOpIf).is_pass());
}

#[test]
fn rule7_op_if_in_witness_v0_is_not_a_violation() {
    // P2WSH(OP_1 OP_IF OP_1 OP_ENDIF): rule 7 is tapscript-only.
    let spk = p2wsh_spk();
    let ev = eval_spend(spk, Some(POST), &[vec![0x51u8, 0x63, 0x51, 0x68]]);
    assert!(verdict(&ev, RuleId::TapscriptOpIf).is_pass());
}

// ---- grandfathering -------------------------------------------------------

#[test]
fn grandfathered_input_exempt_from_rule2() {
    // 257-byte witness item, but the input is pre-activation => exempt.
    let spk = p2wsh_spk();
    let script = vec![0x75u8, 0x51];
    let ev = eval_spend(spk, Some(PRE), &[vec![0x42u8; 257], script]);
    assert!(verdict(&ev, RuleId::ElementSize).is_pass());
}

#[test]
fn grandfathered_input_still_bound_by_rule1() {
    // Pre-activation input, but a 35-byte output is still rejected (rule 1 is
    // never grandfathered).
    let spk = p2wsh_spk();
    let tx = make_tx(&[vec![0x51u8]], vec![spk_of_len_nonopreturn(35)]);
    let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk, Some(PRE)))]);
    let ev = evaluate(
        &tx,
        &EvaluationContext::new(Some(ACTIVATION), SPEND),
        &prevouts,
    );
    assert!(verdict(&ev, RuleId::OutputSize).is_violate());
}

#[test]
fn grandfather_boundary_is_strict() {
    // Created exactly at activation height => NOT grandfathered.
    let spk = p2wsh_spk();
    let script = vec![0x75u8, 0x51];
    let ev = eval_spend(spk, Some(ACTIVATION), &[vec![0x42u8; 257], script]);
    assert!(
        verdict(&ev, RuleId::ElementSize).is_violate(),
        "created at activation is not exempt"
    );
}

// ---- mempool policy -------------------------------------------------------

#[test]
fn mempool_policy_applies_without_activation_or_grandfathering() {
    // Consensus exempts this pre-activation input, while deployed Knots
    // standard policy applies the same element-size rule to every input.
    let spk = p2wsh_spk();
    let script = vec![0x75u8, 0x51];
    let tx = make_tx(
        &[vec![0x42u8; 257], script],
        vec![spk_of_len_nonopreturn(1)],
    );
    let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk, Some(PRE)))]);

    let ev = evaluate_mempool_policy(&tx, &prevouts);

    assert_eq!(ev.evaluation_mode, EvaluationMode::MempoolPolicy);
    assert!(ev.rules_applied);
    assert!(verdict(&ev, RuleId::ElementSize).is_violate());
}

#[test]
fn mempool_policy_does_not_require_creation_height() {
    let spk = p2wsh_spk();
    let tx = make_tx(
        &[vec![0x42u8; 257], vec![0x75u8, 0x51]],
        vec![spk_of_len_nonopreturn(1)],
    );
    let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk, None))]);

    let ev = evaluate_mempool_policy(&tx, &prevouts);

    assert!(verdict(&ev, RuleId::ElementSize).is_violate());
    assert!(!ev.any_unknown());
}

// ---- Unknown paths --------------------------------------------------------

#[test]
fn missing_scriptpubkey_yields_unknown_for_input_rules() {
    // No prevout facts at all for the single input.
    let tx = make_tx(
        &[vec![0x42u8; 257], vec![0x75u8, 0x51]],
        vec![spk_of_len_nonopreturn(1)],
    );
    let prevouts = PrevoutSet::from_vec(vec![None]);
    let ev = evaluate(
        &tx,
        &EvaluationContext::new(Some(ACTIVATION), SPEND),
        &prevouts,
    );
    // Rule 1 still decides (Pass here); rules 2-7 are Unknown(ScriptPubKey).
    assert!(verdict(&ev, RuleId::OutputSize).is_pass());
    for rule in [
        RuleId::ElementSize,
        RuleId::UndefinedVersion,
        RuleId::TaprootAnnex,
        RuleId::ControlBlockSize,
        RuleId::OpSuccess,
        RuleId::TapscriptOpIf,
    ] {
        match verdict(&ev, rule) {
            RuleVerdict::Unknown { missing } => {
                assert!(matches!(missing[0], Missing::ScriptPubKey { input: 0 }));
            }
            other => panic!("rule {rule:?} expected Unknown, got {other:?}"),
        }
    }
}

#[test]
fn proven_violation_retains_other_input_unknowns_and_blocks_primary() {
    let spk = p2wsh_spk();
    let mut tx = make_tx(
        &[vec![0x42u8; 257], vec![0x75u8, 0x51]],
        vec![spk_of_len_nonopreturn(1)],
    );
    tx.input.insert(
        0,
        TxIn {
            previous_output: OutPoint {
                txid: Txid::from_byte_array([9u8; 32]),
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        },
    );
    let prevouts = PrevoutSet::from_vec(vec![None, Some(PrevoutFacts::new(spk, Some(POST)))]);

    let ev = evaluate_mempool_policy(&tx, &prevouts);

    match verdict(&ev, RuleId::ElementSize) {
        RuleVerdict::Violate { evidence, missing } => {
            assert!(matches!(
                evidence[0],
                Violation::WitnessItemTooLarge { input: 1, .. }
            ));
            assert!(matches!(missing[0], Missing::ScriptPubKey { input: 0 }));
        }
        other => panic!("expected Violate with retained missing facts, got {other:?}"),
    }
    assert!(
        ev.primary_violation.is_none(),
        "an unknown earlier input makes the primary rejection indeterminate"
    );
}

#[test]
fn missing_creation_height_with_violation_is_unknown() {
    // scriptPubKey known, would violate rule 2, but grandfathering undecidable.
    let spk = p2wsh_spk();
    let tx = make_tx(
        &[vec![0x42u8; 257], vec![0x75u8, 0x51]],
        vec![spk_of_len_nonopreturn(1)],
    );
    let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk, None))]);
    let ev = evaluate(
        &tx,
        &EvaluationContext::new(Some(ACTIVATION), SPEND),
        &prevouts,
    );
    match verdict(&ev, RuleId::ElementSize) {
        RuleVerdict::Unknown { missing } => {
            assert!(matches!(missing[0], Missing::CreationHeight { input: 0 }));
        }
        other => panic!("expected Unknown, got {other:?}"),
    }
}

#[test]
fn missing_creation_height_without_violation_is_pass() {
    // scriptPubKey known, no structural violation: grandfathering is irrelevant,
    // so the safe verdict is Pass, not Unknown.
    let spk = p2wsh_spk();
    let ev = eval_spend(spk, None, &[vec![0x42u8; 10], vec![0x75u8, 0x51]]);
    assert!(verdict(&ev, RuleId::ElementSize).is_pass());
}

// ---- coinbase, inactive, P2SH ---------------------------------------------

#[test]
fn coinbase_only_checks_rule1() {
    let tx = coinbase_tx(vec![spk_of_len_nonopreturn(35)]);
    let prevouts = PrevoutSet::from_vec(vec![]);
    let ev = evaluate(
        &tx,
        &EvaluationContext::new(Some(ACTIVATION), SPEND),
        &prevouts,
    );
    assert!(ev.is_coinbase);
    assert!(verdict(&ev, RuleId::OutputSize).is_violate());
    for rule in [
        RuleId::ElementSize,
        RuleId::UndefinedVersion,
        RuleId::OpSuccess,
    ] {
        assert!(
            verdict(&ev, rule).is_pass(),
            "coinbase skips input rule {rule:?}"
        );
    }
}

#[test]
fn inactive_context_passes_everything() {
    // spend_height below activation => not active.
    let spk = p2wsh_spk();
    let tx = make_tx(
        &[vec![0x42u8; 257], vec![0x75u8, 0x51]],
        vec![spk_of_len_nonopreturn(35)],
    );
    let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk, Some(POST)))]);
    let ev = evaluate(
        &tx,
        &EvaluationContext::new(Some(ACTIVATION), ACTIVATION - 1),
        &prevouts,
    );
    assert_eq!(ev.evaluation_mode, EvaluationMode::Consensus);
    assert!(!ev.rules_applied);
    assert!(!ev.any_violation());
    assert!(!ev.any_unknown());
}

#[test]
fn expired_context_is_inactive() {
    let spk = p2wsh_spk();
    let tx = make_tx(&[vec![0x51u8]], vec![spk_of_len_nonopreturn(35)]);
    let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk, Some(POST)))]);
    // activation + ACTIVE_DURATION is the first expired height.
    let expiry = ACTIVATION + crate::bip110::ACTIVE_DURATION;
    let ev = evaluate(
        &tx,
        &EvaluationContext::new(Some(ACTIVATION), expiry),
        &prevouts,
    );
    assert!(
        !ev.rules_applied,
        "at expiry height the rules no longer apply as consensus"
    );
}

#[test]
fn legacy_scriptsig_push_over_256_violates_rule2() {
    // Bare (non-witness, non-P2SH) prevout `OP_TRUE`, spent with a scriptSig
    // that pushes a 257-byte element. Client: `EvalScript` on the scriptSig
    // (BASE sigversion) enforces the 256-byte reduced element size
    // (`src/script/interpreter.cpp:436`). Source-confirmed; flagged for regtest
    // cross-check (no official vector covers legacy scriptSig).
    let spk = ScriptBuf::from_bytes(vec![0x51u8]); // bare OP_TRUE
    let mut script_sig = vec![0x4du8, 0x01, 0x01]; // OP_PUSHDATA2, len 257 (LE)
    script_sig.extend(std::iter::repeat_n(0x42u8, 257));
    let tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: Txid::from_byte_array([2u8; 32]),
                vout: 0,
            },
            script_sig: ScriptBuf::from_bytes(script_sig),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(10_000),
            script_pubkey: spk_of_len_nonopreturn(1),
        }],
    };
    let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk, Some(POST)))]);
    let ev = evaluate(
        &tx,
        &EvaluationContext::new(Some(ACTIVATION), SPEND),
        &prevouts,
    );
    match verdict(&ev, RuleId::ElementSize) {
        RuleVerdict::Violate { evidence, .. } => {
            assert!(matches!(
                evidence[0],
                Violation::PushTooLarge {
                    input: 0,
                    script: crate::bip110::ScriptKind::ScriptSig,
                    len: 257,
                    limit: 256,
                    ..
                }
            ));
        }
        other => panic!("expected Violate, got {other:?}"),
    }
}

#[test]
fn p2sh_non_redeem_item_boundaries_256_257() {
    // redeemScript = OP_DROP OP_TRUE. The preceding scriptSig item is checked
    // after the final redeemScript item is popped from the P2SH stack.
    let redeem_script = [0x75, 0x51];
    for (len, expect_pass) in [(256, true), (257, false)] {
        let (tx, spk) = p2sh_tx(&redeem_script, &[vec![0x42; len]], &[]);
        let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk, Some(POST)))]);
        let ev = evaluate_mempool_policy(&tx, &prevouts);
        assert_eq!(
            verdict(&ev, RuleId::ElementSize).is_pass(),
            expect_pass,
            "P2SH scriptSig argument of {len} bytes"
        );
        assert!(!ev.any_unknown());
    }

    let (tx, spk) = p2sh_tx(&redeem_script, &[vec![0x42; 257]], &[]);
    let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk, Some(POST)))]);
    let ev = evaluate_mempool_policy(&tx, &prevouts);
    match verdict(&ev, RuleId::ElementSize) {
        RuleVerdict::Violate { evidence, .. } => assert!(matches!(
            evidence[0],
            Violation::PushTooLarge {
                input: 0,
                script: crate::bip110::ScriptKind::ScriptSig,
                opcode_pos: 0,
                len: 257,
                limit: 256,
            }
        )),
        other => panic!("expected P2SH scriptSig violation, got {other:?}"),
    }
}

#[test]
fn p2sh_redeemscript_blob_is_exempt_but_its_internal_push_is_not() {
    // This otherwise-valid redeemScript is larger than 256 bytes as a whole,
    // but every push inside it is within the reduced limit:
    // <256 bytes> DROP <32 bytes> DROP TRUE.
    let mut allowed_redeem = push_data(&vec![0x42; 256]);
    allowed_redeem.push(0x75);
    allowed_redeem.extend(push_data(&[0x24; 32]));
    allowed_redeem.extend([0x75, 0x51]);
    assert!(allowed_redeem.len() > 256);

    let (tx, spk) = p2sh_tx(&allowed_redeem, &[], &[]);
    let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk, Some(POST)))]);
    let ev = evaluate_mempool_policy(&tx, &prevouts);
    assert!(verdict(&ev, RuleId::ElementSize).is_pass());
    assert!(!ev.any_unknown());

    // The same outer exemption does not exempt a 257-byte push decoded while
    // executing the redeemScript.
    let mut violating_redeem = push_data(&vec![0x42; 257]);
    violating_redeem.extend([0x75, 0x51]);
    let (tx, spk) = p2sh_tx(&violating_redeem, &[], &[]);
    let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk, Some(POST)))]);
    let ev = evaluate_mempool_policy(&tx, &prevouts);
    match verdict(&ev, RuleId::ElementSize) {
        RuleVerdict::Violate { evidence, .. } => assert!(matches!(
            evidence[0],
            Violation::PushTooLarge {
                input: 0,
                script: crate::bip110::ScriptKind::RedeemScript,
                opcode_pos: 0,
                len: 257,
                limit: 256,
            }
        )),
        other => panic!("expected redeemScript violation, got {other:?}"),
    }
}

#[test]
fn grandfathered_p2sh_spend_is_exempt_from_consensus_input_rules() {
    let redeem_script = [0x75, 0x51]; // OP_DROP OP_TRUE
    let (tx, spk) = p2sh_tx(&redeem_script, &[vec![0x42; 257]], &[]);
    let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk, Some(PRE)))]);

    let ev = evaluate(
        &tx,
        &EvaluationContext::new(Some(ACTIVATION), SPEND),
        &prevouts,
    );

    for rule in [
        RuleId::ElementSize,
        RuleId::UndefinedVersion,
        RuleId::TaprootAnnex,
        RuleId::ControlBlockSize,
        RuleId::OpSuccess,
        RuleId::TapscriptOpIf,
    ] {
        assert!(
            verdict(&ev, rule).is_pass(),
            "grandfathering must exempt P2SH input rule {rule:?}"
        );
    }
    assert!(!ev.any_unknown());
}

#[test]
fn current_p2sh_spend_is_evaluated_under_consensus_rules() {
    let redeem_script = [0x75, 0x51]; // OP_DROP OP_TRUE
    let (tx, spk) = p2sh_tx(&redeem_script, &[vec![0x42; 257]], &[]);
    let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk, Some(POST)))]);

    let ev = evaluate(
        &tx,
        &EvaluationContext::new(Some(ACTIVATION), SPEND),
        &prevouts,
    );

    assert!(verdict(&ev, RuleId::ElementSize).is_violate());
    assert!(!ev.any_unknown());
    assert_eq!(
        ev.primary_violation.as_ref().map(|primary| primary.rule),
        Some(RuleId::ElementSize)
    );
}

#[test]
fn p2sh_spend_is_fully_evaluated_under_mempool_policy() {
    let redeem_script = [0x75, 0x51]; // OP_DROP OP_TRUE
    let (tx, spk) = p2sh_tx(&redeem_script, &[vec![0x42; 257]], &[]);
    let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk, Some(PRE)))]);

    let ev = evaluate_mempool_policy(&tx, &prevouts);

    assert!(verdict(&ev, RuleId::ElementSize).is_violate());
    assert!(!ev.any_unknown());
    assert_eq!(
        ev.primary_violation.as_ref().map(|primary| primary.rule),
        Some(RuleId::ElementSize)
    );
}

#[test]
fn p2sh_wrapped_rule3_respects_consensus_grandfathering() {
    let wrapped_programs = [
        {
            let mut script = vec![0x51, 0x20]; // OP_1 push32
            script.extend([0x33; 32]);
            script
        },
        {
            let mut script = vec![0x52, 0x20]; // OP_2 push32
            script.extend([0x44; 32]);
            script
        },
    ];

    for redeem_script in wrapped_programs {
        let (tx, spk) = p2sh_tx(&redeem_script, &[], &[]);

        let pre = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk.clone(), Some(PRE)))]);
        let grandfathered = evaluate(&tx, &EvaluationContext::new(Some(ACTIVATION), SPEND), &pre);
        assert!(
            verdict(&grandfathered, RuleId::UndefinedVersion).is_pass(),
            "pre-activation P2SH-wrapped witness program must be grandfathered"
        );

        let post = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk, Some(POST)))]);
        let current = evaluate(&tx, &EvaluationContext::new(Some(ACTIVATION), SPEND), &post);
        assert!(
            verdict(&current, RuleId::UndefinedVersion).is_violate(),
            "current P2SH-wrapped witness program must receive Rule 3"
        );
    }
}

#[test]
fn rule_ids_serialize_to_stable_snake_case_names() {
    let cases = [
        (RuleId::OutputSize, "\"output_size\""),
        (RuleId::ElementSize, "\"element_size\""),
        (RuleId::UndefinedVersion, "\"undefined_version\""),
        (RuleId::TaprootAnnex, "\"taproot_annex\""),
        (RuleId::ControlBlockSize, "\"control_block_size\""),
        (RuleId::OpSuccess, "\"op_success\""),
        (RuleId::TapscriptOpIf, "\"tapscript_op_if\""),
    ];

    for (rule, expected) in cases {
        assert_eq!(serde_json::to_string(&rule).unwrap(), expected);
    }
}
