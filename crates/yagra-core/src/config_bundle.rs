// SPDX-License-Identifier: AGPL-3.0-only
//! Configuration bundle: export a deployment's monitoring configuration as one JSON document, and
//! import it into another (ADR-040 decision 3).
//!
//! # What this is *not*
//!
//! It is not a backup. `scripts/yagra-backup.sh` (decision 1) takes `pg_dump`, which carries the
//! whole database — inventory, alert history, the audit trail, users, and every sealed secret — and
//! is what you restore after losing a server. This is the other job: **moving a configuration you
//! built in one deployment into a different one**, which is a subset of PostgreSQL, must be
//! human-readable, and must carry no secrets at all.
//!
//! # Secrets
//!
//! [`SecretsMode`] has exactly one variant, `References`, and there is deliberately no second one.
//! A passphrase-resealing mode is a plausible future feature; adding the *variant* now, unused,
//! would create an enum arm waiting to be filled in by someone who has not read ADR-018. The rule
//! is that no code path in this repository turns a sealed secret back into transportable plaintext,
//! and an unused variant is how that rule gets relaxed by accident. The bundle therefore carries
//! **only ids** of credentials, and the importer keeps a reference only when the target already has
//! that exact id — otherwise it drops it and says so.
//!
//! # What is deliberately not carried
//!
//! Each of these is a decision, not an oversight — the reason is next to the exclusion so nobody
//! "completes" the list later without reading it:
//!
//! * **`users` / `api_tokens` / `oidc_providers`** — identity. An import is a write path; carrying
//!   accounts across would make "restore a config" the shortest route to granting yourself a role.
//! * **`credentials`** — the sealed monitoring secrets themselves (see above).
//! * **`notification_channels` / `routing_rules`** — a channel *is* its sealed config; the only
//!   thing an id-only stub could preserve is the id, and the API has no way to attach a config to
//!   an existing channel (create carries the config, and creating mints a new random id). So an
//!   imported channel could never be made to work, and a routing rule pointing at one would notify
//!   nobody, silently — the worst possible outcome for an alerting system. Re-create channels on
//!   the target, then re-create the rules.
//! * **`llm_config` / `ldap_config` / `meraki_*`** — provider credentials, same reason as
//!   `credentials`. `ldap_config` additionally sits with `oidc_providers` above: it decides who may
//!   sign in and as what, so carrying it would make importing a bundle a way to hand yourself a
//!   role from a directory the target deployment does not otherwise trust.
//! * **`pollers` / `mib_catalog`** — infrastructure inventory and a boot-time seeded catalog; both
//!   are properties of the target deployment, not of the configuration being moved.
//! * **`user_dashboards` / `shared_dashboard`** — widget layouts embed references (node ids, group
//!   ids, metric names) that the importer cannot validate, so a carried layout would render broken
//!   widgets with no error. Held back deliberately until widgets can be validated.
//! * **`maintenance_windows` / `mutes`** — bounded-in-time operational state, not configuration.
//! * **The four retention windows of `app_settings`** — retention is a policy of the *target*
//!   deployment (its disks, its compliance window), and lowering one deletes data. An import is not
//!   where an operator expects a deletion policy to change under them. `default_poll_interval_secs`
//!   and `meraki_polling_enabled` are carried; the retention columns are not.
//! * **Metrics, events, flows, alert history** — the time-series and event tiers. Out of scope by
//!   the ADR: those stores have their own migration tools and are sized in gigabytes.
//!
//! # Built-in rows
//!
//! Seeded profiles, templates, classification rules, seeded thresholds and the built-in trap rules
//! are excluded, because the target seeds its own copies at boot and their ids are derived from an
//! array position — carrying one across builds whose catalogs differ would re-key it. The filter is
//! [`crate::seed_ids::is_builtin`], which is the same table the seeder itself reads.
//!
//! # Import semantics
//!
//! Upsert only. The importer never deletes a row, and there is deliberately **no "replace" mode**:
//! it would be one boolean away, and that boolean is what makes an import unrecoverable. Everything
//! runs in one transaction, so `dry_run` is the same code path with a rollback at the end rather
//! than a second, less-tested one.

use crate::cadence::{compute_next_run, Cadence, Schedule};
use crate::seed_ids;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::collections::{BTreeMap, HashSet};
use uuid::Uuid;

/// The document's format marker. An importer that does not recognise it refuses rather than
/// guessing at an arbitrary JSON file an operator picked by mistake.
pub const BUNDLE_FORMAT: &str = "yagra.config-bundle";

/// The bundle schema version. Bumped only for a change an older importer could not read correctly;
/// added optional fields do not bump it (they arrive as `#[serde(default)]`).
pub const BUNDLE_VERSION: u32 = 1;

/// Per-table ceiling on what a bundle carries.
///
/// The bundle is a single JSON document that has to pass through a request body, so it cannot grow
/// with the fleet. Rather than truncate — which would produce a *partial* configuration that looks
/// complete — the export refuses and names the table and the count. A deployment past this size is
/// asking for disaster recovery, which is `pg_dump` (decision 1), not a migration bundle.
pub const MAX_ROWS_PER_TABLE: usize = 10_000;

/// Every table the bundle carries, in dependency order. The importer walks this order, the report
/// is keyed by it, and a test pins each name to both an export and an import statement.
pub const BUNDLE_TABLES: [&str; 16] = [
    "profiles",
    "collection_templates",
    "collection_template_items",
    "profile_collection_templates",
    "classification_rules",
    "node_groups",
    "nodes",
    "thresholds",
    "url_checks",
    "dns_checks",
    "forward_destinations",
    "event_sources",
    "event_rules",
    "report_definitions",
    "report_schedules",
    "analysis_schedules",
];

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
}

impl NoteCode {
    /// Every code.
    ///
    /// Test-only, and deliberately so: the production consumer of the full set is the generated
    /// OpenAPI enum, which utoipa derives from the variants themselves — a Rust list would be a
    /// second copy with no reader. What it is for is the one thing the derive cannot check, that
    /// each variant's token is distinct and survives a round trip.
    #[cfg(test)]
    const ALL: [NoteCode; 6] = [
        NoteCode::SkippedBuiltin,
        NoteCode::SkippedMissingReference,
        NoteCode::ReferenceDropped,
        NoteCode::SecretDroppedImportedDisabled,
        NoteCode::WebhookTokenReset,
        NoteCode::ScheduleNextRunRecomputed,
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

/// Accumulates notes keyed by `(table, code, field)` so a per-row event becomes one counted line.
#[derive(Default)]
struct Notes(BTreeMap<(String, NoteCode, Option<String>), u32>);

impl Notes {
    fn add(&mut self, table: &str, code: NoteCode, field: Option<&str>) {
        *self
            .0
            .entry((table.to_owned(), code, field.map(str::to_owned)))
            .or_insert(0) += 1;
    }

    fn finish(self) -> Vec<BundleNote> {
        self.0
            .into_iter()
            .map(|((table, code, field), count)| BundleNote {
                table,
                code,
                field,
                count,
            })
            .collect()
    }
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
    pub scope_id: String,
    pub metric: String,
    pub direction: String,
    #[serde(default)]
    pub warning: Option<f64>,
    #[serde(default)]
    pub critical: Option<f64>,
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

/// Export/import of the configuration bundle.
pub struct ConfigBundleRepo {
    pool: PgPool,
}

impl ConfigBundleRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    // ── Export ────────────────────────────────────────────────────────────────────────────

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
            "SELECT id, scope_level, scope_id, metric, direction, warning, critical, dwell_samples \
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
                metric: row.try_get("metric")?,
                direction: row.try_get("direction")?,
                warning: row.try_get("warning")?,
                critical: row.try_get("critical")?,
                dwell_samples: row.try_get("dwell_samples")?,
            });
        }
        cap("thresholds", thresholds.len())?;

        let mut url_checks = Vec::new();
        for row in sqlx::query(
            "SELECT node_id, url, method, expected_status, verify_tls, follow_redirects, \
                    timeout_ms, credential_id \
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

    // ── Import ────────────────────────────────────────────────────────────────────────────

    /// Apply a bundle. Upsert only — nothing is ever deleted. With `dry_run` the whole import runs
    /// and is then rolled back, so the report describes exactly what a real run would do.
    pub async fn import(
        &self,
        bundle: &ConfigBundle,
        dry_run: bool,
    ) -> Result<ImportReport, BundleError> {
        check_header(bundle)?;
        let now = Utc::now();
        let mut notes = Notes::default();
        let mut counts: BTreeMap<&'static str, TableResult> = BTreeMap::new();
        let mut tx = self.pool.begin().await?;

        // ── profiles ──────────────────────────────────────────────────────────────────────
        let mut seen = id_set(&mut tx, "SELECT id FROM profiles").await?;
        let c = counter(&mut counts, "profiles");
        for p in &bundle.profiles {
            if seed_ids::is_builtin(p.id) {
                notes.add("profiles", NoteCode::SkippedBuiltin, None);
                c.skipped += 1;
                continue;
            }
            // parent_id is applied in a second pass: a bundle's profile tree can list a child
            // before its parent, and there is no ordering that fixes that for an arbitrary graph.
            sqlx::query(
                "INSERT INTO profiles (id, name, category, vendor, poll_interval_secs) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, \
                     category = EXCLUDED.category, vendor = EXCLUDED.vendor, \
                     poll_interval_secs = EXCLUDED.poll_interval_secs, updated_at = now()",
            )
            .bind(p.id)
            .bind(&p.name)
            .bind(&p.category)
            .bind(&p.vendor)
            .bind(p.poll_interval_secs)
            .execute(&mut *tx)
            .await?;
            bump(c, &mut seen, p.id);
        }
        for p in &bundle.profiles {
            let Some(parent) = p.parent_id else { continue };
            if !seen.contains(&parent) || parent == p.id {
                notes.add("profiles", NoteCode::ReferenceDropped, Some("parent_id"));
                continue;
            }
            sqlx::query("UPDATE profiles SET parent_id = $2 WHERE id = $1")
                .bind(p.id)
                .bind(parent)
                .execute(&mut *tx)
                .await?;
        }
        let profile_ids = seen;

        // ── collection templates + items + links ──────────────────────────────────────────
        let mut seen = id_set(&mut tx, "SELECT id FROM collection_templates").await?;
        let c = counter(&mut counts, "collection_templates");
        for t in &bundle.collection_templates {
            if seed_ids::is_builtin(t.id) {
                notes.add("collection_templates", NoteCode::SkippedBuiltin, None);
                c.skipped += 1;
                continue;
            }
            sqlx::query(
                "INSERT INTO collection_templates (id, name, description) VALUES ($1, $2, $3) \
                 ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, \
                     description = EXCLUDED.description",
            )
            .bind(t.id)
            .bind(&t.name)
            .bind(&t.description)
            .execute(&mut *tx)
            .await?;
            bump(c, &mut seen, t.id);
        }
        let template_ids = seen;

        let mut seen = id_set(&mut tx, "SELECT id FROM collection_template_items").await?;
        let c = counter(&mut counts, "collection_template_items");
        for i in &bundle.collection_template_items {
            if !template_ids.contains(&i.template_id) {
                notes.add(
                    "collection_template_items",
                    NoteCode::SkippedMissingReference,
                    Some("template_id"),
                );
                c.skipped += 1;
                continue;
            }
            sqlx::query(
                "INSERT INTO collection_template_items \
                    (id, template_id, metric_name, oid, collection, metric_kind, enabled) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) \
                 ON CONFLICT (id) DO UPDATE SET template_id = EXCLUDED.template_id, \
                     metric_name = EXCLUDED.metric_name, oid = EXCLUDED.oid, \
                     collection = EXCLUDED.collection, metric_kind = EXCLUDED.metric_kind, \
                     enabled = EXCLUDED.enabled",
            )
            .bind(i.id)
            .bind(i.template_id)
            .bind(&i.metric_name)
            .bind(&i.oid)
            .bind(&i.collection)
            .bind(&i.metric_kind)
            .bind(i.enabled)
            .execute(&mut *tx)
            .await?;
            bump(c, &mut seen, i.id);
        }

        let c = counter(&mut counts, "profile_collection_templates");
        for l in &bundle.profile_collection_templates {
            if !profile_ids.contains(&l.profile_id) || !template_ids.contains(&l.template_id) {
                notes.add(
                    "profile_collection_templates",
                    NoteCode::SkippedMissingReference,
                    None,
                );
                c.skipped += 1;
                continue;
            }
            let res = sqlx::query(
                "INSERT INTO profile_collection_templates (profile_id, template_id) \
                 VALUES ($1, $2) ON CONFLICT DO NOTHING",
            )
            .bind(l.profile_id)
            .bind(l.template_id)
            .execute(&mut *tx)
            .await?;
            if res.rows_affected() > 0 {
                c.created += 1;
            } else {
                c.updated += 1;
            }
        }

        // ── classification rules ──────────────────────────────────────────────────────────
        let mut seen = id_set(&mut tx, "SELECT id FROM classification_rules").await?;
        let c = counter(&mut counts, "classification_rules");
        for r in &bundle.classification_rules {
            if seed_ids::is_builtin(r.id) {
                notes.add("classification_rules", NoteCode::SkippedBuiltin, None);
                c.skipped += 1;
                continue;
            }
            // profile_id is NOT NULL here, so a missing profile cannot be nulled — the rule is
            // skipped. Widening it to "any profile" is not an option a rule can express.
            if !profile_ids.contains(&r.profile_id) {
                notes.add(
                    "classification_rules",
                    NoteCode::SkippedMissingReference,
                    Some("profile_id"),
                );
                c.skipped += 1;
                continue;
            }
            sqlx::query(
                "INSERT INTO classification_rules \
                    (id, priority, sysobjectid_prefix, sysdescr_regex, profile_id, vendor, model, \
                     enabled) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
                 ON CONFLICT (id) DO UPDATE SET priority = EXCLUDED.priority, \
                     sysobjectid_prefix = EXCLUDED.sysobjectid_prefix, \
                     sysdescr_regex = EXCLUDED.sysdescr_regex, profile_id = EXCLUDED.profile_id, \
                     vendor = EXCLUDED.vendor, model = EXCLUDED.model, enabled = EXCLUDED.enabled, \
                     updated_at = now()",
            )
            .bind(r.id)
            .bind(r.priority)
            .bind(&r.sysobjectid_prefix)
            .bind(&r.sysdescr_regex)
            .bind(r.profile_id)
            .bind(&r.vendor)
            .bind(&r.model)
            .bind(r.enabled)
            .execute(&mut *tx)
            .await?;
            bump(c, &mut seen, r.id);
        }

        // ── node groups ───────────────────────────────────────────────────────────────────
        let mut seen = id_set(&mut tx, "SELECT id FROM node_groups").await?;
        let c = counter(&mut counts, "node_groups");
        for g in &bundle.node_groups {
            sqlx::query(
                "INSERT INTO node_groups (id, name, group_type, sort_order, latitude, longitude, \
                                          pool) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) \
                 ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, \
                     group_type = EXCLUDED.group_type, sort_order = EXCLUDED.sort_order, \
                     latitude = EXCLUDED.latitude, longitude = EXCLUDED.longitude, \
                     pool = EXCLUDED.pool",
            )
            .bind(g.id)
            .bind(&g.name)
            .bind(&g.group_type)
            .bind(g.sort_order)
            .bind(g.latitude)
            .bind(g.longitude)
            .bind(&g.pool)
            .execute(&mut *tx)
            .await?;
            bump(c, &mut seen, g.id);
        }
        for g in &bundle.node_groups {
            let Some(parent) = g.parent_id else { continue };
            if !seen.contains(&parent) || parent == g.id {
                notes.add("node_groups", NoteCode::ReferenceDropped, Some("parent_id"));
                continue;
            }
            sqlx::query("UPDATE node_groups SET parent_id = $2 WHERE id = $1")
                .bind(g.id)
                .bind(parent)
                .execute(&mut *tx)
                .await?;
        }
        let group_ids = seen;

        // ── nodes ─────────────────────────────────────────────────────────────────────────
        let credential_ids = id_set(&mut tx, "SELECT id FROM credentials").await?;
        let mut seen = id_set(&mut tx, "SELECT id FROM nodes").await?;
        let c = counter(&mut counts, "nodes");
        for n in &bundle.nodes {
            let profile = keep_ref(
                n.profile_id,
                &profile_ids,
                &mut notes,
                "nodes",
                "profile_id",
            );
            let group = keep_ref(n.group_id, &group_ids, &mut notes, "nodes", "group_id");
            // A credential is never carried, only referenced. It survives only when the target
            // already holds that exact id — which is the same-deployment case, not the migration
            // one; there the operator re-binds a credential they created on the target.
            let credential = keep_ref(
                n.credential_id,
                &credential_ids,
                &mut notes,
                "nodes",
                "credential_id",
            );
            sqlx::query(
                "INSERT INTO nodes (id, name, address, profile_id, group_id, credential_id, pool, \
                                    vendor, model, sort_order, tags) \
                 VALUES ($1, $2, $3::inet, $4, $5, $6, $7, $8, $9, $10, $11) \
                 ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, address = EXCLUDED.address, \
                     profile_id = EXCLUDED.profile_id, group_id = EXCLUDED.group_id, \
                     credential_id = EXCLUDED.credential_id, pool = EXCLUDED.pool, \
                     vendor = EXCLUDED.vendor, model = EXCLUDED.model, \
                     sort_order = EXCLUDED.sort_order, tags = EXCLUDED.tags, updated_at = now()",
            )
            .bind(n.id)
            .bind(&n.name)
            .bind(&n.address)
            .bind(profile)
            .bind(group)
            .bind(credential)
            .bind(&n.pool)
            .bind(&n.vendor)
            .bind(&n.model)
            .bind(n.sort_order)
            .bind(&n.tags)
            .execute(&mut *tx)
            .await?;
            bump(c, &mut seen, n.id);
        }
        for n in &bundle.nodes {
            let Some(parent) = n.parent_id else { continue };
            if !seen.contains(&parent) || parent == n.id {
                notes.add("nodes", NoteCode::ReferenceDropped, Some("parent_id"));
                continue;
            }
            sqlx::query("UPDATE nodes SET parent_id = $2 WHERE id = $1")
                .bind(n.id)
                .bind(parent)
                .execute(&mut *tx)
                .await?;
        }
        let node_ids = seen;

        // ── thresholds ────────────────────────────────────────────────────────────────────
        let mut seen = id_set(&mut tx, "SELECT id FROM thresholds").await?;
        let c = counter(&mut counts, "thresholds");
        for t in &bundle.thresholds {
            if seed_ids::is_builtin(t.id) {
                notes.add("thresholds", NoteCode::SkippedBuiltin, None);
                c.skipped += 1;
                continue;
            }
            // `scope_id` is TEXT because a group scope is a tag value, not a uuid. Only the two
            // levels that *are* uuids are validated; a tag scope has nothing to resolve against.
            if matches!(t.scope_level.as_str(), "node" | "profile") {
                let known =
                    t.scope_id
                        .parse::<Uuid>()
                        .is_ok_and(|id| match t.scope_level.as_str() {
                            "node" => node_ids.contains(&id),
                            _ => profile_ids.contains(&id),
                        });
                if !known {
                    notes.add(
                        "thresholds",
                        NoteCode::SkippedMissingReference,
                        Some("scope_id"),
                    );
                    c.skipped += 1;
                    continue;
                }
            }
            sqlx::query(
                "INSERT INTO thresholds (id, scope_level, scope_id, metric, direction, warning, \
                                         critical, dwell_samples) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
                 ON CONFLICT (id) DO UPDATE SET scope_level = EXCLUDED.scope_level, \
                     scope_id = EXCLUDED.scope_id, metric = EXCLUDED.metric, \
                     direction = EXCLUDED.direction, warning = EXCLUDED.warning, \
                     critical = EXCLUDED.critical, dwell_samples = EXCLUDED.dwell_samples",
            )
            .bind(t.id)
            .bind(&t.scope_level)
            .bind(&t.scope_id)
            .bind(&t.metric)
            .bind(&t.direction)
            .bind(t.warning)
            .bind(t.critical)
            .bind(t.dwell_samples)
            .execute(&mut *tx)
            .await?;
            bump(c, &mut seen, t.id);
        }

        // ── URL / DNS monitor configs ─────────────────────────────────────────────────────
        let mut seen = id_set(&mut tx, "SELECT node_id AS id FROM url_checks").await?;
        let c = counter(&mut counts, "url_checks");
        for u in &bundle.url_checks {
            if !node_ids.contains(&u.node_id) {
                notes.add(
                    "url_checks",
                    NoteCode::SkippedMissingReference,
                    Some("node_id"),
                );
                c.skipped += 1;
                continue;
            }
            let credential = keep_ref(
                u.credential_id,
                &credential_ids,
                &mut notes,
                "url_checks",
                "credential_id",
            );
            sqlx::query(
                "INSERT INTO url_checks (node_id, url, method, expected_status, verify_tls, \
                                         follow_redirects, timeout_ms, credential_id) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
                 ON CONFLICT (node_id) DO UPDATE SET url = EXCLUDED.url, \
                     method = EXCLUDED.method, expected_status = EXCLUDED.expected_status, \
                     verify_tls = EXCLUDED.verify_tls, \
                     follow_redirects = EXCLUDED.follow_redirects, \
                     timeout_ms = EXCLUDED.timeout_ms, credential_id = EXCLUDED.credential_id, \
                     updated_at = now()",
            )
            .bind(u.node_id)
            .bind(&u.url)
            .bind(&u.method)
            .bind(&u.expected_status)
            .bind(u.verify_tls)
            .bind(u.follow_redirects)
            .bind(u.timeout_ms)
            .bind(credential)
            .execute(&mut *tx)
            .await?;
            bump(c, &mut seen, u.node_id);
        }

        let mut seen = id_set(&mut tx, "SELECT node_id AS id FROM dns_checks").await?;
        let c = counter(&mut counts, "dns_checks");
        for d in &bundle.dns_checks {
            if !node_ids.contains(&d.node_id) {
                notes.add(
                    "dns_checks",
                    NoteCode::SkippedMissingReference,
                    Some("node_id"),
                );
                c.skipped += 1;
                continue;
            }
            sqlx::query(
                "INSERT INTO dns_checks (node_id, name, record_type, resolver_ip, resolver_port, \
                                         max_depth, timeout_ms) \
                 VALUES ($1, $2, $3, $4::inet, $5, $6, $7) \
                 ON CONFLICT (node_id) DO UPDATE SET name = EXCLUDED.name, \
                     record_type = EXCLUDED.record_type, resolver_ip = EXCLUDED.resolver_ip, \
                     resolver_port = EXCLUDED.resolver_port, max_depth = EXCLUDED.max_depth, \
                     timeout_ms = EXCLUDED.timeout_ms, updated_at = now()",
            )
            .bind(d.node_id)
            .bind(&d.name)
            .bind(&d.record_type)
            .bind(&d.resolver_ip)
            .bind(d.resolver_port)
            .bind(d.max_depth)
            .bind(d.timeout_ms)
            .execute(&mut *tx)
            .await?;
            bump(c, &mut seen, d.node_id);
        }

        // ── forwarding destinations ───────────────────────────────────────────────────────
        let mut seen = id_set(&mut tx, "SELECT id FROM forward_destinations").await?;
        let c = counter(&mut counts, "forward_destinations");
        for f in &bundle.forward_destinations {
            // A destination that needed a secret arrives disabled: enabled with no secret would
            // start sending — with a wrong or absent community — the moment the import commits.
            let enabled = f.enabled && !f.had_secret;
            if f.had_secret {
                notes.add(
                    "forward_destinations",
                    NoteCode::SecretDroppedImportedDisabled,
                    None,
                );
            }
            // The five sealed columns are never written here, so an existing destination on the
            // target keeps whatever secret it already holds — and then keeps its own enabled state
            // too. Forcing it off would take a working forwarder down to describe a secret the
            // *source* deployment had, which says nothing about this one.
            sqlx::query(
                "INSERT INTO forward_destinations (id, name, enabled, source_kind, dest_kind, \
                                                   target, pool, verbatim, filter, \
                                                   rate_limit_per_sec, ca_cert) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
                 ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, \
                     enabled = CASE WHEN forward_destinations.key_id IS NOT NULL \
                                    THEN forward_destinations.enabled ELSE EXCLUDED.enabled END, \
                     source_kind = EXCLUDED.source_kind, dest_kind = EXCLUDED.dest_kind, \
                     target = EXCLUDED.target, pool = EXCLUDED.pool, \
                     verbatim = EXCLUDED.verbatim, filter = EXCLUDED.filter, \
                     rate_limit_per_sec = EXCLUDED.rate_limit_per_sec, \
                     ca_cert = EXCLUDED.ca_cert, updated_at = now()",
            )
            .bind(f.id)
            .bind(&f.name)
            .bind(enabled)
            .bind(&f.source_kind)
            .bind(&f.dest_kind)
            .bind(&f.target)
            .bind(&f.pool)
            .bind(f.verbatim)
            .bind(&f.filter)
            .bind(f.rate_limit_per_sec)
            .bind(&f.ca_cert)
            .execute(&mut *tx)
            .await?;
            bump(c, &mut seen, f.id);
        }

        // ── event sources + rules ─────────────────────────────────────────────────────────
        let mut seen = id_set(&mut tx, "SELECT id FROM event_sources").await?;
        let c = counter(&mut counts, "event_sources");
        for s in &bundle.event_sources {
            let node = keep_ref(s.node_id, &node_ids, &mut notes, "event_sources", "node_id");
            // A webhook source cannot exist without a token (the table's CHECK), and a token is a
            // bearer credential that must not cross deployments. It is created with the digest of
            // a value generated here and immediately discarded — a hash with no preimage anyone
            // holds — and disabled, so nothing can authenticate against it until the operator
            // rotates the token and gets one back.
            let webhook = s.kind == WEBHOOK_KIND;
            let enabled = s.enabled && !webhook;
            if webhook {
                notes.add("event_sources", NoteCode::WebhookTokenReset, None);
            }
            let token_hash = webhook.then(unusable_token_hash);
            // On conflict the target's own token wins and, with it, its own enabled state: a
            // source that already authenticates senders is working, and replacing its token would
            // break every sender pointed at it. `COALESCE` also keeps the `kind <> 'webhook' OR
            // token_hash IS NOT NULL` CHECK satisfiable when an existing non-webhook source is
            // updated *into* a webhook one.
            sqlx::query(
                "INSERT INTO event_sources (id, name, kind, enabled, node_id, token_hash) \
                 VALUES ($1, $2, $3, $4, $5, $6) \
                 ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, kind = EXCLUDED.kind, \
                     node_id = EXCLUDED.node_id, updated_at = now(), \
                     token_hash = COALESCE(event_sources.token_hash, EXCLUDED.token_hash), \
                     enabled = CASE WHEN event_sources.token_hash IS NOT NULL \
                                    THEN event_sources.enabled ELSE EXCLUDED.enabled END",
            )
            .bind(s.id)
            .bind(&s.name)
            .bind(&s.kind)
            .bind(enabled)
            .bind(node)
            .bind(token_hash)
            .execute(&mut *tx)
            .await?;
            bump(c, &mut seen, s.id);
        }
        let source_ids = seen;

        let mut seen = id_set(&mut tx, "SELECT id FROM event_rules").await?;
        let c = counter(&mut counts, "event_rules");
        for r in &bundle.event_rules {
            if seed_ids::is_builtin(r.id) {
                notes.add("event_rules", NoteCode::SkippedBuiltin, None);
                c.skipped += 1;
                continue;
            }
            // Both references are *narrowing*: NULL means "any source" / "any node". Dropping a
            // dangling one would widen the rule to the whole fleet, so the rule is skipped instead.
            let source_missing = r.source_id.is_some_and(|id| !source_ids.contains(&id));
            let node_missing = r.node_id.is_some_and(|id| !node_ids.contains(&id));
            if source_missing || node_missing {
                notes.add("event_rules", NoteCode::SkippedMissingReference, None);
                c.skipped += 1;
                continue;
            }
            sqlx::query(
                "INSERT INTO event_rules (id, name, enabled, source_kind, source_id, node_id, \
                                          match_kind, pattern, clear_pattern, severity, ttl_secs, \
                                          min_count, window_secs) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
                 ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, enabled = EXCLUDED.enabled, \
                     source_kind = EXCLUDED.source_kind, source_id = EXCLUDED.source_id, \
                     node_id = EXCLUDED.node_id, match_kind = EXCLUDED.match_kind, \
                     pattern = EXCLUDED.pattern, clear_pattern = EXCLUDED.clear_pattern, \
                     severity = EXCLUDED.severity, ttl_secs = EXCLUDED.ttl_secs, \
                     min_count = EXCLUDED.min_count, window_secs = EXCLUDED.window_secs, \
                     updated_at = now()",
            )
            .bind(r.id)
            .bind(&r.name)
            .bind(r.enabled)
            .bind(&r.source_kind)
            .bind(r.source_id)
            .bind(r.node_id)
            .bind(&r.match_kind)
            .bind(&r.pattern)
            .bind(&r.clear_pattern)
            .bind(&r.severity)
            .bind(r.ttl_secs)
            .bind(r.min_count)
            .bind(r.window_secs)
            .execute(&mut *tx)
            .await?;
            bump(c, &mut seen, r.id);
        }

        // ── reports ───────────────────────────────────────────────────────────────────────
        let mut seen = id_set(&mut tx, "SELECT id FROM report_definitions").await?;
        let c = counter(&mut counts, "report_definitions");
        for d in &bundle.report_definitions {
            sqlx::query(
                "INSERT INTO report_definitions (id, name, description, spec) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, \
                     description = EXCLUDED.description, spec = EXCLUDED.spec, updated_at = now()",
            )
            .bind(d.id)
            .bind(&d.name)
            .bind(&d.description)
            .bind(&d.spec)
            .execute(&mut *tx)
            .await?;
            bump(c, &mut seen, d.id);
        }
        let definition_ids = seen;

        let mut seen = id_set(&mut tx, "SELECT id FROM report_schedules").await?;
        let c = counter(&mut counts, "report_schedules");
        for s in &bundle.report_schedules {
            if !definition_ids.contains(&s.definition_id) {
                notes.add(
                    "report_schedules",
                    NoteCode::SkippedMissingReference,
                    Some("definition_id"),
                );
                c.skipped += 1;
                continue;
            }
            // `next_run_at` is a clock reading from the source deployment; carrying it would either
            // fire everything at once (a past instant) or hold a schedule for a period.
            let next = next_run(
                &s.frequency,
                s.day_of_week,
                s.day_of_month,
                s.at_hour,
                s.at_minute,
                now,
            );
            notes.add(
                "report_schedules",
                NoteCode::ScheduleNextRunRecomputed,
                None,
            );
            sqlx::query(
                "INSERT INTO report_schedules (id, definition_id, frequency, day_of_week, \
                                               day_of_month, at_hour, at_minute, enabled, \
                                               next_run_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
                 ON CONFLICT (id) DO UPDATE SET definition_id = EXCLUDED.definition_id, \
                     frequency = EXCLUDED.frequency, day_of_week = EXCLUDED.day_of_week, \
                     day_of_month = EXCLUDED.day_of_month, at_hour = EXCLUDED.at_hour, \
                     at_minute = EXCLUDED.at_minute, enabled = EXCLUDED.enabled, \
                     next_run_at = EXCLUDED.next_run_at, updated_at = now()",
            )
            .bind(s.id)
            .bind(s.definition_id)
            .bind(&s.frequency)
            .bind(s.day_of_week)
            .bind(s.day_of_month)
            .bind(s.at_hour)
            .bind(s.at_minute)
            .bind(s.enabled)
            .bind(next)
            .execute(&mut *tx)
            .await?;
            bump(c, &mut seen, s.id);
        }

        // ── analysis schedules ────────────────────────────────────────────────────────────
        let mut seen = id_set(&mut tx, "SELECT id FROM analysis_schedules").await?;
        let c = counter(&mut counts, "analysis_schedules");
        for s in &bundle.analysis_schedules {
            // `scope_id` is polymorphic (node / group / NULL for the whole fleet), so a dangling id
            // cannot be nulled — that would silently widen the schedule to the entire fleet.
            let resolved = match (s.scope_kind.as_str(), s.scope_id) {
                ("node", Some(id)) => node_ids.contains(&id),
                ("group", Some(id)) => group_ids.contains(&id),
                ("node" | "group", None) => false,
                _ => true,
            };
            if !resolved {
                notes.add(
                    "analysis_schedules",
                    NoteCode::SkippedMissingReference,
                    Some("scope_id"),
                );
                c.skipped += 1;
                continue;
            }
            let next = next_run(
                &s.frequency,
                s.day_of_week,
                s.day_of_month,
                s.at_hour,
                s.at_minute,
                now,
            );
            notes.add(
                "analysis_schedules",
                NoteCode::ScheduleNextRunRecomputed,
                None,
            );
            sqlx::query(
                "INSERT INTO analysis_schedules (id, tool, scope_kind, scope_id, scope_label, \
                                                 params, frequency, day_of_week, day_of_month, \
                                                 at_hour, at_minute, enabled, next_run_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
                 ON CONFLICT (id) DO UPDATE SET tool = EXCLUDED.tool, \
                     scope_kind = EXCLUDED.scope_kind, scope_id = EXCLUDED.scope_id, \
                     scope_label = EXCLUDED.scope_label, params = EXCLUDED.params, \
                     frequency = EXCLUDED.frequency, day_of_week = EXCLUDED.day_of_week, \
                     day_of_month = EXCLUDED.day_of_month, at_hour = EXCLUDED.at_hour, \
                     at_minute = EXCLUDED.at_minute, enabled = EXCLUDED.enabled, \
                     next_run_at = EXCLUDED.next_run_at, updated_at = now()",
            )
            .bind(s.id)
            .bind(&s.tool)
            .bind(&s.scope_kind)
            .bind(s.scope_id)
            .bind(&s.scope_label)
            .bind(&s.params)
            .bind(&s.frequency)
            .bind(s.day_of_week)
            .bind(s.day_of_month)
            .bind(s.at_hour)
            .bind(s.at_minute)
            .bind(s.enabled)
            .bind(next)
            .execute(&mut *tx)
            .await?;
            bump(c, &mut seen, s.id);
        }

        // ── deployment settings ───────────────────────────────────────────────────────────
        if let Some(a) = &bundle.app_settings {
            sqlx::query(
                "INSERT INTO app_settings (id, default_poll_interval_secs, meraki_polling_enabled) \
                 VALUES (TRUE, $1, $2) \
                 ON CONFLICT (id) DO UPDATE SET \
                     default_poll_interval_secs = EXCLUDED.default_poll_interval_secs, \
                     meraki_polling_enabled = EXCLUDED.meraki_polling_enabled, updated_at = now()",
            )
            .bind(a.default_poll_interval_secs)
            .bind(a.meraki_polling_enabled)
            .execute(&mut *tx)
            .await?;
        }

        if dry_run {
            tx.rollback().await?;
        } else {
            tx.commit().await?;
        }

        Ok(ImportReport {
            dry_run,
            tables: BUNDLE_TABLES
                .iter()
                .filter_map(|t| counts.remove(*t))
                .collect(),
            notes: notes.finish(),
        })
    }
}

/// The `event_sources.kind` token whose rows carry a bearer token.
const WEBHOOK_KIND: &str = "webhook";

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

/// Reject a document that is not a bundle this build can read, before it touches the database.
pub fn check_header(bundle: &ConfigBundle) -> Result<(), BundleError> {
    if bundle.format != BUNDLE_FORMAT {
        return Err(BundleError::NotABundle);
    }
    if bundle.version > BUNDLE_VERSION {
        return Err(BundleError::UnsupportedVersion(bundle.version));
    }
    Ok(())
}

/// Keep a reference only if the target has its target; otherwise clear it and count a note.
fn keep_ref(
    id: Option<Uuid>,
    known: &HashSet<Uuid>,
    notes: &mut Notes,
    table: &str,
    field: &str,
) -> Option<Uuid> {
    match id {
        Some(v) if known.contains(&v) => Some(v),
        Some(_) => {
            notes.add(table, NoteCode::ReferenceDropped, Some(field));
            None
        }
        None => None,
    }
}

/// The next firing instant for a preset cadence, recomputed on the target's clock.
fn next_run(
    frequency: &str,
    day_of_week: Option<i16>,
    day_of_month: Option<i16>,
    at_hour: i16,
    at_minute: i16,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    compute_next_run(
        Schedule {
            frequency: Cadence::from_stored(frequency),
            day_of_week,
            day_of_month,
            at_hour,
            at_minute,
        },
        now,
    )
}

/// A SHA-256 digest of 32 freshly generated bytes that are never returned or stored.
///
/// Used only where a column requires a token digest and no token may cross deployments. The result
/// is a well-formed digest whose preimage nobody holds, so it authenticates nothing.
fn unusable_token_hash() -> String {
    let bytes: [u8; 32] = rand::random();
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The set of ids a table already holds, read inside the import transaction so it sees this
/// import's own inserts. The SQL is a `&'static str` literal at every call site — nothing here is
/// built from a request.
async fn id_set(
    tx: &mut Transaction<'_, Postgres>,
    sql: &'static str,
) -> Result<HashSet<Uuid>, sqlx::Error> {
    let rows = sqlx::query(sql).fetch_all(&mut **tx).await?;
    rows.into_iter()
        .map(|r| r.try_get::<Uuid, _>("id"))
        .collect()
}

fn counter<'a>(
    counts: &'a mut BTreeMap<&'static str, TableResult>,
    table: &'static str,
) -> &'a mut TableResult {
    counts.entry(table).or_insert_with(|| TableResult {
        table: table.to_owned(),
        created: 0,
        updated: 0,
        skipped: 0,
    })
}

/// Count a written row as created or updated, and remember its id for later references.
fn bump(c: &mut TableResult, seen: &mut HashSet<Uuid>, id: Uuid) {
    if seen.insert(id) {
        c.created += 1;
    } else {
        c.updated += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This module's own source, split at the import banner. Two tests read it: one proves the
    /// export names no sealed column, the other that every carried table is handled on both sides.
    /// The split has to be exact, so it is asserted rather than assumed.
    fn sections() -> (&'static str, &'static str) {
        const BANNER: &str = "    // ── Import ───";
        let src = include_str!("config_bundle.rs");
        let at = src
            .find(BANNER)
            .expect("the import section banner moved; both source-reading tests depend on it");
        (&src[..at], &src[at..])
    }

    fn empty_bundle() -> ConfigBundle {
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

    /// The one property the whole design rests on: **no sealed column is ever selected**. This
    /// reads the module's own export queries rather than a serialized value, because the type has
    /// no field to hold a secret — so a leak could only arrive as a new `SELECT`, and only a source
    /// check sees that. The needles are the real column names shared by `credentials`,
    /// `notification_channels` and `forward_destinations`.
    #[test]
    fn no_export_query_names_a_sealed_column() {
        let (export, _) = sections();
        for column in ["wrapped_dek", "dek_nonce", "ciphertext", "ct_nonce"] {
            assert!(
                !export.contains(column),
                "the export mentions the sealed column {column}; a bundle must carry no secret"
            );
        }
        // `key_id` appears once, as a NULL test that reveals only whether a secret exists.
        assert_eq!(export.matches("key_id").count(), 1);
        assert!(export.contains("(key_id IS NOT NULL) AS had_secret"));
    }

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

    /// Every carried table must be both read by the export and written by the import. A table added
    /// to the bundle struct but forgotten in the importer would export fine and silently vanish on
    /// the way in.
    #[test]
    fn every_bundle_table_is_both_exported_and_imported() {
        let (export, import) = sections();
        for table in BUNDLE_TABLES {
            // Needles built at runtime: this test reads its own file, so a literal would match
            // itself and pass forever.
            let read = format!("FROM {table} ");
            let written = format!("INTO {table} ");
            assert!(
                export.contains(&read),
                "{table} is never selected by the export"
            );
            assert!(
                import.contains(&written),
                "{table} is never written by the import"
            );
        }
    }

    /// The header check is what stops an arbitrary JSON file from reaching the importer's SQL.
    #[test]
    fn a_foreign_document_is_refused_before_any_write() {
        let mut b = empty_bundle();
        b.format = "some-other-tool".into();
        assert!(matches!(check_header(&b), Err(BundleError::NotABundle)));

        let mut b = empty_bundle();
        b.version = BUNDLE_VERSION + 1;
        assert!(matches!(
            check_header(&b),
            Err(BundleError::UnsupportedVersion(v)) if v == BUNDLE_VERSION + 1
        ));

        assert!(check_header(&empty_bundle()).is_ok());
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

    /// A dangling reference is cleared and counted; a resolvable one is kept untouched.
    #[test]
    fn a_dangling_reference_is_dropped_and_reported() {
        let known: HashSet<Uuid> = [Uuid::from_u128(7)].into_iter().collect();
        let mut notes = Notes::default();
        assert_eq!(
            keep_ref(
                Some(Uuid::from_u128(7)),
                &known,
                &mut notes,
                "nodes",
                "profile_id"
            ),
            Some(Uuid::from_u128(7))
        );
        assert_eq!(
            keep_ref(
                Some(Uuid::from_u128(8)),
                &known,
                &mut notes,
                "nodes",
                "profile_id"
            ),
            None
        );
        assert_eq!(
            keep_ref(None, &known, &mut notes, "nodes", "profile_id"),
            None
        );
        let out = notes.finish();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].code, NoteCode::ReferenceDropped);
        assert_eq!(out[0].field.as_deref(), Some("profile_id"));
        assert_eq!(out[0].count, 1);
    }

    /// Notes collapse per (table, code, field) rather than emitting one line per row — a bundle
    /// with 500 dangling credentials must read as one line saying 500.
    #[test]
    fn notes_are_counted_not_repeated() {
        let mut notes = Notes::default();
        for _ in 0..500 {
            notes.add("nodes", NoteCode::ReferenceDropped, Some("credential_id"));
        }
        notes.add("nodes", NoteCode::ReferenceDropped, Some("group_id"));
        let out = notes.finish();
        assert_eq!(out.len(), 2);
        assert_eq!(out.iter().map(|n| n.count).sum::<u32>(), 501);
    }

    /// The unusable webhook digest must be digest-shaped and never repeat, so it cannot be guessed
    /// from another import.
    #[test]
    fn the_placeholder_webhook_digest_is_shaped_like_one_and_never_repeats() {
        let a = unusable_token_hash();
        let b = unusable_token_hash();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    /// A recomputed schedule always fires in the future — never at an instant already past, which
    /// is what carrying the source deployment's `next_run_at` would have produced.
    #[test]
    fn a_recomputed_schedule_fires_in_the_future() {
        let now = DateTime::parse_from_rfc3339("2026-03-05T12:34:00Z")
            .unwrap()
            .with_timezone(&Utc);
        for (freq, dow, dom) in [
            ("daily", None, None),
            ("weekly", Some(3), None),
            ("monthly", None, Some(28)),
            // An unknown cadence from a newer core still yields a usable instant.
            ("hourly-ish", None, None),
        ] {
            let next = next_run(freq, dow, dom, 2, 15, now);
            assert!(next > now, "{freq} produced a past instant: {next}");
        }
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

    /// The bundle names itself and its own version, so a file on disk is identifiable without the
    /// tool that wrote it.
    #[test]
    fn the_format_marker_and_version_are_stable() {
        assert_eq!(BUNDLE_FORMAT, "yagra.config-bundle");
        assert_eq!(BUNDLE_VERSION, 1);
        assert_eq!(BUNDLE_TABLES.len(), 16);
        let mut sorted = BUNDLE_TABLES;
        sorted.sort_unstable();
        let mut deduped = sorted.to_vec();
        deduped.dedup();
        assert_eq!(deduped.len(), BUNDLE_TABLES.len(), "duplicate table name");
    }
}
