// SPDX-License-Identifier: AGPL-3.0-only
//! Assemble walked interface-address rows into an [`L3Snapshot`] (ADR-043).
//!
//! Kept **pure** — already-walked [`SnmpInstanceRow`]s in, a snapshot out — for the same reason
//! `neighbors::assemble` is: this is where the feature is won or lost, and every rule deserves a
//! test that runs without a device. Three things it must get right:
//!
//! * **The address is the row index, in both tables.** `ipAddrTable` is indexed by the IPv4 address
//!   itself (four sub-identifiers) and `ipAddressTable` by an `InetAddressType` + length-prefixed
//!   `InetAddress`. Neither has an "address" column worth walking, so the instance is not a join
//!   key to be discarded here the way `lldpRemTimeMark` is — it is the answer.
//!
//! * **Two tables, one preference.** A device may answer the old table, the new one, or both (the
//!   lab's Huawei USG answers both). Both are walked because neither alone is enough: `.4.20` is
//!   IPv4-only, and `.4.34` has no mask column. Where they overlap, the modern row wins — but that
//!   choice is made by [`L3Snapshot::canonicalize`], not here, so the precedence lives in one place
//!   rather than being re-implemented per caller.
//!
//! * **A missing prefix is not a missing address.** An address whose mask could not be decoded is
//!   still recorded, with `prefix_len = 0`. It forms no subnet edge, but Increment 4 resolves host
//!   routes through the routing table and needs to know they exist.

use std::collections::BTreeMap;
use yagra_bus::SnmpL3Column;
use yagra_common::{
    decode_prefix_pointer, inet_address_from_index, prefix_len_from_mask, L3AddrType, L3Address,
    L3Column, L3Snapshot, L3SourceTable,
};
use yagra_transport::{SnmpInstanceRow, SnmpValue};

/// The columns of one table row, keyed by which field they carry.
type Row<'a> = BTreeMap<L3Column, &'a SnmpValue>;

/// Build the node's address snapshot from every walked row.
///
/// `columns` is the job's declared column list; rows whose base is not in it are ignored, so a
/// poller that receives a column it has no handling for degrades to "that field is absent" rather
/// than mis-attributing values — the same contract `neighbors::assemble` offers.
#[must_use]
pub fn assemble(columns: &[SnmpL3Column], rows: &[SnmpInstanceRow]) -> L3Snapshot {
    let field_by_base: BTreeMap<&str, L3Column> =
        columns.iter().map(|c| (c.oid.as_str(), c.field)).collect();

    // `BTreeMap` (not `Hash`) so the intermediate ordering is deterministic — it matters when the
    // cap trims the snapshot, and when a failing test has to be read.
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

    let mut addresses = Vec::new();
    for (instance, row) in &by_instance {
        // Which table a bucket belongs to is decided by the columns it actually carries, not by the
        // shape of its index. The shapes happen not to collide today, but keying off the columns
        // means a future column cannot make them start to.
        if let Some(a) = new_table_address(instance, row) {
            addresses.push(a);
        } else if let Some(a) = old_table_address(instance, row) {
            addresses.push(a);
        }
    }
    L3Snapshot::new(addresses)
}

/// One `ipAddressTable` (`.4.34`) row → an address.
///
/// Index: `(ipAddressAddrType, ipAddressAddr)`, the address being length-prefixed. The prefix
/// length comes from `ipAddressPrefix`, a RowPointer whose own tail carries it — so
/// `ipAddressPrefixTable` never has to be walked.
fn new_table_address(instance: &[u32], row: &Row<'_>) -> Option<L3Address> {
    // Require at least one column that only this table has, so an `ipAddrTable` bucket cannot be
    // mistaken for a malformed one of these.
    if !row.contains_key(&L3Column::IpAddressIfIndex)
        && !row.contains_key(&L3Column::IpAddressType)
        && !row.contains_key(&L3Column::IpAddressPrefix)
    {
        return None;
    }
    let (&addr_type_id, rest) = instance.split_first()?;
    let ip = inet_address_from_index(addr_type_id, rest)?;

    // The RowPointer names its own ifIndex, which is the fallback when `ipAddressIfIndex` was not
    // walked or the agent omitted it.
    let prefix = oid(row, L3Column::IpAddressPrefix).and_then(decode_prefix_pointer);
    let ifindex = int(row, L3Column::IpAddressIfIndex)
        .and_then(|v| u32::try_from(v).ok())
        .or_else(|| prefix.map(|(i, _)| i))
        .unwrap_or(0);

    let mut addr = L3Address::new(ifindex, ip, prefix.map_or(0, |(_, k)| k.prefix_len));
    addr.source_table = L3SourceTable::IpAddressTable;
    addr.addr_type =
        int(row, L3Column::IpAddressType).map_or(L3AddrType::Unknown, L3AddrType::from_snmp);
    Some(addr)
}

/// One `ipAddrTable` (`.4.20`) row → an address.
///
/// Index: the IPv4 address, four sub-identifiers, nothing else. This table is IPv4-only by design;
/// it is walked anyway because it returns the mask as a plain column, which is the cheapest correct
/// answer on any agent that implements it, and because some agents implement only this one.
fn old_table_address(instance: &[u32], row: &Row<'_>) -> Option<L3Address> {
    if !row.contains_key(&L3Column::IpAdEntIfIndex) && !row.contains_key(&L3Column::IpAdEntNetMask)
    {
        return None;
    }
    let &[a, b, c, d] = instance else { return None };
    let octets: Vec<u8> = [a, b, c, d]
        .iter()
        .map(|v| u8::try_from(*v).ok())
        .collect::<Option<_>>()?;
    let ip = std::net::IpAddr::V4(std::net::Ipv4Addr::new(
        octets[0], octets[1], octets[2], octets[3],
    ));

    // The mask arrives as four octets. It reaches us as `Bytes` on both transports only because
    // ASN.1 `IpAddress` is mapped there — the v3 walker used to drop it, which would have made this
    // column work on v2c and silently return nothing on v3.
    let prefix_len = bytes(row, L3Column::IpAdEntNetMask)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .and_then(prefix_len_from_mask)
        .unwrap_or(0);
    let ifindex = int(row, L3Column::IpAdEntIfIndex)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);

    let mut addr = L3Address::new(ifindex, ip, prefix_len);
    addr.source_table = L3SourceTable::IpAddrTable;
    // The old table has no `ipAddressType` column, so nothing is known about the address's role.
    addr.addr_type = L3AddrType::Unknown;
    Some(addr)
}

/// The octets of an `OCTET STRING`-ish value; `None` for a value the agent typed as something else
/// (which is not coerced — reading an integer column as octets is how a subtype gets misread).
fn bytes<'a>(row: &Row<'a>, field: L3Column) -> Option<&'a [u8]> {
    match row.get(&field)? {
        SnmpValue::Bytes(b) => Some(b.as_slice()),
        SnmpValue::Int(_) | SnmpValue::Oid(_) => None,
    }
}

fn int(row: &Row<'_>, field: L3Column) -> Option<i64> {
    match row.get(&field)? {
        SnmpValue::Int(i) => Some(*i),
        SnmpValue::Bytes(_) | SnmpValue::Oid(_) => None,
    }
}

/// The dotted-decimal form of an `OBJECT IDENTIFIER` value. This is the accessor that had no
/// consumer before ADR-043: both walkers produce `SnmpValue::Oid`, and every reader dropped it.
fn oid<'a>(row: &Row<'a>, field: L3Column) -> Option<&'a str> {
    match row.get(&field)? {
        SnmpValue::Oid(s) => Some(s.as_str()),
        SnmpValue::Int(_) | SnmpValue::Bytes(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::builtin_l3_columns;

    /// The job's column list, exactly as core sends it.
    fn columns() -> Vec<SnmpL3Column> {
        builtin_l3_columns()
            .into_iter()
            .map(|(field, oid)| SnmpL3Column {
                field,
                oid: oid.to_owned(),
            })
            .collect()
    }

    fn base_of(field: L3Column) -> String {
        builtin_l3_columns()
            .into_iter()
            .find(|(f, _)| *f == field)
            .map(|(_, o)| o.to_owned())
            .unwrap()
    }

    fn row(field: L3Column, instance: &[u32], value: SnmpValue) -> SnmpInstanceRow {
        SnmpInstanceRow {
            oid_base: base_of(field),
            instance: instance.to_vec(),
            value,
        }
    }

    /// The lab's Huawei USG, old table: `192.168.1.1` on ifIndex 8 with a /24 mask.
    fn usg_old_table() -> Vec<SnmpInstanceRow> {
        let inst = [192, 168, 1, 1];
        vec![
            row(L3Column::IpAdEntIfIndex, &inst, SnmpValue::Int(8)),
            row(
                L3Column::IpAdEntNetMask,
                &inst,
                SnmpValue::Bytes(vec![255, 255, 255, 0]),
            ),
        ]
    }

    /// The same address from the new table, whose index is `(type, len, octets)`.
    fn usg_new_table() -> Vec<SnmpInstanceRow> {
        let inst = [1, 4, 192, 168, 1, 1];
        vec![
            row(L3Column::IpAddressIfIndex, &inst, SnmpValue::Int(8)),
            row(L3Column::IpAddressType, &inst, SnmpValue::Int(1)),
            row(
                L3Column::IpAddressPrefix,
                &inst,
                SnmpValue::Oid("1.3.6.1.2.1.4.32.1.5.8.1.4.192.168.1.0.24".into()),
            ),
        ]
    }

    #[test]
    fn an_ip_addr_table_row_becomes_one_address() {
        let snap = assemble(&columns(), &usg_old_table());
        assert_eq!(snap.len(), 1);
        let a = &snap.addresses[0];
        assert_eq!(a.ip.to_string(), "192.168.1.1");
        assert_eq!(a.prefix_len, 24);
        assert_eq!(a.ifindex, 8);
        assert_eq!(a.source_table, L3SourceTable::IpAddrTable);
        assert_eq!(a.subnet().unwrap().to_string(), "192.168.1.0/24");
    }

    #[test]
    fn an_ip_address_table_row_decodes_its_prefix_from_the_row_pointer() {
        let snap = assemble(&columns(), &usg_new_table());
        assert_eq!(snap.len(), 1);
        let a = &snap.addresses[0];
        assert_eq!(a.ip.to_string(), "192.168.1.1");
        assert_eq!(
            a.prefix_len, 24,
            "the /24 is the pointer's last sub-identifier"
        );
        assert_eq!(a.ifindex, 8);
        assert_eq!(a.addr_type, L3AddrType::Unicast);
        assert_eq!(a.source_table, L3SourceTable::IpAddressTable);
    }

    #[test]
    fn the_two_tables_agree_and_do_not_duplicate() {
        // The real device answers both walks for the same address; exactly one record must survive,
        // and it must be the modern one — it is the only source of `ipAddressType`.
        let mut rows = usg_old_table();
        rows.extend(usg_new_table());
        let snap = assemble(&columns(), &rows);
        assert_eq!(snap.len(), 1, "one address, not two");
        assert_eq!(
            snap.addresses[0].source_table,
            L3SourceTable::IpAddressTable
        );
        assert_eq!(snap.addresses[0].addr_type, L3AddrType::Unicast);
        assert_eq!(snap.addresses[0].prefix_len, 24);
    }

    #[test]
    fn a_device_that_answers_neither_yields_an_empty_snapshot() {
        // Distinct from a *failed* walk, which never reaches this function: the worker sends
        // `l3 = None` for that and core writes nothing.
        let snap = assemble(&columns(), &[]);
        assert!(snap.is_empty());
        assert!(!snap.truncated);
    }

    #[test]
    fn the_lab_host_route_is_recorded_but_forms_no_subnet() {
        // `Dialer1 133.123.189.109/32` — the PPPoE WAN address on the real device. Increment 4
        // resolves its peer through the routing table, so it must be *stored*, just not joined.
        let inst = [1, 4, 133, 123, 189, 109];
        let rows = vec![
            row(L3Column::IpAddressIfIndex, &inst, SnmpValue::Int(16)),
            row(L3Column::IpAddressType, &inst, SnmpValue::Int(1)),
            row(
                L3Column::IpAddressPrefix,
                &inst,
                SnmpValue::Oid("1.3.6.1.2.1.4.32.1.5.16.1.4.133.123.189.109.32".into()),
            ),
        ];
        let snap = assemble(&columns(), &rows);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap.addresses[0].prefix_len, 32);
        assert!(snap.addresses[0].is_host_route());
        assert_eq!(snap.addresses[0].subnet(), None);
        assert!(snap.subnets().is_empty());
    }

    #[test]
    fn a_v6_only_device_produces_v6_subnet_keys() {
        let mut inst = vec![2, 16];
        inst.extend([0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        let mut pointer = String::from("1.3.6.1.2.1.4.32.1.5.5.2.16");
        for b in [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0] {
            pointer.push('.');
            pointer.push_str(&b.to_string());
        }
        pointer.push_str(".64");
        let rows = vec![
            row(L3Column::IpAddressIfIndex, &inst, SnmpValue::Int(5)),
            row(L3Column::IpAddressType, &inst, SnmpValue::Int(1)),
            row(L3Column::IpAddressPrefix, &inst, SnmpValue::Oid(pointer)),
        ];
        let snap = assemble(&columns(), &rows);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap.addresses[0].ip.to_string(), "2001:db8::1");
        assert_eq!(snap.addresses[0].prefix_len, 64);
        assert_eq!(
            snap.subnets().iter().next().unwrap().to_string(),
            "2001:db8::/64"
        );
    }

    #[test]
    fn an_address_whose_prefix_cannot_be_decoded_is_still_recorded() {
        // `zeroDotZero` is what RFC 4293 says an agent sends when it has no prefix row. Dropping the
        // address would lose an interface; recording it with prefix 0 keeps it visible and simply
        // forms no edge.
        let inst = [1, 4, 10, 0, 0, 1];
        let rows = vec![
            row(L3Column::IpAddressIfIndex, &inst, SnmpValue::Int(3)),
            row(L3Column::IpAddressType, &inst, SnmpValue::Int(1)),
            row(
                L3Column::IpAddressPrefix,
                &inst,
                SnmpValue::Oid("0.0".into()),
            ),
        ];
        let snap = assemble(&columns(), &rows);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap.addresses[0].prefix_len, 0);
        assert_eq!(snap.addresses[0].subnet(), None);
    }

    #[test]
    fn a_non_contiguous_mask_does_not_invent_a_prefix() {
        let inst = [10, 0, 0, 1];
        let rows = vec![
            row(L3Column::IpAdEntIfIndex, &inst, SnmpValue::Int(3)),
            row(
                L3Column::IpAdEntNetMask,
                &inst,
                SnmpValue::Bytes(vec![255, 0, 255, 0]),
            ),
        ];
        let snap = assemble(&columns(), &rows);
        assert_eq!(snap.addresses[0].prefix_len, 0);
    }

    #[test]
    fn the_row_pointer_supplies_the_ifindex_when_the_column_is_absent() {
        // Some agents answer `ipAddressPrefix` but not `ipAddressIfIndex`. The pointer names the
        // ifIndex itself, so the address still joins to the interface inventory.
        let inst = [1, 4, 192, 168, 1, 1];
        let rows = vec![row(
            L3Column::IpAddressPrefix,
            &inst,
            SnmpValue::Oid("1.3.6.1.2.1.4.32.1.5.8.1.4.192.168.1.0.24".into()),
        )];
        let snap = assemble(&columns(), &rows);
        assert_eq!(snap.addresses[0].ifindex, 8);
    }

    #[test]
    fn an_anycast_row_is_recorded_and_marked_so_it_anchors_nothing() {
        // The agent saying `anycast(2)` is the evidence that this is a VRRP/HSRP VIP.
        let inst = [1, 4, 10, 0, 0, 1];
        let rows = vec![
            row(L3Column::IpAddressIfIndex, &inst, SnmpValue::Int(2)),
            row(L3Column::IpAddressType, &inst, SnmpValue::Int(2)),
            row(
                L3Column::IpAddressPrefix,
                &inst,
                SnmpValue::Oid("1.3.6.1.2.1.4.32.1.5.2.1.4.10.0.0.0.24".into()),
            ),
        ];
        let snap = assemble(&columns(), &rows);
        assert_eq!(snap.addresses[0].addr_type, L3AddrType::Anycast);
        assert!(!snap.addresses[0].can_form_subnet_edge());
    }

    #[test]
    fn a_column_the_job_did_not_declare_is_ignored_rather_than_misattributed() {
        let mut rows = usg_old_table();
        rows.push(SnmpInstanceRow {
            oid_base: "1.3.6.1.2.1.4.99.1.1".into(),
            instance: vec![192, 168, 1, 1],
            value: SnmpValue::Int(1234),
        });
        let snap = assemble(&columns(), &rows);
        assert_eq!(snap.len(), 1);
        assert_eq!(
            snap.addresses[0].ifindex, 8,
            "the stray column changed nothing"
        );
    }

    #[test]
    fn a_wrongly_typed_value_is_skipped_not_coerced() {
        // A mask that arrived as an integer must not be read as octets, and vice versa.
        let inst = [10, 0, 0, 1];
        let rows = vec![
            row(L3Column::IpAdEntIfIndex, &inst, SnmpValue::Int(3)),
            row(L3Column::IpAdEntNetMask, &inst, SnmpValue::Int(24)),
        ];
        let snap = assemble(&columns(), &rows);
        assert_eq!(snap.addresses[0].prefix_len, 0);
    }

    #[test]
    fn assembly_is_order_independent() {
        let mut rows = usg_old_table();
        rows.extend(usg_new_table());
        let forward = assemble(&columns(), &rows);
        rows.reverse();
        let reversed = assemble(&columns(), &rows);
        assert_eq!(forward, reversed);
        assert_eq!(forward.content_key(), reversed.content_key());
    }
}
