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
use std::net::IpAddr;
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
                // A resolvable name whose every answer is blocked is refused; a DNS failure falls
                // through and is reported as unreachable below (reqwest will fail the same way).
                if let Ok(addrs) = tokio::net::lookup_host((host, port)).await {
                    let addrs: Vec<_> = addrs.collect();
                    if !addrs.is_empty() && addrs.iter().all(|a| is_ssrf_blocked(a.ip())) {
                        return Err(TransportError::Io("blocked target address".to_owned()));
                    }
                }
            }
        }
    }

    let redirect = if spec.follow_redirects {
        // Re-validate every redirect hop, not just the initial target: a `302 Location:
        // http://169.254.169.254/…` (or any loopback/link-local IP literal) would otherwise
        // defeat the guard above. Refuse any hop to an unsupported scheme or an SSRF-blocked
        // IP-literal host before the request to it is ever made. (A hop to a *hostname* is
        // allowed here; the initial target's resolved addresses were already checked, and the
        // metadata/loopback escalation surface always arrives as an IP literal in `Location`.)
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
                                                // Allowed hops: a hostname (resolution checked at hop 0) and a private internal target.
        assert!(!blocked("http://example.com/login"));
        assert!(!blocked("http://10.0.0.5/health"));
    }
}
