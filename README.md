# Mempool Atlas

Mempool Atlas observes and compares the mempools of multiple Bitcoin nodes. Its first deployment is a Core and Knots comparison for the 2026 fork-monitoring work, but the product model is intentionally generic.

The project treats five claims independently: a node received a transaction, admitted it, currently holds it, organically rejected it, or produced a classifier result. In particular, absence from one mempool is not labelled as rejection without supporting evidence.

## Initial architecture

```text
node + peer-observer + local RPC
              |
         atlas-agent
      SQLite durable outbox
              |
         atlas-server
       SQLite current state
          /         \
   JSON read API   atlas-web
```

RPC reconciliation is the authoritative current-membership plane. peer-observer adds low-latency admission, removal, replacement, rejection, peer, and raw-transaction evidence. The first vertical slice proves a recorded peer-observer event through normalization, idempotent SQLite ingest, and the read API before live capture or renderer work expands.

Current status: the recorded-event vertical slice, SQLite/API service, and minimal browser table are working and tested. Live NATS subscription, RPC polling, and the edge outbox are not wired yet.

See the [system architecture visualisation](docs/mempool-atlas-system.html) for the implemented foundation, planned live data path, and the distinction between peer-observer evidence and RPC-authoritative membership.

## Development

Prerequisites are a current Rust toolchain, Node.js, npm, `protoc`, and `just`.

```bash
npm --prefix web install
just build
just test
just lint
just proto-check
```

Database migrations must use `just db-migrate-dev` or `just db-migrate-deploy`; both call the backup-first migration wrapper. Run the API with `just dev` and the Vite client in a second terminal with `just web-dev`.

`just proto-check` expects the local peer-observer checkout at `../peer-observer`; set `PEER_OBSERVER_REPO` to override that location.

See `AGENTS.md` for repository conventions. Active implementation work is tracked in Beads.
