// SPDX-License-Identifier: AGPL-3.0-only
//! Nodes ▸ Subnet overlaps (ADR-187) — address ranges two sites both use, and what an operator has
//! said about them.
//!
//! One read and five writes. The read recomputes the comparison from the stored interface addresses
//! on every call ([`crate::subnet_overlaps`] is pure) and is **group-filtered**: a scoped caller is
//! told an overlap reaches sites it cannot see, and how many, never which. [`overlaps_view`] is the
//! seam the MCP `get_subnet_overlaps` tool shares, so the two surfaces cannot disclose differently.
//!
//! The writes — exclusion rules and "this overlap is deliberate" — are deployment-wide
//! configuration: `ManageConfig`, and **refused** for a scoped caller, whose view of an overlap is
//! partial and whose decision would silence it for sites they cannot see.

use std::collections::{BTreeSet, HashMap, HashSet};

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::{routing::get, routing::put, Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig, RequireView, Scoped};
use super::scope::{require_fleet_wide, NodeScope};
use super::util::CreatedId;
use super::{AdminState, ApiState};
use crate::repo::{OverlapRuleInput, OverlapRuleRefusal, StoredOverlapRule};
use crate::subnet_overlaps::{
    self, Ack, ExclusionReason, Input, Overlap, OverlapStatus, Port, Rule, SiteId,
};
use yagra_common::{SubnetKey, MAX_ADDRESSES_PER_NODE};

/// The built-in CGNAT rule seeded by migration 0143. Switchable, never editable or deletable.
#[allow(
    dead_code,
    reason = "named by the migration and the tests; the repo reads `builtin`"
)]
pub(crate) const CGNAT_RULE_ID: Uuid = Uuid::from_u128(0x0a0187);

/// The most overlaps one answer lists. `counts` beside them is never capped.
pub(crate) const OVERLAPS_MAX: usize = 2_000;

/// The longest note or port text accepted.
const TEXT_MAX: usize = 200;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    get_subnet_overlaps,
    create_rule,
    update_rule,
    delete_rule,
    set_ack,
    delete_ack
))]
pub(super) struct Doc;

/// The subnet-overlap routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/subnet-overlaps", get(get_subnet_overlaps))
        .route(
            "/api/v1/subnet-overlaps/rules",
            axum::routing::post(create_rule),
        )
        .route(
            "/api/v1/subnet-overlaps/rules/:id",
            put(update_rule).delete(delete_rule),
        )
        .route(
            "/api/v1/subnet-overlaps/acks",
            put(set_ack).delete(delete_ack),
        )
}

/// What Nodes ▸ Subnet overlaps shows.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct SubnetOverlapsView {
    /// Open first, then intentional, then excluded; inside each, same address, nested, then same
    /// range, and more sites first. At most 2,000 — `counts` says how many there are.
    overlaps: Vec<Overlap>,
    counts: OverlapCounts,
    /// Every exclusion rule, built-in first.
    rules: Vec<OverlapRuleView>,
    /// Devices the caller may see.
    nodes_total: u32,
    /// Of those, how many have reported their interface addresses at all. A device with no SNMP,
    /// or whose address walk has never succeeded, contributes nothing — so **no overlaps is not the
    /// same as none** unless this equals `nodes_total`.
    nodes_with_addresses: u32,
    /// Of those, how many address lists were cut at the per-device cap.
    nodes_truncated: u32,
    /// Distinct ranges compared, across the whole deployment.
    subnets_checked: u32,
}

/// How many overlaps the caller can see, by status.
#[derive(Debug, Default, Serialize, utoipa::ToSchema)]
pub(crate) struct OverlapCounts {
    open: u32,
    intentional: u32,
    excluded: u32,
}

/// One exclusion rule.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct OverlapRuleView {
    id: Uuid,
    /// Places inside this range match. `null` ⇒ any range.
    range: Option<String>,
    /// Places on a port whose name or description carries this as whole words (case-insensitive;
    /// a word may be followed by digits, so `dialer` matches `Dialer1` and `ha` does not match
    /// `Port-channel1`). `null` ⇒ any port.
    port_text: Option<String>,
    reason: ExclusionReason,
    note: String,
    enabled: bool,
    /// Built in: can be switched off, not edited or deleted.
    builtin: bool,
    /// How many overlaps this rule currently excludes, across the deployment.
    excluded_count: u32,
}

/// An exclusion rule as written by an operator.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct OverlapRuleBody {
    /// A network, `address/length`. Host bits are cleared. At least one of `range` and
    /// `port_text` is required.
    range: Option<String>,
    /// Words a port's name or description must carry, whole and in order (case-insensitive; a
    /// word may be followed by digits), at most 200 characters.
    port_text: Option<String>,
    reason: ExclusionReason,
    #[serde(default)]
    note: String,
    #[serde(default = "enabled_by_default")]
    enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

/// "This overlap is deliberate."
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct OverlapAckBody {
    /// The overlap's `key`, as the list returned it.
    key: String,
    #[serde(default)]
    note: String,
}

/// Which acknowledgement to remove.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub(crate) struct OverlapAckQuery {
    /// The overlap's `key`.
    key: String,
}

/// The site of every node: the nearest folder of type Site above it, else its own folder.
fn sites_by_node(
    groups: &[crate::groups::GroupSummary],
    nodes: &[(Uuid, String, Option<Uuid>)],
) -> HashMap<Uuid, SiteId> {
    let by_id: HashMap<Uuid, &crate::groups::GroupSummary> =
        groups.iter().map(|g| (g.id, g)).collect();
    let site_of_folder = |folder: Option<Uuid>| -> SiteId {
        let mut at = folder;
        // Bounded by the number of folders, so a cycle a bad import left cannot spin forever.
        for _ in 0..=groups.len() {
            let Some(id) = at else { break };
            let Some(g) = by_id.get(&id) else { break };
            if g.group_type == "site" {
                return Some(id);
            }
            at = g.parent_id;
        }
        folder
    };
    nodes
        .iter()
        .map(|(id, _, folder)| (*id, site_of_folder(*folder)))
        .collect()
}

fn internal(e: &anyhow::Error, what: &'static str) -> ApiError {
    ApiError::from_internal(e.as_ref(), what, "failed to read subnet overlaps")
}

/// The comparison before names are filled in and the list is cut to [`OVERLAPS_MAX`] — what an
/// acknowledgement looks its key up in, so an overlap past the cap can still be acknowledged.
struct Compared {
    groups: Vec<crate::groups::GroupSummary>,
    nodes: Vec<(Uuid, String, Option<Uuid>)>,
    stored_rules: Vec<StoredOverlapRule>,
    notes: HashMap<String, String>,
    findings: subnet_overlaps::Findings,
    visible: Option<HashSet<Uuid>>,
    nodes_with_addresses: u32,
    nodes_truncated: u32,
}

async fn compare(admin: &AdminState, scope: &NodeScope) -> ApiResult<Compared> {
    let (groups, nodes, snapshots, stored_rules, stored_acks) = tokio::try_join!(
        admin.groups.list(),
        admin.repo.node_folders(),
        admin.l3.all_current(),
        admin.repo.subnet_overlap_rules(),
        admin.repo.subnet_overlap_acks(),
    )
    .map_err(|e| internal(&e, "read subnet overlap inputs"))?;

    let site_of = sites_by_node(&groups, &nodes);
    let folder_of: HashMap<Uuid, Option<Uuid>> = nodes.iter().map(|(id, _, g)| (*id, *g)).collect();
    let visible: Option<HashSet<Uuid>> = (!scope.is_all()).then(|| {
        nodes
            .iter()
            .filter(|(_, _, g)| scope.allows_group(*g))
            .map(|(id, ..)| *id)
            .collect()
    });
    let sees = |n: &Uuid| visible.as_ref().is_none_or(|v| v.contains(n));

    // A node deleted since its address walk has no folder row; its addresses are not compared.
    let observed: Vec<(Uuid, &yagra_common::L3Snapshot)> = snapshots
        .iter()
        .filter(|(n, _)| folder_of.contains_key(&n.0))
        .map(|(n, s)| (n.0, s))
        .collect();

    let ids: Vec<Uuid> = observed.iter().map(|(n, _)| *n).collect();
    let ports: HashMap<(Uuid, u32), Port> = admin
        .repo
        .interface_idents_for(&ids)
        .await
        .map_err(|e| internal(&e, "read interface names"))?
        .into_iter()
        .filter_map(|((node, ifindex), ident)| {
            Some((
                (node, u32::try_from(ifindex).ok()?),
                Port {
                    name: ident.if_name,
                    alias: ident.if_alias,
                },
            ))
        })
        .collect();

    let rules: Vec<Rule> = stored_rules.iter().map(rule_of).collect();
    let acks: Vec<Ack> = stored_acks
        .iter()
        .map(|(key, sites, _)| Ack {
            key: key.clone(),
            sites: sites.iter().map(|s| (!s.is_nil()).then_some(*s)).collect(),
        })
        .collect();
    let notes: HashMap<String, String> = stored_acks.into_iter().map(|(k, _, n)| (k, n)).collect();

    let findings = subnet_overlaps::find(&Input {
        observed: &observed,
        site_of: &site_of,
        ports: &ports,
        rules: &rules,
        acks: &acks,
        visible: visible.as_ref(),
    });

    let nodes_with_addresses = observed.iter().filter(|(n, _)| sees(n)).count();
    let nodes_truncated = observed
        .iter()
        .filter(|(n, s)| sees(n) && (s.truncated || s.addresses.len() >= MAX_ADDRESSES_PER_NODE))
        .count();
    Ok(Compared {
        groups,
        nodes,
        stored_rules,
        notes,
        findings,
        nodes_with_addresses: count(nodes_with_addresses),
        nodes_truncated: count(nodes_truncated),
        visible,
    })
}

/// The overlaps the caller may see — the seam REST and MCP share.
pub(crate) async fn overlaps_view(
    admin: &AdminState,
    scope: &NodeScope,
) -> ApiResult<SubnetOverlapsView> {
    let Compared {
        groups,
        nodes,
        stored_rules,
        notes,
        findings,
        visible,
        nodes_with_addresses,
        nodes_truncated,
    } = compare(admin, scope).await?;
    let sees = |n: &Uuid| visible.as_ref().is_none_or(|v| v.contains(n));

    let group_names: HashMap<Uuid, &str> = groups.iter().map(|g| (g.id, g.name.as_str())).collect();
    let node_names: HashMap<Uuid, &str> =
        nodes.iter().map(|(id, n, _)| (*id, n.as_str())).collect();
    let mut counts = OverlapCounts::default();
    let mut overlaps = findings.overlaps;
    for o in &mut overlaps {
        match o.status {
            OverlapStatus::Open => counts.open += 1,
            OverlapStatus::Intentional => {
                counts.intentional += 1;
                o.note = notes.get(&o.key).cloned();
            }
            OverlapStatus::Excluded => counts.excluded += 1,
        }
        for p in &mut o.places {
            p.site_name = p
                .site_id
                .and_then(|s| group_names.get(&s))
                .map(|n| (*n).to_owned());
            p.node_name = node_names.get(&p.node_id).map(|n| (*n).to_owned());
        }
    }
    overlaps.truncate(OVERLAPS_MAX);

    Ok(SubnetOverlapsView {
        overlaps,
        counts,
        rules: stored_rules
            .into_iter()
            .map(|r| OverlapRuleView {
                excluded_count: findings.rule_hits.get(&r.id).copied().unwrap_or(0),
                id: r.id,
                range: r.range,
                port_text: r.port_text,
                reason: ExclusionReason::from_stored(&r.reason),
                note: r.note,
                enabled: r.enabled,
                builtin: r.builtin,
            })
            .collect(),
        nodes_total: count(nodes.iter().filter(|(id, ..)| sees(id)).count()),
        nodes_with_addresses,
        nodes_truncated,
        subnets_checked: findings.subnets_checked,
    })
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl SubnetOverlapsView {
    /// Every field populated, for the MCP forbidden-key canary (`mcp/dto.rs`).
    #[cfg(test)]
    pub(crate) fn sample() -> Self {
        use crate::subnet_overlaps::{Exclusion, OverlapHint, OverlapKind, Place};
        let site = Uuid::new_v4();
        Self {
            overlaps: vec![Overlap {
                key: "same:192.0.2.0/24".to_owned(),
                kind: OverlapKind::SameAddress,
                status: OverlapStatus::Excluded,
                subnet: "192.0.2.0/24".to_owned(),
                outer_withheld: false,
                inner: vec!["192.0.2.0/25".to_owned()],
                inner_count: 1,
                shared_addresses: vec!["192.0.2.1".to_owned()],
                site_count: 2,
                hidden_sites: 1,
                node_count: 1,
                places: vec![Place {
                    site_id: Some(site),
                    site_name: Some("site-a".to_owned()),
                    node_id: Uuid::new_v4(),
                    node_name: Some("rt-01".to_owned()),
                    ifindex: 1,
                    if_name: Some("ge-0/0/0".to_owned()),
                    if_alias: Some("ONU".to_owned()),
                    address: "192.0.2.1/24".to_owned(),
                    subnet: "192.0.2.0/24".to_owned(),
                    ip: "192.0.2.1".parse().expect("literal"),
                }],
                place_count: 1,
                hint: Some(OverlapHint::Wan {
                    word: "onu".to_owned(),
                }),
                excluded_by: vec![Exclusion::Rule {
                    rule_id: CGNAT_RULE_ID,
                }],
                note: Some("deliberate".to_owned()),
                sites: [Some(site)].into(),
            }],
            counts: OverlapCounts {
                open: 0,
                intentional: 0,
                excluded: 1,
            },
            rules: vec![OverlapRuleView {
                id: CGNAT_RULE_ID,
                range: Some("100.64.0.0/10".to_owned()),
                port_text: Some("onu".to_owned()),
                reason: ExclusionReason::Wan,
                note: "carrier".to_owned(),
                enabled: true,
                builtin: true,
                excluded_count: 1,
            }],
            nodes_total: 2,
            nodes_with_addresses: 1,
            nodes_truncated: 0,
            subnets_checked: 1,
        }
    }
}

/// A stored rule as the comparison reads it. A range that no longer parses matches nothing.
fn rule_of(r: &StoredOverlapRule) -> Rule {
    let range = r.range.as_deref().map(str::parse::<SubnetKey>);
    Rule {
        id: r.id,
        enabled: r.enabled && !matches!(range, Some(Err(_))),
        range: range.and_then(Result::ok),
        port_text: r.port_text.clone(),
    }
}

/// Parse and bound a rule body at the edge.
fn rule_input(body: OverlapRuleBody) -> ApiResult<OverlapRuleInput> {
    let range = match body
        .range
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => None,
        Some(text) => {
            let key: SubnetKey = text.parse().map_err(|_| {
                ApiError::bad_request(
                    "invalid_range",
                    "range must be a network written as address/length, e.g. 192.0.2.0/24",
                )
            })?;
            Some(key.to_string())
        }
    };
    let port_text = body
        .port_text
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty());
    if range.is_none() && port_text.is_none() {
        return Err(ApiError::bad_request(
            "empty_rule",
            "a rule needs a range, a port text, or both",
        ));
    }
    let too_long = |s: &str| s.chars().count() > TEXT_MAX;
    if port_text.as_deref().is_some_and(too_long) || too_long(&body.note) {
        return Err(ApiError::bad_request(
            "text_too_long",
            format!("port_text and note are at most {TEXT_MAX} characters"),
        ));
    }
    Ok(OverlapRuleInput {
        range,
        port_text,
        reason: body.reason.as_str(),
        note: body.note.trim().to_owned(),
        enabled: body.enabled,
    })
}

fn refusal(r: OverlapRuleRefusal) -> ApiError {
    match r {
        OverlapRuleRefusal::NotFound => ApiError::not_found("rule_not_found", "no such rule"),
        OverlapRuleRefusal::Builtin => ApiError::conflict(
            "builtin_rule",
            "a built-in rule can only be switched on or off",
        ),
    }
}

const SCOPED_WRITE: &str =
    "subnet-overlap rules and acknowledgements apply to every site; a folder-scoped token cannot set them";

#[utoipa::path(
    get, path = "/api/v1/subnet-overlaps", tag = "nodes",
    responses(
        (status = 200, description = "Address ranges that more than one site carries — the same range at two sites, or one site's range inside another's — with the exclusion rules and what each excludes. A caller scoped to some folders is told how many sites it cannot see, never which", body = SubnetOverlapsView),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the View permission", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn get_subnet_overlaps(
    _perm: RequireView,
    Scoped(scope): Scoped,
    admin: Admin,
) -> ApiResult<Json<SubnetOverlapsView>> {
    overlaps_view(&admin, &scope).await.map(Json)
}

#[utoipa::path(
    post, path = "/api/v1/subnet-overlaps/rules", tag = "nodes",
    request_body = OverlapRuleBody,
    responses(
        (status = 201, description = "The rule was added", body = CreatedId),
        (status = 400, description = "`invalid_range`, `empty_rule` or `text_too_long`", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig, or the token is scoped to some folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn create_rule(
    _perm: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<OverlapRuleBody>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    require_fleet_wide(&scope, SCOPED_WRITE)?;
    let input = rule_input(body)?;
    let id = admin
        .repo
        .insert_subnet_overlap_rule(&input)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "insert overlap rule", "failed to add the rule")
        })?;
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

#[utoipa::path(
    put, path = "/api/v1/subnet-overlaps/rules/{id}", tag = "nodes",
    params(("id" = Uuid, Path, description = "Rule id")),
    request_body = OverlapRuleBody,
    responses(
        (status = 204, description = "The rule was replaced"),
        (status = 400, description = "`invalid_range`, `empty_rule` or `text_too_long`", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig, or the token is scoped to some folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 404, description = "`rule_not_found`", body = super::error::ErrorBody),
        (status = 409, description = "`builtin_rule`: a built-in rule accepts only a change of `enabled`", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn update_rule(
    _perm: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<OverlapRuleBody>,
) -> ApiResult<StatusCode> {
    require_fleet_wide(&scope, SCOPED_WRITE)?;
    let input = rule_input(body)?;
    admin
        .repo
        .update_subnet_overlap_rule(id, &input)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "update overlap rule", "failed to save the rule")
        })?
        .map_err(refusal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete, path = "/api/v1/subnet-overlaps/rules/{id}", tag = "nodes",
    params(("id" = Uuid, Path, description = "Rule id")),
    responses(
        (status = 204, description = "The rule was deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig, or the token is scoped to some folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 404, description = "`rule_not_found`", body = super::error::ErrorBody),
        (status = 409, description = "`builtin_rule`: a built-in rule can be switched off, not deleted", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_rule(
    _perm: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    require_fleet_wide(&scope, SCOPED_WRITE)?;
    admin
        .repo
        .delete_subnet_overlap_rule(id)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "delete overlap rule",
                "failed to delete the rule",
            )
        })?
        .map_err(refusal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    put, path = "/api/v1/subnet-overlaps/acks", tag = "nodes",
    request_body = OverlapAckBody,
    responses(
        (status = 204, description = "The overlap is recorded as deliberate for the sites it spans now; a site joining it later reopens it"),
        (status = 400, description = "`text_too_long`", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig, or the token is scoped to some folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 404, description = "`overlap_not_found`: no current overlap has this key", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_ack(
    _perm: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<OverlapAckBody>,
) -> ApiResult<StatusCode> {
    require_fleet_wide(&scope, SCOPED_WRITE)?;
    if body.note.chars().count() > TEXT_MAX {
        return Err(ApiError::bad_request(
            "text_too_long",
            format!("note is at most {TEXT_MAX} characters"),
        ));
    }
    // The sites are the ones the overlap spans now, read from the comparison itself rather than
    // taken from the client, so an acknowledgement cannot cover a site nobody was shown.
    // Uncut and unnamed: an overlap past the listing's cap is still one an operator can mark.
    let compared = compare(&admin, &scope).await?;
    let Some(overlap) = compared
        .findings
        .overlaps
        .iter()
        .find(|o| o.key == body.key)
    else {
        return Err(ApiError::not_found(
            "overlap_not_found",
            "no current overlap has this key",
        ));
    };
    let sites: Vec<Uuid> = overlap
        .site_ids()
        .iter()
        .map(|s| s.unwrap_or(Uuid::nil()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    admin
        .repo
        .set_subnet_overlap_ack(&body.key, &sites, body.note.trim())
        .await
        .map_err(|e| ApiError::from_internal(e.as_ref(), "set overlap ack", "failed to save"))?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete, path = "/api/v1/subnet-overlaps/acks", tag = "nodes",
    params(OverlapAckQuery),
    responses(
        (status = 204, description = "The overlap is open again"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig, or the token is scoped to some folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 404, description = "`ack_not_found`: this overlap was not recorded as deliberate", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_ack(
    _perm: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Query(q): Query<OverlapAckQuery>,
) -> ApiResult<StatusCode> {
    require_fleet_wide(&scope, SCOPED_WRITE)?;
    let removed = admin
        .repo
        .delete_subnet_overlap_ack(&q.key)
        .await
        .map_err(|e| ApiError::from_internal(e.as_ref(), "delete overlap ack", "failed to save"))?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            "ack_not_found",
            "this overlap was not recorded as deliberate",
        ))
    }
}

#[cfg(test)]
mod tests {
    use crate::api::tests_support::{live_state, scoped_token, send, token};
    use axum::http::StatusCode;
    use serde_json::json;
    use yagra_common::{L3AddrType, L3Address, L3Snapshot, L3SourceTable, Role};

    const PATH: &str = "/api/v1/subnet-overlaps";

    /// `pgtest::group` creates a folder of type Site, which is what a site is here.
    async fn site(pool: &sqlx::PgPool, name: &str) -> uuid::Uuid {
        crate::pgtest::group(pool, name).await
    }

    async fn addresses(st: &super::ApiState, node: uuid::Uuid, list: &[(&str, u8)]) {
        let admin = st.admin.as_ref().expect("live");
        let snap = L3Snapshot::new(
            list.iter()
                .enumerate()
                .map(|(i, (ip, len))| L3Address {
                    ifindex: u32::try_from(i + 1).unwrap(),
                    ip: ip.parse().unwrap(),
                    prefix_len: *len,
                    addr_type: L3AddrType::Unicast,
                    source_table: L3SourceTable::IpAddressTable,
                })
                .collect(),
        );
        admin
            .l3
            .record_observation(node, &snap)
            .await
            .expect("record addresses");
    }

    /// The same gateway at two sites is listed; a scoped caller is told a second site exists
    /// without its name; a rule, an acknowledgement and their removal are each ACCEPTED and each
    /// moves the overlap where the list says it went.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_overlap_is_listed_scoped_excluded_and_acknowledged(pool: sqlx::PgPool) {
        let st = live_state(pool.clone()).await;
        let tokyo = site(&pool, "tokyo").await;
        let nagoya = site(&pool, "nagoya").await;
        let a = crate::pgtest::node(&pool, "tky-core-01", 1, Some(tokyo)).await;
        let b = crate::pgtest::node(&pool, "ngy-rt-01", 2, Some(nagoya)).await;
        addresses(&st, a, &[("10.10.20.1", 24), ("100.64.1.1", 22)]).await;
        addresses(&st, b, &[("10.10.20.1", 24), ("100.64.2.9", 22)]).await;
        let admin = token(&st, Role::Admin);

        let (status, body) = send(&st, "GET", PATH, &admin, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["counts"]["open"], 1, "{body}");
        assert_eq!(
            body["counts"]["excluded"], 1,
            "the built-in CGNAT rule: {body}"
        );
        let o = &body["overlaps"][0];
        assert_eq!(o["key"], "same:10.10.20.0/24", "{body}");
        assert_eq!(o["kind"], "same_address", "{body}");
        assert!(o["places"][0]["site_name"].as_str().is_some(), "{body}");
        let cgnat = &body["rules"][0];
        assert_eq!(cgnat["builtin"], true, "{body}");
        assert_eq!(cgnat["excluded_count"], 1, "{body}");

        let (status, body) = send(&st, "GET", PATH, &scoped_token(&st, &[tokyo]), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let o = &body["overlaps"][0];
        assert_eq!(o["hidden_sites"], 1, "{body}");
        assert!(
            !body.to_string().contains("ngy-rt-01") && !body.to_string().contains("nagoya"),
            "the other site is counted, never named: {body}"
        );

        // A scoped token may not set a deployment-wide rule.
        let rule = json!({"range": "10.10.20.0/24", "reason": "other"});
        let (status, _) = send(
            &st,
            "POST",
            "/api/v1/subnet-overlaps/rules",
            &scoped_token(&st, &[tokyo]),
            Some(rule.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/subnet-overlaps/rules",
            &admin,
            Some(rule),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let rule_id = body["id"].as_str().expect("id").to_owned();
        let (_, body) = send(&st, "GET", PATH, &admin, None).await;
        assert_eq!(body["counts"]["open"], 0, "{body}");
        assert_eq!(body["counts"]["excluded"], 2, "{body}");

        let path = format!("/api/v1/subnet-overlaps/rules/{rule_id}");
        let (status, body) = send(&st, "DELETE", &path, &admin, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

        let (status, body) = send(
            &st,
            "PUT",
            "/api/v1/subnet-overlaps/acks",
            &admin,
            Some(json!({"key": "same:10.10.20.0/24", "note": "NAT inside"})),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let (_, body) = send(&st, "GET", PATH, &admin, None).await;
        assert_eq!(body["counts"]["intentional"], 1, "{body}");
        let acked = body["overlaps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["key"] == "same:10.10.20.0/24")
            .expect("listed");
        assert_eq!(acked["status"], "intentional", "{body}");
        assert_eq!(acked["note"], "NAT inside", "{body}");

        let (status, _) = send(
            &st,
            "DELETE",
            "/api/v1/subnet-overlaps/acks?key=same%3A10.10.20.0%2F24",
            &admin,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, body) = send(&st, "GET", PATH, &admin, None).await;
        assert_eq!(body["counts"]["open"], 1, "{body}");
    }

    /// The built-in CGNAT rule can be switched off but not edited or deleted, and a rule naming
    /// neither a range nor a port is refused.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_builtin_rule_only_switches_and_an_empty_rule_is_refused(pool: sqlx::PgPool) {
        let st = live_state(pool.clone()).await;
        let admin = token(&st, Role::Admin);
        let path = format!("/api/v1/subnet-overlaps/rules/{}", super::CGNAT_RULE_ID);
        let off = json!({
            "range": "100.64.0.0/10", "reason": "wan", "enabled": false,
            "note": "CGNAT (RFC 6598): addresses a carrier hands out behind its own NAT",
        });
        let (status, body) = send(&st, "PUT", &path, &admin, Some(off)).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let (_, body) = send(&st, "GET", PATH, &admin, None).await;
        assert_eq!(body["rules"][0]["enabled"], false, "{body}");

        let edit = json!({"range": "100.64.0.0/12", "reason": "wan"});
        let (status, body) = send(&st, "PUT", &path, &admin, Some(edit)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        let (status, _) = send(&st, "DELETE", &path, &admin, None).await;
        assert_eq!(status, StatusCode::CONFLICT);

        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/subnet-overlaps/rules",
            &admin,
            Some(json!({"port_text": "  ", "reason": "wan"})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        let (status, _) = send(&st, "GET", PATH, &token(&st, Role::Viewer), None).await;
        assert_eq!(status, StatusCode::OK, "a Viewer reads the list");
        let (status, _) = send(
            &st,
            "POST",
            "/api/v1/subnet-overlaps/rules",
            &token(&st, Role::Viewer),
            Some(json!({"port_text": "wan", "reason": "wan"})),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "a Viewer cannot write one");
    }
}
