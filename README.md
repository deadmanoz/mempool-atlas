# Mempool Atlas

Mempool Atlas lets you explore one Bitcoin node's observed mempool. Its MVP includes server-side, extensible transaction classification. An optional Core and Knots comparison workspace supports the 2026 fork-monitoring work as a read-time derived view over independent source snapshots rather than the core product model.

The project treats five claims independently: a node received a transaction, admitted it, currently holds it, organically rejected it, or produced a classifier result. In particular, absence from one mempool is not labelled as rejection without supporting evidence.

## Initial architecture

```text
node + peer-observer + verbose RPC
              |
         atlas-agent
      SQLite durable outbox
  single live / bounded RPC batches
              |
         atlas-server
   source-labelled event ledger
  source-partitioned membership + facts
  raw variants -> shape + classifications
       capture integrity + rejections
              |
 source discovery + source-scoped JSON API
 membership inspector | aggregate summary
 rejection window     | comparison aggregates
              |
 aggregate single-source workbench (primary)
 Canvas membership inspector + optional comparison
```

RPC reconciliation is the authoritative current-membership plane. peer-observer adds low-latency admission, removal, replacement, rejection, peer, and raw-transaction evidence. On each node, `atlas-agent` subscribes to peer-observer's `mempool` and `netmsg` NATS subjects, persists supported observations in a source-bound SQLite outbox, and continuously reconciles its effective local projection with verbose `getrawmempool`. The agent retains the current common Core and Knots subset: virtual size, exact base fee in satoshis, and the node-reported mempool entry time. A peer-observer admission can be visible while those RPC facts are still pending.

Full mode preserves all supported mempool NATS evidence. P2P transaction capture defaults to explicitly inbound observations with `ATLAS_P2P_POLICY=inbound`, preserving peer evidence from before node admission or rejection; use `all` only for a bounded P2P transaction relay diagnostic. This reduces outbound relay fan-out before persistence. It is not transaction deduplication, and the observation model remains able to retain every raw peer sighting when requested. Periodic capture accounting reports persisted and policy-suppressed observations alongside the pending outbox depth.

Delivery is strict FIFO across capture sessions. Live and other non-reconciliation evidence uses the single-event endpoint. A contiguous prefix of `mempool_reconciled` events is delivered in an atomic batch of at most 512 events and 4 MiB, without coalescing or replacing their individual identities. The agent removes a single event or whole batch only after Atlas returns HTTP 202 with complete, same-order acknowledgements. Non-202 diagnostics retain a safely rendered prefix of at most 1,024 response bytes. Transport failures, HTTP 408, 425, 429, and 5xx responses use deterministic equal-jitter exponential backoff up to a configured maximum; other statuses and invalid acknowledgements enter the maximum retry band immediately. Both classes retain the unchanged FIFO head. Delivery keeps running while NATS or RPC is unavailable, and the two inputs retry independently. An explicit `rpc-only` mode provides the guaranteed operating floor when peer-observer is unavailable.

The central service co-locates data without merging node state. Events retain their source identity, current membership and capture integrity are maintained independently for each source, and the primary read returns exactly one source snapshot. The optional comparison workspace derives membership-set regions at read time from two to four source projections and returns only aggregates. A transaction present in one source and absent from another is a set difference, not proof of filtering, rejection, or relay causality. Atlas does not maintain a canonical combined mempool, and rejection evidence remains a separate per-source read model.

Current status: the live node-local capture, durable outbox, fact-bearing RPC reconciliation, source-scoped SQLite/API service, aggregate workbench, on-demand Canvas inspector, server-side shape and multi-taxonomy classification, bounded rejection surface, and optional read-time comparison workspace are implemented and component-tested. The registered packs include baseline behavior heuristics, BIP-110 conformance, and data-carrying-protocol fingerprints.

See the [system architecture visualisation](docs/mempool-atlas-system.html) for the implemented live data path, primary single-source aggregate experience, source-partitioned storage, classification and rejection reads, optional derived comparison, and the distinction between peer-observer evidence and RPC-authoritative membership.

## Development

Prerequisites are a current Rust toolchain, Node.js, npm, `protoc`, and `just`.

```bash
npm --prefix web install
just build
just test
just lint
just proto-check
```

Run `just test-baseline-scale` for the explicit release-mode acceptance test that reconciles and delivers a 200,000-transaction baseline.

Use `just seed-forks` to load the deterministic `knots`, `core`, and `libre-relay` comparison dataset through the real batch-ingest path. Its order is strictest to most permissive for the comparison UI, but the resulting membership-set differences are not evidence of rejection or relay causality.

Database changes must use the backup-first `just` targets. Fresh databases use `just db-migrate-dev` and `just agent-db-migrate-dev`. The central database is schema generation 6, including the partial rejection lookup index; the source-bound agent outbox remains generation 3. Both deliberately reject stale generations. For an existing preproduction deployment, stop Atlas and every agent, run `just db-reinitialize-deploy` centrally and `just agent-db-reinitialize-deploy` for every source, then restart matching binaries. Reinitialization preserves a verified backup before replacing each database; central state and all source outboxes must be replaced together.

Run the central API with `just dev`, and run the Vite client in a second terminal with `just web-dev`.

Select the browser source with `?source=<source-id>` or set `VITE_ATLAS_SOURCE_ID` when building the web client. The read API provides source discovery at `GET /api/v1/sources`, the full membership inspector at `GET /api/v1/sources/{source_id}/mempool`, the primary aggregate workbench at `GET /api/v1/sources/{source_id}/mempool/summary`, and the separate rejection window at `GET /api/v1/sources/{source_id}/rejections`. `GET /api/v1/sources/compare?sources=a,b,c` derives aggregate membership-set regions for two to four ordered sources without creating a combined mempool. The Canvas inspector plots membership in individual base-fee-rate and age bands, scales glyph area by virtual size, and shows entries awaiting RPC facts separately. It does not claim to represent ancestor-package mining priority.

For each node, put its source-specific settings in an untracked `.env`, then initialize and run the agent with:

```bash
just agent-db-migrate-dev
just agent-dev
```

`ATLAS_SOURCE_ID`, `ATLAS_SERVER_URL`, `ATLAS_RPC_USERNAME`, and `ATLAS_RPC_PASSWORD` are required. `ATLAS_AGENT_MODE` defaults to `full`; set it to `rpc-only` to omit NATS capture. In full mode, `ATLAS_P2P_POLICY` accepts `inbound` or `all` and defaults to `inbound`. The agent database defaults to `var/atlas-agent.db` and must live on persistent storage. Use one writer and one database per source, and do not delete the database as a recovery shortcut. A protocol for explicitly resetting central source state after agent database loss is deferred.

`just proto-check` expects the local peer-observer checkout at `../peer-observer`; set `PEER_OBSERVER_REPO` to override that location.

See `AGENTS.md` for repository conventions. Active implementation work is tracked in Beads.
