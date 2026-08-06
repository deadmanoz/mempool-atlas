# Separate policy facts from collection state

Status: accepted

## Context

The policy evaluator needs exact transaction bytes and input scripts. A missing
fact can mean that the node authoritatively reported no output, but it can also
mean work was not scheduled, a bounded batch was deferred, transport failed, or
a malformed response omitted data. Treating all of those cases as evaluator
unknowns manufactures policy conclusions from operational failures.

Bitcoin JSON-RPC adds an important distinction: a present `result: null` and an
omitted `result` member are not equivalent for `gettxout`.

## Decision

Atlas keeps fact collection separate from rule evaluation.

A verified raw transaction remains in a bounded pending window while its input
scripts are resolved. Fact work advances in fair bounded waves and does not
refetch a candidate merely because it crossed a wave boundary. Current
mempool-parent outputs are resolved from verified raw parents; confirmed
prevouts use `gettxout(txid, vout, false)`.

The evaluator runs only when every required script is present or has a genuine
terminal result. A present null `gettxout` result is terminal only for an
outpoint known not to be a current mempool parent. Unscheduled work, capacity
deferral, batch failure, malformed responses, and missing envelopes remain
collection state and leave the public assessment unset.

Atlas owns the classification JSON-RPC wire boundary so it can:

- preserve missing, null, and value results distinctly;
- preserve error-member presence and give valid errors precedence;
- reconcile out-of-order batch responses by unique request ID;
- reject duplicate, unexpected, excess, or missing responses; and
- bound each response body before JSON parsing.

The pure evaluator can report compatible, violating, or indeterminate status
with typed evidence and missing-fact counts. A proven violation and an
unresolved fact may coexist for the same rule.

Classification lifecycle is published separately as `classifying`, `complete`,
or `paused`. Complete can retain transactions whose assessments were
unavailable after bounded work was exhausted.

## Consequences

- Operational failure cannot be presented as policy evidence.
- Progress may occur without an immediately visible assessment revision.
- Bounded retries and deferral let later candidates continue.
- Positive confirmed script facts may be reused under bounded eviction, while
  nulls and failures are never cached.
- The transport is more explicit than a general JSON-RPC client, but the
  evaluator remains pure and independently testable.
