use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::model::{
    MempoolSnapshot, ModelError, SourceAvailability, SourceSnapshotResponse, SourceSummary,
    SourcesResponse, validate_source_id, validate_source_label,
};
use crate::rpc::{RpcClient, RpcError};

#[derive(Debug)]
pub struct SourceRuntime {
    source_id: String,
    source_label: String,
    poll_interval: Duration,
    state: RwLock<RuntimeState>,
}

#[derive(Debug, Default)]
struct RuntimeState {
    last_poll_started_at_ms: Option<u64>,
    latest: Option<Arc<MempoolSnapshot>>,
    last_error: Option<String>,
    cached_response: Option<Bytes>,
}

impl SourceRuntime {
    pub fn new(
        source_id: String,
        source_label: String,
        poll_interval: Duration,
    ) -> Result<Self, RuntimeError> {
        validate_source_id(&source_id)?;
        validate_source_label(&source_label)?;
        if poll_interval.is_zero() {
            return Err(RuntimeError::ZeroPollInterval);
        }
        Ok(Self {
            source_id,
            source_label,
            poll_interval,
            state: RwLock::new(RuntimeState::default()),
        })
    }

    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    pub fn source_label(&self) -> &str {
        &self.source_label
    }

    pub async fn run(self: Arc<Self>, rpc: RpcClient) {
        loop {
            if let Err(error) = self.poll_once(&rpc).await {
                warn!(
                    source_id = %self.source_id,
                    error = %error,
                    "mempool snapshot poll failed; retaining the last good snapshot"
                );
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    pub async fn poll_once(&self, rpc: &RpcClient) -> Result<(), RuntimeError> {
        let started_at_ms = system_now_ms()?;
        self.record_poll_started(started_at_ms).await;
        match rpc
            .get_mempool_snapshot(&self.source_id, &self.source_label)
            .await
        {
            Ok(snapshot) => {
                let transaction_count = snapshot.transaction_count;
                let observed_at_ms = snapshot.observed_at_ms;
                self.record_success(snapshot).await?;
                info!(
                    source_id = %self.source_id,
                    transaction_count,
                    observed_at_ms,
                    "published mempool snapshot"
                );
                Ok(())
            }
            Err(error) => {
                self.record_failure(error.public_message().to_owned())
                    .await?;
                Err(RuntimeError::Rpc(error))
            }
        }
    }

    pub async fn summary(&self) -> SourceSummary {
        let state = self.state.read().await;
        summary_from_state(self, &state)
    }

    pub async fn snapshot_response(&self) -> SourceSnapshotResponse {
        let state = self.state.read().await;
        SourceSnapshotResponse {
            source: summary_from_state(self, &state),
            snapshot: state.latest.clone(),
        }
    }

    pub async fn snapshot_response_bytes(&self) -> Result<Bytes, RuntimeError> {
        let state = self.state.read().await;
        if let Some(response) = &state.cached_response {
            return Ok(response.clone());
        }
        let response = SourceSnapshotResponse {
            source: summary_from_state(self, &state),
            snapshot: state.latest.clone(),
        };
        drop(state);
        encode_response(response).await
    }

    pub(crate) async fn record_poll_started(&self, started_at_ms: u64) {
        self.state.write().await.last_poll_started_at_ms = Some(started_at_ms);
    }

    pub(crate) async fn record_success(
        &self,
        snapshot: MempoolSnapshot,
    ) -> Result<(), RuntimeError> {
        if snapshot.source_id != self.source_id {
            return Err(RuntimeError::SnapshotSourceMismatch {
                expected: self.source_id.clone(),
                actual: snapshot.source_id,
            });
        }

        let latest = Arc::new(snapshot);
        let last_poll_started_at_ms = self.state.read().await.last_poll_started_at_ms;
        let response = SourceSnapshotResponse {
            source: summary_from_parts(self, last_poll_started_at_ms, Some(&latest), None),
            snapshot: Some(Arc::clone(&latest)),
        };
        let cached_response = encode_response(response).await?;

        let mut state = self.state.write().await;
        state.latest = Some(latest);
        state.last_error = None;
        state.cached_response = Some(cached_response);
        Ok(())
    }

    pub(crate) async fn record_failure(&self, error: String) -> Result<(), RuntimeError> {
        let (last_poll_started_at_ms, latest) = {
            let state = self.state.read().await;
            (state.last_poll_started_at_ms, state.latest.clone())
        };
        let response = SourceSnapshotResponse {
            source: summary_from_parts(
                self,
                last_poll_started_at_ms,
                latest.as_ref(),
                Some(&error),
            ),
            snapshot: latest,
        };
        let cached_response = encode_response(response).await?;

        let mut state = self.state.write().await;
        state.last_error = Some(error);
        state.cached_response = Some(cached_response);
        Ok(())
    }
}

fn summary_from_state(runtime: &SourceRuntime, state: &RuntimeState) -> SourceSummary {
    summary_from_parts(
        runtime,
        state.last_poll_started_at_ms,
        state.latest.as_ref(),
        state.last_error.as_deref(),
    )
}

fn summary_from_parts(
    runtime: &SourceRuntime,
    last_poll_started_at_ms: Option<u64>,
    latest: Option<&Arc<MempoolSnapshot>>,
    last_error: Option<&str>,
) -> SourceSummary {
    let availability = match (latest, last_error) {
        (None, None) => SourceAvailability::Waiting,
        (None, Some(_)) => SourceAvailability::Error,
        (Some(_), None) => SourceAvailability::Ready,
        (Some(_), Some(_)) => SourceAvailability::Stale,
    };
    SourceSummary {
        source_id: runtime.source_id.clone(),
        source_label: runtime.source_label.clone(),
        availability,
        poll_interval_seconds: runtime.poll_interval.as_secs(),
        last_poll_started_at_ms,
        snapshot_observed_at_ms: latest.map(|snapshot| snapshot.observed_at_ms),
        chain_tip: latest.map(|snapshot| snapshot.chain_tip.clone()),
        transaction_count: latest.map(|snapshot| snapshot.transaction_count),
        total_vsize: latest.map(|snapshot| snapshot.total_vsize),
        last_error: last_error.map(str::to_owned),
    }
}

async fn encode_response(response: SourceSnapshotResponse) -> Result<Bytes, RuntimeError> {
    let encoded = tokio::task::spawn_blocking(move || serde_json::to_vec(&response)).await??;
    Ok(Bytes::from(encoded))
}

#[derive(Clone, Debug)]
pub struct SourceRegistry {
    sources: Arc<BTreeMap<String, Arc<SourceRuntime>>>,
}

impl SourceRegistry {
    pub fn new(sources: Vec<Arc<SourceRuntime>>) -> Result<Self, RuntimeError> {
        let mut indexed = BTreeMap::new();
        for source in sources {
            let source_id = source.source_id().to_owned();
            if indexed.insert(source_id.clone(), source).is_some() {
                return Err(RuntimeError::DuplicateSource(source_id));
            }
        }
        if indexed.is_empty() {
            return Err(RuntimeError::NoSources);
        }
        Ok(Self {
            sources: Arc::new(indexed),
        })
    }

    pub fn get(&self, source_id: &str) -> Option<Arc<SourceRuntime>> {
        self.sources.get(source_id).cloned()
    }

    pub async fn sources_response(&self) -> SourcesResponse {
        let mut sources = Vec::with_capacity(self.sources.len());
        for source in self.sources.values() {
            sources.push(source.summary().await);
        }
        SourcesResponse { sources }
    }

    pub async fn is_ready(&self) -> bool {
        for source in self.sources.values() {
            if source.summary().await.snapshot_observed_at_ms.is_some() {
                return true;
            }
        }
        false
    }
}

fn system_now_ms() -> Result<u64, RuntimeError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RuntimeError::InvalidSystemClock)?;
    u64::try_from(elapsed.as_millis()).map_err(|_| RuntimeError::SystemTimeOverflow)
}

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("poll interval must be greater than zero")]
    ZeroPollInterval,
    #[error("at least one source is required")]
    NoSources,
    #[error("source {0:?} is configured more than once")]
    DuplicateSource(String),
    #[error("snapshot source {actual:?} does not match runtime source {expected:?}")]
    SnapshotSourceMismatch { expected: String, actual: String },
    #[error("snapshot response worker failed: {0}")]
    SnapshotResponseTask(#[from] tokio::task::JoinError),
    #[error("snapshot response could not be encoded: {0}")]
    SnapshotResponseEncoding(#[from] serde_json::Error),
    #[error("system clock is before the Unix epoch")]
    InvalidSystemClock,
    #[error("system time cannot be represented in milliseconds")]
    SystemTimeOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ChainTip, MempoolEntry};

    fn runtime() -> SourceRuntime {
        SourceRuntime::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            Duration::from_secs(60),
        )
        .expect("runtime")
    }

    fn snapshot(observed_at_ms: u64) -> MempoolSnapshot {
        MempoolSnapshot::new(
            "core".to_owned(),
            "Bitcoin Core".to_owned(),
            observed_at_ms,
            ChainTip {
                height: 900_000,
                hash: "00".repeat(32),
            },
            vec![
                MempoolEntry::new(
                    "00".repeat(32),
                    141,
                    1_200,
                    observed_at_ms.saturating_sub(1_000),
                )
                .expect("entry"),
            ],
        )
        .expect("snapshot")
    }

    #[tokio::test]
    async fn successful_snapshot_replaces_current_state() {
        let runtime = runtime();
        assert_eq!(
            runtime.summary().await.availability,
            SourceAvailability::Waiting
        );

        runtime.record_poll_started(10).await;
        runtime
            .record_success(snapshot(20))
            .await
            .expect("record snapshot");
        let response = runtime.snapshot_response().await;

        assert_eq!(response.source.availability, SourceAvailability::Ready);
        assert_eq!(response.source.last_poll_started_at_ms, Some(10));
        assert_eq!(response.source.snapshot_observed_at_ms, Some(20));
        assert_eq!(
            response
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.transaction_count),
            Some(1)
        );
    }

    #[tokio::test]
    async fn failed_poll_retains_last_good_snapshot_as_stale() {
        let runtime = runtime();
        runtime
            .record_success(snapshot(20))
            .await
            .expect("record snapshot");
        let before = runtime
            .snapshot_response()
            .await
            .snapshot
            .expect("snapshot");

        runtime.record_poll_started(30).await;
        runtime
            .record_failure("node unavailable".to_owned())
            .await
            .expect("record failure");
        let response = runtime.snapshot_response().await;
        let after = response.snapshot.expect("retained snapshot");

        assert_eq!(response.source.availability, SourceAvailability::Stale);
        assert_eq!(response.source.snapshot_observed_at_ms, Some(20));
        assert_eq!(
            response.source.last_error.as_deref(),
            Some("node unavailable")
        );
        assert!(Arc::ptr_eq(&before, &after));
    }

    #[tokio::test]
    async fn failure_before_first_snapshot_is_an_error() {
        let runtime = runtime();
        runtime
            .record_failure("authentication failed".to_owned())
            .await
            .expect("record failure");

        let response = runtime.snapshot_response().await;
        assert_eq!(response.source.availability, SourceAvailability::Error);
        assert!(response.snapshot.is_none());
    }

    #[tokio::test]
    async fn registry_keeps_sources_independent() {
        let core = Arc::new(runtime());
        let knots = Arc::new(
            SourceRuntime::new(
                "knots".to_owned(),
                "Bitcoin Knots".to_owned(),
                Duration::from_secs(60),
            )
            .expect("runtime"),
        );
        let registry =
            SourceRegistry::new(vec![Arc::clone(&knots), Arc::clone(&core)]).expect("registry");

        assert_eq!(
            registry
                .sources_response()
                .await
                .sources
                .iter()
                .map(|source| source.source_id.as_str())
                .collect::<Vec<_>>(),
            vec!["core", "knots"]
        );
        assert!(registry.get("missing").is_none());
        assert!(!registry.is_ready().await);

        core.record_success(snapshot(20))
            .await
            .expect("record snapshot");
        assert!(registry.is_ready().await);
    }

    #[tokio::test]
    async fn encoded_snapshot_response_is_shared_between_requests() {
        let runtime = runtime();
        runtime
            .record_success(snapshot(20))
            .await
            .expect("record snapshot");

        let first = runtime
            .snapshot_response_bytes()
            .await
            .expect("first response");
        let second = runtime
            .snapshot_response_bytes()
            .await
            .expect("second response");

        assert_eq!(first.as_ptr(), second.as_ptr());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&first).expect("JSON response")["snapshot"]
                ["transaction_count"],
            1
        );
    }
}
