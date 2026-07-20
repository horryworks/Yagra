// SPDX-License-Identifier: AGPL-3.0-only
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
    ExpectedStatus, HostSample, HttpMethod, IfIndex, InterfaceField, MerakiTier, MetricKind,
    NodeId, SeriesKey,
};

/// Current bus message schema version. Bump on a backward-compatible change; a
/// breaking change needs an N/N-1 migration plan (ADR-017).
pub const BUS_SCHEMA_VERSION: u16 = 1;

const fn default_version() -> u16 {
    BUS_SCHEMA_VERSION
}

/// W3C trace-context carrier (`traceparent`/`tracestate`) propagated across the bus so one poll is
/// a single distributed trace (yagra-telemetry). An opaque `String`→`String` header bag: the bus
/// contract carries it **without depending on OpenTelemetry**, and it serializes to nothing when
/// empty (tracing export off, or an N-1 producer), preserving N/N-1 compatibility (ADR-017).
pub type TraceContext = std::collections::HashMap<String, String>;

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
    /// W3C trace context of the core-side dispatch span, so the poller's poll span joins the same
    /// distributed trace (yagra-telemetry). Empty (and omitted from the wire) when tracing export
    /// is off — zero steady-state cost; an N-1 poller ignores it (ADR-017). Working-set jobs the
    /// poller mints locally carry none (that trace is poller-rooted); only core-dispatched jobs
    /// (legacy per-job / "poll now") set it.
    #[serde(default, skip_serializing_if = "TraceContext::is_empty")]
    pub trace_context: TraceContext,
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
            trace_context: TraceContext::new(),
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
            trace_context: TraceContext::new(),
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
            trace_context: TraceContext::new(),
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
            trace_context: TraceContext::new(),
        }
    }

    /// A new SNMP v3 (USM) table-walk poll job for `node` at `target` — the v3 analogue of
    /// [`PollJob::snmp_table`].
    #[must_use]
    pub fn snmp_v3_table(
        job_id: Uuid,
        node_id: NodeId,
        target: IpAddr,
        check: SnmpV3TableCheck,
        interval_secs: u32,
    ) -> Self {
        Self {
            schema_version: BUS_SCHEMA_VERSION,
            job_id,
            node_id,
            target,
            check: CheckSpec::SnmpV3Table(check),
            interval_secs,
            credential_ref: None,
            probe_identity: false,
            trace_context: TraceContext::new(),
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
            trace_context: TraceContext::new(),
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
            trace_context: TraceContext::new(),
        }
    }
}

// ── Distributed poller pool (ADR-009/020) — working-set distribution ────────────────
//
// Instead of core publishing every individual [`PollJob`] on every tick, core hands each
// poller a *working set* — the set of polling specs it owns — as a full snapshot plus
// incremental deltas, and the poller schedules them locally (ADR-020). This cuts steady-state
// bus traffic and keeps polling running through a WAN blip. The messages below are the wire
// contract for that; they follow the same conventions as the rest of this file (`schema_version`
// with `default_version()`, `#[serde(default)]` on optional fields, no `deny_unknown_fields`),
// so they stay N/N-1 tolerant during a rolling upgrade (ADR-017).

/// One polling work item as it lives in a poller's working set: a [`PollJob`] **without** its
/// per-dispatch `job_id` (ADR-020). Core distributes reusable specs; the poller stamps a fresh
/// `job_id` each time it schedules one locally, so the same spec produces correlatable results
/// poll after poll without core re-sending it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobSpec {
    /// Node being polled.
    pub node_id: NodeId,
    /// Address to poll (IPv4 or IPv6).
    pub target: IpAddr,
    /// What to do.
    pub check: CheckSpec,
    /// Desired polling interval, in seconds (the poller's local scheduler applies jitter).
    pub interval_secs: u32,
    /// Ask the poller to also probe `sysDescr.0` on this poll (mirrors [`PollJob::probe_identity`]).
    /// Defaulted for N-1 compatibility (ADR-017).
    #[serde(default)]
    pub probe_identity: bool,
}

impl JobSpec {
    /// Strip a [`PollJob`] down to its reusable working-set spec, dropping the per-dispatch
    /// identity (`job_id`, `schema_version`, `credential_ref`).
    #[must_use]
    pub fn from_job(job: &PollJob) -> Self {
        Self {
            node_id: job.node_id,
            target: job.target,
            check: job.check.clone(),
            interval_secs: job.interval_secs,
            probe_identity: job.probe_identity,
        }
    }

    /// Re-hydrate a dispatchable [`PollJob`] from this spec, stamping the given `job_id` and the
    /// current [`BUS_SCHEMA_VERSION`]. Credentials are resolved/inlined by core separately, so
    /// `credential_ref` is left `None` here.
    #[must_use]
    pub fn to_job(&self, job_id: Uuid) -> PollJob {
        PollJob {
            schema_version: BUS_SCHEMA_VERSION,
            job_id,
            node_id: self.node_id,
            target: self.target,
            check: self.check.clone(),
            interval_secs: self.interval_secs,
            credential_ref: None,
            probe_identity: self.probe_identity,
            // A locally-minted working-set job is the root of its own trace (no core dispatch span
            // to inherit) — the result-side context still links it to core's ingest span.
            trace_context: TraceContext::new(),
        }
    }
}

/// All of one node's working-set specs — the granularity of both snapshot and delta, so core
/// can add, replace, or drop a node's polling as a unit (ADR-020).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeJobs {
    /// The node these specs belong to.
    pub node_id: NodeId,
    /// Every polling spec core wants this poller to run for the node.
    pub specs: Vec<JobSpec>,
}

/// How many nodes core packs into one snapshot chunk. Sized so a chunk of representative
/// table-walk specs stays well under NATS's 1 MB message cap (ADR-020) — see the chunk-size
/// bound test.
pub const SNAPSHOT_CHUNK_NODES: usize = 100;

/// A working-set sync addressed to one poller on its assignment subject
/// (`yagra.poller.assign.{id}`). Because that single subject preserves order, the poller can use
/// `seq` to gate deltas and detect gaps (ADR-020). Tagged so a new variant never breaks an older
/// poller — an unknown tag is skipped by the poller's malformed-message handling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncMsg {
    /// One chunk of a full working-set snapshot; the poller assembles all chunks of a snapshot
    /// and replaces its whole set with their union.
    SnapshotChunk(WorkingSetSnapshot),
    /// An incremental change (upserts/removes) that must apply at `seq = last_seq + 1`.
    Delta(WorkingSetDelta),
}

/// One chunk of a full working-set snapshot for a poller. A snapshot is `chunk_total` chunks that
/// share one `(epoch, seq)`; the poller reassembles them and replaces its whole set. `epoch`
/// identifies the core process (it bumps on restart) and `seq` is the poller's monotonic stream
/// counter, so the poller can tell a stale/racing snapshot from the current one (ADR-020).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkingSetSnapshot {
    /// Message schema version (defaulted for forward-compat).
    #[serde(default = "default_version")]
    pub schema_version: u16,
    /// The poller this snapshot is addressed to (sanitized id).
    pub poller_id: String,
    /// Core-process epoch — a change forces the poller to resync.
    pub epoch: Uuid,
    /// The monotonic per-poller stream sequence this snapshot establishes.
    pub seq: u64,
    /// 0-based index of this chunk within the snapshot.
    pub chunk_index: u32,
    /// Total number of chunks in this snapshot.
    pub chunk_total: u32,
    /// The nodes (with their specs) carried by this chunk.
    pub nodes: Vec<NodeJobs>,
    /// Total nodes across the whole snapshot (all chunks) — a hint for the poller/UI. Defaulted
    /// for N-1 compatibility (ADR-017).
    #[serde(default)]
    pub total_nodes: u32,
}

/// An incremental working-set change for a poller, applied only when it lands exactly at the next
/// sequence (`last_seq + 1`); a gap forces a snapshot resync (ADR-020). This is also the failover
/// mechanism: when a poller dies, the reassignment of its nodes to survivors arrives as ordinary
/// upsert/remove deltas — no special-case code path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkingSetDelta {
    /// Message schema version (defaulted for forward-compat).
    #[serde(default = "default_version")]
    pub schema_version: u16,
    /// The poller this delta is addressed to (sanitized id).
    pub poller_id: String,
    /// Core-process epoch (must match the poller's current epoch, else it resyncs).
    pub epoch: Uuid,
    /// Sequence of this delta; must equal the poller's `last_seq + 1`.
    pub seq: u64,
    /// Nodes to add or replace wholesale. Defaulted so a remove-only delta omits it (ADR-017).
    #[serde(default)]
    pub upserts: Vec<NodeJobs>,
    /// Nodes to drop from the working set. Defaulted so an upsert-only delta omits it.
    #[serde(default)]
    pub removes: Vec<NodeId>,
}

/// A session-revocation notice fanned out **core→core** on [`crate::subjects::auth_revoke`]
/// (Core HA active/active, ADR-016 Increment 2a). Stateless signed session tokens are self-
/// validating, so logout / account-disable must be broadcast to every core's in-memory denylist
/// (and persisted to `auth_revocations`) to take effect. Tagged so a future variant never breaks an
/// N-1 core (unknown tag ⇒ the message is skipped). Carries only a token **hash** or a user id and
/// timestamps — never a raw token or password (security.md).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthRevoke {
    /// Revoke one specific signed token (server-side logout). `hash` is the SHA-256 hex of the token;
    /// `exp_unix` is the token's own expiry, after which the denylist entry self-prunes.
    Token {
        /// SHA-256 hex of the revoked token (never the token itself).
        hash: String,
        /// Unix seconds after which the entry can be dropped (the token's own `exp`).
        exp_unix: u64,
    },
    /// Revoke every token for `uid` issued at or before `cutoff_iat` — the account was disabled,
    /// demoted, password-reset, or deleted. `exp_unix` bounds how long the entry is retained (the
    /// maximum lifetime of any token that could still be in flight).
    User {
        /// The affected user's id.
        uid: Uuid,
        /// Deny tokens whose `iat` is ≤ this (Unix seconds); a fresh login after this is allowed.
        cutoff_iat: u64,
        /// Unix seconds after which the entry can be dropped.
        exp_unix: u64,
    },
}

/// How often a poller publishes a [`HeartbeatMsg`] (seconds). One shared home for the cadence so
/// the poller's beat interval (Yagra-poller) and core's liveness judgment (Yagra-core) can't drift
/// (ADR-009).
pub const HEARTBEAT_SECS: u64 = 10;

/// Seconds without a heartbeat after which core treats a poller as offline and drops it from its
/// pool's hash ring — three missed [`HEARTBEAT_SECS`] beats (ADR-009). The offline event flows to
/// survivors as ordinary working-set deltas (no special failover path).
pub const OFFLINE_AFTER_SECS: u64 = 30;

/// A poller liveness + telemetry beat, published every ~10s on [`crate::subjects::heartbeat`]
/// (ADR-009). Core's registry marks a poller offline after ~30s (3 missed beats) and rebalances
/// its nodes to survivors. The `epoch`/`last_seq` echo lets core notice a poller that is behind or
/// on a stale epoch and push it a fresh snapshot. Every telemetry field defaults so the beat stays
/// N/N-1 tolerant (ADR-017).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeartbeatMsg {
    /// Message schema version (defaulted for forward-compat).
    #[serde(default = "default_version")]
    pub schema_version: u16,
    /// Sanitized poller id (stable across restarts).
    pub poller_id: String,
    /// Pool this poller serves.
    pub pool: String,
    /// Per-process incarnation (a fresh UUID each start → lets core detect a restart).
    pub incarnation: Uuid,
    /// Poller build version (`CARGO_PKG_VERSION`).
    #[serde(default)]
    pub version: String,
    /// The epoch the poller currently holds a working set for (`None` before its first snapshot).
    #[serde(default)]
    pub epoch: Option<Uuid>,
    /// The highest sync sequence the poller has applied.
    #[serde(default)]
    pub last_seq: u64,
    /// Number of nodes in the poller's working set.
    #[serde(default)]
    pub working_set_nodes: u32,
    /// Number of specs in the poller's working set.
    #[serde(default)]
    pub working_set_specs: u32,
    /// Polls currently in flight.
    #[serde(default)]
    pub inflight: u32,
    /// Total results this poller has produced since start.
    #[serde(default)]
    pub results_total: u64,
    /// Passive-event listeners the poller has bound (e.g. `syslog:514`, `trap:162`).
    #[serde(default)]
    pub listeners: Vec<String>,
    /// The poller host's own resource sample (CPU/load/memory/disk) for self-observability. `None`
    /// from an N-1 poller that predates host telemetry — core then simply shows no host data for
    /// it. Core is the single writer of the resulting `yagra_host_*` series to the TSDB (remote
    /// pollers can't reach it directly), so this heartbeat field is that path.
    #[serde(default)]
    pub host: Option<HostSample>,
}

/// A poller's request for a fresh full snapshot, published on [`crate::subjects::sync_request`]
/// (ADR-020). Sent at startup and whenever the poller detects a gap / epoch mismatch. This is
/// plain pub/sub, **not** request-reply, because the reply is several snapshot chunks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncRequest {
    /// Message schema version (defaulted for forward-compat).
    #[serde(default = "default_version")]
    pub schema_version: u16,
    /// Sanitized id of the poller requesting the snapshot.
    pub poller_id: String,
    /// Pool the poller serves.
    pub pool: String,
    /// The poller's current incarnation.
    pub incarnation: Uuid,
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
    /// SNMP v3 (USM) GETBULK walk of table columns — the v3 analogue of [`CheckSpec::SnmpTable`].
    /// Like the variants around it, an older poller that doesn't know this tag simply skips the
    /// job (N-1 compatible): a v3 node just keeps getting scalars + ICMP until the poller upgrades.
    SnmpV3Table(SnmpV3TableCheck),
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

/// SNMP v3 (USM) table-walk parameters — the v3 analogue of [`SnmpTableCheck`]. Carries the same
/// USM fields as [`SnmpV3Check`] plus the numeric/metadata table columns. Auth/priv keys are
/// resolved/decrypted by core and inlined here (ADR-018/020); the poller never reads the secret
/// store. All fields are `#[serde(default)]` so an N-1 poller that gains this variant reads a
/// forward-compatible message (ADR-017).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpV3TableCheck {
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
    /// Which poller produced this result (its sanitized id), when the poller stamps one
    /// (ADR-009). Descriptive provenance for the Pollers view / self-observability — never a
    /// TSDB label. Defaulted so an older poller that doesn't stamp it stays N-1 compatible
    /// (ADR-017); core treats `None` as "unknown / central".
    #[serde(default)]
    pub poller_id: Option<String>,
    /// W3C trace context of the poller's poll span, so core's result-ingest span joins the same
    /// distributed trace (yagra-telemetry). This is what links a poll end-to-end in **both**
    /// dispatch modes — including working-set mode, where the trace is poller-rooted. Empty (and
    /// omitted from the wire) when tracing export is off; an N-1 poller never sends it (ADR-017).
    #[serde(default, skip_serializing_if = "TraceContext::is_empty")]
    pub trace_context: TraceContext,
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

// ── Passive events (Phase 2) — edge-received syslog / SNMP traps / webhooks ─────────

/// What kind of passive event a poller (or core, for webhooks) received.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A syslog datagram (RFC 5424 / RFC 3164 / raw fallback).
    Syslog,
    /// An SNMP trap or inform (v1/v2c).
    Trap,
    /// An inbound webhook received on core's northbound API.
    Webhook,
}

impl EventKind {
    /// Stable string form (matches the serde tag and the `events.kind` DB column).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Syslog => "syslog",
            Self::Trap => "trap",
            Self::Webhook => "webhook",
        }
    }
}

/// A passive event received at the edge, published on `yagra.events` for core to match
/// against event rules. The `message` field is the single normalized text rules bite on
/// (for traps it is rendered as `"<trap_oid> oid=value; …"`); the structured fields are
/// preserved for display. All detail fields are `#[serde(default)]` so the message stays
/// N/N-1 tolerant (ADR-017).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventMsg {
    /// Message schema version (defaulted for forward-compat).
    #[serde(default = "default_version")]
    pub schema_version: u16,
    /// Receiver-generated id (idempotency / tracing).
    pub event_id: Uuid,
    /// What kind of event this is.
    pub kind: EventKind,
    /// Reception time at the edge, as Unix time in milliseconds (UTC).
    pub at_unix_ms: i64,
    /// Datagram source address (`None` for webhooks — they correlate via the source binding).
    #[serde(default)]
    pub source_ip: Option<IpAddr>,
    /// Which poller pool received it, if known.
    #[serde(default)]
    pub pool: Option<String>,
    /// Normalized text the match rules run against (≤ 4096 chars, lossy UTF-8).
    pub message: String,
    /// Syslog facility (decoded from PRI), if present.
    #[serde(default)]
    pub facility: Option<u8>,
    /// Syslog severity (decoded from PRI), if present. Named to avoid confusion with
    /// the alert severity a matching rule assigns.
    #[serde(default)]
    pub syslog_severity: Option<u8>,
    /// Syslog HOSTNAME field, if present.
    #[serde(default)]
    pub hostname: Option<String>,
    /// Syslog APP-NAME / TAG field, if present.
    #[serde(default)]
    pub app_name: Option<String>,
    /// The trap's snmpTrapOID (v1 mapped per RFC 3584), if this is a trap.
    #[serde(default)]
    pub trap_oid: Option<String>,
    /// Trap varbinds as dotted-OID → rendered-value pairs (capped at the receiver).
    #[serde(default)]
    pub varbinds: Vec<(String, String)>,
    /// Whether the original message was clipped to the size cap.
    #[serde(default)]
    pub truncated: bool,
}

// ── Flow records (Phase 3, ADR-031) — edge-received NetFlow/IPFIX/sFlow, pre-aggregated ─────

/// A batch of edge-aggregated flow records for one bucket window, published on `yagra.flows`
/// for core to write to ClickHouse (ADR-031).
///
/// The poller receives raw NetFlow/IPFIX/sFlow datagrams, folds identical 5-tuples within the
/// bucket window, and keeps only the **top-N by bytes** — so the high-cardinality flow tuple
/// stays bounded on the wire and **never reaches the TSDB** (the cardinality invariant that
/// motivates the whole ADR). Flow is a best-effort, loss-tolerant tier (ADR-017): a failed
/// publish is dropped, not buffered. All detail fields are `#[serde(default)]` for N/N-1
/// tolerance. Core resolves `exporter_ip` → node via its own inventory, so the poller need not
/// know which node an exporter maps to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowBatch {
    /// Message schema version (defaulted for forward-compat).
    #[serde(default = "default_version")]
    pub schema_version: u16,
    /// Poller that received and aggregated the flows.
    pub poller_id: String,
    /// Pool the poller belongs to.
    pub pool: String,
    /// Source address of the export datagrams (the monitored device). Core maps this to a node.
    pub exporter_ip: IpAddr,
    /// Bucket start as Unix time in milliseconds (UTC), aligned to `bucket_secs`.
    pub bucket_start_ms: i64,
    /// Aggregation window width in seconds (e.g. 60).
    pub bucket_secs: u32,
    /// Top-N aggregated records for this bucket, ordered by bytes descending.
    pub records: Vec<FlowRecord>,
    /// Count of distinct flows dropped/folded beyond the top-N (or key) cap (observability).
    #[serde(default)]
    pub dropped: u32,
}

/// One aggregated flow within a [`FlowBatch`]: a 5-tuple (+ ingress ifIndex / ToS) with summed
/// byte/packet/flow counts over the bucket window. Addresses are `IpAddr` (v4 or v6 — never
/// assume v4). `src_as`/`dst_as` are reserved for a later increment (`0` = unknown until then).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowRecord {
    /// Source address.
    pub src_ip: IpAddr,
    /// Destination address.
    pub dst_ip: IpAddr,
    /// Source transport port (0 for non-port protocols).
    pub src_port: u16,
    /// Destination transport port (0 for non-port protocols).
    pub dst_port: u16,
    /// IP protocol number (6 = TCP, 17 = UDP, 1 = ICMP, …).
    pub proto: u8,
    /// IP type-of-service / DSCP byte, if the exporter provided it.
    #[serde(default)]
    pub tos: u8,
    /// Ingress interface ifIndex, if the exporter provided it (`0` = unknown).
    #[serde(default)]
    pub if_index: u32,
    /// Source autonomous-system number (reserved; ADR-031 Increment 3, `0` = unknown).
    #[serde(default)]
    pub src_as: u32,
    /// Destination autonomous-system number (reserved; ADR-031 Increment 3, `0` = unknown).
    #[serde(default)]
    pub dst_as: u32,
    /// Bytes observed over the bucket window.
    pub bytes: u64,
    /// Packets observed over the bucket window.
    pub packets: u64,
    /// Number of original flow records folded into this row.
    pub flows: u32,
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
    fn trace_context_round_trips_and_is_omitted_when_empty() {
        // Tracing off (the default): an empty carrier must not appear on the wire, so there's zero
        // steady-state cost and an N-1 peer sees exactly the old shape (skip_serializing_if).
        let job = sample_job();
        assert!(job.trace_context.is_empty());
        let json = serde_json::to_string(&job).unwrap();
        assert!(
            !json.contains("trace_context"),
            "empty trace context must be omitted from the wire: {json}"
        );

        // Tracing on: a populated W3C context round-trips intact so the poller can rebuild the
        // parent span from it.
        let traceparent = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";
        let mut traced = sample_job();
        traced
            .trace_context
            .insert("traceparent".to_owned(), traceparent.to_owned());
        let json = serde_json::to_string(&traced).unwrap();
        assert!(json.contains("traceparent"));
        let back: PollJob = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.trace_context.get("traceparent").map(String::as_str),
            Some(traceparent)
        );
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
    fn snmp_v3_table_job_round_trips_with_snake_case_tag() {
        let job = PollJob::snmp_v3_table(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 3)),
            SnmpV3TableCheck {
                user: "monitor".into(),
                security_level: "authpriv".into(),
                auth_protocol: Some("sha256".into()),
                auth_key: Some("auth-pass".into()),
                priv_protocol: Some("aes256".into()),
                priv_key: Some("priv-pass".into()),
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
        assert!(json.contains("\"kind\":\"snmp_v3_table\""));
        let back: PollJob = serde_json::from_str(&json).unwrap();
        assert_eq!(job, back);
    }

    #[test]
    fn snmp_v3_table_check_defaults_are_forward_compatible() {
        // An N-1 producer that omits every newer optional field (auth/priv, meta_columns,
        // timeout) still deserializes with safe defaults (ADR-017).
        let json = r#"{
            "kind": "snmp_v3_table",
            "user": "monitor",
            "security_level": "noauth",
            "columns": [
                { "metric_name": "if_hc_in_octets", "oid": "1.3.6.1.2.1.31.1.1.1.6" }
            ]
        }"#;
        let check: CheckSpec = serde_json::from_str(json).unwrap();
        let CheckSpec::SnmpV3Table(t) = check else {
            panic!("expected snmp_v3_table variant");
        };
        assert_eq!(t.user, "monitor");
        assert!(t.auth_protocol.is_none());
        assert!(t.priv_key.is_none());
        assert!(t.meta_columns.is_empty());
        assert_eq!(t.timeout_ms, default_snmp_timeout_ms());
        assert_eq!(t.columns[0].kind, MetricKind::Gauge); // column kind defaults to gauge
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
    fn event_msg_round_trips_through_json() {
        let event = EventMsg {
            schema_version: BUS_SCHEMA_VERSION,
            event_id: Uuid::nil(),
            kind: EventKind::Syslog,
            at_unix_ms: 1_700_000_000_000,
            source_ip: Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10))),
            pool: Some("default".into()),
            message: "link down on ge-0/0/1".into(),
            facility: Some(23),
            syslog_severity: Some(4),
            hostname: Some("edge-sw1".into()),
            app_name: Some("chassisd".into()),
            trap_oid: None,
            varbinds: Vec::new(),
            truncated: false,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"kind\":\"syslog\""));
        let back: EventMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
    }

    #[test]
    fn minimal_event_msg_defaults_optional_fields() {
        // N/N-1 (ADR-017): a producer that sends only the required fields (or an older
        // schema without the detail fields) still deserializes; extras are ignored.
        let json = r#"{
            "event_id": "00000000-0000-0000-0000-000000000000",
            "kind": "trap",
            "at_unix_ms": 0,
            "message": "1.3.6.1.6.3.1.1.5.3",
            "future_field": "ignored"
        }"#;
        let event: EventMsg = serde_json::from_str(json).unwrap();
        assert_eq!(event.schema_version, BUS_SCHEMA_VERSION); // defaulted
        assert_eq!(event.kind, EventKind::Trap);
        assert!(event.source_ip.is_none());
        assert!(event.varbinds.is_empty());
        assert!(!event.truncated);
    }

    #[test]
    fn event_kind_str_matches_serde_tag() {
        for kind in [EventKind::Syslog, EventKind::Trap, EventKind::Webhook] {
            let tag = serde_json::to_string(&kind).unwrap();
            assert_eq!(tag, format!("\"{}\"", kind.as_str()));
        }
    }

    #[test]
    fn flow_batch_round_trips_through_json() {
        let batch = FlowBatch {
            schema_version: BUS_SCHEMA_VERSION,
            poller_id: "edge-1".into(),
            pool: "default".into(),
            exporter_ip: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
            bucket_start_ms: 1_700_000_000_000,
            bucket_secs: 60,
            records: vec![FlowRecord {
                src_ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)),
                dst_ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 3)),
                src_port: 40000,
                dst_port: 443,
                proto: 6,
                tos: 0,
                if_index: 2,
                src_as: 0,
                dst_as: 0,
                bytes: 8192,
                packets: 12,
                flows: 4,
            }],
            dropped: 7,
        };
        let json = serde_json::to_string(&batch).unwrap();
        let back: FlowBatch = serde_json::from_str(&json).unwrap();
        assert_eq!(batch, back);
    }

    #[test]
    fn minimal_flow_record_defaults_optional_fields() {
        // N/N-1 (ADR-017): a producer that predates tos/if_index/as fields still deserializes.
        let json = r#"{
            "src_ip": "10.0.0.2",
            "dst_ip": "2001:db8::1",
            "src_port": 40000,
            "dst_port": 443,
            "proto": 6,
            "bytes": 8192,
            "packets": 12,
            "flows": 4,
            "future_field": "ignored"
        }"#;
        let rec: FlowRecord = serde_json::from_str(json).unwrap();
        assert_eq!(rec.tos, 0);
        assert_eq!(rec.if_index, 0);
        assert_eq!(rec.src_as, 0);
        assert_eq!(rec.dst_as, 0);
        assert!(rec.dst_ip.is_ipv6()); // v6 addresses parse (never assume v4)
    }

    #[test]
    fn minimal_flow_batch_defaults_version_and_dropped() {
        let json = r#"{
            "poller_id": "edge-1",
            "pool": "default",
            "exporter_ip": "192.168.1.1",
            "bucket_start_ms": 0,
            "bucket_secs": 60,
            "records": []
        }"#;
        let batch: FlowBatch = serde_json::from_str(json).unwrap();
        assert_eq!(batch.schema_version, BUS_SCHEMA_VERSION); // defaulted
        assert_eq!(batch.dropped, 0); // defaulted
        assert!(batch.records.is_empty());
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

    // ── Distributed poller pool (ADR-009/020) — working-set wire contract ────────────

    fn sample_node_jobs() -> NodeJobs {
        NodeJobs {
            node_id: NodeId::from(Uuid::nil()),
            specs: vec![JobSpec::from_job(&sample_job())],
        }
    }

    #[test]
    fn poll_result_without_poller_id_defaults_none() {
        // N-1 (ADR-017): a PollResult from a poller that doesn't stamp provenance has no
        // `poller_id` — new core must default it to None, not fail to deserialize.
        let json = r#"{
            "job_id": "00000000-0000-0000-0000-000000000000",
            "node_id": "00000000-0000-0000-0000-000000000000",
            "at_unix_ms": 0,
            "outcome": "reachable"
        }"#;
        let result: PollResult = serde_json::from_str(json).unwrap();
        assert!(result.poller_id.is_none());
        assert!(result.samples.is_empty());
        // Same N-1 rule for the trace context: an older poller sends none → defaults to empty.
        assert!(result.trace_context.is_empty());
    }

    #[test]
    fn poll_result_with_poller_id_round_trips() {
        let result = PollResult {
            schema_version: BUS_SCHEMA_VERSION,
            job_id: Uuid::nil(),
            node_id: NodeId::from(Uuid::nil()),
            at_unix_ms: 42,
            outcome: CheckOutcome::Reachable,
            samples: vec![Sample::gauge("icmp_rtt_ms", 5.0)],
            interfaces: Vec::new(),
            sys_descr: None,
            poller_id: Some("edge-poller-1".into()),
            trace_context: TraceContext::new(),
        };
        let json = serde_json::to_string(&result).unwrap();
        let back: PollResult = serde_json::from_str(&json).unwrap();
        assert_eq!(result, back);
        assert_eq!(back.poller_id.as_deref(), Some("edge-poller-1"));
    }

    #[test]
    fn job_spec_from_and_to_job_is_inverse_modulo_identity() {
        let job = PollJob::snmp(
            Uuid::new_v4(),
            NodeId::from(Uuid::new_v4()),
            IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3)),
            SnmpCheck {
                community: "public".into(),
                oids: vec!["1.3.6.1.2.1.1.3.0".into()],
                columns: Vec::new(),
                timeout_ms: 2000,
            },
            45,
        );
        let spec = JobSpec::from_job(&job);
        let new_id = Uuid::new_v4();
        let rebuilt = spec.to_job(new_id);
        // Everything but the per-dispatch identity survives the round-trip.
        assert_eq!(rebuilt.job_id, new_id);
        assert_eq!(rebuilt.schema_version, BUS_SCHEMA_VERSION);
        assert_eq!(rebuilt.credential_ref, None);
        assert_eq!(rebuilt.node_id, job.node_id);
        assert_eq!(rebuilt.target, job.target);
        assert_eq!(rebuilt.check, job.check);
        assert_eq!(rebuilt.interval_secs, job.interval_secs);
        assert_eq!(rebuilt.probe_identity, job.probe_identity);
        // …and from_job(to_job(spec)) is the identity on the spec itself.
        assert_eq!(JobSpec::from_job(&rebuilt), spec);
    }

    #[test]
    fn job_spec_round_trips_through_json() {
        let spec = JobSpec::from_job(&sample_job());
        let json = serde_json::to_string(&spec).unwrap();
        let back: JobSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, back);
    }

    #[test]
    fn job_spec_tolerates_missing_and_unknown_fields() {
        // Old producer: no probe_identity; new producer: an extra field we ignore (ADR-017).
        let json = r#"{
            "node_id": "00000000-0000-0000-0000-000000000000",
            "target": "10.0.0.1",
            "check": { "kind": "icmp", "count": 3, "timeout_ms": 1000 },
            "interval_secs": 30,
            "future_field": "ignored"
        }"#;
        let spec: JobSpec = serde_json::from_str(json).unwrap();
        assert!(!spec.probe_identity);
        assert_eq!(spec.interval_secs, 30);
    }

    #[test]
    fn snapshot_chunk_round_trips_and_carries_snake_case_tag() {
        let msg = SyncMsg::SnapshotChunk(WorkingSetSnapshot {
            schema_version: BUS_SCHEMA_VERSION,
            poller_id: "edge-1".into(),
            epoch: Uuid::nil(),
            seq: 7,
            chunk_index: 0,
            chunk_total: 1,
            nodes: vec![sample_node_jobs()],
            total_nodes: 1,
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"snapshot_chunk\""));
        let back: SyncMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn delta_round_trips_and_carries_snake_case_tag() {
        let msg = SyncMsg::Delta(WorkingSetDelta {
            schema_version: BUS_SCHEMA_VERSION,
            poller_id: "edge-1".into(),
            epoch: Uuid::nil(),
            seq: 8,
            upserts: vec![sample_node_jobs()],
            removes: vec![NodeId::from(Uuid::nil())],
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"delta\""));
        let back: SyncMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn sync_msg_tags_are_stable_and_unknown_tag_errs() {
        // The literal tag strings are part of the wire contract — pin them.
        let snap: SyncMsg = serde_json::from_str(
            r#"{"type":"snapshot_chunk","poller_id":"p","epoch":"00000000-0000-0000-0000-000000000000","seq":1,"chunk_index":0,"chunk_total":1,"nodes":[]}"#,
        )
        .unwrap();
        assert!(matches!(snap, SyncMsg::SnapshotChunk(_)));

        let delta: SyncMsg = serde_json::from_str(
            r#"{"type":"delta","poller_id":"p","epoch":"00000000-0000-0000-0000-000000000000","seq":2}"#,
        )
        .unwrap();
        assert!(matches!(delta, SyncMsg::Delta(_)));

        // An unknown tag is an error the consumer can skip — it must not panic.
        let unknown = serde_json::from_str::<SyncMsg>(r#"{"type":"reset","poller_id":"p"}"#);
        assert!(unknown.is_err());
    }

    #[test]
    fn working_set_snapshot_tolerates_missing_and_unknown_fields() {
        // Old producer: no schema_version / total_nodes; new producer: an extra field (ADR-017).
        let json = r#"{
            "poller_id": "edge-1",
            "epoch": "00000000-0000-0000-0000-000000000000",
            "seq": 3,
            "chunk_index": 1,
            "chunk_total": 4,
            "nodes": [],
            "future_field": 99
        }"#;
        let snap: WorkingSetSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(snap.schema_version, BUS_SCHEMA_VERSION); // defaulted
        assert_eq!(snap.total_nodes, 0); // defaulted
        assert_eq!(snap.chunk_total, 4);
    }

    #[test]
    fn working_set_delta_tolerates_missing_and_unknown_fields() {
        // A remove-only delta omits `upserts`; an upsert-only delta omits `removes`; both, plus a
        // missing schema_version and an unknown field, must deserialize (ADR-017).
        let json = r#"{
            "poller_id": "edge-1",
            "epoch": "00000000-0000-0000-0000-000000000000",
            "seq": 9,
            "removes": ["00000000-0000-0000-0000-000000000000"],
            "future_field": true
        }"#;
        let delta: WorkingSetDelta = serde_json::from_str(json).unwrap();
        assert_eq!(delta.schema_version, BUS_SCHEMA_VERSION); // defaulted
        assert!(delta.upserts.is_empty()); // defaulted
        assert_eq!(delta.removes.len(), 1);
    }

    #[test]
    fn heartbeat_round_trips_and_tolerates_minimal_form() {
        let hb = HeartbeatMsg {
            schema_version: BUS_SCHEMA_VERSION,
            poller_id: "edge-1".into(),
            pool: "tokyo".into(),
            incarnation: Uuid::nil(),
            version: "0.1.0".into(),
            epoch: Some(Uuid::nil()),
            last_seq: 12,
            working_set_nodes: 3,
            working_set_specs: 7,
            inflight: 1,
            results_total: 100,
            listeners: vec!["syslog:514".into()],
            host: Some(yagra_common::HostSample {
                cpu_pct: 12.5,
                mem_used_bytes: 2,
                mem_total_bytes: 8,
                disks: vec![yagra_common::DiskUsage {
                    mount: "root".into(),
                    used_bytes: 10,
                    size_bytes: 100,
                }],
                ..Default::default()
            }),
        };
        let json = serde_json::to_string(&hb).unwrap();
        let back: HeartbeatMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(hb, back);

        // Old producer: only the required identity fields + an unknown extra (ADR-017).
        let minimal = r#"{
            "poller_id": "edge-1",
            "pool": "default",
            "incarnation": "00000000-0000-0000-0000-000000000000",
            "future_field": "ignored"
        }"#;
        let hb: HeartbeatMsg = serde_json::from_str(minimal).unwrap();
        assert_eq!(hb.schema_version, BUS_SCHEMA_VERSION); // defaulted
        assert!(hb.version.is_empty());
        assert!(hb.epoch.is_none());
        assert_eq!(hb.last_seq, 0);
        assert!(hb.listeners.is_empty());
        assert!(hb.host.is_none()); // N-1 poller: no host telemetry
    }

    #[test]
    fn sync_request_round_trips_and_tolerates_missing_version() {
        let req = SyncRequest {
            schema_version: BUS_SCHEMA_VERSION,
            poller_id: "edge-1".into(),
            pool: "tokyo".into(),
            incarnation: Uuid::nil(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: SyncRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);

        let old = r#"{
            "poller_id": "edge-1",
            "pool": "default",
            "incarnation": "00000000-0000-0000-0000-000000000000",
            "future_field": 1
        }"#;
        let req: SyncRequest = serde_json::from_str(old).unwrap();
        assert_eq!(req.schema_version, BUS_SCHEMA_VERSION); // defaulted
    }

    #[test]
    fn full_snapshot_chunk_stays_under_nats_message_cap() {
        // A worst-case-ish chunk: SNAPSHOT_CHUNK_NODES nodes, each with one interface table-walk
        // spec (community + several numeric columns + metadata columns). It must serialize to well
        // under NATS's 1 MB message cap (ADR-020); we assert a conservative 900 KB ceiling.
        let spec = JobSpec {
            node_id: NodeId::from(Uuid::new_v4()),
            target: IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)),
            check: CheckSpec::SnmpTable(SnmpTableCheck {
                community: "a-representative-community-string".into(),
                columns: vec![
                    SnmpColumn {
                        metric_name: "if_hc_in_octets".into(),
                        oid: "1.3.6.1.2.1.31.1.1.1.6".into(),
                        kind: MetricKind::Counter,
                    },
                    SnmpColumn {
                        metric_name: "if_hc_out_octets".into(),
                        oid: "1.3.6.1.2.1.31.1.1.1.10".into(),
                        kind: MetricKind::Counter,
                    },
                    SnmpColumn {
                        metric_name: "if_in_errors".into(),
                        oid: "1.3.6.1.2.1.2.2.1.14".into(),
                        kind: MetricKind::Counter,
                    },
                    SnmpColumn {
                        metric_name: "if_out_errors".into(),
                        oid: "1.3.6.1.2.1.2.2.1.20".into(),
                        kind: MetricKind::Counter,
                    },
                    SnmpColumn {
                        metric_name: "if_oper_status".into(),
                        oid: "1.3.6.1.2.1.2.2.1.8".into(),
                        kind: MetricKind::Gauge,
                    },
                ],
                meta_columns: vec![
                    SnmpMetaColumn {
                        field: InterfaceField::Name,
                        oid: "1.3.6.1.2.1.31.1.1.1.1".into(),
                    },
                    SnmpMetaColumn {
                        field: InterfaceField::Alias,
                        oid: "1.3.6.1.2.1.31.1.1.1.18".into(),
                    },
                    SnmpMetaColumn {
                        field: InterfaceField::Speed,
                        oid: "1.3.6.1.2.1.31.1.1.1.15".into(),
                    },
                ],
                timeout_ms: 2000,
            }),
            interval_secs: 60,
            probe_identity: false,
        };
        let nodes: Vec<NodeJobs> = (0..SNAPSHOT_CHUNK_NODES)
            .map(|_| NodeJobs {
                node_id: NodeId::from(Uuid::new_v4()),
                specs: vec![spec.clone()],
            })
            .collect();
        let snap = SyncMsg::SnapshotChunk(WorkingSetSnapshot {
            schema_version: BUS_SCHEMA_VERSION,
            poller_id: "edge-poller-fully-qualified.example.com".into(),
            epoch: Uuid::new_v4(),
            seq: u64::MAX,
            chunk_index: 0,
            chunk_total: 1,
            total_nodes: SNAPSHOT_CHUNK_NODES as u32,
            nodes,
        });
        let bytes = serde_json::to_vec(&snap).unwrap();
        assert!(
            bytes.len() < 900_000,
            "snapshot chunk of {SNAPSHOT_CHUNK_NODES} nodes was {} bytes",
            bytes.len()
        );
    }

    #[test]
    fn auth_revoke_round_trips_both_variants() {
        let token = AuthRevoke::Token {
            hash: "deadbeef".into(),
            exp_unix: 1_800_000_000,
        };
        let user = AuthRevoke::User {
            uid: Uuid::new_v4(),
            cutoff_iat: 1_700_000_000,
            exp_unix: 1_700_086_400,
        };
        for msg in [token, user] {
            let bytes = serde_json::to_vec(&msg).unwrap();
            assert_eq!(serde_json::from_slice::<AuthRevoke>(&bytes).unwrap(), msg);
        }
    }

    #[test]
    fn auth_revoke_tag_is_stable_and_tolerates_unknown_fields() {
        // The wire tag must stay `kind` (a consumer keys on it) and unknown fields are ignored
        // (N-1 tolerance, ADR-017 — we never use deny_unknown_fields).
        let json = r#"{"kind":"token","hash":"abc","exp_unix":42,"future_field":true}"#;
        let msg: AuthRevoke = serde_json::from_str(json).unwrap();
        assert_eq!(
            msg,
            AuthRevoke::Token {
                hash: "abc".into(),
                exp_unix: 42
            }
        );
    }
}
