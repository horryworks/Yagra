-- 0075_alert_subject — an alert's subject is no longer always a node (ADR-009 Increment 2).
--
-- Increment 1 gave the alert engine a `Subject` sum type so it could raise an alert about a poller
-- *pool* that has nodes and no live poller — a failure no `PollResult` can ever report, because the
-- symptom is that no result arrives. Those alerts were delivered over the notification channels and
-- deliberately kept off every stored and served surface, because `alert_history` and `alert_acks`
-- are keyed by node id. This is the expand that lets them be stored, listed and acknowledged.
--
-- Additive / expand-only (ADR-017): two nullable-or-defaulted columns per table, no rewrite (a
-- `NOT NULL DEFAULT` on `TEXT` is metadata-only since PostgreSQL 11), nothing existing changes.
--
-- ## Why `node` stays NOT NULL and holds a derived id
--
-- The obvious shape is `node UUID NULL` plus a kind. It was rejected for one reason: both readers
-- decode that column into a bare, non-Option `Uuid` (`history.rs::recent`, `top_nodes_by_fires`),
-- so a single NULL row fails the decode for the **whole page** — an N-1 core rolled back onto this
-- data would answer 500 on the History screen for every row, not just the new one, and ADR-017
-- treats alert history as data a rollback must not break. Dropping the NULL would then need the
-- read relaxation shipped a release ahead of the write, and `alert_acks.node` is a primary-key
-- column besides, so it would need its key rebuilt.
--
-- Instead a non-node subject is stored under a stable UUIDv5 of its flat form
-- (`yagra_alert::Subject::storage_id`). A node's id is unchanged — every existing row and every
-- open acknowledgement keeps its identity byte-for-byte — and a rolled-back core sees a pool row as
-- an ordinary row whose node it cannot resolve. Degraded, not broken.
--
-- ⚠️ The column therefore means **subject identity**, not "a node". `subject_kind` is what says
-- whether it names one; every read that means a node filters on that column (see
-- `top_nodes_by_fires` and `SCOPE_PREDICATE`) rather than trusting the id to resolve.
--
-- ## No CHECK on subject_kind, deliberately
--
-- Same reasoning as `topology_links.sources` (ADR-043): the Rust enum is the guard, and a CHECK
-- would turn "a newer core wrote a kind this schema has not heard of" from a row an older core
-- degrades into a write that fails outright. `SubjectKind::from_token` returns None for an unknown
-- token and the reader drops that row, which is the direction that keeps the page serving.

ALTER TABLE alert_history
    ADD COLUMN IF NOT EXISTS subject_kind TEXT NOT NULL DEFAULT 'node',  -- node | pool
    ADD COLUMN IF NOT EXISTS subject_ref  TEXT;                          -- the pool's name; NULL for a node

ALTER TABLE alert_acks
    ADD COLUMN IF NOT EXISTS subject_kind TEXT NOT NULL DEFAULT 'node',
    ADD COLUMN IF NOT EXISTS subject_ref  TEXT;
