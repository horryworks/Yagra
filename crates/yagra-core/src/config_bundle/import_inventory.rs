// SPDX-License-Identifier: AGPL-3.0-only
//! Writing back **what is monitored**: profiles, collection templates and their items, the links
//! between the two, classification rules, node groups, and the nodes themselves.
//!
//! This half is defined by what leaves it. Every table here produces an id set a later block needs,
//! and the four that outlive this file are handed to [`super::import_attached`] as
//! [`super::import::Ids`]. Nothing here reads one from there, which is why the order cannot be got
//! wrong: the other half takes the value this one returns.
//!
//! `credentials` is the one table named here that the bundle does not carry. It is read, never
//! written — a bundle holds credential *ids* only, and a reference is kept exactly when the target
//! already holds that id (ADR-018: no code path in this repository turns a sealed secret back into
//! transportable plaintext).
//!
//! Two second passes are deliberate. A profile tree and a folder tree can each list a child before
//! its parent, and no single ordering fixes that for an arbitrary graph — so `parent_id` is applied
//! after every row of that table exists, and a parent that still does not resolve is dropped with a
//! note rather than failing the row.

use super::import::Ids;
use super::import::{bump, counter, id_set, keep_ref};
use super::*;
use crate::seed_ids;
use sqlx::{Postgres, Transaction};
use std::collections::BTreeMap;

pub(super) async fn write<'a>(
    mut tx: Transaction<'a, Postgres>,
    bundle: &ConfigBundle,
    notes: &mut Notes,
    counts: &mut BTreeMap<&'static str, TableResult>,
) -> Result<(Transaction<'a, Postgres>, Ids), BundleError> {
    // ── profiles ──────────────────────────────────────────────────────────────────────
    let mut seen = id_set(&mut tx, "SELECT id FROM profiles").await?;
    let c = counter(counts, "profiles");
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
    let c = counter(counts, "collection_templates");
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
    let c = counter(counts, "collection_template_items");
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

    let c = counter(counts, "profile_collection_templates");
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
    let c = counter(counts, "classification_rules");
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
    let c = counter(counts, "node_groups");
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
    let c = counter(counts, "nodes");
    for n in &bundle.nodes {
        let profile = keep_ref(n.profile_id, &profile_ids, notes, "nodes", "profile_id");
        let group = keep_ref(n.group_id, &group_ids, notes, "nodes", "group_id");
        // A credential is never carried, only referenced. It survives only when the target
        // already holds that exact id — which is the same-deployment case, not the migration
        // one; there the operator re-binds a credential they created on the target.
        let credential = keep_ref(
            n.credential_id,
            &credential_ids,
            notes,
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

    Ok((
        tx,
        Ids {
            profiles: profile_ids,
            groups: group_ids,
            nodes: node_ids,
            credentials: credential_ids,
        },
    ))
}
