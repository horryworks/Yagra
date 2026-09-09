// SPDX-License-Identifier: AGPL-3.0-only
//! Password login, logout, "who am I", and changing your own password — the `/api/v1/auth/*`
//! endpoints.
//!
//! Three of the four are the one part of the API that cannot take an authorization guard, because
//! passing them *is* how a caller becomes authorized. What stands in for a guard here is the
//! throttle: login is the only endpoint where an anonymous caller can make the server do expensive
//! work (Argon2) and learn something from the answer, so it is rate-limited per account and
//! globally before the hash is ever computed, and every outcome is audited.
//!
//! The fourth, [`change_own_password`], is the exception: it takes [`Caller`], because "change
//! *my* password" has no meaning without an identity. It lives here rather than in `api/users.rs`
//! because the account it acts on is the one in the bearer token — there is no id to put in the
//! path, and that is the point: this endpoint cannot be aimed at anybody else.
//!
//! **Audit rows carry the attempted username and never the credential** (security.md). A failed
//! login records the name that was tried — that is what makes a guessing run visible — but the
//! password never reaches a log, a response, or the audit table.
//!
//! 🚨 **Every handler in this module writes its own audit row, and must.** `audit_mw` skips
//! `/api/v1/auth/` wholesale (`api/mod.rs`), so a mutating endpoint added here that forgets
//! `audit_record` leaves no trace at all — and nothing fails.

use super::error::{ApiError, ApiResult};
use super::extract::{bearer, Admin, Caller};
use super::users::{check_password, mutation_result};
use super::util::audit_record;
use super::ApiState;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use yagra_common::{Role, Scope, UserKind};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(login, logout, auth_me, change_own_password))]
pub(super) struct Doc;

/// The auth routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/me", get(auth_me))
        .route("/api/v1/auth/password", put(change_own_password))
}

/// Login request body. Never logged, never echoed.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct LoginBody {
    username: String,
    password: String,
}

/// A freshly issued session.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct LoginOk {
    /// The bearer token for subsequent requests.
    token: String,
    /// The role it carries, so the UI can render the right navigation immediately.
    role: Role,
}

/// The caller's own identity.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct AuthMe {
    role: Role,
    username: String,
    /// Which slice of the inventory this account sees: `"All"`, or the node groups it is limited
    /// to. The UI reads it to say so out loud — an operator looking at a filtered node list has no
    /// other way to tell a narrow scope from a small fleet.
    scope: Scope,
    /// How this account signs in. The WebUI draws "Change my password" only for `local`, and for
    /// `oidc`/`ldap` says where the password actually lives — one fact on the wire, so the UI holds
    /// no copy of the rule (ADR-122 決定 5).
    ///
    /// `null` means this core has no user store to ask (skeleton mode). **Read it as "not local"** —
    /// every consumer fails closed, because the alternative is drawing a control whose write path
    /// does not exist here.
    kind: Option<UserKind>,
}

/// Exchange a username and password for a bearer token.
///
/// Takes `Admin` with no permission guard in front of it — unlike every other handler — because
/// there is nothing to authenticate yet. A skeleton deployment answering 503 here is correct and
/// not a disclosure: "there is no user store" is exactly what a would-be logger-in needs to know.
#[utoipa::path(
    post, path = "/api/v1/auth/login", tag = "session",
    security(()),
    request_body = LoginBody,
    responses(
        (status = 200, description = "A bearer token and the role it carries", body = LoginOk),
        (status = 401, description = "Incorrect username or password — one code for both, so the endpoint is not an account-enumeration oracle", body = super::error::ErrorBody),
        (status = 429, description = "Too many attempts; `Retry-After` carries the wait in seconds", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no user store, so there is nothing to log in to", body = super::error::ErrorBody),
    ),
)]
async fn login(
    State(st): State<ApiState>,
    admin: Admin,
    Json(body): Json<LoginBody>,
) -> ApiResult<Json<LoginOk>> {
    // NOTE: this handler must **not** take the `Ldap` extractor, however much it looks like it
    // should. That extractor answers 503 when no directory store exists, which is every deployment
    // that does not use one — putting it in this signature would make them all unable to log in.
    // Absence of a directory is an ordinary branch here, so `st.ldap` is read directly.
    //
    // Brute-force guard, checked *before* Argon2 or any directory bind runs: verifying a password is
    // deliberately expensive, so an unthrottled login endpoint is a CPU amplifier as well as a
    // guessing oracle — and against a directory it is worse, because repeated binds drive a real
    // domain account towards its lockout threshold.
    if let Err(reject) = st.login_throttle.check(&body.username) {
        // Recorded as plain `auth.login`: this fires before the account is looked up, so it cannot
        // know which source the name belongs to. The asymmetry with the rows below is deliberate.
        audit_record(&admin.audit, &body.username, "auth.login", 429).await;
        return Err(ApiError::too_many_requests(
            "too_many_attempts",
            format!(
                "too many login attempts; retry in {} seconds",
                reject.retry_after_secs
            ),
        )
        .retry_after(reject.retry_after_secs));
    }

    // One indexed lookup decides the path. Local accounts are resolved without touching the
    // directory at all — not even reading its configuration — so a directory that is unreachable,
    // or a KEK that cannot be read, can never delay or block a break-glass local admin.
    let route = admin
        .users
        .login_route(&body.username)
        .await
        .map_err(|e| ApiError::from_internal(e.as_ref(), "login", "login failed"))?;
    match route {
        crate::auth::LoginRoute::Local => local_login(&st, &admin, &body).await,
        crate::auth::LoginRoute::External(yagra_common::UserKind::Ldap) => {
            directory_login(&st, &admin, &body).await
        }
        // An SSO account signs in through its provider; a disabled or service account cannot sign in
        // at all. Both answer exactly as they did before this path existed.
        crate::auth::LoginRoute::External(_) | crate::auth::LoginRoute::Refused => {
            invalid_credentials(&st, &admin, &body.username, "auth.login").await
        }
        // No such account. If a directory is configured, this is how a first-time sign-in provisions
        // one; otherwise it is an ordinary unknown user.
        crate::auth::LoginRoute::Unknown => {
            if st.ldap.is_some() {
                directory_login(&st, &admin, &body).await
            } else {
                invalid_credentials(&st, &admin, &body.username, "auth.login").await
            }
        }
    }
}

/// The 401 every failed login ends in. One message for "no such user", "wrong password" and "the
/// directory said no" — telling them apart is an account-enumeration oracle.
async fn invalid_credentials(
    st: &ApiState,
    admin: &Admin,
    username: &str,
    action: &str,
) -> ApiResult<Json<LoginOk>> {
    st.login_throttle.record_failure(username);
    audit_record(&admin.audit, username, action, 401).await;
    Err(ApiError::unauthorized_with(
        "invalid_credentials",
        "incorrect username or password",
    ))
}

/// Password check against the local `users` table — unchanged behaviour.
async fn local_login(st: &ApiState, admin: &Admin, body: &LoginBody) -> ApiResult<Json<LoginOk>> {
    let verified = admin
        .users
        .verify(&body.username, &body.password)
        .await
        .map_err(|e| ApiError::from_internal(e.as_ref(), "login", "login failed"))?;
    let Some((user_id, principal)) = verified else {
        return invalid_credentials(st, admin, &body.username, "auth.login").await;
    };
    st.login_throttle.record_success(&body.username);
    let role = principal.role;
    let token = st.sessions.issue(user_id, principal, &body.username);
    audit_record(&admin.audit, &body.username, "auth.login", 200).await;
    Ok(Json(LoginOk { token, role }))
}

/// Two-stage bind against the configured LDAP/AD directory (ADR-041).
///
/// The audit action is `auth.login.ldap` rather than `auth.login`, and an unreachable directory gets
/// `auth.login.ldap_unavailable`. The client cannot tell any of these apart — every one is the same
/// 401 — but an auditor investigating a lockout must be able to separate a local password-guessing
/// run from one that is driving a real domain account towards `badPwdCount`. All three keep the
/// `auth.login` prefix, so an existing `LIKE 'auth.login%'` query still finds everything.
async fn directory_login(
    st: &ApiState,
    admin: &Admin,
    body: &LoginBody,
) -> ApiResult<Json<LoginOk>> {
    let Some(repo) = st.ldap.as_ref() else {
        return invalid_credentials(st, admin, &body.username, "auth.login").await;
    };
    let cfg = match repo.enabled_config().await {
        Ok(Some(cfg)) => cfg,
        // Not configured, switched off, or unreadable (a KEK problem). None of these is the
        // person's fault, so none of them records a throttle failure.
        Ok(None) => return invalid_credentials(st, admin, &body.username, "auth.login").await,
        Err(e) => {
            tracing::error!(error = %e, "could not load the directory configuration");
            audit_record(
                &admin.audit,
                &body.username,
                "auth.login.ldap_unavailable",
                401,
            )
            .await;
            return Err(ApiError::unauthorized_with(
                "invalid_credentials",
                "incorrect username or password",
            ));
        }
    };

    match crate::ldap::authenticate(&cfg, &body.username, &body.password).await {
        crate::ldap::LdapAuth::Unavailable(why) => {
            // **No throttle failure.** If a domain-controller outage armed the per-account
            // exponential lockout, five attempts each would leave every user locked out of Yagra
            // even after the DC came back — an outage that outlives its own cause.
            tracing::warn!(reason = %why, "the directory could not be consulted");
            audit_record(
                &admin.audit,
                &body.username,
                "auth.login.ldap_unavailable",
                401,
            )
            .await;
            Err(ApiError::unauthorized_with(
                "invalid_credentials",
                "incorrect username or password",
            ))
        }
        crate::ldap::LdapAuth::Denied(why) => {
            tracing::debug!(reason = %why, "the directory refused a login");
            invalid_credentials(st, admin, &body.username, "auth.login.ldap").await
        }
        crate::ldap::LdapAuth::Ok(identity, role) => {
            let upsert = admin
                .users
                .upsert_external_user(
                    yagra_common::UserKind::Ldap,
                    *crate::ldap::LDAP_PROVIDER_ID,
                    &identity.subject,
                    &identity.username,
                    role,
                )
                .await
                .map_err(|e| ApiError::from_internal(e.as_ref(), "login", "login failed"))?;
            match upsert {
                crate::auth::ExternalUpsert::Ok(user_id, principal) => {
                    st.login_throttle.record_success(&body.username);
                    let role = principal.role;
                    // The session and the audit row carry the **directory's** name, not what was
                    // typed: AD matches `sAMAccountName` case-insensitively while `users.username`
                    // does not, so the canonical spelling is the only one that stays stable.
                    let token = st.sessions.issue(user_id, principal, &identity.username);
                    audit_record(&admin.audit, &identity.username, "auth.login.ldap", 200).await;
                    Ok(Json(LoginOk { token, role }))
                }
                crate::auth::ExternalUpsert::Disabled => {
                    // An admin switched this account off. Not a credential failure, so it does not
                    // arm the lockout — but it must not sign in either.
                    audit_record(&admin.audit, &identity.username, "auth.login.ldap", 401).await;
                    Err(ApiError::unauthorized_with(
                        "invalid_credentials",
                        "incorrect username or password",
                    ))
                }
                crate::auth::ExternalUpsert::UsernameTaken(existing) => {
                    // Otherwise undiagnosable: the right password, a generic 401, forever.
                    tracing::warn!(
                        directory_user = %identity.username,
                        directory_dn = %identity.dn,
                        existing = %existing,
                        "directory login refused: another account already holds that username"
                    );
                    audit_record(
                        &admin.audit,
                        &identity.username,
                        "auth.login.ldap_conflict",
                        401,
                    )
                    .await;
                    Err(ApiError::unauthorized_with(
                        "invalid_credentials",
                        "incorrect username or password",
                    ))
                }
            }
        }
    }
}

/// Revoke the caller's bearer token so it cannot be reused.
///
/// Idempotent and unguarded: an absent, expired or already-revoked token still answers 204. Making
/// logout require a valid session would mean the one action that fixes a suspect token is refused
/// precisely when the token has gone bad.
#[utoipa::path(
    post, path = "/api/v1/auth/logout", tag = "session",
    security(()),
    responses(
        (status = 204, description = "Token revoked; an absent, expired or already-revoked token answers 204 too"),
    ),
)]
async fn logout(State(st): State<ApiState>, headers: HeaderMap) -> StatusCode {
    if let Some(token) = bearer(&headers) {
        st.sessions.revoke_token(token);
    }
    StatusCode::NO_CONTENT
}

/// Who the bearer token belongs to. [`Caller`] does the work: it demands a real session (not open
/// in public-dashboard mode, since an anonymous visitor has no identity to report).
#[utoipa::path(
    get, path = "/api/v1/auth/me", tag = "session",
    responses(
        (status = 200, description = "The bearer holder's role and username", body = AuthMe),
        (status = 401, description = "No valid bearer token — closed even on a public dashboard", body = super::error::ErrorBody),
    ),
)]
async fn auth_me(State(st): State<ApiState>, caller: Caller) -> ApiResult<Json<AuthMe>> {
    // `st.admin.as_ref()` rather than the `Admin` extractor, deliberately: taking the extractor
    // would make this endpoint answer 503 on a skeleton core, and "who am I" is answerable there —
    // the session in hand already carries the role, the name and the scope. Only the account kind
    // needs the store, so only that one field degrades.
    let kind = match st.admin.as_ref() {
        Some(admin) => admin.users.kind_of(caller.0.user_id).await.map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "read account kind",
                "failed to read the account",
            )
        })?,
        None => None,
    };
    Ok(Json(AuthMe {
        role: caller.0.principal.role,
        username: caller.0.username,
        scope: caller.0.principal.scope,
        kind,
    }))
}

/// Change-your-own-password request body. Neither field is logged, echoed, or audited.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ChangeOwnPassword {
    /// The password the caller signs in with today. Required even though the caller already holds a
    /// valid session — without it, a stolen session is a stolen account.
    current_password: String,
    /// What to replace it with. Same minimum length as an administrator's reset.
    new_password: String,
}

/// Change the password of the account the bearer token belongs to.
///
/// **This ends the caller's own session, on purpose** (ADR-122 決定 3). `revoke_user` is the same
/// primitive an administrator's reset uses, so there is one answer to "a password changed — what
/// happens to the tokens", and signing in again is what proves the new password actually works.
///
/// The alternative (revoke everything, then mint a replacement) is not safe here: a signed session
/// is denied when its `iat` is **at or before** the revocation cutoff, both are second-granularity,
/// and `issue` reads the clock itself — so a replacement minted in the same second would be denied,
/// and the caller could not tell that from a broken password.
#[utoipa::path(
    put, path = "/api/v1/auth/password", tag = "session",
    request_body = ChangeOwnPassword,
    responses(
        (status = 204, description = "Password changed; every session of this account — the caller's own included — is revoked, so the client must sign in again"),
        (status = 400, description = "The new password is too short (`weak_password`), is the same as the current one (`password_unchanged`), or this account signs in through a directory or an identity provider and has no local password (`not_a_local_account`)", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token, or `current_password` is wrong — one code for both", body = super::error::ErrorBody),
        (status = 404, description = "The session names an account that no longer exists", body = super::error::ErrorBody),
        (status = 429, description = "Too many attempts; `Retry-After` carries the wait in seconds", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn change_own_password(
    caller: Caller,
    admin: Admin,
    State(st): State<ApiState>,
    Json(body): Json<ChangeOwnPassword>,
) -> ApiResult<StatusCode> {
    let username = caller.0.username.clone();
    let user_id = caller.0.user_id;

    // The **same** throttle login uses, checked before Argon2 runs. Holding a session is not a
    // reason to be handed an unmetered password oracle — this is precisely the endpoint someone who
    // has stolen a session reaches for. One limiter rather than two, so guessing here spends the
    // same budget as guessing at the sign-in page. The visible consequence is that mistyping your
    // current password repeatedly also locks you out of signing in for a while; that is the trade,
    // and the alternative leaves half the budget unspent for an attacker who is already inside.
    if let Err(reject) = st.login_throttle.check(&username) {
        audit_record(&admin.audit, &username, "auth.password_change", 429).await;
        return Err(ApiError::too_many_requests(
            "too_many_attempts",
            format!(
                "too many password attempts; retry in {} seconds",
                reject.retry_after_secs
            ),
        )
        .retry_after(reject.retry_after_secs));
    }

    // Asked *before* the current password is verified, and that ordering is the entire reason this
    // lookup exists: `verify` refuses a non-local account before it ever looks at a hash, so a
    // directory user would be told their current password is wrong — false, and undiagnosable from
    // the outside. The UI does not draw the control for these accounts; this is the half that holds
    // when something other than the UI calls.
    let kind = admin.users.kind_of(user_id).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "read account kind",
            "failed to change the password",
        )
    })?;
    match kind {
        Some(UserKind::Local) => {}
        Some(UserKind::Oidc | UserKind::Ldap | UserKind::Service) => {
            audit_record(&admin.audit, &username, "auth.password_change", 400).await;
            // The message an operator reads here is `users.rs`'s, not a second wording of it.
            mutation_result(crate::auth::UserMutation::NotLocal, user_id)?;
        }
        None => {
            audit_record(&admin.audit, &username, "auth.password_change", 404).await;
            return Err(ApiError::not_found("user_not_found", "no such account"));
        }
    }

    check_password(&body.new_password)?;
    if body.new_password == body.current_password {
        // Cheap, and it saves the operator from being signed out for a change that changed nothing.
        return Err(ApiError::bad_request(
            "password_unchanged",
            "the new password is the same as the current one",
        ));
    }

    // Confirms possession, not merely a session. The uuid comparison is defensive: a session whose
    // username resolves to a *different* id would mean two accounts had exchanged names, and
    // writing a password under that assumption is the one mistake here nobody can undo.
    let verified = admin
        .users
        .verify(&username, &body.current_password)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "verify current password",
                "failed to change the password",
            )
        })?;
    match verified {
        Some((id, _)) if id == user_id => {}
        _ => {
            st.login_throttle.record_failure(&username);
            audit_record(&admin.audit, &username, "auth.password_change", 401).await;
            return Err(ApiError::unauthorized_with(
                "invalid_credentials",
                "incorrect username or password",
            ));
        }
    }

    let outcome = admin
        .users
        .set_password(user_id, &body.new_password)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "change own password",
                "failed to change the password",
            )
        })?;
    mutation_result(outcome, user_id)?;

    st.login_throttle.record_success(&username);
    // Every session this account holds, the caller's own included. An API token is deliberately
    // left alone — an administrator's reset does not revoke tokens either, and one answer to that
    // question is better than two that differ by which endpoint you used.
    st.sessions.revoke_user(user_id);
    audit_record(&admin.audit, &username, "auth.password_change", 204).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    use crate::api::tests_support::{private_state, public_state};
    use axum::body::{to_bytes, Body};
    use axum::http::{header::AUTHORIZATION, Request};
    use axum::response::IntoResponse;
    use tower::ServiceExt;
    use uuid::Uuid;
    use yagra_common::{Principal, Scope};

    async fn send(
        st: ApiState,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: &str,
    ) -> axum::response::Response {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        router(st)
            .oneshot(b.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn logout_succeeds_with_no_token_at_all() {
        // The action that fixes a suspect token must not require that token to be good.
        let resp = send(private_state(), "POST", "/api/v1/auth/logout", None, "").await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn who_am_i_needs_a_session_even_on_a_public_dashboard() {
        for st in [private_state(), public_state()] {
            let resp = send(st, "GET", "/api/v1/auth/me", None, "").await;
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn who_am_i_reports_the_bearer_holders_identity() {
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Operator, Scope::All),
            "op1",
        );
        let resp = send(st, "GET", "/api/v1/auth/me", Some(&token), "").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["username"], "op1");
        assert_eq!(json["role"], "operator");
    }

    #[tokio::test]
    async fn changing_your_own_password_is_closed_to_anonymous_callers() {
        // Gated before the store is consulted: an anonymous caller learns it is unauthenticated,
        // never whether this deployment has a write side.
        for st in [private_state(), public_state()] {
            let resp = send(
                st,
                "PUT",
                "/api/v1/auth/password",
                None,
                r#"{"current_password":"a","new_password":"bbbbbbbbbb"}"#,
            )
            .await;
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn changing_your_own_password_needs_a_user_store() {
        // A real session on a skeleton core: authenticated, then 503. The order matters — swapping
        // it would tell an anonymous caller which subsystems this deployment runs.
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Operator, Scope::All),
            "op1",
        );
        let resp = send(
            st,
            "PUT",
            "/api/v1/auth/password",
            Some(&token),
            r#"{"current_password":"a","new_password":"bbbbbbbbbb"}"#,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn who_am_i_reports_no_account_kind_without_a_user_store() {
        // The one field that degrades. It must be absent rather than guessed at `local`: every
        // consumer reads "not local" from it, and a guess would draw a control with no write path.
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Operator, Scope::All),
            "op1",
        );
        let resp = send(st, "GET", "/api/v1/auth/me", Some(&token), "").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["kind"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn login_needs_a_user_store() {
        // No guard runs first here, so skeleton mode answers 503 — which is the honest answer to
        // "let me log in" on a deployment that has no accounts.
        let resp = send(
            public_state(),
            "POST",
            "/api/v1/auth/login",
            None,
            r#"{"username":"a","password":"b"}"#,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn a_throttled_login_tells_the_client_how_long_to_wait() {
        // Without Retry-After a throttled client has to guess, and guessing wrong turns a rate
        // limit into a hot loop against it.
        let resp = ApiError::too_many_requests("too_many_attempts", "slow down")
            .retry_after(42)
            .into_response();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(resp.headers()[axum::http::header::RETRY_AFTER], "42");
    }
    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// An account created through the store can sign in, and the token it gets works.
    ///
    /// End to end on purpose: the password hash, the login handler and the session store are three
    /// separate mechanisms, and each has its own unit tests that cannot see the other two.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_real_account_can_sign_in_and_use_what_it_is_given(pool: sqlx::PgPool) {
        use crate::api::tests_support::{account_token, live_state, send};
        let st = live_state(pool.clone()).await;
        let (_, _) = account_token(&st, "operator-jo", yagra_common::Role::Operator).await;

        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/auth/login",
            "",
            Some(serde_json::json!({
                "username": "operator-jo",
                "password": "correct horse battery staple",
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        let issued = body["token"].as_str().expect("a session token").to_owned();

        let (status, nodes) = send(&st, "GET", "/api/v1/nodes", &issued, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{nodes}");
    }

    /// Changing your own password is accepted, ends the session that did it, and the new password
    /// is the one that works afterwards.
    ///
    /// All three halves in one test on purpose. Asserting only the 204 would pass against a handler
    /// that hashed nothing; asserting only the revoke would pass against one that broke the account.
    /// What makes this a *write* test rather than another refusal is the last two lines — the old
    /// password stops working and the new one starts.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn changing_your_own_password_takes_effect_and_ends_the_session(pool: sqlx::PgPool) {
        use crate::api::tests_support::{account_token, live_state, send};
        let st = live_state(pool.clone()).await;
        let (token, _) = account_token(&st, "operator-pat", yagra_common::Role::Operator).await;

        let (status, body) = send(
            &st,
            "PUT",
            "/api/v1/auth/password",
            &token,
            Some(serde_json::json!({
                "current_password": "correct horse battery staple",
                "new_password": "a much better passphrase",
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");

        // The caller's own token went with it. This is the design (ADR-122 決定 3), not a side
        // effect — a client that kept using it would be reading a session the password no longer
        // authorizes.
        let (status, _) = send(&st, "GET", "/api/v1/nodes", &token, None).await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);

        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/auth/login",
            "",
            Some(serde_json::json!({
                "username": "operator-pat",
                "password": "a much better passphrase",
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");

        let (status, _) = send(
            &st,
            "POST",
            "/api/v1/auth/login",
            "",
            Some(serde_json::json!({
                "username": "operator-pat",
                "password": "correct horse battery staple",
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);
    }

    /// The wrong current password is refused, and the account is left exactly as it was.
    ///
    /// The second half is the one worth writing: a handler that verified *after* writing would pass
    /// the 401 assertion and have already replaced the password.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_wrong_current_password_changes_nothing(pool: sqlx::PgPool) {
        use crate::api::tests_support::{account_token, live_state, send};
        let st = live_state(pool.clone()).await;
        let (token, _) = account_token(&st, "operator-sam", yagra_common::Role::Operator).await;

        let (status, _) = send(
            &st,
            "PUT",
            "/api/v1/auth/password",
            &token,
            Some(serde_json::json!({
                "current_password": "not the one",
                "new_password": "a much better passphrase",
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::UNAUTHORIZED);

        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/auth/login",
            "",
            Some(serde_json::json!({
                "username": "operator-sam",
                "password": "correct horse battery staple",
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    }

    /// A short password and a password identical to the current one are both refused, and neither
    /// refusal ends the caller's session — being signed out for a rejected change would be worse
    /// than the mistake.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_refused_change_leaves_the_session_alone(pool: sqlx::PgPool) {
        use crate::api::tests_support::{account_token, live_state, send};
        let st = live_state(pool.clone()).await;
        let (token, _) = account_token(&st, "operator-kim", yagra_common::Role::Operator).await;

        for (current, next, code) in [
            ("correct horse battery staple", "short", "weak_password"),
            (
                "correct horse battery staple",
                "correct horse battery staple",
                "password_unchanged",
            ),
        ] {
            let (status, body) = send(
                &st,
                "PUT",
                "/api/v1/auth/password",
                &token,
                Some(serde_json::json!({
                    "current_password": current,
                    "new_password": next,
                })),
            )
            .await;
            assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(body["error"]["code"], code, "{body}");
        }

        let (status, _) = send(&st, "GET", "/api/v1/nodes", &token, None).await;
        assert_eq!(status, axum::http::StatusCode::OK);
    }

    /// `/auth/me` reports the account kind, which is what the WebUI draws the control from.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn who_am_i_reports_a_real_accounts_kind(pool: sqlx::PgPool) {
        use crate::api::tests_support::{account_token, live_state, send};
        let st = live_state(pool.clone()).await;
        let (token, _) = account_token(&st, "operator-lee", yagra_common::Role::Operator).await;

        let (status, body) = send(&st, "GET", "/api/v1/auth/me", &token, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["kind"], "local", "{body}");
    }
}
