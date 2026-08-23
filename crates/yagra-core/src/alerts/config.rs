// SPDX-License-Identifier: AGPL-3.0-only
//! Assembling the alert engine's config from the database, and keeping it fresh.
//!
//! [`super::rules::AlertConfig`] is the thing the engine resolves every check against: the
//! thresholds, the per-node metadata scope resolution needs, the dependency topology suppression
//! walks, and the nodes currently inside a maintenance window. This module is where those four come
//! out of PostgreSQL and become one snapshot — the *write* side of the type the sibling modules
//! read.
//!
//! It lived in `main.rs` until ADR-090, for no reason beyond history: it is not part of booting.
//!
//! ## The shape worth knowing before editing
//!
//! **The expensive half is gated on the config generation** (S6). [`AlertConfigBase`] is a full
//! fleet scan — every node, a 50k-entry metadata map, a topology build — so it is rebuilt only when
//! [`crate::config_gen`] says configuration actually changed. Maintenance windows are
//! time-dependent and cannot be cached that way, so they are re-resolved every cycle over the node
//! list the base already holds.
//!
//! 🚨 **A failed read must never become an empty value here** (ADR-080). A ruleset that comes back
//! empty is indistinguishable from "every rule was deleted", and
//! [`super::engine::AlertManager::observe`] closes every alert on one of those — so one failed
//! threshold query would resolve the whole fleet's alerts and page a recovery for each. Every load
//! in [`load_alert_config_base`] therefore propagates with `?` rather than carrying its own
//! `unwrap_or`, and `guards.rs`-style structural tests at the bottom of this file pin both that
//! and ADR-043 決定 5's single-topology rule.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;
use yagra_common::NodeId;

use super::{ActiveMute, AlertConfig, AlertManager, NodeMeta, Notifier};
use crate::maintenance::MaintenanceRepo;
use crate::notifications::NotificationRepo;
use crate::repo::NodeRepo;
use crate::thresholds::ThresholdStore;
use crate::{
    classification, config_gen, events, groups, maintenance, poolres, thresholds,
    topology_projection,
};
use yagra_topology::Topology;

/// The config-derived half of the alert config: all thresholds + a full node scan folded into the
/// node-meta map and dependency topology. This is the expensive full-fleet work (a `list_nodes`
/// scan + a 50k-entry map/topology build), gated behind the config generation so it runs only after
/// an actual config change rather than every 30s refresh (S6). The raw node list is retained so the
/// time-dependent maintenance resolution can run each cycle without re-scanning the DB.
pub(crate) struct AlertConfigBase {
    rules: Vec<thresholds::StoredThreshold>,
    nodes: Vec<yagra_common::Node>,
    meta: HashMap<NodeId, NodeMeta>,
    /// Folder groups holding at least one node in each **effective** poll pool — what makes a
    /// pool-coverage alert (ADR-009) visible to the group-scoped operator whose site went dark.
    /// Built here because this is the one place that already scans the whole fleet, and rebuilt on
    /// the same config-generation gate so it costs a steady-state refresh nothing.
    pool_groups: HashMap<String, std::collections::BTreeSet<Uuid>>,
    /// The graph the alert engine suppresses with.
    ///
    /// **This is the only topology in the struct, deliberately.** ADR-043 決定 5's shadow mode does
    /// not put a second graph here for the engine to maybe-use: in `shadow` this field holds the
    /// *manual* graph, exactly as in `manual`, and the derived alternative is computed on demand by
    /// the read-side endpoint that displays the difference. There is therefore no runtime state in
    /// which a shadow graph can suppress anything — not because a flag says so, but because the
    /// engine is never given one.
    topology: Topology,
    /// Metric names that publish one series per interface, so each port gets its own check
    /// (ADR-076). Rebuilt on the same config-generation gate as everything else here.
    per_interface: std::collections::BTreeSet<String>,
}

/// Load the config-derived alert base (thresholds + node-meta + dependency topology).
///
/// 🚨 **Every read here propagates its error; none of them degrades to an empty value** (ADR-080).
/// Each one used to have its own `unwrap_or`, each justified as "the narrowing failure, so it is
/// safe" — and every one of those justifications was wrong in the same way. Narrowing does not mean
/// "no alert fires that would not have fired anyway" once alerts are *open*: a check whose rule
/// stops resolving is a check with no rule, and `AlertManager::observe`'s `!alerting` branch closes
/// every alert on it. So one failed `list_all()` resolved the whole fleet's threshold alerts, sent a
/// recovery for each, and re-fired them all thirty seconds later.
///
/// One rule, no list of which reads are "critical": **if anything failed, the caller keeps the
/// config it already has.** A list of exceptions is a thing that rots; a single `?` is not.
///
/// The one deliberate exception is [`NodeRepo::get_topology_mode`], which keeps its own fallback —
/// and for the opposite reason: it degrades to the mode that *changes nothing* (`Manual`), so it
/// cannot silence or redirect anything. Read its doc comment before copying the pattern.
pub(crate) async fn load_alert_config_base(
    repo: &NodeRepo,
    thresholds: &ThresholdStore,
    groups: &groups::GroupRepo,
    topo: &topology_projection::TopologySources,
) -> anyhow::Result<AlertConfigBase> {
    let rules = thresholds
        .list_all()
        .await
        .map_err(|e| anyhow::anyhow!("load thresholds: {e}"))?;
    let nodes = repo
        .list_nodes()
        .await
        .map_err(|e| anyhow::anyhow!("load nodes: {e}"))?;
    // Folder-pool inheritance (0054).
    let pools = poolres::PoolResolver::build(
        groups
            .pool_rows()
            .await
            .map_err(|e| anyhow::anyhow!("load folder pools: {e}"))?,
    );
    // Folder-group threshold scope (ADR-075 増分 3): a rule on a group covers every group inside
    // it, so each node needs its group plus every group above it. Read the edges once — the walk
    // is per-node and `group_ancestors` is a linear scan of this slice.
    let group_edges = groups
        .edges()
        .await
        .map_err(|e| anyhow::anyhow!("load group edges: {e}"))?;
    // Which metrics are per-interface (ADR-076). Built from the collection catalogue, never from
    // whether a sample carries an `ifindex` label — that label is a row key, so a chassis reading
    // would otherwise be split into one bogus check per "port" (ADR-011).
    //
    // The repo is constructed here rather than threaded through `LeaderTasks`: it is a handle on
    // the pool `repo` already owns, and this runs only when the config generation advances.
    let per_interface = crate::collection::CollectionRepo::new(repo.pool())
        .per_interface_metric_names()
        .await
        .map_err(|e| anyhow::anyhow!("load per-interface metric names: {e}"))?;
    let mut meta = HashMap::new();
    let mut pool_groups: HashMap<String, std::collections::BTreeSet<Uuid>> = HashMap::new();
    for node in &nodes {
        // Ungrouped nodes contribute nothing: a scoped caller cannot see them either way, and
        // adding a `None` bucket would be the fail-open reading.
        if let Some(group) = node.group {
            pool_groups
                .entry(pools.resolve_pool(node).to_owned())
                .or_default()
                .insert(group.as_uuid());
        }
        meta.insert(
            node.id,
            NodeMeta {
                profile: node.profile.as_ref().map(ToString::to_string),
                // Tag values (threshold scope) and the folder group (RBAC visibility) are two
                // different things — see the `NodeMeta` docs before touching either.
                tag_groups: node.tags.values().cloned().collect(),
                folder_group: node.group.map(|g| g.as_uuid()),
                folder_chain: node.group.map_or_else(Vec::new, |g| {
                    let own = g.as_uuid();
                    std::iter::once(own)
                        .chain(groups::group_ancestors(&group_edges, own))
                        .collect()
                }),
            },
        );
    }

    // ADR-043 決定 5. The engine gets the derived graph only in `derived`; `shadow` is byte-for-byte
    // `manual` here, and the comparison an operator reviews is computed by the read-side endpoint.
    let topology = if repo.get_topology_mode().await.uses_derived() {
        topology_projection::derived_topology(topo, &nodes).await.0
    } else {
        // Dependency edge child → parent feeds parent-down suppression (ADR-015).
        topology_projection::manual_topology(&nodes)
    };

    Ok(AlertConfigBase {
        rules,
        nodes,
        meta,
        pool_groups,
        topology,
        per_interface,
    })
}

/// Resolve the set of nodes currently inside an active maintenance window. Time-dependent (window
/// boundaries move with wall-clock), so it runs every refresh cycle — but over the *cached* node
/// list, not a fresh DB scan. Folder-group scopes expand against the inventory tree (recursive incl.
/// subgroups, ADR-022) — the same chain the Troubleshoot scope uses; only touches the DB when one is
/// actually active.
///
/// **An exemption (migration 0081) subtracts from the *inherited* half only.** A window that names
/// a node — `WindowScope::Node` — always applies, so an operator who released a box from its
/// group's window and then deliberately opened one on that box gets the suppression they asked
/// for. Were exemptions applied to the union, that second window would do nothing and there would
/// be nothing on screen saying why.
async fn resolve_maintenance(
    maintenance: &MaintenanceRepo,
    groups: &groups::GroupRepo,
    repo: &NodeRepo,
    nodes: &[yagra_common::Node],
) -> std::collections::BTreeSet<NodeId> {
    let scopes = maintenance.active_scopes().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to load maintenance windows");
        Vec::new()
    });
    let named = maintenance::nodes_named_by_a_window(&scopes, nodes);
    let mut inherited = maintenance::nodes_covered_by_a_class_window(&scopes, nodes);
    let folder_groups: Vec<Uuid> = scopes
        .iter()
        .filter(|(level, _)| *level == maintenance::WindowScope::FolderGroup)
        .filter_map(|(_, id)| Uuid::parse_str(id).ok())
        .collect();
    if !folder_groups.is_empty() {
        match groups.edges().await {
            Ok(edges) => {
                let mut group_ids: Vec<Uuid> = Vec::new();
                for root in folder_groups {
                    group_ids.extend(groups::group_subtree(&edges, root));
                }
                match repo.nodes_in_groups(&group_ids).await {
                    // Folder-group coverage is inherited too: the window names the folder, not the
                    // node, so releasing one member must be able to cancel it.
                    Ok(node_ids) => {
                        inherited.extend(node_ids.into_iter().map(yagra_common::NodeId::from))
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to resolve folder-group maintenance");
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "failed to load group edges for maintenance"),
        }
    }
    for node in exempt_nodes(maintenance, maintenance::ExemptionKind::Maintenance).await {
        inherited.remove(&node);
    }
    named.union(&inherited).copied().collect()
}

/// The nodes released from `kind`, or an empty set if the read fails.
///
/// Degrading to "nobody is exempt" keeps a database hiccup on the side that **suppresses more**,
/// which is the same direction every other failure in this refresh takes: a lost exemption means
/// an operator's release stops applying and they see the node go quiet again, whereas inventing
/// one would silently un-suppress a node during planned work.
async fn exempt_nodes(
    maintenance: &MaintenanceRepo,
    kind: maintenance::ExemptionKind,
) -> std::collections::BTreeSet<NodeId> {
    match maintenance.exempt_nodes(kind).await {
        Ok(ids) => ids.into_iter().map(yagra_common::NodeId::from).collect(),
        Err(e) => {
            tracing::warn!(error = %e, kind = kind.as_str(), "failed to load suppression exemptions");
            std::collections::BTreeSet::new()
        }
    }
}

/// Assemble the full alert config (config base + current maintenance). Used for the initial
/// synchronous load at startup; the refresh loop uses the two halves directly with generation
/// caching so the base isn't rebuilt when config is unchanged (S6).
pub(crate) async fn load_alert_config(
    repo: &NodeRepo,
    thresholds: &ThresholdStore,
    maintenance: &MaintenanceRepo,
    groups: &groups::GroupRepo,
    topo: &topology_projection::TopologySources,
) -> anyhow::Result<AlertConfig> {
    let base = load_alert_config_base(repo, thresholds, groups, topo).await?;
    let in_maintenance = resolve_maintenance(maintenance, groups, repo, &base.nodes).await;
    Ok(AlertConfig::new(base.rules, base.meta)
        .with_topology(base.topology)
        .with_maintenance(in_maintenance)
        .with_pool_groups(base.pool_groups)
        .with_per_interface(base.per_interface))
}

/// Load the unexpired mutes into the notifier (check ids recomputed from names here). A
/// group-scoped mute is expanded to one per-node entry across the folder-group subtree (recursive,
/// ADR-022), so the notifier's per-node matching is unchanged; the expansion re-runs each refresh
/// so membership changes are honored. Failures degrade to the existing snapshot (warn).
pub(crate) async fn load_mutes(
    notifier: &Notifier,
    maintenance: &MaintenanceRepo,
    repo: &NodeRepo,
    groups: &groups::GroupRepo,
) {
    let mutes = match maintenance.list_mutes().await {
        Ok(mutes) => mutes,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load mutes");
            return;
        }
    };
    let mut active: Vec<ActiveMute> = Vec::new();
    let mut group_roots: Vec<Uuid> = Vec::new();
    for m in &mutes {
        match m.scope_kind {
            maintenance::MuteScope::Node => {
                if let Some(node_id) = m.node_id {
                    active.push(ActiveMute::new(node_id, m.check_name.as_deref()));
                }
            }
            maintenance::MuteScope::Group => {
                if let Some(group_id) = m.group_id {
                    group_roots.push(group_id);
                }
            }
        }
    }
    if !group_roots.is_empty() {
        match groups.edges().await {
            Ok(edges) => {
                let mut group_ids: Vec<Uuid> = Vec::new();
                for root in group_roots {
                    group_ids.extend(groups::group_subtree(&edges, root));
                }
                match repo.nodes_in_groups(&group_ids).await {
                    // A group mute silences the whole node (check=None) — and, being inherited
                    // rather than named, is the half a release can cancel. The node mutes pushed
                    // above are deliberately not filtered: a mute naming this node always applies.
                    Ok(node_ids) => {
                        let exempt =
                            exempt_nodes(maintenance, maintenance::ExemptionKind::Mute).await;
                        active.extend(
                            node_ids
                                .into_iter()
                                .filter(|n| !exempt.contains(&yagra_common::NodeId::from(*n)))
                                .map(|n| ActiveMute::new(n, None)),
                        );
                    }
                    Err(e) => tracing::warn!(error = %e, "failed to resolve group mute nodes"),
                }
            }
            Err(e) => tracing::warn!(error = %e, "failed to load group edges for mutes"),
        }
    }
    notifier.set_mutes(active).await;
}

/// Load the DB notification channels + routing rules into the notifier. Failures degrade to
/// the existing snapshot (warn) rather than dropping routing.
pub(crate) async fn load_routing(notifier: &Notifier, notifications: &NotificationRepo) {
    let channels = notifications
        .list_open_channels()
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to load notification channels");
            Vec::new()
        });
    let rules = notifications.list_rules().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to load routing rules");
        Vec::new()
    });
    notifier.set_routing(channels, rules).await;
}

/// Leader-only refresh loop: keep the alert engine's config fresh. Rebuild the config-derived base
/// only when the config generation changes (S6, [`config_gen`]); re-resolve time-dependent
/// maintenance windows each cycle; reload classifier + event rules so edits apply without a restart.
/// Runs until the shutdown token drops it. Spawned by `LeaderTasks::spawn_refresh_loops`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_alert_config_refresh(
    alerts: Arc<AlertManager>,
    repo: Arc<NodeRepo>,
    thresholds: Arc<ThresholdStore>,
    maintenance: Arc<MaintenanceRepo>,
    group_repo: Arc<groups::GroupRepo>,
    classifier: Arc<classification::Classifier>,
    classification: Arc<classification::ClassificationRepo>,
    event_engine: Arc<events::EventEngine>,
    topo_sources: topology_projection::TopologySources,
) {
    // Cache the config-derived alert base keyed by the config generation, so the full node scan +
    // meta/topology rebuild runs only after an actual config change (S6). Maintenance windows are
    // time-dependent, so re-resolve them each cycle over the cached node list, and only swap the
    // live config when the base or the in-maintenance set actually changed.
    let mut cached_base: Option<(u64, AlertConfigBase)> = None;
    let mut last_maintenance: Option<std::collections::BTreeSet<NodeId>> = None;
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        let generation = config_gen::current();
        let base_changed = cached_base.as_ref().map(|(g, _)| *g) != Some(generation);
        if base_changed {
            // 🚨 A failed rebuild installs nothing (ADR-080 決定 2). `cached_base` keeps its old
            // generation, so the next cycle sees the same mismatch and reads again — the retry is
            // the loop itself. Installing a partial base instead is what made a single database
            // blip resolve every open threshold alert in the fleet and page a recovery for each.
            match load_alert_config_base(&repo, &thresholds, &group_repo, &topo_sources).await {
                Ok(base) => cached_base = Some((generation, base)),
                Err(e) => {
                    metrics::counter!("yagra_alert_config_load_failures_total").increment(1);
                    tracing::warn!(error = %e, "rebuilding the alert config failed; keeping the previous one");
                    continue;
                }
            }
        }
        // Only reachable before the first successful build, and only when that build failed.
        let Some((_, base)) = cached_base.as_ref() else {
            continue;
        };
        // A release from inherited suppression is sized to the coverage in force when it is
        // granted, and coverage can stop sooner than it said it would. Re-derive them before
        // resolving: an orphaned release is not just a marker on a quiet row, it is a node the
        // *next* window over that group would skip. The handlers that remove coverage call this
        // too — this is the backstop that does not depend on anyone remembering to.
        match maintenance::reconcile_exemptions(&maintenance, &group_repo, &repo).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(count = n, "re-derived suppression exemptions"),
            Err(e) => tracing::warn!(error = %e, "failed to reconcile suppression exemptions"),
        }
        let in_maintenance =
            resolve_maintenance(&maintenance, &group_repo, &repo, &base.nodes).await;
        if base_changed || last_maintenance.as_ref() != Some(&in_maintenance) {
            let config = AlertConfig::new(base.rules.clone(), base.meta.clone())
                .with_topology(base.topology.clone())
                .with_maintenance(in_maintenance.clone())
                .with_pool_groups(base.pool_groups.clone())
                .with_per_interface(base.per_interface.clone());
            alerts.set_config(config);
            last_maintenance = Some(in_maintenance);
        }
        // Pick up classification-rule edits without a restart (also reloaded inline by the
        // rule-edit handlers; this catches any drift / multi-instance future).
        if let Err(e) = classifier.reload(&classification).await {
            tracing::warn!(error = %e, "failed to refresh classification rules");
        }
        // Event rules + node address map (also reloaded inline after rule edits).
        event_engine.reload(&repo).await;
    }
}

#[cfg(test)]
mod tests {

    /// This module's own source. Read through [`crate::module_source`] rather than
    /// `include_str!` + `.split("#[cfg(test)]")`, so a future `#[cfg(test)] mod x;` declaration
    /// cannot truncate what these two assertions see (ADR-089/090).
    fn production_source() -> String {
        let files =
            crate::module_source::files(&crate::module_source::roots("src/alerts", "config"));
        let (_, code) = files
            .into_iter()
            .find(|(name, _)| name == "config.rs")
            .expect("alerts/config.rs is a module root");
        // A floor: an absence claim over an empty string is satisfied by nothing at all.
        assert!(
            code.contains("async fn load_alert_config_base"),
            "the loader is not in the text these tests read; they would pass for want of anything to check"
        );
        code
    }

    /// **In shadow mode the alert engine receives the manual graph, and nothing else.**
    ///
    /// ADR-043 決定 5's safety property, and the reason [`AlertConfigBase`] carries one topology
    /// rather than two: the derived graph is chosen *only* when the mode says to use it, so there is
    /// no runtime state in which a preview graph could suppress a real alert. That is a property of
    /// one expression, and this is what stops the expression growing a second branch.
    ///
    /// The needles are assembled at runtime — a literal one would match this test's own source and
    /// pass forever.
    #[test]
    fn only_the_derived_mode_hands_the_engine_a_derived_graph() {
        let production = production_source();
        let guard = format!("get_topology_mode().await.{}()", "uses_derived");
        assert!(
            production.contains(&guard),
            "the topology choice is no longer gated on `uses_derived`"
        );
        let call = format!("{}::derived_topology", "topology_projection");
        assert_eq!(
            production.matches(call.as_str()).count(),
            1,
            "the derived graph reaches the engine from exactly one place; a second call site is \
             how a preview graph starts suppressing alerts"
        );
        assert!(
            !production.contains("shadow_topology"),
            "a shadow graph must not be a field the engine could be handed"
        );
    }

    /// 🚨 ADR-080: a failed read must not become an empty value here.
    ///
    /// Every load in this function used to carry its own `unwrap_or`, each justified as "the
    /// narrowing failure, so it is safe". They were all wrong the same way: narrowing is *not*
    /// harmless once alerts are open, because a check whose rule stops resolving is a check with no
    /// rule, and `AlertManager::observe` closes every alert on one of those. A single failed
    /// threshold read therefore resolved the whole fleet's alerts and paged a recovery for each.
    ///
    /// This is the only permanent guard: the failure needs a sick database to reproduce, so no
    /// behavioural test can stand in for it. The needle is assembled at runtime — a literal would
    /// match this test's own source and pass forever.
    #[test]
    fn the_alert_config_base_never_degrades_a_failed_load_to_an_empty_one() {
        let production = production_source();
        let f = production
            .split("async fn load_alert_config_base")
            .nth(1)
            .expect("the loader exists");
        let body = &f[..f.find("\n}\n").map_or(f.len(), |i| i + 2)];
        let needle = format!("unwrap{}or", "_");
        assert!(
            !body.contains(needle.as_str()),
            "a read in load_alert_config_base degrades to an empty value again; an empty ruleset \
             is indistinguishable from `every rule was deleted` and resolves the fleet"
        );
        assert!(
            body.contains("-> anyhow::Result<AlertConfigBase>"),
            "the loader must be able to fail, or the caller cannot keep the previous config"
        );
    }
}
