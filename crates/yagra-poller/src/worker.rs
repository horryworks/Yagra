//! The poller work loop.
//!
//! A poller consumes [`PollJob`]s from the bus, executes them via the [`Transport`]
//! abstraction (never a raw protocol), and publishes [`PollResult`]s back. It holds no
//! state beyond the in-flight job — that statelessness is what lets pollers scale out
//! and fail over (ADR-003/009). Counters are reported raw; rates are derived later
//! (ADR-012).

use crate::limiter::PollLimiter;
use futures::stream::{Stream, StreamExt};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use yagra_bus::{Bus, CheckOutcome, CheckSpec, PollJob, PollResult, Sample, BUS_SCHEMA_VERSION};
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
            match transport
                .snmp_get(job.target, &snmp.community, &snmp.oids, timeout)
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
                        .map(|s| Sample::gauge(snmp_metric_name(&s.oid), s.value))
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
            match transport
                .snmp_v3_get(job.target, &params, &v3.oids, timeout)
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
                        .map(|s| Sample::gauge(snmp_metric_name(&s.oid), s.value))
                        .collect();
                    result(job, at_unix_ms, outcome, mapped)
                }
                Err(err) => {
                    tracing::warn!(job_id = %job.job_id, error = %err, "snmp v3 get failed");
                    result(job, at_unix_ms, CheckOutcome::Error, Vec::new())
                }
            }
        }
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
