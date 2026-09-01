// SPDX-License-Identifier: AGPL-3.0-only
//! Configuration bundle: export this deployment's monitoring configuration, and import one
//! produced elsewhere (ADR-040 decision 3).
//!
//! Both directions are `ManageSystem` + Admin. The export is gated as tightly as the import even
//! though it only reads: a bundle is the whole configuration — every node, address, threshold,
//! forwarding target and match rule — which is a reconnaissance document, not a dashboard.
//!
//! Neither endpoint is group-scoped, and the ledger records why: an Admin is unscoped by
//! construction (ADR-014), so there is no partial bundle to produce. That matters more here than
//! elsewhere — a scoped export would yield a file containing *some* of the configuration with
//! nothing anywhere saying so, and importing it would look like a complete migration.
//!
//! The import is the only destructive-adjacent path in this feature, so it is upsert-only, runs in
//! one transaction, and offers `?dry_run=true` — which is the same code path with a rollback, not a
//! separate simulation that could disagree with the real thing.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageSystem};
use super::ApiState;
use crate::config_bundle::{BundleError, ConfigBundle, ImportReport};
use axum::{extract::DefaultBodyLimit, extract::Query, routing::get, Json, Router};
use serde::Deserialize;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(export_bundle, import_bundle))]
pub(super) struct Doc;

/// Ceiling on an uploaded bundle. Axum's default is 2 MiB, which a few thousand nodes exceed; the
/// export's own per-table cap is what keeps a produced bundle comfortably under this.
const MAX_BUNDLE_BODY: usize = 8 * 1024 * 1024;

/// The configuration-bundle routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/config/bundle",
            get(export_bundle).post(import_bundle),
        )
        .layer(DefaultBodyLimit::max(MAX_BUNDLE_BODY))
}

/// Import options.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub(crate) struct ImportQuery {
    /// Run the whole import and roll it back, returning the report it would have produced.
    #[serde(default)]
    dry_run: bool,
}

#[utoipa::path(
    get, path = "/api/v1/config/bundle", tag = "config",
    responses(
        (status = 200, description = "The deployment's monitoring configuration as a portable bundle. Carries no secrets — credentials, channel configs and ingest tokens stay in this deployment", body = ConfigBundle),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 413, description = "A table holds more rows than one bundle carries; use a database dump for a deployment this size", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn export_bundle(_guard: RequireManageSystem, admin: Admin) -> ApiResult<Json<ConfigBundle>> {
    let bundle = admin.config_bundle.export().await.map_err(to_api_error)?;
    tracing::info!(
        nodes = bundle.nodes.len(),
        groups = bundle.node_groups.len(),
        profiles = bundle.profiles.len(),
        "configuration bundle exported"
    );
    Ok(Json(bundle))
}

/// Apply a bundle. Existing rows with the same id are updated, new ones created; nothing is ever
/// deleted, and there is no replace mode.
#[utoipa::path(
    post, path = "/api/v1/config/bundle", tag = "config",
    params(ImportQuery),
    request_body = ConfigBundle,
    responses(
        (status = 200, description = "Import report: per-table counts plus what was skipped or changed and why. With `dry_run` nothing was committed", body = ImportReport),
        (status = 400, description = "Not a Yagra configuration bundle, or a schema version this build cannot read", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageSystem", body = super::error::ErrorBody),
        (status = 413, description = "The uploaded bundle is larger than the accepted body size", body = super::error::ErrorBody),
        (status = 503, description = "Inventory storage is unavailable (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn import_bundle(
    _guard: RequireManageSystem,
    admin: Admin,
    Query(q): Query<ImportQuery>,
    Json(bundle): Json<ConfigBundle>,
) -> ApiResult<Json<ImportReport>> {
    let report = admin
        .config_bundle
        .import(&bundle, q.dry_run)
        .await
        .map_err(to_api_error)?;
    // Worth a log line even on a dry run: this is the one endpoint that rewrites configuration in
    // bulk, and `audit_mw` records that it was called, not what it did.
    tracing::info!(
        dry_run = report.dry_run,
        tables = report.tables.len(),
        created = report.tables.iter().map(|t| t.created).sum::<u32>(),
        updated = report.tables.iter().map(|t| t.updated).sum::<u32>(),
        skipped = report.tables.iter().map(|t| t.skipped).sum::<u32>(),
        "configuration bundle imported"
    );
    Ok(Json(report))
}

/// Map the bundle's typed failures onto the status codes the UI branches on. The database variant
/// keeps its text in the log and never in the response (security.md).
fn to_api_error(e: BundleError) -> ApiError {
    match e {
        BundleError::TooLarge { table, count, cap } => ApiError::payload_too_large(
            "bundle_too_large",
            format!(
                "{table} has {count} rows, more than the {cap} a configuration bundle carries; \
                 use a database dump to move a deployment this size"
            ),
        ),
        BundleError::NotABundle => ApiError::bad_request(
            "not_a_bundle",
            "this file is not a Yagra configuration bundle",
        ),
        BundleError::UnsupportedVersion(v) => ApiError::bad_request(
            "unsupported_bundle_version",
            format!("bundle schema version {v} is newer than this deployment understands"),
        ),
        BundleError::Db(e) => {
            ApiError::from_internal(&e, "configuration bundle", "configuration bundle failed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn app(state: ApiState) -> Router {
        crate::api::router(state)
    }

    /// Gated before the store is consulted: an anonymous caller must not learn from the status code
    /// whether this deployment even has an inventory (api-conventions).
    #[tokio::test]
    async fn anonymous_cannot_export_and_is_told_only_that() {
        let resp = app(private_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/config/bundle")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// `public_dashboard` opens reads. The export is a read, and it must stay shut anyway — the
    /// whole configuration is not dashboard material.
    #[tokio::test]
    async fn a_public_dashboard_does_not_open_the_export() {
        let resp = app(public_state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/config/bundle")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn anonymous_cannot_import() {
        let resp = app(public_state())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/config/bundle?dry_run=true")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"format":"yagra.config-bundle","version":1}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// The four typed outcomes the UI branches on, pinned to their status codes.
    #[test]
    fn each_bundle_failure_maps_to_the_status_the_ui_expects() {
        let too_large = to_api_error(BundleError::TooLarge {
            table: "nodes",
            count: 20_000,
            cap: 10_000,
        });
        assert_eq!(too_large.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(too_large.code(), "bundle_too_large");

        let foreign = to_api_error(BundleError::NotABundle);
        assert_eq!(foreign.status(), StatusCode::BAD_REQUEST);
        assert_eq!(foreign.code(), "not_a_bundle");

        let newer = to_api_error(BundleError::UnsupportedVersion(9));
        assert_eq!(newer.status(), StatusCode::BAD_REQUEST);
        assert_eq!(newer.code(), "unsupported_bundle_version");
        assert!(newer.message().contains('9'));
    }

    /// A database failure must not put its own text in front of an API client (security.md).
    #[test]
    fn a_database_failure_reveals_nothing_about_itself() {
        let err = to_api_error(BundleError::Db(sqlx::Error::RowNotFound));
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!err.message().to_lowercase().contains("row"));
    }
    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// Export then import the same deployment: the round trip is accepted and adds nothing.
    ///
    /// The document this sends is the one this build produced a moment earlier, so a field the
    /// exporter writes and the importer cannot read fails here rather than at a customer's second
    /// deployment.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_bundle_this_deployment_exported_imports_back_into_it(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        send(
            &st,
            "POST",
            "/api/v1/node-groups",
            &tok,
            Some(serde_json::json!({ "name": "tokyo", "group_type": "site" })),
        )
        .await;
        let profiles = crate::pgtest::rows(&pool, "profiles").await;
        let groups = crate::pgtest::rows(&pool, "node_groups").await;

        let (status, bundle) = send(&st, "GET", "/api/v1/config/bundle", &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{bundle}");

        let (status, report) = send(&st, "POST", "/api/v1/config/bundle", &tok, Some(bundle)).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{report}");
        assert_eq!(crate::pgtest::rows(&pool, "profiles").await, profiles);
        assert_eq!(crate::pgtest::rows(&pool, "node_groups").await, groups);
    }
}
