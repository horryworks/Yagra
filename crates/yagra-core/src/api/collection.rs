// SPDX-License-Identifier: AGPL-3.0-only
//! What Yagra actually collects from a device: per-node collection items, the reusable templates a
//! profile attaches, and the interface inventory those items populate.
//!
//! Every write is `ManageConfig`, and so is every *read* except the interface list — a collection
//! set is monitoring configuration, not fleet status. The interface list is the exception because
//! it is device state an operator looks at, not a setting.
//!
//! **Every item carries an OID and a metric name that leave this process.** The metric name is
//! interpolated into TSDB queries and the OID is handed to SNMP, so both are validated at the edge
//! by [`validate_item`] before anything is stored — once a row exists, nothing revalidates it.
//!
//! Utilization is **derived here at read time, never stored** (ADR-012): pollers write raw counters
//! and rates come from the TSDB's `rate()`, which handles counter wrap and resets. Interface rows
//! join the stored metadata with those query-time values.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig, RequireView};
use super::util::{now_unix_s, CreatedId, DEFAULT_RATE_LOOKBACK_SECS};
use super::{is_valid_metric_name, is_valid_oid, ApiState};
use crate::collection::CreateTemplateOutcome;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{delete, get},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
use yagra_common::resolve_collection_set;

/// An interface whose metadata has not been refreshed within this window is flagged stale. The UI
/// shows the row either way — a switch that stopped answering SNMP still has ports.
const INTERFACE_STALE_SECS: i64 = 900;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_node_collection,
    create_node_collection,
    delete_collection_item,
    list_node_interfaces,
    list_collection_templates,
    create_collection_template,
    delete_collection_template,
    list_template_items,
    create_template_item,
    delete_template_item,
    list_profile_templates,
    set_profile_templates
))]
pub(super) struct Doc;

/// The collection routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/nodes/:node_id/collection",
            get(list_node_collection).post(create_node_collection),
        )
        .route(
            "/api/v1/collection/:item_id",
            delete(delete_collection_item),
        )
        .route(
            "/api/v1/nodes/:node_id/interfaces",
            get(list_node_interfaces),
        )
        .route(
            "/api/v1/collection-templates",
            get(list_collection_templates).post(create_collection_template),
        )
        .route(
            "/api/v1/collection-templates/:id",
            delete(delete_collection_template),
        )
        .route(
            "/api/v1/collection-templates/:id/items",
            get(list_template_items).post(create_template_item),
        )
        .route(
            "/api/v1/collection-templates/:id/items/:item_id",
            delete(delete_template_item),
        )
        .route(
            "/api/v1/profiles/:id/templates",
            get(list_profile_templates).put(set_profile_templates),
        )
}

/// Create body for a collection item — the same shape whether it lands on a node or a template.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateCollectionItem {
    metric_name: String,
    oid: String,
    collection: String,
    metric_kind: String,
    enabled: Option<bool>,
}

/// Reject an item body at the edge.
///
/// One function for both creators on purpose: the node and template paths write into the same
/// table and feed the same poller, so an item that is unacceptable on one must be unacceptable on
/// the other. Two copies would eventually disagree, and the looser one becomes the way in.
fn validate_item(body: &CreateCollectionItem) -> Result<(), ApiError> {
    if !is_valid_metric_name(&body.metric_name) {
        return Err(ApiError::bad_request(
            "invalid_metric_name",
            "metric_name must be a valid identifier",
        ));
    }
    if !is_valid_oid(&body.oid) {
        return Err(ApiError::bad_request(
            "invalid_oid",
            "oid must be a dotted numeric OID",
        ));
    }
    if !matches!(body.collection.as_str(), "scalar" | "table")
        || !matches!(body.metric_kind.as_str(), "gauge" | "counter")
    {
        return Err(ApiError::bad_request(
            "invalid_collection_item",
            "collection must be scalar|table and metric_kind gauge|counter",
        ));
    }
    Ok(())
}

/// `?resolved=true` returns the effective set rather than the node's own overrides.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct CollectionQuery {
    resolved: Option<bool>,
}

/// A node's collection items: its own overrides, or — with `?resolved=true` — the effective set
/// after the profile's defaults are overridden by them. The resolved view is what the poller sees.
#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/collection", tag = "collection",
    params(("node_id" = Uuid, Path, description = "Node id"), CollectionQuery),
    responses(
        (status = 200, description = "The node's own stored items, or — with `?resolved=true` — the effective set the poller sees, which carries no item ids", body = serde_json::Value),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such node (resolved view only)", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn list_node_collection(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(node_id): Path<Uuid>,
    Query(q): Query<CollectionQuery>,
) -> ApiResult<Json<Value>> {
    if !q.resolved.unwrap_or(false) {
        let list = admin
            .collection
            .list_items("node", node_id)
            .await
            .map_err(|e| {
                ApiError::from_internal(
                    e.as_ref(),
                    "list node collection",
                    "failed to list collection set",
                )
            })?;
        return Ok(Json(serde_json::to_value(list).unwrap_or(Value::Null)));
    }
    let node = admin
        .repo
        .get_node(node_id)
        .await
        .map_err(|e| ApiError::from_internal(e.as_ref(), "get node", "failed to load node"))?;
    let profile = node
        .ok_or_else(|| ApiError::not_found("node_not_found", format!("no node {node_id}")))?
        .profile
        .map(|p| p.0);
    let scoped = admin
        .collection
        .list_items_for_node(node_id, profile)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "resolve collection",
                "failed to resolve collection set",
            )
        })?;
    let resolved = resolve_collection_set(&scoped);
    Ok(Json(serde_json::to_value(resolved).unwrap_or(Value::Null)))
}

#[utoipa::path(
    post, path = "/api/v1/nodes/{node_id}/collection", tag = "collection",
    params(("node_id" = Uuid, Path, description = "Node id")),
    request_body = CreateCollectionItem,
    responses(
        (status = 201, description = "Item created", body = CreatedId),
        (status = 400, description = "The metric name is not an identifier, the OID is not dotted-numeric, or collection/metric_kind is out of vocabulary", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn create_node_collection(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(node_id): Path<Uuid>,
    Json(body): Json<CreateCollectionItem>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    validate_item(&body)?;
    let id = admin
        .collection
        .create_item(
            "node",
            node_id,
            &body.metric_name,
            &body.oid,
            &body.collection,
            &body.metric_kind,
            body.enabled.unwrap_or(true),
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "create collection item",
                "failed to create collection item",
            )
        })?;
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

#[utoipa::path(
    delete, path = "/api/v1/collection/{item_id}", tag = "collection",
    params(("item_id" = Uuid, Path, description = "Collection item id")),
    responses(
        (status = 204, description = "Item deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such collection item", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn delete_collection_item(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(item_id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    match admin.collection.delete_item(item_id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "collection_item_not_found",
            format!("no collection item {item_id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "delete collection item",
            "failed to delete collection item",
        )),
    }
}

/// Create-template body.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateTemplate {
    name: String,
    description: Option<String>,
}

/// Replace-all body for a profile's attached templates.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct SetProfileTemplates {
    template_ids: Vec<Uuid>,
}

#[utoipa::path(
    get, path = "/api/v1/collection-templates", tag = "collection",
    responses(
        (status = 200, description = "Every collection template with its item count", body = Vec<crate::collection::TemplateSummary>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn list_collection_templates(
    _guard: RequireManageConfig,
    admin: Admin,
) -> ApiResult<Json<Vec<crate::collection::TemplateSummary>>> {
    let list = admin.collection.list_templates().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list templates",
            "failed to list collection templates",
        )
    })?;
    Ok(Json(list))
}

#[utoipa::path(
    post, path = "/api/v1/collection-templates", tag = "collection",
    request_body = CreateTemplate,
    responses(
        (status = 201, description = "Template created", body = CreatedId),
        (status = 400, description = "The template name is blank", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 409, description = "A template with that name already exists", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn create_collection_template(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<CreateTemplate>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_name",
            "template name must not be empty",
        ));
    }
    match admin
        .collection
        .create_template(name, body.description.as_deref())
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "create template",
                "failed to create collection template",
            )
        })? {
        CreateTemplateOutcome::Created(id) => Ok((StatusCode::CREATED, Json(CreatedId { id }))),
        CreateTemplateOutcome::NameTaken => Err(ApiError::conflict(
            "template_name_taken",
            "a template with that name already exists",
        )),
    }
}

#[utoipa::path(
    delete, path = "/api/v1/collection-templates/{id}", tag = "collection",
    params(("id" = Uuid, Path, description = "Collection template id")),
    responses(
        (status = 204, description = "Template deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such template", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn delete_collection_template(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    match admin.collection.delete_template(id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "template_not_found",
            format!("no template {id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "delete template",
            "failed to delete collection template",
        )),
    }
}

#[utoipa::path(
    get, path = "/api/v1/collection-templates/{id}/items", tag = "collection",
    params(("id" = Uuid, Path, description = "Collection template id")),
    responses(
        (status = 200, description = "The template's items", body = Vec<crate::collection::TemplateItem>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn list_template_items(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<crate::collection::TemplateItem>>> {
    let list = admin
        .collection
        .list_template_items(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "list template items",
                "failed to list template items",
            )
        })?;
    Ok(Json(list))
}

#[utoipa::path(
    post, path = "/api/v1/collection-templates/{id}/items", tag = "collection",
    params(("id" = Uuid, Path, description = "Collection template id")),
    request_body = CreateCollectionItem,
    responses(
        (status = 201, description = "Item created", body = CreatedId),
        (status = 400, description = "The metric name is not an identifier, the OID is not dotted-numeric, or collection/metric_kind is out of vocabulary", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn create_template_item(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateCollectionItem>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    validate_item(&body)?;
    let item_id = admin
        .collection
        .create_template_item(
            id,
            &body.metric_name,
            &body.oid,
            &body.collection,
            &body.metric_kind,
            body.enabled.unwrap_or(true),
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "create template item",
                "failed to create template item",
            )
        })?;
    Ok((StatusCode::CREATED, Json(CreatedId { id: item_id })))
}

#[utoipa::path(
    delete, path = "/api/v1/collection-templates/{id}/items/{item_id}", tag = "collection",
    params(
        ("id" = Uuid, Path, description = "Collection template id"),
        ("item_id" = Uuid, Path, description = "Item id within that template"),
    ),
    responses(
        (status = 204, description = "Item deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such item in that template", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn delete_template_item(
    _guard: RequireManageConfig,
    admin: Admin,
    Path((id, item_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    match admin.collection.delete_template_item(id, item_id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "template_item_not_found",
            format!("no item {item_id} in template {id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "delete template item",
            "failed to delete template item",
        )),
    }
}

#[utoipa::path(
    get, path = "/api/v1/profiles/{id}/templates", tag = "collection",
    params(("id" = Uuid, Path, description = "Device profile id")),
    responses(
        (status = 200, description = "The templates attached to this profile", body = Vec<crate::collection::TemplateSummary>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn list_profile_templates(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<crate::collection::TemplateSummary>>> {
    let list = admin
        .collection
        .list_profile_templates(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "list profile templates",
                "failed to list profile templates",
            )
        })?;
    Ok(Json(list))
}

#[utoipa::path(
    put, path = "/api/v1/profiles/{id}/templates", tag = "collection",
    params(("id" = Uuid, Path, description = "Device profile id")),
    request_body = SetProfileTemplates,
    responses(
        (status = 204, description = "The profile's attached templates now are exactly `template_ids`"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode: no write side", body = super::error::ErrorBody),
    ),
)]
async fn set_profile_templates(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<SetProfileTemplates>,
) -> ApiResult<StatusCode> {
    admin
        .collection
        .set_profile_templates(id, &body.template_ids)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "set profile templates",
                "failed to set profile templates",
            )
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// One interface row for the node-detail Interfaces tab: stored metadata joined with query-time
/// `rate()`/`latest()` metrics. Utilization is derived here and never stored (ADR-012).
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct InterfaceRow {
    ifindex: u32,
    if_name: Option<String>,
    if_alias: Option<String>,
    if_speed_bps: Option<i64>,
    oper_status: Option<f64>,
    in_bps: Option<f64>,
    out_bps: Option<f64>,
    in_util_pct: Option<f64>,
    out_util_pct: Option<f64>,
    last_seen_unix: Option<i64>,
    stale: bool,
}

/// Interfaces discovered on a node, with query-time utilization.
///
/// `View`, not `ManageConfig` — unlike the rest of this module. An interface list is device state
/// an operator reads, not a setting they author. Skeleton mode answers an empty list rather than
/// 503: the interface inventory is PostgreSQL-only, so "none known" is the truthful answer.
#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/interfaces", tag = "collection",
    params(("node_id" = Uuid, Path, description = "Node id")),
    responses(
        (status = 200, description = "Interfaces known for this node with query-time utilization; empty in skeleton mode", body = Vec<InterfaceRow>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View", body = super::error::ErrorBody),
    ),
)]
async fn list_node_interfaces(
    _guard: RequireView,
    State(st): State<ApiState>,
    Path(node_id): Path<Uuid>,
) -> ApiResult<Json<Vec<InterfaceRow>>> {
    let Some(admin) = st.admin.as_ref() else {
        return Ok(Json(Vec::new()));
    };
    let metas = admin.repo.list_interfaces(node_id).await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "list interfaces", "failed to list interfaces")
    })?;
    let now = now_unix_s();
    // One batched fetch for the whole node (3 TSDB round-trips), not 3 per interface — a 48-port
    // switch refreshing for every open client would otherwise be ~150 sequential queries.
    let live = st
        .store
        .node_interface_live(node_id, DEFAULT_RATE_LOOKBACK_SECS)
        .await;
    let mut out = Vec::with_capacity(metas.len());
    for m in metas {
        let ifindex = u32::try_from(m.ifindex).unwrap_or(0);
        let l = live.get(&m.ifindex).copied().unwrap_or_default();
        // Octet counters come back as bytes/sec from rate(); ×8 ⇒ bits/sec.
        let in_bps = l.in_bps.map(|r| r * 8.0);
        let out_bps = l.out_bps.map(|r| r * 8.0);
        // A zero or absent speed means "unknown", not "0 bps" — dividing by it would report an
        // infinite utilization for an interface that simply never advertised its rate.
        let speed = m.if_speed.filter(|s| *s > 0);
        let util = |bps: Option<f64>| match (bps, speed) {
            (Some(b), Some(s)) => Some(b / s as f64 * 100.0),
            _ => None,
        };
        let stale = m.last_seen_s.is_none_or(|s| now - s > INTERFACE_STALE_SECS);
        out.push(InterfaceRow {
            ifindex,
            if_name: m.if_name,
            if_alias: m.if_alias,
            if_speed_bps: m.if_speed,
            oper_status: l.oper_status,
            in_bps,
            out_bps,
            in_util_pct: util(in_bps),
            out_util_pct: util(out_bps),
            last_seen_unix: m.last_seen_s,
            stale,
        });
    }
    Ok(Json(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::{to_bytes, Body};
    use axum::http::{header::AUTHORIZATION, Request};
    use tower::ServiceExt;
    use yagra_common::{Principal, Role, Scope};

    const ID: &str = "00000000-0000-0000-0000-000000000001";

    /// Every configuration route this module serves — the interface list is deliberately absent,
    /// since it is the one endpoint here gated on `View`.
    fn config_routes() -> Vec<(&'static str, String)> {
        vec![
            ("GET", format!("/api/v1/nodes/{ID}/collection")),
            ("POST", format!("/api/v1/nodes/{ID}/collection")),
            ("DELETE", format!("/api/v1/collection/{ID}")),
            ("GET", "/api/v1/collection-templates".to_owned()),
            ("POST", "/api/v1/collection-templates".to_owned()),
            ("DELETE", format!("/api/v1/collection-templates/{ID}")),
            ("GET", format!("/api/v1/collection-templates/{ID}/items")),
            ("POST", format!("/api/v1/collection-templates/{ID}/items")),
            (
                "DELETE",
                format!("/api/v1/collection-templates/{ID}/items/{ID}"),
            ),
            ("GET", format!("/api/v1/profiles/{ID}/templates")),
            ("PUT", format!("/api/v1/profiles/{ID}/templates")),
        ]
    }

    async fn status_of(st: ApiState, method: &str, path: &str, token: Option<&str>) -> StatusCode {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        router(st)
            .oneshot(b.body(Body::from("{}")).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn collection_configuration_is_closed_to_anonymous_and_to_a_public_dashboard() {
        // Including the reads: what a device is polled for is configuration, and the OIDs in it
        // describe the fleet's internals.
        for (method, path) in config_routes() {
            assert_eq!(
                status_of(private_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "private {method} {path}"
            );
            assert_eq!(
                status_of(public_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "public {method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn a_viewer_or_operator_cannot_change_what_is_collected() {
        let st = private_state();
        for role in [Role::Viewer, Role::Operator] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            for (method, path) in config_routes() {
                assert_eq!(
                    status_of(st.clone(), method, &path, Some(&token)).await,
                    StatusCode::FORBIDDEN,
                    "{role:?} {method} {path}"
                );
            }
        }
    }

    #[tokio::test]
    async fn the_interface_list_reads_openly_and_is_empty_without_a_database() {
        // The one `View` endpoint here, and the one that answers 200 in skeleton mode: the
        // inventory is PostgreSQL-only, so "no interfaces known" is true rather than unavailable.
        let resp = router(public_state())
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/nodes/{ID}/interfaces"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            serde_json::json!([])
        );
    }

    #[test]
    fn an_item_is_validated_the_same_way_for_a_node_and_for_a_template() {
        // Both creators call this. If they validated separately the looser one would become the
        // way to get a bad OID into the poller's job.
        let ok = |metric: &str, oid: &str| CreateCollectionItem {
            metric_name: metric.to_owned(),
            oid: oid.to_owned(),
            collection: "scalar".to_owned(),
            metric_kind: "gauge".to_owned(),
            enabled: None,
        };
        assert!(validate_item(&ok("if_in_octets", "1.3.6.1.2.1.2.2.1.10")).is_ok());

        // A metric name reaches a TSDB selector; an OID reaches SNMP.
        assert_eq!(
            validate_item(&ok("up} or vector(1) #", "1.3.6.1"))
                .unwrap_err()
                .code(),
            "invalid_metric_name"
        );
        assert_eq!(
            validate_item(&ok("ok_name", "1.3.6.x")).unwrap_err().code(),
            "invalid_oid"
        );

        let mut bad_kind = ok("ok_name", "1.3.6.1");
        bad_kind.metric_kind = "histogram".to_owned();
        assert_eq!(
            validate_item(&bad_kind).unwrap_err().code(),
            "invalid_collection_item"
        );
    }
}
