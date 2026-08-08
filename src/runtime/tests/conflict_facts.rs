use super::*;

#[tokio::test]
async fn paused_publication_allows_a_bounded_cached_sidecar() {
    let runtime = runtime();
    runtime
        .record_success(observation(20))
        .await
        .expect("classifying observation");
    let generation = runtime
        .classification_generation()
        .await
        .expect("classification generation");
    assert!(
        runtime
            .record_classification_update(generation, None, ClassificationState::Paused)
            .await
            .expect("pause publication")
    );
    let manifest = runtime
        .current_manifest_payload()
        .await
        .expect("paused manifest");
    let manifest = serde_json::from_slice::<Value>(&manifest.body).expect("manifest JSON");
    let population_id = manifest["population_id"].as_str().expect("population ID");
    let structure_id = manifest["stages"]
        .as_array()
        .expect("stages")
        .iter()
        .find(|stage| stage["kind"] == "structure")
        .and_then(|stage| stage["content_id"].as_str())
        .expect("structure ID");

    let (first, second) = tokio::join!(
        runtime.conflict_fingerprint_payload(population_id, structure_id),
        runtime.conflict_fingerprint_payload(population_id, structure_id),
    );
    let ConflictFactLookup::Ready(first) = first.expect("first fingerprint lookup") else {
        panic!("paused publication must expose conflict fingerprints");
    };
    let ConflictFactLookup::Ready(second) = second.expect("second fingerprint lookup") else {
        panic!("paused publication must reuse conflict fingerprints");
    };
    assert_eq!(first.body.as_ptr(), second.body.as_ptr());
}
