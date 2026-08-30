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

/// Where one poll's wall-clock went, in four disjoint phases (ADR-109).
///
/// The poller had **no histogram at all** until this one, so "the poll took 800 ms" and "the poll
/// took 120 ms and spent the rest queueing" were the same observation from outside. The 50,000-node
/// load test needed exactly that distinction and could not have it, and the cost of not having it
/// was a wrong answer rather than no answer: `permits / throughput` was read as a mean hold time,
/// and it is only that when the permits are full — which nobody could have known either way,
/// because the gauge that says so did not exist yet (ADR-109). 🚨 So read this histogram beside
/// `yagra_poll_inflight`, never on its own — a phase that looks slow while the gauge sits below the
/// cap is a poller being starved, not a poller being slow, and the two want opposite fixes.
///
/// 🚨 **`execute + publish` is the permit hold time — and until ADR-109 Increment 2 it was
/// `wait_device + execute + publish`, which is a sentence that cost a wrong measurement the day it
/// was read.** The permit used to be taken on the poll loop and handed into the task, so a job
/// waiting for its device held one; on 15,000 unreachable devices that was **149 of 256 permits
/// occupied by jobs talking to nothing**. `claim_then_permit` now takes the permit only once the
/// device is free, so the four phases are:
///
/// | phase | where | holds a permit |
/// |---|---|---|
/// | `wait_admit` | the poll loop | no — it holds a *spawn* slot (`ADMISSION_FACTOR`) |
/// | `wait_device` | the task, non-DNS | no, bar the instant of the claim |
/// | `wait_permit` | the task, DNS / Meraki only | no — those have no device to wait for |
/// | `execute`, `publish` | the task | **yes** |
///
/// ⚠️ So `permits / throughput` measured from outside is now `execute + publish` per poll, and an
/// unexplained remainder means a phase is missing here rather than that the arithmetic is wrong.
/// ⚠️ `wait_device` and `wait_permit` are mutually exclusive per poll: a check kind is one or the
/// other, never both.
pub(crate) const POLL_PHASE_METRIC: &str = "yagra_poll_phase_seconds";

/// Buckets for [`POLL_PHASE_METRIC`], applied where the exporter is installed.
///
/// The range spans one network round trip (5 ms) to past the single-flight ceiling (30 s), because
/// the distribution this exists to show is **bimodal**: a probe that is answered, and a probe that
/// waits out a 1 s timeout. A scale that blurred those two together would answer the question with
/// an average, which is what the poller already had.
///
/// ⚠️ Named here rather than at the install site so the metric's identity is in one place — a
/// histogram registered under one name and configured under another is a silent downgrade to this
/// exporter's default rolling summary, and a summary's quantiles cannot be added up across a pool.
pub(crate) const POLL_PHASE_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

/// Record one phase of one poll, broken out by the kind of check it was (ADR-110).
///
/// 🚨 **The `kind` label is what makes the histogram readable at all on an SNMP fleet.** Without
/// it, one distribution mixes an ICMP probe (three echoes), a table walk (~40 sequential round
/// trips) and six slow-tier walks that run once an hour, so "the mean poll takes 22.5 seconds" was
/// true, unactionable, and could not say which check spent the time. The label comes from
/// [`CheckSpec::kind_label`], which is the same word core stamps on the dispatch span.
///
/// ⚠️ **A Meraki collect records only its two waits**, under the literal `meraki_collect`: that
/// branch fans one job out to many results, so `execute` and `publish` would not mean what they
/// mean everywhere else. An absent `execute` there says the phase is not recorded, not that the
/// poll was free.
fn record_phase(kind: &'static str, phase: &'static str, since: Instant) {
    metrics::histogram!(POLL_PHASE_METRIC, "kind" => kind, "phase" => phase)
        .record(since.elapsed().as_secs_f64());
}

/// Run the poll loop over a stream of jobs. Each job runs concurrently under the
/// [`PollLimiter`]: a global concurrency cap bounds total load and per-device single-flight makes
/// a poll wait for the probe already running against its target, dropping it only if the device is
/// still busy at the deadline (backpressure, monitoring-conventions).
/// Returns when the stream ends. Stream-generic so the same loop drives both the in-memory
/// bus (tests/skeleton) and the NATS queue subscription (production), ADR-003/009.
///
/// 🚨 **This loop awaits a spawn slot and nothing else** (ADR-109 Inc.2). Everything a *device* or
/// the concurrency cap can make a job wait for happens inside the spawned task, because a wait on
/// this line is a wait for every job behind it — `a_busy_device_does_not_stall_other_devices` below
/// is what holds that, and it fails against either of the two arrangements that came before.
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
            let queued_at = Instant::now();
            let Some(admit) = limiter.acquire_admission().await else {
                continue; // shutdown
            };
            record_phase("meraki_collect", "wait_admit", queued_at);
            let limiter = limiter.clone();
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
                    let _admit = admit;
                    // The permit is taken here rather than on the loop: a Meraki collect has no
                    // device to wait for, but awaiting concurrency on the loop is the same
                    // head-of-line stall for every job behind it (ADR-109 Inc.2).
                    let permit_at = Instant::now();
                    let Some(_guard) = limiter.begin_global().await else {
                        return; // shutdown
                    };
                    record_phase("meraki_collect", "wait_permit", permit_at);
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

        // 🚨 **A spawn slot is awaited here — not the concurrency permit, and not the device.**
        // Three arrangements are ruled out by three measurements, and `PollLimiter` carries all of
        // them: waiting for the *device* here is head-of-line blocking (4.8 jobs/min against 187
        // specs); taking the *permit* here means every job's device wait is spent holding one
        // (149 of 256 permits occupied by jobs talking to nothing, on 15,000 unreachable devices);
        // and awaiting nothing at all spawns a task per due job, which `WorkingSet::due` can hand
        // back tens of thousands of at once. So the loop awaits the one thing that bounds *spawns*
        // and nothing else (ADR-109 Inc.2).
        let queued_at = Instant::now();
        let Some(admit) = limiter.acquire_admission().await else {
            continue; // shutdown
        };
        // Read once, out here: the task takes the job by value, and every phase must carry the same
        // word or one poll would land in two kinds.
        let kind = job.check.kind_label();
        record_phase(kind, "wait_admit", queued_at);
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
                let _admit = admit;
                // Per-device single-flight, awaited here rather than on the loop: a device still
                // being walked now delays only the jobs aimed at *it*. DNS monitors share the
                // `0.0.0.0` display address (see above), so they wait for a permit and nothing else.
                //
                // ⚠️ **`wait_device` now covers the permit wait too, and cannot be split from it.**
                // `claim_then_permit` interleaves the two — it takes a permit only once the device
                // looks free and gives it straight back if it loses the race — so there is no
                // instant to measure between them. What the phase means is unchanged in the way
                // that matters: it is time *outside* the permit, bar the moment of the claim.
                // A DNS or Meraki job has no device, so its wait is recorded as `wait_permit`.
                let claimed_at = Instant::now();
                let guard = if dns {
                    limiter.begin_global().await
                } else {
                    limiter.claim_then_permit(job.target, wait).await
                };
                // Recorded before the branch below, so a job that waited out its whole deadline and
                // was then dropped is in the distribution rather than missing from it — that tail is
                // the reason this phase is measured separately from `execute`.
                record_phase(
                    kind,
                    if dns { "wait_permit" } else { "wait_device" },
                    claimed_at,
                );
                // Released (and the target unmarked) when the probe finishes.
                let Some(_guard) = guard else {
                    metrics::counter!("yagra_poll_skipped_backpressure_total").increment(1);
                    tracing::debug!(
                        target = %job.target,
                        "skipping poll: device busy past the deadline"
                    );
                    return;
                };
                let running = inflight.fetch_add(1, Ordering::Relaxed) + 1;
                // This counter existed and only ever reached the heartbeat. Exported, it answers
                // "are the permits actually occupied?" — from outside, a saturated poller and an
                // idle one holding a huge working set looked identical during the 50,000-node test.
                metrics::gauge!("yagra_poll_inflight").set(running as f64);
                metrics::counter!("yagra_poll_jobs_executed_total").increment(1);
                let probed_at = Instant::now();
                let mut result = execute(&job, transport.as_ref(), now_unix_ms()).await;
                record_phase(kind, "execute", probed_at);
                stamp_poller_id(&mut result, &poller_id);
                // Carry the poll span's context so core's result-ingest span joins this trace.
                result.trace_context = yagra_telemetry::current_trace_context();
                // Store-and-forward: live-publish when connected, else buffer for replay (Phase 3).
                let published_at = Instant::now();
                sink.submit(result).await;
                record_phase(kind, "publish", published_at);
                results_total.fetch_add(1, Ordering::Relaxed);
                let left = inflight.fetch_sub(1, Ordering::Relaxed).saturating_sub(1);
                metrics::gauge!("yagra_poll_inflight").set(left as f64);
            }
            .instrument(span),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::testkit::*;

    /// The phase histogram reaches the exporter under the name and buckets it was configured with.
    ///
    /// 🚨 A metric registered under one name and given buckets under another does not fail: this
    /// exporter silently falls back to a rolling summary, whose quantiles cannot be added up across
    /// a pool — so the fleet-level question would get a per-poller answer that looks fine. Nothing
    /// else in the build compares the two spellings, and they sit in different modules because the
    /// exporter is installed at startup and the metric is recorded in the poll loop.
    #[test]
    fn the_phase_histogram_reaches_the_exporter_under_its_own_name_and_buckets() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new()
            .set_buckets_for_metric(
                metrics_exporter_prometheus::Matcher::Full(POLL_PHASE_METRIC.to_owned()),
                POLL_PHASE_BUCKETS,
            )
            .expect("the shipped buckets are a valid bucket list")
            .build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            record_phase("snmp_table", "execute", Instant::now());
        });
        let rendered = handle.render();

        assert!(
            rendered.contains("yagra_poll_phase_seconds_bucket"),
            "no histogram buckets rendered — the name the exporter was configured with and the \
             name `record_phase` writes to have diverged, and this metric is now a summary:\n\
             {rendered}"
        );
        assert!(
            rendered.contains("phase=\"execute\""),
            "the phase label did not survive to the exporter:\n{rendered}"
        );
        // 🚨 The kind label is the half that makes the histogram readable on an SNMP fleet
        // (ADR-110): without it one distribution mixes a three-echo ICMP probe with a
        // ~40-round-trip table walk and six hourly walks, and no amount of reading it says which
        // one was slow. The word comes from `CheckSpec::kind_label`, so it is the same word core
        // put in the dispatch span.
        assert!(
            rendered.contains("kind=") && rendered.contains("snmp_table"),
            "the kind label did not survive to the exporter:
{rendered}"
        );
        for edge in ["0.005", "1", "30"] {
            assert!(
                rendered.contains(&format!("le=\"{edge}\"")),
                "bucket edge {edge} is missing; POLL_PHASE_BUCKETS is not what was applied:\n\
                 {rendered}"
            );
        }
    }
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
    /// designs apart: with the wait on the loop, `run_stream` parks in `claim_then_permit` for the full
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
