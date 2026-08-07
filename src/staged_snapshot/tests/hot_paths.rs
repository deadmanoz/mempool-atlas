use super::*;

#[test]
fn retained_membership_validation_preserves_structural_invariants() {
    let (source, snapshot) = fixture();
    let first = encode_staged_snapshot(&source, &snapshot).expect("first encoding");

    let mut wrong_population_kind = first.population.clone();
    wrong_population_kind.descriptor.kind = StageKind::Membership;
    assert!(
        validate_reused_membership(
            &wrong_population_kind,
            &first.membership,
            &snapshot,
            snapshot.transaction_count,
        )
        .is_err()
    );

    let mut wrong_population_length = first.population.clone();
    wrong_population_length.descriptor.uncompressed_bytes += 1;
    assert!(
        validate_reused_membership(
            &wrong_population_length,
            &first.membership,
            &snapshot,
            snapshot.transaction_count,
        )
        .is_err()
    );

    let mut wrong_membership_dependency = first.membership.clone();
    wrong_membership_dependency.descriptor.dependency_ids = vec![hash(0xff)];
    assert!(
        validate_reused_membership(
            &first.population,
            &wrong_membership_dependency,
            &snapshot,
            snapshot.transaction_count,
        )
        .is_err()
    );

    assert!(
        validate_reused_membership(
            &first.population,
            &first.membership,
            &snapshot,
            snapshot.transaction_count + 1,
        )
        .is_err()
    );
}

#[cfg(debug_assertions)]
#[test]
fn retained_membership_validation_checks_digests_in_debug_builds() {
    let (source, snapshot) = fixture();
    let first = encode_staged_snapshot(&source, &snapshot).expect("first encoding");
    let mut corrupted = first.membership.clone();
    let mut corrupted_bytes = corrupted.bytes.to_vec();
    corrupted_bytes[0] ^= 1;
    corrupted.bytes = Bytes::from(corrupted_bytes);

    assert!(
        validate_reused_membership(
            &first.population,
            &corrupted,
            &snapshot,
            snapshot.transaction_count,
        )
        .is_err()
    );
}

#[test]
fn retained_membership_rejects_a_different_same_sized_txid_population() {
    let (source, snapshot) = fixture();
    let first = encode_staged_snapshot(&source, &snapshot).expect("first encoding");
    let mut different_snapshot = snapshot.clone();
    different_snapshot.transactions[0].txid = hash(0);

    let error = encode_staged_snapshot_with_limits(
        &source,
        &different_snapshot,
        Some((first.population, first.membership)),
        StagedSnapshotLimits::default(),
    )
    .expect_err("retained population must match the snapshot txids");

    assert!(matches!(error, StagedSnapshotError::Invalid(message) if
        message == "retained population stage does not match snapshot txids"));
}

#[test]
fn exact_dictionary_codes_grow_past_the_bounded_initial_capacity() {
    let mut values = (0..20)
        .map(|index| Some(format!("value-{index}")))
        .collect::<Vec<_>>();
    values.extend([None, Some("value-0".to_owned())]);

    let (dictionary, codes) = exact_dictionary_codes(values).expect("dictionary codes");
    let encoded = serde_json::json!({ "codes": codes });

    assert_eq!(
        dictionary,
        (0..20)
            .map(|index| format!("value-{index}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        decoded_column(&encoded, "codes"),
        (1..=20).chain([0, 1]).collect::<Vec<_>>()
    );
}
