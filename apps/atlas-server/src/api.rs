use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use atlas_model::{
    Classification, IngestBatchRequest, IngestBatchResponse, IngestResponse,
    MAX_INGEST_BATCH_BODY_BYTES, MempoolSnapshot, MempoolSummary, ModelError, NormalizedEvent,
    ScriptType, SourceId, SourcesResponse, SummaryDetail, SummaryFilter,
};
use axum::extract::{DefaultBodyLimit, Path, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::store::{Store, StoreError};
use crate::summary::compute_summary;

pub fn router(store: Store) -> Router {
    router_with_clock(store, system_now_ms)
}

/// Builds the router with an explicit clock so tests and fixture generation
/// can produce deterministic `as_of_ms` values.
pub fn router_with_clock(store: Store, now_ms: fn() -> u64) -> Router {
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
        .route("/api/v1/sources", get(sources))
        .route("/api/v1/sources/{source_id}/mempool", get(mempool))
        .route(
            "/api/v1/sources/{source_id}/mempool/summary",
            get(mempool_summary),
        )
        .with_state(AppState {
            store: Arc::new(store),
            now_ms,
        })
}

fn system_now_ms() -> u64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch");
    u64::try_from(elapsed.as_millis()).expect("system clock beyond representable milliseconds")
}

#[derive(Clone)]
struct AppState {
    store: Arc<Store>,
    now_ms: fn() -> u64,
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

async fn sources(State(state): State<AppState>) -> Result<Json<SourcesResponse>, ApiError> {
    let store = Arc::clone(&state.store);
    let sources = tokio::task::spawn_blocking(move || store.sources())
        .await
        .map_err(ApiError::Task)??;
    Ok(Json(SourcesResponse { sources }))
}

/// Raw summary query grammar. Facet lists are comma-separated; unknown
/// parameters are rejected so typos never silently widen a filter.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SummaryQuery {
    class: Option<String>,
    script: Option<String>,
    feerate_min: Option<f64>,
    feerate_max: Option<f64>,
    detail: Option<String>,
}

fn parse_summary_query(query: Option<&str>) -> Result<(SummaryFilter, SummaryDetail), ApiError> {
    let query: SummaryQuery = serde_urlencoded::from_str(query.unwrap_or(""))
        .map_err(|error| ApiError::InvalidQuery(error.to_string()))?;

    let classes = query
        .class
        .as_deref()
        .map(|list| {
            parse_facet_list(list, "class", |key| {
                Classification::from_key(key).ok_or_else(|| ModelError::UnknownFilterValue {
                    facet: "class",
                    value: key.to_owned(),
                })
            })
        })
        .transpose()?;
    let scripts = query
        .script
        .as_deref()
        .map(|list| {
            parse_facet_list(list, "script", |key| {
                ScriptType::from_key(key).ok_or_else(|| ModelError::UnknownFilterValue {
                    facet: "script",
                    value: key.to_owned(),
                })
            })
        })
        .transpose()?;
    let filter = SummaryFilter {
        classes,
        scripts,
        feerate_min: query.feerate_min,
        feerate_max: query.feerate_max,
    };
    filter.validate()?;

    let mut detail = SummaryDetail::default();
    if let Some(selections) = query.detail.as_deref() {
        for selection in selections.split(',') {
            match selection {
                "ecdf" => detail.ecdf = true,
                "joint" => detail.joint_fee_size = true,
                other => {
                    return Err(ModelError::UnknownDetailSelection {
                        value: other.to_owned(),
                    }
                    .into());
                }
            }
        }
    }
    Ok((filter, detail))
}

fn parse_facet_list<T>(
    list: &str,
    facet: &'static str,
    parse: impl Fn(&str) -> Result<T, ModelError>,
) -> Result<Vec<T>, ModelError> {
    if list.is_empty() {
        return Err(ModelError::EmptyFilterFacet { facet });
    }
    list.split(',').map(parse).collect()
}

async fn mempool_summary(
    State(state): State<AppState>,
    Path(source_id): Path<String>,
    RawQuery(query): RawQuery,
) -> Result<Json<MempoolSummary>, ApiError> {
    let source_id = SourceId::new(source_id)?;
    let (filter, detail) = parse_summary_query(query.as_deref())?;
    let store = Arc::clone(&state.store);
    let requested_source = source_id.clone();
    let as_of_ms = (state.now_ms)();
    let summary = tokio::task::spawn_blocking(move || -> Result<_, StoreError> {
        let Some(facts) = store.mempool_facts(&source_id)? else {
            return Ok(None);
        };
        Ok(Some(compute_summary(
            source_id, &facts, &filter, detail, as_of_ms,
        )))
    })
    .await
    .map_err(ApiError::Task)??
    .ok_or(ApiError::SourceNotFound(requested_source))?;
    Ok(Json(summary))
}

#[derive(Debug)]
enum ApiError {
    Model(atlas_model::ModelError),
    InvalidQuery(String),
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
            Self::InvalidQuery(message) => (
                StatusCode::BAD_REQUEST,
                format!("invalid query string: {message}"),
            ),
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
