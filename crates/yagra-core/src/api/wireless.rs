// SPDX-License-Identifier: AGPL-3.0-only
//! Wireless controllers and the access points they report (ADR-064).
//!
//! One read today: the AP list, across every controller or narrowed to one. It is built from the
//! inventories controllers publish on each poll, so an AP appears here as soon as a controller's AP
//! walk has run — before, and whether or not, anyone imports it as a node.
//!
//! **Scoping is by the reporting controller, in the SQL.** An AP has no node of its own until it is
//! imported, so what bounds it is the controllers that report it: an AP is listed when a controller
//! the caller can see reports it (`WirelessRepo::list_page`). The REST handler and the MCP tool call
//! the same [`wireless_ap_page`], so neither can forget the predicate.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireView, Scoped};
use super::ApiState;
use crate::wireless::{ApFilter, WirelessRepo};
use axum::{extract::Query, routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yagra_common::WlanApState;

/// Default page size for the AP list.
const AP_DEFAULT_LIMIT: i64 = 200;
/// Hard cap on the page size (api-conventions): the most APs one controller may report, so a whole
/// controller fits one page.
const AP_MAX_LIMIT: i64 = 2048;
/// The longest search string accepted, in characters.
const AP_SEARCH_MAX_CHARS: usize = 64;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(list_wireless_aps))]
pub(super) struct Doc;

/// The wireless routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new().route("/api/v1/wireless/aps", get(list_wireless_aps))
}

/// One access point.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct WirelessApRow {
    /// Stable id, derived from the MAC address. The same AP keeps it when the controller serving it
    /// changes.
    pub ap_id: Uuid,
    /// MAC address, lower-case and colon-separated.
    pub mac: String,
    /// The AP's name on its controller.
    pub name: Option<String>,
    pub serial: Option<String>,
    /// Model, as the controller spells it.
    pub model: Option<String>,
    /// Software version.
    pub sw_version: Option<String>,
    /// Management address. `null` when the controller reports none, which it does for an AP that is
    /// down.
    pub ip: Option<String>,
    /// The vendor's own grouping of APs (a Huawei AP group).
    pub vendor_group: Option<String>,
    /// What the serving controller says: `associated` (in service), `backup` (a standby controller's
    /// view) or `not_associated` (down, not yet joined, or failing). `null` for a state this server
    /// version does not recognise.
    pub state: Option<WlanApState>,
    /// The controller's own word for the state (`normal`, `fault`, `standby`).
    pub run_state: String,
    /// Wireless clients online through this AP, as the serving controller reports it.
    pub clients: Option<i32>,
    /// The AP's node, once it has been imported as one.
    pub node_id: Option<Uuid>,
    /// The node of the controller serving this AP: the last one to report it in service.
    pub controller_node_id: Option<Uuid>,
    /// When a controller first reported this AP (RFC 3339).
    pub first_seen: String,
    /// When a controller last reported this AP (RFC 3339). It stops advancing when no controller
    /// reports the AP any more; the AP is never removed.
    pub last_seen: String,
    /// When a controller last reported this AP in service (RFC 3339). `null` if it never has been.
    pub last_associated_at: Option<String>,
    /// What each controller that reports this AP says, the serving controller first. Two entries
    /// for an AP behind an HA pair.
    pub reported_by: Vec<WirelessApSighting>,
}

/// One controller's view of one access point.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct WirelessApSighting {
    /// The reporting controller's node.
    pub controller_node_id: Option<Uuid>,
    pub state: Option<WlanApState>,
    pub run_state: String,
    pub clients: Option<i32>,
    /// When this controller last reported the AP (RFC 3339).
    pub last_seen: String,
    /// When this controller last reported the AP in service (RFC 3339).
    pub last_associated_at: Option<String>,
}

/// Keyset cursor for the next page.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct WirelessApCursor {
    /// Pass back as `after_key`.
    pub key: String,
    /// Pass back as `after_id`.
    pub ap_id: Uuid,
}

/// What a controller's last complete AP inventory said about the controller itself.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct WirelessControllerSummary {
    /// The controller's node.
    pub node_id: Uuid,
    /// The vendor dialect its AP table was read in.
    pub flavor: Option<yagra_common::WlanFlavor>,
    /// How many APs its last inventory carried.
    pub aps_reported: i32,
    /// Set when the controller reported more APs than one inventory may carry: how many it
    /// reported. The list then holds only the first ones by MAC address.
    pub aps_truncated_at: Option<i32>,
    /// When its last complete inventory arrived (RFC 3339). `null` if none has.
    pub last_inventory_at: Option<String>,
}

/// One page of access points, ordered by name.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct WirelessApPage {
    pub aps: Vec<WirelessApRow>,
    /// `null` ⇒ this was the last page.
    pub next: Option<WirelessApCursor>,
    /// The controller named by `controller_node_id`, when one was named and it has reported an
    /// inventory. `null` otherwise.
    pub controller: Option<WirelessControllerSummary>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct WirelessApsQuery {
    /// Only APs this controller node reports.
    #[serde(default)]
    controller_node_id: Option<Uuid>,
    /// Only APs in this state: `associated`, `backup` or `not_associated`.
    #[serde(default)]
    state: Option<String>,
    /// A case-insensitive substring of the name, MAC address, IP address or model (at most 64
    /// characters). Matched literally.
    #[serde(default)]
    search: Option<String>,
    /// Page size, 1–2048 (default 200).
    #[serde(default)]
    limit: Option<i64>,
    /// Keyset cursor: `next.key` from the previous page. Pair with `after_id`.
    #[serde(default)]
    after_key: Option<String>,
    /// Keyset cursor: `next.ap_id` from the previous page. Pair with `after_key`.
    #[serde(default)]
    after_id: Option<Uuid>,
}

/// Access points reported by the wireless controllers Yagra monitors.
///
/// Built from each controller's AP table on every poll, so an AP is listed whether or not it has
/// been imported as a node. An AP behind an HA pair appears once: `state` and `clients` are what the
/// controller serving it says, and `reported_by` shows every controller's view. An AP that no
/// controller reports any more stays in the list with its `last_seen` ageing.
#[utoipa::path(
    get, path = "/api/v1/wireless/aps", tag = "wireless",
    params(WirelessApsQuery),
    responses(
        (status = 200, description = "One page of access points, ordered by name", body = WirelessApPage),
        (status = 400, description = "Unknown state, search longer than 64 characters, or only one half of the cursor", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_wireless_aps(
    _guard: RequireView,
    Scoped(scope): Scoped,
    admin: Admin,
    Query(q): Query<WirelessApsQuery>,
) -> ApiResult<Json<WirelessApPage>> {
    let request = WirelessApRequest::parse(
        q.controller_node_id,
        q.state.as_deref(),
        q.search.as_deref(),
        q.after_key,
        q.after_id,
        q.limit,
    )?;
    Ok(Json(wireless_ap_page(&admin, &scope, request).await?))
}

/// A validated AP list request — what both surfaces hand [`wireless_ap_page`].
#[derive(Debug)]
pub(crate) struct WirelessApRequest {
    filter: ApFilter,
    after: Option<(String, Uuid)>,
    limit: i64,
}

impl WirelessApRequest {
    /// Validate the raw parameters. An unknown state or an over-long search is refused rather than
    /// ignored: widening a filter the caller asked for answers a different question.
    pub(crate) fn parse(
        controller_node: Option<Uuid>,
        state: Option<&str>,
        search: Option<&str>,
        after_key: Option<String>,
        after_id: Option<Uuid>,
        limit: Option<i64>,
    ) -> Result<Self, ApiError> {
        let state = match state.map(str::trim).filter(|s| !s.is_empty()) {
            None => None,
            Some(token) => Some(WlanApState::from_token(token).ok_or_else(|| {
                ApiError::bad_request(
                    "invalid_state",
                    "state must be associated, backup or not_associated",
                )
            })?),
        };
        let search = search.map(str::trim).filter(|s| !s.is_empty());
        if search.is_some_and(|s| s.chars().count() > AP_SEARCH_MAX_CHARS) {
            return Err(ApiError::bad_request(
                "invalid_search",
                "search must be at most 64 characters",
            ));
        }
        let after = match (after_key, after_id) {
            (Some(key), Some(id)) => Some((key, id)),
            (None, None) => None,
            _ => {
                return Err(ApiError::bad_request(
                    "invalid_cursor",
                    "after_key and after_id must be given together",
                ))
            }
        };
        Ok(Self {
            filter: ApFilter {
                controller_node,
                state,
                search: search.map(str::to_owned),
            },
            after,
            limit: limit.unwrap_or(AP_DEFAULT_LIMIT).clamp(1, AP_MAX_LIMIT),
        })
    }
}

/// One page of APs — the seam REST and MCP share (api-conventions).
pub(crate) async fn wireless_ap_page(
    admin: &super::AdminState,
    scope: &super::scope::NodeScope,
    request: WirelessApRequest,
) -> ApiResult<WirelessApPage> {
    let WirelessApRequest {
        filter,
        after,
        limit,
    } = request;
    let filter_controller = filter.controller_node;
    let rows = admin
        .wireless
        .list_page(scope.group_filter(), &filter, after, limit)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "list wireless aps",
                "failed to list access points",
            )
        })?;
    // A cursor only when the page came back full — a short page is the end of the list.
    let next = rows
        .last()
        .filter(|_| i64::try_from(rows.len()).unwrap_or(0) == limit)
        .map(|r| WirelessApCursor {
            key: WirelessRepo::sort_key(r),
            ap_id: r.ap_id,
        });
    let rfc = |t: chrono::DateTime<chrono::Utc>| t.to_rfc3339();
    // Only for a controller the caller can see: the summary names a node, and a scoped caller must
    // not learn that a controller outside its folders exists by asking for it.
    let controller = match filter_controller {
        Some(node) if scope_allows_node(admin, scope, node).await? => admin
            .wireless
            .controller(node)
            .await
            .map_err(|e| {
                ApiError::from_internal(
                    e.as_ref(),
                    "read wireless controller",
                    "failed to read the controller",
                )
            })?
            .map(|c| WirelessControllerSummary {
                node_id: c.node_id,
                flavor: c.flavor,
                aps_reported: c.aps_reported,
                aps_truncated_at: c.aps_truncated_at,
                last_inventory_at: c.last_inventory_at.map(rfc),
            }),
        _ => None,
    };
    Ok(WirelessApPage {
        controller,
        aps: rows
            .into_iter()
            .map(|r| WirelessApRow {
                ap_id: r.ap_id,
                mac: r.mac,
                name: r.name,
                serial: r.serial,
                model: r.model,
                sw_version: r.sw_version,
                ip: r.ip.map(|ip| ip.to_string()),
                vendor_group: r.vendor_group,
                state: r.state,
                run_state: r.run_state,
                clients: r.clients,
                node_id: r.node_id,
                controller_node_id: r.owner_node_id,
                first_seen: rfc(r.first_seen),
                last_seen: rfc(r.last_seen),
                last_associated_at: r.last_associated_at.map(rfc),
                reported_by: r
                    .sightings
                    .into_iter()
                    .map(|s| WirelessApSighting {
                        controller_node_id: s.controller_node_id,
                        state: s.state,
                        run_state: s.run_state,
                        clients: s.clients,
                        last_seen: rfc(s.last_seen),
                        last_associated_at: s.last_associated_at.map(rfc),
                    })
                    .collect(),
            })
            .collect(),
        next,
    })
}

/// Whether `node` is a node the caller may see.
async fn scope_allows_node(
    admin: &super::AdminState,
    scope: &super::scope::NodeScope,
    node: Uuid,
) -> ApiResult<bool> {
    if scope.group_filter().is_none() {
        return Ok(true);
    }
    let found = admin.repo.get_node(node).await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "read node", "failed to read the controller")
    })?;
    Ok(found.is_some_and(|n| scope.allows_group(n.group.map(|g| g.as_uuid()))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests_support::{live_state, private_state, scoped_token, send, token};
    use axum::http::StatusCode;
    use yagra_common::{ApMac, Role, WlanApObservation, WlanFlavor, WlanInventory};

    #[test]
    fn a_request_refuses_what_it_cannot_honour() {
        assert!(WirelessApRequest::parse(None, Some("up"), None, None, None, None).is_err());
        let long = "x".repeat(65);
        assert!(WirelessApRequest::parse(None, None, Some(&long), None, None, None).is_err());
        assert!(
            WirelessApRequest::parse(None, None, None, Some("k".into()), None, None).is_err(),
            "half a cursor would restart paging from the top forever"
        );
        let ok = WirelessApRequest::parse(None, Some(" backup "), Some("  "), None, None, Some(0))
            .expect("valid");
        assert_eq!(ok.filter.state, Some(WlanApState::Backup));
        assert_eq!(ok.filter.search, None, "a blank search is no search");
        assert_eq!(ok.limit, 1, "the limit is clamped, not refused");
        let big = WirelessApRequest::parse(None, None, None, None, None, Some(1_000_000)).unwrap();
        assert_eq!(big.limit, AP_MAX_LIMIT);
    }

    #[tokio::test]
    async fn the_ap_list_authenticates_before_reporting_anything_about_the_deployment() {
        let st = private_state();
        let (status, _) = send(&st, "GET", "/api/v1/wireless/aps", "not-a-token", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let viewer = token(&st, Role::Viewer);
        let (status, _) = send(&st, "GET", "/api/v1/wireless/aps", &viewer, None).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "skeleton mode, after the guard"
        );
    }

    /// End to end over a real database: a viewer reads a controller's APs, and a scoped caller reads
    /// only the APs a controller in its folders reports.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_viewer_lists_a_controllers_aps_and_a_scoped_caller_only_its_own(pool: sqlx::PgPool) {
        let mine = crate::pgtest::group(&pool, "mine").await;
        let theirs = crate::pgtest::group(&pool, "theirs").await;
        let ours = crate::pgtest::node(&pool, "wac-ours", 1, Some(mine)).await;
        let alien = crate::pgtest::node(&pool, "wac-alien", 2, Some(theirs)).await;
        let repo = WirelessRepo::new(pool.clone());
        let obs = |mac: u8, name: &str| WlanApObservation {
            mac: ApMac::new([0, 0, 0, 0, 0, mac]),
            name: Some(name.into()),
            serial: None,
            model: Some("AirEngine5776-26".into()),
            sw_version: None,
            ip: Some("10.0.0.1".parse().unwrap()),
            vendor_group: None,
            run_state: "normal".into(),
            state: WlanApState::Associated,
            clients: Some(4),
            cpu_pct: None,
            mem_pct: None,
            temp_c: None,
        };
        let now = chrono::Utc::now();
        repo.record_inventory(
            ours,
            &WlanInventory::bounded(
                WlanFlavor::Huawei,
                vec![obs(1, "ap-1"), obs(2, "ap-2")],
                1024,
            ),
            now,
        )
        .await
        .unwrap();
        repo.record_inventory(
            alien,
            &WlanInventory::bounded(WlanFlavor::Huawei, vec![obs(3, "ap-3")], 1024),
            now,
        )
        .await
        .unwrap();

        let st = live_state(pool).await;
        let viewer = token(&st, Role::Viewer);
        let (status, body) = send(&st, "GET", "/api/v1/wireless/aps", &viewer, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["aps"].as_array().map(Vec::len), Some(3));
        assert_eq!(body["aps"][0]["name"], "ap-1");
        assert_eq!(body["aps"][0]["state"], "associated");
        assert_eq!(body["aps"][0]["clients"], 4);
        assert_eq!(body["aps"][0]["controller_node_id"], ours.to_string());
        assert_eq!(
            body["aps"][0]["reported_by"].as_array().map(Vec::len),
            Some(1)
        );
        assert!(body["next"].is_null());
        assert!(body["controller"].is_null(), "no controller was named");

        let uri = format!("/api/v1/wireless/aps?controller_node_id={alien}");
        let (_, body) = send(&st, "GET", &uri, &viewer, None).await;
        assert_eq!(body["aps"].as_array().map(Vec::len), Some(1));
        assert_eq!(body["controller"]["aps_reported"], 1);
        assert_eq!(body["controller"]["flavor"], "huawei");

        let scoped = scoped_token(&st, &[mine]);
        let (status, body) = send(&st, "GET", "/api/v1/wireless/aps", &scoped, None).await;
        assert_eq!(status, StatusCode::OK);
        let names: Vec<&str> = body["aps"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|a| a["name"].as_str())
            .collect();
        assert_eq!(
            names,
            ["ap-1", "ap-2"],
            "an AP reported only outside the scope was listed"
        );
        // Naming a controller outside the scope returns neither its APs nor its summary.
        let uri = format!("/api/v1/wireless/aps?controller_node_id={alien}");
        let (_, body) = send(&st, "GET", &uri, &scoped, None).await;
        assert_eq!(body["aps"].as_array().map(Vec::len), Some(0));
        assert!(
            body["controller"].is_null(),
            "a controller outside the scope was described"
        );

        let (status, _) = send(&st, "GET", "/api/v1/wireless/aps?state=up", &viewer, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (_, page) = send(&st, "GET", "/api/v1/wireless/aps?limit=1", &viewer, None).await;
        let key = page["next"]["key"]
            .as_str()
            .expect("a full page has a cursor")
            .to_owned();
        let id = page["next"]["ap_id"].as_str().unwrap().to_owned();
        let uri = format!("/api/v1/wireless/aps?limit=1&after_key={key}&after_id={id}");
        let (_, next) = send(&st, "GET", &uri, &viewer, None).await;
        assert_eq!(next["aps"][0]["name"], "ap-2");
    }
}
