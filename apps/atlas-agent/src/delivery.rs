//! Strict FIFO delivery from the node-local outbox to Atlas ingest.

use atlas_model::{
    IngestBatchRequest, IngestBatchResponse, IngestResponse, IngestStatus,
    MAX_INGEST_BATCH_BODY_BYTES, NormalizedEvent,
};
use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, StatusCode};
use thiserror::Error;

use crate::outbox::{Outbox, OutboxError, PendingDelivery, PendingEvent};

const MAX_STORED_ERROR_CHARS: usize = 2_000;

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
        Err(error) => return defer(outbox, &pending, format!("request failed: {error}")),
    };

    if response.status() != StatusCode::ACCEPTED {
        return defer(
            outbox,
            &pending,
            format!("unexpected HTTP status {}", response.status()),
        );
    }

    let acknowledgement = match response.json::<IngestResponse>().await {
        Ok(acknowledgement) => acknowledgement,
        Err(error) => {
            return defer(
                outbox,
                &pending,
                format!("invalid ingest acknowledgement: {error}"),
            );
        }
    };
    if acknowledgement.event_id != pending.event.event_id {
        return defer(
            outbox,
            &pending,
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
            return defer(outbox, head, format!("batch serialization failed: {error}"));
        }
    };
    if body.len() > MAX_INGEST_BATCH_BODY_BYTES {
        return defer(
            outbox,
            head,
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
        Err(error) => return defer(outbox, head, format!("batch request failed: {error}")),
    };

    if response.status() != StatusCode::ACCEPTED {
        return defer(
            outbox,
            head,
            format!("unexpected batch HTTP status {}", response.status()),
        );
    }

    let acknowledgement = match response.json::<IngestBatchResponse>().await {
        Ok(acknowledgement) => acknowledgement,
        Err(error) => {
            return defer(
                outbox,
                head,
                format!("invalid batch ingest acknowledgement: {error}"),
            );
        }
    };
    if acknowledgement.acknowledgements.len() != pending.len() {
        return defer(
            outbox,
            head,
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
    error: String,
) -> Result<DeliveryOutcome, DeliveryError> {
    let error = truncate(&error, MAX_STORED_ERROR_CHARS);
    let attempts = outbox.mark_failed(pending.outbox_id, &error)?;
    Ok(DeliveryOutcome::Deferred {
        outbox_id: pending.outbox_id,
        attempts,
        error,
    })
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use atlas_model::{Evidence, IngestBatchRequest, SourceId, SourceSessionId};
    use axum::http::StatusCode as AxumStatusCode;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::{Json, Router};
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
            .map(|value| format!("{value:064x}"))
            .collect::<BTreeSet<_>>();
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
            DeliveryOutcome::Deferred { attempts: 1, .. }
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

        assert!(matches!(outcome, DeliveryOutcome::Deferred { .. }));
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
}
