use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use atlas_agent::delivery::{DeliveryOutcome, deliver_next};
use atlas_agent::outbox::{AgentIdentity, Outbox};
use atlas_model::{
    Evidence, IngestBatchRequest, IngestBatchResponse, IngestResponse, IngestStatus,
    MempoolEntryFacts, MempoolEntryFactsStatus, SourceId, SourceSessionId,
};
use atlas_server::{Store, router};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use reqwest::Client;
use tokio::task::JoinHandle;

fn source() -> SourceId {
    SourceId::new("core-a").expect("source")
}

fn identity() -> AgentIdentity {
    AgentIdentity::new(
        source(),
        SourceSessionId::new("session-a").expect("session"),
    )
}

fn facts() -> MempoolEntryFacts {
    MempoolEntryFacts {
        vsize: 141,
        fee_sats: 1_200,
        entered_at_ms: 1_721_234_000_000,
    }
}

fn snapshot(count: u64) -> BTreeMap<String, MempoolEntryFacts> {
    (1..=count)
        .map(|value| (format!("{value:064x}"), facts()))
        .collect()
}

fn open_outbox(path: &Path) -> Outbox {
    Outbox::open(path, source()).expect("open outbox")
}

async fn serve(application: Router) -> (String, JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address: SocketAddr = listener.local_addr().expect("address");
    let task = tokio::spawn(async move {
        axum::serve(listener, application).await.expect("serve");
    });
    (format!("http://{address}/api/v1/events"), task)
}

#[tokio::test]
async fn reconciliation_batches_drain_before_later_live_evidence() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let outbox_path = temporary.path().join("outbox.db");
    let store_path = temporary.path().join("atlas.db");
    Outbox::migrate(&outbox_path).expect("migrate outbox");
    Store::migrate(&store_path).expect("migrate store");
    let outbox = open_outbox(&outbox_path);
    let store = Store::open(&store_path).expect("store");
    let baseline = snapshot(1_025);

    assert_eq!(
        outbox
            .reconcile_rpc_snapshot(&identity(), baseline, 100)
            .expect("baseline"),
        1_025
    );
    let removed_txid = format!("{:064x}", 1_u64);
    outbox
        .enqueue_observation(
            &identity(),
            101,
            102,
            Evidence::MempoolRemoved {
                txid: removed_txid,
                reason: Some("live removal".to_owned()),
            },
            Some("mempool"),
            Some(b"live removal"),
        )
        .expect("live evidence");

    let (endpoint, server) = serve(router(store.clone())).await;
    let client = Client::new();
    let mut delivered_batch_sizes = Vec::new();
    let mut single_deliveries = 0_usize;
    loop {
        match deliver_next(&outbox, &client, &endpoint)
            .await
            .expect("delivery")
        {
            DeliveryOutcome::Idle => break,
            DeliveryOutcome::BatchDelivered { event_count, .. } => {
                delivered_batch_sizes.push(event_count);
            }
            DeliveryOutcome::Delivered { .. } => single_deliveries += 1,
            DeliveryOutcome::Deferred { error, .. } => panic!("delivery deferred: {error}"),
        }
    }

    assert_eq!(delivered_batch_sizes, [512, 512, 1]);
    assert_eq!(single_deliveries, 1);
    assert_eq!(outbox.pending_count().expect("pending count"), 0);
    let snapshot = store
        .mempool(&source())
        .expect("central mempool")
        .expect("known source");
    assert_eq!(snapshot.memberships.len(), 1_024);
    assert!(snapshot.memberships.iter().all(|membership| {
        membership.facts == MempoolEntryFactsStatus::Available { facts: facts() }
    }));
    server.abort();
}

#[derive(Clone)]
struct LostResponseState {
    store: Store,
    calls: Arc<AtomicUsize>,
    statuses: Arc<Mutex<Vec<Vec<IngestStatus>>>>,
}

async fn commit_then_lose_first_response(
    State(state): State<LostResponseState>,
    Json(request): Json<IngestBatchRequest>,
) -> Response {
    let event_ids = request
        .events
        .iter()
        .map(|event| event.event_id.clone())
        .collect::<Vec<_>>();
    let statuses = state.store.ingest_batch(&request).expect("batch ingest");
    state
        .statuses
        .lock()
        .expect("status lock")
        .push(statuses.clone());
    if state.calls.fetch_add(1, Ordering::SeqCst) == 0 {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }

    let acknowledgements = event_ids
        .into_iter()
        .zip(statuses)
        .map(|(event_id, status)| IngestResponse { event_id, status })
        .collect();
    (
        StatusCode::ACCEPTED,
        Json(IngestBatchResponse { acknowledgements }),
    )
        .into_response()
}

#[tokio::test]
async fn committed_batch_with_lost_response_retries_after_agent_reopen() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let outbox_path = temporary.path().join("outbox.db");
    let store_path = temporary.path().join("atlas.db");
    Outbox::migrate(&outbox_path).expect("migrate outbox");
    Store::migrate(&store_path).expect("migrate store");
    let outbox = open_outbox(&outbox_path);
    let store = Store::open(&store_path).expect("store");
    assert_eq!(
        outbox
            .reconcile_rpc_snapshot(&identity(), snapshot(3), 100)
            .expect("baseline"),
        3
    );

    let state = LostResponseState {
        store: store.clone(),
        calls: Arc::new(AtomicUsize::new(0)),
        statuses: Arc::new(Mutex::new(Vec::new())),
    };
    let application = Router::new()
        .route(
            "/api/v1/events/batch",
            post(commit_then_lose_first_response),
        )
        .with_state(state.clone());
    let (endpoint, server) = serve(application).await;
    let client = Client::new();

    assert!(matches!(
        deliver_next(&outbox, &client, &endpoint)
            .await
            .expect("first delivery"),
        DeliveryOutcome::Deferred { attempts: 1, .. }
    ));
    assert_eq!(outbox.pending_count().expect("pending after loss"), 3);
    assert_eq!(
        store
            .mempool(&source())
            .expect("central")
            .expect("known source")
            .memberships
            .len(),
        3
    );
    drop(outbox);

    let reopened = open_outbox(&outbox_path);
    assert!(matches!(
        deliver_next(&reopened, &client, &endpoint)
            .await
            .expect("retry"),
        DeliveryOutcome::BatchDelivered { event_count: 3, .. }
    ));
    assert_eq!(reopened.pending_count().expect("pending after retry"), 0);
    let statuses = state.statuses.lock().expect("status lock");
    assert_eq!(
        statuses.as_slice(),
        [
            vec![IngestStatus::Applied; 3],
            vec![IngestStatus::Duplicate; 3],
        ]
    );
    server.abort();
}
