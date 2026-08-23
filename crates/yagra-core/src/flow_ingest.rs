// SPDX-License-Identifier: AGPL-3.0-only
//! What happens to a traffic-flow record after it arrives on the bus (ADR-031, and ADR-034 for the
//! verbatim relay).
//!
//! Two independent halves, and keeping them independent is the design:
//!
//!  - **Storage** — decode, resolve the exporter to a node, enrich with IP→ASN where the export
//!    carried none, and batch-insert into ClickHouse. Only when a flow store is configured
//!    (default-OFF), and leader-only, so exactly one writer persists.
//!  - **Verbatim relay** — subscribed unconditionally, because forwarding raw flow datagrams is
//!    useful without a flow store and costs one relaxed load when no flow destination exists.
//!
//! Lived in `main.rs` until ADR-090; none of it is part of booting.
//!
//! ## Why match and persist are split (S27)
//!
//! A slow ClickHouse must not stall the `yagra.flows` subscription, because NATS drops a slow
//! consumer **silently**. So the consumer decodes and hands rows to the writer over a bounded
//! channel, and the writer is the only thing that waits on the store. This tier is explicitly
//! loss-tolerant: a dropped batch is counted and reported, not retried.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::{Stream, StreamExt};
use tokio::sync::mpsc::error::TrySendError;
use tokio::time::Instant;
use uuid::Uuid;
use yagra_bus::FlowBatch;
use yagra_telemetry::CancellationToken;

use crate::flowstore::{FlowRow, FlowStore};
use crate::repo::NodeRepo;

/// Max flow rows buffered before a forced ClickHouse insert (bounds memory between flush ticks).
const FLOW_INSERT_MAX_ROWS: usize = 10_000;
/// How often the flow writer flushes accumulated rows to ClickHouse.
const FLOW_INSERT_FLUSH_SECS: u64 = 5;
/// How often the flow consumer refreshes its exporter-IP → node-id snapshot.
const FLOW_ADDR_REFRESH_SECS: u64 = 60;
/// Cap on the flow consumer's "already-missed" exporter set (throttles miss-triggered addr-map
/// reloads to once per distinct exporter). Bounded well above any realistic exporter count; a
/// pathological flood of distinct source IPs clears it, re-arming the periodic refresh as the
/// backstop rather than growing memory unbounded.
const FLOW_MISS_CACHE_MAX: usize = 65_536;
/// Bounded hand-off queue between the flow consumer (bus → rows) and the ClickHouse writer. A full
/// queue means the writer is behind a slow/hung ClickHouse; the consumer then drops + counts rows
/// (`channel_full`) instead of stalling the `yagra.flows` subscription into a silent NATS drop (S27).
pub(crate) const FLOW_PERSIST_CHANNEL_CAP: usize = 16_384;

/// Ensure the ClickHouse flow schema exists, with a short retry to tolerate ClickHouse coming up
/// after core (compose gates on `service_started`, not health). Best-effort: after the retries it
/// logs and moves on — inserts will keep failing (dropped, loss-tolerant tier) until ClickHouse is
/// reachable, at which point they succeed against the now-present tables.
pub(crate) async fn ensure_flow_schema(store: &Arc<dyn FlowStore>) {
    for attempt in 1..=5u32 {
        match store.ensure_schema().await {
            Ok(()) => return,
            Err(e) => {
                tracing::warn!(attempt, error = %e, "ClickHouse flow schema ensure failed; retrying");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
    tracing::error!(
        "could not ensure ClickHouse flow schema after retries — flow inserts will fail until reachable"
    );
}

/// Bound ClickHouse's own system log tables (ADR-031 Increment 4). Best-effort by construction:
/// a failure here is logged and the flow pipeline starts anyway.
///
/// 🚨 **This must never be able to stop flow ingestion.** It is housekeeping on the store's
/// self-telemetry, not on Yagra's data — a ClickHouse that refuses `ALTER TABLE system.*` (managed
/// service, restricted user) is still a perfectly good flow store, and treating that as fatal would
/// trade a disk-growth problem for an outage.
pub(crate) async fn bound_clickhouse_system_logs(store: &Arc<dyn FlowStore>, days: u32) {
    if let Err(e) = store.bound_system_logs(days).await {
        tracing::warn!(
            error = %e,
            days,
            "could not bound ClickHouse system log retention; its own logs stay unbounded"
        );
    }
}

/// Build the ClickHouse rows for one edge-aggregated flow batch: resolve the exporter to a node and
/// fill in only the AS numbers the exporter didn't provide from the offline IP→ASN table (the
/// exporter's own BGP view is authoritative — ADR-031). Returns `None` when the exporter isn't
/// mapped to a node (the batch is dropped by the caller). Pure — unit-tested.
fn flow_rows_from_batch(
    batch: &FlowBatch,
    addr_map: &HashMap<std::net::IpAddr, Uuid>,
    ipasn: Option<&Arc<crate::ipasn::IpAsnDb>>,
) -> Option<Vec<FlowRow>> {
    let node_id = addr_map.get(&batch.exporter_ip).copied()?;
    let mut rows = Vec::with_capacity(batch.records.len());
    for rec in &batch.records {
        let mut src_as = rec.src_as;
        let mut dst_as = rec.dst_as;
        if let Some(db) = ipasn {
            if src_as == 0 {
                src_as = db.lookup(rec.src_ip).unwrap_or(0);
            }
            if dst_as == 0 {
                dst_as = db.lookup(rec.dst_ip).unwrap_or(0);
            }
        }
        rows.push(FlowRow {
            node_id,
            ts_unix_ms: batch.bucket_start_ms,
            exporter_ip: batch.exporter_ip,
            if_index: rec.if_index,
            src_ip: rec.src_ip,
            dst_ip: rec.dst_ip,
            src_port: rec.src_port,
            dst_port: rec.dst_port,
            proto: rec.proto,
            tos: rec.tos,
            src_as,
            dst_as,
            bytes: rec.bytes,
            packets: rec.packets,
            flows: rec.flows,
        });
    }
    Some(rows)
}

/// Hand rows to the flow writer without ever awaiting ClickHouse (ADR-024/025 match/persist split).
/// A full queue means the writer is behind a slow/hung ClickHouse: the row is dropped and counted
/// (`channel_full`) so the `yagra.flows` subscription keeps draining — turning what used to be an
/// invisible NATS slow-consumer drop into a measured one (S27). Returns the number of rows dropped.
fn send_flow_rows(tx: &tokio::sync::mpsc::Sender<FlowRow>, rows: Vec<FlowRow>) -> u64 {
    let mut dropped = 0u64;
    for row in rows {
        match tx.try_send(row) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => dropped += 1,
            // Writer gone (shutdown): stop — teardown does the final flush of what's already queued.
            Err(TrySendError::Closed(_)) => break,
        }
    }
    if dropped > 0 {
        metrics::counter!("yagra_flow_rows_dropped_total", "reason" => "channel_full")
            .increment(dropped);
    }
    dropped
}

/// Decide whether a flow batch from `exporter_ip` should trigger an out-of-band address-map reload.
/// Returns `true` only for the *first* miss of each distinct exporter (recording it in `missed`), so
/// a steady stream of batches from an unregistered/never-mapped exporter — the normal case, since
/// routers often export from a loopback that differs from their configured management address —
/// cannot spin a full-table `SELECT` + map rebuild on the flow-ingest hot path once per batch. A
/// genuinely just-added node is still picked up: its first miss reloads immediately, and thereafter
/// it is present in the map; anything still unmapped is caught by the periodic refresh (S27 follow-up).
fn should_reload_on_miss(
    addr_map: &HashMap<std::net::IpAddr, Uuid>,
    missed: &mut std::collections::HashSet<std::net::IpAddr>,
    exporter_ip: std::net::IpAddr,
) -> bool {
    !addr_map.contains_key(&exporter_ip) && missed.insert(exporter_ip)
}

/// Consume verbatim flow datagrams from `yagra.flows.raw` and tee them to the forwarder (ADR-034
/// Increment 2). Deliberately does nothing else: these datagrams exist only so a forwarding
/// destination can be given what the exporter actually sent. ClickHouse is fed by the aggregate
/// stream above, and duplicating that here would double-count every flow.
pub(crate) async fn consume_raw_flows<S>(mut stream: S, forward: crate::forward::ForwardHandle)
where
    S: Stream<Item = yagra_bus::RawFlowDatagram> + Unpin,
{
    while let Some(datagram) = stream.next().await {
        // Never blocks: with no flow destination this is one relaxed atomic load, and a full inlet
        // drops and counts rather than back-pressuring the subscription into a NATS slow-consumer.
        forward.offer_flow(&datagram);
    }
}

/// Consume edge-aggregated flow batches from the bus, resolve each exporter to a node (via the same
/// address map the event pipeline uses), enrich AS numbers, and hand the rows to the ClickHouse
/// writer over a bounded queue. Never awaits ClickHouse, so a slow/hung ClickHouse can't stall the
/// `yagra.flows` subscription into a silent NATS slow-consumer drop (ADR-024/025 match/persist split,
/// S27). Spawned via `spawn_cancellable` — the writer owns the final flush on shutdown.
pub(crate) async fn consume_flows<S>(
    mut flows: S,
    tx: tokio::sync::mpsc::Sender<FlowRow>,
    repo: Arc<NodeRepo>,
    ipasn: crate::ipasn::IpAsnHandle,
) where
    S: Stream<Item = FlowBatch> + Unpin,
{
    let mut addr_map = repo.address_map().await.unwrap_or_default();
    let mut last_refresh = Instant::now();
    // Exporters we've already tried (and failed) to resolve since startup — throttles the
    // miss-triggered reload below to once per distinct exporter (see `should_reload_on_miss`).
    let mut missed: std::collections::HashSet<std::net::IpAddr> = std::collections::HashSet::new();
    while let Some(batch) = flows.next().await {
        // Refresh the exporter→node snapshot periodically (nodes are added/removed at runtime).
        if last_refresh.elapsed() >= Duration::from_secs(FLOW_ADDR_REFRESH_SECS) {
            if let Ok(m) = repo.address_map().await {
                addr_map = m;
            }
            last_refresh = Instant::now();
        }
        // On a mapping miss, reload once more in case the exporter's node was just added — but only
        // the first time we see each exporter, so a never-registered exporter can't reload per batch.
        if missed.len() >= FLOW_MISS_CACHE_MAX {
            missed.clear();
        }
        if should_reload_on_miss(&addr_map, &mut missed, batch.exporter_ip) {
            if let Ok(m) = repo.address_map().await {
                addr_map = m;
                last_refresh = Instant::now();
            }
        }
        // Snapshot the hot-swappable IP→ASN table once per batch (not per record).
        let ipasn_now = ipasn.read().unwrap().clone();
        let Some(rows) = flow_rows_from_batch(&batch, &addr_map, ipasn_now.as_ref()) else {
            metrics::counter!("yagra_flow_batches_unmapped_total").increment(1);
            tracing::debug!(exporter = %batch.exporter_ip, "flow batch from unmapped exporter — dropped");
            continue;
        };
        send_flow_rows(&tx, rows);
    }
}

/// Async ClickHouse flow writer (ADR-031): drains the bounded flow queue, batches rows across
/// consumed items, and bulk-inserts on a size (`FLOW_INSERT_MAX_ROWS`) or time
/// (`FLOW_INSERT_FLUSH_SECS`) trigger. Best-effort/loss-tolerant (ADR-017): an insert failure drops
/// the batch and counts it (unlike the metrics path, which spills). Takes the shutdown token
/// directly (not `spawn_cancellable`) so it can do a best-effort final flush on cancel.
pub(crate) async fn run_flow_writer(
    mut rx: tokio::sync::mpsc::Receiver<FlowRow>,
    store: Arc<dyn FlowStore>,
    shutdown: CancellationToken,
) {
    let mut buf: Vec<FlowRow> = Vec::new();
    let mut ticker = tokio::time::interval(Duration::from_secs(FLOW_INSERT_FLUSH_SECS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                while let Ok(r) = rx.try_recv() {
                    buf.push(r);
                    if buf.len() >= FLOW_INSERT_MAX_ROWS {
                        flush_flow(&store, &mut buf).await;
                    }
                }
                flush_flow(&store, &mut buf).await;
                break;
            }
            _ = ticker.tick() => {
                flush_flow(&store, &mut buf).await;
            }
            first = rx.recv() => {
                match first {
                    None => {
                        flush_flow(&store, &mut buf).await;
                        break;
                    }
                    Some(r) => {
                        buf.push(r);
                        while buf.len() < FLOW_INSERT_MAX_ROWS {
                            match rx.try_recv() {
                                Ok(r) => buf.push(r),
                                Err(_) => break,
                            }
                        }
                        if buf.len() >= FLOW_INSERT_MAX_ROWS {
                            flush_flow(&store, &mut buf).await;
                        }
                        metrics::gauge!("yagra_persist_queue_depth", "stream" => "flow")
                            .set(rx.len() as f64);
                    }
                }
            }
        }
    }
}

/// Insert one buffered batch of flow rows; on failure the batch is dropped and counted (flow is a
/// loss-tolerant tier, unlike the metrics path which spills — ADR-017).
async fn flush_flow(store: &Arc<dyn FlowStore>, buf: &mut Vec<FlowRow>) {
    if buf.is_empty() {
        return;
    }
    let rows = std::mem::take(buf);
    let n = rows.len() as u64;
    match store.insert_batch(&rows).await {
        Ok(()) => metrics::counter!("yagra_flow_rows_written_total").increment(n),
        Err(e) => {
            metrics::counter!("yagra_flow_rows_dropped_total", "reason" => "insert_error")
                .increment(n);
            tracing::warn!(error = %e, rows = n, "ClickHouse flow insert failed — batch dropped (loss-tolerant tier)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_bus::DEFAULT_POOL;

    // ---- Flow ingest match/persist split (S27, ADR-031) ----

    fn flow_rec(
        src: &str,
        dst: &str,
        src_as: u32,
        dst_as: u32,
        bytes: u64,
    ) -> yagra_bus::FlowRecord {
        yagra_bus::FlowRecord {
            src_ip: src.parse().unwrap(),
            dst_ip: dst.parse().unwrap(),
            src_port: 1234,
            dst_port: 443,
            proto: 6,
            tos: 0,
            if_index: 2,
            src_as,
            dst_as,
            bytes,
            packets: 10,
            flows: 1,
        }
    }

    fn flow_batch(exporter: &str, records: Vec<yagra_bus::FlowRecord>) -> FlowBatch {
        FlowBatch {
            poller_id: "test-poller".into(),
            pool: DEFAULT_POOL.into(),
            exporter_ip: exporter.parse().unwrap(),
            bucket_start_ms: 1_700_000_000_000,
            bucket_secs: 60,
            records,
            dropped: 0,
        }
    }

    #[test]
    fn flow_rows_dropped_when_exporter_unmapped() {
        let addr_map: HashMap<std::net::IpAddr, Uuid> = HashMap::new();
        let batch = flow_batch(
            "198.51.100.7",
            vec![flow_rec("10.0.0.1", "8.8.8.8", 0, 0, 100)],
        );
        assert!(
            flow_rows_from_batch(&batch, &addr_map, None).is_none(),
            "an unmapped exporter yields no rows (the batch is dropped by the caller)"
        );
    }

    #[test]
    fn miss_reload_throttled_to_once_per_exporter() {
        use std::collections::HashSet;
        use std::net::IpAddr;
        let mapped: IpAddr = "10.0.0.1".parse().unwrap();
        let addr_map: HashMap<IpAddr, Uuid> = HashMap::from([(mapped, Uuid::from_u128(1))]);
        let mut missed: HashSet<IpAddr> = HashSet::new();

        // A mapped exporter never triggers a reload and is never recorded as missed.
        assert!(!should_reload_on_miss(&addr_map, &mut missed, mapped));
        assert!(missed.is_empty());

        // First batch from an unmapped exporter reloads once…
        let unknown: IpAddr = "198.51.100.7".parse().unwrap();
        assert!(should_reload_on_miss(&addr_map, &mut missed, unknown));
        // …and every subsequent batch from the same still-unmapped exporter is throttled (no reload).
        assert!(!should_reload_on_miss(&addr_map, &mut missed, unknown));
        assert!(!should_reload_on_miss(&addr_map, &mut missed, unknown));

        // A different unmapped exporter still gets its own one-shot reload.
        let unknown2: IpAddr = "203.0.113.9".parse().unwrap();
        assert!(should_reload_on_miss(&addr_map, &mut missed, unknown2));
        assert!(!should_reload_on_miss(&addr_map, &mut missed, unknown2));
    }

    #[test]
    fn flow_rows_resolve_node_and_preserve_fields() {
        let exporter: std::net::IpAddr = "198.51.100.7".parse().unwrap();
        let node = Uuid::from_u128(42);
        let addr_map: HashMap<std::net::IpAddr, Uuid> = HashMap::from([(exporter, node)]);
        let batch = flow_batch(
            "198.51.100.7",
            vec![
                flow_rec("10.0.0.1", "8.8.8.8", 0, 0, 100),
                flow_rec("10.0.0.2", "1.1.1.1", 0, 0, 200),
            ],
        );
        let rows = flow_rows_from_batch(&batch, &addr_map, None).expect("mapped exporter");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.node_id == node));
        assert_eq!(rows[0].ts_unix_ms, 1_700_000_000_000);
        assert_eq!((rows[0].bytes, rows[1].bytes), (100, 200));
        // No IP→ASN table and the exporter sent 0 → AS stays unknown.
        assert_eq!((rows[0].src_as, rows[0].dst_as), (0, 0));
    }

    #[test]
    fn flow_as_enrichment_fills_only_zeros() {
        // Offline IP→ASN table maps 8.8.8.0/24 → AS15169; an exporter's own non-zero AS still wins.
        let db = crate::ipasn::IpAsnDb::from_tsv("8.8.8.0\t8.8.8.255\t15169\tUS\tGOOGLE\n");
        let exporter: std::net::IpAddr = "198.51.100.7".parse().unwrap();
        let node = Uuid::from_u128(7);
        let addr_map: HashMap<std::net::IpAddr, Uuid> = HashMap::from([(exporter, node)]);
        let batch = flow_batch(
            "198.51.100.7",
            vec![
                // dst 8.8.8.8, dst_as=0 → enriched to 15169; src_as=64500 provided → preserved.
                flow_rec("10.0.0.1", "8.8.8.8", 64500, 0, 100),
                // dst 9.9.9.9 not in the table → stays unknown.
                flow_rec("10.0.0.2", "9.9.9.9", 0, 0, 50),
            ],
        );
        let rows = flow_rows_from_batch(&batch, &addr_map, Some(&db)).expect("mapped exporter");
        assert_eq!(
            rows[0].src_as, 64500,
            "exporter-provided AS is authoritative"
        );
        assert_eq!(
            rows[0].dst_as, 15169,
            "a zero AS is filled from the offline table"
        );
        assert_eq!(
            rows[1].dst_as, 0,
            "an address not in the table stays unknown"
        );
    }

    #[tokio::test]
    async fn flow_send_drops_and_counts_when_queue_full() {
        // A full hand-off queue (writer behind a slow ClickHouse) drops rows and reports the count
        // instead of blocking the bus consumer — the S27 backpressure contract.
        let node = Uuid::from_u128(1);
        let mk = |n: usize| -> Vec<FlowRow> {
            (0..n)
                .map(|i| FlowRow {
                    node_id: node,
                    ts_unix_ms: 0,
                    exporter_ip: "198.51.100.7".parse().unwrap(),
                    if_index: 0,
                    src_ip: "10.0.0.1".parse().unwrap(),
                    dst_ip: "8.8.8.8".parse().unwrap(),
                    src_port: 0,
                    dst_port: 0,
                    proto: 6,
                    tos: 0,
                    src_as: 0,
                    dst_as: 0,
                    bytes: i as u64,
                    packets: 0,
                    flows: 1,
                })
                .collect()
        };
        let (tx, mut rx) = tokio::sync::mpsc::channel::<FlowRow>(2);
        // Queue cap 2: the first two rows are accepted, the remaining three are dropped.
        let dropped = send_flow_rows(&tx, mk(5));
        assert_eq!(dropped, 3, "rows beyond the queue capacity are dropped");
        assert_eq!(rx.try_recv().unwrap().bytes, 0);
        assert_eq!(rx.try_recv().unwrap().bytes, 1);
        assert!(rx.try_recv().is_err(), "only the accepted rows are queued");
    }
}
