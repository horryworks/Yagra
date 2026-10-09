// SPDX-License-Identifier: AGPL-3.0-only
//! Assemble walked rows into a node's port VLAN snapshot (ADR-201).
//!
//! Kept **pure** — already-walked [`SnmpInstanceRow`]s in, a [`VlanSnapshot`] out — for the reason
//! `l3::assemble` is: every rule below was learned from a real device, and each deserves a test that
//! runs without one. The session lives in `worker/adjacency.rs`.
//!
//! The OID set is decided **here**, by dialect, rather than sent by core. The job names a dialect
//! (`yagra_common::VlanDialect`), and the poller owns which columns that dialect means — the same
//! split ADR-180 made for the neighbour columns the poller adds itself. Core decides *whether* a
//! device gets the walk; this file decides *what the walk reads*.
//!
//! # Cisco Catalyst (decision 3)
//!
//! No Catalyst walked for ADR-201 answers Q-BRIDGE, so the walk reads CISCO-VTP-MIB's trunk table
//! and CISCO-VLAN-MEMBERSHIP-MIB, both indexed by `ifIndex`. A port is, in this order:
//!
//! 1. a **trunk** when `vlanTrunkPortDynamicStatus` says trunking;
//! 2. else **access** when `vmVlan` has a row for it;
//! 3. else a **trunk** when the trunk table says it is configured `on` / `onNoNegotiate` (a trunk
//!    whose link is down, or a Port-channel with no member up);
//! 4. else **unknown** when the trunk table has a row for it at all;
//! 5. else it is **not a switch port** (a routed port has a row in neither table).
//!
//! 🚨 The order is the point. **Every access port also has a trunk-table row**, carrying the trunk
//! defaults (native 1, every VLAN allowed) — read the trunk table first without its status column
//! and the whole switch reads as trunks to every VLAN. An old C3550 is the opposite case: its trunk
//! table holds the trunks only, which this order handles without a branch.
//!
//! Port-channel membership comes from `ifStackTable` and nothing else. A C2960L answers IEEE 802.3ad's
//! `dot3adAggPortAttachedAggID` with a value that is no interface at all on ports that are not
//! bundled, so that table is not walked.
//!
//! # Huawei (decision 4)
//!
//! HUAWEI-L2IF-MIB's port table is indexed by Huawei's own port number, which column 2 maps to an
//! `ifIndex`. The mode is column 32 (the operating mode, VRP only) when present, else column 3 (the
//! configured mode, which on VRP is `6` — negotiate — for every unused port). Q-BRIDGE is not read:
//! on VRP its static tables disagree with the device's own operating mode on most ports.
//!
//! Eth-Trunk membership comes from HUAWEI-IF-EXT-MIB: `hwTrunkIfTable` maps a trunk *index* to the
//! Eth-Trunk's `ifIndex`, and `hwTrunkMemTable` lists each member under that index. 🚨 The index is
//! not the Eth-Trunk's number (Eth-Trunk5 is index 0 on one device walked), so the two are only ever
//! joined through the `ifIndex`.

use std::collections::{BTreeMap, BTreeSet};
use yagra_common::{
    ids_from_bitmap, ranges_from_ids, PortMode, PortVlan, VlanDialect, VlanRange, VlanSnapshot,
};
use yagra_transport::{SnmpInstanceRow, SnmpValue};

// ---- Cisco ----
const CISCO_TRUNK_NATIVE: &str = "1.3.6.1.4.1.9.9.46.1.6.1.1.5";
const CISCO_TRUNK_ENABLED_1K: &str = "1.3.6.1.4.1.9.9.46.1.6.1.1.4";
const CISCO_TRUNK_STATE: &str = "1.3.6.1.4.1.9.9.46.1.6.1.1.13";
const CISCO_TRUNK_STATUS: &str = "1.3.6.1.4.1.9.9.46.1.6.1.1.14";
const CISCO_TRUNK_ENABLED_2K: &str = "1.3.6.1.4.1.9.9.46.1.6.1.1.17";
const CISCO_TRUNK_ENABLED_3K: &str = "1.3.6.1.4.1.9.9.46.1.6.1.1.18";
const CISCO_TRUNK_ENABLED_4K: &str = "1.3.6.1.4.1.9.9.46.1.6.1.1.19";
const CISCO_VM_VLAN: &str = "1.3.6.1.4.1.9.9.68.1.2.2.1.2";
const CISCO_VOICE_VLAN: &str = "1.3.6.1.4.1.9.9.68.1.5.1.1.1";
const IF_STACK_STATUS: &str = "1.3.6.1.2.1.31.1.2.1.3";

// ---- Huawei ----
const HW_PORT_IFINDEX: &str = "1.3.6.1.4.1.2011.5.25.42.1.1.1.3.1.2";
const HW_PORT_TYPE: &str = "1.3.6.1.4.1.2011.5.25.42.1.1.1.3.1.3";
const HW_PORT_PVID: &str = "1.3.6.1.4.1.2011.5.25.42.1.1.1.3.1.4";
const HW_PORT_ACTIVE_TYPE: &str = "1.3.6.1.4.1.2011.5.25.42.1.1.1.3.1.32";
const HW_TRUNK_ALLOW_LOW: &str = "1.3.6.1.4.1.2011.5.25.42.1.1.1.10.1.2";
const HW_TRUNK_ALLOW_HIGH: &str = "1.3.6.1.4.1.2011.5.25.42.1.1.1.10.1.3";
const HW_HYBRID_TAGGED_LOW: &str = "1.3.6.1.4.1.2011.5.25.42.1.1.1.9.1.2";
const HW_HYBRID_TAGGED_HIGH: &str = "1.3.6.1.4.1.2011.5.25.42.1.1.1.9.1.3";
const HW_HYBRID_UNTAGGED_LOW: &str = "1.3.6.1.4.1.2011.5.25.42.1.1.1.9.1.4";
const HW_HYBRID_UNTAGGED_HIGH: &str = "1.3.6.1.4.1.2011.5.25.42.1.1.1.9.1.5";
const HW_TRUNK_IFINDEX: &str = "1.3.6.1.4.1.2011.5.25.41.1.3.3.1.4";
const HW_TRUNK_MEMBER: &str = "1.3.6.1.4.1.2011.5.25.41.1.4.1.1.2";

/// The columns a dialect walks. An unknown dialect walks nothing.
#[must_use]
pub fn columns(dialect: VlanDialect) -> Vec<String> {
    let set: &[&str] = match dialect {
        VlanDialect::Cisco => &[
            CISCO_TRUNK_STATUS,
            CISCO_TRUNK_STATE,
            CISCO_TRUNK_NATIVE,
            CISCO_TRUNK_ENABLED_1K,
            CISCO_TRUNK_ENABLED_2K,
            CISCO_TRUNK_ENABLED_3K,
            CISCO_TRUNK_ENABLED_4K,
            CISCO_VM_VLAN,
            CISCO_VOICE_VLAN,
            IF_STACK_STATUS,
        ],
        VlanDialect::Huawei => &[
            HW_PORT_IFINDEX,
            HW_PORT_TYPE,
            HW_PORT_PVID,
            HW_PORT_ACTIVE_TYPE,
            HW_TRUNK_ALLOW_LOW,
            HW_TRUNK_ALLOW_HIGH,
            HW_HYBRID_TAGGED_LOW,
            HW_HYBRID_TAGGED_HIGH,
            HW_HYBRID_UNTAGGED_LOW,
            HW_HYBRID_UNTAGGED_HIGH,
            HW_TRUNK_IFINDEX,
            HW_TRUNK_MEMBER,
        ],
        VlanDialect::Unknown => &[],
    };
    set.iter().map(|s| (*s).to_owned()).collect()
}

/// Build the snapshot for `dialect` from every walked row.
#[must_use]
pub fn assemble(dialect: VlanDialect, rows: &[SnmpInstanceRow]) -> VlanSnapshot {
    match dialect {
        VlanDialect::Cisco => cisco(rows),
        VlanDialect::Huawei => huawei(rows),
        VlanDialect::Unknown => VlanSnapshot::default(),
    }
}

/// Every row of one column, keyed by its full instance.
fn column<'a>(rows: &'a [SnmpInstanceRow], base: &str) -> BTreeMap<Vec<u32>, &'a SnmpValue> {
    rows.iter()
        .filter(|r| r.oid_base == base)
        .map(|r| (r.instance.clone(), &r.value))
        .collect()
}

/// A single-index column as `index -> integer`.
fn ints(rows: &[SnmpInstanceRow], base: &str) -> BTreeMap<u32, i64> {
    column(rows, base)
        .into_iter()
        .filter_map(|(inst, v)| match (inst.as_slice(), v) {
            ([i], SnmpValue::Int(n)) => Some((*i, *n)),
            _ => None,
        })
        .collect()
}

/// A single-index column as `index -> octets`.
fn bytes<'a>(rows: &'a [SnmpInstanceRow], base: &str) -> BTreeMap<u32, &'a [u8]> {
    column(rows, base)
        .into_iter()
        .filter_map(|(inst, v)| match (inst.as_slice(), v) {
            ([i], SnmpValue::Bytes(b)) => Some((*i, b.as_slice())),
            _ => None,
        })
        .collect()
}

/// A VLAN ID from an integer column; anything outside 1..=4094 (Cisco's 0 for "none", its 4096 for
/// "no voice VLAN") is no VLAN.
fn vlan_id(n: Option<&i64>) -> Option<u16> {
    let v = u16::try_from(*n?).ok()?;
    (yagra_common::VLAN_MIN..=yagra_common::VLAN_MAX)
        .contains(&v)
        .then_some(v)
}

/// The VLANs set across one port's bitmap columns, each paired with the VLAN its first bit means.
fn vlans_from_blocks(blocks: &[(Option<&[u8]>, u16)]) -> Vec<VlanRange> {
    ranges_from_ids(
        blocks
            .iter()
            .filter_map(|(b, base)| b.map(|b| ids_from_bitmap(b, *base)))
            .flatten(),
    )
}

fn cisco(rows: &[SnmpInstanceRow]) -> VlanSnapshot {
    let status = ints(rows, CISCO_TRUNK_STATUS);
    let state = ints(rows, CISCO_TRUNK_STATE);
    let native = ints(rows, CISCO_TRUNK_NATIVE);
    let b1 = bytes(rows, CISCO_TRUNK_ENABLED_1K);
    let b2 = bytes(rows, CISCO_TRUNK_ENABLED_2K);
    let b3 = bytes(rows, CISCO_TRUNK_ENABLED_3K);
    let b4 = bytes(rows, CISCO_TRUNK_ENABLED_4K);
    let vm = ints(rows, CISCO_VM_VLAN);
    let voice = ints(rows, CISCO_VOICE_VLAN);

    let in_trunk_table: BTreeSet<u32> = status
        .keys()
        .chain(state.keys())
        .chain(native.keys())
        .copied()
        .collect();
    let switch_ports: BTreeSet<u32> = in_trunk_table.iter().chain(vm.keys()).copied().collect();

    let mut ports: BTreeMap<u32, PortVlan> = BTreeMap::new();
    for &ifx in &switch_ports {
        let trunking = status.get(&ifx) == Some(&1);
        // `on` (1) and `onNoNegotiate` (5): configured as a trunk whatever the link is doing.
        let configured_trunk = matches!(state.get(&ifx), Some(1 | 5));
        let mode = if trunking {
            PortMode::Trunk
        } else if vm.contains_key(&ifx) {
            PortMode::Access
        } else if configured_trunk {
            PortMode::Trunk
        } else {
            PortMode::Unknown
        };
        let mut p = PortVlan::new(ifx, mode);
        match mode {
            PortMode::Trunk => {
                p.native = vlan_id(native.get(&ifx));
                p.allowed = vlans_from_blocks(&[
                    (b1.get(&ifx).copied(), 0),
                    (b2.get(&ifx).copied(), 1024),
                    (b3.get(&ifx).copied(), 2048),
                    (b4.get(&ifx).copied(), 3072),
                ]);
            }
            PortMode::Access => {
                p.access_vlan = vlan_id(vm.get(&ifx));
                p.voice_vlan = vlan_id(voice.get(&ifx));
            }
            PortMode::Hybrid | PortMode::NotL2 | PortMode::Unknown => {}
        }
        ports.insert(ifx, p);
    }

    // Port-channel membership: an `ifStackTable` pair whose upper and lower layers are both switch
    // ports. Index `0` is "nothing above / below" and never a member relation.
    for inst in column(rows, IF_STACK_STATUS).keys() {
        if let [upper, lower] = inst.as_slice() {
            if *upper != 0
                && *lower != 0
                && upper != lower
                && switch_ports.contains(upper)
                && switch_ports.contains(lower)
            {
                if let Some(p) = ports.get_mut(lower) {
                    p.lag_ifindex = Some(*upper);
                }
            }
        }
    }
    VlanSnapshot::new(ports.into_values().collect())
}

/// Huawei's mode codes, shared by the configured (3) and operating (32) columns.
fn huawei_mode(code: i64) -> PortMode {
    match code {
        0 => PortMode::NotL2,
        1 => PortMode::Trunk,
        2 => PortMode::Access,
        3 => PortMode::Hybrid,
        // 6 is "negotiate" in the configured column: the outcome is only in the operating one.
        // 4 and 5 (fabric, QinQ) are operating modes this build does not draw.
        _ => PortMode::Unknown,
    }
}

fn huawei(rows: &[SnmpInstanceRow]) -> VlanSnapshot {
    let ifindex = ints(rows, HW_PORT_IFINDEX);
    let configured = ints(rows, HW_PORT_TYPE);
    let operating = ints(rows, HW_PORT_ACTIVE_TYPE);
    let pvid = ints(rows, HW_PORT_PVID);
    let allow_lo = bytes(rows, HW_TRUNK_ALLOW_LOW);
    let allow_hi = bytes(rows, HW_TRUNK_ALLOW_HIGH);
    let tag_lo = bytes(rows, HW_HYBRID_TAGGED_LOW);
    let tag_hi = bytes(rows, HW_HYBRID_TAGGED_HIGH);
    let untag_lo = bytes(rows, HW_HYBRID_UNTAGGED_LOW);
    let untag_hi = bytes(rows, HW_HYBRID_UNTAGGED_HIGH);

    let mut ports: BTreeMap<u32, PortVlan> = BTreeMap::new();
    for (&port_num, &ifx) in &ifindex {
        let Ok(ifx) = u32::try_from(ifx) else {
            continue;
        };
        if ifx == 0 {
            continue;
        }
        let mode = match (operating.get(&port_num), configured.get(&port_num)) {
            (Some(&op), _) => huawei_mode(op),
            (None, Some(&cfg)) => huawei_mode(cfg),
            (None, None) => PortMode::Unknown,
        };
        let mut p = PortVlan::new(ifx, mode);
        let pvid = vlan_id(pvid.get(&port_num));
        match mode {
            PortMode::Trunk => {
                p.native = pvid;
                p.allowed = vlans_from_blocks(&[
                    (allow_lo.get(&port_num).copied(), 0),
                    (allow_hi.get(&port_num).copied(), 2048),
                ]);
            }
            PortMode::Access => p.access_vlan = pvid,
            PortMode::Hybrid => {
                p.native = pvid;
                p.tagged = vlans_from_blocks(&[
                    (tag_lo.get(&port_num).copied(), 0),
                    (tag_hi.get(&port_num).copied(), 2048),
                ]);
                p.untagged = vlans_from_blocks(&[
                    (untag_lo.get(&port_num).copied(), 0),
                    (untag_hi.get(&port_num).copied(), 2048),
                ]);
            }
            PortMode::NotL2 | PortMode::Unknown => {}
        }
        ports.insert(ifx, p);
    }

    // Eth-Trunk membership, joined through the trunk's ifIndex — never through its index.
    let trunk_ifindex: BTreeMap<u32, u32> = ints(rows, HW_TRUNK_IFINDEX)
        .into_iter()
        .filter_map(|(idx, ifx)| Some((idx, u32::try_from(ifx).ok().filter(|i| *i != 0)?)))
        .collect();
    for inst in column(rows, HW_TRUNK_MEMBER).keys() {
        if let [trunk_idx, member] = inst.as_slice() {
            if let Some(&lag) = trunk_ifindex.get(trunk_idx) {
                if *member == 0 || *member == lag {
                    continue;
                }
                ports
                    .entry(*member)
                    .or_insert_with(|| PortVlan::new(*member, PortMode::NotL2))
                    .lag_ifindex = Some(lag);
            }
        }
    }
    VlanSnapshot::new(ports.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn int(base: &str, inst: &[u32], v: i64) -> SnmpInstanceRow {
        SnmpInstanceRow {
            oid_base: base.to_owned(),
            instance: inst.to_vec(),
            value: SnmpValue::Int(v),
        }
    }
    fn oct(base: &str, inst: &[u32], b: Vec<u8>) -> SnmpInstanceRow {
        SnmpInstanceRow {
            oid_base: base.to_owned(),
            instance: inst.to_vec(),
            value: SnmpValue::Bytes(b),
        }
    }
    /// A bitmap of `len` bytes with the given VLANs set, counting from `base`.
    fn bitmap(len: usize, base: u16, vlans: &[u16]) -> Vec<u8> {
        let mut b = vec![0u8; len];
        for v in vlans {
            let off = usize::from(v - base);
            b[off / 8] |= 0x80 >> (off % 8);
        }
        b
    }
    fn all_first_block() -> Vec<u8> {
        let mut b = vec![0xffu8; 128];
        b[0] = 0x7f;
        b
    }

    /// The shape of a lab Catalyst 3850: an access port that also carries the trunk table's default
    /// row, a trunk whose upper blocks come back as empty strings, a configured trunk with its link
    /// down, a routed port in neither table, and a Port-channel with two members.
    fn catalyst_3850_rows() -> Vec<SnmpInstanceRow> {
        let mut r = vec![
            // ifIndex 7: access VLAN 100 + voice 200, with the trunk defaults beside it.
            int(CISCO_TRUNK_STATUS, &[7], 2),
            int(CISCO_TRUNK_STATE, &[7], 2),
            int(CISCO_TRUNK_NATIVE, &[7], 1),
            oct(CISCO_TRUNK_ENABLED_1K, &[7], all_first_block()),
            oct(CISCO_TRUNK_ENABLED_2K, &[7], vec![0xff; 128]),
            int(CISCO_VM_VLAN, &[7], 100),
            int(CISCO_VOICE_VLAN, &[7], 200),
            // ifIndex 11: trunking, native 1, allowed 20,68,70,90,200; upper blocks empty.
            int(CISCO_TRUNK_STATUS, &[11], 1),
            int(CISCO_TRUNK_STATE, &[11], 1),
            int(CISCO_TRUNK_NATIVE, &[11], 1),
            oct(
                CISCO_TRUNK_ENABLED_1K,
                &[11],
                bitmap(128, 0, &[20, 68, 70, 90, 200]),
            ),
            oct(CISCO_TRUNK_ENABLED_2K, &[11], Vec::new()),
            oct(CISCO_TRUNK_ENABLED_3K, &[11], Vec::new()),
            oct(CISCO_TRUNK_ENABLED_4K, &[11], Vec::new()),
            // ifIndex 23: configured trunk (on), link down — not trunking, no vmVlan row.
            int(CISCO_TRUNK_STATUS, &[23], 2),
            int(CISCO_TRUNK_STATE, &[23], 1),
            int(CISCO_TRUNK_NATIVE, &[23], 1),
            oct(CISCO_TRUNK_ENABLED_1K, &[23], bitmap(128, 0, &[20, 68])),
            // Port-channel 76 trunking with members 3 and 38; voice 4096 means none on 3.
            int(CISCO_TRUNK_STATUS, &[76], 1),
            int(CISCO_TRUNK_NATIVE, &[76], 1),
            oct(CISCO_TRUNK_ENABLED_1K, &[76], bitmap(128, 0, &[20])),
            int(CISCO_TRUNK_STATUS, &[3], 1),
            int(CISCO_TRUNK_NATIVE, &[3], 1),
            oct(CISCO_TRUNK_ENABLED_1K, &[3], bitmap(128, 0, &[20])),
            int(CISCO_VOICE_VLAN, &[3], 4096),
            int(CISCO_TRUNK_STATUS, &[38], 1),
            int(CISCO_TRUNK_NATIVE, &[38], 1),
            oct(CISCO_TRUNK_ENABLED_1K, &[38], bitmap(128, 0, &[20])),
        ];
        // ifStack: Po76 over 3 and 38, plus the ubiquitous 0-pairs and a routed port (13).
        for (u, l) in [(76, 3), (76, 38), (0, 13), (13, 0), (0, 76), (3, 0)] {
            r.push(int(IF_STACK_STATUS, &[u, l], 1));
        }
        r
    }

    #[test]
    fn a_catalyst_access_port_is_not_read_as_the_trunk_defaults_beside_it() {
        let s = cisco(&catalyst_3850_rows());
        let p = s.port(7).unwrap();
        assert_eq!(p.mode, PortMode::Access);
        assert_eq!((p.access_vlan, p.voice_vlan), (Some(100), Some(200)));
        assert!(p.allowed.is_empty() && p.native.is_none());
    }

    #[test]
    fn a_catalyst_trunk_reads_empty_upper_blocks_as_no_vlans() {
        let s = cisco(&catalyst_3850_rows());
        let p = s.port(11).unwrap();
        assert_eq!(p.mode, PortMode::Trunk);
        assert_eq!(p.native, Some(1));
        assert_eq!(
            p.allowed,
            vec![(20, 20), (68, 68), (70, 70), (90, 90), (200, 200)]
        );
    }

    #[test]
    fn a_configured_catalyst_trunk_with_its_link_down_is_still_a_trunk() {
        let p = cisco(&catalyst_3850_rows()).port(23).cloned().unwrap();
        assert_eq!(p.mode, PortMode::Trunk);
        assert_eq!(p.allowed, vec![(20, 20), (68, 68)]);
    }

    #[test]
    fn a_routed_catalyst_port_is_absent_and_port_channel_members_point_at_it() {
        let s = cisco(&catalyst_3850_rows());
        assert!(s.port(13).is_none(), "a routed port is in neither table");
        assert_eq!(s.port(3).unwrap().lag_ifindex, Some(76));
        assert_eq!(s.port(38).unwrap().lag_ifindex, Some(76));
        assert_eq!(s.port(76).unwrap().lag_ifindex, None);
        assert_eq!(s.port(3).unwrap().voice_vlan, None);
    }

    /// An old C3550 lists only its trunks in the trunk table; access ports live in vmVlan alone.
    #[test]
    fn a_c3550_with_only_trunks_in_its_trunk_table_reads_both_kinds() {
        let rows = vec![
            int(CISCO_TRUNK_STATUS, &[17], 1),
            int(CISCO_TRUNK_STATE, &[17], 1),
            int(CISCO_TRUNK_NATIVE, &[17], 16),
            oct(CISCO_TRUNK_ENABLED_1K, &[17], all_first_block()),
            oct(CISCO_TRUNK_ENABLED_2K, &[17], vec![0xff; 128]),
            oct(CISCO_TRUNK_ENABLED_3K, &[17], vec![0xff; 128]),
            oct(CISCO_TRUNK_ENABLED_4K, &[17], vec![0xff; 128]),
            int(CISCO_VM_VLAN, &[5], 11),
        ];
        let s = cisco(&rows);
        let t = s.port(17).unwrap();
        assert_eq!((t.mode, t.native), (PortMode::Trunk, Some(16)));
        assert_eq!(t.allowed, vec![(1, 4094)], "VLAN 4095 is reserved");
        assert_eq!(s.port(5).unwrap().access_vlan, Some(11));
    }

    /// The shape of a lab S5731 on VRP: the operating-mode column decides over a configured `6`, a
    /// trunk carries a PVID it does not allow (shown as configured, by decision), a hybrid port, and
    /// an Eth-Trunk whose index is not its number with two members across the stack.
    fn vrp_rows() -> Vec<SnmpInstanceRow> {
        vec![
            // port 1 → ifIndex 215 (Eth-Trunk0), trunk.
            int(HW_PORT_IFINDEX, &[1], 215),
            int(HW_PORT_TYPE, &[1], 1),
            int(HW_PORT_ACTIVE_TYPE, &[1], 1),
            int(HW_PORT_PVID, &[1], 1),
            oct(
                HW_TRUNK_ALLOW_LOW,
                &[1],
                bitmap(256, 0, &[700, 801, 802, 889]),
            ),
            oct(HW_TRUNK_ALLOW_HIGH, &[1], vec![0; 256]),
            // port 4 → ifIndex 7, configured negotiate (6), operating access, PVID 1.
            int(HW_PORT_IFINDEX, &[4], 7),
            int(HW_PORT_TYPE, &[4], 6),
            int(HW_PORT_ACTIVE_TYPE, &[4], 2),
            int(HW_PORT_PVID, &[4], 1),
            oct(HW_TRUNK_ALLOW_LOW, &[4], vec![0xff; 256]),
            // port 7 → ifIndex 10, access 875.
            int(HW_PORT_IFINDEX, &[7], 10),
            int(HW_PORT_TYPE, &[7], 2),
            int(HW_PORT_ACTIVE_TYPE, &[7], 2),
            int(HW_PORT_PVID, &[7], 875),
            // port 9 → ifIndex 12, hybrid untagged 1 tagged 100.
            int(HW_PORT_IFINDEX, &[9], 12),
            int(HW_PORT_TYPE, &[9], 3),
            int(HW_PORT_ACTIVE_TYPE, &[9], 3),
            int(HW_PORT_PVID, &[9], 1),
            oct(HW_HYBRID_TAGGED_LOW, &[9], bitmap(256, 0, &[100])),
            oct(HW_HYBRID_UNTAGGED_LOW, &[9], bitmap(256, 0, &[1])),
            // ports 20 and 21 → ifIndex 55 and 159, not L2 (Eth-Trunk members), and a stack port 22.
            int(HW_PORT_IFINDEX, &[20], 55),
            int(HW_PORT_TYPE, &[20], 0),
            int(HW_PORT_ACTIVE_TYPE, &[20], 0),
            int(HW_PORT_IFINDEX, &[21], 159),
            int(HW_PORT_TYPE, &[21], 0),
            int(HW_PORT_ACTIVE_TYPE, &[21], 0),
            int(HW_PORT_IFINDEX, &[22], 57),
            int(HW_PORT_TYPE, &[22], 0),
            int(HW_PORT_ACTIVE_TYPE, &[22], 0),
            // Eth-Trunk index 0 is ifIndex 215; its members are 55 and 159.
            int(HW_TRUNK_IFINDEX, &[0], 215),
            int(HW_TRUNK_MEMBER, &[0, 55], 1),
            int(HW_TRUNK_MEMBER, &[0, 159], 1),
        ]
    }

    #[test]
    fn a_vrp_trunk_keeps_its_configured_pvid_and_allowed_list() {
        let s = huawei(&vrp_rows());
        let p = s.port(215).unwrap();
        assert_eq!(p.mode, PortMode::Trunk);
        assert_eq!(p.native, Some(1));
        assert_eq!(p.allowed, vec![(700, 700), (801, 802), (889, 889)]);
    }

    #[test]
    fn the_operating_mode_wins_over_a_configured_negotiate() {
        let p = huawei(&vrp_rows()).port(7).cloned().unwrap();
        assert_eq!(p.mode, PortMode::Access);
        assert_eq!(p.access_vlan, Some(1));
        assert!(p.allowed.is_empty(), "an access port shows no trunk list");
    }

    #[test]
    fn a_hybrid_port_keeps_untagged_and_tagged_apart() {
        let p = huawei(&vrp_rows()).port(12).cloned().unwrap();
        assert_eq!(p.mode, PortMode::Hybrid);
        assert_eq!(
            (p.untagged.clone(), p.tagged.clone()),
            (vec![(1, 1)], vec![(100, 100)])
        );
    }

    #[test]
    fn eth_trunk_members_are_joined_by_ifindex_and_a_stack_port_is_not_one() {
        let s = huawei(&vrp_rows());
        assert_eq!(s.port(55).unwrap().lag_ifindex, Some(215));
        assert_eq!(s.port(159).unwrap().lag_ifindex, Some(215));
        let stack = s.port(57).unwrap();
        assert_eq!((stack.mode, stack.lag_ifindex), (PortMode::NotL2, None));
    }

    /// YunShan has no operating-mode column: the configured one decides, and a `6` there (never
    /// seen yet) is reported as unknown rather than guessed.
    #[test]
    fn yunshan_without_the_operating_column_reads_the_configured_one() {
        let rows = vec![
            int(HW_PORT_IFINDEX, &[5], 5),
            int(HW_PORT_TYPE, &[5], 1),
            int(HW_PORT_PVID, &[5], 1),
            oct(
                HW_TRUNK_ALLOW_LOW,
                &[5],
                bitmap(256, 0, &[100, 130, 150, 160]),
            ),
            int(HW_PORT_IFINDEX, &[7], 7),
            int(HW_PORT_TYPE, &[7], 2),
            int(HW_PORT_PVID, &[7], 100),
            int(HW_PORT_IFINDEX, &[8], 8),
            int(HW_PORT_TYPE, &[8], 6),
        ];
        let s = huawei(&rows);
        assert_eq!(s.port(5).unwrap().mode, PortMode::Trunk);
        assert_eq!(s.port(5).unwrap().allowed.len(), 4);
        assert_eq!(s.port(7).unwrap().access_vlan, Some(100));
        assert_eq!(s.port(8).unwrap().mode, PortMode::Unknown);
    }

    #[test]
    fn an_unknown_dialect_walks_and_reports_nothing() {
        assert!(columns(VlanDialect::Unknown).is_empty());
        assert!(assemble(VlanDialect::Unknown, &vrp_rows()).ports.is_empty());
        assert_eq!(columns(VlanDialect::Cisco).len(), 10);
        assert_eq!(columns(VlanDialect::Huawei).len(), 12);
    }
}
