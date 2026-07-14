use std::sync::Arc;

use atlas_model::{IngestResponse, MempoolSnapshot, NormalizedEvent, SourceId};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::store::{Store, StoreError};

pub fn router(store: Store) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/api/v1/events", post(ingest_event))
        .route("/api/v1/mempool", get(mempool))
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

#[derive(Debug, Deserialize)]
struct MempoolQuery {
    source: Option<String>,
}

async fn mempool(
    State(state): State<AppState>,
    Query(query): Query<MempoolQuery>,
) -> Result<Json<MempoolSnapshot>, ApiError> {
    let source_id = query.source.map(SourceId::new).transpose()?;
    let store = Arc::clone(&state.store);
    let snapshot = tokio::task::spawn_blocking(move || store.mempool(source_id.as_ref()))
        .await
        .map_err(ApiError::Task)??;
    Ok(Json(snapshot))
}

#[derive(Debug)]
enum ApiError {
    Model(atlas_model::ModelError),
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
            Self::Store(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
            Self::Task(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
        };
        (status, Json(ErrorResponse { error: message })).into_response()
    }
}
