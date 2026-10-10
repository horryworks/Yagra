// SPDX-License-Identifier: AGPL-3.0-only
//! The `node_state_snapshots` table: the fleet health timeline.
//!
//! One row per `(state, count)` per tick, all sharing a timestamp. Low cardinality by
//! construction (at most six states), which is why this is metadata in PostgreSQL rather than a
//! series in the TSDB.
//!
//! ⚠️ [`NodeRepo::prune_state_snapshots`] is one of the ten sites
//! `retention.rs::PRUNE_SITES` searches. That list names this file by path; a wrong path can only
//! make its search *fail*, naming the subject, which is what makes a hand-maintained list
//! tolerable there.

use sqlx::Row;

// Only the settings struct: `retention::Row` would collide with `sqlx::Row` above.

use super::*;

impl NodeRepo {
    /// Append one node-state snapshot: a row per `(state, count)`, all sharing the same `now()`
    /// timestamp (single statement). For the fleet health timeline. Low cardinality (≤6 rows).
    pub async fn insert_state_snapshot(&self, counts: &[(String, i64)]) -> anyhow::Result<()> {
        if counts.is_empty() {
            return Ok(());
        }
        let states: Vec<String> = counts.iter().map(|(s, _)| s.clone()).collect();
        // Saturate rather than silently wrap: a per-state node count above i32::MAX is not
        // reachable at the design scale (tens of thousands), so flag it instead of corrupting
        // the timeline with a negative value.
        let nums: Vec<i32> = counts
            .iter()
            .map(|(_, c)| {
                i32::try_from(*c).unwrap_or_else(|_| {
                    tracing::warn!(count = *c, "state-snapshot count exceeds i32; saturating");
                    i32::MAX
                })
            })
            .collect();
        sqlx::query(
            "INSERT INTO node_state_snapshots (ts, state, count) \
             SELECT now(), s, c FROM unnest($1::text[], $2::int[]) AS t(s, c)",
        )
        .bind(&states)
        .bind(&nums)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// State-count snapshots over `[from_s, to_s]` (Unix seconds) as `(ts_unix, state, count)`,
    /// oldest first. Pivoted into per-state series at the API edge.
    pub async fn state_history(
        &self,
        from_s: i64,
        to_s: i64,
    ) -> anyhow::Result<Vec<(i64, String, i64)>> {
        let rows = sqlx::query(
            "SELECT extract(epoch from ts)::bigint AS t, state, count \
             FROM node_state_snapshots \
             WHERE ts >= to_timestamp($1) AND ts <= to_timestamp($2) ORDER BY ts",
        )
        .bind(from_s)
        .bind(to_s)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                Ok((
                    r.try_get("t")?,
                    r.try_get("state")?,
                    r.try_get::<i32, _>("count")? as i64,
                ))
            })
            .collect()
    }

    /// Delete state snapshots older than `older_than_secs` (retention). Returns rows removed.
    pub async fn prune_state_snapshots(&self, older_than_secs: i64) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "DELETE FROM node_state_snapshots WHERE ts < now() - ($1::double precision * interval '1 second')",
        )
        .bind(older_than_secs as f64)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use crate::pgtest;

    /// A snapshot goes in as one row per state and comes back out inside its window.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_snapshot_reads_back_inside_its_window_and_not_outside_it(pool: sqlx::PgPool) {
        let repo = pgtest::repo(pool.clone());
        repo.insert_state_snapshot(&[("ok".to_owned(), 41), ("unreachable".to_owned(), 1)])
            .await
            .expect("insert");
        assert_eq!(pgtest::rows(&pool, "node_state_snapshots").await, 2);

        let now = chrono::Utc::now().timestamp();
        let inside = repo
            .state_history(now - 3600, now + 3600)
            .await
            .expect("history");
        assert_eq!(inside.len(), 2);
        let ok = inside
            .iter()
            .find(|(_, state, _)| state == "ok")
            .expect("the ok row");
        assert_eq!(ok.2, 41);

        let before = repo
            .state_history(now - 7200, now - 3600)
            .await
            .expect("history");
        assert!(
            before.is_empty(),
            "a window that ended an hour ago returned a snapshot taken now"
        );
    }

    /// An empty snapshot writes nothing — the early return, which the timeline depends on.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_empty_snapshot_writes_no_row(pool: sqlx::PgPool) {
        pgtest::repo(pool.clone())
            .insert_state_snapshot(&[])
            .await
            .expect("insert");
        assert_eq!(pgtest::rows(&pool, "node_state_snapshots").await, 0);
    }

    /// Pruning keeps what is inside the retention window and removes what is outside it.
    ///
    /// ⚠️ The exact boundary is not reachable here: rows are stamped `now()` by the writer, so a
    /// test cannot place one at the cut without backdating it with SQL of its own — which would be
    /// a second copy of the schema. What is provable is that the predicate points the right way,
    /// which is the direction this has been got wrong in.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn pruning_keeps_what_is_inside_the_window(pool: sqlx::PgPool) {
        let repo = pgtest::repo(pool.clone());
        repo.insert_state_snapshot(&[("ok".to_owned(), 7)])
            .await
            .expect("insert");

        assert_eq!(
            repo.prune_state_snapshots(3600).await.expect("prune"),
            0,
            "a row taken a moment ago was pruned by an hour-long retention window"
        );
        assert_eq!(pgtest::rows(&pool, "node_state_snapshots").await, 1);

        assert_eq!(repo.prune_state_snapshots(0).await.expect("prune"), 1);
        assert_eq!(pgtest::rows(&pool, "node_state_snapshots").await, 0);
    }
}
