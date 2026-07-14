CREATE TABLE source (
    source_id TEXT PRIMARY KEY NOT NULL,
    latest_session_id TEXT NOT NULL,
    last_seen_at_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE event (
    event_id TEXT PRIMARY KEY NOT NULL,
    source_id TEXT NOT NULL REFERENCES source(source_id),
    source_session_id TEXT NOT NULL,
    local_sequence INTEGER NOT NULL,
    observed_at_ms INTEGER NOT NULL,
    received_at_ms INTEGER NOT NULL,
    event_kind TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    UNIQUE (source_id, source_session_id, local_sequence)
) STRICT;

CREATE INDEX event_source_observed_idx
    ON event (source_id, observed_at_ms DESC);

CREATE TABLE current_membership (
    source_id TEXT NOT NULL REFERENCES source(source_id),
    txid TEXT NOT NULL,
    present INTEGER NOT NULL CHECK (present IN (0, 1)),
    updated_at_ms INTEGER NOT NULL,
    evidence_event_id TEXT NOT NULL REFERENCES event(event_id),
    PRIMARY KEY (source_id, txid)
) STRICT;

CREATE INDEX current_membership_present_idx
    ON current_membership (source_id, present, txid);

CREATE TABLE transaction_variant (
    wtxid TEXT PRIMARY KEY NOT NULL,
    txid TEXT NOT NULL,
    raw_transaction BLOB,
    first_event_id TEXT NOT NULL REFERENCES event(event_id)
) STRICT;

CREATE INDEX transaction_variant_txid_idx
    ON transaction_variant (txid);
