-- URL monitoring: response-body inspection — keyword matching and JSON extraction
-- (ADR-047 Increments 2 and 3).
--
-- Additive (expand-contract, ADR-017): three nullable/defaulted columns, no rewrite of existing
-- rows, no destructive step. Every existing URL monitor gets `body_match = NULL` and
-- `json_extract = NULL`, which mean "no content rule" and "extract nothing" — and a monitor with
-- neither never reads the response body at all, so nothing about how they are polled changes.
--
-- N/N-1 in both directions:
--   * an N-1 core never SELECTs these columns, so it keeps polling every monitor exactly as before;
--   * an N core reading a row written by an N-1 core sees the defaults, i.e. no rules.
-- Downgrading therefore loses the *rules* (the columns stay, unread) rather than breaking the
-- monitor — the same shape as `credential_id`, which shipped unused for the same reason.
--
-- JSONB rather than flat columns, mirroring `expected_status` in this same table: each is one
-- operator-edited document, and adding a knob later is then a serde field rather than a migration.
--   body_match:   {"pattern":"\"status\":\"ok\"","mode":"contains"}
--                 `mode` is one of `contains` / `not_contains`.
--   json_extract: [{"metric":"queue_depth","path":"data.queue.depth"}, …]
--                 `path` is dot-separated; array elements are indexed by number.
--
-- `body_max_bytes` is a real column with a real CHECK because it is a scalar bound, exactly like
-- `timeout_ms` above it — the precedent in this table. It is deliberately a property of the
-- *monitor*, not of either rule: there is one body and one read, so two budgets could disagree.
--
-- No CHECK on the JSONB *contents*, though. The Rust type is the guard (the API edge validates
-- pattern length, path length, metric-name grammar and the reserved-name collision before any
-- write), and a DB-level copy of those bounds would be a second source drifting from the first —
-- the same call `topology_links.sources` made. What is worth pinning here is only the document
-- shape, which is what an application bug would get wrong in a way that survives a round trip.

ALTER TABLE url_checks
    ADD COLUMN IF NOT EXISTS body_match JSONB,
    ADD COLUMN IF NOT EXISTS json_extract JSONB,
    ADD COLUMN IF NOT EXISTS body_max_bytes INTEGER NOT NULL DEFAULT 65536;

ALTER TABLE url_checks
    DROP CONSTRAINT IF EXISTS url_checks_body_match_is_object;
ALTER TABLE url_checks
    ADD CONSTRAINT url_checks_body_match_is_object
    CHECK (body_match IS NULL OR jsonb_typeof(body_match) = 'object');

ALTER TABLE url_checks
    DROP CONSTRAINT IF EXISTS url_checks_json_extract_is_array;
ALTER TABLE url_checks
    ADD CONSTRAINT url_checks_json_extract_is_array
    CHECK (json_extract IS NULL OR jsonb_typeof(json_extract) = 'array');

ALTER TABLE url_checks
    DROP CONSTRAINT IF EXISTS url_checks_body_max_bytes_range;
ALTER TABLE url_checks
    ADD CONSTRAINT url_checks_body_max_bytes_range
    CHECK (body_max_bytes BETWEEN 1024 AND 1048576);
