//! Open questions, encoded as failing-by-default `#[ignore]`d tests.
//!
//! Each test asserts the behavior the evaluator *should* have once the open
//! question is resolved against the enforcement client on regtest. They are
//! ignored (not deleted) so the gap is visible in `cargo test` output and so a
//! future regtest-oracle step can un-ignore them one at a time as it confirms
//! each answer against a live `bitcoin-bip110-client` node.
//!
//! Run them with: `cargo test -p rdts-rules --test verify_open -- --ignored`.
//!
//! None of these are covered by the official BIP-110 vectors (which are all
//! native single-input spends).

use bitcoin::hashes::Hash;
use bitcoin::{
    Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness, absolute,
    transaction,
};
use rdts_rules::{EvaluationContext, PrevoutFacts, PrevoutSet, RuleId, evaluate};

const ACTIVATION: u32 = 1000;
const SPEND: u32 = 1500;

fn p2sh_spk() -> ScriptBuf {
    let mut v = vec![0xa9u8, 0x14];
    v.extend(std::iter::repeat_n(0x11u8, 20));
    v.push(0x87);
    ScriptBuf::from_bytes(v)
}

fn tx_p2sh(script_sig: Vec<u8>, witness_items: &[Vec<u8>]) -> Transaction {
    let refs: Vec<&[u8]> = witness_items.iter().map(|v| v.as_slice()).collect();
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

fn eval(spk: ScriptBuf, script_sig: Vec<u8>, witness_items: &[Vec<u8>]) -> rdts_rules::TxEvidence {
    let tx = tx_p2sh(script_sig, witness_items);
    let prevouts = PrevoutSet::from_vec(vec![Some(PrevoutFacts::new(spk, Some(ACTIVATION)))]);
    evaluate(
        &tx,
        &EvaluationContext::new(Some(ACTIVATION), SPEND),
        &prevouts,
    )
}

// VERIFY(client): P2SH-wrapped nested SegWit (rule 2 on the inner witness).
//
// The client dispatches P2SH by evaluating the scriptSig, which pushes the
// redeemScript; if that redeemScript is itself a witness program, the witness
// is verified with `is_p2sh = true`
// (`src/script/interpreter.cpp:2109-2116`). The reduced element-size limit then
// applies to the inner witness stack items just as for native SegWit. We do not
// yet model this nesting and instead return `Unknown(UnsupportedSpend)`.
//
// OPEN QUESTION for the regtest oracle: confirm that a P2SH-P2WSH spend with a
// 257-byte script-argument witness item is rejected for rule 2 (element size),
// and that the redeemScript push in the scriptSig is exempt (below).
#[test]
#[ignore = "VERIFY(client): P2SH-wrapped nested segwit not modeled; confirm on regtest"]
fn verify_p2sh_p2wsh_nested_witness_item_257_violates_rule2() {
    // redeemScript = P2WSH program (OP_0 <32>); scriptSig pushes it.
    let mut redeem = vec![0x00u8, 0x20];
    redeem.extend(std::iter::repeat_n(0xabu8, 32));
    let mut script_sig = vec![redeem.len() as u8];
    script_sig.extend_from_slice(&redeem);

    let witness = vec![vec![0x42u8; 257], vec![0x75u8, 0x51]]; // [257-byte arg, OP_DROP OP_TRUE]
    let ev = eval(p2sh_spk(), script_sig, &witness);

    // Expected once modeled + regtest-confirmed:
    assert!(
        ev.rule(RuleId::ElementSize).verdict.is_violate(),
        "P2SH-P2WSH 257-byte witness item should violate rule 2"
    );
}

// VERIFY(client): the BIP16 redeemScript push is exempt from rule 2.
//
// The client evaluates the scriptSig with `SCRIPT_VERIFY_REDUCED_DATA` cleared
// so the redeemScript push may exceed 256 bytes
// (`src/script/interpreter.cpp:2034-2035`), then re-checks the remaining
// scriptSig stack items against the reduced limit (`:2090-2096`). We do not
// model P2SH and return `Unknown(UnsupportedSpend)`.
//
// OPEN QUESTION for the regtest oracle: confirm that a P2SH spend whose
// redeemScript is itself larger than 256 bytes is NOT rejected for rule 2 on
// account of the redeemScript push, while an oversized *non-redeemScript*
// scriptSig push IS rejected.
#[test]
#[ignore = "VERIFY(client): P2SH redeemScript push exemption not modeled; confirm on regtest"]
fn verify_p2sh_redeemscript_push_is_exempt() {
    // redeemScript of 300 bytes (e.g. a large bare script ending in OP_TRUE).
    let mut redeem = vec![0x51u8]; // OP_TRUE ...
    redeem.resize(300, 0x00);
    // scriptSig pushes the 300-byte redeemScript with OP_PUSHDATA2.
    let mut script_sig = vec![
        0x4du8,
        (redeem.len() & 0xff) as u8,
        (redeem.len() >> 8) as u8,
    ];
    script_sig.extend_from_slice(&redeem);

    let ev = eval(p2sh_spk(), script_sig, &[]);

    // Expected once modeled + regtest-confirmed: the oversized push is the
    // redeemScript, so rule 2 passes.
    assert!(
        ev.rule(RuleId::ElementSize).verdict.is_pass(),
        "the BIP16 redeemScript push must be exempt from rule 2"
    );
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
    let ev = eval(ScriptBuf::from_bytes(spk), Vec::new(), &witness);

    // The intended post-reconciliation behavior is that this is NOT reported as
    // an RDTS rule-7 violation, because the node rejects it earlier for a
    // non-RDTS reason. Currently we DO report rule 7, so this fails by default.
    assert!(
        ev.rule(RuleId::TapscriptOpIf).verdict.is_pass(),
        "commitment-mismatched spend should not be attributed to rule 7"
    );
}
