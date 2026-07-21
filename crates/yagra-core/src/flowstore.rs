// SPDX-License-Identifier: AGPL-3.0-only
//! The flow store seam: where edge-aggregated flow records are written and queried (ADR-031).
//!
//! [`FlowStore`] abstracts flow storage so the bus consumer/writer and the flow API don't care
//! whether they talk to ClickHouse ([`ChStore`], live) or an in-memory fake ([`InMemoryFlowStore`],
//! tests). ClickHouse is the **4th store** added to ADR-004's split — a *loss-tolerant* tier
//! (ADR-017): 1-month TTL, single-node MVP, no must-never-lose guarantee. It exists because the flow
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
#[cfg(test)]
use std::sync::Mutex;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

/// Default flow retention in days (ADR-031). Tunable via `YAGRA_FLOW_RETENTION_DAYS`.
pub const DEFAULT_FLOW_RETENTION_DAYS: u32 = 30;

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

/// A top-N flow query: one node, a time window, a result cap, and optional drill-down filters.
/// Filters are strongly typed (never raw strings) so nothing device-supplied is interpolated.
#[derive(Debug, Clone, Copy)]
pub struct FlowQuery {
    /// Node whose flows to query.
    pub node_id: Uuid,
    /// Window start (Unix ms, inclusive).
    pub from_unix_ms: i64,
    /// Window end (Unix ms, inclusive).
    pub to_unix_ms: i64,
    /// Max rows to return (clamped).
    pub limit: u32,
    /// Optional IP-protocol filter.
    pub proto: Option<u8>,
    /// Optional destination-port filter.
    pub dst_port: Option<u16>,
    /// Optional peer filter — rows where this address is the source or the destination.
    pub peer: Option<IpAddr>,
    /// Optional AS filter — rows where this ASN is the source or the destination AS (0 = unknown).
    pub asn: Option<u32>,
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
#[derive(Debug, Clone, Copy)]
pub struct FlowSeriesQuery {
    /// Node whose trend to query.
    pub node_id: Uuid,
    /// Window start (Unix ms, inclusive).
    pub from_unix_ms: i64,
    /// Window end (Unix ms, inclusive).
    pub to_unix_ms: i64,
    /// Optional IP-protocol filter (the only fact-table filter the rollup supports).
    pub proto: Option<u8>,
}

/// A top-talker: one host address with summed traffic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

/// A trend point: bytes/packets for one protocol at one 5-minute bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

/// Build the optional fact-table filter fragment (`AND …`) for a [`FlowQuery`]. Only typed values
/// are interpolated: `proto`/`dst_port` are integers and `peer` is a validated [`IpAddr`] rendered
/// via [`ip_to_ch`] — no device-supplied string ever reaches SQL.
fn flow_filters_sql(q: &FlowQuery) -> String {
    let mut s = String::new();
    if let Some(p) = q.proto {
        s.push_str(&format!(" AND proto = {p}"));
    }
    if let Some(port) = q.dst_port {
        s.push_str(&format!(" AND dst_port = {port}"));
    }
    if let Some(peer) = q.peer {
        let ip = ip_to_ch(peer);
        s.push_str(&format!(" AND (src_ip = '{ip}' OR dst_ip = '{ip}')"));
    }
    if let Some(asn) = q.asn {
        s.push_str(&format!(" AND (src_as = {asn} OR dst_as = {asn})"));
    }
    s
}

// ─── ClickHouse (live) ────────────────────────────────────────────────────────────────

/// A [`FlowStore`] backed by ClickHouse over its HTTP interface.
pub struct ChStore {
    http: reqwest::Client,
    base: String,
    retention_days: u32,
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
            retention_days: retention_days.max(1),
        }
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
        let ttl = self.retention_days;
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
        tracing::info!(retention_days = ttl, "ClickHouse flow schema ensured");
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
        let sql = format!(
            "SELECT src_ip AS k, sum(bytes) AS bytes, sum(packets) AS packets, sum(flows) AS flows
             FROM flow_records
             WHERE node_id = '{}' AND ts >= toDateTime({from}) AND ts <= toDateTime({to}){filters}
             GROUP BY src_ip ORDER BY bytes DESC LIMIT {limit} FORMAT JSONEachRow",
            q.node_id
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
        let (from, to) = window_secs(q.from_unix_ms, q.to_unix_ms);
        let limit = q.limit.clamp(1, 1000);
        let filters = flow_filters_sql(q);
        // `any(src_as)`/`any(dst_as)` keep one row per (src,dst): grouping by AS too would split a
        // conversation if its stored AS ever varied. AS-per-IP is effectively stable, so `any` is safe.
        let sql = format!(
            "SELECT src_ip AS s, dst_ip AS d, any(src_as) AS src_as, any(dst_as) AS dst_as, sum(bytes) AS bytes, sum(packets) AS packets, sum(flows) AS flows
             FROM flow_records
             WHERE node_id = '{}' AND ts >= toDateTime({from}) AND ts <= toDateTime({to}){filters}
             GROUP BY src_ip, dst_ip ORDER BY bytes DESC LIMIT {limit} FORMAT JSONEachRow",
            q.node_id
        );
        Ok(self
            .query_json(&sql)
            .await?
            .iter()
            .map(|v| FlowConversation {
                src: normalize_ch_ip(&j_str(v, "s")),
                dst: normalize_ch_ip(&j_str(v, "d")),
                src_asn: j_u64(v, "src_as") as u32,
                dst_asn: j_u64(v, "dst_as") as u32,
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
        let sql = format!(
            "SELECT dst_port AS k, sum(bytes) AS bytes, sum(packets) AS packets, sum(flows) AS flows
             FROM flow_records
             WHERE node_id = '{}' AND ts >= toDateTime({from}) AND ts <= toDateTime({to}){filters}
             GROUP BY dst_port ORDER BY bytes DESC LIMIT {limit} FORMAT JSONEachRow",
            q.node_id
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
        let sql = format!(
            "SELECT proto AS k, sum(bytes) AS bytes, sum(packets) AS packets, sum(flows) AS flows
             FROM flow_records
             WHERE node_id = '{}' AND ts >= toDateTime({from}) AND ts <= toDateTime({to}){filters}
             GROUP BY proto ORDER BY bytes DESC LIMIT {limit} FORMAT JSONEachRow",
            q.node_id
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
        let sql = format!(
            "SELECT {col} AS asn, sum(bytes) AS bytes, sum(packets) AS packets, sum(flows) AS flows
             FROM flow_records
             WHERE node_id = '{}' AND ts >= toDateTime({from}) AND ts <= toDateTime({to}){filters}
             GROUP BY asn ORDER BY bytes DESC LIMIT {limit} FORMAT JSONEachRow",
            q.node_id
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
        let proto_filter = q
            .proto
            .map(|p| format!(" AND proto = {p}"))
            .unwrap_or_default();
        let sql = format!(
            "SELECT toUnixTimestamp(ts) AS t, proto, sum(bytes) AS bytes, sum(packets) AS packets
             FROM flow_rollup_5m
             WHERE node_id = '{}' AND ts >= toDateTime({from}) AND ts <= toDateTime({to}){proto_filter}
             GROUP BY t, proto ORDER BY t ASC FORMAT JSONEachRow",
            q.node_id
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

    fn in_window(&self, q: &FlowQuery) -> Vec<FlowRow> {
        self.rows
            .lock()
            .expect("flow fake mutex poisoned")
            .iter()
            .filter(|r| {
                r.node_id == q.node_id
                    && r.ts_unix_ms >= q.from_unix_ms
                    && r.ts_unix_ms <= q.to_unix_ms
                    && q.proto.is_none_or(|p| r.proto == p)
                    && q.dst_port.is_none_or(|port| r.dst_port == port)
                    && q.peer
                        .is_none_or(|peer| r.src_ip == peer || r.dst_ip == peer)
                    && q.asn.is_none_or(|a| r.src_as == a || r.dst_as == a)
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
            proto: q.proto,
            dst_port: None,
            peer: None,
            asn: None,
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
            node_id: node,
            from_unix_ms: 0,
            to_unix_ms: 10_000,
            limit: 10,
            proto: None,
            dst_port: None,
            peer: None,
            asn: None,
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
            node_id: node,
            from_unix_ms: 0,
            to_unix_ms: 10_000,
            limit: 10,
            proto: None,
            dst_port: None,
            peer: None,
            asn: None,
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
            node_id: node,
            from_unix_ms: 0,
            to_unix_ms: 1_000_000,
            proto: None,
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
            node_id: node,
            from_unix_ms: 0,
            to_unix_ms: 10_000,
            limit: 10,
            proto: None,
            dst_port: None,
            peer: None,
            asn: None,
        };
        let top = store.top_as(&q, AsDir::Dst).await.unwrap();
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].asn, 15169);
        assert_eq!(top[0].bytes, 1500);
        assert_eq!(top[1].asn, 13335);

        // Protocol filter (UDP) narrows to just the Cloudflare AS.
        let q_udp = FlowQuery {
            proto: Some(17),
            ..q
        };
        let udp = store.top_as(&q_udp, AsDir::Dst).await.unwrap();
        assert_eq!(udp.len(), 1);
        assert_eq!(udp[0].asn, 13335);

        // Peer filter (a specific destination host) narrows the port view.
        let q_peer = FlowQuery {
            peer: Some("1.1.1.1".parse().unwrap()),
            ..q
        };
        let ports = store.top_ports(&q_peer).await.unwrap();
        assert_eq!(ports.len(), 1);
        assert_eq!(ports[0].port, 53);

        // AS filter narrows every view to flows touching that ASN (as src or dst).
        let q_asn = FlowQuery {
            asn: Some(13335),
            ..q
        };
        let as_only = store.top_as(&q_asn, AsDir::Dst).await.unwrap();
        assert_eq!(as_only.len(), 1);
        assert_eq!(as_only[0].asn, 13335);
        let talkers = store.top_talkers(&q_asn).await.unwrap();
        assert_eq!(talkers.len(), 1);
        assert_eq!(talkers[0].addr, "10.0.0.3"); // only the Cloudflare-bound flow survives
    }
}
