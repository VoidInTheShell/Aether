CREATE TABLE IF NOT EXISTS codex_turn_state_buckets (
    key_id          TEXT        NOT NULL,
    model           TEXT        NOT NULL,
    encrypted_value BYTEA       NOT NULL,
    value_len       INTEGER     NOT NULL,
    issued_at       TIMESTAMPTZ NOT NULL,
    harvested_at    TIMESTAMPTZ NOT NULL,
    expires_at      TIMESTAMPTZ NOT NULL,
    source          TEXT        NOT NULL,
    last_exit       TEXT        NOT NULL DEFAULT '',
    PRIMARY KEY (key_id, model),
    CONSTRAINT codex_turn_state_bucket_source_check CHECK (source IN ('probe', 'passive'))
);

CREATE INDEX IF NOT EXISTS idx_codex_turn_state_buckets_expires_at
    ON codex_turn_state_buckets (expires_at);
