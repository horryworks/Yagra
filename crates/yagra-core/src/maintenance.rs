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

    /// Delete a window. Returns whether a row was removed.
    pub async fn delete_window(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM maintenance_windows WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
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

    /// End every still-open upgrade window now. Returns how many were closed.
    ///
    /// Called by the core that comes back after an upgrade, once the run has reported an outcome
    /// (ADR-050 decision 12). The process that opened the window is never the process that sees the
    /// run finish, so this is the only place it can happen.
    ///
    /// **`ends_at` moves to `now()`; the row is not deleted.** The fleet really was silenced for
    /// that long, and an operator asking "why did nothing alert at 10:31" deserves to find the
    /// answer rather than an absence. [`Self::active_scopes`] filters on `ends_at > now()`, so the
    /// suppression stops on the alerting config's next refresh either way.
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

    /// Delete a mute. Returns whether a row was removed.
    pub async fn delete_mute(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM mutes WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

/// Resolve active window scopes against the inventory: the set of nodes currently in
/// maintenance. Scope semantics mirror threshold resolution (ADR-013): node = the node id,
/// profile = the node's profile id, group = any tag value. The hierarchical
/// [`WindowScope::FolderGroup`] scope is resolved separately by the caller (it needs the group
/// edges + DB membership, via [`crate::groups::group_subtree`] + `NodeRepo::nodes_in_groups`),
/// so it is ignored here.
#[must_use]
pub fn nodes_in_maintenance(scopes: &[(WindowScope, String)], nodes: &[Node]) -> BTreeSet<NodeId> {
    let mut out = BTreeSet::new();
    for node in nodes {
        let covered = scopes.iter().any(|(level, scope_id)| match level {
            WindowScope::Node => *scope_id == node.id.to_string(),
            WindowScope::Profile => {
                node.profile.map(|p| p.to_string()).as_deref() == Some(scope_id.as_str())
            }
            WindowScope::Group => node.tags.values().any(|v| v == scope_id),
            WindowScope::FolderGroup => false,
            // No id to match: the deployment itself is out of service, so every node is.
            WindowScope::System => true,
        });
        if covered {
            out.insert(node.id);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use yagra_common::ProfileId;

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
        let set = nodes_in_maintenance(&scopes, &[a.clone(), b.clone()]);
        assert!(set.contains(&a.id));
        assert!(!set.contains(&b.id));
    }

    #[test]
    fn profile_scope_matches_profile_members() {
        let profile = ProfileId::new();
        let a = node(Some(profile), &[]);
        let b = node(None, &[]);
        let scopes = vec![(WindowScope::Profile, profile.to_string())];
        let set = nodes_in_maintenance(&scopes, &[a.clone(), b.clone()]);
        assert!(set.contains(&a.id));
        assert!(!set.contains(&b.id));
    }

    #[test]
    fn group_scope_matches_tag_values() {
        let a = node(None, &[("site", "tokyo")]);
        let b = node(None, &[("site", "osaka")]);
        let scopes = vec![(WindowScope::Group, "tokyo".to_owned())];
        let set = nodes_in_maintenance(&scopes, &[a.clone(), b.clone()]);
        assert!(set.contains(&a.id));
        assert!(!set.contains(&b.id));
    }

    #[test]
    fn folder_group_scope_is_resolved_elsewhere() {
        // FolderGroup needs the group tree + DB membership, so `nodes_in_maintenance` (in-memory,
        // tag-only) must ignore it — the caller unions the resolved node set. A folder-group scope
        // alone therefore yields nothing here, even for a node that happens to carry the id as a tag.
        let a = node(None, &[("site", "tokyo")]);
        let scopes = vec![(WindowScope::FolderGroup, Uuid::new_v4().to_string())];
        assert!(nodes_in_maintenance(&scopes, &[a]).is_empty());
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
        assert!(nodes_in_maintenance(&[], &[a]).is_empty());
    }
}
