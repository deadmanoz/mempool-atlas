# Changelog

All notable changes to Mempool Atlas will be documented in this file.

## [Unreleased]

- Derive transaction shape facts and first classifier verdicts server-side from observed raw P2P transaction bytes: total output value, input and output counts, a documented dominant-script-type rule, and an ordered baseline heuristic rule pack (coinjoin, consolidation, batch, data, lightning, payment), persisted per txid in schema generation 4 and surfaced through every summary dimension with explicit per-histogram underived buckets instead of guesses.
- Add a source-scoped aggregate mempool summary endpoint with canonical shared bins, server-side filter facets, optional ECDF and joint fee-size detail blocks, explicitly unavailable underived dimensions, and a source discovery endpoint, backed by golden read-API fixtures that a contract test keeps equal to live responses.
- Add a feature-gated deterministic dev-seed tool that refreshes a synthetic per-classification mempool through the real batch-ingest path so local development has realistic data without live nodes.
- Make an aggregate visualization workbench the primary browser view, with source discovery and switching, classification and script filter chips, switchable composition and treemap heroes, a per-class fee-rate ECDF, a joint fee-by-size heatmap with marginals, an in-payload capture-honesty banner, and five-second polling, while moving per-transaction detail behind an on-demand inspector.
- Enrich source-local membership with exact verbose RPC virtual size, base fee, and node entry time while representing peer-observer admissions awaiting those facts explicitly.
- Render large selected-source snapshots as a dependency-free Canvas fee-by-age swim view with bounded transaction inspection.
- Preserve the in-process classifier contract as a required MVP extension point.
- Make one selected source the primary Mempool Atlas read model and reserve cross-source comparison for an optional derived workspace.
- Persist source-scoped NATS capture gaps, expose historical evidence integrity with each source snapshot, and warn without claiming that RPC recovered missing forensic evidence.
- Replace stale preproduction central and agent databases through a coordinated backup-first schema reinitialization instead of evolving them in place.
- Add live node-local peer-observer NATS capture with visible slow-consumer loss, a source-bound durable FIFO outbox, independent HTTP delivery, and periodic RPC membership reconciliation.
- Default P2P transaction capture to inbound observations while preserving all mempool evidence, with an explicit full P2P transaction relay diagnostic policy and capture accounting.
- Batch contiguous RPC reconciliation events into atomic, bounded FIFO deliveries while retaining individual event identities and the single-event evidence path.
- Add explicit RPC-only agent operation and order capture against RPC reconciliation with a shared projection fence.
- Add backup-first migration and run targets for the agent database.
- Accept large raw-transaction evidence payloads at the event ingest endpoint.
- Add a system architecture visualisation that distinguishes the evidence and state planes.
- Clarify source-partitioned storage and derived comparison semantics.
- Establish the standalone capture-first project scaffold.
- Decode pinned peer-observer mempool and P2P transaction events into a generic observation model.
- Add idempotent SQLite ingest, current-membership reduction, and a read API.
- Add a minimal Vite client for a selected source.
