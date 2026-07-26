# Use exact rule-combination buckets in the classification terrain

Status: accepted

## Context

The first classification terrain assigned each violating transaction to the
evaluator's deterministic first rejection. That produced one territory per
rule, but it hid the rest of a transaction's proven violations. A transaction
that violated both R2 and R7 appeared only under whichever rule rejected first,
even though the snapshot already carried both facts.

Duplicating the transaction into both rule territories would expose the
overlap, but it would make the terrain cease to be a partition of the current
mempool. It would also make transaction counts and areas ambiguous. Partial
assessments add a separate concern: a transaction can have proven violations
while one or more rule checks remain unresolved, including a proven and an
unknown result for the same rule.

The compact snapshot assessment already carries `violated_rules`,
`unknown_rules`, and `primary_rule`. The interface can represent these facts
without changing the backend, API, or runtime publication model.

## Decision

Derive a canonical rule signature for every violating assessment in the
browser. Map the seven rule IDs to fixed bit positions in their public R1 to R7
order, independently of array order and `primary_rule`.

When `unknown_rules` is empty, place the transaction in an exact bucket keyed
by its complete `violated_rules` mask. An assessment that violates R2 and R7
therefore belongs to the `R2 + R7` bucket, regardless of which rule rejected
first. Render only exact combinations observed in the current snapshot rather
than allocating every theoretical combination.

When any rule remains unknown, place the transaction in a separate partial
bucket keyed by both its proven and unknown masks. Do not label that population
as an exact rule set. Preserve a rule in both masks when the assessment contains
both a proven violation and unresolved facts for that rule.

Place every transaction in exactly one terrain region. Compatible,
indeterminate, and unclassified transactions retain their distinct status
regions. Complete and partial violating assessments retain distinct sections.

Keep the rule controls as marginal filters. Selecting R2 includes every exact
or partial bucket with a proven R2 violation, including `R2 only`, `R2 + R7`,
and any other observed combination. These rule totals intentionally overlap
and are not additive. Selecting a combination instead scopes the inspector to
that exact or partial signature. Compatible, indeterminate, and unclassified
status regions are selectable through the same inspector without borrowing a
rule or combination identity.

Keep `primary_rule` as first-rejection metadata in transaction detail. It does
not choose the terrain bucket. Count and virtual-size modes continue to choose
the layout metric and transaction-tile area, while section and bucket frames
use bounded readability weighting.

This decision changes only browser grouping and presentation. It does not
change the snapshot contract, the source-scoped API, classification work, or
runtime publication. It supersedes only the interface-grouping paragraph in
[ADR 0003](0003-periodic-in-memory-snapshots.md).

## Consequences

The terrain now exposes multi-rule assessments directly while remaining a true
partition of current membership. Exact bucket identity is stable when API rule
arrays arrive in a different order or when first rejection differs.

Rule-filter totals can sum to more than the total violating population, so the
interface must state that they overlap. Exact and partial totals remain
separate because unresolved checks cannot justify an exact combination claim.

There are 127 possible non-empty exact masks, but the browser creates regions
only for combinations present in the current snapshot. Partial combinations
are likewise observed-state only. This avoids persistent state and theoretical
bucket allocation while keeping the current-snapshot memory model unchanged.

Tests must cover order-independent signatures, separation of exact and partial
assessments, overlapping marginal rule counts, one terrain glyph per
transaction, and layouts that allocate only observed buckets.

## Alternatives considered

### Keep first-rejection territories

This preserves a simple seven-territory layout but hides independently proven
violations and makes combinations such as R2 plus R7 invisible in the primary
view.

### Duplicate transactions into every violated-rule territory

This makes marginal membership obvious but breaks the terrain's one-to-one
relationship with current mempool membership. Counts and areas would require a
second interpretation.

### Assign combinations to a lowest or highest rule

This still discards the complete rule-set identity and merely replaces one
arbitrary primary grouping rule with another.

### Add backend combination aggregates

The existing compact assessment already contains all facts needed to derive
the grouping. A new endpoint would duplicate current-snapshot semantics and
couple the backend to one presentation without improving correctness.
