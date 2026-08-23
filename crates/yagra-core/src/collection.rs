// SPDX-License-Identifier: AGPL-3.0-only
//! Collection-set persistence: which OIDs/metrics to collect, per profile and per node.
//!
//! Mirrors [`crate::thresholds::ThresholdStore`]: scope-based rows that the scheduler
//! resolves into an effective per-node set via [`yagra_common::resolve_collection_set`]
//! (a node-level item overrides the profile default with the same metric name). This is
//! the I/O adapter only; resolution and the built-in catalog live in `yagra-common`.

use serde::Serialize;
use sqlx::{PgPool, Row};
use std::collections::BTreeSet;
use uuid::Uuid;
use yagra_common::{CollectionItem, CollectionKind, MetricKind, ScopeLevel, ScopedCollectionItem};

/// A stored collection item with its id and scope, for the API (the scheduler ignores id).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct StoredCollectionItem {
    pub id: Uuid,
    pub scope_level: ScopeLevel,
    pub scope_id: Uuid,
    #[serde(flatten)]
    pub item: CollectionItem,
    pub enabled: bool,
}

/// Read the stored `collection` token back into its kind.
///
/// 🚨 **This used to be its own `match` ending in `_ => Scalar`, and that wildcard shipped a silent
/// total failure.** ADR-062's `optical` rows were seeded, read back as scalars, dispatched as SNMP
/// GETs of a table root, and produced nothing — with no error, no log line, and a green test suite,
/// because every test constructed the enum directly instead of going through the database. The
/// token list now lives on the enum ([`CollectionKind::from_token`]) so adding a variant cannot
/// leave a reader behind.
///
/// An unknown token still has to mean *something* here, since the row exists and the query already
/// succeeded. `Scalar` remains the fallback — it is the shape that asks the device for one OID and
/// stops — but it is now reached only by a token no binary knows, and it says so.
fn parse_collection_kind(s: &str) -> CollectionKind {
    CollectionKind::from_token(s).unwrap_or_else(|| {
        tracing::warn!(
            token = %s,
            "unknown collection kind in the database — treating it as a scalar GET. This row was \
             written by a newer core; the metric it names will not be collected correctly until \
             this binary is upgraded."
        );
        CollectionKind::Scalar
    })
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

    /// Whether any stored collection item or template item declares `metric_name` as a raw
    /// counter. Consulted by threshold creation: a counter's sampled value is monotonic, so a
    /// fixed bound cannot be evaluated against it (rates are query-time derived, ADR-012).
    pub async fn metric_declared_counter(&self, metric_name: &str) -> anyhow::Result<bool> {
        let row = sqlx::query(
            "SELECT EXISTS(SELECT 1 FROM collection_items \
                    WHERE metric_name = $1 AND metric_kind = 'counter') \
                 OR EXISTS(SELECT 1 FROM collection_template_items \
                    WHERE metric_name = $1 AND metric_kind = 'counter') AS is_counter",
        )
        .bind(metric_name)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("is_counter")?)
    }

    /// Every metric name that publishes **one series per interface** (ADR-076).
    ///
    /// The union of the built-in catalogue and both operator-editable item tables, answered by
    /// [`yagra_common::item_publishes_per_interface`] — the OID rules it applies are the only
    /// thing that can decide this, and they live in Rust because `ifindex` is a row key rather
    /// than a port number (ADR-011). The engine needs the set, not a per-metric probe, because it
    /// asks the question once per distinct metric on every poll result.
    ///
    /// Both item tables are consulted, exactly as [`Self::metric_declared_counter`] does: an item
    /// defined on a template and one defined at a scope are the same thing to a poller, and
    /// consulting only one would silently leave half the fleet's interface metrics sharing a
    /// single check.
    pub async fn per_interface_metric_names(&self) -> anyhow::Result<BTreeSet<String>> {
        let mut out: BTreeSet<String> = yagra_common::builtin_catalog()
            .iter()
            .filter(|i| yagra_common::item_publishes_per_interface(i))
            .map(|i| i.metric_name.clone())
            .collect();
        let rows = sqlx::query(
            "SELECT metric_name, oid, collection FROM collection_items              UNION              SELECT metric_name, oid, collection FROM collection_template_items",
        )
        .fetch_all(&self.pool)
        .await?;
        for row in rows {
            let item = CollectionItem {
                metric_name: row.try_get("metric_name")?,
                oid: row.try_get("oid")?,
                kind: parse_collection_kind(&row.try_get::<String, _>("collection")?),
                // Unread by `item_publishes_per_interface`; the dimension is decided by the OID
                // and the collection kind, never by whether the value is a counter.
                metric_kind: yagra_common::MetricKind::Gauge,
            };
            if yagra_common::item_publishes_per_interface(&item) {
                out.insert(item.metric_name);
            }
        }
        Ok(out)
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
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct TemplateSummary {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub item_count: i64,
}

/// One metric in a template, with its id, for the template editor.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// This module's code, comments stripped — see
    /// [`crate::module_source::code_no_comments`] for why both.
    fn production_source() -> String {
        crate::module_source::code_no_comments("src", "collection")
    }

    #[test]
    fn an_unknown_stored_token_degrades_to_the_safe_default() {
        // These columns carry what some build wrote, and a newer core may use a token this one has
        // never seen. Each fallback is the conservative reading: a `Scalar` collection fetches one
        // OID instead of walking a table, and a `Gauge` is used as-is rather than being treated as
        // a counter — the direction that cannot fabricate a rate.
        assert_eq!(parse_collection_kind("nonsense"), CollectionKind::Scalar);
        assert_eq!(parse_metric_kind("nonsense"), MetricKind::Gauge);
        assert_eq!(parse_scope_level("nonsense"), ScopeLevel::Profile);
    }

    #[test]
    fn the_parsers_round_trip_every_token_the_writers_store() {
        // `create_item` / `create_template_item` bind the caller's string straight into the column,
        // so reader and writer must agree on the whole vocabulary or a stored row reads back as
        // something else entirely.
        assert_eq!(parse_collection_kind("table"), CollectionKind::Table);
        assert_eq!(parse_collection_kind("scalar"), CollectionKind::Scalar);
        assert_eq!(parse_metric_kind("counter"), MetricKind::Counter);
        assert_eq!(parse_metric_kind("gauge"), MetricKind::Gauge);
        assert_eq!(parse_scope_level("node"), ScopeLevel::Node);
        assert_eq!(parse_scope_level("group"), ScopeLevel::Group);
        assert_eq!(parse_scope_level("profile"), ScopeLevel::Profile);
    }

    #[test]
    fn the_counter_probe_asks_both_tables_because_a_metric_can_live_in_either() {
        // A metric is a counter if *any* stored item says so — ad-hoc node items and template
        // items are two independent places an operator can declare one, and the threshold guard
        // that consumes this must not be fooled by checking only one of them.
        let src = production_source();
        assert!(src.contains("FROM collection_items"));
        assert!(src.contains("FROM collection_template_items"));
        let probe = src
            .split_once("pub async fn metric_declared_counter")
            .expect("the counter probe exists")
            .1;
        let body = probe.split_once("pub async fn").map_or(probe, |(b, _)| b);
        assert_eq!(
            body.matches("metric_kind = 'counter'").count(),
            2,
            "both item tables must be consulted, or a counter threshold slips through"
        );
        assert!(
            body.contains("$1"),
            "the metric name must be bound, never interpolated"
        );
    }

    #[test]
    fn deleting_a_template_item_is_scoped_to_its_template() {
        // Without the template_id in the WHERE, a wrong id would reach across and delete another
        // template's row — a cross-tenant delete by typo.
        assert!(production_source()
            .contains("DELETE FROM collection_template_items WHERE id = $1 AND template_id = $2"));
    }

    #[test]
    fn replacing_a_profiles_templates_is_transactional() {
        // The delete-then-insert would otherwise be able to land half-applied, leaving a profile
        // collecting nothing at all.
        let src = production_source();
        let replace = src
            .split_once("pub async fn set_profile_templates")
            .expect("the replace exists")
            .1;
        assert!(replace.contains("self.pool.begin()"));
        assert!(replace.contains("tx.commit()"));
        assert!(replace.contains("DELETE FROM profile_collection_templates WHERE profile_id = $1"));
    }

    #[test]
    fn a_duplicate_template_name_is_a_named_outcome_not_an_opaque_error() {
        // 23505 is the unique violation on `collection_templates.name`; mapping it here is what
        // lets the API answer 409 instead of 500.
        let src = production_source();
        assert!(src.contains(r#"e.code().as_deref() == Some("23505")"#));
        assert!(src.contains("CreateTemplateOutcome::NameTaken"));
    }

    #[test]
    fn every_statement_binds_its_values_instead_of_interpolating_them() {
        // Metric names, OIDs and scope levels all arrive from the API edge.
        let src = production_source();
        for builder in ["format!(", "push_str("] {
            assert!(
                !src.contains(builder),
                "SQL may be being built by string concatenation ({builder}); bind the value instead"
            );
        }
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    /// The reader accepts every token the writer can produce.
    ///
    /// 🚨 The bug this pins was invisible from either side alone: the seeder wrote `optical`
    /// correctly and the enum handled it correctly, but the reader in between fell through a
    /// wildcard to `Scalar`. The scheduler then built an SNMP GET of a table root, the optical
    /// probe was never created, and `list_node_metrics` reported the metric as node-level — all
    /// without an error or a log line. Only a test that crosses the writer/reader seam sees it.
    #[test]
    fn the_reader_accepts_every_token_the_seeder_writes() {
        for kind in CollectionKind::ALL {
            assert_eq!(
                parse_collection_kind(kind.as_str()),
                kind,
                "{kind:?} does not survive the database round trip"
            );
        }
    }

    /// An unknown token degrades to a scalar GET rather than failing the whole query — an older
    /// core must still be able to read a newer core's rows, losing only the metric it cannot run.
    #[test]
    fn an_unknown_token_degrades_instead_of_failing() {
        assert_eq!(
            parse_collection_kind("from_the_future"),
            CollectionKind::Scalar
        );
    }

    #[test]
    fn metric_kinds_round_trip_too() {
        assert_eq!(parse_metric_kind("counter"), MetricKind::Counter);
        assert_eq!(parse_metric_kind("gauge"), MetricKind::Gauge);
    }
}
