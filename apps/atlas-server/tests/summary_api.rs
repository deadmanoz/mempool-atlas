//! HTTP behavior of the aggregate summary and source-listing endpoints:
//! strict query validation, honest awaiting-RPC separation, and stable
//! source discovery. Aggregation arithmetic is unit-tested in the summary
//! module; fixture parity lives in the fixture contract test.

use atlas_model::{
    AggregateBin, DimensionHistogram, Evidence, MempoolEntryFacts, MempoolSummary, NormalizedEvent,
    ReconciledMembership, SourceId, SourceSessionId, SourcesResponse,
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

fn txid(index: u64) -> String {
    format!("{index:064x}")
}

fn event(source_id: &str, sequence: u64, evidence: Evidence) -> NormalizedEvent {
    NormalizedEvent::new(
        SourceId::new(source_id).expect("source"),
        SourceSessionId::new("session-a").expect("session"),
        sequence,
        1_000 + sequence,
        1_001 + sequence,
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

fn application(events: &[NormalizedEvent]) -> (tempfile::TempDir, Router) {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store = Store::open(database).expect("open");
    for event in events {
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

#[tokio::test]
async fn summary_separates_awaiting_rpc_from_fact_bearing_totals() {
    let (_temporary, application) = application(&[
        event("source-a", 1, present(txid(1), 200, 1_700)),
        event("source-a", 2, Evidence::MempoolAdded { txid: txid(2) }),
        event("source-a", 3, Evidence::MempoolAdded { txid: txid(3) }),
    ]);

    let (status, body) = get(application, "/api/v1/sources/source-a/mempool/summary").await;
    assert_eq!(status, StatusCode::OK);
    let summary: MempoolSummary = serde_json::from_value(body).expect("summary");
    assert_eq!(summary.as_of_ms, AS_OF_MS);
    assert_eq!(
        summary.totals.all,
        AggregateBin {
            count: 1,
            vsize: 200
        }
    );
    assert_eq!(summary.totals.matching, summary.totals.all);
    assert_eq!(summary.totals.awaiting_rpc.count, 2);
    assert!(summary.ecdf.is_none());
    assert!(summary.joint_fee_size.is_none());
}

#[tokio::test]
async fn summary_applies_filters_from_query_parameters() {
    let (_temporary, application) = application(&[
        event("source-a", 1, present(txid(1), 200, 1_700)), // 8.5 sat/vB
        event("source-a", 2, present(txid(2), 800, 200)),   // 0.25 sat/vB
    ]);

    let (status, body) = get(
        application,
        "/api/v1/sources/source-a/mempool/summary?t.behavior=unknown&feerate_min=1",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let summary: MempoolSummary = serde_json::from_value(body).expect("summary");
    assert_eq!(summary.totals.all.count, 2);
    assert_eq!(
        summary.totals.matching,
        AggregateBin {
            count: 1,
            vsize: 200
        }
    );
    assert_eq!(summary.filter_echo.taxonomies.len(), 1);
    assert_eq!(summary.filter_echo.taxonomies[0].key, "behavior");
    assert_eq!(
        summary.filter_echo.taxonomies[0].verdicts,
        vec!["unknown".to_owned()]
    );
    assert_eq!(summary.filter_echo.feerate_min, Some(1.0));
}

#[tokio::test]
async fn summary_rejects_malformed_queries_with_json_errors() {
    let cases = [
        // Unknown parameters, facet values, and detail selections must never
        // silently widen or narrow a filter.
        "/api/v1/sources/source-a/mempool/summary?flavor=spicy",
        // The pre-taxonomy `class` grammar is gone, not silently ignored.
        "/api/v1/sources/source-a/mempool/summary?class=unknown",
        // Unknown taxonomy, unknown verdict, empty verdict list, and a
        // duplicated taxonomy parameter.
        "/api/v1/sources/source-a/mempool/summary?t.bogus=payment",
        "/api/v1/sources/source-a/mempool/summary?t.behavior=snazzy",
        "/api/v1/sources/source-a/mempool/summary?t.behavior=",
        "/api/v1/sources/source-a/mempool/summary?t.behavior=payment&t.behavior=data",
        "/api/v1/sources/source-a/mempool/summary?script=opreturn",
        "/api/v1/sources/source-a/mempool/summary?script=",
        "/api/v1/sources/source-a/mempool/summary?feerate_min=8&feerate_max=4",
        "/api/v1/sources/source-a/mempool/summary?feerate_min=-1",
        "/api/v1/sources/source-a/mempool/summary?feerate_min=fast",
        "/api/v1/sources/source-a/mempool/summary?detail=everything",
    ];
    for uri in cases {
        let (_temporary, application) =
            application(&[event("source-a", 1, present(txid(1), 200, 1_700))]);
        let (status, body) = get(application, uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert!(
            body.get("error").is_some_and(serde_json::Value::is_string),
            "{uri} should return a JSON error body, got {body}"
        );
    }
}

#[tokio::test]
async fn summary_carries_taxonomy_bins_and_derived_verdicts_land_in_them() {
    // A deterministic OP_RETURN-carrying transaction classifies as `data`
    // once its raw bytes are observed over P2P.
    let payload =
        bitcoin::script::PushBytesBuf::try_from(b"atlas-test".to_vec()).expect("push bytes");
    let data_transaction = Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: bitcoin::Txid::from_byte_array([0x24; 32]),
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::from_slice(&[vec![0xab; 107]]),
        }],
        output: vec![
            TxOut {
                value: Amount::from_sat(0),
                script_pubkey: ScriptBuf::new_op_return(payload),
            },
            TxOut {
                value: Amount::from_sat(120_000),
                script_pubkey: ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([0x33; 20])),
            },
        ],
    };
    let data_txid = data_transaction.compute_txid().to_string();
    let (_temporary, application) = application(&[
        event(
            "source-a",
            1,
            Evidence::P2pTransaction {
                txid: data_txid.clone(),
                wtxid: data_transaction.compute_wtxid().to_string(),
                raw_transaction_hex: Some(hex::encode(bitcoin::consensus::encode::serialize(
                    &data_transaction,
                ))),
                peer_id: Some(7),
                inbound: Some(true),
            },
        ),
        event("source-a", 2, present(data_txid, 200, 1_700)),
        event("source-a", 3, present(txid(1), 400, 800)),
    ]);

    let (status, body) = get(application, "/api/v1/sources/source-a/mempool/summary").await;
    assert_eq!(status, StatusCode::OK);
    let summary: MempoolSummary = serde_json::from_value(body).expect("summary");

    // The catalog leads with the behavior taxonomy and its verdict order
    // defines the histogram's bin order.
    let behavior = &summary.bins.taxonomies[0];
    assert_eq!(behavior.key, "behavior");
    assert_eq!(summary.histograms.taxonomies[0].key, "behavior");
    let DimensionHistogram::Available { bins, underived } =
        &summary.histograms.taxonomies[0].histogram
    else {
        panic!("behavior histogram must be available");
    };
    assert_eq!(bins.len(), behavior.verdicts.len());
    let bin_for = |verdict: &str| {
        let index = behavior
            .verdicts
            .iter()
            .position(|descriptor| descriptor.key == verdict)
            .unwrap_or_else(|| panic!("behavior taxonomy declares {verdict}"));
        bins[index]
    };
    assert_eq!(
        bin_for("data"),
        AggregateBin {
            count: 1,
            vsize: 200
        }
    );
    // The verdictless membership lands in the honest unknown bin, never in
    // the taxonomy's underived bucket.
    assert_eq!(
        bin_for("unknown"),
        AggregateBin {
            count: 1,
            vsize: 400
        }
    );
    assert_eq!(*underived, AggregateBin::default());
}

#[tokio::test]
async fn summary_carries_the_bip110_taxonomy_and_filters_on_its_verdicts() {
    // A plain 1-in/2-out P2WPKH transaction is BIP-110 `conforming` once its
    // raw bytes are observed; the raw-byte-less membership stays `unknown`.
    let conforming_transaction = Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: bitcoin::Txid::from_byte_array([0x42; 32]),
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::from_slice(&[vec![0xab; 107]]),
        }],
        output: vec![
            TxOut {
                value: Amount::from_sat(80_000),
                script_pubkey: ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([0x44; 20])),
            },
            TxOut {
                value: Amount::from_sat(15_000),
                script_pubkey: ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([0x45; 20])),
            },
        ],
    };
    let conforming_txid = conforming_transaction.compute_txid().to_string();
    let (_temporary, application) = application(&[
        event(
            "source-a",
            1,
            Evidence::P2pTransaction {
                txid: conforming_txid.clone(),
                wtxid: conforming_transaction.compute_wtxid().to_string(),
                raw_transaction_hex: Some(hex::encode(bitcoin::consensus::encode::serialize(
                    &conforming_transaction,
                ))),
                peer_id: Some(3),
                inbound: Some(true),
            },
        ),
        event("source-a", 2, present(conforming_txid, 200, 1_700)),
        event("source-a", 3, present(txid(1), 400, 800)),
    ]);

    let (status, body) = get(
        application.clone(),
        "/api/v1/sources/source-a/mempool/summary",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let summary: MempoolSummary = serde_json::from_value(body).expect("summary");

    // The bip110 taxonomy travels second, after the behavior taxonomy.
    let bip110 = &summary.bins.taxonomies[1];
    assert_eq!(bip110.key, "bip110");
    assert_eq!(summary.histograms.taxonomies[1].key, "bip110");
    let DimensionHistogram::Available { bins, underived } =
        &summary.histograms.taxonomies[1].histogram
    else {
        panic!("bip110 histogram must be available");
    };
    assert_eq!(bins.len(), bip110.verdicts.len());
    let bin_for = |verdict: &str| {
        let index = bip110
            .verdicts
            .iter()
            .position(|descriptor| descriptor.key == verdict)
            .unwrap_or_else(|| panic!("bip110 taxonomy declares {verdict}"));
        bins[index]
    };
    assert_eq!(
        bin_for("conforming"),
        AggregateBin {
            count: 1,
            vsize: 200
        }
    );
    assert_eq!(
        bin_for("unknown"),
        AggregateBin {
            count: 1,
            vsize: 400
        }
    );
    assert_eq!(*underived, AggregateBin::default());

    // The verdict filters through the taxonomy query grammar.
    let (status, body) = get(
        application,
        "/api/v1/sources/source-a/mempool/summary?t.bip110=conforming",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let summary: MempoolSummary = serde_json::from_value(body).expect("summary");
    assert_eq!(summary.totals.all.count, 2);
    assert_eq!(
        summary.totals.matching,
        AggregateBin {
            count: 1,
            vsize: 200
        }
    );
}

#[tokio::test]
async fn summary_carries_the_data_protocol_taxonomy_and_filters_on_its_verdicts() {
    // An OP_RETURN-carrying transaction with an unrecognized payload lands
    // in `op_return_other` once its raw bytes are observed; the
    // raw-byte-less membership stays `unknown`.
    let payload =
        bitcoin::script::PushBytesBuf::try_from(b"atlas-test".to_vec()).expect("push bytes");
    let carrier_transaction = Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: bitcoin::Txid::from_byte_array([0x51; 32]),
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::from_slice(&[vec![0xab; 107]]),
        }],
        output: vec![
            TxOut {
                value: Amount::from_sat(0),
                script_pubkey: ScriptBuf::new_op_return(payload),
            },
            TxOut {
                value: Amount::from_sat(120_000),
                script_pubkey: ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([0x52; 20])),
            },
        ],
    };
    let carrier_txid = carrier_transaction.compute_txid().to_string();
    let (_temporary, application) = application(&[
        event(
            "source-a",
            1,
            Evidence::P2pTransaction {
                txid: carrier_txid.clone(),
                wtxid: carrier_transaction.compute_wtxid().to_string(),
                raw_transaction_hex: Some(hex::encode(bitcoin::consensus::encode::serialize(
                    &carrier_transaction,
                ))),
                peer_id: Some(5),
                inbound: Some(true),
            },
        ),
        event("source-a", 2, present(carrier_txid, 200, 1_700)),
        event("source-a", 3, present(txid(1), 400, 800)),
    ]);

    let (status, body) = get(
        application.clone(),
        "/api/v1/sources/source-a/mempool/summary",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let summary: MempoolSummary = serde_json::from_value(body).expect("summary");

    // The data_protocol taxonomy travels third, after behavior and bip110.
    let data_protocol = &summary.bins.taxonomies[2];
    assert_eq!(data_protocol.key, "data_protocol");
    assert_eq!(summary.histograms.taxonomies[2].key, "data_protocol");
    let DimensionHistogram::Available { bins, underived } =
        &summary.histograms.taxonomies[2].histogram
    else {
        panic!("data_protocol histogram must be available");
    };
    assert_eq!(bins.len(), data_protocol.verdicts.len());
    let bin_for = |verdict: &str| {
        let index = data_protocol
            .verdicts
            .iter()
            .position(|descriptor| descriptor.key == verdict)
            .unwrap_or_else(|| panic!("data_protocol taxonomy declares {verdict}"));
        bins[index]
    };
    assert_eq!(
        bin_for("op_return_other"),
        AggregateBin {
            count: 1,
            vsize: 200
        }
    );
    assert_eq!(
        bin_for("unknown"),
        AggregateBin {
            count: 1,
            vsize: 400
        }
    );
    assert_eq!(*underived, AggregateBin::default());

    // The verdict filters through the taxonomy query grammar.
    let (status, body) = get(
        application,
        "/api/v1/sources/source-a/mempool/summary?t.data_protocol=op_return_other",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let summary: MempoolSummary = serde_json::from_value(body).expect("summary");
    assert_eq!(summary.totals.all.count, 2);
    assert_eq!(
        summary.totals.matching,
        AggregateBin {
            count: 1,
            vsize: 200
        }
    );
}

#[tokio::test]
async fn summary_for_unknown_source_is_not_found() {
    let (_temporary, application) = application(&[]);
    let (status, body) = get(application, "/api/v1/sources/absent-node/mempool/summary").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body,
        serde_json::json!({ "error": "source absent-node was not found" })
    );
}

#[tokio::test]
async fn sources_lists_membership_counts_in_stable_order() {
    let (_temporary, application) = application(&[
        event("source-b", 1, present(txid(1), 200, 400)),
        event("source-a", 1, present(txid(2), 300, 600)),
        event("source-a", 2, Evidence::MempoolAdded { txid: txid(3) }),
    ]);

    let (status, body) = get(application, "/api/v1/sources").await;
    assert_eq!(status, StatusCode::OK);
    let response: SourcesResponse = serde_json::from_value(body).expect("sources");
    let summarized: Vec<(&str, u64)> = response
        .sources
        .iter()
        .map(|source| (source.source_id.as_str(), source.membership_count))
        .collect();
    assert_eq!(summarized, vec![("source-a", 2), ("source-b", 1)]);
}

#[tokio::test]
async fn sources_is_empty_before_any_ingest() {
    let (_temporary, application) = application(&[]);
    let (status, body) = get(application, "/api/v1/sources").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({ "sources": [] }));
}
