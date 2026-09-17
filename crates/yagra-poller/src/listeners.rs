// SPDX-License-Identifier: AGPL-3.0-only
//! Passive-event UDP listeners: syslog and SNMP traps (Phase 2 passive monitoring).
//!
//! Each protocol runs **N parallel `recv_from` loops** bound to the same **unprivileged**
//! container port via `SO_REUSEPORT` (compose maps host 514/162 onto it — the binary keeps
//! only `cap_net_raw`). The kernel load-balances arriving datagrams across the N sockets,
//! each of which has its own kernel receive queue drained by its own task — so a momentary
//! stall in one reader (e.g. a slow NATS publish) never backs up the others' queues, and the
//! single-task receive ceiling is lifted (S9). Each socket also requests an enlarged
//! `SO_RCVBUF` so bursts are absorbed by the kernel instead of silently dropped. On non-Unix
//! (the dev box) `SO_REUSEPORT` is unavailable, so a single socket is used regardless.
//!
//! Every received datagram is rate-limited per source IP ([`yagra_ingest::SourceLimiter`],
//! shared across all readers behind a `std::sync::Mutex` — the critical section is a few
//! arithmetic ops and is never held across an `.await`, so the global budget stays exact
//! without async-lock overhead, S22), parsed by the pure `yagra-ingest` parsers, normalized
//! into an [`EventMsg`], and published on `yagra.events` for core to match. Publish failures
//! are logged and counted, never fatal — UDP event delivery is best-effort by nature.
//!
//! **Nothing a datagram does can end a reader** (ADR-158). One datagram's parsing runs inside
//! [`contained`], so a panic there is `yagra_edge_datagram_panics_total` and a dropped datagram.
//! And each reader is started by [`spawn_supervised`] over an `Arc` of its socket, so a reader that
//! dies anyway is started again on the same socket. Before both, one crafted SNMP inform per reader
//! stopped trap reception for the life of the process while the heartbeat went on reporting the
//! listener as bound.
//!
//! The **original datagram** rides along on `EventMsg.raw` (base64, ADR-034) so core can forward
//! it byte-exact to external collectors. It is attached *after* the rate-limit / oversize /
//! parse / community gates, so anything Yagra dropped is never forwarded either. Cost is ~1.3×
//! on `yagra.events`; every other field is lossy (lossy-UTF-8, 4096-char clip, 32-varbind cap),
//! which is exactly why the bytes have to be carried rather than reconstructed.
//!
//! Enabled per poller via env (`YAGRA_SYSLOG_BIND` / `YAGRA_TRAP_BIND`); unset = off.
//! This is site-deployment config, deliberately *not* config-over-bus (ADR-020 covers
//! per-device secrets, not which sockets a site's poller opens).
//!
//! **Log discipline:** raw message bodies only at `debug` (syslog routinely carries
//! credentials); the trap community value is never logged at any level.

use socket2::{Domain, Protocol, Socket, Type};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;
use uuid::Uuid;
use yagra_bus::{encode_raw, Bus, EventKind, EventMsg};
use yagra_ingest::{
    build_inform_response, clip_event_text, parse_syslog, parse_trap, SourceLimiter, TrapError,
    TrapEvent,
};
use yagra_telemetry::{spawn_supervised, CancellationToken};

/// Syslog datagrams beyond this are truncated by the recv buffer (RFC 5424 transport
/// guidance; anything bigger than this over UDP is already pathological).
const SYSLOG_BUF_BYTES: usize = 8 * 1024;
/// Trap datagrams beyond this are rejected outright.
const TRAP_BUF_BYTES: usize = 64 * 1024;

pub(crate) fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Run `f` — one datagram's worth of parsing — and turn a panic inside it into a counted drop.
///
/// The second layer of ADR-158 決定 2. [`spawn_supervised`] brings a reader back after a panic, but
/// only after a wait, and a datagram that panics its parser would otherwise take the reader down
/// every time it is re-sent. Contained, a hostile datagram costs itself and nothing else:
/// `yagra_edge_datagram_panics_total{listener}` goes up by one, the panic hook has logged its
/// text, and the loop reads the next one.
///
/// `AssertUnwindSafe` is sound here because nothing `f` touches outlives a panic in a broken
/// state: the parsers are pure, and the flow listener's shared state sits behind a
/// `std::sync::Mutex` whose poisoning every lock site already recovers from.
pub(crate) fn contained<T>(listener: &'static str, f: impl FnOnce() -> T) -> Option<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(value) => Some(value),
        Err(_) => {
            metrics::counter!("yagra_edge_datagram_panics_total", "listener" => listener)
                .increment(1);
            None
        }
    }
}

/// Run the syslog UDP listener until the socket errors persistently.
///
/// The socket is shared rather than owned so that [`spawn_supervised`] can start a replacement
/// reader on the same socket after a panic (ADR-158) — the kernel queue keeps what arrived while
/// no reader was running.
pub async fn run_syslog_listener<B: Bus>(
    sock: Arc<UdpSocket>,
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
        // Sync critical section (a few arithmetic ops) — the guard is dropped at the end of
        // this `if` condition, never held across the `.await` in `publish` below (S22).
        if !allow(&limiter, peer.ip()) {
            metrics::counter!("yagra_events_dropped_total", "reason" => "rate_limit").increment(1);
            continue;
        }

        let Some(parsed) = contained("syslog", || parse_syslog(&buf[..len])) else {
            continue;
        };
        let event = EventMsg {
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
            // The original datagram, for byte-exact forwarding (ADR-034). Encoded here because
            // `buf` is reused by the next `recv_from` — nothing downstream can recover it.
            raw: Some(encode_raw(&buf[..len])),
            src_port: Some(peer.port()),
        };
        publish(bus.as_ref(), event, peer.ip(), len).await;
    }
}

/// One trap datagram that passed every gate, with what the listener still has to send.
struct AdmittedTrap {
    trap: TrapEvent,
    /// The rendered, clipped event message.
    message: String,
    truncated: bool,
    /// The Response to send back when the datagram was an inform.
    ack: Option<Vec<u8>>,
}

/// Why a trap datagram was not admitted.
enum TrapRefusal {
    UnsupportedVersion,
    Unparseable(TrapError),
    Community,
}

/// Everything the trap listener does with one datagram that is not I/O: parse it, check its
/// community, build the inform ack, render the message. One synchronous call, so that
/// [`contained`] covers all of it at once (ADR-158).
fn admit_trap(datagram: &[u8], community: Option<&str>) -> Result<AdmittedTrap, TrapRefusal> {
    let trap = parse_trap(datagram).map_err(|e| match e {
        TrapError::UnsupportedVersion => TrapRefusal::UnsupportedVersion,
        other @ (TrapError::Malformed(_) | TrapError::NotATrap) => TrapRefusal::Unparseable(other),
    })?;
    if let Some(expected) = community {
        if trap.community != expected {
            return Err(TrapRefusal::Community);
        }
    }
    // Informs expect an acknowledgement Response (RFC 3416 §4.2.7) — built only once the
    // community has been accepted, so a stranger's inform is never answered.
    let ack = if trap.is_inform {
        build_inform_response(datagram)
    } else {
        None
    };
    // A trap with many/large varbinds can render past the 4096-char event cap; clip
    // here (the syslog parser clips internally) so every message satisfies the DB CHECK.
    let (message, truncated) = clip_event_text(&trap.render_message());
    Ok(AdmittedTrap {
        trap,
        message,
        truncated,
        ack,
    })
}

/// Run the SNMP trap/inform UDP listener until the socket errors persistently.
/// If `community` is set, traps with a different community are dropped (counted,
/// value never logged). The socket is shared for the reason [`run_syslog_listener`] gives.
pub async fn run_trap_listener<B: Bus>(
    sock: Arc<UdpSocket>,
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
        if !allow(&limiter, peer.ip()) {
            metrics::counter!("yagra_events_dropped_total", "reason" => "rate_limit").increment(1);
            continue;
        }

        let admitted = match contained("trap", || admit_trap(&buf[..len], community.as_deref())) {
            // Already counted by `contained`.
            None => continue,
            Some(Ok(admitted)) => admitted,
            Some(Err(TrapRefusal::UnsupportedVersion)) => {
                // SNMPv3 traps are explicitly out of scope this release (USM keys on the
                // poller would conflict with ADR-020) — see yagra-ingest::trap.
                metrics::counter!("yagra_events_dropped_total", "reason" => "parse_error")
                    .increment(1);
                tracing::debug!(source = %peer.ip(), "dropping unsupported-version SNMP datagram");
                continue;
            }
            Some(Err(TrapRefusal::Unparseable(e))) => {
                metrics::counter!("yagra_events_dropped_total", "reason" => "parse_error")
                    .increment(1);
                tracing::debug!(source = %peer.ip(), error = %e, "dropping unparseable trap datagram");
                continue;
            }
            Some(Err(TrapRefusal::Community)) => {
                metrics::counter!("yagra_events_dropped_total", "reason" => "community")
                    .increment(1);
                tracing::debug!(source = %peer.ip(), "dropping trap with mismatched community");
                continue;
            }
        };
        let AdmittedTrap {
            trap,
            message,
            truncated,
            ack,
        } = admitted;

        // Best-effort: a lost ack makes the sender retransmit, which is the protocol's own remedy.
        if let Some(response) = ack {
            if let Err(e) = sock.send_to(&response, peer).await {
                tracing::debug!(source = %peer.ip(), error = %e, "failed to ack inform");
            }
        }

        let event = EventMsg {
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
            // The original PDU, for byte-exact forwarding (ADR-034). Doubly worth carrying for
            // traps: the parsed form keeps only the first 32 varbinds, clipped to 256 chars.
            raw: Some(encode_raw(&buf[..len])),
            src_port: Some(peer.port()),
        };
        publish(bus.as_ref(), event, peer.ip(), len).await;
    }
}

/// Take one token for `source` from the shared limiter. Uses a `std::sync::Mutex` (not the
/// async one) because `SourceLimiter::allow` is synchronous and the critical section is tiny —
/// the guard is created and dropped entirely within this call, so it is never held across an
/// `.await` (S22). Poison-tolerant: a limiter never panics while holding the lock, but if some
/// future change ever did, recovering the inner value keeps intake alive rather than crashing
/// every reader.
pub(crate) fn allow(limiter: &Mutex<SourceLimiter>, source: IpAddr) -> bool {
    limiter
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .allow(source, now_unix_ms())
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

/// Bind `workers` UDP listener sockets to the same env-style bind address (e.g. `0.0.0.0:1514`,
/// `[::]:1162` for dual-stack) with `SO_REUSEPORT`, so the kernel load-balances datagrams across
/// N independent receive queues (S9). Each socket requests `rcvbuf_bytes` of `SO_RCVBUF` so bursts
/// are buffered by the kernel rather than dropped. Returns one socket per reader task.
///
/// `SO_REUSEPORT` is Unix-only; on other platforms (the dev box) a single socket is returned
/// regardless of `workers`, since a second bind to the same port would fail without it.
pub async fn bind_reuseport(
    bind: &str,
    workers: usize,
    rcvbuf_bytes: usize,
) -> anyhow::Result<Vec<UdpSocket>> {
    let addr: SocketAddr = bind
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid listener bind address {bind:?}: {e}"))?;
    let effective = if cfg!(unix) { workers.max(1) } else { 1 };
    // Bind the first socket to resolve an ephemeral (`:0`, in tests) port to a concrete one, then
    // bind the remaining sockets to that exact address so they all share the same port.
    let first = make_reuseport_socket(addr, rcvbuf_bytes)?;
    let bound = first.local_addr()?;
    let mut socks = Vec::with_capacity(effective);
    socks.push(first);
    for _ in 1..effective {
        socks.push(make_reuseport_socket(bound, rcvbuf_bytes)?);
    }
    Ok(socks)
}

/// Build one `SO_REUSEPORT` + enlarged-`SO_RCVBUF` UDP socket bound to `addr`.
fn make_reuseport_socket(addr: SocketAddr, rcvbuf_bytes: usize) -> anyhow::Result<UdpSocket> {
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let sock = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    // SO_REUSEPORT lets the N reader sockets share the port; the kernel spreads datagrams across
    // them. Unix-only — the `#[cfg]` keeps this compiling on the Windows dev box.
    #[cfg(unix)]
    sock.set_reuse_port(true)?;
    // Best-effort: a larger receive buffer reduces silent kernel drops under burst. The kernel
    // clamps the request to `net.core.rmem_max`; failing to raise it is non-fatal.
    if let Err(e) = sock.set_recv_buffer_size(rcvbuf_bytes) {
        tracing::debug!(error = %e, rcvbuf_bytes, "could not enlarge SO_RCVBUF (kernel clamp?)");
    }
    // Keep the historical dual-stack behaviour for `[::]` binds (tokio's default).
    if addr.is_ipv6() {
        let _ = sock.set_only_v6(false);
    }
    // tokio requires a non-blocking socket for `from_std`.
    sock.set_nonblocking(true)?;
    sock.bind(&addr.into())?;
    let std_sock: std::net::UdpSocket = sock.into();
    Ok(UdpSocket::from_std(std_sock)?)
}

/// The three knobs both UDP edge listeners share.
///
/// syslog/trap and the flow collector open their sockets the same way and are tuned the same way.
/// Everything else about them is separate — their own rate limiters, their own env knobs, their own
/// subjects — which is why exactly these three travel between the two and nothing else does.
pub(crate) struct EdgeTuning {
    /// Parallel `recv_from` readers per listener (S9).
    pub(crate) workers: usize,
    /// `SO_RCVBUF` per socket, so a burst is absorbed by the kernel instead of silently dropped.
    pub(crate) rcvbuf: usize,
    /// The **raw** `YAGRA_POLLER_POOL`, not the defaulted one: an event's `pool` is stored NULL
    /// core-side when it is unset, and only job subscription uses the default.
    pub(crate) pool: Option<String>,
}

impl EdgeTuning {
    /// Read once, at startup, by the caller that starts both listeners.
    ///
    /// ⚠️ Read unconditionally, where the single `spawn_event_listeners` used to reach it only after
    /// finding a bind address. The whole difference is one `available_parallelism()` call at boot in
    /// a deployment that configures no listener at all, and its answer is discarded.
    pub(crate) fn from_env() -> Self {
        Self {
            workers: env_listener_workers(),
            rcvbuf: crate::env_usize("YAGRA_LISTENER_RCVBUF_BYTES", 4 * 1024 * 1024),
            pool: crate::env_nonempty("YAGRA_POLLER_POOL"),
        }
    }
}

/// Number of parallel `recv_from` readers per edge listener (S9). Defaults to the host's parallelism
/// capped at 4 (a single poller sustains tens of thousands of msg/s well before this matters); env
/// `YAGRA_LISTENER_WORKERS` overrides. Non-Unix collapses to one socket in `bind_reuseport` anyway.
fn env_listener_workers() -> usize {
    let default = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, 4);
    crate::env_usize("YAGRA_LISTENER_WORKERS", default).max(1)
}

/// Bind and spawn the syslog / SNMP-trap listeners for every address configured via env, returning a
/// label per listener that actually bound (for the heartbeat's `listeners` telemetry).
///
/// Unset (or empty) env = listener disabled. Both share one rate limiter, so the global budget
/// covers all passive **event** intake on this poller; the flow collector has its own, for the
/// reason [`crate::flow::start`] gives.
pub(crate) async fn start(
    bus: &Arc<yagra_bus::NatsBus>,
    shutdown: &CancellationToken,
    tuning: &EdgeTuning,
) -> Vec<String> {
    let syslog_bind = crate::env_nonempty("YAGRA_SYSLOG_BIND");
    let trap_bind = crate::env_nonempty("YAGRA_TRAP_BIND");
    if syslog_bind.is_none() && trap_bind.is_none() {
        return Vec::new();
    }

    // Edge intake caps (S8). Raised from the original 50/500 after the 2026-07-11 load test showed
    // the core matcher + single NATS event subscriber sustain ≥27k msg/s with zero NATS drop (the
    // real ceiling is the async persist writer, which sheds best-effort past it) — so the old
    // defaults dropped 75-97% of a realistic multi-device / chassis-storm flow for no protective
    // benefit. A chassis router's syslog burst now fits per-source; the global cap stays well under
    // the measured drain limit. Both remain env-tunable per deployment.
    let per_source = crate::env_f64("YAGRA_EVENT_RATE_PER_SOURCE", 200.0);
    let global = crate::env_f64("YAGRA_EVENT_RATE_GLOBAL", 5000.0);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    // One shared limiter behind a `std::sync::Mutex` (S22): its critical section is a few
    // arithmetic ops and is never held across an await, so all readers share the exact global
    // budget without async-lock overhead. Sharing (not sharding) keeps the global rate correct.
    let limiter = Arc::new(std::sync::Mutex::new(yagra_ingest::SourceLimiter::new(
        per_source, global, now_ms,
    )));

    let mut labels = Vec::new();

    if let Some(bind) = syslog_bind {
        match bind_reuseport(&bind, tuning.workers, tuning.rcvbuf).await {
            Ok(socks) => {
                let n = socks.len();
                tracing::info!(%bind, workers = n, rcvbuf = tuning.rcvbuf, per_source, global, "syslog listener enabled");
                labels.push(format!("syslog:{bind}"));
                for sock in socks {
                    // Supervised (ADR-158): a reader that panics is started again on the same
                    // socket, so the label above stays true.
                    let sock = Arc::new(sock);
                    let (bus, limiter, pool) = (bus.clone(), limiter.clone(), tuning.pool.clone());
                    spawn_supervised(shutdown, "syslog_listener", move || {
                        run_syslog_listener(
                            sock.clone(),
                            bus.clone(),
                            limiter.clone(),
                            pool.clone(),
                        )
                    });
                }
            }
            Err(e) => tracing::error!(%bind, error = %e, "failed to bind syslog listener"),
        }
    }

    if let Some(bind) = trap_bind {
        // Optional community filter — value must never be logged.
        let community = crate::env_nonempty("YAGRA_TRAP_COMMUNITY");
        match bind_reuseport(&bind, tuning.workers, tuning.rcvbuf).await {
            Ok(socks) => {
                let n = socks.len();
                tracing::info!(%bind, workers = n, rcvbuf = tuning.rcvbuf, community_filter = community.is_some(), "trap listener enabled (v1/v2c; v3 traps out of scope)");
                labels.push(format!("trap:{bind}"));
                for sock in socks {
                    let sock = Arc::new(sock);
                    let (bus, limiter, community, pool) = (
                        bus.clone(),
                        limiter.clone(),
                        community.clone(),
                        tuning.pool.clone(),
                    );
                    spawn_supervised(shutdown, "trap_listener", move || {
                        run_trap_listener(
                            sock.clone(),
                            bus.clone(),
                            limiter.clone(),
                            community.clone(),
                            pool.clone(),
                        )
                    });
                }
            }
            Err(e) => tracing::error!(%bind, error = %e, "failed to bind trap listener"),
        }
    }

    labels
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

        let sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
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

        let sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
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

    /// A v2c inform that `snmp2` re-encodes larger than it arrived: 8,000 varbinds whose Timeticks
    /// is sent in one byte (`43 01 FF`) and re-encoded in five. Re-encoding it outgrew the encoder's
    /// fixed buffer and panicked the reader that received it (ADR-158).
    fn crafted_inform(community: &[u8]) -> Vec<u8> {
        fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
            let mut out = vec![tag];
            match content.len() {
                n @ 0..=0x7F => out.push(n as u8),
                n @ 0x80..=0xFF => out.extend([0x81, n as u8]),
                n => out.extend([0x82, (n >> 8) as u8, n as u8]),
            }
            out.extend_from_slice(content);
            out
        }
        let varbinds = [0x30, 0x06, 0x06, 0x01, 0x2B, 0x43, 0x01, 0xFF].repeat(8_000);
        let mut pdu = Vec::new();
        pdu.extend(tlv(0x02, &[0x09])); // request-id
        pdu.extend(tlv(0x02, &[0x00])); // error-status
        pdu.extend(tlv(0x02, &[0x00])); // error-index
        pdu.extend(tlv(0x30, &varbinds));
        let mut message = Vec::new();
        message.extend(tlv(0x02, &[0x01])); // v2c
        message.extend(tlv(0x04, community));
        message.extend(tlv(snmp2::snmp::MSG_INFORM, &pdu));
        tlv(0x30, &message)
    }

    /// The datagram that stopped a trap reader for good is acknowledged, and the same reader goes
    /// on to turn the next trap into an event. The reader runs **unsupervised** here, so this is
    /// the inform fix itself and not the supervisor covering for it.
    #[tokio::test]
    async fn a_crafted_inform_does_not_stop_the_trap_listener() {
        let bus = Arc::new(InMemoryBus::new(16));
        let mut events = bus.subscribe_events();

        let sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let addr = sock.local_addr().unwrap();
        tokio::spawn(run_trap_listener(
            sock,
            bus.clone(),
            limiter(),
            Some("public".into()),
            None,
        ));

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let crafted = crafted_inform(b"public");
        assert!(crafted.len() < 65_507, "must fit in one UDP datagram");
        client.send_to(&crafted, addr).await.unwrap();

        let mut ack = vec![0u8; 70_000];
        let (n, _) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.recv_from(&mut ack),
        )
        .await
        .expect("the crafted inform is acknowledged")
        .unwrap();
        assert_eq!(n, crafted.len(), "the ack is the inform, retagged");
        assert_eq!(ack[0], 0x30);

        client.send_to(&trap_bytes(b"public"), addr).await.unwrap();
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
                .await
                .expect("the reader is still alive and publishes the next trap")
                .unwrap();
            if event.trap_oid.as_deref() == Some("1.3.6.1.6.3.1.1.5.3") {
                break;
            }
        }
    }

    /// The second layer: a reader that dies is started again **on the same socket**, and a datagram
    /// sent after the panic still becomes an event — the kernel queue kept it while no reader ran.
    #[tokio::test]
    async fn a_reader_that_died_is_replaced_on_the_same_socket() {
        type Task = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
        let bus = Arc::new(InMemoryBus::new(8));
        let mut events = bus.subscribe_events();
        let shutdown = CancellationToken::new();

        let sock = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let addr = sock.local_addr().unwrap();
        let starts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (counted, reader_bus, reader_limiter) = (starts.clone(), bus.clone(), limiter());
        spawn_supervised(&shutdown, "test_syslog_listener", move || -> Task {
            let n = counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            if n == 1 {
                return Box::pin(async { panic!("the first reader dies") });
            }
            Box::pin(run_syslog_listener(
                sock.clone(),
                reader_bus.clone(),
                reader_limiter.clone(),
                Some("default".into()),
            ))
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while starts.load(std::sync::atomic::Ordering::SeqCst) < 1 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client
            .send_to(
                b"<13>Jul  6 22:14:15 edge-sw1 chassisd: after the panic",
                addr,
            )
            .await
            .unwrap();
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
            .await
            .expect("the replacement reader receives it")
            .unwrap();
        assert_eq!(event.message, "chassisd: after the panic");
        assert!(starts.load(std::sync::atomic::Ordering::SeqCst) >= 2);
        shutdown.cancel();
    }

    /// A panic inside one datagram's parsing is a counted drop, not a dead reader.
    #[test]
    fn a_panic_inside_one_datagram_is_a_counted_drop() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        let (survived, dropped) = metrics::with_local_recorder(&recorder, || {
            (
                contained("trap", || 7),
                contained("trap", || -> u8 { panic!("a hostile datagram") }),
            )
        });
        assert_eq!(survived, Some(7));
        assert_eq!(dropped, None);
        let rendered = handle.render();
        assert!(
            rendered
                .lines()
                .any(|l| l == "yagra_edge_datagram_panics_total{listener=\"trap\"} 1"),
            "{rendered}"
        );
    }

    #[test]
    fn bind_rejects_malformed_address() {
        let err =
            futures::executor::block_on(bind_reuseport("not-an-addr", 1, 1 << 20)).unwrap_err();
        assert!(err.to_string().contains("invalid listener bind address"));
    }

    /// The parallel bind opens the requested number of sockets, all sharing one concrete port.
    /// On non-Unix (no `SO_REUSEPORT`) it collapses to a single socket regardless of `workers`.
    #[tokio::test]
    async fn bind_reuseport_shares_one_port_across_readers() {
        let socks = bind_reuseport("127.0.0.1:0", 4, 1 << 20)
            .await
            .expect("bind should succeed");
        let expected = if cfg!(unix) { 4 } else { 1 };
        assert_eq!(socks.len(), expected);
        // Every reader socket is bound to the same port (the ephemeral one the first resolved).
        let port = socks[0].local_addr().unwrap().port();
        assert_ne!(port, 0);
        for s in &socks {
            assert_eq!(s.local_addr().unwrap().port(), port);
        }
    }

    /// A datagram sent to the shared port is delivered to exactly one of the N parallel readers
    /// (kernel `SO_REUSEPORT` load-balancing) and still becomes a bus event end-to-end.
    #[tokio::test]
    async fn parallel_readers_receive_over_shared_port() {
        let bus = Arc::new(InMemoryBus::new(8));
        let mut events = bus.subscribe_events();

        let socks = bind_reuseport("127.0.0.1:0", 3, 1 << 20).await.unwrap();
        let addr = socks[0].local_addr().unwrap();
        let limiter = limiter();
        for sock in socks {
            tokio::spawn(run_syslog_listener(
                Arc::new(sock),
                bus.clone(),
                limiter.clone(),
                Some("default".into()),
            ));
        }

        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client
            .send_to(b"<13>Jul  6 22:14:15 edge-sw1 chassisd: link down", addr)
            .await
            .unwrap();

        let event = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
            .await
            .expect("event within timeout")
            .unwrap();
        assert_eq!(event.kind, EventKind::Syslog);
        assert_eq!(event.message, "chassisd: link down");
    }
}
