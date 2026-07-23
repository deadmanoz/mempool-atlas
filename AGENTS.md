# Mempool Atlas

Mempool Atlas is a standalone Bitcoin mempool viewer. Attempt #3 focuses on one
selected source: a central service periodically pulls a complete mempool
snapshot, keeps only the latest successful snapshot in memory, and serves a
Canvas-based website. Comparison and archival are separate future products.

## Architecture

- `apps/atlas/src/rpc.rs` owns bounded Bitcoin RPC collection. It preflights
  `getmempoolinfo`, decodes verbose `getrawmempool`, and then records
  `getblockchaininfo`.
- `apps/atlas/src/model.rs` owns the flat source-scoped snapshot contract.
- `apps/atlas/src/runtime.rs` builds snapshots off-lock and atomically replaces
  the latest successful in-memory `Arc`.
- `apps/atlas/src/api.rs` exposes health, readiness, source discovery, current
  membership, and the built website.
- `apps/atlas/src/main.rs` configures one source, reads the RPC password from a
  credential file, binds to loopback, and owns process lifecycle.
- `web/` discovers the source, validates one complete snapshot, and renders the
  fee-by-age swim view, filters, health state, and bounded txid inspector.

Production reuses the existing WireGuard-only node RPC proxy. No Atlas process,
database, queue, or container runs on the Bitcoin node. Deployment addresses,
credentials, and fleet inventory remain outside this repository.

## Key dependencies

- `corepc-client` supplies synchronous Bitcoin Core v28 RPC. Keep complete RPC
  observations behind `spawn_blocking`.
- `bitcoin` validates transaction IDs, block hashes, and exact BTC amounts.
- `axum` provides the JSON and static-file HTTP surface.
- `bytes` holds one shared encoded response per published source state so large
  reads do not reserialize or allocate one full payload per request.
- `tower-http` serves the built Vite application from the same process.
- Tokio owns the poll loop, locks, listener, and shutdown signals.
- Vite and TypeScript build the dependency-light browser client.

## Build and test

Use `just` targets whenever one exists:

- `just build` builds Rust and the website.
- `just test` runs Rust and website tests.
- `just lint` checks Rust formatting, Clippy, TypeScript, and frontend format.
- `just format` formats Rust and frontend source.
- `just dev` runs the complete service.
- `just web-dev` runs Vite for frontend development.
- `just clean` removes generated Rust and frontend build output.

## Snapshot invariants

- One source snapshot describes only that node's current mempool.
- Membership is keyed and strictly sorted by `txid`.
- Retain only `vsize`, exact base fee in satoshis, and node entry time from
  verbose RPC.
- Age is relative to snapshot observation time, never the browser clock.
- Publish a snapshot only after the full observation validates.
- Publish the structured snapshot and its shared encoded response together.
- A failed poll keeps the prior snapshot visible as stale.
- Enforce `ATLAS_MAX_MEMPOOL_ENTRIES` before and during verbose decoding.
- Keep every integer exactly representable by browser JSON numbers.
- Treat received, present, rejected, and classified as independent claims.
- Absence from a source is not evidence of rejection, filtering, or relay
  causality.

## Scope boundaries

- Do not add a database, migration, event queue, delta protocol, or node-local
  service to the current-state viewer.
- Do not mix forensic evidence or archival retention into the viewer process.
- Keep future comparison source-scoped and derive differences from independent
  complete snapshots.
- Keep source IDs configurable. Never commit hostnames, private addresses, RPC
  credentials, or deployment inventory.
- Require a server-side RPC whitelist containing only `getmempoolinfo`,
  `getrawmempool`, and `getblockchaininfo` for the Atlas identity.
- Keep `corepc` logging below trace. Trace output can include the complete raw
  verbose mempool response.

## Repository etiquette

- Track multi-session work in Beads and keep active notes resumable.
- Commit only when explicitly requested.
- Use atomic conventional commits with no agent attribution.
- Keep changes scoped to the active Bead and record side work as discovered
  Beads.

## Gotchas

- `corepc-client` buffers the HTTP response before custom deserialization.
- `corepc-client` 0.8 has a fixed 15-second transport timeout. Prove the real
  large-snapshot path stays inside it before adding sources or relying on a
  materially larger mempool, or replace the transport. The first deployed
  single-source slice succeeded with 28,520 to 33,381 entries.
- Snapshot replacement can briefly retain both the old and new snapshot while
  readers finish. The service also retains one encoded JSON representation, so
  the entry cap is not a complete memory bound.
- The public deployment applies reverse-proxy request and connection limits
  plus JSON compression. Shared response bytes prevent per-request allocation,
  not bandwidth abuse.
- The node-side RPC proxy must stream verbose responses without proxy-temp
  spill. Default collection is every five minutes; choose production cadence
  from measured bytes, duration, node cost, and freshness.
- The service binds only to loopback and expects the existing presentation-host
  web proxy.
- The executable configures one source even though the read model remains
  source-scoped for later comparison.
- Browser refresh fetches the latest server copy. It does not trigger an RPC
  poll.
- A restart discards current state by design and waits for the first new
  snapshot. There is no application data recovery path.

## Documentation

- `agent_docs/agent-runtime.md` is the implementation-backed runtime reference.
- `docs/architecture.md` explains the current end-to-end design.
- `docs/adr/0003-periodic-in-memory-snapshots.md` records the attempt #3 reset.

`agent_docs/.docs-ref` stores the commit against which references were last
validated. After architectural changes, run
`bash /Users/anthonymilton/dev/agent-skills/manage-agent-docs-skills/scripts/refresh-agent-docs.sh .`,
review the report, and use `--update` only after the implementation and
references are committed.
