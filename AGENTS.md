# Mempool Atlas

Mempool Atlas is a classification-first Bitcoin mempool viewer. One central
service on the presentation host periodically pulls complete, independent
mempool snapshots from a small configured set of Bitcoin nodes, evaluates
current transactions against the seven BIP-110 rules as deployed Bitcoin Knots
mempool policy, keeps only the latest successful observation per source in
memory, and serves Canvas-based single-node and comparison products. Archival
remains a separate product.

## Architecture

- `apps/atlas/src/rpc.rs` owns the complete membership observation. It brackets
  `getmempoolinfo` and verbose `getrawmempool` with matching
  `getblockchaininfo` calls and records the successful collection window.
- `apps/atlas/src/policy_rpc.rs` owns authenticated, bounded policy batches. It
  preserves JSON member presence, restores out-of-order batch responses by
  globally unique numeric request ID, requires Bitcoin Core's JSON-RPC v2
  response shape, and caps each body before parsing.
- `apps/atlas/src/policy.rs` owns continuous, bounded classification through
  concurrent batched `getrawtransaction` and `gettxout(txid, vout, false)`.
  It owns current in-memory generations, same-generation pending fact waves,
  exact surviving `wtxid` classification reuse, bounded positive script
  caches, and stale result rejection.
- `crates/rdts-rules/` is the pure, evidence-carrying evaluator for all seven
  rules. Production uses its Knots mempool-policy mode, not its separate
  consensus mode.
- `apps/atlas/src/model.rs` owns the source-scoped snapshot, compact assessment,
  and typed transaction-detail contracts.
- `apps/atlas/src/runtime.rs` owns the bounded multi-source coordinator.
  Membership rounds poll sources sequentially, while classification advances
  source-local generations round-robin. Both paths share one RPC work gate. It
  publishes complete membership first, then atomically publishes strictly
  newer matching detail revisions as assessments complete.
- `apps/atlas/src/api.rs` exposes health, readiness, source discovery, current
  membership, current transaction detail, and the built website.
- `apps/atlas/src/main.rs` validates a root-controlled source file containing
  one to four sources, resolves named systemd credentials, divides one total
  classification cache budget among sources, binds to loopback, and owns
  process lifecycle.
- `web/` contains two product entries. The node viewer validates and renders
  one complete snapshot. The comparison page fetches two independent current
  snapshots, derives their sorted membership partition, and leads with a
  source-local policy matrix in the browser.

Production reuses the existing WireGuard-only node RPC proxy. No Atlas process,
agent, database, queue, retained history, ZMQ subscriber, container, or new
listener runs on the Bitcoin node. Atlas adds no network path. Deployment
addresses, credentials, and fleet inventory remain outside this repository.

## Key dependencies

- `corepc-client` supplies synchronous Bitcoin Core v28 RPC for complete
  membership observations. Its fixed 15-second transport timeout applies to
  that path. Keep the calls behind `spawn_blocking`.
- `minreq` supplies lazy HTTP response reads for Atlas-owned classification
  batches with a 20-second timeout and redirects disabled. `base64` encodes the
  Basic Auth header, and `serde_json` raw values preserve wire-member presence
  until the envelope is interpreted.
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

- One source snapshot describes only that node's current mempool. Comparison
  never constructs a combined server-side mempool.
- Bind every successful snapshot to collection start, completion, duration,
  and a chain tip that remained stable across the membership RPC sequence.
- Poll configured sources sequentially in deterministic order. One source
  failure must not prevent later sources in the round from being attempted.
- Admit membership and classification RPC work through one service-wide gate.
  Advance source-local classification generations fairly, and rearm only a
  source that published replacement membership.
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
  generation and advance it with every published assessment batch. Return
  the same revision with transaction detail.
- A failed poll keeps the prior snapshot visible as stale.
- Classification is best effort. Missing or invalid raw transaction data leaves
  an entry explicitly unclassified without invalidating fresh membership.
- Keep a successfully verified raw transaction in a bounded same-generation
  pending window while its required input scripts are resolved. Advance that
  work through fair bounded fact waves without refetching the candidate merely
  because its facts crossed a wave boundary.
- Evaluate a candidate only after every required script is present or has a
  genuine terminal lookup result. A successful null-shaped `gettxout` response
  from the trusted Bitcoin Core endpoint is terminal only for an outpoint known
  not to be a current mempool parent. The `result` member must be present in the
  required Bitcoin Core JSON-RPC 2.0 success shape; an omitted member is an
  operational failure. A null fallback after parent-raw failure is ambiguous
  and remains collection state.
- Capacity deferral, unscheduled work, batch or transport failure, malformed or
  oversized responses, and missing response envelopes remain collection state.
  They leave public `bip110` as `null` rather than manufacturing an
  indeterminate assessment.
- Treat drain state explicitly: continue while eligible work remains and no
  systemic circuit breaker fired, defer a candidate for the rest of the
  generation after two attempts for one fact source in its bounded pending
  window are exhausted, complete when no eligible work remains, pause only for
  a systemic RPC failure, and stop stale work after replacement. Deferred
  candidates remain unclassified and become eligible again with the next
  successful membership generation.
- Merge policy results only while their in-memory generation is current, and
  publish them only when their generation matches runtime state and their
  revision strictly advances.
- Stop scheduling new RPC waves as soon as a generation is superseded. Discard
  results from already in-flight stale work.
- Cache classifications by the source-bound `wtxid`, verify both `txid` and
  `wtxid`, and carry only exact survivors into the next generation. Reuse
  current-transaction output scripts only for the same exact variant. Retain
  positive confirmed `OutPoint` script facts across generations under bounded
  eviction, but never cache nulls or failures.
- Evaluate P2SH, P2SH-P2WPKH, and P2SH-P2WSH according to deployed Knots policy.
  Exempt the final scriptSig redeemScript blob, check earlier scriptSig items
  and pushes inside the redeemScript for rule 2, and treat P2SH-wrapped witness
  versions 1 through 16 as rule 3 rather than native Taproot or P2A.
- Retain at most one evidence exemplar and one missing-fact exemplar per rule,
  alongside exact evidence and missing counts.
- In the browser, place each complete violating assessment in one canonical
  exact `violated_rules` bucket. Keep assessments with any `unknown_rules` in
  separate partial buckets keyed by both proven and unknown rule sets.
- Place each transaction once in the terrain. Rule filters are marginal and
  overlap, while `primary_rule` remains detail metadata and never chooses a
  terrain bucket.
- Derive the comparison policy matrix as four count-only source-local rows in
  one pass over the disjoint membership arrays. Common txids contribute once to
  each source row. Conserve compatible, violating, indeterminate, and
  unclassified totals per row. Include exact and partial violations in the
  aggregate violating status, but include only exact signatures in at most
  three dominant combination controls per row. Sort by count descending and
  then canonical signature, and retain hidden combination and transaction
  counts.
- Preserve a positive glyph area for every populated terrain region, including
  rare status and partial buckets. Reuse geometry for selection-only paints and
  invalidate it when snapshot membership, size mode, or viewport changes.
- Keep node and comparison exploration in canonical URL state. Preserve valid
  state across ordinary refresh, clear it on intentional source or pair
  changes, and keep a well-formed absent txid explicit without manufacturing an
  assessment.
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
- Keep comparison source-scoped and derive differences in the browser from two
  independent complete snapshots. Retain no combined server projection.
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
- Classification admits up to 2,048 witness variants per pending window by
  default and uses four concurrent raw RPC lanes. It drains bounded candidate
  windows and fact waves until the policy report says continue, complete,
  paused, or stale. Raw transaction and mempool-parent batches contain at most
  256 requests. Confirmed prevout batches have a nominal 512-request cap, but
  the 16 MiB estimate and 64 KiB per-script-hex bound currently reduce that to
  254. Each enrichment batch has a 20-second transport timeout. RPC lanes accept
  one through eight, and candidate admission accepts one through 8,192.
  Confirmed prevout work uses half the configured lanes rounded up.
- Candidate raw work and mempool-parent raw work each have an independent
  256 MiB aggregate response estimate per candidate window or parent fact wave.
  The parent wave also caps at 8,192 transactions.
- Use 65,536 unique required prevouts as the target per pending window. Always
  admit one candidate even when that transaction alone exceeds the count
  target; its raw and retained-script byte bounds still apply.
  Confirmed-prevout requests have a separate 256 MiB estimated aggregate
  ceiling per fact wave, currently 4,064 worst-case calls under the 64 KiB plus
  512-byte per-response estimate. Work beyond a wave bound remains pending for
  fair continuation. A locally exhausted or capacity-blocked candidate is
  deferred only for the current generation; neither condition becomes a typed
  missing evaluator fact. Capacity recovery is bounded by the pending candidate
  count, yields and checks staleness between relief passes, and does not discard
  a positive fact already fetched in the current call while one of its
  dependents survives.
- Returned transaction hex is capped at 8,000,000 characters. Each
  classification HTTP body rejects `Transfer-Encoding` and is capped at 16 MiB
  before JSON parsing. A declared length above the cap is rejected immediately,
  a declared body must arrive at exactly that length, and a close-delimited body
  aborts on the first byte beyond the cap. Concurrent bodies, membership
  buffering, and the rest of the process remain subject to the production
  2 GiB memory cgroup.
- The Atlas-owned policy transport distinguishes an omitted `result` member, a
  present JSON null, and a present value. It also preserves `error` presence,
  gives errors precedence over results, requires the Bitcoin Core JSON-RPC 2.0
  envelope shape over HTTP 200, and rejects duplicate or unexpected response
  IDs and excess responses.
- `ATLAS_CLASSIFICATION_TOTAL_CACHE_MIB` is a 256 MiB default, 512 MiB maximum
  service-wide budget divided among configured sources. Each share bounds the
  combined admission of that source's exact current-transaction outputs and
  cross-generation positive confirmed scripts. Its pending resolver has a
  separate retained-script ceiling equal to the smaller of that share and
  256 MiB. Neither bound covers unresolved fact indexes, pending raw
  transactions, classifications, RPC buffers, encoded snapshots, allocator
  overhead, or reader overlap.
- Policy logs expose `fact_requests`, `facts_resolved`, `facts_missing`,
  `capacity_deferred`, `deferred_candidates`, `response_failures`,
  `systemic_response_failures`, `missing_responses`, `batch_failures`, and
  `response_bytes` separately.
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
  One due membership round polls all sources before classification resumes.
  A currently running bounded classification slice may finish first, but later
  slices cannot overtake the waiting round. Choose production cadence from
  measured bytes, duration, node cost, and freshness.
- The service binds only to loopback and expects the existing presentation-host
  web proxy.
- The executable accepts one to four configured sources. The configured
  classification cache is a service-wide total divided among them. Each
  source's pending-script ceiling is no larger than its share, while the
  existing 256 MiB absolute pending ceiling remains.
- The comparison membership partition places each txid once into
  present-in-both, observed-only-left, or observed-only-right. A common txid
  retains both source-local entries and explicitly exposes different witness
  variants.
- Comparison matrix controls atomically select region, source-local policy side,
  aggregate status or exact signature filter, and clear txid through the
  canonical view-state transition. Keep partial counts informative unless the
  product gains an explicit partial-only filter.
- Pair changes abort obsolete snapshot and detail reads. Selection-only Canvas
  paints reuse identity- and viewport-bound geometry. Keep transaction-level
  keyboard access virtual and bounded instead of creating one DOM node per
  transaction.
- Node URLs encode source, one rule or terrain region, and an optional txid.
  Comparison URLs encode an atomic distinct pair, membership region, policy
  side, policy filter, and optional txid. Comparison txid lookup must not retain
  a second union-sized index.
- Browser refresh fetches the latest server copy. It does not trigger an RPC
  poll.
- A restart discards current state by design and waits for the first new
  snapshot. There is no application data recovery path.

## Documentation

- `agent_docs/agent-runtime.md` is the implementation-backed runtime reference.
- `docs/architecture.md` explains the current end-to-end design.
- `docs/adr/0003-periodic-in-memory-snapshots.md` records the attempt #3 reset.
- `docs/adr/0004-exact-rule-combination-buckets.md` records the terrain's
  classification grouping semantics.
- `docs/adr/0005-resolve-policy-facts-before-evaluation.md` records the pending
  fact resolver, explicit-null semantics, positive prevout cache, and P2SH
  evaluation. It supersedes ADR 0003's attempt-once classification details.
- `docs/adr/0006-own-policy-json-rpc-wire-boundary.md` records the bounded,
  presence-preserving classification transport and its HTTP framing contract.
- `docs/adr/0007-browser-derived-snapshot-comparison.md` records bounded
  multi-source scheduling and the symmetric browser-derived comparison.

`agent_docs/.docs-ref` stores the commit against which references were last
validated. After architectural changes, run
`bash /Users/anthonymilton/dev/agent-skills/manage-agent-docs-skills/scripts/refresh-agent-docs.sh .`,
review the report, and use `--update` only after the implementation and
references are committed.
