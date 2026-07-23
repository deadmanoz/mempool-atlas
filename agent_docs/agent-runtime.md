# Atlas runtime

The `mempool-atlas` binary runs one periodic collector, one in-memory source
runtime, and one HTTP listener on the presentation host. It stores no
application data on disk.

## Startup

`main.rs` parses one source configuration, rejects a non-loopback bind, reads a
one-line RPC password file, constructs the RPC client and `SourceRuntime`, then
starts the poll task and Axum server.

The process can serve `/healthz` and static files before a snapshot exists.
`/readyz` returns unavailable until the first complete poll succeeds.

## Collection sequence

Each poll runs all blocking RPC and decoding work in `spawn_blocking`:

1. Call `getmempoolinfo`.
2. Reject a reported size above `ATLAS_MAX_MEMPOOL_ENTRIES`.
3. Call `getrawmempool true`.
4. Decode only `vsize`, `time`, and `fees.base`.
5. Enforce the entry limit again while decoding.
6. Validate txids and convert BTC fees exactly to integer satoshis.
7. Sort transactions by txid.
8. Call `getblockchaininfo` and validate the best block hash.
9. Record the completed observation time.
10. Build and validate the complete `MempoolSnapshot`.

`getmempoolinfo` and `getrawmempool` are not atomic. Both size checks are
required.

## Publication and failure

The collector builds the next snapshot without holding the runtime lock.
`record_success` then replaces the reader-visible `Arc<MempoolSnapshot>` under
a short write lock and clears any prior error.

If any call or validation fails, `record_failure` keeps the current snapshot and
stores a stable public error. Detailed transport and validation errors remain in
service logs so private response content cannot leak through the website.
Source availability is:

| Snapshot | Last error | Availability |
| --- | --- | --- |
| absent | absent | `waiting` |
| absent | present | `error` |
| present | absent | `ready` |
| present | present | `stale` |

The loop sleeps for `ATLAS_POLL_SECONDS` after each attempt. A slow RPC call
therefore lengthens the start-to-start cadence. Shutdown aborts the poll task
after the HTTP server finishes graceful shutdown.

## Read API

`GET /api/v1/sources` returns source metadata without the transaction vector.
`GET /api/v1/sources/{source_id}/mempool` returns the same metadata plus the
latest snapshot, or `null` while waiting. Both use `Cache-Control: no-store`.

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
duplicate-free.

`corepc-client` buffers the JSON-RPC response before the custom deserializer
runs. The entry cap is not a byte or peak-memory guarantee. Deployment
acceptance must measure real memory using the target node and a large mempool.
The version 0.8 transport timeout is fixed at 15 seconds, so acceptance must also
verify large real responses finish reliably within that window.
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
| `ATLAS_BIND` | no | `127.0.0.1:3101` |
| `ATLAS_WEB_ROOT` | no | `web/dist` |

The RPC URL points at the existing private WireGuard-only node proxy. The
binary has no WireGuard-specific code and no public RPC fallback.
Deployment must whitelist the Atlas RPC identity to exactly
`getmempoolinfo`, `getrawmempool`, and `getblockchaininfo`. The proxy path must
stream the response without writing it through proxy-temp storage.

## Restart and recovery

An ordinary restart loses the in-memory snapshot and begins collecting again.
No migrations, backups, checkpoints, replays, or recovery commands exist.
Historical data and forensic archives are outside this process.
