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
proxy. It validates and publishes complete membership independently from
classification, progressively replaces that membership's in-memory
classification detail, exposes the current observation through a source-scoped
HTTP API, and serves the single-node website.

Complete membership remains authoritative and uses `getmempoolinfo`, verbose
`getrawmempool`, and `getblockchaininfo`. A fixed-interval membership loop
installs each successful result as a new process-local generation. It
immediately publishes complete membership with only the exact `txid` and
`wtxid` classifications that survived from the previous generation.

A separate loop then continuously drains bounded, best-effort classification
slices through `getrawtransaction` and
`gettxout(txid, vout, false)`. Fresh unclassified variants run before carried
partial results with missing prevouts. Each witness variant is attempted at
most once per membership generation and becomes eligible again with the next
successful generation.

Policy results merge only while their generation remains current. Runtime
publication separately requires the same generation and a strictly increasing
revision. Work superseded by newer membership is discarded rather than
crossing the snapshot boundary, and its RPC schedulers stop starting new waves.
Already in-flight transport work can finish. A systemic slice that produces no
classifications and reports batch or all-response failure pauses the rest of
that generation until the next membership.

Each new membership exposes `classification_revision` 0 and every published
classification slice advances it. Both snapshots and transaction details carry
this membership-local revision. The browser rejects older detail and detail
whose compact assessment changed, while allowing a later detail revision when
source, membership, transaction identity, witness identity, and compact
assessment remain consistent.

Missing raw transaction data leaves a member explicitly unclassified. Missing
prevouts produce typed unknowns, preserve any independently proven rule
violations, and remain eligible for retry. The current detail map retains exact
evidence and missing-fact counts with at most one exemplar of each per rule.
Classification gaps do not invalidate fresh membership.

No Atlas process, database, event log, queue, ZMQ subscriber, container, new
listener, delivery protocol, or retained history runs on the node. The central
viewer also has no database, event log, queue, or retained history. It uses only
the existing WireGuard RPC route and adds no network path. A failed membership
poll preserves the last good in-memory observation and makes its stale state
visible. A restart waits for a new snapshot.

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
response bytes or allocator peaks. Classification defaults to 2,048 candidates
per slice, configurable up to a hard 8,192-entry slice maximum, and four
concurrent raw RPC lanes, with at most eight configurable lanes. Candidate raw
work and mempool-parent raw work have separate 256 MiB aggregate response
estimates. The parent phase also caps at 8,192 transactions, and raw batches
contain at most 256 requests.

One slice considers at most 65,536 unique required prevouts. Confirmed prevout
batches have a nominal 512-request cap, but the 16 MiB estimate and 64 KiB
per-script-hex bound currently reduce that to 254. The confirmed phase has its
own 256 MiB aggregate response estimate, permitting 4,064 worst-case calls
under current constants. Classification batches have a 20-second transport
timeout. Returned transaction hex is capped at 8,000,000 characters and each
decoded JSON-RPC response envelope at 16 MiB.

The minreq transport buffers and parses a complete HTTP response before that
decoded envelope guard runs. The phase limits bound estimated planned work, not
transport memory. The production 2 GiB memory cgroup is the hard transient
boundary.

The auxiliary output-script cache uses a 256 MiB default admission estimate
with a 512 MiB configuration maximum. This is not a process-memory quota:
membership responses, classifications, encoded API responses, concurrent RPC
work, allocator overhead, and overlapping readers remain outside it. The
complete membership path keeps the `corepc-client` 0.8 fixed 15-second
transport timeout.

The membership interval is independent of classification. Normal start-to-start
cadence remains fixed while membership work fits inside the interval. A
membership overrun delays the next tick instead of triggering catch-up polls.

Target-host acceptance must measure peak memory and both RPC paths. Later
multi-node collection should avoid overlapping large responses unless
measurements justify it.

This design gives up history and incremental transfer in exchange for an
auditable vertical slice. If the viewer proves valuable, comparison and
archival can deepen independently from evidence rather than preemptively.
