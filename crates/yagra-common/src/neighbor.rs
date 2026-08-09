// SPDX-License-Identifier: AGPL-3.0-only
//! CDP/LLDP neighbour records: one normalized model over two protocols, plus the change key
//! (ADR-038).
//!
//! What this is for: answering "which of my ports is wired to what, and **when did that change**".
//! It is deliberately *not* topology generation (requirement §4.2 stays unscheduled) and it feeds
//! no alert — a neighbour change is an event, not a threshold, and firing one per port during a
//! patching session is exactly the notification flood the alert rules exist to prevent.
//!
//! Three things make this module the place where the feature succeeds or fails:
//!
//! 1. **Neighbours cannot go in the TSDB.** A chassis id or port id is unbounded device-supplied
//!    text, and [`crate::SeriesKey`] is fixed at `{node, ifindex, metric}` (ADR-011). Putting one
//!    in a label is the cardinality explosion CLAUDE.md §7.1 names as the project's biggest risk.
//!    Adjacency therefore travels on `PollResult.neighbors` and lands in PostgreSQL, the same tier
//!    as the interface inventory and the DNS chain. Only the count becomes a metric.
//!
//! 2. **The identity must exclude everything the agent churns.** `lldpRemTable` is indexed by
//!    `(lldpRemTimeMark, lldpRemLocalPortNum, lldpRemIndex)`: `timeMark` moves on every refresh and
//!    `remIndex` is reassigned at the agent's discretion. Key on either and every poll registers a
//!    "change", turning the history into a poll log — the same failure ADR-033 avoided by dropping
//!    TTLs and round-robin ordering from [`crate::DnsChain::content_key`]. So identity here is
//!    exactly `(local port, remote chassis id, remote port id)` and nothing else.
//!
//! 3. **Ids are typed octet strings, not text.** `lldpRemChassisIdSubtype = 4` means the value is a
//!    raw six-byte MAC; reading it as UTF-8 yields replacement characters and a key that can differ
//!    between polls of the same unchanged link. Every id is therefore rendered through
//!    [`render_chassis_id`] / [`render_port_id`], which pick MAC / network-address / text / hex by
//!    subtype and **never** decode lossily.

use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, Ipv6Addr};

/// Stable TSDB metric: how many neighbours the node currently reports. Node-level and bounded by
/// [`MAX_NEIGHBORS_PER_NODE`], so it is a safe series; the adjacency itself never becomes one.
pub const METRIC_SNMP_NEIGHBOR_COUNT: &str = "snmp_neighbor_count";

/// Cap on neighbours recorded per node. A misconfigured hub port or a shared segment can present
/// hundreds of entries; without a cap the JSONB document and the content key grow unbounded.
/// Applied **after** sorting so truncation is deterministic, and the overflow is reported via
/// [`NeighborSet::truncated`] rather than dropped silently.
pub const MAX_NEIGHBORS_PER_NODE: usize = 256;

/// Row budget for the adjacency walk itself, across all of its columns.
///
/// Distinct from [`MAX_NEIGHBORS_PER_NODE`], which caps the *assembled* result: nineteen columns
/// describe one neighbour, so the walk legitimately reads far more rows than it keeps. Sized at
/// `256 × 19` rounded up, so a device at the entity cap is never truncated by the row cap first —
/// the two limits would otherwise interact in a way that made the reported `truncated` flag lie.
pub const MAX_NEIGHBOR_WALK_ROWS: usize = 8192;

/// Cap on any single device-supplied string, in characters. `sysDescr` in particular runs to
/// hundreds of bytes on some agents and is pure payload.
pub(crate) const MAX_FIELD_CHARS: usize = 255;

/// `lldpLocPortTable` columns, in the order [`builtin_neighbor_columns`] lists them.
const OID_LLDP_LOC_PORT_ID_SUBTYPE: &str = "1.0.8802.1.1.2.1.3.7.1.2";
const OID_LLDP_LOC_PORT_ID: &str = "1.0.8802.1.1.2.1.3.7.1.3";
const OID_LLDP_LOC_PORT_DESC: &str = "1.0.8802.1.1.2.1.3.7.1.4";
/// `lldpRemTable` columns.
const OID_LLDP_REM_CHASSIS_ID_SUBTYPE: &str = "1.0.8802.1.1.2.1.4.1.1.4";
const OID_LLDP_REM_CHASSIS_ID: &str = "1.0.8802.1.1.2.1.4.1.1.5";
const OID_LLDP_REM_PORT_ID_SUBTYPE: &str = "1.0.8802.1.1.2.1.4.1.1.6";
const OID_LLDP_REM_PORT_ID: &str = "1.0.8802.1.1.2.1.4.1.1.7";
const OID_LLDP_REM_PORT_DESC: &str = "1.0.8802.1.1.2.1.4.1.1.8";
const OID_LLDP_REM_SYS_NAME: &str = "1.0.8802.1.1.2.1.4.1.1.9";
const OID_LLDP_REM_SYS_DESC: &str = "1.0.8802.1.1.2.1.4.1.1.10";
const OID_LLDP_REM_SYS_CAP_ENABLED: &str = "1.0.8802.1.1.2.1.4.1.1.12";
/// `lldpRemManAddrTable` — walked for its **index**, not its value (ADR-043).
///
/// `lldpRemManAddrSubtype` and `lldpRemManAddr` are `not-accessible`: the peer's management address
/// is carried in the row index, as
/// `(lldpRemTimeMark, lldpRemLocalPortNum, lldpRemIndex, addrSubtype, addrLen, addr…)`. So any
/// accessible column of the table will do, and `lldpRemManAddrIfSubtype` is the cheapest. What
/// comes back is discarded; the address is decoded from the instance.
const OID_LLDP_REM_MAN_ADDR_IF_SUBTYPE: &str = "1.0.8802.1.1.2.1.4.2.1.3";
/// CISCO-CDP-MIB columns.
const OID_CDP_INTERFACE_NAME: &str = "1.3.6.1.4.1.9.9.23.1.1.1.1.6";
const OID_CDP_CACHE_ADDRESS_TYPE: &str = "1.3.6.1.4.1.9.9.23.1.2.1.1.3";
const OID_CDP_CACHE_ADDRESS: &str = "1.3.6.1.4.1.9.9.23.1.2.1.1.4";
const OID_CDP_CACHE_DEVICE_ID: &str = "1.3.6.1.4.1.9.9.23.1.2.1.1.6";
const OID_CDP_CACHE_DEVICE_PORT: &str = "1.3.6.1.4.1.9.9.23.1.2.1.1.7";
const OID_CDP_CACHE_PLATFORM: &str = "1.3.6.1.4.1.9.9.23.1.2.1.1.8";
const OID_CDP_CACHE_CAPABILITIES: &str = "1.3.6.1.4.1.9.9.23.1.2.1.1.9";

/// Which discovery protocol reported an adjacency.
///
/// Kept on every record rather than normalized away: a switch running both protocols reports the
/// same physical link twice with *different* identities (LLDP names the peer by chassis MAC, CDP by
/// hostname), so collapsing them would mean guessing that two differently-identified rows are one
/// link. Showing both, labelled, is the honest answer.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NeighborProto {
    /// IEEE 802.1AB LLDP (`lldpRemTable`).
    Lldp,
    /// Cisco Discovery Protocol (`cdpCacheTable`).
    Cdp,
}

impl NeighborProto {
    /// Every protocol, for iteration and coverage tests.
    pub const ALL: [NeighborProto; 2] = [NeighborProto::Lldp, NeighborProto::Cdp];

    /// The stable token stored in the change key and rendered by the UI.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            NeighborProto::Lldp => "lldp",
            NeighborProto::Cdp => "cdp",
        }
    }

    /// Parse a stored token back into a protocol.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "lldp" => Some(NeighborProto::Lldp),
            "cdp" => Some(NeighborProto::Cdp),
            _ => None,
        }
    }
}

/// What the peer says it is, as one vocabulary shared by both protocols.
///
/// LLDP reports this as a `BITS` value over `lldpRemSysCapEnabled` and CDP as a four-byte mask over
/// `cdpCacheCapabilities`; the two bit layouts have nothing in common. Normalizing them here is
/// what lets the UI render one column — the alternative is a per-protocol legend, which is the
/// "same fact in two shapes" the extensibility rules exist to prevent.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NeighborCapability {
    Router,
    Bridge,
    Switch,
    WlanAp,
    Phone,
    /// An end station: LLDP `stationOnly`, CDP `host`.
    Host,
    Repeater,
    /// LLDP `docsisCableDevice`.
    CableDevice,
    /// CDP `igmp` (IGMP conditional filtering).
    Igmp,
    /// LLDP bit 0 (`other`) — the agent claims a role outside the enumeration.
    Other,
}

impl NeighborCapability {
    /// Every capability, in the order the UI presents them.
    pub const ALL: [NeighborCapability; 10] = [
        NeighborCapability::Router,
        NeighborCapability::Bridge,
        NeighborCapability::Switch,
        NeighborCapability::WlanAp,
        NeighborCapability::Phone,
        NeighborCapability::Host,
        NeighborCapability::Repeater,
        NeighborCapability::CableDevice,
        NeighborCapability::Igmp,
        NeighborCapability::Other,
    ];

    /// The stable token used in the change key and as the i18n key suffix.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            NeighborCapability::Router => "router",
            NeighborCapability::Bridge => "bridge",
            NeighborCapability::Switch => "switch",
            NeighborCapability::WlanAp => "wlan_ap",
            NeighborCapability::Phone => "phone",
            NeighborCapability::Host => "host",
            NeighborCapability::Repeater => "repeater",
            NeighborCapability::CableDevice => "cable_device",
            NeighborCapability::Igmp => "igmp",
            NeighborCapability::Other => "other",
        }
    }

    /// Parse a stored token back into a capability.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        NeighborCapability::ALL
            .into_iter()
            .find(|c| c.as_str() == s)
    }
}

/// One observed adjacency.
///
/// The first three string fields are the **identity**; everything below them is payload. Payload
/// changes still append a history row (a peer that was reimaged is a real change on that port), but
/// they never split one link into two records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Neighbor {
    /// Which protocol reported this.
    pub proto: NeighborProto,

    /// The local port, as the device names it: LLDP renders `lldpLocPortId` by its subtype, CDP
    /// uses `cdpInterfaceName`. Falls back to `port <n>` / `ifindex <n>` when the naming table has
    /// no row, so the record still has an identity rather than being dropped.
    pub local_port: String,
    /// The peer's chassis id, rendered by subtype: LLDP `lldpRemChassisId`, CDP `cdpCacheDeviceId`.
    pub remote_chassis: String,
    /// The peer's port id, rendered by subtype: LLDP `lldpRemPortId`, CDP `cdpCacheDevicePort`.
    pub remote_port: String,

    /// The local `ifIndex`, when the protocol genuinely supplies one. CDP indexes its cache by
    /// `ifIndex` so this is exact; LLDP's `lldpLocPortNum` is an arbitrary local index that only
    /// *often* equals `ifIndex`, so LLDP records leave this `None` rather than guess.
    #[serde(default)]
    pub local_ifindex: Option<u32>,
    /// `lldpRemPortDesc` — the peer's own description of its port.
    #[serde(default)]
    pub remote_port_desc: Option<String>,
    /// `lldpRemSysName`. CDP has no separate system name (its device id serves both).
    #[serde(default)]
    pub remote_sys_name: Option<String>,
    /// `lldpRemSysDesc`.
    #[serde(default)]
    pub remote_sys_desc: Option<String>,
    /// The peer's management address, from `cdpCacheAddress` for CDP and from `lldpRemManAddrTable`
    /// for LLDP. `None` when the peer advertised none.
    //
    // ⚠️ The `///` above is published verbatim to API clients through the generated OpenAPI
    // document, so the reasoning lives here instead.
    //
    // This is the field that lets an adjacency row be matched back to an inventory node, and it is
    // why ADR-043 could not build a graph from L2 alone until it was collected: a bare LLDP row
    // names its peer by chassis id and port id, neither of which is an address. LLDP keeps it in
    // the *index* of `lldpRemManAddrTable` rather than in a column, which is why collecting it
    // needed the instance-preserving walker.
    #[serde(default)]
    pub remote_mgmt_addr: Option<String>,
    /// `cdpCachePlatform` — the peer's hardware/software platform string (CDP only).
    #[serde(default)]
    pub remote_platform: Option<String>,
    /// What the peer says it is, normalized across both protocols.
    #[serde(default)]
    pub capabilities: Vec<NeighborCapability>,
}

impl Neighbor {
    /// A record with only its identity filled in — the shape the poller starts from.
    #[must_use]
    pub fn new(
        proto: NeighborProto,
        local_port: impl Into<String>,
        remote_chassis: impl Into<String>,
        remote_port: impl Into<String>,
    ) -> Self {
        Self {
            proto,
            local_port: local_port.into(),
            remote_chassis: remote_chassis.into(),
            remote_port: remote_port.into(),
            local_ifindex: None,
            remote_port_desc: None,
            remote_sys_name: None,
            remote_sys_desc: None,
            remote_mgmt_addr: None,
            remote_platform: None,
            capabilities: Vec::new(),
        }
    }

    /// The identity tuple: what makes two observations "the same link".
    fn identity(&self) -> (&str, &str, &str, NeighborProto) {
        (
            self.local_port.as_str(),
            self.remote_chassis.as_str(),
            self.remote_port.as_str(),
            self.proto,
        )
    }

    /// Whether this record names a link at all. A row whose local port and both remote ids are
    /// empty carries no information and would collide with every other such row.
    fn has_identity(&self) -> bool {
        let named_peer = !self.remote_chassis.is_empty() || !self.remote_port.is_empty();
        !self.local_port.is_empty() && named_peer
    }

    /// Sanitize and clamp every device-supplied string, dropping payload fields that end up empty.
    ///
    /// Control characters are removed here, which is also what makes [`NeighborSet::content_key`]
    /// unambiguous without escaping: no field can contain the newline the encoding uses as its
    /// separator.
    fn sanitize(&mut self) {
        clamp(&mut self.local_port);
        clamp(&mut self.remote_chassis);
        clamp(&mut self.remote_port);
        for field in [
            &mut self.remote_port_desc,
            &mut self.remote_sys_name,
            &mut self.remote_sys_desc,
            &mut self.remote_mgmt_addr,
            &mut self.remote_platform,
        ] {
            if let Some(v) = field.as_mut() {
                clamp(v);
            }
            if field.as_deref().is_some_and(str::is_empty) {
                *field = None;
            }
        }
        self.capabilities.sort_unstable();
        self.capabilities.dedup();
    }
}

/// Strip control characters, trim, and cap at [`MAX_FIELD_CHARS`] characters (not bytes, so a
/// multi-byte string is never cut mid-character).
fn clamp(s: &mut String) {
    let cleaned: String = s
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_FIELD_CHARS)
        .collect();
    *s = cleaned.trim().to_owned();
}

/// Every neighbour a node reports on one observation, as a set.
///
/// The unit of storage is the whole set per node, not one row per adjacency, because a partial walk
/// must never read as "every neighbour disappeared". The poller reports the set it could observe,
/// core replaces it atomically, and a failed walk sends no set at all — so nothing is written. A
/// per-adjacency table would need an explicit "this is complete" flag to get the same guarantee.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct NeighborSet {
    /// The adjacencies, canonically ordered.
    #[serde(default)]
    pub neighbors: Vec<Neighbor>,
    /// Whether [`MAX_NEIGHBORS_PER_NODE`] was hit and rows were dropped. Reported rather than
    /// silently swallowed — a truncated view that looks complete is worse than no view.
    #[serde(default)]
    pub truncated: bool,
}

impl NeighborSet {
    /// Build a set from observed records, canonicalizing immediately.
    #[must_use]
    pub fn new(neighbors: Vec<Neighbor>) -> Self {
        let mut set = Self {
            neighbors,
            truncated: false,
        };
        set.canonicalize();
        set
    }

    /// How many adjacencies the set holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.neighbors.len()
    }

    /// Whether the node reported no adjacency at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.neighbors.is_empty()
    }

    /// Put the set in canonical form so two observations of an unchanged network compare equal.
    ///
    /// 1. Every string is sanitized and clamped ([`Neighbor::sanitize`]).
    /// 2. Records with no usable identity are dropped.
    /// 3. Records are sorted by identity — agents return table rows in whatever order the walk
    ///    happened to reach them, and `lldpRemIndex` renumbering re-orders them for free.
    /// 4. Exact duplicates (same identity **and** same payload) collapse; a duplicate identity with
    ///    differing payload keeps the first, so the result has one record per link.
    /// 5. The set is truncated to [`MAX_NEIGHBORS_PER_NODE`] *after* sorting, so which records
    ///    survive does not depend on walk order.
    pub fn canonicalize(&mut self) {
        for n in &mut self.neighbors {
            n.sanitize();
        }
        self.neighbors.retain(Neighbor::has_identity);
        self.neighbors
            .sort_by(|a, b| a.identity().cmp(&b.identity()));
        self.neighbors.dedup_by(|a, b| a.identity() == b.identity());
        if self.neighbors.len() > MAX_NEIGHBORS_PER_NODE {
            self.neighbors.truncate(MAX_NEIGHBORS_PER_NODE);
            self.truncated = true;
        }
    }

    /// The stable content key used for append-on-change comparison.
    ///
    /// **Excludes** `lldpRemTimeMark` (moves on every agent refresh), `lldpRemIndex` (reassigned at
    /// the agent's discretion), row order, and any TTL/age counter — none of them are ever carried
    /// into [`Neighbor`] in the first place, which is the real guarantee. **Includes** the identity
    /// and every payload field, so a peer that was renamed, reimaged or re-addressed registers as a
    /// change on that port.
    ///
    /// ⚠️ This encoding is effectively a wire format. Changing it re-keys every stored set and emits
    /// exactly one spurious change row per node on the first poll after the upgrade. It is
    /// versioned (`v1` first line); bump the version and say so in RELEASE_NOTES if it changes. It
    /// is built by hand rather than through `serde_json` precisely so serde's output cannot drift
    /// underneath it.
    ///
    /// Call [`Self::canonicalize`] first — this reads the set as-is and does not sort.
    #[must_use]
    pub fn content_key(&self) -> String {
        let mut out = String::from("v1\n");
        for n in &self.neighbors {
            // Identity first, then payload, one field per line. `sanitize` has already removed
            // every control character, so no value can forge a line boundary.
            out.push_str("n=");
            out.push_str(n.proto.as_str());
            out.push('\n');
            push_field(&mut out, "lp", Some(&n.local_port));
            push_field(&mut out, "rc", Some(&n.remote_chassis));
            push_field(&mut out, "rp", Some(&n.remote_port));
            push_field(
                &mut out,
                "li",
                n.local_ifindex.map(|i| i.to_string()).as_deref(),
            );
            push_field(&mut out, "pd", n.remote_port_desc.as_deref());
            push_field(&mut out, "sn", n.remote_sys_name.as_deref());
            push_field(&mut out, "sd", n.remote_sys_desc.as_deref());
            push_field(&mut out, "ma", n.remote_mgmt_addr.as_deref());
            push_field(&mut out, "pl", n.remote_platform.as_deref());
            let caps: Vec<&str> = n.capabilities.iter().map(|c| c.as_str()).collect();
            push_field(&mut out, "cp", Some(&caps.join(",")));
        }
        // Truncation is part of the content: a node that grew past the cap has genuinely changed
        // what we can see of it, and the operator should get a history row saying so.
        out.push_str(if self.truncated { "t=1\n" } else { "t=0\n" });
        out
    }
}

/// Append one `key=value` line, using `-` for an absent value so "unset" and "empty" cannot collide.
fn push_field(out: &mut String, key: &str, value: Option<&str>) {
    out.push_str(key);
    out.push('=');
    out.push_str(value.unwrap_or("-"));
    out.push('\n');
}

// ── Rendering device-supplied ids ────────────────────────────────────────────────────

/// How an LLDP id column's octets should be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdShape {
    Mac,
    NetworkAddress,
    Text,
}

/// `LldpChassisIdSubtype` → how to read the octets. **The numbering differs from the port subtype**
/// (`macAddress` is 4 here and 3 there), which is exactly the kind of off-by-one that would render
/// every MAC as mojibake, so the two mappings are separate functions with a test pinning them apart.
const fn chassis_id_shape(subtype: i64) -> IdShape {
    match subtype {
        4 => IdShape::Mac,            // macAddress
        5 => IdShape::NetworkAddress, // networkAddress
        _ => IdShape::Text,           // chassisComponent / interfaceAlias / portComponent / …
    }
}

/// `LldpPortIdSubtype` → how to read the octets.
const fn port_id_shape(subtype: i64) -> IdShape {
    match subtype {
        3 => IdShape::Mac,            // macAddress
        4 => IdShape::NetworkAddress, // networkAddress
        _ => IdShape::Text,           // interfaceAlias / portComponent / interfaceName / local / …
    }
}

/// Render `lldpRemChassisId` / `lldpLocChassisId` octets according to their subtype.
#[must_use]
pub fn render_chassis_id(subtype: i64, bytes: &[u8]) -> String {
    render_id(chassis_id_shape(subtype), bytes)
}

/// Render `lldpRemPortId` / `lldpLocPortId` octets according to their subtype.
#[must_use]
pub fn render_port_id(subtype: i64, bytes: &[u8]) -> String {
    render_id(port_id_shape(subtype), bytes)
}

fn render_id(shape: IdShape, bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let rendered = match shape {
        IdShape::Mac => render_mac(bytes),
        IdShape::NetworkAddress => render_network_address(bytes),
        IdShape::Text => None,
    };
    // Every fallback lands on text-or-hex rather than failing: an agent that mislabels its own
    // subtype should still produce a stable, readable id instead of dropping the adjacency.
    rendered.unwrap_or_else(|| render_text(bytes))
}

/// Six octets as lowercase colon-separated hex. `None` for any other length — that is not a MAC,
/// whatever the subtype claimed.
#[must_use]
pub fn render_mac(bytes: &[u8]) -> Option<String> {
    let mac: &[u8; 6] = bytes.try_into().ok()?;
    Some(
        mac.iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":"),
    )
}

/// An LLDP `networkAddress`: one IANA address-family octet followed by the address itself
/// (RFC 3232 — 1 = IPv4, 2 = IPv6). `None` for any other family or a length mismatch.
#[must_use]
pub fn render_network_address(bytes: &[u8]) -> Option<String> {
    match bytes {
        [1, rest @ ..] => render_bare_address(rest),
        [2, rest @ ..] => render_bare_address(rest),
        _ => None,
    }
}

/// A bare address by length: 4 octets ⇒ IPv4, 16 ⇒ IPv6. This is how CDP carries
/// `cdpCacheAddress` (whose `cdpCacheAddressType` adds nothing the length does not already say).
#[must_use]
pub fn render_bare_address(bytes: &[u8]) -> Option<String> {
    if let Ok(v4) = <[u8; 4]>::try_from(bytes) {
        return Some(Ipv4Addr::from(v4).to_string());
    }
    if let Ok(v6) = <[u8; 16]>::try_from(bytes) {
        return Some(Ipv6Addr::from(v6).to_string());
    }
    None
}

/// Device-supplied octets as display text.
///
/// **Never lossy.** Bytes that are not valid UTF-8 become hex rather than a string of replacement
/// characters: the identity has to be byte-stable across polls, and `from_utf8_lossy` maps
/// different inputs onto the same `U+FFFD` while making a MAC unrecognisable. Control characters
/// are stripped so the value cannot forge a line in the content key.
#[must_use]
pub fn render_text(bytes: &[u8]) -> String {
    let Ok(s) = std::str::from_utf8(bytes) else {
        return render_hex(bytes);
    };
    let cleaned: String = s.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        render_hex(bytes)
    } else {
        trimmed.to_owned()
    }
}

/// Octets as lowercase colon-separated hex — the last-resort rendering, and the reason an
/// undecodable id is still a *stable* id.
#[must_use]
pub fn render_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// `lldpRemSysCapEnabled` (a `BITS` value, bit 0 = most significant bit of the first octet).
#[must_use]
pub fn lldp_capabilities(bytes: &[u8]) -> Vec<NeighborCapability> {
    const BITS: [NeighborCapability; 8] = [
        NeighborCapability::Other,       // 0 other
        NeighborCapability::Repeater,    // 1 repeater
        NeighborCapability::Bridge,      // 2 bridge
        NeighborCapability::WlanAp,      // 3 wlanAccessPoint
        NeighborCapability::Router,      // 4 router
        NeighborCapability::Phone,       // 5 telephone
        NeighborCapability::CableDevice, // 6 docsisCableDevice
        NeighborCapability::Host,        // 7 stationOnly
    ];
    let mut out = Vec::new();
    for (i, cap) in BITS.iter().enumerate() {
        let byte = i / 8;
        let bit = 7 - (i % 8);
        if bytes.get(byte).is_some_and(|b| b & (1 << bit) != 0) {
            out.push(*cap);
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// `cdpCacheCapabilities` — a big-endian 32-bit mask (CISCO-CDP-MIB).
#[must_use]
pub fn cdp_capabilities(bytes: &[u8]) -> Vec<NeighborCapability> {
    const MASKS: [(u32, NeighborCapability); 7] = [
        (0x01, NeighborCapability::Router),
        (0x02, NeighborCapability::Bridge), // transparentBridge
        (0x04, NeighborCapability::Bridge), // sourceRouteBridge — same role, one token
        (0x08, NeighborCapability::Switch),
        (0x10, NeighborCapability::Host),
        (0x20, NeighborCapability::Igmp),
        (0x40, NeighborCapability::Repeater),
    ];
    // Shorter encodings occur in the wild; read whatever octets are present as the low bytes.
    let mut mask: u32 = 0;
    for b in bytes.iter().rev().take(4).enumerate() {
        mask |= u32::from(*b.1) << (8 * b.0);
    }
    let mut out: Vec<NeighborCapability> = MASKS
        .iter()
        .filter(|(m, _)| mask & m != 0)
        .map(|(_, c)| *c)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

// ── The fixed column list (a bus contract) ───────────────────────────────────────────

/// Which neighbour column an OID base carries. Typed rather than matched on the OID string so the
/// poller's assembly is a `match` the compiler checks, in the same spirit as
/// [`crate::InterfaceField`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NeighborColumn {
    /// `lldpLocPortIdSubtype`
    LldpLocPortIdSubtype,
    /// `lldpLocPortId`
    LldpLocPortId,
    /// `lldpLocPortDesc`
    LldpLocPortDesc,
    /// `lldpRemChassisIdSubtype`
    LldpRemChassisIdSubtype,
    /// `lldpRemChassisId`
    LldpRemChassisId,
    /// `lldpRemPortIdSubtype`
    LldpRemPortIdSubtype,
    /// `lldpRemPortId`
    LldpRemPortId,
    /// `lldpRemPortDesc`
    LldpRemPortDesc,
    /// `lldpRemSysName`
    LldpRemSysName,
    /// `lldpRemSysDesc`
    LldpRemSysDesc,
    /// `lldpRemSysCapEnabled`
    LldpRemSysCapEnabled,
    /// `lldpRemManAddrIfSubtype` — walked only so its row index yields the peer's management
    /// address; the value itself is discarded.
    LldpRemManAddrIfSubtype,
    /// `cdpInterfaceName` — the local port name, indexed by `ifIndex`.
    CdpInterfaceName,
    /// `cdpCacheAddressType`
    CdpCacheAddressType,
    /// `cdpCacheAddress`
    CdpCacheAddress,
    /// `cdpCacheDeviceId`
    CdpCacheDeviceId,
    /// `cdpCacheDevicePort`
    CdpCacheDevicePort,
    /// `cdpCachePlatform`
    CdpCachePlatform,
    /// `cdpCacheCapabilities`
    CdpCacheCapabilities,
}

/// The neighbour columns and their OID bases — LLDP-MIB and CISCO-CDP-MIB, both fixed standards.
///
/// Deliberately **not** a collection template: a [`crate::CollectionItem`] declares a TSDB series
/// (`metric_name` + `MetricKind`) and the numeric walker drops every non-numeric row, so adjacency
/// strings cannot travel that path at all. This mirrors
/// [`crate::builtin_interface_meta_columns`] — a fixed list of columns that walk alongside metrics
/// but land in PostgreSQL — and for the same reason: these OIDs are standardised, so there is
/// nothing here for an operator to configure.
///
/// **This list is the bus contract**; keep it stable for N/N-1 compatibility.
#[must_use]
pub fn builtin_neighbor_columns() -> Vec<(NeighborColumn, &'static str)> {
    vec![
        (
            NeighborColumn::LldpLocPortIdSubtype,
            OID_LLDP_LOC_PORT_ID_SUBTYPE,
        ),
        (NeighborColumn::LldpLocPortId, OID_LLDP_LOC_PORT_ID),
        (NeighborColumn::LldpLocPortDesc, OID_LLDP_LOC_PORT_DESC),
        (
            NeighborColumn::LldpRemChassisIdSubtype,
            OID_LLDP_REM_CHASSIS_ID_SUBTYPE,
        ),
        (NeighborColumn::LldpRemChassisId, OID_LLDP_REM_CHASSIS_ID),
        (
            NeighborColumn::LldpRemPortIdSubtype,
            OID_LLDP_REM_PORT_ID_SUBTYPE,
        ),
        (NeighborColumn::LldpRemPortId, OID_LLDP_REM_PORT_ID),
        (NeighborColumn::LldpRemPortDesc, OID_LLDP_REM_PORT_DESC),
        (NeighborColumn::LldpRemSysName, OID_LLDP_REM_SYS_NAME),
        (NeighborColumn::LldpRemSysDesc, OID_LLDP_REM_SYS_DESC),
        (
            NeighborColumn::LldpRemSysCapEnabled,
            OID_LLDP_REM_SYS_CAP_ENABLED,
        ),
        (
            NeighborColumn::LldpRemManAddrIfSubtype,
            OID_LLDP_REM_MAN_ADDR_IF_SUBTYPE,
        ),
        (NeighborColumn::CdpInterfaceName, OID_CDP_INTERFACE_NAME),
        (
            NeighborColumn::CdpCacheAddressType,
            OID_CDP_CACHE_ADDRESS_TYPE,
        ),
        (NeighborColumn::CdpCacheAddress, OID_CDP_CACHE_ADDRESS),
        (NeighborColumn::CdpCacheDeviceId, OID_CDP_CACHE_DEVICE_ID),
        (
            NeighborColumn::CdpCacheDevicePort,
            OID_CDP_CACHE_DEVICE_PORT,
        ),
        (NeighborColumn::CdpCachePlatform, OID_CDP_CACHE_PLATFORM),
        (
            NeighborColumn::CdpCacheCapabilities,
            OID_CDP_CACHE_CAPABILITIES,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lldp(local: &str, chassis: &str, port: &str) -> Neighbor {
        Neighbor::new(NeighborProto::Lldp, local, chassis, port)
    }

    fn sample() -> NeighborSet {
        let mut a = lldp("Gi0/1", "aa:bb:cc:dd:ee:01", "Gi1/0/24");
        a.remote_sys_name = Some("core-sw-01".into());
        a.capabilities = vec![NeighborCapability::Bridge, NeighborCapability::Router];
        let mut b = lldp("Gi0/2", "aa:bb:cc:dd:ee:02", "Gi1/0/25");
        b.remote_sys_name = Some("core-sw-02".into());
        NeighborSet::new(vec![a, b])
    }

    fn key_of(mut set: NeighborSet) -> String {
        set.canonicalize();
        set.content_key()
    }

    // ── The content key: what must NOT count as a change ─────────────────────────────

    /// The single most important property in this module. `lldpRemTimeMark` moves on every agent
    /// refresh and `lldpRemIndex` is renumbered at will, and both arrive as part of the table index
    /// — so if either reached the model, the "change" log would gain a row on every single poll.
    /// The guarantee is structural (neither is a field of [`Neighbor`]); this pins the observable
    /// consequence: two walks of an unchanged network, whose rows the agent returned in a different
    /// order, produce the same key.
    #[test]
    fn content_key_ignores_row_order() {
        let forward = sample();
        let mut reversed = sample();
        reversed.neighbors.reverse();
        assert_eq!(key_of(forward), key_of(reversed));
    }

    #[test]
    fn content_key_ignores_capability_order_and_duplicates() {
        let mut shuffled = sample();
        shuffled.neighbors[0].capabilities = vec![
            NeighborCapability::Router,
            NeighborCapability::Bridge,
            NeighborCapability::Router,
        ];
        assert_eq!(key_of(sample()), key_of(shuffled));
    }

    #[test]
    fn content_key_ignores_surrounding_whitespace_in_device_text() {
        // Agents pad DisplayStrings; padding must not read as a rewiring.
        let mut padded = sample();
        padded.neighbors[0].remote_sys_name = Some("  core-sw-01  ".into());
        assert_eq!(key_of(sample()), key_of(padded));
    }

    #[test]
    fn content_key_is_versioned() {
        assert!(key_of(sample()).starts_with("v1\n"));
    }

    // ── The content key: what MUST count as a change ─────────────────────────────────

    #[test]
    fn content_key_changes_when_the_peer_on_a_port_changes() {
        let mut moved = sample();
        moved.neighbors[0].remote_chassis = "aa:bb:cc:dd:ee:09".into();
        assert_ne!(key_of(sample()), key_of(moved));
    }

    #[test]
    fn content_key_changes_when_a_link_is_added_or_removed() {
        let mut added = sample();
        added
            .neighbors
            .push(lldp("Gi0/3", "aa:bb:cc:dd:ee:03", "Gi1/0/26"));
        assert_ne!(key_of(sample()), key_of(added));

        let mut removed = sample();
        removed.neighbors.truncate(1);
        assert_ne!(key_of(sample()), key_of(removed));
    }

    /// Payload is not identity, but it is content: a peer that was renamed or reimaged is a real
    /// change on that port, and the operator wants the timestamp.
    #[test]
    fn content_key_changes_when_payload_changes() {
        let mut renamed = sample();
        renamed.neighbors[0].remote_sys_name = Some("core-sw-01-replaced".into());
        assert_ne!(key_of(sample()), key_of(renamed));

        let mut recapped = sample();
        recapped.neighbors[0].capabilities = vec![NeighborCapability::Router];
        assert_ne!(key_of(sample()), key_of(recapped));
    }

    /// An unset payload field and an empty one must not collide, or "the agent stopped reporting
    /// sysName" would look identical to "sysName is blank".
    #[test]
    fn an_absent_field_is_distinguishable_from_a_present_one() {
        let mut cleared = sample();
        cleared.neighbors[0].remote_sys_name = None;
        assert_ne!(key_of(sample()), key_of(cleared));
    }

    #[test]
    fn content_key_records_truncation() {
        let mut whole = sample();
        whole.truncated = false;
        let mut cut = sample();
        cut.truncated = true;
        assert_ne!(key_of(whole), key_of(cut));
    }

    /// Device text cannot forge a line in the encoding, because `sanitize` strips control
    /// characters before the key is built.
    #[test]
    fn device_text_cannot_forge_a_key_line() {
        let mut evil = sample();
        evil.neighbors[0].remote_sys_name = Some("core\nrc=spoofed".into());
        let key = key_of(evil);
        // Checked line-wise: the injected text survives *inside* a value, which is fine — what
        // must not happen is a line of its own that a reader would parse as a chassis id.
        assert!(
            !key.lines().any(|l| l == "rc=spoofed"),
            "device text forged a key line:\n{key}"
        );
        assert!(key.lines().any(|l| l == "sn=corerc=spoofed"), "{key}");
    }

    // ── Canonicalization ─────────────────────────────────────────────────────────────

    #[test]
    fn canonicalize_is_idempotent() {
        let mut once = sample();
        once.canonicalize();
        let mut twice = once.clone();
        twice.canonicalize();
        assert_eq!(once, twice);
    }

    #[test]
    fn canonicalize_drops_rows_with_no_identity() {
        let set = NeighborSet::new(vec![
            lldp("", "aa:bb:cc:dd:ee:01", "Gi1/0/24"), // no local port
            lldp("Gi0/2", "", ""),                     // no remote id at all
            lldp("Gi0/3", "aa:bb:cc:dd:ee:03", ""),    // chassis only — usable
        ]);
        assert_eq!(set.len(), 1);
        assert_eq!(set.neighbors[0].local_port, "Gi0/3");
    }

    #[test]
    fn canonicalize_collapses_repeated_rows() {
        let set = NeighborSet::new(vec![
            lldp("Gi0/1", "aa:bb:cc:dd:ee:01", "Gi1/0/24"),
            lldp("Gi0/1", "aa:bb:cc:dd:ee:01", "Gi1/0/24"),
        ]);
        assert_eq!(set.len(), 1);
    }

    /// A switch running both protocols reports the same physical link twice under different
    /// identities. Both are kept — collapsing them would mean guessing that a chassis MAC and a
    /// hostname name the same box.
    #[test]
    fn the_same_link_seen_by_both_protocols_is_kept_twice() {
        let set = NeighborSet::new(vec![
            lldp("Gi0/1", "aa:bb:cc:dd:ee:01", "Gi1/0/24"),
            Neighbor::new(NeighborProto::Cdp, "Gi0/1", "core-sw-01", "Gi1/0/24"),
        ]);
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn canonicalize_caps_the_set_and_reports_it() {
        let many: Vec<Neighbor> = (0..MAX_NEIGHBORS_PER_NODE + 40)
            .map(|i| {
                lldp(
                    &format!("Gi0/{i:04}"),
                    &format!("aa:bb:cc:dd:ee:{i:02x}"),
                    "e1",
                )
            })
            .rev() // reversed input must still retain the same set
            .collect();
        let set = NeighborSet::new(many.clone());
        assert_eq!(set.len(), MAX_NEIGHBORS_PER_NODE);
        assert!(set.truncated, "overflow must be reported, never silent");

        let mut forward = many;
        forward.reverse();
        assert_eq!(NeighborSet::new(forward).neighbors, set.neighbors);
    }

    #[test]
    fn canonicalize_clamps_long_device_text() {
        let mut n = lldp("Gi0/1", "aa:bb:cc:dd:ee:01", "e1");
        n.remote_sys_desc = Some("x".repeat(4000));
        let set = NeighborSet::new(vec![n]);
        assert_eq!(
            set.neighbors[0].remote_sys_desc.as_deref().map(str::len),
            Some(MAX_FIELD_CHARS)
        );
    }

    #[test]
    fn clamping_never_splits_a_multibyte_character() {
        let mut n = lldp("Gi0/1", "aa:bb:cc:dd:ee:01", "e1");
        n.remote_sys_desc = Some("あ".repeat(400));
        let set = NeighborSet::new(vec![n]);
        let desc = set.neighbors[0].remote_sys_desc.clone().unwrap();
        assert_eq!(desc.chars().count(), MAX_FIELD_CHARS);
        assert!(desc.chars().all(|c| c == 'あ'));
    }

    // ── Rendering ids ────────────────────────────────────────────────────────────────

    /// The bug this prevents: `lldpRemChassisId` with subtype 4 is six raw bytes. Reading it as
    /// UTF-8 yields replacement characters — unreadable, and (worse) different byte sequences map
    /// onto the same rendering, so two different peers could share one identity.
    #[test]
    fn a_mac_chassis_id_renders_as_a_mac_not_as_mojibake() {
        let bytes = [0x00u8, 0x1b, 0x54, 0xff, 0x00, 0x9a];
        let rendered = render_chassis_id(4, &bytes);
        assert_eq!(rendered, "00:1b:54:ff:00:9a");
        assert!(!rendered.contains('\u{fffd}'));
        assert_ne!(rendered, String::from_utf8_lossy(&bytes));
    }

    /// The subtype numbering is *not* shared between the two columns. Feeding a port subtype to the
    /// chassis mapper (or the reverse) is the mistake this pins.
    #[test]
    fn chassis_and_port_subtype_numbering_differ() {
        let mac = [0x00u8, 0x1b, 0x54, 0x11, 0x22, 0x33];
        // macAddress: 4 for a chassis id, 3 for a port id.
        assert_eq!(render_chassis_id(4, &mac), "00:1b:54:11:22:33");
        assert_eq!(render_port_id(3, &mac), "00:1b:54:11:22:33");
        // At the *other* protocol's number the same bytes are not read as a MAC.
        assert_ne!(render_chassis_id(3, &mac), "00:1b:54:11:22:33");
        assert_ne!(render_port_id(4, &mac), "00:1b:54:11:22:33");
    }

    #[test]
    fn a_network_address_id_renders_by_family() {
        // IANA family 1 = IPv4, 2 = IPv6.
        assert_eq!(render_chassis_id(5, &[1, 10, 1, 2, 3]), "10.1.2.3");
        let v6 = [
            2u8, 0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
        ];
        assert_eq!(render_chassis_id(5, &v6), "2001:db8::1");
        // An unknown family falls back rather than dropping the id.
        assert_eq!(render_network_address(&[9, 1, 2, 3]), None);
    }

    #[test]
    fn a_text_id_renders_as_text_and_undecodable_bytes_render_as_hex() {
        assert_eq!(
            render_port_id(5, b"GigabitEthernet1/0/24"),
            "GigabitEthernet1/0/24"
        );
        // Invalid UTF-8 must not become replacement characters — hex is stable across polls.
        assert_eq!(render_port_id(5, &[0xff, 0xfe]), "ff:fe");
    }

    #[test]
    fn a_mislabelled_length_falls_back_instead_of_dropping_the_id() {
        // Subtype says macAddress but the value is eight bytes: keep something stable.
        assert_eq!(render_chassis_id(4, b"switch01"), "switch01");
        assert!(!render_chassis_id(4, &[1, 2, 3]).is_empty());
    }

    #[test]
    fn an_empty_id_renders_empty_so_the_row_can_be_dropped() {
        assert_eq!(render_chassis_id(4, &[]), "");
        assert_eq!(render_port_id(5, &[]), "");
    }

    #[test]
    fn cdp_addresses_render_by_length() {
        assert_eq!(
            render_bare_address(&[192, 168, 1, 1]).as_deref(),
            Some("192.168.1.1")
        );
        assert_eq!(render_bare_address(&[0u8; 16]).as_deref(), Some("::"));
        assert_eq!(render_bare_address(&[1, 2, 3]), None);
    }

    // ── Capabilities ─────────────────────────────────────────────────────────────────

    #[test]
    fn lldp_capability_bits_map_to_the_shared_vocabulary() {
        // Bit 2 (bridge) + bit 4 (router) = 0b0010_1000 = 0x28.
        assert_eq!(
            lldp_capabilities(&[0x28]),
            vec![NeighborCapability::Router, NeighborCapability::Bridge]
        );
        // Bit 5 (telephone) = 0b0000_0100 = 0x04.
        assert_eq!(lldp_capabilities(&[0x04]), vec![NeighborCapability::Phone]);
        assert!(lldp_capabilities(&[]).is_empty());
        assert!(lldp_capabilities(&[0x00, 0x00]).is_empty());
    }

    #[test]
    fn cdp_capability_bits_map_to_the_shared_vocabulary() {
        // router | switch
        assert_eq!(
            cdp_capabilities(&[0, 0, 0, 0x09]),
            vec![NeighborCapability::Router, NeighborCapability::Switch]
        );
        // Both bridge bits collapse onto one token rather than showing "bridge" twice.
        assert_eq!(
            cdp_capabilities(&[0, 0, 0, 0x06]),
            vec![NeighborCapability::Bridge]
        );
        // A short encoding is read as the low-order octets.
        assert_eq!(cdp_capabilities(&[0x10]), vec![NeighborCapability::Host]);
        assert!(cdp_capabilities(&[]).is_empty());
    }

    /// The normalization the ADR asks for: an LLDP end-station and a CDP host are the same fact,
    /// so they must render as the same token or the UI needs a per-protocol legend.
    #[test]
    fn both_protocols_agree_on_the_shared_tokens() {
        assert_eq!(lldp_capabilities(&[0x01]), vec![NeighborCapability::Host]); // stationOnly
        assert_eq!(cdp_capabilities(&[0x10]), vec![NeighborCapability::Host]); // host
        assert_eq!(lldp_capabilities(&[0x08]), vec![NeighborCapability::Router]);
        assert_eq!(cdp_capabilities(&[0x01]), vec![NeighborCapability::Router]);
    }

    // ── Tokens and serde ─────────────────────────────────────────────────────────────

    #[test]
    fn every_variant_round_trips_through_its_token_and_through_serde() {
        for p in NeighborProto::ALL {
            assert_eq!(NeighborProto::from_token(p.as_str()), Some(p));
            let json = serde_json::to_string(&p).unwrap();
            assert_eq!(json, format!("\"{}\"", p.as_str()));
        }
        for c in NeighborCapability::ALL {
            assert_eq!(NeighborCapability::from_token(c.as_str()), Some(c));
            let json = serde_json::to_string(&c).unwrap();
            assert_eq!(json, format!("\"{}\"", c.as_str()));
        }
        assert_eq!(NeighborProto::from_token("foo"), None);
        assert_eq!(NeighborCapability::from_token("foo"), None);
    }

    #[test]
    fn capability_tokens_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for c in NeighborCapability::ALL {
            assert!(seen.insert(c.as_str()), "duplicate token {}", c.as_str());
        }
        assert_eq!(seen.len(), NeighborCapability::ALL.len());
    }

    #[test]
    fn a_neighbor_tolerates_missing_optional_fields() {
        // An N-1 producer sends only the identity; every payload field defaults.
        let n: Neighbor = serde_json::from_str(
            r#"{"proto":"lldp","local_port":"Gi0/1","remote_chassis":"aa","remote_port":"e1"}"#,
        )
        .unwrap();
        assert_eq!(n, Neighbor::new(NeighborProto::Lldp, "Gi0/1", "aa", "e1"));
        let empty: NeighborSet = serde_json::from_str("{}").unwrap();
        assert!(empty.is_empty() && !empty.truncated);
    }

    // ── The column list ──────────────────────────────────────────────────────────────

    #[test]
    fn every_neighbor_column_has_a_distinct_oid_base() {
        let cols = builtin_neighbor_columns();
        let mut oids = std::collections::BTreeSet::new();
        let mut fields = std::collections::BTreeSet::new();
        for (field, oid) in &cols {
            assert!(oids.insert(*oid), "duplicate OID base {oid}");
            assert!(
                fields.insert(format!("{field:?}")),
                "duplicate column {field:?}"
            );
            assert!(
                oid.split('.').all(|p| p.parse::<u32>().is_ok()),
                "{oid} is not a dotted numeric OID"
            );
        }
        assert_eq!(
            cols.len(),
            19,
            "the column list is a bus contract — adding one is an N/N-1 event, not a tidy-up"
        );
    }

    /// The `lldpRemTable` and `cdpCacheTable` column bases must not be prefixes of one another, or
    /// the poller's per-column bucketing would attribute a row to the wrong column.
    #[test]
    fn no_column_base_is_a_prefix_of_another() {
        let cols = builtin_neighbor_columns();
        for (_, a) in &cols {
            for (_, b) in &cols {
                if a == b {
                    continue;
                }
                assert!(
                    !b.starts_with(&format!("{a}.")),
                    "{a} is an OID prefix of {b}"
                );
            }
        }
    }
}
