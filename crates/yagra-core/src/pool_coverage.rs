// SPDX-License-Identifier: AGPL-3.0-only
//! Poller-pool coverage: does every pool that has nodes still have a poller to poll them?
//!
//! This is the one failure the alert engine cannot see by itself. `AlertManager::observe` is fed
//! `PollResult`s, so it reasons about devices that answered or failed to answer — but when a pool
//! loses its last live poller the scheduler falls back to legacy per-job publish on
//! `yagra.jobs.{pool}` with nothing subscribed, plain NATS discards the jobs, and **no
//! `PollResult` is produced at all**. The nodes decay to `unknown` rather than `down`, every
//! dashboard reads calm, and an entire site stops being monitored silently. ADR-009's scale-to-zero
//! discussion names this as 監視自身の盲点.
//!
//! Before this module the condition was visible only to someone already looking: a `warning` string
//! on `GET /api/v1/pollers` and a pill on Settings ▸ Pollers. Here it becomes an alert delivered
//! over the configured notification channels, plus gauges an operator's own Prometheus (or a KEDA
//! `ScaledObject`) can act on.
//!
//! The condition itself is defined **once**, by [`PoolCoverage::is_uncovered`], and the API's
//! `PoolSummary.warning` is derived from the same function — so the pill an operator sees and the
//! page they receive cannot disagree.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use std::sync::Arc;

use crate::alerts::{AlertManager, Notifier};
use crate::coordinator::{Coordinator, PollerView};
use crate::groups::GroupRepo;
use crate::history::AlertHistoryStore;
use crate::meraki::MerakiDeviceRepo;
use crate::poolres::PoolResolver;
use crate::repo::NodeRepo;
// Self-import so the watch loop below keeps the `pool_coverage::` paths it was written with. It
// lived in `main.rs` until ADR-083, where those paths were the only way to name this module; the
// alternative was to strip 9 prefixes and make the move stop being verifiable by diff.
use crate::{alerts, config_gen, groups, meraki, pool_coverage};

/// How often the watch loop samples coverage.
///
/// Matches `coordinator::OFFLINE_AFTER`: sampling faster cannot see anything new, because a
/// poller's liveness only changes on that granularity.
pub const WATCH_TICK: Duration = Duration::from_secs(30);

/// Default time a pool must be uncovered before it notifies.
///
/// `HEARTBEAT_SECS` is 10 and `OFFLINE_AFTER` is 30, so a poller must miss three beats to read
/// dead — this is ten times that. It has to be comfortably longer than a pod restart or an image
/// pull, because a `leaving` heartbeat **removes** the poller from the registry immediately
/// (`coordinator.rs`), which means an ordinary rolling restart raises the condition instantly. The
/// debounce is what stops that paging anyone.
pub const DEFAULT_RAISE_AFTER: Duration = Duration::from_secs(300);

/// Env var overriding [`DEFAULT_RAISE_AFTER`]; `0` disables coverage notifications entirely.
pub const RAISE_AFTER_ENV: &str = "YAGRA_POOL_COVERAGE_ALERT_AFTER_SECS";

/// Most pools that get their own labelled gauge series.
///
/// Pool names are operator-authored free text and the Prometheus recorder is installed without an
/// idle timeout, so a series lives for the life of the process — a typo would be permanent. Past
/// this many pools the labelled gauge is dropped **whole** for that tick rather than truncated: a
/// half-populated labelled gauge is a wrong answer, whereas the unlabelled count is still a right
/// one. See `.claude/rules/monitoring-conventions.md` on label cardinality.
const MAX_LABELLED_POOLS: usize = 64;

/// Gauge: how many pools currently have nodes and no live poller. No labels — this is the series
/// to alert on and to drive a scale-up from.
const M_POOLS_UNCOVERED: &str = "yagra_pools_without_live_poller";
/// Gauge: nodes at risk in each pool, `0` for a healthy one.
const M_POOL_NODES_UNCOVERED: &str = "yagra_pool_nodes_without_live_poller";
/// Counter: coverage notifications emitted, by lifecycle point.
const M_NOTIFICATIONS: &str = "yagra_pool_coverage_notifications_total";

/// The metric name a coverage alert reports, and the check name its id is derived from.
pub const COVERAGE_METRIC: &str = "live_pollers";

/// One pool's polling coverage: how many nodes depend on it, and how many live pollers serve it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolCoverage {
    /// Pool name (`default` for unassigned nodes).
    pub pool: String,
    /// Non-Meraki nodes whose **effective** pool is this one.
    pub nodes: usize,
    /// Live (online) pollers serving it.
    pub live_pollers: usize,
}

impl PoolCoverage {
    /// **The** definition of `nodes_without_live_poller`.
    ///
    /// `api/pollers.rs` derives `PoolSummary.warning` from this, so the pill on Settings ▸ Pollers
    /// and the notification an operator receives are the same judgement, not two spellings of it.
    #[must_use]
    pub const fn is_uncovered(&self) -> bool {
        self.nodes > 0 && self.live_pollers == 0
    }
}

/// Merge the live poller registry with per-pool node counts. Pure: no I/O, no clock.
///
/// Reports a pool that has nodes **or** a live poller, so a poller registered but not yet assigned
/// work still appears (uncovered is false for it — no nodes are at risk).
#[must_use]
pub fn coverage(live: &[PollerView], node_pools: &HashMap<String, usize>) -> Vec<PoolCoverage> {
    // An offline poller cannot serve work, so it does not count towards coverage.
    let mut live_per_pool: HashMap<&str, usize> = HashMap::new();
    for v in live.iter().filter(|v| v.online) {
        *live_per_pool.entry(v.pool.as_str()).or_insert(0) += 1;
    }

    let mut names: Vec<&str> = node_pools
        .keys()
        .map(String::as_str)
        .chain(live_per_pool.keys().copied())
        .collect();
    names.sort_unstable();
    names.dedup();

    names
        .into_iter()
        .map(|name| PoolCoverage {
            pool: name.to_owned(),
            nodes: node_pools.get(name).copied().unwrap_or(0),
            live_pollers: live_per_pool.get(name).copied().unwrap_or(0),
        })
        .collect()
}

/// Non-Meraki node counts keyed by **effective** pool (own > nearest ancestor folder > default).
///
/// Infallible, and degrades the same way `poller_inventory` does (ADR-017): this answers a question
/// an operator asks *while* something is broken.
///
/// The Meraki exclusion is load-bearing, not tidiness. Core's org collector polls Meraki-managed
/// nodes directly, so they depend on no pool poller — counting them would put a Meraki-only
/// deployment whose `default` pool has no registered poller into a permanent false alarm.
pub async fn node_counts_by_pool(
    repo: &NodeRepo,
    meraki: &MerakiDeviceRepo,
    groups: &GroupRepo,
) -> HashMap<String, usize> {
    let meraki_ids = meraki.node_ids().await.unwrap_or_default();
    let resolver = match groups.pool_rows().await {
        Ok(rows) => PoolResolver::build(rows),
        Err(e) => {
            tracing::warn!(error = %e, "loading folder pools failed; resolving without inheritance");
            PoolResolver::empty()
        }
    };
    let mut counts: HashMap<String, usize> = HashMap::new();
    match repo.list_nodes().await {
        Ok(nodes) => {
            for n in nodes {
                if meraki_ids.contains(&n.id.as_uuid()) {
                    continue;
                }
                *counts
                    .entry(resolver.resolve_pool(&n).to_owned())
                    .or_insert(0) += 1;
            }
        }
        Err(e) => tracing::error!(error = %e, "list nodes for pool coverage failed"),
    }
    counts
}

/// A coverage edge worth telling someone about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverageEvent {
    /// The pool has been uncovered for longer than the debounce.
    Raise { pool: String, nodes: usize },
    /// A previously-raised pool is covered again (or is gone).
    Clear { pool: String },
}

/// How long a pool has been uncovered, and whether we have already said so.
#[derive(Debug, Clone, Copy)]
struct PoolWatch {
    since: Instant,
    raised: bool,
}

/// Debounce plus edge detection over successive coverage samples.
///
/// Pure and clock-injected (the same shape as `Coordinator`), so every edge is testable without a
/// bus, a database or real time.
///
/// State is deliberately **not persisted**. A core restart or an HA leader flip re-arms the timer,
/// so a pool that has been dark for an hour is re-notified only after another full debounce — late,
/// never false, which is the right direction for the failure to lean. It also means a still-dark
/// pool gets one duplicate raise per leader flip; PagerDuty and JSM collapse that server-side on
/// the dedup string, and webhook/email see it twice. That is the property
/// `Dispatcher::dispatch_resolve` already documents for the same reason.
#[derive(Debug)]
pub struct CoverageWatch {
    raise_after: Duration,
    watching: HashMap<String, PoolWatch>,
}

impl CoverageWatch {
    /// New watch with the given debounce. `Duration::ZERO` disables notification entirely.
    #[must_use]
    pub fn new(raise_after: Duration) -> Self {
        Self {
            raise_after,
            watching: HashMap::new(),
        }
    }

    /// Read the debounce from the environment, falling back to [`DEFAULT_RAISE_AFTER`].
    #[must_use]
    pub fn from_env() -> Self {
        let secs = std::env::var(RAISE_AFTER_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok());
        Self::new(secs.map_or(DEFAULT_RAISE_AFTER, Duration::from_secs))
    }

    /// Whether notifications are switched off.
    #[must_use]
    pub const fn disabled(&self) -> bool {
        self.raise_after.is_zero()
    }

    /// Feed one coverage sample; returns the edges crossed.
    ///
    /// Iterates the **watch map**, not just the current uncovered set, so a pool that disappears
    /// entirely — renamed, or its last node moved away — still gets its `Clear` instead of leaving
    /// a remote incident open forever.
    pub fn observe(&mut self, coverage: &[PoolCoverage], now: Instant) -> Vec<CoverageEvent> {
        if self.disabled() {
            self.watching.clear();
            return Vec::new();
        }

        let mut events = Vec::new();
        let uncovered: HashMap<&str, usize> = coverage
            .iter()
            .filter(|c| c.is_uncovered())
            .map(|c| (c.pool.as_str(), c.nodes))
            .collect();

        // Clear anything no longer uncovered — but only if we actually raised it. A pool that
        // recovered inside the debounce was never announced, so announcing its recovery would be
        // noise about an event nobody was told about. This is the rolling-restart case, and it is
        // the single most important behaviour in this type.
        self.watching.retain(|pool, watch| {
            if uncovered.contains_key(pool.as_str()) {
                return true;
            }
            if watch.raised {
                events.push(CoverageEvent::Clear { pool: pool.clone() });
            }
            false
        });

        for (pool, nodes) in uncovered {
            let watch = self.watching.entry(pool.to_owned()).or_insert(PoolWatch {
                since: now,
                raised: false,
            });
            if !watch.raised && now.saturating_duration_since(watch.since) >= self.raise_after {
                watch.raised = true;
                events.push(CoverageEvent::Raise {
                    pool: pool.to_owned(),
                    nodes,
                });
            }
        }
        events.sort_by(|a, b| key_of(a).cmp(key_of(b)));
        events
    }
}

/// Stable ordering key so a tick's events are deterministic (several pools can flip together).
fn key_of(event: &CoverageEvent) -> &str {
    match event {
        CoverageEvent::Raise { pool, .. } | CoverageEvent::Clear { pool } => pool.as_str(),
    }
}

/// The gauge values one coverage sample produces, as `(pool, nodes_at_risk)` plus the total.
///
/// Split out from emission so the property that matters is testable: a **recovered pool is present
/// with `0.0`**, it does not simply stop being reported. `yagra_topology_anchor_unresolved` only
/// ever writes its non-zero entries, which — with no `idle_timeout` on the recorder — leaves a
/// stale non-zero series forever. Do not "tidy" the zeros away.
#[must_use]
pub fn gauge_values(coverage: &[PoolCoverage]) -> (usize, Vec<(String, f64)>) {
    let uncovered = coverage.iter().filter(|c| c.is_uncovered()).count();
    if coverage.len() > MAX_LABELLED_POOLS {
        return (uncovered, Vec::new());
    }
    let per_pool = coverage
        .iter()
        .map(|c| {
            let at_risk = if c.is_uncovered() { c.nodes } else { 0 };
            #[expect(
                clippy::cast_precision_loss,
                reason = "a gauge is f64; node counts are far below 2^53"
            )]
            (c.pool.clone(), at_risk as f64)
        })
        .collect();
    (uncovered, per_pool)
}

/// Emit the coverage gauges for one sample.
pub fn publish_gauges(coverage: &[PoolCoverage]) {
    let (uncovered, per_pool) = gauge_values(coverage);
    #[expect(
        clippy::cast_precision_loss,
        reason = "a gauge is f64; pool counts are far below 2^53"
    )]
    metrics::gauge!(M_POOLS_UNCOVERED).set(uncovered as f64);
    if per_pool.is_empty() && coverage.len() > MAX_LABELLED_POOLS {
        tracing::warn!(
            pools = coverage.len(),
            cap = MAX_LABELLED_POOLS,
            "too many poller pools for a per-pool gauge; reporting the total only"
        );
        return;
    }
    for (pool, at_risk) in per_pool {
        metrics::gauge!(M_POOL_NODES_UNCOVERED, "pool" => pool).set(at_risk);
    }
}

/// Count one emitted coverage notification.
pub fn count_notification(event: &'static str) {
    metrics::counter!(M_NOTIFICATIONS, "event" => event).increment(1);
}

/// Wall-clock milliseconds, for stamping a coverage alert.
#[must_use]
pub fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Leader-only loop: alert when a poller pool has nodes but no live poller (ADR-009's blind spot).
///
/// **Leader-only is a correctness requirement, not an optimization.** A standby core runs no
/// coordinator — `run_heartbeat_consumer` is spawned here in `leader_work` — so its registry is
/// permanently empty and it would read *every* pool as dark and page for the whole fleet. This is
/// the same hazard `api/pollers.rs::resolve_polled_by` guards with an `is_leader` check.
///
/// **The node scan is generation-gated** (ADR-026, the idiom `SweepCache` already uses): pool
/// membership is config-derived and `audit_mw` bumps `config_gen` on every config mutation, so an
/// unchanged generation means the counts are identical and a steady-state tick costs one in-memory
/// registry read. Without that this would scan the node table every 30 seconds, which at 50,000
/// nodes is not affordable.
pub(crate) async fn run_pool_coverage_watch(
    coordinator: Arc<Coordinator>,
    repo: Arc<NodeRepo>,
    meraki: Arc<meraki::MerakiDeviceRepo>,
    groups: Arc<groups::GroupRepo>,
    alerts: Arc<AlertManager>,
    notifier: Arc<Notifier>,
    history: Arc<AlertHistoryStore>,
) {
    let mut watch = pool_coverage::CoverageWatch::from_env();
    if watch.disabled() {
        tracing::info!(
            env = pool_coverage::RAISE_AFTER_ENV,
            "poller-pool coverage notifications are disabled; gauges still published"
        );
    }
    let mut cached: Option<(u64, HashMap<String, usize>)> = None;
    loop {
        tokio::time::sleep(pool_coverage::WATCH_TICK).await;

        let generation = config_gen::current();
        if cached.as_ref().is_none_or(|(gen, _)| *gen != generation) {
            let counts = pool_coverage::node_counts_by_pool(&repo, &meraki, &groups).await;
            // An empty map means "no pool has any nodes", which silently disables the whole check.
            // `node_counts_by_pool` already degrades on a read error, so only a genuinely empty
            // inventory produces one legitimately — keep the previous answer when we had one
            // rather than letting a bad read look like a healthy fleet.
            if counts.is_empty() && cached.is_some() {
                tracing::warn!("pool coverage: node counts came back empty; keeping the last set");
            } else {
                cached = Some((generation, counts));
            }
        }
        let Some((_, node_pools)) = cached.as_ref() else {
            continue;
        };

        let coverage =
            pool_coverage::coverage(&coordinator.poller_views(Instant::now()), node_pools);
        pool_coverage::publish_gauges(&coverage);

        for event in watch.observe(&coverage, Instant::now()) {
            let action = match event {
                pool_coverage::CoverageEvent::Raise { pool, nodes } => {
                    tracing::warn!(
                        pool = %pool,
                        nodes,
                        "poller pool has nodes but no live poller — they are not being monitored"
                    );
                    pool_coverage::count_notification("raise");
                    alerts.raise_pool_coverage_alert(&pool, pool_coverage::now_unix_ms())
                }
                pool_coverage::CoverageEvent::Clear { pool } => {
                    tracing::info!(pool = %pool, "poller pool has a live poller again");
                    pool_coverage::count_notification("clear");
                    alerts.resolve_pool_coverage_alert(&pool)
                }
            };
            let Some(action) = action else { continue };
            // Persist before notifying, and write **inline** rather than through the batch
            // channel: that channel exists to keep the poll-result hot path off the database, and
            // this loop is a 30-second tick that emits at most one row per pool per debounce. A
            // failure here is logged and the notification still goes out — an operator being paged
            // matters more than the row, and the row is what History reads afterwards.
            if let Some(alert) = alerts::recordable_alert(&action) {
                let resolved = matches!(action, crate::alerts::NotifyAction::Resolve(_));
                if let Err(e) = history.record(alert, resolved).await {
                    tracing::warn!(error = %e, "recording a pool-coverage transition failed");
                }
            }
            notifier.handle(action).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The structural assertions below read this file's own code through `crate::module_source`,
    // which since ADR-091 removes each test-only item rather than truncating at the first one.
    // They read `pool_coverage.rs` rather than `main.rs` because ADR-083 moved the loop here, and
    // repointing them was not optional bookkeeping: a needle aimed at the old file finds nothing
    // and panics, which is the loud half of the failure — the quiet half is a `.split()` argument
    // that still matches something and checks nothing. Both assertions below were re-run against
    // a deliberately broken body after the move, and both failed, which is the only reason to
    // believe them.

    /// **A coverage transition is written to History, not only notified.**
    ///
    /// ⚠️ Found on real hardware, not by a test: Increment 1 deliberately kept pool alerts out of
    /// `alert_history`, so the watch loop was wired to the notifier alone. Increment 2 opened the
    /// store to every subject — and nothing in the type system connected the two, so the alert
    /// raised, paged, and left no row. The gauges and the log line all looked correct.
    ///
    /// Structural because the loop's body is a 30-second tick around a real database; the
    /// behaviour a unit test *can* reach is `alerts::recordable_alert`, tested in `alerts/mod.rs`.
    #[test]
    fn a_pool_coverage_transition_reaches_the_history_store() {
        let production = crate::module_source::code("src", "pool_coverage");
        let watch = production
            .split("async fn run_pool_coverage_watch")
            .nth(1)
            .expect("the watch loop exists");
        let body = &watch[..watch.find("\nfn ").unwrap_or(watch.len())];
        assert!(
            body.contains("history.record("),
            "the coverage watch notifies without recording — the alert pages and History stays \
             empty, which is exactly what shipped in Increment 1"
        );
        assert!(
            body.contains("notifier.handle("),
            "…and it must still notify"
        );
    }

    fn view(id: &str, pool: &str, online: bool) -> PollerView {
        PollerView {
            id: id.to_owned(),
            pool: pool.to_owned(),
            online,
            seconds_since_seen: 0,
            version: String::new(),
            incarnation: uuid::Uuid::nil(),
            working_set_nodes: 0,
            working_set_specs: 0,
            inflight: 0,
            results_total: 0,
            host: None,
            listeners: Vec::new(),
            caps: Vec::new(),
        }
    }

    fn counts(pairs: &[(&str, usize)]) -> HashMap<String, usize> {
        pairs.iter().map(|(p, n)| ((*p).to_owned(), *n)).collect()
    }

    fn cov(pool: &str, nodes: usize, live: usize) -> PoolCoverage {
        PoolCoverage {
            pool: pool.to_owned(),
            nodes,
            live_pollers: live,
        }
    }

    #[test]
    fn nodes_with_no_live_poller_are_uncovered() {
        let c = coverage(&[], &counts(&[("tokyo", 412)]));
        assert_eq!(c, vec![cov("tokyo", 412, 0)]);
        assert!(c[0].is_uncovered());
    }

    #[test]
    fn an_offline_poller_does_not_cover_a_pool() {
        // The whole point: a registered-but-dead poller must not read as coverage.
        let c = coverage(&[view("p1", "tokyo", false)], &counts(&[("tokyo", 5)]));
        assert_eq!(c, vec![cov("tokyo", 5, 0)]);
        assert!(c[0].is_uncovered());
    }

    #[test]
    fn a_live_poller_covers_its_pool() {
        let c = coverage(&[view("p1", "tokyo", true)], &counts(&[("tokyo", 5)]));
        assert_eq!(c, vec![cov("tokyo", 5, 1)]);
        assert!(!c[0].is_uncovered());
    }

    #[test]
    fn a_pool_with_a_poller_and_no_nodes_is_not_uncovered() {
        // A poller registered but not yet assigned work is reported, without a warning.
        let c = coverage(&[view("p1", "spare", true)], &HashMap::new());
        assert_eq!(c, vec![cov("spare", 0, 1)]);
        assert!(!c[0].is_uncovered());
    }

    /// The rolling-restart case. A `leaving` heartbeat removes the poller from the registry at
    /// once, so the condition is true immediately on any ordinary restart — if this ever emits a
    /// `Raise`, the feature becomes a pager that fires on every deploy.
    #[test]
    fn a_pool_that_recovers_inside_the_debounce_says_nothing_at_all() {
        let t0 = Instant::now();
        let mut w = CoverageWatch::new(Duration::from_secs(300));
        assert!(w.observe(&[cov("tokyo", 5, 0)], t0).is_empty());
        assert!(w
            .observe(&[cov("tokyo", 5, 0)], t0 + Duration::from_secs(120))
            .is_empty());
        assert!(
            w.observe(&[cov("tokyo", 5, 1)], t0 + Duration::from_secs(150))
                .is_empty(),
            "no Clear for something that was never raised"
        );
    }

    #[test]
    fn a_sustained_outage_raises_once_and_clears_once() {
        let t0 = Instant::now();
        let mut w = CoverageWatch::new(Duration::from_secs(300));
        assert!(w.observe(&[cov("tokyo", 5, 0)], t0).is_empty());
        assert_eq!(
            w.observe(&[cov("tokyo", 5, 0)], t0 + Duration::from_secs(300)),
            vec![CoverageEvent::Raise {
                pool: "tokyo".to_owned(),
                nodes: 5
            }]
        );
        // Still dark: no repeat.
        assert!(w
            .observe(&[cov("tokyo", 5, 0)], t0 + Duration::from_secs(900))
            .is_empty());
        assert_eq!(
            w.observe(&[cov("tokyo", 5, 2)], t0 + Duration::from_secs(1000)),
            vec![CoverageEvent::Clear {
                pool: "tokyo".to_owned()
            }]
        );
        // And it can raise again after another full debounce.
        let t1 = t0 + Duration::from_secs(2000);
        assert!(w.observe(&[cov("tokyo", 5, 0)], t1).is_empty());
        assert_eq!(
            w.observe(&[cov("tokyo", 5, 0)], t1 + Duration::from_secs(300)),
            vec![CoverageEvent::Raise {
                pool: "tokyo".to_owned(),
                nodes: 5
            }]
        );
    }

    #[test]
    fn a_raised_pool_that_disappears_is_cleared() {
        // Renamed, or its last node moved away: the pool is gone from the sample entirely. Without
        // iterating the watch map this would leave a PagerDuty/JSM incident open forever.
        let t0 = Instant::now();
        let mut w = CoverageWatch::new(Duration::from_secs(60));
        w.observe(&[cov("tokyo", 5, 0)], t0);
        assert_eq!(
            w.observe(&[cov("tokyo", 5, 0)], t0 + Duration::from_secs(60))
                .len(),
            1
        );
        assert_eq!(
            w.observe(&[], t0 + Duration::from_secs(120)),
            vec![CoverageEvent::Clear {
                pool: "tokyo".to_owned()
            }]
        );
    }

    #[test]
    fn a_pool_that_disappears_before_it_raised_says_nothing() {
        let t0 = Instant::now();
        let mut w = CoverageWatch::new(Duration::from_secs(300));
        w.observe(&[cov("tokyo", 5, 0)], t0);
        assert!(w.observe(&[], t0 + Duration::from_secs(30)).is_empty());
    }

    #[test]
    fn a_zero_debounce_disables_notification_and_forgets_its_state() {
        let t0 = Instant::now();
        let mut w = CoverageWatch::new(Duration::ZERO);
        assert!(w.disabled());
        assert!(w.observe(&[cov("tokyo", 5, 0)], t0).is_empty());
        assert!(w
            .observe(&[cov("tokyo", 5, 0)], t0 + Duration::from_secs(86_400))
            .is_empty());
        assert!(w.watching.is_empty(), "disabling must not strand a watch");
    }

    #[test]
    fn several_pools_flip_in_a_deterministic_order() {
        let t0 = Instant::now();
        let mut w = CoverageWatch::new(Duration::from_secs(60));
        let sample = [cov("beta", 1, 0), cov("alpha", 2, 0)];
        w.observe(&sample, t0);
        let events = w.observe(&sample, t0 + Duration::from_secs(60));
        assert_eq!(
            events,
            vec![
                CoverageEvent::Raise {
                    pool: "alpha".to_owned(),
                    nodes: 2
                },
                CoverageEvent::Raise {
                    pool: "beta".to_owned(),
                    nodes: 1
                },
            ]
        );
    }

    /// The anti-`yagra_topology_anchor_unresolved` property. With no `idle_timeout` on the
    /// recorder, a series that stops being written keeps its last value forever — so a recovered
    /// pool must be written as `0.0`, not omitted.
    #[test]
    fn a_recovered_pool_is_reported_as_zero_not_omitted() {
        let (total, per_pool) = gauge_values(&[cov("tokyo", 5, 2), cov("osaka", 3, 0)]);
        assert_eq!(total, 1);
        assert_eq!(
            per_pool,
            vec![("tokyo".to_owned(), 0.0), ("osaka".to_owned(), 3.0)]
        );
    }

    #[test]
    fn past_the_label_cap_the_total_survives_and_the_labels_are_dropped_whole() {
        let many: Vec<PoolCoverage> = (0..=MAX_LABELLED_POOLS)
            .map(|i| cov(&format!("pool-{i}"), 1, 0))
            .collect();
        let (total, per_pool) = gauge_values(&many);
        assert_eq!(total, MAX_LABELLED_POOLS + 1);
        assert!(
            per_pool.is_empty(),
            "a truncated labelled gauge would be a wrong answer; the total is still a right one"
        );
    }
}
