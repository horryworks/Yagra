//! Yagra-poller — stateless poller worker.
//!
//! Pulls polling jobs off the bus (Yagra-bus / NATS), executes them via the transport
//! layer (Yagra-transport / raw-socket ICMP), and ships metrics back. Horizontally
//! scalable: no local state beyond in-flight jobs, so workers can be added/removed and
//! re-sharded freely (ADR-003/009). Jobs are consumed in a NATS queue group so each is
//! delivered to exactly one poller.
//!
//! Without `YAGRA_BUS_URL` the binary stays idle (so a bare `cargo run` doesn't
//! crash-loop or require raw-socket privilege); the container always sets it.

mod discovery;
mod limiter;
mod listeners;
mod worker;

use limiter::PollLimiter;
use metrics_exporter_prometheus::PrometheusBuilder;
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // Self-observability: expose Prometheus metrics on :9100/metrics (monitoring-conventions).
    if let Err(e) = PrometheusBuilder::new()
        .with_http_listener(([0, 0, 0, 0], 9100))
        .install()
    {
        tracing::warn!(error = %e, "failed to start metrics exporter");
    }

    let Ok(bus_url) = std::env::var("YAGRA_BUS_URL") else {
        tracing::warn!("YAGRA_BUS_URL not set — poller idle (no bus configured)");
        std::future::pending::<()>().await;
        return Ok(());
    };

    // Raw-socket ICMP transport — needs CAP_NET_RAW (granted to this container only).
    let transport: Arc<dyn yagra_transport::Transport> =
        Arc::new(yagra_transport::SurgePingTransport::new()?);
    tracing::info!("ICMP transport ready (raw sockets)");

    // Tolerate NATS coming up after the poller (compose has no health gate).
    let bus = Arc::new(connect_bus(&bus_url).await?);

    let queue =
        std::env::var("YAGRA_POLLER_QUEUE").unwrap_or_else(|_| yagra_bus::POLLER_QUEUE.to_owned());
    let jobs = Box::pin(bus.subscribe_jobs(&queue).await?);

    // Rate control: bound total concurrent probes + per-device single-flight (#4).
    let max_concurrent = std::env::var("YAGRA_MAX_CONCURRENT_POLLS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(64);
    let limiter = Arc::new(PollLimiter::new(max_concurrent));
    tracing::info!(%queue, max_concurrent, "Yagra-poller consuming jobs");

    // Discovery sweeps (separate subject) run alongside polling — they need the same raw-socket
    // ICMP + SNMP transport.
    {
        let disco_jobs = Box::pin(bus.subscribe_discovery_jobs(&queue).await?);
        let bus = bus.clone();
        let transport = transport.clone();
        tokio::spawn(discovery::run_discovery_stream(disco_jobs, bus, transport));
    }

    // Passive-event listeners (Phase 2): syslog / SNMP traps, enabled per site via env.
    // They publish EventMsgs on `yagra.events`; core does the rule matching.
    spawn_event_listeners(&bus).await;

    // Runs until the bus closes; the stateless loop carries no recovery state.
    worker::run_stream(jobs, bus, transport, limiter).await;
    tracing::warn!("job stream ended — poller shutting down");
    Ok(())
}

/// Spawn the syslog / SNMP-trap listeners for every bind address configured via env.
/// Unset (or empty) env = listener disabled. Both share one rate limiter so the global
/// budget covers all passive intake on this poller.
async fn spawn_event_listeners(bus: &Arc<yagra_bus::NatsBus>) {
    let syslog_bind = env_nonempty("YAGRA_SYSLOG_BIND");
    let trap_bind = env_nonempty("YAGRA_TRAP_BIND");
    if syslog_bind.is_none() && trap_bind.is_none() {
        return;
    }

    let per_source = env_f64("YAGRA_EVENT_RATE_PER_SOURCE", 50.0);
    let global = env_f64("YAGRA_EVENT_RATE_GLOBAL", 500.0);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let limiter = Arc::new(tokio::sync::Mutex::new(yagra_ingest::SourceLimiter::new(
        per_source, global, now_ms,
    )));
    let pool = env_nonempty("YAGRA_POLLER_POOL");

    if let Some(bind) = syslog_bind {
        match listeners::bind_udp(&bind).await {
            Ok(sock) => {
                tracing::info!(%bind, per_source, global, "syslog listener enabled");
                tokio::spawn(listeners::run_syslog_listener(
                    sock,
                    bus.clone(),
                    limiter.clone(),
                    pool.clone(),
                ));
            }
            Err(e) => tracing::error!(%bind, error = %e, "failed to bind syslog listener"),
        }
    }

    if let Some(bind) = trap_bind {
        // Optional community filter — value must never be logged.
        let community = env_nonempty("YAGRA_TRAP_COMMUNITY");
        match listeners::bind_udp(&bind).await {
            Ok(sock) => {
                tracing::info!(%bind, community_filter = community.is_some(), "trap listener enabled (v1/v2c; v3 traps out of scope)");
                tokio::spawn(listeners::run_trap_listener(
                    sock,
                    bus.clone(),
                    limiter,
                    community,
                    pool,
                ));
            }
            Err(e) => tracing::error!(%bind, error = %e, "failed to bind trap listener"),
        }
    }
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(default)
}

/// Connect to NATS, retrying with a fixed backoff so startup ordering doesn't matter.
async fn connect_bus(url: &str) -> anyhow::Result<yagra_bus::NatsBus> {
    const MAX_ATTEMPTS: u32 = 30;
    let mut attempt = 0;
    loop {
        match yagra_bus::NatsBus::connect(url).await {
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
