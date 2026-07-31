// SPDX-License-Identifier: AGPL-3.0-only
//! Classification rules — how a discovered device's `sysObjectID` / `sysDescr` picks the device
//! profile it should be bound to.
//!
//! `ManageConfig` throughout, reads included: a rule decides what a newly discovered device gets
//! polled for, so the ruleset is monitoring configuration.
//!
//! Everything a rule matches on is validated and normalized **before** it reaches the store, in one
//! place ([`normalize`]): an empty string becomes `None` rather than a rule that matches nothing, a
//! prefix must be dotted-numeric, a regex must compile, and the referenced profile must exist. The
//! regex engine is linear-time, so a pattern cannot ReDoS — but a pattern that does not compile
//! would otherwise be stored and then fail silently on every device it was written for.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig};
use super::util::CreatedId;
use super::{is_valid_oid_prefix, AdminState, ApiState};
use crate::classification::ClassificationRepo;
use axum::{
    extract::Path,
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use serde::Deserialize;
use uuid::Uuid;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_classification_rules,
    create_classification_rule,
    update_classification_rule,
    delete_classification_rule
))]
pub(super) struct Doc;

/// The classification routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/classification-rules",
            get(list_classification_rules).post(create_classification_rule),
        )
        .route(
            "/api/v1/classification-rules/:id",
            put(update_classification_rule).delete(delete_classification_rule),
        )
}

/// Create/update body. At least one of `sysobjectid_prefix` / `sysdescr_regex` must be present.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ClassificationRuleBody {
    priority: i32,
    #[serde(default)]
    sysobjectid_prefix: Option<String>,
    #[serde(default)]
    sysdescr_regex: Option<String>,
    profile_id: String,
    #[serde(default)]
    vendor: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default = "default_enabled")]
    enabled: bool,
}

const fn default_enabled() -> bool {
    true
}

/// A validated, normalized rule ready for the repo.
struct NormalizedRule {
    priority: i32,
    prefix: Option<String>,
    regex: Option<String>,
    profile_id: Uuid,
    vendor: Option<String>,
    model: Option<String>,
    enabled: bool,
}

/// Validate and normalize a rule body. Every error here is about what the caller submitted, so all
/// of them are safe to show; the one internal failure (the profile lookup) is opaque.
async fn normalize(
    repo: &ClassificationRepo,
    body: ClassificationRuleBody,
) -> Result<NormalizedRule, ApiError> {
    // Blank means "not set". Keeping `Some("")` would store a rule with an empty prefix, which
    // matches every device or none depending on the comparison — neither is what was meant.
    let trim_opt = |v: Option<String>| -> Option<String> {
        v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
    };
    let prefix = trim_opt(body.sysobjectid_prefix);
    let regex = trim_opt(body.sysdescr_regex);
    let vendor = trim_opt(body.vendor);
    let model = trim_opt(body.model);

    if prefix.is_none() && regex.is_none() {
        return Err(ApiError::bad_request(
            "invalid_rule",
            "a rule needs a sysObjectID prefix and/or a sysDescr regex",
        ));
    }
    if let Some(p) = &prefix {
        if !is_valid_oid_prefix(p) {
            return Err(ApiError::bad_request(
                "invalid_prefix",
                "sysObjectID prefix must be a dotted-numeric OID (e.g. 1.3.6.1.4.1.9.)",
            ));
        }
    }
    if let Some(re) = &regex {
        // Compiled here purely to reject it now. A pattern that does not compile would be stored
        // happily and then fail on every device it was written for, invisibly.
        if let Err(e) = regex::Regex::new(re) {
            return Err(ApiError::bad_request(
                "invalid_regex",
                format!("sysDescr regex does not compile: {e}"),
            ));
        }
    }
    let Ok(profile_id) = body.profile_id.parse::<Uuid>() else {
        return Err(ApiError::bad_request(
            "invalid_profile",
            format!("'{}' is not a valid profile id", body.profile_id),
        ));
    };
    // A rule pointing at a profile that does not exist would classify devices into nothing.
    let exists = repo.profile_exists(profile_id).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "profile existence check",
            "failed to validate the profile",
        )
    })?;
    if !exists {
        return Err(ApiError::bad_request(
            "profile_not_found",
            format!("no profile {profile_id}"),
        ));
    }
    Ok(NormalizedRule {
        priority: body.priority,
        prefix,
        regex,
        profile_id,
        vendor,
        model,
        enabled: body.enabled,
    })
}

/// Reload the in-memory classifier after a rule edit so the change takes effect immediately.
///
/// Best-effort: the periodic refresh picks the change up regardless, so a failure here delays a
/// rule rather than losing it, and must not fail the write that already succeeded.
async fn reload_classifier(admin: &AdminState) {
    if let Err(e) = admin.classifier.reload(&admin.classification).await {
        tracing::warn!(error = %e, "failed to reload classifier after rule edit");
    }
}

#[utoipa::path(
    get, path = "/api/v1/classification-rules", tag = "classification",
    responses(
        (status = 200, description = "Every rule, in evaluation order", body = Vec<yagra_common::ClassificationRule>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn list_classification_rules(
    _guard: RequireManageConfig,
    admin: Admin,
) -> ApiResult<Json<Vec<yagra_common::ClassificationRule>>> {
    let rules = admin.classification.list_rules().await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "list classification rules",
            "failed to list classification rules",
        )
    })?;
    Ok(Json(rules))
}

#[utoipa::path(
    post, path = "/api/v1/classification-rules", tag = "classification",
    request_body = ClassificationRuleBody,
    responses(
        (status = 201, description = "Rule created", body = CreatedId),
        (status = 400, description = "The rule matches nothing, the prefix is not a dotted OID, the regex does not compile, or the profile does not exist", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn create_classification_rule(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<ClassificationRuleBody>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    let r = normalize(&admin.classification, body).await?;
    let id = admin
        .classification
        .create_rule(
            r.priority,
            r.prefix.as_deref(),
            r.regex.as_deref(),
            r.profile_id,
            r.vendor.as_deref(),
            r.model.as_deref(),
            r.enabled,
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "create classification rule",
                "failed to create classification rule",
            )
        })?;
    reload_classifier(&admin).await;
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

#[utoipa::path(
    put, path = "/api/v1/classification-rules/{id}", tag = "classification",
    params(("id" = Uuid, Path, description = "Rule id")),
    request_body = ClassificationRuleBody,
    responses(
        (status = 204, description = "Rule updated"),
        (status = 400, description = "The submitted rule is invalid", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
        (status = 404, description = "No such rule", body = super::error::ErrorBody),
    ),
)]
async fn update_classification_rule(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<ClassificationRuleBody>,
) -> ApiResult<StatusCode> {
    let r = normalize(&admin.classification, body).await?;
    let updated = admin
        .classification
        .update_rule(
            id,
            r.priority,
            r.prefix.as_deref(),
            r.regex.as_deref(),
            r.profile_id,
            r.vendor.as_deref(),
            r.model.as_deref(),
            r.enabled,
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "update classification rule",
                "failed to update classification rule",
            )
        })?;
    if !updated {
        return Err(ApiError::not_found(
            "rule_not_found",
            format!("no classification rule {id}"),
        ));
    }
    reload_classifier(&admin).await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete, path = "/api/v1/classification-rules/{id}", tag = "classification",
    params(("id" = Uuid, Path, description = "Rule id")),
    responses(
        (status = 204, description = "Rule deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
        (status = 404, description = "No such rule", body = super::error::ErrorBody),
    ),
)]
async fn delete_classification_rule(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let deleted = admin.classification.delete_rule(id).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "delete classification rule",
            "failed to delete classification rule",
        )
    })?;
    if !deleted {
        return Err(ApiError::not_found(
            "rule_not_found",
            format!("no classification rule {id}"),
        ));
    }
    reload_classifier(&admin).await;
    Ok(StatusCode::NO_CONTENT)
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

    fn all_routes() -> Vec<(&'static str, String)> {
        vec![
            ("GET", "/api/v1/classification-rules".to_owned()),
            ("POST", "/api/v1/classification-rules".to_owned()),
            ("PUT", format!("/api/v1/classification-rules/{ID}")),
            ("DELETE", format!("/api/v1/classification-rules/{ID}")),
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
    async fn the_ruleset_is_configuration_and_closed_to_everyone_below_admin() {
        for (method, path) in all_routes() {
            assert_eq!(
                status_of(private_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "anon {method} {path}"
            );
            assert_eq!(
                status_of(public_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "public {method} {path}"
            );
        }
        let st = private_state();
        for role in [Role::Viewer, Role::Operator] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            for (method, path) in all_routes() {
                assert_eq!(
                    status_of(st.clone(), method, &path, Some(&token)).await,
                    StatusCode::FORBIDDEN,
                    "{role:?} {method} {path}"
                );
            }
        }
    }
}
