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
    ///
    /// 🚨 **`host(address)`, never `address::TEXT`.** An explicit cast to text renders an `inet`
    /// *with* its masklen (`10.0.0.1/32`, `2001:db8::1/128`) even though the default display omits
    /// it, and `IpAddr::from_str` rejects that — so the drop-on-parse-failure above silently threw
    /// away **every** node address, and every monitored node was reported as an unmonitored
    /// endpoint. `host()` is documented to return the address alone.
    pub async fn known_addresses(&self) -> anyhow::Result<BTreeSet<IpAddr>> {
        let rows = sqlx::query(
            "SELECT host(address) AS ip FROM nodes \
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
            "SELECT d.id, host(d.ip) AS ip, d.mac, d.via_node, d.via_ifindex, \
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
                    //
                    // 🚨 It was reachable on **every** row until the projection above became
                    // `host(ip)`: `ip::TEXT` renders an `inet` with its masklen, which does not
                    // parse, so the whole list read `0.0.0.0`. See `known_addresses`.
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
            "SELECT id, host(ip) AS ip, mac, via_node, via_ifindex, first_seen, last_seen, \
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

    /// This module's code, with its test items and comments dropped — the reader every
    /// SQL-shape assertion below uses. The upsert's `first_seen` rule, the scope predicate and
    /// the keyset cursor live entirely inside SQL strings, so nothing else can catch a rewrite
    /// that changes their meaning; the peer stores pin their statements the same way.
    ///
    /// ⚠️ **Read through `module_source`, never `include_str!`** (ADR-102). The raw file includes
    /// this test module, so a positive `contains("<literal>")` was satisfied by the needle's own
    /// line and could not fail. Thirty-two of those were live across seven modules — all of them
    /// here, because the negated side already read this function and only the positive side was
    /// left on the raw text. Loud on one side and silent on the other is why they survived
    /// ADR-091's sweep.
    fn production_source() -> String {
        crate::module_source::code_no_comments("src", "arp")
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
        assert!(production_source().contains("last_seen = now()"));
        assert!(
            !production_source().contains("first_seen = now()"),
            "the upsert must never move first_seen"
        );
    }

    #[test]
    fn the_scope_predicate_filters_on_the_observing_nodes_group() {
        // Security-critical: without it a scoped operator reads the addresses of segments they
        // cannot see. The NULL branch is the unrestricted fast path, not a missing filter.
        assert!(production_source().contains("($1::UUID[] IS NULL OR n.group_id = ANY($1))"));
    }

    #[test]
    fn paging_is_keyset_and_never_offset() {
        assert!(production_source().contains("(d.last_seen, d.id) < ($4, $5)"));
        assert!(production_source().contains("ORDER BY d.last_seen DESC, d.id DESC LIMIT"));
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
        assert!(production_source().contains("summary.content_key()"));
    }

    #[test]
    fn the_sweep_has_an_observation_watermark_to_trigger_on() {
        // Also its off switch: no ARP rows ⇒ no watermark ⇒ the sweep returns before it reads the
        // inventory. A deployment that never opted in must not pay for the feature.
        assert!(production_source().contains("SELECT max(last_seen) AS w FROM node_arp"));
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

    /// **An address leaves PostgreSQL through `host()`, never through a cast to text.**
    ///
    /// `inet`'s text output carries the masklen (`10.0.0.1/32`) while its default display does not,
    /// so `address::TEXT` looks right in psql and hands Rust a string `IpAddr::from_str` rejects.
    /// All three statements here had it, and all three failed silently: node addresses vanished
    /// from the known set, and every listed endpoint read `0.0.0.0`.
    ///
    /// The database tests below are the stronger check — this one exists to name the trap in the
    /// place someone would reintroduce it, and to fail rather than merely be un-read.
    #[test]
    fn an_address_is_read_through_host_and_never_cast_to_text() {
        let src = production_source();
        assert_eq!(
            src.matches("host(").count(),
            3,
            "the three address projections are no longer reading through `host()`"
        );
        for needle in ["address::TEXT", "ip::TEXT"] {
            assert!(
                !src.contains(needle),
                "{needle} renders an inet with its masklen, which does not parse as an address"
            );
        }
    }

    // --- Running the SQL, not reading it (ADR-114/116) -----------------------------------------
    //
    // Everything above is either the pure rule (`unmonitored`) or a reading of this module's text.
    // Neither can say whether the eleven statements do what the words claim, and one of the two
    // stores here is the only one in ADR-043 whose reader joins to a *second* table — which is
    // exactly where a scope predicate goes wrong quietly.
    use crate::l3::L3Repo;
    use crate::pgtest;
    use yagra_common::{L3Address, L3Snapshot};

    /// A node with an address of the test's choosing, through the production writer.
    async fn node_at(pool: &sqlx::PgPool, name: &str, addr: &str) -> Uuid {
        pgtest::repo(pool.clone())
            .create_node(name, ip(addr), None, None, None, None, None, None)
            .await
            .expect("create node")
    }

    fn observation(addr: &str, via: Uuid, ifindex: u32) -> EndpointObservation {
        EndpointObservation {
            ip: ip(addr),
            mac: Some("aa:bb:cc:dd:ee:ff".to_owned()),
            via_node: NodeId(via),
            via_ifindex: ifindex,
        }
    }

    /// A summary goes in whole, comes back whole, and the coverage line counts it — including the
    /// truncation flag, which is what says the endpoint list is a sample rather than the network.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_observed_summary_reads_back_and_the_totals_count_it(pool: sqlx::PgPool) {
        let repo = ArpRepo::new(pool.clone());
        assert!(
            repo.observation_watermark()
                .await
                .expect("watermark")
                .is_none(),
            "a fresh database reported an ARP walk that never happened"
        );
        assert_eq!(repo.totals().await.expect("totals"), (0, 0, 0));

        let a = pgtest::node(&pool, "a", 1, None).await;
        let b = pgtest::node(&pool, "b", 2, None).await;
        let seen_by_a = summary(&[(8, "192.168.50.10"), (8, "192.168.50.11")]);
        let seen_by_b = ArpSummary::new(vec![ArpEntry::new(9, ip("192.168.50.12"))], true);
        repo.record_observation(a, &seen_by_a).await.expect("a");
        repo.record_observation(b, &seen_by_b).await.expect("b");

        let all = repo.all_current().await.expect("all_current");
        assert_eq!(all.len(), 2, "the unpaged read did not return every node");
        assert_eq!(
            all.iter().find(|(id, _)| id.0 == a).map(|(_, s)| s),
            Some(&seen_by_a),
            "a summary came back attached to the wrong node, or not at all"
        );
        assert_eq!(
            all.iter().find(|(id, _)| id.0 == b).map(|(_, s)| s),
            Some(&seen_by_b)
        );

        assert_eq!(
            repo.totals().await.expect("totals"),
            (3, 2, 1),
            "the coverage line does not agree with what was stored (observed, nodes, truncated)"
        );
        assert_eq!(
            repo.observation_watermark().await.expect("watermark"),
            Some(pgtest::node_timestamp(&pool, "node_arp", "last_seen", b).await),
            "the watermark is not the newest last_seen in the table"
        );
    }

    /// `first_seen` answers "this port has looked like this for three weeks", so an unchanged walk
    /// must not restart it and a changed one must.
    ///
    /// ⚠️ Read through [`pgtest::node_timestamp`] because `node_arp.first_seen` has **no reader in
    /// production** — the column is written by this statement and consulted by nothing else, so
    /// without the fixture the rule is only assertable as text.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_unchanged_walk_keeps_first_seen_and_a_changed_one_restarts_it(pool: sqlx::PgPool) {
        let node = pgtest::node(&pool, "rtr", 1, None).await;
        let repo = ArpRepo::new(pool.clone());
        let held = summary(&[(8, "192.168.50.10")]);
        repo.record_observation(node, &held).await.expect("first");
        let started = pgtest::node_timestamp(&pool, "node_arp", "first_seen", node).await;

        repo.record_observation(node, &held).await.expect("same");
        assert_eq!(
            pgtest::rows(&pool, "node_arp").await,
            1,
            "the summary was stored twice — the conflict target is not the node"
        );
        assert_eq!(
            pgtest::node_timestamp(&pool, "node_arp", "first_seen", node).await,
            started,
            "an unchanged walk restarted the clock, so every endpoint looks new every six hours"
        );
        assert!(
            pgtest::node_timestamp(&pool, "node_arp", "last_seen", node).await > started,
            "last_seen did not move, so the sweep would never trigger again"
        );

        repo.record_observation(node, &summary(&[(8, "192.168.50.99")]))
            .await
            .expect("changed");
        assert!(
            pgtest::node_timestamp(&pool, "node_arp", "first_seen", node).await > started,
            "first_seen did not restart when the walk actually saw something else"
        );
    }

    /// **The false positive the sweep exists to avoid.** A router monitored on its management
    /// address answers ARP for its LAN interface too, so the known set has to carry every reported
    /// interface address as well as every node address — otherwise the router's own gateway
    /// address is reported as an unmonitored endpoint on every segment it terminates.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn known_addresses_covers_node_addresses_and_reported_interface_addresses(
        pool: sqlx::PgPool,
    ) {
        let rtr = node_at(&pool, "rtr", "10.20.0.1").await;
        L3Repo::new(pool.clone())
            .record_observation(
                rtr,
                &L3Snapshot::new(vec![L3Address::new(8, ip("192.168.50.1"), 24)]),
            )
            .await
            .expect("record l3");

        let known = DiscoveredRepo::new(pool.clone())
            .known_addresses()
            .await
            .expect("known");
        assert!(
            known.contains(&ip("10.20.0.1")),
            "the node's own address is not in the known set: {known:?}"
        );
        assert!(
            known.contains(&ip("192.168.50.1")),
            "a reported interface address is not in the known set, so the router's own gateway \
             address would be reported as an unmonitored endpoint: {known:?}"
        );
        assert!(
            !known.contains(&ip("192.168.50.77")),
            "an address nobody reported is in the known set, which would hide real endpoints"
        );
    }

    /// An endpoint seen again keeps `first_seen` — how long it was on the network before anyone
    /// monitored it — and takes the newer observation for everything else.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_endpoint_seen_again_keeps_first_seen_and_takes_the_new_observation(
        pool: sqlx::PgPool,
    ) {
        let a = pgtest::node(&pool, "a", 1, None).await;
        let b = pgtest::node(&pool, "b", 2, None).await;
        let repo = DiscoveredRepo::new(pool.clone());

        assert_eq!(
            repo.upsert_batch(&[]).await.expect("empty"),
            0,
            "an empty sweep wrote something"
        );
        assert_eq!(
            repo.upsert_batch(&[observation("192.168.50.10", a, 8)])
                .await
                .expect("upsert"),
            1
        );
        let first = repo
            .list_page(None, None, false, None, 10)
            .await
            .expect("list")
            .remove(0);
        assert_eq!(first.ip, ip("192.168.50.10"));
        assert_eq!(first.via_node, Some(NodeId(a)));
        assert_eq!(first.via_ifindex, Some(8));
        assert_eq!(first.mac.as_deref(), Some("aa:bb:cc:dd:ee:ff"));
        assert_eq!(first.promoted_node_id, None);
        assert_eq!(
            first.first_seen, first.last_seen,
            "an endpoint seen once already looks re-observed"
        );

        let mut again = observation("192.168.50.10", b, 3);
        again.mac = None;
        repo.upsert_batch(&[again]).await.expect("re-upsert");
        assert_eq!(
            pgtest::rows(&pool, "l3_discovered").await,
            1,
            "the same endpoint stored twice — the identity is not the address"
        );
        let second = repo
            .list_page(None, None, false, None, 10)
            .await
            .expect("list")
            .remove(0);
        assert_eq!(second.id, first.id);
        assert_eq!(
            second.first_seen, first.first_seen,
            "first_seen moved, so 'how long was this here unmonitored' is now wrong"
        );
        assert!(second.last_seen > first.last_seen);
        assert_eq!(second.via_node, Some(NodeId(b)));
        assert_eq!(second.via_ifindex, Some(3));
        assert_eq!(second.mac, None, "the conflict path did not update the row");
    }

    /// An endpoint that becomes a node stops being a finding — and asking twice changes nothing,
    /// which is what the `IS DISTINCT FROM` guard is for.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn promotion_points_the_row_at_the_node_that_now_monitors_it(pool: sqlx::PgPool) {
        let via = pgtest::node(&pool, "rtr", 1, None).await;
        let repo = DiscoveredRepo::new(pool.clone());
        repo.upsert_batch(&[
            observation("192.168.50.10", via, 8),
            observation("192.168.50.11", via, 8),
        ])
        .await
        .expect("upsert");
        assert_eq!(
            repo.reconcile_promotions().await.expect("reconcile"),
            0,
            "an endpoint nobody monitors was reported as promoted"
        );

        let host = node_at(&pool, "host", "192.168.50.10").await;
        assert_eq!(repo.reconcile_promotions().await.expect("reconcile"), 1);
        assert_eq!(
            repo.reconcile_promotions().await.expect("reconcile"),
            0,
            "reconciling twice rewrote a row that was already correct"
        );

        let listed = repo
            .list_page(None, None, false, None, 10)
            .await
            .expect("list");
        assert_eq!(
            listed.len(),
            1,
            "a promoted endpoint is still in the unmonitored list"
        );
        assert_eq!(listed[0].ip, ip("192.168.50.11"));

        let with_promoted = repo
            .list_page(None, None, true, None, 10)
            .await
            .expect("list");
        assert_eq!(
            with_promoted.len(),
            2,
            "asking for promoted rows did not bring the promoted one back"
        );
        let promoted = with_promoted
            .iter()
            .find(|e| e.ip == ip("192.168.50.10"))
            .expect("the promoted endpoint");
        assert_eq!(promoted.promoted_node_id, Some(NodeId(host)));

        let fetched = repo
            .get(promoted.id)
            .await
            .expect("get")
            .expect("the row just listed");
        assert_eq!(&fetched, promoted, "get and list disagree about one row");
        assert!(
            repo.get(Uuid::new_v4()).await.expect("get").is_none(),
            "an unknown id returned an endpoint"
        );
    }

    /// Pruning drops what aged out, then enforces the ceiling by dropping the **oldest seen** —
    /// so what survives is what is currently on the network.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn pruning_ages_rows_out_then_enforces_the_ceiling_oldest_first(pool: sqlx::PgPool) {
        let via = pgtest::node(&pool, "rtr", 1, None).await;
        let repo = DiscoveredRepo::new(pool.clone());
        // One call each, so `last_seen` genuinely orders them; a single batch stamps one `now()`
        // and the ceiling would then be deciding on the surrogate id alone.
        for addr in ["192.168.50.10", "192.168.50.11", "192.168.50.12"] {
            repo.upsert_batch(&[observation(addr, via, 8)])
                .await
                .expect("upsert");
        }

        assert_eq!(
            repo.prune(3600, 10).await.expect("prune"),
            0,
            "rows seen a moment ago were pruned by an hour-long window under a ceiling of ten"
        );
        assert_eq!(pgtest::rows(&pool, "l3_discovered").await, 3);

        assert_eq!(
            repo.prune(3600, 1).await.expect("prune"),
            2,
            "the ceiling did not take the two rows over it"
        );
        let left = repo
            .list_page(None, None, false, None, 10)
            .await
            .expect("list");
        assert_eq!(left.len(), 1);
        assert_eq!(
            left[0].ip,
            ip("192.168.50.12"),
            "the ceiling kept the oldest-seen row instead of the newest"
        );

        assert_eq!(repo.prune(0, 10).await.expect("prune"), 1);
        assert_eq!(pgtest::rows(&pool, "l3_discovered").await, 0);
    }

    /// **The scope rule, executed**, plus the two filters and the cursor that share its statement.
    ///
    /// The predicate joins through `via_node`, so an endpoint seen only by a node the caller cannot
    /// see must not be listed — otherwise the list leaks the existence of segments outside the
    /// scope.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_listing_is_scoped_by_the_observing_node_and_pages_by_cursor(pool: sqlx::PgPool) {
        let mine = pgtest::group(&pool, "mine").await;
        let theirs = pgtest::group(&pool, "theirs").await;
        let ours = pgtest::node(&pool, "ours", 1, Some(mine)).await;
        let alien = pgtest::node(&pool, "alien", 2, Some(theirs)).await;
        let repo = DiscoveredRepo::new(pool.clone());
        for (addr, via) in [
            ("192.168.50.10", ours),
            ("192.168.50.11", ours),
            ("192.168.60.10", alien),
        ] {
            repo.upsert_batch(&[observation(addr, via, 8)])
                .await
                .expect("upsert");
        }

        // Acceptance first: a predicate that refuses everything reads exactly like one that works.
        assert_eq!(
            repo.list_page(None, None, false, None, 10)
                .await
                .expect("list")
                .len(),
            3,
            "an unrestricted caller must see every endpoint"
        );

        let scoped = repo
            .list_page(Some(&[mine]), None, false, None, 10)
            .await
            .expect("list");
        assert_eq!(
            scoped.len(),
            2,
            "the scope did not filter on the observing node's group"
        );
        assert!(
            scoped.iter().all(|e| e.via_node == Some(NodeId(ours))),
            "an endpoint seen only outside the scope was listed"
        );
        assert!(
            repo.list_page(Some(&[]), None, false, None, 10)
                .await
                .expect("list")
                .is_empty(),
            "an empty scope matched something"
        );

        let by_observer = repo
            .list_page(None, Some(alien), false, None, 10)
            .await
            .expect("list");
        assert_eq!(by_observer.len(), 1);
        assert_eq!(by_observer[0].ip, ip("192.168.60.10"));

        // ⚠️ The bounded loop is part of the assertion: a cursor that stopped being applied would
        // hand back the same newest row forever, and an unbounded `loop` would hang, not fail.
        let mut seen: Vec<(DateTime<Utc>, Uuid)> = Vec::new();
        let mut before: Option<(DateTime<Utc>, Uuid)> = None;
        for _ in 0..8 {
            let page = repo
                .list_page(None, None, false, before, 1)
                .await
                .expect("list");
            let Some(row) = page.first() else { break };
            assert_eq!(page.len(), 1, "LIMIT is not being applied");
            seen.push((row.last_seen, row.id));
            before = Some((row.last_seen, row.id));
        }
        assert_eq!(
            seen.len(),
            3,
            "the cursor walk did not end after the three endpoints: {seen:?}"
        );
        let mut descending = seen.clone();
        descending.sort_unstable();
        descending.reverse();
        descending.dedup();
        assert_eq!(
            descending, seen,
            "the page did not come back newest-seen first, or a row came back twice: {seen:?}"
        );
    }
}
