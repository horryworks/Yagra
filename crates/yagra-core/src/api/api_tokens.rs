// SPDX-License-Identifier: AGPL-3.0-only
//! Personal access tokens (Settings ▸ API tokens) — the durable credential for unattended clients,
//! on both the MCP tool surface at `/mcp` (ADR-028) and the northbound REST API.
//!
//! **A token names the surfaces it may be presented at**, and defaults to `mcp` alone. That default
//! is what makes opening REST to tokens safe to ship: every token that existed before carries it, so
//! an upgrade cannot turn a credential minted for an AI assistant into one that can reconfigure
//! monitoring. Reaching `/api/v1` is an explicit choice made when the token is issued.
//!
//! Admin-only throughout, under `ManageUsers` rather than `ManageConfig`: a token carries a role and
//! a scope, so minting one hands out an identity. That is a user-management act however it is
//! spelled, and an operator who may reconfigure monitoring still must not be able to issue
//! themselves an admin credential. For the same reason a **token cannot mint another** — user
//! administration is closed to token-authenticated callers (`extract::TOKEN_DENIED_PERMISSIONS`),
//! or a credential could issue its own successor and outlive every revocation of the original.
//!
//! **The raw token exists in one response and nowhere else.** Only its hash is stored, so create
//! returns it once and no later read can. It must never be logged or echoed back — the listing
//! carries metadata only.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, Caller, RequireManageUsers};
use super::ApiState;
use axum::{
    extract::Path,
    http::StatusCode,
    routing::{delete, get},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yagra_common::{Role, TokenSurface};

/// Longest accepted token label, in characters (not bytes — the name is operator-facing text).
const MAX_TOKEN_NAME_CHARS: usize = 128;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(list_api_tokens, create_api_token, revoke_api_token))]
pub(super) struct Doc;

/// The API-token routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/api-tokens",
            get(list_api_tokens).post(create_api_token),
        )
        .route("/api/v1/api-tokens/:id", delete(revoke_api_token))
}

/// Request body for `POST /api/v1/api-tokens`.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateApiTokenBody {
    /// Human label (unique, ≤128 chars).
    name: String,
    /// The role the token grants (`viewer` is the right default for a read-only client). Capped at
    /// the owner's role on every use, so this is a ceiling rather than a promise.
    role: Role,
    /// Visibility scope; defaults to `All` when omitted. A group scope limits the token to those
    /// node groups and everything beneath them. It must name groups that exist, and the owner must
    /// itself be unscoped — a token owned by a group-scoped account inherits that account's scope,
    /// so giving it a different one is refused rather than silently ignored.
    scope: Option<yagra_common::Scope>,
    /// Which surfaces the token may authenticate. Defaults to `["mcp"]` when omitted, matching what
    /// every token issued before this field existed can do.
    #[serde(default)]
    surfaces: Option<Vec<TokenSurface>>,
    /// When the token stops working. Omit for no expiry — appropriate for a service account driving
    /// an integration, and deliberately still allowed.
    expires_at: Option<DateTime<Utc>>,
    /// The account the token acts as. Omit to own it yourself; name a service account for anything
    /// unattended, so the credential outlives whoever set it up.
    owner_user_id: Option<Uuid>,
}

/// The one and only response carrying a usable token.
///
/// A named type rather than an inline `json!` because of what the `token` field is: the client has
/// to store it now, since only its hash is kept and no later call can produce it again. Giving it a
/// type makes that field visible in one place if this response ever grows a second consumer.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct CreatedApiToken {
    id: Uuid,
    name: String,
    role: Role,
    surfaces: Vec<TokenSurface>,
    expires_at: Option<DateTime<Utc>>,
    /// The raw bearer token, returned **once**. Never stored, never logged.
    token: String,
}

/// What [`validate_create`] resolved out of the request body.
#[derive(Debug)]
struct NewToken<'a> {
    name: &'a str,
    scope: yagra_common::Scope,
    surfaces: Vec<TokenSurface>,
    expires_at: Option<DateTime<Utc>>,
}

/// Check the request fields that need no store.
///
/// Split out of the handler because the handler's body is only reachable past the `Admin`
/// extractor, so a skeleton-mode test answers `503` and can never see these rules. Keeping the
/// judgement in a plain function is what makes them testable at all.
fn validate_create<'a>(
    body: &'a CreateApiTokenBody,
    now: DateTime<Utc>,
) -> Result<NewToken<'a>, ApiError> {
    let name = body.name.trim();
    if name.is_empty() || name.chars().count() > MAX_TOKEN_NAME_CHARS {
        return Err(ApiError::bad_request(
            "invalid_name",
            "token name must be 1–128 characters",
        ));
    }
    // Absent scope means the whole fleet, matching what the UI offers by default.
    let scope = body.scope.clone().unwrap_or(yagra_common::Scope::All);
    // A group scope is accepted here now. It used to be a `400 unsupported_scope`, because nothing
    // enforced one: a token carrying it would have been silently unrestricted over REST and
    // refused outright by `/mcp`, which is a credential whose meaning depended on where you
    // pointed it. Both surfaces filter by it now, so the refusal has been lifted rather than
    // relaxed — see `api/scope.rs` and `mcp/tools/mod.rs::scope_of`.
    //
    // The token's own scope is what it gets; it is deliberately **not** intersected with the
    // owner's. The role is capped at the owner's (`ApiTokenStore::verify`) because a role is a
    // grant that can be revoked by demotion, whereas a scope on a token is a *narrowing* an admin
    // chose for this credential — silently widening it when the owner's scope widens would make
    // "this token only sees Tokyo" untrue without anyone editing the token.
    // Absent surfaces means MCP alone: the narrow, read-mostly surface, and what every token minted
    // before this field existed is limited to. Widening is opt-in in both directions.
    let surfaces = body
        .surfaces
        .clone()
        .unwrap_or_else(|| vec![TokenSurface::Mcp]);
    if surfaces.is_empty() {
        return Err(ApiError::bad_request(
            "no_surface",
            "a token must name at least one surface it can authenticate",
        ));
    }
    // An already-past expiry mints a credential that is dead on arrival — always a mistake, and one
    // the admin would only discover when the integration failed.
    if body.expires_at.is_some_and(|t| t <= now) {
        return Err(ApiError::bad_request(
            "expiry_in_the_past",
            "the expiry must be in the future",
        ));
    }
    Ok(NewToken {
        name,
        scope,
        surfaces,
        expires_at: body.expires_at,
    })
}

/// Check a requested token scope against the groups that exist and against the prospective owner.
///
/// Two rules. The first is shared with `PUT /users/{id}/scope` — a scope naming no groups, or a
/// group that does not exist, is refused rather than stored as an account that silently sees
/// nothing.
///
/// The second is this endpoint's own: **a token owned by an already-scoped account may only be
/// `All`**, and inherits the owner's scope at verification time. A token can never exceed its
/// owner, so a different group set here could only ever be narrower — and honouring it would mean
/// intersecting two scopes on every request, which needs the folder tree to get right (a token
/// naming a child of one of the owner's roots is inside the owner's scope while sharing none of its
/// ids). Refusing the combination keeps the cap a one-line replacement that cannot be wrong in the
/// widening direction. To give a token a narrower view than its owner, own it with a service
/// account scoped to what the token should see.
async fn checked_token_scope(
    admin: &Admin,
    scope: &yagra_common::Scope,
    owner: Uuid,
) -> Result<(), ApiError> {
    if *scope == yagra_common::Scope::All {
        return Ok(());
    }
    let known: std::collections::HashSet<Uuid> = admin
        .groups
        .edges()
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "read group ids", "failed to read node groups")
        })?
        .into_iter()
        .map(|(id, _parent)| id)
        .collect();
    super::users::checked_scope(scope, &known)?;
    // An unknown owner id is left to the FK below, which already answers `400 unknown_owner`.
    let owner_scope = admin.users.scope_of(owner).await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "read owner scope", "failed to read the owner")
    })?;
    if owner_scope.is_some_and(|s| s != yagra_common::Scope::All) {
        return Err(ApiError::bad_request(
            "owner_is_scoped",
            "this owner is already limited to groups, and a token inherits its owner's scope — \
             omit `scope` or send \"All\"",
        ));
    }
    Ok(())
}

#[utoipa::path(
    post, path = "/api/v1/api-tokens", tag = "api-tokens",
    request_body = CreateApiTokenBody,
    responses(
        (status = 201, description = "Token minted; `token` is the raw bearer and is returned only here", body = CreatedApiToken),
        (status = 400, description = "Bad name, a scope naming no groups or a group that does not exist, a scope on a token whose owner is itself group-scoped, no surface named, an expiry already in the past, or an owner id that names no account", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 409, description = "An API token with that name already exists", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no token store", body = super::error::ErrorBody),
    ),
)]
/// Mint a personal access token.
///
/// The raw token is in this response and nowhere else — only its hash is stored, so no later call
/// can produce it again.
///
/// The token acts as an **account** (`owner_user_id`, defaulting to the caller): disabling or
/// deleting that account stops the token, and its role is capped at the owner's current role on
/// every use. For anything unattended, own it with a service account rather than a person — see
/// `POST /api/v1/users` — so the credential does not depend on who happened to create it.
async fn create_api_token(
    _guard: RequireManageUsers,
    caller: Caller,
    admin: Admin,
    Json(body): Json<CreateApiTokenBody>,
) -> ApiResult<(StatusCode, Json<CreatedApiToken>)> {
    let new = validate_create(&body, Utc::now())?;
    // Default the owner to the person minting it. Naming somebody else is how a token is handed to
    // a service account; the FK below is what makes an unknown id a 400 rather than an orphan.
    let owner = body.owner_user_id.unwrap_or(caller.0.user_id);
    checked_token_scope(&admin, &new.scope, owner).await?;
    let (id, raw) = admin
        .api_tokens
        .create(
            new.name,
            body.role,
            &new.scope,
            &new.surfaces,
            new.expires_at,
            owner,
            &caller.0.username,
        )
        .await
        .map_err(|e| {
            // Two expected failures, both the caller's to fix: a duplicate label, and an owner id
            // that names no account (the FK). Everything else is a fault they cannot act on.
            let db = e
                .downcast_ref::<sqlx::Error>()
                .and_then(sqlx::Error::as_database_error);
            if db.is_some_and(|db| db.is_unique_violation()) {
                return ApiError::conflict(
                    "duplicate_name",
                    "an API token with that name already exists",
                );
            }
            if db.is_some_and(|db| db.is_foreign_key_violation()) {
                return ApiError::bad_request("unknown_owner", "no account with that id");
            }
            ApiError::from_internal(e.as_ref(), "create API token", "failed to create API token")
        })?;
    Ok((
        StatusCode::CREATED,
        Json(CreatedApiToken {
            id,
            name: new.name.to_owned(),
            role: body.role,
            surfaces: new.surfaces,
            expires_at: new.expires_at,
            token: raw,
        }),
    ))
}

#[utoipa::path(
    get, path = "/api/v1/api-tokens", tag = "api-tokens",
    responses(
        (status = 200, description = "Token metadata only — never the raw token or its hash", body = Vec<crate::apitokens::ApiTokenInfo>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no token store", body = super::error::ErrorBody),
    ),
)]
/// Every issued personal access token, as metadata.
///
/// Neither the raw token nor its hash is returned — a token's value exists only in the response
/// that created it. These credentials authenticate whichever surfaces each token names — `/mcp`,
/// this REST API, or both.
async fn list_api_tokens(
    _guard: RequireManageUsers,
    admin: Admin,
) -> ApiResult<Json<Vec<crate::apitokens::ApiTokenInfo>>> {
    // Metadata only — the row type carries no secret, which is what makes this listable at all.
    let tokens = admin.api_tokens.list().await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "list API tokens", "failed to list API tokens")
    })?;
    Ok(Json(tokens))
}

#[utoipa::path(
    delete, path = "/api/v1/api-tokens/{id}", tag = "api-tokens",
    params(("id" = Uuid, Path, description = "API token id")),
    responses(
        (status = 204, description = "Token revoked; idempotent, so an already-revoked id also answers 204"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role below Admin", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no token store", body = super::error::ErrorBody),
    ),
)]
/// Revoke a personal access token, so it stops authenticating every surface immediately.
///
/// Idempotent: a missing or already-revoked id answers `204` too. Revocation is what an operator
/// reaches for when a token may be compromised, so "it was already gone" is success, not `404`.
async fn revoke_api_token(
    _guard: RequireManageUsers,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    // Idempotent: a missing or already-revoked id is a no-op 204. Revocation is what an operator
    // reaches for when a token may be compromised, so "it was already gone" is success, not 404.
    admin.api_tokens.revoke(id).await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "revoke API token", "failed to revoke API token")
    })?;
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
    use yagra_common::{Principal, Scope};

    const ID: &str = "00000000-0000-0000-0000-000000000001";

    fn all_routes() -> Vec<(&'static str, String)> {
        vec![
            ("GET", "/api/v1/api-tokens".to_owned()),
            ("POST", "/api/v1/api-tokens".to_owned()),
            ("DELETE", format!("/api/v1/api-tokens/{ID}")),
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
        for (method, path) in all_routes() {
            assert_eq!(
                status_of(private_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn token_management_is_closed_on_a_public_dashboard() {
        // Including the *read*: the listing names the roles and scopes credentials carry, which is
        // a map of what an attacker would want to steal.
        for (method, path) in all_routes() {
            assert_eq!(
                status_of(public_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn an_operator_cannot_mint_itself_a_credential() {
        // ManageUsers, not ManageConfig: an operator who may reconfigure monitoring must not be
        // able to issue a token — the token carries a role.
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

    fn body(name: &str, scope: Option<Scope>) -> CreateApiTokenBody {
        CreateApiTokenBody {
            name: name.to_owned(),
            role: Role::Viewer,
            scope,
            surfaces: None,
            expires_at: None,
            owner_user_id: None,
        }
    }

    /// A fixed "now" so the expiry rules are tested against an instant, not against the clock.
    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).expect("a valid instant")
    }

    fn check(body: &CreateApiTokenBody) -> Result<NewToken<'_>, ApiError> {
        validate_create(body, now())
    }

    #[test]
    fn a_group_scoped_token_reaches_the_store_checks_rather_than_being_refused_outright() {
        // This asserted the opposite until both surfaces filtered by scope (ADR-028 WS-F). The
        // refusal lived here because a scoped token authenticated nowhere: `/mcp` rejected a
        // non-`All` principal and REST ignored the scope entirely, so the same credential was
        // either useless or unrestricted depending on where it was pointed.
        //
        // What is left in `validate_create` is only what needs no store. The rules that do — the
        // groups must exist, and the owner must not itself be scoped — live in
        // `checked_token_scope`, past the `Admin` extractor where a skeleton-mode test answers 503
        // and could never reach them.
        let req = body("ro", Some(Scope::groups(["tokyo"])));
        let new = check(&req).expect("not refused here");
        assert_eq!(
            new.scope,
            Scope::groups(["tokyo"]),
            "and it is carried through unchanged"
        );
    }

    #[test]
    fn an_absent_or_explicit_all_scope_is_accepted() {
        // Both spellings must survive: the WebUI omits the field, an API client may send "All".
        for scope in [None, Some(Scope::All)] {
            let req = body("  ro  ", scope);
            let new = check(&req).unwrap();
            assert_eq!(new.name, "ro", "the name is trimmed");
            assert_eq!(new.scope, Scope::All);
        }
    }

    #[test]
    fn a_blank_or_oversized_name_is_refused() {
        for name in ["", "   ", &"x".repeat(MAX_TOKEN_NAME_CHARS + 1)] {
            assert_eq!(check(&body(name, None)).unwrap_err().code(), "invalid_name");
        }
        // The boundary itself is allowed, and counts characters rather than bytes.
        assert!(check(&body(&"あ".repeat(MAX_TOKEN_NAME_CHARS), None)).is_ok());
    }

    // The default is the whole reason opening REST to tokens is safe to ship: every token that
    // exists today was minted without this field, and must stay confined to `/mcp` across the
    // upgrade. A client that omits `surfaces` is asking for what it has always got.
    #[test]
    fn an_omitted_surface_list_means_mcp_alone() {
        let req = body("ro", None);
        let new = check(&req).unwrap();
        assert_eq!(new.surfaces, vec![TokenSurface::Mcp]);
    }

    #[test]
    fn a_token_naming_no_surface_is_refused() {
        // It would authenticate nowhere at all — always a mistake, never a narrow token.
        let mut req = body("ro", None);
        req.surfaces = Some(Vec::new());
        assert_eq!(check(&req).unwrap_err().code(), "no_surface");
    }

    #[test]
    fn rest_is_granted_only_when_asked_for() {
        let mut req = body("ci", None);
        req.surfaces = Some(vec![TokenSurface::Rest]);
        assert_eq!(check(&req).unwrap().surfaces, vec![TokenSurface::Rest]);

        req.surfaces = Some(vec![TokenSurface::Mcp, TokenSurface::Rest]);
        assert_eq!(
            check(&req).unwrap().surfaces,
            vec![TokenSurface::Mcp, TokenSurface::Rest],
            "both is a legitimate ask — an assistant that also drives the API"
        );
    }

    #[test]
    fn an_expiry_in_the_past_is_refused_but_no_expiry_is_fine() {
        let mut req = body("ci", None);
        req.expires_at = Some(now() - chrono::Duration::seconds(1));
        assert_eq!(check(&req).unwrap_err().code(), "expiry_in_the_past");

        // The boundary counts as past: a token expiring exactly now is already dead.
        req.expires_at = Some(now());
        assert_eq!(check(&req).unwrap_err().code(), "expiry_in_the_past");

        req.expires_at = Some(now() + chrono::Duration::days(90));
        assert_eq!(check(&req).unwrap().expires_at, req.expires_at);

        // No expiry stays allowed — a service account driving CI should not die on a date nobody
        // wrote down.
        req.expires_at = None;
        assert_eq!(check(&req).unwrap().expires_at, None);
    }

    #[tokio::test]
    async fn an_admin_clears_the_gate_and_reaches_availability() {
        // Positive control: 503 (past RBAC, into skeleton mode), not 403.
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Admin, Scope::All),
            "admin1",
        );
        for (method, path) in all_routes() {
            assert_eq!(
                status_of(st.clone(), method, &path, Some(&token)).await,
                StatusCode::SERVICE_UNAVAILABLE,
                "{method} {path}"
            );
        }
    }
    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// The raw token is returned by the mint and by nothing afterwards.
    ///
    /// The list is checked in the same test on purpose: "only its hash is stored" is a claim about
    /// two endpoints, and asserting the 201 alone would leave the half that matters unread.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn minting_a_token_hands_back_the_raw_value_once_and_never_again(pool: sqlx::PgPool) {
        use crate::api::tests_support::{account_token, live_state, send};
        let st = live_state(pool.clone()).await;
        let (tok, _) = account_token(&st, "fixture-admin", yagra_common::Role::Admin).await;
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/api-tokens",
            &tok,
            Some(serde_json::json!({ "name": "ci", "role": "viewer" })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        let raw = body["token"].as_str().expect("the raw token").to_owned();
        assert!(!raw.is_empty(), "{body}");
        assert_eq!(crate::pgtest::rows(&pool, "api_tokens").await, 1);

        let (status, list) = send(&st, "GET", "/api/v1/api-tokens", &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{list}");
        assert!(
            !list.to_string().contains(&raw),
            "the list handed the raw token back"
        );
    }
}
