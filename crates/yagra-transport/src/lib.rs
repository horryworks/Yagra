// SPDX-License-Identifier: AGPL-3.0-only
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

mod dns;
mod http;
mod icmp;
mod meraki;
mod snmp;
mod snmp_v3;
pub use icmp::SurgePingTransport;
pub use meraki::{
    list_devices, list_networks, list_organizations, MerakiDeviceInfo, MerakiNetworkInfo,
    MerakiOrgInfo,
};

pub use yagra_common::{DnsChain, DnsRecordType, HttpAuth, HttpMethod, MerakiTier};

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

/// One string-valued SNMP scalar (e.g. `sysDescr.0`): the instance OID it came from and
/// its value. Used for device *identity* during discovery — metadata, never TSDB.
#[derive(Debug, Clone, PartialEq)]
pub struct SnmpStringSample {
    /// The dotted instance OID, e.g. `1.3.6.1.2.1.1.1.0`.
    pub oid: String,
    /// The string value (lossily decoded UTF-8; device-supplied, treat as untrusted).
    pub value: String,
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

/// One SNMP value as the agent typed it, for the walk path that must **not** coerce.
///
/// The two existing table walkers each collapse information on purpose: the numeric one drops
/// anything non-numeric, and the string one runs every value through lossy UTF-8. Both are right
/// for metrics and interface names, and both destroy an LLDP chassis id — `lldpRemChassisId` with
/// subtype `macAddress` is six raw bytes, and lossy decoding turns different MACs into the same
/// run of replacement characters. So this variant keeps the bytes, and the *caller* decides how to
/// read them (see `yagra_common::render_chassis_id`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnmpValue {
    /// Any integer-ish value (`INTEGER`, `Counter32/64`, `Unsigned32`, `TimeTicks`) — the subtype
    /// columns that say how to read the octet strings arrive this way.
    Int(i64),
    /// An `OCTET STRING`, verbatim. Device-supplied: treat as untrusted, and do not assume UTF-8.
    Bytes(Vec<u8>),
    /// An `OBJECT IDENTIFIER`, dotted decimal.
    Oid(String),
}

/// One row from a walk that preserves the **full instance index** and the raw value.
///
/// [`SnmpTableSample`] and [`SnmpTableString`] fold a multi-part instance into a synthetic
/// `ifindex` via [`ifindex_from_tail`], which is fine when rows are only aggregated but fatal here:
/// `lldpRemTable` is indexed by `(lldpRemTimeMark, lldpRemLocalPortNum, lldpRemIndex)` and the
/// adjacency's whole meaning lives in those sub-identifiers. Folding them would leave rows that
/// cannot be reassembled into which local port faces which peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnmpInstanceRow {
    /// The column base OID that was walked, e.g. `1.0.8802.1.1.2.1.4.1.1.5`.
    pub oid_base: String,
    /// Every sub-identifier past the column base, unfolded.
    pub instance: Vec<u32>,
    /// The value as the agent typed it.
    pub value: SnmpValue,
}

/// What a URL/HTTP(S) probe needs from the job (the non-secret request shape). The poller maps
/// a [`yagra_bus::HttpCheck`] into this; expected-status matching is applied poller-side, not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpProbeSpec {
    /// Full URL to request, e.g. `https://api.example.com/health`.
    pub url: String,
    /// Request method.
    pub method: HttpMethod,
    /// Verify the TLS certificate chain (default on; off only by operator opt-in).
    pub verify_tls: bool,
    /// Follow 3xx redirects.
    pub follow_redirects: bool,
    /// Credentials to present, already decrypted by core. `None` ⇒ an anonymous probe.
    pub auth: Option<HttpAuth>,
    /// Read at most this many bytes of the response body; `None` — the default for a monitor with
    /// no content rule — means **do not read the body at all** (ADR-047 Inc.2).
    ///
    /// Whether the captured prefix satisfies anything is decided poller-side, like the expected
    /// status: this layer reports raw observations (ADR-012). What it decides is only *how much* to
    /// pull, so one oversized page cannot cost a poller unbounded memory.
    pub body_capture_bytes: Option<u32>,
}

/// What a DNS name-resolution probe needs from the job (ADR-033). The poller maps a
/// [`yagra_bus::DnsCheck`] into this.
///
/// Unlike the other probes there is no separate result type: the probe returns
/// [`yagra_common::DnsChain`] directly, because the chain *is* the artifact — a transport-local
/// mirror would be a field-for-field copy the worker had to translate back for no benefit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsProbeSpec {
    /// The name to resolve, e.g. `horryworks.net`.
    pub name: String,
    /// Which record type the chain must reach.
    pub record_type: DnsRecordType,
    /// Recursive resolver to query; `None` ⇒ the poller container's system resolver.
    pub resolver: Option<std::net::SocketAddr>,
    /// Maximum CNAME hops before giving up.
    pub max_depth: u8,
}

/// Outcome of an HTTP/HTTPS probe. Raw observations only (ADR-012) — the poller derives
/// `http_up` from `reachable` + the expected-status match.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpProbe {
    /// Whether an HTTP response was received at all (any status counts).
    pub reachable: bool,
    /// The HTTP status code, if a response arrived.
    pub status_code: Option<u16>,
    /// Wall-clock request time in milliseconds.
    pub response_time_ms: f64,
    /// Days until the TLS server certificate expires (HTTPS only); `None` for plain HTTP or
    /// if the certificate couldn't be read.
    pub cert_days_to_expiry: Option<f64>,
    /// The prefix of the response body, when [`HttpProbeSpec::body_capture_bytes`] asked for one.
    /// `None` means the body was never read — not that it was empty.
    pub body: Option<BodyCapture>,
}

/// As much of a response body as the probe was allowed to read, and whether that was all of it.
///
/// **Deliberately does not derive `Debug`.** It rides inside [`HttpProbe`], which does derive it,
/// so a single `tracing::debug!(?probe)` would print up to a megabyte of a monitored endpoint's
/// response — which may hold session data, personal data, or a token the page happened to render.
/// The manual impl below reports the shape and never the content, exactly as
/// [`yagra_common::HttpAuth`] does for credentials.
#[derive(Clone, PartialEq, Eq)]
pub struct BodyCapture {
    /// The captured bytes as text, decoded lossily.
    ///
    /// Lossy on purpose: the cut lands on a byte boundary, so a multi-byte character straddling it
    /// becomes a replacement character. That is harmless for substring matching and is strictly
    /// better than discarding a whole capture over its last character. A body that is not text at
    /// all (an image, a binary blob) decodes to nonsense and simply matches nothing — which is the
    /// honest answer for a keyword rule pointed at a binary endpoint.
    pub text: String,
    /// `true` when the body was longer than the budget, **or** when reading it failed part-way.
    /// Both mean the same thing to a rule: bytes exist that were not examined, so an absent keyword
    /// proves nothing (see `yagra_common::BodyMatch::satisfied_by`).
    pub truncated: bool,
}

impl std::fmt::Debug for BodyCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The length and the truncation flag are the diagnostic facts — they answer "did the budget
        // stop us", which is the only question a log line here is ever asked. The bytes are not.
        f.debug_struct("BodyCapture")
            .field("len", &self.text.len())
            .field("truncated", &self.truncated)
            .field("text", &"<not logged>")
            .finish()
    }
}

/// What one Cisco Meraki org-scoped collect needs (the non-secret request shape plus the resolved
/// API key). Strictly **read-only**: the collector issues GET only, and every request host is
/// checked against [`yagra_common::is_meraki_api_host`] so the `api_key` can never leak off-host.
#[derive(Debug, Clone, PartialEq)]
pub struct MerakiCollectSpec {
    /// The Meraki organizationId (the API path segment).
    pub org_id: String,
    /// Dashboard API base URL (regional shard) — host-allow-listed before any request.
    pub base_url: String,
    /// Resolved read-only API key (decrypted by core; never logged).
    pub api_key: String,
    /// Which tier of endpoints to page this cycle.
    pub tier: MerakiTier,
    /// Networks in scope (narrows `networkIds[]` where supported; empty ⇒ all).
    pub network_ids: Vec<String>,
    /// Page-size cap for paginated endpoints.
    pub per_page: u32,
    /// Conservative request-rate budget (requests/sec) the collector paces itself to.
    pub target_rps: f64,
}

/// Raw per-device observations from a Meraki collect. The poller maps these to per-node
/// [`yagra_bus::PollResult`]s (thin-label gauge samples + uplink interface inventory). All Meraki
/// metrics are gauges (ADR-012 exception: the source pre-aggregates — no raw counters here).
#[derive(Debug, Clone, PartialEq)]
pub struct MerakiObservation {
    /// The device serial (the join key back to a node).
    pub serial: String,
    /// Metric samples for this device.
    pub samples: Vec<MerakiSample>,
    /// Uplinks seen (WAN1/WAN2/cellular) → the interface inventory (names for the UI).
    pub uplinks: Vec<MerakiUplink>,
}

/// One Meraki metric sample: a bounded metric name, an optional synthetic uplink ifindex, and a
/// gauge value.
#[derive(Debug, Clone, PartialEq)]
pub struct MerakiSample {
    /// Stable bounded metric name (e.g. `meraki_device_up`).
    pub metric: String,
    /// Synthetic uplink ifindex for per-uplink metrics; `None` for device-level.
    pub ifindex: Option<u32>,
    /// Gauge value.
    pub value: f64,
}

/// One Meraki uplink discovered on a collect: its synthetic ifindex and display name (WAN1/…).
/// Stored in the interface inventory so the UI can label per-uplink series (thin-label model).
#[derive(Debug, Clone, PartialEq)]
pub struct MerakiUplink {
    /// Synthetic ifindex (bounded — WAN1→1, WAN2→2, cellular→3).
    pub ifindex: u32,
    /// Display name (e.g. `WAN1`).
    pub name: String,
}

/// SNMPv3 USM parameters (resolved/decrypted by core and inlined into the job).
///
/// The name stays, the type does not: since ADR-084 this is [`yagra_common::SnmpV3Auth`], the one
/// definition the bus checks also carry. Keeping the local alias means every `SnmpV3Params` here
/// and in the poller reads the same as before, while `SnmpWalker::V3(check.auth.clone())` needs no
/// conversion — the walker and the job now speak the same type rather than two identical ones.
pub use yagra_common::SnmpV3Auth as SnmpV3Params;

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

    /// Fetch OIDs via SNMP v3 (USM) — pure-Rust `snmp2` backend (ADR-021). Counters are
    /// returned raw (ADR-012); auth/priv parameters come resolved from core (ADR-018/020).
    async fn snmp_v3_get(
        &self,
        target: IpAddr,
        params: &SnmpV3Params,
        oids: &[String],
        timeout: Duration,
    ) -> Result<Vec<SnmpSample>, TransportError>;

    /// Fetch string-valued scalar OIDs (e.g. `sysDescr.0` / `sysName.0`) via SNMP v3
    /// (USM). Non-string values are skipped. Used by discovery for device identity.
    async fn snmp_v3_get_strings(
        &self,
        target: IpAddr,
        params: &SnmpV3Params,
        oids: &[String],
        timeout: Duration,
    ) -> Result<Vec<SnmpStringSample>, TransportError>;

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

    /// Walk numeric table *column base* OIDs via SNMP v3 (USM) GETBULK — the v3 analogue of
    /// [`Transport::snmp_walk`], returning one numeric row per instance tagged with its ifIndex.
    /// A per-column walk failure is logged and skipped. Counters are raw (ADR-012); auth/priv
    /// come resolved from core (ADR-018/020) and are never logged.
    async fn snmp_v3_walk(
        &self,
        target: IpAddr,
        params: &SnmpV3Params,
        column_oids: &[String],
        timeout: Duration,
    ) -> Result<Vec<SnmpTableSample>, TransportError>;

    /// Walk string-valued table *column base* OIDs (e.g. `ifName`, `ifAlias`) via SNMP v3 (USM)
    /// GETBULK — the v3 analogue of [`Transport::snmp_walk_strings`]. For interface metadata
    /// (PostgreSQL), never TSDB labels (ADR-011).
    async fn snmp_v3_walk_strings(
        &self,
        target: IpAddr,
        params: &SnmpV3Params,
        column_oids: &[String],
        timeout: Duration,
    ) -> Result<Vec<SnmpTableString>, TransportError>;

    /// Walk table *column base* OIDs via SNMP v2c GETBULK, keeping each row's **full instance
    /// index** and its **raw** value (ADR-038). The neighbour walk needs both: `lldpRemTable`'s
    /// meaning is in its three-part index, and a chassis id is typed octets that lossy UTF-8 would
    /// destroy. A per-column walk failure is logged and skipped, as in the other walkers.
    ///
    /// `max_rows` bounds the **whole call**, across every column, and is enforced *while paging* —
    /// not by truncating the result. The distinction is the reason the parameter exists: a
    /// hundred-thousand-row ARP table has already cost its memory by the time a caller could
    /// truncate it (ADR-043 Increment 3).
    async fn snmp_walk_instances(
        &self,
        target: IpAddr,
        community: &str,
        column_oids: &[String],
        timeout: Duration,
        max_rows: usize,
    ) -> Result<Vec<SnmpInstanceRow>, TransportError>;

    /// The SNMP v3 (USM) analogue of [`Transport::snmp_walk_instances`]. Auth/priv come resolved
    /// from core (ADR-018/020) and are never logged.
    async fn snmp_v3_walk_instances(
        &self,
        target: IpAddr,
        params: &SnmpV3Params,
        column_oids: &[String],
        timeout: Duration,
        max_rows: usize,
    ) -> Result<Vec<SnmpInstanceRow>, TransportError>;

    /// Probe an HTTP/HTTPS URL endpoint: reachability + status code + response time, and (for
    /// HTTPS) the server certificate's days-to-expiry. A network failure is reported as
    /// `reachable = false` (an outage), not an `Err`; `Err` is for un-runnable configs only
    /// (bad URL/scheme, SSRF-blocked target).
    async fn probe_http(
        &self,
        spec: &HttpProbeSpec,
        timeout: Duration,
    ) -> Result<HttpProbe, TransportError>;

    /// Resolve a DNS name through one recursive resolver, walking the CNAME chain so every hop is
    /// observable (ADR-033), and return the chain as observed.
    ///
    /// A DNS-level failure (NXDOMAIN / SERVFAIL / REFUSED / timeout / malformed response / CNAME
    /// loop / depth exceeded) is reported **inside** the returned chain as `failure = Some(..)`,
    /// not as an `Err` — the same contract as [`Transport::probe_http`]. `Err` is reserved for a
    /// check that cannot be run at all: an unusable name, an SSRF-blocked resolver address, or no
    /// system resolver to fall back on. The returned chain is already canonicalized.
    async fn resolve_dns(
        &self,
        spec: &DnsProbeSpec,
        timeout: Duration,
    ) -> Result<DnsChain, TransportError>;

    /// Run one Cisco Meraki org-scoped collect: page the given tier's Dashboard org-bulk endpoints
    /// (**GET only**, host-allow-listed, paced to `spec.target_rps`, honouring 429/`Retry-After`)
    /// and return raw per-device observations. A transient network/5xx failure yields the partial
    /// results collected so far (best-effort, like the ICMP arm); `Err` is reserved for un-runnable
    /// configs (bad/blocked base URL) or an auth failure (401/403).
    async fn collect_meraki(
        &self,
        spec: &MerakiCollectSpec,
        timeout: Duration,
    ) -> Result<Vec<MerakiObservation>, TransportError>;
}

/// A canned [`Transport`] for tests and the single-process walking skeleton.
///
/// Returns a fixed probe regardless of target, so poller logic can be exercised with no
/// raw-socket privilege and no real device.
///
/// **Test-only.** Gated behind this crate's own `cfg(test)` and the `test-util` feature, so it is
/// not compiled into the shipped library — a transport that reports every device reachable has no
/// business being reachable from a release binary. Downstream crates opt in from
/// **dev-dependencies** (`yagra-transport = { …, features = ["test-util"] }`); with resolver v2 that
/// does not leak into a normal `cargo build` of the dependent.
#[cfg(any(test, feature = "test-util"))]
#[derive(Debug, Clone)]
pub struct FakeTransport {
    /// The probe every ICMP call returns.
    pub probe: IcmpProbe,
    /// The samples every SNMP GET call returns.
    pub snmp: Vec<SnmpSample>,
    /// The numeric rows every SNMP table walk returns.
    pub snmp_table: Vec<SnmpTableSample>,
    /// The string rows every SNMP string-table walk returns (v2c and v3 share this canned set).
    pub snmp_table_strings: Vec<SnmpTableString>,
    /// The raw-instance rows every instance walk returns (v2c and v3 share this canned set).
    pub snmp_instances: Vec<SnmpInstanceRow>,
    /// The string scalars every SNMP v3 string GET returns.
    pub snmp_v3_strings: Vec<SnmpStringSample>,
    /// The probe every HTTP call returns.
    pub http: HttpProbe,
    /// The observations every Meraki collect returns.
    pub meraki: Vec<MerakiObservation>,
    /// The chain every DNS resolution returns.
    pub dns: DnsChain,
    /// When set, every scalar SNMP GET (v2c and v3) fails with this message instead of
    /// returning [`Self::snmp`].
    ///
    /// "The agent refused" and "the agent answered with nothing" are different device states
    /// that a caller can easily conflate, and the empty-vec default can only express the second.
    pub snmp_get_error: Option<String>,
}

/// A canned one-hop chain resolving to `10.1.2.3`, or the same query having timed out.
#[cfg(any(test, feature = "test-util"))]
fn fake_dns_chain(resolved: bool) -> DnsChain {
    DnsChain {
        query: "example.test".to_owned(),
        record_type: DnsRecordType::A,
        resolver: "system".to_owned(),
        hops: if resolved {
            vec![yagra_common::DnsHop {
                name: "example.test".to_owned(),
                answers: vec![yagra_common::DnsAnswer {
                    record: yagra_common::DnsRecord::A {
                        addr: std::net::Ipv4Addr::new(10, 1, 2, 3),
                    },
                    ttl: 60,
                }],
            }]
        } else {
            Vec::new()
        },
        failure: if resolved {
            None
        } else {
            Some(yagra_common::DnsFailure::Timeout)
        },
        resolve_ms: 1.0,
    }
}

#[cfg(any(test, feature = "test-util"))]
impl FakeTransport {
    /// The canned instance rows for `column_oids`, honouring `max_rows`.
    ///
    /// The fake enforces the cap because a test is where a caller's budget arithmetic gets checked:
    /// a fake that ignored `max_rows` would let a caller pass the wrong bound and still pass its
    /// tests, and the real walk only proves itself against a device with a hundred-thousand-row
    /// table. Truncated after filtering and in declaration order, so which rows survive is
    /// deterministic.
    fn canned_instances(&self, column_oids: &[String], max_rows: usize) -> Vec<SnmpInstanceRow> {
        self.snmp_instances
            .iter()
            .filter(|r| column_oids.iter().any(|c| c == &r.oid_base))
            .take(max_rows)
            .cloned()
            .collect()
    }

    /// A fake that always reports the target reachable with the given RTT (and an HTTP 200).
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
            snmp_instances: Vec::new(),
            snmp_v3_strings: Vec::new(),
            http: HttpProbe {
                reachable: true,
                status_code: Some(200),
                response_time_ms: rtt_ms,
                cert_days_to_expiry: None,
                body: None,
            },
            meraki: Vec::new(),
            snmp_get_error: None,
            dns: fake_dns_chain(true),
        }
    }

    /// A fake that always reports the target unreachable (100% loss; HTTP unreachable).
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
            snmp_instances: Vec::new(),
            snmp_v3_strings: Vec::new(),
            http: HttpProbe {
                reachable: false,
                status_code: None,
                response_time_ms: 0.0,
                cert_days_to_expiry: None,
                body: None,
            },
            meraki: Vec::new(),
            snmp_get_error: None,
            dns: fake_dns_chain(false),
        }
    }

    /// Set the canned SNMP GET samples this fake returns.
    #[must_use]
    pub fn with_snmp(mut self, samples: Vec<SnmpSample>) -> Self {
        self.snmp = samples;
        self
    }

    /// Make every scalar SNMP GET (v2c and v3) fail, as an unreachable or refusing agent does.
    #[must_use]
    pub fn with_snmp_get_error(mut self, message: &str) -> Self {
        self.snmp_get_error = Some(message.to_owned());
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

    /// Set the canned HTTP probe this fake returns.
    #[must_use]
    pub fn with_http(mut self, probe: HttpProbe) -> Self {
        self.http = probe;
        self
    }

    /// Set the canned DNS chain this fake returns.
    #[must_use]
    pub fn with_dns(mut self, chain: DnsChain) -> Self {
        self.dns = chain;
        self
    }

    /// Set the canned Meraki observations this fake returns.
    #[must_use]
    pub fn with_meraki(mut self, observations: Vec<MerakiObservation>) -> Self {
        self.meraki = observations;
        self
    }
}

#[cfg(any(test, feature = "test-util"))]
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
        match &self.snmp_get_error {
            Some(e) => Err(TransportError::Io(e.clone())),
            None => Ok(self.snmp.clone()),
        }
    }

    async fn snmp_v3_get(
        &self,
        _target: IpAddr,
        _params: &SnmpV3Params,
        _oids: &[String],
        _timeout: Duration,
    ) -> Result<Vec<SnmpSample>, TransportError> {
        match &self.snmp_get_error {
            Some(e) => Err(TransportError::Io(e.clone())),
            None => Ok(self.snmp.clone()),
        }
    }

    async fn snmp_v3_get_strings(
        &self,
        _target: IpAddr,
        _params: &SnmpV3Params,
        oids: &[String],
        _timeout: Duration,
    ) -> Result<Vec<SnmpStringSample>, TransportError> {
        Ok(self
            .snmp_v3_strings
            .iter()
            .filter(|s| oids.iter().any(|o| o == &s.oid))
            .cloned()
            .collect())
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

    async fn snmp_v3_walk(
        &self,
        _target: IpAddr,
        _params: &SnmpV3Params,
        column_oids: &[String],
        _timeout: Duration,
    ) -> Result<Vec<SnmpTableSample>, TransportError> {
        // Same canned rows as the v2c walk — the fake is protocol-agnostic.
        Ok(self
            .snmp_table
            .iter()
            .filter(|r| column_oids.iter().any(|c| c == &r.oid_base))
            .cloned()
            .collect())
    }

    async fn snmp_v3_walk_strings(
        &self,
        _target: IpAddr,
        _params: &SnmpV3Params,
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

    async fn snmp_walk_instances(
        &self,
        _target: IpAddr,
        _community: &str,
        column_oids: &[String],
        _timeout: Duration,
        max_rows: usize,
    ) -> Result<Vec<SnmpInstanceRow>, TransportError> {
        Ok(self.canned_instances(column_oids, max_rows))
    }

    async fn snmp_v3_walk_instances(
        &self,
        _target: IpAddr,
        _params: &SnmpV3Params,
        column_oids: &[String],
        _timeout: Duration,
        max_rows: usize,
    ) -> Result<Vec<SnmpInstanceRow>, TransportError> {
        // Same canned rows as the v2c walk — the fake is protocol-agnostic.
        Ok(self.canned_instances(column_oids, max_rows))
    }

    async fn probe_http(
        &self,
        _spec: &HttpProbeSpec,
        _timeout: Duration,
    ) -> Result<HttpProbe, TransportError> {
        Ok(self.http.clone())
    }

    async fn resolve_dns(
        &self,
        _spec: &DnsProbeSpec,
        _timeout: Duration,
    ) -> Result<DnsChain, TransportError> {
        Ok(self.dns.clone())
    }

    async fn collect_meraki(
        &self,
        _spec: &MerakiCollectSpec,
        _timeout: Duration,
    ) -> Result<Vec<MerakiObservation>, TransportError> {
        Ok(self.meraki.clone())
    }
}

/// Row key for a table instance from its **tail** — the sub-identifiers past the column base. A
/// single trailing sub-id is the row key directly (the common case: ifIndex, entPhysicalIndex, …).
/// A multi-part tail (a multi-index table such as HUAWEI-MEMORY-MIB `hwMemoryDevTable` or BGP4-MIB
/// peers) folds to a stable synthetic key so the row is still collected (node-level health
/// aggregates over rows, so the key only needs to be distinct, not meaningful). An empty tail (the
/// column base itself, no instance) yields `None`.
///
/// Shared by the v2c (`snmp`) and v3 (`snmp_v3`) walkers so their keying can never diverge — a
/// node is only ever polled over one protocol, but keeping one implementation prevents a future
/// drift that would remap synthetic ifindexes (and break TSDB series continuity).
pub(crate) fn ifindex_from_tail(tail: &[u32]) -> Option<u32> {
    match tail {
        [] => None,
        [ifindex] => Some(*ifindex),
        multi => Some(fold_subids(multi)),
    }
}

/// Fold a multi-part instance index into a stable `u32` row key (FNV-1a over the sub-ids). Used
/// only for multi-index tables, where the key just needs to be deterministic and collision-rare.
pub(crate) fn fold_subids(subids: &[u32]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &n in subids {
        for b in n.to_be_bytes() {
            h ^= u32::from(b);
            h = h.wrapping_mul(0x0100_0193);
        }
    }
    h
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

    #[test]
    fn ifindex_from_tail_keys_single_and_folds_multi() {
        // The shared row-keying contract used by both the v2c and v3 table walkers.
        assert_eq!(ifindex_from_tail(&[]), None); // the column base itself: no instance
        assert_eq!(ifindex_from_tail(&[7]), Some(7)); // single trailing sub-id used directly
                                                      // Multi-part tails fold to a stable, distinct, deterministic key.
        let a = ifindex_from_tail(&[1, 0, 0]);
        let b = ifindex_from_tail(&[2, 0, 0]);
        assert!(a.is_some() && b.is_some());
        assert_ne!(a, b);
        assert_eq!(ifindex_from_tail(&[1, 0, 0]), a, "folding is deterministic");
    }

    /// `snmp_v3::raw_value`'s doc says it "mirrors `snmp::raw_value`", and nothing checked that —
    /// so it had silently drifted on two types (`IpAddress` and `Opaque`) that the v2c mapper
    /// handles. Nobody noticed because the only consumer at the time, the neighbour walk, reads
    /// neither. `ipAdEntNetMask` is an ASN.1 `IpAddress`, so ADR-043's IPv4 mask column would have
    /// come back empty on every SNMPv3 node while working perfectly on v2c — a difference no error
    /// would ever surface.
    ///
    /// The two clients have separate value enums, so this feeds each mapper its *own* spelling of
    /// the same ASN.1 type and demands the same [`SnmpValue`] out. That is the only form of this
    /// assertion that can catch a one-sided change.
    #[test]
    fn both_snmp_mappers_agree_on_every_value_type_they_share() {
        use csnmp::ObjectValue;
        use snmp2::Value;

        let v2c = |v: &ObjectValue| Some(crate::snmp::raw_value(v));
        let v3 = crate::snmp_v3::raw_value;

        // INTEGER
        assert_eq!(v2c(&ObjectValue::Integer(-5)), v3(&Value::Integer(-5)));
        // Counter32
        assert_eq!(v2c(&ObjectValue::Counter32(7)), v3(&Value::Counter32(7)));
        // Counter64, including the saturating out-of-range case.
        assert_eq!(
            v2c(&ObjectValue::Counter64(u64::MAX)),
            v3(&Value::Counter64(u64::MAX))
        );
        // OCTET STRING — octets verbatim on both.
        let mac = b"\x00\x1bT\xff\x00\x9a";
        assert_eq!(
            v2c(&ObjectValue::String(mac.to_vec())),
            v3(&Value::OctetString(mac))
        );
        // Opaque — the second variant the v3 wildcard used to swallow.
        assert_eq!(
            v2c(&ObjectValue::Opaque(mac.to_vec())),
            v3(&Value::Opaque(mac))
        );
        // IpAddress — the one ADR-043 actually needs. Both must yield the four octets, so the
        // caller reads a netmask the same way on either transport.
        assert_eq!(
            v2c(&ObjectValue::IpAddress(Ipv4Addr::new(255, 255, 255, 0))),
            v3(&Value::IpAddress([255, 255, 255, 0]))
        );
        assert_eq!(
            v3(&Value::IpAddress([255, 255, 255, 0])),
            Some(SnmpValue::Bytes(vec![255, 255, 255, 0])),
            "a netmask must survive the v3 walk as its octets"
        );
        // OBJECT IDENTIFIER — dotted decimal on both. This is what carries `ipAddressPrefix`.
        let dotted = "1.3.6.1.2.1.4.32.1.5.8.1.4.192.168.1.0.24";
        let v2c_oid: csnmp::ObjectIdentifier = dotted.parse().unwrap();
        let v3_oid = {
            let parts: Vec<u64> = dotted.split('.').map(|p| p.parse().unwrap()).collect();
            snmp2::Oid::from(parts.as_slice()).unwrap()
        };
        assert_eq!(
            v2c(&ObjectValue::ObjectId(v2c_oid)),
            v3(&Value::ObjectIdentifier(v3_oid))
        );
    }
}
