//! Real HTTP/HTTPS endpoint transport (`reqwest` over rustls).
//!
//! The URL-monitoring probe: one request to a URL, reporting reachability, the HTTP status
//! code, response time, and — for HTTPS — the server certificate's days-to-expiry (read from
//! the TLS peer certificate via `reqwest`'s `tls_info`, parsed with `x509-parser`).
//!
//! Per ADR-012 this layer reports **raw** observations only; the poller maps them to samples
//! and derives `http_up` (reachable && status matches). A network failure (timeout/connect/DNS)
//! is reported as `reachable = false` (a real outage), not an error — mirroring the ICMP arm.
//! Errors are reserved for un-runnable configs (bad URL/scheme, SSRF-blocked target).

use crate::{HttpProbe, HttpProbeSpec, TransportError};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use yagra_common::{is_ssrf_blocked, HttpMethod};

/// Run one HTTP(S) probe. See the module docs for the reachable-vs-error contract.
pub(crate) async fn probe_http(
    spec: &HttpProbeSpec,
    timeout: Duration,
) -> Result<HttpProbe, TransportError> {
    let url = reqwest::Url::parse(&spec.url)
        .map_err(|e| TransportError::Io(format!("invalid url: {e}")))?;
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(TransportError::Io(format!(
                "unsupported url scheme: {other}"
            )))
        }
    }

    // SSRF defense-in-depth (the API edge validates too): refuse loopback / link-local
    // (incl. cloud metadata) / multicast / unspecified targets. Private ranges are allowed —
    // an NMS legitimately monitors internal endpoints.
    if let Some(host) = url.host_str() {
        match parse_host_ip(host) {
            Some(ip) => {
                if is_ssrf_blocked(ip) {
                    return Err(TransportError::Io("blocked target address".to_owned()));
                }
            }
            None => {
                let port = url
                    .port_or_known_default()
                    .unwrap_or(if url.scheme() == "https" { 443 } else { 80 });
                // A resolvable name is refused if *any* of its answers is blocked (an external
                // monitoring target never legitimately resolves to loopback/link-local/metadata;
                // a mixed public+blocked answer set is exactly the DNS-rebinding shape). This is an
                // early, explicit rejection — the authoritative, TOCTOU-free enforcement is the
                // `SsrfResolver` wired into the client below, which reqwest also consults for every
                // redirect hop. A DNS failure falls through and is reported as unreachable below.
                if let Ok(addrs) = tokio::net::lookup_host((host, port)).await {
                    if addrs.into_iter().any(|a| is_ssrf_blocked(a.ip())) {
                        return Err(TransportError::Io("blocked target address".to_owned()));
                    }
                }
            }
        }
    }

    let redirect = if spec.follow_redirects {
        // Re-validate every redirect hop, not just the initial target. Two complementary guards
        // cover the whole SSRF surface across hops: this policy refuses an IP-*literal* hop host
        // (`302 Location: http://169.254.169.254/…`) or a bad scheme before the request is made,
        // and the `SsrfResolver` below refuses a *hostname* hop that resolves to a blocked address
        // (the DNS-rebinding / metadata-behind-a-name case the literal check can't see). Since
        // reqwest dials exactly what the resolver returns, there is no resolve-then-connect TOCTOU.
        reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                return attempt.stop();
            }
            if redirect_hop_blocked(attempt.url()) {
                return attempt.error("redirect target address is not allowed (SSRF)");
            }
            attempt.follow()
        })
    } else {
        reqwest::redirect::Policy::none()
    };
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(redirect)
        // Pin DNS to an SSRF-filtered resolver: reqwest resolves — and therefore dials — only
        // non-blocked addresses, for the initial target and every hostname redirect hop. This is
        // the authoritative, TOCTOU-free enforcement (the resolved set is the dialed set).
        .dns_resolver(Arc::new(SsrfResolver))
        // verify_tls is off only by explicit operator opt-in (default on — security.md).
        .danger_accept_invalid_certs(!spec.verify_tls)
        .tls_info(true)
        .user_agent("Yagra-poller")
        .build()
        .map_err(|e| TransportError::Io(format!("http client build failed: {e}")))?;

    let method = match spec.method {
        HttpMethod::Get => reqwest::Method::GET,
        HttpMethod::Head => reqwest::Method::HEAD,
        HttpMethod::Post => reqwest::Method::POST,
    };

    let started = Instant::now();
    match client.request(method, url).send().await {
        Ok(resp) => {
            let response_time_ms = started.elapsed().as_secs_f64() * 1000.0;
            let status_code = Some(resp.status().as_u16());
            let cert_days_to_expiry = resp
                .extensions()
                .get::<reqwest::tls::TlsInfo>()
                .and_then(reqwest::tls::TlsInfo::peer_certificate)
                .and_then(cert_days_to_expiry);
            Ok(HttpProbe {
                reachable: true,
                status_code,
                response_time_ms,
                cert_days_to_expiry,
            })
        }
        Err(e) => {
            // Timeout / connect / DNS failure → a real outage, reported as unreachable so the
            // poller emits http_up = 0 (not an error that would drop the sample).
            tracing::debug!(error = %e, url = %spec.url, "http probe did not complete");
            Ok(HttpProbe {
                reachable: false,
                status_code: None,
                response_time_ms: started.elapsed().as_secs_f64() * 1000.0,
                cert_days_to_expiry: None,
            })
        }
    }
}

/// Parse a URL host string as an IP literal, tolerating the bracketed IPv6 form (`[::1]`).
/// Returns `None` for a domain name (resolution is checked separately).
fn parse_host_ip(host: &str) -> Option<IpAddr> {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    bare.parse::<IpAddr>().ok()
}

/// Whether a redirect hop's URL must be refused (per-hop SSRF defense). Blocks non-http(s)
/// schemes and IP-literal hosts that are SSRF-blocked; a hostname hop is allowed (see the
/// redirect-policy comment in `probe_http`).
fn redirect_hop_blocked(url: &reqwest::Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return true;
    }
    url.host_str()
        .and_then(parse_host_ip)
        .is_some_and(is_ssrf_blocked)
}

/// Drop the SSRF-blocked addresses from a resolved set, returning the ones reqwest may dial. Pure
/// (no I/O) so the allow/deny decision is unit-testable without live DNS.
fn retain_allowed_addrs(addrs: impl IntoIterator<Item = SocketAddr>) -> Vec<SocketAddr> {
    addrs
        .into_iter()
        .filter(|a| !is_ssrf_blocked(a.ip()))
        .collect()
}

/// A `reqwest` DNS resolver that removes SSRF-blocked addresses from every lookup. Because reqwest
/// dials exactly the addresses a resolver hands back — for the initial target *and* every hostname
/// redirect hop — this is the authoritative SSRF guard for name-based targets, closing the
/// resolve-then-connect (DNS-rebinding) TOCTOU window that a separate pre-check leaves open.
/// IP-literal hosts skip DNS and are covered by the pre-request checks (`is_ssrf_blocked` on the
/// initial target, `redirect_hop_blocked` on each hop).
struct SsrfResolver;

impl reqwest::dns::Resolve for SsrfResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            // Port 0: reqwest overrides it with the request's port (matches the default resolver).
            let resolved = tokio::net::lookup_host((name.as_str(), 0)).await?;
            let allowed = retain_allowed_addrs(resolved);
            if allowed.is_empty() {
                // The name either doesn't resolve or every answer is SSRF-blocked. Failing the
                // lookup makes reqwest report a connect error, which the poller records as an
                // unreachable target — it never follows into a blocked address.
                return Err("no SSRF-allowed address for host".into());
            }
            let addrs: reqwest::dns::Addrs = Box::new(allowed.into_iter());
            Ok(addrs)
        })
    }
}

/// Days until a DER-encoded X.509 certificate's `notAfter` (may be negative if already expired).
/// `None` if the cert can't be parsed.
fn cert_days_to_expiry(der: &[u8]) -> Option<f64> {
    let (_, cert) = x509_parser::parse_x509_certificate(der).ok()?;
    let not_after = cert.validity().not_after.timestamp();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    Some((not_after - now) as f64 / 86_400.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_non_http_scheme() {
        let spec = HttpProbeSpec {
            url: "ftp://example.com/x".to_owned(),
            method: HttpMethod::Get,
            verify_tls: true,
            follow_redirects: true,
        };
        let err = probe_http(&spec, Duration::from_millis(500)).await;
        assert!(matches!(err, Err(TransportError::Io(_))));
    }

    #[tokio::test]
    async fn rejects_blocked_ip_literal_target() {
        // Loopback IP literal → SSRF-blocked before any request is made.
        let spec = HttpProbeSpec {
            url: "http://127.0.0.1:9/".to_owned(),
            method: HttpMethod::Get,
            verify_tls: true,
            follow_redirects: true,
        };
        let err = probe_http(&spec, Duration::from_millis(500)).await;
        assert!(matches!(err, Err(TransportError::Io(_))));
    }

    #[tokio::test]
    async fn rejects_cloud_metadata_target() {
        let spec = HttpProbeSpec {
            url: "http://169.254.169.254/latest/meta-data/".to_owned(),
            method: HttpMethod::Get,
            verify_tls: true,
            follow_redirects: true,
        };
        let err = probe_http(&spec, Duration::from_millis(500)).await;
        assert!(matches!(err, Err(TransportError::Io(_))));
    }

    #[test]
    fn unparseable_cert_yields_none() {
        assert_eq!(cert_days_to_expiry(b"not a cert"), None);
    }

    #[test]
    fn redirect_hop_blocks_metadata_loopback_and_bad_scheme() {
        let blocked = |u: &str| redirect_hop_blocked(&reqwest::Url::parse(u).unwrap());
        // The exact attack: a redirect to the cloud-metadata / loopback IP literal.
        assert!(blocked("http://169.254.169.254/latest/meta-data/"));
        assert!(blocked("http://127.0.0.1/"));
        assert!(blocked("http://[::1]/")); // bracketed IPv6 loopback
        assert!(blocked("http://[::ffff:169.254.169.254]/")); // mapped metadata
        assert!(blocked("ftp://example.com/")); // non-http(s) scheme
                                                // Allowed hops: a hostname (resolution filtered by SsrfResolver) and a private target.
        assert!(!blocked("http://example.com/login"));
        assert!(!blocked("http://10.0.0.5/health"));
    }

    #[test]
    fn retain_allowed_addrs_drops_blocked_keeps_public_and_private() {
        let addrs: Vec<SocketAddr> = [
            "127.0.0.1:80",                // loopback → dropped
            "169.254.169.254:80",          // cloud metadata (link-local) → dropped
            "[::1]:80",                    // IPv6 loopback → dropped
            "[::ffff:169.254.169.254]:80", // IPv4-mapped metadata → dropped
            "8.8.8.8:80",                  // public → kept
            "10.0.0.5:80",                 // private internal target → kept (an NMS monitors these)
        ]
        .iter()
        .map(|s| s.parse().unwrap())
        .collect();

        let allowed = retain_allowed_addrs(addrs);
        let ips: Vec<IpAddr> = allowed.iter().map(SocketAddr::ip).collect();
        assert_eq!(ips.len(), 2, "only the public + private answers survive");
        assert!(ips.contains(&"8.8.8.8".parse().unwrap()));
        assert!(ips.contains(&"10.0.0.5".parse().unwrap()));
    }

    #[test]
    fn retain_allowed_addrs_empty_when_all_blocked() {
        // A name that resolves only to SSRF-blocked addresses yields nothing → the resolver fails
        // the lookup and reqwest never dials a blocked host (the rebinding / metadata-behind-a-name
        // case the IP-literal hop check can't see).
        let addrs: Vec<SocketAddr> = ["127.0.0.1:443", "169.254.169.254:443"]
            .iter()
            .map(|s| s.parse().unwrap())
            .collect();
        assert!(retain_allowed_addrs(addrs).is_empty());
    }
}
