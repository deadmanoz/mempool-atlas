# Client performance

Mempool Atlas measures client performance against deterministic staged v2
publications exported from the same Rust domain model used by production. No
captured public mempool is stored in the repository.

## Commands

- `just functional-fixtures` exports the three small sources used by frontend
  development and functional Playwright coverage.
- `just perf-fixtures` also exports two 70,000-transaction sources for
  performance measurement.
- `just publication-digest-fixture` regenerates both compact cross-language
  publication-digest goldens when the v2 preimage or source metadata cases
  change.
- `just stage-projection` regenerates the checked-in staged byte evidence and
  enforces every node and comparison transfer ceiling. Reproducing its exact
  gzip evidence requires Node.js 22.23.2, the version pinned in CI.
- `just perf-web` builds `web/dist`, starts one production-shaped isolated
  origin, runs the desktop and constrained-mobile matrix, and writes
  `web/.perf-results/latest.json`.

`just perf-web` is a manual pre-release operator step. CI does not run the full
headed-browser performance matrix. CI enforces the checked staged-byte
projection and the unit contracts for release gates, result validation,
readiness, retry behavior, and performance helpers. Before a release, an
operator must run `just perf-web` with the pinned Node.js version and review the
merged browser result as additional release evidence.

The exact Node patch pin is deliberate because Node ships the zlib
implementation that produces the checked-in gzip byte evidence. A Node patch
update is a reviewed release-evidence change: update the CI runtime and
projection pin together, regenerate the staged projection, and review the byte
and gate diff. The hard runtime failure prevents evidence produced by different
compressors from being compared as though the toolchain were unchanged. Normal
build, unit-test, and browser-test commands continue to support the wider
documented Node.js range.

The local servers serialize and compress fixture bodies once at startup. A
timed request only selects an immutable manifest or content-addressed stage and
sends it with an explicit `Content-Length`. Product assets come from the Vite
production build, not its development module graph.

The performance profile uses a fixed synthetic observation reference so its
API bodies, content identifiers, and gzip projection are reproducible. The
fixture manifest still records when each export command ran.

## Profiles and measurements

The desktop profile uses a 1440 by 900 CSS viewport without CPU or network
throttling. The constrained-mobile profile uses a 390 by 844 viewport, 150 ms
latency, 160,000 download bytes per second, 93,750 upload bytes per second, and
4x CPU slowdown. Every result records the Chromium version and exact values.

Each node and comparison run starts in a fresh browser context and records:

- every manifest and stage's encoded and decoded bytes, content encoding, TTFB,
  server processing, transfer time, decode, and validation;
- metadata, primary-interactive, and complete-feature-ready wall clocks;
- DOM count, CLS, LCP candidates, and long tasks;
- production-scale interaction handler, settle, long-task, and frame-callback
  evidence;
- page-target heap after collection; and
- worker-inclusive retained memory and attribution from the same
  cross-origin-isolated milestone.

Primary page heap and worker-inclusive retained memory are sampled in a
separate fresh browser context. Each source's second manifest request and all
completion-only stages are held there, so the primary sample cannot drift into
complete state or delay the normal run's completion clock. Replacement retained
memory is sampled after a complete candidate is ready but before the active
publication is released. The raw result records the separate primary sample as
`primary_memory_sample` and the merger rejects missing isolated-context
provenance. Replacement retained memory measures the deliberate coexistence
endpoint, not a transient allocation peak. Long-task and animation-frame gates
independently cover responsiveness during decode, derivation, replacement
preparation, and replacement commit. The raw result records the included
responsiveness intervals. Forced garbage collection and memory measurement sit
between those intervals and are deliberately excluded from the responsiveness
gate.

The node run takes another collected memory sample after activating the
complete BIP-110 terrain and visibly rendering each of its seven rule
selections. This specialist sample includes the logical glyph layout, compact
rule indexes, and retained dim/highlight raster pair. Its release ceilings are
90,000,000 bytes of page heap and 120 MiB of worker-inclusive memory, separate
from the 40,000,000-byte and 60 MiB complete-model ceilings.

Canvas backing stores are outside the V8 heap and worker-inclusive measurements.
Each retained specialist raster is therefore capped at 4,194,304 pixels; the
dim/highlight pair has a nominal 32 MiB RGBA ceiling even on high-density large
displays. The node terrain may retain one additional current base at the same
pixel cap so transaction-only selection restores the existing composition and
paints one local focus marker without replaying every transaction. That base
has a nominal 16 MiB RGBA ceiling and is discarded with the terrain layout.
The comparison terrain uses the same 4,194,304-pixel, nominal 16 MiB retained
base ceiling for transaction-only selection. It falls back to a progressive
repaint above that cap and releases the backing store when its publication is
invalidated or replaced.

The memory API depends on Chrome's Performance Manager, which is not present in
headless Chromium. The performance matrix therefore uses a normal Chromium
window with the documented `ForceEagerMeasureMemory` testing flag. A separate
empty-worker scenario records the worker and page baseline.

## Measurement hooks

The production browser bundle deliberately reads two optional page globals:
`window.__atlasCandidateReadyHook` can hold a fully prepared replacement before
commit, and `window.__atlasPublicationAttemptHook` can hold a node or comparison
publication attempt before candidate preparation. Functional and performance
Playwright use these hooks to observe atomic replacement and retained memory at
boundaries that cannot be sampled reliably from outside the page. When the
globals are absent, production behavior is unchanged.

The hook promises have no local timeout by design. A measurement may hold a
boundary until its sample is complete, while the owning request's abort signal
still releases a superseded or cancelled attempt. A timeout would make retained
memory evidence depend on machine speed and could commit a candidate during a
sample. The tradeoff is that any trusted script already executing in the page
can assign one of these globals and stall publication progress. The hooks are
test coordination points, not a security boundary. Keep the production CSP
`script-src` allowlist narrow, review any analytics script origin as trusted
code, and do not weaken CSP to expose or operate the hooks.

## Release gates

The constrained-mobile transfer gates use the pinned, transport-independent
throughput calibration of `159461.45607954692` bytes per second. A faster local
run reports its measured feasibility but never loosens these ceilings.
`web/perf/release-gates.mjs` is the executable source of truth for the
projection, browser, and merged-result ceilings; its unit test keeps the values
below synchronized with that contract.

The complete executable gate inventory is reproduced below. The unit test
compares this JSON recursively with `RELEASE_GATES`, including explicit `null`
values for gates that do not apply to a scenario.

<!-- release-gates:start -->

```json
{
  "metadata_usable_ms": 5000,
  "maximum_responsiveness_long_task_ms": 200,
  "interaction_handler_ms": 200,
  "interaction_settle_ms": 5000,
  "bip110_rule_navigation_handler_ms": 200,
  "maximum_animation_frame_callback_ms": {
    "desktop": 8,
    "mobile-slow-4g": 16
  },
  "cls": 0.1,
  "node": {
    "primary_interaction_ms": 30000,
    "complete_feature_ready_ms": 52000,
    "primary_transfer_budget_ms": 20000,
    "complete_transfer_budget_ms": 38000,
    "primary_encoded_body_bytes": 3189229,
    "complete_encoded_body_bytes": 6059535,
    "page_heap_bytes": 40000000,
    "cross_context_bytes": 62914560,
    "replacement_retained_bytes": 94371840,
    "bip110_page_heap_bytes": 90000000,
    "bip110_cross_context_bytes": 125829120
  },
  "comparison": {
    "primary_interaction_ms": 50000,
    "complete_feature_ready_ms": 94000,
    "primary_transfer_budget_ms": 36000,
    "complete_transfer_budget_ms": 76000,
    "primary_encoded_body_bytes": 5740612,
    "complete_encoded_body_bytes": 12119070,
    "page_heap_bytes": 60000000,
    "cross_context_bytes": 104857600,
    "replacement_retained_bytes": 157286400,
    "bip110_page_heap_bytes": null,
    "bip110_cross_context_bytes": null
  }
}
```

<!-- release-gates:end -->

| Milestone                         | Wall-clock gate | Compressed-byte ceiling |
| --------------------------------- | --------------: | ----------------------: |
| Node primary interactive          |      30 seconds |         3,189,229 bytes |
| Node complete feature ready       |      52 seconds |         6,059,535 bytes |
| Comparison primary interactive    |      50 seconds |         5,740,612 bytes |
| Comparison complete feature ready |      94 seconds |        12,119,070 bytes |

The complete-byte values in this table are pre-authorized targets. Exceeding a
target sets `requires_pre_authorized_rederivation` and requires the transfer
budget to be derived and approved again. It is not an automatic allowance to
spend the remaining margin. The separate hard maxima are 6,537,919 bytes for a
complete node publication and 13,075,839 bytes for a complete comparison.
`just stage-projection` records a target crossing for re-authorization and
fails only at a hard maximum. `just perf-web` additionally treats each target
as its hard `complete_bytes` browser gate, so crossing a target fails the full
performance matrix until the budget is re-derived and approved.

Node primary readiness requires the manifest, population, and selected
classifier lane. The primary population must support exact search, selection,
canonical URL state, active-lane terrain, label or rule filters, and keyboard
navigation. The fee, age, and virtual-size filter form is not available yet.
Membership, witness, structure, other classifier lanes, and detail remain
visibly pending until their exact stages validate.

Comparison primary readiness requires both manifests, both populations, both
selected BIP-110 lanes. It must support the complete merge-join, common and
source-only regions, search, navigation, and source-local BIP-110 policy views.
Witness and structure-dependent claims remain pending. Complete readiness
requires every stage of both coherent publications, the currently selected
distribution and policy models committed to their views, and no outstanding
decode or derivation work required by the visible complete view.
Non-interactive density-canvas raster painting may remain frame-deferred,
including while a below-fold panel is outside its observation margin. The raw
result declares this readiness contract and the merger rejects results that
omit it.

Both products must retain their primary view during bounded publication churn.
They may replace it only with another internally coherent publication and must
never combine stages declared by different manifests.

Each per-source publication load has a 120-second browser deadline. This leaves
headroom above the 94-second comparison readiness gate while ensuring a stalled
manifest or stage request returns control to the existing retained-publication
and partial-availability handling.

The node performance case activates and settles the complete BIP-110 terrain as
setup, then cycles all seven rule controls inside a dedicated responsiveness
interval. The release gate covers both the synchronous handlers and the cached
visible repaint. Replacement preparation and commit run first in independent
intervals so specialist terrain state cannot contaminate their measurements.
Transaction selection is a separate visual contract: it preserves the active
label, rule, or bucket emphasis and adds one local highlight instead of using a
whole-terrain restyle as a shortcut for the interaction gate.

After the complete memory sample, each stable node run activates the fee-rate
by age lens and waits for its 70,000-row canvas to paint, then changes the
minimum fee rate and waits for the automatically filtered canvas to repaint.
Each stable comparison run switches the source-local distributions from all
transactions to the common population and waits for both aggregate models to
commit. A complete comparison candidate prepares that common source-local model
before readiness and adopts only its aggregate arrays into the candidate-owned
distribution cache. It retains no additional transaction population. The much
smaller source-only scopes remain lazy. Every interaction records its
synchronous handler time, input-to-settled time, outcome, and exact
responsiveness interval in the raw and merged results. The handler ceiling is
200 ms and the settle ceiling is 5 seconds. The existing 200 ms long-task
ceiling and profile-specific 8 ms desktop or 16 ms constrained-mobile
animation-frame ceiling apply across those intervals too. Offscreen deferred
density raster work remains outside the scope-switch settle point, consistent
with the complete-readiness contract.

## Exact staged projection

`docs/client-performance-staged-projection.json` is the checked-in release
evidence produced by the canonical 70,000-row exporter. At gzip level 6, the
current wire format measures:

| Release gate        | Exact worst case |          Ceiling | Result |
| ------------------- | ---------------: | ---------------: | ------ |
| Node primary        |  2,429,614 bytes |  3,189,229 bytes | pass   |
| Node complete       |  5,372,524 bytes |  6,059,535 bytes | pass   |
| Comparison primary  |  4,910,300 bytes |  5,740,612 bytes | pass   |
| Comparison complete | 10,724,914 bytes | 12,119,070 bytes | pass   |

At the pinned throughput, the corresponding projected wall clocks are 25.237,
47.692, 44.794, and 85.258 seconds. Each includes a two-second request
allowance and, respectively, an 8, 12, 12, or 16-second processing allowance.
The remaining margins against the release gates are 4.763, 4.308, 5.206, and
8.742 seconds. `just perf-web` verifies the end-to-end browser outcome rather
than treating the projection as a substitute for an interactive product.

## Functional guarantees

Functional Playwright proves early source metadata, explicit pending states,
primary and complete readiness, atomic replacement while a candidate is held,
and transaction-detail deferral until complete membership arrives. The early
metadata group runs its mobile cases at the 320 px minimum; its two loading
transitions assert no horizontal page overflow before and after data arrives.
The completed-page viewport suite covers 390, 768, and 1440 px widths.

Performance Playwright separately injects one `503` into a node manifest
refresh and one completion-stage refresh in comparison, then proves that the
retained complete publication remains usable and the next refresh recovers.
Stable release measurements run without injected faults. Worker unit tests own
the bounded `409` supersession and `429` rate-limit retry contracts, while API
and worker unit tests own transaction-detail parsing and agreement with the
loaded publication.
