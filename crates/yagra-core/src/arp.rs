// SPDX-License-Identifier: AGPL-3.0-only
//! ARP-based endpoint discovery: what has replied on the wire, and which of it nobody monitors
//! (ADR-043 Increment 3, migration 0070).
//!
//! Two stores and the pure rule between them:
//!
//! * [`ArpRepo`] holds one aggregated summary per node — the observation, replaced wholesale, with
//!   the same "a failed walk writes nothing" discipline as [`crate::neighbors`] and [`crate::l3`].
//! * [`DiscoveredRepo`] holds one row per **endpoint**, fleet-wide — the finding.
//! * [`unmonitored`] turns the first into the second, and is pure so the rule that decides what
//!   counts as "unmonitored" is testable without a database.
//!
//! ## Why the finding is keyed by the endpoint and not by the observation
//!
//! Every router on a segment sees the same host. Keyed by `(router, port, address)` an operator
//! reviewing what is unmonitored would read the same host once per router, and the count — the
//! number the whole feature exists to produce — would be inflated by the redundancy of the network
//! it is describing. So `via_node` is *who told us*, not part of the identity.
//!
//! ## Why these endpoints are not map vertices
//!
//! Deliberate, and the one design point most likely to be "fixed" later. An unmonitored endpoint has
//! no state: no liveness, no thresholds, nothing to colour. Drawing four thousand stateless boxes is
//! exactly what `MAP_CAP` exists to prevent, and it would bury the nodes that do have state. An
//! endpoint becomes a vertex by becoming a **node**, at which point Increment 1's derivation picks
//! it up with no special case anywhere.

use chrono::{DateTime, Utc};
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use uuid::Uuid;
use yagra_common::{ArpSummary, NodeId};

/// Default cadence for the ARP walk: six hours.
///
/// Slower than the neighbour and interface-address walks by design. Those read tables sized by the
/// device; this one reads a table sized by the network, and it is the only walk in ADR-043 that
/// costs a busy switch measurable work. Meraki's inventory tier made the same call at the same
/// number.
pub const DEFAULT_ARP_INTERVAL_SECS: u32 = 21_600;

/// Fleet-wide ceiling on stored endpoints.
///
/// Not a performance guess: a campus with a few thousand hosts fits comfortably, and a deployment
/// that blows past this is one where the answer has stopped being a review list and started being a
/// second inventory. The overflow is reported (`truncated_total`) and the **oldest-seen** rows are
/// the ones dropped, so what survives is what is currently on the network.
pub const MAX_DISCOVERED_ENDPOINTS: usize = 10_000;

/// How long an endpoint survives without being seen again: seven days.
///
/// Shorter than any other retention here on purpose. A laptop that appeared once and left is not a
/// finding, and without an age rule a busy campus fills the table with them until nothing in it can
/// be reviewed.
pub const DISCOVERED_RETENTION_SECS: i64 = 7 * 86_400;

/// How often the endpoint sweep runs when there is anything to sweep.
pub const ENDPOINT_SWEEP_INTERVAL_SECS: u64 = 300;

/// One endpoint the fleet has seen but does not monitor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredEndpoint {
    pub id: Uuid,
    pub ip: IpAddr,
    pub mac: Option<String>,
    /// Which monitored node resolved it; `None` once that node has been deleted.
    pub via_node: Option<NodeId>,
    pub via_ifindex: Option<u32>,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    /// Set once the address became an inventory node — imported from here or added by hand.
    pub promoted_node_id: Option<NodeId>,
}

/// One endpoint as the sweep computed it, before it reaches the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointObservation {
    pub ip: IpAddr,
    pub mac: Option<String>,
    pub via_node: NodeId,
    pub via_ifindex: u32,
}

/// Every endpoint the fleet observed that is **not** already an inventory address.
///
/// Pure: summaries and the known-address set in, findings out. No clock, no database.
///
/// `known` must carry both `nodes.address` **and** every address in `node_l3`. Using only the node
/// addresses looks equivalent and is not: a router monitored on its management address answers ARP
/// for its LAN interface too, so its own `192.168.1.1` would be reported as an unmonitored endpoint
/// on every segment it terminates. That is the false positive that would make the list unreadable
/// on day one, and it is why this takes a set rather than a node list.
///
/// When several nodes see the same endpoint the lowest node id wins, so the attribution does not
/// flip between sweeps as the map's redundancy shifts — a row whose `via_node` changed every five
/// minutes would look like the endpoint was moving.
#[must_use]
pub fn unmonitored(
    summaries: &[(NodeId, ArpSummary)],
    known: &BTreeSet<IpAddr>,
) -> Vec<EndpointObservation> {
    let mut by_ip: BTreeMap<IpAddr, EndpointObservation> = BTreeMap::new();
    for (node, summary) in summaries {
        for entry in &summary.entries {
            if known.contains(&entry.ip) {
                continue;
            }
            let candidate = EndpointObservation {
                ip: entry.ip,
                mac: entry.mac.clone(),
                via_node: *node,
                via_ifindex: entry.ifindex,
            };
            by_ip
                .entry(entry.ip)
                .and_modify(|held| {
                    if candidate.via_node.as_uuid() < held.via_node.as_uuid() {
                        *held = candidate.clone();
                    }
                })
                .or_insert(candidate);
        }
    }
    // Truncated after the map is built, so which endpoints survive does not depend on the order the
    // summaries were read in. Ordered by address, which is also the order an operator scans.
    by_ip.into_values().take(MAX_DISCOVERED_ENDPOINTS).collect()
}

/// PostgreSQL-backed store for per-node ARP observations.
pub struct ArpRepo {
    pool: PgPool,
}

impl ArpRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Record one observed summary, replacing whatever was stored.
    ///
    /// Unlike its siblings there is no history append here — see the migration header. `first_seen`
    /// still holds while the content key is unchanged, so "this port has looked like this for three
    /// weeks" remains answerable without a log.
    ///
    /// Only ever called with a summary the poller actually observed. A *failed* walk sends none, so
    /// this is never reached with an empty stand-in — which is what stops one timed-out walk from
    /// ageing every endpoint behind a router out of the table.
    pub async fn record_observation(
        &self,
        node_id: Uuid,
        summary: &ArpSummary,
    ) -> anyhow::Result<()> {
        let key = summary.content_key();
        sqlx::query(
            "INSERT INTO node_arp \
                 (node_id, arp_key, summary, entry_count, truncated, first_seen, last_seen) \
             VALUES ($1, $2, $3, $4, $5, now(), now()) \
             ON CONFLICT (node_id) DO UPDATE SET \
                 arp_key = EXCLUDED.arp_key, \
                 summary = EXCLUDED.summary, \
                 entry_count = EXCLUDED.entry_count, \
                 truncated = EXCLUDED.truncated, \
                 first_seen = CASE WHEN node_arp.arp_key = EXCLUDED.arp_key \
                                   THEN node_arp.first_seen ELSE now() END, \
                 last_seen = now()",
        )
        .bind(node_id)
        .bind(&key)
        .bind(Json(summary))
        .bind(i32::try_from(summary.observed).unwrap_or(i32::MAX))
        .bind(summary.truncated)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Every node's current summary — the endpoint sweep's input.
    ///
    /// Unpaged for the same reason `L3Repo::all_current` is: this is a whole-fleet computation and a
    /// slice of it produces a wrong answer rather than a partial one. Bounded by design — one
    /// document per node, each capped at `MAX_ARP_ENTRIES_PER_NODE`.
    pub async fn all_current(&self) -> anyhow::Result<Vec<(NodeId, ArpSummary)>> {
        let rows = sqlx::query("SELECT node_id, summary FROM node_arp")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                let summary: Json<ArpSummary> = row.try_get("summary")?;
                Ok((NodeId(row.try_get("node_id")?), summary.0))
            })
            .collect()
    }

    /// The newest `last_seen` across every node, or `None` when no ARP walk has ever landed.
    ///
    /// The sweep's whole trigger, and its off switch: a deployment that never enabled ARP discovery
    /// has no rows here, so the sweep returns before it reads the inventory or the address
    /// projection. That is what keeps a feature nobody opted into from costing the leader anything.
    pub async fn observation_watermark(&self) -> anyhow::Result<Option<DateTime<Utc>>> {
        let row = sqlx::query("SELECT max(last_seen) AS w FROM node_arp")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("w")?)
    }

    /// Fleet totals for the coverage line: endpoints observed, nodes reporting, nodes truncated.
    ///
    /// The third number is the one that matters. A truncated walk means the endpoint list is a
    /// sample, and a list presented as complete when it is a sample is the kind of quiet wrongness
    /// this codebase writes caps to avoid.
    pub async fn totals(&self) -> anyhow::Result<(i64, i64, i64)> {
        let row = sqlx::query(
            "SELECT coalesce(sum(entry_count), 0)::BIGINT AS observed, \
                    count(*)::BIGINT AS nodes, \
                    count(*) FILTER (WHERE truncated)::BIGINT AS truncated \
             FROM node_arp",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok((
            row.try_get("observed")?,
            row.try_get("nodes")?,
            row.try_get("truncated")?,
        ))
    }
}

/// PostgreSQL-backed store for discovered endpoints.
pub struct DiscoveredRepo {
    pool: PgPool,
}

impl DiscoveredRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Every address the fleet already monitors: node addresses plus every interface address any
    /// node has reported.
    ///
    /// One query rather than two so the two halves cannot be read at different instants and produce
    /// an endpoint that is "unmonitored" only because a node was created between them. Addresses
    /// that fail to parse are dropped rather than failing the sweep — a malformed row must not stop
    /// discovery for the whole fleet.
    pub async fn known_addresses(&self) -> anyhow::Result<BTreeSet<IpAddr>> {
        let rows = sqlx::query(
            "SELECT address::TEXT AS ip FROM nodes \
             UNION \
             SELECT a->>'ip' AS ip FROM node_l3, \
                    jsonb_array_elements(coalesce(addresses->'addresses', '[]'::jsonb)) a",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| r.try_get::<Option<String>, _>("ip").ok().flatten())
            .filter_map(|s| s.parse::<IpAddr>().ok())
            .collect())
    }

    /// Upsert one sweep's worth of observations, preserving `first_seen`.
    ///
    /// `first_seen` is the only thing here that cannot be recomputed, and it is what answers "how
    /// long was this host on the network before anyone monitored it" — so the upsert never touches
    /// it. Everything else is last-observation-wins.
    pub async fn upsert_batch(&self, rows: &[EndpointObservation]) -> anyhow::Result<u64> {
        if rows.is_empty() {
            return Ok(0);
        }
        let ips: Vec<String> = rows.iter().map(|r| r.ip.to_string()).collect();
        let macs: Vec<Option<String>> = rows.iter().map(|r| r.mac.clone()).collect();
        let vias: Vec<Uuid> = rows.iter().map(|r| r.via_node.as_uuid()).collect();
        let ifs: Vec<i32> = rows
            .iter()
            .map(|r| i32::try_from(r.via_ifindex).unwrap_or(0))
            .collect();
        let res = sqlx::query(
            "INSERT INTO l3_discovered (ip, mac, via_node, via_ifindex, first_seen, last_seen) \
             SELECT u.ip::INET, u.mac, u.via, u.ifidx, now(), now() \
             FROM UNNEST($1::TEXT[], $2::TEXT[], $3::UUID[], $4::INT[]) \
                  AS u(ip, mac, via, ifidx) \
             ON CONFLICT (ip) DO UPDATE SET \
                 mac = EXCLUDED.mac, \
                 via_node = EXCLUDED.via_node, \
                 via_ifindex = EXCLUDED.via_ifindex, \
                 last_seen = now()",
        )
        .bind(&ips)
        .bind(&macs)
        .bind(&vias)
        .bind(&ifs)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Point every row whose address is now an inventory node at that node.
    ///
    /// Called by the sweep **and** by the import handler, which is the point: an endpoint can become
    /// a node either way, and a rule expressed once cannot disagree with itself. Without it an
    /// operator who added the host by hand would keep reading it in the unmonitored list for a week.
    pub async fn reconcile_promotions(&self) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "UPDATE l3_discovered d SET promoted_node_id = n.id \
             FROM nodes n \
             WHERE d.ip = n.address AND d.promoted_node_id IS DISTINCT FROM n.id",
        )
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Drop endpoints not seen inside the retention window, then enforce the fleet ceiling.
    ///
    /// Returns how many rows went. The ceiling deletes the **oldest-seen** rows, so what survives is
    /// what is currently on the network rather than whatever happened to be inserted first.
    pub async fn prune(&self, retention_secs: i64, cap: usize) -> anyhow::Result<u64> {
        let aged = sqlx::query(
            "DELETE FROM l3_discovered WHERE last_seen < now() - make_interval(secs => $1)",
        )
        .bind(retention_secs as f64)
        .execute(&self.pool)
        .await?;
        // `row_number()` rather than OFFSET: OFFSET here would shift under concurrent inserts, and
        // ADR-019's rule against it is about exactly that instability.
        let over = sqlx::query(
            "DELETE FROM l3_discovered d USING ( \
                 SELECT id, row_number() OVER (ORDER BY last_seen DESC, id DESC) AS rn \
                 FROM l3_discovered \
             ) r WHERE d.id = r.id AND r.rn > $1",
        )
        .bind(i64::try_from(cap).unwrap_or(i64::MAX))
        .execute(&self.pool)
        .await?;
        Ok(aged.rows_affected() + over.rows_affected())
    }

    /// A keyset page of endpoints, newest-seen first (ADR-019 — never OFFSET).
    ///
    /// `groups` is the caller's scope: `None` is unrestricted, `Some(&[])` matches nothing. The
    /// predicate joins through `via_node`, so an endpoint seen only by a node the caller cannot see
    /// is not listed — otherwise the list would leak the existence of segments outside the scope.
    ///
    /// A row whose `via_node` has been deleted is visible **only** to an unrestricted caller: there
    /// is no node left to resolve a group from, and guessing either way would be a decision about
    /// somebody else's visibility.
    pub async fn list_page(
        &self,
        groups: Option<&[Uuid]>,
        via_node: Option<Uuid>,
        include_promoted: bool,
        before: Option<(DateTime<Utc>, Uuid)>,
        limit: i64,
    ) -> anyhow::Result<Vec<DiscoveredEndpoint>> {
        // One statement with nullable bind parameters rather than four assembled shapes: the filters
        // are independent, and a builder would put caller-supplied values next to a format string.
        let rows = sqlx::query(
            "SELECT d.id, d.ip::TEXT AS ip, d.mac, d.via_node, d.via_ifindex, \
                    d.first_seen, d.last_seen, d.promoted_node_id \
             FROM l3_discovered d \
             LEFT JOIN nodes n ON n.id = d.via_node \
             WHERE ($1::UUID[] IS NULL OR n.group_id = ANY($1)) \
               AND ($2::UUID IS NULL OR d.via_node = $2) \
               AND ($3::BOOL OR d.promoted_node_id IS NULL) \
               AND ($4::TIMESTAMPTZ IS NULL OR (d.last_seen, d.id) < ($4, $5)) \
             ORDER BY d.last_seen DESC, d.id DESC LIMIT $6",
        )
        .bind(groups.map(<[Uuid]>::to_vec))
        .bind(via_node)
        .bind(include_promoted)
        .bind(before.map(|(at, _)| at))
        .bind(before.map(|(_, id)| id))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                let ip: String = row.try_get("ip")?;
                Ok(DiscoveredEndpoint {
                    id: row.try_get("id")?,
                    // A row whose address will not parse should not fail the page; `0.0.0.0` is
                    // visibly wrong rather than silently absent, and the column is INET so this is
                    // unreachable short of a manual edit.
                    ip: ip
                        .parse()
                        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)),
                    mac: row.try_get("mac")?,
                    via_node: row.try_get::<Option<Uuid>, _>("via_node")?.map(NodeId),
                    via_ifindex: row
                        .try_get::<Option<i32>, _>("via_ifindex")?
                        .and_then(|v| u32::try_from(v).ok()),
                    first_seen: row.try_get("first_seen")?,
                    last_seen: row.try_get("last_seen")?,
                    promoted_node_id: row
                        .try_get::<Option<Uuid>, _>("promoted_node_id")?
                        .map(NodeId),
                })
            })
            .collect()
    }

    /// One endpoint by id — what the import handler reads before creating a node from it.
    pub async fn get(&self, id: Uuid) -> anyhow::Result<Option<DiscoveredEndpoint>> {
        let row = sqlx::query(
            "SELECT id, ip::TEXT AS ip, mac, via_node, via_ifindex, first_seen, last_seen, \
                    promoted_node_id \
             FROM l3_discovered WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let ip: String = row.try_get("ip")?;
        Ok(Some(DiscoveredEndpoint {
            id: row.try_get("id")?,
            ip: ip
                .parse()
                .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)),
            mac: row.try_get("mac")?,
            via_node: row.try_get::<Option<Uuid>, _>("via_node")?.map(NodeId),
            via_ifindex: row
                .try_get::<Option<i32>, _>("via_ifindex")?
                .and_then(|v| u32::try_from(v).ok()),
            first_seen: row.try_get("first_seen")?,
            last_seen: row.try_get("last_seen")?,
            promoted_node_id: row
                .try_get::<Option<Uuid>, _>("promoted_node_id")?
                .map(NodeId),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::ArpEntry;

    /// This module's own source, for the SQL-shape assertions below. The upsert's `first_seen` rule,
    /// the scope predicate and the keyset cursor live entirely inside SQL strings, so nothing else
    /// can catch a rewrite that changes their meaning — the peer stores pin their statements the
    /// same way.
    const SRC: &str = include_str!("arp.rs");

    /// The executable code above this test module, comments stripped — otherwise a doc comment
    /// *naming* a banned pattern reads as the pattern itself (testing.md's self-match trap).
    fn production_source() -> String {
        SRC.split("#[cfg(test)]")
            .next()
            .expect("split always yields a first element")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn node(n: u128) -> NodeId {
        NodeId(Uuid::from_u128(n))
    }
    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn summary(entries: &[(u32, &str)]) -> ArpSummary {
        ArpSummary::new(
            entries
                .iter()
                .map(|(ifidx, addr)| ArpEntry::new(*ifidx, ip(addr)))
                .collect(),
            false,
        )
    }

    #[test]
    fn an_address_the_fleet_already_monitors_is_not_a_discovery() {
        let known: BTreeSet<IpAddr> = [ip("192.168.1.1"), ip("192.168.1.2")].into_iter().collect();
        let found = unmonitored(
            &[(
                node(1),
                summary(&[(8, "192.168.1.1"), (8, "192.168.1.2"), (8, "192.168.1.50")]),
            )],
            &known,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].ip, ip("192.168.1.50"));
        assert_eq!(found[0].via_ifindex, 8);
    }

    #[test]
    fn a_routers_own_lan_address_is_not_reported_as_unmonitored() {
        // The false positive that would make the list unreadable on day one. The router is monitored
        // on 10.0.0.1 and answers ARP for its LAN interface 192.168.1.1 — which is in `node_l3`, not
        // in `nodes.address`. Feeding only node addresses here would report every router's every
        // interface as a discovery.
        let node_addresses: BTreeSet<IpAddr> = [ip("10.0.0.1")].into_iter().collect();
        let with_l3: BTreeSet<IpAddr> = [ip("10.0.0.1"), ip("192.168.1.1")].into_iter().collect();
        let obs = [(node(1), summary(&[(8, "192.168.1.1")]))];

        assert_eq!(
            unmonitored(&obs, &node_addresses).len(),
            1,
            "this is the wrong answer, and the reason `known` must include node_l3"
        );
        assert!(unmonitored(&obs, &with_l3).is_empty());
    }

    #[test]
    fn one_endpoint_seen_by_three_routers_is_one_finding() {
        // Redundancy in the network must not inflate the count the feature exists to produce.
        let obs = [
            (node(3), summary(&[(1, "192.168.1.50")])),
            (node(1), summary(&[(2, "192.168.1.50")])),
            (node(2), summary(&[(3, "192.168.1.50")])),
        ];
        let found = unmonitored(&obs, &BTreeSet::new());
        assert_eq!(found.len(), 1);
        // Lowest node id wins, so attribution does not flip between sweeps and make the endpoint
        // look like it is moving around the network.
        assert_eq!(found[0].via_node, node(1));
        assert_eq!(found[0].via_ifindex, 2);
    }

    #[test]
    fn the_sweep_is_order_independent_and_bounded() {
        let mut obs: Vec<(NodeId, ArpSummary)> = (0..40u128)
            .map(|i| {
                let entries: Vec<ArpEntry> = (0..400u32)
                    .map(|j| {
                        #[allow(clippy::cast_possible_truncation)]
                        let addr = IpAddr::V4(std::net::Ipv4Addr::from(
                            (10u32 << 24) + (i as u32) * 400 + j + 1,
                        ));
                        ArpEntry::new(1, addr)
                    })
                    .collect();
                (node(i + 1), ArpSummary::new(entries, false))
            })
            .collect();
        let forward = unmonitored(&obs, &BTreeSet::new());
        obs.reverse();
        let reversed = unmonitored(&obs, &BTreeSet::new());
        assert_eq!(
            forward.len(),
            MAX_DISCOVERED_ENDPOINTS,
            "40 nodes × 400 endpoints must be capped, not stored"
        );
        assert_eq!(
            forward, reversed,
            "which endpoints survive the cap must not depend on read order"
        );
    }

    #[test]
    fn an_endpoint_with_no_mac_still_counts() {
        // An incomplete ARP entry names a host that replied to something. Requiring a MAC would drop
        // exactly the hosts that are hardest to reach — which are the interesting ones.
        let mut entry = ArpEntry::new(4, ip("10.9.9.9"));
        entry.mac = None;
        let found = unmonitored(
            &[(node(1), ArpSummary::new(vec![entry], false))],
            &BTreeSet::new(),
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].mac, None);
    }

    #[test]
    fn first_seen_survives_an_endpoint_being_seen_again() {
        // The column's only purpose is "how long has this host been on the network unmonitored";
        // touching it on every sweep would reset that to five minutes, forever.
        assert!(SRC.contains("last_seen = now()"));
        assert!(
            !production_source().contains("first_seen = now()"),
            "the upsert must never move first_seen"
        );
    }

    #[test]
    fn the_scope_predicate_filters_on_the_observing_nodes_group() {
        // Security-critical: without it a scoped operator reads the addresses of segments they
        // cannot see. The NULL branch is the unrestricted fast path, not a missing filter.
        assert!(SRC.contains("($1::UUID[] IS NULL OR n.group_id = ANY($1))"));
    }

    #[test]
    fn paging_is_keyset_and_never_offset() {
        assert!(SRC.contains("(d.last_seen, d.id) < ($4, $5)"));
        assert!(SRC.contains("ORDER BY d.last_seen DESC, d.id DESC LIMIT"));
        assert!(
            !production_source().contains("OFFSET"),
            "OFFSET paging — rows shift under the reader as the sweep updates last_seen"
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

    #[test]
    fn the_stored_key_is_the_models_own_content_key() {
        assert!(SRC.contains("summary.content_key()"));
    }

    #[test]
    fn the_sweep_has_an_observation_watermark_to_trigger_on() {
        // Also its off switch: no ARP rows ⇒ no watermark ⇒ the sweep returns before it reads the
        // inventory. A deployment that never opted in must not pay for the feature.
        assert!(SRC.contains("SELECT max(last_seen) AS w FROM node_arp"));
    }

    #[test]
    fn the_retention_window_is_short_and_the_cap_is_finite() {
        // Tripwires, not tautologies. Lengthening the window turns a review list into a second
        // inventory of every laptop that ever joined the wifi.
        assert_eq!(DISCOVERED_RETENTION_SECS, 7 * 86_400);
        const { assert!(MAX_DISCOVERED_ENDPOINTS <= 10_000) };
        // And the ARP cadence stays in the band the API edge enforces for the other two walks.
        assert!(crate::neighbors::interval_in_bounds(
            DEFAULT_ARP_INTERVAL_SECS
        ));
        const { assert!(DEFAULT_ARP_INTERVAL_SECS > crate::neighbors::DEFAULT_NEIGHBOR_INTERVAL_SECS) };
    }
}
