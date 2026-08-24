// SPDX-License-Identifier: AGPL-3.0-only
//! The document both directions speak: the bundle, its rows, its report, and its failures.
//!
//! Nothing here reads or writes a database — that is the whole reason it is a file of its own.
//! [`super::export`] fills these in from PostgreSQL and [`super::import`] applies them; both sides
//! are checked against this vocabulary by name, never by position, so a field added below is a
//! field neither side can silently ignore.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How secrets appear in a bundle. See the module docs for why there is only one variant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SecretsMode {
    /// Secrets are not carried. Sealed values stay in their deployment; only ids cross.
    #[default]
    References,
}

/// Something the export or the import did that the operator would not otherwise see.
///
/// A silent drop is the failure mode that matters here: a bundle that quietly left out half the
/// rules imports without error and the operator finds out during an incident.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NoteCode {
    /// A built-in seed row was left out; the target seeds its own.
    SkippedBuiltin,
    /// A row was not imported because something it requires is absent on the target.
    SkippedMissingReference,
    /// A row was imported, but a reference it carried was cleared because the target lacks it.
    ReferenceDropped,
    /// A row was imported **disabled** because its secret cannot cross deployments. Re-enter the
    /// secret on the target, then enable it.
    SecretDroppedImportedDisabled,
    /// A webhook ingest source was imported with an unusable token. Rotate it to get a working one.
    WebhookTokenReset,
    /// A schedule's next firing instant was recomputed from its cadence rather than carried.
    ScheduleNextRunRecomputed,
    /// A row was not imported because one of its values is outside the vocabulary that field
    /// accepts. The bundle was written by hand or by a different product.
    SkippedInvalidValue,
}

impl NoteCode {
    /// Every code.
    ///
    /// Test-only, and deliberately so: the production consumer of the full set is the generated
    /// OpenAPI enum, which utoipa derives from the variants themselves — a Rust list would be a
    /// second copy with no reader. What it is for is the one thing the derive cannot check, that
    /// each variant's token is distinct and survives a round trip.
    #[cfg(test)]
    const ALL: [NoteCode; 7] = [
        NoteCode::SkippedBuiltin,
        NoteCode::SkippedMissingReference,
        NoteCode::ReferenceDropped,
        NoteCode::SecretDroppedImportedDisabled,
        NoteCode::WebhookTokenReset,
        NoteCode::ScheduleNextRunRecomputed,
        NoteCode::SkippedInvalidValue,
    ];
}

/// One note, with the table it concerns and how many rows it covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BundleNote {
    /// The table the note is about.
    pub table: String,
    /// What happened.
    pub code: NoteCode,
    /// The column involved, when the note is about one (e.g. `credential_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// How many rows this note covers.
    pub count: u32,
}

// ── Row shapes ────────────────────────────────────────────────────────────────────────────
//
// One struct per carried table, named for it. Every field is a column; nothing is derived, so an
// operator reading the file sees the configuration rather than a projection of it. Timestamps
// (`created_at`/`updated_at`) are deliberately absent: they describe when the *source* deployment
// wrote the row, and carrying them would make an imported row claim a history it does not have.

/// A device profile.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProfileRow {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub parent_id: Option<Uuid>,
    pub category: String,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub poll_interval_secs: Option<i32>,
}

/// A reusable collection template.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CollectionTemplateRow {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// One metric inside a collection template.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CollectionTemplateItemRow {
    pub id: Uuid,
    pub template_id: Uuid,
    pub metric_name: String,
    pub oid: String,
    pub collection: String,
    pub metric_kind: String,
    pub enabled: bool,
}

/// A profile↔template attachment.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProfileTemplateLink {
    pub profile_id: Uuid,
    pub template_id: Uuid,
}

/// A discovery classification rule.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ClassificationRuleRow {
    pub id: Uuid,
    pub priority: i32,
    #[serde(default)]
    pub sysobjectid_prefix: Option<String>,
    #[serde(default)]
    pub sysdescr_regex: Option<String>,
    pub profile_id: Uuid,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub enabled: bool,
}

/// A folder in the inventory tree.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct NodeGroupRow {
    pub id: Uuid,
    pub name: String,
    pub group_type: String,
    #[serde(default)]
    pub parent_id: Option<Uuid>,
    pub sort_order: f64,
    #[serde(default)]
    pub latitude: Option<f64>,
    #[serde(default)]
    pub longitude: Option<f64>,
    #[serde(default)]
    pub pool: Option<String>,
}

/// A monitored node. `credential_id` is a reference only — see the module docs.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct NodeRow {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub parent_id: Option<Uuid>,
    /// IPv4 or IPv6, as text.
    pub address: String,
    #[serde(default)]
    pub profile_id: Option<Uuid>,
    #[serde(default)]
    pub group_id: Option<Uuid>,
    #[serde(default)]
    pub credential_id: Option<Uuid>,
    #[serde(default)]
    pub pool: Option<String>,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub sort_order: f64,
    pub tags: serde_json::Value,
}

/// A threshold rule.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ThresholdRow {
    pub id: Uuid,
    pub scope_level: String,
    /// The rule’s first target. Kept beside `scope_ids` so that a bundle written by a newer
    /// deployment still imports into an older one.
    pub scope_id: String,
    /// Every target the rule applies to. Absent in a bundle written before rules could name more
    /// than one, in which case `scope_id` is the whole answer.
    //
    // `//` rather than `///` for the reasoning: this type derives `ToSchema` and its doc lines are
    // published verbatim. ADR-078 is why both fields exist; the reader falls back when empty.
    #[serde(default)]
    pub scope_ids: Vec<String>,
    pub metric: String,
    pub direction: String,
    #[serde(default)]
    pub warning: Option<f64>,
    #[serde(default)]
    pub critical: Option<f64>,
    /// The four bounds a rule can name (ADR-081). Absent in a bundle written before ranges existed,
    /// in which case `direction` + `warning` + `critical` is the whole answer.
    //
    // `//` for the reasoning, as above: this type derives `ToSchema`. The importer folds these
    // through `ThresholdBounds::from_legacy` when they are all absent, so an old bundle imports
    // unchanged and a new one imports both sides.
    #[serde(default)]
    pub warning_below: Option<f64>,
    #[serde(default)]
    pub critical_below: Option<f64>,
    #[serde(default)]
    pub warning_above: Option<f64>,
    #[serde(default)]
    pub critical_above: Option<f64>,
    pub dwell_samples: i32,
}

/// A URL / HTTP endpoint monitor's configuration (1:1 with its node).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct UrlCheckRow {
    pub node_id: Uuid,
    pub url: String,
    pub method: String,
    pub expected_status: serde_json::Value,
    pub verify_tls: bool,
    pub follow_redirects: bool,
    pub timeout_ms: i32,
    #[serde(default)]
    pub credential_id: Option<Uuid>,
    // Defaulted so a bundle exported before these fields existed still imports.
    /// The monitor's response-body keyword rule, if it has one.
    #[serde(default)]
    pub body_match: Option<serde_json::Value>,
    /// The monitor's JSON extraction rules, if it has any.
    #[serde(default)]
    pub json_extract: Option<serde_json::Value>,
    /// How many bytes of the response body the monitor reads.
    #[serde(default = "default_bundle_body_max_bytes")]
    pub body_max_bytes: i32,
}

/// The column default, restated for a bundle written before the column existed. Not the Rust
/// constant cast inline at each use: an older bundle importing as `0` would fail the CHECK and take
/// the whole import down with it.
const fn default_bundle_body_max_bytes() -> i32 {
    65_536
}

/// A DNS monitor's configuration (1:1 with its node).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DnsCheckRow {
    pub node_id: Uuid,
    pub name: String,
    pub record_type: String,
    #[serde(default)]
    pub resolver_ip: Option<String>,
    pub resolver_port: i32,
    pub max_depth: i16,
    pub timeout_ms: i32,
}

/// A forwarding destination. Its optional sealed secret is not carried; a destination that had one
/// arrives disabled.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ForwardDestinationRow {
    pub id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub source_kind: String,
    pub dest_kind: String,
    pub target: String,
    #[serde(default)]
    pub pool: Option<String>,
    pub verbatim: bool,
    pub filter: serde_json::Value,
    #[serde(default)]
    pub rate_limit_per_sec: Option<i32>,
    #[serde(default)]
    pub ca_cert: Option<String>,
    /// Whether the source deployment had a sealed secret on this destination. Carries no secret —
    /// it is what tells the importer to arrive disabled and what tells the operator to re-enter it.
    #[serde(default)]
    pub had_secret: bool,
}

/// A passive-event ingest source.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EventSourceRow {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub enabled: bool,
    #[serde(default)]
    pub node_id: Option<Uuid>,
}

/// A passive-event match rule.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EventRuleRow {
    pub id: Uuid,
    pub name: String,
    pub enabled: bool,
    #[serde(default)]
    pub source_kind: Option<String>,
    #[serde(default)]
    pub source_id: Option<Uuid>,
    #[serde(default)]
    pub node_id: Option<Uuid>,
    pub match_kind: String,
    pub pattern: String,
    #[serde(default)]
    pub clear_pattern: Option<String>,
    pub severity: String,
    pub ttl_secs: i32,
    pub min_count: i32,
    pub window_secs: i32,
}

/// A saved report template.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ReportDefinitionRow {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub spec: serde_json::Value,
}

/// A recurring report run.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ReportScheduleRow {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub frequency: String,
    #[serde(default)]
    pub day_of_week: Option<i16>,
    #[serde(default)]
    pub day_of_month: Option<i16>,
    pub at_hour: i16,
    pub at_minute: i16,
    pub enabled: bool,
}

/// A recurring Troubleshoot analysis.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AnalysisScheduleRow {
    pub id: Uuid,
    pub tool: String,
    pub scope_kind: String,
    #[serde(default)]
    pub scope_id: Option<Uuid>,
    pub scope_label: String,
    pub params: serde_json::Value,
    pub frequency: String,
    #[serde(default)]
    pub day_of_week: Option<i16>,
    #[serde(default)]
    pub day_of_month: Option<i16>,
    pub at_hour: i16,
    pub at_minute: i16,
    pub enabled: bool,
}

/// The deployment-wide settings the bundle carries. The retention windows are deliberately absent
/// (module docs).
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AppSettingsRow {
    pub default_poll_interval_secs: i32,
    pub meraki_polling_enabled: bool,
}

/// A whole configuration bundle.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ConfigBundle {
    /// Always `yagra.config-bundle`. An importer refuses anything else rather than guessing.
    pub format: String,
    /// Bundle schema version.
    pub version: u32,
    /// When the export ran.
    pub exported_at: DateTime<Utc>,
    /// The Yagra version that produced it.
    pub yagra_version: String,
    /// How secrets are represented. Always `references` — a bundle never carries one.
    #[serde(default)]
    pub secrets: SecretsMode,
    /// What the export left out or changed. Informational; ignored on import.
    #[serde(default)]
    pub notes: Vec<BundleNote>,
    #[serde(default)]
    pub app_settings: Option<AppSettingsRow>,
    #[serde(default)]
    pub profiles: Vec<ProfileRow>,
    #[serde(default)]
    pub collection_templates: Vec<CollectionTemplateRow>,
    #[serde(default)]
    pub collection_template_items: Vec<CollectionTemplateItemRow>,
    #[serde(default)]
    pub profile_collection_templates: Vec<ProfileTemplateLink>,
    #[serde(default)]
    pub classification_rules: Vec<ClassificationRuleRow>,
    #[serde(default)]
    pub node_groups: Vec<NodeGroupRow>,
    #[serde(default)]
    pub nodes: Vec<NodeRow>,
    #[serde(default)]
    pub thresholds: Vec<ThresholdRow>,
    #[serde(default)]
    pub url_checks: Vec<UrlCheckRow>,
    #[serde(default)]
    pub dns_checks: Vec<DnsCheckRow>,
    #[serde(default)]
    pub forward_destinations: Vec<ForwardDestinationRow>,
    #[serde(default)]
    pub event_sources: Vec<EventSourceRow>,
    #[serde(default)]
    pub event_rules: Vec<EventRuleRow>,
    #[serde(default)]
    pub report_definitions: Vec<ReportDefinitionRow>,
    #[serde(default)]
    pub report_schedules: Vec<ReportScheduleRow>,
    #[serde(default)]
    pub analysis_schedules: Vec<AnalysisScheduleRow>,
}

/// What the import did to one table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct TableResult {
    pub table: String,
    pub created: u32,
    pub updated: u32,
    pub skipped: u32,
}

/// The outcome of an import.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ImportReport {
    /// True when nothing was committed: the whole import ran and was rolled back.
    pub dry_run: bool,
    /// Per-table counts, in dependency order.
    pub tables: Vec<TableResult>,
    /// What was skipped or changed, and why.
    pub notes: Vec<BundleNote>,
}

/// Why a bundle could not be produced or accepted. Everything else is an internal error.
#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    /// The deployment is larger than a single bundle document carries.
    #[error("{table} has {count} rows, more than the {cap} a configuration bundle carries")]
    TooLarge {
        table: &'static str,
        count: usize,
        cap: usize,
    },
    /// The uploaded document is not a Yagra configuration bundle.
    #[error("not a Yagra configuration bundle")]
    NotABundle,
    /// The document is a bundle, but from a schema version this build cannot read.
    #[error("bundle schema version {0} is newer than this build understands")]
    UnsupportedVersion(u32),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    // The parent as well: the header check and the two constants it reads live there.
    use crate::config_bundle::testkit::*;
    use crate::config_bundle::*;

    /// A bundle that has been through JSON must not contain a value that looks like sealed bytes or
    /// a token digest, under any key. Complements the source check above: that one catches a new
    /// `SELECT`, this one catches a secret arriving through a field someone renamed.
    #[test]
    fn a_serialized_bundle_carries_no_secret_bearing_key() {
        let mut b = empty_bundle();
        b.forward_destinations.push(ForwardDestinationRow {
            id: Uuid::from_u128(1),
            name: "siem".into(),
            enabled: true,
            source_kind: "syslog".into(),
            dest_kind: "syslog_udp".into(),
            target: "10.0.0.9:514".into(),
            pool: None,
            verbatim: true,
            filter: serde_json::json!({}),
            rate_limit_per_sec: None,
            ca_cert: None,
            had_secret: true,
        });
        b.event_sources.push(EventSourceRow {
            id: Uuid::from_u128(2),
            name: "meraki".into(),
            kind: WEBHOOK_KIND.into(),
            enabled: true,
            node_id: None,
        });
        let json = serde_json::to_string(&b).unwrap();
        for forbidden in [
            "token_hash",
            "wrapped_dek",
            "dek_nonce",
            "ciphertext",
            "ct_nonce",
            "password_hash",
            "secret",
        ] {
            assert!(
                !json.contains(&format!("\"{forbidden}\"")),
                "a serialized bundle exposes a {forbidden} field"
            );
        }
        // `had_secret` is the deliberate exception: a boolean, never a value.
        assert!(json.contains("\"had_secret\":true"));
    }

    /// An older bundle must still load: every collection is `#[serde(default)]`, so a document
    /// written before a table joined the bundle deserializes with that table empty rather than
    /// failing the whole import.
    #[test]
    fn a_bundle_missing_optional_sections_still_loads() {
        let minimal = format!(
            r#"{{"format":"{BUNDLE_FORMAT}","version":1,"exported_at":"2026-01-01T00:00:00Z",
                "yagra_version":"0.1.0"}}"#
        );
        let b: ConfigBundle = serde_json::from_str(&minimal).unwrap();
        assert_eq!(b.secrets, SecretsMode::References);
        assert!(b.nodes.is_empty());
        assert!(b.app_settings.is_none());
        assert!(check_header(&b).is_ok());
    }

    /// Each note code must serialize to its own snake_case token: the WebUI keys one localized
    /// string per code, and two codes sharing a token would make one of them unreachable — a note
    /// the operator never sees is the same as no note.
    #[test]
    fn every_note_code_has_its_own_stable_token() {
        let tokens: Vec<String> = NoteCode::ALL
            .iter()
            .map(|c| serde_json::to_string(c).unwrap())
            .collect();
        let mut unique = tokens.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), NoteCode::ALL.len(), "duplicate note token");
        assert!(tokens.contains(&"\"skipped_builtin\"".to_owned()));
        assert!(tokens.contains(&"\"secret_dropped_imported_disabled\"".to_owned()));
        // Round-trips, so a report written by one build is readable by the next.
        for c in NoteCode::ALL {
            let json = serde_json::to_string(&c).unwrap();
            assert_eq!(serde_json::from_str::<NoteCode>(&json).unwrap(), c);
        }
    }
}
