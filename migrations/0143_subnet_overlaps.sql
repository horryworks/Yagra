-- 0143_subnet_overlaps — what an operator has said about ranges two sites both use (ADR-187).
--
-- reversible: additive only — two new tables and one seeded row. An older core never names
-- either table, so rolling the binary back leaves them in place and unread, and rolling forward
-- again finds them. No `schema_compat` floor, for the reason 0108 records: every release from
-- 0.2.2 on tolerates a database carrying migrations it does not embed.
--
-- WHY
-- The overlaps themselves are computed from `node_l3` every time the screen is opened and are
-- never stored. What is stored is only what a person decided, because that is the one input the
-- stores cannot re-derive:
--
-- * `subnet_overlap_rules` — "places matching this are expected to repeat" (a WAN port, the
--   carrier's CGNAT range). A rule names a range, a word in the port's name or description, or
--   both; one naming neither would match everything and is refused by the CHECK as well as by
--   the API.
-- * `subnet_overlap_acks` — "this overlap is deliberate". Keyed by the overlap's key
--   (`same:<range>` / `nested:<outer range>`) and holding the sites it covered when it was
--   acknowledged, so a site joining it later reopens it.
--
-- The CGNAT row is built in: `builtin` rows can be switched off but not edited or deleted. Its id
-- is `api::subnet_overlaps::CGNAT_RULE_ID`, and the reason token is `ExclusionReason::Wan`.

CREATE TABLE IF NOT EXISTS subnet_overlap_rules (
    id          UUID PRIMARY KEY,
    range_cidr  CIDR NULL,
    port_text   TEXT NULL,
    reason      TEXT NOT NULL,
    note        TEXT NOT NULL DEFAULT '',
    enabled     BOOLEAN NOT NULL DEFAULT TRUE,
    builtin     BOOLEAN NOT NULL DEFAULT FALSE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT subnet_overlap_rules_names_something
        CHECK (range_cidr IS NOT NULL OR (port_text IS NOT NULL AND btrim(port_text) <> ''))
);

INSERT INTO subnet_overlap_rules (id, range_cidr, port_text, reason, note, enabled, builtin)
VALUES ('00000000-0000-0000-0000-0000000a0187', '100.64.0.0/10', NULL, 'wan',
        'CGNAT (RFC 6598): addresses a carrier hands out behind its own NAT', TRUE, TRUE)
ON CONFLICT (id) DO NOTHING;

CREATE TABLE IF NOT EXISTS subnet_overlap_acks (
    overlap_key  TEXT PRIMARY KEY,
    -- The sites the overlap spanned when acknowledged. The nil uuid stands for the tree root.
    site_ids     UUID[] NOT NULL,
    note         TEXT NOT NULL DEFAULT '',
    acked_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
