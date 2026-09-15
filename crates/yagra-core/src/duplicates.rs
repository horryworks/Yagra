// SPDX-License-Identifier: AGPL-3.0-only
//! What Nodes ▸ Duplicates lists (ADR-148): the device nodes that look like **one physical device
//! registered more than once** — usually under two different addresses — and the evidence for each
//! group.
//!
//! **Computed on every read and never stored**, for the reason ADR-139 decision 2 gives a scan's
//! "already in the tree": nodes are added, deleted and re-polled after the fact, and a stored group
//! would be wrong the moment one of them was.
//!
//! **Nothing here deletes anything.** ADR-139 decision 7 settled that duplicates are not merged: the
//! screen lists them and a person picks what goes, through the existing bulk delete.
//!
//! Pure over its arguments — the candidate rows and what the other stores observed about their
//! addresses — so every rule below is tested without a database.
//!
//! ## The evidence, and why each kind is strong or weak
//!
//! A **strong** kind identifies a device by construction, so one of them is enough to call a group
//! a duplicate. A **weak** kind is shared by distinct devices often enough that a group needs two
//! different weak kinds before it is listed at all, and then only as "possible" (decision 2).
//!
//! | Kind | Strength | What it says |
//! |---|---|---|
//! | `address` | strong | two device nodes are monitored at the same address |
//! | `serial` | strong | a chassis serial (one member of a stack's list) or a Meraki serial is shared |
//! | `own_ip` | strong | each node's own interface-address list names the other's monitored address |
//! | `own_ip_one_way` | weak | one list names the other's address, and the other has no list to confirm |
//! | `arp_mac` | strong | some router's ARP cache resolves both monitored addresses to one MAC |
//! | `lldp_chassis` | strong | an LLDP neighbour reports the same chassis id at both addresses |
//! | `cdp_device_id` | weak | a CDP neighbour reports the same device id — usually a hostname |
//! | `name` | weak | the node names are equal, ignoring case and surrounding space |
//!
//! 🚨 **`own_ip` needs both directions.** Sites that reuse the same private addressing are common:
//! router B may carry `192.168.100.1` on a LAN port while node A is a different router monitored at
//! that same address through another path. Then B's list names A's address, but A's list — if A has
//! one — does not name B's. A device polled twice returns the same list both times, so a genuine
//! duplicate always agrees in both directions. One direction with no list on the other side is still
//! worth saying, which is what the weak kind is for.
//!
//! ## A value that too many nodes share identifies none of them
//!
//! [`SHARED_VALUE_MAX`] (decision 3). Placeholder serials, a simulator recording used by several
//! agents, a hostname like `switch`, a virtual address nobody flagged as anycast: each of them is
//! "shared" by many nodes, and grouping on it would draw one enormous false group. Such a value is
//! not used, and is reported in [`Findings::ignored`] rather than dropped silently. `address` is the
//! one kind exempt — device nodes at one address are the duplicate ADR-139 exists for, however many.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::IpAddr;

use serde::Serialize;
use uuid::Uuid;
use yagra_common::NeighborProto;

use crate::repo::DuplicateInput;

/// The most nodes that may share a value before the value stops counting as evidence (decision 3).
///
/// A judgement, not a measurement: a stack or a chassis cluster registered by every member's own
/// address stays under it, and a value carried by nine devices has stopped naming a device.
pub const SHARED_VALUE_MAX: usize = 8;

/// The most groups one read lists. `Findings::total` still says how many there are.
pub const GROUPS_MAX: usize = 500;

/// The most ignored values one read lists. `Findings::ignored_total` still says how many there are.
pub const IGNORED_MAX: usize = 200;

/// A serial shorter than this identifies nothing (`0`, `1`, `NA`).
const SERIAL_MIN_CHARS: usize = 4;

/// Serial numbers devices report when they have none, compared case-insensitively.
///
/// Two of them are in the lab's own recordings (`redacted` on a FortiGate, `<private>` on NX-OS and a
/// C9800, ADR-147); the rest are what vendors and BIOS images put in the field.
const PLACEHOLDER_SERIALS: &[&str] = &[
    "n/a",
    "na",
    "none",
    "null",
    "unknown",
    "not available",
    "not specified",
    "not applicable",
    "default",
    "default string",
    "serial",
    "serial number",
    "system serial number",
    "to be filled by o.e.m.",
    "redacted",
    "<private>",
    "private",
    "tbd",
    "123456789",
    "0123456789",
    "1234567890",
];

/// MAC prefixes that belong to a *virtual* router, not to a device (decision 5). Two routers in a
/// VRRP, HSRP or GLBP group answer ARP with the same one of these, which is exactly the case where a
/// shared MAC does not mean a shared device.
const VIRTUAL_MAC_PREFIXES: &[&str] = &[
    "00:00:5e:00:01:", // VRRP (IPv4) and CARP
    "00:00:5e:00:02:", // VRRP (IPv6)
    "00:00:0c:07:ac:", // HSRP v1
    "00:00:0c:9f:f",   // HSRP v2 (00:00:0c:9f:f0:00 – 00:00:0c:9f:ff:ff)
    "00:07:b4:00:",    // GLBP
];

/// One kind of evidence that two nodes are one device.
///
/// Ordered strong-first so the evidence a group lists reads from the most to the least convincing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateEvidenceKind {
    /// The two nodes are monitored at the same address.
    Address,
    /// The device reports the same serial number (one member of a stack's list counts).
    Serial,
    /// Each node's own interface-address list contains the other node's monitored address.
    OwnIp,
    /// One node's interface-address list contains the other's monitored address, and the other node
    /// has no list to confirm it.
    OwnIpOneWay,
    /// A router's ARP cache resolves both monitored addresses to the same MAC address.
    ArpMac,
    /// An LLDP neighbour reports the same chassis id at both addresses.
    LldpChassis,
    /// A CDP neighbour reports the same device id at both addresses.
    CdpDeviceId,
    /// The node names are equal.
    Name,
}

impl DuplicateEvidenceKind {
    /// Every kind, in display order. Test-only: production code never iterates the kinds, it only
    /// matches on one.
    #[cfg(test)]
    pub const ALL: [Self; 8] = [
        Self::Address,
        Self::Serial,
        Self::OwnIp,
        Self::OwnIpOneWay,
        Self::ArpMac,
        Self::LldpChassis,
        Self::CdpDeviceId,
        Self::Name,
    ];

    /// Whether one of these is enough to call two nodes the same device (decision 2).
    #[must_use]
    pub const fn is_strong(self) -> bool {
        match self {
            Self::Address | Self::Serial | Self::OwnIp | Self::ArpMac | Self::LldpChassis => true,
            Self::OwnIpOneWay | Self::CdpDeviceId | Self::Name => false,
        }
    }

    /// Whether [`SHARED_VALUE_MAX`] applies (decision 3).
    const fn is_capped(self) -> bool {
        match self {
            Self::Address => false,
            Self::Serial
            | Self::OwnIp
            | Self::OwnIpOneWay
            | Self::ArpMac
            | Self::LldpChassis
            | Self::CdpDeviceId
            | Self::Name => true,
        }
    }
}

/// How sure a group is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateConfidence {
    /// Joined by strong evidence, and nothing in the group contradicts it.
    Confident,
    /// Joined only by two kinds of weak evidence, or contradicted by something in the group.
    Possible,
}

/// Something in a group that says its members are *not* one device (decision 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateContradiction {
    /// Two members both report a serial number, and none of them is shared.
    SerialDiffers,
    /// Two members both report a `sysObjectID`, and they differ — the devices are different models.
    ModelDiffers,
}

/// One row of some node's own interface-address list that names a candidate's monitored address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnAddress {
    /// The node whose list it is.
    pub node: Uuid,
    /// The address on that list. Only rows whose type identifies the node (unicast, or not reported)
    /// belong here — the reader drops `anycast` and `broadcast`.
    pub ip: IpAddr,
}

/// What one neighbour said about the device at an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerReport {
    /// The management address the neighbour advertised for its peer.
    pub address: IpAddr,
    /// `lldpRemChassisId` or `cdpCacheDeviceId`, as rendered.
    pub id: String,
    pub protocol: NeighborProto,
}

/// What the stores beside `nodes` observed. The reader narrows every list to rows that name some
/// candidate's monitored address, so none of this grows with the parts of the fleet the caller cannot
/// see; rows about any other node are ignored here as well.
#[derive(Debug, Default)]
pub struct Observations {
    /// Interface-address rows that name a candidate's address (ADR-043).
    pub own_addresses: Vec<OwnAddress>,
    /// The nodes that have an interface-address list at all, whatever it holds.
    pub with_address_list: BTreeSet<Uuid>,
    /// `(address, mac)` pairs from every router's ARP cache.
    pub arp: Vec<(IpAddr, String)>,
    /// What neighbours reported about the device at an address (ADR-038).
    pub peers: Vec<PeerReport>,
    /// A Meraki node's serial, by node id.
    pub meraki_serials: HashMap<Uuid, String>,
}

/// One piece of evidence inside a group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub kind: DuplicateEvidenceKind,
    /// The shared value — the address, the serial, the MAC, the chassis id, the name.
    pub value: String,
    /// The members of the group that share it, at least two.
    pub nodes: Vec<Uuid>,
}

/// One group of nodes that look like one device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub confidence: DuplicateConfidence,
    /// Strongest first.
    pub evidence: Vec<Evidence>,
    pub contradictions: Vec<DuplicateContradiction>,
    /// The suggested keeper first, then the rest from the oldest registration.
    pub members: Vec<Uuid>,
    /// The member suggested to keep (decision 6). Suggested only: nothing is selected for the caller.
    pub keeper: Uuid,
}

/// A value that was not used as evidence because too many nodes share it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ignored {
    pub kind: DuplicateEvidenceKind,
    pub value: String,
    /// How many candidate nodes share it.
    pub nodes: usize,
}

/// Everything one read of the screen shows.
#[derive(Debug, Default)]
pub struct Findings {
    /// Confident groups first, at most [`GROUPS_MAX`].
    pub groups: Vec<Group>,
    /// How many groups there are — more than `groups.len()` when the list was cut.
    pub total: usize,
    /// Values not used as evidence, the most widely shared first, at most [`IGNORED_MAX`].
    pub ignored: Vec<Ignored>,
    /// How many values were not used.
    pub ignored_total: usize,
    /// The device nodes compared.
    pub scanned: usize,
    /// Of those, how many report a serial number. With [`Self::with_address_list`] it tells an empty
    /// result on a fleet that was compared from one on a fleet whose evidence has not been collected.
    pub with_serial: usize,
    /// Of those, how many have an interface-address list.
    pub with_address_list: usize,
}

/// Whether a monitored address can be matched against what devices report about themselves.
///
/// Loopback, link-local, multicast, unspecified and broadcast addresses are carried by every device
/// or by none in particular: a node monitored at `127.0.0.1` would otherwise match the list of every
/// SNMP device in the fleet.
#[must_use]
pub fn address_identifies(ip: IpAddr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return false;
    }
    match ip {
        IpAddr::V4(v4) => !(v4.is_link_local() || v4.is_broadcast()),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) != 0xfe80,
    }
}

/// The serial numbers a stored value names, upper-cased, placeholders removed.
///
/// A stack is stored as its members joined with `, ` (ADR-147 decision 2), so each member is its own
/// value: a stack registered once by its master and once by a member that later became master shares
/// the member serials even when the order of the list moved.
#[must_use]
pub fn serial_parts(raw: &str) -> Vec<String> {
    let mut parts: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| serial_identifies(s))
        .map(str::to_uppercase)
        .collect();
    parts.sort();
    parts.dedup();
    parts
}

fn serial_identifies(serial: &str) -> bool {
    if serial.chars().count() < SERIAL_MIN_CHARS {
        return false;
    }
    let lower = serial.to_lowercase();
    if PLACEHOLDER_SERIALS.contains(&lower.as_str()) {
        return false;
    }
    // `000000000`, `XXXXXXXX`, `--------`.
    let mut chars = lower.chars();
    let first = chars.next();
    !chars.all(|c| Some(c) == first)
}

/// Whether an ARP-resolved MAC address names one device (decision 5).
#[must_use]
pub fn mac_identifies(mac: &str) -> bool {
    let lower = mac.trim().to_ascii_lowercase();
    let octets: Vec<u8> = lower
        .split(':')
        .filter_map(|o| u8::from_str_radix(o, 16).ok())
        .collect();
    let [first, ..] = octets.as_slice() else {
        return false;
    };
    if octets.len() != 6 || lower.split(':').count() != 6 {
        return false;
    }
    if octets.iter().all(|b| *b == 0) || first & 1 == 1 {
        // All zeros, or a group address (which includes the broadcast address).
        return false;
    }
    !VIRTUAL_MAC_PREFIXES.iter().any(|p| lower.starts_with(p))
}

fn name_key(name: &str) -> Option<String> {
    let key = name.trim().to_lowercase();
    (!key.is_empty()).then_some(key)
}

/// A minimal union-find over candidate indexes.
struct Components {
    parent: Vec<usize>,
}

impl Components {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
        }
    }

    fn root(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }

    fn join(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.root(a), self.root(b));
        if ra != rb {
            self.parent[ra.max(rb)] = ra.min(rb);
        }
    }
}

/// Group the candidates (decisions 1–6).
///
/// `candidates` are the device nodes the caller may see; anything `obs` says about any other node is
/// ignored.
#[must_use]
pub fn find(candidates: &[DuplicateInput], obs: &Observations) -> Findings {
    let index: HashMap<Uuid, usize> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id, i))
        .collect();
    let mut by_address: BTreeMap<IpAddr, BTreeSet<usize>> = BTreeMap::new();
    for (i, c) in candidates.iter().enumerate() {
        by_address.entry(c.address).or_default().insert(i);
    }
    let serials: Vec<Vec<String>> = candidates
        .iter()
        .map(|c| {
            let mut parts = c
                .serial_number
                .as_deref()
                .map(serial_parts)
                .unwrap_or_default();
            if let Some(meraki) = obs.meraki_serials.get(&c.id) {
                parts.extend(serial_parts(meraki));
            }
            parts.sort();
            parts.dedup();
            parts
        })
        .collect();

    // Every (kind, value) → the candidates that carry it.
    let mut shared: BTreeMap<(DuplicateEvidenceKind, String), BTreeSet<usize>> = BTreeMap::new();
    let mut carry = |kind, value: String, i: usize| {
        shared.entry((kind, value)).or_default().insert(i);
    };
    for (i, c) in candidates.iter().enumerate() {
        carry(DuplicateEvidenceKind::Address, c.address.to_string(), i);
        for part in &serials[i] {
            carry(DuplicateEvidenceKind::Serial, part.clone(), i);
        }
        if let Some(key) = name_key(&c.name) {
            carry(DuplicateEvidenceKind::Name, key, i);
        }
    }
    for (ip, mac) in &obs.arp {
        if !address_identifies(*ip) || !mac_identifies(mac) {
            continue;
        }
        for &i in by_address.get(ip).into_iter().flatten() {
            carry(
                DuplicateEvidenceKind::ArpMac,
                mac.trim().to_ascii_lowercase(),
                i,
            );
        }
    }
    for peer in &obs.peers {
        let id = peer.id.trim();
        if id.is_empty() || !address_identifies(peer.address) {
            continue;
        }
        let (kind, value) = match peer.protocol {
            NeighborProto::Lldp => (DuplicateEvidenceKind::LldpChassis, id.to_owned()),
            NeighborProto::Cdp => (DuplicateEvidenceKind::CdpDeviceId, id.to_lowercase()),
        };
        for &i in by_address.get(&peer.address).into_iter().flatten() {
            carry(kind, value.clone(), i);
        }
    }

    let mut ignored: Vec<Ignored> = Vec::new();
    let mut items: Vec<(DuplicateEvidenceKind, String, BTreeSet<usize>)> = Vec::new();
    for ((kind, value), nodes) in shared {
        if nodes.len() < 2 {
            continue;
        }
        if kind.is_capped() && nodes.len() > SHARED_VALUE_MAX {
            ignored.push(Ignored {
                kind,
                value,
                nodes: nodes.len(),
            });
            continue;
        }
        items.push((kind, value, nodes));
    }

    // `own_ip` is a relation between two nodes rather than a value both carry, so it is built as
    // pairs. The cap is on the address: every candidate that claims it or is monitored at it.
    let mut claims: BTreeMap<usize, BTreeSet<IpAddr>> = BTreeMap::new();
    for row in &obs.own_addresses {
        if let Some(&i) = index.get(&row.node) {
            claims.entry(i).or_default().insert(row.ip);
        }
    }
    let mut claimants: BTreeMap<IpAddr, BTreeSet<usize>> = BTreeMap::new();
    for (&i, ips) in &claims {
        for &ip in ips {
            if address_identifies(ip) && by_address.get(&ip).is_some_and(|at| !at.contains(&i)) {
                claimants.entry(ip).or_default().insert(i);
            }
        }
    }
    let mut pairs: BTreeMap<(DuplicateEvidenceKind, String), BTreeSet<(usize, usize)>> =
        BTreeMap::new();
    for (ip, reporters) in &claimants {
        let at = &by_address[ip];
        let involved = reporters.len() + at.len();
        if involved > SHARED_VALUE_MAX {
            ignored.push(Ignored {
                kind: DuplicateEvidenceKind::OwnIp,
                value: ip.to_string(),
                nodes: involved,
            });
            continue;
        }
        for &reporter in reporters {
            for &other in at {
                let mutual = claims
                    .get(&other)
                    .is_some_and(|theirs| theirs.contains(&candidates[reporter].address));
                let kind = if mutual {
                    DuplicateEvidenceKind::OwnIp
                } else if obs.with_address_list.contains(&candidates[other].id) {
                    // The other node has a list, and it does not name the reporter: the reused
                    // private address this kind must not mistake for one device.
                    continue;
                } else {
                    DuplicateEvidenceKind::OwnIpOneWay
                };
                let pair = (reporter.min(other), reporter.max(other));
                pairs
                    .entry((kind, ip.to_string()))
                    .or_default()
                    .insert(pair);
            }
        }
    }
    for ((kind, value), set) in pairs {
        let nodes: BTreeSet<usize> = set.iter().flat_map(|&(a, b)| [a, b]).collect();
        items.push((kind, value, nodes));
    }

    // Strong evidence joins outright. Weak evidence joins a pair only when two different weak kinds
    // say so (decision 2).
    let mut strong = Components::new(candidates.len());
    let mut all = Components::new(candidates.len());
    let mut weak_kinds: BTreeMap<(usize, usize), BTreeSet<DuplicateEvidenceKind>> = BTreeMap::new();
    for (kind, _, nodes) in &items {
        let members: Vec<usize> = nodes.iter().copied().collect();
        if kind.is_strong() {
            for pair in members.windows(2) {
                strong.join(pair[0], pair[1]);
                all.join(pair[0], pair[1]);
            }
        } else {
            for (x, &a) in members.iter().enumerate() {
                for &b in &members[x + 1..] {
                    weak_kinds.entry((a, b)).or_default().insert(*kind);
                }
            }
        }
    }
    for (&(a, b), kinds) in &weak_kinds {
        if kinds.len() >= 2 {
            all.join(a, b);
        }
    }

    let mut components: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..candidates.len() {
        let root = all.root(i);
        components.entry(root).or_default().push(i);
    }

    let mut groups: Vec<Group> = components
        .into_values()
        .filter(|members| members.len() >= 2)
        .map(|members| build_group(candidates, &serials, &items, &mut strong, &members))
        .collect();
    groups.sort_by(|a, b| {
        let name = |g: &Group| &candidates[index[&g.keeper]].name;
        a.confidence
            .cmp(&b.confidence)
            .then_with(|| name(a).cmp(name(b)))
            .then_with(|| a.keeper.cmp(&b.keeper))
    });
    let total = groups.len();
    groups.truncate(GROUPS_MAX);

    ignored.sort_by(|a, b| {
        b.nodes
            .cmp(&a.nodes)
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a.value.cmp(&b.value))
    });
    let ignored_total = ignored.len();
    ignored.truncate(IGNORED_MAX);

    Findings {
        groups,
        total,
        ignored,
        ignored_total,
        scanned: candidates.len(),
        with_serial: serials.iter().filter(|s| !s.is_empty()).count(),
        with_address_list: candidates
            .iter()
            .filter(|c| obs.with_address_list.contains(&c.id))
            .count(),
    }
}

fn build_group(
    candidates: &[DuplicateInput],
    serials: &[Vec<String>],
    items: &[(DuplicateEvidenceKind, String, BTreeSet<usize>)],
    strong: &mut Components,
    members: &[usize],
) -> Group {
    let inside: BTreeSet<usize> = members.iter().copied().collect();

    let mut evidence: Vec<Evidence> = items
        .iter()
        .filter_map(|(kind, value, nodes)| {
            let shared: Vec<usize> = nodes.intersection(&inside).copied().collect();
            (shared.len() >= 2).then(|| Evidence {
                kind: *kind,
                value: value.clone(),
                nodes: shared.iter().map(|&i| candidates[i].id).collect(),
            })
        })
        .collect();
    evidence.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.value.cmp(&b.value)));

    let mut contradictions: BTreeSet<DuplicateContradiction> = BTreeSet::new();
    for (x, &a) in members.iter().enumerate() {
        for &b in &members[x + 1..] {
            let (sa, sb) = (&serials[a], &serials[b]);
            if !sa.is_empty() && !sb.is_empty() && !sa.iter().any(|s| sb.contains(s)) {
                contradictions.insert(DuplicateContradiction::SerialDiffers);
            }
            let model = |i: usize| {
                candidates[i]
                    .sys_object_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            };
            if let (Some(ma), Some(mb)) = (model(a), model(b)) {
                if ma != mb {
                    contradictions.insert(DuplicateContradiction::ModelDiffers);
                }
            }
        }
    }

    let root = strong.root(members[0]);
    let one_strong_component = members.iter().all(|&i| strong.root(i) == root);
    let confidence = if one_strong_component && contradictions.is_empty() {
        DuplicateConfidence::Confident
    } else {
        DuplicateConfidence::Possible
    };

    // Most depended-on, then the oldest registration, then by name (decision 6).
    let mut ordered: Vec<usize> = members.to_vec();
    ordered.sort_by(|&a, &b| {
        let (ca, cb) = (&candidates[a], &candidates[b]);
        cb.dependents
            .cmp(&ca.dependents)
            .then_with(|| ca.created_at.cmp(&cb.created_at))
            .then_with(|| ca.name.cmp(&cb.name))
            .then_with(|| ca.id.cmp(&cb.id))
    });
    let keeper = candidates[ordered[0]].id;
    let mut rest: Vec<usize> = ordered[1..].to_vec();
    rest.sort_by(|&a, &b| {
        let (ca, cb) = (&candidates[a], &candidates[b]);
        ca.created_at
            .cmp(&cb.created_at)
            .then_with(|| ca.name.cmp(&cb.name))
            .then_with(|| ca.id.cmp(&cb.id))
    });

    Group {
        confidence,
        evidence,
        contradictions: contradictions.into_iter().collect(),
        members: std::iter::once(keeper)
            .chain(rest.iter().map(|&i| candidates[i].id))
            .collect(),
        keeper,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).expect("a valid instant")
    }

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// A candidate registered `n` seconds after the first, at `address`.
    fn node(n: u128, name: &str, address: &str) -> DuplicateInput {
        DuplicateInput {
            id: id(n),
            name: name.to_owned(),
            address: address.parse().expect("a test address"),
            created_at: at(i64::try_from(n).expect("small")),
            group_id: None,
            vendor: None,
            model: None,
            serial_number: None,
            sys_object_id: None,
            dependents: 0,
        }
    }

    fn with_serial(mut n: DuplicateInput, serial: &str) -> DuplicateInput {
        n.serial_number = Some(serial.to_owned());
        n
    }

    fn with_model(mut n: DuplicateInput, oid: &str) -> DuplicateInput {
        n.sys_object_id = Some(oid.to_owned());
        n
    }

    fn kinds(g: &Group) -> Vec<DuplicateEvidenceKind> {
        g.evidence.iter().map(|e| e.kind).collect()
    }

    #[test]
    fn two_nodes_at_one_address_are_a_confident_group_keeping_the_older() {
        let nodes = [node(2, "sw-b", "10.0.0.1"), node(1, "sw-a", "10.0.0.1")];
        let f = find(&nodes, &Observations::default());
        assert_eq!(f.total, 1);
        let g = &f.groups[0];
        assert_eq!(g.confidence, DuplicateConfidence::Confident);
        assert_eq!(kinds(g), [DuplicateEvidenceKind::Address]);
        assert_eq!(g.keeper, id(1));
        assert_eq!(g.members, [id(1), id(2)]);
    }

    #[test]
    fn nodes_at_different_addresses_with_nothing_shared_are_not_grouped() {
        let nodes = [node(1, "a", "10.0.0.1"), node(2, "b", "10.0.0.2")];
        let f = find(&nodes, &Observations::default());
        assert_eq!(f.total, 0);
        assert_eq!(f.scanned, 2);
    }

    #[test]
    fn a_shared_serial_groups_two_addresses_and_a_stack_member_counts() {
        let nodes = [
            with_serial(node(1, "stack-lo0", "10.0.0.1"), "FCW1929B68S, FCW1931A06Z"),
            with_serial(
                node(2, "stack-vlan1", "192.168.1.1"),
                "fcw1931a06z, FCW1929B6BP",
            ),
        ];
        let f = find(&nodes, &Observations::default());
        assert_eq!(f.total, 1);
        let g = &f.groups[0];
        assert_eq!(g.confidence, DuplicateConfidence::Confident);
        assert_eq!(kinds(g), [DuplicateEvidenceKind::Serial]);
        assert_eq!(g.evidence[0].value, "FCW1931A06Z");
        assert!(
            g.contradictions.is_empty(),
            "a shared member is not a differing serial"
        );
    }

    #[test]
    fn placeholder_serials_identify_nothing() {
        for placeholder in [
            "N/A",
            "000000000",
            "<private>",
            "redacted",
            "xxxxxxxx",
            "0",
            "NA",
        ] {
            let nodes = [
                with_serial(node(1, "a", "10.0.0.1"), placeholder),
                with_serial(node(2, "b", "10.0.0.2"), placeholder),
            ];
            assert_eq!(
                find(&nodes, &Observations::default()).total,
                0,
                "{placeholder}"
            );
        }
        assert!(serial_parts("N/A, , FOX1820GVER").eq(&["FOX1820GVER".to_owned()]));
    }

    #[test]
    fn a_meraki_serial_meets_the_same_serial_read_over_snmp() {
        let nodes = [
            node(1, "ms-api", "10.0.0.1"),
            with_serial(node(2, "ms-snmp", "10.0.0.2"), "Q2XX-AAAA-BBBB"),
        ];
        let obs = Observations {
            meraki_serials: HashMap::from([(id(1), "q2xx-aaaa-bbbb".to_owned())]),
            ..Observations::default()
        };
        let f = find(&nodes, &obs);
        assert_eq!(kinds(&f.groups[0]), [DuplicateEvidenceKind::Serial]);
    }

    #[test]
    fn own_ip_in_both_directions_is_strong() {
        let nodes = [
            node(1, "r1-loopback", "10.255.0.1"),
            node(2, "r1-lan", "192.168.1.1"),
        ];
        let own = |n: u128, ip: &str| OwnAddress {
            node: id(n),
            ip: ip.parse().expect("ip"),
        };
        let obs = Observations {
            own_addresses: vec![own(1, "192.168.1.1"), own(2, "10.255.0.1")],
            with_address_list: BTreeSet::from([id(1), id(2)]),
            ..Observations::default()
        };
        let f = find(&nodes, &obs);
        assert_eq!(f.total, 1);
        assert_eq!(f.groups[0].confidence, DuplicateConfidence::Confident);
        assert!(kinds(&f.groups[0]).contains(&DuplicateEvidenceKind::OwnIp));
        assert_eq!(f.with_address_list, 2);
    }

    /// The reused private address: B carries A's address on a port, and A's own list — which exists —
    /// does not name B. That is two routers, not one.
    #[test]
    fn own_ip_contradicted_by_the_other_list_is_not_evidence() {
        let nodes = [
            node(1, "site-a-gw", "192.168.100.1"),
            node(2, "site-b-gw", "10.2.0.1"),
        ];
        let obs = Observations {
            own_addresses: vec![OwnAddress {
                node: id(2),
                ip: "192.168.100.1".parse().expect("ip"),
            }],
            with_address_list: BTreeSet::from([id(1), id(2)]),
            ..Observations::default()
        };
        assert_eq!(find(&nodes, &obs).total, 0);
    }

    #[test]
    fn own_ip_one_way_is_weak_and_needs_a_second_weak_kind() {
        let nodes = [
            node(1, "core-sw", "10.0.0.1"),
            node(2, "core-sw", "192.168.1.1"),
        ];
        let one_way = Observations {
            own_addresses: vec![OwnAddress {
                node: id(1),
                ip: "192.168.1.1".parse().expect("ip"),
            }],
            with_address_list: BTreeSet::from([id(1)]),
            ..Observations::default()
        };
        let f = find(&nodes, &one_way);
        assert_eq!(f.total, 1, "one-way plus the same name");
        assert_eq!(f.groups[0].confidence, DuplicateConfidence::Possible);
        assert_eq!(
            kinds(&f.groups[0]),
            [
                DuplicateEvidenceKind::OwnIpOneWay,
                DuplicateEvidenceKind::Name
            ]
        );

        let renamed = [
            node(1, "core-sw", "10.0.0.1"),
            node(2, "ping-only", "192.168.1.1"),
        ];
        assert_eq!(find(&renamed, &one_way).total, 0, "one weak kind alone");
    }

    #[test]
    fn a_node_monitored_at_loopback_matches_no_device_list() {
        let nodes = [node(1, "self", "127.0.0.1"), node(2, "router", "10.0.0.1")];
        let obs = Observations {
            own_addresses: vec![OwnAddress {
                node: id(2),
                ip: "127.0.0.1".parse().expect("ip"),
            }],
            ..Observations::default()
        };
        assert_eq!(find(&nodes, &obs).total, 0);
        assert!(!address_identifies("fe80::1".parse().expect("ip")));
        assert!(!address_identifies("169.254.1.1".parse().expect("ip")));
        assert!(address_identifies("2001:db8::1".parse().expect("ip")));
    }

    #[test]
    fn one_mac_behind_two_monitored_addresses_is_strong_unless_it_is_virtual() {
        let nodes = [
            node(1, "srv", "10.0.0.5"),
            node(2, "srv-2nd-ip", "10.0.0.6"),
        ];
        let arp = |mac: &str| Observations {
            arp: vec![
                ("10.0.0.5".parse().expect("ip"), mac.to_owned()),
                ("10.0.0.6".parse().expect("ip"), mac.to_owned()),
            ],
            ..Observations::default()
        };
        let f = find(&nodes, &arp("AA:BB:CC:00:11:22"));
        assert_eq!(kinds(&f.groups[0]), [DuplicateEvidenceKind::ArpMac]);
        assert_eq!(f.groups[0].evidence[0].value, "aa:bb:cc:00:11:22");
        for virtual_or_bogus in [
            "00:00:5e:00:01:0a",
            "00:00:0c:07:ac:01",
            "00:00:0c:9f:f0:01",
            "00:07:b4:00:01:02",
            "00:00:00:00:00:00",
            "ff:ff:ff:ff:ff:ff",
            "01:00:5e:00:00:01",
            "aa:bb:cc",
        ] {
            assert_eq!(
                find(&nodes, &arp(virtual_or_bogus)).total,
                0,
                "{virtual_or_bogus}"
            );
        }
    }

    #[test]
    fn an_lldp_chassis_is_strong_and_a_cdp_device_id_is_weak() {
        let nodes = [node(1, "sw1", "10.0.0.1"), node(2, "sw1-mgmt", "10.9.0.1")];
        let report = |ip: &str, id: &str, protocol| PeerReport {
            address: ip.parse().expect("ip"),
            id: id.to_owned(),
            protocol,
        };
        let lldp = Observations {
            peers: vec![
                report("10.0.0.1", "00:1b:54:11:22:33", NeighborProto::Lldp),
                report("10.9.0.1", "00:1b:54:11:22:33", NeighborProto::Lldp),
            ],
            ..Observations::default()
        };
        assert_eq!(
            find(&nodes, &lldp).groups[0].confidence,
            DuplicateConfidence::Confident
        );

        let cdp = Observations {
            peers: vec![
                report("10.0.0.1", "SW1.example.com", NeighborProto::Cdp),
                report("10.9.0.1", "sw1.example.com", NeighborProto::Cdp),
            ],
            ..Observations::default()
        };
        assert_eq!(find(&nodes, &cdp).total, 0, "a hostname alone");
    }

    #[test]
    fn two_weak_kinds_make_a_possible_group() {
        let nodes = [
            node(1, "Edge-FW", "10.0.0.1"),
            node(2, "edge-fw ", "10.9.0.1"),
        ];
        let obs = Observations {
            peers: vec![
                PeerReport {
                    address: "10.0.0.1".parse().expect("ip"),
                    id: "edge-fw".to_owned(),
                    protocol: NeighborProto::Cdp,
                },
                PeerReport {
                    address: "10.9.0.1".parse().expect("ip"),
                    id: "EDGE-FW".to_owned(),
                    protocol: NeighborProto::Cdp,
                },
            ],
            ..Observations::default()
        };
        let f = find(&nodes, &obs);
        assert_eq!(f.total, 1);
        assert_eq!(f.groups[0].confidence, DuplicateConfidence::Possible);
        assert!(f.groups[0].contradictions.is_empty());
    }

    #[test]
    fn a_value_shared_by_too_many_nodes_is_ignored_and_reported() {
        let many: Vec<DuplicateInput> = (1..=9)
            .map(|n| {
                with_serial(
                    node(n, &format!("sim-{n}"), &format!("10.0.0.{n}")),
                    "JPE00000000",
                )
            })
            .collect();
        let f = find(&many, &Observations::default());
        assert_eq!(f.total, 0);
        assert_eq!(
            f.ignored,
            [Ignored {
                kind: DuplicateEvidenceKind::Serial,
                value: "JPE00000000".to_owned(),
                nodes: 9,
            }]
        );
        assert_eq!(f.ignored_total, 1);

        let eight = &many[..SHARED_VALUE_MAX];
        assert_eq!(
            find(eight, &Observations::default()).total,
            1,
            "the cap is inclusive"
        );
    }

    /// Device nodes at one address are the duplicate ADR-139 exists for, however many there are.
    #[test]
    fn the_address_kind_has_no_cap() {
        let many: Vec<DuplicateInput> = (1..=12)
            .map(|n| node(n, "imported-twice", "10.0.0.1"))
            .collect();
        let f = find(&many, &Observations::default());
        assert_eq!(f.total, 1);
        assert_eq!(f.groups[0].members.len(), 12);
        assert_eq!(f.groups[0].confidence, DuplicateConfidence::Confident);
    }

    #[test]
    fn differing_serials_or_models_demote_a_group_and_say_why() {
        let serials = [
            with_serial(node(1, "a", "10.0.0.1"), "FOX1820GVER"),
            with_serial(node(2, "b", "10.0.0.1"), "FDO27161MC0"),
        ];
        let g = &find(&serials, &Observations::default()).groups[0];
        assert_eq!(g.confidence, DuplicateConfidence::Possible);
        assert_eq!(g.contradictions, [DuplicateContradiction::SerialDiffers]);

        let models = [
            with_model(node(1, "a", "10.0.0.1"), "1.3.6.1.4.1.9.1.1208"),
            with_model(node(2, "b", "10.0.0.1"), "1.3.6.1.4.1.2011.2.23.1"),
        ];
        let g = &find(&models, &Observations::default()).groups[0];
        assert_eq!(g.contradictions, [DuplicateContradiction::ModelDiffers]);

        // One side unknown is not a contradiction.
        let half = [
            with_serial(node(1, "a", "10.0.0.1"), "FOX1820GVER"),
            node(2, "b", "10.0.0.1"),
        ];
        assert_eq!(
            find(&half, &Observations::default()).groups[0].confidence,
            DuplicateConfidence::Confident
        );
    }

    #[test]
    fn the_keeper_is_the_most_depended_on_before_the_oldest() {
        let mut parent = node(3, "newer-parent", "10.0.0.1");
        parent.dependents = 4;
        let nodes = [
            node(1, "older", "10.0.0.1"),
            node(2, "middle", "10.0.0.1"),
            parent,
        ];
        let g = &find(&nodes, &Observations::default()).groups[0];
        assert_eq!(g.keeper, id(3));
        assert_eq!(g.members, [id(3), id(1), id(2)], "the rest from the oldest");
    }

    #[test]
    fn evidence_chains_through_a_middle_node_into_one_group() {
        let nodes = [
            with_serial(node(1, "a", "10.0.0.1"), "FOX1820GVER"),
            with_serial(node(2, "b", "10.0.0.2"), "FOX1820GVER"),
            node(3, "c", "10.0.0.2"),
        ];
        let f = find(&nodes, &Observations::default());
        assert_eq!(f.total, 1);
        let g = &f.groups[0];
        assert_eq!(g.members.len(), 3);
        assert_eq!(
            kinds(g),
            [
                DuplicateEvidenceKind::Address,
                DuplicateEvidenceKind::Serial
            ]
        );
        assert_eq!(g.confidence, DuplicateConfidence::Confident);
    }

    #[test]
    fn a_weak_link_to_a_strong_group_makes_the_whole_group_possible() {
        let nodes = [
            node(1, "fw", "10.0.0.1"),
            node(2, "fw", "10.0.0.1"),
            node(3, "fw", "10.9.9.9"),
        ];
        let obs = Observations {
            peers: vec![
                PeerReport {
                    address: "10.0.0.1".parse().expect("ip"),
                    id: "fw".to_owned(),
                    protocol: NeighborProto::Cdp,
                },
                PeerReport {
                    address: "10.9.9.9".parse().expect("ip"),
                    id: "fw".to_owned(),
                    protocol: NeighborProto::Cdp,
                },
            ],
            ..Observations::default()
        };
        let f = find(&nodes, &obs);
        assert_eq!(f.total, 1);
        assert_eq!(f.groups[0].members.len(), 3);
        assert_eq!(f.groups[0].confidence, DuplicateConfidence::Possible);
    }

    #[test]
    fn groups_are_capped_and_the_total_still_counts_them() {
        let nodes: Vec<DuplicateInput> = (0..(GROUPS_MAX as u128 + 5))
            .flat_map(|g| {
                let address = format!("10.{}.{}.1", g / 250, g % 250);
                [
                    node(g * 2 + 1, "x", &address),
                    node(g * 2 + 2, "y", &address),
                ]
            })
            .collect();
        let f = find(&nodes, &Observations::default());
        assert_eq!(f.groups.len(), GROUPS_MAX);
        assert_eq!(f.total, GROUPS_MAX + 5);
    }

    #[test]
    fn confident_groups_come_before_possible_ones() {
        let nodes = [
            node(1, "aaa", "10.0.0.1"),
            node(2, "aaa", "10.0.0.2"),
            with_model(node(3, "zzz", "10.0.0.9"), "1.3.6.1.4.1.9"),
            with_model(node(4, "zzz", "10.0.0.9"), "1.3.6.1.4.1.2011"),
            node(5, "mmm", "10.0.0.7"),
            node(6, "mmm", "10.0.0.7"),
        ];
        let obs = Observations {
            peers: vec![
                PeerReport {
                    address: "10.0.0.1".parse().expect("ip"),
                    id: "aaa".to_owned(),
                    protocol: NeighborProto::Cdp,
                },
                PeerReport {
                    address: "10.0.0.2".parse().expect("ip"),
                    id: "aaa".to_owned(),
                    protocol: NeighborProto::Cdp,
                },
            ],
            ..Observations::default()
        };
        let f = find(&nodes, &obs);
        let order: Vec<DuplicateConfidence> = f.groups.iter().map(|g| g.confidence).collect();
        assert_eq!(
            order,
            [
                DuplicateConfidence::Confident,
                DuplicateConfidence::Possible,
                DuplicateConfidence::Possible
            ]
        );
        assert_eq!(
            f.groups[0].keeper,
            id(5),
            "the confident one, whatever its name"
        );
    }

    /// Observations about nodes the caller cannot see do not become candidates or evidence.
    #[test]
    fn rows_about_nodes_outside_the_candidates_are_ignored() {
        let nodes = [node(1, "visible", "10.0.0.1")];
        let obs = Observations {
            own_addresses: vec![OwnAddress {
                node: id(99),
                ip: "10.0.0.1".parse().expect("ip"),
            }],
            with_address_list: BTreeSet::from([id(99)]),
            meraki_serials: HashMap::from([(id(99), "Q2XX-AAAA-BBBB".to_owned())]),
            ..Observations::default()
        };
        let f = find(&nodes, &obs);
        assert_eq!(f.total, 0);
        assert_eq!(f.with_address_list, 0);
    }

    /// The WebUI names these tokens in its locale files and its `as const` arrays, so a renamed
    /// variant must fail here rather than render a raw key.
    #[test]
    fn every_kind_serializes_to_the_token_the_webui_names() {
        let tokens: Vec<String> = DuplicateEvidenceKind::ALL
            .iter()
            .map(|k| {
                serde_json::to_value(k)
                    .expect("serializes")
                    .as_str()
                    .expect("a string")
                    .to_owned()
            })
            .collect();
        assert_eq!(
            tokens,
            [
                "address",
                "serial",
                "own_ip",
                "own_ip_one_way",
                "arp_mac",
                "lldp_chassis",
                "cdp_device_id",
                "name"
            ]
        );
        let strong: Vec<DuplicateEvidenceKind> = DuplicateEvidenceKind::ALL
            .into_iter()
            .filter(|k| k.is_strong())
            .collect();
        assert_eq!(strong.len(), 5);
        assert_eq!(
            serde_json::to_value(DuplicateContradiction::ModelDiffers).expect("serializes"),
            "model_differs"
        );
        assert_eq!(
            serde_json::to_value(DuplicateConfidence::Possible).expect("serializes"),
            "possible"
        );
    }
}
