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

use crate::bus::{Bus, BusError, SyncBus};
use crate::messages::{
    DiscoveryJob, DiscoveryResult, EventMsg, HeartbeatMsg, PollJob, PollResult, SyncMsg,
    SyncRequest,
};
use crate::subjects;
use async_nats::Client;
use async_trait::async_trait;
use futures::stream::{Stream, StreamExt};

/// Default poller pool for the single-pool MVP (ADR-009). Multi-pool dispatch routes by
/// `Node::pool` later; today every job lands on this subject and the wildcard catches it.
pub const DEFAULT_POOL: &str = "default";

/// Queue group name shared by all pollers so jobs load-balance across them.
pub const POLLER_QUEUE: &str = "pollers";

/// Strip any `user:pass@` userinfo from a NATS URL before it reaches a log line or error
/// message. The remote-poller path carries credentials in the URL (`tls://user:pass@host:4222`,
/// ADR-020), and those must never be logged (security.md). `tls://poller:secret@host:4222` →
/// `tls://host:4222`; a URL without userinfo is returned unchanged. Display-only, best-effort.
fn redact_url(url: &str) -> String {
    // Only the authority (between `://` and the first `/`, `?` or `#`) can hold userinfo.
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (Some(s), r),
        None => (None, url),
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    // Userinfo runs up to the last `@` in the authority; the rest is host[:port].
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    match scheme {
        Some(s) => format!("{s}://{host}{tail}"),
        None => format!("{host}{tail}"),
    }
}

/// A [`Bus`] over NATS.
pub struct NatsBus {
    client: Client,
    job_subject: String,
}

impl NatsBus {
    /// Connect to NATS at `url` (e.g. `nats://nats:4222`), no TLS root CA — the single-node /
    /// plaintext MVP path. Delegates to [`Self::connect_opts`].
    pub async fn connect(url: &str) -> Result<Self, BusError> {
        Self::connect_opts(url, None).await
    }

    /// Connect to NATS at `url` with an optional TLS root CA (`ca_file`) to pin the server
    /// certificate — the remote-poller path, where credentials cross a trust boundary and TLS is
    /// mandatory (security.md / ADR-020). Authentication rides in the URL
    /// (`nats://user:pass@host`), so there are no extra auth params. With `ca_file = None` this is
    /// the plaintext single-node connection.
    pub async fn connect_opts(
        url: &str,
        ca_file: Option<&std::path::Path>,
    ) -> Result<Self, BusError> {
        let mut opts = async_nats::ConnectOptions::new();
        if let Some(ca) = ca_file {
            opts = opts.add_root_certificates(ca.to_path_buf());
        }
        let client = opts
            .connect(url)
            .await
            .map_err(|e| BusError::Publish(format!("nats connect {}: {e}", redact_url(url))))?;
        tracing::info!(url = %redact_url(url), tls = ca_file.is_some(), "connected to NATS bus");
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

    /// Subscribe to passive events — core side (single consumer, no queue group, same as
    /// results). Malformed messages are skipped.
    pub async fn subscribe_events(&self) -> Result<impl Stream<Item = EventMsg>, BusError> {
        let sub = self
            .client
            .subscribe(subjects::events())
            .await
            .map_err(|e| BusError::Publish(format!("subscribe events: {e}")))?;
        Ok(sub.filter_map(|msg| async move {
            match serde_json::from_slice::<EventMsg>(&msg.payload) {
                Ok(event) => Some(event),
                Err(e) => {
                    tracing::warn!(error = %e, "dropping malformed EventMsg from bus");
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

    // ── Distributed poller pool (ADR-009/020) — control plane ───────────────────────

    /// Subscribe to poller heartbeats — core side (single consumer, no queue group). Malformed
    /// messages are logged and skipped rather than killing the stream.
    pub async fn subscribe_heartbeats(&self) -> Result<impl Stream<Item = HeartbeatMsg>, BusError> {
        let sub = self
            .client
            .subscribe(subjects::heartbeat())
            .await
            .map_err(|e| BusError::Publish(format!("subscribe heartbeats: {e}")))?;
        Ok(sub.filter_map(|msg| async move {
            match serde_json::from_slice::<HeartbeatMsg>(&msg.payload) {
                Ok(hb) => Some(hb),
                Err(e) => {
                    tracing::warn!(error = %e, "dropping malformed HeartbeatMsg from bus");
                    None
                }
            }
        }))
    }

    /// Subscribe to poller snapshot requests — core side (single consumer, no queue group).
    /// Malformed messages are skipped.
    pub async fn subscribe_sync_requests(
        &self,
    ) -> Result<impl Stream<Item = SyncRequest>, BusError> {
        let sub = self
            .client
            .subscribe(subjects::sync_request())
            .await
            .map_err(|e| BusError::Publish(format!("subscribe sync requests: {e}")))?;
        Ok(sub.filter_map(|msg| async move {
            match serde_json::from_slice::<SyncRequest>(&msg.payload) {
                Ok(req) => Some(req),
                Err(e) => {
                    tracing::warn!(error = %e, "dropping malformed SyncRequest from bus");
                    None
                }
            }
        }))
    }

    /// Publish a pool-scoped discovery job — core side (the discovery analogue of
    /// [`SyncBus::publish_job_for_pool`]).
    pub async fn publish_discovery_job_for_pool(
        &self,
        pool: &str,
        job: DiscoveryJob,
    ) -> Result<(), BusError> {
        let payload = serde_json::to_vec(&job)
            .map_err(|e| BusError::Publish(format!("encode discovery job: {e}")))?;
        self.client
            .publish(subjects::discovery_jobs_for_pool(pool), payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish discovery job for pool {pool}: {e}")))
    }

    /// Subscribe to this poller's working-set sync — poller side. A **plain** subscribe (no queue
    /// group): the assignment subject is addressed to one poller, and its single-subject ordering
    /// is what `seq` gap-detection relies on (ADR-020). Malformed messages are skipped.
    pub async fn subscribe_sync(
        &self,
        poller_id: &str,
    ) -> Result<impl Stream<Item = SyncMsg>, BusError> {
        let sub = self
            .client
            .subscribe(subjects::assignment_for(poller_id))
            .await
            .map_err(|e| BusError::Publish(format!("subscribe sync: {e}")))?;
        Ok(sub.filter_map(|msg| async move {
            match serde_json::from_slice::<SyncMsg>(&msg.payload) {
                Ok(sync) => Some(sync),
                Err(e) => {
                    tracing::warn!(error = %e, "dropping malformed SyncMsg from bus");
                    None
                }
            }
        }))
    }

    /// Subscribe (in a queue group) to a specific pool's jobs — poller side. Unlike
    /// [`Self::subscribe_jobs`] (which wildcards every pool), a pooled poller consumes only its own
    /// pool's subject so jobs stay local (ADR-009). Malformed messages are skipped.
    pub async fn subscribe_jobs_for_pool(
        &self,
        pool: &str,
        queue: &str,
    ) -> Result<impl Stream<Item = PollJob>, BusError> {
        let sub = self
            .client
            .queue_subscribe(subjects::jobs_for_pool(pool), queue.to_owned())
            .await
            .map_err(|e| BusError::Publish(format!("subscribe jobs for pool {pool}: {e}")))?;
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

    /// Subscribe (in a queue group) to a specific pool's discovery jobs — poller side. Malformed
    /// messages are skipped.
    pub async fn subscribe_discovery_jobs_for_pool(
        &self,
        pool: &str,
        queue: &str,
    ) -> Result<impl Stream<Item = DiscoveryJob>, BusError> {
        let sub = self
            .client
            .queue_subscribe(subjects::discovery_jobs_for_pool(pool), queue.to_owned())
            .await
            .map_err(|e| {
                BusError::Publish(format!("subscribe discovery jobs for pool {pool}: {e}"))
            })?;
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

    async fn publish_event(&self, event: EventMsg) -> Result<(), BusError> {
        let payload = serde_json::to_vec(&event)
            .map_err(|e| BusError::Publish(format!("encode event: {e}")))?;
        self.client
            .publish(subjects::events(), payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish event: {e}")))
    }
}

#[async_trait]
impl SyncBus for NatsBus {
    async fn publish_sync(&self, poller_id: &str, msg: SyncMsg) -> Result<(), BusError> {
        let payload =
            serde_json::to_vec(&msg).map_err(|e| BusError::Publish(format!("encode sync: {e}")))?;
        // One subject per poller preserves the ordering seq gap-detection relies on (ADR-020).
        self.client
            .publish(subjects::assignment_for(poller_id), payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish sync to {poller_id}: {e}")))
    }

    async fn publish_heartbeat(&self, hb: HeartbeatMsg) -> Result<(), BusError> {
        let payload = serde_json::to_vec(&hb)
            .map_err(|e| BusError::Publish(format!("encode heartbeat: {e}")))?;
        self.client
            .publish(subjects::heartbeat(), payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish heartbeat: {e}")))
    }

    async fn publish_sync_request(&self, req: SyncRequest) -> Result<(), BusError> {
        let payload = serde_json::to_vec(&req)
            .map_err(|e| BusError::Publish(format!("encode sync request: {e}")))?;
        self.client
            .publish(subjects::sync_request(), payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish sync request: {e}")))
    }

    async fn publish_job_for_pool(&self, pool: &str, job: PollJob) -> Result<(), BusError> {
        let payload =
            serde_json::to_vec(&job).map_err(|e| BusError::Publish(format!("encode job: {e}")))?;
        self.client
            .publish(subjects::jobs_for_pool(pool), payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish job for pool {pool}: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::redact_url;

    #[test]
    fn redacts_userinfo_credentials() {
        // The remote-poller URL carries a NATS password — it must not survive into a log.
        assert_eq!(
            redact_url("tls://poller:s3cret@nats.example.com:4222"),
            "tls://nats.example.com:4222"
        );
        assert_eq!(
            redact_url("nats://user:pass@host:4222/path?x=1"),
            "nats://host:4222/path?x=1"
        );
    }

    #[test]
    fn passes_through_urls_without_userinfo() {
        assert_eq!(redact_url("nats://nats:4222"), "nats://nats:4222");
        assert_eq!(
            redact_url("tls://nats.example.com:4222"),
            "tls://nats.example.com:4222"
        );
    }

    #[test]
    fn handles_missing_scheme_and_user_only() {
        assert_eq!(redact_url("host:4222"), "host:4222");
        // user-only (no colon) userinfo is still stripped.
        assert_eq!(redact_url("nats://user@host:4222"), "nats://host:4222");
    }
}
