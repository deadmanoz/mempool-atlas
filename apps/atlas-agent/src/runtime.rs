//! Long-running node-local capture, reconciliation, and delivery loops.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, anyhow, bail};
use async_nats::{Client as NatsClient, ConnectOptions, Event as NatsEvent, Message, Subscriber};
use atlas_model::{CaptureGapCertainty, Evidence, MempoolEntryFacts};
use futures_util::StreamExt;
use reqwest::Client as HttpClient;
use tokio::sync::{Mutex, OwnedMutexGuard, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{debug, info, warn};

use crate::delivery::{DeliveryOutcome, deliver_next};
use crate::outbox::{AgentIdentity, Outbox};
use crate::peer_observer::{MEMPOOL_SUBJECT, NETMSG_SUBJECT, observe_payload};
use crate::rpc::RpcClient;

const IDLE_DELIVERY_POLL: Duration = Duration::from_millis(250);
const CAPTURE_ACCOUNTING_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
pub enum P2pPolicy {
    Inbound,
    All,
}

impl P2pPolicy {
    fn retains(self, evidence: &atlas_model::Evidence) -> bool {
        match (self, evidence) {
            (
                Self::Inbound,
                atlas_model::Evidence::P2pTransaction {
                    inbound: Some(true),
                    ..
                },
            ) => true,
            (Self::Inbound, atlas_model::Evidence::P2pTransaction { .. }) => false,
            (Self::Inbound | Self::All, _) => true,
        }
    }
}

pub struct NatsConfig {
    pub address: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub p2p_policy: P2pPolicy,
}

pub struct RpcConfig {
    pub url: String,
    pub username: String,
    pub password: String,
    pub poll_interval: Duration,
}

pub struct RuntimeConfig {
    pub identity: AgentIdentity,
    pub atlas_ingest_endpoint: String,
    pub delivery_retry_interval: Duration,
    pub nats: Option<NatsConfig>,
    pub rpc: RpcConfig,
}

#[derive(Debug, Eq, PartialEq)]
struct CaptureGapNotice {
    occurred_at_ms: u64,
    reason: &'static str,
    certainty: CaptureGapCertainty,
}

struct NatsCaptureInputs {
    mempool: Subscriber,
    netmsg: Subscriber,
    capture_gaps: mpsc::UnboundedReceiver<CaptureGapNotice>,
}

impl CaptureGapNotice {
    fn from_nats_event(event: &NatsEvent, occurred_at_ms: u64) -> Option<Self> {
        let (reason, certainty) = match event {
            NatsEvent::SlowConsumer(_) => ("slow_consumer", CaptureGapCertainty::KnownLoss),
            NatsEvent::Disconnected => ("disconnected", CaptureGapCertainty::PossibleLoss),
            NatsEvent::Connected
            | NatsEvent::LameDuckMode
            | NatsEvent::Draining
            | NatsEvent::Closed
            | NatsEvent::ServerError(_)
            | NatsEvent::ClientError(_) => return None,
        };
        Some(Self {
            occurred_at_ms,
            reason,
            certainty,
        })
    }
}

pub async fn run(config: RuntimeConfig, outbox: Outbox) -> anyhow::Result<()> {
    if let Some(nats) = config.nats.as_ref() {
        validate_nats_config(nats)?;
    }

    let nats_enabled = config.nats.is_some();
    let p2p_policy = config.nats.as_ref().map(|nats| nats.p2p_policy);
    info!(
        source_id = %config.identity.source_id,
        source_session_id = %config.identity.source_session_id,
        nats_enabled,
        ?p2p_policy,
        "atlas agent starting"
    );

    let http = HttpClient::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("building Atlas HTTP client")?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let projection_fence = Arc::new(Mutex::new(()));
    // A healthy first RPC snapshot establishes the baseline before capture can
    // mutate it. The RPC loop releases this guard after its first failed attempt
    // so NATS remains useful while RPC is unavailable.
    let initial_rpc_guard = Arc::clone(&projection_fence).lock_owned().await;
    let mut tasks = JoinSet::new();

    tasks.spawn(delivery_loop(
        outbox.clone(),
        http,
        config.atlas_ingest_endpoint,
        config.delivery_retry_interval,
        shutdown_rx.clone(),
    ));
    tasks.spawn(rpc_loop(
        outbox.clone(),
        config.identity.clone(),
        config.rpc,
        Arc::clone(&projection_fence),
        initial_rpc_guard,
        shutdown_rx.clone(),
    ));

    if let Some(nats) = config.nats {
        tasks.spawn(nats_loop(
            outbox,
            config.identity,
            nats,
            config.delivery_retry_interval,
            projection_fence,
            shutdown_rx,
        ));
    }

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
            Ok(Err(error)) => warn!(%error, "agent task failed during shutdown"),
            Err(error) => warn!(%error, "agent task panicked during shutdown"),
        }
    }
    outcome
}

async fn connect_nats(config: &NatsConfig) -> anyhow::Result<(NatsClient, NatsCaptureInputs)> {
    let (capture_gap_tx, capture_gap_rx) = mpsc::unbounded_channel();
    let options = match (&config.username, &config.password) {
        (Some(username), Some(password)) => {
            ConnectOptions::new().user_and_password(username.clone(), password.clone())
        }
        (None, None) => ConnectOptions::new(),
        _ => bail!("NATS username and password must be configured together"),
    };
    let options = options.event_callback(move |event| {
        let capture_gap_tx = capture_gap_tx.clone();
        async move {
            let capture_gap =
                if matches!(&event, NatsEvent::SlowConsumer(_) | NatsEvent::Disconnected) {
                    match unix_time_ms() {
                        Ok(occurred_at_ms) => {
                            CaptureGapNotice::from_nats_event(&event, occurred_at_ms)
                        }
                        Err(error) => {
                            warn!(%error, "could not timestamp peer-observer NATS capture gap");
                            None
                        }
                    }
                } else {
                    None
                };

            match event {
                NatsEvent::SlowConsumer(subscription_id) => {
                    warn!(
                        subscription_id,
                        "peer-observer NATS slow consumer; forensic evidence was dropped"
                    );
                }
                NatsEvent::Disconnected => warn!("peer-observer NATS disconnected"),
                NatsEvent::Closed => warn!("peer-observer NATS connection closed"),
                NatsEvent::ServerError(error) => {
                    warn!(%error, "peer-observer NATS server error");
                }
                NatsEvent::ClientError(error) => {
                    warn!(%error, "peer-observer NATS client error");
                }
                NatsEvent::Connected => info!("peer-observer NATS connected"),
                NatsEvent::LameDuckMode | NatsEvent::Draining => {
                    warn!(%event, "peer-observer NATS connection state changed");
                }
            }

            if let Some(capture_gap) = capture_gap
                && capture_gap_tx.send(capture_gap).is_err()
            {
                warn!("peer-observer NATS capture-gap receiver closed");
            }
        }
    });
    let client = options
        .connect(&config.address)
        .await
        .context("connecting to configured NATS endpoint")?;
    let mempool = client
        .subscribe(MEMPOOL_SUBJECT)
        .await
        .context("subscribing to peer-observer mempool events")?;
    let netmsg = client
        .subscribe(NETMSG_SUBJECT)
        .await
        .context("subscribing to peer-observer netmsg events")?;
    client
        .flush()
        .await
        .context("flushing peer-observer NATS subscriptions")?;
    info!(
        subjects = %format_args!("{MEMPOOL_SUBJECT},{NETMSG_SUBJECT}"),
        "peer-observer NATS subscriptions ready"
    );
    Ok((
        client,
        NatsCaptureInputs {
            mempool,
            netmsg,
            capture_gaps: capture_gap_rx,
        },
    ))
}

fn validate_nats_config(config: &NatsConfig) -> anyhow::Result<()> {
    if config.username.is_some() == config.password.is_some() {
        return Ok(());
    }
    bail!("NATS username and password must be configured together")
}

async fn nats_loop(
    outbox: Outbox,
    identity: AgentIdentity,
    config: NatsConfig,
    retry_interval: Duration,
    projection_fence: Arc<Mutex<()>>,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut accounting = CaptureAccounting::default();
    loop {
        let session = connect_nats(&config).await;
        match session {
            Ok((_client, inputs)) => {
                capture_loop(
                    outbox.clone(),
                    identity.clone(),
                    inputs,
                    config.p2p_policy,
                    Arc::clone(&projection_fence),
                    shutdown.clone(),
                    &mut accounting,
                )
                .await?;
                if *shutdown.borrow() {
                    return Ok(());
                }
                warn!("peer-observer NATS subscriptions ended; reconnecting");
            }
            Err(error) => {
                warn!(%error, "peer-observer NATS unavailable; retrying");
            }
        }

        if wait_for_retry(retry_interval, &mut shutdown).await {
            return Ok(());
        }
    }
}

async fn capture_loop(
    outbox: Outbox,
    identity: AgentIdentity,
    inputs: NatsCaptureInputs,
    p2p_policy: P2pPolicy,
    projection_fence: Arc<Mutex<()>>,
    mut shutdown: watch::Receiver<bool>,
    accounting: &mut CaptureAccounting,
) -> anyhow::Result<()> {
    let NatsCaptureInputs {
        mut mempool,
        mut netmsg,
        mut capture_gaps,
    } = inputs;
    let mut accounting_ticker = interval(CAPTURE_ACCOUNTING_INTERVAL);
    accounting_ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    accounting_ticker.tick().await;

    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
            message = mempool.next() => {
                let Some(message) = message else {
                    return Ok(());
                };
                let disposition = capture_message(
                    &outbox,
                    &identity,
                    p2p_policy,
                    &projection_fence,
                    message,
                ).await?;
                accounting.record(disposition);
            }
            message = netmsg.next() => {
                let Some(message) = message else {
                    return Ok(());
                };
                let disposition = capture_message(
                    &outbox,
                    &identity,
                    p2p_policy,
                    &projection_fence,
                    message,
                ).await?;
                accounting.record(disposition);
            }
            capture_gap = capture_gaps.recv() => {
                let Some(capture_gap) = capture_gap else {
                    return Ok(());
                };
                capture_gap_notice(
                    &outbox,
                    &identity,
                    &projection_fence,
                    capture_gap,
                ).await?;
            }
            _ = accounting_ticker.tick() => {
                log_capture_accounting(&outbox, accounting).await;
            }
        }
    }
}

async fn capture_gap_notice(
    outbox: &Outbox,
    identity: &AgentIdentity,
    projection_fence: &Arc<Mutex<()>>,
    capture_gap: CaptureGapNotice,
) -> anyhow::Result<()> {
    let CaptureGapNotice {
        occurred_at_ms,
        reason,
        certainty,
    } = capture_gap;
    let outbox = outbox.clone();
    let identity = identity.clone();
    let _projection_guard = projection_fence.lock().await;
    let pending = tokio::task::spawn_blocking(move || {
        outbox.enqueue_observation(
            &identity,
            occurred_at_ms,
            occurred_at_ms,
            Evidence::CaptureGap {
                input: "peer_observer_nats".to_owned(),
                reason: reason.to_owned(),
                certainty,
            },
            None,
            None,
        )
    })
    .await
    .context("joining NATS capture-gap outbox task")??;
    warn!(
        outbox_id = pending.outbox_id,
        event_id = %pending.event.event_id,
        reason,
        "peer-observer NATS capture gap queued"
    );
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptureDisposition {
    Persisted,
    SuppressedP2p,
    IgnoredUnsupported,
    Invalid,
}

#[derive(Default)]
struct CaptureAccounting {
    total_nats_messages: u64,
    supported_persisted_observations: u64,
    suppressed_p2p_observations: u64,
    ignored_unsupported_messages: u64,
    invalid_messages: u64,
}

impl CaptureAccounting {
    fn record(&mut self, disposition: CaptureDisposition) {
        self.total_nats_messages = self.total_nats_messages.saturating_add(1);
        let counter = match disposition {
            CaptureDisposition::Persisted => &mut self.supported_persisted_observations,
            CaptureDisposition::SuppressedP2p => &mut self.suppressed_p2p_observations,
            CaptureDisposition::IgnoredUnsupported => &mut self.ignored_unsupported_messages,
            CaptureDisposition::Invalid => &mut self.invalid_messages,
        };
        *counter = counter.saturating_add(1);
    }
}

async fn log_capture_accounting(outbox: &Outbox, accounting: &CaptureAccounting) {
    let outbox = outbox.clone();
    let pending_outbox_depth =
        match tokio::task::spawn_blocking(move || outbox.pending_count()).await {
            Ok(Ok(pending_outbox_depth)) => Some(pending_outbox_depth),
            Ok(Err(error)) => {
                warn!(%error, "reading pending outbox depth for NATS capture accounting failed");
                None
            }
            Err(error) => {
                warn!(%error, "pending outbox depth accounting task panicked");
                None
            }
        };
    info!(
        total_nats_messages = accounting.total_nats_messages,
        supported_persisted_observations = accounting.supported_persisted_observations,
        suppressed_p2p_observations = accounting.suppressed_p2p_observations,
        ignored_unsupported_messages = accounting.ignored_unsupported_messages,
        invalid_messages = accounting.invalid_messages,
        ?pending_outbox_depth,
        "peer-observer NATS capture accounting"
    );
}

async fn capture_message(
    outbox: &Outbox,
    identity: &AgentIdentity,
    p2p_policy: P2pPolicy,
    projection_fence: &Arc<Mutex<()>>,
    message: Message,
) -> anyhow::Result<CaptureDisposition> {
    let received_at_ms = unix_time_ms()?;
    let observation = match observe_payload(&message.payload) {
        Ok(Some(observation)) => observation,
        Ok(None) => return Ok(CaptureDisposition::IgnoredUnsupported),
        Err(error) => {
            warn!(subject = %message.subject, %error, "ignoring invalid peer-observer event");
            return Ok(CaptureDisposition::Invalid);
        }
    };
    if !p2p_policy.retains(&observation.evidence) {
        debug!(
            subject = %message.subject,
            event_kind = observation.evidence.kind(),
            "peer-observer P2P observation suppressed by capture policy"
        );
        return Ok(CaptureDisposition::SuppressedP2p);
    }
    let subject = message.subject.to_string();
    let raw_payload = message.payload.to_vec();
    let outbox = outbox.clone();
    let identity = identity.clone();
    let _projection_guard = projection_fence.lock().await;
    let pending = tokio::task::spawn_blocking(move || {
        outbox.enqueue_observation(
            &identity,
            observation.observed_at_ms,
            received_at_ms,
            observation.evidence,
            Some(&subject),
            Some(&raw_payload),
        )
    })
    .await
    .context("joining peer-observer outbox task")??;
    debug!(
        outbox_id = pending.outbox_id,
        event_id = %pending.event.event_id,
        event_kind = pending.event.evidence.kind(),
        "peer-observer event queued"
    );
    Ok(CaptureDisposition::Persisted)
}

async fn delivery_loop(
    outbox: Outbox,
    client: HttpClient,
    endpoint: String,
    retry_interval: Duration,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    loop {
        let wait = match deliver_next(&outbox, &client, &endpoint).await? {
            DeliveryOutcome::Idle => IDLE_DELIVERY_POLL,
            DeliveryOutcome::Delivered {
                outbox_id,
                event_id,
                status,
            } => {
                debug!(outbox_id, %event_id, ?status, "outbox event delivered");
                continue;
            }
            DeliveryOutcome::BatchDelivered {
                first_outbox_id,
                last_outbox_id,
                event_count,
            } => {
                debug!(
                    first_outbox_id,
                    last_outbox_id, event_count, "RPC reconciliation batch delivered"
                );
                continue;
            }
            DeliveryOutcome::Deferred {
                outbox_id,
                attempts,
                error,
            } => {
                warn!(outbox_id, attempts, %error, "outbox head delivery deferred");
                retry_interval
            }
        };

        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
            () = tokio::time::sleep(wait) => {}
        }
    }
}

async fn rpc_loop(
    outbox: Outbox,
    identity: AgentIdentity,
    config: RpcConfig,
    projection_fence: Arc<Mutex<()>>,
    initial_projection_guard: OwnedMutexGuard<()>,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut initial_projection_guard = Some(initial_projection_guard);
    loop {
        let connection = {
            let _projection_guard = match initial_projection_guard.take() {
                Some(guard) => guard,
                None => Arc::clone(&projection_fence).lock_owned().await,
            };
            match RpcClient::connect(
                &config.url,
                config.username.clone(),
                config.password.clone(),
            )
            .await
            {
                Ok((rpc, snapshot)) => {
                    let correction_count =
                        reconcile(outbox.clone(), identity.clone(), snapshot, unix_time_ms()?)
                            .await?;
                    Some((rpc, correction_count))
                }
                Err(error) => {
                    warn!(%error, "Bitcoin RPC unavailable; retrying");
                    None
                }
            }
        };

        if let Some((rpc, correction_count)) = connection {
            info!(correction_count, "Bitcoin RPC reconciliation ready");
            return poll_rpc(
                outbox,
                identity,
                rpc,
                config.poll_interval,
                projection_fence,
                shutdown,
            )
            .await;
        }

        if wait_for_retry(config.poll_interval, &mut shutdown).await {
            return Ok(());
        }
    }
}

async fn poll_rpc(
    outbox: Outbox,
    identity: AgentIdentity,
    rpc: RpcClient,
    poll_interval: Duration,
    projection_fence: Arc<Mutex<()>>,
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
                let _projection_guard = projection_fence.lock().await;
                match rpc.get_mempool_snapshot().await {
                    Ok(snapshot) => {
                        let correction_count = reconcile(
                            outbox.clone(),
                            identity.clone(),
                            snapshot,
                            unix_time_ms()?,
                        ).await?;
                        if correction_count == 0 {
                            debug!("RPC mempool projection already converged");
                        } else {
                            info!(correction_count, "RPC mempool reconciliation queued corrections");
                        }
                    }
                    Err(error) => {
                        warn!(%error, "RPC mempool reconciliation failed; projection retained");
                    }
                }
            }
        }
    }
}

async fn reconcile(
    outbox: Outbox,
    identity: AgentIdentity,
    snapshot: BTreeMap<String, MempoolEntryFacts>,
    completed_at_ms: u64,
) -> anyhow::Result<usize> {
    let correction_count = tokio::task::spawn_blocking(move || {
        outbox.reconcile_rpc_snapshot(&identity, snapshot, completed_at_ms)
    })
    .await
    .context("joining RPC reconciliation outbox task")??;
    Ok(correction_count)
}

async fn wait_for_retry(delay: Duration, shutdown: &mut watch::Receiver<bool>) -> bool {
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
        Some(Ok(Ok(()))) => Err(anyhow!("agent task exited unexpectedly")),
        Some(Ok(Err(error))) => Err(error),
        Some(Err(error)) => Err(error).context("agent task panicked"),
        None => Err(anyhow!("all agent tasks exited unexpectedly")),
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
    use atlas_model::{Evidence, SourceId, SourceSessionId};
    use prost::Message as _;
    use tempfile::TempDir;

    use super::*;
    use crate::peer_observer::wire::bitcoin_primitives::Transaction;
    use crate::peer_observer::wire::ebpf_extractor::Ebpf;
    use crate::peer_observer::wire::ebpf_extractor::ebpf;
    use crate::peer_observer::wire::ebpf_extractor::message::{
        MessageEvent, Metadata, Tx, message_event::Msg,
    };
    use crate::peer_observer::wire::event::Event;
    use crate::peer_observer::wire::event::event::PeerObserverEvent;

    fn test_capture() -> (TempDir, Outbox, AgentIdentity, Arc<Mutex<()>>) {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let database = temporary.path().join("outbox.db");
        Outbox::migrate(&database).expect("migrate");
        let source_id = SourceId::new("core-a").expect("source");
        let outbox = Outbox::open(&database, source_id.clone()).expect("open");
        let identity = AgentIdentity::new(
            source_id,
            SourceSessionId::new("session-a").expect("session"),
        );
        (temporary, outbox, identity, Arc::new(Mutex::new(())))
    }

    fn nats_message(subject: &str, payload: Vec<u8>) -> Message {
        let length = payload.len();
        Message {
            subject: subject.into(),
            reply: None,
            payload: payload.into(),
            headers: None,
            status: None,
            description: None,
            length,
        }
    }

    fn p2p_message(inbound: bool) -> Message {
        let event = Event {
            timestamp: 1_721_234_567_890,
            peer_observer_event: Some(PeerObserverEvent::EbpfExtractor(Ebpf {
                ebpf_event: Some(ebpf::EbpfEvent::Message(MessageEvent {
                    meta: Metadata {
                        peer_id: 17,
                        addr: "127.0.0.1:8333".to_owned(),
                        conn_type: 1,
                        command: "tx".to_owned(),
                        inbound,
                        size: 2,
                    },
                    msg: Some(Msg::Tx(Tx {
                        tx: Transaction {
                            txid: (0_u8..32).collect(),
                            wtxid: (32_u8..64).collect(),
                            raw: Some(vec![0xde, 0xad]),
                        },
                    })),
                })),
            })),
        };
        nats_message(NETMSG_SUBJECT, event.encode_to_vec())
    }

    #[test]
    fn nats_loss_events_map_to_capture_gap_notices() {
        assert_eq!(
            CaptureGapNotice::from_nats_event(&NatsEvent::SlowConsumer(17), 123),
            Some(CaptureGapNotice {
                occurred_at_ms: 123,
                reason: "slow_consumer",
                certainty: CaptureGapCertainty::KnownLoss,
            })
        );
        assert_eq!(
            CaptureGapNotice::from_nats_event(&NatsEvent::Disconnected, 456),
            Some(CaptureGapNotice {
                occurred_at_ms: 456,
                reason: "disconnected",
                certainty: CaptureGapCertainty::PossibleLoss,
            })
        );
    }

    #[test]
    fn other_nats_events_do_not_map_to_capture_gaps() {
        let events = [
            NatsEvent::Connected,
            NatsEvent::Closed,
            NatsEvent::LameDuckMode,
            NatsEvent::Draining,
            NatsEvent::ServerError(async_nats::ServerError::Other("server".to_owned())),
            NatsEvent::ClientError(async_nats::ClientError::Other("client".to_owned())),
        ];

        for event in events {
            assert_eq!(CaptureGapNotice::from_nats_event(&event, 123), None);
        }
    }

    #[tokio::test]
    async fn capture_gap_notice_queues_one_timestamped_non_membership_event() {
        let (_temporary, outbox, identity, projection_fence) = test_capture();

        capture_gap_notice(
            &outbox,
            &identity,
            &projection_fence,
            CaptureGapNotice {
                occurred_at_ms: 123,
                reason: "slow_consumer",
                certainty: CaptureGapCertainty::KnownLoss,
            },
        )
        .await
        .expect("capture gap");

        let pending = outbox.next_pending().expect("next").expect("pending");
        assert_eq!(pending.event.observed_at_ms, 123);
        assert_eq!(pending.event.received_at_ms, 123);
        assert_eq!(
            pending.event.evidence,
            Evidence::CaptureGap {
                input: "peer_observer_nats".to_owned(),
                reason: "slow_consumer".to_owned(),
                certainty: CaptureGapCertainty::KnownLoss,
            }
        );
        assert!(
            outbox
                .projected_membership()
                .expect("projection")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn inbound_policy_does_not_filter_mempool_evidence() {
        let (_temporary, outbox, identity, projection_fence) = test_capture();
        let raw_payload = include_bytes!("../../../fixtures/peer-observer/mempool-added.pb");
        let message = nats_message(MEMPOOL_SUBJECT, raw_payload.to_vec());

        let disposition = capture_message(
            &outbox,
            &identity,
            P2pPolicy::Inbound,
            &projection_fence,
            message,
        )
        .await
        .expect("capture");

        assert_eq!(disposition, CaptureDisposition::Persisted);
        let pending = outbox.next_pending().expect("next").expect("pending");
        assert!(matches!(
            pending.event.evidence,
            Evidence::MempoolAdded { .. }
        ));
        assert_eq!(outbox.pending_count().expect("count"), 1);
    }

    #[tokio::test]
    async fn inbound_policy_persists_inbound_p2p_transaction() {
        let (_temporary, outbox, identity, projection_fence) = test_capture();

        let disposition = capture_message(
            &outbox,
            &identity,
            P2pPolicy::Inbound,
            &projection_fence,
            p2p_message(true),
        )
        .await
        .expect("capture");

        assert_eq!(disposition, CaptureDisposition::Persisted);
        assert_eq!(outbox.pending_count().expect("count"), 1);
    }

    #[tokio::test]
    async fn inbound_policy_suppresses_outbound_p2p_transaction() {
        let (_temporary, outbox, identity, projection_fence) = test_capture();

        let disposition = capture_message(
            &outbox,
            &identity,
            P2pPolicy::Inbound,
            &projection_fence,
            p2p_message(false),
        )
        .await
        .expect("capture");

        assert_eq!(disposition, CaptureDisposition::SuppressedP2p);
        assert_eq!(outbox.pending_count().expect("count"), 0);
    }

    #[tokio::test]
    async fn all_policy_persists_outbound_p2p_transaction() {
        let (_temporary, outbox, identity, projection_fence) = test_capture();

        let disposition = capture_message(
            &outbox,
            &identity,
            P2pPolicy::All,
            &projection_fence,
            p2p_message(false),
        )
        .await
        .expect("capture");

        assert_eq!(disposition, CaptureDisposition::Persisted);
        assert_eq!(outbox.pending_count().expect("count"), 1);
    }

    #[test]
    fn inbound_policy_suppresses_unknown_p2p_direction() {
        let evidence = Evidence::P2pTransaction {
            txid: "00".repeat(32),
            wtxid: "11".repeat(32),
            raw_transaction_hex: None,
            peer_id: None,
            inbound: None,
        };

        assert!(!P2pPolicy::Inbound.retains(&evidence));
        assert!(P2pPolicy::All.retains(&evidence));
    }

    #[test]
    fn capture_accounting_tracks_totals_by_disposition() {
        let mut accounting = CaptureAccounting::default();
        for disposition in [
            CaptureDisposition::Persisted,
            CaptureDisposition::Persisted,
            CaptureDisposition::SuppressedP2p,
            CaptureDisposition::IgnoredUnsupported,
            CaptureDisposition::Invalid,
        ] {
            accounting.record(disposition);
        }

        assert_eq!(accounting.total_nats_messages, 5);
        assert_eq!(accounting.supported_persisted_observations, 2);
        assert_eq!(accounting.suppressed_p2p_observations, 1);
        assert_eq!(accounting.ignored_unsupported_messages, 1);
        assert_eq!(accounting.invalid_messages, 1);
    }
}
