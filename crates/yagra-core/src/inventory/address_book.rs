// SPDX-License-Identifier: AGPL-3.0-only
//! Which node a datagram came from, by its source address (ADR-245) — the one book the event
//! matcher and the flow ingest both read.
//!
//! A router seldom sends its syslog, its traps or its flow exports from the address it is
//! monitored at: it sends from whichever interface faces the collector, or from a loopback. So the
//! book holds every address a node is known by, in two tiers, and asks them in order:
//!
//! 1. **The inventory address** (`nodes.address`). Always wins, so nothing that matched before this
//!    book existed changes node — even where another node's interface carries the same address.
//! 2. **An interface address** (`node_l3`). Counts only when exactly one node carries it, the rule
//!    the network map uses (`yagra_topology::sole_claimant`, ADR-043 decision 2): a VRRP/HSRP
//!    virtual address or a private plan reused at every site identifies nobody. Where several
//!    carry it, the pool of the poller that received the datagram narrows them, and a single
//!    survivor is the answer. A sole carrier is held to the same pool: a datagram received by
//!    another pool's poller is not matched by an interface address at all.
//!
//! Addresses that never name one device are left out of the second tier entirely
//! ([`usable_source`]).
//!
//! ⚠️ What this cannot see (ADR-245, written down rather than solved): an address reused at a site
//! Yagra does not monitor still has one claimant here, and a syslog relay or a NAT matches on any of
//! its interfaces. Both attribute traffic to a node that did not send it.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use uuid::Uuid;
use yagra_base::groups::GroupRepo;
use yagra_base::repo::{CarriedAddressesMark, ManagedAddress, NodeRepo};

use crate::fleet::poolres::PoolResolver;

/// How long the interface-address lists may be kept before they are read whole again even though
/// the mark did not move — a backstop for a change the mark cannot see, not the normal path. The
/// mark misses a set change whose transaction began before the mark was read and committed after
/// it: `first_seen` is `now()`, the transaction's start, so the row lands older than the newest
/// value already observed while the row count stays the same.
const CARRIED_FULL_READ: Duration = Duration::from_secs(600);

/// Which tier answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// The node's inventory address.
    Managed,
    /// The one node whose interface carries the address.
    Carried,
    /// Several nodes carry it; the receiving pool left one.
    CarriedInPool,
}

impl Via {
    /// The metric label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::Carried => "carried",
            Self::CarriedInPool => "carried_in_pool",
        }
    }
}

/// A source address resolved to a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    pub node: Uuid,
    pub via: Via,
}

/// Whether an interface address can say which one device sent a datagram (ADR-245 decision 2).
///
/// Loopback, unspecified, link-local, multicast and the limited broadcast are carried by many
/// devices at once (every Linux host reports `127.0.0.1`), or are never a datagram's source.
#[must_use]
pub fn usable_source(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            !(v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_broadcast())
        }
        IpAddr::V6(v6) => {
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || v6.is_unicast_link_local())
        }
    }
}

/// One snapshot of every address the fleet is known by.
#[derive(Debug, Default)]
pub struct AddressBook {
    managed: HashMap<IpAddr, Uuid>,
    /// Sorted and deduplicated per address.
    carried: HashMap<IpAddr, Vec<Uuid>>,
    /// Each node's effective poll pool.
    pools: HashMap<Uuid, Box<str>>,
}

impl AddressBook {
    /// Build from the two stored halves and the folder tree's pools.
    ///
    /// An interface claim from a node absent from `managed` is dropped: the lists are cached longer
    /// than the node rows, so it is a node deleted since they were read.
    #[must_use]
    pub fn build(
        managed: &[ManagedAddress],
        carried: &[(Uuid, IpAddr)],
        pools: &PoolResolver,
    ) -> Self {
        let mut book = Self {
            managed: HashMap::with_capacity(managed.len()),
            carried: HashMap::new(),
            pools: HashMap::with_capacity(managed.len()),
        };
        for m in managed {
            // Two nodes at one inventory address: the first row wins, as it always has.
            book.managed
                .entry(m.address.to_canonical())
                .or_insert(m.node);
            book.pools
                .insert(m.node, pools.pool_of(m.pool.as_deref(), m.group).into());
        }
        for &(node, ip) in carried {
            if !book.pools.contains_key(&node) || !usable_source(ip) {
                continue;
            }
            book.carried
                .entry(ip.to_canonical())
                .or_default()
                .push(node);
        }
        for claimants in book.carried.values_mut() {
            claimants.sort_unstable();
            claimants.dedup();
        }
        book
    }

    /// The node `source` identifies, received by a poller of `pool` when that is known.
    #[must_use]
    pub fn resolve(&self, source: IpAddr, pool: Option<&str>) -> Option<Resolved> {
        let source = source.to_canonical();
        if let Some(&node) = self.managed.get(&source) {
            return Some(Resolved {
                node,
                via: Via::Managed,
            });
        }
        let claimants = self.carried.get(&source)?;
        // The receiving pool, when known, must be the carrier's own — even for a sole carrier. A
        // private plan reused at a site Yagra does not monitor has one claimant here, and a datagram
        // from that site arrives through that site's poller, not the claimant's (user decision,
        // 2026-11-08). The cost: a site whose devices send to a poller of another pool is matched
        // by its inventory address only.
        let in_pool = |n: &Uuid| pool.is_none_or(|p| self.pools.get(n).is_some_and(|q| &**q == p));
        if let [node] = claimants.as_slice() {
            return in_pool(node).then_some(Resolved {
                node: *node,
                via: Via::Carried,
            });
        }
        let pool = pool?;
        let mut in_pool = claimants
            .iter()
            .filter(|n| self.pools.get(n).is_some_and(|p| &**p == pool));
        match (in_pool.next(), in_pool.next()) {
            (Some(&node), None) => Some(Resolved {
                node,
                via: Via::CarriedInPool,
            }),
            _ => None,
        }
    }

    /// How many addresses each tier holds.
    #[must_use]
    pub fn sizes(&self) -> (usize, usize) {
        (self.managed.len(), self.carried.len())
    }
}

/// The interface-address half as last read, and when the book was last rebuilt.
#[derive(Default)]
struct Cached {
    mark: Option<CarriedAddressesMark>,
    read_at: Option<Instant>,
    carried: Vec<(Uuid, IpAddr)>,
    refreshed_at: Option<Instant>,
}

/// The shared book and how it is kept current. One per process: the event engine owns it and the
/// flow ingest borrows it, so the interface lists are read once for both (ADR-245 decision 3).
#[derive(Default)]
pub struct AddressBookCache {
    book: RwLock<Arc<AddressBook>>,
    /// Also serialises refreshes, so two callers arriving together do not both read the lists.
    cached: tokio::sync::Mutex<Cached>,
}

impl AddressBookCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The book as of the last successful refresh. Empty before the first one.
    #[must_use]
    pub fn current(&self) -> Arc<AddressBook> {
        self.book
            .read()
            .expect("address book lock poisoned")
            .clone()
    }

    /// Rebuild the book. The node rows and the folder pools are read every time (one cheap row per
    /// node); the interface lists only when their mark moved. On any failed read the previous book
    /// stays — a database that blinked must not turn into "nothing matches".
    pub async fn refresh(&self, nodes: &NodeRepo) -> anyhow::Result<()> {
        let mut cached = self.cached.lock().await;
        self.rebuild(nodes, &mut cached).await
    }

    /// [`Self::refresh`], unless someone already did within `max_age`.
    pub async fn refresh_if_older(
        &self,
        nodes: &NodeRepo,
        max_age: Duration,
    ) -> anyhow::Result<()> {
        let mut cached = self.cached.lock().await;
        if cached.refreshed_at.is_some_and(|t| t.elapsed() < max_age) {
            return Ok(());
        }
        self.rebuild(nodes, &mut cached).await
    }

    async fn rebuild(&self, nodes: &NodeRepo, cached: &mut Cached) -> anyhow::Result<()> {
        let managed = nodes.managed_addresses().await?;
        let pools = PoolResolver::build(GroupRepo::new(nodes.pool()).pool_rows().await?);
        let mark = nodes.carried_addresses_mark().await?;
        let lists_stale = cached.mark != Some(mark)
            || cached
                .read_at
                .is_none_or(|t| t.elapsed() >= CARRIED_FULL_READ);
        if lists_stale {
            cached.carried = nodes.carried_addresses().await?;
            cached.mark = Some(mark);
            cached.read_at = Some(Instant::now());
        }
        let book = AddressBook::build(&managed, &cached.carried, &pools);
        let (m, c) = book.sizes();
        metrics::gauge!("yagra_address_book_addresses", "tier" => "managed").set(m as f64);
        metrics::gauge!("yagra_address_book_addresses", "tier" => "carried").set(c as f64);
        *self.book.write().expect("address book lock poisoned") = Arc::new(book);
        cached.refreshed_at = Some(Instant::now());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn node(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn managed(n: u128, addr: &str, pool: Option<&str>) -> ManagedAddress {
        ManagedAddress {
            node: node(n),
            address: ip(addr),
            pool: pool.map(str::to_owned),
            group: None,
        }
    }

    fn book(managed: &[ManagedAddress], carried: &[(u128, &str)]) -> AddressBook {
        let carried: Vec<(Uuid, IpAddr)> = carried.iter().map(|&(n, a)| (node(n), ip(a))).collect();
        AddressBook::build(managed, &carried, &PoolResolver::empty())
    }

    #[test]
    fn the_inventory_address_still_matches() {
        let b = book(&[managed(1, "192.0.2.1", None)], &[]);
        assert_eq!(
            b.resolve(ip("192.0.2.1"), None),
            Some(Resolved {
                node: node(1),
                via: Via::Managed
            })
        );
        assert_eq!(b.resolve(ip("192.0.2.2"), None), None);
    }

    #[test]
    fn an_interface_address_carried_by_one_node_matches_it() {
        let b = book(&[managed(1, "192.0.2.1", None)], &[(1, "198.51.100.1")]);
        assert_eq!(
            b.resolve(ip("198.51.100.1"), Some("default")),
            Some(Resolved {
                node: node(1),
                via: Via::Carried
            })
        );
    }

    /// Nothing that matched before ADR-245 changes node: another node's interface carrying the
    /// same address does not take it over.
    /// A sole carrier in another pool is not the sender: the same private address at a site Yagra
    /// does not monitor arrives through that site's poller (user decision, 2026-11-08).
    #[test]
    fn a_sole_carrier_in_another_pool_is_not_matched() {
        let b = book(
            &[managed(1, "192.0.2.1", Some("osaka"))],
            &[(1, "10.1.1.1")],
        );
        assert_eq!(b.resolve(ip("10.1.1.1"), Some("tokyo")), None);
        assert_eq!(
            b.resolve(ip("10.1.1.1"), Some("osaka")).map(|r| r.via),
            Some(Via::Carried)
        );
        // With no receiving pool to compare, the sole carrier still answers.
        assert_eq!(
            b.resolve(ip("10.1.1.1"), None).map(|r| r.node),
            Some(node(1))
        );
        // The inventory address is not held to the pool: nothing that matched before changes.
        assert_eq!(
            b.resolve(ip("192.0.2.1"), Some("tokyo")).map(|r| r.node),
            Some(node(1))
        );
    }

    #[test]
    fn the_inventory_address_beats_another_nodes_interface() {
        let b = book(
            &[managed(1, "192.0.2.1", None), managed(2, "192.0.2.2", None)],
            &[(2, "192.0.2.1")],
        );
        assert_eq!(
            b.resolve(ip("192.0.2.1"), None).map(|r| r.node),
            Some(node(1))
        );
    }

    #[test]
    fn an_address_two_nodes_carry_identifies_neither_without_a_pool() {
        let b = book(
            &[managed(1, "192.0.2.1", None), managed(2, "192.0.2.2", None)],
            &[(1, "10.0.0.1"), (2, "10.0.0.1")],
        );
        assert_eq!(b.resolve(ip("10.0.0.1"), None), None);
        // Both are in the default pool, so the pool cannot pick one either.
        assert_eq!(b.resolve(ip("10.0.0.1"), Some("default")), None);
    }

    #[test]
    fn the_receiving_pool_picks_between_two_carriers() {
        let b = book(
            &[
                managed(1, "192.0.2.1", Some("tokyo")),
                managed(2, "192.0.2.2", Some("osaka")),
            ],
            &[(1, "10.0.0.1"), (2, "10.0.0.1")],
        );
        assert_eq!(
            b.resolve(ip("10.0.0.1"), Some("osaka")),
            Some(Resolved {
                node: node(2),
                via: Via::CarriedInPool
            })
        );
        assert_eq!(b.resolve(ip("10.0.0.1"), Some("nagoya")), None);
    }

    #[test]
    fn the_pool_is_inherited_from_the_folder() {
        let folder = Uuid::from_u128(100);
        let pools = PoolResolver::build(vec![(folder, None, Some("osaka".to_owned()))]);
        let mut m2 = managed(2, "192.0.2.2", None);
        m2.group = Some(folder);
        let b = AddressBook::build(
            &[managed(1, "192.0.2.1", None), m2],
            &[(node(1), ip("10.0.0.1")), (node(2), ip("10.0.0.1"))],
            &pools,
        );
        assert_eq!(
            b.resolve(ip("10.0.0.1"), Some("osaka")).map(|r| r.node),
            Some(node(2))
        );
        assert_eq!(
            b.resolve(ip("10.0.0.1"), Some("default")).map(|r| r.node),
            Some(node(1))
        );
    }

    /// The same node listing one address twice (two tables, two ports) is still one carrier.
    #[test]
    fn one_node_listing_an_address_twice_is_one_carrier() {
        let b = book(
            &[managed(1, "192.0.2.1", None)],
            &[(1, "10.0.0.1"), (1, "10.0.0.1")],
        );
        assert_eq!(
            b.resolve(ip("10.0.0.1"), None).map(|r| r.node),
            Some(node(1))
        );
    }

    #[test]
    fn addresses_that_name_no_one_device_are_not_carried() {
        let b = book(
            &[managed(1, "192.0.2.1", None)],
            &[
                (1, "127.0.0.1"),
                (1, "0.0.0.0"),
                (1, "169.254.10.1"),
                (1, "224.0.0.5"),
                (1, "255.255.255.255"),
                (1, "::1"),
                (1, "::"),
                (1, "fe80::1"),
                (1, "ff02::1"),
            ],
        );
        assert_eq!(b.sizes(), (1, 0));
        for s in ["127.0.0.1", "169.254.10.1", "fe80::1", "::1"] {
            assert_eq!(b.resolve(ip(s), None), None, "{s}");
        }
    }

    #[test]
    fn global_and_private_addresses_are_usable() {
        for s in [
            "10.0.0.1",
            "192.168.1.1",
            "198.51.100.7",
            "2001:db8::1",
            "fd00::1",
        ] {
            assert!(usable_source(ip(s)), "{s}");
        }
    }

    /// A dual-stack socket reports an IPv4 sender as `::ffff:a.b.c.d`; it is the same device.
    #[test]
    fn an_ipv4_mapped_source_matches_the_ipv4_address() {
        let b = book(&[managed(1, "192.0.2.1", None)], &[(1, "198.51.100.1")]);
        assert_eq!(
            b.resolve(ip("::ffff:192.0.2.1"), None).map(|r| r.node),
            Some(node(1))
        );
        assert_eq!(
            b.resolve(ip("::ffff:198.51.100.1"), None).map(|r| r.node),
            Some(node(1))
        );
    }

    /// The lists are cached longer than the node rows: a claim from a node that has since been
    /// deleted must not match.
    #[test]
    fn a_claim_from_a_node_no_longer_in_the_inventory_is_dropped() {
        let b = book(&[managed(1, "192.0.2.1", None)], &[(9, "10.0.0.9")]);
        assert_eq!(b.resolve(ip("10.0.0.9"), None), None);
    }

    /// And a deleted node's claim no longer makes an address ambiguous for the survivor.
    #[test]
    fn a_deleted_nodes_claim_does_not_make_an_address_ambiguous() {
        let b = book(
            &[managed(1, "192.0.2.1", None)],
            &[(1, "10.0.0.1"), (9, "10.0.0.1")],
        );
        assert_eq!(
            b.resolve(ip("10.0.0.1"), None).map(|r| r.node),
            Some(node(1))
        );
    }

    #[test]
    fn every_via_has_its_own_label() {
        let all = [Via::Managed, Via::Carried, Via::CarriedInPool];
        let labels: std::collections::HashSet<_> = all.iter().map(|v| v.as_str()).collect();
        assert_eq!(labels.len(), all.len());
    }
}
