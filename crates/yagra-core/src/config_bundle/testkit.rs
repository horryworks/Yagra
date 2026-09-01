// SPDX-License-Identifier: AGPL-3.0-only
//! The bundles every test that needs one starts from — an empty one, and a full one.
//!
//! Here rather than in one of the test modules because two of them need it (`super::types` and
//! `super` itself), and a fixture copied into both is the shape `extensibility.md` §3 is about.
//! Declared `#[cfg(test)] mod testkit;` in [`super`], which is also how `srcread` derives that this
//! file is excluded from the text the guards read.

use chrono::Utc;
use uuid::Uuid;

use super::*;

pub(super) fn empty_bundle() -> ConfigBundle {
    ConfigBundle {
        format: BUNDLE_FORMAT.to_owned(),
        version: BUNDLE_VERSION,
        exported_at: Utc::now(),
        yagra_version: "test".to_owned(),
        secrets: SecretsMode::References,
        notes: Vec::new(),
        app_settings: None,
        profiles: Vec::new(),
        collection_templates: Vec::new(),
        collection_template_items: Vec::new(),
        profile_collection_templates: Vec::new(),
        classification_rules: Vec::new(),
        node_groups: Vec::new(),
        nodes: Vec::new(),
        thresholds: Vec::new(),
        url_checks: Vec::new(),
        dns_checks: Vec::new(),
        forward_destinations: Vec::new(),
        event_sources: Vec::new(),
        event_rules: Vec::new(),
        report_definitions: Vec::new(),
        report_schedules: Vec::new(),
        analysis_schedules: Vec::new(),
    }
}

/// A bundle with **at least one row in every collection the format carries**, cross-referenced so
/// the whole document imports without a single dropped reference.
///
/// This is the fixture ADR-116 exists for. Every statement in [`super::import_inventory`] and
/// [`super::import_attached`] sits inside a `for … in &bundle.<collection>`, so a statement only
/// runs when its collection has something in it. `api/config_bundle.rs`'s round trip creates one
/// group and exports that, which left **eight of ten statements in each file unexecuted** —
/// measured by `scripts/sql-coverage.sh`, while `pgtest.rs` described the same two files as
/// "exercised end to end".
///
/// 🚨 **Two each of profiles, groups and nodes, on purpose.** Each of those three has a *second*
/// statement that only runs when a row names another row as its parent: `parent_id` is applied in
/// a separate pass, because a bundle's order does not guarantee a parent arrives before its child.
/// One row of each would leave those three `UPDATE`s unexecuted and still look complete.
///
/// Ids are v4 rather than derived: [`crate::seed_ids::is_builtin`] skips a row whose id lands in a
/// reserved range, and a skipped row is a statement that did not run.
pub(super) fn full_bundle() -> ConfigBundle {
    let profile_parent = Uuid::new_v4();
    let profile_child = Uuid::new_v4();
    let template = Uuid::new_v4();
    let group_parent = Uuid::new_v4();
    let group_child = Uuid::new_v4();
    let node_a = Uuid::new_v4();
    let node_b = Uuid::new_v4();
    let source = Uuid::new_v4();
    let definition = Uuid::new_v4();

    ConfigBundle {
        app_settings: Some(AppSettingsRow {
            default_poll_interval_secs: 120,
            meraki_polling_enabled: true,
        }),
        profiles: vec![
            ProfileRow {
                id: profile_parent,
                name: "edge".to_owned(),
                parent_id: None,
                category: "router".to_owned(),
                vendor: Some("acme".to_owned()),
                poll_interval_secs: Some(60),
            },
            ProfileRow {
                id: profile_child,
                name: "edge-branch".to_owned(),
                parent_id: Some(profile_parent),
                category: "router".to_owned(),
                vendor: None,
                poll_interval_secs: None,
            },
        ],
        collection_templates: vec![CollectionTemplateRow {
            id: template,
            name: "acme cpu".to_owned(),
            description: Some("one scalar".to_owned()),
        }],
        collection_template_items: vec![CollectionTemplateItemRow {
            id: Uuid::new_v4(),
            template_id: template,
            metric_name: "cpu_pct".to_owned(),
            oid: "1.3.6.1.4.1.9.2.1.58.0".to_owned(),
            collection: "scalar".to_owned(),
            metric_kind: "gauge".to_owned(),
            enabled: true,
        }],
        profile_collection_templates: vec![ProfileTemplateLink {
            profile_id: profile_parent,
            template_id: template,
        }],
        classification_rules: vec![ClassificationRuleRow {
            id: Uuid::new_v4(),
            priority: 10,
            sysobjectid_prefix: Some("1.3.6.1.4.1.9.1.".to_owned()),
            sysdescr_regex: None,
            profile_id: profile_parent,
            vendor: Some("acme".to_owned()),
            model: None,
            enabled: true,
        }],
        node_groups: vec![
            NodeGroupRow {
                id: group_parent,
                name: "tokyo".to_owned(),
                group_type: "site".to_owned(),
                parent_id: None,
                sort_order: 1.0,
                latitude: Some(35.68),
                longitude: Some(139.76),
                pool: None,
            },
            NodeGroupRow {
                id: group_child,
                name: "tokyo-dc1".to_owned(),
                group_type: "generic".to_owned(),
                parent_id: Some(group_parent),
                sort_order: 2.0,
                latitude: None,
                longitude: None,
                pool: Some("east".to_owned()),
            },
        ],
        nodes: vec![
            NodeRow {
                id: node_a,
                name: "rtr-1".to_owned(),
                parent_id: None,
                address: "198.51.100.11".to_owned(),
                profile_id: Some(profile_parent),
                group_id: Some(group_parent),
                credential_id: None,
                pool: Some("east".to_owned()),
                vendor: Some("acme".to_owned()),
                model: Some("x100".to_owned()),
                sort_order: 1.0,
                tags: serde_json::json!(["core"]),
            },
            NodeRow {
                id: node_b,
                name: "rtr-1-mgmt".to_owned(),
                parent_id: Some(node_a),
                address: "198.51.100.12".to_owned(),
                profile_id: Some(profile_child),
                group_id: Some(group_child),
                credential_id: None,
                pool: None,
                vendor: None,
                model: None,
                sort_order: 2.0,
                tags: serde_json::json!([]),
            },
        ],
        thresholds: vec![ThresholdRow {
            id: Uuid::new_v4(),
            scope_level: "node".to_owned(),
            scope_id: node_a.to_string(),
            scope_ids: vec![node_a.to_string()],
            metric: "cpu_pct".to_owned(),
            direction: "above".to_owned(),
            warning: Some(80.0),
            critical: Some(90.0),
            warning_below: None,
            critical_below: None,
            warning_above: Some(80.0),
            critical_above: Some(90.0),
            dwell_samples: 3,
        }],
        url_checks: vec![UrlCheckRow {
            node_id: node_a,
            url: "https://example.invalid/health".to_owned(),
            method: "GET".to_owned(),
            expected_status: serde_json::json!([200]),
            verify_tls: true,
            follow_redirects: false,
            timeout_ms: 5_000,
            credential_id: None,
            body_match: None,
            json_extract: None,
            body_max_bytes: 65_536,
        }],
        dns_checks: vec![DnsCheckRow {
            node_id: node_b,
            name: "example.invalid".to_owned(),
            record_type: "A".to_owned(),
            resolver_ip: Some("198.51.100.53".to_owned()),
            resolver_port: 53,
            max_depth: 8,
            timeout_ms: 3_000,
        }],
        forward_destinations: vec![ForwardDestinationRow {
            id: Uuid::new_v4(),
            name: "siem".to_owned(),
            enabled: true,
            source_kind: "syslog".to_owned(),
            dest_kind: "syslog_udp".to_owned(),
            target: "198.51.100.9:514".to_owned(),
            pool: None,
            verbatim: true,
            filter: serde_json::json!({}),
            rate_limit_per_sec: Some(100),
            ca_cert: None,
            had_secret: false,
        }],
        event_sources: vec![EventSourceRow {
            id: source,
            name: "branch syslog".to_owned(),
            kind: "syslog".to_owned(),
            enabled: true,
            node_id: None,
        }],
        event_rules: vec![EventRuleRow {
            id: Uuid::new_v4(),
            name: "link down".to_owned(),
            enabled: true,
            source_kind: Some("syslog".to_owned()),
            source_id: Some(source),
            node_id: None,
            match_kind: "substring".to_owned(),
            pattern: "LINK-3-UPDOWN".to_owned(),
            clear_pattern: Some("LINK-3-UP".to_owned()),
            severity: "warning".to_owned(),
            ttl_secs: 3_600,
            min_count: 1,
            window_secs: 60,
        }],
        report_definitions: vec![ReportDefinitionRow {
            id: definition,
            name: "weekly health".to_owned(),
            description: Some("one section".to_owned()),
            spec: serde_json::json!({ "sections": [] }),
        }],
        report_schedules: vec![ReportScheduleRow {
            id: Uuid::new_v4(),
            definition_id: definition,
            frequency: "weekly".to_owned(),
            day_of_week: Some(1),
            day_of_month: None,
            at_hour: 6,
            at_minute: 30,
            enabled: true,
        }],
        analysis_schedules: vec![AnalysisScheduleRow {
            id: Uuid::new_v4(),
            tool: "anomaly".to_owned(),
            scope_kind: "all".to_owned(),
            scope_id: None,
            scope_label: "whole fleet".to_owned(),
            params: serde_json::json!({}),
            frequency: "daily".to_owned(),
            day_of_week: None,
            day_of_month: None,
            at_hour: 3,
            at_minute: 0,
            enabled: true,
        }],
        ..empty_bundle()
    }
}
