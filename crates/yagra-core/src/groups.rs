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
use std::collections::{HashMap, HashSet, VecDeque};
use uuid::Uuid;

/// Longest ancestor chain any group walk will follow before giving up.
///
/// `node_groups.parent_id` is a self-FK with no cycle constraint — [`would_create_cycle`] guards
/// the three endpoints that can set a parent, but `GroupRepo::delete`'s re-parenting does not
/// re-check — so every upward walk is bounded by this *and* a visited set. A real folder tree is
/// nowhere near this deep. Lives here rather than beside either caller because the bound is a
/// property of the group tree, not of what is being inherited along it.
pub const MAX_GROUP_DEPTH: usize = 64;

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
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct GroupSummary {
    pub id: Uuid,
    pub name: String,
    pub group_type: String,
    pub parent_id: Option<Uuid>,
    /// Manual order within the parent scope (the UI sorts siblings by this, then by name).
    pub sort_order: f64,
    /// The group's own geo coordinates, as stored (both set ⇒ drawn as a pin). A descendant
    /// folder normally leaves these null and inherits — see the `effective_*` pair below.
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// Where this group sits on the map after inheritance: its own coordinates, else the nearest
    /// ancestor's, else null. Computed on every read; never stored.
    ///
    /// **These do not add a pin.** A group is drawn on the map only when `geo_source` is `own`;
    /// for every other group this says which pin its nodes are counted at (`geo_group`).
    // Resolved by `resolve_group_geo`, which is also where the "why" lives.
    pub effective_latitude: Option<f64>,
    pub effective_longitude: Option<f64>,
    /// Whether the effective position is the group's own, inherited from an ancestor, or absent.
    pub geo_source: GeoSource,
    /// The group that supplied the effective position: this group when `geo_source` is `own`, the
    /// ancestor it inherited from when `inherited`, null when `unset`. This is the pin the group's
    /// nodes belong to, so a client never has to walk the folder tree itself.
    pub geo_group: Option<Uuid>,
    /// Poll-pool this folder assigns to its nodes (ADR-009/020, migration 0054). `null` ⇒ inherit
    /// from the nearest ancestor that sets one, else the default pool. A node's own `pool` still
    /// wins — see [`crate::poolres`].
    pub pool: Option<String>,
    /// The IP prefixes in use at this folder (ADR-100 decision 10, migration 0104). Empty for a
    /// folder nothing has attached one to, which is every folder in a deployment with no NetBox.
    ///
    /// 🚨 **Empty also means "you may not see them".** [`crate::api::groups::visible_groups`]
    /// clears this on a row a scoped caller receives only as a breadcrumb ancestor: such a row is
    /// listed so the tree has a spine, and handing over the subnet layout of a site whose
    /// membership the caller cannot see would be a leak the folder's *name* does not constitute.
    pub prefixes: Vec<GroupPrefix>,
}

/// One IP prefix attached to a folder.
///
/// Two fields and no more, on purpose. NetBox's prefix rows also carry `status`, `vrf`,
/// `is_pool`, `role` and a tenant, and none of them has a reader here: what a person needs in
/// order to choose a sweep target is the range and what it is called. Storing the rest would be a
/// second copy of NetBox's inventory that nothing consults.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct GroupPrefix {
    /// Canonical CIDR, e.g. `"192.168.1.0/24"`. PostgreSQL's `cidr` type rendered as text, so the
    /// mask is always present — unlike `inet`, where a host address would print bare.
    pub prefix: String,
    /// NetBox's description of the range ("Matsuyama LAN"), or empty.
    pub description: String,
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

/// The chain of groups **above** `start`, nearest parent first, over the same `(id, parent_id)`
/// edges [`group_subtree`] walks. Excludes `start` itself. A visited set bounds the walk so cyclic
/// or malformed data can't loop forever.
///
/// This is what keeps a group-scoped inventory tree from rendering as orphans: the WebUI builds the
/// forest from `parent_id`, so handing it a scoped subtree without the ancestors leaves every
/// visible root pointing at a parent that is not in the response. The ancestors are breadcrumb
/// only — they carry no membership, and being able to *name* the group above yours is not the same
/// as being able to see what is in it.
#[must_use]
pub fn group_ancestors(edges: &[(Uuid, Option<Uuid>)], start: Uuid) -> Vec<Uuid> {
    let parent_of = |id: Uuid| edges.iter().find(|(e, _)| *e == id).and_then(|(_, p)| *p);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    seen.insert(start);
    let mut cur = parent_of(start);
    while let Some(p) = cur {
        if !seen.insert(p) {
            break;
        }
        out.push(p);
        cur = parent_of(p);
    }
    out
}

/// For every group, the nearest group at-or-above it that carries a value — the shared engine
/// behind *both* inheritable folder attributes (poll pool, ADR-009/020; map coordinates).
///
/// `rows` are `(id, parent_id, own value)`; the result maps a group to the value it resolves to
/// **and the group that supplied it** (itself, when it carries its own). A group whose whole chain
/// is unset is absent from the map.
///
/// One upward walk per group with **path compression** — the answer is written back to every group
/// on the chain — so a resolved chain is walked once, not once per descendant. (Chains that resolve
/// to nothing aren't memoized, so an all-unset forest costs O(groups × depth); with hundreds of rows
/// and [`MAX_GROUP_DEPTH`] that is still trivial, and it keeps the map's meaning simple.) A cycle or
/// an over-deep chain resolves to "nothing inherited" and warns, rather than hanging.
///
/// **One walk, not one per attribute.** The cycle guard, the depth bound and the compression are
/// the parts that are easy to get subtly wrong, and a second copy would be the one that drifts —
/// so callers supply the payload and the fallback rule, never the traversal.
pub fn resolve_nearest_ancestor<T: Clone>(
    rows: impl IntoIterator<Item = (Uuid, Option<Uuid>, Option<T>)>,
) -> HashMap<Uuid, (T, Uuid)> {
    let own: HashMap<Uuid, (Option<Uuid>, Option<T>)> = rows
        .into_iter()
        .map(|(id, parent, value)| (id, (parent, value)))
        .collect();

    let mut resolved: HashMap<Uuid, (T, Uuid)> = HashMap::new();
    for &start in own.keys() {
        if resolved.contains_key(&start) {
            continue; // already answered as part of an earlier group's chain
        }
        // Walk up to the first group with a value (or a memoized answer), recording the path.
        let mut chain: Vec<Uuid> = Vec::new();
        let mut cur = Some(start);
        let mut answer: Option<(T, Uuid)> = None;
        let mut depth = 0usize;
        while let Some(id) = cur {
            if let Some(found) = resolved.get(&id) {
                answer = Some(found.clone());
                break;
            }
            if chain.contains(&id) || depth > MAX_GROUP_DEPTH {
                tracing::warn!(
                    group = %id,
                    "node group ancestry is cyclic or deeper than the supported bound — \
                     treating it as having nothing to inherit"
                );
                break;
            }
            let Some((parent, value)) = own.get(&id) else {
                break; // dangling parent_id: nothing more to inherit from
            };
            // Recorded before the value check so the supplying group is memoized too, not just
            // the descendants that inherit from it.
            chain.push(id);
            if let Some(v) = value {
                answer = Some((v.clone(), id));
                break;
            }
            cur = *parent;
            depth += 1;
        }
        // Path compression: every group we walked through shares the answer.
        if let Some(found) = answer {
            for id in chain {
                resolved.insert(id, found.clone());
            }
        }
    }
    resolved
}

/// Where a group's effective map position came from.
// The geo twin of `crate::poolres::PoolSource`, minus a node level (nodes have no coordinates)
// and minus a default (there is no implicit place on Earth).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GeoSource {
    /// The group carries its own coordinates and is drawn as a pin.
    Own,
    /// Inherited from the nearest ancestor that carries coordinates, named by `geo_group`. The
    /// group is not drawn as its own pin; its nodes are counted at that ancestor's.
    Inherited,
    /// Neither this group nor any ancestor is placed — it is not on the map.
    Unset,
}

/// Fill every row's effective coordinates from the nearest ancestor that carries them.
///
/// **This is the whole of "geo inheritance", and it deliberately does not create pins.** A pin is
/// drawn for a group that carries its *own* coordinates; inheritance means a descendant resolves
/// *to* that pin, so its nodes are counted there. Placing a pin per inheriting group would draw
/// thirty exactly-overlapping pins for thirty racks in one building and hide the site behind them.
///
/// The map therefore has the same number of pins before and after this runs — what changes is what
/// each pin counts, from "the folder's direct members" to "everything that resolves here". That is
/// the substance: a site pin whose nodes all live in rack sub-folders showed nothing at all before.
///
/// Pure (the resolution rule is unit-tested without a database) and resolved on read rather than
/// materialized, for the reason threshold and pool inheritance are (ADR-013): a stored copy goes
/// stale the moment a parent is edited or a folder is moved.
pub fn resolve_group_geo(groups: &mut [GroupSummary]) {
    let resolved = resolve_nearest_ancestor(
        groups
            .iter()
            .map(|g| (g.id, g.parent_id, coords_of(g.latitude, g.longitude))),
    );
    for g in groups.iter_mut() {
        match resolved.get(&g.id) {
            Some(((lat, lon), from)) => {
                g.effective_latitude = Some(*lat);
                g.effective_longitude = Some(*lon);
                g.geo_group = Some(*from);
                g.geo_source = if *from == g.id {
                    GeoSource::Own
                } else {
                    GeoSource::Inherited
                };
            }
            None => {
                g.effective_latitude = None;
                g.effective_longitude = None;
                g.geo_group = None;
                g.geo_source = GeoSource::Unset;
            }
        }
    }
}

/// A placement is both coordinates or neither — a row with only one is unplaced, not half-placed.
/// The write path (`PUT /node-groups/{id}/geo`) sets and clears them together, so a lone value is
/// legacy or hand-edited data; treating it as placed would put a pin on the prime meridian.
fn coords_of(lat: Option<f64>, lon: Option<f64>) -> Option<(f64, f64)> {
    match (lat, lon) {
        // NaN/±inf would project to nowhere and poison the fit-to-view bounds for every other pin.
        (Some(la), Some(lo)) if la.is_finite() && lo.is_finite() => Some((la, lo)),
        _ => None,
    }
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
    ///
    /// Geo inheritance is resolved here rather than by the caller, so there is exactly one place
    /// that answers "where is this folder on the map" — see [`resolve_group_geo`]. It needs the
    /// whole table, which is precisely what this query already returns.
    pub async fn list(&self) -> anyhow::Result<Vec<GroupSummary>> {
        let rows = sqlx::query(
            "SELECT id, name, group_type, parent_id, sort_order, latitude, longitude, pool \
             FROM node_groups ORDER BY sort_order, name, id",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut groups: Vec<GroupSummary> = rows
            .into_iter()
            .map(|row| {
                Ok(GroupSummary {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    group_type: row.try_get("group_type")?,
                    parent_id: row.try_get("parent_id")?,
                    sort_order: row.try_get("sort_order")?,
                    latitude: row.try_get("latitude")?,
                    longitude: row.try_get("longitude")?,
                    // Overwritten wholesale by `resolve_group_geo` below; the row carries no
                    // stored answer for these.
                    effective_latitude: None,
                    effective_longitude: None,
                    geo_source: GeoSource::Unset,
                    geo_group: None,
                    pool: row.try_get("pool")?,
                    // Filled from the second query below: one round trip for the whole tree
                    // rather than a lateral join, because most deployments have no rows here at
                    // all and the empty answer is then a single index-less scan of nothing.
                    prefixes: Vec::new(),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        resolve_group_geo(&mut groups);
        self.attach_prefixes(&mut groups).await?;
        Ok(groups)
    }

    /// Fold `node_group_prefixes` into an already-built group list.
    ///
    /// ⚠️ `prefix::TEXT` — the column is `cidr`, and sqlx has no mapping for it without the
    /// `ipnetwork` feature. Casting in the query keeps that feature (and a crate) out of the
    /// build, and for `cidr` the text form is exactly what was stored: the mask is always
    /// rendered, so `192.168.1.0/24` round-trips. (`inet` would **add** a `/32` to a host
    /// address, which is the trap `dns_check.rs` records.)
    async fn attach_prefixes(&self, groups: &mut [GroupSummary]) -> anyhow::Result<()> {
        let rows = sqlx::query(
            "SELECT group_id, prefix::TEXT AS prefix, description \
             FROM node_group_prefixes ORDER BY prefix",
        )
        .fetch_all(&self.pool)
        .await?;
        if rows.is_empty() {
            return Ok(());
        }
        let mut by_group: std::collections::HashMap<Uuid, Vec<GroupPrefix>> =
            std::collections::HashMap::new();
        for row in rows {
            let group_id: Uuid = row.try_get("group_id")?;
            by_group.entry(group_id).or_default().push(GroupPrefix {
                prefix: row.try_get("prefix")?,
                description: row.try_get("description")?,
            });
        }
        for g in groups.iter_mut() {
            if let Some(v) = by_group.remove(&g.id) {
                g.prefixes = v;
            }
        }
        Ok(())
    }

    /// Whether a folder with this id exists.
    ///
    /// Exists so a write path can refuse an unknown folder with a 400 that names the problem,
    /// instead of letting the foreign key turn it into a 500 that names nothing.
    pub async fn exists(&self, id: Uuid) -> anyhow::Result<bool> {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM node_groups WHERE id = $1")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        Ok(n > 0)
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

    /// Set (or clear with a `None` pair) this folder's map pin. Returns whether the group exists.
    ///
    /// The caller is responsible for the both-or-neither and range rules — half a coordinate pair
    /// is not a location. Written by `PUT /api/v1/node-groups/{id}/geo`, read back by
    /// [`Self::list`] and rendered by the dashboard's Geo map widget.
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

    /// A group row with only the fields geo resolution reads.
    fn geo_row(id: u128, parent: Option<u128>, coords: Option<(f64, f64)>) -> GroupSummary {
        GroupSummary {
            id: Uuid::from_u128(id),
            name: format!("g{id}"),
            group_type: GroupType::Generic.key().to_owned(),
            parent_id: parent.map(Uuid::from_u128),
            sort_order: 0.0,
            latitude: coords.map(|c| c.0),
            longitude: coords.map(|c| c.1),
            effective_latitude: None,
            effective_longitude: None,
            geo_source: GeoSource::Unset,
            geo_group: None,
            pool: None,
            prefixes: Vec::new(),
        }
    }

    /// `(effective lat, effective lon, source, supplying group)` for one row, for terse asserts.
    fn geo_of(
        groups: &[GroupSummary],
        id: u128,
    ) -> (Option<f64>, Option<f64>, GeoSource, Option<u128>) {
        let g = groups
            .iter()
            .find(|g| g.id == Uuid::from_u128(id))
            .expect("row present");
        (
            g.effective_latitude,
            g.effective_longitude,
            g.geo_source,
            g.geo_group.map(|id| id.as_u128()),
        )
    }

    #[test]
    fn a_subgroup_inherits_the_nearest_placed_ancestors_position() {
        // tokyo(placed) → floor2(unplaced) → rack7(unplaced): the whole chain resolves to tokyo's
        // pin. This is the bug the feature exists for — nodes live in racks, the operator places
        // the site, and before inheritance the site pin counted nothing.
        let mut groups = vec![
            geo_row(1, None, Some((35.68, 139.76))),
            geo_row(2, Some(1), None),
            geo_row(3, Some(2), None),
        ];
        resolve_group_geo(&mut groups);
        assert_eq!(
            geo_of(&groups, 1),
            (Some(35.68), Some(139.76), GeoSource::Own, Some(1)),
            "a placed group supplies its own position and names itself as the pin"
        );
        for id in [2, 3] {
            assert_eq!(
                geo_of(&groups, id),
                (Some(35.68), Some(139.76), GeoSource::Inherited, Some(1)),
                "group {id} resolves to the site pin"
            );
        }
    }

    #[test]
    fn the_nearest_placed_ancestor_wins_over_a_farther_one() {
        // region(placed) → site(placed) → rack(unplaced): the rack belongs to the site's pin, not
        // the region's. Nearest wins, exactly as pool and threshold inheritance do.
        let mut groups = vec![
            geo_row(1, None, Some((10.0, 10.0))),
            geo_row(2, Some(1), Some((20.0, 20.0))),
            geo_row(3, Some(2), None),
        ];
        resolve_group_geo(&mut groups);
        assert_eq!(
            geo_of(&groups, 3),
            (Some(20.0), Some(20.0), GeoSource::Inherited, Some(2))
        );
    }

    #[test]
    fn inheritance_never_adds_a_pin() {
        // The load-bearing property: however many groups inherit, the number of groups drawn is
        // still the number carrying their own coordinates. Thirty racks under one building must
        // not become thirty exactly-overlapping pins that hide the building.
        let mut groups = vec![geo_row(1, None, Some((35.0, 139.0)))];
        for i in 2..=31u128 {
            groups.push(geo_row(i, Some(1), None));
        }
        resolve_group_geo(&mut groups);
        assert_eq!(
            groups
                .iter()
                .filter(|g| g.geo_source == GeoSource::Own)
                .count(),
            1,
            "one placed group ⇒ one pin, regardless of how many descendants inherit"
        );
        assert_eq!(
            groups
                .iter()
                .filter(|g| g.geo_group == Some(Uuid::from_u128(1)))
                .count(),
            31,
            "every descendant is counted at that one pin"
        );
    }

    #[test]
    fn an_unplaced_chain_stays_off_the_map() {
        // Nothing placed anywhere, and a dangling parent_id, must both read as "not on the map" —
        // never as (0, 0), which is a real place in the Gulf of Guinea.
        let mut groups = vec![
            geo_row(1, None, None),
            geo_row(2, Some(1), None),
            geo_row(4, Some(9), None),
        ];
        resolve_group_geo(&mut groups);
        for id in [1, 2, 4] {
            assert_eq!(geo_of(&groups, id), (None, None, GeoSource::Unset, None));
        }
    }

    #[test]
    fn a_half_set_or_non_finite_coordinate_is_not_a_placement() {
        // A lone latitude is legacy or hand-edited data (the write path sets both or clears both);
        // treating it as placed would pin the group on the prime meridian. NaN/inf would project
        // to nowhere *and* poison the fit-to-view bounds computed across every other pin.
        let mut groups = vec![
            geo_row(1, None, None),
            geo_row(2, Some(1), None),
            geo_row(3, Some(1), None),
        ];
        groups[0].latitude = Some(35.0); // longitude left null
        groups[1].latitude = Some(f64::NAN);
        groups[1].longitude = Some(139.0);
        groups[2].latitude = Some(35.0);
        groups[2].longitude = Some(f64::INFINITY);
        resolve_group_geo(&mut groups);
        for id in [1, 2, 3] {
            assert_eq!(geo_of(&groups, id), (None, None, GeoSource::Unset, None));
        }
    }

    #[test]
    fn cyclic_ancestry_resolves_to_unplaced_without_hanging() {
        // `would_create_cycle` guards the endpoints that set a parent, but `delete`'s re-parenting
        // does not re-check and there is no DB constraint — so the resolver must survive one.
        let mut groups = vec![geo_row(1, Some(1), None)];
        resolve_group_geo(&mut groups);
        assert_eq!(geo_of(&groups, 1).2, GeoSource::Unset);

        let mut groups = vec![
            geo_row(1, Some(2), None),
            geo_row(2, Some(1), None),
            geo_row(3, None, Some((1.0, 2.0))),
        ];
        resolve_group_geo(&mut groups);
        assert_eq!(geo_of(&groups, 1).2, GeoSource::Unset);
        assert_eq!(geo_of(&groups, 2).2, GeoSource::Unset);
        assert_eq!(
            geo_of(&groups, 3),
            (Some(1.0), Some(2.0), GeoSource::Own, Some(3)),
            "a cycle elsewhere in the forest does not affect a healthy branch"
        );
    }

    #[test]
    fn resolution_is_idempotent() {
        // `list()` fills these on every read; running twice must not drift, and re-resolving rows
        // that already carry an answer must not mistake an inherited value for an own one.
        let mut groups = vec![
            geo_row(1, None, Some((35.0, 139.0))),
            geo_row(2, Some(1), None),
        ];
        resolve_group_geo(&mut groups);
        let once: Vec<_> = groups.iter().map(|g| (g.geo_source, g.geo_group)).collect();
        resolve_group_geo(&mut groups);
        let twice: Vec<_> = groups.iter().map(|g| (g.geo_source, g.geo_group)).collect();
        assert_eq!(once, twice);
        assert_eq!(geo_of(&groups, 2).2, GeoSource::Inherited);
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

    #[test]
    fn group_ancestors_walks_up_nearest_first_and_excludes_the_start() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let d = Uuid::from_u128(4);
        // a → b → d.
        let edges = vec![(a, None), (b, Some(a)), (d, Some(b))];
        assert_eq!(group_ancestors(&edges, d), vec![b, a]);
        assert_eq!(group_ancestors(&edges, b), vec![a]);
        assert_eq!(group_ancestors(&edges, a), Vec::<Uuid>::new()); // a root has none
    }

    #[test]
    fn group_ancestors_of_an_unknown_group_is_empty() {
        assert_eq!(group_ancestors(&[], Uuid::from_u128(9)), Vec::<Uuid>::new());
    }

    #[test]
    fn group_ancestors_terminates_on_a_cycle() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let edges = vec![(a, Some(b)), (b, Some(a))];
        // Walking up from `a` reaches `b`, then `a` again — which is the start, already seen.
        assert_eq!(group_ancestors(&edges, a), vec![b]);
    }

    // The two walks must agree about direction: nothing above a group may also be below it, or a
    // scoped caller's breadcrumb would quietly hand them a sibling subtree's membership.
    #[test]
    fn a_groups_ancestors_and_its_subtree_never_overlap() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let d = Uuid::from_u128(4);
        let edges = vec![(a, None), (b, Some(a)), (c, Some(a)), (d, Some(b))];
        let sub = group_subtree(&edges, b);
        for up in group_ancestors(&edges, b) {
            assert!(!sub.contains(&up), "{up} is both above and below b");
        }
    }
}
