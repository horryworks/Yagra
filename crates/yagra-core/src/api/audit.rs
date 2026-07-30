// SPDX-License-Identifier: AGPL-3.0-only
//! The audit log (Settings ▸ Audit) — who changed or acknowledged what.
//!
//! Read-only here: entries are written by `audit_mw`, which covers every mutating `/api/v1` request
//! via a deny-list, so there is no endpoint that appends one. Its own permission
//! ([`Permission::ViewAudit`](yagra_common::Permission)) rather than `ManageConfig`, because the log
//! names actions taken across domains the caller may not otherwise be allowed to see.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireViewAudit};
use super::util::parse_rfc3339;
use super::ApiState;
use axum::{extract::Query, routing::get, Json, Router};
use serde::Deserialize;

/// The audit routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new().route("/api/v1/audit", get(list_audit))
}

/// Page size plus a keyset cursor (rows strictly older than `before`).
#[derive(Deserialize)]
struct AuditQuery {
    limit: Option<i64>,
    before: Option<String>,
}

async fn list_audit(
    _guard: RequireViewAudit,
    admin: Admin,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Json<Vec<crate::audit::AuditRow>>> {
    // An unparseable cursor is rejected, not dropped: silently returning the newest page instead of
    // the requested one makes a paging bug look like "you have reached the end".
    let before = match q.before.as_deref() {
        Some(s) => Some(parse_rfc3339(s).ok_or_else(|| {
            ApiError::bad_request("invalid_cursor", "before must be an RFC 3339 timestamp")
        })?),
        None => None,
    };
    let rows = admin
        .audit
        .list(q.limit.unwrap_or(crate::audit::DEFAULT_LIMIT), before)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "list audit log", "failed to list the audit log")
        })?;
    Ok(Json(rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::Body;
    use axum::http::{header::AUTHORIZATION, Request, StatusCode};
    use tower::ServiceExt;
    use uuid::Uuid;
    use yagra_common::{Principal, Role, Scope};

    async fn status_of(st: ApiState, path: &str, token: Option<&str>) -> StatusCode {
        let mut b = Request::builder().method("GET").uri(path);
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        router(st)
            .oneshot(b.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn an_anonymous_caller_is_told_it_is_anonymous_and_nothing_else() {
        // `Require*` before `Admin`: 401, never the 503 that would say whether this deployment has
        // a write side. It answered 503 first before the migration.
        assert_eq!(
            status_of(private_state(), "/api/v1/audit", None).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn the_audit_log_is_closed_on_a_public_dashboard() {
        // Reads are open in public-dashboard mode — this one is not. The log names operators and
        // the configuration changes they made.
        assert_eq!(
            status_of(public_state(), "/api/v1/audit", None).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn a_role_without_view_audit_is_403_not_401() {
        let st = private_state();
        for role in [Role::Viewer, Role::Operator] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            assert_eq!(
                status_of(st.clone(), "/api/v1/audit", Some(&token)).await,
                StatusCode::FORBIDDEN,
                "{role:?}"
            );
        }
    }
}
