# Atlas runtime

The `mempool-atlas` binary runs one fixed-interval membership loop, one
current-generation classification loop, one in-memory source runtime, and one
loopback HTTP listener on the presentation host. The current snapshot,
classification state, auxiliary script cache, and transaction-detail map are
memory only. The process stores no application data on disk.

## Startup

`main.rs` parses one source configuration, rejects a non-loopback bind, reads a
one-line RPC password file, constructs separate membership and classification
RPC clients plus `SourceRuntime`, then starts the runtime task and Axum server.
Both RPC clients use the same configured URL and dedicated credential.

The process can serve `/healthz` and static files before a snapshot exists.
`/readyz` returns unavailable until the first complete poll succeeds.

## Complete membership sequence

Each poll runs the blocking membership RPC and decoding in one `spawn_blocking`
task:

1. Call `getmempoolinfo`.
2. Reject a reported size above `ATLAS_MAX_MEMPOOL_ENTRIES`.
3. Call `getrawmempool true`.
4. Decode `wtxid`, `vsize`, `time`, and `fees.base` for every returned `txid`.
5. Enforce the entry limit again while decoding.
6. Validate and canonicalize every `txid` and `wtxid`, reject duplicates, and
   convert BTC fees exactly to integer satoshis.
7. Sort transactions by txid.
8. Call `getblockchaininfo` and validate the best block hash.
9. Record the completed membership observation time.

`getmempoolinfo` and `getrawmempool` are not atomic. Both size checks are
required. Any failure in this sequence rejects the poll.

The complete membership path uses synchronous `corepc-client` 0.8. Its
transport buffers the JSON-RPC response and has a fixed 15-second timeout.
Policy work does not run in this task and cannot delay publication of a
successfully validated membership.

## Membership generation installation

Every successful membership snapshot becomes a new in-memory policy generation:

1. Increment the process-local generation number and index current membership
   by canonical `txid`.
2. Copy only classifications whose exact `txid` and `wtxid` still match.
3. Copy cached current-transaction outputs only when that exact witness variant
   survives and the new generation's auxiliary-cache admission permits them.
   Confirmed prevout scripts do not survive a membership generation boundary.
4. Materialize and validate the complete membership with those exact surviving
   classifications.
5. Encode and publish the membership as revision 0, then wake the separate
   classification loop.

Fresh membership is therefore reader-visible before any new
`getrawtransaction` or `gettxout` work finishes. Exact cache reuse can make
some entries classified in the initial revision, but no stale or merely
same-`txid` witness result is attached.

## Continuous bounded classification

One wake-up drains bounded slices for the installed generation until there is
no eligible work, an RPC slice fails, a systemic no-progress result pauses the
generation, or a newer generation supersedes it:

1. Select up to `ATLAS_CLASSIFICATION_SLICE_ENTRIES` candidates, 2,048 by
   default. Configuration accepts one through the hard 8,192-entry slice
   maximum. Candidate selection also stops at a 256 MiB aggregate raw-response
   estimate.
2. Select currently unclassified variants first, then carried classifications
   with missing prevouts that are retryable. Mark every selection attempted so
   a witness variant is tried at most once in one membership generation.
3. Fetch and verify raw transactions with `getrawtransaction(txid, 0)`. Raw
   candidate batches have at most 256 requests and are split using reported
   `vsize` to target no more than 16 MiB per response.
4. Reuse exact current-parent output scripts where available. Fetch remaining
   unconfirmed parents through the same raw batching path. This second raw
   phase independently caps at 8,192 parents and a 256 MiB aggregate response
   estimate.
5. Consider at most 65,536 unique required prevouts from candidate
   transactions. Resolve confirmed prevout scripts through
   `gettxout(txid, vout, false)`. The nominal 512-request cap is reduced by the
   16 MiB response estimate and 64 KiB per-script-hex bound, making the current
   effective batch maximum 254. Passing `false` avoids the mempool overlay
   hiding an otherwise unspent confirmed output.
   Confirmed-prevout planning has a separate 256 MiB aggregate estimate, which
   permits 4,064 worst-case calls with the current per-response estimate.
6. Evaluate all seven rules and reduce the result into the compact snapshot
   assessment plus typed current transaction detail.
7. Merge results only if the generation is still current. Publish each slice
   that produced classifications as a strictly increasing generation revision,
   then yield and continue with the next bounded slice.

`ATLAS_CLASSIFICATION_RPC_LANES` controls raw transaction and mempool-parent
batch concurrency. It defaults to four and accepts one through eight. Confirmed
prevout work uses half that concurrency rounded up, so the default is two.
Every classification batch has a 20-second transport timeout.

Each returned raw transaction is rejected unless its decoded `txid` and
`wtxid` exactly match current membership. Transaction hex may contain at most
8,000,000 characters, and confirmed script hex may contain at most 64 KiB. A
raw or confirmed-prevout batch is rejected if its decoded JSON-RPC response
envelope exceeds 16 MiB. The minreq transport has already buffered and parsed
the response before this application check, so the guard limits admission into
current state rather than complete transient memory. Candidate raw,
mempool-parent raw, and confirmed-prevout aggregate estimates bound planned
work, not transport allocation. The deployed 2 GiB memory cgroup is the hard
transient boundary.

A missing or invalid raw transaction response leaves that member
unclassified. It creates no classification and is eligible again after the next
successful membership installation. A missing prevout is passed to the
evaluator as an explicit gap. The resulting typed unknowns remain visible,
while independently decidable output rules and proven input rules are
preserved. The cache marks that result retryable for the next membership
generation. Fresh unclassified work is selected before these retryable partial
results.

Classification is best effort. Batch and per-response failures increment the
enrichment report and are logged, but they do not invalidate complete fresh
membership. Departed witness variants are pruned at the next installation and
the process retains no history. When a slice adds no classifications and
reports any batch failure, or response failures at least equal its attempted
candidate count, the runtime pauses the generation until the next membership
installation. This prevents a systemic RPC failure from rapidly draining every
remaining slice. A direct slice error also stops that drain.

The production evaluator uses Knots mempool-policy mode, which applies all
seven rules without activation gating or UTXO grandfathering. Its verdicts do
not prove that the source rejected a transaction and do not claim consensus
invalidity. Missing prevouts and unsupported spend forms become typed unknowns
rather than guesses.

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

Policy ordering has two guards. `PolicyEnricher` merges a completed RPC slice
only when its generation object is still the coordinator's current generation.
`SourceRuntime` separately accepts policy publication only when its generation
matches the published membership and its revision strictly advances the
runtime's current revision. An old in-flight slice is therefore discarded even
if membership changes between policy installation and runtime publication. A
stale classification loop stops draining and waits for the new membership
wake-up. Raw and confirmed-prevout schedulers check generation identity before
each replacement wave, so superseded work can finish already in-flight requests
but cannot schedule further batches or downstream phases.

Every new membership publishes `classification_revision` 0, including any exact
classifications reused from the prior membership. Each slice that adds
classifications increments this membership-local revision. Runtime rejects a
publication when its snapshot revision and policy revision differ, then stores
the snapshot, detail map, policy revision, and encoded response together.

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

Membership uses a Tokio interval with `Delay` missed-tick behaviour. Polls
start at the configured `ATLAS_POLL_SECONDS` cadence while each membership poll
fits inside the interval. A membership overrun delays the next tick instead of
running catch-up polls. Classification runs in the other loop and does not
lengthen this start-to-start cadence. Shutdown aborts the combined runtime task
after the HTTP server finishes graceful shutdown.

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

The classification minreq transport likewise buffers and parses complete HTTP
response bodies before Atlas can measure the decoded JSON-RPC envelope against
its 16 MiB batch guard. Phase estimates limit scheduling, but the production
2 GiB memory cgroup is the hard transient response boundary.

`ATLAS_CLASSIFICATION_CACHE_MIB` is an auxiliary script-cache admission budget,
not a total process-memory cap. It defaults to 256 MiB and accepts at most
512 MiB. The implementation estimates each cached current-transaction output
set or confirmed prevout script as its script bytes plus fixed bookkeeping,
then declines entries that would exceed the budget. Classifications and the
reader detail map are bounded by current membership count rather than this byte
budget. Concurrent RPC responses, the membership buffer, serialized response,
allocator overhead, and briefly overlapping reader state remain outside it.

The process always caps the `corepc` log target at debug even when a broader
`RUST_LOG` enables trace, because the dependency's trace record contains the
complete verbose mempool response.

## Configuration

| Variable | Required | Default | Limit or purpose |
| --- | --- | --- | --- |
| `ATLAS_SOURCE_ID` | yes | none | Stable source identity |
| `ATLAS_SOURCE_LABEL` | no | source ID | Reader-visible label |
| `ATLAS_RPC_URL` | yes | none | Existing private RPC proxy |
| `ATLAS_RPC_USERNAME` | no | `atlas` | Dedicated RPC identity |
| `ATLAS_RPC_PASSWORD_FILE` | yes | none | One-line credential file |
| `ATLAS_POLL_SECONDS` | no | `300` | Nonzero membership interval |
| `ATLAS_MAX_MEMPOOL_ENTRIES` | no | `200000` | At most `200000` |
| `ATLAS_CLASSIFICATION_SLICE_ENTRIES` | no | `2048` | 1 through `8192` |
| `ATLAS_CLASSIFICATION_RPC_LANES` | no | `4` | 1 through `8` |
| `ATLAS_CLASSIFICATION_CACHE_MIB` | no | `256` | 1 through `512` MiB |
| `ATLAS_BIND` | no | `127.0.0.1:3101` | Loopback only |
| `ATLAS_WEB_ROOT` | no | `web/dist` | Built static website |

The RPC URL points at the existing private WireGuard-only node proxy. The
binary has no WireGuard-specific code and no public RPC fallback.
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

An ordinary restart loses the in-memory snapshot, generation state,
classification and auxiliary caches, and detail map, then begins collecting
again. No migrations, backups, checkpoints, replays, or recovery commands
exist. Historical data and forensic archives are outside this process.
