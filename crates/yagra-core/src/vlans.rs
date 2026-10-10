// SPDX-License-Identifier: AGPL-3.0-only
//! Port VLAN persistence: each node's current port mode and VLAN set (ADR-201, migration 0147).
//!
//! Relational metadata, so PostgreSQL — never a TSDB label. Modelled on [`crate::l3`]: one document
//! per node, replaced whole on every observation, so a VLAN removed from a port is gone after the
//! next walk. Unlike `node_l3` there is no change history: the device's own configuration archive
//! is where the history of its VLAN configuration belongs.
//!
//! Only ever written with a snapshot a walk actually completed. A walk with any column unanswered
//! sends no snapshot at all, so this is never reached with a half-read table.

use sqlx::types::Json;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_common::VlanSnapshot;

/// PostgreSQL-backed store for each node's current port VLAN snapshot.
pub struct VlanRepo {
    pool: PgPool,
}

impl VlanRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Replace a node's stored snapshot with this observation.
    pub async fn record_observation(
        &self,
        node_id: Uuid,
        snapshot: &VlanSnapshot,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO node_vlans (node_id, ports, observed_at) VALUES ($1, $2, now()) \
             ON CONFLICT (node_id) DO UPDATE SET ports = EXCLUDED.ports, observed_at = now()",
        )
        .bind(node_id)
        .bind(Json(snapshot))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// One node's current snapshot, or `None` when none was ever recorded.
    ///
    /// `None` and `Some(empty)` are different answers: the first means no walk has completed (or
    /// the device is of a make this build does not read), the second that the device reported no
    /// switch ports. A reader shows the first as "not reported".
    pub async fn current(&self, node_id: Uuid) -> anyhow::Result<Option<VlanSnapshot>> {
        let row = sqlx::query("SELECT ports FROM node_vlans WHERE node_id = $1")
            .bind(node_id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| {
            let snapshot: Json<VlanSnapshot> = row.try_get("ports")?;
            Ok(snapshot.0)
        })
        .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::{PortMode, PortVlan};

    fn trunk(ifindex: u32) -> PortVlan {
        let mut p = PortVlan::new(ifindex, PortMode::Trunk);
        p.native = Some(1);
        p.allowed = vec![(700, 700), (801, 869)];
        p
    }

    /// A stored snapshot reads back as it was written, and a second observation replaces it whole —
    /// the property that makes a removed VLAN disappear.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_observation_replaces_the_stored_snapshot_whole(pool: sqlx::PgPool) {
        let node = yagra_base::pgtest::node(&pool, "vlan-switch", 1, None).await;
        let repo = VlanRepo::new(pool.clone());
        assert_eq!(repo.current(node).await.unwrap(), None);

        let first = VlanSnapshot::new(vec![trunk(3), PortVlan::new(4, PortMode::NotL2)]);
        repo.record_observation(node, &first).await.unwrap();
        assert_eq!(repo.current(node).await.unwrap(), Some(first));

        let second = VlanSnapshot::new(vec![trunk(3)]);
        repo.record_observation(node, &second).await.unwrap();
        assert_eq!(repo.current(node).await.unwrap(), Some(second));
    }
}
