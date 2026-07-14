# Architecture

Mempool Atlas is divided into two Rust process boundaries and one browser client.

The implemented node-side library decodes peer-observer events and injects a configured source identity, source session, and local sequence. The implemented `atlas-server` ingests those normalized events idempotently, reduces current membership into SQLite, and exposes read-only browser data. The browser currently fetches a membership checkpoint, and its isolated state reducer already rejects sequence gaps in preparation for streamed deltas.

The live `atlas-agent` process boundary owns NATS subscription, RPC reconciliation, and a durable SQLite outbox. Those runtime pieces are intentionally not present in the first recorded-event vertical slice.

RPC reconciliation is authoritative for present membership. peer-observer provides event timing and forensic evidence that polling cannot recover, including organic rejections and short-lived transactions. A source can therefore operate in an explicit RPC-only mode without pretending that its evidence is complete.

The shared model never hard-codes Core, Knots, fork heights, or deployment hostnames. Fork-specific source profiles and optional classifiers are configuration layered on top of the generic observation model.
