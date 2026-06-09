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
use yagra_common::{IfIndex, MetricKind, NodeId, SeriesKey};

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
}

/// What kind of check to run. Tagged so new protocols can be added without breaking
/// older consumers (they ignore unknown tags / fields).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CheckSpec {
    /// Liveness/RTT via ICMP echo.
    Icmp(IcmpCheck),
    // Snmp(...) / Http(...) land in later phases.
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
