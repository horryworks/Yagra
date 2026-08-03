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

/// Default cadence for the neighbour walk. Adjacency changes on the order of months, so this is
/// deliberately two orders of magnitude slower than the metric interval — walking `lldpRemTable` on
/// a 48-port switch every minute would spend device time and rate-limit budget re-reading a
/// constant. (Meraki's inventory tier makes the same call at 21600s.)
pub const DEFAULT_NEIGHBOR_INTERVAL_SECS: u32 = 3600;
/// Floor on the cadence, matching the `CHECK` on `app_settings.neighbor_interval_secs`.
pub const MIN_NEIGHBOR_INTERVAL_SECS: u32 = 300;
/// Ceiling on the cadence. Bounded so the setting cannot be used to effectively disable collection
/// while still reading as enabled — that is what the toggle is for.
pub const MAX_NEIGHBOR_INTERVAL_SECS: u32 = 86_400;

/// Whether a cadence is inside the configurable band. Shared by the API edge and the tests so the
/// bound lives in one place (the shape `config::interval_in_bounds` established); the `CHECK`
/// constraint is the backstop, not the primary guard.
#[must_use]
pub fn interval_in_bounds(secs: u32) -> bool {
    (MIN_NEIGHBOR_INTERVAL_SECS..=MAX_NEIGHBOR_INTERVAL_SECS).contains(&secs)
}

/// How the deployment collects adjacency, as stored on the singleton `app_settings` row.
///
/// Deployment-wide rather than per node or per profile: the OIDs are fixed standards (LLDP-MIB /
/// CISCO-CDP-MIB), so there is nothing to tune per device — only whether to collect and how often.
/// A finer grain (per profile) is a later increment if a fleet ever needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NeighborSettings {
    /// Whether neighbour jobs are scheduled at all.
    pub enabled: bool,
    /// How often each SNMP node's neighbour tables are walked.
    pub interval_secs: u32,
}

impl Default for NeighborSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: DEFAULT_NEIGHBOR_INTERVAL_SECS,
        }
    }
}

impl NeighborSettings {
    /// Whether the cadence is inside its configurable band. The API edge rejects anything else.
    #[must_use]
    pub fn in_bounds(&self) -> bool {
        interval_in_bounds(self.interval_secs)
    }
}

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
    /// The caller must pass a canonicalized set (the poller canonicalizes before publish);
    /// otherwise agent row ordering alone would register as a change.
    ///
    /// Only ever called with a set the poller actually observed. A *failed* walk sends no set at
    /// all, so this is never reached with an empty stand-in — which is what stops one timed-out
    /// walk from erasing a node's adjacency.
    pub async fn record_observation(&self, node_id: Uuid, set: &NeighborSet) -> anyhow::Result<()> {
        let key = set.content_key();
        let count = i32::try_from(set.len()).unwrap_or(i32::MAX);
        sqlx::query(
            "WITH up AS ( \
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
                                      THEN node_neighbors.first_seen ELSE now() END, \
                    last_seen = now() \
                RETURNING prev_neighbor_key, neighbor_key \
             ) \
             INSERT INTO node_neighbor_changes \
                (node_id, at, neighbor_key, prev_neighbor_key, neighbors, neighbor_count) \
             SELECT $1, now(), up.neighbor_key, up.prev_neighbor_key, $3, $4 \
             FROM up WHERE up.prev_neighbor_key IS DISTINCT FROM up.neighbor_key",
        )
        .bind(node_id)
        .bind(&key)
        .bind(Json(set))
        .bind(count)
        .bind(set.truncated)
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
                    "SELECT id, at, neighbors, prev_neighbor_key \
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
                    "SELECT id, at, neighbors, prev_neighbor_key \
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

    /// This module's own source, for the SQL-shape assertions below. The append-on-change rule and
    /// the keyset cursor live entirely inside SQL strings, so nothing else can catch a rewrite that
    /// changes their meaning — the peer stores (`dns_check.rs`, `events.rs`, `flowstore.rs`) all
    /// pin their statements the same way.
    const SRC: &str = include_str!("neighbors.rs");

    /// The executable code above this test module, comments stripped. Without this, a test that
    /// reads its own file matches its own needles (testing.md's self-match trap) and a doc comment
    /// *naming* a banned pattern — "never OFFSET" — reads as the pattern itself.
    fn production_source() -> String {
        SRC.split("#[cfg(test)]")
            .next()
            .expect("split always yields a first element")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn history_is_appended_only_when_the_content_key_actually_moved() {
        // The whole point of the CTE. Losing this guard turns a once-per-change table into one row
        // per poll per node — and since neighbours are polled hourly on every SNMP node in the
        // fleet, that is the difference between a readable timeline and an unusable one.
        assert!(SRC.contains("WHERE up.prev_neighbor_key IS DISTINCT FROM up.neighbor_key"));
        // And the append reads the keys the upsert just returned, not the caller's guess.
        assert!(SRC.contains("RETURNING prev_neighbor_key, neighbor_key"));
    }

    #[test]
    fn first_seen_survives_an_unchanged_observation() {
        // "How long has this wiring held" is the column's only purpose; resetting it on every poll
        // would make every adjacency look brand new.
        assert!(SRC.contains(
            "first_seen = CASE WHEN node_neighbors.neighbor_key = EXCLUDED.neighbor_key"
        ));
        assert!(SRC.contains("THEN node_neighbors.first_seen ELSE now() END"));
    }

    #[test]
    fn change_paging_is_keyset_and_never_offset() {
        // ADR-019. The tuple comparison is what makes the cursor stable across inserts.
        assert!(SRC.contains("WHERE node_id = $1 AND (at, id) < ($2, $3)"));
        assert!(SRC.contains("ORDER BY at DESC, id DESC LIMIT"));
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
        assert!(SRC.contains("set.content_key()"));
    }

    #[test]
    fn the_default_cadence_is_in_bounds_and_slow() {
        let d = NeighborSettings::default();
        assert!(d.enabled, "shipped on by default (ADR-038)");
        assert!(d.in_bounds());
        // A tripwire, not a tautology: dropping the cadence to the metric interval would walk two
        // extra tables on every SNMP node every minute.
        assert_eq!(d.interval_secs, 3600);
    }

    #[test]
    fn the_cadence_band_rejects_the_absurd() {
        assert!(!interval_in_bounds(0));
        assert!(!interval_in_bounds(MIN_NEIGHBOR_INTERVAL_SECS - 1));
        assert!(interval_in_bounds(MIN_NEIGHBOR_INTERVAL_SECS));
        assert!(interval_in_bounds(MAX_NEIGHBOR_INTERVAL_SECS));
        assert!(!interval_in_bounds(MAX_NEIGHBOR_INTERVAL_SECS + 1));
        assert!(!NeighborSettings {
            enabled: true,
            interval_secs: 30
        }
        .in_bounds());
    }
}
