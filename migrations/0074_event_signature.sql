-- Vendor event code extracted from a received syslog message (ADR-022 follow-up).
--
-- The Troubleshoot `rule_gap` analysis clusters unmatched events by a signature, which until now
-- was `COALESCE(trap_oid, app_name)`. A large share of real gear supplies neither: its timestamp
-- format falls outside both RFC 3164 and RFC 5424, so the parser degrades to the raw fallback and
-- extracts no APP-NAME, and it is syslog so there is no trap OID. Such a device can emit six
-- figures of events a day and produce zero rule-gap findings — which reads as "no monitoring gaps"
-- rather than "not measurable". The event class is in the message text all along
-- (`%%01URL/4/FILTER(l):…`, `%LINEPROTO-5-UPDOWN:`); this column is where core stores it, extracted
-- once at ingest by `yagra_ingest::extract_signature`.
--
-- Read precedence stays in the query (`events.rs::SIGNATURE_TIERS`), not in this column: the column
-- means one thing — "the vendor's own event code, found in the message" — so changing precedence
-- later is one edit rather than a rewrite of stored rows.
--
-- Additive (expand, ADR-017). An N-1 core names its columns explicitly in the events INSERT, so it
-- simply writes NULL here; the column is nullable and needs no default. There is deliberately **no
-- backfill**: `rule_gap` reads only unmatched rows, and those age out within one retention window
-- (PostgreSQL prunes them at `unmatched_event_hours`, default 24h), so the column populates itself.
--
-- No index. `rule_gap` reads a time-bounded, unmatched-only subset that
-- `events_event_time_idx` (migration 0055) already narrows before the GROUP BY.
--
-- The CHECK is a cardinality backstop, not cosmetic: this value also becomes a VictoriaLogs field,
-- and a pattern that captured something variable (a session id, a port) would turn one event class
-- into millions of series. `yagra_ingest::SIGNATURE_MAX_CHARS` enforces the same 64 upstream.

ALTER TABLE events
    ADD COLUMN signature TEXT
        CHECK (signature IS NULL OR length(signature) BETWEEN 1 AND 64);
