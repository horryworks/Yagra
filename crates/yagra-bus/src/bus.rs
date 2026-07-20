// SPDX-License-Identifier: AGPL-3.0-only
//! The bus abstraction and an in-memory implementation.
//!
//! [`Bus`] is the publish seam core and pollers depend on, so neither calls the other
//! directly (ADR-003). [`InMemoryBus`] is a real, working implementation over Tokio
//! broadcast channels — used for tests and the single-process walking skeleton. A NATS
//! implementation (the production path) slots in behind the same trait later.

use crate::messages::{
    EventMsg, FlowBatch, HeartbeatMsg, PollJob, PollResult, SyncMsg, SyncRequest,
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
    // Control plane (ADR-009/020). `sync`/`pool_jobs` carry the routing key alongside the
    // message — the subscriber filters to its own poller id / pool, mirroring how the NATS
    // transport isolates them via per-poller / per-pool subjects.
    heartbeats: broadcast::Sender<HeartbeatMsg>,
    sync_requests: broadcast::Sender<SyncRequest>,
    sync: broadcast::Sender<(String, SyncMsg)>,
    pool_jobs: broadcast::Sender<(String, PollJob)>,
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
        let (heartbeats, _) = broadcast::channel(capacity);
        let (sync_requests, _) = broadcast::channel(capacity);
        let (sync, _) = broadcast::channel(capacity);
        let (pool_jobs, _) = broadcast::channel(capacity);
        Self {
            jobs,
            results,
            results_backfill,
            events,
            flows,
            heartbeats,
            sync_requests,
            sync,
            pool_jobs,
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
            sys_descr: None,
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
            schema_version: 1,
            job_id: Uuid::nil(),
            node_id: NodeId::from(Uuid::nil()),
            at_unix_ms: 0,
            outcome: CheckOutcome::Reachable,
            samples: vec![Sample::gauge("icmp_rtt_ms", 7.0)],
            interfaces: Vec::new(),
            sys_descr: None,
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
        use crate::messages::{EventKind, EventMsg, BUS_SCHEMA_VERSION};

        let bus = InMemoryBus::new(8);
        let mut rx = bus.subscribe_events();

        let event = EventMsg {
            schema_version: BUS_SCHEMA_VERSION,
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
        };
        bus.publish_event(event.clone()).await.unwrap();

        assert_eq!(rx.recv().await.unwrap(), event);
    }

    #[tokio::test]
    async fn published_flow_batch_reaches_subscriber() {
        use crate::messages::{FlowBatch, FlowRecord, BUS_SCHEMA_VERSION};

        let bus = InMemoryBus::new(8);
        let mut rx = bus.subscribe_flows();

        let batch = FlowBatch {
            schema_version: BUS_SCHEMA_VERSION,
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
            schema_version: 1,
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
            schema_version: 1,
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
            schema_version: 1,
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
}
