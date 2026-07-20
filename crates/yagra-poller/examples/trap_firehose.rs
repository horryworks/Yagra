// SPDX-License-Identifier: AGPL-3.0-only
//! Synthetic SNMP-trap firehose — the S9/S11 passive-trap scale & drop-rate harness. This is the
//! trap counterpart to `event_firehose.rs` (which does syslog) and the one harness the 2026-07
//! scalability audit flagged as still missing.
//!
//! Sends v2c SNMP trap datagrams over UDP to a poller's `YAGRA_TRAP_BIND` at a target rate, spread
//! across many synthetic source IPs, so the whole passive-trap path is load-tested end to end: the
//! poller's per-source + global rate limiters (S8), its receive tasks (S9), the `snmp2` decode +
//! normalize, and the NATS → core event pipeline (matcher + async writers, S10) and its best-effort
//! drop behaviour (S11). Each trap carries a per-datagram sequence varbind, so the poller/core
//! burst-dedup (identical kind+source+message within 5s) can't collapse the firehose into a trickle.
//!
//! Watch both `/metrics` endpoints (mirrors event_firehose):
//!   poller: `yagra_events_received_total{kind="trap"}`   — datagrams the socket accepted
//!           `yagra_events_dropped_total{reason="rate_limit"}` — shed by the limiter (S8)
//!   core:   `yagra_events_ingested_total{kind="trap"}`   — reached the matcher
//!           `yagra_persist_queue_depth{stream="events"|"event_actions"}` — must stay BOUNDED
//!
//! The gap between this firehose's **sent** rate and core `yagra_events_ingested_total{kind="trap"}`
//! is the aggregate drop — rate-limit (visible) + kernel UDP receive-buffer overflow (S9, invisible
//! in metrics) + NATS slow-consumer drop (S11). Measuring that aggregate at the target rate is the
//! ADR-024 "JetStream vs. stay best-effort" decision for the trap path. Because the poller's rate
//! limiter keys on the UDP source IP, spreading across many loopback sources (127.0.0.0/8 on Linux)
//! exercises the global budget and a realistic multi-device mix.
//!
//! Run (against a poller listening with `YAGRA_TRAP_BIND=0.0.0.0:1162`):
//!   cargo run --release --example trap_firehose
//! Env knobs (all optional):
//!   YAGRA_TRAP_TARGET    poller host:port              (default 127.0.0.1:1162)
//!   FIREHOSE_RATE        target traps/sec               (default 1000)
//!   FIREHOSE_SECONDS     run duration; 0 = forever       (default 30)
//!   FIREHOSE_SOURCES     distinct synthetic source IPs   (default 100)
//!   FIREHOSE_SOURCE_BASE first source IP (walks upward)   (default 127.0.0.2)
//!
//! Note: distinct sources come from binding local sockets across the loopback range, which works on
//! Linux (all of 127.0.0.0/8 is loopback) — the test server. On a host without spare loopback
//! addresses only the sources that bind are used (it logs how many); the rest fall back to one.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use snmp2::{pdu, snmp, Oid, Value, Version};
use tokio::net::UdpSocket;

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
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

/// Build one v2c linkDown trap datagram carrying a per-datagram `seq` varbind, so the rendered event
/// message differs each send and burst-dedup can't collapse the stream. Mirrors the fixture builder
/// in `yagra-ingest::trap` tests: the standard sysUpTime.0 + snmpTrapOID.0 identity varbinds plus one
/// extra (ifIndex carrying the sequence). Uses `snmp2`'s widened `pdu::build` — no hand-rolled ASN.1.
fn trap_datagram(seq: u64) -> Vec<u8> {
    let uptime_oid = Oid::from(&[1, 3, 6, 1, 2, 1, 1, 3, 0]).unwrap();
    let trapoid_oid = Oid::from(&[1, 3, 6, 1, 6, 3, 1, 1, 4, 1, 0]).unwrap();
    // linkDown (1.3.6.1.6.3.1.1.5.3).
    let identity = Oid::from(&[1, 3, 6, 1, 6, 3, 1, 1, 5, 3]).unwrap();
    // ifDescr.<n> as the varying varbind — the rendered message includes it, defeating dedup.
    let ifdescr_oid = Oid::from(&[1, 3, 6, 1, 2, 1, 2, 2, 1, 2, 1]).unwrap();
    let seq_str = format!("firehose-{seq}");
    let varbinds: Vec<(&Oid, Value)> = vec![
        (&uptime_oid, Value::Timeticks(seq as u32)),
        (&trapoid_oid, Value::ObjectIdentifier(identity.clone())),
        (&ifdescr_oid, Value::OctetString(seq_str.as_bytes())),
    ];
    let mut buf = pdu::Buf::default();
    // The trailing `None` is snmp2's v3 security param (its `v3` feature is always on in this
    // workspace); v2c ignores it.
    pdu::build(
        Version::V2C,
        b"public",
        snmp::MSG_TRAP,
        42,
        &varbinds,
        0,
        0,
        &mut buf,
        None,
    )
    .expect("build v2c trap");
    buf[..].to_vec()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let target: SocketAddr = std::env::var("YAGRA_TRAP_TARGET")
        .unwrap_or_else(|_| "127.0.0.1:1162".to_owned())
        .parse()
        .map_err(|e| anyhow::anyhow!("YAGRA_TRAP_TARGET must be host:port ({e})"))?;
    let rate = env_usize("FIREHOSE_RATE", 1000).max(1);
    let seconds = env_usize("FIREHOSE_SECONDS", 30);
    let source_count = env_usize("FIREHOSE_SOURCES", 100).max(1);
    let source_base: Ipv4Addr = std::env::var("FIREHOSE_SOURCE_BASE")
        .unwrap_or_else(|_| "127.0.0.2".to_owned())
        .parse()
        .map_err(|e| anyhow::anyhow!("FIREHOSE_SOURCE_BASE must be an IPv4 address ({e})"))?;

    let sources = bind_sources(source_base, source_count).await?;
    eprintln!(
        "trap_firehose → {target}: {rate}/s for {}, across {} source IP(s) (requested {source_count})",
        if seconds == 0 {
            "∞".to_owned()
        } else {
            format!("{seconds}s")
        },
        sources.len()
    );

    // Pace in 50ms ticks so the load is smooth rather than one burst per second (mirrors
    // event_firehose). Each datagram round-robins over the bound source sockets.
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
            let datagram = trap_datagram(i);
            // Fire-and-forget: a send error here (e.g. ENOBUFS on the local socket) is itself part of
            // what we're measuring under load — count it, keep going, never block the pacer.
            if sources[idx].send_to(&datagram, target).await.is_err() {
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
        "done: {i} trap datagrams in {:.1}s ({errors} local send errors)",
        start.elapsed().as_secs_f64()
    );
    Ok(())
}
