# Separate source state from bounded evidence

Status: accepted; SourceReplica slice implemented

Mempool Atlas separates RPC-authoritative source state, recent product evidence, and historical forensic evidence into independently bounded data products. State convergence must not share a FIFO or disk budget with forensic evidence.

The live canaries demonstrated that the original event-ledger design amplified a central delivery failure into unbounded source storage. Core accumulated a 7.13 GB agent database, while Knots accumulated 26.87 GB and exhausted its root filesystem. Replaying or pruning that same mutation stream would retain the coupling that caused the failure.

## Decision

Current membership is owned by a deep `SourceReplica` module. A stable `source_id` names the logical node and a persisted `epoch_id` names one continuous state history. Routine process restarts retain the epoch. Agent database loss, an incompatible schema reset, or an intentionally abandoned history creates a new epoch.

RPC is the sole authority for SourceReplica membership. A changed RPC snapshot advances one source revision. Healthy synchronization sends sorted, coalesced deltas from an acknowledged base revision to a newer target revision. Repeated changes to one transaction occupy one dirty record rather than an append-only mutation history.

Bootstrap, cursor mismatch, epoch replacement, or dirty-set pressure uses an atomic full checkpoint. The server stages one inactive membership generation, verifies its declared entry count and canonical digest, then activates the complete epoch and generation in one transaction. Readers see either the prior complete generation or the replacement complete generation. A different begin can supersede staging only by naming its exact checkpoint ID, and a same-epoch checkpoint must strictly advance the replaced revision. Delayed traffic cannot preempt newer staging or mutate active state.

At most two membership generations exist per source: one active and one staging. A central outage therefore costs storage proportional to current membership, not outage duration or observation rate. A poison forensic observation cannot block state synchronization.

Recent product evidence is stored in a bounded `HotStore`, not a generic permanent event table. Every variable-sized structure has semantic and physical bounds, including age, count, and bytes where applicable. The initial policy is:

- Rejections: seven days or 10,000 records per source, whichever is reached first.
- Other recent evidence metadata: six hours or 250,000 records per source.
- Capture gaps: 256 recent intervals plus compact cumulative counters.
- Idempotency receipts: an accepted-through watermark plus 4,096 recent sequence and digest pairs per source stream.
- Shape and classification facts: retained while current or recently referenced, then garbage-collected after a grace period.

Historical peer evidence uses a bounded spool and verified off-VPS `EvidenceArchive`. Raw transaction content is stored once by digest and sightings reference it. A local segment is deleted only after remote size and digest verification. Archive failure may cause explicit evidence shedding at quota, but must not corrupt or stop current-state convergence.

SQLite remains the persistence engine. The initial capacity envelope is 2 GiB per source VPS and 10 GiB for the central hot store, while preserving at least the greater of 20 percent of the filesystem or 5 GiB outside Atlas. Pressure moves through warning, evidence shedding, state-only operation, and protective Atlas writer shutdown.

## Implemented boundary

The state-only slice enforces one million membership entries, 4,096 dirty mutations, a 1 MiB estimated dirty-byte budget, 512 entries per checkpoint chunk, 4,096 checkpoint chunks, and 4 MiB request bodies. The agent configures a 1 GiB main SQLite page cap, a 64 MiB retained WAL journal limit, and WAL auto-checkpointing every 1,000 pages. The server schema permits at most one active and one staging generation per source.

Those controls bound semantic state and the main agent database, but they do not complete the physical capacity envelope. Transient WAL peak, measured two-snapshot overhead, central database bytes, and the filesystem free-space reserve still require deployment guards and canary measurements. HotStore and EvidenceArchive are not implemented. Legacy evidence code is isolated behind an explicitly experimental router and is absent from both production process surfaces.

## Considered options

- Extending `NormalizedEvent` with epoch and revision fields was rejected because it preserves the permanent event ledger and shared FIFO.
- Sending a full checkpoint after every RPC poll was rejected because it creates continuous full-mempool transfer and write amplification.
- Adding TTL pruning to the existing foreign-key graph was rejected because age alone does not provide a physical bound and the read projections still depend on the ledger.
- A larger VPS, periodic `VACUUM`, PostgreSQL, or Kafka was rejected because each delays or relocates the same unbounded lifecycle.

## Consequences

The replacement uses incompatible preproduction schema generations and does not migrate or replay the discarded canary data. The first deployment is state-only and proves epoch replacement, delta replay, atomic checkpoints, bounded outage storage, and capacity rejection before peer evidence is enabled. Evidence gaps are always explicit; Atlas may lose bounded forensic detail under declared policy, but it may not expose partial state or silently claim complete capture.
