// SPDX-License-Identifier: AGPL-3.0-only
//! Rolling upgrade of remote-site pollers, after core has upgraded itself (ADR-051).
//!
//! ADR-050 replaces everything in core's own compose project, which is core, web, and a co-located
//! poller. A poller at a monitored site is a separate project on a separate host and is therefore
//! untouched — so the operator pressed one button and got a partial upgrade with nothing on screen
//! saying so. This module is the other half: once core is back and reports `succeeded`, it hands the
//! same release to every poller that has declared it can install one, and does so **one at a time
//! per pool** so the pool never loses every live poller at once.
//!
//! Three properties are load-bearing, and each is a decision rather than an implementation detail:
//!
//! * **Core first, always.** N/N-1 covers new-core-with-old-poller; the reverse has a known hole
//!   (ADR-009: an old core does not route jobs to a non-default pool), so the order is a constraint,
//!   not a preference.
//! * **Prefetch, then apply.** Pulling an image over a WAN link is minutes and costs nothing;
//!   recreating the container is seconds and *is* the outage. Splitting them is what keeps a
//!   single-poller site's gap to seconds instead of minutes — the only lever available where no
//!   other poller can take over.
//! * **A failure stops that pool and nothing else.** There is no automatic rollback and no
//!   fleet-wide abort: a stranded poller keeps monitoring on its old build, which is the better
//!   failure, and the other pools have nothing to do with it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;
use yagra_bus::{PollerUpgradeMsg, UpgradeBus, UpgradeStep};

use crate::coordinator::Coordinator;

/// How long to wait for one poller to come back on the target version before giving up on its pool.
///
/// Generous against the central measurement (65s for a whole deployment, ADR-050) because none of
/// that measurement transfers: a site pulls over a WAN link, and the prefetch step that would have
/// absorbed it may itself have failed. Bounded so a site that is simply gone cannot hold its pool's
/// queue open forever.
const RETURN_TIMEOUT: Duration = Duration::from_secs(600);

/// How long to let a prefetch run before starting the apply anyway.
///
/// Not fatal on expiry: the apply pulls too, so a slow or failed prefetch costs a longer outage
/// rather than a failed upgrade. That asymmetry is why this is a wait and not a gate.
const PREFETCH_TIMEOUT: Duration = Duration::from_secs(900);

/// How often to re-read the fleet while waiting.
const POLL_TICK: Duration = Duration::from_secs(5);

/// Let the fleet settle before touching it.
///
/// A core restart bumps the coordinator epoch, which forces **every** poller into a full resync
/// (ADR-020). Starting to remove pollers while that is in flight would stack a reassignment on top
/// of a resynchronisation for no reason. "core is back" and "the fleet is steady" are different
/// moments, and this is the gap between them.
const SETTLE_DELAY: Duration = Duration::from_secs(30);

/// One poller in the convergence queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Sanitized poller id.
    pub id: String,
    /// Pool it serves — the unit of serialization.
    pub pool: String,
    /// Version it reported before the upgrade started.
    pub version: String,
    /// Incarnation it reported before the upgrade started; a change is what proves it restarted.
    pub incarnation: Uuid,
}

/// Group the fleet into per-pool queues, in a stable order.
///
/// Pools are independent failure domains, so they converge in parallel; within a pool the queue is
/// strictly serial. Sorting by id keeps a re-run deterministic — useful when reading two upgrades'
/// audit trails side by side, and the reason this is a `BTreeMap` rather than a `HashMap`.
#[must_use]
pub fn queues(targets: Vec<Target>) -> BTreeMap<String, Vec<Target>> {
    let mut by_pool: BTreeMap<String, Vec<Target>> = BTreeMap::new();
    for t in targets {
        by_pool.entry(t.pool.clone()).or_default().push(t);
    }
    for q in by_pool.values_mut() {
        q.sort_by(|a, b| a.id.cmp(&b.id));
    }
    by_pool
}

/// Would taking one poller out leave this pool with no live poller?
///
/// Not a reason to refuse — a single-poller site is a normal deployment and refusing to upgrade it
/// would mean it is never upgraded at all. It is a reason to *say so*: the pool goes dark for the
/// recreate, `pool_coverage`'s 300s debounce is the entire budget, and no maintenance window
/// silences that alert (deliberately — "the upgrade left a site unmonitored" is the outcome most
/// worth hearing about).
///
/// ⚠️ **`live_in_pool` is every live poller in the pool, not the length of this pool's queue.**
/// Until ADR-051 Inc.4 the caller passed the queue, so a pool of three where one site could replace
/// itself was announced as going dark while two pollers polled throughout. The queue is a subset —
/// pollers with no site updater are never in it — and a subset cannot answer a question about
/// coverage.
#[must_use]
pub fn goes_dark(live_in_pool: usize) -> bool {
    live_in_pool <= 1
}

/// Whether this site's prefetch is worth waiting for, decided before any waiting happens.
///
/// Two inputs, three cases, and the third is the one Inc.4 got wrong by not asking the first
/// question at all — see [`wait_for_prefetch`]. Pure, so the rule can be read and tested without a
/// bus, a coordinator or a clock; the waiting itself is the part that needs all three.
#[must_use]
fn should_wait_for_prefetch(will_report: bool, pool_goes_dark: bool) -> bool {
    // A report ends the wait early, so waiting costs nothing and buys the shorter outage. With no
    // report the wait is blind, and blind is only worth a quarter of an hour when there is no peer
    // left to poll the pool while this site is recreated.
    will_report || pool_goes_dark
}

/// Whether a convergence is running in this process.
///
/// Two things start one — the tail of core's own upgrade, and `POST /api/v1/system/upgrade/pollers`
/// — and they must not overlap: both would publish `apply` to the same pollers, and the second
/// would count the first's restarts as its own returns, then move on to the next poller in a pool
/// that has just lost one.
///
/// A process-wide flag rather than a field on some state, because the thing being serialized *is*
/// process-wide: there is one fleet, one bus connection and (under HA) one leader driving it. A
/// lock threaded through `ApiState` would be the same single value wearing a longer path.
static CONVERGING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Proof that this task, and no other, is converging the fleet.
///
/// [`converge`] takes one by value, so "did you take the lock?" is a question the compiler asks
/// rather than a rule to remember. Released on drop, including on panic.
#[derive(Debug)]
pub struct ConvergeGuard(());

impl Drop for ConvergeGuard {
    fn drop(&mut self) {
        // Stamp the end rather than clearing the snapshot: this is the only place every ending goes
        // through, panic included, and a record that survives the run is what puts "this site did
        // not come back" on a screen for the first time. A target left `applying` under a
        // `finished_at` reads as "it stopped here", which is exactly what happened.
        with_progress(|p| {
            if let Some(c) = p.as_mut() {
                if c.finished_at.is_none() {
                    c.finished_at = Some(crate::api::util::now_unix_s());
                }
            }
        });
        CONVERGING.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Claim the fleet, or `None` if a convergence is already under way.
///
/// The caller turns `None` into a 409; there is no queueing, deliberately. A second press is nearly
/// always the same intent repeated, and running it afterwards would recreate every poller a second
/// time for no version change.
#[must_use]
pub fn try_begin() -> Option<ConvergeGuard> {
    use std::sync::atomic::Ordering;
    CONVERGING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
        .then_some(ConvergeGuard(()))
}

/// Where one poller has got to in a convergence.
///
/// An enum rather than a string because **the WebUI builds a `t()` key from it**: a union nothing
/// iterates lets a new variant reach the operator as a raw key with EN/JA parity still passing
/// (`.claude/rules/extensibility.md` §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConvergeState {
    /// Its pool has not reached it yet.
    Waiting,
    /// Told to fetch the image. It keeps polling throughout — this is not an outage.
    Prefetching,
    /// Being recreated. **This is the outage**, and the only state that costs monitoring.
    Applying,
    /// Back on the target build. The end state that means it worked.
    Returned,
    /// It did not come back within the budget. Its pool's queue stopped here.
    Failed,
    /// Its pool stopped before reaching it, because an earlier poller in that pool failed.
    Skipped,
}

/// One poller of a convergence, and where it has got to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct ConvergingTarget {
    /// Sanitized poller id.
    pub id: String,
    /// The pool whose queue it is in. Pools converge in parallel, so **more than one row can be
    /// `applying` at once** — a screen that says "now upgrading <one poller>" is wrong on any
    /// deployment with two pools.
    pub pool: String,
    /// What it is doing about this run.
    pub state: ConvergeState,
}

/// A convergence, running or finished.
///
/// Held in memory, and that is a deliberate limit rather than an oversight: the case that matters —
/// core upgrades itself, then converges the fleet — starts in the process that reports this, so it
/// is covered. A core restart *during* a convergence loses the record. Persisting it would be a
/// table and a migration for a value whose whole life is minutes.
///
/// ⚠️ **Under HA only the leader has one.** A follower answers `null`, which reads as "nothing is
/// happening". HA is off by default, so this is a difference the lab cannot see.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct Convergence {
    /// The upgrade run this belongs to; the same id every site stamps its own audit line with.
    pub run_id: String,
    /// The release every target is being moved to.
    pub tag: String,
    /// Who asked for it.
    pub requested_by: String,
    /// Unix seconds it began.
    pub started_at: i64,
    /// Unix seconds it ended; `null` while it is still going.
    pub finished_at: Option<i64>,
    /// Every poller in the run, in the order the queues take them.
    pub targets: Vec<ConvergingTarget>,
}

/// The convergence this process is running, or the last one it ran.
///
/// Beside [`CONVERGING`] and for the same reason: the thing being described *is* process-wide —
/// one fleet, one bus connection, one leader driving it.
///
/// **Not cleared when the run ends.** `finished_at` is stamped instead, so the page can say what
/// happened — which is the only place "this site did not come back" has ever reached a screen. The
/// convergence used to leave nothing behind but an audit row and an error log, and a site killed by
/// its own upgrade then read as *aligned*, because it had dropped off the live registry the
/// alignment was computed from.
static PROGRESS: std::sync::RwLock<Option<Convergence>> = std::sync::RwLock::new(None);

/// Read the convergence, for the API. `None` before the first one of this process's life.
#[must_use]
pub fn snapshot() -> Option<Convergence> {
    PROGRESS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Replace the snapshot, or edit it in place.
///
/// Takes the poisoned lock rather than panicking on it: a panic somewhere in a convergence must not
/// make every later read of this page fail too.
fn with_progress<T>(f: impl FnOnce(&mut Option<Convergence>) -> T) -> T {
    let mut guard = PROGRESS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&mut guard)
}

/// Move one target to a new state, if this run still owns the snapshot.
///
/// A no-op when the ids do not match, which is what keeps a task that outlived its own run from
/// writing over a newer one.
fn mark(run_id: &str, poller_id: &str, state: ConvergeState) {
    with_progress(|p| {
        let Some(c) = p.as_mut() else { return };
        if c.run_id != run_id {
            return;
        }
        if let Some(t) = c.targets.iter_mut().find(|t| t.id == poller_id) {
            t.state = state;
        }
    });
}

/// Move every target still in `from` to `to`, for this run.
fn mark_all(run_id: &str, pool: &str, from: ConvergeState, to: ConvergeState) {
    with_progress(|p| {
        let Some(c) = p.as_mut() else { return };
        if c.run_id != run_id {
            return;
        }
        for t in c
            .targets
            .iter_mut()
            .filter(|t| t.pool == pool && t.state == from)
        {
            t.state = to;
        }
    });
}

/// Has this poller come back on the target build?
///
/// Two pieces of evidence, for ADR-050 decision 11's reason: green is not proof. The version alone
/// could be a stale registry entry from before the restart, and a changed incarnation alone only
/// says something restarted. Together they say *this poller restarted and is now the build we
/// asked for*.
#[must_use]
pub fn returned(before: &Target, now_version: &str, now_incarnation: Uuid, want: &str) -> bool {
    now_incarnation != before.incarnation && version_matches(now_version, want)
}

/// `v0.2.3` (a release tag) against `0.2.3` (a crate version). The two spellings meet here and
/// nowhere else, so normalize in one place rather than at each comparison.
fn version_matches(reported: &str, tag: &str) -> bool {
    reported.trim_start_matches('v') == tag.trim_start_matches('v')
}

/// Everything a convergence needs beyond the queue itself.
///
/// A struct rather than eight parameters because clippy is right about what eight parameters mean:
/// these travel together, are cloned together into each pool's task, and none of them is meaningful
/// without the rest. Naming the group also makes the one thing worth noticing explicit — a run is
/// identified centrally and at every site by the same `run_id`, so two audit trails describe one
/// operation.
#[derive(Clone)]
pub struct Run {
    /// Where commands go.
    pub bus: Arc<dyn UpgradeBus>,
    /// Where the answer comes back from: the live poller registry.
    pub coordinator: Arc<Coordinator>,
    /// Where each poller's outcome is recorded, one row apiece.
    pub audit: Arc<crate::audit::AuditRepo>,
    /// Release every poller is being moved to.
    pub tag: String,
    /// Core's upgrade run id, reused verbatim at each site.
    pub run_id: String,
    /// Who pressed the button.
    pub requested_by: String,
}

/// Drive every pool's queue to the target release. Returns once every pool has finished or given up.
///
/// Started either by the task that settles a finished run — so, only after core's own upgrade is
/// known to have succeeded — or by `POST /api/v1/system/upgrade/pollers`, which aligns a fleet that
/// has drifted since (ADR-051 Inc.4 decision 17). The [`ConvergeGuard`] is what makes those two
/// mutually exclusive, and it is taken by value so that cannot be forgotten.
pub async fn converge(run: Run, targets: Vec<Target>, _lock: ConvergeGuard) {
    if targets.is_empty() {
        return;
    }
    let by_pool = queues(targets);
    // Published before the first command goes out, so a page that polls in the next second already
    // knows a convergence exists. `waiting` is the honest starting state: nothing has been asked of
    // any site yet.
    with_progress(|p| {
        *p = Some(Convergence {
            run_id: run.run_id.clone(),
            tag: run.tag.clone(),
            requested_by: run.requested_by.clone(),
            started_at: crate::api::util::now_unix_s(),
            finished_at: None,
            targets: by_pool
                .values()
                .flatten()
                .map(|t| ConvergingTarget {
                    id: t.id.clone(),
                    pool: t.pool.clone(),
                    state: ConvergeState::Waiting,
                })
                .collect(),
        });
    });
    tracing::info!(
        pools = by_pool.len(),
        pollers = by_pool.values().map(Vec::len).sum::<usize>(),
        tag = %run.tag,
        "converging remote pollers onto the release core just installed"
    );

    // Prefetch everywhere first, in parallel and across every pool: nobody stops polling to
    // download, so there is nothing to serialize, and every site that finishes early shortens its
    // own outage. Only the *apply* step is one-at-a-time.
    for t in by_pool.values().flatten() {
        send(&run, &t.id, UpgradeStep::Prefetch).await;
        mark(&run.run_id, &t.id, ConvergeState::Prefetching);
    }

    tokio::time::sleep(SETTLE_DELAY).await;

    let mut tasks = Vec::new();
    for (pool, queue) in by_pool {
        let run = run.clone();
        tasks.push(tokio::spawn(async move {
            converge_pool(&run, &pool, queue).await
        }));
    }
    for t in tasks {
        let _ = t.await;
    }
}

/// One pool's queue, strictly serially. Stops at the first poller that does not come back.
async fn converge_pool(run: &Run, pool: &str, queue: Vec<Target>) {
    let size = queue.len();
    // Ask the registry how many pollers this pool actually has, rather than how many are in the
    // queue. A pool can hold pollers with no site updater; they are not upgraded and they are also
    // not going anywhere, so they are exactly the ones that decide whether this costs coverage.
    let live_in_pool = run
        .coordinator
        .poller_views(std::time::Instant::now())
        .into_iter()
        .filter(|v| v.online && v.pool == pool)
        .count();
    let pool_goes_dark = goes_dark(live_in_pool);
    if pool_goes_dark {
        tracing::warn!(
            %pool,
            "this pool has one live poller, so upgrading it stops monitoring at that site until it \
             returns; the prefetch above is what keeps that to the recreate"
        );
    }
    // One budget for the whole pool, started when the pool's turn does. Every poller waits for its
    // *own* prefetch — the earlier ones return instantly, having finished during their predecessors'
    // applies — but a pool whose sites cannot pull at all spends this once rather than once each.
    let prefetch_deadline = tokio::time::Instant::now() + PREFETCH_TIMEOUT - SETTLE_DELAY;
    for target in queue {
        wait_for_prefetch(run, &target, prefetch_deadline, pool_goes_dark).await;
        mark(&run.run_id, &target.id, ConvergeState::Applying);
        send(run, &target.id, UpgradeStep::Apply).await;
        let ok = wait_for_return(&run.coordinator, &target, &run.tag).await;
        mark(
            &run.run_id,
            &target.id,
            if ok {
                ConvergeState::Returned
            } else {
                ConvergeState::Failed
            },
        );
        let action = format!(
            "upgrade poller {} -> {} ({})",
            target.id,
            run.tag,
            if ok { "returned" } else { "did not return" }
        );
        if let Err(e) = run
            .audit
            .record(&run.requested_by, &action, if ok { 200 } else { 500 })
            .await
        {
            tracing::warn!(error = %e, "could not record the poller upgrade in the audit log");
        }
        if !ok {
            // Stop this pool, leave the others alone. Continuing would take a second poller out of a
            // pool that has just proved it cannot get one back — the exact way to turn one stranded
            // site into an unmonitored one.
            tracing::error!(
                %pool,
                poller = %target.id,
                "poller did not come back on the target version; stopping this pool's upgrade"
            );
            // The rest of this pool is not waiting any more — nothing will reach it. Leaving them
            // `prefetching` would read as work still in flight for as long as the record lives.
            mark_all(
                &run.run_id,
                pool,
                ConvergeState::Prefetching,
                ConvergeState::Skipped,
            );
            mark_all(
                &run.run_id,
                pool,
                ConvergeState::Waiting,
                ConvergeState::Skipped,
            );
            return;
        }
    }
    tracing::info!(%pool, pollers = size, "pool converged");
}

/// Publish one command, logging a failure rather than propagating it.
///
/// Best effort on purpose: the bus is fire-and-forget here, and the thing that actually decides
/// whether this worked is whether the poller comes back on the target version. A publish error is
/// worth a log line and nothing more, because the wait below draws the same conclusion anyway.
async fn send(run: &Run, poller_id: &str, step: UpgradeStep) {
    let msg = PollerUpgradeMsg {
        poller_id: poller_id.to_owned(),
        run_id: run.run_id.clone(),
        tag: run.tag.clone(),
        requested_by: run.requested_by.clone(),
        requested_at: crate::api::util::now_unix_s(),
        step,
    };
    if let Err(e) = run.bus.publish_poller_upgrade(msg).await {
        tracing::warn!(error = %e, poller = %poller_id, ?step, "could not publish an upgrade command");
    }
}

/// Wait until this site has the image, or until the pool's prefetch budget runs out.
///
/// 🚨 **This used to be `sleep(870s)`, unconditionally, before the first apply of every pool.** The
/// budget was the only thing available: the updater writes `status.json` into a volume at the site,
/// and core could not see it. So an upgrade whose pull finished in twenty seconds still took a
/// quarter of an hour to start, and an operator watching the page had no signal in between.
/// ADR-051 Inc.4 decision 18 carries that file on the heartbeat, so the wait now ends when the site
/// says it is ready.
///
/// **A failed prefetch stops the wait too, and does not stop the upgrade.** The apply pulls as well,
/// so a site that could not fetch ahead of time pays a longer outage rather than a failed run — the
/// asymmetry `PREFETCH_TIMEOUT` was written for. Waiting out the budget after a reported failure
/// would spend fifteen minutes to reach the same place.
///
/// **A site that cannot report at all is a third case, and Inc.4 got it wrong.** Decision 18 shipped
/// the report without a capability saying who sends one, so a build that reports nothing and a build
/// that has not reported *yet* were the same silence — and core spent the full budget on both. That
/// is not hypothetical: on 192.168.1.212 the prefetch succeeded in **six seconds** and the apply
/// followed **870 seconds** later, which is every remote site's first self-upgrade by construction,
/// since the poller being replaced is always the older build. `CAP_UPGRADE_REPORT` (Inc.5) names the
/// difference, and the rule it enables is:
///
/// * **The site will report** ⇒ wait for it, with the budget as the backstop. Unchanged.
/// * **It will not, and the pool keeps a live poller** ⇒ do not wait. The apply pulls, so the cost
///   is a longer outage at a site whose pool is still being polled by someone else — and a blind
///   quarter of an hour buys nothing there.
/// * **It will not, and the pool goes dark** ⇒ spend the budget. With no report and no peer, the
///   blind wait is the only remaining way to pull *before* the outage rather than during it, which
///   is the whole of decision 13.
///
/// ⚠️ The capability is read **once, on entry**. A build does not learn to report halfway through,
/// and re-reading each tick would let a site that merely went quiet flip the rule under itself.
async fn wait_for_prefetch(
    run: &Run,
    target: &Target,
    deadline: tokio::time::Instant,
    pool_goes_dark: bool,
) {
    use yagra_bus::{UpgradeReportCommand, UpgradeReportState};
    // Asked before waiting rather than while waiting — see the ⚠️ above. An offline poller answers
    // `false` here, which is the right answer: nothing is coming from a site that is not on the bus.
    let will_report = run
        .coordinator
        .poller_views(std::time::Instant::now())
        .into_iter()
        .find(|v| v.id == target.id)
        .is_some_and(|v| v.caps.iter().any(|c| c == yagra_bus::CAP_UPGRADE_REPORT));
    if !should_wait_for_prefetch(will_report, pool_goes_dark) {
        tracing::info!(
            poller = %target.id,
            "this site's build does not report its prefetch and its pool has another live poller; \
             applying now rather than waiting the budget out blind — the apply pulls, and the \
             longer outage is covered"
        );
        return;
    }
    loop {
        let report = run
            .coordinator
            .poller_views(std::time::Instant::now())
            .into_iter()
            .find(|v| v.id == target.id)
            .and_then(|v| v.upgrade);
        // Keyed on the run id, so a report left over from a previous upgrade cannot end this wait.
        // That is what makes it safe for the poller to send its last status on every beat.
        if let Some(r) =
            report.filter(|r| r.run_id == run.run_id && r.command == UpgradeReportCommand::Prefetch)
        {
            match r.state {
                UpgradeReportState::Succeeded => {
                    tracing::info!(poller = %target.id, "site has the image; starting its apply");
                    return;
                }
                UpgradeReportState::Failed => {
                    tracing::warn!(
                        poller = %target.id,
                        message = %r.message,
                        "site could not prefetch; applying anyway, which pulls — expect a longer outage"
                    );
                    return;
                }
                UpgradeReportState::Running | UpgradeReportState::Unknown => {}
            }
        }
        if tokio::time::Instant::now() >= deadline {
            tracing::debug!(
                poller = %target.id,
                "prefetch budget spent without a report; applying anyway"
            );
            return;
        }
        tokio::time::sleep(POLL_TICK).await;
    }
}

/// Wait for `target` to reappear on `tag`. `false` on timeout.
async fn wait_for_return(coordinator: &Arc<Coordinator>, target: &Target, tag: &str) -> bool {
    let deadline = tokio::time::Instant::now() + RETURN_TIMEOUT;
    loop {
        let now = std::time::Instant::now();
        if let Some(v) = coordinator
            .poller_views(now)
            .into_iter()
            .find(|v| v.id == target.id)
        {
            if v.online && returned(target, &v.version, v.incarnation, tag) {
                tracing::info!(poller = %target.id, version = %v.version, "poller returned on the target version");
                return true;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL_TICK).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A finished convergence is **kept**, with an end stamped on it.
    ///
    /// 🚨 The record surviving the run is what puts "this site did not come back" on a screen for
    /// the first time. A site killed by its own upgrade drops off the live poller registry, so it
    /// vanishes from every list built from that registry and the deployment reports itself aligned;
    /// the audit row was the only trace. Clearing the snapshot on drop would restore exactly that.
    ///
    /// The stamp goes in `Drop` because it is the one place every ending goes through — a pool that
    /// gave up, a task that was killed, a panic. A target left `applying` under a `finished_at`
    /// reads as "it stopped here", which is what happened.
    #[test]
    fn a_finished_convergence_is_stamped_rather_than_cleared() {
        let lock = try_begin().expect("nothing else is converging in this test binary");
        with_progress(|p| {
            *p = Some(Convergence {
                run_id: "run-1".to_owned(),
                tag: "v9.9.9".to_owned(),
                requested_by: "horry".to_owned(),
                started_at: 1,
                finished_at: None,
                targets: vec![ConvergingTarget {
                    id: "edge-1".to_owned(),
                    pool: "default".to_owned(),
                    state: ConvergeState::Applying,
                }],
            });
        });
        assert!(snapshot().expect("published").finished_at.is_none());

        drop(lock);

        let after = snapshot().expect("a finished convergence is kept, not cleared");
        assert!(after.finished_at.is_some(), "the ending is stamped");
        assert_eq!(
            after.targets[0].state,
            ConvergeState::Applying,
            "a target that never finished stays where it stopped, rather than being tidied to a \
             state nothing observed"
        );

        // `mark` is addressed by run id, so a task that outlived its own run cannot write over a
        // newer one — the same discipline the page's `pending` applies from the other side.
        mark("run-2", "edge-1", ConvergeState::Returned);
        assert_eq!(
            snapshot().expect("still there").targets[0].state,
            ConvergeState::Applying
        );
        mark("run-1", "edge-1", ConvergeState::Returned);
        assert_eq!(
            snapshot().expect("still there").targets[0].state,
            ConvergeState::Returned
        );

        with_progress(|p| *p = None);
    }

    fn target(id: &str, pool: &str) -> Target {
        Target {
            id: id.to_owned(),
            pool: pool.to_owned(),
            version: "0.2.2".to_owned(),
            incarnation: Uuid::from_u128(1),
        }
    }

    #[test]
    fn pools_are_separate_queues_and_each_is_ordered() {
        // Pools are independent failure domains: serializing across them would make a ten-site
        // fleet's upgrade ten times longer for no safety at all, while serializing *within* one is
        // the entire mechanism that keeps a multi-poller pool covered throughout.
        let q = queues(vec![
            target("edge-tokyo-2", "tokyo"),
            target("edge-osaka-1", "osaka"),
            target("edge-tokyo-1", "tokyo"),
        ]);
        assert_eq!(q.len(), 2);
        assert_eq!(
            q["tokyo"].iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            vec!["edge-tokyo-1", "edge-tokyo-2"],
            "stable order, so two runs' audit trails line up"
        );
        assert_eq!(q["osaka"].len(), 1);
    }

    #[test]
    fn a_single_poller_pool_is_flagged_as_going_dark() {
        // Not refused — a one-poller site is a normal deployment, and refusing would mean it is
        // never upgraded. Flagged, because its whole budget is the 300s coverage debounce.
        assert!(goes_dark(1));
        assert!(goes_dark(0));
        assert!(!goes_dark(2));
    }

    #[test]
    fn a_site_that_cannot_report_is_only_waited_for_when_its_pool_would_go_dark() {
        // Both accepting cases first, deliberately: a rule tested only by what it refuses is
        // satisfied by an implementation that refuses everything, and this one decides whether a
        // remote site pulls before its outage or during it.
        assert!(
            should_wait_for_prefetch(true, false),
            "a site that reports ends the wait itself, so waiting costs nothing and buys the \
             shorter outage even where a peer would have covered it"
        );
        assert!(
            should_wait_for_prefetch(true, true),
            "reporting and alone: the case the whole prefetch/apply split exists for"
        );
        assert!(
            should_wait_for_prefetch(false, true),
            "no report and no peer — the blind budget is the only remaining way to pull before the \
             outage rather than during it (decision 13)"
        );
        assert!(
            !should_wait_for_prefetch(false, false),
            "no report and a peer still polling the pool: 870 blind seconds buy nothing. This is \
             the case measured on 192.168.1.212, where the site had the image after six"
        );
    }

    #[test]
    fn a_return_needs_both_a_new_incarnation_and_the_target_version() {
        let before = target("edge-1", "tokyo");
        let fresh = Uuid::from_u128(2);

        assert!(returned(&before, "0.2.3", fresh, "v0.2.3"));
        assert!(
            !returned(&before, "0.2.3", before.incarnation, "v0.2.3"),
            "same incarnation: the registry is reporting a poller that never restarted"
        );
        assert!(
            !returned(&before, "0.2.2", fresh, "v0.2.3"),
            "restarted, but came back on the version it already had — the upgrade did not take"
        );
    }

    #[test]
    fn a_release_tag_and_a_crate_version_are_the_same_version() {
        // The `v` is a tag convention; the poller reports `CARGO_PKG_VERSION`, which has none.
        // Comparing them raw would make every upgrade look like it failed.
        assert!(version_matches("0.2.3", "v0.2.3"));
        assert!(version_matches("v0.2.3", "0.2.3"));
        assert!(!version_matches("0.2.3", "v0.2.4"));
    }
}
