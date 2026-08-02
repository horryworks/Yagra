-- 0060_user_scope — the visibility scope an account carries (ADR-014, Phase 2 issuance).
--
-- Expand-only (ADR-017): one nullable-free column with a default, so an N-1 binary that never
-- selects it keeps working and every existing account keeps the unrestricted visibility it had.
--
-- The value is a serialized `yagra_common::Scope`: `"All"` or `{"Groups":["<uuid>", …]}`. Entries
-- are `node_groups.id` UUIDs in canonical lowercase-hyphenated form, never group *names* — a name
-- is editable and not unique, so a rename would silently widen or void a scope. `Scope::group_uuids`
-- is where that rule is written down; this column is the one place it is stored.
--
-- Note the two states that must never collapse into one another: `"All"` is unrestricted, while
-- `{"Groups":[]}` sees **nothing**. A reader that turns a malformed value into `"All"` would make a
-- corrupt row a privilege escalation, so the parser fails closed instead.

ALTER TABLE users ADD COLUMN IF NOT EXISTS scope JSONB NOT NULL DEFAULT '"All"'::jsonb;
