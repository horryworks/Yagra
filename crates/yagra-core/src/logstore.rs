// SPDX-License-Identifier: AGPL-3.0-only
//! The event-log store seam: where passive events are written for search/display (ADR-024).
//!
//! [`LogStore`] abstracts the event log so the persist writer and the events API don't care
//! whether they talk to VictoriaLogs ([`VlStore`], live) or an in-memory fake
//! ([`InMemoryLogStore`], tests). It is the **4th data class** added to ADR-004's store split —
//! a *best-effort observational tier* (between rebuildable Redis and must-preserve PG/VM): losing
//! it degrades event search/forensics only, never alert integrity (which lives in PostgreSQL).
//!
//! Ingest uses VictoriaLogs' JSON-stream endpoint with `{kind, pool}` as the only stream fields
//! (ADR-011/023 cardinality discipline — `node_id`/`source_ip` are regular indexed fields, never
//! stream keys); `message` is the full-text body. Search compiles an [`EventFilter`] into LogsQL.
//! Human node names never enter the store — the API resolves a name search to `node_id`s in
//! PostgreSQL and passes them here (query-time join, ADR-011).

#[cfg(test)]
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use uuid::Uuid;
use yagra_common::trap_oid_name;

use crate::events::{
    EventAction, EventAuthSource, EventBucketCount, EventFilter, EventRow, EventSeverityCount,
    EventSignatureCount, EventStatBucket, EventStatGroup, EventTimeBucket, PersistRecord,
};
use yagra_bus::EventKind;

/// Format a Unix-millis instant as the canonical LogsQL/`_time` literal (UTC, millisecond
/// precision, `Z` suffix — no numeric offset, so it interpolates cleanly into a query).
fn fmt_vl_time_ms(at_unix_ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(at_unix_ms)
        .unwrap_or_else(Utc::now)
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

fn fmt_vl_time_dt(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Quote a value for safe interpolation into a LogsQL phrase/exact filter. Escapes `` and `"`
/// so a device-supplied search term can't break out of the quoted token (injection guard,
/// mirroring `store.rs`'s `promql_label_escape` discipline for PromQL).
fn logsql_quote(v: &str) -> String {
    format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A plain search term against the message: **case-insensitive, matching from the start of a word**
/// — `POLICY` finds `POLICYPERMIT`, `PERMIT` does not.
///
/// This is the one spelling of "what a plain term means" on this backend; the SQL path's is
/// `ILIKE '%term%'`. The two still differ, and the axis is now sub-*word* rather than sub-token.
///
/// **Why a prefix and not a substring, when a substring is what an operator expects.** The index is
/// a dictionary: words in sorted order, each pointing at the blocks holding it. A prefix is a
/// contiguous run of that dictionary, so it is answered from the index; a match inside a word is
/// not in the dictionary at all and can only be found by reading every block in the window.
/// Measured 2026-08-13 on 6,695,066 events, shipping shape (`sort` + `limit 100`), 24h window:
///
///   `i("policypermit")`     exact word          85,462 hits   0.06s
///   `i("policy"*)`          word prefix        116,010 hits   0.12s   ← this
///   `i("firewall"*)`        word prefix            569 hits   0.15s
///   `~"(?i)ewallatck"`      inside a word          566 hits   2.17s   (15× the prefix)
///   `~"(?i)zzzznomatch"`    inside a word            0 hits   1.79s
///
/// Note the inversion in the last two rows: **the more selective the term, the slower the scan**,
/// because `limit` cannot cut it short. Zero hits is the worst case — the whole window must be read
/// before "nothing" can be said. That is why the substring form is not the default and is not made
/// one by widening the range: over 30 days it reaches VictoriaLogs' 30s query ceiling, and the scan
/// grows with the fleet's event volume while the prefix does not.
///
/// The escape hatch for "inside a word" is the regex mode, opted into per query — and, on the
/// Events page only, an automatic second query when this one returns nothing (ADR-053 Inc.2d). That
/// retry deliberately lives in the WebUI rather than here: `GET /events` returns a bare array with
/// nowhere to say "I widened your search", and answering a different question than the one asked
/// without being able to say so is worse than answering narrowly.
fn msg_prefix(term: &str) -> String {
    format!("_msg:i({}*)", logsql_quote(term))
}

/// Node ids the API resolved from the filter's free-text terms, so a term can still find events by
/// **node name** without the name ever entering the log store (ADR-011 keeps names out of the TSDB
/// and log tiers).
///
/// Two sets, not one, and they must not be merged: each is ORed into a *different* clause, so a
/// single combined list would let the Source column's term widen the whole-row search and vice
/// versa. That is the additive/subtractive confusion [`EventFilter::visible_node_ids`] warns about,
/// in its other direction — here the failure is a widening rather than a leak, but it is just as
/// silent.
#[derive(Debug, Default, Clone, Copy)]
pub struct NameIds<'a> {
    /// Ids whose node name matched [`EventFilter::search`] (the whole-row term).
    pub search: &'a [Uuid],
    /// Ids whose node name matched [`EventFilter::source`]'s term (the Source column condition).
    pub source: &'a [Uuid],
}

/// The event-log persistence + search seam (ADR-024). See the module docs.
#[async_trait]
pub trait LogStore: Send + Sync {
    /// Liveness probe for the system-health endpoint. Defaults to `true` (the in-memory fake is
    /// always up); [`VlStore`] overrides it to ping VictoriaLogs.
    async fn healthy(&self) -> bool {
        true
    }
    /// The retention window this store is actually enforcing, as it reports it (ADR-040).
    ///
    /// Same story as [`crate::store::MetricStore::retention_flag`]: VictoriaLogs keeps retention in
    /// a start flag Yagra cannot change at runtime, so it is reported, not set. `None` means
    /// unknown (unreachable, or the product default) and must never be rendered as a number.
    async fn retention_flag(&self) -> Option<String> {
        None
    }
    /// Persist a batch of received events (best-effort — a store hiccup must not stop alerting).
    async fn ingest_batch(&self, records: &[PersistRecord]);
    /// Search the event log, newest first. See [`NameIds`] for the resolved node-name sets.
    async fn search(
        &self,
        filter: &EventFilter,
        names: NameIds<'_>,
        limit: i64,
    ) -> anyhow::Result<Vec<EventRow>>;
    /// Categorical summary counts (kind / action / trap / source) honoring the filter, ordered by
    /// count desc — powers the dashboard passive-event breakdown widgets.
    async fn stats_grouped(
        &self,
        filter: &EventFilter,
        names: NameIds<'_>,
        group: EventStatGroup,
        limit: i64,
    ) -> anyhow::Result<Vec<EventStatBucket>>;
    /// The event-volume time series (counts per `bucket_secs`-wide window, optionally split by
    /// kind) honoring the filter.
    async fn stats_series(
        &self,
        filter: &EventFilter,
        names: NameIds<'_>,
        bucket_secs: i64,
        split_kind: bool,
    ) -> anyhow::Result<Vec<EventTimeBucket>>;

    // ── Troubleshoot analytics (ADR-022) ────────────────────────────────────────────────────────
    //
    // Twins of the `EventRepo` aggregates of the same name. They exist because the analyses were
    // reading PostgreSQL directly, and PostgreSQL holds only the alert-linked subset once a log
    // store is configured (ADR-024) — so `rule_gap`, whose entire job is finding *unmatched* events,
    // was structurally guaranteed to return nothing on exactly the deployments that need it.
    //
    // No [`NameIds`] parameter, unlike `search`/`stats_*`: none of these is driven by a free-text
    // term, so the additive widening sets have no meaning here. Omitting them makes the additive /
    // subtractive confusion `EventFilter::visible_node_ids` warns about unreachable on this path.

    /// Per-(node, time-bucket) event counts. Events with no node are excluded (`event_storm`
    /// attributes a storm to a device).
    async fn agg_counts_by_bucket(
        &self,
        filter: &EventFilter,
        bucket_secs: i64,
    ) -> anyhow::Result<Vec<EventBucketCount>>;

    /// Per-(node, syslog-severity) counts over syslog events (`severity_shift`).
    async fn agg_severity_counts(
        &self,
        filter: &EventFilter,
    ) -> anyhow::Result<Vec<EventSeverityCount>>;

    /// Top unmatched signatures — trap OID, else syslog app-name (`rule_gap`).
    ///
    /// **`sample_node` is always `None` here**, and that is a permitted per-backend difference
    /// rather than an omission: LogsQL has no `min(uuid)`, and after the group scope became a
    /// store-side restriction the representative node is only used to *link* the finding, which
    /// `run_rule_gap` already renders as "fleet" when absent. Pinned by
    /// `the_log_store_signature_path_reports_no_sample_node` so it is not "fixed" into a scan.
    async fn agg_unmatched_signatures(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventSignatureCount>>;

    /// Authentication-failure volume by (source IP, node) — `auth_probe`.
    async fn agg_auth_sources(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventAuthSource>>;
}

// ─── Which store answers an event aggregate (ADR-022 Increment 2) ─────────────────────
//
// One home for the branch, because there are two callers — the Troubleshoot analyses and the MCP
// `event_stats` tool — and a second copy of "ask the log store, else PostgreSQL" is a second place
// someone can forget to change. Both callers reach the stores only through here.
//
// A log-store failure propagates. Falling back to PostgreSQL would answer from the alert-linked
// subset with nothing to say so, which is exactly the defect this module's twins exist to fix; a
// visibly failed job is better than a quietly partial answer.

macro_rules! route_agg {
    ($(#[$m:meta])* $name:ident -> $out:ty, $log:ident, $pg:ident $(, $arg:ident : $ty:ty)*) => {
        $(#[$m])*
        pub(crate) async fn $name(
            logs: Option<&std::sync::Arc<dyn LogStore>>,
            events: &crate::events::EventRepo,
            filter: &EventFilter,
            $($arg: $ty),*
        ) -> anyhow::Result<Vec<$out>> {
            match logs {
                Some(l) => l.$log(filter $(, $arg)*).await,
                None => events.$pg(filter $(, $arg)*).await,
            }
        }
    };
}

route_agg!(
    /// Per-(node, bucket) event counts — `event_storm`, and MCP `event_stats`' volume section.
    route_counts_by_bucket -> EventBucketCount,
    agg_counts_by_bucket, event_counts_by_bucket, bucket_secs: i64
);
route_agg!(
    /// Per-(node, severity) counts — `severity_shift`, and MCP `event_stats`' severity mix.
    route_severity_counts -> EventSeverityCount,
    agg_severity_counts, event_severity_counts
);
route_agg!(
    /// Top unmatched signatures — `rule_gap`, and MCP `event_stats`' rule-gap section.
    route_unmatched_signatures -> EventSignatureCount,
    agg_unmatched_signatures, event_unmatched_signatures, limit: i64
);
route_agg!(
    /// Auth-failure sources — `auth_probe`.
    route_auth_sources -> EventAuthSource,
    agg_auth_sources, event_auth_sources, limit: i64
);

// ─── VictoriaLogs (live) ──────────────────────────────────────────────────────────────

/// A [`LogStore`] backed by VictoriaLogs over HTTP.
pub struct VlStore {
    http: reqwest::Client,
    base: String,
}

impl VlStore {
    /// Point at a VictoriaLogs base URL (e.g. `http://victorialogs:9428`).
    #[must_use]
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base.into(),
        }
    }

    /// Run a LogsQL query and return the NDJSON response body's lines (shared by search + stats).
    async fn query_lines(&self, query: &str) -> anyhow::Result<Vec<String>> {
        let url = format!("{}/select/logsql/query", self.base);
        let resp = self
            .http
            .post(&url)
            .form(&[("query", query)])
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("VictoriaLogs query request failed: {e}"))?;
        if !resp.status().is_success() {
            anyhow::bail!("VictoriaLogs query returned {}", resp.status());
        }
        let body = resp
            .text()
            .await
            .map_err(|e| anyhow::anyhow!("VictoriaLogs query body: {e}"))?;
        Ok(body.lines().map(str::to_owned).collect())
    }
}

/// One event as a VictoriaLogs JSON-stream line: `_time`/`message`/stream+regular fields. Numeric
/// and id fields are emitted as strings so read-back parsing is uniform (VL stores field values as
/// strings). `matched` is a boolean-as-string so the search filter can match it exactly.
fn record_to_json(r: &PersistRecord) -> Value {
    let m = &r.msg;
    let time = fmt_vl_time_ms(m.at_unix_ms);
    let mut obj = serde_json::Map::new();
    obj.insert("_time".into(), json!(time));
    obj.insert("recorded_at".into(), json!(time));
    obj.insert("at_unix_ms".into(), json!(m.at_unix_ms.to_string()));
    obj.insert("event_id".into(), json!(m.event_id.to_string()));
    obj.insert("kind".into(), json!(m.kind.as_str()));
    obj.insert("message".into(), json!(m.message));
    // Stream field: always present (empty when unknown) so the stream is well-defined.
    obj.insert("pool".into(), json!(m.pool.clone().unwrap_or_default()));
    obj.insert(
        "matched".into(),
        json!(if r.matched_rule_id.is_some() {
            "true"
        } else {
            "false"
        }),
    );
    obj.insert("action".into(), json!(r.action));
    if let Some(ip) = m.source_ip {
        obj.insert("source_ip".into(), json!(ip.to_string()));
    }
    if let Some(n) = r.node_id {
        obj.insert("node_id".into(), json!(n.to_string()));
    }
    if let Some(s) = r.source_id {
        obj.insert("source_id".into(), json!(s.to_string()));
    }
    if let Some(rid) = r.matched_rule_id {
        obj.insert("matched_rule_id".into(), json!(rid.to_string()));
    }
    if let Some(f) = m.facility {
        obj.insert("facility".into(), json!(i32::from(f).to_string()));
    }
    if let Some(sv) = m.syslog_severity {
        obj.insert("syslog_severity".into(), json!(i32::from(sv).to_string()));
    }
    if let Some(h) = &m.hostname {
        obj.insert("hostname".into(), json!(h));
    }
    if let Some(a) = &m.app_name {
        obj.insert("app_name".into(), json!(a));
    }
    if let Some(t) = &m.trap_oid {
        obj.insert("trap_oid".into(), json!(t));
    }
    // The middle signature tier (`crate::events::SIGNATURE_TIERS`). Comes off `PersistRecord`, not
    // `EventMsg`, so this and the PostgreSQL insert write the same derived value.
    if let Some(s) = &r.signature {
        obj.insert("signature".into(), json!(s));
    }
    if !m.varbinds.is_empty() {
        obj.insert(
            "varbinds".into(),
            json!(serde_json::to_string(&m.varbinds).unwrap_or_default()),
        );
    }
    Value::Object(obj)
}

/// Make a regex case-insensitive, matching PostgreSQL's `~*`. LogsQL's `~` is case-sensitive, so
/// without this the same pattern meant different things on the two backends.
fn ci_regex(pattern: &str) -> String {
    format!("(?i){pattern}")
}

/// A closed-set clause for one field: nothing when the set is empty (unfiltered), the exact-match
/// form for a single value, `in(…)` for a real set. One helper for `kind` / `action` /
/// `syslog_severity` so the three cannot drift into three spellings.
fn set_clause<'a>(field: &str, values: impl Iterator<Item = &'a str>) -> Option<String> {
    let quoted: Vec<String> = values.map(logsql_quote).collect();
    match quoted.len() {
        0 => None,
        1 => Some(format!("{field}:={}", quoted[0])),
        _ => Some(format!("{field}:in({})", quoted.join(","))),
    }
}

/// `node_id:in("…","…")` for a non-empty id set, or nothing. Distinct from [`set_clause`] because
/// an id set is never collapsed to the exact-match form — the two call sites OR it into a larger
/// disjunction, where a mixed spelling would be one more thing to read carefully.
fn id_set_clause(ids: &[Uuid]) -> Option<String> {
    if ids.is_empty() {
        return None;
    }
    let list = ids
        .iter()
        .map(|i| logsql_quote(&i.to_string()))
        .collect::<Vec<_>>()
        .join(",");
    Some(format!("node_id:in({list})"))
}

/// Wrap a clause in LogsQL's `NOT` when the condition is negated.
fn negate(clause: String, not: bool) -> String {
    if not {
        format!("NOT {clause}")
    } else {
        clause
    }
}

/// Compile an [`EventFilter`] (+ resolved node-name ids) into the LogsQL *filter part* (the leading
/// query before any `|` pipe). Space-separated clauses are ANDed; the free-text term ORs a `_msg`
/// phrase, a `source_ip` phrase, and the resolved `node_id` set (the current 3-way search). Shared
/// by search (`| sort … | limit`) and stats (`| stats by … `), so both filter identically.
fn build_filter_part(filter: &EventFilter, names: NameIds<'_>) -> String {
    let mut clauses: Vec<String> = Vec::new();
    if let Some(before) = filter.before {
        clauses.push(format!("_time:<{}", fmt_vl_time_dt(before)));
    }
    if let Some(since) = filter.since {
        clauses.push(format!("_time:>={}", fmt_vl_time_dt(since)));
    }
    if let Some(until) = filter.until {
        clauses.push(format!("_time:<={}", fmt_vl_time_dt(until)));
    }
    // The three set dimensions share one spelling rule (`set_clause`) rather than three: a single
    // value keeps the exact-match form the guards have always asserted, and only a real set becomes
    // `in(…)`. Measured on 6.7M events, `in(…)` over a list covering every stored value costs what
    // a single value costs, so the split is about keeping the assertions honest, not about speed.
    if let Some(c) = set_clause("kind", filter.kinds.iter().map(String::as_str)) {
        clauses.push(c);
    }
    if let Some(c) = set_clause("action", filter.actions.iter().map(String::as_str)) {
        clauses.push(c);
    }
    // ⚠️ `syslog_severity` is stored as a *string* (`record_to_json`), so the decimal spelling is
    // load-bearing: comparing against an unquoted number matches nothing, silently.
    let sev: Vec<String> = filter.severities.iter().map(i16::to_string).collect();
    if let Some(c) = set_clause("syslog_severity", sev.iter().map(String::as_str)) {
        clauses.push(c);
    }
    if let Some(node) = filter.node_id {
        clauses.push(format!("node_id:={}", logsql_quote(&node.to_string())));
    }
    if let Some(matched) = filter.matched {
        clauses.push(format!(
            "matched:={}",
            logsql_quote(if matched { "true" } else { "false" })
        ));
    }
    if let Some(term) = &filter.search {
        if filter.regex {
            // Regex search is message-only (`~` is LogsQL's regexp operator). Node-name/IP
            // fan-out stays a substring-mode feature; the API skips name resolution here.
            // `(?i)` because the SQL path uses `~*` — without it the same pattern was
            // case-sensitive here and case-insensitive there.
            clauses.push(format!("_msg:~{}", logsql_quote(&ci_regex(term))));
        } else {
            let mut ors = vec![
                msg_prefix(term),
                // Case matters here too, and only here: an IPv6 literal is hex, and the poller may
                // record `FE80::1` where PostgreSQL's `host(inet)` renders `fe80::1`.
                format!("source_ip:i({})", logsql_quote(term)),
            ];
            if let Some(ids) = id_set_clause(names.search) {
                ors.push(ids);
            }
            clauses.push(format!("({})", ors.join(" OR ")));
        }
    }
    // The per-column conditions (ADR-053). Negation is a `NOT` around the whole clause, measured at
    // parity with the form it negates before it shipped — so it is offered in both modes rather
    // than only for the regex one.
    if let Some(c) = &filter.message {
        let inner = if c.regex {
            format!("_msg:~{}", logsql_quote(&ci_regex(&c.term)))
        } else {
            msg_prefix(&c.term)
        };
        clauses.push(negate(inner, c.not));
    }
    if let Some(c) = &filter.source {
        // The Source column shows a node name when the event is attributed and the raw IP when it
        // is not, so the filter has to cover both — the ids are the name half, resolved by the API.
        let mut ors = vec![format!("source_ip:i({})", logsql_quote(&c.term))];
        if let Some(ids) = id_set_clause(names.source) {
            ors.push(ids);
        }
        clauses.push(negate(format!("({})", ors.join(" OR ")), c.not));
    }
    // The RBAC group scope (ADR-014), already resolved to node ids by the API.
    //
    // ⚠️ Pushed **after** the free-text clause and as its own space-separated (i.e. ANDed) term, not
    // into the `ors` above. The `NameIds` sets a few lines up are the *opposite* operation — they
    // widen the search to nodes whose name matches — and putting the two in the same list would
    // turn a
    // restriction into a widening, which is precisely the failure mode the field's doc warns about.
    // The mirror of this clause is `events::EVENT_FILTER_WHERE`'s `$9`, and
    // `both_backends_restrict_to_the_same_visible_node_set` pins the two together.
    if let Some(visible) = &filter.visible_node_ids {
        // An empty visible set matches nothing. `node_id:in()` is not a query VictoriaLogs accepts,
        // so it is spelled as an explicitly unsatisfiable filter rather than omitted — omitting it
        // would return the whole firehose, which is the fail-open inversion.
        if visible.is_empty() {
            clauses.push("node_id:in(\"\")".to_owned());
        } else {
            let ids = visible
                .iter()
                .map(|i| logsql_quote(&i.to_string()))
                .collect::<Vec<_>>()
                .join(",");
            clauses.push(format!("node_id:in({ids})"));
        }
    }
    if clauses.is_empty() {
        "*".to_owned()
    } else {
        clauses.join(" ")
    }
}

/// Compile an [`EventFilter`] (+ resolved node-name ids) into a LogsQL query, newest first.
fn build_search_logsql(filter: &EventFilter, names: NameIds<'_>, limit: i64) -> String {
    format!(
        "{} | sort by (_time) desc | limit {}",
        build_filter_part(filter, names),
        limit.clamp(1, 500)
    )
}

/// The LogsQL `stats by (...)` field(s) for a categorical group — fixed identifiers only (chosen by
/// the enum, never from the request), plus any extra filter clause the dimension needs.
fn stats_group_fields(group: EventStatGroup) -> (&'static str, &'static str) {
    // (extra filter clause, `stats by` fields)
    match group {
        EventStatGroup::Kind => ("", "kind"),
        EventStatGroup::Action => ("", "action"),
        EventStatGroup::Trap => (" trap_oid:*", "trap_oid"),
        EventStatGroup::Source => ("", "node_id, source_ip"),
    }
}

/// Compile a categorical stats query: `<filter> | stats by (<fields>) count() as n | sort …`.
fn build_stats_grouped_logsql(
    filter: &EventFilter,
    names: NameIds<'_>,
    group: EventStatGroup,
    limit: i64,
) -> String {
    let (extra, by) = stats_group_fields(group);
    format!(
        "{}{extra} | stats by ({by}) count() as n | sort by (n) desc | limit {}",
        build_filter_part(filter, names),
        limit.clamp(1, 500)
    )
}

/// Compile the event-volume time series: `<filter> | stats by (_time:Ns[, kind]) count() as n`.
fn build_stats_series_logsql(
    filter: &EventFilter,
    names: NameIds<'_>,
    bucket_secs: i64,
    split_kind: bool,
) -> String {
    let b = bucket_secs.clamp(1, 86_400);
    let by = if split_kind {
        format!("_time:{b}s, kind")
    } else {
        format!("_time:{b}s")
    };
    format!(
        "{} | stats by ({by}) count() as n | sort by (_time) asc",
        build_filter_part(filter, names)
    )
}

// ── Troubleshoot analytics builders (ADR-022) ───────────────────────────────────────────────────
//
// Every `stats by (...)` field list below is a literal. That is the same discipline
// `stats_group_fields` keeps and for the same reason: a field name must never be able to arrive
// from a request. `every_analytics_query_groups_by_literal_fields` pins it.

/// `<filter> node_id:* | stats by (_time:Ns, node_id) count() as n`.
fn build_agg_counts_by_bucket_logsql(filter: &EventFilter, bucket_secs: i64) -> String {
    let b = bucket_secs.clamp(1, 86_400);
    format!(
        "{} node_id:* | stats by (_time:{b}s, node_id) count() as n | sort by (_time) asc",
        build_filter_part(filter, NameIds::default())
    )
}

/// `<filter> kind:="syslog" syslog_severity:* | stats by (node_id, syslog_severity) count() as n`.
fn build_agg_severity_counts_logsql(filter: &EventFilter) -> String {
    format!(
        "{} kind:=\"syslog\" node_id:* syslog_severity:* \
         | stats by (node_id, syslog_severity) count() as n",
        build_filter_part(filter, NameIds::default())
    )
}

/// The unmatched-signature query for **one tier** of [`crate::events::SIGNATURE_TIERS`].
///
/// LogsQL has no `COALESCE`, so "trap OID, else device event code, else app name" cannot be one
/// grouping expression the way it is in SQL. One fixed query per tier, merged in Rust, beats
/// building a single query with a pipe trick: the field list stays literal, and each tier is a
/// query a human can paste into VictoriaLogs unchanged.
///
/// Tier *i* negates every **preceding** tier's field (`-trap_oid:*` …), which is what keeps the
/// queries disjoint and reproduces COALESCE's precedence: a row carrying two of the fields is
/// counted once, under the more specific one.
fn build_agg_unmatched_signature_logsql(filter: &EventFilter, limit: i64, tier: usize) -> String {
    let tiers = crate::events::SIGNATURE_TIERS;
    let field = tiers[tier];
    let excluded: String = tiers[..tier].iter().map(|f| format!(" -{f}:*")).collect();
    format!(
        "{} matched:=\"false\" {field}:*{excluded} | stats by (kind, {field}) count() as n \
         | sort by (n) desc | limit {}",
        build_filter_part(filter, NameIds::default()),
        limit.clamp(1, 500)
    )
}

/// `<filter> (<auth signals>) | stats by (source_ip, node_id) count() as n`.
///
/// ⚠️ The phrases use `i("…")`, the same case-insensitive form the search path now uses, and must
/// not be upgraded to `~"(?i)…"` (~300×, which hit VictoriaLogs' 30s query ceiling on real syslog
/// and was reverted the day it shipped). `run_auth_probe`'s window is always bounded, always
/// explicitly requested, and admission-controlled by the analysis semaphore and per-minute cap —
/// which is the same condition that made case-insensitivity affordable on the Events page once its
/// default range was bounded. Substring matching is not worth 300× on any window.
fn build_agg_auth_sources_logsql(filter: &EventFilter, limit: i64) -> String {
    let mut ors = vec![format!(
        "trap_oid:={}",
        logsql_quote(crate::events::AUTH_FAILURE_TRAP_OID)
    )];
    ors.extend(
        crate::events::AUTH_FAILURE_PHRASES
            .iter()
            .map(|p| format!("_msg:i({})", logsql_quote(p))),
    );
    format!(
        "{} ({}) | stats by (source_ip, node_id) count() as n | sort by (n) desc | limit {}",
        build_filter_part(filter, NameIds::default()),
        ors.join(" OR "),
        limit.clamp(1, 500)
    )
}

/// Read a field VictoriaLogs stores as a string back into an `i64`.
fn vl_i64(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(Value::as_str)?.parse().ok()
}

/// Read a VictoriaLogs field as a non-empty string.
fn vl_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
}

/// Parse one LogsQL categorical stats result line into an [`EventStatBucket`]. VL returns grouped
/// field values + the `n` count (numbers as strings).
fn parse_stat_grouped_row(line: &str, group: EventStatGroup) -> Option<EventStatBucket> {
    let v: Value = serde_json::from_str(line).ok()?;
    let get = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    let count = get("n").and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    Some(match group {
        EventStatGroup::Source => {
            let node_id = get("node_id").and_then(|s| Uuid::parse_str(&s).ok());
            let source_ip = get("source_ip").filter(|s| !s.is_empty());
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
            let key = get("trap_oid").unwrap_or_default();
            let label = trap_oid_name(&key).map(str::to_owned);
            EventStatBucket {
                key,
                label,
                node_id: None,
                count,
            }
        }
        EventStatGroup::Kind => EventStatBucket {
            key: get("kind").unwrap_or_default(),
            label: None,
            node_id: None,
            count,
        },
        EventStatGroup::Action => EventStatBucket {
            key: get("action").unwrap_or_default(),
            label: None,
            node_id: None,
            count,
        },
    })
}

/// Parse one LogsQL time-series stats line into `(bucket_ms, kind, count)`.
fn parse_stat_time_row(line: &str, split_kind: bool) -> Option<(i64, Option<String>, i64)> {
    let v: Value = serde_json::from_str(line).ok()?;
    let get = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    let ts = get("_time")
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|d| d.with_timezone(&Utc).timestamp_millis())?;
    let count = get("n").and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    let kind = if split_kind { get("kind") } else { None };
    Some((ts, kind, count))
}

/// Fold parsed `(bucket_ms, kind, count)` rows into ordered [`EventTimeBucket`]s.
fn fold_time_buckets(
    rows: impl IntoIterator<Item = (i64, Option<String>, i64)>,
    split_kind: bool,
) -> Vec<EventTimeBucket> {
    use std::collections::BTreeMap;
    let mut buckets: BTreeMap<i64, (i64, BTreeMap<String, i64>)> = BTreeMap::new();
    for (ts, kind, n) in rows {
        let entry = buckets.entry(ts).or_default();
        entry.0 += n;
        if let Some(k) = kind {
            *entry.1.entry(k).or_default() += n;
        }
    }
    buckets
        .into_iter()
        .map(|(ts, (count, by))| EventTimeBucket {
            ts_unix_ms: ts,
            count,
            by_kind: split_kind.then_some(by),
        })
        .collect()
}

/// Parse one VictoriaLogs NDJSON result line into an [`EventRow`]. Defensive (like `store.rs`'s
/// `Value`-walking): any missing/mistyped field falls back rather than failing the whole page.
/// Returns `None` only when the line isn't JSON or has no usable `event_id`.
fn parse_ndjson_row(line: &str) -> Option<EventRow> {
    let v: Value = serde_json::from_str(line).ok()?;
    let get = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    let id = Uuid::parse_str(&get("event_id")?).ok()?;
    // VL renames the `message` field to `_msg` (via `_msg_field=message` on ingest).
    let message = get("_msg").or_else(|| get("message")).unwrap_or_default();
    let recorded_at = get("recorded_at")
        .or_else(|| get("_time"))
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);
    Some(EventRow {
        id,
        // Was `unwrap_or_default()`, i.e. the empty string — a kind no reader has a case for, and
        // one PostgreSQL could never produce. Syslog is the honest guess for a log-store row whose
        // `kind` field went missing, and it is at least a value the UI can render.
        kind: get("kind")
            .as_deref()
            .and_then(EventKind::from_token)
            .unwrap_or(EventKind::Syslog),
        at_unix_ms: get("at_unix_ms").and_then(|s| s.parse().ok()).unwrap_or(0),
        recorded_at,
        source_ip: get("source_ip"),
        node_id: get("node_id").and_then(|s| Uuid::parse_str(&s).ok()),
        source_id: get("source_id").and_then(|s| Uuid::parse_str(&s).ok()),
        pool: get("pool").filter(|s| !s.is_empty()),
        facility: get("facility").and_then(|s| s.parse().ok()),
        syslog_severity: get("syslog_severity").and_then(|s| s.parse().ok()),
        hostname: get("hostname"),
        app_name: get("app_name"),
        trap_name: get("trap_oid")
            .as_deref()
            .and_then(trap_oid_name)
            .map(str::to_owned),
        trap_oid: get("trap_oid"),
        varbinds: get("varbinds").and_then(|s| serde_json::from_str(&s).ok()),
        message,
        matched_rule_id: get("matched_rule_id").and_then(|s| Uuid::parse_str(&s).ok()),
        action: get("action")
            .as_deref()
            .map_or(EventAction::None, EventAction::from_stored),
    })
}

#[async_trait]
impl LogStore for VlStore {
    async fn healthy(&self) -> bool {
        let url = format!("{}/health", self.base);
        match self.http.get(&url).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(e) => {
                tracing::warn!(error = %e, "VictoriaLogs health check failed");
                false
            }
        }
    }

    async fn retention_flag(&self) -> Option<String> {
        let url = format!("{}/flags", self.base);
        let body = self.http.get(&url).send().await.ok()?.text().await.ok()?;
        crate::retention::parse_retention_flag(&body)
    }

    async fn ingest_batch(&self, records: &[PersistRecord]) {
        if records.is_empty() {
            return;
        }
        let mut body = String::new();
        for r in records {
            if let Ok(line) = serde_json::to_string(&record_to_json(r)) {
                body.push_str(&line);
                body.push('\n');
            }
        }
        let url = format!(
            "{}/insert/jsonline?_stream_fields=kind,pool&_msg_field=message&_time_field=_time",
            self.base
        );
        match self
            .http
            .post(&url)
            .header("Content-Type", "application/stream+json")
            .body(body)
            .send()
            .await
        {
            Ok(resp) if !resp.status().is_success() => {
                tracing::warn!(status = %resp.status(), "VictoriaLogs ingest non-2xx");
            }
            Err(e) => tracing::warn!(error = %e, "VictoriaLogs ingest request failed"),
            Ok(_) => {}
        }
    }

    async fn search(
        &self,
        filter: &EventFilter,
        names: NameIds<'_>,
        limit: i64,
    ) -> anyhow::Result<Vec<EventRow>> {
        let query = build_search_logsql(filter, names, limit);
        let lines = self.query_lines(&query).await?;
        Ok(lines.iter().filter_map(|l| parse_ndjson_row(l)).collect())
    }

    async fn stats_grouped(
        &self,
        filter: &EventFilter,
        names: NameIds<'_>,
        group: EventStatGroup,
        limit: i64,
    ) -> anyhow::Result<Vec<EventStatBucket>> {
        let query = build_stats_grouped_logsql(filter, names, group, limit);
        let lines = self.query_lines(&query).await?;
        Ok(lines
            .iter()
            .filter_map(|l| parse_stat_grouped_row(l, group))
            .collect())
    }

    async fn stats_series(
        &self,
        filter: &EventFilter,
        names: NameIds<'_>,
        bucket_secs: i64,
        split_kind: bool,
    ) -> anyhow::Result<Vec<EventTimeBucket>> {
        let query = build_stats_series_logsql(filter, names, bucket_secs, split_kind);
        let lines = self.query_lines(&query).await?;
        let parsed = lines
            .iter()
            .filter_map(|l| parse_stat_time_row(l, split_kind));
        Ok(fold_time_buckets(parsed, split_kind))
    }

    async fn agg_counts_by_bucket(
        &self,
        filter: &EventFilter,
        bucket_secs: i64,
    ) -> anyhow::Result<Vec<EventBucketCount>> {
        let lines = self
            .query_lines(&build_agg_counts_by_bucket_logsql(filter, bucket_secs))
            .await?;
        Ok(lines
            .iter()
            .filter_map(|l| {
                let v: Value = serde_json::from_str(l).ok()?;
                Some(EventBucketCount {
                    // `node_id:*` already excluded unattributed events; a row whose id will not
                    // parse is dropped rather than attributed to a nil UUID.
                    node_id: vl_str(&v, "node_id").and_then(|s| Uuid::parse_str(&s).ok())?,
                    // VL reports the bucket as an RFC 3339 `_time`; the DTO wants epoch seconds.
                    bucket_start_s: vl_str(&v, "_time")
                        .and_then(|t| DateTime::parse_from_rfc3339(&t).ok())
                        .map(|t| t.timestamp())?,
                    count: vl_i64(&v, "n").unwrap_or(0),
                })
            })
            .collect())
    }

    async fn agg_severity_counts(
        &self,
        filter: &EventFilter,
    ) -> anyhow::Result<Vec<EventSeverityCount>> {
        let lines = self
            .query_lines(&build_agg_severity_counts_logsql(filter))
            .await?;
        Ok(lines
            .iter()
            .filter_map(|l| {
                let v: Value = serde_json::from_str(l).ok()?;
                Some(EventSeverityCount {
                    node_id: vl_str(&v, "node_id").and_then(|s| Uuid::parse_str(&s).ok())?,
                    // Written as a string by `record_to_json`, like every other numeric field.
                    severity: i16::try_from(vl_i64(&v, "syslog_severity")?).ok()?,
                    count: vl_i64(&v, "n").unwrap_or(0),
                })
            })
            .collect())
    }

    async fn agg_unmatched_signatures(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventSignatureCount>> {
        let mut out: Vec<EventSignatureCount> = Vec::new();
        for (tier, field) in crate::events::SIGNATURE_TIERS.iter().enumerate() {
            let q = build_agg_unmatched_signature_logsql(filter, limit, tier);
            for line in self.query_lines(&q).await? {
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let Some(signature) = vl_str(&v, field) else {
                    continue;
                };
                out.push(EventSignatureCount {
                    kind: vl_str(&v, "kind").unwrap_or_default(),
                    signature,
                    count: vl_i64(&v, "n").unwrap_or(0),
                    // See the trait doc: no `min(uuid)` in LogsQL, and the caller renders `None`.
                    sample_node: None,
                });
            }
        }
        // Re-sort across the tiers: each was ordered and capped on its own, so the merge is only
        // the top-N of the union once it is sorted again.
        out.sort_by_key(|s| std::cmp::Reverse(s.count));
        out.truncate(limit.clamp(1, 500) as usize);
        Ok(out)
    }

    async fn agg_auth_sources(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventAuthSource>> {
        let lines = self
            .query_lines(&build_agg_auth_sources_logsql(filter, limit))
            .await?;
        Ok(lines
            .iter()
            .filter_map(|l| {
                let v: Value = serde_json::from_str(l).ok()?;
                Some(EventAuthSource {
                    source_ip: vl_str(&v, "source_ip"),
                    node_id: vl_str(&v, "node_id").and_then(|s| Uuid::parse_str(&s).ok()),
                    count: vl_i64(&v, "n").unwrap_or(0),
                })
            })
            .collect())
    }
}

// ─── In-memory fake (tests) ───────────────────────────────────────────────────────────

/// Build an [`EventRow`] straight from a stored record (no VL round-trip) — the fake's view.
#[cfg(test)]
fn record_to_event_row(r: &PersistRecord) -> EventRow {
    let m = &r.msg;
    let varbinds =
        (!m.varbinds.is_empty()).then(|| serde_json::to_value(&m.varbinds).unwrap_or(Value::Null));
    EventRow {
        id: m.event_id,
        kind: m.kind,
        at_unix_ms: m.at_unix_ms,
        recorded_at: DateTime::<Utc>::from_timestamp_millis(m.at_unix_ms).unwrap_or_else(Utc::now),
        source_ip: m.source_ip.map(|ip| ip.to_string()),
        node_id: r.node_id,
        source_id: r.source_id,
        pool: m.pool.clone(),
        facility: m.facility.map(i16::from),
        syslog_severity: m.syslog_severity.map(i16::from),
        hostname: m.hostname.clone(),
        app_name: m.app_name.clone(),
        trap_name: m
            .trap_oid
            .as_deref()
            .and_then(trap_oid_name)
            .map(str::to_owned),
        trap_oid: m.trap_oid.clone(),
        varbinds,
        message: m.message.clone(),
        matched_rule_id: r.matched_rule_id,
        action: r.action.to_owned(),
    }
}

/// The fake's model of [`msg_prefix`]: does any **word** in `haystack` start with `needle`?
///
/// `needle` must already be lower-cased. A word starts at index 0 or after a separator, so
/// `i("policy"*)` finds `POLICYPERMIT` while `i("ermit"*)` finds nothing in it. A needle containing
/// separators works the same way: only its first character has to land on a word start and the rest
/// matches literally, which is the phrase-prefix behaviour VictoriaLogs gives (`i("cid=0x814f"*)`
/// matched; `i("policy/6"*)` did not, the word there being `01POLICY`).
///
/// ⚠️ **A word is `[a-z0-9_]` — the underscore is *not* a separator.** Measured against the live
/// store on 2026-08-13 after the first version of this function guessed otherwise: `i("to"*)`
/// returns 836 rows while `Trust_to_Untrust` appears in 111,021 of them, so the underscore is
/// inside the word. `-` `.` `=` `/` `%` all do separate (`i("zone"*)` finds `source-zone=trust`,
/// `i("168"*)` finds `192.168.1.119`).
///
/// ⚠️ This mirrors an *engine*, so it can only ever be approximate. What it must not be is **more
/// permissive** than the engine — and the underscore bug was exactly that, in exactly the direction
/// this note already warned about: every test passed while the deployment returned fewer rows. A
/// fake can be wrong in two ways and only the narrow one is discoverable.
#[cfg(test)]
fn word_prefix_match(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let hay = haystack.to_lowercase();
    let bytes = hay.as_bytes();
    let word_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    hay.match_indices(needle)
        .any(|(i, _)| i == 0 || !word_byte(bytes[i - 1]))
}

/// Whether a stored record satisfies the filter. It models **PostgreSQL** on every axis but one,
/// and the exception is the plain term: there the two backends legitimately differ, so this follows
/// the *log store* — a word-prefix match ([`word_prefix_match`], mirroring [`msg_prefix`]) rather
/// than PostgreSQL's substring.
///
/// That choice is deliberate and is the safe direction. A fake can only be wrong in two ways, and
/// only one of them is discoverable: if it is **narrower** than a real backend, a test that expects
/// a row fails and someone looks. If it is **wider**, every test passes while a deployment returns
/// fewer rows than the suite believed. Case is no longer an axis at all — both backends fold it in
/// both modes. Everything else is shared, and
/// `both_backends_filter_on_event_time_with_the_same_case_rules` is what keeps it that way.
#[cfg(test)]
fn record_matches(r: &PersistRecord, f: &EventFilter, names: NameIds<'_>) -> bool {
    let m = &r.msg;
    let ts = DateTime::<Utc>::from_timestamp_millis(m.at_unix_ms).unwrap_or_else(Utc::now);
    if let Some(before) = f.before {
        if ts >= before {
            return false;
        }
    }
    if let Some(since) = f.since {
        if ts < since {
            return false;
        }
    }
    if let Some(until) = f.until {
        if ts > until {
            return false;
        }
    }
    if !f.kinds.is_empty() && !f.kinds.iter().any(|k| k == m.kind.as_str()) {
        return false;
    }
    if !f.actions.is_empty() && !f.actions.iter().any(|a| a == r.action.as_str()) {
        return false;
    }
    // A trap has no syslog severity, so it cannot satisfy a severity filter — same as `NULL = ANY`
    // in SQL and a missing field in LogsQL.
    if !f.severities.is_empty()
        && !m
            .syslog_severity
            .is_some_and(|s| f.severities.contains(&i16::from(s)))
    {
        return false;
    }
    if let Some(node) = f.node_id {
        if r.node_id != Some(node) {
            return false;
        }
    }
    if let Some(matched) = f.matched {
        if r.matched_rule_id.is_some() != matched {
            return false;
        }
    }
    if let Some(term) = &f.search {
        if f.regex {
            // Message-only, case-insensitive regex (mirrors both `_msg:~"(?i)…"` and SQL `~*`).
            // A pattern that fails to compile matches nothing (the API rejects it at the edge
            // before it reaches here).
            match regex::Regex::new(&ci_regex(term)) {
                Ok(re) if re.is_match(&m.message) => {}
                _ => return false,
            }
        } else {
            let t = term.to_lowercase();
            let hit_msg = word_prefix_match(&m.message, &t);
            let hit_ip = m
                .source_ip
                .map(|ip| ip.to_string().to_lowercase().contains(&t))
                .unwrap_or(false);
            let hit_name = r.node_id.is_some_and(|n| names.search.contains(&n));
            if !(hit_msg || hit_ip || hit_name) {
                return false;
            }
        }
    }
    // The per-column conditions (ADR-053), modelling the PostgreSQL contract on the same axis the
    // whole-row term already differs on. Negation is applied to the whole match, not inside it —
    // "not (a or b)", never "(not a) or b", which is the reading that quietly returns everything.
    if let Some(c) = &f.message {
        let hit = if c.regex {
            regex::Regex::new(&ci_regex(&c.term)).is_ok_and(|re| re.is_match(&m.message))
        } else {
            word_prefix_match(&m.message, &c.term.to_lowercase())
        };
        if hit == c.not {
            return false;
        }
    }
    if let Some(c) = &f.source {
        let t = c.term.to_lowercase();
        let hit = m
            .source_ip
            .is_some_and(|ip| ip.to_string().to_lowercase().contains(&t))
            || r.node_id.is_some_and(|n| names.source.contains(&n));
        if hit == c.not {
            return false;
        }
    }
    // The RBAC restriction, applied last and independently of the search — the third
    // implementation of the same rule (SQL `$9`, LogsQL `node_id:in(…)`, and this fake), which is
    // why `both_backends_restrict_to_the_same_visible_node_set` compares them rather than each on
    // its own. An event with no node id never satisfies a restriction.
    if let Some(visible) = &f.visible_node_ids {
        if !r.node_id.is_some_and(|n| visible.contains(&n)) {
            return false;
        }
    }
    true
}

/// An in-memory [`LogStore`] for tests.
#[cfg(test)]
#[derive(Default)]
pub struct InMemoryLogStore {
    records: Mutex<Vec<PersistRecord>>,
}

#[cfg(test)]
impl InMemoryLogStore {
    /// Number of stored records (test assertions).
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.lock().expect("log fake mutex poisoned").len()
    }

    /// Whether the store holds no records.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
#[async_trait]
impl LogStore for InMemoryLogStore {
    async fn ingest_batch(&self, records: &[PersistRecord]) {
        self.records
            .lock()
            .expect("log fake mutex poisoned")
            .extend(records.iter().cloned());
    }

    async fn search(
        &self,
        filter: &EventFilter,
        names: NameIds<'_>,
        limit: i64,
    ) -> anyhow::Result<Vec<EventRow>> {
        let guard = self.records.lock().expect("log fake mutex poisoned");
        let mut rows: Vec<EventRow> = guard
            .iter()
            .filter(|r| record_matches(r, filter, names))
            .map(record_to_event_row)
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.at_unix_ms));
        rows.truncate(limit.clamp(1, 500) as usize);
        Ok(rows)
    }

    async fn stats_grouped(
        &self,
        filter: &EventFilter,
        names: NameIds<'_>,
        group: EventStatGroup,
        limit: i64,
    ) -> anyhow::Result<Vec<EventStatBucket>> {
        use std::collections::HashMap;
        let guard = self.records.lock().expect("log fake mutex poisoned");
        // Aggregate a count per group identity, carrying the label/node for the DTO (first seen).
        let mut agg: HashMap<String, (Option<String>, Option<Uuid>, i64)> = HashMap::new();
        for r in guard.iter().filter(|r| record_matches(r, filter, names)) {
            let m = &r.msg;
            let (key, label, node): (String, Option<String>, Option<Uuid>) = match group {
                EventStatGroup::Kind => (m.kind.as_str().to_owned(), None, None),
                EventStatGroup::Action => (r.action.as_str().to_owned(), None, None),
                EventStatGroup::Trap => match &m.trap_oid {
                    Some(oid) => (oid.clone(), trap_oid_name(oid).map(str::to_owned), None),
                    None => continue, // trap grouping drops non-traps (mirrors `trap_oid:*`)
                },
                EventStatGroup::Source => {
                    let ip = m.source_ip.map(|i| i.to_string());
                    let key = r
                        .node_id
                        .map(|n| n.to_string())
                        .or_else(|| ip.clone())
                        .unwrap_or_default();
                    (key, ip, r.node_id)
                }
            };
            let e = agg.entry(key).or_insert((label, node, 0));
            e.2 += 1;
        }
        let mut out: Vec<EventStatBucket> = agg
            .into_iter()
            .map(|(key, (label, node_id, count))| EventStatBucket {
                key,
                label,
                node_id,
                count,
            })
            .collect();
        out.sort_by_key(|b| std::cmp::Reverse(b.count));
        out.truncate(limit.clamp(1, 500) as usize);
        Ok(out)
    }

    async fn stats_series(
        &self,
        filter: &EventFilter,
        names: NameIds<'_>,
        bucket_secs: i64,
        split_kind: bool,
    ) -> anyhow::Result<Vec<EventTimeBucket>> {
        let b = bucket_secs.clamp(1, 86_400);
        let guard = self.records.lock().expect("log fake mutex poisoned");
        let rows = guard
            .iter()
            .filter(|r| record_matches(r, filter, names))
            .map(|r| {
                let bucket_ms = (r.msg.at_unix_ms / 1000 / b) * b * 1000;
                let kind = split_kind.then(|| r.msg.kind.as_str().to_owned());
                (bucket_ms, kind, 1_i64)
            });
        Ok(fold_time_buckets(rows, split_kind))
    }

    // The analytics twins. Each reuses `record_matches`, so the fake inherits exactly the filter
    // semantics the two real backends are pinned to — including the empty-visible-set rule, which
    // is what `both_backends_restrict_to_the_same_visible_node_set` extends over these four.

    async fn agg_counts_by_bucket(
        &self,
        filter: &EventFilter,
        bucket_secs: i64,
    ) -> anyhow::Result<Vec<EventBucketCount>> {
        use std::collections::HashMap;
        let b = bucket_secs.max(1);
        let guard = self.records.lock().expect("log fake mutex poisoned");
        let mut agg: HashMap<(Uuid, i64), i64> = HashMap::new();
        for r in guard
            .iter()
            .filter(|r| record_matches(r, filter, NameIds::default()))
        {
            let Some(node) = r.node_id else { continue };
            *agg.entry((node, (r.msg.at_unix_ms / 1000 / b) * b))
                .or_insert(0) += 1;
        }
        Ok(agg
            .into_iter()
            .map(|((node_id, bucket_start_s), count)| EventBucketCount {
                node_id,
                bucket_start_s,
                count,
            })
            .collect())
    }

    async fn agg_severity_counts(
        &self,
        filter: &EventFilter,
    ) -> anyhow::Result<Vec<EventSeverityCount>> {
        use std::collections::HashMap;
        let guard = self.records.lock().expect("log fake mutex poisoned");
        let mut agg: HashMap<(Uuid, i16), i64> = HashMap::new();
        for r in guard
            .iter()
            .filter(|r| record_matches(r, filter, NameIds::default()))
        {
            if r.msg.kind != yagra_bus::EventKind::Syslog {
                continue;
            }
            let (Some(node), Some(sev)) = (r.node_id, r.msg.syslog_severity) else {
                continue;
            };
            *agg.entry((node, i16::from(sev))).or_insert(0) += 1;
        }
        Ok(agg
            .into_iter()
            .map(|((node_id, severity), count)| EventSeverityCount {
                node_id,
                severity,
                count,
            })
            .collect())
    }

    async fn agg_unmatched_signatures(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventSignatureCount>> {
        use std::collections::HashMap;
        let guard = self.records.lock().expect("log fake mutex poisoned");
        let mut agg: HashMap<(String, String), i64> = HashMap::new();
        for r in guard
            .iter()
            .filter(|r| record_matches(r, filter, NameIds::default()))
        {
            if r.matched_rule_id.is_some() {
                continue;
            }
            // `SIGNATURE_TIERS` precedence, applied through the one accessor list — the same rule
            // the SQL COALESCE and the disjoint LogsQL tiers implement.
            let Some(sig) = r.signature_key() else {
                continue;
            };
            *agg.entry((r.msg.kind.as_str().to_owned(), sig.to_owned()))
                .or_insert(0) += 1;
        }
        let mut out: Vec<EventSignatureCount> = agg
            .into_iter()
            .map(|((kind, signature), count)| EventSignatureCount {
                kind,
                signature,
                count,
                sample_node: None,
            })
            .collect();
        out.sort_by_key(|s| std::cmp::Reverse(s.count));
        out.truncate(limit.clamp(1, 500) as usize);
        Ok(out)
    }

    async fn agg_auth_sources(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventAuthSource>> {
        use std::collections::HashMap;
        let guard = self.records.lock().expect("log fake mutex poisoned");
        let mut agg: HashMap<(Option<String>, Option<Uuid>), i64> = HashMap::new();
        for r in guard
            .iter()
            .filter(|r| record_matches(r, filter, NameIds::default()))
        {
            let lower = r.msg.message.to_lowercase();
            let is_auth = r.msg.trap_oid.as_deref() == Some(crate::events::AUTH_FAILURE_TRAP_OID)
                || crate::events::AUTH_FAILURE_PHRASES
                    .iter()
                    .any(|p| lower.contains(p));
            if !is_auth {
                continue;
            }
            *agg.entry((r.msg.source_ip.map(|i| i.to_string()), r.node_id))
                .or_insert(0) += 1;
        }
        let mut out: Vec<EventAuthSource> = agg
            .into_iter()
            .map(|((source_ip, node_id), count)| EventAuthSource {
                source_ip,
                node_id,
                count,
            })
            .collect();
        out.sort_by_key(|s| std::cmp::Reverse(s.count));
        out.truncate(limit.clamp(1, 500) as usize);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::TextCond;
    use yagra_bus::{EventKind, EventMsg};

    fn msg(id: Uuid, message: &str, at_unix_ms: i64) -> EventMsg {
        EventMsg {
            event_id: id,
            kind: EventKind::Syslog,
            at_unix_ms,
            source_ip: Some("10.0.0.1".parse().unwrap()),
            pool: Some("tokyo".into()),
            message: message.into(),
            facility: Some(3),
            syslog_severity: Some(5),
            hostname: Some("rtr1".into()),
            app_name: None,
            trap_oid: None,
            varbinds: Vec::new(),
            truncated: false,
            raw: None,
            src_port: None,
        }
    }

    fn record(id: Uuid, message: &str, at_unix_ms: i64, action: EventAction) -> PersistRecord {
        let m = msg(id, message, at_unix_ms);
        PersistRecord {
            // Derived exactly as `handle_event` derives it, so a fixture cannot carry a signature
            // the real write path would never have produced for that message.
            signature: yagra_ingest::extract_signature(&m.message).map(|s| s.text.to_owned()),
            msg: m,
            node_id: Some(Uuid::from_u128(1)),
            source_id: None,
            matched_rule_id: (action != EventAction::None).then(Uuid::new_v4),
            action,
        }
    }

    /// [`NameIds`] carrying only the whole-row term's ids — the shape most of these cases want.
    fn search_names(ids: &[Uuid]) -> NameIds<'_> {
        NameIds {
            search: ids,
            source: &[],
        }
    }

    #[test]
    fn logsql_quote_escapes_quotes_and_backslashes() {
        assert_eq!(logsql_quote("plain"), "\"plain\"");
        assert_eq!(logsql_quote(r#"a"b"#), "\"a\\\"b\"");
        assert_eq!(logsql_quote(r"a\b"), "\"a\\\\b\"");
    }

    #[test]
    fn build_logsql_maps_every_filter() {
        let node = Uuid::from_u128(7);
        let before = DateTime::parse_from_rfc3339("2024-01-02T03:04:05Z")
            .unwrap()
            .with_timezone(&Utc);
        let since = DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let until = DateTime::parse_from_rfc3339("2024-01-02T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let filter = EventFilter {
            before: Some(before),
            since: Some(since),
            until: Some(until),
            kinds: vec!["syslog".into()],
            node_id: Some(node),
            matched: Some(true),
            search: Some("link down".into()),
            regex: false,
            visible_node_ids: None,
            ..Default::default()
        };
        let name_ids = [Uuid::from_u128(9)];
        let q = build_search_logsql(&filter, search_names(&name_ids), 100);
        assert!(q.contains("_time:<2024-01-02T03:04:05.000Z"));
        assert!(q.contains("_time:>=2024-01-01T00:00:00.000Z"));
        assert!(q.contains("_time:<=2024-01-02T00:00:00.000Z"));
        assert!(q.contains("kind:=\"syslog\""));
        assert!(q.contains(&format!("node_id:=\"{node}\"")));
        assert!(q.contains("matched:=\"true\""));
        assert!(q.contains("_msg:i(\"link down\"*)"));
        assert!(q.contains("source_ip:i(\"link down\")"));
        assert!(q.contains(&format!("node_id:in(\"{}\")", Uuid::from_u128(9))));
        assert!(q.ends_with("| sort by (_time) desc | limit 100"));
    }

    #[test]
    fn a_plain_term_stays_a_phrase_filter_not_a_regex_scan() {
        // Guards a fix that was tried and reverted: a plain term must never become `_msg:~`.
        //
        // ⚠️ The *permitted* spelling widened on 2026-08-13 and this assertion moved with it —
        // `i("term")` → `i("term"*)`, a word **prefix**, so `POLICY` finds `POLICYPERMIT`. What is
        // forbidden is unchanged and is the whole point of the test: the regex form scans every
        // block in the window because a match inside a word is not in the dictionary. The prefix is
        // in the dictionary, so it costs what an exact word costs (0.12s vs 0.06s over 24h on 6.7M
        // events; the regex form is 2.17s for a *more* selective term, and 1.79s to return nothing
        // at all). Full table in `msg_prefix`.
        //
        // Read the earlier revert with that in mind: it failed because it reached for the regex
        // form, not because operators were wrong to expect `POLICY` to find `POLICYPERMIT`.
        let filter = EventFilter {
            search: Some("link".into()),
            regex: false,
            ..EventFilter::default()
        };
        let q = build_search_logsql(&filter, NameIds::default(), 100);
        assert!(q.contains("_msg:i(\"link\"*)"), "{q}");
        assert!(
            !q.contains("_msg:~"),
            "plain term must not become a regex scan: {q}"
        );
    }

    #[test]
    fn both_backends_filter_on_event_time_with_the_same_case_rules() {
        // The one test that pins the SQL and LogsQL builders to *each other*. They are separate
        // implementations of one `EventFilter` and they drifted on four points at once — time
        // column, substring-vs-phrase, regex case, node-name resolution — with nothing failing.
        //
        // Three of the four are now shared and asserted here. The fourth was two axes wearing one
        // name — *case* and *sub-token granularity* — and only the second is still a permitted
        // difference: see `a_plain_term_stays_a_phrase_filter_…` for the 300× measurement behind
        // refusing it, and for why case became affordable once the Events page stopped defaulting
        // to an unbounded range. Permitted, not forgotten — anything else that changes the contract
        // has to change both sides or fail here.
        use crate::events::{EVENT_FILTER_BINDS, EVENT_FILTER_WHERE};

        // 1. Time bounds are event time on both sides. `recorded_at` is ingest time and must not
        //    appear in the predicate at all — VictoriaLogs has no equivalent to compare against.
        assert!(EVENT_FILTER_WHERE.contains("e.at_unix_ms < $1"));
        assert!(EVENT_FILTER_WHERE.contains("e.at_unix_ms >= $2"));
        assert!(EVENT_FILTER_WHERE.contains("e.at_unix_ms <= $3"));
        assert!(
            !EVENT_FILTER_WHERE.contains("recorded_at"),
            "{EVENT_FILTER_WHERE}"
        );

        // 2. Case is now unified in BOTH modes. A regex is case-insensitive either way — `~*`
        //    there, `(?i)` here — and so is a plain term: `ILIKE` there, `i("…")` here. Neither
        //    backend may match `SSH` and miss `ssh`, which is what the plain-term side used to do
        //    on VictoriaLogs only, so the same query answered differently per deployment.
        assert!(EVENT_FILTER_WHERE.contains("ILIKE"));
        assert!(EVENT_FILTER_WHERE.contains("e.message ~* $7"));
        let pattern = build_filter_part(
            &EventFilter {
                search: Some("Term".into()),
                regex: true,
                ..EventFilter::default()
            },
            NameIds::default(),
        );
        assert!(pattern.contains("(?i)"), "{pattern}");
        let plain = build_filter_part(
            &EventFilter {
                search: Some("Term".into()),
                regex: false,
                ..EventFilter::default()
            },
            NameIds::default(),
        );
        assert!(plain.contains("_msg:i(\"Term\"*)"), "{plain}");
        assert!(plain.contains("source_ip:i(\"Term\")"), "{plain}");

        // 3. Every filter dimension reaches both sides. A field added to one builder only is the
        //    exact failure this catches, and it is why the per-column conditions and the three set
        //    dimensions are in this fixture rather than in a test of their own.
        let full = EventFilter {
            before: Some(Utc::now()),
            since: Some(Utc::now()),
            until: Some(Utc::now()),
            kinds: vec!["trap".into()],
            actions: vec!["fired".into()],
            severities: vec![3],
            node_id: Some(Uuid::from_u128(3)),
            matched: Some(false),
            search: Some("x".into()),
            regex: false,
            message: Some(TextCond {
                term: "boom".into(),
                regex: false,
                not: true,
            }),
            source: Some(TextCond {
                term: "10.0.0.1".into(),
                regex: false,
                not: false,
            }),
            visible_node_ids: None,
        };
        let logsql = build_filter_part(&full, NameIds::default());
        for clause in [
            "_time:<",
            "_time:>=",
            "_time:<=",
            "kind:=",
            "action:=",
            "syslog_severity:=",
            "node_id:=",
            "matched:=",
            "_msg:",
            "NOT _msg:i(\"boom\"*)",
            "source_ip:i(\"10.0.0.1\")",
        ] {
            assert!(logsql.contains(clause), "LogsQL missing {clause}: {logsql}");
        }
        // Every bind the predicate declares is present, and the count is the one the consumers'
        // own trailing bind is derived from — so widening the filter cannot leave a `$n` behind.
        for n in 1..=EVENT_FILTER_BINDS {
            assert!(
                EVENT_FILTER_WHERE.contains(&format!("${n}")),
                "SQL missing ${n}"
            );
        }
        for col in ["e.action", "e.syslog_severity", "e.message", "n.name"] {
            assert!(EVENT_FILTER_WHERE.contains(col), "SQL missing {col}");
        }
    }

    #[test]
    fn the_fake_matches_from_a_word_start_like_the_real_index_does() {
        // The fake is the third implementation of "what a plain term means", and the only one no
        // deployment ever runs — so it is the one that can drift without anybody noticing. These
        // cases are transcribed from queries actually issued against the test server's store on
        // 2026-08-13, not from what the syntax looks like it should do.
        let msg = "Aug 13 2026 12:19:33 jpmyj01fw01 %%01POLICY/6/POLICYPERMIT(l):CID=0x814f041e";
        for (needle, want, why) in [
            (
                "policy",
                true,
                "a word starts with it — the case this change is for",
            ),
            ("policypermit", true, "an exact word is a prefix of itself"),
            (
                "ermit",
                false,
                "inside a word: `i(\"ermit\"*)` returned 0 on the real store",
            ),
            (
                "permit",
                false,
                "the operator's complaint — only the widening query finds this",
            ),
            (
                "cid=0x814f",
                true,
                "separators are literal; only the start must land on a word",
            ),
            (
                "policy/6",
                false,
                "the word is `01POLICY`, so the phrase never starts",
            ),
            (
                "POLICY",
                true,
                "case-insensitive, which is what `i(…)` buys",
            ),
            ("", true, "an empty term is no condition at all"),
        ] {
            assert_eq!(
                word_prefix_match(msg, &needle.to_lowercase()),
                want,
                "{needle:?}: {why}"
            );
        }
        // ⚠️ The direction that matters: the fake must never be *more* permissive than the engine,
        // or every test passes while the deployment returns fewer rows than the test believed.
        assert!(
            !word_prefix_match("POLICYPERMIT", "ermit"),
            "the fake fell back to substring matching"
        );
        // 🚨 The underscore, which the first version of this function got wrong in exactly that
        // direction. It is a *word character*, so `Trust_to_Untrust` contains no word `to` —
        // measured, because the guess ("splits on non-alphanumerics") was wrong: `i("to"*)` returns
        // 836 rows on the live store while `Trust_to_Untrust` occurs in 111,021 of them. Everything
        // else in this line does separate.
        assert!(!word_prefix_match("Trust_to_Untrust", "to"));
        assert!(word_prefix_match("Trust_to_Untrust", "trust"));
        for sep in ['-', '.', '=', '/', ' ', ':'] {
            assert!(
                word_prefix_match(&format!("x{sep}policy"), "policy"),
                "{sep:?} should separate words"
            );
        }
        // The same rule the WebUI's highlighter has to reach — `web/src/lib/matchRanges.ts` is the
        // fourth implementation of this, and its test carries these same cases.
        assert!(!word_prefix_match("x_policy", "policy"));
    }

    #[test]
    fn a_negated_term_stays_a_phrase_filter_too() {
        // The sibling of `a_plain_term_stays_a_phrase_filter_not_a_regex_scan`, for the dimension
        // ADR-053 added. Negation must not become the back door through which a substring scan
        // ships: `NOT _msg:i("…")` is the phrase form inverted, `NOT _msg:~"…"` is a scan of every
        // block. The two were measured at parity on 6.7M events (0.152s vs 0.156s over 24h), which
        // is *why* negation is offered in both modes — not a reason to spell it the expensive way.
        let plain = build_filter_part(
            &EventFilter {
                message: Some(TextCond {
                    term: "policypermit".into(),
                    regex: false,
                    not: true,
                }),
                ..EventFilter::default()
            },
            NameIds::default(),
        );
        assert!(plain.contains("NOT _msg:i(\"policypermit\"*)"), "{plain}");
        assert!(!plain.contains("_msg:~"), "{plain}");

        // The regex mode still negates the regex, and still case-folds it.
        let pattern = build_filter_part(
            &EventFilter {
                message: Some(TextCond {
                    term: "^LINK".into(),
                    regex: true,
                    not: true,
                }),
                ..EventFilter::default()
            },
            NameIds::default(),
        );
        assert!(pattern.contains("NOT _msg:~\"(?i)^LINK\""), "{pattern}");
    }

    #[test]
    fn a_single_value_keeps_the_exact_form_and_a_set_becomes_in() {
        // The exact form is what every existing guard asserts, and `kind:="syslog"` is also what a
        // v0.2.6 client's single-value request produced — so the two spellings are not
        // interchangeable for the purposes of *reading* this file, whatever VictoriaLogs makes of
        // them.
        let one = build_filter_part(
            &EventFilter {
                kinds: vec!["syslog".into()],
                ..EventFilter::default()
            },
            NameIds::default(),
        );
        assert!(one.contains("kind:=\"syslog\""), "{one}");
        let many = build_filter_part(
            &EventFilter {
                kinds: vec!["syslog".into(), "trap".into()],
                ..EventFilter::default()
            },
            NameIds::default(),
        );
        assert!(many.contains("kind:in(\"syslog\",\"trap\")"), "{many}");
    }

    #[test]
    fn severity_is_quoted_as_its_decimal_string() {
        // `record_to_json` stores `syslog_severity` as a string. Comparing against a bare number
        // matches nothing at all, and nothing about that failure says so — the page just empties.
        let q = build_filter_part(
            &EventFilter {
                severities: vec![4, 6],
                ..EventFilter::default()
            },
            NameIds::default(),
        );
        assert!(q.contains("syslog_severity:in(\"4\",\"6\")"), "{q}");
        assert!(!q.contains("syslog_severity:in(4"), "{q}");
    }

    #[test]
    fn a_source_condition_covers_the_ip_and_the_resolved_node_names() {
        // The Source column shows a node name where the event is attributed and the raw IP where it
        // is not, so a filter that only matched one of them would return nothing for values the
        // operator can plainly see in the column they typed into.
        let ids = [Uuid::from_u128(11)];
        let q = build_filter_part(
            &EventFilter {
                source: Some(TextCond {
                    term: "rtr".into(),
                    regex: false,
                    not: false,
                }),
                ..EventFilter::default()
            },
            NameIds {
                search: &[],
                source: &ids,
            },
        );
        assert!(q.contains("source_ip:i(\"rtr\")"), "{q}");
        assert!(q.contains(&format!("node_id:in(\"{}\")", ids[0])), "{q}");

        // ⚠️ The inversion that matters here: the *source* term's ids must not leak into the
        // whole-row term's OR group, and vice versa. Resolving one term and ORing it into the
        // other's clause widens that clause by a set nobody asked it to include.
        let split = build_filter_part(
            &EventFilter {
                search: Some("down".into()),
                source: Some(TextCond {
                    term: "rtr".into(),
                    regex: false,
                    not: false,
                }),
                ..EventFilter::default()
            },
            NameIds {
                search: &[Uuid::from_u128(22)],
                source: &ids,
            },
        );
        assert!(
            split.contains(&format!(
                "(_msg:i(\"down\"*) OR source_ip:i(\"down\") OR node_id:in(\"{}\"))",
                Uuid::from_u128(22)
            )),
            "{split}"
        );
        assert!(
            split.contains(&format!(
                "(source_ip:i(\"rtr\") OR node_id:in(\"{}\"))",
                ids[0]
            )),
            "{split}"
        );
    }

    #[test]
    fn both_backends_restrict_to_the_same_visible_node_set() {
        // The RBAC group scope (ADR-014) is the newest dimension of `EventFilter`, and the one with
        // the worst failure direction: it is a *restriction*, and every way of getting it wrong
        // makes it wider. So all three implementations are checked against each other here — the
        // SQL predicate, the LogsQL clause, and the in-memory fake `record_matches`.
        use crate::events::EVENT_FILTER_WHERE;
        let mine = Uuid::from_u128(11);
        let theirs = Uuid::from_u128(22);

        // 1. The SQL side restricts on the array, and does it at the top level — not folded into
        //    the `$7` search alternation, where an `OR` would let non-matching rows back in.
        assert!(EVENT_FILTER_WHERE.contains("($9::uuid[] IS NULL OR e.node_id = ANY($9))"));

        // This used to assert the restriction was the *last* conjunct, which was a proxy for what
        // actually matters and stopped being available the moment ADR-053 appended per-column
        // conditions after it. The real invariant is stronger and is now checked directly: every
        // top-level conjunct is a balanced parenthesised group, so no clause's `OR` can reach out
        // of its own group and widen a neighbour — whatever order they are written in.
        let mut depth = 0i32;
        let mut conjuncts: Vec<String> = Vec::new();
        let mut current = String::new();
        let chars: Vec<char> = EVENT_FILTER_WHERE.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            // A top-level " AND " ends a conjunct; one inside parentheses belongs to it.
            if depth == 0 && chars[i..].starts_with(&[' ', 'A', 'N', 'D', ' ']) {
                conjuncts.push(std::mem::take(&mut current));
                i += 5;
                continue;
            }
            current.push(chars[i]);
            i += 1;
        }
        conjuncts.push(current);
        assert!(
            conjuncts.len() >= 9,
            "the predicate stopped parsing as a conjunction: {conjuncts:?}"
        );
        for c in &conjuncts {
            let c = c.trim();
            assert!(
                c.starts_with('(') && c.ends_with(')'),
                "a top-level conjunct is not self-contained, so a neighbouring OR could widen it: \
                 {c}"
            );
        }
        assert!(
            conjuncts
                .iter()
                .any(|c| c.contains("$9::uuid[]") && c.contains("= ANY($9)")),
            "the group-scope restriction is not a top-level conjunct: {EVENT_FILTER_WHERE}"
        );

        // 2. The LogsQL side emits an ANDed `in(…)` term, separate from the free-text `OR` group.
        let scoped = EventFilter {
            visible_node_ids: Some(vec![mine]),
            ..EventFilter::default()
        };
        let q = build_filter_part(&scoped, NameIds::default());
        assert!(q.contains(&format!("node_id:in(\"{mine}\")")), "{q}");

        // ⚠️ The inversion that matters. `name_node_ids` is the *additive* term — it widens a text
        // search to nodes whose name matched — and it must never be able to satisfy a restriction.
        // Search for a term that matches `theirs` by name while scoped to `mine`: both clauses must
        // be present and ANDed, so the restriction still applies.
        let both = EventFilter {
            search: Some("router".into()),
            visible_node_ids: Some(vec![mine]),
            ..EventFilter::default()
        };
        let q = build_filter_part(&both, search_names(&[theirs]));
        assert!(q.contains(&format!("node_id:in(\"{theirs}\")")), "{q}"); // widening, inside the OR
        assert!(q.contains(&format!("node_id:in(\"{mine}\")")), "{q}"); // restriction, ANDed
        let or_group = q.split(" OR ").count();
        assert!(or_group > 1, "the search term still ORs its alternatives");
        assert!(
            q.trim_end().ends_with(&format!("node_id:in(\"{mine}\")")),
            "the restriction must sit outside the search's OR group: {q}"
        );

        // 3. An empty visible set matches nothing — the fail-open inversion, on all three sides.
        let none_visible = EventFilter {
            visible_node_ids: Some(Vec::new()),
            ..EventFilter::default()
        };
        let q = build_filter_part(&none_visible, NameIds::default());
        assert!(
            q.contains("node_id:in(\"\")"),
            "an empty scope must emit an unsatisfiable filter, never no filter: {q}"
        );
        assert!(!q.contains('*'), "and must not fall back to match-all: {q}");

        // 4. The four Troubleshoot analytics builders (ADR-022 Increment 2) inherit all of the
        //    above by composing `build_filter_part` rather than assembling their own filter. That
        //    is the whole reason they were written that way, so it is asserted rather than assumed:
        //    a builder that hand-rolled its clauses would be the one that forgets the restriction.
        let tiers = 0..crate::events::SIGNATURE_TIERS.len();
        for q in [
            build_agg_counts_by_bucket_logsql(&none_visible, 300),
            build_agg_severity_counts_logsql(&none_visible),
            build_agg_auth_sources_logsql(&none_visible, 20),
        ]
        .into_iter()
        .chain(
            tiers
                .clone()
                .map(|t| build_agg_unmatched_signature_logsql(&none_visible, 20, t)),
        ) {
            assert!(
                q.contains("node_id:in(\"\")"),
                "an analytics query must inherit the unsatisfiable empty scope: {q}"
            );
        }
        for q in [
            build_agg_counts_by_bucket_logsql(&scoped, 300),
            build_agg_severity_counts_logsql(&scoped),
            build_agg_auth_sources_logsql(&scoped, 20),
        ]
        .into_iter()
        .chain(tiers.map(|t| build_agg_unmatched_signature_logsql(&scoped, 20, t)))
        {
            assert!(q.contains(&format!("node_id:in(\"{mine}\")")), "{q}");
        }
    }

    /// Every analytics `stats by (...)` names literal fields.
    ///
    /// The same discipline `stats_group_fields` keeps, extended over the four new builders: a field
    /// name must never be able to arrive from a request. Checked by asserting the exact field list
    /// of each, so adding an interpolated one fails here rather than reaching VictoriaLogs.
    #[test]
    fn every_analytics_query_groups_by_literal_fields() {
        let f = EventFilter::default();
        let cases = [
            (
                build_agg_counts_by_bucket_logsql(&f, 300),
                "stats by (_time:300s, node_id)",
            ),
            (
                build_agg_severity_counts_logsql(&f),
                "stats by (node_id, syslog_severity)",
            ),
            (
                build_agg_auth_sources_logsql(&f, 20),
                "stats by (source_ip, node_id)",
            ),
        ];
        for (q, by) in cases {
            assert!(q.contains(by), "expected `{by}` in: {q}");
            assert!(q.contains("count() as n"), "{q}");
        }
        for (tier, field) in crate::events::SIGNATURE_TIERS.iter().enumerate() {
            let q = build_agg_unmatched_signature_logsql(&f, 20, tier);
            assert!(q.contains(&format!("stats by (kind, {field})")), "{q}");
            assert!(q.contains("count() as n"), "{q}");
            // Every tier reads unmatched rows only. `rule_gap` over matched events is not a gap.
            assert!(q.contains("matched:=\"false\""), "{q}");
        }
    }

    /// The two backends must cluster `rule_gap` on the same key, in the same order.
    ///
    /// PostgreSQL expresses the precedence as one `COALESCE`; LogsQL, which has no `COALESCE`, as
    /// one query per tier with the preceding tiers negated. Those are different enough mechanisms
    /// that nothing but this test makes them agree — and disagreeing is not a crash, it is two
    /// surfaces quietly reporting different rule gaps for the same fleet. Both are generated from
    /// [`crate::events::SIGNATURE_TIERS`], so this asserts the generation, not a transcription.
    #[test]
    fn the_two_backends_cluster_signatures_in_the_same_order() {
        let tiers = crate::events::SIGNATURE_TIERS;
        let f = EventFilter::default();

        // PostgreSQL: one COALESCE, tiers in order. Built at runtime from the list — a literal
        // needle here would be a copy of the thing under test rather than a check of it.
        let expected = format!(
            "COALESCE({})",
            tiers
                .iter()
                .map(|t| format!("e.{t}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let sql = crate::events::agg_unmatched_signatures_sql();
        assert!(sql.contains(&expected), "expected `{expected}` in: {sql}");

        // LogsQL: tier i selects its own field and negates exactly the tiers above it. Anything
        // less makes the tiers overlap, and a row carrying two of the fields is counted twice.
        for (tier, field) in tiers.iter().enumerate() {
            let q = build_agg_unmatched_signature_logsql(&f, 20, tier);
            assert!(
                q.contains(&format!("{field}:*")),
                "tier {tier} must select its own field: {q}"
            );
            for higher in &tiers[..tier] {
                assert!(
                    q.contains(&format!("-{higher}:*")),
                    "tier {tier} must exclude the more specific `{higher}`: {q}"
                );
            }
            for lower in &tiers[tier + 1..] {
                assert!(
                    !q.contains(&format!("-{lower}:*")),
                    "tier {tier} must not exclude the coarser `{lower}`: {q}"
                );
            }
        }
    }

    /// ⚠️ `auth_probe` buys case-insensitive matching, and the price is measured: `~"(?i)term"` is
    /// ~300× a plain phrase and hit VictoriaLogs' 30s ceiling on real syslog (it shipped and was
    /// reverted the same day, 285f58a → 0497fc2). The purchase is defensible because
    /// `run_auth_probe`'s window is always bounded and admission-controlled — the same reason the
    /// Events search could take it once its default range stopped being unbounded.
    ///
    /// So: phrases, never a regex scan. Same shape as
    /// `a_plain_term_stays_a_phrase_filter_not_a_regex_scan`, which guards the search path.
    #[test]
    fn auth_probe_uses_a_case_insensitive_phrase_not_a_regex_scan() {
        let q = build_agg_auth_sources_logsql(&EventFilter::default(), 20);
        assert!(q.contains("_msg:i("), "expected phrase filters: {q}");
        assert!(
            !q.contains("_msg:~"),
            "a regex scan here is ~300× and reaches VictoriaLogs' query ceiling: {q}"
        );
        // The vocabulary is shared with the SQL side, so both ask the same question.
        for p in crate::events::AUTH_FAILURE_PHRASES {
            assert!(q.contains(p), "{p} missing from: {q}");
        }
        assert!(q.contains(crate::events::AUTH_FAILURE_TRAP_OID), "{q}");
    }

    /// The permitted per-backend difference, written down with its reason so it is a design rather
    /// than a bug: LogsQL has no `min(uuid)`, so the log-store path reports no representative node.
    /// After the group scope became a store-side restriction that field only *labels* a finding,
    /// and `run_rule_gap` already renders its absence as "fleet".
    #[tokio::test]
    async fn the_log_store_signature_path_reports_no_sample_node() {
        let store = InMemoryLogStore::default();
        let node = Uuid::from_u128(7);
        let mut r = record(Uuid::new_v4(), "unmatched thing", 1_000, EventAction::None);
        r.node_id = Some(node);
        r.msg.app_name = Some("sshd".into());
        store.ingest_batch(&[r]).await;

        let got = store
            .agg_unmatched_signatures(&EventFilter::default(), 20)
            .await
            .unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].signature, "sshd");
        assert!(
            got[0].sample_node.is_none(),
            "the log-store path deliberately carries no representative node"
        );
    }

    /// The regression test for the worst symptom this increment fixes.
    ///
    /// With a log store configured, PostgreSQL keeps only alert-linked rows — so an unmatched event
    /// never reaches it, and `rule_gap`, whose entire purpose is finding high-volume *unmatched*
    /// events, was structurally guaranteed to return nothing on exactly the deployments that
    /// generate enough syslog to need it.
    #[tokio::test]
    async fn unmatched_signatures_are_readable_when_the_log_store_is_on() {
        let store = InMemoryLogStore::default();
        let mut batch = Vec::new();
        for i in 0..5 {
            let mut r = record(
                Uuid::new_v4(),
                "no rule matches me",
                1_000 + i,
                EventAction::None,
            );
            r.node_id = Some(Uuid::from_u128(1));
            r.msg.app_name = Some("noisy-daemon".into());
            batch.push(r);
        }
        // One matched row, which must NOT be counted as a gap.
        let mut matched = record(Uuid::new_v4(), "handled", 2_000, EventAction::Fired);
        matched.node_id = Some(Uuid::from_u128(1));
        matched.msg.app_name = Some("handled-daemon".into());
        matched.matched_rule_id = Some(Uuid::from_u128(99));
        batch.push(matched);
        store.ingest_batch(&batch).await;

        let got = store
            .agg_unmatched_signatures(&EventFilter::default(), 20)
            .await
            .unwrap();
        assert_eq!(
            got.len(),
            1,
            "only the unmatched signature is a gap: {got:?}"
        );
        assert_eq!(got[0].signature, "noisy-daemon");
        assert_eq!(got[0].count, 5);
    }

    /// The regression test for the *second* wall, and the reason this module gained a middle tier.
    ///
    /// A Huawei USG emits six figures of syslog a day whose datagrams parse as neither RFC 3164
    /// (its timestamp carries a year) nor RFC 5424, so `app_name` is NULL — and it is syslog, so
    /// `trap_oid` is NULL too. Under the old two-tier key every one of those events was unclusterable
    /// and `rule_gap` returned an empty list, which reads as "no monitoring gaps" rather than "not
    /// measurable". The message bodies are the real captures from `jpmyj01fw01`.
    #[tokio::test]
    async fn a_device_with_no_app_name_still_clusters_on_its_own_event_code() {
        let store = InMemoryLogStore::default();
        let bodies = [
            (
                "Aug  7 2026 15:43:42 jpmyj01fw01 %%01URL/4/FILTER(l):CID=0x814f0420;The URL \
              filtering policy was matched. (SrcIp=192.168.1.142, DstIp=182.22.31.124)",
                3,
            ),
            (
                "Aug  7 2026 15:43:41 jpmyj01fw01 %%01POLICY/6/POLICYPERMIT(l):CID=0x814f041e;\
              vsys=public, protocol=6, source-ip=192.168.1.142",
                2,
            ),
        ];
        let mut batch = Vec::new();
        for (body, n) in bodies {
            for i in 0..n {
                let mut r = record(Uuid::new_v4(), body, 1_000 + i, EventAction::None);
                r.node_id = Some(Uuid::from_u128(1));
                // Exactly what the device gives us: neither of the original two tiers.
                assert!(r.msg.app_name.is_none() && r.msg.trap_oid.is_none());
                batch.push(r);
            }
        }
        store.ingest_batch(&batch).await;

        let got = store
            .agg_unmatched_signatures(&EventFilter::default(), 20)
            .await
            .unwrap();
        let mut seen: Vec<(&str, i64)> = got.iter().map(|s| (&*s.signature, s.count)).collect();
        seen.sort_unstable();
        assert_eq!(
            seen,
            [("POLICY/6/POLICYPERMIT", 2), ("URL/4/FILTER", 3)],
            "the device's own event codes must cluster: {got:?}"
        );
    }

    /// A trap OID wins over an app name on the same row — SQL's `COALESCE` precedence, which the
    /// LogsQL tiers reproduce by negating the tiers above them.
    #[tokio::test]
    async fn a_signature_is_counted_once_under_its_trap_oid() {
        // A row carrying all three tiers is counted once, under the most specific — and each tier
        // in turn wins when the ones above it are absent. Together with
        // `the_two_backends_cluster_signatures_in_the_same_order` (which pins the query builders to
        // the same list) this covers the precedence end to end.
        for (drop_above, expected) in [
            (0, "1.3.6.1.6.3.1.1.5.3"),
            (1, "URL/4/FILTER"),
            (2, "also-set"),
        ] {
            let store = InMemoryLogStore::default();
            let mut r = record(
                Uuid::new_v4(),
                "%%01URL/4/FILTER(l):body",
                1_000,
                EventAction::None,
            );
            r.node_id = Some(Uuid::from_u128(1));
            r.msg.trap_oid = Some("1.3.6.1.6.3.1.1.5.3".into());
            r.msg.app_name = Some("also-set".into());
            if drop_above > 0 {
                r.msg.trap_oid = None;
            }
            if drop_above > 1 {
                r.signature = None;
            }
            store.ingest_batch(&[r]).await;

            let got = store
                .agg_unmatched_signatures(&EventFilter::default(), 20)
                .await
                .unwrap();
            assert_eq!(got.len(), 1, "counted more than once: {got:?}");
            assert_eq!(got[0].signature, expected);
        }
    }

    /// Severity arrives from VictoriaLogs as a *string* (`record_to_json` writes every numeric
    /// field that way), so the read-back has a conversion the SQL path does not.
    #[tokio::test]
    async fn severity_counts_round_trip_through_the_log_store() {
        let store = InMemoryLogStore::default();
        let node = Uuid::from_u128(3);
        for (sev, n) in [(3u8, 2), (6u8, 1)] {
            for i in 0..n {
                let mut r = record(
                    Uuid::new_v4(),
                    "msg",
                    1_000 + i64::from(i),
                    EventAction::None,
                );
                r.node_id = Some(node);
                r.msg.syslog_severity = Some(sev);
                store.ingest_batch(&[r]).await;
            }
        }
        let mut got = store
            .agg_severity_counts(&EventFilter::default())
            .await
            .unwrap();
        got.sort_by_key(|s| s.severity);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!((got[0].severity, got[0].count), (3, 2));
        assert_eq!((got[1].severity, got[1].count), (6, 1));
        assert!(got.iter().all(|s| s.node_id == node));
    }

    #[test]
    fn the_in_memory_fake_restricts_by_the_same_rule_as_the_two_query_builders() {
        // `record_matches` is what every store test asserts against, so if it disagreed with the
        // real builders the tests would pass while the deployment leaked.
        let mine = Uuid::from_u128(11);
        let theirs = Uuid::from_u128(22);
        let rec = |node: Option<Uuid>| PersistRecord {
            node_id: node,
            ..record(Uuid::new_v4(), "link down", 1_000, EventAction::None)
        };
        let scoped = EventFilter {
            visible_node_ids: Some(vec![mine]),
            ..EventFilter::default()
        };
        assert!(record_matches(
            &rec(Some(mine)),
            &scoped,
            NameIds::default()
        ));
        assert!(!record_matches(
            &rec(Some(theirs)),
            &scoped,
            NameIds::default()
        ));
        // An unattributed event is hidden from a scoped caller — the same rule an ungrouped node
        // gets, and it matters most here because syslog bodies routinely carry credentials.
        assert!(!record_matches(&rec(None), &scoped, NameIds::default()));
        // …but is still visible when unrestricted, which is the behaviour that must not regress.
        assert!(record_matches(
            &rec(None),
            &EventFilter::default(),
            NameIds::default()
        ));
        // An empty scope sees nothing at all.
        let empty = EventFilter {
            visible_node_ids: Some(Vec::new()),
            ..EventFilter::default()
        };
        assert!(!record_matches(
            &rec(Some(mine)),
            &empty,
            NameIds::default()
        ));
    }

    #[test]
    fn build_logsql_regex_search_is_message_only() {
        let filter = EventFilter {
            search: Some("^%LINK-3".into()),
            regex: true,
            ..EventFilter::default()
        };
        // Even with resolved name ids, regex mode restricts to the message field.
        let q = build_search_logsql(&filter, search_names(&[Uuid::from_u128(9)]), 100);
        // `(?i)` mirrors the SQL path's `~*`; the operator's pattern is otherwise passed through.
        assert!(q.contains("_msg:~\"(?i)^%LINK-3\""), "{q}");
        assert!(!q.contains("source_ip:"));
        assert!(!q.contains("node_id:in("));
    }

    #[test]
    fn build_logsql_matches_all_when_no_filters() {
        let q = build_search_logsql(&EventFilter::default(), NameIds::default(), 50);
        assert_eq!(q, "* | sort by (_time) desc | limit 50");
    }

    #[test]
    fn build_stats_grouped_logsql_maps_each_group() {
        let none = EventFilter::default();
        let q = build_stats_grouped_logsql(&none, NameIds::default(), EventStatGroup::Kind, 10);
        assert!(q.contains("| stats by (kind) count() as n"), "{q}");
        assert!(q.ends_with("| sort by (n) desc | limit 10"), "{q}");
        let q = build_stats_grouped_logsql(&none, NameIds::default(), EventStatGroup::Action, 10);
        assert!(q.contains("| stats by (action) count() as n"), "{q}");
        // Trap grouping requires the OID present, then groups on it.
        let q = build_stats_grouped_logsql(&none, NameIds::default(), EventStatGroup::Trap, 8);
        assert!(
            q.contains("trap_oid:* | stats by (trap_oid) count() as n"),
            "{q}"
        );
        // Source groups by node + source IP together.
        let q = build_stats_grouped_logsql(&none, NameIds::default(), EventStatGroup::Source, 8);
        assert!(
            q.contains("| stats by (node_id, source_ip) count() as n"),
            "{q}"
        );
        // The shared filter part still applies (e.g. a kind filter).
        let filtered = EventFilter {
            kinds: vec!["trap".into()],
            ..EventFilter::default()
        };
        let q = build_stats_grouped_logsql(&filtered, NameIds::default(), EventStatGroup::Trap, 8);
        assert!(q.contains("kind:=\"trap\""), "{q}");
    }

    #[test]
    fn build_stats_series_logsql_buckets_and_splits() {
        let none = EventFilter::default();
        let q = build_stats_series_logsql(&none, NameIds::default(), 3600, false);
        assert!(q.contains("| stats by (_time:3600s) count() as n"), "{q}");
        assert!(q.ends_with("| sort by (_time) asc"), "{q}");
        let q = build_stats_series_logsql(&none, NameIds::default(), 3600, true);
        assert!(
            q.contains("| stats by (_time:3600s, kind) count() as n"),
            "{q}"
        );
    }

    #[tokio::test]
    async fn in_memory_stats_grouped_and_series() {
        let store = InMemoryLogStore::default();
        store
            .ingest_batch(&[
                record(Uuid::from_u128(1), "link down", 1_000, EventAction::Fired),
                record(Uuid::from_u128(2), "link up", 2_000, EventAction::Cleared),
                record(Uuid::from_u128(3), "noise", 3_000, EventAction::None),
            ])
            .await;
        // By kind: all three are syslog (the test `msg()` helper).
        let by_kind = store
            .stats_grouped(
                &EventFilter::default(),
                NameIds::default(),
                EventStatGroup::Kind,
                10,
            )
            .await
            .unwrap();
        assert_eq!(by_kind.len(), 1);
        assert_eq!(by_kind[0].key, "syslog");
        assert_eq!(by_kind[0].count, 3);
        // By action: three distinct outcomes, one each.
        let by_action = store
            .stats_grouped(
                &EventFilter::default(),
                NameIds::default(),
                EventStatGroup::Action,
                10,
            )
            .await
            .unwrap();
        assert_eq!(by_action.len(), 3);
        assert_eq!(by_action.iter().map(|b| b.count).sum::<i64>(), 3);
        // Volume series bucketed at 1s: three distinct buckets, no split.
        let series = store
            .stats_series(&EventFilter::default(), NameIds::default(), 1, false)
            .await
            .unwrap();
        assert_eq!(series.len(), 3);
        assert_eq!(series.iter().map(|b| b.count).sum::<i64>(), 3);
        assert!(series[0].by_kind.is_none());
        // Split by kind populates the per-kind map.
        let split = store
            .stats_series(&EventFilter::default(), NameIds::default(), 1, true)
            .await
            .unwrap();
        assert!(split[0]
            .by_kind
            .as_ref()
            .is_some_and(|m| m.contains_key("syslog")));
    }

    #[test]
    fn record_json_round_trips_through_ndjson_parse() {
        let id = Uuid::from_u128(42);
        let rec = record(
            id,
            "%LINEPROTO-5-UPDOWN",
            1_700_000_000_000,
            EventAction::Fired,
        );
        let line = serde_json::to_string(&record_to_json(&rec)).unwrap();
        // VL renames `message`→`_msg`; simulate that so parse mirrors the query response.
        let mut v: Value = serde_json::from_str(&line).unwrap();
        let obj = v.as_object_mut().unwrap();
        let m = obj.remove("message").unwrap();
        obj.insert("_msg".into(), m);
        let row = parse_ndjson_row(&serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(row.id, id);
        assert_eq!(row.kind, EventKind::Syslog);
        assert_eq!(row.message, "%LINEPROTO-5-UPDOWN");
        assert_eq!(row.at_unix_ms, 1_700_000_000_000);
        assert_eq!(row.source_ip.as_deref(), Some("10.0.0.1"));
        assert_eq!(row.pool.as_deref(), Some("tokyo"));
        assert_eq!(row.facility, Some(3));
        assert_eq!(row.syslog_severity, Some(5));
        assert_eq!(row.action, EventAction::Fired);
        assert!(row.matched_rule_id.is_some());
    }

    #[tokio::test]
    async fn in_memory_ingest_and_search_newest_first() {
        let store = InMemoryLogStore::default();
        assert!(store.is_empty());
        store
            .ingest_batch(&[
                record(
                    Uuid::from_u128(1),
                    "link down ge-0/0/1",
                    1_000,
                    EventAction::Fired,
                ),
                record(
                    Uuid::from_u128(2),
                    "config changed",
                    2_000,
                    EventAction::None,
                ),
                record(
                    Uuid::from_u128(3),
                    "link up ge-0/0/1",
                    3_000,
                    EventAction::Cleared,
                ),
            ])
            .await;
        assert_eq!(store.len(), 3);

        // Newest first, all rows.
        let all = store
            .search(&EventFilter::default(), NameIds::default(), 100)
            .await
            .unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].at_unix_ms, 3_000);

        // Free-text term matches message substring.
        let mut f = EventFilter {
            search: Some("link".into()),
            ..EventFilter::default()
        };
        assert_eq!(
            store
                .search(&f, NameIds::default(), 100)
                .await
                .unwrap()
                .len(),
            2
        );

        // matched=false selects the unmatched record only.
        f = EventFilter {
            matched: Some(false),
            ..EventFilter::default()
        };
        let unmatched = store.search(&f, NameIds::default(), 100).await.unwrap();
        assert_eq!(unmatched.len(), 1);
        assert_eq!(unmatched[0].message, "config changed");
    }

    #[tokio::test]
    async fn in_memory_search_by_resolved_node_name() {
        let store = InMemoryLogStore::default();
        store
            .ingest_batch(&[record(
                Uuid::from_u128(1),
                "opaque body",
                1_000,
                EventAction::None,
            )])
            .await;
        // The term hits neither the message nor the IP, but the node id was resolved from a name.
        let f = EventFilter {
            search: Some("core-rtr".into()),
            ..EventFilter::default()
        };
        assert_eq!(
            store
                .search(&f, NameIds::default(), 100)
                .await
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            store
                .search(&f, search_names(&[Uuid::from_u128(1)]), 100)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn in_memory_time_range_bounds() {
        let store = InMemoryLogStore::default();
        store
            .ingest_batch(&[
                record(Uuid::from_u128(1), "first", 1_000, EventAction::None),
                record(Uuid::from_u128(2), "second", 2_000, EventAction::None),
                record(Uuid::from_u128(3), "third", 3_000, EventAction::None),
            ])
            .await;
        let at = |ms: i64| DateTime::<Utc>::from_timestamp_millis(ms).unwrap();

        // since only: keep 2_000 and 3_000.
        let f = EventFilter {
            since: Some(at(1_500)),
            ..EventFilter::default()
        };
        assert_eq!(
            store
                .search(&f, NameIds::default(), 100)
                .await
                .unwrap()
                .len(),
            2
        );

        // until only: keep 1_000 and 2_000.
        let f = EventFilter {
            until: Some(at(2_500)),
            ..EventFilter::default()
        };
        assert_eq!(
            store
                .search(&f, NameIds::default(), 100)
                .await
                .unwrap()
                .len(),
            2
        );

        // both bounds: only the 2_000 record falls inside [1_500, 2_500].
        let f = EventFilter {
            since: Some(at(1_500)),
            until: Some(at(2_500)),
            ..EventFilter::default()
        };
        let rows = store.search(&f, NameIds::default(), 100).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message, "second");
    }

    #[tokio::test]
    async fn in_memory_regex_search_matches_message() {
        let store = InMemoryLogStore::default();
        store
            .ingest_batch(&[
                record(
                    Uuid::from_u128(1),
                    "link down ge-0/0/1",
                    1_000,
                    EventAction::Fired,
                ),
                record(
                    Uuid::from_u128(2),
                    "config changed",
                    2_000,
                    EventAction::None,
                ),
                record(
                    Uuid::from_u128(3),
                    "link up ge-0/0/1",
                    3_000,
                    EventAction::Cleared,
                ),
            ])
            .await;
        let f = EventFilter {
            search: Some("^link (up|down)".into()),
            regex: true,
            ..EventFilter::default()
        };
        assert_eq!(
            store
                .search(&f, NameIds::default(), 100)
                .await
                .unwrap()
                .len(),
            2
        );
    }
}
