# Keep only independent current snapshots

Status: accepted

## Context

The product answers questions about the mempool a node reports now. Retained
event streams, historical evidence, and server-side convergence add storage and
operational lifecycles that are not required for that experience. Multiple
nodes also do not share one authoritative mempool.

## Decision

Atlas periodically collects a complete mempool observation from each
configured Bitcoin node and retains only the latest successful observation for
that source in memory.

Each observation is source-scoped and records its collection window. Atlas
reads the chain tip before and after membership collection and publishes only
when both reads match. This rejects collections with an observed block or
reorg boundary, but does not freeze normal mempool changes or make
`getmempoolinfo` and `getrawmempool` one atomic read. A failed replacement
leaves the prior source snapshot visible as stale.

Membership is published before best-effort classification finishes. Exact
classifier results may survive only when both `txid` and `wtxid` still match.
New result batches progressively replace the same current observation under a
monotonic classification revision.

One service-wide work gate prevents membership and classification RPC work
from overlapping. Membership rounds poll sources sequentially; classification
advances source-local generations fairly between rounds.

The process writes no application state to disk. Restarting discards all
observations and readiness waits for a new valid snapshot.

## Consequences

- There is no database migration, retention, replay, compaction, or recovery
  path.
- Atlas cannot answer historical arrival, rejection, or relay questions.
- A source failure is isolated and does not invalidate another source.
- Comparing sources must preserve their independent collection windows and
  cannot imply simultaneous observation.
- Archival or forensic evidence belongs in a separate product with its own
  storage and reliability model.
