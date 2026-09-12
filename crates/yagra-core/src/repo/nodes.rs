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

/// A node row plus its position among its siblings in the folder tree (ADR-133).
///
/// The two by-group reads already `ORDER BY sort_order` and then threw the value away, because
/// [`yagra_common::Node`] has no such field — so the caller asked a **second** query
/// ([`NodeRepo::node_sort_orders`]) for the order of the very rows it had just read. That second
/// read is one more PostgreSQL connection held concurrently on the hottest list in the product,
/// which is what `LIST_SEATS` is sized against.
///
/// 🚨 **A wrapper here rather than a field on `Node`.** `Node` is `yagra-common`, shared by the
/// poller, the bus and every crate that speaks about a device; `sort_order` is a fact about where
/// an operator dragged a row in one particular tree, and putting it there would ship it over the
/// wire to every poller for every job. The column stays inside the one file allowed to name the
/// `nodes` table.
#[derive(Debug, Clone)]
pub struct OrderedNode {
    pub node: Node,
    pub sort_order: f64,
}

/// A node row plus the operator's own free-text note (ADR-135 decision 2).
///
/// 🚨 **A wrapper here rather than a field on `Node`, for the reason [`OrderedNode`] gives one
/// paragraph up — and the number is worse.** `Node` is materialized *fleet-wide* by the alert
/// engine's config snapshot, the scheduler's sweep and maintenance-window matching. A note is up
/// to 2,000 characters that **none of those three read**; on a
/// 50,000-node deployment that is up to 100 MB of prose carried through every cached snapshot so
/// that one page at a time can display it.
///
/// So the column stays inside the one file allowed to name the `nodes` table, it is **not** in
/// [`NodeRepo::NODE_COLUMNS`], and exactly two callers read it: the REST node detail and the MCP
/// `get_node_status` tool that mirrors it.
#[derive(Debug, Clone)]
pub struct NodeWithNotes {
    pub node: Node,
    /// `None` ⇒ no note. The API edge maps a whitespace-only string to `None`, so there is one
    /// spelling of "no note" and no reader needs a second case.
    pub notes: Option<String>,
}

/// What one "Edit node" save writes (ADR-135).
///
/// A struct rather than nine positional arguments: `clippy::too_many_arguments` is a design
/// signal, not a lint to silence (`coding-conventions.md`), and five of these are
/// `Option<Option<&str>>` — a shape no reader should have to count commas through.
///
/// 🚨 **Two different readings of `None` live here, and mixing them up is a data-loss bug.**
/// The first four fields are *replacements*: a `None` CLEARS the column, because the edit
/// dialog loads the current values and resends all of them. The last three are *three-state*:
/// the outer `None` means LEAVE ALONE. See [`NodeRepo::set_node_bindings`].
#[derive(Debug, Default)]
pub struct NodeBindingUpdate<'a> {
    // ── Replaced unconditionally (None clears) ──
    pub profile: Option<Uuid>,
    pub credential: Option<Uuid>,
    pub vendor: Option<&'a str>,
    pub model: Option<&'a str>,
    // ── Three-state (outer None leaves the column alone) ──
    /// Validated by the caller as a NATS-subject-safe token.
    pub pool: Option<Option<&'a str>>,
    /// Inner `None` is unreachable — `nodes.name` is `NOT NULL`, so the API edge answers 400
    /// for an empty name rather than passing a clear down here. Spelled the same as its two
    /// neighbours anyway, so the UPDATE applies one shape three times rather than
    /// three shapes.
    pub name: Option<Option<&'a str>>,
    pub notes: Option<Option<&'a str>>,
    /// The node's whole label set, **replaced**. `None` leaves the column alone; an empty slice
    /// clears it. Merging one label into many nodes is [`NodeRepo::merge_node_tags`], a different
    /// question.
    pub tags: Option<&'a [String]>,
    /// Labels the node refuses to inherit from its folder chain, **replaced** on the same terms
    /// (ADR-135 inc. 2). `None` leaves the column alone; an empty slice clears it.
    ///
    /// ⚠️ Only inherited labels can be excluded — a label the node carries itself is removed by
    /// dropping it from `tags`, not by adding it here. An entry naming a label nothing currently
    /// supplies is inert and kept deliberately: if an ancestor re-adds it later, the operator's
    /// "not here" still holds.
    pub tags_excluded: Option<&'a [String]>,
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

    /// One node by id **plus its note** (ADR-135). For the two detail surfaces only.
    ///
    /// A second projection rather than a column on [`Self::NODE_COLUMNS`]: see [`NodeWithNotes`]
    /// for why the note must not ride along on every fleet-wide read. The column list is
    /// interpolated from the same constant, so the two cannot disagree about the node half.
    pub async fn get_node_with_notes(&self, id: Uuid) -> anyhow::Result<Option<NodeWithNotes>> {
        let row = sqlx::query(&format!(
            "SELECT {}, notes FROM nodes WHERE id = $1",
            Self::NODE_COLUMNS
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref()
            .map(|row| {
                Ok(NodeWithNotes {
                    node: node_from_row(row)?,
                    notes: row.try_get("notes")?,
                })
            })
            .transpose()
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
            // `sort_order` is computed the same way `set_node_group` does, rather than left at
            // the column default: every imported node would otherwise share one value and sort
            // ahead of whatever the operator had already placed in that folder. Inside the
            // transaction each row sees the previous one, so a batch lands in order.
            sqlx::query(
                "INSERT INTO nodes \
                   (id, name, address, profile_id, credential_id, vendor, model, group_id, sort_order) \
                 VALUES ($1, $2, $3::inet, $4, $5, $6, $7, $8, \
                   (SELECT COALESCE(MAX(sort_order), 0) + 1 FROM nodes \
                     WHERE group_id IS NOT DISTINCT FROM $8::uuid))",
            )
            .bind(Uuid::new_v4())
            .bind(n.name)
            .bind(n.address.to_string())
            .bind(n.profile)
            .bind(n.credential)
            .bind(n.vendor)
            .bind(n.model)
            .bind(n.group)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(nodes.len() as u32)
    }

    /// Apply one "Edit node" save. Returns whether the node exists.
    ///
    /// **Why the last three are gated and the first four are not**, since the asymmetry looks
    /// arbitrary and is not:
    ///
    /// `profile`/`credential`/`vendor`/`model` have been full replacements since they shipped, and
    /// a caller that omits one blanks it. That is survivable for `vendor`/`model` because
    /// [`Self::fill_node_identity_batch`] refills them from the next poll's `sysDescr`.
    ///
    /// 🚨 **Nothing refills a name or a note.** A caller that omits `notes` — an older WebUI bundle
    /// still open in a tab during a rolling upgrade, an API client written against N-1 — would
    /// silently destroy operator-authored text with no way back. So those two join `pool` in the
    /// three-state reading the `NodeBindings` DTO documents: **absent means leave alone**, and the
    /// only way to clear one is to say so with an empty string.
    pub async fn set_node_bindings(
        &self,
        id: Uuid,
        u: NodeBindingUpdate<'_>,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE nodes SET profile_id = $2, credential_id = $3, vendor = $4, model = $5, \
             pool  = CASE WHEN $6::boolean  THEN $7::text  ELSE pool  END, \
             name  = CASE WHEN $8::boolean  THEN $9::text  ELSE name  END, \
             notes = CASE WHEN $10::boolean THEN $11::text ELSE notes END, \
             tags  = CASE WHEN $12::boolean THEN $13::text[] ELSE tags END, \
             tags_excluded = CASE WHEN $14::boolean THEN $15::text[] ELSE tags_excluded END, \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(u.profile)
        .bind(u.credential)
        .bind(u.vendor)
        .bind(u.model)
        // Each pair is (touch this column at all?, the new value — NULL when clearing).
        .bind(u.pool.is_some())
        .bind(u.pool.flatten())
        .bind(u.name.is_some())
        .bind(u.name.flatten())
        .bind(u.notes.is_some())
        .bind(u.notes.flatten())
        .bind(u.tags.is_some())
        .bind(u.tags.unwrap_or(&[]))
        .bind(u.tags_excluded.is_some())
        .bind(u.tags_excluded.unwrap_or(&[]))
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Add and/or remove tags across MANY nodes at once, **merging** rather than replacing
    /// (ADR-135 decision 7). Returns `(requested, applied)`.
    ///
    /// 🚨 **Merge, not replace, and the difference is the whole reason this is its own method.**
    /// The caller selected nodes in the tree and knows one label it wants on all of them; it does
    /// **not** know what else each of them carries. Only the labels named are added or taken away.
    ///
    /// 🚨 **The three things `jsonb` gave for free and `text[]` does not** (ADR-135 inc. 2):
    ///
    /// * `COALESCE(array_agg(…), '{}')` — `array_agg` over zero rows returns **NULL**, so removing
    ///   a node's last label would otherwise write NULL into a `NOT NULL` column. It fails loudly
    ///   *because* the column is `NOT NULL`; on a nullable one it would silently make every
    ///   reader's `try_get::<Vec<String>>` fail instead.
    /// * `DISTINCT … ORDER BY` — `array ||` concatenates, where `jsonb ||` merged by key. Without
    ///   it, adding a label a node already carries stores it twice, and every badge, every
    ///   `contains` and every RCA fingerprint then sees the duplicate.
    /// * `t <> ALL($3)` — correct on an empty array (removes nothing) and cannot see a NULL,
    ///   because the bind is a `Vec<String>`.
    ///
    /// ⚠️ **Do not add `AND tags IS DISTINCT FROM (the new value)` to skip no-ops.**
    /// `rows_affected()` *is* the `applied` count the API reports, so suppressing a no-op would
    /// make `applied < requested` for a node that already carried the label — which reads to the
    /// operator as a partial failure.
    ///
    /// ⚠️ **This path cannot enforce the per-node label cap** and does not try: it does not know
    /// what each node already carries, and enforcing it in SQL would silently skip some rows,
    /// which would give `applied` two meanings. The edit dialog can always save a node back under
    /// the cap, so an over-cap node is never stuck.
    ///
    /// `scope` narrows which nodes may be written: a caller restricted to some folders cannot
    /// label a node they cannot see. The predicate is written out rather than reusing
    /// [`NodeRepo::SCOPE_PREDICATE`], which is bound to `$1` while this statement needs `$1` for
    /// the id array — the same shape, and the same fail-closed reading of an empty slice, as
    /// [`Self::set_node_group_batch`].
    ///
    /// `applied` lower than `requested` is normal and not an error: an id can name a node that has
    /// since been deleted, or one outside the caller's scope. The two are not distinguished, for
    /// the reason the bulk move states — telling them apart would confirm that a node the caller
    /// may not see exists.
    pub async fn merge_node_tags(
        &self,
        ids: &[Uuid],
        add: &[String],
        remove: &[String],
        scope: GroupFilter<'_>,
    ) -> anyhow::Result<(usize, u64)> {
        let mut seen = std::collections::HashSet::new();
        let ids: Vec<Uuid> = ids.iter().copied().filter(|id| seen.insert(*id)).collect();
        if ids.is_empty() {
            return Ok((0, 0));
        }
        let res = sqlx::query(
            "UPDATE nodes SET tags = ( \
                   SELECT COALESCE(array_agg(DISTINCT t ORDER BY t), '{}') \
                     FROM unnest(tags || $2::text[]) AS t \
                    WHERE t <> ALL ($3::text[])), \
                 updated_at = now() \
             WHERE id = ANY($1) AND ($4::uuid[] IS NULL OR group_id = ANY($4))",
        )
        .bind(&ids)
        .bind(add)
        .bind(remove)
        .bind(Self::scope_bind(scope))
        .execute(&self.pool)
        .await?;
        Ok((ids.len(), res.rows_affected()))
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

    /// Move MANY nodes into one group (or `None` to ungroup them) in a single statement, appending
    /// them to the end of the destination scope **in the order given**. Returns
    /// `(requested, moved)` — the de-duplicated id count, and how many rows actually moved.
    ///
    /// ⚠️ **The `sort_order` base excludes the nodes being moved**, exactly as the single-node
    /// [`Self::set_node_group`] excludes the one node it moves (`id <> $1`). Without that, a node
    /// already sitting in the destination raises the base with its own value and the batch lands
    /// after itself — which reads as "the order jumped" and is invisible in any unit test that
    /// starts from an empty destination.
    ///
    /// ⚠️ **Ids are de-duplicated keeping the first occurrence.** A repeated id becomes two rows
    /// out of `WITH ORDINALITY` and gives that node a non-deterministic order. The order is
    /// preserved rather than sorted: what arrives is the operator's tree order.
    ///
    /// `moved < requested` is normal and not an error — an id can be stale (the node was deleted
    /// between the page load and the click). The caller reports **both** numbers rather than
    /// claiming a count it did not achieve (ADR-124 決定 7).
    ///
    /// `scope` narrows which nodes may move: a caller restricted to some folders cannot pull a
    /// node out of one they cannot see. ⚠️ **The predicate is written out rather than reusing
    /// [`Self::SCOPE_PREDICATE`]**, which is bound to `$1` and this statement needs `$1` for the
    /// id array. Same shape, same fail-closed reading of an empty slice (match nothing).
    /// A node refused by the scope is indistinguishable from a deleted one in the count — both
    /// simply do not move — which the endpoint's doc says out loud.
    pub async fn set_node_group_batch(
        &self,
        ids: &[Uuid],
        group: Option<Uuid>,
        scope: GroupFilter<'_>,
    ) -> anyhow::Result<(usize, u64)> {
        let mut seen = std::collections::HashSet::new();
        let ids: Vec<Uuid> = ids.iter().copied().filter(|id| seen.insert(*id)).collect();
        if ids.is_empty() {
            return Ok((0, 0));
        }
        let res = sqlx::query(
            "UPDATE nodes SET group_id = $2, updated_at = now(), \
             sort_order = (SELECT COALESCE(MAX(peer.sort_order), 0) FROM nodes AS peer \
                           WHERE peer.group_id IS NOT DISTINCT FROM $2::uuid \
                             AND NOT (peer.id = ANY($1))) + t.ord::double precision \
             FROM unnest($1::uuid[]) WITH ORDINALITY AS t(id, ord) \
             WHERE nodes.id = t.id \
               AND ($3::uuid[] IS NULL OR nodes.group_id = ANY($3))",
        )
        .bind(&ids)
        .bind(group)
        .bind(Self::scope_bind(scope))
        .execute(&self.pool)
        .await?;
        Ok((ids.len(), res.rows_affected()))
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
    /// pulls the whole fleet.
    ///
    /// 🚨 **The `NULL` case is a separate statement, and that is the whole point** (ADR-133).
    /// One query reading `group_id IS NOT DISTINCT FROM $2::uuid` is the obvious spelling and
    /// PostgreSQL cannot use a btree index for it — `IS NOT DISTINCT FROM` is not the equality
    /// operator `nodes_group_idx` is built on, so the planner reaches for a sequential scan even
    /// for an unscoped caller. That matters because the inventory tree asks for the ungrouped
    /// bucket **on every render** (ADR-125 決定 1, unconditionally — the bucket's header counts
    /// from what is loaded, not from the server rollup), so the cost is paid per tree, per viewer,
    /// and grows with the fleet rather than with the answer.
    ///
    /// Measured on 10,032 nodes, none of them ungrouped:
    ///
    /// | | plan | rows scanned | shared buffers | execution |
    /// |---|---|---|---|---|
    /// | `IS NOT DISTINCT FROM` | Seq Scan | 10,032 | 166 | 0.636 ms |
    /// | `IS NULL` | Index Scan (`nodes_group_idx`) | 0 | 2 | 0.089 ms |
    ///
    /// ⚠️ **An index alone would not have fixed it** — this is a shape problem, not a missing
    /// index, and ADR-125 recorded that while deferring the index it never shipped.
    ///
    /// The scope predicate rides alongside the group filter rather than replacing it: asking for
    /// the ungrouped bucket (`group = None`) as a scoped caller correctly returns nothing, because
    /// an ungrouped node is outside every group scope (`rbac.rs`).
    pub async fn list_nodes_in_group(
        &self,
        groups: GroupFilter<'_>,
        group: Option<Uuid>,
        limit: i64,
    ) -> anyhow::Result<Vec<OrderedNode>> {
        let limit = limit.clamp(1, 5001);
        // Two statements, one `$2`: the bucket takes the cap there, a named folder takes the id.
        // Written as two literals rather than one built from a flag, so each is greppable as the
        // thing the planner sees.
        let rows = match group {
            None => {
                sqlx::query(&format!(
                    "SELECT {}, sort_order FROM nodes \
                     WHERE {} AND group_id IS NULL \
                     ORDER BY sort_order, name, id LIMIT $2",
                    Self::NODE_COLUMNS,
                    Self::SCOPE_PREDICATE
                ))
                .bind(Self::scope_bind(groups))
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            Some(id) => {
                sqlx::query(&format!(
                    "SELECT {}, sort_order FROM nodes \
                     WHERE {} AND group_id = $2 \
                     ORDER BY sort_order, name, id LIMIT $3",
                    Self::NODE_COLUMNS,
                    Self::SCOPE_PREDICATE
                ))
                .bind(Self::scope_bind(groups))
                .bind(id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };
        rows.iter().map(ordered_node_from_row).collect()
    }

    /// The direct member nodes of **several** groups at once, ordered by group then by the tree's
    /// sort order, capped at `limit` across the whole answer (ADR-125).
    ///
    /// The inventory tree asks for the folders in its viewport, which is tens of them — one request
    /// each meant tens of round trips and, worse, tens of runs of `build_node_summaries` (five
    /// reads apiece). Batching collapses that to one of each.
    ///
    /// 🚨 **`= ANY($2)`, deliberately not `IS NOT DISTINCT FROM`.** PostgreSQL does not treat the
    /// latter as the btree equality operator, so the single-group form above cannot use
    /// `nodes_group_idx` even for an unscoped caller — adding an index would not have helped it.
    /// This form can. The ungrouped bucket keeps the other method precisely because `NULL` is not a
    /// value `= ANY` can match, and pretending otherwise is what the `IS NOT DISTINCT FROM` there
    /// is for.
    ///
    /// ⚠️ `ORDER BY group_id` first so a truncated answer is truncated at a folder boundary rather
    /// than mixing a partial folder into the middle of the list.
    pub async fn list_nodes_in_groups(
        &self,
        groups: GroupFilter<'_>,
        group_ids: &[Uuid],
        limit: i64,
    ) -> anyhow::Result<Vec<OrderedNode>> {
        if group_ids.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, 5001);
        let rows = sqlx::query(&format!(
            "SELECT {}, sort_order FROM nodes \
             WHERE {} AND group_id = ANY($2) \
             ORDER BY group_id, sort_order, name, id LIMIT $3",
            Self::NODE_COLUMNS,
            Self::SCOPE_PREDICATE
        ))
        .bind(Self::scope_bind(groups))
        .bind(group_ids)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(ordered_node_from_row).collect()
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
            // `group_id` and `tags_excluded` ride along for the label resolution the caller does:
            // a node's effective labels are `(what its folder chain supplies - tags_excluded) +
            // tags`, and the folder chain is not reachable from this query (ADR-135 inc. 2).
            "SELECT n.id, n.name, host(n.address) AS address, g.name AS group_name, \
                    p.name AS profile_name, n.tags, n.tags_excluded, n.group_id \
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
                        tags: row.try_get("tags")?,
                        tags_excluded: row.try_get("tags_excluded")?,
                        group_id: row.try_get("group_id")?,
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
        // A freshly created node has no note, and the detail projection says so rather than
        // failing: `get_node_with_notes` reads a column `create_node` never writes.
        let detail = repo
            .get_node_with_notes(id)
            .await
            .expect("read")
            .expect("the node");
        assert_eq!(detail.notes, None);
        assert_eq!(detail.node.name, "core-sw-01");
    }

    /// Renaming a node changes the name and **nothing else** — in particular not its id.
    ///
    /// The id is what every store is keyed by, so a rename that re-keyed the row would silently
    /// orphan the node's metric series and its alert history. Nothing in the product could notice:
    /// the new row would simply have no past.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn renaming_a_node_keeps_its_id_and_every_other_column(pool: sqlx::PgPool) {
        let repo = pgtest::repo(pool);
        let id = repo
            .create_node(
                "typo-sw-01",
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

        assert!(repo
            .set_node_bindings(
                id,
                NodeBindingUpdate {
                    vendor: Some("Cisco"),
                    model: Some("C9300"),
                    name: Some(Some("core-sw-01")),
                    ..Default::default()
                },
            )
            .await
            .expect("rename"));

        let node = repo.get_node(id).await.expect("read").expect("the node");
        assert_eq!(node.name, "core-sw-01");
        assert_eq!(node.id, yagra_common::NodeId::from(id), "the id moved");
        assert_eq!(node.address.to_string(), "10.1.2.3");
        assert_eq!(node.vendor.as_deref(), Some("Cisco"));
        // The pool was not mentioned, so it is untouched — the same three-state rule the name uses.
        assert_eq!(node.pool.as_deref(), Some("edge"));
    }

    /// 🚨 **The whole point of the three-state reading, and it needs both directions.**
    ///
    /// A save that does not mention `notes` must leave the note alone; a save that sends an empty
    /// string must clear it. A test with only the first half passes just as well against an
    /// implementation that ignores the field entirely, and a test with only the second half passes
    /// against one that clears on every write — which is the data-loss bug this shape exists to
    /// prevent (`closing-a-check-must-be-tested-with-reopening`).
    ///
    /// The same is asserted for `name`, where "leave alone" is the only safe reading of absence
    /// because the column is `NOT NULL`.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_unmentioned_note_survives_a_save_and_an_empty_one_clears_it(pool: sqlx::PgPool) {
        let id = pgtest::node(&pool, "sw-1", 1, None).await;
        let repo = pgtest::repo(pool);
        // Write one.
        assert!(repo
            .set_node_bindings(
                id,
                NodeBindingUpdate {
                    notes: Some(Some("in the ceiling void; needs a ladder")),
                    ..Default::default()
                },
            )
            .await
            .expect("set note"));
        let written = repo
            .get_node_with_notes(id)
            .await
            .expect("read")
            .expect("the node");
        assert_eq!(
            written.notes.as_deref(),
            Some("in the ceiling void; needs a ladder")
        );

        // A save that says nothing about notes — what an older client sends — leaves it alone.
        assert!(repo
            .set_node_bindings(
                id,
                NodeBindingUpdate {
                    pool: Some(Some("edge")),
                    ..Default::default()
                },
            )
            .await
            .expect("set pool"));
        let after = repo
            .get_node_with_notes(id)
            .await
            .expect("read")
            .expect("the node");
        assert_eq!(
            after.notes.as_deref(),
            Some("in the ceiling void; needs a ladder"),
            "an unmentioned note was destroyed by an unrelated save"
        );
        assert_eq!(after.node.pool.as_deref(), Some("edge"));
        assert_eq!(after.node.name, "sw-1", "an unmentioned name was rewritten");

        // An explicit clear does clear it — otherwise "leave alone" would be indistinguishable
        // from "this column is write-once".
        assert!(repo
            .set_node_bindings(
                id,
                NodeBindingUpdate {
                    notes: Some(None),
                    ..Default::default()
                },
            )
            .await
            .expect("clear note"));
        let cleared = repo
            .get_node_with_notes(id)
            .await
            .expect("read")
            .expect("the node");
        assert_eq!(cleared.notes, None);
    }

    /// The same three-state reading, for the two label lists (ADR-135 inc. 2).
    ///
    /// 🚨 Both directions, for the reason the note's twin above states: a test with only the "leave
    /// alone" half passes against an implementation that ignores the field, and one with only the
    /// "clear it" half passes against one that clears on every write. `tags` is the field most
    /// exposed to the second — every save the edit dialog makes carries it, and a caller that omits
    /// it (an older WebUI tab mid-upgrade) must not strip a node bare.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn unmentioned_labels_survive_a_save_and_an_empty_list_clears_them(pool: sqlx::PgPool) {
        let id = pgtest::node(&pool, "sw-1", 1, None).await;
        let repo = pgtest::repo(pool);
        let tags = vec!["JAPAN".to_owned(), "core".to_owned()];
        let excluded = vec!["noisy".to_owned()];
        assert!(repo
            .set_node_bindings(
                id,
                NodeBindingUpdate {
                    tags: Some(&tags),
                    tags_excluded: Some(&excluded),
                    ..Default::default()
                },
            )
            .await
            .expect("set labels"));

        // A save that mentions neither — the shape a pool-only edit takes.
        assert!(repo
            .set_node_bindings(
                id,
                NodeBindingUpdate {
                    pool: Some(Some("edge")),
                    ..Default::default()
                },
            )
            .await
            .expect("pool only"));
        let kept = repo.get_node(id).await.expect("read").expect("the node");
        assert_eq!(kept.tags, tags, "an unmentioned label list was destroyed");
        assert_eq!(kept.tags_excluded, excluded);
        assert_eq!(kept.pool.as_deref(), Some("edge"));

        // …and an empty list is a clear, not a no-op: otherwise removing the last label would look
        // like it worked and come back on the next read.
        assert!(repo
            .set_node_bindings(
                id,
                NodeBindingUpdate {
                    tags: Some(&[]),
                    tags_excluded: Some(&[]),
                    ..Default::default()
                },
            )
            .await
            .expect("clear labels"));
        let cleared = repo.get_node(id).await.expect("read").expect("the node");
        assert!(cleared.tags.is_empty());
        assert!(cleared.tags_excluded.is_empty());
    }

    /// 🚨 **A bulk tag edit MERGES. The label a caller never mentioned must survive it.**
    ///
    /// This is the whole reason `merge_node_tags` exists beside the replacing path: the tree gives
    /// the caller a set of ids and one label, and it has no idea what else those nodes carry. A
    /// replacing implementation passes any test that starts from an untagged node — so this one
    /// starts from a node that already has two labels, and checks both of them afterwards.
    ///
    /// Removal is asserted too, and in the same run: a merge that could only add would make the ✕
    /// in the bulk dialog do nothing, silently.
    ///
    /// 🚨 Since ADR-135 inc. 2 it also asserts **de-duplication** and **that the last label leaves
    /// an empty array rather than NULL**. `jsonb ||` merged by key and gave both for free; `text[]`
    /// `||` concatenates, and `array_agg` over no rows returns NULL into a `NOT NULL` column.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_bulk_tag_merges_and_leaves_unmentioned_labels_alone(pool: sqlx::PgPool) {
        let a = pgtest::node(&pool, "a", 1, None).await;
        let b = pgtest::node(&pool, "b", 2, None).await;
        let repo = pgtest::repo(pool);
        let existing = vec!["core".to_owned(), "neteng".to_owned()];
        assert!(repo
            .set_node_bindings(
                a,
                NodeBindingUpdate {
                    tags: Some(&existing),
                    ..Default::default()
                },
            )
            .await
            .expect("seed tags"));

        // `core` is deliberately re-added: on `text[]` the concat would store it twice.
        let add = vec!["JAPAN".to_owned(), "core".to_owned()];
        let (requested, applied) = repo
            .merge_node_tags(&[a, b, a], &add, &[], None)
            .await
            .expect("merge");
        assert_eq!(requested, 2, "the repeated id was not de-duplicated");
        assert_eq!(applied, 2);

        // ⚠️ Compared as **sets**. This list is ordered by the statement's own `ORDER BY`, so its
        // order is the test database's collation — `C` puts `JAPAN` first, `en_US.utf8` does not.
        // What is under test is the merge and the de-duplication, and pinning a collation here
        // would fail on a database that is not wrong.
        let sorted = |mut v: Vec<String>| {
            v.sort_unstable();
            v
        };
        let tagged = repo.get_node(a).await.expect("read").expect("a");
        assert_eq!(
            sorted(tagged.tags),
            vec!["JAPAN".to_owned(), "core".to_owned(), "neteng".to_owned()],
            "a label nobody mentioned was destroyed, or a re-added one was stored twice"
        );
        // The node that started empty got exactly the two labels, once each.
        let fresh = repo.get_node(b).await.expect("read").expect("b");
        assert_eq!(
            sorted(fresh.tags),
            vec!["JAPAN".to_owned(), "core".to_owned()]
        );

        // Removing names labels, and touches only those.
        let (_, applied) = repo
            .merge_node_tags(&[a], &[], &["core".to_owned()], None)
            .await
            .expect("remove");
        assert_eq!(applied, 1);
        let after = repo.get_node(a).await.expect("read").expect("a");
        assert_eq!(
            sorted(after.tags),
            vec!["JAPAN".to_owned(), "neteng".to_owned()],
            "removal took a label it was not asked to"
        );

        // 🚨 The last one out must leave `{}`, not NULL. A NULL here is a `NOT NULL` violation that
        // would surface as a 500 from a handler nobody would connect to this statement.
        let (_, applied) = repo
            .merge_node_tags(&[b], &[], &["JAPAN".to_owned(), "core".to_owned()], None)
            .await
            .expect("remove every label");
        assert_eq!(applied, 1);
        let emptied = repo.get_node(b).await.expect("read").expect("b");
        assert!(emptied.tags.is_empty());
    }

    /// 🚨 **A group-scoped caller cannot relabel a node outside its folders.**
    ///
    /// ADR-135's own remnant recorded that this was never written: the predicate was copied from
    /// `move_nodes` and believed. A scope test that only ever checks the *unscoped* call proves
    /// nothing — this one drives the same statement twice, once with a scope that admits the node
    /// and once with one that does not, so "the predicate is ignored" and "the predicate refuses
    /// everything" are both visible.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_bulk_tag_leaves_nodes_outside_the_scope_alone(pool: sqlx::PgPool) {
        let mine = uuid::Uuid::new_v4();
        let theirs = uuid::Uuid::new_v4();
        for (id, name) in [(mine, "mine"), (theirs, "theirs")] {
            sqlx::query("INSERT INTO node_groups (id, name, group_type) VALUES ($1, $2, 'site')")
                .bind(id)
                .bind(name)
                .execute(&pool)
                .await
                .expect("seed group");
        }
        let a = pgtest::node(&pool, "a", 1, Some(mine)).await;
        let b = pgtest::node(&pool, "b", 2, Some(theirs)).await;
        let repo = pgtest::repo(pool);

        let add = vec!["JAPAN".to_owned()];
        let (requested, applied) = repo
            .merge_node_tags(&[a, b], &add, &[], Some(&[mine]))
            .await
            .expect("scoped merge");
        assert_eq!(requested, 2, "both ids were asked for");
        assert_eq!(applied, 1, "the node outside the scope must not be written");
        assert_eq!(repo.get_node(a).await.expect("read").expect("a").tags, add);
        assert!(
            repo.get_node(b)
                .await
                .expect("read")
                .expect("b")
                .tags
                .is_empty(),
            "a caller scoped to one folder relabelled a node in another"
        );

        // And the accept side: unscoped, the same call reaches both. Without this the test would
        // pass on an implementation that refuses everything.
        let (_, applied) = repo
            .merge_node_tags(&[a, b], &add, &[], None)
            .await
            .expect("unscoped merge");
        assert_eq!(applied, 2);
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
                group: None,
            },
            NewNode {
                name: "imported-2",
                address: "10.9.0.2".parse().expect("address"),
                profile: None,
                credential: None,
                vendor: None,
                model: None,
                group: None,
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

    /// A batch lands in the order it was given, appended after what is already there.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_batch_move_appends_in_the_order_it_was_given(pool: sqlx::PgPool) {
        let repo = pgtest::repo(pool.clone());
        let dest = pgtest::group(&pool, "Tokyo").await;
        let a = pgtest::node(&pool, "a", 1, None).await;
        let b = pgtest::node(&pool, "b", 2, None).await;
        let c = pgtest::node(&pool, "c", 3, None).await;

        let (requested, moved) = repo
            .set_node_group_batch(&[c, a, b], Some(dest), None)
            .await
            .expect("move");
        assert_eq!((requested, moved), (3, 3));

        let order = repo.node_sort_orders(&[a, b, c]).await.expect("orders");
        let of = |id: uuid::Uuid| *order.get(&id).expect("an order");
        assert!(
            of(c) < of(a) && of(a) < of(b),
            "input order was not preserved: {order:?}"
        );
    }

    /// 🚨 The base excludes the nodes being moved.
    ///
    /// A node already sitting in the destination must not raise the base with its own value —
    /// which is invisible in any test that starts from an empty destination, and is why the
    /// single-node writer beside this one carries `AND id <> $1`.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_node_already_in_the_destination_does_not_push_the_batch_past_itself(
        pool: sqlx::PgPool,
    ) {
        let repo = pgtest::repo(pool.clone());
        let dest = pgtest::group(&pool, "Tokyo").await;
        let sitting = pgtest::node(&pool, "sitting", 1, Some(dest)).await;
        let a = pgtest::node(&pool, "a", 2, None).await;

        // Move both — the one already there, and one from outside.
        repo.set_node_group_batch(&[sitting, a], Some(dest), None)
            .await
            .expect("move");

        let order = repo.node_sort_orders(&[sitting, a]).await.expect("orders");
        let of = |id: uuid::Uuid| *order.get(&id).expect("an order");
        assert!(
            of(sitting) < of(a),
            "the batch did not land in its own order: {order:?}"
        );
        assert!(
            of(a) <= 2.0,
            "the base counted a node that was itself moving, so the batch landed at {}",
            of(a)
        );
    }

    /// A stale id is not an error: it simply does not move, and both counts say so.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_batch_reports_how_many_of_the_named_nodes_it_actually_moved(pool: sqlx::PgPool) {
        let repo = pgtest::repo(pool.clone());
        let dest = pgtest::group(&pool, "Tokyo").await;
        let a = pgtest::node(&pool, "a", 1, None).await;
        let gone = uuid::Uuid::new_v4();

        let (requested, moved) = repo
            .set_node_group_batch(&[a, gone], Some(dest), None)
            .await
            .expect("move");
        assert_eq!(requested, 2, "both ids were asked for");
        assert_eq!(moved, 1, "only one of them exists");
    }

    /// A repeated id is one node, not two rows out of `WITH ORDINALITY`.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_repeated_id_is_moved_once(pool: sqlx::PgPool) {
        let repo = pgtest::repo(pool.clone());
        let dest = pgtest::group(&pool, "Tokyo").await;
        let a = pgtest::node(&pool, "a", 1, None).await;

        let (requested, moved) = repo
            .set_node_group_batch(&[a, a, a], Some(dest), None)
            .await
            .expect("move");
        assert_eq!((requested, moved), (1, 1), "the id was counted three times");
    }

    /// The scope narrows which nodes may move, not which folder they may reach.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_batch_move_leaves_nodes_outside_the_scope_where_they_are(pool: sqlx::PgPool) {
        let repo = pgtest::repo(pool.clone());
        let mine = pgtest::group(&pool, "Mine").await;
        let theirs = pgtest::group(&pool, "Theirs").await;
        let dest = pgtest::group(&pool, "Tokyo").await;
        let ours = pgtest::node(&pool, "ours", 1, Some(mine)).await;
        let hidden = pgtest::node(&pool, "hidden", 2, Some(theirs)).await;

        let (requested, moved) = repo
            .set_node_group_batch(&[ours, hidden], Some(dest), Some(&[mine, dest]))
            .await
            .expect("move");
        assert_eq!((requested, moved), (2, 1), "the scope did not hold");
        let still = repo
            .get_node(hidden)
            .await
            .expect("read")
            .expect("the node");
        assert_eq!(
            still.group.map(|g| g.0),
            Some(theirs),
            "an out-of-scope node was moved"
        );
    }

    /// An empty batch is a no-op, and does not go near the database.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_empty_batch_moves_nothing(pool: sqlx::PgPool) {
        let repo = pgtest::repo(pool.clone());
        let dest = pgtest::group(&pool, "Tokyo").await;
        assert_eq!(
            repo.set_node_group_batch(&[], Some(dest), None)
                .await
                .expect("move"),
            (0, 0)
        );
    }

    /// 🚨 **The batch read must return exactly what the single reads return** (ADR-125).
    /// `list_nodes_in_groups` is a second implementation of `list_nodes_in_group` — a different
    /// `ORDER BY`, and since ADR-133 the single read is itself two statements (`IS NULL` for the
    /// bucket, `= $2` for a named folder, so the planner can use `nodes_group_idx` for both). That
    /// is now **three** spellings of one question, written so the tree can ask about a viewport in
    /// one round trip and still ask about the bucket. Three implementations is a mirror, and a
    /// mirror needs the test that fails when they drift (`extensibility.md` §2). Nothing else would
    /// notice: all three return a plausible list of nodes.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_batch_read_returns_what_the_single_reads_return(pool: sqlx::PgPool) {
        let tokyo = pgtest::group(&pool, "Tokyo").await;
        let osaka = pgtest::group(&pool, "Osaka").await;
        let empty = pgtest::group(&pool, "Empty").await;
        let a = pgtest::node(&pool, "a", 1, Some(tokyo)).await;
        let b = pgtest::node(&pool, "b", 2, Some(tokyo)).await;
        let c = pgtest::node(&pool, "c", 1, Some(osaka)).await;
        let ungrouped = pgtest::node(&pool, "z", 1, None).await;
        let repo = pgtest::repo(pool);

        let ids = |v: Vec<OrderedNode>| {
            v.into_iter()
                .map(|o| o.node.id.as_uuid())
                .collect::<Vec<_>>()
        };
        let one = |g| {
            let repo = &repo;
            async move {
                ids(repo
                    .list_nodes_in_group(None, Some(g), 100)
                    .await
                    .expect("one"))
            }
        };

        // Folder by folder, then the same three folders in one call.
        assert_eq!(one(tokyo).await, vec![a, b]);
        assert_eq!(one(osaka).await, vec![c]);
        assert_eq!(one(empty).await, Vec::<Uuid>::new());
        let batched = ids(repo
            .list_nodes_in_groups(None, &[tokyo, osaka, empty], 100)
            .await
            .expect("batch"));
        let mut expected = vec![a, b, c];
        expected.sort();
        let mut got = batched.clone();
        got.sort();
        assert_eq!(got, expected, "the batch must cover exactly the same rows");

        // ⚠️ The empty folder contributes nothing and is indistinguishable in the ROWS from a
        // folder that was never asked about — which is why the API echoes `answered` rather than
        // letting a caller infer coverage from what came back.
        assert_eq!(batched.len(), 3);

        // The ungrouped bucket is NOT reachable through the batch form: `= ANY` cannot match NULL,
        // and that is the whole reason `list_nodes_in_group` keeps a statement of its own for it
        // (`group_id IS NULL` since ADR-133 — index-usable, unlike `IS NOT DISTINCT FROM`).
        assert!(!batched.contains(&ungrouped));
        assert_eq!(
            ids(repo
                .list_nodes_in_group(None, None, 100)
                .await
                .expect("ungrouped")),
            vec![ungrouped]
        );

        // An empty id list asks nothing and touches no database.
        assert!(repo
            .list_nodes_in_groups(None, &[], 100)
            .await
            .expect("empty")
            .is_empty());
    }

    /// 🚨 **Both by-group reads carry each row's `sort_order` back** (ADR-133).
    ///
    /// They have always ordered by it and never returned it, so the API asked a second query for
    /// the order of the rows it had just read. Projecting it is only safe if the value that arrives
    /// is the STORED one — a projection that silently returned the default `0` would look correct
    /// (every row present, plausible order) and would flatten the operator's manual ordering the
    /// moment anything re-sorted client-side.
    ///
    /// ⚠️ The three spellings are checked against each other here too: the bucket (`IS NULL`), a
    /// named folder (`= $2`) and the batch (`= ANY`) must agree about the same rows.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn both_by_group_reads_return_the_stored_sort_order(pool: sqlx::PgPool) {
        let tokyo = pgtest::group(&pool, "Tokyo").await;
        let a = pgtest::node(&pool, "a", 1, Some(tokyo)).await;
        let b = pgtest::node(&pool, "b", 2, Some(tokyo)).await;
        let loose = pgtest::node(&pool, "z", 3, None).await;
        let repo = pgtest::repo(pool);

        // Deliberately not 0, and deliberately fractional: `sort_order` is a `DOUBLE PRECISION`
        // that drag-reorder bisects, so a run of integers would not notice a column read as an int.
        repo.place_node(b, Some(tokyo), 1.5).await.expect("place b");
        repo.place_node(a, Some(tokyo), 2.5).await.expect("place a");
        repo.place_node(loose, None, 7.25).await.expect("place z");

        let named = repo
            .list_nodes_in_group(None, Some(tokyo), 100)
            .await
            .expect("named folder");
        assert_eq!(
            named
                .iter()
                .map(|o| (o.node.id.as_uuid(), o.sort_order))
                .collect::<Vec<_>>(),
            vec![(b, 1.5), (a, 2.5)],
            "the named-folder read returns the stored order, in it"
        );

        let batched = repo
            .list_nodes_in_groups(None, &[tokyo], 100)
            .await
            .expect("batch");
        assert_eq!(
            batched
                .iter()
                .map(|o| (o.node.id.as_uuid(), o.sort_order))
                .collect::<Vec<_>>(),
            vec![(b, 1.5), (a, 2.5)],
            "and the batch form agrees with it, row for row"
        );

        // The bucket takes the third statement (`group_id IS NULL`), so it needs its own case.
        let bucket = repo
            .list_nodes_in_group(None, None, 100)
            .await
            .expect("ungrouped");
        assert_eq!(
            bucket
                .iter()
                .map(|o| (o.node.id.as_uuid(), o.sort_order))
                .collect::<Vec<_>>(),
            vec![(loose, 7.25)],
            "the ungrouped bucket carries it too"
        );
    }

    /// A scoped caller cannot widen their view by batching. The scope predicate rides alongside the
    /// group filter in both forms, and this is the direction that fails open if it ever stops.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_batch_read_refuses_a_group_outside_the_scope(pool: sqlx::PgPool) {
        let mine = pgtest::group(&pool, "Mine").await;
        let theirs = pgtest::group(&pool, "Theirs").await;
        let ours = pgtest::node(&pool, "ours", 1, Some(mine)).await;
        let _hidden = pgtest::node(&pool, "hidden", 1, Some(theirs)).await;
        let repo = pgtest::repo(pool);

        let scope = [mine];
        let got = repo
            .list_nodes_in_groups(Some(&scope), &[mine, theirs], 100)
            .await
            .expect("scoped batch");
        assert_eq!(
            got.into_iter()
                .map(|o| o.node.id.as_uuid())
                .collect::<Vec<_>>(),
            vec![ours],
            "asking about a folder outside the scope must not return its nodes"
        );
    }
}
