// SPDX-License-Identifier: AGPL-3.0-only
//! Passive event pipeline (Phase 2): syslog / SNMP traps / inbound webhooks → rule
//! matching → alerts.
//!
//! Events arrive as [`EventMsg`]s — from pollers over `yagra.events` (syslog/traps) or
//! from the webhook ingest endpoint (api.rs). The [`EventEngine`] runs each through:
//! burst dedup → node correlation (source IP → inventory, or the webhook source's node
//! binding) → rule matching (substring/regex, compiled snapshot) → persist to the
//! `events` table → raise/refresh/clear alerts through the existing [`AlertManager`] /
//! [`Notifier`] / history pipeline (dedup, mutes, SSE all reused).
//!
//! **Event alerts are edge-triggered**: there is no `CheckState`/dwell — damping comes
//! from the per-rule min-count/window gate, TTL refresh (a repeat match extends the
//! deadline, no re-notification), and the poller-side rate limits. Resolution is TTL
//! expiry (sweeper task), a clear-pattern match, or manual close. **Dependency
//! suppression is deliberately skipped** (`root_cause: None`): a device that just
//! emitted an event is demonstrably reachable — if its upstream were truly down no
//! event would arrive at all. **Maintenance windows** suppress event alerts entirely
//! (the event is recorded with `action='suppressed'`).
//!
//! Events whose source correlates to no node are recorded (rule-authoring material,
//! pruned at 24h) but never evaluated — alerts need a node identity, and skipping
//! evaluation also caps regex work under spoofed floods.
//!
//! In-memory lifecycle state (active event alerts, match counters) is lost on core
//! restart — the next matching event re-fires, consistent with the active-alert map.

use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use futures::stream::{Stream, StreamExt};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_alert::Alert;
use yagra_bus::{EventKind, EventMsg};
use yagra_common::{trap_oid_name, CheckId, NodeId, NodeState, Severity};
use yagra_telemetry::CancellationToken;

use crate::alerts::{check_id, AlertManager, Notifier, NotifyAction};
use crate::history::AlertHistoryStore;
use crate::logstore::LogStore;
use crate::repo::NodeRepo;

/// Identical-event burst dedup: a repeat of the same (kind, origin, message) within this
/// window is dropped before the DB write. Transport-level dedup, distinct from alert dedup.
const DEDUP_WINDOW_MS: i64 = 5_000;
/// Bounded size of the burst-dedup window.
const DEDUP_CAP: usize = 4096;
/// TTL sweeper cadence.
const SWEEP_INTERVAL: Duration = Duration::from_secs(15);
/// Matched events follow the alert-history retention.
pub const MATCHED_RETENTION_SECS: i64 = 90 * 86_400;
/// Unmatched events exist for rule authoring only.
pub const UNMATCHED_RETENTION_SECS: i64 = 86_400;
/// Bounded queue between the (single) event matcher and the async batch persist writer. The event
/// log is a best-effort observational tier (ADR-024): sustained overload sheds the newest event
/// rather than blocking the matcher or growing memory unbounded.
pub const PERSIST_CHANNEL_CAP: usize = 8192;
/// Largest batch the persist writer flushes at once (well under Postgres' 65535-parameter ceiling
/// at 16 columns/row).
const PERSIST_BATCH_MAX: usize = 500;
/// Bounded queue between the (single) event matcher and the async **action** writer (S10). The
/// matcher plans alert state changes synchronously under its locks, then hands the resulting
/// fire/resolve side effects (alert-history write + notification delivery) to this channel so they
/// run off the hot path — under an event storm (a real incident) the matcher keeps matching while
/// the writer batches history INSERTs and delivers notifications in parallel. Unlike the persist
/// queue this **never sheds**: the alert-history audit trail must be preserved and the notifier
/// needs FIFO fire→resolve order, so a full queue applies backpressure (still strictly better than
/// the old inline I/O, which serialized every DB round-trip and vendor call on the matcher).
pub const ACTION_CHANNEL_CAP: usize = 8192;
/// Largest action batch the writer records to `alert_history` in one multi-row INSERT.
const ACTION_BATCH_MAX: usize = 500;

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

// ─── Storage ────────────────────────────────────────────────────────────────────────

/// A webhook ingest source, as served by the API (never includes the token hash).
#[derive(Debug, Clone, Serialize)]
pub struct EventSourceView {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub enabled: bool,
    pub node_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// A stored event rule (API shape; the engine compiles enabled ones).
#[derive(Debug, Clone, Serialize)]
pub struct StoredEventRule {
    pub id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub source_kind: Option<String>,
    pub source_id: Option<Uuid>,
    pub node_id: Option<Uuid>,
    pub match_kind: String,
    pub pattern: String,
    pub clear_pattern: Option<String>,
    pub severity: String,
    pub ttl_secs: i32,
    pub min_count: i32,
    pub window_secs: i32,
    pub created_at: DateTime<Utc>,
}

/// Rule create/update parameters (validated at the API edge).
pub struct RuleParams<'a> {
    pub name: &'a str,
    pub enabled: bool,
    pub source_kind: Option<&'a str>,
    pub source_id: Option<Uuid>,
    pub node_id: Option<Uuid>,
    pub match_kind: &'a str,
    pub pattern: &'a str,
    pub clear_pattern: Option<&'a str>,
    pub severity: &'a str,
    pub ttl_secs: i32,
    pub min_count: i32,
    pub window_secs: i32,
}

/// One received event, as served by `GET /api/v1/events`.
#[derive(Debug, Clone, Serialize)]
pub struct EventRow {
    pub id: Uuid,
    pub kind: String,
    pub at_unix_ms: i64,
    pub recorded_at: DateTime<Utc>,
    pub source_ip: Option<String>,
    pub node_id: Option<Uuid>,
    pub source_id: Option<Uuid>,
    pub pool: Option<String>,
    pub facility: Option<i16>,
    pub syslog_severity: Option<i16>,
    pub hostname: Option<String>,
    pub app_name: Option<String>,
    pub trap_oid: Option<String>,
    /// Well-known MIB name for `trap_oid` (e.g. `linkDown`), derived at read time; `None`
    /// for syslog/webhook events or an OID outside the curated set.
    pub trap_name: Option<String>,
    pub varbinds: Option<serde_json::Value>,
    pub message: String,
    pub matched_rule_id: Option<Uuid>,
    pub action: String,
}

/// Per-node event count in one time bucket — the input to the Troubleshoot `event_storm` analysis.
#[derive(Debug, Clone)]
pub struct EventBucketCount {
    pub node_id: Uuid,
    /// Bucket start, Unix seconds.
    pub bucket_start_s: i64,
    pub count: i64,
}

/// Fire/clear churn for one (node, rule) pair — the input to the Troubleshoot `event_flap` analysis.
#[derive(Debug, Clone)]
pub struct EventFlapStat {
    pub node_id: Uuid,
    pub rule_id: Uuid,
    pub rule_name: String,
    /// `fired` + `refreshed` rows in the window.
    pub fires: i64,
    /// `cleared` rows in the window.
    pub clears: i64,
}

/// Per-node syslog-severity count — the input to the Troubleshoot `severity_shift` analysis.
#[derive(Debug, Clone)]
pub struct EventSeverityCount {
    pub node_id: Uuid,
    /// Syslog severity 0–7 (0 = emergency … 7 = debug).
    pub severity: i16,
    pub count: i64,
}

/// A high-volume unmatched-event signature — the input to the Troubleshoot `rule_gap` analysis.
#[derive(Debug, Clone)]
pub struct EventSignatureCount {
    pub kind: String,
    /// The clustering key (trap OID or syslog app-name).
    pub signature: String,
    pub count: i64,
    /// A representative node the signature was seen on (for the finding's node link).
    pub sample_node: Option<Uuid>,
}

/// Authentication-signal volume from one source — the input to the Troubleshoot `auth_probe` analysis.
#[derive(Debug, Clone)]
pub struct EventAuthSource {
    pub source_ip: Option<String>,
    pub node_id: Option<Uuid>,
    pub count: i64,
}

/// Which categorical dimension a `/events/stats` aggregation groups by. A typed enum (never
/// interpolated raw) so only a fixed column name reaches SQL / LogsQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventStatGroup {
    /// By event kind (syslog / trap / webhook).
    Kind,
    /// By pipeline outcome (`action`: fired / refreshed / cleared / suppressed / info / none).
    Action,
    /// By trap OID (the display `label` carries the well-known MIB name when known).
    Trap,
    /// By source: the correlated inventory node when known, else the raw source IP.
    Source,
}

impl EventStatGroup {
    /// Parse the `group_by` query value; `None` for an unknown value (`time` is handled separately).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "kind" => Some(Self::Kind),
            "action" => Some(Self::Action),
            "trap" => Some(Self::Trap),
            "source" => Some(Self::Source),
            _ => None,
        }
    }
}

/// One categorical `/events/stats` bucket: a stable `key` (for the React key + fallback display), an
/// optional display `label` resolved server-side (e.g. a trap's MIB name), an optional `node_id`
/// (set for source grouping when the source maps to an inventory node, so the UI resolves its name
/// — no raw UUID rule), and the row count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventStatBucket {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<Uuid>,
    pub count: i64,
}

/// One time bucket for the `/events/stats?group_by=time` volume series: a bucket-start timestamp
/// (Unix ms), the total count, and — when `split=kind` — the per-kind breakdown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventTimeBucket {
    pub ts_unix_ms: i64,
    pub count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by_kind: Option<std::collections::BTreeMap<String, i64>>,
}

/// One received event queued for the async batch persist writer. Carries the raw [`EventMsg`] plus
/// the correlation/planning outputs the matcher derived, so the writer can fan it out to
/// PostgreSQL and/or the log store without re-deriving anything.
#[derive(Debug, Clone)]
pub struct PersistRecord {
    pub msg: EventMsg,
    pub node_id: Option<Uuid>,
    pub source_id: Option<Uuid>,
    pub matched_rule_id: Option<Uuid>,
    pub action: &'static str,
}

impl PersistRecord {
    /// Whether this row participated in the alert lifecycle. When the log store is enabled these
    /// are the only rows still written to PostgreSQL (ADR-024 Contract) — the rest live in the log
    /// store only.
    #[must_use]
    fn is_alert_linked(&self) -> bool {
        matches!(
            self.action,
            "fired" | "refreshed" | "cleared" | "suppressed"
        )
    }
}

/// One planned alert side effect queued for the async action writer (S10): a fire/resolve/suppress
/// to record in history and forward to the notifier, off the matcher's hot path. `reason` labels a
/// resolve (`"clear"`/`"ttl"`/`"manual"`) for the resolved-total counter.
pub struct EventAction {
    pub action: NotifyAction,
    pub reason: &'static str,
}

/// Outcome of verifying a webhook source's bearer token.
#[derive(Debug, PartialEq, Eq)]
pub enum TokenVerify {
    /// Token matches; carries the source's optional node binding.
    Ok { node_id: Option<Uuid> },
    /// Source exists and is enabled, but the token doesn't match.
    BadToken,
    /// No such webhook source, or it is disabled (indistinguishable on purpose).
    UnknownOrDisabled,
}

/// Filters for the events list.
///
/// **Every time bound here is event time** — when the device says the event happened
/// (`events.at_unix_ms`), not when Yagra wrote the row (`events.recorded_at`). The two backends
/// used to disagree: the SQL builder filtered and ordered on `recorded_at` while VictoriaLogs
/// filtered `_time`, which it writes from `at_unix_ms`. The same query therefore returned
/// different rows in a different order depending on whether the log store was enabled, and
/// `stats_series_sql` bucketed on one clock while filtering on the other. Event time is the one
/// that can be unified: VictoriaLogs' `_time` is already written from `at_unix_ms` and cannot be
/// rewritten retroactively.
///
/// Consequence to keep in mind: store-and-forward backfill inserts rows with old event times, so
/// a row can appear *behind* a cursor a client has already paged past. That is inherent to
/// ordering a log by event time, and matches what the VictoriaLogs path has always done.
///
/// **One difference between the backends is permitted and deliberate**: a plain `search` term is a
/// substring here (`ILIKE '%term%'`) but a whole-token phrase on VictoriaLogs, so a mid-word term
/// finds fewer rows on a log-store deployment. Making LogsQL do substring costs 300× (measured:
/// a 24h aggregate 19ms → 5.6s, and the paged search exceeded VictoriaLogs' 30s query ceiling),
/// because an inverted word index cannot serve a leading substring without a full scan. The
/// operator's escape hatch is `regex`, which reaches inside tokens on both backends.
#[derive(Debug, Default)]
pub struct EventFilter {
    /// Keyset pagination cursor (exclusive upper bound, event time). Distinct from `until` (a
    /// user-facing range end); when both are set the effective upper bound is their min.
    pub before: Option<DateTime<Utc>>,
    /// User-facing time-range lower bound (inclusive, event time), or `None` for unbounded.
    pub since: Option<DateTime<Utc>>,
    /// User-facing time-range upper bound (inclusive, event time), or `None` for unbounded.
    pub until: Option<DateTime<Utc>>,
    pub kind: Option<String>,
    pub node_id: Option<Uuid>,
    pub matched: Option<bool>,
    /// Case-insensitive substring matched against source (node name / IP) or message. When
    /// `regex` is set, `search` is instead a regular expression matched against the message only.
    pub search: Option<String>,
    /// Interpret `search` as a regular expression (message-only) rather than a substring.
    pub regex: bool,
}

fn generate_token() -> String {
    let bytes: [u8; 32] = rand::random();
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Constant-time equality so token verification doesn't leak match length via timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The shared `WHERE` predicate for the event log + summary-stats queries. Binds $1..=$8:
/// $1 before (paging cursor), $2 since, $3 until, $4 kind, $5 node_id, $6 matched, $7 search,
/// $8 regex. Kept in one place so `list_events` and `/events/stats` filter identically (the
/// dashboard summaries must line up with the log). Uses the `e` (events) / `n` (nodes) aliases —
/// every consumer joins `nodes n` (the name search needs it).
///
/// The three time bounds are **event time** in epoch milliseconds, matching what the VictoriaLogs
/// builder filters on (`logstore::build_filter_part`) — see [`EventFilter`] for why. Callers bind
/// them with [`ms_bound`]. `events_at_idx` (migration 0055) serves the ordering and the range.
pub(crate) const EVENT_FILTER_WHERE: &str = "($1::bigint IS NULL OR e.at_unix_ms < $1) \
     AND ($2::bigint IS NULL OR e.at_unix_ms >= $2) \
     AND ($3::bigint IS NULL OR e.at_unix_ms <= $3) \
     AND ($4::text IS NULL OR e.kind = $4) \
     AND ($5::uuid IS NULL OR e.node_id = $5) \
     AND ($6::boolean IS NULL OR (e.matched_rule_id IS NOT NULL) = $6) \
     AND ($7::text IS NULL \
          OR ($8::boolean = FALSE AND (e.message ILIKE '%' || $7 || '%' \
                                       OR host(e.source_ip) ILIKE '%' || $7 || '%' \
                                       OR n.name ILIKE '%' || $7 || '%')) \
          OR ($8::boolean = TRUE AND e.message ~* $7))";

/// A time bound as the epoch milliseconds [`EVENT_FILTER_WHERE`] compares against. One helper so
/// the three bounds can never be bound in different units.
fn ms_bound(at: Option<DateTime<Utc>>) -> Option<i64> {
    at.map(|t| t.timestamp_millis())
}

/// Build the keyset-paged event-list SQL. Binds are $1..=$8 (filter) + $9 (page size).
///
/// Extracted like the two stats builders so the **ordering column** is assertable: it has to be the
/// one `EVENT_FILTER_WHERE`'s cursor compares against, and the same one VictoriaLogs sorts by, or
/// paging skips and repeats rows. See `logstore::tests::both_backends_filter_on_event_time_…`.
fn list_events_sql() -> String {
    format!(
        "SELECT e.id, e.kind, e.at_unix_ms, e.recorded_at, host(e.source_ip) AS source_ip, \
                e.node_id, e.source_id, e.pool, e.facility, e.syslog_severity, e.hostname, \
                e.app_name, e.trap_oid, e.varbinds, e.message, e.matched_rule_id, e.action \
         FROM events e LEFT JOIN nodes n ON n.id = e.node_id \
         WHERE {EVENT_FILTER_WHERE} \
         ORDER BY e.at_unix_ms DESC LIMIT $9"
    )
}

/// Build the categorical `/events/stats` SQL for a group dimension. All identifiers are fixed
/// (chosen by the enum, never from the request); binds are $1..=$8 (filter) + $9 (row cap).
fn stats_grouped_sql(group: EventStatGroup) -> String {
    let (select, group_by, extra) = match group {
        EventStatGroup::Kind => ("e.kind AS key", "e.kind", ""),
        EventStatGroup::Action => ("e.action AS key", "e.action", ""),
        EventStatGroup::Trap => (
            "e.trap_oid AS key",
            "e.trap_oid",
            " AND e.trap_oid IS NOT NULL",
        ),
        EventStatGroup::Source => (
            "e.node_id AS node_id, host(e.source_ip) AS source_ip",
            "e.node_id, host(e.source_ip)",
            "",
        ),
    };
    format!(
        "SELECT {select}, count(*) AS n \
         FROM events e LEFT JOIN nodes n ON n.id = e.node_id \
         WHERE {EVENT_FILTER_WHERE}{extra} \
         GROUP BY {group_by} ORDER BY n DESC LIMIT $9"
    )
}

/// Build the time-series `/events/stats` SQL. Buckets on event time (`at_unix_ms`) into $9-wide
/// windows; binds are $1..=$8 (filter) + $9 (bucket seconds).
fn stats_series_sql(split_kind: bool) -> String {
    let (select_kind, group_kind) = if split_kind {
        (", e.kind AS kind", ", e.kind")
    } else {
        ("", "")
    };
    format!(
        "SELECT (e.at_unix_ms / 1000 / $9) * $9 AS bucket{select_kind}, count(*) AS n \
         FROM events e LEFT JOIN nodes n ON n.id = e.node_id \
         WHERE {EVENT_FILTER_WHERE} \
         GROUP BY bucket{group_kind} ORDER BY bucket ASC"
    )
}

/// PostgreSQL persistence for event sources, rules, and the event log.
pub struct EventRepo {
    pool: PgPool,
}

impl EventRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    // ── Sources ──

    pub async fn list_sources(&self) -> anyhow::Result<Vec<EventSourceView>> {
        let rows = sqlx::query(
            "SELECT id, name, kind, enabled, node_id, created_at \
             FROM event_sources ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(EventSourceView {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    kind: row.try_get("kind")?,
                    enabled: row.try_get("enabled")?,
                    node_id: row.try_get("node_id")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect()
    }

    /// Create a webhook source; returns its id and the **plaintext token (shown once)**.
    pub async fn create_source(
        &self,
        name: &str,
        node_id: Option<Uuid>,
    ) -> anyhow::Result<(Uuid, String)> {
        let id = Uuid::new_v4();
        let token = generate_token();
        sqlx::query(
            "INSERT INTO event_sources (id, name, kind, enabled, node_id, token_hash) \
             VALUES ($1, $2, 'webhook', true, $3, $4)",
        )
        .bind(id)
        .bind(name)
        .bind(node_id)
        .bind(hash_token(&token))
        .execute(&self.pool)
        .await?;
        Ok((id, token))
    }

    /// Full-replace update of a source's editable fields (token untouched).
    pub async fn update_source(
        &self,
        id: Uuid,
        name: &str,
        enabled: bool,
        node_id: Option<Uuid>,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE event_sources SET name = $2, enabled = $3, node_id = $4, updated_at = now() \
             WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(enabled)
        .bind(node_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Replace the source's token; returns the new **plaintext token (shown once)**,
    /// or `None` if the source doesn't exist.
    pub async fn rotate_token(&self, id: Uuid) -> anyhow::Result<Option<String>> {
        let token = generate_token();
        let res = sqlx::query(
            "UPDATE event_sources SET token_hash = $2, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(hash_token(&token))
        .execute(&self.pool)
        .await?;
        Ok((res.rows_affected() > 0).then_some(token))
    }

    pub async fn delete_source(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM event_sources WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Verify a webhook source's bearer token (constant-time hash compare).
    pub async fn verify_token(&self, id: Uuid, token: &str) -> anyhow::Result<TokenVerify> {
        let row = sqlx::query(
            "SELECT token_hash, enabled, node_id FROM event_sources \
             WHERE id = $1 AND kind = 'webhook'",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(TokenVerify::UnknownOrDisabled);
        };
        if !row.try_get::<bool, _>("enabled")? {
            return Ok(TokenVerify::UnknownOrDisabled);
        }
        let stored: Option<String> = row.try_get("token_hash")?;
        let Some(stored) = stored else {
            return Ok(TokenVerify::UnknownOrDisabled);
        };
        if constant_time_eq(stored.as_bytes(), hash_token(token).as_bytes()) {
            Ok(TokenVerify::Ok {
                node_id: row.try_get("node_id")?,
            })
        } else {
            Ok(TokenVerify::BadToken)
        }
    }

    // ── Rules ──

    pub async fn list_rules(&self) -> anyhow::Result<Vec<StoredEventRule>> {
        let rows = sqlx::query(
            "SELECT id, name, enabled, source_kind, source_id, node_id, match_kind, pattern, \
                    clear_pattern, severity, ttl_secs, min_count, window_secs, created_at \
             FROM event_rules ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(StoredEventRule {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    enabled: row.try_get("enabled")?,
                    source_kind: row.try_get("source_kind")?,
                    source_id: row.try_get("source_id")?,
                    node_id: row.try_get("node_id")?,
                    match_kind: row.try_get("match_kind")?,
                    pattern: row.try_get("pattern")?,
                    clear_pattern: row.try_get("clear_pattern")?,
                    severity: row.try_get("severity")?,
                    ttl_secs: row.try_get("ttl_secs")?,
                    min_count: row.try_get("min_count")?,
                    window_secs: row.try_get("window_secs")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect()
    }

    pub async fn create_rule(&self, p: &RuleParams<'_>) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO event_rules (id, name, enabled, source_kind, source_id, node_id, \
             match_kind, pattern, clear_pattern, severity, ttl_secs, min_count, window_secs) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(id)
        .bind(p.name)
        .bind(p.enabled)
        .bind(p.source_kind)
        .bind(p.source_id)
        .bind(p.node_id)
        .bind(p.match_kind)
        .bind(p.pattern)
        .bind(p.clear_pattern)
        .bind(p.severity)
        .bind(p.ttl_secs)
        .bind(p.min_count)
        .bind(p.window_secs)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    pub async fn update_rule(&self, id: Uuid, p: &RuleParams<'_>) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE event_rules SET name = $2, enabled = $3, source_kind = $4, source_id = $5, \
             node_id = $6, match_kind = $7, pattern = $8, clear_pattern = $9, severity = $10, \
             ttl_secs = $11, min_count = $12, window_secs = $13, updated_at = now() \
             WHERE id = $1",
        )
        .bind(id)
        .bind(p.name)
        .bind(p.enabled)
        .bind(p.source_kind)
        .bind(p.source_id)
        .bind(p.node_id)
        .bind(p.match_kind)
        .bind(p.pattern)
        .bind(p.clear_pattern)
        .bind(p.severity)
        .bind(p.ttl_secs)
        .bind(p.min_count)
        .bind(p.window_secs)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn delete_rule(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM event_rules WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    // ── Event log ──

    /// Persist a batch of received events in one multi-row INSERT (best-effort — the caller runs
    /// off the matcher's hot path, and a DB hiccup must not stop alerting). Returns rows inserted.
    pub async fn insert_events_batch(&self, records: &[&PersistRecord]) -> anyhow::Result<u64> {
        if records.is_empty() {
            return Ok(0);
        }
        let mut qb = sqlx::QueryBuilder::new(
            "INSERT INTO events (id, kind, at_unix_ms, source_ip, node_id, source_id, pool, \
             facility, syslog_severity, hostname, app_name, trap_oid, varbinds, message, \
             matched_rule_id, action) ",
        );
        qb.push_values(records.iter(), |mut b, r| {
            let m = &r.msg;
            let varbinds = (!m.varbinds.is_empty())
                .then(|| serde_json::to_value(&m.varbinds).unwrap_or(serde_json::Value::Null));
            b.push_bind(m.event_id)
                .push_bind(m.kind.as_str())
                .push_bind(m.at_unix_ms);
            // `events.source_ip` is INET; bind the text form and cast (mirrors the `$n::inet` in
            // the old single-row insert).
            b.push_bind(m.source_ip.map(|ip| ip.to_string()))
                .push_unseparated("::inet");
            b.push_bind(r.node_id)
                .push_bind(r.source_id)
                .push_bind(m.pool.clone())
                .push_bind(m.facility.map(i16::from))
                .push_bind(m.syslog_severity.map(i16::from))
                .push_bind(m.hostname.clone())
                .push_bind(m.app_name.clone())
                .push_bind(m.trap_oid.clone())
                .push_bind(varbinds)
                .push_bind(m.message.clone())
                .push_bind(r.matched_rule_id)
                .push_bind(r.action);
        });
        Ok(qb.build().execute(&self.pool).await?.rows_affected())
    }

    /// Keyset-paged event list, newest first by **event time** — the same ordering and the same
    /// cursor column the VictoriaLogs path uses (`| sort by (_time) desc`), so a deployment with
    /// the log store enabled and one without return the same page for the same request.
    pub async fn list_events(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventRow>> {
        let rows = sqlx::query(&list_events_sql())
            .bind(ms_bound(filter.before))
            .bind(ms_bound(filter.since))
            .bind(ms_bound(filter.until))
            .bind(filter.kind.as_deref())
            .bind(filter.node_id)
            .bind(filter.matched)
            .bind(filter.search.as_deref())
            .bind(filter.regex)
            .bind(limit.clamp(1, 500))
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                let trap_oid: Option<String> = row.try_get("trap_oid")?;
                let trap_name = trap_oid
                    .as_deref()
                    .and_then(trap_oid_name)
                    .map(str::to_owned);
                Ok(EventRow {
                    id: row.try_get("id")?,
                    kind: row.try_get("kind")?,
                    at_unix_ms: row.try_get("at_unix_ms")?,
                    recorded_at: row.try_get("recorded_at")?,
                    source_ip: row.try_get("source_ip")?,
                    node_id: row.try_get("node_id")?,
                    source_id: row.try_get("source_id")?,
                    pool: row.try_get("pool")?,
                    facility: row.try_get("facility")?,
                    syslog_severity: row.try_get("syslog_severity")?,
                    hostname: row.try_get("hostname")?,
                    app_name: row.try_get("app_name")?,
                    trap_oid,
                    trap_name,
                    varbinds: row.try_get("varbinds")?,
                    message: row.try_get("message")?,
                    matched_rule_id: row.try_get("matched_rule_id")?,
                    action: row.try_get("action")?,
                })
            })
            .collect()
    }

    // ── Troubleshoot analytics (ADR-022 event/flow increment) ──
    // Read-only aggregates over `events` for the passive-monitoring analyses. All are parameterized
    // (never string-interpolated) and windowed by `at_unix_ms`. NB: when the VictoriaLogs log store
    // is enabled, PostgreSQL retains only alert-linked rows (ADR-024) — so the count-based analyses
    // (`event_storm`/`severity_shift`/`rule_gap`) then see the alert-linked subset, while `event_flap`
    // keys on fired/cleared rows (always alert-linked, hence complete). Analyses note this in-summary.

    /// Per-(node, time-bucket) event counts across `[from_ms, to_ms]`. Uncorrelated events (no node)
    /// are excluded — an event storm is attributed to a device.
    pub async fn event_counts_by_bucket(
        &self,
        from_ms: i64,
        to_ms: i64,
        bucket_secs: i64,
    ) -> anyhow::Result<Vec<EventBucketCount>> {
        let b = bucket_secs.max(1);
        let rows = sqlx::query(
            "SELECT node_id, (at_unix_ms / 1000 / $3) * $3 AS bucket, count(*) AS n \
             FROM events \
             WHERE node_id IS NOT NULL AND at_unix_ms >= $1 AND at_unix_ms <= $2 \
             GROUP BY node_id, bucket",
        )
        .bind(from_ms)
        .bind(to_ms)
        .bind(b)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(EventBucketCount {
                    node_id: row.try_get("node_id")?,
                    bucket_start_s: row.try_get::<i64, _>("bucket")?,
                    count: row.try_get::<i64, _>("n")?,
                })
            })
            .collect()
    }

    /// Fire/clear churn per (node, rule) across `[from_ms, to_ms]` — the raw material for
    /// `event_flap` (repeated linkDown/linkUp, BGP session churn). Only alert-linked rows.
    pub async fn event_flap_stats(
        &self,
        from_ms: i64,
        to_ms: i64,
    ) -> anyhow::Result<Vec<EventFlapStat>> {
        let rows = sqlx::query(
            "SELECT e.node_id, e.matched_rule_id, COALESCE(r.name, 'rule') AS rule_name, \
                    count(*) FILTER (WHERE e.action IN ('fired','refreshed')) AS fires, \
                    count(*) FILTER (WHERE e.action = 'cleared') AS clears \
             FROM events e LEFT JOIN event_rules r ON r.id = e.matched_rule_id \
             WHERE e.node_id IS NOT NULL AND e.matched_rule_id IS NOT NULL \
               AND e.at_unix_ms >= $1 AND e.at_unix_ms <= $2 \
             GROUP BY e.node_id, e.matched_rule_id, r.name",
        )
        .bind(from_ms)
        .bind(to_ms)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(EventFlapStat {
                    node_id: row.try_get("node_id")?,
                    rule_id: row.try_get("matched_rule_id")?,
                    rule_name: row.try_get("rule_name")?,
                    fires: row.try_get::<i64, _>("fires")?,
                    clears: row.try_get::<i64, _>("clears")?,
                })
            })
            .collect()
    }

    /// Per-(node, syslog-severity) counts across `[from_ms, to_ms]` — the input to `severity_shift`.
    pub async fn event_severity_counts(
        &self,
        from_ms: i64,
        to_ms: i64,
    ) -> anyhow::Result<Vec<EventSeverityCount>> {
        let rows = sqlx::query(
            "SELECT node_id, syslog_severity, count(*) AS n \
             FROM events \
             WHERE kind = 'syslog' AND node_id IS NOT NULL AND syslog_severity IS NOT NULL \
               AND at_unix_ms >= $1 AND at_unix_ms <= $2 \
             GROUP BY node_id, syslog_severity",
        )
        .bind(from_ms)
        .bind(to_ms)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(EventSeverityCount {
                    node_id: row.try_get("node_id")?,
                    severity: row.try_get::<i16, _>("syslog_severity")?,
                    count: row.try_get::<i64, _>("n")?,
                })
            })
            .collect()
    }

    /// Top unmatched-event signatures (trap OID or syslog app-name) across `[from_ms, to_ms]` —
    /// the coverage gaps `rule_gap` surfaces. Unmatched rows only.
    pub async fn event_unmatched_signatures(
        &self,
        from_ms: i64,
        to_ms: i64,
        limit: i64,
    ) -> anyhow::Result<Vec<EventSignatureCount>> {
        let rows = sqlx::query(
            // NB: PostgreSQL has no min/max aggregate for `uuid`, so pick a representative node via
            // the text form — the canonical lowercase-hyphenated uuid sorts identically to the binary
            // ordering, and NULL node_ids are ignored by the aggregate (sample_node stays optional).
            "SELECT kind, COALESCE(trap_oid, app_name) AS sig, count(*) AS n, \
                    min(node_id::text)::uuid AS sample_node \
             FROM events \
             WHERE matched_rule_id IS NULL AND COALESCE(trap_oid, app_name) IS NOT NULL \
               AND at_unix_ms >= $1 AND at_unix_ms <= $2 \
             GROUP BY kind, sig ORDER BY n DESC LIMIT $3",
        )
        .bind(from_ms)
        .bind(to_ms)
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(EventSignatureCount {
                    kind: row.try_get("kind")?,
                    signature: row.try_get("sig")?,
                    count: row.try_get::<i64, _>("n")?,
                    sample_node: row.try_get("sample_node")?,
                })
            })
            .collect()
    }

    /// Authentication-signal volume grouped by source across `[from_ms, to_ms]` — the input to
    /// `auth_probe` (authenticationFailure traps + auth-failure syslog).
    pub async fn event_auth_sources(
        &self,
        from_ms: i64,
        to_ms: i64,
        limit: i64,
    ) -> anyhow::Result<Vec<EventAuthSource>> {
        let rows = sqlx::query(
            "SELECT host(source_ip) AS src, node_id, count(*) AS n \
             FROM events \
             WHERE at_unix_ms >= $1 AND at_unix_ms <= $2 \
               AND (trap_oid = '1.3.6.1.6.3.1.1.5.5' \
                    OR message ILIKE '%authentication fail%' \
                    OR message ILIKE '%auth failure%' \
                    OR message ILIKE '%login fail%' \
                    OR message ILIKE '%failed password%') \
             GROUP BY src, node_id ORDER BY n DESC LIMIT $3",
        )
        .bind(from_ms)
        .bind(to_ms)
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(EventAuthSource {
                    source_ip: row.try_get("src")?,
                    node_id: row.try_get("node_id")?,
                    count: row.try_get::<i64, _>("n")?,
                })
            })
            .collect()
    }

    // ── Dashboard summary aggregates (`/events/stats`) ──
    // Fleet-wide counts honoring the full `EventFilter` (the same predicate as `list_events`), for
    // the dashboard passive-event widgets. When VictoriaLogs is enabled these run against the log
    // store instead (the API picks the path); the PG path is accurate within retention when it isn't.

    /// One categorical count aggregation over the events, honoring the full [`EventFilter`], ordered
    /// by count desc. `group` picks a fixed column (never interpolated raw). Binds mirror
    /// `list_events` ($1..=$8) plus the row cap ($9).
    pub async fn stats_grouped(
        &self,
        filter: &EventFilter,
        group: EventStatGroup,
        limit: i64,
    ) -> anyhow::Result<Vec<EventStatBucket>> {
        let sql = stats_grouped_sql(group);
        let rows = sqlx::query(&sql)
            .bind(ms_bound(filter.before))
            .bind(ms_bound(filter.since))
            .bind(ms_bound(filter.until))
            .bind(filter.kind.as_deref())
            .bind(filter.node_id)
            .bind(filter.matched)
            .bind(filter.search.as_deref())
            .bind(filter.regex)
            .bind(limit.clamp(1, 500))
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                let count = row.try_get::<i64, _>("n")?;
                Ok(match group {
                    EventStatGroup::Source => {
                        let node_id: Option<Uuid> = row.try_get("node_id")?;
                        let source_ip: Option<String> = row.try_get("source_ip")?;
                        // `key` is stable for the React key + the fallback display when the source
                        // maps to no node; the UI resolves `node_id` to a name when present.
                        let key = node_id
                            .map(|n| n.to_string())
                            .or_else(|| source_ip.clone())
                            .unwrap_or_default();
                        EventStatBucket {
                            key,
                            label: source_ip,
                            node_id,
                            count,
                        }
                    }
                    EventStatGroup::Trap => {
                        let key: String =
                            row.try_get::<Option<String>, _>("key")?.unwrap_or_default();
                        let label = trap_oid_name(&key).map(str::to_owned);
                        EventStatBucket {
                            key,
                            label,
                            node_id: None,
                            count,
                        }
                    }
                    _ => EventStatBucket {
                        key: row.try_get("key")?,
                        label: None,
                        node_id: None,
                        count,
                    },
                })
            })
            .collect()
    }

    /// The event-volume time series: counts bucketed into `bucket_secs`-wide windows (on event time
    /// `at_unix_ms`), honoring the full [`EventFilter`]; `split_kind` adds the per-kind breakdown.
    pub async fn stats_series(
        &self,
        filter: &EventFilter,
        bucket_secs: i64,
        split_kind: bool,
    ) -> anyhow::Result<Vec<EventTimeBucket>> {
        let b = bucket_secs.clamp(1, 86_400);
        let sql = stats_series_sql(split_kind);
        let rows = sqlx::query(&sql)
            .bind(ms_bound(filter.before))
            .bind(ms_bound(filter.since))
            .bind(ms_bound(filter.until))
            .bind(filter.kind.as_deref())
            .bind(filter.node_id)
            .bind(filter.matched)
            .bind(filter.search.as_deref())
            .bind(filter.regex)
            .bind(b)
            .fetch_all(&self.pool)
            .await?;
        // Fold (bucket, kind) rows into one `EventTimeBucket` per bucket (BTreeMap keeps time order).
        let mut buckets: std::collections::BTreeMap<
            i64,
            (i64, std::collections::BTreeMap<String, i64>),
        > = std::collections::BTreeMap::new();
        for row in rows {
            let bucket_s: i64 = row.try_get("bucket")?;
            let n: i64 = row.try_get("n")?;
            let entry = buckets.entry(bucket_s).or_default();
            entry.0 += n;
            if split_kind {
                let kind: String = row.try_get("kind")?;
                *entry.1.entry(kind).or_default() += n;
            }
        }
        Ok(buckets
            .into_iter()
            .map(|(bucket_s, (count, by))| EventTimeBucket {
                ts_unix_ms: bucket_s.saturating_mul(1000),
                count,
                by_kind: split_kind.then_some(by),
            })
            .collect())
    }

    /// Asymmetric retention: matched events keep the alert-history window; unmatched
    /// rows are rule-authoring material only. Returns (matched, unmatched) rows removed.
    pub async fn prune_old(&self) -> anyhow::Result<(u64, u64)> {
        let matched = sqlx::query(
            "DELETE FROM events WHERE matched_rule_id IS NOT NULL \
             AND recorded_at < now() - $1 * interval '1 second'",
        )
        .bind(MATCHED_RETENTION_SECS)
        .execute(&self.pool)
        .await?
        .rows_affected();
        let unmatched = sqlx::query(
            "DELETE FROM events WHERE matched_rule_id IS NULL \
             AND recorded_at < now() - $1 * interval '1 second'",
        )
        .bind(UNMATCHED_RETENTION_SECS)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok((matched, unmatched))
    }
}

// ─── Rule matching (pure) ───────────────────────────────────────────────────────────

/// A compiled match expression. Substring is checked before regex site-wide (cheap first);
/// matching is case-sensitive (use `(?i)` in a regex for case-insensitive).
#[derive(Debug, Clone)]
pub enum Matcher {
    Substring(String),
    Regex(regex::Regex),
}

impl Matcher {
    /// Whether `text` matches (also used by the API's rule-test endpoint).
    #[must_use]
    pub fn matches(&self, text: &str) -> bool {
        match self {
            Self::Substring(s) => text.contains(s.as_str()),
            Self::Regex(re) => re.is_match(text),
        }
    }
}

/// Compile a matcher, enforcing the same bounds as the DB CHECKs. Shared by the engine
/// snapshot, the API-edge validation, and the rule-test endpoint.
pub fn compile_matcher(match_kind: &str, pattern: &str) -> Result<Matcher, String> {
    if pattern.is_empty() || pattern.len() > 512 {
        return Err("pattern must be 1..=512 characters".to_owned());
    }
    match match_kind {
        "substring" => Ok(Matcher::Substring(pattern.to_owned())),
        "regex" => regex::RegexBuilder::new(pattern)
            .size_limit(1 << 20)
            .build()
            .map(Matcher::Regex)
            .map_err(|e| e.to_string()),
        other => Err(format!("unknown match kind {other:?}")),
    }
}

fn parse_severity(s: &str) -> Severity {
    match s {
        "critical" => Severity::Critical,
        "warning" => Severity::Warning,
        _ => Severity::Info,
    }
}

fn parse_kind(s: &str) -> Option<EventKind> {
    match s {
        "syslog" => Some(EventKind::Syslog),
        "trap" => Some(EventKind::Trap),
        "webhook" => Some(EventKind::Webhook),
        _ => None,
    }
}

/// A rule compiled for the hot path.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub id: Uuid,
    pub name: String,
    source_kind: Option<EventKind>,
    source_id: Option<Uuid>,
    node_id: Option<Uuid>,
    matcher: Matcher,
    clear_matcher: Option<Matcher>,
    pub severity: Severity,
    ttl_secs: u32,
    min_count: u32,
    window_secs: u32,
}

impl CompiledRule {
    /// Whether this rule applies to an event of `kind` from `source_id` on `node`.
    fn applies(&self, kind: EventKind, source: Option<Uuid>, node: Uuid) -> bool {
        self.source_kind.is_none_or(|k| k == kind)
            && self.source_id.is_none_or(|s| Some(s) == source)
            && self.node_id.is_none_or(|n| n == node)
    }
}

/// Compile a stored rule; `None` for disabled rules or a pattern that no longer compiles
/// (rejected at the API edge, so this only catches drift — logged by the caller).
fn compile_rule(stored: &StoredEventRule) -> Option<CompiledRule> {
    if !stored.enabled {
        return None;
    }
    let matcher = compile_matcher(&stored.match_kind, &stored.pattern).ok()?;
    let clear_matcher = match stored.clear_pattern.as_deref() {
        Some(p) => Some(compile_matcher(&stored.match_kind, p).ok()?),
        None => None,
    };
    Some(CompiledRule {
        id: stored.id,
        name: stored.name.clone(),
        source_kind: stored.source_kind.as_deref().and_then(parse_kind),
        source_id: stored.source_id,
        node_id: stored.node_id,
        matcher,
        clear_matcher,
        severity: parse_severity(&stored.severity),
        ttl_secs: u32::try_from(stored.ttl_secs).unwrap_or(1800),
        min_count: u32::try_from(stored.min_count).unwrap_or(1).max(1),
        window_secs: u32::try_from(stored.window_secs).unwrap_or(60).max(1),
    })
}

// ─── Engine ────────────────────────────────────────────────────────────────────────

/// The compiled-rules + address-map snapshot (refreshed by the 30s loop and inline
/// after rule/source edits).
#[derive(Default)]
struct Snapshot {
    rules: Vec<CompiledRule>,
    addresses: HashMap<IpAddr, Uuid>,
}

/// An active (raised, not yet resolved) event alert — tracks the TTL deadline so the
/// sweeper can expire it. Keyed by the alert's check id in [`Runtime::active`].
struct ActiveEvent {
    expires_at_ms: i64,
}

/// Mutable matching state. Never held across an await.
struct Runtime {
    /// Rolling match timestamps per (rule, node) for the min-count/window gate.
    counters: HashMap<(Uuid, Uuid), VecDeque<i64>>,
    /// Active event alerts by check id, with their TTL deadline.
    active: HashMap<CheckId, ActiveEvent>,
    /// Burst-dedup window: content-hash → last-seen ms.
    dedup_seen: HashMap<u64, i64>,
    dedup_order: VecDeque<u64>,
}

impl Runtime {
    fn new() -> Self {
        Self {
            counters: HashMap::new(),
            active: HashMap::new(),
            dedup_seen: HashMap::new(),
            dedup_order: VecDeque::new(),
        }
    }

    /// Whether this event is an identical repeat inside the dedup window.
    fn is_duplicate(&mut self, key: u64, now_ms: i64) -> bool {
        while self.dedup_order.len() >= DEDUP_CAP {
            if let Some(old) = self.dedup_order.pop_front() {
                self.dedup_seen.remove(&old);
            }
        }
        match self.dedup_seen.get(&key) {
            Some(&seen) if now_ms - seen < DEDUP_WINDOW_MS => true,
            _ => {
                if self.dedup_seen.insert(key, now_ms).is_none() {
                    self.dedup_order.push_back(key);
                }
                false
            }
        }
    }

    /// min-count/window gate: record a match and report whether the rule may fire.
    fn gate_passes(&mut self, rule: &CompiledRule, node: Uuid, now_ms: i64) -> bool {
        if rule.min_count <= 1 {
            return true;
        }
        let dq = self.counters.entry((rule.id, node)).or_default();
        let cutoff = now_ms - i64::from(rule.window_secs) * 1000;
        while dq.front().is_some_and(|&t| t < cutoff) {
            dq.pop_front();
        }
        dq.push_back(now_ms);
        dq.len() >= rule.min_count as usize
    }
}

/// What the planning step decided. The manager-side raise/resolve is done **inside**
/// `plan` under the runtime lock (atomically with `Runtime::active`, so the two active
/// sets can never diverge — see the sweep/plan race guard); `actions` carries the
/// resulting [`NotifyAction`]s so the async step only does the I/O (history + notify),
/// each tagged with the metric reason for a resolve ("clear"; ignored for a fire).
#[derive(Default)]
struct Planned {
    row_action: &'static str,
    matched_rule: Option<Uuid>,
    actions: Vec<(NotifyAction, &'static str)>,
}

/// Row-action precedence: the strongest outcome describes the event.
fn action_rank(action: &str) -> u8 {
    match action {
        "fired" => 5,
        "refreshed" => 4,
        "cleared" => 3,
        "suppressed" => 2,
        "info" => 1,
        _ => 0,
    }
}

/// The webhook source binding the ingest endpoint resolved (token already verified).
pub struct SourceBinding {
    pub source_id: Uuid,
    pub node_id: Option<Uuid>,
}

/// Webhook ingest rate limit per source (requests/second; burst 2×).
const INGEST_RATE_PER_SOURCE: f64 = 10.0;

/// Orchestrates the event pipeline. Sync matching state under short-lived locks; all
/// I/O (DB, history, notifications) happens after the locks are released.
pub struct EventEngine {
    repo: Arc<EventRepo>,
    alerts: Arc<AlertManager>,
    notifier: Arc<Notifier>,
    history: Arc<AlertHistoryStore>,
    snapshot: RwLock<Snapshot>,
    runtime: Mutex<Runtime>,
    /// Ingest token buckets per (already-verified) webhook source: (tokens, last-refill ms).
    ingest_rate: Mutex<HashMap<Uuid, (f64, i64)>>,
    /// Non-blocking handoff to the async batch persist writer (ADR-024). `None` in unit tests that
    /// exercise the pure planner only.
    persist_tx: Option<tokio::sync::mpsc::Sender<PersistRecord>>,
    /// Blocking handoff to the async action writer (S10): alert-history + notification I/O for
    /// planned fire/resolve actions runs off the matcher's hot path. `None` falls back to inline
    /// execution (unit tests / skeleton) so behavior is unchanged there.
    action_tx: Option<tokio::sync::mpsc::Sender<EventAction>>,
}

impl EventEngine {
    #[must_use]
    pub fn new(
        repo: Arc<EventRepo>,
        alerts: Arc<AlertManager>,
        notifier: Arc<Notifier>,
        history: Arc<AlertHistoryStore>,
        persist_tx: Option<tokio::sync::mpsc::Sender<PersistRecord>>,
        action_tx: Option<tokio::sync::mpsc::Sender<EventAction>>,
    ) -> Self {
        Self {
            repo,
            alerts,
            notifier,
            history,
            snapshot: RwLock::new(Snapshot::default()),
            runtime: Mutex::new(Runtime::new()),
            ingest_rate: Mutex::new(HashMap::new()),
            persist_tx,
            action_tx,
        }
    }

    /// Webhook ingest rate gate (per verified source, so the key space is bounded by
    /// operator-created sources). Called by the ingest endpoint before any DB write.
    #[must_use]
    pub fn ingest_allowed(&self, source_id: Uuid) -> bool {
        let now_ms = now_unix_ms();
        let burst = INGEST_RATE_PER_SOURCE * 2.0;
        let mut buckets = self.ingest_rate.lock().expect("ingest mutex poisoned");
        let (tokens, last) = buckets.entry(source_id).or_insert((burst, now_ms));
        let elapsed_ms = (now_ms - *last).max(0) as f64;
        *tokens = (*tokens + INGEST_RATE_PER_SOURCE * elapsed_ms / 1000.0).min(burst);
        *last = now_ms;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Reload the rules + address-map snapshot. Keeps the previous snapshot parts on a
    /// load failure (never downgrades to empty because the DB blinked).
    pub async fn reload(&self, nodes: &NodeRepo) {
        let rules = match self.repo.list_rules().await {
            Ok(stored) => {
                let compiled: Vec<CompiledRule> = stored.iter().filter_map(compile_rule).collect();
                let skipped = stored.iter().filter(|r| r.enabled).count() - compiled.len();
                if skipped > 0 {
                    tracing::warn!(skipped, "event rules failed to compile and were skipped");
                }
                Some(compiled)
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to load event rules; keeping previous snapshot");
                None
            }
        };
        let addresses = match nodes.address_map().await {
            Ok(map) => Some(map),
            Err(e) => {
                tracing::warn!(error = %e, "failed to load node address map; keeping previous snapshot");
                None
            }
        };
        let mut snap = self.snapshot.write().expect("snapshot rwlock poisoned");
        if let Some(rules) = rules {
            snap.rules = rules;
        }
        if let Some(addresses) = addresses {
            snap.addresses = addresses;
        }
    }

    /// Feed one event through the pipeline (bus consumer passes `source: None`;
    /// the webhook ingest endpoint passes its verified source binding).
    pub async fn handle_event(&self, msg: EventMsg, source: Option<SourceBinding>) {
        metrics::counter!("yagra_events_ingested_total", "kind" => msg.kind.as_str()).increment(1);
        let now_ms = now_unix_ms();

        // Burst dedup: identical (kind, origin, message) within the window.
        let dedup_key = {
            let mut h = DefaultHasher::new();
            msg.kind.as_str().hash(&mut h);
            msg.source_ip.hash(&mut h);
            source.as_ref().map(|s| s.source_id).hash(&mut h);
            msg.message.hash(&mut h);
            h.finish()
        };
        {
            let mut runtime = self.runtime.lock().expect("runtime mutex poisoned");
            if runtime.is_duplicate(dedup_key, now_ms) {
                metrics::counter!("yagra_events_deduped_total").increment(1);
                return;
            }
        }

        // Correlate: webhook source binding wins, else source-IP → inventory.
        let node_id: Option<Uuid> = source.as_ref().and_then(|s| s.node_id).or_else(|| {
            let snap = self.snapshot.read().expect("snapshot rwlock poisoned");
            msg.source_ip
                .and_then(|ip| snap.addresses.get(&ip).copied())
        });
        if node_id.is_none() {
            metrics::counter!("yagra_events_unmatched_node_total").increment(1);
        }

        // Plan under short locks, then execute the I/O.
        let planned = match node_id {
            Some(node) => self.plan(&msg, source.as_ref().map(|s| s.source_id), node, now_ms),
            None => Planned {
                row_action: "none",
                ..Planned::default()
            },
        };

        // Hand the raise/resolve side effects (history write + notification) to the async action
        // writer so the matcher isn't blocked on DB round-trips / vendor delivery under an event
        // storm (S10). The in-memory alert state already advanced under the plan lock; only the I/O
        // is deferred, in FIFO order, never dropped.
        for (action, reason) in planned.actions {
            self.dispatch_action(action, reason).await;
        }
        self.update_active_gauge();

        // Hand the event to the async batch writer for best-effort persistence (search/forensics,
        // ADR-024). Non-blocking: under sustained overload we shed the newest event rather than
        // block the matcher — alerts already fired above, so a dropped persist never loses an alert.
        if let Some(tx) = &self.persist_tx {
            let action = if planned.row_action.is_empty() {
                "none"
            } else {
                planned.row_action
            };
            let record = PersistRecord {
                msg,
                node_id,
                source_id: source.as_ref().map(|s| s.source_id),
                matched_rule_id: planned.matched_rule,
                action,
            };
            match tx.try_send(record) {
                Ok(()) => metrics::counter!("yagra_events_persist_enqueued_total").increment(1),
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    metrics::counter!("yagra_events_persist_dropped_total", "reason" => "channel_full")
                        .increment(1);
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    metrics::counter!("yagra_events_persist_dropped_total", "reason" => "closed")
                        .increment(1);
                }
            }
        }
    }

    /// The pure matching/planning step: clear pass before fire pass (a message hitting
    /// both resolves rather than flaps), all rules evaluated (no first-match-wins).
    fn plan(&self, msg: &EventMsg, source: Option<Uuid>, node: Uuid, now_ms: i64) -> Planned {
        let node_id = NodeId::from(node);
        let in_maintenance = self.alerts.in_maintenance(node_id);
        let snap = self.snapshot.read().expect("snapshot rwlock poisoned");
        let mut runtime = self.runtime.lock().expect("runtime mutex poisoned");
        let mut planned = Planned {
            row_action: "none",
            ..Planned::default()
        };
        let bump = |planned: &mut Planned, action: &'static str, rule: Uuid| {
            if action_rank(action) > action_rank(planned.row_action) {
                planned.row_action = action;
                planned.matched_rule = Some(rule);
            }
        };

        // For SNMP traps, prepend the resolved MIB name to the text rules match against, so a
        // rule (built-in or user-authored) can match by name (`linkDown`) as well as by raw OID.
        // The stored/displayed message is untouched; matching only sees the enriched copy, and
        // resolution is centralized in core so it works regardless of poller version (N-1 safe).
        let haystack: Cow<str> = match (msg.kind, msg.trap_oid.as_deref().and_then(trap_oid_name)) {
            (EventKind::Trap, Some(name)) => Cow::Owned(format!("{name} {}", msg.message)),
            _ => Cow::Borrowed(msg.message.as_str()),
        };

        for rule in snap
            .rules
            .iter()
            .filter(|r| r.applies(msg.kind, source, node))
        {
            let fire_hit = rule.matcher.matches(&haystack);
            let clear_hit = rule
                .clear_matcher
                .as_ref()
                .is_some_and(|m| m.matches(&haystack));
            if !fire_hit && !clear_hit {
                continue;
            }
            metrics::counter!("yagra_events_matched_total").increment(1);

            // Maintenance window: record the match, raise nothing (ADR-015 quality gate).
            if in_maintenance {
                bump(&mut planned, "suppressed", rule.id);
                continue;
            }

            let check = check_id(node_id, &format!("event:{}", rule.id));

            // Clear takes precedence over fire for the same message (anti-flap). Remove
            // from BOTH active sets under the same lock so a concurrent sweep/plan can't
            // observe them diverged.
            if clear_hit {
                if runtime.active.remove(&check).is_some() {
                    if let Some(action) = self.alerts.resolve_event_alert(check) {
                        planned.actions.push((action, "clear"));
                    }
                }
                bump(&mut planned, "cleared", rule.id);
                continue;
            }

            // Info severity = record only; the alert engine has no Info state.
            if rule.severity == Severity::Info {
                bump(&mut planned, "info", rule.id);
                continue;
            }

            if !runtime.gate_passes(rule, node, now_ms) {
                // Counted toward the gate but below min-count — matched, not fired.
                bump(&mut planned, "info", rule.id);
                continue;
            }

            let deadline = now_ms + i64::from(rule.ttl_secs) * 1000;
            if let Some(active) = runtime.active.get_mut(&check) {
                // Already alerting: extend the TTL, no re-notification.
                active.expires_at_ms = deadline;
                bump(&mut planned, "refreshed", rule.id);
                continue;
            }
            let state = match rule.severity {
                Severity::Critical => NodeState::Critical,
                _ => NodeState::Warning,
            };
            let alert = Alert {
                node: node_id,
                check,
                severity: rule.severity,
                state,
                at_unix_ms: msg.at_unix_ms,
                root_cause: None,
                flapping: false,
                metric: format!("event:{}", rule.name),
                breach: None,
            };
            // Raise in the manager while holding the runtime lock, then mirror into
            // `runtime.active`. Because both sets are mutated together, the sweeper can
            // never resolve the manager alert while a fresh runtime entry lingers (which
            // would permanently suppress re-fires).
            match self.alerts.raise_event_alert(alert) {
                Some(action) => {
                    runtime.active.insert(
                        check,
                        ActiveEvent {
                            expires_at_ms: deadline,
                        },
                    );
                    planned.actions.push((action, "fire"));
                    bump(&mut planned, "fired", rule.id);
                }
                None => {
                    // Manager already had this alert active at the same severity (its dedup
                    // fired). Keep the TTL entry consistent and treat as a refresh.
                    runtime.active.insert(
                        check,
                        ActiveEvent {
                            expires_at_ms: deadline,
                        },
                    );
                    bump(&mut planned, "refreshed", rule.id);
                }
            }
        }
        planned
    }

    /// Execute the I/O for one planned/expired action: record history and forward to the
    /// notifier (the manager-side active-set mutation already happened under the lock).
    async fn run_action(&self, action: NotifyAction, resolve_reason: &'static str) {
        match &action {
            NotifyAction::Fire(alert) => {
                metrics::counter!("yagra_event_alerts_fired_total").increment(1);
                if let Err(e) = self.history.record(alert, false).await {
                    tracing::warn!(error = %e, "failed to record event alert history");
                }
            }
            NotifyAction::Resolve(alert) => {
                if let Err(e) = self.history.record(alert, true).await {
                    tracing::warn!(error = %e, "failed to record event alert resolution");
                }
                metrics::counter!("yagra_event_alerts_resolved_total", "reason" => resolve_reason)
                    .increment(1);
            }
            // Event alerts are never dependency-suppressed (a device emitting an event is
            // demonstrably reachable), so the event pipeline never produces a roll-up. Handled
            // defensively for exhaustiveness; the notifier still forwards it below.
            NotifyAction::Suppress(_) => {}
        }
        self.notifier.handle(action).await;
    }

    /// Hand a planned action to the async action writer (S10). Blocking send — never drops (the
    /// alert-history audit trail must survive and the notifier needs FIFO fire→resolve order); a
    /// full queue backpressures the matcher, which is still strictly better than the old inline I/O.
    /// Falls back to inline execution when no writer is wired (unit tests / skeleton) or if the
    /// writer has already shut down, so the record/notify always lands.
    async fn dispatch_action(&self, action: NotifyAction, reason: &'static str) {
        match &self.action_tx {
            Some(tx) => {
                if let Err(err) = tx.send(EventAction { action, reason }).await {
                    let EventAction { action, reason } = err.0;
                    self.run_action(action, reason).await;
                } else {
                    metrics::counter!("yagra_event_actions_enqueued_total").increment(1);
                }
            }
            None => self.run_action(action, reason).await,
        }
    }

    /// One sweeper pass: resolve TTL-expired alerts, prune stale gate counters. The
    /// runtime removal and the manager resolve happen together under the runtime lock
    /// (so a matching event arriving mid-sweep can't leave the two active sets diverged
    /// and permanently suppress re-fires); the I/O runs after the lock is released.
    pub async fn sweep(&self, now_ms: i64) {
        let actions: Vec<(NotifyAction, &'static str)> = {
            let mut runtime = self.runtime.lock().expect("runtime mutex poisoned");
            let expired: Vec<CheckId> = runtime
                .active
                .iter()
                .filter(|(_, a)| a.expires_at_ms <= now_ms)
                .map(|(c, _)| *c)
                .collect();
            let mut actions = Vec::new();
            for check in &expired {
                runtime.active.remove(check);
                if let Some(action) = self.alerts.resolve_event_alert(*check) {
                    actions.push((action, "ttl"));
                }
            }
            // Gate counters go stale once their newest entry ages past any window.
            let counter_cutoff = now_ms - 2 * 3600 * 1000;
            runtime
                .counters
                .retain(|_, dq| dq.back().is_some_and(|&t| t >= counter_cutoff));
            actions
        };
        for (action, reason) in actions {
            self.dispatch_action(action, reason).await;
        }
        self.update_active_gauge();
    }

    /// Manually close an event alert by its check id. Removes from both active sets under
    /// the runtime lock; resolves in the manager even if the engine lost its own entry
    /// (core-restart drift) so the close always lands.
    pub async fn close_alert(&self, check: CheckId) -> bool {
        let action = {
            let mut runtime = self.runtime.lock().expect("runtime mutex poisoned");
            runtime.active.remove(&check);
            self.alerts.resolve_event_alert(check)
        };
        match action {
            Some(action) => {
                self.dispatch_action(action, "manual").await;
                self.update_active_gauge();
                true
            }
            None => false,
        }
    }

    /// The persistence handle (shared with the API's CRUD handlers).
    #[must_use]
    pub fn repo(&self) -> &Arc<EventRepo> {
        &self.repo
    }

    fn update_active_gauge(&self) {
        let n = {
            let runtime = self.runtime.lock().expect("runtime mutex poisoned");
            runtime.active.len()
        };
        #[allow(clippy::cast_precision_loss)]
        metrics::gauge!("yagra_event_alerts_active").set(n as f64);
    }
}

/// Drain events off the bus into the engine. Returns when the stream ends.
///
/// When forwarding is configured (ADR-034), each message is also offered to the forwarder **before**
/// rule matching — so a destination receives the full firehose, unaffected by the burst dedup that
/// exists to keep alerts sane. `offer` never blocks: a full inlet drops the copy and counts it, so
/// forwarding can never slow intake or alerting.
pub async fn consume_events<S>(
    mut events: S,
    engine: Arc<EventEngine>,
    forward: Option<crate::forward::ForwardHandle>,
) where
    S: Stream<Item = EventMsg> + Unpin,
{
    while let Some(msg) = events.next().await {
        if let Some(forward) = forward.as_ref() {
            forward.offer(&msg);
        }
        engine.handle_event(msg, None).await;
    }
    tracing::warn!("event stream ended");
}

/// TTL sweeper loop (spawned in `run_live`).
pub async fn run_ttl_sweeper(engine: Arc<EventEngine>) {
    loop {
        tokio::time::sleep(SWEEP_INTERVAL).await;
        engine.sweep(now_unix_ms()).await;
    }
}

/// Flush a batch of queued events to the durable stores (ADR-024). PostgreSQL gets the firehose
/// when the log store is disabled, or only the alert-linked rows when it is enabled (Contract —
/// the log store then holds the full firehose for search). Best-effort: a store error is logged,
/// never propagated (alerts already fired synchronously in `handle_event`).
async fn flush_persist(
    repo: &EventRepo,
    logs: &Option<Arc<dyn LogStore>>,
    buf: &mut Vec<PersistRecord>,
) {
    if buf.is_empty() {
        return;
    }
    let pg: Vec<&PersistRecord> = if logs.is_some() {
        buf.iter().filter(|r| r.is_alert_linked()).collect()
    } else {
        buf.iter().collect()
    };
    if !pg.is_empty() {
        match repo.insert_events_batch(&pg).await {
            Ok(n) => {
                metrics::counter!("yagra_events_persisted_total", "store" => "postgres")
                    .increment(n);
            }
            Err(e) => tracing::warn!(error = %e, "batch-insert events to PostgreSQL failed"),
        }
    }
    if let Some(store) = logs {
        store.ingest_batch(buf).await;
        metrics::counter!("yagra_events_persisted_total", "store" => "victorialogs")
            .increment(buf.len() as u64);
    }
    buf.clear();
}

/// Async batch persist writer (ADR-024): drains the bounded persist queue and fans each batch out
/// to PostgreSQL and/or the log store off the matcher's hot path. Batches opportunistically (one
/// blocking `recv`, then a non-blocking drain up to [`PERSIST_BATCH_MAX`]). On shutdown it drains
/// and flushes what's queued (best-effort final flush) before returning.
pub async fn run_persist_writer(
    mut rx: tokio::sync::mpsc::Receiver<PersistRecord>,
    repo: Arc<EventRepo>,
    logs: Option<Arc<dyn LogStore>>,
    shutdown: CancellationToken,
) {
    let mut buf: Vec<PersistRecord> = Vec::with_capacity(PERSIST_BATCH_MAX);
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                while let Ok(rec) = rx.try_recv() {
                    buf.push(rec);
                    if buf.len() >= PERSIST_BATCH_MAX {
                        flush_persist(&repo, &logs, &mut buf).await;
                    }
                }
                flush_persist(&repo, &logs, &mut buf).await;
                break;
            }
            first = rx.recv() => {
                match first {
                    None => {
                        flush_persist(&repo, &logs, &mut buf).await;
                        break;
                    }
                    Some(rec) => {
                        buf.push(rec);
                        while buf.len() < PERSIST_BATCH_MAX {
                            match rx.try_recv() {
                                Ok(rec) => buf.push(rec),
                                Err(_) => break,
                            }
                        }
                        flush_persist(&repo, &logs, &mut buf).await;
                        metrics::gauge!("yagra_persist_queue_depth", "stream" => "events")
                            .set(rx.len() as f64);
                    }
                }
            }
        }
    }
}

/// Map a drained action batch to the `alert_history` rows to insert: a fire records `resolved=false`,
/// a resolve `resolved=true`, and a suppress records nothing (event alerts are never
/// dependency-suppressed, but the variant is handled for exhaustiveness). Pure — unit-tested.
fn history_rows(actions: &[EventAction]) -> Vec<(Alert, bool)> {
    actions
        .iter()
        .filter_map(|ea| match &ea.action {
            NotifyAction::Fire(alert) => Some((alert.clone(), false)),
            NotifyAction::Resolve(alert) => Some((alert.clone(), true)),
            NotifyAction::Suppress(_) => None,
        })
        .collect()
}

/// Flush a drained batch of alert actions (S10): one multi-row `alert_history` INSERT for all
/// fire/resolve rows, then per-action notification delivery in FIFO order (the notifier serializes
/// delivery internally). Best-effort on history: a DB error is logged, never propagated (the
/// in-memory alert state already advanced in the matcher). Fire/resolve counters mirror the inline
/// `run_action` path so metrics are identical whichever path executes.
async fn flush_actions(
    history: &AlertHistoryStore,
    notifier: &Notifier,
    buf: &mut Vec<EventAction>,
) {
    if buf.is_empty() {
        return;
    }
    let rows = history_rows(buf);
    if let Err(e) = history.record_batch(&rows).await {
        tracing::warn!(error = %e, count = rows.len(), "batch-record event alert history failed");
    }
    for ea in buf.drain(..) {
        match &ea.action {
            NotifyAction::Fire(_) => {
                metrics::counter!("yagra_event_alerts_fired_total").increment(1);
            }
            NotifyAction::Resolve(_) => {
                metrics::counter!("yagra_event_alerts_resolved_total", "reason" => ea.reason)
                    .increment(1);
            }
            NotifyAction::Suppress(_) => {}
        }
        notifier.handle(ea.action).await;
    }
}

/// Async writer for event-alert side effects (S10): drains the bounded action queue and runs
/// alert-history + notification I/O off the matcher's hot path. Batches history INSERTs (one
/// blocking `recv`, then a non-blocking drain up to [`ACTION_BATCH_MAX`]) so an event storm doesn't
/// serialize a PG round-trip per action on the matcher. Delivers notifications in FIFO order so a
/// fire always precedes its later resolve. On shutdown it drains and flushes what's queued.
pub async fn run_event_action_writer(
    mut rx: tokio::sync::mpsc::Receiver<EventAction>,
    history: Arc<AlertHistoryStore>,
    notifier: Arc<Notifier>,
    shutdown: CancellationToken,
) {
    let mut buf: Vec<EventAction> = Vec::with_capacity(ACTION_BATCH_MAX);
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                while let Ok(a) = rx.try_recv() {
                    buf.push(a);
                    if buf.len() >= ACTION_BATCH_MAX {
                        flush_actions(&history, &notifier, &mut buf).await;
                    }
                }
                flush_actions(&history, &notifier, &mut buf).await;
                break;
            }
            first = rx.recv() => {
                match first {
                    None => {
                        flush_actions(&history, &notifier, &mut buf).await;
                        break;
                    }
                    Some(a) => {
                        buf.push(a);
                        while buf.len() < ACTION_BATCH_MAX {
                            match rx.try_recv() {
                                Ok(a) => buf.push(a),
                                Err(_) => break,
                            }
                        }
                        flush_actions(&history, &notifier, &mut buf).await;
                        metrics::gauge!("yagra_persist_queue_depth", "stream" => "event_actions")
                            .set(rx.len() as f64);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_event_list_orders_and_pages_on_the_column_the_filter_cursors_on() {
        // These three have to be the same column or paging is broken: the cursor predicate, the
        // ORDER BY, and (in `stats_series_sql`) the bucketing. They were not — the predicate and
        // ordering used `recorded_at` while the bucketing used `at_unix_ms`.
        let sql = list_events_sql();
        assert!(sql.contains("ORDER BY e.at_unix_ms DESC LIMIT $9"), "{sql}");
        assert!(sql.contains(EVENT_FILTER_WHERE), "{sql}");
        assert!(EVENT_FILTER_WHERE.contains("e.at_unix_ms < $1"));
        assert!(stats_series_sql(false).contains("(e.at_unix_ms / 1000 / $9)"));
        // `recorded_at` is still selected and returned (it is real information), just never
        // filtered or ordered on.
        assert!(sql.contains("e.recorded_at,"), "{sql}");
    }

    #[test]
    fn ms_bound_converts_to_the_epoch_millis_the_predicate_compares() {
        let t = DateTime::parse_from_rfc3339("2026-07-28T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(ms_bound(Some(t)), Some(t.timestamp_millis()));
        assert_eq!(ms_bound(None), None);
    }

    #[test]
    fn stats_grouped_sql_uses_shared_filter_and_fixed_columns() {
        // Every group reuses the shared filter predicate and only fixed identifiers reach SQL.
        for g in [
            EventStatGroup::Kind,
            EventStatGroup::Action,
            EventStatGroup::Trap,
            EventStatGroup::Source,
        ] {
            let sql = stats_grouped_sql(g);
            assert!(sql.contains(EVENT_FILTER_WHERE), "{sql}");
            assert!(sql.contains("count(*) AS n"), "{sql}");
            assert!(sql.contains("ORDER BY n DESC LIMIT $9"), "{sql}");
        }
        // Trap grouping drops NULL OIDs; source grouping carries node_id + source_ip for the UI.
        assert!(stats_grouped_sql(EventStatGroup::Trap).contains("e.trap_oid IS NOT NULL"));
        assert!(
            stats_grouped_sql(EventStatGroup::Source).contains("host(e.source_ip) AS source_ip")
        );
        assert!(stats_grouped_sql(EventStatGroup::Kind).contains("GROUP BY e.kind"));
    }

    #[test]
    fn stats_series_sql_buckets_and_optionally_splits_by_kind() {
        let plain = stats_series_sql(false);
        assert!(
            plain.contains("(e.at_unix_ms / 1000 / $9) * $9 AS bucket"),
            "{plain}"
        );
        assert!(plain.contains(EVENT_FILTER_WHERE), "{plain}");
        assert!(
            plain.contains("GROUP BY bucket ORDER BY bucket ASC"),
            "{plain}"
        );
        assert!(!plain.contains("e.kind AS kind"), "{plain}");
        // split=kind adds the per-kind column + group key.
        let split = stats_series_sql(true);
        assert!(split.contains("e.kind AS kind"), "{split}");
        assert!(split.contains("GROUP BY bucket, e.kind"), "{split}");
    }

    #[test]
    fn event_stat_group_parses_known_dimensions() {
        assert_eq!(EventStatGroup::parse("kind"), Some(EventStatGroup::Kind));
        assert_eq!(
            EventStatGroup::parse("action"),
            Some(EventStatGroup::Action)
        );
        assert_eq!(EventStatGroup::parse("trap"), Some(EventStatGroup::Trap));
        assert_eq!(
            EventStatGroup::parse("source"),
            Some(EventStatGroup::Source)
        );
        // `time` is handled on a separate path; unknown values are rejected.
        assert_eq!(EventStatGroup::parse("time"), None);
        assert_eq!(EventStatGroup::parse("bogus"), None);
    }

    fn stored_rule(pattern: &str, severity: &str) -> StoredEventRule {
        StoredEventRule {
            id: Uuid::new_v4(),
            name: "test rule".into(),
            enabled: true,
            source_kind: None,
            source_id: None,
            node_id: None,
            match_kind: "substring".into(),
            pattern: pattern.into(),
            clear_pattern: None,
            severity: severity.into(),
            ttl_secs: 1800,
            min_count: 1,
            window_secs: 60,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn substring_and_regex_matchers() {
        let sub = compile_matcher("substring", "link down").unwrap();
        assert!(sub.matches("chassisd: link down on ge-0/0/1"));
        assert!(!sub.matches("link up"));

        let re = compile_matcher("regex", r"(?i)%LINEPROTO-\d-UPDOWN").unwrap();
        assert!(re.matches("%lineproto-5-updown: state change"));
        assert!(!re.matches("%SYS-5-CONFIG_I"));
    }

    #[test]
    fn invalid_patterns_are_rejected() {
        assert!(compile_matcher("regex", "(unclosed").is_err());
        assert!(compile_matcher("substring", "").is_err());
        assert!(compile_matcher("substring", &"a".repeat(513)).is_err());
        assert!(compile_matcher("glob", "x*").is_err());
        // Pathological expansion (100^4 states) is capped by the 1 MiB size limit.
        assert!(compile_matcher("regex", "((((a{100}){100}){100}){100})").is_err());
    }

    #[test]
    fn rule_scoping_applies_kind_source_and_node() {
        let node = Uuid::new_v4();
        let other_node = Uuid::new_v4();
        let source = Uuid::new_v4();
        let mut stored = stored_rule("x", "warning");
        stored.source_kind = Some("syslog".into());
        stored.node_id = Some(node);
        let rule = compile_rule(&stored).unwrap();

        assert!(rule.applies(EventKind::Syslog, None, node));
        assert!(!rule.applies(EventKind::Trap, None, node));
        assert!(!rule.applies(EventKind::Syslog, None, other_node));

        let mut stored = stored_rule("x", "warning");
        stored.source_id = Some(source);
        let rule = compile_rule(&stored).unwrap();
        assert!(rule.applies(EventKind::Webhook, Some(source), node));
        assert!(!rule.applies(EventKind::Webhook, Some(Uuid::new_v4()), node));
        assert!(!rule.applies(EventKind::Webhook, None, node));
    }

    #[test]
    fn disabled_and_broken_rules_do_not_compile() {
        let mut stored = stored_rule("x", "warning");
        stored.enabled = false;
        assert!(compile_rule(&stored).is_none());

        let mut stored = stored_rule("(bad", "warning");
        stored.match_kind = "regex".into();
        assert!(compile_rule(&stored).is_none());
    }

    #[test]
    fn min_count_window_gate() {
        let mut runtime = Runtime::new();
        let mut stored = stored_rule("x", "critical");
        stored.min_count = 3;
        stored.window_secs = 10;
        let rule = compile_rule(&stored).unwrap();
        let node = Uuid::new_v4();

        assert!(!runtime.gate_passes(&rule, node, 0));
        assert!(!runtime.gate_passes(&rule, node, 1_000));
        assert!(runtime.gate_passes(&rule, node, 2_000)); // 3rd inside 10s
                                                          // Outside the window the count restarts.
        assert!(!runtime.gate_passes(&rule, node, 60_000));
        // Another node's count is independent.
        assert!(!runtime.gate_passes(&rule, Uuid::new_v4(), 2_000));
    }

    #[test]
    fn burst_dedup_window() {
        let mut runtime = Runtime::new();
        assert!(!runtime.is_duplicate(42, 0));
        assert!(runtime.is_duplicate(42, 1_000)); // same key inside 5s
        assert!(!runtime.is_duplicate(43, 1_000)); // different key
        assert!(!runtime.is_duplicate(42, 10_000)); // window elapsed
    }

    #[test]
    fn action_precedence_orders_outcomes() {
        assert!(action_rank("fired") > action_rank("refreshed"));
        assert!(action_rank("refreshed") > action_rank("cleared"));
        assert!(action_rank("cleared") > action_rank("suppressed"));
        assert!(action_rank("suppressed") > action_rank("info"));
        assert!(action_rank("info") > action_rank("none"));
    }

    #[test]
    fn token_hashing_and_constant_time_compare() {
        let token = generate_token();
        assert_eq!(token.len(), 64);
        let hash = hash_token(&token);
        assert_eq!(hash.len(), 64);
        assert_ne!(hash, token);
        assert!(constant_time_eq(
            hash.as_bytes(),
            hash_token(&token).as_bytes()
        ));
        assert!(!constant_time_eq(
            hash.as_bytes(),
            hash_token("other").as_bytes()
        ));
        assert!(!constant_time_eq(b"short", b"longer-value"));
    }

    // ── plan() through a real engine core (no DB I/O reached: plan is sync/pure) ──

    fn engine_for_plan() -> EventEngine {
        // The repo/history are never touched by plan(); connect_lazy gives us handles
        // without a live database.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .expect("lazy pool");
        EventEngine::new(
            Arc::new(EventRepo::new(pool.clone())),
            Arc::new(AlertManager::new()),
            Arc::new(Notifier::from_env()),
            Arc::new(AlertHistoryStore::new(pool)),
            None,
            None,
        )
    }

    fn persist_record(action: &'static str) -> PersistRecord {
        PersistRecord {
            msg: syslog_msg("some event body"),
            node_id: Some(Uuid::new_v4()),
            source_id: None,
            matched_rule_id: (action != "none").then(Uuid::new_v4),
            action,
        }
    }

    fn lazy_repo() -> Arc<EventRepo> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .expect("lazy pool");
        Arc::new(EventRepo::new(pool))
    }

    #[test]
    fn alert_linked_classification() {
        for a in ["fired", "refreshed", "cleared", "suppressed"] {
            assert!(
                persist_record(a).is_alert_linked(),
                "{a} should be alert-linked"
            );
        }
        for a in ["info", "none"] {
            assert!(
                !persist_record(a).is_alert_linked(),
                "{a} should not be alert-linked"
            );
        }
    }

    #[tokio::test]
    async fn persist_writer_routes_non_alert_rows_to_log_store_only() {
        // With the log store enabled, non-alert-linked rows go to the log store and never touch
        // Postgres — so this exercises the writer end-to-end against a never-connected lazy pool.
        let fake = Arc::new(crate::logstore::InMemoryLogStore::default());
        let logs: Option<Arc<dyn LogStore>> = Some(fake.clone());
        let (tx, rx) = tokio::sync::mpsc::channel::<PersistRecord>(16);
        let token = CancellationToken::new();
        let handle = tokio::spawn(run_persist_writer(rx, lazy_repo(), logs, token));

        tx.send(persist_record("none")).await.unwrap();
        tx.send(persist_record("info")).await.unwrap();
        drop(tx); // close the channel → writer drains, flushes, returns
        handle.await.unwrap();

        assert_eq!(fake.len(), 2);
    }

    #[tokio::test]
    async fn persist_writer_final_flush_on_shutdown() {
        let fake = Arc::new(crate::logstore::InMemoryLogStore::default());
        let logs: Option<Arc<dyn LogStore>> = Some(fake.clone());
        let (tx, rx) = tokio::sync::mpsc::channel::<PersistRecord>(16);
        let token = CancellationToken::new();
        let handle = tokio::spawn(run_persist_writer(rx, lazy_repo(), logs, token.clone()));

        tx.send(persist_record("none")).await.unwrap();
        // Give the writer a moment to drain the one message, then cancel; the buffer is already
        // flushed, and the cancel arm's final flush is a no-op.
        token.cancel();
        handle.await.unwrap();
        assert_eq!(fake.len(), 1);
    }

    // ── S10: event-action writer (history + notify offloaded off the matcher) ──

    fn test_alert(node: Uuid, severity: Severity) -> Alert {
        let node_id = NodeId::from(node);
        Alert {
            node: node_id,
            check: check_id(node_id, "event:test"),
            severity,
            state: NodeState::Warning,
            at_unix_ms: 1_000,
            root_cause: None,
            flapping: false,
            metric: "event:test".into(),
            breach: None,
        }
    }

    fn lazy_history() -> Arc<AlertHistoryStore> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused")
            .expect("lazy pool");
        Arc::new(AlertHistoryStore::new(pool))
    }

    #[test]
    fn history_rows_map_fire_and_resolve_and_skip_suppress() {
        let node = Uuid::new_v4();
        let batch = vec![
            EventAction {
                action: NotifyAction::Fire(test_alert(node, Severity::Critical)),
                reason: "fire",
            },
            EventAction {
                action: NotifyAction::Resolve(test_alert(node, Severity::Critical)),
                reason: "clear",
            },
            EventAction {
                action: NotifyAction::Suppress(test_alert(node, Severity::Warning)),
                reason: "fire",
            },
        ];
        let rows = history_rows(&batch);
        // Fire → resolved=false, Resolve → resolved=true, Suppress → no row. Order preserved.
        assert_eq!(rows.len(), 2);
        assert!(!rows[0].1, "fire should record resolved=false");
        assert!(rows[1].1, "resolve should record resolved=true");
    }

    #[tokio::test]
    async fn action_writer_drains_and_returns_on_channel_close() {
        // Suppress actions record no history (no DB touched) and the env notifier has no channels,
        // so this exercises the writer's batch-drain + FIFO delivery + clean shutdown without a
        // live database or notifier. History mapping is covered purely above.
        let (tx, rx) = tokio::sync::mpsc::channel::<EventAction>(16);
        let token = CancellationToken::new();
        let handle = tokio::spawn(run_event_action_writer(
            rx,
            lazy_history(),
            Arc::new(Notifier::from_env()),
            token,
        ));
        for _ in 0..3 {
            tx.send(EventAction {
                action: NotifyAction::Suppress(test_alert(Uuid::new_v4(), Severity::Warning)),
                reason: "fire",
            })
            .await
            .unwrap();
        }
        drop(tx); // close the channel → writer drains, flushes, returns
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn action_writer_final_flush_on_shutdown() {
        let (tx, rx) = tokio::sync::mpsc::channel::<EventAction>(16);
        let token = CancellationToken::new();
        let handle = tokio::spawn(run_event_action_writer(
            rx,
            lazy_history(),
            Arc::new(Notifier::from_env()),
            token.clone(),
        ));
        tx.send(EventAction {
            action: NotifyAction::Suppress(test_alert(Uuid::new_v4(), Severity::Warning)),
            reason: "fire",
        })
        .await
        .unwrap();
        token.cancel();
        handle.await.unwrap();
    }

    fn syslog_msg(message: &str) -> EventMsg {
        EventMsg {
            event_id: Uuid::new_v4(),
            kind: EventKind::Syslog,
            at_unix_ms: 1_000,
            source_ip: Some("10.0.0.1".parse().unwrap()),
            pool: None,
            message: message.into(),
            facility: None,
            syslog_severity: None,
            hostname: None,
            app_name: None,
            trap_oid: None,
            varbinds: Vec::new(),
            truncated: false,
            raw: None,
            src_port: None,
        }
    }

    /// Mirrors what the poller publishes for an SNMP trap: `message` begins with the raw
    /// identity OID (`render_message`), and `trap_oid` carries that identity for name resolution.
    fn trap_msg(trap_oid: &str) -> EventMsg {
        EventMsg {
            event_id: Uuid::new_v4(),
            kind: EventKind::Trap,
            at_unix_ms: 1_000,
            source_ip: Some("10.0.0.1".parse().unwrap()),
            pool: None,
            message: format!("{trap_oid} 1.3.6.1.2.1.2.2.1.1.4=4;"),
            facility: None,
            syslog_severity: None,
            hostname: None,
            app_name: None,
            trap_oid: Some(trap_oid.into()),
            varbinds: vec![("1.3.6.1.2.1.2.2.1.1.4".into(), "4".into())],
            truncated: false,
            raw: None,
            src_port: None,
        }
    }

    fn set_rules(engine: &EventEngine, rules: Vec<CompiledRule>) {
        engine.snapshot.write().unwrap().rules = rules;
    }

    fn fires(p: &Planned) -> Vec<&Alert> {
        p.actions
            .iter()
            .filter_map(|(a, _)| match a {
                NotifyAction::Fire(alert) => Some(alert),
                NotifyAction::Resolve(_) | NotifyAction::Suppress(_) => None,
            })
            .collect()
    }

    fn resolves(p: &Planned) -> usize {
        p.actions
            .iter()
            .filter(|(a, _)| matches!(a, NotifyAction::Resolve(_)))
            .count()
    }

    #[tokio::test]
    async fn plan_fires_then_refreshes_then_clears() {
        let engine = engine_for_plan();
        let node = Uuid::new_v4();
        let mut stored = stored_rule("link down", "critical");
        stored.clear_pattern = Some("link up".into());
        let rule_id = stored.id;
        set_rules(&engine, vec![compile_rule(&stored).unwrap()]);

        // First match fires (and the manager now holds the active alert).
        let p = engine.plan(&syslog_msg("link down on ge-0/0/1"), None, node, 1_000);
        assert_eq!(p.row_action, "fired");
        assert_eq!(p.matched_rule, Some(rule_id));
        let f = fires(&p);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Critical);
        assert_eq!(f[0].state, NodeState::Critical);
        assert!(f[0].root_cause.is_none());
        assert_eq!(f[0].metric, "event:test rule");
        assert_eq!(engine.alerts.active_alerts().len(), 1);

        // Repeat match refreshes (extends TTL), no second fire.
        let p = engine.plan(&syslog_msg("link down on ge-0/0/1"), None, node, 2_000);
        assert_eq!(p.row_action, "refreshed");
        assert!(fires(&p).is_empty());

        // Clear pattern resolves in both active sets.
        let p = engine.plan(&syslog_msg("link up on ge-0/0/1"), None, node, 3_000);
        assert_eq!(p.row_action, "cleared");
        assert_eq!(resolves(&p), 1);
        assert!(engine.alerts.active_alerts().is_empty());

        // After the clear, a new match fires again.
        let p = engine.plan(&syslog_msg("link down on ge-0/0/1"), None, node, 4_000);
        assert_eq!(p.row_action, "fired");
        assert_eq!(fires(&p).len(), 1);
    }

    #[tokio::test]
    async fn plan_matches_trap_by_resolved_name() {
        let engine = engine_for_plan();
        let node = Uuid::new_v4();
        // A built-in-style rule matches the MIB *name*, not the raw OID (source-scoped to traps).
        let mut stored = stored_rule("linkDown", "warning");
        stored.source_kind = Some("trap".into());
        stored.clear_pattern = Some("linkUp".into());
        set_rules(&engine, vec![compile_rule(&stored).unwrap()]);

        // The wire message is the raw OID — "linkDown" is never in it — yet the rule fires,
        // because core enriches the match text with the resolved name (yagra_common::trap_oid_name).
        let down = trap_msg("1.3.6.1.6.3.1.1.5.3");
        assert!(!down.message.contains("linkDown"));
        let p = engine.plan(&down, None, node, 1_000);
        assert_eq!(p.row_action, "fired");
        assert_eq!(fires(&p).len(), 1);
        assert_eq!(engine.alerts.active_alerts().len(), 1);

        // The linkUp trap (a different OID) resolves via the clear pattern's resolved name.
        let p = engine.plan(&trap_msg("1.3.6.1.6.3.1.1.5.4"), None, node, 2_000);
        assert_eq!(p.row_action, "cleared");
        assert_eq!(resolves(&p), 1);
        assert!(engine.alerts.active_alerts().is_empty());
    }

    #[tokio::test]
    async fn plan_unknown_trap_oid_still_matches_by_raw_oid() {
        let engine = engine_for_plan();
        let node = Uuid::new_v4();
        // A vendor trap outside the curated name set: a rule can still match its numeric OID,
        // which is always present in the message (enrichment is additive, never a replacement).
        let stored = stored_rule("1.3.6.1.4.1.9.9.43.2.0.1", "warning");
        set_rules(&engine, vec![compile_rule(&stored).unwrap()]);
        let p = engine.plan(&trap_msg("1.3.6.1.4.1.9.9.43.2.0.1"), None, node, 1_000);
        assert_eq!(p.row_action, "fired");
    }

    #[tokio::test]
    async fn plan_clear_wins_over_fire_for_ambiguous_message() {
        let engine = engine_for_plan();
        let node = Uuid::new_v4();
        let mut stored = stored_rule("link", "warning");
        stored.clear_pattern = Some("link recovered".into());
        set_rules(&engine, vec![compile_rule(&stored).unwrap()]);

        // Fire first so there's something active.
        let p = engine.plan(&syslog_msg("link failed"), None, node, 1_000);
        assert_eq!(p.row_action, "fired");
        // "link recovered" matches BOTH the fire pattern ("link") and the clear pattern —
        // clear must win or the alert would flap.
        let p = engine.plan(&syslog_msg("link recovered"), None, node, 2_000);
        assert_eq!(p.row_action, "cleared");
        assert!(fires(&p).is_empty());
    }

    #[tokio::test]
    async fn plan_info_severity_records_without_alert() {
        let engine = engine_for_plan();
        let node = Uuid::new_v4();
        set_rules(
            &engine,
            vec![compile_rule(&stored_rule("config changed", "info")).unwrap()],
        );
        let p = engine.plan(&syslog_msg("config changed by admin"), None, node, 1_000);
        assert_eq!(p.row_action, "info");
        assert!(p.actions.is_empty());
        assert!(p.matched_rule.is_some());
    }

    #[tokio::test]
    async fn plan_gates_below_min_count() {
        let engine = engine_for_plan();
        let node = Uuid::new_v4();
        let mut stored = stored_rule("auth failure", "warning");
        stored.min_count = 3;
        stored.window_secs = 60;
        set_rules(&engine, vec![compile_rule(&stored).unwrap()]);

        let p = engine.plan(&syslog_msg("auth failure for admin"), None, node, 1_000);
        assert_eq!(p.row_action, "info"); // matched, gated
        let p = engine.plan(&syslog_msg("auth failure for admin"), None, node, 2_000);
        assert_eq!(p.row_action, "info");
        let p = engine.plan(&syslog_msg("auth failure for admin"), None, node, 3_000);
        assert_eq!(p.row_action, "fired"); // 3rd match inside the window
        assert_eq!(fires(&p).len(), 1);
    }

    #[tokio::test]
    async fn plan_suppresses_during_maintenance() {
        let engine = engine_for_plan();
        let node = Uuid::new_v4();
        set_rules(
            &engine,
            vec![compile_rule(&stored_rule("link down", "critical")).unwrap()],
        );
        // Put the node into an active maintenance window via the alert config.
        let mut maint = std::collections::BTreeSet::new();
        maint.insert(NodeId::from(node));
        engine.alerts.set_config(
            crate::alerts::AlertConfig::new(Vec::new(), HashMap::new()).with_maintenance(maint),
        );

        let p = engine.plan(&syslog_msg("link down on ge-0/0/1"), None, node, 1_000);
        assert_eq!(p.row_action, "suppressed");
        assert!(p.actions.is_empty());
    }

    #[tokio::test]
    async fn plan_evaluates_all_rules_not_first_match() {
        let engine = engine_for_plan();
        let node = Uuid::new_v4();
        let r1 = compile_rule(&stored_rule("link down", "warning")).unwrap();
        let r2 = compile_rule(&stored_rule("ge-0/0/1", "critical")).unwrap();
        set_rules(&engine, vec![r1, r2]);

        let p = engine.plan(&syslog_msg("link down on ge-0/0/1"), None, node, 1_000);
        // Both rules matched and both fired independently.
        assert_eq!(fires(&p).len(), 2);
        assert_eq!(p.row_action, "fired");
    }

    #[tokio::test]
    async fn re_fire_after_sweep_is_not_permanently_suppressed() {
        // Regression for the sweep/plan race: because plan() and sweep() mutate the
        // runtime and manager active sets together under the runtime lock, an event that
        // arrives right after a TTL sweep re-fires cleanly rather than getting stuck in a
        // "refreshed" loop with the manager alert already closed.
        let engine = engine_for_plan();
        let node = Uuid::new_v4();
        let mut stored = stored_rule("link down", "critical");
        stored.ttl_secs = 60;
        set_rules(&engine, vec![compile_rule(&stored).unwrap()]);

        // Fire, then expire it via the sweeper (both sets cleared together).
        let p = engine.plan(&syslog_msg("link down"), None, node, 0);
        assert_eq!(p.row_action, "fired");
        assert_eq!(engine.alerts.active_alerts().len(), 1);
        engine.sweep(1_000_000).await; // well past the 60s TTL
        assert!(engine.alerts.active_alerts().is_empty());

        // A new matching event must fire again (not silently refresh a closed alert).
        let p = engine.plan(&syslog_msg("link down"), None, node, 1_001_000);
        assert_eq!(p.row_action, "fired");
        assert_eq!(fires(&p).len(), 1);
        assert_eq!(engine.alerts.active_alerts().len(), 1);
    }

    #[tokio::test]
    async fn close_alert_resolves_both_active_sets() {
        let engine = engine_for_plan();
        let node = Uuid::new_v4();
        let stored = stored_rule("disk full", "warning");
        set_rules(&engine, vec![compile_rule(&stored).unwrap()]);

        let p = engine.plan(&syslog_msg("disk full on /var"), None, node, 0);
        let check = match &p.actions[0].0 {
            NotifyAction::Fire(a) => a.check,
            NotifyAction::Resolve(_) | NotifyAction::Suppress(_) => panic!("expected a fire"),
        };
        assert!(engine.close_alert(check).await);
        assert!(engine.alerts.active_alerts().is_empty());
        // A second close is a no-op (nothing active).
        assert!(!engine.close_alert(check).await);
    }

    #[test]
    fn raise_and_resolve_event_alert_in_manager() {
        let manager = AlertManager::new();
        let node = NodeId::from(Uuid::new_v4());
        let check = check_id(node, "event:test");
        let alert = Alert {
            node,
            check,
            severity: Severity::Warning,
            state: NodeState::Warning,
            at_unix_ms: 1,
            root_cause: None,
            flapping: false,
            metric: "event:test".into(),
            breach: None,
        };

        // First raise fires; a same-severity duplicate is deduped at the manager.
        assert!(manager.raise_event_alert(alert.clone()).is_some());
        assert!(manager.raise_event_alert(alert.clone()).is_none());
        assert_eq!(manager.active_alerts().len(), 1);
        // The node display state rolls up the event alert.
        assert_eq!(manager.node_state(node), Some(NodeState::Warning));

        // A severity escalation replaces and re-fires.
        let mut worse = alert.clone();
        worse.severity = Severity::Critical;
        worse.state = NodeState::Critical;
        assert!(manager.raise_event_alert(worse).is_some());

        // Resolve returns the previously-active alert; second resolve is a no-op.
        let resolved = manager.resolve_event_alert(check);
        assert!(
            matches!(resolved, Some(NotifyAction::Resolve(a)) if a.severity == Severity::Critical)
        );
        assert!(manager.resolve_event_alert(check).is_none());
        assert!(manager.active_alerts().is_empty());
    }
}
