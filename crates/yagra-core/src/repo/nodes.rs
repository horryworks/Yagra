// SPDX-License-Identifier: AGPL-3.0-only
//! The `nodes` table: inventory rows, their placement in the folder tree, and the batch
//! writers that fill in what a poll learned.
//!
//! Two methods here are filed by what their SQL touches rather than by what they are called.
//! `suppression_opt_outs` and `set_suppression_opt_out` sat among the deployment settings for
//! their whole life and read a **per-node column**, so they belong beside the other node
//! reads — see [`super`] for the rule and why it is the SQL that decides.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;

use sqlx::Row;
use uuid::Uuid;
use yagra_common::Node;

// Only the settings struct: `retention::Row` would collide with `sqlx::Row` above.

use super::*;

/// The dependency-graph skeleton for one node: just enough to draw the topology/dependency views
/// (id, display name, upstream parent). Loaded in keyset pages by [`NodeRepo::list_topology_page`]
/// so the endpoints never build one unbounded full-fleet row set (S7).
#[derive(Debug, Clone)]
pub struct TopologyRow {
    pub id: Uuid,
    pub name: String,
    pub parent_id: Option<Uuid>,
}

impl NodeRepo {
    /// Every node in the inventory (internal use; the API paginates via [`Self::list_nodes_page`]).
    pub async fn list_nodes(&self) -> anyhow::Result<Vec<Node>> {
        let rows = sqlx::query(&format!("SELECT {} FROM nodes", Self::NODE_COLUMNS))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(node_from_row).collect()
    }

    /// One node by id, if it exists (for endpoints that need its profile/bindings).
    pub async fn get_node(&self, id: Uuid) -> anyhow::Result<Option<Node>> {
        let row = sqlx::query(&format!(
            "SELECT {} FROM nodes WHERE id = $1",
            Self::NODE_COLUMNS
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(node_from_row).transpose()
    }

    /// One keyset page of nodes ordered by id, starting after `after`, within `groups`.
    ///
    /// The scope predicate lives in the `WHERE`, the cursor in `id > $2` — they are independent, so
    /// paging is unaffected by scoping (a page may simply contain fewer than `limit` rows).
    pub async fn list_nodes_page(
        &self,
        groups: GroupFilter<'_>,
        after: Option<Uuid>,
        limit: i64,
    ) -> anyhow::Result<Vec<Node>> {
        // Upper bound 501 (not 500) so the API can fetch one extra row past a 500-item page to
        // detect "has more" without an extra round-trip; the user-facing limit is capped at 500.
        let limit = limit.clamp(1, 501);
        let rows = match after {
            Some(after) => {
                sqlx::query(&format!(
                    "SELECT {} FROM nodes WHERE {} AND id > $2 ORDER BY id LIMIT $3",
                    Self::NODE_COLUMNS,
                    Self::SCOPE_PREDICATE
                ))
                .bind(Self::scope_bind(groups))
                .bind(after)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(&format!(
                    "SELECT {} FROM nodes WHERE {} ORDER BY id LIMIT $2",
                    Self::NODE_COLUMNS,
                    Self::SCOPE_PREDICATE
                ))
                .bind(Self::scope_bind(groups))
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };
        rows.iter().map(node_from_row).collect()
    }

    /// One keyset page of the dependency-graph skeleton — only `(id, name, parent_id)` — ordered by
    /// id, starting after `after` (S7). Deliberately light: the topology/coverage endpoints don't
    /// need the full node row (address/creds/tags), and at 50k nodes a large page keeps the
    /// whole-graph assembly to a handful of round-trips instead of returning one unbounded JSON blob.
    /// Scoping note: a scoped caller sees only the in-scope nodes, so a dependency edge whose
    /// parent lies outside the scope arrives with a `parent_id` that is not in the response. The
    /// graph renders it as a root, which is the honest reading — the caller cannot see the parent,
    /// so they cannot be told the child hangs off it.
    pub async fn list_topology_page(
        &self,
        groups: GroupFilter<'_>,
        after: Option<Uuid>,
        limit: i64,
    ) -> anyhow::Result<Vec<TopologyRow>> {
        // Larger page than the node list (its 501 cap is UI-facing); +1 to detect "has more".
        let limit = limit.clamp(1, 5001);
        let rows = match after {
            Some(after) => {
                sqlx::query(&format!(
                    "SELECT id, name, parent_id FROM nodes WHERE {} AND id > $2 \
                     ORDER BY id LIMIT $3",
                    Self::SCOPE_PREDICATE
                ))
                .bind(Self::scope_bind(groups))
                .bind(after)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(&format!(
                    "SELECT id, name, parent_id FROM nodes WHERE {} ORDER BY id LIMIT $2",
                    Self::SCOPE_PREDICATE
                ))
                .bind(Self::scope_bind(groups))
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };
        rows.iter()
            .map(|row| {
                Ok(TopologyRow {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    parent_id: row.try_get("parent_id")?,
                })
            })
            .collect()
    }

    /// Create a node; returns its new id. Optional profile, bound credential, parent, and
    /// descriptive vendor/model metadata.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_node(
        &self,
        name: &str,
        address: IpAddr,
        pool: Option<&str>,
        profile: Option<Uuid>,
        credential: Option<Uuid>,
        parent: Option<Uuid>,
        vendor: Option<&str>,
        model: Option<&str>,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO nodes \
             (id, name, address, pool, profile_id, credential_id, parent_id, vendor, model) \
             VALUES ($1, $2, $3::inet, $4, $5, $6, $7, $8, $9)",
        )
        .bind(id)
        .bind(name)
        .bind(address.to_string())
        .bind(pool)
        .bind(profile)
        .bind(credential)
        .bind(parent)
        .bind(vendor)
        .bind(model)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Bulk-import nodes **atomically**: all rows insert in a single transaction, so a failure
    /// partway (e.g. a duplicate name hitting the unique constraint) rolls back the whole batch
    /// instead of leaving a partial import. Returns how many were inserted. Caller pre-validates.
    pub async fn import_nodes(&self, nodes: &[NewNode<'_>]) -> anyhow::Result<u32> {
        let mut tx = self.pool.begin().await?;
        for n in nodes {
            sqlx::query(
                "INSERT INTO nodes (id, name, address, profile_id, credential_id, vendor, model) \
                 VALUES ($1, $2, $3::inet, $4, $5, $6, $7)",
            )
            .bind(Uuid::new_v4())
            .bind(n.name)
            .bind(n.address.to_string())
            .bind(n.profile)
            .bind(n.credential)
            .bind(n.vendor)
            .bind(n.model)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(nodes.len() as u32)
    }

    /// Set (or clear) a node's profile, bound credential, and vendor/model metadata, and optionally
    /// move it to a different poll-pool (ADR-009). Returns whether the node exists.
    ///
    /// `profile`/`credential`/`vendor`/`model` are set to the passed value (a `None` clears) — the
    /// node-edit UI loads the current values and resends them, so an unchanged field is preserved.
    /// `pool` is three-state so the pool can be *left alone* independently: outer `None` = leave the
    /// pool unchanged, inner `None` = clear it to NULL (falls back to the `default` pool), inner
    /// `Some` = set it (the caller has already validated it as a NATS-subject-safe token).
    pub async fn set_node_bindings(
        &self,
        id: Uuid,
        profile: Option<Uuid>,
        credential: Option<Uuid>,
        vendor: Option<&str>,
        model: Option<&str>,
        pool: Option<Option<&str>>,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE nodes SET profile_id = $2, credential_id = $3, vendor = $4, model = $5, \
             pool = CASE WHEN $6::boolean THEN $7::text ELSE pool END, \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(profile)
        .bind(credential)
        .bind(vendor)
        .bind(model)
        // `$6` gates whether the pool is touched at all; `$7` is the new value (NULL when clearing).
        .bind(pool.is_some())
        .bind(pool.flatten())
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Move a node into a group (or `None` to ungroup it), appending it to the **end** of the
    /// destination scope (max sort_order + 1) so it lands predictably at the bottom. Returns
    /// whether the node exists. Used by the "Move to…" picker and a drop directly onto a group;
    /// drag-reorder between siblings goes through [`Self::place_node`] instead.
    pub async fn set_node_group(&self, id: Uuid, group: Option<Uuid>) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE nodes SET group_id = $2, updated_at = now(), \
             sort_order = (SELECT COALESCE(MAX(sort_order), 0) + 1 FROM nodes \
                           WHERE group_id IS NOT DISTINCT FROM $2::uuid AND id <> $1) \
             WHERE id = $1",
        )
        .bind(id)
        .bind(group)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Set (or clear with `None`) a node's own poll-pool (ADR-009/020). `None` ⇒ NULL, so the node
    /// falls back to its folder's pool, else the default pool. Returns whether the node exists.
    ///
    /// Single-purpose on purpose: [`Self::set_node_bindings`] overwrites profile/credential/
    /// vendor/model unconditionally (only its `pool` is three-state-gated), so a caller that wants
    /// to move *just* the pool — the inventory tree's context menu — must not go through it.
    pub async fn set_node_pool(&self, id: Uuid, pool: Option<&str>) -> anyhow::Result<bool> {
        let res = sqlx::query("UPDATE nodes SET pool = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(pool)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The distinct non-empty pools nodes are assigned to. Feeds the pool picker; the `pool` index
    /// (migration 0001) keeps this cheap even at fleet scale.
    pub async fn distinct_pools(&self) -> anyhow::Result<Vec<String>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT pool FROM nodes WHERE pool IS NOT NULL AND pool <> ''",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Set (or clear with `None`) a node's **dependency parent** (upstream) — the `parent_id`
    /// edge that feeds parent-down alert suppression and root-cause roll-up (ADR-015). Distinct
    /// from [`Self::set_node_group`], which moves a node in the inventory *folder* tree. Returns
    /// whether the node exists. The caller validates that `parent` exists and that the new edge
    /// introduces no cycle ([`crate::groups::would_create_cycle`]) before calling this.
    pub async fn set_node_parent(&self, id: Uuid, parent: Option<Uuid>) -> anyhow::Result<bool> {
        let res = sqlx::query("UPDATE nodes SET parent_id = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(parent)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The `(id, sort_order)` of the nodes in `group` (NULL ⇒ ungrouped), ordered. Feeds
    /// [`crate::groups::placement_order`] when a drag drops a node before/after a sibling.
    pub async fn ordered_nodes_in_group(
        &self,
        group: Option<Uuid>,
    ) -> anyhow::Result<Vec<(Uuid, f64)>> {
        let rows = sqlx::query(
            "SELECT id, sort_order FROM nodes \
             WHERE group_id IS NOT DISTINCT FROM $1::uuid ORDER BY sort_order, name, id",
        )
        .bind(group)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("id")?, row.try_get("sort_order")?)))
            .collect()
    }

    /// The ids of nodes whose `group_id` is in `group_ids` — used to resolve a Troubleshoot
    /// "group" scope to a group + its descendant subgroups (the caller flattens the subtree via
    /// [`crate::groups::group_subtree`]). Parameterized `= ANY($1)` (security.md); empty input
    /// short-circuits so we never run an empty-array query.
    pub async fn nodes_in_groups(&self, group_ids: &[Uuid]) -> anyhow::Result<Vec<Uuid>> {
        if group_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT id FROM nodes WHERE group_id = ANY($1) ORDER BY sort_order, name, id",
        )
        .bind(group_ids)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(|row| Ok(row.try_get("id")?)).collect()
    }

    /// A group's **direct** member nodes (or the ungrouped bucket when `group` is `None`), ordered
    /// by the tree's sort order, capped at `limit`. Backs the inventory tree's per-group lazy load
    /// (A-3): the tree fetches a group's members only when it is expanded, so the initial view never
    /// pulls the whole fleet. `IS NOT DISTINCT FROM` so a `NULL` group matches the ungrouped rows.
    ///
    /// The scope predicate rides alongside the group filter rather than replacing it: asking for
    /// the ungrouped bucket (`group = None`) as a scoped caller correctly returns nothing, because
    /// an ungrouped node is outside every group scope (`rbac.rs`).
    pub async fn list_nodes_in_group(
        &self,
        groups: GroupFilter<'_>,
        group: Option<Uuid>,
        limit: i64,
    ) -> anyhow::Result<Vec<Node>> {
        let limit = limit.clamp(1, 5001);
        let rows = sqlx::query(&format!(
            "SELECT {} FROM nodes \
             WHERE {} AND group_id IS NOT DISTINCT FROM $2::uuid \
             ORDER BY sort_order, name, id LIMIT $3",
            Self::NODE_COLUMNS,
            Self::SCOPE_PREDICATE
        ))
        .bind(Self::scope_bind(groups))
        .bind(group)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(node_from_row).collect()
    }

    /// Assign a node to `group` and set its order in one update (drag reorder). Returns existence.
    pub async fn place_node(
        &self,
        id: Uuid,
        group: Option<Uuid>,
        order: f64,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE nodes SET group_id = $2, sort_order = $3, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(group)
        .bind(order)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Fill blank vendor/model
    /// for MANY nodes in one `UPDATE` (the async ingest writer, ADR-025). `COALESCE` preserves any
    /// existing value; a `None` leaves that column alone. `unnest` binds arrays, so the row count is
    /// unbounded by Postgres' parameter ceiling. Dedups keeping the last occurrence per node.
    ///
    /// ⚠️ **"Fill" is meant literally: a node whose vendor and model are already set is not
    /// written** (ADR-110 Increment 1). It used to be — `updated_at = now()` was unconditional, so
    /// a poll carrying a `sysDescr` rewrote its node's row with the values it already had. The
    /// COALESCE meant the stored values never moved, so the write was pure cost.
    ///
    /// 🚨 **How much cost is small, and an earlier version of this note said otherwise.** It is not
    /// a per-cycle fleet write: `assemble.rs` sets `probe_identity` only while `node.vendor` is
    /// `None`, so a node stops sending `sysDescr` as soon as this fills it, and `identify()` never
    /// returns a model without a vendor. What remains is the window between the fill landing in
    /// PostgreSQL and the scheduler's node cache noticing — a few polls per node, once. Measured on
    /// the 32-node lab: **88 `nodes` updates in total** against 14.6M on `interfaces`. Keep the
    /// predicate because a repeated no-op write is still a write, not because it was ever the
    /// expensive one.
    ///
    /// `updated_at` has no reader — it is not in [`NodeRepo::NODE_COLUMNS`], so `node_from_row`
    /// never selects it and no API or UI surface carries it — which is why not advancing it changes
    /// nothing observable.
    pub async fn fill_node_identity_batch(
        &self,
        rows: &[(Uuid, Option<String>, Option<String>)],
    ) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut by_node: BTreeMap<Uuid, (Option<String>, Option<String>)> = BTreeMap::new();
        for (node, vendor, model) in rows {
            by_node.insert(*node, (vendor.clone(), model.clone()));
        }
        let ids: Vec<Uuid> = by_node.keys().copied().collect();
        let vendors: Vec<Option<String>> = by_node.values().map(|v| v.0.clone()).collect();
        let models: Vec<Option<String>> = by_node.values().map(|v| v.1.clone()).collect();
        sqlx::query(
            "UPDATE nodes SET \
                vendor = COALESCE(nodes.vendor, t.vendor), \
                model = COALESCE(nodes.model, t.model), \
                updated_at = now() \
             FROM unnest($1::uuid[], $2::text[], $3::text[]) AS t(id, vendor, model) \
             WHERE nodes.id = t.id \
               AND ((nodes.vendor IS NULL AND t.vendor IS NOT NULL) \
                 OR (nodes.model IS NULL AND t.model IS NOT NULL))",
        )
        .bind(&ids)
        .bind(&vendors)
        .bind(&models)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The `sort_order` of each of the given node ids (for the inventory tree, which orders nodes
    /// within their group). Ids absent from the map default to 0 at the call site. One query.
    pub async fn node_sort_orders(&self, ids: &[Uuid]) -> anyhow::Result<HashMap<Uuid, f64>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query("SELECT id, sort_order FROM nodes WHERE id = ANY($1)")
            .bind(ids)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("id")?, row.try_get("sort_order")?)))
            .collect()
    }

    /// The display name of each of the given node ids, in one query. For joining TSDB results
    /// (which carry only the node id, ADR-011) back to human-readable names — e.g. the fleet
    /// Top-N endpoint. Ids absent from the map default to the id string at the call site.
    ///
    /// Scoped, because this is also the batch resolver behind `POST /api/v1/node-names`: a caller
    /// supplies ids and receives names, so without the filter it would answer "does a node with
    /// this id exist, and what is it called" for the entire fleet. An out-of-scope id is simply
    /// omitted, which is what the endpoint already does for an unknown id.
    pub async fn node_names(
        &self,
        groups: GroupFilter<'_>,
        ids: &[Uuid],
    ) -> anyhow::Result<HashMap<Uuid, String>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query(&format!(
            "SELECT id, name FROM nodes WHERE {} AND id = ANY($2)",
            Self::SCOPE_PREDICATE
        ))
        .bind(Self::scope_bind(groups))
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("id")?, row.try_get("name")?)))
            .collect()
    }

    /// Node ids whose display name matches a case-insensitive substring, capped at `cap`. Lets the
    /// event-log search (which runs against the log store, ADR-024) still find events by node name
    /// without the name ever entering the log store: the API resolves the name → ids here and
    /// passes them as a `node_id` filter (query-time join, ADR-011).
    pub async fn node_ids_by_name_like(
        &self,
        groups: GroupFilter<'_>,
        term: &str,
        cap: i64,
    ) -> anyhow::Result<Vec<Uuid>> {
        let rows = sqlx::query(&format!(
            "SELECT id FROM nodes WHERE {} AND name ILIKE '%' || $2 || '%' LIMIT $3",
            Self::SCOPE_PREDICATE
        ))
        .bind(Self::scope_bind(groups))
        .bind(term)
        .bind(cap.clamp(1, 200))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(|row| Ok(row.try_get("id")?)).collect()
    }

    /// The display facts a notification template renders against (ADR-039), for the given ids in
    /// one query. `LEFT JOIN`ed so an ungrouped node or one with no profile still comes back —
    /// the template just finds those variables undefined.
    ///
    // Unscoped, unlike `node_names`: the notifier is the deployment acting on its own behalf, not
    // a principal reading the inventory, so there is no scope to apply. Same call
    // `analysis/mod.rs` makes for a background run. The result never reaches an API response — it
    // renders into a notification whose destination the operator configured.
    /// Ids with no row are simply absent from the map; the caller falls back to the raw id.
    pub async fn node_facts(&self, ids: &[Uuid]) -> anyhow::Result<HashMap<Uuid, NodeFacts>> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query(
            "SELECT n.id, n.name, host(n.address) AS address, g.name AS group_name, \
                    p.name AS profile_name \
               FROM nodes n \
               LEFT JOIN node_groups g ON g.id = n.group_id \
               LEFT JOIN profiles p ON p.id = n.profile_id \
              WHERE n.id = ANY($1)",
        )
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    row.try_get("id")?,
                    NodeFacts {
                        name: row.try_get("name")?,
                        address: row.try_get("address")?,
                        group: row.try_get("group_name")?,
                        profile: row.try_get("profile_name")?,
                    },
                ))
            })
            .collect()
    }

    /// Address → node-id map for correlating passive events (syslog/trap source IPs) to
    /// inventory. Snapshotted by the event engine's periodic reload. If two nodes share an
    /// address the first row wins.
    pub async fn address_map(&self) -> anyhow::Result<HashMap<IpAddr, Uuid>> {
        let rows = sqlx::query("SELECT id, host(address) AS address FROM nodes")
            .fetch_all(&self.pool)
            .await?;
        let mut map = HashMap::with_capacity(rows.len());
        for row in rows {
            let id: Uuid = row.try_get("id")?;
            let addr: String = row.try_get("address")?;
            if let Ok(addr) = addr.parse::<IpAddr>() {
                map.entry(addr).or_insert(id);
            }
        }
        Ok(map)
    }

    /// Delete a node by id. Returns whether a row was removed.
    pub async fn delete_node(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM nodes WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Nodes an operator has excluded from derived suppression (ADR-043 Increment 3).
    ///
    /// Read as a set rather than a column on [`yagra_common::Node`] on purpose: only the projection
    /// consults it, and widening the shared node model would ripple through every construction site
    /// of a type that already carries everything else about a node.
    ///
    /// Degrades to **empty** on a read failure — empty means nothing is excluded, i.e. the derived
    /// graph applies in full. That is the wrong direction to fail in, so it is worth being explicit:
    /// the alternative (treating a failed read as "exclude everything") would silently disable
    /// suppression fleet-wide on a transient, which is a louder failure but a much more confusing
    /// one. The read is a single indexed scan alongside the node list it accompanies.
    pub async fn suppression_opt_outs(&self) -> std::collections::BTreeSet<yagra_common::NodeId> {
        let Ok(rows) = sqlx::query("SELECT id FROM nodes WHERE suppression_opt_out")
            .fetch_all(&self.pool)
            .await
        else {
            tracing::warn!("failed to read suppression opt-outs; treating none as excluded");
            return std::collections::BTreeSet::new();
        };
        rows.iter()
            .filter_map(|r| r.try_get::<Uuid, _>("id").ok())
            .map(yagra_common::NodeId)
            .collect()
    }

    /// Exclude one node from derived suppression, or put it back. Returns whether the node exists.
    pub async fn set_suppression_opt_out(&self, id: Uuid, opt_out: bool) -> anyhow::Result<bool> {
        let res = sqlx::query("UPDATE nodes SET suppression_opt_out = $2 WHERE id = $1")
            .bind(id)
            .bind(opt_out)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pgtest;

    /// A node written through the production writer comes back with every column it was given.
    ///
    /// The whole point of this file's SQL, and until ADR-115 nothing ran a line of it.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_created_node_reads_back_with_every_column_it_was_given(pool: sqlx::PgPool) {
        let repo = pgtest::repo(pool);
        let id = repo
            .create_node(
                "core-sw-01",
                "10.1.2.3".parse().expect("address"),
                Some("edge"),
                None,
                None,
                None,
                Some("Cisco"),
                Some("C9300"),
            )
            .await
            .expect("create");
        let node = repo.get_node(id).await.expect("read").expect("the node");
        assert_eq!(node.name, "core-sw-01");
        assert_eq!(node.address.to_string(), "10.1.2.3");
        assert_eq!(node.pool.as_deref(), Some("edge"));
        assert_eq!(node.vendor.as_deref(), Some("Cisco"));
        assert_eq!(node.model.as_deref(), Some("C9300"));
        assert_eq!(repo.list_nodes().await.expect("list").len(), 1);
    }

    /// Every setter reports whether it found the row — and says `false` for one that is not there.
    ///
    /// Both directions on purpose: a setter that reported `true` unconditionally would satisfy any
    /// test that only ever names a node that exists, and the callers branch on this to answer 404.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_setter_reports_whether_it_found_the_row(pool: sqlx::PgPool) {
        let group = pgtest::group(&pool, "tokyo").await;
        let id = pgtest::node(&pool, "n1", 1, None).await;
        let parent = pgtest::node(&pool, "n2", 2, None).await;
        let repo = pgtest::repo(pool);
        let absent = Uuid::new_v4();

        assert!(repo.set_node_group(id, Some(group)).await.expect("group"));
        assert!(!repo
            .set_node_group(absent, Some(group))
            .await
            .expect("group"));
        assert!(repo.set_node_pool(id, Some("edge")).await.expect("pool"));
        assert!(!repo
            .set_node_pool(absent, Some("edge"))
            .await
            .expect("pool"));
        assert!(repo
            .set_node_parent(id, Some(parent))
            .await
            .expect("parent"));
        assert!(!repo
            .set_node_parent(absent, Some(parent))
            .await
            .expect("parent"));

        let node = repo.get_node(id).await.expect("read").expect("the node");
        assert_eq!(node.pool.as_deref(), Some("edge"));
        assert_eq!(node.parent, Some(yagra_common::NodeId::from(parent)));
        // The folder is not a column on the node the API returns, so it is read where it lives.
        assert_eq!(
            repo.nodes_in_groups(&[group]).await.expect("in group"),
            vec![id]
        );
    }

    /// Placing a node sets its folder and its order, and both readers agree afterwards.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn placing_a_node_is_visible_to_both_order_readers(pool: sqlx::PgPool) {
        let group = pgtest::group(&pool, "osaka").await;
        let first = pgtest::node(&pool, "a", 1, None).await;
        let second = pgtest::node(&pool, "b", 2, None).await;
        let repo = pgtest::repo(pool);

        assert!(repo.place_node(second, Some(group), 20.0).await.expect("b"));
        assert!(repo.place_node(first, Some(group), 10.0).await.expect("a"));

        let ordered = repo
            .ordered_nodes_in_group(Some(group))
            .await
            .expect("ordered");
        assert_eq!(
            ordered.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![first, second],
            "the group is not returned in sort order"
        );
        let orders = repo
            .node_sort_orders(&[first, second])
            .await
            .expect("orders");
        assert_eq!(orders.get(&first).copied(), Some(10.0));
        assert_eq!(orders.get(&second).copied(), Some(20.0));
    }

    /// 🚨 An import creates a row per entry, **even for an address already monitored**.
    ///
    /// Written expecting the opposite, and pinned to what is true. The statement carries no
    /// `ON CONFLICT` and `nodes.address` has no `UNIQUE`, so nothing between
    /// `POST /api/v1/discovery/import` and the table refuses a second import of the same sweep.
    ///
    /// What that costs is not cosmetic: [`NodeRepo::address_map`] is a `HashMap` keyed by
    /// address, and it is how a syslog line and a flow record find the node they belong to. With
    /// two nodes at one address, one of them silently wins and the other is never attributed.
    ///
    /// Behaviour is unchanged here on purpose — de-duplicating is a decision about *which*
    /// existing node an import should adopt, and about what the UI should offer instead.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn importing_the_same_addresses_twice_creates_them_once(pool: sqlx::PgPool) {
        let repo = pgtest::repo(pool.clone());
        let rows = vec![
            NewNode {
                name: "imported-1",
                address: "10.9.0.1".parse().expect("address"),
                profile: None,
                credential: None,
                vendor: None,
                model: None,
            },
            NewNode {
                name: "imported-2",
                address: "10.9.0.2".parse().expect("address"),
                profile: None,
                credential: None,
                vendor: None,
                model: None,
            },
        ];
        assert_eq!(repo.import_nodes(&rows).await.expect("first"), 2);
        assert_eq!(pgtest::rows(&pool, "nodes").await, 2);

        assert_eq!(repo.import_nodes(&rows).await.expect("second"), 2);
        assert_eq!(
            pgtest::rows(&pool, "nodes").await,
            4,
            "importing the same addresses twice no longer duplicates them — good, but the doc\n\
             above and `address_map`'s callers were written against the old behaviour"
        );
        // And this is the consequence, stated as an assertion rather than as prose: two nodes,
        // one entry in the map every attribution path reads.
        assert_eq!(repo.address_map().await.expect("map").len(), 2);
    }

    /// Deleting reports whether it removed anything, and the row is gone afterwards.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn deleting_a_node_removes_it_once(pool: sqlx::PgPool) {
        let id = pgtest::node(&pool, "doomed", 1, None).await;
        let repo = pgtest::repo(pool.clone());
        assert!(repo.delete_node(id).await.expect("first"));
        assert!(
            !repo.delete_node(id).await.expect("second"),
            "a second delete claimed to have removed the same row"
        );
        assert!(repo.get_node(id).await.expect("read").is_none());
        assert_eq!(pgtest::rows(&pool, "nodes").await, 0);
    }

    /// The name search matches a substring, honours its cap, and stays inside the caller's scope.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_name_search_is_capped_and_scoped(pool: sqlx::PgPool) {
        let mine = pgtest::group(&pool, "mine").await;
        let theirs = pgtest::group(&pool, "theirs").await;
        pgtest::node(&pool, "edge-router-1", 1, Some(mine)).await;
        pgtest::node(&pool, "edge-router-2", 2, Some(mine)).await;
        pgtest::node(&pool, "edge-router-3", 3, Some(theirs)).await;
        let repo = pgtest::repo(pool);

        let all = repo
            .node_ids_by_name_like(None, "edge-router", 10)
            .await
            .expect("search");
        assert_eq!(all.len(), 3);
        let capped = repo
            .node_ids_by_name_like(None, "edge-router", 2)
            .await
            .expect("search");
        assert_eq!(capped.len(), 2, "the cap was not applied");
        let scoped = repo
            .node_ids_by_name_like(Some(&[mine]), "edge-router", 10)
            .await
            .expect("search");
        assert_eq!(scoped.len(), 2, "the scope did not narrow the search");
        assert!(repo
            .node_ids_by_name_like(None, "no-such-name", 10)
            .await
            .expect("search")
            .is_empty());
    }

    /// The id→name lookup answers only for nodes the caller may see.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_name_lookup_answers_only_inside_the_scope(pool: sqlx::PgPool) {
        let mine = pgtest::group(&pool, "mine").await;
        let theirs = pgtest::group(&pool, "theirs").await;
        let a = pgtest::node(&pool, "visible", 1, Some(mine)).await;
        let b = pgtest::node(&pool, "hidden", 2, Some(theirs)).await;
        let repo = pgtest::repo(pool);

        let unrestricted = repo.node_names(None, &[a, b]).await.expect("names");
        assert_eq!(unrestricted.len(), 2);

        let scoped = repo
            .node_names(Some(&[mine]), &[a, b])
            .await
            .expect("names");
        assert_eq!(scoped.get(&a).map(String::as_str), Some("visible"));
        assert!(
            !scoped.contains_key(&b),
            "a name outside the caller's scope was resolved"
        );
    }

    /// The address map is keyed by the address the node was created with.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_address_map_is_keyed_by_address(pool: sqlx::PgPool) {
        let id = pgtest::node(&pool, "mapped", 42, None).await;
        let repo = pgtest::repo(pool);
        let map = repo.address_map().await.expect("map");
        assert_eq!(
            map.get(&"10.0.0.42".parse::<IpAddr>().expect("address")),
            Some(&id)
        );
    }

    /// A suppression opt-out is stored, listed, and can be taken back.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_suppression_opt_out_can_be_set_and_cleared(pool: sqlx::PgPool) {
        let id = pgtest::node(&pool, "never-suppress-me", 1, None).await;
        let repo = pgtest::repo(pool);
        assert!(repo.suppression_opt_outs().await.is_empty());

        assert!(repo.set_suppression_opt_out(id, true).await.expect("set"));
        let opted = repo.suppression_opt_outs().await;
        assert!(opted.contains(&yagra_common::NodeId::from(id)));

        assert!(repo
            .set_suppression_opt_out(id, false)
            .await
            .expect("clear"));
        assert!(
            repo.suppression_opt_outs().await.is_empty(),
            "clearing the opt-out left the node on the list"
        );
    }
}
