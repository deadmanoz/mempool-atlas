use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use atlas_agent::delivery::DeliveryFailureClass;
use atlas_agent::schema;
use atlas_agent::source_replica::{
    ObserveRpcOutcome, SourceReplica, SourceReplicaAction, SourceReplicaLimits,
};
use atlas_agent::state_delivery::{StateActionKind, StateDeliveryOutcome, deliver_next_state};
use atlas_model::{
    CheckpointBegin, CheckpointId, MempoolEntryFacts, SourceId, SourceReplicaCommand,
    SourceReplicaEntry, SourceReplicaRequest,
};
use atlas_server::{Store, router};
use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, http::StatusCode};
use reqwest::Client;
use tokio::sync::watch;
use tokio::task::JoinHandle;

const OBSERVED_1: u64 = 1_721_234_000_100;
const OBSERVED_2: u64 = 1_721_234_000_200;
const OBSERVED_3: u64 = 1_721_234_000_300;
const OBSERVED_4: u64 = 1_721_234_000_400;

fn source() -> SourceId {
    SourceId::new("delivery-core").expect("source")
}

fn txid(value: u64) -> String {
    format!("{value:064x}")
}

fn facts(value: u64) -> MempoolEntryFacts {
    MempoolEntryFacts {
        vsize: 140 + value,
        fee_sats: 1_000 + value,
        entered_at_ms: 1_721_000_000_000 + value,
    }
}

fn snapshot(values: &[u64]) -> BTreeMap<String, MempoolEntryFacts> {
    values
        .iter()
        .map(|value| (txid(*value), facts(*value)))
        .collect()
}

fn entries(values: &[u64]) -> Vec<SourceReplicaEntry> {
    values
        .iter()
        .map(|value| SourceReplicaEntry {
            txid: txid(*value),
            facts: facts(*value),
        })
        .collect()
}

struct TestServer {
    endpoint: String,
    task: JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start_server(application: Router) -> TestServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test server");
    let address: SocketAddr = listener.local_addr().expect("test server address");
    let task = tokio::spawn(async move {
        axum::serve(listener, application)
            .await
            .expect("serve state API");
    });
    TestServer {
        endpoint: format!("http://{address}/api/v1/state"),
        task,
    }
}

async fn deliver(replica: &SourceReplica, endpoint: &str) -> StateDeliveryOutcome {
    let client = Client::new();
    let (_shutdown_tx, mut shutdown_rx) = watch::channel(false);
    deliver_next_state(replica, &client, endpoint, &mut shutdown_rx)
        .await
        .expect("state delivery")
}

fn assert_active(store: &Store, revision: u64, expected_values: &[u64]) {
    let active = store
        .active_source_replica(&source())
        .expect("active state read")
        .expect("active state");
    assert_eq!(active.cursor.revision, revision);
    assert_eq!(active.entries, entries(expected_values));
}

#[tokio::test]
async fn frozen_checkpoint_and_delta_deliver_before_later_divergence_converges() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let replica_path = temporary.path().join("agent.db");
    let store_path = temporary.path().join("atlas.db");
    schema::migrate(&replica_path).expect("migrate agent database");
    Store::migrate(&store_path).expect("migrate central database");
    let replica = SourceReplica::open(&replica_path, source(), SourceReplicaLimits::default())
        .expect("open source replica");
    let store = Store::open(&store_path).expect("open central store");
    let server = start_server(router(store.clone())).await;

    assert_eq!(
        replica
            .observe_rpc_snapshot(&snapshot(&[1]), OBSERVED_1)
            .expect("observe baseline"),
        ObserveRpcOutcome::Baseline {
            revision: 1,
            entry_count: 1,
        }
    );
    let frozen_checkpoint = replica
        .next_action()
        .expect("freeze checkpoint")
        .expect("checkpoint action");
    assert!(matches!(
        frozen_checkpoint,
        SourceReplicaAction::Checkpoint(ref begin) if begin.target_revision == 1
    ));
    assert!(matches!(
        replica
            .observe_rpc_snapshot(&snapshot(&[1, 2]), OBSERVED_2)
            .expect("observe behind checkpoint"),
        ObserveRpcOutcome::Changed { revision: 2, .. }
    ));
    assert_eq!(
        replica.next_action().expect("stable frozen checkpoint"),
        Some(frozen_checkpoint)
    );

    assert!(matches!(
        deliver(&replica, &server.endpoint).await,
        StateDeliveryOutcome::Applied {
            kind: StateActionKind::Checkpoint,
            ref active_cursor,
        } if active_cursor.revision == 1
    ));
    assert_active(&store, 1, &[1]);
    assert!(matches!(
        replica.next_action().expect("post-checkpoint action"),
        Some(SourceReplicaAction::Delta(ref delta))
            if delta.base_revision == 1 && delta.target_revision == 2
    ));
    assert!(matches!(
        deliver(&replica, &server.endpoint).await,
        StateDeliveryOutcome::Applied {
            kind: StateActionKind::Delta,
            ref active_cursor,
        } if active_cursor.revision == 2
    ));
    assert_active(&store, 2, &[1, 2]);

    assert!(matches!(
        replica
            .observe_rpc_snapshot(&snapshot(&[1, 2, 3]), OBSERVED_3)
            .expect("observe first delta"),
        ObserveRpcOutcome::Changed { revision: 3, .. }
    ));
    let frozen_delta = replica
        .next_action()
        .expect("freeze delta")
        .expect("delta action");
    assert!(matches!(
        frozen_delta,
        SourceReplicaAction::Delta(ref delta)
            if delta.base_revision == 2 && delta.target_revision == 3
    ));
    assert!(matches!(
        replica
            .observe_rpc_snapshot(&snapshot(&[1, 2, 3, 4]), OBSERVED_4)
            .expect("observe behind delta"),
        ObserveRpcOutcome::Changed { revision: 4, .. }
    ));
    assert_eq!(
        replica.next_action().expect("stable frozen delta"),
        Some(frozen_delta)
    );

    assert!(matches!(
        deliver(&replica, &server.endpoint).await,
        StateDeliveryOutcome::Applied {
            kind: StateActionKind::Delta,
            ref active_cursor,
        } if active_cursor.revision == 3
    ));
    assert_active(&store, 3, &[1, 2, 3]);
    assert!(matches!(
        replica.next_action().expect("post-delta action"),
        Some(SourceReplicaAction::Delta(ref delta))
            if delta.base_revision == 3 && delta.target_revision == 4
    ));
    assert!(matches!(
        deliver(&replica, &server.endpoint).await,
        StateDeliveryOutcome::Applied {
            kind: StateActionKind::Delta,
            ref active_cursor,
        } if active_cursor.revision == 4
    ));
    assert_active(&store, 4, &[1, 2, 3, 4]);
    assert_eq!(replica.next_action().expect("converged replica"), None);
}

#[derive(Clone)]
struct LossyState {
    store: Store,
    lose_next_commit_response: Arc<AtomicBool>,
}

async fn lossy_state_ingest(
    State(state): State<LossyState>,
    Json(request): Json<SourceReplicaRequest>,
) -> Response {
    let is_commit = matches!(&request.command, SourceReplicaCommand::CheckpointCommit(_));
    let response = state
        .store
        .apply_source_replica(&request)
        .expect("lossy wrapper applies valid state request");
    if is_commit
        && state
            .lose_next_commit_response
            .swap(false, Ordering::SeqCst)
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "simulated checkpoint commit response loss",
        )
            .into_response();
    }
    (StatusCode::ACCEPTED, Json(response)).into_response()
}

#[tokio::test]
async fn lost_checkpoint_commit_response_retries_identical_frozen_action_after_restart() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let replica_path = temporary.path().join("agent.db");
    let store_path = temporary.path().join("atlas.db");
    schema::migrate(&replica_path).expect("migrate agent database");
    Store::migrate(&store_path).expect("migrate central database");
    let store = Store::open(&store_path).expect("open central store");
    let lose_next_commit_response = Arc::new(AtomicBool::new(true));
    let application = Router::new()
        .route("/api/v1/state", post(lossy_state_ingest))
        .with_state(LossyState {
            store: store.clone(),
            lose_next_commit_response: Arc::clone(&lose_next_commit_response),
        });
    let server = start_server(application).await;

    let replica = SourceReplica::open(&replica_path, source(), SourceReplicaLimits::default())
        .expect("open source replica");
    replica
        .observe_rpc_snapshot(&snapshot(&[1, 2]), OBSERVED_1)
        .expect("observe baseline");
    let frozen = replica
        .next_action()
        .expect("freeze checkpoint")
        .expect("checkpoint action");
    assert!(matches!(frozen, SourceReplicaAction::Checkpoint(_)));

    let first = deliver(&replica, &server.endpoint).await;
    assert!(matches!(
        first,
        StateDeliveryOutcome::Deferred {
            ref retry_key,
            class: DeliveryFailureClass::Transient,
            ..
        } if retry_key.kind == StateActionKind::Checkpoint
    ));
    assert!(!lose_next_commit_response.load(Ordering::SeqCst));
    assert_active(&store, 1, &[1, 2]);
    assert_eq!(
        replica.next_action().expect("frozen action after loss"),
        Some(frozen.clone())
    );
    drop(replica);

    let reopened = SourceReplica::open(&replica_path, source(), SourceReplicaLimits::default())
        .expect("reopen source replica");
    assert_eq!(
        reopened.next_action().expect("frozen action after restart"),
        Some(frozen)
    );
    assert!(matches!(
        deliver(&reopened, &server.endpoint).await,
        StateDeliveryOutcome::Applied {
            kind: StateActionKind::Checkpoint,
            ref active_cursor,
        } if active_cursor.revision == 1
    ));
    assert_eq!(reopened.next_action().expect("acknowledged retry"), None);
    let status = reopened.status().expect("source replica status");
    assert_eq!(status.acknowledged_revision, 1);
    assert!(status.has_acknowledged_cursor);
    assert_active(&store, 1, &[1, 2]);
}

#[tokio::test]
async fn checkpoint_conflict_persists_an_explicit_staging_fence_before_replacement() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let replica_path = temporary.path().join("agent.db");
    let store_path = temporary.path().join("atlas.db");
    schema::migrate(&replica_path).expect("migrate agent database");
    Store::migrate(&store_path).expect("migrate central database");
    let replica = SourceReplica::open(&replica_path, source(), SourceReplicaLimits::default())
        .expect("open source replica");
    let store = Store::open(&store_path).expect("open central store");
    let server = start_server(router(store.clone())).await;

    replica
        .observe_rpc_snapshot(&snapshot(&[1, 2]), OBSERVED_1)
        .expect("observe baseline");
    let frozen = replica
        .next_action()
        .expect("freeze local checkpoint")
        .expect("local checkpoint");
    let SourceReplicaAction::Checkpoint(local_begin) = &frozen else {
        panic!("expected local checkpoint");
    };
    let local_checkpoint_id = local_begin.checkpoint_id.clone();

    let epoch_id = replica.status().expect("replica status").epoch_id;
    let foreign_id = CheckpointId::new("foreign-staging").expect("foreign checkpoint");
    let foreign_entries = entries(&[9]);
    let foreign_begin =
        CheckpointBegin::new(foreign_id.clone(), None, 1, OBSERVED_1, 1, &foreign_entries)
            .expect("foreign begin");
    let foreign_request = SourceReplicaRequest::new(
        source(),
        epoch_id,
        SourceReplicaCommand::CheckpointBegin(foreign_begin),
    )
    .expect("foreign request");
    store
        .apply_source_replica(&foreign_request)
        .expect("stage foreign checkpoint");

    let recovery = deliver(&replica, &server.endpoint).await;
    let StateDeliveryOutcome::RecoveryCheckpointRequired {
        active_cursor,
        staging_checkpoint_id,
        ..
    } = recovery
    else {
        panic!("expected recovery checkpoint, got {recovery:?}");
    };
    assert!(active_cursor.is_none());
    assert_eq!(staging_checkpoint_id, Some(foreign_id.clone()));
    assert_eq!(replica.next_action().expect("still frozen"), Some(frozen));

    replica
        .require_checkpoint_against(active_cursor.as_ref(), staging_checkpoint_id.as_ref())
        .expect("persist fenced recovery");
    let replacement = replica
        .next_action()
        .expect("replacement action")
        .expect("replacement checkpoint");
    let SourceReplicaAction::Checkpoint(replacement_begin) = &replacement else {
        panic!("expected replacement checkpoint");
    };
    assert_ne!(replacement_begin.checkpoint_id, local_checkpoint_id);
    assert_eq!(replacement_begin.supersedes_checkpoint_id, Some(foreign_id));

    assert!(matches!(
        deliver(&replica, &server.endpoint).await,
        StateDeliveryOutcome::Applied {
            kind: StateActionKind::Checkpoint,
            ref active_cursor,
        } if active_cursor.revision == 1
    ));
    assert_active(&store, 1, &[1, 2]);
}
