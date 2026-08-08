# Mempool Atlas

Mempool Atlas is a current-state Bitcoin mempool viewer. One process polls one
to four configured Bitcoin nodes, publishes each complete source observation,
progressively evaluates current transactions through independent classifier
lenses, and serves source-local node and comparison views.

## Repository map

- `src/rpc.rs` collects and validates complete mempool membership.
- `src/rpc_transport.rs` owns the shared bounded HTTP policy for every
  node-facing JSON-RPC read: no redirects, explicit timeouts, and bounded
  responses.
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
- `src/staged_snapshot.rs` encodes deterministic v2 publication bundles.
  `src/staged_snapshot/validation.rs` rejects incoherent model inputs before
  publication; focused tests live in `src/staged_snapshot/tests/`.
- `src/perf_fixtures.rs` is the feature-gated canonical exporter for generated
  browser fixture bodies. Production builds do not enable `perf-fixtures`.
- `src/bip110/` is the private pure seven-rule evaluator.
- `web/` contains the node viewer and browser-derived comparison page.
- `web/src/source-summary.ts` owns the strict source-summary wire validator
  shared by discovery parsing and staged-manifest parsing.
- `web/src/atlas-worker.ts` owns staged publication fetch, bounded decoding,
  semantic validation, cache and retry behavior, and the worker message surface.
- `web/src/publication-digest.ts` owns canonical classification-set and
  publication preimages plus SHA-256 digest verification used by the worker.
- `web/src/source-summary-view.ts` owns early source metadata, stale-state
  honesty, and the shared node/comparison loading phases.
- `web/src/source-summary-styles.css` owns the node source-summary component
  and all of its breakpoints.
- `web/src/classification-overview-view.ts` owns the node lens selector,
  multi-label ANY/ALL controls, lens semantics guidance, and classification
  summary subtree. `web/src/classification-query-view.ts` owns the full-result
  Canvas listbox, hit testing, keyboard traversal, resize lifecycle, and
  transaction-local selection paint. `web/src/classifier-terrain.ts` owns the
  compact marginal indexes and deduplicated ANY/ALL query resolution.
- `web/src/snapshot-distributions.ts` owns aggregate-only distribution models
  and caching. The two `*-distributions-view.ts` modules own their complete
  node and comparison distribution DOM subtrees;
  `snapshot-distribution-view-state.ts` owns the node view's identity,
  selection, and variant state.
- `web/src/distribution-interaction.ts` owns the section-local tooltip and pin
  lifecycle. `web/src/distribution-chart-inspection.ts` owns exact bin
  descriptions and pointer hit-testing shared by SVG spectra and Canvas
  densities. `web/src/snapshot-distribution-layout-control.ts` owns the node
  distribution grid's persisted desktop column preference.
- `web/src/comparison-policy-view.ts` owns per-node policy aggregates and
  filter totals. `web/src/comparison-view-transition.ts` classifies
  interactive state changes before the controller applies effects, including
  observed asynchronous canvas scheduling.
- `web/src/terrain-selection-view.ts` restores the last bounded terrain paint
  and overlays transaction-local focus without replaying the full population.
- `web/src/styles.css` owns shared shell, header, toolbar, and terrain rules,
  including their breakpoints. `web/src/distribution-styles.css` owns shared
  Snapshot distributions and chart rules, including their breakpoints.
  `web/src/comparison-styles.css` adds only comparison-page selectors.
- `web/e2e/` holds Playwright viewport, early-metadata, and layout-shift
  coverage driven by the fixture Atlas API in `web/dev/fixture-server.mjs`.
- `web/perf/` holds the production-build performance server, Playwright
  measurement harness, and result merger. `release-result-validator.mjs` owns
  typed release-gate inputs and feasibility derivation. Generated profiles and
  results live under gitignored `web/.perf-fixtures/` and `web/.perf-results/`.
- `scripts/smoke-public.sh` validates the deployed edge contract;
  `scripts/lib/smoke-public-helpers.sh` owns repeated-header parsing and bounded
  first-transaction extraction.
- `docs/architecture.md` is the current system reference.
- `docs/classification.md` is the behavioral specification for classifier
  contracts, rules, thresholds, and limitations.
- `docs/configuration.md` is the public setup reference.
- `docs/deployment-cloudflare.md` is the public deployment and operations
  runbook.

## Commands

Use `just` targets whenever one exists:

- `just build` builds Rust and the website.
- `just test` runs Rust and website unit tests.
  It enables `perf-fixtures` for the Rust suite so both cross-language
  publication-digest goldens run; a bare `cargo test` omits those checks.
- `just test-web-e2e` runs Playwright viewport coverage against the fixture
  Atlas API, after a one-time `just test-web-e2e-install`. It preflights ports
  3101 and 5174 and never reuses an existing fixture or preview server.
- `just functional-fixtures` exports the small Rust-owned functional profile.
- `just perf-fixtures` also exports the 70,000-transaction performance profile.
- `just publication-digest-fixture` regenerates both checked-in Rust/browser
  publication-digest goldens from the Rust-owned model and source-metadata
  cases.
- `just stage-projection` regenerates and enforces the checked staged-byte
  projection. Run it with exactly Node.js 22.23.2 so gzip output matches CI.
- `just perf-web` builds the production web assets, runs the desktop and Slow
  4G performance matrix in normal Chromium, and writes the merged result. Run
  it with exactly Node.js 22.23.2 because it includes `stage-projection`.
- `just lint` runs structure checks, Rust formatting and Clippy, Prettier, and
  TypeScript.
- `just format` formats Rust and frontend source.
- `just dev` runs the complete service.
- `just web-dev` runs Vite for frontend development.
- `just smoke-public <url> <source_id>` validates the deployed Cloudflare
  boundary.
- `just clean` removes generated output.

Rust has an MSRV of 1.88. The web build requires Node.js 20.19.x or 22.12 or
newer and npm 10 or newer.

## Key dependencies

- `minreq` provides every node-facing HTTP read through the shared
  `src/rpc_transport.rs` policy: redirects disabled, explicit timeouts, and
  bounded status line, headers, and body. Membership and classification RPC
  both go through it, and membership calls stay behind `spawn_blocking`.
- The private `bip110` module evaluates typed BIP-110 evidence without I/O or
  runtime state while preserving the `rdts-rules` wire evaluator identifier.
- `bitcoin` validates identifiers, transactions, scripts, and exact amounts.
- `axum` and `tower-http` provide the API, gzip response compression, and
  static file surface.
- Tokio owns runtime loops, locks, the listener, and shutdown.
- Vite and TypeScript build the browser client.
- `happy-dom` is a test-only browser DOM used for view-factory lifecycle tests.
- `@playwright/test` is a test-only browser driver for viewport and local
  production-build performance coverage. It never runs against a live node.

## Current-state invariants

### Membership

- One snapshot describes one source only. Never build a server-side combined
  mempool.
- Read the chain tip before and after `getmempoolinfo` and verbose
  `getrawmempool`, and require the starting and ending height and hash to match.
- Enforce `ATLAS_MAX_MEMPOOL_ENTRIES` before and during decoding.
- Sort membership by canonical `txid` and reject duplicate canonical `txid` or
  `wtxid` values. Retain source-reported virtual size, weight, exact fee, entry
  time, ancestry counts and virtual sizes, delta-adjusted ancestor fees, and
  effective replaceability.
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

- Render the selected `SourceSummary` before requesting its complete snapshot.
  Keep `discovering-sources`, `metadata-ready`, `loading-snapshot`,
  `deriving-view`, and `interactive` separate from source availability.
- Replace early node and comparison summaries in place, preserve retained
  observation errors, and stop `aria-busy` when a request terminates.
- Reserve source-summary, primary workspace, and derived-panel geometry. Keep
  the primary comparison workspace before secondary distributions and policy.
- Make the Classifications lens selector the default node view. Treat
  multi-label populations as marginal and potentially overlapping.
- Use the selected classifier for both the Classifications overview and Buckets
  view. Never combine classifier taxonomies.
- Let Classifications combine labels from the active classifier with ANY or ALL
  and render every matching transaction once in a selectable Canvas. Keep
  complete and partial matches separate, exclude unavailable results, preserve
  the query in canonical URL state, and show only selected-transaction detail
  in its inspector. In ALL mode, disable an unselected label when adding it
  would empty the intersection, while keeping selected labels removable. Keep
  Buckets label and rule controls independent.
- Each transaction appears in exactly one terrain region. Lenses keep complete,
  partial, and unavailable results separate. Transaction properties uses broad
  script-profile presentation groups while retaining exact labels on each
  transaction; smaller generic lenses use exact observed label-set buckets.
- Preserve one selectable Canvas block per transaction inside its terrain group.
  Selecting a transaction preserves the active label, rule, and bucket
  emphasis and adds a transaction-local highlight; only an explicit region or
  filter control may restyle the wider terrain.
  Keep section and bucket area proportional to the selected count or virtual
  size metric, suppress labels that cannot fit, cache classifier partitions
  across interactions, and keep the selected transaction detail persistently
  visible in the inspector. Membership filters apply directly without a
  separate confirmation action, and the count/vsize metric remains a prominent
  snapshot-wide control because it also changes the distribution panels. In
  the inspector, show label and rule controls before their population outcome,
  then show the selected transaction's membership facts and active-lens result.
  Do not repeat every classifier as a cross-lens detail-card stack.
- Keep BIP-110 as a specialist adapter: complete violations use one canonical
  exact `violated_rules` bucket, while partial violations use separate
  proven-plus-unknown buckets.
- Label and rule filters are marginal and may overlap; `primary_rule` never
  chooses terrain placement.
- Comparison merge-joins two independent sorted snapshots in the browser.
- Common transactions retain both source-local witness variants and policy
  assessments.
- Comparison policy focus highlights matching transactions inside the selected
  membership region. Selecting a transaction preserves that focus and adds
  only a local highlight; explicit region, node-policy, or filter changes may
  repaint the wider comparison canvas.
- Absence from a source is not evidence of rejection, filtering, or relay
  causality.
- Keep source, classifier, Classifications label set and match mode, optional
  policy region, filter, and optional transaction in canonical URL state.
- Present Node and Compare as the shared primary view switch in both page
  headers, with the active product visually explicit at every breakpoint.
- Avoid one DOM node or a duplicate union-sized index per transaction.
- Cache distribution and comparison-policy aggregates by their semantic input
  identity. Keep transaction selection direct rather than deriving parallel
  sample tables.
- Keep distribution inspection presentation-only. Spectra and densities use
  one tab stop per chart, pointer hit-testing, arrow-key region traversal, and
  one pinned region without rescanning transactions or creating per-bin DOM.
- Let desktop users persist an auto, one-, two-, or three-column Snapshot
  distributions layout without rebuilding aggregate models. Narrow viewports
  always render one readable column.
- Position distribution ticks from their raw logarithmic domains and describe
  exact bin bounds in inspectors. Do not present the 83-byte serialized
  OP_RETURN script reference as an `op_return_bytes` payload threshold.
- Keep `main.ts` and `comparison-main.ts` focused on page lifecycle and
  cross-view orchestration. Visible distribution subtrees own their lookup,
  rendering, event, resize, cache, and reset lifecycles.
- Keep a component's breakpoints in the stylesheet that defines it. A shared
  component's responsive rules never live in a page-specific stylesheet, or the
  page that does not import it silently loses them.
- Serve both pages down to 320px with no horizontal page scroll. Content that
  cannot shrink scrolls inside its own container; the header clips, so nothing
  may overflow it.

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
- Never log RPC response bodies; the verbose mempool response and error
  bodies can be large and node-controlled.
- Describe results as compatibility with Bitcoin Knots' deployed BIP-110
  mempool policy, never as proof of rejection or consensus invalidity.

## Change discipline

- Keep `CHANGELOG.md` limited to release headings and concise release summaries.
- Update `README.md`, `docs/architecture.md`, or `docs/configuration.md` when a
  change affects the public product, design, or setup.
- Update the editable architecture diagram and exported PNG when the component
  or data-flow boundary changes.
- Add or update browser tests for new flows when Playwright infrastructure is
  present.
- Commit only when explicitly requested. Use atomic conventional commits and
  never add agent attribution.
