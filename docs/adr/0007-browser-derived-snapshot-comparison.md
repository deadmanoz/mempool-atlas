# Derive comparison from independent current snapshots in the browser

Status: accepted

## Context

The first attempt at multi-node comparison coupled node-local agents, retained
event databases, delivery queues, and a central reducer. Unbounded node storage
and an opaque deployment followed. Attempt #3 proved that one presentation-host
process can instead poll a complete mempool, classify it under explicit bounds,
retain only current state, and serve a useful single-node product.

Comparison needs two complete source observations, but it does not need history,
arrival events, a combined database, or another server-side projection. The
presentation VPS is the constrained machine. A naive multi-source extension
would overlap buffered verbose responses and independently drain several large
classification working sets. It could recreate the same capacity problem in a
different form.

The observations are not simultaneous. Their collection windows may be
separated by several seconds, and a block may arrive during either collection.
Absence from one observation is therefore not evidence that its node rejected,
filtered, or failed to relay a transaction.

## Decision

Configure one to four independent sources in one root-controlled JSON file.
Each entry provides a stable ID and label, private RPC URL, RPC username, and a
safe systemd credential name. Addresses, credentials, and deployment inventory
remain outside this repository.

One `AtlasRuntime` coordinates all configured sources:

- a membership round owns one service-wide RPC gate and polls every source in
  configured order;
- one source failure is recorded independently and does not stop later sources;
- each membership observation is bracketed by matching chain-tip reads and
  records its successful collection start, completion, and duration;
- classification advances source-local generations round-robin under the same
  gate, releasing it between bounded slices so a waiting membership round takes
  priority;
- only sources that publish replacement membership are rearmed after a paused
  generation; and
- one total classification cache budget is divided among sources. Each
  source's pending-script ceiling is also capped by its share.

The service retains one current snapshot, classification state, detail map, and
shared encoded response per source. It does not retain a pair projection or
comparison cache.

The comparison is a separate browser entry. It fetches two existing
source-scoped snapshot responses, validates each independently, and performs a
linear merge of their strictly sorted `txid` arrays. The union is partitioned
into exactly three symmetric regions:

1. present in both sampled snapshots;
2. observed only in the left snapshot; and
3. observed only in the right snapshot.

Every `txid` appears once. A common `txid` retains both source-local entries,
including both `wtxid`, fee, virtual size, entry time, and BIP-110 assessment.
Different witness variants are explicit. There is no merged assessment and no
single shared virtual size for a common transaction.

The interface displays both collection windows, observation skew, chain-tip
agreement, freshness, independent source errors, and source-specific totals.
Within the three membership regions it reuses the exact complete and partial
BIP-110 bucket semantics from the node viewer. Browser refresh reads already
published state and never triggers node RPC.

The comparison begins with a count-only policy matrix derived in one browser
pass over the three membership arrays. It shows four source-local rows:
left-only assessed by the left source, common assessed by the left source,
common assessed by the right source, and right-only assessed by the right
source. Each row partitions its population into compatible, violating,
indeterminate, and unclassified. Violating includes exact and partly unresolved
assessments. At most three dominant exact rule combinations are shown per row,
ordered by count and then canonical signature, with hidden combination and
transaction totals made explicit. Matrix controls drive the existing canonical
region, source-side, policy-filter, sample, and inspector transition rather than
creating a second comparison state.

Changing a source pair aborts obsolete snapshot and detail requests and guards
against late results. Canvas geometry is reused for selection-only paints and
invalidated by replacement data, viewport size, or device pixel ratio. A
region-scoped virtual listbox provides bounded keyboard navigation across every
transaction without rendering one DOM option per entry.

The first production pair is Bitcoin Core and Bitcoin Knots because it gives
the comparison direct product value alongside the BIP-110 policy view. Libre
Relay is a later resource and third-source test after the two-source deployment
has measured memory, cadence, and transport behavior.

## Consequences

The VPS does not allocate or retain a third full comparison dataset. It pays
for one current encoded response and source-local classification state per
configured node. Large membership responses and classification RPC slices do
not overlap with each other. Multiple source snapshots and caches still add
resident memory, so the two-source rollout must prove actual memory behavior
before a third source is enabled.

Sequential membership creates visible sampling skew by design. The browser
must show that skew and cannot describe the observations as simultaneous. A
changed chain tip rejects one source poll, keeping its previous snapshot visible
as stale rather than publishing a block-boundary mixture.

The browser temporarily holds two decoded snapshots and the derived membership
indexes. That moves comparison-specific transient work away from the constrained
VPS, but client code must remain linear, avoid one DOM node per transaction, and
reuse Canvas rendering and bounded tables.

Classification remains source-bound even when two sources contain the same
transaction. This preserves generation, witness, and fact provenance. It also
means classifications may be at different revisions when the comparison is
viewed, which the interface reports instead of hiding.

## Alternatives considered

### Build and cache a combined comparison response on the server

This would duplicate a potentially large union beside both source snapshots and
create another invalidation and memory-lifetime problem. The existing source
responses already contain the complete data needed for a browser merge.

### Poll every source concurrently

This minimizes observation skew but overlaps `corepc-client` membership buffers
and node work. The current VPS constraint makes deterministic sequential
collection the safer initial production contract. Measured evidence can justify
a bounded change later.

### Classify only one source

That reduces memory, but makes source-only transactions on the other node a
second-class product and complicates comparison semantics. Dividing one total
budget and scheduling source-local work through one gate preserves full
classification without independent unbounded workers.

### Infer acceptance or rejection from membership differences

The snapshots differ in time, topology, fee conditions, ancestor state, and
other policy and relay inputs. Membership absence alone cannot establish a
cause, so the product uses observational wording throughout.
