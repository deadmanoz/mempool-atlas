//! Bitcoin node RPC access for authoritative mempool reconciliation.

use std::collections::BTreeSet;
use std::str::FromStr;
use std::sync::Arc;

use bitcoin::Txid;
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
    #[error("getrawmempool RPC failed or returned an incompatible response: {0}")]
    GetRawMempool(#[source] CorepcError),
    #[error("Bitcoin RPC worker task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
    #[error("getrawmempool returned invalid txid {txid:?}")]
    InvalidTxid { txid: String },
}

impl RpcClient {
    /// Connects to a node and returns its validated initial mempool snapshot.
    pub async fn connect(
        url: &str,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Result<(Self, BTreeSet<String>), RpcError> {
        let inner = Client::new_with_auth(url, Auth::UserPass(username.into(), password.into()))
            .map_err(RpcError::ClientInitialization)?;
        let client = Self {
            inner: Arc::new(inner),
        };
        let initial_mempool = client.get_raw_mempool().await?;
        Ok((client, initial_mempool))
    }

    /// Fetches the non-verbose mempool response common to supported nodes.
    pub async fn get_raw_mempool(&self) -> Result<BTreeSet<String>, RpcError> {
        let client = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let response = client
                .call::<RawMempoolWire>("getrawmempool", &[])
                .map_err(RpcError::GetRawMempool)?;
            response.into_txids()
        })
        .await?
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawMempoolWire {
    Array(Vec<String>),
    Object {
        txids: Vec<String>,
        #[serde(default, rename = "mempool_sequence")]
        _mempool_sequence: Option<u64>,
    },
}

impl RawMempoolWire {
    fn into_txids(self) -> Result<BTreeSet<String>, RpcError> {
        let txids = match self {
            Self::Array(txids) | Self::Object { txids, .. } => txids,
        };

        txids
            .into_iter()
            .map(|value| {
                Txid::from_str(&value)
                    .map(|txid| txid.to_string())
                    .map_err(|_| RpcError::InvalidTxid { txid: value })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    const TXID_A: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const TXID_B: &str = "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f";

    fn decode(value: Value) -> Result<BTreeSet<String>, RpcError> {
        serde_json::from_value::<RawMempoolWire>(value)
            .expect("supported wire response")
            .into_txids()
    }

    #[test]
    fn decodes_plain_array_response() {
        assert_eq!(
            decode(json!([TXID_A])).expect("valid response"),
            BTreeSet::from([TXID_A.to_owned()])
        );
    }

    #[test]
    fn decodes_object_response_with_optional_sequence() {
        for response in [
            json!({ "txids": [TXID_A] }),
            json!({ "txids": [TXID_A], "mempool_sequence": 42 }),
        ] {
            assert_eq!(
                decode(response).expect("valid response"),
                BTreeSet::from([TXID_A.to_owned()])
            );
        }
    }

    #[test]
    fn canonicalizes_deduplicates_and_orders_txids() {
        let txids =
            decode(json!([TXID_B, TXID_A.to_ascii_uppercase(), TXID_A])).expect("valid response");

        assert_eq!(
            txids.into_iter().collect::<Vec<_>>(),
            vec![TXID_A.to_owned(), TXID_B.to_owned()]
        );
    }

    #[test]
    fn rejects_invalid_txid() {
        assert!(matches!(
            decode(json!(["not-a-txid"])),
            Err(RpcError::InvalidTxid { txid }) if txid == "not-a-txid"
        ));
    }
}
