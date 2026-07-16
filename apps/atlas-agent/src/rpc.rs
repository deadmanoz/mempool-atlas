//! Bitcoin node RPC access for authoritative mempool reconciliation.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;

use atlas_model::MempoolEntryFacts;
use bitcoin::{Amount, Txid};
use corepc_client::client_sync::v28::Client;
use corepc_client::client_sync::{Auth, Error as CorepcError};
use serde::Deserialize;
use thiserror::Error;

#[derive(Clone, Debug)]
pub struct RpcClient {
    inner: Arc<Client>,
}

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("failed to create Bitcoin RPC client: {0}")]
    ClientInitialization(#[source] CorepcError),
    #[error("getrawmempool RPC failed or returned an invalid response: {0}")]
    GetRawMempool(#[source] CorepcError),
    #[error("Bitcoin RPC worker task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
    #[error("getrawmempool returned invalid txid {txid:?}")]
    InvalidTxid { txid: String },
    #[error(
        "getrawmempool entry time {time_seconds} seconds overflows milliseconds for txid {txid}"
    )]
    EntryTimeOverflow { txid: String, time_seconds: u64 },
    #[error("getrawmempool returned invalid entry facts for txid {txid}: {source}")]
    InvalidEntryFacts {
        txid: String,
        #[source]
        source: atlas_model::ModelError,
    },
}

impl RpcClient {
    /// Connects to a node and returns its validated initial mempool snapshot.
    pub async fn connect(
        url: &str,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Result<(Self, BTreeMap<String, MempoolEntryFacts>), RpcError> {
        let inner = Client::new_with_auth(url, Auth::UserPass(username.into(), password.into()))
            .map_err(RpcError::ClientInitialization)?;
        let client = Self {
            inner: Arc::new(inner),
        };
        let initial_snapshot = client.get_mempool_snapshot().await?;
        Ok((client, initial_snapshot))
    }

    /// Fetches the current mempool and the entry facts common to supported nodes.
    pub async fn get_mempool_snapshot(
        &self,
    ) -> Result<BTreeMap<String, MempoolEntryFacts>, RpcError> {
        let client = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let response = client
                .call::<RawMempoolWire>("getrawmempool", &[serde_json::Value::Bool(true)])
                .map_err(RpcError::GetRawMempool)?;
            response.into_snapshot()
        })
        .await?
    }
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
struct RawMempoolWire(BTreeMap<String, RawMempoolEntryWire>);

#[derive(Debug, Deserialize)]
struct RawMempoolEntryWire {
    vsize: u64,
    time: u64,
    fees: RawMempoolFeesWire,
}

#[derive(Debug, Deserialize)]
struct RawMempoolFeesWire {
    #[serde(with = "bitcoin::amount::serde::as_btc")]
    base: Amount,
}

impl RawMempoolWire {
    fn into_snapshot(self) -> Result<BTreeMap<String, MempoolEntryFacts>, RpcError> {
        let mut snapshot = BTreeMap::new();
        for (value, entry) in self.0 {
            let txid = Txid::from_str(&value)
                .map_err(|_| RpcError::InvalidTxid { txid: value })?
                .to_string();
            let entered_at_ms =
                entry
                    .time
                    .checked_mul(1_000)
                    .ok_or_else(|| RpcError::EntryTimeOverflow {
                        txid: txid.clone(),
                        time_seconds: entry.time,
                    })?;
            let facts = MempoolEntryFacts {
                vsize: entry.vsize,
                fee_sats: entry.fees.base.to_sat(),
                entered_at_ms,
            };
            facts
                .validate()
                .map_err(|source| RpcError::InvalidEntryFacts {
                    txid: txid.clone(),
                    source,
                })?;
            snapshot.insert(txid, facts);
        }
        Ok(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    const TXID_A: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const TXID_B: &str = "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f";

    fn decode(value: Value) -> Result<BTreeMap<String, MempoolEntryFacts>, RpcError> {
        serde_json::from_value::<RawMempoolWire>(value)
            .expect("supported wire response")
            .into_snapshot()
    }

    #[test]
    fn decodes_core_verbose_response() {
        let snapshot = decode(json!({
            TXID_A: {
                "vsize": 141,
                "weight": 561,
                "time": 1_721_234_000_u64,
                "height": 850_000,
                "chunkweight": 561,
                "wtxid": TXID_B,
                "fees": {
                    "base": 0.00001200,
                    "modified": 0.00001200,
                    "ancestor": 0.00001200,
                    "descendant": 0.00001200,
                    "chunk": 0.00001200
                },
                "depends": [],
                "spentby": [],
                "bip125-replaceable": false,
                "unbroadcast": false
            }
        }))
        .expect("Core response");

        assert_eq!(
            snapshot,
            BTreeMap::from([(
                TXID_A.to_owned(),
                MempoolEntryFacts {
                    vsize: 141,
                    fee_sats: 1_200,
                    entered_at_ms: 1_721_234_000_000,
                },
            )])
        );
    }

    #[test]
    fn decodes_knots_verbose_response() {
        let snapshot = decode(json!({
            TXID_A: {
                "vsize": 222,
                "weight": 888,
                "time": 1_721_234_001_u64,
                "height": 850_000,
                "startingpriority": 0.0,
                "currentpriority": 0.0,
                "hash": TXID_B,
                "wtxid": TXID_B,
                "fees": {
                    "base": 0.00000001,
                    "modified": 0.00000001,
                    "ancestor": 0.00000001,
                    "descendant": 0.00000001
                },
                "depends": [],
                "spentby": [],
                "bip125-replaceable": false,
                "unbroadcast": false
            }
        }))
        .expect("Knots response");

        assert_eq!(snapshot[TXID_A].fee_sats, 1);
        assert_eq!(snapshot[TXID_A].vsize, 222);
        assert_eq!(snapshot[TXID_A].entered_at_ms, 1_721_234_001_000);
    }

    #[test]
    fn canonicalizes_and_orders_txids() {
        let snapshot = decode(json!({
            TXID_B: { "vsize": 2, "time": 2, "fees": { "base": 0.00000002 } },
            TXID_A.to_ascii_uppercase(): {
                "vsize": 1,
                "time": 1,
                "fees": { "base": 0.00000001 }
            }
        }))
        .expect("valid response");

        assert_eq!(
            snapshot.into_keys().collect::<Vec<_>>(),
            vec![TXID_A.to_owned(), TXID_B.to_owned()]
        );
    }

    #[test]
    fn rejects_invalid_txid() {
        assert!(matches!(
            decode(json!({
                "not-a-txid": {
                    "vsize": 1,
                    "time": 1,
                    "fees": { "base": 0.00000001 }
                }
            })),
            Err(RpcError::InvalidTxid { txid }) if txid == "not-a-txid"
        ));
    }

    #[test]
    fn rejects_missing_and_malformed_required_fields() {
        for response in [
            json!({ TXID_A: { "time": 1, "fees": { "base": 0.00000001 } } }),
            json!({ TXID_A: { "vsize": 1, "fees": { "base": 0.00000001 } } }),
            json!({ TXID_A: { "vsize": 1, "time": 1 } }),
            json!({ TXID_A: { "vsize": 1, "time": 1, "fees": {} } }),
            json!({ TXID_A: { "vsize": 1.5, "time": 1, "fees": { "base": 0.00000001 } } }),
            json!({ TXID_A: { "vsize": 1, "time": -1, "fees": { "base": 0.00000001 } } }),
            json!({ TXID_A: { "vsize": 1, "time": 1, "fees": { "base": -0.00000001 } } }),
            json!({ TXID_A: { "vsize": 1, "time": 1, "fees": { "base": 0.000000001 } } }),
        ] {
            assert!(serde_json::from_value::<RawMempoolWire>(response).is_err());
        }
    }

    #[test]
    fn rejects_non_verbose_response_shapes() {
        for response in [
            json!([TXID_A]),
            json!({ "txids": [TXID_A] }),
            json!({ "txids": [TXID_A], "mempool_sequence": 42 }),
        ] {
            assert!(serde_json::from_value::<RawMempoolWire>(response).is_err());
        }
    }

    #[test]
    fn rejects_entry_time_millisecond_overflow() {
        let time_seconds = u64::MAX / 1_000 + 1;
        assert!(matches!(
            decode(json!({
                TXID_A: {
                    "vsize": 1,
                    "time": time_seconds,
                    "fees": { "base": 0.00000001 }
                }
            })),
            Err(RpcError::EntryTimeOverflow { txid, time_seconds: actual })
                if txid == TXID_A && actual == time_seconds
        ));
    }

    #[test]
    fn rejects_zero_vsize_before_reconciliation() {
        assert!(matches!(
            decode(json!({
                TXID_A: {
                    "vsize": 0,
                    "time": 1,
                    "fees": { "base": 0.00000001 }
                }
            })),
            Err(RpcError::InvalidEntryFacts { txid, source: atlas_model::ModelError::ZeroVsize })
                if txid == TXID_A
        ));
    }

    #[test]
    fn rejects_non_transportable_facts_before_reconciliation() {
        assert!(matches!(
            decode(json!({
                TXID_A: {
                    "vsize": atlas_model::MAX_SAFE_JSON_INTEGER + 1,
                    "time": 1,
                    "fees": { "base": 0.00000001 }
                }
            })),
            Err(RpcError::InvalidEntryFacts {
                txid,
                source: atlas_model::ModelError::MempoolEntryFactTooLarge {
                    field: "vsize",
                    ..
                }
            }) if txid == TXID_A
        ));
    }
}
