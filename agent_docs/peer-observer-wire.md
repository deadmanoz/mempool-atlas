# peer-observer wire contract

Status: retained experimental input contract. The state-only production agent
does not currently subscribe to NATS. This reference remains authoritative for
the later bounded HotStore and EvidenceArchive implementation; it must not be
used to re-enable the retired generic evidence FIFO.

Atlas vendors the peer-observer protobuf schema at commit
`dbff1e37693716358fa414ea961894c16f9a962d`. The same value is recorded in
`proto/peer-observer/PINNED_COMMIT`.

## Vendored schema

`event.proto` imports every peer-observer event family, so its canonical import
closure is small but broader than the events Atlas currently normalizes:

- `event.proto`
- `bitcoin_primitives.proto`
- `ebpf_extractor.proto`
- `ebpf_extractor/connection.proto`
- `ebpf_extractor/mempool.proto`
- `ebpf_extractor/message.proto`
- `ebpf_extractor/validation.proto`
- `rpc_extractor.proto`
- `p2p_extractor.proto`
- `log_extractor.proto`
- `ipc_extractor.proto`

`archive/header.proto` is the additional root needed to read raw archives.
`apps/atlas-agent/build.rs` passes `event.proto` and `archive/header.proto` to
`prost-build`; their imports are compiled transitively.

## Live NATS payloads

The relevant flat subjects are:

| Subject | Content |
| --- | --- |
| `mempool` | eBPF mempool added, removed, replaced and rejected events |
| `netmsg` | eBPF P2P messages, including full `tx` messages |
| `p2p-extractor` | Inventory announced to peer-observer's dedicated P2P connection |

Each NATS payload is one unframed protobuf `event.Event`, encoded with
`prost::Message::encode_to_vec`. There is no length prefix or JSON envelope.
The payload contains neither an Atlas source ID nor an event sequence. The
agent supplies its configured source, source session, local sequence and local
receive timestamp during normalization.

The `Event.timestamp` field is milliseconds since the Unix epoch. P2P message
metadata describes the remote peer and direction relative to the observed
node. The dedicated `p2p-extractor` inventory event has no equivalent peer
metadata and is not a substitute for the eBPF `netmsg` stream.

peer-observer requires the NATS server's `max_payload` to be at least 5 MiB for
large P2P messages.

## Identifier semantics

Mempool events identify transactions by `txid`. A P2P `tx` event contains a
`txid`, `wtxid` and optional raw consensus bytes. Atlas converts peer-observer
hash byte arrays through `bitcoin::Txid` or `bitcoin::Wtxid`; directly encoding
the bytes as hexadecimal produces the wrong displayed identifier order.

A replaced event always closes membership for `replaced_txid`. It opens
membership for `replacement_id` only when `replaced_by_transaction` is true.
When false, `replacement_id` is a package hash and is retained as evidence but
never projected as transaction membership.

## Raw archive framing

The logical archive stream is:

```text
[protobuf varint length][ArchiveHeader]
[protobuf varint length][Event]
[protobuf varint length][Event]
...
```

`.bin.zst` wraps the complete logical stream in streaming zstd compression.
`.bin` stores it uncompressed. The first record is an `ArchiveHeader`, followed
by events until clean EOF. The archive contains no NATS subject, node ID,
schema version or record checksum, so the importer must receive source and
schema-pin metadata from its invocation.

## Fixture

`fixtures/peer-observer/mempool-added.pb` is one deterministic unframed live
payload. Regenerate it from the generated Rust types with:

```bash
cargo run -p atlas-agent --example generate_peer_observer_fixture
```

An upstream update changes the pin, vendored files and fixture together, then
runs the atlas-agent tests before deployment.
