// SPDX-License-Identifier: AGPL-3.0-only
//! The flow store seam: where edge-aggregated flow records are written and queried (ADR-031).
//!
//! [`FlowStore`] abstracts flow storage so the bus consumer/writer and the flow API don't care
//! whether they talk to ClickHouse ([`ChStore`], live) or an in-memory fake ([`InMemoryFlowStore`],
//! tests). ClickHouse is the **5th store** added to ADR-004's split (after VictoriaLogs, ADR-024's
//! 4th data class) — a *loss-tolerant* tier
//! (ADR-017): operator-set TTL (30 days by default), single-node MVP, no must-never-lose
//! guarantee. It exists because the flow
//! tuple `(src ip × dst ip × src port × dst port × proto)` is extreme cardinality and must never
//! reach VictoriaMetrics (CLAUDE.md §7.1); ClickHouse's column store + TTL + materialized-view
//! rollups keep that cardinality contained.
//!
//! Writes use ClickHouse's HTTP interface with `JSONEachRow`; reads compile a typed [`FlowQuery`]
//! into SQL. Only validated typed values (a `Uuid` node id, integer timestamps/limits) are ever
//! interpolated — never device-supplied strings (injection discipline, mirroring `store.rs`'s
//! `promql_label_escape` and `logstore.rs`'s `logsql_quote`). Human node names never enter the
//! store: the API resolves them to `node_id`s in PostgreSQL and filters here by id (query-time join,
//! ADR-011).

use std::net::IpAddr;
use std::sync::atomic::{AtomicU32, Ordering};
#[cfg(test)]
use std::sync::Mutex;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

/// Default flow retention in days (ADR-031). Seeded from `YAGRA_FLOW_RETENTION_DAYS` on first boot,
/// operator-editable thereafter — the policy table is [`crate::retention`] (ADR-040).
pub use crate::retention::DEFAULT_FLOW_DAYS as DEFAULT_FLOW_RETENTION_DAYS;

/// Default retention for ClickHouse's **own** system log tables (ADR-031 Increment 4).
///
/// Not operator-editable from the UI and deliberately not in the [`crate::retention`] policy table:
/// that table is about *Yagra's* data, and this is about the store's self-telemetry. One env var
/// (`YAGRA_CLICKHOUSE_SYSTEM_LOG_RETENTION_DAYS`) is the whole surface.
pub const DEFAULT_SYSTEM_LOG_RETENTION_DAYS: u32 = 7;

/// One flow row to insert (a poller's per-bucket top-N record, with `node_id` resolved by core).
#[derive(Debug, Clone)]
pub struct FlowRow {
    /// Node the exporter maps to.
    pub node_id: Uuid,
    /// Bucket start, Unix ms (stored at second precision).
    pub ts_unix_ms: i64,
    /// Exporting device address.
    pub exporter_ip: IpAddr,
    /// Ingress ifIndex (0 = unknown).
    pub if_index: u32,
    /// Source / destination addresses and ports.
    pub src_ip: IpAddr,
    /// Destination address.
    pub dst_ip: IpAddr,
    /// Source port.
    pub src_port: u16,
    /// Destination port.
    pub dst_port: u16,
    /// IP protocol number.
    pub proto: u8,
    /// ToS / DSCP byte.
    pub tos: u8,
    /// Source AS (0 = unknown; export-provided or enriched from the IP→ASN table).
    pub src_as: u32,
    /// Destination AS (0 = unknown).
    pub dst_as: u32,
    /// Bytes over the bucket.
    pub bytes: u64,
    /// Packets over the bucket.
    pub packets: u64,
    /// Raw flow records folded into this row.
    pub flows: u32,
}

/// A top-N flow query: an optional node scope, a time window, a result cap, and optional drill-down
/// filters. Filters are strongly typed (never raw strings) so nothing device-supplied is
/// interpolated.
///
/// ⚠️ No longer `Copy` — the filters became `Vec`s in ADR-053 Inc.8. Callers that used to rely on an
/// implicit copy now `clone()`, which is what a fan-out of six queries over one filter set wants
/// anyway.
#[derive(Debug, Clone)]
pub struct FlowQuery {
    /// Node whose flows to query; `None` aggregates across every exporter (fleet-wide).
    pub node_id: Option<Uuid>,
    /// Window start (Unix ms, inclusive).
    pub from_unix_ms: i64,
    /// Window end (Unix ms, inclusive).
    pub to_unix_ms: i64,
    /// Max rows to return (clamped).
    pub limit: u32,
    // The four drill-down filters. Empty = no filter; several values in one = a disjunction
    // (`IN (…)`); values across two = a conjunction. ADR-053 Inc.8 widened these from `Option<T>`,
    // because a multi-select control over a single-valued query is the failure `EnumFilterSpec.single`
    // used to guard: three boxes ticked, one value sent, rows missing with nothing saying so.
    //
    // ⚠️ They are typed integers and `IpAddr`, not strings, and [`flow_filters_sql`] interpolates
    // them directly. That is what keeps the interpolation safe — see its doc comment before changing
    // any of these to `String`.
    /// IP protocols to include (empty = every protocol).
    pub proto: Vec<u8>,
    /// Destination ports to include (empty = every port).
    pub dst_port: Vec<u16>,
    /// Peers to include — rows where one of these is the source **or** the destination.
    pub peer: Vec<IpAddr>,
    /// ASNs to include — rows where one of these is the source or destination AS (0 = unknown).
    pub asn: Vec<u32>,
}

/// Which AS side a top-AS query aggregates on. A typed enum (never interpolated raw) so only a
/// fixed column name reaches SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsDir {
    /// Source AS (`src_as`).
    Src,
    /// Destination AS (`dst_as`).
    Dst,
}

impl AsDir {
    /// The ClickHouse column this direction selects — a fixed identifier, safe to interpolate.
    fn column(self) -> &'static str {
        match self {
            AsDir::Src => "src_as",
            AsDir::Dst => "dst_as",
        }
    }
}

/// A flow trend query: one node and a time window (5-minute rollup granularity). The rollup carries
/// only `proto`, so of the fact-table filters only a protocol filter can apply to the trend.
#[derive(Debug, Clone)]
pub struct FlowSeriesQuery {
    /// Node whose trend to query; `None` aggregates across every exporter (fleet-wide).
    pub node_id: Option<Uuid>,
    /// Window start (Unix ms, inclusive).
    pub from_unix_ms: i64,
    /// Window end (Unix ms, inclusive).
    pub to_unix_ms: i64,
    /// IP protocols to include (empty = every protocol) — the only fact-table filter the rollup can
    /// answer, which is why the Flow tab tells the operator that a port/peer/AS drill-down does not
    /// narrow the chart.
    pub proto: Vec<u8>,
}

/// A top-talker: one host address with summed traffic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct FlowTalker {
    /// Host address (v4 or v6, normalized from ClickHouse's v4-mapped form).
    pub addr: String,
    /// Bytes.
    pub bytes: u64,
    /// Packets.
    pub packets: u64,
    /// Distinct flows.
    pub flows: u64,
}

/// A conversation: a src→dst pair with summed traffic. `src_asn`/`dst_asn` are the stored
/// per-flow AS numbers (0 = unknown); the `*_as_name` fields are resolved from the IP→ASN table
/// at the API layer (the store leaves them `None`), mirroring [`FlowAsAgg`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct FlowConversation {
    /// Source address.
    pub src: String,
    /// Destination address.
    pub dst: String,
    /// Source autonomous-system number (0 = unknown).
    pub src_asn: u32,
    /// Destination autonomous-system number (0 = unknown).
    pub dst_asn: u32,
    /// Source AS organization name, if resolvable (filled at the API layer).
    pub src_as_name: Option<String>,
    /// Destination AS organization name, if resolvable (filled at the API layer).
    pub dst_as_name: Option<String>,
    /// Bytes.
    pub bytes: u64,
    /// Packets.
    pub packets: u64,
    /// Distinct flows.
    pub flows: u64,
}

/// A destination-port aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct FlowPortAgg {
    /// Destination port.
    pub port: u16,
    /// Bytes.
    pub bytes: u64,
    /// Packets.
    pub packets: u64,
    /// Distinct flows.
    pub flows: u64,
}

/// A protocol aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct FlowProtoAgg {
    /// IP protocol number.
    pub proto: u8,
    /// Bytes.
    pub bytes: u64,
    /// Packets.
    pub packets: u64,
    /// Distinct flows.
    pub flows: u64,
}

/// An autonomous-system aggregate. `asn = 0` means unknown (the UI labels it accordingly). `name`
/// is resolved from the IP→ASN table at the API layer (the store leaves it `None`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct FlowAsAgg {
    /// Autonomous-system number (0 = unknown).
    pub asn: u32,
    /// AS organization name, if resolvable.
    pub name: Option<String>,
    /// Bytes.
    pub bytes: u64,
    /// Packets.
    pub packets: u64,
    /// Distinct flows.
    pub flows: u64,
}

/// A per-source fan-out row: how many distinct destinations / destination ports one source address
/// touched in the window. The signal for horizontal (many hosts) / vertical (many ports) scans and
/// worm spread — the input to the Troubleshoot `flow_scan` analysis and the `flow_fanout` MCP tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FlowFanout {
    /// Source address.
    pub src: String,
    /// Distinct destination addresses contacted.
    pub distinct_dst: u64,
    /// Distinct destination ports contacted.
    pub distinct_ports: u64,
    /// Distinct flows.
    pub flows: u64,
    /// Bytes.
    pub bytes: u64,
}

/// A trend point: bytes/packets for one protocol at one 5-minute bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct FlowPoint {
    /// Bucket start, Unix ms.
    pub ts_unix_ms: i64,
    /// IP protocol number.
    pub proto: u8,
    /// Bytes.
    pub bytes: u64,
    /// Packets.
    pub packets: u64,
}

/// The flow persistence + query seam (ADR-031). See the module docs.
#[async_trait]
pub trait FlowStore: Send + Sync {
    /// Liveness probe for the system-health endpoint. Defaults to `true` (the fake is always up);
    /// [`ChStore`] overrides it to ping ClickHouse.
    async fn healthy(&self) -> bool {
        true
    }
    /// Create the flow tables / rollup MV / TTL if absent (idempotent). Run once at startup.
    async fn ensure_schema(&self) -> anyhow::Result<()>;
    /// Apply an operator's retention change to the store (ADR-040).
    ///
    /// This exists because `ensure_schema` cannot do it: its statements are
    /// `CREATE TABLE IF NOT EXISTS`, which is a no-op once the tables exist, so the TTL an existing
    /// deployment runs with is whatever it was created with — for years, `YAGRA_FLOW_RETENTION_DAYS`
    /// silently did nothing on any volume that was not brand new. `ALTER TABLE … MODIFY TTL` is not
    /// the nicer option here, it is the only one.
    ///
    /// Defaults to a no-op so the in-memory fake (which has no TTL) need not pretend to have one.
    async fn set_retention_days(&self, _days: u32) -> anyhow::Result<()> {
        Ok(())
    }
    /// Bound ClickHouse's **own** system log tables to `days` (ADR-031 Increment 4). `0` = leave
    /// `system.*` untouched.
    ///
    /// Stock ClickHouse gives `text_log` / `trace_log` / `metric_log` / `asynchronous_metric_log`
    /// and friends **no TTL at all**, and Yagra ships it with stock defaults. Measured on one
    /// deployment after a month: 49 MiB of flow data against 2.3 GiB / ~693M rows of self-
    /// telemetry, a third of a core burned merging it, and the top log producers were the merges of
    /// those very tables. It is a feedback loop, and nothing bounds it.
    ///
    /// 🚨 **A config-file `<ttl>` does not fix an existing deployment**, for exactly the reason
    /// [`Self::set_retention_days`] exists: ClickHouse applies it when it *creates* the table, so
    /// every volume that is not brand new keeps the TTL it was born with — which here is none. The
    /// shipped `config.d` and this `ALTER` are not alternatives; they cover different deployments.
    ///
    /// Defaults to a no-op so the in-memory fake need not pretend to own a ClickHouse.
    async fn bound_system_logs(&self, _days: u32) -> anyhow::Result<()> {
        Ok(())
    }
    /// Insert a batch of flow rows (best-effort tier — a store hiccup is logged, never fatal).
    async fn insert_batch(&self, rows: &[FlowRow]) -> anyhow::Result<()>;
    /// Top source hosts by bytes for a node/window.
    async fn top_talkers(&self, q: &FlowQuery) -> anyhow::Result<Vec<FlowTalker>>;
    /// Top src→dst conversations by bytes.
    async fn top_conversations(&self, q: &FlowQuery) -> anyhow::Result<Vec<FlowConversation>>;
    /// Top destination ports by bytes.
    async fn top_ports(&self, q: &FlowQuery) -> anyhow::Result<Vec<FlowPortAgg>>;
    /// Traffic by IP protocol.
    async fn top_protocols(&self, q: &FlowQuery) -> anyhow::Result<Vec<FlowProtoAgg>>;
    /// Top autonomous systems by bytes, on the `dir` side (`asn` numbers only; names filled by the
    /// API). `asn = 0` (unknown) is included.
    async fn top_as(&self, q: &FlowQuery, dir: AsDir) -> anyhow::Result<Vec<FlowAsAgg>>;
    /// Bytes/packets over time, per protocol, at 5-minute granularity (from the rollup MV).
    async fn series(&self, q: &FlowSeriesQuery) -> anyhow::Result<Vec<FlowPoint>>;
    /// Per-source fan-out (distinct destinations / destination ports), highest fan-out first —
    /// the scan/worm signal (`flow_scan`, `flow_fanout`).
    async fn fanout_by_src(&self, q: &FlowQuery) -> anyhow::Result<Vec<FlowFanout>>;
}

// ─── Helpers ────────────────────────────────────────────────────────────────────────

/// Render an address for a ClickHouse `IPv6` column: v4 is stored v4-mapped so one column holds
/// both families (honoring the IPv4/IPv6 gotcha).
fn ip_to_ch(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_ipv6_mapped().to_string(),
        IpAddr::V6(v6) => v6.to_string(),
    }
}

/// Normalize a ClickHouse-returned IP string: a v4-mapped v6 (`::ffff:a.b.c.d`) becomes dotted v4.
fn normalize_ch_ip(s: &str) -> String {
    match s.parse::<IpAddr>() {
        Ok(IpAddr::V6(v6)) => v6
            .to_ipv4_mapped()
            .map_or_else(|| v6.to_string(), |v4| v4.to_string()),
        Ok(other) => other.to_string(),
        Err(_) => s.to_owned(),
    }
}

/// Read a numeric field tolerant of ClickHouse's 64-bit-int-as-string JSON encoding.
fn j_u64(v: &Value, k: &str) -> u64 {
    match v.get(k) {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

fn j_i64(v: &Value, k: &str) -> i64 {
    match v.get(k) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

fn j_str(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Clamp a window to whole seconds `[from, to]` for interpolation into `toDateTime(...)`.
fn window_secs(from_unix_ms: i64, to_unix_ms: i64) -> (i64, i64) {
    (from_unix_ms.div_euclid(1000), to_unix_ms.div_euclid(1000))
}

/// The node-scope `WHERE` fragment (with a trailing ` AND ` so it prefixes the time bound). A
/// `Some(id)` scopes to one exporter's node; `None` is fleet-wide (every exporter) and emits
/// nothing, so the query aggregates across all nodes. Only a typed [`Uuid`] is interpolated.
fn node_scope_sql(node_id: Option<Uuid>) -> String {
    match node_id {
        Some(id) => format!("node_id = '{id}' AND "),
        None => String::new(),
    }
}

/// Build the optional fact-table filter fragment (`AND …`) for a [`FlowQuery`].
///
/// **This is string interpolation, not a bind, and that is safe for exactly one reason: every value
/// here has already been parsed into a type that cannot carry SQL** — `proto`/`dst_port`/`asn` are
/// integers and `peer` is a validated [`IpAddr`] rendered via [`ip_to_ch`]. ADR-053 Inc.8 widened
/// these to sets, and the property survives *because the set is a `Vec<u16>`, never a `Vec<String>`*.
/// The API edge (`api/flow.rs::flow_query_params`) is where a token becomes a number or a 400.
/// **If you ever make one of these a `String`, this function stops being safe.**
///
/// Each set is `IN (…)` — a disjunction within a column, ANDed across columns, which is what the
/// four drill-downs mean together ("TCP or UDP, on port 443, with this peer").
fn flow_filters_sql(q: &FlowQuery) -> String {
    let mut s = String::new();
    if !q.proto.is_empty() {
        s.push_str(&format!(" AND proto IN ({})", join_nums(&q.proto)));
    }
    if !q.dst_port.is_empty() {
        s.push_str(&format!(" AND dst_port IN ({})", join_nums(&q.dst_port)));
    }
    if !q.peer.is_empty() {
        let ips = q
            .peer
            .iter()
            .map(|p| format!("'{}'", ip_to_ch(*p)))
            .collect::<Vec<_>>()
            .join(",");
        s.push_str(&format!(" AND (src_ip IN ({ips}) OR dst_ip IN ({ips}))"));
    }
    if !q.asn.is_empty() {
        let asns = join_nums(&q.asn);
        s.push_str(&format!(" AND (src_as IN ({asns}) OR dst_as IN ({asns}))"));
    }
    s
}

/// Render a set of integers as a SQL list. Generic over the integer type so the three numeric sets
/// share one implementation rather than three `map(|x| x.to_string())` chains that could each grow a
/// different quoting mistake.
fn join_nums<T: std::fmt::Display>(vals: &[T]) -> String {
    vals.iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Build the top-conversations SQL. The AS aggregates are aliased to `src_asn`/`dst_asn` — **never**
/// the column names `src_as`/`dst_as`. Aliasing `any(src_as) AS src_as` would shadow the column, and
/// the asn drill-down filter (`flow_filters_sql` emits `AND (src_as = N OR dst_as = N)`) would then
/// bind that reference to the aggregate alias; ClickHouse rejects an aggregate in `WHERE`
/// (`ILLEGAL_AGGREGATION`), 500-ing the whole query. A distinct alias keeps the filter bound to the
/// column so the AS drill-down actually narrows the conversations (and thus the Sankey).
fn conversations_sql(q: &FlowQuery) -> String {
    let (from, to) = window_secs(q.from_unix_ms, q.to_unix_ms);
    let limit = q.limit.clamp(1, 1000);
    let filters = flow_filters_sql(q);
    let scope = node_scope_sql(q.node_id);
    // `any(src_as)`/`any(dst_as)` keep one row per (src,dst): grouping by AS too would split a
    // conversation if its stored AS ever varied. AS-per-IP is effectively stable, so `any` is safe.
    format!(
        "SELECT src_ip AS s, dst_ip AS d, any(src_as) AS src_asn, any(dst_as) AS dst_asn, sum(bytes) AS bytes, sum(packets) AS packets, sum(flows) AS flows
         FROM flow_records
         WHERE {scope}ts >= toDateTime({from}) AND ts <= toDateTime({to}){filters}
         GROUP BY src_ip, dst_ip ORDER BY bytes DESC LIMIT {limit} FORMAT JSONEachRow"
    )
}

/// The `ALTER` that moves a flow table's TTL. `table` is never operator input — callers pass a
/// name they read back from the `IN ('flow_records', 'flow_rollup_5m')` list above.
fn ttl_modify_sql(table: &str, days: u32) -> String {
    format!("ALTER TABLE {table} MODIFY TTL ts + INTERVAL {days} DAY DELETE")
}

/// The system log tables to bound, discovered rather than listed (ADR-031 Increment 4).
///
/// Discovered, because a hardcoded list is a mirror of ClickHouse's release notes: 24.8 ships nine
/// of these and a later version will ship a tenth, which a list would silently leave unbounded —
/// the exact failure this increment exists to fix. Three predicates make "discovered" safe:
/// `database = 'system'` keeps it off Yagra's own tables and the operator's, the name pattern is
/// ClickHouse's own naming rule for them, and the join on an `event_date` column guarantees the TTL
/// expression below is even valid for the table.
///
/// 🚨 **The `_<N>` suffix in the pattern is not defensive, it is the common case on an upgrade.**
/// When a system log's declared TTL stops matching the table on disk, ClickHouse does not alter it:
/// it **renames the old table to `<name>_0` and creates a fresh one**. Measured on the first real
/// deployment of this increment — the nine live tables came back bounded and green while
/// `text_log_0`, `trace_log_0`, `metric_log_0` and four more sat beside them holding the entire
/// 2.3 GiB with no TTL and no writer, invisible to a pattern that only matched `_log`. Growth had
/// stopped and nothing had been reclaimed, which is the failure mode that looks exactly like
/// success.
///
/// The `name` values that come back are ClickHouse's, never an operator's — the same property that
/// lets [`ttl_modify_sql`] interpolate a table name without escaping it.
const SYSTEM_LOG_TABLES_SQL: &str = "SELECT t.name AS name, t.engine_full AS engine_full \
     FROM system.tables AS t \
     INNER JOIN (SELECT table FROM system.columns \
                 WHERE database = 'system' AND name = 'event_date' GROUP BY table) AS c \
       ON c.table = t.name \
     WHERE t.database = 'system' AND match(t.name, '_log(_[0-9]+)?$') \
       AND position(t.engine, 'MergeTree') > 0 \
     ORDER BY t.name FORMAT JSONEachRow";

/// The `ALTER` that bounds one ClickHouse system log table. `table` comes from
/// [`SYSTEM_LOG_TABLES_SQL`], i.e. from ClickHouse, never from operator input.
///
/// `event_date` rather than `event_time`: it is the partitioning key of every system log, so
/// expiry drops whole partitions instead of rewriting parts row by row.
fn system_log_ttl_sql(table: &str, days: u32) -> String {
    format!("ALTER TABLE system.{table} MODIFY TTL event_date + INTERVAL {days} DAY DELETE")
}

/// Whether a `system.tables.engine_full` string already declares a `days`-day TTL.
///
/// Two spellings have to be understood: `INTERVAL 30 DAY`, which is what we write, and
/// `toIntervalDay(30)`, which is how ClickHouse normalizes it back. The number is parsed and
/// compared, never substring-matched — a prefix match would read `toIntervalDay(3)` as satisfying a
/// request for 30 days and leave the TTL permanently wrong, which is the whole reason this is a
/// named function with tests rather than an inline `contains`.
fn engine_full_declares_ttl_days(engine_full: &str, days: u32) -> bool {
    ttl_days_of(engine_full) == Some(days)
}

/// The TTL in whole days declared by an `engine_full` string, if it declares one in days.
fn ttl_days_of(engine_full: &str) -> Option<u32> {
    let lower = engine_full.to_ascii_lowercase();
    if let Some(idx) = lower.find("tointervalday(") {
        return leading_u32(&lower[idx + "tointervalday(".len()..]);
    }
    if let Some(idx) = lower.find("interval ") {
        let rest = lower[idx + "interval ".len()..].trim_start();
        let n = leading_u32(rest)?;
        let after = rest
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .trim_start();
        // `INTERVAL 30 HOUR` is a TTL, just not one measured in days — do not claim it matches.
        return after.starts_with("day").then_some(n);
    }
    None
}

/// Leading decimal digits of `s` as a `u32`, or `None` when it does not start with a digit.
fn leading_u32(s: &str) -> Option<u32> {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

// ─── ClickHouse (live) ────────────────────────────────────────────────────────────────

/// A [`FlowStore`] backed by ClickHouse over its HTTP interface.
pub struct ChStore {
    http: reqwest::Client,
    base: String,
    /// Atomic because the store is shared behind an `Arc<dyn FlowStore>` and an operator can change
    /// the retention at runtime through `set_retention_days` (ADR-040).
    retention_days: AtomicU32,
}

impl ChStore {
    /// Point at ClickHouse's HTTP base URL (e.g. `http://clickhouse:8123`) with an explicit
    /// retention (days).
    #[must_use]
    pub fn with_retention(base: impl Into<String>, retention_days: u32) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        Self {
            http,
            base: base.into(),
            retention_days: AtomicU32::new(retention_days.max(1)),
        }
    }

    /// Bring both flow tables' declared TTL in line with `days`, issuing an `ALTER` **only where it
    /// actually differs**.
    ///
    /// The conditional is the load-bearing part. `ALTER … MODIFY TTL` runs with
    /// `materialize_ttl_after_modify = 1` by default, which schedules a mutation across every
    /// existing part; firing it unconditionally would re-mutate the whole flow table on every core
    /// start. Materialization stays on for the case where it *does* fire — an operator who lowers
    /// retention expects the old rows to actually go.
    async fn sync_ttl(&self, days: u32) -> anyhow::Result<()> {
        let rows = self
            .query_json(
                "SELECT name, engine_full FROM system.tables \
                 WHERE database = currentDatabase() \
                   AND name IN ('flow_records', 'flow_rollup_5m') FORMAT JSONEachRow",
            )
            .await?;
        for row in rows {
            let Some(name) = row.get("name").and_then(Value::as_str) else {
                continue;
            };
            let engine_full = row
                .get("engine_full")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if engine_full_declares_ttl_days(engine_full, days) {
                tracing::info!(
                    table = name,
                    retention_days = days,
                    "flow TTL already current"
                );
                continue;
            }
            // Loud on purpose: a shrink schedules a mutation that deletes rows, and the operator
            // needs to be able to find the reason in the log afterwards.
            tracing::warn!(
                table = name,
                to_days = days,
                engine = engine_full,
                "altering flow TTL"
            );
            self.exec(&ttl_modify_sql(name, days)).await?;
        }
        Ok(())
    }

    /// POST a statement with no row body (DDL / plain query); returns the response text.
    async fn exec(&self, sql: &str) -> anyhow::Result<String> {
        let resp = self
            .http
            .post(&self.base)
            .body(sql.to_owned())
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("ClickHouse request failed: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("ClickHouse returned {status}: {}", text.trim());
        }
        Ok(text)
    }

    /// Run a `SELECT ... FORMAT JSONEachRow` and parse each line into a JSON object.
    async fn query_json(&self, sql: &str) -> anyhow::Result<Vec<Value>> {
        let body = self.exec(sql).await?;
        Ok(body
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .collect())
    }
}

#[async_trait]
impl FlowStore for ChStore {
    async fn healthy(&self) -> bool {
        match self.http.get(format!("{}/ping", self.base)).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(e) => {
                tracing::warn!(error = %e, "ClickHouse health check failed");
                false
            }
        }
    }

    async fn ensure_schema(&self) -> anyhow::Result<()> {
        let ttl = self.retention_days.load(Ordering::Relaxed);
        // Fact table: one row per poller-aggregated top-N flow, partitioned by day, TTL-expired.
        self.exec(&format!(
            "CREATE TABLE IF NOT EXISTS flow_records (
                ts DateTime CODEC(DoubleDelta),
                node_id UUID,
                exporter_ip IPv6,
                if_index UInt32,
                src_ip IPv6,
                dst_ip IPv6,
                src_port UInt16,
                dst_port UInt16,
                proto UInt8,
                tos UInt8 DEFAULT 0,
                src_as UInt32 DEFAULT 0,
                dst_as UInt32 DEFAULT 0,
                bytes UInt64,
                packets UInt64,
                flows UInt32
            ) ENGINE = MergeTree
            PARTITION BY toDate(ts)
            ORDER BY (node_id, ts, proto, dst_port)
            TTL ts + INTERVAL {ttl} DAY DELETE"
        ))
        .await?;
        // Light summary rollup (per node / 5-min / proto) for the trend graph.
        self.exec(&format!(
            "CREATE TABLE IF NOT EXISTS flow_rollup_5m (
                ts DateTime,
                node_id UUID,
                proto UInt8,
                bytes UInt64,
                packets UInt64,
                flows UInt64
            ) ENGINE = SummingMergeTree
            PARTITION BY toDate(ts)
            ORDER BY (node_id, ts, proto)
            TTL ts + INTERVAL {ttl} DAY DELETE"
        ))
        .await?;
        self.exec(
            "CREATE MATERIALIZED VIEW IF NOT EXISTS flow_rollup_5m_mv TO flow_rollup_5m AS
                SELECT toStartOfFiveMinutes(ts) AS ts, node_id, proto,
                       sum(bytes) AS bytes, sum(packets) AS packets, sum(flows) AS flows
                FROM flow_records GROUP BY ts, node_id, proto",
        )
        .await?;
        // The CREATEs above are no-ops on an existing deployment, so they cannot carry a changed
        // retention onto tables that already exist. Reconcile the declared TTL explicitly, which is
        // also what makes the setting survive a restart after an operator edits it (ADR-040).
        self.sync_ttl(ttl).await?;
        tracing::info!(retention_days = ttl, "ClickHouse flow schema ensured");
        Ok(())
    }

    async fn set_retention_days(&self, days: u32) -> anyhow::Result<()> {
        let days = days.max(1);
        self.sync_ttl(days).await?;
        // Stored only after the ALTER succeeds, so a failed change does not leave the process
        // believing a TTL that ClickHouse never accepted.
        self.retention_days.store(days, Ordering::Relaxed);
        Ok(())
    }

    async fn bound_system_logs(&self, days: u32) -> anyhow::Result<()> {
        if days == 0 {
            tracing::info!("ClickHouse system-log bounding disabled by configuration");
            return Ok(());
        }
        let rows = self.query_json(SYSTEM_LOG_TABLES_SQL).await?;
        if rows.is_empty() {
            // Not an error: a ClickHouse with every system log switched off is a legitimate — and
            // in fact ideal — shape for this deployment to be in.
            tracing::info!("no ClickHouse system log tables to bound");
            return Ok(());
        }
        for row in rows {
            let Some(name) = row.get("name").and_then(Value::as_str) else {
                continue;
            };
            let engine_full = row
                .get("engine_full")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if engine_full_declares_ttl_days(engine_full, days) {
                continue;
            }
            // ⚠️ First application on a long-lived deployment schedules a real mutation — that is
            // the point (it is what reclaims the disk), but it is also work, so say so.
            tracing::warn!(
                table = name,
                to_days = days,
                "bounding a ClickHouse system log table that had no matching TTL"
            );
            // Per-table tolerance on purpose: a managed ClickHouse may refuse ALTER on some of
            // `system.*`, and one refusal must not skip the tables that would have accepted it.
            if let Err(e) = self.exec(&system_log_ttl_sql(name, days)).await {
                tracing::warn!(table = name, error = %e, "could not bound system log TTL");
            }
        }
        Ok(())
    }

    async fn insert_batch(&self, rows: &[FlowRow]) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut body = String::new();
        for r in rows {
            let obj = serde_json::json!({
                "ts": r.ts_unix_ms.div_euclid(1000),
                "node_id": r.node_id.to_string(),
                "exporter_ip": ip_to_ch(r.exporter_ip),
                "if_index": r.if_index,
                "src_ip": ip_to_ch(r.src_ip),
                "dst_ip": ip_to_ch(r.dst_ip),
                "src_port": r.src_port,
                "dst_port": r.dst_port,
                "proto": r.proto,
                "tos": r.tos,
                "src_as": r.src_as,
                "dst_as": r.dst_as,
                "bytes": r.bytes,
                "packets": r.packets,
                "flows": r.flows,
            });
            if let Ok(line) = serde_json::to_string(&obj) {
                body.push_str(&line);
                body.push('\n');
            }
        }
        let resp = self
            .http
            .post(&self.base)
            .query(&[("query", "INSERT INTO flow_records FORMAT JSONEachRow")])
            .body(body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("ClickHouse insert request failed: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("ClickHouse insert returned {status}: {}", text.trim());
        }
        Ok(())
    }

    async fn top_talkers(&self, q: &FlowQuery) -> anyhow::Result<Vec<FlowTalker>> {
        let (from, to) = window_secs(q.from_unix_ms, q.to_unix_ms);
        let limit = q.limit.clamp(1, 1000);
        let filters = flow_filters_sql(q);
        let scope = node_scope_sql(q.node_id);
        let sql = format!(
            "SELECT src_ip AS k, sum(bytes) AS bytes, sum(packets) AS packets, sum(flows) AS flows
             FROM flow_records
             WHERE {scope}ts >= toDateTime({from}) AND ts <= toDateTime({to}){filters}
             GROUP BY src_ip ORDER BY bytes DESC LIMIT {limit} FORMAT JSONEachRow"
        );
        Ok(self
            .query_json(&sql)
            .await?
            .iter()
            .map(|v| FlowTalker {
                addr: normalize_ch_ip(&j_str(v, "k")),
                bytes: j_u64(v, "bytes"),
                packets: j_u64(v, "packets"),
                flows: j_u64(v, "flows"),
            })
            .collect())
    }

    async fn top_conversations(&self, q: &FlowQuery) -> anyhow::Result<Vec<FlowConversation>> {
        let sql = conversations_sql(q);
        Ok(self
            .query_json(&sql)
            .await?
            .iter()
            .map(|v| FlowConversation {
                src: normalize_ch_ip(&j_str(v, "s")),
                dst: normalize_ch_ip(&j_str(v, "d")),
                src_asn: j_u64(v, "src_asn") as u32,
                dst_asn: j_u64(v, "dst_asn") as u32,
                src_as_name: None, // resolved by the API layer
                dst_as_name: None,
                bytes: j_u64(v, "bytes"),
                packets: j_u64(v, "packets"),
                flows: j_u64(v, "flows"),
            })
            .collect())
    }

    async fn top_ports(&self, q: &FlowQuery) -> anyhow::Result<Vec<FlowPortAgg>> {
        let (from, to) = window_secs(q.from_unix_ms, q.to_unix_ms);
        let limit = q.limit.clamp(1, 1000);
        let filters = flow_filters_sql(q);
        let scope = node_scope_sql(q.node_id);
        let sql = format!(
            "SELECT dst_port AS k, sum(bytes) AS bytes, sum(packets) AS packets, sum(flows) AS flows
             FROM flow_records
             WHERE {scope}ts >= toDateTime({from}) AND ts <= toDateTime({to}){filters}
             GROUP BY dst_port ORDER BY bytes DESC LIMIT {limit} FORMAT JSONEachRow"
        );
        Ok(self
            .query_json(&sql)
            .await?
            .iter()
            .map(|v| FlowPortAgg {
                port: j_u64(v, "k") as u16,
                bytes: j_u64(v, "bytes"),
                packets: j_u64(v, "packets"),
                flows: j_u64(v, "flows"),
            })
            .collect())
    }

    async fn top_protocols(&self, q: &FlowQuery) -> anyhow::Result<Vec<FlowProtoAgg>> {
        let (from, to) = window_secs(q.from_unix_ms, q.to_unix_ms);
        let limit = q.limit.clamp(1, 256);
        let filters = flow_filters_sql(q);
        let scope = node_scope_sql(q.node_id);
        let sql = format!(
            "SELECT proto AS k, sum(bytes) AS bytes, sum(packets) AS packets, sum(flows) AS flows
             FROM flow_records
             WHERE {scope}ts >= toDateTime({from}) AND ts <= toDateTime({to}){filters}
             GROUP BY proto ORDER BY bytes DESC LIMIT {limit} FORMAT JSONEachRow"
        );
        Ok(self
            .query_json(&sql)
            .await?
            .iter()
            .map(|v| FlowProtoAgg {
                proto: j_u64(v, "k") as u8,
                bytes: j_u64(v, "bytes"),
                packets: j_u64(v, "packets"),
                flows: j_u64(v, "flows"),
            })
            .collect())
    }

    async fn top_as(&self, q: &FlowQuery, dir: AsDir) -> anyhow::Result<Vec<FlowAsAgg>> {
        let (from, to) = window_secs(q.from_unix_ms, q.to_unix_ms);
        let limit = q.limit.clamp(1, 1000);
        let col = dir.column(); // fixed identifier ("src_as" | "dst_as")
        let filters = flow_filters_sql(q);
        let scope = node_scope_sql(q.node_id);
        let sql = format!(
            "SELECT {col} AS asn, sum(bytes) AS bytes, sum(packets) AS packets, sum(flows) AS flows
             FROM flow_records
             WHERE {scope}ts >= toDateTime({from}) AND ts <= toDateTime({to}){filters}
             GROUP BY asn ORDER BY bytes DESC LIMIT {limit} FORMAT JSONEachRow"
        );
        Ok(self
            .query_json(&sql)
            .await?
            .iter()
            .map(|v| FlowAsAgg {
                asn: j_u64(v, "asn") as u32,
                name: None, // resolved by the API layer
                bytes: j_u64(v, "bytes"),
                packets: j_u64(v, "packets"),
                flows: j_u64(v, "flows"),
            })
            .collect())
    }

    async fn series(&self, q: &FlowSeriesQuery) -> anyhow::Result<Vec<FlowPoint>> {
        let (from, to) = window_secs(q.from_unix_ms, q.to_unix_ms);
        // The rollup carries only proto, so only a protocol filter can apply to the trend.
        let proto_filter = if q.proto.is_empty() {
            String::new()
        } else {
            format!(" AND proto IN ({})", join_nums(&q.proto))
        };
        let scope = node_scope_sql(q.node_id);
        let sql = format!(
            "SELECT toUnixTimestamp(ts) AS t, proto, sum(bytes) AS bytes, sum(packets) AS packets
             FROM flow_rollup_5m
             WHERE {scope}ts >= toDateTime({from}) AND ts <= toDateTime({to}){proto_filter}
             GROUP BY t, proto ORDER BY t ASC FORMAT JSONEachRow"
        );
        Ok(self
            .query_json(&sql)
            .await?
            .iter()
            .map(|v| FlowPoint {
                ts_unix_ms: j_i64(v, "t") * 1000,
                proto: j_u64(v, "proto") as u8,
                bytes: j_u64(v, "bytes"),
                packets: j_u64(v, "packets"),
            })
            .collect())
    }

    async fn fanout_by_src(&self, q: &FlowQuery) -> anyhow::Result<Vec<FlowFanout>> {
        let (from, to) = window_secs(q.from_unix_ms, q.to_unix_ms);
        let limit = q.limit.clamp(1, 1000);
        let filters = flow_filters_sql(q);
        let scope = node_scope_sql(q.node_id);
        // `uniqExact` is a distinct-count no existing aggregate exposes; ordered by destination
        // fan-out (the horizontal-scan signal). All interpolated values are typed (uuid/ints/IpAddr).
        let sql = format!(
            "SELECT src_ip AS k, uniqExact(dst_ip) AS d, uniqExact(dst_port) AS p, \
                    sum(flows) AS flows, sum(bytes) AS bytes
             FROM flow_records
             WHERE {scope}ts >= toDateTime({from}) AND ts <= toDateTime({to}){filters}
             GROUP BY src_ip ORDER BY d DESC LIMIT {limit} FORMAT JSONEachRow"
        );
        Ok(self
            .query_json(&sql)
            .await?
            .iter()
            .map(|v| FlowFanout {
                src: normalize_ch_ip(&j_str(v, "k")),
                distinct_dst: j_u64(v, "d"),
                distinct_ports: j_u64(v, "p"),
                flows: j_u64(v, "flows"),
                bytes: j_u64(v, "bytes"),
            })
            .collect())
    }
}

// ─── In-memory fake (tests) ───────────────────────────────────────────────────────────

/// An in-memory [`FlowStore`] for tests — models the query contract without a ClickHouse round-trip.
#[cfg(test)]
#[derive(Default)]
pub struct InMemoryFlowStore {
    rows: Mutex<Vec<FlowRow>>,
}

#[cfg(test)]
impl InMemoryFlowStore {
    /// Number of stored rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.lock().expect("flow fake mutex poisoned").len()
    }

    /// Whether the store holds no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The fake's mirror of [`flow_filters_sql`]. **An empty set means "no filter" and a non-empty
    /// one means membership — the same rule the SQL's `IN (…)` follows.** Getting the empty case
    /// backwards here would make the fake *narrower* than the engine and every test would still
    /// pass; getting the non-empty case wrong would make it wider, which is the direction that ships
    /// a bug (a fake that matched more than the real store is exactly how the event-search predicate
    /// went wrong once).
    fn in_window(&self, q: &FlowQuery) -> Vec<FlowRow> {
        /// `true` when the set does not narrow, or when the row's value is in it.
        fn allows<T: PartialEq>(set: &[T], value: &T) -> bool {
            set.is_empty() || set.contains(value)
        }
        self.rows
            .lock()
            .expect("flow fake mutex poisoned")
            .iter()
            .filter(|r| {
                q.node_id.is_none_or(|n| r.node_id == n)
                    && r.ts_unix_ms >= q.from_unix_ms
                    && r.ts_unix_ms <= q.to_unix_ms
                    && allows(&q.proto, &r.proto)
                    && allows(&q.dst_port, &r.dst_port)
                    // Peer and AS match on **either** end, so they cannot go through `allows`.
                    && (q.peer.is_empty()
                        || q.peer.contains(&r.src_ip)
                        || q.peer.contains(&r.dst_ip))
                    && (q.asn.is_empty() || q.asn.contains(&r.src_as) || q.asn.contains(&r.dst_as))
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
#[async_trait]
impl FlowStore for InMemoryFlowStore {
    async fn ensure_schema(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn insert_batch(&self, rows: &[FlowRow]) -> anyhow::Result<()> {
        self.rows
            .lock()
            .expect("flow fake mutex poisoned")
            .extend(rows.iter().cloned());
        Ok(())
    }

    async fn top_talkers(&self, q: &FlowQuery) -> anyhow::Result<Vec<FlowTalker>> {
        use std::collections::HashMap;
        let mut agg: HashMap<String, (u64, u64, u64)> = HashMap::new();
        for r in self.in_window(q) {
            let e = agg.entry(r.src_ip.to_string()).or_default();
            e.0 += r.bytes;
            e.1 += r.packets;
            e.2 += u64::from(r.flows);
        }
        let mut out: Vec<FlowTalker> = agg
            .into_iter()
            .map(|(addr, (bytes, packets, flows))| FlowTalker {
                addr,
                bytes,
                packets,
                flows,
            })
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.bytes));
        out.truncate(q.limit.clamp(1, 1000) as usize);
        Ok(out)
    }

    async fn top_conversations(&self, q: &FlowQuery) -> anyhow::Result<Vec<FlowConversation>> {
        use std::collections::HashMap;
        // Value tuple: (bytes, packets, flows, src_as, dst_as) — AS is any-of-window (last wins),
        // mirroring the store's `any(src_as)`/`any(dst_as)`.
        let mut agg: HashMap<(String, String), (u64, u64, u64, u32, u32)> = HashMap::new();
        for r in self.in_window(q) {
            let e = agg
                .entry((r.src_ip.to_string(), r.dst_ip.to_string()))
                .or_default();
            e.0 += r.bytes;
            e.1 += r.packets;
            e.2 += u64::from(r.flows);
            e.3 = r.src_as;
            e.4 = r.dst_as;
        }
        let mut out: Vec<FlowConversation> = agg
            .into_iter()
            .map(
                |((src, dst), (bytes, packets, flows, src_asn, dst_asn))| FlowConversation {
                    src,
                    dst,
                    src_asn,
                    dst_asn,
                    src_as_name: None,
                    dst_as_name: None,
                    bytes,
                    packets,
                    flows,
                },
            )
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.bytes));
        out.truncate(q.limit.clamp(1, 1000) as usize);
        Ok(out)
    }

    async fn top_ports(&self, q: &FlowQuery) -> anyhow::Result<Vec<FlowPortAgg>> {
        use std::collections::HashMap;
        let mut agg: HashMap<u16, (u64, u64, u64)> = HashMap::new();
        for r in self.in_window(q) {
            let e = agg.entry(r.dst_port).or_default();
            e.0 += r.bytes;
            e.1 += r.packets;
            e.2 += u64::from(r.flows);
        }
        let mut out: Vec<FlowPortAgg> = agg
            .into_iter()
            .map(|(port, (bytes, packets, flows))| FlowPortAgg {
                port,
                bytes,
                packets,
                flows,
            })
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.bytes));
        out.truncate(q.limit.clamp(1, 1000) as usize);
        Ok(out)
    }

    async fn top_protocols(&self, q: &FlowQuery) -> anyhow::Result<Vec<FlowProtoAgg>> {
        use std::collections::HashMap;
        let mut agg: HashMap<u8, (u64, u64, u64)> = HashMap::new();
        for r in self.in_window(q) {
            let e = agg.entry(r.proto).or_default();
            e.0 += r.bytes;
            e.1 += r.packets;
            e.2 += u64::from(r.flows);
        }
        let mut out: Vec<FlowProtoAgg> = agg
            .into_iter()
            .map(|(proto, (bytes, packets, flows))| FlowProtoAgg {
                proto,
                bytes,
                packets,
                flows,
            })
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.bytes));
        Ok(out)
    }

    async fn top_as(&self, q: &FlowQuery, dir: AsDir) -> anyhow::Result<Vec<FlowAsAgg>> {
        use std::collections::HashMap;
        let mut agg: HashMap<u32, (u64, u64, u64)> = HashMap::new();
        for r in self.in_window(q) {
            let asn = match dir {
                AsDir::Src => r.src_as,
                AsDir::Dst => r.dst_as,
            };
            let e = agg.entry(asn).or_default();
            e.0 += r.bytes;
            e.1 += r.packets;
            e.2 += u64::from(r.flows);
        }
        let mut out: Vec<FlowAsAgg> = agg
            .into_iter()
            .map(|(asn, (bytes, packets, flows))| FlowAsAgg {
                asn,
                name: None,
                bytes,
                packets,
                flows,
            })
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.bytes));
        out.truncate(q.limit.clamp(1, 1000) as usize);
        Ok(out)
    }

    async fn series(&self, q: &FlowSeriesQuery) -> anyhow::Result<Vec<FlowPoint>> {
        let fq = FlowQuery {
            node_id: q.node_id,
            from_unix_ms: q.from_unix_ms,
            to_unix_ms: q.to_unix_ms,
            limit: u32::MAX,
            proto: q.proto.clone(),
            dst_port: Vec::new(),
            peer: Vec::new(),
            asn: Vec::new(),
        };
        use std::collections::BTreeMap;
        let mut agg: BTreeMap<(i64, u8), (u64, u64)> = BTreeMap::new();
        for r in self.in_window(&fq) {
            // 5-minute bucket.
            let bucket = (r.ts_unix_ms / 1000).div_euclid(300) * 300;
            let e = agg.entry((bucket, r.proto)).or_default();
            e.0 += r.bytes;
            e.1 += r.packets;
        }
        Ok(agg
            .into_iter()
            .map(|((t, proto), (bytes, packets))| FlowPoint {
                ts_unix_ms: t * 1000,
                proto,
                bytes,
                packets,
            })
            .collect())
    }

    async fn fanout_by_src(&self, q: &FlowQuery) -> anyhow::Result<Vec<FlowFanout>> {
        use std::collections::{HashMap, HashSet};
        let mut agg: HashMap<String, (HashSet<String>, HashSet<u16>, u64, u64)> = HashMap::new();
        for r in self.in_window(q) {
            let e = agg.entry(r.src_ip.to_string()).or_default();
            e.0.insert(r.dst_ip.to_string());
            e.1.insert(r.dst_port);
            e.2 += u64::from(r.flows);
            e.3 += r.bytes;
        }
        let mut out: Vec<FlowFanout> = agg
            .into_iter()
            .map(|(src, (dsts, ports, flows, bytes))| FlowFanout {
                src,
                distinct_dst: dsts.len() as u64,
                distinct_ports: ports.len() as u64,
                flows,
                bytes,
            })
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.distinct_dst));
        out.truncate(q.limit.clamp(1, 1000) as usize);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn row(node: Uuid, src: &str, dst: &str, port: u16, proto: u8, bytes: u64, ts: i64) -> FlowRow {
        FlowRow {
            node_id: node,
            ts_unix_ms: ts,
            exporter_ip: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
            if_index: 2,
            src_ip: src.parse().unwrap(),
            dst_ip: dst.parse().unwrap(),
            src_port: 40000,
            dst_port: port,
            proto,
            tos: 0,
            src_as: 0,
            dst_as: 0,
            bytes,
            packets: bytes / 100,
            flows: 1,
        }
    }

    #[test]
    fn ttl_is_read_from_both_spellings() {
        // What ClickHouse hands back in system.tables.engine_full…
        let normalized = "MergeTree PARTITION BY toDate(ts) ORDER BY (node_id, ts) \
                          TTL ts + toIntervalDay(30) SETTINGS index_granularity = 8192";
        assert!(engine_full_declares_ttl_days(normalized, 30));
        // …and what we write ourselves.
        assert!(engine_full_declares_ttl_days(
            "MergeTree TTL ts + INTERVAL 30 DAY DELETE",
            30
        ));
    }

    /// The bug this helper exists to not have: a substring test would accept `toIntervalDay(3)` as
    /// a 30-day TTL, so the ALTER would never fire and the retention would stay wrong forever.
    #[test]
    fn a_shorter_ttl_is_not_a_prefix_match_for_a_longer_one() {
        let three = "MergeTree TTL ts + toIntervalDay(3) SETTINGS index_granularity = 8192";
        assert!(!engine_full_declares_ttl_days(three, 30));
        assert!(engine_full_declares_ttl_days(three, 3));
        assert!(!engine_full_declares_ttl_days(
            "MergeTree TTL ts + INTERVAL 3 DAY DELETE",
            30
        ));
    }

    #[test]
    fn a_table_with_no_day_ttl_never_matches() {
        assert_eq!(ttl_days_of("MergeTree ORDER BY (node_id, ts)"), None);
        // An hour-denominated TTL is a TTL, but not one measured in days.
        assert_eq!(ttl_days_of("MergeTree TTL ts + INTERVAL 30 HOUR"), None);
        assert!(!engine_full_declares_ttl_days("MergeTree", 30));
    }

    #[test]
    fn the_alter_names_the_table_and_the_window() {
        assert_eq!(
            ttl_modify_sql("flow_records", 7),
            "ALTER TABLE flow_records MODIFY TTL ts + INTERVAL 7 DAY DELETE"
        );
        // Round trip: what we emit is what the reader accepts, so a sync converges in one pass
        // instead of re-issuing the same ALTER on every startup.
        assert!(engine_full_declares_ttl_days(
            &ttl_modify_sql("flow_rollup_5m", 45),
            45
        ));
    }

    /// The system-log bounding must land on ClickHouse's logs and nothing else, and must converge.
    ///
    /// 🚨 The table names in that `ALTER` are interpolated unescaped, which is safe **only**
    /// because [`SYSTEM_LOG_TABLES_SQL`] is what produced them. Its three predicates are therefore
    /// load-bearing, not stylistic: drop `database = 'system'` and the statement reaches Yagra's
    /// own flow tables; drop the `event_date` join and it is emitted for tables where the
    /// expression does not compile; drop `endsWith(…, '_log')` and it reaches `system.parts` and
    /// friends. Each is pinned here because nothing else in the tree reads that string.
    #[test]
    fn the_system_log_alter_is_scoped_to_clickhouses_own_logs_and_converges() {
        assert_eq!(
            system_log_ttl_sql("text_log", 7),
            "ALTER TABLE system.text_log MODIFY TTL event_date + INTERVAL 7 DAY DELETE"
        );
        assert!(SYSTEM_LOG_TABLES_SQL.contains("t.database = 'system'"));
        // The archive suffix is load-bearing: ClickHouse renames a system log to `<name>_0` when
        // its declared TTL stops matching, so on the very upgrade that introduces a TTL the bytes
        // move to a table a plain `_log` match cannot see.
        assert!(SYSTEM_LOG_TABLES_SQL.contains("match(t.name, '_log(_[0-9]+)?$')"));
        assert!(SYSTEM_LOG_TABLES_SQL.contains("name = 'event_date'"));
        assert!(SYSTEM_LOG_TABLES_SQL.contains("MergeTree"));
        // Round trip, same reason as the flow tables above: what we write has to be what the skip
        // check reads back, or every core start re-mutates every system log table on the box.
        assert!(engine_full_declares_ttl_days(
            &system_log_ttl_sql("asynchronous_metric_log", 7),
            7
        ));
        assert!(!engine_full_declares_ttl_days(
            &system_log_ttl_sql("trace_log", 3),
            7
        ));
    }

    #[test]
    fn ip_round_trips_through_ch_form() {
        // v4 stored v4-mapped, normalized back to dotted.
        let v4 = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));
        assert_eq!(ip_to_ch(v4), "::ffff:10.0.0.5");
        assert_eq!(normalize_ch_ip("::ffff:10.0.0.5"), "10.0.0.5");
        // v6 unchanged.
        let v6 = IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1));
        assert_eq!(normalize_ch_ip(&ip_to_ch(v6)), "2001:db8::1");
    }

    #[test]
    fn conversations_sql_asn_filter_does_not_shadow_columns() {
        // Regression: the AS aggregates must not be aliased to the column names `src_as`/`dst_as`.
        // If they are, the asn drill-down filter (`AND (src_as = N OR dst_as = N)`) binds to the
        // aggregate alias and ClickHouse errors `ILLEGAL_AGGREGATION` (aggregate in WHERE), 500-ing
        // the conversations query so the table AND Sankey silently stop reflecting the AS filter.
        let q = FlowQuery {
            node_id: Some(Uuid::from_u128(1)),
            from_unix_ms: 0,
            to_unix_ms: 100_000,
            limit: 10,
            proto: Vec::new(),
            dst_port: Vec::new(),
            peer: Vec::new(),
            asn: vec![15169],
        };
        let sql = conversations_sql(&q);
        // No aggregate is aliased *exactly* to a filtered column (the collision is `AS src_as,` /
        // `AS dst_as,` — note the delimiter, so this doesn't false-match the correct `AS src_asn`).
        assert!(
            !sql.contains("AS src_as,") && !sql.contains("AS src_as "),
            "alias shadows src_as column: {sql}"
        );
        assert!(
            !sql.contains("AS dst_as,") && !sql.contains("AS dst_as "),
            "alias shadows dst_as column: {sql}"
        );
        assert!(sql.contains("any(src_as) AS src_asn"), "{sql}");
        assert!(sql.contains("any(dst_as) AS dst_asn"), "{sql}");
        // The asn filter still references the real columns (so it actually narrows). Spelled
        // `IN (…)` since ADR-053 Inc.8 widened the drill-downs to sets — a one-value set is still a
        // set, which is what keeps this one SQL shape rather than two.
        assert!(
            sql.contains("AND (src_as IN (15169) OR dst_as IN (15169))"),
            "{sql}"
        );
    }

    #[test]
    fn a_drill_down_set_is_a_disjunction_within_a_column_and_a_conjunction_across_them() {
        // What the four filters mean together: "TCP or UDP, on 80 or 443". Written against the SQL
        // because the ClickHouse path has no test double — the in-memory fake mirrors this, and
        // `in_window` carries the note about which direction a mismatch breaks in.
        let q = FlowQuery {
            node_id: None,
            from_unix_ms: 0,
            to_unix_ms: 100_000,
            limit: 10,
            proto: vec![6, 17],
            dst_port: vec![80, 443],
            peer: vec!["10.0.0.1".parse().unwrap()],
            asn: Vec::new(),
        };
        let sql = flow_filters_sql(&q);
        assert!(sql.contains(" AND proto IN (6,17)"), "{sql}");
        assert!(sql.contains(" AND dst_port IN (80,443)"), "{sql}");
        // A peer matches at either end, so its set appears twice — once per side. ⚠️ The rendering
        // is `ip_to_ch`'s, not `Display`'s: the column is IPv6 and a v4 address arrives as its
        // v4-mapped form (`::ffff:10.0.0.1`). Asserting the plain form here failed, which is the
        // test doing its job — a hand-written IPv4 literal would not have matched a single row.
        let mapped = ip_to_ch("10.0.0.1".parse().unwrap());
        assert!(
            sql.contains(&format!(
                "AND (src_ip IN ('{mapped}') OR dst_ip IN ('{mapped}'))"
            )),
            "{sql}"
        );
        // An empty set contributes nothing at all — not `IN ()`, which ClickHouse would reject.
        assert!(
            !sql.contains("src_as"),
            "an unset filter must emit no clause: {sql}"
        );

        let none = flow_filters_sql(&FlowQuery {
            proto: Vec::new(),
            dst_port: Vec::new(),
            peer: Vec::new(),
            ..q
        });
        assert_eq!(none, "", "no filters means no fragment");
    }

    #[test]
    fn node_scope_sql_scopes_only_when_node_given() {
        let id = Uuid::from_u128(1);
        assert_eq!(node_scope_sql(Some(id)), format!("node_id = '{id}' AND "));
        assert_eq!(node_scope_sql(None), "");
    }

    #[test]
    fn conversations_sql_fleet_omits_node_filter() {
        // Fleet-wide (node_id: None) must NOT restrict to a node — it aggregates every exporter.
        let q = FlowQuery {
            node_id: None,
            from_unix_ms: 0,
            to_unix_ms: 100_000,
            limit: 10,
            proto: Vec::new(),
            dst_port: Vec::new(),
            peer: Vec::new(),
            asn: Vec::new(),
        };
        let sql = conversations_sql(&q);
        assert!(
            !sql.contains("node_id ="),
            "fleet query must not scope to a node: {sql}"
        );
        // Still windowed and grouped across all nodes.
        assert!(sql.contains("WHERE ts >= toDateTime(0)"), "{sql}");
        assert!(sql.contains("GROUP BY src_ip, dst_ip"), "{sql}");
    }

    #[tokio::test]
    async fn in_memory_fleet_query_spans_all_nodes() {
        let store = InMemoryFlowStore::default();
        store
            .insert_batch(&[
                row(
                    Uuid::from_u128(1),
                    "10.0.0.2",
                    "8.8.8.8",
                    443,
                    6,
                    1000,
                    1_000,
                ),
                row(
                    Uuid::from_u128(2),
                    "10.0.0.3",
                    "8.8.4.4",
                    443,
                    6,
                    500,
                    1_000,
                ),
            ])
            .await
            .unwrap();
        let q = FlowQuery {
            node_id: None, // fleet-wide
            from_unix_ms: 0,
            to_unix_ms: 10_000,
            limit: 10,
            proto: Vec::new(),
            dst_port: Vec::new(),
            peer: Vec::new(),
            asn: Vec::new(),
        };
        // Fleet-wide aggregates across both exporters' nodes.
        assert_eq!(store.top_talkers(&q).await.unwrap().len(), 2);
        // A node-scoped query still narrows to one.
        let scoped = FlowQuery {
            node_id: Some(Uuid::from_u128(1)),
            ..q
        };
        assert_eq!(store.top_talkers(&scoped).await.unwrap().len(), 1);
    }

    #[test]
    fn json_number_parsing_tolerates_string_encoded_u64() {
        let v: Value = serde_json::json!({"bytes": "18446744073709551615", "packets": 12});
        assert_eq!(j_u64(&v, "bytes"), u64::MAX);
        assert_eq!(j_u64(&v, "packets"), 12);
        assert_eq!(j_u64(&v, "missing"), 0);
    }

    #[tokio::test]
    async fn in_memory_top_talkers_conversations_ports_protocols() {
        let node = Uuid::from_u128(1);
        let store = InMemoryFlowStore::default();
        assert!(store.is_empty());
        store
            .insert_batch(&[
                row(node, "10.0.0.2", "8.8.8.8", 443, 6, 1000, 1_000),
                row(node, "10.0.0.2", "8.8.8.8", 443, 6, 500, 2_000),
                row(node, "10.0.0.3", "1.1.1.1", 53, 17, 200, 1_500),
                // Different node — must be excluded.
                row(
                    Uuid::from_u128(2),
                    "10.9.9.9",
                    "8.8.8.8",
                    443,
                    6,
                    9999,
                    1_000,
                ),
            ])
            .await
            .unwrap();

        let q = FlowQuery {
            node_id: Some(node),
            from_unix_ms: 0,
            to_unix_ms: 10_000,
            limit: 10,
            proto: Vec::new(),
            dst_port: Vec::new(),
            peer: Vec::new(),
            asn: Vec::new(),
        };
        let talkers = store.top_talkers(&q).await.unwrap();
        assert_eq!(talkers[0].addr, "10.0.0.2");
        assert_eq!(talkers[0].bytes, 1500); // two rows summed
        assert_eq!(talkers[0].flows, 2);

        let convos = store.top_conversations(&q).await.unwrap();
        assert_eq!(convos[0].src, "10.0.0.2");
        assert_eq!(convos[0].dst, "8.8.8.8");
        assert_eq!(convos[0].bytes, 1500);

        let ports = store.top_ports(&q).await.unwrap();
        assert_eq!(ports[0].port, 443);
        assert_eq!(ports[0].bytes, 1500);

        let protos = store.top_protocols(&q).await.unwrap();
        assert_eq!(protos[0].proto, 6);
        assert_eq!(protos[0].bytes, 1500);
    }

    #[tokio::test]
    async fn in_memory_conversations_carry_stored_as() {
        let node = Uuid::from_u128(1);
        let store = InMemoryFlowStore::default();
        let mut r = row(node, "10.0.0.2", "17.248.221.6", 443, 6, 1000, 1_000);
        r.src_as = 0; // internal host, unknown AS
        r.dst_as = 15169; // Google (per-row, exporter-provided or write-time enriched)
        store.insert_batch(&[r]).await.unwrap();

        let q = FlowQuery {
            node_id: Some(node),
            from_unix_ms: 0,
            to_unix_ms: 10_000,
            limit: 10,
            proto: Vec::new(),
            dst_port: Vec::new(),
            peer: Vec::new(),
            asn: Vec::new(),
        };
        let convos = store.top_conversations(&q).await.unwrap();
        assert_eq!(convos[0].src_asn, 0);
        assert_eq!(convos[0].dst_asn, 15169);
        // The store never resolves names — that's the API layer's job.
        assert_eq!(convos[0].src_as_name, None);
        assert_eq!(convos[0].dst_as_name, None);
    }

    #[tokio::test]
    async fn in_memory_series_buckets_by_five_minutes_per_proto() {
        let node = Uuid::from_u128(1);
        let store = InMemoryFlowStore::default();
        // Two records in the same 5-min bucket (proto 6), one in a later bucket (proto 17).
        store
            .insert_batch(&[
                row(node, "10.0.0.2", "8.8.8.8", 443, 6, 100, 60_000), // bucket 0
                row(node, "10.0.0.2", "8.8.8.8", 443, 6, 100, 120_000), // bucket 0
                row(node, "10.0.0.3", "1.1.1.1", 53, 17, 50, 600_000), // bucket 600
            ])
            .await
            .unwrap();
        let q = FlowSeriesQuery {
            node_id: Some(node),
            from_unix_ms: 0,
            to_unix_ms: 1_000_000,
            proto: Vec::new(),
        };
        let pts = store.series(&q).await.unwrap();
        assert_eq!(pts.len(), 2);
        assert_eq!(pts[0].ts_unix_ms, 0);
        assert_eq!(pts[0].proto, 6);
        assert_eq!(pts[0].bytes, 200);
        assert_eq!(pts[1].ts_unix_ms, 600_000);
        assert_eq!(pts[1].proto, 17);
    }

    /// A `FlowRow` with an explicit source/destination AS.
    fn row_as(
        node: Uuid,
        src: &str,
        dst: &str,
        port: u16,
        proto: u8,
        bytes: u64,
        dst_as: u32,
    ) -> FlowRow {
        let mut r = row(node, src, dst, port, proto, bytes, 1_000);
        r.dst_as = dst_as;
        r
    }

    #[tokio::test]
    async fn in_memory_top_as_groups_and_filters() {
        let node = Uuid::from_u128(1);
        let store = InMemoryFlowStore::default();
        store
            .insert_batch(&[
                row_as(node, "10.0.0.2", "8.8.8.8", 443, 6, 1000, 15169), // Google
                row_as(node, "10.0.0.2", "8.8.4.4", 443, 6, 500, 15169),  // same AS, folds
                row_as(node, "10.0.0.3", "1.1.1.1", 53, 17, 300, 13335),  // Cloudflare (UDP/53)
            ])
            .await
            .unwrap();

        // Top destination AS: 15169 (1500) then 13335 (300).
        let q = FlowQuery {
            node_id: Some(node),
            from_unix_ms: 0,
            to_unix_ms: 10_000,
            limit: 10,
            proto: Vec::new(),
            dst_port: Vec::new(),
            peer: Vec::new(),
            asn: Vec::new(),
        };
        let top = store.top_as(&q, AsDir::Dst).await.unwrap();
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].asn, 15169);
        assert_eq!(top[0].bytes, 1500);
        assert_eq!(top[1].asn, 13335);

        // Protocol filter (UDP) narrows to just the Cloudflare AS.
        let q_udp = FlowQuery {
            proto: vec![17],
            ..q.clone()
        };
        let udp = store.top_as(&q_udp, AsDir::Dst).await.unwrap();
        assert_eq!(udp.len(), 1);
        assert_eq!(udp[0].asn, 13335);

        // Peer filter (a specific destination host) narrows the port view.
        let q_peer = FlowQuery {
            peer: vec!["1.1.1.1".parse().unwrap()],
            ..q.clone()
        };
        let ports = store.top_ports(&q_peer).await.unwrap();
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].port, 53);

        // AS filter narrows every view to flows touching that ASN (as src or dst).
        let q_asn = FlowQuery {
            asn: vec![13335],
            ..q.clone()
        };
        let as_only = store.top_as(&q_asn, AsDir::Dst).await.unwrap();
        assert_eq!(as_only.len(), 1);
        assert_eq!(as_only[0].asn, 13335);
        let talkers = store.top_talkers(&q_asn).await.unwrap();
        assert_eq!(talkers.len(), 1);
        assert_eq!(talkers[0].addr, "10.0.0.3"); // only the Cloudflare-bound flow survives
    }

    #[tokio::test]
    async fn in_memory_fanout_counts_distinct_dst_and_ports() {
        let node = Uuid::from_u128(1);
        let store = InMemoryFlowStore::default();
        // A scanner (10.0.0.9) hits three distinct hosts on three ports; a normal host talks to one.
        store
            .insert_batch(&[
                row(node, "10.0.0.9", "10.0.1.1", 22, 6, 100, 1_000),
                row(node, "10.0.0.9", "10.0.1.2", 23, 6, 100, 1_100),
                row(node, "10.0.0.9", "10.0.1.3", 80, 6, 100, 1_200),
                row(node, "10.0.0.2", "8.8.8.8", 443, 6, 5000, 1_300),
            ])
            .await
            .unwrap();
        let q = FlowQuery {
            node_id: Some(node),
            from_unix_ms: 0,
            to_unix_ms: 10_000,
            limit: 10,
            proto: Vec::new(),
            dst_port: Vec::new(),
            peer: Vec::new(),
            asn: Vec::new(),
        };
        let fan = store.fanout_by_src(&q).await.unwrap();
        // Highest fan-out first: the scanner leads with 3 distinct destinations / 3 ports.
        assert_eq!(fan[0].src, "10.0.0.9");
        assert_eq!(fan[0].distinct_dst, 3);
        assert_eq!(fan[0].distinct_ports, 3);
        assert_eq!(fan[1].src, "10.0.0.2");
        assert_eq!(fan[1].distinct_dst, 1);
    }
}
