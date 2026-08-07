// SPDX-License-Identifier: AGPL-3.0-only
//! CDP/LLDP adjacency: what a node currently sees on its ports, how that changed over time, and
//! whether this deployment collects it at all (ADR-038).
//!
//! The two reads are per node and node-scoped; the settings are deployment-wide and admin-only.
//! They are separate endpoints rather than more fields on `/api/v1/settings/retention` because they
//! answer a different question and carry a different justification — retention is "how long is
//! anything kept", this is "is a walk being issued, and how often".
//!
//! ⚠️ **`NeighborConfig` now carries three walks, not one**: CDP/LLDP adjacency (ADR-038), interface
//! addresses (ADR-043) and the ARP/ND cache (ADR-043 Increment 3). The name and its first two field
//! names describe only the first, and are kept verbatim because renaming them would break every
//! existing client for no gain. Each later pair is `Option`, where absent means "leave that setting
//! as it is" — the difference between an additive field and a silent regression for a client that
//! predates it.
//!
//! Nothing here writes to a device, and no response carries a credential: adjacency is descriptive
//! device data (a chassis id, a port name, a peer's sysDescr), untrusted and rendered as such.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig, RequireView, VisibleNode};
use super::ApiState;
use crate::neighbors::{self, AdjacencySettings};
use axum::extract::{Path, Query};
use axum::{http::StatusCode, routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yagra_common::NeighborSet;

/// Default page size for the change history.
const HISTORY_DEFAULT_LIMIT: i64 = 50;
/// Hard cap on the page size — an unbounded page is a DoS vector (api-conventions).
const HISTORY_MAX_LIMIT: i64 = 200;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    get_neighbors,
    list_neighbor_history,
    get_adjacency_settings,
    update_neighbor_settings
))]
pub(super) struct Doc;

/// The neighbour routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/nodes/:node_id/neighbors", get(get_neighbors))
        .route(
            "/api/v1/nodes/:node_id/neighbors/history",
            get(list_neighbor_history),
        )
        .route(
            "/api/v1/settings/neighbors",
            get(get_adjacency_settings).put(update_neighbor_settings),
        )
}

// ── Per-node reads ───────────────────────────────────────────────────────────

/// A node's current adjacency and how long it has held.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct CurrentNeighbors {
    /// The adjacencies the node last reported.
    neighbors: NeighborSet,
    /// When this exact set was first seen (RFC 3339).
    first_seen: String,
    /// When it was last confirmed unchanged (RFC 3339).
    last_seen: String,
}

/// One append-on-change history row.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct NeighborChange {
    id: i64,
    /// When the change was recorded (RFC 3339).
    at: String,
    /// The adjacency as of this change.
    neighbors: NeighborSet,
    /// The content key this replaced; `null` marks the first observation ever recorded for the node.
    prev_neighbor_key: Option<String>,
}

/// Keyset cursor for the next page (ADR-019 — never OFFSET).
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct NeighborHistoryCursor {
    at: String,
    id: i64,
}

/// One page of adjacency changes, newest first.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct NeighborHistory {
    changes: Vec<NeighborChange>,
    /// Pass back as `before_at`+`before_id` for the next page; `null` ⇒ this was the last one.
    next: Option<NeighborHistoryCursor>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct HistoryQuery {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    before_at: Option<String>,
    #[serde(default)]
    before_id: Option<i64>,
}

impl HistoryQuery {
    /// The keyset cursor this query names, if any.
    fn cursor(&self) -> Result<Option<(chrono::DateTime<chrono::Utc>, i64)>, ApiError> {
        parse_history_cursor(self.before_at.as_deref(), self.before_id)
    }
}

/// Parse a keyset cursor: both halves or neither.
///
/// A half-specified cursor is **rejected rather than ignored**, and that is the whole reason this is
/// a named function rather than an inline `if let`: silently dropping it restarts paging from the
/// top, so a client walking the history loops over the first page forever while looking like it is
/// making progress. Any second surface that pages this history has to get the same answer.
pub(crate) fn parse_history_cursor(
    before_at: Option<&str>,
    before_id: Option<i64>,
) -> Result<Option<(chrono::DateTime<chrono::Utc>, i64)>, ApiError> {
    match (before_at, before_id) {
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

/// The node's current CDP/LLDP neighbours.
///
/// `404` means no walk has recorded anything for this node yet — the node may not be an SNMP
/// device, may not speak either protocol, or may simply not have been walked since collection was
/// enabled. It is distinct from a recorded **empty** set, which is a real answer meaning the device
/// reports no neighbours.
#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/neighbors", tag = "neighbors",
    params(("node_id" = Uuid, Path, description = "Node id")),
    responses(
        (status = 200, description = "The node's current adjacency and how long it has held", body = CurrentNeighbors),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
        (status = 404, description = "No adjacency has been recorded for the node", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn get_neighbors(
    _perm: RequireView,
    _visible: VisibleNode,
    admin: Admin,
    Path(node_id): Path<Uuid>,
) -> ApiResult<Json<CurrentNeighbors>> {
    Ok(Json(current_neighbors(&admin, node_id).await?))
}

/// One node's current adjacency, with the "nothing recorded" rule.
///
/// The `404` is the load-bearing part and the reason this is a function rather than three lines
/// repeated: **no walk recorded is not an empty set**. A device that genuinely reports no neighbours
/// has a recorded empty set, which is a real answer; a node that was never walked has nothing, and
/// answering `[]` for it would tell an operator — or a model — that the device has no adjacency
/// when what is true is that nobody has looked.
pub(crate) async fn current_neighbors(
    admin: &super::AdminState,
    node_id: Uuid,
) -> ApiResult<CurrentNeighbors> {
    let current = admin
        .neighbors
        .current(node_id)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "get neighbors", "failed to load neighbours")
        })?
        .ok_or_else(|| {
            ApiError::not_found(
                "neighbors_not_found",
                format!("no adjacency recorded for node {node_id}"),
            )
        })?;
    Ok(CurrentNeighbors {
        neighbors: current.set,
        first_seen: current.first_seen.to_rfc3339(),
        last_seen: current.last_seen.to_rfc3339(),
    })
}

/// The node's adjacency change history, newest first.
///
/// A row is written only when the adjacency actually changed, so a quiet rack produces none. The
/// content key deliberately excludes the agent's own churn (LLDP's `TimeMark` and remote index), so
/// a row here means a port genuinely started or stopped facing something, or the peer on it changed.
#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/neighbors/history", tag = "neighbors",
    params(("node_id" = Uuid, Path, description = "Node id"), HistoryQuery),
    responses(
        (status = 200, description = "One page of adjacency changes, newest first", body = NeighborHistory),
        (status = 400, description = "before_at and before_id must be given together, and before_at must be RFC 3339", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_neighbor_history(
    _perm: RequireView,
    _visible: VisibleNode,
    admin: Admin,
    Path(node_id): Path<Uuid>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<NeighborHistory>> {
    Ok(Json(
        neighbor_history(&admin, node_id, q.cursor()?, q.limit).await?,
    ))
}

/// One page of a node's adjacency changes, newest first.
///
/// Carries the limit clamp and the cursor rule, both of which a second surface would otherwise have
/// to reproduce: a cursor is returned **only when the page came back full**, because handing one
/// back on a short page makes a client fetch an empty page to discover the history ended.
pub(crate) async fn neighbor_history(
    admin: &super::AdminState,
    node_id: Uuid,
    before: Option<(chrono::DateTime<chrono::Utc>, i64)>,
    limit: Option<i64>,
) -> ApiResult<NeighborHistory> {
    let limit = limit
        .unwrap_or(HISTORY_DEFAULT_LIMIT)
        .clamp(1, HISTORY_MAX_LIMIT);
    let rows = admin
        .neighbors
        .list_changes(node_id, before, limit)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "list neighbor history",
                "failed to load neighbour history",
            )
        })?;
    // A cursor only when the page came back full — a short page is the end of the history, and
    // handing back a cursor there makes a client fetch an empty page to discover that.
    let next = rows
        .last()
        .filter(|_| i64::try_from(rows.len()).unwrap_or(0) == limit)
        .map(|r| NeighborHistoryCursor {
            at: r.at.to_rfc3339(),
            id: r.id,
        });
    Ok(NeighborHistory {
        changes: rows
            .into_iter()
            .map(|r| NeighborChange {
                id: r.id,
                at: r.at.to_rfc3339(),
                neighbors: r.set,
                prev_neighbor_key: r.prev_neighbor_key,
            })
            .collect(),
        next,
    })
}

// ── Deployment-wide settings ─────────────────────────────────────────────────

/// How this deployment discovers connectivity: CDP/LLDP neighbours and interface addresses.
//
// The `enabled` / `interval_secs` field names describe the neighbour walk and are kept verbatim:
// renaming them would break every existing client for no gain. The L3 pair is added alongside as
// `Option`, so a client that predates it can still `PUT` this body — absent means "leave that
// setting as it is" rather than "turn it off", which is the difference between an additive field
// and a silent regression.
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub(crate) struct NeighborConfig {
    /// Whether CDP/LLDP neighbour walks are issued at all.
    pub enabled: bool,
    /// How often each SNMP node's neighbour tables are walked, in seconds.
    pub interval_secs: u32,
    /// Whether interface-address walks are issued at all. Omitted on update leaves it unchanged.
    #[serde(default)]
    pub l3_enabled: Option<bool>,
    /// How often each SNMP node's interface-address tables are walked, in seconds. Omitted on
    /// update leaves it unchanged.
    #[serde(default)]
    pub l3_interval_secs: Option<u32>,
    /// Whether ARP / IPv6-neighbour walks are issued at all. Omitted on update leaves it unchanged.
    ///
    /// Off unless an operator turns it on: this walk reads a table sized by the network rather than
    /// by the device, and it is the only discovery walk here that costs a busy switch measurable
    /// work. What it buys is the only answer nothing else can give — which hosts are on your
    /// segments that Yagra is not monitoring.
    #[serde(default)]
    pub arp_enabled: Option<bool>,
    /// How often each SNMP node's ARP / IPv6-neighbour tables are walked, in seconds. Omitted on
    /// update leaves it unchanged.
    #[serde(default)]
    pub arp_interval_secs: Option<u32>,
    /// Whether routing-adjacency collection is issued at all. Omitted on update leaves it unchanged.
    ///
    /// On unless an operator turns it off. This is what finds the links that share no subnet — a
    /// point-to-point `/32`, an unnumbered OSPF link, an eBGP session across a segment — which the
    /// shared-subnet rule structurally cannot see. The tables it reads are sized by the device's own
    /// peering mesh, and the routing table itself is never walked: it is probed one destination at a
    /// time.
    #[serde(default)]
    pub routing_enabled: Option<bool>,
    /// How often each SNMP node's routing adjacency is collected, in seconds. Omitted on update
    /// leaves it unchanged.
    #[serde(default)]
    pub routing_interval_secs: Option<u32>,
    /// Smallest cadence this deployment accepts, in seconds.
    #[serde(default)]
    pub min_interval_secs: u32,
    /// Largest cadence this deployment accepts, in seconds.
    #[serde(default)]
    pub max_interval_secs: u32,
}

/// The deployment's adjacency-collection settings, with the accepted cadence range.
#[utoipa::path(
    get, path = "/api/v1/settings/neighbors", tag = "settings",
    responses(
        (status = 200, description = "Whether adjacency is collected, how often, and the accepted range", body = NeighborConfig),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn get_adjacency_settings(
    _guard: RequireView,
    admin: Admin,
) -> ApiResult<Json<NeighborConfig>> {
    Ok(Json(adjacency_config(&admin).await))
}

/// The adjacency-collection settings with the accepted cadence range — the seam both edges call.
///
/// The range travels with the values on purpose: a caller that knows the current interval but not
/// the bounds cannot tell a rejected write from a broken one.
pub(crate) async fn adjacency_config(admin: &super::AdminState) -> NeighborConfig {
    let s = admin.repo.get_adjacency_settings().await;
    NeighborConfig {
        enabled: s.neighbors_enabled,
        interval_secs: s.neighbors_interval_secs,
        l3_enabled: Some(s.l3_enabled),
        l3_interval_secs: Some(s.l3_interval_secs),
        arp_enabled: Some(s.arp_enabled),
        arp_interval_secs: Some(s.arp_interval_secs),
        routing_enabled: Some(s.routing_enabled),
        routing_interval_secs: Some(s.routing_interval_secs),
        min_interval_secs: neighbors::MIN_NEIGHBOR_INTERVAL_SECS,
        max_interval_secs: neighbors::MAX_NEIGHBOR_INTERVAL_SECS,
    }
}

/// Change whether and how often adjacency is collected.
///
/// Applies from the next scheduler sweep. Turning collection off stops issuing walks but keeps
/// everything already recorded — the current set and the change history are unaffected.
#[utoipa::path(
    put, path = "/api/v1/settings/neighbors", tag = "settings",
    request_body = NeighborConfig,
    responses(
        (status = 204, description = "Settings updated; the change applies from the next scheduler sweep"),
        (status = 400, description = "The cadence is outside the allowed range", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn update_neighbor_settings(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<NeighborConfig>,
) -> ApiResult<StatusCode> {
    // The bounds in the request body are informational (they let one shape serve both directions);
    // what the server accepts is decided here, never by what the client sent back.
    //
    // The L3 pair falls back to what is already stored, so a client that predates those fields
    // updates the neighbour settings without silently switching L3 discovery off.
    let current = admin.repo.get_adjacency_settings().await;
    let next = AdjacencySettings {
        neighbors_enabled: body.enabled,
        neighbors_interval_secs: body.interval_secs,
        l3_enabled: body.l3_enabled.unwrap_or(current.l3_enabled),
        l3_interval_secs: body.l3_interval_secs.unwrap_or(current.l3_interval_secs),
        arp_enabled: body.arp_enabled.unwrap_or(current.arp_enabled),
        arp_interval_secs: body.arp_interval_secs.unwrap_or(current.arp_interval_secs),
        routing_enabled: body.routing_enabled.unwrap_or(current.routing_enabled),
        routing_interval_secs: body
            .routing_interval_secs
            .unwrap_or(current.routing_interval_secs),
    };
    if !next.in_bounds() {
        return Err(ApiError::bad_request(
            "invalid_neighbor_interval",
            format!(
                "the neighbour and interface-address intervals must be between {} and {} seconds",
                neighbors::MIN_NEIGHBOR_INTERVAL_SECS,
                neighbors::MAX_NEIGHBOR_INTERVAL_SECS,
            ),
        ));
    }
    admin
        .repo
        .set_adjacency_settings(&next)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "set neighbor settings",
                "failed to update neighbour settings",
            )
        })?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn status(state: ApiState, method: &str, path: &str, body: &str) -> StatusCode {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_owned()))
            .expect("request builds");
        super::super::router(state)
            .oneshot(req)
            .await
            .expect("router responds")
            .status()
    }

    /// Every route authenticates before it reports anything about this deployment — including
    /// whether it has inventory storage at all. 401, never 503 (api-conventions).
    #[tokio::test]
    async fn anonymous_is_refused_before_the_subsystem_is_consulted() {
        for (method, path, body) in [
            (
                "GET",
                "/api/v1/nodes/00000000-0000-0000-0000-000000000000/neighbors",
                "",
            ),
            (
                "GET",
                "/api/v1/nodes/00000000-0000-0000-0000-000000000000/neighbors/history",
                "",
            ),
            ("GET", "/api/v1/settings/neighbors", ""),
            (
                "PUT",
                "/api/v1/settings/neighbors",
                r#"{"enabled":true,"interval_secs":3600}"#,
            ),
        ] {
            assert_eq!(
                status(private_state(), method, path, body).await,
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }
    }

    /// A public-dashboard deployment opens reads, not writes. Adjacency is a read, so the two node
    /// endpoints answer; changing collection stays closed.
    #[tokio::test]
    async fn public_dashboard_opens_the_reads_but_not_the_write() {
        assert_ne!(
            status(public_state(), "GET", "/api/v1/settings/neighbors", "").await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            status(
                public_state(),
                "PUT",
                "/api/v1/settings/neighbors",
                r#"{"enabled":false,"interval_secs":3600}"#
            )
            .await,
            StatusCode::UNAUTHORIZED,
            "a write guard must stay closed on a public deployment"
        );
    }

    /// The cadence band is enforced at the edge, not left to the table CHECK — the CHECK would
    /// surface as a 500 with no usable message.
    #[test]
    fn the_cadence_band_is_the_one_the_module_declares() {
        for bad in [0, 1, neighbors::MIN_NEIGHBOR_INTERVAL_SECS - 1, 999_999] {
            assert!(
                !AdjacencySettings {
                    neighbors_interval_secs: bad,
                    ..AdjacencySettings::default()
                }
                .in_bounds(),
                "{bad} should be rejected as a neighbour cadence"
            );
            // Every cadence is bounded. Checking only the first would let the others through.
            assert!(
                !AdjacencySettings {
                    l3_interval_secs: bad,
                    ..AdjacencySettings::default()
                }
                .in_bounds(),
                "{bad} should be rejected as an interface-address cadence"
            );
            assert!(
                !AdjacencySettings {
                    arp_interval_secs: bad,
                    ..AdjacencySettings::default()
                }
                .in_bounds(),
                "{bad} should be rejected as an ARP cadence"
            );
        }
        for ok in [
            neighbors::MIN_NEIGHBOR_INTERVAL_SECS,
            3600,
            neighbors::MAX_NEIGHBOR_INTERVAL_SECS,
        ] {
            assert!(AdjacencySettings {
                neighbors_interval_secs: ok,
                l3_interval_secs: ok,
                arp_interval_secs: ok,
                ..AdjacencySettings::default()
            }
            .in_bounds());
        }
    }

    /// A client that predates the ARP fields must not switch ARP discovery off — nor, more subtly,
    /// switch it *on*: the body's `None` means "leave it", and both directions are silent failures
    /// if that is got wrong.
    #[test]
    fn an_absent_field_leaves_the_stored_setting_alone() {
        let body: NeighborConfig =
            serde_json::from_str(r#"{"enabled":true,"interval_secs":3600}"#).unwrap();
        assert_eq!(body.arp_enabled, None);
        assert_eq!(body.arp_interval_secs, None);
        assert_eq!(body.l3_enabled, None);

        let current = AdjacencySettings {
            arp_enabled: true,
            arp_interval_secs: 7200,
            ..AdjacencySettings::default()
        };
        assert!(body.arp_enabled.unwrap_or(current.arp_enabled));
        assert_eq!(
            body.arp_interval_secs.unwrap_or(current.arp_interval_secs),
            7200
        );
    }

    /// The response advertises the range the server actually enforces, so a UI cannot render a
    /// control whose values the server will refuse.
    #[test]
    fn the_advertised_range_is_the_enforced_range() {
        let cfg = NeighborConfig {
            enabled: true,
            interval_secs: 3600,
            l3_enabled: Some(true),
            l3_interval_secs: Some(3600),
            arp_enabled: Some(false),
            arp_interval_secs: Some(21_600),
            routing_enabled: Some(true),
            routing_interval_secs: Some(3600),
            min_interval_secs: neighbors::MIN_NEIGHBOR_INTERVAL_SECS,
            max_interval_secs: neighbors::MAX_NEIGHBOR_INTERVAL_SECS,
        };
        assert!(neighbors::interval_in_bounds(cfg.min_interval_secs));
        assert!(neighbors::interval_in_bounds(cfg.max_interval_secs));
        assert!(!neighbors::interval_in_bounds(cfg.min_interval_secs - 1));
        assert!(!neighbors::interval_in_bounds(cfg.max_interval_secs + 1));
    }
}
