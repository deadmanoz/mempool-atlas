//! Rule 1: output scriptPubKey size.
//!
//! Output-scoped: evaluated on every new output while RDTS is active, including
//! coinbase outputs and outputs of transactions all of whose inputs are
//! grandfathered. Never `Unknown` (needs no prevout facts).
//!
//! Client: `src/consensus/tx_verify.cpp:164` `CheckOutputSizes`. An empty
//! scriptPubKey is skipped; otherwise the limit is 83 bytes when the first byte
//! is `OP_RETURN`, else 34 bytes; the check is strict `>`.

use crate::context::{MAX_OUTPUT_DATA_SIZE, MAX_OUTPUT_SCRIPT_SIZE, OP_RETURN};
use crate::verdict::Violation;
use bitcoin::Transaction;

/// Every rule-1 violation in `tx`, in output order.
pub fn violations(tx: &Transaction) -> Vec<Violation> {
    let mut out = Vec::new();
    for (vout, txout) in tx.output.iter().enumerate() {
        let spk = txout.script_pubkey.as_bytes();
        // Client: `if (txout.scriptPubKey.empty()) continue;`
        if spk.is_empty() {
            continue;
        }
        let is_op_return = spk[0] == OP_RETURN;
        let limit = if is_op_return {
            MAX_OUTPUT_DATA_SIZE
        } else {
            MAX_OUTPUT_SCRIPT_SIZE
        };
        if spk.len() > limit {
            out.push(Violation::OutputScriptTooLarge {
                vout,
                len: spk.len(),
                is_op_return,
                limit,
            });
        }
    }
    out
}
