//! Production-isolated Bitcoin JSON-RPC fixture for deployed scale exercises.
//!
//! This binary is available only with the `scale-fixture` feature. It
//! precomputes two complete verbose mempool result bodies before listening so
//! the measured `atlas-agent` process receives stable, deterministic payloads.

use std::convert::Infallible;
use std::fmt::{self, Write as _};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use anyhow::{Context, bail};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Response, StatusCode, header};
use axum::routing::post;
use clap::Parser;
use futures_util::stream;
use serde::Deserialize;
use serde_json::{Value, json};

const DEFAULT_BIND: &str = "127.0.0.1:18443";
const MAX_FIXTURE_ENTRIES: u64 = 200_000;
const PREALLOCATED_BYTES_PER_ENTRY: usize = 800;
const VARIANT_ONE: u8 = 1;
const VARIANT_TWO: u8 = 2;
const JSON_CONTENT_TYPE: &str = "application/json";

#[derive(Debug, Parser)]
#[command(name = "atlas-rpc-fixture")]
#[command(about = "Test-only deterministic Bitcoin JSON-RPC scale fixture")]
struct Cli {
    /// Loopback address on which the fixture listens.
    #[arg(long, default_value = DEFAULT_BIND)]
    bind: SocketAddr,
    /// Exact number of deterministic verbose mempool entries to serve.
    #[arg(long, default_value_t = MAX_FIXTURE_ENTRIES)]
    count: u64,
    /// Dummy HTTP Basic authentication username required from fixture clients.
    #[arg(long, env = "ATLAS_FIXTURE_RPC_USERNAME")]
    rpc_username: String,
    /// Dummy HTTP Basic authentication password required from fixture clients.
    #[arg(long, env = "ATLAS_FIXTURE_RPC_PASSWORD")]
    rpc_password: String,
}

struct FixtureState {
    count: u64,
    variant: AtomicU8,
    verbose_results: [Bytes; 2],
    expected_authorization: String,
}

impl FixtureState {
    fn new(count: u64, username: &str, password: &str) -> anyhow::Result<Self> {
        validate_count(count)?;
        let expected_authorization = basic_authorization(username, password)?;
        Ok(Self {
            count,
            variant: AtomicU8::new(VARIANT_ONE),
            verbose_results: [
                Bytes::from(build_verbose_result(count, VARIANT_ONE)?),
                Bytes::from(build_verbose_result(count, VARIANT_TWO)?),
            ],
            expected_authorization,
        })
    }

    fn current_variant(&self) -> u8 {
        self.variant.load(Ordering::SeqCst)
    }

    fn set_variant(&self, variant: u8) -> Result<(), RpcFault> {
        if !matches!(variant, VARIANT_ONE | VARIANT_TWO) {
            return Err(RpcFault::invalid_params(
                "atlas_setvariant requires variant 1 or 2",
            ));
        }
        self.variant.store(variant, Ordering::SeqCst);
        Ok(())
    }

    fn verbose_result(&self) -> Bytes {
        let index = usize::from(self.current_variant() - 1);
        self.verbose_results[index].clone()
    }

    fn is_authorized(&self, headers: &HeaderMap) -> bool {
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value == self.expected_authorization)
    }
}

#[derive(Debug, Deserialize)]
struct RpcRequest {
    #[serde(default)]
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug)]
struct RpcFault {
    code: i64,
    message: &'static str,
}

impl RpcFault {
    const fn invalid_request(message: &'static str) -> Self {
        Self {
            code: -32600,
            message,
        }
    }

    const fn method_not_found() -> Self {
        Self {
            code: -32601,
            message: "Method not found",
        }
    }

    const fn invalid_params(message: &'static str) -> Self {
        Self {
            code: -32602,
            message,
        }
    }

    const fn parse_error() -> Self {
        Self {
            code: -32700,
            message: "Parse error",
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    validate_bind(cli.bind)?;

    println!(
        "precomputing two verbose mempool variants with {} entries each",
        cli.count
    );
    let state = Arc::new(FixtureState::new(
        cli.count,
        &cli.rpc_username,
        &cli.rpc_password,
    )?);
    let listener = tokio::net::TcpListener::bind(cli.bind)
        .await
        .with_context(|| format!("binding fixture to {}", cli.bind))?;
    println!("atlas RPC fixture listening on {}", cli.bind);
    axum::serve(listener, fixture_router(state)).await?;
    Ok(())
}

fn fixture_router(state: Arc<FixtureState>) -> Router {
    Router::new().route("/", post(handle_rpc)).with_state(state)
}

async fn handle_rpc(
    State(state): State<Arc<FixtureState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    if !state.is_authorized(&headers) {
        return unauthorized_response();
    }
    dispatch_rpc(&state, &body)
}

fn dispatch_rpc(state: &FixtureState, body: &[u8]) -> Response<Body> {
    let value = match serde_json::from_slice::<Value>(body) {
        Ok(value) => value,
        Err(_) => return rpc_error(&Value::Null, RpcFault::parse_error()),
    };
    let id = value.get("id").cloned().unwrap_or(Value::Null);
    let request = match serde_json::from_value::<RpcRequest>(value) {
        Ok(request) => request,
        Err(_) => {
            return rpc_error(
                &id,
                RpcFault::invalid_request("request must contain a string method"),
            );
        }
    };

    match dispatch_method(state, &request) {
        Ok(result) => rpc_success(&request.id, result),
        Err(error) => rpc_error(&request.id, error),
    }
}

fn dispatch_method(state: &FixtureState, request: &RpcRequest) -> Result<Bytes, RpcFault> {
    match request.method.as_str() {
        "getmempoolinfo" => {
            require_no_params(
                &request.params,
                "getmempoolinfo does not accept fixture parameters",
            )?;
            Ok(Bytes::from(
                serde_json::to_vec(&json!({
                    "size": state.count
                }))
                .expect("static mempool info is serializable"),
            ))
        }
        "getrawmempool" => {
            if request.params != json!([true]) {
                return Err(RpcFault::invalid_params(
                    "fixture supports only verbose getrawmempool with params [true]",
                ));
            }
            Ok(state.verbose_result())
        }
        "atlas_setvariant" => {
            let variant = one_u8_param(&request.params).ok_or_else(|| {
                RpcFault::invalid_params("atlas_setvariant requires one integer parameter")
            })?;
            state.set_variant(variant)?;
            Ok(Bytes::from(
                serde_json::to_vec(&json!({ "variant": variant }))
                    .expect("static variant result is serializable"),
            ))
        }
        "atlas_status" => {
            require_no_params(&request.params, "atlas_status does not accept parameters")?;
            Ok(Bytes::from(
                serde_json::to_vec(&json!({
                    "count": state.count,
                    "variant": state.current_variant(),
                    "verbose_result_bytes": {
                        "variant_1": state.verbose_results[0].len(),
                        "variant_2": state.verbose_results[1].len()
                    }
                }))
                .expect("static status is serializable"),
            ))
        }
        _ => Err(RpcFault::method_not_found()),
    }
}

fn require_no_params(params: &Value, message: &'static str) -> Result<(), RpcFault> {
    if params.is_null() || params == &json!([]) {
        Ok(())
    } else {
        Err(RpcFault::invalid_params(message))
    }
}

fn one_u8_param(params: &Value) -> Option<u8> {
    let values = params.as_array()?;
    if values.len() != 1 {
        return None;
    }
    values[0]
        .as_u64()
        .and_then(|value| u8::try_from(value).ok())
}

fn rpc_success(id: &Value, result: Bytes) -> Response<Body> {
    let prefix = Bytes::from_static(br#"{"result":"#);
    let id = serde_json::to_vec(id).expect("JSON-RPC ID is serializable");
    let mut suffix = Vec::with_capacity(21 + id.len());
    suffix.extend_from_slice(br#","error":null,"id":"#);
    suffix.extend_from_slice(&id);
    suffix.push(b'}');
    let suffix = Bytes::from(suffix);
    let content_length = prefix.len() + result.len() + suffix.len();
    let chunks = [
        Ok::<_, Infallible>(prefix),
        Ok::<_, Infallible>(result),
        Ok::<_, Infallible>(suffix),
    ];
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, JSON_CONTENT_TYPE)
        .header(header::CONTENT_LENGTH, content_length)
        .body(Body::from_stream(stream::iter(chunks)))
        .expect("static response is valid")
}

fn rpc_error(id: &Value, fault: RpcFault) -> Response<Body> {
    let body = serde_json::to_vec(&json!({
        "result": Value::Null,
        "error": {
            "code": fault.code,
            "message": fault.message
        },
        "id": id
    }))
    .expect("JSON-RPC error is serializable");
    json_response(body)
}

fn json_response(body: Vec<u8>) -> Response<Body> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, JSON_CONTENT_TYPE)
        .body(Body::from(body))
        .expect("static response is valid")
}

fn unauthorized_response() -> Response<Body> {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(
            header::WWW_AUTHENTICATE,
            r#"Basic realm="atlas-rpc-fixture""#,
        )
        .body(Body::empty())
        .expect("static response is valid")
}

fn validate_count(count: u64) -> anyhow::Result<()> {
    if count > MAX_FIXTURE_ENTRIES {
        bail!("fixture count {count} exceeds supported maximum {MAX_FIXTURE_ENTRIES}");
    }
    Ok(())
}

fn basic_authorization(username: &str, password: &str) -> anyhow::Result<String> {
    if username.is_empty() || password.is_empty() {
        bail!("fixture Basic authentication username and password must not be empty");
    }
    if username.contains(':') {
        bail!("fixture Basic authentication username must not contain ':'");
    }
    if username
        .chars()
        .chain(password.chars())
        .any(char::is_control)
    {
        bail!("fixture Basic authentication credentials must not contain control characters");
    }
    Ok(format!(
        "Basic {}",
        encode_base64(format!("{username}:{password}").as_bytes())
    ))
}

fn encode_base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(char::from(ALPHABET[usize::from(first >> 2)]));
        output.push(char::from(
            ALPHABET[usize::from(((first & 0x03) << 4) | (second >> 4))],
        ));
        if chunk.len() > 1 {
            output.push(char::from(
                ALPHABET[usize::from(((second & 0x0f) << 2) | (third >> 6))],
            ));
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(char::from(ALPHABET[usize::from(third & 0x3f)]));
        } else {
            output.push('=');
        }
    }
    output
}

fn validate_bind(bind: SocketAddr) -> anyhow::Result<()> {
    if !bind.ip().is_loopback() {
        bail!("fixture bind address {bind} must be loopback");
    }
    Ok(())
}

fn build_verbose_result(count: u64, variant: u8) -> anyhow::Result<Vec<u8>> {
    if !matches!(variant, VARIANT_ONE | VARIANT_TWO) {
        bail!("unsupported fixture variant {variant}");
    }

    // Keep the maximum fixture beneath its own deployment cgroup during
    // precomputation. This capacity is deliberately above the measured
    // conservative entry shape so the second retained variant does not
    // trigger a doubling reallocation beside the first.
    let capacity = usize::try_from(count)
        .unwrap_or(usize::MAX)
        .saturating_mul(PREALLOCATED_BYTES_PER_ENTRY);
    let mut body = String::with_capacity(capacity);
    body.push('{');
    for index in 0..count {
        if index > 0 {
            body.push(',');
        }
        let txid_value = index + 1;
        let wtxid_value = 1_000_000 + txid_value;
        let variant_offset = u64::from(variant - 1);
        let vsize = 140 + index % 400 + variant_offset;
        let time = 1_721_234_000 + index % 86_400 + variant_offset;
        let fee_sats = 1_000 + index % 50_000 + variant_offset;
        let modified_fee_sats = fee_sats + 10;
        let previous_vsize = if index > 0 {
            140 + (index - 1) % 400 + variant_offset
        } else {
            0
        };
        let previous_fee_sats = if index > 0 {
            1_000 + (index - 1) % 50_000 + variant_offset
        } else {
            0
        };
        let has_descendant = index + 1 < count;
        let next_vsize = if has_descendant {
            140 + (index + 1) % 400 + variant_offset
        } else {
            0
        };
        let next_fee_sats = if has_descendant {
            1_000 + (index + 1) % 50_000 + variant_offset
        } else {
            0
        };
        let ancestor_count = 1 + u64::from(index > 0);
        let descendant_count = 1 + u64::from(has_descendant);
        let ancestor_size = vsize + previous_vsize;
        let descendant_size = vsize + next_vsize;
        let ancestor_fee_sats = fee_sats + previous_fee_sats;
        let descendant_fee_sats = fee_sats + next_fee_sats;
        let replaceable = index.is_multiple_of(5);
        let unbroadcast = index.is_multiple_of(11);
        write!(
            body,
            "\"{txid_value:064x}\":{{\
             \"vsize\":{vsize},\
             \"size\":{vsize},\
             \"weight\":{},\
             \"time\":{time},\
             \"height\":850000,\
             \"descendantcount\":{descendant_count},\
             \"descendantsize\":{descendant_size},\
             \"ancestorcount\":{ancestor_count},\
             \"ancestorsize\":{ancestor_size},\
             \"wtxid\":\"{wtxid_value:064x}\",\
             \"hash\":\"{wtxid_value:064x}\",\
             \"fees\":{{\
             \"base\":{},\
             \"modified\":{},\
             \"ancestor\":{},\
             \"descendant\":{},\
             \"chunk\":{}\
             }},\
             \"depends\":[",
            vsize * 4,
            BtcAmount(fee_sats),
            BtcAmount(modified_fee_sats),
            BtcAmount(ancestor_fee_sats),
            BtcAmount(descendant_fee_sats),
            BtcAmount(fee_sats),
        )
        .expect("writing to String cannot fail");
        if index > 0 {
            write!(body, "\"{:064x}\"", txid_value - 1).expect("writing to String cannot fail");
        }
        body.push_str("],\"spentby\":[");
        if has_descendant {
            write!(body, "\"{:064x}\"", txid_value + 1).expect("writing to String cannot fail");
        }
        write!(
            body,
            "],\
             \"bip125-replaceable\":{replaceable},\
             \"unbroadcast\":{unbroadcast},\
             \"startingpriority\":{}.0,\
             \"currentpriority\":{}.0\
             }}",
            index % 1_000,
            (index + 1) % 1_000,
        )
        .expect("writing to String cannot fail");
    }
    body.push('}');
    Ok(body.into_bytes())
}

struct BtcAmount(u64);

impl fmt::Display for BtcAmount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}.{:08}",
            self.0 / 100_000_000,
            self.0 % 100_000_000
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use atlas_agent::rpc::RpcClient;
    use atlas_agent::schema;
    use atlas_agent::source_replica::{
        AcknowledgeOutcome, ObserveRpcOutcome, SourceReplica, SourceReplicaAction,
        SourceReplicaLimits,
    };
    use atlas_model::{ReplicaCursor, SourceId};
    use axum::body::to_bytes;
    use serde_json::Map;

    use super::*;

    async fn response_json(response: Response<Body>) -> Value {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        serde_json::from_slice(&bytes).expect("JSON response")
    }

    fn request(id: Value, method: &str, params: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "jsonrpc": "1.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .expect("request JSON")
    }

    async fn spawn_fixture(count: u64) -> (Arc<FixtureState>, String, tokio::task::JoinHandle<()>) {
        let state = Arc::new(
            FixtureState::new(count, "fixture-user", "fixture-password").expect("fixture"),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fixture");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn({
            let application = fixture_router(Arc::clone(&state));
            async move {
                axum::serve(listener, application)
                    .await
                    .expect("serve fixture");
            }
        });
        (state, format!("http://{address}"), server)
    }

    async fn abort_fixture(server: tokio::task::JoinHandle<()>) {
        server.abort();
        assert!(
            server
                .await
                .expect_err("aborted fixture server")
                .is_cancelled()
        );
    }

    #[test]
    fn precomputes_constant_txids_and_changed_required_facts() {
        let first: Map<String, Value> =
            serde_json::from_slice(&build_verbose_result(3, VARIANT_ONE).expect("variant one"))
                .expect("variant one JSON");
        let second: Map<String, Value> =
            serde_json::from_slice(&build_verbose_result(3, VARIANT_TWO).expect("variant two"))
                .expect("variant two JSON");

        assert_eq!(
            first.keys().collect::<BTreeSet<_>>(),
            second.keys().collect::<BTreeSet<_>>()
        );
        assert_eq!(first.len(), 3);
        for txid in first.keys() {
            assert_ne!(first[txid]["vsize"], second[txid]["vsize"]);
            assert_ne!(first[txid]["time"], second[txid]["time"]);
            assert_ne!(first[txid]["fees"]["base"], second[txid]["fees"]["base"]);
            for field in [
                "size",
                "weight",
                "height",
                "ancestorcount",
                "ancestorsize",
                "descendantcount",
                "descendantsize",
                "wtxid",
                "hash",
                "depends",
                "spentby",
                "bip125-replaceable",
                "unbroadcast",
                "startingpriority",
                "currentpriority",
            ] {
                assert!(first[txid].get(field).is_some(), "missing field {field}");
            }
            for fee in ["base", "modified", "ancestor", "descendant", "chunk"] {
                assert!(first[txid]["fees"].get(fee).is_some(), "missing fee {fee}");
            }
        }
        let first_txid = format!("{:064x}", 1);
        let second_txid = format!("{:064x}", 2);
        let third_txid = format!("{:064x}", 3);
        assert_eq!(first[&first_txid]["depends"], json!([]));
        assert_eq!(first[&first_txid]["spentby"], json!([second_txid]));
        assert_eq!(first[&second_txid]["depends"], json!([first_txid]));
        assert_eq!(first[&second_txid]["spentby"], json!([third_txid]));
        assert_eq!(first[&third_txid]["depends"], json!([second_txid]));
        assert_eq!(first[&third_txid]["spentby"], json!([]));
    }

    #[tokio::test]
    async fn preserves_ids_and_reports_control_errors() {
        let state = FixtureState::new(2, "fixture-user", "fixture-password").expect("fixture");
        let info = response_json(dispatch_rpc(
            &state,
            &request(json!("info-id"), "getmempoolinfo", json!([])),
        ))
        .await;
        assert_eq!(info["id"], "info-id");
        assert_eq!(info["result"]["size"], 2);

        let status = response_json(dispatch_rpc(
            &state,
            &request(json!("status-id"), "atlas_status", json!([])),
        ))
        .await;
        assert_eq!(status["id"], "status-id");
        assert_eq!(
            status["result"]["verbose_result_bytes"]["variant_1"],
            state.verbose_results[0].len()
        );
        assert_eq!(
            status["result"]["verbose_result_bytes"]["variant_2"],
            state.verbose_results[1].len()
        );

        let raw_response = dispatch_rpc(
            &state,
            &request(json!("raw-id"), "getrawmempool", json!([true])),
        );
        let declared_length = raw_response
            .headers()
            .get(header::CONTENT_LENGTH)
            .expect("content length")
            .to_str()
            .expect("ASCII content length")
            .parse::<usize>()
            .expect("numeric content length");
        let raw_body = to_bytes(raw_response.into_body(), usize::MAX)
            .await
            .expect("streamed response");
        assert_eq!(declared_length, raw_body.len());

        let invalid_variant = response_json(dispatch_rpc(
            &state,
            &request(json!(7), "atlas_setvariant", json!([3])),
        ))
        .await;
        assert_eq!(invalid_variant["id"], 7);
        assert_eq!(invalid_variant["error"]["code"], -32602);

        let unsupported = response_json(dispatch_rpc(
            &state,
            &request(json!(8), "getblockchaininfo", json!([])),
        ))
        .await;
        assert_eq!(unsupported["id"], 8);
        assert_eq!(unsupported["error"]["code"], -32601);

        let changed = response_json(dispatch_rpc(
            &state,
            &request(json!(9), "atlas_setvariant", json!([2])),
        ))
        .await;
        assert_eq!(changed["result"]["variant"], 2);
        assert_eq!(state.current_variant(), VARIANT_TWO);
    }

    #[tokio::test]
    async fn real_agent_rpc_client_observes_variant_two_as_changed_facts() {
        let (_state, url, server) = spawn_fixture(3).await;

        let unauthenticated = reqwest::Client::new()
            .post(&url)
            .json(&json!({
                "jsonrpc": "1.0",
                "id": "unauthenticated",
                "method": "atlas_status",
                "params": []
            }))
            .send()
            .await
            .expect("unauthenticated request");
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            unauthenticated
                .headers()
                .get(header::WWW_AUTHENTICATE)
                .expect("Basic challenge"),
            r#"Basic realm="atlas-rpc-fixture""#
        );

        let wrong_credentials = reqwest::Client::new()
            .post(&url)
            .basic_auth("fixture-user", Some("wrong-password"))
            .json(&json!({
                "jsonrpc": "1.0",
                "id": "wrong-credentials",
                "method": "atlas_status",
                "params": []
            }))
            .send()
            .await
            .expect("wrong-credentials request");
        assert_eq!(wrong_credentials.status(), StatusCode::UNAUTHORIZED);

        let (client, first) = RpcClient::connect(&url, "fixture-user", "fixture-password", 3)
            .await
            .expect("connect real agent RPC client");
        assert_eq!(first.len(), 3);

        let control: Value = reqwest::Client::new()
            .post(&url)
            .basic_auth("fixture-user", Some("fixture-password"))
            .json(&json!({
                "jsonrpc": "1.0",
                "id": "control",
                "method": "atlas_setvariant",
                "params": [2]
            }))
            .send()
            .await
            .expect("set variant request")
            .json()
            .await
            .expect("set variant response");
        assert_eq!(control["id"], "control");
        assert_eq!(control["result"]["variant"], 2);

        let second = client
            .get_mempool_snapshot()
            .await
            .expect("variant two snapshot");
        assert_eq!(second.len(), first.len());
        assert_eq!(
            second.keys().collect::<Vec<_>>(),
            first.keys().collect::<Vec<_>>()
        );
        for txid in first.keys() {
            assert_ne!(first[txid], second[txid]);
        }

        abort_fixture(server).await;
    }

    #[tokio::test]
    async fn real_rpc_variants_force_source_replica_checkpoint_after_dirty_pressure() {
        let limits = SourceReplicaLimits::default();
        let count = u64::try_from(limits.max_dirty_mutations + 1).expect("dirty limit fits u64");
        let (state, url, server) = spawn_fixture(count).await;
        let (client, first) = RpcClient::connect(&url, "fixture-user", "fixture-password", count)
            .await
            .expect("variant one through real RPC client");

        let temporary = tempfile::tempdir().expect("temporary directory");
        let database = temporary.path().join("agent.db");
        schema::migrate(&database).expect("migrate fresh fixture database");
        let replica = SourceReplica::open(
            &database,
            SourceId::new("scale-fixture").expect("source ID"),
            limits,
        )
        .expect("open source replica");
        assert_eq!(
            replica
                .observe_rpc_snapshot(&first, 1_800_000_000_000)
                .expect("observe variant-one baseline"),
            ObserveRpcOutcome::Baseline {
                revision: 1,
                entry_count: count
            }
        );
        let baseline = match replica.next_action().expect("freeze baseline") {
            Some(SourceReplicaAction::Checkpoint(begin)) => begin,
            other => panic!("expected baseline checkpoint, got {other:?}"),
        };
        replica
            .checkpoint_commit(&baseline.checkpoint_id)
            .expect("construct baseline commit");
        let status = replica.status().expect("baseline status");
        let active = ReplicaCursor {
            epoch_id: status.epoch_id,
            revision: baseline.target_revision,
        };
        assert_eq!(
            replica
                .acknowledge(&active)
                .expect("acknowledge variant-one baseline"),
            AcknowledgeOutcome::Applied
        );

        state
            .set_variant(VARIANT_TWO)
            .expect("atomically switch fixture variant");
        let second = client
            .get_mempool_snapshot()
            .await
            .expect("variant two through real RPC client");
        assert_eq!(
            replica
                .observe_rpc_snapshot(&second, 1_800_000_001_000)
                .expect("observe variant-two pressure"),
            ObserveRpcOutcome::Changed {
                revision: 2,
                mutation_count: count,
                checkpoint_required: true
            }
        );
        assert!(matches!(
            replica.next_action().expect("freeze pressure action"),
            Some(SourceReplicaAction::Checkpoint(_))
        ));

        abort_fixture(server).await;
    }

    #[test]
    fn requires_valid_configured_dummy_basic_auth() {
        assert_eq!(
            basic_authorization("Aladdin", "open sesame").expect("valid credentials"),
            "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
        );
        assert!(basic_authorization("", "password").is_err());
        assert!(basic_authorization("user:name", "password").is_err());
        assert!(basic_authorization("user", "line\nbreak").is_err());

        let state = FixtureState::new(1, "Aladdin", "open sesame").expect("fixture");
        let mut headers = HeaderMap::new();
        assert!(!state.is_authorized(&headers));
        headers.insert(
            header::AUTHORIZATION,
            "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
                .parse()
                .expect("authorization header"),
        );
        assert!(state.is_authorized(&headers));
    }

    #[test]
    fn rejects_non_loopback_scale_and_oversized_counts() {
        assert!(validate_count(MAX_FIXTURE_ENTRIES).is_ok());
        assert!(validate_count(MAX_FIXTURE_ENTRIES + 1).is_err());
        assert!(build_verbose_result(1, 3).is_err());
        assert!(validate_bind("127.0.0.1:18443".parse().expect("loopback")).is_ok());
        assert!(validate_bind("0.0.0.0:18443".parse().expect("wildcard")).is_err());
    }
}
