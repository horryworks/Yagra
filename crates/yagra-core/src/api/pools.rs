// SPDX-License-Identifier: AGPL-3.0-only
//! Poller pools: the picker's option list, and the CRUD that lets an operator name one on purpose
//! (ADR-107).
//!
//! ## A pool is not one row, and this module is where that is reconciled
//!
//! Five things can say a pool exists, and the option list is their **union**:
//!
//! 1. a row in `pools` — somebody described it deliberately (ADR-107);
//! 2. a node assigned to it (`nodes.pool`);
//! 3. a folder assigning it (`node_groups.pool`);
//! 4. a live poller reporting it;
//! 5. an outstanding takeover naming it as the pool being covered for (ADR-107 増分 4, the newest).
//!
//! ⚠️ **(5) is not decoration and it was added because the round-trip test failed without it.**
//! Covering a pool moves every row that named it, so at that instant (2)–(4) all fall silent and
//! the pool would drop out of this list — taking the "restore" affordance with it, since that hangs
//! off the row. It is the same failure as `55816c6` below, one increment later.
//!
//! 🚨 **Reading only (1) would be the tempting simplification and it is wrong.** A deployment that
//! has been running since before this table existed has pools of kinds 2–4 and no rows at all, and
//! an N-1 core writes a pool name without ever learning about the table. Dropping those from the
//! picker is the failure ADR-068 names outright — and ADR-009's `55816c6` already paid once for the
//! narrower version of it, where a pool built from node rows alone stopped being reconciled the
//! moment its last node moved away.
//!
//! ⚠️ **This also means the vocabulary is not closed**, so ADR-053's decision stands unchanged:
//! `pool` remains the one list filter that does not 400 on a token it has never seen.
//!
//! Membership lives elsewhere and stays there: which nodes are in a pool is `repo/nodes.rs` and
//! `groups.rs`, which poller serves one is what that poller reports (`crate::pollers`).

use std::collections::HashSet;
use std::time::Instant;

use axum::{
    extract::Path,
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, Caller, RequireManageSystem, RequireView};
use super::ApiState;

#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_pools,
    create_pool,
    update_pool,
    delete_pool,
    take_over_pool,
    restore_pool
))]
pub(super) struct Doc;

/// The pool routes, merged into `/api/v1` by [`super::router`].
pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/pools", get(list_pools).post(create_pool))
        .route(
            "/api/v1/pools/:name",
            put(update_pool).delete(delete_pool_route),
        )
        // ADR-107 増分 4. Two verbs on one pool rather than a `PUT :name` field, because these are
        // events rather than state: "cover for this" and "stop covering" each move rows and record
        // or forget why, and expressing them as a property of the pool would make an idempotent
        // re-`PUT` re-take-over a pool a person had already restored.
        .route("/api/v1/pools/:name/takeover", post(take_over_pool))
        .route("/api/v1/pools/:name/restore", post(restore_pool))
}

// ── The option list ──────────────────────────────────────────────────────────

/// One pool offered by the pool picker.
#[derive(Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, utoipa::ToSchema)]
pub(crate) struct PoolOption {
    /// Pool name.
    name: String,
    /// Whether a live poller currently serves it. A pool with none takes the legacy per-job path
    /// onto a subject nothing subscribes to, so its jobs are silently discarded — the picker has to
    /// say so rather than present it as an equivalent choice.
    live: bool,
    /// Why this pool exists, in the operator's words. `None` for a pool nobody has described —
    /// including every pool that predates the `pools` table, which is most of them on an existing
    /// deployment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// The pool currently polling this one's members on its behalf, if an operator asked for that
    /// (ADR-107 増分 4). `None` is the ordinary case.
    ///
    /// ⚠️ Its members are **already** in that pool — this says the move is recorded and can be
    /// undone, not that it is pending. A UI that reads it as "will be" would offer a takeover that
    /// has already happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    covered_by: Option<String>,
}

/// The pools that exist, for the assignment picker.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct PoolOptions {
    pools: Vec<PoolOption>,
}

/// Merge the described pools, the pools nodes use, the pools folders assign, and the pools with a
/// live poller into the picker's option list. Pure, so the union, the default-first ordering, the
/// `live` flag and the description carry-over are unit-testable without a database or a coordinator.
///
/// [`yagra_bus::DEFAULT_POOL`] is always offered (it is where an unassigned node lands, whether or
/// not anything references it explicitly); the rest follow alphabetically.
fn build_pool_options(
    described: Vec<(String, Option<String>)>,
    node_pools: Vec<String>,
    group_pools: Vec<String>,
    live: &HashSet<String>,
    covered: &std::collections::BTreeMap<String, String>,
) -> Vec<PoolOption> {
    let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut desc: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for (name, description) in described {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            continue;
        }
        names.insert(trimmed.to_owned());
        if let Some(d) = description.filter(|d| !d.trim().is_empty()) {
            desc.insert(trimmed.to_owned(), d);
        }
    }
    for p in node_pools.into_iter().chain(group_pools) {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            names.insert(trimmed.to_owned());
        }
    }
    names.extend(live.iter().cloned());
    // 🚨 The fifth source, and it is not decoration. Taking a pool over moves every node and folder
    // that named it, so the moment the takeover succeeds none of sources 2–4 mention the pool any
    // more and it **vanishes from this list** — taking the "restore" button with it, because the
    // button hangs off the row. The pool exists precisely because something is being covered for
    // it. Found by the round-trip test, which is the shape of the failure the module doc already
    // warns about from ADR-009's `55816c6`: a pool assembled from members alone stops being
    // reconciled the moment its last member moves away.
    names.extend(covered.keys().cloned());
    names.remove(yagra_bus::DEFAULT_POOL);

    let option = |name: String| PoolOption {
        live: live.contains(&name),
        description: desc.get(&name).cloned(),
        covered_by: covered.get(&name).cloned(),
        name,
    };
    std::iter::once(option(yagra_bus::DEFAULT_POOL.to_owned()))
        .chain(names.into_iter().map(option))
        .collect()
}

/// `GET /api/v1/pools` — the pools that exist, for the assignment picker. Names only, no telemetry.
///
/// Deliberately separate from `GET /pollers`, which scans the whole node table to build its
/// per-pool counts; this is one small table plus two indexed `DISTINCT`s, and is loaded by an
/// ordinary page.
#[utoipa::path(
    get, path = "/api/v1/pools", tag = "nodes",
    responses(
        (status = 200, description = "The pools on offer, default first, each flagged with whether a live poller serves it", body = PoolOptions),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the View permission", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn list_pools(_perm: RequireView, admin: Admin) -> ApiResult<Json<PoolOptions>> {
    Ok(Json(pool_options(&admin).await))
}

/// The known poller pools and whether each has a live poller — shared by `GET /api/v1/pools`, the
/// support bundle, and the MCP `get_system_health(section="pools")` tool (ADR-042 I3a).
pub(crate) async fn pool_options(admin: &super::AdminState) -> PoolOptions {
    // A read error degrades to "fewer suggestions", never to a failed picker — the operator can
    // still type any pool via Custom.
    let described = admin
        .repo
        .list_pools()
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "listing described pools failed");
            Vec::new()
        })
        .into_iter()
        .map(|r| (r.name, r.description))
        .collect();
    let node_pools = admin.repo.distinct_pools().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "listing node pools failed");
        Vec::new()
    });
    let group_pools = admin.groups.distinct_pools().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "listing folder pools failed");
        Vec::new()
    });
    let live = admin.coordinator.live_pools(Instant::now());
    // ADR-107 増分 4. Normally empty, and a small `GROUP BY` when it is not — this stays within the
    // "one small table plus two indexed DISTINCTs" the doc above promises. It degrades to "no pool
    // is marked covered" on a read error, the same way the other three do: a picker that refuses to
    // render is worse than one that omits a badge.
    let covered = admin
        .repo
        .pool_takeovers()
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "listing pool takeovers failed");
            Vec::new()
        })
        .into_iter()
        .map(|t| (t.from_pool, t.to_pool))
        .collect();
    PoolOptions {
        pools: build_pool_options(described, node_pools, group_pools, &live, &covered),
    }
}

// ── CRUD ─────────────────────────────────────────────────────────────────────

/// Validate a pool name the same way an assignment is validated, and for the same reason.
///
/// The name becomes the NATS subject token in `yagra.jobs.{pool}`. A name carrying a `.` partitions
/// the subject and the jobs are published where nothing subscribes — plain NATS, so they are
/// discarded rather than queued. See `api/nodes.rs::validate_pool_update`, which enforces this on
/// the assignment side; a pool created here that could not be assigned would be a trap.
pub(super) fn validate_pool_name(name: &str) -> Result<String, ApiError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_pool",
            "pool name must not be empty",
        ));
    }
    if trimmed.len() > 63 {
        return Err(ApiError::bad_request(
            "invalid_pool",
            "pool name must be 63 characters or fewer",
        ));
    }
    if yagra_bus::subjects::sanitize_token(trimmed) != trimmed {
        return Err(ApiError::bad_request(
            "invalid_pool",
            "pool name may contain only letters, digits, '_' or '-'",
        ));
    }
    Ok(trimmed.to_owned())
}

/// Trim a supplied description to `None` when it carries nothing.
fn clean_description(d: Option<String>) -> Option<String> {
    d.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct CreatePoolRequest {
    /// The pool name. Becomes a NATS subject token, so letters, digits, `_` and `-` only.
    name: String,
    /// Why this pool exists, in the operator's words. Optional.
    #[serde(default)]
    description: Option<String>,
}

/// `POST /api/v1/pools` — describe a pool deliberately.
///
/// Creating one starts nothing. A pool does work only once **both** halves exist: nodes assigned to
/// it, and a poller reporting it. The UI says so at the point of creation rather than letting an
/// operator discover it from a silent absence of data — with only nodes, the scheduler falls back to
/// legacy per-job publish onto a subject nobody subscribes to and the jobs are discarded; with only
/// a poller, nothing is scheduled at all.
#[utoipa::path(
    post, path = "/api/v1/pools", tag = "pollers",
    request_body = CreatePoolRequest,
    responses(
        (status = 201, description = "The pool was described"),
        (status = 400, description = "The name is not a valid subject token", body = super::error::ErrorBody),
        (status = 409, description = "A pool of that name is already described", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the ManageSystem permission", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn create_pool(
    _perm: RequireManageSystem,
    caller: Caller,
    admin: Admin,
    Json(req): Json<CreatePoolRequest>,
) -> ApiResult<axum::http::StatusCode> {
    let name = validate_pool_name(&req.name)?;
    let description = clean_description(req.description);
    let created = admin
        .repo
        .create_pool(
            &name,
            description.as_deref(),
            Some(caller.0.username.as_str()),
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "create pool", "failed to create the pool")
        })?;
    if !created {
        return Err(ApiError::conflict(
            "pool_exists",
            "a pool with that name is already described",
        ));
    }
    Ok(axum::http::StatusCode::CREATED)
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct UpdatePoolRequest {
    /// A new name. Omit to leave it unchanged.
    ///
    /// ⚠️ Renaming moves every node and folder assignment, and is **refused while any poller reports
    /// the old name** — see the handler.
    #[serde(default)]
    name: Option<String>,
    /// Replacement description. Omit to leave it unchanged; send `null` or `""` to clear it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<Option<String>>,
}

/// What a refusal names, so the operator knows which thing to move first.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct PoolInUse {
    /// Nodes assigned to this pool directly.
    nodes: i64,
    /// Folders assigning this pool.
    folders: i64,
    /// Poller ids reporting this pool, or recorded as heading to it.
    pollers: Vec<String>,
}

/// `PUT /api/v1/pools/{name}` — rename a pool and/or replace its description.
///
/// 🚨 **A rename is refused while a poller serving the old name cannot follow a pool change**, and
/// that refusal is the whole safety of this endpoint. Renaming moves `nodes.pool`,
/// `node_groups.pool` **and `pollers.pool`** in one transaction (ADR-107 Inc.2 — before core owned
/// the last of those, this had to refuse for *any* poller at all). What is still refused is a
/// poller that will not act on the change: one that is offline, or whose build predates
/// [`yagra_bus::CAP_POOL_FOLLOW`]. Rename out from under one of those and it goes on listening for
/// the old name while the new name's nodes are published to a subject nobody subscribes to —
/// **plain NATS discards them**. That is a monitoring hole opened by a button, and nothing surfaces
/// it until `pool_coverage`'s 300s debounce.
///
/// 🚨 **The default pool is refused outright** (ADR-107 増分 3). Its name is a constant in the
/// code, not a row here, so renaming the row renames the description and nothing else: every
/// node that is in the pool only by inheritance keeps resolving to the constant and is left
/// behind by the pollers that follow the new name — the same hole, through a different door.
#[utoipa::path(
    put, path = "/api/v1/pools/{name}", tag = "pollers",
    params(("name" = String, Path, description = "The pool to update")),
    request_body = UpdatePoolRequest,
    responses(
        (status = 204, description = "The pool was updated"),
        (status = 400, description = "The new name is not a valid subject token", body = super::error::ErrorBody),
        (status = 404, description = "No pool of that name is described", body = super::error::ErrorBody),
        (status = 409, description = "A poller serving the old name cannot follow the change, the new name is taken, or the pool is the default one (which cannot be renamed)", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the ManageSystem permission", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn update_pool(
    _perm: RequireManageSystem,
    admin: Admin,
    Path(name): Path<String>,
    Json(req): Json<UpdatePoolRequest>,
) -> ApiResult<axum::http::StatusCode> {
    if let Some(description) = req.description {
        let found = admin
            .repo
            .set_pool_description(&name, clean_description(description).as_deref())
            .await
            .map_err(|e| {
                ApiError::from_internal(e.as_ref(), "set pool description", "failed to update")
            })?;
        if !found {
            return Err(ApiError::not_found("pool_not_found", "no such pool"));
        }
    }

    if let Some(raw) = req.name {
        let to = validate_pool_name(&raw)?;
        if to != name {
            // 🚨 **The default pool's name is a constant, not data** (`yagra_bus::DEFAULT_POOL`),
            // so renaming its row does not rename the pool — it detaches the description and
            // leaves a hole. `rename_pool` re-points every row that *names* `default`, pollers
            // included, but a node that names nothing resolves to the constant and stays behind:
            // the pollers would follow the new name while the inventory that is in the pool only
            // by inheritance keeps waiting on the old one, unpolled and unreported. That is the
            // same failure ADR-107 増分 3 fixes in the move path, reached by another door, and the
            // delete path already refuses this name for the mirror-image reason.
            if name == yagra_bus::DEFAULT_POOL {
                return Err(ApiError::conflict(
                    "pool_in_use",
                    "the default pool cannot be renamed — it is where an unassigned node lands, \
                     and those nodes would be left behind in it",
                ));
            }
            let claimants = admin
                .repo
                .pollers_reporting_pool(&name)
                .await
                .map_err(|e| {
                    ApiError::from_internal(e.as_ref(), "pool claimants", "failed to read pollers")
                })?;
            // Only the ones that cannot follow the change (ADR-107 Inc.2). The rename carries
            // `pollers.pool` now, so serving the old name is no longer a reason to refuse — but a
            // poller that will not act on the new name is, and for exactly the reason the old
            // blanket refusal existed: it would keep listening for a subject that no longer
            // receives anything, silently.
            let now = std::time::Instant::now();
            let stuck: Vec<String> = claimants
                .into_iter()
                .filter(|id| {
                    !admin
                        .coordinator
                        .caps_of(id, now)
                        .is_some_and(|caps| caps.iter().any(|c| c == yagra_bus::CAP_POOL_FOLLOW))
                })
                .collect();
            if !stuck.is_empty() {
                return Err(ApiError::conflict(
                    "pool_claimed_by_poller",
                    format!(
                        "cannot rename: {} cannot follow a pool change — offline, or a build older \
                         than this release. Bring them back or upgrade them first; renaming would \
                         leave them listening for the old name with no error shown.",
                        stuck.join(", ")
                    ),
                ));
            }
            let found = admin.repo.rename_pool(&name, &to).await.map_err(|e| {
                // A primary-key collision is the likeliest failure and is the operator's to fix.
                ApiError::from_internal(e.as_ref(), "rename pool", "failed to rename the pool")
            })?;
            if !found {
                return Err(ApiError::not_found("pool_not_found", "no such pool"));
            }
        }
    }
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// `DELETE /api/v1/pools/{name}` — stop describing a pool.
///
/// ⚠️ **Refused while anything still names it**, and that is not caution: the name is *derived* from
/// nodes, folders and pollers as well as from this table, so deleting the row of a pool still in use
/// would not free the name. The pool would keep appearing in the picker with its description gone —
/// a delete that appears to succeed and changes nothing.
#[utoipa::path(
    delete, path = "/api/v1/pools/{name}", tag = "pollers",
    params(("name" = String, Path, description = "The pool to delete")),
    responses(
        (status = 204, description = "The pool is no longer described"),
        (status = 404, description = "No pool of that name is described", body = super::error::ErrorBody),
        (status = 409, description = "Nodes, folders or pollers still name this pool", body = PoolInUse),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the ManageSystem permission", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_pool(
    _perm: RequireManageSystem,
    admin: Admin,
    Path(name): Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    let refs = admin.repo.pool_references(&name).await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "pool references", "failed to read references")
    })?;
    if !refs.is_empty() {
        let mut parts = Vec::new();
        if refs.nodes > 0 {
            parts.push(format!("{} node(s)", refs.nodes));
        }
        if refs.folders > 0 {
            parts.push(format!("{} folder(s)", refs.folders));
        }
        if !refs.pollers.is_empty() {
            parts.push(format!("poller(s) {}", refs.pollers.join(", ")));
        }
        return Err(ApiError::conflict(
            "pool_in_use",
            format!("still named by {}", parts.join(", ")),
        ));
    }
    // `default` is where an unassigned node lands whether or not a row describes it, so removing
    // its row would leave a pool nobody can describe again. Refuse rather than silently no-op.
    if name == yagra_bus::DEFAULT_POOL {
        return Err(ApiError::conflict(
            "pool_in_use",
            "the default pool cannot be deleted — it is where an unassigned node lands",
        ));
    }
    let found = admin.repo.delete_pool(&name).await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "delete pool", "failed to delete the pool")
    })?;
    if !found {
        return Err(ApiError::not_found("pool_not_found", "no such pool"));
    }
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// axum needs a distinct fn item per method on a shared path; the handler above carries the doc.
async fn delete_pool_route(
    perm: RequireManageSystem,
    admin: Admin,
    path: Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    delete_pool(perm, admin, path).await
}

// ── Covering a pool that lost its poller (ADR-107 増分 4) ────────────────────

/// Where a covered pool's members should be pointed.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct TakeOverPoolRequest {
    /// The pool to point them at — one that has a live poller, usually the co-located one.
    to: String,
}

/// What one call re-pointed.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct PoolTakeoverResult {
    /// Nodes re-pointed.
    nodes: u64,
    /// Folders re-pointed.
    folders: u64,
}

/// `POST /api/v1/pools/{name}/takeover` — point this pool's members at one that can poll them.
///
/// The second of the two things an operator can do about a pool with no live poller. The first —
/// bring the site back — no endpoint can do for them: the site's own box has to be handed a new
/// bundle, and this deployment cannot reach it. This one is reversible; `restore_pool` puts every
/// member back where this found it, **including the ones that were inheriting rather than
/// assigned**, which is why the record is a table and not a column.
///
/// 🚨 **This is never the right thing to do merely because a pool is uncovered.** A site poller
/// usually exists because core cannot reach those devices; covering them from a host that cannot
/// see them turns one accurate pool alert into N false `unreachable` ones — worse than the silence
/// it replaces, and indistinguishable from a real outage. It is offered to a person looking at the
/// alert, who can test reachability from this host first (ADR-107 増分 4 決定 4). Nothing calls it
/// automatically and nothing defaults to it.
#[utoipa::path(
    post, path = "/api/v1/pools/{name}/takeover", tag = "system",
    params(("name" = String, Path, description = "the pool that has no live poller")),
    request_body = TakeOverPoolRequest,
    responses(
        (status = 200, description = "how many nodes and folders were re-pointed", body = PoolTakeoverResult),
        (status = 400, description = "the destination is not a usable pool name, or is this pool"),
        (status = 401, description = "not signed in"),
        (status = 403, description = "ManageSystem required"),
        (status = 503, description = "no admin state"),
    ),
    security(("bearer" = []))
)]
async fn take_over_pool(
    _perm: RequireManageSystem,
    caller: Caller,
    admin: Admin,
    Path(name): Path<String>,
    Json(req): Json<TakeOverPoolRequest>,
) -> ApiResult<Json<PoolTakeoverResult>> {
    let from = validate_pool_name(&name)?;
    let to = validate_pool_name(&req.to)?;
    if from == to {
        return Err(ApiError::bad_request(
            "same_pool",
            "the destination is the pool being covered for",
        ));
    }

    // Who is in `from` — by *effective* pool, not by the column. ADR-107 増分 3: a node is in a
    // pool three ways and the two no column records are the majority, so counting rows here would
    // move a fraction of what the operator was shown and report success.
    let resolver = super::util::pool_resolver(&admin).await;
    let inventory =
        crate::pool_coverage::pool_dependent_nodes(&admin.repo, &admin.meraki_devices).await;
    let members = resolver.members(&inventory, &from);

    let counts = admin
        .repo
        .take_over_pool(
            &to,
            &caller.0.username,
            crate::repo::PoolCarry {
                from: &from,
                fall_through: &members.fall_through,
            },
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "take over pool", "failed to re-point the pool")
        })?;

    // 🚨 `warn`, not `info`. This is a person deliberately monitoring devices from somewhere that
    // may not be able to see them, during an incident, and the audit row alone does not say how
    // much moved.
    tracing::warn!(
        from = %from,
        to = %to,
        nodes = counts.nodes,
        folders = counts.folders,
        by = %caller.0.username,
        "pool members covered from another pool"
    );
    Ok(Json(PoolTakeoverResult {
        nodes: counts.nodes,
        folders: counts.folders,
    }))
}

/// `POST /api/v1/pools/{name}/restore` — put a covered pool's members back.
///
/// Each member returns to **its own** recorded assignment, which for an inheriting node means no
/// assignment at all. Restoring them all to the pool's name would pin rows that were never pinned,
/// leaving a deployment the takeover never promised to be reversible about.
///
/// A member whose pool a person has since changed by hand keeps that change — a later human
/// decision outranks this bookkeeping — but its record is dropped either way, so a pool cannot stay
/// marked as covered forever.
///
/// Restoring is deliberately **not** automatic when the site's poller returns: 22 nodes moving on
/// their own the moment a link comes back is its own surprise (ADR-107 増分 4 やらないこと).
#[utoipa::path(
    post, path = "/api/v1/pools/{name}/restore", tag = "system",
    params(("name" = String, Path, description = "the pool that was being covered for")),
    responses(
        (status = 200, description = "how many nodes and folders went back", body = PoolTakeoverResult),
        (status = 400, description = "not a usable pool name"),
        (status = 401, description = "not signed in"),
        (status = 403, description = "ManageSystem required"),
        (status = 503, description = "no admin state"),
    ),
    security(("bearer" = []))
)]
async fn restore_pool(
    _perm: RequireManageSystem,
    caller: Caller,
    admin: Admin,
    Path(name): Path<String>,
) -> ApiResult<Json<PoolTakeoverResult>> {
    let from = validate_pool_name(&name)?;
    let counts = admin
        .repo
        .restore_taken_over_pool(&from)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "restore pool", "failed to put the members back")
        })?;
    tracing::info!(
        pool = %from,
        nodes = counts.nodes,
        folders = counts.folders,
        by = %caller.0.username,
        "pool members restored to their own assignments"
    );
    Ok(Json(PoolTakeoverResult {
        nodes: counts.nodes,
        folders: counts.folders,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn described(rows: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
        rows.iter()
            .map(|(n, d)| ((*n).to_owned(), d.map(str::to_owned)))
            .collect()
    }

    /// The ordinary case: no pool is being covered for another. Named rather than inlined so the
    /// one test that DOES pass a cover reads as the exception it is.
    fn no_cover() -> std::collections::BTreeMap<String, String> {
        std::collections::BTreeMap::new()
    }

    #[test]
    fn a_covered_pool_says_who_is_covering_it_and_the_others_stay_silent() {
        let covered: std::collections::BTreeMap<String, String> =
            [("tokyo".to_owned(), "default".to_owned())]
                .into_iter()
                .collect();
        let opts = build_pool_options(
            Vec::new(),
            vec!["tokyo".to_owned(), "osaka".to_owned()],
            Vec::new(),
            &HashSet::new(),
            &covered,
        );
        let by = |name: &str| {
            opts.iter()
                .find(|o| o.name == name)
                .unwrap_or_else(|| panic!("{name} is in the option list"))
                .covered_by
                .clone()
        };
        assert_eq!(by("tokyo").as_deref(), Some("default"));
        // The other half, and the one that matters: a badge on every pool would say nothing.
        assert_eq!(by("osaka"), None);
        assert_eq!(by(yagra_bus::DEFAULT_POOL), None);
    }

    /// A pool that is covered stays in the list **even though nothing names it any more**.
    ///
    /// 🚨 This is the regression the fifth source exists for, and it is not hypothetical: a
    /// successful takeover moves every node and folder that named the pool, so sources 2–4 all fall
    /// silent at once and the row disappears — with the "restore" affordance on it. The deployment
    /// would then be covering a pool nobody can stop covering.
    #[test]
    fn a_pool_nothing_else_names_survives_because_it_is_being_covered() {
        let covered: std::collections::BTreeMap<String, String> =
            [("tokyo".to_owned(), "default".to_owned())]
                .into_iter()
                .collect();
        // No described row, no node, no folder, no live poller — exactly the state a takeover
        // leaves behind.
        let opts = build_pool_options(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            &HashSet::new(),
            &covered,
        );
        let names: Vec<&str> = opts.iter().map(|o| o.name.as_str()).collect();
        assert!(
            names.contains(&"tokyo"),
            "a covered pool fell out of the list, taking its restore button with it: {names:?}"
        );
    }

    #[test]
    fn pool_options_union_default_first_and_flag_liveness() {
        let live: HashSet<String> = ["tokyo".to_owned(), "spare".to_owned()]
            .into_iter()
            .collect();
        let opts = build_pool_options(
            Vec::new(),
            // Duplicates within and across the two sources, plus blank/whitespace junk from rows
            // written before the API validated pool names.
            vec![
                "tokyo".to_owned(),
                "osaka".to_owned(),
                "tokyo".to_owned(),
                "  ".to_owned(),
            ],
            vec!["osaka".to_owned(), "  edge  ".to_owned()],
            &live,
            &no_cover(),
        );
        let names: Vec<&str> = opts.iter().map(|o| o.name.as_str()).collect();
        // Default always offered and always first (it is where an unassigned node lands, whether
        // or not anything references it); the rest alphabetical, deduped, trimmed.
        assert_eq!(names, vec!["default", "edge", "osaka", "spare", "tokyo"]);

        let live_of = |n: &str| opts.iter().find(|o| o.name == n).map(|o| o.live);
        // A pool only referenced by a live poller still appears (nothing is assigned to it yet).
        assert_eq!(live_of("spare"), Some(true));
        assert_eq!(live_of("tokyo"), Some(true));
        // Assigned but with no live poller — the picker must be able to warn about these.
        assert_eq!(live_of("osaka"), Some(false));
        assert_eq!(live_of("edge"), Some(false));
        assert_eq!(live_of("default"), Some(false));
    }

    #[test]
    fn pool_options_are_offered_even_with_nothing_configured() {
        let opts = build_pool_options(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            &HashSet::new(),
            &no_cover(),
        );
        assert_eq!(opts.len(), 1);
        assert_eq!(opts[0].name, yagra_bus::DEFAULT_POOL);
        assert!(!opts[0].live);
    }

    /// 🚨 **The regression this whole module is arranged to prevent.** A described pool must not
    /// replace the three sources that predate the table: an existing deployment has pools nobody has
    /// described, and dropping them from the picker is ADR-068's failure exactly.
    #[test]
    fn a_pool_nobody_described_still_appears() {
        let live: HashSet<String> = ["reported".to_owned()].into_iter().collect();
        let opts = build_pool_options(
            described(&[("named", Some("described on purpose"))]),
            vec!["from-a-node".to_owned()],
            vec!["from-a-folder".to_owned()],
            &live,
            &no_cover(),
        );
        let names: Vec<&str> = opts.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "default",
                "from-a-folder",
                "from-a-node",
                "named",
                "reported"
            ],
            "a source other than the pools table was dropped"
        );
        // And the description rides along only for the one that has one.
        let desc = |n: &str| {
            opts.iter()
                .find(|o| o.name == n)
                .and_then(|o| o.description.clone())
        };
        assert_eq!(desc("named").as_deref(), Some("described on purpose"));
        assert_eq!(desc("from-a-node"), None);
    }

    /// A row describing a pool that is also in use must appear once, not twice.
    #[test]
    fn describing_a_pool_already_in_use_does_not_duplicate_it() {
        let opts = build_pool_options(
            described(&[("tokyo", Some("東京拠点"))]),
            vec!["tokyo".to_owned()],
            Vec::new(),
            &HashSet::new(),
            &no_cover(),
        );
        let names: Vec<&str> = opts.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, vec!["default", "tokyo"]);
        assert_eq!(opts[1].description.as_deref(), Some("東京拠点"));
    }

    /// A blank description is not a description — otherwise the card renders an empty line where
    /// "説明なし" belongs.
    #[test]
    fn a_blank_description_is_treated_as_none() {
        let opts = build_pool_options(
            described(&[("a", Some("   ")), ("b", None)]),
            Vec::new(),
            Vec::new(),
            &HashSet::new(),
            &no_cover(),
        );
        assert!(opts.iter().all(|o| o.description.is_none()));
    }

    #[test]
    fn a_pool_name_must_be_one_subject_token() {
        // Accepted: the shapes an operator actually types.
        assert_eq!(validate_pool_name(" tokyo ").unwrap(), "tokyo");
        assert_eq!(validate_pool_name("edge-01_a").unwrap(), "edge-01_a");
        // Refused: a dot partitions `yagra.jobs.{pool}` and the jobs go where nothing subscribes.
        assert!(validate_pool_name("tokyo.1").is_err());
        assert!(validate_pool_name("east dc").is_err());
        assert!(validate_pool_name("").is_err());
        assert!(validate_pool_name("   ").is_err());
        assert!(validate_pool_name(&"x".repeat(64)).is_err());
        assert!(validate_pool_name(&"x".repeat(63)).is_ok());
    }

    #[test]
    fn a_description_of_whitespace_clears_rather_than_stores() {
        assert_eq!(clean_description(Some("  ".to_owned())), None);
        assert_eq!(clean_description(None), None);
        assert_eq!(
            clean_description(Some(" 東京拠点 ".to_owned())),
            Some("東京拠点".to_owned())
        );
    }
    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// A pool is created and listed.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn creating_a_pool_stores_it_and_lists_it(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let before = crate::pgtest::rows(&pool, "pools").await;
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/pools",
            &tok,
            Some(serde_json::json!({ "name": "edge", "description": "remote sites" })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        assert_eq!(crate::pgtest::rows(&pool, "pools").await, before + 1);

        let (status, list) = send(&st, "GET", "/api/v1/pools", &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{list}");
        assert!(list.to_string().contains("edge"), "{list}");
    }

    /// Covering a pool moves its members and putting them back is exact (ADR-107 増分 4).
    ///
    /// 🚨 The assertion that carries the increment is the **second** node's. A node that named the
    /// pool goes back to naming it, which a `pool_before_takeover` column could also have managed.
    /// A node that was *inheriting* has to go back to naming **nothing**, and a column cannot
    /// distinguish "was inheriting" from "was never taken over" — both are NULL. If the restore
    /// leaves an explicit pool on it, the deployment is not the one the takeover promised to be
    /// reversible about, and nothing else in the suite would notice.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn covering_a_pool_moves_its_members_and_restoring_puts_each_one_back(
        pool: sqlx::PgPool,
    ) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);

        // One node that names the pool, one that names nothing at all. Through the production
        // writer: `nodes.address` is `inet`, and a hand-rolled INSERT gets that wrong first.
        let named = crate::pgtest::node(&pool, "named", 1, None).await;
        let bare = crate::pgtest::node(&pool, "bare", 2, None).await;
        crate::pgtest::repo(pool.clone())
            .set_node_pool(named, Some("tokyo"))
            .await
            .expect("pin the first node to the pool");

        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/pools/tokyo/takeover",
            &tok,
            Some(serde_json::json!({ "to": "default" })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");

        let pool_of = |id: uuid::Uuid| {
            let pg = pool.clone();
            async move {
                sqlx::query_scalar::<_, Option<String>>("SELECT pool FROM nodes WHERE id = $1")
                    .bind(id)
                    .fetch_one(&pg)
                    .await
                    .expect("read node pool")
            }
        };
        assert_eq!(pool_of(named).await.as_deref(), Some("default"));
        // The pool is now flagged as covered, which is what the UI offers "restore" from.
        let (_, list) = send(&st, "GET", "/api/v1/pools", &tok, None).await;
        assert!(list.to_string().contains("covered_by"), "{list}");

        let (status, body) = send(&st, "POST", "/api/v1/pools/tokyo/restore", &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(pool_of(named).await.as_deref(), Some("tokyo"));
        // ⚠️ `bare` names no pool, so it falls through to the DEFAULT one and was never a member of
        // `tokyo`. Covering `tokyo` must not have touched it — an over-broad takeover that swept up
        // every unassigned node would look identical on the covered pool's own row. The
        // *inheriting* half of the round trip is tested where it can actually happen, in
        // `repo::pool_takeover` against the default pool: `PoolCarry::fall_through` is empty for
        // any other source, which this test learned by asserting the opposite and failing.
        assert_eq!(pool_of(bare).await, None, "a non-member was moved");
        assert_eq!(crate::pgtest::rows(&pool, "pool_takeover").await, 0);
    }
}
