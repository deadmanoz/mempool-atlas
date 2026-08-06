use std::path::PathBuf;
use std::str::FromStr;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::map_response;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tower_http::compression::CompressionLayer;
use tower_http::services::ServeDir;

use crate::model::SourcesResponse;
use crate::runtime::{SourceRegistry, TransactionLookup};

const SNAPSHOT_CACHE_CONTROL: &str = "public, no-cache, must-revalidate";
const NO_STORE: &str = "no-store";

pub fn router(registry: SourceRegistry, web_root: PathBuf) -> Router {
    let static_files = Router::new()
        .fallback_service(ServeDir::new(web_root).append_index_html_on_directories(true))
        .layer(map_response(static_fallback_cache_policy));
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(readiness))
        .route("/api/v1/sources", get(sources))
        .route("/api/v1/sources/{source_id}/mempool", get(source_snapshot))
        .route(
            "/api/v1/sources/{source_id}/transactions/{txid}",
            get(transaction_detail),
        )
        .route_layer(map_response(default_cache_policy))
        .fallback_service(static_files)
        .layer(CompressionLayer::new())
        .with_state(registry)
}

/// Health, readiness, discovery, waiting, and error responses describe current
/// state that is never reusable, so every routed response that does not declare
/// its own policy is `no-store`. Handlers that publish a cached representation
/// set `Cache-Control` themselves and keep it, alongside their `ETag`.
async fn default_cache_policy<B>(mut response: Response<B>) -> Response<B> {
    response
        .headers_mut()
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static(NO_STORE));
    response
}

/// The static service also answers unknown API paths, so its failures are part
/// of the API surface and are `no-store`. Asset and document responses it does
/// serve keep their existing caching behavior, which the edge configures.
async fn static_fallback_cache_policy<B>(response: Response<B>) -> Response<B> {
    if response.status().is_success() || response.status() == StatusCode::NOT_MODIFIED {
        return response;
    }
    default_cache_policy(response).await
}

#[derive(Debug, Serialize)]
struct StatusResponse {
    status: &'static str,
}

async fn health() -> Json<StatusResponse> {
    Json(StatusResponse { status: "ok" })
}

async fn readiness(State(registry): State<SourceRegistry>) -> Response {
    if registry.is_ready().await {
        (StatusCode::OK, Json(StatusResponse { status: "ready" })).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(StatusResponse {
                status: "waiting_for_snapshot",
            }),
        )
            .into_response()
    }
}

async fn sources(
    State(registry): State<SourceRegistry>,
) -> ([(&'static str, &'static str); 1], Json<SourcesResponse>) {
    (
        [(header::CACHE_CONTROL.as_str(), NO_STORE)],
        Json(registry.sources_response().await),
    )
}

async fn source_snapshot(
    State(registry): State<SourceRegistry>,
    Path(source_id): Path<String>,
    request_headers: HeaderMap,
) -> Result<Response, ApiError> {
    let source = registry
        .get(&source_id)
        .ok_or_else(|| ApiError::source_not_found(&source_id))?;
    let payload = source
        .snapshot_response_payload()
        .await
        .map_err(ApiError::snapshot_response)?;
    let not_modified = payload
        .etag
        .as_deref()
        .is_some_and(|etag| if_none_match_matches(&request_headers, etag));
    let mut response = if not_modified {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            Body::from(payload.body),
        )
            .into_response()
    };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(if payload.etag.is_some() {
            SNAPSHOT_CACHE_CONTROL
        } else {
            NO_STORE
        }),
    );
    if let Some(etag) = payload.etag {
        response.headers_mut().insert(
            header::ETAG,
            HeaderValue::from_str(&etag).expect("generated snapshot ETag is a valid header value"),
        );
    }
    Ok(response)
}

fn if_none_match_matches(headers: &HeaderMap, current_etag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|candidate| candidate == "*" || weak_etag(candidate) == weak_etag(current_etag))
}

fn weak_etag(value: &str) -> &str {
    value.strip_prefix("W/").unwrap_or(value)
}

async fn transaction_detail(
    State(registry): State<SourceRegistry>,
    Path((source_id, txid)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let canonical_txid = bitcoin::Txid::from_str(&txid)
        .map_err(|_| ApiError::invalid_txid(&txid))?
        .to_string();
    let source = registry
        .get(&source_id)
        .ok_or_else(|| ApiError::source_not_found(&source_id))?;
    match source.transaction_detail(&canonical_txid).await {
        TransactionLookup::Ready(detail) => Ok((
            StatusCode::OK,
            [(header::CACHE_CONTROL, NO_STORE)],
            Json(detail),
        )
            .into_response()),
        TransactionLookup::WaitingForSnapshot => Err(ApiError::snapshot_not_ready()),
        TransactionLookup::NotPresent => Err(ApiError::transaction_not_present(&canonical_txid)),
        TransactionLookup::Unclassified => Err(ApiError::transaction_unclassified(&canonical_txid)),
    }
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn source_not_found(source_id: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: format!("unknown source {source_id:?}"),
        }
    }

    fn snapshot_response(error: impl std::fmt::Display) -> Self {
        tracing::error!(error = %error, "failed to encode snapshot response");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "current snapshot is temporarily unavailable".to_owned(),
        }
    }

    fn invalid_txid(txid: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: format!("invalid transaction ID {txid:?}"),
        }
    }

    fn snapshot_not_ready() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: "current snapshot is not ready".to_owned(),
        }
    }

    fn transaction_not_present(txid: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: format!("transaction {txid:?} is not in the current snapshot"),
        }
    }

    fn transaction_unclassified(txid: &str) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: format!(
                "transaction {txid:?} is present but has no policy assessment in the current snapshot"
            ),
        }
    }
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            [(header::CACHE_CONTROL, NO_STORE)],
            Json(ErrorResponse {
                error: self.message,
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;
    use crate::model::{
        Bip110Assessment, Bip110RuleDetail, Bip110RuleId, Bip110RuleVerdict, Bip110Status,
        ChainTip, MempoolEntry, MempoolObservation, MempoolSnapshot, TransactionClassification,
        TransactionStructure,
    };
    use crate::runtime::SourceRuntime;

    fn runtime() -> Arc<SourceRuntime> {
        Arc::new(
            SourceRuntime::new(
                "core".to_owned(),
                "Bitcoin Core".to_owned(),
                Duration::from_secs(60),
            )
            .expect("runtime"),
        )
    }

    fn application(source: Arc<SourceRuntime>) -> Router {
        let web_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web");
        router(
            SourceRegistry::new(vec![source]).expect("registry"),
            web_root,
        )
    }

    fn snapshot() -> MempoolSnapshot {
        MempoolSnapshot::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            1_700_000_000_000,
            ChainTip {
                height: 900_000,
                hash: "00".repeat(32),
            },
            vec![MempoolEntry::new("00".repeat(32), 141, 1_200, 1_699_999_000_000).expect("entry")],
        )
        .expect("snapshot")
    }

    fn observation() -> MempoolObservation {
        MempoolObservation::new(snapshot(), BTreeMap::new()).expect("observation")
    }

    fn classified_observation() -> MempoolObservation {
        let assessment = Bip110Assessment {
            status: Bip110Status::Compatible,
            primary_rule: None,
            violated_rules: Vec::new(),
            unknown_rules: Vec::new(),
        };
        let structure = TransactionStructure::new(1, 2, 0, 50_000, 107).expect("structure");
        let mut entry =
            MempoolEntry::new("00".repeat(32), 141, 1_200, 1_699_999_000_000).expect("entry");
        let results = crate::model::test_classifier_results(&assessment);
        entry.classifications = results
            .iter()
            .map(crate::model::ClassificationResult::compact)
            .collect();
        entry.bip110 = Some(assessment.clone());
        entry.structure = Some(structure);
        let snapshot = MempoolSnapshot::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            1_700_000_000_000,
            ChainTip {
                height: 900_000,
                hash: "00".repeat(32),
            },
            vec![entry],
        )
        .expect("snapshot");
        let classification = Arc::new(TransactionClassification {
            structure,
            txid: "00".repeat(32),
            wtxid: "00".repeat(32),
            results,
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
        MempoolObservation::new(
            snapshot,
            BTreeMap::from([("00".repeat(32), classification)]),
        )
        .expect("observation")
    }

    async fn get_json(application: Router, path: &str) -> (StatusCode, Value) {
        let response = application
            .oneshot(Request::get(path).body(Body::empty()).expect("request"))
            .await
            .expect("response");
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        (
            status,
            serde_json::from_slice(&body).expect("JSON response"),
        )
    }

    async fn cache_control(application: Router, path: &str) -> (StatusCode, Option<String>) {
        let response = application
            .oneshot(Request::get(path).body(Body::empty()).expect("request"))
            .await
            .expect("response");
        let policy = response
            .headers()
            .get(header::CACHE_CONTROL)
            .map(|value| value.to_str().expect("Cache-Control").to_owned());
        (response.status(), policy)
    }

    #[tokio::test]
    async fn health_is_independent_of_rpc_readiness() {
        let (status, body) = get_json(application(runtime()), "/healthz").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "status": "ok" }));
    }

    #[tokio::test]
    async fn static_index_is_served_by_the_same_process() {
        let response = application(runtime())
            .oneshot(Request::get("/").body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], "text/html");
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert!(String::from_utf8_lossy(&body).contains("Mempool Atlas"));
    }

    #[tokio::test]
    async fn source_is_visible_while_waiting_for_first_snapshot() {
        let source = runtime();
        let application = application(source);

        let (sources_status, sources) = get_json(application.clone(), "/api/v1/sources").await;
        assert_eq!(sources_status, StatusCode::OK);
        assert_eq!(sources["atlas_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(sources["sources"][0]["availability"], "waiting");

        let (snapshot_status, response) =
            get_json(application.clone(), "/api/v1/sources/core/mempool").await;
        assert_eq!(snapshot_status, StatusCode::OK);
        assert!(response["snapshot"].is_null());

        let (ready_status, ready) = get_json(application, "/readyz").await;
        assert_eq!(ready_status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(ready["status"], "waiting_for_snapshot");
    }

    #[tokio::test]
    async fn waiting_snapshot_response_is_not_cacheable() {
        let response = application(runtime())
            .oneshot(
                Request::get("/api/v1/sources/core/mempool")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert!(!response.headers().contains_key(header::ETAG));
    }

    #[tokio::test]
    async fn operational_and_discovery_responses_are_not_cacheable() {
        let source = runtime();
        let application = application(Arc::clone(&source));

        for path in ["/healthz", "/readyz", "/api/v1/sources"] {
            let (_, policy) = cache_control(application.clone(), path).await;
            assert_eq!(policy.as_deref(), Some("no-store"), "{path} cache policy");
        }

        source
            .record_success(observation())
            .await
            .expect("publish snapshot");
        let (status, policy) = cache_control(application, "/readyz").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(policy.as_deref(), Some("no-store"));
    }

    #[tokio::test]
    async fn unknown_api_route_is_not_cacheable() {
        let (status, policy) = cache_control(application(runtime()), "/api/v1/nonexistent").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(policy.as_deref(), Some("no-store"));
    }

    #[tokio::test]
    async fn api_error_responses_are_not_cacheable() {
        let source = runtime();
        source
            .record_success(observation())
            .await
            .expect("publish snapshot");
        let application = application(source);
        let txid = "00".repeat(32);

        for path in [
            "/api/v1/sources/knots/mempool".to_owned(),
            "/api/v1/sources/core/transactions/not-a-txid".to_owned(),
            format!("/api/v1/sources/core/transactions/{txid}"),
        ] {
            let (status, policy) = cache_control(application.clone(), &path).await;
            assert!(
                status.is_client_error() || status.is_server_error(),
                "{path}"
            );
            assert_eq!(policy.as_deref(), Some("no-store"), "{path} cache policy");
        }
    }

    #[tokio::test]
    async fn method_not_allowed_is_not_cacheable() {
        let response = application(runtime())
            .oneshot(
                Request::post("/healthz")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }

    #[tokio::test]
    async fn static_documents_keep_their_existing_cache_behavior() {
        let (status, policy) = cache_control(application(runtime()), "/").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(policy, None);
    }

    #[tokio::test]
    async fn current_snapshot_is_served_after_atomic_publication() {
        let source = runtime();
        source
            .record_success(observation())
            .await
            .expect("publish snapshot");
        let application = application(source);

        let (status, response) =
            get_json(application.clone(), "/api/v1/sources/core/mempool").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["source"]["availability"], "ready");
        assert_eq!(response["snapshot"]["transaction_count"], 1);
        assert_eq!(response["snapshot"]["transactions"][0]["fee_sats"], 1_200);

        let (ready_status, ready) = get_json(application, "/readyz").await;
        assert_eq!(ready_status, StatusCode::OK);
        assert_eq!(ready["status"], "ready");
    }

    #[tokio::test]
    async fn published_snapshot_supports_conditional_reads() {
        let source = runtime();
        source
            .record_success(observation())
            .await
            .expect("publish snapshot");
        let application = application(source);
        let path = "/api/v1/sources/core/mempool";

        let response = application
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            SNAPSHOT_CACHE_CONTROL
        );
        let etag = response.headers()[header::ETAG]
            .to_str()
            .expect("ETag")
            .to_owned();
        assert!(etag.starts_with("W/\"core-1700000000000-"));

        let response = application
            .oneshot(
                Request::get(path)
                    .header(header::IF_NONE_MATCH, format!("\"other\", {etag}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers()[header::ETAG], etag);
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            SNAPSHOT_CACHE_CONTROL
        );
        assert!(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn current_snapshot_supports_gzip_transfer() {
        let source = runtime();
        source
            .record_success(classified_observation())
            .await
            .expect("publish observation");
        let response = application(source)
            .oneshot(
                Request::get("/api/v1/sources/core/mempool")
                    .header(header::ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_ENCODING], "gzip");
        assert_eq!(response.headers()[header::VARY], "accept-encoding");
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(body.get(..2), Some([0x1f, 0x8b].as_slice()));
    }

    #[tokio::test]
    async fn source_failure_replaces_the_snapshot_validator() {
        let source = runtime();
        source
            .record_success(observation())
            .await
            .expect("publish snapshot");
        let application = application(Arc::clone(&source));
        let path = "/api/v1/sources/core/mempool";
        let first = application
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).expect("request"))
            .await
            .expect("response");
        let first_etag = first.headers()[header::ETAG].clone();

        source
            .record_failure("node unavailable".to_owned())
            .await
            .expect("record failure");
        let response = application
            .oneshot(
                Request::get(path)
                    .header(header::IF_NONE_MATCH, first_etag.clone())
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_ne!(response.headers()[header::ETAG], first_etag);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let response = serde_json::from_slice::<Value>(&body).expect("JSON response");
        assert_eq!(response["source"]["availability"], "stale");
        assert_eq!(response["source"]["last_error"], "node unavailable");
    }

    #[tokio::test]
    async fn unknown_source_is_explicit() {
        let (status, response) =
            get_json(application(runtime()), "/api/v1/sources/knots/mempool").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(response["error"], "unknown source \"knots\"");
    }

    #[tokio::test]
    async fn current_transaction_detail_is_served_atomically() {
        let source = runtime();
        source
            .record_success(classified_observation())
            .await
            .expect("publish observation");
        source
            .record_failure("node unavailable".to_owned())
            .await
            .expect("retain classified observation");
        let txid = "00".repeat(32);

        let (status, response) = get_json(
            application(source),
            &format!("/api/v1/sources/core/transactions/{txid}"),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["txid"], txid);
        assert_eq!(response["assessment"]["status"], "compatible");
        assert_eq!(response["rules"].as_array().map(Vec::len), Some(7));
        assert_eq!(response["rules"][0]["evidence_count"], 0);
        assert_eq!(response["rules"][0]["missing_count"], 0);
        assert_eq!(response["source_id"], "core");
        assert_eq!(response["snapshot_observed_at_ms"], 1_700_000_000_000_u64);
        assert_eq!(response["classification_revision"], 0);
    }

    #[tokio::test]
    async fn present_but_unclassified_transaction_is_explicit() {
        let source = runtime();
        source
            .record_success(classified_observation())
            .await
            .expect("publish classified observation");
        source
            .record_success(observation())
            .await
            .expect("replace with unclassified observation");
        let txid = "00".repeat(32);

        let (status, response) = get_json(
            application(source),
            &format!("/api/v1/sources/core/transactions/{txid}"),
        )
        .await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            response["error"]
                .as_str()
                .is_some_and(|error| error.contains("has no policy assessment"))
        );
    }
}
