use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use atlas_classifiers::registered_packs;
use atlas_model::{
    CheckpointId, IngestBatchRequest, IngestBatchResponse, IngestResponse,
    MAX_INGEST_BATCH_BODY_BYTES, MAX_SOURCE_REPLICA_BODY_BYTES, MempoolSnapshot, MempoolSummary,
    ModelError, NormalizedEvent, RejectionAvailability, ReplicaCursor, ScriptType,
    SourceComparison, SourceId, SourceRejections, SourceReplicaRequest, SourceReplicaResponse,
    SourcesResponse, SummaryDetail, SummaryFilter, TaxonomyDescriptor, TaxonomyFilter,
};
use axum::extract::{DefaultBodyLimit, Path, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;

use crate::comparison::{ComparisonOutcome, compute_comparison};
use crate::rejections::{
    REJECTION_PAGE_DEFAULT, REJECTION_PAGE_MAX, RejectionCursor, compute_rejections,
    rejections_not_collected,
};
use crate::store::{Store, StoreError};
use crate::summary::compute_summary;

pub fn router(store: Store) -> Router {
    router_with_clock(store, system_now_ms)
}

/// Builds the production router with an explicit clock so tests and fixture
/// generation can produce deterministic `as_of_ms` values. The production
/// surface deliberately excludes legacy event-ledger ingest.
pub fn router_with_clock(store: Store, now_ms: fn() -> u64) -> Router {
    build_router(store, now_ms, false)
}

/// Builds the legacy evidence-ingest surface for bounded experiments and
/// contract tests. The server binary never mounts this router.
pub fn experimental_evidence_router(store: Store) -> Router {
    experimental_evidence_router_with_clock(store, system_now_ms)
}

/// Builds the experimental evidence router with a deterministic clock.
pub fn experimental_evidence_router_with_clock(store: Store, now_ms: fn() -> u64) -> Router {
    build_router(store, now_ms, true)
}

fn build_router(store: Store, now_ms: fn() -> u64, include_evidence_ingest: bool) -> Router {
    let application = Router::new()
        .route("/healthz", get(health))
        .route(
            "/api/v1/state",
            post(ingest_state).layer(DefaultBodyLimit::max(MAX_SOURCE_REPLICA_BODY_BYTES)),
        )
        .route("/api/v1/sources", get(sources))
        .route("/api/v1/sources/compare", get(compare))
        .route("/api/v1/sources/{source_id}/mempool", get(mempool))
        .route(
            "/api/v1/sources/{source_id}/mempool/summary",
            get(mempool_summary),
        )
        .route("/api/v1/sources/{source_id}/rejections", get(rejections));

    let application = if include_evidence_ingest {
        application
            .route(
                "/api/v1/events",
                post(ingest_event).layer(DefaultBodyLimit::max(16 * 1024 * 1024)),
            )
            .route(
                "/api/v1/events/batch",
                post(ingest_batch).layer(DefaultBodyLimit::max(MAX_INGEST_BATCH_BODY_BYTES)),
            )
    } else {
        application
    };

    application.with_state(AppState {
        store: Arc::new(store),
        now_ms,
        rejection_availability: if include_evidence_ingest {
            RejectionAvailability::Available
        } else {
            RejectionAvailability::NotCollected
        },
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
    rejection_availability: RejectionAvailability,
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

async fn ingest_state(
    State(state): State<AppState>,
    Json(request): Json<SourceReplicaRequest>,
) -> Result<(StatusCode, Json<SourceReplicaResponse>), ApiError> {
    request.validate().map_err(|error| match error {
        atlas_model::SourceReplicaError::LimitExceeded { .. } => {
            ApiError::StateCapacity(error.to_string())
        }
        _ => ApiError::InvalidStateRequest(error.to_string()),
    })?;
    let store = Arc::clone(&state.store);
    let response = tokio::task::spawn_blocking(move || store.apply_source_replica(&request))
        .await
        .map_err(ApiError::Task)??;
    Ok((StatusCode::ACCEPTED, Json(response)))
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

/// The inclusive bounds on the number of sources a comparison accepts. Two is
/// the minimum for a delta; four keeps the derived-region fan-out bounded.
const COMPARE_MIN_SOURCES: usize = 2;
const COMPARE_MAX_SOURCES: usize = 4;

/// Parses the comparison query grammar. The only accepted parameter is
/// `sources`: an ordered, comma-separated list of two to four distinct valid
/// source IDs. Unknown or repeated parameters, a missing list, an out-of-range
/// count, a duplicate, or an invalid source ID are all rejected so a typo never
/// silently changes the comparison. Existence of each source is checked later
/// against the store.
fn parse_compare_query(query: Option<&str>) -> Result<Vec<SourceId>, ApiError> {
    let parameters: Vec<(String, String)> = serde_urlencoded::from_str(query.unwrap_or(""))
        .map_err(|error| ApiError::InvalidQuery(error.to_string()))?;

    let mut sources_value = None;
    for (parameter, value) in parameters {
        match parameter.as_str() {
            "sources" => {
                reject_duplicate("sources", sources_value.is_some())?;
                sources_value = Some(value);
            }
            other => {
                return Err(ApiError::InvalidQuery(format!("unknown parameter {other}")));
            }
        }
    }
    let Some(raw) = sources_value else {
        return Err(ApiError::InvalidQuery(
            "missing required parameter sources".to_owned(),
        ));
    };

    let mut sources = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for token in raw.split(',') {
        let source = SourceId::new(token.to_owned())?;
        if !seen.insert(source.as_str().to_owned()) {
            return Err(ApiError::InvalidQuery(format!("duplicate source {source}")));
        }
        sources.push(source);
    }
    if !(COMPARE_MIN_SOURCES..=COMPARE_MAX_SOURCES).contains(&sources.len()) {
        return Err(ApiError::InvalidQuery(format!(
            "compare requires between {COMPARE_MIN_SOURCES} and {COMPARE_MAX_SOURCES} sources, got {}",
            sources.len()
        )));
    }
    Ok(sources)
}

async fn compare(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<Json<SourceComparison>, ApiError> {
    let sources = parse_compare_query(query.as_deref())?;
    let taxonomies: Vec<TaxonomyDescriptor> = registered_packs()
        .iter()
        .map(|pack| pack.taxonomy())
        .collect();
    let store = Arc::clone(&state.store);
    let as_of_ms = (state.now_ms)();
    let comparison = tokio::task::spawn_blocking(move || -> Result<_, StoreError> {
        Ok(match store.source_comparison(&sources)? {
            ComparisonOutcome::Computed(inputs) => {
                Ok(compute_comparison(sources, &inputs, as_of_ms, &taxonomies))
            }
            ComparisonOutcome::UnknownSource(source_id) => Err(source_id),
        })
    })
    .await
    .map_err(ApiError::Task)??
    .map_err(ApiError::SourceNotFound)?;
    Ok(Json(comparison))
}

/// The prefix that scopes a query parameter to one taxonomy's verdict filter,
/// as in `t.behavior=payment,data`.
const TAXONOMY_PARAMETER_PREFIX: &str = "t.";

/// Parses the raw summary query grammar. The accepted parameters are exactly
/// `script`, `feerate_min`, `feerate_max`, `detail`, and one `t.<taxonomy>`
/// verdict list per registered taxonomy; facet lists are comma-separated.
/// Unknown or repeated parameters are rejected so typos never silently widen
/// a filter, and taxonomy keys and verdicts must exist in the registry.
fn parse_summary_query(
    query: Option<&str>,
    taxonomies: &[TaxonomyDescriptor],
) -> Result<(SummaryFilter, SummaryDetail), ApiError> {
    let parameters: Vec<(String, String)> = serde_urlencoded::from_str(query.unwrap_or(""))
        .map_err(|error| ApiError::InvalidQuery(error.to_string()))?;

    let mut taxonomy_filters: Vec<TaxonomyFilter> = Vec::new();
    let mut scripts = None;
    let mut feerate_min = None;
    let mut feerate_max = None;
    let mut detail = SummaryDetail::default();
    let mut detail_seen = false;
    for (parameter, value) in parameters {
        if let Some(taxonomy_key) = parameter.strip_prefix(TAXONOMY_PARAMETER_PREFIX) {
            taxonomy_filters.push(parse_taxonomy_filter(taxonomy_key, &value, taxonomies)?);
            continue;
        }
        match parameter.as_str() {
            "script" => {
                reject_duplicate("script", scripts.is_some())?;
                scripts = Some(parse_facet_list(&value, "script", |key| {
                    ScriptType::from_key(key).ok_or_else(|| ModelError::UnknownFilterValue {
                        facet: "script",
                        value: key.to_owned(),
                    })
                })?);
            }
            "feerate_min" => {
                reject_duplicate("feerate_min", feerate_min.is_some())?;
                feerate_min = Some(parse_feerate_bound("feerate_min", &value)?);
            }
            "feerate_max" => {
                reject_duplicate("feerate_max", feerate_max.is_some())?;
                feerate_max = Some(parse_feerate_bound("feerate_max", &value)?);
            }
            "detail" => {
                reject_duplicate("detail", detail_seen)?;
                detail_seen = true;
                for selection in value.split(',') {
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
            other => {
                return Err(ApiError::InvalidQuery(format!("unknown parameter {other}")));
            }
        }
    }

    let filter = SummaryFilter {
        taxonomies: taxonomy_filters,
        scripts,
        feerate_min,
        feerate_max,
    };
    filter.validate()?;
    Ok((filter, detail))
}

/// Parses one `t.<taxonomy>=<verdict,verdict>` parameter against the
/// registered taxonomy vocabularies. Duplicate parameters for the same
/// taxonomy are detected by [`SummaryFilter::validate`].
fn parse_taxonomy_filter(
    taxonomy_key: &str,
    value: &str,
    taxonomies: &[TaxonomyDescriptor],
) -> Result<TaxonomyFilter, ApiError> {
    let taxonomy = taxonomies
        .iter()
        .find(|taxonomy| taxonomy.key == taxonomy_key)
        .ok_or_else(|| ModelError::UnknownTaxonomy {
            key: taxonomy_key.to_owned(),
        })?;
    if value.is_empty() {
        return Err(ModelError::EmptyTaxonomyFilter {
            taxonomy: taxonomy_key.to_owned(),
        }
        .into());
    }
    let verdicts = value
        .split(',')
        .map(|verdict| {
            if taxonomy
                .verdicts
                .iter()
                .any(|descriptor| descriptor.key == verdict)
            {
                Ok(verdict.to_owned())
            } else {
                Err(ModelError::UnknownTaxonomyVerdict {
                    taxonomy: taxonomy_key.to_owned(),
                    verdict: verdict.to_owned(),
                })
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(TaxonomyFilter {
        key: taxonomy_key.to_owned(),
        verdicts,
    })
}

fn reject_duplicate(parameter: &str, seen: bool) -> Result<(), ApiError> {
    if seen {
        return Err(ApiError::InvalidQuery(format!(
            "duplicate parameter {parameter}"
        )));
    }
    Ok(())
}

fn parse_feerate_bound(field: &'static str, value: &str) -> Result<f64, ApiError> {
    value
        .parse()
        .map_err(|_| ApiError::InvalidQuery(format!("{field} is not a number: {value}")))
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
    let taxonomies: Vec<TaxonomyDescriptor> = registered_packs()
        .iter()
        .map(|pack| pack.taxonomy())
        .collect();
    let (filter, detail) = parse_summary_query(query.as_deref(), &taxonomies)?;
    let store = Arc::clone(&state.store);
    let requested_source = source_id.clone();
    let as_of_ms = (state.now_ms)();
    let summary = tokio::task::spawn_blocking(move || -> Result<_, StoreError> {
        let Some(facts) = store.mempool_facts(&source_id)? else {
            return Ok(None);
        };
        Ok(Some(compute_summary(
            source_id,
            &facts,
            &filter,
            detail,
            as_of_ms,
            &taxonomies,
        )))
    })
    .await
    .map_err(ApiError::Task)??
    .ok_or(ApiError::SourceNotFound(requested_source))?;
    Ok(Json(summary))
}

/// Parses the rejection query grammar. The only accepted parameters are
/// `limit` and `before`; unknown or repeated parameters are rejected so a typo
/// never silently changes the page. `limit` defaults to
/// [`REJECTION_PAGE_DEFAULT`], is clamped up to [`REJECTION_PAGE_MAX`], and a
/// zero or non-numeric value is an error. A malformed cursor is an error.
fn parse_rejections_query(
    query: Option<&str>,
) -> Result<(usize, Option<RejectionCursor>), ApiError> {
    let parameters: Vec<(String, String)> = serde_urlencoded::from_str(query.unwrap_or(""))
        .map_err(|error| ApiError::InvalidQuery(error.to_string()))?;

    let mut limit = None;
    let mut before = None;
    for (parameter, value) in parameters {
        match parameter.as_str() {
            "limit" => {
                reject_duplicate("limit", limit.is_some())?;
                limit = Some(parse_rejection_limit(&value)?);
            }
            "before" => {
                reject_duplicate("before", before.is_some())?;
                match RejectionCursor::parse(&value) {
                    Ok(cursor) => before = Some(cursor),
                    Err(_) => return Err(ApiError::InvalidCursor(value)),
                }
            }
            other => {
                return Err(ApiError::InvalidQuery(format!("unknown parameter {other}")));
            }
        }
    }
    Ok((limit.unwrap_or(REJECTION_PAGE_DEFAULT), before))
}

fn parse_rejection_limit(value: &str) -> Result<usize, ApiError> {
    let parsed: usize = value
        .parse()
        .map_err(|_| ApiError::InvalidQuery(format!("limit is not a number: {value}")))?;
    if parsed == 0 {
        return Err(ApiError::InvalidQuery(
            "limit must be greater than zero".to_owned(),
        ));
    }
    Ok(parsed.min(REJECTION_PAGE_MAX))
}

async fn rejections(
    State(state): State<AppState>,
    Path(source_id): Path<String>,
    RawQuery(query): RawQuery,
) -> Result<Json<SourceRejections>, ApiError> {
    let source_id = SourceId::new(source_id)?;
    let (limit, before) = parse_rejections_query(query.as_deref())?;
    if let Some(cursor) = &before
        && cursor.source_id() != source_id.as_str()
    {
        return Err(ApiError::InvalidCursor(cursor.encode()));
    }
    let taxonomies: Vec<TaxonomyDescriptor> = registered_packs()
        .iter()
        .map(|pack| pack.taxonomy())
        .collect();
    let store = Arc::clone(&state.store);
    let availability = state.rejection_availability;
    let requested_source = source_id.clone();
    let as_of_ms = (state.now_ms)();
    let rejections = tokio::task::spawn_blocking(move || -> Result<_, StoreError> {
        if availability == RejectionAvailability::NotCollected {
            return Ok(store
                .active_source_exists(&source_id)?
                .then(|| rejections_not_collected(source_id, as_of_ms)));
        }
        let Some(inputs) = store.rejections(&source_id, limit, before)? else {
            return Ok(None);
        };
        Ok(Some(compute_rejections(
            source_id,
            &inputs,
            as_of_ms,
            &taxonomies,
        )))
    })
    .await
    .map_err(ApiError::Task)??
    .ok_or(ApiError::SourceNotFound(requested_source))?;
    Ok(Json(rejections))
}

#[derive(Debug)]
enum ApiError {
    Model(atlas_model::ModelError),
    InvalidStateRequest(String),
    StateCapacity(String),
    InvalidQuery(String),
    InvalidCursor(String),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_cursor: Option<Option<ReplicaCursor>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    staging_checkpoint_id: Option<CheckpointId>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message, code, active_cursor, staging_checkpoint_id) = match self {
            Self::Model(error) => (StatusCode::BAD_REQUEST, error.to_string(), None, None, None),
            Self::InvalidStateRequest(message) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                message,
                Some("invalid_state_request"),
                Some(None),
                None,
            ),
            Self::StateCapacity(message) => (
                StatusCode::CONFLICT,
                message,
                Some("capacity_exceeded"),
                Some(None),
                None,
            ),
            Self::InvalidQuery(message) => (
                StatusCode::BAD_REQUEST,
                format!("invalid query string: {message}"),
                None,
                None,
                None,
            ),
            Self::InvalidCursor(value) => (
                StatusCode::BAD_REQUEST,
                format!("invalid cursor: {value}"),
                None,
                None,
                None,
            ),
            Self::SourceNotFound(source_id) => (
                StatusCode::NOT_FOUND,
                format!("source {source_id} was not found"),
                None,
                None,
                None,
            ),
            Self::Store(StoreError::SourceReplicaConflict {
                code,
                message,
                active_cursor,
                staging_checkpoint_id,
            }) => (
                StatusCode::CONFLICT,
                message,
                Some(code),
                Some(active_cursor),
                staging_checkpoint_id,
            ),
            Self::Store(StoreError::SourceReplicaCapacity {
                message,
                active_cursor,
            }) => (
                StatusCode::CONFLICT,
                message,
                Some("capacity_exceeded"),
                Some(active_cursor),
                None,
            ),
            Self::Store(StoreError::InvalidSourceReplica {
                message,
                active_cursor,
            }) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                message,
                Some("invalid_state_request"),
                Some(active_cursor),
                None,
            ),
            Self::Store(StoreError::SourceReplicaModel(error)) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                error.to_string(),
                Some("invalid_state_request"),
                Some(None),
                None,
            ),
            Self::Store(error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
                None,
                None,
                None,
            ),
            Self::Task(error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
                None,
                None,
                None,
            ),
        };
        (
            status,
            Json(ErrorResponse {
                error: message,
                code,
                active_cursor,
                staging_checkpoint_id,
            }),
        )
            .into_response()
    }
}
