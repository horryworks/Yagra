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

/// Would taking this poller out leave its pool with no live poller?
///
/// Not a reason to refuse — a single-poller site is a normal deployment and refusing to upgrade it
/// would mean it is never upgraded at all. It is a reason to *say so*: the pool goes dark for the
/// recreate, `pool_coverage`'s 300s debounce is the entire budget, and no maintenance window
/// silences that alert (deliberately — "the upgrade left a site unmonitored" is the outcome most
/// worth hearing about).
#[must_use]
pub fn goes_dark(pool_size: usize) -> bool {
    pool_size <= 1
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
/// Spawned by the same task that settles a finished run, so it starts only after core's own upgrade
/// is known to have succeeded.
pub async fn converge(run: Run, targets: Vec<Target>) {
    if targets.is_empty() {
        return;
    }
    let by_pool = queues(targets);
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
    if goes_dark(size) {
        tracing::warn!(
            %pool,
            "this pool has one poller, so upgrading it stops monitoring at that site until it \
             returns; the prefetch above is what keeps that to the recreate"
        );
    }
    for (i, target) in queue.into_iter().enumerate() {
        // The prefetch has had `SETTLE_DELAY` plus every earlier poller's turn to land; wait out the
        // rest of its budget only for the first one, where nothing else has passed.
        if i == 0 {
            wait_for_prefetch(&target).await;
        }
        send(run, &target.id, UpgradeStep::Apply).await;
        let ok = wait_for_return(&run.coordinator, &target, &run.tag).await;
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

/// Give the first poller of a pool the rest of the prefetch budget before its outage starts.
async fn wait_for_prefetch(target: &Target) {
    let remaining = PREFETCH_TIMEOUT.saturating_sub(SETTLE_DELAY);
    tracing::debug!(poller = %target.id, "waiting out the prefetch budget before the first apply");
    tokio::time::sleep(remaining.min(PREFETCH_TIMEOUT)).await;
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
