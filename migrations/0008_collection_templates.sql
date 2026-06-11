-- 0008_collection_templates — reusable, named metric bundles referenced by profiles.
--
-- Additive (ADR-017). The design's middle layer: MIB → Collection templates → Device
-- profile *references*. A template bundles metrics (collection_template_items); a device
-- profile attaches templates (profile_collection_templates) and holds no raw OIDs itself.
-- The scheduler resolves a node's effective set from the templates linked to its profile,
-- plus the node's own ad-hoc overrides (collection_items, node scope). `collection_items`
-- is kept (now node-scope in practice).

CREATE TABLE collection_templates (
    id          UUID PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    description TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE collection_template_items (
    id          UUID PRIMARY KEY,
    template_id UUID NOT NULL REFERENCES collection_templates (id) ON DELETE CASCADE,
    metric_name TEXT NOT NULL,
    oid         TEXT NOT NULL,
    collection  TEXT NOT NULL,             -- scalar | table
    metric_kind TEXT NOT NULL,             -- gauge | counter
    enabled     BOOLEAN NOT NULL DEFAULT true,
    UNIQUE (template_id, metric_name)
);

-- Which templates a profile attaches (many-to-many). Cascade on either side so deleting a
-- profile or a template tidies its links.
CREATE TABLE profile_collection_templates (
    profile_id  UUID NOT NULL REFERENCES profiles (id) ON DELETE CASCADE,
    template_id UUID NOT NULL REFERENCES collection_templates (id) ON DELETE CASCADE,
    PRIMARY KEY (profile_id, template_id)
);

CREATE INDEX collection_template_items_tpl_idx ON collection_template_items (template_id);
CREATE INDEX profile_collection_templates_profile_idx ON profile_collection_templates (profile_id);
