// SPDX-License-Identifier: AGPL-3.0-only
//! The poller's local working set — a pure state machine (ADR-009/020).
//!
//! In the distributed-poller model core no longer publishes every [`PollJob`] each tick. It hands
//! each poller the set of polling *specs* it owns as a full **snapshot** (chunked) plus incremental
//! **deltas**, and the poller schedules them locally. This module owns that set and the sync
//! protocol that keeps it consistent — with **no I/O**: [`WorkingSet::apply`] folds a [`SyncMsg`]
//! into the set and reports whether the poller needs to resync, [`WorkingSet::due`] pops the specs
//! whose local timers have fired and mints fresh jobs. Clock (`now`) and jitter source (`rng`) are
//! injected so every rule here is deterministically unit-testable without a bus, a clock, or real
//! randomness.
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

/// How often the poller's local scheduler asks [`WorkingSet::due`] what has fired (`main.rs`'s
/// `run_local_scheduler` owns the timer and reads this constant rather than repeating the literal).
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
        }
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

    /// Pop every spec whose timer has fired at/by `now`, minting a fresh [`PollJob`] id for each and
    /// re-arming it one interval ahead. If a spec has fallen more than a full interval behind (e.g.
    /// the poller was paused through a WAN blip), its timer re-anchors to `now + interval` rather
    /// than emitting a backlog burst — at most one make-up poll per spec per call (catch-up cap).
    pub fn due(&mut self, now: Instant) -> Vec<PollJob> {
        let mut out = Vec::new();
        for specs in self.nodes.values_mut() {
            for sched in specs.iter_mut() {
                if sched.next_due <= now {
                    out.push(sched.spec.to_job(Uuid::new_v4()));
                    let interval = Duration::from_secs(u64::from(sched.spec.interval_secs));
                    sched.next_due += interval;
                    if sched.next_due <= now {
                        // Still behind after one step → re-anchor so we don't fire every tick.
                        sched.next_due = now + interval;
                    }
                }
            }
        }
        out
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
            for job in ws.due(now + elapsed) {
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
        let jobs = ws.due(now);
        assert_eq!(jobs.len(), 1);
        // Not due again until an interval passes.
        assert!(ws.due(now).is_empty());
        assert_eq!(ws.due(now + Duration::from_secs(30)).len(), 1);
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
        let j1 = ws.due(now).remove(0);
        let j2 = ws.due(now + Duration::from_secs(30)).remove(0);
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
        let jobs = ws.due(much_later);
        assert_eq!(jobs.len(), 1, "only one make-up poll, not a backlog burst");
        // Re-anchored to now + interval, not left far in the past.
        assert_eq!(
            ws.nodes[&node(1)][0].next_due,
            much_later + Duration::from_secs(30)
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
}
