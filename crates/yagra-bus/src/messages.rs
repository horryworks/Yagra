//! Bus message contract between core (Yagra-core) and pollers (Yagra-poller).
//!
//! These are the *only* way core and pollers talk (ADR-003). Messages are
//! **version-tolerant** (ADR-017): every message carries `schema_version`, new fields
//! are added as `#[serde(default)]`, and unknown fields are ignored (we never use
//! `deny_unknown_fields`). That is what lets a new core run against an old poller, and
//! vice versa, during a rolling upgrade.

use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use uuid::Uuid;
use yagra_common::{IfIndex, InterfaceField, MetricKind, NodeId, SeriesKey};

/// Current bus message schema version. Bump on a backward-compatible change; a
/// breaking change needs an N/N-1 migration plan (ADR-017).
pub const BUS_SCHEMA_VERSION: u16 = 1;

const fn default_version() -> u16 {
    BUS_SCHEMA_VERSION
}

/// A unit of polling work core dispatches to a poller.
///
/// Carries everything the poller needs to execute (target, check spec, interval) so
/// the poller stays stateless. Credentials are delivered by reference here; core
/// resolves/inlines the decrypted secret over the TLS bus at send time (ADR-018/020) —
/// the skeleton's ICMP path needs none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PollJob {
    /// Message schema version (defaulted for forward-compat).
    #[serde(default = "default_version")]
    pub schema_version: u16,
    /// Unique id of this job (correlates the result).
    pub job_id: Uuid,
    /// Node being polled.
    pub node_id: NodeId,
    /// Address to poll (IPv4 or IPv6).
    pub target: IpAddr,
    /// What to do.
    pub check: CheckSpec,
    /// Desired polling interval, in seconds (jitter applied by the scheduler).
    pub interval_secs: u32,
    /// Reference to a credential in the credential store, if the check needs one.
    #[serde(default)]
    pub credential_ref: Option<Uuid>,
}

impl PollJob {
    /// A new ICMP poll job for `node` at `target`.
    #[must_use]
    pub fn icmp(
        job_id: Uuid,
        node_id: NodeId,
        target: IpAddr,
        check: IcmpCheck,
        interval_secs: u32,
    ) -> Self {
        Self {
            schema_version: BUS_SCHEMA_VERSION,
            job_id,
            node_id,
            target,
            check: CheckSpec::Icmp(check),
            interval_secs,
            credential_ref: None,
        }
    }

    /// A new SNMP v2c poll job for `node` at `target`.
    #[must_use]
    pub fn snmp(
        job_id: Uuid,
        node_id: NodeId,
        target: IpAddr,
        check: SnmpCheck,
        interval_secs: u32,
    ) -> Self {
        Self {
            schema_version: BUS_SCHEMA_VERSION,
            job_id,
            node_id,
            target,
            check: CheckSpec::Snmp(check),
            interval_secs,
            credential_ref: None,
        }
    }

    /// A new SNMP v3 poll job for `node` at `target`.
    #[must_use]
    pub fn snmp_v3(
        job_id: Uuid,
        node_id: NodeId,
        target: IpAddr,
        check: SnmpV3Check,
        interval_secs: u32,
    ) -> Self {
        Self {
            schema_version: BUS_SCHEMA_VERSION,
            job_id,
            node_id,
            target,
            check: CheckSpec::SnmpV3(check),
            interval_secs,
            credential_ref: None,
        }
    }

    /// A new SNMP v2c table-walk poll job for `node` at `target`.
    #[must_use]
    pub fn snmp_table(
        job_id: Uuid,
        node_id: NodeId,
        target: IpAddr,
        check: SnmpTableCheck,
        interval_secs: u32,
    ) -> Self {
        Self {
            schema_version: BUS_SCHEMA_VERSION,
            job_id,
            node_id,
            target,
            check: CheckSpec::SnmpTable(check),
            interval_secs,
            credential_ref: None,
        }
    }
}

/// What kind of check to run. Tagged so new protocols can be added without breaking
/// older consumers (they ignore unknown fields; an unknown *tag* is skipped by the
/// poller's malformed-message handling, so old pollers simply ignore newer check kinds).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CheckSpec {
    /// Liveness/RTT via ICMP echo.
    Icmp(IcmpCheck),
    /// Scalar SNMP v2c GET of a set of OIDs.
    Snmp(SnmpCheck),
    /// Scalar SNMP v3 (USM) GET of a set of OIDs.
    SnmpV3(SnmpV3Check),
    /// SNMP v2c GETBULK walk of table columns (per-interface metrics + metadata).
    /// A new variant: older pollers that don't know this tag simply skip the job
    /// (the poller's malformed-message handling), preserving N-1 compatibility.
    SnmpTable(SnmpTableCheck),
    // Http(...) lands in a later phase.
}

/// ICMP echo parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IcmpCheck {
    /// Number of echo requests to send.
    pub count: u8,
    /// Per-request timeout, in milliseconds.
    pub timeout_ms: u32,
}

impl Default for IcmpCheck {
    fn default() -> Self {
        Self {
            count: 3,
            timeout_ms: 1000,
        }
    }
}

/// SNMP v2c check parameters. The community is the resolved credential, inlined by core
/// over the (TLS) bus at send time (ADR-018/020); the poller never reads the secret store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpCheck {
    /// SNMP v2c community string (resolved/decrypted by core).
    pub community: String,
    /// OIDs to GET (dotted form, e.g. `1.3.6.1.2.1.1.3.0`).
    pub oids: Vec<String>,
    /// Per-request timeout, in milliseconds.
    #[serde(default = "default_snmp_timeout_ms")]
    pub timeout_ms: u32,
}

const fn default_snmp_timeout_ms() -> u32 {
    2000
}

/// SNMP v3 (USM) check parameters. Auth/priv keys are resolved/decrypted by core and
/// inlined here (ADR-018/020); the poller never reads the secret store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpV3Check {
    /// USM user name.
    pub user: String,
    /// `noauth` | `auth` | `authpriv`.
    pub security_level: String,
    /// Auth protocol (`md5` | `sha`), if `security_level` is auth/authpriv.
    #[serde(default)]
    pub auth_protocol: Option<String>,
    /// Auth passphrase.
    #[serde(default)]
    pub auth_key: Option<String>,
    /// Privacy protocol (`des` | `aes`), if `security_level` is authpriv.
    #[serde(default)]
    pub priv_protocol: Option<String>,
    /// Privacy passphrase.
    #[serde(default)]
    pub priv_key: Option<String>,
    /// OIDs to GET.
    pub oids: Vec<String>,
    /// Per-request timeout, in milliseconds.
    #[serde(default = "default_snmp_timeout_ms")]
    pub timeout_ms: u32,
}

/// SNMP v2c table-walk parameters. Each numeric column base is walked with GETBULK to
/// yield one sample per interface (keyed by ifIndex); the metadata columns populate the
/// interface inventory and never become TSDB series (ADR-011). The community is the
/// resolved credential, inlined by core (ADR-018/020) — the poller never reads the store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpTableCheck {
    /// SNMP v2c community string (resolved/decrypted by core).
    pub community: String,
    /// Numeric table columns → per-interface TSDB samples.
    pub columns: Vec<SnmpColumn>,
    /// Interface-metadata columns (ifName/ifAlias/ifSpeed) → interface inventory, not TSDB.
    #[serde(default)]
    pub meta_columns: Vec<SnmpMetaColumn>,
    /// Per-request timeout, in milliseconds.
    #[serde(default = "default_snmp_timeout_ms")]
    pub timeout_ms: u32,
}

/// One numeric table column to walk: its stable metric name, the column base OID, and
/// whether the values are gauges or raw counters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpColumn {
    /// Stable TSDB metric name (e.g. `if_hc_in_octets`). Bounded by convention (ADR-011).
    pub metric_name: String,
    /// Column base OID (the walk root), e.g. `1.3.6.1.2.1.31.1.1.1.6`.
    pub oid: String,
    /// Gauge vs raw counter (rates derived at query time, ADR-012).
    #[serde(default = "default_metric_kind")]
    pub kind: MetricKind,
}

const fn default_metric_kind() -> MetricKind {
    MetricKind::Gauge
}

/// One interface-metadata column to walk: which interface field it populates and the
/// column base OID. The value is descriptive (PostgreSQL), never a TSDB label.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpMetaColumn {
    /// Which interface attribute this column carries.
    pub field: InterfaceField,
    /// Column base OID, e.g. `1.3.6.1.2.1.31.1.1.1.1` (ifName).
    pub oid: String,
}

/// The result of executing a [`PollJob`], sent back to core.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PollResult {
    /// Message schema version (defaulted for forward-compat).
    #[serde(default = "default_version")]
    pub schema_version: u16,
    /// The job this answers.
    pub job_id: Uuid,
    /// Node that was polled.
    pub node_id: NodeId,
    /// When the poll completed, as Unix time in milliseconds (UTC).
    pub at_unix_ms: i64,
    /// High-level reachability outcome.
    pub outcome: CheckOutcome,
    /// Collected metric samples (raw values; rates are derived later, ADR-012).
    #[serde(default)]
    pub samples: Vec<Sample>,
    /// Interfaces discovered on this poll (table walks only). Descriptive metadata that
    /// core upserts into the interface inventory; empty for non-table checks. Defaulted so
    /// an older poller that doesn't send it stays N-1 compatible (ADR-017).
    #[serde(default)]
    pub interfaces: Vec<DiscoveredInterface>,
}

/// An interface discovered during a table walk: its index and the descriptive metadata
/// columns. Joined to per-interface metrics at query time; never a TSDB label (ADR-011).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoveredInterface {
    /// Interface index (the table row key).
    pub ifindex: IfIndex,
    /// `ifName`, if walked.
    #[serde(default)]
    pub if_name: Option<String>,
    /// `ifAlias`, if walked.
    #[serde(default)]
    pub if_alias: Option<String>,
    /// Line rate in bits/sec, if walked.
    #[serde(default)]
    pub if_speed: Option<i64>,
}

/// High-level outcome of a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    /// Target responded.
    Reachable,
    /// Target did not respond within the timeout.
    Unreachable,
    /// The check could not be run (transport error, bad config).
    Error,
}

/// One collected metric value. `metric`+`ifindex` form the thin-label identity once
/// combined with the result's node (ADR-011); rates are not computed here (ADR-012).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Stable metric name (e.g. `icmp_rtt_ms`, `if_in_octets`).
    pub metric: String,
    /// Interface index for per-interface metrics; `None` for node-level.
    #[serde(default)]
    pub ifindex: Option<IfIndex>,
    /// Raw value.
    pub value: f64,
    /// Whether this is a gauge or a (raw) counter.
    pub kind: MetricKind,
}

impl Sample {
    /// A node-level gauge sample.
    #[must_use]
    pub fn gauge(metric: impl Into<String>, value: f64) -> Self {
        Self {
            metric: metric.into(),
            ifindex: None,
            value,
            kind: MetricKind::Gauge,
        }
    }

    /// A node-level raw-counter sample (rates derived at query time, ADR-012).
    #[must_use]
    pub fn counter(metric: impl Into<String>, value: f64) -> Self {
        Self {
            metric: metric.into(),
            ifindex: None,
            value,
            kind: MetricKind::Counter,
        }
    }

    /// A per-interface sample of the given kind (gauge or raw counter).
    #[must_use]
    pub fn interface(
        metric: impl Into<String>,
        ifindex: IfIndex,
        value: f64,
        kind: MetricKind,
    ) -> Self {
        Self {
            metric: metric.into(),
            ifindex: Some(ifindex),
            value,
            kind,
        }
    }

    /// The thin-label series identity for this sample under `node`.
    #[must_use]
    pub fn series_key(&self, node: NodeId) -> SeriesKey {
        match self.ifindex {
            Some(idx) => SeriesKey::interface(node, idx, self.metric.as_str()),
            None => SeriesKey::node(node, self.metric.as_str()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn sample_job() -> PollJob {
        PollJob::icmp(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IcmpCheck::default(),
            30,
        )
    }

    #[test]
    fn job_round_trips_through_json() {
        let job = sample_job();
        let json = serde_json::to_string(&job).unwrap();
        let back: PollJob = serde_json::from_str(&json).unwrap();
        assert_eq!(job, back);
    }

    #[test]
    fn unknown_fields_and_missing_version_are_tolerated() {
        // Simulates an older producer (no schema_version) that also a newer one added a
        // field we don't know — ADR-017 forward/backward compatibility.
        let json = r#"{
            "job_id": "00000000-0000-0000-0000-000000000000",
            "node_id": "00000000-0000-0000-0000-000000000000",
            "target": "10.0.0.1",
            "check": { "kind": "icmp", "count": 3, "timeout_ms": 1000 },
            "interval_secs": 30,
            "future_field": "ignored"
        }"#;
        let job: PollJob = serde_json::from_str(json).unwrap();
        assert_eq!(job.schema_version, BUS_SCHEMA_VERSION); // defaulted
        assert_eq!(job.interval_secs, 30);
    }

    #[test]
    fn snmp_table_job_round_trips_with_snake_case_tag() {
        let job = PollJob::snmp_table(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)),
            SnmpTableCheck {
                community: "public".into(),
                columns: vec![SnmpColumn {
                    metric_name: "if_hc_in_octets".into(),
                    oid: "1.3.6.1.2.1.31.1.1.1.6".into(),
                    kind: MetricKind::Counter,
                }],
                meta_columns: vec![SnmpMetaColumn {
                    field: InterfaceField::Name,
                    oid: "1.3.6.1.2.1.31.1.1.1.1".into(),
                }],
                timeout_ms: 2000,
            },
            60,
        );
        let json = serde_json::to_string(&job).unwrap();
        assert!(json.contains("\"kind\":\"snmp_table\""));
        let back: PollJob = serde_json::from_str(&json).unwrap();
        assert_eq!(job, back);
    }

    #[test]
    fn snmp_column_kind_defaults_to_gauge_when_absent() {
        // Forward-compat: a column without an explicit kind defaults rather than failing.
        let col: SnmpColumn = serde_json::from_str(r#"{"metric_name":"x","oid":"1.2.3"}"#).unwrap();
        assert_eq!(col.kind, MetricKind::Gauge);
    }

    #[test]
    fn poll_result_without_interfaces_defaults_empty() {
        // N-1: an older poller's PollResult has no `interfaces` field — new core must
        // default it to empty rather than failing to deserialize (ADR-017).
        let json = r#"{
            "schema_version": 1,
            "job_id": "00000000-0000-0000-0000-000000000000",
            "node_id": "00000000-0000-0000-0000-000000000000",
            "at_unix_ms": 0,
            "outcome": "reachable",
            "samples": []
        }"#;
        let result: PollResult = serde_json::from_str(json).unwrap();
        assert!(result.interfaces.is_empty());
    }

    #[test]
    fn sample_counter_and_interface_helpers_set_kind_and_ifindex() {
        let c = Sample::counter("if_hc_in_octets", 5.0);
        assert_eq!(c.kind, MetricKind::Counter);
        assert_eq!(c.ifindex, None);

        let i = Sample::interface("if_hc_in_octets", IfIndex(4), 9.0, MetricKind::Counter);
        assert_eq!(i.ifindex, Some(IfIndex(4)));
        assert_eq!(i.kind, MetricKind::Counter);
    }

    #[test]
    fn sample_builds_thin_label_series_key() {
        let node = NodeId::from(Uuid::nil());
        let s = Sample::gauge("icmp_rtt_ms", 12.5);
        assert!(!s.series_key(node).is_interface_scoped());

        let iface = Sample {
            metric: "if_in_octets".into(),
            ifindex: Some(IfIndex(3)),
            value: 1000.0,
            kind: MetricKind::Counter,
        };
        let key = iface.series_key(node);
        assert!(key.is_interface_scoped());
        assert_eq!(key.ifindex, Some(IfIndex(3)));
    }
}
