use std::collections::BTreeMap;
use std::net::SocketAddr;

use atlas_agent::schema;
use atlas_agent::source_replica::{
    ObserveRpcOutcome, SourceReplica, SourceReplicaAction, SourceReplicaLimits,
};
use atlas_agent::state_delivery::{StateActionKind, StateDeliveryOutcome, deliver_next_state};
use atlas_model::{
    MAX_CHECKPOINT_CHUNK_ENTRIES, MempoolEntryFacts, MempoolEntryFactsStatus, SourceId,
};
use atlas_server::{Store, router};
use reqwest::Client;
use tokio::sync::watch;

const BASELINE_TXIDS: u64 = 200_000;

fn source() -> SourceId {
    SourceId::new("scale-core").expect("source")
}

fn facts() -> MempoolEntryFacts {
    MempoolEntryFacts {
        vsize: 141,
        fee_sats: 1_200,
        entered_at_ms: 1_721_234_000_000,
    }
}

fn baseline() -> BTreeMap<String, MempoolEntryFacts> {
    (1..=BASELINE_TXIDS)
        .map(|value| (format!("{value:064x}"), facts()))
        .collect()
}

#[tokio::test]
#[ignore = "200,000-txid SourceReplica baseline acceptance"]
async fn two_hundred_thousand_txid_baseline_survives_restart_and_activates_atomically() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let replica_path = temporary.path().join("agent.db");
    let store_path = temporary.path().join("atlas.db");
    schema::migrate(&replica_path).expect("migrate agent database");
    Store::migrate(&store_path).expect("migrate central database");

    let replica = SourceReplica::open(&replica_path, source(), SourceReplicaLimits::default())
        .expect("open source replica");
    let expected = baseline();
    assert_eq!(
        replica
            .observe_rpc_snapshot(&expected, 1_721_234_000_100)
            .expect("observe baseline"),
        ObserveRpcOutcome::Baseline {
            revision: 1,
            entry_count: BASELINE_TXIDS,
        }
    );
    let frozen = replica
        .next_action()
        .expect("freeze checkpoint")
        .expect("checkpoint action");
    let SourceReplicaAction::Checkpoint(begin) = &frozen else {
        panic!("baseline did not freeze as a checkpoint: {frozen:?}");
    };
    assert_eq!(begin.target_revision, 1);
    assert_eq!(begin.expected_entries, BASELINE_TXIDS);
    assert_eq!(
        begin.expected_chunks,
        u32::try_from(
            usize::try_from(BASELINE_TXIDS)
                .expect("baseline count fits usize")
                .div_ceil(MAX_CHECKPOINT_CHUNK_ENTRIES)
        )
        .expect("checkpoint count fits u32")
    );
    drop(replica);

    // Both databases are deliberately reopened before delivery. The agent
    // must retain the exact frozen checkpoint and the server starts solely
    // from its durable, migrated state.
    let replica = SourceReplica::open(&replica_path, source(), SourceReplicaLimits::default())
        .expect("reopen source replica");
    assert_eq!(
        replica.next_action().expect("reopen frozen action"),
        Some(frozen)
    );
    let store = Store::open(&store_path).expect("reopen central store");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind state server");
    let address: SocketAddr = listener.local_addr().expect("state server address");
    let server = tokio::spawn({
        let application = router(store.clone());
        async move {
            axum::serve(listener, application)
                .await
                .expect("serve state API");
        }
    });
    let endpoint = format!("http://{address}/api/v1/state");
    let client = Client::new();
    let (_shutdown_tx, mut shutdown_rx) = watch::channel(false);

    let outcome = deliver_next_state(&replica, &client, &endpoint, &mut shutdown_rx)
        .await
        .expect("deliver baseline checkpoint");
    assert!(matches!(
        outcome,
        StateDeliveryOutcome::Applied {
            kind: StateActionKind::Checkpoint,
            ref active_cursor,
        } if active_cursor.revision == 1
    ));
    assert_eq!(replica.next_action().expect("drained source replica"), None);
    let storage = replica.storage_stats().expect("source replica storage");
    assert_eq!(storage.membership_rows, BASELINE_TXIDS);
    assert_eq!(storage.dirty_rows, 0);
    assert_eq!(storage.frozen_delta_rows, 0);
    assert_eq!(storage.frozen_checkpoint_rows, 0);

    server.abort();
    let server_error = server.await.expect_err("aborted state server");
    assert!(server_error.is_cancelled());
    drop(store);

    // A second central-store reopen verifies that activation, not merely the
    // serving process's connection state, owns the complete baseline.
    let reopened_store = Store::open(&store_path).expect("reopen activated central store");
    let active = reopened_store
        .active_source_replica(&source())
        .expect("active replica read")
        .expect("active replica");
    assert_eq!(active.cursor.revision, 1);
    assert_eq!(active.entries.len(), expected.len());
    assert_eq!(
        active.entries.first().expect("first active entry").txid,
        format!("{:064x}", 1_u64)
    );
    assert_eq!(
        active.entries.last().expect("last active entry").txid,
        format!("{BASELINE_TXIDS:064x}")
    );

    let snapshot = reopened_store
        .mempool(&source())
        .expect("reader-visible baseline")
        .expect("known source");
    assert_eq!(snapshot.memberships.len(), expected.len());
    assert_eq!(snapshot.health.state_cursor, active.cursor);
    assert_eq!(snapshot.health.state_observed_at_ms, 1_721_234_000_100);
    assert_eq!(
        snapshot
            .memberships
            .first()
            .expect("first membership")
            .facts,
        MempoolEntryFactsStatus::Available { facts: facts() }
    );
    assert_eq!(
        snapshot.memberships.last().expect("last membership").facts,
        MempoolEntryFactsStatus::Available { facts: facts() }
    );
}
