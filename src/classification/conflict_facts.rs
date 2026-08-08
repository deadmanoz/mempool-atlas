use std::sync::Arc;

use bitcoin::{OutPoint, Transaction};

use super::{CachedClassification, ClassificationWork};

pub(crate) const MAX_CONFLICT_FACT_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const CONFLICT_FACT_TRANSACTION_OVERHEAD_BYTES: usize = 64;

pub(super) fn retain_within_budget(
    work: &mut ClassificationWork,
    cached: &mut CachedClassification,
    maximum_bytes: usize,
) {
    let previous_bytes = work
        .classifications
        .get(&cached.classification.wtxid)
        .and_then(|previous| previous.classification.input_outpoints.as_ref())
        .map(|outpoints| estimated_retained_bytes(outpoints.len()));
    if let Some(previous_bytes) = previous_bytes {
        work.conflict_fact_bytes = work.conflict_fact_bytes.saturating_sub(previous_bytes);
    }
    let Some(outpoints) = cached.classification.input_outpoints.as_ref() else {
        work.conflict_fact_capacity_exhausted = true;
        return;
    };
    if work.conflict_fact_capacity_exhausted && previous_bytes.is_none() {
        Arc::make_mut(&mut cached.classification).input_outpoints = None;
        return;
    }
    let estimated_bytes = estimated_retained_bytes(outpoints.len());
    let Some(new_total) = work.conflict_fact_bytes.checked_add(estimated_bytes) else {
        Arc::make_mut(&mut cached.classification).input_outpoints = None;
        work.conflict_fact_capacity_exhausted = true;
        return;
    };
    if new_total > maximum_bytes {
        Arc::make_mut(&mut cached.classification).input_outpoints = None;
        work.conflict_fact_capacity_exhausted = true;
        return;
    }
    work.conflict_fact_bytes = new_total;
}

fn estimated_retained_bytes(outpoint_count: usize) -> usize {
    CONFLICT_FACT_TRANSACTION_OVERHEAD_BYTES
        .saturating_add(outpoint_count.saturating_mul(std::mem::size_of::<OutPoint>()))
}

pub(super) fn retain_new_classifications(
    work: &mut ClassificationWork,
    classifications: &mut [CachedClassification],
    maximum_bytes: usize,
) {
    classifications
        .sort_unstable_by(|left, right| left.classification.txid.cmp(&right.classification.txid));
    for cached in classifications {
        retain_within_budget(work, cached, maximum_bytes);
        work.classifications
            .insert(cached.classification.wtxid.clone(), cached.clone());
    }
}

pub(super) fn transaction_input_outpoints(transaction: &Transaction) -> Option<Arc<[OutPoint]>> {
    transaction
        .input
        .iter()
        .all(|input| !input.previous_output.is_null())
        .then(|| {
            transaction
                .input
                .iter()
                .map(|input| input.previous_output)
                .collect::<Vec<_>>()
                .into()
        })
}
