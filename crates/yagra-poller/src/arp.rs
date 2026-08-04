// SPDX-License-Identifier: AGPL-3.0-only
//! Assemble walked ARP / IPv6-neighbour rows into an [`ArpSummary`] (ADR-043 Increment 3).
//!
//! Kept **pure** — already-walked [`SnmpInstanceRow`]s in, a summary out — for the same reason
//! `l3::assemble` and `neighbors::assemble` are. Two things it must get right:
//!
//! * **Both the address and the port are in the row index**, in both tables and in different
//!   shapes. `ipNetToPhysicalTable` indexes `(ifIndex, addrType, addrLen, addr…)`; the legacy
//!   `ipNetToMediaTable` indexes `(ifIndex, a, b, c, d)`. The *value* is only the MAC, so an
//!   index-folding walker would throw away the entire answer — which is why this rides
//!   `snmp_walk_instances` rather than the numeric walker.
//!
//! * **An unusable MAC is not an unusable row.** An incomplete ARP entry answers with zero or four
//!   octets; the address is still a host that replied, so the entry is kept with `mac = None`
//!   rather than dropped. The address is the discovery, not the hardware behind it.
//!
//! The filtering, per-port rollup and cap all live in [`ArpSummary::new`], so this module decodes
//! and nothing else — the aggregation rules are tested once, in `yagra-common`, instead of being
//! re-asserted here against a second implementation.

use std::net::{IpAddr, Ipv4Addr};
use yagra_bus::SnmpArpColumn;
use yagra_common::{inet_address_from_index, render_mac, ArpColumn, ArpEntry, ArpSummary};
use yagra_transport::{SnmpInstanceRow, SnmpValue};

/// Build the node's endpoint summary from every walked row.
///
/// `columns` is the job's declared column list; a row whose base is not in it is ignored, so a
/// poller that receives a column it has no handling for degrades to "that table is absent" rather
/// than mis-attributing values.
///
/// `walk_truncated` is the caller's own answer to "did the transport stop early", which this
/// function cannot see: the walk budget is spent inside `snmp_walk_instances` and a full page looks
/// exactly like a partial one from here.
#[must_use]
pub fn assemble(
    columns: &[SnmpArpColumn],
    rows: &[SnmpInstanceRow],
    walk_truncated: bool,
) -> ArpSummary {
    let mut entries = Vec::new();
    for row in rows {
        let Some(field) = columns
            .iter()
            .find(|c| c.oid == row.oid_base)
            .map(|c| c.field)
        else {
            continue;
        };
        let decoded = match field {
            ArpColumn::IpNetToPhysicalPhysAddress => physical_entry(&row.instance),
            ArpColumn::IpNetToMediaPhysAddress => media_entry(&row.instance),
        };
        if let Some((ifindex, ip)) = decoded {
            let mut entry = ArpEntry::new(ifindex, ip);
            // `render_mac` returns `None` for anything that is not six octets, which is the correct
            // reading of an incomplete entry — and it is the same renderer LLDP chassis ids use, so
            // one address formats identically wherever it appears in the UI.
            entry.mac = match &row.value {
                SnmpValue::Bytes(b) => render_mac(b),
                SnmpValue::Int(_) | SnmpValue::Oid(_) => None,
            };
            entries.push(entry);
        }
    }
    ArpSummary::new(entries, walk_truncated)
}

/// One `ipNetToPhysicalTable` (`.4.35`) row index → `(ifIndex, address)`.
///
/// Index: `(ipNetToPhysicalIfIndex, ipNetToPhysicalNetAddressType, ipNetToPhysicalNetAddress)`,
/// the address being length-prefixed — the same `InetAddressType`/`InetAddress` encoding
/// `ipAddressTable` uses, so it is decoded by the same shared function rather than a second copy of
/// the rule.
fn physical_entry(instance: &[u32]) -> Option<(u32, IpAddr)> {
    let [ifindex, addr_type, rest @ ..] = instance else {
        return None;
    };
    Some((*ifindex, inet_address_from_index(*addr_type, rest)?))
}

/// One `ipNetToMediaTable` (`.4.22`) row index → `(ifIndex, address)`.
///
/// Index: `(ipNetToMediaIfIndex, ipNetToMediaNetAddress)` where the address is four bare
/// sub-identifiers. IPv4-only by design; the table has no v6 form, which is why the modern table is
/// walked alongside it rather than instead of it.
fn media_entry(instance: &[u32]) -> Option<(u32, IpAddr)> {
    let &[ifindex, a, b, c, d] = instance else {
        return None;
    };
    let octets: Vec<u8> = [a, b, c, d]
        .iter()
        .map(|v| u8::try_from(*v).ok())
        .collect::<Option<_>>()?;
    Some((
        ifindex,
        IpAddr::V4(Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3])),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::builtin_arp_columns;

    /// The job's column list, exactly as core sends it.
    fn columns() -> Vec<SnmpArpColumn> {
        builtin_arp_columns()
            .into_iter()
            .map(|(field, oid)| SnmpArpColumn {
                field,
                oid: oid.to_owned(),
            })
            .collect()
    }

    fn base_of(field: ArpColumn) -> String {
        builtin_arp_columns()
            .into_iter()
            .find(|(f, _)| *f == field)
            .map(|(_, o)| o.to_owned())
            .unwrap()
    }

    fn row(field: ArpColumn, instance: &[u32], value: SnmpValue) -> SnmpInstanceRow {
        SnmpInstanceRow {
            oid_base: base_of(field),
            instance: instance.to_vec(),
            value,
        }
    }

    fn mac(bytes: [u8; 6]) -> SnmpValue {
        SnmpValue::Bytes(bytes.to_vec())
    }

    #[test]
    fn a_modern_table_row_yields_the_port_and_the_address() {
        // ifIndex 8, ipv4(1), 4 octets, 192.168.1.20 → aa:bb:cc:dd:ee:ff.
        let rows = vec![row(
            ArpColumn::IpNetToPhysicalPhysAddress,
            &[8, 1, 4, 192, 168, 1, 20],
            mac([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]),
        )];
        let summary = assemble(&columns(), &rows, false);
        assert_eq!(summary.len(), 1);
        assert_eq!(summary.entries[0].ifindex, 8);
        assert_eq!(summary.entries[0].ip.to_string(), "192.168.1.20");
        assert_eq!(summary.entries[0].mac.as_deref(), Some("aa:bb:cc:dd:ee:ff"));
        assert_eq!(
            summary.per_interface,
            vec![yagra_common::ArpInterfaceCount {
                ifindex: 8,
                count: 1
            }]
        );
    }

    #[test]
    fn a_legacy_table_row_yields_the_same_shape() {
        let rows = vec![row(
            ArpColumn::IpNetToMediaPhysAddress,
            &[3, 10, 0, 0, 7],
            mac([0x00, 0x11, 0x22, 0x33, 0x44, 0x55]),
        )];
        let summary = assemble(&columns(), &rows, false);
        assert_eq!(summary.len(), 1);
        assert_eq!(summary.entries[0].ifindex, 3);
        assert_eq!(summary.entries[0].ip.to_string(), "10.0.0.7");
        assert_eq!(summary.entries[0].mac.as_deref(), Some("00:11:22:33:44:55"));
    }

    #[test]
    fn a_device_answering_both_tables_reports_each_address_once() {
        // A dual-implementing agent returns the same neighbour in both tables. The address is the
        // identity, so one record must survive — otherwise every such device would double-count
        // every port and the operator-facing badge would be twice the truth.
        let rows = vec![
            row(
                ArpColumn::IpNetToPhysicalPhysAddress,
                &[8, 1, 4, 192, 168, 1, 20],
                mac([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]),
            ),
            row(
                ArpColumn::IpNetToMediaPhysAddress,
                &[8, 192, 168, 1, 20],
                mac([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]),
            ),
        ];
        let summary = assemble(&columns(), &rows, false);
        assert_eq!(summary.len(), 1);
        assert_eq!(summary.observed, 1);
        assert_eq!(summary.per_interface[0].count, 1);
    }

    #[test]
    fn an_ipv6_neighbour_decodes_and_its_link_local_siblings_do_not_survive() {
        // The realistic shape of an ND cache: one global address worth discovering, surrounded by
        // link-local entries that are noise. Both decode; only one is an endpoint.
        let mut global = vec![7u32, 2, 16];
        global.extend([0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9]);
        let mut ll = vec![7u32, 2, 16];
        ll.extend([0xfe, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]);
        let rows = vec![
            row(
                ArpColumn::IpNetToPhysicalPhysAddress,
                &global,
                mac([1, 2, 3, 4, 5, 6]),
            ),
            row(
                ArpColumn::IpNetToPhysicalPhysAddress,
                &ll,
                mac([1, 2, 3, 4, 5, 7]),
            ),
        ];
        let summary = assemble(&columns(), &rows, false);
        assert_eq!(summary.len(), 1);
        assert_eq!(summary.entries[0].ip.to_string(), "2001:db8::9");
    }

    #[test]
    fn an_incomplete_entry_keeps_its_address_and_loses_only_its_mac() {
        // `ipNetToPhysicalPhysAddress` is a zero-length string while resolution is pending. The
        // address still named a host the device tried to reach.
        let rows = vec![row(
            ArpColumn::IpNetToPhysicalPhysAddress,
            &[8, 1, 4, 192, 168, 1, 21],
            SnmpValue::Bytes(Vec::new()),
        )];
        let summary = assemble(&columns(), &rows, false);
        assert_eq!(summary.len(), 1);
        assert_eq!(summary.entries[0].mac, None);
    }

    #[test]
    fn a_malformed_index_is_skipped_rather_than_guessed() {
        for instance in [
            // Length disagrees with the octets present.
            vec![8, 1, 4, 192, 168, 1],
            // A type/length pair the MIB does not define.
            vec![8, 9, 4, 192, 168, 1, 20],
            // An octet that cannot be one.
            vec![8, 1, 4, 192, 168, 300, 20],
            // No address at all.
            vec![8],
        ] {
            let rows = vec![row(
                ArpColumn::IpNetToPhysicalPhysAddress,
                &instance,
                mac([1, 2, 3, 4, 5, 6]),
            )];
            assert!(
                assemble(&columns(), &rows, false).is_empty(),
                "{instance:?} must not decode"
            );
        }
        // And the legacy table's fixed five-part index is just as strict.
        let rows = vec![row(
            ArpColumn::IpNetToMediaPhysAddress,
            &[3, 10, 0, 7],
            mac([1, 2, 3, 4, 5, 6]),
        )];
        assert!(assemble(&columns(), &rows, false).is_empty());
    }

    #[test]
    fn a_column_the_job_did_not_declare_is_ignored_rather_than_misattributed() {
        let rows = vec![
            row(
                ArpColumn::IpNetToPhysicalPhysAddress,
                &[8, 1, 4, 192, 168, 1, 20],
                mac([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]),
            ),
            SnmpInstanceRow {
                oid_base: "1.3.6.1.2.1.4.35.1.6".into(),
                instance: vec![8, 1, 4, 192, 168, 1, 20],
                value: SnmpValue::Int(3),
            },
        ];
        let summary = assemble(&columns(), &rows, false);
        assert_eq!(summary.len(), 1, "the undeclared column changed nothing");
    }

    #[test]
    fn the_truncation_flag_is_the_callers_answer_not_a_guess() {
        // Nothing in the rows themselves can say whether the walk ran out of budget, so the flag has
        // to be threaded through — and it must survive assembly untouched.
        let rows = vec![row(
            ArpColumn::IpNetToPhysicalPhysAddress,
            &[8, 1, 4, 192, 168, 1, 20],
            mac([1, 2, 3, 4, 5, 6]),
        )];
        assert!(!assemble(&columns(), &rows, false).truncated);
        assert!(assemble(&columns(), &rows, true).truncated);
    }

    #[test]
    fn a_device_with_an_empty_cache_yields_an_empty_summary() {
        // Distinct from a *failed* walk, which never reaches this function: the worker sends
        // `arp = None` for that and core writes nothing.
        let summary = assemble(&columns(), &[], false);
        assert!(summary.is_empty());
        assert!(!summary.truncated);
        assert_eq!(summary.observed, 0);
    }

    #[test]
    fn assembly_is_order_independent() {
        let mut rows = vec![
            row(
                ArpColumn::IpNetToPhysicalPhysAddress,
                &[8, 1, 4, 192, 168, 1, 20],
                mac([1, 2, 3, 4, 5, 6]),
            ),
            row(
                ArpColumn::IpNetToMediaPhysAddress,
                &[3, 10, 0, 0, 7],
                mac([2, 2, 3, 4, 5, 6]),
            ),
            row(
                ArpColumn::IpNetToPhysicalPhysAddress,
                &[8, 1, 4, 192, 168, 1, 21],
                mac([3, 2, 3, 4, 5, 6]),
            ),
        ];
        let forward = assemble(&columns(), &rows, false);
        rows.reverse();
        let reversed = assemble(&columns(), &rows, false);
        assert_eq!(forward, reversed);
        assert_eq!(forward.content_key(), reversed.content_key());
    }
}
