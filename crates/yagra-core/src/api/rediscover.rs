// SPDX-License-Identifier: AGPL-3.0-only
//! Nodes ▸ Rediscover (ADR-186) — re-read one monitored node with its own credential, compare what
//! the device now says with what the node holds, and write only what a person accepts.
//!
//! Three routes, all `ManageConfig` and all addressed to one node by path (`NodeScoped`):
//!
//! - `POST …/rediscover` starts a one-address discovery sweep **to the node's own pool**. There is no
//!   global fallback: a poller on another network would report the device silent, and the dialog
//!   would say the credential does not work.
//! - `GET …/rediscover/{scan_id}` reads the comparison. It is keyed by node as well as scan, so one
//!   node's dialog can never show another's device.
//! - `POST …/rediscover/apply` re-judges what the person was shown and writes it in one guarded
//!   statement.
//!
//! The judgement is [`crate::rediscover`], which is pure; this file reads the stores, calls it, and
//! puts names on the ids. **REST only**: the two writes are outside MCP's frozen write surface
//! (ADR-042), and the read is a proposal for one of them.

use std::collections::HashMap;
use std::time::Instant;

use axum::{
    extract::Path,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yagra_common::NodeKind;

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, Leader, RequireManageConfig, Scoped, VisibleNode};
use super::scope::NodeScope;
use super::{AdminState, ApiState};
use crate::rediscover::{
    check_apply, judge, ApplyRequest, Change, Current, RediscoverState, RediscoverVerdict, Refusal,
    Row,
};
use crate::repo::RediscoverWrite;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(start_rediscovery, get_rediscovery, apply_rediscovery))]
pub(super) struct Doc;

/// The rediscovery routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/nodes/:node_id/rediscover", post(start_rediscovery))
        .route(
            "/api/v1/nodes/:node_id/rediscover/apply",
            post(apply_rediscovery),
        )
        .route(
            "/api/v1/nodes/:node_id/rediscover/:scan_id",
            get(get_rediscovery),
        )
}

/// A rediscovery accepted: read its comparison from `GET …/rediscover/{scan_id}`.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct StartedRediscovery {
    scan_id: Uuid,
    /// The pool whose pollers were asked — the node's own.
    pool: String,
}

/// What Nodes ▸ Rediscover shows.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct RediscoverView {
    scan_id: Uuid,
    /// `waiting` and `reading` say nothing about the device yet — never "nothing changed".
    state: RediscoverState,
    /// Present exactly when `state` is `answered`.
    #[serde(skip_serializing_if = "Option::is_none")]
    comparison: Option<RediscoverComparison>,
}

/// The node beside what the device now says it is.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct RediscoverComparison {
    /// What the device reported. Apply stores the first two with the accepted changes.
    sys_object_id: Option<String>,
    sys_descr: Option<String>,
    sys_name: Option<String>,
    /// A person fixed this node's profile (ADR-140); the profile row reads `locked` when it differs.
    profile_locked: bool,
    profile: RediscoverProfileRow,
    vendor: RediscoverTextRow,
    model: RediscoverTextRow,
}

/// The profile row. `found_id` is chosen by the classification rules from the device's
/// `sysObjectID`; with no `sysObjectID` no profile is proposed (`undetermined`).
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct RediscoverProfileRow {
    current_id: Option<Uuid>,
    current_name: Option<String>,
    found_id: Option<Uuid>,
    found_name: Option<String>,
    /// The rule that chose `found_id`; `null` ⇒ the device fell through to "Generic SNMP", or no
    /// profile was suggested at all (the device gave no sysObjectID).
    rule_id: Option<Uuid>,
    verdict: RediscoverVerdict,
}

/// A vendor or model row. `found` is `null` when nothing could be derived — Apply never blanks the
/// node's value.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct RediscoverTextRow {
    current: Option<String>,
    found: Option<String>,
    verdict: RediscoverVerdict,
}

impl From<Row<String>> for RediscoverTextRow {
    fn from(r: Row<String>) -> Self {
        Self {
            current: r.current,
            found: r.found,
            verdict: r.verdict,
        }
    }
}

/// One profile change, echoing what the dialog showed.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(super) struct ProfileChange {
    /// The profile the dialog showed as current; `null` ⇒ none.
    #[serde(default)]
    from: Option<Uuid>,
    to: Uuid,
}

/// One vendor or model change, echoing what the dialog showed.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(super) struct TextChange {
    /// The value the dialog showed as current; `null` ⇒ none.
    #[serde(default)]
    from: Option<String>,
    to: String,
}

/// The changes a person accepted. Name at least one.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(super) struct RediscoverApplyBody {
    /// The rediscovery the dialog showed.
    scan_id: Uuid,
    #[serde(default)]
    profile: Option<ProfileChange>,
    #[serde(default)]
    vendor: Option<TextChange>,
    #[serde(default)]
    model: Option<TextChange>,
}

/// Which fields an Apply wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RediscoverField {
    Profile,
    Vendor,
    Model,
}

/// What an Apply wrote.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct RediscoverApplied {
    applied: Vec<RediscoverField>,
}

/// The node as the comparison reads it, or 404 when it is not a device node the caller may see.
async fn current_of(admin: &AdminState, scope: &NodeScope, node_id: Uuid) -> ApiResult<Current> {
    let row = admin
        .repo
        .rediscover_current(node_id, scope.group_filter())
        .await
        .map_err(|e| ApiError::from_internal(e.as_ref(), "read node", "failed to read the node"))?
        .ok_or_else(|| ApiError::not_found("node_not_found", "no such device node"))?;
    Ok(Current {
        profile_id: row.profile_id,
        profile_locked: row.profile_locked,
        vendor: row.vendor,
        model: row.model,
    })
}

fn scan_not_found() -> ApiError {
    ApiError::not_found(
        "scan_not_found",
        "no such rediscovery for this node — it finished more than six hours ago, this core \
         restarted, or it belongs to another node",
    )
}

#[utoipa::path(
    post, path = "/api/v1/nodes/{node_id}/rediscover", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    responses(
        (status = 202, description = "Re-read started on the node's own pool; read the comparison by scan id", body = StartedRediscovery),
        (status = 400, description = "The node's bound credential is missing or unusable", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such node, or not one the caller can see", body = super::error::ErrorBody),
        (status = 409, description = "`not_a_device`: a URL, DNS, Meraki or controller-managed AP node, which is not read over SNMP. `no_snmp_credential`: the node has no credential and the deployment no fallback community. `no_live_poller`: no poller of the node's pool is alive — the re-read is never sent from another network", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side, or this core is not the HA leader", body = super::error::ErrorBody),
    ),
)]
async fn start_rediscovery(
    _perm: RequireManageConfig,
    _visible: VisibleNode,
    admin: Admin,
    // Leader-gated for the range scan's reason: only the leader consumes discovery results, so a
    // standby would send real SNMP at the device and never see the answer.
    _leader: Leader,
    Path(node_id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<StartedRediscovery>)> {
    let node = admin
        .repo
        .get_node(node_id)
        .await
        .map_err(|e| ApiError::from_internal(e.as_ref(), "read node", "failed to read the node"))?
        .ok_or_else(|| ApiError::not_found("node_not_found", "no such node"))?;
    let kind = NodeKind::resolve(super::checks::current_node_rows(&admin, node_id).await?);
    if kind != NodeKind::Device {
        return Err(ApiError::conflict(
            "not_a_device",
            format!(
                "a {} node is not read over SNMP, so there is nothing to rediscover",
                kind.display_name()
            ),
        ));
    }
    // The credential its polls use: the bound one, else the deployment's fallback community
    // (`scheduler::dispatch::resolve_snmp_auth`'s order).
    let (communities, credentials) = match node.credential {
        Some(cred) => (
            Vec::new(),
            super::discovery::resolve_scan_credentials(&admin.creds, &[cred.as_uuid().to_string()])
                .await?,
        ),
        None => match admin.dispatcher.fallback_community() {
            Some(c) => (vec![c.to_owned()], Vec::new()),
            None => {
                return Err(ApiError::conflict(
                    "no_snmp_credential",
                    "the node has no SNMP credential and the deployment sets no fallback \
                     community — bind one in Edit node first",
                ))
            }
        },
    };
    let pool = super::util::pool_resolver_or_error(&admin)
        .await?
        .resolve_pool(&node)
        .to_owned();
    if !admin.coordinator.live_pools(Instant::now()).contains(&pool) {
        return Err(ApiError::conflict(
            "no_live_poller",
            format!("no poller of pool '{pool}' is alive to read the device"),
        ));
    }
    let scan_id = admin
        .discovery
        .start_rediscovery(node_id, node.address, communities, credentials, &pool)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "start rediscovery",
                "failed to start the re-read",
            )
        })?;
    Ok((
        StatusCode::ACCEPTED,
        Json(StartedRediscovery { scan_id, pool }),
    ))
}

#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/rediscover/{scan_id}", tag = "nodes",
    params(
        ("node_id" = Uuid, Path, description = "Node id"),
        ("scan_id" = Uuid, Path, description = "The id `POST …/rediscover` returned"),
    ),
    responses(
        (status = 200, description = "Where the re-read is, and — once the device answered — the node beside what it now says", body = RediscoverView),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such device node the caller can see, or (`scan_not_found`) no such rediscovery of this node on this core", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn get_rediscovery(
    _perm: RequireManageConfig,
    _visible: VisibleNode,
    Scoped(scope): Scoped,
    admin: Admin,
    Path((node_id, scan_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<RediscoverView>> {
    let current = current_of(&admin, &scope, node_id).await?;
    let status = admin
        .discovery
        .get_rediscovery(scan_id, node_id)
        .ok_or_else(scan_not_found)?;
    let judgement = judge(&current, &status, &admin.classifier);
    let comparison = match judgement.comparison {
        None => None,
        Some(c) => {
            let names: HashMap<Uuid, String> = admin
                .repo
                .list_profiles()
                .await
                .map_err(|e| {
                    ApiError::from_internal(e.as_ref(), "list profiles", "failed to read profiles")
                })?
                .into_iter()
                .map(|p| (p.id, p.name))
                .collect();
            let name = |id: Option<Uuid>| id.and_then(|id| names.get(&id).cloned());
            Some(RediscoverComparison {
                sys_object_id: c.found.sys_object_id,
                sys_descr: c.found.sys_descr,
                sys_name: c.found.sys_name,
                profile_locked: current.profile_locked,
                profile: RediscoverProfileRow {
                    current_id: c.profile.current,
                    current_name: name(c.profile.current),
                    found_id: c.profile.found,
                    found_name: name(c.profile.found),
                    rule_id: c.rule_id,
                    verdict: c.profile.verdict,
                },
                vendor: c.vendor.into(),
                model: c.model.into(),
            })
        }
    };
    Ok(Json(RediscoverView {
        scan_id,
        state: judgement.state,
        comparison,
    }))
}

#[utoipa::path(
    post, path = "/api/v1/nodes/{node_id}/rediscover/apply", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    request_body = RediscoverApplyBody,
    responses(
        (status = 200, description = "The accepted changes were written, with the device's sysObjectID and sysDescr", body = RediscoverApplied),
        (status = 400, description = "`nothing_to_apply`: the body names no field", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such device node the caller can see, or (`scan_not_found`) no such rediscovery of this node on this core", body = super::error::ErrorBody),
        (status = 409, description = "Nothing was written. `not_answered`: the device has not answered SNMP. `profile_locked`: a person locked the profile. `node_changed`: the node no longer holds what the dialog showed. `judgement_changed`: the rules no longer derive what the dialog showed", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn apply_rediscovery(
    _perm: RequireManageConfig,
    _visible: VisibleNode,
    Scoped(scope): Scoped,
    admin: Admin,
    Path(node_id): Path<Uuid>,
    Json(body): Json<RediscoverApplyBody>,
) -> ApiResult<Json<RediscoverApplied>> {
    let request = ApplyRequest {
        profile: body.profile.map(|c| Change {
            from: c.from,
            to: c.to,
        }),
        vendor: body.vendor.map(|c| Change {
            from: c.from,
            to: c.to,
        }),
        model: body.model.map(|c| Change {
            from: c.from,
            to: c.to,
        }),
    };
    if request.profile.is_none() && request.vendor.is_none() && request.model.is_none() {
        return Err(ApiError::bad_request(
            "nothing_to_apply",
            "name at least one of profile, vendor and model",
        ));
    }
    let current = current_of(&admin, &scope, node_id).await?;
    let status = admin
        .discovery
        .get_rediscovery(body.scan_id, node_id)
        .ok_or_else(scan_not_found)?;
    let plan =
        check_apply(&current, &status, &admin.classifier, &request).map_err(|r| match r {
            Refusal::NothingToApply => ApiError::bad_request(
                "nothing_to_apply",
                "name at least one of profile, vendor and model",
            ),
            Refusal::NotAnswered => ApiError::conflict(
                "not_answered",
                "the device has not answered SNMP, so there is nothing to apply",
            ),
            Refusal::Locked => ApiError::conflict(
                "profile_locked",
                "the node's profile is locked; unlock it in Edit node first",
            ),
            Refusal::NodeChanged => ApiError::conflict(
                "node_changed",
                "the node changed since the comparison was read — read it again",
            ),
            Refusal::JudgementChanged => ApiError::conflict(
                "judgement_changed",
                "the classification no longer derives what was shown — read it again",
            ),
        })?;
    let mut applied = Vec::new();
    if plan.profile.is_some() {
        applied.push(RediscoverField::Profile);
    }
    if plan.vendor.is_some() {
        applied.push(RediscoverField::Vendor);
    }
    if plan.model.is_some() {
        applied.push(RediscoverField::Model);
    }
    let write = RediscoverWrite {
        profile: plan.profile.map(|c| (c.from, c.to)),
        vendor: plan.vendor.map(|c| (c.from, c.to)),
        model: plan.model.map(|c| (c.from, c.to)),
        sys_object_id: plan.sys_object_id,
        sys_descr: plan.sys_descr,
    };
    let written = admin
        .repo
        .apply_rediscovery(node_id, &write, scope.group_filter())
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "apply rediscovery", "failed to update the node")
        })?;
    // The guard refused: a person edited the node between the check above and the write.
    if !written {
        return Err(ApiError::conflict(
            "node_changed",
            "the node changed since the comparison was read — read it again",
        ));
    }
    tracing::info!(node = %node_id, fields = ?applied, "rediscovery applied");
    Ok(Json(RediscoverApplied { applied }))
}

#[cfg(test)]
mod tests {
    use crate::api::tests_support::{live_state, scoped_token, send, token};
    use axum::http::StatusCode;
    use serde_json::json;
    use uuid::Uuid;
    use yagra_bus::{DiscoveredDevice, DiscoveryResult};
    use yagra_common::Role;

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

    /// A poller of `pool` that has just said hello, so the start is not refused for want of one.
    async fn a_live_poller(st: &crate::api::ApiState, pool: &str) {
        st.admin
            .as_ref()
            .expect("live")
            .coordinator
            .observe_heartbeat(
                yagra_bus::HeartbeatMsg {
                    poller_id: "p-1".to_owned(),
                    pool: pool.to_owned(),
                    incarnation: Uuid::new_v4(),
                    version: "0.3.36".to_owned(),
                    epoch: None,
                    last_seq: 0,
                    working_set_nodes: 0,
                    working_set_specs: 0,
                    inflight: 0,
                    results_total: 0,
                    listeners: Vec::new(),
                    caps: Vec::new(),
                    host: None,
                    leaving: false,
                    mgmt_addrs: Vec::new(),
                    upgrade: None,
                },
                std::time::Instant::now(),
            )
            .await;
    }

    /// The poller's answer for the scan, as the consumer would fold it.
    fn answer(st: &crate::api::ApiState, scan_id: Uuid, oid: Option<&str>, descr: Option<&str>) {
        st.admin
            .as_ref()
            .expect("live")
            .discovery
            .fold(DiscoveryResult {
                scan_id,
                found: vec![DiscoveredDevice {
                    address: "192.0.2.10".parse().expect("ip"),
                    reachable: true,
                    sysdescr: descr.map(str::to_owned),
                    sysname: Some("sw-01".to_owned()),
                    sysobjectid: oid.map(str::to_owned),
                    matched_credential: None,
                }],
                probed: 1,
                total: 1,
                done: true,
                cancelled: false,
            });
    }

    async fn started(st: &crate::api::ApiState, tok: &str, node: Uuid) -> Uuid {
        let (status, body) = send(
            st,
            "POST",
            &format!("/api/v1/nodes/{node}/rediscover"),
            tok,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        body["scan_id"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .expect("scan_id")
    }

    /// The whole round trip, **accepted** (ADR-115): a start is 202 and waits, the answer is
    /// compared, Apply writes what was shown — and the row carries it afterwards.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_rediscovery_is_started_compared_and_applied(pool: sqlx::PgPool) {
        let st = loaded_state(&pool).await;
        let tok = token(&st, Role::Admin);
        let node = crate::pgtest::node(&pool, "sw-01", 10, None).await;
        let cred = crate::pgtest::credential(&pool, "lab-v2c", "snmp_v2c").await;
        sqlx::query("UPDATE nodes SET credential_id = $2, address = '192.0.2.10' WHERE id = $1")
            .bind(node)
            .bind(cred)
            .execute(&pool)
            .await
            .expect("bind");

        let (status, body) = send(
            &st,
            "POST",
            &format!("/api/v1/nodes/{node}/rediscover"),
            &tok,
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "no live poller, so no re-read from anywhere else: {body}"
        );
        assert_eq!(body["error"]["code"], "no_live_poller", "{body}");

        a_live_poller(&st, "default").await;
        let scan = started(&st, &tok, node).await;
        let path = format!("/api/v1/nodes/{node}/rediscover/{scan}");
        let (status, view) = send(&st, "GET", &path, &tok, None).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["state"], "waiting", "{view}");
        assert!(view.get("comparison").is_none(), "waiting is not an answer");

        answer(
            &st,
            scan,
            Some("1.3.6.1.4.1.2011.2.23.1"),
            Some("Huawei Versatile Routing Platform Software"),
        );
        let (status, view) = send(&st, "GET", &path, &tok, None).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["state"], "answered", "{view}");
        let cmp = &view["comparison"];
        assert_eq!(cmp["profile"]["verdict"], "differs", "{view}");
        assert_eq!(cmp["vendor"]["found"], "Huawei", "{view}");
        let to_profile = cmp["profile"]["found_id"].clone();
        assert!(
            cmp["profile"]["found_name"]
                .as_str()
                .is_some_and(|n| n.starts_with("Huawei")),
            "the Huawei rule chose the profile: {view}"
        );
        let huawei: Uuid = to_profile
            .as_str()
            .and_then(|s| s.parse().ok())
            .expect("found_id");

        let body = json!({
            "scan_id": scan,
            "profile": { "from": cmp["profile"]["current_id"], "to": to_profile },
            "vendor": { "from": cmp["vendor"]["current"], "to": "Huawei" },
        });
        let (status, out) = send(
            &st,
            "POST",
            &format!("/api/v1/nodes/{node}/rediscover/apply"),
            &tok,
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{out}");
        assert_eq!(out["applied"], json!(["profile", "vendor"]), "{out}");
        let (profile, vendor, oid): (Option<Uuid>, Option<String>, Option<String>) =
            sqlx::query_as("SELECT profile_id, vendor, sys_object_id FROM nodes WHERE id = $1")
                .bind(node)
                .fetch_one(&pool)
                .await
                .expect("row");
        assert_eq!(profile, Some(huawei));
        assert_eq!(vendor.as_deref(), Some("Huawei"));
        assert_eq!(oid.as_deref(), Some("1.3.6.1.4.1.2011.2.23.1"));

        // The same body again: the node no longer holds what that dialog showed.
        let (status, out) = send(
            &st,
            "POST",
            &format!("/api/v1/nodes/{node}/rediscover/apply"),
            &tok,
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{out}");
        assert_eq!(out["error"]["code"], "node_changed", "{out}");
    }

    /// A locked profile is shown as locked and Apply refuses to move it; another node's id cannot
    /// read this scan; a viewer and an out-of-scope caller are refused.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_locked_profile_another_nodes_scan_and_other_callers_are_refused(pool: sqlx::PgPool) {
        let st = loaded_state(&pool).await;
        let tok = token(&st, Role::Admin);
        let mine = crate::pgtest::group(&pool, "mine").await;
        let theirs = crate::pgtest::group(&pool, "theirs").await;
        let node = crate::pgtest::node(&pool, "sw-01", 10, Some(mine)).await;
        let other = crate::pgtest::node(&pool, "sw-02", 11, Some(mine)).await;
        let cred = crate::pgtest::credential(&pool, "lab-v2c", "snmp_v2c").await;
        sqlx::query(
            "UPDATE nodes SET credential_id = $2, profile_locked = true WHERE id = ANY($1)",
        )
        .bind(vec![node, other])
        .bind(cred)
        .execute(&pool)
        .await
        .expect("bind and lock");
        a_live_poller(&st, "default").await;
        let scan = started(&st, &tok, node).await;
        answer(
            &st,
            scan,
            Some("1.3.6.1.4.1.2011.2.23.1"),
            Some("Huawei VRP"),
        );

        let (status, view) = send(
            &st,
            "GET",
            &format!("/api/v1/nodes/{node}/rediscover/{scan}"),
            &tok,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["comparison"]["profile"]["verdict"], "locked", "{view}");
        let (status, out) = send(
            &st,
            "POST",
            &format!("/api/v1/nodes/{node}/rediscover/apply"),
            &tok,
            Some(json!({
                "scan_id": scan,
                "profile": {
                    "from": view["comparison"]["profile"]["current_id"],
                    "to": view["comparison"]["profile"]["found_id"],
                },
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{out}");
        assert_eq!(out["error"]["code"], "profile_locked", "{out}");

        let (status, _) = send(
            &st,
            "GET",
            &format!("/api/v1/nodes/{other}/rediscover/{scan}"),
            &tok,
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "one node's scan is never another node's comparison"
        );

        let viewer = token(&st, Role::Viewer);
        let (status, _) = send(
            &st,
            "POST",
            &format!("/api/v1/nodes/{node}/rediscover"),
            &viewer,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = send(
            &st,
            "POST",
            &format!("/api/v1/nodes/{node}/rediscover"),
            &scoped_token(&st, &[theirs]),
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "outside the caller's folders"
        );
    }

    /// A node with no credential on a deployment with no fallback community has nothing to re-read
    /// with, and a URL monitor is not a device.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_node_with_nothing_to_read_with_or_no_snmp_at_all_is_refused(pool: sqlx::PgPool) {
        let st = loaded_state(&pool).await;
        let tok = token(&st, Role::Admin);
        a_live_poller(&st, "default").await;
        let bare = crate::pgtest::node(&pool, "host-01", 12, None).await;
        let (status, body) = send(
            &st,
            "POST",
            &format!("/api/v1/nodes/{bare}/rediscover"),
            &tok,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"]["code"], "no_snmp_credential", "{body}");

        sqlx::query("INSERT INTO url_checks (node_id, url) VALUES ($1, 'https://example.com/')")
            .bind(bare)
            .execute(&pool)
            .await
            .expect("url check");
        let (status, body) = send(
            &st,
            "POST",
            &format!("/api/v1/nodes/{bare}/rediscover"),
            &tok,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(body["error"]["code"], "not_a_device", "{body}");
    }
}
