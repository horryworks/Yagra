-- 0146_prefix_gap_acks — which missing subnets an operator marked as intentional (ADR-170 Inc.4).
--
-- reversible: additive only — one new table. An older core never names it, so rolling the binary
-- back leaves it in place and unread, and rolling forward again finds it. No `schema_compat`
-- floor, for the reason 0108 records: every release from 0.2.2 on tolerates a database carrying
-- migrations it does not embed.
--
-- WHY
-- The gaps themselves are computed from `node_l3` and the folders' IP prefixes every time Nodes ▸
-- Missing IP prefixes is opened, and are never stored. What is stored is only what a person
-- decided: "this subnet is meant to stay out of this site's IP prefixes".
--
-- * `site_id` is the site's folder; the nil uuid stands for the tree root (devices filed in no
--   folder), the convention `subnet_overlap_acks.site_ids` already uses.
-- * `subnet` is the gap's `network/length` exactly as the API wrote it, compared as text.
-- * `kind` is the gap's kind when it was marked. A gap whose kind has since changed is open
--   again: what was judged deliberate is no longer what the screen shows.
CREATE TABLE IF NOT EXISTS prefix_gap_acks (
    site_id   UUID NOT NULL,
    subnet    TEXT NOT NULL,
    kind      TEXT NOT NULL,
    note      TEXT NOT NULL DEFAULT '',
    acked_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (site_id, subnet)
);
