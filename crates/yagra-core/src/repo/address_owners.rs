// SPDX-License-Identifier: AGPL-3.0-only
//! Which nodes claim an address — by their inventory address **or** by any address one of their
//! interfaces carries (ADR-180).
//!
//! The same two sources the network map's `derive_links` builds its owner map from (ADR-043), so the
//! Neighbors tab and the map agree about who stands at a peer's management address whenever one
//! node does. The rule applied to what comes back — one claimant is a match — is
//! `yagra_topology::sole_claimant`, shared with the map for the same reason. Where several nodes
//! claim it, the tab alone may narrow them by the name the neighbour sent (ADR-180 Inc.4); the map
//! draws no line.
//!
//! ⚠️ Read on every Neighbors-tab refresh (15 s per open tab), so the `node_l3` half must not scan
//! the table: each address is probed through the GIN index migration `0138` adds, via a lateral
//! containment test — a GIN index can serve `@>` per outer row, but not `= ANY(array)`.

use super::{AddressClaim, DeviceIdentity, GroupFilter, NodeRepo};
use sqlx::Row;
use std::net::IpAddr;

impl NodeRepo {
    /// The device nodes the caller may see whose **own interface-address list** carries one of
    /// `addresses`, paired with the address it carries (ADR-139 Inc.3).
    ///
    /// Unlike [`Self::address_claims`] this is a filter, not a projection: a Discovery candidate is
    /// only marked, never refused, so a node outside the caller's folders has nothing to say to them
    /// (ADR-148 decision 8). Only rows whose type identifies a node count — an `anycast` or `broadcast`
    /// address is carried by more than the one device. The monitored address itself is not asked:
    /// a node standing at the candidate's address is [`Self::device_nodes_at`]'s answer.
    ///
    /// At most `SHARED_VALUE_MAX + 1` nodes come back per address, as [`Self::device_nodes_named`]
    /// does per name: an address more nodes than that carry identifies none of them (ADR-148 decision
    /// 3), and a private address plan reused at every site would otherwise return one row per site
    /// on every refresh. The type filter runs in the statement, before the cap, so the rows the cap
    /// keeps are ones that count.
    ///
    /// ⚠️ Read on every scan refresh (2 s while a sweep runs), so each address is probed through the
    /// GIN index of migration `0138` exactly as `address_claims` does; the lateral expansion only
    /// runs over the lists that already matched.
    pub async fn device_nodes_carrying(
        &self,
        addresses: &[IpAddr],
        groups: GroupFilter<'_>,
    ) -> anyhow::Result<Vec<(IpAddr, DeviceIdentity)>> {
        if addresses.is_empty() {
            return Ok(Vec::new());
        }
        let text: Vec<String> = addresses.iter().map(ToString::to_string).collect();
        // The types that do not identify a node, from the one rule that says so. A missing or
        // unrecognised token reads as `Unknown`, which counts — hence the `COALESCE` to a token
        // no list can contain.
        let not_identifying: Vec<&str> = yagra_common::L3AddrType::ALL
            .into_iter()
            .filter(|t| !t.identifies_a_node())
            .map(yagra_common::L3AddrType::as_str)
            .collect();
        let sql = format!(
            "SELECT carried, id, name, address, sys_object_id FROM ( \
               SELECT q.ip AS carried, n.id, n.name, host(n.address) AS address, n.sys_object_id, \
                      row_number() OVER (PARTITION BY q.ip ORDER BY n.id) AS rn \
               FROM unnest($2::text[]) AS q(ip) \
               JOIN node_l3 l \
                 ON l.addresses->'addresses' @> jsonb_build_array(jsonb_build_object('ip', q.ip)) \
               JOIN nodes n ON n.id = l.node_id \
               CROSS JOIN LATERAL jsonb_array_elements(l.addresses->'addresses') a \
               WHERE a->>'ip' = q.ip \
                 AND COALESCE(a->>'addr_type', '') <> ALL($3::text[]) \
                 AND n.address IS NOT NULL AND {scope} AND {device} \
               GROUP BY q.ip, n.id \
             ) carriers WHERE rn <= $4",
            scope = Self::SCOPE_PREDICATE,
            device = Self::DEVICE_NODE_PREDICATE,
        );
        let per_address = i64::try_from(super::SHARED_VALUE_MAX + 1).unwrap_or(i64::MAX);
        let rows = sqlx::query(&sql)
            .bind(Self::scope_bind(groups))
            .bind(&text)
            .bind(&not_identifying)
            .bind(per_address)
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            // `host()`, never `::TEXT`: the cast renders the netmask and every parse would fail.
            let (Ok(carried), Ok(address)) = (
                row.try_get::<String, _>("carried")?.parse::<IpAddr>(),
                row.try_get::<String, _>("address")?.parse::<IpAddr>(),
            ) else {
                continue;
            };
            out.push((
                carried,
                DeviceIdentity {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    address,
                    sys_object_id: row.try_get("sys_object_id")?,
                },
            ));
        }
        Ok(out)
    }

    /// Every node that claims any of `addresses`, each marked with whether `groups` can see it.
    ///
    /// Like [`Self::device_nodes_at`], the scope is **projected, not filtered**: a claimant outside
    /// the caller's folders still comes back, because "two nodes claim this address" is only true if
    /// both are counted. Withholding a hidden claimant's name and id is the API layer's decision.
    ///
    /// Unlike it, URL and DNS monitors are **not** excluded — the map counts them as claimants too,
    /// and matching its rule is the point (ADR-180 decision 2).
    ///
    /// Each claim carries the port it is on (ADR-180 Inc.4), so a node appears once for the port
    /// carrying the address and once more if it is also its inventory address.
    pub async fn address_claims(
        &self,
        addresses: &[IpAddr],
        groups: GroupFilter<'_>,
    ) -> anyhow::Result<Vec<AddressClaim>> {
        if addresses.is_empty() {
            return Ok(Vec::new());
        }
        // Canonical text: the poller writes `node_l3` addresses with `IpAddr`'s `Display`, so the
        // containment test only matches the same spelling.
        let text: Vec<String> = addresses.iter().map(ToString::to_string).collect();
        // `COALESCE` because the scope predicate is a projection here; see `device_nodes_at`. The
        // lateral expansion runs only over the lists the containment test already matched, so the
        // index still decides which rows are read.
        let sql = format!(
            "SELECT q.ip AS address, n.id, n.name, COALESCE({scope}, false) AS visible, \
                    NULL::bigint AS ifindex \
             FROM unnest($2::text[]) AS q(ip) \
             JOIN nodes n ON n.address = q.ip::inet \
             UNION \
             SELECT q.ip AS address, n.id, n.name, COALESCE({scope}, false) AS visible, \
                    (a->>'ifindex')::bigint AS ifindex \
             FROM unnest($2::text[]) AS q(ip) \
             JOIN node_l3 l \
               ON l.addresses->'addresses' @> jsonb_build_array(jsonb_build_object('ip', q.ip)) \
             JOIN nodes n ON n.id = l.node_id \
             CROSS JOIN LATERAL jsonb_array_elements(l.addresses->'addresses') a \
             WHERE a->>'ip' = q.ip",
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
                let ifindex: Option<i64> = row.try_get("ifindex")?;
                Ok(AddressClaim {
                    address: address.parse().map_err(|e| {
                        anyhow::anyhow!("claimed address {address:?} does not parse: {e}")
                    })?,
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    visible: row.try_get("visible")?,
                    // An index a `u32` cannot hold was not written by the poller; the claim still
                    // counts, it just names no port.
                    ifindex: ifindex.and_then(|i| u32::try_from(i).ok()),
                })
            })
            .collect()
    }
}
