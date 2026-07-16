use std::collections::BTreeSet;
use std::net::SocketAddr;

use atlas_agent::delivery::{DeliveryOutcome, deliver_next};
use atlas_agent::outbox::{AgentIdentity, Outbox};
use atlas_model::{MAX_INGEST_BATCH_EVENTS, SourceId, SourceSessionId};
use atlas_server::{Store, router};
use reqwest::Client;

const BASELINE_TXIDS: u64 = 200_000;

fn source() -> SourceId {
    SourceId::new("scale-core").expect("source")
}

fn identity() -> AgentIdentity {
    AgentIdentity::new(
        source(),
        SourceSessionId::new("scale-session").expect("session"),
    )
}

fn baseline() -> BTreeSet<String> {
    (1..=BASELINE_TXIDS)
        .map(|value| format!("{value:064x}"))
        .collect()
}

#[tokio::test]
#[ignore = "200,000-txid baseline acceptance"]
async fn two_hundred_thousand_txid_baseline_drains_in_bounded_batches() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let outbox_path = temporary.path().join("outbox.db");
    let store_path = temporary.path().join("atlas.db");
    Outbox::migrate(&outbox_path).expect("migrate outbox");
    Store::migrate(&store_path).expect("migrate store");
    let outbox = Outbox::open(&outbox_path, source()).expect("open outbox");
    let expected = baseline();
    assert_eq!(
        outbox
            .reconcile_rpc_snapshot(&identity(), expected, 100)
            .expect("baseline reconciliation"),
        usize::try_from(BASELINE_TXIDS).expect("baseline count fits usize")
    );
    assert_eq!(
        outbox.pending_count().expect("pending baseline"),
        BASELINE_TXIDS
    );
    drop(outbox);

    let reopened = Outbox::open(&outbox_path, source()).expect("reopen outbox");
    let store = Store::open(&store_path).expect("store");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address: SocketAddr = listener.local_addr().expect("address");
    let server = tokio::spawn({
        let application = router(store.clone());
        async move {
            axum::serve(listener, application).await.expect("serve");
        }
    });
    let endpoint = format!("http://{address}/api/v1/events");
    let client = Client::new();
    let mut request_count = 0_usize;
    let mut delivered_events = 0_usize;

    loop {
        match deliver_next(&reopened, &client, &endpoint)
            .await
            .expect("delivery")
        {
            DeliveryOutcome::Idle => break,
            DeliveryOutcome::BatchDelivered { event_count, .. } => {
                assert!(event_count <= MAX_INGEST_BATCH_EVENTS);
                request_count += 1;
                delivered_events += event_count;
            }
            DeliveryOutcome::Delivered { event_id, .. } => {
                panic!("baseline event was not batched: {event_id}")
            }
            DeliveryOutcome::Deferred { error, .. } => panic!("delivery deferred: {error}"),
        }
    }

    let expected_requests = usize::try_from(BASELINE_TXIDS)
        .expect("baseline count fits usize")
        .div_ceil(MAX_INGEST_BATCH_EVENTS);
    assert_eq!(request_count, expected_requests);
    assert_eq!(
        delivered_events,
        usize::try_from(BASELINE_TXIDS).expect("count")
    );
    assert_eq!(reopened.pending_count().expect("drained outbox"), 0);
    let snapshot = store
        .mempool(&source())
        .expect("central baseline")
        .expect("known source");
    assert_eq!(
        snapshot.memberships.len(),
        usize::try_from(BASELINE_TXIDS).expect("count")
    );
    assert_eq!(
        snapshot.memberships.first().expect("first").txid,
        format!("{:064x}", 1_u64)
    );
    assert_eq!(
        snapshot.memberships.last().expect("last").txid,
        format!("{BASELINE_TXIDS:064x}")
    );
    drop(snapshot);
    assert_eq!(
        reopened
            .reconcile_rpc_snapshot(&identity(), baseline(), 200)
            .expect("idempotent reconciliation"),
        0
    );
    server.abort();
}
