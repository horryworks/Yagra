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

mod worker;

use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

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
    tracing::info!(%queue, "Yagra-poller (yagra_poller) consuming jobs");

    // Runs until the bus closes; the stateless loop carries no recovery state.
    worker::run_stream(jobs, bus, transport).await;
    tracing::warn!("job stream ended — poller shutting down");
    Ok(())
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
