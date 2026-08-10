//! Bitcoin RPC access for complete, disposable mempool snapshots.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bitcoin::{Amount, BlockHash, SignedAmount, Txid, Wtxid};
use serde::de::{DeserializeOwned, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;
use thiserror::Error;

use crate::model::{
    ChainTip, MAX_SAFE_JSON_INTEGER, MAX_SUPPORTED_MEMPOOL_ENTRIES, MembershipFacts, MempoolEntry,
    MempoolSnapshot, ModelError,
};
use crate::rpc_transport::{self, RpcTransportError};

const JSON_RPC_VERSION: &str = "2.0";
/// Explicit whole-request budget for one membership RPC, covering connect,
/// write, and bounded read. Matches the 30-second budget the service has
/// always allowed verbose mempool reads.
const MEMBERSHIP_RPC_TIMEOUT: Duration = Duration::from_secs(30);
/// Bounded attempts to observe one mempool against a stable chain tip. A block
/// arriving mid-collection is ordinary, so one moved tip must not strand a
/// source on its last good snapshot for the whole round.
const TIP_STABILITY_ATTEMPTS: u32 = 3;
/// Whole-turn retry budget for one source. The shared RPC work gate is held for
/// the entire membership round, so retries stop once a source has spent this
/// long. A source slower than this cannot outrun the roughly ten-minute block
/// interval, and retrying it would only widen the stall for every other source.
const TIP_STABILITY_RETRY_BUDGET: Duration = Duration::from_secs(120);
/// `getblockchaininfo` and `getmempoolinfo` are small control reads.
const CONTROL_RESPONSE_LIMIT_BYTES: usize = 256 * 1024;
/// Worst-case verbose mempool entry budget. Under the default 25-ancestor and
/// 25-descendant policy limits, `depends` and `spentby` carry at most 25
/// txids each (about 1.7 KiB per list), which fits with the fixed fields in
/// 4 KiB.
const VERBOSE_ENTRY_BUDGET_BYTES: usize = 4 * 1024;
/// Fixed JSON-RPC envelope slack for the verbose mempool response.
const RAW_MEMPOOL_ENVELOPE_BYTES: usize = 64 * 1024;

thread_local! {
    /// `MembershipRpcClient::call` deserializes the response body without a
    /// `DeserializeSeed`. The call and deserializer execute on one blocking
    /// thread, so a scoped thread-local carries the configured entry cap.
    static RAW_MEMPOOL_ENTRY_LIMIT: Cell<u64> =
        const { Cell::new(MAX_SUPPORTED_MEMPOOL_ENTRIES) };
}

/// Derives the verbose mempool response byte cap from the configured entry
/// limit, so a misbehaving endpoint cannot buffer arbitrarily more memory
/// than the configured mempool bound implies.
fn raw_mempool_response_limit(maximum_entries: u64) -> usize {
    usize::try_from(maximum_entries)
        .unwrap_or(usize::MAX)
        .saturating_mul(VERBOSE_ENTRY_BUDGET_BYTES)
        .saturating_add(RAW_MEMPOOL_ENVELOPE_BYTES)
}

#[derive(Clone, Debug)]
pub struct RpcClient {
    inner: Arc<MembershipRpcClient>,
    max_mempool_entries: u64,
}

impl RpcClient {
    pub fn new(
        url: &str,
        username: impl Into<String>,
        password: impl Into<String>,
        max_mempool_entries: u64,
    ) -> Result<Self, RpcError> {
        validate_configured_limit(max_mempool_entries)?;
        validate_rpc_url(url)?;
        Ok(Self {
            inner: Arc::new(MembershipRpcClient {
                url: url.to_owned(),
                authorization: rpc_transport::basic_authorization(
                    &username.into(),
                    &password.into(),
                ),
                next_id: AtomicU64::new(1),
            }),
            max_mempool_entries,
        })
    }

    /// Fetches one complete mempool snapshot when the chain-tip reads before
    /// and after membership collection match, then binds it to the reported
    /// ending tip.
    ///
    /// A block arriving mid-collection is an ordinary event, not a node fault,
    /// so a moved tip is retried within [`TIP_STABILITY_ATTEMPTS`] and
    /// [`TIP_STABILITY_RETRY_BUDGET`]. The tip-match requirement itself is
    /// never relaxed: only a collection that observed one stable tip is ever
    /// returned. A source whose collection outlasts the block interval
    /// exhausts the budget and reports how many attempts it spent, rather than
    /// silently failing every round forever.
    pub async fn get_mempool_snapshot(
        &self,
        source_id: &str,
        source_label: &str,
    ) -> Result<MempoolSnapshot, RpcError> {
        let client = Arc::clone(&self.inner);
        let maximum = self.max_mempool_entries;
        let source_id = source_id.to_owned();
        let source_label = source_label.to_owned();
        tokio::task::spawn_blocking(move || {
            let turn_started_at = Instant::now();
            let mut attempts = 1;
            loop {
                let outcome = collect_snapshot_against_stable_tip(
                    &client,
                    &source_id,
                    &source_label,
                    maximum,
                    attempts,
                );
                let unstable = matches!(outcome, Err(RpcError::UnstableChainTip { .. }));
                if !unstable
                    || attempts >= TIP_STABILITY_ATTEMPTS
                    || turn_started_at.elapsed() >= TIP_STABILITY_RETRY_BUDGET
                {
                    return outcome;
                }
                attempts += 1;
            }
        })
        .await?
    }
}

/// Performs one complete membership collection and accepts it only when the
/// chain-tip reads taken before and after the collection match.
///
/// `attempts` is carried into [`RpcError::UnstableChainTip`] so an exhausted
/// retry budget is distinguishable in logs from a single unlucky block.
fn collect_snapshot_against_stable_tip(
    client: &MembershipRpcClient,
    source_id: &str,
    source_label: &str,
    maximum: u64,
    attempts: u32,
) -> Result<MempoolSnapshot, RpcError> {
    let collection_started_at_ms = system_now_ms()?;
    let starting_tip = decode_chain_tip(
        client
            .call::<BlockchainInfoWire>("getblockchaininfo", &[], CONTROL_RESPONSE_LIMIT_BYTES)
            .map_err(RpcError::GetBlockchainInfo)?,
    )?;

    let info = client
        .call::<MempoolInfoWire>("getmempoolinfo", &[], CONTROL_RESPONSE_LIMIT_BYTES)
        .map_err(RpcError::GetMempoolInfo)?;
    validate_reported_mempool_size(info.size, maximum)?;

    let _decode_limit = DecodeEntryLimitGuard::set(maximum);
    let entries = client
        .call::<RawMempoolWire>(
            "getrawmempool",
            &[serde_json::Value::Bool(true)],
            raw_mempool_response_limit(maximum),
        )
        .map_err(RpcError::GetRawMempool)?
        .into_entries(maximum)?;

    let ending_chain = client
        .call::<BlockchainInfoWire>("getblockchaininfo", &[], CONTROL_RESPONSE_LIMIT_BYTES)
        .map_err(RpcError::GetBlockchainInfo)?;
    let collection_completed_at_ms = system_now_ms()?;
    let ending_tip = decode_chain_tip(ending_chain)?;
    if starting_tip != ending_tip {
        return Err(RpcError::UnstableChainTip {
            starting_height: starting_tip.height,
            starting_hash: starting_tip.hash,
            ending_height: ending_tip.height,
            ending_hash: ending_tip.hash,
            attempts,
        });
    }
    let collection_duration_ms =
        collection_completed_at_ms.saturating_sub(collection_started_at_ms);

    MempoolSnapshot::new_with_collection_window(
        source_id.to_owned(),
        source_label.to_owned(),
        collection_started_at_ms,
        collection_completed_at_ms,
        collection_duration_ms,
        ending_tip,
        entries,
    )
    .map_err(RpcError::InvalidSnapshot)
}

/// Bounded single-request JSON-RPC client for membership collection. It
/// never follows redirects, sends the Basic credential only to the
/// configured origin, applies an explicit timeout, and reads responses
/// through per-method byte caps.
struct MembershipRpcClient {
    url: String,
    authorization: String,
    next_id: AtomicU64,
}

impl MembershipRpcClient {
    fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: &[serde_json::Value],
        maximum_response_bytes: usize,
    ) -> Result<T, MembershipRpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = serde_json::to_vec(&MembershipWireRequest {
            jsonrpc: JSON_RPC_VERSION,
            method,
            params,
            id,
        })
        .map_err(MembershipRpcError::EncodeRequest)?;
        let (status_code, body) = rpc_transport::post_json_bounded(
            &self.url,
            &self.authorization,
            MEMBERSHIP_RPC_TIMEOUT,
            body,
            maximum_response_bytes,
        )?;
        let response =
            serde_json::from_slice::<MembershipWireResponse>(&body).map_err(|source| {
                MembershipRpcError::DecodeResponse {
                    status_code,
                    source,
                }
            })?;
        if response.jsonrpc.as_deref() != Some(JSON_RPC_VERSION) || response.id != Some(id) {
            return Err(MembershipRpcError::MismatchedResponseEnvelope);
        }
        if let Some(error) = response.error {
            let code = serde_json::from_str::<MembershipRpcErrorObject>(error.get())
                .map(|object| object.code)
                .map_err(|source| MembershipRpcError::DecodeResponse {
                    status_code,
                    source,
                })?;
            return Err(MembershipRpcError::Rpc { code });
        }
        let result = response.result.ok_or(MembershipRpcError::MissingResult)?;
        serde_json::from_str::<T>(result.get()).map_err(MembershipRpcError::DecodeResult)
    }
}

impl fmt::Debug for MembershipRpcClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MembershipRpcClient")
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct MembershipWireRequest<'a> {
    jsonrpc: &'static str,
    method: &'a str,
    params: &'a [serde_json::Value],
    id: u64,
}

#[derive(Deserialize)]
struct MembershipWireResponse {
    #[serde(default)]
    result: Option<Box<RawValue>>,
    #[serde(default)]
    error: Option<Box<RawValue>>,
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    jsonrpc: Option<String>,
}

#[derive(Deserialize)]
struct MembershipRpcErrorObject {
    code: i32,
}

#[derive(Debug, Error)]
pub enum MembershipRpcError {
    #[error("failed to encode membership RPC request: {0}")]
    EncodeRequest(#[source] serde_json::Error),
    #[error("membership RPC transport failed: {0}")]
    Transport(#[from] RpcTransportError),
    #[error("membership RPC HTTP {status_code} response was not a valid JSON-RPC response")]
    DecodeResponse {
        status_code: u16,
        #[source]
        source: serde_json::Error,
    },
    #[error("membership RPC response envelope did not match the request")]
    MismatchedResponseEnvelope,
    #[error("membership RPC returned error code {code}")]
    Rpc { code: i32 },
    #[error("membership RPC response omitted its result")]
    MissingResult,
    #[error("membership RPC result could not be decoded: {0}")]
    DecodeResult(#[source] serde_json::Error),
}

fn validate_rpc_url(url: &str) -> Result<(), RpcError> {
    // Atlas RPC clients are built without HTTPS support; the supported
    // deployment reaches nodes over loopback or a trusted private transport.
    let remainder = url.strip_prefix("http://").ok_or(RpcError::InvalidRpcUrl)?;
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() {
        return Err(RpcError::InvalidRpcUrl);
    }
    if authority.contains('@') {
        return Err(RpcError::RpcUrlContainsUserinfo);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum RpcError {
    #[error(
        "membership RPC URL must start with http:// and include a host; Atlas RPC clients do not support HTTPS"
    )]
    InvalidRpcUrl,
    #[error(
        "membership RPC URL must not embed credentials; configure the username and password file instead"
    )]
    RpcUrlContainsUserinfo,
    #[error("getmempoolinfo RPC failed or returned an invalid response: {0}")]
    GetMempoolInfo(#[source] MembershipRpcError),
    #[error("getrawmempool RPC failed or returned an invalid response: {0}")]
    GetRawMempool(#[source] MembershipRpcError),
    #[error("getblockchaininfo RPC failed or returned an invalid response: {0}")]
    GetBlockchainInfo(#[source] MembershipRpcError),
    #[error("Bitcoin RPC worker task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
    #[error("configured mempool entry limit {configured} exceeds the supported maximum {maximum}")]
    InvalidMempoolEntryLimit { configured: u64, maximum: u64 },
    #[error("getmempoolinfo returned negative transaction count {size}")]
    InvalidMempoolSize { size: i64 },
    #[error(
        "Bitcoin mempool contains at least {actual} entries, exceeding the configured maximum {maximum}"
    )]
    SnapshotTooLarge { actual: u64, maximum: u64 },
    #[error("getrawmempool returned invalid txid {0:?}")]
    InvalidTxid(String),
    #[error("getrawmempool returned invalid wtxid {0:?}")]
    InvalidWtxid(String),
    #[error("getrawmempool returned duplicate canonical txid {0}")]
    DuplicateTxid(String),
    #[error("getrawmempool returned duplicate canonical wtxid {0}")]
    DuplicateWtxid(String),
    #[error("getrawmempool entry time {time_seconds} seconds overflows for txid {txid}")]
    EntryTimeOverflow { txid: String, time_seconds: u64 },
    #[error("getrawmempool returned invalid entry facts for txid {txid}: {source}")]
    InvalidEntryFacts {
        txid: String,
        #[source]
        source: ModelError,
    },
    #[error("getblockchaininfo returned invalid best block hash {0:?}")]
    InvalidBestBlockHash(String),
    #[error(
        "getblockchaininfo returned block height {0}, which cannot be represented exactly in JSON"
    )]
    InvalidBlockHeight(u64),
    #[error(
        "Bitcoin node chain tip changed during mempool collection from {starting_height}:{starting_hash} to {ending_height}:{ending_hash} across {attempts} attempts"
    )]
    UnstableChainTip {
        starting_height: u64,
        starting_hash: String,
        ending_height: u64,
        ending_hash: String,
        attempts: u32,
    },
    #[error("system clock is before the Unix epoch")]
    InvalidSystemClock,
    #[error("system time cannot be represented in milliseconds")]
    SystemTimeOverflow,
    #[error("RPC observation could not form a snapshot: {0}")]
    InvalidSnapshot(#[source] ModelError),
}

impl RpcError {
    /// Returns a stable status suitable for the public website. Detailed
    /// transport errors can contain private response bodies and stay in logs.
    pub fn public_message(&self) -> &'static str {
        match self {
            Self::GetMempoolInfo(_) | Self::GetRawMempool(_) | Self::GetBlockchainInfo(_) => {
                "Bitcoin node RPC is unavailable"
            }
            Self::SnapshotTooLarge { .. } | Self::InvalidMempoolEntryLimit { .. } => {
                "Bitcoin node mempool exceeds the configured limit"
            }
            Self::Task(_) => "Atlas snapshot worker failed",
            Self::InvalidSystemClock | Self::SystemTimeOverflow => "Atlas system clock is invalid",
            Self::InvalidRpcUrl
            | Self::RpcUrlContainsUserinfo
            | Self::InvalidMempoolSize { .. }
            | Self::InvalidTxid(_)
            | Self::InvalidWtxid(_)
            | Self::DuplicateTxid(_)
            | Self::DuplicateWtxid(_)
            | Self::EntryTimeOverflow { .. }
            | Self::InvalidEntryFacts { .. }
            | Self::InvalidBestBlockHash(_)
            | Self::InvalidBlockHeight(_)
            | Self::UnstableChainTip { .. }
            | Self::InvalidSnapshot(_) => "Bitcoin node returned an invalid mempool snapshot",
        }
    }
}

#[derive(Debug, Deserialize)]
struct MempoolInfoWire {
    size: i64,
}

#[derive(Debug, Deserialize)]
struct BlockchainInfoWire {
    blocks: u64,
    bestblockhash: String,
}

fn decode_chain_tip(chain: BlockchainInfoWire) -> Result<ChainTip, RpcError> {
    if chain.blocks > MAX_SAFE_JSON_INTEGER {
        return Err(RpcError::InvalidBlockHeight(chain.blocks));
    }
    let hash = BlockHash::from_str(&chain.bestblockhash)
        .map_err(|_| RpcError::InvalidBestBlockHash(chain.bestblockhash))?
        .to_string();
    Ok(ChainTip {
        height: chain.blocks,
        hash,
    })
}

#[derive(Debug, Deserialize)]
struct RawMempoolEntryWire {
    wtxid: String,
    vsize: u64,
    weight: u64,
    time: u64,
    #[serde(rename = "ancestorcount")]
    ancestor_count: u64,
    #[serde(rename = "ancestorsize")]
    ancestor_vsize: u64,
    #[serde(rename = "descendantcount")]
    descendant_count: u64,
    #[serde(rename = "descendantsize")]
    descendant_vsize: u64,
    #[serde(rename = "bip125-replaceable")]
    replaceable: bool,
    fees: RawMempoolFeesWire,
}

#[derive(Debug, Deserialize)]
struct RawMempoolFeesWire {
    #[serde(with = "bitcoin::amount::serde::as_btc")]
    base: Amount,
    /// Delta-adjusted ancestor fees, including this transaction, as used for
    /// mining priority. Prioritisation deltas can push this below `base` and
    /// below zero, so it decodes as a signed amount.
    #[serde(with = "bitcoin::amount::serde::as_btc")]
    ancestor: SignedAmount,
}

#[derive(Debug)]
struct RawMempoolWire {
    entries: Result<Vec<MempoolEntry>, RpcError>,
}

impl RawMempoolWire {
    fn into_entries(self, maximum: u64) -> Result<Vec<MempoolEntry>, RpcError> {
        let mut entries = self.entries?;
        enforce_snapshot_limit(entries.len(), maximum)?;
        entries.sort_unstable_by(|left, right| left.txid.cmp(&right.txid));
        if let Some(pair) = entries.windows(2).find(|pair| pair[0].txid == pair[1].txid) {
            return Err(RpcError::DuplicateTxid(pair[0].txid.clone()));
        }
        let mut wtxids = BTreeSet::new();
        if let Some(duplicate) = entries
            .iter()
            .map(|entry| entry.wtxid.clone())
            .find(|wtxid| !wtxids.insert(wtxid.clone()))
        {
            return Err(RpcError::DuplicateWtxid(duplicate));
        }
        Ok(entries)
    }
}

impl<'de> Deserialize<'de> for RawMempoolWire {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(RawMempoolVisitor)
    }
}

struct RawMempoolVisitor;

impl<'de> Visitor<'de> for RawMempoolVisitor {
    type Value = RawMempoolWire;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a verbose getrawmempool object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let maximum = RAW_MEMPOOL_ENTRY_LIMIT.get();
        let mut entries = Vec::new();
        let mut validation_error = None;
        let mut observed_entries = 0_u64;

        while let Some(txid) = map.next_key::<String>()? {
            observed_entries = observed_entries.saturating_add(1);
            if validation_error.is_some() {
                map.next_value::<IgnoredAny>()?;
                continue;
            }
            if observed_entries > maximum {
                map.next_value::<IgnoredAny>()?;
                validation_error = Some(RpcError::SnapshotTooLarge {
                    actual: observed_entries,
                    maximum,
                });
                continue;
            }

            let wire = map.next_value::<RawMempoolEntryWire>()?;
            match decode_entry(txid, wire) {
                Ok(entry) => entries.push(entry),
                Err(error) => validation_error = Some(error),
            }
        }

        Ok(RawMempoolWire {
            entries: validation_error.map_or(Ok(entries), Err),
        })
    }
}

struct DecodeEntryLimitGuard {
    previous: u64,
}

impl DecodeEntryLimitGuard {
    fn set(maximum: u64) -> Self {
        let previous = RAW_MEMPOOL_ENTRY_LIMIT.replace(maximum);
        Self { previous }
    }
}

impl Drop for DecodeEntryLimitGuard {
    fn drop(&mut self) {
        RAW_MEMPOOL_ENTRY_LIMIT.set(self.previous);
    }
}

fn decode_entry(value: String, entry: RawMempoolEntryWire) -> Result<MempoolEntry, RpcError> {
    let txid = Txid::from_str(&value)
        .map_err(|_| RpcError::InvalidTxid(value))?
        .to_string();
    let wtxid = Wtxid::from_str(&entry.wtxid)
        .map_err(|_| RpcError::InvalidWtxid(entry.wtxid))?
        .to_string();
    let entered_at_ms =
        entry
            .time
            .checked_mul(1_000)
            .ok_or_else(|| RpcError::EntryTimeOverflow {
                txid: txid.clone(),
                time_seconds: entry.time,
            })?;
    MempoolEntry::new_variant(
        txid.clone(),
        wtxid,
        entry.vsize,
        entry.fees.base.to_sat(),
        entered_at_ms,
        MembershipFacts {
            weight: entry.weight,
            ancestor_count: entry.ancestor_count,
            ancestor_vsize: entry.ancestor_vsize,
            ancestor_fee_sats: entry.fees.ancestor.to_sat(),
            descendant_count: entry.descendant_count,
            descendant_vsize: entry.descendant_vsize,
            replaceable: entry.replaceable,
        },
    )
    .map_err(|source| RpcError::InvalidEntryFacts { txid, source })
}

fn validate_configured_limit(maximum: u64) -> Result<(), RpcError> {
    if maximum == 0 || maximum > MAX_SUPPORTED_MEMPOOL_ENTRIES {
        return Err(RpcError::InvalidMempoolEntryLimit {
            configured: maximum,
            maximum: MAX_SUPPORTED_MEMPOOL_ENTRIES,
        });
    }
    Ok(())
}

fn validate_reported_mempool_size(size: i64, maximum: u64) -> Result<(), RpcError> {
    let actual = u64::try_from(size).map_err(|_| RpcError::InvalidMempoolSize { size })?;
    if actual > maximum {
        return Err(RpcError::SnapshotTooLarge { actual, maximum });
    }
    Ok(())
}

fn enforce_snapshot_limit(actual: usize, maximum: u64) -> Result<(), RpcError> {
    let actual = u64::try_from(actual).unwrap_or(u64::MAX);
    if actual > maximum {
        return Err(RpcError::SnapshotTooLarge { actual, maximum });
    }
    Ok(())
}

fn system_now_ms() -> Result<u64, RpcError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RpcError::InvalidSystemClock)?;
    u64::try_from(elapsed.as_millis()).map_err(|_| RpcError::SystemTimeOverflow)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc as Shared;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::{Value, json};
    use tokio::sync::Mutex as AsyncMutex;

    use super::*;

    const TXID_A: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const TXID_B: &str = "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f";

    fn decode(value: Value) -> Result<Vec<MempoolEntry>, RpcError> {
        serde_json::from_value::<RawMempoolWire>(value)
            .expect("supported wire response")
            .into_entries(MAX_SUPPORTED_MEMPOOL_ENTRIES)
    }

    fn verbose_entry(wtxid: &str, vsize: u64, time: i64, base_btc: f64) -> Value {
        json!({
            "wtxid": wtxid,
            "vsize": vsize,
            "weight": vsize * 4,
            "time": time,
            "ancestorcount": 1,
            "ancestorsize": vsize,
            "descendantcount": 1,
            "descendantsize": vsize,
            "bip125-replaceable": false,
            "fees": { "base": base_btc, "ancestor": base_btc }
        })
    }

    #[derive(Debug)]
    struct ObservedRpcCall {
        authorization: Option<String>,
        method: String,
        params: Value,
    }

    type ObservedRpcCalls = Shared<AsyncMutex<Vec<ObservedRpcCall>>>;

    #[derive(Debug)]
    struct ChangingTipFixture {
        chain_reads: AtomicUsize,
        ending_height: u64,
        ending_hash: &'static str,
    }

    async fn rpc_fixture(
        State(calls): State<ObservedRpcCalls>,
        headers: HeaderMap,
        Json(request): Json<Value>,
    ) -> Json<Value> {
        let is_batch = request.is_array();
        let requests = request.as_array().cloned().unwrap_or_else(|| vec![request]);
        let authorization = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let mut responses = Vec::with_capacity(requests.len());

        for request in requests {
            let method = request["method"].as_str().expect("RPC method").to_owned();
            let params = request["params"].clone();
            let id = request["id"].clone();
            calls.lock().await.push(ObservedRpcCall {
                authorization: authorization.clone(),
                method: method.clone(),
                params,
            });

            let (result, error) = match method.as_str() {
                "getmempoolinfo" => (json!({ "size": 1 }), Value::Null),
                "getrawmempool" => (
                    json!({ TXID_A: verbose_entry(TXID_A, 141, 1_721_234_000, 0.00001200) }),
                    Value::Null,
                ),
                "getblockchaininfo" => (
                    json!({
                        "blocks": 900_000,
                        "bestblockhash": TXID_B
                    }),
                    Value::Null,
                ),
                other => panic!("unexpected RPC method {other}"),
            };
            responses.push(json!({
                "jsonrpc": "2.0",
                "result": result,
                "error": error,
                "id": id
            }));
        }
        Json(if is_batch {
            Value::Array(responses)
        } else {
            responses.pop().expect("single response")
        })
    }

    /// Reports a fresh tip on every read, modelling a source whose collection
    /// never fits inside one block interval.
    #[derive(Debug)]
    struct AlwaysMovingTipFixture {
        chain_reads: AtomicUsize,
    }

    async fn always_moving_tip_rpc_fixture(
        State(fixture): State<Shared<AlwaysMovingTipFixture>>,
        Json(request): Json<Value>,
    ) -> Json<Value> {
        let method = request["method"].as_str().expect("RPC method");
        let result = match method {
            "getblockchaininfo" => {
                let read = fixture.chain_reads.fetch_add(1, Ordering::SeqCst);
                json!({
                    "blocks": 900_000 + read as u64,
                    "bestblockhash": TXID_A
                })
            }
            "getmempoolinfo" => json!({ "size": 1 }),
            "getrawmempool" => {
                json!({ TXID_A: verbose_entry(TXID_A, 141, 1_721_234_000, 0.00001200) })
            }
            other => panic!("unexpected RPC method {other}"),
        };
        Json(json!({
            "jsonrpc": "2.0",
            "result": result,
            "error": null,
            "id": request["id"]
        }))
    }

    async fn changing_tip_rpc_fixture(
        State(fixture): State<Shared<ChangingTipFixture>>,
        Json(request): Json<Value>,
    ) -> Json<Value> {
        let method = request["method"].as_str().expect("RPC method");
        let result = match method {
            "getblockchaininfo" => {
                if fixture.chain_reads.fetch_add(1, Ordering::SeqCst) == 0 {
                    json!({
                        "blocks": 900_000,
                        "bestblockhash": TXID_A
                    })
                } else {
                    json!({
                        "blocks": fixture.ending_height,
                        "bestblockhash": fixture.ending_hash
                    })
                }
            }
            "getmempoolinfo" => json!({ "size": 1 }),
            "getrawmempool" => {
                json!({ TXID_A: verbose_entry(TXID_A, 141, 1_721_234_000, 0.00001200) })
            }
            other => panic!("unexpected RPC method {other}"),
        };
        Json(json!({
            "jsonrpc": "2.0",
            "result": result,
            "error": null,
            "id": request["id"]
        }))
    }

    #[tokio::test]
    async fn performs_authenticated_membership_snapshot_sequence() {
        let calls = Shared::new(AsyncMutex::new(Vec::new()));
        let application = Router::new()
            .route("/", post(rpc_fixture))
            .with_state(Shared::clone(&calls));
        let (address, server) = crate::spawn_test_server(application).await;

        let client = RpcClient::new(
            &format!("http://{address}/"),
            "atlas",
            "secret",
            MAX_SUPPORTED_MEMPOOL_ENTRIES,
        )
        .expect("RPC client");
        let snapshot = client
            .get_mempool_snapshot("core", "Bitcoin Core")
            .await
            .expect("snapshot");
        server.abort();

        assert_eq!(snapshot.transaction_count, 1);
        assert_eq!(snapshot.transactions[0].fee_sats, 1_200);
        assert_eq!(snapshot.chain_tip.height, 900_000);
        assert!(snapshot.collection_completed_at_ms >= snapshot.collection_started_at_ms);
        assert_eq!(
            snapshot.collection_duration_ms,
            snapshot.collection_completed_at_ms - snapshot.collection_started_at_ms
        );
        assert_eq!(snapshot.observed_at_ms, snapshot.collection_completed_at_ms);
        assert_eq!(snapshot.bip110_summary.unclassified_count, 1);
        assert!(
            snapshot
                .transactions
                .iter()
                .all(|entry| entry.bip110.is_none())
        );

        let calls = calls.lock().await;
        assert_eq!(
            calls
                .iter()
                .map(|call| call.method.as_str())
                .collect::<Vec<_>>(),
            [
                "getblockchaininfo",
                "getmempoolinfo",
                "getrawmempool",
                "getblockchaininfo"
            ]
        );
        assert_eq!(calls[0].params, json!([]));
        assert_eq!(calls[1].params, json!([]));
        assert_eq!(calls[2].params, json!([true]));
        assert_eq!(calls[3].params, json!([]));
        assert!(
            calls
                .iter()
                .all(|call| call.authorization.as_deref() == Some("Basic YXRsYXM6c2VjcmV0"))
        );
    }

    #[tokio::test]
    async fn retries_and_recovers_when_the_chain_tip_moves_once_during_collection() {
        for (expected_height, expected_hash) in [(900_001, TXID_A), (900_000, TXID_B)] {
            let fixture = Shared::new(ChangingTipFixture {
                chain_reads: AtomicUsize::new(0),
                ending_height: expected_height,
                ending_hash: expected_hash,
            });
            let application = Router::new()
                .route("/", post(changing_tip_rpc_fixture))
                .with_state(Shared::clone(&fixture));
            let (address, server) = crate::spawn_test_server(application).await;

            let client = RpcClient::new(
                &format!("http://{address}/"),
                "atlas",
                "secret",
                MAX_SUPPORTED_MEMPOOL_ENTRIES,
            )
            .expect("RPC client");
            let snapshot = client
                .get_mempool_snapshot("core", "Bitcoin Core")
                .await
                .expect("snapshot recovered on the second attempt");
            server.abort();

            // The first attempt spans the new block and is discarded; the
            // second observes the settled tip on both reads and is published.
            assert_eq!(snapshot.chain_tip.height, expected_height);
            assert_eq!(snapshot.chain_tip.hash, expected_hash);
            assert_eq!(fixture.chain_reads.load(Ordering::SeqCst), 4);
        }
    }

    #[tokio::test]
    async fn rejects_snapshot_when_the_chain_tip_never_settles() {
        let fixture = Shared::new(AlwaysMovingTipFixture {
            chain_reads: AtomicUsize::new(0),
        });
        let application = Router::new()
            .route("/", post(always_moving_tip_rpc_fixture))
            .with_state(Shared::clone(&fixture));
        let (address, server) = crate::spawn_test_server(application).await;

        let client = RpcClient::new(
            &format!("http://{address}/"),
            "atlas",
            "secret",
            MAX_SUPPORTED_MEMPOOL_ENTRIES,
        )
        .expect("RPC client");
        let result = client.get_mempool_snapshot("core", "Bitcoin Core").await;
        server.abort();

        // Retries are bounded, so a source that can never observe a stable tip
        // fails its turn instead of holding the shared work gate indefinitely,
        // and reports the exhausted attempt count.
        assert!(matches!(
            result,
            Err(RpcError::UnstableChainTip { attempts, .. })
                if attempts == TIP_STABILITY_ATTEMPTS
        ));
        assert_eq!(
            fixture.chain_reads.load(Ordering::SeqCst),
            2 * TIP_STABILITY_ATTEMPTS as usize
        );
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
        use std::io::Read as _;
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).expect("read request");
            if read == 0 {
                return request;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        let headers_end = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("request headers")
            + 4;
        let headers = String::from_utf8_lossy(&request[..headers_end]).to_ascii_lowercase();
        let content_length = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        while request.len() < headers_end + content_length {
            let read = stream.read(&mut buffer).expect("read request body");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        request
    }

    fn serve_raw_once(response: Vec<u8>) -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("listener address");
        let server = std::thread::spawn(move || {
            use std::io::Write as _;
            let (mut stream, _) = listener.accept().expect("connection");
            let _request = read_http_request(&mut stream);
            stream.write_all(&response).expect("write response");
        });
        (address, server)
    }

    #[tokio::test]
    async fn membership_rpc_refuses_redirects_without_forwarding_credentials() {
        let redirect_target = std::net::TcpListener::bind("127.0.0.1:0").expect("redirect target");
        redirect_target
            .set_nonblocking(true)
            .expect("non-blocking redirect target");
        let location = format!(
            "http://{}/capture",
            redirect_target.local_addr().expect("redirect address")
        );
        let (address, server) = serve_raw_once(
            format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n")
                .into_bytes(),
        );

        let client = RpcClient::new(
            &format!("http://{address}/"),
            "atlas",
            "secret",
            MAX_SUPPORTED_MEMPOOL_ENTRIES,
        )
        .expect("RPC client");
        let result = client.get_mempool_snapshot("core", "Bitcoin Core").await;
        server.join().expect("fixture server");

        assert!(matches!(
            result,
            Err(RpcError::GetBlockchainInfo(MembershipRpcError::Transport(
                RpcTransportError::UnexpectedHttpStatus(302)
            )))
        ));
        assert!(matches!(
            redirect_target.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    #[tokio::test]
    async fn membership_rpc_rejects_announced_over_limit_bodies() {
        let announced = CONTROL_RESPONSE_LIMIT_BYTES + 1;
        let (address, server) = serve_raw_once(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {announced}\r\n\r\n"
            )
            .into_bytes(),
        );

        let client = RpcClient::new(
            &format!("http://{address}/"),
            "atlas",
            "secret",
            MAX_SUPPORTED_MEMPOOL_ENTRIES,
        )
        .expect("RPC client");
        let result = client.get_mempool_snapshot("core", "Bitcoin Core").await;
        server.join().expect("fixture server");

        assert!(matches!(
            result,
            Err(RpcError::GetBlockchainInfo(MembershipRpcError::Transport(
                RpcTransportError::ResponseTooLarge { actual, maximum }
            ))) if actual == announced && maximum == CONTROL_RESPONSE_LIMIT_BYTES
        ));
    }

    #[tokio::test]
    async fn membership_rpc_rejects_unannounced_over_limit_bodies() {
        let mut response = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n".to_vec();
        response.resize(response.len() + CONTROL_RESPONSE_LIMIT_BYTES + 1024, b'x');
        let (address, server) = serve_raw_once(response);

        let client = RpcClient::new(
            &format!("http://{address}/"),
            "atlas",
            "secret",
            MAX_SUPPORTED_MEMPOOL_ENTRIES,
        )
        .expect("RPC client");
        let result = client.get_mempool_snapshot("core", "Bitcoin Core").await;
        server.join().expect("fixture server");

        assert!(matches!(
            result,
            Err(RpcError::GetBlockchainInfo(MembershipRpcError::Transport(
                RpcTransportError::ResponseTooLarge { .. }
            )))
        ));
    }

    #[test]
    fn membership_rpc_urls_must_be_plain_http_origins() {
        assert!(matches!(
            RpcClient::new("ftp://127.0.0.1/", "atlas", "secret", 10),
            Err(RpcError::InvalidRpcUrl)
        ));
        assert!(matches!(
            RpcClient::new("http:///", "atlas", "secret", 10),
            Err(RpcError::InvalidRpcUrl)
        ));
        assert!(matches!(
            RpcClient::new("http://user:pass@127.0.0.1/", "atlas", "secret", 10),
            Err(RpcError::RpcUrlContainsUserinfo)
        ));
        assert!(matches!(
            RpcClient::new("https://node.example/", "atlas", "secret", 10),
            Err(RpcError::InvalidRpcUrl)
        ));
        assert!(RpcClient::new("http://127.0.0.1:8332/", "atlas", "secret", 10).is_ok());
    }

    #[test]
    fn decodes_common_core_and_knots_facts() {
        let entries = decode(json!({
            TXID_A: {
                "vsize": 141,
                "weight": 561,
                "time": 1_721_234_000_u64,
                "height": 850_000,
                "wtxid": TXID_B,
                "ancestorcount": 2,
                "ancestorsize": 363,
                "descendantcount": 1,
                "descendantsize": 141,
                "bip125-replaceable": true,
                "fees": {
                    "base": 0.00001200,
                    "modified": 0.00001200,
                    "ancestor": 0.00001200,
                    "descendant": 0.00001200
                },
                "depends": [],
                "spentby": []
            },
            TXID_B: {
                "wtxid": TXID_A,
                "vsize": 222,
                "weight": 885,
                "time": 1_721_234_001_u64,
                "ancestorcount": 1,
                "ancestorsize": 222,
                "descendantcount": 3,
                "descendantsize": 1_000,
                "bip125-replaceable": false,
                "fees": {
                    "base": 0.00000001,
                    "modified": 0.00000001,
                    "ancestor": 0.00000003
                }
            }
        }))
        .expect("Core and Knots response");

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].txid, TXID_A);
        assert_eq!(entries[0].fee_sats, 1_200);
        assert_eq!(entries[0].vsize, 141);
        assert_eq!(entries[0].weight, 561);
        assert_eq!(entries[0].ancestor_count, 2);
        assert_eq!(entries[0].ancestor_vsize, 363);
        assert_eq!(entries[0].descendant_count, 1);
        assert_eq!(entries[0].descendant_vsize, 141);
        assert_eq!(entries[0].ancestor_fee_sats, 1_200);
        assert!(entries[0].replaceable);
        assert_eq!(entries[0].entered_at_ms, 1_721_234_000_000);
        assert_eq!(entries[1].fee_sats, 1);
        assert_eq!(entries[1].ancestor_fee_sats, 3);
        assert_eq!(entries[1].descendant_vsize, 1_000);
        assert!(!entries[1].replaceable);
    }

    #[test]
    fn decodes_negative_delta_adjusted_ancestor_fees() {
        // A negative prioritisetransaction delta can push the delta-adjusted
        // ancestor total below zero while the base fee stays unsigned.
        let entries = decode(json!({
            TXID_A: {
                "vsize": 141,
                "weight": 561,
                "time": 1_721_234_000_u64,
                "wtxid": TXID_A,
                "ancestorcount": 1,
                "ancestorsize": 141,
                "descendantcount": 1,
                "descendantsize": 141,
                "bip125-replaceable": false,
                "fees": {
                    "base": 0.00001200,
                    "ancestor": -0.00000500
                }
            }
        }))
        .expect("negative ancestor fee decodes");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].fee_sats, 1_200);
        assert_eq!(entries[0].ancestor_fee_sats, -500);
    }

    #[test]
    fn canonicalizes_and_orders_txids() {
        let entries = decode(json!({
            TXID_B: verbose_entry(TXID_B, 2, 2, 0.00000002),
            TXID_A.to_ascii_uppercase(): verbose_entry(TXID_A, 1, 1, 0.00000001)
        }))
        .expect("valid response");

        assert_eq!(
            entries
                .into_iter()
                .map(|entry| entry.txid)
                .collect::<Vec<_>>(),
            vec![TXID_A.to_owned(), TXID_B.to_owned()]
        );
    }

    #[test]
    fn rejects_invalid_or_duplicate_txids() {
        assert!(matches!(
            decode(json!({
                "not-a-txid": verbose_entry(TXID_A, 1, 1, 0.00000001)
            })),
            Err(RpcError::InvalidTxid(txid)) if txid == "not-a-txid"
        ));

        assert!(matches!(
            decode(json!({
                TXID_A: verbose_entry(TXID_A, 1, 1, 0.00000001),
                TXID_A.to_ascii_uppercase(): verbose_entry(TXID_B, 2, 2, 0.00000002)
            })),
            Err(RpcError::DuplicateTxid(txid)) if txid == TXID_A
        ));

        assert!(matches!(
            decode(json!({
                TXID_A: verbose_entry(TXID_B, 1, 1, 0.00000001),
                TXID_B: verbose_entry(TXID_B, 2, 2, 0.00000002)
            })),
            Err(RpcError::DuplicateWtxid(wtxid)) if wtxid == TXID_B
        ));
    }

    #[test]
    fn rejects_missing_malformed_or_unsafe_required_facts() {
        fn without(mut entry: Value, key: &str) -> Value {
            entry
                .as_object_mut()
                .expect("verbose entry object")
                .remove(key);
            entry
        }

        fn with(mut entry: Value, key: &str, value: Value) -> Value {
            entry
                .as_object_mut()
                .expect("verbose entry object")
                .insert(key.to_owned(), value);
            entry
        }

        let base = || verbose_entry(TXID_A, 1, 1, 0.00000001);
        for entry in [
            without(base(), "vsize"),
            without(base(), "time"),
            without(base(), "fees"),
            without(base(), "wtxid"),
            without(base(), "weight"),
            without(base(), "ancestorcount"),
            without(base(), "ancestorsize"),
            without(base(), "descendantcount"),
            without(base(), "descendantsize"),
            without(base(), "bip125-replaceable"),
            with(base(), "wtxid", json!("not-a-wtxid")),
            with(base(), "vsize", json!(0)),
            with(base(), "vsize", json!(1.5)),
            with(base(), "time", json!(-1)),
            with(base(), "weight", json!(0)),
            with(base(), "weight", json!(9)),
            with(base(), "ancestorcount", json!(0)),
            with(base(), "ancestorsize", json!(0)),
            with(base(), "descendantcount", json!(0)),
            with(base(), "descendantsize", json!(0)),
            with(base(), "bip125-replaceable", json!("yes")),
            with(base(), "fees", json!({ "base": -0.00000001 })),
            with(base(), "fees", json!({ "base": 0.00000001 })),
            with(base(), "fees", json!({ "base": 0.000000001 })),
        ] {
            let response = json!({ TXID_A: entry });
            if let Ok(wire) = serde_json::from_value::<RawMempoolWire>(response) {
                assert!(wire.into_entries(MAX_SUPPORTED_MEMPOOL_ENTRIES).is_err());
            }
        }
    }

    #[test]
    fn enforces_preflight_and_decode_limits() {
        assert!(matches!(
            validate_reported_mempool_size(-1, 10),
            Err(RpcError::InvalidMempoolSize { size: -1 })
        ));
        assert!(matches!(
            validate_reported_mempool_size(11, 10),
            Err(RpcError::SnapshotTooLarge {
                actual: 11,
                maximum: 10
            })
        ));

        let _decode_limit = DecodeEntryLimitGuard::set(1);
        let result = serde_json::from_value::<RawMempoolWire>(json!({
            TXID_A: verbose_entry(TXID_A, 1, 1, 0.00000001),
            TXID_B: verbose_entry(TXID_B, 2, 2, 0.00000002)
        }))
        .expect("wire response")
        .into_entries(1);
        assert!(matches!(
            result,
            Err(RpcError::SnapshotTooLarge {
                actual: 2,
                maximum: 1
            })
        ));
    }

    #[test]
    fn public_errors_do_not_expose_snapshot_details() {
        let error = RpcError::InvalidTxid("private response content".to_owned());
        assert_eq!(
            error.public_message(),
            "Bitcoin node returned an invalid mempool snapshot"
        );
        assert!(!error.public_message().contains("private"));
    }
}
