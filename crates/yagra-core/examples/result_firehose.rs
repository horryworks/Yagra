// SPDX-License-Identifier: AGPL-3.0-only
//! Synthetic poll-result firehose — the S1 (ADR-025) scale-validation harness.
//!
//! Publishes `PollResult` messages to the real bus (`yagra.results`) at a target rate against a set
//! of synthetic node ids, so the core's redesigned ingest pipeline can be load-tested end to end:
//! the single in-memory matcher, the async VM writer (coalesced bulk import), and the async PG
//! writer (batched interface/identity/history). Point a running `yagra-core` at the same NATS and
//! watch its `/metrics`:
//!
//!   - `yagra_poll_results_total`            — ingest throughput (should track the target rate)
//!   - `yagra_persist_queue_depth{stream=…}` — must stay BOUNDED, not climb monotonically
//!   - `yagra_result_*_persist_dropped_total`/`yagra_vm_samples_dropped_total` — shed counters
//!   - `yagra_result_ingest_lag_ms`          — end-to-end lag (poll ts → matcher entry)
//!   - `yagra_vm_spill_depth`                — non-zero only while VM is unhealthy
//!
//! To exercise the VM writer's timeout/retry/spill, point core's `YAGRA_TSDB_URL` at a mock VM that
//! can 500/hang. Liveness alerts (and thus the history batch path) are driven by `FIREHOSE_DOWN_PCT`.
//!
//! Run (with a NATS reachable):
//!   cargo run --release --example result_firehose
//! Env knobs (all optional):
//!   YAGRA_BUS_URL       NATS url                         (default nats://127.0.0.1:4222)
//!   FIREHOSE_NODES      synthetic node count             (default 1000)
//!   FIREHOSE_RATE       target results/sec               (default 1000)
//!   FIREHOSE_SECONDS    run duration; 0 = run forever    (default 30)
//!   FIREHOSE_DOWN_PCT   % of results reporting Unreachable, to drive liveness alerts (default 2)
//!   FIREHOSE_IFACES     interfaces per result (needs seeded nodes for the FK upsert) (default 0)
//!   FIREHOSE_IFACE_METRICS  per-interface metric samples per interface, 0..=11 (default 11)
//!   FIREHOSE_SEEDED     1 ⇒ target the deterministic ids from `seed_nodes.rs` instead of random
//!                       NodeIds, so results land on real FK-valid, scheduled nodes (default 0)
//!
//! ## 🚨 `FIREHOSE_IFACES` alone creates no time series, and that was the hole (ADR-110)
//!
//! Until v0.3.5 this harness put interfaces in `PollResult.interfaces` — the **metadata** the
//! PostgreSQL upsert consumes — and left `samples` at two node-level gauges. So it exercised the
//! `interfaces` table and **not one series** of the per-interface TSDB path, which is where 96% of a
//! real fleet's cardinality lives (11 metrics × every port, `collection.rs`). A 50,000 × 24 run
//! through the old harness would have written 1.2M rows and 100k series instead of 13.2M, and
//! reported the receiving side as comfortable.
//!
//! `FIREHOSE_IFACE_METRICS` now emits the built-in catalog's eleven per-interface metrics under the
//! same names the poller uses, so `node × ifindex × metric` matches production exactly. Set it to 0
//! to get the old metadata-only behaviour deliberately.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use uuid::Uuid;
use yagra_bus::{Bus, CheckOutcome, DiscoveredInterface, NatsBus, PollResult, Sample};
use yagra_common::{IfIndex, MetricKind, NodeId};

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

/// Deterministic node-id scheme, **shared verbatim with `seed_nodes.rs`** — `FIREHOSE_SEEDED=1` aims
/// results at the same rows the seeder wrote. High 64 bits are a fixed namespace tag; the low 64 bits
/// carry the index. Keep in lockstep with the seeder if you change it.
const SEED_TAG_HI: u64 = 0xF17E_0000_5EED_0000;

fn seeded_node_id(i: u64) -> NodeId {
    NodeId::from(Uuid::from_u128(((SEED_TAG_HI as u128) << 64) | i as u128))
}

/// The built-in catalog's eleven per-interface metrics, in the order `collection.rs` declares them,
/// with the kind each is stored as.
///
/// ⚠️ **A second copy of a list `yagra_common::builtin_catalog()` owns.** It is spelled out here
/// rather than derived because an example may not depend on the bin crate, and because the rig must
/// be able to emit *fewer* than all eleven on purpose. If the catalog gains a per-interface metric,
/// a run through this harness understates the fleet's cardinality until this list follows.
const IFACE_METRICS: [(&str, MetricKind); 11] = [
    ("if_hc_in_octets", MetricKind::Counter),
    ("if_hc_out_octets", MetricKind::Counter),
    ("if_hc_in_ucast_pkts", MetricKind::Counter),
    ("if_hc_out_ucast_pkts", MetricKind::Counter),
    ("if_in_errors", MetricKind::Counter),
    ("if_out_errors", MetricKind::Counter),
    ("if_in_discards", MetricKind::Counter),
    ("if_out_discards", MetricKind::Counter),
    ("if_oper_status", MetricKind::Gauge),
    ("if_admin_status", MetricKind::Gauge),
    ("if_high_speed", MetricKind::Gauge),
];

/// Build one synthetic result for `node`, index `i` (drives the down/up pattern and sample jitter).
fn make_result(
    node: NodeId,
    i: u64,
    down_every: u64,
    ifaces: usize,
    iface_metrics: usize,
) -> PollResult {
    let down = down_every > 0 && i.is_multiple_of(down_every);
    let outcome = if down {
        CheckOutcome::Unreachable
    } else {
        CheckOutcome::Reachable
    };
    // A couple of node-level gauges with mild deterministic jitter so series aren't flat.
    let rtt = 5.0 + (i % 20) as f64;
    let cpu = 30.0 + (i % 50) as f64;
    let samples = if down {
        Vec::new()
    } else {
        let mut s = Vec::with_capacity(2 + ifaces * iface_metrics);
        s.push(Sample::gauge("icmp_rtt_ms", rtt));
        s.push(Sample::gauge("cpu_pct", cpu));
        // The per-interface half: one sample per (port × metric), which is what makes this a
        // cardinality test rather than a row-count one. Counters climb with `i` so `rate()` is
        // non-zero downstream — a flat counter would leave the interface-utilisation watch with
        // nothing to track, and it is the most expensive reader of these series.
        for k in 0..ifaces {
            let idx = IfIndex(k as u32 + 1);
            for (metric, kind) in IFACE_METRICS.iter().take(iface_metrics) {
                let value = match *metric {
                    "if_oper_status" | "if_admin_status" => 1.0,
                    "if_high_speed" => 1_000.0,
                    "if_in_errors" | "if_out_errors" | "if_in_discards" | "if_out_discards" => {
                        (i / 1_000) as f64
                    }
                    // ~1 Mbit/s per port with a per-port offset, as a monotonic raw counter.
                    _ => (i * 125_000 + k as u64 * 7_919) as f64,
                };
                s.push(Sample::interface(*metric, idx, value, *kind));
            }
        }
        s
    };
    let interfaces = (0..ifaces)
        .map(|k| DiscoveredInterface {
            ifindex: IfIndex(k as u32 + 1),
            if_name: Some(format!("eth{k}")),
            if_alias: None,
            if_speed: Some(1_000_000_000),
            if_duplex: Some(yagra_common::Duplex::Full),
            if_type: Some(yagra_common::IF_TYPE_ETHERNET_CSMACD),
            if_media: Some("1000BASE-T".to_owned()),
            transceiver_model: None,
            rx_power_low_dbm: None,
            rx_power_high_dbm: None,
            tx_power_low_dbm: None,
            tx_power_high_dbm: None,
        })
        .collect();
    PollResult {
        job_id: Uuid::new_v4(),
        node_id: node,
        at_unix_ms: now_ms(),
        outcome,
        samples,
        interfaces,
        sys_descr: None,
        dns_chain: None,
        neighbors: None,
        l3: None,
        arp: None,
        routing: None,
        observational: false,
        poller_id: Some("firehose".to_owned()),
        trace_context: Default::default(),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::var("YAGRA_BUS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_owned());
    let node_count = env_usize("FIREHOSE_NODES", 1000).max(1);
    let rate = env_usize("FIREHOSE_RATE", 1000).max(1);
    let seconds = env_usize("FIREHOSE_SECONDS", 30);
    let down_pct = env_usize("FIREHOSE_DOWN_PCT", 2).min(100);
    let ifaces = env_usize("FIREHOSE_IFACES", 0);
    let iface_metrics =
        env_usize("FIREHOSE_IFACE_METRICS", IFACE_METRICS.len()).min(IFACE_METRICS.len());
    let seeded = env_usize("FIREHOSE_SEEDED", 0) != 0;

    // Every Nth result reports down; N derived from the requested percentage (0 ⇒ never down).
    let down_every = 100u64.checked_div(down_pct as u64).unwrap_or(0);

    eprintln!(
        "result_firehose → {url}: {node_count} {} nodes, {rate}/s, {}s, down≈{down_pct}%, \n         {ifaces} ifaces/result × {iface_metrics} metrics = {} samples/result, \n         {} series over the fleet",
        if seeded { "seeded" } else { "random" },
        if seconds == 0 { "∞".to_owned() } else { seconds.to_string() },
        2 + ifaces * iface_metrics,
        node_count * (2 + ifaces * iface_metrics)
    );
    let bus = NatsBus::connect(&url).await?;
    // Seeded mode reproduces `seed_nodes.rs`' deterministic ids (results hit real FK-valid, scheduled
    // rows); otherwise use throwaway random ids (fine for pure ingest-throughput measurement).
    let nodes: Vec<NodeId> = if seeded {
        (0..node_count as u64).map(seeded_node_id).collect()
    } else {
        (0..node_count).map(|_| NodeId::new()).collect()
    };

    // Pace in 50ms ticks so the load is smooth rather than one burst per second.
    const TICKS_PER_SEC: usize = 20;
    let per_tick = rate.div_ceil(TICKS_PER_SEC);
    let mut ticker = tokio::time::interval(Duration::from_millis(1000 / TICKS_PER_SEC as u64));

    let start = Instant::now();
    let mut i: u64 = 0;
    let mut sent_since_report: u64 = 0;
    let mut last_report = Instant::now();
    loop {
        ticker.tick().await;
        for _ in 0..per_tick {
            let node = nodes[(i as usize) % node_count];
            let result = make_result(node, i, down_every, ifaces, iface_metrics);
            // Fire-and-forget; a publish error means NATS is down — surface and stop.
            if let Err(e) = bus.publish_result(result).await {
                anyhow::bail!("publish_result failed: {e}");
            }
            i += 1;
            sent_since_report += 1;
        }
        if last_report.elapsed() >= Duration::from_secs(1) {
            let secs = last_report.elapsed().as_secs_f64();
            eprintln!(
                "  sent {i} total ({:.0}/s over last {:.1}s)",
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
    eprintln!("done: {i} results in {:.1}s", start.elapsed().as_secs_f64());
    Ok(())
}
