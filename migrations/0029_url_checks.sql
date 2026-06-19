-- URL / HTTP(S) endpoint monitoring: per-node check configuration (1:1 with a node).
--
-- Additive (expand-contract, ADR-017): a new table only. A URL monitor is a node whose profile
-- category is `url-check`; this row carries what to probe. The scheduler dispatches an HTTP job
-- (and skips the always-on ICMP) for any node that has a row here. An N-1 binary that doesn't know
-- the table simply never reads it (those nodes just don't get polled until the new core runs), so
-- the rollout is safe both ways and no data is lost.
--
-- `expected_status` is a tagged JSONB document mirroring yagra_common::ExpectedStatus, e.g.
--   {"kind":"two_xx"} | {"kind":"exact","codes":[200,204]} | {"kind":"range","lo":200,"hi":399}.
-- `credential_id` is reserved for Basic/Bearer/Header auth (resolved/inlined by core, ADR-018/020);
-- unused by the MVP probe but stored so enabling auth later needs no schema change.
--
-- ON DELETE CASCADE on node_id: deleting the node removes its URL check. The credential FK is
-- ON DELETE SET NULL so removing a credential doesn't orphan/delete the monitor.

CREATE TABLE IF NOT EXISTS url_checks (
    node_id          UUID PRIMARY KEY REFERENCES nodes (id) ON DELETE CASCADE,
    url              TEXT NOT NULL,
    method           TEXT NOT NULL DEFAULT 'GET',
    expected_status  JSONB NOT NULL DEFAULT '{"kind":"two_xx"}'::jsonb,
    verify_tls       BOOLEAN NOT NULL DEFAULT true,
    follow_redirects BOOLEAN NOT NULL DEFAULT true,
    timeout_ms       INTEGER NOT NULL DEFAULT 5000
        CHECK (timeout_ms BETWEEN 100 AND 120000),
    credential_id    UUID REFERENCES credentials (id) ON DELETE SET NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
