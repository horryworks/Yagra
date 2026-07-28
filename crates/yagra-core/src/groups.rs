// SPDX-License-Identifier: AGPL-3.0-only
//! Hierarchical node groups (the inventory folder tree).
//!
//! A group has a [`GroupType`] (rendered with its own icon in the UI) and an optional parent,
//! forming a tree. Nodes reference a group via `nodes.group_id` (see [`crate::repo`]). This
//! module owns group CRUD; node↔group assignment lives on [`crate::repo::NodeRepo`].
//!
//! **Delete is non-destructive to nodes:** [`GroupRepo::delete`] re-parents a group's direct
//! child groups and member nodes up to the group's own parent (NULL ⇒ root) in one transaction,
//! then removes the row. Re-parenting a group guards against cycles via [`would_create_cycle`].

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::collections::{HashSet, VecDeque};
use uuid::Uuid;

/// The kind of a group — drives the icon and is purely organizational (not a polling concept).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupType {
    /// A physical site / location.
    Site,
    /// A geographic region (a set of sites).
    Region,
    /// A class of device (routers, switches, firewalls, …).
    DeviceType,
    /// A logical service the nodes deliver.
    Service,
    /// A generic folder with no special meaning.
    Generic,
}

impl GroupType {
    /// Every group type, for the type picker.
    pub const ALL: [GroupType; 5] = [
        GroupType::Site,
        GroupType::Region,
        GroupType::DeviceType,
        GroupType::Service,
        GroupType::Generic,
    ];

    /// Stable snake_case key (matches the serde representation and the stored value).
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            GroupType::Site => "site",
            GroupType::Region => "region",
            GroupType::DeviceType => "device_type",
            GroupType::Service => "service",
            GroupType::Generic => "generic",
        }
    }

    /// Parse a stored/edge key back into a type (validation at the API edge).
    #[must_use]
    pub fn from_key(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.key() == s)
    }
}

/// One group row returned by the API. `group_type` is the snake_case key.
#[derive(Debug, Clone, Serialize)]
pub struct GroupSummary {
    pub id: Uuid,
    pub name: String,
    pub group_type: String,
    pub parent_id: Option<Uuid>,
    /// Manual order within the parent scope (the UI sorts siblings by this, then by name).
    pub sort_order: f64,
    /// Optional geo coordinates for the dashboard map (both set ⇒ plotted as a pin).
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// Poll-pool this folder assigns to its nodes (ADR-009/020, migration 0054). `null` ⇒ inherit
    /// from the nearest ancestor that sets one, else the default pool. A node's own `pool` still
    /// wins — see [`crate::poolres`].
    pub pool: Option<String>,
}

/// A fractional sort_order that places an item between `prev` and `next` — the order values of
/// its new neighbours in the destination scope (either side absent at an edge). Midpoint inserts
/// keep reordering to a single-row update; values are seeded with integer spacing (migration
/// 0015) so a long run of midpoints stays well within `f64` precision. Pure for unit tests.
#[must_use]
pub fn order_between(prev: Option<f64>, next: Option<f64>) -> f64 {
    match (prev, next) {
        (Some(p), Some(n)) => (p + n) / 2.0,
        (Some(p), None) => p + 1.0,
        (None, Some(n)) => n - 1.0,
        (None, None) => 0.0,
    }
}

/// The new sort_order for an item dropped into `siblings` — the destination scope's current
/// items, ordered ascending and **not** including the moving item. `before`/`after` name the
/// drop target (at most one is set); if neither matches a sibling the item is appended after the
/// last one. Pure so the placement maths is unit-tested without a database.
#[must_use]
pub fn placement_order(siblings: &[(Uuid, f64)], before: Option<Uuid>, after: Option<Uuid>) -> f64 {
    let pos = |id: Uuid| siblings.iter().position(|(s, _)| *s == id);
    if let Some(i) = before.and_then(pos) {
        let prev = i.checked_sub(1).map(|j| siblings[j].1);
        order_between(prev, Some(siblings[i].1))
    } else if let Some(i) = after.and_then(pos) {
        let next = siblings.get(i + 1).map(|(_, o)| *o);
        order_between(Some(siblings[i].1), next)
    } else {
        order_between(siblings.last().map(|(_, o)| *o), None)
    }
}

/// Whether re-parenting `moving` under `new_parent` would create a cycle, given the current
/// `(id, parent_id)` edges. A group cannot become its own ancestor (or its own parent). Pure so
/// it can be unit-tested without a database; the API calls it before persisting a move.
#[must_use]
pub fn would_create_cycle(
    edges: &[(Uuid, Option<Uuid>)],
    moving: Uuid,
    new_parent: Option<Uuid>,
) -> bool {
    let parent_of = |id: Uuid| edges.iter().find(|(e, _)| *e == id).and_then(|(_, p)| *p);
    let mut cur = new_parent;
    // Bound the walk by the edge count so malformed (already-cyclic) data can't loop forever.
    for _ in 0..=edges.len() {
        match cur {
            None => return false,
            Some(p) if p == moving => return true,
            Some(p) => cur = parent_of(p),
        }
    }
    true
}

/// The `root` group plus every group beneath it, via BFS over `(id, parent_id)` edges (the shape
/// [`GroupRepo::edges`] returns). Always includes `root`; a visited set bounds the walk so cyclic
/// or malformed data can't loop forever. Pure, so the subtree maths is unit-tested without a DB.
/// Used to resolve a Troubleshoot "group" scope to the group + all its descendant subgroups.
#[must_use]
pub fn group_subtree(edges: &[(Uuid, Option<Uuid>)], root: Uuid) -> Vec<Uuid> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut queue = VecDeque::new();
    queue.push_back(root);
    seen.insert(root);
    while let Some(cur) = queue.pop_front() {
        out.push(cur);
        for (id, parent) in edges {
            if *parent == Some(cur) && seen.insert(*id) {
                queue.push_back(*id);
            }
        }
    }
    out
}

/// PostgreSQL-backed group store.
pub struct GroupRepo {
    pool: PgPool,
}

impl GroupRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// All groups (the UI builds the tree from the flat list). Ordered by the manual sort_order
    /// within each parent scope, then name — the same order the tree renders.
    pub async fn list(&self) -> anyhow::Result<Vec<GroupSummary>> {
        let rows = sqlx::query(
            "SELECT id, name, group_type, parent_id, sort_order, latitude, longitude, pool \
             FROM node_groups ORDER BY sort_order, name, id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(GroupSummary {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    group_type: row.try_get("group_type")?,
                    parent_id: row.try_get("parent_id")?,
                    sort_order: row.try_get("sort_order")?,
                    latitude: row.try_get("latitude")?,
                    longitude: row.try_get("longitude")?,
                    pool: row.try_get("pool")?,
                })
            })
            .collect()
    }

    /// Set (or clear, with `None`) a group's geo coordinates. Returns whether the group exists.
    /// Coordinate-range validation happens at the API edge.
    pub async fn set_geo(
        &self,
        id: Uuid,
        latitude: Option<f64>,
        longitude: Option<f64>,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query("UPDATE node_groups SET latitude = $2, longitude = $3 WHERE id = $1")
            .bind(id)
            .bind(latitude)
            .bind(longitude)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The `(id, sort_order)` of the groups directly under `parent` (NULL ⇒ top level), ordered.
    /// Feeds [`placement_order`] when a drag drops a group before/after a sibling.
    pub async fn ordered_siblings(&self, parent: Option<Uuid>) -> anyhow::Result<Vec<(Uuid, f64)>> {
        let rows = sqlx::query(
            "SELECT id, sort_order FROM node_groups \
             WHERE parent_id IS NOT DISTINCT FROM $1::uuid ORDER BY sort_order, name, id",
        )
        .bind(parent)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("id")?, row.try_get("sort_order")?)))
            .collect()
    }

    /// Re-parent a group and set its order in one update (drag reorder/nest). The caller must
    /// have rejected a cycle-inducing `parent` (see [`would_create_cycle`]). Returns existence.
    pub async fn place(&self, id: Uuid, parent: Option<Uuid>, order: f64) -> anyhow::Result<bool> {
        let res =
            sqlx::query("UPDATE node_groups SET parent_id = $2, sort_order = $3 WHERE id = $1")
                .bind(id)
                .bind(parent)
                .bind(order)
                .execute(&self.pool)
                .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The `(id, parent_id)` edges, for cycle checks before a move.
    pub async fn edges(&self) -> anyhow::Result<Vec<(Uuid, Option<Uuid>)>> {
        let rows = sqlx::query("SELECT id, parent_id FROM node_groups")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("id")?, row.try_get("parent_id")?)))
            .collect()
    }

    /// The `(id, parent_id, pool)` rows, for building a [`crate::poolres::PoolResolver`]. Read
    /// whole (the table is small) so effective-pool resolution costs one query, not one per node.
    pub async fn pool_rows(&self) -> anyhow::Result<Vec<(Uuid, Option<Uuid>, Option<String>)>> {
        let rows = sqlx::query("SELECT id, parent_id, pool FROM node_groups")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    row.try_get("id")?,
                    row.try_get("parent_id")?,
                    row.try_get("pool")?,
                ))
            })
            .collect()
    }

    /// Set (or clear with `None`) just this folder's poll-pool, leaving name/type/parent alone —
    /// the inventory tree's context-menu action. Returns whether the group exists.
    pub async fn set_pool(&self, id: Uuid, pool: Option<&str>) -> anyhow::Result<bool> {
        let res = sqlx::query("UPDATE node_groups SET pool = $2 WHERE id = $1")
            .bind(id)
            .bind(pool)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The distinct non-empty pools folders assign. Feeds the pool picker together with
    /// [`crate::repo::NodeRepo::distinct_pools`].
    pub async fn distinct_pools(&self) -> anyhow::Result<Vec<String>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT pool FROM node_groups WHERE pool IS NOT NULL AND pool <> ''",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Create a group; returns its id. `pool` is the folder's poll-pool assignment (`None` ⇒
    /// inherit), already validated by the caller.
    pub async fn create(
        &self,
        name: &str,
        group_type: GroupType,
        parent: Option<Uuid>,
        pool: Option<&str>,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        // Append to the end of the parent scope (max sort_order + 1) so a new group lands at the
        // bottom of its siblings rather than jumping to the top (the DEFAULT 0).
        sqlx::query(
            "INSERT INTO node_groups (id, name, group_type, parent_id, sort_order, pool) VALUES \
             ($1, $2, $3, $4, \
              (SELECT COALESCE(MAX(sort_order), 0) + 1 FROM node_groups \
               WHERE parent_id IS NOT DISTINCT FROM $4::uuid), $5)",
        )
        .bind(id)
        .bind(name)
        .bind(group_type.key())
        .bind(parent)
        .bind(pool)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Rename / re-type / re-parent a group, and optionally move its poll-pool. Returns whether
    /// the group exists. The caller must have already rejected a cycle-inducing `parent` (see
    /// [`would_create_cycle`]).
    ///
    /// `pool` is three-state, matching `Repo::set_node_bindings`: outer `None` leaves the column
    /// alone, `Some(None)` clears it to NULL (inherit), `Some(Some(p))` sets it.
    pub async fn update(
        &self,
        id: Uuid,
        name: &str,
        group_type: GroupType,
        parent: Option<Uuid>,
        pool: Option<Option<&str>>,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE node_groups SET name = $2, group_type = $3, parent_id = $4, \
                    pool = CASE WHEN $5 THEN $6 ELSE pool END \
             WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(group_type.key())
        .bind(parent)
        .bind(pool.is_some())
        .bind(pool.flatten())
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Delete a group, re-parenting its direct child groups and member nodes up to the group's
    /// own parent (NULL ⇒ root) so **no node is ever deleted**. Atomic. Returns whether the
    /// group existed.
    pub async fn delete(&self, id: Uuid) -> anyhow::Result<bool> {
        let mut tx = self.pool.begin().await?;
        // Resolve the group's parent (and confirm it exists). `query_scalar` over the nullable
        // column yields Option<Option<Uuid>>: outer = row found, inner = the parent value.
        let found: Option<Option<Uuid>> =
            sqlx::query_scalar("SELECT parent_id FROM node_groups WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(parent) = found else {
            return Ok(false);
        };
        // Child groups move up to the parent.
        sqlx::query("UPDATE node_groups SET parent_id = $2 WHERE parent_id = $1")
            .bind(id)
            .bind(parent)
            .execute(&mut *tx)
            .await?;
        // Member nodes move up to the parent (never deleted).
        sqlx::query("UPDATE nodes SET group_id = $2, updated_at = now() WHERE group_id = $1")
            .bind(id)
            .bind(parent)
            .execute(&mut *tx)
            .await?;
        let res = sqlx::query("DELETE FROM node_groups WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(res.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_type_keys_round_trip() {
        for t in GroupType::ALL {
            assert_eq!(GroupType::from_key(t.key()), Some(t));
        }
        assert_eq!(GroupType::from_key("nope"), None);
        // Keys agree with the serde wire form.
        assert_eq!(
            serde_json::to_string(&GroupType::DeviceType).unwrap(),
            "\"device_type\""
        );
    }

    #[test]
    fn order_between_interpolates_and_extends() {
        // Between two neighbours → midpoint.
        assert_eq!(order_between(Some(1.0), Some(3.0)), 2.0);
        // Append after the last → +1.
        assert_eq!(order_between(Some(5.0), None), 6.0);
        // Prepend before the first → -1.
        assert_eq!(order_between(None, Some(2.0)), 1.0);
        // Only element.
        assert_eq!(order_between(None, None), 0.0);
    }

    #[test]
    fn placement_order_targets_neighbours() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        // Siblings already ordered, NOT including the moving item.
        let sibs = [(a, 1.0), (b, 2.0), (c, 3.0)];

        // Drop before b → midpoint of a and b.
        assert_eq!(placement_order(&sibs, Some(b), None), 1.5);
        // Drop after b → midpoint of b and c.
        assert_eq!(placement_order(&sibs, None, Some(b)), 2.5);
        // Drop before the first → below a.
        assert_eq!(placement_order(&sibs, Some(a), None), 0.0);
        // Drop after the last → above c.
        assert_eq!(placement_order(&sibs, None, Some(c)), 4.0);
        // No / unknown target → append to the end.
        assert_eq!(placement_order(&sibs, None, None), 4.0);
        assert_eq!(placement_order(&sibs, Some(Uuid::from_u128(9)), None), 4.0);
        // Empty scope → 0.
        assert_eq!(placement_order(&[], None, None), 0.0);
    }

    #[test]
    fn cycle_detection() {
        // a → b → c  (c's parent is b, b's parent is a, a is root)
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let edges = vec![(a, None), (b, Some(a)), (c, Some(b))];

        // Moving a under c would make a a descendant of itself → cycle.
        assert!(would_create_cycle(&edges, a, Some(c)));
        // A group cannot be its own parent.
        assert!(would_create_cycle(&edges, b, Some(b)));
        // Moving c under a (a is not below c) is fine.
        assert!(!would_create_cycle(&edges, c, Some(a)));
        // Moving to root is always fine.
        assert!(!would_create_cycle(&edges, b, None));
    }

    #[test]
    fn group_subtree_collects_root_and_descendants() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let d = Uuid::from_u128(4);
        // a → {b, c}, b → d.
        let edges = vec![(a, None), (b, Some(a)), (c, Some(a)), (d, Some(b))];

        let mut from_a = group_subtree(&edges, a);
        from_a.sort();
        assert_eq!(from_a, vec![a, b, c, d]); // whole subtree, incl. the root

        let mut from_b = group_subtree(&edges, b);
        from_b.sort();
        assert_eq!(from_b, vec![b, d]); // a branch

        assert_eq!(group_subtree(&edges, c), vec![c]); // a leaf is just itself
    }

    #[test]
    fn group_subtree_unknown_root_is_just_itself() {
        let x = Uuid::from_u128(9);
        assert_eq!(group_subtree(&[], x), vec![x]);
    }

    #[test]
    fn group_subtree_terminates_on_a_cycle() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        // Malformed data: a ↔ b parent each other. The visited set must stop the walk.
        let edges = vec![(a, Some(b)), (b, Some(a))];
        let mut sub = group_subtree(&edges, a);
        sub.sort();
        assert_eq!(sub, vec![a, b]);
    }
}
