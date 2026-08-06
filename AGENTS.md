# Mempool Atlas

Mempool Atlas is a current-state Bitcoin mempool viewer. One process polls one
to four configured Bitcoin nodes, publishes each complete source observation,
progressively evaluates current transactions through independent classifier
lenses, and serves source-local node and comparison views.

## Repository map

- `src/rpc.rs` collects and validates complete mempool membership.
- `src/classification_rpc.rs` owns bounded, presence-preserving classification
  RPC.
- `src/classification.rs` owns current classification generations and fact
  resolution. Tests live in `src/classification/`.
- `src/classifiers.rs` owns the pure property, shape, data-protocol,
  and BIP-110 result classifiers.
- `src/runtime.rs` coordinates source turns and the shared RPC work gate.
  `src/runtime/publisher.rs` owns atomic current-state publication. Tests live
  in `src/runtime/`.
- `src/model.rs` defines public snapshots, lifecycle, assessments,
  and transaction detail.
- `src/api.rs` serves health, readiness, source APIs, and static web
  assets.
- `src/bip110/` is the private pure seven-rule evaluator.
- `web/` contains the node viewer and browser-derived comparison page.
- `web/src/snapshot-distributions.ts` owns aggregate-only distribution models
  and caching. The two `*-distributions-view.ts` modules own their complete
  node and comparison distribution DOM subtrees.
- `web/src/comparison-policy-view.ts` owns source-local policy aggregates and
  bounded samples. `web/src/comparison-view-transition.ts` classifies
  interactive state changes before the controller applies effects.
- `docs/architecture.md` is the current system reference.
- `docs/classification.md` is the behavioral specification for classifier
  contracts, rules, thresholds, and limitations.
- `docs/configuration.md` is the public setup reference.
- `docs/deployment-cloudflare.md` is the public deployment and operations
  runbook.

## Commands

Use `just` targets whenever one exists:

- `just build` builds Rust and the website.
- `just test` runs Rust and website tests.
- `just lint` runs structure checks, Rust formatting and Clippy, Prettier, and
  TypeScript.
- `just format` formats Rust and frontend source.
- `just dev` runs the complete service.
- `just web-dev` runs Vite for frontend development.
- `just smoke-public <url> <source_id>` validates the deployed Cloudflare
  boundary.
- `just clean` removes generated output.

Rust has an MSRV of 1.88. The web build requires Node.js 20.19 or newer and npm
10 or newer.

## Key dependencies

- `corepc-client` provides synchronous complete-membership RPC. It is pinned to
  an immutable commit on the `deadmanoz/corepc` fork that raises the transport
  timeout to 30 seconds. Keep it behind `spawn_blocking`.
- `minreq` provides bounded classification HTTP reads with redirects disabled.
- The private `bip110` module evaluates typed BIP-110 evidence without I/O or
  runtime state while preserving the `rdts-rules` wire evaluator identifier.
- `bitcoin` validates identifiers, transactions, scripts, and exact amounts.
- `axum` and `tower-http` provide the API, gzip response compression, and
  static file surface.
- Tokio owns runtime loops, locks, the listener, and shutdown.
- Vite and TypeScript build the browser client.
- `happy-dom` is a test-only browser DOM used for view-factory lifecycle tests.

## Current-state invariants

### Membership

- One snapshot describes one source only. Never build a server-side combined
  mempool.
- Bracket `getmempoolinfo` and verbose `getrawmempool` with a stable chain tip.
- Enforce `ATLAS_MAX_MEMPOOL_ENTRIES` before and during decoding.
- Sort and deduplicate membership by canonical `txid`, retaining source-reported
  `wtxid`, virtual size, weight, exact fee, entry time, ancestry counts,
  virtual sizes, and delta-adjusted fees, and effective replaceability.
- Publish only complete validated membership. A failed poll retains the prior
  source observation as stale and does not block other sources.
- Age is relative to observation completion, never the browser clock.

### Classification

- Publish fresh membership before new classifier work completes.
- Carry classifier results forward only when their exact `txid` and `wtxid`
  survive.
- Admit membership and classification RPC through one service-wide work gate.
- Advance source generations fairly and reject results from superseded work.
- Evaluate only after all required scripts are present or genuinely terminal.
- Keep unscheduled work, capacity deferral, transport failure, malformed
  responses, and omitted result members as collection state. Public `bip110`
  remains `null`; do not manufacture an indeterminate assessment.
- Treat present JSON `result: null` as terminal only for a confirmed-prevout
  lookup known not to target a current mempool parent.
- Preserve exact evidence and missing-fact counts independently. A rule can
  have both a proven violation and unresolved facts.
- Cache only validated positive script facts under bounded eviction. Never
  cache nulls or failures.
- Derive per-transaction structure facts from the raw transaction during
  classification. Public `structure` is non-null exactly when classifier
  results are present and carries forward under the same identity gate.
- Expose lifecycle as `classifying`, `complete`, or `paused`. Membership health
  remains independent and `complete` may contain unavailable assessments.
- Publish snapshot, lifecycle, revision, encoded response, and matching detail
  atomically.
- Release the RPC work gate before snapshot materialization, validation, JSON
  encoding, and current-state publication.
- Publish a versioned classifier catalog and summaries with each snapshot.
- Keep exact properties, heuristic shapes, data fingerprints, and policy
  evaluations as independent lenses. Never manufacture one universal type or
  cross-product taxonomy.
- Preserve proven labels in partial results and name the missing fact classes.
- Keep compact snapshot results free of evidence. Retain bounded evidence only
  in transaction detail.
- Publish one matching `ETag` with every cached source representation. Waiting
  responses have no validator and remain non-cacheable.

### Browser

- Make the Classifications lens selector the default node view. Treat
  multi-label populations as marginal and potentially overlapping.
- Use the selected classifier for both the Classifications overview and Buckets
  view. Never combine classifier taxonomies.
- Each transaction appears in exactly one terrain region. Lenses keep complete,
  partial, and unavailable results separate. Transaction properties uses broad
  script-profile presentation groups while retaining exact labels on each
  transaction; smaller generic lenses use exact observed label-set buckets.
- Preserve one selectable Canvas block per transaction inside its terrain group.
  Keep section and bucket area proportional to the selected count or virtual
  size metric, suppress labels that cannot fit, cache classifier partitions
  across interactions, and keep filters, samples, and transaction evidence
  behind explicit disclosure.
- Keep BIP-110 as a specialist adapter: complete violations use one canonical
  exact `violated_rules` bucket, while partial violations use separate
  proven-plus-unknown buckets.
- Label and rule filters are marginal and may overlap; `primary_rule` never
  chooses terrain placement.
- Comparison merge-joins two independent sorted snapshots in the browser.
- Common transactions retain both source-local witness variants and policy
  assessments.
- Absence from a source is not evidence of rejection, filtering, or relay
  causality.
- Keep source, classifier, optional policy region, filter, and optional
  transaction in canonical URL state.
- Avoid one DOM node or a duplicate union-sized index per transaction.
- Cache distribution and comparison-policy aggregates by their semantic input
  identity. Keep only bounded transaction samples in derived browser models.
- Keep `main.ts` and `comparison-main.ts` focused on page lifecycle and
  cross-view orchestration. Visible distribution subtrees own their lookup,
  rendering, event, resize, cache, and reset lifecycles.

## Scope and security

- Keep the viewer memory-only and current-state only. Do not add a database,
  queue, delta protocol, ZMQ feed, node-side agent, or history here.
- Keep comparison browser-derived and source-local.
- Restrict the Atlas RPC identity to `getblockchaininfo`, `getmempoolinfo`,
  `getrawmempool`, `getrawtransaction`, and `gettxout`.
- Keep addresses, credentials, and private infrastructure details out of the
  repository.
- Bind Atlas only to loopback. Public ingress uses Cloudflare Tunnel, with TLS,
  compression, cache rules, WAF, rate limits, and security headers at the edge.
- Keep `/healthz` and `/readyz` private in the public tunnel configuration.
- Keep `corepc` logging below trace because trace output may contain the full
  verbose mempool response.
- Describe results as compatibility with Bitcoin Knots' deployed BIP-110
  mempool policy, never as proof of rejection or consensus invalidity.

## Change discipline

- Update `CHANGELOG.md` under `[Unreleased]` for user-visible behavior.
- Update `README.md`, `docs/architecture.md`, or `docs/configuration.md` when a
  change affects the public product, design, or setup.
- Update the editable architecture diagram and exported PNG when the component
  or data-flow boundary changes.
- Add or update browser tests for new flows when Playwright infrastructure is
  present.
- Commit only when explicitly requested. Use atomic conventional commits and
  never add agent attribution.
