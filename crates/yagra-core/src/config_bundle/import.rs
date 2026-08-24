// SPDX-License-Identifier: AGPL-3.0-only
//! Applying a bundle **into** a deployment.
//!
//! Upsert only. Nothing is ever deleted and there is deliberately no "replace" mode — it would be
//! one boolean away, and that boolean is what makes an import unrecoverable. `dry_run` is this same
//! code path with a rollback at the end rather than a second, less-tested one.
//!
//! The tables are walked in [`super::BUNDLE_TABLES`] order, which is dependency order: a block
//! keeps the id set it wrote, and a later block drops a reference the target cannot resolve rather
//! than failing the row.

use super::*;
use crate::cadence::{compute_next_run, Cadence, Schedule};
use crate::seed_ids;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use std::collections::{BTreeMap, HashSet};
use uuid::Uuid;

impl ConfigBundleRepo {
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
            // A bundle written before ADR-078 carries only `scope_id`; one written after carries
            // the set. Fall back rather than skip — an old bundle is the common import, not an
            // error, and reading it as "no targets" would import a rule that matches nothing.
            let targets: Vec<String> = if t.scope_ids.is_empty() {
                if t.scope_id.is_empty() {
                    Vec::new()
                } else {
                    vec![t.scope_id.clone()]
                }
            } else {
                t.scope_ids.clone()
            };
            // 🚨 The vocabulary is checked here, and until ADR-081 it was not checked anywhere on
            // this path. `scope_level` and `direction` are TEXT columns with no CHECK constraint,
            // this importer bound them straight through, and the reader turns an unrecognised token
            // into a named default: an unknown level reads as `profile` (a rule that matches no
            // node and is silently inert), and an unknown direction reads as `above`. A bundle
            // carrying `"Below"` — one capital, from a hand edit or another product — therefore
            // imported `snmp_up below 0.5` as `above 0.5`, which pages for **every healthy device**.
            // The API edge has always parsed both through `from_token`; a second writer of the same
            // rows enforcing neither is the shape `extensibility.md` §3 names.
            let (Some(_), Some(direction)) = (
                yagra_common::ScopeLevel::from_token(&t.scope_level),
                yagra_common::Direction::from_token(&t.direction),
            ) else {
                notes.add(
                    "thresholds",
                    NoteCode::SkippedInvalidValue,
                    Some("scope_level/direction"),
                );
                c.skipped += 1;
                continue;
            };
            // ADR-081: the four bounds are the truth. An older bundle carries none of them, and
            // folds through the one conversion the database read also uses — so "what an old
            // bundle meant" is decided in one place rather than two.
            let mut bounds = yagra_common::ThresholdBounds {
                warning_below: t.warning_below,
                critical_below: t.critical_below,
                warning_above: t.warning_above,
                critical_above: t.critical_above,
            };
            if bounds.is_empty() {
                bounds =
                    yagra_common::ThresholdBounds::from_legacy(direction, t.warning, t.critical);
            }
            // A rule with no bound at all is stored, listed and never fires. Importing one would
            // put a rule on the operator's screen that does nothing, which is the failure ADR-081
            // exists to remove rather than to import.
            //
            // 🚨 Liveness is the exception, for the reason `api/thresholds.rs` spells out:
            // `__liveness__` is decided from the poll outcome rather than from a value, so a
            // bound-less row is its correct shape. A per-scope liveness override is a rule an
            // operator can legitimately have created and therefore legitimately export.
            if bounds.is_empty() && t.metric != crate::alerts::LIVENESS {
                notes.add("thresholds", NoteCode::SkippedInvalidValue, Some("bounds"));
                c.skipped += 1;
                continue;
            }
            // `scope_id`/`scope_ids` are TEXT because the legacy `group` scope is a tag value, not
            // a uuid. Only the levels that *are* uuids are validated; a tag scope has nothing to
            // resolve against, and `global` has no id at all.
            //
            // ⚠️ EVERY target is checked, not just the first: a rule that names four profiles of
            // which one is missing would otherwise import as a rule that silently covers three,
            // and the import report would say nothing happened.
            if matches!(t.scope_level.as_str(), "node" | "profile" | "group_id") {
                let all_known = !targets.is_empty()
                    && targets.iter().all(|s| {
                        s.parse::<Uuid>()
                            .is_ok_and(|id| match t.scope_level.as_str() {
                                "node" => node_ids.contains(&id),
                                "group_id" => group_ids.contains(&id),
                                _ => profile_ids.contains(&id),
                            })
                    });
                if !all_known {
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
                "INSERT INTO thresholds (id, scope_level, scope_id, scope_ids, metric, direction, \
                                         warning, critical, warning_below, critical_below, \
                                         warning_above, critical_above, dwell_samples) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
                 ON CONFLICT (id) DO UPDATE SET scope_level = EXCLUDED.scope_level, \
                     scope_id = EXCLUDED.scope_id, scope_ids = EXCLUDED.scope_ids, \
                     metric = EXCLUDED.metric, \
                     direction = EXCLUDED.direction, warning = EXCLUDED.warning, \
                     critical = EXCLUDED.critical, \
                     warning_below = EXCLUDED.warning_below, \
                     critical_below = EXCLUDED.critical_below, \
                     warning_above = EXCLUDED.warning_above, \
                     critical_above = EXCLUDED.critical_above, \
                     dwell_samples = EXCLUDED.dwell_samples",
            )
            .bind(t.id)
            .bind(&t.scope_level)
            // The legacy column keeps the first target, for the same reason `ThresholdWrite`
            // writes it: a core that predates ADR-078 resolves by it.
            .bind(targets.first().map_or("", String::as_str))
            .bind(&targets)
            .bind(&t.metric)
            // The legacy triple is derived from the bounds rather than copied from the bundle, for
            // the same reason `ThresholdWrite::legacy` derives it: a row saying `above` beside
            // bounds that face down leaves nothing able to say which half to believe.
            .bind(bounds.direction().as_str())
            .bind(bounds.warning())
            .bind(bounds.critical())
            .bind(bounds.warning_below)
            .bind(bounds.critical_below)
            .bind(bounds.warning_above)
            .bind(bounds.critical_above)
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
                                         follow_redirects, timeout_ms, credential_id, body_match, \
                                         json_extract, body_max_bytes) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
                 ON CONFLICT (node_id) DO UPDATE SET url = EXCLUDED.url, \
                     method = EXCLUDED.method, expected_status = EXCLUDED.expected_status, \
                     verify_tls = EXCLUDED.verify_tls, \
                     follow_redirects = EXCLUDED.follow_redirects, \
                     timeout_ms = EXCLUDED.timeout_ms, credential_id = EXCLUDED.credential_id, \
                     body_match = EXCLUDED.body_match, json_extract = EXCLUDED.json_extract, \
                     body_max_bytes = EXCLUDED.body_max_bytes, \
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
            .bind(&u.body_match)
            .bind(&u.json_extract)
            .bind(u.body_max_bytes)
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
}
