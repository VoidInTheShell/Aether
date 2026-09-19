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

CREATE TABLE IF NOT EXISTS codex_turn_state_account_verdicts (
    key_id                      TEXT        PRIMARY KEY,
    verdict                     TEXT        NOT NULL DEFAULT 'normal',
    consecutive_degraded_rounds INTEGER     NOT NULL DEFAULT 0,
    degraded_models             JSONB       NOT NULL DEFAULT '[]',
    last_probe_at               TIMESTAMPTZ,
    degraded_since              TIMESTAMPTZ,
    updated_at                  TIMESTAMPTZ NOT NULL
);

CREATE TABLE IF NOT EXISTS codex_turn_state_counters (
    id       SMALLINT    PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    harvest  BIGINT      NOT NULL DEFAULT 0,
    substitute BIGINT    NOT NULL DEFAULT 0,
    inject   BIGINT      NOT NULL DEFAULT 0,
    pass     BIGINT      NOT NULL DEFAULT 0,
    skip     BIGINT      NOT NULL DEFAULT 0,
    since    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS codex_turn_state_probe_run (
    id          SMALLINT    PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    running     BOOLEAN     NOT NULL DEFAULT false,
    started_at  TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    total       INTEGER     NOT NULL DEFAULT 0,
    done        INTEGER     NOT NULL DEFAULT 0,
    lines       JSONB       NOT NULL DEFAULT '[]'
);
