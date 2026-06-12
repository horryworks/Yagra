//! Live NATS implementation of the [`Bus`] seam (ADR-007).
//!
//! This is the production core⇄poller transport. It is an I/O adapter (needs a running
//! NATS server), so it is feature-gated (`nats`) and exercised in deployment rather than
//! unit tests — the message contract itself is tested in [`crate::messages`] and the
//! poll loop against [`crate::InMemoryBus`].
//!
//! Wire format is JSON (version-tolerant per ADR-017). Jobs are published per poller
//! pool subject and pollers consume them with a **queue group** so each job is delivered
//! to exactly one poller (load-balanced), while results fan in on a single subject.

use crate::bus::{Bus, BusError};
use crate::messages::{DiscoveryJob, DiscoveryResult, PollJob, PollResult};
use crate::subjects;
use async_nats::Client;
use async_trait::async_trait;
use futures::stream::{Stream, StreamExt};

/// Default poller pool for the single-pool MVP (ADR-009). Multi-pool dispatch routes by
/// `Node::pool` later; today every job lands on this subject and the wildcard catches it.
pub const DEFAULT_POOL: &str = "default";

/// Queue group name shared by all pollers so jobs load-balance across them.
pub const POLLER_QUEUE: &str = "pollers";

/// A [`Bus`] over NATS.
pub struct NatsBus {
    client: Client,
    job_subject: String,
}

impl NatsBus {
    /// Connect to NATS at `url` (e.g. `nats://nats:4222`).
    pub async fn connect(url: &str) -> Result<Self, BusError> {
        let client = async_nats::connect(url)
            .await
            .map_err(|e| BusError::Publish(format!("nats connect {url}: {e}")))?;
        tracing::info!(%url, "connected to NATS bus");
        Ok(Self {
            client,
            job_subject: subjects::jobs_for_pool(DEFAULT_POOL),
        })
    }

    /// Route published jobs to a specific pool's subject (defaults to [`DEFAULT_POOL`]).
    #[must_use]
    pub fn with_pool(mut self, pool: &str) -> Self {
        self.job_subject = subjects::jobs_for_pool(pool);
        self
    }

    /// Subscribe (in a queue group) to every pool's jobs — poller side. Malformed
    /// messages are logged and skipped rather than killing the stream.
    pub async fn subscribe_jobs(
        &self,
        queue: &str,
    ) -> Result<impl Stream<Item = PollJob>, BusError> {
        let sub = self
            .client
            .queue_subscribe(subjects::jobs_all(), queue.to_owned())
            .await
            .map_err(|e| BusError::Publish(format!("subscribe jobs: {e}")))?;
        Ok(sub.filter_map(|msg| async move {
            match serde_json::from_slice::<PollJob>(&msg.payload) {
                Ok(job) => Some(job),
                Err(e) => {
                    tracing::warn!(error = %e, "dropping malformed PollJob from bus");
                    None
                }
            }
        }))
    }

    /// Subscribe to poll results — core side. Malformed messages are skipped.
    pub async fn subscribe_results(&self) -> Result<impl Stream<Item = PollResult>, BusError> {
        let sub = self
            .client
            .subscribe(subjects::results())
            .await
            .map_err(|e| BusError::Publish(format!("subscribe results: {e}")))?;
        Ok(sub.filter_map(|msg| async move {
            match serde_json::from_slice::<PollResult>(&msg.payload) {
                Ok(result) => Some(result),
                Err(e) => {
                    tracing::warn!(error = %e, "dropping malformed PollResult from bus");
                    None
                }
            }
        }))
    }

    /// Publish a discovery sweep job — core side.
    pub async fn publish_discovery_job(&self, job: DiscoveryJob) -> Result<(), BusError> {
        let payload = serde_json::to_vec(&job)
            .map_err(|e| BusError::Publish(format!("encode discovery job: {e}")))?;
        self.client
            .publish(subjects::discovery_jobs(), payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish discovery job: {e}")))
    }

    /// Subscribe (in a queue group) to discovery jobs — poller side. Malformed messages skipped.
    pub async fn subscribe_discovery_jobs(
        &self,
        queue: &str,
    ) -> Result<impl Stream<Item = DiscoveryJob>, BusError> {
        let sub = self
            .client
            .queue_subscribe(subjects::discovery_jobs(), queue.to_owned())
            .await
            .map_err(|e| BusError::Publish(format!("subscribe discovery jobs: {e}")))?;
        Ok(sub.filter_map(|msg| async move {
            match serde_json::from_slice::<DiscoveryJob>(&msg.payload) {
                Ok(job) => Some(job),
                Err(e) => {
                    tracing::warn!(error = %e, "dropping malformed DiscoveryJob from bus");
                    None
                }
            }
        }))
    }

    /// Publish a discovery result — poller side.
    pub async fn publish_discovery_result(&self, result: DiscoveryResult) -> Result<(), BusError> {
        let payload = serde_json::to_vec(&result)
            .map_err(|e| BusError::Publish(format!("encode discovery result: {e}")))?;
        self.client
            .publish(subjects::discovery_results(), payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish discovery result: {e}")))
    }

    /// Subscribe to discovery results — core side. Malformed messages skipped.
    pub async fn subscribe_discovery_results(
        &self,
    ) -> Result<impl Stream<Item = DiscoveryResult>, BusError> {
        let sub = self
            .client
            .subscribe(subjects::discovery_results())
            .await
            .map_err(|e| BusError::Publish(format!("subscribe discovery results: {e}")))?;
        Ok(sub.filter_map(|msg| async move {
            match serde_json::from_slice::<DiscoveryResult>(&msg.payload) {
                Ok(result) => Some(result),
                Err(e) => {
                    tracing::warn!(error = %e, "dropping malformed DiscoveryResult from bus");
                    None
                }
            }
        }))
    }
}

#[async_trait]
impl Bus for NatsBus {
    async fn publish_job(&self, job: PollJob) -> Result<(), BusError> {
        let payload =
            serde_json::to_vec(&job).map_err(|e| BusError::Publish(format!("encode job: {e}")))?;
        self.client
            .publish(self.job_subject.clone(), payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish job: {e}")))
    }

    async fn publish_result(&self, result: PollResult) -> Result<(), BusError> {
        let payload = serde_json::to_vec(&result)
            .map_err(|e| BusError::Publish(format!("encode result: {e}")))?;
        self.client
            .publish(subjects::results(), payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish result: {e}")))
    }
}
