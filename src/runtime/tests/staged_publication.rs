use super::*;

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
        .stage_payload(Some(StageKind::Population), None, population_id)
        .await
    {
        StageLookup::Ready(payload) => payload,
        lookup => panic!("unexpected population lookup: {lookup:?}"),
    };
    let membership_before = match runtime
        .stage_payload(Some(StageKind::Membership), None, membership_id)
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
        .stage_payload(Some(StageKind::Population), None, population_id)
        .await
    {
        StageLookup::Ready(payload) => payload,
        lookup => panic!("unexpected population lookup: {lookup:?}"),
    };
    let membership_after = match runtime
        .stage_payload(Some(StageKind::Membership), None, membership_id)
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
        .stage_payload(Some(StageKind::Membership), None, membership_id)
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
        .stage_payload(Some(StageKind::Membership), None, membership_id)
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
