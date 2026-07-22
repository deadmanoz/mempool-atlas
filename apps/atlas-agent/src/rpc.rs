//! Bitcoin node RPC access for authoritative mempool reconciliation.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use atlas_model::{MAX_CHECKPOINT_ENTRIES, MempoolEntryFacts};
use bitcoin::{Amount, Txid};
use corepc_client::client_sync::v28::Client;
use corepc_client::client_sync::{Auth, Error as CorepcError};
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use thiserror::Error;

thread_local! {
    /// `corepc-client::Client::call` owns deserialization and does not expose a
    /// `DeserializeSeed`. The synchronous call and its deserializer run on the
    /// same blocking thread, so a scoped thread-local carries the configured
    /// entry cap into `RawMempoolVisitor` without a process-global race.
    static RAW_MEMPOOL_ENTRY_LIMIT: Cell<u64> = const { Cell::new(MAX_CHECKPOINT_ENTRIES) };
}

#[derive(Clone, Debug)]
pub struct RpcClient {
    inner: Arc<Client>,
    max_mempool_entries: u64,
}

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("failed to create Bitcoin RPC client: {0}")]
    ClientInitialization(#[source] CorepcError),
    #[error("getrawmempool RPC failed or returned an invalid response: {0}")]
    GetRawMempool(#[source] CorepcError),
    #[error("getmempoolinfo RPC failed or returned an invalid response: {0}")]
    GetMempoolInfo(#[source] CorepcError),
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
    #[error("getrawmempool returned invalid txid {txid:?}")]
    InvalidTxid { txid: String },
    #[error("getrawmempool returned duplicate canonical txid {txid}")]
    DuplicateTxid { txid: String },
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
        max_mempool_entries: u64,
    ) -> Result<(Self, BTreeMap<String, MempoolEntryFacts>), RpcError> {
        validate_configured_limit(max_mempool_entries)?;
        let inner = Client::new_with_auth(url, Auth::UserPass(username.into(), password.into()))
            .map_err(RpcError::ClientInitialization)?;
        let client = Self {
            inner: Arc::new(inner),
            max_mempool_entries,
        };
        let initial_snapshot = client.get_mempool_snapshot().await?;
        Ok((client, initial_snapshot))
    }

    /// Fetches the current mempool and the entry facts common to supported nodes.
    pub async fn get_mempool_snapshot(
        &self,
    ) -> Result<BTreeMap<String, MempoolEntryFacts>, RpcError> {
        let client = Arc::clone(&self.inner);
        let maximum = self.max_mempool_entries;
        tokio::task::spawn_blocking(move || {
            // `corepc-client` buffers JSON-RPC responses before deserializing
            // them. Reject an oversized node-reported mempool through the
            // small `getmempoolinfo` response before asking for the verbose
            // map, then enforce the limit again after the inherently racy
            // second call.
            let info = client
                .call::<MempoolInfoWire>("getmempoolinfo", &[])
                .map_err(RpcError::GetMempoolInfo)?;
            validate_reported_mempool_size(info.size, maximum)?;
            let _decode_limit = DecodeEntryLimitGuard::set(maximum);
            let response = client
                .call::<RawMempoolWire>("getrawmempool", &[serde_json::Value::Bool(true)])
                .map_err(RpcError::GetRawMempool)?;
            response.into_snapshot(maximum)
        })
        .await?
    }
}

#[derive(Debug)]
struct RawMempoolWire {
    snapshot: Result<BTreeMap<String, MempoolEntryFacts>, RpcError>,
}

#[derive(Debug, Deserialize)]
struct MempoolInfoWire {
    size: i64,
}

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
    fn into_snapshot(
        self,
        max_mempool_entries: u64,
    ) -> Result<BTreeMap<String, MempoolEntryFacts>, RpcError> {
        let snapshot = self.snapshot?;
        enforce_snapshot_limit(snapshot.len(), max_mempool_entries)?;
        Ok(snapshot)
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

impl<'de> Visitor<'de> for RawMempoolVisitor {
    type Value = RawMempoolWire;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a verbose getrawmempool object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut snapshot = BTreeMap::new();
        let mut validation_error = None;
        let mut observed_entries = 0_u64;
        let maximum = RAW_MEMPOOL_ENTRY_LIMIT.get();

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

            let entry = map.next_value::<RawMempoolEntryWire>()?;
            match decode_entry(txid, entry) {
                Ok((txid, facts)) => {
                    if snapshot.insert(txid.clone(), facts).is_some() {
                        validation_error = Some(RpcError::DuplicateTxid { txid });
                    }
                }
                Err(error) => validation_error = Some(error),
            }
        }

        Ok(RawMempoolWire {
            snapshot: validation_error.map_or(Ok(snapshot), Err),
        })
    }
}

fn decode_entry(
    value: String,
    entry: RawMempoolEntryWire,
) -> Result<(String, MempoolEntryFacts), RpcError> {
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
    Ok((txid, facts))
}

fn validate_configured_limit(maximum: u64) -> Result<(), RpcError> {
    if maximum == 0 || maximum > MAX_CHECKPOINT_ENTRIES {
        return Err(RpcError::InvalidMempoolEntryLimit {
            configured: maximum,
            maximum: MAX_CHECKPOINT_ENTRIES,
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

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    const TXID_A: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const TXID_B: &str = "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f";

    fn decode(value: Value) -> Result<BTreeMap<String, MempoolEntryFacts>, RpcError> {
        serde_json::from_value::<RawMempoolWire>(value)
            .expect("supported wire response")
            .into_snapshot(MAX_CHECKPOINT_ENTRIES)
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

    #[test]
    fn rejects_snapshot_above_configured_entry_limit() {
        let _decode_limit = DecodeEntryLimitGuard::set(1);
        let wire = serde_json::from_value::<RawMempoolWire>(json!({
            TXID_A: { "vsize": 1, "time": 1, "fees": { "base": 0.00000001 } },
            TXID_B: { "vsize": 2, "time": 2, "fees": { "base": 0.00000002 } }
        }))
        .expect("valid verbose response");

        assert!(matches!(
            wire.into_snapshot(1),
            Err(RpcError::SnapshotTooLarge {
                actual: 2,
                maximum: 1
            })
        ));
    }

    #[test]
    fn rejects_invalid_or_oversized_mempool_info_before_verbose_fetch() {
        let minimal = serde_json::from_value::<MempoolInfoWire>(json!({ "size": 10 }))
            .expect("only size is required from Core or Knots");
        assert_eq!(minimal.size, 10);
        assert!(serde_json::from_value::<MempoolInfoWire>(json!({ "size": "10" })).is_err());

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
        validate_reported_mempool_size(10, 10).expect("limit is inclusive");
    }

    #[test]
    fn rejects_duplicate_txids_after_canonicalization() {
        let response = json!({
            TXID_A: { "vsize": 1, "time": 1, "fees": { "base": 0.00000001 } },
            TXID_A.to_ascii_uppercase(): {
                "vsize": 2,
                "time": 2,
                "fees": { "base": 0.00000002 }
            }
        });

        assert!(matches!(
            serde_json::from_value::<RawMempoolWire>(response)
                .expect("wire shape")
                .into_snapshot(MAX_CHECKPOINT_ENTRIES),
            Err(RpcError::DuplicateTxid { txid }) if txid == TXID_A
        ));
    }
}
