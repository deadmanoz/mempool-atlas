//! Continuous, bounded classification of one disposable mempool snapshot.
//!
//! The node remains the membership authority. One generation owns only the
//! classifications and output scripts that still belong to its exact current
//! witness variants. RPC work happens away from that state and is merged only
//! while the generation is still current.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::bip110::{
    PrevoutFacts, PrevoutSet, RuleId, RuleVerdict, TxEvidence, evaluate_mempool_policy,
};
use bitcoin::consensus::encode::deserialize_hex;
use bitcoin::{OutPoint, ScriptBuf, Transaction, Txid};
use serde::Deserialize;
use serde_json::json;
use thiserror::Error;
use tracing::warn;

use crate::classification_rpc::{
    ClassificationRpcClient, ClassificationRpcError, ClassificationRpcOutcome,
};
use crate::classifiers::{classify_transaction, transaction_structure};
#[cfg(test)]
use crate::model::MempoolEntry;
use crate::model::{
    Bip110Assessment, Bip110RuleDetail, Bip110RuleId, Bip110RuleVerdict, Bip110Status,
    MAX_BIP110_DETAIL_EXEMPLARS_PER_RULE, MempoolSnapshot, TransactionClassification,
    TransactionClassifications, TransactionStructure,
};

const RAW_TRANSACTION_BATCH_SIZE: usize = 256;
const MAX_GETTXOUT_BATCH_SIZE: usize = 512;
const MAX_RPC_LANES: usize = 8;
const MAX_TRANSACTIONS_PER_SLICE: usize = 8_192;
const MAX_BATCH_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_RAW_RESPONSE_BYTES_PER_SLICE: usize = 256 * 1024 * 1024;
const MAX_CONFIRMED_RESPONSE_BYTES_PER_SLICE: usize = 256 * 1024 * 1024;
const MAX_UNIQUE_PREVOUTS_PER_SLICE: usize = 65_536;
const MAX_FACT_ATTEMPTS_PER_SOURCE_PER_WINDOW: u8 = 2;
// Pending fact scripts survive across resolver waves, unlike decoded RPC
// envelopes. Keep that retained working set independently bounded. This is
// the absolute per-source ceiling; a source configured with a smaller
// auxiliary-cache budget uses that smaller value for pending scripts too. An
// individual accepted raw transaction may exceed the fact-count target, but
// its script facts still cannot exceed the effective byte ceiling.
const MAX_PENDING_SCRIPT_BYTES: usize = MAX_CONFIRMED_RESPONSE_BYTES_PER_SLICE;
const MAX_AUXILIARY_CACHE_BYTES: usize = 512 * 1024 * 1024;
const ESTIMATED_JSON_BYTES_PER_RESPONSE: usize = 512;
const MAX_TRANSACTION_HEX_CHARACTERS: usize = 8_000_000;
// Classification is best effort. Scripts above this generous ceiling remain
// operational collection failures and leave their candidates unclassified.
// The ceiling is far above every 4..=42 byte witness program while still
// bounding legacy-script decoding and per-batch response estimates.
const MAX_CONFIRMED_SCRIPT_HEX_CHARACTERS: usize = 64 * 1024;
const ESTIMATED_GETTXOUT_RESPONSE_BYTES: usize =
    MAX_CONFIRMED_SCRIPT_HEX_CHARACTERS + ESTIMATED_JSON_BYTES_PER_RESPONSE;
const RPC_BATCH_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClassificationLimits {
    max_transactions_per_slice: usize,
    rpc_lanes: usize,
    max_auxiliary_cache_bytes: usize,
}

impl ClassificationLimits {
    pub fn new(
        max_transactions_per_slice: usize,
        rpc_lanes: usize,
        max_auxiliary_cache_bytes: usize,
    ) -> Result<Self, ClassificationError> {
        if max_transactions_per_slice == 0
            || max_transactions_per_slice > MAX_TRANSACTIONS_PER_SLICE
            || rpc_lanes == 0
            || rpc_lanes > MAX_RPC_LANES
            || max_auxiliary_cache_bytes == 0
            || max_auxiliary_cache_bytes > MAX_AUXILIARY_CACHE_BYTES
        {
            return Err(ClassificationError::InvalidLimits);
        }
        Ok(Self {
            max_transactions_per_slice,
            rpc_lanes,
            max_auxiliary_cache_bytes,
        })
    }

    fn prevout_rpc_lanes(self) -> usize {
        self.rpc_lanes.div_ceil(2)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResolverLimits {
    max_parent_transactions_per_wave: usize,
    max_confirmed_prevouts_per_wave: usize,
    max_unique_prevouts_per_window: usize,
    max_pending_script_bytes: usize,
}

impl ResolverLimits {
    fn production(max_auxiliary_cache_bytes: usize) -> Self {
        Self {
            max_parent_transactions_per_wave: MAX_TRANSACTIONS_PER_SLICE,
            max_confirmed_prevouts_per_wave: maximum_confirmed_prevouts_per_slice(),
            max_unique_prevouts_per_window: MAX_UNIQUE_PREVOUTS_PER_SLICE,
            max_pending_script_bytes: MAX_PENDING_SCRIPT_BYTES.min(max_auxiliary_cache_bytes),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ClassificationPipeline {
    client: Arc<ClassificationRpcClient>,
    coordinator: Arc<Mutex<ClassificationCoordinator>>,
    limits: ClassificationLimits,
    resolver_limits: ResolverLimits,
}

impl ClassificationPipeline {
    pub fn new(
        url: &str,
        username: String,
        password: String,
        limits: ClassificationLimits,
    ) -> Result<Self, ClassificationError> {
        Ok(Self {
            client: Arc::new(ClassificationRpcClient::new(
                url,
                username,
                password,
                RPC_BATCH_TIMEOUT,
                MAX_BATCH_RESPONSE_BYTES,
            )),
            coordinator: Arc::new(Mutex::new(ClassificationCoordinator::new(
                limits.max_auxiliary_cache_bytes,
            ))),
            limits,
            resolver_limits: ResolverLimits::production(limits.max_auxiliary_cache_bytes),
        })
    }

    #[cfg(test)]
    fn with_test_confirmed_wave_limit(mut self, maximum: usize) -> Self {
        assert!(maximum > 0);
        self.resolver_limits.max_confirmed_prevouts_per_wave = maximum;
        self
    }

    #[cfg(test)]
    fn with_test_pending_script_limit(mut self, maximum: usize) -> Self {
        assert!(maximum > 0);
        self.resolver_limits.max_pending_script_bytes = maximum;
        self
    }

    /// Installs fresh complete membership and carries every exact
    /// classification that survived from the previous generation into the
    /// revision-zero publication handoff.
    pub(crate) fn install_snapshot(
        &self,
        membership: MempoolSnapshot,
    ) -> Result<ClassificationGenerationStart, ClassificationError> {
        self.install_snapshot_before_exposure(membership, || {})
    }

    fn install_snapshot_before_exposure(
        &self,
        membership: MempoolSnapshot,
        before_exposure: impl FnOnce(),
    ) -> Result<ClassificationGenerationStart, ClassificationError> {
        let variants = membership
            .transactions
            .iter()
            .map(|entry| {
                entry
                    .txid
                    .parse::<Txid>()
                    .map(|txid| {
                        (
                            txid,
                            CurrentVariant {
                                wtxid: entry.wtxid.clone(),
                                vsize: entry.vsize,
                            },
                        )
                    })
                    .map_err(|_| ClassificationError::InvalidMembershipTxid(entry.txid.clone()))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let membership = Arc::new(membership);

        let publication = {
            let mut coordinator = self.coordinator();
            let id = coordinator
                .next_generation
                .checked_add(1)
                .ok_or(ClassificationError::GenerationOverflow)?;
            coordinator.next_generation = id;

            let mut work = ClassificationWork::default();
            let mut carried_outputs = Vec::new();
            let mut classifications = TransactionClassifications::new();
            if let Some(previous) = coordinator.current.clone() {
                let previous_work = previous.work();
                for entry in &membership.transactions {
                    if let Some(cached) = previous_work.classifications.get(&entry.wtxid)
                        && cached.classification.txid == entry.txid
                    {
                        work.classifications
                            .insert(entry.wtxid.clone(), cached.clone());
                        classifications
                            .insert(entry.txid.clone(), Arc::clone(&cached.classification));
                    }
                    let Ok(txid) = entry.txid.parse::<Txid>() else {
                        continue;
                    };
                    if let Some(outputs) = previous_work.outputs.get(&txid)
                        && outputs.wtxid == entry.wtxid
                    {
                        carried_outputs.push((txid, outputs.clone()));
                    }
                }
            }
            for (txid, outputs) in carried_outputs {
                insert_outputs(
                    &mut work,
                    &mut coordinator.confirmed_cache,
                    txid,
                    outputs,
                    self.limits.max_auxiliary_cache_bytes,
                );
            }

            let generation = Arc::new(ClassificationGeneration {
                id,
                membership: Arc::clone(&membership),
                variants,
                work: Mutex::new(work),
                driver: tokio::sync::Mutex::new(()),
            });
            let has_eligible_work = {
                let work = generation.work();
                generation.membership.transactions.iter().any(|entry| {
                    work.classifications
                        .get(&entry.wtxid)
                        .is_none_or(|cached| cached.retryable)
                })
            };
            // Build revision zero before making the generation visible. A
            // concurrent classifier may run as soon as `current` changes, but
            // it cannot mutate this already-built installation handoff.
            let publication = ClassificationGenerationStart {
                generation: id,
                revision: 0,
                has_eligible_work,
                membership,
                classifications,
            };
            before_exposure();
            coordinator.current = Some(Arc::clone(&generation));
            publication
        };

        Ok(publication)
    }

    /// Advances one bounded slice from the current generation. Raw acquisition
    /// receives at most two attempts for each witness variant in a generation;
    /// successfully admitted transactions then retain that decoded raw data
    /// while their facts advance through as many bounded waves as needed. A
    /// later membership snapshot makes incomplete variants eligible again.
    pub async fn classify_next(&self) -> Result<ClassificationSliceReport, ClassificationError> {
        let expected_generation = self.current_generation().map(|generation| generation.id);
        self.classify_next_for_generation(expected_generation).await
    }

    pub(crate) async fn classify_next_for_generation(
        &self,
        expected_generation: Option<u64>,
    ) -> Result<ClassificationSliceReport, ClassificationError> {
        let Some(generation) = self.current_generation() else {
            return Ok(ClassificationSliceReport::waiting());
        };
        if expected_generation != Some(generation.id) {
            return Ok(ClassificationSliceReport::stale(generation.id));
        }
        // The generation-local driver is the scheduling seam. RPC batches are
        // internally concurrent, but only one caller may mutate the pending
        // queue and its fair cursors at a time. Membership installation does
        // not take this lock and can supersede an in-flight generation.
        let _driver = generation.driver.lock().await;
        if expected_generation != Some(generation.id) || !self.is_current_generation(&generation) {
            return Ok(ClassificationSliceReport::stale(generation.id));
        }

        let mut advance = ClassificationAdvance::default();
        let mut pending = {
            let coordinator = self.coordinator();
            if coordinator
                .current
                .as_ref()
                .is_none_or(|current| !Arc::ptr_eq(current, &generation))
            {
                return Ok(ClassificationSliceReport::stale(generation.id));
            }
            generation.work().pending.take()
        };

        if pending.is_none() {
            let candidates = {
                let mut work = generation.work();
                let fresh = generation.membership.transactions.iter().filter(|entry| {
                    !work.attempted.contains(&entry.wtxid)
                        && !work.classifications.contains_key(&entry.wtxid)
                });
                let retryable = generation.membership.transactions.iter().filter(|entry| {
                    !work.attempted.contains(&entry.wtxid)
                        && work
                            .classifications
                            .get(&entry.wtxid)
                            .is_some_and(|cached| cached.retryable)
                });
                let candidates = bounded_variants(
                    fresh.chain(retryable).map(|entry| ExpectedVariant {
                        txid: entry.txid.clone(),
                        wtxid: entry.wtxid.clone(),
                        vsize: entry.vsize,
                    }),
                    self.limits.max_transactions_per_slice,
                );
                for candidate in &candidates {
                    work.attempted.insert(candidate.wtxid.clone());
                }
                candidates
            };
            advance.attempted = candidates.len();
            if !candidates.is_empty() {
                let fetched = self
                    .fetch_raw_transactions_concurrently(
                        candidates,
                        self.limits.rpc_lanes,
                        Some(&generation),
                    )
                    .await;
                advance.response_failures += fetched.response_failures;
                advance.systemic_response_failures += fetched.systemic_response_failures;
                advance.missing_responses += fetched.missing_responses;
                advance.all_missing_batches += fetched.all_missing_batches;
                advance.batch_failures += fetched.batch_failures;
                advance.response_bytes += fetched.response_bytes;
                if !self.is_current_generation(&generation) {
                    return Ok(ClassificationSliceReport::stale_after(
                        generation.id,
                        &advance,
                    ));
                }
                let mut retryable_variants = Vec::new();
                let transactions = fetched
                    .values
                    .into_iter()
                    .filter_map(|(expected, value)| match value {
                        RpcValue::Found(transaction) => Some((expected, transaction)),
                        RpcValue::RetryableFailure => {
                            retryable_variants.push(expected.wtxid);
                            None
                        }
                        RpcValue::ExplicitNull | RpcValue::SystemicFailure => None,
                    })
                    .collect::<Vec<_>>();
                if !retryable_variants.is_empty() {
                    let mut work = generation.work();
                    for wtxid in retryable_variants {
                        if work.raw_retry_used.insert(wtxid.clone()) {
                            work.attempted.remove(&wtxid);
                        }
                    }
                }
                if !transactions.is_empty() {
                    let new_pending =
                        PendingSlice::new(transactions, &generation, self.resolver_limits);
                    advance.outputs.extend(
                        new_pending
                            .raw_outputs
                            .iter()
                            .map(|(txid, outputs)| (*txid, outputs.clone())),
                    );
                    pending = Some(new_pending);
                }
            }
        }

        if let Some(mut current) = pending {
            {
                let mut coordinator = self.coordinator();
                if coordinator
                    .current
                    .as_ref()
                    .is_none_or(|candidate| !Arc::ptr_eq(candidate, &generation))
                {
                    return Ok(ClassificationSliceReport::stale_after(
                        generation.id,
                        &advance,
                    ));
                }
                let work = generation.work();
                let (resolved, capacity_deferred) = current.hydrate_from_caches(
                    &generation.variants,
                    &work.outputs,
                    &mut coordinator.confirmed_cache,
                    self.resolver_limits,
                );
                advance.facts_resolved += resolved;
                advance.capacity_deferred += capacity_deferred;
            }

            advance.classifications.extend(current.classify_ready());
            current.activate_waiting(&generation, self.resolver_limits);

            let mut fact_waves_paused = advance.batch_failures > 0
                || advance.systemic_response_failures > 0
                || advance.all_missing_batches > 0;
            let parent_wave = if fact_waves_paused {
                Vec::new()
            } else {
                current.next_parent_wave(&generation, self.resolver_limits)
            };
            if !parent_wave.is_empty() {
                advance.fact_requests += parent_wave.len();
                let selected_parents = parent_wave
                    .iter()
                    .filter_map(|expected| expected.txid.parse::<Txid>().ok())
                    .collect::<BTreeSet<_>>();
                let fetched = self
                    .fetch_raw_transactions_concurrently(
                        parent_wave,
                        self.limits.rpc_lanes,
                        Some(&generation),
                    )
                    .await;
                advance.response_failures += fetched.response_failures;
                advance.systemic_response_failures += fetched.systemic_response_failures;
                advance.missing_responses += fetched.missing_responses;
                advance.all_missing_batches += fetched.all_missing_batches;
                advance.batch_failures += fetched.batch_failures;
                advance.response_bytes += fetched.response_bytes;
                if !self.is_current_generation(&generation) {
                    return Ok(ClassificationSliceReport::stale_after(
                        generation.id,
                        &advance,
                    ));
                }
                fact_waves_paused |= fetched.batch_failures > 0
                    || fetched.systemic_response_failures > 0
                    || fetched.all_missing_batches > 0;
                let mut resolved_parents = BTreeSet::new();
                for (expected, value) in fetched.values {
                    let Ok(parent_txid) = expected.txid.parse::<Txid>() else {
                        continue;
                    };
                    match value {
                        RpcValue::Found(transaction) => {
                            resolved_parents.insert(parent_txid);
                            let (outputs, resolved, capacity_deferred) = current
                                .apply_parent_transaction(
                                    &expected,
                                    &transaction,
                                    self.resolver_limits,
                                );
                            advance.outputs.insert(parent_txid, outputs);
                            advance.facts_resolved += resolved;
                            advance.capacity_deferred += capacity_deferred;
                        }
                        RpcValue::ExplicitNull
                        | RpcValue::RetryableFailure
                        | RpcValue::SystemicFailure => {}
                    }
                }
                if !fact_waves_paused {
                    for parent_txid in selected_parents.difference(&resolved_parents) {
                        current.fall_back_parent_to_gettxout(*parent_txid);
                    }
                }
            }

            let confirmed_wave = if fact_waves_paused {
                Vec::new()
            } else {
                current.next_confirmed_wave(self.resolver_limits)
            };
            if !confirmed_wave.is_empty() {
                advance.fact_requests += confirmed_wave.len();
                let fetched = self
                    .fetch_confirmed_prevouts_concurrently(
                        confirmed_wave,
                        self.limits.prevout_rpc_lanes(),
                        Some(&generation),
                    )
                    .await;
                advance.response_failures += fetched.response_failures;
                advance.systemic_response_failures += fetched.systemic_response_failures;
                advance.missing_responses += fetched.missing_responses;
                advance.all_missing_batches += fetched.all_missing_batches;
                advance.batch_failures += fetched.batch_failures;
                advance.response_bytes += fetched.response_bytes;
                if !self.is_current_generation(&generation) {
                    return Ok(ClassificationSliceReport::stale_after(
                        generation.id,
                        &advance,
                    ));
                }
                for (outpoint, value) in fetched.values {
                    let (script, resolved, missing, capacity_deferred) =
                        current.apply_confirmed_value(outpoint, value, self.resolver_limits);
                    if let Some(script) = script {
                        advance.confirmed_scripts.insert(outpoint, script);
                    }
                    advance.facts_resolved += resolved;
                    advance.facts_missing += missing;
                    advance.capacity_deferred += capacity_deferred;
                }
            }

            advance.classifications.extend(current.classify_ready());
            if !current.has_pending_script_capacity(self.resolver_limits)
                && current.defer_one_capacity_blocked_fact()
            {
                advance.capacity_deferred += 1;
            }
            advance.deferred_candidates += current.defer_exhausted_candidates();
            if !self
                .resolve_capacity_pressure(&mut current, &generation, &mut advance)
                .await
            {
                return Ok(ClassificationSliceReport::stale_after(
                    generation.id,
                    &advance,
                ));
            }
            current.activate_waiting(&generation, self.resolver_limits);
            pending = (!current.is_empty()).then_some(current);
        }

        let systemic_failure = advance.batch_failures > 0
            || advance.systemic_response_failures > 0
            || advance.all_missing_batches > 0;

        let newly_classified = advance.classifications.len();
        let (publication_inputs, classified, remaining, stale) = {
            let mut coordinator = self.coordinator();
            if coordinator
                .current
                .as_ref()
                .is_none_or(|current| !Arc::ptr_eq(current, &generation))
            {
                (None, 0, 0, true)
            } else {
                let mut work = generation.work();
                for (txid, outputs) in &advance.outputs {
                    insert_outputs(
                        &mut work,
                        &mut coordinator.confirmed_cache,
                        *txid,
                        outputs.clone(),
                        self.limits.max_auxiliary_cache_bytes,
                    );
                }
                for (outpoint, script) in &advance.confirmed_scripts {
                    let reserved_bytes = work.output_cache_bytes;
                    coordinator
                        .confirmed_cache
                        .insert(*outpoint, script.clone(), reserved_bytes);
                }
                for cached in &advance.classifications {
                    work.classifications
                        .insert(cached.classification.wtxid.clone(), cached.clone());
                }
                if !advance.classifications.is_empty() {
                    work.revision = work
                        .revision
                        .checked_add(1)
                        .ok_or(ClassificationError::GenerationOverflow)?;
                }
                if let Some(mut pending) = pending {
                    let (resolved, capacity_deferred) = pending.hydrate_from_caches(
                        &generation.variants,
                        &work.outputs,
                        &mut coordinator.confirmed_cache,
                        self.resolver_limits,
                    );
                    advance.facts_resolved += resolved;
                    advance.capacity_deferred += capacity_deferred;
                    work.pending = Some(pending);
                }
                let classified = work.classifications.len();
                let unstarted = generation
                    .membership
                    .transactions
                    .iter()
                    .filter(|entry| {
                        !work.attempted.contains(&entry.wtxid)
                            && work
                                .classifications
                                .get(&entry.wtxid)
                                .is_none_or(|cached| cached.retryable)
                    })
                    .count();
                let remaining = unstarted.saturating_add(
                    work.pending
                        .as_ref()
                        .map_or(0, PendingSlice::candidate_count),
                );
                let publication_revision =
                    (!advance.classifications.is_empty()).then_some(work.revision);
                (publication_revision, classified, remaining, false)
            }
        };

        if stale {
            return Ok(ClassificationSliceReport::stale_after(
                generation.id,
                &advance,
            ));
        }
        let publication = if let Some(revision) = publication_inputs {
            Some(ClassificationRevisionDelta {
                generation: generation.id,
                revision,
                changed: std::mem::take(&mut advance.classifications)
                    .into_iter()
                    .map(|cached| cached.classification)
                    .collect(),
            })
        } else {
            None
        };
        let disposition = if systemic_failure {
            ClassificationDisposition::Paused
        } else if remaining == 0 {
            ClassificationDisposition::Complete
        } else {
            ClassificationDisposition::Continue
        };
        Ok(ClassificationSliceReport {
            generation: generation.id,
            attempted: advance.attempted,
            newly_classified,
            fact_requests: advance.fact_requests,
            facts_resolved: advance.facts_resolved,
            facts_missing: advance.facts_missing,
            capacity_deferred: advance.capacity_deferred,
            deferred_candidates: advance.deferred_candidates,
            response_failures: advance.response_failures,
            systemic_response_failures: advance.systemic_response_failures,
            missing_responses: advance.missing_responses,
            batch_failures: advance.batch_failures,
            response_bytes: advance.response_bytes,
            classified,
            remaining,
            complete: remaining == 0 && !systemic_failure,
            stale: false,
            disposition,
            publication,
        })
    }

    #[cfg(test)]
    fn classify_entries(&self, entries: &mut [MempoolEntry]) -> ClassificationSliceReport {
        let mut membership = entries.to_vec();
        for entry in &mut membership {
            entry.bip110 = None;
        }
        membership.sort_unstable_by(|left, right| left.txid.cmp(&right.txid));
        let membership = MempoolSnapshot::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            1,
            crate::model::ChainTip {
                height: 1,
                hash: "00".repeat(32),
            },
            membership,
        )
        .expect("test membership");
        let initial = self
            .install_snapshot(membership)
            .expect("install membership");
        let mut classifications = initial.classifications;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let report = runtime
            .block_on(self.classify_next())
            .expect("classification slice");
        let revision = report.publication.as_ref().map_or(0, |delta| {
            for classification in &delta.changed {
                classifications.insert(classification.txid.clone(), Arc::clone(classification));
            }
            delta.revision
        });
        let observation = crate::model::MempoolObservation::materialize_retained(
            &initial.membership,
            revision,
            classifications,
        )
        .expect("materialize test observation");
        entries.clone_from_slice(&observation.snapshot.transactions);
        report
    }

    /// Reuses every known fact that becomes admissible as classifications free
    /// pending bytes. Each continuing pass removes at least one candidate, so
    /// the loop is bounded by the 8,192-candidate window. Yielding and checking
    /// generation identity between passes keeps replacement responsive.
    async fn recover_known_facts(
        &self,
        pending: &mut PendingSlice,
        generation: &Arc<ClassificationGeneration>,
        advance: &mut ClassificationAdvance,
    ) -> bool {
        loop {
            if !self.is_current_generation(generation) {
                return false;
            }
            let classified_before = advance.classifications.len();
            {
                let mut coordinator = self.coordinator();
                if coordinator
                    .current
                    .as_ref()
                    .is_none_or(|candidate| !Arc::ptr_eq(candidate, generation))
                {
                    return false;
                }
                let work = generation.work();
                let (resolved, capacity_deferred) = pending.hydrate_from_caches(
                    &generation.variants,
                    &work.outputs,
                    &mut coordinator.confirmed_cache,
                    self.resolver_limits,
                );
                advance.facts_resolved += resolved;
                advance.capacity_deferred += capacity_deferred;
            }
            let (resolved, capacity_deferred) = pending.hydrate_from_transient(
                &advance.outputs,
                &advance.confirmed_scripts,
                self.resolver_limits,
            );
            advance.facts_resolved += resolved;
            advance.capacity_deferred += capacity_deferred;
            advance.classifications.extend(pending.classify_ready());
            if advance.classifications.len() == classified_before {
                return true;
            }
            tokio::task::yield_now().await;
        }
    }

    /// Reclaims enough pending bytes to consume positive facts fetched in this
    /// call before those transient maps disappear. A continuing iteration must
    /// either classify or defer a candidate, so the combined recovery is
    /// bounded by the pending candidate count. Every iteration yields and the
    /// next recovery pass rejects a superseded generation.
    async fn resolve_capacity_pressure(
        &self,
        pending: &mut PendingSlice,
        generation: &Arc<ClassificationGeneration>,
        advance: &mut ClassificationAdvance,
    ) -> bool {
        let mut deferred_once = false;
        loop {
            if !self.recover_known_facts(pending, generation, advance).await {
                return false;
            }
            let transient_fact_survives = pending
                .has_transient_capacity_blocked_fact(&advance.outputs, &advance.confirmed_scripts);
            if deferred_once && !transient_fact_survives {
                return true;
            }
            let deferred = pending.defer_capacity_blocked_candidate();
            if deferred == 0 {
                return true;
            }
            advance.deferred_candidates += deferred;
            deferred_once = true;
            tokio::task::yield_now().await;
        }
    }

    async fn fetch_raw_transactions_concurrently(
        &self,
        expected: Vec<ExpectedVariant>,
        lanes: usize,
        generation: Option<&Arc<ClassificationGeneration>>,
    ) -> ConcurrentFetch<ExpectedVariant, Transaction> {
        let mut remaining = expected.into_iter().peekable();
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..lanes {
            if generation.is_some_and(|generation| !self.is_current_generation(generation)) {
                break;
            }
            let Some(batch) = next_raw_transaction_batch(&mut remaining) else {
                break;
            };
            let pipeline = self.clone();
            tasks.spawn(async move {
                tokio::task::spawn_blocking(move || {
                    let fetched = pipeline.fetch_raw_transactions(&batch);
                    (batch, fetched)
                })
                .await
            });
        }

        let mut fetched = ConcurrentFetch::default();
        let mut stop_scheduling = false;
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok((batch, Ok(values)))) => {
                    let all_missing = batch.len() > 1 && values.missing_responses == batch.len();
                    fetched.response_failures += values.response_failures;
                    fetched.systemic_response_failures += values.systemic_response_failures;
                    fetched.missing_responses += values.missing_responses;
                    fetched.all_missing_batches += usize::from(all_missing);
                    fetched.response_bytes += values.response_bytes;
                    let mut rpc_values = values.values;
                    if all_missing {
                        rpc_values.fill_with(|| RpcValue::SystemicFailure);
                    }
                    fetched.values.extend(batch.into_iter().zip(rpc_values));
                    stop_scheduling |= values.systemic_response_failures > 0 || all_missing;
                }
                Ok(Ok((_, Err(error)))) => {
                    fetched.batch_failures += 1;
                    stop_scheduling = true;
                    warn!(error = %error, "classification raw transaction batch failed");
                }
                Ok(Err(error)) | Err(error) => {
                    fetched.batch_failures += 1;
                    stop_scheduling = true;
                    warn!(error = %error, "classification raw transaction task failed");
                }
            }
            if !stop_scheduling
                && generation.is_none_or(|generation| self.is_current_generation(generation))
                && let Some(batch) = next_raw_transaction_batch(&mut remaining)
            {
                let pipeline = self.clone();
                tasks.spawn(async move {
                    tokio::task::spawn_blocking(move || {
                        let fetched = pipeline.fetch_raw_transactions(&batch);
                        (batch, fetched)
                    })
                    .await
                });
            }
            debug_assert!(tasks.len() <= lanes);
        }
        fetched
    }

    async fn fetch_confirmed_prevouts_concurrently(
        &self,
        outpoints: Vec<OutPoint>,
        lanes: usize,
        generation: Option<&Arc<ClassificationGeneration>>,
    ) -> ConcurrentFetch<OutPoint, ScriptBuf> {
        let mut remaining = outpoints.into_iter();
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..lanes {
            if generation.is_some_and(|generation| !self.is_current_generation(generation)) {
                break;
            }
            let Some(batch) = next_confirmed_prevout_batch(&mut remaining) else {
                break;
            };
            let pipeline = self.clone();
            tasks.spawn(async move {
                tokio::task::spawn_blocking(move || {
                    let fetched = pipeline.fetch_confirmed_prevouts(&batch);
                    (batch, fetched)
                })
                .await
            });
        }

        let mut fetched = ConcurrentFetch::default();
        let mut stop_scheduling = false;
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok((batch, Ok(values)))) => {
                    let all_missing = batch.len() > 1 && values.missing_responses == batch.len();
                    fetched.response_failures += values.response_failures;
                    fetched.systemic_response_failures += values.systemic_response_failures;
                    fetched.missing_responses += values.missing_responses;
                    fetched.all_missing_batches += usize::from(all_missing);
                    fetched.response_bytes += values.response_bytes;
                    let mut rpc_values = values.values;
                    if all_missing {
                        rpc_values.fill_with(|| RpcValue::SystemicFailure);
                    }
                    fetched.values.extend(batch.into_iter().zip(rpc_values));
                    stop_scheduling |= values.systemic_response_failures > 0 || all_missing;
                }
                Ok(Ok((_, Err(error)))) => {
                    fetched.batch_failures += 1;
                    stop_scheduling = true;
                    warn!(error = %error, "classification confirmed prevout batch failed");
                }
                Ok(Err(error)) | Err(error) => {
                    fetched.batch_failures += 1;
                    stop_scheduling = true;
                    warn!(error = %error, "classification confirmed prevout task failed");
                }
            }
            if !stop_scheduling
                && generation.is_none_or(|generation| self.is_current_generation(generation))
                && let Some(batch) = next_confirmed_prevout_batch(&mut remaining)
            {
                let pipeline = self.clone();
                tasks.spawn(async move {
                    tokio::task::spawn_blocking(move || {
                        let fetched = pipeline.fetch_confirmed_prevouts(&batch);
                        (batch, fetched)
                    })
                    .await
                });
            }
            debug_assert!(tasks.len() <= lanes);
        }
        fetched
    }

    fn fetch_raw_transactions(
        &self,
        expected: &[ExpectedVariant],
    ) -> Result<BatchFetch<Transaction>, ClassificationError> {
        let estimated_response_bytes = estimated_raw_batch_response_bytes(expected);
        if estimated_response_bytes > MAX_BATCH_RESPONSE_BYTES {
            return Err(ClassificationError::BatchResponseEstimateTooLarge {
                estimated: estimated_response_bytes,
                maximum: MAX_BATCH_RESPONSE_BYTES,
            });
        }
        let parameters = expected
            .iter()
            .map(|variant| json!([variant.txid, 0]))
            .collect::<Vec<_>>();
        let batch = self
            .client
            .send_batch("getrawtransaction", &parameters)
            .map_err(map_classification_rpc_error)?;
        let response_bytes = batch.response_bytes;

        let mut response_failures = 0;
        let mut systemic_response_failures = 0;
        let mut missing_responses = 0;
        let values = expected
            .iter()
            .zip(batch.responses)
            .map(|(expected, response)| {
                let Some(response) = response else {
                    response_failures += 1;
                    missing_responses += 1;
                    return RpcValue::RetryableFailure;
                };
                let transaction = match response.outcome() {
                    ClassificationRpcOutcome::ProtocolFailure
                    | ClassificationRpcOutcome::RpcError(-32700..=-32600 | -28) => {
                        systemic_response_failures += 1;
                        response_failures += 1;
                        return RpcValue::SystemicFailure;
                    }
                    ClassificationRpcOutcome::Value(result) => (|| {
                        if result.get().len() > MAX_TRANSACTION_HEX_CHARACTERS + 2 {
                            return Err(());
                        }
                        let hex = serde_json::from_str::<&str>(result.get()).map_err(|_| ())?;
                        if hex.len() > MAX_TRANSACTION_HEX_CHARACTERS {
                            return Err(());
                        }
                        deserialize_hex::<Transaction>(hex).map_err(|_| ())
                    })(),
                    ClassificationRpcOutcome::Null
                    | ClassificationRpcOutcome::RpcError(_)
                    | ClassificationRpcOutcome::Malformed => Err(()),
                }
                .and_then(|transaction| {
                    if transaction.compute_txid().to_string() != expected.txid
                        || transaction.compute_wtxid().to_string() != expected.wtxid
                    {
                        return Err(());
                    }
                    // A coinbase has exactly one null prevout. Any other raw
                    // shape containing a null prevout cannot resolve the
                    // per-input facts required by the classifiers and must
                    // remain an unavailable assessment rather than reaching
                    // the terminal-candidate path with a missing fact.
                    if !transaction.is_coinbase()
                        && transaction
                            .input
                            .iter()
                            .any(|input| input.previous_output.is_null())
                    {
                        return Err(());
                    }
                    // Reject transactions whose structural facts cannot be
                    // represented exactly, so every admitted candidate can
                    // later derive its structure infallibly.
                    transaction_structure(&transaction).map_err(|_| ())?;
                    Ok(transaction)
                });
                match transaction {
                    Ok(transaction) => RpcValue::Found(transaction),
                    Err(()) => {
                        response_failures += 1;
                        RpcValue::RetryableFailure
                    }
                }
            })
            .collect();
        Ok(BatchFetch {
            values,
            response_failures,
            systemic_response_failures,
            missing_responses,
            response_bytes,
        })
    }

    fn fetch_confirmed_prevouts(
        &self,
        outpoints: &[OutPoint],
    ) -> Result<BatchFetch<ScriptBuf>, ClassificationError> {
        let estimated_response_bytes = estimated_confirmed_batch_response_bytes(outpoints.len());
        if outpoints.len() > MAX_GETTXOUT_BATCH_SIZE
            || estimated_response_bytes > MAX_BATCH_RESPONSE_BYTES
        {
            return Err(ClassificationError::BatchResponseEstimateTooLarge {
                estimated: estimated_response_bytes,
                maximum: MAX_BATCH_RESPONSE_BYTES,
            });
        }
        let parameters = outpoints
            .iter()
            .map(|outpoint| json!([outpoint.txid.to_string(), outpoint.vout, false]))
            .collect::<Vec<_>>();
        let batch = self
            .client
            .send_batch("gettxout", &parameters)
            .map_err(map_classification_rpc_error)?;
        let response_bytes = batch.response_bytes;

        let mut response_failures = 0;
        let mut systemic_response_failures = 0;
        let mut missing_responses = 0;
        let values = batch
            .responses
            .into_iter()
            .map(|response| {
                let Some(response) = response else {
                    response_failures += 1;
                    missing_responses += 1;
                    return RpcValue::RetryableFailure;
                };
                let result = match response.outcome() {
                    ClassificationRpcOutcome::ProtocolFailure
                    | ClassificationRpcOutcome::RpcError(-32700..=-32600 | -28) => {
                        systemic_response_failures += 1;
                        response_failures += 1;
                        return RpcValue::SystemicFailure;
                    }
                    ClassificationRpcOutcome::Null => return RpcValue::ExplicitNull,
                    ClassificationRpcOutcome::Value(result) => result,
                    ClassificationRpcOutcome::RpcError(_) | ClassificationRpcOutcome::Malformed => {
                        response_failures += 1;
                        return RpcValue::RetryableFailure;
                    }
                };
                if result.get().len() > ESTIMATED_GETTXOUT_RESPONSE_BYTES {
                    response_failures += 1;
                    return RpcValue::RetryableFailure;
                }
                let wire = match serde_json::from_str::<GetTxOutWire<'_>>(result.get()) {
                    Ok(wire) => wire,
                    Err(_) => {
                        response_failures += 1;
                        return RpcValue::RetryableFailure;
                    }
                };
                if wire.script_pub_key.hex.len() > MAX_CONFIRMED_SCRIPT_HEX_CHARACTERS {
                    response_failures += 1;
                    return RpcValue::RetryableFailure;
                }
                match ScriptBuf::from_hex(wire.script_pub_key.hex) {
                    Ok(script) => RpcValue::Found(script),
                    Err(_) => {
                        response_failures += 1;
                        RpcValue::RetryableFailure
                    }
                }
            })
            .collect();
        Ok(BatchFetch {
            values,
            response_failures,
            systemic_response_failures,
            missing_responses,
            response_bytes,
        })
    }

    fn current_generation(&self) -> Option<Arc<ClassificationGeneration>> {
        self.coordinator().current.clone()
    }

    fn is_current_generation(&self, generation: &Arc<ClassificationGeneration>) -> bool {
        self.coordinator()
            .current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, generation))
    }

    fn coordinator(&self) -> MutexGuard<'_, ClassificationCoordinator> {
        self.coordinator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Debug)]
pub struct ClassificationSliceReport {
    pub generation: u64,
    pub attempted: usize,
    pub newly_classified: usize,
    pub fact_requests: usize,
    pub facts_resolved: usize,
    pub facts_missing: usize,
    pub capacity_deferred: usize,
    pub deferred_candidates: usize,
    pub response_failures: usize,
    pub systemic_response_failures: usize,
    pub missing_responses: usize,
    pub batch_failures: usize,
    pub response_bytes: usize,
    pub classified: usize,
    pub remaining: usize,
    pub complete: bool,
    pub stale: bool,
    pub(crate) disposition: ClassificationDisposition,
    pub(crate) publication: Option<ClassificationRevisionDelta>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClassificationDisposition {
    Continue,
    Complete,
    Paused,
    Stale,
}

impl ClassificationSliceReport {
    fn waiting() -> Self {
        Self {
            generation: 0,
            attempted: 0,
            newly_classified: 0,
            fact_requests: 0,
            facts_resolved: 0,
            facts_missing: 0,
            capacity_deferred: 0,
            deferred_candidates: 0,
            response_failures: 0,
            systemic_response_failures: 0,
            missing_responses: 0,
            batch_failures: 0,
            response_bytes: 0,
            classified: 0,
            remaining: 0,
            complete: true,
            stale: false,
            disposition: ClassificationDisposition::Complete,
            publication: None,
        }
    }

    fn stale(generation: u64) -> Self {
        Self {
            generation,
            attempted: 0,
            newly_classified: 0,
            fact_requests: 0,
            facts_resolved: 0,
            facts_missing: 0,
            capacity_deferred: 0,
            deferred_candidates: 0,
            response_failures: 0,
            systemic_response_failures: 0,
            missing_responses: 0,
            batch_failures: 0,
            response_bytes: 0,
            classified: 0,
            remaining: 0,
            complete: false,
            stale: true,
            disposition: ClassificationDisposition::Stale,
            publication: None,
        }
    }

    fn stale_after(generation: u64, advance: &ClassificationAdvance) -> Self {
        Self {
            generation,
            attempted: advance.attempted,
            newly_classified: 0,
            fact_requests: advance.fact_requests,
            facts_resolved: advance.facts_resolved,
            facts_missing: advance.facts_missing,
            capacity_deferred: advance.capacity_deferred,
            deferred_candidates: advance.deferred_candidates,
            response_failures: advance.response_failures,
            systemic_response_failures: advance.systemic_response_failures,
            missing_responses: advance.missing_responses,
            batch_failures: advance.batch_failures,
            response_bytes: advance.response_bytes,
            classified: 0,
            remaining: 0,
            complete: false,
            stale: true,
            disposition: ClassificationDisposition::Stale,
            publication: None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct ClassificationGenerationStart {
    pub(crate) generation: u64,
    pub(crate) revision: u64,
    pub(crate) has_eligible_work: bool,
    pub(crate) membership: Arc<MempoolSnapshot>,
    pub(crate) classifications: TransactionClassifications,
}

#[derive(Debug)]
pub(crate) struct ClassificationRevisionDelta {
    pub(crate) generation: u64,
    pub(crate) revision: u64,
    pub(crate) changed: Vec<Arc<TransactionClassification>>,
}

#[derive(Debug, Error)]
pub enum ClassificationError {
    #[error(
        "classification limits require 1..={MAX_RPC_LANES} RPC lanes, a 1..={MAX_AUXILIARY_CACHE_BYTES}-byte auxiliary cache, and 1..={MAX_TRANSACTIONS_PER_SLICE} transactions per slice"
    )]
    InvalidLimits,
    #[error("classification RPC batch failed: {0}")]
    Batch(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error(
        "classification RPC batch response used {actual} bytes, exceeding the {maximum}-byte limit"
    )]
    BatchResponseTooLarge { actual: usize, maximum: usize },
    #[error(
        "classification RPC batch response is estimated at {estimated} bytes, exceeding the {maximum}-byte limit"
    )]
    BatchResponseEstimateTooLarge { estimated: usize, maximum: usize },
    #[error("membership contained invalid txid {0:?}")]
    InvalidMembershipTxid(String),
    #[error("classification generation counter overflowed")]
    GenerationOverflow,
}

#[derive(Clone, Debug)]
struct ExpectedVariant {
    txid: String,
    wtxid: String,
    vsize: u64,
}

#[derive(Clone, Debug)]
struct CachedClassification {
    classification: Arc<TransactionClassification>,
    retryable: bool,
}

#[derive(Debug)]
struct BatchFetch<T> {
    values: Vec<RpcValue<T>>,
    response_failures: usize,
    systemic_response_failures: usize,
    missing_responses: usize,
    response_bytes: usize,
}

#[derive(Debug)]
enum RpcValue<T> {
    Found(T),
    /// The successful no-value shape that Bitcoin Core emits as JSON `null`.
    /// Item-local, missing-response, and decoding failures remain retryable.
    ExplicitNull,
    RetryableFailure,
    /// A standard RPC failure that must trip the generation circuit breaker
    /// rather than consume a candidate-local retry.
    SystemicFailure,
}

#[derive(Debug)]
struct ConcurrentFetch<K, V> {
    values: Vec<(K, RpcValue<V>)>,
    response_failures: usize,
    systemic_response_failures: usize,
    missing_responses: usize,
    all_missing_batches: usize,
    batch_failures: usize,
    response_bytes: usize,
}

impl<K, V> Default for ConcurrentFetch<K, V> {
    fn default() -> Self {
        Self {
            values: Vec::new(),
            response_failures: 0,
            systemic_response_failures: 0,
            missing_responses: 0,
            all_missing_batches: 0,
            batch_failures: 0,
            response_bytes: 0,
        }
    }
}

#[derive(Debug)]
struct ClassificationCoordinator {
    next_generation: u64,
    current: Option<Arc<ClassificationGeneration>>,
    confirmed_cache: ConfirmedScriptCache,
}

impl ClassificationCoordinator {
    fn new(maximum_cache_bytes: usize) -> Self {
        Self {
            next_generation: 0,
            current: None,
            confirmed_cache: ConfirmedScriptCache::new(maximum_cache_bytes),
        }
    }
}

#[derive(Debug)]
struct ClassificationGeneration {
    id: u64,
    membership: Arc<MempoolSnapshot>,
    variants: BTreeMap<Txid, CurrentVariant>,
    work: Mutex<ClassificationWork>,
    driver: tokio::sync::Mutex<()>,
}

impl ClassificationGeneration {
    fn work(&self) -> MutexGuard<'_, ClassificationWork> {
        self.work
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Clone, Debug)]
struct CurrentVariant {
    wtxid: String,
    vsize: u64,
}

#[derive(Debug, Default)]
struct ClassificationWork {
    classifications: BTreeMap<String, CachedClassification>,
    outputs: BTreeMap<Txid, CachedOutputs>,
    attempted: BTreeSet<String>,
    raw_retry_used: BTreeSet<String>,
    pending: Option<PendingSlice>,
    output_cache_bytes: usize,
    revision: u64,
}

#[derive(Clone, Debug)]
struct CachedOutputs {
    wtxid: String,
    scripts: Arc<Vec<ScriptBuf>>,
    estimated_bytes: usize,
}

impl CachedOutputs {
    fn script(&self, vout: u32) -> Option<ScriptBuf> {
        usize::try_from(vout)
            .ok()
            .and_then(|index| self.scripts.get(index))
            .cloned()
    }
}

#[derive(Clone, Debug)]
struct ConfirmedCacheEntry {
    script: ScriptBuf,
    estimated_bytes: usize,
    stamp: u64,
}

/// Positive-only, weighted LRU cache for immutable confirmed outpoint scripts.
///
/// A positive script is safe across membership generations because its txid
/// commits to the referenced output. Nulls and failures are deliberately not
/// represented. `ClassificationWork::output_cache_bytes` is reserved from this
/// cache's ceiling so both auxiliary cache kinds share one configured bound.
#[derive(Debug)]
struct ConfirmedScriptCache {
    entries: BTreeMap<OutPoint, ConfirmedCacheEntry>,
    lru: BTreeSet<(u64, OutPoint)>,
    estimated_bytes: usize,
    maximum_bytes: usize,
    next_stamp: u64,
}

impl ConfirmedScriptCache {
    fn new(maximum_bytes: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            lru: BTreeSet::new(),
            estimated_bytes: 0,
            maximum_bytes,
            next_stamp: 0,
        }
    }

    fn get(&mut self, outpoint: &OutPoint) -> Option<ScriptBuf> {
        let entry = self.entries.get(outpoint)?;
        let old_stamp = entry.stamp;
        let script = entry.script.clone();
        self.lru.remove(&(old_stamp, *outpoint));
        let stamp = self.allocate_stamp();
        self.entries
            .get_mut(outpoint)
            .expect("confirmed cache entry remains present")
            .stamp = stamp;
        self.lru.insert((stamp, *outpoint));
        Some(script)
    }

    fn insert(&mut self, outpoint: OutPoint, script: ScriptBuf, reserved_bytes: usize) -> bool {
        if let Some(existing) = self.entries.get(&outpoint)
            && existing.script != script
        {
            // An outpoint commits to exactly one script. If two positive
            // responses disagree, trust neither for later generations: remove
            // the prior value and refuse the replacement so the next use must
            // fetch the fact again.
            let existing = self
                .entries
                .remove(&outpoint)
                .expect("conflicting confirmed cache entry was just observed");
            self.lru.remove(&(existing.stamp, outpoint));
            self.estimated_bytes = self
                .estimated_bytes
                .saturating_sub(existing.estimated_bytes);
            warn!(%outpoint, "evicted conflicting confirmed outpoint script");
            return false;
        }

        let available = self.maximum_bytes.saturating_sub(reserved_bytes);
        let estimated_bytes = estimated_script_bytes(&script);
        if estimated_bytes > available {
            return false;
        }

        if let Some(existing) = self.entries.remove(&outpoint) {
            self.lru.remove(&(existing.stamp, outpoint));
            self.estimated_bytes = self
                .estimated_bytes
                .saturating_sub(existing.estimated_bytes);
        }
        self.shrink_to(available.saturating_sub(estimated_bytes));
        let stamp = self.allocate_stamp();
        self.estimated_bytes = self.estimated_bytes.saturating_add(estimated_bytes);
        self.entries.insert(
            outpoint,
            ConfirmedCacheEntry {
                script,
                estimated_bytes,
                stamp,
            },
        );
        self.lru.insert((stamp, outpoint));
        true
    }

    fn reserve_for_outputs(&mut self, output_bytes: usize) {
        self.shrink_to(self.maximum_bytes.saturating_sub(output_bytes));
    }

    fn shrink_to(&mut self, target_bytes: usize) {
        while self.estimated_bytes > target_bytes {
            let Some((stamp, outpoint)) = self.lru.pop_first() else {
                self.entries.clear();
                self.estimated_bytes = 0;
                break;
            };
            let Some(entry) = self.entries.get(&outpoint) else {
                continue;
            };
            if entry.stamp != stamp {
                continue;
            }
            let entry = self
                .entries
                .remove(&outpoint)
                .expect("LRU entry was just observed");
            self.estimated_bytes = self.estimated_bytes.saturating_sub(entry.estimated_bytes);
        }
    }

    fn allocate_stamp(&mut self) -> u64 {
        if self.next_stamp == u64::MAX {
            let ordered = self
                .lru
                .iter()
                .map(|(_, outpoint)| *outpoint)
                .collect::<Vec<_>>();
            self.lru.clear();
            for (index, outpoint) in ordered.into_iter().enumerate() {
                let stamp = u64::try_from(index).unwrap_or(u64::MAX - 1);
                if let Some(entry) = self.entries.get_mut(&outpoint) {
                    entry.stamp = stamp;
                    self.lru.insert((stamp, outpoint));
                }
            }
            self.next_stamp = u64::try_from(self.lru.len()).unwrap_or(u64::MAX - 1);
        }
        let stamp = self.next_stamp;
        self.next_stamp += 1;
        stamp
    }
}

#[derive(Debug)]
struct PreparedCandidate {
    expected: ExpectedVariant,
    transaction: Arc<Transaction>,
}

#[derive(Debug)]
struct PendingCandidate {
    expected: ExpectedVariant,
    transaction: Arc<Transaction>,
    required: Vec<OutPoint>,
    fact_cursor: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FactSource {
    CurrentParent,
    Confirmed,
    /// The membership said this was a mempool parent, but its raw lookup did
    /// not succeed. A positive `gettxout` may prove it just confirmed. A null
    /// fallback is not authoritative because it may still be unconfirmed, so
    /// it remains operationally pending rather than becoming evaluator input.
    ParentFallback,
}

#[derive(Clone, Debug)]
enum FactState {
    Ready(ScriptBuf),
    Awaiting {
        source: FactSource,
        attempts: u8,
    },
    /// A known or planned script could not fit the pending working set. This
    /// is candidate-local: releasing other candidates may make the same shared
    /// fact admissible without poisoning it for the rest of the slice.
    CapacityBlocked(FactSource),
    /// This fact exhausted its bounded attempts. Candidates that need it
    /// remain publicly unclassified and are deferred until the next membership
    /// generation.
    OperationallyDeferred,
    /// Created from the successful no-value shape returned by the confirmed
    /// `gettxout` path.
    ExplicitlyMissing,
}

#[derive(Debug)]
struct PendingSlice {
    queued: VecDeque<PreparedCandidate>,
    active: BTreeMap<String, PendingCandidate>,
    fair_order: VecDeque<String>,
    facts: BTreeMap<OutPoint, FactState>,
    deferred_facts: BTreeSet<OutPoint>,
    raw_outputs: BTreeMap<Txid, CachedOutputs>,
    pending_script_bytes: usize,
}

impl PendingSlice {
    fn new(
        transactions: Vec<(ExpectedVariant, Transaction)>,
        generation: &ClassificationGeneration,
        limits: ResolverLimits,
    ) -> Self {
        // `bounded_variants` has already admitted the raw set under the 256 MiB
        // aggregate estimate. Keeping each decoded transaction here avoids raw
        // refetch while fact waves advance and gives this pending window an
        // explicit raw working-set bound independent from the reuse cache.
        let mut queued = VecDeque::with_capacity(transactions.len());
        let mut raw_outputs = BTreeMap::new();
        for (expected, transaction) in transactions {
            if let Ok(txid) = expected.txid.parse::<Txid>() {
                raw_outputs.insert(txid, cached_outputs(&expected, &transaction));
            }
            queued.push_back(PreparedCandidate {
                expected,
                transaction: Arc::new(transaction),
            });
        }
        let mut pending = Self {
            queued,
            active: BTreeMap::new(),
            fair_order: VecDeque::new(),
            facts: BTreeMap::new(),
            deferred_facts: BTreeSet::new(),
            raw_outputs,
            pending_script_bytes: 0,
        };
        pending.activate_waiting(generation, limits);
        pending
    }

    fn candidate_count(&self) -> usize {
        self.active.len().saturating_add(self.queued.len())
    }

    fn is_empty(&self) -> bool {
        self.active.is_empty() && self.queued.is_empty()
    }

    fn activate_waiting(&mut self, generation: &ClassificationGeneration, limits: ResolverLimits) {
        while let Some(next) = self.queued.front() {
            let unique = next
                .transaction
                .input
                .iter()
                .filter(|input| !input.previous_output.is_null())
                .map(|input| input.previous_output)
                .collect::<BTreeSet<_>>();
            let new_facts = unique
                .iter()
                .filter(|outpoint| !self.facts.contains_key(outpoint))
                .count();
            if !self.active.is_empty()
                && self.facts.len().saturating_add(new_facts)
                    > limits.max_unique_prevouts_per_window
            {
                break;
            }

            // Always admit at least one candidate. A singleton that exceeds
            // the fact-count target remains bounded by the already-enforced
            // single raw-response limit and the retained-script byte limit.
            let next = self.queued.pop_front().expect("front candidate exists");
            let required = unique.iter().copied().collect::<Vec<_>>();
            for outpoint in unique {
                let initial = if self.deferred_facts.contains(&outpoint) {
                    FactState::OperationallyDeferred
                } else {
                    FactState::Awaiting {
                        source: if generation.variants.contains_key(&outpoint.txid) {
                            FactSource::CurrentParent
                        } else {
                            FactSource::Confirmed
                        },
                        attempts: 0,
                    }
                };
                self.facts.entry(outpoint).or_insert(initial);
            }
            let key = next.expected.wtxid.clone();
            self.fair_order.push_back(key.clone());
            self.active.insert(
                key,
                PendingCandidate {
                    expected: next.expected,
                    transaction: next.transaction,
                    required,
                    fact_cursor: 0,
                },
            );
        }
        self.recount_script_bytes();
    }

    fn hydrate_from_caches(
        &mut self,
        variants: &BTreeMap<Txid, CurrentVariant>,
        outputs: &BTreeMap<Txid, CachedOutputs>,
        confirmed: &mut ConfirmedScriptCache,
        limits: ResolverLimits,
    ) -> (usize, usize) {
        let waiting = self
            .facts
            .iter()
            .filter_map(|(outpoint, state)| match state {
                FactState::Awaiting { source, .. } => Some((*outpoint, *source)),
                FactState::CapacityBlocked(source) => Some((*outpoint, *source)),
                FactState::Ready(_)
                | FactState::OperationallyDeferred
                | FactState::ExplicitlyMissing => None,
            })
            .collect::<Vec<_>>();
        let mut resolved = 0;
        let mut capacity_deferred = 0;
        for (outpoint, source) in waiting {
            let script = match source {
                FactSource::CurrentParent => self
                    .raw_outputs
                    .get(&outpoint.txid)
                    .and_then(|cached| cached.script(outpoint.vout))
                    .or_else(|| {
                        let expected = variants.get(&outpoint.txid)?;
                        outputs
                            .get(&outpoint.txid)
                            .filter(|cached| cached.wtxid == expected.wtxid)
                            .and_then(|cached| cached.script(outpoint.vout))
                    }),
                FactSource::Confirmed | FactSource::ParentFallback => confirmed.get(&outpoint),
            };
            if let Some(script) = script {
                if self.store_script(outpoint, script, limits.max_pending_script_bytes) {
                    resolved += 1;
                } else {
                    self.mark_capacity_blocked(outpoint);
                    capacity_deferred += 1;
                }
            }
        }
        (resolved, capacity_deferred)
    }

    fn hydrate_from_transient(
        &mut self,
        outputs: &BTreeMap<Txid, CachedOutputs>,
        confirmed: &BTreeMap<OutPoint, ScriptBuf>,
        limits: ResolverLimits,
    ) -> (usize, usize) {
        let waiting = self
            .facts
            .iter()
            .filter_map(|(outpoint, state)| match state {
                FactState::Awaiting { source, .. } | FactState::CapacityBlocked(source) => {
                    Some((*outpoint, *source))
                }
                FactState::Ready(_)
                | FactState::OperationallyDeferred
                | FactState::ExplicitlyMissing => None,
            })
            .collect::<Vec<_>>();
        let mut resolved = 0;
        let mut capacity_deferred = 0;
        for (outpoint, source) in waiting {
            let script = match source {
                FactSource::CurrentParent => outputs
                    .get(&outpoint.txid)
                    .and_then(|cached| cached.script(outpoint.vout)),
                FactSource::Confirmed | FactSource::ParentFallback => {
                    confirmed.get(&outpoint).cloned()
                }
            };
            if let Some(script) = script {
                if self.store_script(outpoint, script, limits.max_pending_script_bytes) {
                    resolved += 1;
                } else {
                    self.mark_capacity_blocked(outpoint);
                    capacity_deferred += 1;
                }
            }
        }
        (resolved, capacity_deferred)
    }

    fn next_parent_wave(
        &mut self,
        generation: &ClassificationGeneration,
        limits: ResolverLimits,
    ) -> Vec<ExpectedVariant> {
        if !self.has_pending_script_capacity(limits) {
            return Vec::new();
        }
        let mut selected = Vec::new();
        let mut selected_txids = BTreeSet::new();
        let mut estimated_bytes = 0_usize;
        loop {
            let mut added_this_round = false;
            let candidate_count = self.fair_order.len();
            for _ in 0..candidate_count {
                let Some(key) = self.fair_order.pop_front() else {
                    break;
                };
                self.fair_order.push_back(key.clone());
                let Some(candidate) = self.active.get_mut(&key) else {
                    continue;
                };
                let selected_parent =
                    next_candidate_fact(candidate, &self.facts, |outpoint, state| {
                        matches!(
                            state,
                            FactState::Awaiting {
                                source: FactSource::CurrentParent,
                                attempts
                            } if *attempts < MAX_FACT_ATTEMPTS_PER_SOURCE_PER_WINDOW
                        ) && !selected_txids.contains(&outpoint.txid)
                    });
                let Some(txid) = selected_parent.map(|outpoint| outpoint.txid) else {
                    continue;
                };
                let Some(parent) = generation.variants.get(&txid) else {
                    continue;
                };
                let expected = ExpectedVariant {
                    txid: txid.to_string(),
                    wtxid: parent.wtxid.clone(),
                    vsize: parent.vsize,
                };
                let next_bytes = estimated_raw_response_bytes(&expected);
                if !selected.is_empty()
                    && estimated_bytes.saturating_add(next_bytes) > MAX_RAW_RESPONSE_BYTES_PER_SLICE
                {
                    continue;
                }
                estimated_bytes = estimated_bytes.saturating_add(next_bytes);
                self.record_parent_attempt(txid);
                selected_txids.insert(txid);
                selected.push(expected);
                added_this_round = true;
                if selected.len() >= limits.max_parent_transactions_per_wave {
                    return selected;
                }
            }
            if !added_this_round {
                break;
            }
        }
        selected
    }

    fn next_confirmed_wave(&mut self, limits: ResolverLimits) -> Vec<OutPoint> {
        let available = limits
            .max_pending_script_bytes
            .saturating_sub(self.pending_script_bytes);
        let maximum = limits
            .max_confirmed_prevouts_per_wave
            .min(available / ESTIMATED_GETTXOUT_RESPONSE_BYTES);
        let maximum = if available > 0 { maximum.max(1) } else { 0 };
        if maximum == 0 {
            return Vec::new();
        }
        let mut selected = Vec::new();
        let mut selected_outpoints = BTreeSet::new();
        loop {
            let mut added_this_round = false;
            let candidate_count = self.fair_order.len();
            for _ in 0..candidate_count {
                let Some(key) = self.fair_order.pop_front() else {
                    break;
                };
                self.fair_order.push_back(key.clone());
                let Some(candidate) = self.active.get_mut(&key) else {
                    continue;
                };
                let selected_outpoint =
                    next_candidate_fact(candidate, &self.facts, |outpoint, state| {
                        matches!(
                            state,
                            FactState::Awaiting {
                                source: FactSource::Confirmed | FactSource::ParentFallback,
                                attempts
                            } if *attempts < MAX_FACT_ATTEMPTS_PER_SOURCE_PER_WINDOW
                        ) && !selected_outpoints.contains(outpoint)
                    });
                let Some(outpoint) = selected_outpoint else {
                    continue;
                };
                self.record_fact_attempt(outpoint);
                selected_outpoints.insert(outpoint);
                selected.push(outpoint);
                added_this_round = true;
                if selected.len() >= maximum {
                    return selected;
                }
            }
            if !added_this_round {
                break;
            }
        }
        selected
    }

    fn apply_parent_transaction(
        &mut self,
        expected: &ExpectedVariant,
        transaction: &Transaction,
        limits: ResolverLimits,
    ) -> (CachedOutputs, usize, usize) {
        let outputs = cached_outputs(expected, transaction);
        let Ok(txid) = expected.txid.parse::<Txid>() else {
            return (outputs, 0, 0);
        };
        let outpoints = self
            .facts
            .iter()
            .filter_map(|(outpoint, state)| {
                (outpoint.txid == txid
                    && matches!(
                        state,
                        FactState::Awaiting {
                            source: FactSource::CurrentParent,
                            ..
                        }
                    ))
                .then_some(*outpoint)
            })
            .collect::<Vec<_>>();
        let mut resolved = 0;
        let mut capacity_deferred = 0;
        for outpoint in outpoints {
            let Some(script) = outputs.script(outpoint.vout) else {
                self.defer_if_attempts_exhausted(outpoint);
                continue;
            };
            if self.store_script(outpoint, script, limits.max_pending_script_bytes) {
                resolved += 1;
            } else {
                self.mark_capacity_blocked(outpoint);
                capacity_deferred += 1;
            }
        }
        (outputs, resolved, capacity_deferred)
    }

    fn fall_back_parent_to_gettxout(&mut self, parent_txid: Txid) {
        for (outpoint, state) in &mut self.facts {
            if outpoint.txid == parent_txid
                && matches!(
                    state,
                    FactState::Awaiting {
                        source: FactSource::CurrentParent,
                        ..
                    }
                )
            {
                *state = FactState::Awaiting {
                    source: FactSource::ParentFallback,
                    attempts: 0,
                };
            }
        }
    }

    fn apply_confirmed_value(
        &mut self,
        outpoint: OutPoint,
        value: RpcValue<ScriptBuf>,
        limits: ResolverLimits,
    ) -> (Option<ScriptBuf>, usize, usize, usize) {
        match value {
            RpcValue::Found(script) => {
                if self.store_script(outpoint, script.clone(), limits.max_pending_script_bytes) {
                    (Some(script), 1, 0, 0)
                } else {
                    // Positive immutable facts may still enter the long-lived
                    // cache. This candidate remains unclassified until pending
                    // admission succeeds, and will not refetch meanwhile.
                    self.mark_capacity_blocked(outpoint);
                    (Some(script), 0, 0, 1)
                }
            }
            RpcValue::ExplicitNull => match self.facts.get(&outpoint) {
                Some(FactState::Awaiting {
                    source: FactSource::Confirmed,
                    ..
                }) => {
                    self.facts.insert(outpoint, FactState::ExplicitlyMissing);
                    (None, 0, 1, 0)
                }
                Some(FactState::Awaiting {
                    source: FactSource::ParentFallback,
                    ..
                }) => {
                    self.defer_if_attempts_exhausted(outpoint);
                    (None, 0, 0, 0)
                }
                Some(FactState::Awaiting {
                    source: FactSource::CurrentParent,
                    ..
                })
                | Some(FactState::CapacityBlocked(_))
                | Some(FactState::OperationallyDeferred)
                | Some(FactState::Ready(_) | FactState::ExplicitlyMissing)
                | None => (None, 0, 0, 0),
            },
            RpcValue::RetryableFailure => {
                self.defer_if_attempts_exhausted(outpoint);
                (None, 0, 0, 0)
            }
            RpcValue::SystemicFailure => (None, 0, 0, 0),
        }
    }

    fn record_fact_attempt(&mut self, outpoint: OutPoint) {
        if let Some(FactState::Awaiting { attempts, .. }) = self.facts.get_mut(&outpoint) {
            *attempts = attempts.saturating_add(1);
        }
    }

    fn record_parent_attempt(&mut self, parent_txid: Txid) {
        for (outpoint, state) in &mut self.facts {
            if outpoint.txid == parent_txid
                && let FactState::Awaiting {
                    source: FactSource::CurrentParent,
                    attempts,
                } = state
            {
                *attempts = attempts.saturating_add(1);
            }
        }
    }

    fn defer_if_attempts_exhausted(&mut self, outpoint: OutPoint) {
        if matches!(
            self.facts.get(&outpoint),
            Some(FactState::Awaiting { attempts, .. })
                if *attempts >= MAX_FACT_ATTEMPTS_PER_SOURCE_PER_WINDOW
        ) {
            self.defer_fact(outpoint);
        }
    }

    fn defer_fact(&mut self, outpoint: OutPoint) {
        self.deferred_facts.insert(outpoint);
        if let Some(state) = self.facts.get_mut(&outpoint) {
            *state = FactState::OperationallyDeferred;
        }
    }

    fn mark_capacity_blocked(&mut self, outpoint: OutPoint) {
        let source = match self.facts.get(&outpoint) {
            Some(FactState::Awaiting { source, .. }) | Some(FactState::CapacityBlocked(source)) => {
                *source
            }
            Some(
                FactState::Ready(_)
                | FactState::OperationallyDeferred
                | FactState::ExplicitlyMissing,
            )
            | None => return,
        };
        self.facts
            .insert(outpoint, FactState::CapacityBlocked(source));
    }

    fn has_pending_script_capacity(&self, limits: ResolverLimits) -> bool {
        self.pending_script_bytes < limits.max_pending_script_bytes
    }

    fn defer_one_capacity_blocked_fact(&mut self) -> bool {
        let outpoint = self.fair_order.iter().find_map(|key| {
            self.active.get(key).and_then(|candidate| {
                candidate.required.iter().find_map(|outpoint| {
                    matches!(self.facts.get(outpoint), Some(FactState::Awaiting { .. }))
                        .then_some(*outpoint)
                })
            })
        });
        if let Some(outpoint) = outpoint {
            self.mark_capacity_blocked(outpoint);
            true
        } else {
            false
        }
    }

    /// Removes only candidates made unresolvable by an exhausted fact. Raw
    /// candidates and untouched facts elsewhere in the pending slice continue
    /// fairly; shared exhausted outpoints fan out without repeated RPC work.
    fn defer_exhausted_candidates(&mut self) -> usize {
        let doomed = self
            .active
            .iter()
            .filter_map(|(key, candidate)| {
                candidate
                    .required
                    .iter()
                    .any(|outpoint| {
                        matches!(
                            self.facts.get(outpoint),
                            Some(FactState::OperationallyDeferred)
                        )
                    })
                    .then_some(key.clone())
            })
            .collect::<Vec<_>>();
        if doomed.is_empty() {
            return 0;
        }
        for key in &doomed {
            self.active.remove(key);
        }
        let active = &self.active;
        self.fair_order
            .retain(|candidate_key| active.contains_key(candidate_key));
        self.drop_unreferenced_facts();
        doomed.len()
    }

    fn has_transient_capacity_blocked_fact(
        &self,
        outputs: &BTreeMap<Txid, CachedOutputs>,
        confirmed: &BTreeMap<OutPoint, ScriptBuf>,
    ) -> bool {
        self.facts.iter().any(|(outpoint, state)| match state {
            FactState::CapacityBlocked(FactSource::CurrentParent) => {
                self.raw_outputs
                    .get(&outpoint.txid)
                    .and_then(|cached| cached.script(outpoint.vout))
                    .is_none()
                    && outputs
                        .get(&outpoint.txid)
                        .and_then(|cached| cached.script(outpoint.vout))
                        .is_some()
            }
            FactState::CapacityBlocked(FactSource::Confirmed | FactSource::ParentFallback) => {
                confirmed.contains_key(outpoint)
            }
            FactState::Ready(_)
            | FactState::Awaiting { .. }
            | FactState::OperationallyDeferred
            | FactState::ExplicitlyMissing => false,
        })
    }

    fn defer_capacity_blocked_candidate(&mut self) -> usize {
        let reference_counts = self
            .active
            .values()
            .flat_map(|candidate| candidate.required.iter().copied())
            .fold(
                BTreeMap::<OutPoint, usize>::new(),
                |mut counts, outpoint| {
                    *counts.entry(outpoint).or_default() += 1;
                    counts
                },
            );
        let mut doomed = None;
        let mut most_freed_bytes = 0;
        for key in &self.fair_order {
            let Some(candidate) = self.active.get(key) else {
                continue;
            };
            if !candidate.required.iter().any(|outpoint| {
                matches!(
                    self.facts.get(outpoint),
                    Some(FactState::CapacityBlocked(_))
                )
            }) {
                continue;
            }
            let freed_bytes = candidate
                .required
                .iter()
                .filter_map(|outpoint| {
                    let FactState::Ready(script) = self.facts.get(outpoint)? else {
                        return None;
                    };
                    (reference_counts.get(outpoint) == Some(&1))
                        .then(|| estimated_script_bytes(script))
                })
                .fold(0_usize, usize::saturating_add);
            if doomed.is_none() || freed_bytes > most_freed_bytes {
                doomed = Some(key.clone());
                most_freed_bytes = freed_bytes;
            }
        }
        let Some(doomed) = doomed else {
            return 0;
        };
        self.active.remove(&doomed);
        self.fair_order
            .retain(|candidate_key| candidate_key != &doomed);
        self.drop_unreferenced_facts();
        1
    }

    fn classify_ready(&mut self) -> Vec<CachedClassification> {
        let ready = self
            .active
            .iter()
            .filter_map(|(key, candidate)| {
                candidate
                    .required
                    .iter()
                    .all(|outpoint| {
                        matches!(
                            self.facts.get(outpoint),
                            Some(FactState::Ready(_) | FactState::ExplicitlyMissing)
                        )
                    })
                    .then_some(key.clone())
            })
            .collect::<Vec<_>>();
        let mut classifications = Vec::with_capacity(ready.len());
        for key in ready {
            let candidate = self
                .active
                .remove(&key)
                .expect("ready candidate remains active");
            let (prevouts, retryable) = if candidate.transaction.is_coinbase() {
                (PrevoutSet::default(), false)
            } else {
                let facts = candidate
                    .transaction
                    .input
                    .iter()
                    .map(|input| match self.facts.get(&input.previous_output) {
                        Some(FactState::Ready(script)) => {
                            Some(PrevoutFacts::new(script.clone(), None))
                        }
                        Some(FactState::ExplicitlyMissing) => None,
                        Some(
                            FactState::Awaiting { .. }
                            | FactState::CapacityBlocked(_)
                            | FactState::OperationallyDeferred,
                        )
                        | None => {
                            unreachable!("only terminal candidates are classified")
                        }
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
            classification.results = classify_transaction(
                &candidate.transaction,
                &prevouts,
                &classification.assessment,
            );
            classifications.push(CachedClassification {
                classification: Arc::new(classification),
                retryable,
            });
        }
        let active = &self.active;
        self.fair_order
            .retain(|candidate_key| active.contains_key(candidate_key));
        self.drop_unreferenced_facts();
        classifications
    }

    fn store_script(&mut self, outpoint: OutPoint, script: ScriptBuf, maximum: usize) -> bool {
        if matches!(self.facts.get(&outpoint), Some(FactState::Ready(_))) {
            return true;
        }
        if !matches!(
            self.facts.get(&outpoint),
            Some(FactState::Awaiting { .. } | FactState::CapacityBlocked(_))
        ) {
            return false;
        }
        let estimated_bytes = estimated_script_bytes(&script);
        let Some(new_total) = self.pending_script_bytes.checked_add(estimated_bytes) else {
            return false;
        };
        if new_total > maximum {
            return false;
        }
        let Some(state) = self.facts.get_mut(&outpoint) else {
            return false;
        };
        if !matches!(
            state,
            FactState::Awaiting { .. } | FactState::CapacityBlocked(_)
        ) {
            return false;
        }
        *state = FactState::Ready(script);
        self.pending_script_bytes = new_total;
        true
    }

    fn drop_unreferenced_facts(&mut self) {
        let referenced = self
            .active
            .values()
            .flat_map(|candidate| candidate.required.iter().copied())
            .collect::<BTreeSet<_>>();
        self.facts
            .retain(|outpoint, _| referenced.contains(outpoint));
        self.recount_script_bytes();
    }

    fn recount_script_bytes(&mut self) {
        self.pending_script_bytes = self.facts.values().fold(0_usize, |total, state| {
            let FactState::Ready(script) = state else {
                return total;
            };
            total.saturating_add(estimated_script_bytes(script))
        });
    }
}

fn next_candidate_fact(
    candidate: &mut PendingCandidate,
    facts: &BTreeMap<OutPoint, FactState>,
    predicate: impl Fn(&OutPoint, &FactState) -> bool,
) -> Option<OutPoint> {
    if candidate.required.is_empty() {
        return None;
    }
    for offset in 0..candidate.required.len() {
        let index = candidate
            .fact_cursor
            .saturating_add(offset)
            .wrapping_rem(candidate.required.len());
        let outpoint = candidate.required[index];
        let Some(state) = facts.get(&outpoint) else {
            continue;
        };
        if predicate(&outpoint, state) {
            candidate.fact_cursor = (index + 1) % candidate.required.len();
            return Some(outpoint);
        }
    }
    None
}

#[derive(Debug, Default)]
struct ClassificationAdvance {
    attempted: usize,
    fact_requests: usize,
    facts_resolved: usize,
    facts_missing: usize,
    capacity_deferred: usize,
    deferred_candidates: usize,
    classifications: Vec<CachedClassification>,
    outputs: BTreeMap<Txid, CachedOutputs>,
    confirmed_scripts: BTreeMap<OutPoint, ScriptBuf>,
    response_failures: usize,
    systemic_response_failures: usize,
    missing_responses: usize,
    all_missing_batches: usize,
    batch_failures: usize,
    response_bytes: usize,
}

#[derive(Debug, Deserialize)]
struct GetTxOutWire<'a> {
    #[serde(borrow, rename = "scriptPubKey")]
    script_pub_key: ScriptPubKeyWire<'a>,
}

#[derive(Debug, Deserialize)]
struct ScriptPubKeyWire<'a> {
    hex: &'a str,
}

fn cached_outputs(expected: &ExpectedVariant, transaction: &Transaction) -> CachedOutputs {
    let scripts = transaction
        .output
        .iter()
        .map(|output| output.script_pubkey.clone())
        .collect::<Vec<_>>();
    let estimated_bytes = scripts
        .iter()
        .fold(ESTIMATED_JSON_BYTES_PER_RESPONSE, |total, script| {
            total.saturating_add(script.len())
        });
    CachedOutputs {
        wtxid: expected.wtxid.clone(),
        scripts: Arc::new(scripts),
        estimated_bytes,
    }
}

fn insert_outputs(
    work: &mut ClassificationWork,
    confirmed_cache: &mut ConfirmedScriptCache,
    txid: Txid,
    outputs: CachedOutputs,
    maximum: usize,
) {
    let replaced_bytes = work
        .outputs
        .get(&txid)
        .map_or(0, |existing| existing.estimated_bytes);
    let base = work.output_cache_bytes.saturating_sub(replaced_bytes);
    let Some(new_total) = base.checked_add(outputs.estimated_bytes) else {
        return;
    };
    if new_total > maximum {
        return;
    }
    confirmed_cache.reserve_for_outputs(new_total);
    work.output_cache_bytes = new_total;
    work.outputs.insert(txid, outputs);
}

fn estimated_script_bytes(script: &ScriptBuf) -> usize {
    script
        .len()
        .saturating_add(ESTIMATED_JSON_BYTES_PER_RESPONSE)
}

fn bounded_variants(
    variants: impl IntoIterator<Item = ExpectedVariant>,
    maximum_entries: usize,
) -> Vec<ExpectedVariant> {
    let mut bounded = Vec::with_capacity(maximum_entries.min(MAX_TRANSACTIONS_PER_SLICE));
    let mut estimated_bytes = 0_usize;
    for variant in variants {
        if bounded.len() >= maximum_entries {
            break;
        }
        let variant_bytes = estimated_raw_response_bytes(&variant);
        if !bounded.is_empty()
            && estimated_bytes.saturating_add(variant_bytes) > MAX_RAW_RESPONSE_BYTES_PER_SLICE
        {
            break;
        }
        estimated_bytes = estimated_bytes.saturating_add(variant_bytes);
        bounded.push(variant);
    }
    bounded
}

#[cfg(test)]
fn raw_transaction_batches(expected: Vec<ExpectedVariant>) -> Vec<Vec<ExpectedVariant>> {
    let mut remaining = expected.into_iter().peekable();
    std::iter::from_fn(|| next_raw_transaction_batch(&mut remaining)).collect()
}

fn next_raw_transaction_batch(
    remaining: &mut std::iter::Peekable<std::vec::IntoIter<ExpectedVariant>>,
) -> Option<Vec<ExpectedVariant>> {
    let mut batch = Vec::with_capacity(RAW_TRANSACTION_BATCH_SIZE);
    let mut estimated_bytes = 0_usize;
    while batch.len() < RAW_TRANSACTION_BATCH_SIZE {
        let Some(next) = remaining.peek() else {
            break;
        };
        let next_bytes = estimated_raw_response_bytes(next);
        if !batch.is_empty()
            && estimated_bytes.saturating_add(next_bytes) > MAX_BATCH_RESPONSE_BYTES
        {
            break;
        }
        estimated_bytes = estimated_bytes.saturating_add(next_bytes);
        batch.push(remaining.next().expect("peeked raw transaction variant"));
    }
    (!batch.is_empty()).then_some(batch)
}

fn next_confirmed_prevout_batch(
    remaining: &mut std::vec::IntoIter<OutPoint>,
) -> Option<Vec<OutPoint>> {
    let maximum_count = maximum_confirmed_prevouts_per_batch();
    let batch = remaining.by_ref().take(maximum_count).collect::<Vec<_>>();
    (!batch.is_empty()).then_some(batch)
}

fn maximum_confirmed_prevouts_per_batch() -> usize {
    MAX_GETTXOUT_BATCH_SIZE.min(
        MAX_BATCH_RESPONSE_BYTES
            .checked_div(ESTIMATED_GETTXOUT_RESPONSE_BYTES)
            .unwrap_or(0),
    )
}

fn maximum_confirmed_prevouts_per_slice() -> usize {
    MAX_UNIQUE_PREVOUTS_PER_SLICE.min(
        MAX_CONFIRMED_RESPONSE_BYTES_PER_SLICE
            .checked_div(ESTIMATED_GETTXOUT_RESPONSE_BYTES)
            .unwrap_or(0),
    )
}

fn estimated_raw_response_bytes(expected: &ExpectedVariant) -> usize {
    usize::try_from(expected.vsize)
        .unwrap_or(usize::MAX)
        .saturating_mul(8)
        .saturating_add(ESTIMATED_JSON_BYTES_PER_RESPONSE)
}

fn estimated_raw_batch_response_bytes(expected: &[ExpectedVariant]) -> usize {
    expected.iter().fold(0_usize, |total, variant| {
        total.saturating_add(estimated_raw_response_bytes(variant))
    })
}

fn estimated_confirmed_batch_response_bytes(outpoint_count: usize) -> usize {
    outpoint_count.saturating_mul(ESTIMATED_GETTXOUT_RESPONSE_BYTES)
}

fn map_classification_rpc_error(error: ClassificationRpcError) -> ClassificationError {
    match error {
        ClassificationRpcError::ResponseTooLarge { actual, maximum } => {
            ClassificationError::BatchResponseTooLarge { actual, maximum }
        }
        error => ClassificationError::Batch(Box::new(error)),
    }
}

fn classification_from_evidence(
    txid: String,
    wtxid: String,
    structure: TransactionStructure,
    evidence: TxEvidence,
) -> TransactionClassification {
    let primary_rule = evidence
        .primary_violation
        .as_ref()
        .map(|primary| public_rule_id(primary.rule));
    let mut violated_rules = Vec::new();
    let mut unknown_rules = Vec::new();
    let rules = evidence
        .rules
        .into_iter()
        .map(|outcome| {
            let rule = public_rule_id(outcome.rule);
            let number = rule.number();
            let (verdict, mut evidence, mut missing) = match outcome.verdict {
                RuleVerdict::Pass => (Bip110RuleVerdict::Pass, Vec::new(), Vec::new()),
                RuleVerdict::Violate { evidence, missing } => {
                    violated_rules.push(rule);
                    if !missing.is_empty() {
                        unknown_rules.push(rule);
                    }
                    (Bip110RuleVerdict::Violate, evidence, missing)
                }
                RuleVerdict::Unknown { missing } => {
                    unknown_rules.push(rule);
                    (Bip110RuleVerdict::Unknown, Vec::new(), missing)
                }
            };
            let evidence_count = u64::try_from(evidence.len()).unwrap_or(u64::MAX);
            let missing_count = u64::try_from(missing.len()).unwrap_or(u64::MAX);
            evidence.truncate(MAX_BIP110_DETAIL_EXEMPLARS_PER_RULE);
            missing.truncate(MAX_BIP110_DETAIL_EXEMPLARS_PER_RULE);
            Bip110RuleDetail {
                rule,
                number,
                verdict,
                evidence_count,
                evidence,
                missing_count,
                missing,
            }
        })
        .collect::<Vec<_>>();
    let status = if violated_rules.is_empty() {
        if unknown_rules.is_empty() {
            Bip110Status::Compatible
        } else {
            Bip110Status::Indeterminate
        }
    } else {
        Bip110Status::Violating
    };
    TransactionClassification {
        txid,
        wtxid,
        structure,
        results: Vec::new(),
        assessment: Bip110Assessment {
            status,
            primary_rule,
            violated_rules,
            unknown_rules,
        },
        rules,
    }
}

fn public_rule_id(rule: RuleId) -> Bip110RuleId {
    match rule {
        RuleId::OutputSize => Bip110RuleId::OutputSize,
        RuleId::ElementSize => Bip110RuleId::ElementSize,
        RuleId::UndefinedVersion => Bip110RuleId::UndefinedVersion,
        RuleId::TaprootAnnex => Bip110RuleId::TaprootAnnex,
        RuleId::ControlBlockSize => Bip110RuleId::ControlBlockSize,
        RuleId::OpSuccess => Bip110RuleId::OpSuccess,
        RuleId::TapscriptOpIf => Bip110RuleId::TapscriptOpIf,
    }
}

#[cfg(test)]
mod tests;
