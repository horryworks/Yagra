-- 0149_credential_try_order — the order Discovery tries stored credentials in (ADR-205 Inc.2).
--
-- reversible: one column is added, NOT NULL with a constant default, and every existing row is
-- numbered in the order it was created — which is the order the list was already returned in, so
-- nothing anyone sees moves. A core from before it neither reads nor writes the column; one that
-- creates a credential leaves it at 0, which lists it first until the order is next saved, and
-- loses nothing. No `schema_compat` floor, for the reason 0108 records: every release from 0.2.2 on
-- tolerates a database carrying migrations it does not embed.
--
-- WHY
-- The order credentials were tried in was the order they were created in, and nothing could change
-- it: putting the right community first meant deleting and re-creating the others.
ALTER TABLE credentials
    ADD COLUMN IF NOT EXISTS try_order INTEGER NOT NULL DEFAULT 0;

UPDATE credentials c
   SET try_order = numbered.n
  FROM (SELECT id, row_number() OVER (ORDER BY created_at, id) AS n FROM credentials) AS numbered
 WHERE c.id = numbered.id;
