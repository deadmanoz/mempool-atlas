//! HTTP behavior of the aggregate summary and source-listing endpoints:
//! strict query validation, honest awaiting-RPC separation, and stable
//! source discovery. Aggregation arithmetic is unit-tested in the summary
//! module; fixture parity lives in the fixture contract test.

use atlas_model::{
    AggregateBin, Evidence, MempoolEntryFacts, MempoolSummary, NormalizedEvent,
    ReconciledMembership, SourceId, SourceSessionId, SourcesResponse,
};
use atlas_server::{Store, router_with_clock};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
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
        "/api/v1/sources/source-a/mempool/summary?class=unknown&feerate_min=1",
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
    assert_eq!(
        summary.filter_echo.classes,
        Some(vec![atlas_model::Classification::Unknown])
    );
    assert_eq!(summary.filter_echo.feerate_min, Some(1.0));
}

#[tokio::test]
async fn summary_rejects_malformed_queries_with_json_errors() {
    let cases = [
        // Unknown parameters, facet values, and detail selections must never
        // silently widen or narrow a filter.
        "/api/v1/sources/source-a/mempool/summary?flavor=spicy",
        "/api/v1/sources/source-a/mempool/summary?class=snazzy",
        "/api/v1/sources/source-a/mempool/summary?script=opreturn",
        "/api/v1/sources/source-a/mempool/summary?class=",
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
