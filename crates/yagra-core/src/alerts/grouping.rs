// SPDX-License-Identifier: AGPL-3.0-only
//! How strongly an outage's alerts are grouped: the deployment's level and its exceptions
//! (ADR-236, migration 0169; exceptions by folder and tag since ADR-262, migration 0185). The grouping itself is `yagra_topology::grouping`; this is where the
//! operator's choice is kept.

use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_topology::grouping::Level;

/// One exception to the deployment's level (ADR-262 decision 2, the former per-folder level): the
/// nodes inside one of `group_ids` and/or carrying one or all of `tags` (`tag_match`, ADR-267;
/// `join`, decision 10) group at `level` instead. Never names neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GroupingException {
    pub id: Uuid,
    pub group_ids: Vec<Uuid>,
    pub tags: Vec<String>,
    pub join: crate::alerts::applies_to::Join,
    pub tag_match: crate::alerts::applies_to::TagMatch,
    pub level: Level,
}

/// What an exception is written with, by the create and the edit alike.
#[derive(Debug, Clone)]
pub(crate) struct ExceptionDraft {
    pub group_ids: Vec<Uuid>,
    pub tags: Vec<String>,
    pub join: crate::alerts::applies_to::Join,
    /// `None` keeps what an edited exception has (an older client must not widen an "all" one,
    /// ADR-267); a new exception takes `any`.
    pub tag_match: Option<crate::alerts::applies_to::TagMatch>,
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

    /// Every exception to the deployment's level (ADR-262 decision 2), oldest first.
    pub(crate) async fn exceptions(&self) -> anyhow::Result<Vec<GroupingException>> {
        let rows = sqlx::query(
            "SELECT id, group_ids, tags, join_and, tags_all, level FROM alert_grouping_exceptions \
             ORDER BY created_at, id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                Ok(GroupingException {
                    id: r.try_get("id")?,
                    group_ids: r.try_get("group_ids")?,
                    tags: r.try_get("tags")?,
                    join: crate::alerts::applies_to::Join::from_and(r.try_get("join_and")?),
                    tag_match: crate::alerts::applies_to::TagMatch::from_all(
                        r.try_get("tags_all")?,
                    ),
                    level: level_from(r.try_get("level")?),
                })
            })
            .collect()
    }

    /// Add an exception. The caller has checked it names at least one folder or tag (the table's
    /// CHECK refuses one that names neither).
    pub(crate) async fn create_exception(&self, draft: &ExceptionDraft) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO alert_grouping_exceptions (id, group_ids, tags, join_and, level, tags_all) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(id)
        .bind(&draft.group_ids)
        .bind(&draft.tags)
        .bind(draft.join.is_and())
        .bind(i16::from(draft.level.as_u8()))
        .bind(draft.tag_match.unwrap_or_default().is_all())
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Replace what an exception applies to and its level. `false` when there is no such exception.
    pub(crate) async fn update_exception(
        &self,
        id: Uuid,
        draft: &ExceptionDraft,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE alert_grouping_exceptions SET group_ids = $2, tags = $3, level = $4, join_and = $5, \
             tags_all = COALESCE($6, tags_all) WHERE id = $1",
        )
        .bind(id)
        .bind(&draft.group_ids)
        .bind(&draft.tags)
        .bind(i16::from(draft.level.as_u8()))
        .bind(draft.join.is_and())
        .bind(draft.tag_match.map(crate::alerts::applies_to::TagMatch::is_all))
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Remove an exception. `false` when there was none.
    pub(crate) async fn delete_exception(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM alert_grouping_exceptions WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

/// The level that holds for one node (ADR-262 decision 2): the **weakest** of the exceptions that
/// reach it, else `None` (the deployment's level holds). An exception reaches the node by its
/// folders (`folder_chain` is the node's own folder and every folder above it) and/or its tags
/// (one or all of them, by `tag_match`), joined by the exception's `join`
/// ([`crate::alerts::applies_to::reaches_chain`]). Weakest, not nearest: a grouping too strong hides a real outage, and
/// one too weak only sends more notifications.
#[must_use]
pub(crate) fn weakest_level(
    exceptions: &[GroupingException],
    folder_chain: &[Uuid],
    tags: &[String],
) -> Option<Level> {
    exceptions
        .iter()
        .filter(|e| {
            crate::alerts::applies_to::reaches_chain(
                &e.group_ids,
                &e.tags,
                e.join,
                e.tag_match,
                folder_chain,
                tags,
            )
        })
        .map(|e| e.level)
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exception(group_ids: &[Uuid], tags: &[&str], level: Level) -> GroupingException {
        GroupingException {
            id: Uuid::new_v4(),
            group_ids: group_ids.to_vec(),
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            join: crate::alerts::applies_to::Join::Or,
            tag_match: crate::alerts::applies_to::TagMatch::Any,
            level,
        }
    }

    #[test]
    fn the_weakest_exception_reaching_a_node_wins() {
        let parent = Uuid::new_v4();
        let child = Uuid::new_v4();
        let other = Uuid::new_v4();
        let list = [
            exception(&[parent], &[], Level::Site),
            exception(&[child], &[], Level::Certain),
            exception(&[], &["no-lldp"], Level::Off),
            exception(&[other], &[], Level::Off),
        ];
        // Nested folders: the parent's Site and the child's Certain both reach - the weaker wins,
        // not the nearer (ADR-236 decision 4, overruled by ADR-262 decision 2).
        assert_eq!(
            weakest_level(&list, &[child, parent], &[]),
            Some(Level::Certain)
        );
        // A tag reaches too, and Off beats every folder.
        assert_eq!(
            weakest_level(&list, &[child, parent], &["no-lldp".to_owned()]),
            Some(Level::Off)
        );
        // A folder alone, and nothing at all.
        assert_eq!(weakest_level(&list, &[parent], &[]), Some(Level::Site));
        assert_eq!(weakest_level(&list, &[], &["core".to_owned()]), None);
    }

    #[test]
    fn an_unreadable_stored_level_changes_nothing() {
        assert_eq!(level_from(2), Level::Links);
        assert_eq!(level_from(9), Level::Off);
        assert_eq!(level_from(-1), Level::Off);
    }

    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn levels_and_exceptions_round_trip(pool: PgPool) {
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

        use crate::alerts::applies_to::{Join, TagMatch};
        let draft = |group_ids: Vec<Uuid>, tags: &[&str], tag_match, level| ExceptionDraft {
            group_ids,
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            join: Join::Or,
            tag_match,
            level,
        };
        let site = yagra_base::pgtest::group(&pool, "Site A").await;
        let id = repo
            .create_exception(&draft(vec![site], &["core"], None, Level::Site))
            .await
            .expect("create");
        assert_eq!(
            repo.exceptions().await.expect("list"),
            vec![GroupingException {
                id,
                group_ids: vec![site],
                tags: vec!["core".to_owned()],
                join: Join::Or,
                tag_match: TagMatch::Any,
                level: Level::Site
            }],
            "a create that names no tag_match takes any"
        );
        assert!(repo
            .update_exception(
                id,
                &draft(
                    Vec::new(),
                    &["edge", "24x7"],
                    Some(TagMatch::All),
                    Level::Off
                )
            )
            .await
            .expect("update"));
        let read = &repo.exceptions().await.expect("list")[0];
        assert_eq!((read.level, read.tag_match), (Level::Off, TagMatch::All));
        // ADR-267: an edit that omits tag_match keeps the exception's own.
        assert!(repo
            .update_exception(id, &draft(Vec::new(), &["edge", "24x7"], None, Level::Off))
            .await
            .expect("update"));
        assert_eq!(
            repo.exceptions().await.expect("list")[0].tag_match,
            TagMatch::All
        );
        assert!(!repo
            .update_exception(Uuid::new_v4(), &draft(vec![site], &[], None, Level::Off))
            .await
            .expect("update"));
        assert!(
            repo.create_exception(&draft(Vec::new(), &[], None, Level::Off))
                .await
                .is_err(),
            "an exception naming nothing is refused by the table"
        );
        assert!(repo.delete_exception(id).await.expect("delete"));
        assert!(!repo.delete_exception(id).await.expect("delete"));
    }
}
