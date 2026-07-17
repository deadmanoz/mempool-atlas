//! Structural shape facts derived from one parsed transaction.

use atlas_model::ScriptType;
use bitcoin::{Script, Transaction};

/// Shape facts shared by shape-driven rule packs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransactionShape {
    pub total_output_sats: u64,
    pub input_count: u64,
    pub output_count: u64,
    /// The canonical script type chosen by the dominance rule documented on
    /// [`Self::derive`].
    pub script_type: ScriptType,
}

impl TransactionShape {
    /// Derives shape facts from `transaction`.
    ///
    /// The canonical `script_type` of a mixed-type transaction is a
    /// wire-visible contract: the output type with the greatest output count
    /// is dominant, ties break toward the type with the larger summed output
    /// value, and remaining ties break toward the earlier position in
    /// [`ScriptType::ALL`]. A zero-value `OP_RETURN` output therefore never
    /// outranks an equally counted payment output.
    #[must_use]
    pub fn derive(transaction: &Transaction) -> Self {
        let mut counts = [0_u64; ScriptType::ALL.len()];
        let mut value_sums = [0_u64; ScriptType::ALL.len()];
        for output in &transaction.output {
            let index = output_script_type(&output.script_pubkey).index();
            counts[index] += 1;
            value_sums[index] += output.value.to_sat();
        }

        let mut script_type = ScriptType::ALL[0];
        for candidate in ScriptType::ALL {
            if (counts[candidate.index()], value_sums[candidate.index()])
                > (counts[script_type.index()], value_sums[script_type.index()])
            {
                script_type = candidate;
            }
        }

        Self {
            total_output_sats: value_sums.iter().sum(),
            input_count: transaction.input.len() as u64,
            output_count: transaction.output.len() as u64,
            script_type,
        }
    }
}

fn output_script_type(script: &Script) -> ScriptType {
    if script.is_p2tr() {
        ScriptType::P2tr
    } else if script.is_p2wpkh() {
        ScriptType::P2wpkh
    } else if script.is_p2wsh() {
        ScriptType::P2wsh
    } else if script.is_p2sh() {
        ScriptType::P2sh
    } else if script.is_p2pkh() {
        ScriptType::P2pkh
    } else if script.is_op_return() {
        ScriptType::OpReturn
    } else {
        ScriptType::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        op_return_script, other_script, output, p2pkh_script, p2sh_script, p2tr_script,
        p2wpkh_script, p2wsh_script, transaction_with,
    };

    #[test]
    fn derive_counts_inputs_outputs_and_sums_output_value() {
        let transaction = transaction_with(
            2,
            vec![
                output(1_000, p2wpkh_script()),
                output(2_500, p2wpkh_script()),
                output(0, op_return_script()),
            ],
        );
        assert_eq!(
            TransactionShape::derive(&transaction),
            TransactionShape {
                total_output_sats: 3_500,
                input_count: 2,
                output_count: 3,
                script_type: ScriptType::P2wpkh,
            }
        );
    }

    #[test]
    fn every_recognized_output_form_maps_to_its_facet() {
        for (script, expected) in [
            (p2tr_script(), ScriptType::P2tr),
            (p2wpkh_script(), ScriptType::P2wpkh),
            (p2wsh_script(), ScriptType::P2wsh),
            (p2sh_script(), ScriptType::P2sh),
            (p2pkh_script(), ScriptType::P2pkh),
            (op_return_script(), ScriptType::OpReturn),
            (other_script(), ScriptType::Other),
        ] {
            let transaction = transaction_with(1, vec![output(1_000, script)]);
            assert_eq!(TransactionShape::derive(&transaction).script_type, expected);
        }
    }

    #[test]
    fn dominance_prefers_the_most_common_output_type() {
        let transaction = transaction_with(
            1,
            vec![
                output(100, p2sh_script()),
                output(100, p2sh_script()),
                output(100, p2sh_script()),
                output(1_000_000, p2tr_script()),
            ],
        );
        assert_eq!(
            TransactionShape::derive(&transaction).script_type,
            ScriptType::P2sh
        );
    }

    #[test]
    fn summed_value_breaks_count_ties() {
        let transaction = transaction_with(
            1,
            vec![output(100, p2tr_script()), output(900, p2pkh_script())],
        );
        assert_eq!(
            TransactionShape::derive(&transaction).script_type,
            ScriptType::P2pkh
        );
    }

    #[test]
    fn zero_value_op_return_defers_to_the_payment_output() {
        let transaction = transaction_with(
            1,
            vec![output(0, op_return_script()), output(700, p2wpkh_script())],
        );
        assert_eq!(
            TransactionShape::derive(&transaction).script_type,
            ScriptType::P2wpkh
        );
    }

    #[test]
    fn catalog_order_breaks_remaining_ties() {
        let transaction = transaction_with(
            1,
            vec![output(500, p2wpkh_script()), output(500, p2tr_script())],
        );
        assert_eq!(
            TransactionShape::derive(&transaction).script_type,
            ScriptType::P2tr
        );
    }
}
