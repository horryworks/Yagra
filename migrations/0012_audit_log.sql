-- Audit log (security.md: every state-changing API call records who did what).
--
-- One row per mutating /api/v1 request (method + path + response status) plus auth
-- events (login success/failure — username only, never a credential). Recorded by an
-- API-layer middleware so new endpoints are covered automatically. Append-only.

CREATE TABLE audit_log (
    id        UUID PRIMARY KEY,
    at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    username  TEXT NOT NULL,                -- '(unauthenticated)' when no valid session
    action    TEXT NOT NULL,                -- e.g. 'POST /api/v1/nodes' or 'auth.login'
    status    INTEGER NOT NULL              -- HTTP response status
);

CREATE INDEX audit_log_at_idx ON audit_log (at DESC);
