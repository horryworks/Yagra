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

use crate::events::{EventFilter, EventRow, PersistRecord};

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

/// Quote a value for safe interpolation into a LogsQL phrase/exact filter. Escapes `\` and `"`
/// so a device-supplied search term can't break out of the quoted token (injection guard,
/// mirroring `store.rs`'s `promql_label_escape` discipline for PromQL).
fn logsql_quote(v: &str) -> String {
    format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The event-log persistence + search seam (ADR-024). See the module docs.
#[async_trait]
pub trait LogStore: Send + Sync {
    /// Liveness probe for the system-health endpoint. Defaults to `true` (the in-memory fake is
    /// always up); [`VlStore`] overrides it to ping VictoriaLogs.
    async fn healthy(&self) -> bool {
        true
    }
    /// Persist a batch of received events (best-effort — a store hiccup must not stop alerting).
    async fn ingest_batch(&self, records: &[PersistRecord]);
    /// Search the event log, newest first. `name_node_ids` are node ids the API resolved from a
    /// node-name match (so free-text search still finds events by node name without the name ever
    /// entering the store).
    async fn search(
        &self,
        filter: &EventFilter,
        name_node_ids: &[Uuid],
        limit: i64,
    ) -> anyhow::Result<Vec<EventRow>>;
}

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
    if !m.varbinds.is_empty() {
        obj.insert(
            "varbinds".into(),
            json!(serde_json::to_string(&m.varbinds).unwrap_or_default()),
        );
    }
    Value::Object(obj)
}

/// Compile an [`EventFilter`] (+ resolved node-name ids) into a LogsQL query, newest first.
/// Space-separated clauses are ANDed; the free-text term ORs a `_msg` phrase, a `source_ip`
/// phrase, and the resolved `node_id` set (the current 3-way message/IP/node-name search).
fn build_search_logsql(filter: &EventFilter, name_node_ids: &[Uuid], limit: i64) -> String {
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
    if let Some(kind) = &filter.kind {
        clauses.push(format!("kind:={}", logsql_quote(kind)));
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
            clauses.push(format!("_msg:~{}", logsql_quote(term)));
        } else {
            let mut ors = vec![
                format!("_msg:{}", logsql_quote(term)),
                format!("source_ip:{}", logsql_quote(term)),
            ];
            if !name_node_ids.is_empty() {
                let ids = name_node_ids
                    .iter()
                    .map(|i| logsql_quote(&i.to_string()))
                    .collect::<Vec<_>>()
                    .join(",");
                ors.push(format!("node_id:in({ids})"));
            }
            clauses.push(format!("({})", ors.join(" OR ")));
        }
    }
    let filter_part = if clauses.is_empty() {
        "*".to_owned()
    } else {
        clauses.join(" ")
    };
    format!(
        "{filter_part} | sort by (_time) desc | limit {}",
        limit.clamp(1, 500)
    )
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
        kind: get("kind").unwrap_or_default(),
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
        action: get("action").unwrap_or_else(|| "none".to_owned()),
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
        name_node_ids: &[Uuid],
        limit: i64,
    ) -> anyhow::Result<Vec<EventRow>> {
        let query = build_search_logsql(filter, name_node_ids, limit);
        let url = format!("{}/select/logsql/query", self.base);
        let resp = self
            .http
            .post(&url)
            .form(&[("query", query.as_str())])
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
        Ok(body.lines().filter_map(parse_ndjson_row).collect())
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
        kind: m.kind.as_str().to_owned(),
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

/// Whether a stored record satisfies the filter (substring semantics, matching the current
/// PostgreSQL `ILIKE` path — the fake models the API contract, not VL's phrase tokenizer).
#[cfg(test)]
fn record_matches(r: &PersistRecord, f: &EventFilter, name_node_ids: &[Uuid]) -> bool {
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
    if let Some(kind) = &f.kind {
        if m.kind.as_str() != kind {
            return false;
        }
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
            // Message-only regex (mirrors the LogsQL `_msg:~` path). A pattern that fails to
            // compile matches nothing (the API rejects it at the edge before it reaches here).
            match regex::Regex::new(term) {
                Ok(re) if re.is_match(&m.message) => {}
                _ => return false,
            }
        } else {
            let t = term.to_lowercase();
            let hit_msg = m.message.to_lowercase().contains(&t);
            let hit_ip = m
                .source_ip
                .map(|ip| ip.to_string().to_lowercase().contains(&t))
                .unwrap_or(false);
            let hit_name = r.node_id.is_some_and(|n| name_node_ids.contains(&n));
            if !(hit_msg || hit_ip || hit_name) {
                return false;
            }
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
        name_node_ids: &[Uuid],
        limit: i64,
    ) -> anyhow::Result<Vec<EventRow>> {
        let guard = self.records.lock().expect("log fake mutex poisoned");
        let mut rows: Vec<EventRow> = guard
            .iter()
            .filter(|r| record_matches(r, filter, name_node_ids))
            .map(record_to_event_row)
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.at_unix_ms));
        rows.truncate(limit.clamp(1, 500) as usize);
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_bus::{EventKind, EventMsg};

    fn msg(id: Uuid, message: &str, at_unix_ms: i64) -> EventMsg {
        EventMsg {
            schema_version: 1,
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
        }
    }

    fn record(id: Uuid, message: &str, at_unix_ms: i64, action: &'static str) -> PersistRecord {
        PersistRecord {
            msg: msg(id, message, at_unix_ms),
            node_id: Some(Uuid::from_u128(1)),
            source_id: None,
            matched_rule_id: (action != "none").then(Uuid::new_v4),
            action,
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
            kind: Some("syslog".into()),
            node_id: Some(node),
            matched: Some(true),
            search: Some("link down".into()),
            regex: false,
        };
        let name_ids = [Uuid::from_u128(9)];
        let q = build_search_logsql(&filter, &name_ids, 100);
        assert!(q.contains("_time:<2024-01-02T03:04:05.000Z"));
        assert!(q.contains("_time:>=2024-01-01T00:00:00.000Z"));
        assert!(q.contains("_time:<=2024-01-02T00:00:00.000Z"));
        assert!(q.contains("kind:=\"syslog\""));
        assert!(q.contains(&format!("node_id:=\"{node}\"")));
        assert!(q.contains("matched:=\"true\""));
        assert!(q.contains("_msg:\"link down\""));
        assert!(q.contains("source_ip:\"link down\""));
        assert!(q.contains(&format!("node_id:in(\"{}\")", Uuid::from_u128(9))));
        assert!(q.ends_with("| sort by (_time) desc | limit 100"));
    }

    #[test]
    fn build_logsql_regex_search_is_message_only() {
        let filter = EventFilter {
            search: Some("^%LINK-3".into()),
            regex: true,
            ..EventFilter::default()
        };
        // Even with resolved name ids, regex mode restricts to the message field.
        let q = build_search_logsql(&filter, &[Uuid::from_u128(9)], 100);
        assert!(q.contains("_msg:~\"^%LINK-3\""));
        assert!(!q.contains("source_ip:"));
        assert!(!q.contains("node_id:in("));
    }

    #[test]
    fn build_logsql_matches_all_when_no_filters() {
        let q = build_search_logsql(&EventFilter::default(), &[], 50);
        assert_eq!(q, "* | sort by (_time) desc | limit 50");
    }

    #[test]
    fn record_json_round_trips_through_ndjson_parse() {
        let id = Uuid::from_u128(42);
        let rec = record(id, "%LINEPROTO-5-UPDOWN", 1_700_000_000_000, "fired");
        let line = serde_json::to_string(&record_to_json(&rec)).unwrap();
        // VL renames `message`→`_msg`; simulate that so parse mirrors the query response.
        let mut v: Value = serde_json::from_str(&line).unwrap();
        let obj = v.as_object_mut().unwrap();
        let m = obj.remove("message").unwrap();
        obj.insert("_msg".into(), m);
        let row = parse_ndjson_row(&serde_json::to_string(&v).unwrap()).unwrap();
        assert_eq!(row.id, id);
        assert_eq!(row.kind, "syslog");
        assert_eq!(row.message, "%LINEPROTO-5-UPDOWN");
        assert_eq!(row.at_unix_ms, 1_700_000_000_000);
        assert_eq!(row.source_ip.as_deref(), Some("10.0.0.1"));
        assert_eq!(row.pool.as_deref(), Some("tokyo"));
        assert_eq!(row.facility, Some(3));
        assert_eq!(row.syslog_severity, Some(5));
        assert_eq!(row.action, "fired");
        assert!(row.matched_rule_id.is_some());
    }

    #[tokio::test]
    async fn in_memory_ingest_and_search_newest_first() {
        let store = InMemoryLogStore::default();
        assert!(store.is_empty());
        store
            .ingest_batch(&[
                record(Uuid::from_u128(1), "link down ge-0/0/1", 1_000, "fired"),
                record(Uuid::from_u128(2), "config changed", 2_000, "none"),
                record(Uuid::from_u128(3), "link up ge-0/0/1", 3_000, "cleared"),
            ])
            .await;
        assert_eq!(store.len(), 3);

        // Newest first, all rows.
        let all = store
            .search(&EventFilter::default(), &[], 100)
            .await
            .unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].at_unix_ms, 3_000);

        // Free-text term matches message substring.
        let mut f = EventFilter {
            search: Some("link".into()),
            ..EventFilter::default()
        };
        assert_eq!(store.search(&f, &[], 100).await.unwrap().len(), 2);

        // matched=false selects the unmatched record only.
        f = EventFilter {
            matched: Some(false),
            ..EventFilter::default()
        };
        let unmatched = store.search(&f, &[], 100).await.unwrap();
        assert_eq!(unmatched.len(), 1);
        assert_eq!(unmatched[0].message, "config changed");
    }

    #[tokio::test]
    async fn in_memory_search_by_resolved_node_name() {
        let store = InMemoryLogStore::default();
        store
            .ingest_batch(&[record(Uuid::from_u128(1), "opaque body", 1_000, "none")])
            .await;
        // The term hits neither the message nor the IP, but the node id was resolved from a name.
        let f = EventFilter {
            search: Some("core-rtr".into()),
            ..EventFilter::default()
        };
        assert_eq!(store.search(&f, &[], 100).await.unwrap().len(), 0);
        assert_eq!(
            store
                .search(&f, &[Uuid::from_u128(1)], 100)
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
                record(Uuid::from_u128(1), "first", 1_000, "none"),
                record(Uuid::from_u128(2), "second", 2_000, "none"),
                record(Uuid::from_u128(3), "third", 3_000, "none"),
            ])
            .await;
        let at = |ms: i64| DateTime::<Utc>::from_timestamp_millis(ms).unwrap();

        // since only: keep 2_000 and 3_000.
        let f = EventFilter {
            since: Some(at(1_500)),
            ..EventFilter::default()
        };
        assert_eq!(store.search(&f, &[], 100).await.unwrap().len(), 2);

        // until only: keep 1_000 and 2_000.
        let f = EventFilter {
            until: Some(at(2_500)),
            ..EventFilter::default()
        };
        assert_eq!(store.search(&f, &[], 100).await.unwrap().len(), 2);

        // both bounds: only the 2_000 record falls inside [1_500, 2_500].
        let f = EventFilter {
            since: Some(at(1_500)),
            until: Some(at(2_500)),
            ..EventFilter::default()
        };
        let rows = store.search(&f, &[], 100).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message, "second");
    }

    #[tokio::test]
    async fn in_memory_regex_search_matches_message() {
        let store = InMemoryLogStore::default();
        store
            .ingest_batch(&[
                record(Uuid::from_u128(1), "link down ge-0/0/1", 1_000, "fired"),
                record(Uuid::from_u128(2), "config changed", 2_000, "none"),
                record(Uuid::from_u128(3), "link up ge-0/0/1", 3_000, "cleared"),
            ])
            .await;
        let f = EventFilter {
            search: Some("^link (up|down)".into()),
            regex: true,
            ..EventFilter::default()
        };
        assert_eq!(store.search(&f, &[], 100).await.unwrap().len(), 2);
    }
}
