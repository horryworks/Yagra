-- 0049_dns_checks — DNS name-resolution monitoring (ADR-033): per-node check configuration, the
-- current resolution chain, and an append-on-change history of that chain.
--
-- Additive only (ADR-017 expand-contract): three new tables, no change to any existing one. A DNS
-- monitor is a node whose profile category is `dns-check`; a `dns_checks` row is what makes the
-- scheduler dispatch a DNS job (and skip the always-on ICMP) for that node — the same discriminator
-- shape as `url_checks` (migration 0029). An N-1 binary never reads these tables, so a rollback just
-- leaves them orphaned: those nodes stop being polled, exactly the pre-0029 state of URL monitors.
-- No column is ever dropped or retyped, so there is no contract phase.
--
-- WHY THE CHAIN IS NOT IN THE TSDB
-- A resolution chain is `name → CNAME target → A/AAAA set`. SeriesKey is fixed at
-- {node, ifindex, metric} (ADR-011) with no API for extra labels, and a CNAME target is unbounded
-- free text — putting it in VictoriaMetrics would be the cardinality explosion CLAUDE.md §7.1 names
-- as the project's single biggest risk. Chains therefore live here, in the same tier as the
-- interface inventory and `nodes.vendor`/`model`: poller-returned structured strings → PostgreSQL.
-- Only the numeric summaries (dns_up / dns_resolve_ms / dns_chain_length / dns_answer_count) become
-- metrics.
--
-- WHY THREE TABLES
--   dns_checks         operator-owned configuration. The poll path never writes it, so a poll can
--                      never contend with a config edit.
--   dns_chains         poller-owned CURRENT state, 1:1 with the node — the `interfaces` pattern
--                      (upsert every observation, first_seen/last_seen). Answers "what does this
--                      name resolve to right now, and when did we last confirm it".
--   dns_chain_changes  append-only, written ONLY when `chain_key` differs from the previous
--                      observation. Answers "how did it change over time".
--
-- ABOUT `chain_key`
-- It is the canonical content encoding from yagra_common::DnsChain::content_key(): query + record
-- type + hop sequence + failure, with TTLs, resolve time and resolver DELIBERATELY EXCLUDED. TTLs
-- count down and round-robin A-records arrive rotated on every query, so keying on them would turn
-- this history into a per-poll log. It is stored verbatim rather than hashed so that "why was this
-- recorded as a change?" is answerable with a plain SELECT. The encoding is versioned (`v1` first
-- line); changing it re-keys every stored chain and emits one spurious change row per DNS node.
--
-- `prev_chain_key` on dns_chains exists so the upsert and the conditional append can be a single
-- atomic statement: a CTE whose RETURNING carries the pre-update key. PostgreSQL's row lock on
-- ON CONFLICT DO UPDATE serializes concurrent cores, so no transition is double-appended or lost.
--
-- Monitoring data only — no device credentials or secrets (security.md).

CREATE TABLE IF NOT EXISTS dns_checks (
    node_id       UUID PRIMARY KEY REFERENCES nodes (id) ON DELETE CASCADE,
    -- The name to resolve, stored normalized (lowercase, no trailing dot).
    name          TEXT NOT NULL,
    -- 'A' | 'AAAA' | 'CNAME' (yagra_common::DnsRecordType tokens).
    record_type   TEXT NOT NULL DEFAULT 'A',
    -- NULL ⇒ the poller container's system resolver.
    resolver_ip   INET,
    resolver_port INTEGER  NOT NULL DEFAULT 53   CHECK (resolver_port BETWEEN 1 AND 65535),
    max_depth     SMALLINT NOT NULL DEFAULT 8    CHECK (max_depth     BETWEEN 1 AND 16),
    -- Total budget for the whole chain walk, not per hop.
    timeout_ms    INTEGER  NOT NULL DEFAULT 3000 CHECK (timeout_ms    BETWEEN 100 AND 30000),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS dns_chains (
    node_id        UUID PRIMARY KEY REFERENCES nodes (id) ON DELETE CASCADE,
    chain_key      TEXT NOT NULL,
    -- The key this row held before the most recent observation; drives the conditional append.
    prev_chain_key TEXT,
    -- The full yagra_common::DnsChain as observed (hops, TTLs, failure, timing).
    chain          JSONB NOT NULL,
    resolved       BOOLEAN NOT NULL,
    -- yagra_common::DnsFailure::kind_token(), NULL when resolved. For grouping only — the
    -- discriminating detail (rcode / loop name / depth) stays in `chain`.
    failure_kind   TEXT,
    resolver       TEXT,
    resolve_ms     DOUBLE PRECISION,
    -- When this exact chain was first observed (reset whenever chain_key changes).
    first_seen     TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- When it was last confirmed (bumped on every observation, like interfaces.last_seen).
    last_seen      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS dns_chain_changes (
    -- BIGSERIAL rather than a random UUID: this is a pure append log, so a dense monotonic id gives
    -- a cheap stable keyset-cursor tiebreaker and a tighter index in a hot append path.
    id             BIGSERIAL PRIMARY KEY,
    node_id        UUID NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
    at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    chain_key      TEXT NOT NULL,
    -- NULL on the first observation for a node (the genesis row).
    prev_chain_key TEXT,
    chain          JSONB NOT NULL,
    resolved       BOOLEAN NOT NULL,
    failure_kind   TEXT,
    resolver       TEXT
);

-- The node-detail history view pages newest-first, keyset on (at DESC, id DESC).
CREATE INDEX IF NOT EXISTS dns_chain_changes_node_at_idx
    ON dns_chain_changes (node_id, at DESC, id DESC);
-- The retention pruner scans by age across all nodes.
CREATE INDEX IF NOT EXISTS dns_chain_changes_at_idx ON dns_chain_changes (at);
