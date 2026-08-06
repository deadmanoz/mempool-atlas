use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::RwLock;
use tracing::info;

use crate::classification::{ClassificationGenerationStart, ClassificationRevisionDelta};
use crate::model::{
    ClassificationProgress, ClassificationState, MempoolObservation, MempoolSnapshot,
    SourceAvailability, SourceSnapshotResponse, SourceSummary, TransactionClassifications,
    TransactionDetailResponse,
};

use super::{RuntimeError, TransactionLookup};

#[derive(Debug)]
pub(super) struct CurrentStatePublisher {
    source_id: String,
    source_label: String,
    poll_interval: Duration,
    state: RwLock<CurrentState>,
    #[cfg(test)]
    next_classification_preparation: std::sync::Mutex<Option<Arc<PreparationBlock>>>,
}

#[derive(Debug, Default)]
struct CurrentState {
    last_poll_started_at_ms: Option<u64>,
    membership: Option<Arc<MempoolSnapshot>>,
    latest: Option<Arc<MempoolSnapshot>>,
    classifications: TransactionClassifications,
    last_error: Option<String>,
    cached_response: Option<CachedSnapshotResponse>,
    classification_generation: Option<u64>,
    classification_revision: u64,
    classification_state: Option<ClassificationState>,
    status_revision: u64,
    response_revision: u64,
}

#[derive(Clone, Debug)]
struct CachedSnapshotResponse {
    body: Bytes,
    etag: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct SnapshotResponsePayload {
    pub(crate) body: Bytes,
    pub(crate) etag: Option<String>,
}

struct PreparedObservation {
    latest: Arc<MempoolSnapshot>,
    classifications: TransactionClassifications,
    materialization_validation_ms: u128,
}

impl CurrentStatePublisher {
    pub(super) fn new(source_id: String, source_label: String, poll_interval: Duration) -> Self {
        Self {
            source_id,
            source_label,
            poll_interval,
            state: RwLock::new(CurrentState::default()),
            #[cfg(test)]
            next_classification_preparation: std::sync::Mutex::new(None),
        }
    }

    pub(super) fn source_id(&self) -> &str {
        &self.source_id
    }

    pub(super) fn source_label(&self) -> &str {
        &self.source_label
    }

    pub(super) fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    pub(super) async fn classification_generation(&self) -> Option<u64> {
        self.state.read().await.classification_generation
    }

    pub(super) async fn record_poll_started(&self, started_at_ms: u64) {
        let mut state = self.state.write().await;
        state.last_poll_started_at_ms = Some(started_at_ms);
        state.status_revision = state.status_revision.wrapping_add(1);
    }

    pub(super) async fn summary(&self) -> SourceSummary {
        let state = self.state.read().await;
        summary_from_state(self, &state)
    }

    pub(super) async fn snapshot_response(&self) -> SourceSnapshotResponse {
        let state = self.state.read().await;
        SourceSnapshotResponse {
            source: summary_from_state(self, &state),
            snapshot: state.latest.clone(),
        }
    }

    pub(super) async fn snapshot_response_payload(
        &self,
    ) -> Result<SnapshotResponsePayload, RuntimeError> {
        let state = self.state.read().await;
        if let Some(response) = &state.cached_response {
            return Ok(SnapshotResponsePayload {
                body: response.body.clone(),
                etag: response.etag.clone(),
            });
        }
        let response = SourceSnapshotResponse {
            source: summary_from_state(self, &state),
            snapshot: state.latest.clone(),
        };
        drop(state);
        let (body, _) = encode_response(response).await?;
        Ok(SnapshotResponsePayload { body, etag: None })
    }

    pub(super) async fn publish_membership(
        &self,
        publication: ClassificationGenerationStart,
    ) -> Result<bool, RuntimeError> {
        let ClassificationGenerationStart {
            generation,
            revision,
            has_eligible_work,
            membership,
            classifications,
        } = publication;
        if membership.source_id != self.source_id {
            return Err(RuntimeError::SnapshotSourceMismatch {
                expected: self.source_id.clone(),
                actual: membership.source_id.clone(),
            });
        }
        if revision != 0 {
            return Err(RuntimeError::ClassificationRevisionMismatch {
                publication: revision,
                snapshot: 0,
            });
        }
        if classifications.len() > membership.transactions.len() {
            return Err(RuntimeError::ClassificationsExceedMembership {
                classified: classifications.len(),
                membership: membership.transactions.len(),
            });
        }
        if self
            .state
            .read()
            .await
            .classification_generation
            .is_some_and(|current| current >= generation)
        {
            return Ok(false);
        }

        let changed_count = classifications.len();
        let prepared =
            prepare_observation(Arc::clone(&membership), revision, classifications).await?;
        let classification_state = if has_eligible_work {
            ClassificationState::Classifying
        } else {
            ClassificationState::Complete
        };

        loop {
            let (status_revision, current_generation, last_poll_started_at_ms) = {
                let state = self.state.read().await;
                (
                    state.status_revision,
                    state.classification_generation,
                    state.last_poll_started_at_ms,
                )
            };
            if current_generation.is_some_and(|current| current >= generation) {
                return Ok(false);
            }
            let response = SourceSnapshotResponse {
                source: summary_from_parts(
                    self,
                    last_poll_started_at_ms,
                    Some(&prepared.latest),
                    Some(classification_state),
                    None,
                ),
                snapshot: Some(Arc::clone(&prepared.latest)),
            };
            let (body, json_encoding_ms) = encode_response(response).await?;

            let commit_started_at = Instant::now();
            let mut state = self.state.write().await;
            if state
                .classification_generation
                .is_some_and(|current| current >= generation)
            {
                return Ok(false);
            }
            if state.status_revision != status_revision
                || state.classification_generation != current_generation
            {
                continue;
            }
            state.membership = Some(Arc::clone(&membership));
            state.latest = Some(Arc::clone(&prepared.latest));
            state.classifications = prepared.classifications;
            debug_assert!(state.classifications.len() <= membership.transactions.len());
            state.last_error = None;
            state.classification_generation = Some(generation);
            state.classification_revision = revision;
            state.classification_state = Some(classification_state);
            state.status_revision = state.status_revision.wrapping_add(1);
            let (response_revision, encoded_bytes) =
                replace_cached_response(self, &mut state, body);
            let total_classified_count = state.classifications.len();
            let commit_ms = commit_started_at.elapsed().as_millis();
            drop(state);
            info!(
                source_id = %self.source_id,
                generation,
                classification_revision = revision,
                response_revision,
                changed_count,
                total_classified_count,
                materialization_validation_ms = prepared.materialization_validation_ms,
                json_encoding_ms,
                commit_ms,
                encoded_bytes,
                "published current membership state"
            );
            return Ok(true);
        }
    }

    pub(super) async fn publish_classification_update(
        &self,
        generation: u64,
        delta: Option<ClassificationRevisionDelta>,
        classification_state: ClassificationState,
    ) -> Result<bool, RuntimeError> {
        if let Some(delta) = &delta
            && delta.generation != generation
        {
            return Err(RuntimeError::ClassificationReportGenerationMismatch {
                report: generation,
                publication: delta.generation,
            });
        }

        let Some(delta) = delta else {
            return self
                .publish_lifecycle(generation, classification_state)
                .await;
        };
        let (
            mut status_revision,
            current_revision,
            membership,
            mut classifications,
            mut last_poll_started_at_ms,
            mut last_error,
            classification_accumulation_started_at,
        ) = {
            let state = self.state.read().await;
            if state.classification_generation != Some(generation) {
                return Ok(false);
            }
            if state.classification_revision >= delta.revision {
                return Ok(false);
            }
            let expected_revision = state
                .classification_revision
                .checked_add(1)
                .ok_or(RuntimeError::ClassificationGenerationOverflow)?;
            if delta.revision != expected_revision {
                return Err(RuntimeError::ClassificationRevisionSequence {
                    current: state.classification_revision,
                    publication: delta.revision,
                });
            }
            let classification_accumulation_started_at = Instant::now();
            (
                state.status_revision,
                state.classification_revision,
                state
                    .membership
                    .clone()
                    .ok_or(RuntimeError::ClassificationStateWithoutSnapshot)?,
                state.classifications.clone(),
                state.last_poll_started_at_ms,
                state.last_error.clone(),
                classification_accumulation_started_at,
            )
        };

        let changed_count = delta.changed.len();
        for classification in delta.changed {
            classifications.insert(classification.txid.clone(), classification);
        }
        let classification_accumulation_ms =
            classification_accumulation_started_at.elapsed().as_millis();
        if classifications.len() > membership.transactions.len() {
            return Err(RuntimeError::ClassificationsExceedMembership {
                classified: classifications.len(),
                membership: membership.transactions.len(),
            });
        }

        #[cfg(test)]
        self.wait_on_next_classification_preparation().await;

        let prepared =
            prepare_observation(Arc::clone(&membership), delta.revision, classifications).await?;
        loop {
            let response = SourceSnapshotResponse {
                source: summary_from_parts(
                    self,
                    last_poll_started_at_ms,
                    Some(&prepared.latest),
                    Some(classification_state),
                    last_error.as_deref(),
                ),
                snapshot: Some(Arc::clone(&prepared.latest)),
            };
            let (body, json_encoding_ms) = encode_response(response).await?;

            let commit_started_at = Instant::now();
            let mut state = self.state.write().await;
            if state.classification_generation != Some(generation)
                || state.classification_revision != current_revision
            {
                return Ok(false);
            }
            if state.status_revision != status_revision {
                status_revision = state.status_revision;
                last_poll_started_at_ms = state.last_poll_started_at_ms;
                last_error.clone_from(&state.last_error);
                drop(state);
                continue;
            }
            state.latest = Some(Arc::clone(&prepared.latest));
            state.classifications = prepared.classifications;
            debug_assert!(state.classifications.len() <= membership.transactions.len());
            state.classification_revision = delta.revision;
            let classification_state_changed =
                state.classification_state != Some(classification_state);
            state.classification_state = Some(classification_state);
            if classification_state_changed {
                state.status_revision = state.status_revision.wrapping_add(1);
            }
            let (response_revision, encoded_bytes) =
                replace_cached_response(self, &mut state, body);
            let total_classified_count = state.classifications.len();
            let commit_ms = commit_started_at.elapsed().as_millis();
            drop(state);
            info!(
                source_id = %self.source_id,
                generation,
                classification_revision = delta.revision,
                response_revision,
                changed_count,
                total_classified_count,
                classification_accumulation_ms,
                materialization_validation_ms = prepared.materialization_validation_ms,
                json_encoding_ms,
                commit_ms,
                encoded_bytes,
                classification_state = ?classification_state,
                "published current classification state"
            );
            return Ok(true);
        }
    }

    async fn publish_lifecycle(
        &self,
        generation: u64,
        classification_state: ClassificationState,
    ) -> Result<bool, RuntimeError> {
        loop {
            let (
                status_revision,
                current_revision,
                last_poll_started_at_ms,
                latest,
                last_error,
                current_classification_state,
            ) = {
                let state = self.state.read().await;
                if state.classification_generation != Some(generation) {
                    return Ok(false);
                }
                (
                    state.status_revision,
                    state.classification_revision,
                    state.last_poll_started_at_ms,
                    state
                        .latest
                        .clone()
                        .ok_or(RuntimeError::ClassificationStateWithoutSnapshot)?,
                    state.last_error.clone(),
                    state.classification_state,
                )
            };
            if current_classification_state == Some(classification_state) {
                return Ok(true);
            }
            let response = SourceSnapshotResponse {
                source: summary_from_parts(
                    self,
                    last_poll_started_at_ms,
                    Some(&latest),
                    Some(classification_state),
                    last_error.as_deref(),
                ),
                snapshot: Some(Arc::clone(&latest)),
            };
            let (body, json_encoding_ms) = encode_response(response).await?;

            let commit_started_at = Instant::now();
            let mut state = self.state.write().await;
            if state.classification_generation != Some(generation) {
                return Ok(false);
            }
            if state.status_revision != status_revision
                || state.classification_revision != current_revision
            {
                continue;
            }
            state.classification_state = Some(classification_state);
            state.status_revision = state.status_revision.wrapping_add(1);
            let (response_revision, encoded_bytes) =
                replace_cached_response(self, &mut state, body);
            let total_classified_count = state.classifications.len();
            let commit_ms = commit_started_at.elapsed().as_millis();
            drop(state);
            info!(
                source_id = %self.source_id,
                generation,
                classification_revision = current_revision,
                response_revision,
                changed_count = 0,
                total_classified_count,
                materialization_validation_ms = 0,
                json_encoding_ms,
                commit_ms,
                encoded_bytes,
                classification_state = ?classification_state,
                "published current classification lifecycle"
            );
            return Ok(true);
        }
    }

    pub(super) async fn publish_failure(&self, error: String) -> Result<(), RuntimeError> {
        loop {
            let (
                status_revision,
                classification_generation,
                classification_revision,
                last_poll_started_at_ms,
                latest,
                classification_state,
            ) = {
                let state = self.state.read().await;
                (
                    state.status_revision,
                    state.classification_generation,
                    state.classification_revision,
                    state.last_poll_started_at_ms,
                    state.latest.clone(),
                    state.classification_state,
                )
            };
            let response = SourceSnapshotResponse {
                source: summary_from_parts(
                    self,
                    last_poll_started_at_ms,
                    latest.as_ref(),
                    classification_state,
                    Some(&error),
                ),
                snapshot: latest,
            };
            let (body, json_encoding_ms) = encode_response(response).await?;

            let commit_started_at = Instant::now();
            let mut state = self.state.write().await;
            if state.status_revision != status_revision
                || state.classification_generation != classification_generation
                || state.classification_revision != classification_revision
            {
                continue;
            }
            state.last_error = Some(error);
            state.status_revision = state.status_revision.wrapping_add(1);
            let (response_revision, encoded_bytes) =
                replace_cached_response(self, &mut state, body);
            let total_classified_count = state.classifications.len();
            let commit_ms = commit_started_at.elapsed().as_millis();
            drop(state);
            info!(
                source_id = %self.source_id,
                classification_revision,
                response_revision,
                changed_count = 0,
                total_classified_count,
                materialization_validation_ms = 0,
                json_encoding_ms,
                commit_ms,
                encoded_bytes,
                "published current source failure"
            );
            return Ok(());
        }
    }

    pub(super) async fn transaction_detail(&self, txid: &str) -> TransactionLookup {
        let state = self.state.read().await;
        let Some(snapshot) = state.latest.as_ref() else {
            return TransactionLookup::WaitingForSnapshot;
        };
        if snapshot
            .transactions
            .binary_search_by(|entry| entry.txid.as_str().cmp(txid))
            .is_err()
        {
            return TransactionLookup::NotPresent;
        }
        let Some(classification) = state.classifications.get(txid) else {
            return TransactionLookup::Unclassified;
        };
        TransactionLookup::Ready(TransactionDetailResponse {
            source_id: self.source_id.clone(),
            snapshot_observed_at_ms: snapshot.observed_at_ms,
            classification_revision: state.classification_revision,
            txid: classification.txid.clone(),
            wtxid: classification.wtxid.clone(),
            classifications: classification.results.clone(),
            assessment: classification.assessment.clone(),
            rules: classification.rules.clone(),
        })
    }

    #[cfg(test)]
    pub(super) fn block_next_classification_preparation(&self) -> Arc<PreparationBlock> {
        let block = Arc::new(PreparationBlock::default());
        *self
            .next_classification_preparation
            .lock()
            .expect("classification preparation hook is not poisoned") = Some(Arc::clone(&block));
        block
    }

    #[cfg(test)]
    async fn wait_on_next_classification_preparation(&self) {
        let block = self
            .next_classification_preparation
            .lock()
            .expect("classification preparation hook is not poisoned")
            .take();
        if let Some(block) = block {
            block.started.notify_one();
            block
                .release
                .acquire()
                .await
                .expect("classification preparation test hook remains open")
                .forget();
        }
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(super) struct PreparationBlock {
    started: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}

#[cfg(test)]
impl Default for PreparationBlock {
    fn default() -> Self {
        Self {
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        }
    }
}

#[cfg(test)]
impl PreparationBlock {
    pub(super) async fn wait_until_started(&self) {
        self.started.notified().await;
    }

    pub(super) fn release(&self) {
        self.release.add_permits(1);
    }
}

async fn prepare_observation(
    membership: Arc<MempoolSnapshot>,
    revision: u64,
    classifications: TransactionClassifications,
) -> Result<PreparedObservation, RuntimeError> {
    let started_at = Instant::now();
    let observation = tokio::task::spawn_blocking(move || {
        MempoolObservation::materialize_retained(&membership, revision, classifications)
    })
    .await??;
    Ok(PreparedObservation {
        latest: Arc::new(observation.snapshot),
        classifications: observation.classifications,
        materialization_validation_ms: started_at.elapsed().as_millis(),
    })
}

async fn encode_response(response: SourceSnapshotResponse) -> Result<(Bytes, u128), RuntimeError> {
    let started_at = Instant::now();
    let encoded = tokio::task::spawn_blocking(move || serde_json::to_vec(&response)).await??;
    Ok((Bytes::from(encoded), started_at.elapsed().as_millis()))
}

fn replace_cached_response(
    publisher: &CurrentStatePublisher,
    state: &mut CurrentState,
    body: Bytes,
) -> (u64, usize) {
    state.response_revision = state.response_revision.wrapping_add(1);
    let response_revision = state.response_revision;
    let etag = state.latest.as_ref().map(|snapshot| {
        format!(
            "W/\"{}-{}-{response_revision}\"",
            publisher.source_id, snapshot.observed_at_ms
        )
    });
    let encoded_bytes = body.len();
    state.cached_response = Some(CachedSnapshotResponse { body, etag });
    (response_revision, encoded_bytes)
}

fn summary_from_state(publisher: &CurrentStatePublisher, state: &CurrentState) -> SourceSummary {
    summary_from_parts(
        publisher,
        state.last_poll_started_at_ms,
        state.latest.as_ref(),
        state.classification_state,
        state.last_error.as_deref(),
    )
}

fn summary_from_parts(
    publisher: &CurrentStatePublisher,
    last_poll_started_at_ms: Option<u64>,
    latest: Option<&Arc<MempoolSnapshot>>,
    classification_state: Option<ClassificationState>,
    last_error: Option<&str>,
) -> SourceSummary {
    let availability = match (latest, last_error) {
        (None, None) => SourceAvailability::Waiting,
        (None, Some(_)) => SourceAvailability::Error,
        (Some(_), None) => SourceAvailability::Ready,
        (Some(_), Some(_)) => SourceAvailability::Stale,
    };
    let classification = match (latest, classification_state) {
        (Some(snapshot), Some(state)) => {
            Some(ClassificationProgress::from_snapshot(state, snapshot))
        }
        (None, None) => None,
        _ => {
            debug_assert!(false, "snapshot and classification lifecycle must coexist");
            None
        }
    };
    SourceSummary {
        source_id: publisher.source_id.clone(),
        source_label: publisher.source_label.clone(),
        availability,
        poll_interval_seconds: publisher.poll_interval.as_secs(),
        last_poll_started_at_ms,
        snapshot_observed_at_ms: latest.map(|snapshot| snapshot.observed_at_ms),
        chain_tip: latest.map(|snapshot| snapshot.chain_tip.clone()),
        transaction_count: latest.map(|snapshot| snapshot.transaction_count),
        total_vsize: latest.map(|snapshot| snapshot.total_vsize),
        classification,
        last_error: last_error.map(str::to_owned),
    }
}
