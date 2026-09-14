// SPDX-License-Identifier: AGPL-3.0-only
//! `entity_row_names` — what each vendor-table row on a node is called (ADR-143).
//!
//! The poller reads a table's names hourly and sends them on the result that carried the values;
//! the ingest writer upserts them here, the alert engine loads them once at startup, and the metric
//! endpoint joins them onto a row's value. Descriptive device text, **never a TSDB label** (ADR-011).

use std::collections::BTreeMap;

use super::*;

/// One stored name: `(node_id, metric, row_key, name)`.
pub type RowNameRow = (Uuid, String, i64, String);

impl NodeRepo {
    /// Upsert names for many nodes in one statement.
    ///
    /// Dedups within the batch keeping the last name per `(node, metric, row)` — `ON CONFLICT` cannot
    /// touch one key twice in a statement — and rewrites a stored row only when its name changed,
    /// because names are re-sent every hour and almost never differ.
    ///
    /// A row naming a node deleted since its poll is dropped and the statement runs **once** more,
    /// the ADR-141 shape [`Self::upsert_interfaces_batch`] uses: one deleted node must not lose every
    /// other node's names behind a single warning.
    pub async fn upsert_row_names_batch(&self, rows: &[RowNameRow]) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut by_key: BTreeMap<(Uuid, &str, i64), &str> = BTreeMap::new();
        for (node, metric, row, name) in rows {
            by_key.insert((*node, metric.as_str(), *row), name.as_str());
        }
        let written = match self.upsert_row_name_rows(&by_key).await {
            Ok(written) => written,
            Err(sqlx::Error::Database(db)) if db.is_foreign_key_violation() => {
                let mut named: Vec<Uuid> = by_key.keys().map(|(node, ..)| *node).collect();
                named.dedup();
                let present = self.existing_node_ids(&named).await?;
                by_key.retain(|(node, ..), _| present.contains(node));
                self.upsert_row_name_rows(&by_key).await?
            }
            Err(e) => return Err(e.into()),
        };
        let offered = by_key.len() as u64;
        metrics::counter!("yagra_row_name_upsert_rows_total", "outcome" => "written")
            .increment(written);
        metrics::counter!("yagra_row_name_upsert_rows_total", "outcome" => "skipped")
            .increment(offered.saturating_sub(written));
        Ok(())
    }

    /// The statement [`Self::upsert_row_names_batch`] runs. Returns the `sqlx` error unconverted,
    /// because the caller's retry turns on whether it is a foreign-key violation.
    async fn upsert_row_name_rows(
        &self,
        by_key: &BTreeMap<(Uuid, &str, i64), &str>,
    ) -> Result<u64, sqlx::Error> {
        if by_key.is_empty() {
            return Ok(0);
        }
        let n = by_key.len();
        let mut nodes: Vec<Uuid> = Vec::with_capacity(n);
        let mut metrics_col: Vec<String> = Vec::with_capacity(n);
        let mut keys: Vec<i64> = Vec::with_capacity(n);
        let mut names: Vec<String> = Vec::with_capacity(n);
        for ((node, metric, row), name) in by_key {
            nodes.push(*node);
            metrics_col.push((*metric).to_owned());
            keys.push(*row);
            names.push((*name).to_owned());
        }
        let done = sqlx::query(
            "INSERT INTO entity_row_names (node_id, metric, row_key, name) \
             SELECT * FROM UNNEST($1::uuid[], $2::text[], $3::bigint[], $4::text[]) \
             ON CONFLICT (node_id, metric, row_key) DO UPDATE \
                SET name = EXCLUDED.name, updated_at = now() \
              WHERE entity_row_names.name IS DISTINCT FROM EXCLUDED.name",
        )
        .bind(&nodes)
        .bind(&metrics_col)
        .bind(&keys)
        .bind(&names)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected())
    }

    /// Every stored name — what the alert engine loads at startup, before results arrive, so a rule
    /// scoped to a row name keeps matching across a restart (ADR-143 decision 3).
    pub async fn list_row_names(&self) -> anyhow::Result<Vec<RowNameRow>> {
        let rows = sqlx::query("SELECT node_id, metric, row_key, name FROM entity_row_names")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|r| {
                Ok((
                    r.try_get("node_id")?,
                    r.try_get("metric")?,
                    r.try_get("row_key")?,
                    r.try_get("name")?,
                ))
            })
            .collect()
    }

    /// One node's names for the given metrics: `(metric, row_key, name)`.
    pub async fn row_names_for(
        &self,
        node: Uuid,
        metrics: &[String],
    ) -> anyhow::Result<Vec<(String, i64, String)>> {
        if metrics.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "SELECT metric, row_key, name FROM entity_row_names \
             WHERE node_id = $1 AND metric = ANY($2) ORDER BY metric, row_key",
        )
        .bind(node)
        .bind(metrics)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                Ok((
                    r.try_get("metric")?,
                    r.try_get("row_key")?,
                    r.try_get("name")?,
                ))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::pgtest;
    use sqlx::PgPool;

    /// The accepting side: names land, a changed name is rewritten, an unchanged one is not, and a
    /// row for a node that has since been deleted does not take the rest of the batch with it.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn names_are_upserted_read_back_and_survive_a_deleted_node(pool: PgPool) {
        let repo = pgtest::repo(pool.clone());
        let node = pgtest::node(&pool, "sw-1", 1, None).await;
        let gone = uuid::Uuid::new_v4();
        repo.upsert_row_names_batch(&[
            (node, "cisco_mem_used".into(), 1, "Processor".into()),
            (node, "cisco_mem_used".into(), 2, "I/O".into()),
            (gone, "cisco_mem_used".into(), 1, "nobody".into()),
        ])
        .await
        .expect("a deleted node's row is dropped, not fatal");
        assert_eq!(pgtest::rows(&pool, "entity_row_names").await, 2);

        repo.upsert_row_names_batch(&[(node, "cisco_mem_used".into(), 2, "I/O pool".into())])
            .await
            .expect("rename");
        let mut got = repo
            .row_names_for(node, &["cisco_mem_used".to_owned()])
            .await
            .expect("read");
        got.sort();
        assert_eq!(
            got,
            vec![
                ("cisco_mem_used".to_owned(), 1, "Processor".to_owned()),
                ("cisco_mem_used".to_owned(), 2, "I/O pool".to_owned()),
            ]
        );
        assert_eq!(repo.list_row_names().await.expect("list").len(), 2);
        assert!(repo
            .row_names_for(node, &["huawei_mem_usage".to_owned()])
            .await
            .expect("read")
            .is_empty());
    }
}
