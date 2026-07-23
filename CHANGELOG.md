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
