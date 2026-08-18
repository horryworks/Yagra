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
    copper_designation, duplex_from_dot3, duplex_from_huawei, if_type_from_snmp,
    medium_from_huawei, DnsFailure, IfIndex, InterfaceField, Medium, MetricKind, NodeId,
    METRIC_DNS_ANSWER_COUNT, METRIC_DNS_CHAIN_LENGTH, METRIC_DNS_RESOLVE_MS, METRIC_DNS_UP,
    METRIC_HTTP_BODY_MATCH, METRIC_HTTP_BODY_TRUNCATED, METRIC_HTTP_RESPONSE_TIME_MS,
    METRIC_HTTP_STATUS_CODE, METRIC_HTTP_UP, METRIC_ICMP_RTT_MS, METRIC_SSL_CERT_DAYS_TO_EXPIRY,
    OID_DOT3_DUPLEX_STATUS, OID_HW_ETHERNET_DUPLEX, OID_HW_ETHERNET_PORT_TYPE, OID_IF_HIGH_SPEED,
    OID_IF_TYPE,
};
use yagra_transport::{
    DnsProbeSpec, HttpProbeSpec, MerakiCollectSpec, SnmpTableSample, SnmpTableString, SnmpV3Params,
    Transport, TransportError,
};

/// sysDescr.0 — system description scalar (the v3 GET form).
/// Ceiling on how long one job waits for another probe against the same device (see
/// `single_flight_wait`). A device's specs are serialised, so this bounds the tail of that chain;
/// 60s comfortably covers the slowest walk measured (6.0s against a 232-interface switch) while
/// keeping a long-interval check from parking for hours behind a wedged device.
const MAX_SINGLE_FLIGHT_WAIT: Duration = Duration::from_secs(60);

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
            let walker = SnmpWalker::V3(yagra_transport::SnmpV3Params {
                user: check.user.clone(),
                security_level: check.security_level.clone(),
                auth_protocol: check.auth_protocol.clone(),
                auth_key: check.auth_key.clone(),
                priv_protocol: check.priv_protocol.clone(),
                priv_key: check.priv_key.clone(),
            });
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
                // The Meraki API reports no link mode either — these come from EtherLike-MIB.
                if_duplex: None,
                if_type: None,
                if_media: None,
                transceiver_model: None,
                // Meraki reports no transceiver diagnostics; the optical probe is SNMP-only.
                rx_power_low_dbm: None,
                rx_power_high_dbm: None,
                tx_power_low_dbm: None,
                tx_power_high_dbm: None,
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
        // The link's negotiated mode rides along for free (ADR-063 Inc.1): both are INTEGER-valued
        // and indexed by ifIndex alone, so they cost extra GETBULK sequences on a session this poll
        // was opening anyway — not a session, which is what S5 was about. Gated on the same
        // condition as ifHighSpeed because it marks "this job is gathering interface metadata".
        //
        // ⚠️ They are appended here rather than declared as `InterfaceField` variants on purpose:
        // a new variant would make every N-1 poller drop the entire SnmpTable spec. The reasoning
        // is on `yagra_common::link_mode`, next to the constants.
        numeric_oids.push(OID_DOT3_DUPLEX_STATUS.to_owned());
        // Huawei YunShan implements neither EtherLike-MIB nor MAU-MIB, so both of ADR-063's
        // existing paths are dead there and the duplex column was permanently blank. This is
        // one more column on a walk already being issued, and it returns no rows on the
        // devices that answer the standard one. ⚠️ Its enumeration is NOT the standard one —
        // see `duplex_from_huawei`, which is why the fold below cannot share a mapper.
        numeric_oids.push(OID_HW_ETHERNET_DUPLEX.to_owned());
        // Whether the port is metal or optical (ADR-063 Inc.4), two columns from the duplex one in
        // the same Huawei table. The standard answer — `ifMauType` — is walked by the hourly media
        // job and is dead on this platform (`1.3.6.1.2.1.26` answers No Such Object), and the other
        // implemented source only ever names a pluggable, so a fixed RJ45 port had no source at all.
        numeric_oids.push(OID_HW_ETHERNET_PORT_TYPE.to_owned());
        numeric_oids.push(OID_IF_TYPE.to_owned());
    }

    let mut samples = Vec::new();
    let mut raw = RawInterfaceNumerics::default();
    match walker
        .walk(transport, job.target, &numeric_oids, timeout)
        .await
    {
        Ok(rows) => {
            for row in rows {
                // 🚨 `ifHighSpeed` is TWO things at once and must feed both.
                //
                // It is a declared metric column in the built-in interface template
                // (`if_high_speed`, a gauge) *and* the 64-bit source `resolve_if_speed` needs. It
                // used to sit in the `else if` chain below, where the metric arm matched first and
                // this insert was **unreachable on every node whose profile carries the standard
                // interface template** — i.e. every SNMP node. The visible effect was a permanently
                // empty speed column wherever `ifSpeed` could not answer: a device that reports only
                // ifXTable (measured: 19 of 21 lab devices) got nothing at all, and a real 10G+ port
                // whose 32-bit `ifSpeed` saturates got nothing either, because decision 7 refuses the
                // sentinel and then had no fallback left to reach.
                //
                // Hoisted out of the chain rather than reordered: the metric sample is still owed,
                // so this is an `and`, not an `or`. `the_high_speed_column_is_both_a_metric_and_the_
                // speed_source` pins the overlap so a catalog edit cannot quietly make this dead code.
                if row.oid_base == OID_IF_HIGH_SPEED {
                    raw.high.insert(row.ifindex, row.value);
                }
                if let Some(col) = by_base.get(row.oid_base.as_str()) {
                    samples.push(Sample::interface(
                        col.metric_name.clone(),
                        IfIndex(row.ifindex),
                        row.value,
                        col.kind,
                    ));
                } else if row.oid_base == OID_IF_HIGH_SPEED {
                    // Already captured above; kept as an explicit no-op arm so the chain still
                    // enumerates every OID this walk appends and nothing falls through to
                    // `speed_oids` by accident.
                } else if row.oid_base == OID_DOT3_DUPLEX_STATUS {
                    raw.duplex.insert(row.ifindex, row.value);
                } else if row.oid_base == OID_HW_ETHERNET_DUPLEX {
                    raw.hw_duplex.insert(row.ifindex, row.value);
                } else if row.oid_base == OID_HW_ETHERNET_PORT_TYPE {
                    raw.hw_port_type.insert(row.ifindex, row.value);
                } else if row.oid_base == OID_IF_TYPE {
                    raw.if_type.insert(row.ifindex, row.value);
                } else if speed_oids.iter().any(|o| o == &row.oid_base) {
                    raw.speed.insert(row.ifindex, row.value);
                }
            }
        }
        Err(err) => tracing::warn!(job_id = %job.job_id, error = %err, "snmp table walk failed"),
    }

    let interfaces =
        walk_interface_metadata(job, transport, walker, meta_columns, &raw, timeout).await;

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
    // Keyed by the *resolved* ifIndex, so an interface seen through two dialects merges into one
    // row rather than giving core two to upsert in an arbitrary order.
    let mut windows: BTreeMap<u32, optical::OpticalWindow> = BTreeMap::new();
    // Built lazily and at most once per poll: both dialects that need it walk the same two
    // ENTITY-MIB columns, and a node bound to two vendor profiles must not walk them twice.
    let mut entity: Option<optical::EntityIndex> = None;

    for probe in probes {
        if probe.rx_metric.is_none() && probe.tx_metric.is_none() && probe.temp_metric.is_none() {
            continue;
        }
        let (readings, raw_windows, temps) = match optical::simple_dialect(probe.flavor) {
            Some(dialect) => {
                let (r, w) = walk_simple_optical(job, transport, walker, timeout, &dialect).await;
                (r, w, Vec::new())
            }
            // The correlated dialects have no threshold objects at all — RFC 3433 defines none,
            // and Cisco's live in a separate table with a different row shape. So they draw their
            // two lines and no band, which is a stated degradation rather than a missing case.
            None => match optical::sensor_dialect(probe.flavor) {
                Some(dialect) => {
                    let (r, t) = walk_entity_sensor_optical(
                        job,
                        transport,
                        walker,
                        timeout,
                        &dialect,
                        probe.temp_metric.is_some(),
                    )
                    .await;
                    (r, HashMap::new(), t)
                }
                // Unreachable today — every flavour is served by one of the two tables, and
                // `every_flavor_is_served_by_exactly_one_dialect_kind` pins that. Skipping rather
                // than panicking is what an older poller meeting a newer dialect must do anyway.
                None => continue,
            },
        };
        if readings.is_empty() && temps.is_empty() {
            continue;
        }

        // Translate entPhysicalIndex → ifIndex for the dialects that need it. A row that does not
        // translate is DROPPED: emitting it under its raw entity index would land the series on
        // `MetricDimension::Entity`, which costs storage and appears on no chart (decision 3).
        // The thresholds travel through exactly the same translation — they are keyed by the same
        // row, so a window that survives while its reading did not would be a band with no line.
        // Both the readings and the temperatures are keyed by entPhysicalIndex and need the same
        // index — the readings to *find* their port, the temperatures to prove they have none.
        if !probe.flavor.is_ifindex_keyed() && entity.is_none() {
            entity = Some(walk_entity_index(job, transport, walker, timeout).await);
        }

        // Chassis temperature (ADR-070 decision 2). The rows kept here are the ones that reach no
        // interface: an SFP's own sensors climb ENTITY-MIB to their port (measured 42 of 42), a
        // chassis sensor dead-ends. That is the whole of the SFP/chassis split — no free-text rule
        // is added, and ADR-062 Issue #66's exclusion of a module's own temperature holds by
        // construction rather than by a list of strings.
        if let (Some(metric), Some(idx)) = (probe.temp_metric.as_ref(), entity.as_ref()) {
            let mut kept = 0usize;
            for (ent, celsius) in &temps {
                if idx.ifindex_for(*ent).is_some() {
                    continue;
                }
                kept += 1;
                samples.push(Sample::interface(
                    metric.clone(),
                    IfIndex(*ent),
                    *celsius,
                    MetricKind::Gauge,
                ));
            }
            if !temps.is_empty() {
                tracing::debug!(
                    job_id = %job.job_id,
                    flavor = ?probe.flavor,
                    kept,
                    port_attached = temps.len() - kept,
                    "chassis temperature sensors",
                );
            }
        }

        let (resolved, resolved_windows) = if probe.flavor.is_ifindex_keyed() {
            (readings, raw_windows)
        } else {
            let idx = entity
                .as_ref()
                .expect("built above for a non-ifindex-keyed dialect");
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
            let mapped_windows = raw_windows
                .into_iter()
                .filter_map(|(ent, w)| idx.ifindex_for(ent).map(|ifindex| (ifindex, w)))
                .collect();
            (mapped, mapped_windows)
        };
        windows.extend(resolved_windows);

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
        // ⚠️ These carry the thresholds and NOTHING else — no name, no alias, no speed, no link
        // mode. Core's interface upsert COALESCEs every column against its existing value, so the
        // `None`s preserve whatever the metadata walk stored rather than blanking it. That property
        // is what makes it safe to write the same row from two different probes, and it has a test.
        interfaces: windows
            .into_iter()
            .map(|(ifindex, w)| DiscoveredInterface {
                ifindex: IfIndex(ifindex),
                if_name: None,
                if_alias: None,
                if_speed: None,
                if_duplex: None,
                if_type: None,
                if_media: None,
                transceiver_model: None,
                rx_power_low_dbm: w.rx_low,
                rx_power_high_dbm: w.rx_high,
                tx_power_low_dbm: w.tx_low,
                tx_power_high_dbm: w.tx_high,
            })
            .collect(),
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

/// Walk a two-column optical dialect and scale it to dBm, together with the module's own
/// acceptable window when the dialect publishes one. Indices are whatever the dialect keys on;
/// the caller translates them if needed.
///
/// One walk for readings and thresholds together: they live in the same table, and the thresholds
/// are what turn a dBm figure into something an operator can act on, so splitting them would
/// double the SNMP sessions to draw one chart.
async fn walk_simple_optical(
    job: &PollJob,
    transport: &dyn Transport,
    walker: &SnmpWalker,
    timeout: Duration,
    dialect: &optical::SimpleDialect,
) -> (
    Vec<optical::OpticalSample>,
    HashMap<u32, optical::OpticalWindow>,
) {
    let mut columns = vec![dialect.rx_oid.to_owned(), dialect.tx_oid.to_owned()];
    if let Some(l) = dialect.limits {
        columns.extend([
            l.rx_low_oid.to_owned(),
            l.rx_high_oid.to_owned(),
            l.tx_low_oid.to_owned(),
            l.tx_high_oid.to_owned(),
        ]);
    }
    let rows = match walker.walk(transport, job.target, &columns, timeout).await {
        Ok(rows) => rows,
        Err(err) => {
            tracing::debug!(job_id = %job.job_id, error = %err, "optical walk failed");
            return (Vec::new(), HashMap::new());
        }
    };

    let mut samples = Vec::new();
    // Raw, unvalidated bounds per row key; validated in one pass below so a bad pair is refused
    // as a pair rather than half-kept.
    let mut raw: HashMap<u32, optical::OpticalWindow> = HashMap::new();
    for row in rows {
        let base = row.oid_base.as_str();
        // A port with no transceiver still answers, with the vendor's placeholder. Drop the row
        // here — before the scale turns the marker into an ordinary number — so a dark port is a
        // gap in the chart rather than a flat line at whatever the marker happens to scale to.
        // Applied to the bound columns too: they only escaped before because all four carrying
        // the same marker made `low == high`, which is an accident of the pair, not a guard.
        if dialect.no_module == Some(row.value) {
            continue;
        }
        let scaled = row.value * dialect.scale;
        if base == dialect.rx_oid || base == dialect.tx_oid {
            samples.push(optical::OpticalSample {
                ifindex: row.ifindex,
                reading: if base == dialect.rx_oid {
                    optical::OpticalReading::Rx
                } else {
                    optical::OpticalReading::Tx
                },
                dbm: scaled,
            });
            continue;
        }
        let Some(l) = dialect.limits else { continue };
        let w = raw.entry(row.ifindex).or_default();
        if base == l.rx_low_oid {
            w.rx_low = Some(scaled);
        } else if base == l.rx_high_oid {
            w.rx_high = Some(scaled);
        } else if base == l.tx_low_oid {
            w.tx_low = Some(scaled);
        } else if base == l.tx_high_oid {
            w.tx_high = Some(scaled);
        }
    }

    let windows = raw
        .into_iter()
        .filter_map(|(ifindex, w)| {
            let (rx_low, rx_high) = optical::validated_window(w.rx_low, w.rx_high);
            let (tx_low, tx_high) = optical::validated_window(w.tx_low, w.tx_high);
            let out = optical::OpticalWindow {
                rx_low,
                rx_high,
                tx_low,
                tx_high,
            };
            (!out.is_empty()).then_some((ifindex, out))
        })
        .collect();
    (samples, windows)
}

/// Walk a correlated sensor table and pull two different things out of one pass: the optical
/// readings, and — when asked — the chassis temperatures.
///
/// Four numeric columns in one session, then the entity text in a second — the same two-session
/// shape the interface walk uses, and for the same reason: the numeric and string walkers are
/// separate transports.
///
/// The `dialect` argument is the whole of ADR-070 decision 1 on this side: Cisco does not implement
/// RFC 3433, but it implements the identical table at its own root, so the columns move and nothing
/// else does.
///
/// Returns `(optical readings, (entPhysicalIndex, °C) candidates)`. The temperatures are
/// **candidates** because "is this a chassis sensor or an SFP's own?" is answered by whether the
/// entity resolves to an interface, and that index belongs to the caller.
async fn walk_entity_sensor_optical(
    job: &PollJob,
    transport: &dyn Transport,
    walker: &SnmpWalker,
    timeout: Duration,
    dialect: &optical::SensorDialect,
    want_temperature: bool,
) -> (Vec<optical::OpticalSample>, Vec<(u32, f64)>) {
    let columns = vec![
        dialect.type_oid.to_owned(),
        dialect.scale_oid.to_owned(),
        dialect.precision_oid.to_owned(),
        dialect.value_oid.to_owned(),
    ];
    let rows = match walker.walk(transport, job.target, &columns, timeout).await {
        Ok(rows) => rows,
        Err(err) => {
            tracing::debug!(job_id = %job.job_id, error = %err, "entity-sensor walk failed");
            return (Vec::new(), Vec::new());
        }
    };
    let mut types: HashMap<u32, i64> = HashMap::new();
    let mut scales: HashMap<u32, i64> = HashMap::new();
    let mut precisions: HashMap<u32, i64> = HashMap::new();
    let mut values: HashMap<u32, i64> = HashMap::new();
    for row in rows {
        let v = row.value as i64;
        // Not a `match`: the arms are runtime values now, so the compiler cannot help here. The
        // `else` arm is the one that matters — a column this dialect did not ask for is skipped
        // rather than folded into whichever bucket happened to come last.
        let base = row.oid_base.as_str();
        let bucket = if base == dialect.type_oid {
            &mut types
        } else if base == dialect.scale_oid {
            &mut scales
        } else if base == dialect.precision_oid {
            &mut precisions
        } else if base == dialect.value_oid {
            &mut values
        } else {
            continue;
        };
        bucket.insert(row.ifindex, v);
    }
    if values.is_empty() {
        return (Vec::new(), Vec::new());
    }

    // units(9) and no decimals are the MIB's own defaults for an agent that omits either column.
    let scale_of = |ent: &u32| scales.get(ent).copied().unwrap_or(9);
    let precision_of = |ent: &u32| precisions.get(ent).copied().unwrap_or(0);

    // Chassis temperature, from the same rows (ADR-070 decision 2). Deliberately computed before
    // the text walk: a temperature needs no free text, so a device with nothing but chassis
    // sensors still reports them.
    let temps: Vec<(u32, f64)> = if want_temperature {
        let mut t: Vec<(u32, f64)> = values
            .iter()
            .filter_map(|(ent, value)| {
                let celsius = optical::entity_sensor_celsius(
                    *value,
                    *types.get(ent)?,
                    scale_of(ent),
                    precision_of(ent),
                )?;
                Some((*ent, celsius))
            })
            .collect();
        t.sort_unstable_by_key(|(ent, _)| *ent);
        t
    } else {
        Vec::new()
    };

    // Only now walk the text, and only for the entities that produced a candidate reading.
    let text = walk_entity_text(job, transport, walker, timeout).await;

    // Ascending entity order so "first lane wins" in `dedupe_readings` is deterministic.
    let mut ents: Vec<u32> = values.keys().copied().collect();
    ents.sort_unstable();
    let readings = ents
        .into_iter()
        .filter_map(|ent| {
            let dbm = optical::entity_sensor_dbm(
                *values.get(&ent)?,
                *types.get(&ent)?,
                scale_of(&ent),
                precision_of(&ent),
            )?;
            let reading = optical::reading_from_text(text.get(&ent)?)?;
            Some(optical::OpticalSample {
                ifindex: ent,
                reading,
                dbm,
            })
        })
        .collect();
    (readings, temps)
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

/// Row budget for the MAU walk. `ifMauTable` holds roughly one row per Ethernet port, so a few
/// thousand is generous for any single device; the bound exists so a pathological agent cannot turn
/// an hourly attribute read into an unbounded one. Stated rather than defaulted, like every other
/// `walk_instances` caller.
const MAU_MAX_ROWS: usize = 4096;

/// Execute a media-type walk (v2c or v3, selected by `walker`) — ADR-063 Inc.2.
///
/// Two sources, tried in order, and the order is the point:
///
/// 1. **`ifMauTable`**, which answers with a registry designation covering copper *and* optics. Its
///    `(ifIndex, ifMauIndex)` index is why this cannot ride the interface walk — the ordinary
///    walkers fold a multi-subid tail into a hash. The instance walker preserves it.
/// 2. **ENTITY-MIB**, only for ports MAU did not answer and only when `entity_fallback` is on. It
///    returns a vendor part string, which is a *different fact*: it is stored as
///    `transceiver_model` and only promotes to `if_media` when it demonstrably contains a
///    designation. It reaches pluggables alone — a fixed copper port has no entity to describe, so
///    a device with no MAU-MIB gets nothing for its RJ45 ports and that is honest.
///
/// The result is **observational**, like the optical and neighbour walks: most devices do not
/// implement MAU-MIB, and a silent one is not an unreachable one. Reporting otherwise would page
/// someone about a healthy box.
///
/// Every field except the media pair is `None` on the way out. Core's interface upsert COALESCEs
/// each column, so these rows fill their own columns and leave the name, alias, speed, duplex and
/// optical window exactly as the other probes wrote them.
async fn execute_mau(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    entity_fallback: bool,
    timeout: Duration,
    walker: &SnmpWalker,
) -> PollResult {
    let columns = vec![yagra_common::OID_IF_MAU_TYPE.to_owned()];
    let (mut media, unknown) = match walker
        .walk_instances(transport, job.target, &columns, timeout, MAU_MAX_ROWS)
        .await
    {
        Ok(rows) => crate::mau::media_by_ifindex(&rows),
        Err(err) => {
            tracing::debug!(job_id = %job.job_id, error = %err, "ifMauTable walk failed");
            (BTreeMap::new(), Vec::new())
        }
    };
    if !unknown.is_empty() {
        // A *number*, not an OID string — it is what someone extending `MAU_TYPES` needs, and the
        // only way a gap in a hand-transcribed registry becomes visible from a running deployment.
        tracing::debug!(
            job_id = %job.job_id,
            subids = ?unknown,
            "ifMauType registrations not in the transcribed table; media left unknown",
        );
    }

    if entity_fallback {
        let text = walk_entity_media_text(job, transport, walker, timeout).await;
        if !text.is_empty() {
            let index = walk_entity_index(job, transport, walker, timeout).await;
            crate::mau::merge_entity_fallback(&mut media, &text, |ent| index.ifindex_for(ent));
        }
    }

    let interfaces = media
        .into_iter()
        .map(|(ifindex, row)| DiscoveredInterface {
            ifindex: IfIndex(ifindex),
            if_name: None,
            if_alias: None,
            if_speed: None,
            // MAU's duplex is secondary: `dot3StatsDuplexStatus` runs on the fast path and wins by
            // arriving first, because the upsert COALESCEs rather than overwrites. This fills the
            // column only on a device that implements MAU-MIB but not EtherLike-MIB.
            if_duplex: row.duplex,
            if_type: None,
            if_media: row.media,
            transceiver_model: row.transceiver_model,
            rx_power_low_dbm: None,
            rx_power_high_dbm: None,
            tx_power_low_dbm: None,
            tx_power_high_dbm: None,
        })
        .collect();

    PollResult {
        job_id: job.job_id,
        node_id: job.node_id,
        at_unix_ms,
        outcome: CheckOutcome::Reachable,
        samples: Vec::new(),
        interfaces,
        sys_descr: None,
        dns_chain: None,
        neighbors: None,
        l3: None,
        arp: None,
        routing: None,
        poller_id: None,
        // Never a liveness statement — see the doc comment.
        observational: true,
        trace_context: Default::default(),
    }
}

/// Walk the ENTITY-MIB text columns that can name a pluggable.
///
/// Two describing columns because which one carries a designation varies by vendor, plus
/// `entPhysicalName` — which is **not** a candidate but the yardstick: `mau::entity_text` throws
/// away any description that merely restates the component's own name. Without that third column
/// this walk reported every port as its own transceiver (see that function's 🚨).
async fn walk_entity_media_text(
    job: &PollJob,
    transport: &dyn Transport,
    walker: &SnmpWalker,
    timeout: Duration,
) -> BTreeMap<u32, String> {
    let columns = vec![
        ENT_PHYSICAL_MODEL_NAME.to_owned(),
        optical::ENT_PHYSICAL_DESCR.to_owned(),
        optical::ENT_PHYSICAL_NAME.to_owned(),
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
        Ok(rows) => crate::mau::entity_text(&rows, optical::ENT_PHYSICAL_NAME),
        Err(err) => {
            tracing::debug!(job_id = %job.job_id, error = %err, "entity media text walk failed");
            BTreeMap::new()
        }
    }
}

/// `entPhysicalModelName` — ENTITY-MIB's vendor part number for a physical component. The column
/// `optical.rs` had no use for, since a part number says nothing about a light level.
const ENT_PHYSICAL_MODEL_NAME: &str = "1.3.6.1.2.1.47.1.1.1.1.13";

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

/// The per-ifIndex numeric readings the caller's single combined walk demuxed out, for the metadata
/// fold below.
///
/// A struct rather than four `&HashMap` parameters: clippy called it at nine arguments, and it was
/// right for the usual reason — four maps of the same type in a row are four chances to pass duplex
/// where ifType belongs, with nothing to catch it. Every field is raw as the agent reported it;
/// interpretation (`resolve_if_speed`, `duplex_from_dot3`, `if_type_from_snmp`) happens in the fold.
#[derive(Debug, Default)]
struct RawInterfaceNumerics {
    /// `ifSpeed` (32-bit, bits/sec) — saturates at ~4.29 Gbps, hence `high`.
    speed: HashMap<u32, f64>,
    /// `ifHighSpeed` (units of 1,000,000 bits/sec).
    high: HashMap<u32, f64>,
    /// `dot3StatsDuplexStatus` (`unknown(1)` / `halfDuplex(2)` / `fullDuplex(3)`).
    duplex: HashMap<u32, f64>,
    /// `hwEthernetDuplex` (`full(1)` / `half(2)`) — the Huawei fallback. **Kept in its own map
    /// rather than merged into `duplex`**: the two columns disagree on what `1` means, so a
    /// merged map would need to remember which column each row came from anyway.
    hw_duplex: HashMap<u32, f64>,
    /// `hwEthernetPortType` (`other(1)` / `copper(2)` / `fiber(3)`) — the only medium source any
    /// device in this lab supplies. ⚠️ Its own map for the same reason `hw_duplex` has one: it
    /// overlaps the duplex enumeration on every value and agrees with it on none.
    hw_port_type: HashMap<u32, f64>,
    /// `ifType` (IANAifType).
    if_type: HashMap<u32, f64>,
}

/// Fold interface metadata into [`DiscoveredInterface`]s: walk the `ifName`/`ifAlias` **string**
/// columns (the poll's second and only other SNMP session), and resolve `if_speed` from the
/// `ifSpeed`/`ifHighSpeed` values already gathered by the combined numeric walk in the caller (S5).
async fn walk_interface_metadata(
    job: &PollJob,
    transport: &dyn Transport,
    walker: &SnmpWalker,
    meta_columns: &[SnmpMetaColumn],
    raw: &RawInterfaceNumerics,
    timeout: Duration,
) -> Vec<DiscoveredInterface> {
    let RawInterfaceNumerics {
        speed: raw_speed,
        high: raw_high,
        duplex: raw_duplex,
        hw_duplex: raw_hw_duplex,
        hw_port_type: raw_hw_port_type,
        if_type: raw_iftype,
    } = raw;
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
        if_duplex: None,
        if_type: None,
        if_media: None,
        transceiver_model: None,
        // The metadata walk never reads thresholds — they come from the optical probe, and core's
        // upsert COALESCEs, so leaving them None here preserves whatever that probe stored.
        rx_power_low_dbm: None,
        rx_power_high_dbm: None,
        tx_power_low_dbm: None,
        tx_power_high_dbm: None,
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

    // Duplex and ifType, from the same numeric walk (ADR-063 Inc.1). Folded over their own union of
    // ifindexes rather than the speed one: a device may answer EtherLike-MIB for a port whose
    // ifSpeed it does not report, and vice versa.
    for ifindex in raw_duplex
        .keys()
        .chain(raw_hw_duplex.keys())
        .chain(raw_iftype.keys())
        .copied()
        .collect::<BTreeSet<u32>>()
    {
        // EtherLike wins when present: it is the standard, and a device answering both should
        // not have its duplex decided by which vendor MIB the poller happened to read second.
        let duplex = raw_duplex
            .get(&ifindex)
            .copied()
            .and_then(duplex_from_dot3)
            .or_else(|| {
                raw_hw_duplex
                    .get(&ifindex)
                    .copied()
                    .and_then(duplex_from_huawei)
            });
        let if_type = raw_iftype
            .get(&ifindex)
            .copied()
            .and_then(if_type_from_snmp);
        if duplex.is_none() && if_type.is_none() {
            // Nothing usable — do not materialise a row for an interface the metric walk never
            // saw either, or a device answering `unknown(1)` for every port would inflate the
            // inventory with index-only records.
            continue;
        }
        let rec = ifs.entry(ifindex).or_insert_with(|| blank(ifindex));
        rec.if_duplex = duplex;
        rec.if_type = if_type;
    }

    // Media, for the devices that name the medium (ADR-063 Inc.4). Last of the three folds because
    // a designation is medium × speed and neither half answers alone — this reads the `if_speed` the
    // first fold resolved.
    //
    // Only interfaces the walk already produced a record for are touched (`get_mut`, never
    // `or_insert`): a port with a medium but no speed has nothing to say, and materialising an
    // index-only row for it would put a blank line in the inventory.
    for (ifindex, reading) in raw_hw_port_type {
        // ⚠️ Fibre is read and deliberately dropped — `copper_designation` carries the reason
        // (two writers, one COALESCEd column, and only copper's designation is unique per speed).
        if !matches!(medium_from_huawei(*reading), Some(Medium::Copper)) {
            continue;
        }
        let Some(rec) = ifs.get_mut(ifindex) else {
            continue;
        };
        let Some(bps) = rec.if_speed else {
            continue;
        };
        if let Some(designation) = copper_designation(bps) {
            rec.if_media = Some(designation.to_owned());
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
///
/// ⚠️ **A saturated `ifSpeed` with no usable `ifHighSpeed` resolves to `None`, not to the sentinel**
/// (ADR-063 decision 7). `4294967295` is the value the gauge reports when the real rate exceeds what
/// it can express — it is a "too big to say" marker, not a measurement. This used to fall through to
/// it, and the lab's down 10G ports are stored that way today: harmless while the only reader was
/// the chart's bandwidth line, wrong the moment a speed column renders it as "4.29 Gbps". The same
/// value is also `in_util_pct`'s denominator, so utilisation was being computed against a rate no
/// interface has.
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
        // Saturated or absent: only ifHighSpeed can answer. Falling back to `speed` here would
        // store the sentinel itself — see the ⚠️ on this function.
        _ => high_bps.map(|bps| bps as i64),
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

        // How long a job may wait for another probe against the same device to finish.
        //
        // Bounded by the job's own interval: a poll still waiting when its successor is due has
        // stopped being late and started being a queue, and shedding it is the honest answer.
        // Capped at [`MAX_SINGLE_FLIGHT_WAIT`] so a daily check does not sit for hours.
        //
        // Zero-interval jobs (an operator's "poll now") get the cap rather than no wait at all —
        // an on-demand poll landing while the scheduled one is mid-walk should queue behind it,
        // not report a skip to the person who pressed the button.
        fn single_flight_wait(job: &PollJob) -> Duration {
            match job.interval_secs {
                0 => MAX_SINGLE_FLIGHT_WAIT,
                secs => Duration::from_secs(u64::from(secs)).min(MAX_SINGLE_FLIGHT_WAIT),
            }
        }

        // DNS monitors share a target by design — many names, one resolver, and every check using
        // the system resolver carries the same 0.0.0.0 display address. Per-target single-flight
        // would therefore drop every DNS check but one on each cycle, so they take the global-only
        // guard for the same reason Meraki collectors do. Pile-up stays bounded by each check's
        // total timeout budget (≤30 s, enforced in the transport) plus the global concurrency cap.
        let guard = if matches!(job.check, CheckSpec::Dns(_)) {
            limiter.begin_global().await
        } else {
            limiter
                .begin_for(job.target, single_flight_wait(&job))
                .await
        };
        let Some(guard) = guard else {
            metrics::counter!("yagra_poll_skipped_backpressure_total").increment(1);
            tracing::debug!(target = %job.target, "skipping poll: device busy past the deadline");
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

    /// A dark port answers the walk with the vendor's placeholder on every column. It must produce
    /// no reading and no band — while a real row on the same walk still comes through, which is the
    /// half that stops "skip everything" from passing as a fix.
    #[tokio::test]
    async fn a_no_module_row_yields_neither_a_reading_nor_a_band() {
        let d = optical::simple_dialect(yagra_common::OpticalFlavor::Huawei).expect("huawei");
        let row = |oid: &str, ifindex: u32, value: f64| yagra_transport::SnmpTableSample {
            oid_base: oid.to_owned(),
            ifindex,
            value,
        };
        let limits = d.limits.expect("huawei publishes a window");
        let mut fake = FakeTransport::reachable(1.0);
        fake.snmp_table = vec![
            // ifIndex 1 — no transceiver: every column reads the marker, as the lab USG does.
            row(d.rx_oid, 1, -1.0),
            row(d.tx_oid, 1, -1.0),
            row(limits.rx_low_oid, 1, -1.0),
            row(limits.rx_high_oid, 1, -1.0),
            // ifIndex 2 — a live module, with a window that is a genuine pair.
            row(d.rx_oid, 2, -1005.0),
            row(d.tx_oid, 2, -950.0),
            row(limits.rx_low_oid, 2, -1410.0),
            row(limits.rx_high_oid, 2, 200.0),
        ];
        let job = PollJob::snmp_optical(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            yagra_bus::SnmpOpticalCheck {
                community: "public".to_owned(),
                probes: Vec::new(),
                timeout_ms: 1_000,
            },
            30,
        );
        let walker = SnmpWalker::V2c("public".to_owned());
        let (readings, windows) =
            walk_simple_optical(&job, &fake, &walker, Duration::from_secs(1), &d).await;

        assert!(
            readings.iter().all(|r| r.ifindex == 2),
            "the dark port must contribute no reading, got {readings:?}"
        );
        assert_eq!(readings.len(), 2, "the live port still reports rx and tx");
        assert!(
            readings.iter().any(|r| (r.dbm - -10.05).abs() < 1e-9),
            "the live receive level survives unchanged, got {readings:?}"
        );
        assert!(
            !windows.contains_key(&1),
            "a marker row must not become a band either"
        );
        assert!(windows.contains_key(&2), "the live port keeps its band");
    }

    /// **The Cisco sensor dialect end to end: optical readings, the sentinel, and the SFP/chassis
    /// split** (ADR-070 decisions 1 and 2).
    ///
    /// There was no test of the correlated path at all before this — every optical test exercised
    /// a `SimpleDialect`. That gap is why the shape of this one matters more than its size: the
    /// four things asserted here each fail *silently* into "no series", which on a real device is
    /// indistinguishable from "this switch has no optics".
    #[tokio::test]
    async fn the_cisco_sensor_dialect_splits_optical_readings_from_chassis_temperature() {
        use yagra_transport::{SnmpInstanceRow, SnmpTableSample, SnmpTableString, SnmpValue};
        let d = optical::sensor_dialect(yagra_common::OpticalFlavor::CiscoEntitySensor)
            .expect("cisco is a correlated dialect");
        let num = |oid: &str, ent: u32, value: f64| SnmpTableSample {
            oid_base: oid.to_owned(),
            ifindex: ent,
            value,
        };
        // type / scale / precision / value for one entity, in the shape a real Nexus sends.
        let sensor = |ent: u32, ty: f64, scale: f64, prec: f64, value: f64| {
            vec![
                num(d.type_oid, ent, ty),
                num(d.scale_oid, ent, scale),
                num(d.precision_oid, ent, prec),
                num(d.value_oid, ent, value),
            ]
        };
        let text = |ent: u32, s: &str| SnmpTableString {
            oid_base: optical::ENT_PHYSICAL_NAME.to_owned(),
            ifindex: ent,
            value: s.to_owned(),
        };

        let mut fake = FakeTransport::reachable(1.0);
        fake.snmp_table = [
            // ent 100 — a live receive sensor on Ethernet1/1. The exact row from the lab N3K.
            sensor(100, 14.0, 8.0, 0.0, -13187.0),
            // ent 101 — the SFP's *own* temperature, sitting in the same table. Excluded by
            // ADR-062 Issue #66, and excluded here because it reaches a port.
            sensor(101, 8.0, 9.0, 0.0, 45.0),
            // ent 200 — `module-1 FRONT`, a chassis sensor. Reaches no port, so it is the one
            // temperature that survives.
            sensor(200, 8.0, 9.0, 0.0, 31.0),
            // ent 300 — a transmit sensor reading 0, which is what an N9K sends for all fourteen
            // of its dBm sensors when nothing is plugged in. 0 dBm is 1 mW — stronger than the
            // live port above — so it must produce nothing.
            sensor(300, 14.0, 8.0, 0.0, 0.0),
        ]
        .concat();
        fake.snmp_table_strings = vec![
            text(100, "Ethernet1/1 Lane 1 Transceiver Receive Power Sensor"),
            text(101, "Ethernet1/1 Lane 1 Transceiver Temperature Sensor"),
            text(200, "module-1 FRONT"),
            text(300, "Ethernet1/2 Lane 1 Transceiver Transmit Power Sensor"),
        ];
        // ENTITY-MIB: the two optical sensors and the SFP temperature hang off ports; the chassis
        // sensor has a parent that leads nowhere. This is the whole SFP/chassis discriminator.
        let alias = |ent: u32, ifindex: u32| SnmpInstanceRow {
            oid_base: optical::ENT_ALIAS_MAPPING.to_owned(),
            instance: vec![ent, 0],
            value: SnmpValue::Oid(format!("1.3.6.1.2.1.2.2.1.1.{ifindex}")),
        };
        let parent = |ent: u32, p: u32| SnmpInstanceRow {
            oid_base: optical::ENT_PHYSICAL_CONTAINED_IN.to_owned(),
            instance: vec![ent],
            value: SnmpValue::Int(i64::from(p)),
        };
        fake.snmp_instances = vec![
            parent(100, 10),
            parent(101, 10),
            alias(10, 1), // port Ethernet1/1
            parent(300, 20),
            alias(20, 2), // port Ethernet1/2
            // The chassis sensor climbs to a module that owns no interface — a dead end, exactly
            // as `module-1 FRONT` does on the real N9K (four hops, no alias).
            parent(200, 900),
        ];

        let job = PollJob::snmp_optical(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            yagra_bus::SnmpOpticalCheck {
                community: "public".to_owned(),
                probes: vec![yagra_bus::OpticalProbe {
                    flavor: yagra_common::OpticalFlavor::CiscoEntitySensor,
                    rx_metric: Some(yagra_common::METRIC_IF_RX_POWER_DBM.to_owned()),
                    tx_metric: Some(yagra_common::METRIC_IF_TX_POWER_DBM.to_owned()),
                    temp_metric: Some(yagra_common::METRIC_CISCO_TEMP_C.to_owned()),
                }],
                timeout_ms: 1_000,
            },
            30,
        );
        let r = execute(&job, &fake, 1_000).await;
        let of = |metric: &str| -> Vec<(Option<u32>, f64)> {
            r.samples
                .iter()
                .filter(|s| s.metric == metric)
                .map(|s| (s.ifindex.map(|i| i.0), s.value))
                .collect()
        };

        // The Cisco columns were walked at all — if the dialect were still hardcoded to the
        // standard root this would be empty, which is the pre-ADR-070 behaviour on every Catalyst.
        assert_eq!(
            of(yagra_common::METRIC_IF_RX_POWER_DBM),
            vec![(Some(1), -13.187)],
            "the live receive level, translated to its ifIndex"
        );
        // The 0 dBm marker must not become the strongest reading on the switch.
        assert!(
            of(yagra_common::METRIC_IF_TX_POWER_DBM).is_empty(),
            "a 0 dBm sensor is 'no module', not a measurement"
        );
        // Exactly one temperature: the chassis one. The SFP's own temperature (ent 101, 45 °C) is
        // excluded *structurally* — it reaches a port — not by matching its description.
        assert_eq!(
            of(yagra_common::METRIC_CISCO_TEMP_C),
            vec![(Some(200), 31.0)],
            "only the sensor that belongs to no port becomes a chassis temperature"
        );
    }

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
                if_duplex: None,
                if_type: None,
                if_media: None,
                transceiver_model: None,
                // A Meraki uplink is not an SNMP transceiver; the optical window is never filled
                // from this path, and `None` here leaves anything already stored untouched.
                rx_power_low_dbm: None,
                rx_power_high_dbm: None,
                tx_power_low_dbm: None,
                tx_power_high_dbm: None,
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

    /// A table job built from the **real built-in catalog**, not from a hand-written fixture.
    ///
    /// 🚨 The hand-written `snmp_table_job` declares two metric columns and neither is
    /// `if_high_speed`, which is precisely why nothing caught the demux bug it was supposed to
    /// cover: the fake was narrower than the thing it stood for, so every test agreed while every
    /// real node behaved differently. Anything asserting how metric columns and interface-metadata
    /// columns interact has to be built from the catalog that actually ships.
    fn catalog_table_job() -> PollJob {
        use yagra_bus::{SnmpColumn, SnmpMetaColumn, SnmpTableCheck};
        use yagra_common::{builtin_catalog, builtin_interface_meta_columns, CollectionKind};
        PollJob::snmp_table(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            SnmpTableCheck {
                community: "public".to_owned(),
                columns: builtin_catalog()
                    .into_iter()
                    .filter(|i| i.kind == CollectionKind::Table)
                    .map(|i| SnmpColumn {
                        metric_name: i.metric_name,
                        oid: i.oid,
                        kind: i.metric_kind,
                    })
                    .collect(),
                meta_columns: builtin_interface_meta_columns()
                    .into_iter()
                    .map(|(field, oid)| SnmpMetaColumn {
                        field,
                        oid: oid.to_owned(),
                    })
                    .collect(),
                timeout_ms: 2000,
            },
            60,
        )
    }

    /// The overlap this walk has to survive, stated as its own assertion.
    ///
    /// `ifHighSpeed` is a metric the catalog charts **and** the only 64-bit source for the speed
    /// column. If a future edit drops it from the catalog, the hoisted capture in the demux becomes
    /// ordinary code rather than a deliberate exception — and the comment explaining why it is
    /// hoisted becomes a lie. Failing here is how that gets noticed.
    #[test]
    fn the_high_speed_column_is_both_a_metric_and_the_speed_source() {
        use yagra_common::{builtin_catalog, builtin_interface_meta_columns, InterfaceField};
        assert!(
            builtin_catalog().iter().any(|i| i.oid == OID_IF_HIGH_SPEED),
            "ifHighSpeed must still be a declared metric column, or the demux hoist is pointless",
        );
        // …and it must NOT be the declared meta column, which is the 32-bit ifSpeed. If these two
        // ever became the same OID, `resolve_if_speed` would read Mbps as bits/sec and store a
        // 1 Gbps link as 1000 bps — a wrong number, which is worse than the empty cell this fixes.
        let meta_speed = builtin_interface_meta_columns()
            .into_iter()
            .find(|(f, _)| matches!(f, InterfaceField::Speed))
            .map(|(_, oid)| oid)
            .expect("a Speed meta column exists");
        assert_ne!(meta_speed, OID_IF_HIGH_SPEED);
    }

    /// A device that answers **only** ifXTable still gets a speed.
    ///
    /// This is the shape 19 of the 21 lab devices actually have — `ifSpeed` absent, `ifHighSpeed`
    /// present — and the shape every 10G+ port has once its 32-bit gauge saturates. Before the
    /// demux hoist this stored `None` for all of them.
    #[tokio::test]
    async fn if_high_speed_feeds_the_speed_column_as_well_as_the_metric() {
        use yagra_transport::SnmpTableSample;
        let t = FakeTransport::reachable(0.0).with_snmp_table(vec![
            // ifHighSpeed only — no ifSpeed row at all, exactly as the lab captures answer.
            SnmpTableSample {
                oid_base: OID_IF_HIGH_SPEED.to_owned(),
                ifindex: 7,
                value: 10_000.0,
            },
            SnmpTableSample {
                oid_base: OID_IF_HIGH_SPEED.to_owned(),
                ifindex: 8,
                value: 1_000.0,
            },
        ]);
        let r = execute(&catalog_table_job(), &t, 1_000).await;

        let speed = |ifindex: u32| {
            r.interfaces
                .iter()
                .find(|i| i.ifindex == IfIndex(ifindex))
                .unwrap_or_else(|| panic!("ifIndex {ifindex} must have an interface row"))
                .if_speed
        };
        assert_eq!(speed(7), Some(10_000_000_000), "10 Gbps from ifHighSpeed");
        assert_eq!(speed(8), Some(1_000_000_000), "1 Gbps from ifHighSpeed");

        // …and the metric is still charted. The fix is an `and`: reordering the chain instead of
        // hoisting would have swapped one silent loss for another.
        for ifindex in [7u32, 8] {
            assert!(
                r.samples.iter().any(|s| s.metric == "if_high_speed"
                    && s.ifindex == Some(IfIndex(ifindex))
                    && s.kind == MetricKind::Gauge),
                "if_high_speed sample for ifIndex {ifindex} must still be emitted",
            );
        }
    }

    /// A saturated 32-bit `ifSpeed` resolves through `ifHighSpeed` when both arrive together.
    ///
    /// ADR-063 decision 7 refuses to store the `4294967295` sentinel. That refusal is only correct
    /// if the 64-bit column can still answer — otherwise it turns a wrong number into no number,
    /// which is what the lab's two real 10G ports had.
    #[tokio::test]
    async fn a_saturated_if_speed_still_resolves_through_the_high_speed_column() {
        use yagra_transport::SnmpTableSample;
        let t = FakeTransport::reachable(0.0).with_snmp_table(vec![
            SnmpTableSample {
                oid_base: "1.3.6.1.2.1.2.2.1.5".to_owned(),
                ifindex: 4,
                value: u32::MAX as f64,
            },
            SnmpTableSample {
                oid_base: OID_IF_HIGH_SPEED.to_owned(),
                ifindex: 4,
                value: 10_000.0,
            },
        ]);
        let r = execute(&catalog_table_job(), &t, 1_000).await;
        let iface = r
            .interfaces
            .iter()
            .find(|i| i.ifindex == IfIndex(4))
            .expect("ifIndex 4");
        assert_eq!(iface.if_speed, Some(10_000_000_000));
    }

    /// The lab's real Huawei USG, port for port (ADR-063 Inc.4).
    ///
    /// Its two live ports are metal and run at 100 Mbit/s and 1 Gbit/s; its two 10GE ports are
    /// optical. Before this increment every one of the sixteen media cells was empty, because the
    /// standard source (`ifMauType`) answers No Such Object on this platform and the other one only
    /// ever names a pluggable.
    #[tokio::test]
    async fn a_huawei_port_gets_its_media_from_the_medium_and_the_speed() {
        use yagra_transport::SnmpTableSample;
        let sample = |oid: &str, ifindex: u32, value: f64| SnmpTableSample {
            oid_base: oid.to_owned(),
            ifindex,
            value,
        };
        let t = FakeTransport::reachable(0.0).with_snmp_table(vec![
            // GE0/0/1 — copper at 100 Mbit/s.
            sample(OID_IF_HIGH_SPEED, 7, 100.0),
            sample(OID_HW_ETHERNET_PORT_TYPE, 7, 2.0),
            // GE0/0/2 — copper at 1 Gbit/s.
            sample(OID_IF_HIGH_SPEED, 8, 1_000.0),
            sample(OID_HW_ETHERNET_PORT_TYPE, 8, 2.0),
            // 10GE0/0/0 — optical. Read, and deliberately left without a designation.
            sample(OID_IF_HIGH_SPEED, 4, 10_000.0),
            sample(OID_HW_ETHERNET_PORT_TYPE, 4, 3.0),
            // A port the agent will not classify: other(1) is "not known", not a third medium.
            sample(OID_IF_HIGH_SPEED, 9, 1_000.0),
            sample(OID_HW_ETHERNET_PORT_TYPE, 9, 1.0),
        ]);
        let r = execute(&catalog_table_job(), &t, 1_000).await;
        let media = |ifindex: u32| {
            r.interfaces
                .iter()
                .find(|i| i.ifindex == IfIndex(ifindex))
                .unwrap_or_else(|| panic!("ifIndex {ifindex}"))
                .if_media
                .clone()
        };
        assert_eq!(media(7).as_deref(), Some("100BASE-TX"));
        assert_eq!(media(8).as_deref(), Some("1000BASE-T"));
        assert_eq!(
            media(4),
            None,
            "a fibre port must not be given a designation"
        );
        assert_eq!(media(9), None, "other(1) must not become a medium");
    }

    /// A medium with no speed, and a speed with no medium, both stay empty.
    ///
    /// Stated because the fold reads two maps and the failure would be silent either way: a
    /// designation invented from half the inputs is a wrong value in a column whose whole point is
    /// that it never guesses.
    #[tokio::test]
    async fn media_needs_both_halves_and_declines_when_it_has_one() {
        use yagra_transport::SnmpTableSample;
        let sample = |oid: &str, ifindex: u32, value: f64| SnmpTableSample {
            oid_base: oid.to_owned(),
            ifindex,
            value,
        };
        let t = FakeTransport::reachable(0.0).with_snmp_table(vec![
            // Copper, but the device never reported a speed for it.
            sample(OID_HW_ETHERNET_PORT_TYPE, 11, 2.0),
            // A speed, but no medium column at all — every non-Huawei device in the lab.
            sample(OID_IF_HIGH_SPEED, 12, 1_000.0),
            // Copper at a speed with no transcribed twisted-pair registration (2.5GBASE-T).
            sample(OID_HW_ETHERNET_PORT_TYPE, 13, 2.0),
            sample(OID_IF_HIGH_SPEED, 13, 2_500.0),
        ]);
        let r = execute(&catalog_table_job(), &t, 1_000).await;
        let iface = |ifindex: u32| r.interfaces.iter().find(|i| i.ifindex == IfIndex(ifindex));
        // No speed ⇒ no row is materialised for it at all, and certainly no media.
        assert!(iface(11).is_none_or(|i| i.if_media.is_none()));
        assert_eq!(iface(12).expect("ifIndex 12").if_media, None);
        assert_eq!(iface(13).expect("ifIndex 13").if_media, None);
        // …and the speed itself is still stored for the two that reported one.
        assert_eq!(iface(12).unwrap().if_speed, Some(1_000_000_000));
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
    }

    /// A saturated `ifSpeed` with no usable `ifHighSpeed` is **not** a speed (ADR-063 decision 7).
    ///
    /// This assertion is inverted from what it used to be, and the reversal is the point: the old
    /// behaviour stored `u32::MAX` itself as a "best-effort" rate. `4294967295` is the gauge's way
    /// of saying *"the real rate is larger than I can express"* — keeping it means a 10 Gbps port
    /// is recorded as 4.29 Gbps, and `in_util_pct` is then computed against a rate no interface
    /// has. It was invisible while the only reader was the throughput chart's bandwidth line on a
    /// hand-selected (therefore up) interface; a speed column renders it for every port.
    ///
    /// The lab's down 10G ports store the sentinel today, so this is a live wrong value, not a
    /// hypothetical one.
    #[test]
    fn a_saturated_if_speed_with_no_high_speed_is_unknown_not_the_sentinel() {
        assert_eq!(resolve_if_speed(Some(u32::MAX as f64), None), None);
        // Same when the device answers ifHighSpeed but with the "no idea" zero, which is what a
        // down port typically reports — measured on the lab's 10GE0/0/0.
        assert_eq!(resolve_if_speed(Some(u32::MAX as f64), Some(0.0)), None);
        // ⚠️ But one bit below the cap is a real 4.29 Gbps reading and must survive.
        assert_eq!(
            resolve_if_speed(Some(u32::MAX as f64 - 1.0), None),
            Some(u32::MAX as i64 - 1)
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

    /// Duplex and ifType ride the same numeric walk and land on the right ifIndex (ADR-063 Inc.1).
    ///
    /// The accepting case is the load-bearing one: everything about this feature — the OIDs being
    /// appended, the demux arms, the fold — fails *silently* into "column always empty", which is
    /// indistinguishable from a device that does not implement EtherLike-MIB. A test that only
    /// checked the rejecting cases would pass against a poller that walks neither OID.
    #[tokio::test]
    async fn snmp_table_walk_carries_duplex_and_if_type() {
        use yagra_transport::SnmpTableSample;
        let t = FakeTransport::reachable(0.0).with_snmp_table(vec![
            // Two interfaces' worth of a metric column, so the poll is reachable and so the
            // per-ifIndex demux has something to get wrong.
            SnmpTableSample {
                oid_base: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                ifindex: 1,
                value: 10.0,
            },
            SnmpTableSample {
                oid_base: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                ifindex: 2,
                value: 20.0,
            },
            // ifIndex 1: a copper port, full duplex, ethernetCsmacd.
            SnmpTableSample {
                oid_base: OID_DOT3_DUPLEX_STATUS.to_owned(),
                ifindex: 1,
                value: 3.0,
            },
            SnmpTableSample {
                oid_base: OID_IF_TYPE.to_owned(),
                ifindex: 1,
                value: 6.0,
            },
            // ifIndex 2: a loopback answering `unknown(1)` — the shape that must NOT become a
            // stored duplex. `if_type` still lands, and it is what lets a reader say "does not
            // apply" rather than "could not read".
            SnmpTableSample {
                oid_base: OID_DOT3_DUPLEX_STATUS.to_owned(),
                ifindex: 2,
                value: 1.0,
            },
            SnmpTableSample {
                oid_base: OID_IF_TYPE.to_owned(),
                ifindex: 2,
                value: 24.0,
            },
        ]);
        let r = execute(&snmp_table_job(), &t, 1_000).await;
        let find = |ix: u32| {
            r.interfaces
                .iter()
                .find(|i| i.ifindex == IfIndex(ix))
                .unwrap_or_else(|| panic!("ifIndex {ix} discovered"))
        };

        let copper = find(1);
        assert_eq!(
            copper.if_duplex,
            Some(yagra_common::Duplex::Full),
            "full duplex on ifIdx 1"
        );
        assert_eq!(copper.if_type, Some(6));

        let loopback = find(2);
        assert_eq!(
            loopback.if_duplex, None,
            "`unknown(1)` must store as unknown, not as a duplex"
        );
        assert_eq!(loopback.if_type, Some(24));
    }

    /// A device with no EtherLike-MIB still reports duplex, via Huawei's column (ADR-063 Inc.3).
    ///
    /// 🚨 The assertion on ifIndex 1 is the one that matters: `hwEthernetDuplex` says `full(1)`,
    /// and the standard mapper reads `1` as `unknown`. If the Huawei rows were ever fed through
    /// `duplex_from_dot3` — the obvious "reuse" — this would be `None`, which is byte-identical to
    /// the behaviour before this feature existed. **The bug would look like the feature simply not
    /// working**, on a device nobody can compare against, so it has to be pinned here.
    #[tokio::test]
    async fn a_device_without_etherlike_mib_gets_duplex_from_the_huawei_column() {
        use yagra_transport::SnmpTableSample;
        let metric = |ifindex: u32| SnmpTableSample {
            oid_base: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
            ifindex,
            value: 10.0,
        };
        let hw = |ifindex: u32, value: f64| SnmpTableSample {
            oid_base: OID_HW_ETHERNET_DUPLEX.to_owned(),
            ifindex,
            value,
        };
        let t = FakeTransport::reachable(0.0).with_snmp_table(vec![
            metric(1),
            metric(2),
            metric(3),
            metric(4),
            // ifIndex 1: the lab USG's shape — Huawei column only, `full(1)` on every port.
            hw(1, 1.0),
            hw(2, 2.0),
            // ifIndex 3: both columns present and *disagreeing*. EtherLike is the standard and
            // must win, so the answer is Half even though the Huawei column says full.
            SnmpTableSample {
                oid_base: OID_DOT3_DUPLEX_STATUS.to_owned(),
                ifindex: 3,
                value: 2.0,
            },
            hw(3, 1.0),
            // ifIndex 4: `3` is a value the Huawei enumeration does not define. It must not be
            // read as `fullDuplex(3)` from the other MIB.
            hw(4, 3.0),
        ]);
        let r = execute(&snmp_table_job(), &t, 1_000).await;
        let row = |ix: u32| r.interfaces.iter().find(|i| i.ifindex == IfIndex(ix));
        let duplex = |ix: u32| {
            row(ix)
                .unwrap_or_else(|| panic!("ifIndex {ix} discovered"))
                .if_duplex
        };

        assert_eq!(
            duplex(1),
            Some(yagra_common::Duplex::Full),
            "hwEthernetDuplex full(1) — reusing the dot3 mapper here would silently give None"
        );
        assert_eq!(duplex(2), Some(yagra_common::Duplex::Half));
        assert_eq!(
            duplex(3),
            Some(yagra_common::Duplex::Half),
            "dot3StatsDuplexStatus wins when a device answers both"
        );
        // 3 is not a value in Huawei's enumeration. It must not borrow `fullDuplex(3)` from the
        // standard one — and because that leaves the row with nothing usable, the fold declines to
        // materialise the interface at all rather than adding an index-only record (the same guard
        // that stops a device answering `unknown(1)` everywhere from inflating the inventory).
        assert!(
            row(4).is_none(),
            "an unmappable duplex reading must not conjure an interface row"
        );
    }

    /// A device that implements neither OID still gets its names and speed — the ADR-063 columns
    /// are additive, and a poller that walks two OIDs the agent ignores must not lose the rest.
    #[tokio::test]
    async fn a_device_without_etherlike_mib_still_reports_its_interfaces() {
        use yagra_transport::SnmpTableSample;
        let t = FakeTransport::reachable(0.0)
            .with_snmp_table(vec![SnmpTableSample {
                oid_base: "1.3.6.1.2.1.31.1.1.1.6".to_owned(),
                ifindex: 1,
                value: 10.0,
            }])
            .with_snmp_table_strings(vec![SnmpTableString {
                oid_base: "1.3.6.1.2.1.31.1.1.1.1".to_owned(),
                ifindex: 1,
                value: "GE0/0/1".to_owned(),
            }]);
        let r = execute(&snmp_table_job(), &t, 1_000).await;
        let iface = r
            .interfaces
            .iter()
            .find(|i| i.ifindex == IfIndex(1))
            .expect("ifIndex 1 discovered");
        assert_eq!(iface.if_name.as_deref(), Some("GE0/0/1"));
        assert_eq!(iface.if_duplex, None);
        assert_eq!(iface.if_type, None);
    }
}
