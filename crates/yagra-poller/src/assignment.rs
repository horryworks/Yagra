// SPDX-License-Identifier: AGPL-3.0-only
//! What core has assigned this poller — kept current, and turned into jobs (ADR-009/020).
//!
//! Two loops, one shared [`WorkingSet`]. The first folds core's syncs into it and asks for a fresh
//! snapshot whenever a gap or an epoch mismatch says the set can no longer be trusted; the second
//! ticks at the working set's own quantum, pops what is due, and feeds the worker over a bounded
//! channel. Between them they are the only writers and the only reader of that set.
//!
//! ⚠️ **The set itself is `working_set.rs`, and it stays pure.** That module's doc says it in as many
//! words — a state machine with *no I/O*, clock and jitter injected, measured at zero `.await`. The
//! two loops here publish to the bus and drive a timer, so putting them there would break the only
//! property that module claims. ADR-099 refused the same move into `optical.rs` and its siblings for
//! the same reason. The division to keep in mind: **`working_set.rs` decides what is due, this file
//! decides when to ask.**

use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::mpsc;
use uuid::Uuid;
use yagra_bus::{NatsBus, PollJob, SyncBus, SyncMsg, SyncRequest};
use yagra_telemetry::{spawn_cancellable, CancellationToken};

use crate::pool::PoolState;
use crate::working_set::{self, ApplyOutcome, WorkingSet};
use crate::PollerIdentity;

/// How many jobs may wait between the local scheduler and the worker loop.
///
/// Backpressure rather than loss: a full channel makes the scheduler await, and it never drops
/// (see [`run_local_scheduler`]).
const JOB_CHANNEL_DEPTH: usize = 256;

/// Publish what this poller's working set holds, and what it asks for (ADR-108 Inc.2).
///
/// **One function because the two are one fact.** `yagra_working_set_specs` was set from here *and*
/// from `heartbeat.rs`, and a demand gauge added beside only one of them would read stale for up to
/// a whole beat after every sync — or be forgotten at the second site altogether, which is the
/// shape this repository keeps paying for. `guards.rs` pins the metric names to this function.
///
/// ⚠️ The two answer different faults. The spec count says what core handed this poller; the demand
/// says what that work costs per second. A poller starved of assignments and a poller unable to
/// serve them look identical in either gauge alone.
///
/// Here rather than in `working_set.rs` for the reason ADR-108 decision 2 gives: that module is a
/// pure state machine with no I/O, so it owns the number and this file owns the meter.
pub(crate) fn publish_working_set_gauges(ws: &WorkingSet) {
    let (_, specs) = ws.stats();
    metrics::gauge!("yagra_working_set_specs").set(f64::from(specs));
    metrics::gauge!("yagra_poll_demand_per_second").set(ws.demand_per_sec());
}

/// Subscribe, ask for a snapshot, and start both loops. Returns the receiving half of the job
/// channel, which the caller merges with the bus-delivered jobs into the worker's single stream.
///
/// **Subscribe first, then request** (ADR-020): the reply arrives as chunks on this poller's
/// assignment subject rather than as a request-reply, so asking before subscribing can miss it.
pub(crate) async fn start(
    bus: &Arc<NatsBus>,
    identity: &PollerIdentity,
    pool: &Arc<PoolState>,
    working_set: &Arc<Mutex<WorkingSet>>,
    shutdown: &CancellationToken,
) -> anyhow::Result<mpsc::Receiver<PollJob>> {
    let sync_sub = Box::pin(bus.subscribe_sync(&identity.id).await?);
    let initial = SyncRequest {
        poller_id: identity.id.clone(),
        pool: pool.current(),
        incarnation: identity.incarnation,
    };
    if let Err(e) = bus.publish_sync_request(initial).await {
        tracing::warn!(error = %e, "failed to publish initial sync request");
    }
    spawn_cancellable(
        shutdown,
        run_sync_loop(
            sync_sub,
            working_set.clone(),
            bus.clone(),
            identity.id.clone(),
            pool.clone(),
            identity.incarnation,
        ),
    );

    let (jobs_tx, jobs_rx) = mpsc::channel::<PollJob>(JOB_CHANNEL_DEPTH);
    spawn_cancellable(shutdown, run_local_scheduler(working_set.clone(), jobs_tx));
    Ok(jobs_rx)
}

/// Consume this poller's working-set syncs, folding each into the shared [`WorkingSet`]. On a gap /
/// epoch mismatch ([`ApplyOutcome::NeedSync`]) it asks core for a fresh snapshot; on a successful
/// apply it advances the sync metrics and the specs gauge. Runs until the stream ends.
async fn run_sync_loop<B, S>(
    mut sync: S,
    working_set: Arc<Mutex<WorkingSet>>,
    bus: Arc<B>,
    poller_id: String,
    pool: Arc<PoolState>,
    incarnation: Uuid,
) where
    B: SyncBus + 'static,
    S: futures::Stream<Item = SyncMsg> + Unpin,
{
    use futures::stream::StreamExt;
    use rand::Rng;
    // Zero-capture jitter source (Send across the await points), guarded so it never gets a 0 bound.
    let mut rng = |bound: u32| {
        if bound == 0 {
            0
        } else {
            rand::thread_rng().gen_range(0..bound)
        }
    };
    while let Some(msg) = sync.next().await {
        let is_snapshot = matches!(msg, SyncMsg::SnapshotChunk(_));
        // Core's answer to "which pool is this poller in" rides the snapshot (ADR-107 Inc.2), so
        // it is read here rather than in `working_set.rs` — that module is a pure state machine
        // with no I/O, and adopting a pool reconnects the bus. `None` from an N-1 core leaves the
        // pool exactly as the environment set it, which is the pre-Inc.2 behaviour.
        //
        // Read **before** the apply: the snapshot's nodes are the new pool's, so a reader watching
        // both would otherwise see this poller holding another pool's work for an instant.
        if let SyncMsg::SnapshotChunk(s) = &msg {
            if let Some(assigned) = s.pool.clone() {
                pool.adopt(&assigned).await;
            }
        }
        let outcome = {
            let mut ws = working_set.lock().expect("working set mutex poisoned");
            ws.apply(msg, Instant::now(), &mut rng)
        };
        match outcome {
            ApplyOutcome::Applied => {
                if is_snapshot {
                    metrics::counter!("yagra_sync_snapshots_total").increment(1);
                } else {
                    metrics::counter!("yagra_sync_deltas_total").increment(1);
                }
                publish_working_set_gauges(
                    &working_set.lock().expect("working set mutex poisoned"),
                );
            }
            ApplyOutcome::NeedSync => {
                metrics::counter!("yagra_sync_gaps_total").increment(1);
                tracing::info!("working-set gap/epoch mismatch — requesting a fresh snapshot");
                let req = SyncRequest {
                    poller_id: poller_id.clone(),
                    pool: pool.current(),
                    incarnation,
                };
                if let Err(e) = bus.publish_sync_request(req).await {
                    tracing::warn!(error = %e, "failed to publish sync request");
                }
            }
            ApplyOutcome::Ignored => {}
        }
    }
    tracing::warn!("working-set sync stream ended");
}

/// Tick every [`working_set::SCHEDULER_TICK`], popping the due specs from the working set and
/// forwarding each as a job to the worker loop over a bounded channel (backpressure: a full channel
/// awaits, it never drops). Stops when the channel closes (worker gone).
///
/// The period is the working set's constant, not a literal here: it spaces a node's specs by whole
/// ticks so they cannot collide in the worker's per-device single-flight guard, which only holds
/// while this timer runs at that period.
async fn run_local_scheduler(working_set: Arc<Mutex<WorkingSet>>, jobs_tx: mpsc::Sender<PollJob>) {
    let mut tick = tokio::time::interval(working_set::SCHEDULER_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let (due, missed) = {
            let mut ws = working_set.lock().expect("working set mutex poisoned");
            let due = ws.due(Instant::now());
            // Drained under the same lock as the walk that produced it, so no tick's tally can be
            // attributed to the next one.
            (due, ws.take_cycles_missed())
        };
        // The polling deficit, which had no instrument until the 50,000-node load test went looking
        // for one (2026-08-29): two pollers served 587 of the 1,673 polls/s their intervals asked
        // for and every existing counter read healthy, because almost nothing was *dropped* (the
        // shed counter moved by a few hundred against 930,000 executed) — the loop below
        // back-pressures on a bounded channel and the cycle silently stretches instead.
        // 🚨 This loop was the prime suspect for *why*, and it was measured and cleared (ADR-109).
        // One poller carrying 50,000 nodes on that box now serves 1,675 polls/s — the whole of what
        // a 30-second interval asks for — while this counter stays at 0; drop the permits to 64 and
        // the same poller yields exactly 532 polls/s, which is 64 divided by the 120.6 ms one poll
        // takes, and this counter fires ~95,000 per 30 s. So the ceiling is the permit count and
        // nothing else, and the tick below is not in the way at either end.
        // ⚠️ What is still unexplained is the load test's own number: two pollers should have made
        // ~1,064 polls/s at 64 permits each and made 587. Do not treat this loop as cleared *for
        // that run* — the differences (a third, co-located poller in the pool; 6 vCPU rather than
        // 3) were never isolated.
        // ⚠️ Distinct from `yagra_poll_skipped_backpressure_total`, which counts a job that WAS
        // dispatched and then dropped at the device single-flight guard.
        if missed > 0 {
            metrics::counter!("yagra_poll_cycles_missed_total").increment(missed);
        }
        for job in due {
            if jobs_tx.send(job).await.is_err() {
                tracing::warn!("worker channel closed — stopping local scheduler");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use yagra_bus::{IcmpCheck, JobSpec, NodeJobs, WorkingSetSnapshot};
    use yagra_common::NodeId;

    fn spec(node: NodeId, interval: u32) -> JobSpec {
        JobSpec::from_job(&PollJob::icmp(
            Uuid::nil(),
            node,
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IcmpCheck::default(),
            interval,
        ))
    }

    /// Both names reach the exporter from one call, carrying what the set actually holds.
    ///
    /// `guards.rs` says the two gauges are published from one place; this says that place publishes
    /// both, with real numbers. Neither implies the other — a function naming both metrics can
    /// still set one of them and forget the second, which is the state this crate was in.
    #[test]
    fn both_working_set_gauges_reach_the_exporter() {
        let n = NodeId::from(Uuid::from_u128(1));
        let mut ws = WorkingSet::new();
        let mut rng = |_bound: u32| 0;
        let applied = ws.apply(
            SyncMsg::SnapshotChunk(WorkingSetSnapshot {
                poller_id: "p1".to_owned(),
                epoch: Uuid::from_u128(9),
                seq: 1,
                chunk_index: 0,
                chunk_total: 1,
                nodes: vec![NodeJobs {
                    node_id: n,
                    specs: vec![spec(n, 10), spec(n, 40)],
                }],
                total_nodes: 1,
                pool: None,
            }),
            Instant::now(),
            &mut rng,
        );
        assert_eq!(applied, ApplyOutcome::Applied, "the fixture must land");

        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || publish_working_set_gauges(&ws));
        let rendered = handle.render();

        assert!(
            rendered.contains("yagra_working_set_specs 2"),
            "the spec count is missing from:\n{rendered}"
        );
        // 1/10 + 1/40. Asserted as a rendered value rather than a float comparison, because what
        // an operator reads is this text.
        assert!(
            rendered.contains("yagra_poll_demand_per_second 0.125"),
            "the demand gauge is missing or wrong in:\n{rendered}"
        );
    }
}
