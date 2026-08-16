// SPDX-License-Identifier: AGPL-3.0-only
//! The poller work loop.
//!
//! A poller consumes [`PollJob`]s from the bus, executes them via the [`Transport`]
//! abstraction (never a raw protocol), and publishes [`PollResult`]s back. It holds no
//! state beyond the in-flight job — that statelessness is what lets pollers scale out
//! and fail over (ADR-003/009). Counters are reported raw; rates are derived later
//! (ADR-012).

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
    CheckOutcome, CheckSpec, DiscoveredInterface, PollJob, PollResult, Sample, SnmpColumn,
    SnmpMetaColumn, SnmpTableCheck, SnmpV3TableCheck,
};
use yagra_common::{
    DnsFailure, IfIndex, InterfaceField, MetricKind, NodeId, METRIC_DNS_ANSWER_COUNT,
    METRIC_DNS_CHAIN_LENGTH, METRIC_DNS_RESOLVE_MS, METRIC_DNS_UP, METRIC_HTTP_BODY_MATCH,
    METRIC_HTTP_BODY_TRUNCATED, METRIC_HTTP_RESPONSE_TIME_MS, METRIC_HTTP_STATUS_CODE,
    METRIC_HTTP_UP, METRIC_ICMP_RTT_MS, METRIC_SSL_CERT_DAYS_TO_EXPIRY, OID_IF_HIGH_SPEED,
};
use yagra_transport::{
    DnsProbeSpec, HttpProbeSpec, MerakiCollectSpec, SnmpTableSample, SnmpTableString, SnmpV3Params,
    Transport, TransportError,
};

/// sysDescr.0 — system description scalar (the v3 GET form).
const SYSDESCR_OID: &str = "1.3.6.1.2.1.1.1.0";
/// sysDescr column base — walking it yields the `.0` instance (the v2c path).
const SYSDESCR_BASE: &str = "1.3.6.1.2.1.1.1";

/// Execute one job and build its result. Pure given the transport and timestamp, so it
/// is unit-testable without a clock or a bus.
pub async fn execute(job: &PollJob, transport: &dyn Transport, at_unix_ms: i64) -> PollResult {
    match &job.check {
        CheckSpec::Icmp(icmp) => {
            let timeout = Duration::from_millis(u64::from(icmp.timeout_ms));
            match transport.probe_icmp(job.target, icmp.count, timeout).await {
                Ok(probe) => {
                    let outcome = if probe.reachable {
                        CheckOutcome::Reachable
                    } else {
                        CheckOutcome::Unreachable
                    };
                    let mut samples = vec![Sample::gauge("icmp_loss_pct", probe.loss_pct)];
                    if let Some(rtt) = probe.rtt_ms {
                        samples.push(Sample::gauge(METRIC_ICMP_RTT_MS, rtt));
                    }
                    result(job, at_unix_ms, outcome, samples)
                }
                Err(err) => {
                    tracing::warn!(job_id = %job.job_id, error = %err, "icmp probe failed");
                    result(job, at_unix_ms, CheckOutcome::Error, Vec::new())
                }
            }
        }
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
            let walker = SnmpWalker::V3(yagra_transport::SnmpV3Params {
                user: v3.user.clone(),
                security_level: v3.security_level.clone(),
                auth_protocol: v3.auth_protocol.clone(),
                auth_key: v3.auth_key.clone(),
                priv_protocol: v3.priv_protocol.clone(),
                priv_key: v3.priv_key.clone(),
            });
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
            let walker = SnmpWalker::V3(yagra_transport::SnmpV3Params {
                user: check.user.clone(),
                security_level: check.security_level.clone(),
                auth_protocol: check.auth_protocol.clone(),
                auth_key: check.auth_key.clone(),
                priv_protocol: check.priv_protocol.clone(),
                priv_key: check.priv_key.clone(),
            });
            execute_optical(job, transport, at_unix_ms, &check.probes, timeout, &walker).await
        }
        CheckSpec::SnmpNeighbors(check) => {
            let timeout = Duration::from_millis(u64::from(check.timeout_ms));
            let walker = SnmpWalker::V2c(check.community.clone());
            execute_neighbors(job, transport, at_unix_ms, &check.columns, timeout, &walker).await
        }
        CheckSpec::SnmpV3Neighbors(check) => {
            let timeout = Duration::from_millis(u64::from(check.timeout_ms));
            let walker = SnmpWalker::V3(yagra_transport::SnmpV3Params {
                user: check.user.clone(),
                security_level: check.security_level.clone(),
                auth_protocol: check.auth_protocol.clone(),
                auth_key: check.auth_key.clone(),
                priv_protocol: check.priv_protocol.clone(),
                priv_key: check.priv_key.clone(),
            });
            execute_neighbors(job, transport, at_unix_ms, &check.columns, timeout, &walker).await
        }
        CheckSpec::SnmpL3(check) => {
            let timeout = Duration::from_millis(u64::from(check.timeout_ms));
            let walker = SnmpWalker::V2c(check.community.clone());
            execute_l3(job, transport, at_unix_ms, &check.columns, timeout, &walker).await
        }
        CheckSpec::SnmpV3L3(check) => {
            let timeout = Duration::from_millis(u64::from(check.timeout_ms));
            let walker = SnmpWalker::V3(yagra_transport::SnmpV3Params {
                user: check.user.clone(),
                security_level: check.security_level.clone(),
                auth_protocol: check.auth_protocol.clone(),
                auth_key: check.auth_key.clone(),
                priv_protocol: check.priv_protocol.clone(),
                priv_key: check.priv_key.clone(),
            });
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
            let walker = SnmpWalker::V3(yagra_transport::SnmpV3Params {
                user: check.user.clone(),
                security_level: check.security_level.clone(),
                auth_protocol: check.auth_protocol.clone(),
                auth_key: check.auth_key.clone(),
                priv_protocol: check.priv_protocol.clone(),
                priv_key: check.priv_key.clone(),
            });
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
            let walker = SnmpWalker::V3(yagra_transport::SnmpV3Params {
                user: check.user.clone(),
                security_level: check.security_level.clone(),
                auth_protocol: check.auth_protocol.clone(),
                auth_key: check.auth_key.clone(),
                priv_protocol: check.priv_protocol.clone(),
                priv_key: check.priv_key.clone(),
            });
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
        CheckSpec::Http(http) => {
            let timeout = Duration::from_millis(u64::from(http.timeout_ms));
            let spec = HttpProbeSpec {
                url: http.url.clone(),
                method: http.method,
                verify_tls: http.verify_tls,
                follow_redirects: http.follow_redirects,
                auth: http.auth.clone(),
                // A monitor with neither body feature never reads the body — the budget is asked
                // for only when there is something to decide with it.
                body_capture_bytes: http.body_capture_bytes(),
            };
            match transport.probe_http(&spec, timeout).await {
                Ok(probe) => {
                    // http_up = reachable AND the status matched expectation (down or wrong-status
                    // both read as 0 so a single threshold covers them).
                    let status_ok = probe
                        .status_code
                        .is_some_and(|c| http.expected_status.matches(c));
                    let up = probe.reachable && status_ok;
                    let mut samples =
                        vec![Sample::gauge(METRIC_HTTP_UP, if up { 1.0 } else { 0.0 })];
                    if let Some(code) = probe.status_code {
                        samples.push(Sample::gauge(METRIC_HTTP_STATUS_CODE, f64::from(code)));
                    }
                    // Only when the endpoint actually answered. The transport fills
                    // `response_time_ms` on the failure path too, but that value is time-to-
                    // timeout/connect-failure, not a response time: emitting it would draw a
                    // "slow response" at the timeout value for the whole outage, and a
                    // response-time threshold would then page for the same incident `http_up`
                    // already covers.
                    //
                    // Deliberately unlike the DNS arm below, which emits `dns_resolve_ms` even
                    // when the name does not resolve — there the failure value is how long the
                    // resolver took to *answer* NXDOMAIN/SERVFAIL, a real measurement. Do not
                    // "align" the two.
                    if probe.reachable {
                        samples.push(Sample::gauge(
                            METRIC_HTTP_RESPONSE_TIME_MS,
                            probe.response_time_ms,
                        ));
                    }
                    if let Some(days) = probe.cert_days_to_expiry {
                        samples.push(Sample::gauge(METRIC_SSL_CERT_DAYS_TO_EXPIRY, days));
                    }
                    // The content rule, when the monitor carries one and the endpoint answered.
                    // Deliberately NOT folded into `http_up`: that gauge's meaning is liveness +
                    // status, and widening it here would retroactively change what every existing
                    // `http_up` series meant.
                    if let (Some(rule), true) = (http.body_match.as_ref(), probe.reachable) {
                        // `None` = we could not decide: the body outgrew the budget (or a read
                        // failed) and the keyword was not in the prefix. Reported as 0 — the same
                        // value as a genuine violation, because the one thing it must never do is
                        // read as healthy (ADR-047 決定 3). `http_body_truncated` is what tells the
                        // two apart afterwards.
                        let verdict = probe
                            .body
                            .as_ref()
                            .and_then(|b| rule.satisfied_by(&b.text, b.truncated));
                        if verdict.is_none() {
                            tracing::warn!(
                                job_id = %job.job_id,
                                url = %http.url,
                                max_bytes = http.body_max_bytes,
                                "url monitor body rule could not be decided within its byte budget; \
                                 reporting it as unsatisfied"
                            );
                        }
                        samples.push(Sample::gauge(
                            METRIC_HTTP_BODY_MATCH,
                            if verdict == Some(true) { 1.0 } else { 0.0 },
                        ));
                        if let Some(body) = probe.body.as_ref() {
                            samples.push(Sample::gauge(
                                METRIC_HTTP_BODY_TRUNCATED,
                                if body.truncated { 1.0 } else { 0.0 },
                            ));
                        }
                    }
                    // Operator-named values lifted out of a JSON body (ADR-047 Inc.3). Parsed once
                    // for the whole rule set — a document big enough to matter should not be
                    // re-parsed per rule.
                    //
                    // A truncated body is almost always invalid JSON and so yields nothing, which
                    // is the correct answer rather than a special case: half a document cannot be
                    // read reliably, and every failure here records **no sample**. Writing 0 would
                    // be indistinguishable from the value genuinely being 0 (ADR-047 決定 3).
                    if !http.json_extract.is_empty() && probe.reachable {
                        match probe
                            .body
                            .as_ref()
                            .and_then(|b| serde_json::from_str::<serde_json::Value>(&b.text).ok())
                        {
                            Some(doc) => {
                                for rule in &http.json_extract {
                                    match rule.extract(&doc) {
                                        Some(v) => samples.push(Sample::gauge(&rule.metric, v)),
                                        None => tracing::debug!(
                                            job_id = %job.job_id,
                                            metric = %rule.metric,
                                            path = %rule.path,
                                            "url monitor json path yielded no usable number; recording no sample"
                                        ),
                                    }
                                }
                            }
                            None => tracing::warn!(
                                job_id = %job.job_id,
                                url = %http.url,
                                truncated = probe.body.as_ref().is_some_and(|b| b.truncated),
                                rules = http.json_extract.len(),
                                "url monitor response body is not valid JSON; recording no extracted metrics"
                            ),
                        }
                    }
                    let outcome = if probe.reachable {
                        CheckOutcome::Reachable
                    } else {
                        CheckOutcome::Unreachable
                    };
                    result(job, at_unix_ms, outcome, samples)
                }
                Err(err) => {
                    // An un-runnable config (bad URL / SSRF-blocked): record http_up = 0 so the
                    // series exists and alerts can fire, then report the error outcome.
                    tracing::warn!(job_id = %job.job_id, error = %err, "http probe failed");
                    result(
                        job,
                        at_unix_ms,
                        CheckOutcome::Error,
                        vec![Sample::gauge(METRIC_HTTP_UP, 0.0)],
                    )
                }
            }
        }
        CheckSpec::Dns(dns) => {
            let timeout = Duration::from_millis(u64::from(dns.timeout_ms));
            let spec = DnsProbeSpec {
                name: dns.name.clone(),
                record_type: dns.record_type,
                resolver: dns
                    .resolver
                    .map(|ip| std::net::SocketAddr::new(ip, dns.resolver_port)),
                max_depth: dns.max_depth,
            };
            match transport.resolve_dns(&spec, timeout).await {
                Ok(chain) => {
                    let up = chain.resolved();
                    let samples = vec![
                        Sample::gauge(METRIC_DNS_UP, if up { 1.0 } else { 0.0 }),
                        Sample::gauge(METRIC_DNS_RESOLVE_MS, chain.resolve_ms),
                        // `as f64` on a hop/answer count is lossless at any realistic size (both
                        // are capped well below 2^53) and the metric is a float anyway.
                        Sample::gauge(METRIC_DNS_CHAIN_LENGTH, chain.hops.len() as f64),
                        Sample::gauge(
                            METRIC_DNS_ANSWER_COUNT,
                            chain.terminal_answer_count() as f64,
                        ),
                    ];
                    // Same split as the HTTP arm: "the resolver answered" is reachability, "the
                    // answer was absent or negative" is a threshold concern. So NXDOMAIN /
                    // SERVFAIL / REFUSED stay Reachable with dns_up = 0 (the seeded critical
                    // threshold fires), while a timeout — no answer at all — is Unreachable.
                    let outcome = match chain.failure {
                        Some(DnsFailure::Timeout) => CheckOutcome::Unreachable,
                        _ => CheckOutcome::Reachable,
                    };
                    let mut r = result(job, at_unix_ms, outcome, samples);
                    r.dns_chain = Some(chain);
                    r
                }
                Err(err) => {
                    // An un-runnable config (bad name / SSRF-blocked resolver / no system
                    // resolver): record dns_up = 0 so the series exists and alerts can fire.
                    tracing::warn!(job_id = %job.job_id, error = %err, "dns probe failed");
                    result(
                        job,
                        at_unix_ms,
                        CheckOutcome::Error,
                        vec![Sample::gauge(METRIC_DNS_UP, 0.0)],
                    )
                }
            }
        }
    }
}

/// Execute a Cisco Meraki org-scoped collect. Unlike the per-node checks, this fans **one** job out
/// to **many** results: the transport pages the org's Dashboard endpoints (read-only) and returns
/// per-device observations, and we emit one ordinary [`PollResult`] per device (attributed via the
/// inlined serial→node_id map) so the whole consume/write/alert spine works unchanged. A device the
/// API reports but that we didn't import is simply skipped (scope enforced at fan-out). Metrics are
/// gauges (ADR-012 exception — the source pre-aggregates); uplinks become interface inventory rows.
pub async fn execute_meraki(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
) -> Vec<PollResult> {
    let CheckSpec::MerakiCollect(check) = &job.check else {
        return Vec::new();
    };
    let timeout = Duration::from_millis(u64::from(check.timeout_ms));
    let spec = MerakiCollectSpec {
        org_id: check.org_id.clone(),
        base_url: check.base_url.clone(),
        api_key: check.api_key.clone(),
        tier: check.tier,
        network_ids: check.network_ids.clone(),
        per_page: check.per_page,
        target_rps: check.target_rps,
    };
    let observations = match transport.collect_meraki(&spec, timeout).await {
        Ok(obs) => obs,
        Err(err) => {
            tracing::warn!(job_id = %job.job_id, org = %check.org_id, error = %err, "meraki collect failed");
            return Vec::new();
        }
    };

    let by_serial: HashMap<&str, NodeId> = check
        .devices
        .iter()
        .map(|d| (d.serial.as_str(), d.node_id))
        .collect();

    let mut results = Vec::new();
    for obs in observations {
        let Some(&node_id) = by_serial.get(obs.serial.as_str()) else {
            continue; // reported by the API but not imported → not in scope
        };
        let samples = obs
            .samples
            .into_iter()
            .map(|s| match s.ifindex {
                Some(idx) => Sample::interface(s.metric, IfIndex(idx), s.value, MetricKind::Gauge),
                None => Sample::gauge(s.metric, s.value),
            })
            .collect();
        let interfaces = obs
            .uplinks
            .into_iter()
            .map(|u| DiscoveredInterface {
                ifindex: IfIndex(u.ifindex),
                if_name: Some(u.name),
                if_alias: None,
                if_speed: None,
            })
            .collect();
        results.push(PollResult {
            job_id: job.job_id,
            node_id,
            at_unix_ms,
            outcome: CheckOutcome::Reachable,
            samples,
            interfaces,
            sys_descr: None,
            dns_chain: None,
            neighbors: None,
            l3: None,
            arp: None,
            routing: None,
            observational: false,
            poller_id: None,
            trace_context: Default::default(),
        });
    }
    results
}

/// The credential half of an SNMP check that differs between v2c and v3 (community vs USM params).
/// Capturing it here lets everything above it — the scalar GET, the column walk, the interface
/// metadata fold, the identity probe — be written once instead of twice: v2c and v3 differ only in
/// which transport method carries the credential, never in what is done with the rows.
enum SnmpWalker {
    V2c(String),
    V3(SnmpV3Params),
}

impl SnmpWalker {
    /// GET scalar OIDs via the appropriate protocol.
    async fn get(
        &self,
        transport: &dyn Transport,
        target: IpAddr,
        oids: &[String],
        timeout: Duration,
    ) -> Result<Vec<yagra_transport::SnmpSample>, TransportError> {
        match self {
            SnmpWalker::V2c(community) => {
                transport.snmp_get(target, community, oids, timeout).await
            }
            SnmpWalker::V3(params) => transport.snmp_v3_get(target, params, oids, timeout).await,
        }
    }

    /// Fetch `sysDescr.0` for the identity probe, so core can fill the node's maker/model.
    /// Best-effort: `None` on any transport error or an empty/missing value. The two protocols
    /// reach it differently — v2c walks the column base (its GET path returns numerics only),
    /// v3 does a scalar string GET.
    async fn fetch_sys_descr(
        &self,
        transport: &dyn Transport,
        target: IpAddr,
        timeout: Duration,
    ) -> Option<String> {
        let value = match self {
            SnmpWalker::V2c(community) => {
                let bases = [SYSDESCR_BASE.to_owned()];
                transport
                    .snmp_walk_strings(target, community, &bases, timeout)
                    .await
                    .ok()?
                    .into_iter()
                    .find(|r| r.oid_base == SYSDESCR_BASE)
                    .map(|r| r.value)
            }
            SnmpWalker::V3(params) => {
                let oids = [SYSDESCR_OID.to_owned()];
                transport
                    .snmp_v3_get_strings(target, params, &oids, timeout)
                    .await
                    .ok()?
                    .into_iter()
                    .find(|r| r.oid == SYSDESCR_OID)
                    .map(|r| r.value)
            }
        };
        value.filter(|v| !v.is_empty())
    }

    /// Walk numeric table columns via the appropriate protocol.
    async fn walk(
        &self,
        transport: &dyn Transport,
        target: IpAddr,
        columns: &[String],
        timeout: Duration,
    ) -> Result<Vec<SnmpTableSample>, TransportError> {
        match self {
            SnmpWalker::V2c(community) => {
                transport
                    .snmp_walk(target, community, columns, timeout)
                    .await
            }
            SnmpWalker::V3(params) => {
                transport
                    .snmp_v3_walk(target, params, columns, timeout)
                    .await
            }
        }
    }

    /// Walk table columns keeping raw instance indices and raw octets (the neighbour walk).
    ///
    /// `max_rows` is the caller's budget for the whole walk — see `Transport::snmp_walk_instances`.
    /// Every caller states one; there is deliberately no default, because the tables this walker is
    /// pointed at range from tens of rows (`ipAddrTable`) to hundreds of thousands
    /// (`ipNetToPhysicalTable`) and no single number is right for both.
    async fn walk_instances(
        &self,
        transport: &dyn Transport,
        target: IpAddr,
        columns: &[String],
        timeout: Duration,
        max_rows: usize,
    ) -> Result<Vec<yagra_transport::SnmpInstanceRow>, TransportError> {
        match self {
            SnmpWalker::V2c(community) => {
                transport
                    .snmp_walk_instances(target, community, columns, timeout, max_rows)
                    .await
            }
            SnmpWalker::V3(params) => {
                transport
                    .snmp_v3_walk_instances(target, params, columns, timeout, max_rows)
                    .await
            }
        }
    }

    /// Walk string-valued table columns (interface metadata) via the appropriate protocol.
    async fn walk_strings(
        &self,
        transport: &dyn Transport,
        target: IpAddr,
        columns: &[String],
        timeout: Duration,
    ) -> Result<Vec<SnmpTableString>, TransportError> {
        match self {
            SnmpWalker::V2c(community) => {
                transport
                    .snmp_walk_strings(target, community, columns, timeout)
                    .await
            }
            SnmpWalker::V3(params) => {
                transport
                    .snmp_v3_walk_strings(target, params, columns, timeout)
                    .await
            }
        }
    }
}

/// Execute an SNMP scalar-GET check (v2c or v3, selected by `walker`): GET the bare OIDs and the
/// explicitly-named scalar columns together, name each sample (a configured column keeps its metric
/// name and kind; a bare OID falls back to the poller's built-in naming), and run the identity
/// probe when core asked for one.
///
/// The v2c and v3 arms of [`execute`] used to carry a copy of this each — ~48 lines apiece that
/// differed only in the credential type and which transport method was called. The table path had
/// already solved exactly that with [`SnmpWalker`]; this brings the scalar path in line, so an SNMP
/// behaviour change (a new outcome rule, a naming tweak) is one edit rather than two that can drift.
async fn execute_scalar_get(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    oids: &[String],
    columns: &[SnmpColumn],
    timeout: Duration,
    walker: &SnmpWalker,
) -> PollResult {
    let col_by_oid: HashMap<&str, &SnmpColumn> =
        columns.iter().map(|c| (c.oid.as_str(), c)).collect();
    let mut all_oids = oids.to_vec();
    all_oids.extend(columns.iter().map(|c| c.oid.clone()));
    match walker.get(transport, job.target, &all_oids, timeout).await {
        Ok(samples) => {
            // No values back ⇒ treat as unreachable (agent down / wrong credential).
            let outcome = if samples.is_empty() {
                CheckOutcome::Unreachable
            } else {
                CheckOutcome::Reachable
            };
            let mapped = samples
                .into_iter()
                .map(|s| match col_by_oid.get(s.oid.as_str()) {
                    // Configured column → honour its metric name and kind.
                    Some(col) => Sample {
                        metric: col.metric_name.clone(),
                        ifindex: None,
                        value: s.value,
                        kind: col.kind,
                    },
                    // Bare OID → the poller's built-in naming (gauge).
                    None => Sample::gauge(snmp_metric_name(&s.oid), s.value),
                })
                .collect();
            let mut r = result(job, at_unix_ms, outcome, mapped);
            if job.probe_identity && outcome == CheckOutcome::Reachable {
                r.sys_descr = walker.fetch_sys_descr(transport, job.target, timeout).await;
            }
            r
        }
        Err(err) => {
            tracing::warn!(job_id = %job.job_id, error = %err, "snmp get failed");
            result(job, at_unix_ms, CheckOutcome::Error, Vec::new())
        }
    }
}

/// Execute an SNMP table-walk check (v2c or v3, selected by `walker`): numeric columns become
/// per-interface samples (keyed by ifIndex, using the column's explicit metric name and kind — no
/// OID-name guessing), and metadata columns become [`DiscoveredInterface`] records (PostgreSQL
/// inventory, never TSDB labels — ADR-011). Shared by [`execute_snmp_table`] (v2c) and
/// [`execute_snmp_v3_table`] (v3).
async fn execute_table_walk(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    columns: &[SnmpColumn],
    meta_columns: &[SnmpMetaColumn],
    timeout: Duration,
    walker: &SnmpWalker,
) -> PollResult {
    let by_base: HashMap<&str, &SnmpColumn> = columns.iter().map(|c| (c.oid.as_str(), c)).collect();
    // Interface-speed columns (ifSpeed) declared in meta_columns; ifHighSpeed is walked poller-side.
    let speed_oids: Vec<String> = meta_columns
        .iter()
        .filter(|m| matches!(m.field, InterfaceField::Speed))
        .map(|m| m.oid.clone())
        .collect();

    // ONE numeric walk for the metric columns AND the interface-speed columns (ifSpeed +
    // ifHighSpeed), demuxed by column base. A table poll previously opened a fresh SNMP session
    // (UDP socket + client) per walk — metric, ifSpeed, ifHighSpeed — holding the poll's global
    // permit ~3× longer and amplifying permit exhaustion during a mass outage (S5). Folding the
    // numeric walks into one leaves just this walk + the string-metadata walk below (2 sessions).
    let mut numeric_oids: Vec<String> = columns.iter().map(|c| c.oid.clone()).collect();
    numeric_oids.extend(speed_oids.iter().cloned());
    if !speed_oids.is_empty() {
        numeric_oids.push(OID_IF_HIGH_SPEED.to_owned());
    }

    let mut samples = Vec::new();
    let mut raw_speed: HashMap<u32, f64> = HashMap::new();
    let mut raw_high: HashMap<u32, f64> = HashMap::new();
    match walker
        .walk(transport, job.target, &numeric_oids, timeout)
        .await
    {
        Ok(rows) => {
            for row in rows {
                if let Some(col) = by_base.get(row.oid_base.as_str()) {
                    samples.push(Sample::interface(
                        col.metric_name.clone(),
                        IfIndex(row.ifindex),
                        row.value,
                        col.kind,
                    ));
                } else if row.oid_base == OID_IF_HIGH_SPEED {
                    raw_high.insert(row.ifindex, row.value);
                } else if speed_oids.iter().any(|o| o == &row.oid_base) {
                    raw_speed.insert(row.ifindex, row.value);
                }
            }
        }
        Err(err) => tracing::warn!(job_id = %job.job_id, error = %err, "snmp table walk failed"),
    }

    let interfaces = walk_interface_metadata(
        job,
        transport,
        walker,
        meta_columns,
        &raw_speed,
        &raw_high,
        timeout,
    )
    .await;

    // Reachable iff the agent returned at least one value (matches the scalar SNMP arm).
    let outcome = if samples.is_empty() {
        CheckOutcome::Unreachable
    } else {
        CheckOutcome::Reachable
    };

    PollResult {
        job_id: job.job_id,
        node_id: job.node_id,
        at_unix_ms,
        outcome,
        samples,
        interfaces,
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

/// Row budget for one ENTITY-MIB walk.
///
/// `entPhysicalTable` is one row per part — a fully populated chassis switch runs to a few
/// thousand — and the alias and containment columns are walked once per poll. The bound is what
/// stops a pathological agent from turning an optical probe into an unbounded read.
const OPTICAL_ENTITY_MAX_ROWS: usize = 8192;

/// Execute an optical-transceiver (DDM/DOM) probe — ADR-062.
///
/// Two shapes, chosen per dialect by [`optical::simple_dialect`]:
///
/// - **Plain columns** (Huawei / Juniper / H3C): one numeric walk of two columns and a fixed
///   multiplier. Juniper and H3C key their rows by ifIndex already; Huawei keys them by
///   `entPhysicalIndex` and needs the same translation as the dialect below.
/// - **ENTITY-SENSOR-MIB** (Cisco / Arista and anything else standards-based): four numeric
///   columns correlated per sensor, plus the entity's free text to say which direction it is.
///
/// The result is **observational**. A device whose transceivers will not answer — because it has
/// none, because the MIB is unimplemented, or because the view excludes it — is not unreachable,
/// and reporting it as such would page someone about a healthy box. Everything else here is best
/// effort by design: an unreadable dialect logs and contributes nothing.
async fn execute_optical(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    probes: &[yagra_bus::OpticalProbe],
    timeout: Duration,
    walker: &SnmpWalker,
) -> PollResult {
    let mut samples = Vec::new();
    // Built lazily and at most once per poll: both dialects that need it walk the same two
    // ENTITY-MIB columns, and a node bound to two vendor profiles must not walk them twice.
    let mut entity: Option<optical::EntityIndex> = None;

    for probe in probes {
        if probe.rx_metric.is_none() && probe.tx_metric.is_none() {
            continue;
        }
        let readings = match optical::simple_dialect(probe.flavor) {
            Some(dialect) => walk_simple_optical(job, transport, walker, timeout, &dialect).await,
            None => walk_entity_sensor_optical(job, transport, walker, timeout).await,
        };
        if readings.is_empty() {
            continue;
        }

        // Translate entPhysicalIndex → ifIndex for the dialects that need it. A row that does not
        // translate is DROPPED: emitting it under its raw entity index would land the series on
        // `MetricDimension::Entity`, which costs storage and appears on no chart (decision 3).
        let resolved = if probe.flavor.is_ifindex_keyed() {
            readings
        } else {
            let idx = match &entity {
                Some(idx) => idx,
                None => {
                    entity = Some(walk_entity_index(job, transport, walker, timeout).await);
                    entity.as_ref().expect("just assigned")
                }
            };
            let before = readings.len();
            let mapped: Vec<optical::OpticalSample> = readings
                .into_iter()
                .filter_map(|s| {
                    idx.ifindex_for(s.ifindex)
                        .map(|ifindex| optical::OpticalSample { ifindex, ..s })
                })
                .collect();
            if mapped.len() < before {
                tracing::debug!(
                    job_id = %job.job_id,
                    flavor = ?probe.flavor,
                    dropped = before - mapped.len(),
                    "optical rows dropped: no interface maps to their physical entity"
                );
            }
            mapped
        };

        for s in optical::dedupe_readings(resolved) {
            let metric = match s.reading {
                optical::OpticalReading::Rx => probe.rx_metric.as_ref(),
                optical::OpticalReading::Tx => probe.tx_metric.as_ref(),
            };
            if let Some(name) = metric {
                samples.push(Sample::interface(
                    name.clone(),
                    IfIndex(s.ifindex),
                    s.dbm,
                    MetricKind::Gauge,
                ));
            }
        }
    }

    PollResult {
        job_id: job.job_id,
        node_id: job.node_id,
        at_unix_ms,
        // Never a liveness statement — see the doc comment.
        outcome: CheckOutcome::Reachable,
        samples,
        interfaces: Vec::new(),
        sys_descr: None,
        dns_chain: None,
        neighbors: None,
        l3: None,
        arp: None,
        routing: None,
        observational: true,
        poller_id: None,
        trace_context: Default::default(),
    }
}

/// Walk a two-column optical dialect and scale it to dBm. Indices are whatever the dialect keys
/// on; the caller translates them if needed.
async fn walk_simple_optical(
    job: &PollJob,
    transport: &dyn Transport,
    walker: &SnmpWalker,
    timeout: Duration,
    dialect: &optical::SimpleDialect,
) -> Vec<optical::OpticalSample> {
    let columns = vec![dialect.rx_oid.to_owned(), dialect.tx_oid.to_owned()];
    let rows = match walker.walk(transport, job.target, &columns, timeout).await {
        Ok(rows) => rows,
        Err(err) => {
            tracing::debug!(job_id = %job.job_id, error = %err, "optical walk failed");
            return Vec::new();
        }
    };
    rows.into_iter()
        .filter_map(|row| {
            let reading = if row.oid_base == dialect.rx_oid {
                optical::OpticalReading::Rx
            } else if row.oid_base == dialect.tx_oid {
                optical::OpticalReading::Tx
            } else {
                return None;
            };
            Some(optical::OpticalSample {
                ifindex: row.ifindex,
                reading,
                dbm: row.value * dialect.scale,
            })
        })
        .collect()
}

/// Walk ENTITY-SENSOR-MIB and correlate its columns into dBm readings keyed by
/// `entPhysicalIndex`.
///
/// Four numeric columns in one session, then the entity text in a second — the same two-session
/// shape the interface walk uses, and for the same reason: the numeric and string walkers are
/// separate transports.
async fn walk_entity_sensor_optical(
    job: &PollJob,
    transport: &dyn Transport,
    walker: &SnmpWalker,
    timeout: Duration,
) -> Vec<optical::OpticalSample> {
    let columns = vec![
        optical::ENT_SENSOR_TYPE.to_owned(),
        optical::ENT_SENSOR_SCALE.to_owned(),
        optical::ENT_SENSOR_PRECISION.to_owned(),
        optical::ENT_SENSOR_VALUE.to_owned(),
    ];
    let rows = match walker.walk(transport, job.target, &columns, timeout).await {
        Ok(rows) => rows,
        Err(err) => {
            tracing::debug!(job_id = %job.job_id, error = %err, "entity-sensor walk failed");
            return Vec::new();
        }
    };
    let mut types: HashMap<u32, i64> = HashMap::new();
    let mut scales: HashMap<u32, i64> = HashMap::new();
    let mut precisions: HashMap<u32, i64> = HashMap::new();
    let mut values: HashMap<u32, i64> = HashMap::new();
    for row in rows {
        let v = row.value as i64;
        let bucket = match row.oid_base.as_str() {
            optical::ENT_SENSOR_TYPE => &mut types,
            optical::ENT_SENSOR_SCALE => &mut scales,
            optical::ENT_SENSOR_PRECISION => &mut precisions,
            optical::ENT_SENSOR_VALUE => &mut values,
            _ => continue,
        };
        bucket.insert(row.ifindex, v);
    }
    if values.is_empty() {
        return Vec::new();
    }

    // Only now walk the text, and only for the entities that produced a candidate reading.
    let text = walk_entity_text(job, transport, walker, timeout).await;

    // Ascending entity order so "first lane wins" in `dedupe_readings` is deterministic.
    let mut ents: Vec<u32> = values.keys().copied().collect();
    ents.sort_unstable();
    ents.into_iter()
        .filter_map(|ent| {
            let dbm = optical::entity_sensor_dbm(
                *values.get(&ent)?,
                *types.get(&ent)?,
                // `units(9)` / no decimals are the MIB's own defaults for an agent that omits them.
                scales.get(&ent).copied().unwrap_or(9),
                precisions.get(&ent).copied().unwrap_or(0),
            )?;
            let reading = optical::reading_from_text(text.get(&ent)?)?;
            Some(optical::OpticalSample {
                ifindex: ent,
                reading,
                dbm,
            })
        })
        .collect()
}

/// `entPhysicalIndex` → the best free text describing it (`entPhysicalName` preferred, falling
/// back to `entPhysicalDescr`).
///
/// Both are walked because vendors disagree on which one carries the direction: Cisco puts it in
/// `entPhysicalDescr`, and some agents leave that generic and put the useful string in
/// `entPhysicalName`. Whichever parses wins — `reading_from_text` refuses anything ambiguous, so
/// preferring one cannot silently pick a wrong direction.
async fn walk_entity_text(
    job: &PollJob,
    transport: &dyn Transport,
    walker: &SnmpWalker,
    timeout: Duration,
) -> HashMap<u32, String> {
    let columns = vec![
        optical::ENT_PHYSICAL_DESCR.to_owned(),
        optical::ENT_PHYSICAL_NAME.to_owned(),
    ];
    let mut out: HashMap<u32, String> = HashMap::new();
    match walker
        .walk_strings(transport, job.target, &columns, timeout)
        .await
    {
        Ok(rows) => {
            for row in rows {
                // Device-supplied text: kept only to classify, never rendered or used as a label.
                if optical::reading_from_text(&row.value).is_some() {
                    out.insert(row.ifindex, row.value);
                } else {
                    out.entry(row.ifindex).or_insert(row.value);
                }
            }
        }
        Err(err) => {
            tracing::debug!(job_id = %job.job_id, error = %err, "entity text walk failed");
        }
    }
    out
}

/// Walk the two ENTITY-MIB relations that attach a physical entity to an interface.
async fn walk_entity_index(
    job: &PollJob,
    transport: &dyn Transport,
    walker: &SnmpWalker,
    timeout: Duration,
) -> optical::EntityIndex {
    let mut idx = optical::EntityIndex::default();
    let columns = vec![
        optical::ENT_ALIAS_MAPPING.to_owned(),
        optical::ENT_PHYSICAL_CONTAINED_IN.to_owned(),
    ];
    match walker
        .walk_instances(
            transport,
            job.target,
            &columns,
            timeout,
            OPTICAL_ENTITY_MAX_ROWS,
        )
        .await
    {
        Ok(rows) => {
            let (alias, parent): (Vec<_>, Vec<_>) = rows
                .into_iter()
                .partition(|r| r.oid_base == optical::ENT_ALIAS_MAPPING);
            idx.add_alias_rows(&alias);
            idx.add_parent_rows(&parent);
        }
        Err(err) => {
            tracing::debug!(job_id = %job.job_id, error = %err, "entity index walk failed");
        }
    }
    idx
}

/// Execute a CDP/LLDP neighbour walk (v2c or v3, selected by `walker`) — ADR-038.
///
/// Three properties distinguish this from every other arm:
///
/// * The result is **observational**: it says nothing about the node's reachability. `outcome` is
///   fixed at `Reachable` and core ignores it, because either alternative is a real bug — reporting
///   `Unreachable` on a failed hourly walk pages someone for a healthy device, and reporting
///   `Reachable` unconditionally would cancel a genuine outage ICMP had detected.
/// * `neighbors` is `Some` **only when the walk actually produced rows to interpret**. A transport
///   failure sends `None`, so core writes nothing rather than recording "every link disappeared".
/// * A device that simply has no neighbours sends `Some(empty)`, which *does* replace the stored
///   set — that is a real observation, and it is how an unplugged switch stops showing stale peers.
async fn execute_neighbors(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    columns: &[yagra_bus::SnmpNeighborColumn],
    timeout: Duration,
    walker: &SnmpWalker,
) -> PollResult {
    let bases: Vec<String> = columns.iter().map(|c| c.oid.clone()).collect();
    let mut r = result(job, at_unix_ms, CheckOutcome::Reachable, Vec::new());
    r.observational = true;
    match walker
        .walk_instances(
            transport,
            job.target,
            &bases,
            timeout,
            yagra_common::MAX_NEIGHBOR_WALK_ROWS,
        )
        .await
    {
        Ok(rows) => {
            let set = crate::neighbors::assemble(columns, &rows);
            if set.truncated {
                metrics::counter!("yagra_neighbor_rows_truncated_total").increment(1);
                tracing::warn!(
                    job_id = %job.job_id,
                    kept = set.len(),
                    "neighbour set exceeded the per-node cap; the excess was dropped"
                );
            }
            r.samples.push(Sample::gauge(
                yagra_common::METRIC_SNMP_NEIGHBOR_COUNT,
                set.len() as f64,
            ));
            r.neighbors = Some(set);
        }
        Err(err) => {
            // No set, no count sample: the poll observed nothing, and saying "0 neighbours" here
            // would be a claim the walk never made.
            tracing::debug!(job_id = %job.job_id, error = %err, "neighbour walk failed");
        }
    }
    r
}

/// Execute an interface-address walk (v2c or v3, selected by `walker`) — ADR-043.
///
/// Structurally identical to [`execute_neighbors`], and for the same three reasons:
///
/// * The result is **observational**. An hourly address walk that timed out must not push
///   `Unreachable` into the dwell window ICMP owns, and must not report `Reachable` either — that
///   would cancel a genuine outage. It has nothing to say about liveness, so it says nothing.
/// * `l3` is `Some` **only when the walk actually produced rows**. A transport failure sends `None`,
///   so core writes nothing rather than recording that the device lost its addressing — which, one
///   derivation later, would read as every link through that node disappearing.
/// * A device with no addresses to report sends `Some(empty)`, which *does* replace the stored
///   snapshot. That is a real observation.
async fn execute_l3(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    columns: &[yagra_bus::SnmpL3Column],
    timeout: Duration,
    walker: &SnmpWalker,
) -> PollResult {
    let bases: Vec<String> = columns.iter().map(|c| c.oid.clone()).collect();
    let mut r = result(job, at_unix_ms, CheckOutcome::Reachable, Vec::new());
    r.observational = true;
    match walker
        .walk_instances(
            transport,
            job.target,
            &bases,
            timeout,
            yagra_common::MAX_L3_WALK_ROWS,
        )
        .await
    {
        Ok(rows) => {
            let snapshot = crate::l3::assemble(columns, &rows);
            if snapshot.truncated {
                metrics::counter!("yagra_l3_rows_truncated_total").increment(1);
                tracing::warn!(
                    job_id = %job.job_id,
                    kept = snapshot.len(),
                    "interface-address set exceeded the per-node cap; the excess was dropped"
                );
            }
            r.samples.push(Sample::gauge(
                yagra_common::METRIC_SNMP_L3_ADDRESS_COUNT,
                snapshot.len() as f64,
            ));
            r.l3 = Some(snapshot);
        }
        Err(err) => {
            // No snapshot, no count sample: the poll observed nothing, and saying "0 addresses"
            // here would be a claim the walk never made.
            tracing::debug!(job_id = %job.job_id, error = %err, "interface-address walk failed");
        }
    }
    r
}

/// Execute an ARP / IPv6-neighbour walk (v2c or v3, selected by `walker`) — ADR-043 Increment 3.
///
/// Shares the three properties of [`execute_neighbors`] and [`execute_l3`] — observational, `Some`
/// only when the walk produced rows, `Some(empty)` is a real answer — and adds a fourth that is
/// specific to this check:
///
/// * **The row budget comes from the job**, and the truncation flag is derived from it here rather
///   than inside the assembler. Only this layer knows how many rows it asked for, so only this
///   layer can tell a full answer from a walk that ran out — and `truncated` is what stops a
///   partial read of a large table from being published as the whole picture.
async fn execute_arp(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    columns: &[yagra_bus::SnmpArpColumn],
    max_rows: u32,
    timeout: Duration,
    walker: &SnmpWalker,
) -> PollResult {
    let bases: Vec<String> = columns.iter().map(|c| c.oid.clone()).collect();
    // Core decides the fleet-wide budget, but a core that sent nonsense (or a field an N-1 core
    // never sent at all) must not turn into "walk the whole table": the transport's own ceiling is
    // the backstop and this is the floor under it.
    let budget = usize::try_from(max_rows)
        .unwrap_or(yagra_common::MAX_ARP_WALK_ROWS)
        .clamp(1, yagra_common::MAX_ARP_WALK_ROWS);
    let mut r = result(job, at_unix_ms, CheckOutcome::Reachable, Vec::new());
    r.observational = true;
    match walker
        .walk_instances(transport, job.target, &bases, timeout, budget)
        .await
    {
        Ok(rows) => {
            // The walk stops *at* the budget, so hitting it exactly is the signal that there was
            // more table behind it. One row short is a complete answer.
            let walk_truncated = rows.len() >= budget;
            let summary = crate::arp::assemble(columns, &rows, walk_truncated);
            if summary.truncated {
                metrics::counter!("yagra_arp_rows_truncated_total").increment(1);
                tracing::warn!(
                    job_id = %job.job_id,
                    kept = summary.len(),
                    observed = summary.observed,
                    "ARP cache exceeded the walk or sample cap; the excess was dropped"
                );
            }
            r.samples.push(Sample::gauge(
                yagra_common::METRIC_SNMP_ARP_ENTRY_COUNT,
                f64::from(summary.observed),
            ));
            r.arp = Some(summary);
        }
        Err(err) => {
            // No summary, no count sample: the poll observed nothing, and saying "0 endpoints" here
            // would be a claim the walk never made — one that would then age every discovered
            // endpoint behind this router out of the table.
            tracing::debug!(job_id = %job.job_id, error = %err, "ARP walk failed");
        }
    }
    r
}

/// Execute a routing-adjacency collection (v2c or v3, selected by `walker`) — ADR-043 Increment 4.
///
/// Shares the three properties of [`execute_neighbors`], [`execute_l3`] and [`execute_arp`] —
/// observational, `Some` only when something was actually collected, `Some(empty)` is a real answer
/// — and adds a fourth specific to this check:
///
/// * **Two calls, two budgets.** The adjacency walk reads tables sized by the device's peering
///   mesh; the probes read one destination each. Sharing a budget would let a route reflector's
///   hundreds of iBGP peers consume it before the probes ran, and which half lost would depend on
///   the order the bases happened to be listed in — a silent, configuration-dependent gap.
///
/// A failure of *either* call leaves that half's rows out and lets the other half through: a device
/// that answers `bgpPeerState` but has no `inetCidrRouteTable` is ordinary, and refusing the whole
/// observation because one table is absent would collect nothing from most of the fleet.
async fn execute_routing(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    columns: &[yagra_bus::SnmpRoutingColumn],
    probes: &[yagra_bus::SnmpRouteProbe],
    timeout: Duration,
    walker: &SnmpWalker,
) -> PollResult {
    let mut r = result(job, at_unix_ms, CheckOutcome::Reachable, Vec::new());
    r.observational = true;

    let mut rows = Vec::new();
    let mut answered = false;
    let mut truncated = false;

    if !columns.is_empty() {
        let bases: Vec<String> = columns.iter().map(|c| c.oid.clone()).collect();
        match walker
            .walk_instances(
                transport,
                job.target,
                &bases,
                timeout,
                yagra_common::MAX_ROUTING_WALK_ROWS,
            )
            .await
        {
            Ok(found) => {
                truncated |= found.len() >= yagra_common::MAX_ROUTING_WALK_ROWS;
                rows.extend(found);
                answered = true;
            }
            Err(err) => {
                tracing::debug!(job_id = %job.job_id, error = %err, "routing adjacency walk failed");
            }
        }
    }

    if !probes.is_empty() {
        // Every probe is its own subtree root, so they go in one call: the transport walks each in
        // turn and the shared budget is the *probe* budget, sized for exactly this many roots.
        let bases: Vec<String> = probes.iter().map(|p| p.oid.clone()).collect();
        match walker
            .walk_instances(
                transport,
                job.target,
                &bases,
                timeout,
                yagra_common::MAX_ROUTE_PROBE_ROWS,
            )
            .await
        {
            Ok(found) => {
                truncated |= found.len() >= yagra_common::MAX_ROUTE_PROBE_ROWS;
                rows.extend(found);
                answered = true;
            }
            Err(err) => {
                tracing::debug!(job_id = %job.job_id, error = %err, "route probes failed");
            }
        }
    }

    if !answered {
        // Neither half produced anything, so nothing was observed. Sending `Some(empty)` here would
        // erase the node's stored adjacency on a transport failure — and, one derivation later,
        // every link that node was in.
        return r;
    }

    let snapshot = crate::routing::assemble(columns, probes, &rows, truncated);
    if snapshot.truncated {
        metrics::counter!("yagra_routing_rows_truncated_total").increment(1);
        tracing::warn!(
            job_id = %job.job_id,
            kept = snapshot.len(),
            "routing adjacency exceeded a walk or sample cap; the excess was dropped"
        );
    }
    r.samples.push(Sample::gauge(
        yagra_common::METRIC_SNMP_ROUTING_ADJACENCY_COUNT,
        snapshot.len() as f64,
    ));
    r.routing = Some(snapshot);
    r
}

/// Execute an SNMP v2c table-walk check — a thin wrapper over [`execute_table_walk`].
async fn execute_snmp_table(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    table: &SnmpTableCheck,
    timeout: Duration,
) -> PollResult {
    let walker = SnmpWalker::V2c(table.community.clone());
    execute_table_walk(
        job,
        transport,
        at_unix_ms,
        &table.columns,
        &table.meta_columns,
        timeout,
        &walker,
    )
    .await
}

/// Execute an SNMP v3 (USM) table-walk check — the v3 analogue of [`execute_snmp_table`]. Maps the
/// USM params exactly as the scalar v3 arm does, then shares the walk/mapping logic.
async fn execute_snmp_v3_table(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    table: &SnmpV3TableCheck,
    timeout: Duration,
) -> PollResult {
    let params = SnmpV3Params {
        user: table.user.clone(),
        security_level: table.security_level.clone(),
        auth_protocol: table.auth_protocol.clone(),
        auth_key: table.auth_key.clone(),
        priv_protocol: table.priv_protocol.clone(),
        priv_key: table.priv_key.clone(),
    };
    let walker = SnmpWalker::V3(params);
    execute_table_walk(
        job,
        transport,
        at_unix_ms,
        &table.columns,
        &table.meta_columns,
        timeout,
        &walker,
    )
    .await
}

/// Fold interface metadata into [`DiscoveredInterface`]s: walk the `ifName`/`ifAlias` **string**
/// columns (the poll's second and only other SNMP session), and resolve `if_speed` from the
/// `ifSpeed`/`ifHighSpeed` values already gathered by the combined numeric walk in the caller (S5).
async fn walk_interface_metadata(
    job: &PollJob,
    transport: &dyn Transport,
    walker: &SnmpWalker,
    meta_columns: &[SnmpMetaColumn],
    raw_speed: &HashMap<u32, f64>,
    raw_high: &HashMap<u32, f64>,
    timeout: Duration,
) -> Vec<DiscoveredInterface> {
    let field_by_base: HashMap<&str, InterfaceField> = meta_columns
        .iter()
        .map(|m| (m.oid.as_str(), m.field))
        .collect();
    let string_oids: Vec<String> = meta_columns
        .iter()
        .filter(|m| matches!(m.field, InterfaceField::Name | InterfaceField::Alias))
        .map(|m| m.oid.clone())
        .collect();

    let mut ifs: BTreeMap<u32, DiscoveredInterface> = BTreeMap::new();
    let blank = |ifindex: u32| DiscoveredInterface {
        ifindex: IfIndex(ifindex),
        if_name: None,
        if_alias: None,
        if_speed: None,
    };

    if !string_oids.is_empty() {
        match walker
            .walk_strings(transport, job.target, &string_oids, timeout)
            .await
        {
            Ok(rows) => {
                for row in rows {
                    let Some(field) = field_by_base.get(row.oid_base.as_str()) else {
                        continue;
                    };
                    let rec = ifs.entry(row.ifindex).or_insert_with(|| blank(row.ifindex));
                    match field {
                        InterfaceField::Name => rec.if_name = Some(row.value),
                        InterfaceField::Alias => rec.if_alias = Some(row.value),
                        InterfaceField::Speed => {}
                    }
                }
            }
            Err(err) => {
                tracing::debug!(job_id = %job.job_id, error = %err, "snmp ifName/ifAlias walk failed");
            }
        }
    }

    // Resolve the effective bandwidth from the pre-walked 32-bit `ifSpeed` and 64-bit `ifHighSpeed`
    // (Mbps), so links above the ~4.29 Gbps `ifSpeed` cap report their true rate. ifHighSpeed is
    // gathered poller-side (not a bus column) to keep the job contract N/N-1 compatible.
    for ifindex in raw_speed
        .keys()
        .chain(raw_high.keys())
        .copied()
        .collect::<BTreeSet<u32>>()
    {
        match resolve_if_speed(
            raw_speed.get(&ifindex).copied(),
            raw_high.get(&ifindex).copied(),
        ) {
            Some(bps) => {
                let rec = ifs.entry(ifindex).or_insert_with(|| blank(ifindex));
                rec.if_speed = Some(bps);
            }
            None => tracing::debug!(
                job_id = %job.job_id,
                ifindex,
                "no resolvable interface speed (ifSpeed/ifHighSpeed absent or out of range)"
            ),
        }
    }

    ifs.into_values().collect()
}

/// Resolve the effective interface bandwidth (bits/sec) from `ifSpeed` (32-bit) and `ifHighSpeed`
/// (units of 1,000,000 bits/sec).
///
/// Below the 32-bit `ifSpeed` saturation point (`u32::MAX`, ~4.29 Gbps) `ifSpeed` is authoritative
/// — it can express sub-Mbps links that `ifHighSpeed` rounds to 0. At/above the cap (or when
/// `ifSpeed` is missing/0) the 64-bit `ifHighSpeed` is used. Non-finite, negative, or
/// out-of-`i64`-range values are dropped rather than stored as a bogus saturated speed.
fn resolve_if_speed(if_speed: Option<f64>, if_high_speed: Option<f64>) -> Option<i64> {
    let sane = |v: f64| v.is_finite() && (0.0..=i64::MAX as f64).contains(&v);
    let speed = if_speed.filter(|v| sane(*v));
    let high_bps = if_high_speed
        .filter(|v| v.is_finite() && *v > 0.0)
        .map(|mbps| mbps * 1_000_000.0)
        .filter(|bps| sane(*bps));

    // 4_294_967_295: the value a 32-bit `ifSpeed` reports once the real rate exceeds it.
    const IF_SPEED_CAP: f64 = u32::MAX as f64;

    match speed {
        Some(s) if s > 0.0 && s < IF_SPEED_CAP => Some(s as i64),
        _ => high_bps
            .map(|bps| bps as i64)
            .or_else(|| speed.map(|s| s as i64)),
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

/// Run the poll loop over a stream of jobs. Each job runs concurrently under the
/// [`PollLimiter`]: a global concurrency cap bounds total load and per-device single-flight
/// drops a poll whose target is still being probed (backpressure, monitoring-conventions).
/// Returns when the stream ends. Stream-generic so the same loop drives both the in-memory
/// bus (tests/skeleton) and the NATS queue subscription (production), ADR-003/009.
///
/// `poller_id` (its sanitized id) is stamped onto every published result for provenance; the shared
/// `results_total` counter is bumped on each successful publish and `inflight` tracks probes in
/// flight — both feed the poller's heartbeat telemetry (ADR-009).
pub async fn run_stream<S>(
    mut jobs: S,
    sink: Arc<StoreForwardSink>,
    transport: Arc<dyn Transport>,
    limiter: Arc<PollLimiter>,
    poller_id: Option<Arc<str>>,
    results_total: Arc<AtomicU64>,
    inflight: Arc<AtomicU64>,
) where
    S: Stream<Item = PollJob> + Unpin,
{
    while let Some(job) = jobs.next().await {
        // Meraki org collectors share a sentinel target (0.0.0.0) and are single-flighted per org
        // by core, so they use only the global concurrency cap (not per-device single-flight, which
        // would wrongly drop concurrent collects for different orgs) and fan out to many results.
        if matches!(job.check, CheckSpec::MerakiCollect(_)) {
            let Some(guard) = limiter.begin_global().await else {
                continue; // shutdown
            };
            let sink = sink.clone();
            let transport = transport.clone();
            let poller_id = poller_id.clone();
            let results_total = results_total.clone();
            let inflight = inflight.clone();
            // Poll span: child of core's dispatch span when the job carried one (legacy/poll-now),
            // else a fresh root (working-set). Secret-free fields only (no community/API key).
            let span = tracing::info_span!(
                "poll.meraki_collect",
                job_id = %job.job_id,
                node_id = %job.node_id,
            );
            yagra_telemetry::set_span_parent(&span, &job.trace_context);
            tokio::spawn(
                async move {
                    let _guard = guard;
                    inflight.fetch_add(1, Ordering::Relaxed);
                    metrics::counter!("yagra_poll_jobs_executed_total").increment(1);
                    // Snapshot the poll span's context once; every fanned-out result carries it so
                    // core's ingest spans all join this trace.
                    let ctx = yagra_telemetry::current_trace_context();
                    let results = execute_meraki(&job, transport.as_ref(), now_unix_ms()).await;
                    for mut result in results {
                        stamp_poller_id(&mut result, &poller_id);
                        result.trace_context = ctx.clone();
                        // Store-and-forward: publishes live when connected, else buffers for replay
                        // (Phase 3). Infallible — the poll loop never blocks/errors on the return.
                        sink.submit(result).await;
                        results_total.fetch_add(1, Ordering::Relaxed);
                    }
                    inflight.fetch_sub(1, Ordering::Relaxed);
                }
                .instrument(span),
            );
            continue;
        }

        // DNS monitors share a target by design — many names, one resolver, and every check using
        // the system resolver carries the same 0.0.0.0 display address. Per-target single-flight
        // would therefore drop every DNS check but one on each cycle, so they take the global-only
        // guard for the same reason Meraki collectors do. Pile-up stays bounded by each check's
        // total timeout budget (≤30 s, enforced in the transport) plus the global concurrency cap.
        let guard = if matches!(job.check, CheckSpec::Dns(_)) {
            limiter.begin_global().await
        } else {
            limiter.try_begin(job.target).await
        };
        let Some(guard) = guard else {
            metrics::counter!("yagra_poll_skipped_backpressure_total").increment(1);
            tracing::debug!(target = %job.target, "skipping poll: previous still in flight");
            continue;
        };
        let sink = sink.clone();
        let transport = transport.clone();
        let poller_id = poller_id.clone();
        let results_total = results_total.clone();
        let inflight = inflight.clone();
        // Poll span: child of core's dispatch span when the job carried one (legacy/poll-now), else
        // a fresh root (working-set). Secret-free fields only (no community/creds — security.md).
        let span = tracing::info_span!(
            "poll.execute",
            job_id = %job.job_id,
            node_id = %job.node_id,
            target = %job.target,
        );
        yagra_telemetry::set_span_parent(&span, &job.trace_context);
        tokio::spawn(
            async move {
                let _guard = guard; // released (and target unmarked) when the probe finishes
                inflight.fetch_add(1, Ordering::Relaxed);
                metrics::counter!("yagra_poll_jobs_executed_total").increment(1);
                let mut result = execute(&job, transport.as_ref(), now_unix_ms()).await;
                stamp_poller_id(&mut result, &poller_id);
                // Carry the poll span's context so core's result-ingest span joins this trace.
                result.trace_context = yagra_telemetry::current_trace_context();
                // Store-and-forward: live-publish when connected, else buffer for replay (Phase 3).
                sink.submit(result).await;
                results_total.fetch_add(1, Ordering::Relaxed);
                inflight.fetch_sub(1, Ordering::Relaxed);
            }
            .instrument(span),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use uuid::Uuid;
    use yagra_bus::{Bus, IcmpCheck, InMemoryBus, SnmpCheck};
    use yagra_common::NodeId;
    use yagra_transport::{FakeTransport, SnmpSample};

    fn icmp_job() -> PollJob {
        PollJob::icmp(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IcmpCheck::default(),
            30,
        )
    }

    /// A neighbour job's own columns, as core sends them.
    fn neighbor_columns() -> Vec<yagra_bus::SnmpNeighborColumn> {
        yagra_common::builtin_neighbor_columns()
            .into_iter()
            .map(|(field, oid)| yagra_bus::SnmpNeighborColumn {
                field,
                oid: oid.to_owned(),
            })
            .collect()
    }

    fn neighbor_job() -> PollJob {
        PollJob::snmp_neighbors(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            yagra_bus::SnmpNeighborCheck {
                community: "public".into(),
                columns: neighbor_columns(),
                timeout_ms: 2000,
            },
            3600,
        )
    }

    /// The safety property from ADR-038: a neighbour result must never reach the liveness state
    /// machine. `outcome` feeds it on every result, so a device that speaks no LLDP/CDP — an
    /// entirely normal state — would otherwise register as either an outage or a recovery.
    #[tokio::test]
    async fn a_neighbor_result_is_observational_and_states_nothing_about_liveness() {
        let transport = FakeTransport::reachable(1.0);
        let r = execute(&neighbor_job(), &transport, 0).await;
        assert!(
            r.observational,
            "core keys its skip-the-alert-engine branch off this flag"
        );
        // A device with nothing to report still made a real observation: an empty set, which
        // replaces whatever was stored (so an unplugged switch stops showing stale peers).
        assert_eq!(
            r.neighbors.as_ref().map(yagra_common::NeighborSet::len),
            Some(0)
        );
        assert_eq!(
            r.samples.len(),
            1,
            "one bounded node-level count sample, and no per-adjacency series"
        );
        assert_eq!(
            r.samples[0].metric,
            yagra_common::METRIC_SNMP_NEIGHBOR_COUNT
        );
    }

    fn l3_job() -> PollJob {
        PollJob::snmp_l3(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            yagra_bus::SnmpL3Check {
                community: "public".into(),
                columns: yagra_common::builtin_l3_columns()
                    .into_iter()
                    .map(|(field, oid)| yagra_bus::SnmpL3Column {
                        field,
                        oid: oid.to_owned(),
                    })
                    .collect(),
                timeout_ms: 2000,
            },
            3600,
        )
    }

    /// The same safety property ADR-038 established, now for ADR-043's walk. `outcome` feeds the
    /// liveness state machine on *every* result, so an hourly address walk must be able to say
    /// nothing about reachability — reporting a failure would page someone for a healthy device,
    /// and reporting success would cancel a genuine outage ICMP had detected.
    #[tokio::test]
    async fn an_l3_result_is_observational_and_states_nothing_about_liveness() {
        let transport = FakeTransport::reachable(1.0);
        let r = execute(&l3_job(), &transport, 0).await;
        assert!(
            r.observational,
            "core keys its skip-the-alert-engine branch off this flag"
        );
        // A device with no addresses to report still made a real observation: an empty snapshot,
        // which replaces whatever was stored.
        assert_eq!(r.l3.as_ref().map(yagra_common::L3Snapshot::len), Some(0));
        assert_eq!(
            r.samples.len(),
            1,
            "one bounded node-level count sample, and no per-address series — an IP in a label is \
             the cardinality explosion CLAUDE.md §7.1 names"
        );
        assert_eq!(
            r.samples[0].metric,
            yagra_common::METRIC_SNMP_L3_ADDRESS_COUNT
        );
    }

    /// The other direction, which is the half that is easy to forget: an ordinary liveness check
    /// must **not** be marked observational, or it stops driving alerts entirely.
    #[tokio::test]
    async fn an_icmp_result_is_not_observational() {
        let transport = FakeTransport::reachable(1.0);
        let job = PollJob::icmp(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            yagra_bus::IcmpCheck::default(),
            60,
        );
        let r = execute(&job, &transport, 0).await;
        assert!(!r.observational);
        assert!(r.l3.is_none());
    }

    /// A failed walk must send **no** set. Sending `Some(empty)` would tell core the device has no
    /// neighbours, wiping a correct stored adjacency because one SNMP request timed out.
    #[tokio::test]
    async fn a_failed_neighbor_walk_reports_no_set_rather_than_an_empty_one() {
        /// A transport whose instance walk always fails; everything else delegates to the fake.
        struct WalkFails(FakeTransport);
        #[async_trait::async_trait]
        impl Transport for WalkFails {
            async fn snmp_walk_instances(
                &self,
                _t: IpAddr,
                _c: &str,
                _o: &[String],
                _to: Duration,
                _max: usize,
            ) -> Result<Vec<yagra_transport::SnmpInstanceRow>, TransportError> {
                Err(TransportError::Io("snmp connect refused".into()))
            }
            async fn snmp_v3_walk_instances(
                &self,
                _t: IpAddr,
                _p: &SnmpV3Params,
                _o: &[String],
                _to: Duration,
                _max: usize,
            ) -> Result<Vec<yagra_transport::SnmpInstanceRow>, TransportError> {
                Err(TransportError::Io("snmp connect refused".into()))
            }
            async fn probe_icmp(
                &self,
                t: IpAddr,
                c: u8,
                to: Duration,
            ) -> Result<yagra_transport::IcmpProbe, TransportError> {
                self.0.probe_icmp(t, c, to).await
            }
            async fn snmp_get(
                &self,
                t: IpAddr,
                c: &str,
                o: &[String],
                to: Duration,
            ) -> Result<Vec<SnmpSample>, TransportError> {
                self.0.snmp_get(t, c, o, to).await
            }
            async fn snmp_v3_get(
                &self,
                t: IpAddr,
                p: &SnmpV3Params,
                o: &[String],
                to: Duration,
            ) -> Result<Vec<SnmpSample>, TransportError> {
                self.0.snmp_v3_get(t, p, o, to).await
            }
            async fn snmp_v3_get_strings(
                &self,
                t: IpAddr,
                p: &SnmpV3Params,
                o: &[String],
                to: Duration,
            ) -> Result<Vec<yagra_transport::SnmpStringSample>, TransportError> {
                self.0.snmp_v3_get_strings(t, p, o, to).await
            }
            async fn snmp_walk(
                &self,
                t: IpAddr,
                c: &str,
                o: &[String],
                to: Duration,
            ) -> Result<Vec<SnmpTableSample>, TransportError> {
                self.0.snmp_walk(t, c, o, to).await
            }
            async fn snmp_walk_strings(
                &self,
                t: IpAddr,
                c: &str,
                o: &[String],
                to: Duration,
            ) -> Result<Vec<SnmpTableString>, TransportError> {
                self.0.snmp_walk_strings(t, c, o, to).await
            }
            async fn snmp_v3_walk(
                &self,
                t: IpAddr,
                p: &SnmpV3Params,
                o: &[String],
                to: Duration,
            ) -> Result<Vec<SnmpTableSample>, TransportError> {
                self.0.snmp_v3_walk(t, p, o, to).await
            }
            async fn snmp_v3_walk_strings(
                &self,
                t: IpAddr,
                p: &SnmpV3Params,
                o: &[String],
                to: Duration,
            ) -> Result<Vec<SnmpTableString>, TransportError> {
                self.0.snmp_v3_walk_strings(t, p, o, to).await
            }
            async fn probe_http(
                &self,
                s: &HttpProbeSpec,
                to: Duration,
            ) -> Result<yagra_transport::HttpProbe, TransportError> {
                self.0.probe_http(s, to).await
            }
            async fn resolve_dns(
                &self,
                s: &yagra_transport::DnsProbeSpec,
                to: Duration,
            ) -> Result<yagra_transport::DnsChain, TransportError> {
                self.0.resolve_dns(s, to).await
            }
            async fn collect_meraki(
                &self,
                s: &yagra_transport::MerakiCollectSpec,
                to: Duration,
            ) -> Result<Vec<yagra_transport::MerakiObservation>, TransportError> {
                self.0.collect_meraki(s, to).await
            }
        }

        let r = execute(
            &neighbor_job(),
            &WalkFails(FakeTransport::reachable(1.0)),
            0,
        )
        .await;
        assert!(r.observational);
        assert!(
            r.neighbors.is_none(),
            "a failed walk must not read as 'this device has no neighbours'"
        );
        assert!(
            r.samples.is_empty(),
            "no count sample either — the poll observed nothing to count"
        );
    }

    #[tokio::test]
    async fn a_v3_neighbor_job_takes_the_same_path() {
        let job = PollJob::snmp_v3_neighbors(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            yagra_bus::SnmpV3NeighborCheck {
                user: "monitor".into(),
                security_level: "authpriv".into(),
                auth_protocol: Some("sha256".into()),
                auth_key: Some("auth-pass-12345".into()),
                priv_protocol: Some("aes256".into()),
                priv_key: Some("priv-pass-12345".into()),
                columns: neighbor_columns(),
                timeout_ms: 2000,
            },
            3600,
        );
        let r = execute(&job, &FakeTransport::reachable(1.0), 0).await;
        assert!(r.observational);
        assert!(r.neighbors.is_some());
    }

    #[tokio::test]
    async fn meraki_collect_fans_out_to_mapped_nodes_only() {
        use yagra_bus::{MerakiCollectCheck, MerakiDeviceRef};
        use yagra_common::MerakiTier;
        use yagra_transport::{MerakiObservation, MerakiSample, MerakiUplink};

        let node_a = NodeId::new();
        let transport = FakeTransport::reachable(1.0).with_meraki(vec![
            MerakiObservation {
                serial: "Q2-A".into(),
                samples: vec![
                    MerakiSample {
                        metric: "meraki_device_up".into(),
                        ifindex: None,
                        value: 1.0,
                    },
                    MerakiSample {
                        metric: "meraki_uplink_loss_pct".into(),
                        ifindex: Some(1),
                        value: 0.5,
                    },
                ],
                uplinks: vec![MerakiUplink {
                    ifindex: 1,
                    name: "WAN1".into(),
                }],
            },
            // Reported by the API but not imported → must be skipped (scope at fan-out).
            MerakiObservation {
                serial: "Q2-UNMAPPED".into(),
                samples: vec![MerakiSample {
                    metric: "meraki_device_up".into(),
                    ifindex: None,
                    value: 1.0,
                }],
                uplinks: vec![],
            },
        ]);

        let check = MerakiCollectCheck {
            org_id: "1".into(),
            meraki_org_uuid: Uuid::nil(),
            tier: MerakiTier::Uplink,
            base_url: "https://api.meraki.com".into(),
            api_key: "k".into(),
            devices: vec![MerakiDeviceRef {
                serial: "Q2-A".into(),
                node_id: node_a,
            }],
            network_ids: vec![],
            per_page: 1000,
            target_rps: 2.0,
            timeout_ms: 30_000,
        };
        let job = PollJob::meraki_collect(Uuid::nil(), check, 300);

        let results = execute_meraki(&job, &transport, 42).await;
        assert_eq!(results.len(), 1, "only the imported device is emitted");
        let r = &results[0];
        assert_eq!(r.node_id, node_a);
        assert_eq!(r.at_unix_ms, 42);
        assert_eq!(r.outcome, CheckOutcome::Reachable);
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == "meraki_uplink_loss_pct" && s.ifindex == Some(IfIndex(1))));
        assert_eq!(
            r.interfaces,
            vec![DiscoveredInterface {
                ifindex: IfIndex(1),
                if_name: Some("WAN1".into()),
                if_alias: None,
                if_speed: None,
            }]
        );
    }

    #[tokio::test]
    async fn reachable_probe_yields_rtt_and_loss_samples() {
        let t = FakeTransport::reachable(7.5);
        let r = execute(&icmp_job(), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Reachable);
        // Both loss and rtt samples present.
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == "icmp_rtt_ms" && s.value == 7.5));
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == "icmp_loss_pct" && s.value == 0.0));
    }

    #[tokio::test]
    async fn unreachable_probe_has_no_rtt_sample() {
        let t = FakeTransport::unreachable();
        let r = execute(&icmp_job(), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Unreachable);
        assert!(!r.samples.iter().any(|s| s.metric == "icmp_rtt_ms"));
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == "icmp_loss_pct" && s.value == 100.0));
    }

    fn snmp_job() -> PollJob {
        PollJob::snmp(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            SnmpCheck {
                community: "public".to_owned(),
                oids: vec!["1.3.6.1.2.1.1.3.0".to_owned()],
                columns: Vec::new(),
                timeout_ms: 2000,
            },
            30,
        )
    }

    #[tokio::test]
    async fn snmp_samples_map_to_named_metrics() {
        let t = FakeTransport::reachable(0.0).with_snmp(vec![SnmpSample {
            oid: "1.3.6.1.2.1.1.3.0".to_owned(),
            value: 123.0,
        }]);
        let r = execute(&snmp_job(), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Reachable);
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == "snmp_sys_uptime_ticks" && s.value == 123.0));
    }

    #[tokio::test]
    async fn snmp_no_values_is_unreachable() {
        // FakeTransport with no canned SNMP samples → empty → unreachable.
        let t = FakeTransport::reachable(0.0);
        let r = execute(&snmp_job(), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Unreachable);
        assert!(r.samples.is_empty());
    }

    #[tokio::test]
    async fn snmp_probe_identity_fetches_sysdescr() {
        use yagra_transport::{SnmpSample, SnmpTableString};
        let mut job = snmp_job();
        job.probe_identity = true;
        let t = FakeTransport::reachable(0.0)
            .with_snmp(vec![SnmpSample {
                oid: "1.3.6.1.2.1.1.3.0".to_owned(),
                value: 1.0,
            }])
            .with_snmp_table_strings(vec![SnmpTableString {
                oid_base: "1.3.6.1.2.1.1.1".to_owned(),
                ifindex: 0,
                value: "Huawei Versatile Routing Platform Software VRP USG6000".to_owned(),
            }]);
        let r = execute(&job, &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Reachable);
        assert_eq!(
            r.sys_descr.as_deref(),
            Some("Huawei Versatile Routing Platform Software VRP USG6000")
        );
    }

    #[tokio::test]
    async fn snmp_without_probe_identity_has_no_sysdescr() {
        use yagra_transport::SnmpSample;
        // probe_identity defaults false on snmp_job(); even with a sysDescr available it's not sent.
        let t = FakeTransport::reachable(0.0).with_snmp(vec![SnmpSample {
            oid: "1.3.6.1.2.1.1.3.0".to_owned(),
            value: 1.0,
        }]);
        let r = execute(&snmp_job(), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Reachable);
        assert!(r.sys_descr.is_none());
    }

    #[tokio::test]
    async fn snmp_scalar_columns_use_configured_metric_name() {
        use yagra_bus::SnmpColumn;
        use yagra_common::MetricKind;
        use yagra_transport::SnmpSample;
        let job = PollJob::snmp(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            SnmpCheck {
                community: "public".to_owned(),
                oids: Vec::new(),
                columns: vec![SnmpColumn {
                    metric_name: "cpu_util".to_owned(),
                    oid: "1.3.6.1.4.1.9.2.1.58.0".to_owned(),
                    kind: MetricKind::Gauge,
                }],
                timeout_ms: 2000,
            },
            30,
        );
        let t = FakeTransport::reachable(0.0).with_snmp(vec![SnmpSample {
            oid: "1.3.6.1.4.1.9.2.1.58.0".to_owned(),
            value: 42.0,
        }]);
        let r = execute(&job, &t, 1_000).await;
        // The configured metric name is used, not the built-in OID-derived fallback.
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == "cpu_util" && s.value == 42.0));
        assert!(!r.samples.iter().any(|s| s.metric.starts_with("snmp_oid_")));
    }

    fn snmp_table_job() -> PollJob {
        use yagra_bus::{SnmpColumn, SnmpMetaColumn, SnmpTableCheck};
        use yagra_common::{InterfaceField, MetricKind};
        PollJob::snmp_table(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            SnmpTableCheck {
                community: "public".to_owned(),
                columns: vec![
                    SnmpColumn {
                        metric_name: "if_hc_in_octets".to_owned(),
                        oid: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                        kind: MetricKind::Counter,
                    },
                    SnmpColumn {
                        metric_name: "if_oper_status".to_owned(),
                        oid: "1.3.6.1.2.1.2.2.1.8".to_owned(),
                        kind: MetricKind::Gauge,
                    },
                ],
                meta_columns: vec![
                    SnmpMetaColumn {
                        field: InterfaceField::Name,
                        oid: "1.3.6.1.2.1.31.1.1.1.1".to_owned(),
                    },
                    SnmpMetaColumn {
                        field: InterfaceField::Speed,
                        oid: "1.3.6.1.2.1.2.2.1.5".to_owned(),
                    },
                ],
                timeout_ms: 2000,
            },
            60,
        )
    }

    #[tokio::test]
    async fn snmp_table_maps_columns_to_per_interface_samples() {
        use yagra_common::MetricKind;
        use yagra_transport::SnmpTableSample;
        let t = FakeTransport::reachable(0.0).with_snmp_table(vec![
            SnmpTableSample {
                oid_base: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                ifindex: 1,
                value: 1000.0,
            },
            SnmpTableSample {
                oid_base: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                ifindex: 2,
                value: 2000.0,
            },
            SnmpTableSample {
                oid_base: "1.3.6.1.2.1.2.2.1.8".to_owned(),
                ifindex: 1,
                value: 1.0,
            },
        ]);
        let r = execute(&snmp_table_job(), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Reachable);
        // The in-octets counter for ifIndex 2 is mapped by name, ifindex, and kind.
        let octets2 = r
            .samples
            .iter()
            .find(|s| s.metric == "if_hc_in_octets" && s.ifindex == Some(IfIndex(2)))
            .expect("if_hc_in_octets ifIndex 2 present");
        assert_eq!(octets2.value, 2000.0);
        assert_eq!(octets2.kind, MetricKind::Counter);
        // The oper-status gauge for ifIndex 1.
        assert!(r.samples.iter().any(|s| s.metric == "if_oper_status"
            && s.ifindex == Some(IfIndex(1))
            && s.kind == MetricKind::Gauge));
    }

    #[tokio::test]
    async fn snmp_table_metadata_folds_into_discovered_interfaces() {
        use yagra_transport::{SnmpTableSample, SnmpTableString};
        let t = FakeTransport::reachable(0.0)
            .with_snmp_table(vec![
                // One numeric sample so the poll counts as reachable.
                SnmpTableSample {
                    oid_base: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                    ifindex: 1,
                    value: 10.0,
                },
                // ifSpeed (numeric meta column) for ifIndex 1.
                SnmpTableSample {
                    oid_base: "1.3.6.1.2.1.2.2.1.5".to_owned(),
                    ifindex: 1,
                    value: 1_000_000_000.0,
                },
            ])
            .with_snmp_table_strings(vec![SnmpTableString {
                oid_base: "1.3.6.1.2.1.31.1.1.1.1".to_owned(),
                ifindex: 1,
                value: "Gi0/1".to_owned(),
            }]);
        let r = execute(&snmp_table_job(), &t, 1_000).await;
        assert_eq!(r.interfaces.len(), 1);
        let iface = &r.interfaces[0];
        assert_eq!(iface.ifindex, IfIndex(1));
        assert_eq!(iface.if_name.as_deref(), Some("Gi0/1"));
        assert_eq!(iface.if_speed, Some(1_000_000_000));
        // ifSpeed must NOT have leaked into the TSDB samples (it's metadata, not a metric).
        assert!(!r.samples.iter().any(|s| s.metric == "1.3.6.1.2.1.2.2.1.5"));
    }

    fn snmp_v3_table_job() -> PollJob {
        use yagra_bus::{SnmpColumn, SnmpMetaColumn, SnmpV3TableCheck};
        use yagra_common::{InterfaceField, MetricKind};
        PollJob::snmp_v3_table(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 3)),
            SnmpV3TableCheck {
                user: "monitor".to_owned(),
                security_level: "authpriv".to_owned(),
                auth_protocol: Some("sha256".to_owned()),
                auth_key: Some("auth-pass".to_owned()),
                priv_protocol: Some("aes256".to_owned()),
                priv_key: Some("priv-pass".to_owned()),
                columns: vec![SnmpColumn {
                    metric_name: "if_hc_in_octets".to_owned(),
                    oid: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                    kind: MetricKind::Counter,
                }],
                meta_columns: vec![SnmpMetaColumn {
                    field: InterfaceField::Name,
                    oid: "1.3.6.1.2.1.31.1.1.1.1".to_owned(),
                }],
                timeout_ms: 2000,
            },
            60,
        )
    }

    #[tokio::test]
    async fn snmp_v3_table_walks_columns_and_metadata_like_v2c() {
        use yagra_common::MetricKind;
        use yagra_transport::{SnmpTableSample, SnmpTableString};
        // The v3 table path drives the same walk/fold logic as v2c (shared `execute_table_walk`);
        // the fake returns its canned rows for the v3 walk too. This proves a v3 node now collects
        // per-interface metrics + interface metadata instead of being silently limited to scalars.
        let t = FakeTransport::reachable(0.0)
            .with_snmp_table(vec![SnmpTableSample {
                oid_base: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                ifindex: 7,
                value: 4242.0,
            }])
            .with_snmp_table_strings(vec![SnmpTableString {
                oid_base: "1.3.6.1.2.1.31.1.1.1.1".to_owned(),
                ifindex: 7,
                value: "Gi0/7".to_owned(),
            }]);
        let r = execute(&snmp_v3_table_job(), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Reachable);
        let octets = r
            .samples
            .iter()
            .find(|s| s.metric == "if_hc_in_octets" && s.ifindex == Some(IfIndex(7)))
            .expect("v3 table produced the per-interface counter");
        assert_eq!(octets.value, 4242.0);
        assert_eq!(octets.kind, MetricKind::Counter);
        // Interface metadata is folded from the v3 string walk (PostgreSQL inventory, ADR-011).
        assert_eq!(r.interfaces.len(), 1);
        assert_eq!(r.interfaces[0].ifindex, IfIndex(7));
        assert_eq!(r.interfaces[0].if_name.as_deref(), Some("Gi0/7"));
    }

    #[tokio::test]
    async fn snmp_table_ignores_out_of_range_if_speed() {
        use yagra_transport::{SnmpTableSample, SnmpTableString};
        let t = FakeTransport::reachable(0.0)
            .with_snmp_table(vec![
                // One numeric metric sample so the poll counts as reachable.
                SnmpTableSample {
                    oid_base: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                    ifindex: 1,
                    value: 10.0,
                },
                // A non-finite ifSpeed must be dropped, not silently saturated to i64::MAX.
                SnmpTableSample {
                    oid_base: "1.3.6.1.2.1.2.2.1.5".to_owned(),
                    ifindex: 1,
                    value: f64::INFINITY,
                },
            ])
            .with_snmp_table_strings(vec![SnmpTableString {
                oid_base: "1.3.6.1.2.1.31.1.1.1.1".to_owned(),
                ifindex: 1,
                value: "Gi0/1".to_owned(),
            }]);
        let r = execute(&snmp_table_job(), &t, 1_000).await;
        assert_eq!(r.interfaces.len(), 1);
        let iface = &r.interfaces[0];
        assert_eq!(iface.if_name.as_deref(), Some("Gi0/1"));
        assert_eq!(
            iface.if_speed, None,
            "out-of-range ifSpeed must be dropped rather than saturated"
        );
    }

    #[tokio::test]
    async fn snmp_table_no_values_is_unreachable() {
        let t = FakeTransport::reachable(0.0); // no canned table rows
        let r = execute(&snmp_table_job(), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Unreachable);
        assert!(r.samples.is_empty());
        assert!(r.interfaces.is_empty());
    }

    fn http_job(check: yagra_bus::HttpCheck) -> PollJob {
        PollJob::http(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            check,
            60,
        )
    }

    fn http_check(url: &str) -> yagra_bus::HttpCheck {
        yagra_bus::HttpCheck {
            url: url.to_owned(),
            method: yagra_common::HttpMethod::Get,
            expected_status: yagra_common::ExpectedStatus::TwoXx,
            verify_tls: true,
            follow_redirects: true,
            timeout_ms: 5000,
            auth: None,
            body_match: None,
            json_extract: Vec::new(),
            body_max_bytes: yagra_common::DEFAULT_BODY_MAX_BYTES,
        }
    }

    #[tokio::test]
    async fn http_up_when_reachable_and_status_matches() {
        use yagra_transport::HttpProbe;
        let t = FakeTransport::reachable(0.0).with_http(HttpProbe {
            reachable: true,
            status_code: Some(200),
            response_time_ms: 12.0,
            cert_days_to_expiry: Some(45.0),
            body: None,
        });
        let r = execute(&http_job(http_check("https://example.com/")), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Reachable);
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == METRIC_HTTP_UP && s.value == 1.0));
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == METRIC_HTTP_STATUS_CODE && s.value == 200.0));
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == METRIC_SSL_CERT_DAYS_TO_EXPIRY && s.value == 45.0));
        // The transport has always measured this; for a long time nothing turned it into a sample,
        // so a URL monitor could say whether the endpoint was up but never whether it was slow.
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == METRIC_HTTP_RESPONSE_TIME_MS && s.value == 12.0));
    }

    #[tokio::test]
    async fn http_down_when_status_unexpected() {
        use yagra_transport::HttpProbe;
        // Reachable, but a 500 doesn't match the default any-2xx expectation → http_up = 0.
        let t = FakeTransport::reachable(0.0).with_http(HttpProbe {
            reachable: true,
            status_code: Some(500),
            response_time_ms: 8.0,
            cert_days_to_expiry: None,
            body: None,
        });
        let r = execute(&http_job(http_check("https://example.com/")), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Reachable);
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == METRIC_HTTP_UP && s.value == 0.0));
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == METRIC_HTTP_STATUS_CODE && s.value == 500.0));
        // The endpoint answered — it just answered wrongly — so the response time is a real
        // measurement and must still be recorded. The gate below is on *reachability*, not on
        // `http_up`; conflating the two would blind the latency series for every monitor whose
        // expected-status check is failing, which is exactly when latency is worth seeing.
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == METRIC_HTTP_RESPONSE_TIME_MS && s.value == 8.0));
    }

    #[tokio::test]
    async fn http_down_and_unreachable_when_no_response() {
        use yagra_transport::HttpProbe;
        // A timeout: the transport still fills `response_time_ms` (elapsed until it gave up), so
        // this fake carries the realistic 5000 ms rather than the plain `unreachable()` fake's 0.0
        // — otherwise dropping the reachability gate would emit a 0.0 nobody would notice.
        let t = FakeTransport::unreachable().with_http(HttpProbe {
            reachable: false,
            status_code: None,
            response_time_ms: 5000.0,
            cert_days_to_expiry: None,
            body: None,
        });
        let r = execute(&http_job(http_check("https://example.com/")), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Unreachable);
        assert!(r
            .samples
            .iter()
            .any(|s| s.metric == METRIC_HTTP_UP && s.value == 0.0));
        assert!(!r
            .samples
            .iter()
            .any(|s| s.metric == METRIC_HTTP_STATUS_CODE));
        // No response, no response time. Emitting the timeout duration would draw a flat "slow
        // response" line for the whole outage and let a latency threshold page for the same
        // incident `http_up` already covers.
        assert!(
            !r.samples
                .iter()
                .any(|s| s.metric == METRIC_HTTP_RESPONSE_TIME_MS),
            "an unreachable endpoint must not report a response time"
        );
    }

    // ── URL body keyword matching (ADR-047 Increment 2) ─────────────────────────────

    /// A reachable 200 whose body is `text`, captured whole or cut short.
    fn http_probe_with_body(text: &str, truncated: bool) -> yagra_transport::HttpProbe {
        yagra_transport::HttpProbe {
            reachable: true,
            status_code: Some(200),
            response_time_ms: 12.0,
            cert_days_to_expiry: None,
            body: Some(yagra_transport::BodyCapture {
                text: text.to_owned(),
                truncated,
            }),
        }
    }

    fn http_check_matching(url: &str, rule: yagra_common::BodyMatch) -> yagra_bus::HttpCheck {
        let mut check = http_check(url);
        check.body_match = Some(rule);
        check
    }

    fn sample(r: &PollResult, metric: &str) -> Option<f64> {
        r.samples
            .iter()
            .find(|s| s.metric == metric)
            .map(|s| s.value)
    }

    #[tokio::test]
    async fn a_monitor_with_no_rule_asks_for_no_body_and_reports_no_match_gauge() {
        // The default for every URL monitor that existed before this increment. It must stay
        // byte-identical: no body read (so `http_response_time_ms` keeps meaning the same thing)
        // and no new series appearing on monitors nobody reconfigured.
        let t = FakeTransport::reachable(0.0).with_http(http_probe_with_body("anything", false));
        let r = execute(&http_job(http_check("https://example.com/")), &t, 1_000).await;
        assert_eq!(sample(&r, METRIC_HTTP_BODY_MATCH), None);
        assert_eq!(sample(&r, METRIC_HTTP_BODY_TRUNCATED), None);
    }

    #[tokio::test]
    async fn a_satisfied_rule_reports_one_and_an_untruncated_read() {
        let t = FakeTransport::reachable(0.0)
            .with_http(http_probe_with_body(r#"{"status":"ok"}"#, false));
        let rule = yagra_common::BodyMatch::contains(r#""status":"ok""#);
        let r = execute(
            &http_job(http_check_matching("https://example.com/health", rule)),
            &t,
            1_000,
        )
        .await;
        assert_eq!(sample(&r, METRIC_HTTP_BODY_MATCH), Some(1.0));
        assert_eq!(sample(&r, METRIC_HTTP_BODY_TRUNCATED), Some(0.0));
        // The content rule is its own gauge: a green body must not have altered availability, and
        // a failing one (below) must not silently redefine what `http_up` has always meant.
        assert_eq!(sample(&r, METRIC_HTTP_UP), Some(1.0));
    }

    #[tokio::test]
    async fn a_two_hundred_whose_body_says_it_is_broken_is_caught_without_touching_http_up() {
        // The whole point of the feature: the endpoint answers 200, so `http_up` is 1 and always
        // was — only the body says the service is down.
        let t = FakeTransport::reachable(0.0)
            .with_http(http_probe_with_body("<h1>Database unavailable</h1>", false));
        let rule = yagra_common::BodyMatch {
            pattern: "Database unavailable".to_owned(),
            mode: yagra_common::BodyMatchMode::NotContains,
        };
        let r = execute(
            &http_job(http_check_matching("https://example.com/", rule)),
            &t,
            1_000,
        )
        .await;
        assert_eq!(sample(&r, METRIC_HTTP_UP), Some(1.0));
        assert_eq!(sample(&r, METRIC_HTTP_BODY_MATCH), Some(0.0));
        assert_eq!(
            r.outcome,
            CheckOutcome::Reachable,
            "a content failure is not a liveness failure — the endpoint answered"
        );
    }

    #[tokio::test]
    async fn an_undecidable_rule_reports_zero_and_says_the_body_was_truncated() {
        // The budget ran out before the keyword appeared. Reporting 1 here would be the silent lie
        // ADR-047 決定 3 forbids; reporting 0 alerts, and the truncation gauge is what tells the
        // operator it was the budget rather than the keyword.
        let t = FakeTransport::reachable(0.0).with_http(http_probe_with_body("<html>…", true));
        let rule = yagra_common::BodyMatch::contains("healthy");
        let r = execute(
            &http_job(http_check_matching("https://example.com/", rule)),
            &t,
            1_000,
        )
        .await;
        assert_eq!(sample(&r, METRIC_HTTP_BODY_MATCH), Some(0.0));
        assert_eq!(sample(&r, METRIC_HTTP_BODY_TRUNCATED), Some(1.0));

        // And the direction that would otherwise go unnoticed: a `not_contains` rule must NOT read
        // as satisfied just because the forbidden text was past the cut.
        let quiet = yagra_common::BodyMatch {
            pattern: "unavailable".to_owned(),
            mode: yagra_common::BodyMatchMode::NotContains,
        };
        let r = execute(
            &http_job(http_check_matching("https://example.com/", quiet)),
            &t,
            1_000,
        )
        .await;
        assert_eq!(
            sample(&r, METRIC_HTTP_BODY_MATCH),
            Some(0.0),
            "a truncated body must never report a satisfied not_contains rule"
        );
    }

    #[tokio::test]
    async fn an_unreachable_endpoint_reports_no_body_verdict_at_all() {
        // There is no body to judge, and emitting 0 would double-page for the outage `http_up`
        // already covers — the same reasoning as the response-time gate above.
        let t = FakeTransport::unreachable().with_http(yagra_transport::HttpProbe {
            reachable: false,
            status_code: None,
            response_time_ms: 5000.0,
            cert_days_to_expiry: None,
            body: None,
        });
        let rule = yagra_common::BodyMatch::contains("ok");
        let r = execute(
            &http_job(http_check_matching("https://example.com/", rule)),
            &t,
            1_000,
        )
        .await;
        assert_eq!(sample(&r, METRIC_HTTP_UP), Some(0.0));
        assert_eq!(sample(&r, METRIC_HTTP_BODY_MATCH), None);
        assert_eq!(sample(&r, METRIC_HTTP_BODY_TRUNCATED), None);
    }

    // ── URL JSON extraction (ADR-047 Increment 3) ───────────────────────────────────

    fn http_check_extracting(url: &str, rules: &[(&str, &str)]) -> yagra_bus::HttpCheck {
        let mut check = http_check(url);
        check.json_extract = rules
            .iter()
            .map(|(metric, path)| yagra_common::JsonExtract {
                metric: (*metric).to_owned(),
                path: (*path).to_owned(),
            })
            .collect();
        check
    }

    const HEALTH_JSON: &str = r#"{"queue":{"depth":42,"lag_s":1.5},"healthy":true,"note":"fine"}"#;

    #[tokio::test]
    async fn every_rule_records_its_value_under_the_operators_own_metric_name() {
        let t = FakeTransport::reachable(0.0).with_http(http_probe_with_body(HEALTH_JSON, false));
        let check = http_check_extracting(
            "https://example.com/health",
            &[
                ("queue_depth", "queue.depth"),
                ("queue_lag_seconds", "queue.lag_s"),
                ("service_healthy", "healthy"),
            ],
        );
        let r = execute(&http_job(check), &t, 1_000).await;
        assert_eq!(sample(&r, "queue_depth"), Some(42.0));
        assert_eq!(sample(&r, "queue_lag_seconds"), Some(1.5));
        // A boolean health flag becomes a 0/1 gauge — which wants the same 0.5 threshold bound as
        // every other boolean here (migration 0030's trap).
        assert_eq!(sample(&r, "service_healthy"), Some(1.0));
        // Extraction is additive: it must not disturb what the monitor already reported.
        assert_eq!(sample(&r, METRIC_HTTP_UP), Some(1.0));
        assert_eq!(sample(&r, METRIC_HTTP_RESPONSE_TIME_MS), Some(12.0));
    }

    #[tokio::test]
    async fn a_rule_that_finds_nothing_records_nothing_rather_than_zero() {
        // The ADR-047 決定 3 rule, and the reason `extract` returns an Option: a `0` here would be
        // indistinguishable from the queue genuinely being empty, so a "queue is fine" dashboard
        // would be showing the absence of a reading.
        let t = FakeTransport::reachable(0.0).with_http(http_probe_with_body(HEALTH_JSON, false));
        let check = http_check_extracting(
            "https://example.com/health",
            &[
                ("missing_key", "queue.nope"),
                ("not_a_number", "note"),
                ("good", "queue.depth"),
            ],
        );
        let r = execute(&http_job(check), &t, 1_000).await;
        assert_eq!(sample(&r, "missing_key"), None);
        assert_eq!(sample(&r, "not_a_number"), None);
        // One failing rule must not suppress its siblings.
        assert_eq!(sample(&r, "good"), Some(42.0));
    }

    #[tokio::test]
    async fn a_body_that_is_not_json_records_no_extracted_metrics() {
        let t = FakeTransport::reachable(0.0)
            .with_http(http_probe_with_body("<html>not json</html>", false));
        let check =
            http_check_extracting("https://example.com/", &[("queue_depth", "queue.depth")]);
        let r = execute(&http_job(check), &t, 1_000).await;
        assert_eq!(sample(&r, "queue_depth"), None);
        // Availability is untouched: an HTML page is a perfectly reachable endpoint.
        assert_eq!(sample(&r, METRIC_HTTP_UP), Some(1.0));
    }

    #[tokio::test]
    async fn a_truncated_body_records_no_extracted_metrics() {
        // Half a JSON document does not parse, so this falls out of the parse rather than needing a
        // special case — but it is worth pinning, because the alternative (parsing a prefix
        // leniently) would record numbers from a document we never saw the end of.
        let cut = &HEALTH_JSON[..30];
        let t = FakeTransport::reachable(0.0).with_http(http_probe_with_body(cut, true));
        let check =
            http_check_extracting("https://example.com/", &[("queue_depth", "queue.depth")]);
        let r = execute(&http_job(check), &t, 1_000).await;
        assert_eq!(sample(&r, "queue_depth"), None);
    }

    #[tokio::test]
    async fn an_unreachable_endpoint_records_no_extracted_metrics() {
        let t = FakeTransport::unreachable().with_http(yagra_transport::HttpProbe {
            reachable: false,
            status_code: None,
            response_time_ms: 5000.0,
            cert_days_to_expiry: None,
            body: None,
        });
        let check =
            http_check_extracting("https://example.com/", &[("queue_depth", "queue.depth")]);
        let r = execute(&http_job(check), &t, 1_000).await;
        assert_eq!(sample(&r, "queue_depth"), None);
        assert_eq!(sample(&r, METRIC_HTTP_UP), Some(0.0));
    }

    #[tokio::test]
    async fn extraction_alone_still_opens_the_body() {
        // The budget moved off `BodyMatch` so that a monitor with only extraction has an answer to
        // "how much do I read". If that regressed, the transport would be asked for no body and
        // every rule would silently record nothing.
        let check = http_check_extracting("https://example.com/", &[("q", "queue.depth")]);
        assert!(check.body_match.is_none());
        assert_eq!(
            check.body_capture_bytes(),
            Some(yagra_common::DEFAULT_BODY_MAX_BYTES)
        );
        assert_eq!(
            http_check("https://example.com/").body_capture_bytes(),
            None,
            "neither feature ⇒ the transport never reads the body"
        );
    }

    // ── DNS name-resolution monitoring (ADR-033) ────────────────────────────────────

    fn dns_job(check: yagra_bus::DnsCheck) -> PollJob {
        PollJob::dns(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            check,
            60,
        )
    }

    fn dns_check(name: &str) -> yagra_bus::DnsCheck {
        yagra_bus::DnsCheck {
            name: name.to_owned(),
            record_type: yagra_common::DnsRecordType::A,
            resolver: None,
            resolver_port: 53,
            max_depth: 8,
            timeout_ms: 3000,
        }
    }

    /// `horryworks.net → CNAME horry.net → A 10.1.2.3`, the shape from the feature request.
    fn resolved_chain() -> yagra_common::DnsChain {
        yagra_common::DnsChain {
            query: "horryworks.net".into(),
            record_type: yagra_common::DnsRecordType::A,
            resolver: "10.0.0.53:53".into(),
            hops: vec![
                yagra_common::DnsHop {
                    name: "horryworks.net".into(),
                    answers: vec![yagra_common::DnsAnswer {
                        record: yagra_common::DnsRecord::Cname {
                            target: "horry.net".into(),
                        },
                        ttl: 300,
                    }],
                },
                yagra_common::DnsHop {
                    name: "horry.net".into(),
                    answers: vec![yagra_common::DnsAnswer {
                        record: yagra_common::DnsRecord::A {
                            addr: "10.1.2.3".parse().unwrap(),
                        },
                        ttl: 60,
                    }],
                },
            ],
            failure: None,
            resolve_ms: 14.0,
        }
    }

    fn failed_chain(failure: yagra_common::DnsFailure) -> yagra_common::DnsChain {
        yagra_common::DnsChain {
            hops: Vec::new(),
            failure: Some(failure),
            ..resolved_chain()
        }
    }

    fn sample_value(r: &PollResult, metric: &str) -> Option<f64> {
        r.samples
            .iter()
            .find(|s| s.metric == metric)
            .map(|s| s.value)
    }

    #[tokio::test]
    async fn dns_resolved_chain_emits_up_one_and_attaches_the_chain() {
        let t = FakeTransport::reachable(0.0).with_dns(resolved_chain());
        let r = execute(&dns_job(dns_check("horryworks.net")), &t, 1_000).await;

        assert_eq!(r.outcome, CheckOutcome::Reachable);
        assert_eq!(sample_value(&r, METRIC_DNS_UP), Some(1.0));
        assert_eq!(sample_value(&r, METRIC_DNS_CHAIN_LENGTH), Some(2.0));
        assert_eq!(sample_value(&r, METRIC_DNS_ANSWER_COUNT), Some(1.0));
        assert_eq!(sample_value(&r, METRIC_DNS_RESOLVE_MS), Some(14.0));

        // The chain rides the result so core can persist it; it is NOT expressed as metrics.
        let chain = r.dns_chain.expect("the chain must travel with the result");
        assert_eq!(chain.hops.len(), 2);
        assert_eq!(chain.hops[1].name, "horry.net");
    }

    #[tokio::test]
    async fn dns_nxdomain_emits_up_zero_but_stays_reachable() {
        // The resolver answered — it just said "no such name". Node state stays Ok and the
        // dns_up threshold is what fires, exactly like a reachable URL returning HTTP 500.
        let t = FakeTransport::reachable(0.0)
            .with_dns(failed_chain(yagra_common::DnsFailure::NxDomain));
        let r = execute(&dns_job(dns_check("nope.example")), &t, 1_000).await;

        assert_eq!(r.outcome, CheckOutcome::Reachable);
        assert_eq!(sample_value(&r, METRIC_DNS_UP), Some(0.0));
        assert_eq!(sample_value(&r, METRIC_DNS_ANSWER_COUNT), Some(0.0));
        assert!(r.dns_chain.is_some(), "a failed chain is still recorded");
    }

    #[tokio::test]
    async fn dns_timeout_emits_up_zero_and_unreachable() {
        // No answer at all — that, and only that, means the target is unreachable.
        let t =
            FakeTransport::reachable(0.0).with_dns(failed_chain(yagra_common::DnsFailure::Timeout));
        let r = execute(&dns_job(dns_check("horryworks.net")), &t, 1_000).await;

        assert_eq!(r.outcome, CheckOutcome::Unreachable);
        assert_eq!(sample_value(&r, METRIC_DNS_UP), Some(0.0));
    }

    #[tokio::test]
    async fn dns_servfail_and_refused_stay_reachable() {
        for failure in [
            yagra_common::DnsFailure::ServFail,
            yagra_common::DnsFailure::Refused,
            yagra_common::DnsFailure::NoData,
        ] {
            let t = FakeTransport::reachable(0.0).with_dns(failed_chain(failure.clone()));
            let r = execute(&dns_job(dns_check("horryworks.net")), &t, 1_000).await;
            assert_eq!(r.outcome, CheckOutcome::Reachable, "{failure:?}");
            assert_eq!(sample_value(&r, METRIC_DNS_UP), Some(0.0), "{failure:?}");
        }
    }

    #[tokio::test]
    async fn dns_samples_are_node_level_gauges_with_valid_metric_names() {
        // Thin-label model (ADR-011): a DNS chain must never become a series label, so every
        // sample it produces has to be a plain node-level gauge.
        let t = FakeTransport::reachable(0.0).with_dns(resolved_chain());
        let r = execute(&dns_job(dns_check("horryworks.net")), &t, 1_000).await;
        assert_eq!(r.samples.len(), 4);
        for s in &r.samples {
            assert!(s.ifindex.is_none(), "{} must be node-level", s.metric);
            assert_eq!(s.kind, MetricKind::Gauge, "{} must be a gauge", s.metric);
            assert!(
                yagra_common::is_valid_metric_name(&s.metric),
                "{} must be TSDB-safe",
                s.metric
            );
        }
    }

    /// Walking skeleton: a job published to the bus flows through the poll loop and a
    /// result with samples comes back on the bus — the core⇄poller seam, end to end.
    #[tokio::test]
    async fn job_flows_through_loop_to_result_on_bus() {
        use tokio_stream::wrappers::BroadcastStream;

        let bus = Arc::new(InMemoryBus::new(16));
        let jobs_rx = bus.subscribe_jobs();
        let mut results_rx = bus.subscribe_results();
        let transport: Arc<dyn Transport> = Arc::new(FakeTransport::reachable(5.0));

        // Adapt the broadcast receiver into the generic job stream the loop consumes.
        let jobs = Box::pin(BroadcastStream::new(jobs_rx).filter_map(|r| async move { r.ok() }));
        let limiter = Arc::new(PollLimiter::new(16));
        let results_total = Arc::new(AtomicU64::new(0));
        let inflight = Arc::new(AtomicU64::new(0));
        tokio::spawn(run_stream(
            jobs,
            crate::store_forward::StoreForwardSink::passthrough(bus.clone()),
            transport,
            limiter,
            None, // single-process skeleton: no poller id to stamp
            results_total,
            inflight,
        ));

        // Simulate core dispatching a job.
        bus.publish_job(icmp_job()).await.unwrap();

        let result = results_rx.recv().await.unwrap();
        assert_eq!(result.job_id, Uuid::nil());
        assert_eq!(result.outcome, CheckOutcome::Reachable);
        assert!(!result.samples.is_empty());
        assert!(result.poller_id.is_none(), "None poller id leaves it unset");
    }

    /// Distributed-poller walking skeleton (ADR-009/020): a spec lands in a [`WorkingSet`] via a
    /// single-chunk snapshot, `due()` mints a job, and it flows through the *same* `run_stream` to a
    /// [`PollResult`] on the bus — stamped with the producing poller's id.
    #[tokio::test]
    async fn snapshot_due_job_flows_through_run_stream_with_poller_id() {
        use crate::working_set::{ApplyOutcome, WorkingSet};
        use std::time::Instant;
        use yagra_bus::{NodeJobs, SyncMsg, WorkingSetSnapshot};

        let bus = Arc::new(InMemoryBus::new(16));
        let mut results_rx = bus.subscribe_results();
        let transport: Arc<dyn Transport> = Arc::new(FakeTransport::reachable(5.0));

        // Build a one-node, one-ICMP-spec working set from a single-chunk snapshot.
        let node = NodeId::from(Uuid::nil());
        let snap = SyncMsg::SnapshotChunk(WorkingSetSnapshot {
            poller_id: "edge-1".into(),
            epoch: Uuid::from_u128(1),
            seq: 1,
            chunk_index: 0,
            chunk_total: 1,
            nodes: vec![NodeJobs {
                node_id: node,
                specs: vec![yagra_bus::JobSpec::from_job(&icmp_job())],
            }],
            total_nodes: 1,
        });
        let mut ws = WorkingSet::new();
        let now = Instant::now();
        let mut rng = |_bound: u32| 0u32; // no jitter → due at `now`
        assert_eq!(ws.apply(snap, now, &mut rng), ApplyOutcome::Applied);
        let jobs: Vec<PollJob> = ws.due(now);
        assert_eq!(jobs.len(), 1, "the single spec is due");

        let limiter = Arc::new(PollLimiter::new(16));
        let results_total = Arc::new(AtomicU64::new(0));
        let inflight = Arc::new(AtomicU64::new(0));
        let poller_id: Arc<str> = Arc::from("edge-1");
        let job_stream = Box::pin(futures::stream::iter(jobs));
        tokio::spawn(run_stream(
            job_stream,
            crate::store_forward::StoreForwardSink::passthrough(bus.clone()),
            transport,
            limiter,
            Some(poller_id),
            results_total.clone(),
            inflight,
        ));

        let result = results_rx.recv().await.unwrap();
        assert_eq!(result.outcome, CheckOutcome::Reachable);
        assert_eq!(result.poller_id.as_deref(), Some("edge-1"));
        assert_eq!(result.node_id, node);
    }

    #[test]
    fn resolve_if_speed_prefers_ifspeed_below_cap() {
        // A 1 Gbps link: ifSpeed is exact and below the 32-bit cap, so it wins.
        assert_eq!(
            resolve_if_speed(Some(1_000_000_000.0), Some(1000.0)),
            Some(1_000_000_000)
        );
    }

    #[test]
    fn resolve_if_speed_uses_high_speed_when_saturated() {
        // 10 Gbps: ifSpeed saturates at u32::MAX, ifHighSpeed (10000 Mbps) gives the true rate.
        assert_eq!(
            resolve_if_speed(Some(u32::MAX as f64), Some(10_000.0)),
            Some(10_000_000_000)
        );
        // 100 Gbps with ifSpeed absent entirely → ifHighSpeed (100000 Mbps).
        assert_eq!(
            resolve_if_speed(None, Some(100_000.0)),
            Some(100_000_000_000)
        );
    }

    #[test]
    fn resolve_if_speed_keeps_sub_mbps_precision() {
        // A 64 kbps link: ifHighSpeed rounds to 0, so the exact ifSpeed must be kept.
        assert_eq!(resolve_if_speed(Some(64_000.0), Some(0.0)), Some(64_000));
    }

    #[test]
    fn resolve_if_speed_drops_invalid_and_handles_absence() {
        assert_eq!(resolve_if_speed(None, None), None);
        // Non-finite / negative ifSpeed is dropped; falls back to ifHighSpeed when present.
        assert_eq!(resolve_if_speed(Some(f64::INFINITY), None), None);
        assert_eq!(resolve_if_speed(Some(-1.0), None), None);
        assert_eq!(
            resolve_if_speed(Some(f64::NAN), Some(40_000.0)),
            Some(40_000_000_000)
        );
        // ifSpeed reporting the saturated cap with no ifHighSpeed → best-effort, keep the cap.
        assert_eq!(
            resolve_if_speed(Some(u32::MAX as f64), None),
            Some(u32::MAX as i64)
        );
    }

    /// ifHighSpeed (walked poller-side, not a bus column) overrides a saturated ifSpeed so a
    /// 10 Gbps interface stores its true rate, not the 32-bit cap.
    #[tokio::test]
    async fn snmp_table_high_speed_overrides_saturated_if_speed() {
        use yagra_transport::SnmpTableSample;
        let t = FakeTransport::reachable(0.0).with_snmp_table(vec![
            // One numeric metric sample so the poll counts as reachable.
            SnmpTableSample {
                oid_base: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                ifindex: 1,
                value: 10.0,
            },
            // ifSpeed saturated at the 32-bit cap.
            SnmpTableSample {
                oid_base: "1.3.6.1.2.1.2.2.1.5".to_owned(),
                ifindex: 1,
                value: u32::MAX as f64,
            },
            // ifHighSpeed = 10000 Mbps (walked from OID_IF_HIGH_SPEED).
            SnmpTableSample {
                oid_base: OID_IF_HIGH_SPEED.to_owned(),
                ifindex: 1,
                value: 10_000.0,
            },
        ]);
        let r = execute(&snmp_table_job(), &t, 1_000).await;
        let iface = r
            .interfaces
            .iter()
            .find(|i| i.ifindex == IfIndex(1))
            .expect("ifIndex 1 discovered");
        assert_eq!(iface.if_speed, Some(10_000_000_000));
    }
}
