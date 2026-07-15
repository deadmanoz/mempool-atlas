# Mempool Atlas

Mempool Atlas observes and compares the mempools of multiple Bitcoin nodes. Its first deployment is a Core and Knots comparison for the 2026 fork-monitoring work, but the product model is intentionally generic.

The project treats five claims independently: a node received a transaction, admitted it, currently holds it, organically rejected it, or produced a classifier result. In particular, absence from one mempool is not labelled as rejection without supporting evidence.

## Initial architecture

```text
node + peer-observer + local RPC
              |
         atlas-agent
      SQLite durable outbox
  single live / bounded RPC batches
              |
         atlas-server
   source-labelled event ledger
  source-partitioned membership
          /         \
   JSON read API   atlas-web
```

RPC reconciliation is the authoritative current-membership plane. peer-observer adds low-latency admission, removal, replacement, rejection, peer, and raw-transaction evidence. On each node, `atlas-agent` subscribes to peer-observer's `mempool` and `netmsg` NATS subjects, persists supported observations in a source-bound SQLite outbox, and continuously reconciles its effective local projection with `getrawmempool`.

Full mode preserves all supported mempool NATS evidence. P2P transaction capture defaults to explicitly inbound observations with `ATLAS_P2P_POLICY=inbound`, preserving peer evidence from before node admission or rejection; use `all` only for a bounded P2P transaction relay diagnostic. This reduces outbound relay fan-out before persistence. It is not transaction deduplication, and the observation model remains able to retain every raw peer sighting when requested. Periodic capture accounting reports persisted and policy-suppressed observations alongside the pending outbox depth.

Delivery is strict FIFO across capture sessions. Live and other non-reconciliation evidence uses the single-event endpoint. A contiguous prefix of `mempool_reconciled` events is delivered in an atomic batch of at most 512 events and 4 MiB, without coalescing or replacing their individual identities. The agent removes a single event or whole batch only after Atlas returns HTTP 202 with complete, same-order acknowledgements. Delivery keeps running while NATS or RPC is unavailable, and the two inputs retry independently. An explicit `rpc-only` mode provides the guaranteed operating floor when peer-observer is unavailable.

The central service co-locates data without merging node state. Events retain their source identity, current membership is maintained independently for each source, and cross-source intersections or differences are derived only when requested. Atlas does not maintain a canonical combined mempool.

Current status: the live node-local capture, durable outbox, RPC reconciliation, SQLite/API service, and minimal browser table are implemented and component-tested. Comparison views and transaction classifiers remain the next product layer.

See the [system architecture visualisation](docs/mempool-atlas-system.html) for the implemented live data path, source-partitioned storage, planned comparison layer, and the distinction between peer-observer evidence and RPC-authoritative membership.

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

Database migrations must use the backup-first `just` targets. Run the central API with `just db-migrate-dev` followed by `just dev`, and run the Vite client in a second terminal with `just web-dev`.

For each node, put its source-specific settings in an untracked `.env`, then initialize and run the agent with:

```bash
just agent-db-migrate-dev
just agent-dev
```

`ATLAS_SOURCE_ID`, `ATLAS_SERVER_URL`, `ATLAS_RPC_USERNAME`, and `ATLAS_RPC_PASSWORD` are required. `ATLAS_AGENT_MODE` defaults to `full`; set it to `rpc-only` to omit NATS capture. In full mode, `ATLAS_P2P_POLICY` accepts `inbound` or `all` and defaults to `inbound`. The agent database defaults to `var/atlas-agent.db` and must live on persistent storage. Use one writer and one database per source, and do not delete the database as a recovery shortcut. A protocol for explicitly resetting central source state after agent database loss is deferred.

`just proto-check` expects the local peer-observer checkout at `../peer-observer`; set `PEER_OBSERVER_REPO` to override that location.

See `AGENTS.md` for repository conventions. Active implementation work is tracked in Beads.
