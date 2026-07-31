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

// A `Leader` extractor (the ADR-016 standby guard) is not here yet either: the two handlers that
// need it still call `require_leader(&st)` in `mod.rs`. It arrives with the events domain,
// alongside the `AckAlerts` marker — same reasoning, an extractor with no user is dead code.

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
