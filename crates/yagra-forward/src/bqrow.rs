// SPDX-License-Identifier: AGPL-3.0-only
//! Normalized BigQuery rows for forwarded passive data (ADR-034 Increment 3).
//!
//! The relay destinations ([`crate::render`]) reproduce a *datagram*; BigQuery is the other kind of
//! destination — it takes a **structured row**, one per event or per flow record, with typed columns
//! an analyst can query. There is deliberately no raw-payload column: a table that stores the
//! original bytes is a copy of the log, not an analysis of it, and it makes the credential exposure
//! that already worries us (syslog bodies carry passwords) permanent and queryable off-box.
//!
//! Row shape and table schema live in the same file **on purpose**. They are one contract with two
//! halves — `ensure_table` creates columns from [`event_schema`] / [`flow_schema`], and the writer
//! emits keys from [`event_row`] / [`flow_row`]. A key that drifts out of the schema is silently
//! dropped by BigQuery (the insert asks for `ignoreUnknownValues`, because the alternative is losing
//! whole batches to an older table), so the drift would be invisible in production. The tests at the
//! bottom assert every emitted key exists as a column, which is the only place that can catch it.
//!
//! **Absent fields are omitted, not sent as null.** A missing key lands as `NULL` in a `NULLABLE`
//! column either way, and omitting keeps the request small on the fields that are usually empty
//! (`trap_oid` on syslog, `hostname` on a device that sends none).

use std::net::IpAddr;

use serde_json::{json, Value};
use yagra_bus::EventMsg;

/// Longest message text put in a row. Reception already truncates at 4096 characters and sets
/// `truncated`; this is a second belt so a future intake change cannot start posting megabyte rows
/// into a batch sized for small ones.
const MAX_MESSAGE_CHARS: usize = 4096;

/// Most varbinds carried into a row. Reception caps at 32; this mirrors it so the repeated field
/// cannot grow unbounded if that cap ever moves.
const MAX_VARBINDS: usize = 32;

// ── Table schemas ────────────────────────────────────────────────────────────────────────────

/// BigQuery schema for a syslog/trap destination's table.
///
/// `node_id` is deliberately **absent**. The tee runs before the event engine resolves a source
/// address to a monitored node, so there is no node id to write — inventing one here would mean
/// duplicating that resolution (and its cache) inside the forwarder. Join on `source_ip` instead.
#[must_use]
pub fn event_schema() -> Value {
    json!([
        { "name": "event_id",   "type": "STRING",    "mode": "NULLABLE", "description": "Yagra's id for the received event." },
        { "name": "event_time", "type": "TIMESTAMP", "mode": "NULLABLE", "description": "When Yagra received it." },
        { "name": "kind",       "type": "STRING",    "mode": "NULLABLE", "description": "syslog | trap | webhook." },
        { "name": "pool",       "type": "STRING",    "mode": "NULLABLE", "description": "Poller pool that received it." },
        { "name": "source_ip",  "type": "STRING",    "mode": "NULLABLE", "description": "Sending device's address." },
        { "name": "src_port",   "type": "INTEGER",   "mode": "NULLABLE", "description": "Sending device's source port." },
        { "name": "hostname",   "type": "STRING",    "mode": "NULLABLE", "description": "Syslog HOSTNAME field." },
        { "name": "app_name",   "type": "STRING",    "mode": "NULLABLE", "description": "Syslog APP-NAME / tag." },
        { "name": "facility",   "type": "INTEGER",   "mode": "NULLABLE", "description": "Syslog facility (0-23)." },
        { "name": "severity",   "type": "INTEGER",   "mode": "NULLABLE", "description": "Syslog severity (0 = emerg .. 7 = debug)." },
        { "name": "trap_oid",   "type": "STRING",    "mode": "NULLABLE", "description": "SNMP trap identity OID." },
        { "name": "message",    "type": "STRING",    "mode": "NULLABLE", "description": "Normalized message text." },
        { "name": "truncated",  "type": "BOOLEAN",   "mode": "NULLABLE", "description": "Whether reception truncated the text." },
        {
            "name": "varbinds", "type": "RECORD", "mode": "REPEATED",
            "description": "SNMP varbinds. A repeated record rather than a JSON blob so `UNNEST` can filter on an OID.",
            "fields": [
                { "name": "oid",   "type": "STRING", "mode": "NULLABLE" },
                { "name": "value", "type": "STRING", "mode": "NULLABLE" }
            ]
        }
    ])
}

/// BigQuery schema for a flow destination's table — one row per decoded flow record.
///
/// `observed_time` is when Yagra received the export, not the flow's own start/end: NetFlow's
/// switched timestamps are relative to the exporter's sysUpTime and are not reconstructed by the
/// parser, so writing them would mean writing a guess.
#[must_use]
pub fn flow_schema() -> Value {
    json!([
        { "name": "observed_time", "type": "TIMESTAMP", "mode": "NULLABLE", "description": "When Yagra received the export datagram." },
        { "name": "exporter_ip",   "type": "STRING",    "mode": "NULLABLE", "description": "Exporting device's address." },
        { "name": "pool",          "type": "STRING",    "mode": "NULLABLE", "description": "Poller pool that received it." },
        { "name": "export_proto",  "type": "STRING",    "mode": "NULLABLE", "description": "netflow | sflow." },
        { "name": "src_addr",      "type": "STRING",    "mode": "NULLABLE", "description": "Flow source address." },
        { "name": "dst_addr",      "type": "STRING",    "mode": "NULLABLE", "description": "Flow destination address." },
        { "name": "src_port",      "type": "INTEGER",   "mode": "NULLABLE", "description": "Flow source transport port." },
        { "name": "dst_port",      "type": "INTEGER",   "mode": "NULLABLE", "description": "Flow destination transport port." },
        { "name": "proto",         "type": "INTEGER",   "mode": "NULLABLE", "description": "IP protocol number." },
        { "name": "tos",           "type": "INTEGER",   "mode": "NULLABLE", "description": "IP type-of-service / DSCP byte." },
        { "name": "if_index",      "type": "INTEGER",   "mode": "NULLABLE", "description": "Ingress interface ifIndex (0 = unknown)." },
        { "name": "src_as",        "type": "INTEGER",   "mode": "NULLABLE", "description": "Source AS number (0 = unknown)." },
        { "name": "dst_as",        "type": "INTEGER",   "mode": "NULLABLE", "description": "Destination AS number (0 = unknown)." },
        { "name": "bytes",         "type": "INTEGER",   "mode": "NULLABLE", "description": "Bytes reported by the record." },
        { "name": "packets",       "type": "INTEGER",   "mode": "NULLABLE", "description": "Packets reported by the record." }
    ])
}

/// The column BigQuery should partition a syslog/trap table by (DAY partitioning).
pub const EVENT_PARTITION_FIELD: &str = "event_time";
/// The column BigQuery should partition a flow table by (DAY partitioning).
pub const FLOW_PARTITION_FIELD: &str = "observed_time";
/// Clustering for a syslog/trap table — the two columns nearly every query filters on.
pub const EVENT_CLUSTERING: [&str; 2] = ["kind", "source_ip"];
/// Clustering for a flow table.
pub const FLOW_CLUSTERING: [&str; 2] = ["exporter_ip", "src_addr"];

// ── Rows ─────────────────────────────────────────────────────────────────────────────────────

/// One decoded flow record, with the datagram context needed to make a self-contained row.
/// Separate from [`crate::FlowFields`] (the filter's view) because a row carries counters and
/// interface context a filter has no operator for.
#[derive(Debug, Clone, Copy)]
pub struct FlowRow<'a> {
    /// When the export datagram was received, in Unix milliseconds.
    pub observed_unix_ms: i64,
    /// Exporting device's address.
    pub exporter_ip: IpAddr,
    /// Poller pool that received it.
    pub pool: Option<&'a str>,
    /// Wire format: `netflow` or `sflow`.
    pub export_proto: &'a str,
    /// Flow source address.
    pub src_addr: IpAddr,
    /// Flow destination address.
    pub dst_addr: IpAddr,
    /// Flow source transport port.
    pub src_port: u16,
    /// Flow destination transport port.
    pub dst_port: u16,
    /// IP protocol number.
    pub proto: u8,
    /// IP type-of-service / DSCP byte.
    pub tos: u8,
    /// Ingress interface ifIndex (0 = unknown).
    pub if_index: u32,
    /// Source AS number (0 = unknown).
    pub src_as: u32,
    /// Destination AS number (0 = unknown).
    pub dst_as: u32,
    /// Bytes reported by the record.
    pub bytes: u64,
    /// Packets reported by the record.
    pub packets: u64,
    /// This record's index within its datagram. Only used to make the insert id unique.
    pub seq: usize,
}

/// Build the `insertAll` row envelope for one received event.
///
/// The `insertId` is the event id, so a replayed batch (a retry after a timeout that actually
/// succeeded) is de-duplicated by BigQuery rather than double-counted.
#[must_use]
pub fn event_row(msg: &EventMsg) -> Value {
    let mut row = serde_json::Map::new();
    row.insert("event_id".into(), json!(msg.event_id.to_string()));
    row.insert("event_time".into(), json!(rfc3339(msg.at_unix_ms)));
    row.insert("kind".into(), json!(msg.kind.as_str()));
    row.insert("message".into(), json!(clip(&msg.message)));
    row.insert("truncated".into(), json!(msg.truncated));
    put(&mut row, "pool", msg.pool.as_deref().map(Value::from));
    put(
        &mut row,
        "source_ip",
        msg.source_ip.map(|ip| Value::from(ip.to_string())),
    );
    put(&mut row, "src_port", msg.src_port.map(Value::from));
    put(
        &mut row,
        "hostname",
        msg.hostname.as_deref().map(Value::from),
    );
    put(
        &mut row,
        "app_name",
        msg.app_name.as_deref().map(Value::from),
    );
    put(&mut row, "facility", msg.facility.map(Value::from));
    put(&mut row, "severity", msg.syslog_severity.map(Value::from));
    put(
        &mut row,
        "trap_oid",
        msg.trap_oid.as_deref().map(Value::from),
    );
    if !msg.varbinds.is_empty() {
        let vbs: Vec<Value> = msg
            .varbinds
            .iter()
            .take(MAX_VARBINDS)
            .map(|(oid, value)| json!({ "oid": oid, "value": clip(value) }))
            .collect();
        row.insert("varbinds".into(), Value::Array(vbs));
    }
    json!({ "insertId": msg.event_id.to_string(), "json": Value::Object(row) })
}

/// Build the `insertAll` row envelope for one decoded flow record.
///
/// The `insertId` is derived from the datagram (`exporter`, receive time, record index) rather than
/// generated, so re-sending the same datagram de-duplicates instead of duplicating — the same
/// property the event id gives for free.
#[must_use]
pub fn flow_row(rec: &FlowRow<'_>) -> Value {
    let mut row = serde_json::Map::new();
    row.insert("observed_time".into(), json!(rfc3339(rec.observed_unix_ms)));
    row.insert("exporter_ip".into(), json!(rec.exporter_ip.to_string()));
    row.insert("export_proto".into(), json!(rec.export_proto));
    row.insert("src_addr".into(), json!(rec.src_addr.to_string()));
    row.insert("dst_addr".into(), json!(rec.dst_addr.to_string()));
    row.insert("src_port".into(), json!(rec.src_port));
    row.insert("dst_port".into(), json!(rec.dst_port));
    row.insert("proto".into(), json!(rec.proto));
    row.insert("tos".into(), json!(rec.tos));
    row.insert("if_index".into(), json!(rec.if_index));
    row.insert("src_as".into(), json!(rec.src_as));
    row.insert("dst_as".into(), json!(rec.dst_as));
    row.insert("bytes".into(), json!(rec.bytes));
    row.insert("packets".into(), json!(rec.packets));
    put(&mut row, "pool", rec.pool.map(Value::from));
    let insert_id = format!("{}-{}-{}", rec.exporter_ip, rec.observed_unix_ms, rec.seq);
    json!({ "insertId": insert_id, "json": Value::Object(row) })
}

fn put(row: &mut serde_json::Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        row.insert(key.to_owned(), value);
    }
}

fn clip(text: &str) -> String {
    text.chars().take(MAX_MESSAGE_CHARS).collect()
}

/// Unix milliseconds as an RFC 3339 timestamp. BigQuery also accepts an epoch number, but a string
/// is unambiguous about its unit — a `TIMESTAMP` column fed raw milliseconds reads as year 56000.
fn rfc3339(unix_ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(unix_ms)
        .unwrap_or_else(chrono::Utc::now)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use uuid::Uuid;
    use yagra_bus::{EventKind, EventMsg};

    /// Every column name a schema declares, including nested record fields (flattened — a nested
    /// name can never collide with a top-level one in these two schemas).
    fn columns(schema: &Value) -> Vec<String> {
        let mut out = Vec::new();
        for field in schema.as_array().expect("schema is an array") {
            out.push(field["name"].as_str().expect("named column").to_owned());
            if let Some(nested) = field.get("fields").and_then(Value::as_array) {
                for f in nested {
                    out.push(f["name"].as_str().expect("named column").to_owned());
                }
            }
        }
        out
    }

    /// Keys a row emits, flattened the same way.
    fn keys(row: &Value) -> Vec<String> {
        let mut out = Vec::new();
        let obj = row["json"].as_object().expect("row body is an object");
        for (k, v) in obj {
            out.push(k.clone());
            if let Some(items) = v.as_array() {
                for item in items {
                    for nk in item.as_object().into_iter().flatten().map(|(k, _)| k) {
                        out.push(nk.clone());
                    }
                }
            }
        }
        out
    }

    fn event(kind: EventKind) -> EventMsg {
        EventMsg {
            schema_version: yagra_bus::BUS_SCHEMA_VERSION,
            event_id: Uuid::from_u128(7),
            kind,
            at_unix_ms: 1_700_000_000_000,
            source_ip: Some(Ipv4Addr::new(10, 0, 0, 9).into()),
            pool: Some("tokyo".into()),
            message: "link down on Gi0/1".into(),
            facility: Some(23),
            syslog_severity: Some(3),
            hostname: Some("core-sw-1".into()),
            app_name: Some("LINK".into()),
            trap_oid: (kind == EventKind::Trap).then(|| "1.3.6.1.6.3.1.1.5.3".to_owned()),
            varbinds: vec![("1.3.6.1.2.1.2.2.1.1".to_owned(), "3".to_owned())],
            truncated: false,
            raw: None,
            src_port: Some(51_234),
        }
    }

    fn flow<'a>(pool: Option<&'a str>) -> FlowRow<'a> {
        FlowRow {
            observed_unix_ms: 1_700_000_000_000,
            exporter_ip: Ipv4Addr::new(192, 168, 1, 1).into(),
            pool,
            export_proto: "netflow",
            src_addr: Ipv4Addr::new(10, 1, 1, 5).into(),
            dst_addr: Ipv4Addr::new(8, 8, 8, 8).into(),
            src_port: 40_000,
            dst_port: 53,
            proto: 17,
            tos: 0,
            if_index: 2,
            src_as: 0,
            dst_as: 15_169,
            bytes: 1024,
            packets: 8,
            seq: 3,
        }
    }

    #[test]
    fn every_event_key_is_a_declared_column() {
        // The insert asks BigQuery to ignore unknown values (so an older table still takes the
        // batch), which means a key that drifts out of the schema is dropped in silence. This is
        // the only place that can notice.
        let declared = columns(&event_schema());
        for key in keys(&event_row(&event(EventKind::Trap))) {
            assert!(declared.contains(&key), "{key} has no column in the schema");
        }
    }

    #[test]
    fn every_flow_key_is_a_declared_column() {
        let declared = columns(&flow_schema());
        for key in keys(&flow_row(&flow(Some("tokyo")))) {
            assert!(declared.contains(&key), "{key} has no column in the schema");
        }
    }

    #[test]
    fn partition_and_clustering_columns_exist_in_their_schema() {
        // BigQuery rejects table creation outright if these name nothing, and the failure would
        // only show up the first time an operator points a destination at a fresh dataset.
        let events = columns(&event_schema());
        assert!(events.contains(&EVENT_PARTITION_FIELD.to_owned()));
        for c in EVENT_CLUSTERING {
            assert!(events.contains(&c.to_owned()), "{c} is not a column");
        }
        let flows = columns(&flow_schema());
        assert!(flows.contains(&FLOW_PARTITION_FIELD.to_owned()));
        for c in FLOW_CLUSTERING {
            assert!(flows.contains(&c.to_owned()), "{c} is not a column");
        }
    }

    #[test]
    fn no_schema_stores_the_raw_payload() {
        // A raw-bytes column would make the credential exposure in syslog bodies permanent and
        // queryable off-box. BigQuery destinations are normalized rows, deliberately.
        for schema in [event_schema(), flow_schema()] {
            for column in columns(&schema) {
                assert!(
                    !["raw", "payload", "bytes_raw", "datagram"].contains(&column.as_str()),
                    "{column} looks like a raw-payload column"
                );
            }
        }
    }

    #[test]
    fn absent_fields_are_omitted_rather_than_written_as_null() {
        let mut msg = event(EventKind::Syslog);
        msg.hostname = None;
        msg.pool = None;
        msg.source_ip = None;
        msg.varbinds.clear();
        let row = event_row(&msg);
        let body = row["json"].as_object().unwrap();
        for absent in ["hostname", "pool", "source_ip", "varbinds"] {
            assert!(!body.contains_key(absent), "{absent} should be omitted");
        }
        // ...and a field that *is* set still lands.
        assert_eq!(body["app_name"], json!("LINK"));
    }

    #[test]
    fn event_insert_id_is_the_event_id_so_a_replayed_batch_deduplicates() {
        let msg = event(EventKind::Syslog);
        let row = event_row(&msg);
        assert_eq!(row["insertId"], json!(msg.event_id.to_string()));
        // The same message rendered twice must produce the same id — that is the whole mechanism.
        assert_eq!(event_row(&msg)["insertId"], row["insertId"]);
    }

    #[test]
    fn flow_insert_id_is_derived_so_a_resent_datagram_deduplicates() {
        let rec = flow(None);
        let id = flow_row(&rec)["insertId"].clone();
        assert_eq!(id, json!("192.168.1.1-1700000000000-3"));
        // A different record in the same datagram must not collide with it.
        let mut other = rec;
        other.seq = 4;
        assert_ne!(flow_row(&other)["insertId"], id);
    }

    #[test]
    fn timestamps_are_rfc3339_not_raw_milliseconds() {
        // A TIMESTAMP column fed 1700000000000 as a number reads as year 56000, and nothing rejects
        // it — the mistake only surfaces as nonsense in a query weeks later.
        let row = event_row(&event(EventKind::Syslog));
        assert_eq!(row["json"]["event_time"], json!("2023-11-14T22:13:20.000Z"));
        assert_eq!(
            flow_row(&flow(None))["json"]["observed_time"],
            json!("2023-11-14T22:13:20.000Z")
        );
    }

    #[test]
    fn a_nonsensical_timestamp_does_not_panic() {
        let mut msg = event(EventKind::Syslog);
        msg.at_unix_ms = i64::MAX;
        let row = event_row(&msg);
        assert!(row["json"]["event_time"].is_string());
    }

    #[test]
    fn oversized_text_is_clipped_and_varbinds_are_bounded() {
        let mut msg = event(EventKind::Trap);
        msg.message = "x".repeat(MAX_MESSAGE_CHARS * 2);
        msg.varbinds = (0..MAX_VARBINDS * 2)
            .map(|i| (format!("1.3.6.1.4.1.{i}"), "v".to_owned()))
            .collect();
        let row = event_row(&msg);
        assert_eq!(
            row["json"]["message"].as_str().unwrap().chars().count(),
            MAX_MESSAGE_CHARS
        );
        assert_eq!(
            row["json"]["varbinds"].as_array().unwrap().len(),
            MAX_VARBINDS
        );
    }

    #[test]
    fn numeric_columns_are_numbers_and_addresses_are_strings() {
        let row = event_row(&event(EventKind::Syslog));
        assert_eq!(row["json"]["severity"], json!(3));
        assert!(row["json"]["source_ip"].is_string());
        let f = flow_row(&flow(None));
        assert_eq!(f["json"]["bytes"], json!(1024));
        assert!(f["json"]["src_addr"].is_string());
    }
}
