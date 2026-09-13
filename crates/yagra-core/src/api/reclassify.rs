// SPDX-License-Identifier: AGPL-3.0-only
//! Nodes ▸ Reclassify (ADR-140) — the device nodes whose profile differs from the one the current
//! classification rules choose, and the two writes that act on them.
//!
//! `ManageConfig` throughout, like the rules themselves: a node's profile decides what it is polled
//! for. **Group-filtered throughout**, because every answer here is about nodes — a scoped caller
//! sees and changes only the nodes in their own folders, and a request naming any other node is
//! counted as hidden, the same answer a node that does not exist gets.
//!
//! The judgement lives in [`crate::reclassify`], which is pure; this file reads the stores, calls it,
//! and puts names on the ids. [`reclassify_view`] is the seam the MCP `get_config(kind="reclassify")`
//! branch shares, so the two surfaces cannot answer differently (ADR-042 read parity).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use axum::{
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig, Scoped};
use super::scope::NodeScope;
use super::{AdminState, ApiState};
use crate::reclassify::{check_request, propose, Skip, PROPOSAL_LIMIT};
use crate::repo::{ReclassifyInput, ReclassifyWrite};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(get_reclassify, apply_reclassify, lock_reclassify))]
pub(super) struct Doc;

/// The reclassification routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/reclassify", get(get_reclassify))
        .route("/api/v1/reclassify/apply", post(apply_reclassify))
        .route("/api/v1/reclassify/lock", post(lock_reclassify))
}

/// What Nodes ▸ Reclassify shows.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct ReclassifyView {
    /// Unlocked device nodes whose profile differs from the one the classification rules choose,
    /// ordered by node name. At most 5,000 — `total` says how many there are.
    proposals: Vec<ReclassifyProposal>,
    /// How many unlocked device nodes differ.
    total: usize,
    /// Locked nodes the rules would move. Counted and never listed: a person fixed their profile.
    /// A lock is cleared from the node's edit dialog.
    locked: usize,
    /// Device nodes the rules were run for, whatever they chose. With `proposals` empty, `0` here
    /// means nothing has been compared yet — not that every node matches.
    identified: usize,
    /// Device nodes the rules cannot be run for yet, because no `sysObjectID` is stored for them.
    /// The poller reads it on its hourly identity probe, so a new or just-upgraded deployment fills
    /// this in within the hour; a node that is not SNMP-polled never has one.
    unidentified: usize,
}

/// One node the rules would move.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct ReclassifyProposal {
    node_id: Uuid,
    node_name: String,
    /// `null` ⇒ the node has no profile.
    current_profile_id: Option<Uuid>,
    current_profile_name: Option<String>,
    suggested_profile_id: Uuid,
    suggested_profile_name: Option<String>,
    /// The rule that chose the profile; `null` ⇒ no rule matched and the device fell through to
    /// "Generic SNMP".
    rule: Option<MatchedRule>,
    /// The vendor and model applying writes onto the node; `null` leaves the node's own.
    vendor: Option<String>,
    model: Option<String>,
    /// What the device last said it is — the input the rules ran on.
    sys_object_id: String,
    sys_descr: Option<String>,
}

/// The rule behind a proposal, as the classification-rules screen shows it.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct MatchedRule {
    id: Uuid,
    priority: i32,
    sysobjectid_prefix: Option<String>,
    sysdescr_regex: Option<String>,
}

/// The proposals the caller may see — the seam REST and MCP share.
pub(crate) async fn reclassify_view(
    admin: &AdminState,
    scope: &NodeScope,
) -> ApiResult<ReclassifyView> {
    let nodes = read_inputs(admin, scope).await?;
    let set = propose(&nodes, &admin.classifier, PROPOSAL_LIMIT);
    let profiles: HashMap<Uuid, String> = admin
        .repo
        .list_profiles()
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "list profiles", "failed to read profiles")
        })?
        .into_iter()
        .map(|p| (p.id, p.name))
        .collect();
    let rules: HashMap<Uuid, MatchedRule> = admin
        .classification
        .list_rules()
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "list classification rules",
                "failed to read classification rules",
            )
        })?
        .into_iter()
        .map(|r| {
            (
                r.id,
                MatchedRule {
                    id: r.id,
                    priority: r.priority,
                    sysobjectid_prefix: r.sysobjectid_prefix,
                    sysdescr_regex: r.sysdescr_regex,
                },
            )
        })
        .collect();
    let proposals = set
        .proposals
        .into_iter()
        .map(|p| ReclassifyProposal {
            node_id: p.node.id,
            node_name: p.node.name.clone(),
            current_profile_id: p.node.profile_id,
            current_profile_name: p.node.profile_id.and_then(|id| profiles.get(&id).cloned()),
            suggested_profile_id: p.suggestion.profile_id,
            suggested_profile_name: profiles.get(&p.suggestion.profile_id).cloned(),
            rule: p.suggestion.rule_id.and_then(|id| rules.get(&id).cloned()),
            vendor: p.suggestion.vendor,
            model: p.suggestion.model,
            sys_object_id: p.node.sys_object_id.clone().unwrap_or_default(),
            sys_descr: p.node.sys_descr.clone(),
        })
        .collect();
    Ok(ReclassifyView {
        proposals,
        total: set.total,
        locked: set.locked,
        identified: set.identified,
        unidentified: set.unidentified,
    })
}

async fn read_inputs(admin: &AdminState, scope: &NodeScope) -> ApiResult<Vec<ReclassifyInput>> {
    admin
        .repo
        .reclassify_inputs(scope.group_filter())
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "read reclassify inputs",
                "failed to read the inventory",
            )
        })
}

#[utoipa::path(
    get, path = "/api/v1/reclassify", tag = "classification",
    responses(
        (status = 200, description = "The device nodes whose profile differs from the one the classification rules choose, with counts of locked and not-yet-identified nodes", body = ReclassifyView),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn get_reclassify(
    _perm: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
) -> ApiResult<Json<ReclassifyView>> {
    reclassify_view(&admin, &scope).await.map(Json)
}

/// One change to make, echoing what the screen showed.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(super) struct ReclassifyItem {
    node_id: Uuid,
    /// The profile the screen showed the node on; `null` ⇒ it had none. The change is skipped if the
    /// node has moved since.
    #[serde(default)]
    from_profile_id: Option<Uuid>,
    /// The profile the screen proposed. The change is skipped if the rules no longer choose it.
    to_profile_id: Uuid,
}

/// The changes to make.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(super) struct ReclassifyApplyBody {
    /// At most 5,000. A node named twice counts once, and its last entry is the one used.
    items: Vec<ReclassifyItem>,
}

/// What an apply did. The four counts add up to the number of distinct nodes named.
#[derive(Debug, Default, Serialize, utoipa::ToSchema)]
pub(crate) struct ReclassifyApplied {
    /// Nodes moved to the profile the rules choose.
    applied: usize,
    /// Nodes no longer on the profile the screen showed, or for which the rules no longer choose
    /// the profile the screen proposed.
    skipped_changed: usize,
    /// Nodes someone locked.
    skipped_locked: usize,
    /// Nodes the caller may not see, that are not device nodes, or that do not exist.
    skipped_hidden: usize,
}

fn too_many(what: &str) -> ApiError {
    ApiError::bad_request(
        "too_many_nodes",
        format!("at most {PROPOSAL_LIMIT} {what} per request"),
    )
}

#[utoipa::path(
    post, path = "/api/v1/reclassify/apply", tag = "classification",
    request_body = ReclassifyApplyBody,
    responses(
        (status = 200, description = "How many nodes moved and why the rest did not", body = ReclassifyApplied),
        (status = 400, description = "More than 5,000 items", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn apply_reclassify(
    _perm: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<ReclassifyApplyBody>,
) -> ApiResult<Json<ReclassifyApplied>> {
    if body.items.len() > PROPOSAL_LIMIT {
        return Err(too_many("items"));
    }
    let wanted: BTreeMap<Uuid, &ReclassifyItem> =
        body.items.iter().map(|item| (item.node_id, item)).collect();
    let nodes = read_inputs(&admin, &scope).await?;
    let by_id: HashMap<Uuid, &ReclassifyInput> = nodes.iter().map(|n| (n.id, n)).collect();

    let mut out = ReclassifyApplied::default();
    let mut writes = Vec::new();
    for item in wanted.values() {
        match check_request(
            by_id.get(&item.node_id).copied(),
            item.from_profile_id,
            item.to_profile_id,
            &admin.classifier,
        ) {
            Ok(suggestion) => writes.push(ReclassifyWrite {
                node: item.node_id,
                from: item.from_profile_id,
                to: item.to_profile_id,
                vendor: suggestion.vendor,
                model: suggestion.model,
            }),
            Err(Skip::Hidden) => out.skipped_hidden += 1,
            Err(Skip::Locked) => out.skipped_locked += 1,
            Err(Skip::Changed) => out.skipped_changed += 1,
        }
    }
    let moved = admin
        .repo
        .apply_reclassification(&writes, scope.group_filter())
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "apply reclassification",
                "failed to update nodes",
            )
        })?;
    // A write the guard refused lost a race with a person since the check above — the same answer
    // as a change detected there.
    out.applied = moved.len();
    out.skipped_changed += writes.len() - moved.len();
    Ok(Json(out))
}

/// Lock or unlock the profile of the named nodes.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(super) struct ReclassifyLockBody {
    /// At most 5,000.
    node_ids: Vec<Uuid>,
    /// `true` fixes each node's profile against reclassification; `false` releases it.
    locked: bool,
}

/// What a lock did. The two counts add up to the number of distinct nodes named.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct ReclassifyLocked {
    updated: usize,
    /// Nodes the caller may not see, that are not device nodes, or that do not exist.
    skipped_hidden: usize,
}

#[utoipa::path(
    post, path = "/api/v1/reclassify/lock", tag = "classification",
    request_body = ReclassifyLockBody,
    responses(
        (status = 200, description = "How many nodes were locked or unlocked", body = ReclassifyLocked),
        (status = 400, description = "More than 5,000 node ids", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn lock_reclassify(
    _perm: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<ReclassifyLockBody>,
) -> ApiResult<Json<ReclassifyLocked>> {
    if body.node_ids.len() > PROPOSAL_LIMIT {
        return Err(too_many("node ids"));
    }
    let ids: Vec<Uuid> = body
        .node_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let written = admin
        .repo
        .set_profile_locks(&ids, body.locked, scope.group_filter())
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "set profile locks", "failed to update nodes")
        })?;
    Ok(Json(ReclassifyLocked {
        updated: written.len(),
        skipped_hidden: ids.len() - written.len(),
    }))
}

#[cfg(test)]
mod tests {
    use crate::api::tests_support::{live_state, send, token};
    use crate::seed_ids::SeedRange;
    use axum::http::StatusCode;
    use serde_json::json;
    use uuid::Uuid;
    use yagra_common::Role;

    fn profile(name: &str) -> Uuid {
        let i = yagra_common::builtin_profiles()
            .iter()
            .position(|p| p.name == name)
            .unwrap_or_else(|| panic!("no built-in profile {name}"));
        SeedRange::Profiles.id(i)
    }

    /// An Alcatel OmniSwitch on "Nokia SR router", identified the way the poll path identifies it.
    async fn misfiled_omniswitch(pool: &sqlx::PgPool) -> Uuid {
        let id = crate::pgtest::node(pool, "ale-sw01", 1, None).await;
        sqlx::query("UPDATE nodes SET profile_id = $2 WHERE id = $1")
            .bind(id)
            .bind(profile("Nokia SR router"))
            .execute(pool)
            .await
            .expect("the old rule's profile");
        crate::pgtest::repo(pool.clone())
            .update_snmp_identity_batch(&[(
                id,
                Some("1.3.6.1.4.1.6486.801.1.1.2.1.11.1.9".to_owned()),
                Some("Alcatel-Lucent Enterprise OS6860E-U28 8.5.255.R02 GA".to_owned()),
            )])
            .await
            .expect("identity");
        id
    }

    async fn loaded_state(pool: &sqlx::PgPool) -> crate::api::ApiState {
        let st = live_state(pool.clone()).await;
        let admin = st.admin.as_ref().expect("live mode");
        admin
            .classifier
            .reload(&admin.classification)
            .await
            .expect("load the built-in rules");
        st
    }

    /// The whole round trip, **accepted** (ADR-115): the misfiled node is proposed with the rule that
    /// chose it, an apply moves it and takes the rule's vendor, and the next read no longer lists it.
    ///
    /// 🚨 The last read is the half that proves the write landed where the read looks. An apply that
    /// answered `applied: 1` and wrote nothing would pass every assertion before it.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_proposal_is_listed_applied_and_then_gone(pool: sqlx::PgPool) {
        let st = loaded_state(&pool).await;
        let tok = token(&st, Role::Admin);
        let id = misfiled_omniswitch(&pool).await;

        let (status, view) = send(&st, "GET", "/api/v1/reclassify", &tok, None).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["total"], 1, "{view}");
        assert_eq!(view["identified"], 1, "{view}");
        let p = &view["proposals"][0];
        assert_eq!(p["node_id"], id.to_string());
        assert_eq!(p["current_profile_name"], "Nokia SR router");
        assert_eq!(p["suggested_profile_name"], "Alcatel-Lucent OmniSwitch");
        assert_eq!(p["rule"]["sysobjectid_prefix"], "1.3.6.1.4.1.6486.");

        let (status, out) = send(
            &st,
            "POST",
            "/api/v1/reclassify/apply",
            &tok,
            Some(json!({ "items": [{
                "node_id": id,
                "from_profile_id": profile("Nokia SR router"),
                "to_profile_id": profile("Alcatel-Lucent OmniSwitch"),
            }]})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{out}");
        assert_eq!(
            out,
            json!({"applied": 1, "skipped_changed": 0, "skipped_locked": 0, "skipped_hidden": 0})
        );

        let (_, view) = send(&st, "GET", "/api/v1/reclassify", &tok, None).await;
        assert_eq!(view["total"], 0, "{view}");
        let (_, detail) = send(&st, "GET", &format!("/api/v1/nodes/{id}"), &tok, None).await;
        assert_eq!(
            detail["profile_id"],
            profile("Alcatel-Lucent OmniSwitch").to_string()
        );
        assert_eq!(detail["vendor"], "Alcatel-Lucent");
    }

    /// A lock is **accepted**, the locked node is counted rather than listed, and an apply that names
    /// it anyway is skipped as locked rather than applied over the lock.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_locked_node_is_counted_not_listed_and_not_moved(pool: sqlx::PgPool) {
        let st = loaded_state(&pool).await;
        let tok = token(&st, Role::Admin);
        let id = misfiled_omniswitch(&pool).await;
        let stranger = Uuid::new_v4();

        let (status, out) = send(
            &st,
            "POST",
            "/api/v1/reclassify/lock",
            &tok,
            Some(json!({ "node_ids": [id, id, stranger], "locked": true })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{out}");
        assert_eq!(out, json!({"updated": 1, "skipped_hidden": 1}));

        let (_, view) = send(&st, "GET", "/api/v1/reclassify", &tok, None).await;
        assert_eq!(
            (view["total"].clone(), view["locked"].clone()),
            (json!(0), json!(1))
        );

        let (status, out) = send(
            &st,
            "POST",
            "/api/v1/reclassify/apply",
            &tok,
            Some(json!({ "items": [{
                "node_id": id,
                "from_profile_id": profile("Nokia SR router"),
                "to_profile_id": profile("Alcatel-Lucent OmniSwitch"),
            }]})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{out}");
        assert_eq!(out["applied"], 0);
        assert_eq!(out["skipped_locked"], 1);
        let (_, detail) = send(&st, "GET", &format!("/api/v1/nodes/{id}"), &tok, None).await;
        assert_eq!(detail["profile_id"], profile("Nokia SR router").to_string());
    }

    /// A Viewer is refused all three with 403 — and, since refusals alone prove nothing, the two
    /// tests above are the accepted half.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_viewer_can_neither_read_nor_change_reclassification(pool: sqlx::PgPool) {
        let st = loaded_state(&pool).await;
        let tok = token(&st, Role::Viewer);
        for (method, path, body) in [
            ("GET", "/api/v1/reclassify", None),
            (
                "POST",
                "/api/v1/reclassify/apply",
                Some(json!({ "items": [] })),
            ),
            (
                "POST",
                "/api/v1/reclassify/lock",
                Some(json!({ "node_ids": [], "locked": true })),
            ),
        ] {
            let (status, _) = send(&st, method, path, &tok, body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
        }
    }
}
