// SPDX-License-Identifier: AGPL-3.0-only
//! The work loop: jobs in, results out (ADR-003/009/020).
//!
//! The only file here that knows about the bus, the rate limiter, or the store-and-forward buffer —
//! everything below it is handed a job and a [`Transport`] and hands back a [`PollResult`]. A
//! poller holds no state beyond the in-flight job, and that is what lets pollers scale out and fail
//! over.
//!
//! ⚠️ **Two check kinds are named here and it is not incidental.** `MerakiCollect` fans one job out
//! to many results, and `Dns` takes the global-only guard rather than per-device single-flight
//! because every DNS check against the system resolver carries the same `0.0.0.0` display address
//! and would otherwise starve itself. A third kind that needs either treatment has to say so —
//! `guards.rs` reads the ownership table (ADR-099).

use super::*;

/// sysDescr.0 — system description scalar (the v3 GET form).
/// Ceiling on how long one job waits for another probe against the same device (see
/// `single_flight_wait`). A device's specs are serialised, so this bounds the tail of that chain;
/// 60s comfortably covers the slowest walk measured (6.0s against a 232-interface switch) while
/// keeping a long-interval check from parking for hours behind a wedged device.
const MAX_SINGLE_FLIGHT_WAIT: Duration = Duration::from_secs(60);

/// Run the poll loop over a stream of jobs. Each job runs concurrently under the
/// [`PollLimiter`]: a global concurrency cap bounds total load and per-device single-flight makes
/// a poll wait for the probe already running against its target, dropping it only if the device is
/// still busy at the deadline (backpressure, monitoring-conventions).
/// Returns when the stream ends. Stream-generic so the same loop drives both the in-memory
/// bus (tests/skeleton) and the NATS queue subscription (production), ADR-003/009.
///
/// 🚨 **This loop awaits the concurrency permit and nothing else.** Everything a *device* can make
/// a job wait for happens inside the spawned task, because a wait on this line is a wait for every
/// job behind it — `a_busy_device_does_not_stall_other_devices` below is what holds that.
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

        // 🚨 **Only the permit is awaited here. The per-device wait is awaited in the task.**
        // Both used to happen on this line, and that made one device's serialised conversation
        // stall every *other* device's job behind it — a fan-out loop that fanned out one job at a
        // time. The permit is the half that belongs here: it is what bounds concurrency, so
        // awaiting it is backpressure rather than head-of-line blocking, and it keeps this from
        // spawning unboundedly. See `PollLimiter::acquire_permit` for the measurement.
        let Some(permit) = limiter.acquire_permit().await else {
            continue; // shutdown
        };
        // DNS monitors share a target by design — many names, one resolver, and every check using
        // the system resolver carries the same 0.0.0.0 display address. Per-target single-flight
        // would therefore drop every DNS check but one on each cycle, so they take the global-only
        // guard for the same reason Meraki collectors do. Pile-up stays bounded by each check's
        // total timeout budget (≤30 s, enforced in the transport) plus the global concurrency cap.
        // Both this and the wait are decided out here because the task takes the job by value.
        let dns = matches!(job.check, CheckSpec::Dns(_));
        let wait = single_flight_wait(&job);
        let limiter = limiter.clone();
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
                // Per-device single-flight, awaited here rather than on the loop: a device still
                // being walked now delays only the jobs aimed at *it*. DNS monitors share the
                // `0.0.0.0` display address (see above), so they hold the permit alone.
                let guard = if dns {
                    Some(limiter.global_guard(permit))
                } else {
                    limiter.claim_target(permit, job.target, wait).await
                };
                // Released (and the target unmarked) when the probe finishes.
                let Some(_guard) = guard else {
                    metrics::counter!("yagra_poll_skipped_backpressure_total").increment(1);
                    tracing::debug!(
                        target = %job.target,
                        "skipping poll: device busy past the deadline"
                    );
                    return;
                };
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
    use crate::worker::testkit::*;
    use uuid::Uuid;
    use yagra_bus::{Bus, InMemoryBus};
    use yagra_common::NodeId;
    use yagra_transport::FakeTransport;

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

    /// **A device that is still being walked stalls its own next spec — and nothing else.**
    ///
    /// The regression for the head-of-line stall described on [`run_stream`]. Both jobs are due at
    /// once and the *blocked* one is first, which is the only ordering that can tell the two
    /// designs apart: with the wait on the loop, `run_stream` parks in `claim_target` for the full
    /// single-flight budget and the second device's job is never even pulled off the stream.
    ///
    /// 🚨 **The 60 s interval is load-bearing.** `single_flight_wait` is `min(interval, 60s)`, so a
    /// short interval here would let the loop clear the blocked job inside the timeout and the test
    /// would pass against the code it exists to reject. Confirmed by reverting the fix: this fails
    /// (the recv times out), and the other two tests in this module do not.
    #[tokio::test]
    async fn a_busy_device_does_not_stall_other_devices() {
        use std::net::Ipv4Addr;
        use yagra_bus::IcmpCheck;

        fn job_at(node: u128, target: Ipv4Addr, interval_secs: u32) -> PollJob {
            PollJob::icmp(
                Uuid::nil(),
                NodeId::from(Uuid::from_u128(node)),
                IpAddr::V4(target),
                IcmpCheck::default(),
                interval_secs,
            )
        }

        let bus = Arc::new(InMemoryBus::new(16));
        let mut results_rx = bus.subscribe_results();
        let transport: Arc<dyn Transport> = Arc::new(FakeTransport::reachable(5.0));
        let limiter = Arc::new(PollLimiter::new(16));

        let busy = Ipv4Addr::new(10, 0, 0, 1);
        let other = Ipv4Addr::new(10, 0, 0, 2);

        // Hold `busy`'s single-flight marker for the whole test: a probe against it is still
        // walking, exactly as a wedged or unreachable device leaves it.
        let held = limiter
            .begin_for(IpAddr::V4(busy), Duration::from_secs(1))
            .await
            .expect("the marker is free before anything else takes it");

        let jobs = vec![job_at(1, busy, 60), job_at(2, other, 60)];
        tokio::spawn(run_stream(
            Box::pin(futures::stream::iter(jobs)),
            crate::store_forward::StoreForwardSink::passthrough(bus.clone()),
            transport,
            limiter.clone(),
            None,
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
        ));

        let result = tokio::time::timeout(Duration::from_secs(5), results_rx.recv())
            .await
            .expect(
                "no result inside 5s: the second device's job never ran while the first was \
                 waiting. That is the head-of-line stall — the loop awaited the per-device \
                 single-flight (up to 60s here) instead of the spawned task doing it",
            )
            .unwrap();
        assert_eq!(
            result.node_id,
            NodeId::from(Uuid::from_u128(2)),
            "the result that came back is the unblocked device's"
        );
        drop(held);
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
            pool: None,
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
}
