//! Yagra-core (`saihai`) — Core/API.
//!
//! Orchestration, scheduling, and the northbound REST API (`/api/v1`, ADR-008/019) the
//! WebUI and external automation consume. Dispatches polling jobs to workers
//! (Yagra-poller) through the bus (Yagra-bus, ADR-003) and owns metadata-store
//! interaction (PostgreSQL) and the TSDB write/read path (VictoriaMetrics).
//!
//! Two run modes (see [`config`]): **live** when the store/bus URLs are present (real
//! PostgreSQL + NATS + VictoriaMetrics), else an in-memory **skeleton** so a bare
//! `cargo run` still serves the API.

mod api;
mod config;
mod repo;
mod scheduler;
mod sink;
mod store;

use std::sync::Arc;
use std::time::Duration;

use config::Config;
use futures::stream::{Stream, StreamExt};
use hikyaku::{Bus, IcmpCheck, NatsBus, PollResult};
use repo::NodeRepo;
use sink::InMemorySink;
use store::{MetricStore, VmStore};
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    match Config::from_env() {
        Some(cfg) => run_live(cfg).await,
        None => run_skeleton().await,
    }
}

/// Live mode: PostgreSQL + NATS + VictoriaMetrics, real ICMP polling end to end.
async fn run_live(cfg: Config) -> anyhow::Result<()> {
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

    // Result consumer: bus → TSDB.
    {
        let store = store.clone();
        let results = Box::pin(bus.subscribe_results().await?);
        tokio::spawn(consume_results(results, store));
    }

    // Scheduler: inventory → jobs on the bus, jittered across the interval.
    {
        let repo = repo.clone();
        let bus = bus.clone();
        tokio::spawn(run_scheduler(repo, bus, cfg.poll_interval_secs));
    }

    serve(store, &cfg.api_addr).await
}

/// Skeleton mode: serve the API over an in-memory sink seeded with one demo reading.
async fn run_skeleton() -> anyhow::Result<()> {
    tracing::warn!("store/bus URLs not set — running in in-memory skeleton mode (no real polling)");
    let sink = Arc::new(InMemorySink::default());
    // Demo seed so the walking-skeleton WebUI shows data before real polling is wired.
    sink.ingest(&PollResult {
        schema_version: 1,
        job_id: Uuid::nil(),
        node_id: yagra_common::NodeId::from(Uuid::nil()),
        at_unix_ms: 0,
        outcome: hikyaku::CheckOutcome::Reachable,
        samples: vec![hikyaku::Sample::gauge("icmp_rtt_ms", 8.0)],
    });
    serve(sink, "0.0.0.0:8080").await
}

/// Drain poll results off the bus into the metric store. Returns when the stream ends.
async fn consume_results<S>(mut results: S, store: Arc<dyn MetricStore>)
where
    S: Stream<Item = PollResult> + Unpin,
{
    while let Some(result) = results.next().await {
        store.write(&result).await;
    }
    tracing::warn!("result stream ended");
}

/// Periodically turn the inventory into ICMP jobs, spread across the interval with
/// per-node jitter so N nodes don't poll on the same tick (anti-stampede).
async fn run_scheduler(repo: Arc<NodeRepo>, bus: Arc<NatsBus>, interval_secs: u32) {
    let interval = Duration::from_secs(u64::from(interval_secs));
    let window_ms = interval.as_millis().max(1) as u64;
    loop {
        match repo.list_nodes().await {
            Ok(nodes) => {
                tracing::debug!(count = nodes.len(), "scheduling poll round");
                for node in nodes {
                    let bus = bus.clone();
                    let jitter = Duration::from_millis(rand::random::<u64>() % window_ms);
                    tokio::spawn(async move {
                        tokio::time::sleep(jitter).await;
                        let job = scheduler::build_icmp_job(
                            &node,
                            IcmpCheck::default(),
                            interval_secs,
                            Uuid::new_v4(),
                        );
                        if let Err(e) = bus.publish_job(job).await {
                            tracing::warn!(error = %e, node = %node.id, "failed to publish poll job");
                        }
                    });
                }
            }
            Err(e) => tracing::error!(error = %e, "scheduler: listing nodes failed"),
        }
        tokio::time::sleep(interval).await;
    }
}

/// Bind and serve the northbound API.
async fn serve(store: Arc<dyn MetricStore>, addr: &str) -> anyhow::Result<()> {
    let app = api::router(store);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "Yagra-core (saihai) API listening on /api/v1");
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
