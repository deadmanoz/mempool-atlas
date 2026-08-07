use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::sync::RwLock;
use tracing::info;

use crate::classification::{ClassificationGenerationStart, ClassificationRevisionDelta};
use crate::model::{
    ClassificationProgress, ClassificationState, MempoolObservation, MempoolSnapshot,
    SourceAvailability, SourceSummary, TransactionClassifications, TransactionDetailResponse,
};
#[cfg(test)]
use crate::staged_snapshot::reencode_manifest_for_source_with_limits;
use crate::staged_snapshot::{
    EncodedManifest, EncodedStage, StageDescriptor, StageKind, StagedSnapshotBundle,
    StagedSnapshotLimits, encode_staged_snapshot_with_limits, reencode_manifest_for_source,
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
    #[cfg(test)]
    next_publication_limits: std::sync::Mutex<Option<StagedSnapshotLimits>>,
    #[cfg(test)]
    next_poll_start_reencoding_limits: std::sync::Mutex<Option<StagedSnapshotLimits>>,
    #[cfg(test)]
    next_manifest_replacement_preparation: std::sync::Mutex<Option<Arc<PreparationBlock>>>,
}

#[derive(Debug, Default)]
struct CurrentState {
    last_poll_started_at_ms: Option<u64>,
    poll_start_publication_failed: bool,
    membership: Option<Arc<MempoolSnapshot>>,
    latest: Option<Arc<MempoolSnapshot>>,
    classifications: TransactionClassifications,
    last_error: Option<String>,
    publication: Option<CurrentV2Publication>,
    classification_generation: Option<u64>,
    classification_revision: u64,
    classification_state: Option<ClassificationState>,
    status_revision: u64,
}

#[derive(Clone, Debug)]
struct EncodedBody {
    content_id: String,
    body: Bytes,
}

#[derive(Clone, Debug)]
struct PublishedStage {
    descriptor: StageDescriptor,
    body: Bytes,
}

#[derive(Clone, Debug)]
struct CurrentV2Publication {
    manifest_value: crate::staged_snapshot::StagedSnapshotManifest,
    manifest: EncodedBody,
    stages: Vec<PublishedStage>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PublicationPayload {
    pub(crate) body: Bytes,
    pub(crate) etag: String,
    pub(crate) content_id: String,
    pub(crate) uncompressed_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PublicationLookup<T> {
    Ready(T),
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum StageLookup {
    Ready(PublicationPayload),
    Unavailable,
    Superseded,
    Unknown,
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
            #[cfg(test)]
            next_publication_limits: std::sync::Mutex::new(None),
            #[cfg(test)]
            next_poll_start_reencoding_limits: std::sync::Mutex::new(None),
            #[cfg(test)]
            next_manifest_replacement_preparation: std::sync::Mutex::new(None),
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

    pub(super) async fn record_poll_started(&self, started_at_ms: u64) -> Result<(), RuntimeError> {
        loop {
            let (
                status_revision,
                classification_generation,
                classification_revision,
                source,
                retained_manifest,
            ) = {
                let state = self.state.read().await;
                if state.last_poll_started_at_ms == Some(started_at_ms)
                    && !state.poll_start_publication_failed
                {
                    return Ok(());
                }
                let source = summary_from_parts(
                    self,
                    Some(started_at_ms),
                    state.latest.as_ref(),
                    state.classification_state,
                    state.last_error.as_deref(),
                );
                (
                    state.status_revision,
                    state.classification_generation,
                    state.classification_revision,
                    source,
                    state
                        .publication
                        .as_ref()
                        .map(|publication| publication.manifest_value.clone()),
                )
            };
            let replacement = if let Some(manifest) = retained_manifest.as_ref() {
                match self.reencode_poll_start_manifest(manifest, &source) {
                    Ok(replacement) => Some(replacement),
                    Err(error) => {
                        let mut state = self.state.write().await;
                        if state.status_revision != status_revision
                            || state.classification_generation != classification_generation
                            || state.classification_revision != classification_revision
                        {
                            continue;
                        }
                        state.poll_start_publication_failed = true;
                        state.status_revision = state.status_revision.wrapping_add(1);
                        return Err(error.into());
                    }
                }
            } else {
                None
            };
            #[cfg(test)]
            if replacement.is_some() {
                self.wait_on_next_manifest_replacement_preparation().await;
            }
            let mut state = self.state.write().await;
            if state.status_revision != status_revision
                || state.classification_generation != classification_generation
                || state.classification_revision != classification_revision
            {
                continue;
            }
            if let Some(manifest) = replacement {
                replace_manifest(&mut state, manifest)?;
            }
            state.last_poll_started_at_ms = Some(started_at_ms);
            state.poll_start_publication_failed = false;
            state.status_revision = state.status_revision.wrapping_add(1);
            return Ok(());
        }
    }

    pub(super) async fn summary(&self) -> SourceSummary {
        let state = self.state.read().await;
        summary_from_state(self, &state)
    }

    #[cfg(test)]
    pub(super) async fn published_state(&self) -> PublishedState {
        let state = self.state.read().await;
        PublishedState {
            source: summary_from_state(self, &state),
            snapshot: state.latest.clone(),
        }
    }

    pub(super) async fn manifest_payload(&self) -> PublicationLookup<PublicationPayload> {
        let state = self.state.read().await;
        let Some(publication) = &state.publication else {
            return PublicationLookup::Unavailable;
        };
        PublicationLookup::Ready(PublicationPayload {
            body: publication.manifest.body.clone(),
            etag: manifest_etag(&publication.manifest.content_id),
            content_id: publication.manifest.content_id.clone(),
            uncompressed_bytes: publication.manifest.body.len() as u64,
        })
    }

    pub(super) async fn stage_payload(
        &self,
        kind: StageKind,
        classifier_id: Option<&str>,
        content_id: &str,
    ) -> StageLookup {
        let state = self.state.read().await;
        let Some(publication) = &state.publication else {
            return StageLookup::Unavailable;
        };
        let Some(stage) = publication.stages.iter().find(|stage| {
            stage.descriptor.kind == kind
                && stage.descriptor.classifier_id.as_deref() == classifier_id
        }) else {
            return StageLookup::Unknown;
        };
        if stage.descriptor.content_id != content_id {
            if publication
                .stages
                .iter()
                .any(|candidate| candidate.descriptor.content_id == content_id)
            {
                return StageLookup::Unknown;
            }
            return StageLookup::Superseded;
        }
        StageLookup::Ready(PublicationPayload {
            body: stage.body.clone(),
            etag: stage_etag(stage),
            content_id: stage.descriptor.content_id.clone(),
            uncompressed_bytes: stage.descriptor.uncompressed_bytes,
        })
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
                    if state.poll_start_publication_failed {
                        None
                    } else {
                        state.last_poll_started_at_ms
                    },
                )
            };
            if current_generation.is_some_and(|current| current >= generation) {
                return Ok(false);
            }
            let source = summary_from_parts(
                self,
                last_poll_started_at_ms,
                Some(&prepared.latest),
                Some(classification_state),
                None,
            );
            let (candidate, json_encoding_ms) = self
                .prepare_publication(source, Arc::clone(&prepared.latest), None)
                .await?;

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
            state.last_poll_started_at_ms = last_poll_started_at_ms;
            state.poll_start_publication_failed = false;
            state.last_error = None;
            state.classification_generation = Some(generation);
            state.classification_revision = revision;
            state.classification_state = Some(classification_state);
            state.status_revision = state.status_revision.wrapping_add(1);
            let encoded_bytes = candidate.encoded_bytes();
            let publication_id = candidate.manifest_value.publication_id.clone();
            state.publication = Some(candidate);
            let total_classified_count = state.classifications.len();
            let commit_ms = commit_started_at.elapsed().as_millis();
            drop(state);
            info!(
                source_id = %self.source_id,
                generation,
                classification_revision = revision,
                publication_id,
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
            reused_membership,
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
                state
                    .publication
                    .as_ref()
                    .ok_or(RuntimeError::PublicationStateWithoutBundle)?
                    .membership_stages()?,
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
            let source = summary_from_parts(
                self,
                last_poll_started_at_ms,
                Some(&prepared.latest),
                Some(classification_state),
                last_error.as_deref(),
            );
            let (candidate, json_encoding_ms) = self
                .prepare_publication(
                    source,
                    Arc::clone(&prepared.latest),
                    Some(reused_membership.clone()),
                )
                .await?;

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
            let encoded_bytes = candidate.encoded_bytes();
            let publication_id = candidate.manifest_value.publication_id.clone();
            state.publication = Some(candidate);
            let total_classified_count = state.classifications.len();
            let commit_ms = commit_started_at.elapsed().as_millis();
            drop(state);
            info!(
                source_id = %self.source_id,
                generation,
                classification_revision = delta.revision,
                publication_id,
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
                retained_manifest,
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
                    state
                        .publication
                        .as_ref()
                        .map(|publication| publication.manifest_value.clone())
                        .ok_or(RuntimeError::PublicationStateWithoutBundle)?,
                )
            };
            if current_classification_state == Some(classification_state) {
                return Ok(true);
            }
            let source = summary_from_parts(
                self,
                last_poll_started_at_ms,
                Some(&latest),
                Some(classification_state),
                last_error.as_deref(),
            );
            let encode_started_at = Instant::now();
            let manifest = reencode_manifest_for_source(&retained_manifest, &source)?;
            let json_encoding_ms = encode_started_at.elapsed().as_millis();

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
            let publication_id = manifest.value.publication_id.clone();
            let encoded_bytes = replace_manifest(&mut state, manifest)?;
            state.classification_state = Some(classification_state);
            state.status_revision = state.status_revision.wrapping_add(1);
            let total_classified_count = state.classifications.len();
            let commit_ms = commit_started_at.elapsed().as_millis();
            drop(state);
            info!(
                source_id = %self.source_id,
                generation,
                classification_revision = current_revision,
                publication_id,
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
                retained_manifest,
            ) = {
                let state = self.state.read().await;
                (
                    state.status_revision,
                    state.classification_generation,
                    state.classification_revision,
                    state.last_poll_started_at_ms,
                    state.latest.clone(),
                    state.classification_state,
                    state
                        .publication
                        .as_ref()
                        .map(|publication| publication.manifest_value.clone()),
                )
            };
            let source = summary_from_parts(
                self,
                last_poll_started_at_ms,
                latest.as_ref(),
                classification_state,
                Some(&error),
            );
            let encode_started_at = Instant::now();
            let manifest = retained_manifest
                .as_ref()
                .map(|retained| reencode_manifest_for_source(retained, &source))
                .transpose()?;
            let json_encoding_ms = encode_started_at.elapsed().as_millis();
            #[cfg(test)]
            if manifest.is_some() {
                self.wait_on_next_manifest_replacement_preparation().await;
            }

            let commit_started_at = Instant::now();
            let mut state = self.state.write().await;
            if state.status_revision != status_revision
                || state.classification_generation != classification_generation
                || state.classification_revision != classification_revision
            {
                continue;
            }
            let (publication_id, encoded_bytes) = if let Some(manifest) = manifest {
                let publication_id = manifest.value.publication_id.clone();
                let encoded_bytes = replace_manifest(&mut state, manifest)?;
                (Some(publication_id), encoded_bytes)
            } else {
                (None, 0)
            };
            state.last_error = Some(error);
            state.status_revision = state.status_revision.wrapping_add(1);
            let total_classified_count = state.classifications.len();
            let commit_ms = commit_started_at.elapsed().as_millis();
            drop(state);
            info!(
                source_id = %self.source_id,
                classification_revision,
                publication_id,
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
            return TransactionLookup::WaitingForPublication;
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

    async fn prepare_publication(
        &self,
        source: SourceSummary,
        snapshot: Arc<MempoolSnapshot>,
        reused_membership: Option<(EncodedStage, EncodedStage)>,
    ) -> Result<(CurrentV2Publication, u128), RuntimeError> {
        let limits = self.take_publication_limits();
        let started_at = Instant::now();
        let bundle = tokio::task::spawn_blocking(move || {
            encode_staged_snapshot_with_limits(&source, &snapshot, reused_membership, limits)
        })
        .await??;
        Ok((
            CurrentV2Publication::from_bundle(bundle),
            started_at.elapsed().as_millis(),
        ))
    }

    fn take_publication_limits(&self) -> StagedSnapshotLimits {
        #[cfg(test)]
        {
            return self
                .next_publication_limits
                .lock()
                .expect("publication limit test hook is not poisoned")
                .take()
                .unwrap_or_default();
        }
        #[cfg(not(test))]
        StagedSnapshotLimits::default()
    }

    fn reencode_poll_start_manifest(
        &self,
        retained: &crate::staged_snapshot::StagedSnapshotManifest,
        source: &SourceSummary,
    ) -> Result<EncodedManifest, crate::staged_snapshot::StagedSnapshotError> {
        #[cfg(test)]
        if let Some(limits) = self
            .next_poll_start_reencoding_limits
            .lock()
            .expect("poll-start re-encoding limit test hook is not poisoned")
            .take()
        {
            return reencode_manifest_for_source_with_limits(retained, source, limits);
        }
        reencode_manifest_for_source(retained, source)
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
    pub(super) fn block_next_manifest_replacement_preparation(&self) -> Arc<PreparationBlock> {
        let block = Arc::new(PreparationBlock::default());
        *self
            .next_manifest_replacement_preparation
            .lock()
            .expect("manifest replacement test hook is not poisoned") = Some(Arc::clone(&block));
        block
    }

    #[cfg(test)]
    pub(super) fn limit_next_publication(&self, limits: StagedSnapshotLimits) {
        *self
            .next_publication_limits
            .lock()
            .expect("publication limit test hook is not poisoned") = Some(limits);
    }

    #[cfg(test)]
    pub(super) fn limit_next_poll_start_reencoding(&self, limits: StagedSnapshotLimits) {
        *self
            .next_poll_start_reencoding_limits
            .lock()
            .expect("poll-start re-encoding limit test hook is not poisoned") = Some(limits);
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

    #[cfg(test)]
    async fn wait_on_next_manifest_replacement_preparation(&self) {
        let block = self
            .next_manifest_replacement_preparation
            .lock()
            .expect("manifest replacement test hook is not poisoned")
            .take();
        if let Some(block) = block {
            block.started.notify_one();
            block
                .release
                .acquire()
                .await
                .expect("manifest replacement test hook remains open")
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

impl CurrentV2Publication {
    fn from_bundle(bundle: StagedSnapshotBundle) -> Self {
        let manifest_value = bundle.manifest.value;
        let manifest = encoded_manifest_body(bundle.manifest.content_id, bundle.manifest.bytes);
        let stages = std::iter::once(bundle.population)
            .chain(std::iter::once(bundle.membership))
            .chain(std::iter::once(bundle.structure))
            .chain(bundle.classifier_stages)
            .map(published_stage)
            .collect();
        Self {
            manifest_value,
            manifest,
            stages,
        }
    }

    fn membership_stages(&self) -> Result<(EncodedStage, EncodedStage), RuntimeError> {
        let population = self
            .stages
            .iter()
            .find(|stage| stage.descriptor.kind == StageKind::Population)
            .ok_or(RuntimeError::PublicationStateWithoutBundle)?;
        let membership = self
            .stages
            .iter()
            .find(|stage| stage.descriptor.kind == StageKind::Membership)
            .ok_or(RuntimeError::PublicationStateWithoutBundle)?;
        Ok((population.encoded_stage(), membership.encoded_stage()))
    }

    fn encoded_bytes(&self) -> usize {
        self.stages
            .iter()
            .fold(self.manifest.body.len(), |total, stage| {
                total.saturating_add(stage.body.len())
            })
    }
}

impl PublishedStage {
    fn encoded_stage(&self) -> EncodedStage {
        EncodedStage {
            descriptor: self.descriptor.clone(),
            bytes: self.body.clone(),
        }
    }
}

fn encoded_manifest_body(content_id: String, bytes: Bytes) -> EncodedBody {
    EncodedBody {
        content_id,
        body: bytes,
    }
}

fn published_stage(stage: EncodedStage) -> PublishedStage {
    PublishedStage {
        descriptor: stage.descriptor,
        body: stage.bytes,
    }
}

fn replace_manifest(
    state: &mut CurrentState,
    manifest: EncodedManifest,
) -> Result<usize, RuntimeError> {
    let publication = state
        .publication
        .as_mut()
        .ok_or(RuntimeError::PublicationStateWithoutBundle)?;
    let stage_graph_matches = manifest.value.stages.len() == publication.stages.len()
        && manifest
            .value
            .stages
            .iter()
            .zip(&publication.stages)
            .all(|(descriptor, stage)| descriptor == &stage.descriptor);
    if !stage_graph_matches
        || manifest.value.population_id != publication.manifest_value.population_id
        || manifest.value.classification_set_id != publication.manifest_value.classification_set_id
    {
        return Err(RuntimeError::PublicationManifestStageMismatch);
    }
    publication.manifest_value = manifest.value;
    publication.manifest = encoded_manifest_body(manifest.content_id, manifest.bytes);
    Ok(publication.encoded_bytes())
}

fn manifest_etag(content_id: &str) -> String {
    format!("W/\"atlas-v2-manifest-{content_id}\"")
}

fn stage_etag(stage: &PublishedStage) -> String {
    let kind = match stage.descriptor.kind {
        StageKind::Population => "population".to_owned(),
        StageKind::Membership => "membership".to_owned(),
        StageKind::Structure => "structure".to_owned(),
        StageKind::Classifier => format!(
            "classifier-{}",
            stage
                .descriptor
                .classifier_id
                .as_deref()
                .expect("classifier stages carry their classifier ID")
        ),
    };
    format!(
        "W/\"atlas-v2-stage-{kind}-{}\"",
        stage.descriptor.content_id
    )
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub(super) struct PublishedState {
    pub(super) source: SourceSummary,
    pub(super) snapshot: Option<Arc<MempoolSnapshot>>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ChainTip;

    #[test]
    fn manifest_replacement_reports_a_missing_publication_bundle() {
        let publisher = CurrentStatePublisher::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            Duration::from_secs(30),
        );
        let snapshot = Arc::new(
            MempoolSnapshot::new(
                "core".to_owned(),
                "Bitcoin Core".to_owned(),
                1_700_000_000_000,
                ChainTip {
                    height: 900_000,
                    hash: "00".repeat(32),
                },
                Vec::new(),
            )
            .expect("snapshot"),
        );
        let source = summary_from_parts(
            &publisher,
            None,
            Some(&snapshot),
            Some(ClassificationState::Complete),
            None,
        );
        let manifest = encode_staged_snapshot_with_limits(
            &source,
            &snapshot,
            None,
            StagedSnapshotLimits::default(),
        )
        .expect("encoded staged snapshot")
        .manifest;

        let error = replace_manifest(&mut CurrentState::default(), manifest)
            .expect_err("missing publication bundle must be reported");

        assert!(matches!(error, RuntimeError::PublicationStateWithoutBundle));
    }

    #[test]
    fn manifest_replacement_reports_the_full_retained_bundle_size() {
        let publisher = CurrentStatePublisher::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            Duration::from_secs(30),
        );
        let snapshot = Arc::new(
            MempoolSnapshot::new(
                "core".to_owned(),
                "Bitcoin Core".to_owned(),
                1_700_000_000_000,
                ChainTip {
                    height: 900_000,
                    hash: "00".repeat(32),
                },
                Vec::new(),
            )
            .expect("snapshot"),
        );
        let source = summary_from_parts(
            &publisher,
            None,
            Some(&snapshot),
            Some(ClassificationState::Complete),
            None,
        );
        let bundle = encode_staged_snapshot_with_limits(
            &source,
            &snapshot,
            None,
            StagedSnapshotLimits::default(),
        )
        .expect("encoded staged snapshot");
        let retained_manifest = bundle.manifest.value.clone();
        let mut state = CurrentState {
            publication: Some(CurrentV2Publication::from_bundle(bundle)),
            ..CurrentState::default()
        };
        let mut changed_source = source;
        changed_source.last_error = Some("RPC timeout".to_owned());
        let replacement = reencode_manifest_for_source(&retained_manifest, &changed_source)
            .expect("replacement manifest");

        let encoded_bytes = replace_manifest(&mut state, replacement).expect("replace manifest");
        let publication = state.publication.as_ref().expect("publication");
        let stage_bytes = publication
            .stages
            .iter()
            .map(|stage| stage.body.len())
            .sum::<usize>();

        assert_eq!(encoded_bytes, publication.manifest.body.len() + stage_bytes);
        assert_eq!(encoded_bytes, publication.encoded_bytes());
    }

    #[test]
    fn manifest_replacement_rejects_a_different_stage_graph() {
        let publisher = CurrentStatePublisher::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            Duration::from_secs(30),
        );
        let snapshot = Arc::new(
            MempoolSnapshot::new(
                "core".to_owned(),
                "Bitcoin Core".to_owned(),
                1_700_000_000_000,
                ChainTip {
                    height: 900_000,
                    hash: "00".repeat(32),
                },
                Vec::new(),
            )
            .expect("snapshot"),
        );
        let source = summary_from_parts(
            &publisher,
            None,
            Some(&snapshot),
            Some(ClassificationState::Complete),
            None,
        );
        let bundle = encode_staged_snapshot_with_limits(
            &source,
            &snapshot,
            None,
            StagedSnapshotLimits::default(),
        )
        .expect("encoded staged snapshot");
        let retained_manifest = bundle.manifest.value.clone();
        let mut state = CurrentState {
            publication: Some(CurrentV2Publication::from_bundle(bundle)),
            ..CurrentState::default()
        };
        let mut replacement = reencode_manifest_for_source(&retained_manifest, &source)
            .expect("replacement manifest");
        replacement.value.stages[0].content_id = "ff".repeat(32);

        let error = replace_manifest(&mut state, replacement)
            .expect_err("a different stage graph must be rejected");

        assert!(matches!(
            error,
            RuntimeError::PublicationManifestStageMismatch
        ));
    }
}
