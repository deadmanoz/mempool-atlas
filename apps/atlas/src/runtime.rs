use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use thiserror::Error;
use tokio::sync::{Notify, RwLock};
use tracing::{info, warn};

use crate::model::{
    MempoolObservation, MempoolSnapshot, ModelError, SourceAvailability, SourceSnapshotResponse,
    SourceSummary, SourcesResponse, TransactionClassifications, TransactionDetailResponse,
    validate_source_id, validate_source_label,
};
use crate::policy::{PolicyEnricher, PolicyError, PolicyPublication};
use crate::rpc::{RpcClient, RpcError};

#[derive(Debug)]
pub struct SourceRuntime {
    source_id: String,
    source_label: String,
    poll_interval: Duration,
    state: RwLock<RuntimeState>,
    classification_wakeup: Notify,
}

#[derive(Debug, Default)]
struct RuntimeState {
    last_poll_started_at_ms: Option<u64>,
    latest: Option<Arc<MempoolSnapshot>>,
    classifications: TransactionClassifications,
    last_error: Option<String>,
    cached_response: Option<Bytes>,
    policy_generation: Option<u64>,
    policy_revision: u64,
    status_revision: u64,
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
            source_id,
            source_label,
            poll_interval,
            state: RwLock::new(RuntimeState::default()),
            classification_wakeup: Notify::new(),
        })
    }

    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    pub fn source_label(&self) -> &str {
        &self.source_label
    }

    pub async fn run(self: Arc<Self>, rpc: RpcClient, policy: PolicyEnricher) {
        tokio::join!(
            Arc::clone(&self).run_membership(rpc, policy.clone()),
            self.run_classification(policy)
        );
    }

    async fn run_membership(self: Arc<Self>, rpc: RpcClient, policy: PolicyEnricher) {
        let mut interval = tokio::time::interval(self.poll_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            if let Err(error) = self.poll_once(&rpc, &policy).await {
                warn!(
                    source_id = %self.source_id,
                    error = %error,
                    "mempool snapshot poll failed; retaining the last good snapshot"
                );
            }
        }
    }

    async fn run_classification(&self, policy: PolicyEnricher) {
        loop {
            self.classification_wakeup.notified().await;
            loop {
                let slice_started_at = Instant::now();
                let expected_generation = self.state.read().await.policy_generation;
                let report = match policy
                    .classify_next_for_generation(expected_generation)
                    .await
                {
                    Ok(report) => report,
                    Err(error) => {
                        warn!(
                            source_id = %self.source_id,
                            error = %error,
                            elapsed_ms = slice_started_at.elapsed().as_millis(),
                            "BIP-110 classification slice failed"
                        );
                        break;
                    }
                };
                info!(
                    source_id = %self.source_id,
                    generation = report.generation,
                    attempted = report.attempted,
                    newly_classified = report.newly_classified,
                    response_failures = report.response_failures,
                    batch_failures = report.batch_failures,
                    response_bytes = report.response_bytes,
                    elapsed_ms = slice_started_at.elapsed().as_millis(),
                    classified = report.classified,
                    remaining = report.remaining,
                    complete = report.complete,
                    stale = report.stale,
                    "completed BIP-110 classification slice"
                );
                if let Some(publication) = report.publication {
                    match self.record_policy_progress(publication).await {
                        Ok(true) => {}
                        Ok(false) => break,
                        Err(error) => {
                            warn!(
                                source_id = %self.source_id,
                                error = %error,
                                "failed to publish BIP-110 classification progress"
                            );
                            break;
                        }
                    }
                }
                if report.stale {
                    break;
                }
                let no_progress_failure = report.newly_classified == 0
                    && (report.batch_failures > 0
                        || (report.attempted > 0 && report.response_failures >= report.attempted));
                if no_progress_failure {
                    warn!(
                        source_id = %self.source_id,
                        generation = report.generation,
                        response_failures = report.response_failures,
                        batch_failures = report.batch_failures,
                        "pausing BIP-110 classification until the next membership generation"
                    );
                    break;
                }
                if report.complete {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }
    }

    pub async fn poll_once(
        &self,
        rpc: &RpcClient,
        policy: &PolicyEnricher,
    ) -> Result<(), RuntimeError> {
        let started_at_ms = system_now_ms()?;
        self.record_poll_started(started_at_ms).await;
        match rpc
            .get_mempool_snapshot(&self.source_id, &self.source_label)
            .await
        {
            Ok(snapshot) => {
                let transaction_count = snapshot.transaction_count;
                let observed_at_ms = snapshot.observed_at_ms;
                let publication = policy.install_snapshot(snapshot)?;
                let classified = publication
                    .observation
                    .snapshot
                    .bip110_summary
                    .compatible_count
                    + publication
                        .observation
                        .snapshot
                        .bip110_summary
                        .violating_count
                    + publication
                        .observation
                        .snapshot
                        .bip110_summary
                        .indeterminate_count;
                self.record_membership(publication).await?;
                self.classification_wakeup.notify_one();
                info!(
                    source_id = %self.source_id,
                    transaction_count,
                    classified,
                    observed_at_ms,
                    "published complete mempool membership"
                );
                Ok(())
            }
            Err(error) => {
                self.record_failure(error.public_message().to_owned())
                    .await?;
                Err(RuntimeError::Rpc(error))
            }
        }
    }

    pub async fn summary(&self) -> SourceSummary {
        let state = self.state.read().await;
        summary_from_state(self, &state)
    }

    pub async fn snapshot_response(&self) -> SourceSnapshotResponse {
        let state = self.state.read().await;
        SourceSnapshotResponse {
            source: summary_from_state(self, &state),
            snapshot: state.latest.clone(),
        }
    }

    pub async fn snapshot_response_bytes(&self) -> Result<Bytes, RuntimeError> {
        let state = self.state.read().await;
        if let Some(response) = &state.cached_response {
            return Ok(response.clone());
        }
        let response = SourceSnapshotResponse {
            source: summary_from_state(self, &state),
            snapshot: state.latest.clone(),
        };
        drop(state);
        encode_response(response).await
    }

    pub(crate) async fn record_poll_started(&self, started_at_ms: u64) {
        let mut state = self.state.write().await;
        state.last_poll_started_at_ms = Some(started_at_ms);
        state.status_revision = state.status_revision.wrapping_add(1);
    }

    #[cfg(test)]
    pub(crate) async fn record_success(
        &self,
        observation: MempoolObservation,
    ) -> Result<(), RuntimeError> {
        let generation = self
            .state
            .read()
            .await
            .policy_generation
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(RuntimeError::PolicyGenerationOverflow)?;
        self.record_membership(PolicyPublication {
            generation,
            revision: 0,
            observation,
        })
        .await
    }

    async fn record_membership(&self, publication: PolicyPublication) -> Result<(), RuntimeError> {
        let publication_revision = publication.revision;
        let MempoolObservation {
            snapshot,
            classifications,
        } = publication.observation;
        if snapshot.source_id != self.source_id {
            return Err(RuntimeError::SnapshotSourceMismatch {
                expected: self.source_id.clone(),
                actual: snapshot.source_id,
            });
        }
        if snapshot.classification_revision != publication_revision {
            return Err(RuntimeError::PolicyRevisionMismatch {
                publication: publication_revision,
                snapshot: snapshot.classification_revision,
            });
        }

        let latest = Arc::new(snapshot);
        let last_poll_started_at_ms = self.state.read().await.last_poll_started_at_ms;
        let response = SourceSnapshotResponse {
            source: summary_from_parts(self, last_poll_started_at_ms, Some(&latest), None),
            snapshot: Some(Arc::clone(&latest)),
        };
        let cached_response = encode_response(response).await?;

        let mut state = self.state.write().await;
        state.latest = Some(latest);
        state.classifications = classifications;
        state.last_error = None;
        state.cached_response = Some(cached_response);
        state.policy_generation = Some(publication.generation);
        state.policy_revision = publication.revision;
        state.status_revision = state.status_revision.wrapping_add(1);
        Ok(())
    }

    async fn record_policy_progress(
        &self,
        publication: PolicyPublication,
    ) -> Result<bool, RuntimeError> {
        if publication.observation.snapshot.source_id != self.source_id {
            return Err(RuntimeError::SnapshotSourceMismatch {
                expected: self.source_id.clone(),
                actual: publication.observation.snapshot.source_id,
            });
        }
        if publication.observation.snapshot.classification_revision != publication.revision {
            return Err(RuntimeError::PolicyRevisionMismatch {
                publication: publication.revision,
                snapshot: publication.observation.snapshot.classification_revision,
            });
        }
        let latest = Arc::new(publication.observation.snapshot);
        let classifications = publication.observation.classifications;

        loop {
            let (
                status_revision,
                last_poll_started_at_ms,
                last_error,
                current_generation,
                current_revision,
            ) = {
                let state = self.state.read().await;
                (
                    state.status_revision,
                    state.last_poll_started_at_ms,
                    state.last_error.clone(),
                    state.policy_generation,
                    state.policy_revision,
                )
            };
            if current_generation != Some(publication.generation)
                || current_revision >= publication.revision
            {
                return Ok(false);
            }
            let response = SourceSnapshotResponse {
                source: summary_from_parts(
                    self,
                    last_poll_started_at_ms,
                    Some(&latest),
                    last_error.as_deref(),
                ),
                snapshot: Some(Arc::clone(&latest)),
            };
            let cached_response = encode_response(response).await?;

            let mut state = self.state.write().await;
            if state.status_revision != status_revision
                || state.policy_generation != Some(publication.generation)
            {
                continue;
            }
            if state.policy_revision >= publication.revision {
                return Ok(false);
            }
            state.latest = Some(Arc::clone(&latest));
            state.classifications = classifications.clone();
            state.cached_response = Some(cached_response);
            state.policy_revision = publication.revision;
            return Ok(true);
        }
    }

    pub(crate) async fn record_failure(&self, error: String) -> Result<(), RuntimeError> {
        loop {
            let (
                status_revision,
                policy_generation,
                policy_revision,
                last_poll_started_at_ms,
                latest,
            ) = {
                let state = self.state.read().await;
                (
                    state.status_revision,
                    state.policy_generation,
                    state.policy_revision,
                    state.last_poll_started_at_ms,
                    state.latest.clone(),
                )
            };
            let response = SourceSnapshotResponse {
                source: summary_from_parts(
                    self,
                    last_poll_started_at_ms,
                    latest.as_ref(),
                    Some(&error),
                ),
                snapshot: latest,
            };
            let cached_response = encode_response(response).await?;

            let mut state = self.state.write().await;
            if state.status_revision != status_revision
                || state.policy_generation != policy_generation
                || state.policy_revision != policy_revision
            {
                continue;
            }
            state.last_error = Some(error);
            state.cached_response = Some(cached_response);
            state.status_revision = state.status_revision.wrapping_add(1);
            return Ok(());
        }
    }

    pub async fn transaction_detail(&self, txid: &str) -> TransactionLookup {
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
            classification_revision: state.policy_revision,
            txid: classification.txid.clone(),
            wtxid: classification.wtxid.clone(),
            assessment: classification.assessment.clone(),
            rules: classification.rules.clone(),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransactionLookup {
    Ready(TransactionDetailResponse),
    WaitingForSnapshot,
    NotPresent,
    Unclassified,
}

fn summary_from_state(runtime: &SourceRuntime, state: &RuntimeState) -> SourceSummary {
    summary_from_parts(
        runtime,
        state.last_poll_started_at_ms,
        state.latest.as_ref(),
        state.last_error.as_deref(),
    )
}

fn summary_from_parts(
    runtime: &SourceRuntime,
    last_poll_started_at_ms: Option<u64>,
    latest: Option<&Arc<MempoolSnapshot>>,
    last_error: Option<&str>,
) -> SourceSummary {
    let availability = match (latest, last_error) {
        (None, None) => SourceAvailability::Waiting,
        (None, Some(_)) => SourceAvailability::Error,
        (Some(_), None) => SourceAvailability::Ready,
        (Some(_), Some(_)) => SourceAvailability::Stale,
    };
    SourceSummary {
        source_id: runtime.source_id.clone(),
        source_label: runtime.source_label.clone(),
        availability,
        poll_interval_seconds: runtime.poll_interval.as_secs(),
        last_poll_started_at_ms,
        snapshot_observed_at_ms: latest.map(|snapshot| snapshot.observed_at_ms),
        chain_tip: latest.map(|snapshot| snapshot.chain_tip.clone()),
        transaction_count: latest.map(|snapshot| snapshot.transaction_count),
        total_vsize: latest.map(|snapshot| snapshot.total_vsize),
        last_error: last_error.map(str::to_owned),
    }
}

async fn encode_response(response: SourceSnapshotResponse) -> Result<Bytes, RuntimeError> {
    let encoded = tokio::task::spawn_blocking(move || serde_json::to_vec(&response)).await??;
    Ok(Bytes::from(encoded))
}

#[derive(Clone, Debug)]
pub struct SourceRegistry {
    sources: Arc<BTreeMap<String, Arc<SourceRuntime>>>,
}

impl SourceRegistry {
    pub fn new(sources: Vec<Arc<SourceRuntime>>) -> Result<Self, RuntimeError> {
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
        SourcesResponse { sources }
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
    Policy(#[from] PolicyError),
    #[error("poll interval must be greater than zero")]
    ZeroPollInterval,
    #[error("at least one source is required")]
    NoSources,
    #[error("source {0:?} is configured more than once")]
    DuplicateSource(String),
    #[error("snapshot source {actual:?} does not match runtime source {expected:?}")]
    SnapshotSourceMismatch { expected: String, actual: String },
    #[error("policy generation counter overflowed")]
    PolicyGenerationOverflow,
    #[error(
        "policy publication revision {publication} does not match snapshot revision {snapshot}"
    )]
    PolicyRevisionMismatch { publication: u64, snapshot: u64 },
    #[error("snapshot response worker failed: {0}")]
    SnapshotResponseTask(#[from] tokio::task::JoinError),
    #[error("snapshot response could not be encoded: {0}")]
    SnapshotResponseEncoding(#[from] serde_json::Error),
    #[error("system clock is before the Unix epoch")]
    InvalidSystemClock,
    #[error("system time cannot be represented in milliseconds")]
    SystemTimeOverflow,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::{Json, Router, extract::State, routing::post};
    use bitcoin::{
        Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness, absolute,
        consensus::encode::serialize_hex, transaction::Version,
    };
    use serde_json::{Value, json};
    use tokio::sync::{Notify, Semaphore};

    use super::*;
    use crate::model::{ChainTip, MempoolEntry, TransactionClassification};
    use crate::{
        Bip110Assessment, Bip110RuleDetail, Bip110RuleId, Bip110RuleVerdict, Bip110Status,
        PolicyLimits,
    };

    #[derive(Debug)]
    struct BlockingPolicyFixture {
        raw_transactions: BTreeMap<String, String>,
        raw_batches_started: AtomicUsize,
        raw_batch_started: Notify,
        release_raw_batch: Arc<Semaphore>,
    }

    #[derive(Debug)]
    struct FailingPolicyFixture {
        raw_batches_started: AtomicUsize,
        raw_batch_started: Notify,
    }

    async fn blocking_policy_rpc(
        State(fixture): State<Arc<BlockingPolicyFixture>>,
        Json(request): Json<Value>,
    ) -> Json<Value> {
        let requests = request.as_array().expect("batch request");
        let blocks_on_raw = requests.iter().any(|request| {
            request["method"]
                .as_str()
                .is_some_and(|method| method == "getrawtransaction")
        });
        if blocks_on_raw {
            fixture.raw_batches_started.fetch_add(1, Ordering::SeqCst);
            fixture.raw_batch_started.notify_waiters();
            fixture
                .release_raw_batch
                .acquire()
                .await
                .expect("test fixture release semaphore remains open")
                .forget();
        }

        let responses = requests
            .iter()
            .map(|request| {
                let method = request["method"].as_str().expect("method");
                let params = &request["params"];
                let result = match method {
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
                    "gettxout" => json!({
                        "scriptPubKey": {
                            "hex": "00140000000000000000000000000000000000000000"
                        }
                    }),
                    other => panic!("unexpected RPC method {other}"),
                };
                json!({
                    "jsonrpc": "2.0",
                    "result": result,
                    "error": null,
                    "id": request["id"]
                })
            })
            .collect::<Vec<_>>();
        Json(Value::Array(responses))
    }

    async fn start_blocking_policy_fixture(
        transactions: &[Transaction],
    ) -> (
        Arc<BlockingPolicyFixture>,
        std::net::SocketAddr,
        tokio::task::JoinHandle<()>,
    ) {
        let fixture = Arc::new(BlockingPolicyFixture {
            raw_transactions: transactions
                .iter()
                .map(|transaction| {
                    (
                        transaction.compute_txid().to_string(),
                        serialize_hex(transaction),
                    )
                })
                .collect(),
            raw_batches_started: AtomicUsize::new(0),
            raw_batch_started: Notify::new(),
            release_raw_batch: Arc::new(Semaphore::new(0)),
        });
        let application = Router::new()
            .route("/", post(blocking_policy_rpc))
            .with_state(Arc::clone(&fixture));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move {
            axum::serve(listener, application)
                .await
                .expect("fixture server");
        });
        (fixture, address, server)
    }

    async fn start_failing_policy_fixture() -> (
        Arc<FailingPolicyFixture>,
        std::net::SocketAddr,
        tokio::task::JoinHandle<()>,
    ) {
        async fn fail_policy_rpc(
            State(fixture): State<Arc<FailingPolicyFixture>>,
            Json(request): Json<Value>,
        ) -> Json<Value> {
            fixture.raw_batches_started.fetch_add(1, Ordering::SeqCst);
            fixture.raw_batch_started.notify_waiters();
            Json(Value::Array(
                request
                    .as_array()
                    .expect("batch request")
                    .iter()
                    .map(|request| {
                        json!({
                            "jsonrpc": "2.0",
                            "result": null,
                            "error": {
                                "code": -32601,
                                "message": "classification method unavailable"
                            },
                            "id": request["id"]
                        })
                    })
                    .collect(),
            ))
        }

        let fixture = Arc::new(FailingPolicyFixture {
            raw_batches_started: AtomicUsize::new(0),
            raw_batch_started: Notify::new(),
        });
        let application = Router::new()
            .route("/", post(fail_policy_rpc))
            .with_state(Arc::clone(&fixture));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move {
            axum::serve(listener, application)
                .await
                .expect("fixture server");
        });
        (fixture, address, server)
    }

    async fn wait_for_raw_batches(fixture: &BlockingPolicyFixture, expected: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let notified = fixture.raw_batch_started.notified();
                if fixture.raw_batches_started.load(Ordering::SeqCst) >= expected {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("classification request reached the fixture");
    }

    async fn wait_for_classified(runtime: &SourceRuntime, expected: u64) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let snapshot = runtime
                    .snapshot_response()
                    .await
                    .snapshot
                    .expect("published snapshot");
                if snapshot.bip110_summary.compatible_count == expected {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("classification progress was published");
    }

    async fn wait_for_failing_raw_batches(fixture: &FailingPolicyFixture, expected: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let notified = fixture.raw_batch_started.notified();
                if fixture.raw_batches_started.load(Ordering::SeqCst) >= expected {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("classification request reached the failing fixture");
    }

    fn runtime() -> SourceRuntime {
        SourceRuntime::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            Duration::from_secs(60),
        )
        .expect("runtime")
    }

    fn snapshot(observed_at_ms: u64) -> MempoolSnapshot {
        snapshot_for("00".repeat(32), observed_at_ms)
    }

    fn snapshot_for(txid: String, observed_at_ms: u64) -> MempoolSnapshot {
        MempoolSnapshot::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            observed_at_ms,
            ChainTip {
                height: 900_000,
                hash: "00".repeat(32),
            },
            vec![
                MempoolEntry::new(txid, 141, 1_200, observed_at_ms.saturating_sub(1_000))
                    .expect("entry"),
            ],
        )
        .expect("snapshot")
    }

    fn observation(observed_at_ms: u64) -> MempoolObservation {
        MempoolObservation::new(snapshot(observed_at_ms), BTreeMap::new()).expect("observation")
    }

    fn compatible_observation(txid: String, observed_at_ms: u64) -> MempoolObservation {
        let classification = Arc::new(TransactionClassification {
            txid: txid.clone(),
            wtxid: txid.clone(),
            assessment: Bip110Assessment {
                status: Bip110Status::Compatible,
                primary_rule: None,
                violated_rules: Vec::new(),
                unknown_rules: Vec::new(),
            },
            rules: Bip110RuleId::ALL
                .into_iter()
                .map(|rule| Bip110RuleDetail {
                    rule,
                    number: rule.number(),
                    verdict: Bip110RuleVerdict::Pass,
                    evidence_count: 0,
                    evidence: Vec::new(),
                    missing_count: 0,
                    missing: Vec::new(),
                })
                .collect(),
        });
        let mut snapshot = snapshot_for(txid.clone(), observed_at_ms);
        snapshot.transactions[0].bip110 = Some(classification.assessment.clone());
        let snapshot = snapshot
            .with_classification_revision(1)
            .expect("classification revision");
        MempoolObservation::new(snapshot, BTreeMap::from([(txid, classification)]))
            .expect("classified observation")
    }

    fn policy_transaction(value: u64) -> Transaction {
        Transaction {
            version: Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::default(),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(value),
                script_pubkey: ScriptBuf::from_bytes(vec![0x51]),
            }],
        }
    }

    fn policy_snapshot(transactions: &[Transaction], observed_at_ms: u64) -> MempoolSnapshot {
        let mut entries = transactions
            .iter()
            .map(|transaction| {
                MempoolEntry::new_variant(
                    transaction.compute_txid().to_string(),
                    transaction.compute_wtxid().to_string(),
                    100,
                    1_000,
                    observed_at_ms.saturating_sub(1),
                )
                .expect("entry")
            })
            .collect::<Vec<_>>();
        entries.sort_unstable_by(|left, right| left.txid.cmp(&right.txid));
        MempoolSnapshot::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            observed_at_ms,
            ChainTip {
                height: 900_000,
                hash: "00".repeat(32),
            },
            entries,
        )
        .expect("membership")
    }

    #[tokio::test]
    async fn successful_snapshot_replaces_current_state() {
        let runtime = runtime();
        assert_eq!(
            runtime.summary().await.availability,
            SourceAvailability::Waiting
        );

        runtime.record_poll_started(10).await;
        runtime
            .record_success(observation(20))
            .await
            .expect("record snapshot");
        let response = runtime.snapshot_response().await;

        assert_eq!(response.source.availability, SourceAvailability::Ready);
        assert_eq!(response.source.last_poll_started_at_ms, Some(10));
        assert_eq!(response.source.snapshot_observed_at_ms, Some(20));
        assert_eq!(
            response
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.transaction_count),
            Some(1)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn membership_publishes_before_background_slices_drain_one_wakeup() {
        let transactions = [
            policy_transaction(500),
            policy_transaction(501),
            policy_transaction(502),
        ];
        let (fixture, address, server) = start_blocking_policy_fixture(&transactions).await;
        let policy = PolicyEnricher::new(
            &format!("http://{address}/"),
            "atlas".to_owned(),
            "secret".to_owned(),
            PolicyLimits::new(2, 1, 1024 * 1024).expect("limits"),
        )
        .expect("policy enricher");
        let runtime = Arc::new(runtime());
        let classifier = tokio::spawn({
            let runtime = Arc::clone(&runtime);
            let policy = policy.clone();
            async move { runtime.run_classification(policy).await }
        });

        let publication = policy
            .install_snapshot(policy_snapshot(&transactions, 20))
            .expect("install membership");
        runtime
            .record_membership(publication)
            .await
            .expect("publish membership");
        runtime.classification_wakeup.notify_one();
        wait_for_raw_batches(&fixture, 1).await;

        let before_release = runtime.snapshot_response().await;
        let before_release_snapshot = before_release.snapshot.expect("published membership");
        assert_eq!(
            before_release.source.availability,
            SourceAvailability::Ready
        );
        assert_eq!(before_release_snapshot.transaction_count, 3);
        assert_eq!(before_release_snapshot.classification_revision, 0);
        assert_eq!(before_release_snapshot.bip110_summary.unclassified_count, 3);
        assert!(
            before_release_snapshot
                .transactions
                .iter()
                .all(|entry| entry.bip110.is_none()),
            "membership must be visible before the blocked policy RPC returns"
        );

        fixture.release_raw_batch.add_permits(1);
        wait_for_raw_batches(&fixture, 2).await;
        wait_for_classified(&runtime, 2).await;
        let after_first_slice = runtime.snapshot_response().await;
        let after_first_slice_snapshot = after_first_slice.snapshot.expect("first progress");
        assert_eq!(after_first_slice_snapshot.observed_at_ms, 20);
        assert_eq!(after_first_slice_snapshot.classification_revision, 1);
        assert_eq!(
            after_first_slice_snapshot.bip110_summary.compatible_count,
            2
        );
        assert_eq!(
            after_first_slice_snapshot.bip110_summary.unclassified_count,
            1
        );

        fixture.release_raw_batch.add_permits(1);
        wait_for_classified(&runtime, 3).await;
        let complete = runtime
            .snapshot_response()
            .await
            .snapshot
            .expect("complete snapshot");
        assert_eq!(complete.observed_at_ms, 20);
        assert_eq!(complete.classification_revision, 2);
        assert_eq!(complete.bip110_summary.compatible_count, 3);
        assert_eq!(complete.bip110_summary.unclassified_count, 0);
        assert_eq!(
            fixture.raw_batches_started.load(Ordering::SeqCst),
            2,
            "one wake-up must drain both bounded slices without another membership install"
        );

        classifier.abort();
        let _ = classifier.await;
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stale_slice_waits_for_replacement_membership_publication() {
        let old_transactions = [policy_transaction(500)];
        let replacement_transactions = [policy_transaction(501)];
        let all_transactions = old_transactions
            .iter()
            .chain(replacement_transactions.iter())
            .cloned()
            .collect::<Vec<_>>();
        let (fixture, address, server) = start_blocking_policy_fixture(&all_transactions).await;
        let policy = PolicyEnricher::new(
            &format!("http://{address}/"),
            "atlas".to_owned(),
            "secret".to_owned(),
            PolicyLimits::new(1, 1, 1024 * 1024).expect("limits"),
        )
        .expect("policy enricher");
        let runtime = Arc::new(runtime());
        let classifier = tokio::spawn({
            let runtime = Arc::clone(&runtime);
            let policy = policy.clone();
            async move { runtime.run_classification(policy).await }
        });

        let old_publication = policy
            .install_snapshot(policy_snapshot(&old_transactions, 20))
            .expect("install old membership");
        runtime
            .record_membership(old_publication)
            .await
            .expect("publish old membership");
        runtime.classification_wakeup.notify_one();
        wait_for_raw_batches(&fixture, 1).await;

        let replacement_publication = policy
            .install_snapshot(policy_snapshot(&replacement_transactions, 30))
            .expect("install replacement membership");
        fixture.release_raw_batch.add_permits(1);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                wait_for_raw_batches(&fixture, 2),
            )
            .await
            .is_err(),
            "a stale slice must not drain the replacement generation before it is published"
        );
        assert_eq!(fixture.raw_batches_started.load(Ordering::SeqCst), 1);

        runtime
            .record_membership(replacement_publication)
            .await
            .expect("publish replacement membership");
        runtime.classification_wakeup.notify_one();
        wait_for_raw_batches(&fixture, 2).await;
        fixture.release_raw_batch.add_permits(1);
        wait_for_classified(&runtime, 1).await;
        let replacement = runtime
            .snapshot_response()
            .await
            .snapshot
            .expect("replacement progress");
        assert_eq!(replacement.observed_at_ms, 30);
        assert_eq!(replacement.transaction_count, 1);
        assert_eq!(replacement.bip110_summary.compatible_count, 1);

        classifier.abort();
        let _ = classifier.await;
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replacement_generation_cannot_start_before_membership_publication() {
        let old_transactions = [policy_transaction(500)];
        let replacement_transactions = [policy_transaction(501)];
        let all_transactions = old_transactions
            .iter()
            .chain(replacement_transactions.iter())
            .cloned()
            .collect::<Vec<_>>();
        let (fixture, address, server) = start_blocking_policy_fixture(&all_transactions).await;
        let policy = PolicyEnricher::new(
            &format!("http://{address}/"),
            "atlas".to_owned(),
            "secret".to_owned(),
            PolicyLimits::new(1, 1, 1024 * 1024).expect("limits"),
        )
        .expect("policy enricher");
        let runtime = Arc::new(runtime());
        let classifier = tokio::spawn({
            let runtime = Arc::clone(&runtime);
            let policy = policy.clone();
            async move { runtime.run_classification(policy).await }
        });

        let old_publication = policy
            .install_snapshot(policy_snapshot(&old_transactions, 20))
            .expect("install old membership");
        runtime
            .record_membership(old_publication)
            .await
            .expect("publish old membership");
        let replacement_publication = policy
            .install_snapshot(policy_snapshot(&replacement_transactions, 30))
            .expect("install replacement membership");

        runtime.classification_wakeup.notify_one();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                wait_for_raw_batches(&fixture, 1),
            )
            .await
            .is_err(),
            "policy work must wait until runtime publishes the matching generation"
        );

        runtime
            .record_membership(replacement_publication)
            .await
            .expect("publish replacement membership");
        runtime.classification_wakeup.notify_one();
        wait_for_raw_batches(&fixture, 1).await;
        fixture.release_raw_batch.add_permits(1);
        wait_for_classified(&runtime, 1).await;
        let replacement = runtime
            .snapshot_response()
            .await
            .snapshot
            .expect("replacement progress");
        assert_eq!(replacement.observed_at_ms, 30);
        assert_eq!(replacement.bip110_summary.compatible_count, 1);

        classifier.abort();
        let _ = classifier.await;
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn systemic_policy_failure_pauses_until_the_next_membership() {
        let transactions = [
            policy_transaction(500),
            policy_transaction(501),
            policy_transaction(502),
        ];
        let (fixture, address, server) = start_failing_policy_fixture().await;
        let policy = PolicyEnricher::new(
            &format!("http://{address}/"),
            "atlas".to_owned(),
            "secret".to_owned(),
            PolicyLimits::new(2, 1, 1024 * 1024).expect("limits"),
        )
        .expect("policy enricher");
        let runtime = Arc::new(runtime());
        let classifier = tokio::spawn({
            let runtime = Arc::clone(&runtime);
            let policy = policy.clone();
            async move { runtime.run_classification(policy).await }
        });

        let publication = policy
            .install_snapshot(policy_snapshot(&transactions, 20))
            .expect("install membership");
        runtime
            .record_membership(publication)
            .await
            .expect("publish membership");
        runtime.classification_wakeup.notify_one();
        wait_for_failing_raw_batches(&fixture, 1).await;

        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                wait_for_failing_raw_batches(&fixture, 2),
            )
            .await
            .is_err(),
            "a no-progress policy failure must not drain the remaining generation"
        );
        let snapshot = runtime
            .snapshot_response()
            .await
            .snapshot
            .expect("membership remains visible");
        assert_eq!(snapshot.transaction_count, 3);
        assert_eq!(snapshot.bip110_summary.unclassified_count, 3);

        classifier.abort();
        let _ = classifier.await;
        server.abort();
    }

    #[tokio::test]
    async fn stale_or_nonadvancing_policy_progress_cannot_replace_runtime_state() {
        let runtime = runtime();
        let old_txid = "00".repeat(32);
        let current_txid = "01".repeat(32);

        runtime
            .record_membership(PolicyPublication {
                generation: 1,
                revision: 0,
                observation: observation(20),
            })
            .await
            .expect("record old membership");
        runtime
            .record_membership(PolicyPublication {
                generation: 2,
                revision: 0,
                observation: MempoolObservation::new(
                    snapshot_for(current_txid.clone(), 30),
                    BTreeMap::new(),
                )
                .expect("current observation"),
            })
            .await
            .expect("record current membership");
        let before_stale = runtime
            .snapshot_response_bytes()
            .await
            .expect("current bytes");

        assert!(
            !runtime
                .record_policy_progress(PolicyPublication {
                    generation: 1,
                    revision: 1,
                    observation: compatible_observation(old_txid.clone(), 20),
                })
                .await
                .expect("discard stale progress")
        );
        let after_stale = runtime
            .snapshot_response_bytes()
            .await
            .expect("bytes after stale progress");
        assert_eq!(before_stale.as_ptr(), after_stale.as_ptr());
        assert_eq!(
            runtime
                .snapshot_response()
                .await
                .snapshot
                .expect("current snapshot")
                .observed_at_ms,
            30
        );
        assert!(matches!(
            runtime.transaction_detail(&old_txid).await,
            TransactionLookup::NotPresent
        ));
        assert!(matches!(
            runtime.transaction_detail(&current_txid).await,
            TransactionLookup::Unclassified
        ));

        assert!(
            runtime
                .record_policy_progress(PolicyPublication {
                    generation: 2,
                    revision: 1,
                    observation: compatible_observation(current_txid.clone(), 30),
                })
                .await
                .expect("record current progress")
        );
        let current_bytes = runtime
            .snapshot_response_bytes()
            .await
            .expect("current progress bytes");
        assert!(matches!(
            runtime.transaction_detail(&current_txid).await,
            TransactionLookup::Ready(_)
        ));

        for revision in [0, 1] {
            assert!(
                !runtime
                    .record_policy_progress(PolicyPublication {
                        generation: 2,
                        revision,
                        observation: MempoolObservation::new(
                            snapshot_for(current_txid.clone(), 99)
                                .with_classification_revision(revision)
                                .expect("classification revision"),
                            BTreeMap::new(),
                        )
                        .expect("non-advancing observation"),
                    })
                    .await
                    .expect("discard non-advancing progress")
            );
            let after_nonadvancing = runtime
                .snapshot_response_bytes()
                .await
                .expect("bytes after non-advancing progress");
            assert_eq!(current_bytes.as_ptr(), after_nonadvancing.as_ptr());
            assert!(matches!(
                runtime.transaction_detail(&current_txid).await,
                TransactionLookup::Ready(_)
            ));
        }
    }

    #[tokio::test]
    async fn failed_poll_retains_last_good_snapshot_as_stale() {
        let runtime = runtime();
        runtime
            .record_success(observation(20))
            .await
            .expect("record snapshot");
        let before = runtime
            .snapshot_response()
            .await
            .snapshot
            .expect("snapshot");

        runtime.record_poll_started(30).await;
        runtime
            .record_failure("node unavailable".to_owned())
            .await
            .expect("record failure");
        let response = runtime.snapshot_response().await;
        let after = response.snapshot.expect("retained snapshot");

        assert_eq!(response.source.availability, SourceAvailability::Stale);
        assert_eq!(response.source.snapshot_observed_at_ms, Some(20));
        assert_eq!(
            response.source.last_error.as_deref(),
            Some("node unavailable")
        );
        assert!(Arc::ptr_eq(&before, &after));
    }

    #[tokio::test]
    async fn failure_before_first_snapshot_is_an_error() {
        let runtime = runtime();
        runtime
            .record_failure("authentication failed".to_owned())
            .await
            .expect("record failure");

        let response = runtime.snapshot_response().await;
        assert_eq!(response.source.availability, SourceAvailability::Error);
        assert!(response.snapshot.is_none());
    }

    #[tokio::test]
    async fn registry_keeps_sources_independent() {
        let core = Arc::new(runtime());
        let knots = Arc::new(
            SourceRuntime::new(
                "knots".to_owned(),
                "Bitcoin Knots".to_owned(),
                Duration::from_secs(60),
            )
            .expect("runtime"),
        );
        let registry =
            SourceRegistry::new(vec![Arc::clone(&knots), Arc::clone(&core)]).expect("registry");

        assert_eq!(
            registry
                .sources_response()
                .await
                .sources
                .iter()
                .map(|source| source.source_id.as_str())
                .collect::<Vec<_>>(),
            vec!["core", "knots"]
        );
        assert!(registry.get("missing").is_none());
        assert!(!registry.is_ready().await);

        core.record_success(observation(20))
            .await
            .expect("record snapshot");
        assert!(registry.is_ready().await);
    }

    #[tokio::test]
    async fn encoded_snapshot_response_is_shared_between_requests() {
        let runtime = runtime();
        runtime
            .record_success(observation(20))
            .await
            .expect("record snapshot");

        let first = runtime
            .snapshot_response_bytes()
            .await
            .expect("first response");
        let second = runtime
            .snapshot_response_bytes()
            .await
            .expect("second response");

        assert_eq!(first.as_ptr(), second.as_ptr());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&first).expect("JSON response")["snapshot"]
                ["transaction_count"],
            1
        );
    }
}
