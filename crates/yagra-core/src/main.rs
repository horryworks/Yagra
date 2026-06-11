//! Yagra-core — Core/API.
//!
//! Orchestration, scheduling, and the northbound REST API (`/api/v1`, ADR-008/019) the
//! WebUI and external automation consume. Dispatches polling jobs to workers
//! (Yagra-poller) through the bus (Yagra-bus, ADR-003) and owns metadata-store
//! interaction (PostgreSQL) and the TSDB write/read path (VictoriaMetrics).
//!
//! Two run modes (see [`config`]): **live** when the store/bus URLs are present (real
//! PostgreSQL + NATS + VictoriaMetrics), else an in-memory **skeleton** so a bare
//! `cargo run` still serves the API.

mod alerts;
mod api;
mod auth;
mod collection;
mod config;
mod history;
mod repo;
mod scheduler;
mod secrets;
mod sink;
mod store;
mod thresholds;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use alerts::{AlertConfig, AlertManager, NodeMeta, Notifier};
use api::{AdminState, ApiState};
use auth::{SessionStore, UserStore};
use axum::routing::get;
use collection::CollectionRepo;
use config::Config;
use futures::stream::{Stream, StreamExt};
use history::AlertHistoryStore;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use repo::{NodeListing, NodeRepo, StaticNodeList};
use secrets::CredentialStore;
use sink::InMemorySink;
use store::{MetricStore, VmStore};
use thresholds::ThresholdStore;
use uuid::Uuid;
use yagra_bus::{Bus, IcmpCheck, NatsBus, PollResult};
use yagra_topology::Topology;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // Self-observability (ADR / monitoring-conventions): install the Prometheus recorder
    // up front so `metrics::counter!` calls anywhere are captured; `/metrics` renders it.
    let metrics = PrometheusBuilder::new()
        .install_recorder()
        .map_err(|e| anyhow::anyhow!("install Prometheus recorder: {e}"))?;

    match Config::from_env() {
        Some(cfg) => run_live(cfg, metrics).await,
        None => run_skeleton(metrics).await,
    }
}

/// Live mode: PostgreSQL + NATS + VictoriaMetrics, real ICMP polling end to end.
async fn run_live(cfg: Config, metrics: PrometheusHandle) -> anyhow::Result<()> {
    tracing::info!(
        interval = cfg.poll_interval_secs,
        "Yagra-core starting (live mode)"
    );

    // Metadata store: connect, migrate, seed demo inventory if empty.
    let repo = Arc::new(NodeRepo::connect(&cfg.database_url).await?);
    repo.migrate().await?;
    repo.seed_demo_nodes_if_empty().await?;

    // TSDB + bus.
    let store: Arc<dyn MetricStore> = Arc::new(VmStore::new(cfg.tsdb_url.clone()));
    let bus = Arc::new(connect_bus(&cfg.bus_url).await?);

    // Alert engine + optional notifier (Webhook/email, ADR-015) + history persistence.
    let alerts = Arc::new(AlertManager::new());
    let notifier = Notifier::from_env().map(Arc::new);
    let history = Arc::new(AlertHistoryStore::new(repo.pool()));

    // Result consumer: bus → TSDB + alert engine (+ history + notifications + interface
    // inventory upsert).
    {
        let store = store.clone();
        let alerts = alerts.clone();
        let notifier = notifier.clone();
        let history = history.clone();
        let repo = repo.clone();
        let results = Box::pin(bus.subscribe_results().await?);
        tokio::spawn(consume_results(
            results, store, alerts, notifier, history, repo,
        ));
    }

    // Credential store, shared by the API admin and the scheduler's SNMP resolution.
    let creds = Arc::new(CredentialStore::from_env(repo.pool()));

    // SNMP v2c (ADR-021): community is resolved per node from its bound credential; an env
    // community is a fallback for nodes without one. What to collect comes from the node's
    // resolved collection set (per-node/profile), falling back to the built-in catalog.
    let env_community = std::env::var("YAGRA_SNMP_COMMUNITY")
        .ok()
        .filter(|c| !c.is_empty());
    let collection = Arc::new(CollectionRepo::new(repo.pool()));

    // Scheduler: inventory → jobs on the bus, jittered across the interval.
    {
        let repo = repo.clone();
        let bus = bus.clone();
        let creds = creds.clone();
        let env_community = env_community.clone();
        let collection = collection.clone();
        tokio::spawn(run_scheduler(
            repo,
            bus,
            cfg.poll_interval_secs,
            creds,
            env_community,
            collection,
        ));
    }

    // Thresholds: snapshot into the alert engine now, then refresh periodically so edits
    // take effect without a restart.
    let thresholds = Arc::new(ThresholdStore::new(repo.pool()));
    alerts.set_config(load_alert_config(&repo, &thresholds).await);
    {
        let alerts = alerts.clone();
        let repo = repo.clone();
        let thresholds = thresholds.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                alerts.set_config(load_alert_config(&repo, &thresholds).await);
            }
        });
    }

    // Write side (inventory + encrypted credentials + users + thresholds), sharing the pool.
    let users = Arc::new(UserStore::new(repo.pool()));
    let admin_password =
        std::env::var("YAGRA_ADMIN_PASSWORD").unwrap_or_else(|_| "admin".to_owned());
    users.ensure_default_admin(&admin_password).await?;
    let admin = Some(Arc::new(AdminState {
        repo: repo.clone(),
        creds,
        users,
        thresholds,
        collection,
    }));
    let sessions = Arc::new(SessionStore::new());

    let nodes: Arc<dyn NodeListing> = repo;
    let state = ApiState {
        store,
        nodes,
        alerts,
        admin,
        sessions,
        history: Some(history),
        public_dashboard: cfg.public_dashboard,
    };
    serve(state, &cfg.api_addr, metrics).await
}

/// Skeleton mode: serve the API over an in-memory sink seeded with one demo reading.
async fn run_skeleton(metrics: PrometheusHandle) -> anyhow::Result<()> {
    tracing::warn!("store/bus URLs not set — running in in-memory skeleton mode (no real polling)");
    let sink = Arc::new(InMemorySink::default());
    // Demo seed so the walking-skeleton WebUI shows data before real polling is wired.
    sink.ingest(&PollResult {
        schema_version: 1,
        job_id: Uuid::nil(),
        node_id: yagra_common::NodeId::from(Uuid::nil()),
        at_unix_ms: 0,
        outcome: yagra_bus::CheckOutcome::Reachable,
        samples: vec![yagra_bus::Sample::gauge("icmp_rtt_ms", 8.0)],
        interfaces: Vec::new(),
    });
    let state = ApiState {
        store: sink,
        nodes: Arc::new(StaticNodeList::demo()),
        alerts: Arc::new(AlertManager::new()),
        admin: None,
        sessions: Arc::new(SessionStore::new()),
        history: None,
        // Skeleton has no user store (login returns 503), so reads must stay open or the
        // dev dashboard would be unreachable. Auth gating applies in live mode.
        public_dashboard: true,
    };
    serve(state, "0.0.0.0:8080", metrics).await
}

/// Drain poll results off the bus into the metric store and the alert engine. Returns
/// when the stream ends.
async fn consume_results<S>(
    mut results: S,
    store: Arc<dyn MetricStore>,
    alerts: Arc<AlertManager>,
    notifier: Option<Arc<Notifier>>,
    history: Arc<AlertHistoryStore>,
    repo: Arc<NodeRepo>,
) where
    S: Stream<Item = PollResult> + Unpin,
{
    use crate::alerts::NotifyAction;
    while let Some(result) = results.next().await {
        metrics::counter!("yagra_poll_results_total").increment(1);
        store.write(&result).await;
        // Upsert any interfaces discovered on this poll (table walks). Metadata only —
        // names/aliases live in PostgreSQL, joined to metrics at query time (ADR-011).
        // Best-effort: a failed upsert must not drop the metric write or alerting.
        for iface in &result.interfaces {
            let ifindex = i32::try_from(iface.ifindex.0).unwrap_or(i32::MAX);
            if let Err(e) = repo
                .upsert_interface(
                    result.node_id.as_uuid(),
                    ifindex,
                    iface.if_name.as_deref(),
                    iface.if_alias.as_deref(),
                    iface.if_speed,
                )
                .await
            {
                tracing::warn!(node = %result.node_id, error = %e, "failed to upsert interface");
            }
        }
        for action in alerts.observe(&result) {
            // Persist the lifecycle transition (best-effort).
            let recorded = match &action {
                NotifyAction::Fire(alert) => history.record(alert, false).await,
                NotifyAction::Resolve(alert) => history.record(alert, true).await,
            };
            if let Err(e) = recorded {
                tracing::warn!(error = %e, "failed to record alert history");
            }
            if let Some(notifier) = &notifier {
                notifier.handle(action).await;
            }
        }
    }
    tracing::warn!("result stream ended");
}

/// Periodically turn the inventory into ICMP jobs, spread across the interval with
/// per-node jitter so N nodes don't poll on the same tick (anti-stampede).
async fn run_scheduler(
    repo: Arc<NodeRepo>,
    bus: Arc<NatsBus>,
    interval_secs: u32,
    creds: Arc<CredentialStore>,
    env_community: Option<String>,
    collection: Arc<CollectionRepo>,
) {
    let interval = Duration::from_secs(u64::from(interval_secs));
    let window_ms = interval.as_millis().max(1) as u64;
    let jitter = || Duration::from_millis(rand::random::<u64>() % window_ms);
    loop {
        match repo.list_nodes().await {
            Ok(nodes) => {
                tracing::debug!(count = nodes.len(), "scheduling poll round");
                for node in nodes {
                    // Resolve the SNMP community: the node's bound credential (decrypted)
                    // wins; otherwise the env fallback. None ⇒ no SNMP for this node.
                    if let Some(community) =
                        resolve_community(&creds, &node, env_community.as_deref()).await
                    {
                        // Resolve the node's effective collection set (node overrides
                        // profile); fall back to the built-in catalog so an unconfigured
                        // node still gets the default sysUpTime + interface poll.
                        let items = resolve_node_collection(&collection, &node).await;
                        let (scalar, table) =
                            scheduler::build_snmp_checks(&community, &items, 2000);

                        if let Some(check) = scalar {
                            let bus = bus.clone();
                            let node = node.clone();
                            let delay = jitter();
                            tokio::spawn(async move {
                                tokio::time::sleep(delay).await;
                                let job = scheduler::build_snmp_job(
                                    &node,
                                    check,
                                    interval_secs,
                                    Uuid::new_v4(),
                                );
                                publish(&bus, job, "snmp", node.id).await;
                            });
                        }
                        if let Some(check) = table {
                            let bus = bus.clone();
                            let node = node.clone();
                            let delay = jitter();
                            tokio::spawn(async move {
                                tokio::time::sleep(delay).await;
                                let job = scheduler::build_snmp_table_job(
                                    &node,
                                    check,
                                    interval_secs,
                                    Uuid::new_v4(),
                                );
                                publish(&bus, job, "snmp_table", node.id).await;
                            });
                        }
                    }

                    // ICMP liveness job.
                    let bus = bus.clone();
                    let delay = jitter();
                    tokio::spawn(async move {
                        tokio::time::sleep(delay).await;
                        let job = scheduler::build_icmp_job(
                            &node,
                            IcmpCheck::default(),
                            interval_secs,
                            Uuid::new_v4(),
                        );
                        publish(&bus, job, "icmp", node.id).await;
                    });
                }
            }
            Err(e) => tracing::error!(error = %e, "scheduler: listing nodes failed"),
        }
        tokio::time::sleep(interval).await;
    }
}

/// Resolve a node's effective collection set, defaulting to the built-in catalog when
/// nothing is configured (or the lookup fails) so polling always has a sensible default.
async fn resolve_node_collection(
    collection: &CollectionRepo,
    node: &yagra_common::Node,
) -> Vec<yagra_common::CollectionItem> {
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

/// Resolve a node's SNMP community: prefer its bound credential (decrypted in core, never
/// the poller — ADR-018/020), else the env fallback.
async fn resolve_community(
    creds: &CredentialStore,
    node: &yagra_common::Node,
    env_community: Option<&str>,
) -> Option<String> {
    if let Some(cred) = node.credential {
        match creds.secret(cred.as_uuid()).await {
            Ok(Some(bytes)) => return String::from_utf8(bytes).ok(),
            Ok(None) => tracing::warn!(node = %node.id, "bound credential not found"),
            Err(e) => tracing::warn!(node = %node.id, error = %e, "credential decrypt failed"),
        }
    }
    env_community.map(ToOwned::to_owned)
}

/// Publish a job and bump the published-jobs counter, logging failures.
async fn publish(bus: &NatsBus, job: yagra_bus::PollJob, kind: &str, node: yagra_common::NodeId) {
    match bus.publish_job(job).await {
        Ok(()) => metrics::counter!("yagra_jobs_published_total").increment(1),
        Err(e) => tracing::warn!(error = %e, %kind, node = %node, "failed to publish job"),
    }
}

/// Bind and serve the northbound API plus the Prometheus `/metrics` endpoint.
async fn serve(state: ApiState, addr: &str, metrics: PrometheusHandle) -> anyhow::Result<()> {
    let app = api::router(state).route(
        "/metrics",
        get(move || {
            let handle = metrics.clone();
            async move { handle.render() }
        }),
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "Yagra-core API listening on /api/v1 (+ /metrics)");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Build the alert engine's config snapshot: all thresholds, per-node metadata (profile +
/// group tag-values) for scope resolution, and the dependency topology (parent edges) for
/// suppression / root-cause roll-up. Failures degrade to empty rather than crashing the
/// refresh loop.
async fn load_alert_config(repo: &NodeRepo, thresholds: &ThresholdStore) -> AlertConfig {
    let rules = thresholds.list_all().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to load thresholds");
        Vec::new()
    });
    let nodes = repo.list_nodes().await.unwrap_or_default();
    let mut meta = HashMap::new();
    let mut topology = Topology::new();
    for node in nodes {
        // Dependency edge child → parent feeds parent-down suppression (ADR-015).
        if let Some(parent) = node.parent {
            topology.add_dependency(node.id, parent);
        }
        meta.insert(
            node.id,
            NodeMeta {
                profile: node.profile.map(|p| p.to_string()),
                groups: node.tags.into_values().collect(),
            },
        );
    }
    AlertConfig::new(rules, meta).with_topology(topology)
}

/// Connect to NATS with retry so startup ordering doesn't matter.
async fn connect_bus(url: &str) -> anyhow::Result<NatsBus> {
    const MAX_ATTEMPTS: u32 = 30;
    let mut attempt = 0;
    loop {
        match NatsBus::connect(url).await {
            Ok(bus) => return Ok(bus),
            Err(e) if attempt < MAX_ATTEMPTS => {
                attempt += 1;
                tracing::warn!(error = %e, attempt, "NATS not ready; retrying in 2s");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(e) => anyhow::bail!("NATS connect failed after {MAX_ATTEMPTS} attempts: {e}"),
        }
    }
}
