// SPDX-License-Identifier: AGPL-3.0-only
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
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use yagra_common::{is_ssrf_blocked, HttpAuth, HttpMethod};

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
        match yagra_common::host_ip(host) {
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

    // Reuse one of the four cached clients (follow_redirects × verify_tls) instead of rebuilding
    // rustls config + pool + SSRF resolver wiring every poll. Timeout is a per-request property.
    let client = probe_client(spec.follow_redirects, spec.verify_tls);

    let method = match spec.method {
        HttpMethod::Get => reqwest::Method::GET,
        HttpMethod::Head => reqwest::Method::HEAD,
        HttpMethod::Post => reqwest::Method::POST,
    };

    let mut req = client.request(method, url).timeout(timeout);
    // Present credentials, if core resolved any. `basic_auth`/`bearer_auth` set `Authorization`,
    // which reqwest strips on a cross-origin redirect; a custom header is **not** stripped, so the
    // header scheme is refused at the API edge unless redirects are off (see `api/checks.rs`).
    if let Some(auth) = &spec.auth {
        req = match auth {
            HttpAuth::Basic { username, password } => req.basic_auth(username, Some(password)),
            HttpAuth::Bearer { token } => req.bearer_auth(token),
            HttpAuth::Header { name, value } => req.header(name, value),
        };
    }

    let started = Instant::now();
    match req.send().await {
        Ok(mut resp) => {
            // ⚠️ Measured here and nowhere else. `send()` resolves once the headers are in, so this
            // is time-to-response-headers — the documented meaning of `http_response_time_ms`. The
            // optional body read below happens *after* the stopwatch on purpose: letting it grow to
            // include the read would silently redefine the metric for every existing series, and
            // only for the monitors that happen to carry a content rule.
            let response_time_ms = started.elapsed().as_secs_f64() * 1000.0;
            let status_code = Some(resp.status().as_u16());
            let cert_days_to_expiry = resp
                .extensions()
                .get::<reqwest::tls::TlsInfo>()
                .and_then(reqwest::tls::TlsInfo::peer_certificate)
                .and_then(cert_days_to_expiry);
            let body = match spec.body_capture_bytes {
                Some(cap) => Some(capture_body(&mut resp, cap as usize).await),
                None => None,
            };
            Ok(HttpProbe {
                reachable: true,
                status_code,
                response_time_ms,
                cert_days_to_expiry,
                body,
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
                body: None,
            })
        }
    }
}

/// Read at most `cap` bytes of `resp`'s body, reporting whether anything was left unread.
///
/// Streamed chunk by chunk rather than `resp.bytes()`, which would buffer the *whole* response
/// before any limit could apply — the exact shape that turns one misconfigured monitor pointed at a
/// large download into a poller OOM. The request-level timeout still covers this read, so a body
/// that stops arriving cannot hang the worker.
///
/// A read error mid-body is reported as `truncated`, not as a failure: the headers already arrived,
/// the status is real, and "bytes exist that we did not examine" is exactly what a partial read
/// means to a keyword rule.
async fn capture_body(resp: &mut reqwest::Response, cap: usize) -> crate::BodyCapture {
    let mut buf: Vec<u8> = Vec::new();
    let mut truncated = false;
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                if fill_capped(&mut buf, &chunk, cap) {
                    truncated = true;
                    break;
                }
            }
            Ok(None) => break,
            Err(e) => {
                tracing::debug!(error = %e, "http probe body read did not complete");
                truncated = true;
                break;
            }
        }
    }
    crate::BodyCapture {
        text: String::from_utf8_lossy(&buf).into_owned(),
        truncated,
    }
}

/// Append as much of `chunk` to `buf` as `cap` allows. Returns `true` when the budget stopped it —
/// i.e. bytes exist that were not captured.
///
/// Split out from the read loop because it is the whole judgement: a real `reqwest::Response` is
/// not constructible in a unit test, so leaving this arithmetic inline would leave the off-by-one
/// untested. Pure, so it is.
fn fill_capped(buf: &mut Vec<u8>, chunk: &[u8], cap: usize) -> bool {
    let room = cap.saturating_sub(buf.len());
    if chunk.len() >= room {
        buf.extend_from_slice(&chunk[..room]);
        // `>=` rather than `>`: a chunk that exactly fills the budget leaves us unable to tell
        // whether more follows, so the capture counts as incomplete. Erring toward "indeterminate"
        // is the safe direction — see `yagra_common::BodyMatch::satisfied_by`.
        return true;
    }
    buf.extend_from_slice(chunk);
    false
}

/// One of the four static probe-client variants (`follow_redirects` × `verify_tls`), built once and
/// reused. Rebuilding the rustls config, connection pool, and SSRF-resolver wiring on every poll was
/// pure per-probe overhead. `pool_max_idle_per_host(0)` keeps the deliberate fresh-connect-per-probe
/// measurement semantics (response time includes the TCP+TLS handshake; `tls_info` needs a real
/// handshake to read the cert), so only the *construction* cost is removed. Timeout is applied
/// per-request at the call site, not baked into the client.
fn probe_client(follow_redirects: bool, verify_tls: bool) -> &'static reqwest::Client {
    static CLIENTS: std::sync::OnceLock<[reqwest::Client; 4]> = std::sync::OnceLock::new();
    let clients = CLIENTS.get_or_init(|| {
        std::array::from_fn(|i| {
            // A build failure here means the TLS backend itself can't initialize — every probe
            // would fail regardless, so surfacing it as a panic at first use is appropriate (and
            // the SSRF resolver is load-bearing, so there is no safe degraded fallback client).
            build_probe_client(i & 0b10 != 0, i & 0b01 != 0)
                .expect("probe HTTP client build failed (TLS backend init)")
        })
    });
    &clients[(usize::from(follow_redirects) << 1) | usize::from(verify_tls)]
}

/// Build one probe client with the SSRF resolver, redirect policy, and TLS-verify setting wired in.
/// Split out so [`probe_client`] can cache the (small, fixed) set of variants.
fn build_probe_client(
    follow_redirects: bool,
    verify_tls: bool,
) -> Result<reqwest::Client, TransportError> {
    let redirect = if follow_redirects {
        // Re-validate every redirect hop, not just the initial target. Two complementary guards
        // cover the whole SSRF surface across hops: this policy refuses an IP-*literal* hop host
        // (`302 Location: http://169.254.169.254/…`) or a bad scheme before the request is made,
        // and the `SsrfResolver` below refuses a *hostname* hop that resolves to a blocked address
        // (the DNS-rebinding / metadata-behind-a-name case the literal check can't see). Since
        // reqwest dials exactly what the resolver returns, there is no resolve-then-connect TOCTOU.
        // The closure is stateless (depends only on the hop URL), so one client can be cached.
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
    reqwest::Client::builder()
        .redirect(redirect)
        // Pin DNS to an SSRF-filtered resolver: reqwest resolves — and therefore dials — only
        // non-blocked addresses, for the initial target and every hostname redirect hop. This is
        // the authoritative, TOCTOU-free enforcement (the resolved set is the dialed set).
        .dns_resolver(Arc::new(SsrfResolver))
        // verify_tls is off only by explicit operator opt-in (default on — security.md).
        .danger_accept_invalid_certs(!verify_tls)
        .tls_info(true)
        .user_agent("Yagra-poller")
        // No idle-connection reuse: each probe measures a fresh handshake (see `probe_client`).
        .pool_max_idle_per_host(0)
        .build()
        .map_err(|e| TransportError::Io(format!("http client build failed: {e}")))
}

/// Whether a redirect hop's URL must be refused (per-hop SSRF defense). Blocks non-http(s)
/// schemes and IP-literal hosts that are SSRF-blocked; a hostname hop is allowed (see the
/// redirect-policy comment in `probe_http`).
fn redirect_hop_blocked(url: &reqwest::Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return true;
    }
    url.host_str()
        .and_then(yagra_common::host_ip)
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
    use std::net::IpAddr;

    #[tokio::test]
    async fn rejects_non_http_scheme() {
        let spec = HttpProbeSpec {
            url: "ftp://example.com/x".to_owned(),
            method: HttpMethod::Get,
            verify_tls: true,
            follow_redirects: true,
            auth: None,
            body_capture_bytes: None,
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
            auth: None,
            body_capture_bytes: None,
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
            auth: None,
            body_capture_bytes: None,
        };
        let err = probe_http(&spec, Duration::from_millis(500)).await;
        assert!(matches!(err, Err(TransportError::Io(_))));
    }

    #[test]
    fn unparseable_cert_yields_none() {
        assert_eq!(cert_days_to_expiry(b"not a cert"), None);
    }

    #[test]
    fn a_body_under_the_budget_is_captured_whole_and_not_marked_truncated() {
        // The common case, and the one a false `truncated` would break: every keyword rule would
        // go permanently indeterminate, which reads as "0" on the gauge and pages for nothing.
        let mut buf = Vec::new();
        assert!(!fill_capped(&mut buf, b"hello ", 64));
        assert!(!fill_capped(&mut buf, b"world", 64));
        assert_eq!(buf, b"hello world");
    }

    #[test]
    fn a_body_over_the_budget_stops_at_the_cap_and_reports_truncation() {
        let mut buf = Vec::new();
        assert!(!fill_capped(&mut buf, b"1234", 8));
        // This chunk overruns: keep the 4 bytes that fit, and say bytes went unexamined.
        assert!(fill_capped(&mut buf, b"56789abc", 8));
        assert_eq!(buf, b"12345678");

        // A chunk that lands exactly on the budget also counts as truncated — we cannot see
        // whether more follows, and guessing "that was all" is the direction that lies.
        let mut exact = Vec::new();
        assert!(fill_capped(&mut exact, b"12345678", 8));
        assert_eq!(exact, b"12345678");

        // Already full: nothing more is taken, and it stays truncated.
        let mut full = b"12345678".to_vec();
        assert!(fill_capped(&mut full, b"more", 8));
        assert_eq!(full, b"12345678");
    }

    #[test]
    fn a_captured_body_never_prints_itself() {
        // `HttpProbe` derives Debug and now carries the response body, so one `debug!(?probe)`
        // would dump up to a megabyte of a monitored endpoint's page — which may hold session
        // data, personal data, or a token the page rendered. Same trap `HttpAuth` already has a
        // manual impl for; this is the test that keeps a future `#[derive(Debug)]` from reopening it.
        let probe = HttpProbe {
            reachable: true,
            status_code: Some(200),
            response_time_ms: 5.0,
            cert_days_to_expiry: None,
            body: Some(crate::BodyCapture {
                text: "session=hunter2; user=alice".to_owned(),
                truncated: true,
            }),
        };
        let shown = format!("{probe:?}");
        assert!(!shown.contains("hunter2"), "body content must not print");
        assert!(!shown.contains("alice"));
        // The diagnostic facts still survive — they are the only thing such a log line is asked.
        assert!(shown.contains("truncated: true"));
        assert!(shown.contains("len: 27"));
    }

    #[test]
    fn a_cut_multibyte_character_degrades_to_a_replacement_rather_than_losing_the_capture() {
        // The cut lands on a byte boundary, so the capture must survive a split character —
        // discarding the whole body over its last byte would make a rule undecidable at random.
        let mut buf = Vec::new();
        assert!(fill_capped(&mut buf, "ok日".as_bytes(), 3)); // "ok" + the first byte of 日
        let text = String::from_utf8_lossy(&buf);
        assert!(
            text.starts_with("ok"),
            "the readable prefix survives: {text}"
        );
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
