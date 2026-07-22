CREATE TABLE agent_database (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    source_id TEXT NOT NULL
) STRICT;

CREATE TABLE outbox_state (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    last_rpc_success_at_ms INTEGER CHECK (last_rpc_success_at_ms >= 0)
) STRICT;

INSERT INTO outbox_state (singleton) VALUES (1);

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
    txid TEXT PRIMARY KEY NOT NULL,
    vsize INTEGER CHECK (vsize > 0),
    fee_sats INTEGER CHECK (fee_sats >= 0),
    entered_at_ms INTEGER CHECK (entered_at_ms >= 0),
    CHECK (
        (vsize IS NULL AND fee_sats IS NULL AND entered_at_ms IS NULL)
        OR
        (vsize IS NOT NULL AND fee_sats IS NOT NULL AND entered_at_ms IS NOT NULL)
    )
) STRICT;

-- SourceReplica is the RPC-authoritative, bounded replacement for carrying
-- current membership through the generic evidence outbox. Its epoch survives
-- ordinary process restarts, while an incompatible database reset creates a
-- new epoch.
CREATE TABLE source_replica_state (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    epoch_id TEXT NOT NULL,
    local_revision INTEGER NOT NULL DEFAULT 0 CHECK (local_revision >= 0),
    acknowledged_revision INTEGER NOT NULL DEFAULT 0
        CHECK (acknowledged_revision >= 0),
    has_acknowledged_cursor INTEGER NOT NULL DEFAULT 0
        CHECK (has_acknowledged_cursor IN (0, 1)),
    has_observed_snapshot INTEGER NOT NULL DEFAULT 0
        CHECK (has_observed_snapshot IN (0, 1)),
    checkpoint_required INTEGER NOT NULL DEFAULT 1
        CHECK (checkpoint_required IN (0, 1)),
    checkpoint_replacement_known INTEGER NOT NULL DEFAULT 0
        CHECK (checkpoint_replacement_known IN (0, 1)),
    checkpoint_supersedes_id TEXT CHECK (
        checkpoint_supersedes_id IS NULL
        OR length(trim(checkpoint_supersedes_id)) > 0
    ),
    checkpoint_replaces_epoch_id TEXT,
    checkpoint_replaces_revision INTEGER CHECK (checkpoint_replaces_revision >= 0),
    state_observed_at_ms INTEGER CHECK (state_observed_at_ms >= 0),
    acknowledged_state_observed_at_ms INTEGER
        CHECK (acknowledged_state_observed_at_ms >= 0),
    last_rpc_success_at_ms INTEGER CHECK (last_rpc_success_at_ms >= 0),
    CHECK (acknowledged_revision <= local_revision),
    CHECK (
        (has_acknowledged_cursor = 0
            AND acknowledged_revision = 0
            AND acknowledged_state_observed_at_ms IS NULL)
        OR
        (has_acknowledged_cursor = 1 AND acknowledged_revision >= 1)
    ),
    CHECK (
        (has_observed_snapshot = 0
            AND local_revision = 0
            AND state_observed_at_ms IS NULL)
        OR
        (has_observed_snapshot = 1
            AND local_revision >= 1
            AND state_observed_at_ms IS NOT NULL)
    ),
    CHECK (
        acknowledged_state_observed_at_ms IS NULL
        OR acknowledged_state_observed_at_ms <= state_observed_at_ms
    ),
    CHECK (
        checkpoint_replacement_known = 1
        OR (
            checkpoint_supersedes_id IS NULL
            AND checkpoint_replaces_epoch_id IS NULL
            AND checkpoint_replaces_revision IS NULL
        )
    ),
    CHECK (
        (checkpoint_replaces_epoch_id IS NULL AND checkpoint_replaces_revision IS NULL)
        OR
        (checkpoint_replaces_epoch_id IS NOT NULL AND checkpoint_replaces_revision IS NOT NULL)
    )
) STRICT;

CREATE TABLE source_replica_membership (
    txid TEXT PRIMARY KEY NOT NULL,
    vsize INTEGER NOT NULL CHECK (vsize > 0),
    fee_sats INTEGER NOT NULL CHECK (fee_sats >= 0),
    entered_at_ms INTEGER NOT NULL CHECK (entered_at_ms >= 0)
) STRICT, WITHOUT ROWID;

-- A dirty row is a coalesced marker, not a mutation log. Repeated changes to
-- one txid replace this row and therefore consume constant space.
CREATE TABLE source_replica_dirty (
    txid TEXT PRIMARY KEY NOT NULL,
    base_membership TEXT NOT NULL CHECK (base_membership IN ('absent', 'present')),
    base_vsize INTEGER CHECK (base_vsize > 0),
    base_fee_sats INTEGER CHECK (base_fee_sats >= 0),
    base_entered_at_ms INTEGER CHECK (base_entered_at_ms >= 0),
    dirty_revision INTEGER NOT NULL CHECK (dirty_revision >= 1),
    estimated_bytes INTEGER NOT NULL CHECK (estimated_bytes > 0),
    CHECK (
        (base_membership = 'absent'
            AND base_vsize IS NULL
            AND base_fee_sats IS NULL
            AND base_entered_at_ms IS NULL)
        OR
        (base_membership = 'present'
            AND base_vsize IS NOT NULL
            AND base_fee_sats IS NOT NULL
            AND base_entered_at_ms IS NOT NULL)
    )
) STRICT, WITHOUT ROWID;

-- A single metadata row freezes either one delta or one full checkpoint.
-- Frozen payload rows remain immutable until acknowledgement, so observing a
-- newer RPC snapshot cannot change a request that may already be in flight.
CREATE TABLE source_replica_frozen_action (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    action_kind TEXT NOT NULL CHECK (action_kind IN ('delta', 'checkpoint')),
    checkpoint_id TEXT,
    supersedes_checkpoint_id TEXT CHECK (
        supersedes_checkpoint_id IS NULL
        OR length(trim(supersedes_checkpoint_id)) > 0
    ),
    base_revision INTEGER CHECK (base_revision >= 0),
    replaces_epoch_id TEXT,
    replaces_revision INTEGER CHECK (replaces_revision >= 0),
    target_revision INTEGER NOT NULL CHECK (target_revision >= 1),
    state_observed_at_ms INTEGER NOT NULL CHECK (state_observed_at_ms >= 0),
    expected_entries INTEGER NOT NULL CHECK (expected_entries >= 0),
    checkpoint_chunk_entries INTEGER CHECK (checkpoint_chunk_entries > 0),
    content_sha256 TEXT NOT NULL,
    CHECK (
        (action_kind = 'delta'
            AND checkpoint_id IS NULL
            AND supersedes_checkpoint_id IS NULL
            AND base_revision IS NOT NULL
            AND replaces_epoch_id IS NULL
            AND replaces_revision IS NULL
            AND checkpoint_chunk_entries IS NULL)
        OR
        (action_kind = 'checkpoint'
            AND checkpoint_id IS NOT NULL
            AND base_revision IS NULL
            AND checkpoint_chunk_entries IS NOT NULL)
    ),
    CHECK (
        (replaces_epoch_id IS NULL AND replaces_revision IS NULL)
        OR
        (replaces_epoch_id IS NOT NULL AND replaces_revision IS NOT NULL)
    )
) STRICT;

CREATE TABLE source_replica_frozen_delta (
    txid TEXT PRIMARY KEY NOT NULL,
    membership TEXT NOT NULL CHECK (membership IN ('absent', 'present')),
    vsize INTEGER CHECK (vsize > 0),
    fee_sats INTEGER CHECK (fee_sats >= 0),
    entered_at_ms INTEGER CHECK (entered_at_ms >= 0),
    CHECK (
        (membership = 'absent'
            AND vsize IS NULL
            AND fee_sats IS NULL
            AND entered_at_ms IS NULL)
        OR
        (membership = 'present'
            AND vsize IS NOT NULL
            AND fee_sats IS NOT NULL
            AND entered_at_ms IS NOT NULL)
    )
) STRICT, WITHOUT ROWID;

CREATE TABLE source_replica_frozen_checkpoint (
    entry_index INTEGER PRIMARY KEY NOT NULL CHECK (entry_index >= 0),
    txid TEXT UNIQUE NOT NULL,
    vsize INTEGER NOT NULL CHECK (vsize > 0),
    fee_sats INTEGER NOT NULL CHECK (fee_sats >= 0),
    entered_at_ms INTEGER NOT NULL CHECK (entered_at_ms >= 0)
) STRICT, WITHOUT ROWID;
