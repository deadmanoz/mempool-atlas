# Client performance

Mempool Atlas measures client performance against deterministic staged v2
publications exported from the same Rust domain model used by production. No
captured public mempool is stored in the repository.

## Commands

- `just functional-fixtures` exports the three small sources used by frontend
  development and functional Playwright coverage.
- `just perf-fixtures` also exports two 70,000-transaction sources for
  performance measurement.
- `just stage-projection` regenerates the checked-in staged byte evidence and
  enforces every node and comparison transfer ceiling. Reproducing its exact
  gzip evidence requires Node.js 22.23.2, the version pinned in CI.
- `just perf-web` builds `web/dist`, starts one production-shaped isolated
  origin, runs the desktop and constrained-mobile matrix, and writes
  `web/.perf-results/latest.json`.

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

The memory API depends on Chrome's Performance Manager, which is not present in
headless Chromium. The performance matrix therefore uses a normal Chromium
window with the documented `ForceEagerMeasureMemory` testing flag. A separate
empty-worker scenario records the worker and page baseline.

## Release gates

The constrained-mobile transfer gates use the pinned, transport-independent
throughput calibration of `159461.45607954692` bytes per second. A faster local
run reports its measured feasibility but never loosens these ceilings.

| Milestone | Wall-clock gate | Compressed-byte ceiling |
| --- | ---: | ---: |
| Node primary interactive | 30 seconds | 3,189,229 bytes |
| Node complete feature ready | 52 seconds | 6,059,535 bytes |
| Comparison primary interactive | 50 seconds | 5,740,612 bytes |
| Comparison complete feature ready | 94 seconds | 12,119,070 bytes |

The complete-byte values in this table are pre-authorized targets. Exceeding a
target sets `requires_pre_authorized_rederivation` and requires the transfer
budget to be derived and approved again. It is not an automatic allowance to
spend the remaining margin. The separate hard maxima are 6,537,919 bytes for a
complete node publication and 13,075,839 bytes for a complete comparison.
Crossing either hard maximum fails `just stage-projection`.

Node primary readiness requires the manifest, population, and selected
classifier lane. The complete population must support exact search, selection,
canonical URL state, active-lane terrain, filters, and keyboard navigation.
Membership, witness, fee, age, structure, other classifier lanes, and detail
remain visibly pending until their exact stages validate.

Comparison primary readiness requires both manifests, both populations, both
selected lanes, and both BIP-110 lanes. It must support the complete merge-join,
common and source-only regions, search, navigation, selected-classifier views,
and source-local policy views. Witness and structure-dependent claims remain
pending. Complete readiness requires every stage of both coherent
publications, every distribution and policy model committed to its view, and no
outstanding decode or derivation work. Non-interactive density-canvas raster
painting may remain frame-deferred, including while a below-fold panel is
outside its observation margin. The raw result declares this readiness contract
and the merger rejects results that omit it.

Both products must retain their primary view during bounded publication churn.
They may replace it only with another internally coherent publication and must
never combine stages declared by different manifests.

The node performance case activates and settles the complete BIP-110 terrain as
setup, then cycles all seven rule controls inside a dedicated responsiveness
interval. The release gate covers both the synchronous handlers and the cached
visible repaint. Replacement preparation and commit run first in independent
intervals so specialist terrain state cannot contaminate their measurements.

## Exact staged projection

`docs/client-performance-staged-projection.json` is the checked-in release
evidence produced by the canonical 70,000-row exporter. At gzip level 9, the
current wire format measures:

| Release gate | Exact worst case | Ceiling | Result |
| --- | ---: | ---: | --- |
| Node primary | 2,413,371 bytes | 3,189,229 bytes | pass |
| Node complete | 5,303,824 bytes | 6,059,535 bytes | pass |
| Comparison primary | 4,872,313 bytes | 5,740,612 bytes | pass |
| Comparison complete | 10,588,449 bytes | 12,119,070 bytes | pass |

At the pinned throughput, the corresponding transfer projections are 25.135,
47.261, 44.555, and 84.402 seconds. These projections reserve the remainder of
each wall-clock gate for request overhead, worker decode and validation,
derivation, packed-model construction, and rendering. `just perf-web` verifies
the end-to-end browser outcome rather than treating the byte projection as a
substitute for an interactive product.

## Functional guarantees

Functional Playwright separately proves early source metadata, explicit pending
states, primary and complete readiness, coherent supersession recovery,
transaction-detail agreement, and no horizontal page overflow at the supported
320 px minimum. Stable release runs must complete without injected faults;
supersession and rate-limit cases are exercised as separate bounded recovery
tests.
