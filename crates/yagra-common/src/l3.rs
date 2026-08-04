// SPDX-License-Identifier: AGPL-3.0-only
//! Layer-3 interface addresses: what IP prefixes a node has a foot in (ADR-043).
//!
//! What this is for: answering "which nodes share a subnet", which is the **fact** ADR-043 derives
//! its connectivity graph from. Two nodes with an address in the same prefix are L3-adjacent — that
//! is not an inference, it is what having a foot in a network means.
//!
//! Three things make this module the place the feature succeeds or fails:
//!
//! 1. **Do not walk the routing table.** A core router's `ipCidrRouteTable` runs to hundreds of
//!    thousands of rows in a full-route environment; walking it would kill the device and the poll
//!    budget. `ipAddrTable`/`ipAddressTable` have one row per interface address — *tens* — and they
//!    answer the same question for everything except point-to-point `/32`s. The scale problem does
//!    not get managed here, it gets designed out.
//!
//! 2. **The prefix length hides in an OID, not a column.** The modern `ipAddressTable` has no mask
//!    column: `ipAddressPrefix` is a RowPointer into `ipAddressPrefixTable`, and the prefix length
//!    is the pointer's own last sub-identifier. So there is no need to walk `.4.32` at all — see
//!    [`decode_prefix_pointer`]. This is the first real consumer of
//!    `yagra_transport::SnmpValue::Oid`, which until now was produced by both walkers and read by
//!    nobody.
//!
//! 3. **Masking is done on bytes, never on a `u32`.** One code path serves IPv4 and IPv6, so the
//!    project's "never assume v4" rule is satisfied structurally rather than by discipline. See
//!    [`subnet_key`].
//!
//! Storage follows ADR-038's shape exactly (one JSONB document per node, replaced wholesale, with a
//! history row appended only when the content key moves), and for the same reason: a partial or
//! failed walk must never read as "every address disappeared".

use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Stable TSDB metric: how many interface addresses the node currently reports. Node-level and
/// bounded by [`MAX_ADDRESSES_PER_NODE`], so it is a safe series. The addresses themselves never
/// become one — an IP in a label is the cardinality explosion CLAUDE.md §7.1 warns about.
pub const METRIC_SNMP_L3_ADDRESS_COUNT: &str = "snmp_l3_address_count";

/// Cap on addresses recorded per node. A carrier-facing router can carry hundreds of SVIs; without
/// a cap the JSONB document and the content key grow unbounded. Applied **after** sorting so
/// truncation is deterministic, and the overflow is reported via [`L3Snapshot::truncated`] rather
/// than dropped silently — the [`crate::MAX_NEIGHBORS_PER_NODE`] rule.
pub const MAX_ADDRESSES_PER_NODE: usize = 512;

/// The RowPointer prefix every `ipAddressPrefix` value starts with (`ipAddressPrefixOrigin`).
const OID_IP_ADDRESS_PREFIX_ENTRY: &str = "1.3.6.1.2.1.4.32.1.5";

/// `ipAddrTable` columns (RFC 1213, IPv4 only), in the order [`builtin_l3_columns`] lists them.
const OID_IP_AD_ENT_IF_INDEX: &str = "1.3.6.1.2.1.4.20.1.2";
const OID_IP_AD_ENT_NET_MASK: &str = "1.3.6.1.2.1.4.20.1.3";
/// `ipAddressTable` columns (RFC 4293, v4 + v6).
const OID_IP_ADDRESS_IF_INDEX: &str = "1.3.6.1.2.1.4.34.1.3";
const OID_IP_ADDRESS_TYPE: &str = "1.3.6.1.2.1.4.34.1.4";
const OID_IP_ADDRESS_PREFIX: &str = "1.3.6.1.2.1.4.34.1.5";

/// Which table an address was observed in.
///
/// Kept on the record because the two tables do not carry the same information: the old one is
/// IPv4-only but returns the mask as a plain column, the new one covers both families but hides the
/// prefix length in a RowPointer. When a device answers both — the Huawei USG in the lab does — the
/// new table wins, and this field is what makes that choice inspectable afterwards.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum L3SourceTable {
    /// `ipAddressTable` (`.4.34`) — preferred: v4 + v6, and it carries `ipAddressType`.
    #[default]
    IpAddressTable,
    /// `ipAddrTable` (`.4.20`) — IPv4 only, but the mask is a plain column.
    IpAddrTable,
}

impl L3SourceTable {
    /// Every table, for iteration and coverage tests.
    pub const ALL: [L3SourceTable; 2] = [L3SourceTable::IpAddressTable, L3SourceTable::IpAddrTable];

    /// The stable token stored in the content key.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            L3SourceTable::IpAddressTable => "ip_address_table",
            L3SourceTable::IpAddrTable => "ip_addr_table",
        }
    }

    /// Parse a stored token back into a table.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "ip_address_table" => Some(L3SourceTable::IpAddressTable),
            "ip_addr_table" => Some(L3SourceTable::IpAddrTable),
            _ => None,
        }
    }
}

/// `ipAddressType` — what the agent says the address is for.
///
/// This matters more than it looks: an agent reporting `anycast` is telling us the address is a
/// VRRP/HSRP virtual IP, which is precisely the evidence needed to keep a VIP from anchoring a
/// node onto a segment it does not really terminate. `Unknown` is what an `ipAddrTable`-only device
/// gets, since the old table has no such column.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum L3AddrType {
    /// `unicast(1)` — a real address on this box.
    Unicast,
    /// `anycast(2)` — shared with another router (VRRP/HSRP).
    Anycast,
    /// `broadcast(3)`.
    Broadcast,
    /// Not reported (an `ipAddrTable`-only agent), or a value outside the enumeration.
    #[default]
    Unknown,
}

impl L3AddrType {
    /// Every type, for iteration and coverage tests.
    pub const ALL: [L3AddrType; 4] = [
        L3AddrType::Unicast,
        L3AddrType::Anycast,
        L3AddrType::Broadcast,
        L3AddrType::Unknown,
    ];

    /// The stable token stored in the content key.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            L3AddrType::Unicast => "unicast",
            L3AddrType::Anycast => "anycast",
            L3AddrType::Broadcast => "broadcast",
            L3AddrType::Unknown => "unknown",
        }
    }

    /// Parse a stored token back into a type.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "unicast" => Some(L3AddrType::Unicast),
            "anycast" => Some(L3AddrType::Anycast),
            "broadcast" => Some(L3AddrType::Broadcast),
            "unknown" => Some(L3AddrType::Unknown),
            _ => None,
        }
    }

    /// Map the SNMP enumeration. Anything outside it reads as [`L3AddrType::Unknown`] rather than
    /// being rejected — an unexpected value is a reason to distrust the row's *type*, not to drop
    /// the address.
    #[must_use]
    pub fn from_snmp(value: i64) -> Self {
        match value {
            1 => L3AddrType::Unicast,
            2 => L3AddrType::Anycast,
            3 => L3AddrType::Broadcast,
            _ => L3AddrType::Unknown,
        }
    }

    /// Whether an address of this type identifies *this* node on a segment.
    ///
    /// An anycast or broadcast address does not: several routers answer for it, so treating it as
    /// membership would attach a node to a segment it may only be a standby for.
    #[must_use]
    pub const fn identifies_a_node(self) -> bool {
        match self {
            // `Unknown` is included on purpose: an `ipAddrTable`-only device reports every address
            // that way, and excluding it would make the whole old-MIB path produce no edges.
            L3AddrType::Unicast | L3AddrType::Unknown => true,
            L3AddrType::Anycast | L3AddrType::Broadcast => false,
        }
    }
}

/// One interface address, with the prefix length that says which network it is in.
///
/// The identity is the **address**: a device does not hold the same IP twice, and the two source
/// tables report overlapping sets that must collapse to one record each.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct L3Address {
    /// The `ifIndex` this address is configured on. Joins to the interface inventory.
    pub ifindex: u32,
    /// The address itself. Serialized as a string, and never narrowed to v4.
    pub ip: IpAddr,
    /// Prefix length in bits. `0` means the agent gave one we could not decode — the address is
    /// still recorded (Increment 4 needs the host routes) but it can form no subnet edge.
    pub prefix_len: u8,
    /// What the agent says the address is for.
    #[serde(default)]
    pub addr_type: L3AddrType,
    /// Which table it came from.
    #[serde(default)]
    pub source_table: L3SourceTable,
}

impl L3Address {
    /// A record with the two things every address must have.
    #[must_use]
    pub fn new(ifindex: u32, ip: IpAddr, prefix_len: u8) -> Self {
        Self {
            ifindex,
            ip,
            prefix_len,
            addr_type: L3AddrType::Unknown,
            source_table: L3SourceTable::IpAddressTable,
        }
    }

    /// Whether the prefix covers a single host (`/32` on v4, `/128` on v6).
    ///
    /// These are real and are deliberately **stored** — a PPPoE `Dialer` or a tunnel endpoint is a
    /// genuine interface address, and Increment 4 resolves its peer through the routing table. They
    /// simply cannot form a *shared-subnet* edge, because they share no subnet with anything.
    #[must_use]
    pub const fn is_host_route(&self) -> bool {
        match self.ip {
            IpAddr::V4(_) => self.prefix_len == 32,
            IpAddr::V6(_) => self.prefix_len == 128,
        }
    }

    /// Whether this address can place its node on a shared segment.
    ///
    /// Excludes, with a reason for each: addresses that identify no single node (anycast VIPs),
    /// host routes (no shared subnet — Increment 4), an undecodable prefix, and the address classes
    /// that are either node-local or not a network at all. Loopback and link-local are the
    /// important ones: every device has them, and every device's `127.0.0.1` would otherwise
    /// "share a subnet" with every other device's, wiring the entire fleet into one segment.
    #[must_use]
    pub fn can_form_subnet_edge(&self) -> bool {
        if !self.addr_type.identifies_a_node() || self.is_host_route() || self.prefix_len == 0 {
            return false;
        }
        match self.ip {
            IpAddr::V4(v4) => {
                !v4.is_loopback()
                    && !v4.is_link_local()
                    && !v4.is_multicast()
                    && !v4.is_unspecified()
            }
            IpAddr::V6(v6) => {
                !v6.is_loopback()
                    && !v6.is_multicast()
                    && !v6.is_unspecified()
                    && !is_v6_link_local(v6)
            }
        }
    }

    /// The subnet this address sits in, or `None` when it cannot form one.
    #[must_use]
    pub fn subnet(&self) -> Option<SubnetKey> {
        if !self.can_form_subnet_edge() {
            return None;
        }
        subnet_key(self.ip, self.prefix_len)
    }
}

/// `fe80::/10`. `Ipv6Addr::is_unicast_link_local` is still unstable, so this is spelled out.
fn is_v6_link_local(addr: Ipv6Addr) -> bool {
    (addr.segments()[0] & 0xffc0) == 0xfe80
}

/// A network: the masked address plus the prefix length that produced it.
///
/// Two addresses are in the same subnet only when the **whole** key matches. Requiring the prefix
/// length to agree is deliberate (ADR-043 D9): `10.0.0.1/24` beside `10.0.0.2/16` is a
/// misconfiguration, not an adjacency, and this is the only rule with no guessing in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubnetKey {
    /// The network address, i.e. the address with every host bit cleared.
    pub network: IpAddr,
    /// Prefix length in bits.
    pub prefix_len: u8,
}

impl fmt::Display for SubnetKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix_len)
    }
}

/// Mask an address down to its network.
///
/// Operates on the raw octets — 4 of them or 16 — so there is exactly one implementation for both
/// families. Returns `None` for a prefix length that is zero or wider than the family allows; a
/// `/0` is not a subnet anyone shares in any useful sense, and an out-of-range length means the
/// agent's row was malformed.
#[must_use]
pub fn subnet_key(ip: IpAddr, prefix_len: u8) -> Option<SubnetKey> {
    let max = match ip {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    };
    if prefix_len == 0 || prefix_len > max {
        return None;
    }
    let network = match ip {
        IpAddr::V4(v4) => {
            let mut octets = v4.octets();
            mask_octets(&mut octets, prefix_len);
            IpAddr::V4(Ipv4Addr::from(octets))
        }
        IpAddr::V6(v6) => {
            let mut octets = v6.octets();
            mask_octets(&mut octets, prefix_len);
            IpAddr::V6(Ipv6Addr::from(octets))
        }
    };
    Some(SubnetKey {
        network,
        prefix_len,
    })
}

/// Clear every bit below `prefix_len`, in place. Family-agnostic by construction.
fn mask_octets(octets: &mut [u8], prefix_len: u8) {
    let full = usize::from(prefix_len / 8);
    let rem = u32::from(prefix_len % 8);
    for (i, byte) in octets.iter_mut().enumerate() {
        if i < full {
            continue;
        }
        if i == full && rem > 0 {
            // Keep the top `rem` bits of this byte, clear the rest.
            *byte &= !(0xffu8 >> rem);
        } else {
            *byte = 0;
        }
    }
}

/// Decode an `ipAddressPrefix` RowPointer into the network it points at.
///
/// The value is an OID pointing into `ipAddressPrefixTable`, whose index is
/// `(ipAddressPrefixIfIndex, ipAddressPrefixType, ipAddressPrefixPrefix, ipAddressPrefixLength)`:
///
/// ```text
/// 1.3.6.1.2.1.4.32.1.5 . <ifIndex> . <addrType> . <addrLen> . <addrLen octets> . <prefixLen>
/// ```
///
/// So the prefix length is simply the **last** sub-identifier, and `ipAddressPrefixTable` never has
/// to be walked. Confirmed against a real Huawei USG6530F-D:
/// `…4.32.1.5.8.1.4.192.168.1.0.24` ⇒ ifIndex 8, `192.168.1.0/24`, and
/// `…4.32.1.5.16.1.4.133.123.189.109.32` ⇒ `133.123.189.109/32` (a PPPoE `Dialer`).
///
/// Returns the ifIndex the pointer itself names alongside the network, because it is the fallback
/// when `ipAddressIfIndex` is missing from the walk.
///
/// `None` — counted by the caller, never silently swallowed — for `zeroDotZero` (`"0.0"`, which
/// RFC 4293 explicitly permits when the agent has no prefix row), a pointer into a different table,
/// a length/type disagreement, or an octet that cannot be one.
#[must_use]
pub fn decode_prefix_pointer(oid: &str) -> Option<(u32, SubnetKey)> {
    let rest = oid
        .strip_prefix(OID_IP_ADDRESS_PREFIX_ENTRY)?
        .strip_prefix('.')?;
    let sub: Vec<u32> = rest
        .split('.')
        .map(|p| p.parse::<u32>().ok())
        .collect::<Option<_>>()?;
    // ifIndex, addrType, addrLen, <addrLen octets>, prefixLen
    let (&ifindex, &addr_type) = match sub.as_slice() {
        [a, b, ..] => (a, b),
        _ => return None,
    };
    // The address occupies `sub[2..]` minus the trailing prefix length.
    let inet = sub.get(2..sub.len().checked_sub(1)?)?;
    let ip = inet_address_from_index(addr_type, inet)?;
    let prefix_len = u8::try_from(*sub.last()?).ok()?;
    Some((ifindex, subnet_key(ip, prefix_len)?))
}

/// Decode an `InetAddressType` plus a length-prefixed `InetAddress`, as the pair appears inside a
/// table's **row index**.
///
/// `sub` must start at the length sub-identifier: `[len, octet, octet, …]`. Both `ipAddressTable`
/// and `ipAddressPrefixTable` index their rows this way, so this is the one place that mapping is
/// written down — the alternative was the same eight-line rule in two files, which is how the third
/// copy eventually gets it wrong.
///
/// `InetAddressType` is `ipv4(1)`, `ipv6(2)`, `ipv4z(3)`, `ipv6z(4)`. The zoned forms carry a
/// four-byte zone index after the address, which is **dropped**: a zone scopes how you reach an
/// address, not which subnet it belongs to.
#[must_use]
pub fn inet_address_from_index(addr_type: u32, sub: &[u32]) -> Option<IpAddr> {
    let addr_len = usize::try_from(*sub.first()?).ok()?;
    let octets: Vec<u8> = sub
        .get(1..=addr_len)?
        .iter()
        .map(|v| u8::try_from(*v).ok())
        .collect::<Option<_>>()?;
    // Anything past the declared length is the zone index (or nothing at all).
    if sub.len() != 1 + addr_len {
        return None;
    }
    match (addr_type, addr_len) {
        (1, 4) | (3, 8) => Some(IpAddr::V4(Ipv4Addr::new(
            octets[0], octets[1], octets[2], octets[3],
        ))),
        (2, 16) | (4, 20) => {
            let mut b = [0u8; 16];
            b.copy_from_slice(&octets[..16]);
            Some(IpAddr::V6(Ipv6Addr::from(b)))
        }
        _ => None,
    }
}

/// Turn an `ipAddrTable` netmask (four octets, e.g. `255.255.255.0`) into a prefix length.
///
/// Rejects a non-contiguous mask rather than guessing: `255.0.255.0` is not a prefix, and counting
/// its set bits would invent one that the device never configured.
#[must_use]
pub fn prefix_len_from_mask(octets: [u8; 4]) -> Option<u8> {
    let bits = u32::from_be_bytes(octets);
    let ones = bits.leading_ones();
    // Everything below the leading run must be zero for the mask to be a prefix.
    if ones == 32 || (bits << ones) == 0 {
        u8::try_from(ones).ok()
    } else {
        None
    }
}

/// Every interface address a node reports on one observation, as a set.
///
/// The unit of storage is the whole set per node, for the same reason [`crate::NeighborSet`] is: a
/// partial walk must never read as "every address disappeared". A failed walk sends no set at all
/// (`PollResult.l3 = None`) so nothing is written; an **empty** set is a real observation and does
/// replace the stored one, which is how a device that lost its addressing stops showing stale ones.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct L3Snapshot {
    /// The addresses, canonically ordered.
    #[serde(default)]
    pub addresses: Vec<L3Address>,
    /// Whether [`MAX_ADDRESSES_PER_NODE`] was hit and rows were dropped.
    #[serde(default)]
    pub truncated: bool,
}

impl L3Snapshot {
    /// Build a snapshot from observed records, canonicalizing immediately.
    #[must_use]
    pub fn new(addresses: Vec<L3Address>) -> Self {
        let mut set = Self {
            addresses,
            truncated: false,
        };
        set.canonicalize();
        set
    }

    /// How many addresses the snapshot holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.addresses.len()
    }

    /// Whether the node reported no address at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.addresses.is_empty()
    }

    /// Every distinct subnet this node has a foot in.
    ///
    /// This is what makes a node a transit candidate: a box with an address in two or more networks
    /// forwards between them, and one with a single network is an endpoint. ADR-043 決定 2 turns
    /// that count into the containment rule, so it is read off the observation rather than guessed.
    #[must_use]
    pub fn subnets(&self) -> std::collections::BTreeSet<SubnetKey> {
        self.addresses
            .iter()
            .filter_map(L3Address::subnet)
            .collect()
    }

    /// Put the snapshot in canonical form so two observations of an unchanged device compare equal.
    ///
    /// 1. Records are sorted by `(ip, source_table, ifindex)`. Sorting by source table before
    ///    dedup is what implements the two-table precedence: [`L3SourceTable::IpAddressTable`]
    ///    orders first, so when a device answers both walks its `.4.34` row is the one kept.
    /// 2. One record survives per address — a device does not hold the same IP twice, and the two
    ///    tables report overlapping sets.
    /// 3. Truncation to [`MAX_ADDRESSES_PER_NODE`] happens *after* sorting, so which records
    ///    survive does not depend on walk order.
    pub fn canonicalize(&mut self) {
        self.addresses.sort_by(|a, b| {
            (a.ip, a.source_table, a.ifindex).cmp(&(b.ip, b.source_table, b.ifindex))
        });
        self.addresses.dedup_by(|a, b| a.ip == b.ip);
        if self.addresses.len() > MAX_ADDRESSES_PER_NODE {
            self.addresses.truncate(MAX_ADDRESSES_PER_NODE);
            self.truncated = true;
        }
    }

    /// The stable content key used for append-on-change comparison.
    ///
    /// ⚠️ This encoding is effectively a wire format. Changing it re-keys every stored snapshot and
    /// emits exactly one spurious change row per node on the first poll after the upgrade. It is
    /// versioned (`v1` first line); bump the version and say so in RELEASE_NOTES if it changes. It
    /// is built by hand rather than through `serde_json` precisely so serde's output cannot drift
    /// underneath it — the same reasoning as [`crate::NeighborSet::content_key`].
    ///
    /// Call [`Self::canonicalize`] first — this reads the snapshot as-is and does not sort.
    #[must_use]
    pub fn content_key(&self) -> String {
        let mut out = String::from("v1\n");
        for a in &self.addresses {
            // No field here is device-supplied free text — every one is a number, an address or a
            // fixed token — so no value can forge a line boundary.
            out.push_str("a=");
            out.push_str(&a.ip.to_string());
            out.push('\n');
            out.push_str("p=");
            out.push_str(&a.prefix_len.to_string());
            out.push('\n');
            out.push_str("i=");
            out.push_str(&a.ifindex.to_string());
            out.push('\n');
            out.push_str("t=");
            out.push_str(a.addr_type.as_str());
            out.push('\n');
            out.push_str("s=");
            out.push_str(a.source_table.as_str());
            out.push('\n');
        }
        // Truncation is part of the content: a node that grew past the cap has genuinely changed
        // what we can see of it, and the operator should get a history row saying so.
        out.push_str(if self.truncated { "x=1\n" } else { "x=0\n" });
        out
    }
}

// ── The fixed column list (a bus contract) ───────────────────────────────────────────

/// Which L3 column an OID base carries. Typed rather than matched on the OID string so the poller's
/// assembly is a `match` the compiler checks, exactly as [`crate::NeighborColumn`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum L3Column {
    /// `ipAdEntIfIndex` — old table; the instance *is* the IPv4 address.
    IpAdEntIfIndex,
    /// `ipAdEntNetMask` — old table; the mask as a plain `IpAddress` column.
    IpAdEntNetMask,
    /// `ipAddressIfIndex` — new table.
    IpAddressIfIndex,
    /// `ipAddressType` — new table; unicast / anycast / broadcast.
    IpAddressType,
    /// `ipAddressPrefix` — new table; a RowPointer whose tail carries the prefix length.
    IpAddressPrefix,
}

/// The L3 columns and their OID bases — RFC 1213 and RFC 4293, both fixed standards.
///
/// `ipAdEntAddr` (`.4.20.1.1`) and `ipAddressAddr` are deliberately **not** walked: in both tables
/// the address is the row's *index*, so it arrives on every column already. Walking it would be a
/// round trip that returns what we have.
///
/// Both tables are walked, not one. `.4.20` is IPv4-only by design and `.4.34` has no mask column;
/// taking only the cheap one would break the project's "never assume v4" rule, and taking only the
/// modern one loses devices that answer just the old table.
///
/// **This list is the bus contract**; keep it stable for N/N-1 compatibility.
#[must_use]
pub fn builtin_l3_columns() -> Vec<(L3Column, &'static str)> {
    vec![
        (L3Column::IpAdEntIfIndex, OID_IP_AD_ENT_IF_INDEX),
        (L3Column::IpAdEntNetMask, OID_IP_AD_ENT_NET_MASK),
        (L3Column::IpAddressIfIndex, OID_IP_ADDRESS_IF_INDEX),
        (L3Column::IpAddressType, OID_IP_ADDRESS_TYPE),
        (L3Column::IpAddressPrefix, OID_IP_ADDRESS_PREFIX),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str) -> IpAddr {
        IpAddr::V4(s.parse().unwrap())
    }
    fn v6(s: &str) -> IpAddr {
        IpAddr::V6(s.parse().unwrap())
    }

    #[test]
    fn subnet_key_masks_by_bytes_for_both_families() {
        assert_eq!(
            subnet_key(v4("192.168.1.42"), 24).unwrap().to_string(),
            "192.168.1.0/24"
        );
        // A prefix that does not land on a byte boundary exercises the partial-byte branch.
        assert_eq!(
            subnet_key(v4("10.1.2.130"), 26).unwrap().to_string(),
            "10.1.2.128/26"
        );
        assert_eq!(
            subnet_key(v4("172.16.5.9"), 30).unwrap().to_string(),
            "172.16.5.8/30"
        );
        assert_eq!(
            subnet_key(v6("2001:db8::dead:beef"), 64)
                .unwrap()
                .to_string(),
            "2001:db8::/64"
        );
        assert_eq!(
            subnet_key(v6("2001:db8:1:2:3::1"), 48).unwrap().to_string(),
            "2001:db8:1::/48"
        );
        // Out of range for the family, and /0, yield nothing rather than a bogus network.
        assert_eq!(subnet_key(v4("10.0.0.1"), 33), None);
        assert_eq!(subnet_key(v6("2001:db8::1"), 129), None);
        assert_eq!(subnet_key(v4("10.0.0.1"), 0), None);
    }

    #[test]
    fn a_31_is_a_real_point_to_point() {
        // RFC 3021. Dropping /31 as if it were a host route would delete most modern router-to-
        // router links, which is the off-by-one this test exists to prevent.
        let a = L3Address::new(1, v4("10.0.0.0"), 31);
        let b = L3Address::new(1, v4("10.0.0.1"), 31);
        assert!(!a.is_host_route());
        assert!(a.can_form_subnet_edge());
        assert_eq!(a.subnet(), b.subnet());
        assert_eq!(a.subnet().unwrap().to_string(), "10.0.0.0/31");
    }

    #[test]
    fn host_routes_are_stored_but_form_no_subnet() {
        // The lab's real PPPoE WAN address.
        let dialer = L3Address::new(16, v4("133.123.189.109"), 32);
        assert!(dialer.is_host_route());
        assert!(!dialer.can_form_subnet_edge());
        assert_eq!(dialer.subnet(), None);
        // Two unrelated /32s must not "share" 0.0.0.0/32 or each other.
        let other = L3Address::new(3, v4("203.0.113.7"), 32);
        assert_eq!(other.subnet(), None);
        // v6 host routes behave identically.
        let v6host = L3Address::new(4, v6("2001:db8::1"), 128);
        assert!(v6host.is_host_route());
        assert_eq!(v6host.subnet(), None);
    }

    #[test]
    fn node_local_address_classes_never_join_a_segment() {
        // Every device has 127.0.0.1; if it could form an edge the whole fleet would be one segment.
        for (ip, prefix) in [
            (v4("127.0.0.1"), 8),
            (v4("169.254.10.1"), 16),
            (v4("224.0.0.5"), 4),
            (v4("0.0.0.0"), 8),
            (v6("::1"), 128),
            (v6("fe80::1"), 64),
            (v6("ff02::1"), 16),
        ] {
            let a = L3Address::new(1, ip, prefix);
            assert!(!a.can_form_subnet_edge(), "{ip} must not form an edge");
        }
    }

    #[test]
    fn an_anycast_address_is_excluded_but_unknown_is_not() {
        let mut vip = L3Address::new(1, v4("10.0.0.1"), 24);
        vip.addr_type = L3AddrType::Anycast;
        assert!(
            !vip.can_form_subnet_edge(),
            "an HSRP/VRRP VIP anchors nothing"
        );

        let mut real = L3Address::new(1, v4("10.0.0.2"), 24);
        real.addr_type = L3AddrType::Broadcast;
        assert!(!real.can_form_subnet_edge());

        // An ipAddrTable-only device reports every address as Unknown; excluding those would make
        // the entire old-MIB path produce no edges at all.
        let old = L3Address::new(1, v4("10.0.0.3"), 24);
        assert_eq!(old.addr_type, L3AddrType::Unknown);
        assert!(old.can_form_subnet_edge());
    }

    #[test]
    fn mismatched_prefix_lengths_do_not_join() {
        // Same network bits, different masks: a misconfiguration, not an adjacency (D9).
        let a = L3Address::new(1, v4("10.0.0.1"), 24);
        let b = L3Address::new(1, v4("10.0.0.2"), 16);
        assert_ne!(a.subnet(), b.subnet());
    }

    #[test]
    fn decode_prefix_pointer_reads_the_real_captures() {
        // Both of these came off the lab's Huawei USG6530F-D.
        let (ifindex, key) =
            decode_prefix_pointer("1.3.6.1.2.1.4.32.1.5.8.1.4.192.168.1.0.24").unwrap();
        assert_eq!(ifindex, 8);
        assert_eq!(key.to_string(), "192.168.1.0/24");

        let (ifindex, key) =
            decode_prefix_pointer("1.3.6.1.2.1.4.32.1.5.16.1.4.133.123.189.109.32").unwrap();
        assert_eq!(ifindex, 16);
        assert_eq!(key.to_string(), "133.123.189.109/32");
    }

    #[test]
    fn decode_prefix_pointer_handles_v6_and_the_zoned_forms() {
        // ipv6(2), 16 octets, /64.
        let mut oid = String::from("1.3.6.1.2.1.4.32.1.5.7.2.16");
        for b in [0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0] {
            oid.push('.');
            oid.push_str(&b.to_string());
        }
        oid.push_str(".64");
        let (ifindex, key) = decode_prefix_pointer(&oid).unwrap();
        assert_eq!(ifindex, 7);
        assert_eq!(key.to_string(), "2001:db8::/64");

        // ipv4z(3): four address octets plus a four-byte zone index, which is dropped.
        let (_, key) =
            decode_prefix_pointer("1.3.6.1.2.1.4.32.1.5.9.3.8.10.1.2.0.0.0.0.1.24").unwrap();
        assert_eq!(key.to_string(), "10.1.2.0/24");
    }

    #[test]
    fn decode_prefix_pointer_rejects_every_malformed_shape() {
        for bad in [
            // zeroDotZero — RFC 4293 permits it when the agent has no prefix row.
            "0.0",
            // A pointer into some other table.
            "1.3.6.1.2.1.4.33.1.5.8.1.4.192.168.1.0.24",
            // addrLen disagrees with the octets actually present.
            "1.3.6.1.2.1.4.32.1.5.8.1.4.192.168.1.24",
            // An octet that cannot be one.
            "1.3.6.1.2.1.4.32.1.5.8.1.4.192.168.300.0.24",
            // A type/length pair the MIB does not define.
            "1.3.6.1.2.1.4.32.1.5.8.9.4.192.168.1.0.24",
            // Prefix length out of range for the family.
            "1.3.6.1.2.1.4.32.1.5.8.1.4.192.168.1.0.33",
            // Truncated to the point of carrying no index.
            "1.3.6.1.2.1.4.32.1.5.8",
            // Not a number.
            "1.3.6.1.2.1.4.32.1.5.8.1.4.192.168.1.0.x",
        ] {
            assert_eq!(decode_prefix_pointer(bad), None, "{bad} must not decode");
        }
    }

    #[test]
    fn prefix_len_from_mask_rejects_a_non_contiguous_mask() {
        assert_eq!(prefix_len_from_mask([255, 255, 255, 0]), Some(24));
        assert_eq!(prefix_len_from_mask([255, 255, 255, 255]), Some(32));
        assert_eq!(prefix_len_from_mask([255, 255, 255, 252]), Some(30));
        assert_eq!(prefix_len_from_mask([0, 0, 0, 0]), Some(0));
        // Counting set bits here would invent /16 for a mask nobody configured.
        assert_eq!(prefix_len_from_mask([255, 0, 255, 0]), None);
        assert_eq!(prefix_len_from_mask([255, 255, 0, 1]), None);
    }

    #[test]
    fn the_new_table_wins_when_a_device_answers_both() {
        // The Huawei USG answers .4.20 and .4.34 for the same address. One record must survive, and
        // it must be the modern one — it is the only source of `ipAddressType`.
        let mut old = L3Address::new(8, v4("192.168.1.1"), 24);
        old.source_table = L3SourceTable::IpAddrTable;
        let mut new = L3Address::new(8, v4("192.168.1.1"), 24);
        new.source_table = L3SourceTable::IpAddressTable;
        new.addr_type = L3AddrType::Unicast;

        let snap = L3Snapshot::new(vec![old, new]);
        assert_eq!(snap.len(), 1);
        assert_eq!(
            snap.addresses[0].source_table,
            L3SourceTable::IpAddressTable
        );
        assert_eq!(snap.addresses[0].addr_type, L3AddrType::Unicast);
    }

    #[test]
    fn canonicalization_is_order_independent_and_the_key_follows() {
        let a = L3Address::new(1, v4("10.0.0.1"), 24);
        let b = L3Address::new(2, v4("10.0.1.1"), 24);
        let c = L3Address::new(3, v6("2001:db8::1"), 64);
        let one = L3Snapshot::new(vec![a.clone(), b.clone(), c.clone()]);
        let two = L3Snapshot::new(vec![c, a, b]);
        assert_eq!(one, two);
        assert_eq!(one.content_key(), two.content_key());
    }

    #[test]
    fn the_content_key_moves_only_on_a_real_change() {
        let base = L3Snapshot::new(vec![L3Address::new(1, v4("10.0.0.1"), 24)]);
        let same = L3Snapshot::new(vec![L3Address::new(1, v4("10.0.0.1"), 24)]);
        assert_eq!(base.content_key(), same.content_key());

        // A re-masked interface is a real change.
        let remasked = L3Snapshot::new(vec![L3Address::new(1, v4("10.0.0.1"), 25)]);
        assert_ne!(base.content_key(), remasked.content_key());
        // So is the address moving to another port.
        let moved = L3Snapshot::new(vec![L3Address::new(9, v4("10.0.0.1"), 24)]);
        assert_ne!(base.content_key(), moved.content_key());
        // And so is losing the address entirely.
        assert_ne!(base.content_key(), L3Snapshot::default().content_key());
    }

    #[test]
    fn truncation_is_deterministic_and_declares_itself() {
        let many: Vec<L3Address> = (0..MAX_ADDRESSES_PER_NODE + 10)
            .map(|i| {
                #[allow(clippy::cast_possible_truncation)]
                let ip = IpAddr::V4(Ipv4Addr::from((10u32 << 24) + i as u32));
                L3Address::new(1, ip, 24)
            })
            .collect();
        let mut shuffled = many.clone();
        shuffled.reverse();

        let a = L3Snapshot::new(many);
        let b = L3Snapshot::new(shuffled);
        assert_eq!(a.len(), MAX_ADDRESSES_PER_NODE);
        assert!(a.truncated, "the cap must be surfaced, not swallowed");
        assert_eq!(a, b, "which records survive must not depend on walk order");
    }

    #[test]
    fn subnets_counts_distinct_networks_not_addresses() {
        // Two addresses in one network plus one in another: a two-network node, which is what
        // makes it a transit candidate in the containment rule.
        let snap = L3Snapshot::new(vec![
            L3Address::new(1, v4("10.0.0.1"), 24),
            L3Address::new(1, v4("10.0.0.2"), 24),
            L3Address::new(2, v4("10.0.1.1"), 24),
            // Neither of these counts.
            L3Address::new(3, v4("127.0.0.1"), 8),
            L3Address::new(4, v4("198.51.100.1"), 32),
        ]);
        assert_eq!(snap.subnets().len(), 2);
    }

    #[test]
    fn the_column_list_is_the_five_the_adr_names_and_each_oid_is_distinct() {
        let cols = builtin_l3_columns();
        assert_eq!(cols.len(), 5);
        let mut oids: Vec<&str> = cols.iter().map(|(_, o)| *o).collect();
        oids.sort_unstable();
        oids.dedup();
        assert_eq!(
            oids.len(),
            5,
            "a duplicated OID base would merge two columns"
        );
        // ipAdEntAddr / ipAddressAddr are the index, never a walked column.
        assert!(!oids.contains(&"1.3.6.1.2.1.4.20.1.1"));
    }

    #[test]
    fn every_token_round_trips_through_serde_and_from_token() {
        for t in L3AddrType::ALL {
            assert_eq!(L3AddrType::from_token(t.as_str()), Some(t));
            let json = serde_json::to_string(&t).unwrap();
            assert_eq!(json, format!("\"{}\"", t.as_str()));
        }
        for s in L3SourceTable::ALL {
            assert_eq!(L3SourceTable::from_token(s.as_str()), Some(s));
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(json, format!("\"{}\"", s.as_str()));
        }
    }
}
