//! Bounded, best-effort enrichment of one disposable mempool snapshot.
//!
//! The node remains the membership authority. This module only adds current
//! RDTS mempool-policy compatibility facts, caches them by witness transaction
//! ID, and returns explicit gaps when the enrichment budget or RPC data is
//! incomplete.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

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

use crate::model::{
    Bip110Assessment, Bip110RuleDetail, Bip110RuleId, Bip110RuleVerdict, Bip110Status,
    MAX_BIP110_DETAIL_EXEMPLARS_PER_RULE, MempoolEntry, TransactionClassification,
    TransactionClassifications,
};

const RAW_TRANSACTION_BATCH_SIZE: usize = 16;
const GETTXOUT_BATCH_SIZE: usize = 128;
const MAX_TRANSACTION_HEX_CHARACTERS: usize = 8_000_000;
const RPC_BATCH_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PolicyLimits {
    max_transactions_per_snapshot: usize,
    max_duration: Duration,
}

impl PolicyLimits {
    pub fn new(
        max_transactions_per_snapshot: usize,
        max_duration: Duration,
    ) -> Result<Self, PolicyError> {
        if max_transactions_per_snapshot == 0
            || max_duration.is_zero()
            || Instant::now().checked_add(max_duration).is_none()
        {
            return Err(PolicyError::InvalidLimits);
        }
        Ok(Self {
            max_transactions_per_snapshot,
            max_duration,
        })
    }

    pub(crate) fn max_transactions_per_snapshot(self) -> usize {
        self.max_transactions_per_snapshot
    }
}

#[derive(Clone, Debug)]
pub struct PolicyEnricher {
    client: Arc<JsonRpcClient>,
    cache: Arc<Mutex<BTreeMap<String, CachedClassification>>>,
    candidate_cursor: Arc<Mutex<Option<String>>>,
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
            cache: Arc::new(Mutex::new(BTreeMap::new())),
            candidate_cursor: Arc::new(Mutex::new(None)),
            limits,
        })
    }

    /// Adds every cached result, then attempts a bounded number of new or
    /// incomplete witness variants. A missing raw transaction remains
    /// unclassified. Missing prevouts produce retryable typed unknowns. Neither
    /// case invalidates the underlying membership snapshot.
    pub fn enrich(&self, entries: &mut [MempoolEntry]) -> PolicyEnrichmentReport {
        let started_at = Instant::now();
        let deadline = started_at
            .checked_add(self.limits.max_duration)
            .unwrap_or(started_at);
        let membership = entries
            .iter()
            .filter_map(|entry| {
                entry
                    .txid
                    .parse::<Txid>()
                    .ok()
                    .map(|txid| (txid, entry.wtxid.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        let current_variants = entries
            .iter()
            .map(|entry| (entry.wtxid.clone(), entry.txid.clone()))
            .collect::<BTreeMap<_, _>>();

        {
            let mut cache = self.cache();
            cache.retain(|wtxid, result| {
                current_variants
                    .get(wtxid)
                    .is_some_and(|txid| txid == &result.classification.txid)
            });
        }
        self.apply_cache(entries);

        let candidates = {
            let cursor = self.candidate_cursor();
            let start = cursor.as_deref().map_or(0, |txid| {
                entries.partition_point(|entry| entry.txid.as_str() <= txid)
            });
            drop(cursor);
            let cache = self.cache();
            let candidates = entries[start..]
                .iter()
                .chain(entries[..start].iter())
                .filter(|entry| {
                    cache.get(&entry.wtxid).is_none_or(|cached| {
                        cached.classification.txid != entry.txid || cached.retryable
                    })
                })
                .take(self.limits.max_transactions_per_snapshot)
                .map(|entry| ExpectedVariant {
                    txid: entry.txid.clone(),
                    wtxid: entry.wtxid.clone(),
                })
                .collect::<Vec<_>>();
            drop(cache);
            candidates
        };
        let mut report = PolicyEnrichmentReport {
            attempted: 0,
            newly_classified: 0,
            response_failures: 0,
            batch_failures: 0,
            deadline_reached: false,
            classifications: BTreeMap::new(),
        };

        for expected_batch in candidates.chunks(RAW_TRANSACTION_BATCH_SIZE) {
            if Instant::now() >= deadline {
                report.deadline_reached = true;
                break;
            }
            report.attempted += expected_batch.len();
            if let Some(last) = expected_batch.last() {
                *self.candidate_cursor() = Some(last.txid.clone());
            }
            let fetched = match self.fetch_raw_transactions(expected_batch) {
                Ok(fetched) => fetched,
                Err(error) => {
                    report.batch_failures += 1;
                    warn!(error = %error, "BIP-110 raw transaction batch failed");
                    break;
                }
            };
            report.response_failures += fetched.response_failures;

            let transactions = expected_batch
                .iter()
                .zip(fetched.values)
                .filter_map(|(expected, transaction)| {
                    transaction.map(|transaction| (expected, transaction))
                })
                .collect::<Vec<_>>();
            if transactions.is_empty() {
                continue;
            }

            let resolution = self.resolve_prevouts(&transactions, &membership, deadline);
            report.response_failures += resolution.response_failures;
            report.batch_failures += resolution.batch_failures;
            report.deadline_reached |= resolution.deadline_reached;

            for (expected, transaction) in transactions {
                let (prevouts, retryable) = if transaction.is_coinbase() {
                    (PrevoutSet::default(), false)
                } else {
                    let facts = transaction
                        .input
                        .iter()
                        .map(|input| {
                            resolution
                                .scripts
                                .get(&input.previous_output)
                                .map(|script| PrevoutFacts::new(script.clone(), None))
                        })
                        .collect::<Vec<_>>();
                    let retryable = facts.iter().any(Option::is_none);
                    (PrevoutSet::from_vec(facts), retryable)
                };

                let evidence = evaluate_mempool_policy(&transaction, &prevouts);
                let classification = Arc::new(classification_from_evidence(
                    expected.txid.clone(),
                    expected.wtxid.clone(),
                    evidence,
                ));
                self.cache().insert(
                    expected.wtxid.clone(),
                    CachedClassification {
                        classification,
                        retryable,
                    },
                );
                report.newly_classified += 1;
            }

            if resolution.stop_requested {
                break;
            }
        }

        self.apply_cache(entries);
        report.classifications = self.current_classifications(entries);
        report
    }

    fn fetch_raw_transactions(
        &self,
        expected: &[ExpectedVariant],
    ) -> Result<BatchFetch<Transaction>, PolicyError> {
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

        let mut response_failures = 0;
        let values = expected
            .iter()
            .zip(responses)
            .map(|(expected, response)| {
                let transaction = response
                    .ok_or(())
                    .and_then(valid_rpc_response)
                    .and_then(|response| response.result::<String>().map_err(|_| ()))
                    .and_then(|hex| {
                        if hex.len() > MAX_TRANSACTION_HEX_CHARACTERS {
                            return Err(());
                        }
                        deserialize_hex::<Transaction>(&hex).map_err(|_| ())
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
        })
    }

    fn fetch_parent_transactions(
        &self,
        expected: &[(Txid, String)],
    ) -> Result<BatchFetch<Transaction>, PolicyError> {
        let variants = expected
            .iter()
            .map(|(txid, wtxid)| ExpectedVariant {
                txid: txid.to_string(),
                wtxid: wtxid.clone(),
            })
            .collect::<Vec<_>>();
        self.fetch_raw_transactions(&variants)
    }

    fn fetch_confirmed_prevouts(
        &self,
        outpoints: &[OutPoint],
    ) -> Result<BatchFetch<ScriptBuf>, PolicyError> {
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

        let mut response_failures = 0;
        let values = responses
            .into_iter()
            .map(|response| {
                let script = response
                    .ok_or(())
                    .and_then(valid_rpc_response)
                    .and_then(|response| response.result::<Option<GetTxOutWire>>().map_err(|_| ()))
                    .and_then(|wire| wire.ok_or(()))
                    .and_then(|wire| ScriptBuf::from_hex(&wire.script_pub_key.hex).map_err(|_| ()));
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
        })
    }

    fn resolve_prevouts(
        &self,
        transactions: &[(&ExpectedVariant, Transaction)],
        membership: &BTreeMap<Txid, String>,
        deadline: Instant,
    ) -> PrevoutResolution {
        let required = transactions
            .iter()
            .flat_map(|(_, transaction)| {
                transaction
                    .input
                    .iter()
                    .filter(|input| !input.previous_output.is_null())
                    .map(|input| input.previous_output)
            })
            .collect::<BTreeSet<_>>();
        let mut parent_outpoints = BTreeMap::<Txid, Vec<OutPoint>>::new();
        let mut confirmed_outpoints = Vec::new();
        for outpoint in required {
            if membership.contains_key(&outpoint.txid) {
                parent_outpoints
                    .entry(outpoint.txid)
                    .or_default()
                    .push(outpoint);
            } else {
                confirmed_outpoints.push(outpoint);
            }
        }

        let mut resolution = PrevoutResolution::default();
        let expected_parents = parent_outpoints
            .keys()
            .filter_map(|txid| membership.get(txid).map(|wtxid| (*txid, wtxid.clone())))
            .collect::<Vec<_>>();
        for parent_batch in expected_parents.chunks(RAW_TRANSACTION_BATCH_SIZE) {
            if Instant::now() >= deadline {
                resolution.deadline_reached = true;
                resolution.stop_requested = true;
                return resolution;
            }
            let fetched = match self.fetch_parent_transactions(parent_batch) {
                Ok(fetched) => fetched,
                Err(error) => {
                    resolution.batch_failures += 1;
                    resolution.stop_requested = true;
                    warn!(error = %error, "BIP-110 mempool parent batch failed");
                    return resolution;
                }
            };
            resolution.response_failures += fetched.response_failures;
            for ((parent_txid, _), transaction) in parent_batch.iter().zip(fetched.values) {
                let Some(transaction) = transaction else {
                    continue;
                };
                for outpoint in &parent_outpoints[parent_txid] {
                    if let Some(output) = transaction.output.get(outpoint.vout as usize) {
                        resolution
                            .scripts
                            .insert(*outpoint, output.script_pubkey.clone());
                    } else {
                        resolution.response_failures += 1;
                    }
                }
            }
        }

        for outpoint_batch in confirmed_outpoints.chunks(GETTXOUT_BATCH_SIZE) {
            if Instant::now() >= deadline {
                resolution.deadline_reached = true;
                resolution.stop_requested = true;
                return resolution;
            }
            let fetched = match self.fetch_confirmed_prevouts(outpoint_batch) {
                Ok(fetched) => fetched,
                Err(error) => {
                    resolution.batch_failures += 1;
                    resolution.stop_requested = true;
                    warn!(error = %error, "BIP-110 confirmed prevout batch failed");
                    return resolution;
                }
            };
            resolution.response_failures += fetched.response_failures;
            for (outpoint, script) in outpoint_batch.iter().zip(fetched.values) {
                if let Some(script) = script {
                    resolution.scripts.insert(*outpoint, script);
                }
            }
        }
        resolution
    }

    fn apply_cache(&self, entries: &mut [MempoolEntry]) {
        let cache = self.cache();
        for entry in entries {
            entry.bip110 = cache
                .get(&entry.wtxid)
                .filter(|cached| cached.classification.txid == entry.txid)
                .map(|cached| cached.classification.assessment.clone());
        }
    }

    fn current_classifications(&self, entries: &[MempoolEntry]) -> TransactionClassifications {
        let cache = self.cache();
        entries
            .iter()
            .filter_map(|entry| {
                cache
                    .get(&entry.wtxid)
                    .filter(|cached| cached.classification.txid == entry.txid)
                    .map(|cached| (entry.txid.clone(), Arc::clone(&cached.classification)))
            })
            .collect()
    }

    fn cache(&self) -> MutexGuard<'_, BTreeMap<String, CachedClassification>> {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn candidate_cursor(&self) -> MutexGuard<'_, Option<String>> {
        self.candidate_cursor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Debug)]
pub struct PolicyEnrichmentReport {
    pub attempted: usize,
    pub newly_classified: usize,
    pub response_failures: usize,
    pub batch_failures: usize,
    pub deadline_reached: bool,
    pub classifications: TransactionClassifications,
}

#[derive(Debug, Error)]
pub enum PolicyError {
    #[error("policy enrichment limits must be greater than zero")]
    InvalidLimits,
    #[error("failed to create policy RPC client: {0}")]
    ClientInitialization(#[source] jsonrpc::minreq_http::Error),
    #[error("failed to encode policy RPC parameters: {0}")]
    EncodeParameters(#[source] serde_json::Error),
    #[error("policy RPC batch failed: {0}")]
    Batch(#[source] jsonrpc::Error),
}

#[derive(Debug)]
struct ExpectedVariant {
    txid: String,
    wtxid: String,
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
}

#[derive(Debug, Default)]
struct PrevoutResolution {
    scripts: BTreeMap<OutPoint, ScriptBuf>,
    response_failures: usize,
    batch_failures: usize,
    deadline_reached: bool,
    stop_requested: bool,
}

#[derive(Debug, Deserialize)]
struct GetTxOutWire {
    #[serde(rename = "scriptPubKey")]
    script_pub_key: ScriptPubKeyWire,
}

#[derive(Debug, Deserialize)]
struct ScriptPubKeyWire {
    hex: String,
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

fn valid_rpc_response(response: jsonrpc::Response) -> Result<jsonrpc::Response, ()> {
    if response
        .jsonrpc
        .as_deref()
        .is_none_or(|version| version == "2.0")
    {
        Ok(response)
    } else {
        Err(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::net::SocketAddr;
    use std::sync::Arc as Shared;

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

    type FixtureState = Shared<AsyncMutex<RpcFixture>>;

    #[derive(Debug)]
    struct RpcFixture {
        calls: Vec<(String, Value)>,
        raw_transactions: BTreeMap<String, String>,
        gettxout_results: VecDeque<Value>,
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
            }
        }

        fn with_gettxout_results(mut self, results: impl IntoIterator<Item = Value>) -> Self {
            self.gettxout_results = results.into_iter().collect();
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
        State(state): State<FixtureState>,
        Json(request): Json<Value>,
    ) -> Json<Value> {
        let requests = request.as_array().expect("batch request");
        let mut responses = Vec::with_capacity(requests.len());
        let mut fixture = state.lock().await;
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
        Json(Value::Array(responses))
    }

    fn valid_gettxout_result() -> Value {
        json!({
            "scriptPubKey": {
                "hex": "00140000000000000000000000000000000000000000"
            }
        })
    }

    async fn start_rpc_fixture(fixture: RpcFixture) -> (FixtureState, SocketAddr, JoinHandle<()>) {
        let state = Shared::new(AsyncMutex::new(fixture));
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
            PolicyLimits::new(10, Duration::from_secs(5)).expect("limits"),
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
            .cache()
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
        let fixture = fixture.lock().await;
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
            let cache = enricher.cache();
            assert!(
                cache
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
            let cache = enricher.cache();
            assert!(
                !cache
                    .get(&wtxid)
                    .expect("complete classification")
                    .retryable
            );
        }
        let fixture = fixture.lock().await;
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

        let fixture = fixture.lock().await;
        let parent_txid = parent_txid.to_string();
        assert_eq!(
            fixture
                .calls
                .iter()
                .filter(|(method, params)| {
                    method == "getrawtransaction" && params[0] == parent_txid
                })
                .count(),
            2,
            "the parent is fetched once as a member and once to resolve the child prevout"
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
        let cache = enricher.cache();
        assert_eq!(cache.len(), 1);
        assert!(!cache.contains_key(&first_wtxid));
        assert_eq!(
            cache
                .get(&second_wtxid)
                .expect("replacement witness variant")
                .classification
                .wtxid,
            second_wtxid
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn round_robin_prevents_retryable_entries_from_starving_fresh_members() {
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
            PolicyLimits::new(1, Duration::from_secs(5)).expect("limits"),
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
            "the retryable cached result remains visible while the cursor advances"
        );
        let fixture = fixture.lock().await;
        let fetched_txids = fixture
            .calls
            .iter()
            .filter(|(method, _)| method == "getrawtransaction")
            .map(|(_, parameters)| parameters[0].as_str().expect("txid").to_owned())
            .collect::<Vec<_>>();
        assert_eq!(fetched_txids, [first_txid, second_txid]);
    }

    #[test]
    fn policy_limits_must_be_nonzero() {
        assert!(PolicyLimits::new(0, Duration::from_secs(1)).is_err());
        assert!(PolicyLimits::new(1, Duration::ZERO).is_err());
        assert!(PolicyLimits::new(1, Duration::MAX).is_err());
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
