//! Replay of the official BIP-110 (`REDUCED_DATA`) consensus test vectors.
//!
//! Provenance: `bip-0110/test-vectors.json` from the Bitcoin BIPs repository
//! (github.com/bitcoin/bips), last changed by commit
//! `b0a1a276021cf371a93865315b274a55616e3b6c` ("BIP-110: Add test vectors JSON
//! and generator script", 2026-06-26), repo HEAD
//! `c021a5f51ae9d3e71a41eac3dda6dc060fead35d`. Copied verbatim to
//! `tests/fixtures/bip110-official-test-vectors.json` (MIT licensed, (c) 2026
//! The Bitcoin Knots developers). The vectors were generated and verified
//! against the reference implementation
//! (github.com/dathonohm/bitcoin) via the accompanying `test-vectors.py`.
//!
//! Each vector is a single-input block-context case: it gives the spent
//! output(s), the spending/creating transaction, and whether a block containing
//! it is valid while `REDUCED_DATA` is active. We map "post-activation" to a
//! non-grandfathered input and "pre-activation" to a grandfathered one, then
//! assert our verdict matches.

use bitcoin::consensus::encode::deserialize_hex;
use bitcoin::hex::FromHex;
use bitcoin::{ScriptBuf, Transaction};
use rdts_rules::{
    EvaluationContext, EvaluationMode, PrevoutFacts, PrevoutSet, RuleId, evaluate,
    evaluate_mempool_policy,
};
use serde::Deserialize;

const ACTIVATION: u32 = 1000;
const SPEND: u32 = 1500;

#[derive(Deserialize)]
struct VectorFile {
    vectors: Vec<Vector>,
}

#[derive(Deserialize)]
struct Vector {
    rule: u8,
    name: String,
    reduced_data_active: bool,
    spent_utxo: String,
    spent_outputs: Vec<SpentOutput>,
    tx: String,
    expected: String,
}

#[derive(Deserialize)]
struct SpentOutput {
    #[serde(rename = "scriptPubKey")]
    script_pubkey: String,
}

fn rule_id(number: u8) -> RuleId {
    match number {
        1 => RuleId::OutputSize,
        2 => RuleId::ElementSize,
        3 => RuleId::UndefinedVersion,
        4 => RuleId::TaprootAnnex,
        5 => RuleId::ControlBlockSize,
        6 => RuleId::OpSuccess,
        7 => RuleId::TapscriptOpIf,
        n => panic!("unexpected rule number {n}"),
    }
}

fn load() -> VectorFile {
    let raw = include_str!("fixtures/bip110-official-test-vectors.json");
    serde_json::from_str(raw).expect("parse official vectors")
}

fn prevouts(v: &Vector, creation_height: u32) -> PrevoutSet {
    v.spent_outputs
        .iter()
        .map(|o| {
            let bytes = Vec::<u8>::from_hex(&o.script_pubkey).expect("decode spk hex");
            Some(PrevoutFacts::new(
                ScriptBuf::from_bytes(bytes),
                Some(creation_height),
            ))
        })
        .collect()
}

#[test]
fn official_vectors_replay() {
    let file = load();
    assert_eq!(file.vectors.len(), 16, "expected 16 official vectors");

    let mut checked = 0;
    for v in &file.vectors {
        assert!(
            v.reduced_data_active,
            "[{}] all official vectors are RDTS-active",
            v.name
        );

        let tx: Transaction = deserialize_hex(&v.tx).expect("decode vector tx");

        // Grandfathering: "pre-activation" => created below activation
        // (exempt); "post-activation" => created at/after (not exempt).
        let creation_height = match v.spent_utxo.as_str() {
            "pre-activation" => ACTIVATION - 1,
            "post-activation" => ACTIVATION,
            other => panic!("[{}] unexpected spent_utxo {other}", v.name),
        };

        let prevouts = prevouts(v, creation_height);

        let ctx = EvaluationContext::new(Some(ACTIVATION), SPEND);
        let evidence = evaluate(&tx, &ctx, &prevouts);

        assert!(
            evidence.rules_applied,
            "[{}] context should be active",
            v.name
        );
        assert!(
            !evidence.any_unknown(),
            "[{}] fully specified, no Unknown expected",
            v.name
        );

        match v.expected.as_str() {
            "valid" => assert!(
                !evidence.any_violation(),
                "[{}] expected VALID but got a violation: {:#?}",
                v.name,
                evidence.rules,
            ),
            "invalid" => {
                let target = rule_id(v.rule);
                assert!(
                    evidence.rule(target).verdict.is_violate(),
                    "[{}] expected rule {} to Violate, got {:#?}",
                    v.name,
                    v.rule,
                    evidence.rule(target).verdict,
                );
            }
            other => panic!("[{}] unexpected expected={other}", v.name),
        }
        checked += 1;
    }
    assert_eq!(checked, 16);
}

#[test]
fn official_vectors_replay_under_mempool_policy() {
    let file = load();
    let mut violated_rules = [false; 7];

    for v in &file.vectors {
        let tx: Transaction = deserialize_hex(&v.tx).expect("decode vector tx");
        // Deliberately mark every input pre-activation. Policy must ignore the
        // creation height and apply all seven restrictions anyway.
        let prevouts = prevouts(v, ACTIVATION - 1);
        let evidence = evaluate_mempool_policy(&tx, &prevouts);

        assert_eq!(evidence.evaluation_mode, EvaluationMode::MempoolPolicy);
        assert!(evidence.rules_applied);
        assert!(!evidence.any_unknown(), "[{}] fully specified", v.name);

        let grandfathered_rule_two = v.name == "grandfathered_witness_item_257_valid";
        if v.expected == "invalid" || grandfathered_rule_two {
            let target = rule_id(v.rule);
            assert!(
                evidence.rule(target).verdict.is_violate(),
                "[{}] expected policy rule {} to Violate, got {:#?}",
                v.name,
                v.rule,
                evidence.rule(target).verdict,
            );
            violated_rules[usize::from(v.rule - 1)] = true;
        } else {
            assert!(
                !evidence.any_violation(),
                "[{}] expected policy-valid but got {:#?}",
                v.name,
                evidence.rules,
            );
        }
    }

    assert!(
        violated_rules.into_iter().all(|covered| covered),
        "official invalid vectors must exercise all seven policy rules"
    );
}
