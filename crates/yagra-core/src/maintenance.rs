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
use yagra_common::{Node, NodeId, ScopeLevel};

/// A stored maintenance window (API shape; times are RFC 3339 text at the edge).
#[derive(Debug, Clone, Serialize)]
pub struct StoredWindow {
    pub id: Uuid,
    pub name: String,
    pub level: ScopeLevel,
    pub scope_id: String,
    pub starts_at: String,
    pub ends_at: String,
    pub enabled: bool,
    /// Whether the window covers "now" (computed at read time for the UI).
    pub active: bool,
}

/// A stored mute (API shape). `check_name: None` mutes every check on the node.
#[derive(Debug, Clone, Serialize)]
pub struct StoredMute {
    pub id: Uuid,
    pub node_id: Uuid,
    pub check_name: Option<String>,
    pub until_at: String,
    pub reason: Option<String>,
}

fn parse_level(s: &str) -> ScopeLevel {
    match s {
        "node" => ScopeLevel::Node,
        "group" => ScopeLevel::Group,
        _ => ScopeLevel::Profile,
    }
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
                    level: parse_level(&row.try_get::<String, _>("scope_level")?),
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
    pub async fn active_scopes(&self) -> anyhow::Result<Vec<(ScopeLevel, String)>> {
        let rows = sqlx::query(
            "SELECT scope_level, scope_id FROM maintenance_windows \
             WHERE enabled AND starts_at <= now() AND ends_at > now()",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    parse_level(&row.try_get::<String, _>("scope_level")?),
                    row.try_get("scope_id")?,
                ))
            })
            .collect()
    }

    /// Unexpired mutes, soonest-expiring first. Expired rows are dropped lazily on read
    /// (they are inert either way — the notifier only loads unexpired ones).
    pub async fn list_mutes(&self) -> anyhow::Result<Vec<StoredMute>> {
        sqlx::query("DELETE FROM mutes WHERE until_at <= now()")
            .execute(&self.pool)
            .await?;
        let rows = sqlx::query(
            "SELECT id, node_id, check_name, until_at, reason FROM mutes \
             WHERE until_at > now() ORDER BY until_at",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let until: DateTime<Utc> = row.try_get("until_at")?;
                Ok(StoredMute {
                    id: row.try_get("id")?,
                    node_id: row.try_get("node_id")?,
                    check_name: row.try_get("check_name")?,
                    until_at: until.to_rfc3339(),
                    reason: row.try_get("reason")?,
                })
            })
            .collect()
    }

    /// Create a mute; returns its id.
    pub async fn create_mute(
        &self,
        node_id: Uuid,
        check_name: Option<&str>,
        until_at: DateTime<Utc>,
        reason: Option<&str>,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO mutes (id, node_id, check_name, until_at, reason) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(node_id)
        .bind(check_name)
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
/// profile = the node's profile id, group = any tag value.
#[must_use]
pub fn nodes_in_maintenance(scopes: &[(ScopeLevel, String)], nodes: &[Node]) -> BTreeSet<NodeId> {
    let mut out = BTreeSet::new();
    for node in nodes {
        let covered = scopes.iter().any(|(level, scope_id)| match level {
            ScopeLevel::Node => *scope_id == node.id.to_string(),
            ScopeLevel::Profile => {
                node.profile.map(|p| p.to_string()).as_deref() == Some(scope_id.as_str())
            }
            ScopeLevel::Group => node.tags.values().any(|v| v == scope_id),
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
        let scopes = vec![(ScopeLevel::Node, a.id.to_string())];
        let set = nodes_in_maintenance(&scopes, &[a.clone(), b.clone()]);
        assert!(set.contains(&a.id));
        assert!(!set.contains(&b.id));
    }

    #[test]
    fn profile_scope_matches_profile_members() {
        let profile = ProfileId::new();
        let a = node(Some(profile), &[]);
        let b = node(None, &[]);
        let scopes = vec![(ScopeLevel::Profile, profile.to_string())];
        let set = nodes_in_maintenance(&scopes, &[a.clone(), b.clone()]);
        assert!(set.contains(&a.id));
        assert!(!set.contains(&b.id));
    }

    #[test]
    fn group_scope_matches_tag_values() {
        let a = node(None, &[("site", "tokyo")]);
        let b = node(None, &[("site", "osaka")]);
        let scopes = vec![(ScopeLevel::Group, "tokyo".to_owned())];
        let set = nodes_in_maintenance(&scopes, &[a.clone(), b.clone()]);
        assert!(set.contains(&a.id));
        assert!(!set.contains(&b.id));
    }

    #[test]
    fn no_active_scopes_means_no_maintenance() {
        let a = node(None, &[("site", "tokyo")]);
        assert!(nodes_in_maintenance(&[], &[a]).is_empty());
    }
}
