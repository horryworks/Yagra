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
mod config;
mod repo;
mod scheduler;
mod secrets;
mod sink;
mod store;

use std::sync::Arc;
use std::time::Duration;

use alerts::{AlertManager, Notifier};
use api::{AdminState, ApiState};
use axum::routing::get;
use config::Config;
use futures::stream::{Stream, StreamExt};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use repo::{NodeListing, NodeRepo, StaticNodeList};
use secrets::CredentialStore;
use sink::InMemorySink;
use store::{MetricStore, VmStore};
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

    // Alert engine + optional Webhook notifier (ADR-015).
    let alerts = Arc::new(AlertManager::new());
    let notifier = std::env::var("YAGRA_WEBHOOK_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .map(|url| {
            tracing::info!("alert Webhook notifier enabled");
            Arc::new(Notifier::webhook(url))
        });

    // Result consumer: bus → TSDB + alert engine.
    {
        let store = store.clone();
        let alerts = alerts.clone();
        let notifier = notifier.clone();
        let results = Box::pin(bus.subscribe_results().await?);
        tokio::spawn(consume_results(results, store, alerts, notifier));
    }

    // Optional SNMP v2c polling (ADR-021 PoC): enabled by a configured community. OIDs
    // default to sysUpTime. Per-node credential binding replaces this env hook later.
    let snmp = std::env::var("YAGRA_SNMP_COMMUNITY")
        .ok()
        .filter(|c| !c.is_empty())
        .map(|community| {
            let oids = std::env::var("YAGRA_SNMP_OIDS")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|s| {
                    s.split(',')
                        .map(|x| x.trim().to_owned())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| vec!["1.3.6.1.2.1.1.3.0".to_owned()]);
            tracing::info!(oids = oids.len(), "SNMP v2c polling enabled");
            (community, oids)
        });

    // Scheduler: inventory → jobs on the bus, jittered across the interval.
    {
        let repo = repo.clone();
        let bus = bus.clone();
        tokio::spawn(run_scheduler(repo, bus, cfg.poll_interval_secs, snmp));
    }

    // Write side (inventory + encrypted credentials), sharing the metadata DB pool.
    let creds = Arc::new(CredentialStore::from_env(repo.pool()));
    let admin = Some(Arc::new(AdminState {
        repo: repo.clone(),
        creds,
    }));

    let nodes: Arc<dyn NodeListing> = repo;
    let state = ApiState {
        store,
        nodes,
        alerts,
        admin,
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
) where
    S: Stream<Item = PollResult> + Unpin,
{
    while let Some(result) = results.next().await {
        metrics::counter!("yagra_poll_results_total").increment(1);
        store.write(&result).await;
        for action in alerts.observe(&result) {
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
    snmp: Option<(String, Vec<String>)>,
) {
    let interval = Duration::from_secs(u64::from(interval_secs));
    let window_ms = interval.as_millis().max(1) as u64;
    let jitter = || Duration::from_millis(rand::random::<u64>() % window_ms);
    loop {
        match repo.list_nodes().await {
            Ok(nodes) => {
                tracing::debug!(count = nodes.len(), "scheduling poll round");
                for node in nodes {
                    // Optional SNMP job (clone the node before the ICMP task takes it).
                    if let Some((community, oids)) = &snmp {
                        let bus = bus.clone();
                        let node = node.clone();
                        let check = SnmpCheck {
                            community: community.clone(),
                            oids: oids.clone(),
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
                            match bus.publish_job(job).await {
                                Ok(()) => {
                                    metrics::counter!("yagra_jobs_published_total").increment(1);
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, node = %node.id, "failed to publish snmp job");
                                }
                            }
                        });
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
                        match bus.publish_job(job).await {
                            Ok(()) => metrics::counter!("yagra_jobs_published_total").increment(1),
                            Err(e) => {
                                tracing::warn!(error = %e, node = %node.id, "failed to publish poll job");
                            }
                        }
                    });
                }
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
