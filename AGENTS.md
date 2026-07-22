# Mempool Atlas

Mempool Atlas is a standalone Bitcoin mempool observation and visualisation tool. Its primary view is one selected source; optional comparison derives aggregates from independent source snapshots and never creates a combined mempool.

## Architecture

- `apps/atlas-agent/` owns the production RPC-only `SourceReplica`: source-bound SQLite identity, persistent epoch and revisions, complete verbose-RPC observations, bounded coalesced dirty state, one frozen delta or checkpoint, and independent HTTP delivery to `POST /api/v1/state`.
- `apps/atlas-server/` owns the idempotent state reducer, at most one active and one staging membership generation per source, atomic checkpoint activation, and all source-scoped product reads. Its production router does not mount legacy evidence ingest.
- `crates/atlas-model/` owns the versioned SourceReplica wire contract and shared read types. It must not depend on agent or server internals.
- `crates/atlas-storage/` owns the shared application-level SQLite capacity policy: main-database limits, DB/WAL/SHM envelope accounting, filesystem reserves, WAL pressure handling, and verified connection pragmas.
- `crates/atlas-classifiers/`, peer-observer decoding, and the legacy event reducer are experimental building blocks for the later bounded evidence products. They must not establish or mutate production membership.
- `web/` polls source summaries for the primary aggregate workbench. The full membership snapshot is fetched only for the on-demand Canvas inspector. Comparison is read-time SQL over active source generations.

RPC is the sole current-membership authority. First complete observation, including empty, creates revision 1. Changed snapshots advance once; unchanged snapshots update freshness. A persistent agent database retains its epoch across restart. A reset creates a new epoch and replaces the server's active cursor through a checkpoint.

State delivery never shares ordering or disk budget with evidence. Healthy changes use sorted deltas. Bootstrap, cursor conflict, epoch change, or dirty pressure uses a full checkpoint. The server stages, verifies, and atomically activates it; readers never see staging rows.

Production limits are layered. Each agent accepts at most 200,000 entries, with a 1 GiB main-database cap inside a 1.75 GiB DB/WAL/SHM admission envelope. The server accepts two persistent source identities and 200,000 entries per active or staging generation, with a 3 GiB main-database cap inside a 3.5 GiB envelope. Non-loopback server binds require an exact source allowlist. Canary deployment adds fixed 2 GiB agent and 4 GiB server filesystems plus 1 GiB no-swap memory cgroups as hard boundaries.

## Key Dependencies

- `corepc-client` provides synchronous `getmempoolinfo` preflight and verbose `getrawmempool`; keep both behind `spawn_blocking`.
- `reqwest` delivers SourceReplica requests over Rustls.
- `rusqlite` backs source-local and central state.
- `fs4` reports total and non-privileged available filesystem capacity for SQLite write admission.
- `bitcoin` validates identifiers and exact node facts.
- `async-nats` and `prost` remain for deferred peer evidence work, not the production runtime.

## Build & Test

Use `just` targets whenever one exists:

- `just build` builds the Rust workspace and web client.
- `just test` runs Rust and web tests.
- `just lint` runs formatting checks, Clippy, and TypeScript checks.
- `just test-baseline-scale` proves the 200,000-transaction agent-to-server checkpoint path in release mode.
- `just regen-api-fixtures` rewrites deterministic golden read fixtures; review the diff.
- `just seed-dev` and `just seed-forks` replace deterministic source snapshots through the real state endpoint.
- `just dev`, `just agent-dev`, and `just web-dev` run the server, one source agent, and Vite.
- `just db-migrate-dev` and `just agent-db-migrate-dev` apply schemas only through the backup-first wrapper.
- `just db-reinitialize-dev` and `just agent-db-reinitialize-dev` back up and replace stale preproduction databases.
- `just clean` removes generated build output.

## Conventions

- Treat received, admitted, currently present, rejected, and classified as independent claims.
- Absence from a source is not evidence of rejection, filtering, or relay causality.
- Membership is keyed by `txid`; raw witness variants use `wtxid`.
- Node mempool entry time is source state. `state_observed_at_ms` is the agent's completed RPC observation time.
- State-only reads must report `capture.status = not_collected`. Never infer forensic completeness.
- SourceReplica actions are immutable after freezing. Remove them only after an exact expected acknowledgement.
- Checkpoints bind replacement cursor, target cursor, counts, chunks, and canonical digest. Replacing an abandoned staging checkpoint requires its exact checkpoint ID as a compare-and-swap token. Staging is never reader-visible.
- Keep source IDs and fork presets configurable. Never commit credentials, hostnames, peer addresses, or deployment inventory.
- Clean preproduction schemas are central generation 8 and agent generation 5. Stale databases are replaced only through backup-first reinitialization. Production migrations become append-only once real data must be retained.

## Repository Etiquette

- Track multi-session work in Beads and keep active notes resumable.
- Commit only when explicitly requested. Use atomic conventional commits with no agent attribution.
- Keep changes scoped to the active Bead; record side work as discovered Beads.

## Gotchas

- The first baseline is a checkpoint even for an empty mempool.
- Deltas require an acknowledged base. A same-epoch server cursor ahead of local state rotates the local epoch before rebaseline.
- Dirty storage is coalesced by txid and capped by row and estimated-byte budgets. Pressure clears dirty detail and requires a checkpoint.
- Agent `max_page_count` bounds the main database only. The DB/WAL/SHM envelope is write-admission policy, not a filesystem quota. A frozen checkpoint can coexist with current membership, and a transaction can temporarily cross the 64 MiB WAL high-water target before relief and retry.
- The server holds at most one active and one staging generation per source. Deltas and heartbeats are rejected while staging exists.
- The server source cap is a persistent identity cap, not a concurrency limit. SourceReplica has no source-retirement command, and every command is checked against the configured allowlist.
- A same-epoch checkpoint must strictly advance the active revision. If the server is at or ahead of the local revision, the agent rotates epoch before rebaseline.
- The production server exposes `/api/v1/state`, not `/api/v1/events` or `/api/v1/events/batch`. Do not re-enable legacy ingest as a shortcut.
- Product reads use `active_source_replica` views only. Evidence-only or staging-only sources are invisible.
- Core and Knots verbose responses may differ outside `vsize`, `time`, and `fees.base`. Reject a whole snapshot if a required fact is missing, malformed, overflowing, inexact, or oversized. Preflight `getmempoolinfo.size` against `ATLAS_MAX_MEMPOOL_ENTRIES`, decode directly into one validated snapshot map, and enforce the limit again after the racy second RPC call. `corepc-client` still buffers the HTTP response, so deployment memory testing remains required.
- Keep the `corepc` log target capped below trace. `corepc-client` trace output includes the complete raw RPC result, so broad diagnostic trace logging must never write one verbose mempool response per poll.
- SQLite temp storage stays in memory, but database, WAL, and SHM paths must remain writable. The fixed canary filesystem and cgroup are the hard deployment boundaries when application admission cannot prevent transient growth.

## Documentation

### Reference (`agent_docs/`)

- `agent_docs/agent-runtime.md` documents production agent state, delivery, configuration, bounds, and recovery.
- `agent_docs/peer-observer-wire.md` documents the retained experimental protobuf, NATS, identifier, and archive contract.
- `docs/architecture.md` explains the implemented end-to-end system and evidence boundary.
- `docs/adr/0002-separate-source-state-from-bounded-evidence.md` records the bounded data-lifecycle decision.

### Refreshing Docs

`agent_docs/.docs-ref` stores the commit against which references were last validated. After architectural changes, run `bash /Users/anthonymilton/dev/agent-skills/manage-agent-docs-skills/scripts/refresh-agent-docs.sh .`, review the report, and use `--update` only after the relevant code is committed and the references are correct.
