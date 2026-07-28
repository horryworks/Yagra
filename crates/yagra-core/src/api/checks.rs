// SPDX-License-Identifier: AGPL-3.0-only
//! Single-purpose monitor checks: **URL/HTTP** and **DNS resolution** (ADR-033).
//!
//! These two are one module because they are one shape. A node that monitors a URL, and a node
//! that monitors a name resolution, are both "a node plus a 1:1 config row in a side table", and
//! both expose the same five operations: read the config, write it, delete it, and create a node
//! already carrying it. Before this they were two verbatim copies of that code — the `get_*` pair
//! differed only in which repository handle they touched and which noun appeared in their strings.
//!
//! So the operations are written once over [`CheckKind`] and instantiated twice. The consequence
//! that matters is not the line count: it is that a rule added to one kind cannot be added to only
//! one kind. That is exactly the bug this module shipped with — the "a node is one kind" rule was
//! enforced on the DNS writer and not the URL writer, so the same node could be given both, and
//! the scheduler (which resolves kind in one place, URL first) would silently never run its DNS
//! check. [`reject_conflicting_monitor`] is now reached from the generic writer, so neither
//! direction can be forgotten.
//!
//! What stays per-kind is the part that genuinely differs: edge validation
//! ([`validate_monitor_url`] vs [`validate_dns_check`]) and what a monitor's `address` column
//! should display. DNS additionally has a read surface with no URL counterpart — the recorded
//! resolution chain and its append-on-change history.

use super::extract::{Admin, RequireManageConfig, RequireView};
use super::nodes::validate_pool_create;
use super::util::CreatedId;
use super::{AdminState, ApiError, ApiResult, ApiState};
use axum::{
    extract::{Path, Query},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr};
use uuid::Uuid;
use yagra_common::{is_ssrf_blocked, DnsCheckConfig, ProfileCategory, UrlCheckConfig};

/// The URL- and DNS-check routes, merged into `/api/v1` by [`super::router`].
pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/nodes/:node_id/url-check",
            get(get_check::<UrlCheck>)
                .put(set_check::<UrlCheck>)
                .delete(delete_check::<UrlCheck>),
        )
        .route("/api/v1/url-monitors", post(create_url_monitor))
        .route(
            "/api/v1/nodes/:node_id/dns-check",
            get(get_check::<DnsCheck>)
                .put(set_check::<DnsCheck>)
                .delete(delete_check::<DnsCheck>),
        )
        .route("/api/v1/dns-monitors", post(create_dns_monitor))
        // The recorded resolution chain — DNS only; a URL monitor has no equivalent.
        .route("/api/v1/nodes/:node_id/dns-chain", get(get_dns_chain))
        .route(
            "/api/v1/nodes/:node_id/dns-chain/history",
            get(list_dns_chain_history),
        )
}

// ── The kind abstraction ─────────────────────────────────────────────────────

/// One of the single-purpose monitor kinds a node can be bound to. **A node is exactly one kind.**
///
/// Implementations are zero-sized markers; everything is an associated item, so a handler is
/// instantiated per kind at the route (`get_check::<UrlCheck>`) with no dispatch at runtime.
///
/// Adding a third kind means implementing this and adding its routes — the CRUD, the conflict
/// guard and the create-with-rollback path come with it. (It does *not* make a new **check** kind
/// cheap overall: that still touches the poller, the transport, `CheckSpec`, the scheduler and the
/// WebUI. See `.claude/rules/extensibility.md` §6 — this module is one of ~10 surfaces.)
pub(crate) trait CheckKind: Send + Sync + 'static {
    /// The operator-editable config persisted 1:1 with the node.
    type Config: Serialize + DeserializeOwned + Send + Sync + 'static;

    /// The kind in prose, for messages and log context: `"url check"`.
    const NOUN: &'static str;
    /// The `404` code when the node carries no check of this kind. Spelled out per kind rather
    /// than built with `format!` so every code a client can match on is greppable in this repo.
    const NOT_FOUND_CODE: &'static str;
    /// The *other* kind's label, as it appears in a conflict message.
    const CONFLICTS_WITH: &'static str;
    /// The built-in profile a node of this kind is bound to on creation, so it inherits that
    /// kind's default thresholds.
    const CATEGORY: ProfileCategory;

    fn load(
        admin: &AdminState,
        node_id: Uuid,
    ) -> impl Future<Output = anyhow::Result<Option<Self::Config>>> + Send;

    fn save(
        admin: &AdminState,
        node_id: Uuid,
        cfg: &Self::Config,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;

    fn remove(
        admin: &AdminState,
        node_id: Uuid,
    ) -> impl Future<Output = anyhow::Result<bool>> + Send;

    /// Does the node already carry the *other* kind?
    fn conflict_exists(
        admin: &AdminState,
        node_id: Uuid,
    ) -> impl Future<Output = anyhow::Result<bool>> + Send;

    /// Validate and normalize an operator-supplied config at the API edge (security.md: parse into
    /// checked values here, not at the point of use).
    fn validate(cfg: Self::Config) -> Result<Self::Config, ApiError>;

    /// The `address` column for a node of this kind. **Display only** — neither kind is pinged,
    /// and the probe re-derives its real target every poll — so this is best-effort and must not
    /// fail the request.
    fn display_address(cfg: &Self::Config) -> impl Future<Output = IpAddr> + Send;
}

/// The unspecified address: what a monitor shows when it has no meaningful single IP.
const NO_ADDRESS: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

/// HTTP/HTTPS endpoint monitoring.
pub(crate) struct UrlCheck;

impl CheckKind for UrlCheck {
    type Config = UrlCheckConfig;

    const NOUN: &'static str = "url check";
    const NOT_FOUND_CODE: &'static str = "url_check_not_found";
    const CONFLICTS_WITH: &'static str = "DNS";
    const CATEGORY: ProfileCategory = ProfileCategory::UrlCheck;

    async fn load(admin: &AdminState, node_id: Uuid) -> anyhow::Result<Option<Self::Config>> {
        admin.url_checks.get(node_id).await
    }

    async fn save(admin: &AdminState, node_id: Uuid, cfg: &Self::Config) -> anyhow::Result<()> {
        admin.url_checks.upsert(node_id, cfg).await
    }

    async fn remove(admin: &AdminState, node_id: Uuid) -> anyhow::Result<bool> {
        admin.url_checks.delete(node_id).await
    }

    async fn conflict_exists(admin: &AdminState, node_id: Uuid) -> anyhow::Result<bool> {
        Ok(admin.dns_checks.get(node_id).await?.is_some())
    }

    fn validate(cfg: Self::Config) -> Result<Self::Config, ApiError> {
        validate_monitor_url(&cfg.url)?;
        Ok(cfg)
    }

    async fn display_address(cfg: &Self::Config) -> IpAddr {
        match validate_monitor_url(&cfg.url) {
            Ok(url) => resolve_monitor_address(&url).await,
            Err(_) => NO_ADDRESS,
        }
    }
}

/// DNS name-resolution monitoring (ADR-033).
pub(crate) struct DnsCheck;

impl CheckKind for DnsCheck {
    type Config = DnsCheckConfig;

    const NOUN: &'static str = "dns check";
    const NOT_FOUND_CODE: &'static str = "dns_check_not_found";
    const CONFLICTS_WITH: &'static str = "URL";
    const CATEGORY: ProfileCategory = ProfileCategory::DnsCheck;

    async fn load(admin: &AdminState, node_id: Uuid) -> anyhow::Result<Option<Self::Config>> {
        admin.dns_checks.get(node_id).await
    }

    async fn save(admin: &AdminState, node_id: Uuid, cfg: &Self::Config) -> anyhow::Result<()> {
        admin.dns_checks.upsert(node_id, cfg).await
    }

    async fn remove(admin: &AdminState, node_id: Uuid) -> anyhow::Result<bool> {
        admin.dns_checks.delete(node_id).await
    }

    async fn conflict_exists(admin: &AdminState, node_id: Uuid) -> anyhow::Result<bool> {
        Ok(admin.url_checks.get(node_id).await?.is_some())
    }

    fn validate(cfg: Self::Config) -> Result<Self::Config, ApiError> {
        validate_dns_check(&cfg)
    }

    async fn display_address(cfg: &Self::Config) -> IpAddr {
        // A DNS monitor is never pinged, so show the resolver we ask — or nothing when the
        // poller's system resolver is used.
        cfg.resolver.unwrap_or(NO_ADDRESS)
    }
}

// ── The shared operations ────────────────────────────────────────────────────

/// Refuse to bind `K` to a node that already carries the *other* kind.
///
/// The scheduler resolves a node's kind in one place ([`crate::scheduler::SpecialMonitor::resolve`])
/// and URL wins, so a node holding both rows would have its DNS check silently never run. Guarding
/// only the DNS writer — which is what shipped — just moved the hole to the URL writer. Node
/// *creation* needs no guard: those paths make a fresh node.
async fn reject_conflicting_monitor<K: CheckKind>(
    admin: &AdminState,
    node_id: Uuid,
) -> Result<(), ApiError> {
    let conflicting = K::conflict_exists(admin, node_id).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "monitor-kind conflict lookup",
            "failed to check monitor kind",
        )
    })?;
    if conflicting {
        // Name the kind the node *already* has, not the incoming one: an operator setting a URL
        // must not be told their node "is already a URL monitor".
        return Err(ApiError::conflict(
            "conflicting_monitor_kind",
            format!("node is already a {} monitor", K::CONFLICTS_WITH),
        ));
    }
    Ok(())
}

/// The check config for a node, or `404` if the node is not that kind of monitor.
async fn get_check<K: CheckKind>(
    _perm: RequireView,
    admin: Admin,
    Path(node_id): Path<Uuid>,
) -> ApiResult<Json<K::Config>> {
    let cfg = K::load(&admin, node_id).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            &format!("get {}", K::NOUN),
            format!("failed to load {}", K::NOUN),
        )
    })?;
    cfg.map(Json).ok_or_else(|| {
        ApiError::not_found(
            K::NOT_FOUND_CODE,
            format!("no {} for node {node_id}", K::NOUN),
        )
    })
}

/// Create or replace a node's check config. The node must already exist and must not already be
/// the other kind of monitor.
async fn set_check<K: CheckKind>(
    _perm: RequireManageConfig,
    admin: Admin,
    Path(node_id): Path<Uuid>,
    Json(cfg): Json<K::Config>,
) -> ApiResult<StatusCode> {
    let cfg = K::validate(cfg)?;
    let exists = admin
        .repo
        .get_node(node_id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                &format!("set {}: load node", K::NOUN),
                "failed to load node",
            )
        })?
        .is_some();
    if !exists {
        return Err(ApiError::not_found(
            "node_not_found",
            format!("no node {node_id}"),
        ));
    }
    reject_conflicting_monitor::<K>(&admin, node_id).await?;
    K::save(&admin, node_id, &cfg).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            &format!("set {}", K::NOUN),
            format!("failed to save {}", K::NOUN),
        )
    })?;
    Ok(StatusCode::NO_CONTENT)
}

/// Remove a node's check config. The node itself — and, for DNS, its recorded chains — are
/// untouched.
async fn delete_check<K: CheckKind>(
    _perm: RequireManageConfig,
    admin: Admin,
    Path(node_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let removed = K::remove(&admin, node_id).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            &format!("delete {}", K::NOUN),
            format!("failed to delete {}", K::NOUN),
        )
    })?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            K::NOT_FOUND_CODE,
            format!("no {} for node {node_id}", K::NOUN),
        ))
    }
}

/// The node fields every "create a monitor in one call" body carries, whatever the kind.
struct NewMonitor {
    /// Display name for the node.
    name: String,
    parent_id: Option<Uuid>,
    pool: Option<String>,
}

/// Create a monitor in one call: a node bound to the kind's built-in profile (so it inherits that
/// kind's default thresholds) plus its check config.
///
/// Both writes are one logical unit. If the check does not save, the node is **rolled back** —
/// a node of a monitor category with no check row is invisible to the scheduler and would sit in
/// the inventory forever producing nothing, which reads to an operator as a monitoring gap rather
/// than as a failed creation.
async fn create_monitor<K: CheckKind>(
    admin: &AdminState,
    node: NewMonitor,
    config: K::Config,
) -> Result<Uuid, ApiError> {
    let name = node.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_name",
            "monitor name must not be empty",
        ));
    }
    let config = K::validate(config)?;
    let pool = validate_pool_create(node.pool)?;
    // Absent built-in profile is not fatal: the monitor still runs, it just has no inherited
    // thresholds until one is bound.
    let profile = admin
        .repo
        .profile_id_for_category(K::CATEGORY.as_str())
        .await
        .unwrap_or(None);
    let address = K::display_address(&config).await;
    let node_id = admin
        .repo
        .create_node(
            name,
            address,
            pool.as_deref(),
            profile,
            None,
            node.parent_id,
            None,
            None,
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                &format!("create {} monitor: create node", K::NOUN),
                format!("failed to create {} monitor", K::NOUN),
            )
        })?;
    if let Err(e) = K::save(admin, node_id, &config).await {
        let _ = admin.repo.delete_node(node_id).await;
        return Err(ApiError::from_internal(
            e.as_ref(),
            &format!("create {} monitor: save check", K::NOUN),
            format!("failed to create {} monitor", K::NOUN),
        ));
    }
    Ok(node_id)
}

// ── URL monitoring ───────────────────────────────────────────────────────────

/// `name`/`parent_id`/`pool` plus a flattened [`UrlCheckConfig`] (only `url` is required;
/// everything else defaults).
#[derive(Deserialize)]
struct CreateUrlMonitor {
    name: String,
    #[serde(default)]
    parent_id: Option<Uuid>,
    #[serde(default)]
    pool: Option<String>,
    #[serde(flatten)]
    config: UrlCheckConfig,
}

async fn create_url_monitor(
    _perm: RequireManageConfig,
    admin: Admin,
    Json(body): Json<CreateUrlMonitor>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    let id = create_monitor::<UrlCheck>(
        &admin,
        NewMonitor {
            name: body.name,
            parent_id: body.parent_id,
            pool: body.pool,
        },
        body.config,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

/// Validate an operator-supplied monitor URL at the API edge (security.md: validate input; ADR
/// §229: SSRF). Scheme must be http/https; an IP-literal host that is loopback/link-local
/// (incl. cloud metadata)/multicast/unspecified is refused. Hostnames are allowed here (the
/// transport does a deeper resolved-address check before the request). `Ok` ⇒ the parsed URL.
fn validate_monitor_url(url: &str) -> Result<reqwest::Url, ApiError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| ApiError::bad_request("invalid_url", format!("{url:?} is not a valid URL")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(ApiError::bad_request(
            "invalid_url_scheme",
            "url scheme must be http or https",
        ));
    }
    let Some(host) = parsed.host_str() else {
        return Err(ApiError::bad_request("invalid_url", "url must have a host"));
    };
    // `host_ip`, not `host.parse()`: an IPv6 literal arrives bracketed, so parsing it directly
    // fails and every v6 address reads as a hostname that skips this check entirely.
    if let Some(ip) = yagra_common::host_ip(host) {
        if is_ssrf_blocked(ip) {
            return Err(ApiError::bad_request(
                "blocked_target",
                "target address is not allowed (loopback / link-local / metadata)",
            ));
        }
    }
    Ok(parsed)
}

/// Best-effort resolve a URL's host to a single management IP for the node's `address` column.
/// Falls back to the unspecified address if the host cannot be resolved.
async fn resolve_monitor_address(url: &reqwest::Url) -> IpAddr {
    let Some(host) = url.host_str() else {
        return NO_ADDRESS;
    };
    if let Some(ip) = yagra_common::host_ip(host) {
        return ip;
    }
    let port = url.port_or_known_default().unwrap_or(443);
    match tokio::net::lookup_host((host, port)).await {
        Ok(mut addrs) => addrs.next().map_or(NO_ADDRESS, |a| a.ip()),
        Err(_) => NO_ADDRESS,
    }
}

// ── DNS monitoring (ADR-033) ─────────────────────────────────────────────────

/// Largest history page a client may request.
const DNS_HISTORY_MAX_LIMIT: i64 = 200;
/// Page size when the client does not ask for one.
const DNS_HISTORY_DEFAULT_LIMIT: i64 = 50;

/// Validate an operator-supplied DNS check at the API edge, returning it with the name normalized.
///
/// Two things are enforced here (security.md — parse into checked values at the edge): the name
/// must be a syntactically valid DNS name, and a named resolver must not be an SSRF-blocked
/// address. The resolver policy is deliberately *not* the URL policy: an NMS legitimately points
/// at internal DNS servers (RFC1918/ULA allowed), while loopback, link-local (including
/// 169.254.169.254), multicast and the unspecified address are never real targets. The transport
/// re-checks before opening a socket (defense in depth).
fn validate_dns_check(cfg: &DnsCheckConfig) -> Result<DnsCheckConfig, ApiError> {
    let Some(name) = yagra_common::validate_dns_name(&cfg.name) else {
        return Err(ApiError::bad_request(
            "invalid_dns_name",
            format!("not a usable DNS name: {}", cfg.name),
        ));
    };
    if let Some(ip) = cfg.resolver {
        if yagra_common::is_resolver_blocked(ip) {
            return Err(ApiError::bad_request(
                "blocked_resolver",
                format!("resolver address is not allowed: {ip}"),
            ));
        }
    }
    if cfg.resolver_port == 0 {
        return Err(ApiError::bad_request(
            "invalid_resolver_port",
            "resolver port must be 1-65535",
        ));
    }
    if !(1..=16).contains(&cfg.max_depth) {
        return Err(ApiError::bad_request(
            "invalid_max_depth",
            "max_depth must be 1-16",
        ));
    }
    if !(100..=30_000).contains(&cfg.timeout_ms) {
        return Err(ApiError::bad_request(
            "invalid_timeout",
            "timeout_ms must be 100-30000",
        ));
    }
    Ok(DnsCheckConfig {
        name,
        ..cfg.clone()
    })
}

/// The check config is spelled out rather than `#[serde(flatten)]`ed (as [`CreateUrlMonitor`]
/// does) because both the node and the check have a `name`: flattening would bind them to the same
/// JSON key and silently force the display label to equal the resolved name.
#[derive(Deserialize)]
struct CreateDnsMonitor {
    /// Display name for the node.
    name: String,
    /// The DNS name to resolve.
    dns_name: String,
    #[serde(default)]
    parent_id: Option<Uuid>,
    #[serde(default)]
    pool: Option<String>,
    #[serde(default)]
    record_type: yagra_common::DnsRecordType,
    #[serde(default)]
    resolver: Option<IpAddr>,
    #[serde(default)]
    resolver_port: Option<u16>,
    #[serde(default)]
    max_depth: Option<u8>,
    #[serde(default)]
    timeout_ms: Option<u32>,
}

impl CreateDnsMonitor {
    /// The check config this request describes, with server-side defaults filled in.
    fn config(&self) -> DnsCheckConfig {
        let base = DnsCheckConfig::new(self.dns_name.clone());
        DnsCheckConfig {
            record_type: self.record_type,
            resolver: self.resolver,
            resolver_port: self.resolver_port.unwrap_or(base.resolver_port),
            max_depth: self.max_depth.unwrap_or(base.max_depth),
            timeout_ms: self.timeout_ms.unwrap_or(base.timeout_ms),
            ..base
        }
    }
}

async fn create_dns_monitor(
    _perm: RequireManageConfig,
    admin: Admin,
    Json(body): Json<CreateDnsMonitor>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    let config = body.config();
    let id = create_monitor::<DnsCheck>(
        &admin,
        NewMonitor {
            name: body.name,
            parent_id: body.parent_id,
            pool: body.pool,
        },
        config,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

/// The node's current resolution chain, plus how long it has held.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DnsChainCurrent {
    chain: yagra_common::DnsChain,
    resolved: bool,
    failure_kind: Option<String>,
    first_seen: String,
    last_seen: String,
}

async fn get_dns_chain(
    _perm: RequireView,
    admin: Admin,
    Path(node_id): Path<Uuid>,
) -> ApiResult<Json<DnsChainCurrent>> {
    let current = admin
        .dns_checks
        .current_chain(node_id)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "get dns chain", "failed to load dns chain")
        })?
        .ok_or_else(|| {
            ApiError::not_found(
                "dns_chain_not_found",
                format!("no resolution recorded for node {node_id}"),
            )
        })?;
    Ok(Json(DnsChainCurrent {
        chain: current.chain,
        resolved: current.resolved,
        failure_kind: current.failure_kind,
        first_seen: current.first_seen.to_rfc3339(),
        last_seen: current.last_seen.to_rfc3339(),
    }))
}

/// One append-on-change history row.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DnsChainChange {
    id: i64,
    at: String,
    chain: yagra_common::DnsChain,
    resolved: bool,
    failure_kind: Option<String>,
    prev_chain_key: Option<String>,
}

/// Keyset cursor for the next page (ADR-019 — never OFFSET).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DnsHistoryCursor {
    at: String,
    id: i64,
}

/// One page of chain changes, newest first.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DnsChainHistory {
    changes: Vec<DnsChainChange>,
    /// Pass back as `before_at`+`before_id` for the next page; `null` ⇒ this was the last one.
    next: Option<DnsHistoryCursor>,
}

#[derive(Deserialize)]
struct DnsHistoryQuery {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    before_at: Option<String>,
    #[serde(default)]
    before_id: Option<i64>,
}

impl DnsHistoryQuery {
    /// The keyset cursor this query names, if any.
    ///
    /// Both halves or neither. A half-specified cursor is rejected rather than ignored: silently
    /// dropping it restarts paging from the top, so a client walking the history would loop over
    /// the first page forever while looking like it was making progress.
    fn cursor(&self) -> Result<Option<(chrono::DateTime<chrono::Utc>, i64)>, ApiError> {
        match (self.before_at.as_deref(), self.before_id) {
            (Some(at), Some(id)) => {
                let ts = super::parse_rfc3339(at).ok_or_else(|| {
                    ApiError::bad_request("invalid_cursor", "before_at must be RFC 3339")
                })?;
                Ok(Some((ts, id)))
            }
            (None, None) => Ok(None),
            _ => Err(ApiError::bad_request(
                "invalid_cursor",
                "before_at and before_id must be given together",
            )),
        }
    }
}

async fn list_dns_chain_history(
    _perm: RequireView,
    admin: Admin,
    Path(node_id): Path<Uuid>,
    Query(q): Query<DnsHistoryQuery>,
) -> ApiResult<Json<DnsChainHistory>> {
    let limit = q
        .limit
        .unwrap_or(DNS_HISTORY_DEFAULT_LIMIT)
        .clamp(1, DNS_HISTORY_MAX_LIMIT);
    let before = q.cursor()?;
    let rows = admin
        .dns_checks
        .list_changes(node_id, before, limit)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "list dns chain history",
                "failed to load dns chain history",
            )
        })?;
    // A cursor only when the page came back full — a short page is the end of the history, and
    // handing back a cursor there makes a client fetch an empty page to discover that.
    let next = rows
        .last()
        .filter(|_| i64::try_from(rows.len()).unwrap_or(0) == limit)
        .map(|r| DnsHistoryCursor {
            at: r.at.to_rfc3339(),
            id: r.id,
        });
    Ok(Json(DnsChainHistory {
        changes: rows
            .into_iter()
            .map(|r| DnsChainChange {
                id: r.id,
                at: r.at.to_rfc3339(),
                chain: r.chain,
                resolved: r.resolved,
                failure_kind: r.failure_kind,
                prev_chain_key: r.prev_chain_key,
            })
            .collect(),
        next,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::Body;
    use axum::http::{header::AUTHORIZATION, Request};
    use tower::ServiceExt;
    use yagra_common::{Principal, Role, Scope};

    /// A concrete node id, so routing can never be what rejects a request below.
    const NODE: &str = "00000000-0000-0000-0000-000000000001";

    /// **Every** method/path this module serves. The guard tests iterate it, so a route left out of
    /// this list is a route whose authorization nothing checks — and the whole point of moving a
    /// domain is that its guards are uniform.
    fn routes_under_test() -> Vec<(&'static str, String)> {
        vec![
            ("GET", format!("/api/v1/nodes/{NODE}/url-check")),
            ("PUT", format!("/api/v1/nodes/{NODE}/url-check")),
            ("DELETE", format!("/api/v1/nodes/{NODE}/url-check")),
            ("POST", "/api/v1/url-monitors".to_owned()),
            ("GET", format!("/api/v1/nodes/{NODE}/dns-check")),
            ("PUT", format!("/api/v1/nodes/{NODE}/dns-check")),
            ("DELETE", format!("/api/v1/nodes/{NODE}/dns-check")),
            ("POST", "/api/v1/dns-monitors".to_owned()),
            ("GET", format!("/api/v1/nodes/{NODE}/dns-chain")),
            ("GET", format!("/api/v1/nodes/{NODE}/dns-chain/history")),
        ]
    }

    #[test]
    fn the_conflict_message_names_the_kind_the_node_already_has() {
        // The guard is symmetric (it used to exist on the DNS writer only), so the message must
        // name the *existing* kind, not the incoming one — otherwise the URL direction would tell
        // an operator their node "is already a URL monitor" while they were setting a URL.
        assert_eq!(UrlCheck::CONFLICTS_WITH, "DNS");
        assert_eq!(DnsCheck::CONFLICTS_WITH, "URL");
    }

    #[test]
    fn each_kind_binds_to_its_own_built_in_profile() {
        // Getting this backwards would give a URL monitor the DNS default thresholds — which
        // evaluate against a metric it never emits, so it would simply never alert.
        assert_eq!(UrlCheck::CATEGORY, ProfileCategory::UrlCheck);
        assert_eq!(DnsCheck::CATEGORY, ProfileCategory::DnsCheck);
        assert_ne!(UrlCheck::NOT_FOUND_CODE, DnsCheck::NOT_FOUND_CODE);
    }

    #[test]
    fn a_monitor_url_must_be_http_and_must_not_point_inward() {
        assert!(validate_monitor_url("https://example.com/health").is_ok());
        assert!(validate_monitor_url("http://192.0.2.10:8080/up").is_ok());
        // A hostname is allowed through here even if it resolves inward — the transport re-checks
        // the resolved address before opening a socket, which is the check that can actually see it.
        assert!(validate_monitor_url("https://internal.corp.example/").is_ok());

        for (url, code) in [
            ("ftp://example.com/x", "invalid_url_scheme"),
            ("file:///etc/passwd", "invalid_url_scheme"),
            ("not a url", "invalid_url"),
            // The cloud metadata endpoint is the one that turns an NMS into a credential leak.
            ("http://169.254.169.254/latest/meta-data/", "blocked_target"),
            ("http://127.0.0.1:8080/", "blocked_target"),
            ("http://[::1]/", "blocked_target"),
        ] {
            let err = validate_monitor_url(url).expect_err(url);
            assert_eq!(err.code(), code, "{url}");
            assert_eq!(err.status(), StatusCode::BAD_REQUEST, "{url}");
        }
    }

    #[test]
    fn a_dns_check_is_normalized_and_its_resolver_is_policed() {
        let cfg = DnsCheckConfig::new("WWW.Example.COM.".to_owned());
        let ok = validate_dns_check(&cfg).expect("a plain name");
        // Normalized on the way in, so the same name typed two ways is one check.
        assert_eq!(ok.name, "www.example.com");

        // An internal resolver is legitimate for an NMS and must stay allowed — this is where the
        // DNS policy deliberately differs from the URL one.
        let internal = DnsCheckConfig {
            resolver: Some("10.0.0.53".parse().unwrap()),
            ..DnsCheckConfig::new("example.com".to_owned())
        };
        assert!(validate_dns_check(&internal).is_ok());

        let blocked = DnsCheckConfig {
            resolver: Some("169.254.169.254".parse().unwrap()),
            ..DnsCheckConfig::new("example.com".to_owned())
        };
        assert_eq!(
            validate_dns_check(&blocked).expect_err("metadata").code(),
            "blocked_resolver"
        );

        for (cfg, code) in [
            (
                DnsCheckConfig::new("not a hostname".to_owned()),
                "invalid_dns_name",
            ),
            (
                DnsCheckConfig {
                    resolver_port: 0,
                    ..DnsCheckConfig::new("example.com".to_owned())
                },
                "invalid_resolver_port",
            ),
            (
                DnsCheckConfig {
                    max_depth: 0,
                    ..DnsCheckConfig::new("example.com".to_owned())
                },
                "invalid_max_depth",
            ),
            (
                DnsCheckConfig {
                    timeout_ms: 50,
                    ..DnsCheckConfig::new("example.com".to_owned())
                },
                "invalid_timeout",
            ),
        ] {
            assert_eq!(validate_dns_check(&cfg).expect_err(code).code(), code);
        }
    }

    #[test]
    fn a_half_specified_history_cursor_is_rejected_rather_than_ignored() {
        let none = DnsHistoryQuery {
            limit: None,
            before_at: None,
            before_id: None,
        };
        assert!(none.cursor().expect("no cursor").is_none());

        let both = DnsHistoryQuery {
            limit: None,
            before_at: Some("2026-07-28T00:00:00Z".to_owned()),
            before_id: Some(42),
        };
        let (at, id) = both.cursor().expect("a full cursor").expect("some");
        assert_eq!(at.to_rfc3339(), "2026-07-28T00:00:00+00:00");
        assert_eq!(id, 42);

        // Ignoring a half cursor would page from the top forever while looking like progress.
        for half in [
            DnsHistoryQuery {
                limit: None,
                before_at: Some("2026-07-28T00:00:00Z".to_owned()),
                before_id: None,
            },
            DnsHistoryQuery {
                limit: None,
                before_at: None,
                before_id: Some(42),
            },
        ] {
            assert_eq!(
                half.cursor().expect_err("half cursor").code(),
                "invalid_cursor"
            );
        }

        let unparseable = DnsHistoryQuery {
            limit: None,
            before_at: Some("yesterday".to_owned()),
            before_id: Some(1),
        };
        assert_eq!(
            unparseable.cursor().expect_err("bad stamp").code(),
            "invalid_cursor"
        );
    }

    #[tokio::test]
    async fn a_url_monitors_display_address_never_fails_the_request() {
        // Display only: an unresolvable or malformed URL must still produce a node, because the
        // probe re-resolves the URL every poll and never uses this column.
        let literal = UrlCheckConfig::new("https://192.0.2.10/health".to_owned());
        assert_eq!(
            UrlCheck::display_address(&literal).await,
            "192.0.2.10".parse::<IpAddr>().unwrap()
        );

        let unresolvable = UrlCheckConfig::new("https://no-such-host.invalid/health".to_owned());
        assert_eq!(UrlCheck::display_address(&unresolvable).await, NO_ADDRESS);
    }

    #[tokio::test]
    async fn a_dns_monitor_displays_its_resolver_or_nothing() {
        let system = DnsCheckConfig::new("example.com".to_owned());
        assert_eq!(DnsCheck::display_address(&system).await, NO_ADDRESS);

        let explicit = DnsCheckConfig {
            resolver: Some("10.0.0.53".parse().unwrap()),
            ..DnsCheckConfig::new("example.com".to_owned())
        };
        assert_eq!(
            DnsCheck::display_address(&explicit).await,
            "10.0.0.53".parse::<IpAddr>().unwrap()
        );
    }

    async fn status_of(st: ApiState, method: &str, path: &str, token: Option<&str>) -> StatusCode {
        let mut b = Request::builder().method(method).uri(path);
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        let body = if matches!(method, "POST" | "PUT") {
            b = b.header("content-type", "application/json");
            Body::from("{}")
        } else {
            Body::empty()
        };
        router(st)
            .oneshot(b.body(body).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn an_anonymous_caller_is_told_it_is_anonymous_and_nothing_else() {
        // Guard order: `Require*` runs before `Admin`, so an unauthenticated caller gets 401 and
        // never the 503 that would reveal whether this deployment has a write side at all. Every
        // route here answered 503 first before the migration — this is the deliberate change.
        for (method, path) in routes_under_test() {
            assert_eq!(
                status_of(private_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn a_public_dashboard_opens_the_reads_and_none_of_the_writes() {
        // `public_dashboard` is a display mode, not an authorization mode. Editing what a node
        // monitors from an unauthenticated browser would be a config-write hole.
        for (method, path) in routes_under_test() {
            let want = if method == "GET" {
                StatusCode::SERVICE_UNAVAILABLE // View is open; the request stops at availability
            } else {
                StatusCode::UNAUTHORIZED
            };
            assert_eq!(
                status_of(public_state(), method, &path, None).await,
                want,
                "{method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn writing_a_check_takes_manage_config_which_an_operator_does_not_hold() {
        // What a node monitors is configuration, so the writes sit behind `ManageConfig` (Admin),
        // not the operational `AckAlerts`/`ManageMaintenance` an operator carries. 403 and 401 must
        // stay distinguishable — "not logged in" and "not allowed" are different fixes.
        let st = private_state();
        for (role, name) in [(Role::Viewer, "viewer1"), (Role::Operator, "op1")] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), name);
            for (method, path) in routes_under_test() {
                let want = if method == "GET" {
                    StatusCode::SERVICE_UNAVAILABLE // both roles hold View
                } else {
                    StatusCode::FORBIDDEN
                };
                assert_eq!(
                    status_of(st.clone(), method, &path, Some(&token)).await,
                    want,
                    "{role:?} {method} {path}"
                );
            }
        }
    }

    #[tokio::test]
    async fn an_admin_clears_every_guard_and_stops_only_at_availability() {
        // Pins that each route is wired to a marker an admin satisfies: a route given the wrong
        // one would 403 here, which no other test in this file would catch.
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Admin, Scope::All),
            "admin1",
        );
        for (method, path) in routes_under_test() {
            assert_eq!(
                status_of(st.clone(), method, &path, Some(&token)).await,
                StatusCode::SERVICE_UNAVAILABLE,
                "{method} {path}"
            );
        }
    }
}
