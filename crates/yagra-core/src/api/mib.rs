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

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_mib_catalog,
    list_metric_meanings,
    create_mib_entry,
    delete_mib_entry
))]
pub(super) struct Doc;

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
        .route("/api/v1/metric-meanings", get(list_metric_meanings))
}

/// One metric and what it measures, in one sentence.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct MetricMeaning {
    /// Stable metric name — the same spelling a threshold rule's `metric` and a TSDB series use.
    pub metric: String,
    /// One sentence, in English. English is canonical (`crate::metric_meaning`); the WebUI renders
    /// a translation of it, so the wording here and the wording on screen may differ by language,
    /// never by content.
    pub meaning: String,
    /// Where the number comes from: `check` (one of Yagra's own probes), `derived` (computed per
    /// port at evaluation time, and therefore present in **no** time series — asking
    /// `query_metrics` for it returns nothing), or `collected` (read off a device by a metric set,
    /// with its OID in `get_config(kind=mib_catalog)`).
    pub source: String,
}

#[utoipa::path(
    get, path = "/api/v1/metric-meanings", tag = "mib",
    responses(
        (status = 200, description = "Every metric Yagra can explain, sorted by metric name, each with where its number comes from. Static vocabulary — it does not depend on what this deployment collects", body = Vec<MetricMeaning>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks read permission", body = super::error::ErrorBody),
    ),
)]
/// What each metric measures — the dictionary behind a bare metric name (ADR-079 決定 4).
///
/// **No `Admin` extractor and no 503.** The table is compiled in, so this answers identically in
/// skeleton mode and on a public dashboard; requiring the write side would refuse a question that
/// needs no database.
///
/// ⚠️ **The WebUI does not call this**, and that is deliberate rather than an oversight. The screen
/// needs the sentence in the operator's language and synchronously during render, so it reads the
/// i18n bundle — whose English half is generated from the very same table. The documented consumers
/// are the OpenAPI contract and the `get_config(kind=metric_meanings)` MCP tool, which is the whole
/// reason the route exists: before it, "what does `icmp_loss_pct` measure" was a question the alert
/// rules table answered and `/mcp` could not.
async fn list_metric_meanings(_guard: RequireView) -> Json<Vec<MetricMeaning>> {
    Json(metric_meanings())
}

/// The dictionary as the API serves it. Shared with the MCP tool so the two cannot come to disagree
/// about which metrics are explained.
pub(crate) fn metric_meanings() -> Vec<MetricMeaning> {
    crate::metric_meaning::METRIC_MEANINGS
        .iter()
        .map(|(metric, meaning)| MetricMeaning {
            metric: (*metric).to_owned(),
            meaning: (*meaning).to_owned(),
            source: crate::metric_meaning::metric_source(metric).to_owned(),
        })
        .collect()
}

/// Maximum entries returned by one list call.
///
/// Large enough that no hand-curated catalog is ever truncated — the seeded standard + vendor sets
/// are a few hundred rows — and small enough that an unfiltered request is a query rather than a
/// table scan delivered to a client.
pub(crate) const MIB_MAX: i64 = 2000;

/// Free-text search over the catalog.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct MibQuery {
    q: Option<String>,
    /// Maximum entries to return (1–2000, default 2000).
    limit: Option<i64>,
}

/// List the catalog, filtered and capped.
///
/// The seam both edges call, so the blank-term rule and the cap exist once. A clamp on one edge
/// only is a clamp the other edge does not have (ADR-042 I1) — the MCP tool and this handler differ
/// in their *default*, which is a choice, not in their ceiling, which is not.
pub(crate) async fn mib_catalog(
    admin: &super::AdminState,
    q: Option<&str>,
    limit: Option<i64>,
) -> ApiResult<Vec<crate::mib::MibEntry>> {
    admin
        .mib
        .list(catalog_needle(q), catalog_limit(limit))
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "list mib catalog", "failed to list MIB catalog")
        })
}

/// A blank or whitespace-only term means "no filter", not "match the empty string".
fn catalog_needle(q: Option<&str>) -> Option<&str> {
    q.map(str::trim).filter(|s| !s.is_empty())
}

/// The row cap. Its own function so the test exercises the clamp the query actually gets rather
/// than a copy of the expression, which is how a clamp comes to exist on one edge only.
fn catalog_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(MIB_MAX).clamp(1, MIB_MAX)
}

#[utoipa::path(
    get, path = "/api/v1/mib-catalog", tag = "mib",
    params(MibQuery),
    responses(
        (status = 200, description = "Matching catalog entries, or the whole catalog when no term is given, capped at `limit` (1–2000, default 2000)", body = Vec<crate::mib::MibEntry>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks read permission", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn list_mib_catalog(
    _guard: RequireView,
    admin: Admin,
    Query(q): Query<MibQuery>,
) -> ApiResult<Json<Vec<crate::mib::MibEntry>>> {
    Ok(Json(mib_catalog(&admin, q.q.as_deref(), q.limit).await?))
}

/// Create-entry body for the catalog.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateMibEntry {
    metric_name: String,
    oid: String,
    collection: String,
    metric_kind: String,
    vendor: Option<String>,
    description: Option<String>,
}

#[utoipa::path(
    post, path = "/api/v1/mib-catalog", tag = "mib",
    request_body = CreateMibEntry,
    responses(
        (status = 201, description = "Entry added to the catalog", body = CreatedId),
        (status = 400, description = "The metric name is not an identifier, the OID is not dotted-numeric, or collection/metric_kind is outside its vocabulary", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 409, description = "An entry already claims that metric name", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
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

#[utoipa::path(
    delete, path = "/api/v1/mib-catalog/{id}", tag = "mib",
    params(("id" = Uuid, Path, description = "Catalog entry id")),
    responses(
        (status = 204, description = "Entry removed"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such entry", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
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

    /// The metric dictionary answers **without a store**, and still behind the read guard.
    ///
    /// Every other read in this module answers `503` in skeleton mode because it takes `Admin`.
    /// This one is a compiled-in table, so it answers `200` — which is the property that lets the
    /// `get_config(kind=metric_meanings)` tool answer above the live-mode gate rather than
    /// reporting "unavailable" where its REST twin returns a sentence. Pinned, because "it does
    /// not need the database" is a claim two places now depend on and nothing else would notice
    /// if someone added an `Admin` extractor here.
    #[tokio::test]
    async fn the_metric_dictionary_answers_without_a_store_but_not_without_a_reader() {
        assert_eq!(
            status_of(private_state(), "GET", "/api/v1/metric-meanings", None).await,
            StatusCode::UNAUTHORIZED,
            "a dictionary is not a reason to skip the read guard",
        );
        assert_eq!(
            status_of(public_state(), "GET", "/api/v1/metric-meanings", None).await,
            StatusCode::OK,
            "skeleton mode has no write side, and this read does not need one",
        );
    }

    /// The served dictionary is the table, whole — not a prefix, not a filtered view.
    #[test]
    fn the_served_dictionary_is_the_whole_table() {
        let served = metric_meanings();
        assert_eq!(served.len(), crate::metric_meaning::METRIC_MEANINGS.len());
        // A floor, so a table that emptied itself could not pass the equality above.
        assert!(served.len() > 50, "only {} metrics explained", served.len());
        let liveness = served
            .iter()
            .find(|m| m.metric == crate::alerts::LIVENESS)
            .expect("the reachability sentinel is explained — it is the one metric with no OID");
        assert!(!liveness.meaning.is_empty());
    }

    #[test]
    fn the_catalog_limit_is_clamped_to_the_cap_in_both_directions() {
        // Until ADR-042 I3b this query had no bound at all, on either edge — survivable while the
        // only caller was a settings screen an operator opened by hand, and not once an MCP tool
        // could ask for the whole table. `?limit=` is caller-supplied, so zero, negative and
        // unbounded all have to land inside the cap.
        assert_eq!(catalog_limit(None), MIB_MAX);
        assert_eq!(catalog_limit(Some(100)), 100);
        assert_eq!(catalog_limit(Some(0)), 1);
        assert_eq!(catalog_limit(Some(-5)), 1);
        assert_eq!(catalog_limit(Some(i64::MAX)), MIB_MAX);
    }

    #[test]
    fn a_blank_search_term_is_no_filter_rather_than_an_empty_match() {
        assert_eq!(catalog_needle(None), None);
        assert_eq!(catalog_needle(Some("")), None);
        assert_eq!(catalog_needle(Some("   ")), None);
        assert_eq!(catalog_needle(Some("  ifHC  ")), Some("ifHC"));
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
