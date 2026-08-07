use super::*;

async fn publication_stages(runtime: &SourceRuntime, manifest: &Value) -> Vec<PublicationPayload> {
    let mut payloads = Vec::new();
    for descriptor in manifest["stages"].as_array().expect("stages") {
        let kind = match descriptor["kind"].as_str().expect("stage kind") {
            "population" => StageKind::Population,
            "membership" => StageKind::Membership,
            "structure" => StageKind::Structure,
            "classifier" => StageKind::Classifier,
            kind => panic!("unexpected stage kind {kind}"),
        };
        let classifier_id = descriptor["classifier_id"].as_str();
        let content_id = descriptor["content_id"].as_str().expect("content ID");
        match runtime.stage_payload(kind, classifier_id, content_id).await {
            StageLookup::Ready(payload) => payloads.push(payload),
            lookup => panic!("unexpected stage lookup: {lookup:?}"),
        }
    }
    payloads
}

#[tokio::test]
async fn classification_publication_reuses_membership_buffers() {
    let runtime = runtime();
    let txid = "00".repeat(32);
    runtime
        .record_membership(membership_publication(1, true, observation(20)))
        .await
        .expect("publish membership");

    let manifest = runtime
        .current_manifest_payload()
        .await
        .expect("membership manifest");
    let manifest = serde_json::from_slice::<Value>(&manifest.body).expect("manifest JSON");
    let population_id = manifest["population_id"].as_str().expect("population ID");
    let membership_id = manifest["stages"]
        .as_array()
        .expect("stages")
        .iter()
        .find(|stage| stage["kind"] == "membership")
        .and_then(|stage| stage["content_id"].as_str())
        .expect("membership ID");
    let population_before = match runtime
        .stage_payload(StageKind::Population, None, population_id)
        .await
    {
        StageLookup::Ready(payload) => payload,
        lookup => panic!("unexpected population lookup: {lookup:?}"),
    };
    let membership_before = match runtime
        .stage_payload(StageKind::Membership, None, membership_id)
        .await
    {
        StageLookup::Ready(payload) => payload,
        lookup => panic!("unexpected membership lookup: {lookup:?}"),
    };

    runtime
        .record_classification_progress(revision_delta(1, 1, compatible_observation(txid, 20)))
        .await
        .expect("publish classification");
    let population_after = match runtime
        .stage_payload(StageKind::Population, None, population_id)
        .await
    {
        StageLookup::Ready(payload) => payload,
        lookup => panic!("unexpected population lookup: {lookup:?}"),
    };
    let membership_after = match runtime
        .stage_payload(StageKind::Membership, None, membership_id)
        .await
    {
        StageLookup::Ready(payload) => payload,
        lookup => panic!("unexpected membership lookup: {lookup:?}"),
    };

    assert_eq!(
        population_before.body.as_ptr(),
        population_after.body.as_ptr()
    );
    assert_eq!(
        membership_before.body.as_ptr(),
        membership_after.body.as_ptr()
    );
}

#[tokio::test]
async fn poll_start_reencodes_only_the_manifest() {
    let runtime = runtime();
    runtime
        .record_membership(membership_publication(1, true, observation(20)))
        .await
        .expect("publish membership");
    let before = runtime
        .current_manifest_payload()
        .await
        .expect("membership manifest");
    let before_manifest = serde_json::from_slice::<Value>(&before.body).expect("manifest JSON");
    let before_stages = publication_stages(&runtime, &before_manifest).await;

    runtime
        .record_poll_started(30)
        .await
        .expect("record poll start");

    let after = runtime
        .current_manifest_payload()
        .await
        .expect("poll-start manifest");
    let after_manifest = serde_json::from_slice::<Value>(&after.body).expect("manifest JSON");
    let after_stages = publication_stages(&runtime, &after_manifest).await;
    assert_eq!(after_manifest["source"]["last_poll_started_at_ms"], 30);
    assert_eq!(before_manifest["stages"], after_manifest["stages"]);
    assert_eq!(
        before_manifest["population_id"],
        after_manifest["population_id"]
    );
    assert_eq!(
        before_manifest["classification_set_id"],
        after_manifest["classification_set_id"]
    );
    assert_ne!(
        before_manifest["publication_id"],
        after_manifest["publication_id"]
    );
    assert_ne!(before.etag, after.etag);
    assert_eq!(before_stages.len(), after_stages.len());
    for (before_stage, after_stage) in before_stages.iter().zip(&after_stages) {
        assert_eq!(before_stage.content_id, after_stage.content_id);
        assert_eq!(before_stage.etag, after_stage.etag);
        assert_eq!(before_stage.body.as_ptr(), after_stage.body.as_ptr());
    }
}

#[tokio::test]
async fn poll_start_retries_after_a_concurrent_failure_publication() {
    let runtime = Arc::new(runtime());
    runtime
        .record_membership(membership_publication(1, true, observation(20)))
        .await
        .expect("publish membership");
    let block = runtime.block_next_manifest_replacement_preparation();
    let poll_start = tokio::spawn({
        let runtime = Arc::clone(&runtime);
        async move { runtime.record_poll_started(30).await }
    });
    tokio::time::timeout(Duration::from_secs(1), block.wait_until_started())
        .await
        .expect("poll-start manifest preparation blocked");

    runtime
        .record_failure("transient RPC failure".to_owned())
        .await
        .expect("publish concurrent failure");
    block.release();
    poll_start
        .await
        .expect("poll-start task")
        .expect("retry poll-start publication");

    let manifest = runtime
        .current_manifest_payload()
        .await
        .expect("poll-start manifest");
    let manifest = serde_json::from_slice::<Value>(&manifest.body).expect("manifest JSON");
    assert_eq!(manifest["source"]["last_poll_started_at_ms"], 30);
    assert_eq!(manifest["source"]["last_error"], "transient RPC failure");
}

#[tokio::test]
async fn failure_publication_retries_after_a_concurrent_poll_start() {
    let runtime = Arc::new(runtime());
    runtime
        .record_membership(membership_publication(1, true, observation(20)))
        .await
        .expect("publish membership");
    let block = runtime.block_next_manifest_replacement_preparation();
    let failure = tokio::spawn({
        let runtime = Arc::clone(&runtime);
        async move {
            runtime
                .record_failure("transient RPC failure".to_owned())
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(1), block.wait_until_started())
        .await
        .expect("failure manifest preparation blocked");

    runtime
        .record_poll_started(30)
        .await
        .expect("publish concurrent poll start");
    block.release();
    failure
        .await
        .expect("failure task")
        .expect("retry failure publication");

    let manifest = runtime
        .current_manifest_payload()
        .await
        .expect("failure manifest");
    let manifest = serde_json::from_slice::<Value>(&manifest.body).expect("manifest JSON");
    assert_eq!(manifest["source"]["last_poll_started_at_ms"], 30);
    assert_eq!(manifest["source"]["last_error"], "transient RPC failure");
}

#[tokio::test]
async fn failure_publication_retry_promotes_the_newest_pending_poll_start() {
    let runtime = Arc::new(runtime());
    runtime
        .record_poll_started(10)
        .await
        .expect("record initial poll start");
    runtime
        .record_membership(membership_publication(1, true, observation(20)))
        .await
        .expect("publish membership");
    runtime.limit_next_poll_start_reencoding(StagedSnapshotLimits {
        max_stage_bytes: 1,
        max_publication_bytes: 1,
    });
    runtime
        .record_poll_started(30)
        .await
        .expect_err("first poll-start publication must fail");

    let block = runtime.block_next_manifest_replacement_preparation();
    let failure = tokio::spawn({
        let runtime = Arc::clone(&runtime);
        async move {
            runtime
                .record_failure("transient RPC failure".to_owned())
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(1), block.wait_until_started())
        .await
        .expect("failure manifest preparation blocked");

    runtime.limit_next_poll_start_reencoding(StagedSnapshotLimits {
        max_stage_bytes: 1,
        max_publication_bytes: 1,
    });
    runtime
        .record_poll_started(50)
        .await
        .expect_err("newer poll-start publication must fail");
    assert_eq!(runtime.summary().await.last_poll_started_at_ms, Some(10));

    block.release();
    failure
        .await
        .expect("failure task")
        .expect("retry failure publication");

    let summary = runtime.summary().await;
    assert_eq!(summary.last_poll_started_at_ms, Some(50));
    assert_eq!(summary.last_error.as_deref(), Some("transient RPC failure"));
    let manifest = runtime
        .current_manifest_payload()
        .await
        .expect("failure manifest");
    let manifest = serde_json::from_slice::<Value>(&manifest.body).expect("manifest JSON");
    assert_eq!(manifest["source"]["last_poll_started_at_ms"], 50);
    assert_eq!(manifest["source"]["last_error"], "transient RPC failure");
}

#[tokio::test]
async fn publication_limit_failure_retains_the_current_bundle_and_domain_state() {
    let runtime = runtime();
    let txid = "00".repeat(32);
    runtime
        .record_membership(membership_publication(1, true, observation(20)))
        .await
        .expect("publish membership");
    let before = runtime
        .current_manifest_payload()
        .await
        .expect("membership manifest");
    runtime.limit_next_publication(StagedSnapshotLimits {
        max_stage_bytes: 1,
        max_publication_bytes: 1,
    });

    let error = runtime
        .record_classification_progress(revision_delta(1, 1, compatible_observation(txid, 20)))
        .await
        .expect_err("publication limit must reject the candidate");
    assert!(matches!(
        error,
        RuntimeError::StagedSnapshot(StagedSnapshotError::EncodedBodyTooLarge { .. })
    ));

    let after = runtime
        .current_manifest_payload()
        .await
        .expect("retained manifest");
    assert_eq!(before.body.as_ptr(), after.body.as_ptr());
    assert_eq!(before.etag, after.etag);
    assert_eq!(
        runtime
            .published_state()
            .await
            .snapshot
            .expect("retained snapshot")
            .classification_revision,
        0
    );
}

#[tokio::test]
async fn classification_publication_failure_pauses_without_staling_membership() {
    let runtime = runtime();
    let txid = "00".repeat(32);
    runtime
        .record_membership(membership_publication(1, true, observation(20)))
        .await
        .expect("publish membership");
    let before = runtime
        .current_manifest_payload()
        .await
        .expect("membership manifest");
    let manifest = serde_json::from_slice::<Value>(&before.body).expect("manifest JSON");
    let membership_id = manifest["stages"]
        .as_array()
        .expect("stages")
        .iter()
        .find(|stage| stage["kind"] == "membership")
        .and_then(|stage| stage["content_id"].as_str())
        .expect("membership ID");
    let membership_before = match runtime
        .stage_payload(StageKind::Membership, None, membership_id)
        .await
    {
        StageLookup::Ready(payload) => payload,
        lookup => panic!("unexpected membership lookup: {lookup:?}"),
    };
    runtime.limit_next_publication(StagedSnapshotLimits {
        max_stage_bytes: 1,
        max_publication_bytes: 1,
    });

    assert!(
        !runtime
            .publish_classification(
                Ok(classification_report(
                    1,
                    ClassificationDisposition::Continue,
                    Some(revision_delta(
                        1,
                        1,
                        compatible_observation(txid.clone(), 20),
                    )),
                )),
                Some(1),
            )
            .await
    );

    let response = runtime.published_state().await;
    assert_eq!(response.source.availability, SourceAvailability::Ready);
    assert_eq!(response.source.last_error, None);
    assert_classification_progress(&response, ClassificationState::Paused, 0, 0, 1);
    assert_eq!(
        response
            .snapshot
            .expect("retained snapshot")
            .classification_revision,
        0
    );
    assert!(matches!(
        runtime.transaction_detail(&txid).await,
        TransactionLookup::Unclassified
    ));
    let membership_after = match runtime
        .stage_payload(StageKind::Membership, None, membership_id)
        .await
    {
        StageLookup::Ready(payload) => payload,
        lookup => panic!("unexpected membership lookup: {lookup:?}"),
    };
    assert_eq!(
        membership_before.body.as_ptr(),
        membership_after.body.as_ptr()
    );
}
