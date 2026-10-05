// SPDX-License-Identifier: AGPL-3.0-only
//! Who gets told — the delivery half of the alert module (ADR-083).
//!
//! Mutes, routing rules, template selection, and the four channels (Webhook / PagerDuty / JSM /
//! Email) with their vendor wire formats and the SSRF guard. Takes a [`super::NotifyAction`] and
//! turns it into an outbound request; **it names no engine type at all**, which is the property
//! that made ADR-083's split provably behaviour-free and the one to preserve.
//!
//! The neighbouring notification modules and what each owns: [`crate::notifications`] the stored
//! channel and routing rows, [`crate::notify_facts`] the facts a template may reference,
//! [`crate::notify_render`] the rendering itself. This module is the dispatcher over them.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use uuid::Uuid;
use yagra_alert::{
    Alert, DeliveryFailure, DispatchOutcome, DispatchReport, Dispatcher, Notification,
    NotifyChannel, NotifyError, RetryPolicy, Subject,
};
use yagra_common::{is_ssrf_blocked, AlertFacts, CheckId, NodeId, NotifyEvent, Severity};

use crate::notification_log::{DeliveryLog, DeliveryRecord};
use crate::notifications::{ChannelConfig, ChannelKind, OpenChannel, RoutingRule};
use crate::notify_facts::{context_for, node_ids_for, AlertFactsSource};
use crate::notify_render::{body_must_be_json, render_with_fallback, ChannelTemplate};

use super::rules::check_id;
use super::NotifyAction;

/// A Webhook [`NotifyChannel`]: POSTs the alert JSON to a configured URL.
pub struct WebhookChannel {
    http: reqwest::Client,
    url: String,
    secrets: Secrets,
}

impl WebhookChannel {
    #[must_use]
    pub fn new(url: String) -> Self {
        // A webhook endpoint that 30x-redirects to a loopback/metadata address is an escalation
        // vector, so core never follows a redirect on the notification path.
        Self {
            http: hardened_client(),
            secrets: Secrets::of_url(&url),
            url,
        }
    }
}

/// Whether a webhook target must be refused (SSRF, runtime/defense-in-depth alongside the API-edge
/// [`crate::api`] check). An IP-literal host is judged directly; a hostname is resolved and refused
/// only if **every** answer is blocked. A DNS failure is *not* treated as blocked — the POST then
/// fails naturally and is reported as a delivery error.
async fn webhook_target_blocked(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return true;
    };
    if let Some(ip) = yagra_common::host_ip(host) {
        return is_ssrf_blocked(ip);
    }
    let port = url
        .port_or_known_default()
        .unwrap_or(if url.scheme() == "https" { 443 } else { 80 });
    match tokio::net::lookup_host((host, port)).await {
        Ok(addrs) => {
            let addrs: Vec<_> = addrs.collect();
            !addrs.is_empty() && addrs.iter().all(|a| is_ssrf_blocked(a.ip()))
        }
        Err(_) => false,
    }
}

#[async_trait]
impl NotifyChannel for WebhookChannel {
    async fn deliver(&self, notification: &Notification) -> Result<(), NotifyError> {
        // SSRF guard at delivery time (the API edge validates the configured URL, but DNS can
        // change between config and delivery): refuse a target whose every resolved address is
        // blocked before any request leaves core.
        if let Ok(url) = reqwest::Url::parse(&self.url) {
            if webhook_target_blocked(&url).await {
                return Err(
                    DeliveryFailure::yagra("webhook target address is not allowed (SSRF)").into(),
                );
            }
        }
        let resp = self
            .http
            .post(&self.url)
            .header("content-type", "application/json")
            .body(notification.payload.clone())
            .send()
            .await
            .map_err(delivery_error)?;
        // No `Retry-After` wait here, unlike the vendor channels: a webhook's 429 is a refusal
        // like any other, and waiting on it would only lengthen ADR-104's worst case.
        if resp.status().is_success() {
            return Ok(());
        }
        Err(refusal(resp, &self.secrets).await.into())
    }
}

/// The dedup identity string sent to lifecycle-aware vendors: PagerDuty `dedup_key` and
/// JSM `alias`. Stable across restarts (check ids are UUIDv5), so a resolve always finds
/// the incident its fire created.
///
/// `pub(crate)` because a notification template exposes it as `{{ dedup_key }}` (ADR-039): an
/// operator correlating what Yagra sent with what the vendor shows needs the same string, and two
/// spellings of it would drift.
pub(crate) fn dedup_string(key: &yagra_alert::DedupKey) -> String {
    // `Subject`'s Display renders a node as a bare UUID, so a node alert's dedup string is
    // byte-identical to what it was before subjects existed — an incident opened by an older
    // core still closes. A pool renders as `pool:<name>`.
    format!(
        "yagra:{}:{}:{}",
        key.subject,
        key.check,
        key.severity.as_str()
    )
}

/// The outbound client every notification channel uses: bounded timeout, **no redirect
/// following** (SSRF). A build failure keeps the no-redirect policy — see [`crate::http::client`],
/// which is where that became true; the fallback that used to be written here followed redirects.
fn hardened_client() -> reqwest::Client {
    crate::http::client(
        std::time::Duration::from_secs(10),
        crate::http::Redirects::None,
    )
}

/// A failed HTTP delivery as the text an operator may read (ADR-192).
///
/// **The URL is taken out first.** reqwest's `Display` names the URL it was sending to, and a
/// webhook URL routinely carries its own secret (`.../hooks/T0/B0/XXXX`): the configured URL is
/// sealed at rest and never returned by the API, so it must not come back through an error message
/// either - nor go into a log line. The source chain is then appended, because the top-level text
/// alone ("error sending request") does not say whether DNS, the TCP connect or TLS was the part
/// that failed, which is the one thing an operator testing a channel needs to know.
///
/// **The host is then taken out of the chain**, because a cause can name it on its own: rustls's
/// "certificate not valid for name \"hooks.example.com\"" is the host the sealed URL points at.
///
/// **Which side failed** (ADR-195 decision 1): a request that could not be built is Yagra's; one
/// that carries a status was answered by the remote; everything else - a timeout, a refused
/// connection, DNS, TLS - never got an answer, which is the network between the two.
fn delivery_error(e: reqwest::Error) -> NotifyError {
    use std::error::Error as _;
    let host = e
        .url()
        .and_then(|u| u.host_str())
        .map(|h| h.trim_start_matches('[').trim_end_matches(']').to_owned())
        .filter(|h| !h.is_empty());
    let status = e.status().map(|s| s.as_u16());
    let builder = e.is_builder();
    let timeout = e.is_timeout();
    let e = e.without_url();
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    if let Some(host) = host {
        text = text.replace(&host, "<host>");
    }
    if timeout && !text.contains("timed out") {
        text.push_str(" (timed out)");
    }
    let failure = if builder {
        DeliveryFailure::yagra(text)
    } else if status.is_some() {
        DeliveryFailure::remote(status, text, None)
    } else {
        DeliveryFailure::network(text)
    };
    failure.into()
}

/// The strings a channel must never let out through a delivery log: its URL, the host it points
/// at, a webhook's path tokens, a vendor key, an SMTP login (ADR-195 decision 2).
///
/// A remote's error body is the reason this exists. Some services echo the request back when they
/// refuse it, and the request carried the key in a header or the routing key in the body.
#[derive(Debug, Clone, Default)]
pub(crate) struct Secrets(Vec<String>);

/// Which path segments are treated as tokens: `/hooks/T0/B0/XXXX`-style URLs put the secret in the
/// path. A segment this long is a token; so is a shorter one of at least [`TOKEN_WITH_DIGIT_MIN`]
/// characters that contains a digit. The API's own words (`v2`, `alerts`, `services`) stay readable.
const TOKEN_MIN: usize = 16;
const TOKEN_WITH_DIGIT_MIN: usize = 8;

fn looks_like_a_token(segment: &str) -> bool {
    let len = segment.chars().count();
    len >= TOKEN_MIN || (len >= TOKEN_WITH_DIGIT_MIN && segment.chars().any(|c| c.is_ascii_digit()))
}

impl Secrets {
    /// The secrets in a URL: the whole URL, its host, its query, and every long path segment.
    pub(crate) fn of_url(url: &str) -> Self {
        let mut out = Self::default();
        out.push(url);
        if let Ok(parsed) = reqwest::Url::parse(url) {
            if let Some(host) = parsed.host_str() {
                out.push(host.trim_start_matches('[').trim_end_matches(']'));
            }
            if let Some(query) = parsed.query() {
                out.push(query);
            }
            if let Some(segments) = parsed.path_segments() {
                for seg in segments {
                    if looks_like_a_token(seg) {
                        out.push(seg);
                    }
                }
            }
        }
        out
    }

    /// The same, plus one more secret (a key, a password).
    pub(crate) fn with(mut self, secret: &str) -> Self {
        self.push(secret);
        self
    }

    fn push(&mut self, secret: &str) {
        let secret = secret.trim();
        if !secret.is_empty() {
            self.0.push(secret.to_owned());
        }
    }

    /// `text` with every secret replaced. Longest first, so a URL is replaced whole before its
    /// host would leave the rest of it behind.
    pub(crate) fn redact(&self, text: &str) -> String {
        let mut secrets: Vec<&String> = self.0.iter().collect();
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        let mut out = text.to_owned();
        for secret in secrets {
            out = out.replace(secret.as_str(), "<redacted>");
        }
        out
    }
}

/// How much of a refusal's body is read off the wire. The rest is never downloaded, so a remote
/// answering with megabytes costs nothing.
const RESPONSE_READ_MAX_BYTES: usize = 4096;
/// How much of it is kept, in characters (ADR-195 decision 2).
pub(crate) const RESPONSE_KEEP_MAX_CHARS: usize = 512;

/// A non-2xx answer as a [`DeliveryFailure`]: the status, and the start of what the remote said,
/// with this channel's secrets taken out.
///
/// The body is where a vendor says *why* - "Key format is not valid", "Invalid routing key" -
/// which is the difference between "JSM refused" and "JSM refused because the key is wrong".
async fn refusal(mut resp: reqwest::Response, secrets: &Secrets) -> DeliveryFailure {
    let status = resp.status();
    let mut body = Vec::new();
    while body.len() < RESPONSE_READ_MAX_BYTES {
        match resp.chunk().await {
            Ok(Some(chunk)) => body.extend_from_slice(&chunk),
            Ok(None) | Err(_) => break,
        }
    }
    body.truncate(RESPONSE_READ_MAX_BYTES);
    DeliveryFailure::remote(
        Some(status.as_u16()),
        format!("unexpected status {status}"),
        response_excerpt(&body, secrets),
    )
}

/// The kept part of a response body: valid text, control characters flattened, secrets taken out,
/// then cut at [`RESPONSE_KEEP_MAX_CHARS`]. Redacting before cutting, so a secret straddling the
/// cut cannot leave its first half behind. `None` for an empty body.
pub(crate) fn response_excerpt(body: &[u8], secrets: &Secrets) -> Option<String> {
    let text: String = String::from_utf8_lossy(body)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let text = secrets.redact(text.trim());
    if text.is_empty() {
        return None;
    }
    Some(truncate_chars(&text, RESPONSE_KEEP_MAX_CHARS))
}

/// Map a vendor API response to the channel result. 429 waits out `Retry-After` (capped
/// at 10s) and then returns `Err` so the dispatcher's retry policy counts the attempt.
/// `also_ok` admits one vendor-specific extra status (e.g. JSM close → 404 = already
/// closed, which must read as success for idempotency).
async fn vendor_response(
    resp: reqwest::Response,
    also_ok: Option<reqwest::StatusCode>,
    secrets: &Secrets,
) -> Result<(), NotifyError> {
    let status = resp.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let wait_secs = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(2);
        let mut failure = refusal(resp, secrets).await;
        "rate limited (429)".clone_into(&mut failure.message);
        tokio::time::sleep(std::time::Duration::from_secs(wait_secs.min(10))).await;
        return Err(failure.into());
    }
    if status.is_success() || also_ok.is_some_and(|s| s == status) {
        return Ok(());
    }
    Err(refusal(resp, secrets).await.into())
}

/// PagerDuty Events API v2 [`NotifyChannel`]: `trigger` on fire, `resolve` on recovery,
/// correlated by `dedup_key`. The routing key is a secret — never logged.
pub struct PagerDutyChannel {
    http: reqwest::Client,
    url: String,
    routing_key: String,
    secrets: Secrets,
}

/// Default (US) Events API v2 endpoint; EU tenants override via the channel config.
const PAGERDUTY_DEFAULT_URL: &str = "https://events.pagerduty.com/v2/enqueue";

impl PagerDutyChannel {
    #[must_use]
    pub fn new(routing_key: String, api_url: Option<String>) -> Self {
        let url = api_url.unwrap_or_else(|| PAGERDUTY_DEFAULT_URL.to_owned());
        Self {
            http: hardened_client(),
            secrets: Secrets::of_url(&url).with(&routing_key),
            url,
            routing_key,
        }
    }

    async fn send_event(
        &self,
        action: &str,
        notification: &Notification,
        with_payload: bool,
    ) -> Result<(), NotifyError> {
        if let Ok(url) = reqwest::Url::parse(&self.url) {
            if webhook_target_blocked(&url).await {
                return Err(DeliveryFailure::yagra(
                    "PagerDuty target address is not allowed (SSRF)",
                )
                .into());
            }
        }
        let body = pagerduty_body(&self.routing_key, action, notification, with_payload);
        let resp = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(delivery_error)?;
        vendor_response(resp, None, &self.secrets).await
    }
}

/// The Events API v2 request body (pure — unit-tested against the wire contract).
fn pagerduty_body(
    routing_key: &str,
    action: &str,
    notification: &Notification,
    with_payload: bool,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "routing_key": routing_key,
        "event_action": action,
        "dedup_key": dedup_string(&notification.dedup_key),
    });
    if with_payload {
        // custom_details carries the full alert JSON (payload is pre-rendered JSON text).
        let mut details: serde_json::Value =
            serde_json::from_str(&notification.payload).unwrap_or(serde_json::Value::Null);
        // The node's effective labels, under a namespaced key so a PagerDuty event rule can match
        // on them — `custom_details.yagra_tags contains JAPAN` (ADR-135 inc. 2).
        //
        // 🚨 **This was an object keyed by tag name until ADR-135 inc. 2 and is now an array.**
        // A rule written against `custom_details.yagra_tags.region` stops matching, the incident
        // is still created, and it routes to the default — no error anywhere. Nothing in this
        // repository can detect that, which is why it is in the release notes; the lab has no
        // PagerDuty channel to observe it on (ADR-083's remnant).
        //
        // ⚠️ **Only when the body is a JSON object.** The body may be an operator's template
        // output, which is theirs; inserting into a scalar or an array would mean replacing what
        // they wrote rather than adding beside it. A template that wants tags in some other shape
        // already has the `{{ tags }}` variable.
        if !notification.tags.is_empty() {
            if let Some(obj) = details.as_object_mut() {
                obj.insert(
                    "yagra_tags".to_owned(),
                    serde_json::json!(notification.tags),
                );
            }
        }
        body["payload"] = serde_json::json!({
            "summary": truncate_chars(&notification.summary, 1024),
            "source": notification.dedup_key.subject.to_string(),
            "severity": notification.severity.as_str(),
            "custom_details": details,
        });
    }
    body
}

#[async_trait]
impl NotifyChannel for PagerDutyChannel {
    async fn deliver(&self, notification: &Notification) -> Result<(), NotifyError> {
        self.send_event("trigger", notification, true).await
    }

    async fn deliver_resolve(&self, notification: &Notification) -> Result<(), NotifyError> {
        // Resolve needs only the dedup_key; PD ignores unknown keys (idempotent).
        self.send_event("resolve", notification, false).await
    }
}

/// JSM Alerts (Opsgenie-compatible) [`NotifyChannel`]: create alert on fire (dedup via
/// `alias`), close-by-alias on recovery. The GenieKey is a secret — never logged.
pub struct JsmChannel {
    http: reqwest::Client,
    api_url: String,
    api_key: String,
    secrets: Secrets,
}

impl JsmChannel {
    #[must_use]
    pub fn new(api_url: String, api_key: String) -> Self {
        let api_url = api_url.trim_end_matches('/').to_owned();
        Self {
            http: hardened_client(),
            // The JSM API URL is a public endpoint and its path is the API's own, so only the host
            // and the key are secret here - but `of_url` costs nothing to apply uniformly.
            secrets: Secrets::of_url(&api_url).with(&api_key),
            api_url,
            api_key,
        }
    }

    async fn guard(&self, url: &str) -> Result<(), NotifyError> {
        if let Ok(url) = reqwest::Url::parse(url) {
            if webhook_target_blocked(&url).await {
                return Err(
                    DeliveryFailure::yagra("JSM target address is not allowed (SSRF)").into(),
                );
            }
        }
        Ok(())
    }
}

#[async_trait]
impl NotifyChannel for JsmChannel {
    async fn deliver(&self, notification: &Notification) -> Result<(), NotifyError> {
        let url = format!("{}/alerts", self.api_url);
        self.guard(&url).await?;
        let resp = self
            .http
            .post(&url)
            .header("authorization", format!("GenieKey {}", self.api_key))
            .json(&jsm_create_body(notification))
            .send()
            .await
            .map_err(delivery_error)?;
        vendor_response(resp, None, &self.secrets).await
    }

    async fn deliver_resolve(&self, notification: &Notification) -> Result<(), NotifyError> {
        let url = jsm_close_url(&self.api_url, notification);
        self.guard(&url).await?;
        let resp = self
            .http
            .post(&url)
            .header("authorization", format!("GenieKey {}", self.api_key))
            .json(&serde_json::json!({ "source": "yagra" }))
            .send()
            .await
            .map_err(delivery_error)?;
        // 404 = no open alert with that alias (already closed / never created) — success,
        // so a resolve is idempotent and never dangles on retry.
        vendor_response(resp, Some(reqwest::StatusCode::NOT_FOUND), &self.secrets).await
    }
}

/// JSM cuts an alert's title (`message`) at this many characters. The template editor shows the cut
/// at the same place (`web/src/pages/templateModel.ts::JSM_MESSAGE_MAX_CHARS`), which
/// `the_editor_cuts_a_jsm_title_where_the_channel_does` pins.
pub(crate) const JSM_MESSAGE_MAX_CHARS: usize = 130;

/// The JSM/Opsgenie create-alert body (pure — unit-tested against the wire contract).
fn jsm_create_body(notification: &Notification) -> serde_json::Value {
    let priority = match notification.severity {
        Severity::Critical => "P1",
        Severity::Warning => "P3",
        Severity::Info => "P5",
    };
    let mut body = serde_json::json!({
        "message": truncate_chars(&notification.summary, JSM_MESSAGE_MAX_CHARS),
        "alias": dedup_string(&notification.dedup_key),
        "priority": priority,
        "description": truncate_chars(&notification.payload, JSM_DESCRIPTION_MAX_CHARS),
        "source": "yagra",
    });
    // JSM's own `tags` field, which its alert policies and routing rules match on natively — so
    // "page the Japan rota for anything tagged JAPAN" is written over there, where the on-call
    // rota already lives (ADR-015, ADR-135 decision 8). The field was simply empty until there was
    // a way to put a label on a node.
    //
    // ✅ **This is the one surface the inc. 2 reshape makes simpler rather than more awkward.**
    // Opsgenie tags have always been a flat list of strings, so the old shape had to flatten a map
    // into `region=JAPAN` first; a label goes in as itself. ⚠️ A JSM policy matching the literal
    // `region=JAPAN` must be changed to match `JAPAN`.
    //
    // Omitted entirely when there are none: an empty array is a field the API has to be told to
    // ignore.
    let tags = jsm_tags(&notification.tags);
    if !tags.is_empty() {
        body["tags"] = serde_json::json!(tags);
    }
    // JSM's "extra properties" table: what the alert is about, one fact per row, which its rules
    // can also match on (ADR-194 decision 3). Sent whether or not the channel has a template.
    if !notification.details.is_empty() {
        let details: serde_json::Map<String, serde_json::Value> = notification
            .details
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        body["details"] = serde_json::Value::Object(details);
    }
    body
}

/// JSM keeps at most this many tags on an alert (the Opsgenie-compatible API's documented limit).
pub(crate) const JSM_TAGS_MAX: usize = 20;
/// ...each at most this many characters long.
pub(crate) const JSM_TAG_MAX_CHARS: usize = 50;
/// JSM's `description` holds at most this many characters.
pub(crate) const JSM_DESCRIPTION_MAX_CHARS: usize = 15_000;

/// The node's tags as JSM can hold them (ADR-194 decision 4): a tag longer than
/// [`JSM_TAG_MAX_CHARS`] is left out, and the first [`JSM_TAGS_MAX`] of the rest are kept.
///
/// ⚠️ **Left out, never shortened.** A shortened tag is a different tag, and the reason these go
/// to JSM at all is that its rules match on them: `TOKYO-NETWORK-…` cut short could page whichever
/// rota owns the prefix. The node's own labels come first in the list (`tagres`), so the ones
/// dropped by the count are inherited ones. What JSM itself does past its limits is undocumented.
fn jsm_tags(tags: &[String]) -> Vec<&str> {
    let fitting: Vec<&str> = tags
        .iter()
        .map(String::as_str)
        .filter(|t| t.chars().count() <= JSM_TAG_MAX_CHARS)
        .collect();
    let too_long = tags.len() - fitting.len();
    let over_count = fitting.len().saturating_sub(JSM_TAGS_MAX);
    if too_long + over_count > 0 {
        metrics::counter!(M_JSM_TAGS_DROPPED).increment((too_long + over_count) as u64);
        tracing::warn!(
            too_long,
            over_count,
            kept = fitting.len().min(JSM_TAGS_MAX),
            "some of the node's tags were not sent to JSM: it holds {JSM_TAGS_MAX} tags of up to \
             {JSM_TAG_MAX_CHARS} characters"
        );
    }
    fitting.into_iter().take(JSM_TAGS_MAX).collect()
}

/// Counter for node tags left off a JSM alert because they would not fit its limits (ADR-194).
const M_JSM_TAGS_DROPPED: &str = "yagra_notification_jsm_tags_dropped_total";

/// The JSM/Opsgenie close-by-alias URL.
///
/// The alias is percent-encoded as one path segment. A node alias is UUID hex, dashes and
/// colons, none of which that encoding touches — so the URL is byte-identical to the one an
/// older core built, and an incident opened before this change still closes.
//
// It stopped being safe to interpolate raw once a pool subject entered the alias: a pool name is
// operator-authored free text and may hold a space or a `/`, which would silently address the
// wrong resource or produce an unparseable URL — and a close that never lands is the dangling
// incident `Dispatcher::dispatch_resolve` exists to prevent. `Url::path_segments_mut` is the url
// crate reqwest already carries; no new dependency.
fn jsm_close_url(api_url: &str, notification: &Notification) -> String {
    let alias = dedup_string(&notification.dedup_key);
    let encoded = reqwest::Url::parse(api_url)
        .ok()
        .and_then(|mut url| {
            url.path_segments_mut().ok()?.pop_if_empty().push(&alias);
            Some(url)
        })
        .and_then(|url| {
            url.path_segments()?
                .next_back()
                .map(std::borrow::ToOwned::to_owned)
        })
        // A non-base or unparseable `api_url` is a misconfiguration the delivery guard already
        // rejects; fall back to the raw alias rather than dropping the close.
        .unwrap_or(alias);
    format!("{api_url}/alerts/{encoded}/close?identifierType=alias")
}

/// Clip to at most `max` characters on a char boundary (vendor field limits).
fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((idx, _)) => text[..idx].to_owned(),
        None => text.to_owned(),
    }
}

/// An email [`NotifyChannel`] over SMTP (`lettre`, async + rustls).
pub struct EmailChannel {
    mailer: lettre::AsyncSmtpTransport<lettre::Tokio1Executor>,
    from: lettre::message::Mailbox,
    to: lettre::message::Mailbox,
    /// The SMTP host and login, kept out of a failure's text (ADR-195). v0.3.43's `/verify` noted
    /// that an SMTP error was the one delivery error not redacted like an HTTP one.
    secrets: Secrets,
}

impl EmailChannel {
    /// Build from explicit SMTP params. Returns `None` if host/from/to are malformed.
    pub fn new(
        host: &str,
        port: Option<u16>,
        from: &str,
        to: &str,
        user: Option<&str>,
        pass: Option<&str>,
    ) -> Option<Self> {
        use lettre::transport::smtp::authentication::Credentials;
        if host.is_empty() {
            return None;
        }
        let from = from.parse().ok()?;
        let to = to.parse().ok()?;
        let mut builder = lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::relay(host).ok()?;
        if let Some(port) = port {
            builder = builder.port(port);
        }
        let mut secrets = Secrets::default().with(host);
        if let (Some(user), Some(pass)) = (user, pass) {
            builder = builder.credentials(Credentials::new(user.to_owned(), pass.to_owned()));
            secrets = secrets.with(user).with(pass);
        }
        Some(Self {
            mailer: builder.build(),
            from,
            to,
            secrets,
        })
    }

    /// Build from env (`YAGRA_SMTP_HOST`, `_FROM`, `_TO`, optional `_PORT`/`_USER`/`_PASS`).
    /// Returns `None` if the required vars are missing or malformed.
    pub fn from_env() -> Option<Self> {
        let host = std::env::var("YAGRA_SMTP_HOST")
            .ok()
            .filter(|s| !s.is_empty())?;
        let from = std::env::var("YAGRA_SMTP_FROM").ok()?;
        let to = std::env::var("YAGRA_SMTP_TO").ok()?;
        let port = std::env::var("YAGRA_SMTP_PORT")
            .ok()
            .and_then(|p| p.parse::<u16>().ok());
        let user = std::env::var("YAGRA_SMTP_USER").ok();
        let pass = std::env::var("YAGRA_SMTP_PASS").ok();
        Self::new(&host, port, &from, &to, user.as_deref(), pass.as_deref())
    }
}

/// Build a live delivery channel from a stored channel config (None if email params are bad).
pub(crate) fn build_channel(config: &ChannelConfig) -> Option<Arc<dyn NotifyChannel>> {
    match config {
        ChannelConfig::Webhook { url } => {
            Some(Arc::new(WebhookChannel::new(url.clone())) as Arc<dyn NotifyChannel>)
        }
        ChannelConfig::Email {
            host,
            port,
            from,
            to,
            user,
            pass,
        } => EmailChannel::new(host, *port, from, to, user.as_deref(), pass.as_deref())
            .map(|c| Arc::new(c) as Arc<dyn NotifyChannel>),
        ChannelConfig::PagerDuty {
            routing_key,
            api_url,
        } => Some(
            Arc::new(PagerDutyChannel::new(routing_key.clone(), api_url.clone()))
                as Arc<dyn NotifyChannel>,
        ),
        ChannelConfig::Jsm { api_url, api_key } => {
            Some(Arc::new(JsmChannel::new(api_url.clone(), api_key.clone()))
                as Arc<dyn NotifyChannel>)
        }
    }
}

/// Prefix on a test notification's subject line (ADR-192 decision 1), so a person reading it on a
/// phone at 3am does not mistake it for the sample alert it describes.
pub(crate) const TEST_SUBJECT_PREFIX: &str = "[TEST] ";

/// What one test send did (ADR-192), and each call it made (ADR-195).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestDelivery {
    /// Whether the channel accepted the notification.
    pub delivered: bool,
    /// Whether the incident the test opened was closed again. `None` for a channel with no
    /// incident to close (webhook, email), and when the delivery itself failed.
    pub closed: Option<bool>,
    /// The first failure, as text an operator may read - never carrying the channel's URL
    /// ([`delivery_error`]).
    pub error: Option<String>,
    /// The send, as the delivery log records it.
    pub send: yagra_alert::Attempt,
    /// The close, when one was attempted.
    pub close: Option<yagra_alert::Attempt>,
}

/// The test notification for a channel: the template preview's sample alert, rendered through the
/// channel's own template exactly as a real one would be, and marked as a test (ADR-192 decision 1).
///
/// Three marks, each where it can go without editing what an operator wrote:
/// - the subject line starts with [`TEST_SUBJECT_PREFIX`], templated or not;
/// - the **built-in** body gains `"test": true` (JSON) or a first line saying so (text). A
///   templated body is the operator's text and is left alone, so a webhook with a body template
///   carries no mark in its payload - the dialog says so;
/// - the check id is new on every call, so PagerDuty's `dedup_key` and JSM's `alias` never fold a
///   test into an incident opened by an earlier one.
pub(crate) fn test_notification(kind: ChannelKind, template: &ChannelTemplate) -> Notification {
    let (mut alert, resolved) =
        crate::notify_facts::preview_sample(yagra_common::PreviewSample::Threshold);
    alert.check = CheckId::from(Uuid::new_v4());
    let mut facts = context_for(&alert, NotifyEvent::Fire, &resolved);
    facts.if_name = crate::notify_facts::preview_port_name(yagra_common::PreviewSample::Threshold);
    let builtin = builtin_for_kind(kind, &alert, NotifyEvent::Fire, Some(&facts));
    let builtin = Notification {
        payload: mark_as_test(kind, &builtin.payload),
        ..builtin
    };
    let mut n = if template.is_builtin() {
        builtin
    } else {
        // The same renderer `for_channel` uses, so a template that falls back on delivery falls
        // back here too - the test shows what a real alert would send, failures included.
        let rendered = render_with_fallback(
            Some(template),
            &facts,
            body_must_be_json(kind),
            &builtin.summary,
            &builtin.payload,
        );
        Notification {
            summary: rendered.subject,
            payload: rendered.body,
            ..builtin
        }
    };
    n.summary = format!("{TEST_SUBJECT_PREFIX}{}", n.summary);
    n
}

/// The test mark on a built-in body: `"test": true` added to the alert JSON, or a first line saying
/// so on the text a person reads (ADR-194 decision 5). A JSON body that is not an object comes back
/// unchanged - the built-in payload always is one, but this must not be the place that breaks it.
fn mark_as_test(kind: ChannelKind, payload: &str) -> String {
    if !body_must_be_json(kind) {
        return format!("{}\n\n{payload}", crate::notify_text::TEST_BODY_LINE);
    }
    match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(serde_json::Value::Object(mut obj)) => {
            obj.insert("test".to_owned(), serde_json::Value::Bool(true));
            serde_json::Value::Object(obj).to_string()
        }
        _ => payload.to_owned(),
    }
}

/// How long to wait before closing the incident a test opened, or `None` when the channel kind has
/// no incident to close (ADR-192 decision 2).
///
/// **JSM creates an alert asynchronously.** Its create call answers 202 before the alert exists,
/// and a close-by-alias that arrives first answers 404 - which [`JsmChannel::deliver_resolve`]
/// rightly reads as "already closed", so the test would report success and leave the alert open.
/// The pause is the guard; whether five seconds is enough is only known against a real tenant.
/// PagerDuty orders events per routing key, so its short pause is courtesy rather than correctness.
pub(crate) fn test_close_delay(kind: ChannelKind) -> Option<std::time::Duration> {
    match kind {
        ChannelKind::Webhook | ChannelKind::Email => None,
        ChannelKind::PagerDuty => Some(std::time::Duration::from_secs(2)),
        ChannelKind::Jsm => Some(std::time::Duration::from_secs(5)),
    }
}

/// Send one test notification, once, and close what it opened (ADR-192 decisions 2 and 3).
///
/// Deliberately **not** through a [`Dispatcher`]: no retry (the operator is waiting and wants the
/// first failure, not the fourth) and no dedup state left behind. `close_after` is
/// [`test_close_delay`] for the channel's kind; a test passes zero.
pub(crate) async fn send_test(
    channel: &dyn NotifyChannel,
    notification: &Notification,
    close_after: Option<std::time::Duration>,
) -> TestDelivery {
    let send = timed(channel.deliver(notification)).await;
    if let Some(f) = &send.failure {
        return TestDelivery {
            delivered: false,
            closed: None,
            error: Some(f.message.clone()),
            send,
            close: None,
        };
    }
    let Some(wait) = close_after else {
        return TestDelivery {
            delivered: true,
            closed: None,
            error: None,
            send,
            close: None,
        };
    };
    tokio::time::sleep(wait).await;
    let close = timed(channel.deliver_resolve(notification)).await;
    TestDelivery {
        delivered: true,
        closed: Some(close.failure.is_none()),
        error: close.failure.as_ref().map(|f| f.message.clone()),
        send,
        close: Some(close),
    }
}

/// One call to a channel, timed, as an [`yagra_alert::Attempt`].
async fn timed(
    call: impl std::future::Future<Output = Result<(), NotifyError>>,
) -> yagra_alert::Attempt {
    let started = std::time::Instant::now();
    let result = call.await;
    yagra_alert::Attempt {
        duration: started.elapsed(),
        failure: result.err().map(NotifyError::into_failure),
    }
}

#[async_trait]
impl NotifyChannel for EmailChannel {
    async fn deliver(&self, notification: &Notification) -> Result<(), NotifyError> {
        use lettre::AsyncTransport;
        let email = lettre::Message::builder()
            .from(self.from.clone())
            .to(self.to.clone())
            .subject(notification.summary.clone())
            .body(notification.payload.clone())
            .map_err(|e| DeliveryFailure::yagra(self.secrets.redact(&e.to_string())))?;
        self.mailer
            .send(email)
            .await
            .map_err(|e| smtp_failure(&e, &self.secrets))?;
        Ok(())
    }
}

/// An SMTP failure as a [`DeliveryFailure`]: a reply code means the server answered and refused
/// (`Remote`, with the code as the status); anything else never reached a reply.
fn smtp_failure(e: &lettre::transport::smtp::Error, secrets: &Secrets) -> DeliveryFailure {
    let text = secrets.redact(&e.to_string());
    match e.status() {
        Some(code) => DeliveryFailure::remote(code.to_string().parse::<u16>().ok(), text, None),
        None => DeliveryFailure::network(text),
    }
}

/// Fan-out channel: deliver to every configured channel; fails if any fails (so the
/// dispatcher's retry covers a transient outage on any of them).
pub struct MultiChannel {
    channels: Vec<Box<dyn NotifyChannel>>,
}

#[async_trait]
impl NotifyChannel for MultiChannel {
    async fn deliver(&self, notification: &Notification) -> Result<(), NotifyError> {
        for channel in &self.channels {
            channel.deliver(notification).await?;
        }
        Ok(())
    }

    // Must forward (not inherit the no-op default) or a lifecycle-aware child channel
    // would never see its resolve.
    async fn deliver_resolve(&self, notification: &Notification) -> Result<(), NotifyError> {
        for channel in &self.channels {
            channel.deliver_resolve(notification).await?;
        }
        Ok(())
    }
}

/// An unexpired mute, resolved for matching: the node plus the precomputed [`CheckId`]
/// (mutes are stored by check *name*, but an [`Alert`] only carries the id — the v5 hash
/// is recomputed here at load time). `check: None` mutes every check on the node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveMute {
    pub node: NodeId,
    pub check: Option<CheckId>,
    /// The stored check name verbatim, so a per-interface metric's alerts match too (ADR-076).
    /// See [`mute_matches`] for why the id alone is not enough.
    pub metric: Option<String>,
}

impl ActiveMute {
    /// Build from a stored mute row (node uuid + optional check name).
    #[must_use]
    pub fn new(node: Uuid, check_name: Option<&str>) -> Self {
        let node = NodeId::from(node);
        Self {
            node,
            check: check_name.map(|name| check_id(node, name)),
            metric: check_name.map(str::to_owned),
        }
    }
}

/// Whether an alert is covered by any active mute (separate fn for unit testing).
///
/// A mute names a node, so an alert with a non-node subject is never muted — a pool-coverage
/// alert cannot be silenced from the UI in this increment. That is a gap, not a decision: giving
/// a mute a pool target belongs with the rest of the scope-and-surface work (Increment 2).
///
/// # Why the metric is matched as well as the check id (ADR-076 decision 5)
///
/// A mute stores a *check name* and [`ActiveMute::new`] turns it into `check_id(node, name)` — the
/// **node-level** id. Since ADR-076 a per-interface metric's alerts carry
/// `interface_check_id(node, ifindex, name)` instead, so an id-only comparison would match none of
/// them: the operator picks `if_oper_status` from the metric picker (ADR-075 decision 18 put the
/// same picker on this form), saves, and the mute silently silences nothing. Matching the metric
/// name too makes a node-level mute cover **every port's** alerts for that metric, which is what
/// picking a per-interface metric on a node-scoped form plainly means.
///
/// ⚠️ Muting **one port** is still impossible: `api/maintenance.rs` validates `check_name` with
/// [`yagra_common::is_valid_metric_name`], which cannot express the `metric@ifindex` form. Written
/// down rather than worked around — the form has no port field to fill in either.
#[must_use]
fn mute_matches(mutes: &[ActiveMute], alert: &Alert) -> bool {
    let Some(node) = alert.node() else {
        return false;
    };
    mutes.iter().any(|m| {
        m.node == node
            && match (&m.metric, m.check) {
                // A mute with no check name covers the whole node, as it always has.
                (None, _) => true,
                (Some(metric), check) => {
                    check.is_some_and(|c| c == alert.check) || *metric == alert.metric
                }
            }
    })
}

/// A channel's notification-template override plus the one thing rendering needs to know about
/// the channel itself (ADR-039).
///
/// Held next to the dispatchers rather than inside the built [`NotifyChannel`] so a template edit
/// takes effect on the next routing refresh without rebuilding the channel — which would reset its
/// dedup state and re-page every active alert.
struct ChannelOverride {
    template: ChannelTemplate,
    /// Whether this channel carries the body as JSON (webhook/PagerDuty) — see
    /// [`crate::notify_render::body_must_be_json`].
    needs_json: bool,
}

/// One channel as [`Notifier::install_routing`] takes it: its id, the live channel (`None` when
/// the stored config could not be built), and the override that renders it (`None` = built-in).
///
/// Exists so that building a channel and installing it are separate steps: `build_channel` only
/// ever makes real HTTP/SMTP clients, which is why the 117 lines of [`Notifier::handle`] had no
/// test at all before ADR-104. A test hands these in directly.
struct BuiltChannel {
    id: Uuid,
    channel: Option<Arc<dyn NotifyChannel>>,
    over: Option<ChannelOverride>,
    /// Whether this channel reads the alert's resolved facts without being told to: PagerDuty and
    /// JSM put the node's tags on the wire (ADR-135), and JSM and email write their built-in text
    /// from the facts (ADR-194). Resolved here because it reads the stored config kind, which
    /// `install_routing` no longer has.
    reads_facts: bool,
    /// Whether this channel's built-in notification is the text a person reads rather than the
    /// alert as JSON (ADR-194) - see [`builtin_for_kind`].
    text_builtin: bool,
    /// The stored kind, for the delivery log (ADR-195). `None` only for a test's fake channel.
    kind: Option<ChannelKind>,
}

/// The live routing snapshot: the always-on env default route, the DB-configured channels
/// (each with its own dedup+retry dispatcher), and the rules that select channels per alert.
///
/// **Immutable, and shared as an `Arc`** (ADR-104). Delivery clones the `Arc` and drops the lock
/// guard before it awaits anything, so a wedged vendor endpoint cannot hold up the 30-second
/// refresh that installs a new mute or a new channel. Replacing it is a whole-value swap:
/// see [`Notifier::install_routing`].
struct Routing {
    /// Env-configured channels (`YAGRA_WEBHOOK_URL`/`YAGRA_SMTP_*`) — fire for *every* alert,
    /// preserving the pre-routing behaviour. `None` if no env channel is set.
    ///
    /// **Always the built-in format.** It has no channel id and no database row, so there is
    /// nothing for a per-channel override to hang off (ADR-039 decision 1); a deployment that
    /// wants templated notifications configures a channel in the UI.
    default: Option<Arc<Dispatcher<Arc<dyn NotifyChannel>>>>,
    /// DB channels by id, each with its own dedup state (preserved across config refresh).
    channels: HashMap<Uuid, Arc<Dispatcher<Arc<dyn NotifyChannel>>>>,
    /// Per-channel template overrides, for the channels that have one. Absent = built-in format.
    overrides: HashMap<Uuid, ChannelOverride>,
    /// Routing rules (severity → channel ids).
    rules: Vec<RoutingRule>,
    /// The channels whose built-in notification is text rather than JSON (ADR-194).
    text: HashSet<Uuid>,
    /// Each channel's kind, for the delivery log (ADR-195).
    kinds: HashMap<Uuid, ChannelKind>,
}

impl Routing {
    /// Channel ids whose enabled routing rule matches this severity (a rule with no severity
    /// matches any).
    ///
    /// One function rather than the three verbatim copies the fire / resolve / roll-up paths each
    /// carried — the set has to be identical across them or a resolve would go somewhere its fire
    /// did not (ADR-104 decision 5).
    fn matched(&self, severity: Severity) -> BTreeSet<Uuid> {
        self.rules
            .iter()
            .filter(|r| r.enabled && rule_matches_severity(r.severity, severity))
            .flat_map(|r| r.channel_ids.iter().copied())
            .collect()
    }

    /// The built-in notification this channel starts from: the text form for a JSM or email
    /// channel, the JSON form for the rest (ADR-194). `text` is `None` when no channel is text.
    fn builtin_for<'a>(
        &self,
        channel: Uuid,
        json: &'a Notification,
        text: Option<&'a Notification>,
    ) -> &'a Notification {
        match text {
            Some(t) if self.text.contains(&channel) => t,
            _ => json,
        }
    }
}

/// Counter for delivery itself (ADR-104 decision 4).
///
/// Before this, a [`DispatchOutcome`] only ever reached a `tracing::info!` — so "the PagerDuty
/// endpoint is wedged and every page is 31.5 seconds late" was invisible on the metrics endpoint,
/// which is the one place an operator would look for it.
const M_DISPATCH: &str = "yagra_notification_dispatch_total";
/// Wall time for one dispatch, including retries and backoff. See [`M_DISPATCH`].
const M_DELIVERY_SECONDS: &str = "yagra_notification_delivery_seconds";

/// The `outcome` label for one dispatch result: `delivered`, `suppressed` (a duplicate of a
/// still-active alert, so the channel was never called) or `failed` (every retry exhausted).
///
/// A named function rather than a `match` inside [`record_dispatch`] so the mapping is testable,
/// and exhaustive rather than wildcarded so a fourth outcome cannot be filed under whichever arm
/// happened to be last.
fn outcome_label(outcome: DispatchOutcome) -> &'static str {
    match outcome {
        DispatchOutcome::Delivered { .. } => "delivered",
        DispatchOutcome::Suppressed => "suppressed",
        DispatchOutcome::Failed { .. } => "failed",
    }
}

/// Record one dispatch: its outcome and how long it took.
///
/// ⚠️ **`route` is `default` or a channel UUID, so this label is bounded but not constant.**
/// Channels are operator-created and there are single digits of them in practice; a deployment
/// with hundreds would pay for it (`monitoring-conventions.md` — every label is a cardinality
/// cost). It is the useful grouping precisely because two PagerDuty channels are two on-call
/// rotations.
fn record_dispatch(
    route: &str,
    event: NotifyEvent,
    outcome: DispatchOutcome,
    started: std::time::Instant,
) {
    metrics::counter!(
        M_DISPATCH,
        "route" => route.to_owned(),
        "event" => event.as_str(),
        "outcome" => outcome_label(outcome),
    )
    .increment(1);
    metrics::histogram!(M_DELIVERY_SECONDS, "route" => route.to_owned())
        .record(started.elapsed().as_secs_f64());
}

/// Forwards alert lifecycle to the configured channels with the engine's dedup + retry
/// (ADR-015). Channels + rules come from the database (refreshed periodically via
/// [`Self::set_routing`]); env channels remain an always-on default route.
///
/// # Nothing here waits on delivery (ADR-104)
///
/// [`Self::set_routing`] and [`Self::set_mutes`] are **not `async`**, and that is the guarantee
/// rather than a coincidence: they cannot await, so "the config refresh is stuck behind a wedged
/// vendor endpoint" is not a state this type can be in. Until ADR-104 both took the same mutex
/// that [`Self::handle`] held across PagerDuty/JSM requests with retry and backoff — measured at
/// up to 31.5 seconds per notification, or 61.5 with a 429 — so an operator muting a noisy node
/// waited out the vendor before the mute applied.
///
/// What is still serialized is one channel at a time, inside its own [`Dispatcher`], which is
/// where the dedup and ordering guarantees live. See that type's doc.
///
/// ⚠️ **One consequence to know.** A delivery that has already cloned the routing snapshot can
/// finish against a channel deleted a moment later; deletion no longer waits for it. The bound is
/// "the notifications already in flight", and waiting is the thing being removed.
pub struct Notifier {
    /// The routing snapshot. `std::sync::RwLock` deliberately: it is never held across an await,
    /// and making that impossible is the point.
    routing: RwLock<Arc<Routing>>,
    /// Unexpired mutes — matching alerts are not delivered (UI/history unaffected).
    ///
    /// Held apart from [`Routing`] so that installing a mute does not clone the channel map:
    /// mutes are re-resolved on every 30-second cycle because they expire, while channels change
    /// only when an operator edits one.
    mutes: RwLock<Arc<Vec<ActiveMute>>>,
    /// Resolves node names/group/profile for a template's context (ADR-039). `None` in skeleton
    /// mode and before startup wiring, in which case a template sees ids instead of names.
    facts: RwLock<Option<Arc<dyn AlertFactsSource>>>,
    /// Whether *any* channel currently has a template.
    ///
    /// Read without touching the routing snapshot so that a deployment with no templates — which
    /// is every deployment until someone writes one — does exactly what it did before this feature
    /// landed, including issuing no extra query to resolve names nobody is going to interpolate.
    any_templates: AtomicBool,
    /// Whether *any* channel reads the alert's facts by itself - a PagerDuty or JSM channel puts
    /// the node's tags on the wire (ADR-135), and a JSM or email channel writes its built-in text
    /// from them (ADR-194).
    ///
    /// 🚨 **This exists so tag-based paging works without anyone writing a template.** The facts
    /// lookup below is gated on [`Self::any_templates`], so a variable alone would have reached
    /// only the channels an operator had already customized — shipping the feature inert for
    /// everyone else, which is the failure mode ADR-135 was opened to fix rather than repeat.
    ///
    /// A deployment with only webhook channels still issues no extra query.
    any_facts_channel: AtomicBool,
    /// Where each delivery is recorded (ADR-195). Recording never waits: see [`DeliveryLog`].
    delivery_log: RwLock<Option<DeliveryLog>>,
}

impl Notifier {
    /// Build a notifier with the env default route (a Webhook via `YAGRA_WEBHOOK_URL` and/or
    /// email via `YAGRA_SMTP_*`). DB channels/rules are layered on later via `set_routing`.
    #[must_use]
    pub fn from_env() -> Self {
        let mut channels: Vec<Box<dyn NotifyChannel>> = Vec::new();
        if let Ok(url) = std::env::var("YAGRA_WEBHOOK_URL") {
            if !url.is_empty() {
                channels.push(Box::new(WebhookChannel::new(url)));
            }
        }
        if let Some(email) = EmailChannel::from_env() {
            channels.push(Box::new(email));
        }
        let default = (!channels.is_empty()).then(|| {
            tracing::info!(
                channels = channels.len(),
                "alert notifier default route enabled"
            );
            Arc::new(MultiChannel { channels }) as Arc<dyn NotifyChannel>
        });
        Self::with_default(default)
    }

    /// A notifier over a given default route, with no DB channels or rules yet.
    ///
    /// Split out of [`Self::from_env`] for the same reason as [`Self::install_routing`]: `from_env`
    /// reads process-wide environment variables, so a test cannot choose a default route without
    /// changing what every other test in the process sees.
    ///
    /// ⚠️ **A fixture builds its notifier here, never through [`Self::from_env`].** That
    /// reader takes `YAGRA_WEBHOOK_URL` / `YAGRA_SMTP_*` from the process environment, so a
    /// developer who happens to have one exported would make a test deliver a real webhook or a
    /// real mail. `with_default(None)` has no route at all and so cannot (`api::tests_support`).
    pub(crate) fn with_default(default: Option<Arc<dyn NotifyChannel>>) -> Self {
        Self {
            routing: RwLock::new(Arc::new(Routing {
                default: default.map(|c| Arc::new(Dispatcher::new(c, RetryPolicy::default()))),
                channels: HashMap::new(),
                overrides: HashMap::new(),
                rules: Vec::new(),
                text: HashSet::new(),
                kinds: HashMap::new(),
            })),
            mutes: RwLock::new(Arc::new(Vec::new())),
            facts: RwLock::new(None),
            any_templates: AtomicBool::new(false),
            any_facts_channel: AtomicBool::new(false),
            delivery_log: RwLock::new(None),
        }
    }

    /// Attach the delivery log (ADR-195). Called once at startup, after the shutdown token
    /// exists; until then, and on a core with no write side, deliveries are not recorded.
    pub fn set_delivery_log(&self, log: DeliveryLog) {
        *self
            .delivery_log
            .write()
            .expect("notifier delivery log lock poisoned") = Some(log);
    }

    /// Attach the source that resolves node names/group/profile for a template's context
    /// (ADR-039). Called once at startup; a core with no write side never calls it and its
    /// templates render ids instead of names.
    pub fn set_facts_source(&self, source: Arc<dyn AlertFactsSource>) {
        *self.facts.write().expect("notifier facts lock poisoned") = Some(source);
    }

    /// Replace the DB routing snapshot. Channels that still exist keep their dispatcher (so the
    /// periodic refresh doesn't reset dedup and re-page active alerts); new channels get a
    /// fresh dispatcher; removed channels are dropped.
    ///
    /// A channel's **connection config** is treated as immutable — changing it means delete +
    /// recreate, because the live channel object is what holds it. Its **notification template**
    /// is not: it lives beside the dispatcher rather than inside the channel, so it is replaced
    /// wholesale here and an edit takes effect on the next refresh with no restart and without
    /// resetting dedup (ADR-039).
    ///
    /// **Not `async` since ADR-104** — see the type doc.
    pub fn set_routing(&self, channels: Vec<OpenChannel>, rules: Vec<RoutingRule>) {
        let built = channels
            .into_iter()
            .map(|ch| {
                // `needs_json` reads the stored config, so it is resolved here, on the side that
                // still has one — `install_routing` sees only built channels.
                let over = if ch.template.is_builtin() {
                    None
                } else {
                    Some(ChannelOverride {
                        needs_json: body_must_be_json(ch.config.kind()),
                        template: ch.template,
                    })
                };
                BuiltChannel {
                    id: ch.id,
                    channel: build_channel(&ch.config),
                    over,
                    reads_facts: reads_facts(ch.config.kind()),
                    text_builtin: !body_must_be_json(ch.config.kind()),
                    kind: Some(ch.config.kind()),
                }
            })
            .collect();
        self.install_routing(built, rules);
    }

    /// Install an already-built routing snapshot, carrying over the dispatcher of every channel
    /// that survives so its dedup state is not reset.
    ///
    /// Split out of [`Self::set_routing`] so a test can hand in fake channels (ADR-104
    /// decision 3). The write lock spans the read of the previous value so two concurrent
    /// installs cannot each build from the same predecessor; it still cannot be blocked by
    /// delivery, which never holds this lock across an await.
    fn install_routing(&self, channels: Vec<BuiltChannel>, rules: Vec<RoutingRule>) {
        let mut slot = self
            .routing
            .write()
            .expect("notifier routing lock poisoned");
        let mut next = HashMap::new();
        let mut overrides = HashMap::new();
        let mut fact_channels = HashSet::new();
        let mut text = HashSet::new();
        let mut kinds = HashMap::new();
        for built in channels {
            if let Some(kind) = built.kind {
                kinds.insert(built.id, kind);
            }
            if let Some(over) = built.over {
                overrides.insert(built.id, over);
            }
            if built.reads_facts {
                fact_channels.insert(built.id);
            }
            if built.text_builtin {
                text.insert(built.id);
            }
            if let Some(existing) = slot.channels.get(&built.id) {
                next.insert(built.id, Arc::clone(existing)); // preserve dedup
            } else if let Some(channel) = built.channel {
                next.insert(
                    built.id,
                    Arc::new(Dispatcher::new(channel, RetryPolicy::default())),
                );
            }
        }
        // Only keep an override for a channel that actually has a live dispatcher, so the flag
        // below cannot be set by a channel whose config failed to build.
        overrides.retain(|id, _| next.contains_key(id));
        fact_channels.retain(|id| next.contains_key(id));
        text.retain(|id| next.contains_key(id));
        kinds.retain(|id, _| next.contains_key(id));
        self.any_templates
            .store(!overrides.is_empty(), Ordering::Relaxed);
        self.any_facts_channel
            .store(!fact_channels.is_empty(), Ordering::Relaxed);
        *slot = Arc::new(Routing {
            default: slot.default.clone(),
            channels: next,
            overrides,
            rules,
            text,
            kinds,
        });
    }

    /// Replace the unexpired-mute snapshot (refreshed alongside routing).
    ///
    /// **Not `async` since ADR-104** — see the type doc. This is the call an operator is waiting
    /// on when they mute a noisy node.
    pub fn set_mutes(&self, mutes: Vec<ActiveMute>) {
        *self.mutes.write().expect("notifier mutes lock poisoned") = Arc::new(mutes);
    }

    /// The current routing snapshot. Clones one `Arc` and releases the lock — never held across
    /// an await.
    fn routing(&self) -> Arc<Routing> {
        Arc::clone(&self.routing.read().expect("notifier routing lock poisoned"))
    }

    /// Resolve the template context for an alert, or `None` when nothing would read it.
    ///
    /// Deliberately **before** the routing snapshot is read: this is the one part of delivery that
    /// touches the database, and there is no reason for it to be inside anything.
    ///
    /// ⚠️ **Three things read it, not one.** A channel template interpolates it; since ADR-135 a
    /// PagerDuty or JSM channel puts its `tags` on the wire whether or not anyone wrote a
    /// template; and since ADR-194 a JSM or email channel writes its built-in text from it. A
    /// deployment with none of those still issues no query at all, which is the property this
    /// gate has always been for.
    async fn context(&self, alert: &Alert, event: NotifyEvent) -> Option<AlertFacts> {
        if !self.any_templates.load(Ordering::Relaxed)
            && !self.any_facts_channel.load(Ordering::Relaxed)
        {
            return None;
        }
        // Every subject renders through a template now. The vocabulary carries `subject_kind` and
        // an always-present `subject_name` so a template can read correctly for both kinds; a
        // template written before those existed still renders, because `node_id`/`node_name` fall
        // back to the subject's own identifier rather than to a nil UUID (`notify_facts`).
        let source = self
            .facts
            .read()
            .expect("notifier facts lock poisoned")
            .clone();
        let resolved = match &source {
            Some(src) => src.facts(&node_ids_for(alert)).await,
            None => HashMap::new(),
        };
        let mut facts = context_for(alert, event, &resolved);
        // `context_for` is pure, so the port's name is asked for here (ADR-196 decision 6).
        if let (Some(node), Some(ifindex), Some(src)) = (alert.node(), alert.ifindex, &source) {
            facts.if_name = src.port_name(node.as_uuid(), ifindex.0).await;
        }
        // A Meraki organization is identified by id and carries no name, so `context_for` — which
        // is pure — can only call it by its flat form. The page that wakes someone should say
        // which organization (ADR-164 decision 18).
        if let (Some(org), Some(src)) = (alert.subject.meraki_org(), &source) {
            if let Some(name) = src.meraki_org_name(org).await {
                facts.subject_name.clone_from(&name);
                facts.node_name = name;
            }
        }
        Some(facts)
    }

    /// Apply one notify action (deliver a fire, or resolve/clear a recovered alert).
    ///
    /// **Holds no lock across delivery** (ADR-104): the routing snapshot and the mutes are each
    /// one `Arc` clone taken before anything is awaited. What serializes is one channel at a time,
    /// inside its own [`Dispatcher`].
    ///
    /// ⚠️ This still awaits its caller. A wedged vendor endpoint no longer blocks other channels,
    /// other callers, or the config refresh — but the single-consumer notification worker that
    /// feeds this is still one action at a time, so its bounded queue can still fill. Giving each
    /// channel its own queue would need a policy for a full queue, and dropping a page is worse
    /// than delaying one; deliberately left out (ADR-104 decision 6).
    pub async fn handle(&self, action: NotifyAction) {
        // Resolving names is I/O, so it happens first. A muted or rolled-up alert pays for a
        // lookup it will not use, which the facts cache makes negligible and which is worth not
        // restructuring the suppression checks around.
        let facts = match &action {
            NotifyAction::Fire(a) => self.context(a, NotifyEvent::Fire).await,
            NotifyAction::Resolve(a) => self.context(a, NotifyEvent::Resolve).await,
            NotifyAction::Suppress(a) => self.context(a, NotifyEvent::Suppress).await,
        };
        let routing = self.routing();
        match action {
            NotifyAction::Fire(alert) => {
                // Suppressed downstream alert: it's attributed to an upstream root cause and
                // rolled into that incident, so we don't page for it separately (the root
                // cause's own alert — root_cause: None — is what notifies). It still fired
                // for the UI/history; only the duplicate notification is suppressed.
                if let Some(root) = alert.root_cause {
                    tracing::debug!(subject = %alert.subject, %root, "suppressing downstream alert notification (rolled up under root cause)");
                    return;
                }
                // Muted: the operator asked for silence on this node/check until the mute
                // expires. The alert itself stays live in the UI/history.
                let mutes = Arc::clone(&self.mutes.read().expect("notifier mutes lock poisoned"));
                if mute_matches(&mutes, &alert) {
                    tracing::debug!(subject = %alert.subject, "suppressing muted alert notification");
                    return;
                }
                let notification = with_subject_facts(
                    json_notification(&alert, NotifyEvent::Fire, facts.as_ref()),
                    facts.as_ref(),
                );
                let text = (!routing.text.is_empty())
                    .then(|| text_notification(&alert, NotifyEvent::Fire, facts.as_ref()));
                let matched = routing.matched(alert.severity);
                let done = Delivered {
                    alert: &alert,
                    event: NotifyEvent::Fire,
                    facts: facts.as_ref(),
                    message: "alert notification dispatched",
                };
                if let Some(d) = routing.default.as_ref() {
                    let started = std::time::Instant::now();
                    let at = chrono::Utc::now();
                    let report = d.dispatch(notification.clone()).await;
                    self.after_dispatch(&done, None, None, &report, at, started);
                }
                for id in matched {
                    if let Some(d) = routing.channels.get(&id) {
                        let base = routing.builtin_for(id, &notification, text.as_ref());
                        let n = for_channel(id, &routing.overrides, facts.as_ref(), base);
                        let started = std::time::Instant::now();
                        let at = chrono::Utc::now();
                        let report = d.dispatch(n).await;
                        let kind = routing.kinds.get(&id).copied();
                        self.after_dispatch(&done, Some(id), kind, &report, at, started);
                    }
                }
            }
            NotifyAction::Resolve(alert) => {
                // A root-cause-suppressed alert never delivered its fire, so there is no
                // remote incident to close — just clear local dedup (mirror of the fire path).
                if alert.root_cause.is_some() {
                    let key = alert.dedup_key();
                    if let Some(d) = routing.default.as_ref() {
                        d.mark_resolved(&key).await;
                    }
                    for d in routing.channels.values() {
                        d.mark_resolved(&key).await;
                    }
                    return;
                }
                // Deliver the resolve to the same channels the fire was routed to (same
                // severity match) so lifecycle-aware channels (PagerDuty/JSM) close their
                // incident; webhook/email keep their no-op default. Deliberately NOT
                // mute-filtered: a mute placed after the fire must not leave a remote
                // incident dangling open (vendor resolves are idempotent).
                self.close(
                    &routing,
                    &alert,
                    NotifyEvent::Resolve,
                    facts.as_ref(),
                    "alert resolve dispatched",
                )
                .await;
            }
            NotifyAction::Suppress(alert) => {
                // A downstream alert that had been paging standalone is now rolled up under its
                // upstream root cause: close its remote incident so on-call isn't left with a
                // separate open page. Mirrors the (non-root-cause) resolve close path — the alert
                // itself stays live in the UI grouped under the root cause. Vendor resolves are
                // idempotent, so a repeat close is harmless.
                self.close(
                    &routing,
                    &alert,
                    NotifyEvent::Suppress,
                    facts.as_ref(),
                    "downstream alert rolled up (incident closed)",
                )
                .await;
            }
        }
    }

    /// Close a remote incident on the channels the fire was routed to, and clear local dedup on
    /// the rest.
    ///
    /// Resolve and roll-up were 32 identical lines apart from their log wording, which is why the
    /// wording is a parameter rather than a merged sentence: it is what an operator greps for, so
    /// both strings are preserved verbatim (ADR-104 decision 5).
    async fn close(
        &self,
        routing: &Routing,
        alert: &Alert,
        event: NotifyEvent,
        facts: Option<&AlertFacts>,
        message: &'static str,
    ) {
        let key = alert.dedup_key();
        let notification = with_subject_facts(json_notification(alert, event, facts), facts);
        let text = (!routing.text.is_empty()).then(|| text_notification(alert, event, facts));
        let matched = routing.matched(alert.severity);
        let done = Delivered {
            alert,
            event,
            facts,
            message,
        };
        if let Some(d) = routing.default.as_ref() {
            let started = std::time::Instant::now();
            let at = chrono::Utc::now();
            let report = d.dispatch_resolve(notification.clone()).await;
            self.after_dispatch(&done, None, None, &report, at, started);
        }
        for (id, d) in &routing.channels {
            if matched.contains(id) {
                let base = routing.builtin_for(*id, &notification, text.as_ref());
                let n = for_channel(*id, &routing.overrides, facts, base);
                let started = std::time::Instant::now();
                let at = chrono::Utc::now();
                let report = d.dispatch_resolve(n).await;
                let kind = routing.kinds.get(id).copied();
                self.after_dispatch(&done, Some(*id), kind, &report, at, started);
            } else {
                d.mark_resolved(&key).await;
            }
        }
    }

    /// Everything that follows one dispatch: the two metrics, the log line, and the delivery-log
    /// row (ADR-195). `channel` is `None` for the environment default route.
    ///
    /// One function rather than four copies, because the four used to be one `info!` each and
    /// now carry the failure's side and status too - the copies that drift are the ones nobody
    /// reads during an incident.
    fn after_dispatch(
        &self,
        done: &Delivered<'_>,
        channel: Option<Uuid>,
        kind: Option<ChannelKind>,
        report: &DispatchReport,
        at: chrono::DateTime<chrono::Utc>,
        started: std::time::Instant,
    ) {
        let route = channel.map_or_else(|| "default".to_owned(), |id| id.to_string());
        let outcome = report.outcome;
        record_dispatch(&route, done.event, outcome, started);
        match report.last_failure() {
            Some(f) if matches!(outcome, DispatchOutcome::Failed { .. }) => {
                tracing::warn!(
                    ?outcome,
                    subject = %done.alert.subject,
                    route = %route,
                    side = ?f.side,
                    status = ?f.status,
                    error = %f.message,
                    "{}", done.message
                );
            }
            _ => {
                tracing::info!(?outcome, subject = %done.alert.subject, route = %route, "{}", done.message);
            }
        }
        let delivered = match outcome {
            DispatchOutcome::Delivered { .. } => true,
            DispatchOutcome::Failed { .. } => false,
            // The channel was never called: nothing was delivered or failed, so nothing to log.
            DispatchOutcome::Suppressed => return,
        };
        let log = self
            .delivery_log
            .read()
            .expect("notifier delivery log lock poisoned")
            .clone();
        if let Some(log) = log {
            log.record(DeliveryRecord {
                at,
                channel_id: channel,
                channel_kind: kind,
                event: done.event.into(),
                subject: done.alert.subject.to_string(),
                node_id: done.alert.node().map(|n| n.as_uuid()),
                subject_name: done
                    .facts
                    .map(|f| f.subject_name.clone())
                    .filter(|n| !n.is_empty()),
                severity: Some(done.alert.severity),
                delivered,
                duration: started.elapsed(),
                attempts: report.attempts.clone(),
            });
        }
    }
}

/// What every dispatch of one notify action shares, for [`Notifier::after_dispatch`].
struct Delivered<'a> {
    alert: &'a Alert,
    event: NotifyEvent,
    facts: Option<&'a AlertFacts>,
    /// The log wording, which an operator greps for and so is kept verbatim (ADR-104 decision 5).
    message: &'static str,
}

/// The notification Yagra sends when a channel has no template — and the fallback when its
/// template cannot be used.
///
/// **Deliberately a `format!` and not a built-in template** (ADR-039 decision 3). This is what
/// every failure path lands on, so it must not depend on the machinery that just failed. It is
/// also the reason the wording lives in exactly one place: the three lifecycle points used to
/// spell it out at three separate call sites inside `handle`, which is how two of them would
/// eventually stop agreeing.
/// Hang the subject node's tags (ADR-135) and the alert's facts as key/value pairs (ADR-194) on a
/// notification.
///
/// Applied to the **built-in** notification, before `for_channel` renders any template, because
/// `for_channel` carries every field it does not rewrite through from the built-in — so doing it
/// here means the tags reach a templated channel and an untemplated one identically.
///
/// `None` facts is the ordinary case on a deployment with no PagerDuty, JSM or email channel and
/// no template: nothing is resolved, so there is nothing to hang.
fn with_subject_facts(n: Notification, facts: Option<&AlertFacts>) -> Notification {
    match facts {
        Some(f) => n
            .with_tags(f.tags.clone())
            .with_details(crate::notify_text::details(f)),
        None => n,
    }
}

/// Whether a channel of this kind reads the alert's facts with no template (see
/// [`Notifier::any_facts_channel`]).
fn reads_facts(kind: ChannelKind) -> bool {
    match kind {
        ChannelKind::PagerDuty | ChannelKind::Jsm | ChannelKind::Email => true,
        ChannelKind::Webhook => false,
    }
}

/// The built-in notification for a channel of this kind (ADR-194): the alert as JSON for the
/// channels a program reads (webhook, PagerDuty), the text a person reads for JSM and email.
/// Delivery, the template preview and the test send all start here, so the three cannot disagree.
pub(crate) fn builtin_for_kind(
    kind: ChannelKind,
    alert: &Alert,
    event: NotifyEvent,
    facts: Option<&AlertFacts>,
) -> Notification {
    if body_must_be_json(kind) {
        with_subject_facts(json_notification(alert, event, facts), facts)
    } else {
        text_notification(alert, event, facts)
    }
}

/// The text built-in (`notify_text`). With no resolved facts it still renders, naming the node by
/// id: the facts `context_for` builds from the alert alone. A notification is never dropped for
/// want of a name.
fn text_notification(
    alert: &Alert,
    event: NotifyEvent,
    facts: Option<&AlertFacts>,
) -> Notification {
    let fallback;
    let facts = match facts {
        Some(f) => f,
        None => {
            fallback = context_for(alert, event, &HashMap::new());
            &fallback
        }
    };
    with_subject_facts(
        Notification::for_alert(
            alert,
            crate::notify_text::subject(alert, facts),
            crate::notify_text::body(alert, facts),
        ),
        Some(facts),
    )
}

/// The editor's draft subject for a channel of this kind (ADR-039 Inc.2, ADR-194 decision 6).
#[must_use]
pub(crate) const fn builtin_subject_template_for(
    kind: ChannelKind,
    event: NotifyEvent,
) -> &'static str {
    match kind {
        ChannelKind::Webhook | ChannelKind::PagerDuty => builtin_node_subject_template(event),
        ChannelKind::Jsm | ChannelKind::Email => crate::notify_text::node_subject_template(event),
    }
}

/// The editor's draft body for a channel of this kind (ADR-197 decision 2): the text body for
/// JSM and email, and `None` for webhook and PagerDuty, whose built-in body is the whole alert as
/// JSON and has no template.
#[must_use]
pub(crate) fn builtin_body_template_for(kind: ChannelKind, event: NotifyEvent) -> Option<String> {
    match kind {
        ChannelKind::Webhook | ChannelKind::PagerDuty => None,
        ChannelKind::Jsm | ChannelKind::Email => {
            Some(crate::notify_text::node_body_template(event))
        }
    }
}

/// The built-in notification with no facts resolved — a node is named by its id. See
/// [`json_notification`]. Test vocabulary: every production path passes the facts it has.
#[cfg(test)]
pub(crate) fn builtin_notification(alert: &Alert, event: NotifyEvent) -> Notification {
    json_notification(alert, event, None)
}

/// The built-in notification a program reads: the whole alert as JSON, and a one-line summary.
///
/// **The payload is unchanged by ADR-196; the summary is not.** A node's summary used to be
/// `node <uuid> is critical`, which is what PagerDuty showed the person it paged. It is now the
/// same title JSM and email carry (`notify_text::subject`): the node's name when the facts were
/// resolved, its id when they were not, and the alert's name either way.
fn json_notification(
    alert: &Alert,
    event: NotifyEvent,
    facts: Option<&AlertFacts>,
) -> Notification {
    let summary = match (&alert.subject, event) {
        (Subject::Node(_), _) => {
            let fallback;
            let facts = match facts {
                Some(f) => f,
                None => {
                    fallback = context_for(alert, event, &HashMap::new());
                    &fallback
                }
            };
            crate::notify_text::subject(alert, facts)
        }
        (Subject::Pool(pool), NotifyEvent::Fire) => {
            format!("poller pool \"{pool}\" has no live poller — its nodes are not being monitored")
        }
        (Subject::Pool(pool), NotifyEvent::Resolve) => {
            format!("resolved: poller pool \"{pool}\" has a live poller again")
        }
        // Unreachable today — a pool alert is raised through `raise_event_alert`, which sets
        // `root_cause: None`, and the dependency graph a roll-up walks is a graph of nodes. Spelled
        // out anyway so a future suppression path cannot silently emit node-shaped wording.
        (Subject::Pool(pool), NotifyEvent::Suppress) => {
            format!("rolled up: poller pool \"{pool}\" suppressed")
        }
        // Like a node, an organization is named by id here: the built-in text has only the alert
        // to go on, and a templated channel gets the name through `notify_facts` (`subject_name`).
        (Subject::MerakiOrg(org), NotifyEvent::Fire) => format!(
            "Meraki organization {org}: the Dashboard API is not answering — its devices' states \
             are the last ones collected"
        ),
        (Subject::MerakiOrg(org), NotifyEvent::Resolve) => {
            format!("resolved: Meraki organization {org} is being collected again")
        }
        // Unreachable today, for the reason the pool arm gives. Spelled out for the same one.
        (Subject::MerakiOrg(org), NotifyEvent::Suppress) => {
            format!("rolled up: Meraki organization {org} suppressed")
        }
    };
    let payload = serde_json::to_string(alert).unwrap_or_else(|_| "{}".to_owned());
    Notification::for_alert(alert, summary, payload)
}

/// The built-in subject of a **node** alert, written as a notification template (ADR-039 Inc.2).
///
/// The editor opens a webhook or PagerDuty channel that has no template on this text, as a draft
/// the operator can edit (JSM and email open on `notify_text::node_subject_template`),
/// so they start from what is sent today instead of an empty field. It is served rather than copied
/// into the WebUI because the wording lives in [`builtin_notification`]'s `format!`s, and a second
/// copy in another language would drift from it with nothing to notice.
/// `every_builtin_subject_template_renders_the_builtin_subject` pins the two together.
///
/// Node alerts only: a poller pool's and a Meraki organization's built-in wording are separate
/// sentences, and they keep being sent as long as the operator saves the draft untouched — the
/// editor stores no template in that case.
///
/// Since ADR-196 it is the same draft JSM and email open on, because the built-in summary is the
/// same sentence.
#[must_use]
pub(crate) const fn builtin_node_subject_template(event: NotifyEvent) -> &'static str {
    crate::notify_text::node_subject_template(event)
}

/// Counter for a template that could not be used and fell back to the built-in format (ADR-039).
///
/// The `reason` label is the point: `compile` means a template stored before it could be validated,
/// `render` a runtime failure, `too_large` an output past the cap, and `not_json` a body a JSON
/// channel would have mangled. They send an operator to four different places.
const M_TEMPLATE_ERR: &str = "yagra_notification_template_errors_total";

/// What one channel actually receives: its template's output, or — if it has no template, or the
/// template could not be used — the built-in text unchanged.
///
/// **This function cannot fail.** A template is operator-authored text that runs for the first time
/// during an outage; letting a mistake in it swallow the page would make the feature worse than not
/// having it (ADR-039 decision 5). Fallback is per field, so a typo in the body does not also
/// discard a subject that was written correctly.
fn for_channel(
    channel: Uuid,
    overrides: &HashMap<Uuid, ChannelOverride>,
    facts: Option<&AlertFacts>,
    builtin: &Notification,
) -> Notification {
    let (Some(over), Some(facts)) = (overrides.get(&channel), facts) else {
        return builtin.clone();
    };
    let rendered = render_with_fallback(
        Some(&over.template),
        facts,
        over.needs_json,
        &builtin.summary,
        &builtin.payload,
    );
    for failure in &rendered.failures {
        metrics::counter!(M_TEMPLATE_ERR, "reason" => failure.kind.as_str()).increment(1);
        tracing::warn!(
            channel = %channel,
            field = failure.field.as_str(),
            reason = failure.kind.as_str(),
            detail = %failure.message,
            "notification template unusable; sent the built-in format instead"
        );
    }
    Notification {
        summary: rendered.subject,
        payload: rendered.body,
        ..builtin.clone()
    }
}

/// Match a routing rule's severity against an alert's (separate fn for unit testing).
#[must_use]
fn rule_matches_severity(rule_severity: Option<Severity>, alert_severity: Severity) -> bool {
    rule_severity.is_none_or(|s| s == alert_severity)
}

#[cfg(test)]
mod template_tests {
    use super::*;
    use crate::notify_facts::tests::threshold_alert;
    use crate::notify_render::FailureKind;
    use yagra_common::{sample_facts, NodeId};

    fn over(subject: Option<&str>, body: Option<&str>, needs_json: bool) -> ChannelOverride {
        ChannelOverride {
            template: ChannelTemplate {
                free_layout: false,
                subject: subject.map(str::to_owned),
                body: body.map(str::to_owned),
            },
            needs_json,
        }
    }

    /// The editor's draft is [`builtin_node_subject_template`], and it is only honest if rendering
    /// it sends exactly what no template sends. Rendered through the real renderer against every
    /// preview sample, so a reworded `format!` or a renamed variable fails here, not in an inbox.
    ///
    /// Per channel kind since ADR-194: JSM and email name the node, so their draft is a different
    /// template — and it is checked against unresolved facts too, where the name is the id.
    #[test]
    fn every_builtin_subject_template_renders_the_builtin_subject() {
        let mut compared = 0;
        for kind in [
            ChannelKind::Webhook,
            ChannelKind::Email,
            ChannelKind::PagerDuty,
            ChannelKind::Jsm,
        ] {
            for sample in yagra_common::PreviewSample::ALL {
                let (alert, resolved) = crate::notify_facts::preview_sample(sample);
                for resolved in [resolved, HashMap::new()] {
                    for event in NotifyEvent::ALL {
                        let mut facts = context_for(&alert, event, &resolved);
                        facts.if_name = crate::notify_facts::preview_port_name(sample);
                        let template = ChannelTemplate {
                            free_layout: false,
                            subject: Some(builtin_subject_template_for(kind, event).to_owned()),
                            body: None,
                        };
                        let rendered =
                            render_with_fallback(Some(&template), &facts, false, "FELL BACK", "{}");
                        assert!(rendered.failures.is_empty(), "{:?}", rendered.failures);
                        assert_eq!(
                            rendered.subject,
                            builtin_for_kind(kind, &alert, event, Some(&facts)).summary,
                            "the {kind:?} {} {} draft does not render the built-in subject",
                            sample.as_str(),
                            event.as_str()
                        );
                        compared += 1;
                    }
                }
            }
        }
        assert_eq!(
            compared, 48,
            "4 kinds x 2 samples x resolved or not x 3 points"
        );
    }

    /// The built-in body as a template renders the built-in body, byte for byte (ADR-197
    /// decision 1) — at every point in the alert's life, for both samples, with and without the
    /// node's facts resolved, and for an alert about a table row. Webhook and PagerDuty have no
    /// body template, and say so.
    #[test]
    fn every_builtin_body_template_renders_the_builtin_body() {
        let mut compared = 0;
        for kind in [ChannelKind::Webhook, ChannelKind::PagerDuty] {
            for event in NotifyEvent::ALL {
                assert_eq!(builtin_body_template_for(kind, event), None);
            }
        }
        for kind in [ChannelKind::Email, ChannelKind::Jsm] {
            for sample in yagra_common::PreviewSample::ALL {
                let (alert, resolved) = crate::notify_facts::preview_sample(sample);
                for resolved in [resolved, HashMap::new()] {
                    for event in NotifyEvent::ALL {
                        let mut base = context_for(&alert, event, &resolved);
                        base.if_name = crate::notify_facts::preview_port_name(sample);
                        // Every optional line, on and off: no port name beside an index, a row
                        // name, a flapping node, no labels.
                        let mut no_name = base.clone();
                        no_name.if_name = None;
                        let mut row = base.clone();
                        row.ifindex = None;
                        row.if_name = None;
                        row.row_name = Some("I/O".to_owned());
                        row.flapping = true;
                        row.tags = Vec::new();
                        row.threshold = Some(0.25);
                        for facts in [base, no_name, row] {
                            let template = ChannelTemplate {
                                free_layout: false,
                                subject: None,
                                body: builtin_body_template_for(kind, event),
                            };
                            let rendered = render_with_fallback(
                                Some(&template),
                                &facts,
                                false,
                                "",
                                "FELL BACK",
                            );
                            assert!(rendered.failures.is_empty(), "{:?}", rendered.failures);
                            assert_eq!(
                                rendered.body,
                                builtin_for_kind(kind, &alert, event, Some(&facts)).payload,
                                "the {kind:?} {} {} body draft does not render the built-in body",
                                sample.as_str(),
                                event.as_str()
                            );
                            compared += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(
            compared, 72,
            "2 kinds x 2 samples x resolved or not x 3 points x 3 shapes"
        );
    }

    /// The copy the code editor offers with free layout on (`templateDisplay.ts`'s
    /// `laidOutBuiltinSource`, ADR-199) sends the built-in text, at every point in the alert's life.
    /// The two helpers below write the shape that function writes; what this test proves is that
    /// [`crate::notify_render::lay_out`] turns that shape back into the built-in, line breaks and all.
    #[test]
    fn the_laid_out_builtin_sends_the_builtin() {
        fn inner(text: &str) -> &str {
            text.strip_suffix('\n').unwrap_or(text)
        }
        fn laid_out(by: &[(NotifyEvent, String)]) -> String {
            let get = |e: NotifyEvent| by.iter().find(|(x, _)| *x == e).map(|(_, t)| t.as_str());
            let fire = get(NotifyEvent::Fire).expect("fire");
            let indent = |t: &str| {
                inner(t)
                    .split('\n')
                    .map(|l| format!("  {l}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let arms: Vec<NotifyEvent> = [NotifyEvent::Resolve, NotifyEvent::Suppress]
                .into_iter()
                .filter(|e| get(*e) != Some(fire))
                .collect();
            if arms.is_empty() {
                return fire.to_owned();
            }
            let mut lines = Vec::new();
            for (i, e) in arms.iter().enumerate() {
                let tag = if i == 0 { "if" } else { "elif" };
                lines.push(format!("{{% {tag} event == \"{}\" %}}", e.as_str()));
                lines.push(indent(get(*e).expect("arm")));
            }
            lines.push("{% else %}".to_owned());
            lines.push(indent(fire));
            lines.push("{% endif %}".to_owned());
            lines.join("\n")
        }
        let mut compared = 0;
        for kind in [ChannelKind::Email, ChannelKind::Jsm] {
            let subjects: Vec<(NotifyEvent, String)> = NotifyEvent::ALL
                .into_iter()
                .map(|e| (e, builtin_subject_template_for(kind, e).to_owned()))
                .collect();
            let bodies: Vec<(NotifyEvent, String)> = NotifyEvent::ALL
                .into_iter()
                .map(|e| (e, builtin_body_template_for(kind, e).expect("a text body")))
                .collect();
            // The built-in body is its subject followed by the same lines at every point, which is
            // the case the copy writes once.
            let rests: Vec<&str> = subjects
                .iter()
                .zip(&bodies)
                .map(|((_, s), (_, b))| {
                    b.strip_prefix(s.as_str())
                        .expect("body starts with subject")
                })
                .collect();
            assert!(rests.iter().all(|r| *r == rests[0] && r.starts_with('\n')));
            let template = ChannelTemplate {
                free_layout: true,
                subject: Some(laid_out(&subjects)),
                body: Some(format!("{}\n{}", laid_out(&subjects), &rests[0][1..])),
            };
            assert!(template.subject.as_deref().unwrap().contains("\n  "));
            for sample in yagra_common::PreviewSample::ALL {
                let (alert, resolved) = crate::notify_facts::preview_sample(sample);
                for event in NotifyEvent::ALL {
                    let mut facts = context_for(&alert, event, &resolved);
                    facts.if_name = crate::notify_facts::preview_port_name(sample);
                    let rendered =
                        render_with_fallback(Some(&template), &facts, false, "FELL", "FELL");
                    assert!(rendered.failures.is_empty(), "{:?}", rendered.failures);
                    let builtin = builtin_for_kind(kind, &alert, event, Some(&facts));
                    assert_eq!(rendered.subject, builtin.summary);
                    assert_eq!(rendered.body, builtin.payload);
                    compared += 1;
                }
            }
        }
        assert_eq!(compared, 12, "2 kinds x 2 samples x 3 points");
    }

    /// Webhook and PagerDuty are read by programs, and ADR-194 changes nothing they receive: the
    /// built-in is still the whole alert as JSON with the id-based subject, byte for byte.
    #[test]
    fn a_program_reads_the_same_json_it_always_did() {
        let alert = threshold_alert(NodeId::new());
        for kind in [ChannelKind::Webhook, ChannelKind::PagerDuty] {
            for event in NotifyEvent::ALL {
                let n = builtin_for_kind(kind, &alert, event, None);
                let old = builtin_notification(&alert, event);
                assert_eq!(n.summary, old.summary);
                assert_eq!(n.payload, old.payload);
            }
        }
    }

    /// JSM and email are read by people: the built-in body is text, never the alert JSON, and it
    /// is there even when no facts could be resolved.
    #[test]
    fn a_person_reads_text_even_with_no_facts() {
        let alert = threshold_alert(NodeId::new());
        for kind in [ChannelKind::Jsm, ChannelKind::Email] {
            for event in NotifyEvent::ALL {
                let n = builtin_for_kind(kind, &alert, event, None);
                assert!(
                    serde_json::from_str::<serde_json::Value>(&n.payload).is_err(),
                    "{kind:?} body is JSON: {}",
                    n.payload
                );
                assert!(n.payload.starts_with(&n.summary), "{}", n.payload);
            }
        }
    }

    /// The editor strikes through the part of a JSM title past the cut; it is only honest if the
    /// cut is the one this channel makes.
    #[test]
    fn the_editor_cuts_a_jsm_title_where_the_channel_does() {
        let ts = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../web/src/pages/templateModel.ts"),
        )
        .expect("web/src/pages/templateModel.ts");
        let line = ts
            .lines()
            .find(|l| l.starts_with("export const JSM_MESSAGE_MAX_CHARS"))
            .expect("JSM_MESSAGE_MAX_CHARS is declared in templateModel.ts");
        let value: usize = line
            .trim_end_matches(';')
            .rsplit('=')
            .next()
            .and_then(|v| v.trim().parse().ok())
            .expect("a plain number");
        assert_eq!(value, JSM_MESSAGE_MAX_CHARS);
    }

    /// The exact text every deployment receives today. **A change here is a change to every
    /// operator's inbox**, so it is pinned rather than described: the whole N-1 story of ADR-039
    /// is that a channel with no template sends what it sent before, byte for byte.
    ///
    /// ADR-196 changed it once, deliberately: the summary names the alert ("Ping response time")
    /// instead of saying only the state, and drops the `node ` prefix to read like the JSM and
    /// email title it now shares. The payload did not change.
    #[test]
    fn the_built_in_wording_is_unchanged_for_every_lifecycle_point() {
        let node = NodeId::new();
        let alert = threshold_alert(node);
        for (event, want) in [
            (
                NotifyEvent::Fire,
                format!("{node} is critical: Ping response time"),
            ),
            (NotifyEvent::Resolve, format!("resolved: {node} recovered")),
            (
                NotifyEvent::Suppress,
                format!("rolled up: {node} suppressed under upstream"),
            ),
        ] {
            let n = builtin_notification(&alert, event);
            assert_eq!(n.summary, want);
            // The payload has always been the whole alert as JSON.
            assert_eq!(n.payload, serde_json::to_string(&alert).unwrap());
            assert_eq!(n.dedup_key, alert.dedup_key());
            assert_eq!(n.severity, alert.severity);
        }
    }

    /// 🚨 **A PagerDuty or JSM channel makes the notifier resolve node facts even with no
    /// template anywhere** — which is what makes tag-based paging work out of the box (ADR-135
    /// decision 9). Without this, a `tags` variable would have reached only the deployments that
    /// had already written a template, i.e. almost none.
    ///
    /// Both directions, because the interesting failure is the gate being stuck open (a fact
    /// query per alert on every webhook-only deployment) as much as stuck shut.
    #[test]
    fn a_vendor_channel_opens_the_facts_gate_and_a_webhook_does_not() {
        use crate::notifications::ChannelConfig;
        let pagerduty = ChannelConfig::PagerDuty {
            routing_key: "rk".to_owned(),
            api_url: None,
        };
        let webhook = ChannelConfig::Webhook {
            url: "https://example.invalid/hook".to_owned(),
        };
        // An email channel writes its built-in text from the facts (ADR-194).
        let email = ChannelConfig::Email {
            host: "smtp.example.invalid".to_owned(),
            port: None,
            from: "yagra@example.com".to_owned(),
            to: "noc@example.com".to_owned(),
            user: None,
            pass: None,
        };
        for (config, want) in [(&webhook, false), (&pagerduty, true), (&email, true)] {
            let n = Notifier::with_default(None);
            n.set_routing(
                vec![OpenChannel {
                    id: Uuid::new_v4(),
                    // No template: this is the whole point — the gate must open on the channel
                    // KIND, not on anyone having customized it.
                    template: ChannelTemplate::default(),
                    config: config.clone(),
                }],
                Vec::new(),
            );
            assert!(!n.any_templates.load(Ordering::Relaxed), "no template");
            assert_eq!(
                n.any_facts_channel.load(Ordering::Relaxed),
                want,
                "{config:?} should {} open the facts gate",
                if want { "" } else { "not" }
            );
        }
    }

    /// A channel with no override gets the built-in notification untouched — same object, not a
    /// re-render that happens to agree.
    #[test]
    fn a_channel_without_a_template_receives_the_built_in_notification() {
        let id = Uuid::new_v4();
        let builtin = builtin_notification(&threshold_alert(NodeId::new()), NotifyEvent::Fire);
        let facts = sample_facts(NotifyEvent::Fire);
        assert_eq!(
            for_channel(id, &HashMap::new(), Some(&facts), &builtin),
            builtin
        );
    }

    #[test]
    fn a_template_replaces_the_subject_and_body_for_that_channel_only() {
        let templated = Uuid::new_v4();
        let plain = Uuid::new_v4();
        let mut overrides = HashMap::new();
        overrides.insert(
            templated,
            over(Some("{{ severity }} on {{ node_name }}"), None, false),
        );
        let builtin = builtin_notification(&threshold_alert(NodeId::new()), NotifyEvent::Fire);
        let facts = sample_facts(NotifyEvent::Fire);

        let a = for_channel(templated, &overrides, Some(&facts), &builtin);
        assert_eq!(a.summary, "critical on core-sw-01");
        assert_eq!(a.payload, builtin.payload, "the body was not overridden");

        let b = for_channel(plain, &overrides, Some(&facts), &builtin);
        assert_eq!(b, builtin, "one channel's template must not reach another");
    }

    /// The property the whole module is built around: a template that fails at render time costs
    /// the customisation, never the notification.
    #[test]
    fn a_failing_template_still_sends_the_built_in_text() {
        let id = Uuid::new_v4();
        let mut overrides = HashMap::new();
        overrides.insert(
            id,
            over(Some("{{ nope.attr }}"), Some("{{ also.bad }}"), false),
        );
        let builtin = builtin_notification(&threshold_alert(NodeId::new()), NotifyEvent::Fire);
        let out = for_channel(
            id,
            &overrides,
            Some(&sample_facts(NotifyEvent::Fire)),
            &builtin,
        );
        assert_eq!(out, builtin);
    }

    /// A JSON channel is the case where a "successful" render is still wrong: PagerDuty parses the
    /// body with `unwrap_or(Null)`, so an unescaped quote would page on-call with no detail.
    #[test]
    fn a_json_channel_rejects_a_body_that_is_not_json() {
        let id = Uuid::new_v4();
        let builtin = builtin_notification(&threshold_alert(NodeId::new()), NotifyEvent::Fire);
        let facts = sample_facts(NotifyEvent::Fire);

        let mut overrides = HashMap::new();
        overrides.insert(id, over(None, Some("{{ node_name }} is down"), true));
        assert_eq!(
            for_channel(id, &overrides, Some(&facts), &builtin).payload,
            builtin.payload
        );

        // The same template is fine where the body is plain text.
        let mut overrides = HashMap::new();
        overrides.insert(id, over(None, Some("{{ node_name }} is down"), false));
        assert_eq!(
            for_channel(id, &overrides, Some(&facts), &builtin).payload,
            "core-sw-01 is down"
        );
    }

    /// Without a facts source there is no context to render against, so the built-in text stands.
    /// A half-rendered notification full of blanks would be worse than the plain one.
    #[test]
    fn no_resolved_context_means_no_rendering() {
        let id = Uuid::new_v4();
        let mut overrides = HashMap::new();
        overrides.insert(id, over(Some("{{ node_name }}"), None, false));
        let builtin = builtin_notification(&threshold_alert(NodeId::new()), NotifyEvent::Fire);
        assert_eq!(for_channel(id, &overrides, None, &builtin), builtin);
    }

    /// Which channel kinds demand JSON is decided once, in `notify_render`, and read from there —
    /// `set_routing` must not grow a second opinion.
    #[test]
    fn the_json_rule_comes_from_the_channel_kind() {
        for (kind, want) in [
            (crate::notifications::ChannelKind::Webhook, true),
            (crate::notifications::ChannelKind::PagerDuty, true),
            (crate::notifications::ChannelKind::Jsm, false),
            (crate::notifications::ChannelKind::Email, false),
        ] {
            assert_eq!(body_must_be_json(kind), want);
        }
    }

    #[test]
    fn the_failure_reasons_are_the_metric_labels() {
        // Guards the label set the dashboards and the ADR name.
        assert_eq!(M_TEMPLATE_ERR, "yagra_notification_template_errors_total");
        assert_eq!(FailureKind::NotJson.as_str(), "not_json");
    }
}

#[cfg(test)]
mod tests {
    use super::super::rules::interface_check_id;
    use super::*;
    use yagra_common::NodeState;

    #[tokio::test]
    async fn vendor_response_handles_success_failure_429_and_extra_ok() {
        // 202 Accepted (both vendors' success status).
        assert!(
            vendor_response(synth_response(202, &[]), None, &Secrets::default())
                .await
                .is_ok()
        );
        // Hard failure surfaces as a delivery error (dispatcher retries).
        assert!(
            vendor_response(synth_response(400, &[]), None, &Secrets::default())
                .await
                .is_err()
        );
        // 429 waits out Retry-After then errs so the retry policy counts the attempt.
        let start = std::time::Instant::now();
        let r = vendor_response(
            synth_response(429, &[("retry-after", "0")]),
            None,
            &Secrets::default(),
        )
        .await;
        let f = r.unwrap_err().into_failure();
        assert_eq!(f.message, "rate limited (429)");
        assert_eq!(f.status, Some(429));
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        // JSM close treats 404 (already closed) as success — resolve stays idempotent.
        let ok404 = vendor_response(
            synth_response(404, &[]),
            Some(reqwest::StatusCode::NOT_FOUND),
            &Secrets::default(),
        )
        .await;
        assert!(ok404.is_ok());
        assert!(
            vendor_response(synth_response(404, &[]), None, &Secrets::default())
                .await
                .is_err()
        );
    }

    #[test]
    fn the_built_in_wording_for_a_pool_names_the_pool_and_not_a_node() {
        let alert = match mgr_alert() {
            NotifyAction::Fire(a) => a,
            other => panic!("expected a fire, got {other:?}"),
        };
        for (event, expected) in [
            (
                NotifyEvent::Fire,
                "poller pool \"tokyo\" has no live poller",
            ),
            (
                NotifyEvent::Resolve,
                "resolved: poller pool \"tokyo\" has a live poller again",
            ),
        ] {
            let n = builtin_notification(&alert, event);
            assert!(n.summary.starts_with(expected), "got {:?}", n.summary);
            assert!(
                !n.summary.contains("node "),
                "a pool must not be described as a node: {:?}",
                n.summary
            );
        }
    }
    use super::super::testkit::*;
    #[test]
    fn routing_rule_severity_match() {
        // None severity ⇒ matches every alert severity.
        assert!(rule_matches_severity(None, Severity::Critical));
        assert!(rule_matches_severity(None, Severity::Warning));
        // A specific severity matches only that one.
        assert!(rule_matches_severity(
            Some(Severity::Critical),
            Severity::Critical
        ));
        assert!(!rule_matches_severity(
            Some(Severity::Critical),
            Severity::Warning
        ));
    }

    #[test]
    fn build_channel_makes_webhook() {
        let ch = build_channel(&ChannelConfig::Webhook {
            url: "http://example.test/hook".to_owned(),
        });
        assert!(ch.is_some());
    }

    #[test]
    fn build_channel_makes_pagerduty_and_jsm() {
        assert!(build_channel(&ChannelConfig::PagerDuty {
            routing_key: "rk".to_owned(),
            api_url: None,
        })
        .is_some());
        assert!(build_channel(&ChannelConfig::Jsm {
            api_url: "https://api.atlassian.com/jsm/ops/integration/v2".to_owned(),
            api_key: "key".to_owned(),
        })
        .is_some());
    }

    fn vendor_notification(severity: Severity) -> Notification {
        let alert = Alert {
            subject: Subject::Node(NodeId::from(Uuid::nil())),
            check: yagra_common::CheckId::from(Uuid::nil()),
            severity,
            state: NodeState::Critical,
            at_unix_ms: 1,
            root_cause: None,
            flapping: false,
            metric: "event:test".to_owned(),
            breach: None,
            ifindex: None,
            row: None,
            row_name: None,
        };
        Notification::for_alert(&alert, "node down", r#"{"metric":"event:test"}"#)
    }

    #[test]
    fn pagerduty_body_matches_events_v2_contract() {
        let n = vendor_notification(Severity::Critical);
        let body = pagerduty_body("rk-secret", "trigger", &n, true);
        assert_eq!(body["routing_key"], "rk-secret");
        assert_eq!(body["event_action"], "trigger");
        let dedup = body["dedup_key"].as_str().unwrap();
        assert!(dedup.starts_with("yagra:"));
        assert!(dedup.ends_with(":critical"));
        assert_eq!(body["payload"]["summary"], "node down");
        assert_eq!(body["payload"]["severity"], "critical");
        // custom_details is the parsed alert JSON, not a double-encoded string.
        assert_eq!(body["payload"]["custom_details"]["metric"], "event:test");

        // Resolve carries only the correlation fields (payload omitted).
        let resolve = pagerduty_body("rk-secret", "resolve", &n, false);
        assert_eq!(resolve["event_action"], "resolve");
        assert_eq!(resolve["dedup_key"], body["dedup_key"]);
        assert!(resolve.get("payload").is_none());
        // No tags on this node, so nothing is added — the key must be absent rather than an
        // empty object, since a PagerDuty event rule testing for it would then always match.
        assert!(body["payload"]["custom_details"]
            .get("yagra_tags")
            .is_none());
    }

    /// The node's labels reach PagerDuty in `custom_details`, which is what an event rule can
    /// route on (ADR-135 decision 9) — and the operator's own body is added to, never rewritten.
    ///
    /// 🚨 **This test is the only evidence the shape is right.** The lab has no PagerDuty channel
    /// (ADR-083's remnant), so nothing downstream of here has ever been observed.
    #[test]
    fn pagerduty_carries_the_nodes_tags_without_disturbing_the_body() {
        let n = vendor_notification(Severity::Critical).with_tags(vec!["JAPAN".to_owned()]);
        let body = pagerduty_body("rk-secret", "trigger", &n, true);
        // An ARRAY since ADR-135 inc. 2, not an object keyed by tag name. An event rule matches it
        // with `contains`.
        assert_eq!(
            body["payload"]["custom_details"]["yagra_tags"]
                .as_array()
                .expect("yagra_tags is an array")
                .iter()
                .map(|v| v.as_str().expect("a string"))
                .collect::<Vec<_>>(),
            vec!["JAPAN"]
        );
        // What the body already said is still there, untouched.
        assert_eq!(body["payload"]["custom_details"]["metric"], "event:test");

        // 🚨 A body that is not a JSON object is left completely alone: it belongs to whoever
        // wrote the template, and there is nowhere to add a key without replacing what they said.
        let mut scalar = n.clone();
        scalar.payload = r#""just a string""#.to_owned();
        let body = pagerduty_body("rk-secret", "trigger", &scalar, true);
        assert_eq!(body["payload"]["custom_details"], "just a string");
    }

    /// The node's labels reach JSM through **its own** `tags` field, the one its alert policies
    /// route on. It stays absent when there are none.
    ///
    /// Since ADR-135 inc. 2 they go in as themselves rather than as flattened `key=value` strings,
    /// which is what Opsgenie's field wanted all along. ⚠️ A policy matching `region=JAPAN` has to
    /// be changed to match `JAPAN`.
    #[test]
    fn jsm_carries_the_nodes_tags_in_its_native_field() {
        let n = vendor_notification(Severity::Critical)
            .with_tags(vec!["JAPAN".to_owned(), "core".to_owned()]);
        let body = jsm_create_body(&n);
        let sent: Vec<&str> = body["tags"]
            .as_array()
            .expect("tags is an array")
            .iter()
            .map(|v| v.as_str().expect("a string"))
            .collect();
        assert_eq!(sent, vec!["JAPAN", "core"]);

        assert!(
            jsm_create_body(&vendor_notification(Severity::Critical))
                .get("tags")
                .is_none(),
            "an unlabelled node must not send an empty tag list"
        );
    }

    /// JSM holds 20 tags of up to 50 characters (ADR-194 decision 4). A longer tag is left out
    /// rather than cut — a cut tag is a different tag, and JSM's rules match on them — and the
    /// first 20 that fit are kept, which are the node's own because those come first.
    #[test]
    fn jsm_tags_are_kept_within_its_limits_by_dropping_never_cutting() {
        let long = "x".repeat(JSM_TAG_MAX_CHARS + 1);
        let exact = "y".repeat(JSM_TAG_MAX_CHARS);
        let mut tags = vec![long.clone(), exact.clone()];
        tags.extend((0..25).map(|i| format!("t{i}")));
        let body = jsm_create_body(&vendor_notification(Severity::Critical).with_tags(tags));
        let sent: Vec<&str> = body["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(sent.len(), JSM_TAGS_MAX);
        assert_eq!(sent[0], exact, "a tag of exactly the limit is kept");
        assert!(
            !sent.iter().any(|t| t.starts_with('x')),
            "the long one is dropped"
        );
        assert_eq!(
            sent[JSM_TAGS_MAX - 1],
            "t18",
            "kept in order, the rest dropped"
        );
    }

    /// The alert's facts go to JSM's own "extra properties" field as strings, and a description
    /// longer than JSM holds is cut to fit.
    #[test]
    fn jsm_carries_the_facts_in_details_and_a_description_it_can_hold() {
        let n = vendor_notification(Severity::Critical).with_details(vec![
            ("node".to_owned(), "core-sw-01".to_owned()),
            ("address".to_owned(), "192.0.2.11".to_owned()),
        ]);
        let n = Notification {
            payload: "a".repeat(JSM_DESCRIPTION_MAX_CHARS + 5),
            ..n
        };
        let body = jsm_create_body(&n);
        assert_eq!(body["details"]["node"], "core-sw-01");
        assert_eq!(body["details"]["address"], "192.0.2.11");
        assert_eq!(
            body["description"].as_str().unwrap().chars().count(),
            JSM_DESCRIPTION_MAX_CHARS
        );
        assert!(
            jsm_create_body(&vendor_notification(Severity::Critical))
                .get("details")
                .is_none(),
            "no facts, no empty details"
        );
    }

    #[test]
    fn jsm_body_and_close_url_match_opsgenie_contract() {
        let n = vendor_notification(Severity::Warning);
        let body = jsm_create_body(&n);
        assert_eq!(body["message"], "node down");
        assert_eq!(body["priority"], "P3"); // warning → P3 (critical P1, info P5)
        assert_eq!(body["source"], "yagra");
        let alias = body["alias"].as_str().unwrap().to_owned();
        assert!(alias.starts_with("yagra:"));

        let url = jsm_close_url("https://api.atlassian.com/jsm/ops/integration/v2", &n);
        assert_eq!(
            url,
            format!(
                "https://api.atlassian.com/jsm/ops/integration/v2/alerts/{alias}/close?identifierType=alias"
            )
        );

        // Severity → priority mapping extremes.
        assert_eq!(
            jsm_create_body(&vendor_notification(Severity::Critical))["priority"],
            "P1"
        );
        assert_eq!(
            jsm_create_body(&vendor_notification(Severity::Info))["priority"],
            "P5"
        );

        // JSM's message field caps at 130 chars.
        let mut long = vendor_notification(Severity::Warning);
        long.summary = "x".repeat(500);
        assert_eq!(
            jsm_create_body(&long)["message"].as_str().unwrap().len(),
            130
        );
    }

    fn synth_response(status: u16, headers: &[(&str, &str)]) -> reqwest::Response {
        let mut builder = axum::http::Response::builder().status(status);
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        reqwest::Response::from(builder.body("").unwrap())
    }

    #[tokio::test]
    async fn webhook_target_blocked_for_metadata_literal_allows_private() {
        async fn blocked(u: &str) -> bool {
            webhook_target_blocked(&reqwest::Url::parse(u).unwrap()).await
        }
        // SSRF-escalation surface (resolved before any request leaves core).
        assert!(blocked("http://169.254.169.254/hook").await);
        assert!(blocked("http://127.0.0.1/hook").await);
        assert!(blocked("http://[::ffff:169.254.169.254]/").await);
        // A legitimate internal (private-range) webhook stays allowed.
        assert!(!blocked("http://10.0.0.5/hook").await);
    }

    /// A node-scoped mute silences every port's alerts for that metric (ADR-076 decision 5).
    ///
    /// Before this, `ActiveMute::new` built `check_id(node, name)` — the node-level id — so once
    /// per-interface alerts carried a per-port id, a mute created from the metric picker matched
    /// nothing at all. Silently: the operator saw the mute listed and kept being paged.
    #[test]
    fn a_node_mute_on_a_per_interface_metric_covers_every_port() {
        use yagra_common::IfIndex;

        let node = NodeId::new();
        let mute = ActiveMute::new(node.as_uuid(), Some("if_in_util_pct"));

        let port_alert = |idx: u32| Alert {
            subject: Subject::Node(node),
            check: interface_check_id(node, IfIndex(idx), "if_in_util_pct"),
            severity: Severity::Critical,
            state: NodeState::Critical,
            at_unix_ms: 0,
            root_cause: None,
            flapping: false,
            metric: "if_in_util_pct".to_owned(),
            breach: None,
            ifindex: Some(IfIndex(idx)),
            row: None,
            row_name: None,
        };
        assert!(mute_matches(std::slice::from_ref(&mute), &port_alert(7)));
        assert!(mute_matches(std::slice::from_ref(&mute), &port_alert(48)));

        // It must not spill onto a different metric on the same node.
        let other = Alert {
            metric: "icmp_rtt_ms".to_owned(),
            check: check_id(node, "icmp_rtt_ms"),
            ifindex: None,
            ..port_alert(7)
        };
        assert!(!mute_matches(std::slice::from_ref(&mute), &other));

        // Nor onto another node.
        let elsewhere = Alert {
            subject: Subject::Node(NodeId::new()),
            ..port_alert(7)
        };
        assert!(!mute_matches(std::slice::from_ref(&mute), &elsewhere));

        // A mute with no check name still covers the whole node, as it always did.
        let whole_node = ActiveMute::new(node.as_uuid(), None);
        assert!(mute_matches(
            std::slice::from_ref(&whole_node),
            &port_alert(7)
        ));
        assert!(mute_matches(std::slice::from_ref(&whole_node), &other));
    }

    #[test]
    fn mute_matches_node_and_check() {
        let node = NodeId::new();
        let other = NodeId::new();
        let alert = Alert {
            subject: Subject::Node(node),
            check: check_id(node, "icmp_rtt_ms"),
            severity: Severity::Critical,
            state: NodeState::Critical,
            at_unix_ms: 0,
            root_cause: None,
            flapping: false,
            metric: "icmp_rtt_ms".to_string(),
            breach: None,
            ifindex: None,
            row: None,
            row_name: None,
        };

        // Whole-node mute matches any check on the node; another node's mute doesn't.
        assert!(mute_matches(
            &[ActiveMute::new(node.as_uuid(), None)],
            &alert
        ));
        assert!(!mute_matches(
            &[ActiveMute::new(other.as_uuid(), None)],
            &alert
        ));

        // Check-scoped mute matches only that check name (ids recomputed from the name).
        assert!(mute_matches(
            &[ActiveMute::new(node.as_uuid(), Some("icmp_rtt_ms"))],
            &alert
        ));
        assert!(!mute_matches(
            &[ActiveMute::new(
                node.as_uuid(),
                Some("snmp_sys_uptime_ticks")
            )],
            &alert
        ));
    }

    #[test]
    fn a_node_dedup_string_is_unchanged_by_the_subject_split() {
        // The vendor-facing identity: PagerDuty's `dedup_key` and JSM's `alias`. A change here
        // silently orphans every incident opened by a previous release.
        let node = NodeId::from(Uuid::from_u128(7));
        let check = CheckId::from(Uuid::from_u128(8));
        let key = yagra_alert::DedupKey {
            subject: Subject::Node(node),
            check,
            severity: Severity::Critical,
        };
        assert_eq!(dedup_string(&key), format!("yagra:{node}:{check}:critical"));
    }

    /// A pool name may contain a space or a slash, which would break the close-by-alias URL. A
    /// close that never lands is the dangling incident the resolve path exists to prevent.
    #[test]
    fn the_jsm_close_url_encodes_a_pool_name_and_leaves_a_node_alias_alone() {
        let notification = |subject: Subject| Notification {
            dedup_key: yagra_alert::DedupKey {
                subject,
                check: CheckId::from(Uuid::from_u128(2)),
                severity: Severity::Critical,
            },
            severity: Severity::Critical,
            summary: String::new(),
            payload: String::new(),
            tags: Vec::new(),
            details: Vec::new(),
        };
        let node = NodeId::from(Uuid::from_u128(1));
        let url = jsm_close_url("https://api.example/v2", &notification(Subject::Node(node)));
        assert!(
            url.contains(&format!("yagra:{node}:")) && !url.contains('%'),
            "a node alias must be byte-identical to what an older core sent: {url}"
        );

        let url = jsm_close_url(
            "https://api.example/v2",
            &notification(Subject::Pool("tokyo dc/2".to_owned())),
        );
        assert!(
            url.contains("tokyo%20dc%2F2"),
            "pool name not encoded: {url}"
        );
    }

    #[test]
    fn a_pool_coverage_alert_cannot_be_muted_by_a_node_mute() {
        // Documented gap rather than a decision — a mute names a node. Pinned so the behaviour is
        // deliberate rather than discovered.
        let alert = match mgr_alert() {
            NotifyAction::Fire(a) => a,
            other => panic!("expected a fire, got {other:?}"),
        };
        let mutes = vec![ActiveMute::new(Uuid::from_u128(1), None)];
        assert!(!mute_matches(&mutes, &alert));
    }

    /// The delivery metrics say what their docs say (ADR-104 decision 4).
    ///
    /// Same shape as `the_failure_reasons_are_the_metric_labels` above: the names are what an
    /// operator's dashboard and alert rules are written against, so renaming one is a breaking
    /// change to something outside this repository and should read as one in the diff.
    #[test]
    fn the_delivery_metrics_are_named_and_labelled_as_documented() {
        assert_eq!(M_DISPATCH, "yagra_notification_dispatch_total");
        assert_eq!(M_DELIVERY_SECONDS, "yagra_notification_delivery_seconds");
        assert_eq!(
            outcome_label(DispatchOutcome::Delivered { attempts: 1 }),
            "delivered"
        );
        assert_eq!(outcome_label(DispatchOutcome::Suppressed), "suppressed");
        assert_eq!(
            outcome_label(DispatchOutcome::Failed { attempts: 3 }),
            "failed"
        );
        // The `event` label comes from the shared vocabulary rather than a fourth spelling of it.
        assert_eq!(
            NotifyEvent::ALL.map(|e| e.as_str()),
            ["fire", "resolve", "suppress"]
        );
    }

    fn mgr_alert() -> NotifyAction {
        manager()
            .raise_pool_coverage_alert("tokyo", 1_000)
            .expect("a fresh manager raises")
    }
}

/// What `Notifier::handle` actually does with an action — the 117 lines that had no test at all
/// until ADR-104, because `set_routing` could only build real HTTP/SMTP clients.
///
/// 🚨 **Every one of these asserts that something *arrives*, not only that something is
/// suppressed.** A suite of "nothing was delivered" claims is satisfied by a notifier that
/// delivers nothing at all (`rejection-only-tests-pass-when-everything-rejects`), and this file
/// installs fake channels, so that failure is one typo away rather than hypothetical.
///
/// 🚨 **And the fake must not be the thing under test.** The first version of
/// [`a_resolve_cannot_overtake_the_fire_it_resolves`] gated both deliveries on one mutex, so the
/// *fake* ordered them and the test stayed green with the dispatcher's lock released before
/// delivery — the exact defect it was written to catch. Each lifecycle point now has its own
/// release, and the test asserts the resolve never reaches the channel at all while the fire is in
/// flight, which is a fact about the lane rather than about the fixture.
#[cfg(test)]
mod delivery_tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;
    use yagra_common::NodeState;

    /// The whole suite has a deadline: if delivery starts waiting on the routing snapshot again —
    /// the defect ADR-104 removed — these fail on the clock instead of hanging a CI run forever.
    const DEADLINE: Duration = Duration::from_secs(5);

    /// One channel's log of what it was handed, in order. `fire` is `deliver`, `close` is
    /// `deliver_resolve` (a recovery or a roll-up).
    #[derive(Default)]
    struct Seen(Vec<(&'static str, String)>);

    impl Seen {
        fn count(&self, kind: &str) -> usize {
            self.0.iter().filter(|(k, _)| *k == kind).count()
        }
        fn kinds(&self) -> Vec<&'static str> {
            self.0.iter().map(|(k, _)| *k).collect()
        }
    }

    type Log = Arc<Mutex<Seen>>;

    fn log() -> Log {
        Arc::new(Mutex::new(Seen::default()))
    }

    /// A channel that records what it receives and always succeeds.
    struct Recorder(Log);

    #[async_trait]
    impl NotifyChannel for Recorder {
        async fn deliver(&self, n: &Notification) -> Result<(), NotifyError> {
            self.0.lock().unwrap().0.push(("fire", n.summary.clone()));
            Ok(())
        }
        async fn deliver_resolve(&self, n: &Notification) -> Result<(), NotifyError> {
            self.0.lock().unwrap().0.push(("close", n.summary.clone()));
            Ok(())
        }
    }

    /// A vendor endpoint that has stopped answering: it announces its arrival, then does not
    /// return until the test releases it.
    ///
    /// Stands in for the real thing without paying its price — a wedged PagerDuty costs three
    /// attempts at a ten-second timeout plus 1.5s of backoff, which is why holding the routing
    /// snapshot across it mattered enough to be worth an ADR.
    ///
    /// ⚠️ **The two lifecycle points are released separately, on purpose.** One shared gate would
    /// serialize them here, and then this fixture — not the dispatcher — would be what keeps a
    /// fire ahead of its resolve.
    struct Gate {
        fire_gate: Arc<tokio::sync::Notify>,
        close_gate: Arc<tokio::sync::Notify>,
        arrived: tokio::sync::mpsc::UnboundedSender<&'static str>,
        seen: Log,
    }

    #[async_trait]
    impl NotifyChannel for Gate {
        async fn deliver(&self, n: &Notification) -> Result<(), NotifyError> {
            let _ = self.arrived.send("fire");
            self.fire_gate.notified().await;
            self.seen
                .lock()
                .unwrap()
                .0
                .push(("fire", n.summary.clone()));
            Ok(())
        }
        async fn deliver_resolve(&self, n: &Notification) -> Result<(), NotifyError> {
            let _ = self.arrived.send("close");
            self.close_gate.notified().await;
            self.seen
                .lock()
                .unwrap()
                .0
                .push(("close", n.summary.clone()));
            Ok(())
        }
    }

    fn built(id: Uuid, channel: impl NotifyChannel + 'static) -> BuiltChannel {
        BuiltChannel {
            id,
            channel: Some(Arc::new(channel)),
            over: None,
            reads_facts: false,
            text_builtin: false,
            kind: None,
        }
    }

    /// An enabled rule sending `severity` (None = any) to one channel.
    fn rule(channel: Uuid, severity: Option<Severity>) -> RoutingRule {
        RoutingRule {
            id: Uuid::new_v4(),
            name: "test rule".to_owned(),
            enabled: true,
            severity,
            channel_ids: vec![channel],
        }
    }

    fn alert(node: NodeId, severity: Severity, root_cause: Option<NodeId>) -> Alert {
        Alert {
            subject: Subject::Node(node),
            check: check_id(node, "icmp_rtt_ms"),
            severity,
            state: NodeState::Critical,
            at_unix_ms: 0,
            root_cause,
            flapping: false,
            metric: "icmp_rtt_ms".to_owned(),
            breach: None,
            ifindex: None,
            row: None,
            row_name: None,
        }
    }

    /// A channel that always refuses, the way JSM refuses a wrong key.
    struct Refuser;

    #[async_trait]
    impl NotifyChannel for Refuser {
        async fn deliver(&self, _: &Notification) -> Result<(), NotifyError> {
            Err(DeliveryFailure::remote(
                Some(401),
                "unexpected status 401",
                Some("bad key".to_owned()),
            )
            .into())
        }
    }

    /// ADR-195: every dispatch that called a channel becomes one delivery-log row, saying which
    /// channel, whether it arrived, and - for a failure - on whose side; a suppressed duplicate
    /// called nothing and records nothing.
    #[tokio::test(start_paused = true)]
    async fn each_delivery_is_recorded_with_where_it_failed() {
        let (ok_log, _) = (log(), log());
        let n = Notifier::with_default(None);
        let (dlog, mut rx) = crate::notification_log::DeliveryLog::for_test();
        n.set_delivery_log(dlog);
        let (good, bad) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut refusing = built(bad, Refuser);
        refusing.kind = Some(ChannelKind::Jsm);
        n.install_routing(
            vec![built(good, Recorder(ok_log.clone())), refusing],
            vec![rule(good, None), rule(bad, None)],
        );
        let node = NodeId::new();
        let fire = alert(node, Severity::Critical, None);
        n.handle(NotifyAction::Fire(fire.clone())).await;

        let mut rows = Vec::new();
        while let Ok(r) = rx.try_recv() {
            rows.push(r);
        }
        assert_eq!(rows.len(), 2, "one row per channel called");
        let ok = rows.iter().find(|r| r.channel_id == Some(good)).unwrap();
        assert!(ok.delivered);
        assert_eq!(ok.attempts.len(), 1);
        assert_eq!(ok.node_id, Some(node.as_uuid()));
        let failed = rows.iter().find(|r| r.channel_id == Some(bad)).unwrap();
        assert!(!failed.delivered);
        assert_eq!(failed.channel_kind, Some(ChannelKind::Jsm));
        assert_eq!(
            failed.attempts.len(),
            3,
            "the retry policy's three attempts"
        );
        let f = failed.attempts[2].failure.as_ref().unwrap();
        assert_eq!(f.side, yagra_alert::FailureSide::Remote);
        assert_eq!(f.response.as_deref(), Some("bad key"));

        // The same alert again: the good channel suppresses it as a duplicate and calls nothing,
        // so only the refusing channel (which never delivered, so has no dedup entry) is recorded.
        n.handle(NotifyAction::Fire(fire)).await;
        let mut again = Vec::new();
        while let Ok(r) = rx.try_recv() {
            again.push(r);
        }
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].channel_id, Some(bad));
    }

    #[tokio::test]
    async fn a_fire_reaches_the_default_route_and_every_channel_a_rule_matches() {
        let (dflt, a, b) = (log(), log(), log());
        let n = Notifier::with_default(Some(Arc::new(Recorder(dflt.clone()))));
        let (id_a, id_b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        n.install_routing(
            vec![
                built(id_a, Recorder(a.clone())),
                built(id_b, Recorder(b.clone())),
            ],
            vec![rule(id_a, Some(Severity::Critical))],
        );

        n.handle(NotifyAction::Fire(alert(
            NodeId::new(),
            Severity::Critical,
            None,
        )))
        .await;

        assert_eq!(
            dflt.lock().unwrap().count("fire"),
            1,
            "the env default route fires for every alert, rules or no rules"
        );
        assert_eq!(
            a.lock().unwrap().count("fire"),
            1,
            "a rule named channel A for this severity"
        );
        assert_eq!(
            b.lock().unwrap().count("fire"),
            0,
            "no rule named channel B, so it must not be paged"
        );
    }

    /// A JSM or email channel receives the text built-in and a webhook beside it the JSON one, for
    /// the same alert (ADR-194) — and a template that cannot be used on the text channel falls
    /// back to the text, not to the JSON.
    #[tokio::test]
    async fn each_channel_starts_from_its_own_kind_of_built_in() {
        let (json_log, text_log, broken_log) = (log(), log(), log());
        let n = Notifier::with_default(None);
        let (json_id, text_id, broken_id) =
            (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let text = |id, l: &Log| BuiltChannel {
            reads_facts: true,
            text_builtin: true,
            ..built(id, Recorder(l.clone()))
        };
        let mut broken = text(broken_id, &broken_log);
        broken.over = Some(ChannelOverride {
            template: ChannelTemplate {
                free_layout: false,
                subject: Some("{{ nope.attr }}".to_owned()),
                body: None,
            },
            needs_json: false,
        });
        n.install_routing(
            vec![
                built(json_id, Recorder(json_log.clone())),
                text(text_id, &text_log),
                broken,
            ],
            vec![
                rule(json_id, None),
                rule(text_id, None),
                rule(broken_id, None),
            ],
        );
        let node = NodeId::new();
        n.handle(NotifyAction::Fire(alert(node, Severity::Critical, None)))
            .await;

        let first = |l: &Log| l.lock().unwrap().0[0].1.clone();
        // No facts source is wired, so both name the node by its id — and, since ADR-196, the
        // program's summary and the person's title are the same sentence.
        let want = format!("{node} is critical: Ping response time");
        assert_eq!(first(&json_log), want);
        assert_eq!(first(&text_log), want);
        assert_eq!(first(&broken_log), want, "fell back to the text built-in");
    }

    #[tokio::test]
    async fn a_muted_alert_is_delivered_nowhere_but_an_unmuted_one_still_is() {
        let (dflt, a) = (log(), log());
        let n = Notifier::with_default(Some(Arc::new(Recorder(dflt.clone()))));
        let id_a = Uuid::from_u128(1);
        n.install_routing(
            vec![built(id_a, Recorder(a.clone()))],
            vec![rule(id_a, None)],
        );
        let (muted, other) = (NodeId::new(), NodeId::new());
        n.set_mutes(vec![ActiveMute::new(muted.as_uuid(), None)]);

        n.handle(NotifyAction::Fire(alert(muted, Severity::Critical, None)))
            .await;
        assert_eq!(dflt.lock().unwrap().count("fire"), 0, "muted node");
        assert_eq!(a.lock().unwrap().count("fire"), 0, "muted node");

        // The accept side: the mute silences that node, not the notifier.
        n.handle(NotifyAction::Fire(alert(other, Severity::Critical, None)))
            .await;
        assert_eq!(dflt.lock().unwrap().count("fire"), 1, "a different node");
        assert_eq!(a.lock().unwrap().count("fire"), 1, "a different node");
    }

    #[tokio::test]
    async fn a_rolled_up_alert_does_not_page_but_its_roll_up_closes_the_incident() {
        let a = log();
        let n = Notifier::with_default(None);
        let id_a = Uuid::from_u128(1);
        n.install_routing(
            vec![built(id_a, Recorder(a.clone()))],
            vec![rule(id_a, None)],
        );
        let node = NodeId::new();
        let upstream = NodeId::new();

        // Attributed to an upstream root cause: it fired for the UI, but the page belongs to the
        // upstream incident.
        n.handle(NotifyAction::Fire(alert(
            node,
            Severity::Critical,
            Some(upstream),
        )))
        .await;
        assert_eq!(a.lock().unwrap().count("fire"), 0);

        // A roll-up of an alert that *had* been paging standalone still has to close it, or
        // on-call is left with a page nothing will ever resolve.
        n.handle(NotifyAction::Suppress(alert(
            node,
            Severity::Critical,
            Some(upstream),
        )))
        .await;
        assert_eq!(
            a.lock().unwrap().count("close"),
            1,
            "the roll-up closes the remote incident"
        );
    }

    #[tokio::test]
    async fn a_resolve_reaches_the_matched_channel_and_clears_dedup_on_the_rest() {
        let (a, b) = (log(), log());
        let n = Notifier::with_default(None);
        let (id_a, id_b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let both = || {
            vec![
                built(id_a, Recorder(a.clone())),
                built(id_b, Recorder(b.clone())),
            ]
        };
        n.install_routing(both(), vec![rule(id_a, None), rule(id_b, None)]);
        let node = NodeId::new();
        let fire = || NotifyAction::Fire(alert(node, Severity::Critical, None));

        n.handle(fire()).await;
        assert_eq!(a.lock().unwrap().count("fire"), 1);
        assert_eq!(b.lock().unwrap().count("fire"), 1);
        // Still the same alert: dedup holds it back on both.
        n.handle(fire()).await;
        assert_eq!(a.lock().unwrap().count("fire"), 1, "deduped");
        assert_eq!(b.lock().unwrap().count("fire"), 1, "deduped");

        // Now only A is routed. The resolve is delivered to A; B is not called, but its dedup must
        // still be cleared or the alert could never page there again.
        n.install_routing(both(), vec![rule(id_a, None)]);
        n.handle(NotifyAction::Resolve(alert(node, Severity::Critical, None)))
            .await;
        assert_eq!(a.lock().unwrap().count("close"), 1, "A was routed");
        assert_eq!(b.lock().unwrap().count("close"), 0, "B was not routed");

        n.install_routing(both(), vec![rule(id_a, None), rule(id_b, None)]);
        n.handle(fire()).await;
        assert_eq!(
            a.lock().unwrap().count("fire"),
            2,
            "A re-pages after resolve"
        );
        assert_eq!(
            b.lock().unwrap().count("fire"),
            2,
            "B re-pages too — mark_resolved cleared its dedup even though it got no close"
        );
    }

    /// 🎯 **The property ADR-104 exists to buy**, stated as behaviour rather than as a grep over
    /// this file: with one channel wedged mid-delivery, the config refresh still applies and a
    /// notification bound for a different channel still goes out.
    ///
    /// Before ADR-104 all three of these waited on the same mutex the wedged delivery held, so an
    /// operator muting a noisy node waited out the vendor's whole retry budget first.
    #[tokio::test]
    async fn a_wedged_channel_blocks_neither_the_config_refresh_nor_another_channel() {
        let fire_gate = Arc::new(tokio::sync::Notify::new());
        let (tx, mut arrived) = tokio::sync::mpsc::unbounded_channel();
        let (a, b) = (log(), log());
        let (id_a, id_b) = (Uuid::from_u128(1), Uuid::from_u128(2));

        let n = Arc::new(Notifier::with_default(None));
        let install = |n: &Notifier| {
            n.install_routing(
                vec![
                    built(
                        id_a,
                        Gate {
                            fire_gate: Arc::clone(&fire_gate),
                            close_gate: Arc::new(tokio::sync::Notify::new()),
                            arrived: tx.clone(),
                            seen: a.clone(),
                        },
                    ),
                    built(id_b, Recorder(b.clone())),
                ],
                // Critical goes to the wedged channel, Warning to the healthy one, so the two
                // actions below are routed to different lanes.
                vec![
                    rule(id_a, Some(Severity::Critical)),
                    rule(id_b, Some(Severity::Warning)),
                ],
            );
        };
        install(&n);

        let wedged = tokio::spawn({
            let n = Arc::clone(&n);
            async move {
                n.handle(NotifyAction::Fire(alert(
                    NodeId::new(),
                    Severity::Critical,
                    None,
                )))
                .await;
            }
        });
        assert_eq!(
            arrived.recv().await,
            Some("fire"),
            "the wedged channel was called and has not returned"
        );

        tokio::time::timeout(DEADLINE, async {
            n.set_mutes(vec![ActiveMute::new(Uuid::from_u128(9), None)]);
            install(&n);
            n.handle(NotifyAction::Fire(alert(
                NodeId::new(),
                Severity::Warning,
                None,
            )))
            .await;
        })
        .await
        .expect("a wedged channel must not hold up mutes, routing, or another channel");

        assert_eq!(
            b.lock().unwrap().count("fire"),
            1,
            "the healthy channel delivered while the other was still wedged"
        );
        assert_eq!(
            a.lock().unwrap().count("fire"),
            0,
            "…and the wedged one had not finished"
        );

        fire_gate.notify_one();
        tokio::time::timeout(DEADLINE, wedged)
            .await
            .expect("the wedged delivery finishes once its endpoint answers")
            .expect("the delivery task did not panic");
        assert_eq!(a.lock().unwrap().count("fire"), 1);
    }

    /// A fire is never overtaken by its own resolve, even when the fire is stuck in delivery.
    ///
    /// This is what the per-channel lock in [`Dispatcher`] buys, and it is the reason ADR-104 kept
    /// that lock across delivery instead of claiming the dedup key and releasing it.
    ///
    /// 🚨 **The load-bearing assertion is the middle one**, not the final ordering: with the lock
    /// released before delivery the resolve reaches the channel immediately, and the final order
    /// then depends only on which gate the test opens first. Asserting that it does not arrive at
    /// all is a fact about the lane. Verified by breaking it — see the module doc.
    #[tokio::test]
    async fn a_resolve_cannot_overtake_the_fire_it_resolves() {
        let fire_gate = Arc::new(tokio::sync::Notify::new());
        let close_gate = Arc::new(tokio::sync::Notify::new());
        let (tx, mut arrived) = tokio::sync::mpsc::unbounded_channel();
        let seen = log();
        let id_a = Uuid::from_u128(1);

        let n = Arc::new(Notifier::with_default(None));
        n.install_routing(
            vec![built(
                id_a,
                Gate {
                    fire_gate: Arc::clone(&fire_gate),
                    close_gate: Arc::clone(&close_gate),
                    arrived: tx,
                    seen: seen.clone(),
                },
            )],
            vec![rule(id_a, None)],
        );
        let node = NodeId::new();

        let fire = tokio::spawn({
            let n = Arc::clone(&n);
            async move {
                n.handle(NotifyAction::Fire(alert(node, Severity::Critical, None)))
                    .await;
            }
        });
        assert_eq!(arrived.recv().await, Some("fire"));

        let resolve = tokio::spawn({
            let n = Arc::clone(&n);
            async move {
                n.handle(NotifyAction::Resolve(alert(node, Severity::Critical, None)))
                    .await;
            }
        });
        // Long enough for the resolve to reach the channel if anything would let it. Time rather
        // than a signal because the property is an *absence*: there is nothing to wait for.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            arrived.try_recv().is_err(),
            "a resolve reached the channel while the fire it resolves was still in flight"
        );

        // Released in reverse, so nothing but the lane can be producing the order below.
        close_gate.notify_one();
        fire_gate.notify_one();
        tokio::time::timeout(DEADLINE, async {
            fire.await.expect("fire task");
            resolve.await.expect("resolve task");
        })
        .await
        .expect("both actions complete once the endpoint answers");

        assert_eq!(
            seen.lock().unwrap().kinds(),
            vec!["fire", "close"],
            "the lane delivered them in the order they arrived"
        );
    }
}

/// The test send (ADR-192): what it sends, and what it closes.
#[cfg(test)]
mod test_send_tests {
    use super::*;
    use std::sync::Mutex;

    /// A channel that records each call and fails the ones it is told to.
    struct Fake {
        calls: Mutex<Vec<&'static str>>,
        fail_fire: bool,
        fail_close: bool,
    }

    impl Fake {
        fn new(fail_fire: bool, fail_close: bool) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fail_fire,
                fail_close,
            }
        }
        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl NotifyChannel for Fake {
        async fn deliver(&self, _: &Notification) -> Result<(), NotifyError> {
            self.calls.lock().unwrap().push("fire");
            if self.fail_fire {
                return Err(
                    DeliveryFailure::remote(Some(401), "unexpected status 401", None).into(),
                );
            }
            Ok(())
        }
        async fn deliver_resolve(&self, _: &Notification) -> Result<(), NotifyError> {
            self.calls.lock().unwrap().push("close");
            if self.fail_close {
                return Err(DeliveryFailure::remote(Some(429), "rate limited (429)", None).into());
            }
            Ok(())
        }
    }

    fn template(subject: Option<&str>, body: Option<&str>) -> ChannelTemplate {
        ChannelTemplate {
            free_layout: false,
            subject: subject.map(str::to_owned),
            body: body.map(str::to_owned),
        }
    }

    #[test]
    fn the_built_in_test_is_marked_in_the_subject_and_in_the_json() {
        let n = test_notification(ChannelKind::Webhook, &ChannelTemplate::default());
        assert!(n.summary.starts_with(TEST_SUBJECT_PREFIX), "{}", n.summary);
        let body: serde_json::Value = serde_json::from_str(&n.payload).unwrap();
        assert_eq!(body["test"], true, "{body}");
        // Still the alert JSON a real notification carries, with the mark beside it.
        assert!(body.get("severity").is_some(), "{body}");
    }

    #[test]
    fn the_built_in_test_is_marked_on_the_first_line_of_a_text_body() {
        for kind in [ChannelKind::Jsm, ChannelKind::Email] {
            let n = test_notification(kind, &ChannelTemplate::default());
            assert!(n.summary.starts_with(TEST_SUBJECT_PREFIX), "{}", n.summary);
            assert!(
                n.payload
                    .starts_with(&format!("{}\n\n", crate::notify_text::TEST_BODY_LINE)),
                "{}",
                n.payload
            );
            assert!(
                n.summary.contains("core-sw-01"),
                "names the node: {}",
                n.summary
            );
        }
    }

    #[test]
    fn a_template_renders_the_test_and_its_body_is_left_alone() {
        let n = test_notification(
            ChannelKind::Email,
            &template(
                Some("{{ severity }} on {{ node_name }}"),
                Some("{{ node_name }} is down"),
            ),
        );
        assert_eq!(
            n.summary,
            format!("{TEST_SUBJECT_PREFIX}critical on core-sw-01")
        );
        // The operator's body is theirs: no `"test"` key is pushed into it.
        assert_eq!(n.payload, "core-sw-01 is down");
    }

    /// A subject-only template keeps the built-in body, so that body still carries the mark.
    #[test]
    fn a_subject_only_template_keeps_the_marked_built_in_body() {
        let n = test_notification(ChannelKind::PagerDuty, &template(Some("x"), None));
        assert_eq!(n.summary, format!("{TEST_SUBJECT_PREFIX}x"));
        let body: serde_json::Value = serde_json::from_str(&n.payload).unwrap();
        assert_eq!(body["test"], true);
    }

    /// A template that cannot be used falls back exactly as it would on a real alert — the test
    /// shows what would be sent, not a second opinion.
    #[test]
    fn a_broken_template_falls_back_to_the_marked_built_in_text() {
        let n = test_notification(
            ChannelKind::Webhook,
            &template(None, Some("{{ nope.attr }}")),
        );
        let body: serde_json::Value = serde_json::from_str(&n.payload).unwrap();
        assert_eq!(body["test"], true);
    }

    /// PagerDuty's `dedup_key` and JSM's `alias` come from this: two tests must not fold into one
    /// incident, nor into an incident a real alert opened.
    #[test]
    fn every_test_has_its_own_dedup_key() {
        let a = test_notification(ChannelKind::PagerDuty, &ChannelTemplate::default());
        let b = test_notification(ChannelKind::PagerDuty, &ChannelTemplate::default());
        assert_ne!(dedup_string(&a.dedup_key), dedup_string(&b.dedup_key));
        let (sample, _) =
            crate::notify_facts::preview_sample(yagra_common::PreviewSample::Threshold);
        assert_ne!(a.dedup_key, sample.dedup_key());
    }

    #[test]
    fn only_the_incident_kinds_close_what_they_opened() {
        assert_eq!(test_close_delay(ChannelKind::Webhook), None);
        assert_eq!(test_close_delay(ChannelKind::Email), None);
        assert!(test_close_delay(ChannelKind::PagerDuty).is_some());
        // JSM creates asynchronously; closing at once would 404 and leave the alert open.
        assert!(test_close_delay(ChannelKind::Jsm).unwrap() >= std::time::Duration::from_secs(3));
    }

    /// The test dialog warns that a test pages the on-call for exactly the kinds that close an
    /// incident here. `web/src/pages/channelTest.ts::INCIDENT_KINDS` is that list's copy; read it
    /// so the warning and the behaviour cannot drift. ⚠️ It parses one line, which says so.
    #[test]
    fn the_webuis_incident_kinds_are_the_kinds_a_test_closes() {
        let ts = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../web/src/pages/channelTest.ts"),
        )
        .expect("web/src/pages/channelTest.ts");
        let line = ts
            .lines()
            .find(|l| l.contains("export const INCIDENT_KINDS"))
            .expect("INCIDENT_KINDS is declared in channelTest.ts");
        let listed: Vec<&str> = line.split('\'').skip(1).step_by(2).collect();
        let ours: Vec<&str> = [
            ChannelKind::Webhook,
            ChannelKind::Email,
            ChannelKind::PagerDuty,
            ChannelKind::Jsm,
        ]
        .into_iter()
        .filter(|k| test_close_delay(*k).is_some())
        .map(|k| k.as_str())
        .collect();
        assert_eq!(listed, ours);
    }

    #[tokio::test]
    async fn an_incident_channel_is_fired_then_closed() {
        let ch = Fake::new(false, false);
        let n = test_notification(ChannelKind::PagerDuty, &ChannelTemplate::default());
        let out = send_test(&ch, &n, Some(std::time::Duration::ZERO)).await;
        assert_eq!(ch.calls(), ["fire", "close"]);
        assert!(out.delivered);
        assert_eq!(out.closed, Some(true));
        assert_eq!(out.error, None);
        // Both calls are kept for the delivery log (ADR-195).
        assert!(out.send.failure.is_none());
        assert!(out.close.unwrap().failure.is_none());
    }

    #[tokio::test]
    async fn a_channel_with_no_incident_is_sent_once() {
        let ch = Fake::new(false, false);
        let n = test_notification(ChannelKind::Webhook, &ChannelTemplate::default());
        let out = send_test(&ch, &n, None).await;
        assert_eq!(ch.calls(), ["fire"]);
        assert_eq!(out.closed, None);
        assert!(out.delivered);
    }

    /// One attempt, no retry, and nothing to close when nothing was opened.
    #[tokio::test]
    async fn a_failed_fire_is_reported_once_and_not_closed() {
        let ch = Fake::new(true, false);
        let n = test_notification(ChannelKind::Jsm, &ChannelTemplate::default());
        let out = send_test(&ch, &n, Some(std::time::Duration::ZERO)).await;
        assert_eq!(ch.calls(), ["fire"]);
        assert!(!out.delivered);
        assert_eq!(out.closed, None);
        assert_eq!(out.error.as_deref(), Some("unexpected status 401"));
        assert_eq!(out.send.failure.unwrap().status, Some(401));
        assert!(out.close.is_none());
    }

    /// The page went out but the incident is still open: the operator has to be told, because
    /// someone now has to close it by hand.
    #[tokio::test]
    async fn a_failed_close_is_reported_as_delivered_but_still_open() {
        let ch = Fake::new(false, true);
        let n = test_notification(ChannelKind::PagerDuty, &ChannelTemplate::default());
        let out = send_test(&ch, &n, Some(std::time::Duration::ZERO)).await;
        assert_eq!(ch.calls(), ["fire", "close"]);
        assert!(out.delivered);
        assert_eq!(out.closed, Some(false));
        assert_eq!(out.error.as_deref(), Some("rate limited (429)"));
    }

    /// reqwest names the URL in its `Display`; a webhook's URL is its secret.
    #[tokio::test]
    async fn a_delivery_error_does_not_carry_the_url() {
        let err = reqwest::Client::new()
            .post("http://hooks.nonexistent.invalid/s3cr3t-path")
            .send()
            .await
            .unwrap_err();
        assert!(err.to_string().contains("s3cr3t-path"), "premise: {err}");
        let failure = delivery_error(err).into_failure();
        // Nothing answered a name that does not resolve: the network's side, not the remote's.
        assert_eq!(failure.side, yagra_alert::FailureSide::Network);
        let text = failure.message;
        assert!(!text.contains("s3cr3t-path"), "{text}");
        assert!(!text.contains("nonexistent.invalid"), "{text}");
        assert!(!text.is_empty());
    }

    /// The other way a host gets out: rustls names the host it expected when the certificate is for
    /// another name, deep in the source chain the error text keeps on purpose.
    #[tokio::test]
    async fn a_certificate_for_another_name_does_not_carry_the_host() {
        use std::sync::Arc;
        let kp = rcgen::KeyPair::generate().expect("keypair");
        let cert = rcgen::CertificateParams::new(vec!["other.test".to_owned()])
            .expect("params")
            .self_signed(&kp)
            .expect("sign");
        let server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(kp.serialize_der().into()),
        )
        .expect("server config");
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((tcp, _)) = listener.accept().await {
                let _ = acceptor.accept(tcp).await;
            }
        });

        // Trusted, so the failure is the name and not the issuer.
        let client = reqwest::Client::builder()
            .add_root_certificate(reqwest::Certificate::from_pem(cert.pem().as_bytes()).unwrap())
            .build()
            .unwrap();
        let err = client
            .post(format!("https://localhost:{port}/s3cr3t-path"))
            .send()
            .await
            .unwrap_err();
        let failure = delivery_error(err).into_failure();
        assert_eq!(failure.side, yagra_alert::FailureSide::Network);
        let text = failure.message;
        // Premise: this is the name-mismatch failure, which is the one that names the host.
        assert!(text.contains("not valid for name"), "{text}");
        assert!(!text.contains("localhost"), "{text}");
        assert!(!text.contains("s3cr3t-path"), "{text}");
    }

    /// ADR-195 decision 2: a refusal keeps the start of what the remote said - that is where a
    /// vendor says why - with the channel's key, URL and host taken out first.
    #[tokio::test]
    async fn a_refusal_keeps_the_remotes_reason_without_the_channels_secrets() {
        let key = "0123456789abcdef-genie";
        let url = "https://api.example.com/v2/hooks/T0LONGTOKEN123";
        let secrets = Secrets::of_url(url).with(key);
        let body = format!(
            "{{\"message\":\"Key format is not valid\",\"echo\":\"GenieKey {key} for {url}\"}}"
        );
        let resp = synth(401, &body);
        let f = refusal(resp, &secrets).await;
        assert_eq!(f.side, yagra_alert::FailureSide::Remote);
        assert_eq!(f.status, Some(401));
        let excerpt = f.response.unwrap();
        assert!(excerpt.contains("Key format is not valid"), "{excerpt}");
        assert!(!excerpt.contains(key), "{excerpt}");
        assert!(!excerpt.contains("api.example.com"), "{excerpt}");
        assert!(!excerpt.contains("T0LONGTOKEN123"), "{excerpt}");
        assert!(excerpt.contains("<redacted>"), "{excerpt}");
    }

    #[test]
    fn a_long_answer_is_cut_and_an_empty_one_is_none() {
        let secrets = Secrets::default();
        let long = "x".repeat(5000);
        let cut = response_excerpt(long.as_bytes(), &secrets).unwrap();
        assert_eq!(cut.chars().count(), RESPONSE_KEEP_MAX_CHARS);
        assert_eq!(response_excerpt(b"  \n ", &secrets), None);
        // Control characters would break a one-line display; they are flattened.
        assert_eq!(
            response_excerpt(b"a\nb\tc", &secrets).as_deref(),
            Some("a b c")
        );
    }

    /// Short path segments are the API's own words and stay readable; long ones are tokens.
    #[test]
    fn only_long_path_segments_are_treated_as_secrets() {
        let s = Secrets::of_url("https://hooks.example.com/services/T0/B0/abcdefgh12345678");
        let out = s.redact("posted to /services/T0/B0/abcdefgh12345678 on hooks.example.com");
        assert!(out.contains("/services/T0/B0/"), "{out}");
        assert!(!out.contains("abcdefgh12345678"), "{out}");
        assert!(!out.contains("hooks.example.com"), "{out}");
    }

    /// A 2xx from the vendor is success, anything else is the remote's refusal; a 429 keeps its
    /// own message so the operator sees it was rate limiting.
    #[tokio::test]
    async fn a_vendor_refusal_is_the_remotes_side_with_its_status() {
        let secrets = Secrets::default();
        assert!(vendor_response(synth(202, ""), None, &secrets)
            .await
            .is_ok());
        let f = vendor_response(synth(403, "forbidden"), None, &secrets)
            .await
            .unwrap_err()
            .into_failure();
        assert_eq!(f.side, yagra_alert::FailureSide::Remote);
        assert_eq!(f.status, Some(403));
        assert_eq!(f.response.as_deref(), Some("forbidden"));
    }

    fn synth(status: u16, body: &str) -> reqwest::Response {
        reqwest::Response::from(
            axum::http::Response::builder()
                .status(status)
                .body(body.to_owned())
                .unwrap(),
        )
    }
}
