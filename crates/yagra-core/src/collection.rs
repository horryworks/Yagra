// SPDX-License-Identifier: AGPL-3.0-only
//! Collection-set persistence: which OIDs/metrics to collect, per profile and per node.
//!
//! Mirrors [`crate::thresholds::ThresholdStore`]: scope-based rows that the scheduler
//! resolves into an effective per-node set via [`yagra_common::resolve_collection_set`]
//! (a node-level item overrides the profile default with the same metric name). This is
//! the I/O adapter only; resolution and the built-in catalog live in `yagra-common`.

use serde::Serialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_common::{CollectionItem, CollectionKind, MetricKind, ScopeLevel, ScopedCollectionItem};

/// A stored collection item with its id and scope, for the API (the scheduler ignores id).
#[derive(Debug, Clone, Serialize)]
pub struct StoredCollectionItem {
    pub id: Uuid,
    pub scope_level: ScopeLevel,
    pub scope_id: Uuid,
    #[serde(flatten)]
    pub item: CollectionItem,
    pub enabled: bool,
}

fn parse_collection_kind(s: &str) -> CollectionKind {
    match s {
        "table" => CollectionKind::Table,
        _ => CollectionKind::Scalar,
    }
}

fn parse_metric_kind(s: &str) -> MetricKind {
    match s {
        "counter" => MetricKind::Counter,
        _ => MetricKind::Gauge,
    }
}

fn parse_scope_level(s: &str) -> ScopeLevel {
    match s {
        "node" => ScopeLevel::Node,
        "group" => ScopeLevel::Group,
        _ => ScopeLevel::Profile,
    }
}

/// PostgreSQL-backed collection-set store.
pub struct CollectionRepo {
    pool: PgPool,
}

impl CollectionRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The enabled collection items that apply to a node, each tagged with the scope it came
    /// from: **profile-scope** items from the templates attached to the node's profile, plus
    /// **node-scope** ad-hoc overrides. The scheduler resolves these (node overrides profile,
    /// duplicate metric names across templates dedup by name); an empty result means "fall
    /// back to the built-in catalog".
    pub async fn list_items_for_node(
        &self,
        node_id: Uuid,
        profile_id: Option<Uuid>,
    ) -> anyhow::Result<Vec<ScopedCollectionItem>> {
        let to_item = |metric_name: String,
                       oid: String,
                       collection: String,
                       metric_kind: String| CollectionItem {
            metric_name,
            oid,
            kind: parse_collection_kind(&collection),
            metric_kind: parse_metric_kind(&metric_kind),
        };

        let mut out: Vec<ScopedCollectionItem> = Vec::new();

        // Profile scope: metrics from every template attached to the node's profile.
        if let Some(profile) = profile_id {
            let rows = sqlx::query(
                "SELECT cti.metric_name, cti.oid, cti.collection, cti.metric_kind \
                 FROM profile_collection_templates pct \
                 JOIN collection_template_items cti ON cti.template_id = pct.template_id \
                 WHERE pct.profile_id = $1 AND cti.enabled = true",
            )
            .bind(profile)
            .fetch_all(&self.pool)
            .await?;
            for row in rows {
                out.push(ScopedCollectionItem {
                    level: ScopeLevel::Profile,
                    item: to_item(
                        row.try_get("metric_name")?,
                        row.try_get("oid")?,
                        row.try_get("collection")?,
                        row.try_get("metric_kind")?,
                    ),
                });
            }
        }

        // Node scope: the node's own ad-hoc overrides/additions.
        let rows = sqlx::query(
            "SELECT metric_name, oid, collection, metric_kind FROM collection_items \
             WHERE enabled = true AND scope_level = 'node' AND scope_id = $1",
        )
        .bind(node_id)
        .fetch_all(&self.pool)
        .await?;
        for row in rows {
            out.push(ScopedCollectionItem {
                level: ScopeLevel::Node,
                item: to_item(
                    row.try_get("metric_name")?,
                    row.try_get("oid")?,
                    row.try_get("collection")?,
                    row.try_get("metric_kind")?,
                ),
            });
        }
        Ok(out)
    }

    /// All collection items defined at one scope (for the API editor), with ids.
    pub async fn list_items(
        &self,
        scope_level: &str,
        scope_id: Uuid,
    ) -> anyhow::Result<Vec<StoredCollectionItem>> {
        let rows = sqlx::query(
            "SELECT id, scope_level, scope_id, metric_name, oid, collection, metric_kind, enabled \
             FROM collection_items WHERE scope_level = $1 AND scope_id = $2 ORDER BY metric_name",
        )
        .bind(scope_level)
        .bind(scope_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(StoredCollectionItem {
                    id: row.try_get("id")?,
                    scope_level: parse_scope_level(&row.try_get::<String, _>("scope_level")?),
                    scope_id: row.try_get("scope_id")?,
                    item: CollectionItem {
                        metric_name: row.try_get("metric_name")?,
                        oid: row.try_get("oid")?,
                        kind: parse_collection_kind(&row.try_get::<String, _>("collection")?),
                        metric_kind: parse_metric_kind(&row.try_get::<String, _>("metric_kind")?),
                    },
                    enabled: row.try_get("enabled")?,
                })
            })
            .collect()
    }

    /// Create (or update, on the unique scope+metric_name) a collection item; returns its id.
    /// Upserts so re-adding the same metric at a scope edits it rather than 409-ing.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_item(
        &self,
        scope_level: &str,
        scope_id: Uuid,
        metric_name: &str,
        oid: &str,
        collection: &str,
        metric_kind: &str,
        enabled: bool,
    ) -> anyhow::Result<Uuid> {
        let row = sqlx::query(
            "INSERT INTO collection_items \
                (id, scope_level, scope_id, metric_name, oid, collection, metric_kind, enabled) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (scope_level, scope_id, metric_name) DO UPDATE SET \
                oid = EXCLUDED.oid, collection = EXCLUDED.collection, \
                metric_kind = EXCLUDED.metric_kind, enabled = EXCLUDED.enabled \
             RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(scope_level)
        .bind(scope_id)
        .bind(metric_name)
        .bind(oid)
        .bind(collection)
        .bind(metric_kind)
        .bind(enabled)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("id")?)
    }

    /// Delete a collection item by id. Returns whether a row was removed.
    pub async fn delete_item(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM collection_items WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    // ── Collection templates (reusable metric bundles) ───────────────────────

    /// All collection templates with their metric counts (for the templates list + the
    /// profile attach picker).
    pub async fn list_templates(&self) -> anyhow::Result<Vec<TemplateSummary>> {
        let rows = sqlx::query(
            "SELECT t.id, t.name, t.description, count(i.id) AS item_count \
             FROM collection_templates t \
             LEFT JOIN collection_template_items i ON i.template_id = t.id \
             GROUP BY t.id, t.name, t.description ORDER BY t.name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(TemplateSummary {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    description: row.try_get("description")?,
                    item_count: row.try_get("item_count")?,
                })
            })
            .collect()
    }

    /// Create a template; returns its id, or [`CreateTemplateOutcome::NameTaken`] on a
    /// duplicate name (the `name` column is UNIQUE).
    pub async fn create_template(
        &self,
        name: &str,
        description: Option<&str>,
    ) -> anyhow::Result<CreateTemplateOutcome> {
        let id = Uuid::new_v4();
        let res = sqlx::query(
            "INSERT INTO collection_templates (id, name, description) VALUES ($1, $2, $3)",
        )
        .bind(id)
        .bind(name)
        .bind(description)
        .execute(&self.pool)
        .await;
        match res {
            Ok(_) => Ok(CreateTemplateOutcome::Created(id)),
            Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("23505") => {
                Ok(CreateTemplateOutcome::NameTaken)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Delete a template (cascades to its items + profile links). Returns whether it existed.
    pub async fn delete_template(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM collection_templates WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The metrics in a template (for the template editor).
    pub async fn list_template_items(
        &self,
        template_id: Uuid,
    ) -> anyhow::Result<Vec<TemplateItem>> {
        let rows = sqlx::query(
            "SELECT id, metric_name, oid, collection, metric_kind, enabled \
             FROM collection_template_items WHERE template_id = $1 ORDER BY metric_name",
        )
        .bind(template_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(TemplateItem {
                    id: row.try_get("id")?,
                    item: CollectionItem {
                        metric_name: row.try_get("metric_name")?,
                        oid: row.try_get("oid")?,
                        kind: parse_collection_kind(&row.try_get::<String, _>("collection")?),
                        metric_kind: parse_metric_kind(&row.try_get::<String, _>("metric_kind")?),
                    },
                    enabled: row.try_get("enabled")?,
                })
            })
            .collect()
    }

    /// Add (or update, on the unique template+metric_name) a metric in a template; returns its id.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_template_item(
        &self,
        template_id: Uuid,
        metric_name: &str,
        oid: &str,
        collection: &str,
        metric_kind: &str,
        enabled: bool,
    ) -> anyhow::Result<Uuid> {
        let row = sqlx::query(
            "INSERT INTO collection_template_items \
                (id, template_id, metric_name, oid, collection, metric_kind, enabled) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (template_id, metric_name) DO UPDATE SET \
                oid = EXCLUDED.oid, collection = EXCLUDED.collection, \
                metric_kind = EXCLUDED.metric_kind, enabled = EXCLUDED.enabled \
             RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(template_id)
        .bind(metric_name)
        .bind(oid)
        .bind(collection)
        .bind(metric_kind)
        .bind(enabled)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("id")?)
    }

    /// Delete a metric from a template (scoped to the template so a wrong id can't reach
    /// another template's row). Returns whether a row was removed.
    pub async fn delete_template_item(
        &self,
        template_id: Uuid,
        item_id: Uuid,
    ) -> anyhow::Result<bool> {
        let res =
            sqlx::query("DELETE FROM collection_template_items WHERE id = $1 AND template_id = $2")
                .bind(item_id)
                .bind(template_id)
                .execute(&self.pool)
                .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The templates attached to a profile.
    pub async fn list_profile_templates(
        &self,
        profile_id: Uuid,
    ) -> anyhow::Result<Vec<TemplateSummary>> {
        let rows = sqlx::query(
            "SELECT t.id, t.name, t.description, count(i.id) AS item_count \
             FROM profile_collection_templates pct \
             JOIN collection_templates t ON t.id = pct.template_id \
             LEFT JOIN collection_template_items i ON i.template_id = t.id \
             WHERE pct.profile_id = $1 \
             GROUP BY t.id, t.name, t.description ORDER BY t.name",
        )
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(TemplateSummary {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    description: row.try_get("description")?,
                    item_count: row.try_get("item_count")?,
                })
            })
            .collect()
    }

    /// Replace the set of templates attached to a profile (transactional).
    pub async fn set_profile_templates(
        &self,
        profile_id: Uuid,
        template_ids: &[Uuid],
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM profile_collection_templates WHERE profile_id = $1")
            .bind(profile_id)
            .execute(&mut *tx)
            .await?;
        for template_id in template_ids {
            sqlx::query(
                "INSERT INTO profile_collection_templates (profile_id, template_id) \
                 VALUES ($1, $2) ON CONFLICT DO NOTHING",
            )
            .bind(profile_id)
            .bind(template_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

/// A collection template row for the API (id + name + description + metric count).
#[derive(Debug, Clone, Serialize)]
pub struct TemplateSummary {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub item_count: i64,
}

/// One metric in a template, with its id, for the template editor.
#[derive(Debug, Clone, Serialize)]
pub struct TemplateItem {
    pub id: Uuid,
    #[serde(flatten)]
    pub item: CollectionItem,
    pub enabled: bool,
}

/// Result of creating a template: its new id, or that the name is already taken.
pub enum CreateTemplateOutcome {
    Created(Uuid),
    NameTaken,
}
