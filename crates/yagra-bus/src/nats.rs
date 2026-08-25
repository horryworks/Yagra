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

use crate::bus::{Bus, BusError, DiscoveryBus, LogBus, PeerBus, SyncBus, UpgradeBus};
use crate::messages::{
    AuthRevoke, DiscoveryCancel, DiscoveryJob, DiscoveryResult, EventMsg, FlowBatch, HeartbeatMsg,
    PollJob, PollResult, PollerLogChunk, PollerLogRequest, PollerUpgradeMsg, RawFlowDatagram,
    SyncMsg, SyncRequest,
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

/// Install `ring` as this **process's** default rustls crypto provider. Call it once, at startup.
///
/// 🚨 Both `ring` and `aws-lc-rs` end up enabled in this dependency graph (the workspace
/// `Cargo.toml` says so at length), so rustls installs no default of its own and **any library that
/// builds a `ClientConfig` on our behalf panics the first time it tries.** Every call site this
/// workspace owns names the provider explicitly instead — see `yagra-core/src/tls.rs` — but
/// `async_nats`'s `add_root_certificates` builds its own, and so does `lettre`'s SMTP TLS. Those
/// have no call site to fix, so the answer has to be process-wide.
///
/// Measured on 192.168.1.211, 2026-08-25 (ADR-065 Inc.5 bug 3): the first `tls://` bus URL this
/// product has ever run with put core into a crash loop **three seconds after the WebUI reported
/// the switch had succeeded**. Nothing before that point exercises this — the single-node bus is
/// plaintext, and every test builds an `InMemoryBus`.
///
/// ⚠️ Deliberately **not** solved by handing `async_nats` a config of ours through
/// `tls_client_config`: with that set it also loads the platform certificate store and fails the
/// connection if the store errors, which is a new way for the bus to not come up. Installing the
/// provider leaves its existing behaviour — pin the given CA, ignore system roots — exactly as it
/// was.
///
/// Idempotent, and silent when a provider is already installed: both binaries call it and a second
/// install is not a condition worth reporting.
pub fn install_tls_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

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
    let (clean, _, password) = split_userinfo(url);
    (clean, password)
}

/// Split a NATS URL into `(url_without_userinfo, user, password)`.
///
/// 🚨 **The credentials in a NATS URL are ours to apply, not the client library's.** `async_nats`
/// 0.49 builds its `CONNECT` from `ConnectOptions` alone — `ServerAddr::username()` and
/// `password()` exist and the connector never calls them. So a URL of the form
/// `tls://core:secret@nats:4222` handed straight to `connect()` authenticates as **nobody**, and
/// the server answers `authentication error`. Measured on 192.168.1.211, 2026-08-25: core
/// retried that forever against a bus whose password it was holding all along.
///
/// This is the single parser both callers share, so "where does the userinfo end" has one answer
/// and the redaction in [`redact_url`] cannot disagree with what was actually sent.
pub fn split_userinfo(url: &str) -> (String, Option<String>, Option<String>) {
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
    let (user, password) = match userinfo {
        // A `user:pass` pair. Split at the FIRST colon: the username cannot contain one, and a
        // password may (`redact_url`'s tests carry `poller:a:b@host`).
        Some(ui) => match ui.split_once(':') {
            Some((u, p)) => (Some(u.to_owned()), Some(p.to_owned())),
            // Userinfo with no colon is a bare username, which NATS treats as a token. Reported so
            // the caller can decide; no call site uses it today.
            None => (Some(ui.to_owned()), None),
        },
        None => (None, None),
    };
    let clean = match scheme {
        Some(s) => format!("{s}://{host}{tail}"),
        None => format!("{host}{tail}"),
    };
    (clean, user, password)
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
    /// mandatory (security.md / ADR-020). Authentication is written in the URL
    /// (`nats://user:pass@host`) and **applied here**: see [`split_userinfo`] for why the URL alone
    /// is not enough. With `ca_file = None` and no userinfo this is the plaintext single-node
    /// connection, which presents no credential at all.
    pub async fn connect_opts(
        url: &str,
        ca_file: Option<&std::path::Path>,
    ) -> Result<Self, BusError> {
        let mut opts = async_nats::ConnectOptions::new();
        if let Some(ca) = ca_file {
            opts = opts.add_root_certificates(ca.to_path_buf());
        }
        let (clean_url, user, password) = split_userinfo(url);
        if let (Some(u), Some(p)) = (user, password) {
            opts = opts.user_and_password(u, p);
        }
        let client = opts
            .connect(&clean_url)
            .await
            .map_err(|e| BusError::Publish(format!("nats connect {}: {e}", redact_url(url))))?;
        tracing::info!(url = %redact_url(url), tls = ca_file.is_some(), "connected to NATS bus");
        Ok(Self {
            client,
            job_subject: subjects::jobs_for_pool(DEFAULT_POOL),
        })
    }

    /// Connect as `username`, with `pool` as the connection name. The password (bootstrap secret)
    /// comes from the URL userinfo and is re-supplied explicitly, and the userinfo is stripped so
    /// the explicit credential is the one that travels. On the plaintext single-node bus the URL
    /// has no userinfo, so no credential is presented — behaviour matches [`Self::connect_opts`].
    ///
    /// 🚨 **`username` is a deployment decision and the caller owns it**, because the two shipped
    /// bus configurations want opposite answers:
    ///
    /// * **Auth Callout on (ADR-030)** — the username must be the poller's own id: that is what
    ///   core's callout scopes `yagra.poller.assign.{id}` to, and `nats-server.conf`'s static
    ///   `poller` account is deliberately bypassed.
    /// * **Auth Callout off — the shipped default** — the username must be the literal `poller`
    ///   from the URL, because the static account is the only thing authorizing anything.
    ///
    /// This took the poller's id unconditionally until 2026-08-25, so on the default configuration
    /// it presented its **container hostname** and NATS answered `authentication error - User
    /// "4aeea2381430"` forever (measured on 192.168.1.211, ADR-065 Inc.5 bug 8). The configuration
    /// file said what should happen — "this static account remains the fallback when callout is
    /// off" — and nothing on this side had ever been told which mode it was in.
    pub async fn connect_opts_identified(
        url: &str,
        ca_file: Option<&std::path::Path>,
        username: &str,
        pool: &str,
    ) -> Result<Self, BusError> {
        let mut opts = async_nats::ConnectOptions::new().name(pool.to_owned());
        if let Some(ca) = ca_file {
            opts = opts.add_root_certificates(ca.to_path_buf());
        }
        let (clean_url, password) = split_userinfo_password(url);
        if let Some(pass) = password {
            opts = opts.user_and_password(username.to_owned(), pass);
        }
        let client = opts
            .connect(&clean_url)
            .await
            .map_err(|e| BusError::Publish(format!("nats connect {}: {e}", redact_url(url))))?;
        tracing::info!(
            url = %redact_url(url),
            tls = ca_file.is_some(),
            // The name presented to the broker, which is the poller's id only under Auth Callout.
            // Logged as what was actually sent, because "authentication error - User \"…\"" is the
            // only other place it appears and it is on the server's side of the wire.
            user = %username,
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

    /// Subscribe to this poller's upgrade commands — poller side (ADR-051). A plain subscribe on a
    /// subject addressed to one poller, like the assignment stream and for the same reason.
    ///
    /// **A build that does not call this receives nothing**, which is the entire N-1 story for the
    /// family: an older poller is not told to upgrade, so it stays put and keeps polling. Malformed
    /// messages are skipped, as everywhere else.
    pub async fn subscribe_poller_upgrades(
        &self,
        poller_id: &str,
    ) -> Result<impl Stream<Item = PollerUpgradeMsg>, BusError> {
        let sub = self
            .client
            .subscribe(subjects::upgrade_for(poller_id))
            .await
            .map_err(|e| BusError::Publish(format!("subscribe poller upgrades: {e}")))?;
        Ok(sub.filter_map(|msg| async move {
            match serde_json::from_slice::<PollerUpgradeMsg>(&msg.payload) {
                Ok(m) => Some(m),
                Err(e) => {
                    tracing::warn!(error = %e, "dropping malformed PollerUpgradeMsg from bus");
                    None
                }
            }
        }))
    }

    /// Subscribe to this poller's support-log requests — poller side (ADR-045 Inc.4). A plain
    /// subscribe on a subject addressed to one poller, like the upgrade stream and for the same
    /// reason.
    ///
    /// **A build that does not call this receives nothing**, which is the whole N-1 story for the
    /// family: an older poller is never asked, so core's deadline expires and the bundle records
    /// that site as unrepresented rather than mis-parsing a request it half-understands.
    pub async fn subscribe_poller_log_requests(
        &self,
        poller_id: &str,
    ) -> Result<impl Stream<Item = PollerLogRequest>, BusError> {
        let sub = self
            .client
            .subscribe(subjects::poller_logs_for(poller_id))
            .await
            .map_err(|e| BusError::Publish(format!("subscribe poller log requests: {e}")))?;
        Ok(sub.filter_map(|msg| async move {
            match serde_json::from_slice::<PollerLogRequest>(&msg.payload) {
                Ok(m) => Some(m),
                Err(e) => {
                    tracing::warn!(error = %e, "dropping malformed PollerLogRequest from bus");
                    None
                }
            }
        }))
    }

    /// Subscribe to support-log chunks from every poller — core side (ADR-045 Inc.4). One fan-in
    /// subject; the consumer routes by `request_id`, so a chunk for a request that has already
    /// timed out is dropped rather than mis-attributed.
    pub async fn subscribe_poller_log_chunks(
        &self,
    ) -> Result<impl Stream<Item = PollerLogChunk>, BusError> {
        let sub = self
            .client
            .subscribe(subjects::poller_log_reply())
            .await
            .map_err(|e| BusError::Publish(format!("subscribe poller log chunks: {e}")))?;
        Ok(sub.filter_map(|msg| async move {
            match serde_json::from_slice::<PollerLogChunk>(&msg.payload) {
                Ok(m) => Some(m),
                Err(e) => {
                    tracing::warn!(error = %e, "dropping malformed PollerLogChunk from bus");
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

    /// Subscribe to stop-this-sweep commands — poller side (ADR-068 Inc.2).
    ///
    /// **No queue group, unlike the jobs subscriptions above.** A job must go to exactly one poller;
    /// a stop must reach whichever poller took that job, and core does not know which one that was.
    /// So every poller hears every stop and the `scan_id` decides who acts.
    ///
    /// `pool` subscribes the pool-scoped subject; the caller pairs this with a second subscription
    /// on the global one, because a sweep is cancelled on whichever route it was published on.
    pub async fn subscribe_discovery_cancels(
        &self,
        pool: Option<&str>,
    ) -> Result<impl Stream<Item = DiscoveryCancel>, BusError> {
        let subject = pool.map_or_else(subjects::discovery_cancel, |p| {
            subjects::discovery_cancel_for_pool(p)
        });
        let sub = self
            .client
            .subscribe(subject)
            .await
            .map_err(|e| BusError::Publish(format!("subscribe discovery cancels: {e}")))?;
        Ok(sub.filter_map(|msg| async move {
            match serde_json::from_slice::<DiscoveryCancel>(&msg.payload) {
                Ok(c) => Some(c),
                Err(e) => {
                    tracing::warn!(error = %e, "dropping malformed DiscoveryCancel from bus");
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

    async fn flush(&self) -> Result<(), BusError> {
        self.client
            .flush()
            .await
            .map_err(|e| BusError::Publish(format!("flush: {e}")))
    }
}

#[async_trait]
impl UpgradeBus for NatsBus {
    async fn publish_poller_upgrade(&self, msg: PollerUpgradeMsg) -> Result<(), BusError> {
        let subject = subjects::upgrade_for(&msg.poller_id);
        let poller = msg.poller_id.clone();
        let payload = serde_json::to_vec(&msg)
            .map_err(|e| BusError::Publish(format!("encode poller upgrade: {e}")))?;
        self.client
            .publish(subject, payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish upgrade to {poller}: {e}")))
    }
}

#[async_trait]
impl LogBus for NatsBus {
    async fn publish_poller_log_request(&self, msg: PollerLogRequest) -> Result<(), BusError> {
        let subject = subjects::poller_logs_for(&msg.poller_id);
        let poller = msg.poller_id.clone();
        let payload = serde_json::to_vec(&msg)
            .map_err(|e| BusError::Publish(format!("encode poller log request: {e}")))?;
        self.client
            .publish(subject, payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish log request to {poller}: {e}")))
    }

    async fn publish_poller_log_chunk(&self, msg: PollerLogChunk) -> Result<(), BusError> {
        let payload = serde_json::to_vec(&msg)
            .map_err(|e| BusError::Publish(format!("encode poller log chunk: {e}")))?;
        self.client
            .publish(subjects::poller_log_reply(), payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish poller log chunk: {e}")))
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

    async fn publish_discovery_cancel(
        &self,
        pool: Option<&str>,
        msg: DiscoveryCancel,
    ) -> Result<(), BusError> {
        let payload = serde_json::to_vec(&msg)
            .map_err(|e| BusError::Publish(format!("encode discovery cancel: {e}")))?;
        // The route the sweep was published on, so a job that fell back to the global subject is
        // cancelled there — see `subjects::discovery_cancel_for_pool`.
        let subject = pool.map_or_else(subjects::discovery_cancel, |p| {
            subjects::discovery_cancel_for_pool(p)
        });
        self.client
            .publish(subject, payload.into())
            .await
            .map_err(|e| BusError::Publish(format!("publish discovery cancel: {e}")))
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
    use super::{install_tls_crypto_provider, redact_url, split_userinfo_password};

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

    /// The **username** has to come back out too, because `async_nats` will not read it from the
    /// URL and we have to hand it over ourselves.
    ///
    /// 🚨 The one that cost a day: `ServerAddr::username()`/`password()` exist in async-nats 0.49
    /// and the connector never calls them — `CONNECT` is built from `ConnectOptions` alone. So
    /// `connect(url_with_userinfo)` authenticates as nobody and the server answers
    /// `authentication error` while the caller is holding the right password.
    #[test]
    fn userinfo_yields_both_halves_and_survives_a_password_with_colons() {
        assert_eq!(
            super::split_userinfo("tls://core:s3cret@nats:4222"),
            (
                "tls://nats:4222".to_owned(),
                Some("core".to_owned()),
                Some("s3cret".to_owned())
            )
        );
        // The split is at the FIRST colon: a username cannot contain one, a password may.
        assert_eq!(
            super::split_userinfo("tls://poller:a:b@host:4222"),
            (
                "tls://host:4222".to_owned(),
                Some("poller".to_owned()),
                Some("a:b".to_owned())
            )
        );
        // The single-node plaintext bus presents nothing, and must not start presenting an
        // empty-string credential — NATS rejects that where it accepts no credential at all.
        assert_eq!(
            super::split_userinfo("nats://nats:4222"),
            ("nats://nats:4222".to_owned(), None, None)
        );
        // A bare username (NATS token auth) is reported as a user with no password, so the caller
        // can tell it apart from "no credentials" rather than silently dropping it.
        assert_eq!(
            super::split_userinfo("nats://token@host:4222"),
            (
                "nats://host:4222".to_owned(),
                Some("token".to_owned()),
                None
            )
        );
        // The two-value form stays exactly what its callers already read.
        assert_eq!(
            super::split_userinfo_password("tls://core:s3cret@nats:4222"),
            ("tls://nats:4222".to_owned(), Some("s3cret".to_owned()))
        );
    }

    /// The connect path must actually *apply* what it parsed, and only the source says so —
    /// standing up a NATS server with authorization is not something this suite can do.
    ///
    /// Reads the two connect functions and asserts each hands the credential to `ConnectOptions`.
    /// `connect_opts` had exactly this shape for its whole life and was wrong the whole time: its
    /// doc said "authentication rides in the URL, so there are no extra auth params", which is a
    /// true sentence about NATS and a false one about this client library.
    #[test]
    fn every_connect_path_hands_its_credentials_to_the_client() {
        let src = include_str!("nats.rs");
        let mut checked = 0usize;
        for f in [
            "pub async fn connect_opts(",
            "pub async fn connect_opts_identified(",
        ] {
            let start = src.find(f).unwrap_or_else(|| panic!("{f} is gone"));
            let body = &src[start..];
            let body = &body[..body.find("\n    }").expect("the function is terminated")];
            assert!(
                body.contains("user_and_password("),
                "{f} never calls `user_and_password`, so async-nats sends no credential and the \
                 server answers `authentication error`"
            );
            checked += 1;
        }
        assert_eq!(checked, 2, "both connect paths must be inspected");
    }

    /// A provider gets installed and installing twice is not an error.
    ///
    /// Weak on its own — the assertion that matters is the one below, that both binaries call it —
    /// but it is what proves the ⚠️ in the doc: `install_default()` returning `Err` because another
    /// provider is already there must not become a panic in a process that starts up twice under
    /// one test binary.
    #[test]
    fn installing_the_crypto_provider_is_idempotent() {
        install_tls_crypto_provider();
        install_tls_crypto_provider();
        assert!(
            rustls::crypto::CryptoProvider::get_default().is_some(),
            "no process-level provider after installing one, so every library that builds its own \
             client config still panics"
        );
    }

    /// **Both binaries must call it, and nothing but this notices if one stops.**
    ///
    /// The failure it guards is not a compile error and not a test failure: it is a crash loop on
    /// the one deployment shape that has a `tls://` bus, reported to the operator three seconds
    /// after the WebUI told them the change had succeeded. `yagra-poller` is the worse half — a
    /// remote site's poller is the whole reason the TLS bus exists, and nothing in this workspace
    /// runs one.
    ///
    /// Reads the two `main.rs` files directly rather than through `srcread`, because the claim is
    /// about the startup sequence and not about production text: a call inside a `#[cfg(test)]`
    /// block would be exactly the drift this refuses, and `srcread` would have removed it first.
    #[test]
    fn both_binaries_install_the_crypto_provider_at_startup() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/ is above this crate");
        let mut checked = 0usize;
        for bin in ["yagra-core", "yagra-poller"] {
            let path = root.join(bin).join("src").join("main.rs");
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
            assert!(
                src.contains("install_tls_crypto_provider()"),
                "{bin}'s startup does not install a rustls crypto provider, so the first `tls://` \
                 bus URL it is given panics on connect (ADR-065 Inc.5 bug 3)"
            );
            checked += 1;
        }
        assert_eq!(checked, 2, "both binaries must be inspected, not one");
    }
}
