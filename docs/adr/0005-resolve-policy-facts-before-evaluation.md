# Resolve policy facts before evaluation

Status: accepted

## Context

The first continuous classifier treated one bounded slice as one attempt. It
selected current witness variants, fetched as many raw transactions and
prevout scripts as the slice planners admitted, then evaluated every fetched
transaction. Any script that had not been scheduled or returned was supplied
to the rule evaluator as missing.

This made several different conditions look identical:

- the node explicitly returned no current output for an outpoint;
- a confirmed-prevout request fell beyond the slice estimate;
- a required outpoint or mempool parent fell beyond another planner bound;
- a request failed in transport, returned malformed data, or was absent from a
  batch response.

Only the first condition is a fact about the current transaction. The others
are unfinished or failed collection work. Converting them into evaluator
unknowns caused deterministic starvation: transactions beyond a stable sorted
prefix were marked indeterminate on every membership generation even when the
node could answer their `gettxout` requests immediately.

The classifier also treated P2SH-wrapped spends as an unsupported evaluator
form. The deployed Bitcoin Knots policy has exact behavior for P2SH,
P2SH-P2WPKH, P2SH-P2WSH, and P2SH-wrapped undefined witness versions, so that
unknown was avoidable without collecting any additional facts.

## Decision

Separate collection lifecycle from policy verdicts.

Each current membership generation owns a bounded pending resolver. A raw
transaction that has been admitted and verified remains pending while its
required scripts are resolved through exact current-parent outputs, a bounded
positive script cache, or `gettxout(txid, vout, false)`. Bounded waves advance
that resolver fairly rather than restarting from the same sorted outpoint
prefix. Successfully admitted raw transactions are not fetched again merely
because their fact work crossed a wave boundary.

Evaluate a candidate only after every required input script is either present
or has a genuine terminal lookup result. A successful null-shaped `gettxout`
response from the trusted Bitcoin Core endpoint becomes a typed missing script
fact only when membership identifies the outpoint as confirmed. If a current
mempool parent's raw lookup failed, the same `gettxout(..., false)` null is
ambiguous: that call deliberately excludes unconfirmed outputs, so the fallback
remains operationally unresolved until its bounded attempts are exhausted.
Capacity deferral, unscheduled work, batch failure, transport failure,
malformed responses, oversized responses, and missing response envelopes stay
inside the resolver as pending, deferred, or failed operational work. They
leave the transaction's public `bip110` value `null`; they do not manufacture
an indeterminate verdict.

Retain successful immutable `OutPoint` to script facts across membership
generations under the configured auxiliary byte bound. An outpoint commits to
its output script, so a validated positive fact may be shared by later current
transactions. Do not cache nulls or failures. Evict admitted positive facts
when necessary rather than allowing early sorted entries to monopolize the
cache. Current-transaction output reuse remains tied to an exact surviving
`txid` and `wtxid`.

Classification draining follows explicit resolver progress. A wave that only
resolves facts is progress and may continue without publishing a snapshot
revision. Each unresolved fact has two attempts for its current source within
one bounded pending window. Exhausting that local budget defers only the
affected candidate for the rest of the generation, releases its retained
working state, and lets queued and later candidate windows continue. A hard
pending-script admission failure has the same candidate-local outcome, with at
most one relief pass per pending candidate and a yield and stale check between
passes. Positive facts already fetched in the call remain available while a
dependent candidate survives. Only a systemic RPC failure pauses the whole
generation, preventing a busy retry loop without allowing a poison item to
stop unrelated work. A newer membership generation makes deferred candidates
eligible again and supersedes all pending work; already in-flight calls may
finish, but stale results cannot update current state or shared positive facts.

Model P2SH evaluation according to the deployed policy. Exempt the final
scriptSig item as the redeemScript blob, apply the element-size rule to prior
scriptSig items and pushes within the redeemScript, and dispatch exact
P2SH-wrapped witness programs with P2SH context. Native-only Taproot and P2A
branches remain unavailable through P2SH, so wrapped witness versions 1 through
16 use the deployed undefined-version rule.

This decision supersedes ADR 0003's attempt-once scheduling and its treatment
of any unavailable planned prevout as an evaluator unknown. It does not change
the current-snapshot API, membership publication, network boundary, or lack of
retained history.

## Consequences

`bip110: null` now has one clear operational meaning: this exact current
witness variant has no completed assessment yet. `indeterminate` means the
evaluator received a genuine unresolved fact, not that Atlas ran out of room in
one internal planner wave.

Classification may require several internal fact waves before a new public
revision appears. Runtime progress reporting must therefore distinguish facts
resolved from transactions assessed. Tests use small injected limits to prove
fair continuation, explicit-null semantics, systemic-failure pausing, stale-result
rejection, and cache bounds without constructing production-sized batches.

Pending raw transactions and in-flight responses consume memory in addition to
the auxiliary cache. Their admission remains bounded independently. The
resolver's unique-prevout count is a wave target with a deliberate one-candidate
exception so an unusually wide transaction cannot be starved forever; the
separate retained-script byte ceiling remains absolute. The production memory
cgroup remains the hard transient limit. Positive script reuse reduces repeated
node work but is not a persistence or recovery path; all state disappears on
process restart.

The current `jsonrpc` 0.18 response model collapses literal `result: null` and
an omitted `result` member before policy code sees the response. The trusted
Bitcoin Core endpoint emits the member correctly, but a later transport change
must preserve its wire presence to enforce the decision against malformed
servers. Bead `atlas-wgx` records that follow-up.

P2SH classification becomes deterministic from facts Atlas already collects.
Tests must match the exact deployed-client behavior, including element-size
boundaries, nested v0 witness programs, wrapped undefined witness versions,
primary-rule ordering, and the distinction between consensus grandfathering
and current mempool policy.

## Alternatives considered

### Increase the confirmed-prevout cap

A larger constant would postpone the sorted-prefix failure but leave the other
planner omissions and RPC failures conflated with genuine missing facts. It
would not establish fairness or a bounded continuation model.

### Add more RPC lanes or memory

Concurrency can reduce wall-clock time for scheduled work, but it cannot make
an omitted outpoint eligible. More memory without a lifecycle model would also
make the working set harder to reason about.

### Retry every partial classification next generation

The observed outpoints occupied the same deterministic prefix across
generations, so repeated membership polls reproduced the same unknowns. This
also needlessly refetched raw transactions whose bytes were already verified
within the generation.

### Publish a new pending verdict

Pending is collection state, not a BIP-110 policy outcome. Adding it to the
evaluator status would mix operational progress with compatible, violating,
and genuinely indeterminate rule results. The existing `null` assessment is
the correct public representation.
