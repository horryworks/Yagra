// SPDX-License-Identifier: AGPL-3.0-only
//! Reading a deployment's configuration **out** (ADR-040 decision 3).
//!
//! Sixteen tables plus the two carried columns of `app_settings`, in [`super::BUNDLE_TABLES`]
//! order, each row filtered through [`crate::seed_ids::is_builtin`] so a target's own seeded rows
//! are never re-keyed by an import.
//!
//! 🚨 **Nothing here writes**, and that is a check rather than a convention:
//! `super::guards` refuses an `INSERT`/`UPDATE`/`DELETE` in this file, and refuses any mention of a
//! sealed column. `GET /api/v1/config/bundle` is gated as tightly as the import precisely because
//! what it returns is the whole configuration — see `api/config_bundle.rs`.
//!
//! Not split further: the split line elsewhere in this module is a dependency chain, and there is
//! none here. One value crosses a block boundary (`carried_templates`) and it is consumed in the
//! block immediately below the one that makes it. Cutting on table position would be an arbitrary
//! line, not a rule anyone could re-apply.

use super::*;
use crate::seed_ids;
use chrono::Utc;
use sqlx::Row;
use std::collections::HashSet;
use uuid::Uuid;

impl ConfigBundleRepo {
    /// Build a bundle from the current configuration.
    pub async fn export(&self) -> Result<ConfigBundle, BundleError> {
        let mut notes = Notes::default();
        let mut conn = self.pool.acquire().await?;

        let mut profiles = Vec::new();
        for row in sqlx::query(
            "SELECT id, name, parent_id, category, vendor, poll_interval_secs \
             FROM profiles ORDER BY name",
        )
        .fetch_all(&mut *conn)
        .await?
        {
            let id: Uuid = row.try_get("id")?;
            if seed_ids::is_builtin(id) {
                notes.add("profiles", NoteCode::SkippedBuiltin, None);
                continue;
            }
            profiles.push(ProfileRow {
                id,
                name: row.try_get("name")?,
                parent_id: row.try_get("parent_id")?,
                category: row.try_get("category")?,
                vendor: row.try_get("vendor")?,
                poll_interval_secs: row.try_get("poll_interval_secs")?,
            });
        }
        cap("profiles", profiles.len())?;

        let mut collection_templates = Vec::new();
        for row in
            sqlx::query("SELECT id, name, description FROM collection_templates ORDER BY name")
                .fetch_all(&mut *conn)
                .await?
        {
            let id: Uuid = row.try_get("id")?;
            if seed_ids::is_builtin(id) {
                notes.add("collection_templates", NoteCode::SkippedBuiltin, None);
                continue;
            }
            collection_templates.push(CollectionTemplateRow {
                id,
                name: row.try_get("name")?,
                description: row.try_get("description")?,
            });
        }
        cap("collection_templates", collection_templates.len())?;

        let carried_templates: HashSet<Uuid> = collection_templates.iter().map(|t| t.id).collect();
        let mut collection_template_items = Vec::new();
        for row in sqlx::query(
            "SELECT id, template_id, metric_name, oid, collection, metric_kind, enabled \
             FROM collection_template_items ORDER BY template_id, metric_name",
        )
        .fetch_all(&mut *conn)
        .await?
        {
            let template_id: Uuid = row.try_get("template_id")?;
            // An item of a built-in template travels nowhere: the target seeds the template and its
            // items together, and carrying the items alone would attach them to a template whose id
            // may map to a different metric set there.
            if !carried_templates.contains(&template_id) {
                continue;
            }
            collection_template_items.push(CollectionTemplateItemRow {
                id: row.try_get("id")?,
                template_id,
                metric_name: row.try_get("metric_name")?,
                oid: row.try_get("oid")?,
                collection: row.try_get("collection")?,
                metric_kind: row.try_get("metric_kind")?,
                enabled: row.try_get("enabled")?,
            });
        }
        cap("collection_template_items", collection_template_items.len())?;

        let mut profile_collection_templates = Vec::new();
        for row in sqlx::query(
            "SELECT profile_id, template_id FROM profile_collection_templates \
             ORDER BY profile_id, template_id",
        )
        .fetch_all(&mut *conn)
        .await?
        {
            let profile_id: Uuid = row.try_get("profile_id")?;
            let template_id: Uuid = row.try_get("template_id")?;
            // A link is carried when the *profile* is an operator's. The template may well be a
            // built-in — the target has it under the same reserved id, so the link still resolves.
            if seed_ids::is_builtin(profile_id) {
                continue;
            }
            profile_collection_templates.push(ProfileTemplateLink {
                profile_id,
                template_id,
            });
        }
        cap(
            "profile_collection_templates",
            profile_collection_templates.len(),
        )?;

        let mut classification_rules = Vec::new();
        for row in sqlx::query(
            "SELECT id, priority, sysobjectid_prefix, sysdescr_regex, profile_id, vendor, model, \
                    enabled \
             FROM classification_rules ORDER BY priority, id",
        )
        .fetch_all(&mut *conn)
        .await?
        {
            let id: Uuid = row.try_get("id")?;
            if seed_ids::is_builtin(id) {
                notes.add("classification_rules", NoteCode::SkippedBuiltin, None);
                continue;
            }
            classification_rules.push(ClassificationRuleRow {
                id,
                priority: row.try_get("priority")?,
                sysobjectid_prefix: row.try_get("sysobjectid_prefix")?,
                sysdescr_regex: row.try_get("sysdescr_regex")?,
                profile_id: row.try_get("profile_id")?,
                vendor: row.try_get("vendor")?,
                model: row.try_get("model")?,
                enabled: row.try_get("enabled")?,
            });
        }
        cap("classification_rules", classification_rules.len())?;

        let mut node_groups = Vec::new();
        for row in sqlx::query(
            "SELECT id, name, group_type, parent_id, sort_order, latitude, longitude, pool \
             FROM node_groups ORDER BY sort_order, name",
        )
        .fetch_all(&mut *conn)
        .await?
        {
            node_groups.push(NodeGroupRow {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
                group_type: row.try_get("group_type")?,
                parent_id: row.try_get("parent_id")?,
                sort_order: row.try_get("sort_order")?,
                latitude: row.try_get("latitude")?,
                longitude: row.try_get("longitude")?,
                pool: row.try_get("pool")?,
            });
        }
        cap("node_groups", node_groups.len())?;

        let mut nodes = Vec::new();
        for row in sqlx::query(
            "SELECT id, name, parent_id, host(address) AS address, profile_id, group_id, \
                    credential_id, pool, vendor, model, sort_order, tags \
             FROM nodes ORDER BY sort_order, name",
        )
        .fetch_all(&mut *conn)
        .await?
        {
            nodes.push(NodeRow {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
                parent_id: row.try_get("parent_id")?,
                address: row.try_get("address")?,
                profile_id: row.try_get("profile_id")?,
                group_id: row.try_get("group_id")?,
                credential_id: row.try_get("credential_id")?,
                pool: row.try_get("pool")?,
                vendor: row.try_get("vendor")?,
                model: row.try_get("model")?,
                sort_order: row.try_get("sort_order")?,
                tags: row.try_get("tags")?,
            });
        }
        cap("nodes", nodes.len())?;

        let mut thresholds = Vec::new();
        for row in sqlx::query(
            "SELECT id, scope_level, scope_id, scope_ids, metric, direction, warning, critical, \
                    warning_below, critical_below, warning_above, critical_above, dwell_samples \
             FROM thresholds ORDER BY metric, id",
        )
        .fetch_all(&mut *conn)
        .await?
        {
            let id: Uuid = row.try_get("id")?;
            if seed_ids::is_builtin(id) {
                notes.add("thresholds", NoteCode::SkippedBuiltin, None);
                continue;
            }
            thresholds.push(ThresholdRow {
                id,
                scope_level: row.try_get("scope_level")?,
                scope_id: row.try_get("scope_id")?,
                scope_ids: row.try_get("scope_ids")?,
                metric: row.try_get("metric")?,
                direction: row.try_get("direction")?,
                warning: row.try_get("warning")?,
                critical: row.try_get("critical")?,
                warning_below: row.try_get("warning_below")?,
                critical_below: row.try_get("critical_below")?,
                warning_above: row.try_get("warning_above")?,
                critical_above: row.try_get("critical_above")?,
                dwell_samples: row.try_get("dwell_samples")?,
            });
        }
        cap("thresholds", thresholds.len())?;

        let mut url_checks = Vec::new();
        for row in sqlx::query(
            "SELECT node_id, url, method, expected_status, verify_tls, follow_redirects, \
                    timeout_ms, credential_id, body_match, json_extract, body_max_bytes \
             FROM url_checks ORDER BY node_id",
        )
        .fetch_all(&mut *conn)
        .await?
        {
            url_checks.push(UrlCheckRow {
                node_id: row.try_get("node_id")?,
                url: row.try_get("url")?,
                method: row.try_get("method")?,
                expected_status: row.try_get("expected_status")?,
                verify_tls: row.try_get("verify_tls")?,
                follow_redirects: row.try_get("follow_redirects")?,
                timeout_ms: row.try_get("timeout_ms")?,
                credential_id: row.try_get("credential_id")?,
                body_match: row.try_get("body_match")?,
                json_extract: row.try_get("json_extract")?,
                body_max_bytes: row.try_get("body_max_bytes")?,
            });
        }
        cap("url_checks", url_checks.len())?;

        let mut dns_checks = Vec::new();
        for row in sqlx::query(
            "SELECT node_id, name, record_type, host(resolver_ip) AS resolver_ip, resolver_port, \
                    max_depth, timeout_ms \
             FROM dns_checks ORDER BY node_id",
        )
        .fetch_all(&mut *conn)
        .await?
        {
            dns_checks.push(DnsCheckRow {
                node_id: row.try_get("node_id")?,
                name: row.try_get("name")?,
                record_type: row.try_get("record_type")?,
                resolver_ip: row.try_get("resolver_ip")?,
                resolver_port: row.try_get("resolver_port")?,
                max_depth: row.try_get("max_depth")?,
                timeout_ms: row.try_get("timeout_ms")?,
            });
        }
        cap("dns_checks", dns_checks.len())?;

        // A NULL test on the wrapped-key column, never the sealed bytes: this query must not select
        // a sealed column at all (see `no_export_query_names_a_sealed_column`), and the table's
        // all-or-none CHECK makes any one of the five columns a faithful presence test.
        let mut forward_destinations = Vec::new();
        for row in sqlx::query(
            "SELECT id, name, enabled, source_kind, dest_kind, target, pool, verbatim, filter, \
                    rate_limit_per_sec, ca_cert, (key_id IS NOT NULL) AS had_secret \
             FROM forward_destinations ORDER BY name",
        )
        .fetch_all(&mut *conn)
        .await?
        {
            let had_secret: bool = row.try_get("had_secret")?;
            if had_secret {
                notes.add(
                    "forward_destinations",
                    NoteCode::SecretDroppedImportedDisabled,
                    None,
                );
            }
            forward_destinations.push(ForwardDestinationRow {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
                enabled: row.try_get("enabled")?,
                source_kind: row.try_get("source_kind")?,
                dest_kind: row.try_get("dest_kind")?,
                target: row.try_get("target")?,
                pool: row.try_get("pool")?,
                verbatim: row.try_get("verbatim")?,
                filter: row.try_get("filter")?,
                rate_limit_per_sec: row.try_get("rate_limit_per_sec")?,
                ca_cert: row.try_get("ca_cert")?,
                had_secret,
            });
        }
        cap("forward_destinations", forward_destinations.len())?;

        let mut event_sources = Vec::new();
        for row in
            sqlx::query("SELECT id, name, kind, enabled, node_id FROM event_sources ORDER BY name")
                .fetch_all(&mut *conn)
                .await?
        {
            let kind: String = row.try_get("kind")?;
            if kind == WEBHOOK_KIND {
                notes.add("event_sources", NoteCode::WebhookTokenReset, None);
            }
            event_sources.push(EventSourceRow {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
                kind,
                enabled: row.try_get("enabled")?,
                node_id: row.try_get("node_id")?,
            });
        }
        cap("event_sources", event_sources.len())?;

        let mut event_rules = Vec::new();
        for row in sqlx::query(
            "SELECT id, name, enabled, source_kind, source_id, node_id, match_kind, pattern, \
                    clear_pattern, severity, ttl_secs, min_count, window_secs \
             FROM event_rules ORDER BY name",
        )
        .fetch_all(&mut *conn)
        .await?
        {
            let id: Uuid = row.try_get("id")?;
            if seed_ids::is_builtin(id) {
                notes.add("event_rules", NoteCode::SkippedBuiltin, None);
                continue;
            }
            event_rules.push(EventRuleRow {
                id,
                name: row.try_get("name")?,
                enabled: row.try_get("enabled")?,
                source_kind: row.try_get("source_kind")?,
                source_id: row.try_get("source_id")?,
                node_id: row.try_get("node_id")?,
                match_kind: row.try_get("match_kind")?,
                pattern: row.try_get("pattern")?,
                clear_pattern: row.try_get("clear_pattern")?,
                severity: row.try_get("severity")?,
                ttl_secs: row.try_get("ttl_secs")?,
                min_count: row.try_get("min_count")?,
                window_secs: row.try_get("window_secs")?,
            });
        }
        cap("event_rules", event_rules.len())?;

        let mut report_definitions = Vec::new();
        for row in
            sqlx::query("SELECT id, name, description, spec FROM report_definitions ORDER BY name")
                .fetch_all(&mut *conn)
                .await?
        {
            report_definitions.push(ReportDefinitionRow {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
                description: row.try_get("description")?,
                spec: row.try_get("spec")?,
            });
        }
        cap("report_definitions", report_definitions.len())?;

        let mut report_schedules = Vec::new();
        for row in sqlx::query(
            "SELECT id, definition_id, frequency, day_of_week, day_of_month, at_hour, at_minute, \
                    enabled \
             FROM report_schedules ORDER BY id",
        )
        .fetch_all(&mut *conn)
        .await?
        {
            report_schedules.push(ReportScheduleRow {
                id: row.try_get("id")?,
                definition_id: row.try_get("definition_id")?,
                frequency: row.try_get("frequency")?,
                day_of_week: row.try_get("day_of_week")?,
                day_of_month: row.try_get("day_of_month")?,
                at_hour: row.try_get("at_hour")?,
                at_minute: row.try_get("at_minute")?,
                enabled: row.try_get("enabled")?,
            });
        }
        cap("report_schedules", report_schedules.len())?;

        let mut analysis_schedules = Vec::new();
        for row in sqlx::query(
            "SELECT id, tool, scope_kind, scope_id, scope_label, params, frequency, day_of_week, \
                    day_of_month, at_hour, at_minute, enabled \
             FROM analysis_schedules ORDER BY id",
        )
        .fetch_all(&mut *conn)
        .await?
        {
            analysis_schedules.push(AnalysisScheduleRow {
                id: row.try_get("id")?,
                tool: row.try_get("tool")?,
                scope_kind: row.try_get("scope_kind")?,
                scope_id: row.try_get("scope_id")?,
                scope_label: row.try_get("scope_label")?,
                params: row.try_get("params")?,
                frequency: row.try_get("frequency")?,
                day_of_week: row.try_get("day_of_week")?,
                day_of_month: row.try_get("day_of_month")?,
                at_hour: row.try_get("at_hour")?,
                at_minute: row.try_get("at_minute")?,
                enabled: row.try_get("enabled")?,
            });
        }
        cap("analysis_schedules", analysis_schedules.len())?;

        let app_settings = sqlx::query(
            "SELECT default_poll_interval_secs, meraki_polling_enabled FROM app_settings \
             WHERE id = TRUE",
        )
        .fetch_optional(&mut *conn)
        .await?
        .map(|row| {
            Ok::<_, sqlx::Error>(AppSettingsRow {
                default_poll_interval_secs: row.try_get("default_poll_interval_secs")?,
                meraki_polling_enabled: row.try_get("meraki_polling_enabled")?,
            })
        })
        .transpose()?;

        Ok(ConfigBundle {
            format: BUNDLE_FORMAT.to_owned(),
            version: BUNDLE_VERSION,
            exported_at: Utc::now(),
            yagra_version: env!("CARGO_PKG_VERSION").to_owned(),
            secrets: SecretsMode::References,
            notes: notes.finish(),
            app_settings,
            profiles,
            collection_templates,
            collection_template_items,
            profile_collection_templates,
            classification_rules,
            node_groups,
            nodes,
            thresholds,
            url_checks,
            dns_checks,
            forward_destinations,
            event_sources,
            event_rules,
            report_definitions,
            report_schedules,
            analysis_schedules,
        })
    }
}

/// Refuse a table whose row count exceeds what one bundle carries.
fn cap(table: &'static str, count: usize) -> Result<(), BundleError> {
    if count > MAX_ROWS_PER_TABLE {
        return Err(BundleError::TooLarge {
            table,
            count,
            cap: MAX_ROWS_PER_TABLE,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The export refuses rather than truncating. A partial bundle that looked complete is the
    /// failure this cap exists to prevent.
    #[test]
    fn an_oversized_table_refuses_the_export_instead_of_truncating() {
        assert!(cap("nodes", MAX_ROWS_PER_TABLE).is_ok());
        let err = cap("nodes", MAX_ROWS_PER_TABLE + 1).unwrap_err();
        match err {
            BundleError::TooLarge { table, count, cap } => {
                assert_eq!(table, "nodes");
                assert_eq!(count, MAX_ROWS_PER_TABLE + 1);
                assert_eq!(cap, MAX_ROWS_PER_TABLE);
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }
}
