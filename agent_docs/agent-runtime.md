# Atlas runtime

The `mempool-atlas` binary runs one periodic collector, one in-memory source
runtime, and one loopback HTTP listener on the presentation host. The current
snapshot, classification cache, and transaction-detail map are memory only. The
process stores no application data on disk.

## Startup

`main.rs` parses one source configuration, rejects a non-loopback bind, reads a
one-line RPC password file, constructs the membership and classification RPC
clients plus `SourceRuntime`, then starts the poll task and Axum server. Both RPC
clients use the same configured URL and dedicated credential.

The process can serve `/healthz` and static files before a snapshot exists.
`/readyz` returns unavailable until the first complete poll succeeds.

## Complete membership sequence

Each poll runs the blocking RPC, decoding, and classification work in one
`spawn_blocking` task:

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

## Bounded classification sequence

After membership and its chain tip are complete, `PolicyEnricher` adds current
Knots RDTS/BIP-110 mempool-policy compatibility:

1. Build a current `txid` to `wtxid` membership index.
2. Prune the source-bound cache to variants whose `wtxid` is still present with
   the same `txid`.
3. Attach every cached compact assessment.
4. Starting after the prior candidate cursor and wrapping through sorted
   membership, select up to `ATLAS_MAX_CLASSIFICATIONS_PER_POLL` uncached or
   retryable variants. Advance the cursor to the final selection so persistent
   partial results cannot starve later members.
5. Fetch raw transactions with `getrawtransaction(txid, 0)` in batches of 16.
   Reject a result unless its decoded `txid` and `wtxid` exactly match the
   membership entry.
6. Fetch unconfirmed parent transactions in batches of 16. Resolve confirmed
   prevout scripts with `gettxout(txid, vout, false)` in batches of 128, so the
   mempool overlay does not hide an otherwise unspent confirmed output.
7. Evaluate all seven rules and reduce the result into the compact snapshot
   assessment plus typed current transaction detail.

The default cap is 10,000 candidate witness variants per poll. The default
45-second classification duration is a soft budget checked before starting
more work. A request already in flight can complete after it. Every
classification batch uses a 20-second transport timeout.

A missing or invalid raw transaction response leaves that member
unclassified. It creates no cache entry and can be attempted on a later poll.
A missing prevout is passed to the evaluator as an explicit gap. The resulting
typed unknowns remain visible, while independently decidable output rules and
proven input rules are preserved. The cache marks that result retryable so it
can become complete on a later poll.

Classification is best effort. Batch and per-response failures increment the
enrichment report and are logged, but they do not invalidate complete fresh
membership. The cache contains no departed variants and no history.

The production evaluator uses Knots mempool-policy mode, which applies all
seven rules without activation gating or UTXO grandfathering. Its verdicts do
not prove that the source rejected a transaction and do not claim consensus
invalidity. Missing prevouts and unsupported spend forms become typed unknowns
rather than guesses.

Each classified transaction retains seven rule outcomes. For every rule, the
detail includes the exact number of proven violations and missing facts, with
at most one evidence exemplar and one missing-fact exemplar. A rule can contain
both proven evidence and unresolved inputs.

## Publication and failure

After enrichment, Atlas builds and validates the complete
`MempoolObservation`: one sorted snapshot and a detail map whose transaction and
witness identities match that snapshot. The collector does this without
holding the runtime lock. `record_success` serializes the current-snapshot API
response, then replaces the reader-visible `Arc<MempoolSnapshot>`, detail map,
and shared encoded response under one short write lock. It also clears any
prior membership error.

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

The loop sleeps for `ATLAS_POLL_SECONDS` after each attempt. Membership and
classification duration therefore lengthen the start-to-start cadence.
Shutdown aborts the poll task after the HTTP server finishes graceful shutdown.

## Read API

`GET /api/v1/sources` returns source metadata without the transaction vector.
`GET /api/v1/sources/{source_id}/mempool` returns the same metadata plus the
latest snapshot, or `null` while waiting. Both use `Cache-Control: no-store`.

`GET /api/v1/sources/{source_id}/transactions/{txid}` looks only in the detail
map paired with the latest snapshot. It returns:

- `200` with source ID, snapshot observation time, current `txid` and `wtxid`,
  compact assessment, and seven typed rule details;
- `400` when the path contains an invalid transaction ID;
- `404` for an unknown source or transaction absent from current membership;
- `503` before the first snapshot or when a present transaction is
  unclassified.

The detail response also uses `Cache-Control: no-store`. A successful response
always identifies the exact snapshot and witness variant, so the browser can
discard detail that raced a refresh.

After each success or failure transition, Atlas serializes the complete
snapshot response once on a blocking worker. It atomically stores the resulting
immutable `Bytes` with the structured snapshot. Requests clone that shared
buffer, so concurrent readers do not allocate and serialize independent
full-snapshot bodies. A replacement can briefly retain the old structured and
encoded representations while an existing response finishes.

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
window. The current classification cache and detail map add bounded-per-entry
state, but their total size still scales with current membership.

The process always caps the `corepc` log target at debug even when a broader
`RUST_LOG` enables trace, because the dependency's trace record contains the
complete verbose mempool response.

## Configuration

| Variable | Required | Default |
| --- | --- | --- |
| `ATLAS_SOURCE_ID` | yes | none |
| `ATLAS_SOURCE_LABEL` | no | source ID |
| `ATLAS_RPC_URL` | yes | none |
| `ATLAS_RPC_USERNAME` | no | `atlas` |
| `ATLAS_RPC_PASSWORD_FILE` | yes | none |
| `ATLAS_POLL_SECONDS` | no | `300` |
| `ATLAS_MAX_MEMPOOL_ENTRIES` | no | `200000` |
| `ATLAS_MAX_CLASSIFICATIONS_PER_POLL` | no | `10000` |
| `ATLAS_CLASSIFICATION_BUDGET_SECONDS` | no | `45` |
| `ATLAS_BIND` | no | `127.0.0.1:3101` |
| `ATLAS_WEB_ROOT` | no | `web/dist` |

The RPC URL points at the existing private WireGuard-only node proxy. The
binary has no WireGuard-specific code and no public RPC fallback.
Deployment must whitelist the Atlas RPC identity to exactly
`getmempoolinfo`, `getrawmempool`, `getblockchaininfo`, `getrawtransaction`,
and `gettxout`. The proxy path must stream the response without writing it
through proxy-temp storage.

No Atlas process, database, queue, history, container, or new listener runs on
the Bitcoin node. Atlas adds no transport path beside the existing private
proxy.

The first deployed source validated the membership path with complete 28,520
to 33,381 entry snapshots. End-to-end membership-only polls took 6.9 to 15.4
seconds, service memory peaked below 49 MB after a full browser load, and the
node proxy created no temporary files. Those measurements predate the current
classification cache and detail map. Repeat them for the classified deployment,
materially larger mempools, or additional sources.

## Restart and recovery

An ordinary restart loses the in-memory snapshot, classification cache, and
detail map, then begins collecting again. No migrations, backups, checkpoints,
replays, or recovery commands exist. Historical data and forensic archives are
outside this process.
