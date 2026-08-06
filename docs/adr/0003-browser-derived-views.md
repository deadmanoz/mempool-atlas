# Derive comparison and terrain partitions in the browser

Status: accepted

## Context

Comparison needs two complete source observations, not a third combined
mempool. The browser also needs to show transactions that match several labels
or violate several rules without duplicating them and corrupting area or count
totals.

The observations are independent and may have different witness variants,
policy assessments, chain tips, or collection times. Absence from one sample is
not rejection evidence.

## Decision

The server exposes only independent source snapshots. The comparison page
fetches two snapshots and merge-joins their sorted `txid` arrays in the browser
to derive three disjoint regions:

- present in both snapshots;
- observed only in the left snapshot; and
- observed only in the right snapshot.

Common transactions retain both source-local entries. The policy matrix uses
four source-local rows: left-only on the left, common on the left, common on the
right, and right-only on the right. It never converts membership absence into
a policy verdict.

The node terrain is also a partition driven by the selected classifier. Every
lens separates complete, partial, and unavailable results. Smaller generic
lenses assign each result to one canonical bucket keyed by its exact observed
label set. `transaction_properties` instead uses a browser-only summary adapter
with broad script-profile groups; exact property labels remain on the
transaction and in marginal controls.

The `knots_bip110` lens uses a specialist adapter. Compatible, indeterminate,
and unset assessments have distinct regions. A complete violating assessment
is assigned to one canonical bucket keyed by its exact set of violated rules.
An assessment with unresolved rules is assigned to a separate partial bucket
keyed by both the proven and unresolved sets.

Label and rule controls are marginal filters over those canonical regions. They
may highlight overlapping populations, but they do not change which single
region owns a transaction. `primary_rule` remains transaction-detail metadata
and does not choose terrain placement.

The node page keeps source, classifier, optional policy selection, and
transaction state in the URL. Comparison keeps its source pair, membership
region, policy filter, and optional transaction. Pair or source changes clear
incompatible state, and obsolete requests cannot replace a newer selection.

## Consequences

- The server retains no combined comparison projection or comparison cache.
- Counts and terrain area conserve membership because each transaction appears
  exactly once.
- Multi-label and multi-rule overlap remains visible through lens-specific
  groups, exact combination buckets where useful, and marginal filters.
- Sampling skew and source-local lifecycle must remain visible in comparison.
- The browser must stay linear in snapshot size and avoid one DOM element per
  transaction.
