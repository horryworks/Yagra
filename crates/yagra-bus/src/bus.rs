//! The bus abstraction and an in-memory implementation.
//!
//! [`Bus`] is the publish seam core and pollers depend on, so neither calls the other
//! directly (ADR-003). [`InMemoryBus`] is a real, working implementation over Tokio
//! broadcast channels — used for tests and the single-process walking skeleton. A NATS
//! implementation (the production path) slots in behind the same trait later.

use crate::messages::{PollJob, PollResult};
use async_trait::async_trait;
use thiserror::Error;
use tokio::sync::broadcast;

/// Errors publishing to the bus.
#[derive(Debug, Error)]
pub enum BusError {
    /// The underlying transport rejected the publish.
    #[error("bus publish failed: {0}")]
    Publish(String),
}

/// The publish side of the core⇄poller bus.
///
/// Core publishes [`PollJob`]s; pollers publish [`PollResult`]s. Subscription is
/// implementation-specific (see [`InMemoryBus::subscribe_jobs`]).
#[async_trait]
pub trait Bus: Send + Sync {
    /// Publish a poll job for pollers to consume.
    async fn publish_job(&self, job: PollJob) -> Result<(), BusError>;
    /// Publish a poll result for core to consume.
    async fn publish_result(&self, result: PollResult) -> Result<(), BusError>;
}

/// An in-process bus over Tokio broadcast channels.
///
/// Fan-out, fire-and-forget: publishing with no current subscribers is **not** an error
/// (the message is simply dropped), matching a pub/sub bus's semantics.
pub struct InMemoryBus {
    jobs: broadcast::Sender<PollJob>,
    results: broadcast::Sender<PollResult>,
}

impl InMemoryBus {
    /// Create a bus with the given per-channel buffer capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (jobs, _) = broadcast::channel(capacity);
        let (results, _) = broadcast::channel(capacity);
        Self { jobs, results }
    }

    /// Subscribe to poll jobs (poller side).
    #[must_use]
    pub fn subscribe_jobs(&self) -> broadcast::Receiver<PollJob> {
        self.jobs.subscribe()
    }

    /// Subscribe to poll results (core side).
    #[must_use]
    pub fn subscribe_results(&self) -> broadcast::Receiver<PollResult> {
        self.results.subscribe()
    }
}

impl Default for InMemoryBus {
    fn default() -> Self {
        Self::new(1024)
    }
}

#[async_trait]
impl Bus for InMemoryBus {
    async fn publish_job(&self, job: PollJob) -> Result<(), BusError> {
        // broadcast::send only errors when there are no receivers — fine for pub/sub.
        let _ = self.jobs.send(job);
        Ok(())
    }

    async fn publish_result(&self, result: PollResult) -> Result<(), BusError> {
        let _ = self.results.send(result);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{CheckOutcome, IcmpCheck, PollResult, Sample};
    use std::net::{IpAddr, Ipv4Addr};
    use uuid::Uuid;
    use yagra_common::NodeId;

    #[tokio::test]
    async fn published_job_reaches_subscriber() {
        let bus = InMemoryBus::new(8);
        let mut rx = bus.subscribe_jobs();

        let job = PollJob::icmp(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IcmpCheck::default(),
            30,
        );
        bus.publish_job(job.clone()).await.unwrap();

        assert_eq!(rx.recv().await.unwrap(), job);
    }

    #[tokio::test]
    async fn published_result_reaches_subscriber() {
        let bus = InMemoryBus::new(8);
        let mut rx = bus.subscribe_results();

        let result = PollResult {
            schema_version: 1,
            job_id: Uuid::nil(),
            node_id: NodeId::from(Uuid::nil()),
            at_unix_ms: 0,
            outcome: CheckOutcome::Reachable,
            samples: vec![Sample::gauge("icmp_rtt_ms", 12.5)],
            interfaces: Vec::new(),
        };
        bus.publish_result(result.clone()).await.unwrap();

        assert_eq!(rx.recv().await.unwrap(), result);
    }

    #[tokio::test]
    async fn publish_without_subscribers_is_ok() {
        let bus = InMemoryBus::new(8);
        let job = PollJob::icmp(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IcmpCheck::default(),
            30,
        );
        // No subscribers — must not error.
        bus.publish_job(job).await.unwrap();
    }
}
