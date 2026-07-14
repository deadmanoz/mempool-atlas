//! Decode peer-observer protobuf events into the Atlas domain model.

use atlas_model::{Evidence, NormalizedEvent, Replacement, SourceId, SourceSessionId};
use bitcoin::hashes::Hash;
use bitcoin::{Txid, Wtxid};
use prost::Message;
use thiserror::Error;

use self::wire::ebpf_extractor::ebpf::EbpfEvent;
use self::wire::ebpf_extractor::mempool::mempool_event;
use self::wire::ebpf_extractor::message::message_event;
use self::wire::event::event::PeerObserverEvent;

pub const MEMPOOL_SUBJECT: &str = "mempool";
pub const NETMSG_SUBJECT: &str = "netmsg";
pub const P2P_EXTRACTOR_SUBJECT: &str = "p2p-extractor";

#[derive(Debug, Error)]
pub enum PeerObserverError {
    #[error("invalid peer-observer protobuf: {0}")]
    Protobuf(#[from] prost::DecodeError),
    #[error("{field} must be 32 bytes, got {actual}")]
    InvalidHashLength { field: &'static str, actual: usize },
    #[error("invalid normalized event: {0}")]
    Model(#[from] atlas_model::ModelError),
}

/// Decodes one unframed NATS payload and normalizes supported evidence.
pub fn normalize_payload(
    payload: &[u8],
    source_id: &SourceId,
    source_session_id: &SourceSessionId,
    local_sequence: u64,
    received_at_ms: u64,
) -> Result<Option<NormalizedEvent>, PeerObserverError> {
    let event = wire::event::Event::decode(payload)?;
    normalize_event(
        &event,
        source_id,
        source_session_id,
        local_sequence,
        received_at_ms,
    )
}

/// Normalizes a decoded event. Unsupported peer-observer events are ignored.
pub fn normalize_event(
    event: &wire::event::Event,
    source_id: &SourceId,
    source_session_id: &SourceSessionId,
    local_sequence: u64,
    received_at_ms: u64,
) -> Result<Option<NormalizedEvent>, PeerObserverError> {
    let Some(PeerObserverEvent::EbpfExtractor(ebpf)) = event.peer_observer_event.as_ref() else {
        return Ok(None);
    };
    let Some(ebpf_event) = ebpf.ebpf_event.as_ref() else {
        return Ok(None);
    };

    let evidence = match ebpf_event {
        EbpfEvent::Mempool(mempool) => normalize_mempool(mempool)?,
        EbpfEvent::Message(message) => normalize_message(message)?,
        EbpfEvent::Connection(_) | EbpfEvent::Validation(_) => None,
    };

    evidence
        .map(|evidence| {
            NormalizedEvent::new(
                source_id.clone(),
                source_session_id.clone(),
                local_sequence,
                event.timestamp,
                received_at_ms,
                evidence,
            )
            .map_err(PeerObserverError::from)
        })
        .transpose()
}

fn normalize_mempool(
    event: &wire::ebpf_extractor::mempool::MempoolEvent,
) -> Result<Option<Evidence>, PeerObserverError> {
    let Some(event) = event.event.as_ref() else {
        return Ok(None);
    };

    let evidence = match event {
        mempool_event::Event::Added(added) => Evidence::MempoolAdded {
            txid: txid_string("mempool.added.txid", &added.txid)?,
        },
        mempool_event::Event::Removed(removed) => Evidence::MempoolRemoved {
            txid: txid_string("mempool.removed.txid", &removed.txid)?,
            reason: Some(removed.reason.clone()),
        },
        mempool_event::Event::Rejected(rejected) => Evidence::MempoolRejected {
            txid: txid_string("mempool.rejected.txid", &rejected.txid)?,
            reason: rejected.reason.clone(),
        },
        mempool_event::Event::Replaced(replaced) => {
            let replacement = if replaced.replaced_by_transaction {
                Replacement::Transaction {
                    txid: txid_string("mempool.replaced.replacement_id", &replaced.replacement_id)?,
                }
            } else {
                Replacement::PackageHash {
                    hash: txid_string(
                        "mempool.replaced.replacement_package_hash",
                        &replaced.replacement_id,
                    )?,
                }
            };
            Evidence::MempoolReplaced {
                replaced_txid: txid_string(
                    "mempool.replaced.replaced_txid",
                    &replaced.replaced_txid,
                )?,
                replacement,
            }
        }
    };

    Ok(Some(evidence))
}

fn normalize_message(
    event: &wire::ebpf_extractor::message::MessageEvent,
) -> Result<Option<Evidence>, PeerObserverError> {
    let Some(message_event::Msg::Tx(tx)) = event.msg.as_ref() else {
        return Ok(None);
    };

    Ok(Some(Evidence::P2pTransaction {
        txid: txid_string("netmsg.tx.txid", &tx.tx.txid)?,
        wtxid: wtxid_string("netmsg.tx.wtxid", &tx.tx.wtxid)?,
        raw_transaction_hex: tx.tx.raw.as_ref().map(hex::encode),
        peer_id: Some(event.meta.peer_id),
        inbound: Some(event.meta.inbound),
    }))
}

fn txid_string(field: &'static str, bytes: &[u8]) -> Result<String, PeerObserverError> {
    if bytes.len() != 32 {
        return Err(PeerObserverError::InvalidHashLength {
            field,
            actual: bytes.len(),
        });
    }
    Ok(Txid::from_slice(bytes)
        .expect("length checked above")
        .to_string())
}

fn wtxid_string(field: &'static str, bytes: &[u8]) -> Result<String, PeerObserverError> {
    if bytes.len() != 32 {
        return Err(PeerObserverError::InvalidHashLength {
            field,
            actual: bytes.len(),
        });
    }
    Ok(Wtxid::from_slice(bytes)
        .expect("length checked above")
        .to_string())
}

#[allow(clippy::all)]
pub mod wire {
    pub mod bitcoin_primitives {
        include!(concat!(env!("OUT_DIR"), "/bitcoin_primitives.rs"));
    }

    pub mod ebpf_extractor {
        pub mod connection {
            include!(concat!(env!("OUT_DIR"), "/ebpf_extractor.connection.rs"));
        }
        pub mod mempool {
            include!(concat!(env!("OUT_DIR"), "/ebpf_extractor.mempool.rs"));
        }
        pub mod message {
            include!(concat!(env!("OUT_DIR"), "/ebpf_extractor.message.rs"));
        }
        pub mod validation {
            include!(concat!(env!("OUT_DIR"), "/ebpf_extractor.validation.rs"));
        }
        include!(concat!(env!("OUT_DIR"), "/ebpf_extractor.rs"));
    }

    pub mod event {
        include!(concat!(env!("OUT_DIR"), "/event.rs"));
    }

    pub mod header {
        include!(concat!(env!("OUT_DIR"), "/header.rs"));
    }

    pub mod ipc_extractor {
        include!(concat!(env!("OUT_DIR"), "/ipc_extractor.rs"));
    }

    pub mod log_extractor {
        include!(concat!(env!("OUT_DIR"), "/log_extractor.rs"));
    }

    pub mod p2p_extractor {
        include!(concat!(env!("OUT_DIR"), "/p2p_extractor.rs"));
    }

    pub mod rpc_extractor {
        include!(concat!(env!("OUT_DIR"), "/rpc_extractor.rs"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer_observer::wire::bitcoin_primitives::Transaction;
    use crate::peer_observer::wire::ebpf_extractor::Ebpf;
    use crate::peer_observer::wire::ebpf_extractor::ebpf;
    use crate::peer_observer::wire::ebpf_extractor::mempool::{
        Added, MempoolEvent, Rejected, Removed, Replaced,
    };
    use crate::peer_observer::wire::ebpf_extractor::message::{
        MessageEvent, Metadata, Tx, message_event::Msg,
    };
    use crate::peer_observer::wire::event::Event;
    use crate::peer_observer::wire::event::event::PeerObserverEvent;

    const OBSERVED_AT_MS: u64 = 1_721_234_567_890;
    const RECEIVED_AT_MS: u64 = OBSERVED_AT_MS + 7;
    const TXID: &str = "1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100";
    const WTXID: &str = "3f3e3d3c3b3a393837363534333231302f2e2d2c2b2a29282726252423222120";

    fn hash_bytes(start: u8) -> Vec<u8> {
        (start..start + 32).collect()
    }

    fn mempool_event(event: mempool_event::Event) -> Event {
        Event {
            timestamp: OBSERVED_AT_MS,
            peer_observer_event: Some(PeerObserverEvent::EbpfExtractor(Ebpf {
                ebpf_event: Some(ebpf::EbpfEvent::Mempool(MempoolEvent {
                    event: Some(event),
                })),
            })),
        }
    }

    fn normalized(event: &Event) -> NormalizedEvent {
        normalize_event(
            event,
            &SourceId::new("core-a").expect("source"),
            &SourceSessionId::new("session-a").expect("session"),
            42,
            RECEIVED_AT_MS,
        )
        .expect("normalization")
        .expect("supported event")
    }

    #[test]
    fn decodes_checked_unframed_fixture() {
        let event = normalize_payload(
            include_bytes!("../../../fixtures/peer-observer/mempool-added.pb"),
            &SourceId::new("core-a").expect("source"),
            &SourceSessionId::new("session-a").expect("session"),
            42,
            RECEIVED_AT_MS,
        )
        .expect("fixture decode")
        .expect("supported fixture");

        assert_eq!(event.event_id, "core-a/session-a/42");
        assert_eq!(
            event.evidence,
            Evidence::MempoolAdded {
                txid: TXID.to_owned(),
            }
        );
    }

    #[test]
    fn normalizes_mempool_added() {
        let event = mempool_event(mempool_event::Event::Added(Added {
            txid: hash_bytes(0),
            vsize: 453,
            fee: 123,
        }));

        let event = normalized(&event);
        assert_eq!(event.observed_at_ms, OBSERVED_AT_MS);
        assert_eq!(event.received_at_ms, RECEIVED_AT_MS);
        assert_eq!(
            event.evidence,
            Evidence::MempoolAdded {
                txid: TXID.to_owned(),
            }
        );
    }

    #[test]
    fn normalizes_mempool_removed() {
        let event = normalized(&mempool_event(mempool_event::Event::Removed(Removed {
            txid: hash_bytes(0),
            reason: "block".to_owned(),
            vsize: 100,
            fee: 250,
            entry_time: 1_721_234_000,
        })));

        assert_eq!(
            event.evidence,
            Evidence::MempoolRemoved {
                txid: TXID.to_owned(),
                reason: Some("block".to_owned()),
            }
        );
    }

    #[test]
    fn normalizes_mempool_rejected() {
        let event = normalized(&mempool_event(mempool_event::Event::Rejected(Rejected {
            txid: hash_bytes(0),
            reason: "min relay fee not met".to_owned(),
        })));

        assert_eq!(
            event.evidence,
            Evidence::MempoolRejected {
                txid: TXID.to_owned(),
                reason: "min relay fee not met".to_owned(),
            }
        );
    }

    #[test]
    fn transaction_replacement_opens_replacement_membership() {
        let event = normalized(&mempool_event(mempool_event::Event::Replaced(Replaced {
            replaced_txid: hash_bytes(0),
            replaced_vsize: 100,
            replaced_fee: 500,
            replaced_entry_time: 1_721_234_000,
            replacement_id: hash_bytes(32),
            replacement_vsize: 120,
            replacement_fee: 800,
            replaced_by_transaction: true,
        })));

        assert_eq!(
            event.evidence,
            Evidence::MempoolReplaced {
                replaced_txid: TXID.to_owned(),
                replacement: Replacement::Transaction {
                    txid: WTXID.to_owned(),
                },
            }
        );
        assert_eq!(event.membership_mutations().len(), 2);
    }

    #[test]
    fn package_hash_replacement_does_not_open_membership() {
        let event = normalized(&mempool_event(mempool_event::Event::Replaced(Replaced {
            replaced_txid: hash_bytes(0),
            replaced_vsize: 100,
            replaced_fee: 500,
            replaced_entry_time: 1_721_234_000,
            replacement_id: hash_bytes(32),
            replacement_vsize: 240,
            replacement_fee: 1_600,
            replaced_by_transaction: false,
        })));

        assert_eq!(
            event.evidence,
            Evidence::MempoolReplaced {
                replaced_txid: TXID.to_owned(),
                replacement: Replacement::PackageHash {
                    hash: WTXID.to_owned(),
                },
            }
        );
        let mutations = event.membership_mutations();
        assert_eq!(mutations.len(), 1);
        assert_eq!(mutations[0].txid, TXID);
        assert!(!mutations[0].present);
    }

    #[test]
    fn normalizes_p2p_transaction_with_raw_bytes() {
        let event = Event {
            timestamp: OBSERVED_AT_MS,
            peer_observer_event: Some(PeerObserverEvent::EbpfExtractor(Ebpf {
                ebpf_event: Some(ebpf::EbpfEvent::Message(MessageEvent {
                    meta: Metadata {
                        peer_id: 17,
                        addr: "127.0.0.1:8333".to_owned(),
                        conn_type: 1,
                        command: "tx".to_owned(),
                        inbound: true,
                        size: 2,
                    },
                    msg: Some(Msg::Tx(Tx {
                        tx: Transaction {
                            txid: hash_bytes(0),
                            wtxid: hash_bytes(32),
                            raw: Some(vec![0xde, 0xad]),
                        },
                    })),
                })),
            })),
        };

        assert_eq!(
            normalized(&event).evidence,
            Evidence::P2pTransaction {
                txid: TXID.to_owned(),
                wtxid: WTXID.to_owned(),
                raw_transaction_hex: Some("dead".to_owned()),
                peer_id: Some(17),
                inbound: Some(true),
            }
        );
    }

    #[test]
    fn rejects_non_hash_length_identifiers() {
        let event = mempool_event(mempool_event::Event::Added(Added {
            txid: vec![0; 31],
            vsize: 1,
            fee: 1,
        }));

        assert!(matches!(
            normalize_event(
                &event,
                &SourceId::new("core-a").expect("source"),
                &SourceSessionId::new("session-a").expect("session"),
                1,
                RECEIVED_AT_MS,
            ),
            Err(PeerObserverError::InvalidHashLength { actual: 31, .. })
        ));
    }
}
