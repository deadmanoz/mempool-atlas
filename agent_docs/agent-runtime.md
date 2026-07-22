# Atlas Agent Runtime

`atlas-agent` is the source-local state durability boundary. One process and one persistent SQLite database represent exactly one configured Bitcoin node. The current production runtime observes RPC state only; peer evidence capture is not mounted.

## Runtime loops

`state_runtime::run` starts two independent loops:

- The RPC loop connects with `corepc-client`, preflights `getmempoolinfo.size` against `ATLAS_MAX_MEMPOOL_ENTRIES`, obtains an initial verbose `getrawmempool` snapshot, then polls at the configured cadence. A failed poll retains the last complete snapshot.
- The delivery loop asks `SourceReplica::next_action` for one frozen delta, checkpoint, or heartbeat and sends it to `POST /api/v1/state`. Delivery continues while RPC is unavailable.

Both blocking RPC and SQLite work run outside Tokio worker threads. SQLite `BUSY` and `LOCKED` results are transient: delivery retries without changing the frozen action, and RPC persistence retains the last complete replica until the next full poll. This prevents a long snapshot replacement or checkpoint freeze in one loop from terminating the other. Shutdown signals stop both loops cleanly.

The runtime caps the `corepc` log target at debug even under a broader `RUST_LOG=trace`. `corepc-client` trace records contain complete raw RPC results, which would otherwise write one verbose mempool response per poll.

## Persistent identity and revision

`SourceReplica::open` binds a migrated database to one `source_id`. First use creates a random `epoch_id`; reopening the same database preserves it. A different configured source is rejected.

Revision zero means no complete RPC observation has been accepted. The first valid observation becomes revision 1 and requires a checkpoint, including when the mempool is empty. Later changed snapshots advance once per complete observation. Unchanged snapshots retain the revision and update freshness only.

The wire cursor is `(epoch_id, revision)`. A new database creates a new epoch and checkpoints over the server's old active cursor. A checkpoint in the same epoch must strictly advance the replaced revision. If the server reports that epoch at or ahead of local state, the agent treats that as local rollback, rotates its epoch, and republishes the current complete snapshot as revision 1.

## Local tables and bounds

| Table | Role | Bound |
| --- | --- | --- |
| `source_replica_state` | Singleton identity, cursors, freshness, and checkpoint flags | One row |
| `source_replica_membership` | Latest complete fact-bearing RPC snapshot | `ATLAS_MAX_MEMPOOL_ENTRIES` |
| `source_replica_dirty` | Net divergence per txid | `ATLAS_MAX_DIRTY_MUTATIONS` and `ATLAS_MAX_DIRTY_BYTES` |
| `source_replica_frozen_action` | Metadata for the exact retryable action | One row |
| `source_replica_frozen_delta` | Stable sorted delta mutations | Dirty mutation bound |
| `source_replica_frozen_checkpoint` | Stable complete checkpoint indexed by ordinal | Membership bound |

The diff is a merge stream over sorted stored membership and the validated RPC `BTreeMap`. Once the remaining dirty budget would be exceeded, the collector stops retaining detailed changes, clears any partial detail, counts the remainder only, and directly replaces membership from the already-held RPC map. This avoids an additional full diff allocation.

A frozen action is immutable across retries and restarts. New observations update current membership while retaining it. Dirty state after acknowledgement is rebased against the membership that the frozen action delivered.

## Delivery contract

Deltas bind base revision, target revision, observation time, sorted mutations, and a canonical SHA-256 digest. Checkpoints bind the replacement cursor, optional exact staging checkpoint to supersede, target cursor, observation time, total entries, total chunks, and a chunk-independent canonical digest. Each chunk has a separate digest that includes checkpoint ID and chunk index.

The agent validates every successful acknowledgement before changing local delivery state. Exact lost-ack retries are safe. Cursor, epoch, checkpoint, or replay conflicts trigger a replacement checkpoint against the server's reported active cursor and staging checkpoint ID. That staging ID is a compare-and-swap token: a delayed begin cannot delete a different checkpoint. Other operator-action failures and transient failures differ only in retry delay; neither discards state.

Checkpoint requests are resumable from server-reported progress. The agent sends only the remaining sequential chunks, then commits. It clears the frozen action only after the expected active cursor is acknowledged.

## Configuration

| Variable | Required | Default | Purpose |
| --- | --- | --- | --- |
| `ATLAS_SOURCE_ID` | yes | none | Stable logical source identity |
| `ATLAS_AGENT_DATABASE` | no | `var/atlas-agent.db` | Persistent source-local SQLite database |
| `ATLAS_SERVER_URL` | yes | none | Central Atlas base URL |
| `ATLAS_RPC_URL` | no | `http://127.0.0.1:8332` | Bitcoin RPC endpoint |
| `ATLAS_RPC_USERNAME` | yes | none | RPC username |
| `ATLAS_RPC_PASSWORD` | yes | none | RPC password |
| `ATLAS_RPC_POLL_SECONDS` | no | `5` | Full observation cadence |
| `ATLAS_DELIVERY_RETRY_MILLISECONDS` | no | `1000` | Initial retry band |
| `ATLAS_DELIVERY_RETRY_MAX_MILLISECONDS` | no | `60000` | Maximum retry band |
| `ATLAS_MAX_MEMPOOL_ENTRIES` | no | `1000000` | Maximum complete membership |
| `ATLAS_MAX_DIRTY_MUTATIONS` | no | `4096` | Maximum coalesced delta rows |
| `ATLAS_MAX_DIRTY_BYTES` | no | `1048576` | Estimated coalesced delta bytes |
| `ATLAS_CHECKPOINT_CHUNK_ENTRIES` | no | `512` | Entries per checkpoint chunk |
| `ATLAS_AGENT_DB_MAX_BYTES` | no | `1073741824` | Main SQLite page cap, excluding WAL |

The agent sets SQLite temporary storage to memory, journal mode to WAL, retained journal size to 64 MiB, WAL auto-checkpoint to 1,000 pages, and `max_page_count` from `ATLAS_AGENT_DB_MAX_BYTES`. The page cap is a hard circuit breaker for the main database. It does not reserve filesystem space or bound transient WAL peak.

## Recovery and deployment

Use `just agent-db-migrate-deploy` for a fresh database, `just agent-db-backup` for an explicit backup, and `just agent-db-reinitialize-deploy` for a deliberate preproduction replacement. Schema generation 4 rejects stale databases. Every migration or reinitialization must go through the backup-first wrapper.

An ordinary restart needs no special action. A lost HTTP response retries the exact frozen payload. A central reset causes the agent to checkpoint against no active cursor. A deliberate agent reset creates a new epoch and replaces the server cursor on the next checkpoint.

The verbose RPC response decodes directly into the one full `BTreeMap` used for an observation, rather than first building a second raw map. The agent checks the node-reported count before the verbose call and the decoded count afterwards. `corepc-client` still buffers the HTTP response body, so this is an entry-count guard rather than a complete response-byte bound. Checkpoint freezing can temporarily retain a second membership copy in SQLite. Deployment capacity testing must include the snapshot map, both SQLite copies, indexes, the RPC body buffer, and transient WAL headroom before enabling a canary.
