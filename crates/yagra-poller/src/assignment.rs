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

use crate::working_set::{self, ApplyOutcome, WorkingSet};
use crate::PollerIdentity;

/// How many jobs may wait between the local scheduler and the worker loop.
///
/// Backpressure rather than loss: a full channel makes the scheduler await, and it never drops
/// (see [`run_local_scheduler`]).
const JOB_CHANNEL_DEPTH: usize = 256;

/// Subscribe, ask for a snapshot, and start both loops. Returns the receiving half of the job
/// channel, which the caller merges with the bus-delivered jobs into the worker's single stream.
///
/// **Subscribe first, then request** (ADR-020): the reply arrives as chunks on this poller's
/// assignment subject rather than as a request-reply, so asking before subscribing can miss it.
pub(crate) async fn start(
    bus: &Arc<NatsBus>,
    identity: &PollerIdentity,
    working_set: &Arc<Mutex<WorkingSet>>,
    shutdown: &CancellationToken,
) -> anyhow::Result<mpsc::Receiver<PollJob>> {
    let sync_sub = Box::pin(bus.subscribe_sync(&identity.id).await?);
    let initial = SyncRequest {
        poller_id: identity.id.clone(),
        pool: identity.pool.clone(),
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
            identity.pool.clone(),
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
    pool: String,
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
                let (_, specs) = working_set
                    .lock()
                    .expect("working set mutex poisoned")
                    .stats();
                metrics::gauge!("yagra_working_set_specs").set(f64::from(specs));
            }
            ApplyOutcome::NeedSync => {
                metrics::counter!("yagra_sync_gaps_total").increment(1);
                tracing::info!("working-set gap/epoch mismatch — requesting a fresh snapshot");
                let req = SyncRequest {
                    poller_id: poller_id.clone(),
                    pool: pool.clone(),
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
        let due = {
            let mut ws = working_set.lock().expect("working set mutex poisoned");
            ws.due(Instant::now())
        };
        for job in due {
            if jobs_tx.send(job).await.is_err() {
                tracing::warn!("worker channel closed — stopping local scheduler");
                return;
            }
        }
    }
}
