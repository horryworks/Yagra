// SPDX-License-Identifier: AGPL-3.0-only
//! A switch port's VLAN membership — its mode, its native VLAN and the VLANs it carries (ADR-201).
//!
//! The unit of storage is the whole set per node, exactly like [`crate::L3Snapshot`]: a walk that
//! did not finish sends no snapshot at all (`PollResult.vlans = None`) and nothing is written, so a
//! half-read table can never read as "every port lost its VLANs". An **empty** snapshot is a real
//! observation and replaces the stored one.
//!
//! Every value here is the device's **configuration**, deliberately (user decision, 2026-10-09):
//! the native VLAN is the configured PVID even when that VLAN is not allowed on the trunk, and the
//! allowed list is what the operator wrote, including VLANs the device has not created. That is
//! what an operator compares against the CLI, and it is what both Cisco's trunk table and Huawei's
//! L2 interface table report.
//!
//! VLAN lists are kept as sorted, merged, inclusive ranges over 1..=4094 — VLAN 0 and 4095 are
//! reserved and never reported. "Every VLAN" is the single range `1..=4094`; there is no separate
//! "all" value to disagree with it.

use serde::{Deserialize, Serialize};

/// The lowest usable VLAN ID.
pub const VLAN_MIN: u16 = 1;
/// The highest usable VLAN ID (4095 is reserved by 802.1Q).
pub const VLAN_MAX: u16 = 4094;

/// One inclusive VLAN range, `[lo, hi]` on the wire.
pub type VlanRange = (u16, u16);

/// How a port forwards VLANs, as the device reports it.
///
/// A LAG member is **not** a mode: it is recorded as [`PortVlan::lag_ifindex`] beside whatever mode
/// the device gives the member (Cisco repeats the aggregate's trunk settings on every member,
/// Huawei reports the member as not switching on its own). Which of the two a surface shows is a
/// display decision, made once in core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortMode {
    /// One untagged VLAN (plus, on Cisco and Meraki, an optional voice VLAN).
    Access,
    /// A native VLAN plus a list of allowed, tagged VLANs.
    Trunk,
    /// Huawei only: separate untagged and tagged lists, more than one untagged VLAN allowed.
    Hybrid,
    /// The port does not switch: a routed port, a stack port, a LAG member on Huawei.
    NotL2,
    /// The device answered but in a way this build cannot place (Huawei's negotiated mode with no
    /// operating-mode column, or a value a newer core wrote). Shown as "not reported".
    #[serde(other)]
    Unknown,
}

/// One port's VLAN facts. Which fields are meaningful depends on `mode`; the rest stay empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortVlan {
    /// The port's `ifIndex` — the join key to the interface inventory.
    pub ifindex: u32,
    /// How the port forwards VLANs.
    pub mode: PortMode,
    /// Trunk and hybrid: the native VLAN (Huawei's PVID). `None` = the device reports none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<u16>,
    /// Access: the port's VLAN.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_vlan: Option<u16>,
    /// Access: the voice VLAN (Cisco, Meraki), when one is configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_vlan: Option<u16>,
    /// Trunk: the allowed VLANs, as configured.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<VlanRange>,
    /// Hybrid: the VLANs sent untagged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub untagged: Vec<VlanRange>,
    /// Hybrid: the VLANs sent tagged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tagged: Vec<VlanRange>,
    /// The `ifIndex` of the aggregate (Eth-Trunk, Port-channel) this port is a member of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lag_ifindex: Option<u32>,
}

impl PortVlan {
    /// A port with only its mode known; the caller fills in what that mode has.
    #[must_use]
    pub fn new(ifindex: u32, mode: PortMode) -> Self {
        Self {
            ifindex,
            mode,
            native: None,
            access_vlan: None,
            voice_vlan: None,
            allowed: Vec::new(),
            untagged: Vec::new(),
            tagged: Vec::new(),
            lag_ifindex: None,
        }
    }
}

/// Whether a VLAN the device defines is forwarding (ADR-257 decision 1). Only Cisco's VTP table
/// says; a Huawei name read from Q-BRIDGE carries no state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VlanState {
    /// `vtpVlanState` 1, operational.
    Active,
    /// `vtpVlanState` 2, suspended: defined but forwarding nothing.
    Suspended,
    /// Any other value (the two MTU states), or one a newer poller wrote.
    #[serde(other)]
    Unknown,
}

/// One VLAN the device defines, with the name it gives it (ADR-257).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VlanName {
    /// The VLAN ID, 1..=4094.
    pub id: u16,
    /// The name exactly as the device answered it, trimmed. Empty when the device has none.
    #[serde(default)]
    pub name: String,
    /// The VLAN's state, when the table says one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<VlanState>,
}

/// Every port's VLAN facts on one observation of one node.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VlanSnapshot {
    /// The ports, ordered by `ifIndex`. Always the whole set: a walk that hits its row bound sends
    /// no snapshot at all rather than a partial one.
    #[serde(default)]
    pub ports: Vec<PortVlan>,
    /// The VLANs the device defines, ordered by ID (ADR-257). `None` means the names were **not
    /// read** — a poller older than ADR-257, a make with no name table, or a name walk that did not
    /// finish. `Some(empty)` means the device was read and defines none. The difference is what
    /// lets a surface say "not defined" only when it actually looked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub names: Option<Vec<VlanName>>,
}

impl VlanSnapshot {
    /// Build a snapshot, ordering the ports by `ifIndex` and keeping the first of any duplicate.
    /// The names are not read; [`Self::with_names`] adds them.
    #[must_use]
    pub fn new(mut ports: Vec<PortVlan>) -> Self {
        ports.sort_by_key(|p| p.ifindex);
        ports.dedup_by_key(|p| p.ifindex);
        Self { ports, names: None }
    }

    /// The same snapshot with the device's VLAN names, ordered by ID with the first of any
    /// duplicate kept and IDs outside 1..=4094 dropped. `None` keeps "not read".
    #[must_use]
    pub fn with_names(mut self, names: Option<Vec<VlanName>>) -> Self {
        self.names = names.map(|mut n| {
            n.retain(|v| (VLAN_MIN..=VLAN_MAX).contains(&v.id));
            n.sort_by_key(|v| v.id);
            n.dedup_by_key(|v| v.id);
            n
        });
        self
    }

    /// The port with this `ifIndex`, if the snapshot has one.
    #[must_use]
    pub fn port(&self, ifindex: u32) -> Option<&PortVlan> {
        self.ports
            .binary_search_by_key(&ifindex, |p| p.ifindex)
            .ok()
            .map(|i| &self.ports[i])
    }
}

/// Which vendor's tables a VLAN walk reads (ADR-201 decisions 3 and 4).
///
/// Chosen by core from the node's recorded vendor, never guessed by the poller: the two
/// vendors' tables share no OID, and walking both on every device would cost each one a walk of a
/// subtree it does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VlanDialect {
    /// CISCO-VTP-MIB's trunk table, CISCO-VLAN-MEMBERSHIP-MIB, and `ifStackTable` for Port-channels.
    Cisco,
    /// HUAWEI-L2IF-MIB's port tables and HUAWEI-IF-EXT-MIB's Eth-Trunk membership.
    Huawei,
    /// A dialect a newer core named. The poller sends nothing for it rather than failing the spec.
    #[serde(other)]
    Unknown,
}

impl VlanDialect {
    /// The dialect for a device, from the maker identification recorded on the node (`"Cisco"`,
    /// `"Huawei"` — what `yagra_discovery::identify` and the classification rules write). The node
    /// row the scheduler's sweep holds carries the vendor, not the `sysObjectID`, so editing a
    /// node's vendor also changes which tables are walked. Any other maker gets no VLAN job, and its
    /// VLAN cells read "not reported".
    #[must_use]
    pub fn for_vendor(vendor: &str) -> Option<Self> {
        let v = vendor.trim().to_ascii_lowercase();
        if v.starts_with("cisco") {
            Some(Self::Cisco)
        } else if v.starts_with("huawei") {
            Some(Self::Huawei)
        } else {
            None
        }
    }
}

/// The VLAN a switch virtual interface routes for, read from its interface name (ADR-257 decision
/// 6): Cisco's `Vlan30` (ifDescr) and `Vl30` (ifName), Huawei's `Vlanif30`, and the
/// `Vlan-interface30` spelling of HPE Comware. Case and a space before the number are ignored.
///
/// `None` for anything else — including a VLAN ID outside 1..=4094 — so a name this does not
/// recognise leaves a gateway empty rather than attaching it to the wrong VLAN.
#[must_use]
pub fn svi_vlan(if_name: &str) -> Option<u16> {
    let lower = if_name.trim().to_ascii_lowercase();
    // Longest prefix first: `vl` is a prefix of all three others.
    let rest = ["vlan-interface", "vlanif", "vlan", "vl"]
        .iter()
        .find_map(|p| lower.strip_prefix(p))?;
    let digits = rest.trim_start();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) || digits.starts_with('0') {
        return None;
    }
    let id: u16 = digits.parse().ok()?;
    (VLAN_MIN..=VLAN_MAX).contains(&id).then_some(id)
}

/// Sorted, merged ranges from any list of VLAN IDs. IDs outside 1..=4094 are dropped.
#[must_use]
pub fn ranges_from_ids(ids: impl IntoIterator<Item = u16>) -> Vec<VlanRange> {
    let mut ids: Vec<u16> = ids
        .into_iter()
        .filter(|v| (VLAN_MIN..=VLAN_MAX).contains(v))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let mut out: Vec<VlanRange> = Vec::new();
    for v in ids {
        match out.last_mut() {
            Some((_, hi)) if *hi + 1 == v => *hi = v,
            _ => out.push((v, v)),
        }
    }
    out
}

/// The VLAN IDs set in an SNMP port-list style bitmap: the most significant bit of the first byte
/// is VLAN `base`, the next bit `base + 1`, and so on.
///
/// The bitmap's length is whatever the device returned. Cisco answers 128 bytes per 1024-VLAN block
/// but some IOS-XE trains answer the upper blocks with an **empty** string, which means "none of
/// these" (ADR-201 decision 3); Huawei answers 256 bytes per 2048. A short string is read as far as
/// it goes, so neither needs a special case.
#[must_use]
pub fn ids_from_bitmap(bytes: &[u8], base: u16) -> Vec<u16> {
    let mut out = Vec::new();
    for (i, byte) in bytes.iter().enumerate() {
        if *byte == 0 {
            continue;
        }
        for bit in 0..8u16 {
            if byte & (0x80 >> bit) != 0 {
                let offset = u32::try_from(i).unwrap_or(u32::MAX) * 8 + u32::from(bit);
                if let Ok(v) = u16::try_from(u32::from(base) + offset) {
                    out.push(v);
                }
            }
        }
    }
    out
}

/// Parse a VLAN list the way the Meraki Dashboard writes one: `"all"`, or comma-separated IDs and
/// `lo-hi` ranges (`"20,68,70"`, `"158-159,192"`). `None` for anything else, rather than a guess.
#[must_use]
pub fn parse_vlan_list(text: &str) -> Option<Vec<VlanRange>> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("all") {
        return Some(vec![(VLAN_MIN, VLAN_MAX)]);
    }
    if text.is_empty() {
        return Some(Vec::new());
    }
    let mut ids = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if let Some((lo, hi)) = part.split_once('-') {
            let lo: u16 = lo.trim().parse().ok()?;
            let hi: u16 = hi.trim().parse().ok()?;
            if lo > hi {
                return None;
            }
            ids.extend(lo.max(VLAN_MIN)..=hi.min(VLAN_MAX));
        } else {
            ids.push(part.parse().ok()?);
        }
    }
    Some(ranges_from_ids(ids))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bitmap_is_read_most_significant_bit_first_from_its_base() {
        // 0x08 in byte 12 is bit 100 of a 0-based block; 0x20 in byte 16 is 130.
        let mut b = vec![0u8; 32];
        b[12] = 0x08;
        b[16] = 0x20;
        assert_eq!(ids_from_bitmap(&b, 0), vec![100, 130]);
        assert_eq!(ids_from_bitmap(&b, 2048), vec![2148, 2178]);
    }

    #[test]
    fn an_empty_bitmap_means_no_vlans_in_its_block() {
        assert!(ids_from_bitmap(&[], 1024).is_empty());
    }

    #[test]
    fn a_full_first_block_without_vlan_zero_is_one_to_1023() {
        let mut b = vec![0xffu8; 128];
        b[0] = 0x7f;
        assert_eq!(ranges_from_ids(ids_from_bitmap(&b, 0)), vec![(1, 1023)]);
    }

    #[test]
    fn ids_merge_into_ranges_and_drop_the_reserved_ends() {
        assert_eq!(
            ranges_from_ids([0, 700, 801, 802, 803, 4095, 872, 803]),
            vec![(700, 700), (801, 803), (872, 872)]
        );
    }

    #[test]
    fn the_dashboards_lists_parse() {
        assert_eq!(parse_vlan_list("all"), Some(vec![(1, 4094)]));
        assert_eq!(
            parse_vlan_list("20,68,70"),
            Some(vec![(20, 20), (68, 68), (70, 70)])
        );
        assert_eq!(
            parse_vlan_list("158-159,192"),
            Some(vec![(158, 159), (192, 192)])
        );
        assert_eq!(parse_vlan_list("x"), None);
        assert_eq!(parse_vlan_list("9-3"), None);
    }

    #[test]
    fn the_dialect_is_the_recorded_vendor() {
        assert_eq!(VlanDialect::for_vendor("Huawei"), Some(VlanDialect::Huawei));
        assert_eq!(VlanDialect::for_vendor(" cisco "), Some(VlanDialect::Cisco));
        assert_eq!(VlanDialect::for_vendor("Juniper"), None);
    }

    #[test]
    fn a_mode_or_dialect_this_build_does_not_know_still_decodes() {
        let m: PortMode = serde_json::from_str(r#""private_vlan""#).unwrap();
        assert_eq!(m, PortMode::Unknown);
        let d: VlanDialect = serde_json::from_str(r#""juniper""#).unwrap();
        assert_eq!(d, VlanDialect::Unknown);
        let p: PortVlan =
            serde_json::from_str(r#"{"ifindex":3,"mode":"trunk","future":1}"#).unwrap();
        assert!(p.allowed.is_empty() && p.native.is_none());
    }

    #[test]
    fn an_svi_name_gives_its_vlan_in_every_spelling() {
        assert_eq!(svi_vlan("Vlan30"), Some(30));
        assert_eq!(svi_vlan("Vl30"), Some(30));
        assert_eq!(svi_vlan("Vlanif110"), Some(110));
        assert_eq!(svi_vlan("VLANIF4094"), Some(4094));
        assert_eq!(svi_vlan("Vlan-interface20"), Some(20));
        assert_eq!(svi_vlan("Vlan 20"), Some(20));
        assert_eq!(svi_vlan(" vlan1 "), Some(1));
    }

    #[test]
    fn a_name_that_is_not_an_svi_gives_no_vlan() {
        for n in [
            "GigabitEthernet1/0/1",
            "Vlan",
            "Vlan0",
            "Vlan4095",
            "Vlan030",
            "Vlan30.1",
            "Vlanif-30",
            "Virtual-Template1",
            "Vlan30a",
            "",
        ] {
            assert_eq!(svi_vlan(n), None, "{n}");
        }
    }

    #[test]
    fn names_are_ordered_deduplicated_and_kept_in_range() {
        let n = |id, name: &str| VlanName {
            id,
            name: name.into(),
            state: None,
        };
        let s = VlanSnapshot::new(Vec::new()).with_names(Some(vec![
            n(30, "b"),
            n(0, "x"),
            n(10, "a"),
            n(30, "c"),
            n(4095, "y"),
        ]));
        let got: Vec<_> = s
            .names
            .unwrap()
            .into_iter()
            .map(|v| (v.id, v.name))
            .collect();
        assert_eq!(got, vec![(10, "a".into()), (30, "b".into())]);
        assert_eq!(VlanSnapshot::new(Vec::new()).with_names(None).names, None);
    }

    #[test]
    fn names_not_read_and_names_read_empty_survive_the_wire_apart() {
        // A snapshot from a poller older than ADR-257 has no `names` key: not read.
        let old: VlanSnapshot = serde_json::from_str(r#"{"ports":[]}"#).unwrap();
        assert_eq!(old.names, None);
        // Not read is not written, so an N-1 core reads the same document it always did.
        assert!(!serde_json::to_string(&old).unwrap().contains("names"));
        // Read and empty is a different answer and must round-trip as one.
        let empty = VlanSnapshot::new(Vec::new()).with_names(Some(Vec::new()));
        let back: VlanSnapshot =
            serde_json::from_str(&serde_json::to_string(&empty).unwrap()).unwrap();
        assert_eq!(back.names, Some(Vec::new()));
        // A state this build does not know still decodes, and a future field is ignored.
        let v: VlanName =
            serde_json::from_str(r#"{"id":5,"name":"x","state":"mtu_too_big","more":1}"#).unwrap();
        assert_eq!(v.state, Some(VlanState::Unknown));
    }

    #[test]
    fn a_snapshot_is_ordered_and_looked_up_by_ifindex() {
        let s = VlanSnapshot::new(vec![
            PortVlan::new(9, PortMode::Access),
            PortVlan::new(3, PortMode::Trunk),
            PortVlan::new(9, PortMode::Trunk),
        ]);
        assert_eq!(s.ports.len(), 2);
        assert_eq!(s.port(3).map(|p| p.mode), Some(PortMode::Trunk));
        assert!(s.port(4).is_none());
    }
}
