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
    updated_at_ms INTEGER NOT NULL,
    evidence_event_id TEXT NOT NULL REFERENCES event(event_id),
    vsize INTEGER CHECK (vsize > 0),
    fee_sats INTEGER CHECK (fee_sats >= 0),
    entered_at_ms INTEGER CHECK (entered_at_ms >= 0),
    CHECK (
        (vsize IS NULL AND fee_sats IS NULL AND entered_at_ms IS NULL)
        OR
        (vsize IS NOT NULL AND fee_sats IS NOT NULL AND entered_at_ms IS NOT NULL)
    ),
    PRIMARY KEY (source_id, txid)
) STRICT;

CREATE TABLE transaction_variant (
    wtxid TEXT PRIMARY KEY NOT NULL,
    txid TEXT NOT NULL,
    raw_transaction BLOB,
    first_event_id TEXT NOT NULL REFERENCES event(event_id)
) STRICT;

CREATE INDEX transaction_variant_txid_idx
    ON transaction_variant (txid);

CREATE TABLE source_capture_state (
    source_id TEXT PRIMARY KEY NOT NULL REFERENCES source(source_id),
    first_gap_at_ms INTEGER NOT NULL CHECK (first_gap_at_ms >= 0),
    latest_gap_at_ms INTEGER NOT NULL CHECK (latest_gap_at_ms >= first_gap_at_ms),
    marker_count INTEGER NOT NULL CHECK (marker_count >= 1),
    strongest_certainty TEXT NOT NULL
        CHECK (strongest_certainty IN ('possible_loss', 'known_loss')),
    latest_input TEXT NOT NULL CHECK (length(trim(latest_input)) > 0),
    latest_reason TEXT NOT NULL CHECK (length(trim(latest_reason)) > 0),
    latest_evidence_event_id TEXT NOT NULL REFERENCES event(event_id)
) STRICT;
