-- 0035_alert_acks — inbound acknowledgement state reflected from external tools (ADR-015, A1).
--
-- Additive / expand-only (ADR-017): one new table, nothing existing changes (N-1 compatible).
-- Yagra holds NO ack *action* of its own — acknowledgement/escalation/on-call live in the
-- external incident tool (PagerDuty / JSM). That tool calls the inbound webhook/REST endpoint
-- (`POST /api/v1/alerts/ack`) to MIRROR ack state into Yagra so the Active alerts and History
-- screens can show a read-only "acked" indicator. This is one-way inbound reflection only — Yagra
-- never writes ack back out to the external tool (no two-way sync, ADR-015).
--
-- Keyed by the alert dedup identity (node, check, severity) — the same key the engine dedups on
-- (yagra_alert::DedupKey) — so an ack tracks an incident, not a single history transition. Upsert
-- on the key (re-ack just refreshes who/when); a clear deletes the row.

CREATE TABLE IF NOT EXISTS alert_acks (
    node             UUID NOT NULL,
    check_id         UUID NOT NULL,
    severity         TEXT NOT NULL,            -- info | warning | critical (dedup key, ADR-015)
    acked_at_unix_ms BIGINT NOT NULL,          -- when the external tool recorded the ack (UTC ms)
    acked_by         TEXT NOT NULL,            -- external actor reference (id/handle) — never a secret
    source           TEXT NOT NULL,            -- originating tool: pagerduty | jsm | manual | …
    note             TEXT,                     -- optional free-text note from the external tool
    recorded_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (node, check_id, severity)
);
