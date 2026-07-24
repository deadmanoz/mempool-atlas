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
    ChainTip, MAX_SUPPORTED_MEMPOOL_ENTRIES, MempoolEntry, MempoolSnapshot, ModelError,
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

    /// Fetches one complete mempool snapshot and binds it to the node's
    /// chain tip immediately after the verbose mempool call.
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
            let info = client
                .call::<MempoolInfoWire>("getmempoolinfo", &[])
                .map_err(RpcError::GetMempoolInfo)?;
            validate_reported_mempool_size(info.size, maximum)?;

            let _decode_limit = DecodeEntryLimitGuard::set(maximum);
            let entries = client
                .call::<RawMempoolWire>("getrawmempool", &[serde_json::Value::Bool(true)])
                .map_err(RpcError::GetRawMempool)?
                .into_entries(maximum)?;

            let chain = client
                .call::<BlockchainInfoWire>("getblockchaininfo", &[])
                .map_err(RpcError::GetBlockchainInfo)?;
            let hash = BlockHash::from_str(&chain.bestblockhash)
                .map_err(|_| RpcError::InvalidBestBlockHash(chain.bestblockhash))?
                .to_string();
            let observed_at_ms = system_now_ms()?;

            MempoolSnapshot::new(
                source_id,
                source_label,
                observed_at_ms,
                ChainTip {
                    height: chain.blocks,
                    hash,
                },
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

#[derive(Debug, Deserialize)]
struct RawMempoolEntryWire {
    wtxid: String,
    vsize: u64,
    time: u64,
    fees: RawMempoolFeesWire,
}

#[derive(Debug, Deserialize)]
struct RawMempoolFeesWire {
    #[serde(with = "bitcoin::amount::serde::as_btc")]
    base: Amount,
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

    #[derive(Debug)]
    struct ObservedRpcCall {
        authorization: Option<String>,
        method: String,
        params: Value,
    }

    type ObservedRpcCalls = Shared<AsyncMutex<Vec<ObservedRpcCall>>>;

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
                    json!({
                        TXID_A: {
                            "wtxid": TXID_A,
                            "vsize": 141,
                            "time": 1_721_234_000_u64,
                            "fees": { "base": 0.00001200 }
                        }
                    }),
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

    #[tokio::test]
    async fn performs_authenticated_membership_snapshot_sequence() {
        let calls = Shared::new(AsyncMutex::new(Vec::new()));
        let application = Router::new()
            .route("/", post(rpc_fixture))
            .with_state(Shared::clone(&calls));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let server = tokio::spawn(async move {
            axum::serve(listener, application)
                .await
                .expect("fixture server");
        });

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
            ["getmempoolinfo", "getrawmempool", "getblockchaininfo"]
        );
        assert_eq!(calls[0].params, json!([]));
        assert_eq!(calls[1].params, json!([true]));
        assert_eq!(calls[2].params, json!([]));
        assert!(
            calls
                .iter()
                .all(|call| call.authorization.as_deref() == Some("Basic YXRsYXM6c2VjcmV0"))
        );
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
                "time": 1_721_234_001_u64,
                "fees": {
                    "base": 0.00000001,
                    "modified": 0.00000001
                }
            }
        }))
        .expect("Core and Knots response");

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].txid, TXID_A);
        assert_eq!(entries[0].fee_sats, 1_200);
        assert_eq!(entries[0].vsize, 141);
        assert_eq!(entries[0].entered_at_ms, 1_721_234_000_000);
        assert_eq!(entries[1].fee_sats, 1);
    }

    #[test]
    fn canonicalizes_and_orders_txids() {
        let entries = decode(json!({
            TXID_B: {
                "wtxid": TXID_B,
                "vsize": 2,
                "time": 2,
                "fees": { "base": 0.00000002 }
            },
            TXID_A.to_ascii_uppercase(): {
                "wtxid": TXID_A,
                "vsize": 1,
                "time": 1,
                "fees": { "base": 0.00000001 }
            }
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
                "not-a-txid": {
                    "wtxid": TXID_A,
                    "vsize": 1,
                    "time": 1,
                    "fees": { "base": 0.00000001 }
                }
            })),
            Err(RpcError::InvalidTxid(txid)) if txid == "not-a-txid"
        ));

        assert!(matches!(
            decode(json!({
                TXID_A: {
                    "wtxid": TXID_A,
                    "vsize": 1,
                    "time": 1,
                    "fees": { "base": 0.00000001 }
                },
                TXID_A.to_ascii_uppercase(): {
                    "wtxid": TXID_B,
                    "vsize": 2,
                    "time": 2,
                    "fees": { "base": 0.00000002 }
                }
            })),
            Err(RpcError::DuplicateTxid(txid)) if txid == TXID_A
        ));

        assert!(matches!(
            decode(json!({
                TXID_A: {
                    "wtxid": TXID_B,
                    "vsize": 1,
                    "time": 1,
                    "fees": { "base": 0.00000001 }
                },
                TXID_B: {
                    "wtxid": TXID_B,
                    "vsize": 2,
                    "time": 2,
                    "fees": { "base": 0.00000002 }
                }
            })),
            Err(RpcError::DuplicateWtxid(wtxid)) if wtxid == TXID_B
        ));
    }

    #[test]
    fn rejects_missing_malformed_or_unsafe_required_facts() {
        for response in [
            json!({ TXID_A: {
                "wtxid": TXID_A,
                "time": 1,
                "fees": { "base": 0.00000001 }
            } }),
            json!({ TXID_A: {
                "wtxid": TXID_A,
                "vsize": 1,
                "fees": { "base": 0.00000001 }
            } }),
            json!({ TXID_A: {
                "wtxid": TXID_A,
                "vsize": 1,
                "time": 1
            } }),
            json!({ TXID_A: {
                "vsize": 1,
                "time": 1,
                "fees": { "base": 0.00000001 }
            } }),
            json!({ TXID_A: {
                "wtxid": "not-a-wtxid",
                "vsize": 1,
                "time": 1,
                "fees": { "base": 0.00000001 }
            } }),
            json!({ TXID_A: {
                "wtxid": TXID_A,
                "vsize": 0,
                "time": 1,
                "fees": { "base": 0.00000001 }
            } }),
            json!({ TXID_A: {
                "wtxid": TXID_A,
                "vsize": 1.5,
                "time": 1,
                "fees": { "base": 0.00000001 }
            } }),
            json!({ TXID_A: {
                "wtxid": TXID_A,
                "vsize": 1,
                "time": -1,
                "fees": { "base": 0.00000001 }
            } }),
            json!({ TXID_A: {
                "wtxid": TXID_A,
                "vsize": 1,
                "time": 1,
                "fees": { "base": -0.00000001 }
            } }),
            json!({ TXID_A: {
                "wtxid": TXID_A,
                "vsize": 1,
                "time": 1,
                "fees": { "base": 0.000000001 }
            } }),
        ] {
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
            TXID_A: {
                "wtxid": TXID_A,
                "vsize": 1,
                "time": 1,
                "fees": { "base": 0.00000001 }
            },
            TXID_B: {
                "wtxid": TXID_B,
                "vsize": 2,
                "time": 2,
                "fees": { "base": 0.00000002 }
            }
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
