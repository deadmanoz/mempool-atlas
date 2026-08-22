use super::*;

fn bip110_result(entry: &mut MempoolEntry) -> &mut ClassificationResult {
    entry
        .classifications
        .iter_mut()
        .find(|result| result.classifier_id == KNOTS_BIP110_CLASSIFIER_ID)
        .expect("BIP-110 result")
}

fn expect_invalid_snapshot(source: &SourceSummary, snapshot: &MempoolSnapshot, expected: &str) {
    let error = encode_staged_snapshot(source, snapshot).expect_err("encoding must fail");
    assert!(matches!(error, StagedSnapshotError::Invalid(_)));
    assert!(
        error.to_string().contains(expected),
        "unexpected error: {error}"
    );
}

#[test]
fn mismatched_structure_and_result_presence_fails_recoverably() {
    let (source, snapshot) = fixture();

    let mut missing_structure = snapshot.clone();
    missing_structure.transactions[0].structure = None;
    expect_invalid_snapshot(
        &source,
        &missing_structure,
        "mismatched structure and classifier result presence",
    );

    let mut unexpected_structure = snapshot;
    unexpected_structure.transactions[1].structure =
        Some(TransactionStructure::new(1, 1, 0, 100, 0).expect("structure"));
    expect_invalid_snapshot(
        &source,
        &unexpected_structure,
        "mismatched structure and classifier result presence",
    );
}

#[test]
fn mismatched_bip110_result_and_assessment_fails_recoverably() {
    let (source, snapshot) = fixture();
    let expected = "BIP-110 result does not match its assessment";

    let mut wrong_state = snapshot.clone();
    bip110_result(&mut wrong_state.transactions[0]).state = ClassificationResultState::Complete;
    expect_invalid_snapshot(&source, &wrong_state, expected);

    let mut wrong_primary = snapshot.clone();
    bip110_result(&mut wrong_primary.transactions[0]).primary_label = Some("compatible".to_owned());
    expect_invalid_snapshot(&source, &wrong_primary, expected);

    let mut wrong_labels = snapshot.clone();
    bip110_result(&mut wrong_labels.transactions[0]).labels = vec!["compatible".to_owned()];
    expect_invalid_snapshot(&source, &wrong_labels, expected);

    let mut multiple_labels = snapshot.clone();
    bip110_result(&mut multiple_labels.transactions[0])
        .labels
        .push("violating".to_owned());
    expect_invalid_snapshot(&source, &multiple_labels, expected);

    let mut missing_partial_fact = snapshot.clone();
    bip110_result(&mut missing_partial_fact.transactions[0])
        .missing_facts
        .clear();
    expect_invalid_snapshot(&source, &missing_partial_fact, expected);

    let mut wrong_partial_fact = snapshot.clone();
    bip110_result(&mut wrong_partial_fact.transactions[0]).missing_facts =
        vec!["raw_transaction".to_owned()];
    expect_invalid_snapshot(&source, &wrong_partial_fact, expected);

    let mut complete_with_partial_fact = snapshot;
    complete_with_partial_fact.transactions[0]
        .bip110
        .as_mut()
        .expect("assessment")
        .unknown_rules
        .clear();
    bip110_result(&mut complete_with_partial_fact.transactions[0]).state =
        ClassificationResultState::Complete;
    expect_invalid_snapshot(&source, &complete_with_partial_fact, expected);
}
