// SPDX-License-Identifier: AGPL-3.0-only
//! A Meraki switch's LLDP/CDP neighbours, from `switch/ports/topology/discovery/byDevice`
//! (ADR-181) — the organization-wide listing that tells, per switch port, what the port hears —
//! and an MX's or an MR's, from `devices/{serial}/lldpCdp`, one device at a time (ADR-181 Inc.3,
//! Inc.5).
//!
//! The two answer the same facts in two shapes: the listing as `{name, value}` pairs labelled
//! the way the Dashboard displays them, the per-device read as camelCase fields. Both are read into
//! one [`Fields`] keyed by the listing's labels, so a row is built by one function per protocol
//! whichever endpoint it came from.
//!
//! Each port carries two lists of `{name, value}` pairs, one per protocol, labelled the way the
//! Dashboard displays them ("System name", "Port ID", …). This module is the one place those labels
//! are read, so a label the Dashboard renames costs one line here and one fixture.
//!
//! Measured on a real organization (2026-09-26, 854 switches, 3,841 ports with neighbours): every
//! port id was a plain decimal, no port listed two LLDP chassis, and the oldest `lastUpdatedAt` was
//! just under 24 hours — so a neighbour unplugged stays listed for up to a day. That age is not
//! filtered here (ADR-181 decision 5): the Dashboard shows the same rows.
//!
//! 🚨 **The Dashboard's LLDP "System capabilities" do not name the IEEE roles a switch or router
//! advertises.** On that organization they were "S-VLAN Component of a VLAN Bridge" and "Two-port
//! MAC Relay" on almost every row — values an 802.1AB peer only sets for provider bridges and
//! TPMRs, and which look like the Dashboard reading the bitmap byte-swapped. Guessing the intended
//! role from them would print "bridge" on rows nobody measured, so only the literal IEEE names are
//! read and those two are dropped: an LLDP row from Meraki usually has no capabilities (ADR-181
//! decision 10). CDP's list is ordinary text ("Router, Switch") and is read as such.

use std::collections::BTreeMap;
use std::net::IpAddr;

use serde_json::Value;
use yagra_common::{
    meraki_port_name, parse_mac, render_mac, switch_port_ifindex, Neighbor, NeighborCapability,
    NeighborIdKind, NeighborProto,
};

/// The format every set built from these rows carries (ADR-182) — MS from the organization-wide
/// listing and MX/MR from the per-device read alike, because both are read by this module. Raise it
/// whenever a field here starts to read differently for the same cabling: core then records the
/// next read as a change of spelling, not of adjacency. The fingerprint test at the bottom of this
/// file fails until you do.
///
/// `1`: ADR-181 Inc.4 — local ports read "Port 7", a CDP peer's bare-hex id is a MAC, a lone
/// CDP version `1` is dropped, and a port's LLDP and CDP rows lend each other name and roles.
pub const MERAKI_NEIGHBOR_FORMAT: u32 = 1;

/// The largest page `switch/ports/topology/discovery/byDevice` accepts: "The perPage parameter must
/// be between 3 and 20" (measured 2026-09-26 with 50, 100 and 1000). 854 switches took 43 pages and
/// 25 s.
pub(crate) const SWITCH_PORT_TOPOLOGY_PER_PAGE: u32 = 20;

/// The labels read out of a port's `lldp` list.
mod lldp {
    pub const CHASSIS_ID: &str = "Chassis ID";
    pub const PORT_ID: &str = "Port ID";
    pub const PORT_DESCRIPTION: &str = "Port description";
    pub const SYSTEM_NAME: &str = "System name";
    pub const SYSTEM_DESCRIPTION: &str = "System description";
    pub const MANAGEMENT_ADDRESS: &str = "Management address";
    pub const CAPABILITIES: &str = "System capabilities";
}

/// The labels read out of a port's `cdp` list.
mod cdp {
    pub const DEVICE_ID: &str = "Device ID";
    pub const PORT_ID: &str = "Port ID";
    pub const PLATFORM: &str = "Platform";
    pub const VERSION: &str = "Version";
    pub const SYSTEM_NAME: &str = "System name";
    /// Preferred over [`ADDRESS`] when both are listed: it is the address the peer offers for
    /// managing it, which is what an inventory node is registered under.
    pub const MANAGEMENT_ADDRESS: &str = "Management address";
    pub const ADDRESS: &str = "Address";
    pub const CAPABILITIES: &str = "Capabilities";
}

/// A capability's display name (lowercased) → the role it stands for. The IEEE 802.1AB names and
/// CDP's; a name missing here is dropped, never guessed (see the module doc for the two LLDP names
/// that are dropped on purpose).
const CAPABILITY_NAMES: &[(&str, NeighborCapability)] = &[
    // CDP
    ("router", NeighborCapability::Router),
    ("transparent bridge", NeighborCapability::Bridge),
    ("source route bridge", NeighborCapability::Bridge),
    ("switch", NeighborCapability::Switch),
    ("host", NeighborCapability::Host),
    ("igmp conditional filtering", NeighborCapability::Igmp),
    ("repeater", NeighborCapability::Repeater),
    ("voip phone", NeighborCapability::Phone),
    // LLDP (IEEE 802.1AB) — "router" and "repeater" are shared with CDP above
    ("other", NeighborCapability::Other),
    ("bridge", NeighborCapability::Bridge),
    ("mac bridge", NeighborCapability::Bridge),
    ("wlan access point", NeighborCapability::WlanAp),
    ("telephone", NeighborCapability::Phone),
    ("docsis cable device", NeighborCapability::CableDevice),
    ("station only", NeighborCapability::Host),
];

/// `switch/ports/topology/discovery/byDevice` → each listed switch's neighbours, by serial.
///
/// A switch the listing names gets an entry even when none of its ports yields a neighbour: it was
/// read, and it hears nothing. A switch the listing does not name gets none — the caller leaves its
/// stored neighbours alone (ADR-181 decision 4).
pub(crate) fn parse_switch_port_topology(items: &[Value]) -> BTreeMap<String, Vec<Neighbor>> {
    let mut out: BTreeMap<String, Vec<Neighbor>> = BTreeMap::new();
    for item in items {
        let Some(serial) = item.get("serial").and_then(Value::as_str) else {
            continue;
        };
        let neighbors = out.entry(serial.to_owned()).or_default();
        for port in item
            .get("ports")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(port_id) = port
                .get("portId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|p| !p.is_empty())
            else {
                continue;
            };
            let local = Local::switch_port(port_id);
            neighbors.extend(port_neighbors(
                local,
                &Fields::of(port.get("lldp")),
                &Fields::of(port.get("cdp")),
            ));
        }
    }
    out
}

/// A per-device read's LLDP fields → the listing's labels.
const LLDP_FIELDS: &[(&str, &str)] = &[
    ("chassisId", lldp::CHASSIS_ID),
    ("portId", lldp::PORT_ID),
    ("portDescription", lldp::PORT_DESCRIPTION),
    ("systemName", lldp::SYSTEM_NAME),
    ("systemDescription", lldp::SYSTEM_DESCRIPTION),
    ("managementAddress", lldp::MANAGEMENT_ADDRESS),
    ("systemCapabilities", lldp::CAPABILITIES),
];

/// A per-device read's CDP fields → the listing's labels.
const CDP_FIELDS: &[(&str, &str)] = &[
    ("deviceId", cdp::DEVICE_ID),
    ("portId", cdp::PORT_ID),
    ("platform", cdp::PLATFORM),
    ("version", cdp::VERSION),
    ("managementAddress", cdp::MANAGEMENT_ADDRESS),
    ("address", cdp::ADDRESS),
    ("capabilities", cdp::CAPABILITIES),
];

/// `devices/{serial}/lldpCdp` for an MX → its LAN-side neighbours (ADR-181 Inc.3), and for an MR →
/// the peer on its wired port (Inc.5; every recorded MR names that port `wired0`).
///
/// The body is `{"sourceMac": …, "ports": {"port3": {"lldp": {…}, "cdp": {…}, …}, "wan1": …}}`
/// (recorded 2026-09-26 on ten MX). A `wan` port's peer is dropped (decision 3): it is the upstream
/// line — measured, 11 of 14 were another Meraki device's internet port and the rest a carrier's
/// equipment — and listing it would put a carrier's router on Discovery's Unregistered list. An
/// empty or absent `ports` is an answer: the MX hears nothing.
pub(crate) fn parse_device_lldp_cdp(body: &Value) -> Vec<Neighbor> {
    let mut out = Vec::new();
    for (key, port) in body
        .get("ports")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let key = key.trim();
        if key.is_empty() || key.to_ascii_lowercase().starts_with("wan") {
            continue;
        }
        let local = Local {
            port: key,
            ifindex: None,
        };
        out.extend(port_neighbors(
            local,
            &Fields::of_object(port.get("lldp"), LLDP_FIELDS),
            &Fields::of_object(port.get("cdp"), CDP_FIELDS),
        ));
    }
    out
}

/// One protocol's `{name, value}` list, first occurrence of each name, blanks dropped.
struct Fields<'a>(BTreeMap<&'a str, &'a str>);

impl<'a> Fields<'a> {
    fn of(list: Option<&'a Value>) -> Self {
        let mut map = BTreeMap::new();
        for pair in list.and_then(Value::as_array).into_iter().flatten() {
            let (Some(name), Some(value)) = (
                pair.get("name").and_then(Value::as_str),
                pair.get("value").and_then(Value::as_str),
            ) else {
                continue;
            };
            let value = value.trim();
            if !value.is_empty() {
                map.entry(name.trim()).or_insert(value);
            }
        }
        Self(map)
    }

    /// A per-device read's protocol object (`lldp` or `cdp`), each camelCase field put under the
    /// label the organization-wide listing uses for it. A field this module does not name, or one
    /// that is not a string (`nativeVlan` is a number), is not read.
    fn of_object(object: Option<&'a Value>, names: &[(&str, &'a str)]) -> Self {
        let mut map = BTreeMap::new();
        if let Some(object) = object.and_then(Value::as_object) {
            for (field, label) in names {
                if let Some(value) = object.get(*field).and_then(Value::as_str).map(str::trim) {
                    if !value.is_empty() {
                        map.insert(*label, value);
                    }
                }
            }
        }
        Self(map)
    }

    fn get(&self, name: &str) -> Option<&'a str> {
        self.0.get(name).copied()
    }

    fn owned(&self, name: &str) -> Option<String> {
        self.get(name).map(str::to_owned)
    }
}

/// The port a neighbour is heard on.
#[derive(Clone, Copy)]
struct Local<'a> {
    /// The Dashboard's name for it: a switch port's id (`7`), an MX port's key (`port3`), an MR's
    /// (`wired0`).
    port: &'a str,
    /// The ifIndex the Interfaces tab keys the port by — a switch's (ADR-181 decision 8, ADR-167
    /// decision 4). An MX has no Interfaces rows, so none (Inc.3 decision 6); an MR's are its radios, so
    /// none either (Inc.5 decision 4).
    ifindex: Option<u32>,
}

impl<'a> Local<'a> {
    fn switch_port(port: &'a str) -> Self {
        Self {
            port,
            ifindex: Some(switch_port_ifindex(port)),
        }
    }
}

/// One port's LLDP and CDP rows, each filled from the other where it is blank (ADR-181 Inc.4
/// decision 1). A Meraki peer is heard both ways: its LLDP row names it and its CDP row does not, and
/// its CDP row says what it is while the Dashboard's LLDP capabilities are unreadable (decision 10). So
/// a CDP row with no name takes the LLDP row's, and an LLDP row with no capabilities takes the CDP
/// row's — only when both rows name the same chassis, i.e. the same peer. Measured on a lab copy of
/// a real organization, that filled 560 of 638 CDP names and 560 of 854 LLDP capability lists.
fn port_neighbors(local: Local<'_>, lldp: &Fields<'_>, cdp: &Fields<'_>) -> Vec<Neighbor> {
    let mut lldp = lldp_neighbor(local, lldp);
    let mut cdp = cdp_neighbor(local, cdp);
    if let (Some(l), Some(c)) = (lldp.as_mut(), cdp.as_mut()) {
        fill_from_sibling(l, c);
    }
    lldp.into_iter().chain(cdp).collect()
}

/// What each row of one peer lacks, from the other row. Nothing when the chassis ids differ.
fn fill_from_sibling(lldp: &mut Neighbor, cdp: &mut Neighbor) {
    if lldp.remote_chassis != cdp.remote_chassis {
        return;
    }
    if cdp.remote_sys_name.is_none() {
        cdp.remote_sys_name.clone_from(&lldp.remote_sys_name);
    }
    if lldp.capabilities.is_empty() {
        lldp.capabilities.clone_from(&cdp.capabilities);
    }
}

/// The local side both protocols share. The port reads as the Dashboard shows it, `Port 7`
/// (ADR-181 Inc.4 decision 3); the ifindex stays the raw id's.
fn on_port(proto: NeighborProto, local: Local<'_>, chassis: String, port: String) -> Neighbor {
    let mut n = Neighbor::new(proto, meraki_port_name(local.port), chassis, port);
    n.local_ifindex = local.ifindex;
    n
}

fn lldp_neighbor(local: Local<'_>, f: &Fields<'_>) -> Option<Neighbor> {
    let (chassis, chassis_kind) = id_with_kind(f.get(lldp::CHASSIS_ID)?);
    let (port, port_kind) = f.get(lldp::PORT_ID).map_or((String::new(), None), |p| {
        let (p, k) = id_with_kind(p);
        (p, Some(k))
    });
    let mut n = on_port(NeighborProto::Lldp, local, chassis, port);
    n.remote_chassis_kind = Some(chassis_kind);
    n.remote_port_kind = port_kind;
    n.remote_port_desc = f.owned(lldp::PORT_DESCRIPTION);
    n.remote_sys_name = f.owned(lldp::SYSTEM_NAME);
    n.remote_sys_desc = f.owned(lldp::SYSTEM_DESCRIPTION);
    n.remote_mgmt_addr = f.get(lldp::MANAGEMENT_ADDRESS).and_then(first_address);
    n.capabilities = f
        .get(lldp::CAPABILITIES)
        .map(capabilities)
        .unwrap_or_default();
    Some(n)
}

fn cdp_neighbor(local: Local<'_>, f: &Fields<'_>) -> Option<Neighbor> {
    let (device_id, device_id_kind) = cdp_device_id(f.get(cdp::DEVICE_ID)?);
    let port = f.owned(cdp::PORT_ID).unwrap_or_default();
    let mut n = on_port(NeighborProto::Cdp, local, device_id, port);
    n.remote_chassis_kind = Some(device_id_kind);
    // CDP names its peer's port by name — text, as the SNMP walk records it.
    n.remote_port_kind = (!n.remote_port.is_empty()).then_some(NeighborIdKind::Text);
    n.remote_platform = f.owned(cdp::PLATFORM);
    // CDP's counterpart of an LLDP system description, as on the SNMP walk (ADR-180 decision 6).
    n.remote_sys_desc = f.get(cdp::VERSION).and_then(cdp_version);
    n.remote_sys_name = f.owned(cdp::SYSTEM_NAME);
    n.remote_mgmt_addr = f
        .get(cdp::MANAGEMENT_ADDRESS)
        .and_then(first_address)
        .or_else(|| f.get(cdp::ADDRESS).and_then(first_address));
    n.capabilities = f
        .get(cdp::CAPABILITIES)
        .map(capabilities)
        .unwrap_or_default();
    Some(n)
}

/// An id as the Dashboard rendered it, and what it is: a MAC — in either separator, either case —
/// is rewritten the way the SNMP walk renders one, so the same peer seen both ways is one chassis
/// and core can look its maker up (ADR-180 decision 4). Anything else is text.
fn id_with_kind(raw: &str) -> (String, NeighborIdKind) {
    match parse_mac(raw).and_then(|m| render_mac(&m)) {
        Some(mac) => (mac, NeighborIdKind::Mac),
        None => (raw.to_owned(), NeighborIdKind::Text),
    }
}

/// A MAC in any spelling the Dashboard uses — six `:`/`-`-separated octets or twelve bare hex
/// digits, either case — rendered the way a neighbour row carries one, so a device's `mac` and a
/// neighbour's chassis id compare as text (ADR-180 Inc.3).
pub(crate) fn canonical_mac(raw: &str) -> Option<String> {
    let raw = raw.trim();
    parse_mac(raw)
        .or_else(|| bare_hex_mac(raw))
        .and_then(|m| render_mac(&m))
}

/// A CDP device id as the Dashboard rendered it, and what it is (ADR-181 Inc.2 decision B). A Meraki
/// peer names itself by its MAC as twelve bare hex digits (`0c8ddb000002`) — measured on a real
/// organization, 2,571 of 2,873 CDP rows, every one a Meraki device or a Cisco CBS — so those are
/// read as a MAC too, which gives the row the same chassis as the peer's LLDP row and a maker
/// name. Anything else is what [`id_with_kind`] makes of it: a CDP device id is usually a name.
fn cdp_device_id(raw: &str) -> (String, NeighborIdKind) {
    match bare_hex_mac(raw).and_then(|m| render_mac(&m)) {
        Some(mac) => (mac, NeighborIdKind::Mac),
        None => id_with_kind(raw),
    }
}

/// Twelve hex digits and nothing else. `None` for anything else — a name that happens to be
/// twelve characters of `0-9a-f` is the one case read wrongly, and the cost is a maker name beside
/// it (display only, ADR-180 decision 5).
fn bare_hex_mac(raw: &str) -> Option<[u8; 6]> {
    if raw.len() != 12 || !raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 6];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&raw[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// A CDP version string worth showing, or `None` (ADR-181 Inc.2 decision A). A Meraki peer answers
/// `"1"` — measured on a real organization, 2,569 of 2,873 CDP rows, every one a Meraki peer, and
/// no other value of one or two characters — which put a lone "1" under the model on the
/// Neighbors tab. A version that says something (an IOS banner, `SCCP 9.4.1.3.SR3`) is kept.
fn cdp_version(raw: &str) -> Option<String> {
    let meaningless = raw.len() <= 2 && raw.bytes().all(|b| b.is_ascii_digit());
    (!meaningless).then(|| raw.to_owned())
}

/// The first IPv4 or IPv6 address in a value, as core compares it. A value may list several;
/// measured, a few CDP "Address" values were not a lone address.
fn first_address(raw: &str) -> Option<String> {
    raw.split([',', ';', ' '])
        .find_map(|t| t.trim().parse::<IpAddr>().ok())
        .map(|ip| ip.to_string())
}

/// A comma-separated list of capability names → the roles this build knows, sorted and deduped.
fn capabilities(raw: &str) -> Vec<NeighborCapability> {
    let mut out: Vec<NeighborCapability> = raw
        .split(',')
        .filter_map(|name| {
            let name = name.trim().to_ascii_lowercase();
            CAPABILITY_NAMES
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, c)| *c)
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::NeighborSet;

    /// One switch in the shape the Dashboard answers (field names and value shapes as recorded
    /// 2026-09-26; every value is made up).
    fn row() -> Value {
        serde_json::json!({
            "serial": "Q2SW-0001",
            "name": "sw-01",
            "mac": "00:18:0a:00:00:01",
            "network": {"id": "N_1", "name": "site-a"},
            "model": "MS120-8",
            "ports": [
                {
                    "portId": "7",
                    "lastUpdatedAt": "2026-09-26T00:00:00Z",
                    "lldp": [
                        {"name": "System name", "value": "ap-01"},
                        {"name": "System description", "value": "Meraki MR36 Cloud Managed AP"},
                        {"name": "Port ID", "value": "0"},
                        {"name": "Chassis ID", "value": "0C-8D-DB-00-00-02"},
                        {"name": "Management VLAN", "value": "10"},
                        {"name": "Port VLAN", "value": "10"},
                        {"name": "Management address", "value": "192.0.2.21"},
                        {"name": "Port description", "value": "eth0"},
                        {"name": "System capabilities", "value": "S-VLAN Component of a VLAN Bridge, Two-port MAC Relay"}
                    ],
                    "cdp": [
                        {"name": "Platform", "value": "Meraki MR36 Cloud Managed AP"},
                        {"name": "Device ID", "value": "0c8ddb000002"},
                        {"name": "Port ID", "value": "Port 0"},
                        {"name": "Native VLAN", "value": "10"},
                        {"name": "Address", "value": "192.0.2.21"},
                        {"name": "Version", "value": "1"},
                        {"name": "Capabilities", "value": "Router, Switch, IGMP conditional filtering"}
                    ]
                },
                {
                    "portId": "8",
                    "lastUpdatedAt": "2026-09-26T00:00:00Z",
                    "lldp": [
                        {"name": "Chassis ID", "value": "core-sw"},
                        {"name": "Port ID", "value": "Gi1/0/8"},
                        {"name": "System capabilities", "value": ""}
                    ],
                    "cdp": []
                }
            ]
        })
    }

    fn parsed() -> BTreeMap<String, Vec<Neighbor>> {
        parse_switch_port_topology(&[row()])
    }

    #[test]
    fn an_lldp_row_reads_every_label_and_renders_the_mac_as_the_walk_does() {
        let got = parsed();
        let n = got["Q2SW-0001"]
            .iter()
            .find(|n| n.proto == NeighborProto::Lldp && n.local_port == "Port 7")
            .expect("the LLDP row on port 7");
        assert_eq!(n.local_ifindex, Some(7));
        assert_eq!(n.remote_chassis, "0c:8d:db:00:00:02");
        assert_eq!(n.remote_chassis_kind, Some(NeighborIdKind::Mac));
        assert_eq!(n.remote_port, "0");
        assert_eq!(n.remote_port_kind, Some(NeighborIdKind::Text));
        assert_eq!(n.remote_sys_name.as_deref(), Some("ap-01"));
        assert_eq!(
            n.remote_sys_desc.as_deref(),
            Some("Meraki MR36 Cloud Managed AP")
        );
        assert_eq!(n.remote_port_desc.as_deref(), Some("eth0"));
        assert_eq!(n.remote_mgmt_addr.as_deref(), Some("192.0.2.21"));
    }

    /// decision 10: the two names the Dashboard puts on nearly every LLDP row are not guessed into a role.
    /// A row with no CDP sibling (port 8) is left with none.
    #[test]
    fn the_dashboards_lldp_capability_names_are_not_guessed_into_roles() {
        assert!(capabilities("S-VLAN Component of a VLAN Bridge, Two-port MAC Relay").is_empty());
        let got = parsed();
        let eight = got["Q2SW-0001"]
            .iter()
            .find(|n| n.proto == NeighborProto::Lldp && n.local_port == "Port 8")
            .expect("port 8");
        assert!(eight.capabilities.is_empty());
        assert_eq!(
            capabilities("Bridge, Router, Station Only"),
            [
                NeighborCapability::Router,
                NeighborCapability::Bridge,
                NeighborCapability::Host
            ]
        );
    }

    #[test]
    fn a_cdp_row_reads_its_text_capabilities_and_falls_back_to_the_plain_address() {
        let got = parsed();
        let n = got["Q2SW-0001"]
            .iter()
            .find(|n| n.proto == NeighborProto::Cdp)
            .expect("the CDP row");
        assert_eq!(n.local_port, "Port 7");
        assert_eq!(n.remote_port, "Port 0");
        assert_eq!(
            n.remote_platform.as_deref(),
            Some("Meraki MR36 Cloud Managed AP")
        );
        assert_eq!(n.remote_mgmt_addr.as_deref(), Some("192.0.2.21"));
        assert_eq!(
            n.capabilities,
            [
                NeighborCapability::Router,
                NeighborCapability::Switch,
                NeighborCapability::Igmp
            ]
        );
    }

    /// Inc.2 decision B: a Meraki peer's bare-hex CDP device id is its MAC — the same chassis its LLDP
    /// row on the same port carries.
    #[test]
    fn a_meraki_peers_bare_hex_device_id_is_read_as_the_mac_its_lldp_row_names() {
        let got = parsed();
        let rows = &got["Q2SW-0001"];
        let cdp = rows.iter().find(|n| n.proto == NeighborProto::Cdp).unwrap();
        let lldp = rows
            .iter()
            .find(|n| n.proto == NeighborProto::Lldp && n.local_port == "Port 7")
            .unwrap();
        assert_eq!(cdp.remote_chassis, "0c:8d:db:00:00:02");
        assert_eq!(cdp.remote_chassis_kind, Some(NeighborIdKind::Mac));
        assert_eq!(cdp.remote_chassis, lldp.remote_chassis);
        // A name stays a name, and so does anything that is not exactly twelve hex digits.
        for (raw, kind) in [
            ("rtr-01.example.com", NeighborIdKind::Text),
            ("0c8ddb00000", NeighborIdKind::Text),
            ("0c8ddb0000020", NeighborIdKind::Text),
            ("0c8ddb00000g", NeighborIdKind::Text),
            ("0C-8D-DB-00-00-02", NeighborIdKind::Mac),
        ] {
            assert_eq!(cdp_device_id(raw).1, kind, "{raw}");
        }
    }

    /// ADR-180 Inc.3: a device's `mac` and a neighbour's chassis compare as text only if both are
    /// rendered one way, whatever spelling each arrived in.
    #[test]
    fn every_mac_spelling_the_dashboard_uses_renders_as_a_neighbour_row_carries_it() {
        for raw in [
            "0c:8d:db:00:00:02",
            "0C-8D-DB-00-00-02",
            "0c8ddb000002",
            " 0C8DDB000002 ",
        ] {
            assert_eq!(
                canonical_mac(raw).as_deref(),
                Some("0c:8d:db:00:00:02"),
                "{raw}"
            );
        }
        for raw in ["", "sw-01", "0c:8d:db:00:00", "0c8ddb00000g"] {
            assert_eq!(canonical_mac(raw), None, "{raw}");
        }
    }

    /// Inc.4 decision 1: one peer's two rows lend each other what each lacks — the CDP row its name,
    /// the LLDP row its capabilities — and a port reads as the Dashboard shows it.
    #[test]
    fn one_peers_lldp_and_cdp_rows_fill_each_others_blanks() {
        let got = parsed();
        let rows = &got["Q2SW-0001"];
        let on = |proto| {
            rows.iter()
                .find(|n| n.proto == proto && n.local_port == "Port 7")
                .expect("port 7's row")
        };
        let (lldp, cdp) = (on(NeighborProto::Lldp), on(NeighborProto::Cdp));
        assert_eq!(cdp.remote_sys_name.as_deref(), Some("ap-01"));
        assert_eq!(lldp.capabilities, cdp.capabilities);
        assert_eq!(
            lldp.capabilities,
            [
                NeighborCapability::Router,
                NeighborCapability::Switch,
                NeighborCapability::Igmp
            ]
        );
        assert_eq!(lldp.local_ifindex, Some(7));
    }

    /// Two rows on one port that name different chassis are two peers: neither lends the other
    /// anything.
    #[test]
    fn rows_naming_different_chassis_lend_nothing() {
        let rows = parse_device_lldp_cdp(&mx_body());
        let on = |proto| {
            rows.iter()
                .find(|n| n.proto == proto && n.local_port == "Port 3")
                .expect("port3's row")
        };
        assert_eq!(on(NeighborProto::Cdp).remote_sys_name, None);
        assert_eq!(
            on(NeighborProto::Lldp).capabilities,
            [NeighborCapability::Router, NeighborCapability::Bridge]
        );
    }

    /// Inc.2 decision A: a Meraki peer's `"1"` is not shown; a version that says something is.
    #[test]
    fn a_cdp_version_of_one_or_two_digits_is_dropped_and_a_real_one_kept() {
        let got = parsed();
        let n = got["Q2SW-0001"]
            .iter()
            .find(|n| n.proto == NeighborProto::Cdp)
            .unwrap();
        assert_eq!(n.remote_sys_desc, None);
        for kept in ["SCCP 9.4.1.3.SR3", "3.2.1.1", "123", "v1"] {
            assert_eq!(cdp_version(kept).as_deref(), Some(kept));
        }
        for dropped in ["1", "12"] {
            assert_eq!(cdp_version(dropped), None);
        }
    }

    #[test]
    fn a_cdp_management_address_wins_over_the_plain_one() {
        let port = serde_json::json!([
            {"name": "Device ID", "value": "rtr-01"},
            {"name": "Address", "value": "198.51.100.1"},
            {"name": "Management address", "value": "198.51.100.9"}
        ]);
        let n = cdp_neighbor(Local::switch_port("1"), &Fields::of(Some(&port))).expect("a row");
        assert_eq!(n.remote_mgmt_addr.as_deref(), Some("198.51.100.9"));
    }

    #[test]
    fn a_row_with_no_identity_is_skipped_and_a_blank_value_is_absent() {
        let got = parsed();
        let rows = &got["Q2SW-0001"];
        // Port 7: LLDP + CDP. Port 8: LLDP only (its CDP list is empty).
        assert_eq!(rows.len(), 3);
        let eight = rows
            .iter()
            .find(|n| n.local_port == "Port 8")
            .expect("port 8");
        assert_eq!(eight.remote_chassis_kind, Some(NeighborIdKind::Text));
        assert!(eight.capabilities.is_empty());
        assert_eq!(eight.remote_mgmt_addr, None);
        let no_chassis = serde_json::json!([{"name": "System name", "value": "x"}]);
        assert!(lldp_neighbor(Local::switch_port("1"), &Fields::of(Some(&no_chassis))).is_none());
    }

    /// decision 4: a listed switch with no neighbours is an answer (empty); an unlisted one is absent.
    #[test]
    fn a_listed_switch_that_hears_nothing_still_gets_an_entry() {
        let quiet = serde_json::json!({"serial": "Q2SW-0002", "ports": []});
        let got = parse_switch_port_topology(&[row(), quiet]);
        assert_eq!(got.get("Q2SW-0002").map(Vec::len), Some(0));
        assert!(!got.contains_key("Q2SW-0003"));
    }

    /// ADR-182: how this module spells a row is pinned against [`MERAKI_NEIGHBOR_FORMAT`]. When a
    /// change here makes the same cabling read differently, this fails — raise the format, then
    /// paste the new keys below. Core marks the first read after the upgrade as a change of
    /// spelling only because the number moved; forget it and every Meraki device's history gains
    /// a row nobody can explain. ⚠️ It sees only what these two fixtures exercise.
    #[test]
    fn the_spelling_is_pinned_to_the_format() {
        let switch = NeighborSet::new(parsed()["Q2SW-0001"].clone(), MERAKI_NEIGHBOR_FORMAT);
        let appliance = NeighborSet::new(parse_device_lldp_cdp(&mx_body()), MERAKI_NEIGHBOR_FORMAT);
        assert_eq!(
            (MERAKI_NEIGHBOR_FORMAT, switch.content_key(), appliance.content_key()),
            (
                1,
                concat!(
                    "v1\nn=lldp\nlp=Port 7\nrc=0c:8d:db:00:00:02\nrp=0\nli=7\npd=eth0\nsn=ap-01\n",
                    "sd=Meraki MR36 Cloud Managed AP\nma=192.0.2.21\npl=-\ncp=router,switch,igmp\n",
                    "n=cdp\nlp=Port 7\nrc=0c:8d:db:00:00:02\nrp=Port 0\nli=7\npd=-\nsn=ap-01\nsd=-\n",
                    "ma=192.0.2.21\npl=Meraki MR36 Cloud Managed AP\ncp=router,switch,igmp\n",
                    "n=lldp\nlp=Port 8\nrc=core-sw\nrp=Gi1/0/8\nli=8\npd=-\nsn=-\nsd=-\nma=-\npl=-\n",
                    "cp=\nt=0\n",
                )
                .to_owned(),
                concat!(
                    "v1\nn=lldp\nlp=Port 3\nrc=00:00:0c:00:00:20\nrp=Gi0/1\nli=-\n",
                    "pd=GigabitEthernet0/1\nsn=sw-02\nsd=Cisco IOS Software\nma=192.0.2.31\npl=-\n",
                    "cp=router,bridge\nn=cdp\nlp=Port 3\nrc=sw-02.example.com\n",
                    "rp=GigabitEthernet0/1\nli=-\npd=-\nsn=-\n",
                    "sd=Cisco IOS Software, C2960CX Software, Version 15.2(7)E\nma=192.0.2.31\n",
                    "pl=cisco WS-C2960CX-8PC-L\ncp=switch\nn=cdp\nlp=Port 5\n",
                    "rc=0c:8d:db:00:00:30\nrp=Port 1\nli=-\npd=-\nsn=-\nsd=-\nma=-\npl=-\ncp=\nt=0\n",
                )
                .to_owned(),
            ),
            "a Meraki neighbour row reads differently: raise MERAKI_NEIGHBOR_FORMAT and re-pin \
             these keys (ADR-182)"
        );
    }

    #[test]
    fn the_rows_survive_canonicalisation_whole() {
        let got = parsed();
        let set = NeighborSet::new(got["Q2SW-0001"].clone(), 0);
        assert_eq!(set.neighbors.len(), 3);
        assert!(!set.truncated);
    }

    /// One MX in the shape `devices/{serial}/lldpCdp` answers (field names and value shapes as
    /// recorded 2026-09-26; every value is made up).
    fn mx_body() -> Value {
        serde_json::json!({
            "sourceMac": "0c:8d:db:00:00:10",
            "ports": {
                "port3": {
                    "cdp": {
                        "sourcePort": "0c:8d:db:00:00:13",
                        "platform": "cisco WS-C2960CX-8PC-L",
                        "deviceId": "sw-02.example.com",
                        "address": "192.0.2.30",
                        "portId": "GigabitEthernet0/1",
                        "nativeVlan": 1,
                        "version": "Cisco IOS Software, C2960CX Software, Version 15.2(7)E",
                        "capabilities": "Switch, IGMP",
                        "managementAddress": "192.0.2.31"
                    },
                    "lldp": {
                        "sourcePort": "0c:8d:db:00:00:13",
                        "systemName": "sw-02",
                        "systemDescription": "Cisco IOS Software",
                        "chassisId": "00:00:0c:00:00:20",
                        "managementVlan": 1,
                        "portVlan": 1,
                        "managementAddress": "192.0.2.31",
                        "portId": "Gi0/1",
                        "portDescription": "GigabitEthernet0/1",
                        "systemCapabilities": "Bridge, Router"
                    },
                    "deviceMac": "00:00:0c:00:00:20",
                    "device": {"url": "https://example.com/"}
                },
                "port5": {
                    "cdp": {"deviceId": "0c8ddb000030", "portId": "Port 1", "version": "1"},
                    "deviceMac": "0c:8d:db:00:00:30"
                },
                "wan1": {
                    "lldp": {
                        "systemName": "mx-02",
                        "systemDescription": "Meraki MX68 Cloud Managed Security Appliance",
                        "chassisId": "0c:8d:db:00:00:40",
                        "portId": "0"
                    },
                    "deviceMac": "0c:8d:db:00:00:40"
                }
            }
        })
    }

    /// Inc.3: the LAN ports' peers, read by the same builders as a switch's; the `wan` port's is
    /// dropped (decision 3), and an MX port has no ifIndex (decision 6).
    #[test]
    fn an_mx_answer_yields_its_lan_side_through_the_same_builders() {
        let rows = parse_device_lldp_cdp(&mx_body());
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert!(rows.iter().all(|n| n.local_port != "wan1"));
        assert!(rows.iter().all(|n| n.local_ifindex.is_none()));

        let lldp = rows
            .iter()
            .find(|n| n.proto == NeighborProto::Lldp)
            .expect("port3's LLDP row");
        assert_eq!(lldp.local_port, "Port 3");
        assert_eq!(lldp.remote_chassis, "00:00:0c:00:00:20");
        assert_eq!(lldp.remote_chassis_kind, Some(NeighborIdKind::Mac));
        assert_eq!(lldp.remote_sys_name.as_deref(), Some("sw-02"));
        assert_eq!(lldp.remote_mgmt_addr.as_deref(), Some("192.0.2.31"));
        assert_eq!(
            lldp.capabilities,
            [NeighborCapability::Router, NeighborCapability::Bridge]
        );

        let cdp = rows
            .iter()
            .find(|n| n.proto == NeighborProto::Cdp && n.local_port == "Port 3")
            .expect("port3's CDP row");
        assert_eq!(cdp.remote_chassis, "sw-02.example.com");
        assert_eq!(
            cdp.remote_platform.as_deref(),
            Some("cisco WS-C2960CX-8PC-L")
        );
        assert_eq!(cdp.remote_mgmt_addr.as_deref(), Some("192.0.2.31"));

        // Inc.2 applies here too: a Meraki peer's bare-hex id is a MAC and its "1" is dropped.
        let meraki = rows
            .iter()
            .find(|n| n.local_port == "Port 5")
            .expect("port5's CDP row");
        assert_eq!(meraki.remote_chassis, "0c:8d:db:00:00:30");
        assert_eq!(meraki.remote_sys_desc, None);
    }

    /// Inc.5: an MR's answer is an MX's shape with one port, `wired0` (recorded on ten MR, every
    /// one named so). It keeps that name — "Port N" is for numbered ports only — has no ifIndex (an
    /// MR's Interfaces rows are its radios), and its two rows of one Meraki switch fill each other.
    #[test]
    fn an_mr_answer_yields_its_wired_port_through_the_same_builders() {
        let body = serde_json::json!({
            "sourceMac": "0c:8d:db:00:00:50",
            "ports": {
                "wired0": {
                    "cdp": {
                        "sourcePort": "wired0",
                        "platform": "MS120-24",
                        "deviceId": "0c8ddb000060",
                        "address": "192.0.2.60",
                        "portId": "Port 7",
                        "nativeVlan": 1,
                        "version": "1",
                        "capabilities": "Switch",
                        "managementAddress": "192.0.2.60"
                    },
                    "lldp": {
                        "sourcePort": "wired0",
                        "systemName": "sw-01",
                        "systemDescription": "Meraki MS120-24 Cloud Managed Switch",
                        "chassisId": "0c:8d:db:00:00:60",
                        "managementVlan": 1,
                        "portVlan": 1,
                        "managementAddress": "192.0.2.60",
                        "portId": "7",
                        "portDescription": "Port 7",
                        "systemCapabilities": "S-VLAN Component of a VLAN Bridge"
                    },
                    "deviceMac": "0c:8d:db:00:00:60",
                    "device": {"url": "https://example.com/"}
                }
            }
        });
        let rows = parse_device_lldp_cdp(&body);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(rows.iter().all(|n| n.local_port == "wired0"));
        assert!(rows.iter().all(|n| n.local_ifindex.is_none()));
        assert!(rows
            .iter()
            .all(|n| n.remote_mgmt_addr.as_deref() == Some("192.0.2.60")));

        let lldp = rows
            .iter()
            .find(|n| n.proto == NeighborProto::Lldp)
            .expect("the LLDP row");
        assert_eq!(lldp.remote_sys_name.as_deref(), Some("sw-01"));
        // The Dashboard's LLDP value is unreadable (decision 10); the CDP row's lends it a role.
        assert_eq!(lldp.capabilities, [NeighborCapability::Switch]);

        let cdp = rows
            .iter()
            .find(|n| n.proto == NeighborProto::Cdp)
            .expect("the CDP row");
        assert_eq!(cdp.remote_chassis, "0c:8d:db:00:00:60");
        assert_eq!(cdp.remote_sys_name.as_deref(), Some("sw-01"));
    }

    #[test]
    fn an_mx_that_hears_nothing_answers_an_empty_list() {
        assert!(
            parse_device_lldp_cdp(&serde_json::json!({"sourceMac": "x", "ports": {}})).is_empty()
        );
        assert!(parse_device_lldp_cdp(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn an_address_list_yields_its_first_address() {
        assert_eq!(
            first_address("2001:db8::1, 192.0.2.1").as_deref(),
            Some("2001:db8::1")
        );
        assert_eq!(first_address("not an address"), None);
    }
}
