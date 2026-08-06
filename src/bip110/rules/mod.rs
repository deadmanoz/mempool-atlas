//! Per-transaction orchestration for consensus and mempool-policy evaluation.

mod script_rules;

use crate::bip110::context::{MAX_OUTPUT_DATA_SIZE, MAX_OUTPUT_SCRIPT_SIZE, OP_RETURN};
use crate::bip110::prevout::PrevoutSet;
use crate::bip110::verdict::{
    EvaluationMode, Missing, PrimaryViolation, RuleId, RuleOutcome, RuleVerdict, TxEvidence,
    Violation,
};
use bitcoin::Transaction;

#[cfg(test)]
use crate::bip110::context::EvaluationContext;

/// The six input-scoped rules, in report order.
const INPUT_SCOPED: [RuleId; 6] = [
    RuleId::ElementSize,
    RuleId::UndefinedVersion,
    RuleId::TaprootAnnex,
    RuleId::ControlBlockSize,
    RuleId::OpSuccess,
    RuleId::TapscriptOpIf,
];

#[derive(Debug, Clone, Copy)]
enum Applicability {
    #[cfg(test)]
    Consensus(EvaluationContext),
    MempoolPolicy,
}

/// Evaluate every RDTS consensus rule for `tx`.
///
/// - `ctx` supplies the branch activation height and the evaluation height.
/// - `prevouts` supplies per-input scriptPubKey and creation height; absent
///   facts yield explicit `Unknown` verdicts for the input-scoped rules.
///
/// When RDTS is not active at the evaluation height, every rule is reported
/// `Pass` and `rules_applied` is `false`.
#[cfg(test)]
pub fn evaluate_consensus(
    tx: &Transaction,
    ctx: &EvaluationContext,
    prevouts: &PrevoutSet,
) -> TxEvidence {
    let is_coinbase = tx.is_coinbase();

    if !ctx.rdts_active() {
        return TxEvidence {
            evaluation_mode: EvaluationMode::Consensus,
            rules_applied: false,
            is_coinbase,
            primary_violation: None,
            rules: all_pass(),
        };
    }

    evaluate_applied(tx, prevouts, Applicability::Consensus(*ctx))
}

/// Evaluate every RDTS rule as deployed Knots standard-mempool policy.
pub fn evaluate_mempool_policy(tx: &Transaction, prevouts: &PrevoutSet) -> TxEvidence {
    evaluate_applied(tx, prevouts, Applicability::MempoolPolicy)
}

/// Return every rule-1 violation in output order.
fn output_size_violations(tx: &Transaction) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (vout, txout) in tx.output.iter().enumerate() {
        let script = txout.script_pubkey.as_bytes();
        // Client: `if (txout.scriptPubKey.empty()) continue;`
        if script.is_empty() {
            continue;
        }
        let is_op_return = script[0] == OP_RETURN;
        let limit = if is_op_return {
            MAX_OUTPUT_DATA_SIZE
        } else {
            MAX_OUTPUT_SCRIPT_SIZE
        };
        if script.len() > limit {
            violations.push(Violation::OutputScriptTooLarge {
                vout,
                len: script.len(),
                is_op_return,
                limit,
            });
        }
    }
    violations
}

fn evaluate_applied(
    tx: &Transaction,
    prevouts: &PrevoutSet,
    applicability: Applicability,
) -> TxEvidence {
    let is_coinbase = tx.is_coinbase();

    // Rule 1: output-scoped, always evaluated on new outputs (incl. coinbase).
    let output_violations = output_size_violations(tx);
    let mut primary_violation =
        output_violations
            .first()
            .cloned()
            .map(|evidence| PrimaryViolation {
                rule: RuleId::OutputSize,
                evidence,
            });
    let rule1 = match output_violations {
        v if v.is_empty() => RuleVerdict::Pass,
        evidence => RuleVerdict::Violate {
            evidence,
            missing: Vec::new(),
        },
    };

    // Rules 2-7: accumulate per-rule violations and unknowns across inputs.
    let mut violations: [Vec<Violation>; 6] = Default::default();
    let mut unknowns: [Vec<Missing>; 6] = Default::default();
    let mut primary_blocked = false;

    if !is_coinbase {
        for (i, txin) in tx.input.iter().enumerate() {
            match prevouts.get(i) {
                // No prevout facts at all: cannot classify the spend, so every
                // input-scoped rule is Unknown for this input.
                None => {
                    push_unknown_all(&mut unknowns, Missing::ScriptPubKey { input: i });
                    if primary_violation.is_none() {
                        primary_blocked = true;
                    }
                }
                Some(facts) => {
                    #[cfg(test)]
                    {
                        if let Applicability::Consensus(ctx) = applicability
                            && facts
                                .creation_height
                                .is_some_and(|height| ctx.is_grandfathered(height).unwrap_or(false))
                        {
                            continue;
                        }
                    }
                    let matches =
                        script_rules::evaluate_input(i, txin, facts.script_pubkey.as_script());
                    match applicability {
                        Applicability::MempoolPolicy => record_matches(
                            matches,
                            &mut violations,
                            &mut primary_violation,
                            primary_blocked,
                        ),
                        #[cfg(test)]
                        Applicability::Consensus(_) => match facts.creation_height {
                            Some(_) => record_matches(
                                matches,
                                &mut violations,
                                &mut primary_violation,
                                primary_blocked,
                            ),
                            None => {
                                if !matches.is_empty() && primary_violation.is_none() {
                                    primary_blocked = true;
                                }
                                let mut affected_rules = [false; 6];
                                for rule_match in matches {
                                    affected_rules[rule_slot(rule_match.rule)] = true;
                                }
                                for (slot, affected) in affected_rules.into_iter().enumerate() {
                                    if affected {
                                        unknowns[slot].push(Missing::CreationHeight { input: i });
                                    }
                                }
                            }
                        },
                    }
                }
            }
        }
    }

    // Assemble rule outcomes in order 1..=7.
    let mut rules = Vec::with_capacity(7);
    rules.push(outcome(RuleId::OutputSize, rule1));
    for rule in INPUT_SCOPED {
        let slot = rule_slot(rule);
        let verdict = if !violations[slot].is_empty() {
            RuleVerdict::Violate {
                evidence: std::mem::take(&mut violations[slot]),
                missing: std::mem::take(&mut unknowns[slot]),
            }
        } else if !unknowns[slot].is_empty() {
            RuleVerdict::Unknown {
                missing: std::mem::take(&mut unknowns[slot]),
            }
        } else {
            RuleVerdict::Pass
        };
        rules.push(outcome(rule, verdict));
    }

    let evaluation_mode = match applicability {
        Applicability::MempoolPolicy => EvaluationMode::MempoolPolicy,
        #[cfg(test)]
        Applicability::Consensus(_) => EvaluationMode::Consensus,
    };
    TxEvidence {
        evaluation_mode,
        rules_applied: true,
        is_coinbase,
        primary_violation,
        rules,
    }
}

fn record_matches(
    matches: Vec<PrimaryViolation>,
    violations: &mut [Vec<Violation>; 6],
    primary_violation: &mut Option<PrimaryViolation>,
    primary_blocked: bool,
) {
    if primary_violation.is_none() && !primary_blocked {
        *primary_violation = matches.first().cloned();
    }
    for rule_match in matches {
        violations[rule_slot(rule_match.rule)].push(rule_match.evidence);
    }
}

/// Index of an input-scoped rule within the accumulator arrays.
fn rule_slot(rule: RuleId) -> usize {
    INPUT_SCOPED
        .iter()
        .position(|r| *r == rule)
        .expect("input-scoped rule")
}

/// Push a `Missing` to every input-scoped rule's unknown list.
fn push_unknown_all(unknowns: &mut [Vec<Missing>; 6], missing: Missing) {
    for slot in unknowns.iter_mut() {
        slot.push(missing.clone());
    }
}

fn outcome(rule: RuleId, verdict: RuleVerdict) -> RuleOutcome {
    RuleOutcome {
        rule,
        number: rule.number(),
        verdict,
    }
}

#[cfg(test)]
fn all_pass() -> Vec<RuleOutcome> {
    [
        RuleId::OutputSize,
        RuleId::ElementSize,
        RuleId::UndefinedVersion,
        RuleId::TaprootAnnex,
        RuleId::ControlBlockSize,
        RuleId::OpSuccess,
        RuleId::TapscriptOpIf,
    ]
    .into_iter()
    .map(|rule| outcome(rule, RuleVerdict::Pass))
    .collect()
}
