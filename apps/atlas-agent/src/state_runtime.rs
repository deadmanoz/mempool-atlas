//! RPC-only SourceReplica runtime.
//!
//! This is the production agent path while bounded evidence capture is still
//! deferred. RPC observation and state delivery are independent loops sharing
//! only the bounded SourceReplica database.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, anyhow};
use reqwest::Client as HttpClient;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{debug, info, warn};

use crate::delivery::DeliveryRetryPolicy;
use crate::rpc::RpcClient;
use crate::source_replica::{ObserveRpcOutcome, SourceReplica, SourceReplicaStoreError};
use crate::state_delivery::{
    StateDeliveryError, StateDeliveryOutcome, StateRetryKey, deliver_next_state,
};

const IDLE_DELIVERY_POLL: Duration = Duration::from_millis(250);

pub struct RpcStateConfig {
    pub url: String,
    pub username: String,
    pub password: String,
    pub poll_interval: Duration,
}

pub struct StateRuntimeConfig {
    pub state_endpoint: String,
    pub delivery_retry: DeliveryRetryPolicy,
    pub rpc: RpcStateConfig,
}

pub async fn run(config: StateRuntimeConfig, replica: SourceReplica) -> anyhow::Result<()> {
    info!(
        source_id = %replica.source_id(),
        "Atlas state-only agent starting"
    );
    let http = HttpClient::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("building Atlas state HTTP client")?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut tasks = JoinSet::new();

    tasks.spawn(delivery_loop(
        replica.clone(),
        http,
        config.state_endpoint,
        config.delivery_retry,
        shutdown_rx.clone(),
    ));
    tasks.spawn(rpc_loop(replica, config.rpc, shutdown_rx));

    let outcome = tokio::select! {
        signal = tokio::signal::ctrl_c() => signal
            .context("waiting for shutdown signal")
            .inspect(|()| info!("shutdown signal received")),
        task = tasks.join_next() => task_outcome(task),
    };

    let _ = shutdown_tx.send(true);
    while let Some(task) = tasks.join_next().await {
        match task {
            Ok(Ok(())) => {}
            Ok(Err(error)) => warn!(%error, "state-only agent task failed during shutdown"),
            Err(error) => warn!(%error, "state-only agent task panicked during shutdown"),
        }
    }
    outcome
}

async fn delivery_loop(
    replica: SourceReplica,
    client: HttpClient,
    endpoint: String,
    retry_policy: DeliveryRetryPolicy,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut previous_retry: Option<StateRetryKey> = None;
    let mut attempts = 0_u64;

    loop {
        let outcome = match deliver_next_state(&replica, &client, &endpoint, &mut shutdown).await {
            Ok(outcome) => outcome,
            Err(error) if state_delivery_contention(&error) => {
                let delay = retry_policy.initial();
                warn!(
                    %error,
                    delay_ms = delay.as_millis(),
                    "source state database is busy; retrying delivery"
                );
                if wait_or_shutdown(delay, &mut shutdown).await {
                    return Ok(());
                }
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let wait = match outcome {
            StateDeliveryOutcome::Idle => {
                previous_retry = None;
                attempts = 0;
                IDLE_DELIVERY_POLL
            }
            StateDeliveryOutcome::Applied {
                kind,
                active_cursor,
            } => {
                previous_retry = None;
                attempts = 0;
                debug!(?kind, epoch_id = %active_cursor.epoch_id, revision = active_cursor.revision, "source state delivered");
                continue;
            }
            StateDeliveryOutcome::RecoveryCheckpointRequired {
                code,
                active_cursor,
                staging_checkpoint_id,
            } => {
                let store = replica.clone();
                let replacement = active_cursor.clone();
                let supersedes = staging_checkpoint_id.clone();
                let recovery = tokio::task::spawn_blocking(move || {
                    store.require_checkpoint_against(replacement.as_ref(), supersedes.as_ref())
                })
                .await?;
                match recovery {
                    Ok(()) => {
                        previous_retry = None;
                        attempts = 0;
                        warn!(
                            %code,
                            ?active_cursor,
                            ?staging_checkpoint_id,
                            "server cursor requires a replacement checkpoint"
                        );
                        retry_policy.initial()
                    }
                    Err(error) if error.is_retryable_contention() => {
                        let delay = retry_policy.initial();
                        warn!(
                            %error,
                            delay_ms = delay.as_millis(),
                            "source state database is busy; retrying checkpoint recovery"
                        );
                        delay
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            StateDeliveryOutcome::Deferred {
                retry_key,
                class,
                error,
            } => {
                attempts = if previous_retry.as_ref() == Some(&retry_key) {
                    attempts.saturating_add(1)
                } else {
                    1
                };
                let seed = retry_key.deterministic_seed();
                let delay = retry_policy.delay(class, replica.source_id(), seed, attempts);
                warn!(
                    ?retry_key,
                    attempts,
                    ?class,
                    delay_ms = delay.as_millis(),
                    %error,
                    "source state delivery deferred"
                );
                previous_retry = Some(retry_key);
                delay
            }
            StateDeliveryOutcome::Shutdown => return Ok(()),
        };

        if wait_or_shutdown(wait, &mut shutdown).await {
            return Ok(());
        }
    }
}

async fn rpc_loop(
    replica: SourceReplica,
    config: RpcStateConfig,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let max_mempool_entries = replica.limits().max_membership_entries;
    loop {
        match RpcClient::connect(
            &config.url,
            config.username.clone(),
            config.password.clone(),
            max_mempool_entries,
        )
        .await
        {
            Ok((rpc, snapshot)) => {
                match observe_snapshot(&replica, snapshot, unix_time_ms()?).await {
                    Ok(outcome) => {
                        log_observation(outcome);
                        info!("Bitcoin RPC SourceReplica ready");
                        return poll_rpc(replica, rpc, config.poll_interval, shutdown).await;
                    }
                    Err(error) if source_replica_contention(&error) => {
                        warn!(
                            %error,
                            "source state database is busy; initial RPC snapshot retained by the node and will be fetched again"
                        );
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => warn!(%error, "Bitcoin RPC unavailable; retrying"),
        }

        if wait_or_shutdown(config.poll_interval, &mut shutdown).await {
            return Ok(());
        }
    }
}

async fn poll_rpc(
    replica: SourceReplica,
    rpc: RpcClient,
    poll_interval: Duration,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut ticker = interval(poll_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    ticker.tick().await;

    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
            _ = ticker.tick() => {
                match rpc.get_mempool_snapshot().await {
                    Ok(snapshot) => {
                        match observe_snapshot(&replica, snapshot, unix_time_ms()?).await {
                            Ok(outcome) => log_observation(outcome),
                            Err(error) if source_replica_contention(&error) => {
                                warn!(
                                    %error,
                                    "source state database is busy; current replica retained and observation will retry on the next poll"
                                );
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    Err(error) => {
                        warn!(%error, "RPC mempool observation failed; current replica retained");
                    }
                }
            }
        }
    }
}

async fn observe_snapshot(
    replica: &SourceReplica,
    snapshot: std::collections::BTreeMap<String, atlas_model::MempoolEntryFacts>,
    completed_at_ms: u64,
) -> anyhow::Result<ObserveRpcOutcome> {
    let store = replica.clone();
    tokio::task::spawn_blocking(move || store.observe_rpc_snapshot(&snapshot, completed_at_ms))
        .await
        .context("joining SourceReplica observation task")?
        .context("persisting RPC SourceReplica observation")
}

fn log_observation(outcome: ObserveRpcOutcome) {
    match outcome {
        ObserveRpcOutcome::Baseline {
            revision,
            entry_count,
        } => info!(revision, entry_count, "RPC state baseline persisted"),
        ObserveRpcOutcome::Changed {
            revision,
            mutation_count,
            checkpoint_required,
        } => info!(
            revision,
            mutation_count, checkpoint_required, "RPC state revision persisted"
        ),
        ObserveRpcOutcome::Unchanged { revision } => {
            debug!(revision, "RPC state unchanged; freshness persisted")
        }
    }
}

fn state_delivery_contention(error: &StateDeliveryError) -> bool {
    matches!(
        error,
        StateDeliveryError::Store(error) if error.is_retryable_contention()
    )
}

fn source_replica_contention(error: &anyhow::Error) -> bool {
    error.chain().any(|source| {
        source
            .downcast_ref::<SourceReplicaStoreError>()
            .is_some_and(SourceReplicaStoreError::is_retryable_contention)
    })
}

async fn wait_or_shutdown(delay: Duration, shutdown: &mut watch::Receiver<bool>) -> bool {
    if *shutdown.borrow() {
        return true;
    }
    tokio::select! {
        changed = shutdown.changed() => changed.is_err() || *shutdown.borrow(),
        () = tokio::time::sleep(delay) => false,
    }
}

fn task_outcome(
    task: Option<Result<anyhow::Result<()>, tokio::task::JoinError>>,
) -> anyhow::Result<()> {
    match task {
        Some(Ok(Ok(()))) => Err(anyhow!("state-only agent task exited unexpectedly")),
        Some(Ok(Err(error))) => Err(error),
        Some(Err(error)) => Err(error).context("state-only agent task panicked"),
        None => Err(anyhow!("all state-only agent tasks exited unexpectedly")),
    }
}

fn unix_time_ms() -> anyhow::Result<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_millis();
    u64::try_from(millis).context("current Unix timestamp does not fit u64")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::mpsc;

    use atlas_model::SourceId;
    use rusqlite::{Connection, TransactionBehavior};
    use tokio::time::timeout;

    use super::*;
    use crate::schema;
    use crate::source_replica::SourceReplicaLimits;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_writer_contention_defers_delivery_and_rpc_persistence() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let database = temporary.path().join("agent.db");
        schema::migrate(&database).expect("migrate agent database");
        let replica = SourceReplica::open_with_test_busy_timeout(
            &database,
            SourceId::new("contention-core").expect("source"),
            SourceReplicaLimits::default(),
            Duration::from_millis(20),
        )
        .expect("open source replica");
        replica
            .observe_rpc_snapshot(&BTreeMap::new(), 1)
            .expect("persist empty baseline");

        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let blocked_database = database.clone();
        let blocker = std::thread::spawn(move || {
            let mut connection = Connection::open(blocked_database).expect("open blocking writer");
            connection
                .busy_timeout(Duration::from_secs(1))
                .expect("configure blocking writer");
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .expect("acquire blocking writer transaction");
            locked_tx.send(()).expect("announce blocking writer");
            release_rx.recv().expect("wait to release blocking writer");
            transaction.commit().expect("release blocking writer");
        });
        locked_rx.recv().expect("blocking writer is ready");

        let store = replica.clone();
        let contention = tokio::task::spawn_blocking(move || store.next_action())
            .await
            .expect("join blocked state action")
            .expect_err("state action should encounter the blocking writer");
        assert!(contention.is_retryable_contention());

        let observation_error = observe_snapshot(&replica, BTreeMap::new(), 2)
            .await
            .expect_err("RPC observation should encounter the blocking writer");
        assert!(source_replica_contention(&observation_error));

        let client = HttpClient::builder()
            .timeout(Duration::from_millis(50))
            .build()
            .expect("HTTP client");
        let retry_policy =
            DeliveryRetryPolicy::new(Duration::from_millis(10), Duration::from_millis(20))
                .expect("retry policy");
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let delivery = tokio::spawn(delivery_loop(
            replica.clone(),
            client,
            "http://127.0.0.1:1/api/v1/state".to_owned(),
            retry_policy,
            shutdown_rx,
        ));

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !delivery.is_finished(),
            "retryable SQLite contention must not terminate the delivery loop"
        );

        shutdown_tx.send(true).expect("request delivery shutdown");
        release_tx.send(()).expect("release blocking writer");
        blocker.join().expect("join blocking writer");
        let result = timeout(Duration::from_secs(1), delivery)
            .await
            .expect("delivery loop shuts down promptly")
            .expect("join delivery loop");
        result.expect("delivery loop exits cleanly after contention");

        let status = replica.status().expect("source replica status");
        assert!(status.has_observed_snapshot);
        assert_eq!(status.local_revision, 1);
        assert_eq!(status.state_observed_at_ms, Some(1));
    }
}
