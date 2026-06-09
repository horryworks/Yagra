//! Yagra-poller (`banshu`) — stateless poller worker.
//!
//! Pulls polling jobs off the bus (Yagra-bus), executes them via the transport layer
//! (Yagra-transport), and ships metrics back. Horizontally scalable: no local state
//! beyond in-flight jobs, so workers can be added/removed and re-sharded freely (ADR-003).
//!
//! The poll loop lives in [`worker`] (implemented and tested). What remains is wiring it
//! to the real NATS bus (ADR-007) and the real ICMP transport (`surge-ping`, needs
//! `CAP_NET_RAW`); until then this entry point only initializes tracing.

mod worker;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!("Yagra-poller (banshu) — stub; NATS bus + ICMP transport wiring pending");

    // The loop is ready: once the NATS `Bus` impl and `surge-ping` transport land, the
    // entry point becomes roughly:
    //     let bus = Arc::new(NatsBus::connect(&bus_url).await?);
    //     let transport = Arc::new(SurgePingTransport::new());
    //     worker::run(bus.subscribe_jobs(), bus, transport).await;
    // Referenced here so the tested loop is wired into the binary, not orphaned:
    let _entrypoint = worker::run::<hikyaku::InMemoryBus>;

    Ok(())
}
