// SPDX-License-Identifier: AGPL-3.0-only
//! Flow-export UDP listener + edge aggregation (Phase 3, ADR-031).
//!
//! Sibling to the syslog/trap listeners ([`crate::listeners`]): N `SO_REUSEPORT` reader sockets on
//! an unprivileged port (`YAGRA_FLOW_BIND`, e.g. `0.0.0.0:2055`) receive NetFlow v9 / IPFIX
//! datagrams, rate-limit per source, and parse them with the pure `yagra-ingest` flow parser. The
//! decoded records are **not** published per-datagram — the raw flow tuple is extreme cardinality.
//! Instead they are folded into per-exporter top-N aggregates over a bucket window ([`FlowState`]),
//! and a separate flush ticker publishes one [`FlowBatch`] per exporter per bucket on `yagra.flows`.
//!
//! Alongside the aggregate, every accepted datagram is relayed **verbatim** on `yagra.flows.raw`
//! ([`RawFlowTee`], ADR-034 Increment 2) so core's forwarder can tee it to an external collector.
//! The aggregate cannot serve that purpose: bucketing, top-N truncation and 5-tuple folding are all
//! irreversible. The relay is unconditional — a capture toggle would make forwarding fidelity depend
//! on configuration — and hands off through a bounded channel so the receive loop never awaits a
//! publish.
//!
//! Best-effort, loss-tolerant tier (ADR-017): a publish failure is counted and dropped, never
//! buffered. Parsing shares the `yagra-ingest` never-panic contract; the socket setup, source rate
//! limiter, and timestamp helper are reused verbatim from [`crate::listeners`].

use crate::listeners::{allow, now_unix_ms};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use yagra_bus::{encode_raw, Bus, FlowBatch, FlowRecord, RawFlowDatagram, BUS_SCHEMA_VERSION};
use yagra_ingest::{
    parse_flow_export, parse_sflow, ExporterBuckets, FlowError, FlowTemplates, SourceLimiter,
};

/// Flow datagrams beyond this are rejected outright (a NetFlow/IPFIX export never approaches it).
const FLOW_BUF_BYTES: usize = 64 * 1024;

/// Hand-off depth between the reader sockets and the raw-datagram publisher. A datagram is ~1.5 kB,
/// so this bounds the relay's buffer at a few MB; past it the oldest-arriving surplus is dropped and
/// counted rather than stalling `recv_from` (which would drop datagrams in the kernel instead, with
/// no counter to show for it).
const RAW_FLOW_CHANNEL_CAP: usize = 4096;

/// Which flow-export wire format a listener socket speaks. NetFlow v5/v9 and IPFIX share the
/// template-based [`parse_flow_export`] path (and the :2055-style collector port); sFlow rides its
/// own datagram shape on its own port (:6343) via [`parse_sflow`]. Both decode to `RawFlow` and feed
/// the same per-exporter aggregator.
///
/// This is the bus type itself, not a parallel copy: the value the listener was configured with is
/// the value that rides `yagra.flows.raw`, so core decodes with the parser the receiver actually
/// used and the two can never drift.
pub use yagra_bus::RawFlowProto as FlowProto;

/// Shared, mutation-only edge state for the flow listeners: the template cache (learned from
/// template FlowSets) and the per-exporter top-N aggregators. Held behind a `std::sync::Mutex`; the
/// critical section is pure parse+fold arithmetic and is never held across an `.await` (S22).
pub struct FlowState {
    templates: FlowTemplates,
    buckets: ExporterBuckets,
}

impl FlowState {
    /// New state keeping `top_n` flows per exporter per bucket.
    #[must_use]
    pub fn new(top_n: usize) -> Self {
        Self {
            templates: FlowTemplates::new(),
            buckets: ExporterBuckets::new(top_n),
        }
    }
}

/// One received datagram queued for the raw relay. Kept as bytes so the receive loop's only extra
/// work is a `Vec` copy — base64 encoding and message assembly happen on the publisher task.
struct RawFlowItem {
    exporter: IpAddr,
    src_port: u16,
    proto: FlowProto,
    at_unix_ms: i64,
    bytes: Vec<u8>,
}

/// The verbatim-relay inlet handed to each flow reader socket (ADR-034 Increment 2).
/// [`Self::offer`] never blocks, so a slow bus can never back-pressure datagram reception.
pub struct RawFlowTee {
    tx: mpsc::Sender<RawFlowItem>,
}

impl RawFlowTee {
    /// Queue one received datagram for relay. A full queue drops it and counts it — flow is a
    /// best-effort tier, and losing a forwarded copy must never cost a received flow record.
    fn offer(&self, exporter: IpAddr, src_port: u16, proto: FlowProto, bytes: &[u8]) {
        let item = RawFlowItem {
            exporter,
            src_port,
            proto,
            at_unix_ms: now_unix_ms(),
            bytes: bytes.to_vec(),
        };
        if self.tx.try_send(item).is_err() {
            metrics::counter!("yagra_flow_raw_dropped_total", "reason" => "channel_full")
                .increment(1);
        }
    }
}

/// The publisher half of the raw relay: the queue plus the identity stamped onto every datagram.
/// Owned by the task [`run_raw_flow_relay`] runs on.
pub struct RawFlowRelay {
    rx: mpsc::Receiver<RawFlowItem>,
    poller_id: String,
    pool: Option<String>,
}

/// Build the raw-relay pair. The tee is shared across every flow reader socket; the relay is moved
/// into one [`run_raw_flow_relay`] task.
#[must_use]
pub fn raw_flow_tee(poller_id: String, pool: Option<String>) -> (Arc<RawFlowTee>, RawFlowRelay) {
    let (tx, rx) = mpsc::channel(RAW_FLOW_CHANNEL_CAP);
    (
        Arc::new(RawFlowTee { tx }),
        RawFlowRelay {
            rx,
            poller_id,
            pool,
        },
    )
}

/// Publish queued datagrams verbatim on `yagra.flows.raw`. Runs until the inlet closes; a publish
/// failure is counted and dropped (best-effort tier — never buffered, never retried).
pub async fn run_raw_flow_relay<B: Bus>(bus: Arc<B>, mut relay: RawFlowRelay) {
    while let Some(item) = relay.rx.recv().await {
        let bytes = item.bytes.len() as u64;
        let datagram = RawFlowDatagram {
            schema_version: BUS_SCHEMA_VERSION,
            poller_id: relay.poller_id.clone(),
            pool: relay.pool.clone(),
            exporter_ip: item.exporter,
            src_port: item.src_port,
            proto: item.proto,
            at_unix_ms: item.at_unix_ms,
            bytes: encode_raw(&item.bytes),
        };
        match bus.publish_raw_flow(datagram).await {
            Ok(()) => {
                metrics::counter!("yagra_flow_raw_relayed_total", "proto" => item.proto.as_str())
                    .increment(1);
                metrics::counter!("yagra_flow_raw_bytes_total").increment(bytes);
            }
            Err(e) => {
                metrics::counter!("yagra_flow_raw_dropped_total", "reason" => "publish_error")
                    .increment(1);
                tracing::debug!(exporter = %item.exporter, error = %e, "failed to relay raw flow datagram");
            }
        }
    }
}

/// Run one flow-listener reader loop until the socket errors persistently. Received datagrams are
/// rate-limited, parsed according to `proto` (NetFlow/IPFIX vs sFlow), and folded into `state`;
/// nothing is published here (the flush ticker does that). NetFlow and sFlow readers can share one
/// `state` — both decode to `RawFlow` and merge into the same per-exporter aggregator.
///
/// `tee` relays each accepted datagram verbatim for forwarding (ADR-034). `None` disables the relay,
/// which the tests use to exercise reception alone.
pub async fn run_flow_listener(
    sock: UdpSocket,
    state: Arc<Mutex<FlowState>>,
    limiter: Arc<Mutex<SourceLimiter>>,
    proto: FlowProto,
    tee: Option<Arc<RawFlowTee>>,
) {
    let mut buf = vec![0u8; FLOW_BUF_BYTES];
    loop {
        let (len, peer) = match sock.recv_from(&mut buf).await {
            Ok(ok) => ok,
            Err(e) => {
                tracing::warn!(error = %e, "flow listener recv error");
                continue;
            }
        };
        metrics::counter!("yagra_flow_datagrams_received_total", "proto" => proto.as_str())
            .increment(1);
        if len >= FLOW_BUF_BYTES {
            metrics::counter!("yagra_flow_datagrams_dropped_total", "reason" => "oversize")
                .increment(1);
            continue;
        }
        if !allow(&limiter, peer.ip()) {
            metrics::counter!("yagra_flow_datagrams_dropped_total", "reason" => "rate_limit")
                .increment(1);
            continue;
        }
        let exporter = peer.ip();
        // Parse + fold under one short sync lock; guard dropped at the end of this block so it is
        // never held across the metric/log calls (no await inside).
        let outcome = {
            let mut guard = state.lock().unwrap_or_else(PoisonError::into_inner);
            let FlowState { templates, buckets } = &mut *guard;
            let parsed = match proto {
                FlowProto::Netflow => parse_flow_export(templates, exporter, &buf[..len]),
                FlowProto::Sflow => parse_sflow(&buf[..len]),
            };
            match parsed {
                Ok(flows) => {
                    let n = flows.len();
                    for f in flows {
                        buckets.add(exporter, f);
                    }
                    Ok(n)
                }
                Err(e) => Err(e),
            }
        };
        match outcome {
            Ok(n) => {
                // Relay only what Yagra itself accepted, so a forwarding destination and the flow
                // tab agree on what was received. `n == 0` still relays: a NetFlow v9 / IPFIX
                // template set carries no records but is exactly what a downstream collector needs
                // to decode everything after it.
                if let Some(tee) = tee.as_ref() {
                    tee.offer(exporter, peer.port(), proto, &buf[..len]);
                }
                if n > 0 {
                    metrics::counter!("yagra_flow_records_total", "proto" => proto.as_str())
                        .increment(n as u64);
                }
            }
            Err(FlowError::UnsupportedVersion(_)) => {
                // A version this proto's parser doesn't handle (e.g. NetFlow v1, or a non-5 sFlow);
                // counted, not noisy.
                metrics::counter!("yagra_flow_datagrams_dropped_total", "reason" => "unsupported_version")
                    .increment(1);
            }
            Err(e) => {
                metrics::counter!("yagra_flow_datagrams_dropped_total", "reason" => "parse_error")
                    .increment(1);
                tracing::debug!(source = %exporter, error = %e, "dropping unparseable flow datagram");
            }
        }
    }
}

/// Flush the per-exporter aggregates every `bucket_secs`, publishing one [`FlowBatch`] per exporter
/// that had flows. Runs until cancelled.
pub async fn run_flow_flusher<B: Bus>(
    bus: Arc<B>,
    state: Arc<Mutex<FlowState>>,
    poller_id: String,
    pool: String,
    bucket_secs: u32,
) {
    let secs = bucket_secs.max(1);
    let mut ticker = tokio::time::interval(Duration::from_secs(u64::from(secs)));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await; // first tick is immediate — skip it (nothing accumulated yet)
    loop {
        ticker.tick().await;
        let (batches, dropped_exporters) = {
            let mut guard = state.lock().unwrap_or_else(PoisonError::into_inner);
            guard.buckets.drain()
        };
        if dropped_exporters > 0 {
            metrics::counter!("yagra_flow_records_dropped_total", "reason" => "exporter_cap")
                .increment(u64::from(dropped_exporters));
        }
        if batches.is_empty() {
            continue;
        }
        let bucket_start_ms = bucket_start_ms(secs);
        for eb in batches {
            let records: Vec<FlowRecord> = eb
                .records
                .into_iter()
                .map(|a| FlowRecord {
                    src_ip: a.src_ip,
                    dst_ip: a.dst_ip,
                    src_port: a.src_port,
                    dst_port: a.dst_port,
                    proto: a.proto,
                    tos: a.tos,
                    if_index: a.if_index,
                    src_as: a.src_as, // export-provided AS (0 = unknown); core enriches zeros
                    dst_as: a.dst_as,
                    bytes: a.bytes,
                    packets: a.packets,
                    flows: a.flows,
                })
                .collect();
            if eb.dropped > 0 {
                metrics::counter!("yagra_flow_records_dropped_total", "reason" => "topn")
                    .increment(u64::from(eb.dropped));
            }
            let batch = FlowBatch {
                schema_version: BUS_SCHEMA_VERSION,
                poller_id: poller_id.clone(),
                pool: pool.clone(),
                exporter_ip: eb.exporter,
                bucket_start_ms,
                bucket_secs: secs,
                records,
                dropped: eb.dropped,
            };
            publish_flow(bus.as_ref(), batch).await;
        }
    }
}

/// Start-of-bucket timestamp (ms) for the window being flushed: `now` floored to a `secs` boundary.
fn bucket_start_ms(secs: u32) -> i64 {
    let period_ms = i64::from(secs) * 1000;
    if period_ms <= 0 {
        return now_unix_ms();
    }
    (now_unix_ms() / period_ms) * period_ms
}

/// Publish one flow batch; failures are counted + logged, never fatal (best-effort tier).
async fn publish_flow<B: Bus>(bus: &B, batch: FlowBatch) {
    let exporter = batch.exporter_ip;
    let records = batch.records.len();
    match bus.publish_flows(batch).await {
        Ok(()) => {
            tracing::debug!(%exporter, records, "flow batch published");
        }
        Err(e) => {
            metrics::counter!("yagra_flow_publish_failures_total").increment(1);
            tracing::warn!(%exporter, error = %e, "failed to publish flow batch");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use yagra_bus::InMemoryBus;

    fn limiter() -> Arc<Mutex<SourceLimiter>> {
        Arc::new(Mutex::new(SourceLimiter::new(500.0, 5000.0, 0)))
    }

    /// Build a minimal NetFlow v9 datagram with one template + one data record.
    fn nf9_one_record(src: Ipv4Addr, dst: Ipv4Addr, bytes: u32) -> Vec<u8> {
        // IPV4_SRC(8,4) IPV4_DST(12,4) L4_DST_PORT(11,2) PROTOCOL(4,1) IN_BYTES(1,4) DST_AS(17,4)
        let fields: [(u16, u16); 6] = [(8, 4), (12, 4), (11, 2), (4, 1), (1, 4), (17, 4)];
        let mut tmpl = Vec::new();
        tmpl.extend_from_slice(&256u16.to_be_bytes());
        tmpl.extend_from_slice(&(fields.len() as u16).to_be_bytes());
        for (ie, len) in fields {
            tmpl.extend_from_slice(&ie.to_be_bytes());
            tmpl.extend_from_slice(&len.to_be_bytes());
        }
        let mut tmpl_set = Vec::new();
        tmpl_set.extend_from_slice(&0u16.to_be_bytes());
        tmpl_set.extend_from_slice(&((4 + tmpl.len()) as u16).to_be_bytes());
        tmpl_set.extend_from_slice(&tmpl);

        let mut data = Vec::new();
        data.extend_from_slice(&src.octets());
        data.extend_from_slice(&dst.octets());
        data.extend_from_slice(&443u16.to_be_bytes());
        data.push(6);
        data.extend_from_slice(&bytes.to_be_bytes());
        data.extend_from_slice(&15169u32.to_be_bytes()); // dst_as (Google)
        let mut data_set = Vec::new();
        data_set.extend_from_slice(&256u16.to_be_bytes());
        data_set.extend_from_slice(&((4 + data.len()) as u16).to_be_bytes());
        data_set.extend_from_slice(&data);

        let mut pkt = Vec::new();
        pkt.extend_from_slice(&9u16.to_be_bytes());
        pkt.extend_from_slice(&1u16.to_be_bytes());
        pkt.extend_from_slice(&[0u8; 12]);
        pkt.extend_from_slice(&7u32.to_be_bytes());
        pkt.extend_from_slice(&tmpl_set);
        pkt.extend_from_slice(&data_set);
        pkt
    }

    /// End-to-end over a real UDP socket: NetFlow datagram in → FlowBatch on the bus after a flush.
    #[tokio::test]
    async fn netflow_datagram_becomes_flow_batch() {
        let bus = Arc::new(InMemoryBus::new(8));
        let mut flows = bus.subscribe_flows();
        let state = Arc::new(Mutex::new(FlowState::new(500)));

        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = sock.local_addr().unwrap();
        tokio::spawn(run_flow_listener(
            sock,
            state.clone(),
            limiter(),
            FlowProto::Netflow,
            None,
        ));
        // 1-second bucket so the test flushes quickly.
        tokio::spawn(run_flow_flusher(
            bus.clone(),
            state.clone(),
            "edge-1".into(),
            "default".into(),
            1,
        ));

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let pkt = nf9_one_record(Ipv4Addr::new(10, 0, 0, 5), Ipv4Addr::new(8, 8, 8, 8), 4096);
        client.send_to(&pkt, addr).await.unwrap();

        let batch = tokio::time::timeout(std::time::Duration::from_secs(5), flows.recv())
            .await
            .expect("flow batch within timeout")
            .unwrap();
        assert_eq!(batch.poller_id, "edge-1");
        assert_eq!(batch.exporter_ip, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(batch.records.len(), 1);
        assert_eq!(
            batch.records[0].dst_ip,
            IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))
        );
        assert_eq!(batch.records[0].dst_port, 443);
        assert_eq!(batch.records[0].bytes, 4096);
        // Export-provided destination AS survives the AggregatedFlow → FlowRecord mapping.
        assert_eq!(batch.records[0].dst_as, 15169);
    }

    /// Build a minimal sFlow v5 datagram: one compact flow sample carrying one raw Ethernet/IPv4/TCP
    /// packet header, sampled 1-in-`rate`.
    fn sflow_one_tcp(
        src: Ipv4Addr,
        dst: Ipv4Addr,
        dport: u16,
        rate: u32,
        frame_len: u32,
    ) -> Vec<u8> {
        // Ethernet II + IPv4 + TCP ports.
        let mut header = Vec::new();
        header.extend_from_slice(&[0x00, 0x11, 0x22, 0x33, 0x44, 0x55]); // dst MAC
        header.extend_from_slice(&[0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb]); // src MAC
        header.extend_from_slice(&0x0800u16.to_be_bytes()); // ethertype IPv4
        header.push(0x45); // v4, IHL 5
        header.push(0x00); // tos
        header.extend_from_slice(&40u16.to_be_bytes()); // total length
        header.extend_from_slice(&[0u8; 4]); // id + flags/frag
        header.push(64); // ttl
        header.push(6); // proto TCP
        header.extend_from_slice(&0u16.to_be_bytes()); // checksum
        header.extend_from_slice(&src.octets());
        header.extend_from_slice(&dst.octets());
        header.extend_from_slice(&1234u16.to_be_bytes()); // src port
        header.extend_from_slice(&dport.to_be_bytes()); // dst port
        header.extend_from_slice(&[0u8; 12]); // rest of TCP header

        let mut rec_body = Vec::new();
        rec_body.extend_from_slice(&1u32.to_be_bytes()); // header_protocol = ethernet
        rec_body.extend_from_slice(&frame_len.to_be_bytes());
        rec_body.extend_from_slice(&0u32.to_be_bytes()); // stripped
        rec_body.extend_from_slice(&(header.len() as u32).to_be_bytes());
        rec_body.extend_from_slice(&header);
        while rec_body.len() % 4 != 0 {
            rec_body.push(0);
        }
        let mut record = Vec::new();
        record.extend_from_slice(&1u32.to_be_bytes()); // flow_format = raw packet header
        record.extend_from_slice(&(rec_body.len() as u32).to_be_bytes());
        record.extend_from_slice(&rec_body);

        let mut sample = Vec::new();
        sample.extend_from_slice(&0u32.to_be_bytes()); // seq
        sample.extend_from_slice(&0u32.to_be_bytes()); // source_id
        sample.extend_from_slice(&rate.to_be_bytes()); // sampling_rate
        sample.extend_from_slice(&0u32.to_be_bytes()); // sample_pool
        sample.extend_from_slice(&0u32.to_be_bytes()); // drops
        sample.extend_from_slice(&1u32.to_be_bytes()); // input ifIndex
        sample.extend_from_slice(&0u32.to_be_bytes()); // output ifIndex
        sample.extend_from_slice(&1u32.to_be_bytes()); // num_records
        sample.extend_from_slice(&record);

        let mut pkt = Vec::new();
        pkt.extend_from_slice(&5u32.to_be_bytes()); // version
        pkt.extend_from_slice(&1u32.to_be_bytes()); // agent addr type v4
        pkt.extend_from_slice(&[192, 168, 1, 1]); // agent ip
        pkt.extend_from_slice(&0u32.to_be_bytes()); // sub_agent_id
        pkt.extend_from_slice(&0u32.to_be_bytes()); // seq
        pkt.extend_from_slice(&0u32.to_be_bytes()); // uptime
        pkt.extend_from_slice(&1u32.to_be_bytes()); // num_samples
        pkt.extend_from_slice(&1u32.to_be_bytes()); // sample type = flow sample
        pkt.extend_from_slice(&(sample.len() as u32).to_be_bytes());
        pkt.extend_from_slice(&sample);
        pkt
    }

    /// End-to-end over a real UDP socket for the sFlow path: sFlow datagram in → scaled FlowBatch out.
    #[tokio::test]
    async fn sflow_datagram_becomes_flow_batch() {
        let bus = Arc::new(InMemoryBus::new(8));
        let mut flows = bus.subscribe_flows();
        let state = Arc::new(Mutex::new(FlowState::new(500)));

        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = sock.local_addr().unwrap();
        tokio::spawn(run_flow_listener(
            sock,
            state.clone(),
            limiter(),
            FlowProto::Sflow,
            None,
        ));
        tokio::spawn(run_flow_flusher(
            bus.clone(),
            state.clone(),
            "edge-1".into(),
            "default".into(),
            1,
        ));

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let pkt = sflow_one_tcp(
            Ipv4Addr::new(10, 0, 0, 9),
            Ipv4Addr::new(8, 8, 4, 4),
            443,
            1000,
            1500,
        );
        client.send_to(&pkt, addr).await.unwrap();

        let batch = tokio::time::timeout(std::time::Duration::from_secs(5), flows.recv())
            .await
            .expect("flow batch within timeout")
            .unwrap();
        assert_eq!(batch.records.len(), 1);
        assert_eq!(
            batch.records[0].dst_ip,
            IpAddr::V4(Ipv4Addr::new(8, 8, 4, 4))
        );
        assert_eq!(batch.records[0].dst_port, 443);
        // Byte/packet counts are scaled by the 1-in-1000 sampling rate.
        assert_eq!(batch.records[0].bytes, 1500 * 1000);
        assert_eq!(batch.records[0].packets, 1000);
    }

    #[tokio::test]
    async fn garbage_datagram_is_dropped_without_publishing() {
        let bus = Arc::new(InMemoryBus::new(8));
        let mut flows = bus.subscribe_flows();
        let state = Arc::new(Mutex::new(FlowState::new(500)));

        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = sock.local_addr().unwrap();
        tokio::spawn(run_flow_listener(
            sock,
            state.clone(),
            limiter(),
            FlowProto::Netflow,
            None,
        ));
        tokio::spawn(run_flow_flusher(
            bus.clone(),
            state.clone(),
            "edge-1".into(),
            "default".into(),
            1,
        ));

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client
            .send_to(&[0xFF, 0x00, 0x13, 0x37], addr)
            .await
            .unwrap();

        // No flow batch should ever arrive (give the flusher a couple of ticks).
        let got = tokio::time::timeout(std::time::Duration::from_millis(2500), flows.recv()).await;
        assert!(got.is_err(), "garbage must not produce a flow batch");
    }

    /// Spin up a listener with the raw relay attached and return its bus + socket address.
    async fn listener_with_relay(proto: FlowProto) -> (Arc<InMemoryBus>, std::net::SocketAddr) {
        let bus = Arc::new(InMemoryBus::new(16));
        let state = Arc::new(Mutex::new(FlowState::new(500)));
        let (tee, relay) = raw_flow_tee("edge-1".into(), Some("tokyo".into()));
        tokio::spawn(run_raw_flow_relay(bus.clone(), relay));

        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = sock.local_addr().unwrap();
        tokio::spawn(run_flow_listener(sock, state, limiter(), proto, Some(tee)));
        (bus, addr)
    }

    /// The relay's whole point: what arrives on `yagra.flows.raw` is byte-identical to the datagram
    /// the exporter sent — the aggregate could never reproduce it.
    #[tokio::test]
    async fn accepted_datagram_is_relayed_byte_for_byte() {
        let (bus, addr) = listener_with_relay(FlowProto::Netflow).await;
        let mut raw = bus.subscribe_raw_flows();

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let pkt = nf9_one_record(Ipv4Addr::new(10, 0, 0, 5), Ipv4Addr::new(8, 8, 8, 8), 4096);
        client.send_to(&pkt, addr).await.unwrap();
        let sent_from = client.local_addr().unwrap();

        let dg = tokio::time::timeout(std::time::Duration::from_secs(5), raw.recv())
            .await
            .expect("raw datagram within timeout")
            .unwrap();
        assert_eq!(dg.datagram().unwrap(), pkt, "bytes must be untouched");
        assert_eq!(dg.poller_id, "edge-1");
        assert_eq!(dg.pool.as_deref(), Some("tokyo"));
        assert_eq!(dg.exporter_ip, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(dg.src_port, sent_from.port());
        assert_eq!(dg.proto, FlowProto::Netflow);
    }

    /// A NetFlow v9 template-only datagram decodes to zero records, but a downstream collector
    /// cannot decode anything after it without the template — so it must still be relayed.
    #[tokio::test]
    async fn template_only_datagram_carries_no_records_but_is_still_relayed() {
        let (bus, addr) = listener_with_relay(FlowProto::Netflow).await;
        let mut raw = bus.subscribe_raw_flows();
        let mut flows = bus.subscribe_flows();

        // The template set of a v9 export, with the data set stripped off.
        let full = nf9_one_record(Ipv4Addr::new(10, 0, 0, 5), Ipv4Addr::new(8, 8, 8, 8), 4096);
        // header(20) + template set(4 + 4 + 6*4) = 52 bytes; the rest is the data set.
        let template_only = full[..52].to_vec();

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.send_to(&template_only, addr).await.unwrap();

        let dg = tokio::time::timeout(std::time::Duration::from_secs(5), raw.recv())
            .await
            .expect("raw datagram within timeout")
            .unwrap();
        assert_eq!(dg.datagram().unwrap(), template_only);
        // ...and it produced no aggregate records, which is exactly why the aggregate stream cannot
        // stand in for the relay.
        assert!(flows.try_recv().is_err());
    }

    /// Garbage is dropped by reception, so it must not be forwarded either: a destination should
    /// receive what Yagra received, not what Yagra refused.
    #[tokio::test]
    async fn unparseable_datagram_is_not_relayed() {
        let (bus, addr) = listener_with_relay(FlowProto::Netflow).await;
        let mut raw = bus.subscribe_raw_flows();

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client
            .send_to(&[0xFF, 0x00, 0x13, 0x37], addr)
            .await
            .unwrap();

        let got = tokio::time::timeout(std::time::Duration::from_millis(500), raw.recv()).await;
        assert!(got.is_err(), "garbage must not be relayed");
    }

    /// The reception path must not depend on the relay draining: a stalled publisher fills the
    /// bounded queue, and `offer` has to drop rather than await.
    #[tokio::test]
    async fn a_full_relay_queue_drops_instead_of_blocking_reception() {
        let (tee, relay) = raw_flow_tee("edge-1".into(), None);
        // No relay task spawned — nothing ever drains the queue.
        let exporter = IpAddr::V4(Ipv4Addr::LOCALHOST);
        for _ in 0..(RAW_FLOW_CHANNEL_CAP + 64) {
            // Would hang here if `offer` awaited capacity.
            tee.offer(exporter, 2055, FlowProto::Netflow, b"x");
        }
        assert_eq!(
            relay.rx.len(),
            RAW_FLOW_CHANNEL_CAP,
            "queue must stay bounded"
        );
    }
}
