// SPDX-License-Identifier: AGPL-3.0-only
//! Routing-adjacency persistence, and the rule that decides which destinations get probed
//! (ADR-043 Increment 4, migration 0071).
//!
//! Structured observations, so this is PostgreSQL (store separation) — a peer address in a
//! `SeriesKey` label is the cardinality explosion CLAUDE.md §7.1 names (ADR-011). Runtime
//! `sqlx::query` (not the compile-time macro) so the build needs no live database.
//!
//! Modelled on [`crate::l3`] with one deliberate difference: **there is no change-history table**.
//! The reasoning is in migration 0071's header — the durable answer to "how long have these two been
//! connected" is `node_links.first_seen`, keyed by the pair rather than by whichever end reported,
//! and a per-node history here would be a second, noisier answer to a question already answered.
//!
//! The other half of this module is [`RoutingPlan`], which is pure: it decides which host addresses
//! each node is asked to probe for. That decision is core's, not the poller's, and it is the reason
//! `inetCidrRouteTable` can be consulted at all without walking it.

use chrono::{DateTime, Utc};
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use uuid::Uuid;
use yagra_common::{NodeId, RoutingSnapshot, MAX_ROUTE_PROBES_PER_NODE};

// The host addresses this plan is built from are read by `L3Repo::host_addresses` — the query lives
// with the table it reads, not here, so `node_l3` has exactly one owner.

/// PostgreSQL-backed store for node routing adjacency.
pub struct RoutingRepo {
    pool: PgPool,
}

impl RoutingRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Record one observed snapshot, replacing whatever was stored.
    ///
    /// `first_seen` is held across an unchanged observation and reset when the content key moves —
    /// the same rule `node_l3` uses, minus the history append the CTE there exists for.
    ///
    /// Only ever called with a snapshot the poller actually observed. A collection where *neither*
    /// half answered sends nothing at all, so this is never reached with an empty stand-in — which
    /// is what stops one timed-out walk from erasing a node's adjacency, and with it every
    /// point-to-point link that node was in.
    pub async fn record_observation(
        &self,
        node_id: Uuid,
        snapshot: &RoutingSnapshot,
    ) -> anyhow::Result<()> {
        let key = snapshot.content_key();
        let count = i32::try_from(snapshot.len()).unwrap_or(i32::MAX);
        sqlx::query(
            "INSERT INTO node_routing \
                (node_id, routing_key, adjacencies, adjacency_count, truncated, \
                 first_seen, last_seen) \
             VALUES ($1, $2, $3, $4, $5, now(), now()) \
             ON CONFLICT (node_id) DO UPDATE SET \
                routing_key = EXCLUDED.routing_key, \
                adjacencies = EXCLUDED.adjacencies, \
                adjacency_count = EXCLUDED.adjacency_count, \
                truncated = EXCLUDED.truncated, \
                first_seen = CASE WHEN node_routing.routing_key = EXCLUDED.routing_key \
                                  THEN node_routing.first_seen ELSE now() END, \
                last_seen = now()",
        )
        .bind(node_id)
        .bind(&key)
        .bind(Json(snapshot))
        .bind(count)
        .bind(snapshot.truncated)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Every node's current snapshot — the derivation task's third input (ADR-043).
    ///
    /// Deliberately unpaged, for the same reason `L3Repo::all_current` is: deriving a graph from a
    /// slice of the fleet produces a wrong graph, not a partial one.
    pub async fn all_current(&self) -> anyhow::Result<Vec<(NodeId, RoutingSnapshot)>> {
        let rows = sqlx::query("SELECT node_id, adjacencies FROM node_routing")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                let snapshot: Json<RoutingSnapshot> = row.try_get("adjacencies")?;
                Ok((NodeId(row.try_get("node_id")?), snapshot.0))
            })
            .collect()
    }

    /// The newest `last_seen` across every node, or `None` when nothing has been observed.
    ///
    /// The third watermark in the derivation task's change signal — see
    /// `L3Repo::observation_watermark` for why `config_gen` alone is not enough.
    pub async fn observation_watermark(&self) -> anyhow::Result<Option<DateTime<Utc>>> {
        let row = sqlx::query("SELECT max(last_seen) AS w FROM node_routing")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("w")?)
    }
}

/// Which host addresses each node should be asked to probe for.
///
/// **The whole cost model of Increment 4 lives here.** A route probe is cheap per destination and
/// there is no way to ask "which of your routes are host routes" in one request, so the question has
/// to be turned around: core supplies the list of destinations worth asking about, and each is one
/// bounded subtree walk.
///
/// Two rules keep the list small enough to send to a fleet:
///
/// 1. **Only nodes that hold an unmatched host address of their own are asked to probe.** A `/32`
///    point-to-point has a host address at *both* ends, so a node with none is not an endpoint of
///    one and probing it would find nothing. This is what stops the probe list from being attached
///    to every SNMP node in a 50,000-node fleet.
/// 2. **A node is never asked about its own addresses.** The route to one's own address is a local
///    route to oneself, which names no peer.
///
/// The map is therefore the size of the fleet's point-to-point routers, not of the fleet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoutingPlan {
    probes: BTreeMap<NodeId, Vec<IpAddr>>,
}

impl RoutingPlan {
    /// Build the plan from each node's unmatched host addresses.
    ///
    /// "Unmatched" is the caller's judgement — [`host_addresses`] reads them off `node_l3`, where
    /// Increment 1 stores host routes precisely so this increment can resolve them. Every list is
    /// sorted and then truncated, so which destinations survive the cap is deterministic rather
    /// than a function of map iteration order.
    #[must_use]
    pub fn build(host_addrs: &BTreeMap<NodeId, BTreeSet<IpAddr>>) -> Self {
        let all: BTreeSet<IpAddr> = host_addrs.values().flatten().copied().collect();
        let probes = host_addrs
            .iter()
            .filter(|(_, own)| !own.is_empty())
            .map(|(id, own)| {
                let mut targets: Vec<IpAddr> =
                    all.iter().filter(|ip| !own.contains(ip)).copied().collect();
                targets.truncate(MAX_ROUTE_PROBES_PER_NODE);
                (*id, targets)
            })
            .filter(|(_, targets)| !targets.is_empty())
            .collect();
        Self { probes }
    }

    /// The destinations this node should probe for. Empty for every node that is not an endpoint of
    /// a point-to-point host-route link, which is nearly all of them.
    #[must_use]
    pub fn targets_for(&self, node: NodeId) -> &[IpAddr] {
        self.probes.get(&node).map_or(&[], Vec::as_slice)
    }

    /// How many nodes will be asked to probe. Reported as a metric so an empty plan is visible
    /// rather than looking like "the probe found nothing".
    #[must_use]
    pub fn prober_count(&self) -> usize {
        self.probes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This module's own source, for the SQL-shape assertions below. The upsert's `first_seen` rule
    /// and the host-address filter live entirely inside SQL strings, so nothing else can catch a
    /// rewrite that changes their meaning — the peer stores pin their statements the same way.
    const SRC: &str = include_str!("routing.rs");

    /// The executable code above this test module, comments stripped. Without this, a test that
    /// reads its own file matches its own needles (testing.md's self-match trap) and a doc comment
    /// *naming* a banned pattern reads as the pattern itself.
    fn production_source() -> String {
        SRC.split("#[cfg(test)]")
            .next()
            .expect("split always yields a first element")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn ids(n: usize) -> Vec<NodeId> {
        (1..=n)
            .map(|i| NodeId(uuid::Uuid::from_u128(i as u128)))
            .collect()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn plan(rows: &[(NodeId, &[&str])]) -> RoutingPlan {
        let map: BTreeMap<NodeId, BTreeSet<IpAddr>> = rows
            .iter()
            .map(|(id, addrs)| (*id, addrs.iter().map(|s| ip(s)).collect()))
            .collect();
        RoutingPlan::build(&map)
    }

    #[test]
    fn each_end_of_a_point_to_point_probes_for_the_other() {
        let n = ids(2);
        let p = plan(&[(n[0], &["198.51.100.1"]), (n[1], &["198.51.100.2"])]);
        assert_eq!(p.targets_for(n[0]), &[ip("198.51.100.2")]);
        assert_eq!(p.targets_for(n[1]), &[ip("198.51.100.1")]);
        assert_eq!(p.prober_count(), 2);
    }

    #[test]
    fn a_node_with_no_host_address_is_never_asked_to_probe() {
        // The rule that keeps this off 50,000 nodes: a `/32` point-to-point has a host address at
        // both ends, so a node with none is not an endpoint of one.
        let n = ids(3);
        let p = plan(&[
            (n[0], &["198.51.100.1"]),
            (n[1], &["198.51.100.2"]),
            (n[2], &[]),
        ]);
        assert!(p.targets_for(n[2]).is_empty());
        assert_eq!(p.prober_count(), 2);
    }

    #[test]
    fn a_node_is_never_asked_about_its_own_addresses() {
        // The route to one's own address is a local route to oneself and names no peer.
        let n = ids(2);
        let p = plan(&[
            (n[0], &["198.51.100.1", "203.0.113.1"]),
            (n[1], &["198.51.100.2"]),
        ]);
        assert_eq!(p.targets_for(n[0]), &[ip("198.51.100.2")]);
        let mut expected = vec![ip("198.51.100.1"), ip("203.0.113.1")];
        expected.sort();
        assert_eq!(p.targets_for(n[1]), expected.as_slice());
    }

    #[test]
    fn the_only_node_with_a_host_address_probes_for_nothing() {
        // The lab: the USG holds a PPPoE `/32` and nothing else in the inventory does, so there is
        // no destination worth asking about and no probe is issued at all.
        let n = ids(2);
        let p = plan(&[(n[0], &["133.123.189.109"]), (n[1], &[])]);
        assert!(p.targets_for(n[0]).is_empty());
        assert_eq!(p.prober_count(), 0, "an empty probe list is not a probe");
    }

    #[test]
    fn the_cap_is_applied_after_sorting_so_truncation_is_deterministic() {
        let n = ids(2);
        let many: Vec<String> = (0..MAX_ROUTE_PROBES_PER_NODE + 20)
            .map(|i| format!("198.51.{}.{}", i / 250, i % 250 + 1))
            .collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let p = plan(&[(n[0], &refs), (n[1], &["203.0.113.1"])]);

        let targets = p.targets_for(n[1]);
        assert_eq!(targets.len(), MAX_ROUTE_PROBES_PER_NODE);
        assert!(
            targets.windows(2).all(|w| w[0] < w[1]),
            "the list must be ordered, or which targets survive the cap is arbitrary"
        );
        // And building the same input twice gives the same answer.
        assert_eq!(p, plan(&[(n[0], &refs), (n[1], &["203.0.113.1"])]));
    }

    #[test]
    fn a_v6_host_route_is_planned_alongside_a_v4_one() {
        let n = ids(2);
        let p = plan(&[
            (n[0], &["2001:db8::1", "198.51.100.1"]),
            (n[1], &["2001:db8::2"]),
        ]);
        assert_eq!(p.targets_for(n[0]), &[ip("2001:db8::2")]);
        assert!(p.targets_for(n[1]).contains(&ip("198.51.100.1")));
        assert!(p.targets_for(n[1]).contains(&ip("2001:db8::1")));
    }

    #[test]
    fn an_empty_fleet_plans_nothing() {
        assert_eq!(RoutingPlan::default().prober_count(), 0);
        assert!(RoutingPlan::default().targets_for(ids(1)[0]).is_empty());
    }

    #[test]
    fn first_seen_survives_an_unchanged_observation() {
        // "How long has this node had these adjacencies" is the column's only purpose; resetting it
        // on every poll would make every peering look brand new.
        assert!(
            SRC.contains("first_seen = CASE WHEN node_routing.routing_key = EXCLUDED.routing_key")
        );
        assert!(SRC.contains("THEN node_routing.first_seen ELSE now() END"));
    }

    #[test]
    fn the_stored_key_is_the_models_own_content_key() {
        // Re-deriving it here would let the reader and the writer disagree about what a change is.
        assert!(SRC.contains("snapshot.content_key()"));
    }

    #[test]
    fn an_observation_watermark_exists_for_the_derivation_trigger() {
        // If this statement is ever removed, a routing-only change stops redrawing the map.
        assert!(SRC.contains("SELECT max(last_seen) AS w FROM node_routing"));
    }

    #[test]
    fn no_statement_pages_with_offset() {
        assert!(
            !production_source().contains("OFFSET"),
            "OFFSET paging — rows shift under the reader (ADR-019)"
        );
    }

    #[test]
    fn every_statement_binds_its_values_instead_of_interpolating_them() {
        let src = production_source();
        for builder in ["format!(", "push_str("] {
            assert!(
                !src.contains(builder),
                "SQL may be being built by string concatenation ({builder}); bind the value instead"
            );
        }
    }
}
