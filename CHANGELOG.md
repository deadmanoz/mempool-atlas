# Changelog

All notable changes to Mempool Atlas will be documented in this file.

## [Unreleased]

- Rebuild Mempool Atlas as a single central service that periodically pulls
  complete mempools from a bounded configured source set over existing private
  WireGuard RPC paths, retains only the latest successful snapshot per source
  in memory, exposes a small source-scoped API, and serves the website.
- Coordinate one to four sources under one RPC work gate: poll complete
  membership sequentially in deterministic rounds, isolate source failures,
  and advance successful source-local classification generations fairly in
  bounded round-robin slices.
- Bracket every membership observation with matching chain-tip reads and expose
  its collection start, completion, and duration so comparisons show sampling
  skew instead of implying simultaneous observations.
- Add a separate browser-derived comparison page that merge-joins two current
  snapshots into present-in-both and two symmetric observed-only regions,
  preserves source-local witness variants and policy assessments, and stores no
  server-side comparison projection or history.
- Encode each published snapshot response once and share its immutable bytes
  across readers so concurrent requests cannot multiply large serialization
  work and memory allocation.
- Restore the single-node Canvas swim view with fee-rate, age, and virtual-size
  filters plus a bounded transaction inspector.
- Make the BIP-110 classification terrain the primary view, with count and
  virtual-size modes, explicit coverage, canonical exact buckets for complete
  violating assessments, separate partial buckets for proven violations with
  unresolved checks, overlapping marginal rule filters, and per-transaction
  rule evidence. Retain fee-rate by age as a secondary lens and first rejection
  as transaction-detail metadata.
- Add a pure seven-rule RDTS evaluator with separate consensus and deployed
  Knots mempool-policy modes, deterministic primary rejection, all proven
  violations, and typed unknown facts.
- Publish complete membership independently from policy work, then resolve the
  current generation through bounded concurrent candidate and script-fact
  waves before evaluation.
- Preserve missing, null, and value JSON-RPC results through an Atlas-owned
  policy transport, cap each classification body before parsing, reconcile
  concurrent responses by request ID, and keep malformed omissions out of
  terminal missing facts.
- Keep each admitted, verified raw transaction pending across same-generation
  fact waves so bounded work continues fairly without refetching the candidate.
- Reserve typed missing script facts for successful null-shaped
  `gettxout(..., false)` responses from the trusted Bitcoin Core endpoint when
  the outpoint is known not to be a current mempool parent. Keep an ambiguous
  null fallback after parent-raw failure, capacity deferral, unscheduled work,
  and failed collection as bounded operational state with an unclassified
  `bip110: null`.
- Reuse exact surviving `txid` and `wtxid` classifications and current outputs,
  retain positive confirmed `OutPoint` scripts across generations under bounded
  eviction, and never cache nulls or failures.
- Evaluate P2SH redeemScript pushes, exact P2SH-P2WPKH and P2SH-P2WSH spends,
  and P2SH-wrapped witness versions 1 through 16 according to deployed Knots
  mempool policy.
- Bound classification with a configurable candidate-window size, RPC lanes,
  auxiliary script-cache admission, and retained pending-script admission while
  retaining no history.
- Cap each candidate window at 8,192 variants and a 256 MiB candidate-raw
  estimate, each mempool-parent wave at 8,192 transactions and a 256 MiB
  estimate, and each confirmed-prevout fact wave at a 256 MiB aggregate
  estimate. Use 65,536 unique required prevouts as the per-window target with
  one byte-bounded singleton exception.
- Drive classification with explicit continue, complete, paused, and stale
  outcomes; continue after fact-only progress without publishing a revision,
  defer only candidates that exhaust two attempts for one fact source in a
  bounded pending window, pause only on systemic RPC failure, and reject
  superseded results before they can update current state or shared positive
  facts.
- Count generation-local candidate deferrals separately from systemic pauses so
  operators can distinguish poison facts and capacity pressure from a broken
  RPC path.
- Expose `classification_revision` with snapshots and transaction detail so the
  browser can keep progressive rule evidence consistent with its visible
  terrain.
- Remove the experimental node agents, SQLite replicas, checkpoint and delta
  protocol, evidence pipeline, classifiers, database tooling, and earlier
  server-projected multi-node comparison surface from the active workspace.
  Their code and lessons remain available in Git history.
- Separate current-state visualisation from historical archival. Attempt #3
  intentionally stores no application history and requires no database
  migration or recovery path.
- Validate the first deployed single-source slice over the existing WireGuard
  RPC path, including public proxy limits, complete snapshot rendering,
  client-side filtering, proxy temp-file behavior, and target-host memory.
