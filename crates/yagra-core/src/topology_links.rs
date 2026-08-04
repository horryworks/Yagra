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
    /// This module's own source, for the SQL-shape assertions below. The scope rule and the
    /// never-wholesale-delete rule live entirely inside SQL strings, so nothing else can catch a
    /// rewrite that changes their meaning.
    const SRC: &str = include_str!("topology_links.rs");

    fn production_source() -> String {
        SRC.split("#[cfg(test)]")
            .next()
            .expect("split always yields a first element")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
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
        assert!(SRC.contains("ORDER BY id LIMIT"));
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
}
