-- 0116_entity_rows — a vendor table's rows get names, an alert can be about one row, and a threshold
-- rule can pick rows by name (ADR-143).
--
-- reversible: additive only — one new table and three nullable columns, nothing narrowed and nothing
-- rewritten. An older core never names `entity_row_names`, and it projects and inserts `alert_history`
-- and `thresholds` rows through explicit column lists, so the new columns stay NULL on every row it
-- writes and are never read. Rolling the binary back leaves the values in place, unread: a rule
-- carrying a row pattern is then applied to every row and merges with an unpatterned rule at the same
-- scope by "most restrictive wins", which is exactly how the rules behaved before this migration.
-- Rolling forward again finds everything where it was. No `schema_compat` floor, for the reason 0108
-- records: every release from 0.2.2 on tolerates a database carrying migrations it does not embed.
--
-- WHY A TABLE OF NAMES AND NOT A TSDB LABEL
-- A row name is device-supplied text (`I/O`, `MPU Board 0`). ADR-011 keeps every series to two labels
-- (`node`, `ifindex`) because a free-text label is the cardinality explosion CLAUDE.md names, so the
-- name lives here and is joined by `(node_id, metric, row_key)` — the same pair the series carries.
--
-- WHY `metric` IS IN THE KEY
-- One table row carries several metrics (`cisco_mem_used` and `cisco_mem_free`), and core joins a name
-- to a value knowing only the metric name, never the OID. The rows stored are only the ones whose value
-- was not zero when the name was read, so a 306-entity Huawei stack stores its four boards, not 306.
--
-- WHY `row_key` IS BIGINT
-- The row key is a `u32` — the last OID sub-identifier, or a folded multi-part index — and a live Huawei
-- USG already answers with 3237192130. `INTEGER` would not hold it.

CREATE TABLE IF NOT EXISTS entity_row_names (
    node_id    UUID        NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    metric     TEXT        NOT NULL,
    row_key    BIGINT      NOT NULL,
    name       TEXT        NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (node_id, metric, row_key)
);

-- Which row an alert was about, and what that row was called when it fired. Descriptive, like
-- `ifindex` (0094): identity is the check id, which a one-way hash cannot give back.
ALTER TABLE alert_history ADD COLUMN IF NOT EXISTS row_key BIGINT;
ALTER TABLE alert_history ADD COLUMN IF NOT EXISTS row_name TEXT;

-- A rule's row-name pattern (`I/O`, `MPU Board *`). NULL means every row, which is every existing rule.
ALTER TABLE thresholds ADD COLUMN IF NOT EXISTS row_match TEXT;
