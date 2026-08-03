// SPDX-License-Identifier: AGPL-3.0-only
//! Assemble walked LLDP/CDP rows into a normalized [`NeighborSet`] (ADR-038).
//!
//! Split out of `worker.rs` and kept **pure** — it takes already-walked [`SnmpInstanceRow`]s and
//! returns a set — because this is where the feature is won or lost and every rule in it deserves a
//! test that runs without a device. The two things it must get right:
//!
//! * **Bucketing by index.** `lldpRemTable` is indexed by
//!   `(lldpRemTimeMark, lldpRemLocalPortNum, lldpRemIndex)`, and rows for one adjacency arrive
//!   spread across eight separate column walks. They are joined back on the whole index tuple,
//!   then the tuple is **thrown away**: `timeMark` moves on every agent refresh and `remIndex` is
//!   renumbered at will, so carrying either into the record would make every poll look like a
//!   rewiring. What survives is `(local port, remote chassis, remote port)`.
//! * **Not decoding octets as text.** A chassis id's meaning is in its subtype column, which is a
//!   *different row of a different column walk*. The rendering therefore has to happen here, after
//!   the join — which is exactly why the transport had to stop coercing values on the way in.

use std::collections::BTreeMap;
use yagra_bus::SnmpNeighborColumn;
use yagra_common::{
    cdp_capabilities, lldp_capabilities, render_bare_address, render_chassis_id, render_port_id,
    render_text, Neighbor, NeighborColumn, NeighborProto, NeighborSet,
};
use yagra_transport::{SnmpInstanceRow, SnmpValue};

/// The columns of one table row, keyed by which field they carry.
type Row<'a> = BTreeMap<NeighborColumn, &'a SnmpValue>;

/// Build the node's neighbour set from every walked row.
///
/// `columns` is the job's declared column list; rows whose base is not in it are ignored, so a
/// poller that receives a column it has no handling for degrades to "that field is absent" rather
/// than mis-attributing values.
#[must_use]
pub fn assemble(columns: &[SnmpNeighborColumn], rows: &[SnmpInstanceRow]) -> NeighborSet {
    let field_by_base: BTreeMap<&str, NeighborColumn> =
        columns.iter().map(|c| (c.oid.as_str(), c.field)).collect();

    // Join every column's rows back together on the full instance index. `BTreeMap` (not `Hash`)
    // so the intermediate ordering is deterministic — useful when the cap trims the set and when a
    // failing test has to be read.
    let mut by_instance: BTreeMap<Vec<u32>, Row<'_>> = BTreeMap::new();
    for row in rows {
        let Some(field) = field_by_base.get(row.oid_base.as_str()).copied() else {
            continue;
        };
        by_instance
            .entry(row.instance.clone())
            .or_default()
            .insert(field, &row.value);
    }

    // `lldpLocPortTable` and `cdpInterfaceName` are *naming* tables, not adjacency tables: each maps
    // a local port number to a name, and each is indexed by that number alone — so an adjacency row
    // cannot read its own local port's name out of its own entry. Lift both out first.
    let lldp_ports = lldp_local_port_names(&by_instance);
    let cdp_ports = cdp_interface_names(&by_instance);

    let mut neighbors = Vec::new();
    for (instance, row) in &by_instance {
        if let Some(n) = lldp_neighbor(instance, row, &lldp_ports) {
            neighbors.push(n);
        } else if let Some(n) = cdp_neighbor(instance, row, &cdp_ports) {
            neighbors.push(n);
        }
    }
    NeighborSet::new(neighbors)
}

/// `ifIndex` → the local interface's name, from `cdpInterfaceName` (indexed by `ifIndex` alone).
fn cdp_interface_names(by_instance: &BTreeMap<Vec<u32>, Row<'_>>) -> BTreeMap<u32, String> {
    let mut out = BTreeMap::new();
    for (instance, row) in by_instance {
        let [ifindex] = instance[..] else { continue };
        if let Some(name) = text(row, NeighborColumn::CdpInterfaceName) {
            out.insert(ifindex, name);
        }
    }
    out
}

/// `lldpLocPortNum` → the port's rendered name, from `lldpLocPortTable`.
fn lldp_local_port_names(by_instance: &BTreeMap<Vec<u32>, Row<'_>>) -> BTreeMap<u32, String> {
    let mut out = BTreeMap::new();
    for (instance, row) in by_instance {
        // This table is indexed by `lldpLocPortNum` alone.
        let [port_num] = instance[..] else { continue };
        let subtype = int(row, NeighborColumn::LldpLocPortIdSubtype).unwrap_or(0);
        let name = bytes(row, NeighborColumn::LldpLocPortId)
            .map(|b| render_port_id(subtype, b))
            .filter(|s| !s.is_empty())
            // Some agents leave lldpLocPortId blank (or omit it) and put the useful string in the
            // description column instead.
            .or_else(|| text(row, NeighborColumn::LldpLocPortDesc));
        if let Some(name) = name {
            out.insert(port_num, name);
        }
    }
    out
}

/// One `lldpRemTable` row → a neighbour, or `None` when this instance is not an LLDP remote row.
fn lldp_neighbor(
    instance: &[u32],
    row: &Row<'_>,
    local_ports: &BTreeMap<u32, String>,
) -> Option<Neighbor> {
    // The three-part index is the tell. `time_mark` and `rem_index` are bound only to be discarded
    // — naming them is the documentation that dropping them is deliberate, not an oversight.
    let &[_time_mark, local_port_num, _rem_index] = instance else {
        return None;
    };
    let chassis_bytes = bytes(row, NeighborColumn::LldpRemChassisId)?;
    let chassis = render_chassis_id(
        int(row, NeighborColumn::LldpRemChassisIdSubtype).unwrap_or(0),
        chassis_bytes,
    );
    let port = bytes(row, NeighborColumn::LldpRemPortId).map_or_else(String::new, |b| {
        render_port_id(
            int(row, NeighborColumn::LldpRemPortIdSubtype).unwrap_or(0),
            b,
        )
    });
    // A port whose naming row is missing still gets a stable identity — dropping the adjacency
    // because the *local* side has no label would lose the one fact the operator asked for.
    let local_port = local_ports
        .get(&local_port_num)
        .cloned()
        .unwrap_or_else(|| format!("port {local_port_num}"));

    let mut n = Neighbor::new(NeighborProto::Lldp, local_port, chassis, port);
    n.remote_port_desc = text(row, NeighborColumn::LldpRemPortDesc);
    n.remote_sys_name = text(row, NeighborColumn::LldpRemSysName);
    n.remote_sys_desc = text(row, NeighborColumn::LldpRemSysDesc);
    n.capabilities = bytes(row, NeighborColumn::LldpRemSysCapEnabled)
        .map(lldp_capabilities)
        .unwrap_or_default();
    // `remote_mgmt_addr` stays None: LLDP keeps it in `lldpRemManAddrTable`, whose index encodes
    // the address itself. That decode is a separate increment (see `Neighbor::remote_mgmt_addr`).
    Some(n)
}

/// One `cdpCacheTable` row → a neighbour, or `None` when this instance is not a CDP cache row.
fn cdp_neighbor(
    instance: &[u32],
    row: &Row<'_>,
    interface_names: &BTreeMap<u32, String>,
) -> Option<Neighbor> {
    // Indexed by `(cdpCacheIfIndex, cdpCacheDeviceIndex)`. The device index is the agent's own
    // slot number and is dropped for the same reason `lldpRemIndex` is.
    let &[ifindex, _device_index] = instance else {
        return None;
    };
    let device_id = text(row, NeighborColumn::CdpCacheDeviceId)?;
    let device_port = text(row, NeighborColumn::CdpCacheDevicePort).unwrap_or_default();
    // CDP names the local side directly, which is why `cdpInterfaceName` is walked at all. Absent
    // ⇒ the ifIndex itself, so the adjacency still has an identity.
    let local_port = interface_names
        .get(&ifindex)
        .cloned()
        .unwrap_or_else(|| format!("ifindex {ifindex}"));

    let mut n = Neighbor::new(NeighborProto::Cdp, local_port, device_id, device_port);
    // Unlike LLDP, CDP's cache *is* indexed by ifIndex, so this one is exact rather than a guess.
    n.local_ifindex = Some(ifindex);
    n.remote_platform = text(row, NeighborColumn::CdpCachePlatform);
    n.remote_mgmt_addr = bytes(row, NeighborColumn::CdpCacheAddress).and_then(render_bare_address);
    n.capabilities = bytes(row, NeighborColumn::CdpCacheCapabilities)
        .map(cdp_capabilities)
        .unwrap_or_default();
    Some(n)
}

/// The octets of an `OCTET STRING` value; `None` for a value the agent typed as something else
/// (which is not coerced — reading an integer column as text is how a subtype gets misread).
fn as_bytes(value: &SnmpValue) -> Option<&[u8]> {
    match value {
        SnmpValue::Bytes(b) => Some(b.as_slice()),
        SnmpValue::Int(_) | SnmpValue::Oid(_) => None,
    }
}

fn bytes<'a>(row: &Row<'a>, field: NeighborColumn) -> Option<&'a [u8]> {
    row.get(&field).and_then(|v| as_bytes(v))
}

fn int(row: &Row<'_>, field: NeighborColumn) -> Option<i64> {
    match row.get(&field)? {
        SnmpValue::Int(i) => Some(*i),
        SnmpValue::Bytes(_) | SnmpValue::Oid(_) => None,
    }
}

fn text(row: &Row<'_>, field: NeighborColumn) -> Option<String> {
    bytes(row, field).map(render_text).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::{builtin_neighbor_columns, NeighborCapability};

    /// The job's column list, exactly as core sends it.
    fn columns() -> Vec<SnmpNeighborColumn> {
        builtin_neighbor_columns()
            .into_iter()
            .map(|(field, oid)| SnmpNeighborColumn {
                field,
                oid: oid.to_owned(),
            })
            .collect()
    }

    fn base_of(field: NeighborColumn) -> String {
        builtin_neighbor_columns()
            .into_iter()
            .find(|(f, _)| *f == field)
            .map(|(_, oid)| oid.to_owned())
            .expect("every column has a base")
    }

    fn row(field: NeighborColumn, instance: &[u32], value: SnmpValue) -> SnmpInstanceRow {
        SnmpInstanceRow {
            oid_base: base_of(field),
            instance: instance.to_vec(),
            value,
        }
    }

    fn b(field: NeighborColumn, instance: &[u32], value: &[u8]) -> SnmpInstanceRow {
        row(field, instance, SnmpValue::Bytes(value.to_vec()))
    }

    fn i(field: NeighborColumn, instance: &[u32], value: i64) -> SnmpInstanceRow {
        row(field, instance, SnmpValue::Int(value))
    }

    /// One LLDP adjacency: local port 3 (named `Gi0/3`) facing `core-sw-01`'s `Gi1/0/24`.
    /// `time_mark` and `rem_index` are the caller's, so a test can move them.
    fn lldp_rows(time_mark: u32, rem_index: u32) -> Vec<SnmpInstanceRow> {
        let idx = [time_mark, 3, rem_index];
        vec![
            // The naming table (indexed by lldpLocPortNum alone).
            i(NeighborColumn::LldpLocPortIdSubtype, &[3], 5), // interfaceName
            b(NeighborColumn::LldpLocPortId, &[3], b"Gi0/3"),
            // The adjacency itself.
            i(NeighborColumn::LldpRemChassisIdSubtype, &idx, 4), // macAddress
            b(
                NeighborColumn::LldpRemChassisId,
                &idx,
                &[0x00, 0x1b, 0x54, 0xff, 0x00, 0x9a],
            ),
            i(NeighborColumn::LldpRemPortIdSubtype, &idx, 5), // interfaceName
            b(NeighborColumn::LldpRemPortId, &idx, b"Gi1/0/24"),
            b(NeighborColumn::LldpRemSysName, &idx, b"core-sw-01"),
            b(NeighborColumn::LldpRemSysDesc, &idx, b"Cisco IOS 15.2"),
            b(NeighborColumn::LldpRemSysCapEnabled, &idx, &[0x28]), // bridge + router
        ]
    }

    #[test]
    fn an_lldp_row_becomes_one_normalized_neighbor() {
        let set = assemble(&columns(), &lldp_rows(1000, 1));
        assert_eq!(set.len(), 1);
        let n = &set.neighbors[0];
        assert_eq!(n.proto, NeighborProto::Lldp);
        assert_eq!(n.local_port, "Gi0/3");
        // The chassis id is a MAC, rendered as one — not as replacement characters.
        assert_eq!(n.remote_chassis, "00:1b:54:ff:00:9a");
        assert_eq!(n.remote_port, "Gi1/0/24");
        assert_eq!(n.remote_sys_name.as_deref(), Some("core-sw-01"));
        assert_eq!(n.remote_sys_desc.as_deref(), Some("Cisco IOS 15.2"));
        assert_eq!(
            n.capabilities,
            vec![NeighborCapability::Router, NeighborCapability::Bridge]
        );
        // LLDP's local port number is not an ifIndex, so none is claimed.
        assert_eq!(n.local_ifindex, None);
    }

    /// **The property this whole module exists for.** `lldpRemTimeMark` advances on every agent
    /// refresh and `lldpRemIndex` is reassigned at the agent's discretion — both arrive as part of
    /// the table index. If either reached the record, the change log would gain a row on every
    /// poll and stop being a change log.
    #[test]
    fn a_moved_time_mark_and_renumbered_rem_index_produce_an_identical_set() {
        let first = assemble(&columns(), &lldp_rows(1000, 1));
        let later = assemble(&columns(), &lldp_rows(987_654, 9));
        assert_eq!(first, later);
        assert_eq!(first.content_key(), later.content_key());
    }

    #[test]
    fn a_missing_local_port_name_still_yields_an_identity() {
        // Drop the two naming rows: the adjacency must survive with a synthetic port label.
        let rows: Vec<_> = lldp_rows(1000, 1)
            .into_iter()
            .filter(|r| r.instance != vec![3])
            .collect();
        let set = assemble(&columns(), &rows);
        assert_eq!(set.len(), 1);
        assert_eq!(set.neighbors[0].local_port, "port 3");
    }

    #[test]
    fn the_local_port_description_is_the_fallback_name() {
        let mut rows = lldp_rows(1000, 1);
        rows.retain(|r| r.oid_base != base_of(NeighborColumn::LldpLocPortId));
        rows.push(b(
            NeighborColumn::LldpLocPortDesc,
            &[3],
            b"GigabitEthernet0/3",
        ));
        let set = assemble(&columns(), &rows);
        assert_eq!(set.neighbors[0].local_port, "GigabitEthernet0/3");
    }

    #[test]
    fn a_cdp_row_becomes_one_normalized_neighbor() {
        let idx = [7, 2];
        let rows = vec![
            b(
                NeighborColumn::CdpInterfaceName,
                &[7],
                b"GigabitEthernet0/7",
            ),
            b(NeighborColumn::CdpCacheDeviceId, &idx, b"edge-rtr-02"),
            b(NeighborColumn::CdpCacheDevicePort, &idx, b"FastEthernet0/1"),
            b(NeighborColumn::CdpCachePlatform, &idx, b"cisco WS-C2960"),
            i(NeighborColumn::CdpCacheAddressType, &idx, 1),
            b(NeighborColumn::CdpCacheAddress, &idx, &[192, 168, 1, 9]),
            b(NeighborColumn::CdpCacheCapabilities, &idx, &[0, 0, 0, 0x09]),
        ];
        let set = assemble(&columns(), &rows);
        assert_eq!(set.len(), 1);
        let n = &set.neighbors[0];
        assert_eq!(n.proto, NeighborProto::Cdp);
        assert_eq!(n.local_port, "GigabitEthernet0/7");
        assert_eq!(n.remote_chassis, "edge-rtr-02");
        assert_eq!(n.remote_port, "FastEthernet0/1");
        assert_eq!(n.remote_platform.as_deref(), Some("cisco WS-C2960"));
        assert_eq!(n.remote_mgmt_addr.as_deref(), Some("192.168.1.9"));
        // CDP's cache genuinely *is* indexed by ifIndex, so this one is exact.
        assert_eq!(n.local_ifindex, Some(7));
        assert_eq!(
            n.capabilities,
            vec![NeighborCapability::Router, NeighborCapability::Switch]
        );
    }

    #[test]
    fn a_cdp_row_without_an_interface_name_falls_back_to_the_ifindex() {
        let idx = [7, 2];
        let set = assemble(
            &columns(),
            &[
                b(NeighborColumn::CdpCacheDeviceId, &idx, b"edge-rtr-02"),
                b(NeighborColumn::CdpCacheDevicePort, &idx, b"Fa0/1"),
            ],
        );
        assert_eq!(set.neighbors[0].local_port, "ifindex 7");
        assert_eq!(set.neighbors[0].local_ifindex, Some(7));
    }

    #[test]
    fn both_protocols_assemble_together_without_interfering() {
        let mut rows = lldp_rows(1000, 1);
        let idx = [7, 2];
        rows.push(b(NeighborColumn::CdpInterfaceName, &[7], b"Gi0/7"));
        rows.push(b(NeighborColumn::CdpCacheDeviceId, &idx, b"edge-rtr-02"));
        rows.push(b(NeighborColumn::CdpCacheDevicePort, &idx, b"Fa0/1"));
        let set = assemble(&columns(), &rows);
        assert_eq!(set.len(), 2);
        let protos: Vec<_> = set.neighbors.iter().map(|n| n.proto).collect();
        assert!(protos.contains(&NeighborProto::Lldp));
        assert!(protos.contains(&NeighborProto::Cdp));
    }

    /// A `cdpInterfaceName` row is indexed by ifIndex alone — the same shape as the LLDP naming
    /// table. It must not be mistaken for an adjacency in its own right.
    #[test]
    fn naming_rows_do_not_become_neighbors() {
        let set = assemble(
            &columns(),
            &[
                b(NeighborColumn::CdpInterfaceName, &[7], b"Gi0/7"),
                i(NeighborColumn::LldpLocPortIdSubtype, &[3], 5),
                b(NeighborColumn::LldpLocPortId, &[3], b"Gi0/3"),
            ],
        );
        assert!(set.is_empty());
    }

    #[test]
    fn a_device_that_speaks_neither_protocol_yields_an_empty_set() {
        // Distinct from "the walk failed": an empty set is a real observation the caller stores.
        let set = assemble(&columns(), &[]);
        assert!(set.is_empty() && !set.truncated);
    }

    /// Rows for a column the poller was not asked to walk (or does not know) are ignored rather
    /// than attributed to whichever field happens to sort nearby.
    #[test]
    fn rows_from_an_undeclared_column_are_ignored() {
        let mut rows = lldp_rows(1000, 1);
        rows.push(SnmpInstanceRow {
            oid_base: "1.2.3.4.5".to_owned(),
            instance: vec![1000, 3, 1],
            value: SnmpValue::Bytes(b"junk".to_vec()),
        });
        assert_eq!(
            assemble(&columns(), &rows),
            assemble(&columns(), &lldp_rows(1000, 1))
        );
    }

    /// A subtype column that arrives as octets (a broken agent) must not be read as an integer, and
    /// must not take the whole adjacency down with it.
    #[test]
    fn a_malformed_subtype_column_degrades_to_text_rendering() {
        let mut rows = lldp_rows(1000, 1);
        rows.retain(|r| r.oid_base != base_of(NeighborColumn::LldpRemChassisIdSubtype));
        rows.push(b(
            NeighborColumn::LldpRemChassisIdSubtype,
            &[1000, 3, 1],
            b"4",
        ));
        let set = assemble(&columns(), &rows);
        assert_eq!(set.len(), 1);
        // Subtype unreadable ⇒ shape 0 ⇒ text/hex, and the MAC bytes are not valid UTF-8.
        assert_eq!(set.neighbors[0].remote_chassis, "00:1b:54:ff:00:9a");
    }

    #[test]
    fn the_set_is_capped_and_the_overflow_is_reported() {
        let mut rows = Vec::new();
        for n in 0..(yagra_common::MAX_NEIGHBORS_PER_NODE as u32 + 10) {
            let idx = [1000, n, 1];
            rows.push(i(NeighborColumn::LldpRemChassisIdSubtype, &idx, 7));
            rows.push(b(
                NeighborColumn::LldpRemChassisId,
                &idx,
                format!("peer-{n:04}").as_bytes(),
            ));
            rows.push(b(NeighborColumn::LldpRemPortId, &idx, b"e1"));
        }
        let set = assemble(&columns(), &rows);
        assert_eq!(set.len(), yagra_common::MAX_NEIGHBORS_PER_NODE);
        assert!(set.truncated);
    }
}
