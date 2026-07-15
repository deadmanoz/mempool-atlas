CREATE TABLE agent_state (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    source_id TEXT NOT NULL,
    last_rpc_success_at_ms INTEGER CHECK (last_rpc_success_at_ms >= 0)
) STRICT;

CREATE TABLE session_sequence (
    source_session_id TEXT PRIMARY KEY NOT NULL,
    next_sequence INTEGER NOT NULL CHECK (next_sequence >= 1)
) STRICT;

CREATE TABLE outbox_event (
    outbox_id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_session_id TEXT NOT NULL REFERENCES session_sequence(source_session_id),
    local_sequence INTEGER NOT NULL CHECK (local_sequence >= 1),
    event_json TEXT NOT NULL,
    nats_subject TEXT,
    raw_payload BLOB,
    delivery_attempts INTEGER NOT NULL DEFAULT 0 CHECK (delivery_attempts >= 0),
    last_delivery_error TEXT,
    UNIQUE (source_session_id, local_sequence)
) STRICT;

CREATE TABLE projected_membership (
    txid TEXT PRIMARY KEY NOT NULL
) STRICT;
