//! Bus message contract between core (Yagra-core) and pollers (Yagra-poller).
//!
//! These are the *only* way core and pollers talk (ADR-003). Messages are
//! **version-tolerant** (ADR-017): every message carries `schema_version`, new fields
//! are added as `#[serde(default)]`, and unknown fields are ignored (we never use
//! `deny_unknown_fields`). That is what lets a new core run against an old poller, and
//! vice versa, during a rolling upgrade.

use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr};
use uuid::Uuid;
use yagra_common::{
    ExpectedStatus, HttpMethod, IfIndex, InterfaceField, MerakiTier, MetricKind, NodeId, SeriesKey,
};

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
    /// Ask the poller to also fetch `sysDescr.0` on this poll so core can classify the device's
    /// maker/model (set by core only while a node's vendor is still blank). Honoured for the
    /// scalar SNMP checks. Defaulted for N-1 compatibility (ADR-017): an older poller ignores it
    /// and simply never probes identity.
    #[serde(default)]
    pub probe_identity: bool,
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
            probe_identity: false,
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
            probe_identity: false,
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
            probe_identity: false,
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
            probe_identity: false,
        }
    }

    /// A new HTTP/HTTPS URL-monitor poll job for `node`. The real request target is the
    /// `check.url`; `target` carries the node's management IP (display / optional ICMP).
    #[must_use]
    pub fn http(
        job_id: Uuid,
        node_id: NodeId,
        target: IpAddr,
        check: HttpCheck,
        interval_secs: u32,
    ) -> Self {
        Self {
            schema_version: BUS_SCHEMA_VERSION,
            job_id,
            node_id,
            target,
            check: CheckSpec::Http(check),
            interval_secs,
            credential_ref: None,
            probe_identity: false,
        }
    }

    /// A new Meraki org-scoped collector job. Unlike the per-node checks above, one collect job
    /// pages the org-bulk Dashboard endpoints for a whole organization and fans the result out to
    /// many nodes (the poller emits one [`PollResult`] per device). `node_id`/`target` are
    /// therefore sentinels: `node_id` carries the internal org handle (correlation / single-flight
    /// clear) and `target` is unspecified (the collector resolves `check.base_url`).
    #[must_use]
    pub fn meraki_collect(job_id: Uuid, check: MerakiCollectCheck, interval_secs: u32) -> Self {
        let org_handle = NodeId::from(check.meraki_org_uuid);
        Self {
            schema_version: BUS_SCHEMA_VERSION,
            job_id,
            node_id: org_handle,
            target: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            check: CheckSpec::MerakiCollect(check),
            interval_secs,
            credential_ref: None,
            probe_identity: false,
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
    /// HTTP/HTTPS URL-endpoint check (status/up + TLS cert expiry). Like the variants above,
    /// an older poller that doesn't know this tag simply skips the job (N-1 compatible).
    Http(HttpCheck),
    /// Cisco Meraki org-scoped collector: page one tier of Dashboard org-bulk endpoints and fan
    /// the result out to many nodes. Read-only (GET). Like the variants above, an older poller that
    /// doesn't know this tag simply skips the job (N-1 compatible).
    MerakiCollect(MerakiCollectCheck),
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

/// HTTP/HTTPS URL-endpoint check parameters. The `url` is the actual request target (the
/// enclosing [`PollJob::target`] stays the node's management IP, for display / optional ICMP).
/// Auth lands later (resolved/inlined by core, ADR-018/020); the MVP probe is unauthenticated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HttpCheck {
    /// Full URL to probe, e.g. `https://api.example.com/health`.
    pub url: String,
    /// Request method (default `GET`).
    #[serde(default)]
    pub method: HttpMethod,
    /// Which status codes count as healthy (default: any 2xx).
    #[serde(default)]
    pub expected_status: ExpectedStatus,
    /// Verify the TLS certificate chain (default `true`).
    #[serde(default = "default_true")]
    pub verify_tls: bool,
    /// Follow 3xx redirects (default `true`).
    #[serde(default = "default_true")]
    pub follow_redirects: bool,
    /// Per-request timeout, in milliseconds.
    #[serde(default = "default_http_timeout_ms")]
    pub timeout_ms: u32,
}

const fn default_http_timeout_ms() -> u32 {
    5000
}

/// SNMP v2c check parameters. The community is the resolved credential, inlined by core
/// over the (TLS) bus at send time (ADR-018/020); the poller never reads the secret store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpCheck {
    /// SNMP v2c community string (resolved/decrypted by core).
    pub community: String,
    /// Bare OIDs to GET (dotted form, e.g. `1.3.6.1.2.1.1.3.0`). The poller names these via
    /// its built-in OID→metric map (legacy / env-configured path).
    pub oids: Vec<String>,
    /// Scalar OIDs to GET *with an explicit metric name and kind*. Used for configured
    /// collection sets so a node's chosen scalar metric names are honoured (rather than the
    /// poller's built-in naming). Defaulted for N-1 compatibility (ADR-017).
    #[serde(default)]
    pub columns: Vec<SnmpColumn>,
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
    /// Scalar OIDs to GET *with an explicit metric name and kind* (configured collection
    /// sets — mirrors [`SnmpCheck::columns`]). Defaulted for N-1 compatibility (ADR-017).
    #[serde(default)]
    pub columns: Vec<SnmpColumn>,
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

/// Cisco Meraki org-scoped collector parameters. One job pages one [`MerakiTier`] of the org's
/// Dashboard API and yields per-device samples. Strictly **read-only** (the poller issues GET only).
///
/// `api_key` is the resolved credential, inlined by core over the (TLS) bus at send time
/// (ADR-018/020) — the poller never reads the secret store. It is sent only to hosts matching
/// [`yagra_common::is_meraki_api_host`] (validated on every request incl. pagination links) so it
/// cannot be exfiltrated. `devices` is the serial→node_id map (built from `meraki_devices`) the
/// stateless poller needs to attribute each API row to a node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MerakiCollectCheck {
    /// The Meraki organizationId (the API path segment).
    pub org_id: String,
    /// Internal handle of the owning `meraki_orgs` row (correlation / single-flight clear).
    pub meraki_org_uuid: Uuid,
    /// Which tier of endpoints to page this cycle.
    pub tier: MerakiTier,
    /// Dashboard API base URL (regional shard); host-allow-listed by the collector.
    pub base_url: String,
    /// Resolved read-only API key (decrypted by core; never logged).
    pub api_key: String,
    /// serial → node_id map for this org (only in-scope devices; empty ⇒ nothing to attribute).
    #[serde(default)]
    pub devices: Vec<MerakiDeviceRef>,
    /// Meraki networkIds in scope (narrows API calls where supported; empty ⇒ all).
    #[serde(default)]
    pub network_ids: Vec<String>,
    /// Page size cap for paginated endpoints.
    #[serde(default = "default_meraki_per_page")]
    pub per_page: u32,
    /// Conservative request-rate budget (requests/sec) the collector paces itself to — well under
    /// the org cap so the customer's own tooling keeps headroom (safeguard).
    #[serde(default = "default_meraki_target_rps")]
    pub target_rps: f64,
    /// Overall per-request timeout, in milliseconds.
    #[serde(default = "default_meraki_timeout_ms")]
    pub timeout_ms: u32,
}

const fn default_meraki_per_page() -> u32 {
    1000
}

fn default_meraki_target_rps() -> f64 {
    2.0
}

const fn default_meraki_timeout_ms() -> u32 {
    30_000
}

/// One serial → node_id mapping inlined into a [`MerakiCollectCheck`] so the stateless poller can
/// attribute each org-bulk API row (keyed by serial) to the right node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MerakiDeviceRef {
    /// Device serial (the join key from the org-bulk endpoints).
    pub serial: String,
    /// The Yagra node representing that device.
    pub node_id: NodeId,
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
    /// The device's SNMP `sysDescr.0`, if this poll was asked to probe identity (`probe_identity`).
    /// Core classifies it (maker/model) and fills the node's blank vendor/model. Descriptive
    /// device text — never a TSDB label. Defaulted so an older poller stays N-1 compatible.
    #[serde(default)]
    pub sys_descr: Option<String>,
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

// ── Discovery (Phase C) — a separate job/result pair on its own subjects ────────────

/// A discovery sweep request: probe each target for ICMP liveness + SNMP identity (sysDescr /
/// sysName), trying the candidate credentials and communities. Runs on the poller (it has
/// raw-socket ICMP).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryJob {
    #[serde(default = "default_version")]
    pub schema_version: u16,
    /// Correlates the result back to the originating scan.
    pub scan_id: Uuid,
    /// Addresses to probe (IPv4 or IPv6).
    pub targets: Vec<IpAddr>,
    /// Candidate SNMP v2c communities to try; the first that answers wins.
    #[serde(default)]
    pub communities: Vec<String>,
    /// Candidate stored credentials (v2c or v3), resolved/decrypted by core and inlined
    /// (ADR-018/020). Tried before the ad-hoc `communities`; the first that answers wins
    /// and its `cred_ref` is echoed back so import can bind it by reference. Defaulted for
    /// N-1 compatibility (an older poller ignores this field and uses `communities` only).
    #[serde(default)]
    pub credentials: Vec<DiscoveryCredential>,
    /// Per-probe timeout in milliseconds.
    #[serde(default = "default_snmp_timeout_ms")]
    pub timeout_ms: u32,
}

/// One candidate credential for a discovery sweep. Exactly one of `community` / `v3` is
/// set (kind-dependent); the secret is resolved/decrypted by core and inlined over the
/// bus (ADR-018/020) — the poller never reads the secret store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryCredential {
    /// Credential-store id, echoed back on a match (the matched credential is bound to
    /// the imported node **by reference**, never by value — security.md).
    pub cred_ref: Uuid,
    /// SNMP v2c community (non-v3 credential kinds).
    #[serde(default)]
    pub community: Option<String>,
    /// SNMP v3 USM parameters (`snmp_v3` credential kind).
    #[serde(default)]
    pub v3: Option<DiscoveryV3>,
}

/// SNMP v3 USM parameters for a discovery probe (mirrors [`SnmpV3Check`]'s tokens:
/// `security_level` ∈ noauth|auth|authpriv, lowercase protocol tokens).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryV3 {
    pub user: String,
    pub security_level: String,
    #[serde(default)]
    pub auth_protocol: Option<String>,
    #[serde(default)]
    pub auth_key: Option<String>,
    #[serde(default)]
    pub priv_protocol: Option<String>,
    #[serde(default)]
    pub priv_key: Option<String>,
}

/// One device found by a discovery sweep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoveredDevice {
    /// The probed address.
    pub address: IpAddr,
    /// Whether it answered ICMP.
    pub reachable: bool,
    /// `sysDescr.0` if it answered SNMP (device-supplied — treat as untrusted).
    #[serde(default)]
    pub sysdescr: Option<String>,
    /// `sysName.0` if it answered SNMP.
    #[serde(default)]
    pub sysname: Option<String>,
    /// `sysObjectID.0` if it answered SNMP — the vendor-assigned enterprise OID that
    /// authoritatively identifies the device type (e.g. `1.3.6.1.4.1.9.1.516`). Preferred
    /// over the free-form `sysdescr` for profile classification. `None` for an older poller
    /// that didn't probe it (ADR-017 N-1: core falls back to `sysdescr`).
    #[serde(default)]
    pub sysobjectid: Option<String>,
    /// The stored credential that answered SNMP, by reference (never the value). `None`
    /// when an ad-hoc community matched or nothing answered.
    #[serde(default)]
    pub matched_credential: Option<Uuid>,
}

/// A (possibly partial) result of one [`DiscoveryJob`]. The poller publishes progress as
/// it sweeps: each message carries the **cumulative** `found` list so any single message
/// is a complete snapshot (an older core that treats the first message as final still
/// converges on correct data — ADR-017).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryResult {
    #[serde(default = "default_version")]
    pub schema_version: u16,
    pub scan_id: Uuid,
    /// All devices found so far (cumulative).
    pub found: Vec<DiscoveredDevice>,
    /// Targets probed so far. Defaults to 0 for an older poller's single final message.
    #[serde(default)]
    pub probed: u32,
    /// Total targets in the sweep.
    #[serde(default)]
    pub total: u32,
    /// Whether the sweep has finished. Defaults to **true** so an older poller's single
    /// result message still completes the scan (ADR-017).
    #[serde(default = "default_true")]
    pub done: bool,
}

const fn default_true() -> bool {
    true
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
        assert!(!job.probe_identity); // N-1: absent identity-probe flag defaults off
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
    fn meraki_collect_job_round_trips_with_snake_case_tag() {
        let job = PollJob::meraki_collect(
            Uuid::nil(),
            MerakiCollectCheck {
                org_id: "123456".into(),
                meraki_org_uuid: Uuid::nil(),
                tier: MerakiTier::Uplink,
                base_url: "https://api.meraki.com".into(),
                api_key: "REDACTED".into(),
                devices: vec![MerakiDeviceRef {
                    serial: "Q2XX-XXXX-XXXX".into(),
                    node_id: NodeId::from(Uuid::nil()),
                }],
                network_ids: vec!["N_1".into()],
                per_page: 1000,
                target_rps: 2.0,
                timeout_ms: 30_000,
            },
            300,
        );
        let json = serde_json::to_string(&job).unwrap();
        assert!(json.contains("\"kind\":\"meraki_collect\""));
        assert!(json.contains("\"tier\":\"uplink\""));
        let back: PollJob = serde_json::from_str(&json).unwrap();
        assert_eq!(job, back);
        // Sentinels: node_id carries the org handle; target is unspecified (the collector uses
        // check.base_url, not this address).
        assert_eq!(back.target, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    }

    #[test]
    fn meraki_collect_check_defaults_are_forward_compatible() {
        // A producer that omits the newer optional fields still deserializes with safe defaults.
        let json = r#"{
            "org_id":"123456",
            "meraki_org_uuid":"00000000-0000-0000-0000-000000000000",
            "tier":"availability",
            "base_url":"https://api.meraki.com",
            "api_key":"x"
        }"#;
        let c: MerakiCollectCheck = serde_json::from_str(json).unwrap();
        assert!(c.devices.is_empty());
        assert!(c.network_ids.is_empty());
        assert_eq!(c.per_page, 1000);
        assert_eq!(c.target_rps, 2.0);
        assert_eq!(c.timeout_ms, 30_000);
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
        assert!(result.sys_descr.is_none()); // N-1: older poller sends no sysDescr
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
    fn discovery_job_with_credentials_round_trips() {
        let job = DiscoveryJob {
            schema_version: BUS_SCHEMA_VERSION,
            scan_id: Uuid::nil(),
            targets: vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))],
            communities: vec!["public".into()],
            credentials: vec![
                DiscoveryCredential {
                    cred_ref: Uuid::nil(),
                    community: Some("secret-community".into()),
                    v3: None,
                },
                DiscoveryCredential {
                    cred_ref: Uuid::nil(),
                    community: None,
                    v3: Some(DiscoveryV3 {
                        user: "monitor".into(),
                        security_level: "authpriv".into(),
                        auth_protocol: Some("sha256".into()),
                        auth_key: Some("a-pass".into()),
                        priv_protocol: Some("aes256".into()),
                        priv_key: Some("p-pass".into()),
                    }),
                },
            ],
            timeout_ms: 2000,
        };
        let json = serde_json::to_string(&job).unwrap();
        let back: DiscoveryJob = serde_json::from_str(&json).unwrap();
        assert_eq!(job, back);
    }

    #[test]
    fn old_discovery_messages_default_new_fields() {
        // N-1 (ADR-017): an older core's job has no `credentials`; an older poller's
        // single result has no progress fields — it must read as a *final* snapshot.
        let job: DiscoveryJob = serde_json::from_str(
            r#"{
                "scan_id": "00000000-0000-0000-0000-000000000000",
                "targets": ["10.0.0.1"],
                "communities": ["public"]
            }"#,
        )
        .unwrap();
        assert!(job.credentials.is_empty());

        let result: DiscoveryResult = serde_json::from_str(
            r#"{
                "scan_id": "00000000-0000-0000-0000-000000000000",
                "found": [{"address": "10.0.0.1", "reachable": true}]
            }"#,
        )
        .unwrap();
        assert!(result.done, "missing done must default to true");
        assert_eq!(result.probed, 0);
        assert_eq!(result.found[0].matched_credential, None);
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
