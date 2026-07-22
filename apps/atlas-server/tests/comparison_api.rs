//! HTTP behavior of the read-time source comparison endpoint: strict query
//! validation, the ordered staged deltas, honest awaiting-RPC separation, and
//! correct taxonomy attribution of the `added` regions. Region aggregation
//! arithmetic is shared with the summary engine and unit-tested there; fixture
//! parity lives in the fixture contract test.

use atlas_model::{
    AggregateBin, DimensionHistogram, Evidence, MempoolEntryFacts, NormalizedEvent,
    ReconciledMembership, SourceComparison, SourceId, SourceSessionId,
};
use atlas_server::{Store, router_with_clock};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use bitcoin::absolute::LockTime;
use bitcoin::hashes::Hash;
use bitcoin::transaction::Version;
use bitcoin::{
    Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, WPubkeyHash, Witness,
};
use tower::ServiceExt;

const AS_OF_MS: u64 = 1_752_710_400_000;
const MINUTE_MS: u64 = 60_000;

fn txid(index: u64) -> String {
    format!("{index:064x}")
}

fn raw_input(byte: u8) -> TxIn {
    TxIn {
        previous_output: OutPoint {
            txid: bitcoin::Txid::from_byte_array([byte; 32]),
            vout: 0,
        },
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness: Witness::from_slice(&[vec![0xab; 107]]),
    }
}

fn p2wpkh_output(byte: u8, sats: u64) -> TxOut {
    TxOut {
        value: Amount::from_sat(sats),
        script_pubkey: ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([byte; 20])),
    }
}

/// A deterministic 1-in/2-out P2WPKH transaction; classifies as `payment`.
fn payment_tx(byte: u8) -> Transaction {
    Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![raw_input(byte)],
        output: vec![
            p2wpkh_output(byte.wrapping_add(1), 2_000_000),
            p2wpkh_output(byte.wrapping_add(2), 500_000),
        ],
    }
}

/// A deterministic OP_RETURN carrier; classifies as behavior `data` and
/// data_protocol `op_return_other`.
fn data_tx(byte: u8, payload: &[u8]) -> Transaction {
    let payload = bitcoin::script::PushBytesBuf::try_from(payload.to_vec()).expect("push bytes");
    Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![raw_input(byte)],
        output: vec![
            TxOut {
                value: Amount::from_sat(0),
                script_pubkey: ScriptBuf::new_op_return(payload),
            },
            p2wpkh_output(byte.wrapping_add(1), 90_000),
        ],
    }
}

fn event(source_id: &str, session: &str, sequence: u64, evidence: Evidence) -> NormalizedEvent {
    NormalizedEvent::new(
        SourceId::new(source_id).expect("source"),
        SourceSessionId::new(session).expect("session"),
        sequence,
        AS_OF_MS - 3_600_000 + sequence * MINUTE_MS,
        AS_OF_MS - 3_600_000 + sequence * MINUTE_MS + 5,
        evidence,
    )
    .expect("event")
}

fn present(txid: String, vsize: u64, fee_sats: u64) -> Evidence {
    Evidence::MempoolReconciled {
        txid,
        membership: ReconciledMembership::Present {
            facts: MempoolEntryFacts {
                vsize,
                fee_sats,
                entered_at_ms: AS_OF_MS - 300_000,
            },
        },
    }
}

fn p2p(transaction: &Transaction) -> Evidence {
    Evidence::P2pTransaction {
        txid: transaction.compute_txid().to_string(),
        wtxid: transaction.compute_wtxid().to_string(),
        raw_transaction_hex: Some(hex::encode(bitcoin::consensus::encode::serialize(
            transaction,
        ))),
        peer_id: Some(9),
        inbound: Some(true),
    }
}

/// A three-source staged fork over `strict` (subset), `mid`, and `loose`
/// (superset):
/// - `pay` is shared by all three.
/// - `shared_await` is a member of all three but awaits RPC facts in `loose`.
/// - `mid_data` is relayed by `mid` and `loose` but filtered by `strict`
///   (added at the strict -> mid stage).
/// - `loose_data` is relayed only by `loose` (added at the mid -> loose stage).
/// - `strict_only` is carried by `strict` alone, so the strict -> mid stage has
///   a non-empty reverse anomaly while the mid -> loose stage's is empty.
fn fork_store() -> (tempfile::TempDir, Router) {
    let pay = payment_tx(0x11);
    let mid_data = data_tx(0x22, b"mid-data");
    let loose_data = data_tx(0x33, b"loose-data");
    let pay_txid = pay.compute_txid().to_string();
    let mid_data_txid = mid_data.compute_txid().to_string();
    let loose_data_txid = loose_data.compute_txid().to_string();
    let shared_await_txid = txid(0xa1);
    let strict_only_txid = txid(0xb2);

    let events = vec![
        // strict: the shared payment, the shared awaiting member (present here),
        // and its own transaction the more-permissive sources never saw.
        event(
            "strict",
            "strict-s",
            1,
            present(pay_txid.clone(), 200, 1_800),
        ),
        event(
            "strict",
            "strict-s",
            2,
            present(shared_await_txid.clone(), 180, 900),
        ),
        event("strict", "strict-s", 3, present(strict_only_txid, 210, 700)),
        // mid: the shared payment, the shared awaiting member, and the data
        // carrier strict filtered out.
        event("mid", "mid-s", 1, present(pay_txid.clone(), 200, 2_000)),
        event(
            "mid",
            "mid-s",
            2,
            present(shared_await_txid.clone(), 180, 950),
        ),
        event(
            "mid",
            "mid-s",
            3,
            present(mid_data_txid.clone(), 600, 6_000),
        ),
        // loose: observes every shared and added tx's bytes so classification
        // derives, holds the shared awaiting member without RPC facts, and
        // additionally relays loose_data.
        event("loose", "loose-s", 1, p2p(&pay)),
        event("loose", "loose-s", 2, present(pay_txid, 200, 2_100)),
        event("loose", "loose-s", 3, p2p(&mid_data)),
        event("loose", "loose-s", 4, present(mid_data_txid, 600, 6_200)),
        event("loose", "loose-s", 5, p2p(&loose_data)),
        event("loose", "loose-s", 6, present(loose_data_txid, 550, 5_500)),
        event(
            "loose",
            "loose-s",
            7,
            Evidence::MempoolAdded {
                txid: shared_await_txid,
            },
        ),
    ];

    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store = Store::open(database).expect("open");
    for event in &events {
        store.ingest(event).expect("ingest");
    }
    (temporary, router_with_clock(store, || AS_OF_MS))
}

async fn get(application: Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let response = application
        .oneshot(Request::get(uri).body(Body::empty()).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).expect("json body"))
}

fn behavior_bins(region_histograms: &atlas_model::SummaryHistograms) -> &[AggregateBin] {
    taxonomy_bins(region_histograms, 0)
}

fn bin_for(comparison: &SourceComparison, taxonomy_index: usize, verdict: &str) -> usize {
    comparison.shared.bins.taxonomies[taxonomy_index]
        .verdicts
        .iter()
        .position(|descriptor| descriptor.key == verdict)
        .unwrap_or_else(|| panic!("taxonomy declares {verdict}"))
}

fn taxonomy_bins(histograms: &atlas_model::SummaryHistograms, index: usize) -> &[AggregateBin] {
    match &histograms.taxonomies[index].histogram {
        DimensionHistogram::Available { bins, .. } => bins,
        DimensionHistogram::Unavailable { .. } => panic!("taxonomy histogram must be available"),
    }
}

#[tokio::test]
async fn three_source_comparison_reports_staged_deltas_and_shared_intersection() {
    let (_temporary, application) = fork_store();
    let (status, body) = get(
        application,
        "/api/v1/sources/compare?sources=strict,mid,loose",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let comparison: SourceComparison = serde_json::from_value(body).expect("comparison");

    // The request order is echoed verbatim.
    assert_eq!(
        comparison
            .sources
            .iter()
            .map(SourceId::as_str)
            .collect::<Vec<_>>(),
        vec!["strict", "mid", "loose"]
    );
    assert_eq!(comparison.as_of_ms, AS_OF_MS);

    // The shared intersection is the shared payment (present) plus the shared
    // awaiting-RPC member, which is counted separately and never given a vsize.
    assert_eq!(
        comparison.shared.present,
        AggregateBin {
            count: 1,
            vsize: 200
        }
    );
    assert_eq!(comparison.shared.awaiting_rpc.count, 1);
    let shared_behavior = behavior_bins(&comparison.shared.histograms);
    assert_eq!(shared_behavior[bin_for(&comparison, 0, "payment")].count, 1);

    // Per-source totals accompany the derived regions, in request order.
    let totals: Vec<(&str, u64, u64)> = comparison
        .source_totals
        .iter()
        .map(|total| {
            (
                total.source_id.as_str(),
                total.present.count,
                total.awaiting_rpc.count,
            )
        })
        .collect();
    assert_eq!(
        totals,
        vec![("strict", 3, 0), ("mid", 3, 0), ("loose", 3, 1)]
    );

    assert_eq!(comparison.stages.len(), 2);

    // Stage 1: mid relays the data carrier strict filters. The added region's
    // taxonomy attribution puts it in the behavior `data` bin and the
    // data_protocol `op_return_other` bin, and the reverse anomaly (strict's
    // own transaction) is present and non-zero.
    let first = &comparison.stages[0];
    assert_eq!(first.from.as_str(), "strict");
    assert_eq!(first.to.as_str(), "mid");
    assert_eq!(
        first.added.present,
        AggregateBin {
            count: 1,
            vsize: 600
        }
    );
    let added_behavior = taxonomy_bins(&first.added.histograms, 0);
    assert_eq!(added_behavior[bin_for(&comparison, 0, "data")].count, 1);
    let added_protocol = taxonomy_bins(&first.added.histograms, 2);
    assert_eq!(
        added_protocol[bin_for(&comparison, 2, "op_return_other")].count,
        1
    );
    assert_eq!(
        first.anomaly.present,
        AggregateBin {
            count: 1,
            vsize: 210
        }
    );
    assert_eq!(first.anomaly.awaiting_rpc.count, 0);

    // Stage 2: loose adds its own data carrier; mid carries nothing loose lacks,
    // so the reverse anomaly is empty under the clean nesting.
    let second = &comparison.stages[1];
    assert_eq!(second.from.as_str(), "mid");
    assert_eq!(second.to.as_str(), "loose");
    assert_eq!(
        second.added.present,
        AggregateBin {
            count: 1,
            vsize: 550
        }
    );
    let second_added_behavior = taxonomy_bins(&second.added.histograms, 0);
    assert_eq!(
        second_added_behavior[bin_for(&comparison, 0, "data")].count,
        1
    );
    assert_eq!(second.anomaly.present, AggregateBin::default());
    assert_eq!(second.anomaly.awaiting_rpc.count, 0);
}

#[tokio::test]
async fn two_source_comparison_has_one_stage_over_the_leading_pair() {
    let (_temporary, application) = fork_store();
    let (status, body) = get(application, "/api/v1/sources/compare?sources=strict,mid").await;
    assert_eq!(status, StatusCode::OK);
    let comparison: SourceComparison = serde_json::from_value(body).expect("comparison");

    assert_eq!(
        comparison
            .sources
            .iter()
            .map(SourceId::as_str)
            .collect::<Vec<_>>(),
        vec!["strict", "mid"]
    );
    // strict and mid both carry the payment and the shared awaiting member with
    // facts, so the intersection has two present members and none awaiting.
    assert_eq!(
        comparison.shared.present,
        AggregateBin {
            count: 2,
            vsize: 380
        }
    );
    assert_eq!(comparison.shared.awaiting_rpc.count, 0);

    assert_eq!(comparison.stages.len(), 1);
    let stage = &comparison.stages[0];
    assert_eq!(stage.from.as_str(), "strict");
    assert_eq!(stage.to.as_str(), "mid");
    assert_eq!(stage.added.present.count, 1);
    let added_behavior = taxonomy_bins(&stage.added.histograms, 0);
    assert_eq!(added_behavior[bin_for(&comparison, 0, "data")].count, 1);
    assert_eq!(stage.anomaly.present.count, 1);
}

#[tokio::test]
async fn unknown_source_in_the_list_is_not_found_and_names_it() {
    let (_temporary, application) = fork_store();
    let (status, body) = get(
        application,
        "/api/v1/sources/compare?sources=strict,ghost-node",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body,
        serde_json::json!({ "error": "source ghost-node was not found" })
    );
}

#[tokio::test]
async fn malformed_comparison_queries_are_bad_requests() {
    let cases = [
        // Fewer than two sources.
        "/api/v1/sources/compare?sources=strict",
        // More than four sources (a count error before any existence check).
        "/api/v1/sources/compare?sources=a,b,c,d,e",
        // A duplicated source.
        "/api/v1/sources/compare?sources=strict,strict",
        // An invalid source ID.
        "/api/v1/sources/compare?sources=strict,bad%2Fsource",
        // An empty source token.
        "/api/v1/sources/compare?sources=strict,",
        // A missing sources parameter.
        "/api/v1/sources/compare",
        // An unknown parameter must never be silently ignored.
        "/api/v1/sources/compare?sources=strict,mid&extra=1",
        // A repeated sources parameter.
        "/api/v1/sources/compare?sources=strict,mid&sources=loose",
    ];
    for uri in cases {
        let (_temporary, application) = fork_store();
        let (status, body) = get(application, uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert!(
            body.get("error").is_some_and(serde_json::Value::is_string),
            "{uri} should return a JSON error body, got {body}"
        );
    }
}
