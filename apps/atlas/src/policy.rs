//! Continuous, bounded enrichment of one disposable mempool snapshot.
//!
//! The node remains the membership authority. One generation owns only the
//! classifications and output scripts that still belong to its exact current
//! witness variants. RPC work happens away from that state and is merged only
//! while the generation is still current.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bitcoin::consensus::encode::deserialize_hex;
use bitcoin::{OutPoint, ScriptBuf, Transaction, Txid};
use jsonrpc::Client as JsonRpcClient;
use jsonrpc::minreq_http::MinreqHttpTransport;
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

const RAW_TRANSACTION_BATCH_SIZE: usize = 256;
const MAX_GETTXOUT_BATCH_SIZE: usize = 512;
const MAX_RPC_LANES: usize = 8;
const MAX_TRANSACTIONS_PER_SLICE: usize = 8_192;
const MAX_BATCH_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_RAW_RESPONSE_BYTES_PER_SLICE: usize = 256 * 1024 * 1024;
const MAX_CONFIRMED_RESPONSE_BYTES_PER_SLICE: usize = 256 * 1024 * 1024;
const MAX_UNIQUE_PREVOUTS_PER_SLICE: usize = 65_536;
const MAX_AUXILIARY_CACHE_BYTES: usize = 512 * 1024 * 1024;
const ESTIMATED_JSON_BYTES_PER_RESPONSE: usize = 512;
const MAX_TRANSACTION_HEX_CHARACTERS: usize = 8_000_000;
// Classification is best effort, and scripts above this generous ceiling stay
// typed as missing. The ceiling is far above every 4..=42 byte witness program
// while still bounding legacy-script decoding and per-batch response estimates.
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

#[derive(Clone, Debug)]
pub struct PolicyEnricher {
    client: Arc<JsonRpcClient>,
    coordinator: Arc<Mutex<PolicyCoordinator>>,
    limits: PolicyLimits,
}

impl PolicyEnricher {
    pub fn new(
        url: &str,
        username: String,
        password: String,
        limits: PolicyLimits,
    ) -> Result<Self, PolicyError> {
        let transport = MinreqHttpTransport::builder()
            .timeout(RPC_BATCH_TIMEOUT)
            .url(url)
            .map_err(PolicyError::ClientInitialization)?
            .basic_auth(username, Some(password))
            .build();
        Ok(Self {
            client: Arc::new(JsonRpcClient::with_transport(transport)),
            coordinator: Arc::new(Mutex::new(PolicyCoordinator::default())),
            limits,
        })
    }

    /// Installs fresh complete membership and immediately materializes every
    /// exact classification that survived from the previous generation.
    pub(crate) fn install_snapshot(
        &self,
        membership: MempoolSnapshot,
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

        let generation = {
            let mut coordinator = self.coordinator();
            let id = coordinator
                .next_generation
                .checked_add(1)
                .ok_or(PolicyError::GenerationOverflow)?;
            coordinator.next_generation = id;

            let mut work = GenerationWork::default();
            if let Some(previous) = coordinator.current.as_ref() {
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
                        && work
                            .auxiliary_cache_bytes
                            .checked_add(outputs.estimated_bytes)
                            .is_some_and(|bytes| bytes <= self.limits.max_auxiliary_cache_bytes)
                    {
                        work.auxiliary_cache_bytes += outputs.estimated_bytes;
                        work.outputs.insert(txid, outputs.clone());
                    }
                }
            }

            let generation = Arc::new(PolicyGeneration {
                id,
                membership,
                variants,
                work: Mutex::new(work),
            });
            coordinator.current = Some(Arc::clone(&generation));
            generation
        };

        self.publication_for(&generation)
    }

    /// Classifies one bounded slice from the current generation. Every current
    /// variant is attempted at most once per generation. A later membership
    /// snapshot makes incomplete variants eligible again.
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

        if candidates.is_empty() {
            let (classified, classifications) = {
                let work = generation.work();
                (
                    work.classifications.len(),
                    current_classifications(&generation.membership, &work.classifications),
                )
            };
            return Ok(PolicyEnrichmentReport {
                generation: generation.id,
                attempted: 0,
                newly_classified: 0,
                response_failures: 0,
                batch_failures: 0,
                response_bytes: 0,
                classified,
                remaining: 0,
                complete: true,
                stale: false,
                publication: None,
                classifications,
            });
        }

        let attempted = candidates.len();
        let result = self
            .classify_candidates(Arc::clone(&generation), candidates)
            .await;

        let (publication_inputs, classified, remaining, stale) = {
            let coordinator = self.coordinator();
            if coordinator
                .current
                .as_ref()
                .is_none_or(|current| !Arc::ptr_eq(current, &generation))
            {
                (None, 0, 0, true)
            } else {
                let mut work = generation.work();
                for (txid, outputs) in &result.outputs {
                    insert_outputs(
                        &mut work,
                        *txid,
                        outputs.clone(),
                        self.limits.max_auxiliary_cache_bytes,
                    );
                }
                for (outpoint, script) in &result.confirmed_scripts {
                    insert_confirmed_script(
                        &mut work,
                        *outpoint,
                        script.clone(),
                        self.limits.max_auxiliary_cache_bytes,
                    );
                }
                for cached in &result.classifications {
                    work.classifications
                        .insert(cached.classification.wtxid.clone(), cached.clone());
                }
                if !result.classifications.is_empty() {
                    work.revision = work
                        .revision
                        .checked_add(1)
                        .ok_or(PolicyError::GenerationOverflow)?;
                }
                let classified = work.classifications.len();
                let remaining = generation
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
                let publication_inputs = (!result.classifications.is_empty()).then(|| {
                    (
                        work.revision,
                        current_classifications(&generation.membership, &work.classifications),
                    )
                });
                (publication_inputs, classified, remaining, false)
            }
        };

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
        let classifications = publication
            .as_ref()
            .map_or_else(BTreeMap::new, |publication| {
                publication.observation.classifications.clone()
            });
        Ok(PolicyEnrichmentReport {
            generation: generation.id,
            attempted,
            newly_classified: result.classifications.len(),
            response_failures: result.response_failures,
            batch_failures: result.batch_failures,
            response_bytes: result.response_bytes,
            classified,
            remaining,
            complete: remaining == 0,
            stale,
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

    async fn classify_candidates(
        &self,
        generation: Arc<PolicyGeneration>,
        candidates: Vec<ExpectedVariant>,
    ) -> ClassificationBatch {
        let fetched = self
            .fetch_raw_transactions_concurrently(
                candidates,
                self.limits.rpc_lanes,
                Some(&generation),
            )
            .await;
        if !self.is_current_generation(&generation) {
            return ClassificationBatch {
                response_failures: fetched.response_failures,
                batch_failures: fetched.batch_failures,
                response_bytes: fetched.response_bytes,
                ..ClassificationBatch::default()
            };
        }
        let transactions = fetched
            .values
            .into_iter()
            .filter_map(|(expected, transaction)| {
                transaction.map(|transaction| (expected, transaction))
            })
            .collect::<Vec<_>>();
        if transactions.is_empty() {
            return ClassificationBatch {
                response_failures: fetched.response_failures,
                batch_failures: fetched.batch_failures,
                response_bytes: fetched.response_bytes,
                ..ClassificationBatch::default()
            };
        }

        let mut outputs = transactions
            .iter()
            .filter_map(|(expected, transaction)| {
                expected
                    .txid
                    .parse::<Txid>()
                    .ok()
                    .map(|txid| (txid, cached_outputs(expected, transaction)))
            })
            .collect::<BTreeMap<_, _>>();
        let mut scripts = BTreeMap::new();
        let mut required = BTreeSet::new();
        for outpoint in transactions.iter().flat_map(|(_, transaction)| {
            transaction
                .input
                .iter()
                .filter(|input| !input.previous_output.is_null())
                .map(|input| input.previous_output)
        }) {
            if required.len() >= MAX_UNIQUE_PREVOUTS_PER_SLICE && !required.contains(&outpoint) {
                continue;
            }
            required.insert(outpoint);
        }
        let mut missing_parents = BTreeSet::new();
        let mut missing_confirmed = Vec::new();

        {
            let work = generation.work();
            for outpoint in required {
                if let Some(parent) = generation.variants.get(&outpoint.txid) {
                    let local = outputs
                        .get(&outpoint.txid)
                        .filter(|cached| cached.wtxid == parent.wtxid)
                        .or_else(|| {
                            work.outputs
                                .get(&outpoint.txid)
                                .filter(|cached| cached.wtxid == parent.wtxid)
                        });
                    if let Some(script) = local.and_then(|cached| cached.script(outpoint.vout)) {
                        scripts.insert(outpoint, script);
                    } else {
                        missing_parents.insert(outpoint.txid);
                    }
                } else if let Some(script) = work.confirmed_scripts.get(&outpoint) {
                    scripts.insert(outpoint, script.clone());
                } else if missing_confirmed.len() < maximum_confirmed_prevouts_per_slice() {
                    missing_confirmed.push(outpoint);
                }
            }
        }

        let parent_variants = bounded_variants(
            missing_parents.into_iter().filter_map(|txid| {
                generation
                    .variants
                    .get(&txid)
                    .map(|variant| ExpectedVariant {
                        txid: txid.to_string(),
                        wtxid: variant.wtxid.clone(),
                        vsize: variant.vsize,
                    })
            }),
            MAX_TRANSACTIONS_PER_SLICE,
        );
        if !self.is_current_generation(&generation) {
            return ClassificationBatch {
                response_failures: fetched.response_failures,
                batch_failures: fetched.batch_failures,
                response_bytes: fetched.response_bytes,
                ..ClassificationBatch::default()
            };
        }
        let parent_fetch = self
            .fetch_raw_transactions_concurrently(
                parent_variants,
                self.limits.rpc_lanes,
                Some(&generation),
            )
            .await;
        if !self.is_current_generation(&generation) {
            return ClassificationBatch {
                response_failures: fetched.response_failures + parent_fetch.response_failures,
                batch_failures: fetched.batch_failures + parent_fetch.batch_failures,
                response_bytes: fetched.response_bytes + parent_fetch.response_bytes,
                ..ClassificationBatch::default()
            };
        }
        for (expected, transaction) in parent_fetch.values {
            let Some(transaction) = transaction else {
                continue;
            };
            let Ok(txid) = expected.txid.parse::<Txid>() else {
                continue;
            };
            outputs.insert(txid, cached_outputs(&expected, &transaction));
        }
        for transaction in &transactions {
            for input in &transaction.1.input {
                let outpoint = input.previous_output;
                if scripts.contains_key(&outpoint) {
                    continue;
                }
                let Some(parent) = generation.variants.get(&outpoint.txid) else {
                    continue;
                };
                if let Some(script) = outputs
                    .get(&outpoint.txid)
                    .filter(|cached| cached.wtxid == parent.wtxid)
                    .and_then(|cached| cached.script(outpoint.vout))
                {
                    scripts.insert(outpoint, script);
                }
            }
        }

        let confirmed_fetch = self
            .fetch_confirmed_prevouts_concurrently(
                missing_confirmed,
                self.limits.prevout_rpc_lanes(),
                Some(&generation),
            )
            .await;
        let confirmed_scripts = confirmed_fetch
            .values
            .into_iter()
            .filter_map(|(outpoint, script)| script.map(|script| (outpoint, script)))
            .collect::<BTreeMap<_, _>>();
        scripts.extend(
            confirmed_scripts
                .iter()
                .map(|(outpoint, script)| (*outpoint, script.clone())),
        );

        let classifications = transactions
            .into_iter()
            .map(|(expected, transaction)| {
                let (prevouts, retryable) = if transaction.is_coinbase() {
                    (PrevoutSet::default(), false)
                } else {
                    let facts = transaction
                        .input
                        .iter()
                        .map(|input| {
                            scripts
                                .get(&input.previous_output)
                                .map(|script| PrevoutFacts::new(script.clone(), None))
                        })
                        .collect::<Vec<_>>();
                    let retryable = facts.iter().any(Option::is_none);
                    (PrevoutSet::from_vec(facts), retryable)
                };
                let evidence = evaluate_mempool_policy(&transaction, &prevouts);
                CachedClassification {
                    classification: Arc::new(classification_from_evidence(
                        expected.txid,
                        expected.wtxid,
                        evidence,
                    )),
                    retryable,
                }
            })
            .collect();

        ClassificationBatch {
            classifications,
            outputs,
            confirmed_scripts,
            response_failures: fetched.response_failures
                + parent_fetch.response_failures
                + confirmed_fetch.response_failures,
            batch_failures: fetched.batch_failures
                + parent_fetch.batch_failures
                + confirmed_fetch.batch_failures,
            response_bytes: fetched.response_bytes
                + parent_fetch.response_bytes
                + confirmed_fetch.response_bytes,
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
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok((batch, Ok(values)))) => {
                    fetched.response_failures += values.response_failures;
                    fetched.response_bytes += values.response_bytes;
                    fetched.values.extend(batch.into_iter().zip(values.values));
                }
                Ok(Ok((_, Err(error)))) => {
                    fetched.batch_failures += 1;
                    warn!(error = %error, "BIP-110 raw transaction batch failed");
                }
                Ok(Err(error)) | Err(error) => {
                    fetched.batch_failures += 1;
                    warn!(error = %error, "BIP-110 raw transaction task failed");
                }
            }
            if generation.is_none_or(|generation| self.is_current_generation(generation))
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
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok((batch, Ok(values)))) => {
                    fetched.response_failures += values.response_failures;
                    fetched.response_bytes += values.response_bytes;
                    fetched.values.extend(batch.into_iter().zip(values.values));
                }
                Ok(Ok((_, Err(error)))) => {
                    fetched.batch_failures += 1;
                    warn!(error = %error, "BIP-110 confirmed prevout batch failed");
                }
                Ok(Err(error)) | Err(error) => {
                    fetched.batch_failures += 1;
                    warn!(error = %error, "BIP-110 confirmed prevout task failed");
                }
            }
            if generation.is_none_or(|generation| self.is_current_generation(generation))
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
            .map(|variant| jsonrpc::try_arg(json!([variant.txid, 0])))
            .collect::<Result<Vec<_>, _>>()
            .map_err(PolicyError::EncodeParameters)?;
        let requests = parameters
            .iter()
            .map(|parameters| {
                self.client
                    .build_request("getrawtransaction", Some(parameters))
            })
            .collect::<Vec<_>>();
        let responses = self
            .client
            .send_batch(&requests)
            .map_err(PolicyError::Batch)?;
        let response_bytes = validated_decoded_response_bytes(&responses)?;

        let mut response_failures = 0;
        let values = expected
            .iter()
            .zip(responses)
            .map(|(expected, response)| {
                let transaction = response
                    .ok_or(())
                    .and_then(|response| {
                        let result = valid_rpc_result(&response)?;
                        if result.get().len() > MAX_TRANSACTION_HEX_CHARACTERS + 2 {
                            return Err(());
                        }
                        let hex = serde_json::from_str::<&str>(result.get()).map_err(|_| ())?;
                        if hex.len() > MAX_TRANSACTION_HEX_CHARACTERS {
                            return Err(());
                        }
                        deserialize_hex::<Transaction>(hex).map_err(|_| ())
                    })
                    .and_then(|transaction| {
                        if transaction.compute_txid().to_string() != expected.txid
                            || transaction.compute_wtxid().to_string() != expected.wtxid
                        {
                            return Err(());
                        }
                        Ok(transaction)
                    });
                match transaction {
                    Ok(transaction) => Some(transaction),
                    Err(()) => {
                        response_failures += 1;
                        None
                    }
                }
            })
            .collect();
        Ok(BatchFetch {
            values,
            response_failures,
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
            .map(|outpoint| {
                jsonrpc::try_arg(json!([outpoint.txid.to_string(), outpoint.vout, false]))
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(PolicyError::EncodeParameters)?;
        let requests = parameters
            .iter()
            .map(|parameters| self.client.build_request("gettxout", Some(parameters)))
            .collect::<Vec<_>>();
        let responses = self
            .client
            .send_batch(&requests)
            .map_err(PolicyError::Batch)?;
        let response_bytes = validated_decoded_response_bytes(&responses)?;

        let mut response_failures = 0;
        let values = responses
            .into_iter()
            .map(|response| {
                let script = response.ok_or(()).and_then(|response| {
                    let result = valid_rpc_result(&response)?;
                    if result.get().len() > ESTIMATED_GETTXOUT_RESPONSE_BYTES {
                        return Err(());
                    }
                    let wire = serde_json::from_str::<Option<GetTxOutWire<'_>>>(result.get())
                        .map_err(|_| ())?
                        .ok_or(())?;
                    if wire.script_pub_key.hex.len() > MAX_CONFIRMED_SCRIPT_HEX_CHARACTERS {
                        return Err(());
                    }
                    ScriptBuf::from_hex(wire.script_pub_key.hex).map_err(|_| ())
                });
                match script {
                    Ok(script) => Some(script),
                    Err(()) => {
                        response_failures += 1;
                        None
                    }
                }
            })
            .collect();
        Ok(BatchFetch {
            values,
            response_failures,
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
    pub response_failures: usize,
    pub batch_failures: usize,
    pub response_bytes: usize,
    pub classified: usize,
    pub remaining: usize,
    pub complete: bool,
    pub stale: bool,
    pub(crate) publication: Option<PolicyPublication>,
    pub classifications: TransactionClassifications,
}

impl PolicyEnrichmentReport {
    fn waiting() -> Self {
        Self {
            generation: 0,
            attempted: 0,
            newly_classified: 0,
            response_failures: 0,
            batch_failures: 0,
            response_bytes: 0,
            classified: 0,
            remaining: 0,
            complete: true,
            stale: false,
            publication: None,
            classifications: BTreeMap::new(),
        }
    }

    fn stale(generation: u64) -> Self {
        Self {
            generation,
            attempted: 0,
            newly_classified: 0,
            response_failures: 0,
            batch_failures: 0,
            response_bytes: 0,
            classified: 0,
            remaining: 0,
            complete: false,
            stale: true,
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
    #[error("failed to create policy RPC client: {0}")]
    ClientInitialization(#[source] jsonrpc::minreq_http::Error),
    #[error("failed to encode policy RPC parameters: {0}")]
    EncodeParameters(#[source] serde_json::Error),
    #[error("policy RPC batch failed: {0}")]
    Batch(#[source] jsonrpc::Error),
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
    values: Vec<Option<T>>,
    response_failures: usize,
    response_bytes: usize,
}

#[derive(Debug)]
struct ConcurrentFetch<K, V> {
    values: Vec<(K, Option<V>)>,
    response_failures: usize,
    batch_failures: usize,
    response_bytes: usize,
}

impl<K, V> Default for ConcurrentFetch<K, V> {
    fn default() -> Self {
        Self {
            values: Vec::new(),
            response_failures: 0,
            batch_failures: 0,
            response_bytes: 0,
        }
    }
}

#[derive(Debug, Default)]
struct PolicyCoordinator {
    next_generation: u64,
    current: Option<Arc<PolicyGeneration>>,
}

#[derive(Debug)]
struct PolicyGeneration {
    id: u64,
    membership: Arc<MempoolSnapshot>,
    variants: BTreeMap<Txid, CurrentVariant>,
    work: Mutex<GenerationWork>,
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
    confirmed_scripts: BTreeMap<OutPoint, ScriptBuf>,
    attempted: BTreeSet<String>,
    auxiliary_cache_bytes: usize,
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

#[derive(Debug, Default)]
struct ClassificationBatch {
    classifications: Vec<CachedClassification>,
    outputs: BTreeMap<Txid, CachedOutputs>,
    confirmed_scripts: BTreeMap<OutPoint, ScriptBuf>,
    response_failures: usize,
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

fn insert_outputs(work: &mut GenerationWork, txid: Txid, outputs: CachedOutputs, maximum: usize) {
    let replaced_bytes = work
        .outputs
        .get(&txid)
        .map_or(0, |existing| existing.estimated_bytes);
    let base = work.auxiliary_cache_bytes.saturating_sub(replaced_bytes);
    let Some(new_total) = base.checked_add(outputs.estimated_bytes) else {
        return;
    };
    if new_total > maximum {
        return;
    }
    work.auxiliary_cache_bytes = new_total;
    work.outputs.insert(txid, outputs);
}

fn insert_confirmed_script(
    work: &mut GenerationWork,
    outpoint: OutPoint,
    script: ScriptBuf,
    maximum: usize,
) {
    if work.confirmed_scripts.contains_key(&outpoint) {
        return;
    }
    let estimated_bytes = script
        .len()
        .saturating_add(ESTIMATED_JSON_BYTES_PER_RESPONSE);
    let Some(new_total) = work.auxiliary_cache_bytes.checked_add(estimated_bytes) else {
        return;
    };
    if new_total > maximum {
        return;
    }
    work.auxiliary_cache_bytes = new_total;
    work.confirmed_scripts.insert(outpoint, script);
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

fn validated_decoded_response_bytes(
    responses: &[Option<jsonrpc::Response>],
) -> Result<usize, PolicyError> {
    // jsonrpc's minreq transport has already buffered and parsed the HTTP body
    // here. It exposes no receive-size setting, so this guard prevents further
    // per-result decoding while the process memory cgroup remains the hard
    // transient bound.
    validated_decoded_response_bytes_with_limit(responses, MAX_BATCH_RESPONSE_BYTES)
}

fn validated_decoded_response_bytes_with_limit(
    responses: &[Option<jsonrpc::Response>],
    maximum: usize,
) -> Result<usize, PolicyError> {
    let mut counter = ResponseByteCounter(2);
    for response in responses.iter().flatten() {
        if serde_json::to_writer(&mut counter, response).is_err() {
            counter.0 = usize::MAX;
            break;
        }
        counter.0 = counter.0.saturating_add(1);
    }
    if counter.0 > maximum {
        return Err(PolicyError::BatchResponseTooLarge {
            actual: counter.0,
            maximum,
        });
    }
    Ok(counter.0)
}

struct ResponseByteCounter(usize);

impl std::io::Write for ResponseByteCounter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
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

fn valid_rpc_result(response: &jsonrpc::Response) -> Result<&serde_json::value::RawValue, ()> {
    if response
        .jsonrpc
        .as_deref()
        .is_none_or(|version| version == "2.0")
        && response.error.is_none()
    {
        response.result.as_deref().ok_or(())
    } else {
        Err(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::net::SocketAddr;
    use std::sync::Arc as Shared;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::task::JoinHandle;

    use super::*;

    #[derive(Debug)]
    struct FixtureState {
        fixture: AsyncMutex<RpcFixture>,
        active_requests: AtomicUsize,
        max_active_requests: AtomicUsize,
        delay: Duration,
    }

    #[derive(Debug)]
    struct RpcFixture {
        calls: Vec<(String, Value)>,
        raw_transactions: BTreeMap<String, String>,
        gettxout_results: VecDeque<Value>,
        delay: Duration,
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
                gettxout_results: VecDeque::new(),
                delay: Duration::ZERO,
            }
        }

        fn with_gettxout_results(mut self, results: impl IntoIterator<Item = Value>) -> Self {
            self.gettxout_results = results.into_iter().collect();
            self
        }

        fn with_delay(mut self, delay: Duration) -> Self {
            self.delay = delay;
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
        tokio::time::sleep(state.delay).await;
        let mut fixture = state.fixture.lock().await;
        for request in requests {
            let method = request["method"].as_str().expect("method").to_owned();
            let params = request["params"].clone();
            fixture.calls.push((method.clone(), params.clone()));
            let result = match method.as_str() {
                "getrawtransaction" => {
                    let txid = params[0].as_str().expect("raw transaction txid");
                    Value::String(
                        fixture
                            .raw_transactions
                            .get(txid)
                            .unwrap_or_else(|| panic!("missing raw transaction {txid}"))
                            .clone(),
                    )
                }
                "gettxout" => fixture
                    .gettxout_results
                    .pop_front()
                    .unwrap_or_else(valid_gettxout_result),
                other => panic!("unexpected RPC method {other}"),
            };
            responses.push(json!({
                "jsonrpc": "2.0",
                "result": result,
                "error": null,
                "id": request["id"]
            }));
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
        let state = Shared::new(FixtureState {
            fixture: AsyncMutex::new(fixture),
            active_requests: AtomicUsize::new(0),
            max_active_requests: AtomicUsize::new(0),
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
            [
                "getrawtransaction",
                "gettxout",
                "getrawtransaction",
                "gettxout"
            ]
        );
        assert_eq!(fixture.calls[0].1, json!([txid, 0]));
        assert_eq!(fixture.calls[1].1[2], Value::Bool(false));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn null_prevout_is_typed_retryable_and_replaced_after_retry() {
        let transaction = transaction();
        let txid = transaction.compute_txid().to_string();
        let wtxid = transaction.compute_wtxid().to_string();
        let rpc = RpcFixture::new([transaction.clone()])
            .with_gettxout_results([Value::Null, valid_gettxout_result()]);
        let (fixture, address, server) = start_rpc_fixture(rpc).await;
        let enricher = test_enricher(address);

        let (entries, first) = enrich_entries(&enricher, vec![mempool_entry(&transaction)]).await;
        assert_eq!(first.attempted, 1);
        assert_eq!(first.newly_classified, 1);
        assert_eq!(first.response_failures, 1);
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
            assert!(work.confirmed_scripts.is_empty());
            assert!(work.attempted.is_empty());
            assert_eq!(work.auxiliary_cache_bytes, 0);
        }
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
        let rpc = RpcFixture::new([first_variant, second_variant])
            .with_gettxout_results([Value::Null, valid_gettxout_result()]);
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
        let (fixture, address, server) =
            start_rpc_fixture(RpcFixture::new([transaction.clone()])).await;
        let enricher = test_enricher_with_limits(
            address,
            PolicyLimits::new(1, 1, 1).expect("tiny cache limit"),
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
            let work = generation.work();
            assert_eq!(work.classifications.len(), 1);
            assert!(work.outputs.is_empty());
            assert!(work.confirmed_scripts.is_empty());
            assert!(work.auxiliary_cache_bytes <= 1);
        }
        assert!(!fixture.fixture.lock().await.calls.is_empty());
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
        let rpc = RpcFixture::new([]).with_gettxout_results([json!({
            "scriptPubKey": {
                "hex": oversized_hex
            }
        })]);
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
        assert!(fetch.values[0].is_none());
        assert_eq!(fetch.response_failures, 1);
        assert!(fetch.response_bytes < MAX_BATCH_RESPONSE_BYTES);
        assert_eq!(fixture.fixture.lock().await.calls.len(), 1);
    }

    #[test]
    fn decoded_response_guard_counts_the_complete_response_object() {
        let response = jsonrpc::Response {
            result: Some(
                serde_json::value::RawValue::from_string("\"bounded\"".to_owned())
                    .expect("raw JSON string"),
            ),
            error: None,
            id: json!(1),
            jsonrpc: Some("2.0".to_owned()),
        };

        let error = validated_decoded_response_bytes_with_limit(&[Some(response)], 8)
            .expect_err("complete response exceeds tiny limit");

        assert!(matches!(
            error,
            PolicyError::BatchResponseTooLarge { maximum: 8, .. }
        ));
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
