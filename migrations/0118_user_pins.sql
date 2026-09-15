-- 0118_user_pins — per-account pins on the inventory tree (ADR-146).
--
-- reversible: additive (one new table and its indexes; nothing narrowed, nothing rewritten in
-- place). No `schema_compat` floor is owed, for the reason 0083 and 0115 record: every release from
-- 0.2.2 on tolerates a database carrying migrations it does not embed, so this does not move the
-- oldest core that can start afterwards.
--
-- ⚠️ ROLLBACK BEHAVIOUR, STATED PLAINLY. An older core does not serve /api/v1/pins, so after a
-- downgrade the WebUI's GET answers 404 and the tree draws no pin controls and no "Pinned only"
-- button. The rows survive untouched and come back on the way up. Nothing about monitoring changes:
-- no poll, alert or rebuild reads this table.
--
-- WHY A TABLE AND NOT A KEY IN `user_preferences`
-- ADR-058's document is opaque to the backend, and its own rule is that anything the backend must
-- read does not belong in it. The backend reads pins: a pinned node usually sits in a folder the
-- tree has not loaded, so `GET /api/v1/pins` returns the pinned nodes themselves. The foreign keys
-- are the second reason — deleting a node, a folder or an account takes its pins with it, where an
-- id inside a JSON blob would outlive the node forever.
--
-- ONE ROW, ONE PIN, ONE TARGET
-- A pin names either a node or a folder, never both and never neither (the CHECK). The two partial
-- unique indexes make repeating a pin a no-op (`ON CONFLICT DO NOTHING`) rather than a duplicate.
--
-- WHY `node_id` AND `group_id` ARE INDEXED
-- Deleting a node runs one referential action per deleted row against every table whose foreign key
-- points at `nodes`; without an index each one reads this whole table. The bulk delete removes up to
-- 1000 nodes in one statement — the cost 0115 removed for two other tables. Partial for 0115's
-- reason: the action looks rows up by equality with the deleted id, so a NULL is never a match.
CREATE TABLE IF NOT EXISTS user_pins (
    user_id    UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    node_id    UUID REFERENCES nodes (id) ON DELETE CASCADE,
    group_id   UUID REFERENCES node_groups (id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT user_pins_one_target CHECK ((node_id IS NULL) <> (group_id IS NULL))
);

CREATE UNIQUE INDEX IF NOT EXISTS user_pins_user_node_uq
    ON user_pins (user_id, node_id) WHERE node_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS user_pins_user_group_uq
    ON user_pins (user_id, group_id) WHERE group_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS user_pins_node_idx
    ON user_pins (node_id) WHERE node_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS user_pins_group_idx
    ON user_pins (group_id) WHERE group_id IS NOT NULL;
