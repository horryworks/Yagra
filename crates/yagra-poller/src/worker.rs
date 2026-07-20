// SPDX-License-Identifier: AGPL-3.0-only
//! The poller work loop.
//!
//! A poller consumes [`PollJob`]s from the bus, executes them via the [`Transport`]
//! abstraction (never a raw protocol), and publishes [`PollResult`]s back. It holds no
//! state beyond the in-flight job — that statelessness is what lets pollers scale out
//! and fail over (ADR-003/009). Counters are reported raw; rates are derived later
//! (ADR-012).

use crate::limiter::PollLimiter;
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
    SnmpMetaColumn, SnmpTableCheck, SnmpV3TableCheck, BUS_SCHEMA_VERSION,
};
use yagra_common::{
    IfIndex, InterfaceField, MetricKind, NodeId, METRIC_HTTP_STATUS_CODE, METRIC_HTTP_UP,
    METRIC_SSL_CERT_DAYS_TO_EXPIRY, OID_IF_HIGH_SPEED,
};
use yagra_transport::{
    HttpProbeSpec, MerakiCollectSpec, SnmpTableSample, SnmpTableString, SnmpV3Params, Transport,
    TransportError,
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
                        samples.push(Sample::gauge("icmp_rtt_ms", rtt));
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
            // GET the bare OIDs and the explicitly-named scalar columns together.
            let col_by_oid: HashMap<&str, &SnmpColumn> =
                snmp.columns.iter().map(|c| (c.oid.as_str(), c)).collect();
            let mut all_oids = snmp.oids.clone();
            all_oids.extend(snmp.columns.iter().map(|c| c.oid.clone()));
            match transport
                .snmp_get(job.target, &snmp.community, &all_oids, timeout)
                .await
            {
                Ok(samples) => {
                    // No values back ⇒ treat as unreachable (agent down / wrong community).
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
                    // Identity probe: on a reachable poll core asked us to classify, fetch
                    // sysDescr.0 so core can fill the node's maker/model (best-effort).
                    if job.probe_identity && outcome == CheckOutcome::Reachable {
                        r.sys_descr =
                            fetch_sys_descr_v2c(job.target, &snmp.community, timeout, transport)
                                .await;
                    }
                    r
                }
                Err(err) => {
                    tracing::warn!(job_id = %job.job_id, error = %err, "snmp get failed");
                    result(job, at_unix_ms, CheckOutcome::Error, Vec::new())
                }
            }
        }
        CheckSpec::SnmpV3(v3) => {
            let timeout = Duration::from_millis(u64::from(v3.timeout_ms));
            let params = yagra_transport::SnmpV3Params {
                user: v3.user.clone(),
                security_level: v3.security_level.clone(),
                auth_protocol: v3.auth_protocol.clone(),
                auth_key: v3.auth_key.clone(),
                priv_protocol: v3.priv_protocol.clone(),
                priv_key: v3.priv_key.clone(),
            };
            // GET the bare OIDs and the explicitly-named scalar columns together (mirrors
            // the v2c arm: configured collection sets travel as named columns).
            let col_by_oid: HashMap<&str, &SnmpColumn> =
                v3.columns.iter().map(|c| (c.oid.as_str(), c)).collect();
            let mut all_oids = v3.oids.clone();
            all_oids.extend(v3.columns.iter().map(|c| c.oid.clone()));
            match transport
                .snmp_v3_get(job.target, &params, &all_oids, timeout)
                .await
            {
                Ok(samples) => {
                    let outcome = if samples.is_empty() {
                        CheckOutcome::Unreachable
                    } else {
                        CheckOutcome::Reachable
                    };
                    let mapped = samples
                        .into_iter()
                        .map(|s| match col_by_oid.get(s.oid.as_str()) {
                            Some(col) => Sample {
                                metric: col.metric_name.clone(),
                                ifindex: None,
                                value: s.value,
                                kind: col.kind,
                            },
                            None => Sample::gauge(snmp_metric_name(&s.oid), s.value),
                        })
                        .collect();
                    let mut r = result(job, at_unix_ms, outcome, mapped);
                    if job.probe_identity && outcome == CheckOutcome::Reachable {
                        r.sys_descr =
                            fetch_sys_descr_v3(job.target, &params, timeout, transport).await;
                    }
                    r
                }
                Err(err) => {
                    tracing::warn!(job_id = %job.job_id, error = %err, "snmp v3 get failed");
                    result(job, at_unix_ms, CheckOutcome::Error, Vec::new())
                }
            }
        }
        CheckSpec::SnmpTable(table) => {
            let timeout = Duration::from_millis(u64::from(table.timeout_ms));
            execute_snmp_table(job, transport, at_unix_ms, table, timeout).await
        }
        CheckSpec::SnmpV3Table(table) => {
            let timeout = Duration::from_millis(u64::from(table.timeout_ms));
            execute_snmp_v3_table(job, transport, at_unix_ms, table, timeout).await
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
                    if let Some(days) = probe.cert_days_to_expiry {
                        samples.push(Sample::gauge(METRIC_SSL_CERT_DAYS_TO_EXPIRY, days));
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
            schema_version: BUS_SCHEMA_VERSION,
            job_id: job.job_id,
            node_id,
            at_unix_ms,
            outcome: CheckOutcome::Reachable,
            samples,
            interfaces,
            sys_descr: None,
            poller_id: None,
            trace_context: Default::default(),
        });
    }
    results
}

/// The credential half of a table walk that differs between v2c and v3 (community vs USM params).
/// Capturing it here lets the column-walk + interface-metadata logic below be shared across both
/// protocols instead of duplicated — a v3 walk keys rows and folds interface metadata identically.
enum SnmpWalker {
    V2c(String),
    V3(SnmpV3Params),
}

impl SnmpWalker {
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
        schema_version: BUS_SCHEMA_VERSION,
        job_id: job.job_id,
        node_id: job.node_id,
        at_unix_ms,
        outcome,
        samples,
        interfaces,
        sys_descr: None,
        poller_id: None,
        // Stamped by `run_stream` from the poll span before publish (empty here = no trace).
        trace_context: Default::default(),
    }
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
        schema_version: BUS_SCHEMA_VERSION,
        job_id: job.job_id,
        node_id: job.node_id,
        at_unix_ms,
        outcome,
        samples,
        interfaces: Vec::new(),
        sys_descr: None,
        poller_id: None,
        // Stamped by `run_stream` from the poll span before publish (empty here = no trace).
        trace_context: Default::default(),
    }
}

/// Fetch `sysDescr.0` over v2c (walk the column base → the `.0` instance). Best-effort: `None`
/// on any transport error or an empty/missing value. Used for the identity probe (maker/model).
async fn fetch_sys_descr_v2c(
    target: std::net::IpAddr,
    community: &str,
    timeout: Duration,
    transport: &dyn Transport,
) -> Option<String> {
    let bases = [SYSDESCR_BASE.to_owned()];
    let rows = transport
        .snmp_walk_strings(target, community, &bases, timeout)
        .await
        .ok()?;
    rows.into_iter()
        .find(|r| r.oid_base == SYSDESCR_BASE)
        .map(|r| r.value)
        .filter(|v| !v.is_empty())
}

/// Fetch `sysDescr.0` over v3 (scalar GET). Best-effort, like [`fetch_sys_descr_v2c`].
async fn fetch_sys_descr_v3(
    target: std::net::IpAddr,
    params: &yagra_transport::SnmpV3Params,
    timeout: Duration,
    transport: &dyn Transport,
) -> Option<String> {
    let oids = [SYSDESCR_OID.to_owned()];
    let rows = transport
        .snmp_v3_get_strings(target, params, &oids, timeout)
        .await
        .ok()?;
    rows.into_iter()
        .find(|r| r.oid == SYSDESCR_OID)
        .map(|r| r.value)
        .filter(|v| !v.is_empty())
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

        let Some(guard) = limiter.try_begin(job.target).await else {
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
    }

    #[tokio::test]
    async fn http_down_and_unreachable_when_no_response() {
        let t = FakeTransport::unreachable(); // http probe: reachable=false, no status
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
        use yagra_bus::{NodeJobs, SyncMsg, WorkingSetSnapshot, BUS_SCHEMA_VERSION};

        let bus = Arc::new(InMemoryBus::new(16));
        let mut results_rx = bus.subscribe_results();
        let transport: Arc<dyn Transport> = Arc::new(FakeTransport::reachable(5.0));

        // Build a one-node, one-ICMP-spec working set from a single-chunk snapshot.
        let node = NodeId::from(Uuid::nil());
        let snap = SyncMsg::SnapshotChunk(WorkingSetSnapshot {
            schema_version: BUS_SCHEMA_VERSION,
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
