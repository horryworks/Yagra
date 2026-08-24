// SPDX-License-Identifier: AGPL-3.0-only
//! What an analysis is allowed to read, expressed as four traits (ADR-098).
//!
//! Before this file the fifteen analyses were methods on [`super::AnalysisRunner`], which holds
//! five PostgreSQL-backed concrete types. That is why **1,182 production lines shipped with no
//! tests**: not because nobody wrote them, but because no test could construct the value they are
//! methods on. ADR-093 measured that and stopped; this is the rest of it.
//!
//! The traits are cut by **what the caller needs**, never per repository (ADR-092 decision 1) —
//! twelve methods against five concrete types with a hundred-odd between them. The same shape as
//! [`crate::repo::NodeListing`], which was already here as precedent.
//!
//! 🎯 **[`AnalysisEvents`] is the point of the exercise.** The store choice — VictoriaLogs when it
//! is configured, PostgreSQL otherwise (ADR-024) — used to be a free function taking
//! `&EventRepo`, so every analysis held the PostgreSQL repo in its hand and a source-text check
//! watched for anyone using it. With the router behind this trait, an analysis has no `EventRepo`
//! to reach: reading the alert-linked subset by mistake is no longer a thing that can be written,
//! so that check is gone rather than converted. The routing itself still lives once, in
//! [`crate::logstore`]'s `route_*` helpers, shared with the MCP `event_stats` tool — the
//! implementation below calls them.
//!
//! ⚠️ What this does **not** buy: with a fake [`AnalysisEvents`] in a test, the branch *inside*
//! `route_*` (log store vs PostgreSQL) is not exercised. It needs an `EventRepo`, so it needs a
//! database. The purchase is "an analysis cannot bypass the router", not "the router is right".

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::broadcast;
use uuid::Uuid;
use yagra_common::Node;
use yagra_topology::Topology;

use crate::events::{
    EventAuthSource, EventBucketCount, EventFilter, EventFlapStat, EventRepo, EventRow,
    EventSeverityCount, EventSignatureCount,
};
use crate::groups::GroupRepo;
use crate::logstore::{LogStore, NameIds};
use crate::repo::NodeRepo;
use crate::topology_projection::{derived_topology, TopologySources};

use super::{broadcast_job, AnalysisRepo, JobFrame};

// ── Progress ─────────────────────────────────────────────────────────────────────────────────

/// Where a running job's progress goes: persisted, then broadcast to the SSE stream.
///
/// A seam because it is the **only** effect the fifteen analyses have — twenty-five of the
/// twenty-six `progress` calls are inside a `run_*` body, and every one of them reaches
/// [`AnalysisRepo`]. Everything else an analysis touches was already a trait or a handle.
#[async_trait]
pub(super) trait JobProgress: Send + Sync {
    async fn tick(&self, id: Uuid, pct: i32, phase: &str);
}

/// The production sink: write the tick to `analysis_jobs`, then re-read the row and broadcast it.
pub(super) struct RepoProgress {
    repo: Arc<AnalysisRepo>,
    tx: broadcast::Sender<JobFrame>,
}

impl RepoProgress {
    pub(super) const fn new(repo: Arc<AnalysisRepo>, tx: broadcast::Sender<JobFrame>) -> Self {
        Self { repo, tx }
    }
}

#[async_trait]
impl JobProgress for RepoProgress {
    /// Both failures are swallowed on purpose: a progress tick that cannot be stored is a worse
    /// report, not a failed job, and failing the analysis over it would lose the findings.
    async fn tick(&self, id: Uuid, pct: i32, phase: &str) {
        if let Err(e) = self.repo.set_progress(id, pct, phase).await {
            tracing::warn!(error = %e, job = %id, "analysis progress update failed");
        }
        if let Ok(Some(job)) = self.repo.get(id).await {
            broadcast_job(&self.tx, &job);
        }
    }
}

// ── Passive events ───────────────────────────────────────────────────────────────────────────

/// Every passive-event read an analysis may make (ADR-024, ADR-022 Increment 2).
///
/// The four aggregates have a VictoriaLogs twin and route; `flap_stats` deliberately does not —
/// every action it counts is alert-linked, so PostgreSQL is complete for it (pinned by
/// `events::tests::event_flap_only_counts_rows_postgresql_keeps`). `recent_events` is the
/// timeline read `incident_signals` makes, which routes for the same reason the aggregates do.
#[async_trait]
pub(super) trait AnalysisEvents: Send + Sync {
    async fn counts_by_bucket(
        &self,
        filter: &EventFilter,
        bucket_secs: i64,
    ) -> anyhow::Result<Vec<EventBucketCount>>;
    async fn severity_counts(
        &self,
        filter: &EventFilter,
    ) -> anyhow::Result<Vec<EventSeverityCount>>;
    async fn unmatched_signatures(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventSignatureCount>>;
    async fn auth_sources(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventAuthSource>>;
    /// Fire/clear counts per (rule, node). PostgreSQL-only by design — see the trait doc.
    async fn flap_stats(&self, from_ms: i64, to_ms: i64) -> anyhow::Result<Vec<EventFlapStat>>;
    /// The newest events matching `filter`, for one node's incident timeline.
    async fn recent_events(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventRow>>;
}

/// The production implementation: the log store when one is configured, PostgreSQL otherwise.
pub(super) struct RoutedEvents {
    logs: Option<Arc<dyn LogStore>>,
    events: Arc<EventRepo>,
}

impl RoutedEvents {
    pub(super) const fn new(logs: Option<Arc<dyn LogStore>>, events: Arc<EventRepo>) -> Self {
        Self { logs, events }
    }
}

#[async_trait]
impl AnalysisEvents for RoutedEvents {
    async fn counts_by_bucket(
        &self,
        filter: &EventFilter,
        bucket_secs: i64,
    ) -> anyhow::Result<Vec<EventBucketCount>> {
        crate::logstore::route_counts_by_bucket(
            self.logs.as_ref(),
            &self.events,
            filter,
            bucket_secs,
        )
        .await
    }

    async fn severity_counts(
        &self,
        filter: &EventFilter,
    ) -> anyhow::Result<Vec<EventSeverityCount>> {
        crate::logstore::route_severity_counts(self.logs.as_ref(), &self.events, filter).await
    }

    async fn unmatched_signatures(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventSignatureCount>> {
        crate::logstore::route_unmatched_signatures(self.logs.as_ref(), &self.events, filter, limit)
            .await
    }

    async fn auth_sources(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventAuthSource>> {
        crate::logstore::route_auth_sources(self.logs.as_ref(), &self.events, filter, limit).await
    }

    async fn flap_stats(&self, from_ms: i64, to_ms: i64) -> anyhow::Result<Vec<EventFlapStat>> {
        self.events.event_flap_stats(from_ms, to_ms).await
    }

    async fn recent_events(
        &self,
        filter: &EventFilter,
        limit: i64,
    ) -> anyhow::Result<Vec<EventRow>> {
        match self.logs.as_ref() {
            // With ADR-024 on, PostgreSQL holds only the alert-linked subset, so a timeline built
            // from it shows the events that had already alerted and nothing that led up to them.
            Some(logs) => logs.search(filter, NameIds::default(), limit).await,
            None => self.events.list_events(filter, limit).await,
        }
    }
}

// ── Inventory ────────────────────────────────────────────────────────────────────────────────

/// The fleet as an analysis sees it: which nodes are in scope, and what they are called.
#[async_trait]
pub(super) trait FleetInventory: Send + Sync {
    async fn list_nodes(&self) -> anyhow::Result<Vec<Node>>;
    async fn nodes_in_groups(&self, group_ids: &[Uuid]) -> anyhow::Result<Vec<Uuid>>;
    async fn node_names(&self, ids: &[Uuid]) -> anyhow::Result<HashMap<Uuid, String>>;
    /// Every group's `(id, parent)` — a group scope covers the group and its whole subtree.
    async fn group_edges(&self) -> anyhow::Result<Vec<(Uuid, Option<Uuid>)>>;
}

pub(super) struct RepoInventory {
    nodes: Arc<NodeRepo>,
    groups: Arc<GroupRepo>,
}

impl RepoInventory {
    pub(super) const fn new(nodes: Arc<NodeRepo>, groups: Arc<GroupRepo>) -> Self {
        Self { nodes, groups }
    }
}

#[async_trait]
impl FleetInventory for RepoInventory {
    async fn list_nodes(&self) -> anyhow::Result<Vec<Node>> {
        self.nodes.list_nodes().await
    }

    async fn nodes_in_groups(&self, group_ids: &[Uuid]) -> anyhow::Result<Vec<Uuid>> {
        self.nodes.nodes_in_groups(group_ids).await
    }

    /// Unrestricted deliberately: the ids handed here are the job's own resolved scope, which was
    /// checked against the launching principal at create time (`api/analysis.rs`). Re-filtering
    /// would scope a background run to whoever happens to be reading the results.
    async fn node_names(&self, ids: &[Uuid]) -> anyhow::Result<HashMap<Uuid, String>> {
        self.nodes.node_names(None, ids).await
    }

    async fn group_edges(&self) -> anyhow::Result<Vec<(Uuid, Option<Uuid>)>> {
        self.groups.edges().await
    }
}

// ── Connectivity graph ───────────────────────────────────────────────────────────────────────

/// The derived connectivity graph, for `incident_correlate`'s one-hop neighbour expansion
/// (ADR-043 → ADR-022 Increment 2).
#[async_trait]
pub(super) trait DerivedGraph: Send + Sync {
    async fn derived(&self, nodes: &[Node]) -> Topology;
}

pub(super) struct ProjectedGraph {
    topo: TopologySources,
}

impl ProjectedGraph {
    pub(super) const fn new(topo: TopologySources) -> Self {
        Self { topo }
    }
}

#[async_trait]
impl DerivedGraph for ProjectedGraph {
    /// The anchor resolution is dropped: `incident_correlate` suppresses nothing, so an unresolved
    /// anchor costs it a peer in a diagnostic rather than silencing an outage. The gate that does
    /// care lives in `topology_mode.rs`.
    async fn derived(&self, nodes: &[Node]) -> Topology {
        derived_topology(&self.topo, nodes).await.0
    }
}
