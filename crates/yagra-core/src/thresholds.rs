// SPDX-License-Identifier: AGPL-3.0-only
//! Threshold persistence (Workstream #1).
//!
//! Stores scope-based threshold rules; the alert engine resolves them per (node, metric)
//! via [`yagra_common::resolve_effective`] (most-specific-wins, most-restrictive tie-break,
//! ADR-013). This is the I/O adapter; the resolution + evaluation logic is in `yagra-common`
//! (tested there) and the firing logic in [`crate::alerts`].

use serde::Serialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_common::{Direction, ScopeLevel, ThresholdRule};

/// A stored threshold rule with its scope and id (id is for the API; the engine ignores it).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct StoredThreshold {
    pub id: Uuid,
    // Serialized as `scope_level` so the GET response matches the POST body field name.
    #[serde(rename = "scope_level")]
    pub level: ScopeLevel,
    pub scope_id: String,
    #[serde(flatten)]
    pub rule: ThresholdRule,
}

fn parse_level(s: &str) -> ScopeLevel {
    match s {
        "node" => ScopeLevel::Node,
        "group" => ScopeLevel::Group,
        _ => ScopeLevel::Profile,
    }
}

fn parse_direction(s: &str) -> Direction {
    match s {
        "below" => Direction::Below,
        _ => Direction::Above,
    }
}

/// PostgreSQL-backed threshold store.
pub struct ThresholdStore {
    pool: PgPool,
}

impl ThresholdStore {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Columns every read below selects, in the order [`Self::row_to_threshold`] expects.
    const COLUMNS: &'static str =
        "id, scope_level, scope_id, metric, direction, warning, critical, dwell_samples";

    /// **Every** threshold rule — the alert engine snapshots these to evaluate against.
    ///
    /// Deliberately uncapped, and must stay so: a truncated snapshot is not a shorter list, it is
    /// an alert engine that silently stops evaluating some rules. The *API* read is capped
    /// separately ([`Self::list_page`]) because a browser rendering the whole fleet's node-level
    /// overrides is a different problem from the engine needing all of them.
    pub async fn list_all(&self) -> anyhow::Result<Vec<StoredThreshold>> {
        let rows = sqlx::query(&format!("SELECT {} FROM thresholds", Self::COLUMNS))
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(Self::row_to_threshold).collect()
    }

    /// One page of threshold rules for the API, plus the total, so the caller can tell the operator
    /// how many were withheld rather than silently showing a prefix.
    ///
    /// Ordered so the page is stable and readable: broadest scope first (profile → group → node),
    /// then by metric. Without an `ORDER BY`, PostgreSQL is free to return a different arbitrary
    /// subset each time the same page is fetched.
    pub async fn list_page(&self, limit: i64) -> anyhow::Result<(Vec<StoredThreshold>, i64)> {
        let total: i64 = sqlx::query_scalar("SELECT count(*) FROM thresholds")
            .fetch_one(&self.pool)
            .await?;
        let rows = sqlx::query(&format!(
            "SELECT {} FROM thresholds \
             ORDER BY CASE scope_level WHEN 'profile' THEN 0 WHEN 'group' THEN 1 ELSE 2 END, \
             metric, scope_id LIMIT $1",
            Self::COLUMNS
        ))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let items = rows
            .into_iter()
            .map(Self::row_to_threshold)
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok((items, total))
    }

    fn row_to_threshold(row: sqlx::postgres::PgRow) -> anyhow::Result<StoredThreshold> {
        let dwell: i32 = row.try_get("dwell_samples")?;
        Ok(StoredThreshold {
            id: row.try_get("id")?,
            level: parse_level(&row.try_get::<String, _>("scope_level")?),
            scope_id: row.try_get("scope_id")?,
            rule: ThresholdRule {
                metric: row.try_get("metric")?,
                direction: parse_direction(&row.try_get::<String, _>("direction")?),
                warning: row.try_get("warning")?,
                critical: row.try_get("critical")?,
                dwell_samples: u32::try_from(dwell).unwrap_or(1),
            },
        })
    }

    /// Create a threshold rule; returns its id.
    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        &self,
        scope_level: &str,
        scope_id: &str,
        metric: &str,
        direction: &str,
        warning: Option<f64>,
        critical: Option<f64>,
        dwell_samples: i32,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO thresholds \
             (id, scope_level, scope_id, metric, direction, warning, critical, dwell_samples) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(id)
        .bind(scope_level)
        .bind(scope_id)
        .bind(metric)
        .bind(direction)
        .bind(warning)
        .bind(critical)
        .bind(dwell_samples.max(1))
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Delete a threshold rule. Returns whether a row was removed.
    pub async fn delete(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM thresholds WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}
