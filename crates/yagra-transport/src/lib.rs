//! Yagra-transport — device I/O abstraction over ICMP / SNMP / HTTP.
//!
//! All device I/O goes through the [`Transport`] trait so pollers and discovery never
//! speak a raw protocol directly (ADR / coding-conventions). Protocol differences, raw
//! counter collection, and rate-limiting/backpressure live behind it. Crucially, rate
//! and utilization are **not** computed here — pollers store raw counters and rates are
//! derived at query/eval time (ADR-012), so this layer is stateless per device.
//!
//! Phase 1 covers ICMP; SNMP/HTTP land behind the same trait. The real ICMP transport
//! (raw sockets via `surge-ping`, needs `CAP_NET_RAW`) is a later drop-in; [`FakeTransport`]
//! lets the poller and the walking skeleton be exercised without privileges or a device.

use async_trait::async_trait;
use std::net::IpAddr;
use std::time::Duration;
use thiserror::Error;

mod icmp;
mod snmp;
mod snmp_v3;
pub use icmp::{summarize, SurgePingTransport};

/// Outcome of an ICMP probe. Raw observations only — no derived rates.
#[derive(Debug, Clone, PartialEq)]
pub struct IcmpProbe {
    /// Whether the target responded at least once.
    pub reachable: bool,
    /// Mean round-trip time in milliseconds, if any reply was received.
    pub rtt_ms: Option<f64>,
    /// Packet loss percentage over the probe (0.0–100.0).
    pub loss_pct: f64,
}

/// One numeric SNMP value: the OID it came from and its value as `f64`. Non-numeric
/// OIDs (strings, etc.) are skipped — only countable/gaugeable values become metrics.
/// Counters are reported **raw** (rates are derived at query time, ADR-012).
#[derive(Debug, Clone, PartialEq)]
pub struct SnmpSample {
    /// The dotted OID, e.g. `1.3.6.1.2.1.1.3.0`.
    pub oid: String,
    /// The value as a float (counters lose no range at MVP magnitudes).
    pub value: f64,
}

/// One numeric value from a table walk: the column base it was walked from, the row's
/// index (the trailing sub-identifier — the ifIndex), and the raw value. The caller maps
/// `oid_base` back to a metric name (cardinality stays bounded that way).
#[derive(Debug, Clone, PartialEq)]
pub struct SnmpTableSample {
    /// The column base OID that was walked, e.g. `1.3.6.1.2.1.31.1.1.1.6`.
    pub oid_base: String,
    /// Row index (ifIndex) — the trailing sub-identifier of the instance OID.
    pub ifindex: u32,
    /// Raw value (counters reported as-is, ADR-012).
    pub value: f64,
}

/// One string value from a table walk (e.g. `ifName`/`ifAlias`): the column base, the
/// row's ifIndex, and the value. Used for interface *metadata* (PostgreSQL), never TSDB.
#[derive(Debug, Clone, PartialEq)]
pub struct SnmpTableString {
    /// The column base OID that was walked, e.g. `1.3.6.1.2.1.31.1.1.1.1`.
    pub oid_base: String,
    /// Row index (ifIndex).
    pub ifindex: u32,
    /// The string value (lossily decoded UTF-8; device-supplied, treat as untrusted).
    pub value: String,
}

/// SNMPv3 USM parameters (resolved/decrypted by core and inlined into the job). Keys are
/// the auth/priv passphrases. `security_level` is `noauth` / `auth` / `authpriv`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SnmpV3Params {
    pub user: String,
    pub security_level: String,
    pub auth_protocol: Option<String>,
    pub auth_key: Option<String>,
    pub priv_protocol: Option<String>,
    pub priv_key: Option<String>,
}

/// Errors performing device I/O.
#[derive(Debug, Error)]
pub enum TransportError {
    /// Underlying socket/IO failure.
    #[error("transport io error: {0}")]
    Io(String),
    /// The transport for this protocol is not yet implemented.
    #[error("transport not implemented: {0}")]
    Unimplemented(&'static str),
}

/// Abstraction over device access. Implementations: a real ICMP/SNMP/HTTP transport
/// (production) and [`FakeTransport`] (tests / skeleton).
#[async_trait]
pub trait Transport: Send + Sync {
    /// Send `count` ICMP echoes to `target`, each bounded by `timeout`, and report a
    /// single aggregated [`IcmpProbe`].
    async fn probe_icmp(
        &self,
        target: IpAddr,
        count: u8,
        timeout: Duration,
    ) -> Result<IcmpProbe, TransportError>;

    /// Fetch the given OIDs from `target` via SNMP v2c with `community`, returning the
    /// numeric values (non-numeric OIDs skipped). Counters are returned raw (ADR-012).
    async fn snmp_get(
        &self,
        target: IpAddr,
        community: &str,
        oids: &[String],
        timeout: Duration,
    ) -> Result<Vec<SnmpSample>, TransportError>;

    /// Fetch OIDs via SNMP v3 (USM). The USM crypto path is pending the net-snmp FFI
    /// decision (ADR-021): the real transport returns `Unimplemented` until then; the
    /// message + credential plumbing exists so v3 is representable end to end.
    async fn snmp_v3_get(
        &self,
        target: IpAddr,
        params: &SnmpV3Params,
        oids: &[String],
        timeout: Duration,
    ) -> Result<Vec<SnmpSample>, TransportError>;

    /// Walk one or more table *column base* OIDs via SNMP v2c GETBULK, returning the
    /// numeric value of every row, tagged with its ifIndex. A per-column walk failure is
    /// logged and skipped (one bad column doesn't fail the poll). Counters are raw (ADR-012).
    async fn snmp_walk(
        &self,
        target: IpAddr,
        community: &str,
        column_oids: &[String],
        timeout: Duration,
    ) -> Result<Vec<SnmpTableSample>, TransportError>;

    /// Walk table *column base* OIDs whose values are strings (e.g. `ifName`, `ifAlias`),
    /// returning each row's string value tagged with its ifIndex. For interface metadata
    /// (PostgreSQL), never TSDB labels (ADR-011).
    async fn snmp_walk_strings(
        &self,
        target: IpAddr,
        community: &str,
        column_oids: &[String],
        timeout: Duration,
    ) -> Result<Vec<SnmpTableString>, TransportError>;
}

/// A canned [`Transport`] for tests and the single-process walking skeleton.
///
/// Returns a fixed probe regardless of target, so poller logic can be exercised with no
/// raw-socket privilege and no real device.
#[derive(Debug, Clone)]
pub struct FakeTransport {
    /// The probe every ICMP call returns.
    pub probe: IcmpProbe,
    /// The samples every SNMP GET call returns.
    pub snmp: Vec<SnmpSample>,
    /// The numeric rows every SNMP table walk returns.
    pub snmp_table: Vec<SnmpTableSample>,
    /// The string rows every SNMP string-table walk returns.
    pub snmp_table_strings: Vec<SnmpTableString>,
}

impl FakeTransport {
    /// A fake that always reports the target reachable with the given RTT.
    #[must_use]
    pub fn reachable(rtt_ms: f64) -> Self {
        Self {
            probe: IcmpProbe {
                reachable: true,
                rtt_ms: Some(rtt_ms),
                loss_pct: 0.0,
            },
            snmp: Vec::new(),
            snmp_table: Vec::new(),
            snmp_table_strings: Vec::new(),
        }
    }

    /// A fake that always reports the target unreachable (100% loss).
    #[must_use]
    pub fn unreachable() -> Self {
        Self {
            probe: IcmpProbe {
                reachable: false,
                rtt_ms: None,
                loss_pct: 100.0,
            },
            snmp: Vec::new(),
            snmp_table: Vec::new(),
            snmp_table_strings: Vec::new(),
        }
    }

    /// Set the canned SNMP GET samples this fake returns.
    #[must_use]
    pub fn with_snmp(mut self, samples: Vec<SnmpSample>) -> Self {
        self.snmp = samples;
        self
    }

    /// Set the canned numeric table-walk rows this fake returns.
    #[must_use]
    pub fn with_snmp_table(mut self, rows: Vec<SnmpTableSample>) -> Self {
        self.snmp_table = rows;
        self
    }

    /// Set the canned string table-walk rows this fake returns.
    #[must_use]
    pub fn with_snmp_table_strings(mut self, rows: Vec<SnmpTableString>) -> Self {
        self.snmp_table_strings = rows;
        self
    }
}

#[async_trait]
impl Transport for FakeTransport {
    async fn probe_icmp(
        &self,
        _target: IpAddr,
        _count: u8,
        _timeout: Duration,
    ) -> Result<IcmpProbe, TransportError> {
        Ok(self.probe.clone())
    }

    async fn snmp_get(
        &self,
        _target: IpAddr,
        _community: &str,
        _oids: &[String],
        _timeout: Duration,
    ) -> Result<Vec<SnmpSample>, TransportError> {
        Ok(self.snmp.clone())
    }

    async fn snmp_v3_get(
        &self,
        _target: IpAddr,
        _params: &SnmpV3Params,
        _oids: &[String],
        _timeout: Duration,
    ) -> Result<Vec<SnmpSample>, TransportError> {
        Ok(self.snmp.clone())
    }

    async fn snmp_walk(
        &self,
        _target: IpAddr,
        _community: &str,
        column_oids: &[String],
        _timeout: Duration,
    ) -> Result<Vec<SnmpTableSample>, TransportError> {
        // Return only the rows for the requested columns, as a real per-column walk would.
        Ok(self
            .snmp_table
            .iter()
            .filter(|r| column_oids.iter().any(|c| c == &r.oid_base))
            .cloned()
            .collect())
    }

    async fn snmp_walk_strings(
        &self,
        _target: IpAddr,
        _community: &str,
        column_oids: &[String],
        _timeout: Duration,
    ) -> Result<Vec<SnmpTableString>, TransportError> {
        Ok(self
            .snmp_table_strings
            .iter()
            .filter(|r| column_oids.iter().any(|c| c == &r.oid_base))
            .cloned()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[tokio::test]
    async fn fake_reachable_reports_rtt_and_no_loss() {
        let t = FakeTransport::reachable(12.5);
        let p = t
            .probe_icmp(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                3,
                Duration::from_millis(1000),
            )
            .await
            .unwrap();
        assert!(p.reachable);
        assert_eq!(p.rtt_ms, Some(12.5));
        assert_eq!(p.loss_pct, 0.0);
    }

    #[tokio::test]
    async fn fake_unreachable_reports_full_loss() {
        let t = FakeTransport::unreachable();
        let p = t
            .probe_icmp(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                3,
                Duration::from_millis(1000),
            )
            .await
            .unwrap();
        assert!(!p.reachable);
        assert_eq!(p.rtt_ms, None);
        assert_eq!(p.loss_pct, 100.0);
    }
}
