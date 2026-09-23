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
//!   — is checked with [`is_meraki_api_host`] before it is issued, so no URL a server hands back can
//!   send the bearer key off-host (credential-exfiltration guard). The one other place a request can
//!   physically go is a lab build's [`MerakiWireOrigin`] (ADR-166): chosen by whoever runs that
//!   box, never by a URL, and a release build has no way to set one.
//! * **Paced + Retry-After.** Requests are spaced to `target_rps` (well under the org cap, headroom
//!   for the customer), and a 429 is obeyed via its `Retry-After` header — retried up to
//!   `MAX_RATE_LIMIT_RETRIES` times in a row, then given up on.
//! * **Bounded.** Pagination is capped; a transient network/5xx failure returns the partial results
//!   collected so far rather than hammering.

use crate::{
    MerakiCollectSpec, MerakiCollected, MerakiObservation, MerakiPort, MerakiRadio, MerakiSample,
    MerakiUplink, TransportError,
};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};
use yagra_common::{assign_radio_slots, WlanBand, WlanRadioObservation};
use yagra_common::{
    is_meraki_api_host, switch_port_ifindex, switch_port_oper_status, switch_port_speed_bps,
    uplink_ifindex, uplink_name, Duplex, MerakiHaRole, MerakiListing, MerakiTier,
    MerakiUplinkStatus, METRIC_MERAKI_DEVICE_UP, METRIC_MERAKI_PORT_IN_BPS,
    METRIC_MERAKI_PORT_OUT_BPS, METRIC_MERAKI_UPLINK_FAILED, METRIC_MERAKI_UPLINK_LATENCY_MS,
    METRIC_MERAKI_UPLINK_LOSS_PCT, METRIC_MERAKI_UPLINK_RECV_BPS, METRIC_MERAKI_UPLINK_SENT_BPS,
    METRIC_MERAKI_UPLINK_STATUS, METRIC_MERAKI_VPN_HUBS_REACHABLE,
    METRIC_MERAKI_VPN_HUBS_UNREACHABLE, METRIC_MERAKI_VPN_HUBS_UNREACHABLE_PCT,
    METRIC_MERAKI_VPN_SPOKES_UNREACHABLE, METRIC_WLAN_AP_CLIENT_COUNT, METRIC_WLAN_AP_SSID_COUNT,
};

/// Dashboard API v1 path prefix (appended to the org's `base_url`).
const API_PREFIX: &str = "/api/v1";
/// Hard cap on pages per endpoint (bounded-pagination safeguard). The switch-port listings have
/// their own ([`SWITCH_PORT_MAX_PAGES`]) and a clock besides ([`SWITCH_PORTS_BUDGET`]).
const MAX_PAGES: usize = 50;
/// The slowest a Meraki session paces, whatever it is asked for: one request every ten seconds.
///
/// Public because core halves an organization's `target_rps` between its two collect lanes
/// (ADR-169 決定 4) and has to know where the halving stops — below twice this, the two lanes
/// together send more than the organization's setting.
pub const MERAKI_MIN_RPS: f64 = 0.1;
/// Hard cap on consecutive 429/Retry-After waits before giving up on an endpoint.
const MAX_RATE_LIMIT_RETRIES: u32 = 6;

fn io(msg: impl Into<String>) -> TransportError {
    TransportError::Io(msg.into())
}

// ── Where the requests physically go (ADR-166) ──────────────────────────────────────────────

/// Where this process's Dashboard API requests are **physically** sent, when that is not the host
/// each URL names. A lab points it at a recorded Dashboard; nothing else sets one.
///
/// Every URL is still built from the organization's stored `base_url` and still checked against
/// [`is_meraki_api_host`] — on the first request and on every `Link: rel=next` — exactly as when no
/// origin is set. Only the scheme, host and port of the request actually sent are replaced, at the
/// one place a request leaves this module (`Session::page_through`). So the allow-list, the https
/// rule on the stored URL and the region picker keep meaning what they meant.
///
/// 🚨 **A release build has no way to set one.** The only reader of the environment is
/// `from_env`, which exists only under the `lab-meraki-mock` feature, and only the lab build script
/// enables it; every other caller passes `None`. The type itself is not gated so the paths it
/// changes are the paths every test run covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerakiWireOrigin(reqwest::Url);

impl MerakiWireOrigin {
    /// Parse an origin: `http` or `https`, a host, an optional port — and nothing else.
    ///
    /// A path, query, fragment or user name is refused rather than dropped, so a value that meant
    /// something else cannot quietly send requests somewhere half-intended. The error does not
    /// repeat the value: a URL can carry credentials in its user-info.
    pub fn parse(value: &str) -> Result<Self, TransportError> {
        let refused = || io("meraki wire origin must be a bare http(s)://host[:port]");
        let url = reqwest::Url::parse(value.trim()).map_err(|_| refused())?;
        let bare = matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some_and(|h| !h.is_empty())
            && url.username().is_empty()
            && url.password().is_none()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none();
        if bare {
            Ok(Self(url))
        } else {
            Err(refused())
        }
    }

    /// Read a setting: unset or blank means "no origin", anything else must [`parse`](Self::parse).
    pub fn from_setting(value: Option<&str>) -> Result<Option<Self>, TransportError> {
        match value.map(str::trim) {
            None | Some("") => Ok(None),
            Some(v) => Self::parse(v).map(Some),
        }
    }

    /// `logical` as it goes on the wire: the same path and query, sent to this origin. The query is
    /// copied as already encoded, never rebuilt, so `networkIds%5B%5D` arrives exactly as the real
    /// Dashboard would receive it.
    fn wire(&self, logical: &reqwest::Url) -> reqwest::Url {
        let mut sent = self.0.clone();
        sent.set_path(logical.path());
        sent.set_query(logical.query());
        sent
    }
}

impl std::fmt::Display for MerakiWireOrigin {
    /// The origin only — scheme, host and port — which is all [`parse`](Self::parse) admits.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.origin().ascii_serialization())
    }
}

#[cfg(feature = "lab-meraki-mock")]
impl MerakiWireOrigin {
    /// The variable a lab build reads. Named only inside the feature, so a binary built without it
    /// does not even carry the name — which is how "a release build cannot set one" is checked.
    const ENV: &'static str = "YAGRA_MERAKI_MOCK_URL";

    /// Read the origin from the environment, once, at startup (ADR-166).
    ///
    /// Unset or blank is `None`. A value that is set and malformed is an **error**: the binary
    /// refuses to start rather than send an organization's key somewhere nobody chose. A set value
    /// is announced with one WARN naming the origin, so a box running this way says so in its log.
    pub fn from_env() -> Result<Option<Self>, TransportError> {
        let value = match std::env::var(Self::ENV) {
            Ok(v) => Some(v),
            Err(std::env::VarError::NotPresent) => None,
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(io(format!("{} is not valid UTF-8", Self::ENV)));
            }
        };
        let origin =
            Self::from_setting(value.as_deref()).map_err(|e| io(format!("{}: {e}", Self::ENV)))?;
        if let Some(o) = &origin {
            tracing::warn!(
                origin = %o,
                "lab build: Meraki Dashboard API requests are sent to this origin instead of the \
                 host each URL names (ADR-166)"
            );
        }
        Ok(origin)
    }
}

// ── Control-plane result types (core: adding an organization, and the inventory sync) ───────

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
    /// `lanIp`, when the device reports one. Never pinged, and not only shown: core matches it
    /// against the folders' IP ranges, and an imported node takes it as its address (ADR-164 決定
    /// 14).
    ///
    /// ⚠️ **An MX reports none** — measured on 686 of 686: it carries `wan1Ip`/`wan2Ip` instead.
    /// Those are not read: a WAN address is in nobody's IP ranges and is often dynamic, and it filed
    /// sites into another site's folder. Core takes an MX's address from its VLANs instead
    /// ([`fetch_network_lans`], ADR-164 決定 28).
    pub lan_ip: Option<String>,
}

// ── Session: one per collect / control call ─────────────────────────────────────────────────

/// A Meraki API session: one reqwest client reused across all of a call's pages (keep-alive
/// amortizes the many sequential GETs — unlike `probe_http`'s per-request client), plus a request
/// pacer. Constructing it validates the base host against the allow-list up front — whether or not
/// a lab build has given it a [`MerakiWireOrigin`] to send to instead.
struct Session {
    client: reqwest::Client,
    base: reqwest::Url,
    wire: Option<MerakiWireOrigin>,
    min_interval: Duration,
    last: Option<Instant>,
    /// How many pages one listing may run to. [`MAX_PAGES`] unless a collect says otherwise.
    max_pages: usize,
    /// When no further request may be sent, if the collect set one (ADR-167: the switch-port tier
    /// must end inside the organization's collect lease).
    deadline: Option<Instant>,
}

impl Session {
    fn new(
        base_url: &str,
        api_key: &str,
        target_rps: f64,
        timeout: Duration,
        wire: Option<&MerakiWireOrigin>,
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

        let rps = target_rps.max(MERAKI_MIN_RPS);
        Ok(Self {
            client,
            base,
            wire: wire.cloned(),
            min_interval: Duration::from_secs_f64(1.0 / rps),
            last: None,
            max_pages: MAX_PAGES,
            deadline: None,
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
        paging: Paging,
        shape: Shape,
    ) -> Result<Vec<Value>, TransportError> {
        let (items, stop) = self.get_paged_traced(path, query, paging, shape).await;
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
        paging: Paging,
        shape: Shape,
    ) -> Result<(Vec<Value>, Option<MerakiFetchError>), MerakiFetchError> {
        let (items, stop) = self.get_paged_traced(path, query, paging, shape).await;
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
        paging: Paging,
        shape: Shape,
    ) -> Result<Vec<Value>, MerakiFetchError> {
        let (items, stop) = self.get_paged_traced(path, query, paging, shape).await;
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
        paging: Paging,
        shape: Shape,
    ) -> (Vec<Value>, Option<Stop>) {
        let mut items: Vec<Value> = Vec::new();
        let stop = self
            .page_through(path, query, paging, shape, &mut items)
            .await
            .err();
        (items, stop)
    }

    /// Page through `path`, pushing into `items`. `Ok(())` only when the listing ran to its end.
    async fn page_through(
        &mut self,
        path: &str,
        query: &[(&str, String)],
        paging: Paging,
        shape: Shape,
        items: &mut Vec<Value>,
    ) -> Result<(), Stop> {
        let mut url = self.base.join(path).map_err(|e| {
            tracing::debug!(error = %e, path, "invalid meraki path");
            Stop::Malformed
        })?;
        // Only when there is something to add: an empty `query_pairs_mut` still leaves a bare `?`.
        if !query.is_empty() || matches!(paging, Paging::Upto(_)) {
            let mut qp = url.query_pairs_mut();
            for (k, v) in query {
                qp.append_pair(k, v);
            }
            if let Paging::Upto(per_page) = paging {
                qp.append_pair("perPage", &per_page.to_string());
            }
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
            // Checked after pacing, which may itself have slept past it.
            if self.deadline.is_some_and(|d| Instant::now() >= d) {
                tracing::warn!(
                    pages = visited.len(),
                    "meraki collect ran out of its time budget; keeping what it read"
                );
                return Err(Stop::Budget);
            }
            // What goes on the wire (ADR-166). `url` itself stays the logical one: the allow-list
            // above and `visited` below compare logical URLs. A physical URL in `visited` would
            // never equal a `Link` again, so a server answering with its own page would be paged
            // to the cap instead of stopped at `Cycle`.
            let sent = match &self.wire {
                Some(origin) => origin.wire(&url),
                None => url.clone(),
            };
            let resp = match self.client.get(sent).send().await {
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
            items.extend(page_items(&body, shape)?);
            visited.push(url);

            match next_page(&visited, next.as_deref(), self.max_pages) {
                PageStep::Next(n) => url = n,
                PageStep::Done => return Ok(()),
                PageStep::Stop(stop) => return Err(stop),
            }
        }
    }
}

/// How a listing is asked to page: with a page size, or with none at all.
///
/// A page size used to be appended to every listing, which is only right for listings that document
/// one. `appliance/uplinks/usage/byNetwork` documents none and was recorded without one — one page
/// for a whole 434-network organization (ADR-164 決定 23) — and a listing's documented maximum
/// differs (`appliance/vpn/statuses` stops at 300), so each call site says which it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Paging {
    /// Send `perPage` with this size.
    Upto(u32),
    /// Send no `perPage`. A `Link: rel=next` is still followed if the server ever sends one.
    Unpaged,
}

/// What a page of a listing looks like (ADR-167 決定 2).
///
/// Every listing this module read before the switch ports answers a bare JSON array, and ADR-164
/// 決定 19 made anything else `Malformed`. Two of the three switch-port listings answer an envelope
/// instead — `{"items": [...], "meta": {...}}`, measured on a real organization — while the third
/// (`switch/ports/bySwitch`) is a bare array again. So each call site says which it reads, and the
/// rule stays exactly as strict for both: the wrong shape is `Malformed`, never an empty page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// A bare JSON array.
    Array,
    /// An object whose `items` is a JSON array; whatever else it carries (`meta`) is not read.
    Items,
    /// One bare JSON object, which is the one item (ADR-164 決定 28: a network's
    /// `appliance/singleLan` is a settings document, not a listing).
    Object,
}

/// Why a paged read ended before the server said it had no more pages.
///
/// One vocabulary for both readers, because which of these is an error is the *caller's* question:
/// the lenient reader keeps what it has for the last six, the strict one refuses all nine.
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
    /// The session's page cap ([`MAX_PAGES`] unless the collect set its own) was read and the
    /// server still offered another.
    PageCap,
    /// The collect's time budget ran out before the next request (ADR-167). Only the switch-port
    /// tier sets one ([`SWITCH_PORTS_BUDGET`]).
    Budget,
}

impl Stop {
    /// Whether the lenient reader reports this rather than returning what it gathered. These three
    /// were errors before the vocabulary existed, and a collect that swallowed a refused key would
    /// report every device as simply having no data.
    fn fails_a_collect(self) -> bool {
        match self {
            Self::Host | Self::Auth(_) | Self::Malformed => true,
            Self::Network
            | Self::RateLimited
            | Self::Status(_)
            | Self::Cycle
            | Self::PageCap
            | Self::Budget => false,
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
            | Self::PageCap
            | Self::Budget => "meraki response could not be read".into(),
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
            Stop::Cycle | Stop::PageCap | Stop::Budget => Self::Truncated,
        }
    }
}

/// The items of one page. Pure, so the rule is tested without a server.
///
/// 🚨 **A 200 whose body is not the listing's shape is not a page** (ADR-164 決定 19). Every parser
/// below reads array elements only. A body of the wrong shape used to be taken as one item, which no
/// parser can read a `serial` out of — so it read as a listing that was **complete and empty**: the
/// inventory sync marked every stored device missing, and a collect counted as answered by the
/// Dashboard, which is what closes an organization's collection alert (決定 18). An empty array is
/// still an answer; an organization may hold nothing.
///
/// Which shape is right is the listing's to say ([`Shape`], ADR-167 決定 2): an envelope where a
/// bare array is expected is `Malformed`, and so is a bare array where an envelope is.
fn page_items(body: &str, shape: Shape) -> Result<Vec<Value>, Stop> {
    let parsed = match serde_json::from_str::<Value>(body) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error = %e, "meraki json parse failed");
            return Err(Stop::Malformed);
        }
    };
    match (shape, parsed) {
        (Shape::Array, Value::Array(items)) => Ok(items),
        (Shape::Items, Value::Object(mut envelope)) => match envelope.remove("items") {
            Some(Value::Array(items)) => Ok(items),
            _ => {
                tracing::debug!("meraki listing answered an envelope with no items array");
                Err(Stop::Malformed)
            }
        },
        (Shape::Object, Value::Object(document)) => Ok(vec![Value::Object(document)]),
        (Shape::Array | Shape::Items | Shape::Object, _) => {
            tracing::debug!(
                ?shape,
                "meraki listing answered something other than its shape"
            );
            Err(Stop::Malformed)
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
/// Stops at `max_pages` ([`MAX_PAGES`] for every listing but the switch ports'), and — ADR-158 B5 —
/// at a next link naming a page already fetched. A server that answered with its own URL, or a
/// cycle between two pages, used to be followed to the cap and every repeat taken in again: the
/// same devices fifty times over, each one a sample.
///
/// 🚨 Those two stops are **not** [`PageStep::Done`]. They used to be indistinguishable from it
/// (both were `None`), which is harmless to a collect and would be read by the inventory sync as
/// "the organization has exactly these devices" (ADR-164).
fn next_page(visited: &[reqwest::Url], next: Option<&str>, max_pages: usize) -> PageStep {
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
    if visited.len() >= max_pages {
        tracing::warn!(max = max_pages, "meraki pagination truncated at page cap");
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
///
/// `wire` is where the requests physically go when a lab build says so (ADR-166), `None` otherwise.
pub(crate) async fn collect(
    spec: &MerakiCollectSpec,
    timeout: Duration,
    wire: Option<&MerakiWireOrigin>,
) -> Result<MerakiCollected, MerakiFetchError> {
    let mut session = Session::new(
        &spec.base_url,
        &spec.api_key,
        spec.target_rps,
        timeout,
        wire,
    )
    .map_err(|e| {
        tracing::debug!(error = %e, "meraki collect session refused");
        MerakiFetchError::Config
    })?;
    // How each listing of this collect ended (ADR-164 決定 18 and 25).
    let mut listings = Listings::default();
    // Every listing asks the WHOLE organization and keeps the watched networks' rows here
    // (ADR-164 決定 22). Sending the networks as `networkIds[]` is what the Dashboard documents and
    // what it refuses: its nginx answers 414 past a request target of 8,177 characters (measured
    // 2026-09-22), which about two hundred network ids reach — and a new organization watches every
    // network it has. Two of the four listings ignored the filter anyway: `uplinksLossAndLatency`
    // and `byUsage` answered the whole organization when asked about one network.
    let watched = Watched::new(&spec.network_ids);
    let no_query: [(&str, String); 0] = [];
    // A listing after a tier's first one is contained: a refusal there costs that listing's readings
    // and is named in the report, rather than throwing away what the listings before it brought back
    // (決定 25). The FIRST listing's refusal still fails the collect — a revoked key or an unreadable
    // answer is not a partial answer.
    let contained = |r: Result<(Vec<Value>, Option<MerakiFetchError>), MerakiFetchError>| {
        r.unwrap_or_else(|why| (Vec::new(), Some(why)))
    };

    let mut data: Vec<DeviceDatum> = Vec::new();
    // A switch's ports, by serial — the switch-port tier's interface inventory (ADR-167).
    let mut ports: BTreeMap<String, BTreeMap<u32, MerakiPort>> = BTreeMap::new();
    // An access point's radios, by serial then slot — the wireless tier's (ADR-168).
    let mut radios: BTreeMap<String, BTreeMap<u32, MerakiRadio>> = BTreeMap::new();
    match spec.tier {
        MerakiTier::Availability => {
            let path = format!(
                "{API_PREFIX}/organizations/{}/devices/availabilities",
                spec.org_id
            );
            let (items, stop) = session
                .get_paged_reported(&path, &no_query, Paging::Upto(spec.per_page), Shape::Array)
                .await?;
            let kept = watched.keep(items);
            listings.note(MerakiListing::Availabilities, stop, kept.len());
            data.extend(parse_availability(&kept));
        }
        MerakiTier::Uplink => {
            let loss_path = format!(
                "{API_PREFIX}/organizations/{}/devices/uplinksLossAndLatency",
                spec.org_id
            );
            let q = [("timespan", "300".to_owned())];
            let (items, stop) = session
                .get_paged_reported(&loss_path, &q, Paging::Upto(spec.per_page), Shape::Array)
                .await?;
            let kept = watched.keep(items);
            listings.note(MerakiListing::UplinksLossAndLatency, stop, kept.len());
            data.extend(parse_uplink_loss_latency(&kept));

            let status_path = format!(
                "{API_PREFIX}/organizations/{}/appliance/uplink/statuses",
                spec.org_id
            );
            let (items, stop) = contained(
                session
                    .get_paged_reported(
                        &status_path,
                        &no_query,
                        Paging::Upto(spec.per_page),
                        Shape::Array,
                    )
                    .await,
            );
            let kept = watched.keep(items);
            listings.note(MerakiListing::ApplianceUplinkStatuses, stop, kept.len());
            data.extend(parse_uplink_statuses(&kept));

            // Auto VPN, last (決定 25): the slowest of the three (4–5 s for 347 rows measured), and a
            // listing whose failure must not cost the uplinks' readings. Its documented page size
            // tops out at 300 — `perPage=1000` is a 400 (measured).
            let vpn_path = format!(
                "{API_PREFIX}/organizations/{}/appliance/vpn/statuses",
                spec.org_id
            );
            let (items, stop) = contained(
                session
                    .get_paged_reported(
                        &vpn_path,
                        &no_query,
                        Paging::Upto(spec.per_page.min(VPN_STATUSES_MAX_PER_PAGE)),
                        Shape::Array,
                    )
                    .await,
            );
            let admitted = items.iter().filter(|it| watched.admits(it)).count();
            listings.note(MerakiListing::ApplianceVpnStatuses, stop, admitted);
            // Every row goes in, watched or not: a peer is judged by its OWN row.
            data.extend(parse_vpn_statuses(&items, &watched));
        }
        // Every MX's WAN uplinks, over the tier's own interval (ADR-164 決定 23). What this read
        // replaced, `summary/top/devices/byUsage`, refuses a timespan under 28,800 seconds, answers
        // an organization's top ten devices only, and carries a total rather than sent/received —
        // it failed on every real organization and never stored a sample.
        MerakiTier::Traffic => {
            let path = format!(
                "{API_PREFIX}/organizations/{}/appliance/uplinks/usage/byNetwork",
                spec.org_id
            );
            let window = usage_window(spec.interval_secs);
            let q = [("timespan", window.to_string())];
            let (items, stop) = session
                .get_paged_reported(&path, &q, Paging::Unpaged, Shape::Array)
                .await?;
            let kept = watched.keep(items);
            listings.note(MerakiListing::ApplianceUplinksUsage, stop, kept.len());
            data.extend(parse_uplinks_usage(&kept, window));
        }
        // Every switch port's status, speed and traffic (ADR-167). Three organization-wide
        // listings joined by serial; the first is the spine.
        MerakiTier::SwitchPorts => {
            // Measured on a real organization (854 switches): statuses 76 s, one usage bucket
            // 20–30 s, the configured names 84 s. Core leases the collect lane for 300 s, so it
            // stops asking at the budget and keeps what it read — the names, read last, go first.
            // (Since ADR-169 availability no longer waits behind this: it has the other lane.)
            session.max_pages = SWITCH_PORT_MAX_PAGES;
            session.deadline = Some(Instant::now() + SWITCH_PORTS_BUDGET);

            let status_path = format!(
                "{API_PREFIX}/organizations/{}/switch/ports/statuses/bySwitch",
                spec.org_id
            );
            let (items, stop) = session
                .get_paged_reported(
                    &status_path,
                    &no_query,
                    Paging::Upto(SWITCH_PORT_STATUSES_PER_PAGE),
                    Shape::Items,
                )
                .await?;
            let kept = watched.keep(items);
            listings.note(MerakiListing::SwitchPortStatuses, stop, kept.len());
            let (statuses, spine) = parse_switch_port_statuses(&kept);
            data.extend(statuses);
            ports = spine;

            // One five-minute bucket the Dashboard has had time to fill (決定 6).
            let (t0, t1) = switch_usage_bucket(unix_now_secs());
            let usage_path = format!(
                "{API_PREFIX}/organizations/{}/switch/ports/usage/history/byDevice/byInterval",
                spec.org_id
            );
            let q = [
                ("interval", SWITCH_USAGE_BUCKET_SECS.to_string()),
                ("t0", t0.to_string()),
                ("t1", t1.to_string()),
            ];
            let (items, stop) = contained(
                session
                    .get_paged_reported(
                        &usage_path,
                        &q,
                        Paging::Upto(SWITCH_PORT_USAGE_PER_PAGE),
                        Shape::Items,
                    )
                    .await,
            );
            let admitted = items.iter().filter(|it| on_spine(it, &ports)).count();
            listings.note(MerakiListing::SwitchPortUsage, stop, admitted);
            data.extend(parse_switch_port_usage(&items, &ports));

            if spec.port_names {
                let config_path = format!(
                    "{API_PREFIX}/organizations/{}/switch/ports/bySwitch",
                    spec.org_id
                );
                let (items, stop) = contained(
                    session
                        .get_paged_reported(
                            &config_path,
                            &no_query,
                            Paging::Upto(SWITCH_PORT_CONFIG_PER_PAGE),
                            Shape::Array,
                        )
                        .await,
                );
                let admitted = items.iter().filter(|it| on_spine(it, &ports)).count();
                listings.note(MerakiListing::SwitchPortConfig, stop, admitted);
                name_switch_ports(&items, &mut ports);
            }
        }
        // Every access point's clients and each radio's channel utilization, and — when core asks,
        // every twenty minutes — the SSIDs each one broadcasts and its radios' channel and power
        // (ADR-168). Organization-wide listings joined by serial.
        MerakiTier::Wireless => {
            // Measured on a real organization (1,710 access points): clients about 3 s, utilization
            // about 2 s, the SSIDs 78 s at 500 a page. The budget is the switch ports' reason: core
            // leases the collect lane for 300 s.
            //
            // 🚨 Since ADR-169 core sends the SSID read as a job of its own (`ssid_only`), in the
            // slow lane: no clients, and the utilization is read only to know which radios
            // answered — its values are the rounds' to publish. A second client count or
            // utilization sample between two rounds shrinks VictoriaMetrics' interval estimate,
            // and the ordinary interval is then drawn as a gap.
            session.max_pages = WIRELESS_MAX_PAGES;
            session.deadline = Some(Instant::now() + WIRELESS_BUDGET);

            if !spec.ssid_only {
                let clients_path = format!(
                    "{API_PREFIX}/organizations/{}/wireless/clients/overview/byDevice",
                    spec.org_id
                );
                let (items, stop) = session
                    .get_paged_reported(
                        &clients_path,
                        &no_query,
                        Paging::Upto(spec.per_page.min(WIRELESS_CLIENTS_PER_PAGE)),
                        Shape::Items,
                    )
                    .await?;
                let kept = watched.keep(items);
                listings.note(MerakiListing::WirelessClients, stop, kept.len());
                data.extend(parse_wireless_clients(&kept));
            }

            let util_path = format!(
                "{API_PREFIX}/organizations/{}/wireless/devices/channelUtilization/byDevice",
                spec.org_id
            );
            let window = WIRELESS_UTIL_WINDOW_SECS.to_string();
            let q = [("interval", window.clone()), ("timespan", window)];
            let util = session
                .get_paged_reported(
                    &util_path,
                    &q,
                    Paging::Upto(spec.per_page.min(WIRELESS_UTIL_PER_PAGE)),
                    Shape::Array,
                )
                .await;
            // On its own the SSID read has this listing first, and a first listing's refusal fails
            // the collect (決定 25).
            let (items, stop) = if spec.ssid_only {
                util?
            } else {
                contained(util)
            };
            let kept = watched.keep(items);
            listings.note(MerakiListing::WirelessChannelUtilization, stop, kept.len());
            radios = parse_channel_utilization(&kept);

            if spec.ssid_statuses || spec.ssid_only {
                let ssid_path = format!(
                    "{API_PREFIX}/organizations/{}/wireless/ssids/statuses/byDevice",
                    spec.org_id
                );
                let (items, stop) = contained(
                    session
                        .get_paged_reported(
                            &ssid_path,
                            &no_query,
                            Paging::Upto(WIRELESS_SSIDS_PER_PAGE),
                            Shape::Items,
                        )
                        .await,
                );
                let kept = watched.keep(items);
                let admitted = kept
                    .iter()
                    .filter(|it| {
                        it.get("serial")
                            .and_then(Value::as_str)
                            .is_some_and(|s| radios.contains_key(s))
                    })
                    .count();
                listings.note(MerakiListing::WirelessSsidStatuses, stop, admitted);
                data.extend(apply_ssid_statuses(&kept, &mut radios));
            }
            if spec.ssid_only {
                for radio in radios.values_mut().flat_map(BTreeMap::values_mut) {
                    radio.channel_util_pct = None;
                    radio.non_wifi_util_pct = None;
                }
            }
        }
        // The inventory is read by core's periodic sync (`fetch_inventory`), not by a collect —
        // nothing to gather here.
        MerakiTier::Inventory => {}
    }
    let mut observations = fold(data);
    attach_ports(&mut observations, ports);
    attach_radios(&mut observations, radios);
    Ok(MerakiCollected {
        observations,
        stopped: listings.stopped,
        failed_listing: listings.failed,
    })
}

// ── The switch-port tier (ADR-167) ──────────────────────────────────────────────────────────

/// The largest page `switch/ports/statuses/bySwitch` accepts: "The perPage parameter must be
/// between 3 and 20" (measured 2026-09-22). The collect's default of 1000 is a 400 here.
const SWITCH_PORT_STATUSES_PER_PAGE: u32 = 20;
/// The largest page `switch/ports/usage/history/byDevice/byInterval` accepts (3–50, measured).
const SWITCH_PORT_USAGE_PER_PAGE: u32 = 50;
/// The largest page `switch/ports/bySwitch` accepts (3–50, measured).
const SWITCH_PORT_CONFIG_PER_PAGE: u32 = 50;
/// How many pages one switch-port listing may run to: enough for the most switches one
/// organization may hold as nodes (50,000, `max_devices`' ceiling) at 20 a page. It is a bound
/// against a server that never stops, not the thing that bounds a collect — the clock is
/// ([`SWITCH_PORTS_BUDGET`]). At the measured 1.7 s a page the fixed [`MAX_PAGES`] would have cut
/// the listing at 1,000 switches, silently.
const SWITCH_PORT_MAX_PAGES: usize = 2_500;
/// How long one switch-port collect may keep asking. Core leases the lane it sends one in (the
/// organization's slow lane, ADR-169) for 300 s ("LEASE" in its Meraki scheduler) and counts a
/// flight that outlives it as unanswered, so this stops well inside it.
const SWITCH_PORTS_BUDGET: Duration = Duration::from_secs(240);
/// The Dashboard's switch-port usage buckets are five minutes long.
const SWITCH_USAGE_BUCKET_SECS: u64 = 300;
/// How long after a bucket ends it is read. Measured on a real organization: 78 s after its end no
/// switch had a bucket, 378 s 18%, 460 s 43%, 678 s all of them — each switch appears whole and at
/// once, and before that it lists `ports: []`. Twelve minutes clears the slowest measured.
const SWITCH_USAGE_SETTLE_SECS: u64 = 720;

/// The one usage bucket a switch-port collect at `now` reads (ADR-167 決定 6): the newest
/// five-minute bucket that ended at least [`SWITCH_USAGE_SETTLE_SECS`] ago, as `(t0, t1)` in Unix
/// seconds. Aligned to the bucket grid — the Dashboard rounds an unaligned `t0` down, which would
/// make the answer two buckets.
#[must_use]
fn switch_usage_bucket(now_unix_secs: u64) -> (u64, u64) {
    let t1 = now_unix_secs.saturating_sub(SWITCH_USAGE_SETTLE_SECS) / SWITCH_USAGE_BUCKET_SECS
        * SWITCH_USAGE_BUCKET_SECS;
    (t1.saturating_sub(SWITCH_USAGE_BUCKET_SECS), t1)
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Whether a usage or configuration row belongs to a switch on the spine.
fn on_spine(row: &Value, spine: &BTreeMap<String, BTreeMap<u32, MerakiPort>>) -> bool {
    row.get("serial")
        .and_then(Value::as_str)
        .is_some_and(|s| spine.contains_key(s))
}

/// `switch/ports/statuses/bySwitch` → each port's status, admin state and speed, and the spine the
/// other two listings are joined to: every switch it reports ports for, with those ports.
///
/// * `if_oper_status`: `Connected` 1, `Disconnected` 2, a word this build does not know nothing.
/// * `if_admin_status`: `enabled` true 1, false 2.
/// * `if_high_speed`: Mbps, only while the port has a speed.
///
/// 🚨 **A switch with no port `Connected` is left out altogether** (決定 14). Measured on a real
/// organization: every offline or dormant switch was still listed, with every port `Disconnected`.
/// Reported as such, each switch that went down would raise a port-down alert per port beside its
/// own node-down one. An online switch with nothing plugged in is left out too, which costs nothing:
/// it has no reading worth taking.
fn parse_switch_port_statuses(
    items: &[Value],
) -> (
    Vec<DeviceDatum>,
    BTreeMap<String, BTreeMap<u32, MerakiPort>>,
) {
    let mut data = Vec::new();
    let mut spine: BTreeMap<String, BTreeMap<u32, MerakiPort>> = BTreeMap::new();
    for row in items {
        let (Some(serial), Some(rows)) = (
            row.get("serial").and_then(Value::as_str),
            row.get("ports").and_then(Value::as_array),
        ) else {
            continue;
        };
        let status_of = |p: &Value| {
            p.get("status")
                .and_then(Value::as_str)
                .and_then(switch_port_oper_status)
        };
        if !rows.iter().any(|p| status_of(p) == Some(1.0)) {
            continue;
        }
        let ports = spine.entry(serial.to_owned()).or_default();
        for p in rows {
            let Some(port_id) = p.get("portId").and_then(Value::as_str) else {
                continue;
            };
            let ifindex = switch_port_ifindex(port_id);
            let speed_bps = p
                .get("speed")
                .and_then(Value::as_str)
                .and_then(switch_port_speed_bps);
            let duplex = p
                .get("duplex")
                .and_then(Value::as_str)
                .and_then(|w| Duplex::parse(&w.trim().to_ascii_lowercase()));
            let mut push = |metric: &str, value: f64| {
                data.push(DeviceDatum {
                    serial: serial.to_owned(),
                    sample: MerakiSample {
                        metric: metric.to_owned(),
                        ifindex: Some(ifindex),
                        value,
                    },
                    uplink: None,
                });
            };
            if let Some(oper) = status_of(p) {
                push("if_oper_status", oper);
            }
            if let Some(enabled) = p.get("enabled").and_then(Value::as_bool) {
                push("if_admin_status", if enabled { 1.0 } else { 2.0 });
            }
            if let Some(bps) = speed_bps {
                // `ifHighSpeed` is whole Mbps; every speed the Dashboard reports is one.
                #[allow(clippy::cast_precision_loss)]
                push("if_high_speed", bps as f64 / 1e6);
            }
            ports.insert(
                ifindex,
                MerakiPort {
                    ifindex,
                    port_id: port_id.to_owned(),
                    alias: None,
                    speed_bps,
                    duplex,
                },
            );
        }
    }
    (data, spine)
}

/// `switch/ports/usage/history/byDevice/byInterval` → each port's average receive and send rate
/// over the bucket, bits per second (決定 6), for the switches on the spine.
///
/// * `bandwidth.usage.downstream` is what the port **received** (`meraki_port_in_bps`) and
///   `upstream` what it **sent** — measured: uplink ports' downstream about twice their upstream,
///   every access port's the other way round, over three settled buckets and every switch.
/// * `bandwidth` is kilobits per second of **1,000** bits (its ratio to `data.usage`'s kilobytes of
///   1,024 bytes was 0.977), so bits/s is `× 1000`.
/// * Both directions or neither: a port with one would draw half a link.
/// * A port the bucket does not list gets **no** reading, never a zero — measured, about 1.7% of the
///   connected ports were missing from a settled bucket, and a zero there would be a false outage.
/// * Should the answer carry more than one interval, the newest is read.
fn parse_switch_port_usage(
    items: &[Value],
    spine: &BTreeMap<String, BTreeMap<u32, MerakiPort>>,
) -> Vec<DeviceDatum> {
    let mut out = Vec::new();
    for row in items {
        let Some(serial) = row.get("serial").and_then(Value::as_str) else {
            continue;
        };
        let Some(known) = spine.get(serial) else {
            continue;
        };
        for p in row
            .get("ports")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(ifindex) = p
                .get("portId")
                .and_then(Value::as_str)
                .map(switch_port_ifindex)
            else {
                continue;
            };
            if !known.contains_key(&ifindex) {
                continue;
            }
            let newest = p
                .get("intervals")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .max_by_key(|i| i.get("endTs").and_then(Value::as_str).unwrap_or(""));
            let Some(usage) = newest.and_then(|i| i.get("bandwidth")?.get("usage")) else {
                continue;
            };
            let (Some(down), Some(up)) = (
                usage.get("downstream").and_then(json_number),
                usage.get("upstream").and_then(json_number),
            ) else {
                continue;
            };
            for (metric, kbps) in [
                (METRIC_MERAKI_PORT_IN_BPS, down),
                (METRIC_MERAKI_PORT_OUT_BPS, up),
            ] {
                out.push(DeviceDatum {
                    serial: serial.to_owned(),
                    sample: MerakiSample {
                        metric: metric.to_owned(),
                        ifindex: Some(ifindex),
                        // Whole bits: `32.3 * 1000.0` is 32,299.999… in binary floating point.
                        value: (kbps * 1000.0).round(),
                    },
                    uplink: None,
                });
            }
        }
    }
    out
}

/// `switch/ports/bySwitch` → the name each port on the spine was given in the Dashboard. A port
/// with no name gets `""`, so a name an operator removed is removed here too.
fn name_switch_ports(items: &[Value], spine: &mut BTreeMap<String, BTreeMap<u32, MerakiPort>>) {
    for row in items {
        let Some(known) = row
            .get("serial")
            .and_then(Value::as_str)
            .and_then(|s| spine.get_mut(s))
        else {
            continue;
        };
        for p in row
            .get("ports")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(port) = p
                .get("portId")
                .and_then(Value::as_str)
                .and_then(|id| known.get_mut(&switch_port_ifindex(id)))
            else {
                continue;
            };
            let name = p.get("name").and_then(Value::as_str).unwrap_or("").trim();
            port.alias = Some(name.to_owned());
        }
    }
}

/// Hand each switch its ports, creating the observation for a switch that has ports but no sample.
fn attach_ports(
    observations: &mut Vec<MerakiObservation>,
    spine: BTreeMap<String, BTreeMap<u32, MerakiPort>>,
) {
    let mut at = ObservationIndex::of(observations);
    for (serial, ports) in spine {
        at.observation(observations, serial).ports = ports.into_values().collect();
    }
}

/// Where each device's observation sits in the collect's list, so attaching the switch ports or the
/// radios is one lookup per device. Each used to scan the list per device — quadratic in the
/// organization's switches or access points, on the poller's async worker, every collect.
struct ObservationIndex(HashMap<String, usize>);

impl ObservationIndex {
    fn of(observations: &[MerakiObservation]) -> Self {
        let mut at = HashMap::with_capacity(observations.len());
        for (i, o) in observations.iter().enumerate() {
            // The first one answers, as the scan's `find` did — `fold` never makes two.
            at.entry(o.serial.clone()).or_insert(i);
        }
        Self(at)
    }

    /// The observation for `serial`, appended with nothing in it when the collect has none.
    fn observation<'a>(
        &mut self,
        observations: &'a mut Vec<MerakiObservation>,
        serial: String,
    ) -> &'a mut MerakiObservation {
        let i = match self.0.get(&serial) {
            Some(&i) => i,
            None => {
                let i = observations.len();
                self.0.insert(serial.clone(), i);
                observations.push(MerakiObservation {
                    serial,
                    samples: Vec::new(),
                    uplinks: Vec::new(),
                    ports: Vec::new(),
                    radios: Vec::new(),
                });
                i
            }
        };
        &mut observations[i]
    }
}

// ── The wireless tier (ADR-168) ─────────────────────────────────────────────────────────────

/// The largest page `wireless/clients/overview/byDevice` accepts: "The perPage parameter must be
/// between 3 and 1000" (measured 2026-09-22).
const WIRELESS_CLIENTS_PER_PAGE: u32 = 1000;
/// The largest page `wireless/devices/channelUtilization/byDevice` accepts (3–1000, measured).
const WIRELESS_UTIL_PER_PAGE: u32 = 1000;
/// The page `wireless/ssids/statuses/byDevice` is read at. It accepts up to 500, and a page of 500
/// took 20–25 s on a real organization — close to the 30 s a request may take. Half of that keeps
/// each page near 11 s; the time is per row, so the whole listing costs the same either way.
const WIRELESS_SSIDS_PER_PAGE: u32 = 250;
/// How many pages one wireless listing may run to: the most access points one organization may hold
/// as nodes (50,000, `max_devices`' ceiling) at [`WIRELESS_SSIDS_PER_PAGE`]. A bound against a
/// server that never stops; the clock is what bounds a collect ([`WIRELESS_BUDGET`]).
const WIRELESS_MAX_PAGES: usize = 200;
/// How long one wireless collect may keep asking — inside core's 300 s collect lease, for the reason
/// [`SWITCH_PORTS_BUDGET`] gives.
const WIRELESS_BUDGET: Duration = Duration::from_secs(240);
/// The channel-utilization window, asked as both `interval` and `timespan`: the last five minutes,
/// one bucket. Measured: accepted, and nearly every value changed from one five-minute read to the
/// next. The Dashboard refuses windows it has no bucket for.
const WIRELESS_UTIL_WINDOW_SECS: u32 = 300;

/// `wireless/clients/overview/byDevice` → each access point's clients online
/// (`counts.byStatus.online`) as `wlan_ap_client_count`, the name a controller-walked AP uses.
///
/// Every access point of the organization has a row — a stopped one answers 0, which is the truth
/// about it (ADR-168 決定 4), so it is published like any other.
fn parse_wireless_clients(items: &[Value]) -> Vec<DeviceDatum> {
    items
        .iter()
        .filter_map(|it| {
            let serial = it.get("serial")?.as_str()?.to_owned();
            let online = it
                .get("counts")?
                .get("byStatus")?
                .get("online")
                .and_then(json_number)?;
            Some(DeviceDatum {
                serial,
                sample: MerakiSample {
                    metric: METRIC_WLAN_AP_CLIENT_COUNT.to_owned(),
                    ifindex: None,
                    value: online,
                },
                uplink: None,
            })
        })
        .collect()
}

/// `wireless/devices/channelUtilization/byDevice` → each access point's radios, one per band it
/// reported, with the band's utilization (`total`) and the non-Wi-Fi part of it (`nonWifi`).
///
/// * The radio is the band's **first** slot ([`WlanBand::slot_base`]). The Dashboard reports per
///   band, so on an access point with two radios in one band both are in this number, and it lands
///   on the first (ADR-168 決定 5 — measured: no such access point among 1,710).
/// * A stopped access point lists no band at all (measured: every offline and dormant one), so it
///   gets no radio — and, by [`apply_ssid_statuses`], nothing from the SSID read either.
/// * A band word this build does not know is skipped, never guessed.
/// * The values are percentages with two decimals and are kept as they came.
fn parse_channel_utilization(items: &[Value]) -> BTreeMap<String, BTreeMap<u32, MerakiRadio>> {
    let mut out: BTreeMap<String, BTreeMap<u32, MerakiRadio>> = BTreeMap::new();
    for row in items {
        let Some(serial) = row.get("serial").and_then(Value::as_str) else {
            continue;
        };
        for b in row
            .get("byBand")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(band) = b
                .get("band")
                .and_then(Value::as_str)
                .and_then(WlanBand::from_meraki)
            else {
                continue;
            };
            let pct = |key: &str| b.get(key)?.get("percentage").and_then(json_number);
            let (total, non_wifi) = (pct("total"), pct("nonWifi"));
            if total.is_none() && non_wifi.is_none() {
                continue;
            }
            let slot = band.slot_base();
            out.entry(serial.to_owned()).or_default().insert(
                slot,
                MerakiRadio {
                    slot,
                    band,
                    channel_util_pct: total,
                    non_wifi_util_pct: non_wifi,
                    channel: None,
                    tx_power_dbm: None,
                },
            );
        }
    }
    out
}

/// `wireless/ssids/statuses/byDevice` → for each access point whose radios were measured just now:
/// how many SSIDs it broadcasts (`wlan_ap_ssid_count`), and each radio's channel and transmit power.
///
/// 🚨 **Only access points already in `radios` — the ones the utilization read measured** (ADR-168
/// 決定 4). Measured on a real organization: every offline and dormant access point was still
/// answered here with its last configuration, broadcasting. Published as such, a stopped access point
/// would read as broadcasting its SSIDs on its channels.
///
/// * One row per BSS (SSID × radio). A radio is its `radio.index`; its band, channel and power are the
///   same on every BSS of it (measured: no disagreement among 11,566). Radios are numbered into slots
///   by [`assign_radio_slots`], in index order within a band, as a controller-walked AP's are.
/// * A radio gets its channel and power only in a band the utilization read measured: a band that is
///   not on air has no row to put them on (決定 2).
/// * An SSID counts once when it is enabled and broadcasting on at least one radio. A measured access
///   point broadcasting nothing is a real 0.
/// * A channel or power the Dashboard left `null` is left out, never published as 0.
fn apply_ssid_statuses(
    items: &[Value],
    radios: &mut BTreeMap<String, BTreeMap<u32, MerakiRadio>>,
) -> Vec<DeviceDatum> {
    let mut out = Vec::new();
    for row in items {
        let Some(serial) = row.get("serial").and_then(Value::as_str) else {
            continue;
        };
        let Some(measured) = radios.get_mut(serial) else {
            continue;
        };
        let bss = row
            .get("basicServiceSets")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();

        let mut ssids: HashSet<String> = HashSet::new();
        let mut by_index: BTreeMap<u32, WlanRadioObservation> = BTreeMap::new();
        for b in bss {
            let radio = b.get("radio");
            let ssid = b.get("ssid");
            let on = |v: Option<&Value>, key: &str| {
                v.and_then(|v| v.get(key)).and_then(Value::as_bool) == Some(true)
            };
            if on(ssid, "enabled") && on(radio, "isBroadcasting") {
                let key = ssid.and_then(|s| {
                    s.get("number")
                        .and_then(Value::as_u64)
                        .map(|n| n.to_string())
                        .or_else(|| s.get("name").and_then(Value::as_str).map(str::to_owned))
                });
                if let Some(key) = key {
                    ssids.insert(key);
                }
            }
            let Some(radio) = radio else {
                continue;
            };
            let (Some(index), Some(band)) = (
                radio
                    .get("index")
                    .and_then(Value::as_str)
                    .and_then(|i| i.trim().parse::<u32>().ok()),
                radio
                    .get("band")
                    .and_then(Value::as_str)
                    .and_then(WlanBand::from_meraki),
            ) else {
                continue;
            };
            let channel = radio
                .get("channel")
                .and_then(Value::as_u64)
                .and_then(|c| u32::try_from(c).ok());
            // Whole dBm on the Dashboard (5–25 measured); the conversion only ever drops a fraction
            // nothing sends.
            #[allow(clippy::cast_possible_truncation)]
            let power = radio
                .get("power")
                .and_then(json_number)
                .map(|p| p.round() as i32);
            by_index
                .entry(index)
                .or_insert_with(|| WlanRadioObservation {
                    slot: 0,
                    band,
                    up: None,
                    clients: None,
                    channel,
                    channel_util_pct: None,
                    interference_pct: None,
                    noise_dbm: None,
                    client_signal_dbm: None,
                    tx_power_dbm: power,
                    in_octets: None,
                    out_octets: None,
                });
        }

        #[allow(clippy::cast_precision_loss)]
        out.push(DeviceDatum {
            serial: serial.to_owned(),
            sample: MerakiSample {
                metric: METRIC_WLAN_AP_SSID_COUNT.to_owned(),
                ifindex: None,
                value: ssids.len() as f64,
            },
            uplink: None,
        });

        for r in assign_radio_slots(by_index.into_iter().collect()) {
            if !measured.values().any(|m| m.band == r.band) {
                continue;
            }
            let radio = measured.entry(r.slot).or_insert(MerakiRadio {
                slot: r.slot,
                band: r.band,
                channel_util_pct: None,
                non_wifi_util_pct: None,
                channel: None,
                tx_power_dbm: None,
            });
            radio.channel = r.channel;
            radio.tx_power_dbm = r.tx_power_dbm.map(f64::from);
        }
    }
    out
}

/// Hand each access point its radios, creating the observation for one that has radios but no
/// sample.
fn attach_radios(
    observations: &mut Vec<MerakiObservation>,
    radios: BTreeMap<String, BTreeMap<u32, MerakiRadio>>,
) {
    let mut at = ObservationIndex::of(observations);
    for (serial, slots) in radios {
        at.observation(observations, serial).radios = slots.into_values().collect();
    }
}

/// How the listings of one collect ended (ADR-164 決定 18 and 25).
#[derive(Debug, Default)]
struct Listings {
    /// The FIRST reason a listing stopped early. One is enough: what core asks of it is "did the
    /// Dashboard answer", and a second stop after the first adds nothing.
    stopped: Option<MerakiFetchError>,
    /// The first listing that stopped early **with none of the rows this collect keeps** — and why.
    failed: Option<(MerakiListing, MerakiFetchError)>,
}

impl Listings {
    /// Record one listing's end. `kept` is how many of its rows the collect keeps: a listing that
    /// stopped early with some is a partial answer, which is still an answer; one that stopped with
    /// none is a failed listing, and is named even when the tier's other listings answered — that
    /// used to be hidden, because a collect only counted as failed when it brought back nothing at
    /// all.
    fn note(&mut self, listing: MerakiListing, stop: Option<MerakiFetchError>, kept: usize) {
        if let Some(why) = stop {
            self.stopped = self.stopped.or(Some(why));
            if kept == 0 {
                self.failed = self.failed.or(Some((listing, why)));
            }
        }
    }
}

/// The largest page `appliance/vpn/statuses` accepts: "The perPage parameter must be between 3 and
/// 300" (measured 2026-09-22 with `perPage=1000`).
const VPN_STATUSES_MAX_PER_PAGE: u32 = 300;

/// `appliance/vpn/statuses` → each watched MX's Auto VPN reachability (ADR-164 決定 25).
///
/// A row is a network with its VPN-participating MX (`deviceSerial` — measured: always the pair's
/// configured primary, even while the primary is down and its spare carries the tunnels), the MX's
/// `deviceStatus`, its `vpnMode` and one entry per Meraki peer network with `reachability`.
///
/// * **Only a row whose own MX is up says anything.** A down device's row is stale — on a real
///   organization every dormant spoke listed all its hubs unreachable. Its node has its own alert.
/// * **A peer is judged by its own row.** An unreachable peer whose MX is down is not counted, so
///   one dead hub does not put every one of its spokes into warning; a peer answering as reachable
///   counts whatever its row says. A peer with no row of its own cannot be placed, and is skipped.
/// * Every MX gets its **hub** peers counted — spokes peer with hubs, and hubs are meshed with each
///   other — and the share unreachable, when at least one hub was counted. A hub also gets its
///   unreachable **spokes** counted, for display.
///
/// `items` is the whole organization's answer, not only the watched rows: which peer is a hub, and
/// whether it is up, comes from the peer's own row, which may sit in a network nobody watches.
fn parse_vpn_statuses(items: &[Value], watched: &Watched<'_>) -> Vec<DeviceDatum> {
    let mode_of = |row: &Value| {
        row.get("vpnMode")
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase)
    };
    let up_of = |row: &Value| {
        MerakiAvailability::from_status(
            row.get("deviceStatus")
                .and_then(Value::as_str)
                .unwrap_or(""),
        )
        .is_up()
    };
    let peers: HashMap<&str, (Option<String>, bool)> = items
        .iter()
        .filter_map(|row| {
            let net = row.get("networkId").and_then(Value::as_str)?;
            Some((net, (mode_of(row), up_of(row))))
        })
        .collect();

    let mut out = Vec::new();
    for row in items {
        if !watched.admits(row) || !up_of(row) {
            continue;
        }
        let Some(serial) = row.get("deviceSerial").and_then(Value::as_str) else {
            continue;
        };
        let (mut hubs_ok, mut hubs_down, mut spokes_down) = (0u32, 0u32, 0u32);
        for peer in row
            .get("merakiVpnPeers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let reachable = match peer
                .get("reachability")
                .and_then(Value::as_str)
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("reachable") => true,
                Some("unreachable") => false,
                _ => continue, // a word this build does not know says nothing
            };
            let Some((peer_mode, peer_up)) = peer
                .get("networkId")
                .and_then(Value::as_str)
                .and_then(|n| peers.get(n))
            else {
                continue;
            };
            if !reachable && !peer_up {
                continue; // the peer is down: its own alert, not this MX's
            }
            match peer_mode.as_deref() {
                Some("hub") if reachable => hubs_ok += 1,
                Some("hub") => hubs_down += 1,
                Some("spoke") if !reachable => spokes_down += 1,
                _ => {}
            }
        }
        let mut push = |metric: &str, value: f64| {
            out.push(DeviceDatum {
                serial: serial.to_owned(),
                sample: MerakiSample {
                    metric: metric.to_owned(),
                    ifindex: None,
                    value,
                },
                uplink: None,
            });
        };
        let hubs = hubs_ok + hubs_down;
        if hubs > 0 {
            push(METRIC_MERAKI_VPN_HUBS_REACHABLE, f64::from(hubs_ok));
            push(METRIC_MERAKI_VPN_HUBS_UNREACHABLE, f64::from(hubs_down));
            push(
                METRIC_MERAKI_VPN_HUBS_UNREACHABLE_PCT,
                f64::from(hubs_down) * 100.0 / f64::from(hubs),
            );
        }
        if mode_of(row).as_deref() == Some("hub") {
            push(METRIC_MERAKI_VPN_SPOKES_UNREACHABLE, f64::from(spokes_down));
        }
    }
    out
}

/// The networks a collect reports on (ADR-164 決定 22). **Empty means every network** — the bus's
/// reading of an empty list, which core no longer sends (決定 16) but which a message omitting the
/// field still decodes to.
struct Watched<'a>(HashSet<&'a str>);

impl<'a> Watched<'a> {
    fn new(ids: &'a [String]) -> Self {
        Self(ids.iter().map(String::as_str).collect())
    }

    /// Whether one row of an organization-wide answer belongs to a watched network. A row naming
    /// no network is not admitted: nothing places it in a watched one.
    fn admits(&self, it: &Value) -> bool {
        self.0.is_empty() || row_network(it).is_some_and(|n| self.0.contains(n))
    }

    /// The rows of an organization-wide answer that belong to a watched network.
    fn keep(&self, items: Vec<Value>) -> Vec<Value> {
        if self.0.is_empty() {
            return items;
        }
        items.into_iter().filter(|it| self.admits(it)).collect()
    }
}

/// A row's network: `networkId` on the two uplink listings and on the uplink usage, `network.id` on
/// availabilities. Measured on a real organization (2026-09-22): every row of all four carried one.
fn row_network(it: &Value) -> Option<&str> {
    it.get("networkId").and_then(Value::as_str).or_else(|| {
        it.get("network")
            .and_then(|n| n.get("id"))
            .and_then(Value::as_str)
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
                ports: Vec::new(),
                radios: Vec::new(),
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
            let status = MerakiUplinkStatus::from_word(
                u.get("status").and_then(Value::as_str).unwrap_or(""),
            );
            let meta = MerakiUplink {
                ifindex,
                name: uplink_name(ifindex).unwrap_or(iface).to_owned(),
            };
            // The status for the chart, and the failed flag the seeded rule reads — the second on
            // every row, healthy ones too, so an open alert has a reading to close on (決定 24).
            for (metric, value) in [
                (METRIC_MERAKI_UPLINK_STATUS, status.gauge()),
                (
                    METRIC_MERAKI_UPLINK_FAILED,
                    if status.failed() { 1.0 } else { 0.0 },
                ),
            ] {
                out.push(DeviceDatum {
                    serial: serial.to_owned(),
                    sample: MerakiSample {
                        metric: metric.to_owned(),
                        ifindex: Some(ifindex),
                        value,
                    },
                    uplink: Some(meta.clone()),
                });
            }
        }
    }
    out
}

/// The window a traffic collect asks usage over: the tier's interval, so consecutive collects tile
/// time without a gap or an overlap — within what the Dashboard was measured to accept (300 s to one
/// day; ADR-164 決定 23). The traffic tier's cadence bounds are the same range, so the clamp only
/// matters to a job that does not say its interval.
#[must_use]
pub fn usage_window(interval_secs: u32) -> u32 {
    interval_secs.clamp(USAGE_WINDOW_MIN_SECS, USAGE_WINDOW_MAX_SECS)
}

/// The shortest usage window the Dashboard was measured to accept (2026-09-22).
const USAGE_WINDOW_MIN_SECS: u32 = 300;
/// The longest window the recording measured. The Dashboard documents 14 days; nothing longer
/// than a day was ever asked of it, and the traffic tier's cadence tops out at a day anyway.
const USAGE_WINDOW_MAX_SECS: u32 = 86_400;

/// `appliance/uplinks/usage/byNetwork`: each row is a network, and each of its `byUplink` entries
/// one MX's one uplink — `sent` / `received` in bytes over the window. Stored as the window's
/// average rate in bits per second, per uplink.
///
/// A number that arrives as a JSON string is read too: `appliance/vpn/stats` sends its byte counts
/// that way although its documentation says integer, so the sibling listing is not trusted to stay
/// numeric either. An entry whose interface is not one of the three known uplinks is skipped, not
/// given an invented row key (cardinality). A zero is a reading: an idle uplink is not a missing one.
fn parse_uplinks_usage(items: &[Value], window_secs: u32) -> Vec<DeviceDatum> {
    let secs = f64::from(window_secs.max(1));
    let mut out = Vec::new();
    for row in items {
        let Some(uplinks) = row.get("byUplink").and_then(Value::as_array) else {
            continue;
        };
        for u in uplinks {
            let (Some(serial), Some(iface)) = (
                u.get("serial").and_then(Value::as_str),
                u.get("interface").and_then(Value::as_str),
            ) else {
                continue;
            };
            let Some(ifindex) = uplink_ifindex(iface) else {
                continue;
            };
            let meta = MerakiUplink {
                ifindex,
                name: uplink_name(ifindex).unwrap_or(iface).to_owned(),
            };
            for (field, metric) in [
                ("sent", METRIC_MERAKI_UPLINK_SENT_BPS),
                ("received", METRIC_MERAKI_UPLINK_RECV_BPS),
            ] {
                if let Some(bytes) = u.get(field).and_then(json_number) {
                    out.push(DeviceDatum {
                        serial: serial.to_owned(),
                        sample: MerakiSample {
                            metric: metric.to_owned(),
                            ifindex: Some(ifindex),
                            value: bytes * 8.0 / secs,
                        },
                        uplink: Some(meta.clone()),
                    });
                }
            }
        }
    }
    out
}

/// A JSON number, or a string holding one.
fn json_number(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        .filter(|n: &f64| n.is_finite())
}

// ── Control-plane (adding an organization) ──────────────────────────────────────────────────

/// List the organizations the API key can access (`GET /organizations`). Read-only.
///
/// `wire` is where the request physically goes when a lab build says so (ADR-166), `None`
/// otherwise.
pub async fn list_organizations(
    base_url: &str,
    api_key: &str,
    timeout: Duration,
    wire: Option<&MerakiWireOrigin>,
) -> Result<Vec<MerakiOrgInfo>, TransportError> {
    let mut s = Session::new(base_url, api_key, 2.0, timeout, wire)?;
    let items = s
        .get_paged(
            &format!("{API_PREFIX}/organizations"),
            &[],
            Paging::Upto(1000),
            Shape::Array,
        )
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
/// `timeout` bounds one request. Bounding the whole read is the caller's job. `wire` is where the
/// requests physically go when a lab build says so (ADR-166), `None` otherwise.
pub async fn fetch_inventory(
    base_url: &str,
    api_key: &str,
    org_id: &str,
    target_rps: f64,
    timeout: Duration,
    wire: Option<&MerakiWireOrigin>,
) -> Result<MerakiInventory, MerakiFetchError> {
    let mut s = Session::new(base_url, api_key, target_rps, timeout, wire).map_err(|e| {
        tracing::debug!(error = %e, "meraki inventory session refused");
        MerakiFetchError::Config
    })?;
    let org = format!("{API_PREFIX}/organizations/{org_id}");

    let networks = s
        .get_paged_strict(
            &format!("{org}/networks"),
            &[],
            Paging::Upto(1000),
            Shape::Array,
        )
        .await?;
    let devices = s
        .get_paged_strict(
            &format!("{org}/devices"),
            &[],
            Paging::Upto(1000),
            Shape::Array,
        )
        .await?;
    let availabilities = s
        .get_paged_strict(
            &format!("{org}/devices/availabilities"),
            &[],
            Paging::Upto(1000),
            Shape::Array,
        )
        .await?;

    Ok(assemble_inventory(&networks, &devices, &availabilities))
}

/// Read every MX's warm-spare role (ADR-164 決定 26): `(serial, role)` for every appliance the
/// organization lists, `None` for one whose warm spare is not enabled. One paged GET of
/// `appliance/uplink/statuses` — the listing the uplink collect also reads. Read-only.
///
/// Strict, like [`fetch_inventory`]: a short answer is an error rather than a partial list, so a
/// device missing from a truncated answer is never read as having lost its role. The caller treats
/// any error as "no roles this time" and keeps what it stored.
pub async fn fetch_ha_roles(
    base_url: &str,
    api_key: &str,
    org_id: &str,
    target_rps: f64,
    timeout: Duration,
    wire: Option<&MerakiWireOrigin>,
) -> Result<Vec<(String, Option<MerakiHaRole>)>, MerakiFetchError> {
    let mut s = Session::new(base_url, api_key, target_rps, timeout, wire).map_err(|e| {
        tracing::debug!(error = %e, "meraki ha-role session refused");
        MerakiFetchError::Config
    })?;
    let rows = s
        .get_paged_strict(
            &format!("{API_PREFIX}/organizations/{org_id}/appliance/uplink/statuses"),
            &[],
            Paging::Upto(1000),
            Shape::Array,
        )
        .await?;
    Ok(parse_ha_roles(&rows))
}

/// `(serial, role)` per row: the role only while the warm spare is enabled. A single MX reports
/// `enabled: false` with `role: "primary"` — measured on 14 of 14 — which is not a pair.
fn parse_ha_roles(rows: &[Value]) -> Vec<(String, Option<MerakiHaRole>)> {
    rows.iter()
        .filter_map(|r| {
            let serial = r.get("serial")?.as_str()?.to_owned();
            let ha = r.get("highAvailability");
            let enabled = ha
                .and_then(|h| h.get("enabled"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let role = ha
                .and_then(|h| h.get("role"))
                .and_then(Value::as_str)
                .and_then(MerakiHaRole::from_token)
                .filter(|_| enabled);
            Some((serial, role))
        })
        .collect()
}

/// One address an MX holds on its network's LAN side (ADR-164 決定 28): its IP on one VLAN, or — in
/// a network with VLANs turned off — on the single LAN, which has no VLAN number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MerakiLanAddress {
    /// The VLAN's number (`id`). `None` on a single LAN.
    pub vlan_id: Option<u32>,
    /// The MX's own address on it (`applianceIp`), as the Dashboard wrote it.
    pub appliance_ip: String,
}

/// One network's LAN addresses, or why this read could not say.
pub type MerakiNetworkLan = Result<Vec<MerakiLanAddress>, MerakiFetchError>;

/// Read the LAN addresses the MX holds in each of `network_ids`, one network at a time (ADR-164 決定
/// 28): `GET /networks/{id}/appliance/vlans`, and where that answers 400 — VLANs are off —
/// `GET /networks/{id}/appliance/singleLan`. Where both answer 400 the network has no LAN side at
/// all and its list is **empty, which is an answer**: the caller stops waiting for one. Read-only.
///
/// Per network, not per organization, because the organization-wide
/// `organizations/{id}/appliance/vlans` is a beta endpoint: it answered 404 with no body on a real
/// organization that had not opted in to early access (measured 2026-09-23). The per-network one
/// answered 342 networks of 342, at a median of 220 ms.
///
/// Returns the networks it **reached**, in the order given. It stops at `budget` — the networks
/// after that are absent from the answer and read by a later sync — and at the first failure that
/// would be the same for every network (a refused key, a host outside the allow-list, 429s that
/// outlast the retries), so one bad key costs one request rather than three hundred. A network's
/// own failure is that network's `Err`: never an empty list, which would read as "no LAN".
///
/// `Err` only when no session could be built (`Config`): then nothing was sent.
pub async fn fetch_network_lans(
    base_url: &str,
    api_key: &str,
    network_ids: &[String],
    target_rps: f64,
    timeout: Duration,
    budget: Duration,
    wire: Option<&MerakiWireOrigin>,
) -> Result<Vec<(String, MerakiNetworkLan)>, MerakiFetchError> {
    let mut s = Session::new(base_url, api_key, target_rps, timeout, wire).map_err(|e| {
        tracing::debug!(error = %e, "meraki session for the LAN reads refused");
        MerakiFetchError::Config
    })?;
    let deadline = Instant::now() + budget;
    s.deadline = Some(deadline);
    let mut out = Vec::new();
    for network in network_ids {
        let answer = network_lan(&mut s, network).await;
        // A request the deadline refused was never sent: nothing was learned about this network.
        if matches!(answer, Err(MerakiFetchError::Truncated)) && Instant::now() >= deadline {
            break;
        }
        let same_for_every_network = matches!(
            answer,
            Err(MerakiFetchError::Auth(_) | MerakiFetchError::Host | MerakiFetchError::RateLimited)
        );
        out.push((network.clone(), answer));
        if same_for_every_network {
            break;
        }
    }
    Ok(out)
}

/// One network's LAN addresses: the VLANs, else the single LAN, else none.
async fn network_lan(s: &mut Session, network: &str) -> MerakiNetworkLan {
    // The id goes into the path. One the Dashboard would never issue is refused rather than joined,
    // so a listing cannot steer a request to another resource.
    if network.is_empty()
        || !network
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(MerakiFetchError::Malformed);
    }
    let path = format!("{API_PREFIX}/networks/{network}/appliance");
    match s
        .get_paged_strict(&format!("{path}/vlans"), &[], Paging::Unpaged, Shape::Array)
        .await
    {
        Ok(rows) => Ok(parse_vlan_lans(&rows)),
        // Taken to mean VLANs are off, and the single-LAN read decides. Half of that was measured:
        // all 342 networks with VLANs on answered 400 to the single-LAN read, and all 8 with them
        // off answered it 200. What `vlans` itself answers where they are off was not measured — any
        // 400 goes this way, so a different 400 costs one more request and ends in `Ok(empty)`.
        Err(MerakiFetchError::Status(400)) => match s
            .get_paged_strict(
                &format!("{path}/singleLan"),
                &[],
                Paging::Unpaged,
                Shape::Object,
            )
            .await
        {
            Ok(documents) => Ok(documents.iter().filter_map(parse_single_lan).collect()),
            Err(MerakiFetchError::Status(400)) => Ok(Vec::new()),
            Err(e) => Err(e),
        },
        Err(e) => Err(e),
    }
}

/// A network's VLANs → one address per VLAN that names one. Pure.
fn parse_vlan_lans(rows: &[Value]) -> Vec<MerakiLanAddress> {
    rows.iter()
        .filter_map(|r| {
            Some(MerakiLanAddress {
                vlan_id: vlan_number(r.get("id")),
                appliance_ip: r.get("applianceIp")?.as_str()?.to_owned(),
            })
        })
        .collect()
}

/// A VLAN's `id`: a number on the real Dashboard (1,035 rows of 1,035), read from a string too.
fn vlan_number(v: Option<&Value>) -> Option<u32> {
    match v? {
        Value::Number(n) => n.as_u64().and_then(|n| u32::try_from(n).ok()),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// A network's single-LAN settings → its one address, if it names one. Pure.
fn parse_single_lan(document: &Value) -> Option<MerakiLanAddress> {
    Some(MerakiLanAddress {
        vlan_id: None,
        appliance_ip: document.get("applianceIp")?.as_str()?.to_owned(),
    })
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
        lan_ip: it.get("lanIp").and_then(Value::as_str).map(str::to_owned),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The ports and radios go on the device's own observation when the collect has one, and on a
    /// new one — appended, in serial order — when it does not; an observation of the ports' does not
    /// get another for its radios. What the index replaced was a scan per device, so this pins that
    /// the answer did not move with it.
    #[test]
    fn ports_and_radios_join_their_devices_observation_or_start_one() {
        let seen = |serial: &str| MerakiObservation {
            serial: serial.into(),
            samples: vec![MerakiSample {
                metric: "meraki_device_up".into(),
                ifindex: None,
                value: 1.0,
            }],
            uplinks: Vec::new(),
            ports: Vec::new(),
            radios: Vec::new(),
        };
        let port = |ifindex: u32| MerakiPort {
            ifindex,
            port_id: ifindex.to_string(),
            alias: None,
            speed_bps: None,
            duplex: None,
        };
        let radio = |slot: u32| MerakiRadio {
            slot,
            band: yagra_common::WlanBand::Band5G,
            channel_util_pct: Some(5.0),
            non_wifi_util_pct: None,
            channel: None,
            tx_power_dbm: None,
        };
        let mut observations = vec![seen("Q2-B"), seen("Q2-A")];
        attach_ports(
            &mut observations,
            BTreeMap::from([
                (
                    "Q2-A".to_owned(),
                    BTreeMap::from([(2, port(2)), (1, port(1))]),
                ),
                ("Q2-C".to_owned(), BTreeMap::from([(1, port(1))])),
            ]),
        );
        attach_radios(
            &mut observations,
            BTreeMap::from([
                ("Q2-B".to_owned(), BTreeMap::from([(1, radio(1))])),
                ("Q2-C".to_owned(), BTreeMap::from([(1, radio(1))])),
                ("Q2-D".to_owned(), BTreeMap::from([(1, radio(1))])),
            ]),
        );
        let serials: Vec<&str> = observations.iter().map(|o| o.serial.as_str()).collect();
        assert_eq!(serials, ["Q2-B", "Q2-A", "Q2-C", "Q2-D"]);
        let at = |s: &str| observations.iter().find(|o| o.serial == s).unwrap();
        assert_eq!(at("Q2-A").ports, [port(1), port(2)]);
        assert_eq!(
            at("Q2-A").samples.len(),
            1,
            "the sample stayed with its device"
        );
        assert_eq!(at("Q2-B").radios, [radio(1)]);
        assert_eq!((at("Q2-C").ports.len(), at("Q2-C").radios.len()), (1, 1));
        assert!(at("Q2-D").samples.is_empty() && at("Q2-D").ports.is_empty());
    }

    #[test]
    fn session_new_refuses_non_meraki_host() {
        let t = Duration::from_secs(5);
        let lab = MerakiWireOrigin::parse("http://mock.example:8080").unwrap();
        // The key-exfiltration guard: a non-Meraki base is rejected before any request — and a
        // wire origin changes nothing about that, because it is checked on the stored URL.
        for wire in [None, Some(&lab)] {
            let err = Session::new("https://evil.example.com", "k", 2.0, t, wire);
            assert!(matches!(err, Err(TransportError::Io(_))));
            // And plain http is rejected.
            let err = Session::new("http://api.meraki.com", "k", 2.0, t, wire);
            assert!(matches!(err, Err(TransportError::Io(_))));
            // The canonical host is accepted.
            assert!(Session::new("https://api.meraki.com", "k", 2.0, t, wire).is_ok());
        }
    }

    #[test]
    fn a_wire_origin_is_a_bare_origin_and_nothing_else() {
        for ok in [
            "http://mock.example:8080",
            "http://mock.example:8080/",
            "https://mock.example",
            "  http://127.0.0.1:18080  ",
            "http://[2001:db8::1]:8080",
        ] {
            assert!(
                MerakiWireOrigin::parse(ok).is_ok(),
                "{ok} should be accepted"
            );
        }
        for refused in [
            "",
            "mock.example:8080",
            "ftp://mock.example",
            "http://mock.example:8080/api/v1",
            "http://mock.example:8080/?x=1",
            "http://mock.example:8080/#top",
            "http://user@mock.example:8080",
            "http://user:secret@mock.example:8080",
        ] {
            let err = MerakiWireOrigin::parse(refused).unwrap_err().to_string();
            assert!(
                !err.contains("secret") && !err.contains("user"),
                "the refusal must not repeat the value: {err}"
            );
        }
    }

    #[test]
    fn an_unset_or_blank_setting_is_no_origin() {
        assert_eq!(MerakiWireOrigin::from_setting(None).unwrap(), None);
        assert_eq!(MerakiWireOrigin::from_setting(Some("")).unwrap(), None);
        assert_eq!(MerakiWireOrigin::from_setting(Some("   ")).unwrap(), None);
        assert!(MerakiWireOrigin::from_setting(Some("http://mock.example"))
            .unwrap()
            .is_some());
        assert!(MerakiWireOrigin::from_setting(Some("not a url")).is_err());
    }

    #[test]
    fn the_wire_url_keeps_the_path_and_the_encoded_query_byte_for_byte() {
        let origin = MerakiWireOrigin::parse("http://mock.example:8080").unwrap();
        let logical = reqwest::Url::parse(
            "https://api.meraki.com/api/v1/organizations/1/devices/uplinksLossAndLatency\
             ?networkIds%5B%5D=N_1&networkIds%5B%5D=N_2&timespan=300&perPage=1000",
        )
        .unwrap();
        let sent = origin.wire(&logical);
        assert_eq!(
            sent.as_str(),
            "http://mock.example:8080/api/v1/organizations/1/devices/uplinksLossAndLatency\
             ?networkIds%5B%5D=N_1&networkIds%5B%5D=N_2&timespan=300&perPage=1000"
        );
        // The logical URL is what the allow-list and the cycle check keep reading.
        assert_eq!(logical.host_str(), Some("api.meraki.com"));
        assert_eq!(origin.to_string(), "http://mock.example:8080");
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
            next_page(
                std::slice::from_ref(&first),
                Some(first.as_str()),
                MAX_PAGES
            ),
            PageStep::Stop(Stop::Cycle)
        );
    }

    #[test]
    fn a_next_link_back_to_an_earlier_page_ends_the_paging() {
        let (a, b) = (page("Q2-A"), page("Q2-B"));
        assert_eq!(
            next_page(&[a.clone(), b], Some(a.as_str()), MAX_PAGES),
            PageStep::Stop(Stop::Cycle)
        );
    }

    /// The ordinary case still pages, and the cap still stops it.
    #[test]
    fn a_new_next_link_is_followed_until_the_page_cap() {
        let fresh = page("Q2-NEW");
        assert_eq!(
            next_page(&[page("Q2-A")], Some(fresh.as_str()), MAX_PAGES),
            PageStep::Next(fresh.clone())
        );

        let full: Vec<reqwest::Url> = (0..MAX_PAGES).map(|i| page(&i.to_string())).collect();
        assert_eq!(
            next_page(&full, Some(fresh.as_str()), MAX_PAGES),
            PageStep::Stop(Stop::PageCap)
        );
        assert_eq!(
            next_page(&full[..MAX_PAGES - 1], Some(fresh.as_str()), MAX_PAGES),
            PageStep::Next(fresh)
        );
        assert_eq!(
            next_page(&[], Some("not a url"), MAX_PAGES),
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
        assert_eq!(
            next_page(std::slice::from_ref(&a), None, MAX_PAGES),
            PageStep::Done
        );

        let full: Vec<reqwest::Url> = (0..MAX_PAGES).map(|i| page(&i.to_string())).collect();
        for abandoned in [
            next_page(std::slice::from_ref(&a), Some(a.as_str()), MAX_PAGES),
            next_page(&full, Some(fresh.as_str()), MAX_PAGES),
            next_page(&[], Some("not a url"), MAX_PAGES),
        ] {
            assert_ne!(abandoned, PageStep::Done);
            assert!(matches!(abandoned, PageStep::Stop(_)), "{abandoned:?}");
        }
    }

    /// ADR-164 決定 19. A 200 that is not an array used to become one item no parser can read, so
    /// it read as a listing that was complete and empty — every stored device marked missing by the
    /// sync, and a collect counted as answered. It is the stop both readers already refuse.
    #[test]
    fn a_body_that_is_not_an_array_is_not_a_page() {
        for body in [
            r#"{"errors":["something went wrong"]}"#,
            r#"{"items":[{"serial":"Q2-A"}],"meta":{}}"#,
            r#""ok""#,
            "null",
            "42",
            "",
            "<html>maintenance</html>",
        ] {
            assert_eq!(
                page_items(body, Shape::Array),
                Err(Stop::Malformed),
                "{body}"
            );
        }
        assert!(Stop::Malformed.fails_a_collect());
    }

    /// The accept side: an array is its items, and an empty one is an answer — an organization
    /// may hold no devices, and that must not become a failure.
    #[test]
    fn an_array_body_is_its_items_and_an_empty_one_is_still_an_answer() {
        let items = page_items(r#"[{"serial":"Q2-A"},{"serial":"Q2-B"}]"#, Shape::Array)
            .expect("two items");
        assert_eq!(items.len(), 2);
        assert_eq!(items[1]["serial"], "Q2-B");
        assert_eq!(page_items("[]", Shape::Array), Ok(Vec::new()));
    }

    /// ADR-167 決定 2. Two of the switch-port listings answer `{items, meta}`: that is their page,
    /// and the rule stays as strict as 決定 19's — a bare array where the envelope belongs, or an
    /// envelope with no `items` array, is `Malformed`, never an empty listing.
    #[test]
    fn an_envelope_is_a_page_only_where_the_listing_answers_one() {
        let items = page_items(
            r#"{"items":[{"serial":"Q2-A"}],"meta":{"counts":{"items":{"total":1,"remaining":0}}}}"#,
            Shape::Items,
        )
        .expect("one item");
        assert_eq!(items.len(), 1);
        assert_eq!(page_items(r#"{"items":[]}"#, Shape::Items), Ok(Vec::new()));
        for body in [
            r#"[{"serial":"Q2-A"}]"#,
            r#"{"meta":{}}"#,
            r#"{"items":{"serial":"Q2-A"}}"#,
            r#"{"items":null}"#,
            r#"{"errors":["nope"]}"#,
            "",
        ] {
            assert_eq!(
                page_items(body, Shape::Items),
                Err(Stop::Malformed),
                "{body}"
            );
        }
        // And the other way round: every listing read before ADR-167 still refuses an envelope.
        assert_eq!(
            page_items(r#"{"items":[{"serial":"Q2-A"}]}"#, Shape::Array),
            Err(Stop::Malformed)
        );
    }

    /// 決定 3's page cap is the session's, not a constant's: a switch-port listing runs past 50.
    #[test]
    fn the_page_cap_is_the_one_the_session_was_given() {
        let fresh = page("Q2-NEW");
        let fifty: Vec<reqwest::Url> = (0..MAX_PAGES).map(|i| page(&i.to_string())).collect();
        assert_eq!(
            next_page(&fifty, Some(fresh.as_str()), SWITCH_PORT_MAX_PAGES),
            PageStep::Next(fresh.clone())
        );
        assert_eq!(
            next_page(&fifty, Some(fresh.as_str()), MAX_PAGES),
            PageStep::Stop(Stop::PageCap)
        );
        assert_eq!(
            MerakiFetchError::from(Stop::Budget),
            MerakiFetchError::Truncated
        );
        assert!(!Stop::Budget.fails_a_collect());
    }

    /// 決定 6. The bucket read ended at least twelve minutes ago, lies on the five-minute grid, and
    /// is exactly one bucket long.
    #[test]
    fn the_usage_bucket_is_the_newest_one_the_dashboard_has_had_time_to_fill() {
        // 12:00:00 UTC on some day: the newest bucket that had ended by 11:48 is 11:40–11:45.
        let noon = 1_790_078_400;
        assert_eq!(switch_usage_bucket(noon), (noon - 1200, noon - 900));
        for now in [noon, noon + 1, noon + 299, noon + 300, noon + 4_321] {
            let (t0, t1) = switch_usage_bucket(now);
            assert_eq!(t1 - t0, 300, "{now}");
            assert_eq!(t1 % 300, 0, "{now}");
            assert!(
                now - t1 >= 720,
                "{now}: the bucket ended only {}s ago",
                now - t1
            );
            assert!(now - t1 < 720 + 300, "{now}: a newer settled bucket exists");
        }
        // A clock at the epoch does not underflow.
        assert_eq!(switch_usage_bucket(0), (0, 0));
    }

    /// The shape measured on a real organization, with fake identities: a status row per switch,
    /// every port listed.
    fn status_row(serial: &str, ports: &[(&str, &str, bool, &str, &str)]) -> Value {
        serde_json::json!({
            "serial": serial,
            "network": { "id": "N_1" },
            "ports": ports.iter().map(|(id, status, enabled, speed, duplex)| serde_json::json!({
                "portId": id, "status": status, "enabled": enabled,
                "speed": speed, "duplex": duplex, "isUplink": false,
                "errors": [], "warnings": [],
            })).collect::<Vec<_>>(),
        })
    }

    fn sample_of(data: &[DeviceDatum], serial: &str, metric: &str, ifindex: u32) -> Option<f64> {
        data.iter()
            .find(|d| {
                d.serial == serial && d.sample.metric == metric && d.sample.ifindex == Some(ifindex)
            })
            .map(|d| d.sample.value)
    }

    #[test]
    fn a_switch_ports_status_speed_and_duplex_read_as_an_snmp_switchs_do() {
        let rows = [status_row(
            "Q2SW-0001",
            &[
                ("1", "Connected", true, "1 Gbps", "full"),
                ("2", "Disconnected", true, "", ""),
                ("3", "Disconnected", false, "", ""),
                ("1_MA-MOD-8X10G_1", "Connected", true, "10 Gbps", "full"),
                ("5", "Something new", true, "100 Mbps", "half"),
            ],
        )];
        let (data, spine) = parse_switch_port_statuses(&rows);
        let at = |metric, ifindex| sample_of(&data, "Q2SW-0001", metric, ifindex);
        assert_eq!(at("if_oper_status", 1), Some(1.0));
        assert_eq!(at("if_oper_status", 2), Some(2.0));
        assert_eq!(at("if_admin_status", 2), Some(1.0));
        assert_eq!(at("if_admin_status", 3), Some(2.0));
        assert_eq!(at("if_high_speed", 1), Some(1000.0));
        assert_eq!(at("if_high_speed", 2), None, "no link, no speed");
        // A word this build does not know says nothing about the link — the port is still listed.
        assert_eq!(at("if_oper_status", 5), None);
        assert_eq!(at("if_high_speed", 5), Some(100.0));

        let module = switch_port_ifindex("1_MA-MOD-8X10G_1");
        assert_eq!(at("if_high_speed", module), Some(10_000.0));

        let ports = &spine["Q2SW-0001"];
        assert_eq!(ports.len(), 5);
        assert_eq!(ports[&1].port_id, "1");
        assert_eq!(ports[&1].speed_bps, Some(1_000_000_000));
        assert_eq!(ports[&1].duplex, Some(Duplex::Full));
        assert_eq!(ports[&5].duplex, Some(Duplex::Half));
        assert_eq!(ports[&2].speed_bps, None);
        assert_eq!(ports[&2].duplex, None);
        assert_eq!(ports[&module].port_id, "1_MA-MOD-8X10G_1");
        assert!(
            ports.values().all(|p| p.alias.is_none()),
            "names are read separately"
        );
    }

    /// 決定 14. A switch that is down is still listed, every port `Disconnected`; reporting that
    /// would raise a port-down alert per port beside the switch's own node-down one.
    #[test]
    fn a_switch_with_no_port_connected_reports_no_ports() {
        let rows = [
            status_row(
                "Q2SW-DOWN",
                &[
                    ("1", "Disconnected", true, "", ""),
                    ("2", "Disconnected", true, "", ""),
                ],
            ),
            status_row("Q2SW-UP", &[("1", "Connected", true, "1 Gbps", "full")]),
            serde_json::json!({ "serial": "Q2SW-EMPTY", "ports": [] }),
        ];
        let (data, spine) = parse_switch_port_statuses(&rows);
        assert!(data.iter().all(|d| d.serial == "Q2SW-UP"));
        assert_eq!(spine.keys().collect::<Vec<_>>(), ["Q2SW-UP"]);
    }

    fn usage_row(serial: &str, ports: &[(&str, Value)]) -> Value {
        serde_json::json!({
            "serial": serial,
            "network": { "id": "N_1" },
            "ports": ports.iter().map(|(id, intervals)| serde_json::json!({
                "portId": id, "intervals": intervals,
            })).collect::<Vec<_>>(),
        })
    }

    fn interval(end: &str, down: Value, up: Value) -> Value {
        serde_json::json!({
            "startTs": "2026-09-22T11:40:00.000000Z", "endTs": end,
            "data": { "usage": { "total": 1, "upstream": 1, "downstream": 1 } },
            "bandwidth": { "usage": { "total": 0, "upstream": up, "downstream": down } },
        })
    }

    #[test]
    fn a_ports_usage_is_downstream_in_and_upstream_out_in_bits_per_second() {
        let (_, spine) = parse_switch_port_statuses(&[status_row(
            "Q2SW-0001",
            &[
                ("1", "Connected", true, "1 Gbps", "full"),
                ("2", "Connected", true, "1 Gbps", "full"),
                ("3", "Connected", true, "1 Gbps", "full"),
                ("4", "Connected", true, "1 Gbps", "full"),
            ],
        )]);
        let end = "2026-09-22T11:45:00.000000Z";
        let rows = [
            usage_row(
                "Q2SW-0001",
                &[
                    (
                        "1",
                        serde_json::json!([interval(
                            end,
                            serde_json::json!(16.1),
                            serde_json::json!(32.3)
                        )]),
                    ),
                    // One direction only: neither is stored.
                    (
                        "2",
                        serde_json::json!([interval(end, serde_json::json!(5), Value::Null)]),
                    ),
                    // Two intervals: the newer is read.
                    (
                        "3",
                        serde_json::json!([
                            interval(
                                "2026-09-22T11:50:00.000000Z",
                                serde_json::json!(2),
                                serde_json::json!(1)
                            ),
                            interval(end, serde_json::json!(9), serde_json::json!(9)),
                        ]),
                    ),
                    // A port the status listing never named.
                    (
                        "9",
                        serde_json::json!([interval(
                            end,
                            serde_json::json!(1),
                            serde_json::json!(1)
                        )]),
                    ),
                ],
            ),
            // A switch not on the spine.
            usage_row(
                "Q2SW-OFF",
                &[(
                    "1",
                    serde_json::json!([interval(end, serde_json::json!(1), serde_json::json!(1))]),
                )],
            ),
        ];
        let data = parse_switch_port_usage(&rows, &spine);
        let at = |metric, ifindex| sample_of(&data, "Q2SW-0001", metric, ifindex);
        assert_eq!(at(METRIC_MERAKI_PORT_IN_BPS, 1), Some(16_100.0));
        assert_eq!(at(METRIC_MERAKI_PORT_OUT_BPS, 1), Some(32_300.0));
        assert_eq!(at(METRIC_MERAKI_PORT_IN_BPS, 2), None);
        assert_eq!(at(METRIC_MERAKI_PORT_OUT_BPS, 2), None);
        assert_eq!(at(METRIC_MERAKI_PORT_IN_BPS, 3), Some(2_000.0));
        // Port 4 was not in the bucket: no reading, not a zero.
        assert_eq!(at(METRIC_MERAKI_PORT_IN_BPS, 4), None);
        assert_eq!(at(METRIC_MERAKI_PORT_IN_BPS, 9), None);
        assert!(data.iter().all(|d| d.serial == "Q2SW-0001"));
    }

    #[test]
    fn a_ports_configured_name_becomes_its_alias_and_a_missing_one_clears_it() {
        let (_, mut spine) = parse_switch_port_statuses(&[status_row(
            "Q2SW-0001",
            &[
                ("1", "Connected", true, "1 Gbps", "full"),
                ("2", "Connected", true, "1 Gbps", "full"),
                ("3", "Disconnected", true, "", ""),
            ],
        )]);
        let rows = [
            serde_json::json!({ "serial": "Q2SW-0001", "ports": [
                { "portId": "1", "name": "uplink to core" },
                { "portId": "2", "name": null },
                { "portId": "7", "name": "not a port the statuses named" },
            ]}),
            serde_json::json!({ "serial": "Q2SW-OFF", "ports": [{ "portId": "1", "name": "x" }] }),
        ];
        name_switch_ports(&rows, &mut spine);
        let ports = &spine["Q2SW-0001"];
        assert_eq!(ports[&1].alias.as_deref(), Some("uplink to core"));
        assert_eq!(ports[&2].alias.as_deref(), Some(""));
        assert_eq!(ports[&3].alias, None, "not in the answer: nothing said");
        assert!(!ports.contains_key(&7));
        assert!(!spine.contains_key("Q2SW-OFF"));
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

    /// ADR-164 決定 24: `failed` and `not connected` are different numbers, and the failed flag is
    /// on every uplink row — 0 on the healthy ones — so an alert on it always has something to
    /// close on.
    #[test]
    fn uplink_statuses_emit_a_status_and_a_failed_flag_per_uplink() {
        let items = vec![json!({
            "serial": "Q2-A", "networkId": "N_1",
            "highAvailability": {"enabled": true, "role": "primary"},
            "uplinks": [
                {"interface": "wan1", "status": "active"},
                {"interface": "wan2", "status": "failed"},
                {"interface": "cellular", "status": "not connected"}
            ]
        })];
        let obs = fold(parse_uplink_statuses(&items));
        let value = |metric: &str, ifindex: u32| {
            obs[0]
                .samples
                .iter()
                .find(|s| s.metric == metric && s.ifindex == Some(ifindex))
                .map(|s| s.value)
        };
        assert_eq!(value(METRIC_MERAKI_UPLINK_STATUS, 1), Some(2.0));
        assert_eq!(value(METRIC_MERAKI_UPLINK_STATUS, 2), Some(-1.0));
        assert_eq!(value(METRIC_MERAKI_UPLINK_STATUS, 3), Some(0.0));
        assert_eq!(value(METRIC_MERAKI_UPLINK_FAILED, 1), Some(0.0));
        assert_eq!(value(METRIC_MERAKI_UPLINK_FAILED, 2), Some(1.0));
        assert_eq!(value(METRIC_MERAKI_UPLINK_FAILED, 3), Some(0.0));
        assert_eq!(obs[0].samples.len(), 6);
    }

    /// ADR-164 決定 26: a role only while the warm spare is enabled.
    #[test]
    fn ha_roles_are_read_only_from_an_enabled_pair() {
        let rows = vec![
            json!({"serial": "Q2-P", "highAvailability": {"enabled": true, "role": "primary"}}),
            json!({"serial": "Q2-S", "highAvailability": {"enabled": true, "role": "spare"}}),
            json!({"serial": "Q2-1", "highAvailability": {"enabled": false, "role": "primary"}}),
            json!({"serial": "Q2-X", "highAvailability": {"enabled": true, "role": "standby"}}),
            json!({"serial": "Q2-N"}),
            json!({"highAvailability": {"enabled": true, "role": "primary"}}),
        ];
        assert_eq!(
            parse_ha_roles(&rows),
            vec![
                ("Q2-P".to_owned(), Some(MerakiHaRole::Primary)),
                ("Q2-S".to_owned(), Some(MerakiHaRole::Spare)),
                ("Q2-1".to_owned(), None),
                ("Q2-X".to_owned(), None),
                ("Q2-N".to_owned(), None),
            ]
        );
    }

    /// ADR-164 決定 25, as a table: who counts on whose Auto VPN line.
    #[test]
    fn vpn_statuses_count_each_mxs_hubs_and_leave_out_a_peer_that_is_itself_down() {
        let row = |net: &str, serial: &str, status: &str, mode: &str, peers: &[(&str, &str)]| {
            json!({
                "networkId": net, "deviceSerial": serial, "deviceStatus": status, "vpnMode": mode,
                "merakiVpnPeers": peers.iter()
                    .map(|(n, r)| json!({"networkId": n, "reachability": r}))
                    .collect::<Vec<_>>()
            })
        };
        let items = vec![
            // Two meshed hubs, and a third that is itself down.
            row(
                "H1",
                "Q2-H1",
                "online",
                "hub",
                &[
                    ("H2", "reachable"),
                    ("S1", "reachable"),
                    ("S2", "unreachable"),
                    ("S3", "unreachable"),
                ],
            ),
            row("H2", "Q2-H2", "online", "hub", &[("H1", "reachable")]),
            row("H3", "Q2-H3", "dormant", "hub", &[]),
            // Both hubs reached.
            row(
                "S1",
                "Q2-S1",
                "online",
                "spoke",
                &[("H1", "reachable"), ("H2", "reachable")],
            ),
            // One of two lost — and `alerting` is up.
            row(
                "S2",
                "Q2-S2",
                "alerting",
                "spoke",
                &[("H1", "unreachable"), ("H2", "reachable")],
            ),
            // A down MX's row is stale: nothing at all.
            row(
                "S3",
                "Q2-S3",
                "dormant",
                "spoke",
                &[("H1", "unreachable"), ("H2", "unreachable")],
            ),
            // Its unreachable hub is down: that hub is left out, the other one counts.
            row(
                "S4",
                "Q2-S4",
                "online",
                "spoke",
                &[("H3", "unreachable"), ("H1", "reachable")],
            ),
            // Its only hub is down: no hub counted, so nothing — never a guessed "fine".
            row("S5", "Q2-S5", "online", "spoke", &[("H3", "unreachable")]),
            // None reached; a peer with no row of its own and an unknown word say nothing.
            row(
                "S6",
                "Q2-S6",
                "online",
                "spoke",
                &[
                    ("H1", "unreachable"),
                    ("H2", "unreachable"),
                    ("HX", "unreachable"),
                    ("H1", "sideways"),
                ],
            ),
        ];
        let obs = fold(parse_vpn_statuses(&items, &Watched::new(&[])));
        let get = |serial: &str, metric: &str| {
            obs.iter()
                .find(|o| o.serial == serial)
                .and_then(|o| o.samples.iter().find(|s| s.metric == metric))
                .map(|s| s.value)
        };
        let hubs = |serial: &str| {
            (
                get(serial, METRIC_MERAKI_VPN_HUBS_REACHABLE),
                get(serial, METRIC_MERAKI_VPN_HUBS_UNREACHABLE),
                get(serial, METRIC_MERAKI_VPN_HUBS_UNREACHABLE_PCT),
            )
        };
        assert_eq!(hubs("Q2-S1"), (Some(2.0), Some(0.0), Some(0.0)));
        assert_eq!(hubs("Q2-S2"), (Some(1.0), Some(1.0), Some(50.0)));
        assert_eq!(hubs("Q2-S4"), (Some(1.0), Some(0.0), Some(0.0)));
        assert_eq!(hubs("Q2-S6"), (Some(0.0), Some(2.0), Some(100.0)));
        assert_eq!(
            hubs("Q2-H1"),
            (Some(1.0), Some(0.0), Some(0.0)),
            "hubs are meshed"
        );
        // A hub also counts its spokes it does not reach — S2 (up), never S3 (down).
        assert_eq!(
            get("Q2-H1", METRIC_MERAKI_VPN_SPOKES_UNREACHABLE),
            Some(1.0)
        );
        assert_eq!(
            get("Q2-H2", METRIC_MERAKI_VPN_SPOKES_UNREACHABLE),
            Some(0.0)
        );
        assert_eq!(
            get("Q2-S1", METRIC_MERAKI_VPN_SPOKES_UNREACHABLE),
            None,
            "not a hub"
        );
        for silent in ["Q2-S3", "Q2-S5", "Q2-H3"] {
            assert!(
                obs.iter().all(|o| o.serial != silent),
                "{silent} said something"
            );
        }

        // Watching one network keeps that MX only — but its peers are still judged by their own
        // rows, which are not watched.
        let watched = vec!["S2".to_owned()];
        let obs = fold(parse_vpn_statuses(&items, &Watched::new(&watched)));
        assert_eq!(
            obs.iter().map(|o| o.serial.as_str()).collect::<Vec<_>>(),
            ["Q2-S2"]
        );
        assert_eq!(
            obs[0]
                .samples
                .iter()
                .find(|s| s.metric == METRIC_MERAKI_VPN_HUBS_UNREACHABLE_PCT)
                .map(|s| s.value),
            Some(50.0)
        );
    }

    #[test]
    fn the_usage_window_is_the_collect_interval_within_what_the_dashboard_accepted() {
        assert_eq!(usage_window(0), 300, "a job that does not say its interval");
        assert_eq!(usage_window(299), 300);
        assert_eq!(usage_window(300), 300);
        assert_eq!(
            usage_window(1_800),
            1_800,
            "the traffic tier's default cadence"
        );
        assert_eq!(usage_window(86_400), 86_400);
        assert_eq!(usage_window(90_000), 86_400);
    }

    #[test]
    fn uplink_usage_becomes_an_average_rate_per_uplink() {
        // Two networks: one MX with three uplinks (one of them idle, one an unknown interface),
        // and a second MX whose byte counts arrive as strings.
        let items = vec![
            json!({"networkId": "N_1", "name": "site-a", "byUplink": [
                {"serial": "Q2-A", "interface": "wan1", "sent": 1_125_000, "received": 2_250_000},
                {"serial": "Q2-A", "interface": "wan2", "sent": 0, "received": 0},
                {"serial": "Q2-A", "interface": "cellular", "sent": 450, "received": 900},
                {"serial": "Q2-A", "interface": "eth7", "sent": 1, "received": 1}
            ]}),
            json!({"networkId": "N_2", "name": "site-b", "byUplink": [
                {"serial": "Q2-B", "interface": "wan1", "sent": "1800", "received": "3600"}
            ]}),
        ];
        let obs = fold(parse_uplinks_usage(&items, 1_800));
        let rate = |serial: &str, metric: &str, ifindex: u32| {
            obs.iter()
                .find(|o| o.serial == serial)
                .and_then(|o| {
                    o.samples
                        .iter()
                        .find(|s| s.metric == metric && s.ifindex == Some(ifindex))
                })
                .map(|s| s.value)
        };
        // 1,125,000 bytes over 1,800 s = 625 B/s = 5,000 bit/s.
        assert_eq!(
            rate("Q2-A", METRIC_MERAKI_UPLINK_SENT_BPS, 1),
            Some(5_000.0)
        );
        assert_eq!(
            rate("Q2-A", METRIC_MERAKI_UPLINK_RECV_BPS, 1),
            Some(10_000.0)
        );
        assert_eq!(
            rate("Q2-A", METRIC_MERAKI_UPLINK_SENT_BPS, 2),
            Some(0.0),
            "idle is a reading"
        );
        assert_eq!(rate("Q2-A", METRIC_MERAKI_UPLINK_RECV_BPS, 3), Some(4.0));
        assert_eq!(rate("Q2-B", METRIC_MERAKI_UPLINK_SENT_BPS, 1), Some(8.0));
        assert_eq!(rate("Q2-B", METRIC_MERAKI_UPLINK_RECV_BPS, 1), Some(16.0));
        let a = obs.iter().find(|o| o.serial == "Q2-A").unwrap();
        assert_eq!(
            a.samples.len(),
            6,
            "eth7 is skipped, not given an invented row"
        );
        let mut uplinks: Vec<_> = a
            .uplinks
            .iter()
            .map(|u| (u.ifindex, u.name.as_str()))
            .collect();
        uplinks.sort_unstable();
        assert_eq!(uplinks, [(1, "WAN1"), (2, "WAN2"), (3, "cellular")]);
    }

    #[test]
    fn uplink_usage_skips_what_it_cannot_place() {
        let items = vec![
            json!({"networkId": "N_1"}),
            json!({"networkId": "N_1", "byUplink": [
                {"interface": "wan1", "sent": 1, "received": 1},
                {"serial": "Q2-A", "sent": 1, "received": 1},
                {"serial": "Q2-A", "interface": "wan1", "sent": "n/a", "received": null}
            ]}),
        ];
        assert!(parse_uplinks_usage(&items, 300).is_empty());
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

    #[test]
    fn an_mx_wan_address_is_never_taken_for_its_lan_address() {
        // The shape every MX has on the real Dashboard: no `lanIp`, both WAN addresses.
        let d = parse_device_info(&json!({
            "serial": "Q2-MX", "name": "edge-fw", "model": "MX67",
            "productType": "appliance", "networkId": "N_1",
            "wan1Ip": "198.51.100.20", "wan2Ip": "203.0.113.9"
        }))
        .unwrap();
        assert_eq!(
            d.lan_ip, None,
            "ADR-164 決定 28: a WAN address files nothing"
        );
    }

    #[test]
    fn a_networks_vlans_become_one_address_each() {
        let rows = vec![
            json!({"id": 10, "networkId": "N_1", "subnet": "10.1.0.0/24", "applianceIp": "10.1.0.1"}),
            json!({"id": "20", "networkId": "N_1", "applianceIp": "10.2.0.1"}),
            // No address: nothing to take from this VLAN.
            json!({"id": 30, "networkId": "N_1"}),
            json!({"networkId": "N_1", "applianceIp": "10.4.0.1"}),
            json!("not a vlan"),
        ];
        assert_eq!(
            parse_vlan_lans(&rows),
            vec![
                MerakiLanAddress {
                    vlan_id: Some(10),
                    appliance_ip: "10.1.0.1".into()
                },
                MerakiLanAddress {
                    vlan_id: Some(20),
                    appliance_ip: "10.2.0.1".into()
                },
                // An id that cannot be read keeps the address; core orders it last.
                MerakiLanAddress {
                    vlan_id: None,
                    appliance_ip: "10.4.0.1".into()
                },
            ]
        );
    }

    #[test]
    fn a_single_lan_is_one_address_with_no_vlan_number() {
        assert_eq!(
            parse_single_lan(&json!({"subnet": "10.9.0.0/24", "applianceIp": "10.9.0.1"})),
            Some(MerakiLanAddress {
                vlan_id: None,
                appliance_ip: "10.9.0.1".into()
            })
        );
        assert_eq!(parse_single_lan(&json!({"subnet": "10.9.0.0/24"})), None);
    }

    #[test]
    fn a_settings_document_is_one_item_and_only_where_one_is_expected() {
        let one = page_items(r#"{"applianceIp":"10.0.0.1"}"#, Shape::Object).unwrap();
        assert_eq!(one.len(), 1);
        // The other way round stays as strict as ADR-164 決定 19 made it.
        assert_eq!(page_items("[]", Shape::Object), Err(Stop::Malformed));
        assert_eq!(
            page_items(r#"{"applianceIp":"10.0.0.1"}"#, Shape::Array),
            Err(Stop::Malformed)
        );
    }

    // ── What a collect reports (ADR-164 決定 18) ─────────────────────────────────────────────

    fn one_observation() -> MerakiObservation {
        MerakiObservation {
            serial: "Q2-A".into(),
            samples: Vec::new(),
            uplinks: Vec::new(),
            ports: Vec::new(),
            radios: Vec::new(),
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
            failed_listing: None,
        };
        assert_eq!(
            nothing.failure(),
            Some(MerakiFetchError::Network),
            "an outage read as a collect that simply found no devices"
        );

        let partial = MerakiCollected {
            observations: vec![one_observation()],
            stopped: Some(MerakiFetchError::RateLimited),
            failed_listing: None,
        };
        assert_eq!(
            partial.failure(),
            None,
            "pages 1 and 2 arrived: the Dashboard answered, and that is what the alert asks"
        );

        let empty_organization = MerakiCollected::default();
        assert_eq!(empty_organization.failure(), None);

        // ADR-164 決定 25: one listing of a tier failed while another answered — that is a
        // failure now, and it says which listing.
        let one_listing = MerakiCollected {
            observations: vec![one_observation()],
            stopped: Some(MerakiFetchError::Status(400)),
            failed_listing: Some((
                MerakiListing::ApplianceVpnStatuses,
                MerakiFetchError::Status(400),
            )),
        };
        assert_eq!(one_listing.failure(), Some(MerakiFetchError::Status(400)));
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

    /// ADR-168 決定 5, the case the recorded organization did not have: two radios in one band. The
    /// utilization is per band and lands on the band's first slot; the SSID read numbers the radios
    /// the way a controller-walked access point's are (second 5 GHz radio = 12), so the second gets
    /// its channel and power with no utilization of its own. A band word nobody knows is dropped.
    #[test]
    fn a_second_radio_in_a_band_takes_the_next_slot_and_shares_no_utilization() {
        let util = serde_json::json!([{
            "serial": "Q2AP-0001",
            "network": {"id": "N_1"},
            "byBand": [
                {"band": "5", "total": {"percentage": 12.5}, "nonWifi": {"percentage": 1.0}},
                {"band": "60", "total": {"percentage": 99.0}, "nonWifi": {"percentage": 0.0}},
            ],
        }]);
        let mut radios = parse_channel_utilization(util.as_array().unwrap());
        assert_eq!(
            radios["Q2AP-0001"].keys().copied().collect::<Vec<_>>(),
            [2],
            "the unknown band was guessed at"
        );

        let radio = |index: &str, channel: u32| {
            serde_json::json!({
                "ssid": {"number": 0, "enabled": true},
                "radio": {"isBroadcasting": true, "band": "5", "channel": channel,
                          "power": 14, "index": index},
            })
        };
        let ssids = serde_json::json!([{
            "serial": "Q2AP-0001",
            "network": {"id": "N_1"},
            // Listed out of order on purpose: the slot follows the radio's index, not the row.
            "basicServiceSets": [radio("2", 149), radio("1", 36)],
        }]);
        let data = apply_ssid_statuses(ssids.as_array().unwrap(), &mut radios);
        let got: Vec<_> = radios["Q2AP-0001"]
            .values()
            .map(|r| (r.slot, r.channel, r.channel_util_pct))
            .collect();
        assert_eq!(got, [(2, Some(36), Some(12.5)), (12, Some(149), None)]);
        assert_eq!(
            data.iter()
                .find(|d| d.sample.metric == METRIC_WLAN_AP_SSID_COUNT)
                .map(|d| d.sample.value),
            Some(1.0),
            "one SSID on two radios is one SSID"
        );
    }
}
