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
use yagra_alert::{Alert, Subject, SubjectKind};
use yagra_common::{Direction, NodeState, Severity};

/// Count a history row dropped on read because its subject could not be reconstructed.
///
/// The only way that happens is a `subject_kind` this binary does not know — i.e. a newer core
/// wrote the row. Dropping the row beats failing the page (the same call the unreadable-severity
/// fallback makes), but silently dropping it would make History quietly incomplete, so it is
/// counted. Counted rather than logged per occurrence: a page is up to 1000 rows.
fn unreadable_subject_skipped() {
    metrics::counter!("yagra_alert_history_unreadable_subject_total").increment(1);
}

/// One alert-history row for the API.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct AlertHistoryRow {
    /// This row's identity, and the second half of the keyset cursor — pass it as `before_id`
    /// beside `before`. Also the stable key for a list: no other field, or combination of them, is
    /// unique.
    pub id: Uuid,
    /// The node this transition was about; `null` when the subject is not a node — read
    /// `subject_kind` first. It is non-null exactly when `subject_kind` is `node`.
    pub node: Option<Uuid>,
    /// What the transition was about.
    pub subject_kind: SubjectKind,
    /// The subject's name, for a subject identified by name rather than by id (a poller pool).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_name: Option<String>,
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
    /// The SNMP ifIndex of the port this was about, for a per-interface metric (ADR-076).
    ///
    /// `None` for a node-level alert and for every row written before ADR-076 shipped — both mean
    /// "no port was involved", which is why the column is nullable rather than defaulted (there is
    /// no ifIndex value free to mean "none"; `0` is a real one on some agents).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ifindex: Option<u32>,
    /// Insertion time as an RFC 3339 timestamp, and the first half of the **keyset cursor**: the
    /// WebUI passes the last row's `recorded_at` as `before` **and its `id` as `before_id`** to
    /// fetch the next (older) page. Distinct from `at_unix_ms` (the event time).
    ///
    /// ⚠️ Both halves are required, and this is not defensive. `recorded_at` defaults to `now()`,
    /// which in PostgreSQL is the *transaction* timestamp, and [`AlertHistoryStore::record_batch`]
    /// writes a whole flush as one multi-row `INSERT` — so every row of a flush shares a
    /// `recorded_at` to the microsecond. A page boundary landing inside a flush and paging on the
    /// timestamp alone silently **skipped** that flush's remaining rows. A fleet-wide event is
    /// exactly when a flush is large and exactly when someone is reading this log.
    pub recorded_at: String,
}

impl AlertHistoryRow {
    /// Project a subject onto the three columns that describe it. The only place the mapping is
    /// written, so [`Self::subject`] is always its exact inverse for a row this code produced.
    fn project(subject: &Subject) -> (Option<Uuid>, SubjectKind, Option<String>) {
        (
            subject.node().map(|n| n.as_uuid()),
            subject.kind(),
            subject.name().map(str::to_owned),
        )
    }

    /// The subject this row is about, read back from its stored columns.
    ///
    /// The single place the three columns are read as one value, so the ack join and the scope
    /// filter cannot disagree about what a row is about. `None` only for a row whose columns
    /// contradict each other, which [`AlertHistoryStore::recent`] cannot produce — it builds every
    /// row from a subject it already parsed — so in practice this is `Some`.
    #[must_use]
    pub fn subject(&self) -> Option<Subject> {
        Subject::from_storage(
            self.subject_kind,
            self.node.unwrap_or_else(Uuid::nil),
            self.subject_name.as_deref(),
        )
    }
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
    // ⚠️ **The liveness sentinel IS stored**, as its own token — this comment used to claim the
    // opposite ("stored as NULL, not as its internal token"), and the deployment says otherwise:
    // 328 `__liveness__` rows and not one NULL. The check is only for an alert whose descriptive
    // context was never filled in (`Alert::from` starts it empty), which the engine's own paths do
    // not produce. Storing the token is what lets `alerts::restore` hand a liveness alert back as
    // one, and `metric == LIVENESS` is the predicate the whole engine branches on.
    let metric = (!alert.metric.is_empty()).then(|| alert.metric.clone());
    (metric, value, threshold, direction)
}

/// The port column, as the width PostgreSQL stores it in.
///
/// `IfIndex` is a `u32` and the column is `INTEGER` (signed 32-bit), so the top of the range does
/// not fit. A `try_into` that silently dropped such a row's port would be worse than useless, so
/// this saturates to `None` — "we could not record which port" — and says so in a log line rather
/// than storing a negative number that would read back as a different port. No real agent numbers
/// interfaces above 2^31; if one ever does, the log is what finds it.
fn ifindex_column(alert: &Alert) -> Option<i32> {
    let idx = alert.ifindex?;
    i32::try_from(idx.0).map_or_else(
        |_| {
            tracing::warn!(
                ifindex = idx.0,
                metric = %alert.metric,
                "ifIndex exceeds the history column's range; recording the alert without its port"
            );
            None
        },
        Some,
    )
}

/// A validated alert-history query. Built at the API edge by `api::alerts::history_page`.
#[derive(Debug, Default, Clone)]
pub struct HistoryFilter<'a> {
    /// Keyset cursor, paired with [`Self::before_id`]. Distinct from `since`/`until`, which bound
    /// the window being searched rather than where this page starts.
    pub before: Option<DateTime<Utc>>,
    pub before_id: Option<Uuid>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    /// Any of these severities. Empty means unfiltered — the same as omitting the parameter.
    pub severity: Vec<Severity>,
    /// Any of these states. Empty means unfiltered.
    pub state: Vec<NodeState>,
    /// `Some(false)` = fires only, `Some(true)` = clears only.
    pub resolved: Option<bool>,
    /// Restrict to one node subject.
    pub node_id: Option<Uuid>,
    /// Restrict to node subjects currently in these folder groups — the **caller's requested**
    /// group, already expanded to its subtree. Not the caller's RBAC scope; see the const below.
    pub in_group: Option<&'a [Uuid]>,
    /// `Some(true)` = only transitions whose incident is acknowledged, `Some(false)` = only
    /// unacknowledged ones. See [`HISTORY_FILTER_WHERE`] for why this is the one filter whose
    /// answer is computed twice.
    pub acked: Option<bool>,
    /// Case-insensitive substring of the metric name.
    ///
    /// ⚠️ **A more selective term is the slower one.** `alert_history_cursor_idx` still serves the
    /// ordering and the `LIMIT`, so this does not change the access method — but the planner walks
    /// that index until it has filled the page, so a metric matching one row in a million walks the
    /// whole index while a metric matching a third of them stops almost immediately. The same
    /// inversion ADR-024 measured on VictoriaLogs. No index is added for it: at the scale where it
    /// would matter the right index is not obvious, and the wrong one is permanent.
    pub metric: Option<&'a str>,
    /// Case-insensitive substring of the **node's current name**.
    ///
    /// Deliberately distinct from [`Self::node_id`]: that names one node, this asks a question
    /// about a set ("everything called `core-sw…`") that no other parameter can express. Resolved
    /// as a subquery against `nodes` rather than by the caller, because the caller would otherwise
    /// have to page the whole inventory to build the id list.
    pub node_q: Option<&'a str>,
    /// Rows per page; clamped to `1..=1000` by [`AlertHistoryStore::search`].
    pub limit: i64,
}

/// The `WHERE` of a page of alert history. Binds `$1..=$12` in the order [`bind_history_filter`]
/// writes them; `$13` is the page size.
///
/// Every clause is **always present** with a `NULL` bind meaning "no filter", rather than being
/// appended when set — the same rule [`AlertHistoryStore::SCOPE_PREDICATE`] is written to, and for
/// the reason stated there: a conditionally-built predicate has a branch that can be forgotten, and
/// forgetting one fails **open**.
///
/// ⚠️ **`node` is a subject identity, not a node reference** (migration 0075). `$8`, `$9` and `$12`
/// are therefore each paired with `subject_kind = 'node'`. A bare `node = $8` would also match the
/// derived UUID of a pool subject, and a group filter that admitted pool rows would put Yagra's own
/// coverage alerts into an answer about one site's devices.
///
/// ⚠️ **`$10` (ack) is the one filter whose answer this query computes and then throws away.** The
/// ack *shown* on a row is joined in Rust by `api::alerts::decorate_history`, out of a map keyed by
/// the subject — not here — because a subject's storage identity is resolved in `poolres.rs` and
/// not in SQL. So "is it acked" is answered twice, by two mechanisms, and they agree only because
/// the `EXISTS` names `alert_acks`' primary key `(node, check_id, severity)`, which is exactly what
/// `api::alerts::ack_key` builds. `the_ack_filter_reads_the_same_key_the_rust_join_reads` pins that;
/// without it, a row could be filtered *in* as acked and then rendered without its ack pill.
///
/// ⚠️ **The caller's RBAC scope is deliberately NOT here, and must not be added.**
/// [`AlertHistoryStore::SCOPE_PREDICATE`] fails *closed* for a non-node subject — pool→group
/// resolution lives in `poolres.rs` and not in SQL — while the Rust post-filter in
/// `api::alerts::history_page` goes through `scope::allows_subject`, which resolves pools properly.
/// Pushing the scope in here would hide pool-coverage alerts from exactly the scoped operator whose
/// site had gone dark. `$9` is the caller's *requested* group, which can only ever narrow what that
/// post-filter already allowed (`require_visible_group` refuses anything wider), so the two are
/// separate binds on purpose — the same split `analysis::FINDING_SEARCH_WHERE` makes.
const HISTORY_FILTER_WHERE: &str = "\
     ($1::timestamptz IS NULL OR (recorded_at, id) < \
        ($1, coalesce($2::uuid, '00000000-0000-0000-0000-000000000000'::uuid))) \
     AND ($3::timestamptz IS NULL OR recorded_at >= $3) \
     AND ($4::timestamptz IS NULL OR recorded_at <= $4) \
     AND ($5::text[] IS NULL OR severity = ANY($5)) \
     AND ($6::text[] IS NULL OR state = ANY($6)) \
     AND ($7::boolean IS NULL OR resolved = $7) \
     AND ($8::uuid IS NULL OR (subject_kind = 'node' AND node = $8)) \
     AND ($9::uuid[] IS NULL OR (subject_kind = 'node' \
          AND node IN (SELECT id FROM nodes WHERE group_id = ANY($9)))) \
     AND ($10::boolean IS NULL OR EXISTS (SELECT 1 FROM alert_acks a \
          WHERE a.node = alert_history.node AND a.check_id = alert_history.check_id \
            AND a.severity = alert_history.severity) = $10) \
     AND ($11::text IS NULL OR metric ILIKE '%' || $11 || '%') \
     AND ($12::text IS NULL OR (subject_kind = 'node' \
          AND node IN (SELECT id FROM nodes WHERE name ILIKE '%' || $12 || '%')))";

/// Every column [`AlertHistoryStore::read_rows`] reads, in the order it reads them.
///
/// Two statements project this now — the history page and [`AlertHistoryStore::open_alerts`] — and
/// they share one reader, so the list is written once. A second spelling that dropped a column
/// would not be a compile error; it would be a `try_get` failure at core startup.
const HISTORY_COLUMNS: &str = "id, node, subject_kind, subject_ref, check_id, severity, state, \
     at_unix_ms, resolved, metric, observed_value, threshold_value, direction, ifindex, recorded_at";

/// The one statement that reads a page of history. `ORDER BY` names the cursor's columns in the
/// cursor's direction — a keyset cursor is only valid for the ordering it was built for, and
/// `alert_history_cursor_idx` (migration 0082) serves exactly this pair.
fn history_page_sql() -> String {
    format!(
        "SELECT {HISTORY_COLUMNS} \
         FROM alert_history \
         WHERE {HISTORY_FILTER_WHERE} \
         ORDER BY recorded_at DESC, id DESC LIMIT $13"
    )
}

/// The statement behind [`AlertHistoryStore::open_alerts`] and
/// [`AlertHistoryStore::open_alerts_of_deleted_nodes`]: the newest transition per check, kept only
/// when it is a fire, split by whether its subject still exists.
///
/// Three things about it are load-bearing:
///
/// - **`DISTINCT ON (check_id)`** is what makes "open" answerable at all. This table is an
///   append-only transition log, so an alert is open exactly when its check's *latest* row is a
///   fire — not when any unresolved row exists, of which a long outage has many.
/// - **`resolved DESC` in the tie-break** decides a fire and a clear that share a millisecond in
///   favour of the clear, so an ambiguous pair fails *closed*: the alert is not restored, and the
///   next poll re-fires it if it is genuinely still broken. The other order would resurrect a
///   closed incident.
/// - **`node_present` selects which side of the `nodes` sub-select the caller wants**, and the two
///   sides are exhaustive: every open row is in exactly one. That is the property the restore
///   depends on — a row in neither would be one nothing can ever close.
///
/// # Why one function with a flag rather than two statements
///
/// 🚨 The orphan predicate must name `subject_kind = 'node'` explicitly, and a second spelling is
/// exactly where that gets forgotten. A **pool** subject stores a *derived* UUID in `node`
/// (ADR-009 Increment 2), so `node NOT IN (SELECT id FROM nodes)` matches every one of them —
/// filter on the kind and a pool alert would be swept up as an orphan and closed, every restart.
///
/// # The `nodes` sub-select is no longer an exclusion (ADR-097 Increment 5)
///
/// Decision 4 dropped the deleted-node rows outright, and its stated reason was that nothing polls
/// a deleted node, so nothing could ever resolve its restored alert — it would be immortal.
/// **Increment 4 built exactly that path** ([`crate::alerts::AlertManager::forget_deleted_nodes`]),
/// which made the reason false and left the exclusion as the only thing keeping those rows open
/// forever: the restore dropped them, so the sweep — which reads memory — never saw them. Measured
/// 2026-08-31, 43,227 rows left open by deleting 15,000 nodes while core was stopped. So the rows
/// now come back too, through a separate call with its own budget.
fn open_alerts_sql(node_present: bool) -> String {
    // The two predicates are complements over the open rows. A non-node subject has no inventory
    // row that could be missing, so it belongs on the `present` side — which is also the side that
    // seeds the fleet, and a pool subject contributes nothing there either way.
    let subject = match node_present {
        true => "(subject_kind <> 'node' OR node IN (SELECT id FROM nodes))",
        false => "(subject_kind = 'node' AND node NOT IN (SELECT id FROM nodes))",
    };
    format!(
        "SELECT * FROM ( \
           SELECT DISTINCT ON (check_id) {HISTORY_COLUMNS} \
           FROM alert_history \
           ORDER BY check_id, at_unix_ms DESC, resolved DESC, id DESC \
         ) latest \
         WHERE NOT resolved AND {subject} \
         ORDER BY at_unix_ms DESC LIMIT $1"
    )
}

/// An empty set means *unfiltered*, which in this predicate is a `NULL` bind — never an empty
/// array, which `= ANY(…)` would match nothing at all against.
fn set_or_null(tokens: impl IntoIterator<Item = &'static str>) -> Option<Vec<String>> {
    let v: Vec<String> = tokens.into_iter().map(str::to_owned).collect();
    (!v.is_empty()).then_some(v)
}

/// Bind `$1..=$12` of [`HISTORY_FILTER_WHERE`], in the one order that matches it.
///
/// One helper because the sequence is positional and silent when wrong: swapping `$3` and `$4`
/// still compiles, still runs, and just answers a different question.
fn bind_history_filter<'q>(
    q: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    f: &'q HistoryFilter<'_>,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    q.bind(f.before)
        .bind(f.before_id)
        .bind(f.since)
        .bind(f.until)
        .bind(set_or_null(f.severity.iter().map(|s| s.as_str())))
        .bind(set_or_null(f.state.iter().map(|s| s.as_str())))
        .bind(f.resolved)
        .bind(f.node_id)
        .bind(f.in_group)
        .bind(f.acked)
        .bind(f.metric)
        .bind(f.node_q)
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
    /// Every subject is recorded. `node` holds the subject's storage identity — the node's own id
    /// for a node alert — and `subject_kind` is what says which it is (migration 0075).
    pub async fn record(&self, alert: &Alert, resolved: bool) -> anyhow::Result<()> {
        let (metric, value, threshold, direction) = breach_columns(alert);
        sqlx::query(
            "INSERT INTO alert_history \
             (id, node, subject_kind, subject_ref, check_id, severity, state, at_unix_ms, resolved, \
              metric, observed_value, threshold_value, direction, ifindex) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        )
        .bind(Uuid::new_v4())
        .bind(alert.subject.storage_id())
        .bind(alert.subject.kind().as_str())
        .bind(alert.subject.name())
        .bind(alert.check.as_uuid())
        .bind(alert.severity.as_str())
        .bind(alert.state.as_str())
        .bind(alert.at_unix_ms)
        .bind(resolved)
        .bind(metric)
        .bind(value)
        .bind(threshold)
        .bind(direction.map(Direction::as_str))
        .bind(ifindex_column(alert))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Append a **batch** of fire/resolve records in one multi-row INSERT (the async ingest writer,
    /// ADR-025 — mirrors the event pipeline's batch writer). Runs off the matcher's hot path; a DB
    /// hiccup must not stop alerting. 14 columns × the writer's batch cap stays well under Postgres'
    /// 65535-parameter ceiling. Returns rows inserted.
    pub async fn record_batch(&self, records: &[(Alert, bool)]) -> anyhow::Result<u64> {
        if records.is_empty() {
            return Ok(0);
        }
        let mut qb = sqlx::QueryBuilder::new(
            "INSERT INTO alert_history \
             (id, node, subject_kind, subject_ref, check_id, severity, state, at_unix_ms, resolved, \
              metric, observed_value, threshold_value, direction, ifindex) ",
        );
        qb.push_values(records, |mut b, (alert, resolved)| {
            let (metric, value, threshold, direction) = breach_columns(alert);
            b.push_bind(Uuid::new_v4())
                .push_bind(alert.subject.storage_id())
                .push_bind(alert.subject.kind().as_str())
                .push_bind(alert.subject.name().map(str::to_owned))
                .push_bind(alert.check.as_uuid())
                .push_bind(alert.severity.as_str())
                .push_bind(alert.state.as_str())
                .push_bind(alert.at_unix_ms)
                .push_bind(*resolved)
                .push_bind(metric)
                .push_bind(value)
                .push_bind(threshold)
                .push_bind(direction.map(Direction::as_str))
                .push_bind(ifindex_column(alert));
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
    /// parses as `({SCOPE} AND a) OR b` and lets every `b` row escape the scope. The whole
    /// expression is therefore wrapped, and so is each side of its `OR`.
    ///
    /// **A non-node subject is visible to unrestricted callers only.** A pool alert *is* narrowable
    /// in principle — a scoped operator whose nodes are polled by that pool has every reason to see
    /// it, and `api/scope.rs::allows_subject` answers exactly that for the in-memory paths — but
    /// the answer needs effective-pool resolution (own > nearest ancestor folder > default), which
    /// lives in `poolres.rs` and not in SQL by design (0054). Rather than write a second, weaker
    /// copy of that resolution as a subquery, the aggregate side fails **closed**: a scoped caller
    /// sees node rows only. The row still reaches them through `GET /api/v1/alerts` and the live
    /// stream, which resolve pools properly.
    const SCOPE_PREDICATE: &'static str = "((subject_kind = 'node' \
         AND ($2::uuid[] IS NULL OR node IN (SELECT id FROM nodes WHERE group_id = ANY($2)))) \
         OR (subject_kind <> 'node' AND $2::uuid[] IS NULL))";

    /// What a **fire** is, and how far back to count — the one copy (ADR-112 Inc.2).
    ///
    /// Three aggregates below open with this predicate and each used to spell it out. That was
    /// survivable while they only had to agree with each other; it stopped being so when
    /// [`Self::fires_by_severity`] arrived, because its answer and [`Self::top_nodes_by_fires`]'s
    /// appear **two sections apart in one report** and a reader compares them.
    const FIRES_SINCE: &'static str = "resolved = false AND at_unix_ms >= $1";

    /// Alert **fires** in the window, counted per severity. Powers the report's "Alert summary".
    ///
    /// 🎯 **A SQL aggregate rather than a fold over recent rows** (ADR-112 Inc.2). The report used
    /// to read the newest 1000 history rows and count them in memory, so on a fleet with more than
    /// that many transitions inside the window the number was silently low — and the 1000 was
    /// shared with the resolutions, so the effective ceiling was roughly half. "Top alerting
    /// nodes", two sections away in the same report, has always been an aggregate, so the one
    /// document disagreed with itself.
    ///
    /// ⚠️ **No `subject_kind` filter, unlike [`Self::top_nodes_by_fires`].** That one ranks
    /// *nodes*, so a pool subject would rank as a node with no name. This counts severities, and
    /// the fold it replaces counted every row it was handed — restricting here would be a second,
    /// unasked-for change of meaning.
    ///
    /// ⚠️ **No scope predicate**, for the reason `reports/seams.rs` gives about `node_names`: a
    /// report is a fleet-wide artefact generated with no requesting principal (ADR-014 non-goal).
    pub async fn fires_by_severity(&self, since_ms: i64) -> anyhow::Result<Vec<(String, i64)>> {
        let rows = sqlx::query(&format!(
            "SELECT severity, count(*) AS n FROM alert_history WHERE {} GROUP BY severity",
            Self::FIRES_SINCE
        ))
        .bind(since_ms)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("severity")?, row.try_get::<i64, _>("n")?)))
            .collect()
    }

    /// Nodes with the most alert **fires** (resolved=false) at or after `since_ms` (Unix ms),
    /// highest first. Powers the "Top alerting nodes" widget (chronic offenders).
    ///
    /// Node subjects only — the ranking is of *nodes*, and `node` on a pool row is a derived
    /// identity that resolves to no inventory entry (migration 0075), so an unfiltered `GROUP BY`
    /// would rank a pool as a node with an unresolvable name.
    pub async fn top_nodes_by_fires(
        &self,
        since_ms: i64,
        limit: i64,
    ) -> anyhow::Result<Vec<(Uuid, i64)>> {
        let rows = sqlx::query(&format!(
            "SELECT node, count(*) AS n FROM alert_history \
             WHERE {} AND subject_kind = 'node' \
             GROUP BY node ORDER BY n DESC LIMIT $2",
            Self::FIRES_SINCE
        ))
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
             FROM alert_history WHERE {} AND {} \
             GROUP BY dow, hour",
            Self::FIRES_SINCE,
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

    /// A page of history rows (newest first), narrowed by `filter`.
    ///
    /// See [`HISTORY_FILTER_WHERE`] for what each field means in SQL, and for the two things that
    /// are easy to get wrong here: `node` is a *subject* identity rather than a node reference, and
    /// the caller's RBAC scope is deliberately **not** part of this query.
    pub async fn search(&self, filter: &HistoryFilter<'_>) -> anyhow::Result<Vec<AlertHistoryRow>> {
        let sql = history_page_sql();
        let rows = bind_history_filter(sqlx::query(&sql), filter)
            .bind(filter.limit.clamp(1, 1000))
            .fetch_all(&self.pool)
            .await?;
        Self::read_rows(rows)
    }

    /// A page of history rows (newest first).
    ///
    /// `before` + `before_id` are the keyset cursor: when set, only rows strictly older than that
    /// **(recorded_at, id)** pair are returned, so the WebUI can page back through the whole log
    /// instead of being capped at one fetch.
    ///
    /// ⚠️ **The pair is not belt-and-braces — a timestamp alone loses rows.** `recorded_at` defaults
    /// to `now()`, which is the *transaction* timestamp, and [`AlertHistoryStore::record_batch`]
    /// writes an entire flush as one multi-row `INSERT`; every row of a flush therefore carries an
    /// identical `recorded_at`. With `recorded_at < $1` alone, a page boundary landing inside a
    /// flush skipped every sibling not yet returned, and did so silently. Migration 0082 replaced
    /// the single-column index with `(recorded_at DESC, id DESC)` so the composite cursor is an
    /// index seek rather than a filter.
    ///
    /// Passing `before` without `before_id` still means "strictly before this instant": the
    /// `coalesce` to the nil UUID makes the row comparison degrade to the timestamp, because no
    /// UUID sorts below nil. That keeps an N-1 client — which sends only `before` — correct rather
    /// than empty-handed.
    pub async fn recent(
        &self,
        limit: i64,
        before: Option<DateTime<Utc>>,
        before_id: Option<Uuid>,
    ) -> anyhow::Result<Vec<AlertHistoryRow>> {
        self.search(&HistoryFilter {
            before,
            before_id,
            limit,
            ..HistoryFilter::default()
        })
        .await
    }

    /// Every alert that was **open when the previous process stopped and is still about something
    /// the inventory holds** — the newest transition per check, kept only where that transition is
    /// a fire (see [`open_alerts_sql`]).
    ///
    /// This is what `alerts::restore` reads at startup (ADR-097 decision 2). Before it existed, the
    /// engine began every process believing nothing was wrong, so a still-broken device re-fired
    /// (one duplicate incident per restart — measured: 1,356 rows carrying only 18 clears, and one
    /// continuously-down device with eight `__liveness__` fires and no clear in 24 hours) while a
    /// device that recovered *during* the restart never resolved at all, leaving its incident open
    /// in the external tool forever.
    ///
    /// The rows this one leaves behind are [`Self::open_alerts_of_deleted_nodes`]'s, and they go to
    /// a different entry point on the engine — see that method for why the split is not cosmetic.
    ///
    /// ⚠️ `limit` is a backstop, not a policy: the answer is bounded by the number of checks, not by
    /// the size of the log. A deployment that hits it has more open alerts than a human could act on
    /// and wants to know, so the caller warns rather than silently restoring a prefix.
    pub async fn open_alerts(&self, limit: i64) -> anyhow::Result<Vec<AlertHistoryRow>> {
        self.open_alerts_where(limit, true).await
    }

    /// The other half: open alerts about a node the inventory **no longer holds** (ADR-097
    /// Increment 5).
    ///
    /// Deleting a node produces no poll result, so nothing on the poll path can ever resolve its
    /// alert; Increment 4's sweep is what closes it, and the sweep reads the engine's in-memory
    /// `active` set. That works while core is running and fails completely when the delete happens
    /// while core is **stopped**: decision 4 had the restore drop these rows, so by the time the
    /// sweep ran there was nothing in `active` to find. Measured 2026-08-31 — 15,000 nodes deleted
    /// by hand against a stopped core left **43,227** rows open, permanently, with the incidents
    /// they had opened in PagerDuty/JSM still open too.
    ///
    /// 🚨 **Its own budget, not a share of `open_alerts`'s.** One query for both would let a mass
    /// deletion push genuinely open alerts out of the restore — the rows are taken newest-first, and
    /// a load test's residue is the newest thing in the table. A deployment with more orphans than
    /// `limit` closes them across successive restarts instead of starving the live fleet once.
    pub async fn open_alerts_of_deleted_nodes(
        &self,
        limit: i64,
    ) -> anyhow::Result<Vec<AlertHistoryRow>> {
        self.open_alerts_where(limit, false).await
    }

    /// One reader for both sides, so the projection and the row interpretation cannot diverge.
    async fn open_alerts_where(
        &self,
        limit: i64,
        node_present: bool,
    ) -> anyhow::Result<Vec<AlertHistoryRow>> {
        let rows = sqlx::query(&open_alerts_sql(node_present))
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        Self::read_rows(rows)
    }

    /// Turn raw rows into [`AlertHistoryRow`]s. One reader for both statements — the projection is
    /// where the subject columns are interpreted, and a second copy could interpret them differently.
    fn read_rows(rows: Vec<sqlx::postgres::PgRow>) -> anyhow::Result<Vec<AlertHistoryRow>> {
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            // The subject is read first and as a whole. A row whose `subject_kind` this binary does
            // not know (a newer core wrote it) is **dropped**, not rendered as whatever its `node`
            // column happens to hold — for a non-node subject that is a derived identity which
            // resolves to no inventory entry, so showing it would invent a node.
            let kind: String = row.try_get("subject_kind")?;
            let name: Option<String> = row.try_get("subject_ref")?;
            let id: Uuid = row.try_get("node")?;
            let Some(subject) = SubjectKind::from_token(&kind)
                .and_then(|k| Subject::from_storage(k, id, name.as_deref()))
            else {
                unreadable_subject_skipped();
                continue;
            };
            let (node, subject_kind, subject_name) = AlertHistoryRow::project(&subject);
            let recorded_at: DateTime<Utc> = row.try_get("recorded_at")?;
            out.push(AlertHistoryRow {
                id: row.try_get("id")?,
                node,
                subject_kind,
                subject_name,
                check: row.try_get("check_id")?,
                // The column has no CHECK, so an unrecognised token means a newer core wrote
                // the row. Degrading beats failing the whole page of history, and `Info` /
                // `Unknown` are each the least-assertive member of their set — better to
                // under-state one row than to lose the log an operator is reading it from.
                severity: Severity::from_token(row.try_get("severity")?).unwrap_or(Severity::Info),
                state: NodeState::from_token(row.try_get("state")?).unwrap_or(NodeState::Unknown),
                at_unix_ms: row.try_get("at_unix_ms")?,
                resolved: row.try_get("resolved")?,
                metric: row.try_get("metric")?,
                observed_value: row.try_get("observed_value")?,
                threshold_value: row.try_get("threshold_value")?,
                direction: row
                    .try_get::<Option<String>, _>("direction")?
                    .as_deref()
                    .and_then(Direction::from_token),
                // Stored as INTEGER; a negative value could only come from a hand-written row, and
                // there is no port it could name, so it degrades to "no port" like a NULL rather
                // than wrapping into a plausible-looking one.
                ifindex: row
                    .try_get::<Option<i32>, _>("ifindex")?
                    .and_then(|v| u32::try_from(v).ok()),
                recorded_at: recorded_at.to_rfc3339(),
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_alert::Breach;
    use yagra_common::{CheckId, NodeId};

    /// This module's code, comments stripped — see
    /// [`crate::module_source::code_no_comments`] for why both.
    fn production_source() -> String {
        crate::module_source::code_no_comments("src", "history")
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
            ifindex: None,
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
        // And the placeholder list is as long as the column list, so a column added to one
        // without a matching value fails here rather than at runtime (Postgres would reject it,
        // but only on the first alert of a deployment that already stopped recording history).
        //
        // Derived from the column list rather than pinned to a literal count: a magic number here
        // is a second copy of the same fact, and the one that gets "fixed" to match rather than
        // read. What is actually invariant is column count == placeholder count.
        let cols = lists[0].matches(',').count() + 1;
        let placeholders: Vec<String> = (1..=cols).map(|i| format!("${i}")).collect();
        let values = format!("VALUES ({})", placeholders.join(", "));
        assert!(
            src.contains(&values),
            "the single-row INSERT names {cols} columns but its VALUES list is not {values}"
        );
        // The batch writer builds its values with `push_bind` instead of a VALUES literal, so
        // count those the same way — this is the half that has no Postgres-side backstop at all
        // when it is short, because `QueryBuilder` simply emits a narrower row.
        let batch = src
            .split_once("push_values(records")
            .expect("the batch writer exists")
            .1;
        let body = batch.split_once("});").map_or(batch, |(b, _)| b);
        assert_eq!(
            body.matches("push_bind(").count(),
            cols,
            "the batch writer binds a different number of values than the column list names"
        );
    }

    #[test]
    fn a_node_row_projects_and_reads_back_as_the_same_subject() {
        // `project` and `subject` are inverses by construction — the ack join and the scope filter
        // both go through the second, and the writer through the first.
        for subject in [
            yagra_alert::Subject::Node(NodeId::new()),
            yagra_alert::Subject::Pool("tokyo".to_owned()),
        ] {
            let (node, subject_kind, subject_name) = AlertHistoryRow::project(&subject);
            let row = AlertHistoryRow {
                id: Uuid::new_v4(),
                node,
                subject_kind,
                subject_name,
                check: Uuid::new_v4(),
                severity: Severity::Critical,
                state: NodeState::Critical,
                at_unix_ms: 1,
                resolved: false,
                metric: None,
                observed_value: None,
                threshold_value: None,
                direction: None,
                recorded_at: String::new(),
                ifindex: None,
            };
            assert_eq!(row.subject().as_ref(), Some(&subject), "{subject}");
            // `node` is populated exactly when the subject is a node — that biconditional is what
            // the WebUI and the scope filter both branch on.
            assert_eq!(row.node.is_some(), subject.node().is_some(), "{subject}");
        }
    }

    #[test]
    fn the_node_rankings_and_the_scope_predicate_both_name_the_subject_kind() {
        // Two different failures, one root cause. `top_nodes_by_fires` ranks *nodes*, so a pool
        // row's derived id must not enter its GROUP BY (it resolves to no inventory entry and
        // would render as a raw UUID). `SCOPE_PREDICATE` must not let a non-node row past a group
        // filter it cannot evaluate — the fail-closed side of the trade documented on the const.
        let src = squash(&production_source());
        // "A fire since T" is one const, interpolated by all three aggregates (ADR-112 Inc.2).
        // Counted rather than re-spelled per statement: three literals that had to agree is
        // exactly what the const removed, and writing them out here would put the third copy in
        // the test. What made it worth removing: two of the three answer the *same report*, two
        // sections apart, and a reader compares them.
        assert_eq!(
            AlertHistoryStore::FIRES_SINCE,
            "resolved = false AND at_unix_ms >= $1"
        );
        assert_eq!(
            src.matches("Self::FIRES_SINCE").count(),
            3,
            "every fire aggregate must count from the one predicate"
        );
        assert!(
            src.contains("WHERE {} AND subject_kind = 'node' GROUP BY node"),
            "the node ranking no longer filters to node subjects"
        );
        // The whole predicate is parenthesised and so is each side of its `OR` — it is
        // interpolated into `… AND {}`, where an unwrapped `OR` would let every non-node row
        // escape every scope.
        assert!(src.contains(
            "((subject_kind = 'node' AND ($2::uuid[] IS NULL OR node IN \
             (SELECT id FROM nodes WHERE group_id = ANY($2)))) \
             OR (subject_kind <> 'node' AND $2::uuid[] IS NULL))"
        ));
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
        // The cursor is a bound (timestamptz, uuid) pair compared with `<`, never an offset.
        assert!(src.contains("($1::timestamptz IS NULL OR (recorded_at, id) <"));
        assert!(
            !src.contains("OFFSET"),
            "OFFSET paging reintroduced — rows shift under the reader as alerts fire"
        );
        // Both caller-supplied limits are clamped: an unbounded top-N is a DoS vector.
        assert!(src.contains("limit.clamp(1, 1000)"));
        assert!(src.contains("limit.clamp(1, 100)"));
    }

    #[test]
    fn the_page_orders_by_exactly_the_columns_its_cursor_compares() {
        // A keyset cursor is only valid for the ordering it was built for. If the ORDER BY and the
        // cursor ever name different columns — or the same columns in a different direction — paging
        // skips and repeats rows, and does so without any error.
        let src = production_source();
        assert!(src.contains("ORDER BY recorded_at DESC, id DESC"));
        assert!(
            !src.contains("ORDER BY recorded_at DESC LIMIT"),
            "the single-column ordering is back, but the cursor is a pair"
        );
    }

    #[test]
    fn a_timestamp_only_cursor_still_means_strictly_before_that_instant() {
        // An N-1 WebUI sends `before` and no `before_id`. `(recorded_at, id) < ($1, NULL)` would be
        // NULL for every row — an empty page, i.e. "you have reached the end" — so the nil-UUID
        // coalesce is load-bearing, not tidiness. Nothing sorts below the nil UUID, which is what
        // degrades the row comparison back to `recorded_at < $1`.
        let src = production_source();
        assert!(src.contains("coalesce($2::uuid, '00000000-0000-0000-0000-000000000000'::uuid)"));
    }

    #[test]
    fn a_node_or_group_filter_names_the_subject_kind_beside_the_id() {
        // The migration-0075 trap: `alert_history.node` is a *subject* identity, not a node
        // reference. A bare `node = $8` would also match the derived UUID of a pool subject, and a
        // group filter admitting pool rows would put Yagra's own coverage alerts into an answer
        // about one site's devices. Nothing but a source check can catch this without a database.
        //
        // `$12` (the node-name substring) is here for the same reason and is the newer trap: it
        // reads like a filter on a *name*, so the subject-kind pairing looks redundant until you
        // notice a pool subject's derived UUID can collide with a real node's id.
        for bind in ["$8", "$9", "$12"] {
            let clause = HISTORY_FILTER_WHERE
                .split(&format!("({bind}::"))
                .nth(1)
                .unwrap_or_else(|| panic!("{bind} clause"));
            let clause = clause.split(" AND (").next().unwrap_or(clause);
            assert!(
                clause.contains("subject_kind = 'node'"),
                "{bind} filters on `node` without restricting to node subjects: {clause}"
            );
        }
    }

    #[test]
    fn the_history_filter_binds_every_placeholder_it_names() {
        // Positional and silent when wrong: swapping two binds still compiles, still runs, and just
        // answers a different question.
        let named = (1..=12)
            .filter(|n| HISTORY_FILTER_WHERE.contains(&format!("${n}")))
            .count();
        assert_eq!(named, 12, "the predicate should name exactly $1..=$12");
        let binds = production_source()
            .split("fn bind_history_filter")
            .nth(1)
            .expect("the bind helper")
            .split("pub struct AlertHistoryStore")
            .next()
            .expect("split always yields a first element")
            .matches(".bind(")
            .count();
        assert_eq!(binds, named, "one bind per placeholder");
        assert!(history_page_sql().contains("LIMIT $13"));
    }

    #[test]
    fn the_ack_filter_reads_the_same_key_the_rust_join_reads() {
        // "Is this acked" is answered twice — by `$10`'s EXISTS when *filtering*, and by
        // `api::alerts::decorate_history`'s map when *rendering* — because a subject's storage
        // identity resolves in `poolres.rs` rather than in SQL. Two mechanisms, one question, and
        // the failure is invisible: a row filtered in as acked would render with no ack pill, or a
        // row the operator asked to hide would come back. They agree only while both name
        // `alert_acks`' primary key, so pin the three columns.
        let exists = HISTORY_FILTER_WHERE
            .split("EXISTS (SELECT 1 FROM alert_acks a")
            .nth(1)
            .expect("the ack EXISTS");
        let exists = exists.split(") = $10").next().unwrap_or(exists);
        for col in ["a.node = alert_history.node", "a.check_id", "a.severity"] {
            assert!(
                exists.contains(col),
                "the ack EXISTS dropped {col}: {exists}"
            );
        }
        // The Rust side of the same key. `ack_key` builds `(storage_id, check, severity)`, and
        // `alert_history.node` *is* the storage id (migration 0075) — so a change to either half
        // that does not change the other lands here.
        let rust =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/api/alerts.rs"))
                .expect("read api/alerts.rs");
        assert!(
            rust.contains("(subject.storage_id(), check, severity.as_str().to_owned())"),
            "ack_key changed shape; the SQL EXISTS above must change with it"
        );
    }

    #[test]
    fn the_history_filter_carries_the_requested_group_and_not_the_callers_scope() {
        // The decision this pins: `SCOPE_PREDICATE` fails *closed* for a non-node subject, because
        // pool→group resolution lives in `poolres.rs` and not in SQL. Pushing the caller's scope in
        // here would therefore hide pool-coverage alerts from exactly the scoped operator whose site
        // had gone dark. The handler post-filters instead, through `allows_subject`, which resolves
        // pools properly. Exactly one `nodes` subquery — the *requested* group — belongs here.
        // Counting `nodes` subqueries stopped working when `$12` added a second one that asks a
        // different question (a name substring). What must stay unique is the *group* shape —
        // that is the one the caller's scope would take.
        assert_eq!(
            HISTORY_FILTER_WHERE.matches("group_id = ANY(").count(),
            1,
            "a second group subquery means the caller's scope was pushed into this predicate"
        );
        assert!(
            !HISTORY_FILTER_WHERE.contains("subject_kind <> 'node'"),
            "the fail-closed scope arm belongs to SCOPE_PREDICATE, not to the filter"
        );
    }

    #[test]
    fn the_filter_is_always_present_rather_than_appended_when_set() {
        // The fail-open shape: a conditionally-built WHERE has a branch someone can forget, and the
        // branch that gets forgotten returns *more* rows, not fewer.
        //
        // Eleven arms for twelve binds, and `$2` is the deliberate exception: `before_id` has no
        // meaning without `before`, so instead of its own "no filter" arm it is absorbed by the
        // `coalesce` inside `$1`'s. That is what makes a cursor of `before` alone degrade to
        // "strictly before this instant" rather than matching nothing.
        assert_eq!(
            HISTORY_FILTER_WHERE.matches(" IS NULL").count(),
            11,
            "every bind but the cursor's second half must carry its own 'no filter' arm"
        );
        assert!(
            !HISTORY_FILTER_WHERE.contains("$2::uuid IS NULL"),
            "before_id gained its own arm — then a cursor without it matches nothing"
        );
    }

    #[test]
    fn the_row_carries_the_identity_its_cursor_needs() {
        // The cursor's second half has to be readable off a row the client already holds, or the
        // client cannot build the next request. `id` is also the only unique key on the row — the
        // WebUI's previous composite React key could collide within one flush.
        assert!(HISTORY_COLUMNS.starts_with("id, node, subject_kind"));
        assert!(production_source().contains("id: row.try_get(\"id\")?"));
    }

    /// Two statements, one reader. `read_rows` names its columns by string, so a projection that
    /// dropped one would compile, run, and fail at `try_get` — for `open_alerts` that means core
    /// refusing to start. Pin both statements to the shared list instead.
    #[test]
    fn both_statements_project_the_columns_the_reader_names() {
        for sql in [
            history_page_sql(),
            open_alerts_sql(true),
            open_alerts_sql(false),
        ] {
            assert!(
                sql.contains(HISTORY_COLUMNS),
                "a statement spells its own column list: {sql}"
            );
        }
        let src = production_source();
        let reader = src.split("fn read_rows").nth(1).expect("the reader");
        for col in HISTORY_COLUMNS.split(',').map(str::trim) {
            // Matched on the call's argument rather than on `try_get("…")`, because two columns are
            // fetched through a turbofish (`try_get::<Option<i32>, _>("ifindex")`). `node` and
            // `subject_ref` are read into the subject before the struct is built and `check_id`
            // lands in a field called `check`; every one is still fetched by its column name.
            assert!(
                reader.contains(&format!("(\"{col}\")")),
                "the reader never fetches {col}"
            );
        }
    }

    /// The tie-break is the difference between "do not restore an ambiguous pair" and "resurrect a
    /// closed incident", and neither a compiler nor a running deployment would tell you which one
    /// you shipped.
    #[test]
    fn the_open_query_breaks_ties_towards_the_clear() {
        for sql in [open_alerts_sql(true), open_alerts_sql(false)] {
            assert!(
                sql.contains("ORDER BY check_id, at_unix_ms DESC, resolved DESC"),
                "a fire and a clear sharing a millisecond must resolve in favour of the clear: {sql}"
            );
            assert!(sql.contains("WHERE NOT resolved"));
        }
    }

    /// 🚨 The orphan side must name the subject kind, and nothing but this test says so.
    ///
    /// A **pool** subject stores a derived UUID in `node` (ADR-009 Increment 2), so it is in no
    /// deployment's `nodes` table and `node NOT IN (SELECT id FROM nodes)` matches every one of
    /// them. Drop the `subject_kind` clause and every pool alert is read back as an orphan and
    /// closed by the deleted-node sweep, on every restart, with no error anywhere.
    #[test]
    fn only_node_subjects_can_be_orphans() {
        let orphans = open_alerts_sql(false);
        assert!(
            orphans.contains("subject_kind = 'node' AND node NOT IN (SELECT id FROM nodes)"),
            "the orphan side must be narrowed to node subjects: {orphans}"
        );
    }

    /// The two sides are complements: every open row is in exactly one, so no row is left with
    /// nothing that could ever close it — which is the whole of ADR-097 Increment 5.
    ///
    /// Asserted on the text because there is no database here. The `NOT` is what makes them
    /// disjoint and the `subject_kind` split is what makes them exhaustive, so both spellings are
    /// pinned rather than merely "they differ".
    #[test]
    fn the_two_open_queries_partition_the_open_rows() {
        let present = open_alerts_sql(true);
        let orphans = open_alerts_sql(false);
        assert_ne!(present, orphans, "the flag must change the statement");
        assert!(
            present.contains("subject_kind <> 'node' OR node IN (SELECT id FROM nodes)"),
            "the present side keeps non-node subjects and live nodes: {present}"
        );
        // Belt and braces on the needle above: `node IN` is a substring of nothing on the orphan
        // side only because of the ` NOT `, so assert the negation explicitly rather than trusting
        // substring luck.
        assert!(!present.contains("NOT IN (SELECT id FROM nodes)"));
        assert!(!orphans.contains("subject_kind <> 'node'"));
    }

    #[test]
    fn the_calendar_buckets_in_utc_so_they_do_not_move_with_the_session_timezone() {
        // Without the explicit zone the buckets would depend on whatever the DB session is set to,
        // so the same fleet would render a different heatmap from two cores.
        let src = production_source();
        assert_eq!(src.matches("at time zone 'UTC'").count(), 2);
    }

    // ── Against a real database (ADR-114) ────────────────────────────────────────────────
    //
    // ADR-112 Increment 2 moved the "Fires in window" count out of a fold over
    // `recent_history(1000)` and into `fires_by_severity`. It said, in as many words, what that
    // cost: "resolutions are not fires" and "the window excludes what is outside it" had been
    // assertions over the fold, reachable from a fake, and became SQL that no fake can see. These
    // are those assertions, back — and the third is the one the hardware could not answer.

    /// A fire (or resolution) for `subject`, at `at_unix_ms`, with `severity`.
    fn fire(node: Uuid, severity: Severity, at_unix_ms: i64) -> Alert {
        Alert {
            subject: yagra_alert::Subject::Node(NodeId::from(node)),
            check: CheckId::from(Uuid::new_v4()),
            severity,
            state: NodeState::Critical,
            at_unix_ms,
            metric: "icmp_rtt_ms".to_owned(),
            breach: None,
            flapping: false,
            root_cause: None,
            ifindex: None,
        }
    }

    /// **A fire is `resolved = false`, at or after the window's start.**
    ///
    /// Three ways to be counted wrongly, one fixture: a resolution inside the window, a fire
    /// before it, and a fire on the boundary itself (`>=`, so it counts).
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn only_fires_inside_the_window_are_counted(pool: sqlx::PgPool) {
        let node = crate::pgtest::node(&pool, "n1", 1, None).await;
        let store = AlertHistoryStore::new(pool.clone());
        let since = 10_000i64;
        store
            .record_batch(&[
                (fire(node, Severity::Critical, since), false), // on the boundary
                (fire(node, Severity::Critical, since + 5), false),
                (fire(node, Severity::Warning, since + 5), false),
                (fire(node, Severity::Critical, since + 5), true), // a resolution, not a fire
                (fire(node, Severity::Critical, since - 1), false), // one millisecond too early
            ])
            .await
            .expect("record");

        let mut counts = store.fires_by_severity(since).await.expect("count");
        counts.sort();
        assert_eq!(
            counts,
            vec![("critical".to_owned(), 2), ("warning".to_owned(), 1)],
            "a resolution or an out-of-window row was counted as a fire"
        );

        // The acceptance side, so a query that had stopped matching anything cannot pass: widen
        // the window and the row that was excluded appears.
        let all = store.fires_by_severity(0).await.expect("count");
        assert_eq!(all.iter().map(|(_, n)| n).sum::<i64>(), 4);
    }

    /// **The two numbers on one page cannot disagree.**
    ///
    /// "Fires in window" and "Top alerting nodes" are two sections of the same report, two
    /// paragraphs apart, and they used to be produced by different mechanisms over different
    /// row sets. Since ADR-112 Increment 2 they share [`AlertHistoryStore::FIRES_SINCE`] and are
    /// given the same lower bound; this runs both against one fixture and adds them up.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn both_halves_of_the_alert_story_agree_on_one_fixture(pool: sqlx::PgPool) {
        let a = crate::pgtest::node(&pool, "a", 1, None).await;
        let b = crate::pgtest::node(&pool, "b", 2, None).await;
        let store = AlertHistoryStore::new(pool.clone());
        let since = 10_000i64;
        let mut records: Vec<(Alert, bool)> = Vec::new();
        for _ in 0..7 {
            records.push((fire(a, Severity::Critical, since + 1), false));
        }
        for _ in 0..3 {
            records.push((fire(b, Severity::Warning, since + 1), false));
        }
        // Noise neither is allowed to count.
        records.push((fire(a, Severity::Critical, since + 1), true));
        records.push((fire(b, Severity::Critical, since - 1), false));
        store.record_batch(&records).await.expect("record");

        let by_severity: i64 = store
            .fires_by_severity(since)
            .await
            .expect("severity")
            .iter()
            .map(|(_, n)| n)
            .sum();
        let by_node: i64 = store
            .top_nodes_by_fires(since, 100)
            .await
            .expect("nodes")
            .iter()
            .map(|(_, n)| n)
            .sum();

        assert_eq!(by_severity, 10);
        assert_eq!(by_node, by_severity, "the report's two totals disagree");
    }

    /// 🎯 **More than a thousand fires in the window are all counted.**
    ///
    /// The defect ADR-112 Increment 2 fixed: the count used to come from a fold over the most
    /// recent 1,000 history rows, so a busy window silently reported fewer fires than it held —
    /// and fires and resolutions shared that budget, so the effective ceiling depended on the mix.
    ///
    /// 🚨 **The deployment could not answer this.** `.210`'s seven-day window held 816 rows
    /// (503 fires, 313 resolutions), so the old code and the new one both said 503 and the fix was
    /// only ever confirmed as "not broken". 1,200 fires plus 400 resolutions is 1,600 rows — over
    /// the old cap in total, and over it in fires alone.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_window_holding_more_than_a_thousand_fires_reports_all_of_them(pool: sqlx::PgPool) {
        let node = crate::pgtest::node(&pool, "busy", 1, None).await;
        let store = AlertHistoryStore::new(pool.clone());
        let since = 10_000i64;
        let mut records: Vec<(Alert, bool)> = Vec::with_capacity(1_600);
        for i in 0..1_200i64 {
            records.push((fire(node, Severity::Critical, since + i), false));
        }
        for i in 0..400i64 {
            records.push((fire(node, Severity::Critical, since + i), true));
        }
        store.record_batch(&records).await.expect("record");

        let total: i64 = store
            .fires_by_severity(since)
            .await
            .expect("count")
            .iter()
            .map(|(_, n)| n)
            .sum();
        assert_eq!(total, 1_200, "the count is capped again");
        assert_eq!(
            store
                .top_nodes_by_fires(since, 100)
                .await
                .expect("nodes")
                .first()
                .map(|(_, n)| *n),
            Some(1_200),
            "the ranking and the count no longer agree past the old cap"
        );
    }
}
