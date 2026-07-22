use atlas_agent::peer_observer::normalize_payload;
use atlas_model::{
    CaptureGapCertainty, CaptureStatus, Evidence, IngestBatchRequest, IngestBatchResponse,
    IngestResponse, IngestStatus, MAX_INGEST_BATCH_BODY_BYTES, MempoolEntryFacts,
    MempoolEntryFactsStatus, MempoolSnapshot, NormalizedEvent, SourceReplicaEntry,
};
use atlas_model::{SourceId, SourceSessionId};
use atlas_server::{Store, experimental_evidence_router as router, router as production_router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

mod support;

use support::install_checkpoint;

const TXID: &str = "1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100";

#[tokio::test]
async fn production_router_does_not_mount_legacy_event_ingest() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let application = production_router(Store::open(database).expect("store"));

    for path in ["/api/v1/events", "/api/v1/events/batch"] {
        let response = application
            .clone()
            .oneshot(
                Request::post(path)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

fn event() -> NormalizedEvent {
    event_for("source-a", "session-a")
}

fn event_for(source_id: &str, session_id: &str) -> NormalizedEvent {
    normalize_payload(
        include_bytes!("../../../fixtures/peer-observer/mempool-added.pb"),
        &SourceId::new(source_id).expect("source"),
        &SourceSessionId::new(session_id).expect("session"),
        1,
        1_721_234_567_897,
    )
    .expect("decode fixture")
    .expect("supported fixture")
}

#[tokio::test]
async fn event_is_idempotent_but_cannot_establish_product_state() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let application = router(Store::open(database).expect("store"));
    let body = serde_json::to_vec(&event()).expect("event json");

    for expected_status in [IngestStatus::Applied, IngestStatus::Duplicate] {
        let response = application
            .clone()
            .oneshot(
                Request::post("/api/v1/events")
                    .header("content-type", "application/json")
                    .body(Body::from(body.clone()))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        let ingest: IngestResponse = serde_json::from_slice(&bytes).expect("ingest response");
        assert_eq!(ingest.status, expected_status);
    }

    let response = application
        .oneshot(
            Request::get("/api/v1/sources/source-a/mempool")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn source_read_exposes_reconciled_mempool_facts() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store = Store::open(database).expect("store");
    let facts = MempoolEntryFacts {
        vsize: 141,
        fee_sats: 1_200,
        entered_at_ms: 1_721_234_000_000,
    };
    install_checkpoint(
        &store,
        "source-a",
        1_721_234_567_890,
        vec![SourceReplicaEntry::new(TXID, facts.clone()).expect("entry")],
    );
    let application = production_router(store);

    let response = application
        .oneshot(
            Request::get("/api/v1/sources/source-a/mempool")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let snapshot: MempoolSnapshot = serde_json::from_slice(&bytes).expect("snapshot");
    assert_eq!(
        snapshot.memberships[0].facts,
        MempoolEntryFactsStatus::Available { facts }
    );
}

#[tokio::test]
async fn event_batch_is_idempotent_and_acknowledged_in_request_order() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let application = router(Store::open(database).expect("store"));
    let request = IngestBatchRequest {
        events: vec![
            event_for("source-a", "session-a"),
            event_for("source-a", "session-b"),
        ],
    };
    let expected_event_ids = request
        .events
        .iter()
        .map(|event| event.event_id.clone())
        .collect::<Vec<_>>();
    let body = serde_json::to_vec(&request).expect("batch json");

    for expected_status in [IngestStatus::Applied, IngestStatus::Duplicate] {
        let response = application
            .clone()
            .oneshot(
                Request::post("/api/v1/events/batch")
                    .header("content-type", "application/json")
                    .body(Body::from(body.clone()))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        let response: IngestBatchResponse = serde_json::from_slice(&bytes).expect("batch response");
        assert_eq!(
            response
                .acknowledgements
                .iter()
                .map(|acknowledgement| acknowledgement.event_id.clone())
                .collect::<Vec<_>>(),
            expected_event_ids
        );
        assert!(
            response
                .acknowledgements
                .iter()
                .all(|acknowledgement| acknowledgement.status == expected_status)
        );
    }
}

#[tokio::test]
async fn event_batch_rejects_mixed_sources_before_ingest() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store = Store::open(database).expect("store");
    let application = router(store.clone());
    let request = IngestBatchRequest {
        events: vec![
            event_for("source-a", "session-a"),
            event_for("source-b", "session-b"),
        ],
    };

    let response = application
        .oneshot(
            Request::post("/api/v1/events/batch")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&request).expect("batch json"),
                ))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        store
            .mempool(&SourceId::new("source-a").expect("source"))
            .expect("mempool"),
        None
    );
}

#[tokio::test]
async fn event_batch_rejects_a_body_above_the_wire_limit() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let application = router(Store::open(database).expect("store"));

    let response = application
        .oneshot(
            Request::post("/api/v1/events/batch")
                .header("content-type", "application/json")
                .body(Body::from(vec![b' '; MAX_INGEST_BATCH_BODY_BYTES + 1]))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn same_txid_from_two_sources_remains_isolated_by_source_endpoint() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store = Store::open(database).expect("store");
    let facts = MempoolEntryFacts {
        vsize: 141,
        fee_sats: 1_200,
        entered_at_ms: 1_721_234_000_000,
    };
    for source_id in ["source-a", "source-b"] {
        install_checkpoint(
            &store,
            source_id,
            1_721_234_567_890,
            vec![SourceReplicaEntry::new(TXID, facts.clone()).expect("entry")],
        );
    }
    let application = production_router(store);

    for source_id in ["source-a", "source-b"] {
        let response = application
            .clone()
            .oneshot(
                Request::get(format!("/api/v1/sources/{source_id}/mempool"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        let snapshot: MempoolSnapshot = serde_json::from_slice(&bytes).expect("snapshot");
        assert_eq!(snapshot.source_id.as_str(), source_id);
        assert_eq!(snapshot.memberships.len(), 1);
        assert_eq!(snapshot.memberships[0].txid, TXID);
    }
}

#[tokio::test]
async fn source_read_rejects_invalid_source_id_and_returns_not_found_for_unknown_source() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let application = router(Store::open(database).expect("store"));

    for (path, expected_status) in [
        ("/api/v1/sources/source!/mempool", StatusCode::BAD_REQUEST),
        (
            "/api/v1/sources/unknown-source/mempool",
            StatusCode::NOT_FOUND,
        ),
    ] {
        let response = application
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(response.status(), expected_status);
    }
}

#[tokio::test]
async fn gap_evidence_does_not_replace_state_health_or_membership() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store = Store::open(database).expect("store");
    install_checkpoint(&store, "source-a", 1_721_234_567_890, vec![]);
    let application = router(store);
    let gap = NormalizedEvent::new(
        SourceId::new("source-a").expect("source"),
        SourceSessionId::new("session-a").expect("session"),
        1,
        1_721_234_567_897,
        1_721_234_567_897,
        Evidence::CaptureGap {
            input: "peer_observer_nats".to_owned(),
            reason: "slow_consumer".to_owned(),
            certainty: CaptureGapCertainty::KnownLoss,
        },
    )
    .expect("gap event");

    let response = application
        .clone()
        .oneshot(
            Request::post("/api/v1/events")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&gap).expect("event json")))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let response = application
        .oneshot(
            Request::get("/api/v1/sources/source-a/mempool")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let snapshot: MempoolSnapshot = serde_json::from_slice(&bytes).expect("snapshot");
    assert!(snapshot.memberships.is_empty());
    assert_eq!(snapshot.health.capture, CaptureStatus::NotCollected);
}

#[tokio::test]
async fn p2p_transaction_larger_than_axum_default_body_limit_is_ingested() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let application = router(Store::open(database).expect("store"));
    let event = NormalizedEvent::new(
        SourceId::new("source-a").expect("source"),
        SourceSessionId::new("session-a").expect("session"),
        1,
        1_721_234_567_897,
        1_721_234_567_897,
        Evidence::P2pTransaction {
            txid: TXID.to_owned(),
            wtxid: TXID.to_owned(),
            raw_transaction_hex: Some("00".repeat(1_100_000)),
            peer_id: Some(42),
            inbound: Some(true),
        },
    )
    .expect("event");
    let body = serde_json::to_vec(&event).expect("event json");
    assert!(body.len() > 2 * 1024 * 1024);
    assert!(body.len() < 16 * 1024 * 1024);

    let response = application
        .oneshot(
            Request::post("/api/v1/events")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let ingest: IngestResponse = serde_json::from_slice(&bytes).expect("ingest response");
    assert_eq!(ingest.status, IngestStatus::Applied);
}
