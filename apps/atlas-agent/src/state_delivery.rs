//! Idempotent delivery for the RPC-authoritative SourceReplica protocol.

use atlas_model::{
    CheckpointBegin, CheckpointId, CheckpointProgress, MAX_SOURCE_REPLICA_BODY_BYTES,
    ReplicaCursor, SourceReplicaCommand, SourceReplicaRequest, SourceReplicaResponse,
    StateHeartbeat,
};
use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::watch;

use crate::delivery::DeliveryFailureClass;
use crate::source_replica::{SourceReplica, SourceReplicaAction, SourceReplicaStoreError};

const MAX_RESPONSE_BODY_BYTES: usize = 64 * 1024;
const MAX_RENDERED_ERROR_CHARS: usize = 2_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateActionKind {
    Delta,
    Checkpoint,
    Heartbeat,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateRetryKey {
    pub kind: StateActionKind,
    pub target_revision: u64,
    pub state_observed_at_ms: u64,
}

impl StateRetryKey {
    #[must_use]
    pub fn deterministic_seed(&self) -> i64 {
        // SourceReplica revisions are constrained to exact JSON integers, so
        // every valid revision also fits in SQLite's signed integer range.
        i64::try_from(self.target_revision).expect("validated revision fits i64")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StateDeliveryOutcome {
    Idle,
    Applied {
        kind: StateActionKind,
        active_cursor: ReplicaCursor,
    },
    RecoveryCheckpointRequired {
        code: String,
        active_cursor: Option<ReplicaCursor>,
        staging_checkpoint_id: Option<CheckpointId>,
    },
    Deferred {
        retry_key: StateRetryKey,
        class: DeliveryFailureClass,
        error: String,
    },
    Shutdown,
}

#[derive(Debug, Error)]
pub enum StateDeliveryError {
    #[error("source replica store error: {0}")]
    Store(#[from] SourceReplicaStoreError),
    #[error("source replica worker task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
    #[error("source replica request could not be constructed: {0}")]
    Model(#[from] atlas_model::SourceReplicaError),
    #[error("source replica request serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Debug, Deserialize)]
struct StateErrorResponse {
    error: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    active_cursor: Option<ReplicaCursor>,
    #[serde(default)]
    staging_checkpoint_id: Option<CheckpointId>,
}

enum RequestOutcome {
    Accepted(SourceReplicaResponse),
    RecoveryCheckpointRequired {
        code: String,
        active_cursor: Option<ReplicaCursor>,
        staging_checkpoint_id: Option<CheckpointId>,
    },
    Deferred {
        class: DeliveryFailureClass,
        error: String,
    },
}

/// Delivers the next stable SourceReplica action. A checkpoint is sent as one
/// begin, a resumable sequence of chunks, and one commit. The frozen local
/// payload remains unchanged when this function returns a deferred outcome.
pub async fn deliver_next_state(
    replica: &SourceReplica,
    client: &Client,
    endpoint: &str,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<StateDeliveryOutcome, StateDeliveryError> {
    let store = replica.clone();
    let action = tokio::task::spawn_blocking(move || store.next_action()).await??;
    let Some(action) = action else {
        return Ok(StateDeliveryOutcome::Idle);
    };
    let retry_key = retry_key(&action);

    match action {
        SourceReplicaAction::Delta(delta) => {
            let expected = cursor_for_target(replica, delta.target_revision).await?;
            let outcome = send_command(
                replica,
                client,
                endpoint,
                SourceReplicaCommand::Delta(delta),
            )
            .await?;
            finish_membership_action(
                replica,
                StateActionKind::Delta,
                expected,
                outcome,
                retry_key,
            )
            .await
        }
        SourceReplicaAction::Heartbeat(heartbeat) => {
            let expected = cursor_for_target(replica, heartbeat.revision).await?;
            let outcome = send_command(
                replica,
                client,
                endpoint,
                SourceReplicaCommand::Heartbeat(heartbeat.clone()),
            )
            .await?;
            finish_heartbeat(replica, &heartbeat, expected, outcome, retry_key).await
        }
        SourceReplicaAction::Checkpoint(begin) => {
            deliver_checkpoint(replica, client, endpoint, shutdown, begin, retry_key).await
        }
    }
}

async fn deliver_checkpoint(
    replica: &SourceReplica,
    client: &Client,
    endpoint: &str,
    shutdown: &mut watch::Receiver<bool>,
    begin: CheckpointBegin,
    retry_key: StateRetryKey,
) -> Result<StateDeliveryOutcome, StateDeliveryError> {
    let target = cursor_for_target(replica, begin.target_revision).await?;
    let begin_outcome = send_command(
        replica,
        client,
        endpoint,
        SourceReplicaCommand::CheckpointBegin(begin.clone()),
    )
    .await?;
    let received_chunks = match begin_outcome {
        RequestOutcome::Accepted(response) => {
            if response.active_cursor() == Some(&target) && response.progress().is_none() {
                acknowledge(replica, &target).await?;
                return Ok(StateDeliveryOutcome::Applied {
                    kind: StateActionKind::Checkpoint,
                    active_cursor: target,
                });
            }
            match validate_checkpoint_response(
                &begin,
                &target,
                &response,
                begin.replaces.as_ref(),
                None,
            ) {
                Ok(received_chunks) => received_chunks,
                Err(error) => return Ok(deferred_protocol(retry_key, error)),
            }
        }
        RequestOutcome::RecoveryCheckpointRequired {
            code,
            active_cursor,
            staging_checkpoint_id,
        } => {
            return Ok(StateDeliveryOutcome::RecoveryCheckpointRequired {
                code,
                active_cursor,
                staging_checkpoint_id,
            });
        }
        RequestOutcome::Deferred { class, error } => {
            return Ok(StateDeliveryOutcome::Deferred {
                retry_key,
                class,
                error,
            });
        }
    };

    for chunk_index in received_chunks..begin.expected_chunks {
        if *shutdown.borrow() {
            return Ok(StateDeliveryOutcome::Shutdown);
        }
        let store = replica.clone();
        let checkpoint_id = begin.checkpoint_id.clone();
        let chunk = tokio::task::spawn_blocking(move || {
            store.checkpoint_chunk(&checkpoint_id, chunk_index)
        })
        .await??;
        let outcome = send_command(
            replica,
            client,
            endpoint,
            SourceReplicaCommand::CheckpointChunk(chunk),
        )
        .await?;
        match outcome {
            RequestOutcome::Accepted(response) => {
                let received = match validate_checkpoint_response(
                    &begin,
                    &target,
                    &response,
                    begin.replaces.as_ref(),
                    Some(chunk_index),
                ) {
                    Ok(received) => received,
                    Err(error) => return Ok(deferred_protocol(retry_key, error)),
                };
                if received <= chunk_index {
                    return Ok(deferred_protocol(
                        retry_key,
                        format!(
                            "checkpoint acknowledgement reports {received} chunks after chunk {chunk_index}"
                        ),
                    ));
                }
            }
            RequestOutcome::RecoveryCheckpointRequired {
                code,
                active_cursor,
                staging_checkpoint_id,
            } => {
                return Ok(StateDeliveryOutcome::RecoveryCheckpointRequired {
                    code,
                    active_cursor,
                    staging_checkpoint_id,
                });
            }
            RequestOutcome::Deferred { class, error } => {
                return Ok(StateDeliveryOutcome::Deferred {
                    retry_key,
                    class,
                    error,
                });
            }
        }
    }

    if *shutdown.borrow() {
        return Ok(StateDeliveryOutcome::Shutdown);
    }
    let store = replica.clone();
    let checkpoint_id = begin.checkpoint_id.clone();
    let commit =
        tokio::task::spawn_blocking(move || store.checkpoint_commit(&checkpoint_id)).await??;
    let outcome = send_command(
        replica,
        client,
        endpoint,
        SourceReplicaCommand::CheckpointCommit(commit),
    )
    .await?;
    finish_membership_action(
        replica,
        StateActionKind::Checkpoint,
        target,
        outcome,
        retry_key,
    )
    .await
}

fn validate_checkpoint_response(
    begin: &CheckpointBegin,
    target: &ReplicaCursor,
    response: &SourceReplicaResponse,
    expected_active: Option<&ReplicaCursor>,
    sent_chunk: Option<u32>,
) -> Result<u32, String> {
    response
        .validate()
        .map_err(|error| format!("invalid state acknowledgement: {error}"))?;
    if response.active_cursor() != expected_active {
        return Err(format!(
            "checkpoint acknowledgement active cursor {:?} does not equal replacement cursor {:?}",
            response.active_cursor(),
            expected_active
        ));
    }
    let progress = response
        .progress()
        .ok_or_else(|| "checkpoint acknowledgement omitted staging progress".to_owned())?;
    validate_progress(begin, target, progress)?;
    if let Some(sent_chunk) = sent_chunk
        && progress.received_chunks <= sent_chunk
    {
        return Err(format!(
            "checkpoint acknowledgement did not include chunk {sent_chunk}"
        ));
    }
    Ok(progress.received_chunks)
}

fn validate_progress(
    begin: &CheckpointBegin,
    target: &ReplicaCursor,
    progress: &CheckpointProgress,
) -> Result<(), String> {
    if progress.checkpoint_id != begin.checkpoint_id
        || progress.target_cursor != *target
        || progress.expected_entries != begin.expected_entries
        || progress.expected_chunks != begin.expected_chunks
    {
        return Err("checkpoint progress does not match the frozen declaration".to_owned());
    }
    Ok(())
}

async fn finish_membership_action(
    replica: &SourceReplica,
    kind: StateActionKind,
    expected: ReplicaCursor,
    outcome: RequestOutcome,
    retry_key: StateRetryKey,
) -> Result<StateDeliveryOutcome, StateDeliveryError> {
    match outcome {
        RequestOutcome::Accepted(response) => {
            if let Err(error) = response.validate() {
                return Ok(deferred_protocol(retry_key, error.to_string()));
            }
            if response.active_cursor() != Some(&expected) || response.progress().is_some() {
                return Ok(deferred_protocol(
                    retry_key,
                    format!(
                        "state acknowledgement cursor {:?} does not equal expected {expected:?}",
                        response.active_cursor()
                    ),
                ));
            }
            acknowledge(replica, &expected).await?;
            Ok(StateDeliveryOutcome::Applied {
                kind,
                active_cursor: expected,
            })
        }
        RequestOutcome::RecoveryCheckpointRequired {
            code,
            active_cursor,
            staging_checkpoint_id,
        } => Ok(StateDeliveryOutcome::RecoveryCheckpointRequired {
            code,
            active_cursor,
            staging_checkpoint_id,
        }),
        RequestOutcome::Deferred { class, error } => Ok(StateDeliveryOutcome::Deferred {
            retry_key,
            class,
            error,
        }),
    }
}

async fn finish_heartbeat(
    replica: &SourceReplica,
    heartbeat: &StateHeartbeat,
    expected: ReplicaCursor,
    outcome: RequestOutcome,
    retry_key: StateRetryKey,
) -> Result<StateDeliveryOutcome, StateDeliveryError> {
    match outcome {
        RequestOutcome::Accepted(response) => {
            if let Err(error) = response.validate() {
                return Ok(deferred_protocol(retry_key, error.to_string()));
            }
            if response.active_cursor() != Some(&expected) || response.progress().is_some() {
                return Ok(deferred_protocol(
                    retry_key,
                    "heartbeat acknowledgement did not name the expected active cursor".to_owned(),
                ));
            }
            let store = replica.clone();
            let heartbeat = heartbeat.clone();
            let cursor = expected.clone();
            tokio::task::spawn_blocking(move || store.acknowledge_heartbeat(&heartbeat, &cursor))
                .await??;
            Ok(StateDeliveryOutcome::Applied {
                kind: StateActionKind::Heartbeat,
                active_cursor: expected,
            })
        }
        RequestOutcome::RecoveryCheckpointRequired {
            code,
            active_cursor,
            staging_checkpoint_id,
        } => Ok(StateDeliveryOutcome::RecoveryCheckpointRequired {
            code,
            active_cursor,
            staging_checkpoint_id,
        }),
        RequestOutcome::Deferred { class, error } => Ok(StateDeliveryOutcome::Deferred {
            retry_key,
            class,
            error,
        }),
    }
}

async fn acknowledge(
    replica: &SourceReplica,
    cursor: &ReplicaCursor,
) -> Result<(), StateDeliveryError> {
    let store = replica.clone();
    let cursor = cursor.clone();
    tokio::task::spawn_blocking(move || store.acknowledge(&cursor)).await??;
    Ok(())
}

async fn cursor_for_target(
    replica: &SourceReplica,
    target_revision: u64,
) -> Result<ReplicaCursor, StateDeliveryError> {
    let store = replica.clone();
    let status = tokio::task::spawn_blocking(move || store.status()).await??;
    if !status.has_observed_snapshot || target_revision > status.local_revision {
        return Err(SourceReplicaStoreError::StoredInvariant(
            "frozen action target is newer than local state",
        )
        .into());
    }
    Ok(ReplicaCursor {
        epoch_id: status.epoch_id,
        revision: target_revision,
    })
}

async fn send_command(
    replica: &SourceReplica,
    client: &Client,
    endpoint: &str,
    command: SourceReplicaCommand,
) -> Result<RequestOutcome, StateDeliveryError> {
    let store = replica.clone();
    let status = tokio::task::spawn_blocking(move || store.status()).await??;
    let request = SourceReplicaRequest::new(status.source_id, status.epoch_id, command)?;
    let body = serde_json::to_vec(&request)?;
    if body.len() > MAX_SOURCE_REPLICA_BODY_BYTES {
        return Ok(RequestOutcome::Deferred {
            class: DeliveryFailureClass::OperatorAction,
            error: format!(
                "encoded state request is {} bytes; maximum is {MAX_SOURCE_REPLICA_BODY_BYTES}",
                body.len()
            ),
        });
    }

    let response = match client
        .post(endpoint)
        .header(CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return Ok(RequestOutcome::Deferred {
                class: DeliveryFailureClass::Transient,
                error: format!("state request failed: {error}"),
            });
        }
    };
    let status = response.status();
    let body = match bounded_body(response).await {
        Ok(body) => body,
        Err(error) => {
            return Ok(RequestOutcome::Deferred {
                class: DeliveryFailureClass::Transient,
                error,
            });
        }
    };

    if status == StatusCode::ACCEPTED {
        return match serde_json::from_slice::<SourceReplicaResponse>(&body) {
            Ok(response) => Ok(RequestOutcome::Accepted(response)),
            Err(error) => Ok(RequestOutcome::Deferred {
                class: DeliveryFailureClass::OperatorAction,
                error: format!("invalid state acknowledgement: {error}"),
            }),
        };
    }

    let parsed = serde_json::from_slice::<StateErrorResponse>(&body).ok();
    let error = parsed
        .as_ref()
        .map_or_else(|| render_error_body(&body), |parsed| parsed.error.clone());
    let code = parsed.as_ref().and_then(|parsed| parsed.code.clone());
    let active_cursor = parsed
        .as_ref()
        .and_then(|parsed| parsed.active_cursor.clone());
    let staging_checkpoint_id = parsed.and_then(|parsed| parsed.staging_checkpoint_id);
    if let Some(cursor) = &active_cursor
        && let Err(error) = cursor.validate()
    {
        return Ok(RequestOutcome::Deferred {
            class: DeliveryFailureClass::OperatorAction,
            error: format!("invalid active cursor in state error response: {error}"),
        });
    }
    if status == StatusCode::CONFLICT
        && matches!(
            code.as_deref(),
            Some(
                "cursor_mismatch" | "epoch_conflict" | "checkpoint_conflict" | "conflicting_replay"
            )
        )
    {
        return Ok(RequestOutcome::RecoveryCheckpointRequired {
            code: code.expect("matched code is present"),
            active_cursor,
            staging_checkpoint_id,
        });
    }

    let class = if status.is_server_error()
        || matches!(
            status,
            StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_EARLY | StatusCode::TOO_MANY_REQUESTS
        ) {
        DeliveryFailureClass::Transient
    } else {
        DeliveryFailureClass::OperatorAction
    };
    Ok(RequestOutcome::Deferred {
        class,
        error: format!("state endpoint returned {status}: {error}"),
    })
}

async fn bounded_body(mut response: reqwest::Response) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("reading state response failed: {error}"))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BODY_BYTES {
            return Err(format!(
                "state response exceeded {MAX_RESPONSE_BODY_BYTES} bytes"
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn render_error_body(body: &[u8]) -> String {
    let rendered = String::from_utf8_lossy(body);
    rendered.chars().take(MAX_RENDERED_ERROR_CHARS).collect()
}

fn retry_key(action: &SourceReplicaAction) -> StateRetryKey {
    match action {
        SourceReplicaAction::Delta(delta) => StateRetryKey {
            kind: StateActionKind::Delta,
            target_revision: delta.target_revision,
            state_observed_at_ms: delta.state_observed_at_ms,
        },
        SourceReplicaAction::Checkpoint(begin) => StateRetryKey {
            kind: StateActionKind::Checkpoint,
            target_revision: begin.target_revision,
            state_observed_at_ms: begin.state_observed_at_ms,
        },
        SourceReplicaAction::Heartbeat(heartbeat) => StateRetryKey {
            kind: StateActionKind::Heartbeat,
            target_revision: heartbeat.revision,
            state_observed_at_ms: heartbeat.state_observed_at_ms,
        },
    }
}

fn deferred_protocol(retry_key: StateRetryKey, error: impl Into<String>) -> StateDeliveryOutcome {
    StateDeliveryOutcome::Deferred {
        retry_key,
        class: DeliveryFailureClass::OperatorAction,
        error: error.into(),
    }
}
