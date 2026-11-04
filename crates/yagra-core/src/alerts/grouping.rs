// SPDX-License-Identifier: AGPL-3.0-only
//! How strongly an outage's alerts are grouped: the deployment's level and each folder's own
//! (ADR-236, migration 0169). The grouping itself is `yagra_topology::grouping`; this is where the
//! operator's choice is kept.

use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_topology::grouping::Level;

/// A folder's own level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FolderLevel {
    pub group: Uuid,
    pub level: Level,
}

/// The stored levels.
pub(crate) struct GroupingRepo {
    pool: PgPool,
}

/// A stored level, read leniently: a number outside `0..=3` (the column's CHECK forbids one, so only
/// a hand edit or a newer core could write it) reads as [`Level::Off`], the level that changes
/// nothing, rather than letting an unreadable value decide how the fleet pages.
fn level_from(n: i16) -> Level {
    u8::try_from(n)
        .ok()
        .and_then(Level::from_u8)
        .unwrap_or(Level::Off)
}

impl GroupingRepo {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The deployment's level. [`Level::Off`] before the settings row exists.
    pub(crate) async fn level(&self) -> anyhow::Result<Level> {
        let row = sqlx::query("SELECT alert_grouping_level FROM app_settings WHERE id = TRUE")
            .fetch_optional(&self.pool)
            .await?;
        Ok(match row {
            Some(r) => level_from(r.try_get("alert_grouping_level")?),
            None => Level::Off,
        })
    }

    /// Set the deployment's level.
    pub(crate) async fn set_level(&self, level: Level) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO app_settings (id, alert_grouping_level) VALUES (TRUE, $1) \
             ON CONFLICT (id) DO UPDATE SET alert_grouping_level = $1",
        )
        .bind(i16::from(level.as_u8()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Whether a border's notification waits for a failing neighbour to commit (ADR-236 decision
    /// 12). `false` before the settings row exists.
    pub(crate) async fn wait(&self) -> anyhow::Result<bool> {
        let row = sqlx::query("SELECT alert_grouping_wait FROM app_settings WHERE id = TRUE")
            .fetch_optional(&self.pool)
            .await?;
        Ok(match row {
            Some(r) => r.try_get("alert_grouping_wait")?,
            None => false,
        })
    }

    /// Switch waiting on or off.
    pub(crate) async fn set_wait(&self, wait: bool) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO app_settings (id, alert_grouping_wait) VALUES (TRUE, $1) \
             ON CONFLICT (id) DO UPDATE SET alert_grouping_wait = $1",
        )
        .bind(wait)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Every folder's own level.
    pub(crate) async fn folder_levels(&self) -> anyhow::Result<Vec<FolderLevel>> {
        let rows = sqlx::query("SELECT group_id, level FROM alert_grouping_overrides")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|r| {
                Ok(FolderLevel {
                    group: r.try_get("group_id")?,
                    level: level_from(r.try_get("level")?),
                })
            })
            .collect()
    }

    /// Give a folder its own level. `false` when the folder does not exist.
    pub(crate) async fn set_folder_level(&self, group: Uuid, level: Level) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "INSERT INTO alert_grouping_overrides (group_id, level) VALUES ($1, $2) \
             ON CONFLICT (group_id) DO UPDATE SET level = $2",
        )
        .bind(group)
        .bind(i16::from(level.as_u8()))
        .execute(&self.pool)
        .await;
        match res {
            Ok(_) => Ok(true),
            Err(sqlx::Error::Database(db)) if db.is_foreign_key_violation() => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Take a folder's own level away. `false` when it had none.
    pub(crate) async fn clear_folder_level(&self, group: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM alert_grouping_overrides WHERE group_id = $1")
            .bind(group)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreadable_stored_level_changes_nothing() {
        assert_eq!(level_from(2), Level::Links);
        assert_eq!(level_from(9), Level::Off);
        assert_eq!(level_from(-1), Level::Off);
    }

    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn levels_round_trip_and_a_folder_takes_its_level_with_it(pool: PgPool) {
        let repo = GroupingRepo::new(pool.clone());
        assert_eq!(repo.level().await.expect("level"), Level::Off);
        repo.set_level(Level::Certain).await.expect("set");
        assert_eq!(repo.level().await.expect("level"), Level::Certain);
        assert!(!repo.wait().await.expect("wait"));
        repo.set_wait(true).await.expect("set wait");
        assert!(repo.wait().await.expect("wait"));
        assert_eq!(
            repo.level().await.expect("level"),
            Level::Certain,
            "the switch leaves the level alone"
        );

        let site = yagra_base::pgtest::group(&pool, "Site A").await;
        assert!(repo.set_folder_level(site, Level::Site).await.expect("set"));
        assert!(!repo
            .set_folder_level(Uuid::new_v4(), Level::Site)
            .await
            .expect("set"));
        assert_eq!(
            repo.folder_levels().await.expect("list"),
            vec![FolderLevel {
                group: site,
                level: Level::Site
            }]
        );
        sqlx::query("DELETE FROM node_groups WHERE id = $1")
            .bind(site)
            .execute(&pool)
            .await
            .expect("delete folder");
        assert!(repo.folder_levels().await.expect("list").is_empty());
        assert!(!repo.clear_folder_level(site).await.expect("clear"));
    }
}
