use std::collections::BTreeSet;

use crate::model::{
    Bip110Assessment, Bip110Status, ClassificationResult, ClassificationResultState,
    KNOTS_BIP110_CLASSIFIER_ID, MAX_SUPPORTED_MEMPOOL_ENTRIES, MempoolSnapshot, SourceSummary,
};

use super::{StagedSnapshotError, count_u64, invalid, parse_display_hash};

pub(super) fn validate_input(
    source: &SourceSummary,
    snapshot: &MempoolSnapshot,
) -> Result<(), StagedSnapshotError> {
    if source.source_id != snapshot.source_id || source.source_label != snapshot.source_label {
        return Err(invalid("source summary does not identify the snapshot"));
    }
    if source.snapshot_observed_at_ms != Some(snapshot.observed_at_ms)
        || source.chain_tip.as_ref() != Some(&snapshot.chain_tip)
        || source.transaction_count != Some(snapshot.transaction_count)
        || source.total_vsize != Some(snapshot.total_vsize)
        || source
            .classification
            .as_ref()
            .map(|classification| classification.revision)
            != Some(snapshot.classification_revision)
    {
        return Err(invalid(
            "source summary snapshot identity does not match the snapshot",
        ));
    }
    let expected_classified = snapshot.bip110_summary.compatible_count
        + snapshot.bip110_summary.violating_count
        + snapshot.bip110_summary.indeterminate_count;
    if source.classification.as_ref().is_none_or(|classification| {
        classification.classified_count != expected_classified
            || classification.unclassified_count != snapshot.bip110_summary.unclassified_count
    }) {
        return Err(invalid(
            "source summary classification counts do not match the snapshot",
        ));
    }
    if snapshot.transaction_count != count_u64(snapshot.transactions.len(), "transaction_count")? {
        return Err(invalid("transaction_count does not match transactions"));
    }
    if snapshot.transaction_count > MAX_SUPPORTED_MEMPOOL_ENTRIES {
        return Err(invalid(format!(
            "transaction_count exceeds supported maximum {MAX_SUPPORTED_MEMPOOL_ENTRIES}"
        )));
    }

    let mut catalog_ids = BTreeSet::new();
    for descriptor in &snapshot.classifier_catalog {
        if descriptor.id.is_empty() || descriptor.version.is_empty() {
            return Err(invalid("classifier id and version must be non-empty"));
        }
        if !catalog_ids.insert(descriptor.id.as_str()) {
            return Err(invalid(format!(
                "duplicate classifier catalog id {}",
                descriptor.id
            )));
        }
    }
    if snapshot.classification_summaries.len() != snapshot.classifier_catalog.len()
        || snapshot
            .classification_summaries
            .iter()
            .zip(&snapshot.classifier_catalog)
            .any(|(summary, descriptor)| summary.classifier_id != descriptor.id)
    {
        return Err(invalid(
            "classification summaries do not match classifier catalog order",
        ));
    }

    for transaction in &snapshot.transactions {
        parse_display_hash(&transaction.txid, "txid")?;
        parse_display_hash(&transaction.wtxid, "wtxid")?;
        if transaction.structure.is_some() != !transaction.classifications.is_empty() {
            return Err(invalid(format!(
                "transaction {} has mismatched structure and classifier result presence",
                transaction.txid
            )));
        }
        if !transaction.classifications.is_empty()
            && (transaction.classifications.len() != snapshot.classifier_catalog.len()
                || transaction
                    .classifications
                    .iter()
                    .zip(&snapshot.classifier_catalog)
                    .any(|(result, descriptor)| result.classifier_id != descriptor.id))
        {
            return Err(invalid(format!(
                "transaction {} classifier results do not exactly match catalog order",
                transaction.txid
            )));
        }
        let mut result_ids = BTreeSet::new();
        for result in &transaction.classifications {
            if result.evidence.is_some() {
                return Err(invalid(format!(
                    "transaction {} classifier result {} contains snapshot evidence",
                    transaction.txid, result.classifier_id
                )));
            }
            if !catalog_ids.contains(result.classifier_id.as_str()) {
                return Err(invalid(format!(
                    "transaction {} has result for unknown classifier {}",
                    transaction.txid, result.classifier_id
                )));
            }
            if !result_ids.insert(result.classifier_id.as_str()) {
                return Err(invalid(format!(
                    "transaction {} has duplicate result for classifier {}",
                    transaction.txid, result.classifier_id
                )));
            }
        }
        let bip110_result = transaction
            .classifications
            .iter()
            .find(|result| result.classifier_id == KNOTS_BIP110_CLASSIFIER_ID);
        match (bip110_result, transaction.bip110.as_ref()) {
            (None, None) => {}
            (Some(result), Some(assessment)) if bip110_result_matches(result, assessment) => {}
            (Some(_), Some(_)) => {
                return Err(invalid(format!(
                    "transaction {} BIP-110 result does not match its assessment",
                    transaction.txid
                )));
            }
            _ => {
                return Err(invalid(format!(
                    "transaction {} has mismatched BIP-110 result and assessment presence",
                    transaction.txid
                )));
            }
        }
    }
    Ok(())
}

fn bip110_result_matches(result: &ClassificationResult, assessment: &Bip110Assessment) -> bool {
    let partial = !assessment.unknown_rules.is_empty();
    let status = match assessment.status {
        Bip110Status::Compatible => "compatible",
        Bip110Status::Violating => "violating",
        Bip110Status::Indeterminate => "indeterminate",
    };
    let missing_facts_match = if partial {
        result.missing_facts.len() == 1
            && result.missing_facts.first().map(String::as_str) == Some("policy_facts")
    } else {
        result.missing_facts.is_empty()
    };
    let expected_state = if partial {
        ClassificationResultState::Partial
    } else {
        ClassificationResultState::Complete
    };
    result.state == expected_state
        && result.primary_label.as_deref() == Some(status)
        && result.labels.len() == 1
        && result.labels.first().map(String::as_str) == Some(status)
        && missing_facts_match
}
