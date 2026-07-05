-- Cisco Meraki network-scope selector: which of an org's networks Yagra monitors.
--
-- Additive (expand-contract, ADR-017): a new table only. Populated at enumerate time with every
-- network the org has; `monitored` is the operator's per-network include flag (set in the import
-- wizard). The collector narrows `networkIds[]` API calls to monitored networks where the endpoint
-- supports it and bounds fan-out to them. Reconciliation adds newly-appeared networks with
-- monitored=false so they surface as candidates and are never auto-monitored (never-lose-data ethos).
--
-- ON DELETE CASCADE on org_id: removing the org drops its network scope. PK (org_id, network_id):
-- one row per network per org; re-enumerate upserts, preserving `monitored`.

CREATE TABLE IF NOT EXISTS meraki_org_networks (
    org_id       UUID NOT NULL REFERENCES meraki_orgs (id) ON DELETE CASCADE,
    network_id   TEXT NOT NULL,
    name         TEXT NOT NULL DEFAULT '',
    monitored    BOOLEAN NOT NULL DEFAULT false,
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (org_id, network_id)
);
