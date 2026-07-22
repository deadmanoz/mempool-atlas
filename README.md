# Mempool Atlas

Mempool Atlas lets you explore one Bitcoin node's current mempool. Its primary product is a source-scoped aggregate workbench, with an optional read-time comparison across independent sources. Absence from one source is never presented as proof of rejection, filtering, or relay causality.

## Current architecture

The first production-shaped deployment is deliberately state-only. It proves bounded current-state convergence before peer evidence is reintroduced.

```text
Bitcoin Core or Knots
  getrawmempool true
          |
          v
atlas-agent, one per source
  persisted epoch + revision
  current fact-bearing membership
  coalesced dirty set
  one frozen delta or checkpoint
          |
          | POST /api/v1/state
          v
atlas-server
  at most one active + one staging generation per source
  verify checkpoint counts, chunks, and digest
  atomically activate complete generations
          |
          +--> single-source snapshot and aggregate summary
          +--> read-time comparison across independent sources
          |
          v
dependency-free browser workbench and inspector
```

RPC is the sole authority for current membership. Each successful full observation retains the common Core and Knots facts: virtual size, exact base fee in integer satoshis, and node-reported mempool entry time. The first complete observation, including an empty mempool, establishes revision 1. A changed snapshot advances the source revision once; an unchanged snapshot refreshes observation time without inventing a membership revision.

The node-local `SourceReplica` stores the latest complete snapshot and a coalesced dirty record per changed txid. Healthy changes are delivered as sorted deltas from an acknowledged base revision. Dirty pressure, bootstrap, database reset, or a server cursor conflict produces a full checkpoint. One action is frozen before delivery, so retries and process restarts reproduce the same payload. Newer RPC observations may continue while that action is in flight.

The server stages checkpoints outside the reader-visible generation. It verifies declared entries, contiguous chunks, and the canonical whole-snapshot digest before replacing the active generation in one SQLite transaction. A different begin may replace abandoned staging only by naming that exact checkpoint ID, which prevents delayed requests from evicting newer staging work. Product reads use active SourceReplica views only, so a partial checkpoint and legacy experimental evidence can never leak into current membership.

This fixes the failure mode learned from the experimental canaries: a central outage no longer creates a mutation log proportional to outage duration. Local state is bounded by current membership, a small coalesced dirty set, and at most one frozen snapshot. Before requesting verbose RPC state, the agent checks `getmempoolinfo.size` against `ATLAS_MAX_MEMPOOL_ENTRIES`, then decodes directly into one validated snapshot map and checks the limit again. The main agent SQLite file defaults to a 1 GiB `max_page_count` circuit breaker. Transient WAL peak, the RPC transport's buffered response, and filesystem free-space reserve still require measured deployment guards before the 2 GiB source-VPS envelope is considered complete.

Peer-observer decoding, classifiers, and the old evidence reducer remain available only as experimental building blocks. The production agent has no NATS or capture mode, and the production server does not mount `/api/v1/events` or `/api/v1/events/batch`. Reads therefore report capture and rejection evidence as `not_collected`; classifications without prior derived bytes are honestly `unknown`. Bounded recent evidence and verified off-VPS forensic archives are separate future data products, not extensions of the state FIFO.

See the [system architecture visualisation](docs/mempool-atlas-system.html), [architecture guide](docs/architecture.md), and [ADR 0002](docs/adr/0002-separate-source-state-from-bounded-evidence.md).

## Development

Prerequisites are a current Rust toolchain, Node.js, npm, `protoc`, and `just`.

```bash
npm --prefix web install
just build
just test
just lint
just proto-check
```

`just test-baseline-scale` runs the release-mode acceptance path with a 200,000-transaction agent checkpoint, real HTTP reduction, database reopen, and active-read verification.

Run the central API with `just dev`, and run Vite in a second terminal with `just web-dev`. `just seed-dev` refreshes a deterministic 20,000-transaction source through the real checkpoint endpoint. `just seed-forks` refreshes deterministic `knots`, `core`, and `libre-relay` snapshots for the optional comparison workspace. Re-running either command replaces source state rather than growing a history.

The read API provides:

- `GET /api/v1/sources`
- `GET /api/v1/sources/{source_id}/mempool`
- `GET /api/v1/sources/{source_id}/mempool/summary`
- `GET /api/v1/sources/{source_id}/rejections`
- `GET /api/v1/sources/compare?sources=a,b,c`

Select the browser source with `?source=<source-id>` or set `VITE_ATLAS_SOURCE_ID` when building the client.

## Agent configuration

Initialize and run one persistent agent database per source:

```bash
just agent-db-migrate-dev
just agent-dev
```

Required settings are `ATLAS_SOURCE_ID`, `ATLAS_SERVER_URL`, `ATLAS_RPC_USERNAME`, and `ATLAS_RPC_PASSWORD`. The database defaults to `var/atlas-agent.db`. Useful bounds and cadence settings are:

- `ATLAS_RPC_POLL_SECONDS=5`
- `ATLAS_MAX_MEMPOOL_ENTRIES=1000000`
- `ATLAS_MAX_DIRTY_MUTATIONS=4096`
- `ATLAS_MAX_DIRTY_BYTES=1048576`
- `ATLAS_CHECKPOINT_CHUNK_ENTRIES=512`
- `ATLAS_AGENT_DB_MAX_BYTES=1073741824`

An ordinary restart retains the source epoch. Reinitializing or losing the database creates a new epoch; the next checkpoint explicitly replaces the server's active cursor. Do not hand-delete a production database as an operational shortcut.

## Database safety

All database operations use backup-first `just` targets. The clean preproduction generations are central schema 7 and agent schema 4; stale generations are rejected rather than migrated in place.

For a preproduction reset, stop the relevant processes, run `just db-reinitialize-deploy` centrally and `just agent-db-reinitialize-deploy` for each source, then restart matching binaries. Reinitialization preserves a verified backup before replacement.

`just proto-check` expects the local peer-observer checkout at `../peer-observer`; set `PEER_OBSERVER_REPO` to override it. Repository conventions and the detailed runtime contract are indexed from `AGENTS.md`.
