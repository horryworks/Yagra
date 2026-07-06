-- 0042_event_monitoring — passive event pipeline (syslog / SNMP traps / inbound webhooks).
-- Additive only (ADR-017 expand-contract): new tables, no changes to existing ones — an
-- N-1 binary ignores them entirely.

-- Webhook ingest sources (one row per external sender; the bearer token authenticates
-- POST /api/v1/ingest/webhook/:id). `kind` allows syslog/trap for future per-source
-- config, but v1's API only creates 'webhook' rows. The token is stored as a SHA-256
-- hex digest — a bearer verifier never needs the plaintext back (shown once at
-- create/rotate), so hashing beats envelope encryption here.
CREATE TABLE IF NOT EXISTS event_sources (
    id          UUID PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 120),
    kind        TEXT NOT NULL CHECK (kind IN ('syslog', 'trap', 'webhook')),
    enabled     BOOLEAN NOT NULL DEFAULT true,
    -- Optional fixed node binding: events from this source correlate to this node.
    node_id     UUID REFERENCES nodes (id) ON DELETE SET NULL,
    token_hash  TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (kind <> 'webhook' OR token_hash IS NOT NULL)
);

-- Event match rules: substring/regex over the normalized event text. severity 'info'
-- records the match but never raises an alert (the alert engine has no Info state).
-- ttl_secs auto-closes the raised alert; clear_pattern resolves it early (e.g. link-up
-- clears link-down). min_count/window_secs is the edge-triggered analog of dwell:
-- fire only after N matches inside M seconds.
CREATE TABLE IF NOT EXISTS event_rules (
    id            UUID PRIMARY KEY,
    name          TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 120),
    enabled       BOOLEAN NOT NULL DEFAULT true,
    source_kind   TEXT CHECK (source_kind IN ('syslog', 'trap', 'webhook')),  -- NULL = any kind
    source_id     UUID REFERENCES event_sources (id) ON DELETE CASCADE,       -- NULL = any source
    node_id       UUID REFERENCES nodes (id) ON DELETE CASCADE,               -- NULL = global
    match_kind    TEXT NOT NULL CHECK (match_kind IN ('substring', 'regex')),
    pattern       TEXT NOT NULL CHECK (length(pattern) BETWEEN 1 AND 512),
    clear_pattern TEXT CHECK (length(clear_pattern) BETWEEN 1 AND 512),
    severity      TEXT NOT NULL CHECK (severity IN ('info', 'warning', 'critical')),
    ttl_secs      INTEGER NOT NULL DEFAULT 1800 CHECK (ttl_secs BETWEEN 60 AND 604800),
    min_count     INTEGER NOT NULL DEFAULT 1 CHECK (min_count BETWEEN 1 AND 100),
    window_secs   INTEGER NOT NULL DEFAULT 60 CHECK (window_secs BETWEEN 1 AND 3600),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS event_rules_enabled_idx ON event_rules (enabled);

-- Received events (metadata → PostgreSQL, never the TSDB). Everything is kept so
-- operators can author rules against what devices actually send; retention is
-- asymmetric — matched rows follow the alert-history window (90d), unmatched rows are
-- pruned at 24h (see the core retention task).
CREATE TABLE IF NOT EXISTS events (
    id              UUID PRIMARY KEY,
    kind            TEXT NOT NULL CHECK (kind IN ('syslog', 'trap', 'webhook')),
    at_unix_ms      BIGINT NOT NULL,
    recorded_at     TIMESTAMPTZ NOT NULL DEFAULT now(),   -- keyset paging cursor
    source_ip       INET,
    node_id         UUID REFERENCES nodes (id) ON DELETE SET NULL,
    source_id       UUID REFERENCES event_sources (id) ON DELETE SET NULL,
    pool            TEXT,
    facility        SMALLINT,
    syslog_severity SMALLINT,
    hostname        TEXT,
    app_name        TEXT,
    trap_oid        TEXT,
    varbinds        JSONB,
    message         TEXT NOT NULL CHECK (length(message) <= 4096),
    matched_rule_id UUID REFERENCES event_rules (id) ON DELETE SET NULL,
    -- What the pipeline did with it: none (no rule matched), fired / refreshed an alert,
    -- cleared one (clear-pattern), suppressed (maintenance window), info (record-only rule).
    action          TEXT NOT NULL DEFAULT 'none'
                    CHECK (action IN ('none', 'fired', 'refreshed', 'cleared', 'suppressed', 'info'))
);
CREATE INDEX IF NOT EXISTS events_recorded_idx ON events (recorded_at DESC);
CREATE INDEX IF NOT EXISTS events_node_idx ON events (node_id, recorded_at DESC);
CREATE INDEX IF NOT EXISTS events_rule_idx ON events (matched_rule_id, recorded_at DESC);
