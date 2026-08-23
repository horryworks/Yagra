// SPDX-License-Identifier: AGPL-3.0-only
//! Maintenance windows + mutes persistence (alert-quality).
//!
//! Windows are scoped like thresholds (node / profile / group, ADR-013); the alert engine
//! snapshots the *currently active* windows every refresh and treats matching nodes as
//! [`yagra_common::NodeState::Maintenance`] — no alerts fire and existing ones resolve.
//! Mutes silence the **notification** for one node (optionally one check) until a given
//! time; the alert still fires for the UI/history. This is the I/O adapter — the
//! suppression itself lives in [`crate::alerts`].

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, Row};
use std::collections::BTreeSet;
use uuid::Uuid;
use yagra_common::{Node, NodeId};

/// How a maintenance window is scoped. The first three mirror threshold scoping (ADR-013):
/// `Node` = node id, `Profile` = device-class id, `Group` = a tag value. `FolderGroup` is the
/// hierarchical inventory group (resolved recursively, incl. subgroups, ADR-022) — the scope the
/// All Nodes right-click uses. It is *not* a threshold concept, so it stays local to maintenance
/// rather than extending the shared [`yagra_common::ScopeLevel`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WindowScope {
    Node,
    Profile,
    Group,
    /// Hierarchical folder group (recursive). Serialized as `group_id` (the scope_id is a group
    /// UUID) so it can't be confused with the tag-based `group` scope.
    #[serde(rename = "group_id")]
    FolderGroup,
    /// Every node, with no id at all — the deployment itself is out of service.
    ///
    /// Added for ADR-050: an upgrade restarts core and every poller, and alerting about that is
    /// noise an operator cannot act on. The other four scopes all answer "which nodes", and none of
    /// them can say "all of them" without enumerating something that changes underneath.
    ///
    /// ⚠️ **The window hides real outages too.** The upgrade path therefore does both halves: it
    /// fills `ends_at` before it starts, so a run that dies partway cannot leave the fleet silent
    /// indefinitely, *and* the core that comes back closes the window as soon as the run reports an
    /// outcome ([`MaintenanceRepo::end_upgrade_windows`]). The bound is the backstop, not the plan —
    /// a measured upgrade takes ~65s against a 900s bound.
    System,
}

/// The `scope_id` every window the upgrade path opens carries.
///
/// [`WindowScope::System`] is deliberately absent from `WINDOW_SCOPE_LEVELS`, so no operator can
/// open a fleet-wide window through ordinary CRUD — which is what makes "system scope, this id"
/// a precise enough match for [`MaintenanceRepo::end_upgrade_windows`] to close the run's own
/// window without carrying its id through the updater and back.
pub const UPGRADE_SCOPE_ID: &str = "upgrade";

impl WindowScope {
    /// Parse the stored `scope_level` token.
    ///
    /// The fallback is over a `&str` from the database, not over this enum, so it is not the
    /// wildcard `coding-conventions.md` bans — it is the lenient read that keeps one unrecognised
    /// row from failing a whole page. It does mean an **older core reads a `system` window as a
    /// profile window whose id matches no profile**, i.e. suppresses nothing. That is the safe
    /// direction: during a rolling upgrade the old binary alerts too much rather than too little.
    fn parse(s: &str) -> Self {
        match s {
            "node" => WindowScope::Node,
            "group" => WindowScope::Group,
            "group_id" => WindowScope::FolderGroup,
            "system" => WindowScope::System,
            _ => WindowScope::Profile,
        }
    }

    /// The stored token, which must match the serde tag — the column and the JSON field are
    /// produced by two different mechanisms (testing.md).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            WindowScope::Node => "node",
            WindowScope::Profile => "profile",
            WindowScope::Group => "group",
            WindowScope::FolderGroup => "group_id",
            WindowScope::System => "system",
        }
    }
}

/// Whether a mute targets a single node or a whole folder group (recursive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MuteScope {
    Node,
    Group,
}

impl MuteScope {
    fn parse(s: &str) -> Self {
        match s {
            "group" => MuteScope::Group,
            _ => MuteScope::Node,
        }
    }
}

/// Which kind of suppression a node has been released from.
///
/// ⚠️ Not [`yagra_common::Node`]'s `suppression_opt_out` (migration 0069), which is about *derived
/// dependency* suppression (ADR-043). This is about maintenance windows and mutes, and the two must
/// not be wired to each other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExemptionKind {
    Maintenance,
    Mute,
}

impl ExemptionKind {
    /// The stored token, which must match the serde tag — the column and the JSON field are
    /// produced by two different mechanisms (`testing.md`), and a disagreement means rows the
    /// writer produces are rows the reader cannot parse.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ExemptionKind::Maintenance => "maintenance",
            ExemptionKind::Mute => "mute",
        }
    }

    /// Parse the stored token. Like [`WindowScope::parse`] this is lenient over a `&str` from the
    /// database rather than a wildcard over this enum: one unrecognised row must not fail a whole
    /// page. It falls back to `Maintenance` — the narrower of the two, since a mute-only exemption
    /// read as a maintenance one releases a node from planned suppression it was already visible
    /// in, rather than silently un-muting notifications nobody asked to hear.
    fn parse(s: &str) -> Self {
        match s {
            "mute" => ExemptionKind::Mute,
            _ => ExemptionKind::Maintenance,
        }
    }
}

/// A node released from an inherited suppression, until the suppression it was carved out of ends.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct StoredExemption {
    pub id: Uuid,
    pub kind: ExemptionKind,
    pub node_id: Uuid,
    /// RFC 3339. Server-computed from the coverage in force when the release was made.
    pub until_at: String,
}

/// A stored maintenance window (API shape; times are RFC 3339 text at the edge).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct StoredWindow {
    pub id: Uuid,
    pub name: String,
    // Serialized as `scope_level` so the GET response matches the POST body field name.
    #[serde(rename = "scope_level")]
    pub level: WindowScope,
    pub scope_id: String,
    pub starts_at: String,
    pub ends_at: String,
    pub enabled: bool,
    /// Whether the window covers "now" (computed at read time for the UI).
    pub active: bool,
}

/// A stored mute (API shape). A `node` mute silences one node (optionally one check via
/// `metric_name`); a `group` mute silences every node under a folder group (recursive). Exactly
/// one of `node_id` / `group_id` is set, per `scope_kind`.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct StoredMute {
    pub id: Uuid,
    pub scope_kind: MuteScope,
    pub node_id: Option<Uuid>,
    pub group_id: Option<Uuid>,
    // Serialized as `metric_name` (matches the create body); the DB column stays `check_name`.
    #[serde(rename = "metric_name")]
    pub check_name: Option<String>,
    pub until_at: String,
    pub reason: Option<String>,
}

/// PostgreSQL-backed maintenance windows + mutes.
pub struct MaintenanceRepo {
    pool: PgPool,
}

impl MaintenanceRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// All windows, newest first (the UI lists past windows too until deleted).
    pub async fn list_windows(&self) -> anyhow::Result<Vec<StoredWindow>> {
        let rows = sqlx::query(
            "SELECT id, name, scope_level, scope_id, starts_at, ends_at, enabled, \
                    (enabled AND starts_at <= now() AND ends_at > now()) AS active \
             FROM maintenance_windows ORDER BY starts_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let starts: DateTime<Utc> = row.try_get("starts_at")?;
                let ends: DateTime<Utc> = row.try_get("ends_at")?;
                Ok(StoredWindow {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    level: WindowScope::parse(&row.try_get::<String, _>("scope_level")?),
                    scope_id: row.try_get("scope_id")?,
                    starts_at: starts.to_rfc3339(),
                    ends_at: ends.to_rfc3339(),
                    enabled: row.try_get("enabled")?,
                    active: row.try_get("active")?,
                })
            })
            .collect()
    }

    /// Create a window; returns its id. Validation (ends > starts) happens at the API edge.
    pub async fn create_window(
        &self,
        name: &str,
        scope_level: &str,
        scope_id: &str,
        starts_at: DateTime<Utc>,
        ends_at: DateTime<Utc>,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO maintenance_windows (id, name, scope_level, scope_id, starts_at, ends_at) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(id)
        .bind(name)
        .bind(scope_level)
        .bind(scope_id)
        .bind(starts_at)
        .bind(ends_at)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Enable/disable a window. Returns whether it exists.
    pub async fn set_window_enabled(&self, id: Uuid, enabled: bool) -> anyhow::Result<bool> {
        let res = sqlx::query("UPDATE maintenance_windows SET enabled = $2 WHERE id = $1")
            .bind(id)
            .bind(enabled)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// End an **active** window now, leaving the row as a record of the maintenance that actually
    /// happened. Returns whether a row moved.
    ///
    /// This is what the inventory tree's "release" does to a window it can act on, and it is
    /// deliberately not a delete: the operator asked for the suppression to stop, not for the
    /// evidence that it existed to disappear. What is left reads as `ended` and is swept by
    /// [`Self::delete_ended_windows`] when someone wants it gone.
    ///
    /// ⚠️ **The `starts_at <= now()` clause is load-bearing, not symmetry.** Writing `ends_at =
    /// now()` on a window that has not begun yet would store `ends_at < starts_at` — precisely the
    /// inversion `api::maintenance::check_order` rejects at the edge, and the one that suppresses
    /// nothing while looking to the operator like it worked. The UI only offers this on an active
    /// window; this predicate is what makes that true of *any* caller. A window outside the
    /// predicate reports `false`, and the handler turns that into a 404.
    pub async fn end_window_now(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE maintenance_windows SET ends_at = now() \
             WHERE id = $1 AND enabled AND starts_at <= now() AND ends_at > now()",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Delete a window. Returns whether a row was removed.
    pub async fn delete_window(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM maintenance_windows WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Delete the windows among `ids` that have ended. Returns how many rows went.
    ///
    /// **`ends_at <= now()` is evaluated by the database, not by the caller.** The browser's clock
    /// decides nothing here, and the predicate is the exact complement of [`Self::active_scopes`]'
    /// `ends_at > now()` — so a window this removes is one that was already suppressing nothing.
    ///
    /// `ids` is the caller's *visible* set (`api::maintenance::visible_windows`), which is what
    /// makes this honour RBAC group scope. The two conditions are ANDed in one statement so
    /// neither rule can be applied without the other: dropping the id clause would clear the whole
    /// deployment's ended windows for a group-scoped caller, and dropping the time clause would
    /// delete windows that are still suppressing alerts.
    pub async fn delete_ended_windows(&self, ids: &[Uuid]) -> anyhow::Result<u64> {
        // `= ANY('{}')` is valid but pointless; skip the round trip.
        if ids.is_empty() {
            return Ok(0);
        }
        let res =
            sqlx::query("DELETE FROM maintenance_windows WHERE id = ANY($1) AND ends_at <= now()")
                .bind(ids)
                .execute(&self.pool)
                .await?;
        Ok(res.rows_affected())
    }

    /// The scopes of windows active **right now** (enabled, covering the current time).
    /// The alert-config refresh resolves these against the inventory.
    pub async fn active_scopes(&self) -> anyhow::Result<Vec<(WindowScope, String)>> {
        let rows = sqlx::query(
            "SELECT scope_level, scope_id FROM maintenance_windows \
             WHERE enabled AND starts_at <= now() AND ends_at > now()",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    WindowScope::parse(&row.try_get::<String, _>("scope_level")?),
                    row.try_get("scope_id")?,
                ))
            })
            .collect()
    }

    /// The scopes of windows active right now **with the instant each stops**.
    ///
    /// [`Self::active_scopes`] answers "which nodes are suppressed", which is all the alert engine
    /// needs. Releasing one node from an inherited window needs the other half — *when* that
    /// coverage runs out — because the exemption is sized to it. Kept as a second query rather than
    /// widening `active_scopes`, whose result is rebuilt on every refresh cycle for the whole fleet
    /// and does not need the extra column.
    pub async fn active_windows(
        &self,
    ) -> anyhow::Result<Vec<(WindowScope, String, DateTime<Utc>)>> {
        let rows = sqlx::query(
            "SELECT scope_level, scope_id, ends_at FROM maintenance_windows \
             WHERE enabled AND starts_at <= now() AND ends_at > now()",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    WindowScope::parse(&row.try_get::<String, _>("scope_level")?),
                    row.try_get("scope_id")?,
                    row.try_get("ends_at")?,
                ))
            })
            .collect()
    }

    /// End every still-open upgrade window now. Returns how many were closed.
    ///
    /// Called by the core that comes back after an upgrade, once the run has reported an outcome
    /// (ADR-050 decision 12). The process that opened the window is never the process that sees the
    /// run finish, so this is the only place it can happen.
    ///
    /// **`ends_at` moves to `now()`; this path does not delete the row.** The fleet really was
    /// silenced for that long, and an operator asking "why did nothing alert at 10:31" deserves to
    /// find the answer rather than an absence. [`Self::active_scopes`] filters on `ends_at > now()`,
    /// so the suppression stops on the alerting config's next refresh either way.
    ///
    /// That is a rule about *this* path only, not a guarantee the row survives: an operator may
    /// delete a closed window by hand or clear the ended ones in bulk
    /// ([`Self::delete_ended_windows`]), and both are audited.
    ///
    /// [`Self::end_window_now`] closes one window by id the same way and for the same reason. The
    /// two are not merged because their predicates are the *point* of each: this one matches the
    /// upgrade family and no id, and does not care whether the window has started, because the
    /// upgrade path opens it starting now. The other matches one id and must refuse a window that
    /// has not begun.
    pub async fn end_upgrade_windows(&self) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "UPDATE maintenance_windows SET ends_at = now() \
             WHERE scope_level = $1 AND scope_id = $2 AND ends_at > now()",
        )
        .bind(WindowScope::System.as_str())
        .bind(UPGRADE_SCOPE_ID)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Unexpired mutes, soonest-expiring first. Expired rows are dropped lazily on read
    /// (they are inert either way — the notifier only loads unexpired ones).
    pub async fn list_mutes(&self) -> anyhow::Result<Vec<StoredMute>> {
        sqlx::query("DELETE FROM mutes WHERE until_at <= now()")
            .execute(&self.pool)
            .await?;
        let rows = sqlx::query(
            "SELECT id, scope_kind, node_id, group_id, check_name, until_at, reason FROM mutes \
             WHERE until_at > now() ORDER BY until_at",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let until: DateTime<Utc> = row.try_get("until_at")?;
                Ok(StoredMute {
                    id: row.try_get("id")?,
                    scope_kind: MuteScope::parse(&row.try_get::<String, _>("scope_kind")?),
                    node_id: row.try_get("node_id")?,
                    group_id: row.try_get("group_id")?,
                    check_name: row.try_get("check_name")?,
                    until_at: until.to_rfc3339(),
                    reason: row.try_get("reason")?,
                })
            })
            .collect()
    }

    /// Create a mute; returns its id. `scope_kind` is `"node"` (the default) or `"group"`. For a
    /// group mute the whole node-set is silenced, so `check_name` is ignored.
    pub async fn create_mute(
        &self,
        scope_kind: &str,
        scope_id: Uuid,
        check_name: Option<&str>,
        until_at: DateTime<Utc>,
        reason: Option<&str>,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        let group = scope_kind == "group";
        let (node_id, group_id, check) = if group {
            (None, Some(scope_id), None)
        } else {
            (Some(scope_id), None, check_name)
        };
        sqlx::query(
            "INSERT INTO mutes (id, scope_kind, node_id, group_id, check_name, until_at, reason) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(id)
        .bind(if group { "group" } else { "node" })
        .bind(node_id)
        .bind(group_id)
        .bind(check)
        .bind(until_at)
        .bind(reason)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    // ── Exemptions: one node released from a suppression it inherited (migration 0081) ──────

    /// The nodes currently exempt from `kind`. Expired rows are dropped lazily on read — the same
    /// shape as [`Self::list_mutes`], and for the same reason: they are inert either way.
    pub async fn exempt_nodes(&self, kind: ExemptionKind) -> anyhow::Result<Vec<Uuid>> {
        sqlx::query("DELETE FROM suppression_exemptions WHERE until_at <= now()")
            .execute(&self.pool)
            .await?;
        let rows = sqlx::query(
            "SELECT node_id FROM suppression_exemptions WHERE kind = $1 AND until_at > now()",
        )
        .bind(kind.as_str())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| Ok(row.try_get("node_id")?))
            .collect()
    }

    /// Every unexpired exemption, for the read surface.
    pub async fn list_exemptions(&self) -> anyhow::Result<Vec<StoredExemption>> {
        let rows = sqlx::query(
            "SELECT id, kind, node_id, until_at FROM suppression_exemptions \
             WHERE until_at > now() ORDER BY until_at",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let until: DateTime<Utc> = row.try_get("until_at")?;
                Ok(StoredExemption {
                    id: row.try_get("id")?,
                    kind: ExemptionKind::parse(&row.try_get::<String, _>("kind")?),
                    node_id: row.try_get("node_id")?,
                    until_at: until.to_rfc3339(),
                })
            })
            .collect()
    }

    /// Release `node` from `kind` until `until`.
    ///
    /// Upserts on `(kind, node_id)`: releasing a node twice extends the expiry rather than leaving
    /// two rows for the reader to reconcile. `until` is the caller's computation over the coverage
    /// actually in force — see `api::maintenance::inherited_coverage_end`.
    pub async fn set_exemption(
        &self,
        kind: ExemptionKind,
        node: Uuid,
        until: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO suppression_exemptions (id, kind, node_id, until_at) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (kind, node_id) DO UPDATE SET until_at = EXCLUDED.until_at",
        )
        .bind(Uuid::new_v4())
        .bind(kind.as_str())
        .bind(node)
        .bind(until)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Put `node` back under `kind`. Returns whether a row was removed.
    pub async fn clear_exemption(&self, kind: ExemptionKind, node: Uuid) -> anyhow::Result<bool> {
        let res =
            sqlx::query("DELETE FROM suppression_exemptions WHERE kind = $1 AND node_id = $2")
                .bind(kind.as_str())
                .bind(node)
                .execute(&self.pool)
                .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Delete a mute. Returns whether a row was removed.
    pub async fn delete_mute(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM mutes WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

/// Whether a window scope covers `node`, and whether it does so *by naming it*.
///
/// Exhaustive over [`WindowScope`] on purpose — a new scope must decide both answers rather than
/// falling into a wildcard. The hierarchical [`WindowScope::FolderGroup`] scope is resolved
/// separately by the caller (it needs the group edges + DB membership, via
/// [`crate::groups::group_subtree`] + `NodeRepo::nodes_in_groups`), so it is never covered here.
#[must_use]
fn scope_covers(level: WindowScope, scope_id: &str, node: &Node) -> bool {
    match level {
        WindowScope::Node => scope_id == node.id.to_string(),
        WindowScope::Profile => node.profile.map(|p| p.to_string()).as_deref() == Some(scope_id),
        WindowScope::Group => node.tags.values().any(|v| v == scope_id),
        WindowScope::FolderGroup => false,
        // No id to match: the deployment itself is out of service, so every node is.
        WindowScope::System => true,
    }
}

/// The nodes an active window names **directly** — a [`WindowScope::Node`] window on that node.
///
/// Split out because a [`StoredExemption`] must not cancel one. An operator who released a node
/// from its group's window and then deliberately opened a window on *that one node* is asking for
/// it to be suppressed; if the exemption still applied, they would get an unsuppressed node with
/// nothing on screen explaining why.
#[must_use]
pub fn nodes_named_by_a_window(
    scopes: &[(WindowScope, String)],
    nodes: &[Node],
) -> BTreeSet<NodeId> {
    nodes
        .iter()
        .filter(|node| {
            scopes
                .iter()
                .any(|(level, id)| *level == WindowScope::Node && scope_covers(*level, id, node))
        })
        .map(|node| node.id)
        .collect()
}

/// The nodes an active window covers **as part of a class** — profile, tag, or the whole fleet.
///
/// The other half of [`nodes_named_by_a_window`], and the half an exemption may cancel.
/// Folder-group coverage is also inherited but is resolved by the caller against the group tree, so
/// it is added to this set there rather than computed here.
#[must_use]
pub fn nodes_covered_by_a_class_window(
    scopes: &[(WindowScope, String)],
    nodes: &[Node],
) -> BTreeSet<NodeId> {
    nodes
        .iter()
        .filter(|node| {
            scopes
                .iter()
                .any(|(level, id)| *level != WindowScope::Node && scope_covers(*level, id, node))
        })
        .map(|node| node.id)
        .collect()
}

// ── Releasing one node from coverage it inherited (migration 0081) ───────────────────────────
//
// The release itself is granted at the API edge, but the rule it rests on — *what a node merely
// inherits*, and *how long a release from it may last* — has three readers: the grant
// (`api::maintenance`), the alert engine's refresh (`alerts::config::resolve_maintenance`), and
// [`reconcile_exemptions`] below. It lives here because of that, per `api-conventions.md`: a
// helper shared across domains must not live inside one of them.

/// Parse a timestamp this module rendered with `to_rfc3339`. Same call the API edge's
/// `parse_rfc3339` makes; duplicated rather than reached for across the layering, since a domain
/// module must not depend on `api::`.
fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// A node's coverage-relevant facts, resolved once so the decisions over them are pure.
pub(crate) struct CoverageFacts {
    profile: Option<String>,
    /// Tag *values* — a window's `group` scope matches any of them (ADR-013).
    tags: Vec<String>,
    /// This node's folder group plus every group above it.
    ///
    /// This is the same question the alert engine answers from the other end (it walks
    /// `group_subtree` down from each window's group), and both use `groups.rs`' primitives over
    /// the same edges, so `node ∈ subtree(root)` and `root ∈ containing_groups(node)` agree by
    /// construction. Walking up is O(depth) for one node instead of O(groups × windows).
    containing_groups: Vec<Uuid>,
}

impl CoverageFacts {
    /// Resolve them for one already-loaded node. `edges` is the whole `(id, parent_id)` set, so a
    /// caller checking several nodes loads it once.
    pub(crate) fn of(node: &Node, edges: &[(Uuid, Option<Uuid>)]) -> Self {
        let mut containing_groups = Vec::new();
        if let Some(group) = node.group {
            let gid = group.as_uuid();
            containing_groups.push(gid);
            containing_groups.extend(crate::groups::group_ancestors(edges, gid));
        }
        Self {
            profile: node.profile.map(|p| p.to_string()),
            tags: node.tags.values().cloned().collect(),
            containing_groups,
        }
    }
}

/// Whether an active window covers this node **without naming it** — the coverage a release may
/// cancel. Exhaustive over [`WindowScope`]: a new scope has to decide.
pub(crate) fn window_is_inherited_by(
    level: WindowScope,
    scope_id: &str,
    facts: &CoverageFacts,
) -> bool {
    match level {
        // Names the node. Excluded on purpose: a release never cancels coverage aimed at the node.
        WindowScope::Node => false,
        WindowScope::Profile => facts.profile.as_deref() == Some(scope_id),
        WindowScope::Group => facts.tags.iter().any(|t| t == scope_id),
        WindowScope::FolderGroup => {
            Uuid::parse_str(scope_id).is_ok_and(|g| facts.containing_groups.contains(&g))
        }
        // The whole fleet is out of service and no node owns that window, so a single box can be
        // brought back into monitoring without it being cancelled by the window itself.
        WindowScope::System => true,
    }
}

/// When the inherited maintenance coverage on this node runs out, or `None` if none covers it.
///
/// The **latest** end, so the exemption lasts as long as the reason for it does. Taking the
/// earliest would put the node back into maintenance while a longer window still covered it.
pub(crate) fn inherited_maintenance_end(
    windows: &[(WindowScope, String, DateTime<Utc>)],
    facts: &CoverageFacts,
) -> Option<DateTime<Utc>> {
    windows
        .iter()
        .filter(|(level, scope_id, _)| window_is_inherited_by(*level, scope_id, facts))
        .map(|(_, _, ends)| *ends)
        .max()
}

/// The mute counterpart: a group mute over a folder group containing this node.
pub(crate) fn inherited_mute_end(
    mutes: &[StoredMute],
    facts: &CoverageFacts,
) -> Option<DateTime<Utc>> {
    mutes
        .iter()
        .filter(|m| match m.scope_kind {
            // Names the node — same rule as a node-scoped window.
            MuteScope::Node => false,
            MuteScope::Group => m
                .group_id
                .is_some_and(|g| facts.containing_groups.contains(&g)),
        })
        .filter_map(|m| parse_ts(&m.until_at))
        .max()
}

/// What a stored exemption is still entitled to, once the coverage in force has been re-derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExemptionUpdate {
    Keep,
    ShortenTo(DateTime<Utc>),
    Drop,
}

/// Re-derive one release's expiry against the coverage actually in force.
///
/// **It never extends one, and that asymmetry is the safety rule.** Shortening returns a node to
/// monitoring sooner; lengthening would carry a release into a window the operator never released
/// it from, which is precisely the blind spot this feature must not create. So the answer is
/// `min(stored, in force)` — and no coverage in force at all means the reason for the release is
/// gone and so is the release.
pub(crate) fn reconcile_exemption(
    stored_until: DateTime<Utc>,
    in_force_until: Option<DateTime<Utc>>,
) -> ExemptionUpdate {
    match in_force_until {
        None => ExemptionUpdate::Drop,
        Some(end) if end < stored_until => ExemptionUpdate::ShortenTo(end),
        Some(_) => ExemptionUpdate::Keep,
    }
}

/// Bring every stored release back in line with the coverage in force. Returns how many rows it
/// changed, for the caller's log.
///
/// An exemption is sized once, when it is granted, to the coverage covering the node at that
/// instant — but coverage can stop sooner than it said it would: a window **ended early**,
/// disabled or deleted, a mute lifted. Nothing re-derived the expiry, so the release outlived its
/// reason. Two things went wrong then, and the second is the one that matters: the tree drew a
/// "released" marker on a row nothing was suppressing, *and* the stale row stayed live in
/// [`MaintenanceRepo::exempt_nodes`], so the **next** window over that group would have skipped
/// the node silently.
///
/// Called every refresh cycle (the backstop that catches any path, including one added later) and
/// inline by the handlers that remove coverage, so the operator's own screen corrects at once.
/// Cheap when there is nothing to do: one indexed `SELECT` returning no rows.
pub(crate) async fn reconcile_exemptions(
    maintenance: &MaintenanceRepo,
    groups: &crate::groups::GroupRepo,
    repo: &crate::repo::NodeRepo,
) -> anyhow::Result<usize> {
    let rows = maintenance.list_exemptions().await?;
    if rows.is_empty() {
        return Ok(0);
    }
    let windows = maintenance.active_windows().await?;
    let mutes = maintenance.list_mutes().await?;
    let edges = groups.edges().await?;
    let mut changed = 0;
    for row in rows {
        let Some(stored_until) = parse_ts(&row.until_at) else {
            continue;
        };
        // A release whose node is gone releases nothing. The row should have gone with it, so this
        // is a repair rather than an expected path.
        let Some(node) = repo.get_node(row.node_id).await? else {
            maintenance.clear_exemption(row.kind, row.node_id).await?;
            changed += 1;
            continue;
        };
        let facts = CoverageFacts::of(&node, &edges);
        let in_force = match row.kind {
            ExemptionKind::Maintenance => inherited_maintenance_end(&windows, &facts),
            ExemptionKind::Mute => inherited_mute_end(&mutes, &facts),
        };
        match reconcile_exemption(stored_until, in_force) {
            ExemptionUpdate::Keep => {}
            ExemptionUpdate::ShortenTo(until) => {
                maintenance
                    .set_exemption(row.kind, row.node_id, until)
                    .await?;
                changed += 1;
            }
            ExemptionUpdate::Drop => {
                maintenance.clear_exemption(row.kind, row.node_id).await?;
                changed += 1;
            }
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use yagra_common::ProfileId;

    /// This module's own source. The bulk clear's two safety conditions live entirely inside one
    /// SQL string and there is no database in unit tests, so nothing else can catch a rewrite that
    /// drops one of them.
    const SRC: &str = include_str!("maintenance.rs");

    /// The executable code above this test module, comments stripped — otherwise a doc comment
    /// *naming* a pattern reads as the pattern itself (testing.md's self-match trap).
    fn production_source() -> String {
        SRC.split("#[cfg(test)]")
            .next()
            .expect("split always yields a first element")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_bulk_delete_lets_the_database_decide_what_ended_means() {
        // The whole design rests on this: the client sends no timestamp and no list of "ended"
        // ids, so a browser with a skewed clock cannot talk the server into deleting a window that
        // is still suppressing alerts. A rewrite that bound a caller-supplied time here would look
        // fine and pass every other test.
        let src = production_source();
        let stmt = src
            .split_once("DELETE FROM maintenance_windows WHERE id = ANY($1)")
            .expect("the bulk clear statement is still here")
            .1;
        let stmt = &stmt[..stmt.find('"').expect("the statement is a string literal")];
        assert!(
            stmt.contains("AND ends_at <= now()"),
            "the bulk clear must apply the ended test in SQL, found: {stmt}"
        );
    }

    #[test]
    fn the_bulk_delete_is_bounded_by_an_id_list() {
        // Without the id clause a group-scoped caller clears every ended window in the deployment,
        // including ones they cannot see — the filtering happens in the API layer and arrives only
        // as this list.
        let src = production_source();
        assert!(src.contains("DELETE FROM maintenance_windows WHERE id = ANY($1)"));
        assert!(
            src.contains("if ids.is_empty()"),
            "an empty visible set must short-circuit rather than reach the database"
        );
    }

    /// [`production_source`] with line continuations and indentation collapsed, so a needle can
    /// name a SQL statement the way it reads rather than the way `cargo fmt` happened to wrap it.
    fn flattened_source() -> String {
        production_source()
            .replace('\\', " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn ending_a_window_early_cannot_invert_it() {
        // `ends_at = now()` on a window that has not started yet stores `ends_at < starts_at`, and
        // a window whose end precedes its start matches no instant — it suppresses nothing while
        // the operator believes the release "worked". `check_order` refuses that shape at the edge
        // on the way in, so this statement must refuse to create it on the way out. There is no
        // database in unit tests and both guards live inside one SQL string.
        let flat = flattened_source();
        let stmt = flat
            .split_once("UPDATE maintenance_windows SET ends_at = now() WHERE id = $1")
            .expect("the early-end statement is still here")
            .1;
        let stmt = &stmt[..stmt.find('"').expect("the statement is a string literal")];
        assert!(
            stmt.contains("starts_at <= now()"),
            "ending a window early must refuse one that has not begun, found: {stmt}"
        );
        assert!(
            stmt.contains("ends_at > now()"),
            "ending a window early must refuse one already over, found: {stmt}"
        );
    }

    /// Every node an active window covers, however it covers them.
    ///
    /// Production deliberately has no such function: `resolve_maintenance` keeps the two halves
    /// apart because an exemption cancels only the class half. The scope-semantics tests below are
    /// about the *rule* rather than about that split, so they read over the union — and
    /// [`a_direct_window_and_a_class_window_are_told_apart`] is what pins the split itself.
    fn covered(scopes: &[(WindowScope, String)], nodes: &[Node]) -> BTreeSet<NodeId> {
        nodes_named_by_a_window(scopes, nodes)
            .union(&nodes_covered_by_a_class_window(scopes, nodes))
            .copied()
            .collect()
    }

    fn node(profile: Option<ProfileId>, tags: &[(&str, &str)]) -> Node {
        Node {
            id: NodeId::new(),
            name: "n".to_owned(),
            parent: None,
            address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            profile,
            pool: None,
            credential: None,
            vendor: None,
            model: None,
            group: None,
            tags: tags
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    #[test]
    fn node_scope_matches_only_that_node() {
        let a = node(None, &[]);
        let b = node(None, &[]);
        let scopes = vec![(WindowScope::Node, a.id.to_string())];
        let set = covered(&scopes, &[a.clone(), b.clone()]);
        assert!(set.contains(&a.id));
        assert!(!set.contains(&b.id));
    }

    #[test]
    fn profile_scope_matches_profile_members() {
        let profile = ProfileId::new();
        let a = node(Some(profile), &[]);
        let b = node(None, &[]);
        let scopes = vec![(WindowScope::Profile, profile.to_string())];
        let set = covered(&scopes, &[a.clone(), b.clone()]);
        assert!(set.contains(&a.id));
        assert!(!set.contains(&b.id));
    }

    #[test]
    fn group_scope_matches_tag_values() {
        let a = node(None, &[("site", "tokyo")]);
        let b = node(None, &[("site", "osaka")]);
        let scopes = vec![(WindowScope::Group, "tokyo".to_owned())];
        let set = covered(&scopes, &[a.clone(), b.clone()]);
        assert!(set.contains(&a.id));
        assert!(!set.contains(&b.id));
    }

    #[test]
    fn folder_group_scope_is_resolved_elsewhere() {
        // FolderGroup needs the group tree + DB membership, so the in-memory, tag-only resolution
        // must ignore it — the caller unions the resolved node set. A folder-group scope alone
        // therefore yields nothing here, even for a node that happens to carry the id as a tag.
        let a = node(None, &[("site", "tokyo")]);
        let scopes = vec![(WindowScope::FolderGroup, Uuid::new_v4().to_string())];
        assert!(covered(&scopes, &[a]).is_empty());
    }

    #[test]
    fn window_scope_round_trips_through_strings() {
        for (s, scope) in [
            ("node", WindowScope::Node),
            ("profile", WindowScope::Profile),
            ("group", WindowScope::Group),
            ("group_id", WindowScope::FolderGroup),
        ] {
            assert_eq!(WindowScope::parse(s), scope);
            // Serialize emits the same wire string the API edge accepts.
            assert_eq!(
                serde_json::to_value(scope).unwrap(),
                serde_json::Value::String(s.to_owned())
            );
        }
        // Unknown levels fall back to Profile (the broadest), never panic.
        assert_eq!(WindowScope::parse("bogus"), WindowScope::Profile);
    }

    #[test]
    fn mute_scope_round_trips() {
        assert_eq!(MuteScope::parse("group"), MuteScope::Group);
        assert_eq!(MuteScope::parse("node"), MuteScope::Node);
        assert_eq!(MuteScope::parse("bogus"), MuteScope::Node);
        assert_eq!(
            serde_json::to_value(MuteScope::Group).unwrap(),
            serde_json::Value::String("group".to_owned())
        );
    }

    #[test]
    fn no_active_scopes_means_no_maintenance() {
        let a = node(None, &[("site", "tokyo")]);
        assert!(covered(&[], &[a]).is_empty());
    }

    #[test]
    fn exemption_kind_round_trips() {
        for (s, kind) in [
            ("maintenance", ExemptionKind::Maintenance),
            ("mute", ExemptionKind::Mute),
        ] {
            assert_eq!(ExemptionKind::parse(s), kind);
            assert_eq!(kind.as_str(), s);
            // The column token and the JSON tag come from two different mechanisms; a
            // disagreement means rows this writes are rows it cannot read back.
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::Value::String(s.to_owned())
            );
        }
        assert_eq!(ExemptionKind::parse("bogus"), ExemptionKind::Maintenance);
    }

    #[test]
    fn a_direct_window_and_a_class_window_are_told_apart() {
        // The whole exemption design rests on this split: releasing a node cancels the class half
        // and never the half that names it. Both halves together must still be exactly what the
        // engine used to compute in one pass — no node gained, none lost.
        let profile = ProfileId::new();
        let named = node(Some(profile), &[]);
        let by_profile = node(Some(profile), &[]);
        let by_tag = node(None, &[("site", "tokyo")]);
        let untouched = node(None, &[("site", "osaka")]);
        let nodes = [named.clone(), by_profile.clone(), by_tag.clone(), untouched];
        let scopes = vec![
            (WindowScope::Node, named.id.to_string()),
            (WindowScope::Profile, profile.to_string()),
            (WindowScope::Group, "tokyo".to_owned()),
        ];

        let direct = nodes_named_by_a_window(&scopes, &nodes);
        assert_eq!(direct, BTreeSet::from([named.id]));

        let class = nodes_covered_by_a_class_window(&scopes, &nodes);
        assert_eq!(class, BTreeSet::from([named.id, by_profile.id, by_tag.id]));

        // `named` is in both — it is named *and* a member of the covered profile — which is the
        // case that matters: releasing it must still leave the direct window in force. And the two
        // halves together are the whole answer, with the node no window touches still outside it.
        assert_eq!(
            covered(&scopes, &nodes),
            BTreeSet::from([named.id, by_profile.id, by_tag.id])
        );
    }

    #[test]
    fn a_fleet_wide_window_is_inherited_by_every_node() {
        // `System` names no id, so no node "owns" it — an operator can release one box from the
        // upgrade window without the release being cancelled by the window itself.
        let a = node(None, &[]);
        let one = std::slice::from_ref(&a);
        let scopes = vec![(WindowScope::System, UPGRADE_SCOPE_ID.to_owned())];
        assert!(nodes_named_by_a_window(&scopes, one).is_empty());
        assert_eq!(
            nodes_covered_by_a_class_window(&scopes, one),
            BTreeSet::from([a.id])
        );
    }

    // ── What a release may be granted from, and how long it survives ────────────────────────

    fn facts(profile: Option<&str>, tags: &[&str], groups: &[Uuid]) -> CoverageFacts {
        CoverageFacts {
            profile: profile.map(str::to_owned),
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            containing_groups: groups.to_vec(),
        }
    }

    fn at(hour: u32) -> DateTime<Utc> {
        parse_ts(&format!("2026-08-12T{hour:02}:00:00Z")).expect("fixed timestamp parses")
    }

    fn group_mute(group: Uuid, until: DateTime<Utc>) -> StoredMute {
        StoredMute {
            id: Uuid::new_v4(),
            scope_kind: MuteScope::Group,
            node_id: None,
            group_id: Some(group),
            check_name: None,
            until_at: until.to_rfc3339(),
            reason: None,
        }
    }

    #[test]
    fn a_window_that_names_the_node_is_never_inherited() {
        // Rule 1 of the exemption design, and the one with a visible failure mode: if a release
        // could cancel a node-scoped window, an operator who released a box and then deliberately
        // put *that box* into maintenance would get a node that keeps alerting, with nothing on
        // screen saying why.
        let f = facts(Some("p1"), &["tokyo"], &[Uuid::from_u128(1)]);
        assert!(!window_is_inherited_by(
            WindowScope::Node,
            &Uuid::from_u128(99).to_string(),
            &f
        ));
        assert!(window_is_inherited_by(WindowScope::Profile, "p1", &f));
        assert!(!window_is_inherited_by(WindowScope::Profile, "p2", &f));
        assert!(window_is_inherited_by(WindowScope::Group, "tokyo", &f));
        assert!(!window_is_inherited_by(WindowScope::Group, "osaka", &f));
        assert!(window_is_inherited_by(
            WindowScope::FolderGroup,
            &Uuid::from_u128(1).to_string(),
            &f
        ));
        assert!(!window_is_inherited_by(
            WindowScope::FolderGroup,
            &Uuid::from_u128(2).to_string(),
            &f
        ));
        // A fleet-wide window belongs to nobody, so one box can be brought back without the window
        // itself cancelling the release.
        assert!(window_is_inherited_by(
            WindowScope::System,
            UPGRADE_SCOPE_ID,
            &f
        ));
        // An unparseable folder id is not a match — fail closed rather than release on a typo.
        assert!(!window_is_inherited_by(
            WindowScope::FolderGroup,
            "not-a-uuid",
            &f
        ));
    }

    #[test]
    fn the_release_lasts_as_long_as_the_reason_for_it() {
        // The latest end, not the earliest: while a longer window still covers the node, putting
        // it back would re-suppress a box the operator has said is back in service.
        let group = Uuid::from_u128(1);
        let f = facts(Some("p1"), &[], &[group]);
        let windows = vec![
            (WindowScope::FolderGroup, group.to_string(), at(2)),
            (WindowScope::Profile, "p1".to_owned(), at(5)),
            // Covers nothing here, and must not extend the release.
            (WindowScope::Group, "osaka".to_owned(), at(9)),
            // Names some other node.
            (WindowScope::Node, Uuid::from_u128(7).to_string(), at(11)),
        ];
        assert_eq!(inherited_maintenance_end(&windows, &f), Some(at(5)));
    }

    #[test]
    fn a_window_that_only_names_the_node_leaves_nothing_to_release() {
        // The pure half of the API's `not_suppressed` refusal: storing an exemption here would
        // leave the operator believing the node was out of a window it is still in.
        let f = facts(None, &[], &[]);
        let windows = vec![(WindowScope::Node, Uuid::from_u128(3).to_string(), at(4))];
        assert!(inherited_maintenance_end(&windows, &f).is_none());
    }

    #[test]
    fn only_a_group_mute_can_be_released_from() {
        let group = Uuid::from_u128(1);
        let f = facts(None, &[], &[group]);
        let node_mute = StoredMute {
            scope_kind: MuteScope::Node,
            node_id: Some(Uuid::from_u128(5)),
            group_id: None,
            ..group_mute(group, at(9))
        };
        // The node mute is later and is still ignored — it names the node.
        assert_eq!(
            inherited_mute_end(
                &[
                    node_mute.clone(),
                    group_mute(group, at(3)),
                    group_mute(Uuid::from_u128(2), at(23)),
                ],
                &f
            ),
            Some(at(3))
        );
        assert!(inherited_mute_end(&[node_mute], &f).is_none());
    }

    #[test]
    fn a_release_does_not_outlive_the_coverage_it_was_carved_out_of() {
        // The bug this reconcile exists for, seen on the test server: a group window opened for an
        // hour, one node released from it, then the window **ended early** from the tree. The
        // release was sized to the window's original end, nothing re-derived it, and the node
        // stayed released for the remaining 56 minutes — marked as such on a row nothing was
        // suppressing, and, worse, invisible to the next window over that group.
        assert_eq!(reconcile_exemption(at(7), None), ExemptionUpdate::Drop);
        assert_eq!(
            reconcile_exemption(at(7), Some(at(4))),
            ExemptionUpdate::ShortenTo(at(4))
        );
    }

    #[test]
    fn a_release_is_never_extended_by_coverage_it_was_not_granted_from() {
        // The other direction, and the one that would reintroduce the blind spot: a *later* window
        // opening over the group must not lengthen a release the operator never asked for against
        // it. The release runs out on schedule and the node falls back under the new window.
        assert_eq!(
            reconcile_exemption(at(7), Some(at(9))),
            ExemptionUpdate::Keep
        );
        assert_eq!(
            reconcile_exemption(at(7), Some(at(7))),
            ExemptionUpdate::Keep
        );
    }
}
