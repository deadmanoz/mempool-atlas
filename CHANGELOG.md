# Changelog

All notable changes to Mempool Atlas will be documented in this file.

## [Unreleased]

- Publish classification revisions through changed-only deltas and perform
  snapshot materialization, validation, and JSON encoding after releasing the
  shared RPC work gate. This removes three accumulated classification-map
  passes while preserving atomic snapshot, detail, lifecycle, body, and ETag
  replacement.
- Cache aggregate-only distribution models by snapshot and semantic selection,
  and build the comparison policy projection in one pass with bounded samples.
  Revisited views no longer rebuild all nine panels or repeatedly rescan policy
  populations, while transaction-only selection changes avoid population
  rendering and identical selections perform no work.
- Align the internal generation, fact-resolution, and RPC orchestration with
  classification vocabulary now that the pipeline serves all four lenses,
  while retaining precise Knots/BIP-110 policy and wire-visible names.
- Move each distribution section's DOM, cache, resize, render, and reset
  lifecycle out of the page controllers, and share Canvas backing preparation
  without merging renderer-specific geometry or hit-testing behavior.
- Consolidate the Rust workspace into one root package and internalize the
  private BIP-110 evaluator without changing its wire-visible identifier or
  assessment semantics.
- Publish each transaction's delta-adjusted ancestor fees and add a Package
  fee rate panel to both pages: effective ancestor fee rate (package fees over
  package virtual size), the score a miner evaluates, stacked by the selected
  classifier's buckets.
- Add a population scope selector to the comparison page's mirrored
  distribution panels. Panels can now summarize the whole snapshot, the
  transactions present in both snapshots, or the transactions observed in only
  one source, derived from the existing browser merge-join; the absent side
  states plainly that the population has no members there.
- Surface membership and structure facts in transaction detail: weight,
  package and descendant ancestry with the effective package fee rate,
  source-reported replaceability, input and output counts, output value,
  witness bytes, and OP_RETURN payload bytes, on both the node inspector and
  the per-source comparison detail.
- Publish per-transaction structure facts with every snapshot. Membership rows
  now carry the source-reported weight, ancestor and descendant counts and
  virtual sizes, and effective BIP-125 replaceability; a progressive
  `structure` object adds input and output counts, OP_RETURN payload bytes,
  total output value, and witness bytes derived from the raw transaction
  during classification. `structure` is non-null exactly when classifier
  results are present, and the browser validates both the coupling and the
  membership-fact consistency bounds.
- Add four structure-fact panels to the Snapshot distributions section and
  mirror them per source on the comparison page: Data carriage (OP_RETURN
  payload sizes by data-protocols bucket), Inputs × outputs (complexity
  density with marginals), Entanglement (banded unconfirmed ancestors and
  descendants plus the reported replaceability share), and Value moved (total
  output value by the selected classifier's buckets).
- Add a browser-derived Snapshot distributions section to the node view with
  four question-oriented panels in classification-first order: per-lens
  composition bars, a fee structure spectrum stacked by the selected
  classifier's buckets, a joint fee-rate-by-size density heatmap with
  marginals, and a bucket-by-age mosaic. Composition segments and mosaic
  columns deep-link into Buckets selections, and all aggregates are computed
  in the browser from the complete published snapshot.
- Add the same four panels to the comparison page as mirrored source-local
  pairs on shared fixed axes, weighted by virtual size.
- Add an honesty banner that names a retained observation's age and poll
  failure when a source is stale, and reuses the paused-classification summary
  when assessments are missing, plus a pulsing freshness indicator on the
  healthy status strip.
- Add independently versioned exact-property, transaction-shape,
  data-protocol, and BIP-110 classifier lenses over one shared fact pipeline.
- Publish classifier catalogs, per-lens coverage summaries, compact snapshot
  results, and bounded transaction-detail evidence.
- Make Classifications the default node view, drive Buckets from the selected
  classifier, adapt each lens to readable presentation groups, and retain
  BIP-110 as a specialist rule adapter alongside fee-rate-by-age.
- Preserve individual transaction blocks inside proportional classifier
  buckets, cache classifier partitions across interactions, suppress labels
  that cannot fit, and disclose marginal filters, samples, and transaction
  evidence only when requested.
- Restore direct transaction selection from every Buckets block and summarize
  Transaction properties by broad script profile without discarding exact
  version, RBF, witness, data-carrier, or script-family labels.
- Compress large snapshot responses with gzip when the client supports it.
- Specify classifier rules, thresholds, partial-result semantics, and known
  limitations independently in `docs/classification.md`.
- Prepare the first public release under the MIT License with contributor,
  security, conduct, CI, and dependency-maintenance guidance.
- Provide a current-state service for one to four independent Bitcoin mempools,
  with complete source observations and progressive BIP-110 classification.
- Add an interactive classification terrain with exact multi-rule buckets,
  policy evidence, coverage state, transaction search, and a fee-rate view.
- Add browser-derived pairwise comparison with source-local policy results,
  membership regions, collection windows, and sampling-skew disclosure.
- Keep the product memory-only, bounded, and source-scoped, with no database,
  retained history, node-side agent, or server-side combined mempool.
- Document the product with verified screenshots, an editable architecture
  diagram, a portable quick start, and current design decisions only.
- Support revision-aware full-snapshot revalidation with shared `ETag` values
  and bodyless `304` responses while retaining `no-store` for waiting, compact,
  operational, and error responses.
- Add a hardened loopback-origin deployment path through Cloudflare Tunnel,
  including cache, WAF, rate-limit, security-header, monitoring, smoke-test,
  failure, and rollback guidance without committing deployment secrets.
