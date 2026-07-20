// SPDX-License-Identifier: AGPL-3.0-only
//! Real Cisco Meraki Dashboard API transport (`reqwest` over rustls) — strictly **READ-ONLY**.
//!
//! The Dashboard API is org-scoped and bulk: one paged GET returns data for many devices. A
//! [`collect`] call pages one [`MerakiTier`] of endpoints for an organization and returns raw
//! per-device observations; the poller fans those out to per-node results. Control-plane helpers
//! ([`list_organizations`] / [`list_networks`] / [`list_devices`]) back the operator-initiated
//! import wizard in core. All of it lives here so every byte of Meraki I/O goes through one place.
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

use crate::{MerakiCollectSpec, MerakiObservation, MerakiSample, MerakiUplink, TransportError};
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
    async fn get_paged(
        &mut self,
        path: &str,
        query: &[(&str, String)],
        per_page: u32,
    ) -> Result<Vec<Value>, TransportError> {
        let mut url = self
            .base
            .join(path)
            .map_err(|e| io(format!("invalid meraki path {path}: {e}")))?;
        {
            let mut qp = url.query_pairs_mut();
            for (k, v) in query {
                qp.append_pair(k, v);
            }
            qp.append_pair("perPage", &per_page.to_string());
        }

        let mut items: Vec<Value> = Vec::new();
        let mut pages = 0usize;
        let mut rate_retries = 0u32;

        loop {
            // Host allow-list on EVERY request (initial URL + each next-link).
            let host = url.host_str().unwrap_or_default();
            if !is_meraki_api_host(host) {
                return Err(io(
                    "meraki request host is not allow-listed (refusing to send key)",
                ));
            }

            self.pace().await;
            let resp = match self.client.get(url.clone()).send().await {
                Ok(r) => r,
                Err(e) => {
                    tracing::debug!(error = %e, "meraki request did not complete; returning partial");
                    break;
                }
            };
            let status = resp.status();

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                rate_retries += 1;
                if rate_retries > MAX_RATE_LIMIT_RETRIES {
                    tracing::warn!("meraki 429 budget exhausted; returning partial results");
                    break;
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
                return Err(io(format!("meraki api auth failed ({})", status.as_u16())));
            }
            if !status.is_success() {
                tracing::debug!(
                    status = status.as_u16(),
                    "meraki non-success; returning partial"
                );
                break;
            }
            rate_retries = 0;

            let next = next_link(&resp);
            let body = resp
                .text()
                .await
                .map_err(|e| io(format!("meraki response read failed: {e}")))?;
            match serde_json::from_str::<Value>(&body) {
                Ok(Value::Array(arr)) => items.extend(arr),
                Ok(other) => items.push(other),
                Err(e) => return Err(io(format!("meraki json parse failed: {e}"))),
            }
            pages += 1;

            match next {
                Some(n) if pages < MAX_PAGES => {
                    url = reqwest::Url::parse(&n)
                        .map_err(|e| io(format!("invalid meraki next link: {e}")))?;
                }
                Some(_) => {
                    tracing::warn!(max = MAX_PAGES, "meraki pagination truncated at page cap");
                    break;
                }
                None => break,
            }
        }
        Ok(items)
    }
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
) -> Result<Vec<MerakiObservation>, TransportError> {
    let mut session = Session::new(&spec.base_url, &spec.api_key, spec.target_rps, timeout)?;
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
            let items = session.get_paged(&path, &net_query, spec.per_page).await?;
            data.extend(parse_availability(&items));
        }
        MerakiTier::Uplink => {
            let loss_path = format!(
                "{API_PREFIX}/organizations/{}/devices/uplinksLossAndLatency",
                spec.org_id
            );
            let mut q = net_query.clone();
            q.push(("timespan", "300".to_owned()));
            let items = session.get_paged(&loss_path, &q, spec.per_page).await?;
            data.extend(parse_uplink_loss_latency(&items));

            let status_path = format!(
                "{API_PREFIX}/organizations/{}/appliance/uplink/statuses",
                spec.org_id
            );
            let items = session
                .get_paged(&status_path, &net_query, spec.per_page)
                .await?;
            data.extend(parse_uplink_statuses(&items));
        }
        MerakiTier::Traffic => {
            let path = format!(
                "{API_PREFIX}/organizations/{}/summary/top/devices/byUsage",
                spec.org_id
            );
            let mut q = net_query.clone();
            q.push(("timespan", "3600".to_owned()));
            let items = session.get_paged(&path, &q, spec.per_page).await?;
            data.extend(parse_traffic(&items));
        }
        // Inventory reconciliation is operator-initiated (control-plane enumerate), not a recurring
        // metric collect — nothing to gather here.
        MerakiTier::Inventory => {}
    }
    Ok(fold(data))
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
            let up = matches!(status.to_ascii_lowercase().as_str(), "online" | "alerting");
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

// ── Control-plane (import wizard) ───────────────────────────────────────────────────────────

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

/// List the networks in an org (`GET /organizations/{orgId}/networks`). Read-only.
pub async fn list_networks(
    base_url: &str,
    api_key: &str,
    org_id: &str,
    timeout: Duration,
) -> Result<Vec<MerakiNetworkInfo>, TransportError> {
    let mut s = Session::new(base_url, api_key, 2.0, timeout)?;
    let path = format!("{API_PREFIX}/organizations/{org_id}/networks");
    let items = s.get_paged(&path, &[], 1000).await?;
    Ok(items
        .iter()
        .filter_map(|it| {
            Some(MerakiNetworkInfo {
                id: it.get("id")?.as_str()?.to_owned(),
                name: it
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            })
        })
        .collect())
}

/// List the devices in an org (`GET /organizations/{orgId}/devices`). Read-only.
pub async fn list_devices(
    base_url: &str,
    api_key: &str,
    org_id: &str,
    timeout: Duration,
) -> Result<Vec<MerakiDeviceInfo>, TransportError> {
    let mut s = Session::new(base_url, api_key, 2.0, timeout)?;
    let path = format!("{API_PREFIX}/organizations/{org_id}/devices");
    let items = s.get_paged(&path, &[], 1000).await?;
    Ok(items.iter().filter_map(parse_device_info).collect())
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
}
