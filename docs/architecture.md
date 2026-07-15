# Architecture

The self-contained [system architecture visualisation](mempool-atlas-system.html) shows the implemented node-local data path, source-partitioned central state, and the planned comparison and classification layer.

Mempool Atlas is divided into two Rust process boundaries and one browser client.

## Node-local agent

Each `atlas-agent` instance is bound to one logical source. In `full` mode it subscribes to peer-observer's unframed protobuf events on the Core NATS `mempool` and `netmsg` subjects. All supported mempool evidence is retained. P2P transaction capture defaults to explicitly inbound observations, preserving peer evidence from before node admission or rejection; `ATLAS_P2P_POLICY=all` retains the full P2P transaction relay stream for a bounded diagnostic run. This is a volume policy over peer sightings, not transaction deduplication, and the observation model remains extensible enough to represent either policy. Supported observations are assigned the configured source ID, a per-process source session, and a transactionally allocated local sequence before they enter the durable SQLite outbox. In `rpc-only` mode the NATS input is omitted without claiming that the missing forensic evidence exists.

The outbox also stores an effective local membership projection. Every queued membership mutation updates that projection in the same SQLite transaction, even if central delivery is still pending. Periodic non-verbose `getrawmempool` snapshots are compared with this effective projection and produce explicit `mempool_reconciled` corrections. RPC therefore repairs current membership, while peer-observer contributes lower-latency timing and evidence that polling cannot recover, including organic rejection and short-lived transaction events.

Capture enqueue and RPC snapshot reconciliation share a projection fence. The RPC side holds it from immediately before snapshot formation until the resulting corrections commit. This prevents an older snapshot from overtaking a newer peer-observer mutation. The fence does not hold a SQLite transaction open during the network RPC call.

Delivery is an independent runtime capability. It starts from the persisted FIFO head even when NATS or RPC is unavailable, while the input capabilities reconnect separately. Live and other non-reconciliation evidence continues through the single-event endpoint. A contiguous prefix of `mempool_reconciled` events is transported as a batch of at most 512 events and 4 MiB. This changes transport granularity only: every event keeps its source, session, sequence, and evidence identity, and later live evidence never overtakes the prefix.

The server applies a reconciliation batch sequentially in one SQLite transaction and returns one ordered acknowledgement per event. The agent atomically removes the prefix only after a complete same-order HTTP 202 response. Network errors, non-202 responses, malformed or mismatched acknowledgements, and server conflicts retain the entire prefix. A crash or lost response after central commit retries the same events, which central ingest recognizes as duplicates. No new checkpoint or source-reset semantics are introduced.

Periodic capture accounting makes the volume policy observable by reporting persisted and policy-suppressed observations with the pending outbox depth. An intentional outbound suppression is not reported as evidence loss; slow-consumer drops remain a separate degraded-capture signal.

The agent database is part of the source's durable identity and must use persistent storage. One agent writer and one database are allowed per source, and an existing database refuses a different source ID. Losing it discards pending evidence and the effective projection, so deleting it is not a supported reset path. An explicit protocol for resetting or rebaselining central source state after database loss remains deferred.

## Central service and browser

`atlas-server` ingests normalized events idempotently through a single-event endpoint and an atomic reconciliation-batch endpoint, reduces current membership into SQLite, and exposes read-only browser data. The browser fetches a membership checkpoint, and its isolated state reducer rejects sequence gaps in preparation for streamed deltas.

Central storage is shared operationally, not semantically. Every event remains labelled with its `source_id`, and `current_membership` is keyed by `(source_id, txid)`, so the same transaction in Core and Knots produces two independent membership records. Transaction variants can be deduplicated by `wtxid` because the serialized transaction is an intrinsic fact rather than source state.

There is no canonical Atlas mempool. Shared, source-only, and divergent sets are derived comparison views over a selected group of source projections. A short-lived fork therefore does not require Atlas to manufacture or persist mixed membership state.

The shared model never hard-codes Core, Knots, fork heights, or deployment hostnames. Fork-specific source profiles and optional classifiers are configuration layered on top of the generic observation model.

## Operations

Both central and agent SQLite migrations run through `scripts/migrate-safe.sh`, which creates and validates a backup before changing an existing database. Use `just db-migrate-dev` for the central service and `just agent-db-migrate-dev` for the node-local agent. After configuring the required environment values in an untracked `.env`, start them with `just dev` and `just agent-dev` respectively. Deploy a batch-capable server before upgrading an agent; the newer server retains the single endpoint for older agents.
