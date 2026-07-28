// SPDX-License-Identifier: AGPL-3.0-only
//! The node domain.
//!
//! Only the "poll now" action lives here so far. The rest of the inventory endpoints are still in
//! [`super`] and move here last: they are split across four separate line ranges of that file and
//! entangled with `build_node_summaries` and `pool_resolver`, so they are the migration's hardest
//! step rather than its first.

use super::extract::{Admin, RequireManageConfig};
use super::{pool_resolver, ApiError, ApiResult};
use crate::api::ApiState;
use axum::{extract::Path, http::StatusCode, routing::post, Json, Router};
use serde::Serialize;
use uuid::Uuid;

/// The node routes, merged into `/api/v1` by [`super::router`].
pub(crate) fn routes() -> Router<ApiState> {
    Router::new().route("/api/v1/nodes/:node_id/poll", post(poll_node_now))
}

/// What an out-of-schedule poll dispatched.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct PollNowResult {
    /// How many poll jobs went to the bus. Results arrive asynchronously on the normal result
    /// path, so this confirms dispatch, not completion.
    pub dispatched: usize,
    pub node_id: Uuid,
    /// The pool the jobs were published to — the node's *effective* pool, which may be inherited
    /// from its folder rather than set on the node.
    pub pool: String,
}

/// Dispatch one node's full configured poll set (ICMP liveness + SNMP scalar/table, per its
/// bindings) to the bus immediately, bypassing the scheduler's interval and jitter.
///
/// Shared by the REST handler and the MCP `poll_now` tool. Routing to the node's *effective* pool
/// is the part worth sharing: resolving it on only one of the two surfaces would poke a
/// folder-inherited node on the default pool's subject, where no poller for it is listening.
pub(crate) async fn poll_now(
    admin: &super::AdminState,
    node_id: Uuid,
) -> Result<PollNowResult, ApiError> {
    let node = admin
        .repo
        .get_node(node_id)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "poll-now: load node", "failed to load node")
        })?
        .ok_or_else(|| ApiError::not_found("node_not_found", format!("no node {node_id}")))?;
    let pool = pool_resolver(admin).await.resolve(&node).pool;
    let dispatched = admin.poll.poll_now(&node, &pool).await;
    tracing::info!(node = %node_id, dispatched, pool = %pool, "manual poll dispatched");
    Ok(PollNowResult {
        dispatched,
        node_id,
        pool,
    })
}

/// `ManageConfig` — an operator action, like a discovery scan. Audited by the mutation middleware.
/// `202` because the poll is dispatched, not finished, when this returns.
async fn poll_node_now(
    _perm: RequireManageConfig,
    admin: Admin,
    Path(node_id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<PollNowResult>)> {
    Ok((StatusCode::ACCEPTED, Json(poll_now(&admin, node_id).await?)))
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

    fn poll_request(token: Option<&str>) -> Request<Body> {
        let mut b = Request::builder()
            .method("POST")
            .uri(format!("/api/v1/nodes/{}/poll", Uuid::nil()));
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        b.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn a_write_stays_closed_on_a_public_dashboard() {
        // `public_dashboard` opens reads only. A manual poll is an operator action that reaches
        // real devices, so it must stay authenticated even where the dashboard is open.
        let resp = router(public_state())
            .oneshot(poll_request(None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_viewer_is_forbidden_not_unauthorized() {
        // The two must stay distinguishable: 401 means "who are you", 403 means "not allowed".
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "viewer1",
        );
        let resp = router(st)
            .oneshot(poll_request(Some(&token)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn an_authorized_caller_in_skeleton_mode_gets_the_availability_error() {
        // Permission first, availability second: an operator who *is* allowed learns that the
        // write side is absent, which is exactly what the anonymous caller above must not.
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Admin, Scope::All),
            "admin1",
        );
        let resp = router(st)
            .oneshot(poll_request(Some(&token)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn the_result_reports_the_pool_the_jobs_actually_went_to() {
        // The pool is in the DTO because it is the non-obvious half of the answer: a node with no
        // pool of its own is polled on its folder's, and "dispatched: 3" alone hides where.
        let json = serde_json::to_value(PollNowResult {
            dispatched: 3,
            node_id: Uuid::nil(),
            pool: "site-osaka".to_owned(),
        })
        .unwrap();
        assert_eq!(json["dispatched"], 3);
        assert_eq!(json["pool"], "site-osaka");
    }
}
