//! Passive-event UDP listeners: syslog and SNMP traps (Phase 2 passive monitoring).
//!
//! Each listener is a long-lived `recv_from` loop bound to an **unprivileged** container
//! port (compose maps host 514/162 onto it — the binary keeps only `cap_net_raw`). Every
//! received datagram is rate-limited per source IP ([`yagra_ingest::SourceLimiter`]),
//! parsed by the pure `yagra-ingest` parsers, normalized into an [`EventMsg`], and
//! published on `yagra.events` for core to match. Publish failures are logged and
//! counted, never fatal — UDP event delivery is best-effort by nature.
//!
//! Enabled per poller via env (`YAGRA_SYSLOG_BIND` / `YAGRA_TRAP_BIND`); unset = off.
//! This is site-deployment config, deliberately *not* config-over-bus (ADR-020 covers
//! per-device secrets, not which sockets a site's poller opens).
//!
//! **Log discipline:** raw message bodies only at `debug` (syslog routinely carries
//! credentials); the trap community value is never logged at any level.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use uuid::Uuid;
use yagra_bus::{Bus, EventKind, EventMsg, BUS_SCHEMA_VERSION};
use yagra_ingest::{
    build_inform_response, clip_event_text, parse_syslog, parse_trap, SourceLimiter, TrapError,
};

/// Syslog datagrams beyond this are truncated by the recv buffer (RFC 5424 transport
/// guidance; anything bigger than this over UDP is already pathological).
const SYSLOG_BUF_BYTES: usize = 8 * 1024;
/// Trap datagrams beyond this are rejected outright.
const TRAP_BUF_BYTES: usize = 64 * 1024;

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Run the syslog UDP listener until the socket errors persistently.
pub async fn run_syslog_listener<B: Bus>(
    sock: UdpSocket,
    bus: Arc<B>,
    limiter: Arc<Mutex<SourceLimiter>>,
    pool: Option<String>,
) {
    let mut buf = vec![0u8; SYSLOG_BUF_BYTES];
    loop {
        let (len, peer) = match sock.recv_from(&mut buf).await {
            Ok(ok) => ok,
            Err(e) => {
                // Transient errors (e.g. ICMP port-unreachable reflections on some
                // platforms) shouldn't kill the listener.
                tracing::warn!(error = %e, "syslog listener recv error");
                continue;
            }
        };
        metrics::counter!("yagra_events_received_total", "kind" => "syslog").increment(1);
        if !limiter.lock().await.allow(peer.ip(), now_unix_ms()) {
            metrics::counter!("yagra_events_dropped_total", "reason" => "rate_limit").increment(1);
            continue;
        }

        let parsed = parse_syslog(&buf[..len]);
        let event = EventMsg {
            schema_version: BUS_SCHEMA_VERSION,
            event_id: Uuid::new_v4(),
            kind: EventKind::Syslog,
            at_unix_ms: now_unix_ms(),
            source_ip: Some(peer.ip()),
            pool: pool.clone(),
            message: parsed.message,
            facility: parsed.facility,
            syslog_severity: parsed.severity,
            hostname: parsed.hostname,
            app_name: parsed.app_name,
            trap_oid: None,
            varbinds: Vec::new(),
            truncated: parsed.truncated,
        };
        publish(bus.as_ref(), event, peer.ip(), len).await;
    }
}

/// Run the SNMP trap/inform UDP listener until the socket errors persistently.
/// If `community` is set, traps with a different community are dropped (counted,
/// value never logged).
pub async fn run_trap_listener<B: Bus>(
    sock: UdpSocket,
    bus: Arc<B>,
    limiter: Arc<Mutex<SourceLimiter>>,
    community: Option<String>,
    pool: Option<String>,
) {
    let mut buf = vec![0u8; TRAP_BUF_BYTES];
    loop {
        let (len, peer) = match sock.recv_from(&mut buf).await {
            Ok(ok) => ok,
            Err(e) => {
                tracing::warn!(error = %e, "trap listener recv error");
                continue;
            }
        };
        metrics::counter!("yagra_events_received_total", "kind" => "trap").increment(1);
        if len >= TRAP_BUF_BYTES {
            metrics::counter!("yagra_events_dropped_total", "reason" => "oversize").increment(1);
            continue;
        }
        if !limiter.lock().await.allow(peer.ip(), now_unix_ms()) {
            metrics::counter!("yagra_events_dropped_total", "reason" => "rate_limit").increment(1);
            continue;
        }

        let trap = match parse_trap(&buf[..len]) {
            Ok(trap) => trap,
            Err(TrapError::UnsupportedVersion) => {
                // SNMPv3 traps are explicitly out of scope this release (USM keys on the
                // poller would conflict with ADR-020) — see yagra-ingest::trap.
                metrics::counter!("yagra_events_dropped_total", "reason" => "parse_error")
                    .increment(1);
                tracing::debug!(source = %peer.ip(), "dropping unsupported-version SNMP datagram");
                continue;
            }
            Err(e) => {
                metrics::counter!("yagra_events_dropped_total", "reason" => "parse_error")
                    .increment(1);
                tracing::debug!(source = %peer.ip(), error = %e, "dropping unparseable trap datagram");
                continue;
            }
        };

        if let Some(expected) = community.as_deref() {
            if trap.community != expected {
                metrics::counter!("yagra_events_dropped_total", "reason" => "community")
                    .increment(1);
                tracing::debug!(source = %peer.ip(), "dropping trap with mismatched community");
                continue;
            }
        }

        // Informs expect an acknowledgement Response (RFC 3416 §4.2.7) — best-effort.
        if trap.is_inform {
            if let Some(response) = build_inform_response(&buf[..len]) {
                if let Err(e) = sock.send_to(&response, peer).await {
                    tracing::debug!(source = %peer.ip(), error = %e, "failed to ack inform");
                }
            }
        }

        // A trap with many/large varbinds can render past the 4096-char event cap; clip
        // here (the syslog parser clips internally) so every message satisfies the DB CHECK.
        let (message, truncated) = clip_event_text(&trap.render_message());
        let event = EventMsg {
            schema_version: BUS_SCHEMA_VERSION,
            event_id: Uuid::new_v4(),
            kind: EventKind::Trap,
            at_unix_ms: now_unix_ms(),
            source_ip: Some(peer.ip()),
            pool: pool.clone(),
            message,
            facility: None,
            syslog_severity: None,
            hostname: None,
            app_name: None,
            trap_oid: Some(trap.trap_oid),
            varbinds: trap.varbinds,
            truncated,
        };
        publish(bus.as_ref(), event, peer.ip(), len).await;
    }
}

/// Publish one event; failures are counted + logged (never fatal).
async fn publish<B: Bus>(bus: &B, event: EventMsg, source: IpAddr, byte_len: usize) {
    let kind = event.kind;
    match bus.publish_event(event).await {
        Ok(()) => {
            tracing::debug!(%source, kind = kind.as_str(), byte_len, "event published");
        }
        Err(e) => {
            metrics::counter!("yagra_event_publish_failures_total").increment(1);
            tracing::warn!(%source, kind = kind.as_str(), error = %e, "failed to publish event");
        }
    }
}

/// Bind a UDP listener socket from an env-style bind string (e.g. `0.0.0.0:1514`,
/// `[::]:1162` for dual-stack).
pub async fn bind_udp(bind: &str) -> anyhow::Result<UdpSocket> {
    let addr: SocketAddr = bind
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid listener bind address {bind:?}: {e}"))?;
    Ok(UdpSocket::bind(addr).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_bus::InMemoryBus;

    fn limiter() -> Arc<Mutex<SourceLimiter>> {
        Arc::new(Mutex::new(SourceLimiter::new(50.0, 500.0, 0)))
    }

    /// End-to-end over a real UDP socket: datagram in → EventMsg on the bus.
    #[tokio::test]
    async fn syslog_datagram_becomes_bus_event() {
        let bus = Arc::new(InMemoryBus::new(8));
        let mut events = bus.subscribe_events();

        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = sock.local_addr().unwrap();
        tokio::spawn(run_syslog_listener(
            sock,
            bus.clone(),
            limiter(),
            Some("default".into()),
        ));

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client
            .send_to(
                b"<13>Jul  6 22:14:15 edge-sw1 chassisd: link down on ge-0/0/1",
                addr,
            )
            .await
            .unwrap();

        let event = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
            .await
            .expect("event within timeout")
            .unwrap();
        assert_eq!(event.kind, EventKind::Syslog);
        assert_eq!(event.message, "chassisd: link down on ge-0/0/1");
        assert_eq!(event.hostname.as_deref(), Some("edge-sw1"));
        assert_eq!(event.pool.as_deref(), Some("default"));
        assert!(event.source_ip.is_some());
    }

    #[tokio::test]
    async fn trap_datagram_becomes_bus_event_and_garbage_is_dropped() {
        let bus = Arc::new(InMemoryBus::new(8));
        let mut events = bus.subscribe_events();

        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = sock.local_addr().unwrap();
        tokio::spawn(run_trap_listener(
            sock,
            bus.clone(),
            limiter(),
            Some("public".into()),
            None,
        ));

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        // Garbage first — must be dropped without killing the listener.
        client.send_to(&[0xFF, 0x00, 0x13], addr).await.unwrap();
        // Wrong community — must be dropped too.
        client
            .send_to(&trap_bytes(b"wrong-community"), addr)
            .await
            .unwrap();
        // Then a well-formed linkDown trap with the right community.
        client.send_to(&trap_bytes(b"public"), addr).await.unwrap();

        let event = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
            .await
            .expect("event within timeout")
            .unwrap();
        assert_eq!(event.kind, EventKind::Trap);
        assert_eq!(event.trap_oid.as_deref(), Some("1.3.6.1.6.3.1.1.5.3"));
        assert!(event.message.starts_with("1.3.6.1.6.3.1.1.5.3"));
        // Nothing else queued: the garbage and wrong-community datagrams were dropped.
        assert!(events.try_recv().is_err());
    }

    fn trap_bytes(community: &[u8]) -> Vec<u8> {
        use snmp2::{pdu, snmp, Oid, Value, Version};
        let uptime_oid = Oid::from(&[1, 3, 6, 1, 2, 1, 1, 3, 0]).unwrap();
        let trapoid_oid = Oid::from(&[1, 3, 6, 1, 6, 3, 1, 1, 4, 1, 0]).unwrap();
        let link_down = Oid::from(&[1, 3, 6, 1, 6, 3, 1, 1, 5, 3]).unwrap();
        let varbinds: Vec<(&Oid, Value)> = vec![
            (&uptime_oid, Value::Timeticks(1)),
            (&trapoid_oid, Value::ObjectIdentifier(link_down.clone())),
        ];
        let mut buf = pdu::Buf::default();
        pdu::build(
            Version::V2C,
            community,
            snmp::MSG_TRAP,
            7,
            &varbinds,
            0,
            0,
            &mut buf,
            None,
        )
        .unwrap();
        buf[..].to_vec()
    }

    #[test]
    fn bind_rejects_malformed_address() {
        let err = futures::executor::block_on(bind_udp("not-an-addr")).unwrap_err();
        assert!(err.to_string().contains("invalid listener bind address"));
    }
}
