//! Golden read-API fixtures under `fixtures/api/` are a checked contract:
//! the manifest must list exactly the files on disk, every fixture must
//! round-trip through the current wire types, and every fixture must equal
//! the live response the router produces for the recorded request against
//! the deterministic seed store. Regenerate with
//! `just regen-api-fixtures` after intentional contract changes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use atlas_model::{
    Evidence, MempoolEntryFacts, MempoolSummary, NormalizedEvent, ReconciledMembership, SourceId,
    SourceSessionId, SourcesResponse,
};
use atlas_server::{Store, router_with_clock};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use bitcoin::absolute::LockTime;
use bitcoin::hashes::Hash;
use bitcoin::transaction::Version;
use bitcoin::{
    Amount, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, WPubkeyHash, Witness,
};
use serde::{Deserialize, Serialize};
use tower::ServiceExt;

/// The deterministic instant every fixture response is computed at.
const AS_OF_MS: u64 = 1_752_710_400_000;

const MINUTE_MS: u64 = 60_000;
const HOUR_MS: u64 = 3_600_000;
const DAY_MS: u64 = 86_400_000;

#[derive(Clone, Copy)]
struct FixtureSpec {
    path: &'static str,
    method: &'static str,
    request: &'static str,
    status: u16,
    body: BodyKind,
    scenario: &'static str,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum BodyKind {
    Sources,
    MempoolSummary,
    Error,
}

const SPECS: [FixtureSpec; 5] = [
    FixtureSpec {
        path: "sources.json",
        method: "GET",
        request: "/api/v1/sources",
        status: 200,
        body: BodyKind::Sources,
        scenario: "two sources with fact-bearing and awaiting-RPC memberships",
    },
    FixtureSpec {
        path: "mempool-summary-default.json",
        method: "GET",
        request: "/api/v1/sources/source-a/mempool/summary",
        status: 200,
        body: BodyKind::MempoolSummary,
        scenario: "unfiltered summary without detail blocks, over a source with \
                   derived and underived rows, awaiting-RPC entries, and a \
                   possible capture gap",
    },
    FixtureSpec {
        path: "mempool-summary-filtered.json",
        method: "GET",
        request: "/api/v1/sources/source-a/mempool/summary\
                  ?class=unknown&feerate_min=1&feerate_max=64",
        status: 200,
        body: BodyKind::MempoolSummary,
        scenario: "classification facet plus inclusive fee-rate bounds shrink the \
                   matching set while totals.all is unchanged",
    },
    FixtureSpec {
        path: "mempool-summary-detail.json",
        method: "GET",
        request: "/api/v1/sources/source-a/mempool/summary?detail=ecdf,joint",
        status: 200,
        body: BodyKind::MempoolSummary,
        scenario: "optional ECDF and joint fee-size detail blocks",
    },
    FixtureSpec {
        path: "error-unknown-source.json",
        method: "GET",
        request: "/api/v1/sources/absent-node/mempool/summary",
        status: 404,
        body: BodyKind::Error,
        scenario: "summary for an unknown source",
    },
];

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    fixtures: Vec<ManifestEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ManifestEntry {
    path: String,
    method: String,
    request: String,
    status: u16,
    body: BodyKind,
    scenario: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ErrorBody {
    error: String,
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/api")
}

fn manifest_from_specs() -> Manifest {
    Manifest {
        fixtures: SPECS
            .iter()
            .map(|spec| ManifestEntry {
                path: spec.path.to_owned(),
                method: spec.method.to_owned(),
                request: spec.request.to_owned(),
                status: spec.status,
                body: spec.body,
                scenario: spec.scenario.to_owned(),
            })
            .collect(),
    }
}

fn txid(index: u64) -> String {
    format!("{index:064x}")
}

fn raw_input(byte: u8) -> TxIn {
    TxIn {
        previous_output: OutPoint {
            txid: bitcoin::Txid::from_byte_array([byte; 32]),
            vout: 0,
        },
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness: Witness::from_slice(&[vec![0xab; 107]]),
    }
}

fn p2wpkh_output(byte: u8, sats: u64) -> TxOut {
    TxOut {
        value: Amount::from_sat(sats),
        script_pubkey: ScriptBuf::new_p2wpkh(&WPubkeyHash::from_byte_array([byte; 20])),
    }
}

/// Deterministic 2-in/2-out P2WPKH transaction; classifies as `payment`.
fn payment_transaction() -> Transaction {
    Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![raw_input(0x21), raw_input(0x22)],
        output: vec![p2wpkh_output(0x31, 4_000_000), p2wpkh_output(0x32, 950_000)],
    }
}

/// Deterministic transaction carrying an OP_RETURN output; classifies as `data`.
fn data_transaction() -> Transaction {
    let payload =
        bitcoin::script::PushBytesBuf::try_from(b"atlas-fixture".to_vec()).expect("push bytes");
    Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![raw_input(0x23)],
        output: vec![
            TxOut {
                value: Amount::from_sat(0),
                script_pubkey: ScriptBuf::new_op_return(payload),
            },
            p2wpkh_output(0x33, 120_000),
        ],
    }
}

fn p2p_evidence(transaction: &Transaction) -> Evidence {
    Evidence::P2pTransaction {
        txid: transaction.compute_txid().to_string(),
        wtxid: transaction.compute_wtxid().to_string(),
        raw_transaction_hex: Some(hex::encode(bitcoin::consensus::encode::serialize(
            transaction,
        ))),
        peer_id: Some(7),
        inbound: Some(true),
    }
}

fn seed_events() -> Vec<NormalizedEvent> {
    let payment = payment_transaction();
    let data = data_transaction();
    let source_a = SourceId::new("source-a").expect("source");
    let source_b = SourceId::new("source-b").expect("source");
    let session_a = SourceSessionId::new("session-a").expect("session");
    let session_b = SourceSessionId::new("session-b").expect("session");
    let event = |source: &SourceId, session: &SourceSessionId, sequence, evidence| {
        NormalizedEvent::new(
            source.clone(),
            session.clone(),
            sequence,
            AS_OF_MS - HOUR_MS + sequence * MINUTE_MS,
            AS_OF_MS - HOUR_MS + sequence * MINUTE_MS + 5,
            evidence,
        )
        .expect("seed event")
    };
    let present = |txid: String, vsize, fee_sats, age_ms: u64| Evidence::MempoolReconciled {
        txid,
        membership: ReconciledMembership::Present {
            facts: MempoolEntryFacts {
                vsize,
                fee_sats,
                entered_at_ms: AS_OF_MS - age_ms,
            },
        },
    };

    vec![
        event(
            &source_a,
            &session_a,
            1,
            Evidence::CaptureGap {
                input: "peer_observer_nats".to_owned(),
                reason: "disconnected".to_owned(),
                certainty: atlas_model::CaptureGapCertainty::PossibleLoss,
            },
        ),
        // Fee rates and ages chosen to spread across the canonical bins:
        // 8.5 sat/vB fresh, 1.0 at 30m, 1.5 at 2h, 64 at 30h, 0.25 at 4d,
        // 425.5 at 12h.
        event(
            &source_a,
            &session_a,
            2,
            present(txid(1), 141, 1_200, 5 * MINUTE_MS),
        ),
        event(
            &source_a,
            &session_a,
            3,
            present(txid(2), 250, 250, 30 * MINUTE_MS),
        ),
        event(
            &source_a,
            &session_a,
            4,
            present(txid(3), 3_400, 5_100, 2 * HOUR_MS),
        ),
        event(
            &source_a,
            &session_a,
            5,
            present(txid(4), 500, 32_000, 30 * HOUR_MS),
        ),
        event(
            &source_a,
            &session_a,
            6,
            present(txid(5), 800, 200, 4 * DAY_MS),
        ),
        event(
            &source_a,
            &session_a,
            7,
            present(txid(6), 141, 60_000, 12 * HOUR_MS),
        ),
        event(
            &source_a,
            &session_a,
            8,
            Evidence::MempoolAdded { txid: txid(7) },
        ),
        event(
            &source_a,
            &session_a,
            9,
            Evidence::MempoolAdded { txid: txid(8) },
        ),
        // Two transactions observed with raw bytes over P2P, so their shape
        // facts and classifier verdicts derive; the rest stay underived.
        event(&source_a, &session_a, 10, p2p_evidence(&payment)),
        event(
            &source_a,
            &session_a,
            11,
            present(
                payment.compute_txid().to_string(),
                u64::try_from(payment.vsize()).expect("payment vsize"),
                2_400,
                20 * MINUTE_MS,
            ),
        ),
        event(&source_a, &session_a, 12, p2p_evidence(&data)),
        event(
            &source_a,
            &session_a,
            13,
            present(
                data.compute_txid().to_string(),
                u64::try_from(data.vsize()).expect("data vsize"),
                9_000,
                3 * HOUR_MS,
            ),
        ),
        event(
            &source_b,
            &session_b,
            1,
            present(txid(9), 200, 400, HOUR_MS),
        ),
    ]
}

fn seed_router(directory: &Path) -> Router {
    let database = directory.join("atlas.db");
    Store::migrate(&database).expect("migrate seed store");
    let store = Store::open(database).expect("open seed store");
    for event in seed_events() {
        store.ingest(&event).expect("seed event ingest");
    }
    router_with_clock(store, || AS_OF_MS)
}

async fn issue(application: Router, spec: &FixtureSpec) -> (StatusCode, serde_json::Value) {
    let response = application
        .oneshot(
            Request::builder()
                .method(spec.method)
                .uri(spec.request)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let value = serde_json::from_slice(&bytes).expect("response is JSON");
    (status, value)
}

fn read_fixture(path: &str) -> serde_json::Value {
    let file = fixtures_dir().join(path);
    let contents = std::fs::read_to_string(&file).unwrap_or_else(|error| {
        panic!(
            "reading {}: {error}; run just regen-api-fixtures",
            file.display()
        )
    });
    serde_json::from_str(&contents).expect("fixture is JSON")
}

fn assert_round_trip<T>(value: &serde_json::Value, path: &str)
where
    T: Serialize + for<'de> Deserialize<'de>,
{
    let typed: T = serde_json::from_value(value.clone())
        .unwrap_or_else(|error| panic!("{path} no longer matches the wire types: {error}"));
    assert_eq!(
        serde_json::to_value(typed).expect("serialize"),
        *value,
        "{path} carries fields the wire types no longer produce",
    );
}

#[test]
fn manifest_lists_exactly_the_fixture_files() {
    let manifest_raw =
        std::fs::read_to_string(fixtures_dir().join("manifest.json")).expect("manifest.json");
    let manifest: Manifest = serde_json::from_str(&manifest_raw).expect("manifest parses");

    let listed: BTreeSet<String> = manifest
        .fixtures
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    let expected: BTreeSet<String> = SPECS.iter().map(|spec| spec.path.to_owned()).collect();
    assert_eq!(
        listed, expected,
        "manifest entries drifted from the spec list"
    );

    let on_disk: BTreeSet<String> = std::fs::read_dir(fixtures_dir())
        .expect("fixtures directory")
        .map(|entry| {
            entry
                .expect("directory entry")
                .file_name()
                .into_string()
                .expect("utf-8")
        })
        .filter(|name| name.ends_with(".json") && name != "manifest.json")
        .collect();
    assert_eq!(on_disk, expected, "fixture files drifted from the manifest");

    let expected_manifest =
        serde_json::to_value(manifest_from_specs()).expect("serialize manifest");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&manifest_raw).expect("manifest value"),
        expected_manifest,
        "manifest contents drifted from the spec list"
    );
}

#[test]
fn fixtures_round_trip_through_current_wire_types() {
    for spec in &SPECS {
        let value = read_fixture(spec.path);
        match spec.body {
            BodyKind::Sources => assert_round_trip::<SourcesResponse>(&value, spec.path),
            BodyKind::MempoolSummary => assert_round_trip::<MempoolSummary>(&value, spec.path),
            BodyKind::Error => assert_round_trip::<ErrorBody>(&value, spec.path),
        }
    }
}

#[tokio::test]
async fn fixtures_equal_live_responses_from_the_seed_store() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let application = seed_router(temporary.path());
    for spec in &SPECS {
        let (status, live) = issue(application.clone(), spec).await;
        assert_eq!(status.as_u16(), spec.status, "{}", spec.path);
        assert_eq!(
            live,
            read_fixture(spec.path),
            "{} drifted from the live response; run just regen-api-fixtures and review the diff",
            spec.path
        );
    }
}

#[tokio::test]
#[ignore = "rewrites fixtures/api from the deterministic seed store; run via just regen-api-fixtures"]
async fn regenerate_api_fixtures() {
    let directory = fixtures_dir();
    std::fs::create_dir_all(&directory).expect("fixtures directory");
    let temporary = tempfile::tempdir().expect("temporary directory");
    let application = seed_router(temporary.path());
    for spec in &SPECS {
        let (status, value) = issue(application.clone(), spec).await;
        assert_eq!(status.as_u16(), spec.status, "{}", spec.path);
        write_pretty(&directory.join(spec.path), &value);
    }
    write_pretty(
        &directory.join("manifest.json"),
        &serde_json::to_value(manifest_from_specs()).expect("serialize manifest"),
    );
}

fn write_pretty(path: &Path, value: &serde_json::Value) {
    let mut contents = serde_json::to_string_pretty(value).expect("serialize fixture");
    contents.push('\n');
    std::fs::write(path, contents)
        .unwrap_or_else(|error| panic!("writing {}: {error}", path.display()));
}
