// SPDX-License-Identifier: AGPL-3.0-only
//! URL / HTTP(S) endpoint monitoring: configuration and metric vocabulary.
//!
//! A URL monitor is modelled as a dedicated *node kind* (profile category
//! [`crate::ProfileCategory::UrlCheck`]) carrying a single [`UrlCheckConfig`] (1:1 with the
//! node). It reuses the whole monitoring spine — thresholds (profile→group→node), maintenance
//! windows, dependency suppression, dashboards — so nothing here is bespoke beyond the probe
//! shape. Metrics follow the thin-label model (ADR-011): node-level gauges, no new TSDB labels.
//!
//! Secrets never live in this type: an optional auth credential is referenced by id and the
//! decrypted secret is inlined by core over the bus at dispatch time (ADR-018/020).

use crate::ids::CredentialId;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv6Addr};

/// Stable TSDB metric: `1` = the endpoint answered **and** its status matched expectation,
/// `0` = unreachable, connection failed, or the status did not match. One metric expresses
/// "down or wrong status" so a single threshold (`http_up` below 0.5 — only the 0 state, since
/// the engine's below-comparison is inclusive) covers both.
pub const METRIC_HTTP_UP: &str = "http_up";
/// Stable TSDB metric: the HTTP status code observed (diagnostic/display only — no threshold).
pub const METRIC_HTTP_STATUS_CODE: &str = "http_status_code";
/// Stable TSDB metric: days until the TLS server certificate's `notAfter` (HTTPS only).
pub const METRIC_SSL_CERT_DAYS_TO_EXPIRY: &str = "ssl_cert_days_to_expiry";

/// HTTP request method for a URL check. Stored as an UPPERCASE token (the `method` column).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, utoipa::ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    /// `GET` (the default — fetches the body so a content match can run later).
    #[default]
    Get,
    /// `HEAD` (status/headers only; lighter for pure liveness).
    Head,
    /// `POST` (e.g. a health endpoint that expects a POST).
    Post,
}

impl HttpMethod {
    /// The stable UPPERCASE token stored in the DB / sent over the bus.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Head => "HEAD",
            HttpMethod::Post => "POST",
        }
    }

    /// Parse a stored/operator token back into a method (case-insensitive).
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "GET" => Some(HttpMethod::Get),
            "HEAD" => Some(HttpMethod::Head),
            "POST" => Some(HttpMethod::Post),
            _ => None,
        }
    }
}

/// Which HTTP status codes count as "up". Serialized as a tagged object (the `expected_status`
/// JSONB column), e.g. `{"kind":"two_xx"}` / `{"kind":"exact","codes":[200,204]}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExpectedStatus {
    /// Any 2xx (the default).
    #[default]
    TwoXx,
    /// An explicit set of acceptable codes.
    Exact { codes: Vec<u16> },
    /// An inclusive range `[lo, hi]`.
    Range { lo: u16, hi: u16 },
}

impl ExpectedStatus {
    /// Whether `code` is considered healthy under this expectation.
    #[must_use]
    pub fn matches(&self, code: u16) -> bool {
        match self {
            ExpectedStatus::TwoXx => (200..=299).contains(&code),
            ExpectedStatus::Exact { codes } => codes.contains(&code),
            ExpectedStatus::Range { lo, hi } => (*lo..=*hi).contains(&code),
        }
    }
}

const fn default_true() -> bool {
    true
}
const fn default_timeout_ms() -> u32 {
    5000
}

/// A node's URL-monitoring configuration (1:1 with the node). No secrets: the optional auth
/// `credential` is a reference; core resolves/inlines the decrypted value (ADR-018/020).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct UrlCheckConfig {
    /// Full URL to probe, e.g. `https://api.example.com/health`.
    pub url: String,
    /// Request method (default `GET`).
    #[serde(default)]
    pub method: HttpMethod,
    /// Which status codes count as healthy (default: any 2xx).
    #[serde(default)]
    pub expected_status: ExpectedStatus,
    /// Verify the TLS certificate chain (default `true`; never silently disabled — security.md).
    #[serde(default = "default_true")]
    pub verify_tls: bool,
    /// Follow 3xx redirects (default `true`).
    #[serde(default = "default_true")]
    pub follow_redirects: bool,
    /// Per-request timeout, in milliseconds (default 5000).
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u32,
    /// Optional auth credential (Basic/Bearer/Header) — reserved; unused in the MVP probe.
    #[serde(default)]
    pub credential: Option<CredentialId>,
}

impl UrlCheckConfig {
    /// A new URL check with MVP defaults (GET, any-2xx, TLS verified, redirects followed).
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            method: HttpMethod::default(),
            expected_status: ExpectedStatus::default(),
            verify_tls: true,
            follow_redirects: true,
            timeout_ms: default_timeout_ms(),
            credential: None,
        }
    }
}

/// Whether `ip` must be refused as a URL-monitor target (SSRF defense, security.md / ADR §229).
///
/// An NMS legitimately monitors **internal** hosts, so RFC1918 / ULA private ranges are *allowed*.
/// What we block is the SSRF-escalation surface that is never a real monitoring target:
/// loopback, the unspecified address, multicast/broadcast, and link-local — the last of which
/// includes the cloud instance-metadata IP (169.254.169.254 / fe80::). Enforced at the API edge
/// and again in the transport (defense in depth).
#[must_use]
pub fn is_ssrf_blocked(ip: IpAddr) -> bool {
    // Normalize IPv4-mapped IPv6 (`::ffff:a.b.c.d`) to its V4 form first. Otherwise a mapped
    // literal like `::ffff:169.254.169.254` / `::ffff:127.0.0.1` slips past the V6 branch (which
    // never recognizes it as link-local / loopback) and reaches the metadata/loopback address.
    let ip = match ip {
        IpAddr::V6(a) => a.to_ipv4_mapped().map_or(IpAddr::V6(a), IpAddr::V4),
        v4 => v4,
    };
    match ip {
        IpAddr::V4(a) => {
            a.is_loopback()
                || a.is_link_local()
                || a.is_unspecified()
                || a.is_multicast()
                || a.is_broadcast()
        }
        IpAddr::V6(a) => {
            a.is_loopback() || a.is_unspecified() || a.is_multicast() || is_v6_link_local(a)
        }
    }
}

/// Unicast link-local `fe80::/10` (not exposed as a stable std method on all toolchains).
fn is_v6_link_local(a: Ipv6Addr) -> bool {
    (a.segments()[0] & 0xffc0) == 0xfe80
}

/// The IP a URL's host component names, or `None` if it is a hostname.
///
/// The whole job is the brackets. `Url::host_str` returns an IPv6 literal **with** its enclosing
/// `[…]` (`"[::1]"`), which `IpAddr::from_str` rejects — so `host.parse().ok()` reads as "this is a
/// hostname" for every IPv6 address, and any check gated on it is skipped. That is not
/// hypothetical: it is what let `http://[::1]/` past the URL-monitor edge validator while the
/// identical check in the transport (which stripped the brackets) refused it. Four sites needed
/// this and three had their own copy, so it lives here, next to the check it feeds.
#[must_use]
pub fn host_ip(host: &str) -> Option<IpAddr> {
    host.strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
        .parse::<IpAddr>()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn a_bracketed_ipv6_host_is_still_recognized_as_a_literal() {
        // The whole reason this function exists. `Url::host_str` hands back "[::1]", and every
        // caller here feeds the result to `is_ssrf_blocked` — so reading a v6 literal as a
        // hostname does not fail closed, it skips the SSRF check entirely.
        for (host, want) in [
            ("[::1]", Some(IpAddr::V6(Ipv6Addr::LOCALHOST))),
            ("[fe80::1]", Some("fe80::1".parse().unwrap())),
            ("::1", Some(IpAddr::V6(Ipv6Addr::LOCALHOST))), // unbracketed still works
            ("127.0.0.1", Some(IpAddr::V4(Ipv4Addr::LOCALHOST))),
            ("example.com", None),
            ("", None),
            ("[not-an-ip]", None),
        ] {
            assert_eq!(host_ip(host), want, "{host}");
        }

        // The pairing that matters: every blocked literal must survive the round trip bracketed.
        for blocked in ["[::1]", "[::]", "[fe80::1]", "[::ffff:169.254.169.254]"] {
            let ip = host_ip(blocked).expect(blocked);
            assert!(is_ssrf_blocked(ip), "{blocked} must be refused");
        }
    }

    #[test]
    fn http_method_round_trips_token() {
        for m in [HttpMethod::Get, HttpMethod::Head, HttpMethod::Post] {
            assert_eq!(HttpMethod::from_token(m.as_str()), Some(m));
        }
        assert_eq!(HttpMethod::from_token("get"), Some(HttpMethod::Get));
        assert_eq!(HttpMethod::from_token("DELETE"), None);
    }

    #[test]
    fn expected_status_two_xx_matches_only_2xx() {
        let e = ExpectedStatus::TwoXx;
        assert!(e.matches(200));
        assert!(e.matches(204));
        assert!(!e.matches(301));
        assert!(!e.matches(404));
        assert!(!e.matches(500));
    }

    #[test]
    fn expected_status_exact_and_range() {
        assert!(ExpectedStatus::Exact {
            codes: vec![200, 301]
        }
        .matches(301));
        assert!(!ExpectedStatus::Exact {
            codes: vec![200, 301]
        }
        .matches(302));
        assert!(ExpectedStatus::Range { lo: 200, hi: 399 }.matches(302));
        assert!(!ExpectedStatus::Range { lo: 200, hi: 399 }.matches(400));
    }

    #[test]
    fn expected_status_serializes_tagged() {
        let json = serde_json::to_string(&ExpectedStatus::TwoXx).unwrap();
        assert_eq!(json, r#"{"kind":"two_xx"}"#);
    }

    #[test]
    fn url_check_config_defaults() {
        let json = r#"{"url":"https://example.com/health"}"#;
        let cfg: UrlCheckConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.method, HttpMethod::Get);
        assert_eq!(cfg.expected_status, ExpectedStatus::TwoXx);
        assert!(cfg.verify_tls);
        assert!(cfg.follow_redirects);
        assert_eq!(cfg.timeout_ms, 5000);
        assert_eq!(cfg.credential, None);
    }

    #[test]
    fn ssrf_blocks_loopback_linklocal_metadata_but_allows_private() {
        // Blocked: the SSRF-escalation surface.
        assert!(is_ssrf_blocked(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(
            169, 254, 169, 254
        )))); // cloud metadata
        assert!(is_ssrf_blocked(IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
        assert!(is_ssrf_blocked(IpAddr::V4(Ipv4Addr::BROADCAST)));
        assert!(is_ssrf_blocked(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(is_ssrf_blocked("fe80::1".parse().unwrap()));
        // Allowed: legitimate internal monitoring targets.
        assert!(!is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))));
        assert!(!is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2))));
        assert!(!is_ssrf_blocked(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!is_ssrf_blocked("2001:db8::1".parse().unwrap()));
    }

    #[test]
    fn ssrf_normalizes_ipv4_mapped_ipv6() {
        // IPv4-mapped IPv6 must be judged by the V4 rules, not slip past the V6 branch.
        assert!(is_ssrf_blocked("::ffff:169.254.169.254".parse().unwrap())); // mapped metadata
        assert!(is_ssrf_blocked("::ffff:127.0.0.1".parse().unwrap())); // mapped loopback
        assert!(is_ssrf_blocked("::ffff:0.0.0.0".parse().unwrap())); // mapped unspecified
                                                                     // A mapped *private* address stays allowed (still a legitimate internal target).
        assert!(!is_ssrf_blocked("::ffff:10.0.0.1".parse().unwrap()));
        assert!(!is_ssrf_blocked("::ffff:192.168.1.2".parse().unwrap()));
    }
}
