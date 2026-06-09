//! Yagra-transport (`sekisho`) — device I/O abstraction over ICMP / SNMP / HTTP.
//!
//! All device I/O goes through the [`Transport`] trait so pollers and discovery never
//! speak a raw protocol directly (ADR / coding-conventions). Protocol differences, raw
//! counter collection, and rate-limiting/backpressure live behind it. Crucially, rate
//! and utilization are **not** computed here — pollers store raw counters and rates are
//! derived at query/eval time (ADR-012), so this layer is stateless per device.
//!
//! Phase 1 covers ICMP; SNMP/HTTP land behind the same trait. The real ICMP transport
//! (raw sockets via `surge-ping`, needs `CAP_NET_RAW`) is a later drop-in; [`FakeTransport`]
//! lets the poller and the walking skeleton be exercised without privileges or a device.

use async_trait::async_trait;
use std::net::IpAddr;
use std::time::Duration;
use thiserror::Error;

/// Outcome of an ICMP probe. Raw observations only — no derived rates.
#[derive(Debug, Clone, PartialEq)]
pub struct IcmpProbe {
    /// Whether the target responded at least once.
    pub reachable: bool,
    /// Mean round-trip time in milliseconds, if any reply was received.
    pub rtt_ms: Option<f64>,
    /// Packet loss percentage over the probe (0.0–100.0).
    pub loss_pct: f64,
}

/// Errors performing device I/O.
#[derive(Debug, Error)]
pub enum TransportError {
    /// Underlying socket/IO failure.
    #[error("transport io error: {0}")]
    Io(String),
    /// The transport for this protocol is not yet implemented.
    #[error("transport not implemented: {0}")]
    Unimplemented(&'static str),
}

/// Abstraction over device access. Implementations: a real ICMP/SNMP/HTTP transport
/// (production) and [`FakeTransport`] (tests / skeleton).
#[async_trait]
pub trait Transport: Send + Sync {
    /// Send `count` ICMP echoes to `target`, each bounded by `timeout`, and report a
    /// single aggregated [`IcmpProbe`].
    async fn probe_icmp(
        &self,
        target: IpAddr,
        count: u8,
        timeout: Duration,
    ) -> Result<IcmpProbe, TransportError>;
}

/// A canned [`Transport`] for tests and the single-process walking skeleton.
///
/// Returns a fixed probe regardless of target, so poller logic can be exercised with no
/// raw-socket privilege and no real device.
#[derive(Debug, Clone)]
pub struct FakeTransport {
    /// The probe every call returns.
    pub probe: IcmpProbe,
}

impl FakeTransport {
    /// A fake that always reports the target reachable with the given RTT.
    #[must_use]
    pub fn reachable(rtt_ms: f64) -> Self {
        Self {
            probe: IcmpProbe {
                reachable: true,
                rtt_ms: Some(rtt_ms),
                loss_pct: 0.0,
            },
        }
    }

    /// A fake that always reports the target unreachable (100% loss).
    #[must_use]
    pub fn unreachable() -> Self {
        Self {
            probe: IcmpProbe {
                reachable: false,
                rtt_ms: None,
                loss_pct: 100.0,
            },
        }
    }
}

#[async_trait]
impl Transport for FakeTransport {
    async fn probe_icmp(
        &self,
        _target: IpAddr,
        _count: u8,
        _timeout: Duration,
    ) -> Result<IcmpProbe, TransportError> {
        Ok(self.probe.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[tokio::test]
    async fn fake_reachable_reports_rtt_and_no_loss() {
        let t = FakeTransport::reachable(12.5);
        let p = t
            .probe_icmp(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                3,
                Duration::from_millis(1000),
            )
            .await
            .unwrap();
        assert!(p.reachable);
        assert_eq!(p.rtt_ms, Some(12.5));
        assert_eq!(p.loss_pct, 0.0);
    }

    #[tokio::test]
    async fn fake_unreachable_reports_full_loss() {
        let t = FakeTransport::unreachable();
        let p = t
            .probe_icmp(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                3,
                Duration::from_millis(1000),
            )
            .await
            .unwrap();
        assert!(!p.reachable);
        assert_eq!(p.rtt_ms, None);
        assert_eq!(p.loss_pct, 100.0);
    }
}
