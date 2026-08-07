use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{Json, Router, extract::State, routing::post};
use bitcoin::{
    Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness, absolute,
    consensus::encode::serialize_hex, transaction::Version,
};
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, Notify, Semaphore};

use super::*;
use crate::model::{
    ChainTip, MembershipFacts, MempoolEntry, TransactionClassification, TransactionStructure,
};
use crate::{
    Bip110Assessment, Bip110RuleDetail, Bip110RuleId, Bip110RuleVerdict, Bip110Status,
    ClassificationLimits,
};

mod staged_publication;

#[derive(Debug)]
struct BlockingClassificationFixture {
    raw_transactions: BTreeMap<String, String>,
    raw_batches_started: AtomicUsize,
    raw_batch_started: Notify,
    release_raw_batch: Arc<Semaphore>,
}

#[derive(Debug)]
struct FailingClassificationFixture {
    raw_batches_started: AtomicUsize,
    raw_batch_started: Notify,
}

#[derive(Debug, Default)]
struct MembershipProbe {
    source_order: AsyncMutex<Vec<String>>,
    active_verbose_calls: AtomicUsize,
    maximum_active_verbose_calls: AtomicUsize,
}

#[derive(Debug)]
struct MembershipFixture {
    source_id: String,
    probe: Arc<MembershipProbe>,
    chain_reads: AtomicUsize,
    fail_mempool_info: bool,
}

async fn membership_rpc(
    State(fixture): State<Arc<MembershipFixture>>,
    Json(request): Json<Value>,
) -> Json<Value> {
    let method = request["method"].as_str().expect("RPC method");
    if method == "getblockchaininfo" && fixture.chain_reads.fetch_add(1, Ordering::SeqCst) % 2 == 0
    {
        fixture
            .probe
            .source_order
            .lock()
            .await
            .push(fixture.source_id.clone());
    }
    if fixture.fail_mempool_info && method == "getmempoolinfo" {
        return Json(json!({
            "jsonrpc": "2.0",
            "error": { "code": -1, "message": "fixture failure" },
            "id": request["id"]
        }));
    }
    let result = match method {
        "getblockchaininfo" => json!({
            "blocks": 900_000,
            "bestblockhash": "00".repeat(32)
        }),
        "getmempoolinfo" => json!({ "size": 0 }),
        "getrawmempool" => {
            let active = fixture
                .probe
                .active_verbose_calls
                .fetch_add(1, Ordering::SeqCst)
                + 1;
            fixture
                .probe
                .maximum_active_verbose_calls
                .fetch_max(active, Ordering::SeqCst);
            tokio::task::yield_now().await;
            fixture
                .probe
                .active_verbose_calls
                .fetch_sub(1, Ordering::SeqCst);
            json!({})
        }
        other => panic!("unexpected membership RPC method {other}"),
    };
    Json(json!({
        "jsonrpc": "2.0",
        "result": result,
        "error": null,
        "id": request["id"]
    }))
}

async fn start_membership_fixture(
    source_id: &str,
    probe: Arc<MembershipProbe>,
    fail_mempool_info: bool,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let fixture = Arc::new(MembershipFixture {
        source_id: source_id.to_owned(),
        probe,
        chain_reads: AtomicUsize::new(0),
        fail_mempool_info,
    });
    let application = Router::new()
        .route("/", post(membership_rpc))
        .with_state(fixture);
    let (address, server) = crate::spawn_test_server(application).await;
    (address, server)
}

fn atlas_source(
    source_id: &str,
    address: std::net::SocketAddr,
    poll_interval: Duration,
) -> AtlasSource {
    let url = format!("http://{address}/");
    let runtime = Arc::new(
        SourceRuntime::new(source_id.to_owned(), source_id.to_owned(), poll_interval)
            .expect("source runtime"),
    );
    let rpc = RpcClient::new(&url, "atlas", "secret", 100).expect("membership RPC client");
    let classification = ClassificationPipeline::new(
        &url,
        "atlas".to_owned(),
        "secret".to_owned(),
        ClassificationLimits::new(1, 1, 1024 * 1024).expect("classification limits"),
    )
    .expect("classification client");
    AtlasSource::new(runtime, rpc, classification)
}

async fn blocking_classification_rpc(
    State(fixture): State<Arc<BlockingClassificationFixture>>,
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
                "id": request["id"]
            })
        })
        .collect::<Vec<_>>();
    Json(Value::Array(responses))
}

async fn start_blocking_classification_fixture(
    transactions: &[Transaction],
) -> (
    Arc<BlockingClassificationFixture>,
    std::net::SocketAddr,
    tokio::task::JoinHandle<()>,
) {
    let fixture = Arc::new(BlockingClassificationFixture {
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
        .route("/", post(blocking_classification_rpc))
        .with_state(Arc::clone(&fixture));
    let (address, server) = crate::spawn_test_server(application).await;
    (fixture, address, server)
}

async fn start_failing_classification_fixture() -> (
    Arc<FailingClassificationFixture>,
    std::net::SocketAddr,
    tokio::task::JoinHandle<()>,
) {
    async fn fail_classification_rpc(
        State(fixture): State<Arc<FailingClassificationFixture>>,
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

    let fixture = Arc::new(FailingClassificationFixture {
        raw_batches_started: AtomicUsize::new(0),
        raw_batch_started: Notify::new(),
    });
    let application = Router::new()
        .route("/", post(fail_classification_rpc))
        .with_state(Arc::clone(&fixture));
    let (address, server) = crate::spawn_test_server(application).await;
    (fixture, address, server)
}

async fn wait_for_raw_batches(fixture: &BlockingClassificationFixture, expected: usize) {
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
                .published_state()
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

async fn wait_for_failing_raw_batches(fixture: &FailingClassificationFixture, expected: usize) {
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
    let assessment = Bip110Assessment {
        status: Bip110Status::Compatible,
        primary_rule: None,
        violated_rules: Vec::new(),
        unknown_rules: Vec::new(),
    };
    let classification = Arc::new(TransactionClassification {
        structure: TransactionStructure::new(1, 2, 0, 50_000, 107).expect("structure"),
        txid: txid.clone(),
        wtxid: txid.clone(),
        results: crate::model::test_classifier_results(&assessment),
        assessment,
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
    MempoolObservation::materialize(
        &snapshot_for(txid.clone(), observed_at_ms),
        1,
        BTreeMap::from([(txid, classification)]),
    )
    .expect("classified observation")
}

fn indeterminate_observation(txid: String, observed_at_ms: u64) -> MempoolObservation {
    let assessment = Bip110Assessment {
        status: Bip110Status::Indeterminate,
        primary_rule: None,
        violated_rules: Vec::new(),
        unknown_rules: Bip110RuleId::ALL.to_vec(),
    };
    let classification = Arc::new(TransactionClassification {
        structure: TransactionStructure::new(1, 2, 0, 50_000, 107).expect("structure"),
        txid: txid.clone(),
        wtxid: txid.clone(),
        results: crate::model::test_classifier_results(&assessment),
        assessment,
        rules: Bip110RuleId::ALL
            .into_iter()
            .map(|rule| Bip110RuleDetail {
                rule,
                number: rule.number(),
                verdict: Bip110RuleVerdict::Unknown,
                evidence_count: 0,
                evidence: Vec::new(),
                missing_count: 1,
                missing: vec![crate::bip110::Missing::ScriptPubKey { input: 0 }],
            })
            .collect(),
    });
    MempoolObservation::materialize(
        &snapshot_for(txid.clone(), observed_at_ms),
        1,
        BTreeMap::from([(txid, classification)]),
    )
    .expect("indeterminate observation")
}

fn membership_publication(
    generation: u64,
    has_eligible_work: bool,
    observation: MempoolObservation,
) -> ClassificationGenerationStart {
    ClassificationGenerationStart {
        generation,
        revision: 0,
        has_eligible_work,
        membership: Arc::new(observation.snapshot),
        classifications: observation.classifications,
    }
}

fn revision_delta(
    generation: u64,
    revision: u64,
    observation: MempoolObservation,
) -> ClassificationRevisionDelta {
    ClassificationRevisionDelta {
        generation,
        revision,
        changed: observation.classifications.into_values().collect(),
    }
}

fn classification_report(
    generation: u64,
    disposition: ClassificationDisposition,
    publication: Option<ClassificationRevisionDelta>,
) -> ClassificationSliceReport {
    let classified = publication
        .as_ref()
        .map_or(0, |publication| publication.changed.len());
    let newly_classified = classified;
    ClassificationSliceReport {
        generation,
        attempted: 0,
        newly_classified,
        fact_requests: 0,
        facts_resolved: 0,
        facts_missing: 0,
        capacity_deferred: 0,
        deferred_candidates: 0,
        response_failures: usize::from(disposition == ClassificationDisposition::Paused),
        systemic_response_failures: usize::from(disposition == ClassificationDisposition::Paused),
        missing_responses: 0,
        batch_failures: 0,
        response_bytes: 0,
        classified,
        remaining: usize::from(matches!(
            disposition,
            ClassificationDisposition::Continue | ClassificationDisposition::Paused
        )),
        complete: disposition == ClassificationDisposition::Complete,
        stale: disposition == ClassificationDisposition::Stale,
        disposition,
        publication,
    }
}

fn assert_classification_progress(
    response: &publisher::PublishedState,
    state: ClassificationState,
    revision: u64,
    classified_count: u64,
    unclassified_count: u64,
) {
    let progress = response
        .source
        .classification
        .as_ref()
        .expect("classification lifecycle");
    assert_eq!(progress.state, state);
    assert_eq!(progress.revision, revision);
    assert_eq!(progress.classified_count, classified_count);
    assert_eq!(progress.unclassified_count, unclassified_count);
}

fn classification_transaction(value: u64) -> Transaction {
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

fn classification_snapshot(transactions: &[Transaction], observed_at_ms: u64) -> MempoolSnapshot {
    classification_snapshot_for("core", "Bitcoin Core", transactions, observed_at_ms)
}

fn classification_snapshot_for(
    source_id: &str,
    source_label: &str,
    transactions: &[Transaction],
    observed_at_ms: u64,
) -> MempoolSnapshot {
    let mut entries = transactions
        .iter()
        .map(|transaction| {
            MempoolEntry::new_variant(
                transaction.compute_txid().to_string(),
                transaction.compute_wtxid().to_string(),
                100,
                1_000,
                observed_at_ms.saturating_sub(1),
                MembershipFacts::solitary(100, 1_000),
            )
            .expect("entry")
        })
        .collect::<Vec<_>>();
    entries.sort_unstable_by(|left, right| left.txid.cmp(&right.txid));
    MempoolSnapshot::new(
        source_id.to_owned(),
        source_label.to_owned(),
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

    runtime
        .record_poll_started(10)
        .await
        .expect("record poll start");
    runtime
        .record_success(observation(20))
        .await
        .expect("record snapshot");
    let response = runtime.published_state().await;

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
async fn atlas_runtime_polls_sources_in_order_without_verbose_overlap() {
    let probe = Arc::new(MembershipProbe::default());
    let (core_address, core_server) =
        start_membership_fixture("core", Arc::clone(&probe), false).await;
    let (knots_address, knots_server) =
        start_membership_fixture("knots", Arc::clone(&probe), false).await;
    let poll_interval = Duration::from_secs(60);
    let atlas = AtlasRuntime::new(
        vec![
            atlas_source("core", core_address, poll_interval),
            atlas_source("knots", knots_address, poll_interval),
        ],
        poll_interval,
    )
    .expect("Atlas runtime");

    atlas.poll_round(1).await;

    assert_eq!(
        probe.source_order.lock().await.as_slice(),
        ["core", "knots"]
    );
    assert_eq!(probe.maximum_active_verbose_calls.load(Ordering::SeqCst), 1);
    for runtime in atlas.source_runtimes() {
        assert_eq!(
            runtime.summary().await.availability,
            SourceAvailability::Ready
        );
    }

    core_server.abort();
    knots_server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn one_source_failure_does_not_block_later_sources_in_the_round() {
    let probe = Arc::new(MembershipProbe::default());
    let (core_address, core_server) =
        start_membership_fixture("core", Arc::clone(&probe), true).await;
    let (knots_address, knots_server) =
        start_membership_fixture("knots", Arc::clone(&probe), false).await;
    let poll_interval = Duration::from_secs(60);
    let atlas = AtlasRuntime::new(
        vec![
            atlas_source("core", core_address, poll_interval),
            atlas_source("knots", knots_address, poll_interval),
        ],
        poll_interval,
    )
    .expect("Atlas runtime");

    atlas.poll_round(1).await;

    assert_eq!(
        probe.source_order.lock().await.as_slice(),
        ["core", "knots"]
    );
    let runtimes = atlas.source_runtimes();
    assert_eq!(
        runtimes[0].summary().await.availability,
        SourceAvailability::Error
    );
    assert_eq!(
        runtimes[1].summary().await.availability,
        SourceAvailability::Ready
    );

    core_server.abort();
    knots_server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn poll_start_publication_failure_does_not_hide_the_rpc_outcome() {
    let probe = Arc::new(MembershipProbe::default());
    let (address, server) = start_membership_fixture("core", Arc::clone(&probe), true).await;
    let poll_interval = Duration::from_secs(60);
    let runtime = Arc::new(
        SourceRuntime::new("core".to_owned(), "Bitcoin Core".to_owned(), poll_interval)
            .expect("source runtime"),
    );
    runtime
        .record_success(observation(20))
        .await
        .expect("record initial snapshot");
    runtime.limit_next_poll_start_reencoding(StagedSnapshotLimits {
        max_stage_bytes: 1,
        max_publication_bytes: 1,
    });
    let url = format!("http://{address}/");
    let source = AtlasSource::new(
        Arc::clone(&runtime),
        RpcClient::new(&url, "atlas", "secret", 100).expect("membership RPC client"),
        ClassificationPipeline::new(
            &url,
            "atlas".to_owned(),
            "secret".to_owned(),
            ClassificationLimits::new(1, 1, 1024 * 1024).expect("classification limits"),
        )
        .expect("classification client"),
    );
    let atlas = AtlasRuntime::new(vec![source], poll_interval).expect("Atlas runtime");

    atlas.poll_round(1).await;

    assert_eq!(probe.source_order.lock().await.as_slice(), ["core"]);
    let summary = runtime.summary().await;
    assert_eq!(summary.availability, SourceAvailability::Stale);
    assert_eq!(summary.last_poll_started_at_ms, None);
    assert_eq!(
        summary.last_error.as_deref(),
        Some("Bitcoin node RPC is unavailable")
    );
    let manifest = runtime
        .current_manifest_payload()
        .await
        .expect("failure manifest");
    let manifest = serde_json::from_slice::<Value>(&manifest.body).expect("manifest JSON");
    assert_eq!(manifest["source"]["availability"], "stale");
    assert_eq!(
        manifest["source"]["last_error"],
        "Bitcoin node RPC is unavailable"
    );

    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn atlas_runtime_never_overlaps_source_classification_slices() {
    let core_transactions = [
        classification_transaction(700),
        classification_transaction(702),
    ];
    let knots_transaction = classification_transaction(701);
    let (core_fixture, core_address, core_server) =
        start_blocking_classification_fixture(&core_transactions).await;
    let (knots_fixture, knots_address, knots_server) =
        start_blocking_classification_fixture(std::slice::from_ref(&knots_transaction)).await;
    let poll_interval = Duration::from_secs(60);
    let limits = ClassificationLimits::new(1, 1, 1024 * 1024).expect("classification limits");

    let core_runtime = Arc::new(
        SourceRuntime::new("core".to_owned(), "Bitcoin Core".to_owned(), poll_interval)
            .expect("Core runtime"),
    );
    let core_url = format!("http://{core_address}/");
    let core_classification =
        ClassificationPipeline::new(&core_url, "atlas".to_owned(), "secret".to_owned(), limits)
            .expect("Core classification");
    let core_source = AtlasSource::new(
        Arc::clone(&core_runtime),
        RpcClient::new(&core_url, "atlas", "secret", 100).expect("Core RPC"),
        core_classification.clone(),
    );

    let knots_runtime = Arc::new(
        SourceRuntime::new(
            "knots".to_owned(),
            "Bitcoin Knots".to_owned(),
            poll_interval,
        )
        .expect("Knots runtime"),
    );
    let knots_url = format!("http://{knots_address}/");
    let knots_classification =
        ClassificationPipeline::new(&knots_url, "atlas".to_owned(), "secret".to_owned(), limits)
            .expect("Knots classification");
    let knots_source = AtlasSource::new(
        Arc::clone(&knots_runtime),
        RpcClient::new(&knots_url, "atlas", "secret", 100).expect("Knots RPC"),
        knots_classification.clone(),
    );

    core_runtime
        .record_membership(
            core_classification
                .install_snapshot(classification_snapshot_for(
                    "core",
                    "Bitcoin Core",
                    &core_transactions,
                    20,
                ))
                .expect("Core membership"),
        )
        .await
        .expect("publish Core membership");
    knots_runtime
        .record_membership(
            knots_classification
                .install_snapshot(classification_snapshot_for(
                    "knots",
                    "Bitcoin Knots",
                    std::slice::from_ref(&knots_transaction),
                    20,
                ))
                .expect("Knots membership"),
        )
        .await
        .expect("publish Knots membership");

    let atlas = Arc::new(
        AtlasRuntime::new(vec![core_source, knots_source], poll_interval).expect("Atlas runtime"),
    );
    let classifier = tokio::spawn({
        let atlas = Arc::clone(&atlas);
        async move { atlas.run_classification().await }
    });
    atlas.schedule_classification(0);

    wait_for_raw_batches(&core_fixture, 1).await;
    atlas.schedule_classification(1);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            wait_for_raw_batches(&knots_fixture, 1),
        )
        .await
        .is_err(),
        "Knots classification must wait while the Core slice owns the global gate"
    );
    core_fixture.release_raw_batch.add_permits(1);
    wait_for_classified(&core_runtime, 1).await;
    wait_for_raw_batches(&knots_fixture, 1).await;
    assert_eq!(
        core_fixture.raw_batches_started.load(Ordering::SeqCst),
        1,
        "the next Core slice must wait for Knots to receive its fair turn"
    );
    knots_fixture.release_raw_batch.add_permits(1);
    wait_for_classified(&knots_runtime, 1).await;
    wait_for_raw_batches(&core_fixture, 2).await;
    core_fixture.release_raw_batch.add_permits(1);
    wait_for_classified(&core_runtime, 2).await;

    classifier.abort();
    let _ = classifier.await;
    core_server.abort();
    knots_server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn membership_acquires_gate_while_classification_publication_prepares() {
    let transaction = classification_transaction(705);
    let (classification_fixture, classification_address, classification_server) =
        start_blocking_classification_fixture(std::slice::from_ref(&transaction)).await;
    let membership_probe = Arc::new(MembershipProbe::default());
    let (membership_address, membership_server) =
        start_membership_fixture("core", Arc::clone(&membership_probe), false).await;
    let poll_interval = Duration::from_secs(60);
    let runtime = Arc::new(runtime());
    let classification_url = format!("http://{classification_address}/");
    let classification = ClassificationPipeline::new(
        &classification_url,
        "atlas".to_owned(),
        "secret".to_owned(),
        ClassificationLimits::new(1, 1, 1024 * 1024).expect("classification limits"),
    )
    .expect("classification pipeline");
    runtime
        .record_membership(
            classification
                .install_snapshot(classification_snapshot(
                    std::slice::from_ref(&transaction),
                    20,
                ))
                .expect("initial membership"),
        )
        .await
        .expect("publish initial membership");
    let source = AtlasSource::new(
        Arc::clone(&runtime),
        RpcClient::new(
            &format!("http://{membership_address}/"),
            "atlas",
            "secret",
            100,
        )
        .expect("membership client"),
        classification,
    );
    let atlas = Arc::new(AtlasRuntime::new(vec![source], poll_interval).expect("Atlas runtime"));
    let preparation = runtime.block_next_classification_preparation();
    let classifier = tokio::spawn({
        let atlas = Arc::clone(&atlas);
        async move { atlas.run_classification().await }
    });
    atlas.schedule_classification(0);
    wait_for_raw_batches(&classification_fixture, 1).await;
    classification_fixture.release_raw_batch.add_permits(1);
    tokio::time::timeout(Duration::from_secs(1), preparation.wait_until_started())
        .await
        .expect("classification publication preparation blocked");

    let membership_round = tokio::spawn({
        let atlas = Arc::clone(&atlas);
        async move { atlas.poll_round(1).await }
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if membership_probe
                .source_order
                .lock()
                .await
                .iter()
                .any(|source_id| source_id == "core")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("membership RPC acquired the released work gate");
    membership_round
        .await
        .expect("membership round completed while publication remained blocked");

    preparation.release();
    classifier.abort();
    let _ = classifier.await;
    classification_server.abort();
    membership_server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn successful_source_does_not_rearm_another_sources_paused_generation() {
    let (core_fixture, core_address, core_server) = start_failing_classification_fixture().await;
    let knots_transactions = [
        classification_transaction(710),
        classification_transaction(711),
    ];
    let (knots_fixture, knots_address, knots_server) =
        start_blocking_classification_fixture(&knots_transactions).await;
    let poll_interval = Duration::from_secs(60);
    let limits = ClassificationLimits::new(1, 1, 1024 * 1024).expect("classification limits");

    let core_runtime = Arc::new(
        SourceRuntime::new("core".to_owned(), "Bitcoin Core".to_owned(), poll_interval)
            .expect("Core runtime"),
    );
    let core_url = format!("http://{core_address}/");
    let core_classification =
        ClassificationPipeline::new(&core_url, "atlas".to_owned(), "secret".to_owned(), limits)
            .expect("Core classification");
    let core_source = AtlasSource::new(
        Arc::clone(&core_runtime),
        RpcClient::new(&core_url, "atlas", "secret", 100).expect("Core RPC"),
        core_classification.clone(),
    );

    let knots_runtime = Arc::new(
        SourceRuntime::new(
            "knots".to_owned(),
            "Bitcoin Knots".to_owned(),
            poll_interval,
        )
        .expect("Knots runtime"),
    );
    let knots_url = format!("http://{knots_address}/");
    let knots_classification =
        ClassificationPipeline::new(&knots_url, "atlas".to_owned(), "secret".to_owned(), limits)
            .expect("Knots classification");
    let knots_source = AtlasSource::new(
        Arc::clone(&knots_runtime),
        RpcClient::new(&knots_url, "atlas", "secret", 100).expect("Knots RPC"),
        knots_classification.clone(),
    );

    core_runtime
        .record_membership(
            core_classification
                .install_snapshot(classification_snapshot_for(
                    "core",
                    "Bitcoin Core",
                    std::slice::from_ref(&knots_transactions[0]),
                    20,
                ))
                .expect("Core membership"),
        )
        .await
        .expect("publish Core membership");
    knots_runtime
        .record_membership(
            knots_classification
                .install_snapshot(classification_snapshot_for(
                    "knots",
                    "Bitcoin Knots",
                    std::slice::from_ref(&knots_transactions[0]),
                    20,
                ))
                .expect("Knots membership"),
        )
        .await
        .expect("publish Knots membership");

    let atlas = Arc::new(
        AtlasRuntime::new(vec![core_source, knots_source], poll_interval).expect("Atlas runtime"),
    );
    let classifier = tokio::spawn({
        let atlas = Arc::clone(&atlas);
        async move { atlas.run_classification().await }
    });
    atlas.schedule_classification(0);
    atlas.schedule_classification(1);

    wait_for_failing_raw_batches(&core_fixture, 1).await;
    wait_for_raw_batches(&knots_fixture, 1).await;
    knots_fixture.release_raw_batch.add_permits(1);
    wait_for_classified(&knots_runtime, 1).await;

    knots_runtime
        .record_membership(
            knots_classification
                .install_snapshot(classification_snapshot_for(
                    "knots",
                    "Bitcoin Knots",
                    std::slice::from_ref(&knots_transactions[1]),
                    30,
                ))
                .expect("replacement Knots membership"),
        )
        .await
        .expect("publish replacement Knots membership");
    atlas.schedule_classification(1);

    wait_for_raw_batches(&knots_fixture, 2).await;
    assert_eq!(
        core_fixture.raw_batches_started.load(Ordering::SeqCst),
        1,
        "Core must remain paused until Core publishes a replacement generation"
    );
    knots_fixture.release_raw_batch.add_permits(1);
    wait_for_classified(&knots_runtime, 1).await;

    classifier.abort();
    let _ = classifier.await;
    core_server.abort();
    knots_server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn membership_publishes_before_background_slices_drain_one_wakeup() {
    let transactions = [
        classification_transaction(500),
        classification_transaction(501),
        classification_transaction(502),
    ];
    let (fixture, address, server) = start_blocking_classification_fixture(&transactions).await;
    let classification = ClassificationPipeline::new(
        &format!("http://{address}/"),
        "atlas".to_owned(),
        "secret".to_owned(),
        ClassificationLimits::new(2, 1, 1024 * 1024).expect("limits"),
    )
    .expect("classification pipeline");
    let runtime = Arc::new(runtime());
    let classifier = tokio::spawn({
        let runtime = Arc::clone(&runtime);
        let classification = classification.clone();
        async move { runtime.run_classification(classification).await }
    });

    let publication = classification
        .install_snapshot(classification_snapshot(&transactions, 20))
        .expect("install membership");
    runtime
        .record_membership(publication)
        .await
        .expect("publish membership");
    runtime.classification_wakeup.notify_one();
    wait_for_raw_batches(&fixture, 1).await;

    let before_release = runtime.published_state().await;
    assert_classification_progress(&before_release, ClassificationState::Classifying, 0, 0, 3);
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
        "membership must be visible before the blocked classification RPC returns"
    );

    fixture.release_raw_batch.add_permits(1);
    wait_for_raw_batches(&fixture, 2).await;
    wait_for_classified(&runtime, 2).await;
    let after_first_slice = runtime.published_state().await;
    assert_classification_progress(
        &after_first_slice,
        ClassificationState::Classifying,
        1,
        2,
        1,
    );
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
    let complete_response = runtime.published_state().await;
    assert_classification_progress(&complete_response, ClassificationState::Complete, 2, 3, 0);
    let complete = complete_response.snapshot.expect("complete snapshot");
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
async fn revision_zero_lifecycle_uses_actual_eligible_work_and_exact_carry() {
    let transaction = classification_transaction(550);
    let (fixture, address, server) =
        start_blocking_classification_fixture(std::slice::from_ref(&transaction)).await;
    let classification = ClassificationPipeline::new(
        &format!("http://{address}/"),
        "atlas".to_owned(),
        "secret".to_owned(),
        ClassificationLimits::new(1, 1, 1024 * 1024).expect("limits"),
    )
    .expect("classification pipeline");
    let runtime = runtime();

    let first_publication = classification
        .install_snapshot(classification_snapshot(
            std::slice::from_ref(&transaction),
            20,
        ))
        .expect("first membership");
    assert!(first_publication.has_eligible_work);
    runtime
        .record_membership(first_publication)
        .await
        .expect("publish first membership");
    let initial = runtime.published_state().await;
    assert_classification_progress(&initial, ClassificationState::Classifying, 0, 0, 1);

    fixture.release_raw_batch.add_permits(1);
    let report = classification
        .classify_next()
        .await
        .expect("classify first membership");
    assert_eq!(report.disposition, ClassificationDisposition::Complete);
    assert!(
        !runtime
            .record_classification_outcome(report)
            .await
            .expect("publish completed first membership")
    );
    let first_complete = runtime.published_state().await;
    assert_classification_progress(&first_complete, ClassificationState::Complete, 1, 1, 0);

    let carried_publication = classification
        .install_snapshot(classification_snapshot(
            std::slice::from_ref(&transaction),
            30,
        ))
        .expect("replacement membership");
    assert_eq!(carried_publication.revision, 0);
    assert!(!carried_publication.has_eligible_work);
    assert_eq!(carried_publication.classifications.len(), 1);
    runtime
        .record_membership(carried_publication)
        .await
        .expect("publish replacement membership");

    let carried = runtime.published_state().await;
    assert_classification_progress(&carried, ClassificationState::Complete, 0, 1, 0);
    assert_eq!(
        carried
            .snapshot
            .as_ref()
            .expect("carried snapshot")
            .observed_at_ms,
        30
    );
    assert_eq!(fixture.raw_batches_started.load(Ordering::SeqCst), 1);

    server.abort();
}

#[tokio::test]
async fn state_only_completion_preserves_unclassified_revision_and_reencodes_manifest() {
    let runtime = runtime();
    runtime
        .record_membership(membership_publication(1, true, observation(20)))
        .await
        .expect("publish membership");
    let before = runtime
        .manifest_bytes()
        .await
        .expect("classifying response");

    assert!(
        !runtime
            .record_classification_outcome(classification_report(
                1,
                ClassificationDisposition::Complete,
                None
            ))
            .await
            .expect("publish state-only completion")
    );

    let response = runtime.published_state().await;
    assert_classification_progress(&response, ClassificationState::Complete, 0, 0, 1);
    let snapshot = response.snapshot.as_ref().expect("retained snapshot");
    assert_eq!(snapshot.classification_revision, 0);
    assert_eq!(snapshot.bip110_summary.unclassified_count, 1);

    let after = runtime.manifest_bytes().await.expect("completed response");
    assert_ne!(before.as_ptr(), after.as_ptr());
    let encoded = serde_json::from_slice::<Value>(&after).expect("response JSON");
    assert_eq!(encoded["source"]["classification"]["state"], "complete");
    assert_eq!(encoded["source"]["classification"]["revision"], 0);
    assert_eq!(encoded["classification_revision"], 0);
    assert_eq!(encoded["source"]["classification"]["unclassified_count"], 1);
}

#[tokio::test]
async fn publication_and_pause_replace_domain_bundle_and_lifecycle_atomically() {
    let runtime = runtime();
    let txid = "00".repeat(32);
    runtime
        .record_membership(membership_publication(1, true, observation(20)))
        .await
        .expect("publish membership");

    let publication = revision_delta(1, 1, compatible_observation(txid.clone(), 20));
    assert!(
        !runtime
            .record_classification_outcome(classification_report(
                1,
                ClassificationDisposition::Paused,
                Some(publication),
            ))
            .await
            .expect("publish assessment and pause")
    );

    let response = runtime.published_state().await;
    assert_classification_progress(&response, ClassificationState::Paused, 1, 1, 0);
    let snapshot = response.snapshot.as_ref().expect("paused snapshot");
    assert_eq!(snapshot.classification_revision, 1);
    assert_eq!(snapshot.bip110_summary.compatible_count, 1);
    assert!(matches!(
        runtime.transaction_detail(&txid).await,
        TransactionLookup::Ready(TransactionDetailResponse {
            classification_revision: 1,
            ..
        })
    ));

    let encoded = runtime.manifest_bytes().await.expect("paused response");
    let encoded = serde_json::from_slice::<Value>(&encoded).expect("response JSON");
    assert_eq!(encoded["source"]["classification"]["state"], "paused");
    assert_eq!(encoded["source"]["classification"]["revision"], 1);
    assert_eq!(encoded["classification_revision"], 1);
}

#[tokio::test]
async fn retryable_replacement_updates_summary_and_detail_atomically() {
    let runtime = runtime();
    let txid = "00".repeat(32);
    runtime
        .record_membership(membership_publication(1, true, observation(20)))
        .await
        .expect("publish membership");

    assert!(
        runtime
            .record_classification_progress(revision_delta(
                1,
                1,
                indeterminate_observation(txid.clone(), 20),
            ))
            .await
            .expect("publish retryable classification")
    );
    let retryable = runtime.published_state().await;
    assert_classification_progress(&retryable, ClassificationState::Classifying, 1, 1, 0);
    assert_eq!(
        retryable
            .snapshot
            .expect("retryable snapshot")
            .bip110_summary
            .indeterminate_count,
        1
    );
    assert!(matches!(
        runtime.transaction_detail(&txid).await,
        TransactionLookup::Ready(TransactionDetailResponse {
            classification_revision: 1,
            assessment: Bip110Assessment {
                status: Bip110Status::Indeterminate,
                ..
            },
            ..
        })
    ));

    assert!(
        runtime
            .record_classification_update(
                1,
                Some(revision_delta(
                    1,
                    2,
                    compatible_observation(txid.clone(), 20),
                )),
                ClassificationState::Complete,
            )
            .await
            .expect("replace retryable classification")
    );
    let complete = runtime.published_state().await;
    assert_classification_progress(&complete, ClassificationState::Complete, 2, 1, 0);
    let summary = &complete.snapshot.expect("complete snapshot").bip110_summary;
    assert_eq!(summary.compatible_count, 1);
    assert_eq!(summary.indeterminate_count, 0);
    assert!(matches!(
        runtime.transaction_detail(&txid).await,
        TransactionLookup::Ready(TransactionDetailResponse {
            classification_revision: 2,
            assessment: Bip110Assessment {
                status: Bip110Status::Compatible,
                ..
            },
            ..
        })
    ));
}

#[tokio::test]
async fn stale_generation_during_preparation_cannot_replace_body_or_etag() {
    let runtime = Arc::new(runtime());
    let old_txid = "00".repeat(32);
    let replacement_txid = "01".repeat(32);
    runtime
        .record_membership(membership_publication(1, true, observation(20)))
        .await
        .expect("publish old membership");
    let block = runtime.block_next_classification_preparation();
    let stale_publish = tokio::spawn({
        let runtime = Arc::clone(&runtime);
        async move {
            runtime
                .record_classification_progress(revision_delta(
                    1,
                    1,
                    compatible_observation(old_txid, 20),
                ))
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(1), block.wait_until_started())
        .await
        .expect("classification preparation blocked");

    runtime
        .record_membership(membership_publication(
            2,
            true,
            MempoolObservation::new(snapshot_for(replacement_txid.clone(), 30), BTreeMap::new())
                .expect("replacement observation"),
        ))
        .await
        .expect("publish replacement membership");
    let replacement = runtime
        .current_manifest_payload()
        .await
        .expect("replacement payload");
    block.release();
    assert!(
        !stale_publish
            .await
            .expect("stale publication task")
            .expect("stale publication result")
    );

    let after_stale = runtime
        .current_manifest_payload()
        .await
        .expect("payload after stale prepare");
    assert_eq!(replacement.body.as_ptr(), after_stale.body.as_ptr());
    assert_eq!(replacement.etag, after_stale.etag);
    assert!(matches!(
        runtime.transaction_detail(&replacement_txid).await,
        TransactionLookup::Unclassified
    ));
}

#[tokio::test]
async fn publisher_rejects_more_retained_classifications_than_membership() {
    let runtime = runtime();
    let retained_txid = "00".repeat(32);
    let extra_txid = "01".repeat(32);
    runtime
        .record_membership(membership_publication(1, true, observation(20)))
        .await
        .expect("publish membership");
    let retained = compatible_observation(retained_txid, 20)
        .classifications
        .into_values()
        .next()
        .expect("retained classification");
    let extra = compatible_observation(extra_txid, 20)
        .classifications
        .into_values()
        .next()
        .expect("extra classification");
    let before = runtime
        .current_manifest_payload()
        .await
        .expect("membership payload");

    assert!(matches!(
        runtime
            .record_classification_progress(ClassificationRevisionDelta {
                generation: 1,
                revision: 1,
                changed: vec![retained, extra],
            })
            .await,
        Err(RuntimeError::ClassificationsExceedMembership {
            classified: 2,
            membership: 1,
        })
    ));
    let after = runtime
        .current_manifest_payload()
        .await
        .expect("unchanged membership payload");
    assert_eq!(before.body.as_ptr(), after.body.as_ptr());
    assert_eq!(before.etag, after.etag);
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_slice_waits_for_replacement_membership_publication() {
    let old_transactions = [classification_transaction(500)];
    let replacement_transactions = [classification_transaction(501)];
    let all_transactions = old_transactions
        .iter()
        .chain(replacement_transactions.iter())
        .cloned()
        .collect::<Vec<_>>();
    let (fixture, address, server) = start_blocking_classification_fixture(&all_transactions).await;
    let classification = ClassificationPipeline::new(
        &format!("http://{address}/"),
        "atlas".to_owned(),
        "secret".to_owned(),
        ClassificationLimits::new(1, 1, 1024 * 1024).expect("limits"),
    )
    .expect("classification pipeline");
    let runtime = Arc::new(runtime());
    let classifier = tokio::spawn({
        let runtime = Arc::clone(&runtime);
        let classification = classification.clone();
        async move { runtime.run_classification(classification).await }
    });

    let old_publication = classification
        .install_snapshot(classification_snapshot(&old_transactions, 20))
        .expect("install old membership");
    runtime
        .record_membership(old_publication)
        .await
        .expect("publish old membership");
    runtime.classification_wakeup.notify_one();
    wait_for_raw_batches(&fixture, 1).await;

    let replacement_publication = classification
        .install_snapshot(classification_snapshot(&replacement_transactions, 30))
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
        .published_state()
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
    let old_transactions = [classification_transaction(500)];
    let replacement_transactions = [classification_transaction(501)];
    let all_transactions = old_transactions
        .iter()
        .chain(replacement_transactions.iter())
        .cloned()
        .collect::<Vec<_>>();
    let (fixture, address, server) = start_blocking_classification_fixture(&all_transactions).await;
    let classification = ClassificationPipeline::new(
        &format!("http://{address}/"),
        "atlas".to_owned(),
        "secret".to_owned(),
        ClassificationLimits::new(1, 1, 1024 * 1024).expect("limits"),
    )
    .expect("classification pipeline");
    let runtime = Arc::new(runtime());
    let classifier = tokio::spawn({
        let runtime = Arc::clone(&runtime);
        let classification = classification.clone();
        async move { runtime.run_classification(classification).await }
    });

    let old_publication = classification
        .install_snapshot(classification_snapshot(&old_transactions, 20))
        .expect("install old membership");
    runtime
        .record_membership(old_publication)
        .await
        .expect("publish old membership");
    let replacement_publication = classification
        .install_snapshot(classification_snapshot(&replacement_transactions, 30))
        .expect("install replacement membership");

    runtime.classification_wakeup.notify_one();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            wait_for_raw_batches(&fixture, 1),
        )
        .await
        .is_err(),
        "classification work must wait until runtime publishes the matching generation"
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
        .published_state()
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
async fn systemic_classification_failure_pauses_until_the_next_membership() {
    let transactions = [
        classification_transaction(500),
        classification_transaction(501),
        classification_transaction(502),
    ];
    let (fixture, address, server) = start_failing_classification_fixture().await;
    let classification = ClassificationPipeline::new(
        &format!("http://{address}/"),
        "atlas".to_owned(),
        "secret".to_owned(),
        ClassificationLimits::new(2, 1, 1024 * 1024).expect("limits"),
    )
    .expect("classification pipeline");
    let runtime = Arc::new(runtime());
    let classifier = tokio::spawn({
        let runtime = Arc::clone(&runtime);
        let classification = classification.clone();
        async move { runtime.run_classification(classification).await }
    });

    let publication = classification
        .install_snapshot(classification_snapshot(&transactions, 20))
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
        "a systemic classification failure must not drain the remaining generation"
    );
    let paused = runtime.published_state().await;
    assert_classification_progress(&paused, ClassificationState::Paused, 0, 0, 3);
    let snapshot = paused.snapshot.expect("membership remains visible");
    assert_eq!(snapshot.transaction_count, 3);
    assert_eq!(snapshot.bip110_summary.unclassified_count, 3);

    let replacement = classification
        .install_snapshot(classification_snapshot(&transactions, 30))
        .expect("replacement membership");
    assert!(replacement.has_eligible_work);
    runtime
        .record_membership(replacement)
        .await
        .expect("publish replacement membership");
    let restarted = runtime.published_state().await;
    assert_classification_progress(&restarted, ClassificationState::Classifying, 0, 0, 3);
    assert_eq!(
        restarted
            .snapshot
            .as_ref()
            .expect("replacement snapshot")
            .observed_at_ms,
        30
    );

    classifier.abort();
    let _ = classifier.await;
    server.abort();
}

#[tokio::test]
async fn stale_or_nonadvancing_classification_progress_cannot_replace_runtime_state() {
    let runtime = runtime();
    let old_txid = "00".repeat(32);
    let current_txid = "01".repeat(32);

    runtime
        .record_membership(membership_publication(1, true, observation(20)))
        .await
        .expect("record old membership");
    runtime
        .record_membership(membership_publication(
            2,
            true,
            MempoolObservation::new(snapshot_for(current_txid.clone(), 30), BTreeMap::new())
                .expect("current observation"),
        ))
        .await
        .expect("record current membership");
    let before_stale = runtime.manifest_bytes().await.expect("current bytes");
    let current_lifecycle = runtime
        .summary()
        .await
        .classification
        .expect("current lifecycle");
    assert_eq!(current_lifecycle.state, ClassificationState::Classifying);

    assert!(
        !runtime
            .record_classification_outcome(classification_report(
                1,
                ClassificationDisposition::Paused,
                None
            ))
            .await
            .expect("discard stale outcome")
    );
    let after_stale_outcome = runtime
        .manifest_bytes()
        .await
        .expect("bytes after stale outcome");
    assert_eq!(before_stale.as_ptr(), after_stale_outcome.as_ptr());
    assert_eq!(
        runtime.summary().await.classification,
        Some(current_lifecycle)
    );

    assert!(
        !runtime
            .record_classification_progress(revision_delta(
                1,
                1,
                compatible_observation(old_txid.clone(), 20),
            ))
            .await
            .expect("discard stale progress")
    );
    let after_stale = runtime
        .manifest_bytes()
        .await
        .expect("bytes after stale progress");
    assert_eq!(before_stale.as_ptr(), after_stale.as_ptr());
    assert_eq!(
        runtime
            .published_state()
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
            .record_classification_progress(revision_delta(
                2,
                1,
                compatible_observation(current_txid.clone(), 30),
            ))
            .await
            .expect("record current progress")
    );
    assert!(
        !runtime
            .record_classification_outcome(classification_report(
                2,
                ClassificationDisposition::Complete,
                None
            ))
            .await
            .expect("complete current generation")
    );
    let current_bytes = runtime
        .manifest_bytes()
        .await
        .expect("current progress bytes");
    let completed_lifecycle = runtime
        .summary()
        .await
        .classification
        .expect("completed lifecycle");
    assert_eq!(completed_lifecycle.state, ClassificationState::Complete);
    assert_eq!(completed_lifecycle.revision, 1);
    assert!(matches!(
        runtime.transaction_detail(&current_txid).await,
        TransactionLookup::Ready(_)
    ));

    for revision in [0, 1] {
        assert!(
            !runtime
                .record_classification_progress(revision_delta(
                    2,
                    revision,
                    MempoolObservation::new(
                        snapshot_for(current_txid.clone(), 99)
                            .with_classification_revision(revision)
                            .expect("classification revision"),
                        BTreeMap::new(),
                    )
                    .expect("non-advancing observation"),
                ))
                .await
                .expect("discard non-advancing progress")
        );
        let after_nonadvancing = runtime
            .manifest_bytes()
            .await
            .expect("bytes after non-advancing progress");
        assert_eq!(current_bytes.as_ptr(), after_nonadvancing.as_ptr());
        assert_eq!(
            runtime.summary().await.classification,
            Some(completed_lifecycle.clone())
        );
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
    assert!(
        !runtime
            .record_classification_outcome(classification_report(
                1,
                ClassificationDisposition::Complete,
                None
            ))
            .await
            .expect("complete classification")
    );
    let before_response = runtime.published_state().await;
    assert_classification_progress(&before_response, ClassificationState::Complete, 0, 0, 1);
    let before_progress = before_response.source.classification.clone();
    let before = before_response.snapshot.expect("snapshot");

    runtime
        .record_poll_started(30)
        .await
        .expect("record poll start");
    runtime
        .record_failure("node unavailable".to_owned())
        .await
        .expect("record failure");
    let response = runtime.published_state().await;
    let after = response.snapshot.expect("retained snapshot");

    assert_eq!(response.source.availability, SourceAvailability::Stale);
    assert_eq!(response.source.snapshot_observed_at_ms, Some(20));
    assert_eq!(
        response.source.last_error.as_deref(),
        Some("node unavailable")
    );
    assert_eq!(response.source.classification, before_progress);
    assert!(Arc::ptr_eq(&before, &after));
}

#[tokio::test]
async fn failure_before_first_snapshot_is_an_error() {
    let runtime = runtime();
    runtime
        .record_failure("authentication failed".to_owned())
        .await
        .expect("record failure");

    let response = runtime.published_state().await;
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
async fn encoded_manifest_is_shared_between_requests() {
    let runtime = runtime();
    runtime
        .record_success(observation(20))
        .await
        .expect("record snapshot");

    let first = runtime.manifest_bytes().await.expect("first response");
    let second = runtime.manifest_bytes().await.expect("second response");

    assert_eq!(first.as_ptr(), second.as_ptr());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&first).expect("JSON response")["transaction_count"],
        1
    );
}
