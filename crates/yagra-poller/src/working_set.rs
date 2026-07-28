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
//! Anti-stampede (monitoring-conventions): a newly-scheduled spec gets `next_due = now + jitter`
//! (uniform in `[0, interval)`), but a spec that survives a snapshot unchanged **keeps its previous
//! phase** so a resync doesn't bunch every poll onto the same tick.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use uuid::Uuid;
use yagra_bus::{JobSpec, NodeJobs, PollJob, SyncMsg, WorkingSetDelta, WorkingSetSnapshot};
use yagra_common::NodeId;

/// One scheduled polling spec: the reusable [`JobSpec`] plus the local time its next poll is due.
struct ScheduledSpec {
    spec: JobSpec,
    next_due: Instant,
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
}

impl Default for WorkingSet {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkingSet {
    /// An empty working set with no epoch and no pending snapshot.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            epoch: None,
            last_seq: 0,
            pending: None,
        }
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
        for nj in delta.upserts {
            let scheduled = reschedule(self.nodes.get(&nj.node_id), nj.specs, now, rng);
            self.nodes.insert(nj.node_id, scheduled);
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
        let mut new_nodes: HashMap<NodeId, Vec<ScheduledSpec>> = HashMap::new();
        for node_jobs in group.chunks.into_values().flatten() {
            let scheduled = reschedule(old.get(&node_jobs.node_id), node_jobs.specs, now, rng);
            new_nodes.insert(node_jobs.node_id, scheduled);
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
}

/// Build the scheduled specs for a node from its new spec list, donating each surviving spec's
/// previous `next_due` (matched by full [`JobSpec`] equality, greedily, so duplicates pair up) and
/// jittering only the specs that are new or changed. This is what keeps a snapshot/upsert from
/// re-phasing an unchanged poll (anti-stampede, monitoring-conventions).
fn reschedule(
    old: Option<&Vec<ScheduledSpec>>,
    new_specs: Vec<JobSpec>,
    now: Instant,
    rng: &mut impl FnMut(u32) -> u32,
) -> Vec<ScheduledSpec> {
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
        let next_due = reused.unwrap_or_else(|| jitter(now, spec.interval_secs, rng));
        out.push(ScheduledSpec { spec, next_due });
    }
    out
}

/// `now + uniform([0, interval))`, computed in milliseconds. A zero interval yields `now` (no jitter,
/// `rng` is never called with a zero bound).
fn jitter(now: Instant, interval_secs: u32, rng: &mut impl FnMut(u32) -> u32) -> Instant {
    let bound = interval_secs.saturating_mul(1000);
    let offset_ms = if bound == 0 { 0 } else { rng(bound) };
    now + Duration::from_millis(u64::from(offset_ms))
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
}
