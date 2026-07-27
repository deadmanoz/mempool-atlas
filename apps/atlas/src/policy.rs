//! Continuous, bounded enrichment of one disposable mempool snapshot.
//!
//! The node remains the membership authority. One generation owns only the
//! classifications and output scripts that still belong to its exact current
//! witness variants. RPC work happens away from that state and is merged only
//! while the generation is still current.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bitcoin::consensus::encode::deserialize_hex;
use bitcoin::{OutPoint, ScriptBuf, Transaction, Txid};
use rdts_rules::{
    PrevoutFacts, PrevoutSet, RuleId, RuleVerdict, TxEvidence, evaluate_mempool_policy,
};
use serde::Deserialize;
use serde_json::json;
use thiserror::Error;
use tracing::warn;

#[cfg(test)]
use crate::model::MempoolEntry;
use crate::model::{
    Bip110Assessment, Bip110RuleDetail, Bip110RuleId, Bip110RuleVerdict, Bip110Status,
    MAX_BIP110_DETAIL_EXEMPLARS_PER_RULE, MempoolObservation, MempoolSnapshot, ModelError,
    TransactionClassification, TransactionClassifications,
};
use crate::policy_rpc::{PolicyRpcClient, PolicyRpcError, PolicyRpcOutcome};

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
pub struct PolicyLimits {
    max_transactions_per_slice: usize,
    rpc_lanes: usize,
    max_auxiliary_cache_bytes: usize,
}

impl PolicyLimits {
    pub fn new(
        max_transactions_per_slice: usize,
        rpc_lanes: usize,
        max_auxiliary_cache_bytes: usize,
    ) -> Result<Self, PolicyError> {
        if max_transactions_per_slice == 0
            || max_transactions_per_slice > MAX_TRANSACTIONS_PER_SLICE
            || rpc_lanes == 0
            || rpc_lanes > MAX_RPC_LANES
            || max_auxiliary_cache_bytes == 0
            || max_auxiliary_cache_bytes > MAX_AUXILIARY_CACHE_BYTES
        {
            return Err(PolicyError::InvalidLimits);
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
pub struct PolicyEnricher {
    client: Arc<PolicyRpcClient>,
    coordinator: Arc<Mutex<PolicyCoordinator>>,
    limits: PolicyLimits,
    resolver_limits: ResolverLimits,
}

impl PolicyEnricher {
    pub fn new(
        url: &str,
        username: String,
        password: String,
        limits: PolicyLimits,
    ) -> Result<Self, PolicyError> {
        Ok(Self {
            client: Arc::new(PolicyRpcClient::new(
                url,
                username,
                password,
                RPC_BATCH_TIMEOUT,
                MAX_BATCH_RESPONSE_BYTES,
            )),
            coordinator: Arc::new(Mutex::new(PolicyCoordinator::new(
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

    /// Installs fresh complete membership and immediately materializes every
    /// exact classification that survived from the previous generation.
    pub(crate) fn install_snapshot(
        &self,
        membership: MempoolSnapshot,
    ) -> Result<PolicyPublication, PolicyError> {
        self.install_snapshot_before_exposure(membership, || {})
    }

    fn install_snapshot_before_exposure(
        &self,
        membership: MempoolSnapshot,
        before_exposure: impl FnOnce(),
    ) -> Result<PolicyPublication, PolicyError> {
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
                    .map_err(|_| PolicyError::InvalidMembershipTxid(entry.txid.clone()))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let membership = Arc::new(membership);

        let publication = {
            let mut coordinator = self.coordinator();
            let id = coordinator
                .next_generation
                .checked_add(1)
                .ok_or(PolicyError::GenerationOverflow)?;
            coordinator.next_generation = id;

            let mut work = GenerationWork::default();
            let mut carried_outputs = Vec::new();
            if let Some(previous) = coordinator.current.clone() {
                let previous_work = previous.work();
                for entry in &membership.transactions {
                    if let Some(cached) = previous_work.classifications.get(&entry.wtxid)
                        && cached.classification.txid == entry.txid
                    {
                        work.classifications
                            .insert(entry.wtxid.clone(), cached.clone());
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

            let generation = Arc::new(PolicyGeneration {
                id,
                membership,
                variants,
                work: Mutex::new(work),
                driver: tokio::sync::Mutex::new(()),
            });
            // Materialize revision zero before making the generation visible.
            // A concurrent classifier may run as soon as `current` changes,
            // but it cannot mutate this already-built publication.
            let publication = self.publication_for(&generation)?;
            before_exposure();
            coordinator.current = Some(Arc::clone(&generation));
            publication
        };

        Ok(publication)
    }

    /// Advances one bounded slice from the current generation. Raw acquisition
    /// is attempted at most once for each witness variant in a generation;
    /// successfully admitted transactions then retain that decoded raw data
    /// while their facts advance through as many bounded waves as needed. A
    /// later membership snapshot makes incomplete variants eligible again.
    pub async fn classify_next(&self) -> Result<PolicyEnrichmentReport, PolicyError> {
        let expected_generation = self.current_generation().map(|generation| generation.id);
        self.classify_next_for_generation(expected_generation).await
    }

    pub(crate) async fn classify_next_for_generation(
        &self,
        expected_generation: Option<u64>,
    ) -> Result<PolicyEnrichmentReport, PolicyError> {
        let Some(generation) = self.current_generation() else {
            return Ok(PolicyEnrichmentReport::waiting());
        };
        if expected_generation != Some(generation.id) {
            return Ok(PolicyEnrichmentReport::stale(generation.id));
        }
        // The generation-local driver is the scheduling seam. RPC batches are
        // internally concurrent, but only one caller may mutate the pending
        // queue and its fair cursors at a time. Membership installation does
        // not take this lock and can supersede an in-flight generation.
        let _driver = generation.driver.lock().await;
        if expected_generation != Some(generation.id) || !self.is_current_generation(&generation) {
            return Ok(PolicyEnrichmentReport::stale(generation.id));
        }

        let mut advance = PolicyAdvance::default();
        let mut pending = {
            let coordinator = self.coordinator();
            if coordinator
                .current
                .as_ref()
                .is_none_or(|current| !Arc::ptr_eq(current, &generation))
            {
                return Ok(PolicyEnrichmentReport::stale(generation.id));
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
                work.attempted
                    .extend(candidates.iter().map(|candidate| candidate.wtxid.clone()));
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
                    return Ok(PolicyEnrichmentReport::stale_after(generation.id, &advance));
                }
                let transactions = fetched
                    .values
                    .into_iter()
                    .filter_map(|(expected, value)| match value {
                        RpcValue::Found(transaction) => Some((expected, transaction)),
                        RpcValue::ExplicitNull
                        | RpcValue::RetryableFailure
                        | RpcValue::SystemicFailure => None,
                    })
                    .collect::<Vec<_>>();
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
                    return Ok(PolicyEnrichmentReport::stale_after(generation.id, &advance));
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
                    return Ok(PolicyEnrichmentReport::stale_after(generation.id, &advance));
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
                    return Ok(PolicyEnrichmentReport::stale_after(generation.id, &advance));
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
                return Ok(PolicyEnrichmentReport::stale_after(generation.id, &advance));
            }
            current.activate_waiting(&generation, self.resolver_limits);
            pending = (!current.is_empty()).then_some(current);
        }

        let systemic_failure = advance.batch_failures > 0
            || advance.systemic_response_failures > 0
            || advance.all_missing_batches > 0;

        let newly_classified = advance.classifications.len();
        let (publication_inputs, classified, remaining, classifications, stale) = {
            let mut coordinator = self.coordinator();
            if coordinator
                .current
                .as_ref()
                .is_none_or(|current| !Arc::ptr_eq(current, &generation))
            {
                (None, 0, 0, BTreeMap::new(), true)
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
                        .ok_or(PolicyError::GenerationOverflow)?;
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
                let classifications =
                    current_classifications(&generation.membership, &work.classifications);
                let publication_inputs = (!advance.classifications.is_empty())
                    .then(|| (work.revision, classifications.clone()));
                (
                    publication_inputs,
                    classified,
                    remaining,
                    classifications,
                    false,
                )
            }
        };

        if stale {
            return Ok(PolicyEnrichmentReport::stale_after(generation.id, &advance));
        }
        let publication = if let Some((revision, classifications)) = publication_inputs {
            Some(PolicyPublication {
                generation: generation.id,
                revision,
                observation: MempoolObservation::materialize(
                    &generation.membership,
                    revision,
                    classifications,
                )?,
            })
        } else {
            None
        };
        let disposition = if systemic_failure {
            PolicyDrain::Paused
        } else if remaining == 0 {
            PolicyDrain::Complete
        } else {
            PolicyDrain::Continue
        };
        Ok(PolicyEnrichmentReport {
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
            classifications,
        })
    }

    #[cfg(test)]
    fn enrich(&self, entries: &mut [MempoolEntry]) -> PolicyEnrichmentReport {
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
        entries.clone_from_slice(&initial.observation.snapshot.transactions);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let report = runtime
            .block_on(self.classify_next())
            .expect("classification slice");
        if let Some(publication) = &report.publication {
            entries.clone_from_slice(&publication.observation.snapshot.transactions);
        }
        report
    }

    fn publication_for(
        &self,
        generation: &Arc<PolicyGeneration>,
    ) -> Result<PolicyPublication, PolicyError> {
        let (revision, classifications) = {
            let work = generation.work();
            (
                work.revision,
                current_classifications(&generation.membership, &work.classifications),
            )
        };
        Ok(PolicyPublication {
            generation: generation.id,
            revision,
            observation: MempoolObservation::materialize(
                &generation.membership,
                revision,
                classifications,
            )?,
        })
    }

    /// Reuses every known fact that becomes admissible as classifications free
    /// pending bytes. Each continuing pass removes at least one candidate, so
    /// the loop is bounded by the 8,192-candidate window. Yielding and checking
    /// generation identity between passes keeps replacement responsive.
    async fn recover_known_facts(
        &self,
        pending: &mut PendingSlice,
        generation: &Arc<PolicyGeneration>,
        advance: &mut PolicyAdvance,
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
        generation: &Arc<PolicyGeneration>,
        advance: &mut PolicyAdvance,
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
        generation: Option<&Arc<PolicyGeneration>>,
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
            let enricher = self.clone();
            tasks.spawn(async move {
                tokio::task::spawn_blocking(move || {
                    let fetched = enricher.fetch_raw_transactions(&batch);
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
                    warn!(error = %error, "BIP-110 raw transaction batch failed");
                }
                Ok(Err(error)) | Err(error) => {
                    fetched.batch_failures += 1;
                    stop_scheduling = true;
                    warn!(error = %error, "BIP-110 raw transaction task failed");
                }
            }
            if !stop_scheduling
                && generation.is_none_or(|generation| self.is_current_generation(generation))
                && let Some(batch) = next_raw_transaction_batch(&mut remaining)
            {
                let enricher = self.clone();
                tasks.spawn(async move {
                    tokio::task::spawn_blocking(move || {
                        let fetched = enricher.fetch_raw_transactions(&batch);
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
        generation: Option<&Arc<PolicyGeneration>>,
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
            let enricher = self.clone();
            tasks.spawn(async move {
                tokio::task::spawn_blocking(move || {
                    let fetched = enricher.fetch_confirmed_prevouts(&batch);
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
                    warn!(error = %error, "BIP-110 confirmed prevout batch failed");
                }
                Ok(Err(error)) | Err(error) => {
                    fetched.batch_failures += 1;
                    stop_scheduling = true;
                    warn!(error = %error, "BIP-110 confirmed prevout task failed");
                }
            }
            if !stop_scheduling
                && generation.is_none_or(|generation| self.is_current_generation(generation))
                && let Some(batch) = next_confirmed_prevout_batch(&mut remaining)
            {
                let enricher = self.clone();
                tasks.spawn(async move {
                    tokio::task::spawn_blocking(move || {
                        let fetched = enricher.fetch_confirmed_prevouts(&batch);
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
    ) -> Result<BatchFetch<Transaction>, PolicyError> {
        let estimated_response_bytes = estimated_raw_batch_response_bytes(expected);
        if estimated_response_bytes > MAX_BATCH_RESPONSE_BYTES {
            return Err(PolicyError::BatchResponseEstimateTooLarge {
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
            .map_err(map_policy_rpc_error)?;
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
                    PolicyRpcOutcome::ProtocolFailure
                    | PolicyRpcOutcome::RpcError(-32700..=-32600 | -28) => {
                        systemic_response_failures += 1;
                        response_failures += 1;
                        return RpcValue::SystemicFailure;
                    }
                    PolicyRpcOutcome::Value(result) => (|| {
                        if result.get().len() > MAX_TRANSACTION_HEX_CHARACTERS + 2 {
                            return Err(());
                        }
                        let hex = serde_json::from_str::<&str>(result.get()).map_err(|_| ())?;
                        if hex.len() > MAX_TRANSACTION_HEX_CHARACTERS {
                            return Err(());
                        }
                        deserialize_hex::<Transaction>(hex).map_err(|_| ())
                    })(),
                    PolicyRpcOutcome::Null
                    | PolicyRpcOutcome::RpcError(_)
                    | PolicyRpcOutcome::Malformed => Err(()),
                }
                .and_then(|transaction| {
                    if transaction.compute_txid().to_string() != expected.txid
                        || transaction.compute_wtxid().to_string() != expected.wtxid
                    {
                        return Err(());
                    }
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
    ) -> Result<BatchFetch<ScriptBuf>, PolicyError> {
        let estimated_response_bytes = estimated_confirmed_batch_response_bytes(outpoints.len());
        if outpoints.len() > MAX_GETTXOUT_BATCH_SIZE
            || estimated_response_bytes > MAX_BATCH_RESPONSE_BYTES
        {
            return Err(PolicyError::BatchResponseEstimateTooLarge {
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
            .map_err(map_policy_rpc_error)?;
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
                    PolicyRpcOutcome::ProtocolFailure
                    | PolicyRpcOutcome::RpcError(-32700..=-32600 | -28) => {
                        systemic_response_failures += 1;
                        response_failures += 1;
                        return RpcValue::SystemicFailure;
                    }
                    PolicyRpcOutcome::Null => return RpcValue::ExplicitNull,
                    PolicyRpcOutcome::Value(result) => result,
                    PolicyRpcOutcome::RpcError(_) | PolicyRpcOutcome::Malformed => {
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

    fn current_generation(&self) -> Option<Arc<PolicyGeneration>> {
        self.coordinator().current.clone()
    }

    fn is_current_generation(&self, generation: &Arc<PolicyGeneration>) -> bool {
        self.coordinator()
            .current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, generation))
    }

    fn coordinator(&self) -> MutexGuard<'_, PolicyCoordinator> {
        self.coordinator
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Debug)]
pub struct PolicyEnrichmentReport {
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
    pub(crate) disposition: PolicyDrain,
    pub(crate) publication: Option<PolicyPublication>,
    pub classifications: TransactionClassifications,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PolicyDrain {
    Continue,
    Complete,
    Paused,
    Stale,
}

impl PolicyEnrichmentReport {
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
            disposition: PolicyDrain::Complete,
            publication: None,
            classifications: BTreeMap::new(),
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
            disposition: PolicyDrain::Stale,
            publication: None,
            classifications: BTreeMap::new(),
        }
    }

    fn stale_after(generation: u64, advance: &PolicyAdvance) -> Self {
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
            disposition: PolicyDrain::Stale,
            publication: None,
            classifications: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct PolicyPublication {
    pub(crate) generation: u64,
    pub(crate) revision: u64,
    pub(crate) observation: MempoolObservation,
}

#[derive(Debug, Error)]
pub enum PolicyError {
    #[error(
        "policy limits require 1..={MAX_RPC_LANES} RPC lanes, a 1..={MAX_AUXILIARY_CACHE_BYTES}-byte auxiliary cache, and 1..={MAX_TRANSACTIONS_PER_SLICE} transactions per slice"
    )]
    InvalidLimits,
    #[error("policy RPC batch failed: {0}")]
    Batch(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("policy RPC batch response used {actual} bytes, exceeding the {maximum}-byte limit")]
    BatchResponseTooLarge { actual: usize, maximum: usize },
    #[error(
        "policy RPC batch response is estimated at {estimated} bytes, exceeding the {maximum}-byte limit"
    )]
    BatchResponseEstimateTooLarge { estimated: usize, maximum: usize },
    #[error("membership contained invalid txid {0:?}")]
    InvalidMembershipTxid(String),
    #[error("policy generation counter overflowed")]
    GenerationOverflow,
    #[error("policy publication was invalid: {0}")]
    InvalidObservation(#[from] ModelError),
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
struct PolicyCoordinator {
    next_generation: u64,
    current: Option<Arc<PolicyGeneration>>,
    confirmed_cache: ConfirmedScriptCache,
}

impl PolicyCoordinator {
    fn new(maximum_cache_bytes: usize) -> Self {
        Self {
            next_generation: 0,
            current: None,
            confirmed_cache: ConfirmedScriptCache::new(maximum_cache_bytes),
        }
    }
}

#[derive(Debug)]
struct PolicyGeneration {
    id: u64,
    membership: Arc<MempoolSnapshot>,
    variants: BTreeMap<Txid, CurrentVariant>,
    work: Mutex<GenerationWork>,
    driver: tokio::sync::Mutex<()>,
}

impl PolicyGeneration {
    fn work(&self) -> MutexGuard<'_, GenerationWork> {
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
struct GenerationWork {
    classifications: BTreeMap<String, CachedClassification>,
    outputs: BTreeMap<Txid, CachedOutputs>,
    attempted: BTreeSet<String>,
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
/// represented. `GenerationWork::output_cache_bytes` is reserved from this
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
        generation: &PolicyGeneration,
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

    fn activate_waiting(&mut self, generation: &PolicyGeneration, limits: ResolverLimits) {
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
        generation: &PolicyGeneration,
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
            classifications.push(CachedClassification {
                classification: Arc::new(classification_from_evidence(
                    candidate.expected.txid,
                    candidate.expected.wtxid,
                    evidence,
                )),
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
struct PolicyAdvance {
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

fn current_classifications(
    membership: &MempoolSnapshot,
    cache: &BTreeMap<String, CachedClassification>,
) -> TransactionClassifications {
    membership
        .transactions
        .iter()
        .filter_map(|entry| {
            cache
                .get(&entry.wtxid)
                .filter(|cached| cached.classification.txid == entry.txid)
                .map(|cached| (entry.txid.clone(), Arc::clone(&cached.classification)))
        })
        .collect()
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
    work: &mut GenerationWork,
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

fn map_policy_rpc_error(error: PolicyRpcError) -> PolicyError {
    match error {
        PolicyRpcError::ResponseTooLarge { actual, maximum } => {
            PolicyError::BatchResponseTooLarge { actual, maximum }
        }
        error => PolicyError::Batch(Box::new(error)),
    }
}

fn classification_from_evidence(
    txid: String,
    wtxid: String,
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
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc as Shared, Barrier};

    use axum::extract::State;
    use axum::routing::post;
    use axum::{Json, Router};
    use bitcoin::absolute;
    use bitcoin::consensus::encode::serialize_hex;
    use bitcoin::hashes::Hash;
    use bitcoin::transaction::Version;
    use bitcoin::{Amount, Sequence, TxIn, TxOut, Witness};
    use rdts_rules::{EvaluationMode, RuleOutcome};
    use serde_json::Value;
    use tokio::sync::{Mutex as AsyncMutex, Notify, Semaphore};
    use tokio::task::JoinHandle;

    use super::*;

    #[derive(Debug)]
    struct FixtureState {
        fixture: AsyncMutex<RpcFixture>,
        active_requests: AtomicUsize,
        max_active_requests: AtomicUsize,
        gettxout_requests_started: AtomicUsize,
        gettxout_started: Notify,
        release_gettxout: Semaphore,
        block_gettxout: bool,
        delay: Duration,
    }

    #[derive(Debug)]
    struct RpcFixture {
        calls: Vec<(String, Value)>,
        raw_transactions: BTreeMap<String, String>,
        raw_failures: BTreeMap<String, usize>,
        raw_systemic_failures: BTreeMap<String, usize>,
        gettxout_responses: BTreeMap<OutPoint, VecDeque<FixtureResponse>>,
        block_gettxout: bool,
        delay: Duration,
    }

    #[derive(Debug)]
    enum FixtureResponse {
        Result(Value),
        OmittedResult,
        Error,
        SystemicError,
        Missing,
    }

    impl RpcFixture {
        fn new(transactions: impl IntoIterator<Item = Transaction>) -> Self {
            let raw_transactions = transactions
                .into_iter()
                .map(|transaction| {
                    (
                        transaction.compute_txid().to_string(),
                        serialize_hex(&transaction),
                    )
                })
                .collect();
            Self {
                calls: Vec::new(),
                raw_transactions,
                raw_failures: BTreeMap::new(),
                raw_systemic_failures: BTreeMap::new(),
                gettxout_responses: BTreeMap::new(),
                block_gettxout: false,
                delay: Duration::ZERO,
            }
        }

        fn with_raw_failure(mut self, txid: Txid) -> Self {
            self.raw_failures.insert(txid.to_string(), 1);
            self
        }

        fn with_raw_systemic_failure(mut self, txid: Txid) -> Self {
            self.raw_systemic_failures.insert(txid.to_string(), 1);
            self
        }

        fn with_gettxout_results(
            self,
            outpoint: OutPoint,
            results: impl IntoIterator<Item = Value>,
        ) -> Self {
            self.with_gettxout_responses(outpoint, results.into_iter().map(FixtureResponse::Result))
        }

        fn with_gettxout_responses(
            mut self,
            outpoint: OutPoint,
            responses: impl IntoIterator<Item = FixtureResponse>,
        ) -> Self {
            self.gettxout_responses
                .insert(outpoint, responses.into_iter().collect());
            self
        }

        fn with_delay(mut self, delay: Duration) -> Self {
            self.delay = delay;
            self
        }

        fn with_blocked_gettxout(mut self) -> Self {
            self.block_gettxout = true;
            self
        }
    }

    fn transaction() -> Transaction {
        Transaction {
            version: Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: Txid::from_byte_array([7; 32]),
                    vout: 0,
                },
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::from_slice(&[vec![1], vec![2]]),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(500),
                script_pubkey: ScriptBuf::from_hex("00140000000000000000000000000000000000000000")
                    .expect("script"),
            }],
        }
    }

    fn confirmed_outpoint(marker: u8, vout: u32) -> OutPoint {
        OutPoint {
            txid: Txid::from_byte_array([marker; 32]),
            vout,
        }
    }

    fn transaction_spending(outpoints: impl IntoIterator<Item = OutPoint>) -> Transaction {
        let mut transaction = transaction();
        let template = transaction.input[0].clone();
        transaction.input = outpoints
            .into_iter()
            .enumerate()
            .map(|(index, previous_output)| {
                let mut input = template.clone();
                input.previous_output = previous_output;
                input.witness =
                    Witness::from_slice(&[vec![u8::try_from(index).unwrap_or(u8::MAX)], vec![2]]);
                input
            })
            .collect();
        transaction
    }

    fn test_snapshot(observed_at_ms: u64, transactions: &[Transaction]) -> MempoolSnapshot {
        let mut entries = transactions.iter().map(mempool_entry).collect::<Vec<_>>();
        entries.sort_unstable_by(|left, right| left.txid.cmp(&right.txid));
        MempoolSnapshot::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            observed_at_ms,
            crate::model::ChainTip {
                height: observed_at_ms,
                hash: format!("{observed_at_ms:064x}"),
            },
            entries,
        )
        .expect("test membership")
    }

    async fn rpc_fixture(
        State(state): State<Shared<FixtureState>>,
        Json(request): Json<Value>,
    ) -> Json<Value> {
        let requests = request.as_array().expect("batch request");
        let mut responses = Vec::with_capacity(requests.len());
        let active = state.active_requests.fetch_add(1, Ordering::SeqCst) + 1;
        state
            .max_active_requests
            .fetch_max(active, Ordering::SeqCst);
        if state.block_gettxout
            && requests
                .iter()
                .any(|request| request["method"] == "gettxout")
        {
            state
                .gettxout_requests_started
                .fetch_add(1, Ordering::SeqCst);
            state.gettxout_started.notify_waiters();
            state
                .release_gettxout
                .acquire()
                .await
                .expect("blocked gettxout fixture remains open")
                .forget();
        }
        tokio::time::sleep(state.delay).await;
        let mut fixture = state.fixture.lock().await;
        for request in requests {
            let method = request["method"].as_str().expect("method").to_owned();
            let params = request["params"].clone();
            fixture.calls.push((method.clone(), params.clone()));
            let response = match method.as_str() {
                "getrawtransaction" => {
                    let txid = params[0].as_str().expect("raw transaction txid");
                    let should_fail_systemically = fixture
                        .raw_systemic_failures
                        .get_mut(txid)
                        .is_some_and(|remaining| {
                            if *remaining == 0 {
                                false
                            } else {
                                *remaining -= 1;
                                true
                            }
                        });
                    let should_fail = fixture.raw_failures.get_mut(txid).is_some_and(|remaining| {
                        if *remaining == 0 {
                            false
                        } else {
                            *remaining -= 1;
                            true
                        }
                    });
                    if should_fail_systemically {
                        FixtureResponse::SystemicError
                    } else if should_fail {
                        FixtureResponse::Error
                    } else {
                        FixtureResponse::Result(Value::String(
                            fixture
                                .raw_transactions
                                .get(txid)
                                .unwrap_or_else(|| panic!("missing raw transaction {txid}"))
                                .clone(),
                        ))
                    }
                }
                "gettxout" => {
                    let txid = params[0]
                        .as_str()
                        .expect("gettxout txid")
                        .parse::<Txid>()
                        .expect("valid gettxout txid");
                    let vout = params[1]
                        .as_u64()
                        .and_then(|value| u32::try_from(value).ok())
                        .expect("valid gettxout vout");
                    fixture
                        .gettxout_responses
                        .get_mut(&OutPoint { txid, vout })
                        .and_then(VecDeque::pop_front)
                        .unwrap_or_else(|| FixtureResponse::Result(valid_gettxout_result()))
                }
                other => panic!("unexpected RPC method {other}"),
            };
            match response {
                FixtureResponse::Result(result) => responses.push(json!({
                    "jsonrpc": "2.0",
                    "result": result,
                    "id": request["id"]
                })),
                FixtureResponse::OmittedResult => responses.push(json!({
                    "jsonrpc": "2.0",
                    "id": request["id"]
                })),
                FixtureResponse::Error => responses.push(json!({
                    "jsonrpc": "2.0",
                    "error": {
                        "code": -5,
                        "message": "fixture RPC failure"
                    },
                    "id": request["id"]
                })),
                FixtureResponse::SystemicError => responses.push(json!({
                    "jsonrpc": "2.0",
                    "error": {
                        "code": -32601,
                        "message": "fixture method unavailable"
                    },
                    "id": request["id"]
                })),
                FixtureResponse::Missing => {}
            }
        }
        state.active_requests.fetch_sub(1, Ordering::SeqCst);
        Json(Value::Array(responses))
    }

    fn valid_gettxout_result() -> Value {
        json!({
            "scriptPubKey": {
                "hex": "00140000000000000000000000000000000000000000"
            }
        })
    }

    async fn start_rpc_fixture(
        fixture: RpcFixture,
    ) -> (Shared<FixtureState>, SocketAddr, JoinHandle<()>) {
        let delay = fixture.delay;
        let block_gettxout = fixture.block_gettxout;
        let state = Shared::new(FixtureState {
            fixture: AsyncMutex::new(fixture),
            active_requests: AtomicUsize::new(0),
            max_active_requests: AtomicUsize::new(0),
            gettxout_requests_started: AtomicUsize::new(0),
            gettxout_started: Notify::new(),
            release_gettxout: Semaphore::new(0),
            block_gettxout,
            delay,
        });
        let application = Router::new()
            .route("/", post(rpc_fixture))
            .with_state(Shared::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move {
            axum::serve(listener, application)
                .await
                .expect("fixture server");
        });
        (state, address, server)
    }

    async fn start_invalid_policy_fixture() -> (SocketAddr, JoinHandle<()>) {
        let application = Router::new().route("/", post(|| async { "not JSON" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move {
            axum::serve(listener, application)
                .await
                .expect("fixture server");
        });
        (address, server)
    }

    fn test_enricher(address: SocketAddr) -> PolicyEnricher {
        test_enricher_with_limits(
            address,
            PolicyLimits::new(10, 4, 16 * 1024 * 1024).expect("limits"),
        )
    }

    fn test_enricher_with_limits(address: SocketAddr, limits: PolicyLimits) -> PolicyEnricher {
        PolicyEnricher::new(
            &format!("http://{address}/"),
            "atlas".to_owned(),
            "secret".to_owned(),
            limits,
        )
        .expect("enricher")
    }

    fn mempool_entry(transaction: &Transaction) -> MempoolEntry {
        MempoolEntry::new_variant(
            transaction.compute_txid().to_string(),
            transaction.compute_wtxid().to_string(),
            100,
            1_000,
            1,
        )
        .expect("entry")
    }

    async fn enrich_entries(
        enricher: &PolicyEnricher,
        mut entries: Vec<MempoolEntry>,
    ) -> (Vec<MempoolEntry>, PolicyEnrichmentReport) {
        let enricher = enricher.clone();
        tokio::task::spawn_blocking(move || {
            let report = enricher.enrich(&mut entries);
            (entries, report)
        })
        .await
        .expect("enrichment")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn enriches_once_and_reuses_the_wtxid_cache() {
        let transaction = transaction();
        let txid = transaction.compute_txid().to_string();
        let wtxid = transaction.compute_wtxid().to_string();
        let (fixture, address, server) =
            start_rpc_fixture(RpcFixture::new([transaction.clone()])).await;
        let enricher = test_enricher(address);
        let mut entries =
            vec![MempoolEntry::new_variant(txid.clone(), wtxid, 100, 1_000, 1).expect("entry")];
        let first = tokio::task::spawn_blocking({
            let enricher = enricher.clone();
            let mut entries = entries.clone();
            move || {
                let report = enricher.enrich(&mut entries);
                (entries, report)
            }
        })
        .await
        .expect("first enrichment");
        entries = first.0;
        assert_eq!(first.1.newly_classified, 1);
        assert_eq!(
            entries[0]
                .bip110
                .as_ref()
                .map(|assessment| assessment.status),
            Some(Bip110Status::Compatible)
        );
        assert_eq!(first.1.classifications[&txid].rules.len(), 7);

        let second = tokio::task::spawn_blocking({
            let enricher = enricher.clone();
            move || {
                let mut entries = entries;
                let report = enricher.enrich(&mut entries);
                (entries, report)
            }
        })
        .await
        .expect("second enrichment");
        assert_eq!(second.1.attempted, 0);
        assert_eq!(second.1.newly_classified, 0);

        enricher
            .current_generation()
            .expect("current generation")
            .work()
            .classifications
            .get_mut(&transaction.compute_wtxid().to_string())
            .expect("cached classification")
            .retryable = true;
        let third = tokio::task::spawn_blocking({
            let enricher = enricher.clone();
            let mut entries = second.0;
            move || {
                let report = enricher.enrich(&mut entries);
                (entries, report)
            }
        })
        .await
        .expect("retry enrichment");
        server.abort();

        assert_eq!(third.1.attempted, 1);
        assert_eq!(third.1.newly_classified, 1);
        let fixture = fixture.fixture.lock().await;
        assert_eq!(
            fixture
                .calls
                .iter()
                .map(|(method, _)| method.as_str())
                .collect::<Vec<_>>(),
            ["getrawtransaction", "gettxout", "getrawtransaction"]
        );
        assert_eq!(fixture.calls[0].1, json!([txid, 0]));
        assert_eq!(fixture.calls[1].1[2], Value::Bool(false));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn null_prevout_is_typed_retryable_and_replaced_after_retry() {
        let transaction = transaction();
        let txid = transaction.compute_txid().to_string();
        let wtxid = transaction.compute_wtxid().to_string();
        let outpoint = transaction.input[0].previous_output;
        let rpc = RpcFixture::new([transaction.clone()])
            .with_gettxout_results(outpoint, [Value::Null, valid_gettxout_result()]);
        let (fixture, address, server) = start_rpc_fixture(rpc).await;
        let enricher = test_enricher(address);

        let (entries, first) = enrich_entries(&enricher, vec![mempool_entry(&transaction)]).await;
        assert_eq!(first.attempted, 1);
        assert_eq!(first.newly_classified, 1);
        assert_eq!(first.response_failures, 0);
        assert_eq!(
            entries[0]
                .bip110
                .as_ref()
                .map(|assessment| assessment.status),
            Some(Bip110Status::Indeterminate)
        );
        let first_classification = Arc::clone(&first.classifications[&txid]);
        assert_eq!(
            first_classification.assessment.unknown_rules,
            Bip110RuleId::ALL[1..]
        );
        assert_eq!(
            first_classification.rules[0].verdict,
            Bip110RuleVerdict::Pass
        );
        for detail in &first_classification.rules[1..] {
            assert_eq!(detail.verdict, Bip110RuleVerdict::Unknown);
            assert_eq!(detail.missing_count, 1);
            assert!(matches!(
                detail.missing.first(),
                Some(rdts_rules::Missing::ScriptPubKey { input: 0 })
            ));
        }
        {
            let generation = enricher.current_generation().expect("current generation");
            let cache = generation.work();
            assert!(
                cache
                    .classifications
                    .get(&wtxid)
                    .expect("retryable classification")
                    .retryable
            );
        }

        let (entries, second) = enrich_entries(&enricher, entries).await;
        server.abort();

        assert_eq!(second.attempted, 1);
        assert_eq!(second.newly_classified, 1);
        assert_eq!(second.response_failures, 0);
        assert_eq!(
            entries[0]
                .bip110
                .as_ref()
                .map(|assessment| assessment.status),
            Some(Bip110Status::Compatible)
        );
        let second_classification = &second.classifications[&txid];
        assert!(
            !Arc::ptr_eq(&first_classification, second_classification),
            "the complete retry result must replace the cached partial result"
        );
        assert!(second_classification.assessment.unknown_rules.is_empty());
        assert!(
            second_classification
                .rules
                .iter()
                .all(|detail| detail.verdict == Bip110RuleVerdict::Pass)
        );
        {
            let generation = enricher.current_generation().expect("current generation");
            let cache = generation.work();
            assert!(
                !cache
                    .classifications
                    .get(&wtxid)
                    .expect("complete classification")
                    .retryable
            );
        }
        let fixture = fixture.fixture.lock().await;
        assert_eq!(
            fixture
                .calls
                .iter()
                .map(|(method, _)| method.as_str())
                .collect::<Vec<_>>(),
            [
                "getrawtransaction",
                "gettxout",
                "getrawtransaction",
                "gettxout"
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fact_only_waves_continue_without_refetching_candidate_raw() {
        let outpoints = [
            confirmed_outpoint(1, 0),
            confirmed_outpoint(2, 0),
            confirmed_outpoint(3, 0),
        ];
        let transaction = transaction_spending(outpoints);
        let txid = transaction.compute_txid().to_string();
        let (fixture, address, server) =
            start_rpc_fixture(RpcFixture::new([transaction.clone()])).await;
        let enricher = test_enricher(address).with_test_confirmed_wave_limit(1);
        enricher
            .install_snapshot(test_snapshot(1, &[transaction]))
            .expect("membership");

        let first = enricher.classify_next().await.expect("first fact wave");
        assert_eq!(first.attempted, 1);
        assert_eq!(first.fact_requests, 1);
        assert_eq!(first.facts_resolved, 1);
        assert_eq!(first.newly_classified, 0);
        assert_eq!(first.disposition, PolicyDrain::Continue);
        assert!(!first.classifications.contains_key(&txid));

        let second = enricher.classify_next().await.expect("second fact wave");
        assert_eq!(second.attempted, 0);
        assert_eq!(second.fact_requests, 1);
        assert_eq!(second.facts_resolved, 1);
        assert_eq!(second.newly_classified, 0);
        assert_eq!(second.disposition, PolicyDrain::Continue);
        assert!(!second.classifications.contains_key(&txid));

        let third = enricher.classify_next().await.expect("third fact wave");
        assert_eq!(third.attempted, 0);
        assert_eq!(third.fact_requests, 1);
        assert_eq!(third.facts_resolved, 1);
        assert_eq!(third.newly_classified, 1);
        assert_eq!(third.disposition, PolicyDrain::Complete);
        assert_eq!(
            third.classifications[&txid].assessment.status,
            Bip110Status::Compatible
        );
        server.abort();

        let fixture = fixture.fixture.lock().await;
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, _)| method == "getrawtransaction")
                .count(),
            1,
            "fact-only continuation must retain the admitted raw transaction"
        );
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, _)| method == "gettxout")
                .count(),
            3
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn one_confirmed_lookup_fans_out_to_same_generation_candidates() {
        let first = transaction();
        let mut second = transaction();
        second.output[0].value = Amount::from_sat(501);
        assert_eq!(
            first.input[0].previous_output,
            second.input[0].previous_output
        );
        let transactions = [first.clone(), second.clone()];
        let (fixture, address, server) =
            start_rpc_fixture(RpcFixture::new(transactions.clone())).await;
        let enricher = test_enricher(address);
        enricher
            .install_snapshot(test_snapshot(1, &transactions))
            .expect("membership");

        let report = enricher.classify_next().await.expect("shared fact");
        assert_eq!(report.attempted, 2);
        assert_eq!(report.fact_requests, 1);
        assert_eq!(report.facts_resolved, 1);
        assert_eq!(report.newly_classified, 2);
        assert_eq!(report.disposition, PolicyDrain::Complete);
        assert!(report.classifications.values().all(|classification| {
            classification.assessment.status == Bip110Status::Compatible
        }));
        server.abort();

        let fixture = fixture.fixture.lock().await;
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, _)| method == "gettxout")
                .count(),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retryable_first_fact_does_not_starve_later_inputs() {
        let first_outpoint = confirmed_outpoint(4, 0);
        let second_outpoint = confirmed_outpoint(5, 0);
        let transaction = transaction_spending([first_outpoint, second_outpoint]);
        let rpc = RpcFixture::new([transaction.clone()])
            .with_gettxout_results(
                first_outpoint,
                [
                    json!({ "scriptPubKey": { "hex": "zz" } }),
                    valid_gettxout_result(),
                ],
            )
            .with_gettxout_results(second_outpoint, [valid_gettxout_result()]);
        let (fixture, address, server) = start_rpc_fixture(rpc).await;
        let enricher = test_enricher(address).with_test_confirmed_wave_limit(1);
        enricher
            .install_snapshot(test_snapshot(1, &[transaction]))
            .expect("membership");

        let first = enricher
            .classify_next()
            .await
            .expect("malformed first fact");
        assert_eq!(first.response_failures, 1);
        assert_eq!(first.newly_classified, 0);
        assert_eq!(first.disposition, PolicyDrain::Continue);

        let second = enricher.classify_next().await.expect("later fact");
        assert_eq!(second.response_failures, 0);
        assert_eq!(second.facts_resolved, 1);
        assert_eq!(second.newly_classified, 0);
        assert_eq!(second.disposition, PolicyDrain::Continue);

        let third = enricher.classify_next().await.expect("retried first fact");
        assert_eq!(third.response_failures, 0);
        assert_eq!(third.facts_resolved, 1);
        assert_eq!(third.newly_classified, 1);
        assert_eq!(third.disposition, PolicyDrain::Complete);
        server.abort();

        let fixture = fixture.fixture.lock().await;
        let requested = fixture
            .calls
            .iter()
            .filter(|(method, _)| method == "gettxout")
            .map(|(_, params)| OutPoint {
                txid: params[0]
                    .as_str()
                    .expect("txid")
                    .parse()
                    .expect("valid txid"),
                vout: u32::try_from(params[1].as_u64().expect("vout")).expect("valid vout"),
            })
            .collect::<Vec<_>>();
        assert_eq!(requested, [first_outpoint, second_outpoint, first_outpoint]);
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, _)| method == "getrawtransaction")
                .count(),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poison_prefix_candidate_does_not_starve_a_later_slice() {
        let candidates = [
            transaction_spending([confirmed_outpoint(20, 0)]),
            transaction_spending([confirmed_outpoint(21, 0)]),
        ];
        let membership = test_snapshot(1, &candidates);
        let first_txid = membership.transactions[0].txid.clone();
        let second_txid = membership.transactions[1].txid.clone();
        let first = candidates
            .iter()
            .find(|transaction| transaction.compute_txid().to_string() == first_txid)
            .expect("first transaction");
        let poison_outpoint = first.input[0].previous_output;
        let rpc = RpcFixture::new(candidates.clone()).with_gettxout_results(
            poison_outpoint,
            [
                json!({ "scriptPubKey": { "hex": "zz" } }),
                json!({ "scriptPubKey": { "hex": "zz" } }),
            ],
        );
        let (fixture, address, server) = start_rpc_fixture(rpc).await;
        let enricher = test_enricher_with_limits(
            address,
            PolicyLimits::new(1, 1, 16 * 1024 * 1024).expect("limits"),
        );
        enricher.install_snapshot(membership).expect("membership");

        let first_report = enricher
            .classify_next()
            .await
            .expect("first poison attempt");
        assert_eq!(first_report.response_failures, 1);
        assert_eq!(first_report.deferred_candidates, 0);
        assert_eq!(first_report.disposition, PolicyDrain::Continue);

        let second_report = enricher
            .classify_next()
            .await
            .expect("bounded poison retry");
        assert_eq!(second_report.response_failures, 1);
        assert_eq!(second_report.deferred_candidates, 1);
        assert_eq!(second_report.disposition, PolicyDrain::Continue);
        assert!(!second_report.classifications.contains_key(&first_txid));

        let third_report = enricher.classify_next().await.expect("later candidate");
        assert_eq!(third_report.attempted, 1);
        assert_eq!(third_report.newly_classified, 1);
        assert_eq!(third_report.disposition, PolicyDrain::Complete);
        assert!(!third_report.classifications.contains_key(&first_txid));
        assert_eq!(
            third_report.classifications[&second_txid].assessment.status,
            Bip110Status::Compatible
        );
        server.abort();

        let fixture = fixture.fixture.lock().await;
        let fetched_txids = fixture
            .calls
            .iter()
            .filter(|(method, _)| method == "getrawtransaction")
            .map(|(_, params)| params[0].as_str().expect("txid").to_owned())
            .collect::<Vec<_>>();
        assert_eq!(fetched_txids, [first_txid, second_txid]);
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, params)| {
                    method == "gettxout" && params[0] == poison_outpoint.txid.to_string()
                })
                .count(),
            2
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn confirmed_fetch_distinguishes_null_from_omitted_malformed_error_and_missing() {
        let literal_null = confirmed_outpoint(6, 0);
        let omitted_result = confirmed_outpoint(7, 0);
        let malformed = confirmed_outpoint(10, 0);
        let rpc_error = confirmed_outpoint(8, 0);
        let missing_response = confirmed_outpoint(9, 0);
        let rpc = RpcFixture::new([])
            .with_gettxout_responses(literal_null, [FixtureResponse::Result(Value::Null)])
            .with_gettxout_responses(omitted_result, [FixtureResponse::OmittedResult])
            .with_gettxout_responses(
                malformed,
                [FixtureResponse::Result(json!({
                    "scriptPubKey": { "hex": "zz" }
                }))],
            )
            .with_gettxout_responses(rpc_error, [FixtureResponse::Error])
            .with_gettxout_responses(missing_response, [FixtureResponse::Missing]);
        let (fixture, address, server) = start_rpc_fixture(rpc).await;
        let enricher = test_enricher(address);
        let fetch = tokio::task::spawn_blocking(move || {
            enricher.fetch_confirmed_prevouts(&[
                literal_null,
                omitted_result,
                malformed,
                rpc_error,
                missing_response,
            ])
        })
        .await
        .expect("fetch task")
        .expect("bounded response");
        server.abort();

        assert!(matches!(fetch.values[0], RpcValue::ExplicitNull));
        assert!(matches!(fetch.values[1], RpcValue::RetryableFailure));
        assert!(matches!(fetch.values[2], RpcValue::RetryableFailure));
        assert!(matches!(fetch.values[3], RpcValue::RetryableFailure));
        assert!(matches!(fetch.values[4], RpcValue::RetryableFailure));
        assert_eq!(fetch.response_failures, 4);
        assert_eq!(fetch.systemic_response_failures, 0);
        assert_eq!(fetch.missing_responses, 1);
        assert_eq!(fixture.fixture.lock().await.calls.len(), 5);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_response_is_reported_and_retried_without_a_false_missing_fact() {
        let transaction = transaction();
        let txid = transaction.compute_txid().to_string();
        let outpoint = transaction.input[0].previous_output;
        let rpc = RpcFixture::new([transaction.clone()]).with_gettxout_responses(
            outpoint,
            [
                FixtureResponse::Missing,
                FixtureResponse::Result(valid_gettxout_result()),
            ],
        );
        let (_, address, server) = start_rpc_fixture(rpc).await;
        let enricher = test_enricher(address);
        enricher
            .install_snapshot(test_snapshot(1, &[transaction]))
            .expect("membership");

        let first = enricher.classify_next().await.expect("missing response");
        assert_eq!(first.response_failures, 1);
        assert_eq!(first.systemic_response_failures, 0);
        assert_eq!(first.missing_responses, 1);
        assert_eq!(first.facts_missing, 0);
        assert_eq!(first.newly_classified, 0);
        assert_eq!(first.disposition, PolicyDrain::Continue);
        assert!(!first.classifications.contains_key(&txid));

        let second = enricher.classify_next().await.expect("response retry");
        server.abort();

        assert_eq!(second.missing_responses, 0);
        assert_eq!(second.facts_resolved, 1);
        assert_eq!(second.newly_classified, 1);
        assert_eq!(second.disposition, PolicyDrain::Complete);
        assert_eq!(
            second.classifications[&txid].assessment.status,
            Bip110Status::Compatible
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn all_missing_batch_pauses_without_consuming_candidate_local_deferral() {
        let outpoints = [confirmed_outpoint(30, 0), confirmed_outpoint(31, 0)];
        let transaction = transaction_spending(outpoints);
        let malformed = FixtureResponse::Result(json!({
            "scriptPubKey": { "hex": "zz" }
        }));
        let rpc = RpcFixture::new([transaction.clone()])
            .with_gettxout_responses(outpoints[0], [malformed, FixtureResponse::Missing])
            .with_gettxout_responses(
                outpoints[1],
                [
                    FixtureResponse::Result(json!({
                        "scriptPubKey": { "hex": "zz" }
                    })),
                    FixtureResponse::Missing,
                ],
            );
        let (_, address, server) = start_rpc_fixture(rpc).await;
        let enricher = test_enricher(address);
        enricher
            .install_snapshot(test_snapshot(1, &[transaction]))
            .expect("membership");

        let first = enricher.classify_next().await.expect("local failures");
        assert_eq!(first.response_failures, 2);
        assert_eq!(first.missing_responses, 0);
        assert_eq!(first.deferred_candidates, 0);
        assert_eq!(first.disposition, PolicyDrain::Continue);

        let second = enricher.classify_next().await.expect("all missing batch");
        server.abort();

        assert_eq!(second.response_failures, 2);
        assert_eq!(second.systemic_response_failures, 0);
        assert_eq!(second.missing_responses, 2);
        assert_eq!(second.facts_missing, 0);
        assert_eq!(second.deferred_candidates, 0);
        assert_eq!(second.remaining, 1);
        assert!(!second.complete);
        assert_eq!(second.disposition, PolicyDrain::Paused);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn systemic_fact_failure_pauses_even_after_candidate_raw_progress() {
        let transaction = transaction();
        let txid = transaction.compute_txid().to_string();
        let outpoint = transaction.input[0].previous_output;
        let rpc = RpcFixture::new([transaction.clone()])
            .with_gettxout_responses(outpoint, [FixtureResponse::SystemicError]);
        let (_, address, server) = start_rpc_fixture(rpc).await;
        let enricher = test_enricher(address);
        enricher
            .install_snapshot(test_snapshot(1, &[transaction]))
            .expect("membership");

        let report = enricher.classify_next().await.expect("systemic response");
        server.abort();

        assert_eq!(report.attempted, 1);
        assert_eq!(report.fact_requests, 1);
        assert_eq!(report.response_failures, 1);
        assert_eq!(report.systemic_response_failures, 1);
        assert_eq!(report.missing_responses, 0);
        assert_eq!(report.newly_classified, 0);
        assert_eq!(report.remaining, 1);
        assert!(!report.complete);
        assert_eq!(report.disposition, PolicyDrain::Paused);
        assert!(!report.classifications.contains_key(&txid));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn framing_failure_pauses_with_only_the_batch_failure_counter() {
        let transaction = transaction();
        let (address, server) = start_invalid_policy_fixture().await;
        let enricher = test_enricher(address);
        enricher
            .install_snapshot(test_snapshot(1, &[transaction]))
            .expect("membership");

        let report = enricher
            .classify_next()
            .await
            .expect("batch failure report");
        server.abort();

        assert_eq!(report.batch_failures, 1);
        assert_eq!(report.response_failures, 0);
        assert_eq!(report.systemic_response_failures, 0);
        assert_eq!(report.missing_responses, 0);
        assert_eq!(report.fact_requests, 0);
        assert_eq!(report.newly_classified, 0);
        assert!(!report.complete);
        assert_eq!(report.disposition, PolicyDrain::Paused);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn systemic_raw_batch_stops_replenishing_the_current_wave() {
        let transactions = (0..300_u64)
            .map(|index| {
                let mut transaction = transaction();
                transaction.output[0].value = Amount::from_sat(500 + index);
                transaction
            })
            .collect::<Vec<_>>();
        let membership = test_snapshot(1, &transactions);
        let first_txid = membership.transactions[0]
            .txid
            .parse::<Txid>()
            .expect("valid first txid");
        let rpc = RpcFixture::new(transactions).with_raw_systemic_failure(first_txid);
        let (fixture, address, server) = start_rpc_fixture(rpc).await;
        let enricher = test_enricher_with_limits(
            address,
            PolicyLimits::new(300, 1, 16 * 1024 * 1024).expect("limits"),
        );
        enricher.install_snapshot(membership).expect("membership");

        let report = enricher.classify_next().await.expect("systemic raw batch");
        server.abort();

        assert_eq!(report.attempted, 300);
        assert_eq!(report.systemic_response_failures, 1);
        assert_eq!(report.fact_requests, 0);
        assert_eq!(report.newly_classified, 0);
        assert_eq!(report.disposition, PolicyDrain::Paused);
        assert!(!report.complete);
        let fixture = fixture.fixture.lock().await;
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, _)| method == "getrawtransaction")
                .count(),
            RAW_TRANSACTION_BATCH_SIZE,
            "the systemic first batch must not replenish the remaining wave"
        );
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, _)| method == "gettxout")
                .count(),
            0,
            "a systemic raw failure must stop later fact phases in the slice"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn parent_fallback_null_does_not_starve_the_next_slice_candidate() {
        let (parent, child) = (0..10_000_u64)
            .find_map(|value| {
                let mut parent = transaction();
                parent.output[0].value = Amount::from_sat(1_000 + value);
                let mut child = transaction();
                child.input[0].previous_output = OutPoint {
                    txid: parent.compute_txid(),
                    vout: 0,
                };
                child.input[0].witness = Witness::from_slice(&[vec![3], vec![4]]);
                (child.compute_txid() < parent.compute_txid()).then_some((parent, child))
            })
            .expect("find child ordered before parent");
        let parent_txid = parent.compute_txid();
        let parent_txid_string = parent_txid.to_string();
        let child_txid = child.compute_txid().to_string();
        let parent_outpoint = OutPoint {
            txid: parent_txid,
            vout: 0,
        };
        let rpc = RpcFixture::new([parent.clone(), child.clone()])
            .with_raw_failure(parent_txid)
            .with_gettxout_results(parent_outpoint, [Value::Null, Value::Null]);
        let (fixture, address, server) = start_rpc_fixture(rpc).await;
        let enricher = test_enricher_with_limits(
            address,
            PolicyLimits::new(1, 1, 16 * 1024 * 1024).expect("limits"),
        );
        let membership = test_snapshot(1, &[parent, child]);
        assert_eq!(membership.transactions[0].txid, child_txid);
        let initial = enricher.install_snapshot(membership).expect("membership");
        assert!(
            initial.observation.snapshot.transactions[0]
                .bip110
                .is_none()
        );

        let first = enricher.classify_next().await.expect("failed parent raw");
        assert_eq!(first.newly_classified, 0);
        assert_eq!(first.facts_missing, 0);
        assert_eq!(first.disposition, PolicyDrain::Continue);
        assert!(!first.classifications.contains_key(&child_txid));

        let second = enricher.classify_next().await.expect("fallback null retry");
        assert_eq!(second.attempted, 0);
        assert_eq!(second.newly_classified, 0);
        assert_eq!(second.facts_missing, 0);
        assert_eq!(second.deferred_candidates, 1);
        assert_eq!(second.disposition, PolicyDrain::Continue);
        assert!(!second.classifications.contains_key(&child_txid));

        let third = enricher.classify_next().await.expect("later candidate");
        assert_eq!(third.attempted, 1);
        assert_eq!(third.newly_classified, 1);
        assert_eq!(third.disposition, PolicyDrain::Complete);
        assert!(!third.classifications.contains_key(&child_txid));
        assert_eq!(
            third.classifications[&parent_txid_string].assessment.status,
            Bip110Status::Compatible
        );
        server.abort();

        let fixture = fixture.fixture.lock().await;
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, params)| {
                    method == "getrawtransaction" && params[0] == child_txid
                })
                .count(),
            1
        );
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, params)| {
                    method == "getrawtransaction" && params[0] == parent_txid_string
                })
                .count(),
            2
        );
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, _)| method == "gettxout")
                .count(),
            3
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolves_unconfirmed_prevout_from_raw_mempool_parent() {
        let parent = transaction();
        let parent_txid = parent.compute_txid();
        let external_txid = parent.input[0].previous_output.txid;
        let mut child = transaction();
        child.input[0].previous_output = OutPoint {
            txid: parent_txid,
            vout: 0,
        };
        child.input[0].witness = Witness::from_slice(&[vec![3], vec![4]]);
        let child_txid = child.compute_txid().to_string();

        let mut entries = vec![mempool_entry(&parent), mempool_entry(&child)];
        entries.sort_unstable_by(|left, right| left.txid.cmp(&right.txid));
        let (fixture, address, server) =
            start_rpc_fixture(RpcFixture::new([parent.clone(), child])).await;
        let enricher = test_enricher(address);

        let (entries, report) = enrich_entries(&enricher, entries).await;
        server.abort();

        assert_eq!(report.newly_classified, 2);
        assert_eq!(report.response_failures, 0);
        assert_eq!(
            report.classifications[&child_txid].assessment.status,
            Bip110Status::Compatible
        );
        assert!(entries.iter().all(|entry| {
            entry
                .bip110
                .as_ref()
                .is_some_and(|assessment| assessment.status == Bip110Status::Compatible)
        }));

        let fixture = fixture.fixture.lock().await;
        let parent_txid = parent_txid.to_string();
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, params)| {
                    method == "getrawtransaction" && params[0] == parent_txid
                })
                .count(),
            1,
            "the child's parent output is reused from the same raw batch"
        );
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, params)| {
                    method == "getrawtransaction" && params[0] == child_txid
                })
                .count(),
            1
        );
        let gettxout_calls = fixture
            .calls
            .iter()
            .filter(|(method, _)| method == "gettxout")
            .collect::<Vec<_>>();
        assert_eq!(gettxout_calls.len(), 1);
        assert_eq!(gettxout_calls[0].1[0], external_txid.to_string());
        assert_eq!(gettxout_calls[0].1[2], Value::Bool(false));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn current_parent_outputs_are_reused_across_membership_snapshots() {
        let parent = transaction();
        let parent_txid = parent.compute_txid();
        let mut child = transaction();
        child.input[0].previous_output = OutPoint {
            txid: parent_txid,
            vout: 0,
        };
        child.input[0].witness = Witness::from_slice(&[vec![3], vec![4]]);
        let child_txid = child.compute_txid().to_string();
        let (fixture, address, server) =
            start_rpc_fixture(RpcFixture::new([parent.clone(), child.clone()])).await;
        let enricher = test_enricher(address);

        let first = MempoolSnapshot::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            1,
            crate::model::ChainTip {
                height: 1,
                hash: "00".repeat(32),
            },
            vec![mempool_entry(&parent)],
        )
        .expect("first membership");
        enricher.install_snapshot(first).expect("first generation");
        enricher.classify_next().await.expect("parent slice");

        let mut entries = vec![mempool_entry(&parent), mempool_entry(&child)];
        entries.sort_unstable_by(|left, right| left.txid.cmp(&right.txid));
        let second = MempoolSnapshot::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            2,
            crate::model::ChainTip {
                height: 2,
                hash: "11".repeat(32),
            },
            entries,
        )
        .expect("second membership");
        let immediate = enricher
            .install_snapshot(second)
            .expect("second generation");
        assert_eq!(
            immediate
                .observation
                .snapshot
                .bip110_summary
                .compatible_count,
            1
        );
        let report = enricher.classify_next().await.expect("child slice");
        assert_eq!(
            report.classifications[&child_txid].assessment.status,
            Bip110Status::Compatible
        );

        let empty = MempoolSnapshot::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            3,
            crate::model::ChainTip {
                height: 3,
                hash: "22".repeat(32),
            },
            Vec::new(),
        )
        .expect("empty membership");
        enricher.install_snapshot(empty).expect("empty generation");
        let generation = enricher.current_generation().expect("current generation");
        {
            let work = generation.work();
            assert!(work.classifications.is_empty());
            assert!(work.outputs.is_empty());
            assert!(work.attempted.is_empty());
            assert!(work.pending.is_none());
            assert_eq!(work.output_cache_bytes, 0);
        }
        assert!(
            enricher.coordinator().confirmed_cache.estimated_bytes
                <= enricher.limits.max_auxiliary_cache_bytes
        );
        server.abort();

        let fixture = fixture.fixture.lock().await;
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, params)| {
                    method == "getrawtransaction" && params[0] == parent_txid.to_string()
                })
                .count(),
            1,
            "the current parent is not fetched again for the child"
        );
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, params)| {
                    method == "getrawtransaction" && params[0] == child_txid
                })
                .count(),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn confirmed_script_is_reused_for_a_different_child_next_generation() {
        let first = transaction();
        let mut second = transaction();
        second.output[0].value = Amount::from_sat(501);
        let outpoint = first.input[0].previous_output;
        assert_eq!(second.input[0].previous_output, outpoint);
        let first_txid = first.compute_txid().to_string();
        let second_txid = second.compute_txid().to_string();
        let (fixture, address, server) =
            start_rpc_fixture(RpcFixture::new([first.clone(), second.clone()])).await;
        let enricher = test_enricher(address);

        enricher
            .install_snapshot(test_snapshot(1, &[first]))
            .expect("first generation");
        let first_report = enricher.classify_next().await.expect("first child");
        assert_eq!(first_report.newly_classified, 1);
        assert_eq!(
            first_report.classifications[&first_txid].assessment.status,
            Bip110Status::Compatible
        );

        enricher
            .install_snapshot(test_snapshot(2, &[second]))
            .expect("second generation");
        let second_report = enricher.classify_next().await.expect("second child");
        assert_eq!(second_report.newly_classified, 1);
        assert_eq!(second_report.fact_requests, 0);
        assert_eq!(second_report.facts_resolved, 1);
        assert_eq!(
            second_report.classifications[&second_txid]
                .assessment
                .status,
            Bip110Status::Compatible
        );
        server.abort();

        let fixture = fixture.fixture.lock().await;
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, _)| method == "getrawtransaction")
                .count(),
            2
        );
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, _)| method == "gettxout")
                .count(),
            1,
            "the positive confirmed fact is safe to reuse across generations"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn same_txid_new_wtxid_replaces_the_cached_variant() {
        let first_variant = transaction();
        let mut second_variant = first_variant.clone();
        second_variant.input[0].witness = Witness::from_slice(&[vec![9], vec![10]]);
        let txid = first_variant.compute_txid().to_string();
        let first_wtxid = first_variant.compute_wtxid().to_string();
        let second_wtxid = second_variant.compute_wtxid().to_string();
        assert_eq!(txid, second_variant.compute_txid().to_string());
        assert_ne!(first_wtxid, second_wtxid);

        let (fixture, address, server) =
            start_rpc_fixture(RpcFixture::new([first_variant.clone()])).await;
        let enricher = test_enricher(address);
        let (_, first) = enrich_entries(&enricher, vec![mempool_entry(&first_variant)]).await;
        assert_eq!(
            first.classifications[&txid].wtxid, first_wtxid,
            "the first witness variant is cached"
        );

        fixture
            .fixture
            .lock()
            .await
            .raw_transactions
            .insert(txid.clone(), serialize_hex(&second_variant));
        let (entries, second) =
            enrich_entries(&enricher, vec![mempool_entry(&second_variant)]).await;
        server.abort();

        assert_eq!(second.attempted, 1);
        assert_eq!(second.newly_classified, 1);
        assert_eq!(second.classifications[&txid].wtxid, second_wtxid);
        assert_eq!(
            entries[0]
                .bip110
                .as_ref()
                .map(|assessment| assessment.status),
            Some(Bip110Status::Compatible)
        );
        let generation = enricher.current_generation().expect("current generation");
        let cache = generation.work();
        assert_eq!(cache.classifications.len(), 1);
        assert!(!cache.classifications.contains_key(&first_wtxid));
        assert_eq!(
            cache
                .classifications
                .get(&second_wtxid)
                .expect("replacement witness variant")
                .classification
                .wtxid,
            second_wtxid
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fresh_entries_are_classified_before_retryable_entries() {
        let first_variant = transaction();
        let mut second_variant = transaction();
        second_variant.output[0].value = Amount::from_sat(501);
        let mut entries = vec![
            mempool_entry(&first_variant),
            mempool_entry(&second_variant),
        ];
        entries.sort_unstable_by(|left, right| left.txid.cmp(&right.txid));
        let first_txid = entries[0].txid.clone();
        let second_txid = entries[1].txid.clone();
        let outpoint = first_variant.input[0].previous_output;
        let rpc = RpcFixture::new([first_variant, second_variant])
            .with_gettxout_results(outpoint, [Value::Null, valid_gettxout_result()]);
        let (fixture, address, server) = start_rpc_fixture(rpc).await;
        let enricher = test_enricher_with_limits(
            address,
            PolicyLimits::new(1, 4, 16 * 1024 * 1024).expect("limits"),
        );

        let (entries, first) = enrich_entries(&enricher, entries).await;
        assert_eq!(first.attempted, 1);
        assert_eq!(
            first.classifications[&first_txid].assessment.status,
            Bip110Status::Indeterminate
        );
        assert!(!first.classifications.contains_key(&second_txid));

        let (_, second) = enrich_entries(&enricher, entries).await;
        server.abort();

        assert_eq!(second.attempted, 1);
        assert_eq!(
            second.classifications[&second_txid].assessment.status,
            Bip110Status::Compatible
        );
        assert!(
            second.classifications.contains_key(&first_txid),
            "the retryable cached result remains visible while fresh work runs first"
        );
        let fixture = fixture.fixture.lock().await;
        let fetched_txids = fixture
            .calls
            .iter()
            .filter(|(method, _)| method == "getrawtransaction")
            .map(|(_, parameters)| parameters[0].as_str().expect("txid").to_owned())
            .collect::<Vec<_>>();
        assert_eq!(fetched_txids, [first_txid, second_txid]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auxiliary_cache_never_exceeds_its_admission_limit() {
        let transaction = transaction();
        let cache_limit = estimated_script_bytes(&transaction.output[0].script_pubkey);
        let (fixture, address, server) =
            start_rpc_fixture(RpcFixture::new([transaction.clone()])).await;
        let enricher = test_enricher_with_limits(
            address,
            PolicyLimits::new(1, 1, cache_limit).expect("one-script cache limit"),
        );
        let membership = MempoolSnapshot::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            1,
            crate::model::ChainTip {
                height: 1,
                hash: "00".repeat(32),
            },
            vec![mempool_entry(&transaction)],
        )
        .expect("membership");
        enricher.install_snapshot(membership).expect("generation");

        let report = enricher.classify_next().await.expect("slice");
        server.abort();

        assert_eq!(report.newly_classified, 1);
        let generation = enricher.current_generation().expect("current generation");
        {
            let coordinator = enricher.coordinator();
            let work = generation.work();
            assert_eq!(work.classifications.len(), 1);
            assert!(work.pending.is_none());
            assert!(
                work.output_cache_bytes
                    .saturating_add(coordinator.confirmed_cache.estimated_bytes)
                    <= cache_limit
            );
        }
        assert!(!fixture.fixture.lock().await.calls.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pending_script_capacity_deferral_does_not_create_an_unknown_verdict() {
        let transaction = transaction();
        let txid = transaction.compute_txid().to_string();
        let script =
            ScriptBuf::from_hex("00140000000000000000000000000000000000000000").expect("script");
        let pending_limit = estimated_script_bytes(&script) - 1;
        let (fixture, address, server) =
            start_rpc_fixture(RpcFixture::new([transaction.clone()])).await;
        let enricher = test_enricher_with_limits(
            address,
            PolicyLimits::new(10, 4, pending_limit).expect("small source cache budget"),
        );
        assert_eq!(
            enricher.resolver_limits.max_pending_script_bytes,
            pending_limit
        );
        let initial = enricher
            .install_snapshot(test_snapshot(1, &[transaction]))
            .expect("generation");
        assert!(
            initial.observation.snapshot.transactions[0]
                .bip110
                .is_none()
        );

        let first = enricher.classify_next().await.expect("capacity deferral");
        assert!(first.capacity_deferred >= 1);
        assert_eq!(first.deferred_candidates, 1);
        assert_eq!(first.newly_classified, 0);
        assert_eq!(first.disposition, PolicyDrain::Complete);
        assert!(!first.classifications.contains_key(&txid));
        {
            let generation = enricher.current_generation().expect("generation");
            let work = generation.work();
            assert!(work.pending.is_none());
            assert!(work.classifications.is_empty());
        }
        server.abort();

        let fixture = fixture.fixture.lock().await;
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, _)| method == "getrawtransaction")
                .count(),
            1
        );
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, _)| method == "gettxout")
                .count(),
            1,
            "nonzero remaining capacity permits one bounded probe before deferral"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn capacity_recovery_classifies_all_cached_candidates_before_deferral() {
        let outpoints = [
            confirmed_outpoint(40, 0),
            confirmed_outpoint(41, 0),
            confirmed_outpoint(42, 0),
        ];
        let transactions = outpoints
            .into_iter()
            .map(|outpoint| transaction_spending([outpoint]))
            .collect::<Vec<_>>();
        let script =
            ScriptBuf::from_hex("00140000000000000000000000000000000000000000").expect("script");
        let script_bytes = estimated_script_bytes(&script);
        let (fixture, address, server) =
            start_rpc_fixture(RpcFixture::new(transactions.clone())).await;
        let enricher = test_enricher(address).with_test_pending_script_limit(script_bytes);
        {
            let mut coordinator = enricher.coordinator();
            for outpoint in outpoints {
                assert!(
                    coordinator
                        .confirmed_cache
                        .insert(outpoint, script.clone(), 0,)
                );
            }
        }
        enricher
            .install_snapshot(test_snapshot(1, &transactions))
            .expect("membership");

        let first = enricher.classify_next().await.expect("bounded recovery");
        server.abort();

        assert_eq!(first.fact_requests, 0);
        assert_eq!(first.newly_classified, 3);
        assert_eq!(first.deferred_candidates, 0);
        assert_eq!(first.classified, 3);
        assert_eq!(first.remaining, 0);
        assert_eq!(first.disposition, PolicyDrain::Complete);
        let fixture = fixture.fixture.lock().await;
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, _)| method == "getrawtransaction")
                .count(),
            3
        );
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, _)| method == "gettxout")
                .count(),
            0
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn transient_parent_capacity_recovery_reaches_a_fixed_point_before_cache_loss() {
        let (parent, children) = (0..10_000_u64)
            .find_map(|nonce| {
                let mut parent = transaction();
                let output = parent.output[0].clone();
                parent.output = (0..3_u32)
                    .map(|vout| {
                        let mut output = output.clone();
                        output.value =
                            Amount::from_sat(1_000 + nonce.saturating_mul(3) + u64::from(vout));
                        output
                    })
                    .collect();
                let parent_txid = parent.compute_txid();
                let children = (0..3_u32)
                    .map(|vout| {
                        let mut child = transaction_spending([OutPoint {
                            txid: parent_txid,
                            vout,
                        }]);
                        child.input[0].witness =
                            Witness::from_slice(&[vec![u8::try_from(vout).expect("small vout")]]);
                        child.output[0].value = Amount::from_sat(500 + u64::from(vout));
                        child
                    })
                    .collect::<Vec<_>>();
                children
                    .iter()
                    .all(|child| child.compute_txid().to_string() < parent_txid.to_string())
                    .then_some((parent, children))
            })
            .expect("find children ordered before their parent");
        let child_txids = children
            .iter()
            .map(|child| child.compute_txid().to_string())
            .collect::<Vec<_>>();
        let mut transactions = children;
        transactions.push(parent.clone());
        let membership = test_snapshot(1, &transactions);
        assert_eq!(
            membership
                .transactions
                .iter()
                .take(3)
                .map(|entry| entry.txid.clone())
                .collect::<Vec<_>>(),
            {
                let mut expected = child_txids.clone();
                expected.sort_unstable();
                expected
            }
        );
        let script = parent.output[0].script_pubkey.clone();
        let (fixture, address, server) = start_rpc_fixture(RpcFixture::new(transactions)).await;
        let enricher = test_enricher_with_limits(
            address,
            PolicyLimits::new(3, 1, 1).expect("tiny auxiliary cache"),
        )
        .with_test_pending_script_limit(estimated_script_bytes(&script));
        enricher.install_snapshot(membership).expect("membership");

        let report = enricher.classify_next().await.expect("transient recovery");
        server.abort();

        assert_eq!(report.attempted, 3);
        assert_eq!(report.fact_requests, 1);
        assert_eq!(report.newly_classified, 3);
        assert_eq!(report.deferred_candidates, 0);
        assert_eq!(report.remaining, 1);
        assert_eq!(report.disposition, PolicyDrain::Continue);
        assert!(
            child_txids
                .iter()
                .all(|txid| report.classifications.contains_key(txid))
        );
        let generation = enricher.current_generation().expect("generation");
        assert!(
            !generation
                .work()
                .outputs
                .contains_key(&parent.compute_txid()),
            "the transient parent deliberately cannot enter the one-byte reuse cache"
        );
        let fixture = fixture.fixture.lock().await;
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, _)| method == "getrawtransaction")
                .count(),
            4
        );
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, _)| method == "gettxout")
                .count(),
            0
        );
    }

    #[test]
    fn capacity_victim_frees_cached_fact_for_an_adversarial_shared_survivor() {
        let unique = confirmed_outpoint(1, 0);
        let shared = confirmed_outpoint(2, 0);
        assert!(
            unique < shared,
            "cache hydration must admit the unique fact first"
        );
        let survivor = transaction_spending([shared]);
        let victim = transaction_spending([shared, unique]);
        let membership = Arc::new(test_snapshot(1, &[survivor.clone(), victim.clone()]));
        let variants = membership
            .transactions
            .iter()
            .map(|entry| {
                let txid = entry.txid.parse::<Txid>().expect("valid membership txid");
                (
                    txid,
                    CurrentVariant {
                        wtxid: entry.wtxid.clone(),
                        vsize: entry.vsize,
                    },
                )
            })
            .collect();
        let generation = PolicyGeneration {
            id: 1,
            membership,
            variants,
            work: Mutex::new(GenerationWork::default()),
            driver: tokio::sync::Mutex::new(()),
        };
        let expected = |transaction: &Transaction| ExpectedVariant {
            txid: transaction.compute_txid().to_string(),
            wtxid: transaction.compute_wtxid().to_string(),
            vsize: 100,
        };
        let survivor_wtxid = survivor.compute_wtxid().to_string();
        let victim_wtxid = victim.compute_wtxid().to_string();
        let script =
            ScriptBuf::from_hex("00140000000000000000000000000000000000000000").expect("script");
        let script_bytes = estimated_script_bytes(&script);
        let limits = ResolverLimits {
            max_pending_script_bytes: script_bytes,
            ..ResolverLimits::production(MAX_PENDING_SCRIPT_BYTES)
        };
        let mut pending = PendingSlice::new(
            vec![(expected(&survivor), survivor), (expected(&victim), victim)],
            &generation,
            limits,
        );
        assert_eq!(pending.fair_order.front(), Some(&survivor_wtxid));
        let mut confirmed = ConfirmedScriptCache::new(script_bytes * 4);
        assert!(confirmed.insert(unique, script.clone(), 0));
        assert!(confirmed.insert(shared, script, 0));

        let (resolved, capacity_deferred) = pending.hydrate_from_caches(
            &generation.variants,
            &BTreeMap::new(),
            &mut confirmed,
            limits,
        );
        assert_eq!(resolved, 1);
        assert_eq!(capacity_deferred, 1);
        assert!(matches!(
            pending.facts.get(&unique),
            Some(FactState::Ready(_))
        ));
        assert!(matches!(
            pending.facts.get(&shared),
            Some(FactState::CapacityBlocked(FactSource::Confirmed))
        ));

        assert_eq!(pending.defer_capacity_blocked_candidate(), 1);
        assert!(pending.active.contains_key(&survivor_wtxid));
        assert!(!pending.active.contains_key(&victim_wtxid));
        assert_eq!(pending.pending_script_bytes, 0);

        let (resolved, capacity_deferred) = pending.hydrate_from_caches(
            &generation.variants,
            &BTreeMap::new(),
            &mut confirmed,
            limits,
        );
        assert_eq!(resolved, 1);
        assert_eq!(capacity_deferred, 0);
        let classified = pending.classify_ready();
        assert_eq!(classified.len(), 1);
        assert_eq!(classified[0].classification.wtxid, survivor_wtxid);
        assert!(pending.is_empty());
    }

    async fn assert_multi_victim_transient_fact_is_consumed(source: FactSource) {
        let first_ready = confirmed_outpoint(60, 0);
        let second_ready = confirmed_outpoint(61, 0);
        let transient = confirmed_outpoint(62, 0);
        let first = transaction_spending([first_ready, transient]);
        let second = transaction_spending([second_ready, transient]);
        let survivor = transaction_spending([transient]);
        let transactions = [first.clone(), second.clone(), survivor.clone()];
        let small_script = ScriptBuf::from_bytes(vec![0x51; 44]);
        let transient_script = ScriptBuf::from_bytes(vec![0x51; 600]);
        let enricher = test_enricher(SocketAddr::from(([127, 0, 0, 1], 1)))
            .with_test_pending_script_limit(estimated_script_bytes(&transient_script));
        enricher
            .install_snapshot(test_snapshot(1, &transactions))
            .expect("membership");
        let generation = enricher.current_generation().expect("generation");
        let expected = |transaction: &Transaction| ExpectedVariant {
            txid: transaction.compute_txid().to_string(),
            wtxid: transaction.compute_wtxid().to_string(),
            vsize: 100,
        };
        let survivor_wtxid = survivor.compute_wtxid().to_string();
        assert_eq!(
            estimated_script_bytes(&transient_script),
            estimated_script_bytes(&small_script) * 2
        );
        let limits = ResolverLimits {
            max_pending_script_bytes: estimated_script_bytes(&transient_script),
            ..ResolverLimits::production(MAX_PENDING_SCRIPT_BYTES)
        };
        let mut pending = PendingSlice::new(
            vec![
                (expected(&first), first),
                (expected(&second), second),
                (expected(&survivor), survivor),
            ],
            &generation,
            limits,
        );
        pending
            .facts
            .insert(first_ready, FactState::Ready(small_script.clone()));
        pending
            .facts
            .insert(second_ready, FactState::Ready(small_script));
        pending
            .facts
            .insert(transient, FactState::CapacityBlocked(source));
        pending.recount_script_bytes();
        assert_eq!(
            pending.pending_script_bytes,
            limits.max_pending_script_bytes
        );
        let mut advance = PolicyAdvance::default();
        match source {
            FactSource::CurrentParent => {
                advance.outputs.insert(
                    transient.txid,
                    CachedOutputs {
                        wtxid: "00".repeat(32),
                        scripts: Arc::new(vec![transient_script]),
                        estimated_bytes: limits.max_pending_script_bytes,
                    },
                );
            }
            FactSource::Confirmed | FactSource::ParentFallback => {
                advance
                    .confirmed_scripts
                    .insert(transient, transient_script);
            }
        }

        assert!(
            enricher
                .resolve_capacity_pressure(&mut pending, &generation, &mut advance)
                .await
        );
        assert_eq!(advance.deferred_candidates, 2);
        assert_eq!(advance.classifications.len(), 1);
        assert_eq!(
            advance.classifications[0].classification.wtxid,
            survivor_wtxid
        );
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn multi_victim_current_parent_fact_is_consumed_before_transient_loss() {
        assert_multi_victim_transient_fact_is_consumed(FactSource::CurrentParent).await;
    }

    #[tokio::test]
    async fn multi_victim_confirmed_fact_is_consumed_before_transient_loss() {
        assert_multi_victim_transient_fact_is_consumed(FactSource::Confirmed).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn late_results_from_a_replaced_generation_are_discarded() {
        let transaction = transaction();
        let (fixture, address, server) = start_rpc_fixture(
            RpcFixture::new([transaction.clone()]).with_delay(Duration::from_millis(50)),
        )
        .await;
        let enricher = test_enricher(address);
        let membership = MempoolSnapshot::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            1,
            crate::model::ChainTip {
                height: 1,
                hash: "00".repeat(32),
            },
            vec![mempool_entry(&transaction)],
        )
        .expect("membership");
        enricher
            .install_snapshot(membership)
            .expect("first generation");

        let task = tokio::spawn({
            let enricher = enricher.clone();
            async move { enricher.classify_next().await.expect("stale slice") }
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        let replacement = MempoolSnapshot::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            2,
            crate::model::ChainTip {
                height: 2,
                hash: "11".repeat(32),
            },
            Vec::new(),
        )
        .expect("replacement membership");
        let current = enricher
            .install_snapshot(replacement)
            .expect("replacement generation");
        let stale = task.await.expect("classification task");
        server.abort();

        assert!(stale.stale);
        assert!(stale.publication.is_none());
        assert!(current.observation.classifications.is_empty());
        let generation = enricher.current_generation().expect("current generation");
        let work = generation.work();
        assert!(work.classifications.is_empty());
        assert!(work.outputs.is_empty());
        assert!(
            fixture.max_active_requests.load(Ordering::SeqCst) >= 1,
            "the stale request reached the fixture"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replaced_generation_cannot_populate_the_shared_confirmed_cache() {
        let transaction = transaction_spending([confirmed_outpoint(52, 0)]);
        let outpoint = transaction.input[0].previous_output;
        let (fixture, address, server) =
            start_rpc_fixture(RpcFixture::new([transaction.clone()]).with_blocked_gettxout()).await;
        let enricher = test_enricher(address);
        let old = enricher
            .install_snapshot(test_snapshot(1, &[transaction]))
            .expect("old membership");
        let classifier = tokio::spawn({
            let enricher = enricher.clone();
            async move {
                enricher
                    .classify_next_for_generation(Some(old.generation))
                    .await
            }
        });

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let notified = fixture.gettxout_started.notified();
                if fixture.gettxout_requests_started.load(Ordering::SeqCst) > 0 {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("confirmed fetch started");
        let replacement = enricher
            .install_snapshot(test_snapshot(2, &[]))
            .expect("replacement membership");
        assert_eq!(replacement.revision, 0);
        assert!(
            !enricher
                .coordinator()
                .confirmed_cache
                .entries
                .contains_key(&outpoint)
        );

        fixture.release_gettxout.add_permits(1);
        let stale = classifier
            .await
            .expect("classification task")
            .expect("stale report");
        server.abort();

        assert!(stale.stale);
        assert!(stale.publication.is_none());
        assert!(
            !enricher
                .coordinator()
                .confirmed_cache
                .entries
                .contains_key(&outpoint),
            "late positive facts must not cross the stale-generation merge guard"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn same_generation_classification_calls_are_serialized() {
        let transactions = [
            transaction_spending([confirmed_outpoint(50, 0)]),
            transaction_spending([confirmed_outpoint(51, 0)]),
        ];
        let (fixture, address, server) = start_rpc_fixture(
            RpcFixture::new(transactions.clone()).with_delay(Duration::from_millis(30)),
        )
        .await;
        let enricher = test_enricher_with_limits(
            address,
            PolicyLimits::new(1, 1, 16 * 1024 * 1024).expect("limits"),
        );
        let publication = enricher
            .install_snapshot(test_snapshot(1, &transactions))
            .expect("membership");

        let (first, second) = tokio::join!(
            enricher.classify_next_for_generation(Some(publication.generation)),
            enricher.classify_next_for_generation(Some(publication.generation)),
        );
        server.abort();
        let reports = [first.expect("first call"), second.expect("second call")];

        assert_eq!(
            reports.iter().map(|report| report.attempted).sum::<usize>(),
            2
        );
        assert_eq!(
            reports
                .iter()
                .map(|report| report.newly_classified)
                .sum::<usize>(),
            2
        );
        assert_eq!(
            enricher
                .current_generation()
                .expect("generation")
                .work()
                .classifications
                .len(),
            2
        );
        assert_eq!(
            fixture.max_active_requests.load(Ordering::SeqCst),
            1,
            "the generation driver must prevent overlapping scheduler RPC work"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn install_publication_is_revision_zero_before_concurrent_classification_exposure() {
        let transaction = transaction_spending([confirmed_outpoint(54, 0)]);
        let (fixture, address, server) =
            start_rpc_fixture(RpcFixture::new([transaction.clone()])).await;
        let enricher = test_enricher(address);
        let entered = Shared::new(Barrier::new(2));
        let release = Shared::new(Barrier::new(2));
        let installer = tokio::task::spawn_blocking({
            let enricher = enricher.clone();
            let entered = Shared::clone(&entered);
            let release = Shared::clone(&release);
            move || {
                enricher.install_snapshot_before_exposure(test_snapshot(1, &[transaction]), || {
                    entered.wait();
                    release.wait();
                })
            }
        });
        tokio::task::spawn_blocking({
            let entered = Shared::clone(&entered);
            move || entered.wait()
        })
        .await
        .expect("publication materialized");

        let classifier = tokio::spawn({
            let enricher = enricher.clone();
            async move { enricher.classify_next().await }
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            fixture.active_requests.load(Ordering::SeqCst),
            0,
            "the new generation must remain hidden until revision zero is materialized"
        );
        tokio::task::spawn_blocking({
            let release = Shared::clone(&release);
            move || release.wait()
        })
        .await
        .expect("release installer");

        let publication = installer
            .await
            .expect("install task")
            .expect("membership publication");
        let report = classifier
            .await
            .expect("classification task")
            .expect("classification report");
        server.abort();

        assert_eq!(publication.revision, 0);
        assert_eq!(publication.observation.snapshot.classification_revision, 0);
        assert_eq!(report.newly_classified, 1);
        assert_eq!(
            report
                .publication
                .as_ref()
                .map(|publication| publication.revision),
            Some(1)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn raw_batches_use_multiple_but_bounded_rpc_lanes() {
        let transactions = (0..1_300_u64)
            .map(|index| {
                let mut transaction = transaction();
                transaction.output[0].value = Amount::from_sat(500 + index);
                transaction
            })
            .collect::<Vec<_>>();
        let entries = transactions.iter().map(mempool_entry).collect::<Vec<_>>();
        let (fixture, address, server) =
            start_rpc_fixture(RpcFixture::new(transactions).with_delay(Duration::from_millis(30)))
                .await;
        let enricher = test_enricher_with_limits(
            address,
            PolicyLimits::new(1_300, 4, 16 * 1024 * 1024).expect("limits"),
        );
        let membership = MempoolSnapshot::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            1,
            crate::model::ChainTip {
                height: 1,
                hash: "00".repeat(32),
            },
            {
                let mut entries = entries;
                entries.sort_unstable_by(|left, right| left.txid.cmp(&right.txid));
                entries
            },
        )
        .expect("membership");
        enricher.install_snapshot(membership).expect("generation");

        let report = enricher.classify_next().await.expect("parallel slice");
        server.abort();

        assert_eq!(report.attempted, 1_300);
        assert_eq!(report.newly_classified, 1_300);
        assert_eq!(report.batch_failures, 0);
        let maximum = fixture.max_active_requests.load(Ordering::SeqCst);
        assert_eq!(maximum, 4, "all configured lanes should be usable");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn confirmed_batches_use_multiple_but_bounded_rpc_lanes() {
        let outpoint_count = maximum_confirmed_prevouts_per_batch() * 3;
        let (fixture, address, server) =
            start_rpc_fixture(RpcFixture::new([]).with_delay(Duration::from_millis(30))).await;
        let enricher = test_enricher(address);

        let fetched = enricher
            .fetch_confirmed_prevouts_concurrently(vec![OutPoint::null(); outpoint_count], 2, None)
            .await;
        server.abort();

        assert_eq!(fetched.values.len(), outpoint_count);
        assert_eq!(fetched.response_failures, 0);
        assert_eq!(fetched.batch_failures, 0);
        assert_eq!(fixture.max_active_requests.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn raw_batches_obey_count_and_estimated_response_bounds() {
        let variants = (0..RAW_TRANSACTION_BATCH_SIZE + 1)
            .map(|index| ExpectedVariant {
                txid: format!("{index:064x}"),
                wtxid: format!("{index:064x}"),
                vsize: 100,
            })
            .collect();
        let batches = raw_transaction_batches(variants);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].len(), RAW_TRANSACTION_BATCH_SIZE);
        assert_eq!(batches[1].len(), 1);

        let large = (0..2)
            .map(|index| ExpectedVariant {
                txid: format!("{index:064x}"),
                wtxid: format!("{index:064x}"),
                vsize: u64::try_from(MAX_BATCH_RESPONSE_BYTES / 8).expect("vsize"),
            })
            .collect();
        assert_eq!(raw_transaction_batches(large).len(), 2);
    }

    #[test]
    fn slice_candidates_obey_the_aggregate_raw_response_estimate() {
        let variants = (0..40)
            .map(|index| ExpectedVariant {
                txid: format!("{index:064x}"),
                wtxid: format!("{index:064x}"),
                vsize: 1024 * 1024,
            })
            .collect::<Vec<_>>();
        let bounded = bounded_variants(variants.clone(), MAX_TRANSACTIONS_PER_SLICE);
        let estimated = estimated_raw_batch_response_bytes(&bounded);

        assert_eq!(bounded.len(), 31);
        assert!(estimated <= MAX_RAW_RESPONSE_BYTES_PER_SLICE);
        assert!(
            estimated.saturating_add(estimated_raw_response_bytes(&variants[bounded.len()]))
                > MAX_RAW_RESPONSE_BYTES_PER_SLICE
        );
    }

    #[test]
    fn confirmed_prevout_batches_obey_estimated_response_bounds() {
        let maximum = maximum_confirmed_prevouts_per_batch();
        assert!(maximum > 0);
        assert!(maximum < MAX_GETTXOUT_BATCH_SIZE);

        let mut remaining = vec![OutPoint::null(); maximum * 2 + 1].into_iter();
        let batches =
            std::iter::from_fn(|| next_confirmed_prevout_batch(&mut remaining)).collect::<Vec<_>>();

        assert_eq!(
            batches.iter().map(Vec::len).collect::<Vec<_>>(),
            [maximum, maximum, 1]
        );
        assert!(batches.iter().all(|batch| {
            estimated_confirmed_batch_response_bytes(batch.len()) <= MAX_BATCH_RESPONSE_BYTES
        }));
        let slice_maximum = maximum_confirmed_prevouts_per_slice();
        assert!(slice_maximum >= maximum);
        assert!(
            estimated_confirmed_batch_response_bytes(slice_maximum)
                <= MAX_CONFIRMED_RESPONSE_BYTES_PER_SLICE
        );
        assert!(
            estimated_confirmed_batch_response_bytes(slice_maximum + 1)
                > MAX_CONFIRMED_RESPONSE_BYTES_PER_SLICE
        );
    }

    #[test]
    fn confirmed_script_cache_evicts_lru_and_purges_conflicts() {
        let first_outpoint = confirmed_outpoint(10, 0);
        let second_outpoint = confirmed_outpoint(11, 0);
        let third_outpoint = confirmed_outpoint(12, 0);
        let script =
            ScriptBuf::from_hex("00140000000000000000000000000000000000000000").expect("script");
        let conflicting_script =
            ScriptBuf::from_hex("00141111111111111111111111111111111111111111")
                .expect("conflicting script");
        let entry_bytes = estimated_script_bytes(&script);
        let mut cache = ConfirmedScriptCache::new(entry_bytes * 2);

        assert!(cache.insert(first_outpoint, script.clone(), 0));
        assert!(cache.insert(second_outpoint, script.clone(), 0));
        assert_eq!(cache.get(&first_outpoint), Some(script.clone()));
        assert!(cache.insert(third_outpoint, script, 0));
        assert!(cache.entries.contains_key(&first_outpoint));
        assert!(!cache.entries.contains_key(&second_outpoint));
        assert!(cache.entries.contains_key(&third_outpoint));
        assert!(cache.estimated_bytes <= cache.maximum_bytes);

        assert!(!cache.insert(first_outpoint, conflicting_script, 0));
        assert!(
            !cache.entries.contains_key(&first_outpoint),
            "contradictory immutable facts invalidate both cache choices"
        );
        assert!(cache.entries.contains_key(&third_outpoint));
        assert!(cache.estimated_bytes <= cache.maximum_bytes);

        let original = ScriptBuf::from_hex("00140000000000000000000000000000000000000000")
            .expect("original script");
        assert!(cache.insert(first_outpoint, original, 0));
        let larger_conflict = ScriptBuf::from_bytes(vec![0x51; 1_024]);
        let no_available_bytes = cache.maximum_bytes;
        assert!(!cache.insert(first_outpoint, larger_conflict, no_available_bytes));
        assert!(
            !cache.entries.contains_key(&first_outpoint),
            "conflict purging must precede admission-size rejection"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn generation_carry_preserves_shared_auxiliary_cache_accounting() {
        let transaction = transaction();
        let txid = transaction.compute_txid();
        let expected = ExpectedVariant {
            txid: txid.to_string(),
            wtxid: transaction.compute_wtxid().to_string(),
            vsize: 100,
        };
        let outputs = cached_outputs(&expected, &transaction);
        let script = transaction.output[0].script_pubkey.clone();
        let script_bytes = estimated_script_bytes(&script);
        let maximum = outputs.estimated_bytes.saturating_add(script_bytes);
        let (fixture, address, server) =
            start_rpc_fixture(RpcFixture::new([transaction.clone()])).await;
        let enricher = test_enricher_with_limits(
            address,
            PolicyLimits::new(1, 1, maximum).expect("exact shared-cache limit"),
        );
        enricher
            .install_snapshot(test_snapshot(1, std::slice::from_ref(&transaction)))
            .expect("first membership");
        let report = enricher
            .classify_next()
            .await
            .expect("first classification");
        assert_eq!(report.newly_classified, 1);
        {
            let generation = enricher.current_generation().expect("first generation");
            let mut coordinator = enricher.coordinator();
            let work = generation.work();
            assert_eq!(work.output_cache_bytes, outputs.estimated_bytes);
            assert!(coordinator.confirmed_cache.insert(
                confirmed_outpoint(53, 0),
                script,
                work.output_cache_bytes,
            ));
            assert!(
                work.output_cache_bytes
                    .saturating_add(coordinator.confirmed_cache.estimated_bytes)
                    <= maximum
            );
        }

        let replacement = enricher
            .install_snapshot(test_snapshot(2, &[transaction]))
            .expect("replacement membership");
        server.abort();

        assert_eq!(replacement.revision, 0);
        {
            let generation = enricher
                .current_generation()
                .expect("replacement generation");
            let coordinator = enricher.coordinator();
            let work = generation.work();
            assert!(work.outputs.contains_key(&txid));
            assert_eq!(work.output_cache_bytes, outputs.estimated_bytes);
            assert!(
                work.output_cache_bytes
                    .saturating_add(coordinator.confirmed_cache.estimated_bytes)
                    <= maximum,
                "carried outputs must reserve bytes from the shared confirmed cache"
            );
        }
        assert!(!fixture.fixture.lock().await.calls.is_empty());
    }

    #[test]
    fn output_and_confirmed_caches_share_one_byte_ceiling() {
        let transaction = transaction();
        let expected = ExpectedVariant {
            txid: transaction.compute_txid().to_string(),
            wtxid: transaction.compute_wtxid().to_string(),
            vsize: 100,
        };
        let outputs = cached_outputs(&expected, &transaction);
        let script = transaction.output[0].script_pubkey.clone();
        let script_bytes = estimated_script_bytes(&script);
        let maximum = outputs.estimated_bytes.saturating_add(script_bytes);
        let mut cache = ConfirmedScriptCache::new(maximum);
        assert!(cache.insert(confirmed_outpoint(13, 0), script.clone(), 0));
        assert!(cache.insert(confirmed_outpoint(14, 0), script, 0));
        let mut work = GenerationWork::default();

        insert_outputs(
            &mut work,
            &mut cache,
            transaction.compute_txid(),
            outputs,
            maximum,
        );

        assert!(work.outputs.contains_key(&transaction.compute_txid()));
        assert!(
            work.output_cache_bytes
                .saturating_add(cache.estimated_bytes)
                <= maximum
        );
        assert_eq!(cache.entries.len(), 1);
    }

    #[test]
    fn oversized_confirmed_prevout_batch_is_rejected_before_dispatch() {
        let enricher = PolicyEnricher::new(
            "http://127.0.0.1:1/",
            "atlas".to_owned(),
            "secret".to_owned(),
            PolicyLimits::new(1, 1, 1).expect("limits"),
        )
        .expect("enricher");
        let outpoints =
            vec![OutPoint::null(); maximum_confirmed_prevouts_per_batch().saturating_add(1)];

        let error = enricher
            .fetch_confirmed_prevouts(&outpoints)
            .expect_err("oversized estimate");

        assert!(matches!(
            error,
            PolicyError::BatchResponseEstimateTooLarge {
                maximum: MAX_BATCH_RESPONSE_BYTES,
                ..
            }
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn oversized_confirmed_script_is_rejected_after_bounded_decode() {
        let oversized_hex = "00".repeat(MAX_CONFIRMED_SCRIPT_HEX_CHARACTERS / 2 + 1);
        let rpc = RpcFixture::new([]).with_gettxout_results(
            OutPoint::null(),
            [json!({
                "scriptPubKey": {
                    "hex": oversized_hex
                }
            })],
        );
        let (fixture, address, server) = start_rpc_fixture(rpc).await;
        let enricher = test_enricher(address);
        let fetch = tokio::task::spawn_blocking(move || {
            enricher.fetch_confirmed_prevouts(&[OutPoint::null()])
        })
        .await
        .expect("fetch task")
        .expect("bounded response");
        server.abort();

        assert_eq!(fetch.values.len(), 1);
        assert!(matches!(fetch.values[0], RpcValue::RetryableFailure));
        assert_eq!(fetch.response_failures, 1);
        assert!(fetch.response_bytes < MAX_BATCH_RESPONSE_BYTES);
        assert_eq!(fixture.fixture.lock().await.calls.len(), 1);
    }

    #[test]
    fn policy_limits_must_be_nonzero() {
        assert!(PolicyLimits::new(0, 1, 1).is_err());
        assert!(PolicyLimits::new(1, 0, 1).is_err());
        assert!(PolicyLimits::new(1, MAX_RPC_LANES + 1, 1).is_err());
        assert!(PolicyLimits::new(1, 1, 0).is_err());
        assert!(PolicyLimits::new(1, 1, MAX_AUXILIARY_CACHE_BYTES + 1).is_err());
        assert!(PolicyLimits::new(2_048, 4, 256 * 1024 * 1024).is_ok());
        assert!(PolicyLimits::new(MAX_TRANSACTIONS_PER_SLICE, 1, 1).is_ok());
        assert!(PolicyLimits::new(MAX_TRANSACTIONS_PER_SLICE + 1, 1, 1).is_err());
    }

    #[test]
    fn small_source_cache_budget_also_caps_pending_scripts() {
        let budget = 64 * 1024;
        let limits = PolicyLimits::new(1, 1, budget).expect("small source budget");
        let enricher = PolicyEnricher::new(
            "http://127.0.0.1:1/",
            "atlas".to_owned(),
            "secret".to_owned(),
            limits,
        )
        .expect("enricher");

        assert_eq!(enricher.limits.max_auxiliary_cache_bytes, budget);
        assert_eq!(enricher.resolver_limits.max_pending_script_bytes, budget);
    }

    #[test]
    fn production_cache_budget_preserves_the_256_mib_pending_ceiling() {
        let budget = 256 * 1024 * 1024;
        let limits = PolicyLimits::new(2_048, 4, budget).expect("production limits");
        let enricher = PolicyEnricher::new(
            "http://127.0.0.1:1/",
            "atlas".to_owned(),
            "secret".to_owned(),
            limits,
        )
        .expect("enricher");

        assert_eq!(enricher.limits.max_auxiliary_cache_bytes, budget);
        assert_eq!(
            enricher.resolver_limits.max_pending_script_bytes,
            MAX_PENDING_SCRIPT_BYTES
        );
    }

    #[test]
    fn proven_violation_preserves_unknowns_and_an_unresolved_primary() {
        let evidence = TxEvidence {
            evaluation_mode: EvaluationMode::MempoolPolicy,
            rules_applied: true,
            is_coinbase: false,
            primary_violation: None,
            rules: Bip110RuleId::ALL
                .into_iter()
                .map(|public_rule| {
                    let rule = match public_rule {
                        Bip110RuleId::OutputSize => RuleId::OutputSize,
                        Bip110RuleId::ElementSize => RuleId::ElementSize,
                        Bip110RuleId::UndefinedVersion => RuleId::UndefinedVersion,
                        Bip110RuleId::TaprootAnnex => RuleId::TaprootAnnex,
                        Bip110RuleId::ControlBlockSize => RuleId::ControlBlockSize,
                        Bip110RuleId::OpSuccess => RuleId::OpSuccess,
                        Bip110RuleId::TapscriptOpIf => RuleId::TapscriptOpIf,
                    };
                    RuleOutcome {
                        rule,
                        number: rule.number(),
                        verdict: if rule == RuleId::ElementSize {
                            RuleVerdict::Violate {
                                evidence: vec![
                                    rdts_rules::Violation::WitnessItemTooLarge {
                                        input: 1,
                                        item_index: 0,
                                        len: 257,
                                        limit: 256,
                                    },
                                    rdts_rules::Violation::WitnessItemTooLarge {
                                        input: 2,
                                        item_index: 0,
                                        len: 300,
                                        limit: 256,
                                    },
                                ],
                                missing: vec![
                                    rdts_rules::Missing::ScriptPubKey { input: 0 },
                                    rdts_rules::Missing::ScriptPubKey { input: 3 },
                                ],
                            }
                        } else {
                            RuleVerdict::Pass
                        },
                    }
                })
                .collect(),
        };

        let classification =
            classification_from_evidence("txid".to_owned(), "wtxid".to_owned(), evidence);

        assert_eq!(classification.assessment.status, Bip110Status::Violating);
        assert_eq!(classification.assessment.primary_rule, None);
        assert_eq!(
            classification.assessment.violated_rules,
            [Bip110RuleId::ElementSize]
        );
        assert_eq!(
            classification.assessment.unknown_rules,
            [Bip110RuleId::ElementSize]
        );
        assert_eq!(classification.rules[1].verdict, Bip110RuleVerdict::Violate);
        assert_eq!(classification.rules[1].evidence_count, 2);
        assert_eq!(classification.rules[1].evidence.len(), 1);
        assert_eq!(classification.rules[1].missing_count, 2);
        assert_eq!(classification.rules[1].missing.len(), 1);
    }
}
