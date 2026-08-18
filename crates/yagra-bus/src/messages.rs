// SPDX-License-Identifier: AGPL-3.0-only
//! Bus message contract between core (Yagra-core) and pollers (Yagra-poller).
//!
//! These are the *only* way core and pollers talk (ADR-003). Messages are
//! **version-tolerant** (ADR-017) so a new core runs against an old poller, and vice
//! versa, during a rolling upgrade.
//!
//! **There is deliberately no version field.** Compatibility rests entirely on structural
//! tolerance, in five mechanisms:
//!
//! 1. Every new field is `#[serde(default)]`, so an N-1 producer that omits it still
//!    deserializes. A new field needs a `…_tolerates_missing_and_unknown_fields` test.
//! 2. We never use `deny_unknown_fields`, so a field an N-1 consumer has never heard of
//!    (including the `schema_version` that older builds still send) is ignored.
//! 3. [`crate::de_lenient_specs`] decodes list elements individually, so one unknown
//!    `CheckSpec` variant drops that element instead of failing the whole message.
//! 4. Subscribers filter with `.ok()`, and new message families get their own subject
//!    ([`crate::subjects`]) rather than overloading an existing one — subject
//!    partitioning is the real version gate.
//! 5. Optional behaviour is negotiated by capability strings in [`HeartbeatMsg::caps`]
//!    (see [`CAP_RAW_CAPTURE`], [`CAP_FLOW_RELAY`]), not by a version number.
//!
//! A `BUS_SCHEMA_VERSION` constant used to be stamped on every message here. Nothing ever
//! read it — no producer varied its output by it and no consumer branched on it — so it
//! read as a version gate without being one, and was removed. Do not reintroduce a version
//! field unless something actually makes a decision from it.

use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr};
use uuid::Uuid;
use yagra_common::{
    ArpColumn, ArpSummary, DnsChain, DnsRecordType, Duplex, ExpectedStatus, HostSample, HttpAuth,
    HttpMethod, IfIndex, InterfaceField, L3Column, L3Snapshot, MerakiTier, MetricKind,
    NeighborColumn, NeighborSet, NodeId, OpticalFlavor, RoutingColumn, RoutingSnapshot, SeriesKey,
};

/// Capability token a poller advertises in [`HeartbeatMsg::caps`] when it attaches the original
/// datagram to passive events ([`EventMsg::raw`], ADR-034). Core requires it before promising a
/// forwarding destination byte-exact output.
pub const CAP_RAW_CAPTURE: &str = "raw-capture";

/// Capability token a poller advertises in [`HeartbeatMsg::caps`] when it relays received flow-export
/// datagrams verbatim on [`crate::subjects::flows_raw`] ([`RawFlowDatagram`], ADR-034 Increment 2).
/// Distinct from [`CAP_RAW_CAPTURE`] because the two shipped separately: an Increment-1 poller
/// attaches raw bytes to events but publishes no flow datagrams at all, so a flow destination fed
/// only by such pollers receives nothing rather than degraded output.
pub const CAP_FLOW_RELAY: &str = "flow-relay";

/// Capability token a poller advertises in [`HeartbeatMsg::caps`] when it can present credentials
/// on a URL check ([`HttpCheck::auth`]).
///
/// This gate exists because the failure mode is a *false alert*, not degraded output. A poller that
/// does not understand `auth` drops the field and probes the endpoint anonymously; the endpoint
/// answers 401; `http_up` goes to 0 and the operator is paged for a service that is fine. So core
/// withholds an authenticated URL check from any pool with no live poller advertising this, and
/// records a monitoring gap instead — which is the whole point of the rolling-upgrade contract:
/// upgrading pollers must not require the operator to notice.
pub const CAP_HTTP_AUTH: &str = "http-auth";

/// Capability token a poller advertises in [`HeartbeatMsg::caps`] when it can read a URL check's
/// response body and apply a keyword rule to it ([`HttpCheck::body_match`], ADR-047 Inc.2).
///
/// The failure mode is the *inverse* of [`CAP_HTTP_AUTH`]'s and worse. A poller that does not
/// understand `body_match` drops the field, never reads the body, and reports `http_up = 1` — which
/// is byte-identical to "the content rule passed". The operator configured a check for the page
/// that returns 200 while saying `Database unavailable`, and gets a green dashboard for exactly the
/// outage they were guarding against. So core withholds such a monitor from a poller that has not
/// claimed this, producing a visible gap instead of an invisible false OK.
///
/// Separate from [`CAP_HTTP_AUTH`] because the two are independent: a monitor may carry either,
/// both, or neither, and a fleet mid-upgrade can advertise one without the other.
pub const CAP_HTTP_BODY: &str = "http-body";

/// Capability token a poller advertises in [`HeartbeatMsg::caps`] when it can replace itself on
/// command: a site updater is deployed beside it and it has seen that updater's heartbeat (ADR-051).
///
/// Declared, never inferred, and the reason is the same one that made `allow_bundle` a declaration
/// in ADR-050 — the side holding the Docker socket is the side that knows whether it exists. Core
/// could guess from a remote poller's address or its pool and would be wrong in both directions: a
/// site that deliberately left the sidecar commented out looks identical on the bus to one that
/// enabled it. Absence therefore means "cannot", so the worst a missing claim costs is a poller that
/// keeps being listed as needing a hand — never a command sent into a container that cannot act on
/// it, and never a version skew nobody was told about.
pub const CAP_SELF_UPGRADE: &str = "self-upgrade";

/// Capability token a poller advertises in [`HeartbeatMsg::caps`] when it can answer a
/// [`PollerLogRequest`] with its own on-disk log (ADR-045 Inc.4).
///
/// Conditional, like [`CAP_SELF_UPGRADE`] and unlike the four unconditional ones: a poller writes
/// log files only when `YAGRA_LOG_DIR` is set, and asking one that has none produces an empty reply
/// indistinguishable from a poller that is not listening. Core reads absence as "will not answer"
/// and says so in the bundle's omissions **naming the poller**, which is the whole difference
/// between "this site sent nothing" and "there was nothing to send".
pub const CAP_LOG_SHIP: &str = "log-ship";

/// Capability token a poller advertises in [`HeartbeatMsg::caps`] when it honours a
/// [`DiscoveryCancel`] (ADR-068 Increment 2).
///
/// Unlike every other capability here, core **cannot use this to withhold anything** — a sweep is
/// queue-delivered, so core does not know which poller will run it and cannot check that one's
/// claim before publishing. The token buys exactly one thing: telling the operator, *before* they
/// press stop, whether every poller that might be running the sweep understands the command. So it
/// answers "will this work?" and never "should I send it?".
///
/// ⚠️ **It is an approximation and the API field is named for that** (`poller_supports_cancel`, not
/// `will_stop`). For a sweep on the global subject it is the answer across every live poller, and
/// even for a pool-scoped one it says the pool can stop sweeps — not that the poller holding *this*
/// sweep will. The unambiguous signal is the poller's own terminal result carrying
/// [`DiscoveryResult::cancelled`].
pub const CAP_DISCOVERY_CANCEL: &str = "discovery-cancel";

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

    /// A new DNS name-resolution poll job for `node` (ADR-033). The real target is the resolver in
    /// `check` (or the poller's system resolver when it names none); `target` carries the node's
    /// display address, which for a DNS monitor is never pinged.
    #[must_use]
    pub fn dns(
        job_id: Uuid,
        node_id: NodeId,
        target: IpAddr,
        check: DnsCheck,
        interval_secs: u32,
    ) -> Self {
        Self {
            job_id,
            node_id,
            target,
            check: CheckSpec::Dns(check),
            interval_secs,
            credential_ref: None,
            probe_identity: false,
            trace_context: TraceContext::new(),
        }
    }

    /// Build an SNMP v2c CDP/LLDP neighbour-walk job (ADR-038).
    #[must_use]
    pub fn snmp_neighbors(
        job_id: Uuid,
        node_id: NodeId,
        target: IpAddr,
        check: SnmpNeighborCheck,
        interval_secs: u32,
    ) -> Self {
        Self {
            job_id,
            node_id,
            target,
            check: CheckSpec::SnmpNeighbors(check),
            interval_secs,
            credential_ref: None,
            probe_identity: false,
            trace_context: TraceContext::new(),
        }
    }

    /// Build an SNMP v2c optical-transceiver probe job (ADR-062).
    #[must_use]
    pub fn snmp_optical(
        job_id: Uuid,
        node_id: NodeId,
        target: IpAddr,
        check: SnmpOpticalCheck,
        interval_secs: u32,
    ) -> Self {
        Self {
            job_id,
            node_id,
            target,
            check: CheckSpec::SnmpOptical(check),
            interval_secs,
            credential_ref: None,
            probe_identity: false,
            trace_context: TraceContext::new(),
        }
    }

    /// Build an SNMP v3 (USM) optical-transceiver probe job (ADR-062).
    #[must_use]
    pub fn snmp_v3_optical(
        job_id: Uuid,
        node_id: NodeId,
        target: IpAddr,
        check: SnmpV3OpticalCheck,
        interval_secs: u32,
    ) -> Self {
        Self {
            job_id,
            node_id,
            target,
            check: CheckSpec::SnmpV3Optical(check),
            interval_secs,
            credential_ref: None,
            probe_identity: false,
            trace_context: TraceContext::new(),
        }
    }

    /// Build an SNMP v2c media-type walk job (ADR-063 Inc.2).
    #[must_use]
    pub fn snmp_mau(
        job_id: Uuid,
        node_id: NodeId,
        target: IpAddr,
        check: SnmpMauCheck,
        interval_secs: u32,
    ) -> Self {
        Self {
            job_id,
            node_id,
            target,
            check: CheckSpec::SnmpMau(check),
            interval_secs,
            credential_ref: None,
            probe_identity: false,
            trace_context: TraceContext::new(),
        }
    }

    /// Build an SNMP v3 (USM) media-type walk job (ADR-063 Inc.2).
    #[must_use]
    pub fn snmp_v3_mau(
        job_id: Uuid,
        node_id: NodeId,
        target: IpAddr,
        check: SnmpV3MauCheck,
        interval_secs: u32,
    ) -> Self {
        Self {
            job_id,
            node_id,
            target,
            check: CheckSpec::SnmpV3Mau(check),
            interval_secs,
            credential_ref: None,
            probe_identity: false,
            trace_context: TraceContext::new(),
        }
    }

    /// Build an SNMP v3 (USM) CDP/LLDP neighbour-walk job (ADR-038).
    #[must_use]
    pub fn snmp_v3_neighbors(
        job_id: Uuid,
        node_id: NodeId,
        target: IpAddr,
        check: SnmpV3NeighborCheck,
        interval_secs: u32,
    ) -> Self {
        Self {
            job_id,
            node_id,
            target,
            check: CheckSpec::SnmpV3Neighbors(check),
            interval_secs,
            credential_ref: None,
            probe_identity: false,
            trace_context: TraceContext::new(),
        }
    }

    /// Build an SNMP v2c interface-address walk job (ADR-043).
    #[must_use]
    pub fn snmp_l3(
        job_id: Uuid,
        node_id: NodeId,
        target: IpAddr,
        check: SnmpL3Check,
        interval_secs: u32,
    ) -> Self {
        Self {
            job_id,
            node_id,
            target,
            check: CheckSpec::SnmpL3(check),
            interval_secs,
            credential_ref: None,
            probe_identity: false,
            trace_context: TraceContext::new(),
        }
    }

    /// Build an SNMP v3 (USM) interface-address walk job (ADR-043).
    #[must_use]
    pub fn snmp_v3_l3(
        job_id: Uuid,
        node_id: NodeId,
        target: IpAddr,
        check: SnmpV3L3Check,
        interval_secs: u32,
    ) -> Self {
        Self {
            job_id,
            node_id,
            target,
            check: CheckSpec::SnmpV3L3(check),
            interval_secs,
            credential_ref: None,
            probe_identity: false,
            trace_context: TraceContext::new(),
        }
    }

    /// Build an SNMP v2c ARP / IPv6-neighbour walk job (ADR-043 Increment 3).
    #[must_use]
    pub fn snmp_arp(
        job_id: Uuid,
        node_id: NodeId,
        target: IpAddr,
        check: SnmpArpCheck,
        interval_secs: u32,
    ) -> Self {
        Self {
            job_id,
            node_id,
            target,
            check: CheckSpec::SnmpArp(check),
            interval_secs,
            credential_ref: None,
            probe_identity: false,
            trace_context: TraceContext::new(),
        }
    }

    /// Build an SNMP v3 (USM) ARP / IPv6-neighbour walk job (ADR-043 Increment 3).
    #[must_use]
    pub fn snmp_v3_arp(
        job_id: Uuid,
        node_id: NodeId,
        target: IpAddr,
        check: SnmpV3ArpCheck,
        interval_secs: u32,
    ) -> Self {
        Self {
            job_id,
            node_id,
            target,
            check: CheckSpec::SnmpV3Arp(check),
            interval_secs,
            credential_ref: None,
            probe_identity: false,
            trace_context: TraceContext::new(),
        }
    }

    /// Build an SNMP v2c routing-adjacency job (ADR-043 Increment 4).
    #[must_use]
    pub fn snmp_routing(
        job_id: Uuid,
        node_id: NodeId,
        target: IpAddr,
        check: SnmpRoutingCheck,
        interval_secs: u32,
    ) -> Self {
        Self {
            job_id,
            node_id,
            target,
            check: CheckSpec::SnmpRouting(check),
            interval_secs,
            credential_ref: None,
            probe_identity: false,
            trace_context: TraceContext::new(),
        }
    }

    /// Build an SNMP v3 (USM) routing-adjacency job (ADR-043 Increment 4).
    #[must_use]
    pub fn snmp_v3_routing(
        job_id: Uuid,
        node_id: NodeId,
        target: IpAddr,
        check: SnmpV3RoutingCheck,
        interval_secs: u32,
    ) -> Self {
        Self {
            job_id,
            node_id,
            target,
            check: CheckSpec::SnmpV3Routing(check),
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
// contract for that; they follow the same conventions as the rest of this file
// (`#[serde(default)]` on optional fields, no `deny_unknown_fields`), so they stay N/N-1
// tolerant during a rolling upgrade (ADR-017).

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
    /// identity (`job_id`, `credential_ref`).
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

    /// Re-hydrate a dispatchable [`PollJob`] from this spec, stamping the given `job_id`.
    /// Credentials are resolved/inlined by core separately, so `credential_ref` is left
    /// `None` here.
    #[must_use]
    pub fn to_job(&self, job_id: Uuid) -> PollJob {
        PollJob {
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
    /// Every polling spec core wants this poller to run for the node. Decoded **per element**
    /// (see [`de_lenient_specs`]) so one spec this binary can't understand never costs us the
    /// whole message.
    #[serde(deserialize_with = "de_lenient_specs")]
    pub specs: Vec<JobSpec>,
}

/// Deserialize a node's specs, **dropping any element this binary can't decode** instead of
/// failing the whole message.
///
/// This is what makes adding a [`CheckSpec`] variant safe during a rolling upgrade. A [`SyncMsg`]
/// is decoded whole, so without this a single unknown `check.kind` inside one snapshot chunk
/// would fail that entire chunk — the poller would then see a `seq` gap, request a resync, receive
/// the very same chunk again, and loop forever, stalling **all** of its polling rather than just
/// the one spec it didn't understand. Per-element decoding degrades gracefully instead: the poller
/// runs everything it understands and simply doesn't poll what it doesn't.
///
/// The JSON wire form is unchanged — this is purely deserialization tolerance, so it is N/N-1 safe
/// on its own and is meant to ship *before* any new `CheckSpec` variant does (ADR-017).
fn de_lenient_specs<'de, D>(deserializer: D) -> Result<Vec<JobSpec>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<serde_json::Value>::deserialize(deserializer)?;
    let total = raw.len();
    let specs: Vec<JobSpec> = raw
        .into_iter()
        .filter_map(|v| serde_json::from_value::<JobSpec>(v).ok())
        .collect();
    if specs.len() < total {
        // Expected only when a newer core talks to an older poller mid-rollout; the fix is to
        // finish upgrading the pollers, so say that rather than just reporting a count.
        tracing::warn!(
            dropped = total - specs.len(),
            total,
            "skipped working-set specs this binary cannot decode — upgrade this poller"
        );
    }
    Ok(specs)
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
    /// Optional capabilities this poller build advertises, e.g. [`CAP_RAW_CAPTURE`]. Lets core tell
    /// "this poller cannot do X" apart from "this poller is misconfigured" during a rolling upgrade
    /// (ADR-017) instead of silently degrading — the Forwarding page surfaces the difference.
    /// Empty from an N-1 poller.
    #[serde(default)]
    pub caps: Vec<String>,
    /// The poller host's own resource sample (CPU/load/memory/disk) for self-observability. `None`
    /// from an N-1 poller that predates host telemetry — core then simply shows no host data for
    /// it. Core is the single writer of the resulting `yagra_host_*` series to the TSDB (remote
    /// pollers can't reach it directly), so this heartbeat field is that path.
    #[serde(default)]
    pub host: Option<HostSample>,
    /// Set on the **final** beat a poller sends, immediately before it exits on SIGTERM.
    ///
    /// Without it a shutdown is indistinguishable from a network partition, so core waits out
    /// [`OFFLINE_AFTER_SECS`] (three missed beats) before dropping the poller from its pool's hash
    /// ring. During a rolling upgrade that is up to 30 seconds in which the departing poller's
    /// nodes are assigned to something that is no longer running — and if the restart finishes
    /// inside the window, core never reassigns at all and the nodes are simply unpolled for the
    /// duration. One flag turns that into an immediate, deliberate hand-off.
    ///
    /// A field on the existing heartbeat rather than a new message on purpose: a new subject would
    /// need a matching `yagra-authz` publish grant, whose absence fails at runtime with no compile
    /// error. The poller already holds publish rights here.
    ///
    /// An N-1 poller never sets it (falling back to timeout detection) and an N-1 core ignores it,
    /// so the two upgrade in either order.
    #[serde(default)]
    pub leaving: bool,
    /// The poller's own non-loopback interface addresses — where it sits on the network (ADR-043).
    ///
    /// Core needs this to root the derived dependency graph: a node's parents are the nodes
    /// immediately before it on the path *from a poller*, so without knowing where the poller is
    /// there is no direction to derive and the graph has no roots. Every inventory node sharing a
    /// subnet with one of these addresses is an anchor — that is not a guess about which box is the
    /// first hop, it is the statement that a node on the poller's own segment has nothing upstream
    /// of it.
    ///
    /// No prefix length travels with them on purpose: containment is evaluated against each
    /// *node's* prefix, which core already stores, so the wire carries the one fact only the poller
    /// knows.
    ///
    /// ⚠️ A containerized poller reports its bridge address (`172.18.0.x`) and matches nothing. That
    /// is the common case, not the exception, which is why `pollers.anchor_node_id` exists and why
    /// an unresolved anchor blocks derived suppression outright rather than quietly producing a
    /// rootless graph. Empty from an N-1 poller — indistinguishable from "matched nothing", and
    /// both are handled the same way.
    #[serde(default)]
    pub mgmt_addrs: Vec<IpAddr>,
}

/// A poller's request for a fresh full snapshot, published on [`crate::subjects::sync_request`]
/// (ADR-020). Sent at startup and whenever the poller detects a gap / epoch mismatch. This is
/// plain pub/sub, **not** request-reply, because the reply is several snapshot chunks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncRequest {
    /// Sanitized id of the poller requesting the snapshot.
    pub poller_id: String,
    /// Pool the poller serves.
    pub pool: String,
    /// The poller's current incarnation.
    pub incarnation: Uuid,
}

/// Which half of an upgrade a [`PollerUpgradeMsg`] is asking for.
///
/// Split because the two have completely different costs to a site. Fetching an image over a WAN
/// link is minutes and costs no monitoring at all; recreating the container is seconds and is the
/// entire outage. Doing them as one step would make a single-poller pool dark for the download,
/// which is the case ADR-051 decision 13 exists to shrink.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpgradeStep {
    /// Fetch the release's images and stop. The poller keeps running and keeps polling.
    Prefetch,
    /// Install what is already local: recreate the containers from the target composition.
    ///
    /// The default, so a message from a producer that predates the split still means "do the
    /// upgrade" rather than silently meaning "download it and never install it" — a failure that
    /// would look exactly like success from core's side until the version never changed.
    #[default]
    Apply,
}

/// Core telling one poller to replace itself with a published release (ADR-051).
///
/// Published on [`crate::subjects::upgrade_for`] — its own subject, addressed to a single poller, so
/// a build that has never heard of it simply does not subscribe and nothing happens. That is the
/// whole N-1 story for this family: no version field, no capability check on the receiving side, no
/// way for the command to disturb the working-set stream it travels beside.
///
/// **The poller executes none of this.** It validates the fields and writes them into the hand-off
/// file its site updater reads (ADR-050's format, unchanged), because the container that holds the
/// Docker socket must never be the container that talks to the network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollerUpgradeMsg {
    /// Sanitized id of the poller this is addressed to. Redundant with the subject on purpose: a
    /// poller that receives one for someone else drops it rather than acting on a routing mistake.
    pub poller_id: String,
    /// The run this belongs to — core's upgrade run id, so the site's audit trail and the central
    /// one name the same operation.
    pub run_id: String,
    /// Release tag to install, e.g. `v0.2.3`. Validated on both sides (defence in depth: the poller
    /// checks it before writing the file, and the updater checks it again before using it).
    pub tag: String,
    /// Who asked, for the site's own audit line. Empty when core cannot attribute it.
    #[serde(default)]
    pub requested_by: String,
    /// Unix seconds when core issued it.
    #[serde(default)]
    pub requested_at: i64,
    /// Fetch only, or install (see [`UpgradeStep`]).
    #[serde(default)]
    pub step: UpgradeStep,
}

/// Core asking one poller for a window of its own on-disk log (ADR-045 Inc.4).
///
/// Published on [`crate::subjects::poller_logs_for`] — its own subject family addressed to a single
/// poller, for the same reason [`PollerUpgradeMsg`] has one: a build that has never heard of it does
/// not subscribe, so the request reaches nobody rather than being mis-parsed. Core absorbs the
/// silence with a deadline and names the poller in the bundle's omissions.
///
/// **This is the half of the support bundle the disk cannot reach.** A co-located poller's log is
/// read straight off the shared volume (Inc.3); a poller at another site has its own disk, and this
/// is the only way its log crosses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollerLogRequest {
    /// Sanitized id of the poller this is addressed to. Redundant with the subject on purpose,
    /// exactly as [`PollerUpgradeMsg::poller_id`] is: a reply must never be attributed to the wrong
    /// site because of a routing mistake.
    pub poller_id: String,
    /// Correlates every [`PollerLogChunk`] of the answer. Core has one collector per request and
    /// routes replies by this.
    pub request_id: Uuid,
    /// Oldest log file to consider, as Unix seconds. Files last written before it are skipped.
    pub since_unix_s: i64,
    /// How many **raw** log bytes this poller may send in total. It stops at the cap and says so, so
    /// one verbose site cannot fill the bundle.
    pub max_bytes: u64,
}

/// One slice of a poller's answer to a [`PollerLogRequest`] (ADR-045 Inc.4).
///
/// Chunked because NATS's default maximum payload is 1 MB and an hour of a busy poller's JSON lines
/// is larger than that. The shape follows [`DiscoveryResult`]'s: a flat struct with a terminal flag
/// rather than an enum, so a field added later is ignored by an older consumer instead of failing
/// the decode.
///
/// **A refusal is also an answer.** When the poller's own secret scan matches, it sends a single
/// chunk carrying [`PollerLogChunk::refused`] and no bytes — see that field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollerLogChunk {
    /// Echoes [`PollerLogRequest::request_id`].
    pub request_id: Uuid,
    /// Which poller this is from. Core carries it into the archive path, so a bundle from a fleet
    /// says which site each file came from.
    pub poller_id: String,
    /// Position in this reply, from 0. Core reassembles in this order and reports a gap rather than
    /// silently concatenating out of order.
    pub seq: u32,
    /// Set on the final chunk of the reply, including a refusal or an empty answer. Core stops
    /// waiting for this poller when it arrives.
    pub last: bool,
    /// The log file this slice belongs to, e.g. `yagra-poller-edge-1.2026-08-16-10.log`. Empty on a
    /// terminal marker that carries no bytes.
    #[serde(default)]
    pub name: String,
    /// This slice of `name`, base64-encoded ([`encode_raw`]). Base64 rather than a `String` because
    /// a chunk boundary falls at an arbitrary byte offset and would otherwise split a multi-byte
    /// character; the bytes are reassembled before anyone reads them as text.
    #[serde(default)]
    pub bytes: String,
    /// Why this poller sent nothing, when it chose not to.
    ///
    /// Names the **rule**, never the value — the same contract as
    /// [`crate::messages`]'s peers and as core's own redaction refusal, because this string is
    /// logged and then written into the bundle a human reviews. The poller refuses rather than
    /// redacts for the reason ADR-045 決定 4 gives: redacting assumes the pattern set is complete.
    #[serde(default)]
    pub refused: Option<String>,
}

impl PollerLogChunk {
    /// Decode [`Self::bytes`] back to the slice the poller read. `None` when the field is not valid
    /// base64 — a corrupt chunk drops that slice rather than taking down the collector, the same
    /// reaction [`EventMsg::raw_bytes`] has.
    #[must_use]
    pub fn slice(&self) -> Option<Vec<u8>> {
        if self.bytes.is_empty() {
            return Some(Vec::new());
        }
        data_encoding::BASE64.decode(self.bytes.as_bytes()).ok()
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
    /// SNMP v3 (USM) GETBULK walk of table columns — the v3 analogue of [`CheckSpec::SnmpTable`].
    /// Like the variants around it, an older poller that doesn't know this tag simply skips the
    /// job (N-1 compatible): a v3 node just keeps getting scalars + ICMP until the poller upgrades.
    SnmpV3Table(SnmpV3TableCheck),
    /// SNMP v2c optical-transceiver (DDM/DOM) probe — receive/transmit power in dBm (ADR-062).
    ///
    /// Kept out of [`CheckSpec::SnmpTable`] because one OID does not make one metric: the
    /// vendor-neutral spelling correlates five ENTITY-SENSOR-MIB columns, and most dialects key
    /// their rows by `entPhysicalIndex`, which the poller translates to a real ifIndex before
    /// publishing so everything downstream sees an ordinary per-interface gauge.
    ///
    /// Runs at the **normal metric interval**, unlike [`CheckSpec::SnmpNeighbors`] — optical power
    /// drifts continuously with temperature and age, and it shares a chart with throughput, so a
    /// slower cadence would draw a sparser line against the same time axis for no saving worth
    /// having. The result is **observational** ([`PollResult::observational`]): a transceiver that
    /// will not answer says nothing about whether the device is up.
    SnmpOptical(SnmpOpticalCheck),
    /// SNMP v3 (USM) analogue of [`CheckSpec::SnmpOptical`], following the same v2c/v3 pairing as
    /// every other SNMP check here (split by credential shape, not by walk logic).
    SnmpV3Optical(SnmpV3OpticalCheck),
    /// Read each Ethernet port's media type — `ifMauTable`, falling back to ENTITY-MIB's
    /// transceiver strings (ADR-063 Inc.2).
    ///
    /// Kept out of [`CheckSpec::SnmpTable`] for a hard technical reason, not for tidiness:
    /// `ifMauTable` is indexed by `(ifMauIfIndex, ifMauIndex)`, and the ordinary walkers fold any
    /// multi-subid tail into a hash — so the ifIndex is destroyed before the poller sees it. This
    /// check goes through the instance walker instead, the same path ADR-038's neighbour walk uses.
    ///
    /// Runs on the **slow cadence** like [`CheckSpec::SnmpNeighbors`] and unlike
    /// [`CheckSpec::SnmpOptical`]: a port's medium changes when someone swaps a module, not
    /// continuously. The result is **observational** ([`PollResult::observational`]) — a device that
    /// does not implement MAU-MIB is not unreachable, and most do not.
    SnmpMau(SnmpMauCheck),
    /// SNMP v3 (USM) analogue of [`CheckSpec::SnmpMau`].
    SnmpV3Mau(SnmpV3MauCheck),
    /// HTTP/HTTPS URL-endpoint check (status/up + TLS cert expiry). Like the variants above,
    /// an older poller that doesn't know this tag simply skips the job (N-1 compatible).
    Http(HttpCheck),
    /// Cisco Meraki org-scoped collector: page one tier of Dashboard org-bulk endpoints and fan
    /// the result out to many nodes. Read-only (GET). Like the variants above, an older poller that
    /// doesn't know this tag simply skips the job (N-1 compatible).
    MerakiCollect(MerakiCollectCheck),
    /// DNS name-resolution check: resolve a name through one recursive resolver, following the
    /// CNAME chain so every hop is observable (ADR-033). Like the variants above, an older poller
    /// that doesn't know this tag simply skips the job (N-1 compatible) — and since
    /// [`NodeJobs::specs`] decodes per element, an unknown tag inside a working-set chunk costs
    /// only this spec rather than the whole chunk.
    Dns(DnsCheck),
    /// SNMP v2c walk of the CDP/LLDP neighbour tables (ADR-038), on its own slow cadence.
    ///
    /// Kept out of [`CheckSpec::SnmpTable`] rather than folded into it: adjacency changes on the
    /// order of months, so riding the interface-metric interval would walk `lldpRemTable` on a
    /// 48-port switch every minute for nothing — device load and rate-limit budget spent on a
    /// constant. The result is **observational** ([`PollResult::observational`]): it makes no
    /// statement about the node's reachability.
    SnmpNeighbors(SnmpNeighborCheck),
    /// SNMP v3 (USM) analogue of [`CheckSpec::SnmpNeighbors`], following the
    /// `SnmpTable`/`SnmpV3Table` pairing.
    SnmpV3Neighbors(SnmpV3NeighborCheck),
    /// SNMP v2c walk of the interface-address tables (ADR-043), on the same slow cadence as the
    /// neighbour walk.
    ///
    /// This is what makes the connectivity graph derivable: two nodes with an address in the same
    /// prefix are L3-adjacent as a matter of fact, not inference. Kept off the interface-metric
    /// interval for the same reason [`CheckSpec::SnmpNeighbors`] is — addressing changes on the
    /// order of months. Deliberately *not* a routing-table walk: `ipCidrRouteTable` runs to
    /// hundreds of thousands of rows on a core router, whereas these tables have one row per
    /// interface address. The result is **observational** ([`PollResult::observational`]).
    SnmpL3(SnmpL3Check),
    /// SNMP v3 (USM) analogue of [`CheckSpec::SnmpL3`], following the same pairing as every other
    /// v2c/v3 pair here. Split by credential shape, not by walk logic: v2c carries a community
    /// string and v3 carries six USM fields, and folding them would mean one struct with seven
    /// mutually exclusive optional fields plus a poller that re-derives which protocol to speak.
    SnmpV3L3(SnmpV3L3Check),
    /// SNMP v2c walk of the ARP / IPv6-neighbour cache (ADR-043 Increment 3), on the slowest cadence
    /// of any check here.
    ///
    /// **The only check in ADR-043 that costs the device real work**, which is why it is the only
    /// one shipped disabled by default: `ipNetToPhysicalTable` is thousands of rows on a campus
    /// distribution switch, where `ipAddrTable` is tens. `max_rows` bounds it *inside* the paging
    /// loop rather than after collection. The result is **observational**
    /// ([`PollResult::observational`]) — an ARP walk says nothing about whether the device is up.
    SnmpArp(SnmpArpCheck),
    /// SNMP v3 (USM) analogue of [`CheckSpec::SnmpArp`], following the same pairing as every other
    /// v2c/v3 pair here.
    SnmpV3Arp(SnmpV3ArpCheck),
    /// SNMP v2c collection of routing adjacency (ADR-043 Increment 4): the links that share no
    /// subnet, and therefore the ones Increment 1's derivation structurally cannot see.
    ///
    /// Two shapes in one check, because they answer one question and a device that speaks either
    /// speaks both on the same cadence. `columns` is walked (`bgpPeerState`, `ospfNbrState` — both
    /// indexed by the peer's address, which is why the *instance* walker is used); `route_probes`
    /// is not a walk at all but a list of pre-built subtree roots, one per destination, because
    /// `inetCidrRouteTable` runs to hundreds of thousands of rows on a core router and a bounded
    /// walk of it would return the numerically-first routes rather than the interesting ones. The
    /// result is **observational** ([`PollResult::observational`]).
    SnmpRouting(SnmpRoutingCheck),
    /// SNMP v3 (USM) analogue of [`CheckSpec::SnmpRouting`], following the same pairing as every
    /// other v2c/v3 pair here.
    SnmpV3Routing(SnmpV3RoutingCheck),
}

impl CheckSpec {
    /// Every plaintext credential this spec carries, for a fail-closed scan that must run where the
    /// credentials are (ADR-045 Inc.4).
    ///
    /// # Why this lives on the message type
    ///
    /// The support bundle's redaction scan is built from the *literal* secret values a process can
    /// see, because a pattern set only describes leaks somebody imagined. Core's set comes from its
    /// own environment — which is why it structurally cannot cover a device's SNMP community: that
    /// value is decrypted from the credential store and inlined **here**, and only the poller holding
    /// this spec has it in plaintext. So the scan over a poller's log has to run on the poller, and
    /// the poller needs this list.
    ///
    /// Written beside the fields rather than anywhere else for the reason `extensibility.md` §1
    /// gives: the match is exhaustive, so a variant added with a new credential field will not
    /// compile until somebody decides whether it belongs here. A `_ =>` arm would make the *safe*
    /// answer the silent one — a new credential simply stops being enforced, with every test green.
    ///
    /// Returns borrowed strings and includes duplicates; the caller dedupes and applies its own
    /// length floor. **Never log the result.**
    #[must_use]
    pub fn secret_literals(&self) -> Vec<&str> {
        // v3 (USM) carries the same two optional passphrases in every variant.
        fn v3<'a>(auth: Option<&'a str>, private: Option<&'a str>) -> Vec<&'a str> {
            auth.into_iter().chain(private).collect()
        }
        match self {
            // Neither carries a credential: ICMP has no authentication, and a DNS query is
            // unauthenticated by construction.
            Self::Icmp(_) | Self::Dns(_) => Vec::new(),
            Self::Snmp(c) => vec![c.community.as_str()],
            Self::SnmpTable(c) => vec![c.community.as_str()],
            Self::SnmpOptical(c) => vec![c.community.as_str()],
            Self::SnmpMau(c) => vec![c.community.as_str()],
            Self::SnmpNeighbors(c) => vec![c.community.as_str()],
            Self::SnmpL3(c) => vec![c.community.as_str()],
            Self::SnmpArp(c) => vec![c.community.as_str()],
            Self::SnmpRouting(c) => vec![c.community.as_str()],
            Self::SnmpV3(c) => v3(c.auth_key.as_deref(), c.priv_key.as_deref()),
            Self::SnmpV3Table(c) => v3(c.auth_key.as_deref(), c.priv_key.as_deref()),
            Self::SnmpV3Optical(c) => v3(c.auth_key.as_deref(), c.priv_key.as_deref()),
            Self::SnmpV3Mau(c) => v3(c.auth_key.as_deref(), c.priv_key.as_deref()),
            Self::SnmpV3Neighbors(c) => v3(c.auth_key.as_deref(), c.priv_key.as_deref()),
            Self::SnmpV3L3(c) => v3(c.auth_key.as_deref(), c.priv_key.as_deref()),
            Self::SnmpV3Arp(c) => v3(c.auth_key.as_deref(), c.priv_key.as_deref()),
            Self::SnmpV3Routing(c) => v3(c.auth_key.as_deref(), c.priv_key.as_deref()),
            // Only the secret half of each scheme. The username, the header *name* and the URL are
            // structural — they identify which account, not how to use it — and are exactly what a
            // log has to keep saying for a misconfiguration to stay diagnosable. `HttpAuth`'s manual
            // `Debug` draws the same line.
            Self::Http(c) => match c.auth.as_ref() {
                Some(yagra_common::HttpAuth::Basic { password, .. }) => vec![password.as_str()],
                Some(yagra_common::HttpAuth::Bearer { token }) => vec![token.as_str()],
                Some(yagra_common::HttpAuth::Header { value, .. }) => vec![value.as_str()],
                None => Vec::new(),
            },
            Self::MerakiCollect(c) => vec![c.api_key.as_str()],
        }
    }
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

/// DNS name-resolution check parameters (ADR-033). The real target is `resolver` (or, when that is
/// `None`, the poller container's system resolver); the enclosing [`PollJob::target`] stays the
/// node's display address, which a DNS monitor never pings. Every optional field is defaulted so an
/// N-1 core that omits it still produces a runnable check (ADR-017).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsCheck {
    /// The name to resolve, normalized by core before dispatch.
    pub name: String,
    /// Which record type the chain must reach (default `A`).
    #[serde(default)]
    pub record_type: DnsRecordType,
    /// Recursive resolver to query; `None` ⇒ the poller's system resolver.
    #[serde(default)]
    pub resolver: Option<IpAddr>,
    /// Resolver port (default 53).
    #[serde(default = "default_dns_port")]
    pub resolver_port: u16,
    /// Maximum CNAME hops before giving up (default 8).
    #[serde(default = "default_dns_max_depth")]
    pub max_depth: u8,
    /// **Total** budget for the whole chain walk, in milliseconds (default 3000).
    #[serde(default = "default_dns_timeout_ms")]
    pub timeout_ms: u32,
}

const fn default_dns_port() -> u16 {
    53
}
const fn default_dns_max_depth() -> u8 {
    8
}
const fn default_dns_timeout_ms() -> u32 {
    3000
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
    /// Resolved credentials to present, if the monitor is bound to one. Core decrypts and inlines
    /// this exactly as it does SNMP auth; the poller never reads a credential store.
    ///
    /// A field rather than a new [`CheckSpec`] variant, so an N-1 poller drops it rather than
    /// failing to decode the whole working-set chunk. It would then probe *unauthenticated* and
    /// read a 401 as "down", so core withholds authenticated checks from pollers that do not
    /// advertise [`CAP_HTTP_AUTH`] — see that constant.
    #[serde(default)]
    pub auth: Option<HttpAuth>,
    /// Keyword rule to apply to the response body, if the monitor has one (ADR-047 Inc.2).
    ///
    /// A field rather than a new [`CheckSpec`] variant for the same reason `auth` is one: this is a
    /// parameter of the same HTTP probe, and a variant would duplicate the seven fields above it.
    /// Gated on [`CAP_HTTP_BODY`], because an N-1 poller drops it and reports a green result it
    /// never computed.
    #[serde(default)]
    pub body_match: Option<yagra_common::BodyMatch>,
    /// Values to lift out of a JSON response body as operator-named gauges (ADR-047 Inc.3).
    ///
    /// **Not** capability-gated, unlike `body_match`: a poller that drops this records no sample,
    /// so the failure is a visibly absent series rather than a wrong reading. See
    /// `coordinator::spec_required_caps` for why withholding would be the worse trade here.
    #[serde(default)]
    pub json_extract: Vec<yagra_common::JsonExtract>,
    /// How many bytes of the response body the poller may read, for whichever of the two body
    /// features is configured.
    #[serde(default = "default_body_max_bytes")]
    pub body_max_bytes: u32,
}

impl HttpCheck {
    /// How much of the response body to capture, or `None` to skip reading it entirely.
    ///
    /// The rule lives here, at its only point of use, rather than beside the stored config as well:
    /// "does anything need the body" is one fact, and a second copy would disagree the first time a
    /// third body-reading feature lands.
    #[must_use]
    pub fn body_capture_bytes(&self) -> Option<u32> {
        (self.body_match.is_some() || !self.json_extract.is_empty()).then_some(self.body_max_bytes)
    }
}

const fn default_http_timeout_ms() -> u32 {
    5000
}

const fn default_body_max_bytes() -> u32 {
    yagra_common::DEFAULT_BODY_MAX_BYTES
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

/// SNMP v2c neighbour-walk parameters (ADR-038).
///
/// `columns` is the fixed LLDP-MIB / CISCO-CDP-MIB list from
/// [`yagra_common::builtin_neighbor_columns`], sent explicitly rather than assumed by the poller so
/// core stays the one place the OID set is decided (the same reason `meta_columns` is on the wire
/// in [`SnmpTableCheck`]). A poller that receives a column it has no handling for simply ignores
/// that column's rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpNeighborCheck {
    /// SNMP v2c community string (resolved/decrypted by core).
    pub community: String,
    /// Neighbour table columns to walk, keeping raw instance indices and raw octets.
    #[serde(default)]
    pub columns: Vec<SnmpNeighborColumn>,
    /// Per-request timeout, in milliseconds.
    #[serde(default = "default_snmp_timeout_ms")]
    pub timeout_ms: u32,
}

/// One optical dialect to probe, and the metric names its two readings publish under.
///
/// `rx_metric`/`tx_metric` are `Option` rather than fixed constants because an operator can
/// disable either half of the built-in template at node scope, and because core stays the one
/// place a TSDB metric name is decided (the same reason `meta_columns` travels on the wire).
/// Both `None` is a probe with nothing to do; core does not emit one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpticalProbe {
    /// Which vendor dialect to read.
    pub flavor: OpticalFlavor,
    /// Metric name for receive power, or `None` to skip it.
    #[serde(default)]
    pub rx_metric: Option<String>,
    /// Metric name for transmit power, or `None` to skip it.
    #[serde(default)]
    pub tx_metric: Option<String>,
    /// Metric name for **chassis temperature**, or `None` to skip it (ADR-070 decision 2).
    ///
    /// Only the correlated sensor dialects can produce this, and it is not an optical reading at
    /// all — it is the rows of the same sensor table that do *not* belong to a port. It rides this
    /// probe because it is literally the same walk: splitting it into its own `CheckSpec` would
    /// make a device answer the identical table twice per poll.
    ///
    /// `#[serde(default)]` is what keeps this N-1 safe in both directions: an older poller ignores
    /// the field, and an older core simply never sets it.
    #[serde(default)]
    pub temp_metric: Option<String>,
}

/// SNMP v2c optical-transceiver probe parameters (ADR-062).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpOpticalCheck {
    /// SNMP v2c community string (resolved/decrypted by core).
    pub community: String,
    /// Dialects to read. Normally one; a node bound to two vendor profiles gets both, and the
    /// poller publishes whichever answers.
    #[serde(default)]
    pub probes: Vec<OpticalProbe>,
    /// Per-request timeout, in milliseconds.
    #[serde(default = "default_snmp_timeout_ms")]
    pub timeout_ms: u32,
}

/// SNMP v3 (USM) optical-transceiver probe parameters — the v3 analogue of [`SnmpOpticalCheck`].
/// Auth/priv keys are resolved/decrypted by core and inlined here (ADR-018/020); the poller never
/// reads the secret store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpV3OpticalCheck {
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
    /// Dialects to read.
    #[serde(default)]
    pub probes: Vec<OpticalProbe>,
    /// Per-request timeout, in milliseconds.
    #[serde(default = "default_snmp_timeout_ms")]
    pub timeout_ms: u32,
}

/// SNMP v2c media-type walk parameters (ADR-063 Inc.2).
///
/// Carries no column list, unlike [`SnmpTableCheck`]: the OID set is a fixed standard
/// (`ifMauTable`, plus ENTITY-MIB as the fallback) with nothing for an operator to tune, exactly as
/// [`SnmpNeighborCheck`] argued for the LLDP/CDP set. It could not be a collection template either —
/// a `CollectionItem` declares a TSDB series, and a media type is a string attribute.
///
/// **Its own `CheckSpec` variant rather than a field on the table check, and that is the N-1 safe
/// direction.** `de_lenient_specs` decodes per `JobSpec` element, so a poller that has never heard
/// of this variant drops exactly this one spec and keeps collecting everything else. Widening an
/// existing spec would instead have taken the whole `SnmpTable` down with it — the trap ADR-063
/// Inc.1 documents on `InterfaceField`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpMauCheck {
    /// SNMP v2c community string (resolved/decrypted by core).
    pub community: String,
    /// Whether to fall back to ENTITY-MIB's transceiver strings when `ifMauTable` answers nothing.
    ///
    /// Defaults **on**. It costs nothing when MAU answered (the fallback is not attempted) and it is
    /// the only source that reaches a device with no MAU-MIB at all — which, measured, includes the
    /// one SNMP device in this project's lab.
    #[serde(default = "default_true")]
    pub entity_fallback: bool,
    /// Per-request timeout, in milliseconds.
    #[serde(default = "default_snmp_timeout_ms")]
    pub timeout_ms: u32,
}

/// SNMP v3 (USM) media-type walk parameters — the v3 analogue of [`SnmpMauCheck`]. Auth/priv keys
/// are resolved/decrypted by core and inlined here (ADR-018/020); the poller never reads the secret
/// store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpV3MauCheck {
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
    /// See [`SnmpMauCheck::entity_fallback`].
    #[serde(default = "default_true")]
    pub entity_fallback: bool,
    /// Per-request timeout, in milliseconds.
    #[serde(default = "default_snmp_timeout_ms")]
    pub timeout_ms: u32,
}

/// SNMP v3 (USM) neighbour-walk parameters — the v3 analogue of [`SnmpNeighborCheck`]. Auth/priv
/// keys are resolved/decrypted by core and inlined here (ADR-018/020); the poller never reads the
/// secret store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpV3NeighborCheck {
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
    /// Neighbour table columns to walk.
    #[serde(default)]
    pub columns: Vec<SnmpNeighborColumn>,
    /// Per-request timeout, in milliseconds.
    #[serde(default = "default_snmp_timeout_ms")]
    pub timeout_ms: u32,
}

/// SNMP v2c interface-address walk parameters (ADR-043).
///
/// `columns` is the fixed RFC 1213 / RFC 4293 list from [`yagra_common::builtin_l3_columns`], sent
/// explicitly for the same reason [`SnmpNeighborCheck`]'s is: core stays the one place the OID set
/// is decided. A poller that receives a column it has no handling for ignores that column's rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpL3Check {
    /// SNMP v2c community string (resolved/decrypted by core).
    pub community: String,
    /// Address table columns to walk, keeping raw instance indices and raw octets — both tables
    /// carry the address itself in the row *index*, so a folded index would destroy the answer.
    #[serde(default)]
    pub columns: Vec<SnmpL3Column>,
    /// Per-request timeout, in milliseconds.
    #[serde(default = "default_snmp_timeout_ms")]
    pub timeout_ms: u32,
}

/// SNMP v3 (USM) interface-address walk parameters — the v3 analogue of [`SnmpL3Check`]. Auth/priv
/// keys are resolved/decrypted by core and inlined here (ADR-018/020); the poller never reads the
/// secret store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpV3L3Check {
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
    /// Address table columns to walk.
    #[serde(default)]
    pub columns: Vec<SnmpL3Column>,
    /// Per-request timeout, in milliseconds.
    #[serde(default = "default_snmp_timeout_ms")]
    pub timeout_ms: u32,
}

/// SNMP v2c ARP / IPv6-neighbour walk parameters (ADR-043 Increment 3).
///
/// Carries `max_rows` on the wire, which none of the other walk checks do. That is deliberate: the
/// other walks read tables whose size is a property of the device (one row per interface address,
/// one per LLDP peer), while this one reads a table whose size is a property of the *network* — and
/// core is where a fleet-wide bound belongs. A poller that receives a value it considers
/// unreasonable still applies it; the transport's own request ceiling is the backstop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpArpCheck {
    /// SNMP v2c community string (resolved/decrypted by core).
    pub community: String,
    /// Neighbour-cache columns to walk, keeping raw instance indices — both tables carry the
    /// address and the ifIndex in the row *index*, so a folded index would destroy the answer.
    #[serde(default)]
    pub columns: Vec<SnmpArpColumn>,
    /// Row budget for the whole walk, enforced while paging.
    #[serde(default = "default_arp_max_rows")]
    pub max_rows: u32,
    /// Per-request timeout, in milliseconds.
    #[serde(default = "default_snmp_timeout_ms")]
    pub timeout_ms: u32,
}

/// SNMP v3 (USM) ARP walk parameters — the v3 analogue of [`SnmpArpCheck`]. Auth/priv keys are
/// resolved/decrypted by core and inlined here (ADR-018/020); the poller never reads the secret
/// store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpV3ArpCheck {
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
    /// Neighbour-cache columns to walk.
    #[serde(default)]
    pub columns: Vec<SnmpArpColumn>,
    /// Row budget for the whole walk, enforced while paging.
    #[serde(default = "default_arp_max_rows")]
    pub max_rows: u32,
    /// Per-request timeout, in milliseconds.
    #[serde(default = "default_snmp_timeout_ms")]
    pub timeout_ms: u32,
}

/// SNMP v2c routing-adjacency parameters (ADR-043 Increment 4).
///
/// Carries a **pre-built** probe list rather than a list of addresses, so the OID grammar of
/// `inetCidrRouteTable`'s index stays a fact core owns — the same reason every other check here is
/// sent its column OIDs instead of deriving them poller-side. It also means a poller needs no
/// knowledge of `InetAddress` index encoding to run the probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpRoutingCheck {
    /// SNMP v2c community string (resolved/decrypted by core).
    pub community: String,
    /// Adjacency columns to walk, keeping raw instance indices — both tables carry the peer's
    /// address in the row *index*, so a folded index would destroy the answer.
    #[serde(default)]
    pub columns: Vec<SnmpRoutingColumn>,
    /// Targeted route probes: subtree roots that each cover the routes to exactly one destination.
    /// Empty is the normal case — only a node holding a host address of its own is asked to probe.
    #[serde(default)]
    pub route_probes: Vec<SnmpRouteProbe>,
    /// Per-request timeout, in milliseconds.
    #[serde(default = "default_snmp_timeout_ms")]
    pub timeout_ms: u32,
}

/// SNMP v3 (USM) routing-adjacency parameters — the v3 analogue of [`SnmpRoutingCheck`]. Auth/priv
/// keys are resolved/decrypted by core and inlined here (ADR-018/020); the poller never reads the
/// secret store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpV3RoutingCheck {
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
    /// Adjacency columns to walk.
    #[serde(default)]
    pub columns: Vec<SnmpRoutingColumn>,
    /// Targeted route probes — see [`SnmpRoutingCheck::route_probes`].
    #[serde(default)]
    pub route_probes: Vec<SnmpRouteProbe>,
    /// Per-request timeout, in milliseconds.
    #[serde(default = "default_snmp_timeout_ms")]
    pub timeout_ms: u32,
}

/// One routing-adjacency column to walk: which field it carries and the column base OID.
/// Relational metadata (PostgreSQL), never a TSDB label — the same tier as [`SnmpArpColumn`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpRoutingColumn {
    /// Which routing attribute this column carries.
    pub field: RoutingColumn,
    /// Column base OID, e.g. `1.3.6.1.2.1.15.3.1.2` (bgpPeerState).
    pub oid: String,
}

/// One targeted route probe: walk this subtree, and whatever it returns describes the route to
/// `target`.
///
/// `oid` is the column base with the destination's index prefix already appended, so the subtree it
/// roots contains every route to that destination and nothing else. `target` is carried alongside
/// rather than decoded back out of the OID — the poller has the answer already, and re-deriving it
/// would be a second implementation of the encoding core just performed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpRouteProbe {
    /// Which routing attribute the probed column carries.
    pub field: RoutingColumn,
    /// The subtree root to walk: `<column base>.<destType>.<addrLen>.<address octets>`.
    pub oid: String,
    /// The destination this probe asks about.
    pub target: IpAddr,
}

/// The row budget an N-1 core that predates the field is assumed to have meant.
const fn default_arp_max_rows() -> u32 {
    // `MAX_ARP_WALK_ROWS`, restated as a `u32` literal because `yagra_common`'s constant is a
    // `usize` and this must be a `const fn` serde can name. The round-trip test below pins them
    // together so the two cannot drift.
    4096
}

/// One neighbour-cache column to walk: which field it carries and the column base OID. Relational
/// metadata (PostgreSQL), never a TSDB label — the same tier as [`SnmpL3Column`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpArpColumn {
    /// Which neighbour-cache attribute this column carries.
    pub field: ArpColumn,
    /// Column base OID, e.g. `1.3.6.1.2.1.4.35.1.4` (ipNetToPhysicalPhysAddress).
    pub oid: String,
}

/// One address-table column to walk: which field it carries and the column base OID. Relational
/// metadata (PostgreSQL), never a TSDB label — the same tier as [`SnmpNeighborColumn`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpL3Column {
    /// Which address attribute this column carries.
    pub field: L3Column,
    /// Column base OID, e.g. `1.3.6.1.2.1.4.34.1.5` (ipAddressPrefix).
    pub oid: String,
}

/// One neighbour-table column to walk: which field it carries and the column base OID. The value
/// is relational metadata (PostgreSQL), never a TSDB label — the same tier as [`SnmpMetaColumn`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnmpNeighborColumn {
    /// Which neighbour attribute this column carries.
    pub field: NeighborColumn,
    /// Column base OID, e.g. `1.0.8802.1.1.2.1.4.1.1.5` (lldpRemChassisId).
    pub oid: String,
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
    /// The DNS resolution chain observed on this poll (DNS checks only, ADR-033). Structured
    /// metadata core persists into PostgreSQL — **never a TSDB label** (ADR-011), the same tier as
    /// `interfaces` and `sys_descr`. Defaulted so an older poller that doesn't send it stays N-1
    /// compatible; skipped when absent so every non-DNS result's wire form is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns_chain: Option<DnsChain>,
    /// The CDP/LLDP neighbours observed on this poll (neighbour walks only, ADR-038). Same tier as
    /// `dns_chain`: relational metadata core persists into PostgreSQL, **never** a TSDB label
    /// (ADR-011). Defaulted so an older poller stays N-1 compatible; skipped when absent so every
    /// other result's wire form is unchanged.
    ///
    /// `None` and `Some(empty set)` mean different things and core acts on the difference:
    /// `None` = "the walk did not produce a set" (it failed, or this was not a neighbour job) and
    /// nothing is written; `Some(empty)` = "this device reports no neighbours", which replaces the
    /// stored set. Conflating them would let one failed walk read as "every link disappeared".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub neighbors: Option<NeighborSet>,
    /// The interface addresses observed on this poll (L3 walks only, ADR-043). Same tier as
    /// `neighbors`: relational metadata core persists into PostgreSQL, **never** a TSDB label
    /// (ADR-011) — an IP in a series label is the cardinality explosion CLAUDE.md §7.1 names.
    /// Defaulted so an older poller stays N-1 compatible; skipped when absent so every other
    /// result's wire form is unchanged.
    ///
    /// `None` and `Some(empty snapshot)` mean different things and core acts on the difference,
    /// exactly as for `neighbors`: `None` = "the walk did not produce a snapshot" and nothing is
    /// written; `Some(empty)` = "this device reports no interface addresses", which replaces the
    /// stored snapshot. Conflating them would let one failed walk read as "every address
    /// disappeared" — and, one derivation later, as "every link disappeared".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub l3: Option<L3Snapshot>,
    /// The ARP / IPv6-neighbour cache observed on this poll (ARP walks only, ADR-043 Increment 3).
    /// Same tier as `l3` again — relational metadata, **never** a TSDB label.
    ///
    /// Already aggregated poller-side: a bounded endpoint sample plus per-port totals, not the raw
    /// table. That is the difference between a few kilobytes and several thousand rows per node per
    /// cycle on the bus, and the aggregation is deterministic so two pollers reading the same device
    /// publish the same summary.
    ///
    /// `None` and `Some(empty summary)` mean different things, exactly as for `neighbors` and `l3`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arp: Option<ArpSummary>,
    /// The routing adjacencies observed on this poll (routing walks only, ADR-043 Increment 4).
    /// Same tier as `arp` again — relational metadata, **never** a TSDB label.
    ///
    /// `None` and `Some(empty snapshot)` mean different things, exactly as for `neighbors`, `l3`
    /// and `arp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<RoutingSnapshot>,
    /// This result carries **observations only** and makes no claim about the node's reachability.
    ///
    /// Core skips the alert engine entirely for such results. That is not a nicety: `outcome` feeds
    /// the liveness state machine on *every* result, so an hourly neighbour walk that timed out
    /// would push `Unreachable` into the dwell window ICMP owns and page someone for a healthy
    /// device — while hard-coding `Reachable` instead would cancel a genuine outage. A check that
    /// has nothing to say about liveness has to be able to say nothing.
    ///
    /// Defaulted (and omitted from the wire when false) so every existing result is byte-identical
    /// and an N-1 poller's results keep driving alerts exactly as before (ADR-017).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub observational: bool,
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
    /// Negotiated duplex from `dot3StatsDuplexStatus`, if the device implements EtherLike-MIB
    /// (ADR-063 Inc.1).
    ///
    /// `None` covers "MIB absent", "port down so no row" and the agent's own `unknown(1)` alike —
    /// migration 0085 records why that collapse is deliberate. ⚠️ Expect `None` on optical ports:
    /// IEEE 802.3 defines no half duplex above 1 Gbit/s, so there is nothing to negotiate and an
    /// agent answering `unknown(1)` there is being accurate.
    ///
    /// Safe on the wire as a bare enum only because the set is closed — see [`Duplex`].
    #[serde(default)]
    pub if_duplex: Option<Duplex>,
    /// `ifType` (IANAifType) as the raw integer, if walked. Lets a reader tell "the question does
    /// not apply to this interface" from "we could not read it".
    #[serde(default)]
    pub if_type: Option<i32>,
    /// Canonical IEEE media designation — `1000BASE-T`, `10GBASE-SR` — from the MAU walk
    /// (ADR-063 Inc.2).
    ///
    /// A `String` rather than an enum: `dot3MauType` is an IANA registry of 250-and-growing
    /// designations that are byte-identical in every language. `None` for a registration the
    /// poller's table does not carry, which it logs rather than guessing at.
    #[serde(default)]
    pub if_media: Option<String>,
    /// The pluggable's vendor part string from ENTITY-MIB, verbatim — `SFP-1000BaseLX`.
    ///
    /// ⚠️ **Not a media type and never coerced into one.** It is kept as its own fact; it may
    /// *populate* `if_media` when it contains a canonical designation as a whole token, and
    /// otherwise stands alone. `None` for every fixed copper port, which has no pluggable.
    #[serde(default)]
    pub transceiver_model: Option<String>,
    /// Lowest receive power the transceiver considers acceptable, dBm (ADR-062 Inc.4).
    ///
    /// These four arrive from the **optical probe**, not the interface-metadata walk, and every
    /// other field is `None` when they do. That is safe because core's interface upsert COALESCEs
    /// each column against its existing value, so the two walks fill disjoint columns of the same
    /// row — a property with a test, because losing it would blank every interface name once a
    /// poll cycle.
    #[serde(default)]
    pub rx_power_low_dbm: Option<f64>,
    /// Highest acceptable receive power, dBm. See [`DiscoveredInterface::rx_power_low_dbm`].
    #[serde(default)]
    pub rx_power_high_dbm: Option<f64>,
    /// Lowest acceptable transmit power, dBm. See [`DiscoveredInterface::rx_power_low_dbm`].
    #[serde(default)]
    pub tx_power_low_dbm: Option<f64>,
    /// Highest acceptable transmit power, dBm. See [`DiscoveredInterface::rx_power_low_dbm`].
    #[serde(default)]
    pub tx_power_high_dbm: Option<f64>,
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
    /// Whether to try SNMP on a target that did not answer ICMP (ADR-068 Increment 3).
    ///
    /// ⚠️ **The default is `true` — today's behaviour — and it is a decision about the wire, not
    /// about what an operator wants.** This field is absent only from a job published by an N-1
    /// core, and that core meant "probe everything"; defaulting to `false` here would silently
    /// change what an older core's sweeps do at the moment a poller is upgraded. The
    /// operator-facing default is the opposite one and lives at the API edge (`StartScan`), where
    /// "unspecified" means a fresh request that should take the fast path.
    #[serde(default = "default_true")]
    pub snmp_when_unreachable: bool,
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
    pub scan_id: Uuid,
    /// All devices found so far (cumulative).
    pub found: Vec<DiscoveredDevice>,
    /// Targets probed so far — addresses this sweep has put a probe at, which is not the same as
    /// addresses it finished identifying. A target cut short mid-identification still counts; one
    /// the sweep never reached does not. Defaults to 0 for an older poller's single final message.
    #[serde(default)]
    pub probed: u32,
    /// Total targets in the sweep.
    #[serde(default)]
    pub total: u32,
    /// Whether the sweep has finished. Defaults to **true** so an older poller's single
    /// result message still completes the scan (ADR-017).
    #[serde(default = "default_true")]
    pub done: bool,
    /// Whether this sweep stopped because it was cancelled rather than because it finished
    /// (ADR-068 Increment 2). Only meaningful together with `done`.
    ///
    /// **The N-1 default is right by argument, not by luck.** A poller that predates cancellation
    /// never sends this, so it decodes as `false` — and that is exactly true of such a poller: it
    /// never received the stop, so it really did run to completion. Core therefore reads a
    /// `done` without `cancelled` as "the sweep finished before the stop could take effect", which
    /// is the honest report rather than a guess.
    ///
    /// Core could instead infer cancellation from `probed < total`, and deliberately does not: a
    /// stop landing during the final chunk produces a full count, so the inference would report a
    /// cancelled sweep as completed. One bool is cheaper than a wrong answer.
    #[serde(default)]
    pub cancelled: bool,
}

const fn default_true() -> bool {
    true
}

/// Core → pollers: stop sweeping `scan_id` (ADR-068 Increment 2).
///
/// Carries nothing but the id on purpose. It is broadcast to every poller (see
/// [`crate::subjects::discovery_cancel`]) because core does not know which one took the job, so the
/// id is both the address and the whole instruction. Adding a `poller_id` would suggest a targeting
/// this design does not have.
///
/// Publishing is **core-only**: the poller allow-lists grant subscribe and not publish, or one
/// site's poller could stop another site's sweep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryCancel {
    pub scan_id: Uuid,
}

// ── Passive events (Phase 2) — edge-received syslog / SNMP traps / webhooks ─────────

/// What kind of passive event a poller (or core, for webhooks) received.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
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
    /// Every kind. The enumeration for anything that must present all three — a filter's accepted
    /// values, a per-kind tally — so a fourth source cannot be added to some of them and not others.
    pub const ALL: [EventKind; 3] = [Self::Syslog, Self::Trap, Self::Webhook];

    /// Stable string form (matches the serde tag and the `events.kind` DB column).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Syslog => "syslog",
            Self::Trap => "trap",
            Self::Webhook => "webhook",
        }
    }

    /// The inverse of [`Self::as_str`]: an exact token, or `None`.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// A passive event received at the edge, published on `yagra.events` for core to match
/// against event rules. The `message` field is the single normalized text rules bite on
/// (for traps it is rendered as `"<trap_oid> oid=value; …"`); the structured fields are
/// preserved for display. All detail fields are `#[serde(default)]` so the message stays
/// N/N-1 tolerant (ADR-017).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventMsg {
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
    /// The **original datagram**, base64-encoded (ADR-034 forwarding). Every other field on this
    /// message is lossy — the text is lossy-UTF-8 and clipped to 4096 chars, and only the first 32
    /// varbinds survive — so byte-exact forwarding to an external collector needs the bytes
    /// themselves. Pollers attach this for syslog/trap; `None` for webhooks and from an N-1 poller
    /// that predates forwarding (core then falls back to re-rendering from the parsed fields).
    ///
    /// Use [`encode_raw`] / [`EventMsg::raw_bytes`] rather than encoding at the call site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    /// Source port of the datagram. Kept alongside `source_ip` because a forwarding target may want
    /// the full peer address; `None` for webhooks and from an N-1 poller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src_port: Option<u16>,
}

/// Encode a raw datagram for [`EventMsg::raw`] (RFC 4648 standard base64, with padding).
#[must_use]
pub fn encode_raw(bytes: &[u8]) -> String {
    data_encoding::BASE64.encode(bytes)
}

impl EventMsg {
    /// Decode [`EventMsg::raw`] back to the original datagram. Returns `None` when the producer
    /// attached no raw payload, or when the payload is not valid base64 (a corrupt field must not
    /// take down the consumer — the caller falls back to re-rendering from the parsed fields).
    #[must_use]
    pub fn raw_bytes(&self) -> Option<Vec<u8>> {
        let raw = self.raw.as_deref()?;
        data_encoding::BASE64.decode(raw.as_bytes()).ok()
    }
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

// ── Raw flow datagrams (ADR-034 Increment 2) — verbatim relay for forwarding ────────────────

/// Which flow-export wire format a datagram carries. Also the poller listener's protocol selector
/// (`yagra_poller::flow::FlowProto` is an alias of this) so the value on the wire and the value the
/// listener was configured with can never drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawFlowProto {
    /// NetFlow v5 / v9 / IPFIX (template-based).
    Netflow,
    /// sFlow v5 (packet sampling).
    Sflow,
}

impl RawFlowProto {
    /// Stable string form (matches the serde tag; also the metric label and the `kind` filter field).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Netflow => "netflow",
            Self::Sflow => "sflow",
        }
    }
}

/// One received flow-export datagram relayed **verbatim**, published on
/// [`crate::subjects::flows_raw`] for core's forwarder (ADR-034 Increment 2).
///
/// This is deliberately *not* a second copy of [`FlowBatch`]. The batch is edge-aggregated —
/// 1-minute buckets, top-N by bytes, identical 5-tuples folded, and every field outside the
/// aggregation key (TCP flags, ToS, ifIndex, next-hop, per-flow start/end times) discarded — so a
/// downstream collector could never be given back what the exporter actually sent. Forwarding
/// promises "what Yagra received, unchanged", which only the original bytes can satisfy. Both
/// streams therefore ride the bus: the aggregate for ClickHouse (ADR-031), the datagram for the tee.
///
/// A poller publishes these continuously once a flow listener is bound — there is no capture toggle,
/// because a toggle would make forwarding fidelity a function of configuration rather than a
/// property of the system. The cost is the raw datagram volume on the bus (a NetFlow v9 export is
/// ~1400 bytes carrying ~30 records, so ~370 kbit/s at 1000 flows/s); see DEPLOYMENT.md.
///
/// Best-effort, loss-tolerant tier like the rest of flow (ADR-017): a failed publish is counted and
/// dropped, never buffered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawFlowDatagram {
    /// Poller that received the datagram.
    pub poller_id: String,
    /// Which poller pool received it, if the poller declares one.
    #[serde(default)]
    pub pool: Option<String>,
    /// Source address of the datagram — the exporting device.
    pub exporter_ip: IpAddr,
    /// Source port of the datagram.
    #[serde(default)]
    pub src_port: u16,
    /// The wire format, so core decodes with the right parser without sniffing.
    pub proto: RawFlowProto,
    /// Reception time at the edge, as Unix time in milliseconds (UTC).
    pub at_unix_ms: i64,
    /// The original datagram, base64-encoded. Encode with [`encode_raw`], decode with
    /// [`RawFlowDatagram::datagram`].
    pub bytes: String,
}

impl RawFlowDatagram {
    /// Decode [`Self::bytes`] back to the original datagram. Returns `None` when the payload is not
    /// valid base64 — a corrupt field must not take down the consumer.
    #[must_use]
    pub fn datagram(&self) -> Option<Vec<u8>> {
        data_encoding::BASE64.decode(self.bytes.as_bytes()).ok()
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
    fn unknown_and_removed_fields_are_tolerated() {
        // Both directions of ADR-017 at once: an N-1 producer still stamps the removed
        // `schema_version`, and a newer one added a field we don't know. Neither may fail.
        let json = r#"{
            "schema_version": 1,
            "job_id": "00000000-0000-0000-0000-000000000000",
            "node_id": "00000000-0000-0000-0000-000000000000",
            "target": "10.0.0.1",
            "check": { "kind": "icmp", "count": 3, "timeout_ms": 1000 },
            "interval_secs": 30,
            "future_field": "ignored"
        }"#;
        let job: PollJob = serde_json::from_str(json).unwrap();
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
            snmp_when_unreachable: false,
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
        // ADR-068 Inc.3. The one field whose default is *not* the new behaviour: an N-1 core asked
        // for a sweep that probes SNMP everywhere, and an upgraded poller must keep doing that for
        // it rather than quietly narrowing what an older deployment discovers.
        assert!(
            job.snmp_when_unreachable,
            "a job from a core that predates the ICMP gate must still probe silent addresses"
        );

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
            raw: Some(encode_raw(b"<188>link down on ge-0/0/1")),
            src_port: Some(54_321),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"kind\":\"syslog\""));
        let back: EventMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(event, back);
        // The raw payload survives the round trip byte-for-byte — that is the whole point of
        // carrying it (ADR-034): re-rendering from the parsed fields is lossy.
        assert_eq!(
            back.raw_bytes().as_deref(),
            Some(&b"<188>link down on ge-0/0/1"[..])
        );
    }

    #[test]
    fn event_msg_without_raw_decodes_to_none_and_omits_the_field() {
        let event = EventMsg {
            event_id: Uuid::nil(),
            kind: EventKind::Webhook,
            at_unix_ms: 0,
            source_ip: None,
            pool: None,
            message: "alert".into(),
            facility: None,
            syslog_severity: None,
            hostname: None,
            app_name: None,
            trap_oid: None,
            varbinds: Vec::new(),
            truncated: false,
            raw: None,
            src_port: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        // `skip_serializing_if` keeps the common (webhook / no-capture) case off the wire entirely.
        assert!(!json.contains("\"raw\""), "{json}");
        assert!(!json.contains("\"src_port\""), "{json}");
        assert_eq!(event.raw_bytes(), None);
    }

    #[test]
    fn corrupt_raw_payload_decodes_to_none_instead_of_panicking() {
        let mut event = EventMsg {
            event_id: Uuid::nil(),
            kind: EventKind::Syslog,
            at_unix_ms: 0,
            source_ip: None,
            pool: None,
            message: "x".into(),
            facility: None,
            syslog_severity: None,
            hostname: None,
            app_name: None,
            trap_oid: None,
            varbinds: Vec::new(),
            truncated: false,
            raw: Some("!!!not base64!!!".into()),
            src_port: None,
        };
        assert_eq!(event.raw_bytes(), None);
        event.raw = Some(encode_raw(&[0xff, 0x00, 0xfe]));
        assert_eq!(event.raw_bytes(), Some(vec![0xff, 0x00, 0xfe]));
    }

    #[test]
    fn minimal_event_msg_defaults_optional_fields() {
        // N/N-1 (ADR-017): a producer that sends only the required fields (or an older
        // schema without the detail fields) still deserializes; extras are ignored.
        let json = r#"{
            "schema_version": 1,
            "event_id": "00000000-0000-0000-0000-000000000000",
            "kind": "trap",
            "at_unix_ms": 0,
            "message": "1.3.6.1.6.3.1.1.5.3",
            "future_field": "ignored"
        }"#;
        let event: EventMsg = serde_json::from_str(json).unwrap();
        assert_eq!(event.kind, EventKind::Trap);
        assert!(event.source_ip.is_none());
        assert!(event.varbinds.is_empty());
        assert!(!event.truncated);
        // N-1 poller (pre-ADR-034): no raw payload — core falls back to re-rendering.
        assert!(event.raw.is_none());
        assert!(event.src_port.is_none());
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
    fn minimal_flow_batch_defaults_dropped() {
        let json = r#"{
            "schema_version": 1,
            "poller_id": "edge-1",
            "pool": "default",
            "exporter_ip": "192.168.1.1",
            "bucket_start_ms": 0,
            "bucket_secs": 60,
            "records": []
        }"#;
        let batch: FlowBatch = serde_json::from_str(json).unwrap();
        assert_eq!(batch.dropped, 0); // defaulted
        assert!(batch.records.is_empty());
    }

    #[test]
    fn raw_flow_datagram_round_trips_and_decodes_to_the_original_bytes() {
        // A NetFlow v9 header's leading bytes include 0x00 — the payload must survive as *bytes*,
        // which is the whole reason it is base64 rather than a string field.
        let original = [0x00u8, 0x09, 0x00, 0x01, 0xff, 0xfe, 0x00, 0x7f];
        let dg = RawFlowDatagram {
            poller_id: "edge-1".into(),
            pool: Some("tokyo".into()),
            exporter_ip: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
            src_port: 51_234,
            proto: RawFlowProto::Netflow,
            at_unix_ms: 1_700_000_000_000,
            bytes: encode_raw(&original),
        };
        let json = serde_json::to_string(&dg).unwrap();
        assert!(json.contains("\"proto\":\"netflow\""), "{json}");
        let back: RawFlowDatagram = serde_json::from_str(&json).unwrap();
        assert_eq!(dg, back);
        assert_eq!(back.datagram().unwrap(), original.to_vec());
    }

    #[test]
    fn minimal_raw_flow_datagram_defaults_pool_and_port() {
        let json = r#"{
            "schema_version": 1,
            "poller_id": "edge-1",
            "exporter_ip": "2001:db8::1",
            "proto": "sflow",
            "at_unix_ms": 0,
            "bytes": "",
            "future_field": "ignored"
        }"#;
        let dg: RawFlowDatagram = serde_json::from_str(json).unwrap();
        assert_eq!(dg.pool, None);
        assert_eq!(dg.src_port, 0);
        assert!(dg.exporter_ip.is_ipv6()); // never assume v4
        assert_eq!(dg.datagram().unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn corrupt_raw_flow_payload_decodes_to_none_instead_of_panicking() {
        let dg = RawFlowDatagram {
            poller_id: "edge-1".into(),
            pool: None,
            exporter_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            src_port: 0,
            proto: RawFlowProto::Netflow,
            at_unix_ms: 0,
            bytes: "not base64!!".into(),
        };
        assert!(dg.datagram().is_none());
    }

    #[test]
    fn raw_flow_proto_str_matches_serde_tag() {
        for proto in [RawFlowProto::Netflow, RawFlowProto::Sflow] {
            let tag = serde_json::to_string(&proto).unwrap();
            assert_eq!(tag, format!("\"{}\"", proto.as_str()));
        }
    }

    #[test]
    fn sample_builds_thin_label_series_key() {
        let node = NodeId::from(Uuid::nil());
        let s = Sample::gauge("icmp_rtt_ms", 12.5);
        assert_eq!(s.series_key(node).ifindex, None);

        let iface = Sample {
            metric: "if_in_octets".into(),
            ifindex: Some(IfIndex(3)),
            value: 1000.0,
            kind: MetricKind::Counter,
        };
        let key = iface.series_key(node);
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
    fn unknown_check_spec_tag_in_snapshot_chunk_drops_only_that_spec() {
        // The N-1 guarantee that makes adding a CheckSpec variant safe on the working-set path.
        // A SyncMsg is decoded whole, so an unknown `check.kind` anywhere inside a chunk used to
        // fail the entire chunk → seq gap → resync → same chunk → infinite loop, stalling ALL of
        // that poller's polling. It must now cost exactly the one spec nobody understands.
        let json = r#"{
            "type": "snapshot_chunk",
            "poller_id": "edge-1",
            "epoch": "00000000-0000-0000-0000-000000000000",
            "seq": 7,
            "chunk_index": 0,
            "chunk_total": 1,
            "nodes": [{
                "node_id": "00000000-0000-0000-0000-000000000000",
                "specs": [
                    {"node_id":"00000000-0000-0000-0000-000000000000","target":"192.0.2.1",
                     "check":{"kind":"icmp","count":3,"timeout_ms":1000},"interval_secs":60},
                    {"node_id":"00000000-0000-0000-0000-000000000000","target":"192.0.2.1",
                     "check":{"kind":"quantum_tunnel","frobnicate":true},"interval_secs":60}
                ]
            }]
        }"#;

        let msg: SyncMsg = serde_json::from_str(json).expect("chunk must still decode");
        let SyncMsg::SnapshotChunk(snap) = msg else {
            panic!("expected a snapshot chunk");
        };
        assert_eq!(snap.seq, 7, "the chunk's sequence must survive intact");
        assert_eq!(snap.nodes.len(), 1);
        assert_eq!(
            snap.nodes[0].specs.len(),
            1,
            "the known icmp spec is kept and only the unknown one is dropped"
        );
        assert!(matches!(snap.nodes[0].specs[0].check, CheckSpec::Icmp(_)));
    }

    #[test]
    fn unknown_check_spec_tag_in_delta_upsert_drops_only_that_spec() {
        // Deltas carry NodeJobs too, so the same tolerance must hold on the incremental path —
        // otherwise a delta gap forces the resync this whole mechanism exists to avoid.
        let json = r#"{
            "type": "delta",
            "poller_id": "edge-1",
            "epoch": "00000000-0000-0000-0000-000000000000",
            "seq": 8,
            "upserts": [{
                "node_id": "00000000-0000-0000-0000-000000000000",
                "specs": [
                    {"node_id":"00000000-0000-0000-0000-000000000000","target":"192.0.2.1",
                     "check":{"kind":"from_the_future"},"interval_secs":60}
                ]
            }]
        }"#;

        let msg: SyncMsg = serde_json::from_str(json).expect("delta must still decode");
        let SyncMsg::Delta(delta) = msg else {
            panic!("expected a delta");
        };
        assert_eq!(delta.seq, 8);
        assert_eq!(delta.upserts.len(), 1);
        assert!(
            delta.upserts[0].specs.is_empty(),
            "the only spec was undecodable, so the node ends up with no work — not an error"
        );
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
            job_id: Uuid::nil(),
            node_id: NodeId::from(Uuid::nil()),
            at_unix_ms: 42,
            outcome: CheckOutcome::Reachable,
            samples: vec![Sample::gauge("icmp_rtt_ms", 5.0)],
            interfaces: Vec::new(),
            sys_descr: None,
            dns_chain: None,
            neighbors: None,
            l3: None,
            arp: None,
            routing: None,
            observational: false,
            poller_id: Some("edge-poller-1".into()),
            trace_context: TraceContext::new(),
        };
        let json = serde_json::to_string(&result).unwrap();
        let back: PollResult = serde_json::from_str(&json).unwrap();
        assert_eq!(result, back);
        assert_eq!(back.poller_id.as_deref(), Some("edge-poller-1"));
    }

    /// ADR-038's two new result fields, both N-1 sensitive in the same way `dns_chain` was.
    #[test]
    fn poll_result_neighbor_fields_tolerate_missing_and_unknown_fields() {
        // An N-1 poller sends neither field. `observational` must default to *false*, or every
        // pre-upgrade result would stop driving alerts.
        let json = r#"{
            "job_id": "00000000-0000-0000-0000-000000000000",
            "node_id": "00000000-0000-0000-0000-000000000000",
            "at_unix_ms": 0,
            "outcome": "reachable",
            "some_future_field": 42
        }"#;
        let result: PollResult = serde_json::from_str(json).unwrap();
        assert!(result.neighbors.is_none());
        assert!(
            !result.observational,
            "an older poller's results must keep driving liveness"
        );

        // And an ordinary result's wire form is byte-identical to what it was before the fields
        // existed: both are skipped when unset.
        let wire = serde_json::to_string(&result).unwrap();
        assert!(!wire.contains("neighbors"), "{wire}");
        assert!(!wire.contains("observational"), "{wire}");
    }

    /// `None` (no set observed) and `Some(empty)` (this device has no neighbours) must survive the
    /// wire as different values — core writes nothing for the first and replaces the stored set for
    /// the second, so collapsing them would make one failed walk erase a node's whole adjacency.
    #[test]
    fn an_empty_neighbor_set_is_distinct_from_no_set_on_the_wire() {
        let mut result: PollResult = serde_json::from_str(
            r#"{"job_id":"00000000-0000-0000-0000-000000000000",
                "node_id":"00000000-0000-0000-0000-000000000000",
                "at_unix_ms":0,"outcome":"reachable"}"#,
        )
        .unwrap();
        result.neighbors = Some(yagra_common::NeighborSet::default());
        result.observational = true;
        let wire = serde_json::to_string(&result).unwrap();
        let back: PollResult = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.neighbors, Some(yagra_common::NeighborSet::default()));
        assert!(back.observational);
    }

    /// ADR-043's result field, N-1 sensitive in exactly the way `neighbors` was.
    #[test]
    fn an_l3_result_field_tolerates_missing_and_unknown_fields() {
        // An N-1 poller sends no `l3` at all.
        let json = r#"{
            "job_id": "00000000-0000-0000-0000-000000000000",
            "node_id": "00000000-0000-0000-0000-000000000000",
            "at_unix_ms": 0,
            "outcome": "reachable",
            "some_future_field": 42
        }"#;
        let result: PollResult = serde_json::from_str(json).unwrap();
        assert!(result.l3.is_none());

        // A result that carries no L3 snapshot is byte-identical to what it was before the field
        // existed — the whole point of `skip_serializing_if` here.
        let wire = serde_json::to_string(&result).unwrap();
        assert!(!wire.contains("l3"), "{wire}");
    }

    /// `None` (no snapshot observed) and `Some(empty)` (this device reports no addresses) must
    /// survive the wire as different values, for the same reason the neighbour set must: core
    /// writes nothing for the first and *replaces* the stored snapshot for the second. Collapse
    /// them and one failed walk erases a node's addressing — and, one derivation later, every link
    /// that node was part of.
    #[test]
    fn an_empty_l3_snapshot_is_distinct_from_no_snapshot_on_the_wire() {
        let mut result: PollResult = serde_json::from_str(
            r#"{"job_id":"00000000-0000-0000-0000-000000000000",
                "node_id":"00000000-0000-0000-0000-000000000000",
                "at_unix_ms":0,"outcome":"reachable"}"#,
        )
        .unwrap();
        result.l3 = Some(yagra_common::L3Snapshot::default());
        result.observational = true;
        let wire = serde_json::to_string(&result).unwrap();
        let back: PollResult = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.l3, Some(yagra_common::L3Snapshot::default()));
        assert_ne!(
            back.l3, None,
            "an empty observation is still an observation"
        );
        assert!(back.observational);
    }

    /// An L3 check from an N-1 core (or one that gains a field later) still decodes.
    #[test]
    fn an_l3_check_tolerates_missing_and_unknown_fields() {
        let v2c: SnmpL3Check =
            serde_json::from_str(r#"{"community":"public","future":1}"#).unwrap();
        assert!(v2c.columns.is_empty());
        assert_eq!(v2c.timeout_ms, default_snmp_timeout_ms());
        let v3: SnmpV3L3Check =
            serde_json::from_str(r#"{"user":"monitor","security_level":"authpriv"}"#).unwrap();
        assert!(v3.auth_key.is_none() && v3.columns.is_empty());

        // The tags are what an N-1 poller skips on, so they must be the expected snake_case.
        let spec = CheckSpec::SnmpL3(v2c);
        let wire = serde_json::to_string(&spec).unwrap();
        assert!(wire.contains(r#""kind":"snmp_l3""#), "{wire}");
        let spec3 = CheckSpec::SnmpV3L3(v3);
        let wire3 = serde_json::to_string(&spec3).unwrap();
        assert!(wire3.contains(r#""kind":"snmp_v3_l3""#), "{wire3}");
    }

    /// ADR-062's optical check, N-1 sensitive in the same way every other check spec is.
    #[test]
    fn an_optical_check_tolerates_missing_and_unknown_fields() {
        let v2c: SnmpOpticalCheck =
            serde_json::from_str(r#"{"community":"public","future":1}"#).unwrap();
        assert!(v2c.probes.is_empty());
        assert_eq!(v2c.timeout_ms, default_snmp_timeout_ms());
        let v3: SnmpV3OpticalCheck =
            serde_json::from_str(r#"{"user":"monitor","security_level":"authpriv"}"#).unwrap();
        assert!(v3.auth_key.is_none() && v3.probes.is_empty());

        // A probe may name only one of the two readings — that is how an operator disabling one
        // half of the built-in template reaches the poller.
        let probe: OpticalProbe =
            serde_json::from_str(r#"{"flavor":"huawei","rx_metric":"if_rx_power_dbm"}"#).unwrap();
        assert_eq!(probe.flavor, yagra_common::OpticalFlavor::Huawei);
        assert!(probe.tx_metric.is_none());

        // The tags are what an N-1 poller skips on, so they must be the expected snake_case.
        let spec = CheckSpec::SnmpOptical(v2c);
        let wire = serde_json::to_string(&spec).unwrap();
        assert!(wire.contains(r#""kind":"snmp_optical""#), "{wire}");
        let spec3 = CheckSpec::SnmpV3Optical(v3);
        let wire3 = serde_json::to_string(&spec3).unwrap();
        assert!(wire3.contains(r#""kind":"snmp_v3_optical""#), "{wire3}");
    }

    /// `DiscoveredInterface` decodes both N-1 directions (ADR-063 Inc.1).
    ///
    /// The producer half is the one that matters here: an **older poller** publishes a record with
    /// no `if_duplex` / `if_type` at all, and a new core must read it as "not known" rather than
    /// failing — because a failure would take the whole `PollResult` with it. The `schema_version`
    /// key is sent on purpose; it is what an N-1 producer actually puts on the wire (the field was
    /// removed from the structs but old binaries still write it), and ignoring it is the property.
    #[test]
    fn a_discovery_result_tolerates_missing_and_unknown_fields() {
        // The N-1 producer: a poller that predates ADR-068 Inc.2 sends no `cancelled`. The default
        // must be `false`, and that is not merely a safe default — it is *true* of such a poller.
        // It never received the stop, so it really did run to completion, and core reading
        // "finished, not cancelled" reports what happened rather than guessing.
        let old: DiscoveryResult =
            serde_json::from_str(r#"{"scan_id":"00000000-0000-0000-0000-000000000001","found":[],"schema_version":1,"future_field":"x"}"#)
                .unwrap();
        assert!(old.done, "an old poller's single message still completes");
        assert!(
            !old.cancelled,
            "absent `cancelled` must read as 'it finished', which is what an old poller did"
        );
        assert_eq!((old.probed, old.total), (0, 0));

        let stopped = DiscoveryResult {
            scan_id: Uuid::from_u128(1),
            found: Vec::new(),
            probed: 96,
            total: 254,
            done: true,
            cancelled: true,
        };
        let wire = serde_json::to_string(&stopped).unwrap();
        assert!(wire.contains(r#""cancelled":true"#), "{wire}");
        assert_eq!(
            serde_json::from_str::<DiscoveryResult>(&wire).unwrap(),
            stopped
        );
    }

    #[test]
    fn a_discovery_cancel_tolerates_unknown_fields() {
        // A newer core may add fields; an older poller must still act on the id rather than drop
        // the message — dropping it would leave the sweep running with the UI saying "stopping…".
        let c: DiscoveryCancel = serde_json::from_str(
            r#"{"scan_id":"00000000-0000-0000-0000-000000000009","reason":"operator","schema_version":1}"#,
        )
        .unwrap();
        assert_eq!(c.scan_id, Uuid::from_u128(9));
    }

    #[test]
    fn a_discovered_interface_tolerates_missing_and_unknown_fields() {
        let old: DiscoveredInterface =
            serde_json::from_str(r#"{"ifindex":7,"schema_version":1,"future_field":"x"}"#).unwrap();
        assert_eq!(old.ifindex, IfIndex(7));
        assert!(
            old.if_duplex.is_none(),
            "absent duplex must read as unknown"
        );
        assert!(old.if_type.is_none(), "absent ifType must read as unknown");
        assert!(old.if_name.is_none() && old.if_speed.is_none());

        // And the populated case round-trips under the tokens the DB column also uses, so a
        // rename here would be caught rather than silently writing rows core cannot parse back.
        let full = DiscoveredInterface {
            ifindex: IfIndex(7),
            if_name: Some("GE0/0/1".to_owned()),
            if_alias: Some("Internet".to_owned()),
            if_speed: Some(100_000_000),
            if_duplex: Some(Duplex::Full),
            if_type: Some(yagra_common::IF_TYPE_ETHERNET_CSMACD),
            if_media: Some("1000BASE-T".to_owned()),
            transceiver_model: None,
            rx_power_low_dbm: None,
            rx_power_high_dbm: None,
            tx_power_low_dbm: None,
            tx_power_high_dbm: None,
        };
        let wire = serde_json::to_string(&full).unwrap();
        assert!(wire.contains(r#""if_duplex":"full""#), "{wire}");
        assert_eq!(
            serde_json::from_str::<DiscoveredInterface>(&wire).unwrap(),
            full
        );
    }

    /// Every optical dialect survives the wire under a stable token.
    ///
    /// The token is the N/N-1 contract: a core that learns a new dialect sends a tag an older
    /// poller cannot parse, and `de_lenient_specs` then drops **that one spec** rather than the
    /// chunk. Renaming an existing one would instead make every current poller silently stop
    /// collecting optics, with no error anywhere.
    #[test]
    fn every_optical_flavor_round_trips_under_a_stable_token() {
        for flavor in yagra_common::OpticalFlavor::ALL {
            let probe = OpticalProbe {
                flavor,
                rx_metric: Some("if_rx_power_dbm".to_owned()),
                tx_metric: None,
                temp_metric: Some("cisco_temp_c".to_owned()),
            };
            let wire = serde_json::to_string(&probe).unwrap();
            let back: OpticalProbe = serde_json::from_str(&wire).unwrap();
            assert_eq!(back, probe, "{flavor:?} did not round trip");
        }
        // Pinned literally: these strings are the wire, not an implementation detail.
        let wire = serde_json::to_string(&OpticalProbe {
            flavor: yagra_common::OpticalFlavor::EntitySensor,
            rx_metric: None,
            tx_metric: None,
            temp_metric: None,
        })
        .unwrap();
        assert!(wire.contains(r#""flavor":"entity_sensor""#), "{wire}");
        // ADR-070's dialect, pinned the same way. The standard one above must keep its token or
        // every deployed poller silently stops collecting optics; this one must keep its token for
        // the same reason from the day it ships.
        let wire = serde_json::to_string(&OpticalProbe {
            flavor: yagra_common::OpticalFlavor::CiscoEntitySensor,
            rx_metric: None,
            tx_metric: None,
            temp_metric: None,
        })
        .unwrap();
        assert!(wire.contains(r#""flavor":"cisco_entity_sensor""#), "{wire}");
    }

    /// An N-1 core sends no `temp_metric`; a probe without it must still decode (ADR-070).
    ///
    /// The mirror case — a new core sending it to an old poller — is covered by serde ignoring
    /// unknown fields, which every message here relies on and no lint enforces.
    #[test]
    fn an_optical_probe_tolerates_a_missing_temp_metric() {
        let json = r#"{"flavor":"entity_sensor","rx_metric":"if_rx_power_dbm"}"#;
        let probe: OpticalProbe = serde_json::from_str(json).expect("N-1 probe decodes");
        assert_eq!(probe.rx_metric.as_deref(), Some("if_rx_power_dbm"));
        assert_eq!(probe.tx_metric, None);
        assert_eq!(probe.temp_metric, None);
        // And a field this binary has never heard of does not fail the decode either.
        let json = r#"{"flavor":"entity_sensor","some_future_reading":"x"}"#;
        let probe: OpticalProbe = serde_json::from_str(json).expect("unknown field tolerated");
        assert_eq!(probe.temp_metric, None);
    }

    /// ADR-043 Increment 3's result field, N-1 sensitive in exactly the way `l3` was.
    #[test]
    fn an_arp_result_field_tolerates_missing_and_unknown_fields() {
        let json = r#"{
            "job_id": "00000000-0000-0000-0000-000000000000",
            "node_id": "00000000-0000-0000-0000-000000000000",
            "at_unix_ms": 0,
            "outcome": "reachable",
            "some_future_field": 42
        }"#;
        let result: PollResult = serde_json::from_str(json).unwrap();
        assert!(result.arp.is_none());

        // A result that carries no ARP summary is byte-identical to what it was before the field
        // existed. Note the needle: `"arp"` with quotes, because `at_unix_ms` and `trace_context`
        // both contain the bare letters.
        let wire = serde_json::to_string(&result).unwrap();
        assert!(!wire.contains("\"arp\""), "{wire}");

        // And `None` vs `Some(empty)` survives the wire as two different answers, for the third
        // time and the same reason: core writes nothing for the first and replaces the stored
        // summary for the second.
        let mut with = result;
        with.arp = Some(yagra_common::ArpSummary::default());
        with.observational = true;
        let back: PollResult =
            serde_json::from_str(&serde_json::to_string(&with).unwrap()).unwrap();
        assert_eq!(back.arp, Some(yagra_common::ArpSummary::default()));
        assert!(back.observational);
    }

    /// An ARP check from an N-1 core (or one that gains a field later) still decodes, and its row
    /// budget falls back to the value `yagra-common` declares rather than to zero.
    #[test]
    fn an_arp_check_tolerates_missing_and_unknown_fields() {
        let v2c: SnmpArpCheck =
            serde_json::from_str(r#"{"community":"public","future":1}"#).unwrap();
        assert!(v2c.columns.is_empty());
        assert_eq!(v2c.timeout_ms, default_snmp_timeout_ms());
        // A zero here would mean "walk nothing" and the feature would silently collect no data.
        assert_eq!(
            usize::try_from(v2c.max_rows).unwrap(),
            yagra_common::MAX_ARP_WALK_ROWS,
            "the wire default must be the constant yagra-common declares"
        );
        let v3: SnmpV3ArpCheck =
            serde_json::from_str(r#"{"user":"monitor","security_level":"authpriv"}"#).unwrap();
        assert!(v3.auth_key.is_none() && v3.columns.is_empty());
        assert_eq!(v3.max_rows, v2c.max_rows);

        // The tags are what an N-1 poller skips on, so they must be the expected snake_case.
        let wire = serde_json::to_string(&CheckSpec::SnmpArp(v2c)).unwrap();
        assert!(wire.contains(r#""kind":"snmp_arp""#), "{wire}");
        let wire3 = serde_json::to_string(&CheckSpec::SnmpV3Arp(v3)).unwrap();
        assert!(wire3.contains(r#""kind":"snmp_v3_arp""#), "{wire3}");
    }

    /// A result from an N-1 poller carries no routing snapshot, and one that does still round-trips.
    #[test]
    fn a_poll_result_without_routing_is_byte_identical_to_today() {
        let json = r#"{
            "job_id": "00000000-0000-0000-0000-000000000000",
            "node_id": "00000000-0000-0000-0000-000000000000",
            "at_unix_ms": 0,
            "outcome": "reachable",
            "some_future_field": 42
        }"#;
        let result: PollResult = serde_json::from_str(json).unwrap();
        assert!(result.routing.is_none());
        let wire = serde_json::to_string(&result).unwrap();
        assert!(!wire.contains("\"routing\""), "{wire}");

        // `None` vs `Some(empty)` survives the wire as two different answers, for the fourth time
        // and the same reason: core writes nothing for the first and replaces the stored snapshot
        // for the second.
        let mut with = result;
        with.routing = Some(yagra_common::RoutingSnapshot::default());
        with.observational = true;
        let back: PollResult =
            serde_json::from_str(&serde_json::to_string(&with).unwrap()).unwrap();
        assert_eq!(back.routing, Some(yagra_common::RoutingSnapshot::default()));
        assert!(back.observational);
    }

    /// A routing check from an N-1 core (or one that gains a field later) still decodes, and an
    /// absent probe list means "probe nothing" rather than a panic or an unbounded walk.
    #[test]
    fn a_routing_check_tolerates_missing_and_unknown_fields() {
        let v2c: SnmpRoutingCheck =
            serde_json::from_str(r#"{"community":"public","future":1}"#).unwrap();
        assert!(v2c.columns.is_empty());
        assert!(v2c.route_probes.is_empty());
        assert_eq!(v2c.timeout_ms, default_snmp_timeout_ms());

        let v3: SnmpV3RoutingCheck =
            serde_json::from_str(r#"{"user":"monitor","security_level":"authpriv"}"#).unwrap();
        assert!(v3.auth_key.is_none() && v3.columns.is_empty() && v3.route_probes.is_empty());

        // The tags are what an N-1 poller skips on, so they must be the expected snake_case.
        let wire = serde_json::to_string(&CheckSpec::SnmpRouting(v2c)).unwrap();
        assert!(wire.contains(r#""kind":"snmp_routing""#), "{wire}");
        let wire3 = serde_json::to_string(&CheckSpec::SnmpV3Routing(v3)).unwrap();
        assert!(wire3.contains(r#""kind":"snmp_v3_routing""#), "{wire3}");
    }

    /// A probe carries the destination it asks about alongside the OID that encodes it, and both
    /// survive the wire. Losing `target` would leave the poller with rows it cannot attribute.
    #[test]
    fn a_route_probe_round_trips_with_its_target() {
        let probe = SnmpRouteProbe {
            field: RoutingColumn::InetCidrRouteType,
            oid: yagra_common::route_probe_oid(
                "1.3.6.1.2.1.4.24.7.1.8",
                "133.123.189.109".parse().unwrap(),
            ),
            target: "133.123.189.109".parse().unwrap(),
        };
        let back: SnmpRouteProbe =
            serde_json::from_str(&serde_json::to_string(&probe).unwrap()).unwrap();
        assert_eq!(back, probe);
        assert!(back.oid.ends_with(".1.4.133.123.189.109"), "{}", back.oid);
    }

    /// A neighbour check from an N-1 core (or one that gains a field later) still decodes.
    #[test]
    fn neighbor_checks_tolerate_missing_and_unknown_fields() {
        let v2c: SnmpNeighborCheck =
            serde_json::from_str(r#"{"community":"public","future":1}"#).unwrap();
        assert!(v2c.columns.is_empty());
        assert_eq!(v2c.timeout_ms, default_snmp_timeout_ms());
        let v3: SnmpV3NeighborCheck =
            serde_json::from_str(r#"{"user":"monitor","security_level":"authpriv"}"#).unwrap();
        assert!(v3.auth_key.is_none() && v3.columns.is_empty());
    }

    /// The working-set decoder drops a spec it cannot understand rather than failing the chunk —
    /// which is what makes adding these variants safe in *either* upgrade order. Modelled on an
    /// N+1 core sending a variant this build has never heard of.
    #[test]
    fn an_unknown_check_variant_costs_only_its_own_spec() {
        let json = r#"{
            "node_id": "00000000-0000-0000-0000-000000000000",
            "specs": [
              {"node_id":"00000000-0000-0000-0000-000000000000","target":"10.0.0.1",
               "check":{"kind":"icmp","count":3,"timeout_ms":1000},"interval_secs":60},
              {"node_id":"00000000-0000-0000-0000-000000000000","target":"10.0.0.1",
               "check":{"kind":"telepathy","vibes":"good"},"interval_secs":3600}
            ]
        }"#;
        let jobs: NodeJobs = serde_json::from_str(json).unwrap();
        assert_eq!(jobs.specs.len(), 1);
        assert!(matches!(jobs.specs[0].check, CheckSpec::Icmp(_)));
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
    fn heartbeat_tolerates_a_missing_leaving_flag() {
        // An N-1 poller sends no `leaving`, and must read as a normal beat rather than failing to
        // decode — a heartbeat that does not parse is a poller core believes is dead.
        let json = r#"{
            "poller_id": "edge-1",
            "pool": "default",
            "incarnation": "00000000-0000-0000-0000-000000000000"
        }"#;
        let hb: HeartbeatMsg = serde_json::from_str(json).unwrap();
        assert!(!hb.leaving);

        // And the flag round-trips when an N poller does set it.
        let mut bye = hb;
        bye.leaving = true;
        let wire = serde_json::to_string(&bye).unwrap();
        let back: HeartbeatMsg = serde_json::from_str(&wire).unwrap();
        assert!(back.leaving);
    }

    #[test]
    fn poller_upgrade_tolerates_missing_and_unknown_fields() {
        // The mandatory three are what a command means; everything else defaults. `step` defaulting
        // to `Apply` is the load-bearing one: a producer that predates the prefetch split means
        // "upgrade", and defaulting to `Prefetch` would download the release and never install it —
        // a failure indistinguishable from success until the version never changed.
        let json = r#"{
            "poller_id": "edge-1",
            "run_id": "1a2b3c4d-0000-0000-0000-00000000abcd",
            "tag": "v0.2.3",
            "something_newer": {"nested": true}
        }"#;
        let msg: PollerUpgradeMsg = serde_json::from_str(json).unwrap();
        assert_eq!(msg.step, UpgradeStep::Apply);
        assert_eq!(msg.requested_by, "");
        assert_eq!(msg.requested_at, 0);

        let wire = serde_json::to_string(&PollerUpgradeMsg {
            step: UpgradeStep::Prefetch,
            ..msg
        })
        .unwrap();
        let back: PollerUpgradeMsg = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.step, UpgradeStep::Prefetch);
    }

    #[test]
    fn the_upgrade_subject_is_its_own_family_not_a_variant_of_the_assignment_stream() {
        // Subject partitioning *is* the version gate for this family (module docs, mechanism 4): an
        // older poller never subscribes here, so the command reaches nobody and the site stays put.
        // It also means `yagra.poller.assign.>` does not cover it — the trap both bus allow-lists
        // have to be edited for, with no compile error if either is missed.
        let up = crate::subjects::upgrade_for("edge-1");
        assert_eq!(up, "yagra.poller.upgrade.edge-1");
        assert!(!up.starts_with("yagra.poller.assign"));
        // Sanitized like every other per-poller subject, so an id cannot become two tokens.
        assert_eq!(
            crate::subjects::upgrade_for("a.b/c"),
            "yagra.poller.upgrade.a-b-c"
        );
    }

    #[test]
    fn a_heartbeat_tolerates_missing_and_unknown_fields() {
        // ADR-043 added `mgmt_addrs`. An N-1 poller sends none, and a beat that fails to decode is
        // a poller core believes is dead — so the absence has to read as "reported no addresses",
        // which is also what a poller that has none reports.
        let json = r#"{
            "poller_id": "edge-1",
            "pool": "default",
            "incarnation": "00000000-0000-0000-0000-000000000000",
            "future_field": "ignored"
        }"#;
        let hb: HeartbeatMsg = serde_json::from_str(json).unwrap();
        assert!(hb.mgmt_addrs.is_empty());

        // Both families survive the round trip; a v4-only assumption here would silently drop a
        // v6-addressed poller's location.
        let mut located = hb;
        located.mgmt_addrs = vec![
            "192.168.1.9".parse().unwrap(),
            "2001:db8::9".parse().unwrap(),
        ];
        let wire = serde_json::to_string(&located).unwrap();
        let back: HeartbeatMsg = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.mgmt_addrs, located.mgmt_addrs);
    }
    #[test]
    fn http_check_tolerates_missing_and_unknown_fields() {
        // N-1 producer: no `auth` at all. It must decode as an unauthenticated check rather than
        // failing — a failed decode inside a working-set chunk takes the whole chunk down.
        let json = r#"{
            "url": "https://example.test/health",
            "timeout_ms": 5000,
            "future_field": 1
        }"#;
        let check: HttpCheck = serde_json::from_str(json).unwrap();
        assert!(check.auth.is_none());
        assert!(
            check.body_match.is_none(),
            "an absent body_match must read as 'do not read the body'"
        );
        assert!(
            check.verify_tls,
            "an absent verify_tls must not read as disabled"
        );

        // N producer: the field round-trips through its tagged form.
        let with_auth = HttpCheck {
            url: "https://example.test/health".into(),
            method: HttpMethod::Get,
            expected_status: ExpectedStatus::TwoXx,
            verify_tls: true,
            follow_redirects: true,
            timeout_ms: 5000,
            auth: Some(HttpAuth::Bearer {
                token: "tok".into(),
            }),
            body_match: Some(yagra_common::BodyMatch {
                pattern: "\"status\":\"ok\"".into(),
                mode: yagra_common::BodyMatchMode::NotContains,
            }),
            json_extract: vec![yagra_common::JsonExtract {
                metric: "queue_depth".into(),
                path: "data.queue.depth".into(),
            }],
            body_max_bytes: 4096,
        };
        let wire = serde_json::to_string(&with_auth).unwrap();
        assert!(wire.contains("\"scheme\":\"bearer\""));
        assert!(wire.contains("\"mode\":\"not_contains\""));
        let back: HttpCheck = serde_json::from_str(&wire).unwrap();
        assert_eq!(back.auth, with_auth.auth);
        assert_eq!(back.body_match, with_auth.body_match);
        assert_eq!(back.json_extract, with_auth.json_extract);
        assert_eq!(back.body_max_bytes, 4096);

        // The N-1 case for both body features: a rule carrying only a pattern is the shape an older
        // core (or a hand-written job) sends, and it must land on the documented defaults rather
        // than failing the decode and taking the spec with it.
        let bare: HttpCheck = serde_json::from_str(
            r#"{"url":"https://example.test/","body_match":{"pattern":"ok"}}"#,
        )
        .unwrap();
        let rule = bare.body_match.as_ref().expect("rule decoded");
        assert_eq!(rule.mode, yagra_common::BodyMatchMode::Contains);
        assert_eq!(bare.body_max_bytes, yagra_common::DEFAULT_BODY_MAX_BYTES);
        assert!(bare.json_extract.is_empty());
    }

    #[test]
    fn the_body_is_read_only_when_a_feature_needs_it() {
        // The one place that decides whether a poll pays for a body read. A monitor with neither
        // feature must behave exactly as it did before ADR-047 Inc.2 — that is what keeps
        // `http_response_time_ms` measured at the response headers for every monitor.
        let plain = HttpCheck {
            url: "https://example.test/".into(),
            method: HttpMethod::Get,
            expected_status: ExpectedStatus::TwoXx,
            verify_tls: true,
            follow_redirects: true,
            timeout_ms: 5000,
            auth: None,
            body_match: None,
            json_extract: Vec::new(),
            body_max_bytes: 4096,
        };
        assert_eq!(plain.body_capture_bytes(), None);

        let matching = HttpCheck {
            body_match: Some(yagra_common::BodyMatch::contains("ok")),
            ..plain.clone()
        };
        assert_eq!(matching.body_capture_bytes(), Some(4096));

        // Extraction alone must also open the body — it was the second feature to need it, and the
        // budget moved off `BodyMatch` precisely so this case has an answer.
        let extracting = HttpCheck {
            json_extract: vec![yagra_common::JsonExtract {
                metric: "queue_depth".into(),
                path: "queue.depth".into(),
            }],
            ..plain
        };
        assert_eq!(extracting.body_capture_bytes(), Some(4096));
    }

    #[test]
    fn a_poll_job_debug_never_prints_url_credentials() {
        // PollJob, CheckSpec and HttpCheck all derive Debug, so this is one `debug!(?job)` away
        // from being a credential leak if HttpAuth's manual impl is ever replaced by a derive.
        let job = PollJob::http(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            "10.0.0.1".parse().unwrap(),
            HttpCheck {
                url: "https://example.test/health".into(),
                method: HttpMethod::Get,
                expected_status: ExpectedStatus::TwoXx,
                verify_tls: true,
                follow_redirects: true,
                timeout_ms: 5000,
                auth: Some(HttpAuth::Basic {
                    username: "probe".into(),
                    password: "hunter2".into(),
                }),
                body_match: None,
                json_extract: Vec::new(),
                body_max_bytes: yagra_common::DEFAULT_BODY_MAX_BYTES,
            },
            60,
        );
        assert!(!format!("{job:?}").contains("hunter2"));
    }

    #[test]
    fn snapshot_chunk_round_trips_and_carries_snake_case_tag() {
        let msg = SyncMsg::SnapshotChunk(WorkingSetSnapshot {
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
        // Old producer: the removed `schema_version`, no `total_nodes`; new producer: an
        // extra field (ADR-017).
        let json = r#"{
            "schema_version": 1,
            "poller_id": "edge-1",
            "epoch": "00000000-0000-0000-0000-000000000000",
            "seq": 3,
            "chunk_index": 1,
            "chunk_total": 4,
            "nodes": [],
            "future_field": 99
        }"#;
        let snap: WorkingSetSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(snap.total_nodes, 0); // defaulted
        assert_eq!(snap.chunk_total, 4);
    }

    #[test]
    fn working_set_delta_tolerates_missing_and_unknown_fields() {
        // A remove-only delta omits `upserts`; an upsert-only delta omits `removes`; both, plus
        // the removed `schema_version` and an unknown field, must deserialize (ADR-017).
        let json = r#"{
            "schema_version": 1,
            "poller_id": "edge-1",
            "epoch": "00000000-0000-0000-0000-000000000000",
            "seq": 9,
            "removes": ["00000000-0000-0000-0000-000000000000"],
            "future_field": true
        }"#;
        let delta: WorkingSetDelta = serde_json::from_str(json).unwrap();
        assert!(delta.upserts.is_empty()); // defaulted
        assert_eq!(delta.removes.len(), 1);
    }

    #[test]
    fn heartbeat_round_trips_and_tolerates_minimal_form() {
        let hb = HeartbeatMsg {
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
            caps: vec![CAP_RAW_CAPTURE.to_owned()],
            leaving: false,
            mgmt_addrs: Vec::new(),
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
            "schema_version": 1,
            "poller_id": "edge-1",
            "pool": "default",
            "incarnation": "00000000-0000-0000-0000-000000000000",
            "future_field": "ignored"
        }"#;
        let hb: HeartbeatMsg = serde_json::from_str(minimal).unwrap();
        assert!(hb.version.is_empty());
        assert!(hb.epoch.is_none());
        assert_eq!(hb.last_seq, 0);
        assert!(hb.listeners.is_empty());
        assert!(hb.host.is_none()); // N-1 poller: no host telemetry
        assert!(hb.caps.is_empty()); // N-1 poller: advertises no capabilities (ADR-034)
    }

    #[test]
    fn sync_request_round_trips_and_tolerates_legacy_fields() {
        let req = SyncRequest {
            poller_id: "edge-1".into(),
            pool: "tokyo".into(),
            incarnation: Uuid::nil(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: SyncRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);

        let old = r#"{
            "schema_version": 1,
            "poller_id": "edge-1",
            "pool": "default",
            "incarnation": "00000000-0000-0000-0000-000000000000",
            "future_field": 1
        }"#;
        let old: SyncRequest = serde_json::from_str(old).unwrap();
        assert_eq!(old.poller_id, "edge-1");
        assert_eq!(old.pool, "default");
        assert_eq!(old.incarnation, Uuid::nil());
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
