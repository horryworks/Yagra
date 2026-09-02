// SPDX-License-Identifier: AGPL-3.0-only
//! The derived connectivity graph's cache (ADR-043, migration 0066).
//!
//! Everything here is recomputable from `node_l3` and `node_neighbors`, so this is a materialized
//! view maintained by the leader's derivation task rather than a source of truth. What it buys is
//! that a map request does not re-derive the whole fleet, and that `first_seen` — "how long has
//! this link been here" — survives across runs.
//!
//! The one rule that is easy to get wrong: **links are removed by age, never wholesale**. The
//! observations feeding a derivation arrive per node at different times, so a rebuild-from-empty on
//! a cycle where one node's document happened to be missing would delete every link through that
//! node and re-add it a cycle later. That is migration 0062's "everything disappeared" failure,
//! one level up.

use chrono::{DateTime, Utc};
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_common::{DerivedLink, LinkSource, NodeId, TopologyLinkSummary};

/// How many derivation cycles a link may go unobserved before it is deleted.
///
/// Three rather than one: a single missed cycle is an ordinary transient (a device that timed out,
/// a poller that restarted), and deleting on the first miss would make the map flicker in step with
/// the network's noise rather than its topology.
pub const STALE_CYCLES: i64 = 3;

/// One stored link, as read back for the API.
#[derive(Debug, Clone)]
pub struct StoredLink {
    /// Surrogate id — the keyset cursor.
    pub id: i64,
    /// The lower-ordered endpoint. `None` is reserved for Increment 3's non-inventory endpoints;
    /// Increment 1 never writes one.
    pub a_node: Option<NodeId>,
    /// The higher-ordered endpoint.
    pub b_node: Option<NodeId>,
    pub a_ifindex: Option<i32>,
    pub b_ifindex: Option<i32>,
    pub a_if_name: Option<String>,
    pub b_if_name: Option<String>,
    /// Every source that produced this link, strongest first. Unknown tokens — a row written by a
    /// newer core — are dropped rather than failing the read.
    pub sources: Vec<LinkSource>,
    /// The subnet behind an `l3_subnet` edge.
    pub subnet: Option<String>,
    /// The endpoint an operator declared upstream, if any (ADR-043 I2).
    pub forced_parent: Option<NodeId>,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

/// What the last derivation run saw.
#[derive(Debug, Clone)]
pub struct DerivationState {
    /// When the run finished.
    pub derived_at: DateTime<Utc>,
    /// Counters for everything the run declined to turn into a link.
    pub summary: TopologyLinkSummary,
    /// How many links it produced.
    pub link_count: i64,
}

/// PostgreSQL-backed cache of the derived graph.
pub struct TopoLinkRepo {
    pool: PgPool,
}

impl TopoLinkRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Upsert every derived link in one statement, refreshing `last_seen`.
    ///
    /// `first_seen` is deliberately **not** touched on conflict: it is the only column here that
    /// cannot be recomputed, and resetting it on every cycle would make every link look new.
    ///
    /// `sources` travels as one comma-joined string per row and is split server-side. A `text[][]`
    /// bind would require every row to carry the same number of sources, which they do not.
    /// `LinkSource`'s tokens contain no comma, so the join is unambiguous.
    pub async fn upsert_batch(&self, links: &[DerivedLink]) -> anyhow::Result<u64> {
        if links.is_empty() {
            return Ok(0);
        }
        let mut keys: Vec<String> = Vec::with_capacity(links.len());
        let mut a_nodes: Vec<Option<Uuid>> = Vec::with_capacity(links.len());
        let mut b_nodes: Vec<Option<Uuid>> = Vec::with_capacity(links.len());
        let mut a_ifs: Vec<Option<i32>> = Vec::with_capacity(links.len());
        let mut b_ifs: Vec<Option<i32>> = Vec::with_capacity(links.len());
        let mut a_names: Vec<Option<String>> = Vec::with_capacity(links.len());
        let mut b_names: Vec<Option<String>> = Vec::with_capacity(links.len());
        let mut sources: Vec<String> = Vec::with_capacity(links.len());
        let mut subnets: Vec<Option<String>> = Vec::with_capacity(links.len());
        let mut forced: Vec<Option<Uuid>> = Vec::with_capacity(links.len());
        for l in links {
            keys.push(l.link_key());
            a_nodes.push(Some(l.a_node.as_uuid()));
            b_nodes.push(Some(l.b_node.as_uuid()));
            a_ifs.push(l.a_ifindex.and_then(|v| i32::try_from(v).ok()));
            b_ifs.push(l.b_ifindex.and_then(|v| i32::try_from(v).ok()));
            a_names.push(l.a_if_name.clone());
            b_names.push(l.b_if_name.clone());
            sources.push(
                l.sources
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            );
            subnets.push(l.subnet.clone());
            forced.push(l.forced_parent.map(|n| n.as_uuid()));
        }

        let res = sqlx::query(
            "INSERT INTO node_links \
                (link_key, a_node, b_node, a_if, b_if, a_if_name, b_if_name, sources, subnet, \
                 forced_parent, first_seen, last_seen) \
             SELECT t.link_key, t.a_node, t.b_node, t.a_if, t.b_if, t.a_if_name, t.b_if_name, \
                    string_to_array(t.sources_csv, ','), t.subnet, t.forced_parent, now(), now() \
             FROM UNNEST($1::text[], $2::uuid[], $3::uuid[], $4::int[], $5::int[], \
                         $6::text[], $7::text[], $8::text[], $9::text[], $10::uuid[]) \
                  AS t(link_key, a_node, b_node, a_if, b_if, a_if_name, b_if_name, \
                       sources_csv, subnet, forced_parent) \
             ON CONFLICT (link_key) DO UPDATE SET \
                 a_node = EXCLUDED.a_node, \
                 b_node = EXCLUDED.b_node, \
                 a_if = EXCLUDED.a_if, \
                 b_if = EXCLUDED.b_if, \
                 a_if_name = EXCLUDED.a_if_name, \
                 b_if_name = EXCLUDED.b_if_name, \
                 sources = EXCLUDED.sources, \
                 subnet = EXCLUDED.subnet, \
                 forced_parent = EXCLUDED.forced_parent, \
                 last_seen = now()",
        )
        .bind(&keys)
        .bind(&a_nodes)
        .bind(&b_nodes)
        .bind(&a_ifs)
        .bind(&b_ifs)
        .bind(&a_names)
        .bind(&b_names)
        .bind(&sources)
        .bind(&subnets)
        .bind(&forced)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Delete links that have gone unobserved for [`STALE_CYCLES`] derivation cycles.
    ///
    /// Age-gated per row rather than a wholesale rebuild — see the module doc for why that
    /// distinction is the whole safety property.
    pub async fn prune_stale(&self, cycle_secs: i64) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "DELETE FROM node_links WHERE last_seen < now() - make_interval(secs => $1)",
        )
        .bind((cycle_secs * STALE_CYCLES) as f64)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Record what the run saw. One row, replaced each cycle.
    pub async fn record_run(
        &self,
        summary: &TopologyLinkSummary,
        link_count: usize,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO topology_derivation (id, derived_at, summary, link_count) \
             VALUES (TRUE, now(), $1, $2) \
             ON CONFLICT (id) DO UPDATE SET derived_at = now(), summary = $1, link_count = $2",
        )
        .bind(Json(summary))
        .bind(i32::try_from(link_count).unwrap_or(i32::MAX))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// What the last derivation run saw, or `None` before the first run.
    pub async fn last_run(&self) -> anyhow::Result<Option<DerivationState>> {
        let row = sqlx::query(
            "SELECT derived_at, summary, link_count FROM topology_derivation WHERE id = TRUE",
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let summary: Json<TopologyLinkSummary> = row.try_get("summary")?;
        Ok(Some(DerivationState {
            derived_at: row.try_get("derived_at")?,
            summary: summary.0,
            link_count: i64::from(row.try_get::<i32, _>("link_count")?),
        }))
    }

    /// Every link with both endpoints in the inventory, for the projection (ADR-043 I2).
    ///
    /// **Unscoped, and it must stay that way.** This feeds the alert engine's dependency graph, not
    /// a response: a graph filtered to one operator's groups would attribute a root cause to the
    /// nearest *visible* node rather than the actual one, so suppression would differ per viewer.
    /// The scoped read is [`Self::list_page`]; these two callers want opposite things and the API
    /// never reaches this one.
    ///
    /// Rows whose endpoints are NULL are skipped — Increment 3's non-inventory endpoints are map
    /// vertices, never dependency vertices, because a node with no state cannot be up or down.
    pub async fn all_links(&self) -> anyhow::Result<Vec<DerivedLink>> {
        let rows = sqlx::query(
            "SELECT a_node, b_node, sources, subnet, forced_parent FROM node_links \
             WHERE a_node IS NOT NULL AND b_node IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let (Some(a), Some(b)) = (
                row.try_get::<Option<Uuid>, _>("a_node")?,
                row.try_get::<Option<Uuid>, _>("b_node")?,
            ) else {
                continue;
            };
            let tokens: Vec<String> = row.try_get("sources")?;
            let mut link = DerivedLink::new(NodeId(a), NodeId(b), LinkSource::L3Subnet);
            link.sources = tokens
                .iter()
                .filter_map(|t| LinkSource::from_token(t))
                .collect();
            link.sources.sort_unstable();
            link.subnet = row.try_get("subnet")?;
            link.forced_parent = row.try_get::<Option<Uuid>, _>("forced_parent")?.map(NodeId);
            out.push(link);
        }
        Ok(out)
    }

    /// A keyset page of links, ordered by id (ADR-019 — never OFFSET).
    ///
    /// ⚠️ **Both endpoints must be visible to the caller.** `groups` is the group-scope filter (the
    /// same value `NodeRepo::SCOPE_PREDICATE` binds); a link is returned only when *both* of its
    /// ends sit in a visible group. Returning a link with one visible end would tell a scoped
    /// operator that a node exists outside their scope — the fail-open direction the ledger's scope
    /// column exists to prevent. `None` means an unscoped caller and applies no restriction.
    pub async fn list_page(
        &self,
        groups: Option<&[Uuid]>,
        after: Option<i64>,
        limit: i64,
    ) -> anyhow::Result<Vec<StoredLink>> {
        // Two prepared shapes, each written out in full rather than assembled with `format!`: the
        // cursor and the scope are bound, never interpolated. The scope predicate is a no-op when
        // the bound array is NULL, which is what lets one statement serve both callers.
        let bind = groups.map(<[Uuid]>::to_vec);
        let rows = match after {
            None => {
                sqlx::query(
                    "SELECT id, a_node, b_node, a_if, b_if, a_if_name, b_if_name, sources, \
                            subnet, forced_parent, first_seen, last_seen \
                     FROM node_links \
                     WHERE ($1::uuid[] IS NULL OR ( \
                             EXISTS (SELECT 1 FROM nodes n WHERE n.id = node_links.a_node \
                                     AND n.group_id = ANY($1)) \
                         AND EXISTS (SELECT 1 FROM nodes n WHERE n.id = node_links.b_node \
                                     AND n.group_id = ANY($1)))) \
                     ORDER BY id LIMIT $2",
                )
                .bind(&bind)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            Some(cursor) => {
                sqlx::query(
                    "SELECT id, a_node, b_node, a_if, b_if, a_if_name, b_if_name, sources, \
                            subnet, forced_parent, first_seen, last_seen \
                     FROM node_links \
                     WHERE ($1::uuid[] IS NULL OR ( \
                             EXISTS (SELECT 1 FROM nodes n WHERE n.id = node_links.a_node \
                                     AND n.group_id = ANY($1)) \
                         AND EXISTS (SELECT 1 FROM nodes n WHERE n.id = node_links.b_node \
                                     AND n.group_id = ANY($1)))) \
                       AND id > $2 \
                     ORDER BY id LIMIT $3",
                )
                .bind(&bind)
                .bind(cursor)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };

        rows.into_iter()
            .map(|row| {
                let tokens: Vec<String> = row.try_get("sources")?;
                let mut sources: Vec<LinkSource> = tokens
                    .iter()
                    .filter_map(|t| LinkSource::from_token(t))
                    .collect();
                sources.sort_unstable();
                Ok(StoredLink {
                    id: row.try_get("id")?,
                    a_node: row.try_get::<Option<Uuid>, _>("a_node")?.map(NodeId),
                    b_node: row.try_get::<Option<Uuid>, _>("b_node")?.map(NodeId),
                    a_ifindex: row.try_get("a_if")?,
                    b_ifindex: row.try_get("b_if")?,
                    a_if_name: row.try_get("a_if_name")?,
                    b_if_name: row.try_get("b_if_name")?,
                    sources,
                    subnet: row.try_get("subnet")?,
                    forced_parent: row.try_get::<Option<Uuid>, _>("forced_parent")?.map(NodeId),
                    first_seen: row.try_get("first_seen")?,
                    last_seen: row.try_get("last_seen")?,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    /// This module's code, with its test items and comments dropped — the reader every
    /// SQL-shape assertion below uses. The scope rule and the never-wholesale-delete rule live
    /// entirely inside SQL strings, so nothing else can catch a rewrite that changes their
    /// meaning.
    ///
    /// ⚠️ **Read through `module_source`, never `include_str!`** (ADR-102). The raw file includes
    /// this test module, so a positive `contains("<literal>")` was satisfied by the needle's own
    /// line and could not fail. Thirty-two of those were live across seven modules — all of them
    /// here, because the negated side already read this function and only the positive side was
    /// left on the raw text. Loud on one side and silent on the other is why they survived
    /// ADR-091's sweep.
    fn production_source() -> String {
        crate::module_source::code_no_comments("src", "topology_links")
    }

    /// **The security property.** A link is returned only when *both* endpoints are visible.
    /// Filtering on one end would tell a scoped operator that a node exists outside their scope,
    /// which is the fail-open direction — so this counts the two `EXISTS` clauses and the `AND`
    /// joining them, per statement.
    #[test]
    fn a_scoped_read_requires_both_endpoints_to_be_visible() {
        let src = production_source();
        let statements: Vec<&str> = src
            .split("FROM node_links")
            .skip(1)
            .filter(|s| s.contains("$1::uuid[] IS NULL"))
            .collect();
        assert_eq!(
            statements.len(),
            2,
            "expected exactly the paged and unpaged read to carry the scope predicate"
        );
        for s in statements {
            let clause = s.split("ORDER BY").next().unwrap_or(s);
            assert!(
                clause.contains("n.id = node_links.a_node"),
                "the `a` endpoint is not scope-checked: {clause}"
            );
            assert!(
                clause.contains("n.id = node_links.b_node"),
                "the `b` endpoint is not scope-checked: {clause}"
            );
            assert!(
                clause.contains("AND EXISTS"),
                "the two endpoint checks must be ANDed, not ORed: {clause}"
            );
        }
    }

    /// `first_seen` is the one column here that cannot be recomputed. Refreshing it on conflict
    /// would make every link look like it appeared this cycle.
    #[test]
    fn an_upsert_never_resets_first_seen() {
        let src = production_source();
        let update = src
            .split("ON CONFLICT (link_key) DO UPDATE SET")
            .nth(1)
            .expect("the upsert's update clause");
        let clause = update.split("\",").next().unwrap_or(update);
        assert!(
            !clause.contains("first_seen"),
            "first_seen is being written on conflict: {clause}"
        );
        assert!(clause.contains("last_seen = now()"));
    }

    /// Links go stale one at a time. A `DELETE FROM node_links` with no age predicate would empty
    /// the graph on any cycle where one node's observation was momentarily missing.
    #[test]
    fn stale_links_are_deleted_by_age_never_wholesale() {
        let src = production_source();
        for line in src.lines().filter(|l| l.contains("DELETE FROM node_links")) {
            assert!(
                line.contains("WHERE last_seen <"),
                "an unqualified delete would empty the graph: {line}"
            );
        }
    }

    #[test]
    fn paging_is_keyset_and_never_offset() {
        assert!(production_source().contains("ORDER BY id LIMIT"));
        assert!(
            !production_source().contains("OFFSET"),
            "OFFSET paging reintroduced"
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

    // --- Running the SQL, not reading it (ADR-114/116) -----------------------------------------
    //
    // Everything above reads this module's text; everything below sends its statements to a real
    // server. The two answer different questions, and they disagree in both directions: a rewrite
    // that keeps the words and changes the meaning passes every check above, and a projection that
    // simply lists fewer columns than the writer writes is invisible to both the compiler and a
    // text check. That second one is not hypothetical — `neighbors.rs` dropped two settings that
    // way from ADR-063 until ADR-115 ran the SQL and found them.
    use super::{DerivedLink, LinkSource, NodeId, StoredLink, TopoLinkRepo, TopologyLinkSummary};
    use crate::pgtest;

    /// Whether `rows` holds the link `want` — by its endpoint pair, which is its identity.
    fn holds(rows: &[StoredLink], want: &DerivedLink) -> bool {
        rows.iter()
            .any(|r| r.a_node == Some(want.a_node) && r.b_node == Some(want.b_node))
    }

    /// Every column a link carries goes in and comes back out with the same value.
    ///
    /// 🚨 The subject is the **projection**, not the row count. Stopping at "one row exists" is
    /// what lets a reader that names eight of the writer's ten columns pass.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn every_column_of_a_link_survives_the_round_trip(pool: sqlx::PgPool) {
        let a = pgtest::node(&pool, "a", 1, None).await;
        let b = pgtest::node(&pool, "b", 2, None).await;
        let repo = TopoLinkRepo::new(pool.clone());

        let mut link = DerivedLink::new(NodeId(a), NodeId(b), LinkSource::Lldp);
        link.sources.push(LinkSource::L3Subnet);
        link.a_ifindex = Some(11);
        link.b_ifindex = Some(22);
        link.a_if_name = Some("Gi0/1".to_owned());
        link.b_if_name = Some("Gi0/2".to_owned());
        link.subnet = Some("192.168.1.0/24".to_owned());
        link.forced_parent = Some(link.a_node);

        assert_eq!(repo.upsert_batch(&[link.clone()]).await.expect("upsert"), 1);
        assert_eq!(pgtest::rows(&pool, "node_links").await, 1);

        let stored = repo.list_page(None, None, 10).await.expect("list");
        assert_eq!(stored.len(), 1);
        let s = &stored[0];
        assert_eq!(s.a_node, Some(link.a_node));
        assert_eq!(s.b_node, Some(link.b_node));
        // The endpoints are canonically ordered, so which node landed in `a` is not this test's
        // business — but each side's port facts must have travelled with their own side.
        assert_eq!(s.a_ifindex, Some(11));
        assert_eq!(s.b_ifindex, Some(22));
        assert_eq!(s.a_if_name.as_deref(), Some("Gi0/1"));
        assert_eq!(s.b_if_name.as_deref(), Some("Gi0/2"));
        assert_eq!(s.subnet.as_deref(), Some("192.168.1.0/24"));
        assert_eq!(s.forced_parent, Some(link.a_node));
        let mut expected = vec![LinkSource::Lldp, LinkSource::L3Subnet];
        expected.sort_unstable();
        assert_eq!(
            s.sources, expected,
            "the comma-joined sources did not survive `string_to_array`"
        );
        assert_eq!(
            s.first_seen, s.last_seen,
            "a link written once already looks re-observed"
        );
    }

    /// A second observation updates the row in place and refreshes `last_seen` — and leaves
    /// `first_seen` where it was, which is the one fact here that cannot be recomputed.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_second_observation_updates_in_place_and_keeps_first_seen(pool: sqlx::PgPool) {
        let a = pgtest::node(&pool, "a", 1, None).await;
        let b = pgtest::node(&pool, "b", 2, None).await;
        let repo = TopoLinkRepo::new(pool.clone());

        let mut link = DerivedLink::new(NodeId(a), NodeId(b), LinkSource::Lldp);
        link.b_if_name = Some("Gi0/2".to_owned());
        repo.upsert_batch(&[link.clone()]).await.expect("upsert");
        let first = repo
            .list_page(None, None, 10)
            .await
            .expect("list")
            .remove(0);

        link.b_if_name = Some("Te1/1".to_owned());
        link.sources.push(LinkSource::Cdp);
        repo.upsert_batch(&[link.clone()]).await.expect("re-upsert");

        assert_eq!(
            pgtest::rows(&pool, "node_links").await,
            1,
            "the same link was stored twice — the conflict target is not the link key"
        );
        let second = repo
            .list_page(None, None, 10)
            .await
            .expect("list")
            .remove(0);
        assert_eq!(second.id, first.id);
        assert_eq!(
            second.first_seen, first.first_seen,
            "first_seen was reset by a re-observation; every link would look new every cycle"
        );
        assert!(
            second.last_seen > first.last_seen,
            "last_seen did not move, so the age-based prune would delete a link still being seen"
        );
        assert_eq!(
            second.b_if_name.as_deref(),
            Some("Te1/1"),
            "the conflict path did not update the row"
        );
        assert!(second.sources.contains(&LinkSource::Cdp));
    }

    /// The dependency graph's read: both endpoints and every source, with **no** scope filter —
    /// the two links below sit in different groups and both must come back.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn all_links_returns_every_pair_regardless_of_group(pool: sqlx::PgPool) {
        let one = pgtest::group(&pool, "one").await;
        let two = pgtest::group(&pool, "two").await;
        let a = pgtest::node(&pool, "a", 1, Some(one)).await;
        let b = pgtest::node(&pool, "b", 2, Some(two)).await;
        let c = pgtest::node(&pool, "c", 3, Some(two)).await;
        let repo = TopoLinkRepo::new(pool.clone());

        let mut l3 = DerivedLink::new(NodeId(b), NodeId(c), LinkSource::L3Subnet);
        l3.subnet = Some("10.9.0.0/24".to_owned());
        repo.upsert_batch(&[DerivedLink::new(NodeId(a), NodeId(b), LinkSource::Lldp), l3])
            .await
            .expect("upsert");

        let links = repo.all_links().await.expect("all_links");
        assert_eq!(
            links.len(),
            2,
            "the dependency graph is scoped or is dropping rows"
        );
        let canonical = DerivedLink::new(NodeId(a), NodeId(b), LinkSource::Lldp);
        let lldp = links
            .iter()
            .find(|l| l.sources.contains(&LinkSource::Lldp))
            .expect("the lldp link");
        assert_eq!(
            (lldp.a_node, lldp.b_node),
            (canonical.a_node, canonical.b_node),
            "the endpoints came back in a different order than they were stored in"
        );
        let subnet = links
            .iter()
            .find(|l| l.sources.contains(&LinkSource::L3Subnet))
            .expect("the l3 link");
        assert_eq!(subnet.subnet.as_deref(), Some("10.9.0.0/24"));
    }

    /// Links are removed by age, one at a time. A window three cycles wide keeps a link seen a
    /// moment ago; a zero-length one takes it.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_link_seen_this_cycle_survives_the_prune(pool: sqlx::PgPool) {
        let a = pgtest::node(&pool, "a", 1, None).await;
        let b = pgtest::node(&pool, "b", 2, None).await;
        let repo = TopoLinkRepo::new(pool.clone());
        repo.upsert_batch(&[DerivedLink::new(NodeId(a), NodeId(b), LinkSource::Cdp)])
            .await
            .expect("upsert");

        assert_eq!(
            repo.prune_stale(3600).await.expect("prune"),
            0,
            "a link seen a moment ago was deleted — the predicate points the wrong way"
        );
        assert_eq!(pgtest::rows(&pool, "node_links").await, 1);

        assert_eq!(repo.prune_stale(0).await.expect("prune"), 1);
        assert_eq!(pgtest::rows(&pool, "node_links").await, 0);
    }

    /// The derivation state is one row that each run replaces — never a second row.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_derivation_state_is_one_row_the_next_run_replaces(pool: sqlx::PgPool) {
        let repo = TopoLinkRepo::new(pool.clone());
        assert!(
            repo.last_run().await.expect("last_run").is_none(),
            "a fresh database reported a derivation run that never happened"
        );

        let first_summary = TopologyLinkSummary {
            lldp_links: 3,
            unmatched_lldp_rows: 7,
            ..TopologyLinkSummary::default()
        };
        repo.record_run(&first_summary, 3).await.expect("record");
        let first = repo
            .last_run()
            .await
            .expect("last_run")
            .expect("the run just recorded");
        assert_eq!(first.link_count, 3);
        assert_eq!(
            first.summary, first_summary,
            "the summary did not survive the JSONB round trip"
        );

        let next_summary = TopologyLinkSummary {
            l3_links: 9,
            ..TopologyLinkSummary::default()
        };
        repo.record_run(&next_summary, 9).await.expect("record");
        assert_eq!(
            pgtest::rows(&pool, "topology_derivation").await,
            1,
            "the second run appended a row instead of replacing the first"
        );
        let second = repo
            .last_run()
            .await
            .expect("last_run")
            .expect("the second run");
        assert_eq!(second.link_count, 9);
        assert_eq!(second.summary, next_summary);
        assert!(second.derived_at >= first.derived_at);
    }

    /// **The scope rule, executed.** A link is returned to a scoped caller only when *both* of its
    /// endpoints sit in a visible group — one visible end would tell that caller a node exists
    /// outside their scope, which is the fail-open direction.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_scoped_read_returns_a_link_only_when_both_ends_are_visible(pool: sqlx::PgPool) {
        let mine = pgtest::group(&pool, "mine").await;
        let theirs = pgtest::group(&pool, "theirs").await;
        let a = pgtest::node(&pool, "a", 1, Some(mine)).await;
        let b = pgtest::node(&pool, "b", 2, Some(theirs)).await;
        let c = pgtest::node(&pool, "c", 3, Some(mine)).await;
        let repo = TopoLinkRepo::new(pool.clone());
        let crossing = DerivedLink::new(NodeId(a), NodeId(b), LinkSource::Lldp);
        let internal = DerivedLink::new(NodeId(a), NodeId(c), LinkSource::Cdp);
        repo.upsert_batch(&[crossing.clone(), internal.clone()])
            .await
            .expect("upsert");

        // The acceptance side first. Without it, a predicate that refuses everything reads exactly
        // like a predicate that is working (`rejection-only-tests-pass-when-everything-rejects`).
        let all = repo.list_page(None, None, 10).await.expect("list");
        assert!(
            holds(&all, &crossing) && holds(&all, &internal),
            "an unscoped caller must see both links"
        );

        // 🚨 **Both sides, named from the stored link rather than assumed.** The endpoints are put
        // in canonical uuid order, so which group ends up holding `a_node` is luck — and a fixture
        // that only ever exercises one side would let the *other* `EXISTS` be deleted and stay
        // green half the time.
        let a_side = if crossing.a_node == NodeId(a) {
            mine
        } else {
            theirs
        };
        let b_side = if a_side == mine { theirs } else { mine };
        for (scope, side) in [(a_side, "a"), (b_side, "b")] {
            let rows = repo
                .list_page(Some(&[scope]), None, 10)
                .await
                .expect("list");
            assert!(
                !holds(&rows, &crossing),
                "the crossing link was returned to a caller who can see only its `{side}` endpoint"
            );
        }

        let scoped = repo.list_page(Some(&[mine]), None, 10).await.expect("list");
        assert!(
            holds(&scoped, &internal),
            "a link with both endpoints in scope was hidden"
        );

        let both = repo
            .list_page(Some(&[mine, theirs]), None, 10)
            .await
            .expect("list");
        assert!(
            holds(&both, &crossing),
            "widening the scope to both groups did not reveal the crossing link"
        );
    }

    /// The cursor branch. A page of one walks every link exactly once, in id order, and stops.
    ///
    /// ⚠️ The bounded loop is part of the assertion: a `WHERE id > $2` that stopped being applied
    /// would hand back the same first row forever, and an unbounded `loop` would hang rather than
    /// fail.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn paging_by_cursor_walks_every_link_exactly_once(pool: sqlx::PgPool) {
        let a = pgtest::node(&pool, "a", 1, None).await;
        let b = pgtest::node(&pool, "b", 2, None).await;
        let c = pgtest::node(&pool, "c", 3, None).await;
        let repo = TopoLinkRepo::new(pool.clone());
        repo.upsert_batch(&[
            DerivedLink::new(NodeId(a), NodeId(b), LinkSource::Lldp),
            DerivedLink::new(NodeId(a), NodeId(c), LinkSource::Cdp),
            DerivedLink::new(NodeId(b), NodeId(c), LinkSource::Ospf),
        ])
        .await
        .expect("upsert");

        let mut seen: Vec<i64> = Vec::new();
        let mut after: Option<i64> = None;
        for _ in 0..8 {
            let page = repo.list_page(None, after, 1).await.expect("list");
            let Some(link) = page.first() else { break };
            assert_eq!(page.len(), 1, "LIMIT is not being applied");
            seen.push(link.id);
            after = Some(link.id);
        }
        assert_eq!(
            seen.len(),
            3,
            "the cursor walk did not end after the three stored links: {seen:?}"
        );
        let mut ordered = seen.clone();
        ordered.sort_unstable();
        ordered.dedup();
        assert_eq!(
            ordered, seen,
            "the cursor page returned links out of id order or returned one twice: {seen:?}"
        );
    }
}
