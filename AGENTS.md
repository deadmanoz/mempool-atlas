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
- `apps/atlas/src/policy.rs` owns continuous, bounded classification slices
  through concurrent batched `getrawtransaction` and
  `gettxout(txid, vout, false)`. It owns current in-memory generations, exact
  surviving `wtxid` classification reuse, auxiliary script caches, and stale
  result rejection.
- `crates/rdts-rules/` is the pure, evidence-carrying evaluator for all seven
  rules. Production uses its Knots mempool-policy mode, not its separate
  consensus mode.
- `apps/atlas/src/model.rs` owns the source-scoped snapshot, compact assessment,
  and typed transaction-detail contracts.
- `apps/atlas/src/runtime.rs` runs fixed-interval membership independently from
  current-generation classification. It publishes complete membership first,
  then atomically publishes strictly newer matching detail revisions as slices
  complete.
- `apps/atlas/src/api.rs` exposes health, readiness, source discovery, current
  membership, current transaction detail, and the built website.
- `apps/atlas/src/main.rs` configures one source, reads the RPC password from a
  credential file, binds to loopback, and owns process lifecycle.
- `web/` discovers the source, validates one complete snapshot, and renders the
  classification terrain, the secondary fee-rate-by-age lens, health state,
  rule navigation, and bounded transaction-detail inspector.

Production reuses the existing WireGuard-only node RPC proxy. No Atlas process,
agent, database, queue, retained history, ZMQ subscriber, container, or new
listener runs on the Bitcoin node. Atlas adds no network path. Deployment
addresses, credentials, and fleet inventory remain outside this repository.

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
- Tokio owns the independent runtime loops, locks, listener, and shutdown
  signals.
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
- Publish each validated membership generation immediately with only exact
  surviving classifications. Do not wait for new policy RPC work.
- Publish the structured snapshot, its matching current-generation detail map,
  and its shared encoded response as one observation.
- Set reader-visible `classification_revision` to zero for each new membership
  generation and advance it with every published classification slice. Return
  the same revision with transaction detail.
- A failed poll keeps the prior snapshot visible as stale.
- Classification is best effort. Missing raw transaction data leaves an entry
  explicitly unclassified without invalidating fresh membership.
- Missing prevouts produce typed unknowns. Those partial classifications remain
  visible and become retryable in later membership generations while the
  witness variant is current.
- Select unclassified variants before retryable partial results. Attempt each
  exact witness variant at most once per membership generation, then reset
  eligibility with the next successful membership.
- Merge policy results only while their in-memory generation is current, and
  publish them only when their generation matches runtime state and their
  revision strictly advances.
- Stop scheduling new RPC waves as soon as a generation is superseded. Discard
  results from already in-flight stale work.
- When a slice produces no classifications and has a batch failure or
  all-response failure, pause that generation until the next membership
  installation.
- Cache classifications by the source-bound `wtxid`, verify both `txid` and
  `wtxid`, and carry only exact survivors into the next generation. Reuse
  current-transaction output scripts only for the same exact variant and within
  auxiliary-cache admission.
- Retain at most one evidence exemplar and one missing-fact exemplar per rule,
  alongside exact evidence and missing counts.
- Enforce `ATLAS_MAX_MEMPOOL_ENTRIES` before and during verbose decoding.
- Keep every integer exactly representable by browser JSON numbers.
- Treat received, present, rejected, and classified as independent claims.
- Absence from a source is not evidence of rejection, filtering, or relay
  causality.

## Scope boundaries

- Do not add a database, migration, event queue, delta protocol, ZMQ path,
  container, or node-local service to the current-state viewer.
- Do not add a network path beside the existing WireGuard-only RPC proxy.
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
- Classification defaults to 2,048 witness variants per slice and four
  concurrent raw RPC lanes. It drains slices until the current generation is
  attempted or replaced. Raw transaction and mempool-parent batches contain at
  most 256 requests. Confirmed prevout batches have a nominal 512-request cap,
  but the 16 MiB estimate and 64 KiB per-script-hex bound currently reduce that
  to 254. Each enrichment batch has a 20-second transport timeout. RPC lanes
  accept one through eight, and slice size accepts one through 8,192.
  Confirmed prevout work uses half the configured lanes rounded up.
- Candidate raw work and mempool-parent raw work each have an independent
  256 MiB aggregate response estimate per slice. The parent phase also caps at
  8,192 transactions.
- Consider at most 65,536 unique required prevouts per slice. Confirmed-prevout
  requests have a separate 256 MiB estimated aggregate ceiling, currently
  4,064 worst-case calls under the 64 KiB plus 512-byte per-response estimate.
  Facts beyond these planning bounds remain typed as missing.
- Returned transaction hex is capped at 8,000,000 characters and each decoded
  JSON-RPC response envelope at 16 MiB. The minreq transport buffers and parses
  a response before the envelope check, so it is not a peak-memory limit. The
  production 2 GiB memory cgroup is the hard transient boundary.
- `ATLAS_CLASSIFICATION_CACHE_MIB` bounds estimated auxiliary output-script
  admission at 256 MiB by default and 512 MiB maximum. It does not bound
  classifications, RPC buffers, encoded snapshots, allocator overhead, or
  reader overlap.
- A proven violation and unresolved inputs can coexist for one rule. Preserve
  both its exact evidence and missing counts instead of collapsing it to a
  single boolean.
- Snapshot and classification-revision replacement can briefly retain both old
  and new state while readers finish. The service also retains one encoded JSON
  representation, current classification state, auxiliary script caches, and
  detail map, so the entry cap is not a complete memory bound.
- The snapshot and detail contracts both expose `classification_revision`.
  Browser detail must match source, membership observation, `txid`, and
  `wtxid`. It may use detail from a later revision only when the compact
  assessment is unchanged; older or assessment-changing detail is inconsistent.
- The public deployment applies reverse-proxy request and connection limits
  plus JSON compression. Shared response bytes prevent per-request allocation,
  not bandwidth abuse.
- The node-side RPC proxy must stream verbose responses without proxy-temp
  spill. Membership uses a five-minute interval with delayed missed ticks.
  Classification runs independently and must not lengthen an otherwise
  on-schedule membership cadence. Choose production cadence from measured
  bytes, duration, node cost, and freshness.
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
