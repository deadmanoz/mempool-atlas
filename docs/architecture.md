# Architecture

The self-contained [system architecture visualisation](mempool-atlas-system.html) separates the implemented SourceReplica state path from the bounded evidence products that remain deferred. [ADR 0002](adr/0002-separate-source-state-from-bounded-evidence.md) records why the original shared event FIFO was abandoned.

Mempool Atlas has two Rust process boundaries and one dependency-free browser client. The current production surface is RPC-only and state-only.

## SourceReplica at the node

Each `atlas-agent` instance is bound to one stable `source_id` and one persistent SQLite database. The database creates an `epoch_id` on first use and retains it across ordinary restarts. Losing or deliberately reinitializing the database creates a new epoch, which lets the server distinguish a replacement state history from delayed traffic belonging to the old one.

The agent preflights `getmempoolinfo.size` against the configured membership limit before polling verbose `getrawmempool`. It decodes the verbose response directly into one validated transaction-ID-keyed snapshot, then checks the limit again because the two RPC calls are not atomic. It uses only `vsize`, `time`, and `fees.base`, the subset required from both Core and Knots. BTC amounts are converted exactly to integer satoshis. A missing, malformed, overflowing, non-exact, or oversized observation is rejected before changing local state.

The first successful observation establishes revision 1, even when the snapshot is empty. Each later changed snapshot advances the local revision exactly once. An unchanged snapshot updates `state_observed_at_ms` and `last_rpc_success_at_ms` without advancing the membership revision.

Local persistence has four state structures:

- `source_replica_membership` is the latest complete fact-bearing snapshot.
- `source_replica_dirty` coalesces net divergence by txid against the next deliverable base.
- `source_replica_frozen_action` names at most one stable delta or checkpoint.
- A frozen delta contains at most 4,096 mutations; a frozen checkpoint contains at most one copy of the configured membership bound.

The diff collector merge-streams the stored snapshot and the RPC map. It counts all changes but retains detailed rows only within the remaining dirty-row and estimated-byte budget. Crossing either budget discards the partial detail, installs the validated snapshot directly, clears dirty rows, and requires a checkpoint. It never constructs a second unbounded diff vector.

New observations do not rewrite a frozen action. Changes after a frozen delta or checkpoint are represented relative to what that action will make visible. After acknowledgement, surviving dirty rows are rebased against the delivered state. If newer local revisions return to exactly the delivered membership, the agent publishes a checkpoint rather than inventing an empty delta.

## State delivery

The agent and server share the versioned `SourceReplicaRequest` contract on `POST /api/v1/state`.

Healthy delivery uses a sorted `StateDelta` from one acknowledged revision to a newer local revision. An unchanged revision uses a `StateHeartbeat` to update freshness. Bootstrap, cursor mismatch, epoch replacement, or dirty pressure uses a checkpoint with three phases:

1. `CheckpointBegin` declares the replacement cursor, optional exact staging checkpoint to supersede, target epoch and revision, observation time, entry and chunk counts, and whole-snapshot SHA-256 digest.
2. `CheckpointChunk` carries at most 512 sorted entries. Its digest binds the checkpoint ID, chunk index, and entries.
3. `CheckpointCommit` repeats the checkpoint ID, target revision, and whole digest.

The encoded request body is capped at 4 MiB. The agent accepts only HTTP 202 responses that name the expected cursor and checkpoint progress. A lost response safely retries the frozen payload. Machine-readable cursor, epoch, or checkpoint conflicts cause a new checkpoint against the server-reported active cursor and exact staging checkpoint ID. A same-epoch checkpoint must strictly advance the replaced revision; a server cursor at or ahead of local state rotates the local epoch before rebaseline. Transport and server failures retain the frozen action and retry with bounded deterministic backoff.

RPC observation and HTTP delivery are independent runtime loops. A central outage does not stop the agent from replacing its current snapshot and coalescing local divergence, and a Bitcoin RPC outage does not stop delivery of already frozen state.

## Atomic central state

`atlas-server` keeps at most one `active` and one `staging` generation per source. SQLite partial unique indexes enforce that cardinality. Deltas and heartbeats can mutate only the active generation and are rejected while a checkpoint is staging. Checkpoint chunks can mutate only the staging generation. A different checkpoint begin may replace staging only when its `supersedes_checkpoint_id` equals the checkpoint currently there, so delayed begins cannot preempt newer work.

Checkpoint commit verifies:

- the replacement cursor still equals the active cursor;
- received entry and chunk counts equal the declaration;
- chunk indices are contiguous from zero;
- stored txids are unique;
- the canonical whole-snapshot digest matches.

The old active generation is deleted and staging is promoted in one transaction. A reader therefore sees either the previous complete snapshot or the replacement complete snapshot, never a mixture. Exact retries return `duplicate`; reusing a revision, checkpoint ID, or chunk index with different content is a conflict.

Product reads query only `active_source_replica` and `active_source_replica_membership`. A staging-only source and a source seen only by the experimental evidence reducer are not discoverable. A committed empty checkpoint is a known empty source.

## Product reads and browser

One selected source remains the primary read model. `GET /api/v1/sources/{source_id}/mempool` returns its complete active membership and exact state cursor. The aggregate summary endpoint computes fixed-catalog bins, filter facets, optional fee-rate ECDF detail, and joint fee-size detail from that same generation. Source health reports capture as `not_collected` in the state-only deployment.

The browser polls aggregate summaries every five seconds. Its DOM remains bounded by the bin catalog. Opening the on-demand inspector fetches the full current membership for the Canvas swim view and table. Individual base fee rate is a presentation value, not ancestor-package mining priority.

The optional comparison endpoint derives shared and adjacent set-difference regions from two to four active source generations at read time. Only aggregates leave the server, and no combined mempool is stored. Region fee and age facts come from the designated source. A forward or reverse difference is a membership observation, not evidence of policy, rejection, or relay causality.

## Evidence boundary

Peer-observer decoding, the normalized evidence model, intrinsic transaction derivation, and classifier packs remain in the repository as experimental building blocks. The production agent CLI does not expose NATS capture, and the production server router does not mount legacy event ingest. The old event ledger cannot establish or mutate product membership.

Recent rejection, peer, capture-gap, and raw-transaction data will use a separately bounded HotStore. Long-lived forensic data will use a quota-bound spool and verified off-VPS EvidenceArchive. Neither product may share SourceReplica's delivery ordering or disk budget. Until those products exist, Atlas says `not_collected` rather than implying evidence completeness.

## Capacity and operations

Semantic bounds are enforced at both ends: one million membership entries, 4,096 dirty mutations, 4,096 checkpoint chunks, 512 entries per chunk, and a 4 MiB request body. The agent also defaults to a 1 GiB main SQLite file cap via `PRAGMA max_page_count`, a 64 MiB retained WAL journal limit, and automatic WAL checkpoints every 1,000 pages.

The main database cap does not bound transient WAL peak. A frozen checkpoint can coexist with the current snapshot, so physical sizing must include roughly two bounded memberships, indexes, page overhead, and WAL headroom. `corepc-client` buffers a verbose RPC response before decoding it; the small `getmempoolinfo` preflight rejects an already oversized node-reported mempool, but it is not an HTTP response-byte cap. The planned 2 GiB per-source envelope and filesystem reserve of the greater of 20 percent or 5 GiB are not yet runtime-enforced acceptance criteria.

Central and agent schema changes use `scripts/migrate-safe.sh`, which backs up and validates an existing database before any change. Clean preproduction databases are central generation 7 and agent generation 4. Stale generations are intentionally rejected. Reinitialize only through the backup-first `just` targets.
