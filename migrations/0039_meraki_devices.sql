-- Cisco Meraki device binding: per-node config (1:1 with a node), mirroring url_checks (mig 0029).
--
-- Additive (expand-contract, ADR-017): a new table only. A Meraki device is a node whose profile
-- carries its real role (MX->firewall, MS->switch, MR->AP); the presence of THIS row is what marks
-- the node as Meraki-collected, so the scheduler emits no per-node ICMP/SNMP for it (the org
-- collector polls it) — exactly how a url_checks row makes a node HTTP-only. An N-1 binary that
-- doesn't know the table just never treats these as Meraki nodes.
--
-- `serial` is the join key returned by the org-bulk Dashboard endpoints and is globally unique per
-- device (UNIQUE => one node per serial, no cross-org collision). ON DELETE CASCADE on node_id:
-- deleting the node removes its binding; ON DELETE CASCADE on org_id: removing an org removes its
-- device bindings (the nodes themselves are handled by the org-removal flow in core).

CREATE TABLE IF NOT EXISTS meraki_devices (
    node_id      UUID PRIMARY KEY REFERENCES nodes (id) ON DELETE CASCADE,
    org_id       UUID NOT NULL REFERENCES meraki_orgs (id) ON DELETE CASCADE,
    serial       TEXT NOT NULL UNIQUE,
    network_id   TEXT NOT NULL,
    product_type TEXT NOT NULL,
    model        TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS meraki_devices_org_idx ON meraki_devices (org_id);
