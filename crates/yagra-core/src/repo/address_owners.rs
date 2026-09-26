// SPDX-License-Identifier: AGPL-3.0-only
//! Which nodes claim an address — by their inventory address **or** by any address one of their
//! interfaces carries (ADR-180).
//!
//! The same two sources the network map's `derive_links` builds its owner map from (ADR-043), so the
//! Neighbors tab and the map cannot disagree about who stands at a peer's management address. The
//! rule applied to what comes back — one claimant is a match, two identify nobody — is
//! `yagra_topology::sole_claimant`, shared with the map for the same reason.
//!
//! ⚠️ Read on every Neighbors-tab refresh (15 s per open tab), so the `node_l3` half must not scan
//! the table: each address is probed through the GIN index migration `0138` adds, via a lateral
//! containment test — a GIN index can serve `@>` per outer row, but not `= ANY(array)`.

use super::{AddressMatch, GroupFilter, NodeRepo};
use sqlx::Row;
use std::net::IpAddr;

impl NodeRepo {
    /// Every node that claims any of `addresses`, each marked with whether `groups` can see it.
    ///
    /// Like [`Self::device_nodes_at`], the scope is **projected, not filtered**: a claimant outside
    /// the caller's folders still comes back, because "two nodes claim this address" is only true if
    /// both are counted. Withholding a hidden claimant's name and id is the API layer's decision.
    ///
    /// Unlike it, URL and DNS monitors are **not** excluded — the map counts them as claimants too,
    /// and matching its rule is the point (ADR-180 決定 2).
    pub async fn address_claims(
        &self,
        addresses: &[IpAddr],
        groups: GroupFilter<'_>,
    ) -> anyhow::Result<Vec<AddressMatch>> {
        if addresses.is_empty() {
            return Ok(Vec::new());
        }
        // Canonical text: the poller writes `node_l3` addresses with `IpAddr`'s `Display`, so the
        // containment test only matches the same spelling.
        let text: Vec<String> = addresses.iter().map(ToString::to_string).collect();
        // `COALESCE` because the scope predicate is a projection here; see `device_nodes_at`.
        let sql = format!(
            "SELECT q.ip AS address, n.id, n.name, COALESCE({scope}, false) AS visible \
             FROM unnest($2::text[]) AS q(ip) \
             JOIN nodes n ON n.address = q.ip::inet \
             UNION \
             SELECT q.ip AS address, n.id, n.name, COALESCE({scope}, false) AS visible \
             FROM unnest($2::text[]) AS q(ip) \
             JOIN node_l3 l \
               ON l.addresses->'addresses' @> jsonb_build_array(jsonb_build_object('ip', q.ip)) \
             JOIN nodes n ON n.id = l.node_id",
            scope = Self::SCOPE_PREDICATE,
        );
        let rows = sqlx::query(&sql)
            .bind(Self::scope_bind(groups))
            .bind(&text)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| {
                let address: String = row.try_get("address")?;
                Ok(AddressMatch {
                    address: address.parse().map_err(|e| {
                        anyhow::anyhow!("claimed address {address:?} does not parse: {e}")
                    })?,
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    visible: row.try_get("visible")?,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::pgtest;
    use std::net::IpAddr;
    use uuid::Uuid;
    use yagra_common::{L3AddrType, L3Address, L3Snapshot, L3SourceTable};

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    /// Interface addresses for `node`, through the production writer.
    async fn l3(pool: &sqlx::PgPool, node: Uuid, ips: &[&str]) {
        let snapshot = L3Snapshot::new(
            ips.iter()
                .map(|a| L3Address {
                    ifindex: 1,
                    ip: ip(a),
                    prefix_len: 24,
                    addr_type: L3AddrType::Unicast,
                    source_table: L3SourceTable::IpAddressTable,
                })
                .collect(),
        );
        crate::l3::L3Repo::new(pool.clone())
            .record_observation(node, &snapshot)
            .await
            .unwrap();
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn claims_cover_node_and_interface_addresses_v4_and_v6(pool: sqlx::PgPool) {
        let repo = pgtest::repo(pool.clone());
        let a = pgtest::node_at(&pool, "rtr-a", ip("192.0.2.1"), None).await;
        let b = pgtest::node_at(&pool, "rtr-b", ip("198.51.100.1"), None).await;
        l3(&pool, b, &["203.0.113.9", "2001:db8::9"]).await;

        let found = repo
            .address_claims(
                &[
                    ip("192.0.2.1"),
                    ip("203.0.113.9"),
                    ip("2001:db8::9"),
                    ip("192.0.2.99"),
                ],
                None,
            )
            .await
            .unwrap();
        let mut got: Vec<(String, Uuid)> = found
            .iter()
            .map(|m| (m.address.to_string(), m.id))
            .collect();
        got.sort();
        let mut want = vec![
            ("192.0.2.1".to_owned(), a),
            ("2001:db8::9".to_owned(), b),
            ("203.0.113.9".to_owned(), b),
        ];
        want.sort();
        assert_eq!(got, want);
        assert!(found.iter().all(|m| m.visible));
    }

    /// Two claimants come back as two rows: the one-owner rule is applied above this reader, and
    /// it can only be applied if nothing here collapses them.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_address_two_nodes_claim_returns_both(pool: sqlx::PgPool) {
        let repo = pgtest::repo(pool.clone());
        let a = pgtest::node_at(&pool, "rtr-a", ip("192.0.2.1"), None).await;
        let b = pgtest::node_at(&pool, "rtr-b", ip("192.0.2.2"), None).await;
        l3(&pool, b, &["192.0.2.1"]).await;
        let found = repo.address_claims(&[ip("192.0.2.1")], None).await.unwrap();
        let mut ids: Vec<Uuid> = found.iter().map(|m| m.id).collect();
        ids.sort();
        let mut want = vec![a, b];
        want.sort();
        assert_eq!(ids, want);
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_node_outside_the_scope_is_returned_marked_invisible(pool: sqlx::PgPool) {
        let repo = pgtest::repo(pool.clone());
        let seen = pgtest::group(&pool, "site-a").await;
        let other = pgtest::group(&pool, "site-b").await;
        let hidden = pgtest::node_at(&pool, "rtr-hidden", ip("192.0.2.7"), Some(other)).await;
        let root = pgtest::node_at(&pool, "rtr-root", ip("192.0.2.8"), None).await;
        let visible = [seen];
        let found = repo
            .address_claims(&[ip("192.0.2.7"), ip("192.0.2.8")], Some(&visible))
            .await
            .unwrap();
        let mut got: Vec<(Uuid, bool)> = found.iter().map(|m| (m.id, m.visible)).collect();
        got.sort();
        let mut want = vec![(hidden, false), (root, false)];
        want.sort();
        assert_eq!(got, want);
    }

    /// The interface half is probed through the GIN index, not a scan. Sequential scans are switched
    /// off for the statement so a tiny test table cannot hide a query shape the index cannot serve:
    /// with the index unusable, the plan still falls back to a scan and this fails.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_interface_half_can_use_the_gin_index(pool: sqlx::PgPool) {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("SET LOCAL enable_seqscan = off")
            .execute(&mut *tx)
            .await
            .unwrap();
        let rows = sqlx::query(
            "EXPLAIN SELECT l.node_id FROM unnest($1::text[]) AS q(ip) \
             JOIN node_l3 l \
               ON l.addresses->'addresses' @> jsonb_build_array(jsonb_build_object('ip', q.ip))",
        )
        .bind(vec!["192.0.2.1".to_owned()])
        .fetch_all(&mut *tx)
        .await
        .unwrap();
        let plan: Vec<String> = rows
            .iter()
            .map(|r| sqlx::Row::get::<String, _>(r, 0))
            .collect();
        assert!(
            plan.iter().any(|l| l.contains("node_l3_addresses_gin")),
            "plan does not use the index:\n{}",
            plan.join("\n")
        );
    }
}
