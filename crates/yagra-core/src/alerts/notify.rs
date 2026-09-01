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

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use uuid::Uuid;
use yagra_alert::{
    Alert, DispatchOutcome, Dispatcher, Notification, NotifyChannel, NotifyError, RetryPolicy,
    Subject,
};
use yagra_common::{is_ssrf_blocked, AlertFacts, CheckId, NodeId, NotifyEvent, Severity};

use crate::notifications::{ChannelConfig, OpenChannel, RoutingRule};
use crate::notify_facts::{context_for, node_ids_for, AlertFactsSource};
use crate::notify_render::{body_must_be_json, render_with_fallback, ChannelTemplate};

use super::rules::check_id;
use super::NotifyAction;

/// A Webhook [`NotifyChannel`]: POSTs the alert JSON to a configured URL.
pub struct WebhookChannel {
    http: reqwest::Client,
    url: String,
}

impl WebhookChannel {
    #[must_use]
    pub fn new(url: String) -> Self {
        // Hardened client: a bounded timeout and — importantly for SSRF — NO redirect following.
        // A webhook endpoint that 30x-redirects to a loopback/metadata address is an escalation
        // vector, so core never follows a redirect on the notification path. (The config is static,
        // so building the client cannot fail at runtime; the fallback keeps the no-redirect policy.)
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("Yagra-core")
            .build()
            .unwrap_or_default();
        Self { http, url }
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
                return Err(NotifyError::Delivery(
                    "webhook target address is not allowed (SSRF)".to_owned(),
                ));
            }
        }
        self.http
            .post(&self.url)
            .header("content-type", "application/json")
            .body(notification.payload.clone())
            .send()
            .await
            .map_err(|e| NotifyError::Delivery(e.to_string()))?
            .error_for_status()
            .map_err(|e| NotifyError::Delivery(e.to_string()))?;
        Ok(())
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

/// The hardened outbound client shared by the vendor channels: bounded timeout, **no
/// redirect following** (SSRF — same policy as [`WebhookChannel`]).
fn hardened_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Yagra-core")
        .build()
        .unwrap_or_default()
}

/// Map a vendor API response to the channel result. 429 waits out `Retry-After` (capped
/// at 10s) and then returns `Err` so the dispatcher's retry policy counts the attempt.
/// `also_ok` admits one vendor-specific extra status (e.g. JSM close → 404 = already
/// closed, which must read as success for idempotency).
async fn vendor_response(
    resp: reqwest::Response,
    also_ok: Option<reqwest::StatusCode>,
) -> Result<(), NotifyError> {
    let status = resp.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let wait_secs = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(2);
        tokio::time::sleep(std::time::Duration::from_secs(wait_secs.min(10))).await;
        return Err(NotifyError::Delivery("rate limited (429)".to_owned()));
    }
    if status.is_success() || also_ok.is_some_and(|s| s == status) {
        return Ok(());
    }
    Err(NotifyError::Delivery(format!("unexpected status {status}")))
}

/// PagerDuty Events API v2 [`NotifyChannel`]: `trigger` on fire, `resolve` on recovery,
/// correlated by `dedup_key`. The routing key is a secret — never logged.
pub struct PagerDutyChannel {
    http: reqwest::Client,
    url: String,
    routing_key: String,
}

/// Default (US) Events API v2 endpoint; EU tenants override via the channel config.
const PAGERDUTY_DEFAULT_URL: &str = "https://events.pagerduty.com/v2/enqueue";

impl PagerDutyChannel {
    #[must_use]
    pub fn new(routing_key: String, api_url: Option<String>) -> Self {
        Self {
            http: hardened_client(),
            url: api_url.unwrap_or_else(|| PAGERDUTY_DEFAULT_URL.to_owned()),
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
                return Err(NotifyError::Delivery(
                    "PagerDuty target address is not allowed (SSRF)".to_owned(),
                ));
            }
        }
        let body = pagerduty_body(&self.routing_key, action, notification, with_payload);
        let resp = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| NotifyError::Delivery(e.to_string()))?;
        vendor_response(resp, None).await
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
        let details: serde_json::Value =
            serde_json::from_str(&notification.payload).unwrap_or(serde_json::Value::Null);
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
}

impl JsmChannel {
    #[must_use]
    pub fn new(api_url: String, api_key: String) -> Self {
        Self {
            http: hardened_client(),
            api_url: api_url.trim_end_matches('/').to_owned(),
            api_key,
        }
    }

    async fn guard(&self, url: &str) -> Result<(), NotifyError> {
        if let Ok(url) = reqwest::Url::parse(url) {
            if webhook_target_blocked(&url).await {
                return Err(NotifyError::Delivery(
                    "JSM target address is not allowed (SSRF)".to_owned(),
                ));
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
            .map_err(|e| NotifyError::Delivery(e.to_string()))?;
        vendor_response(resp, None).await
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
            .map_err(|e| NotifyError::Delivery(e.to_string()))?;
        // 404 = no open alert with that alias (already closed / never created) — success,
        // so a resolve is idempotent and never dangles on retry.
        vendor_response(resp, Some(reqwest::StatusCode::NOT_FOUND)).await
    }
}

/// The JSM/Opsgenie create-alert body (pure — unit-tested against the wire contract).
fn jsm_create_body(notification: &Notification) -> serde_json::Value {
    let priority = match notification.severity {
        Severity::Critical => "P1",
        Severity::Warning => "P3",
        Severity::Info => "P5",
    };
    serde_json::json!({
        "message": truncate_chars(&notification.summary, 130),
        "alias": dedup_string(&notification.dedup_key),
        "priority": priority,
        "description": notification.payload,
        "source": "yagra",
    })
}

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
        if let (Some(user), Some(pass)) = (user, pass) {
            builder = builder.credentials(Credentials::new(user.to_owned(), pass.to_owned()));
        }
        Some(Self {
            mailer: builder.build(),
            from,
            to,
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
fn build_channel(config: &ChannelConfig) -> Option<Arc<dyn NotifyChannel>> {
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

#[async_trait]
impl NotifyChannel for EmailChannel {
    async fn deliver(&self, notification: &Notification) -> Result<(), NotifyError> {
        use lettre::AsyncTransport;
        let email = lettre::Message::builder()
            .from(self.from.clone())
            .to(self.to.clone())
            .subject(notification.summary.clone())
            .body(notification.payload.clone())
            .map_err(|e| NotifyError::Delivery(e.to_string()))?;
        self.mailer
            .send(email)
            .await
            .map_err(|e| NotifyError::Delivery(e.to_string()))?;
        Ok(())
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
            })),
            mutes: RwLock::new(Arc::new(Vec::new())),
            facts: RwLock::new(None),
            any_templates: AtomicBool::new(false),
        }
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
        for built in channels {
            if let Some(over) = built.over {
                overrides.insert(built.id, over);
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
        self.any_templates
            .store(!overrides.is_empty(), Ordering::Relaxed);
        *slot = Arc::new(Routing {
            default: slot.default.clone(),
            channels: next,
            overrides,
            rules,
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

    /// Resolve the template context for an alert, or `None` when no channel has a template.
    ///
    /// Deliberately **before** the routing snapshot is read: this is the one part of delivery that
    /// touches the database, and there is no reason for it to be inside anything.
    async fn context(&self, alert: &Alert, event: NotifyEvent) -> Option<AlertFacts> {
        if !self.any_templates.load(Ordering::Relaxed) {
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
        let resolved = match source {
            Some(src) => src.facts(&node_ids_for(alert)).await,
            None => HashMap::new(),
        };
        Some(context_for(alert, event, &resolved))
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
                let notification = builtin_notification(&alert, NotifyEvent::Fire);
                let matched = routing.matched(alert.severity);
                if let Some(d) = routing.default.as_ref() {
                    let started = std::time::Instant::now();
                    let outcome = d.dispatch(notification.clone()).await;
                    record_dispatch("default", NotifyEvent::Fire, outcome, started);
                    tracing::info!(?outcome, subject = %alert.subject, route = "default", "alert notification dispatched");
                }
                for id in matched {
                    if let Some(d) = routing.channels.get(&id) {
                        let n = for_channel(id, &routing.overrides, facts.as_ref(), &notification);
                        let started = std::time::Instant::now();
                        let outcome = d.dispatch(n).await;
                        record_dispatch(&id.to_string(), NotifyEvent::Fire, outcome, started);
                        tracing::info!(?outcome, subject = %alert.subject, channel = %id, "alert notification dispatched");
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
        let notification = builtin_notification(alert, event);
        let matched = routing.matched(alert.severity);
        if let Some(d) = routing.default.as_ref() {
            let started = std::time::Instant::now();
            let outcome = d.dispatch_resolve(notification.clone()).await;
            record_dispatch("default", event, outcome, started);
            tracing::info!(?outcome, subject = %alert.subject, route = "default", "{}", message);
        }
        for (id, d) in &routing.channels {
            if matched.contains(id) {
                let n = for_channel(*id, &routing.overrides, facts, &notification);
                let started = std::time::Instant::now();
                let outcome = d.dispatch_resolve(n).await;
                record_dispatch(&id.to_string(), event, outcome, started);
                tracing::info!(?outcome, subject = %alert.subject, channel = %id, "{}", message);
            } else {
                d.mark_resolved(&key).await;
            }
        }
    }
}

/// The notification Yagra sends when a channel has no template — and the fallback when its
/// template cannot be used.
///
/// **Deliberately a `format!` and not a built-in template** (ADR-039 decision 3). This is what
/// every failure path lands on, so it must not depend on the machinery that just failed. It is
/// also the reason the wording lives in exactly one place: the three lifecycle points used to
/// spell it out at three separate call sites inside `handle`, which is how two of them would
/// eventually stop agreeing.
pub(crate) fn builtin_notification(alert: &Alert, event: NotifyEvent) -> Notification {
    let summary = match (&alert.subject, event) {
        (Subject::Node(node), NotifyEvent::Fire) => format!("node {node} is {}", alert.state),
        (Subject::Node(node), NotifyEvent::Resolve) => format!("resolved: node {node} recovered"),
        (Subject::Node(node), NotifyEvent::Suppress) => {
            format!("rolled up: node {node} suppressed under upstream")
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
    };
    let payload = serde_json::to_string(alert).unwrap_or_else(|_| "{}".to_owned());
    Notification::for_alert(alert, summary, payload)
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
                subject: subject.map(str::to_owned),
                body: body.map(str::to_owned),
            },
            needs_json,
        }
    }

    /// The exact text every deployment receives today. **A change here is a change to every
    /// operator's inbox**, so it is pinned rather than described: the whole N-1 story of ADR-039
    /// is that a channel with no template sends what it sent before, byte for byte.
    #[test]
    fn the_built_in_wording_is_unchanged_for_every_lifecycle_point() {
        let node = NodeId::new();
        let alert = threshold_alert(node);
        for (event, want) in [
            (NotifyEvent::Fire, format!("node {node} is critical")),
            (
                NotifyEvent::Resolve,
                format!("resolved: node {node} recovered"),
            ),
            (
                NotifyEvent::Suppress,
                format!("rolled up: node {node} suppressed under upstream"),
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
        assert!(vendor_response(synth_response(202, &[]), None)
            .await
            .is_ok());
        // Hard failure surfaces as a delivery error (dispatcher retries).
        assert!(vendor_response(synth_response(400, &[]), None)
            .await
            .is_err());
        // 429 waits out Retry-After then errs so the retry policy counts the attempt.
        let start = std::time::Instant::now();
        let r = vendor_response(synth_response(429, &[("retry-after", "0")]), None).await;
        assert!(r.is_err());
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        // JSM close treats 404 (already closed) as success — resolve stays idempotent.
        let ok404 = vendor_response(
            synth_response(404, &[]),
            Some(reqwest::StatusCode::NOT_FOUND),
        )
        .await;
        assert!(ok404.is_ok());
        assert!(vendor_response(synth_response(404, &[]), None)
            .await
            .is_err());
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
        }
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
