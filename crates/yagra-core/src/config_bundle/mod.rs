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
//! * **`user_preferences`** — per-account WebUI chrome (ADR-058), an opaque blob nothing on this
//!   side parses. It belongs to a *person*, not to a deployment's configuration, and carrying it
//!   would move one operator's screen settings onto another deployment's accounts.
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

//!
//! # Where the pieces are (ADR-101)
//!
//! Split by **which direction a configuration moves**, which is the line this module already drew
//! for itself in a comment banner — and two checks depended on that banner being spelled exactly:
//!
//! * [`export`] — out of a deployment. Reads, and `guards.rs` refuses a write here.
//! * [`import`] — into one: the transaction, `dry_run`, the report, and the helpers the two
//!   phases share. It names no table itself; the SQL is a literal at each call site.
//! * [`import_inventory`] — **what is monitored** (profiles, templates, classification rules,
//!   groups, nodes). It produces the four id sets that outlive their own block.
//! * [`import_attached`] — **how it is monitored** (thresholds, check configs, forwarding,
//!   events, reports, schedules, deployment settings). It consumes those four and produces none.
//! * [`types`] — the document both directions speak.
//!
//! The inventory/attached line is not a matter of taste: exactly four values cross it
//! (`profiles` / `groups` / `nodes` / `credentials`), and making them an argument is what stops
//! the second half running before the first. Before the split, "profiles before nodes before
//! thresholds" was held up by nothing but the order of statements in one 706-line function.
//!
//! Kept here rather than in a sibling: a child module sees its parent's private items, so the
//! vocabulary below costs no `pub(super)`. And kept **flat** — `srcread::files` reads one directory
//! level, so a nested `import/` would leave every source check running over the files beside it and
//! reporting success.

use sqlx::PgPool;
use std::collections::BTreeMap;

mod export;
mod import;
mod import_attached;
mod import_inventory;
mod types;

#[cfg(test)]
mod guards;
#[cfg(test)]
mod testkit;

pub use types::*;

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

/// Export/import of the configuration bundle.
pub struct ConfigBundleRepo {
    pool: PgPool,
}

impl ConfigBundleRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// The `event_sources.kind` token whose rows carry a bearer token.
const WEBHOOK_KIND: &str = "webhook";

/// Accumulates notes keyed by `(table, code, field)` so a per-row event becomes one counted line.
///
/// Here rather than in [`types`] because both directions write to one: a child module sees its
/// parent, so this is the placement that costs no `pub(super)` at all — the rule `repo/mod.rs`
/// and `events/mod.rs` are cut on. The vocabulary it accumulates ([`NoteCode`], [`BundleNote`])
/// stays with the rest of the document, since that is what crosses the wire.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_bundle::testkit::*;

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
