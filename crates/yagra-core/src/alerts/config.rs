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

/// The reads [`load_alert_config_base`] is assembled from.
///
/// **Six methods, not four repositories** (ADR-093). The loader used to take `&NodeRepo`,
/// `&ThresholdStore`, `&GroupRepo` and `&TopologySources` — four concrete types holding a
/// `PgPool` between them, so there was no way to run the loader in a test and therefore no way to
/// check the one property that matters most about it: that a failed read does not become an empty
/// value. That was left to a test which greps this file for `unwrap_or`, and whose own doc claimed
/// no behavioural test could stand in for it. A fake that returns `Err` is that test.
///
/// The trait names what the loader *asks*, not who answers. Six of the sixty-odd methods those four
/// types expose are reachable from here; a seam sized to the types instead of to the questions was
/// the reason ADR-092 deferred this as expensive when it is not.
///
/// ⚠️ **`topology_mode` and `derived_topology` are two methods on purpose.** One `topology_for`
/// would read better and would move ADR-043 決定 5's choice — *the derived graph reaches the engine
/// only in `derived`* — out of the loader and into every implementation, fakes included. A test
/// would then exercise the fake's copy of the rule. The same mistake ADR-092 caught in its own
/// first draft.
#[async_trait::async_trait]
pub(crate) trait AlertConfigSources: Send + Sync {
    /// Every threshold rule in the deployment.
    async fn thresholds(&self) -> anyhow::Result<Vec<thresholds::StoredThreshold>>;
    /// The whole inventory. The expensive one, and why this is generation-gated.
    async fn nodes(&self) -> anyhow::Result<Vec<yagra_common::Node>>;
    /// `(group, parent, pool)` for every folder group — folder-pool inheritance (migration 0054).
    async fn folder_pools(&self) -> anyhow::Result<Vec<(Uuid, Option<Uuid>, Option<String>)>>;
    /// `(group, parent)` for every folder group, for the ancestor walk.
    async fn group_edges(&self) -> anyhow::Result<Vec<(Uuid, Option<Uuid>)>>;
    /// Metric names that publish one series per interface (ADR-076).
    async fn per_interface_metrics(&self) -> anyhow::Result<std::collections::BTreeSet<String>>;
    /// Which graph the deployment is configured to suppress with.
    ///
    /// Not a `Result`: it degrades to `Manual` inside the repository, which is the mode that
    /// changes nothing — see [`NodeRepo::get_topology_mode`] before copying that.
    async fn topology_mode(&self) -> crate::topology_mode::TopologyMode;
    /// The derived connectivity graph for these nodes (ADR-043). Only called when the mode says so.
    async fn derived_topology(&self, nodes: &[yagra_common::Node]) -> Topology;
}

/// The live sources: the four handles the loader used to take, behind the six questions it asks.
pub(crate) struct LiveConfigSources {
    pub(crate) repo: Arc<NodeRepo>,
    pub(crate) thresholds: Arc<ThresholdStore>,
    pub(crate) groups: Arc<groups::GroupRepo>,
    pub(crate) topo: topology_projection::TopologySources,
}

#[async_trait::async_trait]
impl AlertConfigSources for LiveConfigSources {
    async fn thresholds(&self) -> anyhow::Result<Vec<thresholds::StoredThreshold>> {
        self.thresholds.list_all().await
    }
    async fn nodes(&self) -> anyhow::Result<Vec<yagra_common::Node>> {
        self.repo.list_nodes().await
    }
    async fn folder_pools(&self) -> anyhow::Result<Vec<(Uuid, Option<Uuid>, Option<String>)>> {
        self.groups.pool_rows().await
    }
    async fn group_edges(&self) -> anyhow::Result<Vec<(Uuid, Option<Uuid>)>> {
        self.groups.edges().await
    }
    async fn per_interface_metrics(&self) -> anyhow::Result<std::collections::BTreeSet<String>> {
        // Constructed here rather than held as a field: it is a handle on the pool `repo` already
        // owns, and this runs only when the config generation advances.
        crate::collection::CollectionRepo::new(self.repo.pool())
            .per_interface_metric_names()
            .await
    }
    async fn topology_mode(&self) -> crate::topology_mode::TopologyMode {
        self.repo.get_topology_mode().await
    }
    async fn derived_topology(&self, nodes: &[yagra_common::Node]) -> Topology {
        topology_projection::derived_topology(&self.topo, nodes)
            .await
            .0
    }
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
    sources: &dyn AlertConfigSources,
) -> anyhow::Result<AlertConfigBase> {
    let rules = sources
        .thresholds()
        .await
        .map_err(|e| anyhow::anyhow!("load thresholds: {e}"))?;
    let nodes = sources
        .nodes()
        .await
        .map_err(|e| anyhow::anyhow!("load nodes: {e}"))?;
    // Folder-pool inheritance (0054).
    let pools = poolres::PoolResolver::build(
        sources
            .folder_pools()
            .await
            .map_err(|e| anyhow::anyhow!("load folder pools: {e}"))?,
    );
    // Folder-group threshold scope (ADR-075 増分 3): a rule on a group covers every group inside
    // it, so each node needs its group plus every group above it. Read the edges once — the walk
    // is per-node and `group_ancestors` is a linear scan of this slice.
    let group_edges = sources
        .group_edges()
        .await
        .map_err(|e| anyhow::anyhow!("load group edges: {e}"))?;
    // Which metrics are per-interface (ADR-076). Built from the collection catalogue, never from
    // whether a sample carries an `ifindex` label — that label is a row key, so a chassis reading
    // would otherwise be split into one bogus check per "port" (ADR-011).
    let per_interface = sources
        .per_interface_metrics()
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
    //
    // 🚨 The *choice* stays here rather than behind one `topology_for` method, and that is what
    // makes it testable: a seam that answered "the topology" would put this branch in the live
    // implementation and in every fake, so a test would exercise the fake's copy of the rule
    // instead of this one (ADR-092's lesson, ADR-093's application of it).
    let topology = if sources.topology_mode().await.uses_derived() {
        sources.derived_topology(&nodes).await
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
    sources: &dyn AlertConfigSources,
    maintenance: &MaintenanceRepo,
    groups: &groups::GroupRepo,
    repo: &NodeRepo,
) -> anyhow::Result<AlertConfig> {
    let base = load_alert_config_base(sources).await?;
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
    // Built once: it is four `Arc` clones and the same handles this loop already holds. The loop
    // keeps the individual handles too, because maintenance and mute resolution ask questions
    // that are not part of the config base.
    let sources = LiveConfigSources {
        repo: repo.clone(),
        thresholds: thresholds.clone(),
        groups: group_repo.clone(),
        topo: topo_sources.clone(),
    };
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
            match load_alert_config_base(&sources).await {
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
    use super::*;
    use std::sync::Mutex;
    use yagra_common::{Node, NodeId};

    /// This module's own source, read through [`crate::module_source`]: it removes each test-only
    /// item rather than truncating at the first one, so a test-only declaration added here later
    /// cannot shorten what the structural assertions see (ADR-089/090/091).
    fn production_source() -> String {
        let code = crate::module_source::code("src/alerts", "config");
        // A floor: an absence claim over an empty string is satisfied by nothing at all.
        assert!(
            code.contains("async fn load_alert_config_base"),
            "the loader is not in the text these tests read; they would pass for want of anything to check"
        );
        code
    }

    /// Which read a [`FakeSources`] should fail. One at a time, so a test names the read it broke.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Fails {
        Nothing,
        Thresholds,
        Nodes,
        FolderPools,
        GroupEdges,
        PerInterface,
    }

    /// Every fallible read. A seventh added to the trait makes this list wrong in a way the
    /// compiler cannot see, which is why the walk below also counts against the trait's own text.
    const FALLIBLE: [Fails; 5] = [
        Fails::Thresholds,
        Fails::Nodes,
        Fails::FolderPools,
        Fails::GroupEdges,
        Fails::PerInterface,
    ];

    struct FakeSources {
        fails: Fails,
        nodes: Vec<Node>,
        mode: crate::topology_mode::TopologyMode,
        /// The graph `derived_topology` hands back…
        derived: Topology,
        /// …and whether it was asked for at all, which is half of ADR-043 決定 5's property.
        derived_asked: Mutex<bool>,
    }

    impl FakeSources {
        fn new(fails: Fails, nodes: Vec<Node>, mode: crate::topology_mode::TopologyMode) -> Self {
            Self {
                fails,
                nodes,
                mode,
                derived: Topology::new(),
                derived_asked: Mutex::new(false),
            }
        }
        fn refuse(&self, which: Fails) -> anyhow::Result<()> {
            if self.fails == which {
                anyhow::bail!("the database said no");
            }
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl AlertConfigSources for FakeSources {
        async fn thresholds(&self) -> anyhow::Result<Vec<thresholds::StoredThreshold>> {
            self.refuse(Fails::Thresholds)?;
            Ok(Vec::new())
        }
        async fn nodes(&self) -> anyhow::Result<Vec<Node>> {
            self.refuse(Fails::Nodes)?;
            Ok(self.nodes.clone())
        }
        async fn folder_pools(&self) -> anyhow::Result<Vec<(Uuid, Option<Uuid>, Option<String>)>> {
            self.refuse(Fails::FolderPools)?;
            Ok(Vec::new())
        }
        async fn group_edges(&self) -> anyhow::Result<Vec<(Uuid, Option<Uuid>)>> {
            self.refuse(Fails::GroupEdges)?;
            Ok(Vec::new())
        }
        async fn per_interface_metrics(
            &self,
        ) -> anyhow::Result<std::collections::BTreeSet<String>> {
            self.refuse(Fails::PerInterface)?;
            Ok(std::collections::BTreeSet::new())
        }
        async fn topology_mode(&self) -> crate::topology_mode::TopologyMode {
            self.mode
        }
        async fn derived_topology(&self, _nodes: &[Node]) -> Topology {
            *self.derived_asked.lock().expect("no panic holds this lock") = true;
            self.derived.clone()
        }
    }

    fn nid(n: u128) -> NodeId {
        NodeId::from(Uuid::from_u128(n))
    }

    fn node(id: u128, parent: Option<u128>) -> Node {
        let mut n = Node::new(
            nid(id),
            format!("n{id}"),
            "10.0.0.1".parse().expect("an address"),
        );
        n.parent = parent.map(nid);
        n
    }

    /// **Acceptance side, and it runs first on purpose.** Everything below asserts a *refusal*, and
    /// a loader that refused everything would satisfy all of them
    /// (`rejection-only-tests-pass-when-everything-rejects`).
    #[tokio::test]
    async fn a_healthy_source_set_produces_the_base_it_was_given() {
        let sources = FakeSources::new(
            Fails::Nothing,
            vec![node(1, None), node(2, Some(1))],
            crate::topology_mode::TopologyMode::Manual,
        );
        let base = load_alert_config_base(&sources)
            .await
            .expect("nothing failed, so nothing should refuse");
        assert_eq!(base.nodes.len(), 2);
        assert_eq!(base.meta.len(), 2, "every node gets a metadata entry");
    }

    /// 🚨 **ADR-080: a failed read refuses; it never becomes an empty value.**
    ///
    /// Each read used to carry its own `unwrap_or`, justified as "the narrowing failure, so it is
    /// safe". They were wrong the same way: narrowing is not harmless once alerts are open, because
    /// a check whose rule stops resolving is a check with no rule, and `AlertManager::observe`
    /// closes every alert on one of those. A single failed threshold read therefore resolved the
    /// whole fleet's alerts and paged a recovery for each.
    ///
    /// ⚠️ **This test could not be written before ADR-093, and the version it replaced said so** —
    /// it grepped the loader for `unwrap_or`, and its doc claimed "the failure needs a sick database
    /// to reproduce, so no behavioural test can stand in for it". A fake that returns `Err` is a
    /// sick database. The claim was true of the code as it stood, and false of the code it described.
    #[tokio::test]
    async fn every_fallible_read_refuses_rather_than_narrowing() {
        for which in FALLIBLE {
            let sources = FakeSources::new(
                which,
                vec![node(1, None)],
                crate::topology_mode::TopologyMode::Manual,
            );
            assert!(
                load_alert_config_base(&sources).await.is_err(),
                "{which:?} failed and the loader returned a base anyway; an empty ruleset is \
                 indistinguishable from `every rule was deleted` and resolves the fleet"
            );
        }

        // …and the walk covers every fallible read the trait declares, rather than a list that can
        // fall behind it. A read nobody breaks is a read that may still narrow.
        let src = production_source();
        let decl = src
            .split("trait AlertConfigSources: Send + Sync {")
            .nth(1)
            .expect("the trait is declared in this file");
        let decl = &decl[..decl.find("\n}\n").unwrap_or(decl.len())];
        assert!(
            decl.contains("async fn thresholds"),
            "the slice is not the trait's body, so the count below is of nothing"
        );
        assert_eq!(
            decl.matches("-> anyhow::Result<").count(),
            FALLIBLE.len(),
            "the trait declares a number of fallible reads this test does not walk"
        );
    }

    /// **In shadow mode the engine receives the manual graph, and the derived one is not even
    /// computed.**
    ///
    /// ADR-043 決定 5's safety property, and the reason [`AlertConfigBase`] carries one topology
    /// rather than two: there is no runtime state in which a preview graph could suppress a real
    /// alert. Before ADR-093 this was a needle in this file's own text; now the loader runs.
    #[tokio::test]
    async fn only_the_derived_mode_hands_the_engine_a_derived_graph() {
        // The manual graph says 2 → 1; the derived graph the fake offers says 3 → 1. The two are
        // told apart by which child has a parent at all, so neither can be mistaken for the other.
        let mut derived = Topology::new();
        derived.add_dependency(nid(3), nid(1));
        let nodes = vec![node(1, None), node(2, Some(1)), node(3, None)];

        for mode in [
            crate::topology_mode::TopologyMode::Manual,
            crate::topology_mode::TopologyMode::Shadow,
        ] {
            let mut sources = FakeSources::new(Fails::Nothing, nodes.clone(), mode);
            sources.derived = derived.clone();
            let base = load_alert_config_base(&sources).await.expect("healthy");
            assert_eq!(
                base.topology.parents_of(nid(2)).len(),
                1,
                "{mode:?} must hand the engine the manual graph"
            );
            assert!(
                base.topology.parents_of(nid(3)).is_empty(),
                "{mode:?} handed the engine the derived graph"
            );
            assert!(
                !*sources
                    .derived_asked
                    .lock()
                    .expect("no panic holds this lock"),
                "{mode:?} computed a graph it is not allowed to use"
            );
        }

        // …and the derived graph really does reach the engine in `derived`, or the two assertions
        // above would hold for a loader that never used it at all.
        let mut sources = FakeSources::new(
            Fails::Nothing,
            nodes,
            crate::topology_mode::TopologyMode::Derived,
        );
        sources.derived = derived;
        let base = load_alert_config_base(&sources).await.expect("healthy");
        assert_eq!(
            base.topology.parents_of(nid(3)).len(),
            1,
            "derived mode must hand the engine the derived graph"
        );
    }

    /// **The derived graph reaches the engine from exactly one place.**
    ///
    /// Structural, and it stays structural: "how many call sites" is not something a type or a fake
    /// can answer, and a second one is how a preview graph starts suppressing alerts. The
    /// behavioural half of what this used to assert moved into the test above — its absence here is
    /// not the property being dropped.
    ///
    /// The needles are assembled at runtime — a literal one would match this test's own source and
    /// pass forever.
    #[test]
    fn the_derived_graph_has_one_call_site() {
        let production = production_source();
        let guard = format!("topology_mode().await.{}()", "uses_derived");
        assert!(
            production.contains(&guard),
            "the topology choice is no longer gated on `uses_derived`"
        );
        let call = format!("sources.{}(&nodes)", "derived_topology");
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
}
