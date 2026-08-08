use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc as Shared, Barrier};

use crate::bip110::{EvaluationMode, RuleOutcome};
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use bitcoin::absolute;
use bitcoin::consensus::encode::serialize_hex;
use bitcoin::hashes::Hash;
use bitcoin::transaction::Version;
use bitcoin::{Amount, Sequence, TxIn, TxOut, Witness};
use serde_json::Value;
use tokio::sync::{Mutex as AsyncMutex, Notify, Semaphore};
use tokio::task::JoinHandle;

use super::*;
use crate::model::MembershipFacts;
mod conflict_facts;
fn changed_classifications(
    report: &ClassificationSliceReport,
) -> impl Iterator<Item = &Arc<TransactionClassification>> {
    report
        .publication
        .iter()
        .flat_map(|delta| delta.changed.iter())
}

fn changed_classification<'a>(
    report: &'a ClassificationSliceReport,
    txid: &str,
) -> &'a Arc<TransactionClassification> {
    changed_classifications(report)
        .find(|classification| classification.txid == txid)
        .unwrap_or_else(|| panic!("missing changed classification for {txid}"))
}

fn changed_contains(report: &ClassificationSliceReport, txid: &str) -> bool {
    changed_classifications(report).any(|classification| classification.txid == txid)
}

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

    fn with_raw_failures(mut self, txid: Txid, count: usize) -> Self {
        self.raw_failures.insert(txid.to_string(), count);
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
    let (address, server) = crate::spawn_test_server(application).await;
    (state, address, server)
}

async fn start_invalid_classification_fixture() -> (SocketAddr, JoinHandle<()>) {
    let application = Router::new().route("/", post(|| async { "not JSON" }));
    let (address, server) = crate::spawn_test_server(application).await;
    (address, server)
}

fn test_pipeline(address: SocketAddr) -> ClassificationPipeline {
    test_pipeline_with_limits(
        address,
        ClassificationLimits::new(10, 4, 16 * 1024 * 1024).expect("limits"),
    )
}

fn test_pipeline_with_limits(
    address: SocketAddr,
    limits: ClassificationLimits,
) -> ClassificationPipeline {
    ClassificationPipeline::new(
        &format!("http://{address}/"),
        "atlas".to_owned(),
        "secret".to_owned(),
        limits,
    )
    .expect("pipeline")
}

fn mempool_entry(transaction: &Transaction) -> MempoolEntry {
    MempoolEntry::new_variant(
        transaction.compute_txid().to_string(),
        transaction.compute_wtxid().to_string(),
        100,
        1_000,
        1,
        MembershipFacts::solitary(100, 1_000),
    )
    .expect("entry")
}

async fn classify_entries(
    pipeline: &ClassificationPipeline,
    mut entries: Vec<MempoolEntry>,
) -> (Vec<MempoolEntry>, ClassificationSliceReport) {
    let pipeline = pipeline.clone();
    tokio::task::spawn_blocking(move || {
        let report = pipeline.classify_entries(&mut entries);
        (entries, report)
    })
    .await
    .expect("classification")
}

#[tokio::test(flavor = "multi_thread")]
async fn classifies_once_and_reuses_the_wtxid_cache() {
    let transaction = transaction();
    let txid = transaction.compute_txid().to_string();
    let wtxid = transaction.compute_wtxid().to_string();
    let (fixture, address, server) =
        start_rpc_fixture(RpcFixture::new([transaction.clone()])).await;
    let pipeline = test_pipeline(address);
    let mut entries = vec![
        MempoolEntry::new_variant(
            txid.clone(),
            wtxid,
            100,
            1_000,
            1,
            MembershipFacts::solitary(100, 1_000),
        )
        .expect("entry"),
    ];
    let first = tokio::task::spawn_blocking({
        let pipeline = pipeline.clone();
        let mut entries = entries.clone();
        move || {
            let report = pipeline.classify_entries(&mut entries);
            (entries, report)
        }
    })
    .await
    .expect("first classification");
    entries = first.0;
    assert_eq!(first.1.newly_classified, 1);
    assert_eq!(
        entries[0]
            .bip110
            .as_ref()
            .map(|assessment| assessment.status),
        Some(Bip110Status::Compatible)
    );
    assert_eq!(changed_classification(&first.1, &txid).rules.len(), 7);

    let second = tokio::task::spawn_blocking({
        let pipeline = pipeline.clone();
        move || {
            let mut entries = entries;
            let report = pipeline.classify_entries(&mut entries);
            (entries, report)
        }
    })
    .await
    .expect("second classification");
    assert_eq!(second.1.attempted, 0);
    assert_eq!(second.1.newly_classified, 0);

    pipeline
        .current_generation()
        .expect("current generation")
        .work()
        .classifications
        .get_mut(&transaction.compute_wtxid().to_string())
        .expect("cached classification")
        .retryable = true;
    let third = tokio::task::spawn_blocking({
        let pipeline = pipeline.clone();
        let mut entries = second.0;
        move || {
            let report = pipeline.classify_entries(&mut entries);
            (entries, report)
        }
    })
    .await
    .expect("retry classification");
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
    let pipeline = test_pipeline(address);

    let (entries, first) = classify_entries(&pipeline, vec![mempool_entry(&transaction)]).await;
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
    let first_classification = Arc::clone(changed_classification(&first, &txid));
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
            Some(crate::bip110::Missing::ScriptPubKey { input: 0 })
        ));
    }
    {
        let generation = pipeline.current_generation().expect("current generation");
        let cache = generation.work();
        assert!(
            cache
                .classifications
                .get(&wtxid)
                .expect("retryable classification")
                .retryable
        );
    }

    let (entries, second) = classify_entries(&pipeline, entries).await;
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
    let second_classification = changed_classification(&second, &txid);
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
        let generation = pipeline.current_generation().expect("current generation");
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
    let pipeline = test_pipeline(address).with_test_confirmed_wave_limit(1);
    pipeline
        .install_snapshot(test_snapshot(1, &[transaction]))
        .expect("membership");

    let first = pipeline.classify_next().await.expect("first fact wave");
    assert_eq!(first.attempted, 1);
    assert_eq!(first.fact_requests, 1);
    assert_eq!(first.facts_resolved, 1);
    assert_eq!(first.newly_classified, 0);
    assert_eq!(first.disposition, ClassificationDisposition::Continue);
    assert!(!changed_contains(&first, &txid));

    let second = pipeline.classify_next().await.expect("second fact wave");
    assert_eq!(second.attempted, 0);
    assert_eq!(second.fact_requests, 1);
    assert_eq!(second.facts_resolved, 1);
    assert_eq!(second.newly_classified, 0);
    assert_eq!(second.disposition, ClassificationDisposition::Continue);
    assert!(!changed_contains(&second, &txid));

    let third = pipeline.classify_next().await.expect("third fact wave");
    assert_eq!(third.attempted, 0);
    assert_eq!(third.fact_requests, 1);
    assert_eq!(third.facts_resolved, 1);
    assert_eq!(third.newly_classified, 1);
    assert_eq!(third.disposition, ClassificationDisposition::Complete);
    assert_eq!(
        changed_classification(&third, &txid).assessment.status,
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
    let (fixture, address, server) = start_rpc_fixture(RpcFixture::new(transactions.clone())).await;
    let pipeline = test_pipeline(address);
    pipeline
        .install_snapshot(test_snapshot(1, &transactions))
        .expect("membership");

    let report = pipeline.classify_next().await.expect("shared fact");
    assert_eq!(report.attempted, 2);
    assert_eq!(report.fact_requests, 1);
    assert_eq!(report.facts_resolved, 1);
    assert_eq!(report.newly_classified, 2);
    assert_eq!(report.disposition, ClassificationDisposition::Complete);
    assert!(
        changed_classifications(&report)
            .all(|classification| classification.assessment.status == Bip110Status::Compatible)
    );
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
    let pipeline = test_pipeline(address).with_test_confirmed_wave_limit(1);
    pipeline
        .install_snapshot(test_snapshot(1, &[transaction]))
        .expect("membership");

    let first = pipeline
        .classify_next()
        .await
        .expect("malformed first fact");
    assert_eq!(first.response_failures, 1);
    assert_eq!(first.newly_classified, 0);
    assert_eq!(first.disposition, ClassificationDisposition::Continue);

    let second = pipeline.classify_next().await.expect("later fact");
    assert_eq!(second.response_failures, 0);
    assert_eq!(second.facts_resolved, 1);
    assert_eq!(second.newly_classified, 0);
    assert_eq!(second.disposition, ClassificationDisposition::Continue);

    let third = pipeline.classify_next().await.expect("retried first fact");
    assert_eq!(third.response_failures, 0);
    assert_eq!(third.facts_resolved, 1);
    assert_eq!(third.newly_classified, 1);
    assert_eq!(third.disposition, ClassificationDisposition::Complete);
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
    let pipeline = test_pipeline_with_limits(
        address,
        ClassificationLimits::new(1, 1, 16 * 1024 * 1024).expect("limits"),
    );
    pipeline.install_snapshot(membership).expect("membership");

    let first_report = pipeline
        .classify_next()
        .await
        .expect("first poison attempt");
    assert_eq!(first_report.response_failures, 1);
    assert_eq!(first_report.deferred_candidates, 0);
    assert_eq!(
        first_report.disposition,
        ClassificationDisposition::Continue
    );

    let second_report = pipeline
        .classify_next()
        .await
        .expect("bounded poison retry");
    assert_eq!(second_report.response_failures, 1);
    assert_eq!(second_report.deferred_candidates, 1);
    assert_eq!(
        second_report.disposition,
        ClassificationDisposition::Continue
    );
    assert!(!changed_contains(&second_report, &first_txid));

    let third_report = pipeline.classify_next().await.expect("later candidate");
    assert_eq!(third_report.attempted, 1);
    assert_eq!(third_report.newly_classified, 1);
    assert_eq!(
        third_report.disposition,
        ClassificationDisposition::Complete
    );
    assert!(!changed_contains(&third_report, &first_txid));
    assert_eq!(
        changed_classification(&third_report, &second_txid)
            .assessment
            .status,
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
async fn retryable_candidate_raw_failure_is_retried_in_the_same_generation() {
    let transaction = transaction();
    let txid = transaction.compute_txid();
    let outpoint = transaction.input[0].previous_output;
    let rpc = RpcFixture::new([transaction.clone()])
        .with_raw_failure(txid)
        .with_gettxout_results(outpoint, [valid_gettxout_result()]);
    let (fixture, address, server) = start_rpc_fixture(rpc).await;
    let pipeline = test_pipeline(address);
    let initial = pipeline
        .install_snapshot(test_snapshot(1, &[transaction]))
        .expect("membership");
    assert_eq!(initial.revision, 0);
    assert_eq!(
        initial.membership.bip110_summary.unclassified_count, 1,
        "fresh membership remains visible before classification"
    );

    let first = pipeline.classify_next().await.expect("first raw attempt");
    assert_eq!(first.attempted, 1);
    assert_eq!(first.response_failures, 1);
    assert_eq!(first.newly_classified, 0);
    assert_eq!(first.remaining, 1);
    assert_eq!(first.disposition, ClassificationDisposition::Continue);

    let second = pipeline.classify_next().await.expect("raw retry");
    server.abort();

    assert_eq!(second.attempted, 1);
    assert_eq!(second.response_failures, 0);
    assert_eq!(second.newly_classified, 1);
    assert_eq!(second.remaining, 0);
    assert_eq!(second.disposition, ClassificationDisposition::Complete);
    assert_eq!(
        changed_classification(&second, &txid.to_string())
            .assessment
            .status,
        Bip110Status::Compatible
    );
    let fixture = fixture.fixture.lock().await;
    assert_eq!(
        fixture
            .calls
            .iter()
            .filter(|(method, params)| {
                method == "getrawtransaction" && params[0] == txid.to_string()
            })
            .count(),
        2
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn exhausted_candidate_raw_retry_finishes_unclassified_without_looping() {
    let transaction = transaction();
    let txid = transaction.compute_txid();
    let rpc = RpcFixture::new([transaction.clone()]).with_raw_failures(txid, 2);
    let (fixture, address, server) = start_rpc_fixture(rpc).await;
    let pipeline = test_pipeline(address);
    pipeline
        .install_snapshot(test_snapshot(1, &[transaction]))
        .expect("membership");

    let first = pipeline.classify_next().await.expect("first raw attempt");
    assert_eq!(first.remaining, 1);
    assert_eq!(first.disposition, ClassificationDisposition::Continue);

    let second = pipeline.classify_next().await.expect("bounded raw retry");
    assert_eq!(second.attempted, 1);
    assert_eq!(second.response_failures, 1);
    assert_eq!(second.newly_classified, 0);
    assert_eq!(second.remaining, 0);
    assert_eq!(second.disposition, ClassificationDisposition::Complete);
    assert!(!changed_contains(&second, &txid.to_string()));

    let third = pipeline
        .classify_next()
        .await
        .expect("completed generation remains drained");
    server.abort();

    assert_eq!(third.attempted, 0);
    assert_eq!(third.remaining, 0);
    assert_eq!(third.disposition, ClassificationDisposition::Complete);
    let fixture = fixture.fixture.lock().await;
    assert_eq!(
        fixture
            .calls
            .iter()
            .filter(|(method, params)| {
                method == "getrawtransaction" && params[0] == txid.to_string()
            })
            .count(),
        2
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn non_coinbase_null_prevout_is_rejected_without_panicking() {
    let transaction = transaction_spending([OutPoint::null(), confirmed_outpoint(9, 0)]);
    assert!(!transaction.is_coinbase());
    let txid = transaction.compute_txid();
    let rpc = RpcFixture::new([transaction.clone()]);
    let (fixture, address, server) = start_rpc_fixture(rpc).await;
    let pipeline = test_pipeline(address);
    pipeline
        .install_snapshot(test_snapshot(1, &[transaction]))
        .expect("membership");

    for expected_remaining in [1, 0] {
        let report = pipeline
            .classify_next()
            .await
            .expect("malformed raw transaction remains bounded");
        assert_eq!(report.attempted, 1);
        assert_eq!(report.response_failures, 1);
        assert_eq!(report.newly_classified, 0);
        assert_eq!(report.remaining, expected_remaining);
    }

    let drained = pipeline
        .classify_next()
        .await
        .expect("completed generation remains healthy");
    server.abort();

    assert_eq!(drained.attempted, 0);
    assert_eq!(drained.remaining, 0);
    assert_eq!(drained.disposition, ClassificationDisposition::Complete);
    let fixture = fixture.fixture.lock().await;
    assert_eq!(
        fixture
            .calls
            .iter()
            .filter(|(method, params)| {
                method == "getrawtransaction" && params[0] == txid.to_string()
            })
            .count(),
        2
    );
    assert!(
        fixture.calls.iter().all(|(method, _)| method != "gettxout"),
        "a rejected raw transaction must not advance into fact collection"
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
    let pipeline = test_pipeline(address);
    let fetch = tokio::task::spawn_blocking(move || {
        pipeline.fetch_confirmed_prevouts(&[
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
    let pipeline = test_pipeline(address);
    pipeline
        .install_snapshot(test_snapshot(1, &[transaction]))
        .expect("membership");

    let first = pipeline.classify_next().await.expect("missing response");
    assert_eq!(first.response_failures, 1);
    assert_eq!(first.systemic_response_failures, 0);
    assert_eq!(first.missing_responses, 1);
    assert_eq!(first.facts_missing, 0);
    assert_eq!(first.newly_classified, 0);
    assert_eq!(first.disposition, ClassificationDisposition::Continue);
    assert!(!changed_contains(&first, &txid));

    let second = pipeline.classify_next().await.expect("response retry");
    server.abort();

    assert_eq!(second.missing_responses, 0);
    assert_eq!(second.facts_resolved, 1);
    assert_eq!(second.newly_classified, 1);
    assert_eq!(second.disposition, ClassificationDisposition::Complete);
    assert_eq!(
        changed_classification(&second, &txid).assessment.status,
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
    let pipeline = test_pipeline(address);
    pipeline
        .install_snapshot(test_snapshot(1, &[transaction]))
        .expect("membership");

    let first = pipeline.classify_next().await.expect("local failures");
    assert_eq!(first.response_failures, 2);
    assert_eq!(first.missing_responses, 0);
    assert_eq!(first.deferred_candidates, 0);
    assert_eq!(first.disposition, ClassificationDisposition::Continue);

    let second = pipeline.classify_next().await.expect("all missing batch");
    server.abort();

    assert_eq!(second.response_failures, 2);
    assert_eq!(second.systemic_response_failures, 0);
    assert_eq!(second.missing_responses, 2);
    assert_eq!(second.facts_missing, 0);
    assert_eq!(second.deferred_candidates, 0);
    assert_eq!(second.remaining, 1);
    assert!(!second.complete);
    assert_eq!(second.disposition, ClassificationDisposition::Paused);
}

#[tokio::test(flavor = "multi_thread")]
async fn systemic_fact_failure_pauses_even_after_candidate_raw_progress() {
    let transaction = transaction();
    let txid = transaction.compute_txid().to_string();
    let outpoint = transaction.input[0].previous_output;
    let rpc = RpcFixture::new([transaction.clone()])
        .with_gettxout_responses(outpoint, [FixtureResponse::SystemicError]);
    let (_, address, server) = start_rpc_fixture(rpc).await;
    let pipeline = test_pipeline(address);
    pipeline
        .install_snapshot(test_snapshot(1, &[transaction]))
        .expect("membership");

    let report = pipeline.classify_next().await.expect("systemic response");
    server.abort();

    assert_eq!(report.attempted, 1);
    assert_eq!(report.fact_requests, 1);
    assert_eq!(report.response_failures, 1);
    assert_eq!(report.systemic_response_failures, 1);
    assert_eq!(report.missing_responses, 0);
    assert_eq!(report.newly_classified, 0);
    assert_eq!(report.remaining, 1);
    assert!(!report.complete);
    assert_eq!(report.disposition, ClassificationDisposition::Paused);
    assert!(!changed_contains(&report, &txid));
}

#[tokio::test(flavor = "multi_thread")]
async fn framing_failure_pauses_with_only_the_batch_failure_counter() {
    let transaction = transaction();
    let (address, server) = start_invalid_classification_fixture().await;
    let pipeline = test_pipeline(address);
    pipeline
        .install_snapshot(test_snapshot(1, &[transaction]))
        .expect("membership");

    let report = pipeline
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
    assert_eq!(report.disposition, ClassificationDisposition::Paused);
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
    let pipeline = test_pipeline_with_limits(
        address,
        ClassificationLimits::new(300, 1, 16 * 1024 * 1024).expect("limits"),
    );
    pipeline.install_snapshot(membership).expect("membership");

    let report = pipeline.classify_next().await.expect("systemic raw batch");
    server.abort();

    assert_eq!(report.attempted, 300);
    assert_eq!(report.systemic_response_failures, 1);
    assert_eq!(report.fact_requests, 0);
    assert_eq!(report.newly_classified, 0);
    assert_eq!(report.disposition, ClassificationDisposition::Paused);
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
    let pipeline = test_pipeline_with_limits(
        address,
        ClassificationLimits::new(1, 1, 16 * 1024 * 1024).expect("limits"),
    );
    let membership = test_snapshot(1, &[parent, child]);
    assert_eq!(membership.transactions[0].txid, child_txid);
    let initial = pipeline.install_snapshot(membership).expect("membership");
    assert!(initial.membership.transactions[0].bip110.is_none());

    let first = pipeline.classify_next().await.expect("failed parent raw");
    assert_eq!(first.newly_classified, 0);
    assert_eq!(first.facts_missing, 0);
    assert_eq!(first.disposition, ClassificationDisposition::Continue);
    assert!(!changed_contains(&first, &child_txid));

    let second = pipeline.classify_next().await.expect("fallback null retry");
    assert_eq!(second.attempted, 0);
    assert_eq!(second.newly_classified, 0);
    assert_eq!(second.facts_missing, 0);
    assert_eq!(second.deferred_candidates, 1);
    assert_eq!(second.disposition, ClassificationDisposition::Continue);
    assert!(!changed_contains(&second, &child_txid));

    let third = pipeline.classify_next().await.expect("later candidate");
    assert_eq!(third.attempted, 1);
    assert_eq!(third.newly_classified, 1);
    assert_eq!(third.disposition, ClassificationDisposition::Complete);
    assert!(!changed_contains(&third, &child_txid));
    assert_eq!(
        changed_classification(&third, &parent_txid_string)
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
            .filter(|(method, params)| { method == "getrawtransaction" && params[0] == child_txid })
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
    let pipeline = test_pipeline(address);

    let (entries, report) = classify_entries(&pipeline, entries).await;
    server.abort();

    assert_eq!(report.newly_classified, 2);
    assert_eq!(report.response_failures, 0);
    assert_eq!(
        changed_classification(&report, &child_txid)
            .assessment
            .status,
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
            .filter(|(method, params)| { method == "getrawtransaction" && params[0] == child_txid })
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
    let pipeline = test_pipeline(address);

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
    pipeline.install_snapshot(first).expect("first generation");
    pipeline.classify_next().await.expect("parent slice");

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
    let immediate = pipeline
        .install_snapshot(second)
        .expect("second generation");
    assert_eq!(immediate.classifications.len(), 1);
    conflict_facts::assert_exact_carry(&immediate);
    let report = pipeline.classify_next().await.expect("child slice");
    assert_eq!(
        changed_classification(&report, &child_txid)
            .assessment
            .status,
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
    pipeline.install_snapshot(empty).expect("empty generation");
    let generation = pipeline.current_generation().expect("current generation");
    {
        let work = generation.work();
        assert!(work.classifications.is_empty());
        assert!(work.outputs.is_empty());
        assert!(work.attempted.is_empty());
        assert!(work.raw_retry_used.is_empty());
        assert!(work.pending.is_none());
        assert_eq!(work.output_cache_bytes, 0);
    }
    assert!(
        pipeline.coordinator().confirmed_cache.estimated_bytes
            <= pipeline.limits.max_auxiliary_cache_bytes
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
            .filter(|(method, params)| { method == "getrawtransaction" && params[0] == child_txid })
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
    let pipeline = test_pipeline(address);

    pipeline
        .install_snapshot(test_snapshot(1, &[first]))
        .expect("first generation");
    let first_report = pipeline.classify_next().await.expect("first child");
    assert_eq!(first_report.newly_classified, 1);
    assert_eq!(
        changed_classification(&first_report, &first_txid)
            .assessment
            .status,
        Bip110Status::Compatible
    );

    pipeline
        .install_snapshot(test_snapshot(2, &[second]))
        .expect("second generation");
    let second_report = pipeline.classify_next().await.expect("second child");
    assert_eq!(second_report.newly_classified, 1);
    assert_eq!(second_report.fact_requests, 0);
    assert_eq!(second_report.facts_resolved, 1);
    assert_eq!(
        changed_classification(&second_report, &second_txid)
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
    let pipeline = test_pipeline(address);
    let (_, first) = classify_entries(&pipeline, vec![mempool_entry(&first_variant)]).await;
    assert_eq!(
        changed_classification(&first, &txid).wtxid,
        first_wtxid,
        "the first witness variant is cached"
    );

    fixture
        .fixture
        .lock()
        .await
        .raw_transactions
        .insert(txid.clone(), serialize_hex(&second_variant));
    let (entries, second) = classify_entries(&pipeline, vec![mempool_entry(&second_variant)]).await;
    server.abort();

    assert_eq!(second.attempted, 1);
    assert_eq!(second.newly_classified, 1);
    assert_eq!(changed_classification(&second, &txid).wtxid, second_wtxid);
    assert_eq!(
        entries[0]
            .bip110
            .as_ref()
            .map(|assessment| assessment.status),
        Some(Bip110Status::Compatible)
    );
    let generation = pipeline.current_generation().expect("current generation");
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
    let pipeline = test_pipeline_with_limits(
        address,
        ClassificationLimits::new(1, 4, 16 * 1024 * 1024).expect("limits"),
    );

    let (entries, first) = classify_entries(&pipeline, entries).await;
    assert_eq!(first.attempted, 1);
    assert_eq!(
        changed_classification(&first, &first_txid)
            .assessment
            .status,
        Bip110Status::Indeterminate
    );
    assert!(!changed_contains(&first, &second_txid));

    let (_, second) = classify_entries(&pipeline, entries).await;
    server.abort();

    assert_eq!(second.attempted, 1);
    assert_eq!(
        changed_classification(&second, &second_txid)
            .assessment
            .status,
        Bip110Status::Compatible
    );
    assert!(!changed_contains(&second, &first_txid));
    assert!(
        pipeline
            .current_generation()
            .expect("current generation")
            .work()
            .classifications
            .values()
            .any(|cached| cached.classification.txid == first_txid),
        "the retryable cached result remains retained while fresh work runs first"
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
    let pipeline = test_pipeline_with_limits(
        address,
        ClassificationLimits::new(1, 1, cache_limit).expect("one-script cache limit"),
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
    pipeline.install_snapshot(membership).expect("generation");

    let report = pipeline.classify_next().await.expect("slice");
    server.abort();

    assert_eq!(report.newly_classified, 1);
    let generation = pipeline.current_generation().expect("current generation");
    {
        let coordinator = pipeline.coordinator();
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
    let pipeline = test_pipeline_with_limits(
        address,
        ClassificationLimits::new(10, 4, pending_limit).expect("small source cache budget"),
    );
    assert_eq!(
        pipeline.resolver_limits.max_pending_script_bytes,
        pending_limit
    );
    let initial = pipeline
        .install_snapshot(test_snapshot(1, &[transaction]))
        .expect("generation");
    assert!(initial.membership.transactions[0].bip110.is_none());

    let first = pipeline.classify_next().await.expect("capacity deferral");
    assert!(first.capacity_deferred >= 1);
    assert_eq!(first.deferred_candidates, 1);
    assert_eq!(first.newly_classified, 0);
    assert_eq!(first.disposition, ClassificationDisposition::Complete);
    assert!(!changed_contains(&first, &txid));
    {
        let generation = pipeline.current_generation().expect("generation");
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
    let (fixture, address, server) = start_rpc_fixture(RpcFixture::new(transactions.clone())).await;
    let pipeline = test_pipeline(address).with_test_pending_script_limit(script_bytes);
    {
        let mut coordinator = pipeline.coordinator();
        for outpoint in outpoints {
            assert!(
                coordinator
                    .confirmed_cache
                    .insert(outpoint, script.clone(), 0,)
            );
        }
    }
    pipeline
        .install_snapshot(test_snapshot(1, &transactions))
        .expect("membership");

    let first = pipeline.classify_next().await.expect("bounded recovery");
    server.abort();

    assert_eq!(first.fact_requests, 0);
    assert_eq!(first.newly_classified, 3);
    assert_eq!(first.deferred_candidates, 0);
    assert_eq!(first.classified, 3);
    assert_eq!(first.remaining, 0);
    assert_eq!(first.disposition, ClassificationDisposition::Complete);
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
    let pipeline = test_pipeline_with_limits(
        address,
        ClassificationLimits::new(3, 1, 1).expect("tiny auxiliary cache"),
    )
    .with_test_pending_script_limit(estimated_script_bytes(&script));
    pipeline.install_snapshot(membership).expect("membership");

    let report = pipeline.classify_next().await.expect("transient recovery");
    server.abort();

    assert_eq!(report.attempted, 3);
    assert_eq!(report.fact_requests, 1);
    assert_eq!(report.newly_classified, 3);
    assert_eq!(report.deferred_candidates, 0);
    assert_eq!(report.remaining, 1);
    assert_eq!(report.disposition, ClassificationDisposition::Continue);
    assert!(
        child_txids
            .iter()
            .all(|txid| changed_contains(&report, txid))
    );
    let generation = pipeline.current_generation().expect("generation");
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
    let generation = ClassificationGeneration {
        id: 1,
        membership,
        variants,
        work: Mutex::new(ClassificationWork::default()),
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
    let pipeline = test_pipeline(SocketAddr::from(([127, 0, 0, 1], 1)))
        .with_test_pending_script_limit(estimated_script_bytes(&transient_script));
    pipeline
        .install_snapshot(test_snapshot(1, &transactions))
        .expect("membership");
    let generation = pipeline.current_generation().expect("generation");
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
    let mut advance = ClassificationAdvance::default();
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
        pipeline
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
async fn multi_victim_facts_are_consumed_before_transient_loss() {
    for (case, source) in [
        ("current mempool parent", FactSource::CurrentParent),
        ("confirmed prevout", FactSource::Confirmed),
    ] {
        println!("checking {case}");
        assert_multi_victim_transient_fact_is_consumed(source).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn late_results_from_a_replaced_generation_are_discarded() {
    let transaction = transaction();
    let (fixture, address, server) = start_rpc_fixture(
        RpcFixture::new([transaction.clone()]).with_delay(Duration::from_millis(50)),
    )
    .await;
    let pipeline = test_pipeline(address);
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
    pipeline
        .install_snapshot(membership)
        .expect("first generation");

    let task = tokio::spawn({
        let pipeline = pipeline.clone();
        async move { pipeline.classify_next().await.expect("stale slice") }
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
    let current = pipeline
        .install_snapshot(replacement)
        .expect("replacement generation");
    let stale = task.await.expect("classification task");
    server.abort();

    assert!(stale.stale);
    assert!(stale.publication.is_none());
    assert!(current.classifications.is_empty());
    let generation = pipeline.current_generation().expect("current generation");
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
    let pipeline = test_pipeline(address);
    let old = pipeline
        .install_snapshot(test_snapshot(1, &[transaction]))
        .expect("old membership");
    let classifier = tokio::spawn({
        let pipeline = pipeline.clone();
        async move {
            pipeline
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
    let replacement = pipeline
        .install_snapshot(test_snapshot(2, &[]))
        .expect("replacement membership");
    assert_eq!(replacement.revision, 0);
    assert!(
        !pipeline
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
        !pipeline
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
    let pipeline = test_pipeline_with_limits(
        address,
        ClassificationLimits::new(1, 1, 16 * 1024 * 1024).expect("limits"),
    );
    let publication = pipeline
        .install_snapshot(test_snapshot(1, &transactions))
        .expect("membership");

    let (first, second) = tokio::join!(
        pipeline.classify_next_for_generation(Some(publication.generation)),
        pipeline.classify_next_for_generation(Some(publication.generation)),
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
        pipeline
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
    let pipeline = test_pipeline(address);
    let entered = Shared::new(Barrier::new(2));
    let release = Shared::new(Barrier::new(2));
    let installer = tokio::task::spawn_blocking({
        let pipeline = pipeline.clone();
        let entered = Shared::clone(&entered);
        let release = Shared::clone(&release);
        move || {
            pipeline.install_snapshot_before_exposure(test_snapshot(1, &[transaction]), || {
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
        let pipeline = pipeline.clone();
        async move { pipeline.classify_next().await }
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
    assert_eq!(publication.membership.classification_revision, 0);
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
    let pipeline = test_pipeline_with_limits(
        address,
        ClassificationLimits::new(1_300, 4, 16 * 1024 * 1024).expect("limits"),
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
    pipeline.install_snapshot(membership).expect("generation");

    let report = pipeline.classify_next().await.expect("parallel slice");
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
    let pipeline = test_pipeline(address);

    let fetched = pipeline
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
    let conflicting_script = ScriptBuf::from_hex("00141111111111111111111111111111111111111111")
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
    let pipeline = test_pipeline_with_limits(
        address,
        ClassificationLimits::new(1, 1, maximum).expect("exact shared-cache limit"),
    );
    pipeline
        .install_snapshot(test_snapshot(1, std::slice::from_ref(&transaction)))
        .expect("first membership");
    let report = pipeline
        .classify_next()
        .await
        .expect("first classification");
    assert_eq!(report.newly_classified, 1);
    {
        let generation = pipeline.current_generation().expect("first generation");
        let mut coordinator = pipeline.coordinator();
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

    let replacement = pipeline
        .install_snapshot(test_snapshot(2, &[transaction]))
        .expect("replacement membership");
    server.abort();

    assert_eq!(replacement.revision, 0);
    {
        let generation = pipeline
            .current_generation()
            .expect("replacement generation");
        let coordinator = pipeline.coordinator();
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
    let mut work = ClassificationWork::default();

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
    let pipeline = ClassificationPipeline::new(
        "http://127.0.0.1:1/",
        "atlas".to_owned(),
        "secret".to_owned(),
        ClassificationLimits::new(1, 1, 1).expect("limits"),
    )
    .expect("pipeline");
    let outpoints =
        vec![OutPoint::null(); maximum_confirmed_prevouts_per_batch().saturating_add(1)];

    let error = pipeline
        .fetch_confirmed_prevouts(&outpoints)
        .expect_err("oversized estimate");

    assert!(matches!(
        error,
        ClassificationError::BatchResponseEstimateTooLarge {
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
    let pipeline = test_pipeline(address);
    let fetch =
        tokio::task::spawn_blocking(move || pipeline.fetch_confirmed_prevouts(&[OutPoint::null()]))
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
fn classification_limits_must_be_nonzero() {
    assert!(ClassificationLimits::new(0, 1, 1).is_err());
    assert!(ClassificationLimits::new(1, 0, 1).is_err());
    assert!(ClassificationLimits::new(1, MAX_RPC_LANES + 1, 1).is_err());
    assert!(ClassificationLimits::new(1, 1, 0).is_err());
    assert!(ClassificationLimits::new(1, 1, MAX_AUXILIARY_CACHE_BYTES + 1).is_err());
    assert!(ClassificationLimits::new(2_048, 4, 256 * 1024 * 1024).is_ok());
    assert!(ClassificationLimits::new(MAX_TRANSACTIONS_PER_SLICE, 1, 1).is_ok());
    assert!(ClassificationLimits::new(MAX_TRANSACTIONS_PER_SLICE + 1, 1, 1).is_err());
}

#[test]
fn pending_script_budget_is_capped_by_source_budget_and_global_ceiling() {
    for (case, transactions_per_slice, lanes, budget, expected_pending_budget) in [
        ("small source budget", 1, 1, 64 * 1024, 64 * 1024),
        (
            "production budget",
            2_048,
            4,
            256 * 1024 * 1024,
            MAX_PENDING_SCRIPT_BYTES,
        ),
    ] {
        let limits = ClassificationLimits::new(transactions_per_slice, lanes, budget)
            .unwrap_or_else(|error| panic!("{case}: {error}"));
        let pipeline = ClassificationPipeline::new(
            "http://127.0.0.1:1/",
            "atlas".to_owned(),
            "secret".to_owned(),
            limits,
        )
        .unwrap_or_else(|error| panic!("{case}: {error}"));

        assert_eq!(pipeline.limits.max_auxiliary_cache_bytes, budget, "{case}");
        assert_eq!(
            pipeline.resolver_limits.max_pending_script_bytes, expected_pending_budget,
            "{case}"
        );
    }
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
                                crate::bip110::Violation::WitnessItemTooLarge {
                                    input: 1,
                                    item_index: 0,
                                    len: 257,
                                    limit: 256,
                                },
                                crate::bip110::Violation::WitnessItemTooLarge {
                                    input: 2,
                                    item_index: 0,
                                    len: 300,
                                    limit: 256,
                                },
                            ],
                            missing: vec![
                                crate::bip110::Missing::ScriptPubKey { input: 0 },
                                crate::bip110::Missing::ScriptPubKey { input: 3 },
                            ],
                        }
                    } else {
                        RuleVerdict::Pass
                    },
                }
            })
            .collect(),
    };

    let classification = classification_from_evidence(
        "txid".to_owned(),
        "wtxid".to_owned(),
        TransactionStructure::new(1, 2, 0, 50_000, 107).expect("structure"),
        evidence,
    );

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
