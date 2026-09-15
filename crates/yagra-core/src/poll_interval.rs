// SPDX-License-Identifier: AGPL-3.0-only
//! How far apart a node's polls are, as core needs to know it outside the scheduler (ADR-144).
//!
//! The scheduler resolves a node's effective interval — its profile's override, else the
//! deployment default — and until ADR-144 nothing else asked. Three readers have to agree with that
//! answer, and each had quietly assumed polls thirty seconds apart:
//!
//! - **the `rate()` window a counter is read through.** A window holding fewer than two samples has
//!   no rate, so a 300-second window over a 600-second poll draws five minutes of value and five of
//!   nothing — measured on the PoC box as 25 zero points in 50 ([`rate_window_secs`]);
//! - **the dwell of an alert evaluated once a minute rather than once a poll.** Reading one poll
//!   five times satisfied "3 breaches" with a single poll ([`dwell_ticks`]);
//! - **the flapping window.** Five transitions in ten minutes cannot happen when polls are five
//!   minutes apart, so flapping was never detected at all ([`flap_window_ms`]).
//!
//! The rules are pure. The answer they are applied to lives in a [`PollIntervals`] handle that the
//! scheduler publishes into on every rebuild and everything else reads.
//!
//! ⚠️ **Unknown means today's behaviour.** Before the first publish — right after start, on a
//! standby core, in any test that does not publish — every rule here answers exactly what it
//! answered before ADR-144. A snapshot that never arrives can therefore make nothing worse.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use uuid::Uuid;

/// The narrowest `rate()` window any counter read uses, in seconds.
///
/// Five minutes covers several polls at the intervals it was chosen for, so one missed poll does
/// not blank a rate. It was written down twice before ADR-144 (`api/util.rs` and `store.rs`); this
/// is the one copy.
pub const RATE_WINDOW_FLOOR_SECS: u64 = 300;

/// A `rate()` window wide enough to hold two samples of a counter polled every `interval` seconds,
/// and never narrower than `floor`.
///
/// `max(floor, 2 × interval)`. Below 150 seconds the default floor wins, so a deployment polling
/// every thirty seconds asks VictoriaMetrics exactly what it asked before. The window is **not**
/// left out for MetricsQL to choose: measured at a 60-second interval, a bare `rate(m)` reads as
/// `[60s]` and doubles the chart's peak and quadruples its jitter against `[300s]`.
#[must_use]
pub fn rate_window_secs(floor: u64, interval: Option<u32>) -> u64 {
    let floor = floor.max(1);
    interval.map_or(floor, |secs| floor.max(u64::from(secs).saturating_mul(2)))
}

/// How many evaluator ticks a rule's `dwell` needs, for a check read once per `tick` rather than
/// once per poll.
///
/// A rule's dwell means consecutive polls. An evaluator that ticks faster than the node polls
/// re-reads one poll several times, so counting ticks lets one slow poll satisfy the whole dwell.
/// This counts enough ticks to span `dwell` polls — and **never fewer ticks than `dwell`**, so a
/// node polling faster than the tick keeps exactly the damping it had (three ticks is three
/// minutes, as the rules screen has always said).
#[must_use]
pub fn dwell_ticks(dwell: u32, interval: Option<u32>, tick: Duration) -> u32 {
    let Some(secs) = interval else {
        return dwell;
    };
    let tick_secs = tick.as_secs().max(1);
    let spanning = u64::from(dwell)
        .saturating_mul(u64::from(secs))
        .div_ceil(tick_secs);
    dwell.max(u32::try_from(spanning).unwrap_or(u32::MAX))
}

/// The flap window before ADR-144, and still its lower bound.
const FLAP_WINDOW_FLOOR_MS: i64 = 600_000;

/// How many polls one flap window spans. Twenty is the ratio the fixed window had at the
/// thirty-second default (600 s ÷ 30 s), so a thirty-second deployment keeps its window exactly.
const FLAP_WINDOW_POLLS: i64 = 20;

/// The flapping window for a check whose samples arrive every `interval` seconds:
/// `max(10 minutes, 20 polls)`.
#[must_use]
pub fn flap_window_ms(interval: Option<u32>) -> i64 {
    interval.map_or(FLAP_WINDOW_FLOOR_MS, |secs| {
        FLAP_WINDOW_FLOOR_MS.max(
            i64::from(secs)
                .saturating_mul(FLAP_WINDOW_POLLS)
                .saturating_mul(1000),
        )
    })
}

/// The most nodes a fleet-wide query may name before [`PollIntervals::window_classes`] gives up
/// naming them and widens the whole fleet to its slowest window instead.
///
/// ⚠️ A judgement, not a measurement: it keeps the named batches well inside the candidate query's
/// selector budget (`store.rs`) at the few distinct intervals a real deployment has.
const NAMED_NODES_MAX: usize = 1_000;

/// Every node's effective poll interval as of one scheduler rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntervalSnapshot {
    default_secs: u32,
    /// Only the nodes whose interval differs from the default — at the default, every node would
    /// otherwise cost an entry that says nothing.
    by_node: HashMap<Uuid, u32>,
    /// The slowest interval in play, the default included. Including the default is what keeps a
    /// node added since this rebuild (which polls at the default) from falling outside every
    /// window a fleet-wide read uses.
    fleet_max: u32,
}

impl IntervalSnapshot {
    /// The snapshot for `default_secs` and each node's resolved interval.
    #[must_use]
    pub fn build(default_secs: u32, nodes: impl IntoIterator<Item = (Uuid, u32)>) -> Self {
        let mut by_node = HashMap::new();
        let mut fleet_max = default_secs;
        for (node, secs) in nodes {
            fleet_max = fleet_max.max(secs);
            if secs != default_secs {
                by_node.insert(node, secs);
            }
        }
        Self {
            default_secs,
            by_node,
            fleet_max,
        }
    }

    /// The snapshot a rebuild may publish: `None` unless every read it was resolved from
    /// succeeded.
    ///
    /// 🚨 The scheduler degrades a failed read of the default or of the profile overrides to the
    /// compiled default so that polling carries on. Publishing that degraded answer would move
    /// every window, dwell and flap window in the fleet for the length of a database hiccup, so
    /// the last good snapshot stays in place instead.
    #[must_use]
    pub fn publishable(
        reads_succeeded: bool,
        default_secs: u32,
        nodes: impl IntoIterator<Item = (Uuid, u32)>,
    ) -> Option<Self> {
        reads_succeeded.then(|| Self::build(default_secs, nodes))
    }

    /// `node`'s interval. A node this snapshot does not name polls at the default.
    #[must_use]
    pub fn for_node(&self, node: Uuid) -> u32 {
        self.by_node
            .get(&node)
            .copied()
            .unwrap_or(self.default_secs)
    }
}

/// One set of nodes a counter query reads through one `rate()` window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowClass {
    /// The `rate()` window, in seconds.
    pub window_secs: u64,
    /// The nodes to name. `None` is every node no other class names — the fleet remainder, or the
    /// whole fleet when it is the only class.
    pub nodes: Option<Vec<Uuid>>,
}

/// Which class an answered row belongs to, when a query was split into several.
///
/// A class's query may answer for more nodes than it named — the store falls back to one
/// fleet-wide query when a set is too large to name — so each row is kept only by the class that
/// owns its node. Without that, a node would be reported twice, once through the wrong window.
#[derive(Debug, Default)]
pub struct ClassOwners {
    named: HashMap<Uuid, usize>,
    remainder: Option<usize>,
}

impl ClassOwners {
    /// The owners for `classes`.
    #[must_use]
    pub fn of(classes: &[WindowClass]) -> Self {
        let mut owners = Self::default();
        for (index, class) in classes.iter().enumerate() {
            match &class.nodes {
                Some(nodes) => {
                    for node in nodes {
                        owners.named.insert(*node, index);
                    }
                }
                None => owners.remainder = Some(index),
            }
        }
        owners
    }

    /// Whether class `index` owns rows about `node`.
    #[must_use]
    pub fn owns(&self, index: usize, node: Uuid) -> bool {
        match self.named.get(&node) {
            Some(&owner) => owner == index,
            None => self.remainder == Some(index),
        }
    }
}

/// The split of one counter query into window classes, from a snapshot that may not exist yet.
fn window_classes(
    snapshot: Option<&IntervalSnapshot>,
    floor: u64,
    scope: Option<&[Uuid]>,
) -> Vec<WindowClass> {
    let Some(snap) = snapshot else {
        // Unknown: one class through the floor, over exactly the scope asked for — today's query.
        return vec![WindowClass {
            window_secs: rate_window_secs(floor, None),
            nodes: scope.map(<[Uuid]>::to_vec),
        }];
    };
    match scope {
        Some(ids) => {
            let mut groups: BTreeMap<u64, Vec<Uuid>> = BTreeMap::new();
            for &id in ids {
                let window = rate_window_secs(floor, Some(snap.for_node(id)));
                groups.entry(window).or_default().push(id);
            }
            if groups.is_empty() {
                return vec![WindowClass {
                    window_secs: rate_window_secs(floor, Some(snap.default_secs)),
                    nodes: Some(Vec::new()),
                }];
            }
            groups
                .into_iter()
                .map(|(window_secs, nodes)| WindowClass {
                    window_secs,
                    nodes: Some(nodes),
                })
                .collect()
        }
        None => {
            let base = rate_window_secs(floor, Some(snap.default_secs));
            let mut named: BTreeMap<u64, Vec<Uuid>> = BTreeMap::new();
            for (&id, &secs) in &snap.by_node {
                let window = rate_window_secs(floor, Some(secs));
                if window != base {
                    named.entry(window).or_default().push(id);
                }
            }
            let count: usize = named.values().map(Vec::len).sum();
            if count == 0 {
                return vec![WindowClass {
                    window_secs: base,
                    nodes: None,
                }];
            }
            if count > NAMED_NODES_MAX {
                // Too many to name: one query through the slowest window. A window too wide only
                // smooths a faster node's rate; one too narrow blanks a slower node's.
                return vec![WindowClass {
                    window_secs: rate_window_secs(floor, Some(snap.fleet_max)),
                    nodes: None,
                }];
            }
            let mut classes = vec![WindowClass {
                window_secs: base,
                nodes: None,
            }];
            classes.extend(named.into_iter().map(|(window_secs, mut nodes)| {
                // `by_node` is a hash map; sorted so the same fleet always builds the same query.
                nodes.sort_unstable();
                WindowClass {
                    window_secs,
                    nodes: Some(nodes),
                }
            }));
            classes
        }
    }
}

/// The poll intervals the scheduler last published, shared with everything that reads counters or
/// judges alerts.
///
/// Cheap to clone (one `Arc`) and cheap to read (one uncontended read lock, no allocation), because
/// the alert engine reads it once per poll result.
#[derive(Debug, Clone, Default)]
pub struct PollIntervals(Arc<RwLock<Option<IntervalSnapshot>>>);

impl PollIntervals {
    /// A handle nothing has published into yet — every read answers "unknown".
    #[must_use]
    pub fn unknown() -> Self {
        Self::default()
    }

    /// Replace the snapshot. Called by the scheduler after a rebuild whose reads all succeeded.
    pub fn publish(&self, snapshot: IntervalSnapshot) {
        // A poisoned lock can only come from a panic while holding it, and neither side panics
        // under it; recovering the guard is still better than refusing every later read.
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = Some(snapshot);
    }

    /// `node`'s interval, or `None` before the first publish.
    #[must_use]
    pub fn for_node(&self, node: Uuid) -> Option<u32> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|s| s.for_node(node))
    }

    /// The slowest interval in play, or `None` before the first publish.
    #[must_use]
    pub fn fleet_max(&self) -> Option<u32> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|s| s.fleet_max)
    }

    /// How to split a counter query over `scope` (`None` = the fleet) into sets that each read
    /// through one window. One class whenever every node in play shares a window — which includes
    /// every deployment polling at 150 seconds or faster.
    #[must_use]
    pub fn window_classes(&self, floor: u64, scope: Option<&[Uuid]>) -> Vec<WindowClass> {
        window_classes(
            self.0
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref(),
            floor,
            scope,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn the_rate_window_holds_two_polls_and_never_drops_below_its_floor() {
        let f = RATE_WINDOW_FLOOR_SECS;
        assert_eq!(rate_window_secs(f, None), 300, "unknown is today's window");
        assert_eq!(
            rate_window_secs(f, Some(30)),
            300,
            "a 30s deployment is unchanged"
        );
        assert_eq!(
            rate_window_secs(f, Some(150)),
            300,
            "150s is where the floor stops winning"
        );
        assert_eq!(rate_window_secs(f, Some(151)), 302);
        assert_eq!(rate_window_secs(f, Some(300)), 600);
        assert_eq!(rate_window_secs(f, Some(600)), 1200);
        assert_eq!(rate_window_secs(f, Some(3600)), 7200);
        assert_eq!(
            rate_window_secs(60, Some(30)),
            60,
            "a caller's own floor is kept"
        );
        assert_eq!(rate_window_secs(0, None), 1, "a zero window is not a query");
    }

    #[test]
    fn the_tick_dwell_spans_the_polls_and_never_shrinks() {
        let tick = Duration::from_secs(60);
        assert_eq!(dwell_ticks(3, None, tick), 3, "unknown is today's dwell");
        assert_eq!(
            dwell_ticks(3, Some(30), tick),
            3,
            "faster than the tick keeps its minutes"
        );
        assert_eq!(dwell_ticks(3, Some(60), tick), 3);
        assert_eq!(
            dwell_ticks(3, Some(300), tick),
            15,
            "three 5-minute polls are 15 ticks"
        );
        assert_eq!(
            dwell_ticks(5, Some(90), tick),
            8,
            "rounded up, so it never falls short"
        );
        assert_eq!(
            dwell_ticks(0, Some(300), tick),
            0,
            "the engine floors 0 at 1 itself"
        );
        assert_eq!(
            dwell_ticks(u32::MAX, Some(3600), tick),
            u32::MAX,
            "no overflow"
        );
        assert_eq!(
            dwell_ticks(2, Some(10), Duration::ZERO),
            20,
            "a zero tick counts seconds"
        );
    }

    #[test]
    fn the_flap_window_is_twenty_polls_and_never_below_ten_minutes() {
        assert_eq!(flap_window_ms(None), 600_000, "unknown is today's window");
        assert_eq!(
            flap_window_ms(Some(30)),
            600_000,
            "the ratio the fixed window had"
        );
        assert_eq!(flap_window_ms(Some(10)), 600_000);
        assert_eq!(flap_window_ms(Some(300)), 6_000_000);
        assert_eq!(flap_window_ms(Some(3600)), 72_000_000);
    }

    #[test]
    fn a_snapshot_names_only_the_nodes_that_differ_and_its_max_includes_the_default() {
        let snap = IntervalSnapshot::build(300, [(id(1), 300), (id(2), 30), (id(3), 600)]);
        assert_eq!(snap.for_node(id(1)), 300);
        assert_eq!(snap.for_node(id(2)), 30);
        assert_eq!(snap.for_node(id(3)), 600);
        assert_eq!(
            snap.for_node(id(9)),
            300,
            "a node added since the rebuild polls at the default"
        );
        assert_eq!(snap.by_node.len(), 2);
        assert_eq!(snap.fleet_max, 600);
        assert_eq!(IntervalSnapshot::build(300, [(id(1), 30)]).fleet_max, 300);
        assert_eq!(IntervalSnapshot::build(60, []).fleet_max, 60);
    }

    #[test]
    fn a_rebuild_with_a_failed_read_publishes_nothing() {
        assert_eq!(
            IntervalSnapshot::publishable(false, 300, [(id(1), 30)]),
            None
        );
        assert_eq!(
            IntervalSnapshot::publishable(true, 300, [(id(1), 30)]),
            Some(IntervalSnapshot::build(300, [(id(1), 30)]))
        );
    }

    /// The snapshot is a second copy of the scheduler's answer, so it is pinned to the function
    /// that produces the first.
    #[test]
    fn the_snapshot_agrees_with_the_schedulers_resolution_for_every_node() {
        use yagra_common::ProfileId;
        let fast = Uuid::from_u128(100);
        let slow = Uuid::from_u128(101);
        let overrides: HashMap<Uuid, u32> = [(fast, 30), (slow, 900)].into_iter().collect();
        let profiles = [
            None,
            Some(ProfileId(fast)),
            Some(ProfileId(slow)),
            Some(ProfileId(id(7))),
        ];
        let nodes: Vec<(Uuid, Option<ProfileId>)> = (0..40u128)
            .map(|n| (id(n), profiles[(n % 4) as usize]))
            .collect();
        let resolved = nodes.iter().map(|(node, profile)| {
            (
                *node,
                crate::scheduler::resolve_interval(*profile, &overrides, 300),
            )
        });
        let snap = IntervalSnapshot::build(300, resolved);
        for (node, profile) in &nodes {
            assert_eq!(
                snap.for_node(*node),
                crate::scheduler::resolve_interval(*profile, &overrides, 300)
            );
        }
        assert_eq!(snap.fleet_max, 900);
    }

    #[test]
    fn an_unpublished_handle_answers_unknown_and_splits_nothing() {
        let intervals = PollIntervals::unknown();
        assert_eq!(intervals.for_node(id(1)), None);
        assert_eq!(intervals.fleet_max(), None);
        let scope = [id(1), id(2)];
        assert_eq!(
            intervals.window_classes(RATE_WINDOW_FLOOR_SECS, Some(&scope)),
            vec![WindowClass {
                window_secs: 300,
                nodes: Some(scope.to_vec())
            }]
        );
        assert_eq!(
            intervals.window_classes(RATE_WINDOW_FLOOR_SECS, None),
            vec![WindowClass {
                window_secs: 300,
                nodes: None
            }]
        );

        intervals.publish(IntervalSnapshot::build(300, [(id(1), 600)]));
        let clone = intervals.clone();
        assert_eq!(
            clone.for_node(id(1)),
            Some(600),
            "clones share one snapshot"
        );
        assert_eq!(clone.for_node(id(2)), Some(300));
        assert_eq!(clone.fleet_max(), Some(600));
    }

    #[test]
    fn a_fleet_that_shares_a_window_is_one_query() {
        let intervals = PollIntervals::unknown();
        // 30s and 60s both read through the 300s floor, so the override splits nothing.
        intervals.publish(IntervalSnapshot::build(60, [(id(1), 30), (id(2), 60)]));
        assert_eq!(
            intervals.window_classes(RATE_WINDOW_FLOOR_SECS, None),
            vec![WindowClass {
                window_secs: 300,
                nodes: None
            }]
        );
        assert_eq!(
            intervals.window_classes(RATE_WINDOW_FLOOR_SECS, Some(&[id(1), id(2)])),
            vec![WindowClass {
                window_secs: 300,
                nodes: Some(vec![id(1), id(2)])
            }]
        );
    }

    #[test]
    fn a_mixed_fleet_names_the_nodes_off_the_default_window() {
        let intervals = PollIntervals::unknown();
        intervals.publish(IntervalSnapshot::build(
            300,
            [(id(1), 300), (id(3), 30), (id(2), 30), (id(4), 900)],
        ));
        assert_eq!(
            intervals.window_classes(RATE_WINDOW_FLOOR_SECS, None),
            vec![
                WindowClass {
                    window_secs: 600,
                    nodes: None
                },
                WindowClass {
                    window_secs: 300,
                    nodes: Some(vec![id(2), id(3)])
                },
                WindowClass {
                    window_secs: 1800,
                    nodes: Some(vec![id(4)])
                },
            ]
        );
        assert_eq!(
            intervals.window_classes(RATE_WINDOW_FLOOR_SECS, Some(&[id(4), id(1), id(3)])),
            vec![
                WindowClass {
                    window_secs: 300,
                    nodes: Some(vec![id(3)])
                },
                WindowClass {
                    window_secs: 600,
                    nodes: Some(vec![id(1)])
                },
                WindowClass {
                    window_secs: 1800,
                    nodes: Some(vec![id(4)])
                },
            ]
        );
        assert_eq!(
            intervals.window_classes(RATE_WINDOW_FLOOR_SECS, Some(&[])),
            vec![WindowClass {
                window_secs: 600,
                nodes: Some(Vec::new())
            }]
        );
    }

    #[test]
    fn too_many_named_nodes_widen_the_whole_fleet_instead() {
        let intervals = PollIntervals::unknown();
        let nodes = (0..=NAMED_NODES_MAX as u128).map(|n| (id(n), 600));
        intervals.publish(IntervalSnapshot::build(30, nodes));
        assert_eq!(
            intervals.window_classes(RATE_WINDOW_FLOOR_SECS, None),
            vec![WindowClass {
                window_secs: 1200,
                nodes: None
            }]
        );
    }

    #[test]
    fn each_row_is_kept_by_the_one_class_that_owns_its_node() {
        let classes = vec![
            WindowClass {
                window_secs: 600,
                nodes: None,
            },
            WindowClass {
                window_secs: 300,
                nodes: Some(vec![id(2)]),
            },
        ];
        let owners = ClassOwners::of(&classes);
        assert!(owners.owns(0, id(1)), "the remainder owns an unnamed node");
        assert!(
            !owners.owns(0, id(2)),
            "the remainder's read of a named node is dropped"
        );
        assert!(owners.owns(1, id(2)));
        assert!(
            !owners.owns(1, id(1)),
            "a named class's fleet-wide fallback keeps only its own"
        );

        let scoped = ClassOwners::of(&[WindowClass {
            window_secs: 300,
            nodes: Some(vec![id(5)]),
        }]);
        assert!(
            !scoped.owns(0, id(6)),
            "with no remainder, an unnamed node is nobody's"
        );
    }
}
