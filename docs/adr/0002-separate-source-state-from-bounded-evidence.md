# Separate source state from bounded evidence

Status: accepted; SourceReplica slice implemented

Mempool Atlas separates RPC-authoritative source state, recent product evidence, and historical forensic evidence into independently bounded data products. State convergence must not share delivery ordering or a disk budget with forensic evidence.

The live canaries demonstrated that the original event-ledger design amplified a central delivery failure into unbounded source storage. Core accumulated a 7.13 GB agent database, while Knots accumulated 26.87 GB and exhausted its root filesystem. Replaying or pruning that same mutation stream would retain the coupling that caused the failure.

## Decision

Current membership is owned by a deep `SourceReplica` module. A stable `source_id` names the logical node and a persisted `epoch_id` names one continuous state history. Routine process restarts retain the epoch. Agent database loss, an incompatible schema reset, or an intentionally abandoned history creates a new epoch.

RPC is the sole authority for SourceReplica membership. A changed RPC snapshot advances one source revision. Healthy synchronization sends sorted, coalesced deltas from an acknowledged base revision to a newer target revision. Repeated changes to one transaction occupy one dirty record rather than an append-only mutation history.

Bootstrap, cursor mismatch, epoch replacement, or dirty-set pressure uses an atomic full checkpoint. The server stages one inactive membership generation, verifies its declared entry count and canonical digest, then activates the complete epoch and generation in one transaction. Readers see either the prior complete generation or the replacement complete generation. A different begin can supersede staging only by naming its exact checkpoint ID, and a same-epoch checkpoint must strictly advance the replaced revision. Delayed traffic cannot preempt newer staging or mutate active state.

At most two membership generations exist per source: one active and one staging. The production server admits two persistent source identities and does not provide source retirement. A central outage therefore costs storage proportional to current membership, not outage duration or observation rate. A poison forensic observation cannot block state synchronization.

Recent product evidence is stored in a bounded `HotStore`, not a generic permanent event table. Every variable-sized structure has semantic and physical bounds, including age, count, and bytes where applicable. The initial policy is:

- Rejections: seven days or 10,000 records per source, whichever is reached first.
- Other recent evidence metadata: six hours or 250,000 records per source.
- Capture gaps: 256 recent intervals plus compact cumulative counters.
- Idempotency receipts: an accepted-through watermark plus 4,096 recent sequence and digest pairs per source stream.
- Shape and classification facts: retained while current or recently referenced, then garbage-collected after a grace period.

Historical peer evidence uses a bounded spool and verified off-VPS `EvidenceArchive`. Raw transaction content is stored once by digest and sightings reference it. A local segment is deleted only after remote size and digest verification. Archive failure may cause explicit evidence shedding at quota, but must not corrupt or stop current-state convergence.

SQLite remains the state persistence engine. The production agent accepts at most 200,000 entries, caps its main database at 1 GiB, and admits writes within a 1.75 GiB DB/WAL/SHM envelope. The central server accepts two persistent source identities with at most 200,000 entries in each active or staging generation, caps its main database at 3 GiB, and uses a 3.5 GiB envelope. A non-loopback server bind requires an exact source allowlist whose size fits the identity cap.

Both processes treat 64 MiB as a WAL retention and pressure target, not a transient ceiling. They attempt relief before growth, recheck admission around the write transaction, and retry safe contention without changing frozen state. The application envelope and filesystem reserve reject unsafe new writes, but the deployment filesystem and cgroup are the hard boundaries. Canary agents use fixed 2 GiB ext4 images, the server uses a fixed 4 GiB image, and every Atlas process has a 1 GiB no-swap memory limit. Image creation and service start preserve at least the greater of 20 percent of the host root filesystem or 5 GiB.

## Implemented boundary

The state-only slice enforces the 200,000-entry production membership limit before and after the racy RPC pair, a 4,096-row and 1 MiB estimated dirty budget, 512 entries per checkpoint chunk, and 4 MiB request bodies. A complete RPC observation replaces local membership, repeated changes to one txid coalesce, and dirty pressure falls back to one full checkpoint. The server checks declarations, chunks, commits, post-delta cardinality, and existing database contents against the same configured entry cap.

The shared SQLite policy verifies main-database page ceilings, accounts for DB, WAL, and SHM files, preserves a configured filesystem reserve, and exposes pressure through write rejection and server readiness. WAL checkpoint contention is retryable, while a real capacity breach protects the last committed state. Fixed canary images and systemd cgroups bound disk and memory even when a transaction or buffered RPC response peaks before application admission can intervene.

HotStore and EvidenceArchive are not implemented. Their retention proposals remain separate from the completed SourceReplica state safeguards. Legacy evidence code is isolated behind an explicitly experimental router and is absent from both production process surfaces, so current reads report evidence as `not_collected` rather than implying completeness.

## Considered options

- Extending `NormalizedEvent` with epoch and revision fields was rejected because it preserves the permanent event ledger and shared delivery queue.
- Sending a full checkpoint after every RPC poll was rejected because it creates continuous full-mempool transfer and write amplification.
- Adding TTL pruning to the existing foreign-key graph was rejected because age alone does not provide a physical bound and the read projections still depend on the ledger.
- A larger VPS, periodic `VACUUM`, PostgreSQL, or Kafka was rejected because each delays or relocates the same unbounded lifecycle.

## Consequences

The replacement uses incompatible preproduction schema generations and does not migrate or replay the discarded canary data. The first deployment is state-only and proves epoch replacement, delta replay, atomic checkpoints, bounded outage storage, and capacity rejection before peer evidence is enabled. Evidence gaps are always explicit; Atlas may lose bounded forensic detail under declared policy, but it may not expose partial state or silently claim complete capture.
