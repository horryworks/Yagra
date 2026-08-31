// SPDX-License-Identifier: AGPL-3.0-only
//! The ordered index over `next_due` that makes [`crate::working_set::WorkingSet::due`] cost what
//! it dispatches rather than what it holds (ADR-109 Increment 5).
//!
//! **What this module is not.** It holds no [`yagra_bus::JobSpec`], mints no job and knows nothing
//! about the sync protocol — only `(interval, next_due, node, index)`. `working_set.rs` owns the
//! pairing back to the specs; this owns the order. The split is the same one ADR-089/096/102 draw:
//! a structure with its own invariants gets its own tests, and those tests are not about deltas and
//! epochs.
//!
//! # Why the index is per interval, and why one heap would be wrong
//!
//! ADR-109 Increment 4 ranks a scarce budget by **how late a spec is as a fraction of its own
//! interval**. That fraction moves with the clock, so it is not a static order:
//!
//! ```text
//! (now − d1)/i1 > (now − d2)/i2   ⟺   now·(i2 − i1) > d1·i2 − d2·i1
//! ```
//!
//! With `i1 ≠ i2` the two sides trade places as `now` advances — **a single heap keyed by
//! `next_due` does not reproduce the ranking.** But set `i1 = i2` and the right-hand side collapses
//! to `d1 < d2`: within one interval, "latest relative to its interval" *is* "earliest deadline",
//! which is static and heap-able.
//!
//! So the index is one min-heap per distinct interval. Each heap's root is the best candidate of
//! its tier, and the global best is found by comparing **only the roots** with the very same
//! comparison Increment 4 used. One dispatch costs `tiers` comparisons plus two `log n` heap
//! operations, against a walk of the whole set.
//!
//! ⚠️ **The number of tiers is bounded by the interval vocabulary, not by the fleet.** The API
//! accepts `10..=3600` seconds, so the worst case is 3,591 tiers and a dispatch costing 3,591
//! comparisons — still two orders of magnitude under the walk it replaces, and a real fleet has
//! five to ten. If that ever stops being true the answer is a heap of roots on top of these, which
//! is a separate change and not worth its complexity now.
//!
//! 🚨 **This is a second copy of every spec's `next_due`, and a spec missing from it is a spec that
//! is never polled again** — `working_set.rs` says what keeps the two in step, and why that is the
//! dangerous half of this increment.

use std::cmp::{Ordering, Reverse};
use std::collections::{BTreeMap, BinaryHeap};
use std::time::Instant;

use yagra_common::NodeId;

/// One scheduled spec's place in the order: when it is due, and which spec it is.
///
/// `Ord` is derived, so the field order *is* the ordering — `next_due` first, then the two
/// identity fields. Wrapped in [`Reverse`] inside a [`BinaryHeap`] this yields earliest-deadline
/// first, and the identity fields settle ties.
///
/// 🚨 **The tie-break is not cosmetic.** Specs that have only just come due are all at the same
/// lateness, so without a deterministic second key the boundary of a truncated batch would be
/// decided by `HashMap` iteration order — which is seeded per process, so the same fleet would be
/// served differently after every restart and no test could pin it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TimerKey {
    pub(crate) next_due: Instant,
    pub(crate) node: NodeId,
    pub(crate) index: usize,
}

/// Order two due specs so the one that has waited longest **as a fraction of its own interval**
/// comes first.
///
/// That fraction is what makes the ranking mean "keep the configured ratio". A 60 s check one
/// minute late has waited a whole cycle; a 3600 s check one minute late has waited 1/60th of one,
/// and the second is the one that can afford to wait. Under sustained scarcity the fractions
/// equalise across the whole set, which reproduces the demand ratio the intervals declare.
///
/// The lateness is compared as the pair `behind / interval` rather than as a ratio, so the ordering
/// is total and exact: `f64` has no `Ord`, and a ratio computed in floating point would make the
/// boundary of a truncated batch depend on rounding. Comparing two is one cross-multiplication, and
/// `u128` has room for it — the largest product in play is a process uptime in milliseconds times
/// an hour in milliseconds.
///
/// A zero interval cannot arrive through the API (`10..=3600`) but can over the bus. Ranking it as
/// if it were 1 ms keeps the comparison total without letting it monopolise: at `behind = 0` it
/// still sorts with everything else that has only just come due.
fn later_first(
    now: Instant,
    (interval_a, a): (u32, &TimerKey),
    (interval_b, b): (u32, &TimerKey),
) -> Ordering {
    let behind = |k: &TimerKey| now.saturating_duration_since(k.next_due).as_millis();
    let ms = |secs: u32| u128::from(secs).saturating_mul(1000).max(1);
    (behind(b) * ms(interval_a))
        .cmp(&(behind(a) * ms(interval_b)))
        .then_with(|| a.node.cmp(&b.node))
        .then_with(|| a.index.cmp(&b.index))
}

/// Every scheduled spec, filed by interval and ordered by deadline within each.
#[derive(Debug, Default)]
pub(crate) struct Timers {
    /// `interval_secs` → that tier's specs, earliest deadline at the root.
    tiers: BTreeMap<u32, BinaryHeap<Reverse<TimerKey>>>,
    /// How many keys are filed, maintained rather than summed — [`crate::assignment`] publishes it
    /// on every sync as the consistency check described in `working_set.rs`.
    len: usize,
}

impl Timers {
    /// Build the whole index from `(interval_secs, key)` pairs.
    ///
    /// Collect-then-heapify rather than push-per-key: [`BinaryHeap::from`] is `O(n)`, a push loop
    /// is `O(n log n)`, and this runs on every applied snapshot over the whole working set.
    pub(crate) fn build(entries: impl IntoIterator<Item = (u32, TimerKey)>) -> Self {
        let mut buckets: BTreeMap<u32, Vec<Reverse<TimerKey>>> = BTreeMap::new();
        let mut len = 0usize;
        for (interval, key) in entries {
            buckets.entry(interval).or_default().push(Reverse(key));
            len += 1;
        }
        Self {
            tiers: buckets
                .into_iter()
                .map(|(interval, keys)| (interval, BinaryHeap::from(keys)))
                .collect(),
            len,
        }
    }

    /// File one key back under its interval.
    pub(crate) fn push(&mut self, interval_secs: u32, key: TimerKey) {
        self.tiers
            .entry(interval_secs)
            .or_default()
            .push(Reverse(key));
        self.len += 1;
    }

    /// Take the best spec that is due at/by `now`, or `None` if nothing is.
    ///
    /// "Best" is [`later_first`] over the tier roots. Only the roots are inspected — a tier whose
    /// root is not yet due contributes nothing and is skipped after one comparison, because its
    /// root is by construction the earliest deadline it holds.
    ///
    /// `examined` counts roots looked at, which is what the cost claim of this module *is*; the
    /// caller asserts on it (see `working_set.rs`).
    pub(crate) fn pop_best(
        &mut self,
        now: Instant,
        examined: &mut usize,
    ) -> Option<(u32, TimerKey)> {
        let mut best: Option<(u32, TimerKey)> = None;
        for (&interval, heap) in &self.tiers {
            let Some(Reverse(root)) = heap.peek() else {
                continue;
            };
            *examined += 1;
            if root.next_due > now {
                continue; // this tier's earliest is still in the future, so none of it is due
            }
            // `best` is `Copy`, so match on the value: assigning to it inside a `match &best` arm
            // would hold a borrow across the write.
            let takes_the_lead = match best {
                Some((best_interval, best_key)) => {
                    later_first(now, (interval, root), (best_interval, &best_key)) == Ordering::Less
                }
                None => true,
            };
            if takes_the_lead {
                best = Some((interval, *root));
            }
        }
        let (interval, _) = best?;
        let popped = self.tiers.get_mut(&interval)?.pop()?;
        self.len -= 1;
        Some((interval, popped.0))
    }

    /// How many keys are filed. Equals the working set's spec count, and `working_set.rs` explains
    /// why that equality is worth publishing.
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Every filed `(interval, key)`, for the invariant test in `working_set.rs`.
    #[cfg(test)]
    pub(crate) fn entries(&self) -> Vec<(u32, TimerKey)> {
        self.tiers
            .iter()
            .flat_map(|(&interval, heap)| heap.iter().map(move |r| (interval, r.0)))
            .collect()
    }

    /// How many distinct intervals are filed — the per-dispatch comparison count.
    #[cfg(test)]
    pub(crate) fn tier_count(&self) -> usize {
        self.tiers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use uuid::Uuid;

    fn key(n: u128, due: Instant) -> TimerKey {
        TimerKey {
            next_due: due,
            node: NodeId::from(Uuid::from_u128(n)),
            index: 0,
        }
    }

    /// A `(base, now)` pair an hour apart, so a deadline can be placed in the past without
    /// subtracting from [`Instant::now`] — which is allowed to panic when the result would precede
    /// the monotonic clock's own origin.
    fn clock() -> (Instant, Instant) {
        let base = Instant::now();
        (base, base + Duration::from_secs(3600))
    }

    /// **The accepting side, and it comes first on purpose.** Every other test here says the index
    /// withheld something; an implementation that always returned `None` satisfies all of them and
    /// would stop the poller polling entirely (`rejection-only-tests-pass-when-everything-rejects`).
    #[test]
    fn everything_filed_comes_back_out() {
        let now = Instant::now();
        let mut t = Timers::build((0..50).map(|n| (60, key(n, now))));
        assert_eq!(t.len(), 50);
        let mut seen = 0;
        let mut examined = 0;
        while t.pop_best(now, &mut examined).is_some() {
            seen += 1;
        }
        assert_eq!(seen, 50, "every filed key must be reachable");
        assert_eq!(t.len(), 0);
    }

    /// Within one tier the order is the deadline order, which is what makes the tier heap-able.
    #[test]
    fn one_tier_comes_out_earliest_deadline_first() {
        let (base, now) = clock();
        // Filed out of order on purpose.
        let mut t = Timers::build([
            (60, key(3, base + Duration::from_secs(3599))),
            (60, key(1, base + Duration::from_secs(3591))),
            (60, key(2, base + Duration::from_secs(3595))),
        ]);
        let mut examined = 0;
        let order: Vec<u128> = std::iter::from_fn(|| t.pop_best(now, &mut examined))
            .map(|(_, k)| k.node.0.as_u128())
            .collect();
        assert_eq!(order, vec![1, 2, 3]);
    }

    /// The cross-tier rule, and the case a single deadline-ordered heap gets wrong.
    ///
    /// 🚨 The hourly key has the **earlier** deadline and the **lower** node id, so a plain
    /// earliest-deadline heap — and a rule that fell back to the id tie-break — would both pick it.
    /// A minute late is a whole cycle for the 60 s spec and a sixtieth of one for the 3600 s spec.
    #[test]
    fn across_tiers_the_ranking_is_lateness_over_interval_not_deadline() {
        let (base, now) = clock();
        let mut t = Timers::build([
            (3600, key(1, base + Duration::from_secs(3510))),
            (60, key(2, base + Duration::from_secs(3540))),
        ]);
        let mut examined = 0;
        let (interval, k) = t.pop_best(now, &mut examined).expect("something is due");
        assert_eq!(interval, 60);
        assert_eq!(k.node.0.as_u128(), 2);
    }

    /// A tier whose earliest is in the future contributes nothing, and costs one comparison.
    #[test]
    fn a_tier_with_nothing_due_is_skipped_not_served() {
        let (base, now) = clock();
        let mut t = Timers::build([
            (60, key(1, now + Duration::from_secs(30))),
            (3600, key(2, base + Duration::from_secs(3599))),
        ]);
        let mut examined = 0;
        let (interval, k) = t
            .pop_best(now, &mut examined)
            .expect("the hourly one is due");
        assert_eq!(interval, 3600);
        assert_eq!(k.node.0.as_u128(), 2);
        assert_eq!(
            t.pop_best(now, &mut examined),
            None,
            "the 60 s tier is not due yet and must not be served early"
        );
    }

    /// Equal lateness resolves the same way twice, whatever order the keys were filed in.
    #[test]
    fn equal_lateness_is_broken_by_identity_not_by_filing_order() {
        let now = Instant::now();
        let forward = Timers::build((0..4).map(|n| (60, key(n, now))));
        let backward = Timers::build((0..4).rev().map(|n| (60, key(n, now))));
        let drain = |mut t: Timers| -> Vec<u128> {
            let mut examined = 0;
            std::iter::from_fn(move || t.pop_best(now, &mut examined))
                .map(|(_, k)| k.node.0.as_u128())
                .collect()
        };
        assert_eq!(drain(forward), vec![0, 1, 2, 3]);
        assert_eq!(drain(backward), vec![0, 1, 2, 3]);
    }

    /// The cost claim, stated as a test: one dispatch inspects one root per tier and nothing else.
    #[test]
    fn a_dispatch_inspects_one_root_per_tier() {
        let now = Instant::now();
        let mut t = Timers::build((0..10_000).map(|n| (60 + (n % 3) as u32 * 60, key(n, now))));
        assert_eq!(t.tier_count(), 3);
        let mut examined = 0;
        for _ in 0..10 {
            assert!(t.pop_best(now, &mut examined).is_some());
        }
        assert_eq!(
            examined, 30,
            "ten dispatches over three tiers must inspect thirty roots, not the 10,000 keys filed"
        );
    }

    /// A zero interval is ranked as 1 ms rather than dividing, and does not monopolise.
    #[test]
    fn a_zero_interval_does_not_divide_and_does_not_starve_the_others() {
        let now = Instant::now();
        let mut t = Timers::build([(0, key(9, now)), (60, key(1, now))]);
        let mut examined = 0;
        assert!(t.pop_best(now, &mut examined).is_some());
        assert!(t.pop_best(now, &mut examined).is_some());
        assert_eq!(
            t.pop_best(now, &mut examined),
            None,
            "both were served once"
        );
    }
}
