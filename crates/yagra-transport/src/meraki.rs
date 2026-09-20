// SPDX-License-Identifier: AGPL-3.0-only
//! Real Cisco Meraki Dashboard API transport (`reqwest` over rustls) — strictly **READ-ONLY**.
//!
//! The Dashboard API is org-scoped and bulk: one paged GET returns data for many devices. A
//! [`collect`] call pages one [`MerakiTier`] of endpoints for an organization and returns raw
//! per-device observations; the poller fans those out to per-node results. [`list_organizations`]
//! backs "Add organization" in core, and [`fetch_inventory`] its periodic inventory sync — the one
//! reader of an organization's networks and devices since the import wizard and its two lenient
//! listings went (ADR-164 Inc.5). All of it lives here so every byte of Meraki I/O goes through
//! one place.
//!
//! Safeguards baked in (never affect the customer's Meraki):
//! * **GET only.** The only reqwest verb used anywhere in this module is `.get()`; there is no code
//!   path that writes. Redirects are disabled (the sole "next" is a validated `Link` header).
//! * **Host allow-list.** Every request URL — the initial one and every pagination `Link: rel=next`
//!   — is checked with [`is_meraki_api_host`] before it is issued, so the bearer key can never be
//!   sent off-host (credential-exfiltration guard).
//! * **Paced + Retry-After.** Requests are spaced to `target_rps` (well under the org cap, headroom
//!   for the customer), and a 429 is obeyed via its `Retry-After` header, then adaptively backed off.
//! * **Bounded.** Pagination is capped; a transient network/5xx failure returns the partial results
//!   collected so far rather than hammering.

use crate::{
    MerakiCollectSpec, MerakiCollected, MerakiObservation, MerakiSample, MerakiUplink,
    TransportError,
};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use yagra_common::{
    is_meraki_api_host, uplink_ifindex, uplink_name, uplink_status_value, MerakiTier,
    METRIC_MERAKI_CLIENT_COUNT, METRIC_MERAKI_DEVICE_UP, METRIC_MERAKI_UPLINK_LATENCY_MS,
    METRIC_MERAKI_UPLINK_LOSS_PCT, METRIC_MERAKI_UPLINK_STATUS, METRIC_MERAKI_USAGE_RECV_KB,
    METRIC_MERAKI_USAGE_SENT_KB,
};

/// Dashboard API v1 path prefix (appended to the org's `base_url`).
const API_PREFIX: &str = "/api/v1";
/// Hard cap on pages per endpoint (bounded-pagination safeguard).
const MAX_PAGES: usize = 50;
/// Hard cap on consecutive 429/Retry-After waits before giving up on an endpoint.
const MAX_RATE_LIMIT_RETRIES: u32 = 6;

fn io(msg: impl Into<String>) -> TransportError {
    TransportError::Io(msg.into())
}

// ── Control-plane result types (used by core's import wizard) ───────────────────────────────

/// A Meraki organization the API key can see (`GET /organizations`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerakiOrgInfo {
    /// organizationId.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Dashboard URL, if returned (diagnostic).
    pub url: Option<String>,
}

/// A network within an org (`GET /organizations/{orgId}/networks`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerakiNetworkInfo {
    /// networkId.
    pub id: String,
    /// Display name.
    pub name: String,
}

/// A device within an org (`GET /organizations/{orgId}/devices`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerakiDeviceInfo {
    /// Device serial (globally unique; the join key).
    pub serial: String,
    /// Display name (may be empty on Meraki's side).
    pub name: String,
    /// Model (e.g. `MX67`).
    pub model: Option<String>,
    /// Meraki productType (appliance/switch/wireless/…).
    pub product_type: String,
    /// networkId the device belongs to.
    pub network_id: String,
    /// LAN IP if the device reports one (display only — never pinged).
    pub lan_ip: Option<String>,
}

// ── Session: one per collect / control call ─────────────────────────────────────────────────

/// A Meraki API session: one reqwest client reused across all of a call's pages (keep-alive
/// amortizes the many sequential GETs — unlike `probe_http`'s per-request client), plus a request
/// pacer. Constructing it validates the base host against the allow-list up front.
struct Session {
    client: reqwest::Client,
    base: reqwest::Url,
    min_interval: Duration,
    last: Option<Instant>,
}

impl Session {
    fn new(
        base_url: &str,
        api_key: &str,
        target_rps: f64,
        timeout: Duration,
    ) -> Result<Self, TransportError> {
        let base = reqwest::Url::parse(base_url)
            .map_err(|e| io(format!("invalid meraki base url: {e}")))?;
        match base.scheme() {
            "https" => {}
            other => return Err(io(format!("meraki base url must be https, got {other}"))),
        }
        let host = base
            .host_str()
            .ok_or_else(|| io("meraki base url has no host"))?;
        if !is_meraki_api_host(host) {
            return Err(io(
                "meraki base url host is not an allow-listed Meraki API host",
            ));
        }

        let mut headers = HeaderMap::new();
        let mut bearer = HeaderValue::from_str(&format!("Bearer {api_key}"))
            .map_err(|_| io("invalid meraki api key characters"))?;
        bearer.set_sensitive(true); // keep the key out of any header debug dump
        headers.insert(AUTHORIZATION, bearer);
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .timeout(timeout)
            // READ-ONLY safety: never follow redirects — the only "next" is a Link header we
            // validate ourselves, so a 3xx can't bounce the bearer key to another host.
            .redirect(reqwest::redirect::Policy::none())
            .default_headers(headers)
            .user_agent("Yagra-poller (read-only Meraki monitor)")
            .build()
            .map_err(|e| io(format!("meraki http client build failed: {e}")))?;

        let rps = target_rps.max(0.1);
        Ok(Self {
            client,
            base,
            min_interval: Duration::from_secs_f64(1.0 / rps),
            last: None,
        })
    }

    /// Space requests to the target rate (the conservative budget safeguard).
    async fn pace(&mut self) {
        if let Some(last) = self.last {
            let elapsed = last.elapsed();
            if elapsed < self.min_interval {
                tokio::time::sleep(self.min_interval - elapsed).await;
            }
        }
        self.last = Some(Instant::now());
    }

    /// GET `path` (with `query`), following `Link: rel=next` pagination, and return the flattened
    /// JSON array items across all pages. **GET only.** Every page's host is re-checked against the
    /// allow-list. A network/5xx failure returns the items gathered so far (best-effort).
    ///
    /// ⚠️ **"Best-effort" means a short answer is an `Ok`.** That is right for a metric collect — a
    /// device missing from this round is a gap in a chart — and wrong for anything that concludes
    /// from absence. A caller that asks "which devices exist" uses [`Self::get_paged_strict`].
    async fn get_paged(
        &mut self,
        path: &str,
        query: &[(&str, String)],
        per_page: u32,
    ) -> Result<Vec<Value>, TransportError> {
        let (items, stop) = self.get_paged_traced(path, query, per_page).await;
        match stop {
            Some(stop) if stop.fails_a_collect() => Err(io(stop.collect_message())),
            _ => Ok(items),
        }
    }

    /// [`Self::get_paged`], handing back **why it stopped** beside what it gathered (ADR-164 決定
    /// 18). The three stops that fail a collect are still an `Err`; the five it survives used to be
    /// dropped on the floor here, which is how a Dashboard outage came to read as "no devices".
    async fn get_paged_reported(
        &mut self,
        path: &str,
        query: &[(&str, String)],
        per_page: u32,
    ) -> Result<(Vec<Value>, Option<MerakiFetchError>), MerakiFetchError> {
        let (items, stop) = self.get_paged_traced(path, query, per_page).await;
        match stop {
            Some(stop) if stop.fails_a_collect() => Err(stop.into()),
            stop => Ok((items, stop.map(MerakiFetchError::from))),
        }
    }

    /// [`Self::get_paged`], except that anything short of "the server said there is no next page"
    /// is an error (ADR-164 決定 2).
    ///
    /// The inventory sync marks a device `missing` when a listing does not contain it. Through the
    /// lenient reader a dropped connection on page 2 of 3 would do that to a third of an
    /// organization, and a 429 storm to all of it.
    async fn get_paged_strict(
        &mut self,
        path: &str,
        query: &[(&str, String)],
        per_page: u32,
    ) -> Result<Vec<Value>, MerakiFetchError> {
        let (items, stop) = self.get_paged_traced(path, query, per_page).await;
        match stop {
            None => Ok(items),
            Some(stop) => Err(stop.into()),
        }
    }

    /// The paging loop both readers share: the items gathered, and why it stopped if the server did
    /// not say it was finished. `None` is the only complete answer.
    async fn get_paged_traced(
        &mut self,
        path: &str,
        query: &[(&str, String)],
        per_page: u32,
    ) -> (Vec<Value>, Option<Stop>) {
        let mut items: Vec<Value> = Vec::new();
        let stop = self
            .page_through(path, query, per_page, &mut items)
            .await
            .err();
        (items, stop)
    }

    /// Page through `path`, pushing into `items`. `Ok(())` only when the listing ran to its end.
    async fn page_through(
        &mut self,
        path: &str,
        query: &[(&str, String)],
        per_page: u32,
        items: &mut Vec<Value>,
    ) -> Result<(), Stop> {
        let mut url = self.base.join(path).map_err(|e| {
            tracing::debug!(error = %e, path, "invalid meraki path");
            Stop::Malformed
        })?;
        {
            let mut qp = url.query_pairs_mut();
            for (k, v) in query {
                qp.append_pair(k, v);
            }
            qp.append_pair("perPage", &per_page.to_string());
        }

        let mut visited: Vec<reqwest::Url> = Vec::new();
        let mut rate_retries = 0u32;

        loop {
            // Host allow-list on EVERY request (initial URL + each next-link).
            let host = url.host_str().unwrap_or_default();
            if !is_meraki_api_host(host) {
                return Err(Stop::Host);
            }

            self.pace().await;
            let resp = match self.client.get(url.clone()).send().await {
                Ok(r) => r,
                Err(e) => {
                    tracing::debug!(error = %e, "meraki request did not complete");
                    return Err(Stop::Network);
                }
            };
            let status = resp.status();

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                rate_retries += 1;
                if rate_retries > MAX_RATE_LIMIT_RETRIES {
                    tracing::warn!("meraki 429 budget exhausted");
                    return Err(Stop::RateLimited);
                }
                let wait = retry_after(&resp).unwrap_or_else(|| Duration::from_secs(1));
                tracing::warn!(
                    wait_ms = wait.as_millis(),
                    "meraki 429; honoring Retry-After"
                );
                tokio::time::sleep(wait).await;
                continue; // retry the same url
            }
            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
            {
                return Err(Stop::Auth(status.as_u16()));
            }
            if !status.is_success() {
                tracing::debug!(status = status.as_u16(), "meraki non-success");
                return Err(Stop::Status(status.as_u16()));
            }
            rate_retries = 0;

            let next = next_link(&resp);
            let body = resp.text().await.map_err(|e| {
                tracing::debug!(error = %e, "meraki response read failed");
                Stop::Malformed
            })?;
            match serde_json::from_str::<Value>(&body) {
                Ok(Value::Array(arr)) => items.extend(arr),
                Ok(other) => items.push(other),
                Err(e) => {
                    tracing::debug!(error = %e, "meraki json parse failed");
                    return Err(Stop::Malformed);
                }
            }
            visited.push(url);

            match next_page(&visited, next.as_deref()) {
                PageStep::Next(n) => url = n,
                PageStep::Done => return Ok(()),
                PageStep::Stop(stop) => return Err(stop),
            }
        }
    }
}

/// Why a paged read ended before the server said it had no more pages.
///
/// One vocabulary for both readers, because which of these is an error is the *caller's* question:
/// the lenient reader keeps what it has for the last five, the strict one refuses all eight.
/// Nothing here carries request or response text — a Dashboard API error body can quote the request,
/// and the request carries the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// A URL named a host outside the allow-list; the key was not sent.
    Host,
    /// 401/403 — the key is wrong, revoked, or lacks access to this organization.
    Auth(u16),
    /// The path, a next link, or a response body could not be read as what it should be.
    Malformed,
    /// The request did not complete (DNS, connect, TLS, timeout).
    Network,
    /// 429s outlasted [`MAX_RATE_LIMIT_RETRIES`].
    RateLimited,
    /// Any other non-success status.
    Status(u16),
    /// The next link named a page already fetched (ADR-158 B5).
    Cycle,
    /// [`MAX_PAGES`] pages were read and the server still offered another.
    PageCap,
}

impl Stop {
    /// Whether the lenient reader reports this rather than returning what it gathered. These three
    /// were errors before the vocabulary existed, and a collect that swallowed a refused key would
    /// report every device as simply having no data.
    fn fails_a_collect(self) -> bool {
        match self {
            Self::Host | Self::Auth(_) | Self::Malformed => true,
            Self::Network | Self::RateLimited | Self::Status(_) | Self::Cycle | Self::PageCap => {
                false
            }
        }
    }

    /// The lenient reader's error text for the three it reports.
    fn collect_message(self) -> String {
        match self {
            Self::Host => "meraki request host is not allow-listed (refusing to send key)".into(),
            Self::Auth(code) => format!("meraki api auth failed ({code})"),
            Self::Malformed
            | Self::Network
            | Self::RateLimited
            | Self::Status(_)
            | Self::Cycle
            | Self::PageCap => "meraki response could not be read".into(),
        }
    }
}

/// Why a strict inventory read failed (ADR-164).
///
/// Every variant is a fact about the exchange and none quotes it, so core may store one on the
/// organization's row and show it to an operator. That is the reason this is its own type rather
/// than a [`TransportError::Io`] string: a string would have to be hidden, and "sync failed" with
/// no reason is the state this exists to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MerakiFetchError {
    /// The session could not be built: the base URL is not an https allow-listed host, or the key
    /// holds characters a header cannot carry. Nothing was sent.
    #[error("the organization's base URL or API key cannot be used")]
    Config,
    /// A next link named a host outside the allow-list; the key was not sent there.
    #[error("the Meraki API host is not allow-listed")]
    Host,
    /// 401/403.
    #[error("the Dashboard API refused the key ({0})")]
    Auth(u16),
    /// 429s outlasted the retry budget.
    #[error("the Dashboard API kept rate-limiting the request")]
    RateLimited,
    /// Any other non-success status.
    #[error("the Dashboard API answered {0}")]
    Status(u16),
    /// The request did not complete.
    #[error("the Dashboard API could not be reached")]
    Network,
    /// A response could not be read as JSON, or a next link as a URL.
    #[error("the Dashboard API answered something that could not be read")]
    Malformed,
    /// The listing did not run to its end: a repeating next link, or more pages than the cap.
    #[error("the listing did not run to its end")]
    Truncated,
}

impl MerakiFetchError {
    /// The closed token this failure travels as — on the bus, in a collect report, and on the
    /// organization's row (ADR-164 決定 18).
    ///
    /// 🚨 **Spelled exactly as core's `MerakiSyncFailure` spells the same failure**, because core
    /// reads it back with that type's `from_token`, and a token it does not know becomes
    /// `internal` ("Yagra could not read or write its own database") — a wrong sentence on
    /// screen. This crate cannot see that enum, so
    /// `meraki_sync.rs::a_collect_failure_token_is_the_one_the_sync_stores` holds the two
    /// together from the other side.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Auth(_) => "auth",
            Self::RateLimited => "rate_limited",
            Self::Status(_) => "upstream",
            Self::Network => "unreachable",
            // A next link that left the allow-list is an answer we refuse to follow.
            Self::Host | Self::Malformed => "malformed",
            Self::Truncated => "truncated",
        }
    }

    /// Every variant, for the test that pins [`Self::token`] to core's vocabulary.
    pub const ALL: [Self; 8] = [
        Self::Config,
        Self::Host,
        Self::Auth(401),
        Self::RateLimited,
        Self::Status(500),
        Self::Network,
        Self::Malformed,
        Self::Truncated,
    ];
}

impl From<Stop> for MerakiFetchError {
    fn from(stop: Stop) -> Self {
        match stop {
            Stop::Host => Self::Host,
            Stop::Auth(code) => Self::Auth(code),
            Stop::Malformed => Self::Malformed,
            Stop::Network => Self::Network,
            Stop::RateLimited => Self::RateLimited,
            Stop::Status(code) => Self::Status(code),
            Stop::Cycle | Stop::PageCap => Self::Truncated,
        }
    }
}

/// Where paging goes after the pages in `visited`.
#[derive(Debug, PartialEq, Eq)]
enum PageStep {
    /// Fetch this page next.
    Next(reqwest::Url),
    /// The server offered no next link: the listing is complete.
    Done,
    /// Paging ends here although the server offered more.
    Stop(Stop),
}

/// Decide the step after the pages in `visited`, given the response's `rel=next` URL.
///
/// Stops at [`MAX_PAGES`], and — ADR-158 B5 — at a next link naming a page already fetched. A
/// server that answered with its own URL, or a cycle between two pages, used to be followed to the
/// cap and every repeat taken in again: the same devices fifty times over, each one a sample.
///
/// 🚨 Those two stops are **not** [`PageStep::Done`]. They used to be indistinguishable from it
/// (both were `None`), which is harmless to a collect and would be read by the inventory sync as
/// "the organization has exactly these devices" (ADR-164).
fn next_page(visited: &[reqwest::Url], next: Option<&str>) -> PageStep {
    let Some(next) = next else {
        return PageStep::Done;
    };
    let Ok(next) = reqwest::Url::parse(next) else {
        tracing::debug!("invalid meraki next link");
        return PageStep::Stop(Stop::Malformed);
    };
    if visited.contains(&next) {
        tracing::warn!(
            pages = visited.len(),
            "meraki next link names a page already fetched; stopping pagination"
        );
        return PageStep::Stop(Stop::Cycle);
    }
    if visited.len() >= MAX_PAGES {
        tracing::warn!(max = MAX_PAGES, "meraki pagination truncated at page cap");
        return PageStep::Stop(Stop::PageCap);
    }
    PageStep::Next(next)
}

/// Parse a `Retry-After` header value (delta-seconds) into a duration.
fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    let secs: u64 = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(secs.min(60)))
}

/// Extract the `rel="next"` URL from a `Link` header, if present.
fn next_link(resp: &reqwest::Response) -> Option<String> {
    let header = resp.headers().get(reqwest::header::LINK)?.to_str().ok()?;
    parse_next_link(header)
}

/// Pure `Link`-header parse: find the `<url>; rel=next` entry.
fn parse_next_link(header: &str) -> Option<String> {
    for part in header.split(',') {
        let mut segs = part.split(';');
        let url_seg = segs.next()?.trim();
        let is_next = segs.any(|s| {
            let s = s.trim();
            s == "rel=\"next\"" || s == "rel=next"
        });
        if is_next {
            let url = url_seg.trim_start_matches('<').trim_end_matches('>').trim();
            if !url.is_empty() {
                return Some(url.to_owned());
            }
        }
    }
    None
}

// ── Collect (recurring metric tiers) ────────────────────────────────────────────────────────

/// One parsed per-device datum: a serial, a sample, and (for per-uplink metrics) the uplink to
/// record in the interface inventory.
struct DeviceDatum {
    serial: String,
    sample: MerakiSample,
    uplink: Option<MerakiUplink>,
}

/// Run one org-scoped collect for `spec.tier`. See the trait docs for the error contract.
pub(crate) async fn collect(
    spec: &MerakiCollectSpec,
    timeout: Duration,
) -> Result<MerakiCollected, MerakiFetchError> {
    let mut session = Session::new(&spec.base_url, &spec.api_key, spec.target_rps, timeout)
        .map_err(|e| {
            tracing::debug!(error = %e, "meraki collect session refused");
            MerakiFetchError::Config
        })?;
    // The FIRST reason a listing of this collect stopped early. One is enough: what core asks
    // of it is "did the Dashboard answer", and a second stop after the first adds nothing.
    let mut stopped: Option<MerakiFetchError> = None;
    let net_query: Vec<(&str, String)> = spec
        .network_ids
        .iter()
        .map(|n| ("networkIds[]", n.clone()))
        .collect();

    let mut data: Vec<DeviceDatum> = Vec::new();
    match spec.tier {
        MerakiTier::Availability => {
            let path = format!(
                "{API_PREFIX}/organizations/{}/devices/availabilities",
                spec.org_id
            );
            let (items, stop) = session
                .get_paged_reported(&path, &net_query, spec.per_page)
                .await?;
            stopped = stopped.or(stop);
            data.extend(parse_availability(&items));
        }
        MerakiTier::Uplink => {
            let loss_path = format!(
                "{API_PREFIX}/organizations/{}/devices/uplinksLossAndLatency",
                spec.org_id
            );
            let mut q = net_query.clone();
            q.push(("timespan", "300".to_owned()));
            let (items, stop) = session
                .get_paged_reported(&loss_path, &q, spec.per_page)
                .await?;
            stopped = stopped.or(stop);
            data.extend(parse_uplink_loss_latency(&items));

            let status_path = format!(
                "{API_PREFIX}/organizations/{}/appliance/uplink/statuses",
                spec.org_id
            );
            let (items, stop) = session
                .get_paged_reported(&status_path, &net_query, spec.per_page)
                .await?;
            stopped = stopped.or(stop);
            data.extend(parse_uplink_statuses(&items));
        }
        MerakiTier::Traffic => {
            let path = format!(
                "{API_PREFIX}/organizations/{}/summary/top/devices/byUsage",
                spec.org_id
            );
            let mut q = net_query.clone();
            q.push(("timespan", "3600".to_owned()));
            let (items, stop) = session.get_paged_reported(&path, &q, spec.per_page).await?;
            stopped = stopped.or(stop);
            data.extend(parse_traffic(&items));
        }
        // Inventory reconciliation is operator-initiated (control-plane enumerate), not a recurring
        // metric collect — nothing to gather here.
        MerakiTier::Inventory => {}
    }
    Ok(MerakiCollected {
        observations: fold(data),
        stopped,
    })
}

/// Fold per-device data into observations, deduping uplinks by ifindex. `BTreeMap` gives a stable
/// serial order (nice for tests / deterministic fan-out).
fn fold(data: Vec<DeviceDatum>) -> Vec<MerakiObservation> {
    let mut map: BTreeMap<String, MerakiObservation> = BTreeMap::new();
    for d in data {
        let obs = map
            .entry(d.serial.clone())
            .or_insert_with(|| MerakiObservation {
                serial: d.serial.clone(),
                samples: Vec::new(),
                uplinks: Vec::new(),
            });
        obs.samples.push(d.sample);
        if let Some(u) = d.uplink {
            if !obs.uplinks.iter().any(|x| x.ifindex == u.ifindex) {
                obs.uplinks.push(u);
            }
        }
    }
    map.into_values().collect()
}

fn parse_availability(items: &[Value]) -> Vec<DeviceDatum> {
    items
        .iter()
        .filter_map(|it| {
            let serial = it.get("serial")?.as_str()?.to_owned();
            let status = it.get("status").and_then(Value::as_str).unwrap_or("");
            let up = MerakiAvailability::from_status(status).is_up();
            Some(DeviceDatum {
                serial,
                sample: MerakiSample {
                    metric: METRIC_MERAKI_DEVICE_UP.to_owned(),
                    ifindex: None,
                    value: if up { 1.0 } else { 0.0 },
                },
                uplink: None,
            })
        })
        .collect()
}

fn parse_uplink_loss_latency(items: &[Value]) -> Vec<DeviceDatum> {
    let mut out = Vec::new();
    for it in items {
        let (Some(serial), Some(uplink)) = (
            it.get("serial").and_then(Value::as_str),
            it.get("uplink").and_then(Value::as_str),
        ) else {
            continue;
        };
        let Some(ifindex) = uplink_ifindex(uplink) else {
            continue; // unknown uplink → skip rather than invent a label (cardinality)
        };
        // Take the most recent timeSeries point with non-null values.
        let series = it.get("timeSeries").and_then(Value::as_array);
        let (loss, latency) = series
            .map(|pts| {
                let mut loss = None;
                let mut latency = None;
                for p in pts {
                    if let Some(l) = p.get("lossPercent").and_then(Value::as_f64) {
                        loss = Some(l);
                    }
                    if let Some(l) = p.get("latencyMs").and_then(Value::as_f64) {
                        latency = Some(l);
                    }
                }
                (loss, latency)
            })
            .unwrap_or((None, None));

        let uplink_meta = MerakiUplink {
            ifindex,
            name: uplink_name(ifindex).unwrap_or(uplink).to_owned(),
        };
        if let Some(loss) = loss {
            out.push(DeviceDatum {
                serial: serial.to_owned(),
                sample: MerakiSample {
                    metric: METRIC_MERAKI_UPLINK_LOSS_PCT.to_owned(),
                    ifindex: Some(ifindex),
                    value: loss,
                },
                uplink: Some(uplink_meta.clone()),
            });
        }
        if let Some(latency) = latency {
            out.push(DeviceDatum {
                serial: serial.to_owned(),
                sample: MerakiSample {
                    metric: METRIC_MERAKI_UPLINK_LATENCY_MS.to_owned(),
                    ifindex: Some(ifindex),
                    value: latency,
                },
                uplink: Some(uplink_meta),
            });
        }
    }
    out
}

fn parse_uplink_statuses(items: &[Value]) -> Vec<DeviceDatum> {
    let mut out = Vec::new();
    for it in items {
        let Some(serial) = it.get("serial").and_then(Value::as_str) else {
            continue;
        };
        let Some(uplinks) = it.get("uplinks").and_then(Value::as_array) else {
            continue;
        };
        for u in uplinks {
            let iface = u
                .get("interface")
                .and_then(Value::as_str)
                .or_else(|| u.get("uplink").and_then(Value::as_str))
                .unwrap_or("");
            let Some(ifindex) = uplink_ifindex(iface) else {
                continue;
            };
            let status = u.get("status").and_then(Value::as_str).unwrap_or("");
            out.push(DeviceDatum {
                serial: serial.to_owned(),
                sample: MerakiSample {
                    metric: METRIC_MERAKI_UPLINK_STATUS.to_owned(),
                    ifindex: Some(ifindex),
                    value: uplink_status_value(status),
                },
                uplink: Some(MerakiUplink {
                    ifindex,
                    name: uplink_name(ifindex).unwrap_or(iface).to_owned(),
                }),
            });
        }
    }
    out
}

fn parse_traffic(items: &[Value]) -> Vec<DeviceDatum> {
    let mut out = Vec::new();
    for it in items {
        let Some(serial) = it.get("serial").and_then(Value::as_str) else {
            continue;
        };
        if let Some(sent) = it.pointer("/usage/sent").and_then(Value::as_f64) {
            out.push(DeviceDatum {
                serial: serial.to_owned(),
                sample: MerakiSample {
                    metric: METRIC_MERAKI_USAGE_SENT_KB.to_owned(),
                    ifindex: None,
                    value: sent,
                },
                uplink: None,
            });
        }
        if let Some(recv) = it.pointer("/usage/recv").and_then(Value::as_f64) {
            out.push(DeviceDatum {
                serial: serial.to_owned(),
                sample: MerakiSample {
                    metric: METRIC_MERAKI_USAGE_RECV_KB.to_owned(),
                    ifindex: None,
                    value: recv,
                },
                uplink: None,
            });
        }
        if let Some(clients) = it.pointer("/clients/counts/total").and_then(Value::as_f64) {
            out.push(DeviceDatum {
                serial: serial.to_owned(),
                sample: MerakiSample {
                    metric: METRIC_MERAKI_CLIENT_COUNT.to_owned(),
                    ifindex: None,
                    value: clients,
                },
                uplink: None,
            });
        }
    }
    out
}

// ── Control-plane (adding an organization) ──────────────────────────────────────────────────

/// List the organizations the API key can access (`GET /organizations`). Read-only.
pub async fn list_organizations(
    base_url: &str,
    api_key: &str,
    timeout: Duration,
) -> Result<Vec<MerakiOrgInfo>, TransportError> {
    let mut s = Session::new(base_url, api_key, 2.0, timeout)?;
    let items = s
        .get_paged(&format!("{API_PREFIX}/organizations"), &[], 1000)
        .await?;
    Ok(items
        .iter()
        .filter_map(|it| {
            Some(MerakiOrgInfo {
                id: it.get("id")?.as_str().map(str::to_owned).or_else(|| {
                    // organizationId may serialize as a JSON number in some responses.
                    it.get("id").and_then(Value::as_i64).map(|n| n.to_string())
                })?,
                name: it
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                url: it.get("url").and_then(Value::as_str).map(str::to_owned),
            })
        })
        .collect())
}

// ── Inventory (core's periodic sync, ADR-164) ───────────────────────────────────────────────

/// What the Dashboard API says about whether a device is reachable *from the Meraki cloud*.
///
/// The one reading of the `status` word, shared by the availability collect and the inventory sync,
/// so "this device has been seen online" and `meraki_device_up = 1` cannot come to mean different
/// things.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MerakiAvailability {
    /// `online`.
    Online,
    /// `alerting` — up, with something Meraki wants looked at.
    Alerting,
    /// `offline`.
    Offline,
    /// `dormant` — has not checked in for a long time (or never has).
    Dormant,
    /// A word this build does not know. Read as not up: claiming a device is reachable on the
    /// strength of a word nobody has seen is the wrong way to be wrong.
    Other,
}

impl MerakiAvailability {
    /// Read the API's status word (case-insensitive).
    #[must_use]
    pub fn from_status(status: &str) -> Self {
        match status.to_ascii_lowercase().as_str() {
            "online" => Self::Online,
            "alerting" => Self::Alerting,
            "offline" => Self::Offline,
            "dormant" => Self::Dormant,
            _ => Self::Other,
        }
    }

    /// Whether the device is up. `alerting` is up: it is answering, which is the question.
    #[must_use]
    pub fn is_up(self) -> bool {
        match self {
            Self::Online | Self::Alerting => true,
            Self::Offline | Self::Dormant | Self::Other => false,
        }
    }
}

/// One device in a complete inventory read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerakiInventoryDevice {
    /// What `GET …/devices` said.
    pub info: MerakiDeviceInfo,
    /// What `GET …/devices/availabilities` said, or `None` when that listing did not name the
    /// serial — which is not the same as the device being down.
    pub availability: Option<MerakiAvailability>,
}

/// An organization's networks and devices, read **to the end** (ADR-164 決定 2).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MerakiInventory {
    /// Every network in the organization.
    pub networks: Vec<MerakiNetworkInfo>,
    /// Every device in the organization, whichever network it is in.
    pub devices: Vec<MerakiInventoryDevice>,
}

/// Read an organization's whole inventory: networks, devices, and each device's availability.
/// Three paged GETs on one session, paced to `target_rps`. Read-only.
///
/// 🚨 **All three listings complete, or this is an error.** The caller marks a device missing when
/// it is absent from the answer, so an answer that is merely short must never be returned — see
/// [`Session::get_paged_strict`]. Deliberately **not** narrowed by network: core has to know about
/// the devices in a network nobody is watching in order to say that they are there.
///
/// `timeout` bounds one request. Bounding the whole read is the caller's job.
pub async fn fetch_inventory(
    base_url: &str,
    api_key: &str,
    org_id: &str,
    target_rps: f64,
    timeout: Duration,
) -> Result<MerakiInventory, MerakiFetchError> {
    let mut s = Session::new(base_url, api_key, target_rps, timeout).map_err(|e| {
        tracing::debug!(error = %e, "meraki inventory session refused");
        MerakiFetchError::Config
    })?;
    let org = format!("{API_PREFIX}/organizations/{org_id}");

    let networks = s
        .get_paged_strict(&format!("{org}/networks"), &[], 1000)
        .await?;
    let devices = s
        .get_paged_strict(&format!("{org}/devices"), &[], 1000)
        .await?;
    let availabilities = s
        .get_paged_strict(&format!("{org}/devices/availabilities"), &[], 1000)
        .await?;

    Ok(assemble_inventory(&networks, &devices, &availabilities))
}

/// Join the three listings. Pure, so the join is tested without a server.
fn assemble_inventory(
    networks: &[Value],
    devices: &[Value],
    availabilities: &[Value],
) -> MerakiInventory {
    let status: BTreeMap<&str, MerakiAvailability> = availabilities
        .iter()
        .filter_map(|it| {
            let serial = it.get("serial")?.as_str()?;
            let word = it.get("status").and_then(Value::as_str).unwrap_or("");
            Some((serial, MerakiAvailability::from_status(word)))
        })
        .collect();
    MerakiInventory {
        networks: networks.iter().filter_map(parse_network_info).collect(),
        devices: devices
            .iter()
            .filter_map(parse_device_info)
            .map(|info| MerakiInventoryDevice {
                availability: status.get(info.serial.as_str()).copied(),
                info,
            })
            .collect(),
    }
}

fn parse_network_info(it: &Value) -> Option<MerakiNetworkInfo> {
    Some(MerakiNetworkInfo {
        id: it.get("id")?.as_str()?.to_owned(),
        name: it
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
    })
}

fn parse_device_info(it: &Value) -> Option<MerakiDeviceInfo> {
    Some(MerakiDeviceInfo {
        serial: it.get("serial")?.as_str()?.to_owned(),
        name: it
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        model: it.get("model").and_then(Value::as_str).map(str::to_owned),
        product_type: it
            .get("productType")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        network_id: it
            .get("networkId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        lan_ip: it
            .get("lanIp")
            .and_then(Value::as_str)
            .or_else(|| it.get("wan1Ip").and_then(Value::as_str))
            .map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn session_new_refuses_non_meraki_host() {
        // The key-exfiltration guard: a non-Meraki base is rejected before any request.
        let err = Session::new("https://evil.example.com", "k", 2.0, Duration::from_secs(5));
        assert!(matches!(err, Err(TransportError::Io(_))));
        // And plain http is rejected.
        let err = Session::new("http://api.meraki.com", "k", 2.0, Duration::from_secs(5));
        assert!(matches!(err, Err(TransportError::Io(_))));
        // The canonical host is accepted.
        assert!(Session::new("https://api.meraki.com", "k", 2.0, Duration::from_secs(5)).is_ok());
    }

    #[test]
    fn parse_next_link_finds_rel_next() {
        let h =
            "<https://api.meraki.com/api/v1/organizations/1/devices?startingAfter=Q2>; rel=next";
        assert_eq!(
            parse_next_link(h).as_deref(),
            Some("https://api.meraki.com/api/v1/organizations/1/devices?startingAfter=Q2")
        );
        // rel=first only → no next.
        assert_eq!(
            parse_next_link("<https://api.meraki.com/x>; rel=first"),
            None
        );
        // multiple links.
        let multi =
            "<https://api.meraki.com/prev>; rel=prev, <https://api.meraki.com/next>; rel=\"next\"";
        assert_eq!(
            parse_next_link(multi).as_deref(),
            Some("https://api.meraki.com/next")
        );
    }

    fn page(after: &str) -> reqwest::Url {
        reqwest::Url::parse(&format!(
            "https://api.meraki.com/api/v1/organizations/1/devices?perPage=1000&startingAfter={after}"
        ))
        .unwrap()
    }

    /// ADR-158 B5. A next link naming the page just fetched ends the paging — it used to be
    /// followed to the fifty-page cap, taking in the same items every time.
    #[test]
    fn a_next_link_that_repeats_the_current_page_ends_the_paging() {
        let first = page("Q2-A");
        assert_eq!(
            next_page(std::slice::from_ref(&first), Some(first.as_str())),
            PageStep::Stop(Stop::Cycle)
        );
    }

    #[test]
    fn a_next_link_back_to_an_earlier_page_ends_the_paging() {
        let (a, b) = (page("Q2-A"), page("Q2-B"));
        assert_eq!(
            next_page(&[a.clone(), b], Some(a.as_str())),
            PageStep::Stop(Stop::Cycle)
        );
    }

    /// The ordinary case still pages, and the cap still stops it.
    #[test]
    fn a_new_next_link_is_followed_until_the_page_cap() {
        let fresh = page("Q2-NEW");
        assert_eq!(
            next_page(&[page("Q2-A")], Some(fresh.as_str())),
            PageStep::Next(fresh.clone())
        );

        let full: Vec<reqwest::Url> = (0..MAX_PAGES).map(|i| page(&i.to_string())).collect();
        assert_eq!(
            next_page(&full, Some(fresh.as_str())),
            PageStep::Stop(Stop::PageCap)
        );
        assert_eq!(
            next_page(&full[..MAX_PAGES - 1], Some(fresh.as_str())),
            PageStep::Next(fresh)
        );
        assert_eq!(
            next_page(&[], Some("not a url")),
            PageStep::Stop(Stop::Malformed)
        );
    }

    /// ADR-164 決定 2, the distinction the inventory sync rests on. "The server offered no next
    /// page" is the **only** complete ending. A cycle and the page cap used to be spelled the same
    /// way (`None`), so nothing downstream could tell a finished listing from an abandoned one —
    /// and the sync marks every device the listing does not contain as missing.
    #[test]
    fn only_the_absence_of_a_next_link_is_a_complete_listing() {
        let (a, fresh) = (page("Q2-A"), page("Q2-NEW"));
        assert_eq!(next_page(std::slice::from_ref(&a), None), PageStep::Done);

        let full: Vec<reqwest::Url> = (0..MAX_PAGES).map(|i| page(&i.to_string())).collect();
        for abandoned in [
            next_page(std::slice::from_ref(&a), Some(a.as_str())),
            next_page(&full, Some(fresh.as_str())),
            next_page(&[], Some("not a url")),
        ] {
            assert_ne!(abandoned, PageStep::Done);
            assert!(matches!(abandoned, PageStep::Stop(_)), "{abandoned:?}");
        }
    }

    /// Both directions of the split between the two readers. The lenient one keeps its old
    /// contract exactly — a refused key, an off-list host and an unreadable body were always
    /// errors, everything else returned what had been gathered — and the strict one refuses all
    /// eight, because it may not return a short answer at all.
    #[test]
    fn every_stop_fails_a_strict_read_and_three_fail_a_collect() {
        let all = [
            Stop::Host,
            Stop::Auth(401),
            Stop::Malformed,
            Stop::Network,
            Stop::RateLimited,
            Stop::Status(503),
            Stop::Cycle,
            Stop::PageCap,
        ];
        let failing: Vec<Stop> = all.into_iter().filter(|s| s.fails_a_collect()).collect();
        assert_eq!(failing, [Stop::Host, Stop::Auth(401), Stop::Malformed]);

        // The strict reader's error says which, and never "complete".
        assert_eq!(
            MerakiFetchError::from(Stop::Auth(403)),
            MerakiFetchError::Auth(403)
        );
        assert_eq!(
            MerakiFetchError::from(Stop::Status(503)),
            MerakiFetchError::Status(503)
        );
        assert_eq!(
            MerakiFetchError::from(Stop::Cycle),
            MerakiFetchError::Truncated
        );
        assert_eq!(
            MerakiFetchError::from(Stop::PageCap),
            MerakiFetchError::Truncated
        );
        assert_eq!(
            MerakiFetchError::from(Stop::RateLimited),
            MerakiFetchError::RateLimited
        );
        assert_eq!(
            MerakiFetchError::from(Stop::Network),
            MerakiFetchError::Network
        );

        // The collect's two historical messages are unchanged.
        assert_eq!(
            Stop::Auth(401).collect_message(),
            "meraki api auth failed (401)"
        );
        assert!(Stop::Host
            .collect_message()
            .contains("refusing to send key"));
    }

    /// A fetch error is stored on the organization's row and shown to an operator, so none may
    /// quote the exchange. The variants carry a status code at most; this pins that the rendered
    /// text does too.
    #[test]
    fn a_fetch_error_renders_without_a_url_or_a_key() {
        for e in [
            MerakiFetchError::Config,
            MerakiFetchError::Host,
            MerakiFetchError::Auth(401),
            MerakiFetchError::RateLimited,
            MerakiFetchError::Status(502),
            MerakiFetchError::Network,
            MerakiFetchError::Malformed,
            MerakiFetchError::Truncated,
        ] {
            let text = e.to_string();
            assert!(!text.contains("http"), "{text}");
            assert!(!text.contains("Bearer"), "{text}");
        }
    }

    #[test]
    fn the_status_word_is_read_once_for_the_collect_and_the_inventory() {
        for (word, up) in [
            ("online", true),
            ("Online", true),
            ("alerting", true),
            ("offline", false),
            ("dormant", false),
            ("", false),
            ("rebooting", false),
        ] {
            assert_eq!(
                MerakiAvailability::from_status(word).is_up(),
                up,
                "{word:?}"
            );
        }
        assert_eq!(
            MerakiAvailability::from_status("dormant"),
            MerakiAvailability::Dormant
        );
        assert_eq!(
            MerakiAvailability::from_status("rebooting"),
            MerakiAvailability::Other
        );
    }

    #[test]
    fn the_inventory_joins_availability_onto_devices_by_serial() {
        let networks = vec![json!({"id": "N_1", "name": "HQ"}), json!({"name": "no id"})];
        let devices = vec![
            json!({"serial": "Q2-A", "name": "edge", "productType": "appliance", "networkId": "N_1", "lanIp": "10.0.0.1"}),
            json!({"serial": "Q2-B", "productType": "switch", "networkId": "N_1"}),
            json!({"serial": "Q2-C", "productType": "wireless", "networkId": "N_2"}),
            json!({"name": "no serial"}),
        ];
        let availabilities = vec![
            json!({"serial": "Q2-A", "status": "online"}),
            json!({"serial": "Q2-B", "status": "dormant"}),
            // Q2-C is absent from the listing; a serial nothing else names is ignored.
            json!({"serial": "Q2-Z", "status": "online"}),
        ];
        let inv = assemble_inventory(&networks, &devices, &availabilities);
        assert_eq!(inv.networks.len(), 1);
        let got: Vec<(&str, Option<MerakiAvailability>)> = inv
            .devices
            .iter()
            .map(|d| (d.info.serial.as_str(), d.availability))
            .collect();
        assert_eq!(
            got,
            [
                ("Q2-A", Some(MerakiAvailability::Online)),
                ("Q2-B", Some(MerakiAvailability::Dormant)),
                // Not named by the availability listing: unknown, which is not "down".
                ("Q2-C", None),
            ]
        );
    }

    #[test]
    fn availability_maps_status_to_up() {
        let items = vec![
            json!({"serial": "Q2-A", "status": "online"}),
            json!({"serial": "Q2-B", "status": "offline"}),
            json!({"serial": "Q2-C", "status": "alerting"}),
            json!({"serial": "Q2-D", "status": "dormant"}),
        ];
        let obs = fold(parse_availability(&items));
        let up = |s: &str| {
            obs.iter()
                .find(|o| o.serial == s)
                .and_then(|o| o.samples.first())
                .map(|x| x.value)
        };
        assert_eq!(up("Q2-A"), Some(1.0));
        assert_eq!(up("Q2-B"), Some(0.0));
        assert_eq!(up("Q2-C"), Some(1.0));
        assert_eq!(up("Q2-D"), Some(0.0));
    }

    #[test]
    fn uplink_loss_latency_uses_latest_point_and_synthetic_ifindex() {
        let items = vec![json!({
            "serial": "Q2-A",
            "uplink": "wan2",
            "timeSeries": [
                {"ts": "t0", "lossPercent": 0.0, "latencyMs": 10.0},
                {"ts": "t1", "lossPercent": 2.5, "latencyMs": 22.0}
            ]
        })];
        let obs = fold(parse_uplink_loss_latency(&items));
        assert_eq!(obs.len(), 1);
        let o = &obs[0];
        // wan2 → synthetic ifindex 2, with a named uplink in the inventory.
        assert_eq!(
            o.uplinks,
            vec![MerakiUplink {
                ifindex: 2,
                name: "WAN2".into()
            }]
        );
        let loss = o
            .samples
            .iter()
            .find(|s| s.metric == METRIC_MERAKI_UPLINK_LOSS_PCT)
            .unwrap();
        assert_eq!(loss.value, 2.5);
        assert_eq!(loss.ifindex, Some(2));
        let lat = o
            .samples
            .iter()
            .find(|s| s.metric == METRIC_MERAKI_UPLINK_LATENCY_MS)
            .unwrap();
        assert_eq!(lat.value, 22.0);
    }

    #[test]
    fn unknown_uplink_is_skipped() {
        let items = vec![json!({
            "serial": "Q2-A",
            "uplink": "eth7",
            "timeSeries": [{"lossPercent": 1.0, "latencyMs": 5.0}]
        })];
        assert!(parse_uplink_loss_latency(&items).is_empty());
    }

    #[test]
    fn traffic_maps_usage_and_clients() {
        let items = vec![json!({
            "serial": "Q2-A",
            "usage": {"sent": 100.0, "recv": 250.0, "total": 350.0},
            "clients": {"counts": {"total": 12}}
        })];
        let obs = fold(parse_traffic(&items));
        let get = |m: &str| {
            obs[0]
                .samples
                .iter()
                .find(|s| s.metric == m)
                .map(|s| s.value)
        };
        assert_eq!(get(METRIC_MERAKI_USAGE_SENT_KB), Some(100.0));
        assert_eq!(get(METRIC_MERAKI_USAGE_RECV_KB), Some(250.0));
        assert_eq!(get(METRIC_MERAKI_CLIENT_COUNT), Some(12.0));
    }

    #[test]
    fn device_info_parses_and_tolerates_missing_fields() {
        let d = parse_device_info(&json!({
            "serial": "Q2-A", "name": "edge-fw", "model": "MX67",
            "productType": "appliance", "networkId": "N_1", "lanIp": "10.0.0.1"
        }))
        .unwrap();
        assert_eq!(d.serial, "Q2-A");
        assert_eq!(d.product_type, "appliance");
        assert_eq!(d.lan_ip.as_deref(), Some("10.0.0.1"));
        // A row without a serial is not a usable device.
        assert!(parse_device_info(&json!({"name": "x"})).is_none());
    }

    // ── What a collect reports (ADR-164 決定 18) ─────────────────────────────────────────────

    fn one_observation() -> MerakiObservation {
        MerakiObservation {
            serial: "Q2-A".into(),
            samples: Vec::new(),
            uplinks: Vec::new(),
        }
    }

    /// A collect has failed when it stopped early **and** brought nothing back. Both halves
    /// matter: a partial answer means the Dashboard did answer, and a complete answer that lists
    /// no device is an empty organization, not an outage.
    #[test]
    fn a_collect_that_stopped_with_nothing_has_failed_and_a_partial_one_has_not() {
        let nothing = MerakiCollected {
            observations: Vec::new(),
            stopped: Some(MerakiFetchError::Network),
        };
        assert_eq!(
            nothing.failure(),
            Some(MerakiFetchError::Network),
            "an outage read as a collect that simply found no devices"
        );

        let partial = MerakiCollected {
            observations: vec![one_observation()],
            stopped: Some(MerakiFetchError::RateLimited),
        };
        assert_eq!(
            partial.failure(),
            None,
            "pages 1 and 2 arrived: the Dashboard answered, and that is what the alert asks"
        );

        let empty_organization = MerakiCollected::default();
        assert_eq!(empty_organization.failure(), None);
    }

    /// The five stops the lenient reader survives each become a reason — none is dropped, which is
    /// what used to happen to all five.
    #[test]
    fn every_stop_the_lenient_reader_survives_has_a_reason_to_report() {
        for stop in [
            Stop::Network,
            Stop::RateLimited,
            Stop::Status(503),
            Stop::Cycle,
            Stop::PageCap,
        ] {
            assert!(
                !stop.fails_a_collect(),
                "{stop:?} is one the reader reports as Err"
            );
            let why = MerakiFetchError::from(stop);
            assert!(!why.token().is_empty(), "{stop:?} has no token");
        }
    }

    /// `ALL` is what core's test walks to pin [`MerakiFetchError::token`] to its own vocabulary, so
    /// a variant missing from it is a variant nothing checks. The match has no wildcard: a ninth
    /// variant stops compiling here until it is listed.
    #[test]
    fn the_list_of_fetch_errors_names_every_variant() {
        let listed = |e: MerakiFetchError| {
            MerakiFetchError::ALL
                .iter()
                .any(|a| std::mem::discriminant(a) == std::mem::discriminant(&e))
        };
        for e in MerakiFetchError::ALL {
            let covered = match e {
                MerakiFetchError::Config
                | MerakiFetchError::Host
                | MerakiFetchError::Auth(_)
                | MerakiFetchError::RateLimited
                | MerakiFetchError::Status(_)
                | MerakiFetchError::Network
                | MerakiFetchError::Malformed
                | MerakiFetchError::Truncated => listed(e),
            };
            assert!(covered, "{e:?}");
        }
        assert_eq!(MerakiFetchError::ALL.len(), 8);
    }
}
