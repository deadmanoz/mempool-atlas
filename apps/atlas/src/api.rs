use std::path::PathBuf;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tower_http::services::ServeDir;

use crate::model::SourcesResponse;
use crate::runtime::SourceRegistry;

pub fn router(registry: SourceRegistry, web_root: PathBuf) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(readiness))
        .route("/api/v1/sources", get(sources))
        .route("/api/v1/sources/{source_id}/mempool", get(source_snapshot))
        .fallback_service(ServeDir::new(web_root).append_index_html_on_directories(true))
        .with_state(registry)
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
        [(header::CACHE_CONTROL.as_str(), "no-store")],
        Json(registry.sources_response().await),
    )
}

async fn source_snapshot(
    State(registry): State<SourceRegistry>,
    Path(source_id): Path<String>,
) -> Result<Response, ApiError> {
    let source = registry
        .get(&source_id)
        .ok_or_else(|| ApiError::source_not_found(&source_id))?;
    let body = source
        .snapshot_response_bytes()
        .await
        .map_err(ApiError::snapshot_response)?;
    Ok((
        StatusCode::OK,
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::CONTENT_TYPE, "application/json"),
        ],
        Body::from(body),
    )
        .into_response())
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
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            [(header::CACHE_CONTROL, "no-store")],
            Json(ErrorResponse {
                error: self.message,
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;
    use crate::model::{ChainTip, MempoolEntry, MempoolSnapshot};
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
        let web_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web");
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
    async fn current_snapshot_is_served_after_atomic_publication() {
        let source = runtime();
        source
            .record_success(snapshot())
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
    async fn failed_poll_serves_last_good_snapshot_as_stale() {
        let source = runtime();
        source
            .record_success(snapshot())
            .await
            .expect("publish snapshot");
        source
            .record_failure("node unavailable".to_owned())
            .await
            .expect("record failure");

        let (status, response) =
            get_json(application(source), "/api/v1/sources/core/mempool").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["source"]["availability"], "stale");
        assert_eq!(response["source"]["last_error"], "node unavailable");
        assert_eq!(response["snapshot"]["transaction_count"], 1);
    }

    #[tokio::test]
    async fn unknown_source_is_explicit() {
        let (status, response) =
            get_json(application(runtime()), "/api/v1/sources/knots/mempool").await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(response["error"], "unknown source \"knots\"");
    }
}
