// SPDX-License-Identifier: AGPL-3.0-only
//! The `pools` table: poller pools that have been named deliberately (ADR-107).
//!
//! ⚠️ **This file holds the pool's *description*, and the two writes that move membership between
//! pools — never membership itself.** Which nodes are in a pool is `nodes.pool` /
//! `node_groups.pool` (`nodes.rs`, `groups.rs`) and which pool a poller serves is
//! `pollers.pool` (`crate::pollers`). A method here that answered "who is in this pool" would be
//! a second answer to a question those files already answer, which is precisely the shape ADR-094
//! cut this module up to prevent.
//!
//! [`NodeRepo::rename_pool`] and [`NodeRepo::move_poller_to_pool`] are here anyway, and for one
//! reason: each has to touch three tables **atomically**, and an atomic write has to live in one
//! place. Splitting either across the owning files would mean three transactions that can half
//! commit, and the half-committed state of both is the same one — monitored inventory in a pool
//! with no poller, which is a silent monitoring hole.
//!
//! The one apparent exception is [`NodeRepo::pool_references`], which counts rows in three tables it
//! does not own. It is a *refusal*: delete has to say what is still using the name, and a count that
//! lived anywhere else would be a count the delete path could forget to consult. `guards.rs`
//! declares those three tables against this file for that reason.

use serde::Serialize;
use sqlx::Row;

use super::*;

/// A pool as this deployment has described it. Membership is deliberately absent — see the module
/// doc; the counts the strip renders come from `pool_coverage.rs`, which asks the stores that
/// actually hold them.
#[derive(Debug, Clone, Serialize)]
pub struct PoolRow {
    pub name: String,
    pub description: Option<String>,
}

/// What is still pointing at a pool, so a refused delete can name it (ADR-107 決定 6).
///
/// Pollers are named rather than counted: "1 台" tells an operator nothing actionable, and the
/// whole point of the refusal is to say which box to go and change.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PoolReferences {
    pub nodes: i64,
    pub folders: i64,
    /// Poller ids reporting this pool, or recorded as heading to it. Bounded by the poller
    /// inventory, which is deployment topology rather than fleet-scaled.
    pub pollers: Vec<String>,
}

impl PoolReferences {
    /// Nothing points at this pool, so removing the row removes the pool.
    pub fn is_empty(&self) -> bool {
        self.nodes == 0 && self.folders == 0 && self.pollers.is_empty()
    }
}

/// What a pool move carries with it, when the operator answered that the inventory travels
/// (ADR-107 増分 3).
///
/// The two fields are not redundant. `from` re-points every row that *names* the pool — nodes and
/// folders alike — in one statement each. `fall_through` carries the ids that name nothing and
/// resolve to the pool only by inheritance bottoming out; there is no predicate over the `nodes`
/// table that finds them without re-implementing [`crate::poolres::PoolResolver`], so the resolver
/// finds them and hands them over.
#[derive(Debug, Clone, Copy)]
pub struct PoolCarry<'a> {
    /// The pool being emptied. Its named rows move to the destination.
    pub from: &'a str,
    /// Nodes to pin explicitly. Empty unless `from` is [`yagra_bus::DEFAULT_POOL`], which is the
    /// only pool inheritance can fall through to.
    pub fall_through: &'a [Uuid],
}
impl NodeRepo {
    /// Every described pool, name-ordered.
    ///
    /// ⚠️ **Not "every pool that exists".** A pool is in use the moment something names it, whether
    /// or not it has a row here, so the picker unions this with the three sources that predate the
    /// table (`api/pools.rs::build_pool_options`). Reading this alone would silently drop the pools
    /// an N-1 core created — the failure ADR-068 exists to prevent.
    pub async fn list_pools(&self) -> anyhow::Result<Vec<PoolRow>> {
        let rows = sqlx::query("SELECT name, description FROM pools ORDER BY name")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok(PoolRow {
                    name: row.try_get("name")?,
                    description: row.try_get("description")?,
                })
            })
            .collect()
    }

    /// Describe a pool. `Ok(false)` ⇒ the name was already described.
    pub async fn create_pool(
        &self,
        name: &str,
        description: Option<&str>,
        created_by: Option<&str>,
    ) -> anyhow::Result<bool> {
        let done = sqlx::query(
            "INSERT INTO pools (name, description, created_by) VALUES ($1, $2, $3) \
             ON CONFLICT (name) DO NOTHING",
        )
        .bind(name)
        .bind(description)
        .bind(created_by)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Replace a pool's description. `Ok(false)` ⇒ no such row.
    ///
    /// Creates nothing: a description for a pool nobody described is `create_pool`'s job, and
    /// silently upserting here would make "edit" able to conjure a pool the operator never made.
    pub async fn set_pool_description(
        &self,
        name: &str,
        description: Option<&str>,
    ) -> anyhow::Result<bool> {
        let done = sqlx::query("UPDATE pools SET description = $2 WHERE name = $1")
            .bind(name)
            .bind(description)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Rename a pool, moving every node, folder **and poller** assignment with it, in one
    /// transaction.
    ///
    /// 🚨 **The pollers used to be left behind, and the caller had to refuse the rename because of
    /// it.** Before ADR-107 Inc.2 a poller's pool came from `YAGRA_POLLER_POOL` at its own site and
    /// this transaction could not reach it, so renaming out from under one left the old name in
    /// `live_pools` while the new name's nodes dropped into legacy fan-out, published to a subject
    /// nobody subscribed to and **discarded by plain NATS** — a monitoring hole opened by a button,
    /// visible only after `pool_coverage`'s 300s debounce. Core owns `pollers.pool` now, so they
    /// come along. What the caller must still check is whether each of them *can* follow a change
    /// (`api/pools.rs`); an offline or older poller would reproduce exactly that hole.
    ///
    /// `Ok(false)` ⇒ no such row. A name collision surfaces as the primary key's error.
    pub async fn rename_pool(&self, from: &str, to: &str) -> anyhow::Result<bool> {
        let mut tx = self.pool.begin().await?;
        let done = sqlx::query("UPDATE pools SET name = $2 WHERE name = $1")
            .bind(from)
            .bind(to)
            .execute(&mut *tx)
            .await?;
        if done.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query("UPDATE nodes SET pool = $2 WHERE pool = $1")
            .bind(from)
            .bind(to)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE node_groups SET pool = $2 WHERE pool = $1")
            .bind(from)
            .bind(to)
            .execute(&mut *tx)
            .await?;
        // The pollers come with it (ADR-107 Inc.2). Before core owned `pollers.pool` this was
        // impossible — a poller's pool lived in its own `.env` — so a rename had to be **refused**
        // while anything reported the old name, or the renamed nodes would drop into legacy
        // fan-out on a subject nobody subscribed to. Now the rename is just a move, and the
        // caller's remaining job is to check that each of them *can* follow one.
        sqlx::query("UPDATE pollers SET pool = $2 WHERE pool = $1")
            .bind(from)
            .bind(to)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Remove a pool's row. `Ok(false)` ⇒ no such row.
    ///
    /// Removes the *description*, never an assignment: the caller checks [`Self::pool_references`]
    /// first and refuses while anything points at the name. Deleting the row of a pool still in use
    /// would not free the name — it is derived from the three other sources — so the delete would
    /// appear to succeed and change nothing.
    pub async fn delete_pool(&self, name: &str) -> anyhow::Result<bool> {
        let done = sqlx::query("DELETE FROM pools WHERE name = $1")
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    /// What still points at `name`, for the delete/rename refusals.
    pub async fn pool_references(&self, name: &str) -> anyhow::Result<PoolReferences> {
        let nodes: i64 = sqlx::query_scalar("SELECT count(*) FROM nodes WHERE pool = $1")
            .bind(name)
            .fetch_one(&self.pool)
            .await?;
        let folders: i64 = sqlx::query_scalar("SELECT count(*) FROM node_groups WHERE pool = $1")
            .bind(name)
            .fetch_one(&self.pool)
            .await?;
        // One direction now: `pollers.pool` **is** where the poller belongs (ADR-107 Inc.2), so
        // there is no separate "heading there" state left to consider. Before Inc.2 a recorded
        // destination was a second, pending answer and both had to block a delete.
        let pollers: Vec<String> =
            sqlx::query_scalar("SELECT id FROM pollers WHERE pool = $1 ORDER BY id")
                .bind(name)
                .fetch_all(&self.pool)
                .await?;
        Ok(PoolReferences {
            nodes,
            folders,
            pollers,
        })
    }

    /// Move one poller into `to`, optionally bringing `from`'s monitored inventory with it
    /// (ADR-107 Inc.2).
    ///
    /// 🚨 **One transaction, and that is the safety property.** The two halves fail differently:
    /// a poller moved without its nodes leaves `from` with monitored inventory and no poller, which
    /// is a silent monitoring hole `pool_coverage` only reports after a 300s debounce; nodes moved
    /// without their poller is the same hole pointing the other way. Committing one and not the
    /// other is the outcome nobody would choose, so it is not reachable.
    ///
    /// `take` is `Some` when the caller has decided the inventory travels. The source pool is
    /// passed rather than derived so this method cannot disagree with the pool the caller measured
    /// and warned about — between the check and the write, the poller's row is the only thing that
    /// has changed, and it is changed here.
    ///
    /// 🚨 **Three writes, because a node can be in a pool three ways and only two of them are a
    /// column this can rewrite** (ADR-107 増分 3). A node names the pool itself; or an ancestor
    /// folder does; or **nothing anywhere does and it falls through to the implicit default**. The
    /// first two are the `WHERE pool = $1` pair below. The third has no row to match, so it was
    /// silently left behind — and since a fresh deployment writes a pool on nothing, that third
    /// case was every node in it. Those ids arrive in [`PoolCarry::fall_through`], resolved by
    /// `PoolResolver`, which stays the one implementation of the inheritance rule: a recursive CTE
    /// here would be a second one, and the second copy is what rots.
    ///
    /// Returns `(nodes, folders)` actually re-pointed — 0/0 when `take` is `None`.
    /// `Ok(None)` ⇒ no such poller.
    pub async fn move_poller_to_pool(
        &self,
        id: &str,
        to: &str,
        take: Option<PoolCarry<'_>>,
    ) -> anyhow::Result<Option<(u64, u64)>> {
        let mut tx = self.pool.begin().await?;
        let done = sqlx::query("UPDATE pollers SET pool = $2 WHERE id = $1")
            .bind(id)
            .bind(to)
            .execute(&mut *tx)
            .await?;
        if done.rows_affected() == 0 {
            tx.rollback().await?;
            return Ok(None);
        }
        let mut moved = (0, 0);
        if let Some(take) = take {
            moved.0 = sqlx::query("UPDATE nodes SET pool = $2 WHERE pool = $1")
                .bind(take.from)
                .bind(to)
                .execute(&mut *tx)
                .await?
                .rows_affected();
            moved.1 = sqlx::query("UPDATE node_groups SET pool = $2 WHERE pool = $1")
                .bind(take.from)
                .bind(to)
                .execute(&mut *tx)
                .await?
                .rows_affected();
            if !take.fall_through.is_empty() {
                // ⚠️ The `pool IS NULL OR trim(pool) = ''` guard is not the inheritance rule — it is
                // a concurrency guard. The ids were resolved before this transaction opened, so a
                // node that acquired its own pool in between must keep it rather than be dragged
                // along by a decision made about a state it has left.
                moved.0 += sqlx::query(
                    "UPDATE nodes SET pool = $2 \
                     WHERE id = ANY($1) AND (pool IS NULL OR trim(pool) = '')",
                )
                .bind(take.fall_through)
                .bind(to)
                .execute(&mut *tx)
                .await?
                .rows_affected();
            }
        }
        tx.commit().await?;
        Ok(Some(moved))
    }

    /// Poller ids currently reporting `name`.
    ///
    /// Since ADR-107 Inc.2 this is "the pollers core has serving `name`" — the column is core's,
    /// not the site's. Callers use it to ask whether each of them can follow a pool change before
    /// starting one.
    pub async fn pollers_reporting_pool(&self, name: &str) -> anyhow::Result<Vec<String>> {
        Ok(
            sqlx::query_scalar("SELECT id FROM pollers WHERE pool = $1 ORDER BY id")
                .bind(name)
                .fetch_all(&self.pool)
                .await?,
        )
    }
}
