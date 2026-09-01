// SPDX-License-Identifier: AGPL-3.0-only
//! Bootstrap: the rows a fresh deployment must have before anything works.
//!
//! **The one file whose SQL is not about a single table**, and the exemption is structural rather
//! than granted: seeding is by definition writing the catalogue, so it touches
//! `collection_templates`, `collection_template_items`, `profiles`,
//! `profile_collection_templates`, `collection_items`, `classification_rules`, `thresholds` and
//! `nodes`. [`super::guards`] holds that as a declaration with this sentence as its reason.
//!
//! 🚨 **Seed ids are array positions.** Every id here is `SeedRange::X.id(i)` where `i` is the index
//! in the corresponding `yagra_common::builtin_*()` array, so **inserting an entry mid-array
//! silently re-keys every entry after it** and breaks existing node→profile bindings in production.
//! Always append. Changing an existing built-in also needs a range-delete migration, because the
//! stable id's `ON CONFLICT` shadows the stale row (`extensibility.md` §6).
//!
//! ✅ **[`NodeRepo::seed_builtin_profiles`] is tested now** (ADR-114). This doc used to say it had
//! no test and could not have one: its effect is thirty-odd INSERTs against a live database, which
//! is not a seam of the shape ADR-092 introduced, so the only thing exercising it was that it runs
//! on every boot of every deployment. `#[sqlx::test]` runs those INSERTs — including, at last, a
//! check that every seeded id really is its array position rather than a comment asking for it.

use std::collections::HashMap;

use sqlx::Row;
use uuid::Uuid;
use yagra_common::MetricKind;

// Only the settings struct: `retention::Row` would collide with `sqlx::Row` above.

use super::defaults::DEFAULT_THRESHOLDS;
use super::*;

/// Fixed id for the seeded demo node the walking-skeleton WebUI queries.
pub(super) const DEMO_NODE_ID: Uuid = Uuid::nil();

impl NodeRepo {
    /// If the inventory is empty, seed a few demo nodes so the walking skeleton shows
    /// real ICMP data immediately. Idempotent: only seeds an empty table, so a node an operator
    /// deleted stays deleted **as long as any node remains**.
    ///
    /// ⚠️ **Emptying the inventory completely brings all three back on the next boot**, because
    /// the guard is `count(*) = 0` and nothing records that this deployment has already been
    /// bootstrapped. Measured by `the_demo_seed_returns_once_the_inventory_is_completely_empty`
    /// (ADR-114); the doc here used to claim otherwise.
    pub async fn seed_demo_nodes_if_empty(&self) -> anyhow::Result<()> {
        let count: i64 = sqlx::query("SELECT count(*) AS n FROM nodes")
            .fetch_one(&self.pool)
            .await?
            .try_get("n")?;
        if count > 0 {
            return Ok(());
        }

        // The DEMO_NODE_ID (nil) → loopback node is what the WebUI NodeDetail queries; it
        // is always reachable so the end-to-end path is provable even with no internet.
        let demo: [(Uuid, &str, &str); 3] = [
            (DEMO_NODE_ID, "demo-localhost", "127.0.0.1"),
            (
                Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0101_0101),
                "cloudflare-dns",
                "1.1.1.1",
            ),
            (
                Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0808_0808),
                "google-dns",
                "8.8.8.8",
            ),
        ];
        for (id, name, addr) in demo {
            sqlx::query(
                "INSERT INTO nodes (id, name, address) VALUES ($1, $2, $3::inet) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(id)
            .bind(name)
            .bind(addr)
            .execute(&self.pool)
            .await?;
        }
        tracing::info!(
            seeded = demo.len(),
            "seeded demo nodes into empty inventory"
        );
        Ok(())
    }

    /// Seed the built-in **collection templates** (Standard SNMP, vendor health) and the
    /// **device profiles** (Generic ping/SNMP, Cisco, Huawei) that reference them. Idempotent
    /// and non-destructive: every row uses a stable id and `ON CONFLICT DO NOTHING`, so the
    /// catalog reliably exists after a deploy without clobbering operator edits. Runs every
    /// boot. Also removes the legacy profile-scope `collection_items` the built-in profiles
    /// used to carry (PR #12) — profiles are now templates-only, so those would be ignored.
    pub async fn seed_builtin_profiles(&self) -> anyhow::Result<()> {
        // The stable bases live in `crate::seed_ids`, not here: the config-bundle exporter has to
        // recognise a built-in row to leave it out of a bundle, and a filter that disagreed with
        // this seeder would drop operator rows or carry re-keying ones. Same table, both sides.
        use crate::seed_ids::SeedRange;

        // 1. Templates + their metrics; remember name → id for the profile links.
        let mut template_id_by_name: HashMap<&'static str, Uuid> = HashMap::new();
        for (i, template) in yagra_common::builtin_templates().into_iter().enumerate() {
            let template_id = SeedRange::CollectionTemplates.id(i);
            template_id_by_name.insert(template.name, template_id);
            sqlx::query(
                "INSERT INTO collection_templates (id, name, description) VALUES ($1, $2, $3) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(template_id)
            .bind(template.name)
            .bind(template.description)
            .execute(&self.pool)
            .await?;
            for item in template.items {
                // One source for the stored token, shared with the reader — see
                // `CollectionKind::from_token`, whose absence once made a whole feature inert.
                let collection = item.kind.as_str();
                let metric_kind = match item.metric_kind {
                    MetricKind::Gauge => "gauge",
                    MetricKind::Counter => "counter",
                };
                sqlx::query(
                    "INSERT INTO collection_template_items \
                        (id, template_id, metric_name, oid, collection, metric_kind, enabled) \
                     VALUES ($1, $2, $3, $4, $5, $6, true) \
                     ON CONFLICT (template_id, metric_name) DO NOTHING",
                )
                .bind(Uuid::new_v4())
                .bind(template_id)
                .bind(&item.metric_name)
                .bind(&item.oid)
                .bind(collection)
                .bind(metric_kind)
                .execute(&self.pool)
                .await?;
            }
        }

        // 2. Profiles + their template links; drop any legacy profile-scope collection items.
        let mut profile_id_by_name: HashMap<&'static str, Uuid> = HashMap::new();
        for (i, profile) in yagra_common::builtin_profiles().into_iter().enumerate() {
            let profile_id = SeedRange::Profiles.id(i);
            profile_id_by_name.insert(profile.name, profile_id);
            sqlx::query(
                "INSERT INTO profiles (id, name, category, vendor) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(profile_id)
            .bind(profile.name)
            .bind(profile.category.as_str())
            .bind(profile.vendor)
            .execute(&self.pool)
            .await?;
            // Legacy cleanup: built-in profiles no longer carry direct OIDs (templates-only).
            sqlx::query(
                "DELETE FROM collection_items WHERE scope_level = 'profile' AND scope_id = $1",
            )
            .bind(profile_id)
            .execute(&self.pool)
            .await?;
            for template_name in profile.templates {
                if let Some(template_id) = template_id_by_name.get(template_name) {
                    sqlx::query(
                        "INSERT INTO profile_collection_templates (profile_id, template_id) \
                         VALUES ($1, $2) ON CONFLICT DO NOTHING",
                    )
                    .bind(profile_id)
                    .bind(template_id)
                    .execute(&self.pool)
                    .await?;
                }
            }
        }
        // 3. Built-in classification rules (discovery → suggested profile). Stable ids +
        //    ON CONFLICT DO NOTHING so operator edits survive restarts; references the profile
        //    ids seeded just above. Rules for an unknown profile name are skipped defensively.
        for (i, rule) in yagra_common::builtin_classification_rules()
            .into_iter()
            .enumerate()
        {
            let Some(&profile_id) = profile_id_by_name.get(rule.profile_name) else {
                tracing::warn!(
                    profile = rule.profile_name,
                    "skipping seed rule for unknown profile"
                );
                continue;
            };
            let rule_id = SeedRange::ClassificationRules.id(i);
            sqlx::query(
                "INSERT INTO classification_rules \
                    (id, priority, sysobjectid_prefix, sysdescr_regex, profile_id, vendor, model) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (id) DO NOTHING",
            )
            .bind(rule_id)
            .bind(rule.priority)
            .bind(rule.sysobjectid_prefix)
            .bind(rule.sysdescr_regex)
            .bind(profile_id)
            .bind(rule.vendor)
            .bind(rule.model)
            .execute(&self.pool)
            .await?;
        }
        // 4. Default thresholds for the built-in URL/HTTP endpoint profile so a freshly created URL
        //    monitor alerts out of the box: `http_up` below 0.5 ⇒ critical (down or wrong status),
        //    and `ssl_cert_days_to_expiry` below 30/7 ⇒ warning/critical. Stable ids + ON CONFLICT
        //    DO NOTHING keep operator edits/deletes from being resurrected on the next boot.
        //
        //    NB: `http_up` is a 0/1 gauge and the engine's "below" comparison is INCLUSIVE
        //    (`value <= bound`, thresholds.rs). A bound of 1.0 would therefore fire on the healthy
        //    value 1 too — so the bound sits between the two states (0.5): only 0 (down/wrong-status)
        //    trips it. Migration 0030 corrects already-seeded rows that used the old 1.0 bound.
        if let Some(&url_profile_id) = profile_id_by_name.get("URL / HTTP endpoint") {
            let scope_id = url_profile_id.to_string();
            // (offset, metric, direction, warning, critical, dwell_samples)
            let defaults = [
                (0usize, "http_up", "below", None::<f64>, Some(0.5), 2i32),
                (
                    1usize,
                    "ssl_cert_days_to_expiry",
                    "below",
                    Some(30.0),
                    Some(7.0),
                    1i32,
                ),
                // ADR-047 Inc.2. Seeded because this is a 0/1 gauge whose only sane bound is the
                // same 0.5, and because a content rule an operator configured must alert without
                // a second step.
                //
                // (This note used to contrast with `http_response_time_ms`, "which has no
                // defensible default". ADR-077 decision 4 gave it one — a deliberately loose
                // 3000 ms — so the contrast is gone; see the row below.)
                //
                // It is inert until someone configures a rule: the poller emits this metric only
                // for a monitor that carries one, so adding the row to an existing deployment
                // produces no samples and therefore no alerts on any monitor as it stands today.
                //
                // Offsets are explicit and this one is appended — the ids are stable and
                // ON CONFLICT DO NOTHING, so reusing or reordering one would shadow an operator's
                // edited row rather than update it (see `seed_ids`).
                (
                    2usize,
                    "http_body_match",
                    "below",
                    None::<f64>,
                    Some(0.5),
                    2i32,
                ),
                // ADR-077 decision 4. This REVERSES the note above: response time was deliberately
                // left unseeded because it "varies far too much between environments", and the
                // measurement is what changed the judgement rather than the fact. Two sites polled
                // from the same place differed by 180 ms at the median (Google ~390 ms, another
                // ~570 ms), so a *tight* default is still impossible — but the observed maximum was
                // 685 ms, and 3000 ms sits four times outside that spread. A web page taking three
                // seconds is wrong in any environment.
                //
                // Warning only, and no critical: a site that is actually down fires `http_up` at
                // critical, and two criticals for one outage is the notification storm this
                // project treats as a bug.
                (
                    3usize,
                    "http_response_time_ms",
                    "above",
                    Some(3000.0),
                    None,
                    2i32,
                ),
            ];
            for (offset, metric, direction, warning, critical, dwell) in defaults {
                sqlx::query(
                    "INSERT INTO thresholds \
                        (id, scope_level, scope_id, metric, direction, warning, critical, dwell_samples) \
                     VALUES ($1, 'profile', $2, $3, $4, $5, $6, $7) ON CONFLICT (id) DO NOTHING",
                )
                .bind(SeedRange::UrlThresholds.id(offset))
                .bind(&scope_id)
                .bind(metric)
                .bind(direction)
                .bind(warning)
                .bind(critical)
                .bind(dwell)
                .execute(&self.pool)
                .await?;
            }
        }
        // 6. Default threshold for the built-in DNS profile (ADR-033), so a freshly created DNS
        //    monitor alerts out of the box: `dns_up` below 0.5 ⇒ critical. It reads 0 whenever the
        //    name does not resolve for ANY reason — NXDOMAIN / SERVFAIL / REFUSED / timeout /
        //    CNAME loop / depth exceeded — so this one threshold covers them all.
        //
        //    NB the bound is 0.5, NOT 1.0. `dns_up` is a 0/1 gauge and the engine's "below"
        //    comparison is INCLUSIVE (`value <= bound`, thresholds.rs), so 1.0 would fire on the
        //    healthy value too. That is exactly the mistake migration 0030 had to correct for
        //    `http_up`; seeds are ON CONFLICT DO NOTHING, so getting it wrong needs a corrective
        //    migration rather than an edit here.
        //
        //    `dns_resolve_ms` DOES have a seeded threshold since ADR-077 decision 4, and it is a
        //    reversal: this comment used to say resolver latency "varies far too much between
        //    environments for a default". The spread is real (measured 5–45 ms here) — what
        //    changed is that a bound twenty times outside it, 1000 ms, sits beyond the spread
        //    rather than inside it. Warning only; `dns_up` is what pages when a name is dead.
        //
        //    Reserved stable-id ranges: every one of them is declared in `crate::seed_ids`, which
        //    is also what migration 0020's range-DELETEs are tested against.
        if let Some(&dns_profile_id) = profile_id_by_name.get("DNS name resolution") {
            let scope_id = dns_profile_id.to_string();
            // (offset, metric, direction, warning, critical, dwell_samples)
            let defaults = [
                (0usize, "dns_up", "below", None::<f64>, Some(0.5), 2i32),
                // ADR-077 decision 4, the DNS half — see the URL profile above for the argument.
                // Measured resolution on this deployment ran 5–45 ms, so 1000 ms is twenty times
                // outside the observed spread. A name taking a second to resolve is wrong
                // anywhere. Warning only: a name that does not resolve fires `dns_up` at critical.
                (1usize, "dns_resolve_ms", "above", Some(1000.0), None, 2i32),
            ];
            for (offset, metric, direction, warning, critical, dwell) in defaults {
                sqlx::query(
                    "INSERT INTO thresholds \
                        (id, scope_level, scope_id, metric, direction, warning, critical, dwell_samples) \
                     VALUES ($1, 'profile', $2, $3, $4, $5, $6, $7) ON CONFLICT (id) DO NOTHING",
                )
                .bind(SeedRange::DnsThresholds.id(offset))
                .bind(&scope_id)
                .bind(metric)
                .bind(direction)
                .bind(warning)
                .bind(critical)
                .bind(dwell)
                .execute(&self.pool)
                .await?;
            }
        }
        // 7. The fleet defaults (ADR-075) — `global` scope, so they reach every node including
        //    the ones no profile-scoped rule can: a node with no profile, and a node on a profile
        //    the operator created. Three rows, and each is an ordinary row: editable per scope,
        //    and deletable. Deleting the reachability one stops node-down paging, which is the
        //    point of making it a rule; the screen says so.
        //
        //    WARNING: the bounds on the 0/1 gauges are **0.5, not 1.0**. The engine's `below`
        //    comparison is inclusive (`value <= bound`, `yagra_common::thresholds`), so 1.0 would
        //    fire on the healthy value too — the mistake migration 0030 had to correct for
        //    `http_up`. These are `ON CONFLICT (id) DO NOTHING`, so getting one wrong needs a
        //    corrective migration rather than an edit here.
        //
        //    WARNING: reachability carries **no bound at all** — it is not evaluated against a
        //    sample. The engine reads only `dwell_samples` off it and takes the severity from the
        //    committed `NodeState` (`Unreachable` is always critical). A direction is stored
        //    because the column is NOT NULL, and is unread.
        //
        //    Offsets are explicit and **append-only** — the ids are derived from them and are
        //    `DO NOTHING`, so reusing or reordering one would shadow an operator's edited row
        //    instead of updating it (see `seed_ids`).
        {
            for (offset, profiles, metric, direction, warning, critical, dwell) in
                DEFAULT_THRESHOLDS
            {
                // Resolve the profile names to ids. A name the catalog does not have is skipped
                // with a warning rather than failing the boot — the same defensive shape the
                // classification-rule seeder uses — but a row that resolves to NOTHING is skipped
                // whole, because storing it would create a rule that is listed and matches nobody.
                let mut scope_ids: Vec<String> = Vec::with_capacity(profiles.len());
                for name in profiles {
                    match profile_id_by_name.get(*name) {
                        Some(id) => scope_ids.push(id.to_string()),
                        None => tracing::warn!(
                            profile = *name,
                            metric,
                            "skipping default threshold target for unknown profile"
                        ),
                    }
                }
                if !profiles.is_empty() && scope_ids.is_empty() {
                    tracing::warn!(
                        metric,
                        "skipping default threshold: no target profile exists"
                    );
                    continue;
                }
                let level = if profiles.is_empty() {
                    "global"
                } else {
                    "profile"
                };
                sqlx::query(
                    "INSERT INTO thresholds \
                        (id, scope_level, scope_id, scope_ids, metric, direction, warning, \
                         critical, dwell_samples) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT (id) DO NOTHING",
                )
                .bind(SeedRange::DefaultThresholds.id(offset))
                .bind(level)
                // The legacy column keeps the first target, so a core predating migration 0096
                // still resolves the rule (at one profile instead of all of them).
                .bind(scope_ids.first().map_or("", String::as_str))
                .bind(&scope_ids)
                .bind(metric)
                .bind(direction)
                .bind(warning)
                .bind(critical)
                .bind(dwell)
                .execute(&self.pool)
                .await?;
            }
        }
        tracing::info!(
            "seeded built-in collection templates + device profiles + classification rules"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seed_ids::SeedRange;

    /// The catalogue tables [`NodeRepo::seed_builtin_profiles`] **fills**, with the columns that
    /// identify a row in each.
    ///
    /// ⚠️ Two of the eight this file declares in [`super::super::guards`] are deliberately absent,
    /// and the difference is the point: that list is "tables whose name may appear in this file's
    /// SQL", which is not the same set.
    ///
    /// * `collection_items` is only ever **deleted** from here — the legacy profile-scope rows the
    ///   built-in profiles used to carry. A first cut of this test asserted the seed fills it and
    ///   failed, which is the check working.
    /// * `nodes` belongs to [`NodeRepo::seed_demo_nodes_if_empty`], a different decision, below.
    const CATALOGUE: [(&str, &str); 6] = [
        ("collection_templates", "id"),
        ("collection_template_items", "template_id, metric_name"),
        ("profiles", "id"),
        // A join table with a composite primary key and no `id` column.
        ("profile_collection_templates", "profile_id, template_id"),
        ("classification_rules", "id"),
        ("thresholds", "id"),
    ];

    /// **Bootstrapping an empty database fills every catalogue table.**
    ///
    /// This function had no test at all before ADR-114 — the module doc said so in as many words,
    /// and said why: its effect is thirty-odd INSERTs against a live database. The only thing
    /// exercising it was that it runs on every boot of every deployment.
    ///
    /// Each table is asserted separately rather than as a total, because the failure worth
    /// catching is one loop silently writing nothing while the others still do.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn bootstrapping_fills_every_catalogue_table(pool: sqlx::PgPool) {
        let repo = crate::pgtest::repo(pool.clone());
        for (table, _) in CATALOGUE {
            assert_eq!(
                crate::pgtest::rows(&pool, table).await,
                0,
                "{table} is not empty before the seed"
            );
        }

        repo.seed_builtin_profiles().await.expect("seed");

        for (table, _) in CATALOGUE {
            assert!(
                crate::pgtest::rows(&pool, table).await > 0,
                "{table} is still empty after the seed"
            );
        }
        // The other half of what this function does: it removes the legacy profile-scope
        // collection items rather than writing any. Nothing seeds that table.
        assert_eq!(crate::pgtest::rows(&pool, "collection_items").await, 0);
    }

    /// **Seeding twice changes nothing** — which is what the second boot of every deployment does.
    ///
    /// The whole `ON CONFLICT DO NOTHING` scheme rests on this, and the way it breaks is not a
    /// duplicate row (the constraints stop that) but a *re-keyed* one: a row whose id is derived
    /// from an array position inserts beside the old one rather than colliding with it. So the
    /// comparison is over the key sets, not the counts.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn seeding_twice_leaves_the_same_rows(pool: sqlx::PgPool) {
        let repo = crate::pgtest::repo(pool.clone());
        repo.seed_builtin_profiles().await.expect("first seed");
        let before = catalogue_keys(&pool).await;

        repo.seed_builtin_profiles().await.expect("second seed");
        let after = catalogue_keys(&pool).await;

        assert_eq!(before, after, "the second seed changed the catalogue");
        assert!(
            before.iter().all(|(_, keys)| !keys.is_empty()),
            "a table was compared while empty: {before:?}"
        );
    }

    /// 🚨 **Every seeded id is its entry's array position.**
    ///
    /// The landmine CLAUDE.md names by hand: `SeedRange::X.id(i)` takes `i` from the position in
    /// `yagra_common::builtin_*()`, so **inserting an entry mid-array silently re-keys every later
    /// row** and breaks the node→profile bindings of every existing deployment. Until now the only
    /// thing standing between that and production was the warning comment.
    ///
    /// The two named catalogues are checked per name, so a failure reports *which* entry moved
    /// rather than that something did.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn every_seeded_id_is_its_arrays_position(pool: sqlx::PgPool) {
        crate::pgtest::repo(pool.clone())
            .seed_builtin_profiles()
            .await
            .expect("seed");

        let expected_profiles: Vec<(Uuid, String)> = yagra_common::builtin_profiles()
            .iter()
            .enumerate()
            .map(|(i, p)| (SeedRange::Profiles.id(i), p.name.to_owned()))
            .collect();
        assert!(!expected_profiles.is_empty(), "the catalogue is empty");
        assert_eq!(stored(&pool, "profiles").await, expected_profiles);

        let expected_templates: Vec<(Uuid, String)> = yagra_common::builtin_templates()
            .iter()
            .enumerate()
            .map(|(i, t)| (SeedRange::CollectionTemplates.id(i), t.name.to_owned()))
            .collect();
        assert_eq!(
            stored(&pool, "collection_templates").await,
            expected_templates
        );

        // Classification rules have no unique name, so they are compared by id alone — which is
        // still exactly what shifts when an entry is inserted mid-array.
        let expected_rules: Vec<Uuid> = (0..yagra_common::builtin_classification_rules().len())
            .map(|i| SeedRange::ClassificationRules.id(i))
            .collect();
        let stored_rules: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM classification_rules ORDER BY id")
                .fetch_all(&pool)
                .await
                .expect("read classification_rules");
        assert_eq!(stored_rules, expected_rules);
    }

    /// ⚠️ **Emptying the inventory completely brings the demo nodes back.**
    ///
    /// 🔧 **Measured, not intended.** The doc on [`NodeRepo::seed_demo_nodes_if_empty`] said it
    /// "won't resurrect nodes an operator has deleted", and that is true of *some* — the guard is
    /// `count(*) = 0`, so deleting one node keeps the rest safe. It is false of the case the
    /// sentence describes as a whole: delete every node and the next boot seeds three again.
    ///
    /// This test pins what the code does rather than what the comment claimed, and the comment has
    /// been narrowed to match. Whether the behaviour itself should change is a separate decision
    /// (it needs somewhere to record "already bootstrapped", which is a stored setting and a new
    /// default) and is recorded in ADR-114 rather than made here.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_demo_seed_returns_once_the_inventory_is_completely_empty(pool: sqlx::PgPool) {
        let repo = crate::pgtest::repo(pool.clone());
        repo.seed_demo_nodes_if_empty().await.expect("first seed");
        let seeded = crate::pgtest::rows(&pool, "nodes").await;
        assert!(seeded > 0, "the demo seed wrote no nodes");

        // Delete one: the guard holds, because the table is not empty.
        let first = repo.list_nodes().await.expect("list")[0].id.as_uuid();
        repo.delete_node(first).await.expect("delete one");
        repo.seed_demo_nodes_if_empty().await.expect("second seed");
        assert_eq!(
            crate::pgtest::rows(&pool, "nodes").await,
            seeded - 1,
            "a single deleted node was resurrected"
        );

        // Delete the rest: the guard does not hold, and the whole set comes back.
        for node in repo.list_nodes().await.expect("list") {
            repo.delete_node(node.id.as_uuid()).await.expect("delete");
        }
        repo.seed_demo_nodes_if_empty().await.expect("third seed");
        assert_eq!(
            crate::pgtest::rows(&pool, "nodes").await,
            seeded,
            "the guard is `count(*) = 0`; an empty inventory is seeded again"
        );
    }

    /// `(id, name)` of every row of `table`, ordered by id.
    async fn stored(pool: &sqlx::PgPool, table: &str) -> Vec<(Uuid, String)> {
        // Interpolated because a table name cannot be bound; every caller is a literal above.
        sqlx::query_as::<_, (Uuid, String)>(&format!("SELECT id, name FROM {table} ORDER BY id"))
            .fetch_all(pool)
            .await
            .unwrap_or_else(|e| panic!("read {table}: {e}"))
    }

    /// Every catalogue table's key set as text, ordered — comparable across two seeds.
    async fn catalogue_keys(pool: &sqlx::PgPool) -> Vec<(&'static str, Vec<String>)> {
        let mut out = Vec::new();
        for (table, key) in CATALOGUE {
            let keys: Vec<String> = sqlx::query_scalar(&format!(
                "SELECT concat_ws('/', {key}) FROM {table} ORDER BY 1"
            ))
            .fetch_all(pool)
            .await
            .unwrap_or_else(|e| panic!("read {table}: {e}"));
            out.push((table, keys));
        }
        out
    }
}
