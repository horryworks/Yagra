// SPDX-License-Identifier: AGPL-3.0-only
//! `prefix_gap_acks` — the missing subnets an operator marked as intentional (ADR-170 Inc.4).
//!
//! The gaps themselves are never stored: `crate::prefix_gaps` recomputes them on every read. Only
//! the person's decision lives here.

use super::*;

/// One stored mark: the site (the nil uuid for the root), the subnet, the kind it had when marked,
/// and the note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredGapAck {
    pub site: Uuid,
    pub subnet: String,
    pub kind: String,
    pub note: String,
}

impl NodeRepo {
    /// Every mark.
    pub async fn prefix_gap_acks(&self) -> anyhow::Result<Vec<StoredGapAck>> {
        let rows = sqlx::query("SELECT site_id, subnet, kind, note FROM prefix_gap_acks")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|r| {
                Ok(StoredGapAck {
                    site: r.try_get("site_id")?,
                    subnet: r.try_get("subnet")?,
                    kind: r.try_get("kind")?,
                    note: r.try_get("note")?,
                })
            })
            .collect()
    }

    /// Record (or replace) a mark.
    pub async fn set_prefix_gap_ack(&self, ack: &StoredGapAck) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO prefix_gap_acks (site_id, subnet, kind, note) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (site_id, subnet) DO UPDATE \
             SET kind = EXCLUDED.kind, note = EXCLUDED.note, acked_at = now()",
        )
        .bind(ack.site)
        .bind(&ack.subnet)
        .bind(&ack.kind)
        .bind(&ack.note)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Remove a mark. `false` when there was none.
    pub async fn delete_prefix_gap_ack(&self, site: Uuid, subnet: &str) -> anyhow::Result<bool> {
        let done = sqlx::query("DELETE FROM prefix_gap_acks WHERE site_id = $1 AND subnet = $2")
            .bind(site)
            .bind(subnet)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::StoredGapAck;
    use crate::pgtest;
    use sqlx::PgPool;

    /// A mark round-trips, a second write replaces it rather than adding a row, and removing it
    /// twice says the second time that there was nothing to remove.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_mark_round_trips_and_a_rewrite_replaces_it(pool: PgPool) {
        let repo = pgtest::repo(pool);
        let mut ack = StoredGapAck {
            site: uuid::Uuid::nil(),
            subnet: "192.0.2.0/24".to_owned(),
            kind: "unregistered".to_owned(),
            note: "lab".to_owned(),
        };
        repo.set_prefix_gap_ack(&ack).await.expect("set");
        ack.kind = "partial".to_owned();
        ack.note = "lab, half registered".to_owned();
        repo.set_prefix_gap_ack(&ack).await.expect("replace");
        assert_eq!(
            repo.prefix_gap_acks().await.expect("read"),
            vec![ack.clone()]
        );
        assert!(repo
            .delete_prefix_gap_ack(ack.site, &ack.subnet)
            .await
            .expect("delete"));
        assert!(!repo
            .delete_prefix_gap_ack(ack.site, &ack.subnet)
            .await
            .expect("delete"));
    }
}
