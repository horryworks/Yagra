// SPDX-License-Identifier: AGPL-3.0-only
//! Real ICMP transport: echo requests over an ICMP socket, written here (ADR-213 decision 5; it
//! was the `surge-ping` crate, whose design it keeps).
//!
//! This is the production [`Transport`] for liveness/RTT. It needs `CAP_NET_RAW` (or a
//! `ping_group_range` that admits the poller) at runtime — granted to the poller container only,
//! see `.claude/rules/security.md` — so the socket loop cannot be exercised in a normal unit test;
//! the privilege-free `FakeTransport` covers poller logic. The pure pieces — building a request,
//! reading a reply, [`summarize`], [`probe_exhausted`] — *are* unit-tested.
//!
//! The shape, one socket per address family:
//! - **The socket** is a Linux "ping socket" (`SOCK_DGRAM` + `IPPROTO_ICMP`) when the kernel allows
//!   one, else `SOCK_RAW`. On a ping socket the kernel owns the echo identifier and strips the IP
//!   header; on a raw socket we set the identifier and the IPv4 header arrives in front.
//! - **One receiver task** reads every reply and hands it to whoever is waiting for
//!   `(host, identifier, sequence)` — the identifier left out on a ping socket, where the kernel
//!   rewrites it. That is why overlapping probes to one host take disjoint sequence numbers
//!   (`next_sequences`).
//! - **Only an echo reply counts.** `surge-ping` also counted an ICMP error sent by the target
//!   itself as a reply on a raw socket; a ping socket never delivers errors, so production — which
//!   runs on one — sees no difference.
//!
//! No `unsafe`: `socket2::Socket` becomes a `std::net::UdpSocket` through `From`, which is what
//! lets Tokio drive it.
//!
//! Per ADR-012 this layer collects **raw** observations only (reachability, per-probe
//! RTT, loss %) — it derives no rates and holds no previous-value state.

use crate::{IcmpProbe, Transport, TransportError};
use async_trait::async_trait;
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// ICMP echo payload size in bytes (a conventional 56-byte body → 64-byte packet).
const PAYLOAD_LEN: usize = 56;

const ICMPV4_ECHO_REQUEST: u8 = 8;
const ICMPV4_ECHO_REPLY: u8 = 0;
const ICMPV6_ECHO_REQUEST: u8 = 128;
const ICMPV6_ECHO_REPLY: u8 = 129;

/// Which ICMP: v4 or v6. The two differ in type numbers, in who computes the checksum, and in
/// whether a raw socket hands us the IP header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    V4,
    V6,
}

/// What a waiting probe is keyed by: the host, the identifier (`None` on a ping socket, where the
/// kernel rewrites it), and the sequence number.
type ReplyKey = (IpAddr, Option<u16>, u16);
type Waiters = Arc<Mutex<HashMap<ReplyKey, oneshot::Sender<Instant>>>>;

/// Why one echo did not complete. Only ever logged at `debug`.
#[derive(Debug)]
enum EchoError {
    TimedOut,
    /// Another probe is already waiting for this exact key.
    Duplicate,
    Send(std::io::Error),
    /// The socket is closing.
    Closed,
}

impl std::fmt::Display for EchoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut => f.write_str("timed out"),
            Self::Duplicate => f.write_str("an identical echo is already in flight"),
            Self::Send(e) => write!(f, "send: {e}"),
            Self::Closed => f.write_str("socket closed"),
        }
    }
}

/// One ICMP socket and the task reading its replies.
struct EchoSocket {
    socket: Arc<UdpSocket>,
    family: Family,
    /// A Linux ping socket: the kernel owns the identifier, so replies are matched without it.
    kernel_ident: bool,
    waiters: Waiters,
    receiver: JoinHandle<()>,
}

impl EchoSocket {
    /// Open a ping socket, or a raw one when the kernel refuses that. Needs a Tokio runtime.
    fn open(family: Family) -> std::io::Result<Self> {
        let (domain, protocol) = match family {
            Family::V4 => (Domain::IPV4, Protocol::ICMPV4),
            Family::V6 => (Domain::IPV6, Protocol::ICMPV6),
        };
        let (socket, kind) = match Socket::new(domain, Type::DGRAM, Some(protocol)) {
            Ok(s) => (s, Type::DGRAM),
            Err(_) => (Socket::new(domain, Type::RAW, Some(protocol))?, Type::RAW),
        };
        socket.set_nonblocking(true)?;
        let socket = Arc::new(UdpSocket::from_std(std::net::UdpSocket::from(socket))?);
        let ping_socket =
            kind == Type::DGRAM && cfg!(any(target_os = "linux", target_os = "android"));
        // A raw IPv4 socket (and a DGRAM one off Linux) returns the IP header; IPv6 never does.
        let ip_header = family == Family::V4 && !ping_socket;
        let waiters: Waiters = Arc::default();
        let receiver = tokio::spawn(receive(
            Arc::clone(&socket),
            family,
            ping_socket,
            ip_header,
            Arc::clone(&waiters),
        ));
        Ok(Self {
            socket,
            family,
            kernel_ident: ping_socket,
            waiters,
            receiver,
        })
    }

    /// Send one echo and wait up to `timeout` for its reply; the round-trip time on success.
    async fn echo(
        &self,
        target: IpAddr,
        ident: u16,
        seq: u16,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<Duration, EchoError> {
        let key = (target, (!self.kernel_ident).then_some(ident), seq);
        let (tx, rx) = oneshot::channel();
        {
            let mut waiters = self.waiters.lock().unwrap_or_else(PoisonError::into_inner);
            if waiters.contains_key(&key) {
                return Err(EchoError::Duplicate);
            }
            waiters.insert(key, tx);
        }
        // Removes the waiter however this future ends — including being dropped mid-wait.
        let _guard = WaiterGuard {
            waiters: &self.waiters,
            key,
        };
        let packet = echo_request(self.family, ident, seq, payload);
        // Before the send, not after it: the waiter is already registered, so the receiver can
        // stamp a fast reply before `send_to` returns, and a later start would read it as 0 ms.
        let sent = Instant::now();
        self.socket
            .send_to(&packet, SocketAddr::new(target, 0))
            .await
            .map_err(EchoError::Send)?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(arrived)) => Ok(arrived.saturating_duration_since(sent)),
            Ok(Err(_)) => Err(EchoError::Closed),
            Err(_) => Err(EchoError::TimedOut),
        }
    }
}

impl Drop for EchoSocket {
    fn drop(&mut self) {
        self.receiver.abort();
    }
}

struct WaiterGuard<'a> {
    waiters: &'a Waiters,
    key: ReplyKey,
}

impl Drop for WaiterGuard<'_> {
    fn drop(&mut self) {
        self.waiters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.key);
    }
}

/// The receiver: read every datagram, keep echo replies, wake the probe waiting for each.
async fn receive(
    socket: Arc<UdpSocket>,
    family: Family,
    ping_socket: bool,
    ip_header: bool,
    waiters: Waiters,
) {
    let mut buf = vec![0u8; 2048];
    loop {
        let (len, from) = match socket.recv_from(&mut buf).await {
            Ok(got) => got,
            Err(e) => {
                tracing::debug!(error = %e, "icmp receive failed");
                // Never spin on an error that repeats at once.
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
        };
        let arrived = Instant::now();
        let Some((ident, seq)) = echo_reply(&buf[..len], family, ip_header) else {
            continue;
        };
        let key = (from.ip(), (!ping_socket).then_some(ident), seq);
        let waiter = waiters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&key);
        if let Some(tx) = waiter {
            // The probe may have given up already; nothing to do then.
            let _ = tx.send(arrived);
        }
    }
}

/// An echo request: type, code 0, checksum, identifier, sequence, payload. The IPv4 checksum is
/// ours to compute (a ping socket recomputes it after rewriting the identifier); the IPv6 one is
/// always the kernel's, because it covers a pseudo-header only the kernel knows (RFC 3542 3.1).
fn echo_request(family: Family, ident: u16, seq: u16, payload: &[u8]) -> Vec<u8> {
    let kind = match family {
        Family::V4 => ICMPV4_ECHO_REQUEST,
        Family::V6 => ICMPV6_ECHO_REQUEST,
    };
    let mut packet = Vec::with_capacity(8 + payload.len());
    packet.extend_from_slice(&[kind, 0, 0, 0]);
    packet.extend_from_slice(&ident.to_be_bytes());
    packet.extend_from_slice(&seq.to_be_bytes());
    packet.extend_from_slice(payload);
    if family == Family::V4 {
        let sum = internet_checksum(&packet);
        packet[2..4].copy_from_slice(&sum.to_be_bytes());
    }
    packet
}

/// The RFC 1071 one's-complement checksum.
fn internet_checksum(bytes: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for pair in bytes.chunks(2) {
        let word = match pair {
            [hi, lo] => u16::from_be_bytes([*hi, *lo]),
            [hi] => u16::from_be_bytes([*hi, 0]),
            _ => 0,
        };
        sum += u32::from(word);
    }
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// `(identifier, sequence)` of an echo reply; `None` for anything else — an echo request (a raw
/// socket sees our own), an ICMP error, a truncated packet.
fn echo_reply(packet: &[u8], family: Family, ip_header: bool) -> Option<(u16, u16)> {
    let icmp = if ip_header {
        let first = *packet.first()?;
        if first >> 4 != 4 {
            return None;
        }
        let header_len = usize::from(first & 0x0f) * 4;
        if header_len < 20 {
            return None;
        }
        packet.get(header_len..)?
    } else {
        packet
    };
    let want = match family {
        Family::V4 => ICMPV4_ECHO_REPLY,
        Family::V6 => ICMPV6_ECHO_REPLY,
    };
    match icmp {
        [kind, 0, _, _, i0, i1, s0, s1, ..] if *kind == want => Some((
            u16::from_be_bytes([*i0, *i1]),
            u16::from_be_bytes([*s0, *s1]),
        )),
        _ => None,
    }
}

/// The production transport: ICMP over [`EchoSocket`]s, and every other check through its module.
///
/// One socket per address family is opened up front (the v6 socket is optional — a
/// host without IPv6 still polls v4 targets). Construction needs ICMP-socket privilege.
pub struct LiveTransport {
    v4: EchoSocket,
    v6: Option<EchoSocket>,
    /// Rolling ICMP echo identifier so concurrent probes don't collide on a raw socket.
    ///
    /// ⚠️ **On a Linux ping socket this separates nothing** — the kernel rewrites the identifier,
    /// so a waiting reply is keyed by `(host, sequence)` there; see `next_sequences` for what
    /// actually keeps probes apart.
    ident: AtomicU16,
    /// The first sequence number of the next probe's block (`next_sequences`).
    seq: AtomicU16,
    /// Where Meraki collects physically go, when a lab build says so (ADR-166). `None` from
    /// [`Self::new`], and nothing but [`Self::with_meraki_wire_origin`] changes it.
    meraki_wire: Option<crate::MerakiWireOrigin>,
}

impl LiveTransport {
    /// Open the ICMP sockets. Fails if the v4 socket can't be opened (no ping-socket permission
    /// and no `CAP_NET_RAW`); a missing v6 socket is tolerated (v6 targets then error, v4 still
    /// works). Must be called inside a Tokio runtime: each socket starts its receiver task.
    pub fn new() -> Result<Self, TransportError> {
        let v4 = EchoSocket::open(Family::V4)
            .map_err(|e| TransportError::Io(format!("open ICMPv4 socket: {e}")))?;
        let v6 = match EchoSocket::open(Family::V6) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!(error = %e, "ICMPv6 socket unavailable; IPv6 targets will error");
                None
            }
        };
        // Seed the identifier so two pollers (or two restarts) don't start in lockstep.
        let seed: u16 = rand::random();
        Ok(Self {
            v4,
            v6,
            ident: AtomicU16::new(seed),
            seq: AtomicU16::new(0),
            meraki_wire: None,
        })
    }

    /// Send this transport's Meraki collects to `origin` instead of the host each URL names — a lab
    /// build's recorded Dashboard (ADR-166). `None` leaves them where they were.
    #[must_use]
    pub fn with_meraki_wire_origin(mut self, origin: Option<crate::MerakiWireOrigin>) -> Self {
        self.meraki_wire = origin;
        self
    }

    fn next_ident(&self) -> u16 {
        self.ident.fetch_add(1, Ordering::Relaxed)
    }
}

/// Reserve `count` consecutive sequence numbers for one probe and return the first.
///
/// 🚨 **This is what keeps two probes to the same address from receiving each other's replies.**
/// On a Linux ping socket a reply is matched on `(host, sequence)` alone — the identifier is
/// rewritten by the kernel and dropped from the key — so two probes that both send sequences
/// 0, 1, 2 to one host collide: the second registration fails as a duplicate and its echoes are
/// recorded as lost. Until ADR-110 Increment 8 the per-device single-flight kept same-target probes
/// from overlapping; ICMP now skips that queue, so a poll-now landing on a scheduled probe, or two
/// nodes sharing an address, overlap for real. Disjoint blocks make that harmless without putting
/// ICMP back behind the device's SNMP conversation.
///
/// The counter wraps; two blocks can only coincide after 65,536 sequences have been handed out in
/// between, and a probe lasts about a second.
fn next_sequences(counter: &AtomicU16, count: u8) -> u16 {
    counter.fetch_add(u16::from(count), Ordering::Relaxed)
}

#[async_trait]
impl Transport for LiveTransport {
    async fn probe_icmp(
        &self,
        target: IpAddr,
        count: u8,
        timeout: Duration,
    ) -> Result<IcmpProbe, TransportError> {
        let socket = match target {
            IpAddr::V4(_) => &self.v4,
            IpAddr::V6(_) => self
                .v6
                .as_ref()
                .ok_or_else(|| TransportError::Io("no IPv6 ICMP socket available".to_owned()))?,
        };

        // Echoes to one target go out ONE AT A TIME (parallelism comes from polling many targets at
        // once). N concurrent echo loops to the SAME target once returned 100% loss for every host —
        // that broke liveness fleet-wide and was reverted (64f42ef). Do not reintroduce them.
        // ⚠️ Two PROBES to one target can still overlap (a poll-now, a shared address), which is why
        // each takes its own block of sequence numbers — see `next_sequences`.
        let first_seq = next_sequences(&self.seq, count);
        let ident = self.next_ident();

        // S4 — bound the permit hold of a dead host. A plain serial loop makes an unresponsive host
        // cost count×timeout (default 3×1s); the poll holds the poller's global concurrency permit
        // for that whole span, so at 64 permits the fleet clears only ~21 dead-host polls/s — detection
        // throughput collapses exactly during a mass outage (ADR-023 #1). Cap the *whole* probe with an
        // overall deadline: once a host has stayed completely silent for one full timeout, more echoes
        // to the same black hole only prolong the permit hold without changing the verdict, so stop and
        // report it unreachable now (~1×timeout instead of count×timeout). A host that answers even one
        // echo is kept on the full sweep so partial packet loss is still measured. A single lost echo to
        // an up host cannot manufacture an outage — liveness commits only after DWELL_SAMPLES(=3)
        // consecutive unreachable polls (yagra-alert hysteresis). NOTE: this path needs an ICMP socket
        // and is not unit-testable; validate on a live socket after deploy (loopback demo node must stay
        // icmp_loss_pct=0). The pure pieces (`probe_exhausted`, `summarize`) are unit-tested.
        let started = Instant::now();
        let payload = [0u8; PAYLOAD_LEN];
        let mut rtts_ms = Vec::with_capacity(usize::from(count));
        let mut sent: u8 = 0;
        for i in 0..count {
            sent += 1;
            let seq = first_seq.wrapping_add(u16::from(i));
            match socket.echo(target, ident, seq, &payload, timeout).await {
                Ok(rtt) => rtts_ms.push(rtt.as_secs_f64() * 1000.0),
                Err(err) => {
                    tracing::debug!(%target, error = %err, "icmp echo did not complete");
                }
            }
            if probe_exhausted(rtts_ms.len(), started.elapsed(), timeout) {
                break;
            }
        }

        // Loss is over probes actually sent, not the nominal count: an early break means the remaining
        // echoes were never put on the wire, so counting them as "lost" would be a lie. When the host
        // answers, no break happens and sent == count, so healthy/lossy hosts are unaffected.
        Ok(summarize(sent, &rtts_ms))
    }

    async fn snmp_get(
        &self,
        target: IpAddr,
        community: &str,
        oids: &[String],
        timeout: Duration,
    ) -> Result<Vec<crate::SnmpSample>, TransportError> {
        crate::snmp::snmp_get_v2c(target, community, oids, timeout).await
    }

    async fn snmp_v3_get(
        &self,
        target: IpAddr,
        params: &crate::SnmpV3Params,
        oids: &[String],
        timeout: Duration,
    ) -> Result<Vec<crate::SnmpSample>, TransportError> {
        crate::snmp_v3::snmp_get_v3(target, params, oids, timeout).await
    }

    async fn snmp_v3_get_strings(
        &self,
        target: IpAddr,
        params: &crate::SnmpV3Params,
        oids: &[String],
        timeout: Duration,
    ) -> Result<Vec<crate::SnmpStringSample>, TransportError> {
        crate::snmp_v3::snmp_get_v3_strings(target, params, oids, timeout).await
    }

    async fn snmp_walk(
        &self,
        target: IpAddr,
        community: &str,
        column_oids: &[String],
        limits: crate::WalkLimits,
    ) -> Result<crate::TableWalk<crate::SnmpTableSample>, TransportError> {
        crate::snmp::snmp_walk_v2c(target, community, column_oids, limits).await
    }

    async fn snmp_walk_strings(
        &self,
        target: IpAddr,
        community: &str,
        column_oids: &[String],
        limits: crate::WalkLimits,
    ) -> Result<crate::TableWalk<crate::SnmpTableString>, TransportError> {
        crate::snmp::snmp_walk_strings_v2c(target, community, column_oids, limits).await
    }

    async fn snmp_v3_walk(
        &self,
        target: IpAddr,
        params: &crate::SnmpV3Params,
        column_oids: &[String],
        limits: crate::WalkLimits,
    ) -> Result<crate::TableWalk<crate::SnmpTableSample>, TransportError> {
        crate::snmp_v3::snmp_walk_v3(target, params, column_oids, limits).await
    }

    async fn snmp_v3_walk_strings(
        &self,
        target: IpAddr,
        params: &crate::SnmpV3Params,
        column_oids: &[String],
        limits: crate::WalkLimits,
    ) -> Result<crate::TableWalk<crate::SnmpTableString>, TransportError> {
        crate::snmp_v3::snmp_walk_strings_v3(target, params, column_oids, limits).await
    }

    async fn snmp_walk_instances(
        &self,
        target: IpAddr,
        community: &str,
        column_oids: &[String],
        timeout: Duration,
        max_rows: usize,
    ) -> Result<crate::InstanceWalk, TransportError> {
        crate::snmp::snmp_walk_instances_v2c(target, community, column_oids, timeout, max_rows)
            .await
    }

    async fn snmp_v3_walk_instances(
        &self,
        target: IpAddr,
        params: &crate::SnmpV3Params,
        column_oids: &[String],
        timeout: Duration,
        max_rows: usize,
    ) -> Result<crate::InstanceWalk, TransportError> {
        crate::snmp_v3::snmp_walk_instances_v3(target, params, column_oids, timeout, max_rows).await
    }

    async fn probe_http(
        &self,
        spec: &crate::HttpProbeSpec,
        timeout: Duration,
    ) -> Result<crate::HttpProbe, TransportError> {
        crate::http::probe_http(spec, timeout).await
    }

    async fn resolve_dns(
        &self,
        spec: &crate::DnsProbeSpec,
        timeout: Duration,
    ) -> Result<crate::DnsChain, TransportError> {
        crate::dns::resolve_dns(spec, timeout).await
    }

    async fn collect_meraki(
        &self,
        spec: &crate::MerakiCollectSpec,
        timeout: Duration,
    ) -> Result<crate::MerakiCollected, crate::MerakiFetchError> {
        crate::meraki::collect(spec, timeout, self.meraki_wire.as_ref()).await
    }
}

/// Whether to stop probing early (S4). True once the target has produced **no reply at all**
/// (`received == 0`) *and* we have already waited out the overall `deadline` — at that point the
/// host is confidently unreachable for this poll and further echoes to a silent target only hold
/// the poller's permit longer without changing the verdict. As soon as any echo replies
/// (`received > 0`) this stays false, so the remaining probes are sent and partial loss is measured.
/// Pure — unit-tested (the raw-socket loop that calls it is not).
#[must_use]
fn probe_exhausted(received: usize, elapsed: Duration, deadline: Duration) -> bool {
    received == 0 && elapsed >= deadline
}

/// Aggregate per-probe RTTs into a single [`IcmpProbe`]: reachable if any reply arrived, mean RTT
/// over replies, loss% over the number of probes actually **sent** (`count`). Callers pass the sent
/// count, which equals the requested count unless [`probe_exhausted`] cut the sweep short. Pure —
/// unit-tested.
#[must_use]
pub(crate) fn summarize(count: u8, rtts_ms: &[f64]) -> IcmpProbe {
    let sent = f64::from(count.max(1));
    let received = rtts_ms.len();
    let loss_pct = ((sent - received as f64) / sent) * 100.0;
    let rtt_ms = if received == 0 {
        None
    } else {
        Some(rtts_ms.iter().sum::<f64>() / received as f64)
    };
    IcmpProbe {
        reachable: received > 0,
        rtt_ms,
        loss_pct,
        sent: count.max(1),
        received: u8::try_from(received).unwrap_or(u8::MAX),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The packet `ping` sends — identifier, sequence, a 56-byte body — with a checksum a receiver
    /// accepts: one's-complement summing a packet that carries its own checksum gives zero.
    #[test]
    fn an_ipv4_echo_request_carries_a_valid_checksum() {
        let payload: Vec<u8> = (0u8..56).collect();
        let packet = echo_request(Family::V4, 0x1c46, 1, &payload);
        assert_eq!(&packet[..2], &[ICMPV4_ECHO_REQUEST, 0]);
        assert_eq!(&packet[4..8], &[0x1c, 0x46, 0x00, 0x01]);
        assert_eq!(packet.len(), 64);
        // A packet that carries its own checksum sums to zero.
        assert_eq!(internet_checksum(&packet), 0);
        // And the field is not left at zero, which a kernel would also accept on a ping socket.
        assert_ne!(&packet[2..4], &[0, 0]);
    }

    #[test]
    fn the_checksum_handles_an_odd_length_and_the_carry() {
        // RFC 1071's worked example: 0x0001 + 0xf203 + 0xf4f5 + 0xf6f7 = 0x2ddf0 -> 0xddf2 -> !
        assert_eq!(
            internet_checksum(&[0x00, 0x01, 0xf2, 0x03, 0xf4, 0xf5, 0xf6, 0xf7]),
            !0xddf2
        );
        assert_eq!(internet_checksum(&[0xff]), !0xff00);
    }

    #[test]
    fn an_ipv6_request_leaves_the_checksum_to_the_kernel() {
        let packet = echo_request(Family::V6, 7, 9, &[1, 2, 3]);
        assert_eq!(
            packet,
            vec![ICMPV6_ECHO_REQUEST, 0, 0, 0, 0, 7, 0, 9, 1, 2, 3]
        );
    }

    #[test]
    fn a_reply_reads_with_or_without_the_ipv4_header() {
        let icmp = [
            ICMPV4_ECHO_REPLY,
            0,
            0xab,
            0xcd,
            0x12,
            0x34,
            0x00,
            0x05,
            0xee,
        ];
        // A ping socket hands us the ICMP message alone.
        assert_eq!(echo_reply(&icmp, Family::V4, false), Some((0x1234, 5)));
        // A raw socket puts a 20-byte IPv4 header (IHL 5) in front.
        let mut raw = vec![0x45; 1];
        raw.extend_from_slice(&[0u8; 19]);
        raw.extend_from_slice(&icmp);
        assert_eq!(echo_reply(&raw, Family::V4, true), Some((0x1234, 5)));
        // With options (IHL 6) the ICMP message starts 4 bytes later.
        let mut opts = vec![0x46];
        opts.extend_from_slice(&[0u8; 23]);
        opts.extend_from_slice(&icmp);
        assert_eq!(echo_reply(&opts, Family::V4, true), Some((0x1234, 5)));
        let v6 = [ICMPV6_ECHO_REPLY, 0, 0, 0, 0, 1, 0, 2];
        assert_eq!(echo_reply(&v6, Family::V6, false), Some((1, 2)));
    }

    /// The one behaviour that changed with ADR-213: an ICMP error is not an answer, even from the
    /// target itself. A destination-unreachable quotes our request, identifier and sequence included,
    /// so reading only those two fields would count it as a reply.
    #[test]
    fn only_an_echo_reply_counts() {
        let unreachable = [3u8, 3, 0, 0, 0, 0, 0, 0, 0x45, 0, 0, 0];
        assert_eq!(echo_reply(&unreachable, Family::V4, false), None);
        let request = echo_request(Family::V4, 1, 1, &[]);
        assert_eq!(
            echo_reply(&request, Family::V4, false),
            None,
            "our own request on a raw socket"
        );
        let v6_request = echo_request(Family::V6, 1, 1, &[]);
        assert_eq!(echo_reply(&v6_request, Family::V6, false), None);
        // A v4 reply read as v6 (or the reverse) is not one.
        assert_eq!(
            echo_reply(&[ICMPV4_ECHO_REPLY, 0, 0, 0, 0, 1, 0, 2], Family::V6, false),
            None
        );
    }

    #[test]
    fn a_truncated_or_malformed_packet_is_not_a_reply() {
        assert_eq!(echo_reply(&[], Family::V4, false), None);
        assert_eq!(
            echo_reply(&[ICMPV4_ECHO_REPLY, 0, 0, 0, 0, 1, 0], Family::V4, false),
            None
        );
        assert_eq!(
            echo_reply(&[0x45, 0, 0], Family::V4, true),
            None,
            "header longer than packet"
        );
        assert_eq!(echo_reply(&[0x60; 40], Family::V4, true), None, "not IPv4");
        assert_eq!(
            echo_reply(&[0x44; 40], Family::V4, true),
            None,
            "IHL below 5"
        );
    }

    /// A probe that is dropped while it waits must not leave its key behind, or a later probe with
    /// the same key would be refused as a duplicate.
    #[tokio::test]
    async fn a_dropped_wait_releases_its_key() {
        let waiters: Waiters = Arc::default();
        let key = (IpAddr::from([10, 0, 0, 1]), None, 3);
        {
            let (tx, _rx) = oneshot::channel();
            waiters.lock().unwrap().insert(key, tx);
            let _guard = WaiterGuard {
                waiters: &waiters,
                key,
            };
        }
        assert!(waiters.lock().unwrap().is_empty());
    }

    #[test]
    fn overlapping_probes_to_one_target_never_share_a_sequence_number() {
        // The property the reply map needs on a Linux DGRAM socket, where it keys on
        // (host, sequence) alone: the blocks two probes send must not intersect.
        let counter = AtomicU16::new(0);
        let a = next_sequences(&counter, 3);
        let b = next_sequences(&counter, 3);
        let seqs = |first: u16| (0..3u16).map(move |i| first.wrapping_add(i));
        assert!(
            seqs(a).all(|s| !seqs(b).any(|t| t == s)),
            "{a} and {b} overlap"
        );
    }

    #[test]
    fn a_block_straddling_the_wrap_is_still_disjoint_from_the_next() {
        let counter = AtomicU16::new(u16::MAX - 1);
        let a = next_sequences(&counter, 3); // 65534, 65535, 0
        let b = next_sequences(&counter, 3); // 1, 2, 3
        assert_eq!((a, b), (u16::MAX - 1, 1));
    }

    #[test]
    fn all_replies_means_no_loss_and_mean_rtt() {
        let p = summarize(3, &[10.0, 20.0, 30.0]);
        assert!(p.reachable);
        assert_eq!(p.rtt_ms, Some(20.0));
        assert_eq!(p.loss_pct, 0.0);
    }

    #[test]
    fn partial_replies_report_partial_loss() {
        // 1 of 4 replies → 75% loss, mean over the one reply.
        let p = summarize(4, &[12.0]);
        assert!(p.reachable);
        assert_eq!(p.rtt_ms, Some(12.0));
        assert_eq!(p.loss_pct, 75.0);
    }

    #[test]
    fn no_replies_means_unreachable_full_loss_no_rtt() {
        let p = summarize(3, &[]);
        assert!(!p.reachable);
        assert_eq!(p.rtt_ms, None);
        assert_eq!(p.loss_pct, 100.0);
    }

    /// The raw counts agree with the percentage they sit beside, on every branch.
    ///
    /// 🚨 They exist to be compared with what the KERNEL counted (`nstat IcmpInEchoReps`), so a
    /// `received` that disagrees with `loss_pct` would send that comparison the wrong answer and
    /// look like the receive path dropping replies it never dropped (ADR-109). Checking every
    /// branch is the point: a check that only looked at the full-success case would have passed
    /// with `received` hardcoded to `sent`.
    #[test]
    fn the_raw_counts_agree_with_the_loss_percentage() {
        for (count, rtts) in [
            (3u8, vec![10.0, 20.0, 30.0]),
            (4, vec![12.0]),
            (3, vec![]),
            (1, vec![7.0]),
        ] {
            let p = summarize(count, &rtts);
            assert_eq!(p.sent, count.max(1), "sent for count={count}");
            assert_eq!(
                p.received,
                u8::try_from(rtts.len()).unwrap(),
                "received for count={count}"
            );
            let implied = (f64::from(p.sent) - f64::from(p.received)) / f64::from(p.sent) * 100.0;
            assert!(
                (implied - p.loss_pct).abs() < 1e-9,
                "count={count}: counts imply {implied}% loss but loss_pct says {}",
                p.loss_pct
            );
        }
    }

    #[test]
    fn zero_count_does_not_divide_by_zero() {
        // Defensive: count is clamped to at least 1 for the loss denominator.
        let p = summarize(0, &[]);
        assert_eq!(p.loss_pct, 100.0);
        assert!(!p.reachable);
    }

    #[test]
    fn silent_host_is_exhausted_once_the_deadline_passes() {
        // No reply and the full deadline has elapsed → stop early (dead host frees the permit).
        assert!(probe_exhausted(
            0,
            Duration::from_millis(1000),
            Duration::from_millis(1000)
        ));
        assert!(probe_exhausted(
            0,
            Duration::from_millis(1500),
            Duration::from_millis(1000)
        ));
    }

    #[test]
    fn silent_host_within_the_deadline_keeps_probing() {
        // No reply yet but still inside the budget → keep waiting, don't declare down prematurely.
        assert!(!probe_exhausted(
            0,
            Duration::from_millis(999),
            Duration::from_millis(1000)
        ));
    }

    #[test]
    fn any_reply_never_exhausts_so_partial_loss_is_measured() {
        // Once a single echo has answered, the host is up — keep sending the rest to measure loss,
        // even long past the deadline. This is what preserves partial-packet-loss monitoring.
        assert!(!probe_exhausted(
            1,
            Duration::from_millis(5000),
            Duration::from_millis(1000)
        ));
        assert!(!probe_exhausted(
            2,
            Duration::from_millis(5000),
            Duration::from_millis(1000)
        ));
    }

    #[test]
    fn early_break_reports_full_loss_over_probes_actually_sent() {
        // A dead host that breaks after one silent probe: sent=1, received=0 → 100% loss, not the
        // "1 of 3" a nominal-count denominator would wrongly report.
        let p = summarize(1, &[]);
        assert!(!p.reachable);
        assert_eq!(p.loss_pct, 100.0);
        assert_eq!(p.rtt_ms, None);
    }
}
