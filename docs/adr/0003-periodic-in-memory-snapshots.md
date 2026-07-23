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
current mempool of one remote Bitcoin node? The node and website already have a
private WireGuard path between them. Experimental data does not need to survive.

## Decision

Run one Mempool Atlas process on the presentation host. It periodically pulls a
complete verbose mempool snapshot through the existing WireGuard-only node RPC
proxy. It validates the whole observation, atomically replaces one in-memory
snapshot, exposes that snapshot through a source-scoped HTTP API, and serves the
single-node website.

No Atlas process runs on the node. The viewer has no database, event log,
delivery protocol, or retained history. A failed poll preserves the last good
in-memory snapshot and makes its stale state visible. A restart waits for a new
snapshot.

The first interface is the Canvas "swimming in the mempool" view. The browser
loads the server's latest snapshot on demand and does not trigger node RPC.

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
schemas, checkpoint protocol, evidence pipeline, classifiers, and comparison
UI. Git history remains the archive for that experiment.

The system is intentionally disposable and operationally small. There are no
migrations, database backups, replay guarantees, or node-side Atlas units.

A full verbose RPC response and two briefly overlapping snapshots can consume
significant memory. The 200,000-entry limit bounds membership count but not
response bytes or allocator peaks. Target-host acceptance must measure peak
memory, and later multi-node collection should avoid overlapping large
responses unless measurements justify it.

This design gives up history and incremental transfer in exchange for an
auditable vertical slice. If the viewer proves valuable, comparison and
archival can deepen independently from evidence rather than preemptively.
