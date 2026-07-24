# Changelog

All notable changes to Mempool Atlas will be documented in this file.

## [Unreleased]

- Rebuild Mempool Atlas as a single central service that periodically pulls one
  complete mempool over the existing private WireGuard RPC path, retains only
  the latest successful snapshot in memory, exposes a small source-scoped API,
  and serves the website.
- Encode each published snapshot response once and share its immutable bytes
  across readers so concurrent requests cannot multiply large serialization
  work and memory allocation.
- Restore the single-node Canvas swim view with fee-rate, age, and virtual-size
  filters plus a bounded transaction inspector.
- Make the BIP-110 classification terrain the primary view, with count and
  virtual-size modes, explicit coverage, seven first-reject rule territories,
  and per-transaction rule evidence. Retain fee-rate by age as a secondary
  lens.
- Add a pure seven-rule RDTS evaluator with separate consensus and deployed
  Knots mempool-policy modes, deterministic primary rejection, all proven
  violations, and typed unknown facts.
- Publish complete membership independently from policy work, then continuously
  enrich only its current in-memory generation through bounded concurrent
  `getrawtransaction` and `gettxout(..., false)` slices.
- Reuse exact surviving `txid` and `wtxid` classifications across membership
  generations, prefer fresh unclassified variants before retryable partial
  results, and discard stale work through generation and revision guards.
- Bound classification with configurable slice size, RPC lanes, and auxiliary
  script-cache admission while retaining no history.
- Cap each slice at 8,192 candidates, each candidate-raw and mempool-parent-raw
  phase at a 256 MiB response estimate, required prevouts at 65,536 unique
  outpoints, and confirmed-prevout work at a 256 MiB aggregate estimate.
- Stop scheduling new work for superseded generations and pause a generation
  after systemic no-progress RPC failure instead of rapidly draining more
  failing slices.
- Expose `classification_revision` with snapshots and transaction detail so the
  browser can keep progressive rule evidence consistent with its visible
  terrain.
- Remove the experimental node agents, SQLite replicas, checkpoint and delta
  protocol, evidence pipeline, classifiers, database tooling, and multi-node
  comparison surface from the active workspace. Their code and lessons remain
  available in Git history.
- Separate current-state visualisation from historical archival. Attempt #3
  intentionally stores no application history and requires no database
  migration or recovery path.
- Validate the first deployed single-source slice over the existing WireGuard
  RPC path, including public proxy limits, complete snapshot rendering,
  client-side filtering, proxy temp-file behavior, and target-host memory.
