use std::path::PathBuf;
use std::str::FromStr;

use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::map_response;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tower_http::compression::CompressionLayer;
use tower_http::services::ServeDir;

use crate::model::SourcesResponse;
use crate::runtime::{PublicationLookup, SourceRegistry, StageLookup, TransactionLookup};
use crate::staged_snapshot::StageKind;

const MANIFEST_CACHE_CONTROL: &str = "public, no-cache, must-revalidate";
// A stage ID is the SHA-256 digest of its exact body and is part of the URL, so
// that URL is never reused for different bytes. One year matches the immutable
// static-asset convention; must-revalidate resumes validator checks after it.
const STAGE_CACHE_CONTROL: &str = "public, max-age=31536000, immutable, must-revalidate";
const NO_STORE: &str = "no-store";
const X_ATLAS_CONTENT_ID: &str = "x-atlas-content-id";
const X_ATLAS_UNCOMPRESSED_LENGTH: &str = "x-atlas-uncompressed-length";

pub fn router(registry: SourceRegistry, web_root: PathBuf) -> Router {
    let static_files = Router::new()
        .fallback_service(ServeDir::new(web_root).append_index_html_on_directories(true))
        .layer(map_response(static_fallback_cache_policy));
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(readiness))
        .route("/api/v2/sources", get(sources))
        .route("/api/v2/sources/{source_id}/mempool", get(source_manifest))
        .route(
            "/api/v2/sources/{source_id}/mempool/stages/{kind}/{stage_id}",
            get(snapshot_stage),
        )
        .route(
            "/api/v2/sources/{source_id}/mempool/stages/classifier/{classifier_id}/{stage_id}",
            get(classifier_stage),
        )
        .route(
            "/api/v2/sources/{source_id}/transactions/{txid}",
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

async fn source_manifest(
    State(registry): State<SourceRegistry>,
    Path(source_id): Path<String>,
    request_headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Result<Response, ApiError> {
    reject_query(query)?;
    let source = registry
        .get(&source_id)
        .ok_or_else(|| ApiError::source_not_found(&source_id))?;
    let PublicationLookup::Ready(payload) = source.manifest_payload().await else {
        return Err(ApiError::v2_unavailable());
    };
    Ok(cacheable_payload(
        payload,
        &request_headers,
        MANIFEST_CACHE_CONTROL,
    ))
}

async fn snapshot_stage(
    State(registry): State<SourceRegistry>,
    Path((source_id, kind, stage_id)): Path<(String, String, String)>,
    request_headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Result<Response, ApiError> {
    reject_query(query)?;
    let kind = match kind.as_str() {
        "population" => StageKind::Population,
        "membership" => StageKind::Membership,
        "structure" => StageKind::Structure,
        _ => return Err(ApiError::invalid_stage_kind(&kind)),
    };
    validate_stage_id(&stage_id)?;
    let source = registry
        .get(&source_id)
        .ok_or_else(|| ApiError::source_not_found(&source_id))?;
    stage_response(
        source.stage_payload(kind, None, &stage_id).await,
        &request_headers,
    )
}

async fn classifier_stage(
    State(registry): State<SourceRegistry>,
    Path((source_id, classifier_id, stage_id)): Path<(String, String, String)>,
    request_headers: HeaderMap,
    RawQuery(query): RawQuery,
) -> Result<Response, ApiError> {
    reject_query(query)?;
    validate_classifier_id(&classifier_id)?;
    validate_stage_id(&stage_id)?;
    let source = registry
        .get(&source_id)
        .ok_or_else(|| ApiError::source_not_found(&source_id))?;
    stage_response(
        source
            .stage_payload(StageKind::Classifier, Some(&classifier_id), &stage_id)
            .await,
        &request_headers,
    )
}

fn stage_response(lookup: StageLookup, request_headers: &HeaderMap) -> Result<Response, ApiError> {
    match lookup {
        StageLookup::Ready(payload) => Ok(cacheable_payload(
            payload,
            request_headers,
            STAGE_CACHE_CONTROL,
        )),
        StageLookup::Unavailable => Err(ApiError::v2_unavailable()),
        StageLookup::Superseded => Err(ApiError::superseded_stage()),
        StageLookup::Unknown => Err(ApiError::stage_not_found()),
    }
}

fn cacheable_payload(
    payload: crate::runtime::PublicationPayload,
    request_headers: &HeaderMap,
    cache_control: &'static str,
) -> Response {
    let not_modified = if_none_match_matches(request_headers, &payload.etag);
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
        HeaderValue::from_static(cache_control),
    );
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&payload.etag).expect("generated v2 ETag is a valid header value"),
    );
    response.headers_mut().insert(
        X_ATLAS_CONTENT_ID,
        HeaderValue::from_str(&payload.content_id)
            .expect("generated v2 content ID is a valid header value"),
    );
    response.headers_mut().insert(
        X_ATLAS_UNCOMPRESSED_LENGTH,
        HeaderValue::from_str(&payload.uncompressed_bytes.to_string())
            .expect("generated v2 body length is a valid header value"),
    );
    response
}

fn reject_query(query: Option<String>) -> Result<(), ApiError> {
    if query.is_some() {
        Err(ApiError::query_not_supported())
    } else {
        Ok(())
    }
}

fn validate_stage_id(stage_id: &str) -> Result<(), ApiError> {
    if stage_id.len() == 64
        && stage_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        Ok(())
    } else {
        Err(ApiError::invalid_stage_id())
    }
}

fn validate_classifier_id(classifier_id: &str) -> Result<(), ApiError> {
    let mut bytes = classifier_id.bytes();
    if bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        Ok(())
    } else {
        Err(ApiError::invalid_classifier_id())
    }
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
        TransactionLookup::WaitingForPublication => Err(ApiError::v2_unavailable()),
        TransactionLookup::NotPresent => Err(ApiError::transaction_not_present(&canonical_txid)),
        TransactionLookup::Unclassified => Err(ApiError::transaction_unclassified(&canonical_txid)),
    }
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
    v2_unavailable: bool,
}

impl ApiError {
    fn source_not_found(source_id: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: format!("unknown source {source_id:?}"),
            v2_unavailable: false,
        }
    }

    fn v2_unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: "Current v2 publication unavailable".to_owned(),
            v2_unavailable: true,
        }
    }

    fn superseded_stage() -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: "requested stage is not part of the current v2 publication".to_owned(),
            v2_unavailable: false,
        }
    }

    fn invalid_stage_kind(kind: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: format!("invalid v2 stage kind {kind:?}"),
            v2_unavailable: false,
        }
    }

    fn invalid_stage_id() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: "invalid v2 stage content ID".to_owned(),
            v2_unavailable: false,
        }
    }

    fn invalid_classifier_id() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: "invalid v2 classifier ID".to_owned(),
            v2_unavailable: false,
        }
    }

    fn stage_not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: "v2 stage does not exist for the requested kind or classifier".to_owned(),
            v2_unavailable: false,
        }
    }

    fn query_not_supported() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: "query-dependent v2 representations are not supported".to_owned(),
            v2_unavailable: false,
        }
    }

    fn invalid_txid(txid: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: format!("invalid transaction ID {txid:?}"),
            v2_unavailable: false,
        }
    }

    fn transaction_not_present(txid: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: format!("transaction {txid:?} is not in the current snapshot"),
            v2_unavailable: false,
        }
    }

    fn transaction_unclassified(txid: &str) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: format!(
                "transaction {txid:?} is present but has no policy assessment in the current snapshot"
            ),
            v2_unavailable: false,
        }
    }
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if self.v2_unavailable {
            return (
                self.status,
                [
                    (header::CACHE_CONTROL, NO_STORE),
                    (header::CONTENT_TYPE, "application/problem+json"),
                ],
                Body::from(
                    serde_json::to_vec(&V2UnavailableResponse {
                        problem_type: "v2_unavailable",
                        title: self.message,
                        status: 503,
                        detail:
                            "Atlas has not published a current complete snapshot for this source.",
                    })
                    .expect("v2 unavailable response is serializable"),
                ),
            )
                .into_response();
        }
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

#[derive(Debug, Serialize)]
struct V2UnavailableResponse {
    #[serde(rename = "type")]
    problem_type: &'static str,
    title: String,
    status: u16,
    detail: &'static str,
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

    fn observation_with_fee(fee_sats: u64) -> MempoolObservation {
        let snapshot = MempoolSnapshot::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            1_700_000_000_001,
            ChainTip {
                height: 900_001,
                hash: "11".repeat(32),
            },
            vec![
                MempoolEntry::new("11".repeat(32), 142, fee_sats, 1_699_999_000_001)
                    .expect("entry"),
            ],
        )
        .expect("snapshot");
        MempoolObservation::new(snapshot, BTreeMap::new()).expect("observation")
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

        let (sources_status, sources) = get_json(application.clone(), "/api/v2/sources").await;
        assert_eq!(sources_status, StatusCode::OK);
        assert_eq!(sources["atlas_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(sources["sources"][0]["availability"], "waiting");

        let (snapshot_status, response) =
            get_json(application.clone(), "/api/v2/sources/core/mempool").await;
        assert_eq!(snapshot_status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response,
            json!({
                "type": "v2_unavailable",
                "title": "Current v2 publication unavailable",
                "status": 503,
                "detail": "Atlas has not published a current complete snapshot for this source."
            })
        );

        let (ready_status, ready) = get_json(application, "/readyz").await;
        assert_eq!(ready_status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(ready["status"], "waiting_for_snapshot");
    }

    #[tokio::test]
    async fn prepublication_manifest_is_an_exact_non_cacheable_problem() {
        let application = application(runtime());
        let response = application
            .clone()
            .oneshot(
                Request::get("/api/v2/sources/core/mempool")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
        assert!(!response.headers().contains_key(header::ETAG));

        for path in [
            format!(
                "/api/v2/sources/core/mempool/stages/population/{}",
                "00".repeat(32)
            ),
            format!("/api/v2/sources/core/transactions/{}", "00".repeat(32)),
        ] {
            let (status, body) = get_json(application.clone(), &path).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
            assert_eq!(body["type"], "v2_unavailable", "{path}");
            assert_eq!(body["status"], 503, "{path}");
            assert_eq!(
                body["detail"],
                "Atlas has not published a current complete snapshot for this source.",
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn operational_and_discovery_responses_are_not_cacheable() {
        let source = runtime();
        let application = application(Arc::clone(&source));

        for path in ["/healthz", "/readyz", "/api/v2/sources"] {
            let (_, policy) = cache_control(application.clone(), path).await;
            assert_eq!(policy.as_deref(), Some("no-store"), "{path} cache policy");
        }

        source
            .record_success(observation())
            .await
            .expect("publish snapshot");
        let discovery = application
            .clone()
            .oneshot(
                Request::get("/api/v2/sources")
                    .header(header::IF_NONE_MATCH, "*")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(discovery.status(), StatusCode::OK);
        assert_eq!(discovery.headers()[header::CACHE_CONTROL], NO_STORE);
        assert!(!discovery.headers().contains_key(header::ETAG));
        let (status, policy) = cache_control(application, "/readyz").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(policy.as_deref(), Some("no-store"));
    }

    #[tokio::test]
    async fn unknown_api_route_is_not_cacheable() {
        let application = application(runtime());
        for path in ["/api/v2/nonexistent", "/api/v1/sources"] {
            let (status, policy) = cache_control(application.clone(), path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert_eq!(policy.as_deref(), Some("no-store"), "{path}");
        }
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
            "/api/v2/sources/knots/mempool".to_owned(),
            "/api/v2/sources/core/transactions/not-a-txid".to_owned(),
            format!("/api/v2/sources/core/transactions/{txid}"),
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
    async fn current_manifest_is_served_after_atomic_publication() {
        let source = runtime();
        source
            .record_success(observation())
            .await
            .expect("publish snapshot");
        let application = application(source);

        let (status, response) =
            get_json(application.clone(), "/api/v2/sources/core/mempool").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["source"]["availability"], "ready");
        assert_eq!(response["schema_version"], 2);
        assert_eq!(response["transaction_count"], 1);
        assert_eq!(response["row_count"], 1);
        assert_eq!(response["stages"].as_array().map(Vec::len), Some(7));

        let (ready_status, ready) = get_json(application, "/readyz").await;
        assert_eq!(ready_status, StatusCode::OK);
        assert_eq!(ready["status"], "ready");
    }

    #[tokio::test]
    async fn published_manifest_supports_conditional_reads() {
        let source = runtime();
        source
            .record_success(observation())
            .await
            .expect("publish snapshot");
        let application = application(source);
        let path = "/api/v2/sources/core/mempool";

        let response = application
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            MANIFEST_CACHE_CONTROL
        );
        let etag = response.headers()[header::ETAG]
            .to_str()
            .expect("ETag")
            .to_owned();
        let content_id = response.headers()[X_ATLAS_CONTENT_ID]
            .to_str()
            .expect("content ID")
            .to_owned();
        let uncompressed_length = response.headers()[X_ATLAS_UNCOMPRESSED_LENGTH]
            .to_str()
            .expect("uncompressed length")
            .to_owned();
        assert!(etag.starts_with("W/\"atlas-v2-manifest-"));

        let response = application
            .clone()
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
            MANIFEST_CACHE_CONTROL
        );
        assert!(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body")
                .is_empty()
        );

        let response = application
            .oneshot(
                Request::head(path)
                    .header(header::IF_NONE_MATCH, &etag)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers()[header::ETAG], etag);
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            MANIFEST_CACHE_CONTROL
        );
        assert_eq!(response.headers()[X_ATLAS_CONTENT_ID], content_id);
        assert_eq!(
            response.headers()[X_ATLAS_UNCOMPRESSED_LENGTH],
            uncompressed_length
        );
        assert!(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn current_manifest_supports_gzip_transfer() {
        let source = runtime();
        source
            .record_success(classified_observation())
            .await
            .expect("publish observation");
        let response = application(source)
            .oneshot(
                Request::get("/api/v2/sources/core/mempool")
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
    async fn current_stage_supports_gzip_transfer_and_cache_contract() {
        let source = runtime();
        source
            .record_success(classified_observation())
            .await
            .expect("publish observation");
        let application = application(source);
        let (_, manifest) = get_json(application.clone(), "/api/v2/sources/core/mempool").await;
        let descriptor = manifest["stages"]
            .as_array()
            .expect("stages")
            .iter()
            .find(|descriptor| descriptor["kind"] == "population")
            .expect("population descriptor");
        let content_id = descriptor["content_id"]
            .as_str()
            .expect("population content ID");
        let uncompressed_bytes = descriptor["uncompressed_bytes"]
            .as_u64()
            .expect("population byte length");
        let path = format!("/api/v2/sources/core/mempool/stages/population/{content_id}");

        let response = application
            .oneshot(
                Request::get(path)
                    .header(header::ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_ENCODING], "gzip");
        assert!(
            response
                .headers()
                .get_all(header::VARY)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .flat_map(|value| value.split(','))
                .any(|value| value.trim().eq_ignore_ascii_case("accept-encoding"))
        );
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            STAGE_CACHE_CONTROL
        );
        assert_eq!(response.headers()[X_ATLAS_CONTENT_ID], content_id);
        assert_eq!(
            response.headers()[X_ATLAS_UNCOMPRESSED_LENGTH],
            uncompressed_bytes.to_string()
        );
        assert!(
            response.headers()[header::ETAG]
                .to_str()
                .expect("ETag")
                .starts_with("W/\"atlas-v2-stage-population-")
        );
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(body.get(..2), Some([0x1f, 0x8b].as_slice()));
    }

    #[tokio::test]
    async fn source_failure_replaces_only_the_manifest_validator_and_metadata() {
        let source = runtime();
        source
            .record_success(observation())
            .await
            .expect("publish snapshot");
        let application = application(Arc::clone(&source));
        let path = "/api/v2/sources/core/mempool";
        let first = application
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).expect("request"))
            .await
            .expect("response");
        let first_etag = first.headers()[header::ETAG].clone();
        let first_body = to_bytes(first.into_body(), usize::MAX)
            .await
            .expect("manifest body");
        let first_manifest = serde_json::from_slice::<Value>(&first_body).expect("manifest JSON");
        let population_id = first_manifest["population_id"]
            .as_str()
            .expect("population ID");
        let stage_path = format!("/api/v2/sources/core/mempool/stages/population/{population_id}");
        let stage = application
            .clone()
            .oneshot(
                Request::get(&stage_path)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("stage response");
        let stage_etag = stage.headers()[header::ETAG].clone();
        let stage_body = to_bytes(stage.into_body(), usize::MAX)
            .await
            .expect("stage body");

        source
            .record_failure("node unavailable".to_owned())
            .await
            .expect("record failure");
        let response = application
            .clone()
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

        let retained_stage = application
            .oneshot(
                Request::get(&stage_path)
                    .header(header::IF_NONE_MATCH, "\"different\"")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("retained stage response");
        assert_eq!(retained_stage.status(), StatusCode::OK);
        assert_eq!(retained_stage.headers()[header::ETAG], stage_etag);
        assert_eq!(
            retained_stage.headers()[header::CACHE_CONTROL],
            STAGE_CACHE_CONTROL
        );
        assert_eq!(
            to_bytes(retained_stage.into_body(), usize::MAX)
                .await
                .expect("retained stage body"),
            stage_body
        );
    }

    #[tokio::test]
    async fn current_stage_supports_conditional_reads_after_currentness_check() {
        let source = runtime();
        source
            .record_success(observation())
            .await
            .expect("publish snapshot");
        let application = application(Arc::clone(&source));
        let (_, manifest) = get_json(application.clone(), "/api/v2/sources/core/mempool").await;
        let population_id = manifest["population_id"]
            .as_str()
            .expect("population ID")
            .to_owned();
        let population_bytes = manifest["stages"]
            .as_array()
            .expect("stage descriptors")
            .iter()
            .find(|descriptor| descriptor["kind"] == "population")
            .and_then(|descriptor| descriptor["uncompressed_bytes"].as_u64())
            .expect("population byte length");
        let path = format!("/api/v2/sources/core/mempool/stages/population/{population_id}");

        let first = application
            .clone()
            .oneshot(Request::get(&path).body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(first.headers()[header::CACHE_CONTROL], STAGE_CACHE_CONTROL);
        let etag = first.headers()[header::ETAG].clone();
        assert!(
            etag.to_str()
                .expect("ETag")
                .starts_with("W/\"atlas-v2-stage-population-")
        );
        assert_eq!(first.headers()[X_ATLAS_CONTENT_ID], population_id);
        assert_eq!(
            first.headers()[X_ATLAS_UNCOMPRESSED_LENGTH],
            population_bytes.to_string()
        );

        let not_modified = application
            .clone()
            .oneshot(
                Request::get(&path)
                    .header(header::IF_NONE_MATCH, etag.clone())
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            not_modified.headers()[header::CACHE_CONTROL],
            STAGE_CACHE_CONTROL
        );
        assert_eq!(not_modified.headers()[X_ATLAS_CONTENT_ID], population_id);
        assert_eq!(
            not_modified.headers()[X_ATLAS_UNCOMPRESSED_LENGTH],
            population_bytes.to_string()
        );

        source
            .record_success(observation_with_fee(2_400))
            .await
            .expect("replace publication");
        let superseded = application
            .oneshot(
                Request::get(&path)
                    .header(header::IF_NONE_MATCH, etag)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(superseded.status(), StatusCode::CONFLICT);
        assert_eq!(superseded.headers()[header::CACHE_CONTROL], NO_STORE);
        assert!(!superseded.headers().contains_key(header::ETAG));
    }

    #[tokio::test]
    async fn classifier_stages_use_the_classifier_specific_route() {
        let source = runtime();
        source
            .record_success(observation())
            .await
            .expect("publish snapshot");
        let application = application(source);
        let (_, manifest) = get_json(application.clone(), "/api/v2/sources/core/mempool").await;
        let descriptor = manifest["stages"]
            .as_array()
            .expect("stages")
            .iter()
            .find(|descriptor| descriptor["kind"] == "classifier")
            .expect("classifier descriptor");
        let classifier_id = descriptor["classifier_id"].as_str().expect("classifier ID");
        let content_id = descriptor["content_id"].as_str().expect("content ID");
        let path =
            format!("/api/v2/sources/core/mempool/stages/classifier/{classifier_id}/{content_id}");

        let response = application
            .oneshot(Request::get(path).body(Body::empty()).expect("request"))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()[header::ETAG]
                .to_str()
                .expect("ETag")
                .starts_with(&format!("W/\"atlas-v2-stage-classifier-{classifier_id}-"))
        );
    }

    #[tokio::test]
    async fn v2_get_routes_support_head_with_the_same_cache_contract() {
        let source = runtime();
        source
            .record_success(observation())
            .await
            .expect("publish snapshot");
        let application = application(source);

        let manifest = application
            .clone()
            .oneshot(
                Request::head("/api/v2/sources/core/mempool")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(manifest.status(), StatusCode::OK);
        assert_eq!(
            manifest.headers()[header::CACHE_CONTROL],
            MANIFEST_CACHE_CONTROL
        );
        assert!(manifest.headers().contains_key(header::ETAG));
        assert!(
            to_bytes(manifest.into_body(), usize::MAX)
                .await
                .expect("body")
                .is_empty()
        );

        let (_, manifest_body) =
            get_json(application.clone(), "/api/v2/sources/core/mempool").await;
        let stage_descriptor = manifest_body["stages"]
            .as_array()
            .expect("stages")
            .first()
            .expect("population stage");
        let stage_content_id = stage_descriptor["content_id"].as_str().expect("content ID");
        let stage = application
            .clone()
            .oneshot(
                Request::head(format!(
                    "/api/v2/sources/core/mempool/stages/population/{stage_content_id}"
                ))
                .body(Body::empty())
                .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(stage.status(), StatusCode::OK);
        assert_eq!(stage.headers()[header::CACHE_CONTROL], STAGE_CACHE_CONTROL);
        assert!(stage.headers().contains_key(header::ETAG));
        assert_eq!(stage.headers()["x-atlas-content-id"], stage_content_id);
        assert!(stage.headers().contains_key("x-atlas-uncompressed-length"));
        assert!(
            to_bytes(stage.into_body(), usize::MAX)
                .await
                .expect("body")
                .is_empty()
        );

        let sources = application
            .oneshot(
                Request::head("/api/v2/sources")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(sources.status(), StatusCode::OK);
        assert_eq!(sources.headers()[header::CACHE_CONTROL], NO_STORE);
        assert!(!sources.headers().contains_key(header::ETAG));
        assert!(
            to_bytes(sources.into_body(), usize::MAX)
                .await
                .expect("body")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn unknown_stage_and_query_dependent_stage_are_rejected_without_caching() {
        let source = runtime();
        source
            .record_success(observation())
            .await
            .expect("publish snapshot");
        let application = application(source);
        let (_, manifest) = get_json(application.clone(), "/api/v2/sources/core/mempool").await;
        let population_id = manifest["population_id"]
            .as_str()
            .expect("population ID")
            .to_owned();
        let classifier = manifest["stages"]
            .as_array()
            .expect("stages")
            .iter()
            .find(|descriptor| descriptor["kind"] == "classifier")
            .expect("classifier stage");
        let classifier_id = classifier["classifier_id"].as_str().expect("classifier ID");
        let classifier_content_id = classifier["content_id"]
            .as_str()
            .expect("classifier content ID");

        assert!(reject_query(None).is_ok());
        assert!(reject_query(Some(String::new())).is_err());

        for path in [
            "/api/v2/sources/core/mempool?".to_owned(),
            format!("/api/v2/sources/core/mempool/stages/population/{population_id}?"),
            format!(
                "/api/v2/sources/core/mempool/stages/classifier/{classifier_id}/{classifier_content_id}?"
            ),
        ] {
            let response = application
                .clone()
                .oneshot(Request::get(&path).body(Body::empty()).expect("request"))
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                NO_STORE,
                "{path}"
            );
            assert!(!response.headers().contains_key(header::ETAG), "{path}");
            let body = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body");
            assert_eq!(
                serde_json::from_slice::<Value>(&body).expect("error JSON"),
                json!({
                    "error": "query-dependent v2 representations are not supported"
                }),
                "{path}"
            );
        }

        let (status, policy) = cache_control(
            application.clone(),
            "/api/v2/sources/core/mempool?variant=old",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(policy.as_deref(), Some(NO_STORE));

        let (status, policy) = cache_control(
            application.clone(),
            &format!(
                "/api/v2/sources/core/mempool/stages/population/{}",
                "00".repeat(32)
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(policy.as_deref(), Some(NO_STORE));

        let (status, policy) = cache_control(
            application.clone(),
            &format!(
                "/api/v2/sources/core/mempool/stages/classifier/{classifier_id}/{}",
                "00".repeat(32)
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(policy.as_deref(), Some(NO_STORE));

        let (status, policy) = cache_control(
            application.clone(),
            &format!(
                "/api/v2/sources/core/mempool/stages/population/{}?variant=old",
                "00".repeat(32)
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(policy.as_deref(), Some(NO_STORE));

        let (status, policy) = cache_control(
            application.clone(),
            &format!(
                "/api/v2/sources/core/mempool/stages/classifier/{classifier_id}/{classifier_content_id}?variant=old"
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(policy.as_deref(), Some(NO_STORE));

        for (path, expected) in [
            (
                "/api/v2/sources/core/mempool/stages/population/not-a-digest".to_owned(),
                StatusCode::BAD_REQUEST,
            ),
            (
                format!(
                    "/api/v2/sources/core/mempool/stages/unknown/{}",
                    "00".repeat(32)
                ),
                StatusCode::BAD_REQUEST,
            ),
            (
                format!(
                    "/api/v2/sources/core/mempool/stages/classifier/Unknown/{}",
                    "00".repeat(32)
                ),
                StatusCode::BAD_REQUEST,
            ),
            (
                format!(
                    "/api/v2/sources/core/mempool/stages/classifier/unknown/{}",
                    "00".repeat(32)
                ),
                StatusCode::NOT_FOUND,
            ),
            (
                format!("/api/v2/sources/core/mempool/stages/membership/{population_id}"),
                StatusCode::NOT_FOUND,
            ),
            (
                "/api/v2/sources/knots/mempool/stages/population/not-a-digest".to_owned(),
                StatusCode::BAD_REQUEST,
            ),
            (
                format!(
                    "/api/v2/sources/knots/mempool/stages/population/{}",
                    "00".repeat(32)
                ),
                StatusCode::NOT_FOUND,
            ),
        ] {
            let (status, policy) = cache_control(application.clone(), &path).await;
            assert_eq!(status, expected, "{path}");
            assert_eq!(policy.as_deref(), Some(NO_STORE), "{path}");
        }
    }

    #[tokio::test]
    async fn unknown_source_is_explicit() {
        let (status, response) =
            get_json(application(runtime()), "/api/v2/sources/knots/mempool").await;

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

        let response = application(source)
            .oneshot(
                Request::get(format!("/api/v2/sources/core/transactions/{txid}"))
                    .header(header::IF_NONE_MATCH, "*")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], NO_STORE);
        assert!(!response.headers().contains_key(header::ETAG));
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("detail body");
        let response = serde_json::from_slice::<Value>(&body).expect("detail JSON");
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
            &format!("/api/v2/sources/core/transactions/{txid}"),
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
