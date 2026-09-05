// SPDX-License-Identifier: AGPL-3.0-only
//! The impure half: read the stores, then hand the result to the pure halves and publish.
//!
//! Everything here `.await`s — [`PollDispatcher`] holds the bus plus the four seams in
//! [`super::seams`], and resolves a node's credential, collection set and monitor kind before
//! [`assemble_node_jobs`](super::assemble::assemble_node_jobs) can run.
//!
//! 🚨 **This file decides what gets polled, so getting it wrong stops a node being monitored with
//! nothing raised** (the shape v0.2.13 shipped). It held eight concrete repositories until
//! ADR-111 and therefore had **no tests** — ADR-096 決定 3 and ADR-098 決定 4 each measured that
//! and stopped. The tests below are what the seams bought, and roughly half of them assert a
//! store was **not** queried: the per-node round trips [`MonitorHints`] exists to remove were
//! previously guarded by nothing but a comment.

use crate::l3_routing::RoutingPlan;
use crate::secrets;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;
use yagra_bus::SyncBus;

// The pure halves this file feeds from the stores (ADR-096).
use super::assemble::{
    assemble_node_jobs, hint_admits, AdjacencyPolicy, MonitorHints, SpecialMonitor,
};
use super::seams::{
    AdjacencySource, CollectionSource, CredentialSource, MonitorBindings, RepoAdjacency,
    RepoBindings,
};
use super::SnmpAuth;
use yagra_bus::{JobSpec, PollJob};
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
    bus: Arc<dyn SyncBus>,
    creds: Arc<dyn CredentialSource>,
    collection: Arc<dyn CollectionSource>,
    /// Which single-purpose monitor a node is, if any — a URL or DNS monitor replaces ICMP/SNMP
    /// entirely, and a Meraki-bound node is polled by the org collector instead.
    bindings: Arc<dyn MonitorBindings>,
    /// The deployment's adjacency toggles and the addressing the route-probe plan is built from.
    /// Read once per sweep (the policy is then passed down explicitly, as the poll interval
    /// already is), and once per action on the on-demand path — never in the hot loop.
    adjacency: Arc<dyn AdjacencySource>,
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

/// The stores a [`PollDispatcher`] is wired to.
///
/// ⚠️ **This is the wiring, not the seams** — every field here is a concrete store, and that is
/// correct: `main.rs` has the real ones. The seams are in [`super::seams`], built from these by
/// [`PollDispatcher::new`]. It was called `PollDispatcherSeams` until ADR-111, and the name was a
/// lie — `AnalysisSeams` told the same one and ADR-098 renamed it *while* cutting the traits,
/// because renaming alone is not the repair.
///
/// A struct rather than more parameters: there is one repository per [`NodeKind`] that has a side
/// table, so the argument list grew with every kind added — eight positional `Arc`s, three of them
/// the same shape, behind an `#[allow(clippy::too_many_arguments)]`. Positional arguments of the
/// same type are exactly where two get swapped without a type error, so adding a kind should add a
/// *field*, not a position.
pub struct PollDispatcherStores {
    pub bus: Arc<dyn SyncBus>,
    pub creds: Arc<crate::secrets::CredentialStore>,
    pub collection: Arc<crate::collection::CollectionRepo>,
    pub url_checks: Arc<crate::url_check::UrlCheckRepo>,
    pub dns_checks: Arc<crate::dns_check::DnsCheckRepo>,
    pub meraki_devices: Arc<crate::meraki::MerakiDeviceRepo>,
    /// Deployment-wide settings — half of the adjacency seam.
    pub settings: Arc<crate::repo::NodeRepo>,
    /// Interface addresses, read only to rebuild the route-probe plan (ADR-043 Increment 4).
    pub l3: Arc<crate::l3::L3Repo>,
    /// v2c community fallback for nodes without a bound credential.
    pub env_community: Option<String>,
    /// Fallback poll interval (seconds) for on-demand "poll now" jobs — see the field of the
    /// same name on [`PollDispatcher`].
    pub interval_secs: u32,
}

impl PollDispatcher {
    #[must_use]
    pub fn new(stores: PollDispatcherStores) -> Self {
        let PollDispatcherStores {
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
        } = stores;
        Self::from_seams(
            bus,
            creds,
            collection,
            Arc::new(RepoBindings::new(meraki_devices, url_checks, dns_checks)),
            Arc::new(RepoAdjacency::new(settings, l3)),
            env_community,
            interval_secs,
        )
    }

    /// The real constructor, over the seams rather than the stores — the one a test can reach.
    ///
    /// Positional here and a struct in [`Self::new`] on purpose: the five handles have five
    /// different types, so a swap is a compile error, and the public wiring is the place where
    /// three same-shaped `Arc`s made that argument (ADR-096).
    pub(super) fn from_seams(
        bus: Arc<dyn SyncBus>,
        creds: Arc<dyn CredentialSource>,
        collection: Arc<dyn CollectionSource>,
        bindings: Arc<dyn MonitorBindings>,
        adjacency: Arc<dyn AdjacencySource>,
        env_community: Option<String>,
        interval_secs: u32,
    ) -> Self {
        Self {
            bus,
            creds,
            collection,
            bindings,
            adjacency,
            routing_plan: tokio::sync::RwLock::new(CachedRoutingPlan {
                plan: Arc::new(RoutingPlan::default()),
                built_at: None,
            }),
            env_community,
            interval_secs,
        }
    }

    /// Whether SNMP polling is **configured** for this node — a bound credential, or the
    /// deployment-wide `YAGRA_SNMP_COMMUNITY` fallback that [`resolve_snmp_auth`] applies to every
    /// node without one. Says nothing about whether the device answers.
    ///
    /// Here, and not in the API layer, because this type is the only holder of `env_community`:
    /// deriving the answer from `nodes.credential_id` alone is wrong on any deployment that sets
    /// that variable, and it is wrong **silently** — the node really does have interface rows.
    /// The WebUI reads it as `NodeDetail.snmp_configured` to decide whether the tabs fed only by
    /// an SNMP walk have anything to show (ADR-119).
    ///
    /// ⚠️ **Deliberately over-reports.** A bound credential that fails to open makes
    /// [`resolve_snmp_auth`] return `None` while this still answers `true`. That direction is the
    /// safe one: an empty tab is visible and recoverable, a hidden tab holding real rows is
    /// neither. `snmp_configured_for_agrees_with_resolve_snmp_auth` pins the four ordinary cases
    /// and names this one as the intended difference.
    #[must_use]
    pub fn snmp_configured_for(&self, node: &Node) -> bool {
        node.credential.is_some() || self.env_community.is_some()
    }

    /// The deployment's adjacency policy, degrading to the compiled default on a read failure —
    /// resolved once per sweep, and once per action on the on-demand path.
    pub async fn adjacency_policy(&self) -> AdjacencyPolicy {
        let mut policy: AdjacencyPolicy = self.adjacency.settings().await.into();
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
        match self.adjacency.host_addresses().await {
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
        self.bindings.dns_node_ids().await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "dns-check id preload failed; DNS monitors are skipped for this sweep");
            HashSet::new()
        })
    }

    /// Every node that is a URL monitor, for the periodic scheduler to preload once per sweep.
    /// The [`Self::dns_node_ids`] twin — see it for the empty-set-on-failure semantics.
    pub async fn url_node_ids(&self) -> HashSet<Uuid> {
        self.bindings.url_node_ids().await.unwrap_or_else(|e| {
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
        let bound = match self.bindings.meraki_bound(node.id.as_uuid()).await {
            Ok(bound) => bound,
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
            self.bindings.url_config(node_uuid).await.unwrap_or_else(|e| {
                tracing::warn!(node = %node.id, error = %e, "url-check load failed; treating as non-URL node");
                None
            })
        } else {
            None
        };
        // Skipped entirely once URL has already won — `resolve` would discard it.
        let dns = if url.is_none() && hint_admits(hints.dns, node_uuid) {
            self.bindings.dns_config(node_uuid).await.unwrap_or_else(|e| {
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
                let auth = resolve_http_auth(self.creds.as_ref(), node, cfg.credential).await;
                monitor.with_http_auth(auth)
            } else {
                monitor
            };
            return assemble_node_jobs(node, None, &[], Some(monitor), interval_secs, neighbors);
        }
        let auth =
            resolve_snmp_auth(self.creds.as_ref(), node, self.env_community.as_deref()).await;
        // Only resolve the collection set when SNMP is configured (ICMP needs none).
        let items = if auth.is_some() {
            resolve_node_collection(self.collection.as_ref(), node).await
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
        publish(self.bus.as_ref(), pool, job, kind, node).await
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
            if publish(self.bus.as_ref(), pool, job, kind, node.id).await {
                published += 1;
            }
        }
        metrics::counter!("yagra_manual_poll_jobs_total").increment(published);
        usize::try_from(published).unwrap_or(usize::MAX)
    }
}

/// Resolve a node's effective collection set, defaulting to the built-in catalog when nothing is
/// configured (or the lookup fails) so polling always has a sensible default.
async fn resolve_node_collection(
    collection: &dyn CollectionSource,
    node: &Node,
) -> Vec<CollectionItem> {
    match collection
        .items_for_node(node.id.as_uuid(), node.profile.map(|p| p.0))
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
    creds: &dyn CredentialSource,
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
    creds: &dyn CredentialSource,
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
async fn publish(
    bus: &dyn SyncBus,
    pool: &str,
    mut job: PollJob,
    kind: &str,
    node: NodeId,
) -> bool {
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

#[cfg(test)]
mod tests {
    use super::super::testkit::{
        dns_cfg, node, url_cfg, FakeAdjacency, FakeBindings, FakeCollection, FakeCreds, Harness,
        V3_DOC,
    };
    use super::*;
    use crate::secrets::{KIND_HTTP_AUTH, KIND_SNMP_V3};
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::atomic::Ordering;
    use yagra_bus::CheckSpec;
    use yagra_common::{CollectionKind, CredentialId, MetricKind, ScopeLevel};

    /// The job-kind labels of a build, in order — the same helper `assemble.rs` uses, because the
    /// labels are what an operator sees in the dispatch span and the failed-publish warning.
    fn kinds(jobs: &[(PollJob, &'static str)]) -> Vec<&'static str> {
        jobs.iter().map(|(_, k)| *k).collect()
    }

    /// A set holding exactly `ids`, for a preloaded [`MonitorHints`] field.
    fn set(ids: &[Uuid]) -> HashSet<Uuid> {
        ids.iter().copied().collect()
    }

    // ── The hint gate: what is NOT looked up ─────────────────────────────────────────────────

    /// 🎯 **An empty hint set skips the query itself, not just the decision made from it.**
    ///
    /// The gate has to wrap the lookup: passing a hint while leaving the query where it was would
    /// change nothing, which is exactly the state this file shipped in — a 50k fleet paid 50k
    /// `url_checks` queries a round to find a handful of URL monitors. Nothing guarded the fix.
    ///
    /// The accept half is in the same test on purpose: "the lookup did not happen" is satisfied by
    /// a dispatcher that looks nothing up ever
    /// (`rejection-only-tests-pass-when-everything-rejects`).
    #[tokio::test]
    async fn an_empty_url_hint_skips_the_lookup_and_a_matching_one_performs_it() {
        let n = node("edge-1");
        let id = n.id.as_uuid();
        let policy = AdjacencyPolicy::default();

        let h = Harness::builder()
            .bindings(FakeBindings::new().with_url(id, url_cfg("https://a/health", None)))
            .build();
        let empty = HashSet::new();
        let jobs = h
            .dispatcher
            .build_scheduled_jobs_hinted(
                &n,
                60,
                MonitorHints {
                    url: Some(&empty),
                    dns: Some(&empty),
                },
                &policy,
            )
            .await;
        assert_eq!(
            h.bindings.calls.url_config.load(Ordering::Relaxed),
            0,
            "an empty hint must skip the url_checks query entirely, not just ignore its answer"
        );
        assert_eq!(
            kinds(&jobs),
            vec!["icmp"],
            "with the lookup skipped the node is an ordinary device"
        );

        let hinted = set(&[id]);
        let jobs = h
            .dispatcher
            .build_scheduled_jobs_hinted(
                &n,
                60,
                MonitorHints {
                    url: Some(&hinted),
                    dns: Some(&empty),
                },
                &policy,
            )
            .await;
        assert_eq!(h.bindings.calls.url_config.load(Ordering::Relaxed), 1);
        assert_eq!(kinds(&jobs), vec!["http"], "a hinted node is looked up");
    }

    /// The on-demand single-node path preloads nothing, so `None` must mean "query directly".
    ///
    /// `None` and `Some(∅)` are different facts — "not preloaded" against "there are none of this
    /// kind" — and conflating them costs either every monitor or every round trip.
    #[tokio::test]
    async fn the_default_hints_query_both_side_tables() {
        let n = node("edge-2");
        let h = Harness::builder().build();
        h.dispatcher
            .build_scheduled_jobs_hinted(
                &n,
                60,
                MonitorHints::default(),
                &AdjacencyPolicy::default(),
            )
            .await;
        assert_eq!(h.bindings.calls.url_config.load(Ordering::Relaxed), 1);
        assert_eq!(h.bindings.calls.dns_config.load(Ordering::Relaxed), 1);
    }

    /// 🎯 Once a URL monitor has won, the DNS row is not read: `SpecialMonitor::resolve` would
    /// discard it, so the query is pure cost.
    #[tokio::test]
    async fn a_url_monitor_short_circuits_the_dns_lookup() {
        let n = node("both-rows");
        let id = n.id.as_uuid();
        let h = Harness::builder()
            .bindings(
                FakeBindings::new()
                    .with_url(id, url_cfg("https://a/health", None))
                    .with_dns(id, dns_cfg("example.net")),
            )
            .build();
        let jobs = h
            .dispatcher
            .build_scheduled_jobs_hinted(
                &n,
                60,
                MonitorHints::default(),
                &AdjacencyPolicy::default(),
            )
            .await;
        assert_eq!(
            kinds(&jobs),
            vec!["http"],
            "URL outranks DNS (NodeKind::resolve)"
        );
        assert_eq!(
            h.bindings.calls.dns_config.load(Ordering::Relaxed),
            0,
            "the DNS row cannot change the answer once URL has won, so it must not be read"
        );
    }

    /// 🎯 **The two hints are not interchangeable.** Each gates its own table; wiring the URL set
    /// to the DNS gate would type-check, and the only symptom would be DNS monitors silently never
    /// polled on a fleet that has any URL monitors.
    #[tokio::test]
    async fn each_hint_gates_only_its_own_side_table() {
        let n = node("dns-monitor");
        let id = n.id.as_uuid();
        let empty = HashSet::new();
        let hinted = set(&[id]);
        let policy = AdjacencyPolicy::default();
        let h = Harness::builder()
            .bindings(FakeBindings::new().with_dns(id, dns_cfg("example.net")))
            .build();

        // Named by the URL hint, absent from the DNS one: a correct wiring reads neither table.
        let jobs = h
            .dispatcher
            .build_scheduled_jobs_hinted(
                &n,
                60,
                MonitorHints {
                    url: Some(&hinted),
                    dns: Some(&empty),
                },
                &policy,
            )
            .await;
        assert_eq!(
            kinds(&jobs),
            vec!["icmp"],
            "the DNS hint says there are no DNS monitors, so no DNS job may be built"
        );
        assert_eq!(h.bindings.calls.dns_config.load(Ordering::Relaxed), 0);

        // Swap the sets over and the same node is polled as the DNS monitor it is.
        let jobs = h
            .dispatcher
            .build_scheduled_jobs_hinted(
                &n,
                60,
                MonitorHints {
                    url: Some(&empty),
                    dns: Some(&hinted),
                },
                &policy,
            )
            .await;
        assert_eq!(kinds(&jobs), vec!["dns"]);
    }

    // ── Meraki ───────────────────────────────────────────────────────────────────────────────

    /// 🎯 **The sweep never asks whether a node is Meraki.** It already excluded those nodes by
    /// preloading their ids, so a per-node lookup here is one JOIN per node per round — 50k of
    /// them on the fleet this was measured against. The on-demand path keeps the guard.
    #[tokio::test]
    async fn the_scheduler_path_never_asks_whether_a_node_is_meraki() {
        let n = node("edge-3");
        let h = Harness::builder().build();

        h.dispatcher
            .build_scheduled_jobs_hinted(
                &n,
                60,
                MonitorHints::default(),
                &AdjacencyPolicy::default(),
            )
            .await;
        assert_eq!(
            h.bindings.calls.meraki_bound.load(Ordering::Relaxed),
            0,
            "the sweep excludes Meraki nodes by id set; re-querying is the cost this split removed"
        );

        h.dispatcher.build_node_jobs(&n, 60).await;
        assert_eq!(
            h.bindings.calls.meraki_bound.load(Ordering::Relaxed),
            1,
            "the on-demand path has no preloaded set, so it must still ask"
        );
    }

    /// A Meraki-bound node is collected by the org collector, so "poll now" emits nothing for it —
    /// not even ICMP.
    #[tokio::test]
    async fn a_meraki_bound_node_gets_no_per_node_job() {
        let n = node("meraki-ap");
        let h = Harness::builder()
            .bindings(FakeBindings::new().with_meraki(n.id.as_uuid()))
            .build();
        assert!(h.dispatcher.build_node_jobs(&n, 60).await.is_empty());
    }

    /// A failed binding read is treated as "not Meraki", so a database blip does not stop an
    /// ordinary node being polled.
    ///
    /// ⚠️ The trade is stated rather than hidden: the same blip double-polls a genuinely Meraki
    /// node for one action. Costing a redundant poll is the better half of that choice, and this
    /// test is where the choice is written down.
    #[tokio::test]
    async fn a_failed_meraki_lookup_keeps_polling_the_node() {
        let n = node("edge-4");
        let h = Harness::builder()
            .bindings(
                FakeBindings::new()
                    .failing_meraki()
                    .with_meraki(n.id.as_uuid()),
            )
            .build();
        assert_eq!(
            kinds(&h.dispatcher.build_node_jobs(&n, 60).await),
            vec!["icmp"]
        );
    }

    /// A failed side-table read degrades to "not that kind of monitor" rather than to no poll at
    /// all — the same reasoning as the Meraki path above.
    #[tokio::test]
    async fn a_failed_url_lookup_polls_the_node_as_an_ordinary_device() {
        let n = node("edge-5");
        let h = Harness::builder()
            .bindings(
                FakeBindings::new()
                    .failing_url()
                    .with_url(n.id.as_uuid(), url_cfg("https://a/health", None)),
            )
            .build();
        let jobs = h
            .dispatcher
            .build_scheduled_jobs_hinted(
                &n,
                60,
                MonitorHints::default(),
                &AdjacencyPolicy::default(),
            )
            .await;
        assert_eq!(kinds(&jobs), vec!["icmp"]);
    }

    // ── Credentials ──────────────────────────────────────────────────────────────────────────

    /// Whether a job carries a v3 (USM) or a v2c check, for the credential tests below.
    fn snmp_shape(jobs: &[(PollJob, &'static str)]) -> Vec<String> {
        jobs.iter()
            .filter_map(|(job, _)| match &job.check {
                CheckSpec::Snmp(c) => Some(format!("v2c:{}", c.community)),
                CheckSpec::SnmpV3(c) => Some(format!("v3:{}", c.auth.user)),
                _ => None,
            })
            .collect()
    }

    /// The credential's `kind` picks the protocol: `snmp_v3` is a USM document, anything else is
    /// treated as a v2c community (back-compat with credentials created before kinds meant
    /// anything).
    #[tokio::test]
    async fn the_credential_kind_picks_the_snmp_protocol() {
        let cred = Uuid::from_u128(0x5eed);
        let policy = AdjacencyPolicy::default();

        let mut v3_node = node("v3-device");
        v3_node.credential = Some(CredentialId::from(cred));
        let h = Harness::builder()
            .creds(FakeCreds::new().with(cred, KIND_SNMP_V3, V3_DOC))
            .build();
        let jobs = h
            .dispatcher
            .build_scheduled_jobs_hinted(&v3_node, 60, MonitorHints::default(), &policy)
            .await;
        assert_eq!(snmp_shape(&jobs), vec!["v3:monitor".to_owned()]);

        let mut v2c_node = node("v2c-device");
        v2c_node.credential = Some(CredentialId::from(cred));
        let h = Harness::builder()
            .creds(FakeCreds::new().with(cred, "snmp_v2c", b"s3cret-community"))
            .build();
        let jobs = h
            .dispatcher
            .build_scheduled_jobs_hinted(&v2c_node, 60, MonitorHints::default(), &policy)
            .await;
        assert_eq!(snmp_shape(&jobs), vec!["v2c:s3cret-community".to_owned()]);
    }

    /// A node with no bound credential falls back to the deployment-wide community, and a
    /// deployment with neither is polled by ICMP alone.
    ///
    /// 🎯 The second half also pins that **no collection set is resolved without SNMP auth** —
    /// ICMP needs none, and at fleet scale that lookup is the whole cost of a node.
    #[tokio::test]
    async fn without_a_bound_credential_the_env_community_is_the_fallback() {
        let n = node("unbound");
        let policy = AdjacencyPolicy::default();

        let h = Harness::builder().env_community("public").build();
        let jobs = h
            .dispatcher
            .build_scheduled_jobs_hinted(&n, 60, MonitorHints::default(), &policy)
            .await;
        assert_eq!(snmp_shape(&jobs), vec!["v2c:public".to_owned()]);
        assert!(
            h.collection.reads.load(Ordering::Relaxed) >= 1,
            "with SNMP configured the collection set has to be resolved"
        );

        let h = Harness::builder().build();
        let jobs = h
            .dispatcher
            .build_scheduled_jobs_hinted(&n, 60, MonitorHints::default(), &policy)
            .await;
        assert_eq!(kinds(&jobs), vec!["icmp"]);
        assert_eq!(
            h.collection.reads.load(Ordering::Relaxed),
            0,
            "ICMP needs no collection set, so resolving one is a round trip per node per sweep"
        );
    }

    /// [`PollDispatcher::snmp_configured_for`] is a **second implementation** of the question
    /// [`resolve_snmp_auth`] answers, so this runs both over the four ordinary combinations
    /// (credential bound or not × env community set or not) and demands they agree.
    ///
    /// ⚠️ **It cannot pin the whole rule, and the gap is named rather than hidden.** A bound
    /// credential that fails to open is the one input where the two are meant to disagree —
    /// `resolve_snmp_auth` gives up and this still says `true`. That case is asserted here as the
    /// intended difference (ADR-119 決定 3): showing an empty tab is recoverable, hiding a tab that
    /// holds rows is not. A **third** entrance into `resolve_snmp_auth` would drift past both.
    #[tokio::test]
    async fn snmp_configured_for_agrees_with_resolve_snmp_auth() {
        let cred = Uuid::from_u128(0xc0ffee);
        let bound = |name: &str| {
            let mut n = node(name);
            n.credential = Some(CredentialId::from(cred));
            n
        };
        let good = || FakeCreds::new().with(cred, "snmp_v2c", b"s3cret-community");

        for (label, n, env) in [
            ("no credential, no env", node("bare"), None),
            ("no credential, env set", node("bare"), Some("public")),
            ("credential, no env", bound("v2c"), None),
            ("credential, env set", bound("v2c"), Some("public")),
        ] {
            let mut b = Harness::builder().creds(good());
            if let Some(c) = env {
                b = b.env_community(c);
            }
            let h = b.build();
            let resolved =
                resolve_snmp_auth(h.creds.as_ref(), &n, h.dispatcher.env_community.as_deref())
                    .await
                    .is_some();
            assert_eq!(
                h.dispatcher.snmp_configured_for(&n),
                resolved,
                "{label}: the API answer and the scheduler answer disagree"
            );
        }

        // The named exception. Keep it asserted: if the two ever converge here, one of them
        // changed behaviour and the doc on `snmp_configured_for` stopped being true.
        let n = bound("v3-broken");
        let h = Harness::builder()
            .creds(FakeCreds::new().with(cred, KIND_SNMP_V3, b"{not-a-usm-document"))
            .build();
        assert!(
            resolve_snmp_auth(h.creds.as_ref(), &n, None)
                .await
                .is_none(),
            "an unopenable credential with no env community resolves to no auth"
        );
        assert!(
            h.dispatcher.snmp_configured_for(&n),
            "and `snmp_configured_for` deliberately still says yes — see its doc"
        );
    }
    /// 🚨 **Current behaviour, recorded rather than endorsed** (ADR-111 決定 6): a bound `snmp_v3`
    /// credential that does not parse falls through to the environment's **v2c** community, so a
    /// device that was configured for USM is then polled with a community string.
    ///
    /// The warning is emitted with a static reason and the poll still happens. Whether that is
    /// right is a separate question from whether it is what the code does; this test asserts the
    /// second, and the ADR carries the first.
    #[tokio::test]
    async fn a_malformed_v3_credential_falls_through_to_the_env_community() {
        let cred = Uuid::from_u128(0xbad);
        let mut n = node("v3-broken");
        n.credential = Some(CredentialId::from(cred));
        let h = Harness::builder()
            .creds(FakeCreds::new().with(cred, KIND_SNMP_V3, b"{not-a-usm-document"))
            .env_community("public")
            .build();
        let jobs = h
            .dispatcher
            .build_scheduled_jobs_hinted(
                &n,
                60,
                MonitorHints::default(),
                &AdjacencyPolicy::default(),
            )
            .await;
        assert_eq!(snmp_shape(&jobs), vec!["v2c:public".to_owned()]);
    }

    /// A URL monitor's bound credential is resolved and inlined into the job (ADR-018: decrypted in
    /// core, never in the poller).
    ///
    /// 🎯 The pair asserts the other direction of the same claim — an *unbound* URL monitor, and a
    /// DNS monitor, open no credential at all.
    #[tokio::test]
    async fn only_a_bound_url_monitor_opens_a_credential() {
        let cred = Uuid::from_u128(0xa11e);
        let policy = AdjacencyPolicy::default();

        let n = node("url-authed");
        let h = Harness::builder()
            .bindings(
                FakeBindings::new()
                    .with_url(n.id.as_uuid(), url_cfg("https://a/health", Some(cred))),
            )
            .creds(FakeCreds::new().with(
                cred,
                KIND_HTTP_AUTH,
                br#"{"scheme":"bearer","token":"t0ken"}"#,
            ))
            .build();
        let jobs = h
            .dispatcher
            .build_scheduled_jobs_hinted(&n, 60, MonitorHints::default(), &policy)
            .await;
        assert_eq!(kinds(&jobs), vec!["http"]);
        assert_eq!(h.creds.opens.load(Ordering::Relaxed), 1);
        let auth = match &jobs[0].0.check {
            CheckSpec::Http(c) => c.auth.clone(),
            other => panic!("expected an HTTP check, got {other:?}"),
        };
        assert!(
            matches!(auth, Some(HttpAuth::Bearer { ref token }) if token == "t0ken"),
            "the resolved credential has to reach the job; the poller cannot decrypt one"
        );

        let unbound = node("url-open");
        let dns = node("dns-monitor");
        let h = Harness::builder()
            .bindings(
                FakeBindings::new()
                    .with_url(unbound.id.as_uuid(), url_cfg("https://a/health", None))
                    .with_dns(dns.id.as_uuid(), dns_cfg("example.net")),
            )
            .build();
        h.dispatcher
            .build_scheduled_jobs_hinted(&unbound, 60, MonitorHints::default(), &policy)
            .await;
        h.dispatcher
            .build_scheduled_jobs_hinted(&dns, 60, MonitorHints::default(), &policy)
            .await;
        assert_eq!(
            h.creds.opens.load(Ordering::Relaxed),
            0,
            "an unbound monitor and a DNS monitor pay nothing for credentials"
        );
    }

    // ── Collection set ───────────────────────────────────────────────────────────────────────

    /// An empty resolution and a **failed** read both fall back to the built-in catalogue.
    ///
    /// 🚨 The failure direction is the one that matters: an empty collection set is not an error
    /// anywhere downstream, so a node would simply stop collecting metrics with nothing raised.
    #[tokio::test]
    async fn an_empty_or_failed_collection_read_falls_back_to_the_builtin_catalogue() {
        let n = node("snmp-device");
        let policy = AdjacencyPolicy::default();

        let expected = builtin_scalar_metrics();
        for (label, collection) in [
            ("empty", FakeCollection::new()),
            ("failed", FakeCollection::new().failing()),
        ] {
            let h = Harness::builder()
                .collection(collection)
                .env_community("public")
                .build();
            let jobs = h
                .dispatcher
                .build_scheduled_jobs_hinted(&n, 60, MonitorHints::default(), &policy)
                .await;
            assert_eq!(
                scalar_metrics(&jobs),
                expected,
                "a {label} collection read must yield the built-in catalogue, never nothing"
            );
        }

        // The accept side: a configured set is used instead of the catalogue.
        let h = Harness::builder()
            .collection(FakeCollection::new().with(
                ScopeLevel::Node,
                CollectionItem {
                    metric_name: "only_this".to_owned(),
                    oid: "1.3.6.1.2.1.1.3.0".to_owned(),
                    kind: CollectionKind::Scalar,
                    metric_kind: MetricKind::Gauge,
                },
            ))
            .env_community("public")
            .build();
        let jobs = h
            .dispatcher
            .build_scheduled_jobs_hinted(&n, 60, MonitorHints::default(), &policy)
            .await;
        assert_eq!(scalar_metrics(&jobs), vec!["only_this".to_owned()]);
    }

    /// The scalar metric names a build asks for, sorted.
    fn scalar_metrics(jobs: &[(PollJob, &'static str)]) -> Vec<String> {
        let mut names: Vec<String> = jobs
            .iter()
            .filter_map(|(job, _)| match &job.check {
                CheckSpec::Snmp(c) => Some(c.columns.iter().map(|col| col.metric_name.clone())),
                _ => None,
            })
            .flatten()
            .collect();
        names.sort();
        names
    }

    /// The scalar metric names of the built-in catalogue, sorted — derived, never listed, so the
    /// test above stays true when the catalogue gains an entry.
    fn builtin_scalar_metrics() -> Vec<String> {
        let mut names: Vec<String> = yagra_common::builtin_catalog()
            .into_iter()
            .filter(|i| i.kind == CollectionKind::Scalar)
            .map(|i| i.metric_name)
            .collect();
        names.sort();
        names
    }

    // ── The sweep's id preloads ──────────────────────────────────────────────────────────────

    /// A failed preload yields an **empty** set, and the sweep reads that as "there are no monitors
    /// of this kind" — costing one sweep's URL and DNS jobs.
    ///
    /// ⚠️ That is the deliberate trade, not an oversight: the alternative (`None`, "not preloaded")
    /// would send the fleet back to one query per node per table for that round. This test is where
    /// the choice is written down, so changing it is a decision rather than a slip.
    #[tokio::test]
    async fn a_failed_id_preload_yields_an_empty_set_not_a_missing_hint() {
        let n = node("url-monitor");
        let h = Harness::builder()
            .bindings(
                FakeBindings::new()
                    .failing_id_preloads()
                    .with_url(n.id.as_uuid(), url_cfg("https://a/health", None)),
            )
            .build();
        assert!(h.dispatcher.url_node_ids().await.is_empty());
        assert!(h.dispatcher.dns_node_ids().await.is_empty());

        // The accept side: a healthy preload names the monitors it found.
        let h = Harness::builder()
            .bindings(
                FakeBindings::new().with_url(n.id.as_uuid(), url_cfg("https://a/health", None)),
            )
            .build();
        assert_eq!(h.dispatcher.url_node_ids().await, set(&[n.id.as_uuid()]));
    }

    // ── The route-probe plan cache ───────────────────────────────────────────────────────────

    /// Settings with routing on and every walk that would need SNMP off, so a policy read is about
    /// the plan and nothing else.
    fn routing_on() -> crate::neighbors::AdjacencySettings {
        crate::neighbors::AdjacencySettings::default()
    }

    /// Two nodes, each holding one host address, so the built plan asks both to probe the other.
    fn two_host_routes(a: NodeId, b: NodeId) -> FakeAdjacency {
        FakeAdjacency::new()
            .with_settings(routing_on())
            .with_host(a, IpAddr::V4(Ipv4Addr::new(10, 9, 0, 1)))
            .with_host(b, IpAddr::V4(Ipv4Addr::new(10, 9, 0, 2)))
    }

    /// Within the TTL the plan is reused, so a sweep costs one `node_l3` scan rather than one per
    /// node.
    #[tokio::test]
    async fn the_route_probe_plan_is_built_once_inside_its_ttl() {
        let (a, b) = (NodeId::new(), NodeId::new());
        let h = Harness::builder().adjacency(two_host_routes(a, b)).build();

        let first = h.dispatcher.adjacency_policy().await;
        let second = h.dispatcher.adjacency_policy().await;
        assert_eq!(first.routing_plan.prober_count(), 2);
        assert_eq!(second.routing_plan.prober_count(), 2);
        assert_eq!(
            h.adjacency.host_reads.load(Ordering::Relaxed),
            1,
            "a jsonb_array_elements scan per sweep is what the cache exists to avoid"
        );
        assert_eq!(
            h.adjacency.settings_reads.load(Ordering::Relaxed),
            2,
            "the settings themselves are cheap and are read every time"
        );
    }

    /// 🎯 **A failed refresh does not advance the cache clock**, so the next sweep retries.
    ///
    /// An empty plan issues no probes at all, and nothing downstream can tell that apart from
    /// "there are no point-to-point links" — so a transient database error would silently stop
    /// collecting them, permanently if the TTL had been advanced.
    ///
    /// ⚠️ **The other half of that invariant is out of reach and is not claimed here.** "A failure
    /// keeps the *previously built* plan" needs a success, then an expiry, then a failure — and
    /// the expiry needs a clock this cache does not take as an argument (ADR-111 決定 6). What is
    /// proved below is that the retry happens; what is not proved is what the retry would find.
    #[tokio::test]
    async fn a_failed_plan_refresh_does_not_advance_the_cache_clock() {
        let (a, b) = (NodeId::new(), NodeId::new());
        let h = Harness::builder()
            .adjacency(two_host_routes(a, b).failing_host_read_on(1))
            .build();

        // The very first read fails: there is no previous plan, so an empty one is all there is —
        // and the TTL must not be advanced, or the deployment never probes again.
        let first = h.dispatcher.adjacency_policy().await;
        assert_eq!(first.routing_plan.prober_count(), 0);

        // The retry is what proves the clock did not move: a cached plan would answer without a
        // second read.
        let second = h.dispatcher.adjacency_policy().await;
        assert_eq!(second.routing_plan.prober_count(), 2);
        assert_eq!(h.adjacency.host_reads.load(Ordering::Relaxed), 2);
    }

    /// With routing switched off the plan is never read at all — the policy is settings only.
    #[tokio::test]
    async fn a_deployment_with_routing_off_never_scans_the_addressing() {
        let mut settings = routing_on();
        settings.routing_enabled = false;
        let h = Harness::builder()
            .adjacency(FakeAdjacency::new().with_settings(settings))
            .build();
        let policy = h.dispatcher.adjacency_policy().await;
        assert!(!policy.routing_enabled);
        assert_eq!(h.adjacency.host_reads.load(Ordering::Relaxed), 0);
    }

    // ── Publishing ───────────────────────────────────────────────────────────────────────────

    /// "Poll now" publishes every job it built, to the **named pool's** subject.
    ///
    /// 🎯 The pool is the assertion that was unreachable before: the argument is a `&str` threaded
    /// through three calls, so routing a manual poll to the wrong pool is a silent no-op for the
    /// operator who pressed the button.
    #[tokio::test]
    async fn poll_now_publishes_every_job_to_the_nodes_own_pool() {
        let n = node("edge-6");
        let mut h = Harness::builder().env_community("public").build();
        let published = h.dispatcher.poll_now(&n, "tokyo").await;
        assert!(published >= 2, "an SNMP node emits ICMP plus SNMP work");

        let seen = h.published();
        assert_eq!(
            seen.len(),
            published,
            "the count is what actually reached the bus"
        );
        assert!(
            seen.iter().all(|(pool, _)| pool == "tokyo"),
            "a job published to the wrong pool is never picked up, and nothing reports it"
        );
        assert!(seen.iter().all(|(_, job)| job.node_id == n.id));
    }

    /// A URL monitor's manual poll is a single HTTP job — the single-purpose kinds replace ICMP
    /// rather than adding to it.
    #[tokio::test]
    async fn poll_now_on_a_url_monitor_publishes_one_http_job() {
        let n = node("url-monitor");
        let mut h = Harness::builder()
            .bindings(
                FakeBindings::new().with_url(n.id.as_uuid(), url_cfg("https://a/health", None)),
            )
            .build();
        assert_eq!(h.dispatcher.poll_now(&n, "default").await, 1);
        let seen = h.published();
        assert!(matches!(seen[0].1.check, CheckSpec::Http(_)));
    }

    /// The working-set form of a build is the same jobs as specs, one for one — the per-dispatch
    /// `job_id` is dropped because the poller stamps a fresh one each time it schedules locally.
    #[tokio::test]
    async fn node_specs_mirror_the_jobs_one_for_one() {
        let n = node("edge-7");
        let h = Harness::builder().env_community("public").build();
        let jobs = h
            .dispatcher
            .build_scheduled_jobs_hinted(
                &n,
                60,
                MonitorHints::default(),
                &AdjacencyPolicy::default(),
            )
            .await;
        let specs = h
            .dispatcher
            .build_node_specs(&n, 60, MonitorHints::default(), &AdjacencyPolicy::default())
            .await;
        assert_eq!(specs.len(), jobs.len());
        assert!(
            !specs.is_empty(),
            "an empty build would satisfy the equality above"
        );
        for (spec, (job, _)) in specs.iter().zip(&jobs) {
            assert_eq!(spec.check, job.check);
            assert_eq!(spec.interval_secs, job.interval_secs);
        }
    }

    /// A DNS row that cannot be read degrades to "not a DNS monitor", the same way the URL and
    /// Meraki reads do. All three failures are answered the same way on purpose: a database blip
    /// must cost a node the *right kind* of poll, never the poll itself.
    #[tokio::test]
    async fn a_failed_dns_lookup_polls_the_node_as_an_ordinary_device() {
        let n = node("edge-8");
        let h = Harness::builder()
            .bindings(
                FakeBindings::new()
                    .failing_dns()
                    .with_dns(n.id.as_uuid(), dns_cfg("example.net")),
            )
            .build();
        let jobs = h
            .dispatcher
            .build_scheduled_jobs_hinted(
                &n,
                60,
                MonitorHints::default(),
                &AdjacencyPolicy::default(),
            )
            .await;
        assert_eq!(kinds(&jobs), vec!["icmp"]);
    }

    /// A credential that cannot be *decrypted* takes the same route as one that does not parse:
    /// warn, then fall back to the environment community.
    ///
    /// 🚨 Recorded, not endorsed — see `a_malformed_v3_credential_falls_through_to_the_env_community`
    /// and ADR-111 決定 6. This is the second door into that behaviour, and it is the one a KEK
    /// problem opens: every bound credential in the deployment fails at once, and the whole fleet
    /// silently starts speaking v2c with whatever `YAGRA_SNMP_COMMUNITY` happens to hold.
    #[tokio::test]
    async fn a_credential_that_cannot_be_opened_falls_through_to_the_env_community() {
        let cred = Uuid::from_u128(0xdead);
        let mut n = node("sealed-shut");
        n.credential = Some(CredentialId::from(cred));
        let h = Harness::builder()
            .creds(FakeCreds::new().failing().with(cred, KIND_SNMP_V3, V3_DOC))
            .env_community("public")
            .build();
        let jobs = h
            .dispatcher
            .build_scheduled_jobs_hinted(
                &n,
                60,
                MonitorHints::default(),
                &AdjacencyPolicy::default(),
            )
            .await;
        assert_eq!(snmp_shape(&jobs), vec!["v2c:public".to_owned()]);
        assert_eq!(h.creds.opens.load(Ordering::Relaxed), 1);
    }

    /// "Poll now" stamps the dispatcher's own default interval, not the node's resolved one.
    ///
    /// The poller ignores the value — cadence is owned by the periodic scheduler — but it is on the
    /// wire and in the working-set spec, so a manual poll that stamped something else would show up
    /// as a cadence change to anyone reading a job.
    #[tokio::test]
    async fn poll_now_stamps_the_dispatchers_default_interval() {
        let n = node("edge-9");
        let mut h = Harness::builder()
            .interval_secs(300)
            .env_community("public")
            .build();
        h.dispatcher.poll_now(&n, "default").await;
        let seen = h.published();
        let icmp = seen
            .iter()
            .find(|(_, job)| matches!(job.check, CheckSpec::Icmp(_)))
            .expect("a manual poll always includes ICMP");
        assert_eq!(icmp.1.interval_secs, 300);
    }
}
