//! Device classification: resolve a discovered device's SNMP signature to a suggested
//! device profile.
//!
//! Two pieces:
//! - [`ClassificationRepo`] — PostgreSQL CRUD over the operator-editable `classification_rules`
//!   table (mirrors [`crate::collection::CollectionRepo`]).
//! - [`Classifier`] — an in-memory, compiled snapshot of the rules that discovery consults per
//!   device. Loaded at startup and reloaded after a rule edit (and on a periodic refresh), so
//!   classification stays hot-path cheap (no DB round-trip per candidate) and regexes compile
//!   once, not per device.
//!
//! Matching is single-pass over rules pre-sorted by `(prefix-bearing first, ascending priority,
//! longer prefix first)`. A rule matches only when *all* its present matchers match — a
//! `sysObjectID` prefix AND/or a `sysDescr` regex — so one rule can mean "this vendor AND this
//! NOS" (e.g. Cisco's `9.` prefix + an `ASA` keyword). Because prefix-bearing rules sort ahead of
//! `sysDescr`-only ones, the vendor-assigned enterprise OID still outranks a free-text keyword.

use std::sync::RwLock;

use regex::Regex;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_common::ClassificationRule;

/// Name of the built-in catch-all profile a SNMP-speaking device falls back to when no rule
/// matches (mirrors `yagra_common::builtin_profiles`).
const GENERIC_SNMP_PROFILE: &str = "Generic SNMP";

/// The profile a classification resolved to, plus any vendor/model the rule pins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassificationMatch {
    pub profile_id: Uuid,
    pub vendor: Option<String>,
    pub model: Option<String>,
}

/// A rule with its `sysDescr` pattern pre-compiled, ready for hot-path matching.
struct CompiledRule {
    sysobjectid_prefix: Option<String>,
    sysdescr_regex: Option<Regex>,
    profile_id: Uuid,
    vendor: Option<String>,
    model: Option<String>,
}

/// The immutable snapshot the classifier matches against. Rules are pre-sorted by ascending
/// priority so the first match in each pass is the most specific.
#[derive(Default)]
struct Snapshot {
    rules: Vec<CompiledRule>,
    generic_snmp_id: Option<Uuid>,
}

/// In-memory, reloadable classifier used by discovery. Cheap to clone behind an `Arc`.
pub struct Classifier {
    inner: RwLock<Snapshot>,
}

impl Default for Classifier {
    fn default() -> Self {
        Self {
            inner: RwLock::new(Snapshot::default()),
        }
    }
}

impl Classifier {
    /// An empty classifier (no rules) — every device falls back to "Generic SNMP" only once a
    /// snapshot with that profile id is loaded. Used as the initial state before the first load.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build a classifier directly from rules (compiling their regexes), bypassing the database.
    /// A test seam — production loads via [`Self::reload`].
    #[cfg(test)]
    #[must_use]
    pub fn from_rules(rules: Vec<ClassificationRule>, generic_snmp_id: Option<Uuid>) -> Self {
        Self {
            inner: RwLock::new(Self::compile(rules, generic_snmp_id)),
        }
    }

    fn compile(mut rules: Vec<ClassificationRule>, generic_snmp_id: Option<Uuid>) -> Snapshot {
        // Evaluation order: prefix-bearing rules before sysDescr-only ones (so an authoritative
        // sysObjectID always outranks a free-text keyword), then ascending priority, then longer
        // prefix first (a more specific prefix like `9.12.3.` before `9.`).
        rules.sort_by(|a, b| {
            let rank = |r: &ClassificationRule| u8::from(r.sysobjectid_prefix.is_none());
            let plen =
                |r: &ClassificationRule| r.sysobjectid_prefix.as_ref().map_or(0, String::len);
            rank(a)
                .cmp(&rank(b))
                .then_with(|| a.priority.cmp(&b.priority))
                .then_with(|| plen(b).cmp(&plen(a)))
        });
        let compiled = rules
            .into_iter()
            .filter(|r| r.enabled)
            .filter_map(|r| {
                let sysdescr_regex = match &r.sysdescr_regex {
                    None => None,
                    Some(pat) => match Regex::new(pat) {
                        Ok(re) => Some(re),
                        Err(e) => {
                            // Pattern is validated at the API edge; a bad one here means a
                            // legacy/hand-edited row — skip it rather than fail the load.
                            tracing::warn!(rule = %r.id, error = %e, "skipping rule with invalid sysdescr regex");
                            return None;
                        }
                    },
                };
                Some(CompiledRule {
                    sysobjectid_prefix: r.sysobjectid_prefix,
                    sysdescr_regex,
                    profile_id: r.profile_id.0,
                    vendor: r.vendor,
                    model: r.model,
                })
            })
            .collect();
        Snapshot {
            rules: compiled,
            generic_snmp_id,
        }
    }

    /// Reload the snapshot from the database. On error the existing snapshot is kept (the caller
    /// logs); classification stays available with the last-known-good rules.
    pub async fn reload(&self, repo: &ClassificationRepo) -> anyhow::Result<()> {
        let rules = repo.list_rules().await?;
        let generic = repo.generic_snmp_profile_id().await?;
        let snapshot = Self::compile(rules, generic);
        *self.inner.write().expect("classifier lock poisoned") = snapshot;
        Ok(())
    }

    /// Resolve a device signature to a profile, including the "Generic SNMP" fallback when the
    /// device answered SNMP (`sysObjectID` or `sysDescr` present) but matched no rule. Returns
    /// `None` for a device that gave no SNMP signal (ICMP-only) — the operator picks a profile.
    #[must_use]
    pub fn classify(
        &self,
        sysobjectid: Option<&str>,
        sysdescr: Option<&str>,
    ) -> Option<ClassificationMatch> {
        let snap = self.inner.read().expect("classifier lock poisoned");
        let oid = sysobjectid.map(str::trim).filter(|s| !s.is_empty());

        // First rule (in sorted order) whose every present matcher matches wins. A matcher whose
        // signal is absent disqualifies the rule, so a prefix+regex rule needs both to be present.
        for rule in &snap.rules {
            if let Some(prefix) = &rule.sysobjectid_prefix {
                match oid {
                    Some(o) if o.starts_with(prefix.as_str()) => {}
                    _ => continue,
                }
            }
            if let Some(re) = &rule.sysdescr_regex {
                match sysdescr {
                    Some(d) if re.is_match(d) => {}
                    _ => continue,
                }
            }
            return Some(match_of(rule));
        }
        // Fallback: any SNMP-speaking device gets the generic profile so it still collects the
        // standard set; an ICMP-only device (no SNMP signal) gets no suggestion.
        let answered_snmp = sysobjectid.is_some() || sysdescr.is_some();
        if answered_snmp {
            if let Some(id) = snap.generic_snmp_id {
                return Some(ClassificationMatch {
                    profile_id: id,
                    vendor: None,
                    model: None,
                });
            }
        }
        None
    }
}

fn match_of(rule: &CompiledRule) -> ClassificationMatch {
    ClassificationMatch {
        profile_id: rule.profile_id,
        vendor: rule.vendor.clone(),
        model: rule.model.clone(),
    }
}

/// PostgreSQL-backed store for the operator-editable classification rules.
pub struct ClassificationRepo {
    pool: PgPool,
}

impl ClassificationRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// All rules, ascending by priority (the classifier's evaluation order).
    pub async fn list_rules(&self) -> anyhow::Result<Vec<ClassificationRule>> {
        let rows = sqlx::query(
            "SELECT id, priority, sysobjectid_prefix, sysdescr_regex, profile_id, vendor, model, enabled \
             FROM classification_rules ORDER BY priority, id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(ClassificationRule {
                    id: row.try_get("id")?,
                    priority: row.try_get("priority")?,
                    sysobjectid_prefix: row.try_get("sysobjectid_prefix")?,
                    sysdescr_regex: row.try_get("sysdescr_regex")?,
                    profile_id: row.try_get::<Uuid, _>("profile_id")?.into(),
                    vendor: row.try_get("vendor")?,
                    model: row.try_get("model")?,
                    enabled: row.try_get("enabled")?,
                })
            })
            .collect()
    }

    /// Whether a profile with this id exists — for validating a rule's `profile_id` at the API
    /// edge (a clearer 400 than letting the FK insert fail).
    pub async fn profile_exists(&self, id: Uuid) -> anyhow::Result<bool> {
        let row = sqlx::query("SELECT 1 AS one FROM profiles WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.is_some())
    }

    /// The id of the built-in "Generic SNMP" profile (the classifier's SNMP fallback), if seeded.
    pub async fn generic_snmp_profile_id(&self) -> anyhow::Result<Option<Uuid>> {
        let row = sqlx::query("SELECT id FROM profiles WHERE name = $1")
            .bind(GENERIC_SNMP_PROFILE)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get::<Uuid, _>("id")))
    }

    /// Create a rule; returns its new id. Caller validates the regex/prefix/profile first.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_rule(
        &self,
        priority: i32,
        sysobjectid_prefix: Option<&str>,
        sysdescr_regex: Option<&str>,
        profile_id: Uuid,
        vendor: Option<&str>,
        model: Option<&str>,
        enabled: bool,
    ) -> anyhow::Result<Uuid> {
        let row = sqlx::query(
            "INSERT INTO classification_rules \
                (id, priority, sysobjectid_prefix, sysdescr_regex, profile_id, vendor, model, enabled) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(priority)
        .bind(sysobjectid_prefix)
        .bind(sysdescr_regex)
        .bind(profile_id)
        .bind(vendor)
        .bind(model)
        .bind(enabled)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("id")?)
    }

    /// Update a rule in place. Returns whether the row existed.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_rule(
        &self,
        id: Uuid,
        priority: i32,
        sysobjectid_prefix: Option<&str>,
        sysdescr_regex: Option<&str>,
        profile_id: Uuid,
        vendor: Option<&str>,
        model: Option<&str>,
        enabled: bool,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE classification_rules SET \
                priority = $2, sysobjectid_prefix = $3, sysdescr_regex = $4, profile_id = $5, \
                vendor = $6, model = $7, enabled = $8, updated_at = now() \
             WHERE id = $1",
        )
        .bind(id)
        .bind(priority)
        .bind(sysobjectid_prefix)
        .bind(sysdescr_regex)
        .bind(profile_id)
        .bind(vendor)
        .bind(model)
        .bind(enabled)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Delete a rule by id. Returns whether a row was removed.
    pub async fn delete_rule(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM classification_rules WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(
        priority: i32,
        prefix: Option<&str>,
        regex: Option<&str>,
        profile: u128,
    ) -> ClassificationRule {
        ClassificationRule {
            id: Uuid::from_u128(u128::from(priority as u32) + 1),
            priority,
            sysobjectid_prefix: prefix.map(str::to_owned),
            sysdescr_regex: regex.map(str::to_owned),
            profile_id: Uuid::from_u128(profile).into(),
            vendor: None,
            model: None,
            enabled: true,
        }
    }

    const CISCO: u128 = 0xC15C0;
    const HUAWEI: u128 = 0x4A1;
    const GENERIC: u128 = 0x9E2E61C;

    fn classifier() -> Classifier {
        let rules = vec![
            rule(100, Some("1.3.6.1.4.1.9."), None, CISCO),
            rule(110, None, Some("(?i)cisco|ios"), CISCO),
            rule(120, Some("1.3.6.1.4.1.2011."), None, HUAWEI),
            rule(130, None, Some("(?i)huawei|vrp|usg"), HUAWEI),
        ];
        Classifier::from_rules(rules, Some(Uuid::from_u128(GENERIC)))
    }

    #[test]
    fn sysobjectid_prefix_match_wins() {
        let c = classifier();
        let m = c
            .classify(Some("1.3.6.1.4.1.9.1.516"), Some("whatever"))
            .unwrap();
        assert_eq!(m.profile_id, Uuid::from_u128(CISCO));
    }

    #[test]
    fn sysobjectid_is_authoritative_over_conflicting_sysdescr_keyword() {
        // OID says Huawei; sysDescr says "cisco". The authoritative OID must win even though
        // the Cisco keyword rule (110) has a lower priority number than the Huawei OID rule (120).
        let c = classifier();
        let m = c
            .classify(Some("1.3.6.1.4.1.2011.2.1"), Some("cisco ios"))
            .unwrap();
        assert_eq!(m.profile_id, Uuid::from_u128(HUAWEI));
    }

    #[test]
    fn falls_back_to_sysdescr_regex_when_no_oid_prefix_matches() {
        let c = classifier();
        // sysObjectID under an unknown enterprise → no prefix match; keyword classifies it.
        let m = c
            .classify(Some("1.3.6.1.4.1.99999.1"), Some("Huawei VRP USG6000"))
            .unwrap();
        assert_eq!(m.profile_id, Uuid::from_u128(HUAWEI));
    }

    #[test]
    fn unknown_snmp_device_falls_back_to_generic() {
        let c = classifier();
        let m = c.classify(None, Some("Linux server net-snmp")).unwrap();
        assert_eq!(m.profile_id, Uuid::from_u128(GENERIC));
    }

    #[test]
    fn icmp_only_device_gets_no_suggestion() {
        let c = classifier();
        assert!(c.classify(None, None).is_none());
    }

    #[test]
    fn disabled_rules_are_skipped() {
        let mut r = rule(100, Some("1.3.6.1.4.1.9."), None, CISCO);
        r.enabled = false;
        let c = Classifier::from_rules(vec![r], Some(Uuid::from_u128(GENERIC)));
        // The Cisco rule is disabled, so an SNMP device falls through to generic.
        let m = c.classify(Some("1.3.6.1.4.1.9.1.1"), None).unwrap();
        assert_eq!(m.profile_id, Uuid::from_u128(GENERIC));
    }

    #[test]
    fn combined_prefix_and_regex_requires_both() {
        const ASA: u128 = 0xA5A;
        const CAT: u128 = 0xCA7;
        // Two Cisco rules sharing the 9. prefix: an ASA rule (prefix AND descr) at lower priority,
        // and a prefix-only Catalyst catch-all.
        let rules = vec![
            rule(30, Some("1.3.6.1.4.1.9."), Some(r"(?i)\bASA\b"), ASA),
            rule(80, Some("1.3.6.1.4.1.9."), None, CAT),
        ];
        let c = Classifier::from_rules(rules, Some(Uuid::from_u128(GENERIC)));
        // ASA: both matchers satisfied → ASA.
        let asa = c
            .classify(
                Some("1.3.6.1.4.1.9.1.745"),
                Some("Cisco Adaptive Security Appliance ASA"),
            )
            .unwrap();
        assert_eq!(asa.profile_id, Uuid::from_u128(ASA));
        // Catalyst (same prefix, no ASA keyword) → the ASA rule's regex fails, catch-all wins.
        let cat = c
            .classify(
                Some("1.3.6.1.4.1.9.1.516"),
                Some("Cisco IOS Software, C2960"),
            )
            .unwrap();
        assert_eq!(cat.profile_id, Uuid::from_u128(CAT));
    }

    #[test]
    fn more_specific_prefix_beats_vendor_catch_all() {
        const NEXUS: u128 = 0x4E;
        const CAT: u128 = 0xCA7;
        let rules = vec![
            rule(10, Some("1.3.6.1.4.1.9.12.3."), None, NEXUS),
            rule(80, Some("1.3.6.1.4.1.9."), None, CAT),
        ];
        let c = Classifier::from_rules(rules, Some(Uuid::from_u128(GENERIC)));
        let m = c.classify(Some("1.3.6.1.4.1.9.12.3.1.3"), None).unwrap();
        assert_eq!(m.profile_id, Uuid::from_u128(NEXUS));
    }

    #[test]
    fn prefix_rule_needs_its_descr_when_descr_absent() {
        // A prefix+regex rule can't match a device that returned no sysDescr; the catch-all does.
        const ASA: u128 = 0xA5A;
        const CAT: u128 = 0xCA7;
        let rules = vec![
            rule(30, Some("1.3.6.1.4.1.9."), Some(r"(?i)\bASA\b"), ASA),
            rule(80, Some("1.3.6.1.4.1.9."), None, CAT),
        ];
        let c = Classifier::from_rules(rules, Some(Uuid::from_u128(GENERIC)));
        let m = c.classify(Some("1.3.6.1.4.1.9.1.745"), None).unwrap();
        assert_eq!(m.profile_id, Uuid::from_u128(CAT));
    }

    #[test]
    fn builtin_rule_regexes_all_compile() {
        // The seeded rules are loaded + compiled at runtime; a malformed pattern would be
        // silently dropped (losing a vendor mapping). Catch it here instead.
        for r in yagra_common::builtin_classification_rules() {
            if let Some(re) = r.sysdescr_regex {
                regex::Regex::new(re)
                    .unwrap_or_else(|e| panic!("builtin regex {re:?} does not compile: {e}"));
            }
        }
    }

    #[test]
    fn invalid_regex_rule_is_dropped_not_fatal() {
        let good = rule(100, Some("1.3.6.1.4.1.9."), None, CISCO);
        let bad = rule(50, None, Some("(unclosed"), HUAWEI);
        let c = Classifier::from_rules(vec![good, bad], Some(Uuid::from_u128(GENERIC)));
        // The bad rule is dropped; the good one still classifies.
        let m = c.classify(Some("1.3.6.1.4.1.9.1.1"), None).unwrap();
        assert_eq!(m.profile_id, Uuid::from_u128(CISCO));
    }
}
