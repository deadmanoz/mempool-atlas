use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;

use atlas_agent::schema;
use atlas_agent::source_replica::{
    ObserveRpcOutcome, SourceReplica, SourceReplicaAction, SourceReplicaLimits,
    SourceReplicaStoragePolicy, SourceReplicaStorageStats,
};
use atlas_agent::state_delivery::{StateActionKind, StateDeliveryOutcome, deliver_next_state};
use atlas_model::{
    CheckpointBegin, CheckpointChunk, CheckpointCommit, CheckpointDigest, CheckpointId,
    MAX_CHECKPOINT_CHUNK_ENTRIES, MempoolEntryFacts, MempoolEntryFactsStatus, ReplicaCursor,
    SourceEpochId, SourceId, SourceReplicaCommand, SourceReplicaEntry, SourceReplicaRequest,
    SourceReplicaResponse,
};
use atlas_server::{Store, StoreLimits, router};
use atlas_storage::{SqlitePhysicalSnapshot, SqliteStorageLimits};
use reqwest::Client;
use tokio::sync::watch;

const BASELINE_TXIDS: u64 = 200_000;
const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
const AGENT_FROZEN_REGRESSION_BYTES: u64 = 256 * MIB;
const SERVER_ONE_ACTIVE_REGRESSION_BYTES: u64 = 128 * MIB;
const SERVER_TWO_ACTIVE_REGRESSION_BYTES: u64 = 256 * MIB;
const SERVER_ACTIVE_STAGING_REGRESSION_BYTES: u64 = 512 * MIB;

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

fn agent_limits() -> SourceReplicaLimits {
    SourceReplicaLimits {
        max_membership_entries: BASELINE_TXIDS,
        ..SourceReplicaLimits::default()
    }
}

fn agent_storage_policy() -> SourceReplicaStoragePolicy {
    SourceReplicaStoragePolicy {
        max_total_sqlite_bytes: 1_792 * MIB,
        filesystem_reserve_bytes: 128 * MIB,
        filesystem_reserve_percent: 5,
        retained_wal_high_water_bytes: 64 * MIB,
        wal_autocheckpoint_pages: 1_000,
    }
}

fn server_sqlite_limits() -> SqliteStorageLimits {
    SqliteStorageLimits {
        max_database_bytes: 3 * GIB,
        max_total_sqlite_bytes: 3_584 * MIB,
        filesystem_reserve_bytes: 256 * MIB,
        filesystem_reserve_percent: 5,
        retained_wal_high_water_bytes: 64 * MIB,
        wal_autocheckpoint_pages: 1_000,
    }
}

fn assert_server_image_arithmetic() {
    let limits = server_sqlite_limits();
    assert!(limits.max_total_sqlite_bytes + limits.filesystem_reserve_bytes <= 4 * GIB);
}

fn server_limits() -> StoreLimits {
    let allowed_source_ids = ["scale-core", "scale-knots"]
        .into_iter()
        .map(|value| SourceId::new(value).expect("allowed scale source"))
        .collect::<BTreeSet<_>>();
    StoreLimits::new(
        server_sqlite_limits(),
        2,
        BASELINE_TXIDS,
        Some(allowed_source_ids),
    )
    .expect("scale server limits")
}

fn print_agent_storage(phase: &str, storage: &SourceReplicaStorageStats) {
    println!(
        "scale_storage phase={phase} membership_rows={} dirty_rows={} frozen_checkpoint_rows={} database_bytes={} wal_bytes={} shm_bytes={} total_sqlite_bytes={} page_count={} freelist_count={} max_page_count={} remaining_envelope_bytes={}",
        storage.membership_rows,
        storage.dirty_rows,
        storage.frozen_checkpoint_rows,
        storage.database_bytes,
        storage.wal_bytes,
        storage.shm_bytes,
        storage.total_sqlite_bytes,
        storage.page_count,
        storage.freelist_count,
        storage.max_page_count,
        storage.remaining_envelope_bytes,
    );
}

fn print_server_storage(phase: &str, snapshot: &SqlitePhysicalSnapshot) {
    println!(
        "scale_storage phase={phase} database_bytes={} wal_bytes={} shm_bytes={} total_sqlite_bytes={} page_count={} freelist_count={} max_page_count={} remaining_envelope_bytes={}",
        snapshot.database_bytes,
        snapshot.wal_bytes,
        snapshot.shm_bytes,
        snapshot.total_sqlite_bytes,
        snapshot.page_count,
        snapshot.freelist_count,
        snapshot.max_page_count,
        snapshot.remaining_envelope_bytes,
    );
}

fn assert_server_storage(
    store: &Store,
    phase: &str,
    regression_ceiling_bytes: u64,
) -> SqlitePhysicalSnapshot {
    let readiness = store.readiness().expect("server readiness");
    assert!(readiness.is_ready(), "{:?}", readiness.pressure);
    let snapshot = readiness.snapshot;
    print_server_storage(phase, &snapshot);
    assert!(snapshot.database_bytes <= 3 * GIB);
    assert!(snapshot.total_sqlite_bytes <= 3_584 * MIB);
    assert!(snapshot.wal_bytes <= 64 * MIB);
    assert!(
        snapshot.total_sqlite_bytes < regression_ceiling_bytes,
        "{phase} used {} bytes against regression ceiling {regression_ceiling_bytes}",
        snapshot.total_sqlite_bytes
    );
    snapshot
}

fn scale_entry(value: u64, revision: u64) -> SourceReplicaEntry {
    SourceReplicaEntry::new(
        format!("{value:064x}"),
        MempoolEntryFacts {
            vsize: 140 + revision,
            fee_sats: 1_000 + revision,
            entered_at_ms: 1_721_234_000_000 + revision,
        },
    )
    .expect("scale entry")
}

fn checkpoint_digest(revision: u64) -> String {
    let mut digest = CheckpointDigest::new(BASELINE_TXIDS).expect("checkpoint digest");
    for value in 1..=BASELINE_TXIDS {
        digest
            .push(&scale_entry(value, revision))
            .expect("digest entry");
    }
    digest.finish().expect("complete checkpoint digest")
}

fn apply_scale_checkpoint(
    store: &Store,
    source_id: &SourceId,
    epoch_id: &SourceEpochId,
    checkpoint_id: &CheckpointId,
    replaces: Option<ReplicaCursor>,
    revision: u64,
    commit: bool,
) -> Option<ReplicaCursor> {
    let expected_chunks = u32::try_from(
        usize::try_from(BASELINE_TXIDS)
            .expect("baseline count fits usize")
            .div_ceil(MAX_CHECKPOINT_CHUNK_ENTRIES),
    )
    .expect("checkpoint count fits u32");
    let digest = checkpoint_digest(revision);
    let begin = CheckpointBegin {
        checkpoint_id: checkpoint_id.clone(),
        supersedes_checkpoint_id: None,
        replaces,
        target_revision: revision,
        state_observed_at_ms: 1_721_234_000_000 + revision,
        expected_entries: BASELINE_TXIDS,
        expected_chunks,
        content_sha256: digest.clone(),
    };
    let begin_request = SourceReplicaRequest::new(
        source_id.clone(),
        epoch_id.clone(),
        SourceReplicaCommand::CheckpointBegin(begin),
    )
    .expect("checkpoint begin request");
    let begin_response = store
        .apply_source_replica(&begin_request)
        .expect("apply checkpoint begin");
    assert!(matches!(
        begin_response,
        SourceReplicaResponse::Applied { .. }
    ));

    for chunk_index in 0..expected_chunks {
        let first = u64::from(chunk_index)
            * u64::try_from(MAX_CHECKPOINT_CHUNK_ENTRIES).expect("chunk size fits u64")
            + 1;
        let last =
            (first + u64::try_from(MAX_CHECKPOINT_CHUNK_ENTRIES).expect("chunk size fits u64") - 1)
                .min(BASELINE_TXIDS);
        let entries = (first..=last)
            .map(|value| scale_entry(value, revision))
            .collect();
        let chunk = CheckpointChunk::new(checkpoint_id.clone(), chunk_index, entries)
            .expect("checkpoint chunk");
        let request = SourceReplicaRequest::new(
            source_id.clone(),
            epoch_id.clone(),
            SourceReplicaCommand::CheckpointChunk(chunk),
        )
        .expect("checkpoint chunk request");
        let response = store
            .apply_source_replica(&request)
            .expect("apply checkpoint chunk");
        let progress = response.progress().expect("checkpoint progress");
        assert_eq!(progress.received_chunks, chunk_index + 1);
        if chunk_index + 1 == expected_chunks {
            assert_eq!(progress.received_entries, BASELINE_TXIDS);
        }
    }

    if !commit {
        return None;
    }
    let commit =
        CheckpointCommit::new(checkpoint_id.clone(), revision, digest).expect("checkpoint commit");
    let request = SourceReplicaRequest::new(
        source_id.clone(),
        epoch_id.clone(),
        SourceReplicaCommand::CheckpointCommit(commit),
    )
    .expect("checkpoint commit request");
    let response = store
        .apply_source_replica(&request)
        .expect("apply checkpoint commit");
    response.active_cursor().cloned()
}

fn scale_store(path: &std::path::Path) -> Store {
    let sqlite = server_sqlite_limits();
    Store::migrate_with_limits(path, &sqlite).expect("migrate central database");
    Store::open_with_limits(path, server_limits()).expect("open scale store")
}

#[tokio::test]
#[ignore = "200,000-txid SourceReplica baseline acceptance"]
async fn two_hundred_thousand_txid_baseline_survives_restart_and_activates_atomically() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let replica_path = temporary.path().join("agent.db");
    let store_path = temporary.path().join("atlas.db");
    schema::migrate(&replica_path).expect("migrate agent database");
    let store = scale_store(&store_path);

    let replica = SourceReplica::open_with_storage_policy(
        &replica_path,
        source(),
        agent_limits(),
        agent_storage_policy(),
    )
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
    let frozen_storage = replica.storage_stats().expect("frozen agent storage");
    print_agent_storage("agent_frozen_checkpoint", &frozen_storage);
    assert_eq!(frozen_storage.membership_rows, BASELINE_TXIDS);
    assert_eq!(frozen_storage.frozen_checkpoint_rows, BASELINE_TXIDS);
    assert!(frozen_storage.total_sqlite_bytes < AGENT_FROZEN_REGRESSION_BYTES);
    drop(replica);

    // Both databases are deliberately reopened before delivery. The agent
    // must retain the exact frozen checkpoint and the server starts solely
    // from its durable, migrated state.
    let replica = SourceReplica::open_with_storage_policy(
        &replica_path,
        source(),
        agent_limits(),
        agent_storage_policy(),
    )
    .expect("reopen source replica");
    assert_eq!(
        replica.next_action().expect("reopen frozen action"),
        Some(frozen)
    );
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
    print_agent_storage("agent_acknowledged", &storage);
    assert_server_storage(
        &store,
        "server_one_active",
        SERVER_ONE_ACTIVE_REGRESSION_BYTES,
    );

    server.abort();
    let server_error = server.await.expect_err("aborted state server");
    assert!(server_error.is_cancelled());
    drop(store);

    // A second central-store reopen verifies that activation, not merely the
    // serving process's connection state, owns the complete baseline.
    let reopened_store =
        Store::open_with_limits(&store_path, server_limits()).expect("reopen activated store");
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

#[test]
#[ignore = "two active 200,000-entry server generations"]
fn two_hundred_thousand_txid_two_source_active_footprint() {
    assert_server_image_arithmetic();
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = scale_store(&temporary.path().join("atlas.db"));
    for (source_name, epoch_name, checkpoint_name) in [
        ("scale-core", "scale-core-epoch", "scale-core-r1"),
        ("scale-knots", "scale-knots-epoch", "scale-knots-r1"),
    ] {
        let cursor = apply_scale_checkpoint(
            &store,
            &SourceId::new(source_name).expect("source"),
            &SourceEpochId::new(epoch_name).expect("epoch"),
            &CheckpointId::new(checkpoint_name).expect("checkpoint"),
            None,
            1,
            true,
        )
        .expect("active cursor");
        assert_eq!(cursor.revision, 1);
    }
    assert_server_storage(
        &store,
        "server_two_active",
        SERVER_TWO_ACTIVE_REGRESSION_BYTES,
    );
}

#[test]
#[ignore = "two active plus two staging 200,000-entry server generations"]
fn two_hundred_thousand_txid_two_source_active_and_staging_footprint() {
    assert_server_image_arithmetic();
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = scale_store(&temporary.path().join("atlas.db"));
    let sources = [
        (
            SourceId::new("scale-core").expect("source"),
            SourceEpochId::new("scale-core-epoch").expect("epoch"),
            CheckpointId::new("scale-core-r1").expect("checkpoint"),
            CheckpointId::new("scale-core-r2").expect("checkpoint"),
        ),
        (
            SourceId::new("scale-knots").expect("source"),
            SourceEpochId::new("scale-knots-epoch").expect("epoch"),
            CheckpointId::new("scale-knots-r1").expect("checkpoint"),
            CheckpointId::new("scale-knots-r2").expect("checkpoint"),
        ),
    ];
    let mut active_cursors = Vec::with_capacity(sources.len());
    for (source_id, epoch_id, active_checkpoint, _) in &sources {
        active_cursors.push(
            apply_scale_checkpoint(
                &store,
                source_id,
                epoch_id,
                active_checkpoint,
                None,
                1,
                true,
            )
            .expect("active cursor"),
        );
    }
    for ((source_id, epoch_id, _, staging_checkpoint), active_cursor) in
        sources.iter().zip(active_cursors)
    {
        assert!(
            apply_scale_checkpoint(
                &store,
                source_id,
                epoch_id,
                staging_checkpoint,
                Some(active_cursor),
                2,
                false,
            )
            .is_none()
        );
    }
    assert_server_storage(
        &store,
        "server_two_active_two_staging",
        SERVER_ACTIVE_STAGING_REGRESSION_BYTES,
    );
}
