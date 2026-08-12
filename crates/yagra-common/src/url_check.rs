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
/// Stable TSDB metric: how long the endpoint took to answer, in milliseconds.
///
/// **This is time-to-response-*headers*, not time-to-body-complete.** The probe never reads the
/// response body (`reqwest`'s `send()` resolves once the headers are in), so the measurement covers
/// DNS + TCP + TLS handshake + request + first response byte. Body matching (Inc.2) and JSON
/// extraction (Inc.3) do read the body, **after** this is measured — keep it at that point rather
/// than letting it grow to include the read, or the same metric name silently changes meaning for
/// every existing series, and only for the monitors that happen to use a body feature.
///
/// Emitted **only when the endpoint answered**. See the poller's HTTP arm for why an unreachable
/// probe writes no sample here (it would be a timeout duration, not a response time).
///
/// No default threshold is seeded — response latency varies far too much between environments, the
/// same call ADR-033 made for `dns_resolve_ms`.
pub const METRIC_HTTP_RESPONSE_TIME_MS: &str = "http_response_time_ms";
/// Stable TSDB metric: days until the TLS server certificate's `notAfter` (HTTPS only).
pub const METRIC_SSL_CERT_DAYS_TO_EXPIRY: &str = "ssl_cert_days_to_expiry";
/// Stable TSDB metric: `1` = the response body satisfied the monitor's [`BodyMatch`] rule, `0` = it
/// did not — **or the rule could not be decided** within the byte budget (ADR-047 Inc.2).
///
/// Emitted only when a rule is configured and the endpoint answered. Deliberately **not** folded
/// into [`METRIC_HTTP_UP`]: that gauge's documented meaning is liveness + status expectation, and
/// widening it would silently change what every existing `http_up` series means. A separate gauge
/// also lets an operator alert on content and availability at different severities.
///
/// A seeded `below 0.5` threshold on the built-in URL profile makes it alert out of the box (the
/// 0/1-gauge bound trap of migration 0030 — the engine's below-comparison is inclusive).
pub const METRIC_HTTP_BODY_MATCH: &str = "http_body_match";
/// Stable TSDB metric (diagnostic, no threshold): `1` = the body was larger than the monitor's
/// [`UrlCheckConfig::body_max_bytes`] so only a prefix was examined, `0` = the whole body was read.
///
/// This is what separates the two ways [`METRIC_HTTP_BODY_MATCH`] reaches `0` — "the keyword is
/// genuinely gone" from "the page outgrew the budget and we could not tell". Without it the alert
/// is the same in both cases and the second is undiagnosable at 3am.
pub const METRIC_HTTP_BODY_TRUNCATED: &str = "http_body_truncated";

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
/// Default budget for reading a URL monitor's response body: the same 64 KB the webhook-ingest
/// edge accepts.
pub const DEFAULT_BODY_MAX_BYTES: u32 = 64 * 1024;
/// Smallest and largest read budget an operator may configure. The floor keeps a rule from being
/// configured into permanent indeterminacy; the ceiling bounds what one poll pulls into poller
/// memory (the probe holds the captured prefix, not the whole response).
pub const BODY_MAX_BYTES_RANGE: std::ops::RangeInclusive<u32> = 1024..=1024 * 1024;
/// Longest keyword accepted. Long enough for a JSON fragment, short enough that the rule stays a
/// keyword rather than a smuggled document.
pub const BODY_PATTERN_MAX_LEN: usize = 512;

const fn default_body_max_bytes() -> u32 {
    DEFAULT_BODY_MAX_BYTES
}

/// How a [`BodyMatch`] keyword is meant to relate to the response body. The runtime array is what
/// makes per-member coverage testable — the UI builds a `t()` key from it (extensibility.md §4).
pub const BODY_MATCH_MODES: [&str; 2] = ["contains", "not_contains"];

/// Whether the keyword must be present or absent for the body to be considered healthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BodyMatchMode {
    /// The body must contain the keyword (a health page that must say `"status":"ok"`).
    #[default]
    Contains,
    // `not_contains` is the arm that catches what `http_up` structurally cannot see: a 200 whose
    // body reports the failure. Keep that reasoning here, not in the `///` — see below.
    /// The body must **not** contain the keyword (a page that answers 200 while saying
    /// `Database unavailable`).
    NotContains,
}

impl BodyMatchMode {
    /// The stable token, matching the serde tag and [`BODY_MATCH_MODES`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::NotContains => "not_contains",
        }
    }
}

// ⚠️ This type derives `ToSchema`, so every `///` below is published **verbatim** to API clients
// and into the public API reference. Client-facing description in `///`; the reasoning behind the
// design in `//`, like this. Two decisions worth keeping written down:
//
//  * **Case-sensitive, with no flag to relax it.** The safe direction for a monitor is a false
//    alert, never a false OK: case-folding matches *more*, so `not_contains` would be the mode that
//    quietly stopped catching things. If a case-insensitive mode is ever wanted it is a
//    `#[serde(default)]` field addition away — the cheap direction to move in.
//  * **Presence of the rule is what makes the poller read the body at all.** Without one the probe
//    still stops at the response headers, which is what keeps `METRIC_HTTP_RESPONSE_TIME_MS`
//    meaning the same thing for every monitor.
/// A keyword rule applied to a URL monitor's response body.
///
/// When present, the monitor reports `http_body_match` (`1` satisfied / `0` not) in addition to
/// `http_up`. How much of the body is read is [`UrlCheckConfig::body_max_bytes`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BodyMatch {
    /// The keyword to look for. Matched as plain, case-sensitive text — not a regular expression.
    pub pattern: String,
    /// Whether the keyword must be present or absent (default: present).
    #[serde(default)]
    pub mode: BodyMatchMode,
}

impl BodyMatch {
    /// A `contains` rule.
    #[must_use]
    pub fn contains(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            mode: BodyMatchMode::Contains,
        }
    }

    /// Whether `body` satisfies this rule — or `None` when the answer **depends on bytes that were
    /// never read** (`truncated` and the keyword absent from the prefix).
    ///
    /// The `None` arm is the whole point of the type. Silently reporting "not matched" for a
    /// truncated body turns a `NotContains` monitor into one that reports healthy for a page whose
    /// error text sits past the budget — the "quietly normal" lie ADR-047 決定 3 forbids. The
    /// caller decides what to do with the indeterminate answer; what it must not do is call it OK.
    ///
    /// Finding the keyword is always definitive: the bytes past the cut cannot un-find it.
    #[must_use]
    pub fn satisfied_by(&self, body: &str, truncated: bool) -> Option<bool> {
        let found = body.contains(&self.pattern);
        match (self.mode, found) {
            (BodyMatchMode::Contains, true) => Some(true),
            (BodyMatchMode::NotContains, true) => Some(false),
            // Absent from what we read. Definitive only if "what we read" was all of it.
            (_, false) if truncated => None,
            (BodyMatchMode::Contains, false) => Some(false),
            (BodyMatchMode::NotContains, false) => Some(true),
        }
    }
}

/// Most extraction rules one monitor may carry. A bound rather than a judgement about what is
/// useful: each rule is a new TSDB series per node, and an unbounded list on an operator-editable
/// object is how a thin-label model (ADR-011) gets widened without anyone deciding to.
pub const MAX_JSON_EXTRACTS: usize = 8;
/// Longest extraction path accepted.
pub const JSON_PATH_MAX_LEN: usize = 256;
/// Longest operator-chosen metric name accepted.
pub const METRIC_NAME_MAX_LEN: usize = 96;

/// The metric names a URL monitor emits on its own, which an extraction rule must therefore not
/// claim. Derived from the constants rather than re-listed, so a metric added above cannot go
/// missing here — the collision it would allow is an operator's extracted value **overwriting the
/// availability series** for their own node.
#[must_use]
pub const fn url_monitor_reserved_metrics() -> [&'static str; 6] {
    [
        METRIC_HTTP_UP,
        METRIC_HTTP_STATUS_CODE,
        METRIC_HTTP_RESPONSE_TIME_MS,
        METRIC_SSL_CERT_DAYS_TO_EXPIRY,
        METRIC_HTTP_BODY_MATCH,
        METRIC_HTTP_BODY_TRUNCATED,
    ]
}

// ⚠️ `ToSchema`: the `///` below is published verbatim to API clients. Design notes go in `//`.
//
// **Why a dotted path and deliberately not JSONPath.** A rule must resolve to exactly one number,
// because the sample it produces carries only the `node` label (ADR-011). JSONPath's wildcards and
// filters can select many values, which would force either a reduction rule nobody asked for or a
// silent "first match wins" — and it would add a dependency whose expression cost is unbounded on
// operator-supplied input. A path that can only ever name one location has none of those problems.
/// One number to lift out of a JSON response body and record as a metric.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct JsonExtract {
    /// The metric name to record the value under, e.g. `queue_depth`. Must match
    /// `[A-Za-z_:][A-Za-z0-9_:]*` and must not be one of the names the monitor already emits.
    pub metric: String,
    /// Dot-separated path to the value, e.g. `data.queue.depth` or `items.0.value`. Array elements
    /// are indexed by number; `items[0].value` is accepted and means the same thing.
    ///
    /// Not a JSONPath expression: the path names exactly one location, so the rule always produces
    /// either one number or nothing.
    pub path: String,
}

impl JsonExtract {
    /// The value this rule lifts out of `doc`, or `None` when there is nothing usable there.
    ///
    /// `None` covers every failure — path missing, wrong type, unparseable string, non-finite —
    /// and the caller records **no sample** for it. Writing `0` instead would be indistinguishable
    /// from the value genuinely being `0`, which is the ADR-047 決定 3 rule: a monitor may not
    /// invent a reading it did not take.
    #[must_use]
    pub fn extract(&self, doc: &serde_json::Value) -> Option<f64> {
        resolve_json_path(doc, &self.path).and_then(json_metric_value)
    }
}

/// Walk a dot-separated path into `doc`. `None` if any segment is missing or the path runs into a
/// scalar. An empty segment (`a..b`, a leading or trailing dot) fails rather than being skipped —
/// silently ignoring it would make two different paths mean the same thing.
#[must_use]
pub fn resolve_json_path<'a>(
    doc: &'a serde_json::Value,
    path: &str,
) -> Option<&'a serde_json::Value> {
    // `items[0].value` → `items.0.value`, so operators can write whichever they have the habit of.
    let normalized = path.replace('[', ".").replace(']', "");
    let mut node = doc;
    for segment in normalized.split('.') {
        if segment.is_empty() {
            return None;
        }
        node = match node {
            // An array is indexed by number; anything else in that position is a miss, not a key.
            serde_json::Value::Array(items) => {
                segment.parse::<usize>().ok().and_then(|i| items.get(i))?
            }
            serde_json::Value::Object(map) => map.get(segment)?,
            _ => return None,
        };
    }
    Some(node)
}

/// Coerce a JSON value to the number a gauge can carry, or `None` if it is not one.
///
/// Booleans map to 1/0 because an API health field is often `"healthy": true`; the resulting 0/1
/// gauge wants the same 0.5 threshold bound as every other boolean here (migration 0030's trap).
/// Numeric strings are accepted because plenty of APIs quote their numbers.
#[must_use]
pub fn json_metric_value(v: &serde_json::Value) -> Option<f64> {
    let n = match v {
        serde_json::Value::Number(n) => n.as_f64()?,
        serde_json::Value::Bool(b) => f64::from(u8::from(*b)),
        // `"42"` is a number an API quoted. `"NaN"` and `"inf"` also parse as f64, which is why
        // the finiteness check below is not decoration — a NaN reaching the TSDB poisons the series
        // and every aggregate over it.
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok()?,
        serde_json::Value::Null | serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            return None
        }
    };
    n.is_finite().then_some(n)
}

/// The auth schemes a URL monitor can present. The runtime array is what makes per-member coverage
/// testable — the UI builds a `t()` key from it (extensibility.md §4).
pub const HTTP_AUTH_SCHEMES: [&str; 3] = ["basic", "bearer", "header"];

/// Resolved credentials for a URL monitor — the *decrypted* value, inlined into the poll job by
/// core exactly as SNMP auth is (ADR-018/020). The stored side is a [`CredentialId`] reference.
///
/// **This type deliberately does not derive `Debug`.** It travels inside `HttpCheck` → `CheckSpec`
/// → `PollJob`, all of which do derive it, so a single `tracing::debug!(?job)` anywhere would
/// otherwise print the password. The manual impl below prints the shape and redacts the value.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scheme", rename_all = "snake_case")]
pub enum HttpAuth {
    /// RFC 7617 Basic — sent as an `Authorization: Basic` header.
    Basic { username: String, password: String },
    /// RFC 6750 Bearer — sent as an `Authorization: Bearer` header.
    Bearer { token: String },
    /// An arbitrary header, for APIs that use their own (e.g. `X-API-Key`).
    Header { name: String, value: String },
}

impl HttpAuth {
    /// The scheme token, matching the serde tag and [`HTTP_AUTH_SCHEMES`].
    #[must_use]
    pub const fn scheme(&self) -> &'static str {
        match self {
            Self::Basic { .. } => "basic",
            Self::Bearer { .. } => "bearer",
            Self::Header { .. } => "header",
        }
    }
}

impl std::fmt::Debug for HttpAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // The username is structural (it identifies which account, not how to use it) and is
            // what makes a misconfiguration diagnosable; the password never appears.
            Self::Basic { username, .. } => f
                .debug_struct("HttpAuth::Basic")
                .field("username", username)
                .field("password", &"<redacted>")
                .finish(),
            Self::Bearer { .. } => f
                .debug_struct("HttpAuth::Bearer")
                .field("token", &"<redacted>")
                .finish(),
            // The header *name* is not a secret and is the field most likely to be wrong.
            Self::Header { name, .. } => f
                .debug_struct("HttpAuth::Header")
                .field("name", name)
                .field("value", &"<redacted>")
                .finish(),
        }
    }
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
    // security.md is what forbids the silent-disable; the citation stays in `//` because this
    // `///` is published verbatim to API clients, who cannot open that file.
    /// Verify the TLS certificate chain (default `true`). Turning it off is an explicit operator
    /// choice — it is never disabled silently.
    #[serde(default = "default_true")]
    pub verify_tls: bool,
    /// Follow 3xx redirects (default `true`).
    #[serde(default = "default_true")]
    pub follow_redirects: bool,
    /// Per-request timeout, in milliseconds (default 5000).
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u32,
    /// Optional auth credential (Basic/Bearer/custom header). A reference only — core resolves it
    /// to an [`HttpAuth`] and inlines that into the poll job, the same path SNMP credentials take.
    #[serde(default)]
    pub credential: Option<CredentialId>,
    // A poller that predates this field drops it and reports `http_up = 1` for a page it never
    // looked at — indistinguishable from "the content check passed". Core therefore withholds a
    // monitor carrying one from any poller not advertising `CAP_HTTP_BODY`, the same gate and the
    // same reason as `credential`. (`///` here is published to API clients; this note is not.)
    /// Optional keyword rule applied to the response body.
    #[serde(default)]
    pub body_match: Option<BodyMatch>,
    // Deliberately NOT capability-gated, unlike `body_match` — see `spec_required_caps`. A poller
    // that drops this field records no sample, so the failure is an *absent series*, which is
    // visible. Withholding the spec instead would stop the whole monitor, taking `http_up` with it:
    // that trades one missing extra metric for a total loss of availability monitoring, which is
    // the worse outcome and the opposite of what the `body_match` gate is for.
    /// Values to lift out of a JSON response body and record as operator-named metrics.
    ///
    /// Each rule adds one gauge per poll. A rule whose path is missing, or whose value is not a
    /// number, records **nothing** for that poll rather than a zero.
    #[serde(default)]
    pub json_extract: Vec<JsonExtract>,
    // One body, one read, one budget. This sat on `BodyMatch` first, which left "what is the budget
    // when only extraction is configured" needing an answer nobody would remember.
    /// How many bytes of the response body to read (default 65536, range 1024–1048576). Applies to
    /// both `body_match` and `json_extract`; the body is not read at all unless one of them is set.
    #[serde(default = "default_body_max_bytes")]
    pub body_max_bytes: u32,
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
            body_match: None,
            json_extract: Vec::new(),
            body_max_bytes: default_body_max_bytes(),
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
        // No rule ⇒ the probe never reads the body. Every monitor that existed before ADR-047
        // Inc.2 deserializes to exactly this, so none of them start paying for a body read.
        assert_eq!(cfg.body_match, None);
    }

    #[test]
    fn a_body_rule_defaults_to_contains_with_the_64k_budget() {
        // The wire form a UI (or an N+1 core) may send with only the keyword filled in.
        let cfg: UrlCheckConfig = serde_json::from_str(
            r#"{"url":"https://example.com/health","body_match":{"pattern":"\"status\":\"ok\""}}"#,
        )
        .unwrap();
        assert_eq!(cfg.body_max_bytes, DEFAULT_BODY_MAX_BYTES);
        assert!(cfg.json_extract.is_empty());
        let rule = cfg.body_match.expect("rule present");
        assert_eq!(rule.mode, BodyMatchMode::Contains);
        assert_eq!(rule.pattern, r#""status":"ok""#);
    }

    #[test]
    fn a_found_keyword_is_definitive_even_when_the_body_was_truncated() {
        // Bytes past the cut cannot un-find a keyword, so truncation is irrelevant on this side.
        let must = BodyMatch::contains("ok");
        assert_eq!(must.satisfied_by("all ok here", true), Some(true));
        assert_eq!(must.satisfied_by("all ok here", false), Some(true));

        let must_not = BodyMatch {
            pattern: "Database unavailable".to_owned(),
            mode: BodyMatchMode::NotContains,
        };
        // A 200 whose body says the thing that must not be there — the case `http_up` cannot see.
        assert_eq!(
            must_not.satisfied_by("<h1>Database unavailable</h1>", true),
            Some(false)
        );
    }

    #[test]
    fn an_absent_keyword_is_indeterminate_only_when_bytes_went_unread() {
        // The whole reason `satisfied_by` returns an Option. Read it all ⇒ a real answer…
        let must = BodyMatch::contains("ok");
        assert_eq!(must.satisfied_by("nothing here", false), Some(false));
        let must_not = BodyMatch {
            pattern: "unavailable".to_owned(),
            mode: BodyMatchMode::NotContains,
        };
        assert_eq!(must_not.satisfied_by("all good", false), Some(true));

        // …truncated ⇒ no answer, in BOTH modes. The `not_contains` half is the one that matters:
        // reporting `Some(true)` there is a monitor that says "healthy" about a page whose error
        // text sits past the budget, which is the silent lie ADR-047 決定 3 forbids.
        assert_eq!(must.satisfied_by("nothing here", true), None);
        assert_eq!(must_not.satisfied_by("all good", true), None);
    }

    #[test]
    fn matching_is_case_sensitive_and_that_is_the_safe_direction() {
        // Documented, not incidental: folding case matches more, so the mode that would quietly
        // stop catching things is `not_contains`. A false alert is recoverable; a false OK is not.
        let must_not = BodyMatch {
            pattern: "ERROR".to_owned(),
            mode: BodyMatchMode::NotContains,
        };
        assert_eq!(must_not.satisfied_by("error", false), Some(true));
        assert_eq!(must_not.satisfied_by("ERROR", false), Some(false));
    }

    fn doc() -> serde_json::Value {
        serde_json::json!({
            "status": "ok",
            "healthy": true,
            "queue": { "depth": 42, "lag_s": 1.5 },
            "quoted": "17",
            "items": [{ "value": 7 }, { "value": 9 }],
            "nothing": null,
        })
    }

    #[test]
    fn a_path_names_exactly_one_place_through_objects_and_arrays() {
        let at = |p: &str| resolve_json_path(&doc(), p).and_then(json_metric_value);
        assert_eq!(at("queue.depth"), Some(42.0));
        assert_eq!(at("queue.lag_s"), Some(1.5));
        assert_eq!(at("items.1.value"), Some(9.0));
        // Bracket syntax is normalized, because that is what operators type out of habit.
        assert_eq!(at("items[0].value"), Some(7.0));
    }

    #[test]
    fn a_path_that_does_not_lead_to_a_number_yields_nothing_rather_than_zero() {
        // The whole rule of ADR-047 決定 3: `0` is a reading, and a monitor must not invent one.
        // Each of these would otherwise be recorded as a perfectly plausible zero.
        let at = |p: &str| resolve_json_path(&doc(), p).and_then(json_metric_value);
        assert_eq!(at("queue.missing"), None); // key absent
        assert_eq!(at("items.9.value"), None); // index past the end
        assert_eq!(at("queue.depth.deeper"), None); // path runs into a scalar
        assert_eq!(at("status"), None); // a non-numeric string
        assert_eq!(at("nothing"), None); // null
        assert_eq!(at("queue"), None); // an object is not a number
        assert_eq!(at("items"), None); // nor is an array
        assert_eq!(at(""), None); // empty path
        assert_eq!(at("queue..depth"), None); // empty segment, not silently skipped
        assert_eq!(at("items.x.value"), None); // non-numeric index into an array
    }

    #[test]
    fn booleans_and_quoted_numbers_are_accepted_but_nan_and_infinity_are_not() {
        let at = |p: &str| resolve_json_path(&doc(), p).and_then(json_metric_value);
        assert_eq!(
            at("healthy"),
            Some(1.0),
            "an API health flag is worth a gauge"
        );
        assert_eq!(
            at("quoted"),
            Some(17.0),
            "plenty of APIs quote their numbers"
        );

        // `"NaN"` and `"inf"` both parse as f64 — which is exactly why the finiteness check is not
        // decoration. One NaN sample poisons the series and every aggregate over it.
        for poison in ["NaN", "inf", "-inf", "infinity"] {
            assert_eq!(
                json_metric_value(&serde_json::Value::String(poison.to_owned())),
                None,
                "{poison} must never reach the TSDB"
            );
        }
    }

    #[test]
    fn an_extraction_may_not_claim_a_metric_the_monitor_already_emits() {
        // The hazard is specific: an operator naming their extraction `http_up` would overwrite the
        // availability series for their own node with an arbitrary number from a JSON body.
        // Derived from the constants, so a metric added above cannot go missing from the guard.
        let reserved = url_monitor_reserved_metrics();
        assert!(reserved.contains(&METRIC_HTTP_UP));
        assert!(reserved.contains(&METRIC_HTTP_BODY_MATCH));
        assert!(!reserved.contains(&"queue_depth"));
    }

    #[test]
    fn every_mode_token_matches_its_serde_tag_and_the_runtime_list() {
        // Same pairing as the auth schemes above: `as_str` and `rename_all` are two mechanisms and
        // nothing makes them agree, so a disagreement means core writes a row the poller can't read.
        let all = [BodyMatchMode::Contains, BodyMatchMode::NotContains];
        for m in all {
            let json = serde_json::to_value(m).unwrap();
            assert_eq!(json, serde_json::Value::String(m.as_str().to_owned()));
            assert!(BODY_MATCH_MODES.contains(&m.as_str()));
        }
        assert_eq!(all.len(), BODY_MATCH_MODES.len());
    }

    #[test]
    fn http_auth_debug_never_prints_a_secret() {
        // This type rides inside HttpCheck -> CheckSpec -> PollJob, every one of which derives
        // Debug, so one tracing::debug!(?job) would dump the password if the impl regressed.
        let basic = HttpAuth::Basic {
            username: "probe".to_owned(),
            password: "hunter2".to_owned(),
        };
        let shown = format!("{basic:?}");
        assert!(
            shown.contains("probe"),
            "the username is diagnostic and should be shown"
        );
        assert!(!shown.contains("hunter2"));

        let bearer = HttpAuth::Bearer {
            token: "tok-abc".to_owned(),
        };
        assert!(!format!("{bearer:?}").contains("tok-abc"));

        let header = HttpAuth::Header {
            name: "X-API-Key".to_owned(),
            value: "sekrit".to_owned(),
        };
        let shown = format!("{header:?}");
        assert!(
            shown.contains("X-API-Key"),
            "the header name is not a secret"
        );
        assert!(!shown.contains("sekrit"));
    }

    #[test]
    fn every_scheme_token_matches_its_serde_tag_and_the_runtime_list() {
        // Two mechanisms produce this token — scheme() and serde(rename_all) — and nothing makes
        // them agree, so a disagreement means core writes a shape the poller cannot read.
        let all = [
            HttpAuth::Basic {
                username: String::new(),
                password: String::new(),
            },
            HttpAuth::Bearer {
                token: String::new(),
            },
            HttpAuth::Header {
                name: String::new(),
                value: String::new(),
            },
        ];
        for a in &all {
            let json = serde_json::to_value(a).unwrap();
            assert_eq!(json["scheme"], a.scheme());
            assert!(HTTP_AUTH_SCHEMES.contains(&a.scheme()));
        }
        assert_eq!(all.len(), HTTP_AUTH_SCHEMES.len());
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
