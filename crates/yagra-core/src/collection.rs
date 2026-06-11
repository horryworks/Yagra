//! Collection-set persistence: which OIDs/metrics to collect, per profile and per node.
//!
//! Mirrors [`crate::thresholds::ThresholdStore`]: scope-based rows that the scheduler
//! resolves into an effective per-node set via [`yagra_common::resolve_collection_set`]
//! (a node-level item overrides the profile default with the same metric name). This is
//! the I/O adapter only; resolution and the built-in catalog live in `yagra-common`.

use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_common::{CollectionItem, CollectionKind, MetricKind, ScopeLevel, ScopedCollectionItem};

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
}
