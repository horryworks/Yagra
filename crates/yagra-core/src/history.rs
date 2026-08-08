// SPDX-License-Identifier: AGPL-3.0-only
//! Alert history persistence (Workstream #3).
//!
//! Appends one row per alert fire/resolve so the lifecycle is durable beyond the in-memory
//! active set. Live-only (PostgreSQL); the read endpoint returns an empty list in skeleton
//! mode.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_alert::Alert;
use yagra_common::{Direction, NodeId, NodeState, Severity};

/// Count an alert dropped from the history because its subject is not a node.
///
/// `alert_history` is keyed by node id in its schema, its scope predicate and its readers, so a
/// non-node subject has nowhere to go until that column becomes a subject (Increment 2). Counted
/// rather than logged per occurrence: pool-coverage alerts recur on a tick, and a warning per tick
/// is how a real signal gets tuned out. A non-zero value means History is not the whole story.
fn non_node_skipped() {
    metrics::counter!("yagra_alert_history_non_node_skipped_total").increment(1);
}

/// One alert-history row for the API.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct AlertHistoryRow {
    pub node: Uuid,
    pub check: Uuid,
    pub severity: Severity,
    pub state: NodeState,
    pub at_unix_ms: i64,
    pub resolved: bool,
    /// Metric the check measured (e.g. `icmp_rtt_ms`, or the liveness sentinel). `None` for
    /// rows recorded before this was captured (legacy) so the WebUI can show "—".
    pub metric: Option<String>,
    /// Observed sample value that committed the transition (threshold checks only).
    pub observed_value: Option<f64>,
    /// The bound crossed for the committed severity (threshold checks only).
    pub threshold_value: Option<f64>,
    /// Which way the metric crossed its bound (threshold checks only).
    pub direction: Option<Direction>,
    /// Insertion time as an RFC 3339 timestamp. This is the **keyset cursor**: the WebUI passes
    /// the last row's `recorded_at` as `before` to fetch the next (older) page (matches the audit
    /// log's paging). Distinct from `at_unix_ms` (the event time), which can collide across rows.
    pub recorded_at: String,
}

/// The four alert fields whose column value is a *decision* rather than a copy: a liveness check
/// has no metric worth storing and no breach, so each becomes NULL.
///
/// Shared by both writers so the two cannot drift on what an empty metric or an absent breach
/// means — the failure mode being one writer storing `""` where the other stores NULL, which the
/// reader then renders as a blank metric name instead of "—".
fn breach_columns(alert: &Alert) -> (Option<String>, Option<f64>, Option<f64>, Option<Direction>) {
    let (value, threshold, direction) = match &alert.breach {
        Some(b) => (Some(b.value), b.threshold, Some(b.direction)),
        None => (None, None, None),
    };
    // The liveness sentinel is stored as NULL, not as its internal token: the column is what the
    // operator reads as "what fired".
    let metric = (!alert.metric.is_empty()).then(|| alert.metric.clone());
    (metric, value, threshold, direction)
}

/// PostgreSQL-backed alert history.
pub struct AlertHistoryStore {
    pool: PgPool,
}

impl AlertHistoryStore {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Append a fire (`resolved=false`) or recovery (`resolved=true`) record. Captures the
    /// metric (and numeric breach detail, if a threshold check) so the history is human-readable.
    ///
    /// An alert whose subject is not a node is **not recorded** and is not an error.
    pub async fn record(&self, alert: &Alert, resolved: bool) -> anyhow::Result<()> {
        let Some(node) = alert.node() else {
            non_node_skipped();
            return Ok(());
        };
        let (metric, value, threshold, direction) = breach_columns(alert);
        sqlx::query(
            "INSERT INTO alert_history \
             (id, node, check_id, severity, state, at_unix_ms, resolved, \
              metric, observed_value, threshold_value, direction) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(Uuid::new_v4())
        .bind(node.as_uuid())
        .bind(alert.check.as_uuid())
        .bind(alert.severity.as_str())
        .bind(alert.state.as_str())
        .bind(alert.at_unix_ms)
        .bind(resolved)
        .bind(metric)
        .bind(value)
        .bind(threshold)
        .bind(direction.map(Direction::as_str))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Append a **batch** of fire/resolve records in one multi-row INSERT (the async ingest writer,
    /// ADR-025 — mirrors the event pipeline's batch writer). Runs off the matcher's hot path; a DB
    /// hiccup must not stop alerting. 11 columns × the writer's batch cap stays well under Postgres'
    /// 65535-parameter ceiling. Returns rows inserted.
    ///
    /// Alerts whose subject is not a node are dropped from the batch rather than binding a NULL:
    /// `alert_history.node` is `NOT NULL` and `recent()`/`top_nodes_by_fires()` decode it into a
    /// bare `Uuid`, so one NULL row would fail the decode for a whole page — a 500 on the History
    /// screen for every row, not just that one — and would break an N-1 rollback besides.
    pub async fn record_batch(&self, records: &[(Alert, bool)]) -> anyhow::Result<u64> {
        let rows: Vec<(&Alert, NodeId, bool)> = records
            .iter()
            .filter_map(|(alert, resolved)| Some((alert, alert.node()?, *resolved)))
            .collect();
        if rows.len() < records.len() {
            non_node_skipped();
        }
        if rows.is_empty() {
            return Ok(0);
        }
        let mut qb = sqlx::QueryBuilder::new(
            "INSERT INTO alert_history \
             (id, node, check_id, severity, state, at_unix_ms, resolved, \
              metric, observed_value, threshold_value, direction) ",
        );
        qb.push_values(rows, |mut b, (alert, node, resolved)| {
            let (metric, value, threshold, direction) = breach_columns(alert);
            b.push_bind(Uuid::new_v4())
                .push_bind(node.as_uuid())
                .push_bind(alert.check.as_uuid())
                .push_bind(alert.severity.as_str())
                .push_bind(alert.state.as_str())
                .push_bind(alert.at_unix_ms)
                .push_bind(resolved)
                .push_bind(metric)
                .push_bind(value)
                .push_bind(threshold)
                .push_bind(direction.map(Direction::as_str));
        });
        Ok(qb.build().execute(&self.pool).await?.rows_affected())
    }

    /// The RBAC group-visibility predicate over `alert_history` (ADR-014), bound as `$2`.
    ///
    /// History rows are keyed by node id, not by group, so unlike `NodeRepo`'s version this one goes
    /// through a subquery on `nodes`. The subquery is what makes it correct across a node that has
    /// since moved groups: it resolves membership *now*, which is the same answer every other read
    /// path gives, rather than freezing whatever was true when the alert fired.
    ///
    /// ⚠️ Same parenthesisation trap as `NodeRepo::SCOPE_PREDICATE` — `WHERE {SCOPE} AND a OR b`
    /// parses as `({SCOPE} AND a) OR b` and lets every `b` row escape the scope.
    const SCOPE_PREDICATE: &'static str =
        "($2::uuid[] IS NULL OR node IN (SELECT id FROM nodes WHERE group_id = ANY($2)))";

    /// Nodes with the most alert **fires** (resolved=false) at or after `since_ms` (Unix ms),
    /// highest first. Powers the "Top alerting nodes" widget (chronic offenders).
    pub async fn top_nodes_by_fires(
        &self,
        since_ms: i64,
        limit: i64,
    ) -> anyhow::Result<Vec<(Uuid, i64)>> {
        let rows = sqlx::query(
            "SELECT node, count(*) AS n FROM alert_history \
             WHERE resolved = false AND at_unix_ms >= $1 \
             GROUP BY node ORDER BY n DESC LIMIT $2",
        )
        .bind(since_ms)
        .bind(limit.clamp(1, 100))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("node")?, row.try_get::<i64, _>("n")?)))
            .collect()
    }

    /// Alert-fire counts bucketed by weekday (0=Sun … 6=Sat, UTC) × hour (0–23) at or after
    /// `since_ms`. Powers the "Alert calendar" heatmap. UTC so buckets are stable regardless of
    /// the DB session timezone.
    /// `groups` restricts the count to nodes in those folder groups (ADR-014); `None` counts the
    /// whole fleet. Unlike the rankings, this cannot be filtered after the fact — the counts are
    /// produced by the `GROUP BY` and there are no per-node rows left to drop — so the restriction
    /// has to be part of the query. It is written as [`Self::SCOPE_PREDICATE`]: always present,
    /// bound rather than interpolated, `NULL` meaning unrestricted. A conditionally-appended clause
    /// would have a branch that can be forgotten, and forgetting it fails **open**.
    pub async fn fires_by_weekday_hour(
        &self,
        since_ms: i64,
        groups: crate::repo::GroupFilter<'_>,
    ) -> anyhow::Result<Vec<(i32, i32, i64)>> {
        let rows = sqlx::query(&format!(
            "SELECT \
                extract(dow from to_timestamp(at_unix_ms / 1000.0) at time zone 'UTC')::int AS dow, \
                extract(hour from to_timestamp(at_unix_ms / 1000.0) at time zone 'UTC')::int AS hour, \
                count(*) AS n \
             FROM alert_history WHERE resolved = false AND at_unix_ms >= $1 AND {} \
             GROUP BY dow, hour",
            Self::SCOPE_PREDICATE
        ))
        .bind(since_ms)
        .bind(groups.map(<[Uuid]>::to_vec))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    row.try_get("dow")?,
                    row.try_get("hour")?,
                    row.try_get::<i64, _>("n")?,
                ))
            })
            .collect()
    }

    /// Delete history rows older than `older_than_secs` (retention). Returns rows removed.
    pub async fn prune_old(&self, older_than_secs: i64) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "DELETE FROM alert_history WHERE recorded_at < now() - ($1::double precision * interval '1 second')",
        )
        .bind(older_than_secs as f64)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// A page of history rows (newest first). `before` is the keyset cursor — when set, only rows
    /// strictly older than it (by `recorded_at`, the indexed sort column) are returned, so the
    /// WebUI can page back through the whole log instead of being capped at one fetch.
    pub async fn recent(
        &self,
        limit: i64,
        before: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<AlertHistoryRow>> {
        let rows = sqlx::query(
            "SELECT node, check_id, severity, state, at_unix_ms, resolved, \
                    metric, observed_value, threshold_value, direction, recorded_at \
             FROM alert_history \
             WHERE ($2::timestamptz IS NULL OR recorded_at < $2) \
             ORDER BY recorded_at DESC LIMIT $1",
        )
        .bind(limit.clamp(1, 1000))
        .bind(before)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let recorded_at: DateTime<Utc> = row.try_get("recorded_at")?;
                Ok(AlertHistoryRow {
                    node: row.try_get("node")?,
                    check: row.try_get("check_id")?,
                    // The column has no CHECK, so an unrecognised token means a newer core wrote
                    // the row. Degrading beats failing the whole page of history, and `Info` /
                    // `Unknown` are each the least-assertive member of their set — better to
                    // under-state one row than to lose the log an operator is reading it from.
                    severity: Severity::from_token(row.try_get("severity")?)
                        .unwrap_or(Severity::Info),
                    state: NodeState::from_token(row.try_get("state")?)
                        .unwrap_or(NodeState::Unknown),
                    at_unix_ms: row.try_get("at_unix_ms")?,
                    resolved: row.try_get("resolved")?,
                    metric: row.try_get("metric")?,
                    observed_value: row.try_get("observed_value")?,
                    threshold_value: row.try_get("threshold_value")?,
                    direction: row
                        .try_get::<Option<String>, _>("direction")?
                        .as_deref()
                        .and_then(Direction::from_token),
                    recorded_at: recorded_at.to_rfc3339(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_alert::Breach;
    use yagra_common::{CheckId, NodeId};

    const SRC: &str = include_str!("history.rs");

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

    fn alert(metric: &str, breach: Option<Breach>) -> Alert {
        Alert {
            subject: yagra_alert::Subject::Node(NodeId::new()),
            check: CheckId::from(Uuid::new_v4()),
            severity: Severity::Critical,
            state: NodeState::Critical,
            at_unix_ms: 1_000,
            metric: metric.to_owned(),
            breach,
            flapping: false,
            root_cause: None,
        }
    }

    #[test]
    fn a_liveness_alert_stores_null_rather_than_an_empty_metric() {
        // `""` would render as a blank cell where the UI expects "—"; NULL is the shape the reader
        // already handles for legacy rows.
        let (metric, value, threshold, direction) = breach_columns(&alert("", None));
        assert_eq!(metric, None);
        assert_eq!((value, threshold, direction), (None, None, None));
    }

    #[test]
    fn a_threshold_alert_stores_its_breach_detail() {
        let (metric, value, threshold, direction) = breach_columns(&alert(
            "icmp_rtt_ms",
            Some(Breach {
                value: 150.0,
                threshold: Some(100.0),
                direction: Direction::Above,
            }),
        ));
        assert_eq!(metric.as_deref(), Some("icmp_rtt_ms"));
        assert_eq!(value, Some(150.0));
        assert_eq!(threshold, Some(100.0));
        assert_eq!(direction, Some(Direction::Above));
    }

    #[test]
    fn a_breach_with_no_crossed_bound_keeps_its_observed_value() {
        // `threshold` is optional inside a breach (the committed severity may have no bound of its
        // own), but the observed value and direction are always known — dropping them would leave
        // the history saying "something fired" with nothing to read.
        let (_, value, threshold, direction) = breach_columns(&alert(
            "cpu_util",
            Some(Breach {
                value: 91.0,
                threshold: None,
                direction: Direction::Above,
            }),
        ));
        assert_eq!(value, Some(91.0));
        assert_eq!(threshold, None);
        assert_eq!(direction, Some(Direction::Above));
    }

    /// Collapse every run of whitespace to one space and drop the `\` line-continuations, so a
    /// statement's meaning is compared rather than how its string literal happens to be wrapped.
    /// (`include_str!` hands back the raw source, backslashes and all.)
    fn squash(s: &str) -> String {
        s.split_whitespace()
            .filter(|t| *t != "\\")
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn both_writers_insert_the_same_columns_in_the_same_order() {
        // There are two writers — the per-alert `record` and the batch `record_batch` — and a
        // multi-row INSERT binds positionally. If their column lists ever drifted apart, every
        // value in the batch would land one column over: severities into `state`, timestamps into
        // `resolved`. Nothing would error; the history would just be wrong. So the two lists are
        // compared to *each other* rather than to a third copy that could itself go stale.
        let src = squash(&production_source());
        let lists: Vec<&str> = src
            .match_indices("INSERT INTO alert_history")
            .filter_map(|(i, m)| {
                let rest = &src[i + m.len()..];
                let open = rest.find('(')?;
                let close = rest.find(')')?;
                (open < close).then(|| rest[open..=close].trim())
            })
            .collect();
        assert_eq!(
            lists.len(),
            2,
            "expected the single-row and the batch writer"
        );
        assert_eq!(
            lists[0], lists[1],
            "the two alert_history INSERTs name different columns — the batch writer will file \
             every value under its neighbour's column"
        );
        // And the bind list is as long as the column list, so a column added to one without a
        // matching `push_bind` fails here rather than at runtime.
        assert_eq!(lists[0].matches(',').count() + 1, 11);
        assert!(src.contains("VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"));
    }

    #[test]
    fn an_unreadable_severity_or_state_degrades_instead_of_failing_the_page() {
        // Neither column has a DB CHECK, so a newer core's token reaches this reader. Losing one
        // row's precision beats 500-ing the history screen an operator is mid-incident on.
        assert_eq!(
            Severity::from_token("nonsense").unwrap_or(Severity::Info),
            Severity::Info
        );
        assert_eq!(
            NodeState::from_token("nonsense").unwrap_or(NodeState::Unknown),
            NodeState::Unknown
        );
        // The fallbacks are the least-assertive member of each set, not an arbitrary pick.
        assert_eq!(Severity::ALL[0], Severity::Info);
        // A known token still round-trips, so the fallback is not swallowing everything.
        assert_eq!(Severity::from_token("critical"), Some(Severity::Critical));
        assert_eq!(
            NodeState::from_token("unreachable"),
            Some(NodeState::Unreachable)
        );
    }

    #[test]
    fn history_paging_is_keyset_and_every_limit_is_clamped() {
        let src = production_source();
        // The cursor is a bound timestamptz compared with `<`, never an offset.
        assert!(src.contains("WHERE ($2::timestamptz IS NULL OR recorded_at < $2)"));
        assert!(src.contains("ORDER BY recorded_at DESC LIMIT $1"));
        assert!(
            !src.contains("OFFSET"),
            "OFFSET paging reintroduced — rows shift under the reader as alerts fire"
        );
        // Both caller-supplied limits are clamped: an unbounded top-N is a DoS vector.
        assert!(src.contains("limit.clamp(1, 1000)"));
        assert!(src.contains("limit.clamp(1, 100)"));
    }

    #[test]
    fn the_calendar_buckets_in_utc_so_they_do_not_move_with_the_session_timezone() {
        // Without the explicit zone the buckets would depend on whatever the DB session is set to,
        // so the same fleet would render a different heatmap from two cores.
        let src = production_source();
        assert_eq!(src.matches("at time zone 'UTC'").count(), 2);
    }
}
