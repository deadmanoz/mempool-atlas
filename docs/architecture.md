# Architecture

Mempool Atlas attempt #3 is one central process on the presentation host and
one browser client. Its scope is the current mempool of one selected Bitcoin
node, including a bounded classification of current witness variants against
the seven BIP-110 rules as deployed Bitcoin Knots mempool policy.

[ADR 0005](adr/0005-resolve-policy-facts-before-evaluation.md) defines the
pending fact resolver, explicit-null semantics, positive prevout cache, and
P2SH evaluation described here. It supersedes ADR 0003's attempt-once
classification details.

```mermaid
flowchart TB
    subgraph node["Node host"]
        bitcoind["Bitcoin Core or Knots"]
        rpc_proxy["Existing private nginx RPC proxy"]
        bitcoind --> rpc_proxy
    end

    subgraph presentation["Presentation host: one Atlas process"]
        membership["Complete membership collector"]
        generation["Install current in-memory generation<br/>reuse exact surviving classifications"]
        enrichment["Pending candidate and fact waves<br/>current wtxid and positive script caches"]
        evaluator["Pure seven-rule evaluator"]
        guard["Generation and revision guard"]
        current["Latest membership, classification revision,<br/>detail map, and encoded response in memory"]
        api["Source-scoped JSON API"]
        static["Static website"]
        membership --> generation
        generation -->|"publish membership first"| current
        generation -->|"wake resolver"| enrichment
        enrichment --> evaluator
        evaluator --> guard
        guard -->|"publish current progress"| current
        current --> api
    end

    rpc_proxy -->|"existing WireGuard: membership RPC"| membership
    rpc_proxy -->|"existing WireGuard: bounded detail RPC"| enrichment
    browser["Browser"] --> static
    browser --> api
```

## One process on the presentation host

`apps/atlas/` owns collection, current state, the API, and static file serving:

- `rpc.rs` collects complete membership with `getmempoolinfo`, verbose
  `getrawmempool`, and `getblockchaininfo` through `corepc-client`.
- `policy.rs` resolves bounded windows of current witness variants through
  batched concurrent `getrawtransaction` and
  `gettxout(txid, vout, false)` calls. It owns same-generation pending fact
  waves, exact surviving classification reuse, bounded positive script caches,
  and stale result rejection.
- `model.rs` defines the complete flat snapshot, compact classification, and
  typed transaction-detail contracts.
- `runtime.rs` runs membership and classification as independent loops.
  Membership is published before new policy work, and each successful
  current-generation assessment publication atomically replaces the snapshot
  and matching detail map with a strictly newer revision.
- `api.rs` exposes health, readiness, source discovery, current membership,
  current transaction detail, and the built website.
- `main.rs` owns configuration, the loopback listener, credential-file loading,
  and graceful shutdown.

`crates/rdts-rules/` is a pure evaluator. It has no I/O, async state, chain
access, or clock. It exposes separate consensus and mempool-policy modes. Atlas
uses only the mempool-policy mode, which applies all seven rules without a
deployment activation gate or UTXO grandfathering.

The initial executable configures exactly one source. `SourceRegistry` keeps the
read contract source-scoped so a later comparison product can add independent
sources without inventing a combined mempool.

## Snapshot contract

Each transaction contains current membership facts plus a compact optional
classification:

| Field | Meaning |
| --- | --- |
| `txid` | Current membership key |
| `wtxid` | Current witness variant reported by the source |
| `vsize` | Transaction virtual size |
| `fee_sats` | Exact base fee in integer satoshis |
| `entered_at_ms` | Node-reported mempool entry time |
| `bip110` | Compatible, violating, indeterminate, or `null` when not yet classified |

The snapshot also records source identity, membership observation time,
membership-local `classification_revision`, block height, best block hash,
transaction count, total virtual size, evaluator identity, policy scope, and
the four classification totals. The revision begins at zero for every
membership generation and advances with each published assessment batch.
Transactions are strictly sorted by txid. This makes equality, future
merge-based comparison, and payload validation straightforward.

The compact assessment identifies all proven violated rules, all rules with
missing facts, and the deterministic first rejection when it can be known. A
transaction can have a proven rule violation while another input for that same
rule remains unresolved. In that case the rule appears in both lists. Missing
facts before a proven later rejection can also leave `primary_rule` unset even
though the transaction status is `violating`.

Age is always calculated against `observed_at_ms`. Rendering against the
browser's current clock would make an immutable snapshot appear to change.

## Classification meaning

The production classification is compatibility with the RDTS/BIP-110 rules
enforced as standard mempool policy by the deployed Bitcoin Knots client. It is
not proof that the observed source rejected a transaction, and it is not a
claim that a transaction is consensus-invalid. The evaluator reports these
seven rule checks:

| Rule | Public ID | Check |
| --- | --- | --- |
| 1 | `output_size` | Output script size |
| 2 | `element_size` | Serialized script or witness element size |
| 3 | `undefined_version` | Undefined witness or Tapleaf version |
| 4 | `taproot_annex` | Taproot annex presence |
| 5 | `control_block_size` | Taproot control-block size |
| 6 | `op_success` | `OP_SUCCESS` in Tapscript |
| 7 | `tapscript_op_if` | Executed `OP_IF` or `OP_NOTIF` in Tapscript |

Every classified transaction has one typed outcome per rule. A definite match
contains evidence such as byte lengths, input or output positions, opcodes, and
limits. A successful null-shaped `gettxout` response from the trusted Bitcoin
Core endpoint becomes a typed missing script fact only when the outpoint is
known not to be a current mempool parent. A null fallback after parent-raw
failure is ambiguous and remains collection state until its bounded attempts
are exhausted. Capacity deferral, unscheduled work, and failed collection
likewise leave the transaction unclassified rather than manufacturing an
evaluator unknown.

P2SH evaluation follows deployed Knots policy. The final scriptSig item is the
redeemScript blob and is exempt from Rule 2, while earlier scriptSig items and
pushes inside the redeemScript remain subject to it. Exact P2SH-P2WPKH and
P2SH-P2WSH programs dispatch through nested witness evaluation. P2SH-wrapped
witness versions 1 through 16 use Rule 3 because Taproot and P2A are native
only.

Atlas retains the exact evidence and missing-fact counts for each rule, but
only one evidence exemplar and one missing-fact exemplar. This bounds the
reader-visible detail retained for one transaction. The full evaluator result
exists only while the current transaction is being reduced to this compact
form.

## Collection and enrichment

Every poll first establishes complete membership:

1. `getmempoolinfo` preflights the reported entry count.
2. `getrawmempool true` returns the complete current membership, including
   `txid`, `wtxid`, `vsize`, entry time, and base fee.
3. `getblockchaininfo` binds the observation to the source chain tip.

Before requesting the large verbose response, Atlas rejects a node-reported
mempool above `ATLAS_MAX_MEMPOOL_ENTRIES`. The custom deserializer enforces the
limit again because membership can change between the two RPC calls. It rejects
the complete poll if a required membership field is absent, malformed,
inexact, overflowing, or unsafe for JSON.

Complete membership installation and policy enrichment are separate. A
successful collector result first installs a new process-local generation,
copies only classifications whose exact `txid` and `wtxid` survive, and reuses
current-transaction outputs only for the same exact variant. Validated positive
confirmed `OutPoint` scripts may survive membership generations under the
configured auxiliary cache bound and eviction policy. Null and failed lookups
are never cached. Atlas then materializes, encodes, and publishes complete
membership as revision 0 before waking classification.

The classification loop drains a bounded pending resolver for that generation:

1. Admit at most `ATLAS_CLASSIFICATION_SLICE_ENTRIES` current witness variants,
   2,048 by default and configurable up to a hard 8,192-entry candidate-window
   maximum. Admission also stops at a 256 MiB aggregate candidate-raw response
   estimate.
2. Fetch and verify candidate raw transactions in batches of at most 256, split
   by a `vsize`-based 16 MiB response estimate. A successfully admitted raw
   transaction remains pending across same-generation fact waves and is not
   refetched merely because its script work crosses a wave boundary.
3. Resolve input scripts first from exact current-parent outputs or the bounded
   positive script cache. Fetch unresolved unconfirmed parents through a raw
   fact wave capped at 8,192 transactions and its own 256 MiB aggregate
   estimate.
4. Use 65,536 unique required prevouts as the target for one pending window.
   Always admit one candidate when that transaction alone exceeds the count
   target; its raw and retained-script byte bounds still apply. Resolve
   remaining confirmed scripts with `gettxout(txid, vout, false)`.
   Their nominal 512-request batch cap is reduced by the 16 MiB estimate and
   64 KiB per-script-hex bound to a current effective maximum of 254. Each
   confirmed-prevout fact wave has a separate 256 MiB aggregate estimate,
   permitting 4,064 worst-case calls with the current constants. Facts postponed
   by a wave bound remain pending and later waves continue fairly rather than
   restarting from the same sorted prefix.
5. Run up to `ATLAS_CLASSIFICATION_RPC_LANES` raw batches concurrently, four by
   default and at most eight. Confirmed-prevout work uses half that lane count
   rounded up.
6. Evaluate a transaction only after every required input script is present or
   has a terminal result. A successful null-shaped `gettxout` response from the
   trusted Bitcoin Core endpoint is terminal missing data only for an outpoint
   known not to be a current mempool parent. A null parent-fallback probe is
   ambiguous. It remains collection state, as do capacity deferral, unscheduled
   work, batch or transport failure, malformed or oversized responses, and
   missing response envelopes. Each leaves public `bip110: null`.
7. Merge results only while the generation remains current. Publish a full
   current-snapshot replacement when a wave adds assessments. Fact-only
   progress continues without publishing a classification revision.
8. Give each operationally unresolved fact two attempts for its current source
   within one bounded pending window. A candidate that exhausts those attempts,
   or cannot admit a retained script under the hard pending bound, is deferred
   for the rest of the generation. Capacity recovery is bounded by the pending
   candidate count and yields and checks staleness between relief passes while
   preserving facts already fetched in the current call. Continue with queued
   and later candidate windows. Only a systemic RPC failure pauses the whole
   generation.

A missing or invalid raw transaction response leaves that membership entry
unclassified. So does a locally deferred candidate until the next successful
membership generation resets its eligibility. A transaction with a genuine
terminal missing script can produce a visible partial assessment while
independently decidable violations remain preserved. None of these conditions
invalidates fresh membership.

Classification batches use a 20-second transport timeout. Returned transaction
hex is capped at 8,000,000 characters, confirmed script hex at 64 KiB, and a
batch is rejected if its decoded JSON-RPC response envelope exceeds 16 MiB.
The minreq transport buffers and parses the response before the application can
enforce that guard. The candidate-raw, mempool-parent-raw, and
confirmed-prevout wave estimates bound planned work, not transport allocation.
The production 2 GiB memory cgroup is the hard transient boundary.

## Publication and failure

Each membership or policy-progress snapshot and detail map is built and
cross-validated before publication. The runtime also serializes the complete
snapshot response on a blocking worker. It then replaces the structured
snapshot, matching detail map, and shared encoded response under one short
write lock. Readers therefore see complete membership with the exact matching
classification revision, never membership from one generation with details
from another.

Stale policy work is rejected twice. The policy coordinator merges results only
when its generation object is still current. The runtime then accepts a
publication only when the generation matches its published membership and the
revision strictly advances. RPC schedulers check generation identity before
each replacement wave, so already in-flight requests can finish but no further
batches or downstream phases are scheduled for stale work. Stale results cannot
update current state or the shared positive script cache.

Every resolver call reports one explicit drain disposition. `Continue` means
eligible work remains and no systemic circuit breaker fired, so another bounded
wave may run. Fact-only progress does not publish or advance a revision.
`Complete` means no eligible current work remains. `Paused` means a systemic
RPC failure stopped the generation until replacement membership instead of
allowing a retry loop. `Stale` ends work for a superseded generation. The
runtime follows this disposition directly rather than inferring progress from
classification counts or RPC-failure counters.

Progress logs separately report `fact_requests`, `facts_resolved`,
`facts_missing`, `capacity_deferred`, `deferred_candidates`,
`response_failures`, `systemic_response_failures`, `missing_responses`,
`batch_failures`, and `response_bytes`.

When a wave publishes assessments, its revision is embedded in both the
snapshot and runtime publication and must match exactly.

A failed membership poll records its error but preserves the last good
observation as stale. Before the first success, readiness remains false. Batch
or response failures during best-effort classification are logged and leave
affected transactions unclassified; they do not make a fresh membership
snapshot stale.

The current `jsonrpc` 0.18 model loses one piece of wire information before
policy code runs: both literal `result: null` and an omitted `result` member
deserialize to `Response.result == None`. The trusted Bitcoin Core endpoint
emits a valid result member, but Atlas cannot independently distinguish a
malformed omission on this transport. Bead `atlas-wgx` tracks replacing or
adapting that boundary.

Each current-snapshot HTTP request clones the immutable shared response buffer
rather than allocating and serializing another large body. The presentation
reverse proxy remains responsible for compression, request rate limits, and
concurrency limits before public exposure.

Detailed RPC and validation errors remain in service logs. The public source
status uses a stable, sanitised message so a transport response cannot expose
private node details through the website.

`corepc-client` buffers the HTTP response before custom deserialization. During
replacement, the old structured snapshot and encoded response can also remain
alive while a reader finishes. The 200,000-entry cap is therefore an entry
bound, not a complete memory bound. The configurable auxiliary cache estimate,
256 MiB by default and 512 MiB maximum, covers exact current-transaction output
sets together with positive confirmed `OutPoint` scripts. Positive confirmed
facts are evicted when necessary; nulls and failures are not admitted. Retained
pending scripts have a separate 256 MiB ceiling. Unresolved fact indexes,
pending raw transactions, classification details, concurrent RPC responses,
the encoded snapshot, allocator overhead, and reader overlap remain outside
these budgets.
Deployment acceptance must still observe real peak memory. Classification HTTP
bodies are buffered before the decoded 16 MiB envelope guard runs, so the
deployment's 2 GiB memory cgroup is the hard transient response boundary.

Version 0.8 of `corepc-client` also fixes the JSON-RPC transport timeout at 15
seconds for the complete membership path. The first membership-only deployment
accepted complete 28,520 to 33,381 entry snapshots over the target WireGuard
path in 6.9 to 15.4 seconds end-to-end and peaked below 49 MB after a full
browser load. Those measurements predate the continuous classifier, auxiliary
caches, and progressive detail publications, and are not a 200,000-entry proof.
Materially larger mempools and the classified deployment require renewed
measurement.

## Network boundary

The service does not expose Bitcoin RPC publicly and does not introduce a new
node-side service, agent, container, queue, database, ZMQ subscriber, listener,
or transport.
Deployment supplies the existing WireGuard-only node RPC proxy URL and a
dedicated least-privilege RPC credential. Authentication alone is not an
authorization boundary: the node must whitelist that identity to exactly
`getmempoolinfo`, `getrawmempool`, `getblockchaininfo`, `getrawtransaction`,
and `gettxout`. The proxy must stream large responses without proxy-temp spill
and use timeouts compatible with the validated collection windows. Atlas
itself binds only to loopback behind the existing website proxy.

Hostnames, private addresses, credentials, and fleet inventory belong to the
deployment repository, not this application repository.

## Read API

`GET /api/v1/sources` returns source availability and small snapshot metadata.
`GET /api/v1/sources/{source_id}/mempool` returns that status plus the latest
complete snapshot, or `null` before the first successful poll.

`GET /api/v1/sources/{source_id}/transactions/{txid}` reads the detail map bound
to the current snapshot. It returns:

- `200` with current `txid`, `wtxid`, `classification_revision`, compact
  assessment, seven rule outcomes, exact counts, and bounded exemplars when
  classified;
- `400` for a malformed transaction ID;
- `404` for an unknown source or a transaction absent from current membership;
- `503` while no snapshot exists or when the present transaction has no
  classification yet.

All API responses use `Cache-Control: no-store`.

## Browser boundary

`web/` is a dependency-light Vite application. It discovers the configured
source, fetches one complete snapshot, validates it, and renders:

- a health and freshness summary;
- a primary Canvas classification terrain with compatible, indeterminate,
  unclassified, complete violating, and incomplete violating sections;
- canonical exact buckets for complete violating assessments, plus separate
  partial buckets keyed by both proven and unresolved rule sets;
- count and virtual-size transaction-tile modes with readability weighting for
  section and bucket frames;
- overlapping marginal rule filters, status- and combination-bucket selection,
  representative transactions, and on-demand typed detail;
- a fee-rate-by-age Canvas as a secondary lens with client-side filters.

The browser derives each violating bucket from the compact assessment already
present in the snapshot. It converts the seven canonical rule IDs into a
violated-rule mask and an unknown-rule mask. A zero unknown-rule mask produces
an exact bucket keyed only by the violated set. Any unknown rule produces a
separate partial bucket keyed by both sets, even when that same rule also has a
proven violation. Only combinations observed in the current snapshot allocate
regions.

Every transaction appears once in the terrain. Selecting a rule is a marginal
filter across exact and partial buckets, so rule totals overlap and are not
additive. The deterministic first rejection remains visible in transaction
detail but does not determine terrain placement. These are browser-derived
views of the existing compact assessment; the snapshot schema, API, collector,
and runtime publication model are unchanged.

Selection-only redraws reuse the current snapshot's terrain geometry and map
region metadata by key before painting transaction glyphs. Snapshot, size-mode,
or viewport changes invalidate that geometry. Adaptive label and inset space
keeps a positive glyph area even for rare observed buckets.

The UI performs no automatic full-snapshot polling. Manual refresh fetches the
current server copy and does not initiate node collection.

Both snapshot and detail parsing require a non-negative
`classification_revision`. While loading detail, the browser also holds the
snapshot's `observed_at_ms`, source, `txid`, and `wtxid`. It ignores a response
if its local snapshot changed in flight. It rejects older detail revisions and
accepts a later server revision only when the compact assessment still exactly
matches the visible transaction. Unrelated classification progress can
therefore coexist with an open inspector without attaching changed evidence to
a stale terrain tile.

The service defaults to a five-minute membership interval. A separate
classification loop means policy work does not lengthen a membership poll that
fits within that interval. The interval uses delayed missed ticks, so an
overrunning membership request delays the next start instead of creating
catch-up polls. Production cadence is an operational capacity choice based on
measured response bytes, transfer time, node work, and required freshness, not
a live-data promise.

## Products kept separate

The viewer stores no history. Its rule detail describes only the current
snapshot and is discarded when membership changes. Forensic archives,
peer-observer events, rejection observations, and other retained evidence have
different lifecycle and capacity needs and must remain a separate system.

Multi-node comparison is also a separate product surface. It may reuse the
snapshot model and collection code, but each source remains independent and
comparison is derived from complete snapshots. Absence from a source is not
evidence of rejection, filtering, or relay causality.
