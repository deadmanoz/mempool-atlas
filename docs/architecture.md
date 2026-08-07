# Architecture

Mempool Atlas is a current-state viewer. It periodically observes independent
Bitcoin mempools, progressively classifies their current transactions through
independent lenses, and serves the newest successful observation for each
source.

![Mempool Atlas architecture](assets/architecture.png)

The editable source is [`assets/architecture.drawio`](assets/architecture.drawio).

## System boundary

Atlas runs as one process with five responsibilities:

| Component | Responsibility |
| --- | --- |
| Membership collector | Fetch and validate one complete mempool observation per source |
| Shared fact resolver | Resolve bounded raw transaction and prevout facts once per current generation |
| Classifier lenses | Evaluate exact properties, heuristic shapes, data fingerprints, and BIP-110 compatibility independently |
| Current-state publisher | Atomically expose source snapshots, lifecycle, and transaction detail |
| Web and API server | Serve source-local data and the two browser products |

Each Bitcoin node remains an independent authority for its own mempool. Atlas
does not install software beside the node, combine source membership on the
server, or retain observations after they are replaced.

## Complete membership observations

For each configured source, `src/rpc.rs` performs this sequence:

1. Read the starting chain height and best block hash.
2. Read the reported mempool size and enforce the configured entry limit.
3. Fetch verbose `getrawmempool` membership, keeping each entry's reported
   weight, ancestry counts, virtual sizes, and delta-adjusted ancestor fees,
   and effective replaceability alongside its identity, virtual size, exact
   fee, and entry time.
4. Validate each `txid`, `wtxid`, virtual size, fee, and entry time.
5. Enforce the entry limit while decoding and sort entries by `txid`.
6. Read the ending chain height and best block hash.
7. Publish only if the starting and ending chain-tip reads match.

`getmempoolinfo` and `getrawmempool` are not one atomic read. Atlas compares
chain-tip reads before and after them and rejects the collection when they
differ. Matching reads bind the observation to that reported tip, but do not
freeze mempool changes or prove that the tip never changed between the two
reads. A failed poll leaves the previous successful snapshot visible and marks
it stale. Failure of one source does not prevent later sources in the same
round from being attempted.

Membership requests go through the shared bounded HTTP policy in
`src/rpc_transport.rs`: redirects are never followed, so the Basic credential
can only reach the configured origin; every request carries an explicit
timeout; and each response is read through a byte cap. Control reads use a
small fixed cap, while the verbose mempool cap is derived from the configured
entry limit, so a misbehaving endpoint cannot buffer arbitrarily more memory
than the configured mempool bound implies.

Every accepted observation records collection start, completion, and duration.
Age is derived relative to observation time, not the browser clock.

## Progressive transaction classification

Fresh membership is useful before every transaction has been enriched. Atlas
therefore publishes membership immediately, carrying forward only results
whose exact `txid` and `wtxid` survive from the prior generation.

`src/classification.rs` then advances the current generation through bounded
candidate and fact waves. Its presence-preserving JSON-RPC batching lives in
`src/classification_rpc.rs`, on the same shared `src/rpc_transport.rs` HTTP
policy that membership uses:

1. Fetch and verify raw candidate transactions with `getrawtransaction`.
2. Resolve unconfirmed parent outputs from current mempool transactions.
3. Resolve confirmed prevout scripts with `gettxout(txid, vout, false)`.
4. Evaluate a candidate only after every required script is present or has a
   genuine terminal result.
5. Run each independent classifier against the same bounded fact set.
6. Publish completed results in revisioned batches.

A verified candidate remains pending across fact-wave boundaries, so bounded
work does not repeatedly fetch the same transaction. Positive confirmed
scripts may be reused under bounded eviction. Nulls, failures, and departed
transactions are not cached as facts.

Collection state is kept separate from classifier evidence. Unscheduled work,
capacity deferral, transport errors, malformed responses, and absent JSON-RPC
members leave public classifier results unset. They do not manufacture a
negative match or indeterminate policy result. A present JSON `result: null` is
terminal only in the specific `gettxout` case where the outpoint is known not
to be a current mempool parent.

The classifier reports one of three public lifecycle states:

| State | Meaning |
| --- | --- |
| `classifying` | Eligible classification work remains for the current membership |
| `complete` | No eligible work remains, although some results may be unavailable |
| `paused` | A systemic or internal failure stopped this generation |

Membership health and classification lifecycle are independent. Replacement
membership supersedes stale classifier work, and results from an older
generation cannot update current state.

## Independent classifier lenses

`src/classifiers.rs` implements four versioned lenses over the
resolved fact set:

| Lens | Method | Question answered |
| --- | --- | --- |
| `transaction_properties` | Exact, multi-label | Which serialized and script-family properties are present? |
| `transaction_shape` | Heuristic, multi-label | Which explicitly defined transaction-shape patterns match? |
| `data_protocols` | Fingerprint, multi-label | Which supported data-carrier byte patterns are present? |
| `knots_bip110` | Policy rule set | How does this witness variant evaluate against deployed BIP-110 policy? |

The catalog, rules, thresholds, missing-fact behavior, and limitations are
defined in [`classification.md`](classification.md). Each lens versions its
own vocabulary and semantics. Atlas does not reconcile them into one taxonomy
or form cross-product categories. A transaction can legitimately match labels
from several lenses at once.

Compact snapshot entries carry label keys and completion state. Bounded
evidence is retained in transaction detail. A partial result preserves proven
labels while naming unavailable fact classes, so a missing label is never
presented as a reliable negative.

## Policy evaluator

`src/bip110/` is a private pure evaluator with no RPC, storage, async runtime,
or clock. Atlas uses its Bitcoin Knots mempool-policy mode for the seven
BIP-110 rules. The separate consensus mode, official vectors, and client-source
cross-checks remain test-only machinery in that module; the website does not
describe a policy violation as consensus invalidity. The public wire evaluator
identifier remains `rdts-rules`.

The BIP-110 assessment contains compatible, violating, or indeterminate status
plus typed evidence and missing-fact counts. A rule can have both a proven
violation and unresolved facts. Evidence is bounded to one exemplar of each
kind per rule. Its generic `knots_bip110` classifier result is a projection of
this specialist assessment, which remains available for rule-terrain
compatibility.

## Publication model

`src/runtime.rs` owns one in-memory runtime per source and one shared RPC work
gate. Membership rounds poll sources sequentially. Classification turns rotate
fairly between current source generations and release the gate between bounded
slices.

Classification work hands `src/runtime/publisher.rs` a strictly ordered
revision delta containing only changed transaction results. The publisher owns
the accumulated exact-result map and all reader-visible state. It materializes
and validates the complete snapshot, derives summaries, and encodes JSON after
the RPC gate has been released. Membership RPC can therefore proceed while a
classification revision is prepared. Generation and revision guards reject
superseded work at the atomic commit point.

One publication is prepared as a complete v2 bundle before the commit point.
The publisher encodes and validates the manifest, population, membership,
structure, catalog-ordered classifier stages, and matching detail map without
holding the reader lock. One atomic promotion then replaces:

- the source summary and source-scoped domain snapshot;
- its classification lifecycle and revision;
- its classifier catalog, coverage summaries, and compact transaction results;
- matching transaction-detail records; and
- the content-addressed stage map and manifest validator.

Readers therefore see one coherent publication. Failed preparation cannot
advance the domain snapshot or detail. A poll failure after a successful
publication produces a new manifest containing the stale source metadata while
retaining the exact prior stage buffers and content identifiers. Failure before
the first publication returns an exact non-cacheable `v2_unavailable` problem
response. Transaction detail is accepted by the browser only when source,
observation, transaction identity, witness identity, and assessment agree with
the visible publication.

Source discovery also publishes `atlas_version` from Rust's compiled
`CARGO_PKG_VERSION`. Both browser products render this server-authoritative
value in the shared header, so the visible version describes the running Atlas
process rather than an independently versioned static package.

Each manifest and stage receives a weak `ETag`. A manifest validator changes
when membership, classification, lifecycle, poll-start, or failure metadata
changes. Stage validators are their SHA-256 content identifiers and change only
with their exact bodies. Conditional reads of current representations return
`304` without sending a body. A well-formed content identifier absent from the
current publication returns non-cacheable `409`. An identifier that belongs to
a different current stage, or a request for a stage kind or classifier that is
not present, returns non-cacheable `404`. Malformed identifiers or stage kinds
return non-cacheable `400`, all before conditional validation. Source discovery,
transaction detail, failures, and responses before the first publication remain
non-cacheable. Clients that advertise gzip support receive compressed JSON.

The process retains no application data on disk. Restarting discards current
state and readiness returns only after a new valid observation is available.

## Browser products

Both browser products first render the selected entries from the lightweight
`/api/v2/sources` response. Source label, availability, retained observation,
chain tip, membership totals, classification progress, and poll failure are
therefore visible before stage loading begins. The shared source
view keeps discovery, metadata, snapshot loading, derivation, and interactive
phases separate from ready, stale, waiting, and error availability.

A dedicated worker fetches and validates the current manifest, population, and
selected classifier lane for the primary view. Membership, structure, and the
remaining catalog lanes progress behind that primary quorum. Stage digests,
dependency identifiers, row counts, and publication identity are checked before
the worker commits one internally coherent packed store. Superseded-stage `409`
responses trigger a bounded whole-publication retry; the browser never combines
stages from different manifests. Packed columns, bitsets, and first-seen result
dictionaries remain worker-owned, with presentation adapters exposing rows only
on demand rather than retaining the former full-row object graph.

The node viewer renders one source snapshot. Its default Classifications view
lets the user select one declared lens, inspect marginal label populations, and
open matching transaction samples. Multi-label populations can overlap. The
browser does not combine labels from different classifiers.

The same selection drives the Buckets view. Classifier lenses partition
transactions by complete, partial, or unavailable coverage and a lens-specific
presentation adapter. Transaction properties uses broad script-profile groups;
its exact labels remain marginal and transaction-level. Smaller generic lenses
retain exact observed label-set buckets. Every transaction belongs to exactly
one terrain region.

Below the viewer, a Snapshot distributions section derives nine aggregate
panels in the browser from the same published snapshot, one per question in
classification-first order: composition (one bar per catalog lens), fee
structure (a spectrum stacked by the selected classifier's buckets), ancestor
fee rate (delta-adjusted ancestor fees over ancestor virtual size), shape (a
joint fee-rate-by-size density heatmap with marginals), age (a bucket-by-age
mosaic), data carriage (OP_RETURN carried bytes by data-protocols bucket),
complexity (an input-count by output-count density), entanglement (banded
unconfirmed ancestor and descendant counts with the source-reported
replaceability share), and total output value (the sum of every output,
including change, by the selected classifier's buckets). Panels that need structure facts state
their coverage and skip transactions whose facts have not arrived. Multi-series
panels cap at the largest buckets and roll the remainder into one labelled
aggregate so every panel still covers the whole population. An aggregate-only
model builds all panels in one pass and is cached by snapshot identity and the
semantic classifier, metric, scope, and display limits. Revisited selections do
not rescan the transaction population, and the cache retains no transaction
arrays. Aggregation adds no per-transaction DOM nodes. Composition segments and
mosaic columns deep-link into the corresponding Buckets selection. An honesty
banner above the explore controls names the retained observation's age and poll
failure when a source is stale and reuses the paused-classification summary when
assessments are missing.

The node and comparison distribution sections each own their complete DOM,
cache, resize, rendering, and reset lifecycle behind a small view interface.
The page entry points retain source loading, URL state, and coordination between
views rather than accumulating panel-specific implementation.

Selecting `knots_bip110` uses a specialist adapter over the same terrain engine.
Compatible, indeterminate, and unavailable assessments remain distinct.
Complete violations are grouped by their exact set of violated rules, while
partial violations retain separate proven-plus-unresolved sets. Rule controls
are marginal filters and the full seven-rule evidence remains available only in
this presentation. Fee rate by age remains a secondary view.

The comparison page fetches two independent snapshots and merge-joins their
sorted `txid` arrays in the browser. It derives present-in-both and two
observed-only regions without creating a server-side comparison object. One
policy projection pass builds source-local aggregate rows, including separate
left and right policy views for transactions common to both snapshots. Exact
and partial violation signatures remain separate, marginal rule counts may
overlap, and filter samples retain at most twelve deterministic entries.
Repeated filter selections reuse the aggregate and bounded sample. Mirrored
source-local distribution panels render each side's complete snapshot on
shared fixed axes
using the same browser-derived builders as the node viewer across all nine
questions; the two populations are summarized independently and never merged.
A population scope selector restricts every mirrored panel to the whole
snapshot, the transactions present in both snapshots, or the transactions
observed in only one source, using the same merge-join regions as the
membership canvas; a side with no members in the selected population says so
rather than showing an empty chart as data.

The membership regions and primary comparison workspace precede the secondary
distribution and policy panels in document order. Stable source-card,
sampling, workspace, and panel geometry prevents later derivation from
displacing the interactive comparison as those sections populate.

Both products keep source, classifier or policy selection, and optional
transaction state in the URL. Snapshot and detail requests use generation
guards so obsolete responses cannot replace a newer source or pair selection.
On the comparison page, changing only the selected transaction reuses the
current population view. Repeating the same interactive selection is a true
no-op, while a refreshed snapshot pair still reapplies state and reloads detail
against the new comparison identity.

## Network and credential boundary

Atlas accepts one to four source definitions and reads RPC passwords from
separate named files. Source configuration contains no password values. The
HTTP listener is restricted to loopback. Public traffic terminates at
Cloudflare, passes through a named Cloudflare Tunnel, and reaches Atlas only on
that loopback listener. Cloudflare owns public TLS, compression, cache rules,
WAF, request limits, and security headers. Operational health paths remain
private.

Bitcoin RPC credentials are not inherently read-only. The Atlas identity
should be restricted server-side to:

- `getblockchaininfo`
- `getmempoolinfo`
- `getrawmempool`
- `getrawtransaction`
- `gettxout`

Atlas does not require a database, message broker, ZMQ feed, node-side agent,
or an additional node listener.

The tunnel changes only public ingress. It does not schedule collection work:
Atlas continues to poll and classify every configured source regardless of
which source a browser is viewing.

## Design decisions

Only durable decisions that are not obvious from the code are retained:

- [ADR 0001: Keep only independent current snapshots](adr/0001-current-state-snapshots.md)
- [ADR 0002: Separate policy facts from collection state](adr/0002-policy-facts-and-collection-state.md)
- [ADR 0003: Derive comparison and terrain partitions in the browser](adr/0003-browser-derived-views.md)
