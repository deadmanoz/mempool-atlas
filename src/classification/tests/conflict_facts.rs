use super::*;

pub(super) fn assert_exact_carry(publication: &ClassificationGenerationStart) {
    assert!(
        publication
            .classifications
            .values()
            .all(|classification| classification.input_outpoints.is_some()),
        "exact input outpoints carry only with the surviving txid and wtxid"
    );
}

#[test]
fn budget_keeps_prefix_coverage_without_blocking_classification() {
    let one_row_bytes = CONFLICT_FACT_TRANSACTION_OVERHEAD_BYTES + std::mem::size_of::<OutPoint>();
    let limits = ClassificationLimits::new(1, 1, 1)
        .expect("limits")
        .with_conflict_fact_bytes(one_row_bytes);
    let mut work = ClassificationWork::default();
    let mut first = cached_classification(1);
    let mut second = cached_classification(2);
    let mut third = cached_classification(3);
    for cached in [&mut first, &mut second, &mut third] {
        retain_conflict_facts_within_budget(&mut work, cached, limits.max_conflict_fact_bytes);
    }

    assert!(first.classification.input_outpoints.is_some());
    assert!(second.classification.input_outpoints.is_none());
    assert!(third.classification.input_outpoints.is_none());
    assert!(work.conflict_fact_capacity_exhausted);
    assert_eq!(work.conflict_fact_bytes, one_row_bytes);
}

#[test]
fn retry_replacement_reuses_its_existing_reservation() {
    let mut work = ClassificationWork::default();
    let mut original = cached_classification(9);
    let maximum = CONFLICT_FACT_TRANSACTION_OVERHEAD_BYTES + std::mem::size_of::<OutPoint>();
    retain_conflict_facts_within_budget(&mut work, &mut original, maximum);
    work.classifications
        .insert(original.classification.wtxid.clone(), original.clone());
    work.conflict_fact_capacity_exhausted = true;
    let mut replacement = cached_classification(9);

    retain_conflict_facts_within_budget(&mut work, &mut replacement, maximum);

    assert!(replacement.classification.input_outpoints.is_some());
    assert_eq!(work.conflict_fact_bytes, maximum);
}

fn cached_classification(marker: u8) -> CachedClassification {
    let txid = format!("{marker:02x}").repeat(32);
    let assessment = Bip110Assessment {
        status: Bip110Status::Compatible,
        primary_rule: None,
        violated_rules: Vec::new(),
        unknown_rules: Vec::new(),
    };
    CachedClassification {
        classification: Arc::new(TransactionClassification {
            txid: txid.clone(),
            wtxid: txid,
            structure: TransactionStructure::new(1, 1, 0, 1, 0).expect("structure"),
            results: Vec::new(),
            assessment,
            rules: Vec::new(),
            input_outpoints: Some(vec![confirmed_outpoint(marker, 0)].into()),
        }),
        retryable: false,
    }
}
