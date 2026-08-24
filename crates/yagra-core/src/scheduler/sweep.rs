// SPDX-License-Identifier: AGPL-3.0-only
//! When to poll: the periodic loop, its per-pool mode decision, and the cache that lets a
//! steady-state round cost no rescan.
//!
//! Needs the clock and the pool map. Moved here from `main.rs` by ADR-083; the file it landed
//! in is the one this module is named after.

use crate::coordinator::Coordinator;
use crate::repo::NodeRepo;
use crate::{config_gen, groups, meraki, poolres, scheduler};
use futures::stream::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

// The loop names most of the rest through the `scheduler::` self-import it was written with
// (see the `use crate::{…, scheduler}` line above); these are the ones it names bare.
use yagra_bus::JobSpec;
use yagra_common::NodeId;

/// Cached result of a full sweep's spec resolution, reused while config is unchanged (S2). Holds the
/// per-pool desired working sets so a steady-state round costs no `list_nodes` scan, no per-node spec
/// build, and no credential decrypt — the coordinator is fed the cached sets and handles poller
/// membership + its own diff. Populated **only** when the whole fleet was working-set at build time
/// (a legacy pool needs node rows each round, so a mixed fleet keeps rebuilding, as before).
struct SweepCache {
    /// Config generation this was built at; a mismatch forces a rebuild.
    generation: u64,
    /// Sleep period for the round (fleet-minimum interval), cached so the fast path needn't rescan.
    min_interval: u32,
    /// Per-pool desired working set (`build_node_specs` output).
    ///
    /// Handed to [`Coordinator::reconcile_pool`] **by reference**. It used to be cloned per pool per
    /// round, which at fleet scale meant deep-copying every node's decrypted credentials, OID column
    /// lists and route-probe plans on a path whose entire purpose is that nothing changed — the
    /// steady-state round now touches this map without allocating.
    desired_by_pool: HashMap<String, HashMap<NodeId, Vec<JobSpec>>>,
}

impl SweepCache {
    /// Whether this cache can serve the current round: config unchanged since it was built AND every
    /// cached pool still has a live poller (working-set). A pool that fell back to legacy needs node
    /// rows this round, so the cache can't serve it — force a rebuild. (Config-derived pool membership
    /// is stable while the generation is unchanged, so the cached pool set equals the current one.)
    fn reusable(&self, generation: u64, live: &std::collections::HashSet<String>) -> bool {
        self.generation == generation
            && self
                .desired_by_pool
                .keys()
                .all(|p| scheduler::pool_uses_working_set(p, live))
    }
}

/// Group the round's nodes by the pool that should poll them.
///
/// The pool is the node's **effective** one (own > ancestor folder > default, [`poolres`]), so a
/// folder-level assignment routes its whole subtree. Kept a separate pure function so that
/// resolution is unit-testable without a scheduler loop.
///
/// The map is also **seeded with every live pool** before the node loop. Without that, a pool whose
/// last node moved away simply vanishes from the map and is never reconciled again — its poller
/// keeps polling the stale working set for the life of the core process, double-polling nodes that
/// have since moved elsewhere. `reconcile_pool` with an empty desired set publishes one empty
/// snapshot and is idempotent afterwards, so seeding costs nothing in steady state.
///
/// Meraki device nodes are dropped: core's org collector owns them, not a pool poller.
fn group_by_pool(
    resolved: Vec<(yagra_common::Node, u32)>,
    meraki_node_ids: &std::collections::HashSet<Uuid>,
    live: &std::collections::HashSet<String>,
    resolver: &poolres::PoolResolver,
) -> HashMap<String, Vec<(yagra_common::Node, u32)>> {
    let mut groups: HashMap<String, Vec<(yagra_common::Node, u32)>> = HashMap::new();
    for pool in live {
        groups.entry(pool.clone()).or_default();
    }
    for (node, secs) in resolved {
        if meraki_node_ids.contains(&node.id.as_uuid()) {
            continue;
        }
        let pool = resolver.resolve_pool(&node).to_owned();
        groups.entry(pool).or_default().push((node, secs));
    }
    groups
}

/// Periodically turn the inventory into polling work, choosing per pool (ADR-009/020) between:
///
/// - **working-set mode** — a pool with at least one live poller (`coordinator.live_pools`): core
///   hands the coordinator the pool's *entire* desired spec set (built via
///   [`scheduler::PollDispatcher::build_node_specs`], **not** gated by `due()` — the working set
///   always holds every node, and an interval change flows as a spec change), and the coordinator
///   diffs + distributes it as snapshots/deltas. The poller schedules locally.
/// - **legacy mode** — a pool with no live poller: exactly the previous behavior (per-node
///   `due()` + anti-stampede jitter + per-job publish), but routed to the pool's own subject so an
///   old wildcard poller still absorbs it. This is the zero-poller fallback and N/N-1 safety net.
///
/// The mode is decided per pool every sweep, so a pool is served one way or the other but never
/// both (no double-polling). The effective interval per node is `profile override → global default`
/// (both re-read each round, so a UI edit applies next round). The loop wakes at the smallest
/// interval in play; legacy jitter spans that window.
pub(crate) async fn run_scheduler(
    repo: Arc<NodeRepo>,
    groups_repo: Arc<groups::GroupRepo>,
    dispatcher: Arc<scheduler::PollDispatcher>,
    stats: Arc<scheduler::SchedulerStats>,
    meraki_devices: Arc<meraki::MerakiDeviceRepo>,
    coordinator: Arc<Coordinator>,
) {
    use std::collections::HashSet;
    use std::time::Instant;
    let mut last_dispatched: HashMap<Uuid, Instant> = HashMap::new();
    // Legacy-mode cadence for jobs that run slower than their node's interval, keyed by
    // (node, job kind). Only the neighbour walk is in here today; it is keyed by kind rather than
    // special-cased so a second slow job needs no new bookkeeping.
    let mut last_slow: HashMap<(Uuid, &'static str), Instant> = HashMap::new();
    let mut cache: Option<SweepCache> = None;
    // Last successfully-built folder-pool resolver. A transient DB error must NOT degrade to "no
    // inheritance": that would silently move every folder-assigned node to the default pool for one
    // round, churning both pools' working sets. Reusing the last-known map is the safe failure.
    let mut resolver: Option<poolres::PoolResolver> = None;
    loop {
        // Read the config generation before any work so a change racing the rebuild is caught next
        // round (the cache is tagged with the pre-work value).
        let generation = config_gen::current();
        let now = Instant::now();
        let live = coordinator.live_pools(now);

        // Fast path: config unchanged since the cache was built AND every cached pool still has a
        // live poller (working-set). Reuse the cached desired sets — no DB scan, no per-node spec
        // build, no credential decrypt — and let the coordinator handle poller membership + its diff.
        if let Some(c) = &cache {
            if c.reusable(generation, &live) {
                for (pool, desired) in &c.desired_by_pool {
                    coordinator.reconcile_pool(pool, desired, now).await;
                }
                stats.record_sweep(0);
                stats.set_pool_modes(c.desired_by_pool.len() as u64, 0);
                let sleep_secs = c.min_interval;
                metrics::counter!("yagra_sweep_cache_hits_total").increment(1);
                // Wake early if a poller announced it is leaving: the ring changed, so the desired
                // set must be re-pushed now rather than after a full poll interval.
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(u64::from(sleep_secs))) => {}
                    () = coordinator.sweep_nudged() => {}
                }
                continue;
            }
        }
        metrics::counter!("yagra_sweep_cache_misses_total").increment(1);

        // Meraki device nodes are polled by the org collector, not per-node — preload their ids
        // once per round (like the interval overrides) and skip them, so no per-node lookup runs in
        // the hot loop. A load failure degrades to an empty set (they'd fall through to the
        // per-node dispatcher, which then short-circuits them anyway).
        let meraki_node_ids = meraki_devices.node_ids().await.unwrap_or_default();
        // Resolve the round's intervals: the global default (DB-backed) and any per-profile
        // overrides. On a read failure, degrade to the compiled default / no overrides rather than
        // stalling the poll loop.
        let default_secs = repo
            .get_default_poll_interval()
            .await
            .unwrap_or(crate::config::DEFAULT_POLL_INTERVAL_SECS);
        let overrides = repo.profile_interval_overrides().await.unwrap_or_default();
        // Adjacency policy (ADR-038): read once per rebuild, exactly like the intervals above, so
        // no per-node settings query enters the sweep. Degrades to the compiled default.
        //
        // Resolved through the dispatcher rather than straight off `repo`, because it also carries
        // the route-probe plan (ADR-043 Increment 4), which the dispatcher caches on its own TTL —
        // rebuilding that per sweep would mean a JSONB scan of `node_l3` every round.
        let neighbors = dispatcher.adjacency_policy().await;
        // Folder-pool inheritance (ADR-009/020). One small query per rebuild — never on the cached
        // fast path above, which is already generation-keyed.
        match groups_repo.pool_rows().await {
            Ok(rows) => resolver = Some(poolres::PoolResolver::build(rows)),
            Err(e) => {
                let Some(_) = resolver.as_ref() else {
                    tracing::error!(
                        error = %e,
                        "scheduler: loading folder pools failed and none is cached — skipping the round \
                         rather than routing the fleet to the wrong pool"
                    );
                    // Wake early if a poller announced it is leaving: the ring changed, so the desired
                    // set must be re-pushed now rather than after a full poll interval.
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_secs(u64::from(default_secs))) => {}
                        () = coordinator.sweep_nudged() => {}
                    }
                    continue;
                };
                tracing::warn!(error = %e, "scheduler: loading folder pools failed; reusing the last-known map");
            }
        }
        let pool_resolver = resolver
            .clone()
            .unwrap_or_else(poolres::PoolResolver::empty);
        let mut min_interval = default_secs;

        match repo.list_nodes().await {
            Ok(nodes) => {
                // Pair each node with its resolved interval, and find the round's smallest so the
                // jitter window matches the sleep period (a node is never double-scheduled per round).
                let resolved: Vec<_> = nodes
                    .into_iter()
                    .map(|node| {
                        let secs =
                            scheduler::resolve_interval(node.profile, &overrides, default_secs);
                        (node, secs)
                    })
                    .collect();
                for (_, secs) in &resolved {
                    min_interval = min_interval.min(*secs);
                }
                let window_ms = (u64::from(min_interval).saturating_mul(1000)).max(1);
                let node_count = resolved.len();

                // Group the non-Meraki nodes by their effective pool so each pool's mode is decided
                // once — and seed every live pool so one that has lost all its nodes still gets
                // reconciled (see `group_by_pool`).
                let groups = group_by_pool(resolved, &meraki_node_ids, &live, &pool_resolver);

                tracing::debug!(
                    count = node_count,
                    pools = groups.len(),
                    default_secs,
                    min_interval,
                    "scheduling poll round"
                );

                // `present` tracks only legacy-dispatched nodes so the retain below can prune
                // last_dispatched without dropping their cadence; working-set nodes are removed
                // from it explicitly (so a later legacy fallback re-polls them at once).
                let mut present: HashSet<Uuid> = HashSet::new();
                let mut jobs_round: u64 = 0;
                let mut working_set_pools: u64 = 0;
                let mut legacy_pools: u64 = 0;
                // Collect this rebuild's working-set desired sets to seed the cache (see below).
                let mut new_desired_by_pool: HashMap<String, HashMap<NodeId, Vec<JobSpec>>> =
                    HashMap::new();

                // Per-node working-set builds fan out with bounded concurrency: each resolves a
                // node's URL/SNMP/collection config with a few DB round-trips, so at tens of
                // thousands of nodes doing them strictly one-at-a-time would let the build alone
                // exceed the poll interval. Bounded so the DB connection pool isn't overwhelmed.
                const SWEEP_BUILD_CONCURRENCY: usize = 16;

                // URL and DNS monitors each live in their own 1:1 side table, so resolving a node's
                // kind means a query per table. Preload both id sets once per sweep and let the
                // dispatcher skip the query for every node that isn't one — the same reason Meraki
                // ids are preloaded above. Without this the sweep pays one extra round trip per
                // node per table per round at fleet scale.
                let monitor_ids = Arc::new((
                    dispatcher.url_node_ids().await,
                    dispatcher.dns_node_ids().await,
                ));

                for (pool, members) in groups {
                    if scheduler::pool_uses_working_set(&pool, &live) {
                        // Build the pool's whole desired working set and let the coordinator diff +
                        // distribute it (snapshots/deltas). Not gated by `due()`. These nodes leave
                        // the legacy `last_dispatched` map (a later legacy fallback re-polls at once).
                        for (node, _secs) in &members {
                            last_dispatched.remove(&node.id.as_uuid());
                        }
                        // Own each item into the stream and clone the `Arc` per future so no borrow
                        // crosses an `.await` (keeps the concurrent builds free of lifetime coupling).
                        let desired: HashMap<_, _> = futures::stream::iter(members)
                            .map(|(node, secs)| {
                                let dispatcher = dispatcher.clone();
                                let monitor_ids = monitor_ids.clone();
                                // Cheap: scalars plus one `Arc` to the shared route-probe plan.
                                let neighbors = neighbors.clone();
                                async move {
                                    let (url_ids, dns_ids) = monitor_ids.as_ref();
                                    let specs = dispatcher
                                        .build_node_specs(
                                            &node,
                                            secs,
                                            scheduler::MonitorHints {
                                                url: Some(url_ids),
                                                dns: Some(dns_ids),
                                            },
                                            &neighbors,
                                        )
                                        .await;
                                    (node.id, specs)
                                }
                            })
                            .buffer_unordered(SWEEP_BUILD_CONCURRENCY)
                            .filter_map(|(id, specs)| async move {
                                (!specs.is_empty()).then_some((id, specs))
                            })
                            .collect()
                            .await;
                        // Reconcile from the borrow, then hand the one copy to the cache — the set
                        // is built once per rebuild and never duplicated.
                        coordinator.reconcile_pool(&pool, &desired, now).await;
                        new_desired_by_pool.insert(pool.clone(), desired);
                        working_set_pools += 1;
                    } else {
                        // Legacy: per-node due-check + jittered per-job publish to the pool subject.
                        for (node, secs) in &members {
                            let id = node.id.as_uuid();
                            present.insert(id);
                            let elapsed = last_dispatched.get(&id).map(|&t| now.duration_since(t));
                            if !scheduler::due(elapsed, Duration::from_secs(u64::from(*secs))) {
                                continue;
                            }
                            last_dispatched.insert(id, now);
                            for (job, kind) in dispatcher
                                .build_scheduled_jobs_hinted(
                                    node,
                                    *secs,
                                    scheduler::MonitorHints {
                                        url: Some(&monitor_ids.0),
                                        dns: Some(&monitor_ids.1),
                                    },
                                    &neighbors,
                                )
                                .await
                            {
                                // A job whose own cadence is slower than the node's (today: the
                                // neighbour walk) gets its own due-check. Working-set mode needs
                                // nothing here — the poller schedules each spec by its own
                                // `interval_secs` — but this path publishes on the *node's* tick,
                                // so without the gate an hourly walk would go out every minute.
                                // Existing jobs all carry `*secs`, so they never enter this branch.
                                if job.interval_secs > *secs {
                                    let key = (id, kind);
                                    let elapsed =
                                        last_slow.get(&key).map(|&t| now.duration_since(t));
                                    let cadence = Duration::from_secs(u64::from(job.interval_secs));
                                    if !scheduler::due(elapsed, cadence) {
                                        continue;
                                    }
                                    last_slow.insert(key, now);
                                }
                                jobs_round += 1;
                                let dispatcher = dispatcher.clone();
                                let node_id = node.id;
                                let pool = pool.clone();
                                let delay =
                                    Duration::from_millis(rand::random::<u64>() % window_ms);
                                tokio::spawn(async move {
                                    tokio::time::sleep(delay).await;
                                    dispatcher.publish_job(job, kind, node_id, &pool).await;
                                });
                            }
                        }
                        legacy_pools += 1;
                    }
                }
                // Forget legacy nodes no longer present so the map can't grow unbounded (working-set
                // nodes were already removed above).
                last_dispatched.retain(|id, _| present.contains(id));
                last_slow.retain(|(id, _), _| present.contains(id));
                stats.record_sweep(jobs_round);
                stats.set_pool_modes(working_set_pools, legacy_pools);
                // Seed the fast-path cache only when the whole fleet was working-set — a legacy pool
                // needs node rows every round, so a mixed fleet keeps rebuilding (unchanged behavior).
                // Tagged with the generation read before the rebuild so a racing config change is
                // detected next round.
                cache = if legacy_pools == 0 {
                    Some(SweepCache {
                        generation,
                        min_interval,
                        desired_by_pool: new_desired_by_pool,
                    })
                } else {
                    None
                };
            }
            Err(e) => tracing::error!(error = %e, "scheduler: listing nodes failed"),
        }
        // Wake early if a poller announced it is leaving: the ring changed, so the desired
        // set must be re-pushed now rather than after a full poll interval.
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(u64::from(min_interval))) => {}
            () = coordinator.sweep_nudged() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live_set(pools: &[&str]) -> std::collections::HashSet<String> {
        pools.iter().map(|p| (*p).to_owned()).collect()
    }

    fn test_node(pool: Option<&str>, group: Option<Uuid>) -> yagra_common::Node {
        use std::net::{IpAddr, Ipv4Addr};
        let mut n =
            yagra_common::Node::new(NodeId::new(), "n", IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
        n.pool = pool.map(str::to_owned);
        n.group = group.map(yagra_common::GroupId::from);
        n
    }

    #[test]
    fn group_by_pool_uses_the_effective_pool() {
        let folder = Uuid::from_u128(1);
        let resolver = poolres::PoolResolver::build(vec![(folder, None, Some("tokyo".to_owned()))]);
        let nodes = vec![
            (test_node(None, Some(folder)), 30),          // inherits tokyo
            (test_node(Some("osaka"), Some(folder)), 30), // own pool wins
            (test_node(None, None), 30),                  // default
        ];
        let groups = group_by_pool(
            nodes,
            &std::collections::HashSet::new(),
            &std::collections::HashSet::new(),
            &resolver,
        );
        assert_eq!(groups.get("tokyo").map(Vec::len), Some(1));
        assert_eq!(groups.get("osaka").map(Vec::len), Some(1));
        assert_eq!(groups.get(yagra_bus::DEFAULT_POOL).map(Vec::len), Some(1));
    }

    #[test]
    fn group_by_pool_seeds_live_pools_that_have_no_nodes() {
        // Regression: without seeding, a pool whose last node moved away vanishes from the map and
        // is never reconciled again — its poller keeps polling a stale working set forever, so the
        // moved node ends up polled by two pollers. Editing pools from the UI makes this routine.
        let groups = group_by_pool(
            vec![(test_node(Some("osaka"), None), 30)],
            &std::collections::HashSet::new(),
            &live_set(&["tokyo", "osaka"]),
            &poolres::PoolResolver::empty(),
        );
        assert_eq!(
            groups.get("tokyo").map(Vec::len),
            Some(0),
            "an emptied pool must still be reconciled (with an empty desired set)"
        );
        assert_eq!(groups.get("osaka").map(Vec::len), Some(1));
    }

    #[test]
    fn group_by_pool_drops_meraki_nodes() {
        // Core's org collector polls Meraki devices; no pool poller ever should.
        let meraki = test_node(Some("tokyo"), None);
        let meraki_ids: std::collections::HashSet<Uuid> =
            [meraki.id.as_uuid()].into_iter().collect();
        let groups = group_by_pool(
            vec![(meraki, 30), (test_node(Some("tokyo"), None), 30)],
            &meraki_ids,
            &std::collections::HashSet::new(),
            &poolres::PoolResolver::empty(),
        );
        assert_eq!(groups.get("tokyo").map(Vec::len), Some(1));
    }

    fn sweep_cache(generation: u64, pools: &[&str]) -> SweepCache {
        SweepCache {
            generation,
            min_interval: 30,
            desired_by_pool: pools
                .iter()
                .map(|p| ((*p).to_owned(), HashMap::new()))
                .collect(),
        }
    }

    fn live(pools: &[&str]) -> std::collections::HashSet<String> {
        pools.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn sweep_cache_reused_only_when_gen_matches_and_all_pools_working_set() {
        let c = sweep_cache(7, &["default", "site-a"]);
        // Config unchanged and both pools have live pollers → reuse.
        assert!(c.reusable(7, &live(&["default", "site-a"])));
        // A newer generation (config edited) → rebuild.
        assert!(!c.reusable(8, &live(&["default", "site-a"])));
        // A cached pool lost its poller (fell back to legacy) → rebuild.
        assert!(!c.reusable(7, &live(&["default"])));
        // An empty-fleet cache is vacuously reusable while the generation holds.
        assert!(sweep_cache(7, &[]).reusable(7, &live(&[])));
    }
}
