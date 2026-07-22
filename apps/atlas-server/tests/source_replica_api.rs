use std::collections::BTreeSet;

use atlas_model::{
    CheckpointBegin, CheckpointChunk, CheckpointCommit, CheckpointId, MAX_CHECKPOINT_ENTRIES,
    MempoolEntryFacts, ReplicaCursor, SOURCE_REPLICA_PROTOCOL_VERSION, SourceEpochId, SourceId,
    SourceReplicaCommand, SourceReplicaEntry, SourceReplicaRequest, SourceReplicaResponse,
    StateDelta, StateHeartbeat, StateMutation,
};
use atlas_server::{Store, StoreLimits, router};
use atlas_storage::SqliteStorageLimits;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use rusqlite::Connection;
use serde_json::Value;
use tower::ServiceExt;

const TXID_A: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const TXID_B: &str = "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f";
const TXID_C: &str = "404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f";

fn bounded_limits(max_sources: usize, allowed_source_ids: Option<&[&str]>) -> StoreLimits {
    bounded_membership_limits(
        max_sources,
        StoreLimits::default().max_membership_entries,
        allowed_source_ids,
    )
}

fn bounded_membership_limits(
    max_sources: usize,
    max_membership_entries: u64,
    allowed_source_ids: Option<&[&str]>,
) -> StoreLimits {
    let allowed_source_ids = allowed_source_ids.map(|values| {
        values
            .iter()
            .map(|value| SourceId::new(*value).expect("allowed source"))
            .collect::<BTreeSet<_>>()
    });
    StoreLimits::new(
        StoreLimits::default().sqlite,
        max_sources,
        max_membership_entries,
        allowed_source_ids,
    )
    .expect("store limits")
}

fn epoch(value: &str) -> SourceEpochId {
    SourceEpochId::new(value).expect("epoch")
}

fn checkpoint(value: &str) -> CheckpointId {
    CheckpointId::new(value).expect("checkpoint")
}

fn facts(seed: u64) -> MempoolEntryFacts {
    MempoolEntryFacts {
        vsize: 100 + seed,
        fee_sats: 1_000 + seed,
        entered_at_ms: 1_700_000_000_000 + seed,
    }
}

fn entry(txid: &str, seed: u64) -> SourceReplicaEntry {
    SourceReplicaEntry::new(txid, facts(seed)).expect("entry")
}

fn state_request_for(
    source_id: &str,
    epoch_id: &SourceEpochId,
    command: SourceReplicaCommand,
) -> SourceReplicaRequest {
    SourceReplicaRequest::new(
        SourceId::new(source_id).expect("source"),
        epoch_id.clone(),
        command,
    )
    .expect("state request")
}

fn state_request(epoch_id: &SourceEpochId, command: SourceReplicaCommand) -> SourceReplicaRequest {
    state_request_for("source-a", epoch_id, command)
}

async fn post_state(application: &Router, request: &SourceReplicaRequest) -> (StatusCode, Value) {
    post_json(
        application,
        serde_json::to_value(request).expect("request JSON"),
    )
    .await
}

async fn post_json(application: &Router, body: Value) -> (StatusCode, Value) {
    let response = application
        .clone()
        .oneshot(
            Request::post("/api/v1/state")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).expect("JSON body")))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let body = serde_json::from_slice(&bytes).expect("response JSON");
    (status, body)
}

async fn get_json(application: &Router, path: &str) -> (StatusCode, Value) {
    let response = application
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let body = serde_json::from_slice(&bytes).expect("response JSON");
    (status, body)
}

fn assert_empty_staging(database: &std::path::Path, expected_checkpoint: &CheckpointId) {
    let connection = Connection::open(database).expect("inspect staging generation");
    let (checkpoint_id, chunk_rows, membership_rows): (String, i64, i64) = connection
        .query_row(
            "SELECT generation.checkpoint_id,
                    (SELECT COUNT(*) FROM source_replica_checkpoint_chunk AS chunk
                     WHERE chunk.source_id = generation.source_id
                       AND chunk.generation_id = generation.generation_id),
                    (SELECT COUNT(*) FROM source_replica_membership AS membership
                     WHERE membership.source_id = generation.source_id
                       AND membership.generation_id = generation.generation_id)
             FROM source_replica_generation AS generation
             WHERE generation.source_id = 'source-a' AND generation.role = 'staging'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("staging generation");
    assert_eq!(checkpoint_id, expected_checkpoint.as_str());
    assert_eq!((chunk_rows, membership_rows), (0, 0));
}

async fn install_checkpoint(
    application: &Router,
    epoch_id: &SourceEpochId,
    checkpoint_id: &CheckpointId,
    replaces: Option<ReplicaCursor>,
    target_revision: u64,
    entries: &[SourceReplicaEntry],
) -> ReplicaCursor {
    install_checkpoint_for_source(
        application,
        "source-a",
        epoch_id,
        checkpoint_id,
        replaces,
        target_revision,
        entries,
    )
    .await
}

async fn install_checkpoint_for_source(
    application: &Router,
    source_id: &str,
    epoch_id: &SourceEpochId,
    checkpoint_id: &CheckpointId,
    replaces: Option<ReplicaCursor>,
    target_revision: u64,
    entries: &[SourceReplicaEntry],
) -> ReplicaCursor {
    let expected_chunks = u32::from(!entries.is_empty());
    let begin = CheckpointBegin::new(
        checkpoint_id.clone(),
        replaces,
        target_revision,
        1_700_000_100_000 + target_revision,
        expected_chunks,
        entries,
    )
    .expect("checkpoint begin");
    let (status, response) = post_state(
        application,
        &state_request_for(
            source_id,
            epoch_id,
            SourceReplicaCommand::CheckpointBegin(begin.clone()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{response}");

    if !entries.is_empty() {
        let chunk = CheckpointChunk::new(checkpoint_id.clone(), 0, entries.to_vec())
            .expect("checkpoint chunk");
        let (status, response) = post_state(
            application,
            &state_request_for(
                source_id,
                epoch_id,
                SourceReplicaCommand::CheckpointChunk(chunk),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{response}");
    }

    let commit =
        CheckpointCommit::new(checkpoint_id.clone(), target_revision, begin.content_sha256)
            .expect("checkpoint commit");
    let (status, response) = post_state(
        application,
        &state_request_for(
            source_id,
            epoch_id,
            SourceReplicaCommand::CheckpointCommit(commit),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{response}");
    let response: SourceReplicaResponse = serde_json::from_value(response).expect("state response");
    response
        .active_cursor()
        .cloned()
        .expect("committed checkpoint is active")
}

#[tokio::test]
async fn persistent_source_identity_cap_counts_staging_only_sources_inside_the_writer_transaction()
{
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store = Store::open_with_limits(&database, bounded_limits(1, None)).expect("store");
    let application = router(store.clone());

    let epoch_a = epoch("epoch-a");
    let begin_a = CheckpointBegin::new(
        checkpoint("checkpoint-a"),
        None,
        1,
        1_700_000_000_001,
        0,
        &[],
    )
    .expect("source A begin");
    let (status, body) = post_state(
        &application,
        &state_request_for(
            "source-a",
            &epoch_a,
            SourceReplicaCommand::CheckpointBegin(begin_a),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let begin_b = CheckpointBegin::new(
        checkpoint("checkpoint-b"),
        None,
        1,
        1_700_000_000_002,
        0,
        &[],
    )
    .expect("source B begin");
    let (status, body) = post_state(
        &application,
        &state_request_for(
            "source-b",
            &epoch("epoch-b"),
            SourceReplicaCommand::CheckpointBegin(begin_b),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "capacity_exceeded");
    assert!(
        body["error"]
            .as_str()
            .expect("error")
            .contains("maximum of 1")
    );

    let connection = Connection::open(&database).expect("inspect database");
    let (sources, active, staging): (i64, i64, i64) = connection
        .query_row(
            "SELECT COUNT(DISTINCT source_id),
                    SUM(role = 'active'),
                    SUM(role = 'staging')
             FROM source_replica_generation",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("source counts");
    assert_eq!((sources, active, staging), (1, 0, 1));
}

#[tokio::test]
async fn persistent_source_identity_cap_is_revalidated_when_the_store_reopens() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store = Store::open_with_limits(&database, bounded_limits(2, None)).expect("store");
    let application = router(store);
    for (source_id, epoch_id, checkpoint_id) in [
        ("source-a", "epoch-a", "checkpoint-a"),
        ("source-b", "epoch-b", "checkpoint-b"),
    ] {
        install_checkpoint_for_source(
            &application,
            source_id,
            &epoch(epoch_id),
            &checkpoint(checkpoint_id),
            None,
            1,
            &[],
        )
        .await;
    }
    drop(application);

    assert!(matches!(
        Store::open_with_limits(&database, bounded_limits(1, None)),
        Err(
            atlas_server::StoreError::ExistingSourceIdentityLimitExceeded {
                found: 2,
                maximum: 1
            }
        )
    ));
}

#[tokio::test]
async fn exact_source_allowlist_rejects_an_unlisted_source_before_creation() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store =
        Store::open_with_limits(&database, bounded_limits(2, Some(&["source-a"]))).expect("store");
    let application = router(store);

    let begin = CheckpointBegin::new(
        checkpoint("checkpoint-b"),
        None,
        1,
        1_700_000_000_001,
        0,
        &[],
    )
    .expect("begin");
    let (status, body) = post_state(
        &application,
        &state_request_for(
            "source-b",
            &epoch("epoch-b"),
            SourceReplicaCommand::CheckpointBegin(begin),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "capacity_exceeded");
    assert!(body["error"].as_str().expect("error").contains("allowlist"));

    let connection = Connection::open(&database).expect("inspect database");
    let sources: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM source_replica_generation",
            [],
            |row| row.get(0),
        )
        .expect("generation count");
    assert_eq!(sources, 0);
}

#[tokio::test]
async fn restricted_store_rechecks_allowlist_after_another_store_admits_a_source() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let restricted_store =
        Store::open_with_limits(&database, bounded_limits(2, Some(&["source-a"])))
            .expect("restricted store");
    let permissive_store = Store::open(&database).expect("permissive store");
    let permissive_application = router(permissive_store);
    let source_epoch = epoch("epoch-b");
    let active_cursor = install_checkpoint_for_source(
        &permissive_application,
        "source-b",
        &source_epoch,
        &checkpoint("checkpoint-b"),
        None,
        1,
        &[entry(TXID_A, 1)],
    )
    .await;
    let restricted_application = router(restricted_store.clone());
    let replacement_entries = vec![entry(TXID_B, 2)];
    let replacement = CheckpointBegin::new(
        checkpoint("checkpoint-b-replacement"),
        Some(active_cursor),
        2,
        1_700_000_100_002,
        1,
        &replacement_entries,
    )
    .expect("replacement begin");
    let commands = vec![
        SourceReplicaCommand::Heartbeat(
            StateHeartbeat::new(1, 1_700_000_100_003).expect("heartbeat"),
        ),
        SourceReplicaCommand::Delta(
            StateDelta::new(
                1,
                2,
                1_700_000_100_004,
                vec![StateMutation::Present {
                    txid: TXID_B.to_owned(),
                    facts: facts(2),
                }],
            )
            .expect("delta"),
        ),
        SourceReplicaCommand::CheckpointBegin(replacement.clone()),
        SourceReplicaCommand::CheckpointChunk(
            CheckpointChunk::new(replacement.checkpoint_id.clone(), 0, replacement_entries)
                .expect("chunk"),
        ),
        SourceReplicaCommand::CheckpointCommit(
            CheckpointCommit::new(
                replacement.checkpoint_id,
                replacement.target_revision,
                replacement.content_sha256,
            )
            .expect("commit"),
        ),
    ];

    for command in commands {
        let (status, body) = post_state(
            &restricted_application,
            &state_request_for("source-b", &source_epoch, command),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["code"], "capacity_exceeded");
        assert!(body["error"].as_str().expect("error").contains("allowlist"));
        assert_eq!(body["active_cursor"]["revision"], 1);
    }

    let visible = restricted_store
        .active_source_replica(&SourceId::new("source-b").expect("source"))
        .expect("active read")
        .expect("permissively admitted source remains readable");
    assert_eq!(visible.cursor.revision, 1);
    assert_eq!(visible.entries, vec![entry(TXID_A, 1)]);
}

#[tokio::test]
async fn configured_membership_cap_rejects_checkpoint_declarations_and_foreign_chunks() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let restricted_store =
        Store::open_with_limits(&database, bounded_membership_limits(1, 1, None))
            .expect("restricted store");
    let restricted_application = router(restricted_store);
    let source_epoch = epoch("epoch-a");
    let checkpoint_entries = vec![entry(TXID_A, 1), entry(TXID_B, 2)];
    let begin = CheckpointBegin::new(
        checkpoint("checkpoint-too-large"),
        None,
        1,
        1_700_000_100_000,
        1,
        &checkpoint_entries,
    )
    .expect("checkpoint begin");
    let (status, body) = post_state(
        &restricted_application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointBegin(begin.clone()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "capacity_exceeded");
    assert!(
        body["error"]
            .as_str()
            .expect("error")
            .contains("maximum is 1")
    );

    let permissive_store =
        Store::open_with_limits(&database, bounded_membership_limits(1, 2, None))
            .expect("permissive store");
    let permissive_application = router(permissive_store);
    let (status, body) = post_state(
        &permissive_application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointBegin(begin.clone()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let chunk = CheckpointChunk::new(
        begin.checkpoint_id.clone(),
        0,
        vec![checkpoint_entries[0].clone()],
    )
    .expect("checkpoint chunk");
    let (status, body) = post_state(
        &restricted_application,
        &state_request(&source_epoch, SourceReplicaCommand::CheckpointChunk(chunk)),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "capacity_exceeded");

    let full_chunk =
        CheckpointChunk::new(begin.checkpoint_id.clone(), 0, checkpoint_entries.clone())
            .expect("complete checkpoint chunk");
    let (status, body) = post_state(
        &permissive_application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointChunk(full_chunk),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let commit = CheckpointCommit::new(
        begin.checkpoint_id,
        begin.target_revision,
        begin.content_sha256,
    )
    .expect("checkpoint commit");
    let (status, body) = post_state(
        &restricted_application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointCommit(commit),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "capacity_exceeded");

    let connection = Connection::open(&database).expect("inspect staging generation");
    let (active, staging, staged_entries): (i64, i64, i64) = connection
        .query_row(
            "SELECT SUM(role = 'active'), SUM(role = 'staging'),
                    (SELECT COUNT(*) FROM source_replica_membership)
             FROM source_replica_generation",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("staging state");
    assert_eq!((active, staging, staged_entries), (0, 1, 2));
}

#[tokio::test]
async fn configured_membership_cap_rolls_back_an_oversized_delta() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store =
        Store::open_with_limits(&database, bounded_membership_limits(1, 1, None)).expect("store");
    let application = router(store.clone());
    let source_epoch = epoch("epoch-a");
    install_checkpoint(
        &application,
        &source_epoch,
        &checkpoint("checkpoint-a"),
        None,
        1,
        &[entry(TXID_A, 1)],
    )
    .await;
    let replacement_delta = StateDelta::new(
        1,
        2,
        1_700_000_200_000,
        vec![
            StateMutation::Absent {
                txid: TXID_A.to_owned(),
            },
            StateMutation::Present {
                txid: TXID_B.to_owned(),
                facts: facts(2),
            },
        ],
    )
    .expect("net-neutral replacement delta");
    let (status, body) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::Delta(replacement_delta),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["active_cursor"]["revision"], 2);

    let oversized_delta = StateDelta::new(
        2,
        3,
        1_700_000_200_001,
        vec![StateMutation::Present {
            txid: TXID_C.to_owned(),
            facts: facts(3),
        }],
    )
    .expect("oversized delta");

    let (status, body) = post_state(
        &application,
        &state_request(&source_epoch, SourceReplicaCommand::Delta(oversized_delta)),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "capacity_exceeded");
    assert_eq!(body["active_cursor"]["revision"], 2);
    let visible = store
        .active_source_replica(&SourceId::new("source-a").expect("source"))
        .expect("active read")
        .expect("active generation");
    assert_eq!(visible.cursor.revision, 2);
    assert_eq!(visible.entries, vec![entry(TXID_B, 2)]);
}

#[tokio::test]
async fn opening_store_rejects_declared_or_actual_membership_above_its_cap() {
    let declared = tempfile::tempdir().expect("temporary directory");
    let declared_database = declared.path().join("atlas.db");
    Store::migrate(&declared_database).expect("migrate");
    let permissive_store =
        Store::open_with_limits(&declared_database, bounded_membership_limits(1, 2, None))
            .expect("permissive store");
    let application = router(permissive_store);
    let entries = vec![entry(TXID_A, 1), entry(TXID_B, 2)];
    let begin = CheckpointBegin::new(
        checkpoint("checkpoint-declared"),
        None,
        1,
        1_700_000_300_000,
        1,
        &entries,
    )
    .expect("checkpoint begin");
    let (status, body) = post_state(
        &application,
        &state_request(
            &epoch("epoch-declared"),
            SourceReplicaCommand::CheckpointBegin(begin),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    drop(application);
    assert!(matches!(
        Store::open_with_limits(&declared_database, bounded_membership_limits(1, 1, None)),
        Err(
            atlas_server::StoreError::ExistingGenerationMembershipLimitExceeded {
                declared_entries: 2,
                actual_entries: 0,
                maximum: 1,
                ..
            }
        )
    ));

    let actual = tempfile::tempdir().expect("temporary directory");
    let actual_database = actual.path().join("atlas.db");
    Store::migrate(&actual_database).expect("migrate");
    let permissive_store =
        Store::open_with_limits(&actual_database, bounded_membership_limits(1, 2, None))
            .expect("permissive store");
    let application = router(permissive_store);
    let source_epoch = epoch("epoch-actual");
    install_checkpoint(
        &application,
        &source_epoch,
        &checkpoint("checkpoint-actual"),
        None,
        1,
        &[entry(TXID_A, 1)],
    )
    .await;
    let delta = StateDelta::new(
        1,
        2,
        1_700_000_300_001,
        vec![StateMutation::Present {
            txid: TXID_B.to_owned(),
            facts: facts(2),
        }],
    )
    .expect("delta");
    let (status, body) = post_state(
        &application,
        &state_request(&source_epoch, SourceReplicaCommand::Delta(delta)),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    drop(application);
    assert!(matches!(
        Store::open_with_limits(&actual_database, bounded_membership_limits(1, 1, None)),
        Err(
            atlas_server::StoreError::ExistingGenerationMembershipLimitExceeded {
                declared_entries: 1,
                actual_entries: 2,
                maximum: 1,
                ..
            }
        )
    ));
}

#[tokio::test]
async fn storage_pressure_preserves_active_and_staging_state_then_recovers() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store = Store::open(&database).expect("store");
    let application = router(store.clone());
    let source_epoch = epoch("epoch-a");
    let old_cursor = install_checkpoint(
        &application,
        &source_epoch,
        &checkpoint("checkpoint-old"),
        None,
        1,
        &[entry(TXID_A, 1)],
    )
    .await;

    let new_checkpoint = checkpoint("checkpoint-new");
    let new_entries = vec![entry(TXID_B, 2)];
    let begin = CheckpointBegin::new(
        new_checkpoint.clone(),
        Some(old_cursor.clone()),
        2,
        1_700_000_200_000,
        1,
        &new_entries,
    )
    .expect("checkpoint begin");
    let begin_request = state_request(
        &source_epoch,
        SourceReplicaCommand::CheckpointBegin(begin.clone()),
    );
    let chunk_request = state_request(
        &source_epoch,
        SourceReplicaCommand::CheckpointChunk(
            CheckpointChunk::new(new_checkpoint.clone(), 0, new_entries.clone())
                .expect("checkpoint chunk"),
        ),
    );
    let commit_request = state_request(
        &source_epoch,
        SourceReplicaCommand::CheckpointCommit(
            CheckpointCommit::new(new_checkpoint, 2, begin.content_sha256)
                .expect("checkpoint commit"),
        ),
    );
    for request in [&begin_request, &chunk_request] {
        let (status, body) = post_state(&application, request).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    }

    let filesystem_total_bytes = store
        .readiness()
        .expect("storage snapshot")
        .snapshot
        .filesystem_total_bytes;
    drop(application);
    drop(store);

    let connection = Connection::open(&database).expect("inspect database");
    let page_size = u64::try_from(
        connection
            .pragma_query_value(None, "page_size", |row| row.get::<_, i64>(0))
            .expect("page size"),
    )
    .expect("nonnegative page size");
    let page_count = u64::try_from(
        connection
            .pragma_query_value(None, "page_count", |row| row.get::<_, i64>(0))
            .expect("page count"),
    )
    .expect("nonnegative page count");
    drop(connection);
    let max_database_bytes = page_size * page_count;
    let pressure_storage = SqliteStorageLimits {
        max_database_bytes,
        max_total_sqlite_bytes: max_database_bytes + 1,
        filesystem_reserve_bytes: filesystem_total_bytes,
        filesystem_reserve_percent: 1,
        retained_wal_high_water_bytes: 1,
        wal_autocheckpoint_pages: 1,
    };
    let pressure_limits = StoreLimits::new(
        pressure_storage,
        4,
        StoreLimits::default().max_membership_entries,
        None,
    )
    .expect("pressure store limits");
    let pressure_store =
        Store::open_with_limits(&database, pressure_limits).expect("pressure store");
    let pressure_application = router(pressure_store.clone());

    let (status, body) = post_state(&pressure_application, &chunk_request).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["status"], "duplicate");

    let (status, body) = get_json(&pressure_application, "/healthz").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "ok");
    let (status, body) = get_json(&pressure_application, "/readyz").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["status"], "not_ready");
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| { error.contains("envelope") || error.contains("filesystem") })
    );
    assert!(body["storage"]["database_bytes"].is_u64());

    let (status, body) = post_state(&pressure_application, &commit_request).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "capacity_exceeded");
    assert_eq!(body["active_cursor"]["epoch_id"], source_epoch.as_str());
    assert_eq!(body["active_cursor"]["revision"], 1);

    let visible = pressure_store
        .active_source_replica(&SourceId::new("source-a").expect("source"))
        .expect("active read")
        .expect("old active generation");
    assert_eq!(visible.cursor, old_cursor);
    assert_eq!(visible.entries, vec![entry(TXID_A, 1)]);
    let connection = Connection::open(&database).expect("inspect preserved state");
    let (active, staging, active_entries, staging_entries): (i64, i64, i64, i64) = connection
        .query_row(
            "SELECT
                SUM(role = 'active'),
                SUM(role = 'staging'),
                (SELECT COUNT(*) FROM source_replica_membership AS membership
                 JOIN source_replica_generation AS generation
                   USING (source_id, generation_id)
                 WHERE generation.role = 'active'),
                (SELECT COUNT(*) FROM source_replica_membership AS membership
                 JOIN source_replica_generation AS generation
                   USING (source_id, generation_id)
                 WHERE generation.role = 'staging')
             FROM source_replica_generation WHERE source_id = 'source-a'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("generation counts");
    assert_eq!(
        (active, staging, active_entries, staging_entries),
        (1, 1, 1, 1)
    );
    drop(connection);
    drop(pressure_application);
    drop(pressure_store);

    let recovered_store = Store::open(&database).expect("recovered store");
    let recovered_application = router(recovered_store.clone());
    let (status, body) = post_state(&recovered_application, &commit_request).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let visible = recovered_store
        .active_source_replica(&SourceId::new("source-a").expect("source"))
        .expect("active read")
        .expect("new active generation");
    assert_eq!(visible.cursor.revision, 2);
    assert_eq!(visible.entries, new_entries);
    let (status, body) = get_json(&recovered_application, "/readyz").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "ready");
}

#[tokio::test]
async fn sqlite_full_is_a_capacity_conflict_and_rolls_back_the_delta() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store = Store::open(&database).expect("store");
    let application = router(store.clone());
    let source_epoch = epoch("epoch-a");
    install_checkpoint(
        &application,
        &source_epoch,
        &checkpoint("checkpoint-empty"),
        None,
        1,
        &[],
    )
    .await;
    drop(application);
    drop(store);

    let connection = Connection::open(&database).expect("compact database");
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")
        .expect("compact database");
    let page_size = u64::try_from(
        connection
            .pragma_query_value(None, "page_size", |row| row.get::<_, i64>(0))
            .expect("page size"),
    )
    .expect("nonnegative page size");
    let page_count = u64::try_from(
        connection
            .pragma_query_value(None, "page_count", |row| row.get::<_, i64>(0))
            .expect("page count"),
    )
    .expect("nonnegative page count");
    drop(connection);

    let max_database_bytes = page_size * page_count;
    let storage = SqliteStorageLimits {
        max_database_bytes,
        max_total_sqlite_bytes: max_database_bytes + 64 * 1024 * 1024,
        filesystem_reserve_bytes: 1,
        filesystem_reserve_percent: 1,
        retained_wal_high_water_bytes: 64 * 1024 * 1024,
        wal_autocheckpoint_pages: 1_000,
    };
    let store = Store::open_with_limits(
        &database,
        StoreLimits::new(
            storage,
            4,
            StoreLimits::default().max_membership_entries,
            None,
        )
        .expect("store limits"),
    )
    .expect("capped store");
    let application = router(store.clone());
    let mutations = (1_u64..=4_096)
        .map(|seed| StateMutation::Present {
            txid: format!("{seed:064x}"),
            facts: facts(seed),
        })
        .collect();
    let delta = StateDelta::new(1, 2, 1_700_000_300_000, mutations).expect("large delta");
    let (status, body) = post_state(
        &application,
        &state_request(&source_epoch, SourceReplicaCommand::Delta(delta)),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "capacity_exceeded");
    assert_eq!(body["active_cursor"]["revision"], 1);
    let visible = store
        .active_source_replica(&SourceId::new("source-a").expect("source"))
        .expect("active read")
        .expect("active generation");
    assert_eq!(visible.cursor.revision, 1);
    assert!(visible.entries.is_empty());
    let connection = Connection::open(&database).expect("inspect database");
    let integrity: String = connection
        .pragma_query_value(None, "integrity_check", |row| row.get(0))
        .expect("integrity check");
    assert_eq!(integrity, "ok");
}

#[tokio::test]
async fn failed_commit_keeps_old_generation_and_success_exposes_only_complete_new_generation() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store = Store::open(database).expect("store");
    let application = router(store.clone());
    let source = SourceId::new("source-a").expect("source");

    let old_epoch = epoch("epoch-old");
    let old_cursor = install_checkpoint(
        &application,
        &old_epoch,
        &checkpoint("checkpoint-old"),
        None,
        1,
        &[entry(TXID_A, 1)],
    )
    .await;

    let new_epoch = epoch("epoch-new");
    let new_checkpoint = checkpoint("checkpoint-new");
    let new_entries = vec![entry(TXID_B, 2), entry(TXID_C, 3)];
    let begin = CheckpointBegin::new(
        new_checkpoint.clone(),
        Some(old_cursor.clone()),
        1,
        1_700_000_200_000,
        2,
        &new_entries,
    )
    .expect("checkpoint begin");
    let (status, _) = post_state(
        &application,
        &state_request(
            &new_epoch,
            SourceReplicaCommand::CheckpointBegin(begin.clone()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, body) = post_state(
        &application,
        &state_request(
            &new_epoch,
            SourceReplicaCommand::CheckpointBegin(begin.clone()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["status"], "duplicate");
    assert_eq!(body["active_cursor"]["epoch_id"], old_epoch.as_str());

    let early_second_chunk =
        CheckpointChunk::new(new_checkpoint.clone(), 1, vec![new_entries[1].clone()])
            .expect("early second chunk");
    let (status, body) = post_state(
        &application,
        &state_request(
            &new_epoch,
            SourceReplicaCommand::CheckpointChunk(early_second_chunk),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["code"], "invalid_state_request");

    let first_chunk = CheckpointChunk::new(new_checkpoint.clone(), 0, vec![new_entries[0].clone()])
        .expect("first chunk");
    let (status, _) = post_state(
        &application,
        &state_request(
            &new_epoch,
            SourceReplicaCommand::CheckpointChunk(first_chunk),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let first_chunk_retry =
        CheckpointChunk::new(new_checkpoint.clone(), 0, vec![new_entries[0].clone()])
            .expect("first chunk retry");
    let (status, body) = post_state(
        &application,
        &state_request(
            &new_epoch,
            SourceReplicaCommand::CheckpointChunk(first_chunk_retry),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["status"], "duplicate");

    let commit = CheckpointCommit::new(new_checkpoint.clone(), 1, begin.content_sha256.clone())
        .expect("commit");
    let (status, body) = post_state(
        &application,
        &state_request(
            &new_epoch,
            SourceReplicaCommand::CheckpointCommit(commit.clone()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "checkpoint_conflict");
    assert_eq!(body["active_cursor"]["epoch_id"], old_epoch.as_str());

    let visible = store
        .active_source_replica(&source)
        .expect("active read")
        .expect("old active generation");
    assert_eq!(visible.cursor, old_cursor);
    assert_eq!(visible.entries, vec![entry(TXID_A, 1)]);

    let second_chunk = CheckpointChunk::new(new_checkpoint, 1, vec![new_entries[1].clone()])
        .expect("second chunk");
    let (status, _) = post_state(
        &application,
        &state_request(
            &new_epoch,
            SourceReplicaCommand::CheckpointChunk(second_chunk),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, body) = post_state(
        &application,
        &state_request(
            &new_epoch,
            SourceReplicaCommand::CheckpointCommit(commit.clone()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let (status, body) = post_state(
        &application,
        &state_request(&new_epoch, SourceReplicaCommand::CheckpointCommit(commit)),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["status"], "duplicate");

    let visible = store
        .active_source_replica(&source)
        .expect("active read")
        .expect("new active generation");
    assert_eq!(visible.cursor.epoch_id, new_epoch);
    assert_eq!(visible.cursor.revision, 1);
    assert_eq!(visible.entries, new_entries);
}

#[tokio::test]
async fn delta_retry_is_idempotent_and_cursor_and_epoch_conflicts_are_machine_readable() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store = Store::open(database).expect("store");
    let application = router(store.clone());
    let source_epoch = epoch("epoch-a");
    install_checkpoint(
        &application,
        &source_epoch,
        &checkpoint("checkpoint-a"),
        None,
        1,
        &[entry(TXID_A, 1)],
    )
    .await;

    let delta = StateDelta::new(
        1,
        2,
        1_700_000_300_000,
        vec![
            StateMutation::Absent {
                txid: TXID_A.to_owned(),
            },
            StateMutation::Present {
                txid: TXID_B.to_owned(),
                facts: facts(2),
            },
        ],
    )
    .expect("delta");
    let request = state_request(&source_epoch, SourceReplicaCommand::Delta(delta.clone()));
    for expected in ["applied", "duplicate"] {
        let (status, body) = post_state(&application, &request).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(body["status"], expected);
        assert_eq!(body["active_cursor"]["revision"], 2);
    }

    let conflicting_delta = StateDelta::new(
        1,
        2,
        1_700_000_300_001,
        vec![StateMutation::Present {
            txid: TXID_C.to_owned(),
            facts: facts(3),
        }],
    )
    .expect("conflicting delta");
    let (status, body) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::Delta(conflicting_delta),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "conflicting_replay");

    let wrong_base = StateDelta::new(
        1,
        3,
        1_700_000_300_002,
        vec![StateMutation::Present {
            txid: TXID_C.to_owned(),
            facts: facts(3),
        }],
    )
    .expect("wrong-base delta");
    let (status, body) = post_state(
        &application,
        &state_request(&source_epoch, SourceReplicaCommand::Delta(wrong_base)),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "cursor_mismatch");
    assert_eq!(body["active_cursor"]["revision"], 2);

    let heartbeat = StateHeartbeat::new(2, 1_700_000_300_003).expect("heartbeat");
    let (status, body) = post_state(
        &application,
        &state_request(
            &epoch("wrong-epoch"),
            SourceReplicaCommand::Heartbeat(heartbeat),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "epoch_conflict");
    assert_eq!(body["active_cursor"]["epoch_id"], source_epoch.as_str());

    let heartbeat = StateHeartbeat::new(2, 1_700_000_300_010).expect("heartbeat");
    let (status, body) = post_state(
        &application,
        &state_request(&source_epoch, SourceReplicaCommand::Heartbeat(heartbeat)),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let regressing = StateHeartbeat::new(2, 1_700_000_300_009).expect("heartbeat");
    let (status, body) = post_state(
        &application,
        &state_request(&source_epoch, SourceReplicaCommand::Heartbeat(regressing)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid_state_request");

    let regressing_delta = StateDelta::new(
        2,
        3,
        1_700_000_300_009,
        vec![StateMutation::Present {
            txid: TXID_C.to_owned(),
            facts: facts(3),
        }],
    )
    .expect("delta");
    let (status, body) = post_state(
        &application,
        &state_request(&source_epoch, SourceReplicaCommand::Delta(regressing_delta)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid_state_request");

    let same_revision_checkpoint = CheckpointBegin::new(
        checkpoint("checkpoint-same-revision"),
        Some(ReplicaCursor::new(source_epoch.clone(), 2).expect("active replacement cursor")),
        2,
        1_700_000_300_011,
        1,
        &[entry(TXID_B, 2)],
    )
    .expect("checkpoint begin");
    let same_revision_request = SourceReplicaRequest {
        protocol_version: SOURCE_REPLICA_PROTOCOL_VERSION,
        source_id: SourceId::new("source-a").expect("source"),
        epoch_id: source_epoch.clone(),
        command: SourceReplicaCommand::CheckpointBegin(same_revision_checkpoint),
    };
    let (status, body) = post_state(&application, &same_revision_request).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid_state_request");

    let regressing_checkpoint = CheckpointBegin::new(
        checkpoint("checkpoint-regressing-time"),
        Some(ReplicaCursor::new(source_epoch.clone(), 2).expect("active replacement cursor")),
        3,
        1_700_000_300_009,
        1,
        &[entry(TXID_B, 2)],
    )
    .expect("checkpoint begin");
    let (status, body) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointBegin(regressing_checkpoint),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid_state_request");

    let visible = store
        .active_source_replica(&SourceId::new("source-a").expect("source"))
        .expect("active read")
        .expect("active generation");
    assert_eq!(visible.cursor.revision, 2);
    assert_eq!(visible.entries, vec![entry(TXID_B, 2)]);
}

#[tokio::test]
async fn exact_cas_begin_supersedes_an_abandoned_staging_checkpoint() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store = Store::open(database).expect("store");
    let application = router(store.clone());
    let source_epoch = epoch("epoch-a");
    let active_cursor = install_checkpoint(
        &application,
        &source_epoch,
        &checkpoint("checkpoint-active"),
        None,
        1,
        &[entry(TXID_A, 1)],
    )
    .await;

    let abandoned_id = checkpoint("checkpoint-abandoned");
    let abandoned_entries = vec![entry(TXID_B, 2)];
    let abandoned = CheckpointBegin::new(
        abandoned_id.clone(),
        Some(active_cursor.clone()),
        2,
        1_700_000_400_000,
        1,
        &abandoned_entries,
    )
    .expect("abandoned begin");
    let (status, _) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointBegin(abandoned.clone()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let frozen_heartbeat = StateHeartbeat::new(1, 1_700_000_400_010).expect("heartbeat");
    let (status, body) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::Heartbeat(frozen_heartbeat),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "checkpoint_conflict");
    assert_eq!(body["staging_checkpoint_id"], abandoned_id.as_str());

    let frozen_delta = StateDelta::new(
        1,
        2,
        1_700_000_400_011,
        vec![StateMutation::Present {
            txid: TXID_C.to_owned(),
            facts: facts(3),
        }],
    )
    .expect("delta");
    let (status, body) = post_state(
        &application,
        &state_request(&source_epoch, SourceReplicaCommand::Delta(frozen_delta)),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "checkpoint_conflict");
    assert_eq!(body["staging_checkpoint_id"], abandoned_id.as_str());
    let visible = store
        .active_source_replica(&SourceId::new("source-a").expect("source"))
        .expect("active read")
        .expect("active generation");
    assert_eq!(visible.cursor.revision, 1);
    assert_eq!(visible.entries, vec![entry(TXID_A, 1)]);

    let replacement_id = checkpoint("checkpoint-recovery");
    let mut replacement = CheckpointBegin::new(
        replacement_id.clone(),
        Some(active_cursor),
        2,
        1_700_000_400_001,
        1,
        &[entry(TXID_C, 3)],
    )
    .expect("replacement begin");
    let (status, body) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointBegin(replacement.clone()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "checkpoint_conflict");
    assert_eq!(body["staging_checkpoint_id"], abandoned_id.as_str());

    replacement.supersedes_checkpoint_id = Some(abandoned_id.clone());
    let (status, body) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointBegin(replacement.clone()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["progress"]["checkpoint_id"], replacement_id.as_str());

    let (status, body) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointBegin(replacement),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["status"], "duplicate");
    assert_eq!(body["progress"]["checkpoint_id"], replacement_id.as_str());

    let (status, body) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointBegin(abandoned),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "checkpoint_conflict");
    assert_eq!(body["staging_checkpoint_id"], replacement_id.as_str());

    let abandoned_chunk =
        CheckpointChunk::new(abandoned_id, 0, abandoned_entries).expect("abandoned chunk");
    let (status, body) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointChunk(abandoned_chunk),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "checkpoint_conflict");
    assert_eq!(body["staging_checkpoint_id"], replacement_id.as_str());
}

#[tokio::test]
async fn exact_active_checkpoint_retries_beat_a_newer_staging_fence() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store = Store::open(&database).expect("store");
    let application = router(store);
    let source_epoch = epoch("epoch-a");
    let active_id = checkpoint("checkpoint-active");
    let active_entries = vec![entry(TXID_A, 1)];
    let active_begin = CheckpointBegin::new(
        active_id.clone(),
        None,
        1,
        1_700_000_100_001,
        1,
        &active_entries,
    )
    .expect("active begin");
    let active_chunk =
        CheckpointChunk::new(active_id.clone(), 0, active_entries.clone()).expect("active chunk");
    let active_commit =
        CheckpointCommit::new(active_id.clone(), 1, active_begin.content_sha256.clone())
            .expect("active commit");
    let active_cursor = install_checkpoint(
        &application,
        &source_epoch,
        &active_id,
        None,
        1,
        &active_entries,
    )
    .await;

    let staging_id = checkpoint("checkpoint-newer-staging");
    let staging_begin = CheckpointBegin::new(
        staging_id.clone(),
        Some(active_cursor),
        2,
        1_700_000_450_000,
        1,
        &[entry(TXID_B, 2)],
    )
    .expect("staging begin");
    let (status, body) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointBegin(staging_begin),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    for command in [
        SourceReplicaCommand::CheckpointBegin(active_begin),
        SourceReplicaCommand::CheckpointChunk(active_chunk),
        SourceReplicaCommand::CheckpointCommit(active_commit),
    ] {
        let (status, body) = post_state(&application, &state_request(&source_epoch, command)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(body["status"], "duplicate");
        assert_eq!(body["active_cursor"]["revision"], 1);
        assert_empty_staging(&database, &staging_id);
    }
}

#[tokio::test]
async fn exact_delta_and_heartbeat_retries_beat_a_newer_staging_fence() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store = Store::open(&database).expect("store");
    let application = router(store);
    let source_epoch = epoch("epoch-a");
    install_checkpoint(
        &application,
        &source_epoch,
        &checkpoint("checkpoint-active"),
        None,
        1,
        &[entry(TXID_A, 1)],
    )
    .await;

    let delta = StateDelta::new(
        1,
        2,
        1_700_000_460_000,
        vec![StateMutation::Present {
            txid: TXID_B.to_owned(),
            facts: facts(2),
        }],
    )
    .expect("delta");
    let (status, body) = post_state(
        &application,
        &state_request(&source_epoch, SourceReplicaCommand::Delta(delta.clone())),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let heartbeat = StateHeartbeat::new(2, 1_700_000_460_001).expect("heartbeat");
    let (status, body) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::Heartbeat(heartbeat.clone()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let staging_id = checkpoint("checkpoint-newer-staging");
    let staging_begin = CheckpointBegin::new(
        staging_id.clone(),
        Some(ReplicaCursor::new(source_epoch.clone(), 2).expect("replacement cursor")),
        3,
        1_700_000_460_002,
        1,
        &[entry(TXID_A, 1), entry(TXID_B, 2)],
    )
    .expect("staging begin");
    let (status, body) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointBegin(staging_begin),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    for command in [
        SourceReplicaCommand::Delta(delta),
        SourceReplicaCommand::Heartbeat(heartbeat),
    ] {
        let (status, body) = post_state(&application, &state_request(&source_epoch, command)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(body["status"], "duplicate");
        assert_eq!(body["active_cursor"]["revision"], 2);
        assert_empty_staging(&database, &staging_id);
    }
}

#[tokio::test]
async fn invalid_digest_format_is_rejected_without_mutation() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store = Store::open(database).expect("store");
    let application = router(store.clone());
    let source_epoch = epoch("epoch-a");
    let begin = CheckpointBegin::new(
        checkpoint("checkpoint-a"),
        None,
        1,
        1_700_000_500_000,
        1,
        &[entry(TXID_A, 1)],
    )
    .expect("begin");
    let mut body = serde_json::to_value(state_request(
        &source_epoch,
        SourceReplicaCommand::CheckpointBegin(begin),
    ))
    .expect("request JSON");
    body["command"]["content_sha256"] = Value::String("AA".repeat(32));

    let (status, body) = post_json(&application, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid_state_request");
    assert!(
        store
            .active_source_replica(&SourceId::new("source-a").expect("source"))
            .expect("active read")
            .is_none()
    );
    let (status, body) = get_json(&application, "/api/v1/sources").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({ "sources": [] }));
    let (status, _) = get_json(&application, "/api/v1/sources/source-a/mempool").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn complete_checkpoint_with_wrong_declared_digest_never_activates() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store = Store::open(database).expect("store");
    let application = router(store.clone());
    let source_epoch = epoch("epoch-a");
    let checkpoint_id = checkpoint("checkpoint-a");
    let entries = vec![entry(TXID_A, 1)];
    let mut begin = CheckpointBegin::new(
        checkpoint_id.clone(),
        None,
        1,
        1_700_000_600_000,
        1,
        &entries,
    )
    .expect("begin");
    begin.content_sha256 = "00".repeat(32);
    let (status, body) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointBegin(begin.clone()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let chunk = CheckpointChunk::new(checkpoint_id.clone(), 0, entries).expect("chunk");
    let (status, body) = post_state(
        &application,
        &state_request(&source_epoch, SourceReplicaCommand::CheckpointChunk(chunk)),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let commit = CheckpointCommit::new(checkpoint_id, 1, begin.content_sha256).expect("commit");
    let (status, body) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointCommit(commit),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid_state_request");
    assert!(
        store
            .active_source_replica(&SourceId::new("source-a").expect("source"))
            .expect("active read")
            .is_none()
    );
}

#[tokio::test]
async fn empty_bootstrap_is_invisible_until_commit_and_heartbeat_retries_are_idempotent() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store = Store::open(&database).expect("store");
    let application = router(store.clone());
    let source = SourceId::new("source-a").expect("source");
    let source_epoch = epoch("epoch-a");
    let checkpoint_id = checkpoint("checkpoint-empty");
    let observed_at = 1_700_000_700_000;
    let begin = CheckpointBegin::new(checkpoint_id.clone(), None, 1, observed_at, 0, &[])
        .expect("empty begin");
    let begin_request = state_request(
        &source_epoch,
        SourceReplicaCommand::CheckpointBegin(begin.clone()),
    );
    for expected in ["applied", "duplicate"] {
        let (status, body) = post_state(&application, &begin_request).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(body["status"], expected);
        assert!(body.get("active_cursor").is_none());
        assert_eq!(body["progress"]["received_entries"], 0);
        assert_eq!(body["progress"]["received_chunks"], 0);
    }
    assert!(
        store
            .active_source_replica(&source)
            .expect("active read")
            .is_none()
    );
    let (status, body) = get_json(&application, "/api/v1/sources").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({ "sources": [] }));
    let (status, _) = get_json(&application, "/api/v1/sources/source-a/mempool").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let commit = CheckpointCommit::new(checkpoint_id, 1, begin.content_sha256).expect("commit");
    let commit_request = state_request(
        &source_epoch,
        SourceReplicaCommand::CheckpointCommit(commit),
    );
    for expected in ["applied", "duplicate"] {
        let (status, body) = post_state(&application, &commit_request).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(body["status"], expected);
        assert_eq!(body["active_cursor"]["revision"], 1);
    }
    let visible = store
        .active_source_replica(&source)
        .expect("active read")
        .expect("empty active generation");
    assert!(visible.entries.is_empty());
    let (status, body) = get_json(&application, "/api/v1/sources").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["sources"][0]["membership_count"], 0);
    assert_eq!(body["sources"][0]["state_cursor"]["revision"], 1);
    let (status, body) = get_json(&application, "/api/v1/sources/source-a/mempool").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["memberships"], serde_json::json!([]));
    assert_eq!(body["health"]["capture"]["status"], "not_collected");

    let exact = state_request(
        &source_epoch,
        SourceReplicaCommand::Heartbeat(StateHeartbeat::new(1, observed_at).expect("heartbeat")),
    );
    let (status, body) = post_state(&application, &exact).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["status"], "duplicate");

    let later = state_request(
        &source_epoch,
        SourceReplicaCommand::Heartbeat(
            StateHeartbeat::new(1, observed_at + 1).expect("heartbeat"),
        ),
    );
    for expected in ["applied", "duplicate"] {
        let (status, body) = post_state(&application, &later).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(body["status"], expected);
    }
}

#[tokio::test]
async fn capacity_and_chunk_conflicts_roll_back_and_generation_cardinality_stays_bounded() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let database = temporary.path().join("atlas.db");
    Store::migrate(&database).expect("migrate");
    let store = Store::open(&database).expect("store");
    let application = router(store.clone());
    let source_epoch = epoch("epoch-a");

    let capacity_request = SourceReplicaRequest {
        protocol_version: SOURCE_REPLICA_PROTOCOL_VERSION,
        source_id: SourceId::new("source-a").expect("source"),
        epoch_id: source_epoch.clone(),
        command: SourceReplicaCommand::CheckpointBegin(CheckpointBegin {
            checkpoint_id: checkpoint("checkpoint-too-large"),
            supersedes_checkpoint_id: None,
            replaces: None,
            target_revision: 1,
            state_observed_at_ms: 1_700_000_800_000,
            expected_entries: MAX_CHECKPOINT_ENTRIES + 1,
            expected_chunks: 1,
            content_sha256: "00".repeat(32),
        }),
    };
    let (status, body) = post_state(&application, &capacity_request).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "capacity_exceeded");
    assert!(
        store
            .active_source_replica(&SourceId::new("source-a").expect("source"))
            .expect("active read")
            .is_none()
    );

    let active_cursor = install_checkpoint(
        &application,
        &source_epoch,
        &checkpoint("checkpoint-active"),
        None,
        1,
        &[entry(TXID_A, 1)],
    )
    .await;
    let oversized_id = checkpoint("checkpoint-oversized-chunk");
    let oversized_declared = vec![entry(TXID_B, 2)];
    let begin = CheckpointBegin::new(
        oversized_id.clone(),
        Some(active_cursor.clone()),
        2,
        1_700_000_800_001,
        1,
        &oversized_declared,
    )
    .expect("begin");
    let (status, body) = post_state(
        &application,
        &state_request(&source_epoch, SourceReplicaCommand::CheckpointBegin(begin)),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let oversized_chunk = CheckpointChunk::new(
        oversized_id.clone(),
        0,
        vec![entry(TXID_B, 2), entry(TXID_C, 3)],
    )
    .expect("oversized chunk");
    let (status, body) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointChunk(oversized_chunk),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "capacity_exceeded");

    let recovery_id = checkpoint("checkpoint-recovery");
    let recovery_entries = vec![entry(TXID_B, 2)];
    let mut recovery = CheckpointBegin::new(
        recovery_id.clone(),
        Some(active_cursor.clone()),
        2,
        1_700_000_800_002,
        1,
        &recovery_entries,
    )
    .expect("recovery begin");
    recovery.supersedes_checkpoint_id = Some(oversized_id);
    let (status, body) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointBegin(recovery),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");

    let out_of_range =
        CheckpointChunk::new(recovery_id.clone(), 1, recovery_entries.clone()).expect("chunk");
    let (status, body) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointChunk(out_of_range),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["code"], "invalid_state_request");

    let good_chunk = CheckpointChunk::new(recovery_id.clone(), 0, recovery_entries).expect("chunk");
    let (status, body) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointChunk(good_chunk),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let conflicting_chunk =
        CheckpointChunk::new(recovery_id.clone(), 0, vec![entry(TXID_C, 3)]).expect("chunk");
    let (status, body) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointChunk(conflicting_chunk),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["code"], "checkpoint_conflict");

    let connection = Connection::open(&database).expect("inspect database");
    let (generations, active, staging, staging_entries): (i64, i64, i64, i64) = connection
        .query_row(
            "SELECT COUNT(*),
                    SUM(role = 'active'),
                    SUM(role = 'staging'),
                    (SELECT COUNT(*) FROM source_replica_membership AS membership
                     JOIN source_replica_generation AS generation
                       USING (source_id, generation_id)
                     WHERE generation.role = 'staging')
             FROM source_replica_generation WHERE source_id = 'source-a'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("generation counts");
    assert_eq!(
        (generations, active, staging, staging_entries),
        (2, 1, 1, 1)
    );

    let mut final_recovery = CheckpointBegin::new(
        checkpoint("checkpoint-final-recovery"),
        Some(active_cursor),
        2,
        1_700_000_800_003,
        1,
        &[entry(TXID_C, 3)],
    )
    .expect("final recovery begin");
    final_recovery.supersedes_checkpoint_id = Some(recovery_id);
    let (status, body) = post_state(
        &application,
        &state_request(
            &source_epoch,
            SourceReplicaCommand::CheckpointBegin(final_recovery),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let generations: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM source_replica_generation WHERE source_id = 'source-a'",
            [],
            |row| row.get(0),
        )
        .expect("generation count");
    assert_eq!(generations, 2);
}
