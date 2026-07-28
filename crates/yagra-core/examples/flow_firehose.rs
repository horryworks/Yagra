// SPDX-License-Identifier: AGPL-3.0-only
//! Synthetic flow firehose — the S27 (ADR-031) flow-ingest scale & backpressure harness.
//!
//! Publishes edge-aggregated `FlowBatch` messages to the real bus (`yagra.flows`) at a target rate,
//! so the core's flow pipeline can be load-tested end to end: the bus consumer (exporter→node
//! resolve + AS enrich), the bounded hand-off queue, and the async ClickHouse writer — i.e. the S27
//! match/persist split that mirrors the poll-result (ADR-025) and event (ADR-024) writers. Point a
//! running `yagra-core` (with a ClickHouse `YAGRA_FLOW_*` URL configured) at the same NATS and watch
//! its `/metrics`:
//!
//!   - `yagra_flow_rows_written_total`               — rows persisted (should track the target rate)
//!   - `yagra_persist_queue_depth{stream="flow"}`    — hand-off queue depth; must stay BOUNDED
//!   - `yagra_flow_rows_dropped_total{reason="channel_full"}` — writer behind a slow ClickHouse (S27)
//!   - `yagra_flow_rows_dropped_total{reason="insert_error"}` — ClickHouse insert failures
//!   - `yagra_flow_batches_unmapped_total`           — exporters with no node (0 when seeded, below)
//!
//! To exercise the S27 backpressure contract, pause or slow ClickHouse (e.g. `docker pause` its
//! container) mid-run: the writer falls behind, the bounded queue fills, and the consumer drops +
//! counts `channel_full` **without stalling the `yagra.flows` subscription** — the drop is now
//! measured at the core boundary instead of being a silent NATS slow-consumer drop.
//!
//! Exporter addresses reuse `seed_nodes.rs`' deterministic 10.0.0.0/8 scheme, so with the fleet
//! seeded (`cargo run --example seed_nodes`, at least `FLOW_EXPORTERS` nodes) each batch resolves to
//! a real node and its rows reach ClickHouse. Without seeding the batches count as `batches_unmapped`
//! (still valid for measuring the consumer's resolve path, but nothing reaches ClickHouse).
//!
//! Run (with a NATS reachable):
//!   cargo run --release --example flow_firehose
//! Env knobs (all optional):
//!   YAGRA_BUS_URL           NATS url                         (default nats://127.0.0.1:4222)
//!   FLOW_EXPORTERS          distinct exporter nodes          (default 1000)
//!   FLOW_RATE               target flow records/sec          (default 5000)
//!   FLOW_SECONDS            run duration; 0 = run forever     (default 30)
//!   FLOW_RECORDS_PER_BATCH  records per FlowBatch (top-N)     (default 50)

use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use yagra_bus::{Bus, FlowBatch, FlowRecord, NatsBus, DEFAULT_POOL};

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Deterministic exporter address, **matching `seed_nodes.rs`' `seeded_addr`** (10.0.0.0/8) so a
/// seeded fleet resolves each exporter to a real node. Keep in lockstep with the seeder.
fn exporter_addr(i: u64) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(10, (i >> 16) as u8, (i >> 8) as u8, i as u8))
}

/// One batch of `records` synthetic flows for `exporter`, varied by `seq` so successive batches carry
/// distinct 5-tuples (realistic cardinality; ordered by bytes descending per the `FlowBatch`
/// contract). AS numbers are left 0 so the core's IP→ASN enrichment path is exercised when a dataset
/// is configured.
fn make_batch(exporter: IpAddr, seq: u64, records: usize) -> FlowBatch {
    let recs = (0..records as u64)
        .map(|k| {
            let s = seq.wrapping_add(k);
            // Sources across 100.64.0.0/10 (CGNAT), destinations across a few public /24s, so
            // talkers / conversations / ports / protocols are all non-degenerate.
            let src = Ipv4Addr::new(100, 64 + (seq % 4) as u8, (s >> 8) as u8, s as u8);
            let dst = Ipv4Addr::new(8, 8, (k % 4) as u8, (k % 250) as u8 + 1);
            FlowRecord {
                src_ip: IpAddr::V4(src),
                dst_ip: IpAddr::V4(dst),
                src_port: 1024 + (k % 60000) as u16,
                dst_port: [80u16, 443, 53, 22][(k % 4) as usize],
                proto: if k % 5 == 0 { 17 } else { 6 },
                tos: 0,
                if_index: 1 + (k % 8) as u32,
                src_as: 0,
                dst_as: 0,
                bytes: 1_000_000u64.saturating_sub(k * 1000), // descending, per top-N contract
                packets: 100 + k,
                flows: 1,
            }
        })
        .collect();
    FlowBatch {
        poller_id: "flow-firehose".to_owned(),
        pool: DEFAULT_POOL.to_owned(),
        exporter_ip: exporter,
        bucket_start_ms: now_ms(),
        bucket_secs: 60,
        records: recs,
        dropped: 0,
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::var("YAGRA_BUS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_owned());
    let exporters = env_usize("FLOW_EXPORTERS", 1000).max(1);
    let rate = env_usize("FLOW_RATE", 5000).max(1);
    let seconds = env_usize("FLOW_SECONDS", 30);
    let per_batch = env_usize("FLOW_RECORDS_PER_BATCH", 50).max(1);

    // batches/sec = records/sec ÷ records/batch (rounded up so we never undershoot the row rate).
    let batches_per_sec = rate.div_ceil(per_batch);

    eprintln!(
        "flow_firehose → {url}: {rate} records/s ({batches_per_sec} batches/s × {per_batch} recs), \
         {exporters} exporters, {}",
        if seconds == 0 {
            "∞".to_owned()
        } else {
            format!("{seconds}s")
        }
    );
    let bus = NatsBus::connect(&url).await?;

    // Pace in 50ms ticks so the load is smooth rather than one burst per second.
    const TICKS_PER_SEC: usize = 20;
    let per_tick = batches_per_sec.div_ceil(TICKS_PER_SEC);
    let mut ticker = tokio::time::interval(Duration::from_millis(1000 / TICKS_PER_SEC as u64));

    let start = Instant::now();
    let mut seq: u64 = 0;
    let mut records_sent: u64 = 0;
    let mut sent_since_report: u64 = 0;
    let mut last_report = Instant::now();
    loop {
        ticker.tick().await;
        for _ in 0..per_tick {
            let exporter = exporter_addr(seq % exporters as u64);
            let batch = make_batch(exporter, seq, per_batch);
            let n = batch.records.len() as u64;
            // Fire-and-forget; a publish error means NATS is down — surface and stop.
            if let Err(e) = bus.publish_flows(batch).await {
                anyhow::bail!("publish_flows failed: {e}");
            }
            seq += 1;
            records_sent += n;
            sent_since_report += n;
        }
        if last_report.elapsed() >= Duration::from_secs(1) {
            let secs = last_report.elapsed().as_secs_f64();
            eprintln!(
                "  sent {records_sent} records ({:.0}/s over last {:.1}s)",
                sent_since_report as f64 / secs,
                secs
            );
            sent_since_report = 0;
            last_report = Instant::now();
        }
        if seconds != 0 && start.elapsed() >= Duration::from_secs(seconds as u64) {
            break;
        }
    }
    eprintln!(
        "done: {records_sent} flow records in {:.1}s",
        start.elapsed().as_secs_f64()
    );
    Ok(())
}
