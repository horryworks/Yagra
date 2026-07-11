//! Synthetic syslog firehose — the S9/S11 passive-event scale & drop-rate harness.
//!
//! Sends RFC 5424 syslog datagrams over UDP to a poller's `YAGRA_SYSLOG_BIND` at a target rate,
//! spread across many synthetic source IPs, so the whole passive-event path is load-tested end to
//! end: the poller's per-source + global rate limiters (S8), its single receive task (S9), and the
//! NATS → core event pipeline (matcher + async action/persist writers, S10) and its best-effort
//! drop behaviour (S11). Point it at a running poller and watch both `/metrics` endpoints:
//!
//!   poller: `yagra_events_received_total{kind="syslog"}`         — datagrams the socket accepted
//!           `yagra_events_dropped_total{reason="rate_limit"}`    — shed by the limiter (S8)
//!   core:   `yagra_events_ingested_total{kind="syslog"}`         — reached the matcher
//!           `yagra_events_matched_total`                         — hit a rule
//!           `yagra_persist_queue_depth{stream="events"|"event_actions"}` — must stay BOUNDED
//!           `yagra_events_persist_dropped_total{reason=…}`       — persist shed (log store/PG)
//!
//! The gap between this firehose's **sent** rate and the core's `yagra_events_ingested_total` rate
//! is the aggregate drop — rate-limit (visible) + kernel UDP receive-buffer overflow (S9, invisible
//! in metrics) + NATS slow-consumer drop (S11). Measuring that aggregate at the target rate is exactly
//! what the "JetStream vs. stay best-effort" decision (ADR-024) needs; this harness is that measuring
//! stick. Because the poller's rate limiter keys on the **UDP source IP**, a single source would be
//! clamped at the per-source default (50/s); spreading across many loopback sources (127.0.0.0/8 on
//! Linux) exercises the global budget and a realistic multi-device mix.
//!
//! Run (against a poller listening on udp/5514 with `YAGRA_SYSLOG_BIND=0.0.0.0:5514`):
//!   cargo run --release --example event_firehose
//! Env knobs (all optional):
//!   YAGRA_SYSLOG_TARGET   poller host:port            (default 127.0.0.1:5514)
//!   FIREHOSE_RATE         target datagrams/sec         (default 1000)
//!   FIREHOSE_SECONDS      run duration; 0 = forever    (default 30)
//!   FIREHOSE_SOURCES      distinct synthetic source IPs (default 100)
//!   FIREHOSE_SOURCE_BASE  first source IP (walks upward) (default 127.0.0.2)
//!
//! Note: distinct sources come from binding local sockets across the loopback range, which works on
//! Linux (all of 127.0.0.0/8 is loopback) — the test server. On a host without spare loopback
//! addresses only the sources that bind are used (it logs how many); the rest fall back to one.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::net::UdpSocket;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

/// Bind `count` UDP sockets across the loopback range starting at `base`, so datagrams appear to the
/// poller as coming from distinct source IPs (distinct per-source rate-limit buckets). Sources that
/// can't bind (host has no spare loopback addresses) are skipped; if none bind, one `0.0.0.0:0`
/// socket is used so the harness still runs (all traffic then shares one per-source bucket).
async fn bind_sources(base: Ipv4Addr, count: usize) -> anyhow::Result<Vec<UdpSocket>> {
    let base_u32 = u32::from(base);
    let mut socks = Vec::with_capacity(count);
    for i in 0..count as u32 {
        let ip = Ipv4Addr::from(base_u32.wrapping_add(i));
        if let Ok(sock) = UdpSocket::bind(SocketAddr::from((ip, 0))).await {
            socks.push(sock);
        }
    }
    if socks.is_empty() {
        socks.push(UdpSocket::bind("0.0.0.0:0").await?);
    }
    Ok(socks)
}

/// One RFC 5424 syslog line. The `seq` is embedded in the message so successive datagrams differ —
/// otherwise the poller/core burst-dedup (identical kind+source+message within 5s) would collapse
/// the firehose into a trickle and mask the real throughput. PRI 134 = local0.info; timestamp is the
/// RFC 5424 NILVALUE (`-`), which the parser accepts.
fn syslog_line(host_idx: usize, seq: u64) -> String {
    format!(
        "<134>1 - host-{host_idx} firehose - - - synthetic event seq={seq} ts={}",
        now_ms()
    )
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let target: SocketAddr = std::env::var("YAGRA_SYSLOG_TARGET")
        .unwrap_or_else(|_| "127.0.0.1:5514".to_owned())
        .parse()
        .map_err(|e| anyhow::anyhow!("YAGRA_SYSLOG_TARGET must be host:port ({e})"))?;
    let rate = env_usize("FIREHOSE_RATE", 1000).max(1);
    let seconds = env_usize("FIREHOSE_SECONDS", 30);
    let source_count = env_usize("FIREHOSE_SOURCES", 100).max(1);
    let source_base: Ipv4Addr = std::env::var("FIREHOSE_SOURCE_BASE")
        .unwrap_or_else(|_| "127.0.0.2".to_owned())
        .parse()
        .map_err(|e| anyhow::anyhow!("FIREHOSE_SOURCE_BASE must be an IPv4 address ({e})"))?;

    let sources = bind_sources(source_base, source_count).await?;
    eprintln!(
        "event_firehose → {target}: {rate}/s for {}, across {} source IP(s) (requested {source_count})",
        if seconds == 0 {
            "∞".to_owned()
        } else {
            format!("{seconds}s")
        },
        sources.len()
    );

    // Pace in 50ms ticks so the load is smooth rather than one burst per second (mirrors
    // result_firehose). Each datagram round-robins over the bound source sockets.
    const TICKS_PER_SEC: usize = 20;
    let per_tick = rate.div_ceil(TICKS_PER_SEC);
    let mut ticker = tokio::time::interval(Duration::from_millis(1000 / TICKS_PER_SEC as u64));

    let start = Instant::now();
    let mut i: u64 = 0;
    let mut errors: u64 = 0;
    let mut sent_since_report: u64 = 0;
    let mut last_report = Instant::now();
    loop {
        ticker.tick().await;
        for _ in 0..per_tick {
            let idx = (i as usize) % sources.len();
            let line = syslog_line(idx, i);
            // Fire-and-forget: a send error here (e.g. ENOBUFS on the local socket) is itself part of
            // what we're measuring under load — count it, keep going, never block the pacer.
            if sources[idx].send_to(line.as_bytes(), target).await.is_err() {
                errors += 1;
            }
            i += 1;
            sent_since_report += 1;
        }
        if last_report.elapsed() >= Duration::from_secs(1) {
            let secs = last_report.elapsed().as_secs_f64();
            eprintln!(
                "  sent {i} total ({:.0}/s over last {:.1}s), {errors} send errors",
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
        "done: {i} datagrams in {:.1}s ({errors} local send errors)",
        start.elapsed().as_secs_f64()
    );
    Ok(())
}
