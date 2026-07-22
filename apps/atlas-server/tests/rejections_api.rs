//! HTTP behavior of the source-scoped rejection read surface: the bounded
//! aggregate window, best-effort classification attribution, reason counting
//! with an `other` rollup, cursor pagination, strict query validation, and the
//! honesty invariant that a rejection is never conflated with a removal.

use atlas_model::{
    Evidence, NormalizedEvent, RejectionAvailability, SourceId, SourceRejections, SourceSessionId,
};
use atlas_server::{Store, experimental_evidence_router_with_clock, router_with_clock};
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

mod support;

use support::install_checkpoint;

const AS_OF_MS: u64 = 1_752_710_400_000;

fn txid(index: u64) -> String {
    format!("{index:064x}")
}

/// A rejection event at an explicit observation time, so ordering and cursor
/// tiebreaks are exercised deterministically.
fn reject_at(sequence: u64, observed_at_ms: u64, txid: String, reason: &str) -> NormalizedEvent {
    event_at(
        sequence,
        observed_at_ms,
        Evidence::MempoolRejected {
            txid,
            reason: reason.to_owned(),
        },
    )
}

fn event_at(sequence: u64, observed_at_ms: u64, evidence: Evidence) -> NormalizedEvent {
    NormalizedEvent::new(
        SourceId::new("source-a").expect("source"),
        SourceSessionId::new("session-a").expect("session"),
        sequence,
        observed_at_ms,
        observed_at_ms + 1,
        evidence,
    )
    .expect("event")
}

/// A deterministic transaction carrying an OP_RETURN output; classifies as
/// behavior `data`, bip110 `conforming`, data_protocol `op_return_other`.
fn data_transaction() -> Transaction {
    let payload =
        bitcoin::script::PushBytesBuf::try_from(b"atlas-rejected".to_vec()).expect("push bytes");
    Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: bitcoin::Txid::from_byte_array([0x71; 32]),
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
                value: Amount::from_sat(90_000),
                script_pubkey: ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([0x34; 20])),
            },
        ],
    }
}

fn p2p_evidence(transaction: &Transaction) -> Evidence {
    Evidence::P2pTransaction {
        txid: transaction.compute_txid().to_string(),
        wtxid: transaction.compute_wtxid().to_string(),
        raw_transaction_hex: Some(hex::encode(bitcoin::consensus::encode::serialize(
            transaction,
        ))),
        peer_id: Some(7),
        inbound: Some(true),
    }
}

fn application(events: &[NormalizedEvent], establish_source: bool) -> (tempfile::TempDir, Router) {
    application_with_mode(events, establish_source, true)
}

fn production_application(
    events: &[NormalizedEvent],
    establish_source: bool,
) -> (tempfile::TempDir, Router) {
    application_with_mode(events, establish_source, false)
}

fn application_with_mode(
    events: &[NormalizedEvent],
    establish_source: bool,
    experimental_evidence: bool,
) -> (tempfile::TempDir, Router) {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store = Store::open(database).expect("open");
    for event in events {
        store.ingest(event).expect("ingest");
    }
    if establish_source {
        install_checkpoint(&store, "source-a", AS_OF_MS - 1_000, vec![]);
    }
    let application = if experimental_evidence {
        experimental_evidence_router_with_clock(store, || AS_OF_MS)
    } else {
        router_with_clock(store, || AS_OF_MS)
    };
    (temporary, application)
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

fn rejections_of(body: serde_json::Value) -> SourceRejections {
    serde_json::from_value(body).expect("rejections")
}

#[tokio::test]
async fn known_source_without_rejections_has_an_empty_window() {
    let (_temporary, application) = application(&[], true);

    let (status, body) = get(application, "/api/v1/sources/source-a/rejections").await;
    assert_eq!(status, StatusCode::OK);
    let rejections = rejections_of(body);
    assert_eq!(rejections.as_of_ms, AS_OF_MS);
    assert_eq!(rejections.availability, RejectionAvailability::Available);
    assert_eq!(rejections.window.count, 0);
    assert_eq!(rejections.window.oldest_at_ms, None);
    assert_eq!(rejections.window.newest_at_ms, None);
    assert!(rejections.by_reason.is_empty());
    assert_eq!(rejections.attribution.classified_count, 0);
    assert_eq!(rejections.attribution.unclassified_count, 0);
    assert!(rejections.recent.is_empty());
    assert_eq!(rejections.next_cursor, None);
}

#[tokio::test]
async fn production_state_reports_rejection_evidence_as_not_collected() {
    let (_temporary, application) = production_application(
        &[reject_at(
            1,
            100,
            txid(1),
            "legacy evidence must stay hidden",
        )],
        true,
    );

    let (status, body) = get(application, "/api/v1/sources/source-a/rejections").await;
    assert_eq!(status, StatusCode::OK);
    let rejections = rejections_of(body);
    assert_eq!(rejections.availability, RejectionAvailability::NotCollected);
    assert_eq!(rejections.window.count, 0);
    assert!(rejections.by_reason.is_empty());
    assert!(rejections.attribution.taxonomies.is_empty());
    assert!(rejections.recent.is_empty());
    assert_eq!(rejections.next_cursor, None);
}

#[tokio::test]
async fn rejections_for_unknown_source_are_not_found() {
    let (_temporary, application) = application(&[], false);
    let (status, body) = get(application, "/api/v1/sources/absent-node/rejections").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body,
        serde_json::json!({ "error": "source absent-node was not found" })
    );
}

#[tokio::test]
async fn attribution_partitions_classified_and_unclassified_refusals() {
    let data = data_transaction();
    let data_txid = data.compute_txid().to_string();
    let (_temporary, application) = application(
        &[
            // The classified refusal is seen over P2P first, so its bytes derive
            // shape and verdicts even though it never enters membership.
            event_at(1, 50, p2p_evidence(&data)),
            reject_at(2, 200, data_txid.clone(), "min relay fee not met"),
            // The unclassified refusal is refused before any bytes are seen.
            reject_at(3, 100, txid(9), "insufficient fee"),
        ],
        true,
    );

    let (status, body) = get(application, "/api/v1/sources/source-a/rejections").await;
    assert_eq!(status, StatusCode::OK);
    let rejections = rejections_of(body);

    assert_eq!(rejections.availability, RejectionAvailability::Available);
    assert_eq!(rejections.window.count, 2);
    assert_eq!(rejections.attribution.classified_count, 1);
    assert_eq!(rejections.attribution.unclassified_count, 1);

    // The classified refusal contributes exactly one verdict per taxonomy.
    let behavior = &rejections.attribution.taxonomies[0];
    assert_eq!(behavior.key, "behavior");
    let behavior_data = behavior
        .verdicts
        .iter()
        .find(|entry| entry.verdict == "data")
        .expect("behavior declares data");
    assert_eq!(behavior_data.count, 1);
    let behavior_total: u64 = behavior.verdicts.iter().map(|entry| entry.count).sum();
    assert_eq!(behavior_total, 1);

    // Newest first: the classified refusal (t=200) then the unclassified one.
    assert_eq!(rejections.recent.len(), 2);
    assert_eq!(rejections.recent[0].txid, data_txid);
    assert!(!rejections.recent[0].verdicts.is_empty());
    assert_eq!(
        rejections.recent[0].evidence_event_id,
        "source-a/session-a/2"
    );
    assert_eq!(rejections.recent[1].txid, txid(9));
    assert!(rejections.recent[1].verdicts.is_empty());
}

#[tokio::test]
async fn by_reason_distinguishes_literal_other_from_the_overflow_rollup() {
    let mut events = Vec::new();
    let mut sequence = 0;
    let mut observed = 1_000;
    let mut push = |reason: &str, times: usize, events: &mut Vec<NormalizedEvent>| {
        for _ in 0..times {
            sequence += 1;
            observed += 1;
            events.push(reject_at(sequence, observed, txid(sequence), reason));
        }
    };
    // The node-provided literal `other` remains distinct from the synthetic
    // rollup over excess reason strings.
    push("zzz-hot-reason", 5, &mut events);
    push("other", 4, &mut events);
    for index in 0..14 {
        push(&format!("reason-{index:02}"), 1, &mut events);
    }
    let (_temporary, application) = application(&events, true);

    let (status, body) = get(application, "/api/v1/sources/source-a/rejections").await;
    assert_eq!(status, StatusCode::OK);
    let rejections = rejections_of(body);

    assert_eq!(rejections.window.count, 23);
    // Twelve distinct reasons plus the trailing other bucket.
    assert_eq!(rejections.by_reason.len(), 13);
    assert_eq!(rejections.by_reason[0].reason, "zzz-hot-reason");
    assert_eq!(rejections.by_reason[0].count, 5);
    assert!(!rejections.by_reason[0].is_rollup);
    assert_eq!(rejections.by_reason[1].reason, "other");
    assert_eq!(rejections.by_reason[1].count, 4);
    assert!(!rejections.by_reason[1].is_rollup);
    let last = rejections.by_reason.last().expect("other bucket");
    assert_eq!(last.reason, "other");
    assert_eq!(last.count, 4);
    assert!(last.is_rollup);
}

#[tokio::test]
async fn pagination_walks_the_cursor_to_exhaustion() {
    // Times 100 and 200 are shared, so the event_id tiebreak is exercised.
    let (_temporary, application) = application(
        &[
            reject_at(1, 100, txid(1), "dust"),
            reject_at(2, 100, txid(2), "dust"),
            reject_at(3, 200, txid(3), "dust"),
            reject_at(4, 200, txid(4), "dust"),
            reject_at(5, 300, txid(5), "dust"),
        ],
        true,
    );

    // Newest first: (300,5), (200,4), (200,3), (100,2), (100,1).
    let (status, body) = get(
        application.clone(),
        "/api/v1/sources/source-a/rejections?limit=3",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let page_one = rejections_of(body);
    let page_one_txids: Vec<&str> = page_one.recent.iter().map(|r| r.txid.as_str()).collect();
    assert_eq!(page_one_txids, vec![txid(5), txid(4), txid(3)]);
    // The window aggregate always covers every rejection, not just the page.
    assert_eq!(page_one.window.count, 5);
    let cursor = page_one.next_cursor.expect("more rejections remain");
    assert_eq!(cursor, "200:source-a/session-a/3");

    let (status, body) = get(
        application,
        &format!("/api/v1/sources/source-a/rejections?limit=3&before={cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let page_two = rejections_of(body);
    let page_two_txids: Vec<&str> = page_two.recent.iter().map(|r| r.txid.as_str()).collect();
    assert_eq!(page_two_txids, vec![txid(2), txid(1)]);
    assert_eq!(page_two.next_cursor, None);
}

#[tokio::test]
async fn removals_are_never_counted_as_rejections() {
    let (_temporary, application) = application(
        &[
            reject_at(1, 100, txid(1), "insufficient fee"),
            // A removal for a different transaction must not enter the surface.
            event_at(
                2,
                200,
                Evidence::MempoolRemoved {
                    txid: txid(2),
                    reason: Some("expired".to_owned()),
                },
            ),
        ],
        true,
    );

    let (status, body) = get(application, "/api/v1/sources/source-a/rejections").await;
    assert_eq!(status, StatusCode::OK);
    let rejections = rejections_of(body);
    assert_eq!(rejections.window.count, 1);
    assert_eq!(rejections.recent.len(), 1);
    assert_eq!(rejections.recent[0].txid, txid(1));
    assert!(rejections.recent.iter().all(|r| r.txid != txid(2)));
}

#[tokio::test]
async fn malformed_cursor_is_a_bad_request() {
    for cursor in [
        "not-a-cursor",
        "100:garbage",
        "100:source-a/session-a/01",
        "100:source-a/session-a/1:extra",
    ] {
        let (_temporary, application) =
            application(&[reject_at(1, 100, txid(1), "insufficient fee")], true);
        let (status, body) = get(
            application,
            &format!("/api/v1/sources/source-a/rejections?before={cursor}"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{cursor}");
        assert!(
            body.get("error").is_some_and(serde_json::Value::is_string),
            "{cursor} should return a JSON error"
        );
    }
}

#[tokio::test]
async fn cursor_for_another_source_is_a_bad_request() {
    let (_temporary, application) =
        application(&[reject_at(1, 100, txid(1), "insufficient fee")], true);
    let (status, body) = get(
        application,
        "/api/v1/sources/source-a/rejections?before=100:source-b/session-b/1",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        serde_json::json!({
            "error": "invalid cursor: 100:source-b/session-b/1"
        })
    );
}

#[tokio::test]
async fn invalid_limit_and_unknown_parameters_are_bad_requests() {
    let cases = [
        "/api/v1/sources/source-a/rejections?limit=0",
        "/api/v1/sources/source-a/rejections?limit=abc",
        "/api/v1/sources/source-a/rejections?limit=3&limit=4",
        "/api/v1/sources/source-a/rejections?flavor=spicy",
    ];
    for uri in cases {
        let (_temporary, application) =
            application(&[reject_at(1, 100, txid(1), "insufficient fee")], true);
        let (status, body) = get(application, uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert!(
            body.get("error").is_some_and(serde_json::Value::is_string),
            "{uri} should return a JSON error body, got {body}"
        );
    }
}

#[tokio::test]
async fn limit_is_clamped_to_the_maximum_page_size() {
    // A page size above the maximum is clamped, not rejected.
    let (_temporary, application) =
        application(&[reject_at(1, 100, txid(1), "insufficient fee")], true);
    let (status, _body) = get(
        application,
        "/api/v1/sources/source-a/rejections?limit=100000",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}
