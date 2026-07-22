//! Strict FIFO delivery from the node-local outbox to Atlas ingest.

use atlas_model::{
    IngestBatchRequest, IngestBatchResponse, IngestResponse, IngestStatus,
    MAX_INGEST_BATCH_BODY_BYTES, NormalizedEvent, SourceId,
};
use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, StatusCode};
use thiserror::Error;

use crate::outbox::{Outbox, OutboxError, PendingDelivery, PendingEvent};

const MAX_STORED_ERROR_CHARS: usize = 2_000;
const MAX_ERROR_RESPONSE_BODY_BYTES: usize = 1_024;
const MAX_RENDERED_ERROR_BODY_CHARS: usize = 1_400;
const MAX_RESPONSE_READ_ERROR_CHARS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryFailureClass {
    Transient,
    OperatorAction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeliveryRetryPolicy {
    initial: std::time::Duration,
    maximum: std::time::Duration,
}

impl DeliveryRetryPolicy {
    pub fn new(
        initial: std::time::Duration,
        maximum: std::time::Duration,
    ) -> Result<Self, DeliveryRetryPolicyError> {
        if initial < std::time::Duration::from_millis(1) || maximum < initial {
            return Err(DeliveryRetryPolicyError { initial, maximum });
        }
        Ok(Self { initial, maximum })
    }

    #[must_use]
    pub fn initial(self) -> std::time::Duration {
        self.initial
    }

    #[must_use]
    pub fn delay(
        self,
        class: DeliveryFailureClass,
        source_id: &SourceId,
        outbox_id: i64,
        attempts: u64,
    ) -> std::time::Duration {
        let initial_ms = duration_millis(self.initial);
        let maximum_ms = duration_millis(self.maximum);
        let ceiling_ms = match class {
            DeliveryFailureClass::Transient => {
                let exponent = attempts.saturating_sub(1);
                let multiplier = u32::try_from(exponent)
                    .ok()
                    .and_then(|exponent| 1_u64.checked_shl(exponent))
                    .unwrap_or(u64::MAX);
                initial_ms.saturating_mul(multiplier).min(maximum_ms)
            }
            DeliveryFailureClass::OperatorAction => maximum_ms,
        };
        let lower_ms = ceiling_ms.div_ceil(2);
        let jitter_width = ceiling_ms - lower_ms;
        let jitter = deterministic_retry_hash(source_id, outbox_id, attempts)
            % jitter_width.saturating_add(1);
        std::time::Duration::from_millis(lower_ms + jitter)
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error(
    "initial delivery retry interval {initial:?} must be at least 1ms, and maximum delivery retry interval {maximum:?} must be greater than or equal to it"
)]
pub struct DeliveryRetryPolicyError {
    initial: std::time::Duration,
    maximum: std::time::Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeliveryOutcome {
    Idle,
    Delivered {
        outbox_id: i64,
        event_id: String,
        status: IngestStatus,
    },
    BatchDelivered {
        first_outbox_id: i64,
        last_outbox_id: i64,
        event_count: usize,
    },
    Deferred {
        outbox_id: i64,
        attempts: u64,
        class: DeliveryFailureClass,
        error: String,
    },
}

#[derive(Debug, Error)]
pub enum DeliveryError {
    #[error("outbox error: {0}")]
    Outbox(#[from] OutboxError),
}

pub async fn deliver_next(
    outbox: &Outbox,
    client: &Client,
    endpoint: &str,
) -> Result<DeliveryOutcome, DeliveryError> {
    let Some(pending) = outbox.next_delivery()? else {
        return Ok(DeliveryOutcome::Idle);
    };

    match pending {
        PendingDelivery::Single(pending) => deliver_single(outbox, client, endpoint, pending).await,
        PendingDelivery::ReconciliationBatch(pending) => {
            deliver_reconciliation_batch(outbox, client, endpoint, pending).await
        }
    }
}

async fn deliver_single(
    outbox: &Outbox,
    client: &Client,
    endpoint: &str,
    pending: PendingEvent,
) -> Result<DeliveryOutcome, DeliveryError> {
    let response = match client.post(endpoint).json(&pending.event).send().await {
        Ok(response) => response,
        Err(error) => {
            return defer(
                outbox,
                &pending,
                DeliveryFailureClass::Transient,
                format!("request failed: {error}"),
            );
        }
    };

    if response.status() != StatusCode::ACCEPTED {
        let (class, error) = unexpected_status_error(response, "unexpected HTTP status").await;
        return defer(outbox, &pending, class, error);
    }

    let acknowledgement = match response.json::<IngestResponse>().await {
        Ok(acknowledgement) => acknowledgement,
        Err(error) => {
            return defer(
                outbox,
                &pending,
                DeliveryFailureClass::OperatorAction,
                format!("invalid ingest acknowledgement: {error}"),
            );
        }
    };
    if acknowledgement.event_id != pending.event.event_id {
        return defer(
            outbox,
            &pending,
            DeliveryFailureClass::OperatorAction,
            format!(
                "acknowledged event {} instead of {}",
                acknowledgement.event_id, pending.event.event_id
            ),
        );
    }

    outbox.mark_delivered(pending.outbox_id)?;
    Ok(DeliveryOutcome::Delivered {
        outbox_id: pending.outbox_id,
        event_id: pending.event.event_id,
        status: acknowledgement.status,
    })
}

async fn deliver_reconciliation_batch(
    outbox: &Outbox,
    client: &Client,
    endpoint: &str,
    pending: Vec<PendingEvent>,
) -> Result<DeliveryOutcome, DeliveryError> {
    let head = pending
        .first()
        .expect("reconciliation delivery batch is never empty");
    let events = pending
        .iter()
        .map(|pending| pending.event.clone())
        .collect::<Vec<NormalizedEvent>>();
    let request = IngestBatchRequest { events };
    let body = match serde_json::to_vec(&request) {
        Ok(body) => body,
        Err(error) => {
            return defer(
                outbox,
                head,
                DeliveryFailureClass::OperatorAction,
                format!("batch serialization failed: {error}"),
            );
        }
    };
    if body.len() > MAX_INGEST_BATCH_BODY_BYTES {
        return defer(
            outbox,
            head,
            DeliveryFailureClass::OperatorAction,
            format!(
                "encoded batch body is {} bytes; maximum is {MAX_INGEST_BATCH_BODY_BYTES}",
                body.len()
            ),
        );
    }
    let batch_endpoint = format!("{}/batch", endpoint.trim_end_matches('/'));
    let response = match client
        .post(batch_endpoint)
        .header(CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return defer(
                outbox,
                head,
                DeliveryFailureClass::Transient,
                format!("batch request failed: {error}"),
            );
        }
    };

    if response.status() != StatusCode::ACCEPTED {
        let (class, error) =
            unexpected_status_error(response, "unexpected batch HTTP status").await;
        return defer(outbox, head, class, error);
    }

    let acknowledgement = match response.json::<IngestBatchResponse>().await {
        Ok(acknowledgement) => acknowledgement,
        Err(error) => {
            return defer(
                outbox,
                head,
                DeliveryFailureClass::OperatorAction,
                format!("invalid batch ingest acknowledgement: {error}"),
            );
        }
    };
    if acknowledgement.acknowledgements.len() != pending.len() {
        return defer(
            outbox,
            head,
            DeliveryFailureClass::OperatorAction,
            format!(
                "batch acknowledged {} events instead of {}",
                acknowledgement.acknowledgements.len(),
                pending.len()
            ),
        );
    }
    for (index, (acknowledgement, pending)) in acknowledgement
        .acknowledgements
        .iter()
        .zip(&pending)
        .enumerate()
    {
        if acknowledgement.event_id != pending.event.event_id {
            return defer(
                outbox,
                head,
                DeliveryFailureClass::OperatorAction,
                format!(
                    "batch acknowledgement {index} named event {} instead of {}",
                    acknowledgement.event_id, pending.event.event_id
                ),
            );
        }
    }

    let outbox_ids = pending
        .iter()
        .map(|pending| pending.outbox_id)
        .collect::<Vec<_>>();
    outbox.mark_delivered_prefix(&outbox_ids)?;
    Ok(DeliveryOutcome::BatchDelivered {
        first_outbox_id: *outbox_ids
            .first()
            .expect("reconciliation delivery batch is never empty"),
        last_outbox_id: *outbox_ids
            .last()
            .expect("reconciliation delivery batch is never empty"),
        event_count: outbox_ids.len(),
    })
}

fn defer(
    outbox: &Outbox,
    pending: &PendingEvent,
    class: DeliveryFailureClass,
    error: String,
) -> Result<DeliveryOutcome, DeliveryError> {
    let error = truncate(&error, MAX_STORED_ERROR_CHARS);
    let attempts = outbox.mark_failed(pending.outbox_id, &error)?;
    Ok(DeliveryOutcome::Deferred {
        outbox_id: pending.outbox_id,
        attempts,
        class,
        error,
    })
}

async fn unexpected_status_error(
    response: reqwest::Response,
    label: &str,
) -> (DeliveryFailureClass, String) {
    let status = response.status();
    let class = classify_status(status);
    let body = bounded_response_body(response).await;
    let error = format_unexpected_status(label, status, body);
    (class, error)
}

fn classify_status(status: StatusCode) -> DeliveryFailureClass {
    if status.is_server_error()
        || matches!(
            status,
            StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_EARLY | StatusCode::TOO_MANY_REQUESTS
        )
    {
        DeliveryFailureClass::Transient
    } else {
        DeliveryFailureClass::OperatorAction
    }
}

struct ResponseBodyCapture {
    body: String,
    truncated: bool,
    read_error: Option<String>,
}

async fn bounded_response_body(mut response: reqwest::Response) -> ResponseBodyCapture {
    let capture_limit = MAX_ERROR_RESPONSE_BODY_BYTES + 1;
    let mut captured = Vec::with_capacity(capture_limit);
    let mut truncated = false;
    let mut read_error = None;

    while captured.len() < capture_limit {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) => {
                read_error = Some(error.to_string());
                break;
            }
        };
        let remaining = capture_limit - captured.len();
        let retained = chunk.len().min(remaining);
        captured.extend_from_slice(&chunk[..retained]);
        if retained < chunk.len() {
            truncated = true;
            break;
        }
    }

    if captured.len() > MAX_ERROR_RESPONSE_BODY_BYTES {
        captured.truncate(MAX_ERROR_RESPONSE_BODY_BYTES);
        truncated = true;
    }

    ResponseBodyCapture {
        body: String::from_utf8_lossy(&captured).into_owned(),
        truncated,
        read_error,
    }
}

fn format_unexpected_status(
    label: &str,
    status: StatusCode,
    capture: ResponseBodyCapture,
) -> String {
    let mut details = Vec::new();
    if !capture.body.is_empty() {
        let (body, rendering_truncated) =
            debug_body_prefix(&capture.body, MAX_RENDERED_ERROR_BODY_CHARS);
        let mut detail = format!("response body: {body}");
        if capture.truncated {
            detail.push_str(&format!(
                " (truncated at {MAX_ERROR_RESPONSE_BODY_BYTES} bytes)"
            ));
        }
        if rendering_truncated {
            detail.push_str(&format!(
                " (escaped rendering truncated at {MAX_RENDERED_ERROR_BODY_CHARS} characters)"
            ));
        }
        details.push(detail);
    }
    if let Some(error) = capture.read_error {
        details.push(format!(
            "reading response body failed: {}",
            truncate(&error, MAX_RESPONSE_READ_ERROR_CHARS)
        ));
    }

    if details.is_empty() {
        format!("{label} {status}")
    } else {
        format!("{label} {status}; {}", details.join("; "))
    }
}

fn debug_body_prefix(value: &str, max_chars: usize) -> (String, bool) {
    let mut rendered = String::from("\"");
    let mut rendered_chars = 1;
    let mut truncated = false;

    for character in value.chars() {
        let escaped = character.escape_debug().collect::<String>();
        let escaped_chars = escaped.chars().count();
        if rendered_chars + escaped_chars + 1 > max_chars {
            truncated = true;
            break;
        }
        rendered.push_str(&escaped);
        rendered_chars += escaped_chars;
    }
    rendered.push('"');
    (rendered, truncated)
}

fn duration_millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn deterministic_retry_hash(source_id: &SourceId, outbox_id: i64, attempts: u64) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    source_id
        .as_str()
        .as_bytes()
        .iter()
        .chain(&outbox_id.to_le_bytes())
        .chain(&attempts.to_le_bytes())
        .fold(FNV_OFFSET_BASIS, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
        })
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use atlas_model::{Evidence, IngestBatchRequest, MempoolEntryFacts, SourceId, SourceSessionId};
    use axum::body::{Body, Bytes};
    use axum::http::StatusCode as AxumStatusCode;
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;
    use axum::{Json, Router};
    use futures_util::StreamExt;
    use tokio::task::JoinHandle;

    use crate::outbox::AgentIdentity;

    use super::*;

    const TXID: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    fn test_outbox() -> (tempfile::TempDir, Outbox) {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("outbox.db");
        Outbox::migrate(&path).expect("migrate");
        let source_id = SourceId::new("core-a").expect("source");
        let outbox = Outbox::open(&path, source_id.clone()).expect("open");
        outbox
            .enqueue_observation(
                &AgentIdentity::new(
                    source_id,
                    SourceSessionId::new("session-a").expect("session"),
                ),
                100,
                101,
                Evidence::MempoolAdded {
                    txid: TXID.to_owned(),
                },
                Some("mempool"),
                Some(b"wire event"),
            )
            .expect("enqueue");
        (temporary, outbox)
    }

    fn reconciliation_outbox(count: u64) -> (tempfile::TempDir, Outbox) {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("outbox.db");
        Outbox::migrate(&path).expect("migrate");
        let source_id = SourceId::new("core-a").expect("source");
        let identity = AgentIdentity::new(
            source_id.clone(),
            SourceSessionId::new("session-a").expect("session"),
        );
        let outbox = Outbox::open(&path, source_id).expect("open");
        let snapshot = (0..count)
            .map(|value| {
                (
                    format!("{value:064x}"),
                    MempoolEntryFacts {
                        vsize: 141,
                        fee_sats: 1_200,
                        entered_at_ms: 1_721_234_000_000,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            outbox
                .reconcile_rpc_snapshot(&identity, snapshot, 100)
                .expect("reconcile"),
            usize::try_from(count).expect("test count fits usize")
        );
        (temporary, outbox)
    }

    fn pending_batch_ids(outbox: &Outbox) -> Vec<String> {
        match outbox
            .next_delivery()
            .expect("next delivery")
            .expect("pending delivery")
        {
            PendingDelivery::ReconciliationBatch(pending) => pending
                .into_iter()
                .map(|pending| pending.event.event_id)
                .collect(),
            PendingDelivery::Single(pending) => {
                panic!("expected batch, got {}", pending.event.event_id)
            }
        }
    }

    async fn server(app: Router) -> (String, JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address: SocketAddr = listener.local_addr().expect("address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        (format!("http://{address}/api/v1/events"), task)
    }

    #[tokio::test]
    async fn accepted_matching_acknowledgement_deletes_head() {
        async fn ingest(
            Json(event): Json<atlas_model::NormalizedEvent>,
        ) -> impl axum::response::IntoResponse {
            (
                AxumStatusCode::ACCEPTED,
                Json(IngestResponse {
                    event_id: event.event_id,
                    status: IngestStatus::Applied,
                }),
            )
        }

        let (_temporary, outbox) = test_outbox();
        let (endpoint, server) = server(Router::new().route("/api/v1/events", post(ingest))).await;
        let outcome = deliver_next(&outbox, &Client::new(), &endpoint)
            .await
            .expect("delivery");

        assert!(matches!(
            outcome,
            DeliveryOutcome::Delivered {
                status: IngestStatus::Applied,
                ..
            }
        ));
        assert_eq!(outbox.pending_count().expect("count"), 0);
        server.abort();
    }

    #[tokio::test]
    async fn failed_status_keeps_fifo_head() {
        async fn fail() -> AxumStatusCode {
            AxumStatusCode::SERVICE_UNAVAILABLE
        }

        let (_temporary, outbox) = test_outbox();
        let expected_head = outbox.next_pending().expect("next").expect("head");
        let (endpoint, server) = server(Router::new().route("/api/v1/events", post(fail))).await;
        let outcome = deliver_next(&outbox, &Client::new(), &endpoint)
            .await
            .expect("delivery attempt");

        assert!(matches!(
            outcome,
            DeliveryOutcome::Deferred {
                attempts: 1,
                class: DeliveryFailureClass::Transient,
                ..
            }
        ));
        assert_eq!(
            outbox
                .next_pending()
                .expect("next")
                .expect("head")
                .outbox_id,
            expected_head.outbox_id
        );
        server.abort();
    }

    #[tokio::test]
    async fn mismatched_acknowledgement_keeps_fifo_head() {
        async fn wrong_ack() -> impl axum::response::IntoResponse {
            (
                AxumStatusCode::ACCEPTED,
                Json(IngestResponse {
                    event_id: "wrong/session/1".to_owned(),
                    status: IngestStatus::Duplicate,
                }),
            )
        }

        let (_temporary, outbox) = test_outbox();
        let expected_head = outbox.next_pending().expect("next").expect("head");
        let (endpoint, server) =
            server(Router::new().route("/api/v1/events", post(wrong_ack))).await;
        let outcome = deliver_next(&outbox, &Client::new(), &endpoint)
            .await
            .expect("delivery attempt");

        assert!(matches!(
            outcome,
            DeliveryOutcome::Deferred {
                class: DeliveryFailureClass::OperatorAction,
                ..
            }
        ));
        assert_eq!(
            outbox
                .next_pending()
                .expect("next")
                .expect("head")
                .outbox_id,
            expected_head.outbox_id
        );
        server.abort();
    }

    #[tokio::test]
    async fn non_accepted_single_captures_response_body_and_requires_operator_action() {
        async fn fail() -> impl IntoResponse {
            (
                AxumStatusCode::UNPROCESSABLE_ENTITY,
                "configured source does not match the database",
            )
        }

        let (_temporary, outbox) = test_outbox();
        let expected_head = outbox.next_pending().expect("next").expect("head");
        let (endpoint, server) = server(Router::new().route("/api/v1/events", post(fail))).await;

        let outcome = deliver_next(&outbox, &Client::new(), &endpoint)
            .await
            .expect("delivery attempt");
        let DeliveryOutcome::Deferred {
            outbox_id,
            attempts,
            class,
            error,
        } = outcome
        else {
            panic!("expected deferred delivery")
        };
        assert_eq!(outbox_id, expected_head.outbox_id);
        assert_eq!(attempts, 1);
        assert_eq!(class, DeliveryFailureClass::OperatorAction);
        assert!(error.contains("422 Unprocessable Entity"));
        assert!(error.contains("configured source does not match the database"));
        assert_eq!(outbox.pending_count().expect("count"), 1);
        server.abort();
    }

    #[tokio::test]
    async fn non_accepted_batch_captures_only_a_bounded_response_prefix() {
        async fn fail() -> impl IntoResponse {
            (
                AxumStatusCode::SERVICE_UNAVAILABLE,
                "diagnostic".repeat(MAX_ERROR_RESPONSE_BODY_BYTES),
            )
        }

        let (_temporary, outbox) = reconciliation_outbox(3);
        let expected = pending_batch_ids(&outbox);
        let (endpoint, server) =
            server(Router::new().route("/api/v1/events/batch", post(fail))).await;

        let outcome = deliver_next(&outbox, &Client::new(), &endpoint)
            .await
            .expect("delivery attempt");
        let DeliveryOutcome::Deferred {
            attempts,
            class,
            error,
            ..
        } = outcome
        else {
            panic!("expected deferred delivery")
        };
        assert_eq!(attempts, 1);
        assert_eq!(class, DeliveryFailureClass::Transient);
        assert!(error.contains("503 Service Unavailable"));
        assert!(error.contains("diagnostic"));
        assert!(error.contains("truncated at 1024 bytes"));
        assert!(error.chars().count() <= MAX_STORED_ERROR_CHARS);
        assert_eq!(pending_batch_ids(&outbox), expected);
        server.abort();
    }

    #[tokio::test]
    async fn response_body_exactly_at_the_capture_limit_is_not_reported_as_truncated() {
        async fn fail() -> impl IntoResponse {
            (
                AxumStatusCode::BAD_REQUEST,
                "x".repeat(MAX_ERROR_RESPONSE_BODY_BYTES),
            )
        }

        let (_temporary, outbox) = test_outbox();
        let (endpoint, server) = server(Router::new().route("/api/v1/events", post(fail))).await;

        let DeliveryOutcome::Deferred { error, .. } =
            deliver_next(&outbox, &Client::new(), &endpoint)
                .await
                .expect("delivery attempt")
        else {
            panic!("expected deferred delivery")
        };
        assert!(!error.contains("truncated"));
        assert!(error.contains(&"x".repeat(MAX_ERROR_RESPONSE_BODY_BYTES)));
        server.abort();
    }

    #[tokio::test]
    async fn response_body_prefix_survives_a_later_read_failure() {
        async fn fail() -> Response {
            let prefix = futures_util::stream::once(async {
                Ok::<Bytes, std::io::Error>(Bytes::from_static(b"useful diagnostic prefix"))
            });
            let failure = futures_util::stream::once(async {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                Err(std::io::Error::other("injected body failure"))
            });
            let body = Body::from_stream(prefix.chain(failure));
            Response::builder()
                .status(AxumStatusCode::SERVICE_UNAVAILABLE)
                .body(body)
                .expect("response")
        }

        let (_temporary, outbox) = test_outbox();
        let (endpoint, server) = server(Router::new().route("/api/v1/events", post(fail))).await;

        let DeliveryOutcome::Deferred { error, .. } =
            deliver_next(&outbox, &Client::new(), &endpoint)
                .await
                .expect("delivery attempt")
        else {
            panic!("expected deferred delivery")
        };
        assert!(error.contains("useful diagnostic prefix"), "{error}");
        assert!(error.contains("reading response body failed"), "{error}");
        server.abort();
    }

    #[test]
    fn escaped_body_rendering_preserves_its_truncation_markers() {
        let error = format_unexpected_status(
            "unexpected HTTP status",
            StatusCode::SERVICE_UNAVAILABLE,
            ResponseBodyCapture {
                body: "\0".repeat(MAX_ERROR_RESPONSE_BODY_BYTES),
                truncated: true,
                read_error: None,
            },
        );

        assert!(error.contains("truncated at 1024 bytes"));
        assert!(error.contains("escaped rendering truncated at 1400 characters"));
        assert!(error.chars().count() <= MAX_STORED_ERROR_CHARS);
    }

    #[tokio::test]
    async fn matching_batch_acknowledgements_delete_the_exact_prefix() {
        async fn ingest(
            Json(request): Json<IngestBatchRequest>,
        ) -> impl axum::response::IntoResponse {
            let acknowledgements = request
                .events
                .into_iter()
                .map(|event| IngestResponse {
                    event_id: event.event_id,
                    status: IngestStatus::Applied,
                })
                .collect();
            (
                AxumStatusCode::ACCEPTED,
                Json(IngestBatchResponse { acknowledgements }),
            )
        }

        let (_temporary, outbox) = reconciliation_outbox(3);
        let (endpoint, server) =
            server(Router::new().route("/api/v1/events/batch", post(ingest))).await;
        let outcome = deliver_next(&outbox, &Client::new(), &endpoint)
            .await
            .expect("batch delivery");

        assert!(matches!(
            outcome,
            DeliveryOutcome::BatchDelivered { event_count: 3, .. }
        ));
        assert_eq!(outbox.pending_count().expect("count"), 0);
        server.abort();
    }

    #[tokio::test]
    async fn duplicate_batch_acknowledgements_are_successful() {
        async fn duplicate(
            Json(request): Json<IngestBatchRequest>,
        ) -> impl axum::response::IntoResponse {
            let acknowledgements = request
                .events
                .into_iter()
                .map(|event| IngestResponse {
                    event_id: event.event_id,
                    status: IngestStatus::Duplicate,
                })
                .collect();
            (
                AxumStatusCode::ACCEPTED,
                Json(IngestBatchResponse { acknowledgements }),
            )
        }

        let (_temporary, outbox) = reconciliation_outbox(2);
        let (endpoint, server) =
            server(Router::new().route("/api/v1/events/batch", post(duplicate))).await;
        assert!(matches!(
            deliver_next(&outbox, &Client::new(), &endpoint)
                .await
                .expect("duplicate delivery"),
            DeliveryOutcome::BatchDelivered { event_count: 2, .. }
        ));
        assert_eq!(outbox.pending_count().expect("count"), 0);
        server.abort();
    }

    #[tokio::test]
    async fn short_or_reordered_batch_acknowledgements_delete_nothing() {
        async fn short(
            Json(request): Json<IngestBatchRequest>,
        ) -> impl axum::response::IntoResponse {
            let acknowledgements = request
                .events
                .into_iter()
                .take(2)
                .map(|event| IngestResponse {
                    event_id: event.event_id,
                    status: IngestStatus::Applied,
                })
                .collect();
            (
                AxumStatusCode::ACCEPTED,
                Json(IngestBatchResponse { acknowledgements }),
            )
        }

        async fn reordered(
            Json(request): Json<IngestBatchRequest>,
        ) -> impl axum::response::IntoResponse {
            let acknowledgements = request
                .events
                .into_iter()
                .rev()
                .map(|event| IngestResponse {
                    event_id: event.event_id,
                    status: IngestStatus::Applied,
                })
                .collect();
            (
                AxumStatusCode::ACCEPTED,
                Json(IngestBatchResponse { acknowledgements }),
            )
        }

        for route in [post(short), post(reordered)] {
            let (_temporary, outbox) = reconciliation_outbox(3);
            let expected = pending_batch_ids(&outbox);
            let (endpoint, server) =
                server(Router::new().route("/api/v1/events/batch", route)).await;
            assert!(matches!(
                deliver_next(&outbox, &Client::new(), &endpoint)
                    .await
                    .expect("deferred batch"),
                DeliveryOutcome::Deferred { attempts: 1, .. }
            ));
            assert_eq!(outbox.pending_count().expect("count"), 3);
            assert_eq!(pending_batch_ids(&outbox), expected);
            server.abort();
        }
    }

    #[derive(Clone)]
    struct RetryState {
        calls: Arc<AtomicUsize>,
        received_event_ids: Arc<Mutex<Vec<Vec<String>>>>,
    }

    #[tokio::test]
    async fn lost_batch_response_retries_the_identical_prefix_idempotently() {
        async fn ingest(
            axum::extract::State(state): axum::extract::State<RetryState>,
            Json(request): Json<IngestBatchRequest>,
        ) -> axum::response::Response {
            let event_ids = request
                .events
                .iter()
                .map(|event| event.event_id.clone())
                .collect::<Vec<_>>();
            state
                .received_event_ids
                .lock()
                .expect("lock received requests")
                .push(event_ids);
            if state.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return AxumStatusCode::SERVICE_UNAVAILABLE.into_response();
            }

            let acknowledgements = request
                .events
                .into_iter()
                .map(|event| IngestResponse {
                    event_id: event.event_id,
                    status: IngestStatus::Duplicate,
                })
                .collect();
            (
                AxumStatusCode::ACCEPTED,
                Json(IngestBatchResponse { acknowledgements }),
            )
                .into_response()
        }

        let state = RetryState {
            calls: Arc::new(AtomicUsize::new(0)),
            received_event_ids: Arc::new(Mutex::new(Vec::new())),
        };
        let (_temporary, outbox) = reconciliation_outbox(3);
        let (endpoint, server) = server(
            Router::new()
                .route("/api/v1/events/batch", post(ingest))
                .with_state(state.clone()),
        )
        .await;

        assert!(matches!(
            deliver_next(&outbox, &Client::new(), &endpoint)
                .await
                .expect("lost response"),
            DeliveryOutcome::Deferred { attempts: 1, .. }
        ));
        assert_eq!(outbox.pending_count().expect("count after loss"), 3);
        assert!(matches!(
            deliver_next(&outbox, &Client::new(), &endpoint)
                .await
                .expect("idempotent retry"),
            DeliveryOutcome::BatchDelivered { event_count: 3, .. }
        ));
        assert_eq!(outbox.pending_count().expect("count after retry"), 0);
        let received = state
            .received_event_ids
            .lock()
            .expect("lock received requests");
        assert_eq!(received.len(), 2);
        assert_eq!(received[0], received[1]);
        server.abort();
    }

    #[test]
    fn status_classification_separates_retryable_responses_from_operator_faults() {
        for status in [
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_EARLY,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::SERVICE_UNAVAILABLE,
        ] {
            assert_eq!(classify_status(status), DeliveryFailureClass::Transient);
        }
        for status in [
            StatusCode::OK,
            StatusCode::MOVED_PERMANENTLY,
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::NOT_FOUND,
            StatusCode::CONFLICT,
            StatusCode::PAYLOAD_TOO_LARGE,
            StatusCode::UNPROCESSABLE_ENTITY,
        ] {
            assert_eq!(
                classify_status(status),
                DeliveryFailureClass::OperatorAction
            );
        }
    }

    #[test]
    fn retry_policy_rejects_a_maximum_below_the_initial_interval() {
        assert!(matches!(
            DeliveryRetryPolicy::new(
                std::time::Duration::from_millis(1_001),
                std::time::Duration::from_millis(1_000),
            ),
            Err(DeliveryRetryPolicyError { .. })
        ));
    }

    #[test]
    fn retry_policy_rejects_sub_millisecond_intervals() {
        assert!(
            DeliveryRetryPolicy::new(
                std::time::Duration::from_nanos(999_999),
                std::time::Duration::from_millis(1),
            )
            .is_err()
        );
    }

    #[test]
    fn retry_policy_uses_deterministic_capped_equal_jitter() {
        let source_id = SourceId::new("core-a").expect("source");
        let policy = DeliveryRetryPolicy::new(
            std::time::Duration::from_millis(1_000),
            std::time::Duration::from_millis(8_000),
        )
        .expect("policy");

        for (attempts, ceiling_ms) in [(1, 1_000), (2, 2_000), (3, 4_000), (4, 8_000), (64, 8_000)]
        {
            let delay = policy.delay(DeliveryFailureClass::Transient, &source_id, 17, attempts);
            assert!(delay >= std::time::Duration::from_millis(ceiling_ms / 2));
            assert!(delay <= std::time::Duration::from_millis(ceiling_ms));
            assert_eq!(
                delay,
                policy.delay(DeliveryFailureClass::Transient, &source_id, 17, attempts)
            );
        }

        assert_ne!(
            policy.delay(DeliveryFailureClass::Transient, &source_id, 17, 3),
            policy.delay(
                DeliveryFailureClass::Transient,
                &SourceId::new("knots-a").expect("source"),
                17,
                3
            )
        );
        assert_ne!(
            policy.delay(DeliveryFailureClass::Transient, &source_id, 17, 3),
            policy.delay(DeliveryFailureClass::Transient, &source_id, 18, 3)
        );

        let operator_delay = policy.delay(DeliveryFailureClass::OperatorAction, &source_id, 17, 1);
        assert!(operator_delay >= std::time::Duration::from_millis(4_000));
        assert!(operator_delay <= std::time::Duration::from_millis(8_000));
        assert!(operator_delay > policy.delay(DeliveryFailureClass::Transient, &source_id, 17, 1));

        let minimum_policy = DeliveryRetryPolicy::new(
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(1),
        )
        .expect("minimum policy");
        assert_eq!(
            minimum_policy.delay(DeliveryFailureClass::Transient, &source_id, 17, 1),
            std::time::Duration::from_millis(1)
        );
    }
}
