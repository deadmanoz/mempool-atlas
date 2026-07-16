# Mempool Atlas

Mempool Atlas is a standalone Bitcoin mempool observation and visualization tool whose primary product view is one selected node's mempool. Extensible transaction classification is required for the MVP. Its first deployment may place an optional Core and Knots comparison workspace in front for the 2026 fork-monitoring work, but comparison remains a derived consumer of independent source snapshots and the code and data model must remain implementation-neutral.

## Architecture

- `apps/atlas-agent/` owns peer-observer protobuf decoding, live NATS capture, the inbound-by-default P2P volume policy, periodic RPC reconciliation, the effective local projection, strict FIFO single-event and bounded reconciliation-batch HTTP delivery, and the source-bound SQLite outbox.
- `apps/atlas-server/` owns the implemented idempotent single and atomic batch ingest, source-partitioned central SQLite state, and source-scoped read API. Server-side stream delivery is not implemented.
- `crates/atlas-classifiers/` defines the required MVP classifier contract and will contain in-process rule packs. It is not a dynamic plugin system.
- `crates/atlas-model/` contains shared wire and domain types. It must not depend on agent or server internals.
- `proto/peer-observer/` vendors the minimal canonical peer-observer protobuf import closure at a recorded upstream commit.
- `web/` is a dependency-free TypeScript/Vite client for one selected source. Its primary overview uses Canvas 2D so large mempools do not create one DOM element per transaction.

The architectural rule is that RPC reconciliation is the state plane and peer-observer is the low-latency evidence plane. Missing peer-observer events may reduce forensic detail, but must not leave current membership permanently incorrect. Delivery is independent of both inputs, and capture enqueue is ordered against RPC snapshot reconciliation by one shared projection fence. The local projection distinguishes absent, present while awaiting RPC facts, and present with fact-bearing RPC state.

One source snapshot is the primary read model. Shared storage may co-locate independent source projections, but Atlas never constructs a combined cross-source mempool. A future comparison workspace must fetch two or more source snapshots and derive its result without mutating or owning them.

Full mode retains all supported mempool-subject evidence. P2P transaction capture defaults to explicitly inbound observations, preserving peer evidence from before node admission or rejection; `ATLAS_P2P_POLICY=all` enables the full P2P transaction relay model for a bounded diagnostic run. This is a volume policy over peer sightings, not transaction deduplication, and the generic observation model must remain capable of representing both policies.

## Key Dependencies

- `async-nats` consumes peer-observer's Core NATS `mempool` and `netmsg` subjects.
- `corepc-client` provides the synchronous Bitcoin RPC client used for verbose `getrawmempool`; keep it behind `spawn_blocking` in async code.
- `bitcoin` validates transaction identifiers and deserializes node-reported BTC amounts exactly before conversion to integer satoshis.
- `reqwest` delivers normalized outbox events to the central Atlas HTTP ingest API over Rustls.
- `rusqlite` backs both the central projection and the node-local durable outbox.

## Build & Test

Use `just` targets when one exists:

- `just build` builds the Rust workspace and web client.
- `just test` runs Rust and web tests.
- `just test-baseline-scale` runs the explicit 200,000-transaction reconciliation-batch acceptance test in release mode.
- `just lint` runs formatting checks, Clippy, and TypeScript checks.
- `just proto-check` verifies the vendored peer-observer schemas against the recorded local commit.
- `just format` formats Rust and the web client.
- `just dev` starts the local API server.
- `just agent-dev` starts the node-local agent using its configured environment.
- `just web-dev` starts Vite in a second terminal.
- `just db-migrate-dev` applies migrations through the backup-first wrapper.
- `just db-reinitialize-dev` backs up and replaces a stale preproduction central database with the current schema generation.
- `just db-backup` backs up the configured SQLite database.
- `just agent-db-migrate-dev` applies node-local outbox migrations through the backup-first wrapper.
- `just agent-db-reinitialize-dev` backs up and replaces a stale preproduction outbox with the current schema generation.
- `just agent-db-backup` backs up the configured node-local SQLite database.
- `just clean` removes generated build output.

## Conventions

- Treat `received`, `admitted`, `currently in mempool`, `rejected`, and `classified` as independent statements.
- Absence from a source is not evidence that the source rejected a transaction.
- Membership is keyed by `txid`; raw witness variants are distinguished by `wtxid`.
- Convert peer-observer hash byte arrays through rust-bitcoin hash types. Do not display them by directly hex-encoding the byte array.
- A peer-observer replacement event opens membership for `replacement_id` only when `replaced_by_transaction` is true. Otherwise it is a package hash.
- Store explicit `unknown` or `pending` states when evidence is unavailable. Do not infer facts to fill gaps.
- Treat node-reported mempool entry time as source state, not Atlas observation time. Derive individual base fee rate from `fee_sats / vsize` only for presentation; it is not ancestor-package mining priority.
- Treat capture-gap state as historical evidence integrity, not current liveness. RPC may repair current membership but cannot recreate missing peer-observer history. No reported gap is not proof of completeness.
- Keep source IDs and fork presets configurable. Never commit private hostnames, credentials, peer addresses, or deployment inventory.
- Current preproduction schema generations intentionally reject stale databases instead of carrying cross-generation logic. Replace stale state only through the backup-first `reinitialize` targets and reinitialize central state plus every source outbox together. Once production begins, SQLite migrations become append-only.

## Repository Etiquette

- Track multi-session work in Beads and keep the active issue notes resumable.
- Commit only when explicitly requested.
- Commit messages use conventional format and contain no agent attribution.
- Keep changes scoped to the current Bead. Record side work as a discovered Bead instead of expanding the active feature.

## Gotchas

- Live peer-observer NATS payloads are unframed protobuf `Event` messages on flat subjects. They carry neither the Atlas source ID nor an event sequence, so the agent must add both.
- Core NATS is not a durable queue and provides no causal ordering across `mempool` and `netmsg`. Subscribe and flush before consuming, persist slow-consumer drops as known forensic evidence loss, treat disconnects as possible evidence loss, then let RPC repair membership gaps without clearing the capture history.
- peer-observer mempool events carry only `txid`; P2P transaction events can also carry `wtxid` and raw bytes.
- `ATLAS_P2P_POLICY` accepts `inbound` or `all` and defaults to `inbound`. The policy affects only P2P transaction observations; never apply it to peer-observer mempool evidence.
- Core and Knots verbose mempool responses differ outside the required subset. Decode only `vsize`, `time`, and `fees.base`, ignore unrelated fields, and fail a snapshot that omits, malforms, or exceeds the exact transport range for a required fact.
- The guaranteed operating floor is RPC-only. Linux eBPF, BPF permissions, and USDT-enabled node binaries are required for the richer peer-observer feed.
- Run one agent writer and one persistent agent database per source. The database is source-bound and contains pending evidence plus the effective projection; deleting it is not a supported reset path. A coordinated backup-first preproduction reinitialization of central state and every source outbox is the only current exception.
- Outbox delivery is strict FIFO. Live and other non-reconciliation evidence is delivered singly. A contiguous `mempool_reconciled` prefix may contain at most 512 events and 4 MiB of encoded JSON.
- Remove a single head or reconciliation prefix only after HTTP 202 with complete acknowledgements for the same event IDs in the same order. The server must apply a batch in one transaction so a lost response can safely retry the whole prefix.

## Documentation

### Reference (`agent_docs/`)

- `agent_docs/README.md` explains the reference-document boundary and lists current references.
- `agent_docs/agent-runtime.md` documents agent modes, configuration, persistence, ordering, delivery, and recovery invariants.
- `agent_docs/peer-observer-wire.md` documents the pinned protobuf, NATS, identifier, and archive contracts.

### Refreshing Docs

`agent_docs/.docs-ref` stores the commit hash against which reference docs were last validated. Run `bash /Users/anthonymilton/dev/agent-skills/manage-agent-docs-skills/scripts/refresh-agent-docs.sh .` after architectural changes, and add `--update` only after reviewing the reported diff.
