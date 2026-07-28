# Atlas runtime

The `mempool-atlas` binary runs one fixed-interval membership-round loop, one
fair classification loop, one in-memory runtime per configured source, and one
loopback HTTP listener on the presentation host. One service-wide gate admits
all membership and classification RPC work. Current snapshots, classification
state, auxiliary script caches, and transaction-detail maps are memory only.
The process stores no application data on disk.

[ADR 0005](../docs/adr/0005-resolve-policy-facts-before-evaluation.md) is the
authoritative decision for the pending fact resolver, explicit-null semantics,
positive prevout cache, and P2SH evaluation. It supersedes ADR 0003's
attempt-once classification details. [ADR 0006](../docs/adr/0006-own-policy-json-rpc-wire-boundary.md)
defines the classification transport and HTTP framing contract.
[ADR 0007](../docs/adr/0007-browser-derived-snapshot-comparison.md) defines
bounded multi-source scheduling and browser-derived comparison.

## Startup

`main.rs` rejects a non-loopback bind and parses a root-controlled JSON file
containing one to four source records. The file is capped at 64 KiB, rejects
unknown fields and duplicate source IDs, and names credentials rather than
containing passwords. Atlas reads each one-line password from
`ATLAS_CREDENTIALS_DIRECTORY`, constructs separate membership and
classification RPC clients plus a `SourceRuntime` for every source, divides one
total auxiliary-cache budget evenly among them, then starts the coordinator and
Axum server. Each source's two RPC clients use the same configured URL,
username, and dedicated credential. `rpc.rs` owns the `corepc-client`
membership path. `policy_rpc.rs` owns the separate bounded classification wire
path.

The process can serve `/healthz` and static files before a snapshot exists.
`/readyz` returns unavailable until the first complete poll succeeds.

## Complete membership sequence

Each source turn runs the blocking membership RPC and decoding in one
`spawn_blocking` task:

1. Record the collection start and call `getblockchaininfo` for the starting
   height and best block hash.
2. Call `getmempoolinfo` and reject a reported size above
   `ATLAS_MAX_MEMPOOL_ENTRIES`.
3. Call `getrawmempool true`.
4. Decode `wtxid`, `vsize`, `time`, and `fees.base` for every returned `txid`.
5. Enforce the entry limit again while decoding.
6. Validate and canonicalize every `txid` and `wtxid`, reject duplicates, and
   convert BTC fees exactly to integer satoshis.
7. Sort transactions by txid.
8. Call `getblockchaininfo` again and require the ending height and best block
   hash to match the start.
9. Record collection completion and derive the exact duration. The public
   `observed_at_ms` is this completion time.

`getmempoolinfo` and `getrawmempool` are not atomic. Both size checks and the
stable-tip bracket are required. Any failure in this sequence rejects only that
source's poll.

The complete membership path uses synchronous `corepc-client` 0.8. Its
transport buffers the JSON-RPC response and has a fixed 15-second timeout.
Policy work does not run in this task and cannot delay publication of a
successfully validated membership.

## Multi-source coordination

`AtlasRuntime` owns the configured sources in file order, one Tokio semaphore,
one classification wake-up, and a pending-source set. A membership round
acquires the semaphore for the whole round and attempts every source
sequentially. A source failure records a stable public error and preserves that
source's last good snapshot; later sources are still attempted. After the round
releases the gate, only sources that published a new generation are inserted
into the classification workset.

The classifier repeatedly merges newly pending sources into its active set,
selects the next source from a rotating cursor, acquires the same semaphore for
one bounded resolver slice, and releases it before rotating. A source remains
active only while its drain result is `Continue`. This makes late arrivals and
wraparound fair, keeps membership and classification RPC non-overlapping, and
does not rearm a paused generation merely because another source published.

A due membership round may wait for the currently running bounded
classification slice. Because the semaphore queues waiters, subsequent slices
cannot overtake that round. The membership round then polls all configured
sources before classification resumes.

## Membership generation installation

Every successful membership snapshot becomes a new in-memory policy generation:

1. Increment the process-local generation number and index current membership
   by canonical `txid`.
2. Copy only classifications whose exact `txid` and `wtxid` still match.
3. Copy cached current-transaction outputs only when that exact witness variant
   survives and auxiliary-cache admission permits them. Retain validated
   positive confirmed `OutPoint` scripts across generation boundaries under the
   same configured auxiliary bound and eviction policy. Never cache null or
   failed lookups.
4. Materialize and validate the complete membership with those exact surviving
   classifications.
5. Encode and publish the membership as revision 0, then wake the separate
   classification loop.

Fresh membership is therefore reader-visible before any new
`getrawtransaction` or `gettxout` work finishes. Exact cache reuse can make
some entries classified in the initial revision, but no stale or merely
same-`txid` witness result is attached.

## Continuous bounded classification

A scheduled source turn advances bounded candidate and script-fact waves for
that source's installed generation. The generation owns verified pending raw
transactions and their unresolved facts, so crossing a wave boundary does not
restart work or refetch a successfully admitted candidate:

1. Admit up to `ATLAS_CLASSIFICATION_SLICE_ENTRIES` current witness variants,
   2,048 by default. Configuration accepts one through the hard 8,192-entry
   candidate-window maximum. Admission also stops at a 256 MiB aggregate
   candidate-raw response estimate.
2. Fetch candidates with `getrawtransaction(txid, 0)`. Raw candidate batches
   have at most 256 requests and are split using reported `vsize` to target no
   more than 16 MiB per response. Retain a returned transaction only after its
   decoded `txid` and `wtxid` exactly match current membership.
3. Resolve required input scripts from exact current-parent outputs or the
   bounded positive script cache. Fetch remaining unconfirmed parents through
   the same raw batching path. A parent fact wave independently caps at 8,192
   transactions and a 256 MiB aggregate response estimate.
4. Use 65,536 unique required prevouts as the target for one pending window,
   with a separate 256 MiB retained-script ceiling. Always admit one candidate
   when that transaction alone exceeds the count target; its raw and
   retained-script byte bounds still apply. Resolve remaining confirmed scripts
   through `gettxout(txid, vout, false)`. The nominal 512-request batch cap is
   reduced by the 16 MiB response estimate and 64 KiB per-script-hex bound,
   making the current effective maximum 254. Passing `false` avoids the mempool
   overlay hiding an otherwise unspent confirmed output. Each confirmed-prevout
   fact wave has a separate 256 MiB aggregate estimate, which permits 4,064
   worst-case calls with the current per-response estimate. Facts postponed by
   a wave bound remain pending, and later waves advance fairly instead of
   restarting at the same sorted outpoint prefix.
5. Evaluate all seven rules only after every required script is present or has
   a genuine terminal lookup result. Reduce completed results into the compact
   snapshot assessment and typed current transaction detail.
6. Merge results and positive confirmed script facts only if the generation is
   still current. Publish a strictly increasing generation revision when a wave
   adds assessments. A wave that only resolves facts continues without
   publishing a revision.
7. Give each unresolved fact two attempts for its current source within one
   bounded pending window. If a candidate exhausts those attempts, or cannot
   admit a known script under the pending-script ceiling, defer that candidate
   for the rest of the generation. Bound recovery by the pending candidate
   count, yield and check staleness between relief passes, and preserve positive
   facts already fetched in the current call while a dependent survives. Then
   continue with queued and later candidate windows. Pause the generation only
   for a systemic RPC failure.

`ATLAS_CLASSIFICATION_RPC_LANES` controls raw transaction and mempool-parent
batch concurrency. It defaults to four and accepts one through eight. Confirmed
prevout work uses half that concurrency rounded up, so the default is two.
Every classification batch has a 20-second transport timeout.

Each returned raw transaction is rejected unless its decoded `txid` and
`wtxid` exactly match current membership. Transaction hex may contain at most
8,000,000 characters, and confirmed script hex may contain at most 64 KiB. The
Atlas-owned policy client uses lazy `minreq` reads and caps each HTTP body at
16 MiB before JSON parsing. It rejects `Transfer-Encoding`, rejects an
oversized declared length immediately, and requires the received bytes to
exactly match any declaration. A close-delimited body aborts on the first byte
beyond the cap and must decode as one complete JSON batch. Candidate raw,
mempool-parent raw, and confirmed-prevout aggregate estimates bound planned
work rather than total process allocation. The deployed 2 GiB memory cgroup
remains the hard boundary for concurrent responses, the separately buffered
membership path, and all other state.

The classification client refuses redirects, requires HTTP 200 and the Bitcoin
Core JSON-RPC 2.0 envelope shape, allocates unique numeric IDs across concurrent
lanes, and restores out-of-order responses to request order. Duplicate or
unexpected IDs and excess responses reject the whole batch. Logs count
`response_bytes` as the exact JSON body bytes, including whitespace, for each
successfully decoded and reconciled batch.

A successful null-shaped `gettxout` response from the trusted Bitcoin Core
endpoint becomes a typed missing script fact only when the outpoint is known
not to be a current mempool parent. After a current-parent raw lookup fails, a
null fallback probe is ambiguous and remains operationally unresolved until its
bounded attempts are exhausted. Capacity deferral, unscheduled work, batch or
transport failure, malformed or oversized responses, and missing response
envelopes likewise remain pending, deferred, or failed operational state. They
leave the transaction's public `bip110` value `null`; they do not manufacture
an indeterminate assessment. Operationally unresolved facts receive bounded
local attempts; an exhausted candidate remains unclassified until the next
membership generation rather than blocking later candidates. A completed
assessment with a genuine terminal missing script can still preserve
independently decidable violations.

The owned envelope keeps a missing `result`, present JSON null, and present
value distinct. It also preserves `error` presence. A valid error takes
precedence over any simultaneous result. In an otherwise decodable v2 item,
explicit `error: null`, an omitted result, a malformed error object, and an
ordinary non-systemic RPC error are response-local collection failures. A
missing or wrong JSON-RPC version and configured protocol or warm-up error
codes are systemic. Invalid batch framing, HTTP failure, and body-bound failure
pause the generation through the batch-failure path. Only a valid null-shaped
`gettxout` success can become the terminal fact described above.

A missing or invalid raw transaction response likewise leaves that membership
entry unclassified. It creates no assessment and can be reconsidered after a
later successful membership installation. None of these best-effort policy
failures invalidates complete fresh membership. Departed witness variants are
pruned at the next installation and the process retains no history.

Each resolver call returns an explicit `PolicyDrain` disposition:

- `Continue` means eligible work remains and no systemic circuit breaker fired.
  The runtime yields and runs another bounded wave. Fact-only progress does not
  publish a new revision.
- `Complete` means no eligible work remains for the current generation. That
  source leaves the active workset until it publishes replacement membership.
- `Paused` means a systemic RPC failure tripped the generation circuit breaker.
  That source leaves the active workset until its own replacement membership
  instead of spinning on the same failure.
- `Stale` means a newer membership generation superseded the work. In-flight
  calls may finish, but their results cannot update current state or the shared
  positive script cache.

Progress logs separately report `fact_requests`, `facts_resolved`,
`facts_missing`, `capacity_deferred`, `deferred_candidates`,
`response_failures`, `systemic_response_failures`, `missing_responses`,
`batch_failures`, and `response_bytes`.

The production evaluator uses Knots mempool-policy mode, which applies all
seven rules without activation gating or UTXO grandfathering. Its verdicts do
not prove that the source rejected a transaction and do not claim consensus
invalidity.

P2SH evaluation matches that deployed policy. The final scriptSig item is the
redeemScript blob and is exempt from Rule 2, while preceding scriptSig items and
pushes within the redeemScript remain subject to it. Exact P2SH-P2WPKH and
P2SH-P2WSH programs dispatch through nested witness evaluation. P2SH-wrapped
witness versions 1 through 16 use Rule 3 because Taproot and P2A are native
only.

Each classified transaction retains seven rule outcomes. For every rule, the
detail includes the exact number of proven violations and missing facts, with
at most one evidence exemplar and one missing-fact exemplar. A rule can contain
both proven evidence and unresolved inputs.

## Publication, ordering, and failure

Membership and policy progress both materialize a complete
`MempoolObservation`: one sorted snapshot and a detail map whose transaction and
witness identities match that snapshot. They build and encode the replacement
without holding the reader-visible runtime lock, then replace the
`Arc<MempoolSnapshot>`, detail map, and shared encoded response under one short
write lock.

Policy ordering has two guards. `PolicyEnricher` merges completed resolver work
only when its generation object is still the coordinator's current generation.
`SourceRuntime` separately accepts policy publication only when its generation
matches the published membership and its revision strictly advances the
runtime's current revision. Old in-flight work is therefore discarded even if
membership changes between policy installation and runtime publication. Stale
source work leaves the active classification workset and waits for that
source's next successful membership generation.
Raw and confirmed-prevout schedulers check generation identity before each
replacement wave, so superseded work can finish already in-flight requests but
cannot schedule further batches or downstream phases. Stale work cannot admit
positive scripts into the cross-generation cache.

Every new membership publishes `classification_revision` 0, including any exact
classifications reused from the prior membership. Each wave that publishes new
assessments increments this membership-local revision; a fact-only wave does
not. Runtime rejects a publication when its snapshot revision and policy
revision differ, then stores the snapshot, detail map, policy revision, and
encoded response together.

If any complete-membership call or validation fails, `record_failure` keeps the
current snapshot and matching detail map, then stores a stable public error.
Detailed transport and validation errors remain in service logs so private
response content cannot leak through the website. Source availability is:

| Snapshot | Last error | Availability |
| --- | --- | --- |
| absent | absent | `waiting` |
| absent | present | `error` |
| present | absent | `ready` |
| present | present | `stale` |

Membership rounds use a Tokio interval with `Delay` missed-tick behaviour. A
round starts at the configured `ATLAS_POLL_SECONDS` cadence while the previous
round and any wait for the shared gate fit inside the interval. An overrun
delays the next tick instead of running catch-up rounds. Classification runs in
the other loop but releases the gate after every bounded source slice. Shutdown
aborts the combined runtime task after the HTTP server finishes graceful
shutdown.

## Read API

`GET /api/v1/sources` returns source metadata without the transaction vector.
`GET /api/v1/sources/{source_id}/mempool` returns the same metadata plus the
latest snapshot, or `null` while waiting. Both use `Cache-Control: no-store`.

`GET /api/v1/sources/{source_id}/transactions/{txid}` looks only in the detail
map paired with the latest snapshot. It returns:

- `200` with source ID, snapshot observation time, current `txid` and `wtxid`,
  `classification_revision`, compact assessment, and seven typed rule details;
- `400` when the path contains an invalid transaction ID;
- `404` for an unknown source or transaction absent from current membership;
- `503` before the first snapshot or when a present transaction is
  unclassified.

The snapshot and detail response both expose `classification_revision`. The
browser binds it to `observed_at_ms`, source identity, `txid`, and `wtxid`. It
ignores detail if its local snapshot changes while the request is in flight. It
rejects an older server detail revision, and accepts a later revision only when
the transaction's compact assessment exactly matches the visible snapshot.
This allows unrelated classification progress without attaching changed rule
evidence to a stale terrain tile.

The static site has a node entry at `/` and a comparison entry at `/compare/`.
Comparison has no server projection endpoint. It fetches two existing source
snapshot responses, validates them independently, and merge-joins their sorted
transaction vectors in the browser. It keeps common entries source-local,
aborts obsolete pair and detail requests after selection changes, and still
rejects late results. `web/src/comparison-policy-matrix.ts` derives a one-pass,
count-only policy matrix before the terrain. It summarizes left-only-left,
common-left, common-right, and right-only-right assessment populations. Its four
statuses conserve each row. Aggregate
violating includes exact and partly unresolved assessments, while its bounded
dominant signature controls are exact-only and deterministically ordered.
Matrix actions reuse the canonical comparison transition and therefore update
region, policy side, filter, sample, inspector, and URL together while clearing
the selected txid. Selection-only Canvas paints reuse cached geometry, while a
single virtual listbox option exposes every region transaction to bounded
keyboard navigation without a transaction-sized DOM. The browser stores no
history.

Both products serialize exploration state into canonical query parameters.
The node view records its source, one rule or terrain-region selection, and an
optional txid. The comparison records an atomic distinct source pair,
membership region, source-local policy side, policy filter, and optional txid.
Transaction search is current-snapshot lookup only. Comparison lookup binary-
searches the three sorted regions instead of retaining a duplicate union index.
Ordinary refresh preserves valid state, while a user source or pair change
clears the prior selection. A well-formed txid absent from the current snapshot
remains explicit rather than becoming a fabricated assessment.

After each membership, policy-progress, or failure transition, Atlas serializes
the complete snapshot response once on a blocking worker. It atomically stores
the resulting immutable `Bytes` with the structured snapshot. Requests clone
that shared buffer, so concurrent readers do not allocate and serialize
independent full-snapshot bodies. A replacement can briefly retain the old
structured and encoded representations while an existing response finishes.

## Bounds and validation

The hard application entry limit is 200,000. Configuration may lower it but may
not raise it. Required numeric fields must be exactly representable by a
JavaScript number. Transaction virtual size must be positive, source identity
must be URL-safe, and the transaction vector must be strictly sorted and
duplicate-free by canonical `txid`. Witness variants must also be unique.

`corepc-client` buffers the JSON-RPC response before the custom deserializer
runs. The entry cap is not a byte or peak-memory guarantee. Deployment
acceptance must measure real memory using the target node and a large mempool.
The version 0.8 membership transport timeout is fixed at 15 seconds, so
acceptance must also verify large real responses finish reliably within that
window.

The classification transport bounds each HTTP body at 16 MiB before parsing.
Wave estimates limit scheduling, while the production 2 GiB memory cgroup
remains the hard boundary for concurrent classification bodies, the membership
buffer, and total process memory.

`ATLAS_CLASSIFICATION_TOTAL_CACHE_MIB` is a service-wide auxiliary script-cache
admission budget, not a total process-memory cap. It defaults to 256 MiB,
accepts at most 512 MiB, and is divided evenly among configured sources. The
implementation estimates each cached current-transaction output set or
confirmed prevout script as its script bytes plus fixed bookkeeping, then
declines or evicts entries to keep that source within its share. Positive
confirmed scripts may survive generation replacement, while null and failed
lookups are never cached. Each source's retained pending scripts have a
separate ceiling equal to the smaller of its share and 256 MiB. Unresolved fact
indexes, pending raw transactions, classifications, reader detail maps,
concurrent RPC responses, membership buffers, serialized responses, allocator
overhead, and briefly overlapping reader state remain outside these budgets.

The process always caps the `corepc` log target at debug even when a broader
`RUST_LOG` enables trace, because the dependency's trace record contains the
complete verbose mempool response.

## Configuration

| Variable | Required | Default | Limit or purpose |
| --- | --- | --- | --- |
| `ATLAS_SOURCES_FILE` | yes | none | At most 64 KiB of JSON containing one to four source records |
| `ATLAS_CREDENTIALS_DIRECTORY` | yes | none | Directory containing source-named one-line password files |
| `ATLAS_POLL_SECONDS` | no | `300` | Nonzero membership-round interval |
| `ATLAS_MAX_MEMPOOL_ENTRIES` | no | `200000` | At most `200000` |
| `ATLAS_CLASSIFICATION_SLICE_ENTRIES` | no | `2048` | 1 through `8192` pending candidates |
| `ATLAS_CLASSIFICATION_RPC_LANES` | no | `4` | 1 through `8` |
| `ATLAS_CLASSIFICATION_TOTAL_CACHE_MIB` | no | `256` | 1 through `512` MiB divided among sources |
| `ATLAS_BIND` | no | `127.0.0.1:3101` | Loopback only |
| `ATLAS_WEB_ROOT` | no | `web/dist` | Built static website |

Each source record contains `source_id`, `source_label`, `rpc_url`,
`rpc_username`, and `rpc_password_credential`. The credential field must be a
safe file name, not a path. Each RPC URL points at that source's existing
private WireGuard-only node proxy. The binary has no WireGuard-specific code
and no public RPC fallback.
Deployment must whitelist the Atlas RPC identity to exactly
`getmempoolinfo`, `getrawmempool`, `getblockchaininfo`, `getrawtransaction`,
and `gettxout`. The proxy path must stream the response without writing it
through proxy-temp storage.

No Atlas process, database, queue, history, ZMQ subscriber, container, or new
listener runs on the Bitcoin node. Atlas adds no transport path beside the
existing private proxy.

The first deployed source validated the membership path with complete 28,520
to 33,381 entry snapshots. End-to-end membership-only polls took 6.9 to 15.4
seconds, service memory peaked below 49 MB after a full browser load, and the
node proxy created no temporary files. Those measurements predate the current
continuous classifier, auxiliary caches, and progressive detail publications.
Repeat them for the classified deployment, materially larger mempools, or
additional sources.

## Restart and recovery

An ordinary restart loses every in-memory snapshot, generation state,
classification and auxiliary cache, and detail map, then begins collecting
again in configured source order. No migrations, backups, checkpoints, replays,
or recovery commands exist. Historical data and forensic archives are outside
this process.
