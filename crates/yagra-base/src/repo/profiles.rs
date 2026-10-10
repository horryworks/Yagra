// SPDX-License-Identifier: AGPL-3.0-only
//! The `profiles` table: device-class rows and the per-profile polling-interval override.
//!
//! Only the operator-facing CRUD lives here. The built-in profiles themselves are seeded by
//! [`super::seed`], which writes this table among seven others because bootstrapping is by
//! definition not about one table.

use std::collections::HashMap;

use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

// Only the settings struct: `retention::Row` would collide with `sqlx::Row` above.

use super::*;

/// A device-class/profile row for the API (id + name + role/vendor metadata).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ProfileSummary {
    pub id: Uuid,
    pub name: String,
    /// Functional role token (kebab-case `ProfileCategory`) — the UI's grouping key.
    pub category: String,
    /// Vendor label, if known (descriptive metadata only — never a TSDB label).
    pub vendor: Option<String>,
    /// Per-profile polling-interval override (seconds); `None` ⇒ inherit the global default.
    pub poll_interval_secs: Option<i32>,
}

impl NodeRepo {
    /// All device-class profiles.
    pub async fn list_profiles(&self) -> anyhow::Result<Vec<ProfileSummary>> {
        let rows = sqlx::query(
            "SELECT id, name, category, vendor, poll_interval_secs FROM profiles ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(ProfileSummary {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    category: row.try_get("category")?,
                    vendor: row.try_get("vendor")?,
                    poll_interval_secs: row.try_get("poll_interval_secs")?,
                })
            })
            .collect()
    }

    /// The id of one profile with the given `ProfileCategory` token (e.g. `url-check`), if any.
    /// Used to bind a freshly created URL monitor to the built-in URL/HTTP profile so it inherits
    /// the default thresholds. Lowest name wins for determinism if several share the category.
    pub async fn profile_id_for_category(&self, category: &str) -> anyhow::Result<Option<Uuid>> {
        let row =
            sqlx::query("SELECT id FROM profiles WHERE category = $1 ORDER BY name, id LIMIT 1")
                .bind(category)
                .fetch_optional(&self.pool)
                .await?;
        row.map(|r| Ok(r.try_get("id")?)).transpose()
    }

    /// The id of the profile with the given exact name (for binding imported Meraki devices to their
    /// specific built-in API profile), if one exists.
    pub async fn profile_id_for_name(&self, name: &str) -> anyhow::Result<Option<Uuid>> {
        let row = sqlx::query("SELECT id FROM profiles WHERE name = $1 LIMIT 1")
            .bind(name)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| Ok(r.try_get("id")?)).transpose()
    }

    /// Create a profile; returns its id. `poll_interval_secs` is the optional per-profile interval
    /// override (`None` ⇒ inherit the global default).
    pub async fn create_profile(
        &self,
        name: &str,
        category: &str,
        vendor: Option<&str>,
        poll_interval_secs: Option<i32>,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO profiles (id, name, category, vendor, poll_interval_secs) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(name)
        .bind(category)
        .bind(vendor)
        .bind(poll_interval_secs)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Update a profile's name / category / vendor / interval override. Returns whether the row
    /// existed. A `None` `poll_interval_secs` clears the override (back to the global default).
    pub async fn update_profile(
        &self,
        id: Uuid,
        name: &str,
        category: &str,
        vendor: Option<&str>,
        poll_interval_secs: Option<i32>,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE profiles SET name = $2, category = $3, vendor = $4, poll_interval_secs = $5, \
             updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(category)
        .bind(vendor)
        .bind(poll_interval_secs)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Per-profile interval overrides (only profiles that set one), keyed by profile id. The
    /// scheduler resolves each node against this map, falling back to the global default.
    pub async fn profile_interval_overrides(&self) -> anyhow::Result<HashMap<Uuid, u32>> {
        let rows = sqlx::query(
            "SELECT id, poll_interval_secs FROM profiles WHERE poll_interval_secs IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut map = HashMap::new();
        for row in rows {
            let id: Uuid = row.try_get("id")?;
            let secs: i32 = row.try_get("poll_interval_secs")?;
            if let Ok(secs) = u32::try_from(secs) {
                map.insert(id, secs);
            }
        }
        Ok(map)
    }

    /// Delete a profile. Returns whether a row was removed.
    pub async fn delete_profile(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM profiles WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use crate::pgtest;

    /// A profile is created, found by both lookups, edited, and deleted once.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_profile_round_trips_through_both_lookups(pool: sqlx::PgPool) {
        let repo = pgtest::repo(pool);
        let id = repo
            .create_profile("my switch", "switch", Some("Acme"), Some(120))
            .await
            .expect("create");

        assert_eq!(
            repo.profile_id_for_name("my switch")
                .await
                .expect("by name"),
            Some(id)
        );
        assert!(repo
            .profile_id_for_name("no such profile")
            .await
            .expect("by name")
            .is_none());
        assert_eq!(
            repo.profile_id_for_category("switch")
                .await
                .expect("by category"),
            Some(id),
            "the category lookup did not find the profile just created"
        );

        assert!(repo
            .update_profile(id, "my other switch", "switch", None, None)
            .await
            .expect("update"));
        let listed = repo.list_profiles().await.expect("list");
        let mine = listed.iter().find(|p| p.id == id).expect("the profile");
        assert_eq!(mine.name, "my other switch");
        assert_eq!(mine.vendor, None);
        assert_eq!(mine.poll_interval_secs, None);

        assert!(repo.delete_profile(id).await.expect("delete"));
        assert!(
            !repo.delete_profile(id).await.expect("delete"),
            "a second delete claimed to have removed the same profile"
        );
        assert!(repo.get_node(id).await.expect("read").is_none());
    }

    /// Only a profile that overrides the interval is in the override map.
    ///
    /// The scheduler reads this map per sweep; a profile that appears in it with the *global*
    /// default would silently pin every node bound to it.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn only_a_profile_with_its_own_interval_is_an_override(pool: sqlx::PgPool) {
        let repo = pgtest::repo(pool);
        let fast = repo
            .create_profile("fast", "switch", None, Some(15))
            .await
            .expect("create");
        let inherits = repo
            .create_profile("inherits", "router", None, None)
            .await
            .expect("create");

        let overrides = repo.profile_interval_overrides().await.expect("overrides");
        assert_eq!(overrides.get(&fast).copied(), Some(15));
        assert!(
            !overrides.contains_key(&inherits),
            "a profile with no override of its own is in the override map"
        );
    }
}
