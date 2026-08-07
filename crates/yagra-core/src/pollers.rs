// SPDX-License-Identifier: AGPL-3.0-only
//! Durable poller inventory persistence (ADR-009).
//!
//! The `pollers` table is the *durable* record of which pollers have ever registered — first/last
//! seen, and the pool/version/incarnation of their most recent heartbeat. It is what lets the
//! Pollers view show a poller that is currently **offline** (its live liveness/assignment live in
//! Redis, ADR-004, and vanish on TTL expiry). This is the I/O adapter only; the coordinator
//! decides *when* to upsert (throttled, so a 10s heartbeat doesn't write every beat) and how the
//! rows are merged with the live view for the API — both in a later step.
//!
//! Runtime `sqlx::query` (not the compile-time macro) so the build needs no live database, and all
//! inputs are bound parameters (security.md). Mirrors the [`crate::audit`] repo's shape.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// One `pollers` row (API shape). Timestamps are RFC 3339 text at the edge, matching how the
/// audit repo exposes its `at` column.
#[derive(Debug, Clone, Serialize)]
pub struct PollerRow {
    /// Sanitized poller id (the NATS-subject-safe identifier, stable across restarts).
    pub id: String,
    /// Pool the poller last reported serving.
    pub pool: String,
    /// When this poller was first seen (RFC 3339).
    pub first_seen: String,
    /// When this poller was last seen (RFC 3339).
    pub last_seen: String,
    /// Build version from its most recent heartbeat, if reported.
    pub last_version: Option<String>,
    /// Per-process incarnation from its most recent heartbeat, if reported.
    pub last_incarnation: Option<Uuid>,
    /// Interface addresses the poller reported for itself (ADR-043). Empty from an N-1 poller, and
    /// empty for a containerized poller whose only address is a bridge address — the two cases are
    /// answered the same way, by naming `anchor_node_id`.
    pub mgmt_addrs: Vec<String>,
    /// The node an operator named as this poller's attachment point, rooting the derived dependency
    /// graph. `None` means core places the poller from `mgmt_addrs` instead — or, if that matches
    /// nothing, that the poller is unplaced and derived suppression stays blocked.
    pub anchor_node_id: Option<Uuid>,
}

/// One `monitoring_gaps` row (API shape). A gap is one core↔poller **visibility outage**: core
/// stopped hearing from the poller (partition or the poller went down) and later saw it again. If the
/// poller was alive but partitioned, its store-and-forward buffer backfills the metrics for the
/// window on reconnect (Phase 3); alerts are *not* backfilled (they resume from "now").
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct MonitoringGapRow {
    /// Row id.
    pub id: Uuid,
    /// The poller whose visibility lapsed.
    pub poller_id: String,
    /// Pool it serves.
    pub pool: String,
    /// Start of the gap window (RFC 3339 — core's last contact before the outage).
    pub started_at: String,
    /// End of the gap window (RFC 3339 — core heard from it again).
    pub ended_at: String,
    /// Gap length in seconds (UI convenience).
    pub duration_secs: i64,
    /// When core recorded the gap (RFC 3339).
    pub recorded_at: String,
    /// Passive listeners the poller had bound when the gap began (e.g. `syslog:514`, `trap:162`).
    ///
    /// Empty ⇒ the poller had none, so the gap cost no passive data. Non-empty ⇒ whatever those
    /// listeners would have received in the window is **gone**: syslog, traps and flow exports are
    /// fire-and-forget, so unlike active polling there is no buffer to backfill from. (SNMP informs
    /// are the exception — the sender retries until acknowledged.)
    pub listeners: Vec<String>,
}

/// PostgreSQL-backed durable poller inventory (`pollers`).
pub struct PollerRepo {
    pool: PgPool,
}

impl PollerRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Record that a poller was seen: insert it (first contact) or refresh `last_seen` and the
    /// pool/version/incarnation/addresses of its latest heartbeat. `first_seen` is preserved on
    /// update, and so is `anchor_node_id` — that column is the operator's, not the poller's, and a
    /// heartbeat overwriting it would silently undo the fix for an unplaceable poller on the next
    /// beat.
    ///
    /// Call-site throttling (so a 10s heartbeat isn't a write per beat) is the coordinator's job.
    ///
    /// Addresses travel as text and are cast server-side: `INET` has no `sqlx` codec compiled in
    /// here (the node table reads its address through `host()` for the same reason), and the cast
    /// still makes PostgreSQL reject a malformed address rather than storing it.
    pub async fn upsert_seen(
        &self,
        id: &str,
        pool: &str,
        version: &str,
        incarnation: Uuid,
        mgmt_addrs: &[String],
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO pollers (id, pool, last_version, last_incarnation, mgmt_addrs) \
             VALUES ($1, $2, $3, $4, $5::text[]::inet[]) \
             ON CONFLICT (id) DO UPDATE SET \
               last_seen = now(), \
               pool = EXCLUDED.pool, \
               last_version = EXCLUDED.last_version, \
               last_incarnation = EXCLUDED.last_incarnation, \
               mgmt_addrs = EXCLUDED.mgmt_addrs",
        )
        .bind(id)
        .bind(pool)
        .bind(version)
        .bind(incarnation)
        .bind(mgmt_addrs)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Point a poller at the node it attaches to, or clear it (`None`).
    ///
    /// Separate from [`Self::upsert_seen`] on purpose: this column is written by an operator and
    /// that one by a heartbeat arriving every ten seconds. One statement writing both would make the
    /// poller the last writer, and the anchor would revive its old value within one beat.
    pub async fn set_anchor(&self, id: &str, node_id: Option<Uuid>) -> anyhow::Result<bool> {
        let res = sqlx::query("UPDATE pollers SET anchor_node_id = $2 WHERE id = $1")
            .bind(id)
            .bind(node_id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Every poller in the inventory, ordered by id.
    pub async fn list(&self) -> anyhow::Result<Vec<PollerRow>> {
        let rows = sqlx::query(
            "SELECT id, pool, first_seen, last_seen, last_version, last_incarnation, \
                    anchor_node_id, \
                    ARRAY(SELECT host(a) FROM unnest(mgmt_addrs) AS a) AS mgmt_addrs \
             FROM pollers ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let first_seen: DateTime<Utc> = row.try_get("first_seen")?;
                let last_seen: DateTime<Utc> = row.try_get("last_seen")?;
                Ok(PollerRow {
                    id: row.try_get("id")?,
                    pool: row.try_get("pool")?,
                    first_seen: first_seen.to_rfc3339(),
                    last_seen: last_seen.to_rfc3339(),
                    last_version: row.try_get("last_version")?,
                    last_incarnation: row.try_get("last_incarnation")?,
                    mgmt_addrs: row.try_get("mgmt_addrs")?,
                    anchor_node_id: row.try_get("anchor_node_id")?,
                })
            })
            .collect()
    }

    /// Delete a poller by id (operator removing a decommissioned poller). Returns whether a row
    /// was removed.
    pub async fn delete(&self, id: &str) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM pollers WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Record one detected monitoring gap (a known poller reappeared after being offline). `started`
    /// and `ended` are Unix milliseconds. Best-effort: a failed insert just means the gap isn't
    /// listed. The coordinator calls this once per offline→online transition (one row per gap).
    pub async fn insert_monitoring_gap(
        &self,
        poller_id: &str,
        pool: &str,
        started_ms: i64,
        ended_ms: i64,
        listeners: &[String],
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO monitoring_gaps \
             (poller_id, pool, started_at_unix_ms, ended_at_unix_ms, listeners) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(poller_id)
        .bind(pool)
        .bind(started_ms)
        .bind(ended_ms)
        .bind(listeners.join(","))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Drop recorded gaps older than `retention_secs`.
    ///
    /// `retention::Subject::MonitoringGaps`, on the **alert-linked** window rather than a window of
    /// its own: a gap says "no alert fired here because monitoring was blind", which is only
    /// readable beside the alert history it explains. A poller that flaps writes one row per
    /// transition, so before this nothing bounded the table.
    pub async fn prune_monitoring_gaps(&self, retention_secs: i64) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "DELETE FROM monitoring_gaps WHERE recorded_at < now() - make_interval(secs => $1)",
        )
        .bind(retention_secs as f64)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// The most recent monitoring gaps, newest first (capped). Powers the Pollers page's "Recent
    /// monitoring gaps" section.
    pub async fn list_monitoring_gaps(&self, limit: i64) -> anyhow::Result<Vec<MonitoringGapRow>> {
        let rows = sqlx::query(
            "SELECT id, poller_id, pool, started_at_unix_ms, ended_at_unix_ms, listeners, \
             recorded_at FROM monitoring_gaps ORDER BY recorded_at DESC LIMIT $1",
        )
        .bind(limit.clamp(1, 1000))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let started: i64 = row.try_get("started_at_unix_ms")?;
                let ended: i64 = row.try_get("ended_at_unix_ms")?;
                let recorded_at: DateTime<Utc> = row.try_get("recorded_at")?;
                Ok(MonitoringGapRow {
                    id: row.try_get("id")?,
                    poller_id: row.try_get("poller_id")?,
                    pool: row.try_get("pool")?,
                    started_at: ms_to_rfc3339(started),
                    ended_at: ms_to_rfc3339(ended),
                    duration_secs: (ended - started).max(0) / 1000,
                    recorded_at: recorded_at.to_rfc3339(),
                    listeners: row
                        .try_get::<String, _>("listeners")?
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect(),
                })
            })
            .collect()
    }
}

/// Format Unix milliseconds as RFC 3339 UTC (matching how the rest of this repo exposes timestamps).
fn ms_to_rfc3339(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = include_str!("pollers.rs");

    /// Executable code above the tests, comments stripped — see `dns_check.rs` for why both.
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
    fn a_timestamp_renders_as_rfc3339_utc() {
        assert_eq!(ms_to_rfc3339(0), "1970-01-01T00:00:00+00:00");
        assert!(ms_to_rfc3339(1_700_000_000_000).starts_with("2023-11-14T"));
    }

    #[test]
    fn an_unrepresentable_timestamp_renders_empty_rather_than_panicking() {
        // The column is an i64 written by a poller, so a clock fault or a corrupt row can land
        // outside what a DateTime can hold. The Pollers page showing a blank cell beats core
        // panicking on a list read.
        assert_eq!(ms_to_rfc3339(i64::MAX), "");
        assert_eq!(ms_to_rfc3339(i64::MIN), "");
    }

    #[test]
    fn the_gap_insert_and_read_agree_on_the_listeners_column() {
        // The column is written by one SQL string and read by another; nothing makes them agree,
        // and a mismatch means rows the writer produces are rows the reader cannot parse. Both
        // needles are assembled at runtime so this test does not match its own source.
        let src = production_source();
        let col = format!("{}s", "listener");
        assert!(
            src.contains(&format!("ended_at_unix_ms, {col})")),
            "insert omits the column"
        );
        assert!(
            src.contains(&format!("ended_at_unix_ms, {col}, ")),
            "select omits the column"
        );
    }
    #[test]
    fn a_gaps_duration_is_never_negative() {
        // `started`/`ended` are both poller-supplied, so a clock stepping backwards mid-gap would
        // otherwise report a negative outage — which reads as nonsense in the UI and sorts wrong.
        let duration = |started: i64, ended: i64| (ended - started).max(0) / 1000;
        assert_eq!(duration(1_000, 61_000), 60);
        assert_eq!(duration(61_000, 1_000), 0);
        assert_eq!(duration(1_000, 1_000), 0);
        // Sub-second gaps floor to zero rather than rounding up to a second that did not happen.
        assert_eq!(duration(1_000, 1_999), 0);
        assert!(production_source().contains("(ended - started).max(0) / 1000"));
    }

    #[test]
    fn the_inventory_lists_deterministically_and_the_gap_log_newest_first() {
        // Without an ORDER BY the same page can come back in a different order each fetch, which
        // reads as pollers shuffling on every refresh.
        let src = production_source();
        assert!(src.contains("FROM pollers ORDER BY id"));
        assert!(src.contains("FROM monitoring_gaps ORDER BY recorded_at DESC LIMIT $1"));
    }

    #[test]
    fn heartbeats_upsert_rather_than_accumulating_a_row_per_beat() {
        // A poller heartbeats continuously; an INSERT per beat would grow the table without bound.
        let src = production_source();
        assert!(src.contains("ON CONFLICT"));
        assert!(src.contains("last_seen = now()"));
    }

    /// **The anchor is the operator's column.** A heartbeat arrives every ten seconds; if its
    /// upsert touched `anchor_node_id`, the fix an operator just applied to an unplaceable poller
    /// would be reverted within one beat and derived suppression would stay blocked with no
    /// indication why. The two writers are separate statements, and this pins that.
    #[test]
    fn a_heartbeat_upsert_never_writes_the_operator_set_anchor() {
        let src = production_source();
        let col = format!("anchor_node{}", "_id");
        let update = src
            .split("ON CONFLICT (id) DO UPDATE SET")
            .nth(1)
            .expect("the heartbeat upsert's update clause");
        let clause = update.split("\",").next().unwrap_or(update);
        assert!(
            !clause.contains(&col),
            "the heartbeat is overwriting the anchor: {clause}"
        );
        assert!(
            !update
                .split("\",")
                .next()
                .unwrap_or(update)
                .contains("first_seen"),
            "first_seen must survive an upsert too"
        );
        // And the operator's writer exists and touches only that column.
        assert!(src.contains(&format!("UPDATE pollers SET {col} = $2 WHERE id = $1")));
    }

    #[test]
    fn addresses_are_stored_as_inet_so_a_malformed_one_is_rejected_at_write_time() {
        // Bound as text and cast, because there is no INET codec compiled in — but the cast is what
        // makes PostgreSQL reject `"not-an-address"` instead of storing it and failing later inside
        // anchor resolution, where the failure would be silent.
        let src = production_source();
        assert!(src.contains("$5::text[]::inet[]"));
        assert!(src.contains("ARRAY(SELECT host(a) FROM unnest(mgmt_addrs) AS a)"));
    }

    #[test]
    fn every_statement_binds_its_values_instead_of_interpolating_them() {
        // The poller id is caller-supplied (`YAGRA_POLLER_ID`) and reaches this store unvalidated.
        let src = production_source();
        for builder in ["format!(", "push_str("] {
            assert!(
                !src.contains(builder),
                "SQL may be being built by string concatenation ({builder}); bind the value instead"
            );
        }
    }
}
