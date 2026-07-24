# Use periodic in-memory snapshots for the first product slice

Status: accepted

## Context

The first two Mempool Atlas experiments coupled too many concerns. Attempt #1
allowed captured events to grow without a bound. Attempt #2 addressed that
failure with source replicas, SQLite capacity policies, deltas, checkpoints,
staging generations, and per-node services. It was bounded, but it made the
system difficult for a human reviewer to understand and operate before the
product itself had been proven.

The current product question is smaller: can one person usefully explore the
current mempool of one remote Bitcoin node, including exactly how its current
transactions relate to the seven BIP-110 rules? The node and website already
have a private WireGuard path between them. Experimental data does not need to
survive.

## Decision

Run one Mempool Atlas process on the presentation host. It periodically pulls a
complete verbose mempool snapshot through the existing WireGuard-only node RPC
proxy. It validates the whole observation, atomically replaces one in-memory
snapshot and matching transaction-detail map, exposes that current observation
through a source-scoped HTTP API, and serves the single-node website.

Complete membership remains authoritative and uses `getmempoolinfo`, verbose
`getrawmempool`, and `getblockchaininfo`. After membership succeeds, the same
central process performs bounded, best-effort classification through
`getrawtransaction` and `gettxout(txid, vout, false)`. It caches only
source-bound current `wtxid` variants and prunes results with membership.

Missing raw transaction data leaves a member explicitly unclassified. Missing
prevouts produce typed unknowns, preserve any independently proven rule
violations, and remain eligible for retry. The current detail map retains exact
evidence and missing-fact counts with at most one exemplar of each per rule.
Classification gaps do not invalidate fresh membership.

No Atlas process, database, event log, queue, container, new listener, delivery
protocol, or retained history runs on the node. The central viewer also has no
database, event log, queue, or retained history. A failed membership poll
preserves the last good in-memory observation and makes its stale state visible.
A restart waits for a new snapshot.

The primary interface is a Canvas classification terrain. It partitions the
current source into compatible, indeterminate, unclassified, unresolved-primary,
and seven exact BIP-110 rule territories. Count and virtual-size modes determine
transaction-tile area within each territory. The fee-rate-by-age view remains
available as a secondary lens.
The browser loads the server's latest snapshot on demand and does not trigger
node RPC.

Classification means compatibility with the RDTS/BIP-110 rules deployed as
Bitcoin Knots mempool policy. It does not assert that the observed source
rejected the transaction and does not assert consensus invalidity. A pure
evaluator also exposes a separate consensus mode for tests and future consumers,
but the viewer does not use it.

Archival is a separate system with its own format, storage target, quota, and
retention policy. It must not be on the viewer's request path or share the
viewer's failure domain.

Multi-node comparison is a separate product surface built after the one-node
slice. It may reuse the flat snapshot contract and collector, but it keeps
sources independent and derives comparison at read time. It must expose
observation skew and must not treat absence from a source as evidence of
rejection, filtering, or relay causality.

## Consequences

The active repository removes the attempt #2 agents, server reducer, SQLite
schemas, checkpoint protocol, event evidence pipeline, and comparison UI. Git
history remains the archive for that experiment. The new pure rule evaluator
and bounded transaction detail are current-snapshot components rather than a
retained evidence system.

The system is intentionally disposable and operationally small. There are no
migrations, database backups, replay guarantees, or node-side Atlas units.

A full verbose RPC response and two briefly overlapping snapshots can consume
significant memory. The 200,000-entry limit bounds membership count but not
response bytes or allocator peaks. The current classification cache and detail
map add bounded-per-entry state. Classification defaults to 10,000 uncached or
incomplete witness variants per poll and a 45-second soft budget for starting
more work. Raw transaction and mempool-parent batches contain at most 16
requests, confirmed prevout batches contain at most 128, and classification
batches have a 20-second transport timeout. The complete membership path keeps
the `corepc-client` 0.8 fixed 15-second transport timeout.

Target-host acceptance must measure peak memory and both RPC paths. Later
multi-node collection should avoid overlapping large responses unless
measurements justify it.

This design gives up history and incremental transfer in exchange for an
auditable vertical slice. If the viewer proves valuable, comparison and
archival can deepen independently from evidence rather than preemptively.
