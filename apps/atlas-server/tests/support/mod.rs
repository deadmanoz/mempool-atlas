use atlas_model::{
    CheckpointBegin, CheckpointChunk, CheckpointCommit, CheckpointId, MAX_CHECKPOINT_CHUNK_ENTRIES,
    ReplicaCursor, SourceEpochId, SourceId, SourceReplicaCommand, SourceReplicaEntry,
    SourceReplicaRequest,
};
use atlas_server::Store;

/// Atomically replaces one source's reader-visible state through the real
/// SourceReplica reducer. Entries are sorted and split at the protocol limit,
/// so this helper is suitable for scale fixtures as well as small API tests.
pub fn install_checkpoint(
    store: &Store,
    source_id: &str,
    state_observed_at_ms: u64,
    mut entries: Vec<SourceReplicaEntry>,
) -> ReplicaCursor {
    entries.sort_by(|left, right| left.txid.cmp(&right.txid));
    let source_id = SourceId::new(source_id).expect("source");
    let active = store
        .active_source_replica(&source_id)
        .expect("active state read");
    let (epoch_id, target_revision, replaces) = match active {
        Some(active) => (
            active.cursor.epoch_id.clone(),
            active.cursor.revision + 1,
            Some(active.cursor),
        ),
        None => (
            SourceEpochId::new(format!("fixture-epoch-{source_id}")).expect("epoch"),
            1,
            None,
        ),
    };
    let checkpoint_id =
        CheckpointId::new(format!("fixture-checkpoint-{source_id}-{target_revision}"))
            .expect("checkpoint");
    let expected_chunks = u32::try_from(entries.len().div_ceil(MAX_CHECKPOINT_CHUNK_ENTRIES))
        .expect("checkpoint chunk count");
    let begin = CheckpointBegin::new(
        checkpoint_id.clone(),
        replaces,
        target_revision,
        state_observed_at_ms,
        expected_chunks,
        &entries,
    )
    .expect("checkpoint begin");
    let request = |command| {
        SourceReplicaRequest::new(source_id.clone(), epoch_id.clone(), command)
            .expect("state request")
    };

    store
        .apply_source_replica(&request(SourceReplicaCommand::CheckpointBegin(
            begin.clone(),
        )))
        .expect("begin checkpoint");
    for (chunk_index, entries) in entries.chunks(MAX_CHECKPOINT_CHUNK_ENTRIES).enumerate() {
        let chunk = CheckpointChunk::new(
            checkpoint_id.clone(),
            u32::try_from(chunk_index).expect("checkpoint chunk index"),
            entries.to_vec(),
        )
        .expect("checkpoint chunk");
        store
            .apply_source_replica(&request(SourceReplicaCommand::CheckpointChunk(chunk)))
            .expect("stage checkpoint chunk");
    }
    let commit = CheckpointCommit::new(checkpoint_id, target_revision, begin.content_sha256)
        .expect("checkpoint commit");
    let response = store
        .apply_source_replica(&request(SourceReplicaCommand::CheckpointCommit(commit)))
        .expect("commit checkpoint");
    response
        .active_cursor()
        .cloned()
        .expect("committed checkpoint cursor")
}
