-- 0147_node_vlans — each switch port's mode and VLANs, one document per node (ADR-201).
--
-- reversible: additive only — one new table. An older core never names it, so rolling the binary
-- back leaves it in place and unread, and rolling forward again finds it. No `schema_compat`
-- floor, for the reason 0108 records: every release from 0.2.2 on tolerates a database carrying
-- migrations it does not embed.
--
-- WHY
-- The VLAN walk (Cisco, Huawei) and the Meraki port-name round report every port of a node at
-- once. The whole set is replaced per observation, exactly like `node_l3`, so a port whose VLANs
-- were removed stops showing them on the next walk. It deliberately does not live on `interfaces`:
-- that row has several writers and keeps any column a writer leaves NULL, so a removed VLAN would
-- never go away.
--
-- * `ports` is a `yagra_common::VlanSnapshot`, read back as JSON and joined to `interfaces` by
--   ifIndex when the Interfaces list is drawn.
-- * No change history: what an operator needs here is the current configuration, and the device's
--   own configuration archive is where its history lives.
CREATE TABLE IF NOT EXISTS node_vlans (
    node_id     UUID PRIMARY KEY REFERENCES nodes (id) ON DELETE CASCADE,
    ports       JSONB NOT NULL,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
