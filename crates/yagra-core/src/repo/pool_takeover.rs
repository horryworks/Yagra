// SPDX-License-Identifier: AGPL-3.0-only
//! **Covering a pool that lost its poller, reversibly** (ADR-107 増分 4).
//!
//! When a poller pool has nodes and no live poller, those nodes stop being polled and
//! [`crate::pool_coverage`] raises a `Subject::Pool` alert saying so. One of the two things an
//! operator can do about it is point the pool's members at a pool that *does* have a poller —
//! usually the co-located one — until the site is back.
//!
//! ## Why this is a table and not a column on `nodes`
//!
//! A node is in a pool three ways (ADR-107 増分 3): its own `pool` column, a folder it inherits
//! from, or falling through to the default. Taking a fall-through node over writes an explicit pool
//! where there was NULL, so putting it back means writing NULL again — and a single
//! `pool_before_takeover` column cannot tell "it was inheriting" from "it was never taken over",
//! because both read NULL. Here the *row* is the marker and `previous_pool` is free to be NULL.
//!
//! ⚠️ **Be precise about when that happens: only when the pool being covered is the default one.**
//! `PoolCarry::fall_through` is empty for every other source, because the default is the only pool
//! inheritance can bottom out in — and a node whose *folder* names the covered pool needs no record
//! at all, since the folder moves and the node follows it back. So covering a site pool produces
//! rows that all carry a name, and covering the default pool is the case this design exists for.
//! The first version of the round-trip test claimed otherwise, passed, and described a deployment
//! that cannot exist.
//!
//! ## What this module deliberately does not decide
//!
//! Whether taking over is a good idea. It usually is not: a site poller exists because core cannot
//! reach those devices, and covering them from a host that cannot see them replaces one accurate
//! pool alert with N false `unreachable` ones — the notification flood
//! `monitoring-conventions.md` forbids. That judgement belongs to the operator, who can see the
//! alert and test reachability from the new host; ADR-107 増分 4 決定 4 is that nothing here ever
//! runs by itself.

use serde::Serialize;
use sqlx::Row;

use super::{NodeRepo, PoolCarry};

/// What is outstanding for one pool, for the badge and the restore button.
///
/// There is deliberately no per-subject type. Nothing outside this module needs to know *which*
/// node is covered — the restore is addressed by pool, and a list of ids would be a second answer
/// to "who is in this pool" that `PoolResolver` already gives from live state.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct PoolTakeoverSummary {
    /// The pool being covered for.
    pub from_pool: String,
    /// Where its members are pointed.
    pub to_pool: String,
    /// How many nodes.
    pub nodes: i64,
    /// How many folders.
    pub folders: i64,
    /// Unix seconds of the earliest row, i.e. when the cover began.
    pub taken_at: i64,
    /// Who chose it.
    pub taken_by: String,
}

/// Nodes and folders moved by one call.
#[derive(Debug, Clone, Copy, Serialize, utoipa::ToSchema)]
pub struct PoolTakeoverCounts {
    pub nodes: u64,
    pub folders: u64,
}

impl NodeRepo {
    /// Point `from`'s members at `to`, recording where each of them belongs.
    ///
    /// One transaction, and the **record is written before the move** — deliberately. The
    /// recording statements read the very columns the moves overwrite, so running them afterwards
    /// would store the destination as the origin and make the restore a no-op that looks like a
    /// success.
    ///
    /// `carry.fall_through` carries the inheriting ids, resolved by
    /// [`crate::poolres::PoolResolver`] at the call site for the reason [`PoolCarry`] gives: there
    /// is no predicate over `nodes` that finds them without re-implementing the inheritance rule.
    /// Those rows are recorded with `previous_pool = NULL`.
    ///
    /// Returns what actually moved. A subject already taken over is left exactly as it is —
    /// `ON CONFLICT DO NOTHING` keeps the *first* `previous_pool`, which is the only copy of where
    /// it belongs.
    pub async fn take_over_pool(
        &self,
        to: &str,
        by: &str,
        carry: PoolCarry<'_>,
    ) -> anyhow::Result<PoolTakeoverCounts> {
        let from = carry.from;
        let mut tx = self.pool.begin().await?;

        // Nodes that name the pool. `previous_pool` is the name itself.
        sqlx::query(
            "INSERT INTO pool_takeover (kind, subject_id, from_pool, to_pool, previous_pool, taken_by) \
             SELECT 'node', id, $1, $2, $1, $3 FROM nodes WHERE pool = $1 \
             ON CONFLICT (kind, subject_id) DO NOTHING",
        )
        .bind(from)
        .bind(to)
        .bind(by)
        .execute(&mut *tx)
        .await?;

        // Folders that name it.
        sqlx::query(
            "INSERT INTO pool_takeover (kind, subject_id, from_pool, to_pool, previous_pool, taken_by) \
             SELECT 'group', id, $1, $2, $1, $3 FROM node_groups WHERE pool = $1 \
             ON CONFLICT (kind, subject_id) DO NOTHING",
        )
        .bind(from)
        .bind(to)
        .bind(by)
        .execute(&mut *tx)
        .await?;

        // The inheriting ones. NULL `previous_pool` is the whole point of the table.
        if !carry.fall_through.is_empty() {
            sqlx::query(
                "INSERT INTO pool_takeover (kind, subject_id, from_pool, to_pool, previous_pool, taken_by) \
                 SELECT 'node', id, $1, $2, NULL, $4 FROM nodes \
                 WHERE id = ANY($3) AND (pool IS NULL OR trim(pool) = '') \
                 ON CONFLICT (kind, subject_id) DO NOTHING",
            )
            .bind(from)
            .bind(to)
            .bind(carry.fall_through)
            .bind(by)
            .execute(&mut *tx)
            .await?;
        }

        // Now the move itself, the same three statements `move_poller_to_pool` uses. The
        // `pool IS NULL OR trim(pool) = ''` guard on the third is a concurrency guard, not the
        // inheritance rule: an id that acquired its own pool between the resolve and this
        // transaction keeps it.
        let mut counts = PoolTakeoverCounts {
            nodes: 0,
            folders: 0,
        };
        counts.nodes = sqlx::query("UPDATE nodes SET pool = $2 WHERE pool = $1")
            .bind(from)
            .bind(to)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        counts.folders = sqlx::query("UPDATE node_groups SET pool = $2 WHERE pool = $1")
            .bind(from)
            .bind(to)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if !carry.fall_through.is_empty() {
            counts.nodes += sqlx::query(
                "UPDATE nodes SET pool = $2 \
                 WHERE id = ANY($1) AND (pool IS NULL OR trim(pool) = '')",
            )
            .bind(carry.fall_through)
            .bind(to)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        }
        tx.commit().await?;
        Ok(counts)
    }

    /// Put `from`'s members back where the takeover found them, and forget the record.
    ///
    /// ⚠️ Each subject is restored **to its own** `previous_pool`, not to `from_pool` — those
    /// differ for every inheriting node, whose `previous_pool` is NULL. Restoring the pool name to
    /// all of them would pin rows that were never pinned, which is a different deployment from the
    /// one the takeover was asked to be reversible about.
    ///
    /// A subject that has since been deleted, or whose pool a person has changed by hand, is
    /// updated only if it still sits in `to_pool`: the record is bookkeeping about a decision, and
    /// a later decision by a human outranks it. Its row is dropped either way, so the pool does not
    /// stay marked as covered forever.
    pub async fn restore_taken_over_pool(&self, from: &str) -> anyhow::Result<PoolTakeoverCounts> {
        let mut tx = self.pool.begin().await?;
        let nodes = sqlx::query(
            "UPDATE nodes n SET pool = t.previous_pool FROM pool_takeover t \
             WHERE t.kind = 'node' AND t.subject_id = n.id AND t.from_pool = $1 \
               AND n.pool IS NOT DISTINCT FROM t.to_pool",
        )
        .bind(from)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        let folders = sqlx::query(
            "UPDATE node_groups g SET pool = t.previous_pool FROM pool_takeover t \
             WHERE t.kind = 'group' AND t.subject_id = g.id AND t.from_pool = $1 \
               AND g.pool IS NOT DISTINCT FROM t.to_pool",
        )
        .bind(from)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        sqlx::query("DELETE FROM pool_takeover WHERE from_pool = $1")
            .bind(from)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(PoolTakeoverCounts { nodes, folders })
    }

    /// Every pool currently being covered from elsewhere, oldest cover first.
    pub async fn pool_takeovers(&self) -> anyhow::Result<Vec<PoolTakeoverSummary>> {
        let rows = sqlx::query(
            "SELECT from_pool, \
                    min(to_pool) AS to_pool, \
                    count(*) FILTER (WHERE kind = 'node')  AS nodes, \
                    count(*) FILTER (WHERE kind = 'group') AS folders, \
                    extract(epoch FROM min(taken_at))::bigint AS taken_at, \
                    min(taken_by) AS taken_by \
             FROM pool_takeover GROUP BY from_pool ORDER BY min(taken_at)",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| PoolTakeoverSummary {
                from_pool: r.get("from_pool"),
                to_pool: r.get("to_pool"),
                nodes: r.get("nodes"),
                folders: r.get("folders"),
                taken_at: r.get("taken_at"),
                taken_by: r.get("taken_by"),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use crate::pgtest;
    use uuid::Uuid;

    /// One node that names `pool`, one that names nothing.
    ///
    /// Built through `pgtest::node`, which goes via the production writer -- a hand-rolled INSERT
    /// here got `nodes.address` wrong (it is `inet`, not text) and would have kept getting the
    /// next column wrong too.
    async fn two_nodes(pg: &sqlx::PgPool, pool: &str) -> (Uuid, Uuid) {
        let named = pgtest::node(pg, "named", 1, None).await;
        let bare = pgtest::node(pg, "bare", 2, None).await;
        pgtest::repo(pg.clone())
            .set_node_pool(named, Some(pool))
            .await
            .expect("pin the first node to the pool");
        (named, bare)
    }

    async fn pool_of(pg: &sqlx::PgPool, id: Uuid) -> Option<String> {
        sqlx::query_scalar::<_, Option<String>>("SELECT pool FROM nodes WHERE id = $1")
            .bind(id)
            .fetch_one(pg)
            .await
            .expect("read pool")
    }

    /// The round trip, and the case the table exists for.
    ///
    /// 🚨 The second assertion is the increment. A node that *named* the pool goes back to naming
    /// it, which a `nodes.pool_before_takeover` column could also have managed. A node that was
    /// **inheriting** has to go back to naming nothing, and a column cannot tell "was inheriting"
    /// from "was never taken over" — both read NULL. A restore that leaves it pinned produces a
    /// deployment the takeover never promised to be reversible about, and it would look like
    /// success.
    ///
    /// ⚠️ The source is the **default** pool, and it has to be: `PoolCarry::fall_through` is empty
    /// for any other, because the default is the only pool inheritance can bottom out in. The first
    /// version of this test covered `tokyo` while handing it a fall-through id, which passes and
    /// describes a deployment that cannot exist.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_inheriting_node_comes_back_inheriting(pg: sqlx::PgPool) {
        let default = yagra_bus::DEFAULT_POOL;
        let (named, bare) = two_nodes(&pg, default).await;
        let repo = pgtest::repo(pg.clone());

        let moved = repo
            .take_over_pool(
                "spare",
                "tester",
                super::PoolCarry {
                    from: default,
                    fall_through: &[bare],
                },
            )
            .await
            .expect("take over");
        assert_eq!(moved.nodes, 2, "both nodes should have moved");
        assert_eq!(pool_of(&pg, named).await.as_deref(), Some("spare"));
        assert_eq!(pool_of(&pg, bare).await.as_deref(), Some("spare"));

        let back = repo
            .restore_taken_over_pool(default)
            .await
            .expect("restore");
        assert_eq!(back.nodes, 2);
        assert_eq!(pool_of(&pg, named).await.as_deref(), Some(default));
        assert_eq!(
            pool_of(&pg, bare).await,
            None,
            "an inheriting node came back pinned to a pool it never named"
        );
        assert_eq!(pgtest::rows(&pg, "pool_takeover").await, 0);
    }

    /// Taking a pool over twice keeps the *first* record, because it is the only copy of where its
    /// members belong. Without `ON CONFLICT DO NOTHING` the second call would overwrite
    /// `previous_pool` with the destination and make the restore a no-op that reports success.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_second_takeover_does_not_overwrite_where_the_members_belong(pg: sqlx::PgPool) {
        let (named, _) = two_nodes(&pg, "tokyo").await;
        let repo = pgtest::repo(pg.clone());
        let carry = |from| super::PoolCarry {
            from,
            fall_through: &[] as &[Uuid],
        };
        repo.take_over_pool("default", "tester", carry("tokyo"))
            .await
            .expect("first");
        // The node now names `default`, so the second call finds nothing under `tokyo` to move —
        // and must not record `default` as the place `named` belongs.
        repo.take_over_pool("spare", "tester", carry("tokyo"))
            .await
            .expect("second");
        repo.restore_taken_over_pool("tokyo")
            .await
            .expect("restore");
        assert_eq!(pool_of(&pg, named).await.as_deref(), Some("tokyo"));
    }

    /// The summary is what the picker's `covered_by` badge is built from.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_summary_names_the_pool_doing_the_covering(pg: sqlx::PgPool) {
        let (_, bare) = two_nodes(&pg, "tokyo").await;
        let repo = pgtest::repo(pg.clone());
        assert!(repo.pool_takeovers().await.expect("empty").is_empty());
        repo.take_over_pool(
            "default",
            "tester",
            super::PoolCarry {
                from: "tokyo",
                fall_through: &[bare],
            },
        )
        .await
        .expect("take over");
        let rows = repo.pool_takeovers().await.expect("summary");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].from_pool, "tokyo");
        assert_eq!(rows[0].to_pool, "default");
        assert_eq!(rows[0].nodes, 2);
        assert_eq!(rows[0].taken_by, "tester");
    }
}
