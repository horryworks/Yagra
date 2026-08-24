// SPDX-License-Identifier: AGPL-3.0-only
//! The poller work loop, split by **how a check talks to the device** (ADR-099).
//!
//! A poller consumes [`PollJob`]s from the bus, executes them via the [`Transport`] abstraction
//! (never a raw protocol), and publishes [`PollResult`]s back. It holds no state beyond the
//! in-flight job — that statelessness is what lets pollers scale out and fail over (ADR-003/009).
//! Counters are reported raw; rates are derived later (ADR-012).
//!
//! This file holds two things and nothing else: [`execute`], which decides **which** conversation a
//! job wants, and the vocabulary every conversation shares ([`result`] and the stamping helpers).
//! The conversations themselves are one file each:
//!
//! | file | how it talks to the device |
//! |---|---|
//! | [`stream`] | the loop that feeds all of them — the only one that knows the bus and the limiter |
//! | [`probes`] | **one round trip and the answer is there** — ICMP, HTTP, DNS |
//! | [`meraki`] | **one job, many results** — an org's Dashboard API, paged |
//! | [`snmp`] | the v2c/v3 walker, and the scalar GET that rides it |
//! | [`interfaces`] | a table walk keyed by ifIndex, plus the metadata fold |
//! | [`physical`] | the physical layer — optical power and media, via ENTITY-MIB when needed |
//! | [`adjacency`] | observational neighbour / address / ARP / routing walks |
//!
//! 🚨 **Every arm of [`execute`] delegates; none of them touches the transport itself.** That is
//! what makes the table above true rather than aspirational, and `guards.rs` fails the build if an
//! arm reaches for the device inline. It is not style: the HTTP arm had grown to 101 lines and the
//! DNS arm to 53 — together 47% of the dispatch — while owning 19 of this module's tests and having
//! no function name for anyone to find them by.
//!
//! ⚠️ **The interpretation of what a walk returns is not here and must not move here.**
//! [`crate::optical`], [`crate::mau`], [`crate::neighbors`], [`crate::arp`], [`crate::l3`] and
//! [`crate::routing`] are pure — already-walked rows in, a normalized answer out — and each says so
//! in its own doc. They are pure because that is where a mistake is silent, and pulling a session
//! into one of them would take away the only property they claim.

mod adjacency;
mod interfaces;
mod meraki;
mod physical;
mod probes;
mod snmp;
mod stream;
#[cfg(test)]
mod testkit;

// Re-exported so a sibling's `use super::*` sees them: a private `use` here is visible to every
// descendant, which is what keeps each conversation file free of its own import block.
use adjacency::{execute_arp, execute_l3, execute_neighbors, execute_routing};
use interfaces::{execute_snmp_table, execute_snmp_v3_table};
use meraki::execute_meraki;
use physical::{execute_mau, execute_optical};
use probes::{execute_dns, execute_http, execute_icmp};
use snmp::{execute_scalar_get, SnmpWalker};
pub(crate) use stream::run_stream;

use crate::limiter::PollLimiter;
use crate::optical;
use crate::store_forward::StoreForwardSink;
use futures::stream::{Stream, StreamExt};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::Instrument as _;
use yagra_bus::{
    CheckOutcome, CheckSpec, DiscoveredInterface, DnsCheck, HttpCheck, IcmpCheck, PollJob,
    PollResult, Sample, SnmpColumn, SnmpMetaColumn, SnmpTableCheck, SnmpV3TableCheck,
};
use yagra_common::{
    copper_designation, duplex_from_dot3, duplex_from_huawei, if_type_from_snmp,
    medium_from_huawei, DnsFailure, IfIndex, InterfaceField, Medium, MetricKind, NodeId,
    METRIC_DNS_ANSWER_COUNT, METRIC_DNS_CHAIN_LENGTH, METRIC_DNS_RESOLVE_MS, METRIC_DNS_UP,
    METRIC_HTTP_BODY_MATCH, METRIC_HTTP_BODY_TRUNCATED, METRIC_HTTP_RESPONSE_TIME_MS,
    METRIC_HTTP_STATUS_CODE, METRIC_HTTP_UP, METRIC_ICMP_RTT_MS, METRIC_SNMP_UP,
    METRIC_SSL_CERT_DAYS_TO_EXPIRY, OID_DOT3_DUPLEX_STATUS, OID_HW_ETHERNET_DUPLEX,
    OID_HW_ETHERNET_PORT_TYPE, OID_IF_HIGH_SPEED, OID_IF_TYPE,
};
use yagra_transport::{
    DnsProbeSpec, HttpProbeSpec, MerakiCollectSpec, SnmpTableSample, SnmpTableString, SnmpV3Params,
    Transport, TransportError,
};

/// Execute one job and build its result. Pure given the transport and timestamp, so it
/// is unit-testable without a clock or a bus.
pub async fn execute(job: &PollJob, transport: &dyn Transport, at_unix_ms: i64) -> PollResult {
    match &job.check {
        CheckSpec::Icmp(icmp) => execute_icmp(job, transport, at_unix_ms, icmp).await,
        CheckSpec::Snmp(snmp) => {
            let timeout = Duration::from_millis(u64::from(snmp.timeout_ms));
            let walker = SnmpWalker::V2c(snmp.community.clone());
            execute_scalar_get(
                job,
                transport,
                at_unix_ms,
                &snmp.oids,
                &snmp.columns,
                timeout,
                &walker,
            )
            .await
        }
        CheckSpec::SnmpV3(v3) => {
            let timeout = Duration::from_millis(u64::from(v3.timeout_ms));
            let walker = SnmpWalker::V3(v3.auth.clone());
            execute_scalar_get(
                job,
                transport,
                at_unix_ms,
                &v3.oids,
                &v3.columns,
                timeout,
                &walker,
            )
            .await
        }
        CheckSpec::SnmpTable(table) => {
            let timeout = Duration::from_millis(u64::from(table.timeout_ms));
            execute_snmp_table(job, transport, at_unix_ms, table, timeout).await
        }
        CheckSpec::SnmpV3Table(table) => {
            let timeout = Duration::from_millis(u64::from(table.timeout_ms));
            execute_snmp_v3_table(job, transport, at_unix_ms, table, timeout).await
        }
        CheckSpec::SnmpOptical(check) => {
            let timeout = Duration::from_millis(u64::from(check.timeout_ms));
            let walker = SnmpWalker::V2c(check.community.clone());
            execute_optical(job, transport, at_unix_ms, &check.probes, timeout, &walker).await
        }
        CheckSpec::SnmpV3Optical(check) => {
            let timeout = Duration::from_millis(u64::from(check.timeout_ms));
            let walker = SnmpWalker::V3(check.auth.clone());
            execute_optical(job, transport, at_unix_ms, &check.probes, timeout, &walker).await
        }
        CheckSpec::SnmpMau(check) => {
            let timeout = Duration::from_millis(u64::from(check.timeout_ms));
            let walker = SnmpWalker::V2c(check.community.clone());
            execute_mau(
                job,
                transport,
                at_unix_ms,
                check.entity_fallback,
                timeout,
                &walker,
            )
            .await
        }
        CheckSpec::SnmpV3Mau(check) => {
            let timeout = Duration::from_millis(u64::from(check.timeout_ms));
            let walker = SnmpWalker::V3(check.auth.clone());
            execute_mau(
                job,
                transport,
                at_unix_ms,
                check.entity_fallback,
                timeout,
                &walker,
            )
            .await
        }
        CheckSpec::SnmpNeighbors(check) => {
            let timeout = Duration::from_millis(u64::from(check.timeout_ms));
            let walker = SnmpWalker::V2c(check.community.clone());
            execute_neighbors(job, transport, at_unix_ms, &check.columns, timeout, &walker).await
        }
        CheckSpec::SnmpV3Neighbors(check) => {
            let timeout = Duration::from_millis(u64::from(check.timeout_ms));
            let walker = SnmpWalker::V3(check.auth.clone());
            execute_neighbors(job, transport, at_unix_ms, &check.columns, timeout, &walker).await
        }
        CheckSpec::SnmpL3(check) => {
            let timeout = Duration::from_millis(u64::from(check.timeout_ms));
            let walker = SnmpWalker::V2c(check.community.clone());
            execute_l3(job, transport, at_unix_ms, &check.columns, timeout, &walker).await
        }
        CheckSpec::SnmpV3L3(check) => {
            let timeout = Duration::from_millis(u64::from(check.timeout_ms));
            let walker = SnmpWalker::V3(check.auth.clone());
            execute_l3(job, transport, at_unix_ms, &check.columns, timeout, &walker).await
        }
        CheckSpec::SnmpArp(check) => {
            let timeout = Duration::from_millis(u64::from(check.timeout_ms));
            let walker = SnmpWalker::V2c(check.community.clone());
            execute_arp(
                job,
                transport,
                at_unix_ms,
                &check.columns,
                check.max_rows,
                timeout,
                &walker,
            )
            .await
        }
        CheckSpec::SnmpV3Arp(check) => {
            let timeout = Duration::from_millis(u64::from(check.timeout_ms));
            let walker = SnmpWalker::V3(check.auth.clone());
            execute_arp(
                job,
                transport,
                at_unix_ms,
                &check.columns,
                check.max_rows,
                timeout,
                &walker,
            )
            .await
        }
        CheckSpec::SnmpRouting(check) => {
            let timeout = Duration::from_millis(u64::from(check.timeout_ms));
            let walker = SnmpWalker::V2c(check.community.clone());
            execute_routing(
                job,
                transport,
                at_unix_ms,
                &check.columns,
                &check.route_probes,
                timeout,
                &walker,
            )
            .await
        }
        CheckSpec::SnmpV3Routing(check) => {
            let timeout = Duration::from_millis(u64::from(check.timeout_ms));
            let walker = SnmpWalker::V3(check.auth.clone());
            execute_routing(
                job,
                transport,
                at_unix_ms,
                &check.columns,
                &check.route_probes,
                timeout,
                &walker,
            )
            .await
        }
        CheckSpec::MerakiCollect(_) => {
            // Meraki collects fan out to many results and are dispatched via `execute_meraki` in
            // `run_stream`; `execute` (one job → one result) is never used for them. Guard anyway.
            tracing::error!(job_id = %job.job_id, "meraki collect routed through execute(); ignoring");
            result(job, at_unix_ms, CheckOutcome::Error, Vec::new())
        }
        CheckSpec::Http(http) => execute_http(job, transport, at_unix_ms, http).await,
        CheckSpec::Dns(dns) => execute_dns(job, transport, at_unix_ms, dns).await,
    }
}

/// Stable metric name for an SNMP OID. Known OIDs get friendly names; others fall back to
/// an OID-derived name (a bounded set per profile, so cardinality stays controlled).
fn snmp_metric_name(oid: &str) -> String {
    match oid {
        "1.3.6.1.2.1.1.3.0" => "snmp_sys_uptime_ticks".to_owned(),
        other => format!("snmp_oid_{}", other.replace('.', "_")),
    }
}

fn result(
    job: &PollJob,
    at_unix_ms: i64,
    outcome: CheckOutcome,
    samples: Vec<Sample>,
) -> PollResult {
    PollResult {
        job_id: job.job_id,
        node_id: job.node_id,
        at_unix_ms,
        outcome,
        samples,
        interfaces: Vec::new(),
        sys_descr: None,
        dns_chain: None,
        neighbors: None,
        l3: None,
        arp: None,
        routing: None,
        observational: false,
        poller_id: None,
        // Stamped by `run_stream` from the poll span before publish (empty here = no trace).
        trace_context: Default::default(),
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Stamp the producing poller's provenance onto a result (ADR-009). `None` leaves it unset (the
/// single-process skeleton / an unidentified poller); core reads that as "unknown / central".
fn stamp_poller_id(result: &mut PollResult, poller_id: &Option<Arc<str>>) {
    if let Some(id) = poller_id {
        result.poller_id = Some(id.to_string());
    }
}
