// SPDX-License-Identifier: AGPL-3.0-only
//! The MIB repository (Settings ▸ MIB repository) — a curated catalog of metric-name → OID pairs.
//!
//! It is a lookup table an operator builds up, not something Yagra polls: the collection editor
//! picks entries from it when composing what a profile actually collects. Hence the split gating —
//! any viewer may browse it, only `ManageConfig` may add or remove entries.
//!
//! Both fields are validated at the edge rather than trusted downstream: a metric name is
//! interpolated into TSDB queries and an OID is handed to SNMP, so anything that is not an
//! identifier / dotted-numeric is rejected here (security.md).

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig, RequireView};
use super::util::CreatedId;
use super::{is_valid_metric_name, is_valid_oid, ApiState};
use axum::{
    extract::{Path, Query},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;

/// The MIB-catalog routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/mib-catalog",
            get(list_mib_catalog).post(create_mib_entry),
        )
        .route(
            "/api/v1/mib-catalog/:id",
            axum::routing::delete(delete_mib_entry),
        )
}

/// Free-text search over the catalog.
#[derive(Deserialize)]
struct MibQuery {
    q: Option<String>,
}

async fn list_mib_catalog(
    _guard: RequireView,
    admin: Admin,
    Query(q): Query<MibQuery>,
) -> ApiResult<Json<Vec<crate::mib::MibEntry>>> {
    // A blank or whitespace-only term means "no filter", not "match the empty string".
    let needle = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let list = admin.mib.list(needle).await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "list mib catalog", "failed to list MIB catalog")
    })?;
    Ok(Json(list))
}

/// Create-entry body for the catalog.
#[derive(Deserialize)]
struct CreateMibEntry {
    metric_name: String,
    oid: String,
    collection: String,
    metric_kind: String,
    vendor: Option<String>,
    description: Option<String>,
}

async fn create_mib_entry(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<CreateMibEntry>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
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
            "invalid_mib_entry",
            "collection must be scalar|table and metric_kind gauge|counter",
        ));
    }
    let created = admin
        .mib
        .create(
            &body.metric_name,
            &body.oid,
            &body.collection,
            &body.metric_kind,
            body.vendor.as_deref(),
            body.description.as_deref(),
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "create mib entry", "failed to create MIB entry")
        })?;
    // `None` is the unique-name conflict, not a fault: the catalog is keyed by metric name so two
    // entries cannot claim the same one.
    let id = created.ok_or_else(|| {
        ApiError::conflict(
            "metric_name_taken",
            format!(
                "a catalog entry named '{}' already exists",
                body.metric_name
            ),
        )
    })?;
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

async fn delete_mib_entry(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    match admin.mib.delete(id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "mib_entry_not_found",
            format!("no entry {id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "delete mib entry",
            "failed to delete MIB entry",
        )),
    }
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

    const ID: &str = "00000000-0000-0000-0000-000000000001";

    /// Every write method/path this module serves, so the guard tests cannot silently skip one.
    fn write_routes() -> Vec<(&'static str, String)> {
        vec![
            ("POST", "/api/v1/mib-catalog".to_owned()),
            ("DELETE", format!("/api/v1/mib-catalog/{ID}")),
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
    async fn an_anonymous_caller_is_told_it_is_anonymous_and_nothing_else() {
        for (method, path) in write_routes() {
            assert_eq!(
                status_of(private_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }
        assert_eq!(
            status_of(private_state(), "GET", "/api/v1/mib-catalog", None).await,
            StatusCode::UNAUTHORIZED,
        );
    }

    #[tokio::test]
    async fn browsing_is_open_on_a_public_dashboard_but_editing_is_not() {
        // 503 = past the read guard, into skeleton mode. The catalog is reference data the
        // collection editor reads, so a public deployment may show it…
        assert_eq!(
            status_of(public_state(), "GET", "/api/v1/mib-catalog", None).await,
            StatusCode::SERVICE_UNAVAILABLE,
        );
        // …but the write guard stays closed even there.
        for (method, path) in write_routes() {
            assert_eq!(
                status_of(public_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn a_viewer_may_browse_but_not_edit() {
        // The gating split is the point of this domain: 403 on writes, past the guard on reads.
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "viewer1",
        );
        assert_eq!(
            status_of(st.clone(), "GET", "/api/v1/mib-catalog", Some(&token)).await,
            StatusCode::SERVICE_UNAVAILABLE,
        );
        for (method, path) in write_routes() {
            assert_eq!(
                status_of(st.clone(), method, &path, Some(&token)).await,
                StatusCode::FORBIDDEN,
                "{method} {path}"
            );
        }
    }
}
