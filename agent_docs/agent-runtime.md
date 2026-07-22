# Atlas Agent Runtime

`atlas-agent` is the node-local durability boundary between peer-observer and Bitcoin RPC on one side, and central Atlas ingest on the other. One process instance and SQLite database represent exactly one configured source.

## Data flow

In `full` mode, the agent subscribes to peer-observer's Core NATS `mempool` and `netmsg` subjects. Each payload is an unframed protobuf `Event`. Supported evidence is normalized, assigned a per-process source session and a transactionally allocated sequence, then committed to the outbox before capture continues (`capture_loop`, `capture_message`, and `Outbox::enqueue_observation`).

All supported mempool-subject evidence is retained. P2P transaction observations are subject to `ATLAS_P2P_POLICY`: the default `inbound` policy retains observations explicitly marked inbound and suppresses other P2P sightings before persistence, while `all` retains the full P2P transaction relay stream. Inbound sightings preserve peer evidence from before the node admits or rejects a transaction. `all` is intended for bounded relay diagnostics. The policy reduces peer-level fan-out volume; it does not deduplicate transactions or narrow the underlying observation model.

Every queued membership mutation updates `projected_membership` in the same SQLite transaction. The projection therefore represents the effective source state after all locally accepted evidence, including events not yet delivered to Atlas. A missing transaction key means absent, a row with null fact columns means present while awaiting RPC, and an all-non-null fact bundle means present with verbose RPC state. Live additions create the pending form without erasing facts already known for the same uninterrupted membership; removals delete the row, preventing a later re-entry from inheriting stale facts.

Periodic `getrawmempool true` results are decoded as a transaction-ID-keyed map using only the common Core and Knots fields `vsize`, `time`, and `fees.base`. The base BTC amount is deserialized exactly and converted to integer satoshis; entry seconds are checked before conversion to milliseconds. Unknown fields are ignored, while a missing, malformed, overflowing, or non-exact JSON integer fact rejects that poll without changing the projection. Results are diffed against both membership and facts, then emit explicit tagged `mempool_reconciled` events in deterministic removal-then-upsert order (`Outbox::reconcile_rpc_snapshot`). This means a peer-observer admission is enriched on the next successful poll even though its transaction ID was already present. An unchanged full snapshot emits no events.

Capture and RPC use a shared asynchronous projection fence. Capture holds it while committing a supported observation. RPC holds it from immediately before requesting its snapshot until reconciliation commits. A one-shot initial guard lets a healthy RPC baseline commit before NATS consumption, but releases capture after the first failed RPC attempt. This prevents an older RPC result from overwriting newer captured evidence without keeping a SQLite transaction open across a network call (`runtime::run`, `capture_message`, and `rpc_loop`).

## Delivery contract

Outbox rows are ordered globally by their insertion ID, including rows from different process sessions. `Outbox::next_delivery` returns either the single FIFO head or a contiguous prefix containing only `mempool_reconciled` events. The prefix is capped at 512 events and an exact 4 MiB encoded request body. Encountering live or other non-reconciliation evidence ends the prefix, so nothing can overtake it.

Live and capture-gap events use `POST /api/v1/events`. Reconciliation prefixes use `POST /api/v1/events/batch`; the server validates one source and applies every event sequentially in one SQLite transaction. The agent deletes the prefix atomically only when HTTP 202 contains exactly one `applied` or `duplicate` acknowledgement per requested event, with the same IDs in the same order (`delivery::deliver_next`, `Store::ingest_batch`, and `Outbox::mark_delivered_prefix`). Network failures, non-202 responses, malformed, short, or reordered acknowledgements, and event conflicts retain the whole prefix and increment only the head's attempt count.

For a non-202 response, the stored diagnostic includes a safely escaped prefix of at most 1,024 response bytes and marks both capture and escaped-rendering truncation. A later body-read failure preserves any prefix already received. Transport failures, HTTP 408, 425, 429, and 5xx responses are transient; other non-202 statuses, serialization failures, and invalid acknowledgements require operator action. Transient retries use a deterministic equal-jitter exponential ceiling from `ATLAS_DELIVERY_RETRY_MILLISECONDS` to `ATLAS_DELIVERY_RETRY_MAX_MILLISECONDS`. Operator-action failures enter the maximum band immediately. The failure class changes only the delay: neither class may skip or reorder the FIFO head.

Delivery starts independently of NATS and RPC availability. Each input retries separately, so already-persisted events can drain during a node or peer-observer outage. If the process crashes or loses the response after central acceptance but before local deletion, the same head or prefix is sent again; central event ingest is idempotent by event ID and returns duplicate acknowledgements. Run matching agent and server event schemas in a controlled preproduction release.

Core NATS is not durable. If the bounded subscriber channel fills, `async-nats` drops the affected message and emits a slow-consumer event. The NATS callback sends rare control notices to the capture loop, which persists `slow_consumer` as known loss and `disconnected` as possible loss through the ordinary source-bound FIFO. Other NATS lifecycle events remain logs. Intentional P2P policy suppression is not a capture gap.

The central service reduces those events into monotonic source capture state. It records marker times and certainty, not a continuous outage interval or an estimated transaction-loss count. RPC can repair current membership but cannot reconstruct missing rejection, peer, timing, or raw-transaction evidence, so RPC reconciliation never clears or weakens this historical integrity state. A source with no gap marker means only that no gap has been reported, not that capture is proven complete.

Every 30 seconds, capture accounting logs cumulative total NATS messages, supported persisted observations, policy-suppressed P2P observations, ignored unsupported messages, invalid messages, and the current pending outbox depth. Policy suppression is intentional volume control and is distinct from a slow-consumer loss.

## Modes and configuration

| Environment variable                          |       Required | Default                 | Purpose                                                                              |
| --------------------------------------------- | -------------: | ----------------------- | ------------------------------------------------------------------------------------ |
| `ATLAS_SOURCE_ID`                             |            yes | none                    | Stable logical identity for this node source                                         |
| `ATLAS_AGENT_DATABASE`                        |             no | `var/atlas-agent.db`    | Node-local SQLite outbox and projection                                              |
| `ATLAS_SERVER_URL`                            |            yes | none                    | Base URL for central Atlas ingest                                                    |
| `ATLAS_AGENT_MODE`                            |             no | `full`                  | `full` or `rpc-only`                                                                 |
| `ATLAS_NATS_ADDRESS`                          | full mode only | `127.0.0.1:4222`        | Core NATS endpoint for peer-observer                                                 |
| `ATLAS_NATS_USERNAME` / `ATLAS_NATS_PASSWORD` |             no | none                    | Optional pair; configure both or neither                                             |
| `ATLAS_P2P_POLICY`                            |             no | `inbound`               | `inbound` for explicitly inbound P2P transactions or `all` for the full P2P transaction relay stream |
| `ATLAS_RPC_URL`                               |             no | `http://127.0.0.1:8332` | Bitcoin node RPC endpoint                                                            |
| `ATLAS_RPC_USERNAME` / `ATLAS_RPC_PASSWORD`   |            yes | none                    | Bitcoin RPC credentials                                                              |
| `ATLAS_RPC_POLL_SECONDS`                      |             no | `5`                     | Reconciliation interval                                                              |
| `ATLAS_DELIVERY_RETRY_MILLISECONDS`           |             no | `1000`                  | Initial transient delivery retry ceiling; also the NATS reconnect delay              |
| `ATLAS_DELIVERY_RETRY_MAX_MILLISECONDS`       |             no | `60000`                 | Maximum head-delivery retry ceiling; must be at least the initial value               |

`rpc-only` omits peer-observer capture and preserves RPC-authoritative current membership. It does not synthesize admission, rejection, peer, raw-transaction, or short-lived evidence that was never observed (`AgentMode::RpcOnly`). `ATLAS_P2P_POLICY` does not alter RPC-only operation because no NATS input is active.

## Persistent state and recovery

The schema stores the source binding, last successful RPC time, per-session sequence counters, pending events, delivery failure state, and effective membership plus its all-or-none fact bundle (`apps/atlas-agent/migrations/0001_initial.sql`). Raw NATS subject and payload bytes are retained with a row only while that event remains pending. Every agent-owned SQLite connection uses memory-backed temporary tables, indices, and statement journals, while the persistent database, WAL, and SHM files remain in the configured database directory. This prevents those runtime spills from requiring a writable operating-system temporary directory.

Use one writer and one persistent database per source. Opening an existing database with a different `ATLAS_SOURCE_ID` fails. Initialize it only through the backup-first wrapper:

```bash
just agent-db-migrate-dev
just agent-dev
```

Use `just agent-db-migrate-deploy` for a fresh deployment and `just agent-db-backup` for an explicit backup. Schema generation 3 rejects every stale generation. During this preproduction phase, replace a stale outbox with `just agent-db-reinitialize-deploy` only as part of the coordinated central and all-source reinitialization described in the architecture guide. Put source-specific settings in an untracked `.env`; never commit credentials or deployment inventory.

Deleting the database by hand is not a supported reset. It loses queued forensic evidence and the effective membership baseline, which can leave central source state without the removals needed to converge. Restore a verified backup after local loss. The backup-first preproduction reinitialization is safe only because central state and every source outbox are replaced together. A production source reset and full rebaseline protocol remains deferred.
