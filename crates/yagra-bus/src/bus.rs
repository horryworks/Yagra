// SPDX-License-Identifier: AGPL-3.0-only
//! The bus abstraction and an in-memory implementation.
//!
//! [`Bus`] is the publish seam core and pollers depend on, so neither calls the other
//! directly (ADR-003). [`InMemoryBus`] is a real, working implementation over Tokio
//! broadcast channels — used for tests and the single-process walking skeleton. A NATS
//! implementation (the production path) slots in behind the same trait later.

use crate::messages::{
    AuthRevoke, DiscoveryJob, DiscoveryResult, EventMsg, FlowBatch, HeartbeatMsg, PollJob,
    PollResult, RawFlowDatagram, SyncMsg, SyncRequest,
};
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
    /// Replay a **buffered** poll result on the store-and-forward subject after a partition heals
    /// (Phase 3). Core imports these to the TSDB at their original timestamp but skips alert
    /// evaluation (see [`crate::subjects::results_backfill`]). Defaulted to plain
    /// [`Bus::publish_result`] so an implementation that has no separate channel still works; the
    /// real transports override it.
    async fn publish_result_backfill(&self, result: PollResult) -> Result<(), BusError> {
        self.publish_result(result).await
    }
    /// Publish a passive event (syslog/trap/webhook) for core to consume.
    async fn publish_event(&self, event: EventMsg) -> Result<(), BusError>;
    /// Publish an edge-aggregated flow batch (NetFlow/IPFIX/sFlow) for core to write to ClickHouse
    /// (ADR-031). Best-effort, loss-tolerant tier: a failed publish is dropped, not buffered.
    /// Defaulted to a no-op `Ok` so an implementation without a flow channel still compiles; the
    /// real transports override it.
    async fn publish_flows(&self, _batch: FlowBatch) -> Result<(), BusError> {
        Ok(())
    }
    /// Relay one received flow-export datagram **verbatim** for core's forwarder (ADR-034
    /// Increment 2). Separate from [`Bus::publish_flows`] because the aggregate batch is lossy by
    /// construction and cannot reproduce what the exporter sent. Same best-effort contract: a failed
    /// publish is dropped, not buffered. Defaulted to a no-op `Ok` so an implementation without the
    /// channel still compiles; the real transports override it.
    async fn publish_raw_flow(&self, _datagram: RawFlowDatagram) -> Result<(), BusError> {
        Ok(())
    }
    /// Whether the bus is currently connected to its broker. The poller's store-and-forward sink
    /// uses this to decide between live publish and local buffering (Phase 3). An in-process bus is
    /// always connected; the NATS transport reflects its live connection state.
    fn is_connected(&self) -> bool {
        true
    }
}

/// The working-set / control-plane side of the bus (ADR-009/020).
///
/// Kept separate from the per-job [`Bus`] seam so the distributed-poller coordinator can be
/// unit-tested over [`InMemoryBus`] without a running NATS. Core publishes working-set syncs
/// (snapshot chunks / deltas) addressed to a poller and pool-scoped jobs; pollers publish
/// heartbeats and snapshot requests. Ownership mirrors [`Bus`] (owned messages in) so the two
/// traits read as siblings.
#[async_trait]
pub trait SyncBus: Send + Sync {
    /// Publish a working-set sync (snapshot chunk or delta) addressed to one poller.
    async fn publish_sync(&self, poller_id: &str, msg: SyncMsg) -> Result<(), BusError>;
    /// Publish a poller liveness/telemetry heartbeat.
    async fn publish_heartbeat(&self, hb: HeartbeatMsg) -> Result<(), BusError>;
    /// Publish a poller's request for a fresh snapshot.
    async fn publish_sync_request(&self, req: SyncRequest) -> Result<(), BusError>;
    /// Publish a poll job to a specific pool's subject (per-job fallback / poll-now / legacy).
    async fn publish_job_for_pool(&self, pool: &str, job: PollJob) -> Result<(), BusError>;
}

/// The discovery-sweep side of the bus (Phase C).
///
/// A third seam rather than more methods on [`Bus`] because the two ends are different code:
/// core's `DiscoveryRunner` only publishes jobs, the poller's sweep only publishes results, and
/// neither wants the poll-job surface. Keeping it separate is what lets each take
/// `Arc<dyn DiscoveryBus>` and be driven by [`InMemoryBus`] in a test — the reason these four
/// publishes are on a trait at all (they lived on the NATS type, so both call sites named the
/// production transport and neither was testable).
///
/// The pool split mirrors [`Bus::publish_job`] / [`SyncBus::publish_job_for_pool`] exactly: the
/// global subject is the compatibility path, the per-pool subject routes a sweep to a remote site
/// (ADR-009/020).
#[async_trait]
pub trait DiscoveryBus: Send + Sync {
    /// Publish a discovery sweep job on the global subject — core side.
    async fn publish_discovery_job(&self, job: DiscoveryJob) -> Result<(), BusError>;
    /// Publish a discovery sweep job to a specific pool's subject — core side.
    async fn publish_discovery_job_for_pool(
        &self,
        pool: &str,
        job: DiscoveryJob,
    ) -> Result<(), BusError>;
    /// Publish a (cumulative, possibly partial) sweep result — poller side.
    async fn publish_discovery_result(&self, result: DiscoveryResult) -> Result<(), BusError>;
}

/// The core⇄core fan-out (Core HA active/active, ADR-016 Increment 2a).
///
/// Not part of [`Bus`]: no poller publishes or consumes these, and a poller should not be handed a
/// seam that can revoke sessions. Separate from [`SyncBus`] for the same reason — that one is the
/// poller control plane.
#[async_trait]
pub trait PeerBus: Send + Sync {
    /// Fan out a session revocation so every other core denies the token too. Best-effort live
    /// propagation: the durable `auth_revocations` table is the source of truth, so a failed
    /// publish delays a peer's denial until its next cold load rather than losing it.
    async fn publish_auth_revoke(&self, msg: AuthRevoke) -> Result<(), BusError>;
}

/// An in-process bus over Tokio broadcast channels.
///
/// Fan-out, fire-and-forget: publishing with no current subscribers is **not** an error
/// (the message is simply dropped), matching a pub/sub bus's semantics.
pub struct InMemoryBus {
    jobs: broadcast::Sender<PollJob>,
    results: broadcast::Sender<PollResult>,
    // Store-and-forward replay (Phase 3) — a *separate* channel from `results` so an in-memory
    // consumer can assert backfill routing independently of the live path, mirroring how the NATS
    // transport isolates the two on distinct subjects.
    results_backfill: broadcast::Sender<PollResult>,
    events: broadcast::Sender<EventMsg>,
    // Edge-aggregated flow batches (Phase 3, ADR-031) — a separate channel from `events` mirroring
    // how the NATS transport isolates flow on its own `yagra.flows` subject.
    flows: broadcast::Sender<FlowBatch>,
    // Verbatim flow datagrams for forwarding (ADR-034 Increment 2) — again a separate channel,
    // mirroring the separate `yagra.flows.raw` subject, so a test can assert that the aggregate and
    // the raw relay never cross.
    raw_flows: broadcast::Sender<RawFlowDatagram>,
    // Control plane (ADR-009/020). `sync`/`pool_jobs` carry the routing key alongside the
    // message — the subscriber filters to its own poller id / pool, mirroring how the NATS
    // transport isolates them via per-poller / per-pool subjects.
    heartbeats: broadcast::Sender<HeartbeatMsg>,
    sync_requests: broadcast::Sender<SyncRequest>,
    sync: broadcast::Sender<(String, SyncMsg)>,
    pool_jobs: broadcast::Sender<(String, PollJob)>,
    // Discovery (Phase C). `pool_discovery_jobs` carries its routing key alongside the message,
    // exactly like `pool_jobs`.
    discovery_jobs: broadcast::Sender<DiscoveryJob>,
    pool_discovery_jobs: broadcast::Sender<(String, DiscoveryJob)>,
    discovery_results: broadcast::Sender<DiscoveryResult>,
    // Core⇄core session revocation (ADR-016 Increment 2a).
    auth_revokes: broadcast::Sender<AuthRevoke>,
}

impl InMemoryBus {
    /// Create a bus with the given per-channel buffer capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (jobs, _) = broadcast::channel(capacity);
        let (results, _) = broadcast::channel(capacity);
        let (results_backfill, _) = broadcast::channel(capacity);
        let (events, _) = broadcast::channel(capacity);
        let (flows, _) = broadcast::channel(capacity);
        let (raw_flows, _) = broadcast::channel(capacity);
        let (heartbeats, _) = broadcast::channel(capacity);
        let (sync_requests, _) = broadcast::channel(capacity);
        let (sync, _) = broadcast::channel(capacity);
        let (pool_jobs, _) = broadcast::channel(capacity);
        let (discovery_jobs, _) = broadcast::channel(capacity);
        let (pool_discovery_jobs, _) = broadcast::channel(capacity);
        let (discovery_results, _) = broadcast::channel(capacity);
        let (auth_revokes, _) = broadcast::channel(capacity);
        Self {
            jobs,
            results,
            results_backfill,
            events,
            flows,
            raw_flows,
            heartbeats,
            sync_requests,
            sync,
            pool_jobs,
            discovery_jobs,
            pool_discovery_jobs,
            discovery_results,
            auth_revokes,
        }
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

    /// Subscribe to **backfilled** poll results (core side, store-and-forward). Kept on its own
    /// channel so the backfill consumer (metrics-only, no alert eval) is wired independently of the
    /// live results consumer.
    #[must_use]
    pub fn subscribe_results_backfill(&self) -> broadcast::Receiver<PollResult> {
        self.results_backfill.subscribe()
    }

    /// Subscribe to passive events (core side).
    #[must_use]
    pub fn subscribe_events(&self) -> broadcast::Receiver<EventMsg> {
        self.events.subscribe()
    }

    /// Subscribe to edge-aggregated flow batches (core side, ADR-031).
    #[must_use]
    pub fn subscribe_flows(&self) -> broadcast::Receiver<FlowBatch> {
        self.flows.subscribe()
    }

    /// Subscribe to verbatim flow datagrams (core side, ADR-034 Increment 2).
    #[must_use]
    pub fn subscribe_raw_flows(&self) -> broadcast::Receiver<RawFlowDatagram> {
        self.raw_flows.subscribe()
    }

    /// Subscribe to poller heartbeats (core side).
    #[must_use]
    pub fn subscribe_heartbeats(&self) -> broadcast::Receiver<HeartbeatMsg> {
        self.heartbeats.subscribe()
    }

    /// Subscribe to poller snapshot requests (core side).
    #[must_use]
    pub fn subscribe_sync_requests(&self) -> broadcast::Receiver<SyncRequest> {
        self.sync_requests.subscribe()
    }

    /// Subscribe to working-set syncs (poller side). Yields `(target_poller_id, msg)`; the caller
    /// keeps only the tuples whose id matches its own (the NATS transport does this via the
    /// per-poller assignment subject).
    #[must_use]
    pub fn subscribe_sync(&self) -> broadcast::Receiver<(String, SyncMsg)> {
        self.sync.subscribe()
    }

    /// Subscribe to pool-scoped jobs (poller side). Yields `(pool, job)`; the caller keeps only
    /// the tuples for its own pool (the NATS transport does this via the per-pool job subject).
    #[must_use]
    pub fn subscribe_pool_jobs(&self) -> broadcast::Receiver<(String, PollJob)> {
        self.pool_jobs.subscribe()
    }

    /// Subscribe to global-subject discovery jobs (poller side).
    #[must_use]
    pub fn subscribe_discovery_jobs(&self) -> broadcast::Receiver<DiscoveryJob> {
        self.discovery_jobs.subscribe()
    }

    /// Subscribe to pool-scoped discovery jobs (poller side). Yields `(pool, job)`; the caller
    /// keeps only the tuples for its own pool, as with [`Self::subscribe_pool_jobs`].
    #[must_use]
    pub fn subscribe_pool_discovery_jobs(&self) -> broadcast::Receiver<(String, DiscoveryJob)> {
        self.pool_discovery_jobs.subscribe()
    }

    /// Subscribe to discovery sweep results (core side).
    #[must_use]
    pub fn subscribe_discovery_results(&self) -> broadcast::Receiver<DiscoveryResult> {
        self.discovery_results.subscribe()
    }

    /// Subscribe to session revocations fanned out by other cores.
    #[must_use]
    pub fn subscribe_auth_revoke(&self) -> broadcast::Receiver<AuthRevoke> {
        self.auth_revokes.subscribe()
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

    async fn publish_result_backfill(&self, result: PollResult) -> Result<(), BusError> {
        let _ = self.results_backfill.send(result);
        Ok(())
    }

    async fn publish_event(&self, event: EventMsg) -> Result<(), BusError> {
        let _ = self.events.send(event);
        Ok(())
    }

    async fn publish_flows(&self, batch: FlowBatch) -> Result<(), BusError> {
        let _ = self.flows.send(batch);
        Ok(())
    }

    async fn publish_raw_flow(&self, datagram: RawFlowDatagram) -> Result<(), BusError> {
        let _ = self.raw_flows.send(datagram);
        Ok(())
    }
}

#[async_trait]
impl SyncBus for InMemoryBus {
    async fn publish_sync(&self, poller_id: &str, msg: SyncMsg) -> Result<(), BusError> {
        // broadcast::send only errors when there are no receivers — fine for pub/sub.
        let _ = self.sync.send((poller_id.to_owned(), msg));
        Ok(())
    }

    async fn publish_heartbeat(&self, hb: HeartbeatMsg) -> Result<(), BusError> {
        let _ = self.heartbeats.send(hb);
        Ok(())
    }

    async fn publish_sync_request(&self, req: SyncRequest) -> Result<(), BusError> {
        let _ = self.sync_requests.send(req);
        Ok(())
    }

    async fn publish_job_for_pool(&self, pool: &str, job: PollJob) -> Result<(), BusError> {
        let _ = self.pool_jobs.send((pool.to_owned(), job));
        Ok(())
    }
}

#[async_trait]
impl DiscoveryBus for InMemoryBus {
    async fn publish_discovery_job(&self, job: DiscoveryJob) -> Result<(), BusError> {
        let _ = self.discovery_jobs.send(job);
        Ok(())
    }

    async fn publish_discovery_job_for_pool(
        &self,
        pool: &str,
        job: DiscoveryJob,
    ) -> Result<(), BusError> {
        let _ = self.pool_discovery_jobs.send((pool.to_owned(), job));
        Ok(())
    }

    async fn publish_discovery_result(&self, result: DiscoveryResult) -> Result<(), BusError> {
        let _ = self.discovery_results.send(result);
        Ok(())
    }
}

#[async_trait]
impl PeerBus for InMemoryBus {
    async fn publish_auth_revoke(&self, msg: AuthRevoke) -> Result<(), BusError> {
        let _ = self.auth_revokes.send(msg);
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
            job_id: Uuid::nil(),
            node_id: NodeId::from(Uuid::nil()),
            at_unix_ms: 0,
            outcome: CheckOutcome::Reachable,
            samples: vec![Sample::gauge("icmp_rtt_ms", 12.5)],
            interfaces: Vec::new(),
            sys_descr: None,
            dns_chain: None,
            poller_id: None,
            trace_context: Default::default(),
        };
        bus.publish_result(result.clone()).await.unwrap();

        assert_eq!(rx.recv().await.unwrap(), result);
    }

    #[tokio::test]
    async fn backfill_result_reaches_only_the_backfill_subscriber() {
        // Store-and-forward: a backfilled result must land on the backfill channel and NOT on the
        // live results channel — that isolation is what keeps replayed samples out of alert eval.
        let bus = InMemoryBus::new(8);
        let mut live = bus.subscribe_results();
        let mut backfill = bus.subscribe_results_backfill();

        let result = PollResult {
            job_id: Uuid::nil(),
            node_id: NodeId::from(Uuid::nil()),
            at_unix_ms: 0,
            outcome: CheckOutcome::Reachable,
            samples: vec![Sample::gauge("icmp_rtt_ms", 7.0)],
            interfaces: Vec::new(),
            sys_descr: None,
            dns_chain: None,
            poller_id: None,
            trace_context: Default::default(),
        };
        bus.publish_result_backfill(result.clone()).await.unwrap();

        assert_eq!(backfill.recv().await.unwrap(), result);
        // The live consumer must see nothing on its channel.
        assert!(live.try_recv().is_err());
        // An in-process bus is always "connected".
        assert!(Bus::is_connected(&bus));
    }

    #[tokio::test]
    async fn published_event_reaches_subscriber() {
        use crate::messages::{EventKind, EventMsg};

        let bus = InMemoryBus::new(8);
        let mut rx = bus.subscribe_events();

        let event = EventMsg {
            event_id: Uuid::nil(),
            kind: EventKind::Syslog,
            at_unix_ms: 0,
            source_ip: Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9))),
            pool: None,
            message: "link down".into(),
            facility: None,
            syslog_severity: None,
            hostname: None,
            app_name: None,
            trap_oid: None,
            varbinds: Vec::new(),
            truncated: false,
            raw: None,
            src_port: None,
        };
        bus.publish_event(event.clone()).await.unwrap();

        assert_eq!(rx.recv().await.unwrap(), event);
    }

    #[tokio::test]
    async fn published_flow_batch_reaches_subscriber() {
        use crate::messages::{FlowBatch, FlowRecord};

        let bus = InMemoryBus::new(8);
        let mut rx = bus.subscribe_flows();

        let batch = FlowBatch {
            poller_id: "edge-1".into(),
            pool: "default".into(),
            exporter_ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            bucket_start_ms: 60_000,
            bucket_secs: 60,
            records: vec![FlowRecord {
                src_ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)),
                dst_ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 3)),
                src_port: 12345,
                dst_port: 443,
                proto: 6,
                tos: 0,
                if_index: 2,
                src_as: 0,
                dst_as: 0,
                bytes: 4096,
                packets: 8,
                flows: 3,
            }],
            dropped: 0,
        };
        bus.publish_flows(batch.clone()).await.unwrap();

        assert_eq!(rx.recv().await.unwrap(), batch);
    }

    #[tokio::test]
    async fn raw_flow_datagram_reaches_only_the_raw_subscriber() {
        use crate::messages::{encode_raw, RawFlowDatagram, RawFlowProto};

        let bus = InMemoryBus::new(8);
        let mut raw = bus.subscribe_raw_flows();
        let mut aggregate = bus.subscribe_flows();

        let dg = RawFlowDatagram {
            poller_id: "edge-1".into(),
            pool: Some("tokyo".into()),
            exporter_ip: IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
            src_port: 51_234,
            proto: RawFlowProto::Netflow,
            at_unix_ms: 1_700_000_000_000,
            bytes: encode_raw(&[0x00, 0x09, 0xff]),
        };
        bus.publish_raw_flow(dg.clone()).await.unwrap();

        assert_eq!(raw.recv().await.unwrap(), dg);
        // The ClickHouse consumer must never see a raw datagram — that isolation mirrors the
        // separate `yagra.flows.raw` subject.
        assert!(aggregate.try_recv().is_err());
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

    #[tokio::test]
    async fn published_heartbeat_reaches_subscriber() {
        use crate::messages::HeartbeatMsg;

        let bus = InMemoryBus::new(8);
        let mut rx = bus.subscribe_heartbeats();

        let hb = HeartbeatMsg {
            poller_id: "edge-1".into(),
            pool: "default".into(),
            incarnation: Uuid::nil(),
            version: "0.1.0".into(),
            epoch: None,
            last_seq: 0,
            working_set_nodes: 0,
            working_set_specs: 0,
            inflight: 0,
            results_total: 0,
            listeners: Vec::new(),
            caps: Vec::new(),
            host: None,
        };
        SyncBus::publish_heartbeat(&bus, hb.clone()).await.unwrap();

        assert_eq!(rx.recv().await.unwrap(), hb);
    }

    #[tokio::test]
    async fn published_sync_request_reaches_subscriber() {
        use crate::messages::SyncRequest;

        let bus = InMemoryBus::new(8);
        let mut rx = bus.subscribe_sync_requests();

        let req = SyncRequest {
            poller_id: "edge-1".into(),
            pool: "default".into(),
            incarnation: Uuid::nil(),
        };
        SyncBus::publish_sync_request(&bus, req.clone())
            .await
            .unwrap();

        assert_eq!(rx.recv().await.unwrap(), req);
    }

    #[tokio::test]
    async fn published_sync_reaches_subscriber_with_routing_id() {
        use crate::messages::{SyncMsg, WorkingSetDelta};

        let bus = InMemoryBus::new(8);
        let mut rx = bus.subscribe_sync();

        let msg = SyncMsg::Delta(WorkingSetDelta {
            poller_id: "edge-1".into(),
            epoch: Uuid::nil(),
            seq: 1,
            upserts: Vec::new(),
            removes: Vec::new(),
        });
        SyncBus::publish_sync(&bus, "edge-1", msg.clone())
            .await
            .unwrap();

        let (target, got) = rx.recv().await.unwrap();
        assert_eq!(target, "edge-1"); // the subscriber filters on this
        assert_eq!(got, msg);
    }

    #[tokio::test]
    async fn published_pool_job_reaches_subscriber_with_pool_tag() {
        let bus = InMemoryBus::new(8);
        let mut rx = bus.subscribe_pool_jobs();

        let job = PollJob::icmp(
            Uuid::nil(),
            NodeId::from(Uuid::nil()),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IcmpCheck::default(),
            30,
        );
        SyncBus::publish_job_for_pool(&bus, "tokyo", job.clone())
            .await
            .unwrap();

        let (pool, got) = rx.recv().await.unwrap();
        assert_eq!(pool, "tokyo"); // the subscriber filters on this
        assert_eq!(got, job);
    }

    fn discovery_job() -> crate::messages::DiscoveryJob {
        crate::messages::DiscoveryJob {
            scan_id: Uuid::from_u128(3),
            targets: vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))],
            communities: vec!["public".into()],
            credentials: Vec::new(),
            timeout_ms: 500,
        }
    }

    /// The two discovery subjects must stay isolated for the same reason the poll-job ones do: a
    /// pool-scoped sweep is routed to one site's poller, and a wildcard poller absorbing it as well
    /// would run the scan twice from two networks.
    #[tokio::test]
    async fn a_pooled_discovery_job_does_not_reach_the_global_subscriber() {
        let bus = InMemoryBus::new(8);
        let mut global = bus.subscribe_discovery_jobs();
        let mut pooled = bus.subscribe_pool_discovery_jobs();

        let job = discovery_job();
        DiscoveryBus::publish_discovery_job_for_pool(&bus, "tokyo", job.clone())
            .await
            .unwrap();

        let (pool, got) = pooled.recv().await.unwrap();
        assert_eq!(pool, "tokyo"); // the subscriber filters on this
        assert_eq!(got, job);
        assert!(global.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_global_discovery_job_does_not_reach_the_pooled_subscriber() {
        let bus = InMemoryBus::new(8);
        let mut global = bus.subscribe_discovery_jobs();
        let mut pooled = bus.subscribe_pool_discovery_jobs();

        let job = discovery_job();
        DiscoveryBus::publish_discovery_job(&bus, job.clone())
            .await
            .unwrap();

        assert_eq!(global.recv().await.unwrap(), job);
        assert!(pooled.try_recv().is_err());
    }

    #[tokio::test]
    async fn published_discovery_result_reaches_subscriber() {
        use crate::messages::DiscoveryResult;

        let bus = InMemoryBus::new(8);
        let mut rx = bus.subscribe_discovery_results();

        let result = DiscoveryResult {
            scan_id: Uuid::from_u128(3),
            found: Vec::new(),
            probed: 1,
            total: 1,
            done: true,
        };
        DiscoveryBus::publish_discovery_result(&bus, result.clone())
            .await
            .unwrap();

        assert_eq!(rx.recv().await.unwrap(), result);
    }

    #[tokio::test]
    async fn published_auth_revoke_reaches_subscriber() {
        use crate::messages::AuthRevoke;

        let bus = InMemoryBus::new(8);
        let mut rx = bus.subscribe_auth_revoke();

        let msg = AuthRevoke::Token {
            hash: "0".repeat(64),
            exp_unix: 1_700_000_000,
        };
        PeerBus::publish_auth_revoke(&bus, msg.clone())
            .await
            .unwrap();

        assert_eq!(rx.recv().await.unwrap(), msg);
    }
}
