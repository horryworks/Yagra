// SPDX-License-Identifier: AGPL-3.0-only
//! Per-account pins on the inventory tree (ADR-146).
//!
//! A pin is one person saying "I look at this node, or this folder, often". The tree's "Pinned only"
//! switch then shows what is pinned and the folders above it.
//!
//! **A table rather than a key in `user_preferences`, on purpose.** ADR-058's document is opaque to
//! the backend, and its own rule is that anything the backend must read does not belong in it. Pins
//! are read here: a pinned node usually lives in a folder the tree has not loaded, so the API answers
//! with the pinned nodes themselves. The foreign keys are the second reason — deleting a node, a
//! folder or an account removes its pins, where an id inside a blob would outlive the node.
//!
//! Scoped to the caller by `username` through a `users` subquery, the shape `preferences.rs` uses,
//! so an account can only ever read or write its own rows. **Visibility (ADR-014) is not decided
//! here**: the handlers check a target before pinning it and filter what they return.

use sqlx::{PgPool, Row};
use uuid::Uuid;

/// How many pins one account may hold, nodes and folders together.
///
/// It bounds the read — `GET /api/v1/pins` builds a full inventory row for every pinned node — and
/// nothing else. ⚠️ Checked inside the insert rather than under a lock, so two pins landing in the
/// same instant at 499 can both pass and leave 501. Accepted: the cap bounds a read, it is not a
/// security property.
pub const PINS_MAX: i64 = 500;

/// What a pin names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinTarget {
    Node(Uuid),
    Group(Uuid),
}

/// What a pin request did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinOutcome {
    /// Pinned now, or it already was — repeating a pin is not an error.
    Pinned,
    /// The account already holds the maximum number of pins.
    AtLimit,
    /// The node or folder does not exist (deleted since the tree was drawn).
    NoTarget,
    /// The session's account no longer exists.
    NoUser,
}

/// One account's pins, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserPins {
    pub nodes: Vec<Uuid>,
    pub groups: Vec<Uuid>,
}

/// Insert a node pin, unless the account is at `$3` pins. Zero rows affected means "not inserted",
/// and [`NODE_PIN_STATUS`] says why.
const PIN_NODE: &str = "INSERT INTO user_pins (user_id, node_id) \
     SELECT u.id, n.id FROM users u, nodes n \
      WHERE u.username = $1 AND n.id = $2 \
        AND (SELECT count(*) FROM user_pins p WHERE p.user_id = u.id) < $3 \
     ON CONFLICT DO NOTHING";

const PIN_GROUP: &str = "INSERT INTO user_pins (user_id, group_id) \
     SELECT u.id, g.id FROM users u, node_groups g \
      WHERE u.username = $1 AND g.id = $2 \
        AND (SELECT count(*) FROM user_pins p WHERE p.user_id = u.id) < $3 \
     ON CONFLICT DO NOTHING";

/// Why a node pin inserted nothing: it was already there, the account is gone, the node is gone —
/// or, when all three are no, the cap.
const NODE_PIN_STATUS: &str = "SELECT \
     EXISTS (SELECT 1 FROM user_pins p JOIN users u ON u.id = p.user_id \
              WHERE u.username = $1 AND p.node_id = $2) AS already, \
     EXISTS (SELECT 1 FROM users WHERE username = $1) AS has_user, \
     EXISTS (SELECT 1 FROM nodes WHERE id = $2) AS has_target";

const GROUP_PIN_STATUS: &str = "SELECT \
     EXISTS (SELECT 1 FROM user_pins p JOIN users u ON u.id = p.user_id \
              WHERE u.username = $1 AND p.group_id = $2) AS already, \
     EXISTS (SELECT 1 FROM users WHERE username = $1) AS has_user, \
     EXISTS (SELECT 1 FROM node_groups WHERE id = $2) AS has_target";

const UNPIN_NODE: &str = "DELETE FROM user_pins \
     WHERE user_id = (SELECT id FROM users WHERE username = $1) AND node_id = $2";

const UNPIN_GROUP: &str = "DELETE FROM user_pins \
     WHERE user_id = (SELECT id FROM users WHERE username = $1) AND group_id = $2";

/// PostgreSQL-backed per-account pins (`user_pins`).
pub struct UserPinsRepo {
    pool: PgPool,
}

impl UserPinsRepo {
    /// New store over the metadata pool.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The pins `username` holds, oldest first. An unknown username simply has none.
    pub async fn list_for_user(&self, username: &str) -> anyhow::Result<UserPins> {
        let rows = sqlx::query(
            "SELECT node_id, group_id FROM user_pins \
             WHERE user_id = (SELECT id FROM users WHERE username = $1) \
             ORDER BY created_at, node_id, group_id",
        )
        .bind(username)
        .fetch_all(&self.pool)
        .await?;
        let mut pins = UserPins::default();
        for row in rows {
            let node: Option<Uuid> = row.try_get("node_id")?;
            let group: Option<Uuid> = row.try_get("group_id")?;
            // The CHECK guarantees exactly one of the two is set.
            if let Some(n) = node {
                pins.nodes.push(n);
            } else if let Some(g) = group {
                pins.groups.push(g);
            }
        }
        Ok(pins)
    }

    /// Pin `target` for `username`, holding at most `max` pins.
    ///
    /// `max` is a parameter rather than [`PINS_MAX`] read inside, so a test can reach the cap
    /// without creating five hundred rows.
    pub async fn pin(
        &self,
        username: &str,
        target: PinTarget,
        max: i64,
    ) -> anyhow::Result<PinOutcome> {
        let (insert, status, id) = match target {
            PinTarget::Node(id) => (PIN_NODE, NODE_PIN_STATUS, id),
            PinTarget::Group(id) => (PIN_GROUP, GROUP_PIN_STATUS, id),
        };
        let res = sqlx::query(insert)
            .bind(username)
            .bind(id)
            .bind(max)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() > 0 {
            return Ok(PinOutcome::Pinned);
        }
        let row = sqlx::query(status)
            .bind(username)
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        let already: bool = row.try_get("already")?;
        let has_user: bool = row.try_get("has_user")?;
        let has_target: bool = row.try_get("has_target")?;
        Ok(if already {
            PinOutcome::Pinned
        } else if !has_user {
            PinOutcome::NoUser
        } else if !has_target {
            PinOutcome::NoTarget
        } else {
            PinOutcome::AtLimit
        })
    }

    /// Remove `username`'s pin on `target`. Removing a pin that is not there is not an error.
    pub async fn unpin(&self, username: &str, target: PinTarget) -> anyhow::Result<()> {
        let (sql, id) = match target {
            PinTarget::Node(id) => (UNPIN_NODE, id),
            PinTarget::Group(id) => (UNPIN_GROUP, id),
        };
        sqlx::query(sql)
            .bind(username)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This module's code, comments stripped — see
    /// [`crate::module_source::code_no_comments`] for why both.
    fn production_source() -> String {
        crate::module_source::code_no_comments("src", "pins")
    }

    #[test]
    fn every_statement_binds_its_values_instead_of_interpolating_them() {
        // Ids and usernames reach SQL only as binds. The statements are constants, so a `format!`
        // appearing here is a statement being assembled from something.
        let src = production_source();
        for builder in ["format!(", "push_str("] {
            assert!(
                !src.contains(builder),
                "SQL may be being built by string concatenation ({builder}); bind the value instead"
            );
        }
    }

    #[test]
    fn every_statement_reaches_only_the_callers_own_rows() {
        // All seven statements name the account by username — the two status reads twice, once for
        // the pin and once for whether the account exists — so no call can reach another person's
        // pins. A statement losing its account predicate lowers the count.
        let src = production_source();
        assert_eq!(src.matches("username = $1").count(), 9);
    }

    // ── Against a real database (ADR-114) ────────────────────────────────────────────────

    async fn account(pool: &sqlx::PgPool, name: &str) {
        let created = crate::auth::UserStore::new(pool.clone())
            .create(
                name,
                "correct horse battery staple",
                yagra_common::Role::Viewer.key(),
            )
            .await
            .expect("create user");
        assert!(matches!(
            created,
            crate::auth::UserCreateOutcome::Created(_)
        ));
    }

    /// The cap counts nodes and folders together, a repeated pin is not a second pin, and a
    /// pin on something that does not exist says so rather than looking like the cap.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_cap_counts_nodes_and_folders_together(pool: sqlx::PgPool) {
        let repo = UserPinsRepo::new(pool.clone());
        account(&pool, "pinner").await;
        let site = crate::pgtest::group(&pool, "site").await;
        let other = crate::pgtest::group(&pool, "other").await;
        let node = crate::pgtest::node(&pool, "core-1", 1, Some(site)).await;

        assert_eq!(
            repo.pin("pinner", PinTarget::Group(site), 2).await.unwrap(),
            PinOutcome::Pinned
        );
        assert_eq!(
            repo.pin("pinner", PinTarget::Node(node), 2).await.unwrap(),
            PinOutcome::Pinned
        );
        // At the cap: a new pin is refused, a repeated one is still fine.
        assert_eq!(
            repo.pin("pinner", PinTarget::Group(other), 2)
                .await
                .unwrap(),
            PinOutcome::AtLimit
        );
        assert_eq!(
            repo.pin("pinner", PinTarget::Node(node), 2).await.unwrap(),
            PinOutcome::Pinned
        );
        assert_eq!(crate::pgtest::rows(&pool, "user_pins").await, 2);

        // Missing things are named as missing, at the cap or not.
        assert_eq!(
            repo.pin("pinner", PinTarget::Node(Uuid::new_v4()), 9)
                .await
                .unwrap(),
            PinOutcome::NoTarget
        );
        assert_eq!(
            repo.pin("nobody", PinTarget::Group(other), 9)
                .await
                .unwrap(),
            PinOutcome::NoUser
        );

        repo.unpin("pinner", PinTarget::Group(site)).await.unwrap();
        // Removing it twice is not an error either.
        repo.unpin("pinner", PinTarget::Group(site)).await.unwrap();
        assert_eq!(
            repo.pin("pinner", PinTarget::Group(other), 2)
                .await
                .unwrap(),
            PinOutcome::Pinned
        );
        let pins = repo.list_for_user("pinner").await.unwrap();
        assert_eq!(pins.nodes, vec![node]);
        assert_eq!(pins.groups, vec![other]);
    }

    /// Two accounts pinning the same node hold two pins, and neither sees the other's.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn pins_belong_to_one_account(pool: sqlx::PgPool) {
        let repo = UserPinsRepo::new(pool.clone());
        account(&pool, "alice").await;
        account(&pool, "bob").await;
        let node = crate::pgtest::node(&pool, "core-1", 1, None).await;
        let site = crate::pgtest::group(&pool, "site").await;

        repo.pin("alice", PinTarget::Node(node), PINS_MAX)
            .await
            .unwrap();
        repo.pin("bob", PinTarget::Node(node), PINS_MAX)
            .await
            .unwrap();
        repo.pin("bob", PinTarget::Group(site), PINS_MAX)
            .await
            .unwrap();
        repo.unpin("alice", PinTarget::Node(node)).await.unwrap();

        assert_eq!(
            repo.list_for_user("alice").await.unwrap(),
            UserPins::default()
        );
        let bob = repo.list_for_user("bob").await.unwrap();
        assert_eq!((bob.nodes, bob.groups), (vec![node], vec![site]));
    }
}
