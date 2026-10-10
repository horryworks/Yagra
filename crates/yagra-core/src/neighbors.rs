// SPDX-License-Identifier: AGPL-3.0-only
//! CDP/LLDP adjacency persistence: the current neighbour set per node and the append-on-change
//! history of it (ADR-038, migration 0062).
//!
//! Structured observations, so this is PostgreSQL (store separation) — a chassis id is unbounded
//! device text and `SeriesKey` has no room for it (ADR-011). This is the I/O adapter; the model,
//! its canonicalization and the change key live in `yagra-common` and are tested there. Runtime
//! `sqlx::query` (not the compile-time macro) so the build needs no live database.
//!
//! Modelled on [`crate::dns_check`] down to the CTE: the same "upsert current state, append history
//! only when the content key moved" shape, for the same reason. Adjacency is normally constant, so
//! what an operator wants is the *transition*, and a history that gains a row per poll answers
//! nothing.

use chrono::{DateTime, Utc};
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_common::NeighborSet;

/// The current neighbour set for a node, plus how long it has held.
#[derive(Debug, Clone)]
pub struct CurrentNeighbors {
    /// The set exactly as observed.
    pub set: NeighborSet,
    /// When this exact set was first observed.
    pub first_seen: DateTime<Utc>,
    /// When it was last confirmed still current.
    pub last_seen: DateTime<Utc>,
}

/// One append-on-change history row.
#[derive(Debug, Clone)]
pub struct NeighborChange {
    /// Monotonic id — the keyset cursor tiebreaker.
    pub id: i64,
    /// When the change was recorded.
    pub at: DateTime<Utc>,
    /// The set as of this change.
    pub set: NeighborSet,
    /// The key this replaced; `None` marks the first-ever observation for the node.
    pub prev_neighbor_key: Option<String>,
    /// The producer changed how it spells these rows (ADR-182) — the row records a change of
    /// spelling, not of cabling, though a real change read at the same moment is in it too.
    pub format_changed: bool,
}

/// PostgreSQL-backed store for node adjacency: current set and change history.
pub struct NeighborRepo {
    pool: PgPool,
}

impl NeighborRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Record one observed set: upsert the current state and append a history row **iff** the
    /// content key moved.
    ///
    /// Both happen in a single statement. The upsert's `RETURNING` carries the pre-update key and
    /// the append is guarded on `prev IS DISTINCT FROM new`, so an unchanged poll writes no history
    /// at all — which is the whole feature. PostgreSQL's row lock on `ON CONFLICT DO UPDATE`
    /// serializes concurrent cores, so a transition can be neither double-appended nor lost; that
    /// is also why this is not leader-gated.
    ///
    /// The caller must pass a canonicalized set (the poller canonicalizes before publish, and
    /// core's Meraki sync before it records an MX's or MR's); otherwise agent row ordering alone
    /// would register as a change.
    ///
    /// Only ever called with a set that was actually observed. A *failed* walk or read sends no set
    /// at all, so this is never reached with an empty stand-in — which is what stops one timed-out
    /// walk from erasing a node's adjacency.
    ///
    /// **A change of spelling is not a change of adjacency** (ADR-182). When the key moved and so
    /// did the producer's [`NeighborSet::format`] — or the key's own encoding version, its first
    /// line — the appended row is marked `format_changed` and `first_seen` is kept: the cabling
    /// did not change, the way it is written did. The row is still appended, so a real change read
    /// in the same observation is not lost. ⚠️ The previous format is read in its own CTE, from the
    /// statement's snapshot rather than the locked row; two cores writing one node at the same
    /// instant with two different formats could mark that one row wrongly, and nothing else.
    pub async fn record_observation(&self, node_id: Uuid, set: &NeighborSet) -> anyhow::Result<()> {
        let key = set.content_key();
        let count = i32::try_from(set.len()).unwrap_or(i32::MAX);
        sqlx::query(
            "WITH old AS ( \
                SELECT neighbor_key, COALESCE((neighbors->>'format')::bigint, 0) AS format \
                FROM node_neighbors WHERE node_id = $1 \
             ), up AS ( \
                INSERT INTO node_neighbors \
                    (node_id, neighbor_key, prev_neighbor_key, neighbors, neighbor_count, \
                     truncated, first_seen, last_seen) \
                VALUES ($1, $2, NULL, $3, $4, $5, now(), now()) \
                ON CONFLICT (node_id) DO UPDATE SET \
                    prev_neighbor_key = node_neighbors.neighbor_key, \
                    neighbor_key = EXCLUDED.neighbor_key, \
                    neighbors = EXCLUDED.neighbors, \
                    neighbor_count = EXCLUDED.neighbor_count, \
                    truncated = EXCLUDED.truncated, \
                    first_seen = CASE WHEN node_neighbors.neighbor_key = EXCLUDED.neighbor_key \
                                        OR COALESCE((node_neighbors.neighbors->>'format')::bigint, 0) <> $6 \
                                        OR split_part(node_neighbors.neighbor_key, chr(10), 1) \
                                           <> split_part(EXCLUDED.neighbor_key, chr(10), 1) \
                                      THEN node_neighbors.first_seen ELSE now() END, \
                    last_seen = now() \
                RETURNING prev_neighbor_key, neighbor_key \
             ) \
             INSERT INTO node_neighbor_changes \
                (node_id, at, neighbor_key, prev_neighbor_key, neighbors, neighbor_count, \
                 format_changed) \
             SELECT $1, now(), up.neighbor_key, up.prev_neighbor_key, $3, $4, \
                    COALESCE((SELECT o.format <> $6 \
                                     OR split_part(o.neighbor_key, chr(10), 1) \
                                        <> split_part(up.neighbor_key, chr(10), 1) \
                              FROM old o), FALSE) \
             FROM up WHERE up.prev_neighbor_key IS DISTINCT FROM up.neighbor_key",
        )
        .bind(node_id)
        .bind(&key)
        .bind(Json(set))
        .bind(count)
        .bind(set.truncated)
        .bind(i64::from(set.format))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The node's current neighbour set, if one has ever been observed.
    pub async fn current(&self, node_id: Uuid) -> anyhow::Result<Option<CurrentNeighbors>> {
        let row = sqlx::query(
            "SELECT neighbors, first_seen, last_seen FROM node_neighbors WHERE node_id = $1",
        )
        .bind(node_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let set: Json<NeighborSet> = row.try_get("neighbors")?;
        Ok(Some(CurrentNeighbors {
            set: set.0,
            first_seen: row.try_get("first_seen")?,
            last_seen: row.try_get("last_seen")?,
        }))
    }

    /// Every node's current neighbour set — half the derivation task's input (ADR-043).
    ///
    /// Deliberately unpaged, for the same reason `L3Repo::all_current` is: deriving a graph from a
    /// slice of the fleet produces a wrong graph, not a partial one.
    pub async fn all_current(&self) -> anyhow::Result<Vec<(yagra_common::NodeId, NeighborSet)>> {
        let rows = sqlx::query("SELECT node_id, neighbors FROM node_neighbors")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                let set: Json<NeighborSet> = row.try_get("neighbors")?;
                Ok((yagra_common::NodeId(row.try_get("node_id")?), set.0))
            })
            .collect()
    }

    /// What neighbours report about the devices at the addresses asked about — the duplicate check's
    /// `lldp_chassis` and `cdp_device_id` evidence (ADR-148): each distinct management address,
    /// chassis or device id, and protocol.
    pub async fn reports_about(
        &self,
        addresses: &[String],
    ) -> anyhow::Result<Vec<(std::net::IpAddr, String, yagra_common::NeighborProto)>> {
        let rows = sqlx::query(concat!(
            "SELECT DISTINCT n->>'remote_mgmt_addr' AS addr, n->>'remote_chassis' AS chassis, ",
            "n->>'proto' AS proto ",
            "FROM node_neighbors, ",
            "jsonb_array_elements(coalesce(neighbors->'neighbors', '[]'::jsonb)) n ",
            "WHERE n->>'remote_mgmt_addr' = ANY($1::text[])",
        ))
        .bind(addresses)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                let addr = row
                    .try_get::<Option<String>, _>("addr")
                    .ok()
                    .flatten()?
                    .parse()
                    .ok()?;
                let chassis = row.try_get::<Option<String>, _>("chassis").ok().flatten()?;
                let proto = row.try_get::<Option<String>, _>("proto").ok().flatten()?;
                let proto = serde_json::from_value(serde_json::Value::String(proto)).ok()?;
                Some((addr, chassis, proto))
            })
            .collect())
    }

    /// The newest `last_seen` across every node, or `None` when nothing has been observed.
    ///
    /// Half of the derivation task's change signal — see `L3Repo::observation_watermark` for why
    /// `config_gen` alone is not enough.
    pub async fn observation_watermark(&self) -> anyhow::Result<Option<DateTime<Utc>>> {
        let row = sqlx::query("SELECT max(last_seen) AS w FROM node_neighbors")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("w")?)
    }

    /// A keyset page of change rows, newest first (ADR-019 — never OFFSET).
    ///
    /// `before` is the `(at, id)` of the last row of the previous page.
    pub async fn list_changes(
        &self,
        node_id: Uuid,
        before: Option<(DateTime<Utc>, i64)>,
        limit: i64,
    ) -> anyhow::Result<Vec<NeighborChange>> {
        // Two prepared shapes rather than string-built SQL: the cursor is typed and bound, never
        // interpolated.
        let rows = match before {
            Some((at, id)) => {
                sqlx::query(
                    "SELECT id, at, neighbors, prev_neighbor_key, format_changed \
                     FROM node_neighbor_changes \
                     WHERE node_id = $1 AND (at, id) < ($2, $3) \
                     ORDER BY at DESC, id DESC LIMIT $4",
                )
                .bind(node_id)
                .bind(at)
                .bind(id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "SELECT id, at, neighbors, prev_neighbor_key, format_changed \
                     FROM node_neighbor_changes \
                     WHERE node_id = $1 \
                     ORDER BY at DESC, id DESC LIMIT $2",
                )
                .bind(node_id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?
            }
        };

        rows.into_iter()
            .map(|row| {
                let set: Json<NeighborSet> = row.try_get("neighbors")?;
                Ok(NeighborChange {
                    id: row.try_get("id")?,
                    at: row.try_get("at")?,
                    set: set.0,
                    prev_neighbor_key: row.try_get("prev_neighbor_key")?,
                    format_changed: row.try_get("format_changed")?,
                })
            })
            .collect()
    }

    /// Drop history rows older than `retention_secs`. Returns how many were removed.
    pub async fn prune_changes(&self, retention_secs: i64) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "DELETE FROM node_neighbor_changes WHERE at < now() - make_interval(secs => $1)",
        )
        .bind(retention_secs as f64)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This module's code, with its test items and comments dropped — the reader every
    /// SQL-shape assertion below uses. The append-on-change rule and the keyset cursor live
    /// entirely inside SQL strings, so nothing else can catch a rewrite that changes their
    /// meaning; the peer stores (`dns_check.rs`, `events/repo.rs`, `flowstore.rs`) all pin their
    /// statements the same way.
    ///
    /// ⚠️ **Read through `module_source`, never `include_str!`** (ADR-102). The raw file includes
    /// this test module, so a positive `contains("<literal>")` was satisfied by the needle's own
    /// line and could not fail. Thirty-two of those were live across seven modules — all of them
    /// here, because the negated side already read this function and only the positive side was
    /// left on the raw text. Loud on one side and silent on the other is why they survived
    /// ADR-091's sweep.
    fn production_source() -> String {
        crate::module_source::code_no_comments("src", "neighbors")
    }

    #[test]
    fn history_is_appended_only_when_the_content_key_actually_moved() {
        // The whole point of the CTE. Losing this guard turns a once-per-change table into one row
        // per poll per node — and since neighbours are polled hourly on every SNMP node in the
        // fleet, that is the difference between a readable timeline and an unusable one.
        assert!(production_source()
            .contains("WHERE up.prev_neighbor_key IS DISTINCT FROM up.neighbor_key"));
        // And the append reads the keys the upsert just returned, not the caller's guess.
        assert!(production_source().contains("RETURNING prev_neighbor_key, neighbor_key"));
    }

    #[test]
    fn first_seen_survives_an_unchanged_observation() {
        // "How long has this wiring held" is the column's only purpose; resetting it on every poll
        // would make every adjacency look brand new.
        assert!(production_source().contains(
            "first_seen = CASE WHEN node_neighbors.neighbor_key = EXCLUDED.neighbor_key"
        ));
        assert!(production_source().contains("THEN node_neighbors.first_seen ELSE now() END"));
    }

    #[test]
    fn change_paging_is_keyset_and_never_offset() {
        // ADR-019. The tuple comparison is what makes the cursor stable across inserts.
        assert!(production_source().contains("WHERE node_id = $1 AND (at, id) < ($2, $3)"));
        assert!(production_source().contains("ORDER BY at DESC, id DESC LIMIT"));
        assert!(
            !production_source().contains("OFFSET"),
            "OFFSET paging reintroduced — rows shift under the reader as history is appended"
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
        assert!(production_source().contains("set.content_key()"));
    }

    // --- Running the SQL, not reading it (ADR-114/116) -----------------------------------------
    //
    // The checks above read this module's text — which is the only way to state the
    // append-on-change rule, because it lives inside one CTE. What text cannot say is whether the
    // statement does what the words claim, or whether the reader's projection still names every
    // column the writer writes. Both below.
    use yagra_base::pgtest;
    use yagra_common::{Neighbor, NeighborCapability, NeighborProto};

    /// One adjacency with every optional field filled in, so a payload column that stopped
    /// travelling is visible rather than defaulted away.
    fn full_neighbor(local: &str, chassis: &str) -> Neighbor {
        let mut n = Neighbor::new(NeighborProto::Lldp, local, chassis, "Gi1/1");
        n.local_ifindex = Some(7);
        n.remote_port_desc = Some("uplink to core".to_owned());
        n.remote_sys_name = Some("core-sw-01".to_owned());
        n.remote_sys_desc = Some("Cisco IOS".to_owned());
        n.remote_mgmt_addr = Some("10.0.0.9".to_owned());
        n.remote_platform = Some("C9300".to_owned());
        n.capabilities = vec![NeighborCapability::Bridge, NeighborCapability::Router];
        n
    }

    /// A set goes in, comes back whole, and leaves exactly one history row behind — the first
    /// observation, which replaced nothing.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_observed_set_reads_back_with_its_payload_and_appends_its_first_change(
        pool: sqlx::PgPool,
    ) {
        let node = pgtest::node(&pool, "sw", 1, None).await;
        let repo = NeighborRepo::new(pool.clone());
        let set = NeighborSet::new(vec![full_neighbor("Gi0/1", "aa:bb:cc:00:00:01")], 0);
        repo.record_observation(node, &set).await.expect("record");

        let current = repo
            .current(node)
            .await
            .expect("current")
            .expect("the set just recorded");
        assert_eq!(
            current.set, set,
            "the observed set did not survive the JSONB round trip"
        );
        let stored = &current.set.neighbors[0];
        assert_eq!(stored.remote_sys_name.as_deref(), Some("core-sw-01"));
        assert_eq!(stored.remote_mgmt_addr.as_deref(), Some("10.0.0.9"));
        assert_eq!(stored.capabilities.len(), 2);
        assert_eq!(
            current.first_seen, current.last_seen,
            "a set observed once already looks re-confirmed"
        );

        assert_eq!(pgtest::rows(&pool, "node_neighbors").await, 1);
        let changes = repo.list_changes(node, None, 10).await.expect("changes");
        assert_eq!(
            changes.len(),
            1,
            "the first observation appended no history"
        );
        assert_eq!(
            changes[0].prev_neighbor_key, None,
            "the first-ever observation must name no predecessor"
        );
        assert_eq!(changes[0].set, set);
    }

    /// **The feature.** Adjacency is normally constant, so an unchanged poll must write no history
    /// at all — and must not move `first_seen`, which is how long the adjacency has held.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_unchanged_observation_appends_no_history_and_keeps_first_seen(pool: sqlx::PgPool) {
        let node = pgtest::node(&pool, "sw", 1, None).await;
        let repo = NeighborRepo::new(pool.clone());
        let set = NeighborSet::new(vec![full_neighbor("Gi0/1", "aa:bb:cc:00:00:01")], 0);

        repo.record_observation(node, &set).await.expect("record");
        let first = repo.current(node).await.expect("current").expect("a set");
        repo.record_observation(node, &set)
            .await
            .expect("re-record");
        let second = repo.current(node).await.expect("current").expect("a set");

        assert_eq!(
            pgtest::rows(&pool, "node_neighbor_changes").await,
            1,
            "an unchanged observation appended a history row — the history gains a row per poll"
        );
        assert_eq!(
            second.first_seen, first.first_seen,
            "first_seen moved on an unchanged set, so 'how long has this held' is now wrong"
        );
        assert!(
            second.last_seen > first.last_seen,
            "last_seen did not move, so the set no longer reads as still current"
        );
    }

    /// A real transition appends exactly one row, and that row names the key it replaced.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_changed_set_appends_a_row_naming_the_key_it_replaced(pool: sqlx::PgPool) {
        let node = pgtest::node(&pool, "sw", 1, None).await;
        let repo = NeighborRepo::new(pool.clone());
        let before = NeighborSet::new(vec![full_neighbor("Gi0/1", "aa:bb:cc:00:00:01")], 0);
        let after = NeighborSet::new(vec![full_neighbor("Gi0/1", "aa:bb:cc:00:00:02")], 0);

        repo.record_observation(node, &before).await.expect("first");
        let held = repo.current(node).await.expect("current").expect("a set");
        repo.record_observation(node, &after).await.expect("second");

        let changes = repo.list_changes(node, None, 10).await.expect("changes");
        assert_eq!(changes.len(), 2, "the transition appended no history row");
        assert_eq!(changes[0].set, after, "history is not newest-first");
        assert_eq!(
            changes[0].prev_neighbor_key.as_deref(),
            Some(before.content_key().as_str()),
            "the change row does not name the set it replaced"
        );

        let now = repo.current(node).await.expect("current").expect("a set");
        assert_eq!(now.set, after);
        assert!(
            now.first_seen > held.first_seen,
            "first_seen did not restart when the set actually changed"
        );
        assert!(
            !changes[0].format_changed && !changes[1].format_changed,
            "a change at one format was marked as a change of spelling"
        );
    }

    /// ADR-182: the producer respells its rows and raises its format. The row is still appended —
    /// a real change read at the same moment must not vanish — but it says it is a change of
    /// spelling, and `first_seen` keeps counting from the cabling's own first sight.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_respelled_set_is_marked_and_keeps_first_seen(pool: sqlx::PgPool) {
        let node = pgtest::node(&pool, "sw", 1, None).await;
        let repo = NeighborRepo::new(pool.clone());
        let before = NeighborSet::new(vec![full_neighbor("7", "aa:bb:cc:00:00:01")], 0);
        let after = NeighborSet::new(vec![full_neighbor("Port 7", "aa:bb:cc:00:00:01")], 1);

        repo.record_observation(node, &before).await.expect("first");
        let held = repo.current(node).await.expect("current").expect("a set");
        repo.record_observation(node, &after).await.expect("second");

        let changes = repo.list_changes(node, None, 10).await.expect("changes");
        assert_eq!(
            changes.len(),
            2,
            "the respelled set appended no history row"
        );
        assert!(
            changes[0].format_changed,
            "the row a raised format appended is not marked"
        );
        assert!(!changes[1].format_changed, "the genesis row is marked");
        let now = repo.current(node).await.expect("current").expect("a set");
        assert_eq!(now.set.format, 1, "the stored set lost its format");
        assert_eq!(
            now.first_seen, held.first_seen,
            "first_seen restarted although only the spelling moved"
        );

        // Same format, same key: nothing more. Same format, new key: an ordinary change again.
        repo.record_observation(node, &after).await.expect("third");
        let moved = NeighborSet::new(vec![full_neighbor("Port 8", "aa:bb:cc:00:00:01")], 1);
        repo.record_observation(node, &moved).await.expect("fourth");
        let changes = repo.list_changes(node, None, 10).await.expect("changes");
        assert_eq!(
            changes.len(),
            3,
            "an unchanged read appended, or a change did not"
        );
        assert!(
            !changes[0].format_changed,
            "a change at an unchanged format was marked as a respelling"
        );
    }

    /// A format raised for a producer whose rows did not change leaves no row at all: the key did
    /// not move, and there is nothing to explain.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_raised_format_with_the_same_rows_appends_nothing(pool: sqlx::PgPool) {
        let node = pgtest::node(&pool, "sw", 1, None).await;
        let repo = NeighborRepo::new(pool.clone());
        let rows = vec![full_neighbor("Gi0/1", "aa:bb:cc:00:00:01")];
        repo.record_observation(node, &NeighborSet::new(rows.clone(), 0))
            .await
            .expect("first");
        repo.record_observation(node, &NeighborSet::new(rows, 1))
            .await
            .expect("second");
        let changes = repo.list_changes(node, None, 10).await.expect("changes");
        assert_eq!(
            changes.len(),
            1,
            "a format change with identical rows appended a row"
        );
    }

    /// The derivation task's two reads: every node's current set, unpaged, and the newest
    /// observation across the fleet.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn every_nodes_current_set_comes_back_and_the_watermark_names_the_newest(
        pool: sqlx::PgPool,
    ) {
        let repo = NeighborRepo::new(pool.clone());
        assert!(
            repo.observation_watermark()
                .await
                .expect("watermark")
                .is_none(),
            "a fresh database reported an observation that never happened"
        );

        let a = pgtest::node(&pool, "a", 1, None).await;
        let b = pgtest::node(&pool, "b", 2, None).await;
        let set_a = NeighborSet::new(vec![full_neighbor("Gi0/1", "aa:bb:cc:00:00:01")], 0);
        let set_b = NeighborSet::new(vec![full_neighbor("Gi0/2", "aa:bb:cc:00:00:02")], 0);
        repo.record_observation(a, &set_a).await.expect("record a");
        repo.record_observation(b, &set_b).await.expect("record b");

        let all = repo.all_current().await.expect("all_current");
        assert_eq!(all.len(), 2, "the unpaged read did not return every node");
        assert_eq!(
            all.iter().find(|(id, _)| id.0 == a).map(|(_, s)| s),
            Some(&set_a),
            "a node's set came back attached to the wrong node, or not at all"
        );
        assert_eq!(
            all.iter().find(|(id, _)| id.0 == b).map(|(_, s)| s),
            Some(&set_b)
        );

        let newest = repo
            .current(b)
            .await
            .expect("current")
            .expect("b's set")
            .last_seen;
        assert_eq!(
            repo.observation_watermark().await.expect("watermark"),
            Some(newest),
            "the watermark is not the newest last_seen in the table"
        );
    }

    /// The cursor branch. A page of one walks the history newest-first, exactly once each, and
    /// stops.
    ///
    /// ⚠️ The bounded loop is part of the assertion: a `(at, id) < ($2, $3)` that stopped being
    /// applied would hand back the same newest row forever, and an unbounded `loop` would hang
    /// instead of failing.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn change_paging_walks_the_history_newest_first_and_stops(pool: sqlx::PgPool) {
        let node = pgtest::node(&pool, "sw", 1, None).await;
        let repo = NeighborRepo::new(pool.clone());
        for chassis in [
            "aa:bb:cc:00:00:01",
            "aa:bb:cc:00:00:02",
            "aa:bb:cc:00:00:03",
        ] {
            repo.record_observation(
                node,
                &NeighborSet::new(vec![full_neighbor("Gi0/1", chassis)], 0),
            )
            .await
            .expect("record");
        }
        assert_eq!(pgtest::rows(&pool, "node_neighbor_changes").await, 3);

        let mut seen: Vec<(chrono::DateTime<chrono::Utc>, i64)> = Vec::new();
        let mut before: Option<(chrono::DateTime<chrono::Utc>, i64)> = None;
        for _ in 0..8 {
            let page = repo.list_changes(node, before, 1).await.expect("changes");
            let Some(change) = page.first() else { break };
            assert_eq!(page.len(), 1, "LIMIT is not being applied");
            seen.push((change.at, change.id));
            before = Some((change.at, change.id));
        }
        assert_eq!(
            seen.len(),
            3,
            "the cursor walk did not end after the three change rows: {seen:?}"
        );
        let mut descending = seen.clone();
        descending.sort_unstable();
        descending.reverse();
        descending.dedup();
        assert_eq!(
            descending, seen,
            "the history did not come back newest-first, or a row came back twice: {seen:?}"
        );
    }

    /// History is pruned by age. A row written a moment ago survives an hour-long retention window
    /// and does not survive a zero-length one.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn pruning_removes_history_outside_the_window_and_keeps_what_is_inside(
        pool: sqlx::PgPool,
    ) {
        let node = pgtest::node(&pool, "sw", 1, None).await;
        let repo = NeighborRepo::new(pool.clone());
        repo.record_observation(
            node,
            &NeighborSet::new(vec![full_neighbor("Gi0/1", "aa:bb:cc:00:00:01")], 0),
        )
        .await
        .expect("record");

        assert_eq!(
            repo.prune_changes(3600).await.expect("prune"),
            0,
            "a change recorded a moment ago was pruned by an hour-long window"
        );
        assert_eq!(pgtest::rows(&pool, "node_neighbor_changes").await, 1);

        assert_eq!(repo.prune_changes(0).await.expect("prune"), 1);
        assert_eq!(pgtest::rows(&pool, "node_neighbor_changes").await, 0);
        assert!(
            repo.current(node).await.expect("current").is_some(),
            "pruning the history also removed the current set"
        );
    }
}
