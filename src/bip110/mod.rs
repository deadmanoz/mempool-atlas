//! Private pure evaluator for the seven BIP-110 (Reduced Data Temporary
//! Softfork, RDTS / `REDUCED_DATA`) transaction rules.
//!
//! # What this module is
//!
//! Given a [`bitcoin::Transaction`] and per-input [`PrevoutFacts`], it returns
//! an evidence-carrying [`TxEvidence`]: for each of the seven rules, `Pass`,
//! `Violate { evidence }`, or `Unknown { missing }`. Violation evidence carries
//! the byte counts, opcode positions, and depths that prove it.
//!
//! It is a **pure evaluator**: no I/O, no RPC, no async, no chain access, no
//! clock. The test-only `evaluate_consensus` path applies the deployment window
//! and per-input UTXO grandfathering. [`evaluate_mempool_policy`] mirrors the
//! deployed Knots standard-mempool policy by applying all seven rules without
//! activation gating or grandfathering.
//!
//! # The seven rules (byte-exact, from the enforcement client)
//!
//! 1. Output scriptPubKey `<= 34` bytes, or `<= 83` if it begins with
//!    `OP_RETURN`. Output-scoped: evaluated on every new output, including
//!    coinbase, even when all inputs are grandfathered.
//! 2. Pushed data / script-argument witness items `<= 256` bytes. The BIP16
//!    redeemScript push is exempt.
//! 3. Spending an undefined witness version or undefined Tapleaf version is
//!    invalid. Witness v0, Taproot, and empty-witness P2A are the recognized
//!    exceptions.
//! 4. A Taproot spend containing an annex is invalid.
//! 5. A Taproot control block is `<= 257` bytes (Merkle depth `<= 7`).
//! 6. A Tapscript containing any `OP_SUCCESS` opcode is invalid, even if
//!    execution would not reach it.
//! 7. Executing `OP_IF`/`OP_NOTIF` in Tapscript is invalid. A `0x63`/`0x64`
//!    byte merely pushed as data is not execution.
//!
//! Rules 2-7 are input/script-scoped and exempt for grandfathered inputs (an
//! input spending a UTXO created strictly below the branch activation height).
//! Rule 1 is never grandfathered.
//!
//! # Provenance
//!
//! Semantics are mirrored from the enforcement client
//! `bitcoin-bip110-client` at commit
//! `f41f01e1e6de7025d52a865bef97f2a67277f0f3`, the exact Bitcoin Knots commit
//! deployed on the RDTS vantage. Every constant cites its client source line.
//!
//! # Deliberate non-goals
//!
//! This module does not access a chain or UTXO set and does not independently
//! prove ordinary script validity. It is intended to classify transactions
//! already admitted to a Bitcoin Core mempool, where P2WSH commitments,
//! P2WPKH witness shape, control-block form, and earlier script checks have
//! already succeeded. It does not recompute the Taproot commitment or
//! independently validate P2SH and witness commitments. Callers evaluating
//! arbitrary, unvalidated transactions must supply an equivalent validity
//! boundary before relying on attribution.

mod context;
mod prevout;
mod rules;
mod script_scan;
mod verdict;
mod witness;

pub use prevout::{PrevoutFacts, PrevoutSet};
pub use verdict::{Missing, RuleId, RuleVerdict, TxEvidence, Violation};

#[cfg(test)]
pub use context::{ACTIVE_DURATION, EvaluationContext};
#[cfg(test)]
pub use verdict::{EvaluationMode, RuleOutcome, ScriptKind};

/// Evaluate the BIP-110 consensus rules for a transaction.
///
/// The rules are applied only inside `ctx`'s activation window. Rules 2 through
/// 7 ignore inputs spending UTXOs created before the activation height.
#[cfg(test)]
pub fn evaluate_consensus(
    tx: &bitcoin::Transaction,
    ctx: &EvaluationContext,
    prevouts: &PrevoutSet,
) -> TxEvidence {
    rules::evaluate_consensus(tx, ctx, prevouts)
}

/// Evaluate the RDTS rules as deployed Bitcoin Knots mempool policy.
///
/// All seven rules are applied regardless of deployment state. Input creation
/// heights are intentionally ignored because Knots standard policy does not
/// apply the consensus UTXO-grandfathering exemption.
pub fn evaluate_mempool_policy(tx: &bitcoin::Transaction, prevouts: &PrevoutSet) -> TxEvidence {
    rules::evaluate_mempool_policy(tx, prevouts)
}

#[cfg(test)]
mod tests;
