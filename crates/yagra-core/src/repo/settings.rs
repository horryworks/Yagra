// SPDX-License-Identifier: AGPL-3.0-only
//! The `app_settings` table: the deployment's own settings, which are not about any node.
//!
//! One singleton row (`id = TRUE`) holding the default poll interval, the retention policy, the
//! adjacency-derivation settings, the topology mode and the Meraki toggle. This file is the
//! clearest reason `repo.rs` was split: it answers questions about the *deployment*, and it spent
//! its whole life inside a module whose doc called itself the nodes inventory.
//!
//! 🚨 **A read here that fails must not become a default that differs from the stored value.**
//! Several of these getters swallow the error and return the compiled default, which is correct
//! only because the row is seeded at startup and the compiled default is what the seeder writes.
//! A getter whose fallback disagreed with [`NodeRepo::seed_app_settings`] would silently change
//! behaviour whenever the database hiccuped.

use sqlx::Row;

// Only the settings struct: `retention::Row` would collide with `sqlx::Row` above.
use crate::neighbors::AdjacencySettings;
use crate::retention::RetentionSettings;
use crate::topology_mode::TopologyMode;

use super::*;

impl NodeRepo {
    /// The global default polling interval (seconds) from the singleton `app_settings` row. Falls
    /// back to the compiled default if the row is somehow absent (it is seeded at startup).
    pub async fn get_default_poll_interval(&self) -> anyhow::Result<u32> {
        let row =
            sqlx::query("SELECT default_poll_interval_secs FROM app_settings WHERE id = TRUE")
                .fetch_optional(&self.pool)
                .await?;
        match row {
            Some(r) => {
                let secs: i32 = r.try_get("default_poll_interval_secs")?;
                Ok(u32::try_from(secs).unwrap_or(crate::config::DEFAULT_POLL_INTERVAL_SECS))
            }
            None => Ok(crate::config::DEFAULT_POLL_INTERVAL_SECS),
        }
    }

    /// Set the global default polling interval (seconds), upserting the singleton row. Callers
    /// validate the bounds at the API edge; the table CHECK is the backstop.
    pub async fn set_default_poll_interval(&self, secs: u32) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO app_settings (id, default_poll_interval_secs, updated_at) \
             VALUES (TRUE, $1, now()) \
             ON CONFLICT (id) DO UPDATE SET default_poll_interval_secs = $1, updated_at = now()",
        )
        .bind(i32::try_from(secs).unwrap_or(i32::MAX))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Seed the singleton settings row on first boot from the env-var initial defaults.
    /// Idempotent: `ON CONFLICT DO NOTHING` preserves any operator-edited value across restarts,
    /// which is also why an *existing* deployment keeps the column defaults from migration 0061
    /// rather than importing `YAGRA_FLOW_RETENTION_DAYS` — see that migration's header for why
    /// importing it would delete flow rows nobody asked to lose.
    pub async fn seed_app_settings(
        &self,
        poll_interval_secs: u32,
        flow_retention_days: u32,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO app_settings (id, default_poll_interval_secs, flow_retention_days) \
             VALUES (TRUE, $1, $2) ON CONFLICT (id) DO NOTHING",
        )
        .bind(i32::try_from(poll_interval_secs).unwrap_or(i32::MAX))
        .bind(i32::try_from(flow_retention_days).unwrap_or(i32::MAX))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The operator-configured retention windows (ADR-040). Like `get_meraki_polling_enabled`, this
    /// returns a value rather than a `Result` and degrades to the compiled defaults on any read
    /// failure: a transient database blip must never silently widen or narrow how long data is
    /// kept, and the prune loops call this every tick.
    pub async fn get_retention_settings(&self) -> RetentionSettings {
        let fallback = RetentionSettings::default();
        let Ok(Some(row)) = sqlx::query(
            "SELECT alert_linked_retention_days, unmatched_event_retention_hours, \
                    report_run_retention_days, flow_retention_days, diagnostic_retention_days \
             FROM app_settings WHERE id = TRUE",
        )
        .fetch_optional(&self.pool)
        .await
        else {
            return fallback;
        };
        let read = |col: &str, default: u32| -> u32 {
            row.try_get::<i32, _>(col)
                .ok()
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(default)
        };
        RetentionSettings {
            alert_linked_days: read("alert_linked_retention_days", fallback.alert_linked_days),
            unmatched_event_hours: read(
                "unmatched_event_retention_hours",
                fallback.unmatched_event_hours,
            ),
            report_run_days: read("report_run_retention_days", fallback.report_run_days),
            flow_days: read("flow_retention_days", fallback.flow_days),
            diagnostic_days: read("diagnostic_retention_days", fallback.diagnostic_days),
        }
    }

    /// Set every retention window at once, upserting the singleton row. The API edge validates the
    /// bounds (`retention::days_in_bounds` / `hours_in_bounds`); the table CHECKs are the backstop.
    pub async fn set_retention_settings(&self, s: &RetentionSettings) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO app_settings (id, alert_linked_retention_days, \
                 unmatched_event_retention_hours, report_run_retention_days, \
                 flow_retention_days, diagnostic_retention_days, updated_at) \
             VALUES (TRUE, $1, $2, $3, $4, $5, now()) \
             ON CONFLICT (id) DO UPDATE SET alert_linked_retention_days = $1, \
                 unmatched_event_retention_hours = $2, report_run_retention_days = $3, \
                 flow_retention_days = $4, diagnostic_retention_days = $5, updated_at = now()",
        )
        .bind(i32::try_from(s.alert_linked_days).unwrap_or(i32::MAX))
        .bind(i32::try_from(s.unmatched_event_hours).unwrap_or(i32::MAX))
        .bind(i32::try_from(s.report_run_days).unwrap_or(i32::MAX))
        .bind(i32::try_from(s.flow_days).unwrap_or(i32::MAX))
        .bind(i32::try_from(s.diagnostic_days).unwrap_or(i32::MAX))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// How this deployment discovers connectivity: CDP/LLDP adjacency (ADR-038) and interface
    /// addresses (ADR-043).
    ///
    /// Like `get_retention_settings`, returns a value rather than a `Result` and degrades to the
    /// compiled defaults on any read failure: the scheduler calls this every sweep, and a database
    /// blip must not silently stop collecting or change the cadence.
    ///
    /// Each column is read independently and falls back on its own, so a deployment mid-upgrade —
    /// where migration `0065` has not run and the L3 columns do not exist yet — still gets working
    /// neighbour settings rather than the whole struct collapsing to defaults.
    /// 🚨 **Every column the writer writes must be in this projection.**
    /// `try_get` on a column the query did not select returns `Err`, and each field below turns
    /// an `Err` into the *default* — which is what an absent column looks like mid-upgrade, and
    /// is the whole reason for the fallbacks. The two are indistinguishable here, so a column
    /// left out of the `SELECT` does not fail: it silently pins that setting to its default
    /// forever. `media_discovery_enabled` and `media_interval_secs` were in exactly that state
    /// from ADR-063 Inc.2 until ADR-115 — written by the API, stored in the row, and read back
    /// as `true` / 3600 by the UI and the scheduler alike, so switching the media walk off did
    /// nothing at all. `every_adjacency_switch_round_trips_in_its_own_column` is what says so.
    pub async fn get_adjacency_settings(&self) -> AdjacencySettings {
        let fallback = AdjacencySettings::default();
        let Ok(Some(row)) = sqlx::query(
            "SELECT neighbor_discovery_enabled, neighbor_interval_secs, \
                    l3_discovery_enabled, l3_interval_secs, \
                    arp_discovery_enabled, arp_interval_secs, \
                    routing_discovery_enabled, routing_interval_secs, \
                    media_discovery_enabled, media_interval_secs \
             FROM app_settings WHERE id = TRUE",
        )
        .fetch_optional(&self.pool)
        .await
        else {
            return fallback;
        };
        AdjacencySettings {
            neighbors_enabled: row
                .try_get::<bool, _>("neighbor_discovery_enabled")
                .unwrap_or(fallback.neighbors_enabled),
            neighbors_interval_secs: row
                .try_get::<i32, _>("neighbor_interval_secs")
                .ok()
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(fallback.neighbors_interval_secs),
            l3_enabled: row
                .try_get::<bool, _>("l3_discovery_enabled")
                .unwrap_or(fallback.l3_enabled),
            l3_interval_secs: row
                .try_get::<i32, _>("l3_interval_secs")
                .ok()
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(fallback.l3_interval_secs),
            // Falls back to `false` mid-upgrade, before migration 0070 has run. That is the safe
            // direction for this one specifically: the fallback for the two above is "keep
            // collecting", and for this it is "do not start".
            arp_enabled: row
                .try_get::<bool, _>("arp_discovery_enabled")
                .unwrap_or(fallback.arp_enabled),
            arp_interval_secs: row
                .try_get::<i32, _>("arp_interval_secs")
                .ok()
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(fallback.arp_interval_secs),
            // Falls back to `true` mid-upgrade, before migration 0071 has run — the "keep
            // collecting" direction the two cheap walks use, not the ARP one's "do not start".
            routing_enabled: row
                .try_get::<bool, _>("routing_discovery_enabled")
                .unwrap_or(fallback.routing_enabled),
            routing_interval_secs: row
                .try_get::<i32, _>("routing_interval_secs")
                .ok()
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(fallback.routing_interval_secs),
            // Falls back to `true` mid-upgrade, before migration 0087 has run — the "keep
            // collecting" direction, like the three cheap walks above and not the ARP one.
            media_enabled: row
                .try_get::<bool, _>("media_discovery_enabled")
                .unwrap_or(fallback.media_enabled),
            media_interval_secs: row
                .try_get::<i32, _>("media_interval_secs")
                .ok()
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(fallback.media_interval_secs),
        }
    }

    /// Set the connectivity-discovery settings, upserting the singleton row. The API edge validates
    /// both cadences (`neighbors::interval_in_bounds`); the table CHECKs are the backstop.
    pub async fn set_adjacency_settings(&self, s: &AdjacencySettings) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO app_settings \
                 (id, neighbor_discovery_enabled, neighbor_interval_secs, \
                  l3_discovery_enabled, l3_interval_secs, \
                  arp_discovery_enabled, arp_interval_secs, \
                  routing_discovery_enabled, routing_interval_secs, \
                  media_discovery_enabled, media_interval_secs, updated_at) \
             VALUES (TRUE, $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now()) \
             ON CONFLICT (id) DO UPDATE SET neighbor_discovery_enabled = $1, \
                 neighbor_interval_secs = $2, l3_discovery_enabled = $3, \
                 l3_interval_secs = $4, arp_discovery_enabled = $5, \
                 arp_interval_secs = $6, routing_discovery_enabled = $7, \
                 routing_interval_secs = $8, media_discovery_enabled = $9, \
                 media_interval_secs = $10, updated_at = now()",
        )
        .bind(s.neighbors_enabled)
        .bind(i32::try_from(s.neighbors_interval_secs).unwrap_or(i32::MAX))
        .bind(s.l3_enabled)
        .bind(i32::try_from(s.l3_interval_secs).unwrap_or(i32::MAX))
        .bind(s.arp_enabled)
        .bind(i32::try_from(s.arp_interval_secs).unwrap_or(i32::MAX))
        .bind(s.routing_enabled)
        .bind(i32::try_from(s.routing_interval_secs).unwrap_or(i32::MAX))
        .bind(s.media_enabled)
        .bind(i32::try_from(s.media_interval_secs).unwrap_or(i32::MAX))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Which dependency graph the alert engine uses (ADR-043 決定 5).
    ///
    /// Degrades to [`TopologyMode::Manual`] on any read failure, and on any value it cannot parse.
    /// Both are the same decision: a database blip or a value written by a newer core must not be
    /// able to *turn on* derived suppression, because the failure mode of a wrong dependency graph
    /// is a real outage nobody is told about. Falling back to the mode that changes nothing is the
    /// only fallback that cannot silence anything.
    pub async fn get_topology_mode(&self) -> TopologyMode {
        let Ok(Some(row)) = sqlx::query("SELECT topology_mode FROM app_settings WHERE id = TRUE")
            .fetch_optional(&self.pool)
            .await
        else {
            return TopologyMode::Manual;
        };
        row.try_get::<String, _>("topology_mode")
            .map(|s| TopologyMode::from_stored(&s))
            .unwrap_or(TopologyMode::Manual)
    }

    /// Set the topology mode, upserting the singleton row. The API edge validates the token
    /// (`TopologyMode::from_token`) and checks the blocking preconditions; the column carries no
    /// `CHECK` on purpose (see migration 0067).
    pub async fn set_topology_mode(&self, mode: TopologyMode) -> anyhow::Result<()> {
        // `topology_mode_since` moves only when the mode actually changes. Stamping it on every
        // write would make "how long has this been in shadow" reset whenever an unrelated save
        // touched the row — and that number is the only evidence the promotion checklist has.
        sqlx::query(
            "INSERT INTO app_settings (id, topology_mode, topology_mode_since, updated_at) \
             VALUES (TRUE, $1, now(), now()) \
             ON CONFLICT (id) DO UPDATE SET \
                 topology_mode_since = CASE \
                     WHEN app_settings.topology_mode IS DISTINCT FROM $1 THEN now() \
                     ELSE app_settings.topology_mode_since END, \
                 topology_mode = $1, \
                 updated_at = now()",
        )
        .bind(mode.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// When the topology mode was last changed, or `None` if it never has been.
    pub async fn topology_mode_since(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        sqlx::query("SELECT topology_mode_since FROM app_settings WHERE id = TRUE")
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .and_then(|r| r.try_get("topology_mode_since").ok())
    }

    /// The global Cisco Meraki polling kill switch (safeguard). Defaults to `true` (enabled) if the
    /// row is somehow absent or on any read error, so a transient DB blip never silently pauses
    /// monitoring — the operator's explicit `false` is the only thing that halts polling.
    pub async fn get_meraki_polling_enabled(&self) -> bool {
        sqlx::query("SELECT meraki_polling_enabled FROM app_settings WHERE id = TRUE")
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .and_then(|r| r.try_get::<bool, _>("meraki_polling_enabled").ok())
            .unwrap_or(true)
    }

    /// Set the global Meraki polling kill switch, upserting the singleton row.
    pub async fn set_meraki_polling_enabled(&self, enabled: bool) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO app_settings (id, meraki_polling_enabled, updated_at) \
             VALUES (TRUE, $1, now()) \
             ON CONFLICT (id) DO UPDATE SET meraki_polling_enabled = $1, updated_at = now()",
        )
        .bind(enabled)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pgtest;

    /// Seeding twice writes the defaults once and never overwrites what an operator changed.
    ///
    /// Boot runs the seeder on every start, so "it does not undo yesterday's edit" is a property of
    /// every restart rather than of the first one.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn seeding_never_overwrites_a_value_an_operator_changed(pool: sqlx::PgPool) {
        let repo = pgtest::repo(pool.clone());
        repo.seed_app_settings(30, 7).await.expect("first seed");
        assert_eq!(repo.get_default_poll_interval().await.expect("read"), 30);
        assert_eq!(pgtest::rows(&pool, "app_settings").await, 1);

        repo.set_default_poll_interval(90).await.expect("operator");
        repo.seed_app_settings(30, 7).await.expect("second seed");
        assert_eq!(
            repo.get_default_poll_interval().await.expect("read"),
            90,
            "a restart reset the operator's poll interval to the default"
        );
        assert_eq!(pgtest::rows(&pool, "app_settings").await, 1);
    }

    /// Every retention window round-trips, including the one added after the first release.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_retention_windows_round_trip(pool: sqlx::PgPool) {
        let repo = pgtest::repo(pool);
        repo.seed_app_settings(30, 7).await.expect("seed");
        let want = crate::retention::RetentionSettings {
            alert_linked_days: 45,
            unmatched_event_hours: 12,
            report_run_days: 60,
            flow_days: 3,
            diagnostic_days: 21,
        };
        repo.set_retention_settings(&want).await.expect("write");
        assert_eq!(repo.get_retention_settings().await, want);
    }

    /// Every adjacency switch and interval round-trips — all ten of them.
    ///
    /// Ten fields written by one statement: a bind in the wrong position is not a compile error and
    /// would silently swap two switches, which is how an opt-in walk turns itself on.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn every_adjacency_switch_round_trips_in_its_own_column(pool: sqlx::PgPool) {
        let repo = pgtest::repo(pool);
        repo.seed_app_settings(30, 7).await.expect("seed");
        let want = crate::neighbors::AdjacencySettings {
            neighbors_enabled: true,
            // Distinct values inside the 300..=86400 each column is CHECKed into, so a bind in
            // the wrong position swaps two numbers this test can tell apart.
            neighbors_interval_secs: 600,
            l3_enabled: false,
            l3_interval_secs: 900,
            arp_enabled: true,
            arp_interval_secs: 1200,
            routing_enabled: false,
            routing_interval_secs: 1500,
            media_enabled: true,
            media_interval_secs: 1800,
        };
        repo.set_adjacency_settings(&want).await.expect("write");
        assert_eq!(repo.get_adjacency_settings().await, want);
    }

    /// The topology mode is stored, and its timestamp moves only when the mode actually changes.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_topology_mode_and_the_moment_it_changed(pool: sqlx::PgPool) {
        let repo = pgtest::repo(pool);
        repo.seed_app_settings(30, 7).await.expect("seed");
        assert_eq!(repo.get_topology_mode().await, TopologyMode::Manual);

        repo.set_topology_mode(TopologyMode::Shadow)
            .await
            .expect("write");
        assert_eq!(repo.get_topology_mode().await, TopologyMode::Shadow);
        let first = repo.topology_mode_since().await.expect("a timestamp");

        repo.set_topology_mode(TopologyMode::Derived)
            .await
            .expect("write");
        assert_eq!(repo.get_topology_mode().await, TopologyMode::Derived);
        assert!(
            repo.topology_mode_since().await.expect("a timestamp") >= first,
            "the mode changed but the moment it changed went backwards"
        );
    }

    /// The Meraki polling switch defaults on and can be turned off and back on.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_meraki_switch_moves_in_both_directions(pool: sqlx::PgPool) {
        let repo = pgtest::repo(pool);
        repo.seed_app_settings(30, 7).await.expect("seed");
        assert!(repo.get_meraki_polling_enabled().await);
        repo.set_meraki_polling_enabled(false).await.expect("off");
        assert!(!repo.get_meraki_polling_enabled().await);
        repo.set_meraki_polling_enabled(true).await.expect("on");
        assert!(repo.get_meraki_polling_enabled().await);
    }
}
