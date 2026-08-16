// SPDX-License-Identifier: AGPL-3.0-only
//! MIB repository (Phase B): a curated, searchable OID catalog.
//!
//! A reference table of `metric_name → (oid, kind)` so operators pick metrics by name in the
//! collection editor instead of typing raw OIDs. Seeded on boot from the built-in standard
//! catalog + the vendor (Cisco/Huawei) OID sets ([`yagra_common::builtin_catalog`] /
//! [`yagra_common::builtin_templates`]); operators can add their own. Not part of the polling
//! path — purely a lookup aid.

use serde::Serialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_common::{
    builtin_catalog, builtin_templates, CollectionItem, CollectionKind, MetricKind,
    TEMPLATE_STANDARD_SNMP,
};

/// One catalog entry returned by the API.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct MibEntry {
    pub id: Uuid,
    pub metric_name: String,
    pub oid: String,
    pub collection: String,
    pub metric_kind: String,
    pub vendor: Option<String>,
    pub description: Option<String>,
}

fn collection_str(k: CollectionKind) -> &'static str {
    // Shared with the seeder and the reader (`CollectionKind::from_token`) so the three cannot
    // drift — a token only two of them knew is what made ADR-062 silently collect nothing.
    k.as_str()
}

fn metric_kind_str(k: MetricKind) -> &'static str {
    match k {
        MetricKind::Gauge => "gauge",
        MetricKind::Counter => "counter",
    }
}

/// PostgreSQL-backed OID catalog.
pub struct MibRepo {
    pool: PgPool,
}

impl MibRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// List catalog entries, optionally filtered by a case-insensitive substring on the
    /// metric name / OID / vendor. Ordered by metric name, capped at `limit` rows.
    ///
    /// The bound is not optional. This query used to have none on either edge — the REST handler
    /// passed the term straight through — which was survivable while the only caller was a settings
    /// screen an operator opened by hand, and stopped being survivable when the catalog became
    /// reachable from an MCP tool (ADR-042 I3b). The caller clamps; this signature only refuses to
    /// let anyone forget.
    pub async fn list(&self, q: Option<&str>, limit: i64) -> anyhow::Result<Vec<MibEntry>> {
        let rows = sqlx::query(
            "SELECT id, metric_name, oid, collection, metric_kind, vendor, description \
             FROM mib_catalog \
             WHERE $1 IS NULL \
                OR metric_name ILIKE '%' || $1 || '%' \
                OR oid ILIKE '%' || $1 || '%' \
                OR vendor ILIKE '%' || $1 || '%' \
             ORDER BY metric_name \
             LIMIT $2",
        )
        .bind(q)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(MibEntry {
                    id: row.try_get("id")?,
                    metric_name: row.try_get("metric_name")?,
                    oid: row.try_get("oid")?,
                    collection: row.try_get("collection")?,
                    metric_kind: row.try_get("metric_kind")?,
                    vendor: row.try_get("vendor")?,
                    description: row.try_get("description")?,
                })
            })
            .collect()
    }

    /// Create a catalog entry. Returns `Ok(None)` if the metric name is already taken.
    pub async fn create(
        &self,
        metric_name: &str,
        oid: &str,
        collection: &str,
        metric_kind: &str,
        vendor: Option<&str>,
        description: Option<&str>,
    ) -> anyhow::Result<Option<Uuid>> {
        let id = Uuid::new_v4();
        let row = sqlx::query(
            "INSERT INTO mib_catalog \
                (id, metric_name, oid, collection, metric_kind, vendor, description) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (metric_name) DO NOTHING RETURNING id",
        )
        .bind(id)
        .bind(metric_name)
        .bind(oid)
        .bind(collection)
        .bind(metric_kind)
        .bind(vendor)
        .bind(description)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|_| id))
    }

    /// Delete a catalog entry. Returns whether a row was removed.
    pub async fn delete(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM mib_catalog WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Seed the catalog from the built-in OIDs (idempotent: stable ids + `ON CONFLICT DO
    /// NOTHING`, so operator-added/edited rows are never clobbered). Standard interface/system
    /// metrics carry no vendor; vendor-template metrics are tagged with the template name.
    pub async fn seed_builtin(&self) -> anyhow::Result<()> {
        // Stable id per metric name so re-seeding doesn't duplicate.
        let stable_id =
            |metric: &str| Uuid::new_v5(&Uuid::NAMESPACE_OID, format!("mib:{metric}").as_bytes());
        let mut seed: Vec<(CollectionItem, Option<&str>)> =
            builtin_catalog().into_iter().map(|i| (i, None)).collect();
        for tpl in builtin_templates() {
            if tpl.name == TEMPLATE_STANDARD_SNMP {
                continue; // already covered by builtin_catalog (vendor-less standard set)
            }
            for item in tpl.items {
                seed.push((item, Some(tpl.name)));
            }
        }
        for (item, vendor) in seed {
            sqlx::query(
                "INSERT INTO mib_catalog \
                    (id, metric_name, oid, collection, metric_kind, vendor) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (metric_name) DO NOTHING",
            )
            .bind(stable_id(&item.metric_name))
            .bind(&item.metric_name)
            .bind(&item.oid)
            .bind(collection_str(item.kind))
            .bind(metric_kind_str(item.metric_kind))
            .bind(vendor)
            .execute(&self.pool)
            .await?;
        }
        tracing::info!("seeded built-in MIB catalog");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_tokens_match_db_values() {
        assert_eq!(collection_str(CollectionKind::Scalar), "scalar");
        assert_eq!(collection_str(CollectionKind::Table), "table");
        assert_eq!(metric_kind_str(MetricKind::Gauge), "gauge");
        assert_eq!(metric_kind_str(MetricKind::Counter), "counter");
    }

    #[test]
    fn builtin_catalog_is_seedable_and_vendor_templates_excluded_from_standard() {
        // The standard catalog has entries (seeded vendor-less), and the vendor templates
        // (Cisco/Huawei) are distinct named bundles that get vendor tags.
        assert!(!builtin_catalog().is_empty());
        let vendor_templates: Vec<_> = builtin_templates()
            .into_iter()
            .filter(|t| t.name != TEMPLATE_STANDARD_SNMP)
            .collect();
        assert!(vendor_templates.iter().any(|t| t.name.contains("Huawei")));
    }
}
