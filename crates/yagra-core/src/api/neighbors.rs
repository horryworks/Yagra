// SPDX-License-Identifier: AGPL-3.0-only
//! CDP/LLDP adjacency: what a node currently sees on its ports, how that changed over time, and
//! whether this deployment collects it at all (ADR-038).
//!
//! The two reads are per node and node-scoped; the settings pair is deployment-wide and admin-only.
//! They are separate endpoints rather than two more fields on `/api/v1/settings/retention` because
//! they answer a different question and carry a different justification — retention is "how long is
//! anything kept", this is "is a walk being issued, and how often".
//!
//! Nothing here writes to a device, and no response carries a credential: adjacency is descriptive
//! device data (a chassis id, a port name, a peer's sysDescr), untrusted and rendered as such.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig, RequireView, VisibleNode};
use super::ApiState;
use crate::neighbors::{self, NeighborSettings};
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
    get_neighbor_settings,
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
            get(get_neighbor_settings).put(update_neighbor_settings),
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
    Ok(Json(CurrentNeighbors {
        neighbors: current.set,
        first_seen: current.first_seen.to_rfc3339(),
        last_seen: current.last_seen.to_rfc3339(),
    }))
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
    let limit = q
        .limit
        .unwrap_or(HISTORY_DEFAULT_LIMIT)
        .clamp(1, HISTORY_MAX_LIMIT);
    let before = q.cursor()?;
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
    Ok(Json(NeighborHistory {
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
    }))
}

// ── Deployment-wide settings ─────────────────────────────────────────────────

/// How this deployment collects adjacency.
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub(crate) struct NeighborConfig {
    /// Whether neighbour walks are issued at all.
    pub enabled: bool,
    /// How often each SNMP node's neighbour tables are walked, in seconds.
    pub interval_secs: u32,
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
async fn get_neighbor_settings(
    _guard: RequireView,
    admin: Admin,
) -> ApiResult<Json<NeighborConfig>> {
    let s = admin.repo.get_neighbor_settings().await;
    Ok(Json(NeighborConfig {
        enabled: s.enabled,
        interval_secs: s.interval_secs,
        min_interval_secs: neighbors::MIN_NEIGHBOR_INTERVAL_SECS,
        max_interval_secs: neighbors::MAX_NEIGHBOR_INTERVAL_SECS,
    }))
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
    let next = NeighborSettings {
        enabled: body.enabled,
        interval_secs: body.interval_secs,
    };
    if !next.in_bounds() {
        return Err(ApiError::bad_request(
            "invalid_neighbor_interval",
            format!(
                "the neighbour interval must be between {} and {} seconds",
                neighbors::MIN_NEIGHBOR_INTERVAL_SECS,
                neighbors::MAX_NEIGHBOR_INTERVAL_SECS,
            ),
        ));
    }
    admin.repo.set_neighbor_settings(&next).await.map_err(|e| {
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
                !NeighborSettings {
                    enabled: true,
                    interval_secs: bad
                }
                .in_bounds(),
                "{bad} should be rejected"
            );
        }
        for ok in [
            neighbors::MIN_NEIGHBOR_INTERVAL_SECS,
            3600,
            neighbors::MAX_NEIGHBOR_INTERVAL_SECS,
        ] {
            assert!(NeighborSettings {
                enabled: true,
                interval_secs: ok
            }
            .in_bounds());
        }
    }

    /// The response advertises the range the server actually enforces, so a UI cannot render a
    /// control whose values the server will refuse.
    #[test]
    fn the_advertised_range_is_the_enforced_range() {
        let cfg = NeighborConfig {
            enabled: true,
            interval_secs: 3600,
            min_interval_secs: neighbors::MIN_NEIGHBOR_INTERVAL_SECS,
            max_interval_secs: neighbors::MAX_NEIGHBOR_INTERVAL_SECS,
        };
        assert!(neighbors::interval_in_bounds(cfg.min_interval_secs));
        assert!(neighbors::interval_in_bounds(cfg.max_interval_secs));
        assert!(!neighbors::interval_in_bounds(cfg.min_interval_secs - 1));
        assert!(!neighbors::interval_in_bounds(cfg.max_interval_secs + 1));
    }
}
