// SPDX-License-Identifier: AGPL-3.0-only
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

use crate::bus::{Bus, BusError, DiscoveryBus, PeerBus, SyncBus};
use crate::messages::{
    AuthRevoke, DiscoveryJob, DiscoveryResult, EventMsg, FlowBatch, HeartbeatMsg, PollJob,
    PollResult, RawFlowDatagram, SyncMsg, SyncRequest,
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
///
/// Public because the support bundle (ADR-045) carries connection URLs and must redact them by the
/// **same** rule — a second implementation of "where does the userinfo end" is exactly the kind of
/// near-copy that drifts into leaking one (extensibility §3). If a third consumer appears, this and
/// [`split_userinfo_password`] belong in `yagra-common` next to `host_ip`.
pub fn redact_url(url: &str) -> String {
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

/// Split a NATS URL into `(url_without_userinfo, password)`. Mirrors [`redact_url`]'s authority
/// parsing but also returns the password so the poller can re-supply it explicitly alongside its own
/// id as the username (ADR-030). `tls://poller:secret@host:4222` → (`tls://host:4222`, `Some("secret")`);
/// a URL with no userinfo (single-node plaintext) → (unchanged, `None`).
///
/// Public for the same reason as [`redact_url`], plus one of its own: the support bundle's
/// fail-closed scan needs the *literal* password so it can prove that exact string appears nowhere
/// in the archive, which is a stronger check than any pattern.
pub fn split_userinfo_password(url: &str) -> (String, Option<String>) {
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (Some(s), r),
        None => (None, url),
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let (userinfo, host) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };
    let password = userinfo.and_then(|ui| ui.split_once(':').map(|(_, p)| p.to_owned()));
    let clean = match scheme {
        Some(s) => format!("{s}://{host}{tail}"),
        None => format!("{host}{tail}"),
    };
    (clean, password)
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

    /// Connect and present this poller's **own identity** to core's Auth Callout (ADR-030): the
    /// connection username becomes `poller_id` (what the callout scopes `yagra.poller.assign.{id}` to)
    /// and the connection name becomes `pool`. The password (bootstrap secret) is taken from the URL
    /// userinfo and re-supplied explicitly so the *username* is the poller id rather than the shared
    /// `poller` — the callout keys the scope on it. The userinfo is stripped from the URL so the
    /// explicit credential wins. On the plaintext single-node bus the URL has no userinfo, so no
    /// credential is presented (only the harmless connection name) — behaviour matches
    /// [`Self::connect_opts`] there, and a server without auth callout simply ignores user/name.
    pub async fn connect_opts_identified(
        url: &str,
        ca_file: Option<&std::path::Path>,
        poller_id: &str,
        pool: &str,
    ) -> Result<Self, BusError> {
        let mut opts = async_nats::ConnectOptions::new().name(pool.to_owned());
        if let Some(ca) = ca_file {
            opts = opts.add_root_certificates(ca.to_path_buf());
        }
        let (clean_url, password) = split_userinfo_password(url);
        if let Some(pass) = password {
            opts = opts.user_and_password(poller_id.to_owned(), pass);
        }
        let client = opts
            .connect(&clean_url)
            .await
            .map_err(|e| BusError::Publish(format!("nats connect {}: {e}", redact_url(url))))?;
        tracing::info!(
            url = %redact_url(url),
            tls = ca_file.is_some(),
            poller = %poller_id,
            pool = %pool,
            "connected to NATS bus (identified)"
        );
        Ok(Self {
            client,
            job_subject: subjects::jobs_for_pool(DEFAULT_POOL),
        })
    }

    /// A clone of the underlying NATS client, for control-plane traffic that doesn't fit the [`Bus`]
    /// job/result seam — specifically core's Auth Callout responder, which request-replies on the
    /// NATS system subject `$SYS.REQ.USER.AUTH` (ADR-030). `async_nats::Client` is a cheap Arc handle.
    #[must_use]
    pub fn client(&self) -> Client {
        self.client.clone()
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

    /// Subscribe to **backfilled** poll results — core side (store-and-forward, Phase 3). Plain
    /// `.subscribe()` on the separate backfill subject, mirroring [`Self::subscribe_results`].
    /// Malformed messages are skipped. Only a store-and-forward-aware core subscribes here; an older
    /// core never does, which is what makes a newer poller's backfill degrade safely (dropped, not
    /// alert-flooding).
    pub async fn subscribe_results_backfill(
        &self,
    ) -> Result<impl Stream<Item = PollResult>, BusError> {
        let sub = self
            .client
            .subscribe(subjects::results_backfill())
            .await
            .map_err(|e| BusError::Publish(format!("subscribe results backfill: {e}")))?;
        Ok(sub.filter_map(|msg| async move {
            match serde_json::from_slice::<PollResult>(&msg.payload) {
                Ok(result) => Some(result),
                Err(e) => {
                    tracing::warn!(error = %e, "dropping malformed backfill PollResult from bus");
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

    /// Subscribe to edge-aggregated flow batches — core side (ADR-031, single consumer, mirrors
    /// [`Self::subscribe_events`]). Malformed messages are skipped. Only a flow-aware core subscribes
    /// here; an older core never does, so a newer poller's flow batches degrade safely (dropped).
    pub async fn subscribe_flows(&self) -> Result<impl Stream<Item = FlowBatch>, BusError> {
        let sub = self
            .client
            .subscribe(subjects::flows())
            .await
            .map_err(|e| BusError::Publish(format!("subscribe flows: {e}")))?;
        Ok(sub.filter_map(|msg| async move {
            match serde_json::from_slice::<FlowBatch>(&msg.payload) {
                Ok(batch) => Some(batch),
                Err(e) => {
                    tracing::warn!(error = %e, "dropping malformed FlowBatch from bus");
                    None
                }
            }
        }))
    }

    /// Subscribe to verbatim flow datagrams — core side (ADR-034 Increment 2, mirrors
    /// [`Self::subscribe_flows`]). Malformed messages are skipped. Only a forwarding-aware core
    /// subscribes here, so a newer poller's relay degrades safely on an older core (dropped).
    pub async fn subscribe_raw_flows(
        &self,
    ) -> Result<impl Stream<Item = RawFlowDatagram>, BusError> {
        let sub = self
            .client
            .subscribe(subjects::flows_raw())
            .await
            .map_err(|e| BusError::Publish(format!("subscribe raw flows: {e}")))?;
        Ok(sub.filter_map(|msg| async move {
            match serde_json::from_slice::<RawFlowDatagram>(&msg.payload) {
                Ok(dg) => Some(dg),
                Err(e) => {
                    tracing::warn!(error = %e, "dropping malformed RawFlowDatagram from bus");
                    None
                }
            }
        }))
    }

    // ── Core⇄core control plane (ADR-016 Increment 2 — active/active API) ────────────

    /// Subscribe to session-revocation notices from other cores — core side (fan-out, every core
    /// receives every revocation). Malformed messages are skipped. Additive: an N-1 core never
    /// subscribes here, so a revocation simply doesn't reach it live — the durable `auth_revocations`
    /// table it cold-loads on start is the backstop.
    pub async fn subscribe_auth_revoke(&self) -> Result<impl Stream<Item = AuthRevoke>, BusError> {
        let sub = self
            .client
            .subscribe(subjects::auth_revoke())
            .await
            .map_err(|e| BusError::Publish(format!("subscribe auth revoke: {e}")))?;
        Ok(sub.filter_map(|msg| async move {
            match serde_json::from_slice::<AuthRevoke>(&msg.payload) {
                Ok(r) => Some(r),
                Err(e) => {
                    tracing::warn!(error = %e, "dropping malformed AuthRevoke from bus");
                    None
                }
            }
        }))
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

    /// Subscribe (in a queue group) to a specific pool's jobs — poller side. **The only job
    /// subscribe there is**: an all-pools wildcard subscribe (`yagra.jobs.*`) used to exist beside
    /// this and was removed unused, because a poller absorbing another pool's jobs would receive
    /// that pool's plaintext device credentials (ADR-020) and scan the wrong network. A pooled
    /// poller consumes only its own pool's subject so jobs stay local (ADR-009). Malformed messages
    /// are skipped.
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

    async fn publish_result_backfill(&self, result: PollResult) -> Result<(), BusError> {
        let payload = serde_json::to_vec(&result)
            .map_err(|e| BusError::Publish(format!("encode backfill result: {e}")))?;
        self.client
            .publish(subjects::results_backfill(), payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish backfill result: {e}")))
    }

    async fn publish_event(&self, event: EventMsg) -> Result<(), BusError> {
        let payload = serde_json::to_vec(&event)
            .map_err(|e| BusError::Publish(format!("encode event: {e}")))?;
        self.client
            .publish(subjects::events(), payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish event: {e}")))
    }

    async fn publish_flows(&self, batch: FlowBatch) -> Result<(), BusError> {
        let payload = serde_json::to_vec(&batch)
            .map_err(|e| BusError::Publish(format!("encode flow batch: {e}")))?;
        self.client
            .publish(subjects::flows(), payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish flow batch: {e}")))
    }

    async fn publish_raw_flow(&self, datagram: RawFlowDatagram) -> Result<(), BusError> {
        let payload = serde_json::to_vec(&datagram)
            .map_err(|e| BusError::Publish(format!("encode raw flow datagram: {e}")))?;
        self.client
            .publish(subjects::flows_raw(), payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish raw flow datagram: {e}")))
    }

    fn is_connected(&self) -> bool {
        // async-nats reconnects transparently; this is the only app-visible signal of a live link,
        // and it's what the poller's store-and-forward sink gates buffering on (Phase 3). `Pending`
        // (initial connect / mid-reconnect) counts as not-connected so we buffer rather than trust a
        // publish that would sit in async-nats's pending buffer and be lost on a longer outage.
        self.client.connection_state() == async_nats::connection::State::Connected
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

#[async_trait]
impl DiscoveryBus for NatsBus {
    async fn publish_discovery_job(&self, job: DiscoveryJob) -> Result<(), BusError> {
        let payload = serde_json::to_vec(&job)
            .map_err(|e| BusError::Publish(format!("encode discovery job: {e}")))?;
        self.client
            .publish(subjects::discovery_jobs(), payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish discovery job: {e}")))
    }

    async fn publish_discovery_job_for_pool(
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

    async fn publish_discovery_result(&self, result: DiscoveryResult) -> Result<(), BusError> {
        let payload = serde_json::to_vec(&result)
            .map_err(|e| BusError::Publish(format!("encode discovery result: {e}")))?;
        self.client
            .publish(subjects::discovery_results(), payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish discovery result: {e}")))
    }
}

#[async_trait]
impl PeerBus for NatsBus {
    async fn publish_auth_revoke(&self, msg: AuthRevoke) -> Result<(), BusError> {
        let payload = serde_json::to_vec(&msg)
            .map_err(|e| BusError::Publish(format!("encode auth revoke: {e}")))?;
        self.client
            .publish(subjects::auth_revoke(), payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish auth revoke: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::{redact_url, split_userinfo_password};

    #[test]
    fn splits_userinfo_password_from_url() {
        // The remote form: password extracted, userinfo stripped so an explicit username can win.
        assert_eq!(
            split_userinfo_password("tls://poller:s3cret@nats.example.com:4222"),
            (
                "tls://nats.example.com:4222".to_owned(),
                Some("s3cret".to_owned())
            )
        );
        // Single-node plaintext: no userinfo, no password, URL unchanged.
        assert_eq!(
            split_userinfo_password("nats://nats:4222"),
            ("nats://nats:4222".to_owned(), None)
        );
        // User-only userinfo (no colon) → no password, still stripped.
        assert_eq!(
            split_userinfo_password("nats://user@host:4222"),
            ("nats://host:4222".to_owned(), None)
        );
        // A password containing ':' keeps everything after the first colon.
        assert_eq!(
            split_userinfo_password("tls://poller:a:b@host:4222"),
            ("tls://host:4222".to_owned(), Some("a:b".to_owned()))
        );
    }

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
