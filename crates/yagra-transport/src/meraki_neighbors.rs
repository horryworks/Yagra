// SPDX-License-Identifier: AGPL-3.0-only
//! A Meraki switch's LLDP/CDP neighbours, from `switch/ports/topology/discovery/byDevice`
//! (ADR-181) — the organization-wide listing that tells, per switch port, what the port hears.
//!
//! Each port carries two lists of `{name, value}` pairs, one per protocol, labelled the way the
//! Dashboard displays them ("System name", "Port ID", …). This module is the one place those labels
//! are read, so a label the Dashboard renames costs one line here and one fixture.
//!
//! Measured on a real organization (2026-09-26, 854 switches, 3,841 ports with neighbours): every
//! port id was a plain decimal, no port listed two LLDP chassis, and the oldest `lastUpdatedAt` was
//! just under 24 hours — so a neighbour unplugged stays listed for up to a day. That age is not
//! filtered here (ADR-181 決定 5): the Dashboard shows the same rows.
//!
//! 🚨 **The Dashboard's LLDP "System capabilities" do not name the IEEE roles a switch or router
//! advertises.** On that organization they were "S-VLAN Component of a VLAN Bridge" and "Two-port
//! MAC Relay" on almost every row — values an 802.1AB peer only sets for provider bridges and
//! TPMRs, and which look like the Dashboard reading the bitmap byte-swapped. Guessing the intended
//! role from them would print "bridge" on rows nobody measured, so only the literal IEEE names are
//! read and those two are dropped: an LLDP row from Meraki usually has no capabilities (ADR-181
//! 決定 10). CDP's list is ordinary text ("Router, Switch") and is read as such.

use std::collections::BTreeMap;
use std::net::IpAddr;

use serde_json::Value;
use yagra_common::{
    render_mac, switch_port_ifindex, Neighbor, NeighborCapability, NeighborIdKind, NeighborProto,
};

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
/// stored neighbours alone (ADR-181 決定 4).
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
            let lldp = Fields::of(port.get("lldp"));
            if let Some(n) = lldp_neighbor(port_id, &lldp) {
                neighbors.push(n);
            }
            let cdp = Fields::of(port.get("cdp"));
            if let Some(n) = cdp_neighbor(port_id, &cdp) {
                neighbors.push(n);
            }
        }
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

    fn get(&self, name: &str) -> Option<&'a str> {
        self.0.get(name).copied()
    }

    fn owned(&self, name: &str) -> Option<String> {
        self.get(name).map(str::to_owned)
    }
}

/// The local side both protocols share: the Dashboard's port id, which is the port's `if_name` on
/// the Interfaces tab, and the ifIndex that tab keys it by (ADR-181 決定 8, ADR-167 決定 4).
fn on_port(proto: NeighborProto, port_id: &str, chassis: String, port: String) -> Neighbor {
    let mut n = Neighbor::new(proto, port_id, chassis, port);
    n.local_ifindex = Some(switch_port_ifindex(port_id));
    n
}

fn lldp_neighbor(port_id: &str, f: &Fields<'_>) -> Option<Neighbor> {
    let (chassis, chassis_kind) = id_with_kind(f.get(lldp::CHASSIS_ID)?);
    let (port, port_kind) = f.get(lldp::PORT_ID).map_or((String::new(), None), |p| {
        let (p, k) = id_with_kind(p);
        (p, Some(k))
    });
    let mut n = on_port(NeighborProto::Lldp, port_id, chassis, port);
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

fn cdp_neighbor(port_id: &str, f: &Fields<'_>) -> Option<Neighbor> {
    let (device_id, device_id_kind) = cdp_device_id(f.get(cdp::DEVICE_ID)?);
    let port = f.owned(cdp::PORT_ID).unwrap_or_default();
    let mut n = on_port(NeighborProto::Cdp, port_id, device_id, port);
    n.remote_chassis_kind = Some(device_id_kind);
    // CDP names its peer's port by name — text, as the SNMP walk records it.
    n.remote_port_kind = (!n.remote_port.is_empty()).then_some(NeighborIdKind::Text);
    n.remote_platform = f.owned(cdp::PLATFORM);
    // CDP's counterpart of an LLDP system description, as on the SNMP walk (ADR-180 決定 6).
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
/// and core can look its maker up (ADR-180 決定 4). Anything else is text.
fn id_with_kind(raw: &str) -> (String, NeighborIdKind) {
    match parse_mac(raw).as_deref().and_then(render_mac) {
        Some(mac) => (mac, NeighborIdKind::Mac),
        None => (raw.to_owned(), NeighborIdKind::Text),
    }
}

/// A CDP device id as the Dashboard rendered it, and what it is (ADR-181 増分 2 決定 B). A Meraki
/// peer names itself by its MAC as twelve bare hex digits (`0c8ddb000002`) — measured on a real
/// organization, 2,571 of 2,873 CDP rows, every one a Meraki device or a Cisco CBS — so those are
/// read as a MAC too, which gives the row the same chassis as the peer's LLDP row and a maker
/// name. Anything else is what [`id_with_kind`] makes of it: a CDP device id is usually a name.
fn cdp_device_id(raw: &str) -> (String, NeighborIdKind) {
    match bare_hex_mac(raw).as_deref().and_then(render_mac) {
        Some(mac) => (mac, NeighborIdKind::Mac),
        None => id_with_kind(raw),
    }
}

/// Twelve hex digits and nothing else. `None` for anything else — a name that happens to be
/// twelve characters of `0-9a-f` is the one case read wrongly, and the cost is a maker name beside
/// it (display only, ADR-180 決定 5).
fn bare_hex_mac(raw: &str) -> Option<Vec<u8>> {
    if raw.len() != 12 || !raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..6)
        .map(|i| u8::from_str_radix(&raw[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

/// A CDP version string worth showing, or `None` (ADR-181 増分 2 決定 A). A Meraki peer answers
/// `"1"` — measured on a real organization, 2,569 of 2,873 CDP rows, every one a Meraki peer, and
/// no other value of one or two characters — which put a lone "1" under the model on the
/// Neighbors tab. A version that says something (an IOS banner, `SCCP 9.4.1.3.SR3`) is kept.
fn cdp_version(raw: &str) -> Option<String> {
    let meaningless = raw.len() <= 2 && raw.bytes().all(|b| b.is_ascii_digit());
    (!meaningless).then(|| raw.to_owned())
}

/// Six `:`- or `-`-separated hex octets. `None` for anything else.
fn parse_mac(raw: &str) -> Option<Vec<u8>> {
    let parts: Vec<&str> = raw.split([':', '-']).collect();
    if parts.len() != 6 {
        return None;
    }
    parts
        .iter()
        .map(|p| {
            if p.len() == 2 {
                u8::from_str_radix(p, 16).ok()
            } else {
                None
            }
        })
        .collect()
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
            .find(|n| n.proto == NeighborProto::Lldp && n.local_port == "7")
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

    /// 決定 10: the two names the Dashboard puts on nearly every LLDP row are not guessed into a role.
    #[test]
    fn the_dashboards_lldp_capability_names_are_not_guessed_into_roles() {
        let got = parsed();
        assert!(got["Q2SW-0001"]
            .iter()
            .filter(|n| n.proto == NeighborProto::Lldp)
            .all(|n| n.capabilities.is_empty()));
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
        assert_eq!(n.local_port, "7");
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

    /// 増分 2 決定 B: a Meraki peer's bare-hex CDP device id is its MAC — the same chassis its LLDP
    /// row on the same port carries.
    #[test]
    fn a_meraki_peers_bare_hex_device_id_is_read_as_the_mac_its_lldp_row_names() {
        let got = parsed();
        let rows = &got["Q2SW-0001"];
        let cdp = rows.iter().find(|n| n.proto == NeighborProto::Cdp).unwrap();
        let lldp = rows
            .iter()
            .find(|n| n.proto == NeighborProto::Lldp && n.local_port == "7")
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

    /// 増分 2 決定 A: a Meraki peer's `"1"` is not shown; a version that says something is.
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
        let n = cdp_neighbor("1", &Fields::of(Some(&port))).expect("a row");
        assert_eq!(n.remote_mgmt_addr.as_deref(), Some("198.51.100.9"));
    }

    #[test]
    fn a_row_with_no_identity_is_skipped_and_a_blank_value_is_absent() {
        let got = parsed();
        let rows = &got["Q2SW-0001"];
        // Port 7: LLDP + CDP. Port 8: LLDP only (its CDP list is empty).
        assert_eq!(rows.len(), 3);
        let eight = rows.iter().find(|n| n.local_port == "8").expect("port 8");
        assert_eq!(eight.remote_chassis_kind, Some(NeighborIdKind::Text));
        assert!(eight.capabilities.is_empty());
        assert_eq!(eight.remote_mgmt_addr, None);
        let no_chassis = serde_json::json!([{"name": "System name", "value": "x"}]);
        assert!(lldp_neighbor("1", &Fields::of(Some(&no_chassis))).is_none());
    }

    /// 決定 4: a listed switch with no neighbours is an answer (empty); an unlisted one is absent.
    #[test]
    fn a_listed_switch_that_hears_nothing_still_gets_an_entry() {
        let quiet = serde_json::json!({"serial": "Q2SW-0002", "ports": []});
        let got = parse_switch_port_topology(&[row(), quiet]);
        assert_eq!(got.get("Q2SW-0002").map(Vec::len), Some(0));
        assert!(!got.contains_key("Q2SW-0003"));
    }

    #[test]
    fn the_rows_survive_canonicalisation_whole() {
        let got = parsed();
        let set = NeighborSet::new(got["Q2SW-0001"].clone());
        assert_eq!(set.neighbors.len(), 3);
        assert!(!set.truncated);
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
