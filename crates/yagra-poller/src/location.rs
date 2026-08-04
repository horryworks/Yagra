// SPDX-License-Identifier: AGPL-3.0-only
//! Where this poller sits on the network, reported once per heartbeat (ADR-043).
//!
//! Core roots the derived dependency graph at the poller: a node's parents are the nodes
//! immediately before it on the path *from* a poller, so a graph with no poller in it has no
//! direction and no roots. The only party that knows a poller's own addresses is the poller, so it
//! reports them and core decides what they mean.
//!
//! The enumeration itself is one call into the platform; everything this module actually decides is
//! the filter, which is pure and tested. That split matters because the filter is where a mistake is
//! silent — a loopback address left in the list matches every node's `127.0.0.0/8` and would anchor
//! the graph at everything.

use std::collections::BTreeSet;
use std::net::IpAddr;

/// Cap on how many of its own addresses a poller reports.
///
/// A host with many virtual interfaces (a container host, a router running the poller) can have
/// hundreds. Anchor resolution scans nodes × addresses, and past a handful the extra addresses stop
/// telling core anything new about where the poller is.
pub const MAX_MGMT_ADDRS: usize = 16;

/// This host's usable interface addresses, sorted and capped.
///
/// Returns empty when the platform refuses to enumerate — indistinguishable, deliberately, from a
/// host whose addresses are all filtered out. Both mean "core cannot place this poller from its
/// heartbeat", which is one case with one answer (`pollers.anchor_node_id`), not two.
#[must_use]
pub fn local_mgmt_addrs() -> Vec<IpAddr> {
    match if_addrs::get_if_addrs() {
        Ok(ifs) => usable(ifs.into_iter().map(|i| i.addr.ip())),
        Err(e) => {
            tracing::warn!(error = %e, "could not enumerate local interfaces; core cannot place this poller from its heartbeat");
            Vec::new()
        }
    }
}

/// Keep only addresses that could place the poller on a monitored segment.
///
/// Dropped, and why each would be actively wrong rather than merely useless:
///
/// * **Loopback** — every host has `127.0.0.1`, so it matches every node and would anchor the graph
///   at the whole fleet.
/// * **Link-local** (`169.254/16`, `fe80::/10`) — present on any interface that failed to get an
///   address, and shared by unrelated hosts.
/// * **Unspecified / multicast** — not an interface's identity at all.
///
/// The result is sorted so the heartbeat's contents depend on the host's addresses and not on the
/// order the platform happened to list its interfaces in; otherwise two beats from an unchanged
/// poller could differ.
#[must_use]
pub fn usable(addrs: impl Iterator<Item = IpAddr>) -> Vec<IpAddr> {
    let mut out: BTreeSet<IpAddr> = BTreeSet::new();
    for ip in addrs {
        let drop = match ip {
            IpAddr::V4(v4) => {
                v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() || v4.is_multicast()
            }
            IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_multicast()
                    // `Ipv6Addr::is_unicast_link_local` is unstable, so test `fe80::/10` directly.
                    || (v6.segments()[0] & 0xffc0) == 0xfe80
            }
        };
        if !drop {
            out.insert(ip);
        }
    }
    out.into_iter().take(MAX_MGMT_ADDRS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ips(v: &[&str]) -> Vec<IpAddr> {
        v.iter().map(|s| s.parse().unwrap()).collect()
    }

    #[test]
    fn loopback_and_link_local_are_dropped_in_both_families() {
        // The load-bearing case: `127.0.0.1` sits in the `127.0.0.0/8` every node also has, so
        // leaving it in would make every monitored node share a subnet with the poller and turn the
        // whole fleet into anchors — a graph in which nothing can ever be suppressed.
        let got = usable(
            ips(&[
                "127.0.0.1",
                "::1",
                "169.254.3.4",
                "fe80::1",
                "febf::1",
                "0.0.0.0",
                "::",
                "224.0.0.1",
                "ff02::1",
                "192.168.1.9",
                "2001:db8::9",
            ])
            .into_iter(),
        );
        assert_eq!(got, ips(&["192.168.1.9", "2001:db8::9"]));
    }

    #[test]
    fn fec0_is_kept_because_it_is_not_link_local() {
        // `fe80::/10` ends at `febf::`; site-local `fec0::/10` is deprecated but routable, and a
        // mask that caught it would silently drop a real address. This pins the boundary.
        assert_eq!(usable(ips(&["fec0::1"]).into_iter()), ips(&["fec0::1"]));
    }

    #[test]
    fn the_list_is_deduplicated_sorted_and_capped() {
        let mut many: Vec<IpAddr> = (0..40u8).map(|i| IpAddr::from([10, 0, 0, i])).collect();
        many.extend(many.clone()); // the same address on two interfaces is one fact
        many.reverse();
        let got = usable(many.into_iter());
        assert_eq!(got.len(), MAX_MGMT_ADDRS);
        assert!(got.windows(2).all(|w| w[0] < w[1]), "not sorted: {got:?}");
        assert_eq!(got[0], IpAddr::from([10, 0, 0, 0]));
    }

    #[test]
    fn a_host_with_nothing_usable_reports_nothing_rather_than_something_wrong() {
        assert!(usable(ips(&["127.0.0.1", "::1"]).into_iter()).is_empty());
        assert!(usable(std::iter::empty()).is_empty());
    }
}
