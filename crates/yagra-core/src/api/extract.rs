// SPDX-License-Identifier: AGPL-3.0-only
//! Auth and availability guards as axum extractors.
//!
//! A guard you must remember to write is a guard you can forget to write. These were hand-copied
//! prologues at the top of nearly every handler — 154 `let Some(admin) = st.admin.as_ref() else {
//! return unavailable(); };`, 106 `if let Some(resp) = authorize(&st, &headers, …) { return resp; }`
//! and 77 `require_view(…)` — which meant omitting one was a silent authorization hole rather than a
//! compile error. Worse, they had drifted out of order: 101 handlers checked skeleton-mode
//! availability *before* authenticating and 46 authenticated first, so the same anonymous request
//! got `503 admin_unavailable` from one endpoint and `401 unauthorized` from the next.
//!
//! As extractors the guard is part of the handler's *signature*: a handler that takes
//! [`RequireManageUsers`] cannot run unauthorized, and one that takes [`Admin`] cannot run in
//! skeleton mode. Extractors run in argument order, and every guarded handler lists its permission
//! guard before [`Admin`], so the ordering question is answered once, here:
//!
//! **authenticate first, then report availability.** An unauthenticated caller learns only that it
//! is unauthenticated — never which subsystems this deployment happens to have configured.

use super::{ApiError, ApiState};
use axum::{
    async_trait,
    extract::FromRequestParts,
    http::{request::Parts, HeaderMap},
};
use std::sync::Arc;
use yagra_common::Permission;

/// Extract the `Authorization: Bearer <token>` value, if present.
pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// The caller's username from the bearer session, if any (for audit attribution).
pub(crate) fn current_username(st: &ApiState, headers: &HeaderMap) -> Option<String> {
    bearer(headers)
        .and_then(|t| st.sessions.lookup(t))
        .map(|s| s.username)
}

/// The permission a [`Require`] guard demands. Implemented by the marker types below so the
/// permission is part of the handler's type, visible in its signature.
pub trait RequiredPermission: Send + Sync + 'static {
    const PERMISSION: Permission;
    /// When true, the guard is skipped in public-dashboard mode (read-only endpoints are open).
    /// Only [`ViewPerm`] sets this — a write must stay authenticated on a public deployment.
    const OPEN_ON_PUBLIC_DASHBOARD: bool = false;
}

/// Read access. Granted to every role, and skipped entirely in public-dashboard mode.
pub struct ViewPerm;
impl RequiredPermission for ViewPerm {
    const PERMISSION: Permission = Permission::View;
    const OPEN_ON_PUBLIC_DASHBOARD: bool = true;
}

/// Manage user accounts and roles.
pub struct ManageUsersPerm;
impl RequiredPermission for ManageUsersPerm {
    const PERMISSION: Permission = Permission::ManageUsers;
}

/// Change monitoring configuration: inventory writes, bindings, thresholds, and operator actions
/// like an out-of-schedule poll. The broadest write permission in the vocabulary.
pub struct ManageConfigPerm;
impl RequiredPermission for ManageConfigPerm {
    const PERMISSION: Permission = Permission::ManageConfig;
}

/// Acknowledge, mute, or snooze alerts — an operational reaction to something happening now,
/// rather than a configuration change.
pub struct AckAlertsPerm;
impl RequiredPermission for AckAlertsPerm {
    const PERMISSION: Permission = Permission::AckAlerts;
}

/// Open or close maintenance windows — planned suppression.
pub struct ManageMaintenancePerm;
impl RequiredPermission for ManageMaintenancePerm {
    const PERMISSION: Permission = Permission::ManageMaintenance;
}

/// Create, edit or remove stored monitoring credentials.
///
/// Its own permission rather than `ManageConfig`: these are the SNMP communities, SNMPv3 USM
/// documents and device logins the whole fleet is polled with, so holding them is a strictly
/// larger power than editing what gets polled.
pub struct ManageCredentialsPerm;
impl RequiredPermission for ManageCredentialsPerm {
    const PERMISSION: Permission = Permission::ManageCredentials;
}

/// Read the audit log — who did what, across the whole system, including actions taken in domains
/// the caller cannot otherwise see. Its own permission rather than `ManageConfig` for that reason.
pub struct ViewAuditPerm;
impl RequiredPermission for ViewAuditPerm {
    const PERMISSION: Permission = Permission::ViewAudit;
}

/// A handler argument that proves the caller holds `P`. Rejects with `401` when there is no valid
/// session and `403` when the session's role lacks the permission.
pub struct Require<P: RequiredPermission>(pub std::marker::PhantomData<P>);

/// Read-gated: open in public-dashboard mode, otherwise any authenticated role.
pub type RequireView = Require<ViewPerm>;
/// Write-gated on user administration.
pub type RequireManageUsers = Require<ManageUsersPerm>;
/// Write-gated on monitoring configuration.
pub type RequireManageConfig = Require<ManageConfigPerm>;
/// Gated on alert acknowledgement / muting.
pub type RequireAckAlerts = Require<AckAlertsPerm>;
/// Gated on maintenance-window management.
pub type RequireManageMaintenance = Require<ManageMaintenancePerm>;
/// Gated on managing stored monitoring credentials.
pub type RequireManageCredentials = Require<ManageCredentialsPerm>;
/// Gated on reading the audit log.
pub type RequireViewAudit = Require<ViewAuditPerm>;

#[async_trait]
impl<P: RequiredPermission> FromRequestParts<ApiState> for Require<P> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &ApiState) -> Result<Self, Self::Rejection> {
        if P::OPEN_ON_PUBLIC_DASHBOARD && st.public_dashboard {
            return Ok(Self(std::marker::PhantomData));
        }
        match st.sessions.authorize(bearer(&parts.headers), P::PERMISSION) {
            Ok(_) => Ok(Self(std::marker::PhantomData)),
            Err(crate::auth::AuthError::Forbidden) => Err(ApiError::forbidden()),
            Err(_) => Err(ApiError::unauthorized()),
        }
    }
}

/// The caller's authenticated session — for handlers that need *who* is asking, not merely whether
/// they may.
///
/// Deliberately **not** open in public-dashboard mode, unlike [`RequireView`]: an endpoint scoped to
/// "this account" (My Dashboard) has no meaning without an account, and an anonymous public-mode
/// visitor would otherwise share one nameless layout with every other visitor.
///
/// It demands only [`Permission::View`], so a handler needing a stronger permission *and* the
/// username lists its `Require*` guard first and this second.
pub struct Caller(pub crate::auth::Session);

#[async_trait]
impl FromRequestParts<ApiState> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &ApiState) -> Result<Self, Self::Rejection> {
        match st
            .sessions
            .authorize(bearer(&parts.headers), Permission::View)
        {
            Ok(session) => Ok(Self(session)),
            Err(crate::auth::AuthError::Forbidden) => Err(ApiError::forbidden()),
            Err(_) => Err(ApiError::unauthorized()),
        }
    }
}

/// The live write side. Extracting it replaces the `let Some(admin) = st.admin.as_ref() else {…}`
/// prologue: a handler that needs the inventory/credential stores simply takes this, and skeleton
/// mode rejects it with `503 admin_unavailable` before the body runs.
///
/// List it *after* a `Require*` guard in the argument list so an unauthenticated caller cannot use
/// the 503-vs-401 difference to probe which subsystems a deployment has configured.
pub struct Admin(pub Arc<super::AdminState>);

#[async_trait]
impl FromRequestParts<ApiState> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(_: &mut Parts, st: &ApiState) -> Result<Self, Self::Rejection> {
        st.admin
            .as_ref()
            .map(|a| Self(a.clone()))
            .ok_or_else(ApiError::admin_unavailable)
    }
}

impl std::ops::Deref for Admin {
    type Target = super::AdminState;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// The OIDC provider store — present only when this deployment persists SSO configuration.
///
/// Its own extractor rather than a field read inside each handler, for the same reason as [`Admin`]:
/// six handlers each opened with the identical `let Some(oidc) = st.oidc.as_ref() else { … }`, and
/// the one that forgets it does not fail to compile.
pub struct Oidc(pub Arc<crate::oidc::OidcRepo>);

#[async_trait]
impl FromRequestParts<ApiState> for Oidc {
    type Rejection = ApiError;

    async fn from_request_parts(_: &mut Parts, st: &ApiState) -> Result<Self, Self::Rejection> {
        st.oidc
            .as_ref()
            .map(|o| Self(o.clone()))
            .ok_or_else(ApiError::admin_unavailable)
    }
}

impl std::ops::Deref for Oidc {
    type Target = crate::oidc::OidcRepo;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// The passive-event engine — present only when event ingestion is configured.
///
/// Its own extractor for the same reason as [`Admin`]: four handlers opened with the identical
/// `let Some(engine) = st.events.as_ref() else { … }`.
pub struct Events(pub Arc<crate::events::EventEngine>);

#[async_trait]
impl FromRequestParts<ApiState> for Events {
    type Rejection = ApiError;

    async fn from_request_parts(_: &mut Parts, st: &ApiState) -> Result<Self, Self::Rejection> {
        st.events
            .as_ref()
            .map(|e| Self(e.clone()))
            .ok_or_else(ApiError::admin_unavailable)
    }
}

impl std::ops::Deref for Events {
    type Target = crate::events::EventEngine;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// The caller's resolved group-visibility scope (ADR-014), for a handler that lists or aggregates
/// node-associated data.
///
/// Taking this proves the handler *asked*; what it does with the answer is still its own business,
/// which is why the route ledger records a scoping rule per endpoint and a test checks the two
/// agree. Extracting it and then ignoring it is the one failure this cannot catch on its own.
///
/// Gated like [`RequireView`], **not** like [`Caller`]: a public-dashboard deployment has decided
/// its reads are open, and its anonymous visitors resolve to [`NodeScope::All`]. Making this the
/// stricter of the two would take the public dashboard offline rather than scope it.
pub struct Scoped(pub super::scope::NodeScope);

#[async_trait]
impl FromRequestParts<ApiState> for Scoped {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &ApiState) -> Result<Self, Self::Rejection> {
        if st.public_dashboard && bearer(&parts.headers).is_none() {
            return Ok(Self(super::scope::NodeScope::All));
        }
        let session = match st
            .sessions
            .authorize(bearer(&parts.headers), Permission::View)
        {
            Ok(session) => session,
            Err(crate::auth::AuthError::Forbidden) => return Err(ApiError::forbidden()),
            Err(_) => return Err(ApiError::unauthorized()),
        };
        Ok(Self(super::scope::resolve(st, &session.principal).await?))
    }
}

impl std::ops::Deref for Scoped {
    type Target = super::scope::NodeScope;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// A `:node_id` path parameter the caller is allowed to see, for a handler addressing one node.
///
/// **Out of scope answers `404 node_not_found`, not `403`.** A 403 confirms the node exists, which
/// turns every per-node route into an id-enumeration oracle for a scoped operator — and node ids
/// are the handles that appear in alerts, MCP tools and RCA requests, so they are guessable from
/// things a scoped user legitimately holds. 404 is also the answer this API already gives when the
/// inventory cannot answer at all (`api/nodes.rs`), so the WebUI's existing branch is unchanged.
///
/// Visibility is resolved from the alert engine's fleet-wide node metadata rather than a query, so
/// this costs a hash lookup. A node the snapshot has not seen yet is treated as **not** visible —
/// see [`crate::alerts::AlertManager::node_folder_group`].
///
/// Carries no value on purpose. It is a **proof**, not an accessor: every handler that takes it
/// already extracts the id it needs from its own `Path` (often as part of a multi-param tuple), and
/// a second copy of the same id in the signature is one more thing that can be used instead of the
/// one that was parsed. `_visible: VisibleNode` in an argument list reads as what it is — the guard
/// ran.
pub struct VisibleNode;

#[async_trait]
impl FromRequestParts<ApiState> for VisibleNode {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &ApiState) -> Result<Self, Self::Rejection> {
        let Scoped(scope) = Scoped::from_request_parts(parts, st).await?;
        // `Path` reads the captured params out of the request extensions without consuming them,
        // so a handler may still extract its own `Path<…>` (including a multi-param tuple).
        let params =
            axum::extract::Path::<std::collections::HashMap<String, String>>::from_request_parts(
                parts, st,
            )
            .await
            .map(|p| p.0)
            .unwrap_or_default();
        // A route with no `:node_id`, or an unparseable one, is a mistake rather than a request to
        // honour — fail closed with the same 404 rather than waving it through. The route ledger's
        // `every_node_scoped_route_takes_the_visible_node_extractor` is what catches it at build
        // time; this is the runtime backstop.
        let node = params
            .get("node_id")
            .and_then(|v| uuid::Uuid::parse_str(v).ok())
            .map(yagra_common::NodeId::from)
            .ok_or_else(|| ApiError::not_found("node_not_found", "no such node"))?;
        if scope.allows_node(st, node) {
            Ok(Self)
        } else {
            Err(ApiError::not_found(
                "node_not_found",
                format!("no node {}", node.as_uuid()),
            ))
        }
    }
}

/// Proof that this core currently holds HA leadership (ADR-016).
///
/// Some pipelines exist only in the leader process: the event engine's persist and action channels
/// are drained by leader-only writers, and its active-alert map is fed by the leader-only event
/// pipeline. Serving those on a standby is not merely useless — ingesting there enqueues to a
/// channel nobody drains, which eventually blocks, and closing there acts on an empty map and
/// reports success. So the guard is a correctness requirement, not an optimization.
///
/// Answers `503 not_leader`, which `/readyz` lets a load balancer resolve to the right core.
pub struct Leader;

#[async_trait]
impl FromRequestParts<ApiState> for Leader {
    type Rejection = ApiError;

    async fn from_request_parts(_: &mut Parts, st: &ApiState) -> Result<Self, Self::Rejection> {
        if st.is_leader.load(std::sync::atomic::Ordering::Acquire) {
            Ok(Self)
        } else {
            Err(ApiError::not_leader())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests_support::{private_state, public_state};
    use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
    use yagra_common::{Principal, Role, Scope};

    fn parts_with(token: Option<&str>) -> Parts {
        let mut headers = HeaderMap::new();
        if let Some(t) = token {
            headers.insert(
                axum::http::header::AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {t}")).unwrap(),
            );
        }
        let mut req = Request::new(());
        *req.headers_mut() = headers;
        req.into_parts().0
    }

    #[tokio::test]
    async fn view_is_open_on_a_public_dashboard_but_gated_otherwise() {
        let public = public_state();
        assert!(
            RequireView::from_request_parts(&mut parts_with(None), &public)
                .await
                .is_ok()
        );

        let private = private_state();
        let err = RequireView::from_request_parts(&mut parts_with(None), &private)
            .await
            .err()
            .expect("a private deployment must not serve reads anonymously");
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_write_guard_stays_closed_even_on_a_public_dashboard() {
        // The public-dashboard escape hatch is for reads only — a write must never be anonymous.
        let public = public_state();
        let err = RequireManageUsers::from_request_parts(&mut parts_with(None), &public)
            .await
            .err()
            .expect("public-dashboard mode must not open write endpoints");
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_role_without_the_permission_is_403_not_401() {
        let st = private_state();
        let token = st.sessions.issue(
            uuid::Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "viewer1",
        );
        // The viewer authenticates fine (View passes) but cannot administer accounts. The two
        // failures must stay distinguishable: 401 means "who are you", 403 means "not allowed".
        assert!(
            RequireView::from_request_parts(&mut parts_with(Some(&token)), &st)
                .await
                .is_ok()
        );
        let err = RequireManageUsers::from_request_parts(&mut parts_with(Some(&token)), &st)
            .await
            .err()
            .expect("a viewer must not pass ManageUsers");
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn caller_demands_a_real_session_even_on_a_public_dashboard() {
        // Ported from `require_session_gates_on_a_real_token`, which this extractor replaced.
        // "My Dashboard" is per-account, so unlike `RequireView` it stays closed in public mode —
        // otherwise every anonymous visitor would read and write one shared, nameless layout.
        let public = public_state();
        let err = Caller::from_request_parts(&mut parts_with(None), &public)
            .await
            .err()
            .expect("an account-scoped endpoint has no meaning without an account");
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);

        // A valid bearer yields the session, which is what scopes the layout to a username.
        let st = private_state();
        let token = st.sessions.issue(
            uuid::Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "viewer1",
        );
        let caller = Caller::from_request_parts(&mut parts_with(Some(&token)), &st)
            .await
            .expect("a valid token authorizes");
        assert_eq!(caller.0.username, "viewer1");
    }

    #[tokio::test]
    async fn admin_rejects_in_skeleton_mode() {
        let st = public_state();
        let err = Admin::from_request_parts(&mut parts_with(None), &st)
            .await
            .err()
            .expect("skeleton mode has no write side");
        assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.code(), "admin_unavailable");
    }
}
