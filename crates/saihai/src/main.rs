//! Yagra-core (`saihai`) — Core/API.
//!
//! Orchestration, scheduling, and the northbound REST API (`/api/v1`, ADR-008/019) that
//! the WebUI (Yagra-web) and external automation consume. Dispatches polling jobs to
//! workers (Yagra-poller) through the bus (Yagra-bus, ADR-003) and owns metadata-store
//! interaction.
//!
//! The scheduler ([`scheduler`]), metric sink ([`sink`]), and API ([`api`]) are
//! implemented and tested. What remains is wiring them to the real NATS bus and
//! PostgreSQL/VictoriaMetrics; until then `main` serves the API over an in-memory sink.

mod api;
mod scheduler;
mod sink;

use std::sync::Arc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let sink = Arc::new(sink::InMemorySink::default());

    // Demo seed: one reading so the walking-skeleton API/WebUI show data before real
    // polling is wired (the WebUI's NodeDetail queries this exact node + metric).
    {
        use hikyaku::{CheckOutcome, PollResult, Sample};
        use sink::MetricSink;
        sink.ingest(&PollResult {
            schema_version: 1,
            job_id: uuid::Uuid::nil(),
            node_id: yagra_common::NodeId::from(uuid::Uuid::nil()),
            at_unix_ms: 0,
            outcome: CheckOutcome::Reachable,
            samples: vec![Sample::gauge("icmp_rtt_ms", 8.0)],
        });
    }

    let app = api::router(sink.clone());

    // NATS wiring pending (ADR-007). Once the `Bus` impl lands this becomes:
    //   tokio::spawn(sink::run_sink(bus.subscribe_results(), sink.clone()));
    //   // scheduler loop: for each due node, bus.publish_job(scheduler::build_icmp_job(..)).
    // Referenced so the tested pieces are wired into the binary, not orphaned:
    let _scheduler = scheduler::build_icmp_job;
    let _result_consumer = sink::run_sink::<sink::InMemorySink>;

    let addr = "0.0.0.0:8080";
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "Yagra-core (saihai) API listening on /api/v1");
    axum::serve(listener, app).await?;
    Ok(())
}
