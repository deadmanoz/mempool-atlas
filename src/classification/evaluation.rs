//! CPU scheduling boundary for pure terminal transaction evaluation.
//!
//! A pending slice may retain hundreds of megabytes of decoded transactions
//! and scripts. Move that state through one sequential blocking task per
//! bounded chunk instead of cloning it or running classifier scans on a Tokio
//! async worker.

use super::*;

#[cfg(test)]
mod tests;

const MAX_CLASSIFICATIONS_PER_BLOCKING_CHUNK: usize = 128;
const MAX_CLASSIFICATION_SERIALIZED_BYTES_PER_BLOCKING_CHUNK: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub(super) struct ClassificationChunkLimits {
    max_transactions: usize,
    max_serialized_transaction_bytes: usize,
}

impl ClassificationChunkLimits {
    const PRODUCTION: Self = Self {
        max_transactions: MAX_CLASSIFICATIONS_PER_BLOCKING_CHUNK,
        max_serialized_transaction_bytes: MAX_CLASSIFICATION_SERIALIZED_BYTES_PER_BLOCKING_CHUNK,
    };

    #[cfg(test)]
    pub(super) const fn new(
        max_transactions: usize,
        max_serialized_transaction_bytes: usize,
    ) -> Self {
        Self {
            max_transactions,
            max_serialized_transaction_bytes,
        }
    }
}

#[derive(Debug)]
struct ClassifiedPendingChunk {
    pending: PendingSlice,
    classifications: Vec<CachedClassification>,
    ready_remaining: bool,
}

impl ClassificationPipeline {
    pub(super) async fn classify_ready_bounded(
        &self,
        pending: &mut PendingSlice,
        generation: &Arc<ClassificationGeneration>,
        classifications: &mut Vec<CachedClassification>,
    ) -> Result<bool, ClassificationError> {
        loop {
            if !self.is_current_generation(generation) {
                return Ok(false);
            }

            let owned = std::mem::take(pending);
            let joined = tokio::task::spawn_blocking(move || {
                let mut pending = owned;
                let (classifications, ready_remaining) =
                    pending.classify_ready_chunk(ClassificationChunkLimits::PRODUCTION);
                ClassifiedPendingChunk {
                    pending,
                    classifications,
                    ready_remaining,
                }
            })
            .await;

            // A replacement generation makes this work irrelevant, including
            // a late worker failure. Nothing from the old generation is
            // allowed to re-enter coordinator state.
            if !self.is_current_generation(generation) {
                return Ok(false);
            }

            let chunk = joined.map_err(ClassificationError::EvaluationTask)?;
            *pending = chunk.pending;
            classifications.extend(chunk.classifications);
            if !chunk.ready_remaining {
                break;
            }
        }
        Ok(true)
    }
}

impl PendingSlice {
    #[cfg(test)]
    pub(super) fn classify_ready(&mut self) -> Vec<CachedClassification> {
        self.classify_ready_chunk(ClassificationChunkLimits::PRODUCTION)
            .0
    }

    fn candidate_is_ready(
        facts: &BTreeMap<OutPoint, FactState>,
        candidate: &PendingCandidate,
    ) -> bool {
        candidate.required.iter().all(|outpoint| {
            matches!(
                facts.get(outpoint),
                Some(FactState::Ready(_) | FactState::ExplicitlyMissing)
            )
        })
    }

    fn prepare_ready_candidates(&mut self) {
        if !self.ready.is_empty() {
            return;
        }
        let active = std::mem::take(&mut self.active);
        for (key, candidate) in active {
            if Self::candidate_is_ready(&self.facts, &candidate) {
                self.ready.push_back(candidate);
            } else {
                self.active.insert(key, candidate);
            }
        }
    }

    pub(super) fn classify_ready_chunk(
        &mut self,
        limits: ClassificationChunkLimits,
    ) -> (Vec<CachedClassification>, bool) {
        debug_assert!(limits.max_transactions > 0);
        debug_assert!(limits.max_serialized_transaction_bytes > 0);

        self.prepare_ready_candidates();
        let mut serialized_bytes = 0_usize;
        let mut classifications = Vec::with_capacity(limits.max_transactions);
        while classifications.len() < limits.max_transactions {
            let Some(candidate) = self.ready.front() else {
                break;
            };
            let next_bytes = candidate.transaction.total_size();
            if !classifications.is_empty()
                && serialized_bytes.saturating_add(next_bytes)
                    > limits.max_serialized_transaction_bytes
            {
                break;
            }
            serialized_bytes = serialized_bytes.saturating_add(next_bytes);
            let candidate = self
                .ready
                .pop_front()
                .expect("ready candidate remains queued");
            classifications.push(classify_terminal_candidate(candidate, &self.facts));
        }
        let ready_remaining = !self.ready.is_empty();
        if !ready_remaining {
            let active = &self.active;
            self.fair_order
                .retain(|candidate_key| active.contains_key(candidate_key));
            self.drop_unreferenced_facts();
        }
        (classifications, ready_remaining)
    }
}

fn classify_terminal_candidate(
    candidate: PendingCandidate,
    facts: &BTreeMap<OutPoint, FactState>,
) -> CachedClassification {
    let (prevouts, retryable) = if candidate.transaction.is_coinbase() {
        (PrevoutSet::default(), false)
    } else {
        let facts = candidate
            .transaction
            .input
            .iter()
            .map(|input| match facts.get(&input.previous_output) {
                Some(FactState::Ready(script)) => Some(PrevoutFacts::new(script.clone(), None)),
                Some(FactState::ExplicitlyMissing) => None,
                Some(
                    FactState::Awaiting { .. }
                    | FactState::CapacityBlocked(_)
                    | FactState::OperationallyDeferred,
                )
                | None => unreachable!("only terminal candidates are classified"),
            })
            .collect::<Vec<_>>();
        let retryable = facts.iter().any(Option::is_none);
        (PrevoutSet::from_vec(facts), retryable)
    };
    let evidence = evaluate_mempool_policy(&candidate.transaction, &prevouts);
    let structure = transaction_structure(&candidate.transaction)
        .expect("structure derivability is verified when raw transactions are admitted");
    let mut classification = classification_from_evidence(
        candidate.expected.txid,
        candidate.expected.wtxid,
        structure,
        evidence,
    );
    let classifier_output = classify_transaction_with_metrics(
        &candidate.transaction,
        &prevouts,
        &classification.assessment,
    );
    let recognized_carried_bytes = classification
        .structure
        .op_return_bytes
        .saturating_add(classifier_output.recognized_non_op_return_bytes);
    classification.structure = classification
        .structure
        .with_recognized_carried_bytes(recognized_carried_bytes)
        .expect("recognized carriage is bounded by the admitted raw transaction");
    classification.results = classifier_output.results;
    classification.input_outpoints = transaction_input_outpoints(&candidate.transaction);
    CachedClassification {
        classification: Arc::new(classification),
        retryable,
    }
}
