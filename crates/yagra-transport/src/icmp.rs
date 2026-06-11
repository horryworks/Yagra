//! Real ICMP transport over raw sockets (`surge-ping`).
//!
//! This is the production [`Transport`] for liveness/RTT. It needs `CAP_NET_RAW` at
//! runtime (granted to the poller container only — see `.claude/rules/security.md`),
//! so it cannot be exercised in a normal unit test; the privilege-free [`FakeTransport`]
//! covers poller logic. The pure aggregation step ([`summarize`]) *is* unit-tested.
//!
//! Per ADR-012 this layer collects **raw** observations only (reachability, per-probe
//! RTT, loss %) — it derives no rates and holds no previous-value state.

use crate::{IcmpProbe, Transport, TransportError};
use async_trait::async_trait;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;
use surge_ping::{Client, Config, PingIdentifier, PingSequence, ICMP};

/// ICMP echo payload size in bytes (a conventional 56-byte body → 64-byte packet).
const PAYLOAD_LEN: usize = 56;

/// A real ICMP transport backed by `surge-ping` raw sockets.
///
/// One socket per address family is opened up front (the v6 socket is optional — a
/// host without IPv6 still polls v4 targets). Construction needs raw-socket privilege.
pub struct SurgePingTransport {
    v4: Client,
    v6: Option<Client>,
    /// Rolling ICMP echo identifier so concurrent probes don't collide.
    ident: AtomicU16,
}

impl SurgePingTransport {
    /// Open the ICMP sockets. Fails (needs `CAP_NET_RAW`) if the v4 socket can't bind;
    /// a missing v6 socket is tolerated (v6 targets then error, v4 still works).
    pub fn new() -> Result<Self, TransportError> {
        let v4 = Client::new(&Config::default())
            .map_err(|e| TransportError::Io(format!("open ICMPv4 socket: {e}")))?;
        let v6 = match Client::new(&Config::builder().kind(ICMP::V6).build()) {
            Ok(c) => Some(c),
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
        })
    }

    fn next_ident(&self) -> PingIdentifier {
        PingIdentifier(self.ident.fetch_add(1, Ordering::Relaxed))
    }
}

#[async_trait]
impl Transport for SurgePingTransport {
    async fn probe_icmp(
        &self,
        target: IpAddr,
        count: u8,
        timeout: Duration,
    ) -> Result<IcmpProbe, TransportError> {
        let client = match target {
            IpAddr::V4(_) => &self.v4,
            IpAddr::V6(_) => self
                .v6
                .as_ref()
                .ok_or_else(|| TransportError::Io("no IPv6 ICMP socket available".to_owned()))?,
        };

        let mut pinger = client.pinger(target, self.next_ident()).await;
        pinger.timeout(timeout);

        let payload = [0u8; PAYLOAD_LEN];
        let mut rtts_ms = Vec::with_capacity(usize::from(count));
        for seq in 0..count {
            match pinger.ping(PingSequence(u16::from(seq)), &payload).await {
                Ok((_packet, rtt)) => rtts_ms.push(rtt.as_secs_f64() * 1000.0),
                Err(err) => {
                    tracing::debug!(%target, error = %err, "icmp echo did not complete");
                }
            }
        }

        Ok(summarize(count, &rtts_ms))
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
        _target: IpAddr,
        _params: &crate::SnmpV3Params,
        _oids: &[String],
        _timeout: Duration,
    ) -> Result<Vec<crate::SnmpSample>, TransportError> {
        // USM (auth/priv) needs a v3 client; `csnmp` is v2c-only. Per ADR-021 this is the
        // net-snmp FFI fallback point — pending that decision, v3 polling is unimplemented.
        Err(TransportError::Unimplemented(
            "SNMP v3 (USM) — pending net-snmp FFI decision (ADR-021)",
        ))
    }

    async fn snmp_walk(
        &self,
        target: IpAddr,
        community: &str,
        column_oids: &[String],
        timeout: Duration,
    ) -> Result<Vec<crate::SnmpTableSample>, TransportError> {
        crate::snmp::snmp_walk_v2c(target, community, column_oids, timeout).await
    }

    async fn snmp_walk_strings(
        &self,
        target: IpAddr,
        community: &str,
        column_oids: &[String],
        timeout: Duration,
    ) -> Result<Vec<crate::SnmpTableString>, TransportError> {
        crate::snmp::snmp_walk_strings_v2c(target, community, column_oids, timeout).await
    }
}

/// Aggregate per-probe RTTs into a single [`IcmpProbe`]: reachable if any reply
/// arrived, mean RTT over replies, loss% over the `count` sent. Pure — unit-tested.
#[must_use]
pub fn summarize(count: u8, rtts_ms: &[f64]) -> IcmpProbe {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn zero_count_does_not_divide_by_zero() {
        // Defensive: count is clamped to at least 1 for the loss denominator.
        let p = summarize(0, &[]);
        assert_eq!(p.loss_pct, 100.0);
        assert!(!p.reachable);
    }
}
