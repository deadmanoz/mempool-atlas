use atlas_agent::peer_observer::normalize_payload;
use atlas_model::{IngestResponse, IngestStatus, MempoolSnapshot, NormalizedEvent};
use atlas_model::{SourceId, SourceSessionId};
use atlas_server::{Store, router};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

const TXID: &str = "1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100";

fn event() -> NormalizedEvent {
    normalize_payload(
        include_bytes!("../../../fixtures/peer-observer/mempool-added.pb"),
        &SourceId::new("source-a").expect("source"),
        &SourceSessionId::new("session-a").expect("session"),
        1,
        1_721_234_567_897,
    )
    .expect("decode fixture")
    .expect("supported fixture")
}

#[tokio::test]
async fn event_is_idempotent_and_visible_in_read_api() {
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
            Request::get("/api/v1/mempool?source=source-a")
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
    assert_eq!(snapshot.memberships.len(), 1);
    assert_eq!(snapshot.memberships[0].txid, TXID);
}
