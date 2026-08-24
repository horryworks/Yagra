// SPDX-License-Identifier: AGPL-3.0-only
//! The impure half: read the stores, then hand the result to the pure halves and publish.
//!
//! Everything here `.await`s — [`PollDispatcher`] holds the bus plus one handle per side table,
//! and resolves a node's credential, collection set and monitor kind before
//! [`assemble_node_jobs`](super::assemble::assemble_node_jobs) can run.
//!
//! ⚠️ [`PollDispatcherSeams`] is named for seams but is not one: eight of its ten fields are
//! concrete repositories, so this file has **no tests** — the same shape ADR-092 recorded for
//! `AnalysisSeams`. Fixing that means putting a seam on the *effect* (ADR-092 decision 1), not
//! renaming this one.

use crate::collection::CollectionRepo;
use crate::dns_check::DnsCheckRepo;
use crate::l3_routing::RoutingPlan;
use crate::secrets::{self, CredentialStore};
use crate::url_check::UrlCheckRepo;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;
use yagra_bus::SyncBus;

// The pure halves this file feeds from the stores (ADR-096).
use super::assemble::{
    assemble_node_jobs, hint_admits, AdjacencyPolicy, MonitorHints, SpecialMonitor,
};
use super::SnmpAuth;
use yagra_bus::{JobSpec, NatsBus, PollJob};
use yagra_common::{CollectionItem, HttpAuth, Node, NodeId, NodeKind, NodeRows};

/// How long a [`RoutingPlan`] is reused before it is rebuilt.
///
/// The plan is derived from the fleet's host-route addressing, which changes when an interface is
/// re-addressed — on the order of weeks. Rebuilding it per sweep would mean a `jsonb_array_elements`
/// scan of `node_l3` every thirty seconds to find tens of rows; five minutes matches the derivation
/// task's own cadence, so the probe list and the graph it feeds move together.
const ROUTING_PLAN_TTL: Duration = Duration::from_secs(300);

/// The cached plan and when it was built.
struct CachedRoutingPlan {
    plan: Arc<RoutingPlan>,
    built_at: Option<std::time::Instant>,
}

/// Turns a single node into the poll jobs the bus needs and publishes them. Shared by the
/// periodic [`run_scheduler`](crate) loop (which jitters each publish to avoid a stampede) and
/// the on-demand "poll now" API action (which publishes immediately). Holds the bus plus the
/// seams needed to resolve a node's SNMP auth and collection set — never a direct poller call
/// (core⇄poller flows only through the bus, ADR-003).
pub struct PollDispatcher {
    bus: Arc<NatsBus>,
    creds: Arc<CredentialStore>,
    collection: Arc<CollectionRepo>,
    /// Per-node URL-monitor configs (a node with one is a URL monitor → HTTP job, no ICMP/SNMP).
    url_checks: Arc<UrlCheckRepo>,
    /// Per-node DNS-monitor configs (a node with one is a DNS monitor → DNS job, no ICMP/SNMP).
    dns_checks: Arc<DnsCheckRepo>,
    /// Per-node Meraki bindings (a node with one is polled by the org collector → no ICMP/SNMP).
    meraki_devices: Arc<crate::meraki::MerakiDeviceRepo>,
    /// Deployment-wide settings, for the on-demand path only. The periodic sweep resolves the
    /// neighbour policy once per round and passes it in explicitly (as it already does with the
    /// poll interval), so this is never read in the hot loop.
    settings: Arc<crate::repo::NodeRepo>,
    /// Interface addresses, read only to rebuild the route-probe plan (ADR-043 Increment 4).
    l3: Arc<crate::l3::L3Repo>,
    /// The route-probe plan, rebuilt at most every [`ROUTING_PLAN_TTL`]. Behind a lock rather than
    /// recomputed per sweep because it is a `jsonb_array_elements` scan of `node_l3` that returns
    /// tens of rows, and the addressing it reads changes on the order of weeks.
    routing_plan: tokio::sync::RwLock<CachedRoutingPlan>,
    /// v2c community fallback for nodes without a bound credential.
    env_community: Option<String>,
    /// Fallback poll interval (seconds) stamped on on-demand "poll now" jobs. The periodic
    /// scheduler resolves the effective interval per node (profile override → DB default) and
    /// passes it explicitly; this is only the manual-poll default (the poller ignores the value).
    interval_secs: u32,
}

/// The seams a [`PollDispatcher`] needs, as one named value.
///
/// There is one repository per [`NodeKind`] that has a side table, so the argument list grew with
/// every kind added — eight positional `Arc`s, three of them the same shape, behind an
/// `#[allow(clippy::too_many_arguments)]`. Positional arguments of the same type are exactly where
/// two get swapped without a type error, so adding a kind should add a *field*, not a position.
pub struct PollDispatcherSeams {
    pub bus: Arc<NatsBus>,
    pub creds: Arc<CredentialStore>,
    pub collection: Arc<CollectionRepo>,
    pub url_checks: Arc<UrlCheckRepo>,
    pub dns_checks: Arc<DnsCheckRepo>,
    pub meraki_devices: Arc<crate::meraki::MerakiDeviceRepo>,
    /// Deployment-wide settings — see the field of the same name on [`PollDispatcher`].
    pub settings: Arc<crate::repo::NodeRepo>,
    /// Interface addresses — see the field of the same name on [`PollDispatcher`].
    pub l3: Arc<crate::l3::L3Repo>,
    /// v2c community fallback for nodes without a bound credential.
    pub env_community: Option<String>,
    /// Fallback poll interval (seconds) for on-demand "poll now" jobs — see the field of the
    /// same name on [`PollDispatcher`].
    pub interval_secs: u32,
}

impl PollDispatcher {
    #[must_use]
    pub fn new(seams: PollDispatcherSeams) -> Self {
        let PollDispatcherSeams {
            bus,
            creds,
            collection,
            url_checks,
            dns_checks,
            meraki_devices,
            settings,
            l3,
            env_community,
            interval_secs,
        } = seams;
        Self {
            bus,
            creds,
            collection,
            url_checks,
            dns_checks,
            meraki_devices,
            settings,
            l3,
            routing_plan: tokio::sync::RwLock::new(CachedRoutingPlan {
                plan: Arc::new(RoutingPlan::default()),
                built_at: None,
            }),
            env_community,
            interval_secs,
        }
    }

    /// The deployment's adjacency policy, degrading to the compiled default on a read failure —
    /// resolved once per sweep, and once per action on the on-demand path.
    pub async fn adjacency_policy(&self) -> AdjacencyPolicy {
        let mut policy: AdjacencyPolicy = self.settings.get_adjacency_settings().await.into();
        if policy.routing_enabled {
            policy.routing_plan = self.routing_plan().await;
        }
        policy
    }

    /// The route-probe plan, rebuilt when the cached one has expired (ADR-043 Increment 4).
    ///
    /// A failed read keeps the previous plan rather than falling back to an empty one: an empty
    /// plan issues no probes at all, so a transient database error would silently stop collecting
    /// point-to-point links — and nothing downstream could tell that apart from "there are none".
    /// The TTL is not advanced on failure, so the next sweep retries.
    async fn routing_plan(&self) -> Arc<RoutingPlan> {
        {
            let cached = self.routing_plan.read().await;
            if cached
                .built_at
                .is_some_and(|at| at.elapsed() < ROUTING_PLAN_TTL)
            {
                return cached.plan.clone();
            }
        }
        let mut cached = self.routing_plan.write().await;
        // Re-checked under the write lock: two sweeps racing here would otherwise both rebuild.
        if cached
            .built_at
            .is_some_and(|at| at.elapsed() < ROUTING_PLAN_TTL)
        {
            return cached.plan.clone();
        }
        match self.l3.host_addresses().await {
            Ok(hosts) => {
                let plan = Arc::new(RoutingPlan::build(&hosts));
                metrics::gauge!("yagra_route_probe_nodes")
                    .set(u32::try_from(plan.prober_count()).unwrap_or(u32::MAX));
                cached.plan = plan;
                cached.built_at = Some(std::time::Instant::now());
            }
            Err(e) => {
                tracing::warn!(error = %e, "route-probe plan refresh failed; keeping the previous plan");
            }
        }
        cached.plan.clone()
    }

    /// Every node that is a DNS monitor, for the periodic scheduler to preload once per sweep.
    ///
    /// Without this the sweep would need a `dns_checks` lookup per node per round — the exact
    /// per-node round trip the Meraki path already avoids. DNS monitors are few, so the set is tiny.
    ///
    /// ⚠️ On failure this yields an **empty** set, which [`MonitorHints`] reads as "there are no DNS
    /// monitors", not as "no hint" — so a failed preload costs one sweep's DNS jobs rather than
    /// silently reverting to 50k per-node queries. The warning says so.
    pub async fn dns_node_ids(&self) -> HashSet<Uuid> {
        self.dns_checks.node_ids().await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "dns-check id preload failed; DNS monitors are skipped for this sweep");
            HashSet::new()
        })
    }

    /// Every node that is a URL monitor, for the periodic scheduler to preload once per sweep.
    /// The [`Self::dns_node_ids`] twin — see it for the empty-set-on-failure semantics.
    pub async fn url_node_ids(&self) -> HashSet<Uuid> {
        self.url_checks.node_ids().await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "url-check id preload failed; URL monitors are skipped for this sweep");
            HashSet::new()
        })
    }

    /// Resolve a node's poll jobs. A URL monitor (it has a `url_checks` row) gets a single HTTP
    /// job; otherwise SNMP auth (decrypted in core, never the poller — ADR-018/020) + collection
    /// set + the always-on ICMP. The caller resolves the effective interval and decides whether to
    /// jitter (periodic) or publish at once (poll now).
    pub async fn build_node_jobs(
        &self,
        node: &Node,
        interval_secs: u32,
    ) -> Vec<(PollJob, &'static str)> {
        // Meraki short-circuit: a node with a Meraki binding is polled by the org collector, so it
        // emits no per-node ICMP/SNMP/HTTP job. This guards the on-demand "poll now" path; the
        // periodic scheduler already excludes these nodes (it preloads their ids) and calls
        // `build_scheduled_jobs`, which skips this per-node lookup.
        let bound = match self.meraki_devices.get(node.id.as_uuid()).await {
            Ok(bound) => bound.is_some(),
            Err(e) => {
                tracing::warn!(node = %node.id, error = %e, "meraki-device load failed; treating as non-Meraki node");
                false
            }
        };
        // Asked as "is this kind scheduled per node" rather than "is it Meraki", so a future kind
        // that is also collected elsewhere does not have to remember to add itself here.
        let kind = NodeKind::resolve(NodeRows {
            meraki: bound,
            ..NodeRows::default()
        });
        if !kind.is_polled_per_node() {
            return Vec::new();
        }
        // One node, one settings read — this path is an operator action, not the sweep.
        let neighbors = self.adjacency_policy().await;
        // Default = every hint `None` = look every side table up directly. Correct here: one node,
        // and preloading a fleet-wide id set to serve it would cost far more than it saves.
        self.build_scheduled_jobs_hinted(node, interval_secs, MonitorHints::default(), &neighbors)
            .await
    }

    /// A node's poll jobs **without** the Meraki-binding lookup — used by the periodic scheduler,
    /// which already excludes Meraki nodes by preloading their ids, so re-querying `meraki_devices`
    /// per node every sweep is wasted work (one JOIN per node per round at scale). The on-demand
    /// "poll now" path keeps the guard via [`Self::build_node_jobs`]. A URL monitor gets a single
    /// HTTP job; otherwise SNMP auth (decrypted in core) + collection set + the always-on ICMP.
    ///
    /// `neighbors` is resolved by the caller — once per sweep, or once per action on the on-demand
    /// path — so this never reads settings per node.
    ///
    /// `hints` is what keeps this from adding a per-node round trip *per monitor kind* to every
    /// sweep: when a kind's set is present, only nodes in it are looked up, so the 99.9% of the
    /// fleet that are ordinary devices cost nothing. [`MonitorHints::default`] (the on-demand
    /// "poll now" path, a single node) falls back to querying directly.
    pub async fn build_scheduled_jobs_hinted(
        &self,
        node: &Node,
        interval_secs: u32,
        hints: MonitorHints<'_>,
        neighbors: &AdjacencyPolicy,
    ) -> Vec<(PollJob, &'static str)> {
        let node_uuid = node.id.as_uuid();
        // Both side-table lookups are hint-gated. The URL one was not, for a long time: it ran
        // unconditionally for every node on every sweep while the DNS one beside it was already
        // short-circuited, so a 50k fleet paid 50k `url_checks` queries a round to find the handful
        // of URL monitors. Note the gate has to wrap the lookup itself — the ordering here is the
        // fix, and passing a hint without moving the query would have changed nothing.
        let url = if hint_admits(hints.url, node_uuid) {
            self.url_checks.get(node_uuid).await.unwrap_or_else(|e| {
                tracing::warn!(node = %node.id, error = %e, "url-check load failed; treating as non-URL node");
                None
            })
        } else {
            None
        };
        // Skipped entirely once URL has already won — `resolve` would discard it.
        let dns = if url.is_none() && hint_admits(hints.dns, node_uuid) {
            self.dns_checks.get(node_uuid).await.unwrap_or_else(|e| {
                tracing::warn!(node = %node.id, error = %e, "dns-check load failed; treating as non-DNS node");
                None
            })
        } else {
            None
        };
        // Single-purpose monitors replace ICMP/SNMP entirely, so neither the credential nor the
        // collection lookup is worth paying for them.
        if let Some(monitor) = SpecialMonitor::resolve(url.as_ref(), dns.as_ref()) {
            // A URL monitor may be bound to a credential; that lookup is the same `creds.open` +
            // parse + static-reason-warn path SNMP takes, and costs one round trip per bound URL
            // monitor per sweep. Unbound monitors and DNS monitors pay nothing.
            let monitor = if let SpecialMonitor::Url { cfg, .. } = &monitor {
                let auth = resolve_http_auth(&self.creds, node, cfg.credential).await;
                monitor.with_http_auth(auth)
            } else {
                monitor
            };
            return assemble_node_jobs(node, None, &[], Some(monitor), interval_secs, neighbors);
        }
        let auth = resolve_snmp_auth(&self.creds, node, self.env_community.as_deref()).await;
        // Only resolve the collection set when SNMP is configured (ICMP needs none).
        let items = if auth.is_some() {
            resolve_node_collection(&self.collection, node).await
        } else {
            Vec::new()
        };
        assemble_node_jobs(node, auth.as_ref(), &items, None, interval_secs, neighbors)
    }

    /// The node's poll jobs as reusable working-set [`JobSpec`]s (ADR-020) — the distributed-pool
    /// analogue of [`Self::build_node_jobs`], built via [`Self::build_scheduled_jobs_hinted`]
    /// (Meraki nodes are already excluded on the scheduler path, so the per-node Meraki lookup is
    /// skipped). The per-dispatch `job_id` is dropped here; the poller stamps a fresh one each time
    /// it schedules the spec locally.
    ///
    /// `hints` carries the sweep's preloaded per-kind monitor id sets — see
    /// [`Self::build_scheduled_jobs_hinted`] for why they matter at fleet scale.
    pub async fn build_node_specs(
        &self,
        node: &Node,
        interval_secs: u32,
        hints: MonitorHints<'_>,
        neighbors: &AdjacencyPolicy,
    ) -> Vec<JobSpec> {
        self.build_scheduled_jobs_hinted(node, interval_secs, hints, neighbors)
            .await
            .iter()
            .map(|(job, _kind)| JobSpec::from_job(job))
            .collect()
    }

    /// Publish one already-built job to `pool`'s subject, bumping the published-jobs counter (used
    /// by the periodic scheduler after its per-job jitter delay). Routing per pool keeps a job local
    /// to the pollers serving that pool (ADR-009). Returns whether the publish succeeded.
    pub async fn publish_job(&self, job: PollJob, kind: &str, node: NodeId, pool: &str) -> bool {
        publish(&self.bus, pool, job, kind, node).await
    }

    /// Build and immediately publish every poll job for a node (no jitter) — the operator's
    /// "poll now" action. Returns how many jobs were published. Stamps the dispatcher's default
    /// interval (the poller ignores the value; cadence is owned by the periodic scheduler).
    ///
    /// `pool` is the node's **effective** pool, resolved by the caller (which has the folder tree
    /// for inheritance) — the dispatcher deliberately doesn't own a group repo just for this.
    pub async fn poll_now(&self, node: &Node, pool: &str) -> usize {
        let jobs = self.build_node_jobs(node, self.interval_secs).await;
        let mut published = 0u64;
        for (job, kind) in jobs {
            if publish(&self.bus, pool, job, kind, node.id).await {
                published += 1;
            }
        }
        metrics::counter!("yagra_manual_poll_jobs_total").increment(published);
        usize::try_from(published).unwrap_or(usize::MAX)
    }
}

/// Resolve a node's effective collection set, defaulting to the built-in catalog when nothing is
/// configured (or the lookup fails) so polling always has a sensible default.
async fn resolve_node_collection(collection: &CollectionRepo, node: &Node) -> Vec<CollectionItem> {
    match collection
        .list_items_for_node(node.id.as_uuid(), node.profile.map(|p| p.0))
        .await
    {
        Ok(scoped) => {
            let resolved = yagra_common::resolve_collection_set(&scoped);
            if resolved.is_empty() {
                yagra_common::builtin_catalog()
            } else {
                resolved
            }
        }
        Err(e) => {
            tracing::warn!(node = %node.id, error = %e, "collection load failed; using built-in catalog");
            yagra_common::builtin_catalog()
        }
    }
}

/// Resolve a node's SNMP auth from its bound credential (decrypted in core, never the poller —
/// ADR-018/020). The credential `kind` picks the protocol: `snmp_v3` secrets are USM JSON docs;
/// anything else is treated as a v2c community (back-compat with credentials created before kinds
/// were meaningful). The env community is a v2c fallback for nodes without a bound credential.
/// Resolve a URL monitor's bound credential into the [`HttpAuth`] the poll job carries.
///
/// Mirrors [`resolve_snmp_auth`]: open the sealed secret, parse it, and on any failure warn with a
/// **static** reason and fall back to an unauthenticated probe. The alternative — skipping the poll
/// — would read as an outage; an unauthenticated probe against an endpoint that needs credentials
/// reads as a 401, which is closer to the truth and visible in the status code metric.
async fn resolve_http_auth(
    creds: &CredentialStore,
    node: &Node,
    credential: Option<yagra_common::CredentialId>,
) -> Option<HttpAuth> {
    let cred = credential?;
    match creds.open(cred.as_uuid()).await {
        Ok(Some((kind, bytes))) => match secrets::parse_http_auth(&kind, &bytes) {
            Ok(auth) => Some(auth),
            // Static reason only — never echo any part of the secret.
            Err(reason) => {
                tracing::warn!(node = %node.id, %reason, "invalid http auth credential");
                None
            }
        },
        Ok(None) => {
            tracing::warn!(node = %node.id, "bound credential not found");
            None
        }
        Err(e) => {
            tracing::warn!(node = %node.id, error = %e, "credential decrypt failed");
            None
        }
    }
}

async fn resolve_snmp_auth(
    creds: &CredentialStore,
    node: &Node,
    env_community: Option<&str>,
) -> Option<SnmpAuth> {
    if let Some(cred) = node.credential {
        match creds.open(cred.as_uuid()).await {
            Ok(Some((kind, bytes))) => {
                if kind == secrets::KIND_SNMP_V3 {
                    match secrets::SnmpV3Secret::parse(&bytes) {
                        Ok(secret) => return Some(SnmpAuth::V3(secret)),
                        // Static reason only — never echo any part of the secret.
                        Err(reason) => {
                            tracing::warn!(node = %node.id, %reason, "invalid snmp_v3 credential");
                        }
                    }
                } else if let Ok(community) = String::from_utf8(bytes) {
                    return Some(SnmpAuth::V2c(community));
                }
            }
            Ok(None) => tracing::warn!(node = %node.id, "bound credential not found"),
            Err(e) => tracing::warn!(node = %node.id, error = %e, "credential decrypt failed"),
        }
    }
    env_community.map(|c| SnmpAuth::V2c(c.to_owned()))
}

/// Publish a job to `pool`'s subject and bump the published-jobs counter, logging failures.
/// Returns success. Uses [`SyncBus::publish_job_for_pool`] so legacy per-job dispatch, "poll now",
/// and Meraki all stay local to the target pool (ADR-009) while an old wildcard poller still
/// absorbs them (N/N-1 compatible).
async fn publish(bus: &NatsBus, pool: &str, mut job: PollJob, kind: &str, node: NodeId) -> bool {
    // Seed the job with this dispatch's trace context so the poller's poll span (and core's later
    // result-ingest span) join one distributed trace (yagra-telemetry). A short-lived dispatch span
    // is the trace root for legacy/poll-now jobs; injection is a no-op when tracing export is off,
    // so `trace_context` stays empty and off the wire (N/N-1 safe, ADR-017). Enter/inject/drop the
    // guard before the await — never hold a span guard across `.await`.
    {
        let dispatch = tracing::info_span!("dispatch.poll_job", %kind, node = %node, pool);
        let _enter = dispatch.enter();
        job.trace_context = yagra_telemetry::current_trace_context();
    }
    match bus.publish_job_for_pool(pool, job).await {
        Ok(()) => {
            metrics::counter!("yagra_jobs_published_total").increment(1);
            true
        }
        Err(e) => {
            tracing::warn!(error = %e, %kind, node = %node, pool, "failed to publish job");
            false
        }
    }
}
