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
use yagra_bus::{Bus, IcmpCheck, NatsBus, PollResult, SnmpCheck};

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

    // Result consumer: bus → TSDB + alert engine (+ history + notifications).
    {
        let store = store.clone();
        let alerts = alerts.clone();
        let notifier = notifier.clone();
        let history = history.clone();
        let results = Box::pin(bus.subscribe_results().await?);
        tokio::spawn(consume_results(results, store, alerts, notifier, history));
    }

    // Credential store, shared by the API admin and the scheduler's SNMP resolution.
    let creds = Arc::new(CredentialStore::from_env(repo.pool()));

    // SNMP v2c (ADR-021): community is resolved per node from its bound credential; an env
    // community is a fallback for nodes without one. OIDs default to sysUpTime.
    let env_community = std::env::var("YAGRA_SNMP_COMMUNITY")
        .ok()
        .filter(|c| !c.is_empty());
    let snmp_oids = std::env::var("YAGRA_SNMP_OIDS")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_owned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| vec!["1.3.6.1.2.1.1.3.0".to_owned()]);

    // Scheduler: inventory → jobs on the bus, jittered across the interval.
    {
        let repo = repo.clone();
        let bus = bus.clone();
        let creds = creds.clone();
        let env_community = env_community.clone();
        let snmp_oids = snmp_oids.clone();
        tokio::spawn(run_scheduler(
            repo,
            bus,
            cfg.poll_interval_secs,
            creds,
            env_community,
            snmp_oids,
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
    });
    let state = ApiState {
        store: sink,
        nodes: Arc::new(StaticNodeList::demo()),
        alerts: Arc::new(AlertManager::new()),
        admin: None,
        sessions: Arc::new(SessionStore::new()),
        history: None,
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
) where
    S: Stream<Item = PollResult> + Unpin,
{
    use crate::alerts::NotifyAction;
    while let Some(result) = results.next().await {
        metrics::counter!("yagra_poll_results_total").increment(1);
        store.write(&result).await;
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
    snmp_oids: Vec<String>,
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
                    let community =
                        resolve_community(&creds, &node, env_community.as_deref()).await;
                    if let Some(community) = community {
                        if !snmp_oids.is_empty() {
                            let bus = bus.clone();
                            let node = node.clone();
                            let check = SnmpCheck {
                                community,
                                oids: snmp_oids.clone(),
                                timeout_ms: 2000,
                            };
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

/// Build the alert engine's config snapshot: all thresholds plus per-node metadata
/// (profile + group tag-values) for scope resolution. Failures degrade to empty rather
/// than crashing the refresh loop.
async fn load_alert_config(repo: &NodeRepo, thresholds: &ThresholdStore) -> AlertConfig {
    let rules = thresholds.list_all().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "failed to load thresholds");
        Vec::new()
    });
    let nodes = repo.list_nodes().await.unwrap_or_default();
    let mut meta = HashMap::new();
    for node in nodes {
        meta.insert(
            node.id,
            NodeMeta {
                profile: node.profile.map(|p| p.to_string()),
                groups: node.tags.into_values().collect(),
            },
        );
    }
    AlertConfig::new(rules, meta)
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
