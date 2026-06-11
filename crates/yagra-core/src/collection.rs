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

    /// The enabled collection items that apply to a node: its profile's items (if it has a
    /// profile) plus its node-level items, each tagged with the scope it came from. The
    /// scheduler resolves these (node overrides profile); an empty result means "fall back
    /// to the built-in catalog".
    pub async fn list_items_for_node(
        &self,
        node_id: Uuid,
        profile_id: Option<Uuid>,
    ) -> anyhow::Result<Vec<ScopedCollectionItem>> {
        const COLS: &str = "scope_level, metric_name, oid, collection, metric_kind";
        let rows = match profile_id {
            Some(profile) => {
                sqlx::query(&format!(
                    "SELECT {COLS} FROM collection_items \
                     WHERE enabled = true AND ((scope_level = 'node' AND scope_id = $1) \
                                            OR (scope_level = 'profile' AND scope_id = $2))"
                ))
                .bind(node_id)
                .bind(profile)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(&format!(
                    "SELECT {COLS} FROM collection_items \
                     WHERE enabled = true AND scope_level = 'node' AND scope_id = $1"
                ))
                .bind(node_id)
                .fetch_all(&self.pool)
                .await?
            }
        };
        rows.into_iter()
            .map(|row| {
                Ok(ScopedCollectionItem {
                    level: parse_scope_level(&row.try_get::<String, _>("scope_level")?),
                    item: CollectionItem {
                        metric_name: row.try_get("metric_name")?,
                        oid: row.try_get("oid")?,
                        kind: parse_collection_kind(&row.try_get::<String, _>("collection")?),
                        metric_kind: parse_metric_kind(&row.try_get::<String, _>("metric_kind")?),
                    },
                })
            })
            .collect()
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
}
