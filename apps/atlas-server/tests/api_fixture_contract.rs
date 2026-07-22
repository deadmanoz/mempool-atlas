//! Golden read-API fixtures under `fixtures/api/` are a checked contract:
//! the manifest must list exactly the files on disk, every fixture must
//! round-trip through the current wire types, and every fixture must equal
//! the live response the router produces for the recorded request against
//! the deterministic seed store. Regenerate with
//! `just regen-api-fixtures` after intentional contract changes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use atlas_model::{
    Evidence, MembershipMutation, MempoolEntryFacts, MempoolSummary, NormalizedEvent,
    ReconciledMembership, SourceComparison, SourceId, SourceRejections, SourceReplicaEntry,
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

mod support;

use support::install_checkpoint;

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
    /// Which seed store the request is issued against. Comparison fixtures use
    /// a separate fork store so the single-source fixtures stay byte-identical.
    seed: SeedKind,
    scenario: &'static str,
}

/// The deterministic seed store a fixture is issued against.
#[derive(Clone, Copy, Eq, PartialEq)]
enum SeedKind {
    /// The primary single-source seed backing sources, summaries, and the
    /// production not-collected rejection response.
    Primary,
    /// The three-source fork seed backing the comparison fixture.
    Fork,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum BodyKind {
    Sources,
    MempoolSummary,
    Rejections,
    Comparison,
    Error,
}

const SPECS: [FixtureSpec; 7] = [
    FixtureSpec {
        path: "sources.json",
        method: "GET",
        request: "/api/v1/sources",
        status: 200,
        body: BodyKind::Sources,
        seed: SeedKind::Primary,
        scenario: "two active SourceReplica projections with complete RPC facts",
    },
    FixtureSpec {
        path: "mempool-summary-default.json",
        method: "GET",
        request: "/api/v1/sources/source-a/mempool/summary",
        status: 200,
        body: BodyKind::MempoolSummary,
        seed: SeedKind::Primary,
        scenario: "unfiltered summary without detail blocks over complete active \
                   state with derived and underived rows; capture collection is \
                   explicitly not yet implemented",
    },
    FixtureSpec {
        path: "mempool-summary-filtered.json",
        method: "GET",
        request: "/api/v1/sources/source-a/mempool/summary\
                  ?t.behavior=unknown&feerate_min=1&feerate_max=64",
        status: 200,
        body: BodyKind::MempoolSummary,
        seed: SeedKind::Primary,
        scenario: "behavior-taxonomy verdict facet plus inclusive fee-rate bounds \
                   shrink the matching set while totals.all is unchanged",
    },
    FixtureSpec {
        path: "mempool-summary-detail.json",
        method: "GET",
        request: "/api/v1/sources/source-a/mempool/summary?detail=ecdf,joint",
        status: 200,
        body: BodyKind::MempoolSummary,
        seed: SeedKind::Primary,
        scenario: "optional ECDF and joint fee-size detail blocks",
    },
    FixtureSpec {
        path: "rejections.json",
        method: "GET",
        request: "/api/v1/sources/source-a/rejections",
        status: 200,
        body: BodyKind::Rejections,
        seed: SeedKind::Primary,
        scenario: "production state-only source reports rejection evidence as \
                   not collected instead of presenting an empty observed window",
    },
    FixtureSpec {
        path: "compare.json",
        method: "GET",
        request: "/api/v1/sources/compare?sources=knots,core,libre-relay",
        status: 200,
        body: BodyKind::Comparison,
        seed: SeedKind::Fork,
        scenario: "ordered three-source comparison (knots, core, libre-relay): a \
                   shared fact-bearing intersection, a data tx added at each \
                   stage, a non-empty knots-only \
                   anomaly in the first stage, and an empty anomaly in the second",
    },
    FixtureSpec {
        path: "error-unknown-source.json",
        method: "GET",
        request: "/api/v1/sources/absent-node/mempool/summary",
        status: 404,
        body: BodyKind::Error,
        seed: SeedKind::Primary,
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

/// Deterministic OP_RETURN carrier distinct from `data_transaction`, used as
/// the classified rejected transaction: its bytes are seen over P2P so it
/// derives verdicts, but it never enters membership.
fn rejected_data_transaction() -> Transaction {
    let payload =
        bitcoin::script::PushBytesBuf::try_from(b"atlas-rejected".to_vec()).expect("push bytes");
    Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![raw_input(0x2f)],
        output: vec![
            TxOut {
                value: Amount::from_sat(0),
                script_pubkey: ScriptBuf::new_op_return(payload),
            },
            p2wpkh_output(0x34, 90_000),
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

/// A distinct deterministic 1-in/2-out P2WPKH payment transaction seeded by
/// `byte`; classifies as behavior `payment`.
fn fork_payment_transaction(byte: u8) -> Transaction {
    Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![raw_input(byte)],
        output: vec![
            p2wpkh_output(byte.wrapping_add(1), 3_000_000),
            p2wpkh_output(byte.wrapping_add(2), 700_000),
        ],
    }
}

/// A distinct deterministic OP_RETURN data carrier seeded by `byte` and
/// `payload`; classifies as behavior `data` and data_protocol `op_return_other`.
fn fork_data_transaction(byte: u8, payload: &[u8]) -> Transaction {
    let payload = bitcoin::script::PushBytesBuf::try_from(payload.to_vec()).expect("push bytes");
    Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![raw_input(byte)],
        output: vec![
            TxOut {
                value: Amount::from_sat(0),
                script_pubkey: ScriptBuf::new_op_return(payload),
            },
            p2wpkh_output(byte.wrapping_add(1), 100_000),
        ],
    }
}

/// A staged three-source fork seed for the comparison fixture, ordered least to
/// most permissive: `knots` (strictest), `core`, `libre-relay`. Two shared
/// payments and one shared underived member are common to all three. `core`
/// additionally relays a data carrier `knots` filters (added at the first
/// stage); `libre-relay` additionally relays a second data carrier (added at
/// the second stage). `knots` alone carries one transaction the more-permissive
/// sources never saw, so the first stage's reverse anomaly is non-empty while
/// the second stage's is empty. Bytes are observed once, over `libre-relay`, so
/// every txid's classification derives regardless of the source a region reads.
fn fork_seed_events() -> Vec<NormalizedEvent> {
    let knots = SourceId::new("knots").expect("source");
    let core = SourceId::new("core").expect("source");
    let libre = SourceId::new("libre-relay").expect("source");
    let knots_session = SourceSessionId::new("knots-session").expect("session");
    let core_session = SourceSessionId::new("core-session").expect("session");
    let libre_session = SourceSessionId::new("libre-session").expect("session");

    let shared_one = payment_transaction();
    let shared_two = fork_payment_transaction(0x2a);
    let core_added = data_transaction();
    let libre_added = fork_data_transaction(0x3a, b"fork-libre");
    let shared_one_txid = shared_one.compute_txid().to_string();
    let shared_two_txid = shared_two.compute_txid().to_string();
    let core_added_txid = core_added.compute_txid().to_string();
    let libre_added_txid = libre_added.compute_txid().to_string();
    // Members observed without raw bytes: the shared underived member and the
    // knots-only anomaly. Their SourceReplica entries still carry complete RPC
    // facts.
    let shared_underived_txid = txid(0xa1);
    let anomaly_knots_txid = txid(0xb2);

    let event = |source: &SourceId, session: &SourceSessionId, sequence: u64, evidence| {
        NormalizedEvent::new(
            source.clone(),
            session.clone(),
            sequence,
            AS_OF_MS - HOUR_MS + sequence * MINUTE_MS,
            AS_OF_MS - HOUR_MS + sequence * MINUTE_MS + 5,
            evidence,
        )
        .expect("fork seed event")
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
        // knots (strictest): the two shared payments, the shared underived
        // member, and its own anomaly the others never saw.
        event(
            &knots,
            &knots_session,
            1,
            present(shared_one_txid.clone(), 200, 1_800, 10 * MINUTE_MS),
        ),
        event(
            &knots,
            &knots_session,
            2,
            present(shared_two_txid.clone(), 250, 1_500, 20 * MINUTE_MS),
        ),
        event(
            &knots,
            &knots_session,
            3,
            present(shared_underived_txid.clone(), 180, 900, 40 * MINUTE_MS),
        ),
        event(
            &knots,
            &knots_session,
            4,
            present(anomaly_knots_txid, 210, 700, HOUR_MS),
        ),
        // core: the two shared payments, the shared underived member, and the
        // data carrier knots filtered out (added at the knots -> core stage).
        event(
            &core,
            &core_session,
            1,
            present(shared_one_txid.clone(), 200, 2_000, 12 * MINUTE_MS),
        ),
        event(
            &core,
            &core_session,
            2,
            present(shared_two_txid.clone(), 250, 1_600, 22 * MINUTE_MS),
        ),
        event(
            &core,
            &core_session,
            3,
            present(shared_underived_txid.clone(), 180, 950, 42 * MINUTE_MS),
        ),
        event(
            &core,
            &core_session,
            4,
            present(core_added_txid.clone(), 600, 6_000, 30 * MINUTE_MS),
        ),
        // libre-relay (most permissive): observes every shared and added tx's
        // bytes so classification derives, includes one legacy factless seed
        // input that `split_seed_material` converts to complete state, and
        // additionally relays libre_added.
        event(&libre, &libre_session, 1, p2p_evidence(&shared_one)),
        event(
            &libre,
            &libre_session,
            2,
            present(shared_one_txid, 200, 2_100, 14 * MINUTE_MS),
        ),
        event(&libre, &libre_session, 3, p2p_evidence(&shared_two)),
        event(
            &libre,
            &libre_session,
            4,
            present(shared_two_txid, 250, 1_650, 24 * MINUTE_MS),
        ),
        event(&libre, &libre_session, 5, p2p_evidence(&core_added)),
        event(
            &libre,
            &libre_session,
            6,
            present(core_added_txid, 600, 6_200, 32 * MINUTE_MS),
        ),
        event(&libre, &libre_session, 7, p2p_evidence(&libre_added)),
        event(
            &libre,
            &libre_session,
            8,
            present(libre_added_txid, 550, 5_500, 8 * MINUTE_MS),
        ),
        event(
            &libre,
            &libre_session,
            9,
            Evidence::MempoolAdded {
                txid: shared_underived_txid,
            },
        ),
    ]
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
    // Historical rejections remain internal fixture evidence. The production
    // rejection response deliberately reports not_collected even when these
    // legacy rows exist.
    let rejected = rejected_data_transaction();
    let rejected_txid = rejected.compute_txid().to_string();
    let reject_event = |sequence: u64, observed: u64, evidence| {
        NormalizedEvent::new(
            source_a.clone(),
            session_a.clone(),
            sequence,
            observed,
            observed + 5,
            evidence,
        )
        .expect("seed rejection event")
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
        // Classified refusal: the transaction's bytes are seen over P2P first,
        // so it derives verdicts, then it is refused without ever entering
        // membership.
        reject_event(14, AS_OF_MS - 55 * MINUTE_MS, p2p_evidence(&rejected)),
        reject_event(
            15,
            AS_OF_MS - 52 * MINUTE_MS,
            Evidence::MempoolRejected {
                txid: rejected_txid,
                reason: "min relay fee not met".to_owned(),
            },
        ),
        // Unclassified refusal: refused before any bytes were seen.
        reject_event(
            16,
            AS_OF_MS - 50 * MINUTE_MS,
            Evidence::MempoolRejected {
                txid: txid(50),
                reason: "insufficient fee".to_owned(),
            },
        ),
    ]
}

struct SeedMaterial {
    state: BTreeMap<SourceId, BTreeMap<String, MempoolEntryFacts>>,
    evidence: Vec<NormalizedEvent>,
}

/// Separates historical fixture input from reader-visible SourceReplica
/// state. Legacy factless membership observations receive deterministic RPC
/// facts in the checkpoint and are never ingested as state-changing evidence.
fn split_seed_material(events: Vec<NormalizedEvent>) -> SeedMaterial {
    let mut state = BTreeMap::<SourceId, BTreeMap<String, MempoolEntryFacts>>::new();
    let mut evidence = Vec::new();
    for event in events {
        let mutations = event.membership_mutations();
        if mutations.is_empty() {
            evidence.push(event);
            continue;
        }
        let source = state.entry(event.source_id.clone()).or_default();
        for mutation in mutations {
            match mutation {
                MembershipMutation::Absent { txid } => {
                    source.remove(&txid);
                }
                MembershipMutation::Present { txid, facts } => {
                    source.insert(
                        txid,
                        facts.unwrap_or(MempoolEntryFacts {
                            vsize: 180,
                            fee_sats: 900,
                            entered_at_ms: event.observed_at_ms,
                        }),
                    );
                }
            }
        }
    }
    SeedMaterial { state, evidence }
}

fn router_for(seed: SeedKind, directory: &Path) -> Router {
    let database = directory.join(match seed {
        SeedKind::Primary => "primary.db",
        SeedKind::Fork => "fork.db",
    });
    Store::migrate(&database).expect("migrate seed store");
    let store = Store::open(database).expect("open seed store");
    let events = match seed {
        SeedKind::Primary => seed_events(),
        SeedKind::Fork => fork_seed_events(),
    };
    let material = split_seed_material(events);
    for event in material.evidence {
        store.ingest(&event).expect("seed event ingest");
    }
    for (source_id, entries) in material.state {
        install_checkpoint(
            &store,
            source_id.as_str(),
            AS_OF_MS - 1_000,
            entries
                .into_iter()
                .map(|(txid, facts)| SourceReplicaEntry::new(txid, facts).expect("state entry"))
                .collect(),
        );
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
            BodyKind::Rejections => assert_round_trip::<SourceRejections>(&value, spec.path),
            BodyKind::Comparison => assert_round_trip::<SourceComparison>(&value, spec.path),
            BodyKind::Error => assert_round_trip::<ErrorBody>(&value, spec.path),
        }
    }
}

#[tokio::test]
async fn fixtures_equal_live_responses_from_the_seed_store() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let primary = router_for(SeedKind::Primary, temporary.path());
    let fork = router_for(SeedKind::Fork, temporary.path());
    for spec in &SPECS {
        let application = match spec.seed {
            SeedKind::Primary => primary.clone(),
            SeedKind::Fork => fork.clone(),
        };
        let (status, live) = issue(application, spec).await;
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
    let primary = router_for(SeedKind::Primary, temporary.path());
    let fork = router_for(SeedKind::Fork, temporary.path());
    for spec in &SPECS {
        let application = match spec.seed {
            SeedKind::Primary => primary.clone(),
            SeedKind::Fork => fork.clone(),
        };
        let (status, value) = issue(application, spec).await;
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
