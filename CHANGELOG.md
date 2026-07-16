# Changelog

All notable changes to Mempool Atlas will be documented in this file.

## [Unreleased]

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
