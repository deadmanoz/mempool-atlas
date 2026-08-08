use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use thiserror::Error;
use tokio::sync::{Notify, Semaphore};
use tracing::{info, warn};

mod publisher;

use publisher::CurrentStatePublisher;
pub(crate) use publisher::{PublicationLookup, PublicationPayload, StageLookup};

use crate::classification::{
    ClassificationDisposition, ClassificationError, ClassificationGenerationStart,
    ClassificationPipeline, ClassificationRevisionDelta, ClassificationSliceReport,
};
use crate::model::{
    ClassificationState, ModelError, SourceSummary, SourcesResponse, TransactionDetailResponse,
    validate_source_id, validate_source_label,
};
#[cfg(test)]
use crate::model::{MempoolObservation, MempoolSnapshot, SourceAvailability};
use crate::rpc::{RpcClient, RpcError};
#[cfg(test)]
use crate::staged_snapshot::StagedSnapshotLimits;
use crate::staged_snapshot::{StageKind, StagedSnapshotError};

pub const MAX_CONFIGURED_SOURCES: usize = 4;

#[derive(Clone, Debug)]
pub struct AtlasSource {
    runtime: Arc<SourceRuntime>,
    rpc: RpcClient,
    classification: ClassificationPipeline,
}

impl AtlasSource {
    pub fn new(
        runtime: Arc<SourceRuntime>,
        rpc: RpcClient,
        classification: ClassificationPipeline,
    ) -> Self {
        Self {
            runtime,
            rpc,
            classification,
        }
    }
}

/// Coordinates all current-state sources under one bounded RPC work gate.
///
/// A membership round owns the gate while it polls every source in configured
/// order. Classification then advances sources round-robin in bounded slices,
/// releasing the gate between slices so a due membership round takes priority.
#[derive(Debug)]
pub struct AtlasRuntime {
    sources: Vec<AtlasSource>,
    poll_interval: Duration,
    rpc_work_gate: Semaphore,
    classification_wakeup: Notify,
    classification_pending: Mutex<BTreeSet<usize>>,
}

impl AtlasRuntime {
    pub fn new(sources: Vec<AtlasSource>, poll_interval: Duration) -> Result<Self, RuntimeError> {
        if sources.is_empty() {
            return Err(RuntimeError::NoSources);
        }
        if sources.len() > MAX_CONFIGURED_SOURCES {
            return Err(RuntimeError::TooManySources {
                configured: sources.len(),
                maximum: MAX_CONFIGURED_SOURCES,
            });
        }
        if poll_interval.is_zero() {
            return Err(RuntimeError::ZeroPollInterval);
        }
        let mut source_ids = BTreeSet::new();
        for source in &sources {
            let source_id = source.runtime.source_id().to_owned();
            if !source_ids.insert(source_id.clone()) {
                return Err(RuntimeError::DuplicateSource(source_id));
            }
            if source.runtime.poll_interval() != poll_interval {
                return Err(RuntimeError::PollIntervalMismatch {
                    source_id,
                    expected_seconds: poll_interval.as_secs(),
                    actual_seconds: source.runtime.poll_interval().as_secs(),
                });
            }
        }
        Ok(Self {
            sources,
            poll_interval,
            rpc_work_gate: Semaphore::new(1),
            classification_wakeup: Notify::new(),
            classification_pending: Mutex::new(BTreeSet::new()),
        })
    }

    pub fn source_runtimes(&self) -> Vec<Arc<SourceRuntime>> {
        self.sources
            .iter()
            .map(|source| Arc::clone(&source.runtime))
            .collect()
    }

    pub async fn run(self: Arc<Self>) {
        tokio::join!(
            Arc::clone(&self).run_membership_rounds(),
            self.run_classification()
        );
    }

    async fn run_membership_rounds(self: Arc<Self>) {
        let mut interval = tokio::time::interval(self.poll_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut round_id = 0_u64;
        loop {
            interval.tick().await;
            round_id = round_id.wrapping_add(1);
            self.poll_round(round_id).await;
        }
    }

    async fn poll_round(&self, round_id: u64) {
        let round_started_at = Instant::now();
        let permit = self
            .rpc_work_gate
            .acquire()
            .await
            .expect("Atlas RPC work gate remains open");
        info!(
            round_id,
            source_count = self.sources.len(),
            "starting sequential mempool snapshot round"
        );
        let gate_held_started_at = Instant::now();
        let mut outcomes = Vec::with_capacity(self.sources.len());
        for (source_order, source) in self.sources.iter().enumerate() {
            let source_started_at = Instant::now();
            let started_at_ms = match system_now_ms() {
                Ok(started_at_ms) => started_at_ms,
                Err(error) => {
                    outcomes.push((source_order, Err(error)));
                    continue;
                }
            };
            if let Err(error) = source.runtime.record_poll_started(started_at_ms).await {
                warn!(
                    round_id,
                    source_order,
                    source_id = %source.runtime.source_id(),
                    error = %error,
                    "failed to publish poll-start metadata; continuing source poll"
                );
            }
            let outcome = match source
                .rpc
                .get_mempool_snapshot(source.runtime.source_id(), source.runtime.source_label())
                .await
            {
                Ok(snapshot) => source
                    .classification
                    .install_snapshot(snapshot)
                    .map_err(RuntimeError::from),
                Err(error) => Err(RuntimeError::Rpc(error)),
            };
            outcomes.push((source_order, outcome));
            info!(
                round_id,
                source_order,
                source_id = %source.runtime.source_id(),
                elapsed_ms = source_started_at.elapsed().as_millis(),
                "completed source membership turn"
            );
        }
        let gate_held_ms = gate_held_started_at.elapsed().as_millis();
        drop(permit);
        info!(
            round_id,
            elapsed_ms = round_started_at.elapsed().as_millis(),
            gate_held_ms,
            "completed sequential mempool RPC round"
        );
        for (source_index, outcome) in outcomes {
            let source = &self.sources[source_index];
            match outcome {
                Ok(publication) => {
                    let membership = Arc::clone(&publication.membership);
                    let classified = publication.classifications.len();
                    match source.runtime.publish_membership(publication).await {
                        Ok(true) => {
                            info!(
                                source_id = %source.runtime.source_id(),
                                transaction_count = membership.transaction_count,
                                classified,
                                observed_at_ms = membership.observed_at_ms,
                                collection_started_at_ms = membership.collection_started_at_ms,
                                collection_completed_at_ms = membership.collection_completed_at_ms,
                                collection_duration_ms = membership.collection_duration_ms,
                                chain_height = membership.chain_tip.height,
                                chain_hash = %membership.chain_tip.hash,
                                "published complete mempool membership"
                            );
                            self.schedule_classification(source_index);
                        }
                        Ok(false) => {}
                        Err(error) => {
                            if let Err(state_error) = source
                                .runtime
                                .record_failure("Atlas publication preparation failed".to_owned())
                                .await
                            {
                                warn!(
                                    round_id,
                                    source_order = source_index,
                                    source_id = %source.runtime.source_id(),
                                    error = %state_error,
                                    "failed to publish v2 preparation failure metadata"
                                );
                            }
                            warn!(
                                round_id,
                                source_order = source_index,
                                source_id = %source.runtime.source_id(),
                                error = %error,
                                "mempool snapshot publication failed; retaining the last good v2 bundle"
                            );
                        }
                    }
                }
                Err(RuntimeError::Rpc(error)) => {
                    if let Err(state_error) = source
                        .runtime
                        .record_failure(error.public_message().to_owned())
                        .await
                    {
                        warn!(
                            round_id,
                            source_order = source_index,
                            source_id = %source.runtime.source_id(),
                            error = %state_error,
                            "failed to publish source failure"
                        );
                    }
                    warn!(
                        round_id,
                        source_order = source_index,
                        source_id = %source.runtime.source_id(),
                        error = %error,
                        "mempool snapshot poll failed; retaining the last good snapshot"
                    );
                }
                Err(error) => warn!(
                    round_id,
                    source_order = source_index,
                    source_id = %source.runtime.source_id(),
                    error = %error,
                    "mempool snapshot poll failed; retaining the last good snapshot"
                ),
            }
        }
    }

    fn schedule_classification(&self, source_index: usize) {
        assert!(
            source_index < self.sources.len(),
            "classification source index must be configured"
        );
        self.classification_pending
            .lock()
            .expect("classification pending set is not poisoned")
            .insert(source_index);
        self.classification_wakeup.notify_one();
    }

    async fn run_classification(&self) {
        loop {
            self.classification_wakeup.notified().await;
            let mut active = BTreeSet::new();
            let mut next_source_index = 0;
            loop {
                self.take_pending_classification(&mut active);
                let Some(source_index) = active
                    .range(next_source_index..)
                    .next()
                    .copied()
                    .or_else(|| active.first().copied())
                else {
                    break;
                };
                active.remove(&source_index);
                let source = &self.sources[source_index];
                let permit = self
                    .rpc_work_gate
                    .acquire()
                    .await
                    .expect("Atlas RPC work gate remains open");
                let gate_held_started_at = Instant::now();
                let expected_generation = source.runtime.classification_generation().await;
                let report = source
                    .runtime
                    .classify_one_slice(&source.classification, expected_generation)
                    .await;
                let gate_held_ms = gate_held_started_at.elapsed().as_millis();
                drop(permit);
                info!(
                    source_id = %source.runtime.source_id(),
                    gate_held_ms,
                    "released RPC work gate after classification slice"
                );
                if source
                    .runtime
                    .publish_classification(report, expected_generation)
                    .await
                {
                    active.insert(source_index);
                }
                next_source_index = (source_index + 1) % self.sources.len();
                tokio::task::yield_now().await;
            }
        }
    }

    fn take_pending_classification(&self, active: &mut BTreeSet<usize>) {
        let mut pending = self
            .classification_pending
            .lock()
            .expect("classification pending set is not poisoned");
        active.append(&mut pending);
    }
}

#[derive(Debug)]
pub struct SourceRuntime {
    publisher: CurrentStatePublisher,
    #[cfg(test)]
    classification_wakeup: Notify,
}

impl SourceRuntime {
    pub fn new(
        source_id: String,
        source_label: String,
        poll_interval: Duration,
    ) -> Result<Self, RuntimeError> {
        validate_source_id(&source_id)?;
        validate_source_label(&source_label)?;
        if poll_interval.is_zero() {
            return Err(RuntimeError::ZeroPollInterval);
        }
        Ok(Self {
            publisher: CurrentStatePublisher::new(source_id, source_label, poll_interval),
            #[cfg(test)]
            classification_wakeup: Notify::new(),
        })
    }

    pub fn source_id(&self) -> &str {
        self.publisher.source_id()
    }

    pub fn source_label(&self) -> &str {
        self.publisher.source_label()
    }

    fn poll_interval(&self) -> Duration {
        self.publisher.poll_interval()
    }

    async fn classification_generation(&self) -> Option<u64> {
        self.publisher.classification_generation().await
    }

    #[cfg(test)]
    async fn run_classification(&self, classification: ClassificationPipeline) {
        loop {
            self.classification_wakeup.notified().await;
            loop {
                let expected_generation = self.classification_generation().await;
                let report = self
                    .classify_one_slice(&classification, expected_generation)
                    .await;
                if !self
                    .publish_classification(report, expected_generation)
                    .await
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }
    }

    async fn classify_one_slice(
        &self,
        classification: &ClassificationPipeline,
        expected_generation: Option<u64>,
    ) -> Result<ClassificationSliceReport, ClassificationError> {
        let slice_started_at = Instant::now();
        let report = classification
            .classify_next_for_generation(expected_generation)
            .await?;
        info!(
            source_id = %self.source_id(),
            generation = report.generation,
            attempted = report.attempted,
            newly_classified = report.newly_classified,
            fact_requests = report.fact_requests,
            facts_resolved = report.facts_resolved,
            facts_missing = report.facts_missing,
            capacity_deferred = report.capacity_deferred,
            deferred_candidates = report.deferred_candidates,
            response_failures = report.response_failures,
            systemic_response_failures = report.systemic_response_failures,
            missing_responses = report.missing_responses,
            batch_failures = report.batch_failures,
            response_bytes = report.response_bytes,
            elapsed_ms = slice_started_at.elapsed().as_millis(),
            classified = report.classified,
            remaining = report.remaining,
            complete = report.complete,
            stale = report.stale,
            "completed BIP-110 classification slice"
        );
        if report.disposition == ClassificationDisposition::Paused {
            warn!(
                source_id = %self.source_id(),
                generation = report.generation,
                response_failures = report.response_failures,
                systemic_response_failures = report.systemic_response_failures,
                missing_responses = report.missing_responses,
                batch_failures = report.batch_failures,
                capacity_deferred = report.capacity_deferred,
                deferred_candidates = report.deferred_candidates,
                "pausing BIP-110 classification until the next membership generation"
            );
        }
        Ok(report)
    }

    async fn publish_classification(
        &self,
        report: Result<ClassificationSliceReport, ClassificationError>,
        expected_generation: Option<u64>,
    ) -> bool {
        let report = match report {
            Ok(report) => report,
            Err(error) => {
                if let Err(state_error) =
                    self.record_classification_error(expected_generation).await
                {
                    warn!(
                        source_id = %self.source_id(),
                        error = %state_error,
                        "failed to publish paused BIP-110 classification state"
                    );
                }
                warn!(
                    source_id = %self.source_id(),
                    error = %error,
                    "BIP-110 classification slice failed"
                );
                return false;
            }
        };
        match self.record_classification_outcome(report).await {
            Ok(continue_classification) => continue_classification,
            Err(error) => {
                if let Err(state_error) =
                    self.record_classification_error(expected_generation).await
                {
                    warn!(
                        source_id = %self.source_id(),
                        error = %state_error,
                        "failed to publish paused BIP-110 classification state after publication failure"
                    );
                }
                warn!(
                    source_id = %self.source_id(),
                    error = %error,
                    "failed to publish BIP-110 classification outcome"
                );
                false
            }
        }
    }

    pub async fn summary(&self) -> SourceSummary {
        self.publisher.summary().await
    }

    #[cfg(test)]
    async fn published_state(&self) -> publisher::PublishedState {
        self.publisher.published_state().await
    }

    #[cfg(test)]
    async fn current_manifest_payload(&self) -> Result<PublicationPayload, RuntimeError> {
        match self.manifest_payload().await {
            PublicationLookup::Ready(payload) => Ok(payload),
            PublicationLookup::Unavailable => Err(RuntimeError::PublicationStateWithoutBundle),
        }
    }

    #[cfg(test)]
    async fn manifest_bytes(&self) -> Result<bytes::Bytes, RuntimeError> {
        Ok(self.current_manifest_payload().await?.body)
    }

    pub(crate) async fn manifest_payload(&self) -> PublicationLookup<PublicationPayload> {
        self.publisher.manifest_payload().await
    }

    pub(crate) async fn stage_payload(
        &self,
        kind: StageKind,
        classifier_id: Option<&str>,
        content_id: &str,
    ) -> StageLookup {
        self.publisher
            .stage_payload(kind, classifier_id, content_id)
            .await
    }

    pub(crate) async fn record_poll_started(&self, started_at_ms: u64) -> Result<(), RuntimeError> {
        self.publisher.record_poll_started(started_at_ms).await
    }

    #[cfg(test)]
    pub(crate) async fn record_success(
        &self,
        observation: MempoolObservation,
    ) -> Result<(), RuntimeError> {
        let has_eligible_work = observation.snapshot.transactions.iter().any(|entry| {
            entry
                .bip110
                .as_ref()
                .is_none_or(|assessment| !assessment.unknown_rules.is_empty())
        });
        let generation = self
            .classification_generation()
            .await
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(RuntimeError::ClassificationGenerationOverflow)?;
        let MempoolObservation {
            snapshot,
            classifications,
        } = observation;
        self.record_membership(ClassificationGenerationStart {
            generation,
            revision: snapshot.classification_revision,
            has_eligible_work,
            membership: Arc::new(snapshot),
            classifications,
        })
        .await
    }

    #[cfg(test)]
    async fn record_membership(
        &self,
        publication: ClassificationGenerationStart,
    ) -> Result<(), RuntimeError> {
        self.publish_membership(publication).await?;
        Ok(())
    }

    async fn publish_membership(
        &self,
        publication: ClassificationGenerationStart,
    ) -> Result<bool, RuntimeError> {
        self.publisher.publish_membership(publication).await
    }

    async fn record_classification_outcome(
        &self,
        report: ClassificationSliceReport,
    ) -> Result<bool, RuntimeError> {
        let disposition = report.disposition;
        let classification_state = match disposition {
            ClassificationDisposition::Continue => ClassificationState::Classifying,
            ClassificationDisposition::Complete => ClassificationState::Complete,
            ClassificationDisposition::Paused => ClassificationState::Paused,
            ClassificationDisposition::Stale => return Ok(false),
        };
        let generation = report.generation;
        let current = self
            .record_classification_update(generation, report.publication, classification_state)
            .await?;
        Ok(current && disposition == ClassificationDisposition::Continue)
    }

    async fn record_classification_error(
        &self,
        expected_generation: Option<u64>,
    ) -> Result<(), RuntimeError> {
        let Some(generation) = expected_generation else {
            return Ok(());
        };
        self.record_classification_update(generation, None, ClassificationState::Paused)
            .await?;
        Ok(())
    }

    async fn record_classification_update(
        &self,
        generation: u64,
        publication: Option<ClassificationRevisionDelta>,
        classification_state: ClassificationState,
    ) -> Result<bool, RuntimeError> {
        self.publisher
            .publish_classification_update(generation, publication, classification_state)
            .await
    }

    #[cfg(test)]
    async fn record_classification_progress(
        &self,
        publication: ClassificationRevisionDelta,
    ) -> Result<bool, RuntimeError> {
        let generation = publication.generation;
        self.record_classification_update(
            generation,
            Some(publication),
            ClassificationState::Classifying,
        )
        .await
    }

    pub(crate) async fn record_failure(&self, error: String) -> Result<(), RuntimeError> {
        self.publisher.publish_failure(error).await
    }

    pub async fn transaction_detail(&self, txid: &str) -> TransactionLookup {
        self.publisher.transaction_detail(txid).await
    }

    #[cfg(test)]
    fn block_next_classification_preparation(&self) -> Arc<publisher::PreparationBlock> {
        self.publisher.block_next_classification_preparation()
    }

    #[cfg(test)]
    fn block_next_manifest_replacement_preparation(&self) -> Arc<publisher::PreparationBlock> {
        self.publisher.block_next_manifest_replacement_preparation()
    }

    #[cfg(test)]
    fn limit_next_publication(&self, limits: StagedSnapshotLimits) {
        self.publisher.limit_next_publication(limits);
    }

    #[cfg(test)]
    fn limit_next_poll_start_reencoding(&self, limits: StagedSnapshotLimits) {
        self.publisher.limit_next_poll_start_reencoding(limits);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransactionLookup {
    Ready(TransactionDetailResponse),
    WaitingForPublication,
    NotPresent,
    Unclassified,
}

#[derive(Clone, Debug)]
pub struct SourceRegistry {
    sources: Arc<BTreeMap<String, Arc<SourceRuntime>>>,
}

impl SourceRegistry {
    pub fn new(sources: Vec<Arc<SourceRuntime>>) -> Result<Self, RuntimeError> {
        if sources.len() > MAX_CONFIGURED_SOURCES {
            return Err(RuntimeError::TooManySources {
                configured: sources.len(),
                maximum: MAX_CONFIGURED_SOURCES,
            });
        }
        let mut indexed = BTreeMap::new();
        for source in sources {
            let source_id = source.source_id().to_owned();
            if indexed.insert(source_id.clone(), source).is_some() {
                return Err(RuntimeError::DuplicateSource(source_id));
            }
        }
        if indexed.is_empty() {
            return Err(RuntimeError::NoSources);
        }
        Ok(Self {
            sources: Arc::new(indexed),
        })
    }

    pub fn get(&self, source_id: &str) -> Option<Arc<SourceRuntime>> {
        self.sources.get(source_id).cloned()
    }

    pub async fn sources_response(&self) -> SourcesResponse {
        let mut sources = Vec::with_capacity(self.sources.len());
        for source in self.sources.values() {
            sources.push(source.summary().await);
        }
        SourcesResponse {
            atlas_version: crate::model::ATLAS_VERSION.to_owned(),
            sources,
        }
    }

    pub async fn is_ready(&self) -> bool {
        for source in self.sources.values() {
            if source.summary().await.snapshot_observed_at_ms.is_some() {
                return true;
            }
        }
        false
    }
}

fn system_now_ms() -> Result<u64, RuntimeError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RuntimeError::InvalidSystemClock)?;
    u64::try_from(elapsed.as_millis()).map_err(|_| RuntimeError::SystemTimeOverflow)
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error(transparent)]
    Classification(#[from] ClassificationError),
    #[error(transparent)]
    StagedSnapshot(#[from] StagedSnapshotError),
    #[error("poll interval must be greater than zero")]
    ZeroPollInterval,
    #[error("at least one source is required")]
    NoSources,
    #[error("configured {configured} sources, exceeding the supported maximum {maximum}")]
    TooManySources { configured: usize, maximum: usize },
    #[error("source {0:?} is configured more than once")]
    DuplicateSource(String),
    #[error(
        "source {source_id:?} uses a {actual_seconds}-second poll interval; expected {expected_seconds} seconds"
    )]
    PollIntervalMismatch {
        source_id: String,
        expected_seconds: u64,
        actual_seconds: u64,
    },
    #[error("snapshot source {actual:?} does not match runtime source {expected:?}")]
    SnapshotSourceMismatch { expected: String, actual: String },
    #[error("classification generation counter overflowed")]
    ClassificationGenerationOverflow,
    #[error(
        "classification report generation {report} does not match publication generation {publication}"
    )]
    ClassificationReportGenerationMismatch { report: u64, publication: u64 },
    #[error(
        "classification publication revision {publication} does not match snapshot revision {snapshot}"
    )]
    ClassificationRevisionMismatch { publication: u64, snapshot: u64 },
    #[error(
        "classification publication revision {publication} does not immediately follow current revision {current}"
    )]
    ClassificationRevisionSequence { current: u64, publication: u64 },
    #[error(
        "retained {classified} classifications for membership containing only {membership} transactions"
    )]
    ClassificationsExceedMembership {
        classified: usize,
        membership: usize,
    },
    #[error("classification lifecycle exists without a current snapshot")]
    ClassificationStateWithoutSnapshot,
    #[error("published domain state exists without its v2 bundle")]
    PublicationStateWithoutBundle,
    #[error("replacement v2 manifest does not describe the retained stage bundle")]
    PublicationManifestStageMismatch,
    #[error("v2 publication worker failed: {0}")]
    PublicationTask(#[from] tokio::task::JoinError),
    #[error("system clock is before the Unix epoch")]
    InvalidSystemClock,
    #[error("system time cannot be represented in milliseconds")]
    SystemTimeOverflow,
}

#[cfg(test)]
mod tests;
