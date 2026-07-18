//! Real-transaction builders shared by in-module tests. Script builders emit
//! structurally valid script forms with placeholder hash and key bytes.

use bitcoin::{
    Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness, absolute, transaction,
};

pub fn transaction_from(input: Vec<TxIn>, output: Vec<TxOut>) -> Transaction {
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input,
        output,
    }
}

pub fn transaction_with(input_count: usize, output: Vec<TxOut>) -> Transaction {
    transaction_from(vec![bare_input(); input_count], output)
}

pub fn bare_input() -> TxIn {
    TxIn {
        previous_output: OutPoint::null(),
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness: Witness::new(),
    }
}

pub fn witness_input(elements: &[Vec<u8>]) -> TxIn {
    let mut input = bare_input();
    input.witness = Witness::from_slice(elements);
    input
}

pub fn script_sig_input(script_sig: ScriptBuf) -> TxIn {
    let mut input = bare_input();
    input.script_sig = script_sig;
    input
}

pub fn output(sats: u64, script_pubkey: ScriptBuf) -> TxOut {
    TxOut {
        value: Amount::from_sat(sats),
        script_pubkey,
    }
}

pub fn p2tr_script() -> ScriptBuf {
    witness_program(0x51, 32)
}

pub fn p2wpkh_script() -> ScriptBuf {
    witness_program(0x00, 20)
}

pub fn p2wsh_script() -> ScriptBuf {
    witness_program(0x00, 32)
}

pub fn p2sh_script() -> ScriptBuf {
    let mut bytes = vec![0xa9, 0x14];
    bytes.resize(22, 0x44);
    bytes.push(0x87);
    ScriptBuf::from_bytes(bytes)
}

pub fn p2pkh_script() -> ScriptBuf {
    let mut bytes = vec![0x76, 0xa9, 0x14];
    bytes.resize(23, 0x55);
    bytes.extend_from_slice(&[0x88, 0xac]);
    ScriptBuf::from_bytes(bytes)
}

pub fn op_return_script() -> ScriptBuf {
    ScriptBuf::new_op_return(b"atlas")
}

pub fn other_script() -> ScriptBuf {
    ScriptBuf::from_bytes(vec![0x51])
}

fn witness_program(version_opcode: u8, program_len: u8) -> ScriptBuf {
    let mut bytes = vec![version_opcode, program_len];
    bytes.resize(2 + usize::from(program_len), 0x77);
    ScriptBuf::from_bytes(bytes)
}
