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
mod audit;
mod auth;
mod classification;
mod collection;
mod config;
mod dashboard;
mod discovery;
mod groups;
mod history;
mod maintenance;
mod mib;
mod notifications;
mod repo;
mod scheduler;
mod secrets;
mod sink;
mod store;
mod thresholds;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use alerts::{ActiveMute, AlertConfig, AlertManager, NodeMeta, Notifier};
use api::{AdminState, ApiState};
use audit::AuditRepo;
use auth::{SessionStore, UserStore};
use axum::routing::get;
use collection::CollectionRepo;
use config::Config;
use dashboard::DashboardRepo;
use discovery::DiscoveryRunner;
use futures::stream::{Stream, StreamExt};
use history::AlertHistoryStore;
use maintenance::MaintenanceRepo;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use mib::MibRepo;
use notifications::NotificationRepo;
use repo::{NodeListing, NodeRepo, StaticNodeList};
use secrets::CredentialStore;
use sink::InMemorySink;
use store::{MetricStore, VmStore};
use thresholds::ThresholdStore;
use uuid::Uuid;
use yagra_bus::{NatsBus, PollResult};
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
    repo.seed_builtin_profiles().await?;
    let mib = Arc::new(MibRepo::new(repo.pool()));
    mib.seed_builtin().await?;

    // Device classification: the operator-editable rules that map a discovered device's
    // sysObjectID / sysDescr to a suggested profile. Loaded into an in-memory classifier that
    // discovery consults per candidate; reloaded on a refresh loop + after a rule edit.
    let classification = Arc::new(classification::ClassificationRepo::new(repo.pool()));
    let classifier = Arc::new(classification::Classifier::empty());
    if let Err(e) = classifier.reload(&classification).await {
        tracing::warn!(error = %e, "failed to load classification rules; discovery will use the generic fallback");
    }

    // TSDB + bus.
    let store: Arc<dyn MetricStore> = Arc::new(VmStore::new(cfg.tsdb_url.clone()));
    let bus = Arc::new(connect_bus(&cfg.bus_url).await?);

    // Alert engine + notifier (env default route + DB channels/rules, ADR-015) + history.
    let alerts = Arc::new(AlertManager::new());
    let notifier = Arc::new(Notifier::from_env());
    let notifications = Arc::new(NotificationRepo::from_env(repo.pool()));
    let history = Arc::new(AlertHistoryStore::new(repo.pool()));

    // Result consumer: bus → TSDB + alert engine (+ history + notifications + interface
    // inventory upsert).
    // Self-monitoring counters for the poll loop, shared by the consumer + scheduler and read by
    // the poller-health endpoint.
    let scheduler_stats = Arc::new(scheduler::SchedulerStats::default());
    {
        let store = store.clone();
        let alerts = alerts.clone();
        let notifier = notifier.clone();
        let history = history.clone();
        let repo = repo.clone();
        let stats = scheduler_stats.clone();
        let results = Box::pin(bus.subscribe_results().await?);
        tokio::spawn(consume_results(
            results, store, alerts, notifier, history, repo, stats,
        ));
    }

    // Discovery: a runner that publishes sweep jobs + a consumer that folds results back in,
    // classifying each found device into a suggested profile via the shared classifier.
    let discovery = Arc::new(DiscoveryRunner::new(bus.clone(), classifier.clone()));
    {
        let results = Box::pin(bus.subscribe_discovery_results().await?);
        tokio::spawn(discovery.clone().run_consumer(results));
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

    // Poll dispatcher: turns a node into bus jobs (ICMP + SNMP). Shared by the periodic scheduler
    // and the on-demand "poll now" API action so both build jobs the same way.
    let dispatcher = Arc::new(scheduler::PollDispatcher::new(
        bus.clone(),
        creds.clone(),
        collection.clone(),
        env_community.clone(),
        cfg.poll_interval_secs,
    ));

    // Scheduler: inventory → jobs on the bus, jittered across the interval.
    {
        let repo = repo.clone();
        let dispatcher = dispatcher.clone();
        let stats = scheduler_stats.clone();
        tokio::spawn(run_scheduler(repo, dispatcher, stats));
    }

    // Thresholds + maintenance windows: snapshot into the alert engine now, then refresh
    // periodically so edits (and window start/end boundaries) take effect without a restart.
    let thresholds = Arc::new(ThresholdStore::new(repo.pool()));
    let maintenance = Arc::new(MaintenanceRepo::new(repo.pool()));
    alerts.set_config(load_alert_config(&repo, &thresholds, &maintenance).await);
    {
        let alerts = alerts.clone();
        let repo = repo.clone();
        let thresholds = thresholds.clone();
        let maintenance = maintenance.clone();
        let classifier = classifier.clone();
        let classification = classification.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                alerts.set_config(load_alert_config(&repo, &thresholds, &maintenance).await);
                // Pick up classification-rule edits without a restart (also reloaded inline by
                // the rule-edit handlers; this catches any drift / multi-instance future).
                if let Err(e) = classifier.reload(&classification).await {
                    tracing::warn!(error = %e, "failed to refresh classification rules");
                }
            }
        });
    }

    // Fleet health timeline: snapshot the node-state counts every few minutes into PostgreSQL so
    // the dashboard can chart "degrading vs recovering" over time, and prune old snapshots +
    // alert history past the retention window (the only place these tables are trimmed).
    {
        let repo = repo.clone();
        let alerts = alerts.clone();
        let history = history.clone();
        tokio::spawn(async move {
            const SNAPSHOT_SECS: u64 = 300;
            const RETENTION_SECS: i64 = 90 * 86_400;
            loop {
                tokio::time::sleep(Duration::from_secs(SNAPSHOT_SECS)).await;
                let states = alerts.node_states();
                let mut counts: HashMap<String, i64> = HashMap::new();
                for s in states.values() {
                    *counts.entry(s.as_str().to_owned()).or_insert(0) += 1;
                }
                let snapshot: Vec<(String, i64)> = counts.into_iter().collect();
                if let Err(e) = repo.insert_state_snapshot(&snapshot).await {
                    tracing::warn!(error = %e, "node-state snapshot failed");
                }
                if let Err(e) = repo.prune_state_snapshots(RETENTION_SECS).await {
                    tracing::warn!(error = %e, "prune state snapshots failed");
                }
                if let Err(e) = history.prune_old(RETENTION_SECS).await {
                    tracing::warn!(error = %e, "prune alert history failed");
                }
            }
        });
    }

    // Notification routing + mutes: load the DB channels/rules into the notifier now, then
    // refresh periodically so edits take effect without a restart (env channels stay
    // always-on; expired mutes drop out on refresh).
    load_routing(&notifier, &notifications).await;
    load_mutes(&notifier, &maintenance).await;
    {
        let notifier = notifier.clone();
        let notifications = notifications.clone();
        let maintenance = maintenance.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                load_routing(&notifier, &notifications).await;
                load_mutes(&notifier, &maintenance).await;
            }
        });
    }

    // Write side (inventory + encrypted credentials + users + thresholds), sharing the pool.
    let users = Arc::new(UserStore::new(repo.pool()));
    // Bootstrap admin: use YAGRA_ADMIN_PASSWORD when supplied, otherwise generate a random
    // one-time password — never a well-known default like "admin" (security.md).
    let provided_admin_password = std::env::var("YAGRA_ADMIN_PASSWORD")
        .ok()
        .filter(|p| !p.trim().is_empty());
    let admin_password = provided_admin_password
        .clone()
        .unwrap_or_else(auth::generate_bootstrap_password);
    if users.ensure_default_admin(&admin_password).await? {
        if provided_admin_password.is_some() {
            tracing::warn!(
                "SECURITY: seeded the initial 'admin' account from YAGRA_ADMIN_PASSWORD — \
                 change it after first login"
            );
        } else {
            // One-time disclosure of the generated bootstrap password so the operator can log in;
            // it is not stored in plaintext and will not be shown again.
            tracing::warn!(
                admin_bootstrap_password = %admin_password,
                "SECURITY: no YAGRA_ADMIN_PASSWORD set — generated a one-time bootstrap password \
                 for the 'admin' account (shown once above). Log in and change it immediately."
            );
        }
    }
    let admin = Some(Arc::new(AdminState {
        repo: repo.clone(),
        creds,
        users,
        thresholds,
        collection,
        notifications,
        mib,
        discovery,
        maintenance,
        classification,
        classifier,
        groups: Arc::new(groups::GroupRepo::new(repo.pool())),
        audit: Arc::new(AuditRepo::new(repo.pool())),
        dashboards: Arc::new(DashboardRepo::new(repo.pool())),
        scheduler_stats: scheduler_stats.clone(),
        poll: dispatcher,
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
        sys_descr: None,
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
    notifier: Arc<Notifier>,
    history: Arc<AlertHistoryStore>,
    repo: Arc<NodeRepo>,
    stats: Arc<scheduler::SchedulerStats>,
) where
    S: Stream<Item = PollResult> + Unpin,
{
    use crate::alerts::NotifyAction;
    while let Some(result) = results.next().await {
        metrics::counter!("yagra_poll_results_total").increment(1);
        stats.record_result();
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
        // Identity probe: if the poll fetched sysDescr, classify it and fill the node's blank
        // vendor/model. `fill_node_identity` uses COALESCE so a manually-set value is never
        // clobbered; we only classify when something useful was extracted. Best-effort.
        if let Some(descr) = result.sys_descr.as_deref() {
            let id = yagra_discovery::identify(descr);
            if id.vendor.is_some() || id.model.is_some() {
                match repo
                    .fill_node_identity(
                        result.node_id.as_uuid(),
                        id.vendor.as_deref(),
                        id.model.as_deref(),
                    )
                    .await
                {
                    Ok(true) => tracing::info!(
                        node = %result.node_id, vendor = ?id.vendor, model = ?id.model,
                        "classified node maker/model from sysDescr"
                    ),
                    Ok(false) => {}
                    Err(e) => {
                        tracing::warn!(node = %result.node_id, error = %e, "failed to fill node identity");
                    }
                }
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
            notifier.handle(action).await;
        }
    }
    tracing::warn!("result stream ended");
}

/// Periodically turn the inventory into poll jobs (ICMP + SNMP), spread across the interval
/// with per-job jitter so N nodes don't poll on the same tick (anti-stampede). Job-building is
/// delegated to the shared [`PollDispatcher`] so the periodic and on-demand paths agree.
async fn run_scheduler(
    repo: Arc<NodeRepo>,
    dispatcher: Arc<scheduler::PollDispatcher>,
    stats: Arc<scheduler::SchedulerStats>,
) {
    let interval_secs = dispatcher.interval_secs();
    let interval = Duration::from_secs(u64::from(interval_secs));
    // Clamp to [1, u64::MAX] before narrowing u128→u64 so an extreme interval can't wrap to a
    // tiny jitter window (which would defeat the anti-stampede spread).
    let window_ms = interval.as_millis().clamp(1, u128::from(u64::MAX)) as u64;
    let jitter = || Duration::from_millis(rand::random::<u64>() % window_ms);
    loop {
        match repo.list_nodes().await {
            Ok(nodes) => {
                tracing::debug!(count = nodes.len(), "scheduling poll round");
                let mut jobs_round: u64 = 0;
                for node in nodes {
                    // Resolve auth + collection and build the node's jobs up front; jitter only
                    // the publish so polls spread across the window without a stampede.
                    for (job, kind) in dispatcher.build_node_jobs(&node).await {
                        jobs_round += 1;
                        let dispatcher = dispatcher.clone();
                        let node_id = node.id;
                        let delay = jitter();
                        tokio::spawn(async move {
                            tokio::time::sleep(delay).await;
                            dispatcher.publish_job(job, kind, node_id).await;
                        });
                    }
                }
                stats.record_sweep(jobs_round);
            }
            Err(e) => tracing::error!(error = %e, "scheduler: listing nodes failed"),
        }
        tokio::time::sleep(interval).await;
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
/// group tag-values) for scope resolution, the dependency topology (parent edges) for
/// suppression / root-cause roll-up, and the nodes currently inside an active maintenance
/// window. Failures degrade to empty rather than crashing the refresh loop.
async fn load_alert_config(
    repo: &NodeRepo,
    thresholds: &ThresholdStore,
    maintenance: &MaintenanceRepo,
) -> AlertConfig {
    let rules = thresholds.list_all().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to load thresholds");
        Vec::new()
    });
    let scopes = maintenance.active_scopes().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to load maintenance windows");
        Vec::new()
    });
    let nodes = repo.list_nodes().await.unwrap_or_default();
    let in_maintenance = maintenance::nodes_in_maintenance(&scopes, &nodes);
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
    AlertConfig::new(rules, meta)
        .with_topology(topology)
        .with_maintenance(in_maintenance)
}

/// Load the unexpired mutes into the notifier (check ids recomputed from names here).
/// Failures degrade to the existing snapshot (warn) rather than dropping mutes.
async fn load_mutes(notifier: &Notifier, maintenance: &MaintenanceRepo) {
    match maintenance.list_mutes().await {
        Ok(mutes) => {
            let active: Vec<ActiveMute> = mutes
                .iter()
                .map(|m| ActiveMute::new(m.node_id, m.check_name.as_deref()))
                .collect();
            notifier.set_mutes(active).await;
        }
        Err(e) => tracing::warn!(error = %e, "failed to load mutes"),
    }
}

/// Load the DB notification channels + routing rules into the notifier. Failures degrade to
/// the existing snapshot (warn) rather than dropping routing.
async fn load_routing(notifier: &Notifier, notifications: &NotificationRepo) {
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
