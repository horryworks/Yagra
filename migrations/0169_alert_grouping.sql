-- 0169_alert_grouping — group an outage's alerts at the moment it happens (ADR-236).
--
-- Three things, all additive:
--   * how strongly the deployment groups (0 = never, the default; 1 certain only; 2 from links;
--     3 whole site), on the one settings row.
--   * a folder's own level, which beats the deployment's for every node beneath it. The nearest
--     folder wins. A deleted folder takes its row with it.
--   * the nodes the last topology derivation could not be sure about: each reported a CDP/LLDP row
--     that named no node, so it may have a link nobody can see. Level 1 never rolls one of them up.
--
-- reversible: additive only — one column on `app_settings`, one on `topology_derivation` and one new
-- table. An older core never names any of them; rolling the binary back brings back the parent
-- graph it reads from `nodes.parent_id`, which this version stops reading but does not touch. No
-- `schema_compat` floor, for the reason 0108 records.
ALTER TABLE app_settings
    ADD COLUMN IF NOT EXISTS alert_grouping_level SMALLINT NOT NULL DEFAULT 0
        CHECK (alert_grouping_level BETWEEN 0 AND 3);

CREATE TABLE IF NOT EXISTS alert_grouping_overrides (
    group_id   UUID PRIMARY KEY REFERENCES node_groups (id) ON DELETE CASCADE,
    level      SMALLINT NOT NULL CHECK (level BETWEEN 0 AND 3),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE topology_derivation
    ADD COLUMN IF NOT EXISTS unsure_nodes UUID[] NOT NULL DEFAULT '{}';
