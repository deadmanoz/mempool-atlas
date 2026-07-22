-- RPC-authoritative source state. A source can own at most one reader-visible
-- generation and one checkpoint being staged. The two partial unique indexes
-- below make that storage bound a database invariant rather than an
-- application convention.
CREATE TABLE source_replica_generation (
    source_id TEXT NOT NULL,
    generation_id INTEGER NOT NULL CHECK (generation_id > 0),
    role TEXT NOT NULL CHECK (role IN ('active', 'staging')),
    epoch_id TEXT NOT NULL CHECK (length(trim(epoch_id)) > 0),
    revision INTEGER NOT NULL CHECK (revision > 0),
    state_observed_at_ms INTEGER NOT NULL CHECK (state_observed_at_ms >= 0),
    checkpoint_id TEXT NOT NULL CHECK (length(trim(checkpoint_id)) > 0),
    supersedes_checkpoint_id TEXT CHECK (
        supersedes_checkpoint_id IS NULL
        OR length(trim(supersedes_checkpoint_id)) > 0
    ),
    replaces_epoch_id TEXT,
    replaces_revision INTEGER CHECK (replaces_revision > 0),
    expected_entries INTEGER NOT NULL CHECK (
        expected_entries BETWEEN 0 AND 1000000
    ),
    expected_chunks INTEGER NOT NULL CHECK (
        expected_chunks BETWEEN 0 AND 4096
    ),
    content_sha256 TEXT NOT NULL CHECK (
        length(content_sha256) = 64
        AND content_sha256 = lower(content_sha256)
        AND content_sha256 NOT GLOB '*[^0-9a-f]*'
    ),
    last_delta_target_revision INTEGER CHECK (last_delta_target_revision > 0),
    last_delta_content_sha256 TEXT CHECK (
        last_delta_content_sha256 IS NULL
        OR (
            length(last_delta_content_sha256) = 64
            AND last_delta_content_sha256 = lower(last_delta_content_sha256)
            AND last_delta_content_sha256 NOT GLOB '*[^0-9a-f]*'
        )
    ),
    CHECK (
        (replaces_epoch_id IS NULL AND replaces_revision IS NULL)
        OR
        (replaces_epoch_id IS NOT NULL AND replaces_revision IS NOT NULL)
    ),
    CHECK (
        (last_delta_target_revision IS NULL AND last_delta_content_sha256 IS NULL)
        OR
        (last_delta_target_revision IS NOT NULL AND last_delta_content_sha256 IS NOT NULL)
    ),
    CHECK (
        (expected_entries = 0 AND expected_chunks = 0)
        OR
        (expected_entries > 0 AND expected_chunks > 0
         AND expected_chunks <= expected_entries)
    ),
    PRIMARY KEY (source_id, generation_id)
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX source_replica_one_active_generation
    ON source_replica_generation (source_id)
    WHERE role = 'active';

CREATE UNIQUE INDEX source_replica_one_staging_generation
    ON source_replica_generation (source_id)
    WHERE role = 'staging';

CREATE UNIQUE INDEX source_replica_checkpoint_id
    ON source_replica_generation (source_id, checkpoint_id);

CREATE TABLE source_replica_membership (
    source_id TEXT NOT NULL,
    generation_id INTEGER NOT NULL,
    txid TEXT NOT NULL,
    vsize INTEGER NOT NULL CHECK (vsize > 0),
    fee_sats INTEGER NOT NULL CHECK (fee_sats >= 0),
    entered_at_ms INTEGER NOT NULL CHECK (entered_at_ms >= 0),
    PRIMARY KEY (source_id, generation_id, txid),
    FOREIGN KEY (source_id, generation_id)
        REFERENCES source_replica_generation (source_id, generation_id)
        ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

CREATE TABLE source_replica_checkpoint_chunk (
    source_id TEXT NOT NULL,
    generation_id INTEGER NOT NULL,
    chunk_index INTEGER NOT NULL CHECK (chunk_index BETWEEN 0 AND 4095),
    entry_count INTEGER NOT NULL CHECK (entry_count BETWEEN 1 AND 512),
    content_sha256 TEXT NOT NULL CHECK (
        length(content_sha256) = 64
        AND content_sha256 = lower(content_sha256)
        AND content_sha256 NOT GLOB '*[^0-9a-f]*'
    ),
    PRIMARY KEY (source_id, generation_id, chunk_index),
    FOREIGN KEY (source_id, generation_id)
        REFERENCES source_replica_generation (source_id, generation_id)
        ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

-- Honest read boundaries for the later product-read cutover. These views
-- expose only the complete reader-visible generation and never fabricate
-- event provenance or capture completeness for RPC state.
CREATE VIEW active_source_replica AS
SELECT source_id, generation_id, epoch_id, revision, state_observed_at_ms,
       checkpoint_id, expected_entries, expected_chunks, content_sha256
FROM source_replica_generation
WHERE role = 'active';

CREATE VIEW active_source_replica_membership AS
SELECT generation.source_id, generation.generation_id, generation.epoch_id,
       generation.revision, generation.state_observed_at_ms,
       membership.txid, membership.vsize, membership.fee_sats,
       membership.entered_at_ms
FROM source_replica_generation AS generation
JOIN source_replica_membership AS membership
  ON membership.source_id = generation.source_id
 AND membership.generation_id = generation.generation_id
WHERE generation.role = 'active';
