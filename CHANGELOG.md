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
- Enrich only the current snapshot through bounded `getrawtransaction` and
  `gettxout(..., false)` batches, verify `txid` and `wtxid`, cache by the
  source-bound witness variant, advance work with a fair round-robin cursor,
  and retain no history.
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
