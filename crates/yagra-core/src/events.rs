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
use yagra_common::{CheckId, NodeId, NodeState, Severity};

use crate::alerts::{check_id, AlertManager, Notifier, NotifyAction};
use crate::history::AlertHistoryStore;
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
    pub varbinds: Option<serde_json::Value>,
    pub message: String,
    pub matched_rule_id: Option<Uuid>,
    pub action: String,
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
#[derive(Debug, Default)]
pub struct EventFilter {
    pub before: Option<DateTime<Utc>>,
    pub kind: Option<String>,
    pub node_id: Option<Uuid>,
    pub matched: Option<bool>,
    /// Case-insensitive substring matched against source (node name / IP) or message.
    pub search: Option<String>,
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

    /// Persist one received event (best-effort at the call site — a DB hiccup must not
    /// stop alerting).
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_event(
        &self,
        msg: &EventMsg,
        node_id: Option<Uuid>,
        source_id: Option<Uuid>,
        matched_rule_id: Option<Uuid>,
        action: &str,
    ) -> anyhow::Result<()> {
        let varbinds = (!msg.varbinds.is_empty())
            .then(|| serde_json::to_value(&msg.varbinds).unwrap_or(serde_json::Value::Null));
        sqlx::query(
            "INSERT INTO events (id, kind, at_unix_ms, source_ip, node_id, source_id, pool, \
             facility, syslog_severity, hostname, app_name, trap_oid, varbinds, message, \
             matched_rule_id, action) \
             VALUES ($1, $2, $3, $4::inet, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
        )
        .bind(msg.event_id)
        .bind(msg.kind.as_str())
        .bind(msg.at_unix_ms)
        .bind(msg.source_ip.map(|ip| ip.to_string()))
        .bind(node_id)
        .bind(source_id)
        .bind(msg.pool.as_deref())
        .bind(msg.facility.map(i16::from))
        .bind(msg.syslog_severity.map(i16::from))
        .bind(msg.hostname.as_deref())
        .bind(msg.app_name.as_deref())
        .bind(msg.trap_oid.as_deref())
        .bind(varbinds)
        .bind(&msg.message)
        .bind(matched_rule_id)
        .bind(action)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Keyset-paged event list, newest first (mirrors alert-history paging).
    pub async fn list_events(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventRow>> {
        let rows = sqlx::query(
            "SELECT e.id, e.kind, e.at_unix_ms, e.recorded_at, host(e.source_ip) AS source_ip, \
                    e.node_id, e.source_id, e.pool, e.facility, e.syslog_severity, e.hostname, \
                    e.app_name, e.trap_oid, e.varbinds, e.message, e.matched_rule_id, e.action \
             FROM events e LEFT JOIN nodes n ON n.id = e.node_id \
             WHERE ($1::timestamptz IS NULL OR e.recorded_at < $1) \
               AND ($2::text IS NULL OR e.kind = $2) \
               AND ($3::uuid IS NULL OR e.node_id = $3) \
               AND ($4::boolean IS NULL OR (e.matched_rule_id IS NOT NULL) = $4) \
               AND ($5::text IS NULL \
                    OR e.message ILIKE '%' || $5 || '%' \
                    OR host(e.source_ip) ILIKE '%' || $5 || '%' \
                    OR n.name ILIKE '%' || $5 || '%') \
             ORDER BY e.recorded_at DESC LIMIT $6",
        )
        .bind(filter.before)
        .bind(filter.kind.as_deref())
        .bind(filter.node_id)
        .bind(filter.matched)
        .bind(filter.search.as_deref())
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
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
                    trap_oid: row.try_get("trap_oid")?,
                    varbinds: row.try_get("varbinds")?,
                    message: row.try_get("message")?,
                    matched_rule_id: row.try_get("matched_rule_id")?,
                    action: row.try_get("action")?,
                })
            })
            .collect()
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
}

impl EventEngine {
    #[must_use]
    pub fn new(
        repo: Arc<EventRepo>,
        alerts: Arc<AlertManager>,
        notifier: Arc<Notifier>,
        history: Arc<AlertHistoryStore>,
    ) -> Self {
        Self {
            repo,
            alerts,
            notifier,
            history,
            snapshot: RwLock::new(Snapshot::default()),
            runtime: Mutex::new(Runtime::new()),
            ingest_rate: Mutex::new(HashMap::new()),
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

        // Persist the event row (best-effort — a DB hiccup must not stop alerting).
        if let Err(e) = self
            .repo
            .insert_event(
                &msg,
                node_id,
                source.as_ref().map(|s| s.source_id),
                planned.matched_rule,
                if planned.row_action.is_empty() {
                    "none"
                } else {
                    planned.row_action
                },
            )
            .await
        {
            tracing::warn!(error = %e, kind = msg.kind.as_str(), "failed to persist event");
        }

        for (action, reason) in planned.actions {
            self.run_action(action, reason).await;
        }
        self.update_active_gauge();
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

        for rule in snap
            .rules
            .iter()
            .filter(|r| r.applies(msg.kind, source, node))
        {
            let fire_hit = rule.matcher.matches(&msg.message);
            let clear_hit = rule
                .clear_matcher
                .as_ref()
                .is_some_and(|m| m.matches(&msg.message));
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
        }
        self.notifier.handle(action).await;
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
            self.run_action(action, reason).await;
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
                self.run_action(action, "manual").await;
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
pub async fn consume_events<S>(mut events: S, engine: Arc<EventEngine>)
where
    S: Stream<Item = EventMsg> + Unpin,
{
    while let Some(msg) = events.next().await {
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

#[cfg(test)]
mod tests {
    use super::*;

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
        )
    }

    fn syslog_msg(message: &str) -> EventMsg {
        EventMsg {
            schema_version: 1,
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
                NotifyAction::Resolve(_) => None,
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
            NotifyAction::Resolve(_) => panic!("expected a fire"),
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
