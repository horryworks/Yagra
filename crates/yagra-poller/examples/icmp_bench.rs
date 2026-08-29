// SPDX-License-Identifier: AGPL-3.0-only
//! ICMP transport bench — the ADR-109 bisection harness.
//!
//! Drives `SurgePingTransport` and **nothing else**: no bus, no core, no working set, no local
//! scheduler, no `PollLimiter`. One question, and it takes one run to answer:
//!
//! > A poller with 64 concurrent-poll permits served 390 polls/s and one with 512 served 643, at
//! > 4.8% CPU. Eight times the permits bought 1.65x the throughput. **Is that ceiling inside the
//! > ICMP transport, or upstream of it?**
//!
//! It was answered on 2026-08-29, on the load test's own poller box: at 40 ms RTT this transport is
//! linear in concurrency past 2,048 (64 → 530 polls/s, 256 → 2,123, 512 → 4,178, 2,048 → 14,581)
//! with latency pinned at the theoretical floor.
//!
//! 🚨 **Then the same box was measured again through the whole product, and the two agreed to
//! within 1%.** A real poller carrying 50,000 nodes yields 532 polls/s at 64 permits and 2,136 at
//! 256 — against this harness's 530 and 2,123. So there is no gap between the probe path and the
//! pipeline around it: **the ceiling is `permits ÷ probe time`, and the answer to "transport or
//! upstream?" turned out to be "neither — it is the permit count".** What that leaves unexplained
//! is the load test's own 587 polls/s from two pollers, which is below what two pollers at even the
//! lowest permit setting should manage; ADR-109 records the differences that were never isolated.
//!
//! The harness stays because the curve has to be re-established on any host the question is asked
//! about — the no-delay ceiling differed **8x** between a WSL box and this VM, so "the transport is
//! not the limit" is a claim about a machine, not about the code. Sweep `BENCH_CONCURRENCY`:
//!
//! - It flattens well below `concurrency ÷ latency` ⇒ **the ceiling is the transport.** The prime
//!   suspect is that `SurgePingTransport` holds ONE `surge_ping::Client` per address family, so a
//!   single socket and a single receive task demultiplex every reply this process will ever get.
//! - It tracks `concurrency ÷ latency` ⇒ the transport has room, and a deployment that is slower
//!   than that is short of permits or short of jobs. A running poller's `yagra_poll_inflight` tells
//!   you which: pinned at the cap is throttled, below it is starved.
//!
//! 🚨 **The decisive number is not throughput, it is `nstat`.** Take `IcmpInEchoReps` before and
//! after the run and compare its delta with the `echoes_replied` printed below. Equal means every
//! reply the kernel received reached this process and the loss is the network's. A shortfall means
//! replies are being dropped between the socket and this process — and each dropped reply costs a
//! full `BENCH_TIMEOUT_MS` while the poll waiting for it occupies a permit, which is the mechanism
//! that would turn 8x the permits into 1.65x the throughput.
//!
//! ⚠️ **This measures the transport, not the poller.** What it prints is not a product figure and
//! must not be quoted as one.
//!
//! ⚠️ **It cannot exercise `PollLimiter`'s per-device single-flight.** `yagra-poller` is a bin crate
//! with no library target, so an example cannot reach `crate::limiter` — and reimplementing that
//! guard here would be a second copy of a rule that has exactly one. The targets below are all
//! distinct anyway, which is precisely the case where that guard never engages.
//!
//! Set up 50,000 responders with no LAN traffic at all (Linux, needs root):
//!   ip route add local 10.0.0.0/8 dev lo         # the kernel answers every 10.x echo itself
//!   tc qdisc add dev lo root netem delay 20ms    # optional: a realistic 40 ms round trip
//!
//! Run:
//!   nstat -n
//!   BENCH_CONCURRENCY=512 BENCH_SECONDS=60 cargo run --release --example icmp_bench
//!   nstat | grep -i Icmp
//!
//! Env knobs (all optional):
//!   BENCH_TARGETS      distinct targets to rotate through        (default 50000)
//!   BENCH_BASE         first target address                      (default 10.0.0.1)
//!   BENCH_CONCURRENCY  probes in flight                          (default 64)
//!   BENCH_SECONDS      run duration                              (default 30)
//!   BENCH_COUNT        echoes per probe (the check's `count`)    (default 3)
//!   BENCH_TIMEOUT_MS   per-echo timeout                          (default 1000)

use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use yagra_transport::{SurgePingTransport, Transport};

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

/// Percentile of an already-sorted slice, by nearest rank.
///
/// Returns 0.0 for an empty sample rather than panicking: a run that completed no probe at all is
/// itself a result, and printing it beats aborting before the counters are shown.
fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let targets = env_usize("BENCH_TARGETS", 50_000);
    let concurrency = env_usize("BENCH_CONCURRENCY", 64);
    let seconds = env_usize("BENCH_SECONDS", 30);
    let count = u8::try_from(env_usize("BENCH_COUNT", 3)).unwrap_or(3);
    let timeout = Duration::from_millis(env_usize("BENCH_TIMEOUT_MS", 1000) as u64);
    let base: Ipv4Addr = std::env::var("BENCH_BASE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(Ipv4Addr::new(10, 0, 0, 1));

    let transport: Arc<dyn Transport> = Arc::new(SurgePingTransport::new()?);
    let cursor = Arc::new(AtomicUsize::new(0));
    let latencies: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
    let deadline = Instant::now() + Duration::from_secs(seconds as u64);

    eprintln!(
        "icmp_bench: concurrency={concurrency} targets={targets} count={count} timeout={}ms \
         seconds={seconds} base={base}",
        timeout.as_millis()
    );

    let started = Instant::now();
    let mut workers = Vec::with_capacity(concurrency);
    for _ in 0..concurrency {
        let transport = Arc::clone(&transport);
        let cursor = Arc::clone(&cursor);
        let latencies = Arc::clone(&latencies);
        workers.push(tokio::spawn(async move {
            // (polls, echoes sent, echoes replied, probe errors)
            let mut local = (0u64, 0u64, 0u64, 0u64);
            let mut mine: Vec<f64> = Vec::new();
            while Instant::now() < deadline {
                let i = cursor.fetch_add(1, Ordering::Relaxed) % targets;
                let offset = u32::try_from(i).unwrap_or(0);
                let target = IpAddr::V4(Ipv4Addr::from(u32::from(base).wrapping_add(offset)));
                let at = Instant::now();
                match transport.probe_icmp(target, count, timeout).await {
                    Ok(probe) => {
                        local.0 += 1;
                        local.1 += u64::from(probe.sent);
                        local.2 += u64::from(probe.received);
                        mine.push(at.elapsed().as_secs_f64() * 1000.0);
                    }
                    Err(_) => local.3 += 1,
                }
            }
            latencies
                .lock()
                .expect("latency mutex poisoned")
                .extend(mine);
            local
        }));
    }

    let mut polls = 0u64;
    let mut sent = 0u64;
    let mut replied = 0u64;
    let mut errors = 0u64;
    for w in workers {
        let (p, s, r, e) = w.await?;
        polls += p;
        sent += s;
        replied += r;
        errors += e;
    }
    let elapsed = started.elapsed().as_secs_f64();

    let mut lat = latencies.lock().expect("latency mutex poisoned").clone();
    lat.sort_by(f64::total_cmp);
    let mean = if lat.is_empty() {
        0.0
    } else {
        lat.iter().sum::<f64>() / lat.len() as f64
    };
    let rate = polls as f64 / elapsed;
    // The occupancy identity: with `concurrency` probes always in flight, throughput is
    // `concurrency / mean latency`. Printing the implied figure beside the measured one makes a
    // disagreement visible — it would mean workers sat idle, i.e. the harness is the limit and the
    // run says nothing about the transport.
    let implied = if mean > 0.0 {
        concurrency as f64 / (mean / 1000.0)
    } else {
        0.0
    };
    let loss = if sent == 0 {
        0.0
    } else {
        (sent.saturating_sub(replied)) as f64 / sent as f64 * 100.0
    };
    let p50 = pct(&lat, 50.0);
    let p90 = pct(&lat, 90.0);
    let p99 = pct(&lat, 99.0);
    let max = pct(&lat, 100.0);

    println!("---- icmp_bench ----");
    println!("concurrency      {concurrency}");
    println!("elapsed_s        {elapsed:.1}");
    println!("polls            {polls}");
    println!("polls_per_sec    {rate:.1}");
    println!("implied_per_sec  {implied:.1}  (concurrency / mean latency — should match above)");
    println!(
        "latency_ms       mean {mean:.1}  p50 {p50:.1}  p90 {p90:.1}  p99 {p99:.1}  max {max:.1}"
    );
    println!("echoes_sent      {sent}");
    println!("echoes_replied   {replied}  (compare with the nstat IcmpInEchoReps delta)");
    println!("echo_loss_pct    {loss:.2}");
    println!("probe_errors     {errors}");
    println!("csv,{concurrency},{rate:.1},{mean:.1},{p50:.1},{p99:.1},{sent},{replied},{loss:.2}");
    Ok(())
}
