//! Client-source cross-checks not covered by the official BIP-110 vectors.
//!
//! The fixtures use correct P2SH and P2WSH commitments. Spend-validity details
//! relevant to each case are documented at the test, while synthetic witness
//! elements isolate checks that run before signature validation. Expected RDTS
//! behavior follows the deployed client's `VerifyScript` order. The remaining
//! ignored Taproot case documents a separate commitment-validation boundary.

use super::support;

use crate::bip110::{PrevoutFacts, PrevoutSet, RuleId, RuleVerdict, ScriptKind, Violation};
use bitcoin::hashes::{Hash, sha256};
use bitcoin::{
    Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness, absolute,
    transaction,
};
use support::{p2sh_spk, push_data};

fn witness_program(version_opcode: u8, program: &[u8]) -> Vec<u8> {
    let mut script = vec![version_opcode, program.len() as u8];
    script.extend_from_slice(program);
    script
}

fn p2wsh_redeem_script(witness_script: &[u8]) -> Vec<u8> {
    witness_program(0x00, &sha256::Hash::hash(witness_script).to_byte_array())
}

fn tx_p2sh(
    redeem_script: &[u8],
    script_sig_arguments: &[Vec<u8>],
    witness_items: &[Vec<u8>],
) -> Transaction {
    let refs: Vec<&[u8]> = witness_items.iter().map(|v| v.as_slice()).collect();
    let mut script_sig = Vec::new();
    for argument in script_sig_arguments {
        script_sig.extend(push_data(argument));
    }
    script_sig.extend(push_data(redeem_script));
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: Txid::from_byte_array([3u8; 32]),
                vout: 0,
            },
            script_sig: ScriptBuf::from_bytes(script_sig),
            sequence: Sequence::MAX,
            witness: Witness::from_slice(&refs),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(10_000),
            script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
        }],
    }
}

fn eval_tx(spk: ScriptBuf, tx: &Transaction) -> crate::bip110::TxEvidence {
    let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk, None))]);
    crate::bip110::evaluate_mempool_policy(tx, &prevouts)
}

fn eval_p2sh(
    redeem_script: &[u8],
    script_sig_arguments: &[Vec<u8>],
    witness_items: &[Vec<u8>],
) -> crate::bip110::TxEvidence {
    let tx = tx_p2sh(redeem_script, script_sig_arguments, witness_items);
    eval_tx(p2sh_spk(redeem_script), &tx)
}

fn eval_native(spk: ScriptBuf, witness_items: &[Vec<u8>]) -> crate::bip110::TxEvidence {
    let mut tx = tx_p2sh(&[], &[], witness_items);
    tx.input[0].script_sig = ScriptBuf::new();
    eval_tx(spk, &tx)
}

#[test]
fn verify_p2sh_p2wsh_nested_witness_item_boundaries() {
    // P2WSH(OP_DROP OP_TRUE), with exact SHA256 and P2SH commitments. This is
    // otherwise valid under the ordinary 520-byte element limit.
    let witness_script = vec![0x75, 0x51];
    let redeem_script = p2wsh_redeem_script(&witness_script);
    for (len, expect_pass) in [(256, true), (257, false)] {
        let witness = vec![vec![0x42; len], witness_script.clone()];
        let ev = eval_p2sh(&redeem_script, &[], &witness);
        assert_eq!(
            ev.rule(RuleId::ElementSize).verdict.is_pass(),
            expect_pass,
            "nested P2WSH witness item of {len} bytes"
        );
        assert!(!ev.any_unknown());
    }

    let witness = vec![vec![0x42; 257], witness_script];
    let ev = eval_p2sh(&redeem_script, &[], &witness);

    match &ev.rule(RuleId::ElementSize).verdict {
        RuleVerdict::Violate { evidence, .. } => assert!(matches!(
            evidence[0],
            Violation::WitnessItemTooLarge {
                input: 0,
                item_index: 0,
                len: 257,
                limit: 256,
            }
        )),
        other => panic!("expected nested P2WSH Rule 2 violation, got {other:?}"),
    }
    assert!(!ev.any_unknown());
}

#[test]
fn verify_p2sh_p2wsh_exempts_script_blob_but_checks_internal_pushes() {
    let mut allowed_script = push_data(&vec![0x42; 256]);
    allowed_script.push(0x75);
    allowed_script.extend(push_data(&[0x24; 32]));
    allowed_script.extend([0x75, 0x51]);
    assert!(allowed_script.len() > 256);
    let redeem_script = p2wsh_redeem_script(&allowed_script);
    let ev = eval_p2sh(&redeem_script, &[], &[allowed_script]);
    assert!(ev.rule(RuleId::ElementSize).verdict.is_pass());

    let mut violating_script = push_data(&vec![0x42; 257]);
    violating_script.extend([0x75, 0x51]);
    let redeem_script = p2wsh_redeem_script(&violating_script);
    let ev = eval_p2sh(&redeem_script, &[], &[violating_script]);
    match &ev.rule(RuleId::ElementSize).verdict {
        RuleVerdict::Violate { evidence, .. } => assert!(matches!(
            evidence[0],
            Violation::PushTooLarge {
                input: 0,
                script: ScriptKind::WitnessScript,
                opcode_pos: 0,
                len: 257,
                limit: 256,
            }
        )),
        other => panic!("expected nested witnessScript violation, got {other:?}"),
    }
}

#[test]
fn verify_p2sh_redeemscript_push_is_exempt() {
    // <256 bytes> DROP <32 bytes> DROP TRUE is over 256 bytes as a complete
    // redeemScript, contains no oversized internal push, and leaves one true
    // stack item. Its only scriptSig item is the exempt redeemScript blob.
    let mut redeem_script = push_data(&vec![0x42; 256]);
    redeem_script.push(0x75);
    redeem_script.extend(push_data(&[0x24; 32]));
    redeem_script.extend([0x75, 0x51]);
    assert!(redeem_script.len() > 256);

    let ev = eval_p2sh(&redeem_script, &[], &[]);
    assert!(
        ev.rule(RuleId::ElementSize).verdict.is_pass(),
        "the BIP16 redeemScript push must be exempt from rule 2"
    );
    assert!(!ev.any_unknown());
}

#[test]
fn verify_p2sh_p2wpkh_dispatch_checks_both_witness_items() {
    // The client applies the reduced-element precheck to both P2WPKH witness
    // items before signature validation. Synthetic item sizes isolate that
    // ordering without pretending to be valid DER signatures or public keys.
    let redeem_script = witness_program(0x00, &[0x22; 20]);
    for (len, expect_pass) in [(256, true), (257, false)] {
        let witness = vec![vec![0x30; len], vec![0x02; 33]];
        let ev = eval_p2sh(&redeem_script, &[], &witness);
        assert_eq!(
            ev.rule(RuleId::ElementSize).verdict.is_pass(),
            expect_pass,
            "nested P2WPKH witness item of {len} bytes"
        );
        assert!(!ev.any_unknown());
    }

    let ev = eval_p2sh(&redeem_script, &[], &[vec![0x30; 72], vec![0x02; 257]]);
    match &ev.rule(RuleId::ElementSize).verdict {
        RuleVerdict::Violate { evidence, .. } => assert!(matches!(
            evidence[0],
            Violation::WitnessItemTooLarge {
                input: 0,
                item_index: 1,
                len: 257,
                limit: 256,
            }
        )),
        other => panic!("expected second P2WPKH item violation, got {other:?}"),
    }
}

#[test]
fn verify_noncanonical_p2sh_scriptsig_does_not_dispatch_nested_witness() {
    let redeem_script = witness_program(0x51, &[0x33; 32]);
    let ev = eval_p2sh(&redeem_script, &[vec![0x01]], &[vec![0x50, 0x01]]);

    assert!(ev.rule(RuleId::UndefinedVersion).verdict.is_pass());
    assert!(!ev.any_unknown());

    // The client's canonical form for a 34-byte redeemScript is a direct
    // single push. Encoding that same lone item through OP_PUSHDATA1 reaches
    // WITNESS_MALLEATED_P2SH and must not dispatch the wrapped witness program.
    let mut tx = tx_p2sh(&redeem_script, &[], &[vec![0x50, 0x01]]);
    let mut noncanonical = vec![0x4c, redeem_script.len() as u8];
    noncanonical.extend_from_slice(&redeem_script);
    tx.input[0].script_sig = ScriptBuf::from_bytes(noncanonical);
    let ev = eval_tx(p2sh_spk(&redeem_script), &tx);
    assert!(ev.rule(RuleId::UndefinedVersion).verdict.is_pass());
    assert!(!ev.any_unknown());
}

#[test]
fn verify_p2sh_wrapped_v1_is_undefined_not_taproot() {
    let redeem_script = witness_program(0x51, &[0x33; 32]);
    let ev = eval_p2sh(&redeem_script, &[], &[vec![0x50, 0x01]]);

    match &ev.rule(RuleId::UndefinedVersion).verdict {
        RuleVerdict::Violate { evidence, .. } => assert!(matches!(
            evidence[0],
            Violation::UndefinedWitnessVersion {
                input: 0,
                version: 1,
                p2sh_wrapped: true,
                p2a_non_empty_witness: false,
            }
        )),
        other => panic!("expected wrapped v1 Rule 3 violation, got {other:?}"),
    }
    for rule in [
        RuleId::TaprootAnnex,
        RuleId::ControlBlockSize,
        RuleId::OpSuccess,
        RuleId::TapscriptOpIf,
    ] {
        assert!(ev.rule(rule).verdict.is_pass(), "wrapped v1 is not Taproot");
    }
}

#[test]
fn verify_p2sh_wrapped_p2a_is_undefined_even_with_empty_witness() {
    let redeem_script = witness_program(0x51, &[0x4e, 0x73]);
    let ev = eval_p2sh(&redeem_script, &[], &[]);

    match &ev.rule(RuleId::UndefinedVersion).verdict {
        RuleVerdict::Violate { evidence, .. } => assert!(matches!(
            evidence[0],
            Violation::UndefinedWitnessVersion {
                input: 0,
                version: 1,
                p2sh_wrapped: true,
                p2a_non_empty_witness: false,
            }
        )),
        other => panic!("expected wrapped P2A Rule 3 violation, got {other:?}"),
    }
    for rule in [
        RuleId::TaprootAnnex,
        RuleId::ControlBlockSize,
        RuleId::OpSuccess,
        RuleId::TapscriptOpIf,
    ] {
        assert!(ev.rule(rule).verdict.is_pass(), "wrapped P2A is undefined");
    }
    assert!(!ev.any_unknown());
}

#[test]
fn verify_p2sh_wrapped_v2_is_undefined() {
    let redeem_script = witness_program(0x52, &[0x44; 32]);
    let ev = eval_p2sh(&redeem_script, &[], &[vec![0x01]]);

    match &ev.rule(RuleId::UndefinedVersion).verdict {
        RuleVerdict::Violate { evidence, .. } => assert!(matches!(
            evidence[0],
            Violation::UndefinedWitnessVersion {
                input: 0,
                version: 2,
                p2sh_wrapped: true,
                p2a_non_empty_witness: false,
            }
        )),
        other => panic!("expected wrapped v2 Rule 3 violation, got {other:?}"),
    }
    for rule in [
        RuleId::TaprootAnnex,
        RuleId::ControlBlockSize,
        RuleId::OpSuccess,
        RuleId::TapscriptOpIf,
    ] {
        assert!(ev.rule(rule).verdict.is_pass(), "wrapped v2 is not Taproot");
    }
}

#[test]
fn p2sh_rule2_evidence_preserves_client_stage_order() {
    let outer_redeem = [0x75, 0x51]; // OP_DROP OP_TRUE
    let mut outer_tx = tx_p2sh(&outer_redeem, &[vec![0x11; 257]], &[]);

    let mut inner_redeem = push_data(&vec![0x22; 257]);
    inner_redeem.extend([0x75, 0x51]);
    let mut inner_tx = tx_p2sh(&inner_redeem, &[], &[]);

    let witness_script = vec![0x75, 0x51];
    let nested_redeem = p2wsh_redeem_script(&witness_script);
    let mut nested_tx = tx_p2sh(&nested_redeem, &[], &[vec![0x33; 257], witness_script]);

    let mut inputs = vec![
        outer_tx.input.remove(0),
        inner_tx.input.remove(0),
        nested_tx.input.remove(0),
    ];
    for (index, input) in inputs.iter_mut().enumerate() {
        input.previous_output = OutPoint {
            txid: Txid::from_byte_array([index as u8 + 1; 32]),
            vout: 0,
        };
    }
    let tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: inputs,
        output: outer_tx.output,
    };
    let prevouts = PrevoutSet::from_vec(vec![
        Some(PrevoutFacts::new(p2sh_spk(&outer_redeem), None)),
        Some(PrevoutFacts::new(p2sh_spk(&inner_redeem), None)),
        Some(PrevoutFacts::new(p2sh_spk(&nested_redeem), None)),
    ]);
    let ev = crate::bip110::evaluate_mempool_policy(&tx, &prevouts);

    let RuleVerdict::Violate { evidence, .. } = &ev.rule(RuleId::ElementSize).verdict else {
        panic!("expected ordered P2SH Rule 2 evidence");
    };
    assert!(matches!(
        evidence[0],
        Violation::PushTooLarge {
            input: 0,
            script: ScriptKind::ScriptSig,
            ..
        }
    ));
    assert!(matches!(
        evidence[1],
        Violation::PushTooLarge {
            input: 1,
            script: ScriptKind::RedeemScript,
            ..
        }
    ));
    assert!(matches!(
        evidence[2],
        Violation::WitnessItemTooLarge { input: 2, .. }
    ));
    let primary = ev.primary_violation.as_ref().expect("primary violation");
    assert_eq!(primary.rule, RuleId::ElementSize);
    assert!(matches!(
        primary.evidence,
        Violation::PushTooLarge {
            input: 0,
            script: ScriptKind::ScriptSig,
            ..
        }
    ));
}

#[test]
fn p2sh_evidence_serializes_with_stable_context() {
    let push = Violation::PushTooLarge {
        input: 0,
        script: ScriptKind::RedeemScript,
        opcode_pos: 3,
        len: 257,
        limit: 256,
    };
    let push_json = serde_json::to_value(push).expect("serialize redeemScript evidence");
    assert_eq!(push_json["script"], "redeem_script");

    let undefined = Violation::UndefinedWitnessVersion {
        input: 0,
        version: 1,
        p2sh_wrapped: true,
        p2a_non_empty_witness: false,
    };
    let undefined_json = serde_json::to_value(undefined).expect("serialize wrapped evidence");
    assert_eq!(undefined_json["p2sh_wrapped"], true);

    let native = Violation::UndefinedWitnessVersion {
        input: 0,
        version: 2,
        p2sh_wrapped: false,
        p2a_non_empty_witness: false,
    };
    let native_json = serde_json::to_value(native).expect("serialize native evidence");
    assert_eq!(native_json["p2sh_wrapped"], false);
}

// SCOPE / regtest cross-check: Taproot commitment is not recomputed.
//
// The client verifies the BIP341 taproot commitment (`VerifyTaprootCommitment`,
// `src/script/interpreter.cpp:1988`) and rejects a mismatch with
// `WITNESS_PROGRAM_MISMATCH` *before* reaching the RDTS sub-rules (6/7/2-push).
// This evaluator does not recompute the tweak and reports the RDTS sub-rule
// status directly, assuming an otherwise-valid spend. For a spend whose
// commitment does NOT match the scriptPubKey, our verdict can therefore differ
// from the node (we may report a rule 6/7 violation the node never evaluates).
//
// OPEN QUESTION for the regtest oracle: confirm that for all *otherwise-valid*
// spends the consumers care about, this divergence never changes the block
// verdict; document any consumer that must feed only commitment-checked spends.
#[test]
#[ignore = "SCOPE: taproot commitment not recomputed; confirm divergence is immaterial on regtest"]
fn verify_taproot_commitment_divergence_is_immaterial() {
    // A tapscript with OP_IF whose control block does not commit to `script`
    // under the scriptPubKey's internal key. The node rejects for MISMATCH; we
    // report rule 7. This test documents that gap and must be reconciled
    // against the node before relying on rule 7 for un-checked spends.
    let mut spk = vec![0x51u8, 0x20];
    spk.extend(std::iter::repeat_n(0xcdu8, 32));
    let mut control = vec![0xc0u8];
    control.extend(std::iter::repeat_n(0x99u8, 32)); // arbitrary internal key (won't commit)
    let witness = vec![vec![0x51u8, 0x63, 0x51, 0x68], control]; // OP_1 OP_IF OP_1 OP_ENDIF
    let ev = eval_native(ScriptBuf::from_bytes(spk), &witness);

    // The intended post-reconciliation behavior is that this is NOT reported as
    // an RDTS rule-7 violation, because the node rejects it earlier for a
    // non-RDTS reason. Currently we DO report rule 7, so this fails by default.
    assert!(
        ev.rule(RuleId::TapscriptOpIf).verdict.is_pass(),
        "commitment-mismatched spend should not be attributed to rule 7"
    );
}
