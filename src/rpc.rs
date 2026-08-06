//! Bitcoin RPC access for complete, disposable mempool snapshots.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bitcoin::{Amount, BlockHash, Txid, Wtxid};
use corepc_client::client_sync::v28::Client;
use corepc_client::client_sync::{Auth, Error as CorepcError};
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use thiserror::Error;

use crate::model::{
    ChainTip, MAX_SAFE_JSON_INTEGER, MAX_SUPPORTED_MEMPOOL_ENTRIES, MembershipFacts, MempoolEntry,
    MempoolSnapshot, ModelError,
};

thread_local! {
    /// `corepc-client::Client::call` owns deserialization and does not expose a
    /// `DeserializeSeed`. The call and deserializer execute on one blocking
    /// thread, so a scoped thread-local carries the configured entry cap.
    static RAW_MEMPOOL_ENTRY_LIMIT: Cell<u64> =
        const { Cell::new(MAX_SUPPORTED_MEMPOOL_ENTRIES) };
}

#[derive(Clone, Debug)]
pub struct RpcClient {
    inner: Arc<Client>,
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
        let inner = Client::new_with_auth(url, Auth::UserPass(username.into(), password.into()))
            .map_err(RpcError::ClientInitialization)?;
        Ok(Self {
            inner: Arc::new(inner),
            max_mempool_entries,
        })
    }

    /// Fetches one complete mempool snapshot between two matching chain-tip
    /// reads and binds it to that stable tip.
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
            let collection_started_at_ms = system_now_ms()?;
            let starting_tip = decode_chain_tip(
                client
                    .call::<BlockchainInfoWire>("getblockchaininfo", &[])
                    .map_err(RpcError::GetBlockchainInfo)?,
            )?;

            let info = client
                .call::<MempoolInfoWire>("getmempoolinfo", &[])
                .map_err(RpcError::GetMempoolInfo)?;
            validate_reported_mempool_size(info.size, maximum)?;

            let _decode_limit = DecodeEntryLimitGuard::set(maximum);
            let entries = client
                .call::<RawMempoolWire>("getrawmempool", &[serde_json::Value::Bool(true)])
                .map_err(RpcError::GetRawMempool)?
                .into_entries(maximum)?;

            let ending_chain = client
                .call::<BlockchainInfoWire>("getblockchaininfo", &[])
                .map_err(RpcError::GetBlockchainInfo)?;
            let collection_completed_at_ms = system_now_ms()?;
            let ending_tip = decode_chain_tip(ending_chain)?;
            if starting_tip != ending_tip {
                return Err(RpcError::UnstableChainTip {
                    starting_height: starting_tip.height,
                    starting_hash: starting_tip.hash,
                    ending_height: ending_tip.height,
                    ending_hash: ending_tip.hash,
                });
            }
            let collection_duration_ms =
                collection_completed_at_ms.saturating_sub(collection_started_at_ms);

            MempoolSnapshot::new_with_collection_window(
                source_id,
                source_label,
                collection_started_at_ms,
                collection_completed_at_ms,
                collection_duration_ms,
                ending_tip,
                entries,
            )
            .map_err(RpcError::InvalidSnapshot)
        })
        .await?
    }
}

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("failed to create Bitcoin RPC client: {0}")]
    ClientInitialization(#[source] CorepcError),
    #[error("getmempoolinfo RPC failed or returned an invalid response: {0}")]
    GetMempoolInfo(#[source] CorepcError),
    #[error("getrawmempool RPC failed or returned an invalid response: {0}")]
    GetRawMempool(#[source] CorepcError),
    #[error("getblockchaininfo RPC failed or returned an invalid response: {0}")]
    GetBlockchainInfo(#[source] CorepcError),
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
        "Bitcoin node chain tip changed during mempool collection from {starting_height}:{starting_hash} to {ending_height}:{ending_hash}"
    )]
    UnstableChainTip {
        starting_height: u64,
        starting_hash: String,
        ending_height: u64,
        ending_hash: String,
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
            Self::ClientInitialization(_)
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
    /// mining priority. Prioritisation deltas can push this below `base`.
    #[serde(with = "bitcoin::amount::serde::as_btc")]
    ancestor: Amount,
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
    async fn rejects_snapshot_when_chain_tip_changes_during_collection() {
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
            let result = client.get_mempool_snapshot("core", "Bitcoin Core").await;
            server.abort();

            assert!(matches!(
                result,
                Err(RpcError::UnstableChainTip {
                    starting_height: 900_000,
                    starting_hash,
                    ending_height,
                    ending_hash,
                }) if starting_hash == TXID_A
                    && ending_height == expected_height
                    && ending_hash == expected_hash
            ));
            assert_eq!(fixture.chain_reads.load(Ordering::SeqCst), 2);
        }
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
            with(
                base(),
                "fees",
                json!({ "base": 0.00000001, "ancestor": -0.00000001 }),
            ),
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
