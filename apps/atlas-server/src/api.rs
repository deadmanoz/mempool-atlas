use std::sync::Arc;

use atlas_model::{
    IngestBatchRequest, IngestBatchResponse, IngestResponse, MAX_INGEST_BATCH_BODY_BYTES,
    MempoolSnapshot, NormalizedEvent, SourceId,
};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;

use crate::store::{Store, StoreError};

pub fn router(store: Store) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route(
            "/api/v1/events",
            post(ingest_event).layer(DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
        .route(
            "/api/v1/events/batch",
            post(ingest_batch).layer(DefaultBodyLimit::max(MAX_INGEST_BATCH_BODY_BYTES)),
        )
        .route("/api/v1/sources/{source_id}/mempool", get(mempool))
        .with_state(AppState {
            store: Arc::new(store),
        })
}

#[derive(Clone)]
struct AppState {
    store: Arc<Store>,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn ingest_event(
    State(state): State<AppState>,
    Json(event): Json<NormalizedEvent>,
) -> Result<(StatusCode, Json<IngestResponse>), ApiError> {
    let event_id = event.event_id.clone();
    let store = Arc::clone(&state.store);
    let status = tokio::task::spawn_blocking(move || store.ingest(&event))
        .await
        .map_err(ApiError::Task)??;
    Ok((
        StatusCode::ACCEPTED,
        Json(IngestResponse { event_id, status }),
    ))
}

async fn ingest_batch(
    State(state): State<AppState>,
    Json(request): Json<IngestBatchRequest>,
) -> Result<(StatusCode, Json<IngestBatchResponse>), ApiError> {
    request.validate()?;
    let event_ids = request
        .events
        .iter()
        .map(|event| event.event_id.clone())
        .collect::<Vec<_>>();
    let store = Arc::clone(&state.store);
    let statuses = tokio::task::spawn_blocking(move || store.ingest_batch(&request))
        .await
        .map_err(ApiError::Task)??;
    let acknowledgements = event_ids
        .into_iter()
        .zip(statuses)
        .map(|(event_id, status)| IngestResponse { event_id, status })
        .collect();
    Ok((
        StatusCode::ACCEPTED,
        Json(IngestBatchResponse { acknowledgements }),
    ))
}

async fn mempool(
    State(state): State<AppState>,
    Path(source_id): Path<String>,
) -> Result<Json<MempoolSnapshot>, ApiError> {
    let source_id = SourceId::new(source_id)?;
    let store = Arc::clone(&state.store);
    let requested_source = source_id.clone();
    let snapshot = tokio::task::spawn_blocking(move || store.mempool(&source_id))
        .await
        .map_err(ApiError::Task)??
        .ok_or(ApiError::SourceNotFound(requested_source))?;
    Ok(Json(snapshot))
}

#[derive(Debug)]
enum ApiError {
    Model(atlas_model::ModelError),
    SourceNotFound(SourceId),
    Store(StoreError),
    Task(tokio::task::JoinError),
}

impl From<atlas_model::ModelError> for ApiError {
    fn from(error: atlas_model::ModelError) -> Self {
        Self::Model(error)
    }
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::Model(error) => (StatusCode::BAD_REQUEST, error.to_string()),
            Self::SourceNotFound(source_id) => (
                StatusCode::NOT_FOUND,
                format!("source {source_id} was not found"),
            ),
            Self::Store(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
            Self::Task(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
        };
        (status, Json(ErrorResponse { error: message })).into_response()
    }
}
