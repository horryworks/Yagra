// SPDX-License-Identifier: AGPL-3.0-only
//! Nodes ▸ Duplicates (ADR-148) — device nodes that look like one physical device registered more
//! than once, with the evidence for each group.
//!
//! **One read and no write.** The cleanup is the existing bulk delete (`POST /api/v1/nodes/delete`,
//! ADR-124 増分 6), so this domain owes no accepted-write test of its own. `ManageConfig`, like that
//! delete, because the list exists to be acted on. **Group-filtered**: the candidates are the
//! caller's own device nodes, so a group never names a node outside their folders (ADR-148 決定 8).
//!
//! The judgement is [`crate::duplicates`], which is pure; this file reads the stores it weighs and
//! puts names on the ids. [`duplicates_view`] is the seam the MCP `get_config(kind="duplicate_nodes")`
//! branch shares, so the two surfaces cannot answer differently (ADR-042 read parity).

use std::collections::{BTreeSet, HashMap};

use axum::{routing::get, Json, Router};
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig, Scoped};
use super::scope::NodeScope;
use super::{AdminState, ApiState};
use crate::duplicates::{
    self, DuplicateConfidence, DuplicateContradiction, DuplicateEvidenceKind, Observations,
    OwnAddress, PeerReport,
};
use crate::repo::DuplicateInput;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(get_duplicate_nodes))]
pub(super) struct Doc;

/// The duplicates route, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new().route("/api/v1/nodes/duplicates", get(get_duplicate_nodes))
}

/// What Nodes ▸ Duplicates shows.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct DuplicateNodesView {
    /// Groups of device nodes that look like one physical device. Confident groups first, at most
    /// 500 — `total` says how many there are.
    groups: Vec<DuplicateGroup>,
    /// How many groups there are.
    total: usize,
    /// Values not used as evidence because more than 8 of the compared nodes share them (a shared
    /// monitored address always counts). The most widely shared first, at most 200.
    ignored: Vec<DuplicateIgnoredValue>,
    /// How many values were not used.
    ignored_total: usize,
    /// How many device nodes were compared.
    scanned: usize,
    /// Of those, how many report a serial number. The poller reads it once an hour over SNMP.
    with_serial: usize,
    /// Of those, how many have an interface-address list. The poller reads it once an hour over
    /// SNMP when address discovery is on.
    with_address_list: usize,
}

/// One group of device nodes that look like one physical device.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct DuplicateGroup {
    /// `confident` when strong evidence joins the members and nothing in the group contradicts it;
    /// `possible` when only two kinds of weak evidence join them, or something contradicts it.
    confidence: DuplicateConfidence,
    /// What the members share, strongest first.
    evidence: Vec<DuplicateEvidence>,
    /// What says the members are not one device. Never empty on a `confident` group's opposite:
    /// a contradiction always makes the group `possible`.
    contradictions: Vec<DuplicateContradiction>,
    /// The member suggested to keep first, then the rest from the oldest registration.
    members: Vec<DuplicateMember>,
}

/// One piece of evidence.
///
/// Strong kinds (`address`, `serial`, `own_ip`, `arp_mac`, `lldp_chassis`) are enough on their own.
/// Weak kinds (`own_ip_one_way`, `cdp_device_id`, `name`) list a group only when two of them agree.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct DuplicateEvidence {
    kind: DuplicateEvidenceKind,
    /// The shared value: the address, the serial number, the MAC address, the chassis or device id,
    /// or the name.
    value: String,
    /// The members that share it.
    node_ids: Vec<Uuid>,
}

/// One node in a group.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct DuplicateMember {
    node_id: Uuid,
    node_name: String,
    /// The monitored address.
    address: String,
    /// The inventory folder; `null` ⇒ the tree root.
    group_id: Option<Uuid>,
    vendor: Option<String>,
    model: Option<String>,
    serial_number: Option<String>,
    /// When the node was added.
    created_at: DateTime<Utc>,
    /// How many nodes name this one as their dependency parent.
    dependents: i64,
    /// The member suggested to keep: the most depended-on, then the oldest. A suggestion only.
    suggested_keep: bool,
}

/// A value too many nodes share to count as evidence.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct DuplicateIgnoredValue {
    kind: DuplicateEvidenceKind,
    value: String,
    /// How many of the compared nodes share it.
    nodes: usize,
}

/// The groups the caller may see — the seam REST and MCP share.
pub(crate) async fn duplicates_view(
    admin: &AdminState,
    scope: &NodeScope,
) -> ApiResult<DuplicateNodesView> {
    let candidates = admin
        .repo
        .duplicate_inputs(scope.group_filter())
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "read duplicate inputs",
                "failed to read the inventory",
            )
        })?;
    // Fewer than two nodes cannot hold a duplicate, and there is no reason to read four more stores.
    let obs = if candidates.len() < 2 {
        Observations::default()
    } else {
        observations(admin, &candidates).await?
    };
    let findings = duplicates::find(&candidates, &obs);
    let by_id: HashMap<Uuid, &DuplicateInput> = candidates.iter().map(|c| (c.id, c)).collect();
    let groups = findings
        .groups
        .iter()
        .map(|g| DuplicateGroup {
            confidence: g.confidence,
            evidence: g
                .evidence
                .iter()
                .map(|e| DuplicateEvidence {
                    kind: e.kind,
                    value: e.value.clone(),
                    node_ids: e.nodes.clone(),
                })
                .collect(),
            contradictions: g.contradictions.clone(),
            members: g
                .members
                .iter()
                .filter_map(|id| by_id.get(id))
                .map(|c| DuplicateMember {
                    node_id: c.id,
                    node_name: c.name.clone(),
                    address: c.address.to_string(),
                    group_id: c.group_id,
                    vendor: c.vendor.clone(),
                    model: c.model.clone(),
                    serial_number: c.serial_number.clone(),
                    created_at: c.created_at,
                    dependents: c.dependents,
                    suggested_keep: c.id == g.keeper,
                })
                .collect(),
        })
        .collect();
    Ok(DuplicateNodesView {
        groups,
        total: findings.total,
        ignored: findings
            .ignored
            .into_iter()
            .map(|i| DuplicateIgnoredValue {
                kind: i.kind,
                value: i.value,
                nodes: i.nodes,
            })
            .collect(),
        ignored_total: findings.ignored_total,
        scanned: findings.scanned,
        with_serial: findings.with_serial,
        with_address_list: findings.with_address_list,
    })
}

/// What the stores beside `nodes` say about the candidates' addresses, read concurrently.
async fn observations(
    admin: &AdminState,
    candidates: &[DuplicateInput],
) -> ApiResult<Observations> {
    let addresses: Vec<String> = candidates
        .iter()
        .map(|c| c.address.to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let ids: Vec<Uuid> = candidates.iter().map(|c| c.id).collect();
    let (own, listed, arp, peers, meraki_serials) = tokio::try_join!(
        admin.l3.rows_naming(&addresses),
        admin.l3.nodes_with_a_list(&ids),
        admin.arp.macs_for(&addresses),
        admin.neighbors.reports_about(&addresses),
        admin.meraki_devices.serials_by_node(),
    )
    .map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "read duplicate evidence",
            "failed to read the evidence for duplicates",
        )
    })?;
    Ok(Observations {
        own_addresses: own
            .into_iter()
            .map(|(node, ip)| OwnAddress { node, ip })
            .collect(),
        with_address_list: listed,
        arp,
        peers: peers
            .into_iter()
            .map(|(address, id, protocol)| PeerReport {
                address,
                id,
                protocol,
            })
            .collect(),
        meraki_serials,
    })
}

#[utoipa::path(
    get, path = "/api/v1/nodes/duplicates", tag = "nodes",
    responses(
        (status = 200, description = "Groups of device nodes that look like one physical device registered more than once, with the evidence for each group and the member suggested to keep", body = DuplicateNodesView),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn get_duplicate_nodes(
    _perm: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
) -> ApiResult<Json<DuplicateNodesView>> {
    duplicates_view(&admin, &scope).await.map(Json)
}

#[cfg(test)]
mod tests {
    use crate::api::tests_support::{live_state, scoped_token, send, token};
    use axum::http::StatusCode;
    use yagra_common::Role;

    const PATH: &str = "/api/v1/nodes/duplicates";

    /// Two device nodes at one address come back as one confident group with the older suggested to
    /// keep, and a caller scoped to a different folder is shown neither of them.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn two_nodes_at_one_address_are_one_group_and_invisible_outside_their_folder(
        pool: sqlx::PgPool,
    ) {
        let st = live_state(pool.clone()).await;
        let tokyo = crate::pgtest::group(&pool, "tokyo").await;
        let osaka = crate::pgtest::group(&pool, "osaka").await;
        let older = crate::pgtest::node(&pool, "a-core-sw", 7, Some(tokyo)).await;
        let newer = crate::pgtest::node(&pool, "b-core-sw", 7, Some(tokyo)).await;
        crate::pgtest::node(&pool, "unrelated", 8, Some(tokyo)).await;

        let (status, body) = send(&st, "GET", PATH, &token(&st, Role::Admin), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], 1, "{body}");
        assert_eq!(body["scanned"], 3, "{body}");
        let group = &body["groups"][0];
        assert_eq!(group["confidence"], "confident", "{body}");
        assert_eq!(group["evidence"][0]["kind"], "address", "{body}");
        assert_eq!(group["evidence"][0]["value"], "10.0.0.7", "{body}");
        assert_eq!(group["members"][0]["node_id"], older.to_string(), "{body}");
        assert_eq!(group["members"][0]["suggested_keep"], true, "{body}");
        assert_eq!(group["members"][1]["node_id"], newer.to_string(), "{body}");
        assert_eq!(group["members"][1]["suggested_keep"], false, "{body}");

        let (status, body) = send(&st, "GET", PATH, &scoped_token(&st, &[osaka]), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], 0, "{body}");
        assert_eq!(body["scanned"], 0, "{body}");
    }

    /// A Viewer is refused — the list exists to be acted on, and acting is `ManageConfig` — and an
    /// inventory with nothing to compare answers an empty list rather than an error.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_viewer_is_refused_and_an_empty_inventory_answers_empty(pool: sqlx::PgPool) {
        let st = live_state(pool.clone()).await;
        let (status, body) = send(&st, "GET", PATH, &token(&st, Role::Viewer), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

        let (status, body) = send(&st, "GET", PATH, &token(&st, Role::Admin), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"], 0, "{body}");
        assert_eq!(body["groups"].as_array().map(Vec::len), Some(0), "{body}");
    }
}
