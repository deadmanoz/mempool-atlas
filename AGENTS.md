# Mempool Atlas

Mempool Atlas is a standalone Bitcoin mempool observation and visualization tool. Its first deployment compares Core and Knots for the 2026 fork-monitoring work, but the code and data model must remain implementation-neutral.

## Architecture

- `apps/atlas-agent/` currently owns peer-observer protobuf decoding and node-local normalization. Live NATS, RPC reconciliation, and its durable outbox belong at this boundary when implemented.
- `apps/atlas-server/` owns the implemented idempotent ingest, central SQLite state, and read API. The browser checkpoint/delta reducer exists, but server-side stream delivery does not yet.
- `crates/atlas-model/` contains shared wire and domain types. It must not depend on agent or server internals.
- `crates/atlas-classifiers/` contains built-in classifiers. It is an in-process trait boundary, not a dynamic plugin system.
- `proto/peer-observer/` vendors the minimal canonical peer-observer protobuf import closure at a recorded upstream commit.
- `web/` is a small TypeScript/Vite client. Keep framework and rendering complexity out until the data path is proven.

The architectural rule is that RPC reconciliation is the state plane and peer-observer is the low-latency evidence plane. Missing peer-observer events may reduce forensic detail, but must not leave current membership permanently incorrect.

## Build & Test

Use `just` targets when one exists:

- `just build` builds the Rust workspace and web client.
- `just test` runs Rust and web tests.
- `just lint` runs formatting checks, Clippy, and TypeScript checks.
- `just proto-check` verifies the vendored peer-observer schemas against the recorded local commit.
- `just format` formats Rust and the web client.
- `just dev` starts the local API server.
- `just web-dev` starts Vite in a second terminal.
- `just db-migrate-dev` applies migrations through the backup-first wrapper.
- `just db-backup` backs up the configured SQLite database.
- `just clean` removes generated build output.

## Conventions

- Treat `received`, `admitted`, `currently in mempool`, `rejected`, and `classified` as independent statements.
- Absence from a source is not evidence that the source rejected a transaction.
- Membership is keyed by `txid`; raw witness variants are distinguished by `wtxid`.
- Convert peer-observer hash byte arrays through rust-bitcoin hash types. Do not display them by directly hex-encoding the byte array.
- A peer-observer replacement event opens membership for `replacement_id` only when `replaced_by_transaction` is true. Otherwise it is a package hash.
- Store explicit `unknown` or `pending` states when evidence is unavailable. Do not infer facts to fill gaps.
- Keep source IDs and fork presets configurable. Never commit private hostnames, credentials, peer addresses, or deployment inventory.
- SQLite migrations are append-only once used against persistent data. Run them only through the backup-first `just` targets.

## Repository Etiquette

- Track multi-session work in Beads and keep the active issue notes resumable.
- Commit only when explicitly requested.
- Commit messages use conventional format and contain no agent attribution.
- Keep changes scoped to the current Bead. Record side work as a discovered Bead instead of expanding the active feature.

## Gotchas

- Live peer-observer NATS payloads are unframed protobuf `Event` messages on flat subjects. They carry neither the Atlas source ID nor an event sequence, so the agent must add both.
- peer-observer mempool events carry only `txid`; P2P transaction events can also carry `wtxid` and raw bytes.
- Core and Knots RPC response shapes may differ. Decode the required subset leniently and preserve an explicit capability state.
- The guaranteed operating floor is RPC-only. Linux eBPF, BPF permissions, and USDT-enabled node binaries are required for the richer peer-observer feed.

## Documentation

### Reference (`agent_docs/`)

- `agent_docs/README.md` explains the reference-document boundary and lists current references.
- `agent_docs/peer-observer-wire.md` documents the pinned protobuf, NATS, identifier, and archive contracts.

### Refreshing Docs

`agent_docs/.docs-ref` stores the commit hash against which reference docs were last validated. Run `bash /Users/anthonymilton/dev/agent-skills/manage-agent-docs-skills/scripts/refresh-agent-docs.sh .` after architectural changes, and add `--update` only after reviewing the reported diff.
