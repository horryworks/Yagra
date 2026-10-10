// SPDX-License-Identifier: AGPL-3.0-only
//! The discovery walks' switches and cadences, as stored on the singleton `app_settings` row: the
//! L2 adjacency walk (ADR-038), the L3 interface-address, ARP and routing walks (ADR-043) and the
//! media walk (ADR-063).
//!
//! The shape of a stored row, so it lives with the repository that reads and writes it rather than
//! with `neighbors.rs` (the neighbour tables) or `arp.rs` (the ARP tables) — this layer may not
//! reach up into the modules that use it (ADR-202).

/// Default cadence for the neighbour walk. Adjacency changes on the order of months, so this is
/// deliberately two orders of magnitude slower than the metric interval — walking `lldpRemTable` on
/// a 48-port switch every minute would spend device time and rate-limit budget re-reading a
/// constant.
pub const DEFAULT_NEIGHBOR_INTERVAL_SECS: u32 = 3600;
/// Floor on the cadence, matching the `CHECK` on `app_settings.neighbor_interval_secs`.
pub const MIN_NEIGHBOR_INTERVAL_SECS: u32 = 300;
/// Ceiling on the cadence. Bounded so the setting cannot be used to effectively disable collection
/// while still reading as enabled — that is what the toggle is for.
pub const MAX_NEIGHBOR_INTERVAL_SECS: u32 = 86_400;

/// Default cadence for the ARP walk: six hours.
///
/// Slower than the neighbour and interface-address walks by design. Those read tables sized by the
/// device; this one reads a table sized by the network, and it is the only walk in ADR-043 that
/// costs a busy switch measurable work. Meraki's inventory tier made the same call at the same
/// number.
pub const DEFAULT_ARP_INTERVAL_SECS: u32 = 21_600;

/// Whether a cadence is inside the configurable band. Shared by the API edge and the tests so the
/// bound lives in one place (the shape `config::interval_in_bounds` established); the `CHECK`
/// constraint is the backstop, not the primary guard.
#[must_use]
pub fn interval_in_bounds(secs: u32) -> bool {
    (MIN_NEIGHBOR_INTERVAL_SECS..=MAX_NEIGHBOR_INTERVAL_SECS).contains(&secs)
}

/// How the deployment discovers what is connected to what, as stored on the singleton
/// `app_settings` row: the L2 adjacency walk (ADR-038) and the L3 interface-address walk (ADR-043).
///
/// Deployment-wide rather than per node or per profile: every OID involved is a fixed standard
/// (LLDP-MIB / CISCO-CDP-MIB, RFC 1213 / RFC 4293), so there is nothing to tune per device — only
/// whether to collect and how often. A finer grain (per profile) is a later increment if a fleet
/// ever needs it.
///
/// The two walks share a struct, and are resolved together once per sweep, because they answer the
/// same operator question ("is a discovery walk being issued, and how often") and because a second
/// settings query inside the scheduling loop is exactly what the sweep-level resolution exists to
/// avoid. They keep **separate** toggles and cadences: a fleet may have reason to collect one and
/// not the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdjacencySettings {
    /// Whether CDP/LLDP neighbour jobs are scheduled at all — and, since ADR-181, whether a Meraki
    /// organization's switch-port collect reads its switches' neighbours.
    pub neighbors_enabled: bool,
    /// How often each SNMP node's neighbour tables are walked, and a Meraki organization's switch
    /// neighbours are read.
    pub neighbors_interval_secs: u32,
    /// Whether interface-address jobs are scheduled at all (ADR-043).
    pub l3_enabled: bool,
    /// How often each SNMP node's `ipAddrTable`/`ipAddressTable` are walked.
    pub l3_interval_secs: u32,
    /// Whether ARP / IPv6-neighbour jobs are scheduled at all (ADR-043 Increment 3).
    ///
    /// **Defaults off**, alone among the three. The other two walks read tables sized by the device;
    /// this one reads a table sized by the network, and it is the only check in ADR-043 that costs a
    /// busy switch measurable work. An upgrade must not quietly start issuing it.
    pub arp_enabled: bool,
    /// How often each SNMP node's `ipNetToPhysicalTable`/`ipNetToMediaTable` are walked.
    pub arp_interval_secs: u32,
    /// Whether routing-adjacency jobs are scheduled at all (ADR-043 Increment 4).
    ///
    /// **Defaults on**, like the neighbour and interface-address walks and unlike the ARP one. The
    /// tables read here are sized by the device's own peering mesh, and the route probes are
    /// bounded by construction (one subtree per destination, and only for a node that holds a host
    /// address of its own), so this does not carry the cost that made ARP opt-in.
    pub routing_enabled: bool,
    /// How often each SNMP node's `bgpPeerTable`/`ospfNbrTable` are walked and its route probes
    /// issued.
    pub routing_interval_secs: u32,
    /// Whether media-type walks are issued at all (ADR-063 Inc.2).
    ///
    /// **Defaults on**, like the neighbour, interface-address and routing walks and unlike the ARP
    /// one. It reads one row per Ethernet port on the device itself, once an hour — a table sized by
    /// the device, not by the network, which is the line the ARP walk fell the wrong side of.
    pub media_enabled: bool,
    /// How often each SNMP node's `ifMauTable` (and the ENTITY-MIB fallback) is walked.
    pub media_interval_secs: u32,
}

impl Default for AdjacencySettings {
    fn default() -> Self {
        Self {
            neighbors_enabled: true,
            neighbors_interval_secs: DEFAULT_NEIGHBOR_INTERVAL_SECS,
            l3_enabled: true,
            l3_interval_secs: DEFAULT_NEIGHBOR_INTERVAL_SECS,
            arp_enabled: false,
            arp_interval_secs: DEFAULT_ARP_INTERVAL_SECS,
            routing_enabled: true,
            routing_interval_secs: DEFAULT_NEIGHBOR_INTERVAL_SECS,
            media_enabled: true,
            media_interval_secs: DEFAULT_NEIGHBOR_INTERVAL_SECS,
        }
    }
}

impl AdjacencySettings {
    /// Whether every cadence is inside the configurable band. The API edge rejects anything else.
    #[must_use]
    pub fn in_bounds(&self) -> bool {
        interval_in_bounds(self.neighbors_interval_secs)
            && interval_in_bounds(self.l3_interval_secs)
            && interval_in_bounds(self.arp_interval_secs)
            && interval_in_bounds(self.routing_interval_secs)
            && interval_in_bounds(self.media_interval_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_cadence_is_in_bounds_and_slow() {
        let d = AdjacencySettings::default();
        assert!(d.neighbors_enabled, "shipped on by default (ADR-038)");
        assert!(d.l3_enabled, "shipped on by default (ADR-043)");
        assert!(d.in_bounds());
        // A tripwire, not a tautology: dropping either cadence to the metric interval would walk
        // several extra tables on every SNMP node every minute.
        assert_eq!(d.neighbors_interval_secs, 3600);
        assert_eq!(d.l3_interval_secs, 3600);
    }

    /// The ARP walk is the exception to the two above, and the exception is the point.
    #[test]
    fn the_arp_walk_ships_off_and_slower_than_the_others() {
        let d = AdjacencySettings::default();
        assert!(
            !d.arp_enabled,
            "the one ADR-043 walk that costs a busy device real work must be opt-in — an upgrade \
             that silently started walking ipNetToPhysicalTable on every switch in a fleet is the \
             failure this default exists to prevent"
        );
        assert!(d.arp_interval_secs > d.l3_interval_secs);
        assert!(interval_in_bounds(d.arp_interval_secs));
        const { assert!(DEFAULT_ARP_INTERVAL_SECS > DEFAULT_NEIGHBOR_INTERVAL_SECS) };
    }

    /// The routing walk sides with the two cheap walks, not with ARP — and the reason is the table
    /// it reads, so pin it rather than leaving it to be re-argued.
    #[test]
    fn the_routing_walk_ships_on_because_its_tables_are_sized_by_the_device() {
        let d = AdjacencySettings::default();
        assert!(
            d.routing_enabled,
            "bgpPeerTable and ospfNbrTable are sized by the device's peering mesh, and the route \
             probes are bounded by construction — none of that is the network-sized cost that \
             made the ARP walk opt-in"
        );
        assert_eq!(d.routing_interval_secs, d.l3_interval_secs);
        assert!(interval_in_bounds(d.routing_interval_secs));
    }

    #[test]
    fn the_cadence_band_rejects_the_absurd() {
        assert!(!interval_in_bounds(0));
        assert!(!interval_in_bounds(MIN_NEIGHBOR_INTERVAL_SECS - 1));
        assert!(interval_in_bounds(MIN_NEIGHBOR_INTERVAL_SECS));
        assert!(interval_in_bounds(MAX_NEIGHBOR_INTERVAL_SECS));
        assert!(!interval_in_bounds(MAX_NEIGHBOR_INTERVAL_SECS + 1));
        assert!(!AdjacencySettings {
            neighbors_interval_secs: 30,
            ..AdjacencySettings::default()
        }
        .in_bounds());
        assert!(!AdjacencySettings {
            l3_interval_secs: 30,
            ..AdjacencySettings::default()
        }
        .in_bounds());
    }
}
