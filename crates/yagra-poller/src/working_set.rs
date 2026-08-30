// SPDX-License-Identifier: AGPL-3.0-only
//! The poller's local working set — a pure state machine (ADR-009/020).
//!
//! In the distributed-poller model core no longer publishes every [`PollJob`] each tick. It hands
//! each poller the set of polling *specs* it owns as a full **snapshot** (chunked) plus incremental
//! **deltas**, and the poller schedules them locally. This module owns that set and the sync
//! protocol that keeps it consistent — with **no I/O**: [`WorkingSet::apply`] folds a [`SyncMsg`]
//! into the set and reports whether the poller needs to resync, [`WorkingSet::due`] pops as many
//! of the specs whose local timers have fired as the caller has room for — ranked by how late each
//! is relative to its own interval (ADR-109 Inc.4) — and mints fresh jobs. Clock (`now`) and jitter
//! source (`rng`) are injected so every rule here is deterministically unit-testable without a bus,
//! a clock, or real randomness.
//!
//! Ordering & gap detection (ADR-020): syncs for one poller travel on a single ordered subject, so
//! `seq` gates them. A delta must land exactly at `last_seq + 1` on the current `epoch`; a gap or an
//! epoch mismatch yields [`ApplyOutcome::NeedSync`] (the caller asks core for a fresh snapshot) and
//! **never** mutates the set. A snapshot replaces the whole set and re-anchors `epoch`/`last_seq`.
//!
//! Anti-stampede (monitoring-conventions): a newly-scheduled spec gets `next_due = now + jitter`,
//! but a spec that survives a snapshot unchanged **keeps its previous phase** so a resync doesn't
//! bunch every poll onto the same tick.
//!
//! **The jitter window is sized by a job rate, not by the poll interval, and that distinction is the
//! whole point.** Until v0.2.3 a new spec was jittered uniformly over `[0, interval)`. That is
//! correct for a cold start, and wrong for the case it also covered: a node **handed over** from
//! another poller (failover, scale-out, a rolling upgrade) is indistinguishable here from a
//! brand-new one, so it was re-phased from zero — the previous owner stopped polling it at most one
//! interval ago, and this poller would wait up to another full interval, for a worst-case
//! **2 × interval** between consecutive samples. The pool still had a live poller the whole time, so
//! nothing alerted: `pool_coverage` only fires at zero live pollers, and `monitoring_gaps` records
//! core↔poller visibility, not a missed poll. ADR-023 requires that a rolling upgrade or a
//! reassignment "leaves no polling hole and loses no data", so this was an unmet promise rather than
//! a missing feature.
//!
//! The fix keeps the anti-stampede property but states it directly: newly adopted specs are spread
//! over **how long it takes to poll them once at a sustained rate**
//! ([`WorkingSet::with_adopt_rate`]), clamped to their own interval. Adopting 50 specs takes a
//! fraction of a second; a 25,000-spec cold start still clamps to the interval and behaves exactly
//! as before. The window is a floor no schedule can beat, so this is not a trade against the
//! stampede rule — it is the same rule expressed in the units that actually bound it (ADR-051).
//!
//! **⚠️ That window is also why a node's specs must be placed, not drawn — the interaction cost two
//! days of interface data on the test server (2026-08-11 → 08-13) and shipped in v0.2.3–v0.2.5.**
//! Three mechanisms meet here. [`Self::due`] hands the scheduler everything that fired in one
//! [`SCHEDULER_TICK`] as a single burst; the worker's per-device single-flight guard
//! (`limiter::PollLimiter`) lets exactly **one** probe per target IP run and **drops** the rest
//! rather than deferring them; and [`Self::due`] advances a dropped spec's timer anyway, so the
//! phase relationship between a node's specs never changes once set. Draw those phases from a 70 ms
//! window — which is what `14 specs / 200 per second` yields on a small deployment — and every spec
//! of a node lands in the same tick, so the one that sorts first wins **forever** and the rest are
//! discarded on every cycle. On the test server that left a Huawei firewall reporting only
//! `snmp_sys_uptime_ticks`: the ifTable walk, the vendor tables and even ICMP were dropped every
//! minute, the node stayed `ok` because the surviving scalar *is* the liveness check, and nothing
//! alerted. Note the direction — the *smaller* the deployment the narrower the window, so this hurt
//! first deployments worst and a 25,000-spec fleet not at all.
//!
//! So [`settle`] no longer draws an offset per spec. It draws **one** offset per node — which is
//! what anti-stampede actually asks for, since spreading is between *nodes* — and then steps its
//! specs [`SPEC_STAGGER_MS`] apart by position. The window keeps its ADR-051 meaning untouched; only
//! the placement inside it changed. Separation becomes arithmetic rather than luck: consecutive
//! specs differ by a fixed amount, and because [`WorkingSet::due`] re-arms each by a whole interval,
//! that difference is preserved for the life of the spec.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use uuid::Uuid;
use yagra_bus::{JobSpec, NodeJobs, PollJob, SyncMsg, WorkingSetDelta, WorkingSetSnapshot};
use yagra_common::NodeId;

/// How often the poller's local scheduler asks [`WorkingSet::due`] what has fired
/// (`assignment.rs`'s `run_local_scheduler` owns the timer and reads this constant rather than
/// repeating the literal — ADR-103 moved it out of `main.rs`, and it is there rather than here
/// because this module keeps no timer and does no I/O).
///
/// It is **the** quantum of this module: two specs due within one tick of each other are due *at the
/// same time* as far as anything downstream can tell, which is why [`settle`] spaces a node's specs
/// by whole ticks. The two must stay one fact — a scheduler ticking slower than the spacing would
/// silently restore the collision the spacing exists to prevent.
pub const SCHEDULER_TICK: Duration = Duration::from_millis(TICK_MS as u64);

/// [`SCHEDULER_TICK`] in milliseconds. Declared first so the two cannot disagree.
const TICK_MS: u32 = 500;

/// How far apart [`settle`] places consecutive specs of the **same node**.
///
/// One tick is the minimum that separates them, because [`WorkingSet::due`] resolves at tick
/// granularity. The second tick is margin: the scheduler's timer uses
/// `MissedTickBehavior::Delay`, so a tick that arrives late (the worker channel was full, the
/// process was descheduled) can sweep up two specs that are exactly one tick apart. Two ticks
/// survives a late tick and still costs a node's last spec well under a second of adoption delay.
const SPEC_STAGGER_MS: u32 = 2 * TICK_MS;

// The stagger is measured in ticks, and the compiler is the right place to say so: a stagger below
// one tick would separate the offsets while leaving both jobs in the same `due` burst — separation
// nothing downstream could observe, which is the failure this constant exists to prevent.
const _: () = assert!(SPEC_STAGGER_MS >= TICK_MS);
const _: () = assert!(SPEC_STAGGER_MS.is_multiple_of(TICK_MS));

/// Specs per second a poller is willing to take on when it adopts work from another poller. Sizes
/// the adoption window (see the module docs); it is a spreading rate, not a throttle — nothing
/// enforces it downstream, and `YAGRA_MAX_CONCURRENT_POLLS` is what actually bounds concurrency.
///
/// 200/s means a 2-poller pool of 10,000 specs hands over its half in 25s, comfortably inside the
/// 300s `pool_coverage` debounce, while a full-fleet cold start clamps to the interval as before.
pub const DEFAULT_ADOPT_RATE_PER_SEC: u32 = 200;

/// One scheduled polling spec: the reusable [`JobSpec`] plus the local time its next poll is due.
struct ScheduledSpec {
    spec: JobSpec,
    next_due: Instant,
}

/// One spec part-way through an adoption: `next_due` is `None` while it is still unknown how many
/// specs this apply is adopting in total, because that count is what sizes the window they spread
/// over. A spec that kept its previous phase already has its `Some`.
struct PendingSpec {
    spec: JobSpec,
    next_due: Option<Instant>,
}

/// One spec whose timer has fired, and how late it is **relative to its own interval** — the number
/// [`WorkingSet::due`] ranks a scarce budget on (ADR-109 Increment 4).
///
/// The lateness is kept as the pair `behind_ms / interval_ms` rather than as a ratio, so the
/// ordering is total and exact: `f64` has no `Ord`, and a ratio computed in floating point would
/// make the boundary of a truncated batch depend on rounding. Comparing two is one
/// cross-multiplication, and `u128` has room for it — the largest product in play is a process
/// uptime in milliseconds times an hour in milliseconds.
struct Candidate {
    node: NodeId,
    index: usize,
    behind_ms: u128,
    interval_ms: u128,
}

/// Order two due specs so the one that has waited longest **as a fraction of its own interval**
/// comes first.
///
/// That fraction is what makes the ranking mean "keep the configured ratio". A 60 s check one
/// minute late has waited a whole cycle; a 3600 s check one minute late has waited 1/60th of one,
/// and the second is the one that can afford to wait. Under sustained scarcity the fractions
/// equalise across the whole set, which reproduces the demand ratio the intervals declare.
///
/// 🚨 **The tie-break is not cosmetic.** Specs that have only just come due are all at `behind = 0`,
/// so without a deterministic second key the boundary of a truncated batch would be decided by
/// `HashMap` iteration order — which is seeded per process, so the same fleet would be served
/// differently after every restart and no test could pin it.
fn later_first(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    (b.behind_ms * a.interval_ms)
        .cmp(&(a.behind_ms * b.interval_ms))
        .then_with(|| a.node.cmp(&b.node))
        .then_with(|| a.index.cmp(&b.index))
}

/// A snapshot being reassembled from its chunks, keyed by `(epoch, seq)`. Chunks of one snapshot
/// share a single `(epoch, seq)`; only the newest such group is ever buffered.
struct PendingSnapshot {
    epoch: Uuid,
    seq: u64,
    chunk_total: u32,
    /// chunk_index → the chunk's nodes. A `HashMap` makes duplicate indices idempotent and tolerates
    /// out-of-order arrival.
    chunks: HashMap<u32, Vec<NodeJobs>>,
}

/// What applying a [`SyncMsg`] did to the working set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// The set moved forward: a delta applied, or a snapshot fully assembled and replaced the set.
    Applied,
    /// A gap / epoch mismatch was detected; the set was **not** mutated and the poller should
    /// request a fresh snapshot.
    NeedSync,
    /// Nothing actionable happened: a stale/duplicate message, or a snapshot chunk buffered while
    /// its siblings are still in flight.
    Ignored,
}

/// The poller's authoritative set of polling specs and its sync-protocol state (ADR-020).
pub struct WorkingSet {
    /// node → its scheduled specs.
    nodes: HashMap<NodeId, Vec<ScheduledSpec>>,
    /// The core-process epoch this set belongs to (`None` before the first snapshot).
    epoch: Option<Uuid>,
    /// Highest sync sequence applied.
    last_seq: u64,
    /// A snapshot currently being reassembled from its chunks.
    pending: Option<PendingSnapshot>,
    /// Specs per second used to size the adoption window. `0` disables the cap, restoring the
    /// pre-v0.2.3 behaviour of spreading a new spec across its whole interval.
    adopt_rate_per_sec: u32,
    /// Poll cycles that came and went while this poller was too far behind to serve them
    /// ([`Self::due`] re-anchors past them), drained by [`Self::take_cycles_missed`].
    ///
    /// **This is the only place a poller knows it is not keeping up, and until the 50,000-node
    /// load test of 2026-08-29 it was computed and thrown away.** Two pollers carrying 50,000
    /// nodes served 587 of the 1,673 polls/s their intervals asked for — a 60-second effective
    /// interval against a configured 30 — while every counter read healthy, CPU sat under 5% and
    /// nothing was logged. Nothing was *dropped*, which is why no drop counter moved: the
    /// scheduler back-pressures on a bounded channel and the cycle simply stretches.
    ///
    /// ⚠️ **Not the same shortfall as `yagra_poll_skipped_backpressure_total`**, and the two must
    /// not be read as one number. That counter is a job that *was* dispatched and then dropped at
    /// the device single-flight guard. This one is a job that was never dispatched, because its
    /// turn passed while the poller was still working through an earlier one.
    ///
    /// A count, not a rate, and deliberately in the pure half: `assignment.rs` owns the meter.
    cycles_missed: u64,

    /// How many specs were due at the last [`Self::due`] call and did not get a job.
    ///
    /// A level rather than a tally, so it is read and not drained — see [`Self::deferred`].
    deferred: u32,
}

impl Default for WorkingSet {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkingSet {
    /// An empty working set with no epoch and no pending snapshot, adopting at
    /// [`DEFAULT_ADOPT_RATE_PER_SEC`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_adopt_rate(DEFAULT_ADOPT_RATE_PER_SEC)
    }

    /// As [`Self::new`], with an explicit adoption rate in specs per second (`YAGRA_ADOPT_RATE_PER_SEC`).
    ///
    /// `0` is the escape hatch: it disables the window entirely, so every newly adopted spec is
    /// jittered across its full interval exactly as builds before v0.2.3 did.
    #[must_use]
    pub fn with_adopt_rate(adopt_rate_per_sec: u32) -> Self {
        Self {
            nodes: HashMap::new(),
            epoch: None,
            last_seq: 0,
            pending: None,
            adopt_rate_per_sec,
            cycles_missed: 0,
            deferred: 0,
        }
    }

    /// Take the poll cycles missed since the last call, resetting the tally.
    ///
    /// Drained rather than read so the caller can hand the delta straight to a counter without
    /// keeping a previous value; a snapshot replacing the whole set does **not** clear it, because
    /// this records what this *process* failed to serve, not what the set currently contains.
    pub fn take_cycles_missed(&mut self) -> u64 {
        std::mem::take(&mut self.cycles_missed)
    }

    /// The window, in milliseconds, that `added` newly adopted specs spread over: how long polling
    /// them once takes at the sustained rate. `u32::MAX` when the cap is off, leaving the per-spec
    /// clamp to its own interval as the only bound.
    fn adopt_window_ms(&self, added: usize) -> u32 {
        if self.adopt_rate_per_sec == 0 {
            return u32::MAX;
        }
        let ms = (added as u64).saturating_mul(1000) / u64::from(self.adopt_rate_per_sec);
        u32::try_from(ms).unwrap_or(u32::MAX)
    }

    /// Fold one [`SyncMsg`] into the set. `now` is the local clock; `rng(bound)` must return a value
    /// uniformly in `[0, bound)` and is only called with `bound > 0` (the jitter source). Returns
    /// what happened (see [`ApplyOutcome`]).
    pub fn apply(
        &mut self,
        msg: SyncMsg,
        now: Instant,
        rng: &mut impl FnMut(u32) -> u32,
    ) -> ApplyOutcome {
        match msg {
            SyncMsg::SnapshotChunk(snap) => self.apply_snapshot_chunk(snap, now, rng),
            SyncMsg::Delta(delta) => self.apply_delta(delta, now, rng),
        }
    }

    /// Apply an incremental delta. Requires the current epoch and `seq == last_seq + 1`; otherwise
    /// nothing is mutated and [`ApplyOutcome::NeedSync`] is returned (ADR-020).
    fn apply_delta(
        &mut self,
        delta: WorkingSetDelta,
        now: Instant,
        rng: &mut impl FnMut(u32) -> u32,
    ) -> ApplyOutcome {
        if self.epoch != Some(delta.epoch) {
            return ApplyOutcome::NeedSync; // wrong / absent epoch → resync
        }
        if delta.seq != self.last_seq + 1 {
            return ApplyOutcome::NeedSync; // gap in the stream → resync
        }
        // Two passes: donate surviving phases first so the count of genuinely new specs is known,
        // then size the adoption window from that count and settle the newcomers into it. Staging is
        // equivalent to inserting per node — the nodes are independent.
        let mut staged: Vec<(NodeId, Vec<PendingSpec>)> = Vec::with_capacity(delta.upserts.len());
        let mut added = 0usize;
        for nj in delta.upserts {
            let pending = reschedule(self.nodes.get(&nj.node_id), nj.specs);
            added += pending.iter().filter(|p| p.next_due.is_none()).count();
            staged.push((nj.node_id, pending));
        }
        let window_ms = self.adopt_window_ms(added);
        for (node_id, pending) in staged {
            self.nodes
                .insert(node_id, settle(pending, now, window_ms, rng));
        }
        for node in delta.removes {
            self.nodes.remove(&node);
        }
        self.last_seq = delta.seq;
        ApplyOutcome::Applied
    }

    /// Buffer one snapshot chunk; when every chunk of its `(epoch, seq)` group has arrived, replace
    /// the whole set. A chunk for a group older than one already applied, or older than the group
    /// currently buffering, is ignored (ADR-020).
    fn apply_snapshot_chunk(
        &mut self,
        snap: WorkingSetSnapshot,
        now: Instant,
        rng: &mut impl FnMut(u32) -> u32,
    ) -> ApplyOutcome {
        // Already applied a snapshot of this epoch at this seq or newer → stale, discard.
        if self.epoch == Some(snap.epoch) && snap.seq <= self.last_seq {
            return ApplyOutcome::Ignored;
        }

        let same_group =
            matches!(&self.pending, Some(p) if p.epoch == snap.epoch && p.seq == snap.seq);
        if !same_group {
            // A different group arrived: keep only the newest. Within one epoch a higher seq wins; a
            // different epoch means core restarted, so treat the incoming group as newer.
            let supersede = match &self.pending {
                None => true,
                Some(p) if p.epoch == snap.epoch => snap.seq > p.seq,
                Some(_) => true,
            };
            if !supersede {
                return ApplyOutcome::Ignored; // incoming chunk is older than what we're assembling
            }
            self.pending = Some(PendingSnapshot {
                epoch: snap.epoch,
                seq: snap.seq,
                chunk_total: snap.chunk_total,
                chunks: HashMap::new(),
            });
        }

        let Some(pending) = self.pending.as_mut() else {
            return ApplyOutcome::Ignored; // unreachable: set to Some just above
        };
        // Duplicate index overwrites the same data → idempotent.
        pending.chunks.insert(snap.chunk_index, snap.nodes);
        if pending.chunks.len() < pending.chunk_total as usize {
            return ApplyOutcome::Ignored; // still assembling
        }

        let Some(group) = self.pending.take() else {
            return ApplyOutcome::Ignored;
        };
        self.replace_from_group(group, now, rng);
        ApplyOutcome::Applied
    }

    /// Replace the whole set from a fully-assembled snapshot group, preserving the phase of any spec
    /// that survives unchanged so a resync doesn't restart every timer at once.
    fn replace_from_group(
        &mut self,
        group: PendingSnapshot,
        now: Instant,
        rng: &mut impl FnMut(u32) -> u32,
    ) {
        let old = std::mem::take(&mut self.nodes);
        let mut staged: Vec<(NodeId, Vec<PendingSpec>)> = Vec::new();
        let mut added = 0usize;
        for node_jobs in group.chunks.into_values().flatten() {
            let pending = reschedule(old.get(&node_jobs.node_id), node_jobs.specs);
            added += pending.iter().filter(|p| p.next_due.is_none()).count();
            staged.push((node_jobs.node_id, pending));
        }
        // A cold start (empty `old`) makes every spec new, so the window clamps to the interval and
        // this is byte-for-byte the previous behaviour. A resync after a reassignment donates most
        // phases, so `added` is small and the newcomers land promptly.
        let window_ms = self.adopt_window_ms(added);
        let mut new_nodes: HashMap<NodeId, Vec<ScheduledSpec>> =
            HashMap::with_capacity(staged.len());
        for (node_id, pending) in staged {
            new_nodes.insert(node_id, settle(pending, now, window_ms, rng));
        }
        self.nodes = new_nodes;
        self.epoch = Some(group.epoch);
        self.last_seq = group.seq;
    }

    /// Pop up to `budget` specs whose timers have fired at/by `now`, minting a fresh [`PollJob`] id
    /// for each and re-arming it one interval ahead.
    ///
    /// If a spec has fallen more than a full interval behind (e.g. the poller was paused through a
    /// WAN blip), its timer re-anchors to `now + interval` rather than emitting a backlog burst — at
    /// most one make-up poll per spec per call (catch-up cap).
    ///
    /// # The budget, and what it is for (ADR-109 Increment 4)
    ///
    /// **`budget >= the number of specs that are due` is byte-for-byte the old behaviour**: every
    /// due spec is minted and re-armed, in whatever order they are found. A poller that is keeping
    /// up is always in that case, so nothing below changes what a healthy deployment does.
    ///
    /// The other case is the one this exists for. Before it, `due` returned **everything** that was
    /// due, unbounded, and `assignment.rs` pushed that whole batch into a 256-deep channel before it
    /// looked at the clock again. On 15,000 devices with eight checks each that is a 120,000-job
    /// batch, walked node by node — and a node's specs are adjacent — so a check configured every
    /// 3600 s was dispatched **as often as** one configured every 60 s. Measured 2026-08-30: a
    /// demand ratio of 60 : 1 served at 1.03 : 1, with the four hourly checks eating **65.8%** of
    /// the concurrency budget while ICMP got 3.1%.
    ///
    /// 🚨 **A spec that loses keeps its `next_due`.** That is the whole of the fairness argument and
    /// the one line that must not be "tidied": advancing a deferred spec's timer would stop its
    /// lateness accumulating, so a long-interval check could never out-rank a short one and would
    /// never run again.
    ///
    /// **What that buys, stated exactly.** In steady state the marginal served spec of each tier has
    /// the same `behind / interval`, which makes each tier's service rate proportional to its
    /// demand — every check stretched by the same factor, which is the configured ratio preserved.
    /// ⚠️ **But the long tier's polls arrive in a clump, and the first clump is one *stretched*
    /// interval away.** A starved 3600 s check has to accumulate `3600 × stretch` seconds of
    /// lateness before it out-ranks a 60 s check, and every one of them crosses that line at about
    /// the same moment. Simulated at 1,500 nodes × 8 specs and 14× over-subscription: the hourly
    /// tier receives **nothing for 12.6 hours**, then all of it inside three hours, then nothing
    /// again for about thirteen — while the 60 s tier runs every 833 s, stretched 13.9×. So a
    /// window shorter than the stretched interval reads the long tier as starved, and on a fleet
    /// this far past its capacity that reading is operationally true even though the schedule is
    /// proportional.
    ///
    /// ⚠️ It reorders; it does not create capacity. 15,000 devices × 8 checks ask for 2,817
    /// permit-seconds per second and 256 permits cannot serve 9% of that. What changes is *which*
    /// polls the budget buys — and at that depth, what it buys is liveness.
    pub fn due(&mut self, now: Instant, budget: usize) -> Vec<PollJob> {
        // Pass 1 — who is due, and how late relative to their own interval. Read-only, so the whole
        // set can be walked before anything is decided.
        let mut candidates: Vec<Candidate> = Vec::new();
        for (node, specs) in &self.nodes {
            for (index, sched) in specs.iter().enumerate() {
                if sched.next_due <= now {
                    candidates.push(Candidate {
                        node: *node,
                        index,
                        behind_ms: now.saturating_duration_since(sched.next_due).as_millis(),
                        // A zero interval cannot arrive through the API (`10..=3600`) but can over
                        // the bus. Ranking it as if it were 1 ms keeps the comparison total without
                        // letting it monopolise: at `behind = 0` it still sorts with everything else
                        // that has only just come due.
                        interval_ms: u128::from(sched.spec.interval_secs)
                            .saturating_mul(1000)
                            .max(1),
                    });
                }
            }
        }
        let due_count = candidates.len();
        if due_count > budget {
            // Partial sort: only the front `budget` need to be in order, and the tail is discarded
            // unexamined. O(n) rather than O(n log n), which matters because this runs over the
            // whole working set — 400,000 specs on the largest deployment measured.
            candidates.select_nth_unstable_by(budget, later_first);
            candidates.truncate(budget);
        }

        // Pass 2 — mint and re-arm exactly the chosen ones. Everything else is left untouched,
        // including its `next_due` (see the 🚨 above).
        let mut out = Vec::with_capacity(candidates.len());
        let mut missed: u64 = 0;
        for c in &candidates {
            let Some(sched) = self
                .nodes
                .get_mut(&c.node)
                .and_then(|specs| specs.get_mut(c.index))
            else {
                continue; // unreachable: nothing mutates the set between the two passes
            };
            out.push(sched.spec.to_job(Uuid::new_v4()));
            let interval = Duration::from_secs(u64::from(sched.spec.interval_secs));
            // Whole intervals between the slot that just fired and `now`: the polls that will never
            // be issued because their turn has already passed. Zero while the scheduler keeps up —
            // one tick of lateness is far below one interval.
            //
            // 🚨 **What reaches this counter changed with the budget, and it is worth knowing
            // how.** It has always counted whole cycles that had passed for a spec that ran; what
            // was unreliable was how much of the shortfall ever arrived here at all. The backlog
            // used to sit in the scheduler's send loop rather than in this set, so the counter only
            // moved as often as that loop got round to asking — measured flat at **0** through a
            // settled ten-minute window on a poller serving 62.6 of 1,017 demanded polls per
            // second, and at 361,107 in the ten minutes right after that same poller adopted its
            // assignment. Deferring a spec without touching its `next_due` puts the backlog in
            // this set by construction, so what is counted here is now the deficit of the polls
            // that ran rather than a sample of where the queue happened to be.
            //
            // `checked_div` rather than a zero test: an interval of 0 is not reachable through the
            // API (`10..=3600`), and treating it as "nothing was missed" is the only answer that
            // makes sense if one ever arrives over the bus. `demand_per_sec` skips the same spec for
            // the same reason — one judgement about a degenerate interval, written in two places
            // that must agree.
            let behind = now.saturating_duration_since(sched.next_due).as_millis();
            let skipped = behind.checked_div(interval.as_millis()).unwrap_or(0);
            missed = missed.saturating_add(u64::try_from(skipped).unwrap_or(u64::MAX));
            sched.next_due += interval;
            if sched.next_due <= now {
                // Still behind after one step → re-anchor so we don't fire every tick.
                sched.next_due = now + interval;
            }
        }
        self.cycles_missed = self.cycles_missed.saturating_add(missed);
        self.deferred = u32::try_from(due_count - out.len()).unwrap_or(u32::MAX);
        out
    }

    /// How many specs were due at the last [`Self::due`] call and did **not** get a job.
    ///
    /// The number that says how far behind this poller is *right now*, which nothing else answered:
    /// `yagra_poll_demand_per_second` says what the schedule asks for and
    /// `yagra_poll_jobs_executed_total` says what was served, but the difference between them is a
    /// rate computed over a window, and an operator watching an incident wants the instantaneous
    /// depth. Read, not drained — it is a level, not a tally.
    ///
    /// ⚠️ It is the *last call's* answer, and `assignment.rs` calls `due` several times per tick
    /// when the channel is draining fast. That is the intended reading: the most recent round is
    /// the current state.
    #[must_use]
    pub fn deferred(&self) -> u32 {
        self.deferred
    }

    /// Polls per second this set *asks for*: `Σ(1 / interval_secs)` over every spec (ADR-108 Inc.2).
    ///
    /// 🚨 **[`Self::take_cycles_missed`] answers "how many whole cycles had passed for the polls
    /// that ran", and before ADR-109 Increment 4 how much of the shortfall reached it was a matter
    /// of luck.** It moves when [`Self::due`] finds a spec more than a whole interval behind, which
    /// needs the backlog to be *in this set*. It was not: `due` returned everything due and the
    /// scheduler's send loop held the queue, so how often the set was consulted at all depended on
    /// how long that loop took. Both extremes were measured on the same 15,000 silent devices
    /// (2026-08-30): flat at **0** through a settled 601-second window in which the poller
    /// served 62.6 of the 1,017 polls per second its intervals asked for, and 361,107 in the
    /// ten minutes right after that same poller adopted the assignment. Neither reading is the
    /// deficit; they are two samples of where the queue happened to be.
    ///
    /// Since a spec that loses a budgeted round keeps its `next_due`, the backlog is in this set by
    /// construction and the cycles a deferred spec lost are still on its clock when it finally runs.
    ///
    /// ⚠️ What it still cannot see is a job that was dispatched and then dropped at the device's
    /// single-flight guard; that is `yagra_poll_skipped_backpressure_total`, a different number.
    /// And "is this poller keeping up?" is still best asked as
    /// `rate(yagra_poll_jobs_executed_total) / yagra_poll_demand_per_second`, with
    /// `yagra_poll_deferred_specs` as the instantaneous depth beside it.
    ///
    /// ⚠️ It answers "is this poller serving the work it holds", never "is core handing it the
    /// right work" — an assignment that never arrived shows up in `yagra_working_set_specs`
    /// instead, and the two are different faults.
    ///
    /// 🚨 **The ratio is not ~1 on a healthy poller unless the window is chosen for it, and both
    /// ways it can be wrong were measured the day this shipped** (32-node lab, 187 specs = 95 at
    /// 60 s + 92 at 3600 s):
    ///
    /// - **Too short reads low, and it reads low in whole bursts.** The adoption window is
    ///   `specs / adopt_rate` — here `187 / 200` ≈ **0.94 s** — so the whole fleet's fast specs fire
    ///   as *one* burst a minute, staggered only within a node. A 331 s window owes 5.52 bursts and
    ///   catches 5, giving **0.906**, which is `5 / 5.517` to four figures. A 74 s window caught two
    ///   bursts where it owed 1.23 and read **1.46× too high**. Take the window as a whole multiple
    ///   of the dominant interval, and long enough to contain the slow tier at all.
    /// - **Just after a restart it reads high.** Adopting a set fires every spec once. Core and the
    ///   poller restarting together changes the epoch, which resyncs and fires them *again* — the
    ///   first reading here was **1.57**, with the hourly tier having run twice in eight minutes.
    ///
    /// Chosen properly it is exact. A **600 s** window — ten whole 60 s cycles — on that same lab
    /// gave `achieved` = **1.583333**, which is `95/60` to six figures and a fast-tier ratio of
    /// **1.0000**; the 0.0159 short of the full demand is `92/3600`, the hourly tier that was
    /// legitimately not due. Per check kind it implied **27 / 23 / 23 / 17 / 3 / 2** specs — every
    /// one an integer, and the same numbers as the inventory (27 device nodes, 23 credentialed,
    /// 3 DNS, 2 URL). Over an hour the whole ratio converges; below that, read it as the fast
    /// tier's and know that is what you have.
    ///
    /// A zero interval owes nothing, for the reason [`Self::due`] gives beside its `checked_div`:
    /// the API accepts `10..=3600`, so one can only arrive over the bus.
    ///
    /// ⚠️ **`fold` rather than `sum`, and the difference is visible to an operator.** Rust's
    /// `Sum for f64` folds from **`-0.0`** (the only identity that survives adding `-0.0` to it), so
    /// an empty set sums to negative zero and the Prometheus exporter renders it `-0`. Seen on a
    /// poller holding no assignment the day this shipped. `-0.0 == 0.0` is true, so a test comparing
    /// the two cannot tell — `an_empty_set_demands_nothing` compares the bits.
    #[must_use]
    pub fn demand_per_sec(&self) -> f64 {
        self.nodes
            .values()
            .flatten()
            .filter(|s| s.spec.interval_secs > 0)
            .map(|s| 1.0 / f64::from(s.spec.interval_secs))
            .fold(0.0, |acc, rate| acc + rate)
    }

    /// `(node count, spec count)` — for the heartbeat telemetry and the specs gauge.
    #[must_use]
    pub fn stats(&self) -> (u32, u32) {
        let nodes = u32::try_from(self.nodes.len()).unwrap_or(u32::MAX);
        let specs =
            u32::try_from(self.nodes.values().map(Vec::len).sum::<usize>()).unwrap_or(u32::MAX);
        (nodes, specs)
    }

    /// `(epoch, last_seq)` — echoed in the heartbeat so core can detect a stale/gapped poller.
    #[must_use]
    pub fn sync_state(&self) -> (Option<Uuid>, u64) {
        (self.epoch, self.last_seq)
    }

    /// Every plaintext device credential currently in this working set, deduplicated (ADR-045
    /// Inc.4).
    ///
    /// Feeds the fail-closed scan the poller runs over its own log before shipping it. Core's
    /// equivalent set comes from core's environment and therefore **cannot** contain a device
    /// community string — that value is decrypted from the credential store and inlined into the
    /// spec, so this collection is the only place it exists in plaintext outside the device.
    ///
    /// Built on demand rather than maintained incrementally: a set updated on every `apply` would
    /// either grow without bound or need eviction, and this runs once per support bundle. The cost
    /// is the residual hole worth knowing about — **a credential that has since been removed from
    /// the working set is not in this set**, so a log line written while it was in use would not be
    /// caught. Same class of gap as core's "a secret this process cannot see".
    ///
    /// **Never log the result.**
    #[must_use]
    pub fn secret_literals(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .nodes
            .values()
            .flatten()
            .flat_map(|s| s.spec.check.secret_literals())
            .map(str::to_owned)
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Pair a node's new spec list against what it already had, donating each surviving spec's previous
/// `next_due` (matched by full [`JobSpec`] equality, greedily, so duplicates pair up). Specs that
/// are new or changed come back with `next_due: None` — [`settle`] gives them one once the size of
/// the adoption is known. Donating the phase is what keeps a snapshot/upsert from re-phasing an
/// unchanged poll (anti-stampede, monitoring-conventions).
fn reschedule(old: Option<&Vec<ScheduledSpec>>, new_specs: Vec<JobSpec>) -> Vec<PendingSpec> {
    let mut used = vec![false; old.map_or(0, Vec::len)];
    let mut out = Vec::with_capacity(new_specs.len());
    for spec in new_specs {
        let mut reused = None;
        if let Some(olds) = old {
            for (i, os) in olds.iter().enumerate() {
                if !used[i] && os.spec == spec {
                    used[i] = true;
                    reused = Some(os.next_due);
                    break;
                }
            }
        }
        out.push(PendingSpec {
            spec,
            next_due: reused,
        });
    }
    out
}

/// Give every spec of **one node** that did not keep a phase its `next_due`: one random base offset
/// for the node, spread uniformly over the adoption window and clamped per spec to its own interval
/// (spreading a 10s poll over 25s would move the gap rather than close it), plus a fixed
/// [`SPEC_STAGGER`] per position so the node's own specs cannot land on the same scheduler tick.
///
/// **The draw is per node, not per spec, and that is the correction.** Anti-stampede is about
/// spreading *nodes*; drawing each spec of one node independently is what let all of them share a
/// tick, and a shared tick is permanent — see the module docs. Staggering by position instead makes
/// the separation a property of the arithmetic: consecutive specs are exactly [`SPEC_STAGGER`]
/// apart, and since [`WorkingSet::due`] re-arms each by a whole interval, that gap never closes.
///
/// The separation holds for every realistic schedule, including the harmonically related tiers
/// (60s / 300s / 3600s) where a collision would otherwise repeat forever: two offsets differing by
/// `k × SPEC_STAGGER` coincide only if that difference is a multiple of the intervals' gcd, and the
/// staggers in play (1–20s for a node with up to 20 specs) sit strictly between zero and the
/// shortest interval the API accepts. It degrades gracefully rather than wrapping: `% interval_ms`
/// keeps every offset inside its own interval, so a node with more specs than its interval has room
/// for loses the guarantee for the ones that wrap — it does not schedule them in the past.
fn settle(
    pending: Vec<PendingSpec>,
    now: Instant,
    window_ms: u32,
    rng: &mut impl FnMut(u32) -> u32,
) -> Vec<ScheduledSpec> {
    // Drawn on first use so a settle that donates every phase asks the rng for nothing (a resync
    // must not consume entropy, and a test can assert exactly that).
    let mut base_ms: Option<u32> = None;
    pending
        .into_iter()
        .enumerate()
        .map(|(k, p)| {
            let next_due = p.next_due.unwrap_or_else(|| {
                let interval_ms = p.spec.interval_secs.saturating_mul(1000);
                let bound = interval_ms.min(window_ms);
                let base = *base_ms.get_or_insert_with(|| if bound == 0 { 0 } else { rng(bound) });
                let stagger = u32::try_from(k)
                    .unwrap_or(u32::MAX)
                    .saturating_mul(SPEC_STAGGER_MS);
                let offset_ms = if interval_ms == 0 {
                    0
                } else {
                    base.saturating_add(stagger) % interval_ms
                };
                now + Duration::from_millis(u64::from(offset_ms))
            });
            ScheduledSpec {
                spec: p.spec,
                next_due,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use yagra_bus::{CheckSpec, IcmpCheck, SnmpCheck};

    fn node(n: u128) -> NodeId {
        NodeId::from(Uuid::from_u128(n))
    }

    fn icmp_spec(node: NodeId, interval: u32) -> JobSpec {
        let job = PollJob::icmp(
            Uuid::nil(),
            node,
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IcmpCheck::default(),
            interval,
        );
        JobSpec::from_job(&job)
    }

    fn snmp_spec(node: NodeId, community: &str, interval: u32) -> JobSpec {
        let job = PollJob::snmp(
            Uuid::nil(),
            node,
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            SnmpCheck {
                community: community.to_owned(),
                oids: vec!["1.3.6.1.2.1.1.3.0".to_owned()],
                columns: Vec::new(),
                timeout_ms: 2000,
            },
            interval,
        );
        JobSpec::from_job(&job)
    }

    fn snapshot(epoch: Uuid, seq: u64, idx: u32, total: u32, nodes: Vec<NodeJobs>) -> SyncMsg {
        SyncMsg::SnapshotChunk(WorkingSetSnapshot {
            poller_id: "p1".to_owned(),
            epoch,
            seq,
            chunk_index: idx,
            chunk_total: total,
            nodes,
            total_nodes: 0,
            pool: None,
        })
    }

    fn delta(epoch: Uuid, seq: u64, upserts: Vec<NodeJobs>, removes: Vec<NodeId>) -> SyncMsg {
        SyncMsg::Delta(WorkingSetDelta {
            poller_id: "p1".to_owned(),
            epoch,
            seq,
            upserts,
            removes,
        })
    }

    /// Run `ticks` scheduler ticks from `now` at [`SCHEDULER_TICK`] and return, for each tick, how
    /// many jobs it produced **per node** — the shape the worker actually sees, since one `due` call
    /// becomes one back-to-back burst into a limiter that single-flights per target.
    ///
    /// `late_every` injects a tick that arrives one period late (the scheduler's
    /// `MissedTickBehavior::Delay`), because that is the case a one-tick separation would not
    /// survive. `0` means never.
    fn tick_bursts(
        ws: &mut WorkingSet,
        now: Instant,
        ticks: u32,
        late_every: u32,
    ) -> Vec<HashMap<NodeId, usize>> {
        let mut out = Vec::new();
        let mut elapsed = Duration::ZERO;
        for i in 0..ticks {
            elapsed += SCHEDULER_TICK;
            if late_every != 0 && i % late_every == late_every - 1 {
                elapsed += SCHEDULER_TICK; // this tick was missed; the next sweeps up both
                continue;
            }
            let mut per_node: HashMap<NodeId, usize> = HashMap::new();
            for job in ws.due(now + elapsed, usize::MAX) {
                *per_node.entry(job.node_id).or_default() += 1;
            }
            out.push(per_node);
        }
        out
    }

    /// The firewall that found this bug: an SNMP scalar check, an SNMP table walk and ICMP, all on
    /// one node, all on the same interval — the shape no test in this module had.
    fn one_node_three_specs(n: NodeId, interval: u32) -> NodeJobs {
        NodeJobs {
            node_id: n,
            specs: vec![
                snmp_spec(n, "public", interval),
                snmp_spec(n, "public2", interval),
                icmp_spec(n, interval),
            ],
        }
    }

    /// A jitter source that ignores the bound and returns a fixed offset (deterministic tests).
    fn fixed(offset_ms: u32) -> impl FnMut(u32) -> u32 {
        move |_bound| offset_ms
    }

    #[test]
    fn single_chunk_snapshot_replaces_set_and_sets_sync_state() {
        let mut ws = WorkingSet::new();
        let e = Uuid::from_u128(1);
        let mut rng = fixed(0);
        let out = ws.apply(
            snapshot(
                e,
                1,
                0,
                1,
                vec![NodeJobs {
                    node_id: node(1),
                    specs: vec![icmp_spec(node(1), 30)],
                }],
            ),
            Instant::now(),
            &mut rng,
        );
        assert_eq!(out, ApplyOutcome::Applied);
        assert_eq!(ws.stats(), (1, 1));
        assert_eq!(ws.sync_state(), (Some(e), 1));
    }

    #[test]
    fn multi_chunk_snapshot_assembles_out_of_order_and_dedups() {
        let mut ws = WorkingSet::new();
        let e = Uuid::from_u128(1);
        let mut rng = fixed(0);
        let now = Instant::now();
        // Chunk 1 first (out of order) → buffered, incomplete.
        assert_eq!(
            ws.apply(
                snapshot(
                    e,
                    5,
                    1,
                    2,
                    vec![NodeJobs {
                        node_id: node(2),
                        specs: vec![icmp_spec(node(2), 30)]
                    }]
                ),
                now,
                &mut rng
            ),
            ApplyOutcome::Ignored
        );
        // Duplicate of chunk 1 → still incomplete, idempotent.
        assert_eq!(
            ws.apply(
                snapshot(
                    e,
                    5,
                    1,
                    2,
                    vec![NodeJobs {
                        node_id: node(2),
                        specs: vec![icmp_spec(node(2), 30)]
                    }]
                ),
                now,
                &mut rng
            ),
            ApplyOutcome::Ignored
        );
        // Chunk 0 completes the group.
        assert_eq!(
            ws.apply(
                snapshot(
                    e,
                    5,
                    0,
                    2,
                    vec![NodeJobs {
                        node_id: node(1),
                        specs: vec![icmp_spec(node(1), 30)]
                    }]
                ),
                now,
                &mut rng
            ),
            ApplyOutcome::Applied
        );
        assert_eq!(ws.stats(), (2, 1 + 1));
        assert_eq!(ws.sync_state(), (Some(e), 5));
    }

    #[test]
    fn newer_group_discards_the_stale_buffer() {
        let mut ws = WorkingSet::new();
        let e = Uuid::from_u128(1);
        let mut rng = fixed(0);
        let now = Instant::now();
        // Start assembling group (e, seq 1) — one of two chunks.
        ws.apply(
            snapshot(
                e,
                1,
                0,
                2,
                vec![NodeJobs {
                    node_id: node(1),
                    specs: vec![icmp_spec(node(1), 30)],
                }],
            ),
            now,
            &mut rng,
        );
        // A newer, complete group (e, seq 2) arrives → the stale seq-1 buffer is discarded.
        let out = ws.apply(
            snapshot(
                e,
                2,
                0,
                1,
                vec![NodeJobs {
                    node_id: node(9),
                    specs: vec![icmp_spec(node(9), 30)],
                }],
            ),
            now,
            &mut rng,
        );
        assert_eq!(out, ApplyOutcome::Applied);
        assert_eq!(ws.sync_state(), (Some(e), 2));
        assert_eq!(ws.stats(), (1, 1)); // only node 9 survived, not node 1
    }

    #[test]
    fn different_epoch_chunk_supersedes_and_reanchors_epoch() {
        let mut ws = WorkingSet::new();
        let e1 = Uuid::from_u128(1);
        let e2 = Uuid::from_u128(2);
        let mut rng = fixed(0);
        let now = Instant::now();
        // Assembling (e1, seq 1), incomplete.
        ws.apply(
            snapshot(
                e1,
                1,
                0,
                2,
                vec![NodeJobs {
                    node_id: node(1),
                    specs: vec![icmp_spec(node(1), 30)],
                }],
            ),
            now,
            &mut rng,
        );
        // A chunk on a new epoch (core restarted) supersedes it.
        let out = ws.apply(
            snapshot(
                e2,
                1,
                0,
                1,
                vec![NodeJobs {
                    node_id: node(5),
                    specs: vec![icmp_spec(node(5), 30)],
                }],
            ),
            now,
            &mut rng,
        );
        assert_eq!(out, ApplyOutcome::Applied);
        assert_eq!(ws.sync_state(), (Some(e2), 1));
    }

    #[test]
    fn older_snapshot_of_same_epoch_is_ignored() {
        let mut ws = WorkingSet::new();
        let e = Uuid::from_u128(1);
        let mut rng = fixed(0);
        let now = Instant::now();
        ws.apply(
            snapshot(
                e,
                5,
                0,
                1,
                vec![NodeJobs {
                    node_id: node(1),
                    specs: vec![icmp_spec(node(1), 30)],
                }],
            ),
            now,
            &mut rng,
        );
        // A stale snapshot at seq 3 (< applied 5) is discarded.
        let out = ws.apply(
            snapshot(
                e,
                3,
                0,
                1,
                vec![NodeJobs {
                    node_id: node(2),
                    specs: vec![icmp_spec(node(2), 30)],
                }],
            ),
            now,
            &mut rng,
        );
        assert_eq!(out, ApplyOutcome::Ignored);
        assert_eq!(ws.sync_state(), (Some(e), 5));
        assert_eq!(ws.stats(), (1, 1)); // still node 1 only
    }

    #[test]
    fn delta_applies_upserts_and_removes_at_next_seq() {
        let mut ws = WorkingSet::new();
        let e = Uuid::from_u128(1);
        let mut rng = fixed(0);
        let now = Instant::now();
        ws.apply(
            snapshot(
                e,
                1,
                0,
                1,
                vec![NodeJobs {
                    node_id: node(1),
                    specs: vec![icmp_spec(node(1), 30)],
                }],
            ),
            now,
            &mut rng,
        );
        let out = ws.apply(
            delta(
                e,
                2,
                vec![NodeJobs {
                    node_id: node(2),
                    specs: vec![icmp_spec(node(2), 30)],
                }],
                vec![node(1)],
            ),
            now,
            &mut rng,
        );
        assert_eq!(out, ApplyOutcome::Applied);
        assert_eq!(ws.sync_state(), (Some(e), 2));
        assert_eq!(ws.stats(), (1, 1)); // node 1 removed, node 2 added
    }

    #[test]
    fn delta_gap_needs_sync_and_does_not_mutate() {
        let mut ws = WorkingSet::new();
        let e = Uuid::from_u128(1);
        let mut rng = fixed(0);
        let now = Instant::now();
        ws.apply(
            snapshot(
                e,
                1,
                0,
                1,
                vec![NodeJobs {
                    node_id: node(1),
                    specs: vec![icmp_spec(node(1), 30)],
                }],
            ),
            now,
            &mut rng,
        );
        // seq jumps from 1 to 3 → gap.
        let out = ws.apply(
            delta(
                e,
                3,
                vec![NodeJobs {
                    node_id: node(2),
                    specs: vec![icmp_spec(node(2), 30)],
                }],
                vec![],
            ),
            now,
            &mut rng,
        );
        assert_eq!(out, ApplyOutcome::NeedSync);
        assert_eq!(ws.sync_state(), (Some(e), 1), "seq unchanged");
        assert_eq!(ws.stats(), (1, 1), "node 2 was NOT added");
    }

    #[test]
    fn delta_wrong_epoch_needs_sync_and_does_not_mutate() {
        let mut ws = WorkingSet::new();
        let e = Uuid::from_u128(1);
        let mut rng = fixed(0);
        let now = Instant::now();
        ws.apply(
            snapshot(
                e,
                1,
                0,
                1,
                vec![NodeJobs {
                    node_id: node(1),
                    specs: vec![icmp_spec(node(1), 30)],
                }],
            ),
            now,
            &mut rng,
        );
        let out = ws.apply(
            delta(
                Uuid::from_u128(999),
                2,
                vec![NodeJobs {
                    node_id: node(2),
                    specs: vec![icmp_spec(node(2), 30)],
                }],
                vec![],
            ),
            now,
            &mut rng,
        );
        assert_eq!(out, ApplyOutcome::NeedSync);
        assert_eq!(ws.stats(), (1, 1));
    }

    #[test]
    fn delta_before_any_snapshot_needs_sync() {
        let mut ws = WorkingSet::new();
        let mut rng = fixed(0);
        let out = ws.apply(
            delta(Uuid::from_u128(1), 1, vec![], vec![]),
            Instant::now(),
            &mut rng,
        );
        assert_eq!(out, ApplyOutcome::NeedSync);
    }

    #[test]
    fn unchanged_spec_keeps_phase_across_snapshot_but_changed_rejitters() {
        let mut ws = WorkingSet::new();
        let e = Uuid::from_u128(1);
        let now = Instant::now();
        // First snapshot: node 1 (icmp/30) and node 2 (icmp/30), jitter offset 1000ms.
        let mut rng1 = fixed(1000);
        ws.apply(
            snapshot(
                e,
                1,
                0,
                1,
                vec![
                    NodeJobs {
                        node_id: node(1),
                        specs: vec![icmp_spec(node(1), 30)],
                    },
                    NodeJobs {
                        node_id: node(2),
                        specs: vec![icmp_spec(node(2), 30)],
                    },
                ],
            ),
            now,
            &mut rng1,
        );
        let due_node1 = ws.nodes[&node(1)][0].next_due;
        let due_node2 = ws.nodes[&node(2)][0].next_due;
        assert_eq!(due_node1, now + Duration::from_millis(1000));

        // Second snapshot: node 1 unchanged, node 2's interval changed to 60. New jitter 7000ms.
        let mut rng2 = fixed(7000);
        ws.apply(
            snapshot(
                e,
                2,
                0,
                1,
                vec![
                    NodeJobs {
                        node_id: node(1),
                        specs: vec![icmp_spec(node(1), 30)],
                    },
                    NodeJobs {
                        node_id: node(2),
                        specs: vec![icmp_spec(node(2), 60)],
                    },
                ],
            ),
            now,
            &mut rng2,
        );
        // Node 1 survived unchanged → same phase (NOT re-jittered to 7000).
        assert_eq!(
            ws.nodes[&node(1)][0].next_due,
            due_node1,
            "unchanged spec keeps its phase"
        );
        // Node 2's interval changed → re-jittered to the new offset.
        assert_eq!(
            ws.nodes[&node(2)][0].next_due,
            now + Duration::from_millis(7000),
            "changed spec is re-jittered"
        );
        assert_ne!(ws.nodes[&node(2)][0].next_due, due_node2);
    }

    #[test]
    fn changed_credential_rejitters_the_spec() {
        // A spec differing only by (inlined) community is "changed" → its phase is not preserved.
        let mut ws = WorkingSet::new();
        let e = Uuid::from_u128(1);
        let now = Instant::now();
        let mut rng1 = fixed(1000);
        ws.apply(
            snapshot(
                e,
                1,
                0,
                1,
                vec![NodeJobs {
                    node_id: node(1),
                    specs: vec![snmp_spec(node(1), "public", 30)],
                }],
            ),
            now,
            &mut rng1,
        );
        let mut rng2 = fixed(5000);
        ws.apply(
            snapshot(
                e,
                2,
                0,
                1,
                vec![NodeJobs {
                    node_id: node(1),
                    specs: vec![snmp_spec(node(1), "private", 30)],
                }],
            ),
            now,
            &mut rng2,
        );
        assert_eq!(
            ws.nodes[&node(1)][0].next_due,
            now + Duration::from_millis(5000)
        );
    }

    #[test]
    fn jitter_stays_within_interval() {
        let mut ws = WorkingSet::new();
        let e = Uuid::from_u128(1);
        let now = Instant::now();
        // rng returns bound-1 (the max), so the offset must be strictly under the interval.
        let mut rng = |bound: u32| bound - 1;
        ws.apply(
            snapshot(
                e,
                1,
                0,
                1,
                vec![NodeJobs {
                    node_id: node(1),
                    specs: vec![icmp_spec(node(1), 30)],
                }],
            ),
            now,
            &mut rng,
        );
        let due = ws.nodes[&node(1)][0].next_due;
        assert!(due >= now);
        assert!(
            due < now + Duration::from_secs(30),
            "jitter must be within [0, interval)"
        );
    }

    // ── One node's specs must never share a scheduler tick (the v0.2.3–v0.2.5 regression) ────────
    //
    // These are the tests this module did not have: every case above builds nodes with exactly one
    // spec, so nothing here could see a node's specs collide with each other. They assert the
    // property the worker actually depends on — a `due` burst never carries two jobs for one target
    // — rather than the offsets that produce it, because the offsets are an implementation detail
    // and the burst is what the per-device single-flight guard drops from.

    #[test]
    fn a_nodes_specs_never_share_one_due_burst() {
        // Reproduces the shipped failure: a small deployment, so the adoption window is a few
        // milliseconds wide, and a node carrying three same-interval specs. Before the stagger all
        // three drew from that window, landed in one tick, and the limiter kept the first and
        // dropped the other two — on every cycle, forever.
        let mut ws = WorkingSet::new();
        let now = Instant::now();
        let mut rng = fixed(7); // a 14-spec adoption gives a 70ms window; any draw sits inside one tick
        ws.apply(
            snapshot(
                Uuid::from_u128(1),
                1,
                0,
                1,
                vec![one_node_three_specs(node(1), 60)],
            ),
            now,
            &mut rng,
        );
        // Four full intervals, so a collision that only repeats every cycle still shows up.
        let bursts = tick_bursts(&mut ws, now, 60 * 4 * 2, 0);
        let worst = bursts.iter().filter_map(|b| b.get(&node(1))).max();
        assert_eq!(
            worst,
            Some(&1),
            "a node's specs must reach the worker one tick apart, never together"
        );
        // …and all three must actually be polled: separation is worthless if it costs a poll.
        let polled: usize = bursts.iter().filter_map(|b| b.get(&node(1))).sum();
        assert_eq!(polled, 12, "3 specs × 4 intervals");
    }

    #[test]
    fn the_separation_survives_a_late_tick() {
        // `MissedTickBehavior::Delay` means a blocked scheduler resumes late and sweeps up
        // everything now due. One tick of separation would merge under that; SPEC_STAGGER_MS is two
        // for exactly this reason, so pin the reason.
        let mut ws = WorkingSet::new();
        let now = Instant::now();
        let mut rng = fixed(0);
        ws.apply(
            snapshot(
                Uuid::from_u128(1),
                1,
                0,
                1,
                vec![one_node_three_specs(node(1), 60)],
            ),
            now,
            &mut rng,
        );
        let bursts = tick_bursts(&mut ws, now, 60 * 4 * 2, 3);
        let worst = bursts.iter().filter_map(|b| b.get(&node(1))).max();
        assert_eq!(
            worst,
            Some(&1),
            "every third tick arrives late and must still not merge two of a node's specs"
        );
    }

    #[test]
    fn the_separation_holds_across_harmonic_interval_tiers() {
        // 60 / 300 / 3600 all divide each other, so two specs that once shared a tick would share it
        // forever — `due` re-arms by a whole interval, so their relative phase never drifts apart.
        // This is the case that makes "unlikely" indistinguishable from "broken" in production.
        let n = node(1);
        let mut ws = WorkingSet::new();
        let now = Instant::now();
        let mut rng = fixed(0);
        ws.apply(
            snapshot(
                Uuid::from_u128(1),
                1,
                0,
                1,
                vec![NodeJobs {
                    node_id: n,
                    specs: vec![
                        snmp_spec(n, "public", 60),
                        snmp_spec(n, "public2", 300),
                        icmp_spec(n, 3600),
                    ],
                }],
            ),
            now,
            &mut rng,
        );
        // Two full hours: the 3600s spec coincides with a 60s tick 120 times in that span.
        let bursts = tick_bursts(&mut ws, now, 3600 * 2 * 2, 0);
        let worst = bursts.iter().filter_map(|b| b.get(&n)).max();
        assert_eq!(worst, Some(&1), "harmonic tiers must not lock together");
    }

    #[test]
    fn a_node_draws_one_offset_however_many_specs_it_adopts() {
        // The per-node draw *is* the fix: a per-spec draw is what put them in one tick. Pin the
        // count, because restoring the old behaviour would be a one-line change that breaks nothing
        // else in this file.
        let mut draws = 0usize;
        let mut rng = |bound: u32| {
            draws += 1;
            bound / 2
        };
        let mut ws = WorkingSet::new();
        ws.apply(
            snapshot(
                Uuid::from_u128(1),
                1,
                0,
                1,
                vec![
                    one_node_three_specs(node(1), 60),
                    one_node_three_specs(node(2), 60),
                ],
            ),
            Instant::now(),
            &mut rng,
        );
        assert_eq!(draws, 2, "one draw per node, not one per spec");
    }

    #[test]
    fn due_rearms_one_interval_ahead() {
        let mut ws = WorkingSet::new();
        let e = Uuid::from_u128(1);
        let now = Instant::now();
        let mut rng = fixed(0); // next_due = now
        ws.apply(
            snapshot(
                e,
                1,
                0,
                1,
                vec![NodeJobs {
                    node_id: node(1),
                    specs: vec![icmp_spec(node(1), 30)],
                }],
            ),
            now,
            &mut rng,
        );
        // Due immediately.
        let jobs = ws.due(now, usize::MAX);
        assert_eq!(jobs.len(), 1);
        // Not due again until an interval passes.
        assert!(ws.due(now, usize::MAX).is_empty());
        assert_eq!(ws.due(now + Duration::from_secs(30), usize::MAX).len(), 1);
    }

    #[test]
    fn due_mints_distinct_job_ids_from_the_spec() {
        let mut ws = WorkingSet::new();
        let e = Uuid::from_u128(1);
        let now = Instant::now();
        let mut rng = fixed(0);
        ws.apply(
            snapshot(
                e,
                1,
                0,
                1,
                vec![NodeJobs {
                    node_id: node(1),
                    specs: vec![icmp_spec(node(1), 30)],
                }],
            ),
            now,
            &mut rng,
        );
        let j1 = ws.due(now, usize::MAX).remove(0);
        let j2 = ws.due(now + Duration::from_secs(30), usize::MAX).remove(0);
        assert_ne!(j1.job_id, j2.job_id, "each dispatch gets a fresh id");
        assert_eq!(j1.node_id, node(1));
        assert!(matches!(j1.check, CheckSpec::Icmp(_)));
    }

    #[test]
    fn due_catch_up_is_capped_to_one_makeup_poll() {
        let mut ws = WorkingSet::new();
        let e = Uuid::from_u128(1);
        let now = Instant::now();
        let mut rng = fixed(0); // next_due = now
        ws.apply(
            snapshot(
                e,
                1,
                0,
                1,
                vec![NodeJobs {
                    node_id: node(1),
                    specs: vec![icmp_spec(node(1), 30)],
                }],
            ),
            now,
            &mut rng,
        );
        // A long pause: 300s later, far past the 30s interval.
        let much_later = now + Duration::from_secs(300);
        let jobs = ws.due(much_later, usize::MAX);
        assert_eq!(jobs.len(), 1, "only one make-up poll, not a backlog burst");
        // Re-anchored to now + interval, not left far in the past.
        assert_eq!(
            ws.nodes[&node(1)][0].next_due,
            much_later + Duration::from_secs(30)
        );
    }

    /// The make-up poll above hides ten polls that never happened, and until 2026-08-29 that fact
    /// was computed and discarded — which is why a fleet polled at half its configured rate looked
    /// healthy on every counter.
    ///
    /// 🚨 **Both directions, and the on-time one is the load-bearing half.** A counter that only
    /// ever goes up is indistinguishable from one wired to the wrong branch; the first two
    /// assertions are what say this measures lateness rather than dispatches
    /// (`rejection-only-tests-pass-when-everything-rejects`).
    #[test]
    fn a_missed_cycle_is_counted_and_an_on_time_one_is_not() {
        let mut ws = WorkingSet::new();
        let e = Uuid::from_u128(1);
        let now = Instant::now();
        let mut rng = fixed(0); // next_due = now
        ws.apply(
            snapshot(
                e,
                1,
                0,
                1,
                vec![NodeJobs {
                    node_id: node(1),
                    specs: vec![icmp_spec(node(1), 30)],
                }],
            ),
            now,
            &mut rng,
        );

        // On time: the first poll, then one exactly an interval later. Nothing was missed.
        assert_eq!(ws.due(now, usize::MAX).len(), 1);
        assert_eq!(ws.take_cycles_missed(), 0, "an on-time poll is not a miss");
        assert_eq!(ws.due(now + Duration::from_secs(30), usize::MAX).len(), 1);
        assert_eq!(ws.take_cycles_missed(), 0, "still on time");

        // A tick's worth of lateness is far below one interval and must not register either.
        assert_eq!(
            ws.due(now + Duration::from_secs(60) + SCHEDULER_TICK, usize::MAX)
                .len(),
            1
        );
        assert_eq!(
            ws.take_cycles_missed(),
            0,
            "half a second late on a 30s interval is not a missed cycle"
        );

        // Now fall behind: 300s past the slot that just fired, on a 30s interval, is ten slots
        // that no poll will ever be issued for — the single make-up poll covers the eleventh.
        // (the slot that just fired was at now+90, so now+390 is exactly 300s past it)
        let much_later = now + Duration::from_secs(390);
        assert_eq!(
            ws.due(much_later, usize::MAX).len(),
            1,
            "one make-up poll, as before"
        );
        assert_eq!(ws.take_cycles_missed(), 10);
        assert_eq!(ws.take_cycles_missed(), 0, "draining resets the tally");
    }

    // ── The demand meter (ADR-108 Inc.2) ─────────────────────────────────────────────────────────
    //
    // What the counter above cannot say. `cycles_missed` only moves when `due` re-anchors, so a
    // poller serving 26.9 of the 1,017 polls/s it owed read 0 on hardware. These pin the other half
    // of the ratio — the number the shortfall is measured *against*.

    /// The acceptance side, and the arithmetic: three specs, two intervals, one sum.
    #[test]
    fn demand_is_the_sum_of_one_over_each_interval() {
        let mut ws = WorkingSet::new();
        let e = Uuid::from_u128(1);
        let mut rng = fixed(0);
        let n = node(1);
        let jobs = NodeJobs {
            node_id: n,
            specs: vec![
                snmp_spec(n, "public", 60),
                snmp_spec(n, "public2", 60),
                icmp_spec(n, 30),
            ],
        };
        assert_eq!(
            ws.apply(snapshot(e, 1, 0, 1, vec![jobs]), Instant::now(), &mut rng),
            ApplyOutcome::Applied
        );
        let expected = 2.0 / 60.0 + 1.0 / 30.0;
        assert!(
            (ws.demand_per_sec() - expected).abs() < 1e-9,
            "asked for {} polls/s, expected {expected}",
            ws.demand_per_sec()
        );
    }

    /// 🚨 **The bits, not the value.** `-0.0 == 0.0` is true, so the obvious assertion here passes
    /// for negative zero — and negative zero is what this returned before `demand_per_sec` folded
    /// from an explicit `0.0`, because Rust's `Sum for f64` uses `-0.0` as its identity. The test
    /// was written, was green, and the poller with no assignment still exported `-0`.
    #[test]
    fn an_empty_set_demands_nothing() {
        let ws = WorkingSet::new();
        assert_eq!(
            ws.demand_per_sec().to_bits(),
            0.0_f64.to_bits(),
            "an empty set must ask for positive zero: it is published as-is, and an operator \
             reading `-0` on a gauge reasonably concludes the number is broken (got {})",
            ws.demand_per_sec()
        );
    }

    /// A zero interval cannot arrive through the API (`10..=3600`) but can arrive over the bus, and
    /// it owes nothing — the same judgement `due` makes beside its `checked_div`. The healthy spec
    /// beside it is what stops a division-by-zero `inf` (or a blanket `0`) from passing.
    #[test]
    fn a_zero_interval_spec_contributes_no_demand() {
        let mut ws = WorkingSet::new();
        let e = Uuid::from_u128(1);
        let mut rng = fixed(0);
        let n = node(1);
        let jobs = NodeJobs {
            node_id: n,
            specs: vec![snmp_spec(n, "public", 0), icmp_spec(n, 60)],
        };
        assert_eq!(
            ws.apply(snapshot(e, 1, 0, 1, vec![jobs]), Instant::now(), &mut rng),
            ApplyOutcome::Applied
        );
        assert!(
            ws.demand_per_sec().is_finite(),
            "a zero interval must not divide"
        );
        assert!(
            (ws.demand_per_sec() - 1.0 / 60.0).abs() < 1e-9,
            "only the healthy spec is owed; got {}",
            ws.demand_per_sec()
        );
    }

    /// It must track the set, not the value it had at boot: a poller adopts and sheds work all day.
    #[test]
    fn demand_follows_the_set_through_a_snapshot_and_a_delta() {
        let mut ws = WorkingSet::new();
        let e = Uuid::from_u128(1);
        let mut rng = fixed(0);
        let now = Instant::now();
        let (a, b) = (node(1), node(2));
        assert_eq!(
            ws.apply(
                snapshot(
                    e,
                    1,
                    0,
                    1,
                    vec![one_node_three_specs(a, 60), one_node_three_specs(b, 60)],
                ),
                now,
                &mut rng,
            ),
            ApplyOutcome::Applied
        );
        assert!((ws.demand_per_sec() - 6.0 / 60.0).abs() < 1e-9);

        assert_eq!(
            ws.apply(delta(e, 2, Vec::new(), vec![b]), now, &mut rng),
            ApplyOutcome::Applied
        );
        assert!(
            (ws.demand_per_sec() - 3.0 / 60.0).abs() < 1e-9,
            "dropping a node halves what is owed; got {}",
            ws.demand_per_sec()
        );
    }

    // ── Adoption window (ADR-051) ────────────────────────────────────────────────────────────────
    //
    // The property under test is the one that had no test and no alert: a node handed over from
    // another poller must be polled promptly, not re-phased across a whole interval. `max_bound`
    // records the largest jitter bound the set ever asked for, which is exactly the window.

    /// A jitter source that records every bound it is asked for and always returns the maximum
    /// offset (`bound - 1`), so the assertions bound the *worst* case rather than an average.
    fn recording(seen: &mut Vec<u32>) -> impl FnMut(u32) -> u32 + '_ {
        move |bound| {
            seen.push(bound);
            bound.saturating_sub(1)
        }
    }

    fn many_nodes(count: u128, interval: u32) -> Vec<NodeJobs> {
        (0..count)
            .map(|i| NodeJobs {
                node_id: node(i + 1),
                specs: vec![icmp_spec(node(i + 1), interval)],
            })
            .collect()
    }

    #[test]
    fn a_small_handover_is_adopted_inside_the_rate_window_not_a_whole_interval() {
        // 50 specs at 200/s ⇒ a 250ms window, against a 30s interval. Before ADR-051 the bound was
        // the interval, so the last of these nodes could go unpolled for 30s after its previous
        // owner had already stopped — a 60s hole between consecutive samples that nothing measured.
        let mut ws = WorkingSet::with_adopt_rate(200);
        let e = Uuid::from_u128(1);
        let mut seen = Vec::new();
        let out = ws.apply(
            snapshot(e, 1, 0, 1, many_nodes(50, 30)),
            Instant::now(),
            &mut recording(&mut seen),
        );
        assert_eq!(out, ApplyOutcome::Applied);
        assert_eq!(ws.stats(), (50, 50));
        assert!(
            seen.iter().all(|&b| b == 250),
            "50 specs / 200 per sec = a 250ms window, got {seen:?}"
        );
    }

    #[test]
    fn a_cold_start_still_spreads_across_the_interval() {
        // 25,000 specs at 200/s wants 125s, which exceeds the 30s interval — so the per-spec clamp
        // wins and this is byte-for-byte the pre-ADR-051 behaviour. The rate is a ceiling on the
        // burst, never a licence to poll a whole fleet at once.
        let mut ws = WorkingSet::with_adopt_rate(200);
        let e = Uuid::from_u128(1);
        let mut seen = Vec::new();
        ws.apply(
            snapshot(e, 1, 0, 1, many_nodes(25_000, 30)),
            Instant::now(),
            &mut recording(&mut seen),
        );
        assert!(
            seen.iter().all(|&b| b == 30_000),
            "cold start must clamp to the interval"
        );
    }

    #[test]
    fn the_window_is_sized_by_the_new_specs_only_not_the_whole_set() {
        // The case the fix exists for: a 500-node poller adopts 10 more. Sizing the window off the
        // set (510) instead of the adoption (10) would spread the newcomers over 2.5s instead of
        // 50ms — and, worse, would grow with the size of the poller rather than the size of the
        // failover.
        let mut ws = WorkingSet::with_adopt_rate(200);
        let e = Uuid::from_u128(1);
        let now = Instant::now();
        ws.apply(
            snapshot(e, 1, 0, 1, many_nodes(500, 30)),
            now,
            &mut fixed(0),
        );
        let mut seen = Vec::new();
        let newcomers: Vec<NodeJobs> = (500..510)
            .map(|i| NodeJobs {
                node_id: node(i + 1),
                specs: vec![icmp_spec(node(i + 1), 30)],
            })
            .collect();
        let out = ws.apply(
            delta(e, 2, newcomers, vec![]),
            now,
            &mut recording(&mut seen),
        );
        assert_eq!(out, ApplyOutcome::Applied);
        assert_eq!(seen.len(), 10, "only the newcomers are jittered");
        assert!(
            seen.iter().all(|&b| b == 50),
            "10 specs / 200 per sec = a 50ms window, got {seen:?}"
        );
    }

    #[test]
    fn a_resync_that_donates_every_phase_jitters_nothing() {
        // A core restart bumps the epoch and re-snapshots the whole fleet. Nothing moved, so every
        // spec keeps its phase and the adoption window never applies — the property the original
        // anti-stampede rule bought, and the one this change must not spend.
        let mut ws = WorkingSet::with_adopt_rate(200);
        let now = Instant::now();
        let nodes = many_nodes(200, 30);
        ws.apply(
            snapshot(Uuid::from_u128(1), 1, 0, 1, nodes.clone()),
            now,
            &mut fixed(7_000),
        );
        let before: Vec<Instant> = (1..=200).map(|i| ws.nodes[&node(i)][0].next_due).collect();
        let mut seen = Vec::new();
        ws.apply(
            snapshot(Uuid::from_u128(2), 1, 0, 1, nodes),
            now,
            &mut recording(&mut seen),
        );
        assert!(seen.is_empty(), "an unchanged resync must not re-jitter");
        let after: Vec<Instant> = (1..=200).map(|i| ws.nodes[&node(i)][0].next_due).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn a_zero_adopt_rate_restores_the_interval_wide_jitter() {
        // The escape hatch (`YAGRA_ADOPT_RATE_PER_SEC=0`) for a site that would rather have the old
        // spreading than the prompt adoption.
        let mut ws = WorkingSet::with_adopt_rate(0);
        let mut seen = Vec::new();
        ws.apply(
            snapshot(Uuid::from_u128(1), 1, 0, 1, many_nodes(4, 30)),
            Instant::now(),
            &mut recording(&mut seen),
        );
        assert!(
            seen.iter().all(|&b| b == 30_000),
            "rate 0 means no window, so the interval is the only bound"
        );
    }

    #[test]
    fn a_short_interval_is_never_stretched_by_a_long_window() {
        // A 10s check adopted alongside 10,000 others must not be pushed out to the 50s window —
        // the clamp is per spec, so spreading can only ever shorten a spec's wait, never lengthen it.
        let mut ws = WorkingSet::with_adopt_rate(200);
        let mut seen = Vec::new();
        ws.apply(
            snapshot(Uuid::from_u128(1), 1, 0, 1, many_nodes(10_000, 10)),
            Instant::now(),
            &mut recording(&mut seen),
        );
        assert!(
            seen.iter().all(|&b| b == 10_000),
            "the 50s window must clamp down to the 10s interval"
        );
    }

    // ── A scarce budget goes to whatever is latest relative to its own interval (ADR-109 Inc.4) ──
    //
    // Before this, `due` returned everything and `assignment.rs` pushed the whole batch before it
    // looked at the clock again, so the order was the walk order — node by node, and a node's specs
    // are adjacent. A 3600 s check therefore went out as often as a 60 s one. These pin the rule
    // that replaced it, and the one line it rests on.

    /// One node per id, one spec each, at the interval given. The shape the ranking is about:
    /// specs on *different* nodes, competing for the same budget.
    fn fleet(specs: &[(u128, u32)]) -> Vec<NodeJobs> {
        specs
            .iter()
            .map(|&(n, interval)| NodeJobs {
                node_id: node(n),
                specs: vec![icmp_spec(node(n), interval)],
            })
            .collect()
    }

    fn seeded(nodes: Vec<NodeJobs>, now: Instant) -> WorkingSet {
        let mut ws = WorkingSet::new();
        let mut rng = fixed(0); // no jitter: every spec is due at `now`
        assert_eq!(
            ws.apply(snapshot(Uuid::from_u128(1), 1, 0, 1, nodes), now, &mut rng),
            ApplyOutcome::Applied
        );
        ws
    }

    /// **The accepting side, and it comes first on purpose.**
    ///
    /// Every other assertion here is of the form "the budget cut something". An implementation that
    /// returned nothing, or that always deferred, satisfies all of them — and would stop the poller
    /// polling entirely while this module stayed green
    /// (`rejection-only-tests-pass-when-everything-rejects`). A budget that fits must behave exactly
    /// as the unbudgeted `due` did, because that is the case a healthy deployment is always in.
    #[test]
    fn everything_due_is_minted_when_it_fits() {
        let now = Instant::now();
        let mut ws = seeded(
            vec![
                one_node_three_specs(node(1), 60),
                one_node_three_specs(node(2), 60),
            ],
            now,
        );
        // Three seconds past `now` clears the per-spec stagger (SPEC_STAGGER_MS is two ticks, so
        // a node’s third spec sits 2 s out), leaving all six genuinely due.
        let at = now + Duration::from_secs(3);
        let jobs = ws.due(at, 6);
        assert_eq!(jobs.len(), 6, "a budget that fits mints every due spec");
        assert_eq!(ws.deferred(), 0, "and nothing was held back");
        assert!(
            ws.due(at, 6).is_empty(),
            "…and every one of them was re-armed, as before"
        );
    }

    /// The rank is `behind / interval`, not `behind`, and not the node id.
    ///
    /// 🚨 **The hourly node is given the *lower* id deliberately.** Both specs are exactly one
    /// minute late, so a rule that fell back to walk order or to the id tie-break would pick the
    /// hourly one — which is the behaviour this increment exists to remove.
    #[test]
    fn the_most_overdue_relative_to_its_own_interval_goes_first() {
        let now = Instant::now();
        let mut ws = seeded(fleet(&[(1, 3600), (2, 60)]), now);
        let jobs = ws.due(now + Duration::from_secs(60), 1);
        assert_eq!(jobs.len(), 1, "the budget was one");
        assert_eq!(
            jobs[0].node_id,
            node(2),
            "a 60 s check a minute late has missed a whole cycle; a 3600 s check a minute late has \
             missed a sixtieth of one"
        );
        assert_eq!(ws.deferred(), 1, "the hourly spec is the one held back");
    }

    /// 🚨 **A spec that loses keeps its `next_due`** — the one line the whole design rests on.
    ///
    /// Advance a deferred spec's timer and its lateness stops accumulating, so a long-interval check
    /// can never out-rank a short one and never runs again. Every other test in this file passes
    /// against that wrong version, because they all read the *winner*.
    #[test]
    fn a_spec_that_lost_keeps_its_turn() {
        let now = Instant::now();
        let mut ws = seeded(fleet(&[(1, 3600), (2, 60)]), now);
        let at = now + Duration::from_secs(60);
        let before = ws.nodes[&node(1)][0].next_due;

        assert_eq!(
            ws.due(at, 1)[0].node_id,
            node(2),
            "the 60 s check wins first"
        );
        assert_eq!(
            ws.nodes[&node(1)][0].next_due,
            before,
            "the loser's timer must not move — that is what lets its lateness accumulate"
        );
        assert_eq!(
            ws.due(at, 1)[0].node_id,
            node(1),
            "…so it takes the very next slot, rather than waiting another whole interval"
        );
    }

    /// **Under a mild deficit the served ratio comes back to the configured one.**
    ///
    /// 100 checks a minute and 100 checks an hour is a demand ratio of 60 : 1. The budget here
    /// serves one poll a second against 1.694 demanded, so 41% of the schedule cannot run — and the
    /// question is which 41%.
    ///
    /// Both bounds are load-bearing and they exclude opposite failures: the measured defect served
    /// them at **1.03 : 1** (walk order, so the hourly tier ran as often as the per-minute one),
    /// while a strict "shortest interval always wins" rule would serve the hourly tier **zero**
    /// times. This lands at 49.7 : 1.
    ///
    /// ⚠️ **"Mild" is doing work in that first line, and the hour here is not enough to see the
    /// other regime.** The rank equalises `behind / interval`, so a starved 3600 s check only
    /// out-ranks a 60 s one once it has accumulated `3600 × stretch` seconds of lateness. At 1.7×
    /// that is under an hour and this test sees it. Simulated at 14× over-subscription it is
    /// **12.6 hours**, and the hourly tier then arrives all at once — proportional in the long run,
    /// indistinguishable from starvation on any shorter window. [`WorkingSet::due`]'s doc carries
    /// the numbers; do not read this test as a promise about a fleet far past its capacity.
    #[test]
    fn a_starved_hourly_check_eventually_wins_and_the_ratio_comes_back() {
        let now = Instant::now();
        let fleet_spec: Vec<(u128, u32)> = (0..100)
            .map(|i| (i, 60))
            .chain((100..200).map(|i| (i, 3600)))
            .collect();
        let mut ws = seeded(fleet(&fleet_spec), now);

        let (mut fast, mut slow) = (0usize, 0usize);
        for second in 1..=3600u64 {
            for job in ws.due(now + Duration::from_secs(second), 1) {
                if job.interval_secs == 60 {
                    fast += 1;
                } else {
                    slow += 1;
                }
            }
        }
        assert_eq!(fast + slow, 3600, "one poll a second, for an hour");
        assert!(
            slow > 0,
            "the hourly tier was never served at all — that is strict priority, not the configured \
             ratio ({fast} fast / {slow} slow)"
        );
        let ratio = fast as f64 / slow as f64;
        assert!(
            ratio > 10.0,
            "served {ratio:.2} : 1 against a configured 60 : 1 ({fast} fast / {slow} slow). \
             The measured defect served 1.03 : 1"
        );
    }

    /// Equal lateness is broken deterministically, not by `HashMap` order.
    ///
    /// Two working sets built from the same snapshot have different hash seeds, so without the
    /// `(node, index)` tie-break the boundary of a truncated batch would differ between them — and
    /// therefore between two restarts of the same poller, which no test could pin.
    #[test]
    fn equal_lateness_is_broken_deterministically() {
        let now = Instant::now();
        let spec = fleet(&[(1, 60), (2, 60), (3, 60), (4, 60)]);
        let served = |mut ws: WorkingSet| -> Vec<NodeId> {
            ws.due(now + Duration::from_secs(60), 2)
                .into_iter()
                .map(|j| j.node_id)
                .collect()
        };
        let a = served(seeded(spec.clone(), now));
        let b = served(seeded(spec, now));
        assert_eq!(a.len(), 2);
        assert_eq!(
            a, b,
            "the same fleet must be served in the same order twice"
        );
    }

    /// A deferred spec's lost cycles are counted when it finally runs.
    ///
    /// 🚨 **`yagra_poll_cycles_missed_total` saw this only by luck before.** The backlog used to sit in
    /// the scheduler’s send loop rather than in the working set, so how much of the shortfall reached
    /// the counter depended on how often that loop asked — flat at **0** through a settled ten-minute
    /// window on a poller serving 62.6 of 1,017 demanded polls per second, and 361,107 right after
    /// that same poller adopted its assignment (both 2026-08-30). Keeping a loser’s `next_due` puts
    /// the backlog in the set by construction.
    #[test]
    fn a_deferred_spec_is_counted_when_it_finally_runs() {
        let now = Instant::now();
        let mut ws = seeded(fleet(&[(1, 60), (2, 60)]), now);

        // One minute on: both are due, one is served. It is one cycle late, so one is counted.
        ws.due(now + Duration::from_secs(61), 1);
        assert_eq!(ws.take_cycles_missed(), 1);

        // Another minute: the loser has now been waiting two whole cycles, and says so.
        ws.due(now + Duration::from_secs(121), 1);
        assert_eq!(
            ws.take_cycles_missed(),
            2,
            "the spec that was held back reports the cycles it lost, not just the last one"
        );
    }
}
