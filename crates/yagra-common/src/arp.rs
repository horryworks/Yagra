// SPDX-License-Identifier: AGPL-3.0-only
//! ARP / IPv6-neighbour discovery: which IP addresses a router has actually spoken to, and on which
//! port (ADR-043 Increment 3).
//!
//! This answers a different question from every other walk in the system. `ipAddrTable` says what a
//! device *is*; `lldpRemTable` says what it is *cabled to*. `ipNetToPhysicalTable` says what has
//! **replied on the wire** — which is the only source that can name a host nobody has put in the
//! inventory yet.
//!
//! Three rules shape the whole module, and each is a decision rather than an implementation detail:
//!
//! 1. **The walk is bounded inside the paging loop, not afterwards.** A campus router's ARP table
//!    runs to tens of thousands of rows, and a cap applied after collection has already paid the
//!    memory it was meant to save. [`MAX_ARP_WALK_ROWS`] is passed to
//!    `Transport::snmp_walk_instances`, which stops mid-page.
//!
//! 2. **The poller aggregates before it publishes.** What core needs is "how many endpoints per
//!    port" plus a bounded sample of them, not four thousand rows per node per cycle on the bus.
//!    [`ArpSummary::per_interface`] stays accurate under truncation because it is counted *before*
//!    [`MAX_ARP_ENTRIES_PER_NODE`] trims the sample.
//!
//! 3. **A discovered endpoint is not a map vertex.** Nothing here feeds `derive_links`. An
//!    unmonitored host has no state, and drawing four thousand stateless boxes is precisely what
//!    `MAP_CAP` exists to prevent. These become inventory — and therefore graph vertices — only by
//!    an operator importing them, at which point the ordinary derivation picks them up with no
//!    special case.
//!
//! There is deliberately **no subnet field** on the per-interface rollup. The plan sketched one, but
//! the poller cannot honestly produce it: an ARP row carries no prefix length, and the interface's
//! own prefix comes from the L3 walk, which is a different job that may not have run. Inferring a
//! `/24` because the addresses look contiguous is exactly the guessing ADR-043 決定 2 forbids.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv6Addr};

/// Stable TSDB metric: how many ARP/ND entries the node currently holds. Node-level and bounded by
/// the walk budget, so it is a safe series. The addresses themselves never become one — an IP in a
/// label is the cardinality explosion CLAUDE.md §7.1 warns about.
pub const METRIC_SNMP_ARP_ENTRY_COUNT: &str = "snmp_arp_entry_count";

/// Row budget for the ARP/ND walk itself, across both columns.
///
/// Sized for a busy campus distribution switch (a few thousand hosts) rather than for a core router
/// holding a full Internet-scale neighbour table: past this, the answer stops being "who is on my
/// segments" and starts being a memory bill. The overflow is reported through
/// [`ArpSummary::truncated`], never swallowed.
pub const MAX_ARP_WALK_ROWS: usize = 4096;

/// Cap on the endpoint sample published per node.
///
/// Distinct from [`MAX_ARP_WALK_ROWS`], which bounds the *walk*: this bounds what crosses the bus
/// and what is stored. Applied **after** sorting so truncation is deterministic, and applied after
/// [`ArpSummary::per_interface`] is counted so the rollup keeps telling the truth about a port with
/// six hundred hosts behind it.
pub const MAX_ARP_ENTRIES_PER_NODE: usize = 512;

/// `ipNetToPhysicalPhysAddress` (RFC 4293) — v4 **and** v6, indexed
/// `(ifIndex, addrType, addrLen, addr…)`.
const OID_IP_NET_TO_PHYSICAL_PHYS_ADDRESS: &str = "1.3.6.1.2.1.4.35.1.4";
/// `ipNetToMediaPhysAddress` (RFC 1213) — IPv4 only, indexed `(ifIndex, a, b, c, d)`.
const OID_IP_NET_TO_MEDIA_PHYS_ADDRESS: &str = "1.3.6.1.2.1.4.22.1.2";

/// Which neighbour-cache column an OID base carries.
///
/// Typed rather than matched on the OID string, so the poller's assembly is a `match` the compiler
/// checks — the same contract [`crate::L3Column`] and [`crate::NeighborColumn`] offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArpColumn {
    /// `ipNetToPhysicalPhysAddress` — the modern table; both families.
    IpNetToPhysicalPhysAddress,
    /// `ipNetToMediaPhysAddress` — the legacy IPv4-only table, for agents that answer nothing else.
    IpNetToMediaPhysAddress,
}

impl ArpColumn {
    /// Every column, for iteration and coverage tests.
    pub const ALL: [ArpColumn; 2] = [
        ArpColumn::IpNetToPhysicalPhysAddress,
        ArpColumn::IpNetToMediaPhysAddress,
    ];

    /// The stable token used in stored documents and tests.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ArpColumn::IpNetToPhysicalPhysAddress => "ip_net_to_physical_phys_address",
            ArpColumn::IpNetToMediaPhysAddress => "ip_net_to_media_phys_address",
        }
    }

    /// Parse a stored token back into a column.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "ip_net_to_physical_phys_address" => Some(ArpColumn::IpNetToPhysicalPhysAddress),
            "ip_net_to_media_phys_address" => Some(ArpColumn::IpNetToMediaPhysAddress),
            _ => None,
        }
    }
}

/// The ARP/ND columns and their OID bases — RFC 4293 and RFC 1213, both fixed standards.
///
/// Both are walked for the same reason both address tables are: `.4.22` is IPv4-only and is all some
/// agents implement, while `.4.35` is the only one that can see an IPv6 neighbour. Taking just the
/// cheap one would break the project's "never assume v4" rule; taking just the modern one would lose
/// devices that answer only the old table.
///
/// `ipNetToPhysicalType` is deliberately **not** walked. It would double the row count against the
/// same budget to tell us whether a mapping is `static` or `dynamic`, and neither answer changes
/// whether the address is a host worth knowing about — while halving how far the walk gets on a
/// large table, which does.
///
/// **This list is the bus contract**; keep it stable for N/N-1 compatibility.
#[must_use]
pub fn builtin_arp_columns() -> Vec<(ArpColumn, &'static str)> {
    vec![
        (
            ArpColumn::IpNetToPhysicalPhysAddress,
            OID_IP_NET_TO_PHYSICAL_PHYS_ADDRESS,
        ),
        (
            ArpColumn::IpNetToMediaPhysAddress,
            OID_IP_NET_TO_MEDIA_PHYS_ADDRESS,
        ),
    ]
}

/// One address the device has resolved on one of its ports.
///
/// The identity is the **IP**. A device can hold the same address on two interfaces (VRRP standby,
/// a VRF leak), and `l3_discovered` keys on the address, so collapsing here keeps the two ends of
/// the pipeline agreeing about what one endpoint is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArpEntry {
    /// The `ifIndex` the address was resolved on. Joins to the interface inventory, which is what
    /// turns the count into "42 endpoints behind Gi0/3".
    pub ifindex: u32,
    /// The endpoint's address. Never narrowed to v4.
    pub ip: IpAddr,
    /// The hardware address, lowercase colon-separated hex. `None` when the agent returned
    /// something that is not six octets — an incomplete ARP entry, or a medium that is not Ethernet.
    #[serde(default)]
    pub mac: Option<String>,
}

impl ArpEntry {
    /// An entry with the two things every row must have.
    #[must_use]
    pub fn new(ifindex: u32, ip: IpAddr) -> Self {
        Self {
            ifindex,
            ip,
            mac: None,
        }
    }

    /// Whether this address could name a host an operator might want to monitor.
    ///
    /// The exclusions are not tidiness. An IPv6 neighbour cache is overwhelmingly `fe80::` — a
    /// dual-stack switch holds one link-local per neighbour per port — so without this the
    /// [`MAX_ARP_ENTRIES_PER_NODE`] sample would be entirely link-local and the feature would
    /// discover nothing. Loopback and multicast are excluded for the same reason they are excluded
    /// from subnet membership: they identify no host reachable from anywhere else.
    #[must_use]
    pub fn is_discoverable(&self) -> bool {
        match self.ip {
            IpAddr::V4(v4) => {
                !v4.is_loopback()
                    && !v4.is_link_local()
                    && !v4.is_multicast()
                    && !v4.is_unspecified()
                    && !v4.is_broadcast()
            }
            IpAddr::V6(v6) => {
                !v6.is_loopback()
                    && !v6.is_multicast()
                    && !v6.is_unspecified()
                    && !is_v6_link_local(v6)
            }
        }
    }
}

/// `fe80::/10`. `Ipv6Addr::is_unicast_link_local` is still unstable, so this is spelled out — the
/// same reason [`crate::l3`] spells it out.
fn is_v6_link_local(addr: Ipv6Addr) -> bool {
    (addr.segments()[0] & 0xffc0) == 0xfe80
}

/// How many endpoints one port has resolved.
///
/// Kept even when the entry sample is truncated: an operator asking "is anything unmonitored behind
/// this port" is answered by the count, and a count that silently became "512 across the whole box"
/// would answer it wrongly while looking precise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArpInterfaceCount {
    /// The port's `ifIndex`.
    pub ifindex: u32,
    /// How many discoverable endpoints were resolved on it.
    pub count: u32,
}

/// What one ARP/ND walk saw, already aggregated by the poller.
///
/// The unit of storage is the whole summary per node, for the same reason [`crate::NeighborSet`] and
/// [`crate::L3Snapshot`] are whole-set: a partial walk must never read as "every endpoint left the
/// network". A failed walk sends no summary at all (`PollResult.arp = None`) so nothing is written;
/// an **empty** summary is a real observation and does replace the stored one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArpSummary {
    /// A bounded, canonically ordered sample of the endpoints seen.
    #[serde(default)]
    pub entries: Vec<ArpEntry>,
    /// Per-port totals, counted before the sample was trimmed.
    #[serde(default)]
    pub per_interface: Vec<ArpInterfaceCount>,
    /// How many discoverable endpoints the walk saw in total, before [`MAX_ARP_ENTRIES_PER_NODE`].
    ///
    /// Itself bounded by [`MAX_ARP_WALK_ROWS`] — if the *walk* was cut short this under-counts, and
    /// [`Self::truncated`] is the flag that says so. There is no honest way to report a number for
    /// rows that were never read.
    #[serde(default)]
    pub observed: u32,
    /// Whether either cap dropped something.
    #[serde(default)]
    pub truncated: bool,
}

impl ArpSummary {
    /// Build a summary from observed entries: filter, count per port, then canonicalize and trim.
    ///
    /// The order matters and is the whole reason this is a constructor rather than a struct literal.
    /// Counting after the trim would cap every port at the sample size; filtering after the count
    /// would report link-local noise as endpoints.
    #[must_use]
    pub fn new(entries: Vec<ArpEntry>, walk_truncated: bool) -> Self {
        let mut entries: Vec<ArpEntry> = entries
            .into_iter()
            .filter(ArpEntry::is_discoverable)
            .collect();
        // Sort before dedup so the surviving record for an address is the lowest ifIndex rather than
        // whichever page the agent answered first.
        entries.sort_by_key(|a| (a.ip, a.ifindex));
        entries.dedup_by(|a, b| a.ip == b.ip);

        // `BTreeMap` so the rollup is ordered by ifIndex without a second sort, and so a failing
        // test is readable.
        let mut counts: BTreeMap<u32, u32> = BTreeMap::new();
        for e in &entries {
            *counts.entry(e.ifindex).or_default() += 1;
        }
        let observed = u32::try_from(entries.len()).unwrap_or(u32::MAX);
        let sample_truncated = entries.len() > MAX_ARP_ENTRIES_PER_NODE;
        entries.truncate(MAX_ARP_ENTRIES_PER_NODE);

        Self {
            entries,
            per_interface: counts
                .into_iter()
                .map(|(ifindex, count)| ArpInterfaceCount { ifindex, count })
                .collect(),
            observed,
            truncated: walk_truncated || sample_truncated,
        }
    }

    /// How many entries the published sample holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the walk found no discoverable endpoint at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The stable content key used for append-on-change comparison.
    ///
    /// ⚠️ This encoding is effectively a wire format; it is versioned (`v1` first line) for the same
    /// reason [`crate::L3Snapshot::content_key`] is, and built by hand so serde's output cannot
    /// drift underneath it.
    ///
    /// **`observed` is deliberately excluded, and the per-port counts are not.** An ARP cache is
    /// volatile — one host going quiet for five minutes changes the total — so keying on it would
    /// write a history row almost every cycle and drown the transitions worth reading. The per-port
    /// counts are included because a port going from 0 to 40 endpoints is exactly such a transition.
    #[must_use]
    pub fn content_key(&self) -> String {
        let mut out = String::from("v1\n");
        for e in &self.entries {
            // Every field is a number, an address, or hex we rendered ourselves — no device-supplied
            // free text — so no value can forge a line boundary.
            out.push_str("a=");
            out.push_str(&e.ip.to_string());
            out.push('\n');
            out.push_str("i=");
            out.push_str(&e.ifindex.to_string());
            out.push('\n');
            out.push_str("m=");
            out.push_str(e.mac.as_deref().unwrap_or("-"));
            out.push('\n');
        }
        for c in &self.per_interface {
            out.push_str("p=");
            out.push_str(&c.ifindex.to_string());
            out.push(':');
            out.push_str(&c.count.to_string());
            out.push('\n');
        }
        out.push_str(if self.truncated { "x=1\n" } else { "x=0\n" });
        out
    }
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
    fn link_local_and_loopback_are_never_endpoints() {
        // The load-bearing one is `fe80::`. A dual-stack switch holds one link-local neighbour per
        // peer per port, so without this filter the bounded sample would be entirely link-local and
        // the feature would discover nothing at all.
        for ip in [
            v4("127.0.0.1"),
            v4("169.254.1.1"),
            v4("224.0.0.1"),
            v4("255.255.255.255"),
            v4("0.0.0.0"),
            v6("::1"),
            v6("fe80::1"),
            v6("ff02::1"),
        ] {
            assert!(
                !ArpEntry::new(1, ip).is_discoverable(),
                "{ip} must not be reported as an endpoint"
            );
        }
        for ip in [v4("10.0.0.5"), v4("192.168.1.20"), v6("2001:db8::5")] {
            assert!(
                ArpEntry::new(1, ip).is_discoverable(),
                "{ip} is a real host"
            );
        }
    }

    #[test]
    fn per_interface_counts_survive_the_sample_cap() {
        // The whole point of counting before trimming: a port with 600 hosts behind it must still
        // report 600 after the 512-entry sample has been cut, or the badge an operator reads would
        // quietly become "the sample size" instead of "the answer".
        let entries: Vec<ArpEntry> = (0..MAX_ARP_ENTRIES_PER_NODE + 88)
            .map(|i| {
                #[allow(clippy::cast_possible_truncation)]
                let ip = IpAddr::V4(std::net::Ipv4Addr::from((10u32 << 24) + 1 + i as u32));
                ArpEntry::new(7, ip)
            })
            .collect();
        let summary = ArpSummary::new(entries, false);
        assert_eq!(summary.len(), MAX_ARP_ENTRIES_PER_NODE);
        assert!(summary.truncated);
        assert_eq!(summary.observed, (MAX_ARP_ENTRIES_PER_NODE + 88) as u32);
        assert_eq!(summary.per_interface.len(), 1);
        assert_eq!(
            summary.per_interface[0].count,
            (MAX_ARP_ENTRIES_PER_NODE + 88) as u32
        );
    }

    #[test]
    fn a_walk_that_was_cut_short_declares_itself_even_with_a_small_sample() {
        // The transport stopped mid-page: the sample fits, but the answer is still partial and must
        // not read as "this router has three neighbours".
        let summary = ArpSummary::new(
            vec![
                ArpEntry::new(1, v4("10.0.0.1")),
                ArpEntry::new(1, v4("10.0.0.2")),
                ArpEntry::new(1, v4("10.0.0.3")),
            ],
            true,
        );
        assert_eq!(summary.len(), 3);
        assert!(summary.truncated);
    }

    #[test]
    fn one_address_on_two_ports_collapses_to_the_lower_ifindex() {
        // `l3_discovered` keys on the address, so two records for one IP would be two rows fighting
        // over one unique key. Deduping here keeps both ends of the pipeline agreeing.
        let summary = ArpSummary::new(
            vec![
                ArpEntry::new(9, v4("10.0.0.1")),
                ArpEntry::new(3, v4("10.0.0.1")),
            ],
            false,
        );
        assert_eq!(summary.len(), 1);
        assert_eq!(summary.entries[0].ifindex, 3);
        assert_eq!(summary.observed, 1);
    }

    #[test]
    fn summarizing_is_order_independent() {
        let mut entries = vec![
            ArpEntry::new(1, v4("10.0.0.3")),
            ArpEntry::new(2, v6("2001:db8::9")),
            ArpEntry::new(1, v4("10.0.0.1")),
            ArpEntry::new(3, v4("10.0.0.2")),
        ];
        let forward = ArpSummary::new(entries.clone(), false);
        entries.reverse();
        let reversed = ArpSummary::new(entries, false);
        assert_eq!(forward, reversed);
        assert_eq!(forward.content_key(), reversed.content_key());
    }

    #[test]
    fn the_content_key_ignores_the_total_but_not_a_port_filling_up() {
        // An ARP cache expires entries constantly. Keying on the raw total would append a history
        // row nearly every cycle, which is the failure mode the append-on-change design exists to
        // avoid — while a port going from empty to populated is a genuine change.
        let quiet = ArpSummary::new(vec![ArpEntry::new(1, v4("10.0.0.1"))], false);
        let same = ArpSummary::new(vec![ArpEntry::new(1, v4("10.0.0.1"))], false);
        assert_eq!(quiet.content_key(), same.content_key());

        let grown = ArpSummary::new(
            vec![
                ArpEntry::new(1, v4("10.0.0.1")),
                ArpEntry::new(2, v4("10.0.0.2")),
            ],
            false,
        );
        assert_ne!(quiet.content_key(), grown.content_key());

        // And an empty observation is not the same as never having looked.
        assert_ne!(quiet.content_key(), ArpSummary::default().content_key());
    }

    #[test]
    fn a_changed_mac_is_a_change() {
        // Same address, different hardware behind it: a host was replaced, or something is spoofing.
        let mut a = ArpEntry::new(1, v4("10.0.0.1"));
        a.mac = Some("aa:bb:cc:dd:ee:ff".into());
        let mut b = ArpEntry::new(1, v4("10.0.0.1"));
        b.mac = Some("11:22:33:44:55:66".into());
        assert_ne!(
            ArpSummary::new(vec![a], false).content_key(),
            ArpSummary::new(vec![b], false).content_key()
        );
    }

    #[test]
    fn the_column_list_is_the_two_the_adr_names_and_each_oid_is_distinct() {
        let cols = builtin_arp_columns();
        assert_eq!(cols.len(), 2);
        let mut oids: Vec<&str> = cols.iter().map(|(_, o)| *o).collect();
        oids.sort_unstable();
        oids.dedup();
        assert_eq!(oids.len(), 2, "a duplicated base would merge two columns");
        // `ipNetToPhysicalType` is deliberately absent — see `builtin_arp_columns`.
        assert!(!oids.contains(&"1.3.6.1.2.1.4.35.1.6"));
    }

    #[test]
    fn every_column_token_round_trips_through_serde_and_from_token() {
        for c in ArpColumn::ALL {
            assert_eq!(ArpColumn::from_token(c.as_str()), Some(c));
            let json = serde_json::to_string(&c).unwrap();
            assert_eq!(json, format!("\"{}\"", c.as_str()));
        }
    }

    #[test]
    fn the_walk_budget_is_larger_than_the_sample_it_feeds() {
        // If the sample cap were the larger of the two, `truncated` would be set by the walk before
        // the sample ever filled — and the operator-facing number would be capped by a limit nobody
        // configured. Same relationship `MAX_NEIGHBOR_WALK_ROWS` has to its own per-node cap.
        const { assert!(MAX_ARP_WALK_ROWS > MAX_ARP_ENTRIES_PER_NODE) };
    }
}
