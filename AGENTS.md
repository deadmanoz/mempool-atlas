# Mempool Atlas

Mempool Atlas is a classification-first Bitcoin mempool viewer. Attempt #3
focuses on one selected source: one central service on the presentation host
periodically pulls a complete mempool snapshot, evaluates current transactions
against the seven BIP-110 rules as deployed Bitcoin Knots mempool policy, keeps
only the latest successful observation in memory, and serves a Canvas-based
website. Comparison and archival are separate future products.

## Architecture

- `apps/atlas/src/rpc.rs` owns the complete membership observation:
  `getmempoolinfo`, verbose `getrawmempool`, and `getblockchaininfo`.
- `apps/atlas/src/policy.rs` owns bounded, best-effort classification
  enrichment through batched `getrawtransaction` and
  `gettxout(txid, vout, false)`. Its source-bound `wtxid` cache contains only
  current membership.
- `crates/rdts-rules/` is the pure, evidence-carrying evaluator for all seven
  rules. Production uses its Knots mempool-policy mode, not its separate
  consensus mode.
- `apps/atlas/src/model.rs` owns the source-scoped snapshot, compact assessment,
  and typed transaction-detail contracts.
- `apps/atlas/src/runtime.rs` builds observations off-lock and atomically
  replaces the latest snapshot and matching transaction-detail map.
- `apps/atlas/src/api.rs` exposes health, readiness, source discovery, current
  membership, current transaction detail, and the built website.
- `apps/atlas/src/main.rs` configures one source, reads the RPC password from a
  credential file, binds to loopback, and owns process lifecycle.
- `web/` discovers the source, validates one complete snapshot, and renders the
  classification terrain, the secondary fee-rate-by-age lens, health state,
  rule navigation, and bounded transaction-detail inspector.

Production reuses the existing WireGuard-only node RPC proxy. No Atlas process,
database, queue, retained history, container, or new listener runs on the
Bitcoin node. Deployment addresses, credentials, and fleet inventory remain
outside this repository.

## Key dependencies

- `corepc-client` supplies synchronous Bitcoin Core v28 RPC for complete
  membership observations. Its fixed 15-second transport timeout applies to
  that path. Keep the calls behind `spawn_blocking`.
- `jsonrpc` with `minreq_http` supplies bounded batch transport for
  classification enrichment with a 20-second per-batch timeout.
- `rdts-rules` evaluates exact, typed BIP-110 rule evidence without I/O, chain
  access, async state, or a clock.
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
- Membership is keyed and strictly sorted by `txid`; each entry also carries
  the node-reported `wtxid` for its current witness variant.
- Retain `vsize`, exact base fee in satoshis, node entry time, and the compact
  classification assessment from verbose RPC plus bounded enrichment.
- Age is relative to snapshot observation time, never the browser clock.
- Publish membership only after the complete observation validates.
- Publish the structured snapshot, its matching detail map, and its shared
  encoded response as one observation.
- A failed poll keeps the prior snapshot visible as stale.
- Classification is best effort. Missing raw transaction data leaves an entry
  explicitly unclassified without invalidating fresh membership.
- Missing prevouts produce typed unknowns. Those partial classifications remain
  visible and are retried on later polls while the witness variant is current.
- Advance the source-local candidate cursor after every attempted batch and
  wrap at the end of sorted membership. Retryable low-txid entries must not
  starve fresh or later current members.
- Cache classifications by the source-bound `wtxid`, verify both `txid` and
  `wtxid`, and prune every result that is no longer in current membership.
- Retain at most one evidence exemplar and one missing-fact exemplar per rule,
  alongside exact evidence and missing counts.
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
  `getrawmempool`, `getblockchaininfo`, `getrawtransaction`, and `gettxout` for
  the Atlas identity.
- Describe classification only as compatibility with the deployed Knots
  RDTS/BIP-110 mempool policy. It is not proof that the source rejected a
  transaction and it is not a consensus-validity judgment.
- Keep `corepc` logging below trace. Trace output can include the complete raw
  verbose mempool response.

## Repository etiquette

- Track multi-session work in Beads and keep active notes resumable.
- Commit only when explicitly requested.
- Use atomic conventional commits with no agent attribution.
- Keep changes scoped to the active Bead and record side work as discovered
  Beads.

## Gotchas

- `corepc-client` buffers the complete membership response before custom
  deserialization.
- `corepc-client` 0.8 has a fixed 15-second transport timeout. Prove the real
  large-snapshot path stays inside it before adding sources or relying on a
  materially larger mempool, or replace the transport. The first deployed
  single-source slice succeeded with 28,520 to 33,381 entries.
- Classification defaults to at most 10,000 uncached or incomplete witness
  variants per poll and a 45-second soft budget for starting more work. Raw
  transaction and mempool-parent batches contain at most 16 requests, confirmed
  prevout batches contain at most 128 requests, and each enrichment batch has a
  20-second transport timeout. An in-flight batch can finish after the soft
  budget.
- A proven violation and unresolved inputs can coexist for one rule. Preserve
  both its exact evidence and missing counts instead of collapsing it to a
  single boolean.
- Snapshot replacement can briefly retain both the old and new snapshot while
  readers finish. The service also retains one encoded JSON representation,
  current classification cache, and detail map, so the entry cap is not a
  complete memory bound.
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
