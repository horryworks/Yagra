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
//!
//! ## Where a thing goes (ADR-095)
//!
//! One file per program, and **this file holds what more than one of them needs**. That is not a
//! stylistic preference: a child module sees its parent's private items, so vocabulary placed here
//! costs no `pub(super)` at all, while the same vocabulary in a sibling costs one per item —
//! measured at 2 for `repo/` (ADR-094) against 67 for `analysis/` (ADR-089).
//!
//! | file | the program |
//! |---|---|
//! | `mod.rs` | the vocabulary — what an event is, in this process and on the wire |
//! | [`sql`] | the event query: the predicate **both backends** implement, its binder, and the statements built on it |
//! | [`repo`] | PostgreSQL. **The only file here that may name a table**, with [`sql`] — `guards.rs` enforces it |
//! | [`rules`] | a stored rule becomes a matcher |
//! | [`engine`] | the live matcher: dedup, correlation, planning, dispatch |
//! | [`ingest`] | the four background tasks — the bus consumer, the TTL sweeper, and the two batch writers |
//!
//! ⚠️ Two items sit where their **use** puts them rather than where their name suggests, and both
//! were found by measuring rather than by reading: `parse_severity` lived among the rule compiler
//! but is called only by `EventRepo::list_rules`, and it is the twin of `event_kind_from_stored` —
//! both turn a stored string into an enum, and they were 1,300 lines apart. They are together here.

mod engine;
#[cfg(test)]
mod guards;
mod ingest;
mod repo;
mod rules;
mod sql;
#[cfg(test)]
mod testkit;

// Everything a caller outside this module could name before the split, still named the same way.
//
// ⚠️ `Matcher`, `EVENT_FILTER_WHERE` and `EVENT_FILTER_BINDS` are flagged unused in a non-test
// build and are re-exported anyway. `Matcher` is the return type of two `pub fn`s here, and the
// two constants are read from `logstore.rs`'s tests — the mirror they exist for. Dropping any of
// them would narrow the surface `crate::events` offered before the split, which is a change to
// what callers can name, disguised as a warning fix. ADR-094 met the same thing and answered it
// the same way; `cargo fix` proposes the removal on every run, so the `allow` is what refuses it.
pub use engine::{EventEngine, SourceBinding};
pub use ingest::{consume_events, run_event_action_writer, run_persist_writer, run_ttl_sweeper};
pub use repo::EventRepo;
#[allow(unused_imports)]
pub use rules::{compile_matcher, compile_regex, CompiledRule, Matcher};
pub use sql::{EventSearchSemantics, TextCond};
// The event query, read from the other side of the mirror: `logstore.rs` implements the same
// predicate in LogsQL and asserts against these rather than restating them.
#[allow(unused_imports)]
pub(crate) use sql::{
    agg_unmatched_signatures_sql, AUTH_FAILURE_PHRASES, AUTH_FAILURE_TRAP_OID, EVENT_FILTER_BINDS,
    EVENT_FILTER_WHERE, SIGNATURE_TIERS,
};

use std::hash::Hash;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use yagra_bus::{EventKind, EventMsg};
use yagra_common::Severity;

use crate::alerts::NotifyAction;

/// Identical-event burst dedup: a repeat of the same (kind, origin, message) within this
/// window is dropped before the DB write. Transport-level dedup, distinct from alert dedup.
const DEDUP_WINDOW_MS: i64 = 5_000;
/// Bounded size of the burst-dedup window.
const DEDUP_CAP: usize = 4096;
/// The prefix every passive-event alert carries in its `metric`, and the marker that says
/// **this alert has no time series behind it**.
///
/// An event alert is raised by [`engine`] from a syslog line or a trap, not from a poll result, so
/// nothing ever writes `event:<rule name>` to the TSDB. Anything that reasons about an alert by
/// asking the store whether its metric is still arriving therefore has to recognise and skip these
/// — otherwise every one of them reads as "the data stopped" on the very first look.
///
/// 🚨 **Closing one from outside this module is worse than a wrong answer.** `engine` mutates the
/// manager's alert and its own `runtime.active` under one lock precisely because the two must not
/// diverge: a manager-side resolve that leaves the runtime entry behind makes the rule's re-fire
/// **permanently** suppressed. The freshness sweep (ADR-097 Increment 6) excludes them by this
/// constant, with a test.
pub(crate) const EVENT_METRIC_PREFIX: &str = "event:";

/// TTL sweeper cadence.
const SWEEP_INTERVAL: Duration = Duration::from_secs(15);
// Retention windows are no longer declared here: they are operator-configurable and live in
// `crate::retention` (ADR-040). `prune_old` takes them as arguments so this store has no opinion.
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
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct EventSourceView {
    pub id: Uuid,
    pub name: String,
    pub kind: EventKind,
    pub enabled: bool,
    pub node_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// A stored event rule (API shape; the engine compiles enabled ones).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct StoredEventRule {
    pub id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub source_kind: Option<String>,
    pub source_id: Option<Uuid>,
    pub node_id: Option<Uuid>,
    pub match_kind: EventMatchKind,
    pub pattern: String,
    pub clear_pattern: Option<String>,
    pub severity: Severity,
    pub ttl_secs: i32,
    pub min_count: i32,
    pub window_secs: i32,
    pub created_at: DateTime<Utc>,
}

/// Rule create/update parameters (validated at the API edge).
#[derive(Debug)]
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

/// What the pipeline did with an event. When several rules match one event, the row records the
/// strongest outcome.
//  Variants are declared least → most consequential so the derived `Ord` is that ranking. It *was*
//  a hand-written `action_rank(&str) -> u8` with a `_ => 0` arm — a second copy of this list, where
//  a new variant would have silently ranked below "nothing happened". Below the doc line because a
//  schema's doc text is published verbatim to API clients.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EventAction {
    /// Matched no rule (or none that fired) — stored for search, not linked to an alert.
    #[default]
    None,
    /// Matched an informational rule, or a rule whose count/window gate has not yet passed.
    Info,
    /// Would have fired, but the node is in a maintenance window.
    Suppressed,
    /// Resolved an alert this rule had raised.
    Cleared,
    /// Re-armed an alert that was already active.
    Refreshed,
    /// Raised an alert.
    Fired,
}

/// How a rule's pattern is matched against the event text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventMatchKind {
    /// Case-insensitive substring.
    Substring,
    /// Regular expression.
    Regex,
    /// A match kind this build does not know — a newer core wrote the row. Deliberately not
    /// treated as `Substring`: that would take a rule which is currently *inert* and start
    /// matching it literally, which is a behaviour change with alerting consequences.
    Unknown,
}

impl EventAction {
    /// Every action, least → most consequential.
    pub const ALL: [EventAction; 6] = [
        Self::None,
        Self::Info,
        Self::Suppressed,
        Self::Cleared,
        Self::Refreshed,
        Self::Fired,
    ];

    /// Stable token — the `events.action` column value and the JSON tag.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Info => "info",
            Self::Suppressed => "suppressed",
            Self::Cleared => "cleared",
            Self::Refreshed => "refreshed",
            Self::Fired => "fired",
        }
    }

    /// Parse a stored token, degrading to `None` — which is what an unreadable outcome honestly is
    /// from this build's point of view, and is already what the log-store path returns when the
    /// field is absent.
    #[must_use]
    pub fn from_stored(s: &str) -> Self {
        Self::from_token(s).unwrap_or(Self::None)
    }

    /// Parse a token strictly, for **request** input.
    ///
    /// Separate from [`from_stored`](Self::from_stored) because the two want opposite failure
    /// modes: a row this build cannot read is honestly `none`, but a *filter* that degrades a typo
    /// to `none` answers a different question than the one asked and looks like a correct answer.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// Whether this outcome ties the row to an alert, which is what makes it survive in PostgreSQL
    /// (the log store keeps the whole firehose; PostgreSQL keeps the alert-linked rows — ADR-024).
    #[must_use]
    pub const fn is_alert_linked(self) -> bool {
        match self {
            Self::Fired | Self::Refreshed | Self::Cleared | Self::Suppressed => true,
            Self::None | Self::Info => false,
        }
    }
}

/// Read a stored `kind`. Both the `events` and `event_sources` columns carry an exact CHECK, so
/// under expand-contract a widened CHECK and a new variant ship together and the fallback is
/// unreachable — the `warn!` is there so that if it ever *is* reached, it says so rather than
/// quietly filing traps under syslog.
fn event_kind_from_stored(s: &str) -> EventKind {
    EventKind::from_token(s).unwrap_or_else(|| {
        tracing::warn!(token = %s, "unrecognised event kind in the database; reading it as syslog");
        EventKind::Syslog
    })
}

impl EventMatchKind {
    pub const ALL: [EventMatchKind; 3] = [Self::Substring, Self::Regex, Self::Unknown];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Substring => "substring",
            Self::Regex => "regex",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub fn from_stored(s: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|v| v.as_str() == s)
            .unwrap_or(Self::Unknown)
    }
}

/// One received event, as served by `GET /api/v1/events`.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct EventRow {
    pub id: Uuid,
    pub kind: EventKind,
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
    pub action: EventAction,
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
    /// The clustering key: trap OID, else the device's own event code, else syslog app-name — see
    /// [`SIGNATURE_TIERS`].
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
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
    pub action: EventAction,
    /// The device's own event code, lifted out of the message text at ingest (see
    /// [`signature_of`]). Carried here rather than on [`EventMsg`] so both writers derive it from
    /// one value — PostgreSQL and the log store cannot disagree about a field neither computes.
    pub signature: Option<String>,
}

/// Extract the event's vendor signature, counting which pattern answered.
///
/// **Why core and not the poller.** The pattern list in `yagra-ingest` grows with every vendor
/// Yagra meets. Extracting at the edge would make each new pattern wait for a poller rollout across
/// every remote site before those events cluster; doing it here, a core upgrade is enough. It also
/// lands on the single confluence of all three sources (syslog and traps from the poller's
/// listeners, webhooks from the API edge), and adds no [`EventMsg`] field — so this change has no
/// bus-compatibility surface at all, and an N-1 poller's events get signatures immediately
/// (ADR-017).
///
/// Runs for every kind, traps included: precedence between the signature tiers lives in the read
/// query ([`SIGNATURE_TIERS`]), so this column keeps exactly one meaning and a later change of
/// precedence is one edit rather than a rewrite of stored rows. A rendered trap message leads with
/// a dotted OID and matches no pattern — pinned in `yagra-ingest`'s fixture table.
fn signature_of(msg: &EventMsg) -> Option<String> {
    let found = yagra_ingest::extract_signature(&msg.message);
    metrics::counter!(
        "yagra_event_signature_total",
        "pattern" => found.map_or("none", |s| s.pattern.as_str()),
    )
    .increment(1);
    found.map(|s| s.text.to_owned())
}

impl PersistRecord {
    /// Whether this row participated in the alert lifecycle. When the log store is enabled these
    /// are the only rows still written to PostgreSQL (ADR-024 Contract) — the rest live in the log
    /// store only.
    #[must_use]
    fn is_alert_linked(&self) -> bool {
        self.action.is_alert_linked()
    }

    /// This row's clustering key under [`SIGNATURE_TIERS`] precedence — the in-process equivalent
    /// of the PostgreSQL `COALESCE` and the LogsQL tier chain.
    ///
    /// `#[cfg(test)]` because its only caller is the in-memory [`crate::logstore::LogStore`], which
    /// is itself test-only. That is exactly why it exists: the fake every analysis test runs against
    /// must not be able to answer differently from the two real stores, and a third hand-written
    /// precedence chain inside the fake is how it would.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn signature_key(&self) -> Option<&str> {
        SIGNATURE_TIER_ACCESSORS
            .iter()
            .find_map(|(_, get)| get(self))
    }
}

/// How to read each [`SIGNATURE_TIERS`] entry off a record, in the same order.
///
/// The names are carried alongside the accessors so `every_signature_tier_has_an_accessor` can pin
/// the two lists to each other: a tier added to `SIGNATURE_TIERS` and not here would leave the
/// in-memory store silently one tier behind the stores it stands in for.
#[cfg(test)]
type TierAccessor = (&'static str, fn(&PersistRecord) -> Option<&str>);
#[cfg(test)]
const SIGNATURE_TIER_ACCESSORS: [TierAccessor; 3] = [
    ("trap_oid", |r| r.msg.trap_oid.as_deref()),
    ("signature", |r| r.signature.as_deref()),
    ("app_name", |r| r.msg.app_name.as_deref()),
];

/// One planned alert side effect queued for the async action writer (S10): a fire/resolve/suppress
/// to record in history and forward to the notifier, off the matcher's hot path. `reason` labels a
/// resolve (`"clear"`/`"ttl"`/`"manual"`) for the resolved-total counter.
pub struct QueuedAction {
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
/// See [`TextCond`] for the per-column conditions, and note that they are ANDed with `search`
/// rather than replacing it: the WebUI stopped sending `search` when the filter row shipped, but
/// the parameter is unchanged for MCP and for any client written against it.
///
/// **A plain `search` term is the one place the backends are permitted to differ**, and the
/// difference is now one axis rather than two. Here it is a case-insensitive substring
/// (`ILIKE '%term%'`); on VictoriaLogs it is a case-insensitive whole-token phrase (`i("term")`),
/// so a mid-word `rror` finds fewer rows on a log-store deployment — but `SrcIp` and `srcip` now
/// find the same ones on both. Case shipped once the Events page's default range was bounded to
/// 24h: measured on real syslog it is ~1.1× over a bounded window and ~10× unbounded. Substring is
/// still declined at ~300× (5.6s per 24h, 30.1s unbounded = VictoriaLogs' query ceiling) — an
/// inverted word index cannot serve a leading substring without a full scan. The operator's escape
/// hatch for that axis is `regex`, which reaches inside tokens on either backend.
///
/// The per-column conditions added by ADR-053 (`message`, `source`) inherit that same permitted
/// axis and add negation, which was measured before it shipped: on 6.7M real events `NOT` costs
/// what the form it negates costs, in both the phrase and the regex mode, so there was no reason to
/// restrict it to one of them.
#[derive(Debug, Default)]
pub struct EventFilter {
    /// Keyset pagination cursor (exclusive upper bound, event time). Distinct from `until` (a
    /// user-facing range end); when both are set the effective upper bound is their min.
    pub before: Option<DateTime<Utc>>,
    /// User-facing time-range lower bound (inclusive, event time), or `None` for unbounded.
    pub since: Option<DateTime<Utc>>,
    /// User-facing time-range upper bound (inclusive, event time), or `None` for unbounded.
    pub until: Option<DateTime<Utc>>,
    /// Event kinds to include. **Empty means every kind**, not "no kinds" — the three multi-value
    /// dimensions are `Vec` rather than `Option<Vec>` precisely so there is no second spelling of
    /// "unfiltered" to get wrong. `visible_node_ids` below is the one that stays `Option`, because
    /// there an empty set genuinely means *nothing is visible*.
    pub kinds: Vec<String>,
    pub node_id: Option<Uuid>,
    pub matched: Option<bool>,
    /// Rule outcomes to include ([`EventAction`] tokens); empty means every outcome.
    pub actions: Vec<String>,
    /// Syslog severities (0–7) to include; empty means every severity. A non-syslog event has no
    /// severity and is therefore excluded whenever this is non-empty, on both backends.
    pub severities: Vec<i16>,
    /// Per-column condition on the message text (Excel-style filter row, ADR-053). Independent of
    /// `search`, and ANDed with it: `search` is the whole-row term the API has always taken.
    pub message: Option<TextCond>,
    /// Per-column condition on the event's source — its IP **or** the name of the node it is
    /// attributed to, which is what the Source column displays.
    pub source: Option<TextCond>,
    /// Case-insensitive substring matched against source (node name / IP) or message. When
    /// `regex` is set, `search` is instead a regular expression matched against the message only.
    pub search: Option<String>,
    /// Interpret `search` as a regular expression (message-only) rather than a substring.
    pub regex: bool,
    /// RBAC group scope, already resolved to node ids (ADR-014): the result is **restricted** to
    /// events from these nodes. `None` = unrestricted.
    ///
    /// ⚠️ **Do not confuse this with `name_node_ids`**, which the two search builders also take.
    /// They are opposites. `name_node_ids` is *additive* — it ORs "…or the event came from a node
    /// whose name matches your search term" into the free-text clause, widening the result. This is
    /// *subtractive* and ANDs over everything. Reusing one as the other is the "same fact, two
    /// meanings" bug this codebase has paid for before, and here the failure direction is that a
    /// restriction quietly becomes a widening.
    ///
    /// `Some(vec![])` means "no visible nodes" and must match nothing — never everything.
    ///
    /// An event with **no** `node_id` is excluded whenever this is `Some`, in both backends
    /// (SQL: `NULL = ANY(…)` is NULL; LogsQL: a missing field does not match `in(…)`). That is
    /// deliberate and matches the row-level rule in `api/eventlog.rs::search`: an unattributed
    /// syslog message is exactly where the body may name a device the caller cannot otherwise see.
    pub visible_node_ids: Option<Vec<Uuid>>,
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

/// The stored `severity` of a rule. The column has a CHECK, so an unrecognised value means the row
/// was written by a newer build; the rule still compiles, at the least alarming level.
fn parse_severity(s: &str) -> Severity {
    Severity::from_token(s).unwrap_or(Severity::Info)
}

#[cfg(test)]
mod tests {
    use super::testkit::persist_record;
    use super::*;

    /// The WebUI branches on these two strings, and it hardcodes them.
    ///
    /// `web/src/components/EventLog/eventFilterSpec.ts` declares `'prefix' | 'substring'` — the one
    /// shape ADR-035's generation does not reach, because a client that *narrows* a generated union
    /// still has to spell the members. Renaming a variant here would leave that comparison silently
    /// false, so the mode label and the empty state would both fall back to their unknown-core
    /// wording on a core that knows perfectly well.
    #[test]
    fn the_search_semantics_spelling_is_what_the_webui_expects() {
        assert_eq!(
            serde_json::to_string(&EventSearchSemantics::Prefix).expect("serializes"),
            "\"prefix\""
        );
        assert_eq!(
            serde_json::to_string(&EventSearchSemantics::Substring).expect("serializes"),
            "\"substring\""
        );
    }

    /// A strict token parse for request input, distinct from the lenient stored-row one.
    #[test]
    fn an_action_token_parses_strictly_for_requests_and_leniently_for_rows() {
        for a in EventAction::ALL {
            assert_eq!(EventAction::from_token(a.as_str()), Some(a));
        }
        // A typo in a *filter* must be refused; the same string read out of a row is honestly
        // `none`, because that is what this build can say about an outcome it cannot name.
        assert_eq!(EventAction::from_token("fried"), None);
        assert_eq!(EventAction::from_stored("fried"), EventAction::None);
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

    #[test]
    fn action_precedence_orders_outcomes() {
        // The ordering is now the enum's declaration order (derived `Ord`), which is what `bump`
        // compares. ALL is declared least → most consequential, so it must be sorted.
        assert!(EventAction::ALL.is_sorted());
        assert!(EventAction::Fired > EventAction::Refreshed);
        assert!(EventAction::Refreshed > EventAction::Cleared);
        assert!(EventAction::Cleared > EventAction::Suppressed);
        assert!(EventAction::Suppressed > EventAction::Info);
        assert!(EventAction::Info > EventAction::None);
    }

    #[test]
    fn every_action_round_trips_through_its_token() {
        // `as_str` writes the `events.action` column and serde's `rename_all` writes the JSON tag;
        // nothing makes the two agree, and the log-store path reads back what it wrote.
        for a in EventAction::ALL {
            assert_eq!(EventAction::from_stored(a.as_str()), a);
            assert_eq!(
                serde_json::to_string(&a).unwrap(),
                format!("\"{}\"", a.as_str())
            );
        }
        // A token this build does not know reads as "nothing happened" — the only honest answer,
        // and it keeps the row out of the alert-linked set rather than inventing a link.
        assert_eq!(EventAction::from_stored("escalated"), EventAction::None);
        assert!(!EventAction::from_stored("escalated").is_alert_linked());
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

    #[test]
    fn alert_linked_classification() {
        use EventAction::{Cleared, Fired, Info, None, Refreshed, Suppressed};
        for a in [Fired, Refreshed, Cleared, Suppressed] {
            assert!(
                persist_record(a).is_alert_linked(),
                "{a:?} should be alert-linked"
            );
        }
        for a in [Info, None] {
            assert!(
                !persist_record(a).is_alert_linked(),
                "{a:?} should not be alert-linked"
            );
        }
        // Every variant is classified: the partition above must cover ALL, so a new outcome cannot
        // be added without deciding whether its rows survive in PostgreSQL (ADR-024).
        assert_eq!(EventAction::ALL.len(), 6);
    }
}
