//! The poller work loop.
//!
//! A poller consumes [`PollJob`]s from the bus, executes them via the [`Transport`]
//! abstraction (never a raw protocol), and publishes [`PollResult`]s back. It holds no
//! state beyond the in-flight job — that statelessness is what lets pollers scale out
//! and fail over (ADR-003/009). Counters are reported raw; rates are derived later
//! (ADR-012).

use crate::limiter::PollLimiter;
use futures::stream::{Stream, StreamExt};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use yagra_bus::{
    Bus, CheckOutcome, CheckSpec, DiscoveredInterface, PollJob, PollResult, Sample, SnmpColumn,
    SnmpMetaColumn, SnmpTableCheck, BUS_SCHEMA_VERSION,
};
use yagra_common::{IfIndex, InterfaceField};
use yagra_transport::Transport;

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
                    result(job, at_unix_ms, outcome, mapped)
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
                    result(job, at_unix_ms, outcome, mapped)
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
    }
}

/// Execute an SNMP table-walk check: numeric columns become per-interface samples (keyed
/// by ifIndex, using the column's explicit metric name and kind — no OID-name guessing),
/// and metadata columns become [`DiscoveredInterface`] records (PostgreSQL inventory, never
/// TSDB labels — ADR-011). Split out of [`execute`] for readability and direct testing.
async fn execute_snmp_table(
    job: &PollJob,
    transport: &dyn Transport,
    at_unix_ms: i64,
    table: &SnmpTableCheck,
    timeout: Duration,
) -> PollResult {
    let column_oids: Vec<String> = table.columns.iter().map(|c| c.oid.clone()).collect();
    let by_base: HashMap<&str, &SnmpColumn> =
        table.columns.iter().map(|c| (c.oid.as_str(), c)).collect();

    let mut samples = Vec::new();
    match transport
        .snmp_walk(job.target, &table.community, &column_oids, timeout)
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
                }
            }
        }
        Err(err) => tracing::warn!(job_id = %job.job_id, error = %err, "snmp table walk failed"),
    }

    let interfaces = walk_interface_metadata(
        job,
        transport,
        &table.community,
        &table.meta_columns,
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
    }
}

/// Walk interface-metadata columns and fold them per ifIndex into [`DiscoveredInterface`]s.
/// `ifName`/`ifAlias` are string columns; `ifSpeed` is numeric — each walked appropriately.
async fn walk_interface_metadata(
    job: &PollJob,
    transport: &dyn Transport,
    community: &str,
    meta_columns: &[SnmpMetaColumn],
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
    let speed_oids: Vec<String> = meta_columns
        .iter()
        .filter(|m| matches!(m.field, InterfaceField::Speed))
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
        match transport
            .snmp_walk_strings(job.target, community, &string_oids, timeout)
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

    if !speed_oids.is_empty() {
        match transport
            .snmp_walk(job.target, community, &speed_oids, timeout)
            .await
        {
            Ok(rows) => {
                for row in rows {
                    let rec = ifs.entry(row.ifindex).or_insert_with(|| blank(row.ifindex));
                    rec.if_speed = Some(row.value as i64);
                }
            }
            Err(err) => {
                tracing::debug!(job_id = %job.job_id, error = %err, "snmp ifSpeed walk failed");
            }
        }
    }

    ifs.into_values().collect()
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
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Run the poll loop over a stream of jobs. Each job runs concurrently under the
/// [`PollLimiter`]: a global concurrency cap bounds total load and per-device single-flight
/// drops a poll whose target is still being probed (backpressure, monitoring-conventions).
/// Returns when the stream ends. Stream-generic so the same loop drives both the in-memory
/// bus (tests/skeleton) and the NATS queue subscription (production), ADR-003/009.
pub async fn run_stream<B, S>(
    mut jobs: S,
    bus: Arc<B>,
    transport: Arc<dyn Transport>,
    limiter: Arc<PollLimiter>,
) where
    B: Bus + 'static,
    S: Stream<Item = PollJob> + Unpin,
{
    while let Some(job) = jobs.next().await {
        let Some(guard) = limiter.try_begin(job.target).await else {
            metrics::counter!("yagra_poll_skipped_backpressure_total").increment(1);
            tracing::debug!(target = %job.target, "skipping poll: previous still in flight");
            continue;
        };
        let bus = bus.clone();
        let transport = transport.clone();
        tokio::spawn(async move {
            let _guard = guard; // released (and target unmarked) when the probe finishes
            metrics::counter!("yagra_poll_jobs_executed_total").increment(1);
            let result = execute(&job, transport.as_ref(), now_unix_ms()).await;
            if let Err(err) = bus.publish_result(result).await {
                tracing::error!(error = %err, "failed to publish poll result");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use uuid::Uuid;
    use yagra_bus::{IcmpCheck, InMemoryBus, SnmpCheck};
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

    #[tokio::test]
    async fn snmp_table_no_values_is_unreachable() {
        let t = FakeTransport::reachable(0.0); // no canned table rows
        let r = execute(&snmp_table_job(), &t, 1_000).await;
        assert_eq!(r.outcome, CheckOutcome::Unreachable);
        assert!(r.samples.is_empty());
        assert!(r.interfaces.is_empty());
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
        tokio::spawn(run_stream(jobs, bus.clone(), transport, limiter));

        // Simulate core dispatching a job.
        bus.publish_job(icmp_job()).await.unwrap();

        let result = results_rx.recv().await.unwrap();
        assert_eq!(result.job_id, Uuid::nil());
        assert_eq!(result.outcome, CheckOutcome::Reachable);
        assert!(!result.samples.is_empty());
    }
}
