use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use bitcoin::absolute;
use bitcoin::hashes::Hash;
use bitcoin::transaction::Version;
use bitcoin::{Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness};

use super::*;

fn outpoint(marker: u8) -> OutPoint {
    OutPoint {
        txid: Txid::from_byte_array([marker; 32]),
        vout: 0,
    }
}

fn transaction_spending(previous_output: OutPoint, value: u64) -> Transaction {
    Transaction {
        version: Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output,
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::from_slice(&[vec![1], vec![2]]),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(value),
            script_pubkey: script(),
        }],
    }
}

fn ready_pending_slice(
    transactions: Vec<Transaction>,
    scripts: impl IntoIterator<Item = (OutPoint, ScriptBuf)>,
) -> PendingSlice {
    let mut active = BTreeMap::new();
    for transaction in transactions {
        let expected = ExpectedVariant {
            txid: transaction.compute_txid().to_string(),
            wtxid: transaction.compute_wtxid().to_string(),
            vsize: u64::try_from(transaction.vsize()).expect("test transaction vsize"),
        };
        let key = expected.wtxid.clone();
        active.insert(
            key,
            PendingCandidate {
                expected,
                required: transaction
                    .input
                    .iter()
                    .map(|input| input.previous_output)
                    .collect(),
                transaction: Arc::new(transaction),
                fact_cursor: 0,
            },
        );
    }
    let fair_order = active.keys().cloned().collect::<VecDeque<_>>();
    let facts = scripts
        .into_iter()
        .map(|(outpoint, script)| (outpoint, FactState::Ready(script)))
        .collect();
    let mut pending = PendingSlice {
        queued: VecDeque::new(),
        active,
        ready: VecDeque::new(),
        fair_order,
        facts,
        deferred_facts: BTreeSet::new(),
        raw_outputs: BTreeMap::new(),
        pending_script_bytes: 0,
    };
    pending.recount_script_bytes();
    pending
}

fn script() -> ScriptBuf {
    ScriptBuf::from_bytes([0x00, 0x14].into_iter().chain([0_u8; 20]).collect())
}

#[test]
fn ready_chunk_respects_transaction_count_and_byte_caps() {
    let outpoints = [outpoint(71), outpoint(72), outpoint(73)];
    let transactions = outpoints
        .iter()
        .enumerate()
        .map(|(index, outpoint)| transaction_spending(*outpoint, 500 + index as u64))
        .collect::<Vec<_>>();
    let mut expected = transactions
        .iter()
        .map(|transaction| transaction.compute_wtxid().to_string())
        .collect::<Vec<_>>();
    expected.sort();
    let scripts = outpoints
        .iter()
        .map(|outpoint| (*outpoint, script()))
        .collect::<Vec<_>>();

    let mut by_count = ready_pending_slice(transactions.clone(), scripts.clone());
    let (first, ready_remaining) =
        by_count.classify_ready_chunk(ClassificationChunkLimits::new(2, usize::MAX));
    assert_eq!(
        first
            .iter()
            .map(|cached| cached.classification.wtxid.clone())
            .collect::<Vec<_>>(),
        expected[..2]
    );
    assert!(ready_remaining);
    let (second, ready_remaining) =
        by_count.classify_ready_chunk(ClassificationChunkLimits::new(2, usize::MAX));
    assert_eq!(second[0].classification.wtxid, expected[2]);
    assert!(!ready_remaining);
    assert!(by_count.is_empty());

    let one_transaction_bytes = transactions[0].total_size();
    let mut by_bytes = ready_pending_slice(transactions, scripts);
    let (first, ready_remaining) =
        by_bytes.classify_ready_chunk(ClassificationChunkLimits::new(3, one_transaction_bytes));
    assert_eq!(first.len(), 1);
    assert!(ready_remaining);
}

#[test]
fn ready_chunk_retains_shared_facts_until_the_last_consumer() {
    let shared = outpoint(74);
    let first = transaction_spending(shared, 500);
    let second = transaction_spending(shared, 501);
    let script = script();
    let script_bytes = estimated_script_bytes(&script);
    let mut pending = ready_pending_slice(vec![first, second], [(shared, script)]);

    let (first, ready_remaining) =
        pending.classify_ready_chunk(ClassificationChunkLimits::new(1, usize::MAX));
    assert_eq!(first.len(), 1);
    assert!(ready_remaining);
    assert!(matches!(
        pending.facts.get(&shared),
        Some(FactState::Ready(_))
    ));
    assert_eq!(pending.pending_script_bytes, script_bytes);

    let (second, ready_remaining) =
        pending.classify_ready_chunk(ClassificationChunkLimits::new(1, usize::MAX));
    assert_eq!(second.len(), 1);
    assert!(!ready_remaining);
    assert!(!pending.facts.contains_key(&shared));
    assert_eq!(pending.pending_script_bytes, 0);
    assert!(pending.is_empty());
}
