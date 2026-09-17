// SPDX-License-Identifier: AGPL-3.0-only
//! Interface-address persistence: the current address set per node and the append-on-change
//! history of it (ADR-043, migration 0065).
//!
//! Structured observations, so this is PostgreSQL (store separation) — an IP address in a
//! `SeriesKey` label is the cardinality explosion CLAUDE.md §7.1 names (ADR-011). This is the I/O
//! adapter; the model, its canonicalization and the change key live in `yagra-common` and are
//! tested there. Runtime `sqlx::query` (not the compile-time macro) so the build needs no live
//! database.
//!
//! Modelled on [`crate::neighbors`] down to the CTE: the same "upsert current state, append history
//! only when the content key moved" shape, for the same reason. Addressing is normally constant, so
//! what an operator wants is the *transition*, and a history that gains a row per poll answers
//! nothing.

use chrono::{DateTime, Utc};
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use std::net::IpAddr;
use uuid::Uuid;
use yagra_common::{L3Snapshot, NodeId};

// `node_l3_changes` is written and pruned from the moment ADR-043 shipped, because a history
// cannot be backfilled — the reader can be added later, the recording cannot. The per-node
// reader of the *current* set arrived with its first caller (ADR-157: the Interfaces list and
// `get_node_status` show each port's addresses); `list_changes()` still has none and so still
// does not exist — code with no caller is the thing that rots.
//
/// PostgreSQL-backed store for node interface addresses: current set and change history.
pub struct L3Repo {
    pool: PgPool,
}

impl L3Repo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Record one observed snapshot: upsert the current state and append a history row **iff** the
    /// content key moved.
    ///
    /// Both happen in a single statement. The upsert's `RETURNING` carries the pre-update key and
    /// the append is guarded on `prev IS DISTINCT FROM new`, so an unchanged poll writes no history
    /// at all — which is the whole feature. PostgreSQL's row lock on `ON CONFLICT DO UPDATE`
    /// serializes concurrent cores, so a transition can be neither double-appended nor lost; that
    /// is also why this is not leader-gated.
    ///
    /// The caller must pass a canonicalized snapshot (the poller canonicalizes before publish);
    /// otherwise agent row ordering alone would register as a change.
    ///
    /// Only ever called with a snapshot the poller actually observed. A *failed* walk sends no
    /// snapshot at all, so this is never reached with an empty stand-in — which is what stops one
    /// timed-out walk from erasing a node's addressing, and with it every link that node was in.
    pub async fn record_observation(
        &self,
        node_id: Uuid,
        snapshot: &L3Snapshot,
    ) -> anyhow::Result<()> {
        let key = snapshot.content_key();
        let count = i32::try_from(snapshot.len()).unwrap_or(i32::MAX);
        sqlx::query(
            "WITH up AS ( \
                INSERT INTO node_l3 \
                    (node_id, l3_key, prev_l3_key, addresses, address_count, \
                     truncated, first_seen, last_seen) \
                VALUES ($1, $2, NULL, $3, $4, $5, now(), now()) \
                ON CONFLICT (node_id) DO UPDATE SET \
                    prev_l3_key = node_l3.l3_key, \
                    l3_key = EXCLUDED.l3_key, \
                    addresses = EXCLUDED.addresses, \
                    address_count = EXCLUDED.address_count, \
                    truncated = EXCLUDED.truncated, \
                    first_seen = CASE WHEN node_l3.l3_key = EXCLUDED.l3_key \
                                      THEN node_l3.first_seen ELSE now() END, \
                    last_seen = now() \
                RETURNING prev_l3_key, l3_key \
             ) \
             INSERT INTO node_l3_changes \
                (node_id, at, l3_key, prev_l3_key, addresses, address_count) \
             SELECT $1, now(), up.l3_key, up.prev_l3_key, $3, $4 \
             FROM up WHERE up.prev_l3_key IS DISTINCT FROM up.l3_key",
        )
        .bind(node_id)
        .bind(&key)
        .bind(Json(snapshot))
        .bind(count)
        .bind(snapshot.truncated)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Every node's current snapshot — the derivation task's input (ADR-043).
    ///
    /// Deliberately unpaged: the derivation is a whole-graph computation and cannot produce a
    /// correct answer from a slice of the fleet. The volume is bounded by design — one document per
    /// node, each capped at `MAX_ADDRESSES_PER_NODE` — which at 50k nodes is on the order of tens
    /// of megabytes, and the task runs on the leader at a slow cadence. That is a deliberate
    /// trade, not an oversight: see the memory note on the derivation loop.
    pub async fn all_current(&self) -> anyhow::Result<Vec<(NodeId, L3Snapshot)>> {
        let rows = sqlx::query("SELECT node_id, addresses FROM node_l3")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                let snapshot: Json<L3Snapshot> = row.try_get("addresses")?;
                Ok((NodeId(row.try_get("node_id")?), snapshot.0))
            })
            .collect()
    }

    /// One node's current address set, or `None` when no address walk has ever been recorded for
    /// it (ADR-157).
    ///
    /// `None` and `Some(empty)` are different answers and both are returned as they are: the
    /// first means the walk has not run (or the node has no SNMP), the second means the device
    /// answered the two IP tables with no rows — which is a real observation and replaces the
    /// stored set, exactly as `record_observation` says.
    pub async fn current(&self, node_id: Uuid) -> anyhow::Result<Option<L3Snapshot>> {
        let row = sqlx::query("SELECT addresses FROM node_l3 WHERE node_id = $1")
            .bind(node_id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|row| {
            let snapshot: Json<L3Snapshot> = row.try_get("addresses")?;
            Ok(snapshot.0)
        })
        .transpose()
    }

    /// The newest `last_seen` across every node, or `None` when nothing has been observed.
    ///
    /// This is half the derivation task's change signal. `config_gen` (ADR-026) moves when an
    /// operator edits configuration, which is exactly what a *poll* does not do — so gating the
    /// derivation on `config_gen` alone would leave the map frozen while the network changed
    /// underneath it. The watermark is the other half.
    pub async fn observation_watermark(&self) -> anyhow::Result<Option<DateTime<Utc>>> {
        let row = sqlx::query("SELECT max(last_seen) AS w FROM node_l3")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("w")?)
    }

    /// Every node's host-route addresses (`/32`, `/128`) — the destinations Increment 4's route
    /// probes ask about, and the nodes worth asking (ADR-043).
    ///
    /// This is why Increment 1 stores host routes at all: they are excluded from forming a
    /// *shared-subnet* edge, never from being recorded, precisely so this query can find them.
    ///
    /// The prefix filter runs in SQL so the cost scales with the fleet's point-to-point interfaces
    /// rather than with its whole addressing, and it is deliberately loose — `IN (32, 128)` also
    /// admits a v6 `/32`, which the family check below rejects. Two narrow rules rather than one
    /// clever one: SQL cannot see the address family here and Rust cannot avoid the scan.
    pub async fn host_addresses(
        &self,
    ) -> anyhow::Result<std::collections::BTreeMap<NodeId, std::collections::BTreeSet<IpAddr>>>
    {
        let rows = sqlx::query(
            "SELECT node_id, a->>'ip' AS ip, (a->>'prefix_len')::INT AS prefix_len \
             FROM node_l3, \
                  jsonb_array_elements(coalesce(addresses->'addresses', '[]'::jsonb)) a \
             WHERE (a->>'prefix_len')::INT IN (32, 128)",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut out: std::collections::BTreeMap<NodeId, std::collections::BTreeSet<IpAddr>> =
            std::collections::BTreeMap::new();
        for row in rows {
            let node_id: Uuid = row.try_get("node_id")?;
            let Some(ip) = row
                .try_get::<Option<String>, _>("ip")?
                .and_then(|s| s.parse::<IpAddr>().ok())
            else {
                continue;
            };
            if row.try_get::<Option<i32>, _>("prefix_len")?
                != Some(i32::from(yagra_common::host_prefix_len(ip)))
            {
                continue;
            }
            out.entry(NodeId(node_id)).or_default().insert(ip);
        }
        Ok(out)
    }

    /// The rows of any node's own interface-address list that name one of `addresses` — the
    /// duplicate check's `own_ip` evidence (ADR-148).
    ///
    /// Narrowed in SQL to the addresses asked about, so the answer is sized by the candidates rather
    /// than by the fleet's addressing. A row whose type does not identify its node is dropped here
    /// (`L3AddrType::identifies_a_node`): a VRRP or HSRP virtual address lives on two routers by
    /// design, and would otherwise make them one device.
    pub async fn rows_naming(&self, addresses: &[String]) -> anyhow::Result<Vec<(Uuid, IpAddr)>> {
        let rows = sqlx::query(concat!(
            "SELECT l.node_id, a->>'ip' AS ip, a->>'addr_type' AS addr_type ",
            "FROM node_l3 l, ",
            "jsonb_array_elements(coalesce(l.addresses->'addresses', '[]'::jsonb)) a ",
            "WHERE a->>'ip' = ANY($1::text[])",
        ))
        .bind(addresses)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::new();
        for row in rows {
            let Some(ip) = row
                .try_get::<Option<String>, _>("ip")?
                .and_then(|s| s.parse::<IpAddr>().ok())
            else {
                continue;
            };
            let kind = row
                .try_get::<Option<String>, _>("addr_type")?
                .as_deref()
                .and_then(yagra_common::L3AddrType::from_token)
                .unwrap_or_default();
            if kind.identifies_a_node() {
                out.push((row.try_get("node_id")?, ip));
            }
        }
        Ok(out)
    }

    /// Which of `nodes` have an interface-address list at all (ADR-148). A list that does not name an
    /// address is evidence against `own_ip`; no list is only the absence of evidence.
    pub async fn nodes_with_a_list(
        &self,
        nodes: &[Uuid],
    ) -> anyhow::Result<std::collections::BTreeSet<Uuid>> {
        let rows = sqlx::query("SELECT node_id FROM node_l3 WHERE node_id = ANY($1)")
            .bind(nodes)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| Ok(row.try_get("node_id")?))
            .collect()
    }

    /// Drop history rows older than `retention_secs`. Returns how many were removed.
    pub async fn prune_changes(&self, retention_secs: i64) -> anyhow::Result<u64> {
        let res =
            sqlx::query("DELETE FROM node_l3_changes WHERE at < now() - make_interval(secs => $1)")
                .bind(retention_secs as f64)
                .execute(&self.pool)
                .await?;
        Ok(res.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    /// This module's code, with its test items and comments dropped — the reader every
    /// SQL-shape assertion below uses. The append-on-change rule and the keyset cursor live
    /// entirely inside SQL strings, so nothing else can catch a rewrite that changes their
    /// meaning; the peer stores (`neighbors.rs`, `dns_check.rs`, `events/repo.rs`) all pin their
    /// statements the same way.
    ///
    /// ⚠️ **Read through `module_source`, never `include_str!`** (ADR-102). The raw file includes
    /// this test module, so a positive `contains("<literal>")` was satisfied by the needle's own
    /// line and could not fail. Thirty-two of those were live across seven modules — all of them
    /// here, because the negated side already read this function and only the positive side was
    /// left on the raw text. Loud on one side and silent on the other is why they survived
    /// ADR-091's sweep.
    fn production_source() -> String {
        crate::module_source::code_no_comments("src", "l3")
    }

    #[test]
    fn history_is_appended_only_when_the_content_key_actually_moved() {
        // The whole point of the CTE. Losing this guard turns a once-per-change table into one row
        // per poll per node — and since addresses are walked hourly on every SNMP node in the
        // fleet, that is the difference between a readable timeline and an unusable one.
        assert!(production_source().contains("WHERE up.prev_l3_key IS DISTINCT FROM up.l3_key"));
        // And the append reads the keys the upsert just returned, not the caller's guess.
        assert!(production_source().contains("RETURNING prev_l3_key, l3_key"));
    }

    #[test]
    fn first_seen_survives_an_unchanged_observation() {
        // "How long has this addressing held" is the column's only purpose; resetting it on every
        // poll would make every prefix look brand new.
        assert!(
            production_source().contains("first_seen = CASE WHEN node_l3.l3_key = EXCLUDED.l3_key")
        );
        assert!(production_source().contains("THEN node_l3.first_seen ELSE now() END"));
    }

    /// The per-node reader (`current`, ADR-157) fetches one row by key and pages nothing; the
    /// history reader still does not exist. So the only paging rule to pin is that nobody
    /// reintroduces `OFFSET` when one is added.
    #[test]
    fn no_statement_pages_with_offset() {
        assert!(
            !production_source().contains("OFFSET"),
            "OFFSET paging — rows shift under the reader as history is appended (ADR-019)"
        );
    }

    #[test]
    fn every_statement_binds_its_values_instead_of_interpolating_them() {
        // The cursor and the retention window are caller-supplied and the node id is a path
        // parameter, so none of them may ever be concatenated into a statement.
        let src = production_source();
        for builder in ["format!(", "push_str("] {
            assert!(
                !src.contains(builder),
                "SQL may be being built by string concatenation ({builder}); bind the value instead"
            );
        }
    }

    /// The stored key must be the canonical one, not a re-derivation — otherwise the reader and the
    /// writer could disagree about what counts as a change.
    #[test]
    fn the_stored_key_is_the_models_own_content_key() {
        assert!(production_source().contains("snapshot.content_key()"));
    }

    /// The derivation task's trigger reads a watermark over observations, not just `config_gen`.
    /// If this statement is ever removed, the map silently stops following the network.
    #[test]
    fn an_observation_watermark_exists_for_the_derivation_trigger() {
        assert!(production_source().contains("SELECT max(last_seen) AS w FROM node_l3"));
    }

    /// The host-route filter runs in SQL. Pulling every address into core to find tens of host
    /// routes would make the route-probe plan cost scale with the fleet's addressing rather than
    /// with its point-to-point interfaces — on a 50,000-node fleet, a million JSONB elements per
    /// refresh instead of a filtered scan.
    #[test]
    fn the_host_route_filter_runs_in_sql() {
        assert!(production_source().contains("(a->>'prefix_len')::INT IN (32, 128)"));
    }

    // ── Against a real PostgreSQL (ADR-114) ────────────────────────────────────────────────

    /// The per-node reader answers with exactly what was recorded, distinguishes "never walked"
    /// from "walked and empty", and follows a replacement (ADR-157).
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn current_returns_the_recorded_set_and_none_before_any_walk(pool: sqlx::PgPool) {
        // This module deliberately has no `use super::*` — its other tests read the source as
        // text — so the one test that runs the code names what it needs.
        use super::L3Repo;
        use std::net::IpAddr;
        use yagra_common::{L3Address, L3Snapshot};
        let repo = L3Repo::new(pool.clone());
        let node = crate::pgtest::node(&pool, "l3-current", 1, None).await;
        let other = crate::pgtest::node(&pool, "l3-other", 2, None).await;

        assert_eq!(
            repo.current(node).await.unwrap(),
            None,
            "nothing recorded yet"
        );

        // Two addresses on one port plus one whose prefix could not be decoded — the snapshot
        // comes back byte-for-byte, canonical order and the zero prefix included.
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let first = L3Snapshot::new(vec![
            L3Address::new(71, ip("10.121.1.254"), 24),
            L3Address::new(71, ip("10.104.29.254"), 24),
            L3Address::new(24, ip("fec0::a:0:0:4"), 0),
        ]);
        repo.record_observation(node, &first).await.unwrap();
        assert_eq!(repo.current(node).await.unwrap(), Some(first.clone()));
        assert_eq!(
            repo.current(other).await.unwrap(),
            None,
            "another node's row is not read"
        );

        // An empty observation is a real answer and replaces the set — `Some(empty)`, not `None`.
        repo.record_observation(node, &L3Snapshot::default())
            .await
            .unwrap();
        assert_eq!(
            repo.current(node).await.unwrap(),
            Some(L3Snapshot::default())
        );
    }
}
