# Architecture

The self-contained [system architecture visualisation](mempool-atlas-system.html) shows this design, including which components are implemented and which belong to the next live-capture iteration.

Mempool Atlas is divided into two Rust process boundaries and one browser client.

The implemented node-side library decodes peer-observer events and injects a configured source identity, source session, and local sequence. The implemented `atlas-server` ingests those normalized events idempotently, reduces current membership into SQLite, and exposes read-only browser data. The browser currently fetches a membership checkpoint, and its isolated state reducer already rejects sequence gaps in preparation for streamed deltas.

Central storage is shared operationally, not semantically. Every event remains labelled with its `source_id`, and `current_membership` is keyed by `(source_id, txid)`, so the same transaction in Core and Knots produces two independent membership records. Transaction variants can be deduplicated by `wtxid` because the serialized transaction is an intrinsic fact rather than source state.

There is no canonical Atlas mempool. Shared, source-only, and divergent sets are derived comparison views over a selected group of source projections. A short-lived fork therefore does not require Atlas to manufacture or persist mixed membership state.

The live `atlas-agent` process boundary owns NATS subscription, RPC reconciliation, and a durable SQLite outbox. Those runtime pieces are intentionally not present in the first recorded-event vertical slice.

RPC reconciliation is authoritative for present membership. peer-observer provides event timing and forensic evidence that polling cannot recover, including organic rejections and short-lived transactions. A source can therefore operate in an explicit RPC-only mode without pretending that its evidence is complete.

The shared model never hard-codes Core, Knots, fork heights, or deployment hostnames. Fork-specific source profiles and optional classifiers are configuration layered on top of the generic observation model.
