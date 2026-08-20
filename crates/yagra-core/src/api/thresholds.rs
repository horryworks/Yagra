// SPDX-License-Identifier: AGPL-3.0-only
//! Threshold rules (Alerting ▸ Thresholds) — scope-based warning/critical bounds.
//!
//! Every endpoint is `ManageConfig`: a threshold decides when the fleet pages someone, so reading
//! the ruleset is as sensitive as writing it. Resolution (most-specific-wins, ADR-013) lives in
//! `yagra_common`; this module is only the CRUD surface over [`crate::thresholds::ThresholdStore`].
//!
//! **The list is capped.** Thresholds are the one configuration table that grows with the fleet —
//! a node-level override is per (node × metric), so tens of thousands of nodes means a response
//! nobody can render. The cap is explicit in the payload (`total` / `truncated`) rather than a
//! silent prefix, because a silently short list reads as "these are all the rules", which is
//! exactly the wrong belief to hold about alerting configuration.

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig, VisibleNode};
use super::util::{normalize_search, CreatedId};
use super::{is_valid_metric_name, ApiState};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yagra_common::{Direction, IfIndex, NodeId, ScopeLevel};

/// Maximum rules returned by one list call. Matches the poller drill-down's cap: enough that no
/// real hand-authored ruleset is ever truncated, small enough that a fleet-scale accident is a
/// slow page rather than an unusable one.
const THRESHOLDS_MAX: i64 = 500;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_thresholds,
    list_interface_thresholds,
    create_threshold,
    update_threshold,
    delete_threshold
))]
pub(super) struct Doc;

/// The threshold routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/api/v1/thresholds",
            get(list_thresholds).post(create_threshold),
        )
        .route(
            "/api/v1/thresholds/:id",
            put(update_threshold).delete(delete_threshold),
        )
        .route(
            "/api/v1/nodes/:node_id/interfaces/:ifindex/thresholds",
            get(list_interface_thresholds),
        )
}

/// A capped page of threshold rules.
///
/// `total` is the count of rules **matching the filter**, not `items.len()`, so the UI can say *how
/// many* it is not showing. `truncated` is derived rather than left to the client comparing the
/// two — a client that forgets the comparison shows a complete-looking list.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct ThresholdPage {
    items: Vec<crate::thresholds::StoredThreshold>,
    /// Rules matching the filter, ignoring the cap.
    total: i64,
    /// Whether `items` is a prefix of the matching rules rather than all of them.
    truncated: bool,
}

/// `?limit=` plus the filters, all optional.
///
/// The two enums are parsed at the edge, and an unknown token is a `400` rather than a filter
/// silently dropped. Dropping one would *widen* the answer — the operator asked for node-level
/// rules and would be shown every rule in the fleet, believing the narrower thing.
///
/// Since ADR-053 Inc.4b they are comma-separated strings rather than typed serde fields, so several
/// values can be named at once; the parse moved from serde to [`super::util::parse_set`]. The
/// rejection property is unchanged, which is the half that matters.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct ThresholdQuery {
    limit: Option<i64>,
    /// Case-insensitive substring of the metric name.
    q: Option<String>,
    /// Comma-separated scope levels (`global` | `profile` | `group` | `node`); empty or absent
    /// means every level.
    scope_level: Option<String>,
    /// Comma-separated directions (`above` | `below`); empty or absent means both.
    direction: Option<String>,
}

#[utoipa::path(
    get, path = "/api/v1/thresholds", tag = "thresholds",
    params(ThresholdQuery),
    responses(
        (status = 200, description = "A capped page of matching rules, with the matching total", body = ThresholdPage),
        (status = 400, description = "`scope_level` or `direction` names a value outside its vocabulary", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig — the ruleset decides when the fleet pages someone", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn list_thresholds(
    _guard: RequireManageConfig,
    Query(q): Query<ThresholdQuery>,
    admin: Admin,
) -> ApiResult<Json<ThresholdPage>> {
    let metric = normalize_search(q.q.as_deref());
    let levels = super::util::parse_set(
        "scope_level",
        q.scope_level.as_deref(),
        "global, profile, group or node",
        yagra_common::ScopeLevel::from_token,
    )?;
    let directions = super::util::parse_set(
        "direction",
        q.direction.as_deref(),
        "above or below",
        yagra_common::Direction::from_token,
    )?;
    Ok(Json(
        threshold_page(
            &admin,
            q.limit,
            &crate::thresholds::ThresholdFilter {
                metric: metric.as_deref(),
                level: &levels,
                direction: &directions,
            },
        )
        .await?,
    ))
}

/// A capped page of rules — the seam both edges call.
///
/// The cap and the `truncated` derivation live here rather than in the handler so the MCP tool
/// cannot serve an uncapped list, and cannot report a complete-looking one either. `truncated` is
/// the load-bearing half: a silently short ruleset reads as "these are all the rules", which is
/// exactly the wrong belief to hold about alerting configuration.
///
/// **The filter is deliberately not exposed on the MCP side.** `get_config(kind=thresholds)` is a
/// configuration dump — its callers ask "what is the ruleset", not "show me the CPU ones" — so the
/// filters are a UI narrowing rather than a question MCP cannot otherwise answer (ADR-042 asks for
/// parity on *questions*, and records the reason when a read has no tool of its own). It still
/// comes through this seam, so the cap and `truncated` can never differ between the two edges.
pub(crate) async fn threshold_page(
    admin: &super::AdminState,
    limit: Option<i64>,
    filter: &crate::thresholds::ThresholdFilter<'_>,
) -> ApiResult<ThresholdPage> {
    let limit = limit.unwrap_or(THRESHOLDS_MAX).clamp(1, THRESHOLDS_MAX);
    let (items, total) = admin
        .thresholds
        .list_page(limit, filter)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "list thresholds", "failed to list thresholds")
        })?;
    let truncated = total > i64::try_from(items.len()).unwrap_or(i64::MAX);
    if truncated {
        tracing::info!(total, limit, "threshold list capped to the page limit");
    }
    Ok(ThresholdPage {
        items,
        total,
        truncated,
    })
}

/// One rule that reaches a port, and whether it is the one in force there.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct MatchingThreshold {
    /// The stored rule, in the same shape the rules list serves.
    rule: crate::thresholds::StoredThreshold,
    /// Whether this rule sits at the **winning** scope level for its metric — the most specific
    /// level that reaches this port, and among folder-group rules only the nearest group in the
    /// chain (ADR-013 + ADR-075 決定 11).
    ///
    /// Several rules can carry `true` for one metric at once: the engine merges rules at the
    /// winning level by keeping the more restrictive bound of each severity. So this means "this
    /// rule contributes to the effective bound", not "this rule *is* the effective bound".
    in_force: bool,
}

#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/interfaces/{ifindex}/thresholds", tag = "thresholds",
    params(
        ("node_id" = Uuid, Path, description = "Node id"),
        ("ifindex" = u32, Path, description = "SNMP ifIndex of the interface"),
    ),
    responses(
        (status = 200, description = "Every rule that reaches this port, from any scope level, most specific first, each flagged with whether it is in force", body = Vec<MatchingThreshold>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig, or the node is outside the token's group scope", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
/// The threshold rules that reach one port (ADR-076 決定 11).
///
/// A port is governed by rules at six scope levels, and the narrow ones are usually not where the
/// interesting rule lives — a fleet-wide "any link over 90%" is a global rule, and a page that
/// listed only the port's own rules would show an empty list about a port that is alerting.
///
/// Rules come from PostgreSQL on every call rather than from the alert engine's snapshot, which
/// refreshes on the config generation: a rule saved a second ago is not in that snapshot, and the
/// operator who just pressed Save is precisely the caller of this endpoint. The node's own
/// metadata (profile, tag values, folder chain) does come from the snapshot — it is what decides
/// whether a broad rule reaches this node, and it does not change under the operator's hand.
async fn list_interface_thresholds(
    _perm: RequireManageConfig,
    _visible: VisibleNode,
    State(st): State<ApiState>,
    Path((node_id, ifindex)): Path<(Uuid, u32)>,
    admin: Admin,
) -> ApiResult<Json<Vec<MatchingThreshold>>> {
    Ok(Json(
        interface_thresholds(&st, &admin.0, node_id, ifindex).await?,
    ))
}

/// The seam both edges call — REST above, and the `get_interface_thresholds` MCP tool.
///
/// One function rather than two, for the reason ADR-042 exists: the two surfaces must not come to
/// disagree about which rules reach a port. The scope and permission guards stay with each edge,
/// because they are expressed differently there (extractors vs. an explicit scope check).
pub(crate) async fn interface_thresholds(
    st: &ApiState,
    admin: &super::AdminState,
    node_id: Uuid,
    ifindex: u32,
) -> ApiResult<Vec<MatchingThreshold>> {
    let candidates = admin
        .thresholds
        .candidates_for_interface(node_id, ifindex)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "list interface thresholds",
                "failed to list threshold rules for this interface",
            )
        })?;
    Ok(st
        .alerts
        .matching_rules(&candidates, NodeId::from(node_id), Some(IfIndex(ifindex)))
        .into_iter()
        .map(|(rule, in_force)| MatchingThreshold { rule, in_force })
        .collect())
}

/// Whether the built-in catalog declares `metric` a raw counter. Pure (no store) so the
/// rejection below is testable without a database; custom collection items are the async
/// half, via [`crate::collection::CollectionRepo::metric_declared_counter`].
fn is_builtin_counter(metric: &str) -> bool {
    match yagra_common::builtin_metric_kind(metric) {
        Some(yagra_common::MetricKind::Counter) => true,
        Some(yagra_common::MetricKind::Gauge) | None => false,
    }
}

/// A threshold rule as the caller states it — the body of both `POST` and `PUT`.
///
/// One type for both, the shape `ProfileBody` already uses. Two would be two validators, and this
/// repo has paid for that: URL-check and DNS-check CRUD were two copies of one writer, so the "a
/// node is exactly one kind" rule shipped enforced on only one of them.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct ThresholdBody {
    scope_level: String,
    /// The rule’s target when it has exactly one. Superseded by `scope_ids`, which wins if both
    /// are sent; still accepted so that a client written against the earlier shape keeps working.
    #[serde(default)]
    scope_id: Option<String>,
    /// Every profile, folder group or node the rule applies to — `scope_level` says which of those
    /// they are. Omit it (or send an empty list) for a `global` rule, which applies to every node.
    /// At most 32, no repeats; an `interface` rule names exactly one port.
    #[serde(default)]
    scope_ids: Option<Vec<String>>,
    metric: String,
    direction: String,
    warning: Option<f64>,
    critical: Option<f64>,
    dwell_samples: Option<i32>,
}

/// Most targets one rule may name (ADR-078 decision 3).
///
/// The largest built-in fan-out is four (the Huawei VRP profiles), so this is room rather than a
/// constraint. It exists because [`crate::alerts::threshold_applies`] walks the set for every
/// sample: an unbounded list is a per-poll cost an operator can set from a text field.
/// ⚠️ Not measured — chosen as headroom over the built-ins. Someone wanting more targets than
/// this wants a folder group, which is one target that grows on its own.
const MAX_SCOPE_IDS: usize = 32;

/// A body that passed the checks that need no I/O, with the target set already normalized.
#[derive(Debug)]
struct ParsedThreshold<'a> {
    scope_level: &'a str,
    /// Owned rather than borrowed: the set is *derived* from the body (either field may have
    /// supplied it) rather than being one of its strings.
    scope_ids: Vec<String>,
    direction: &'a str,
}

/// The synchronous half of validating a rule — shared by create and update.
///
/// The counter check is **not** here: it reads the stored collection items, so it is async and
/// stays in the handlers, which call it in the same order on both paths.
fn parse_threshold_body(body: &ThresholdBody) -> ApiResult<ParsedThreshold<'_>> {
    if !is_valid_metric_name(&body.metric) {
        return Err(ApiError::bad_request(
            "invalid_metric_name",
            "metric must be a valid identifier",
        ));
    }
    // The vocabulary comes from the enum, not from a list of literals repeated here — a new
    // `ScopeLevel` variant would otherwise be rejected by an edge nobody remembered to widen.
    let Some(level) = ScopeLevel::from_token(&body.scope_level) else {
        // The message lists the vocabulary from the enum rather than spelling it out: the literal
        // that used to be here still read "global|profile|group|group_id|node" after a sixth level
        // existed, so the error told the caller a valid value was invalid.
        return Err(ApiError::bad_request(
            "invalid_threshold",
            format!(
                "scope_level must be one of {}",
                ScopeLevel::ALL.map(ScopeLevel::as_str).join("|")
            ),
        ));
    };
    // Same reasoning as the level above: two tokens, one enum that already knows them.
    let Some(_) = Direction::from_token(&body.direction) else {
        return Err(ApiError::bad_request(
            "invalid_threshold",
            format!(
                "direction must be one of {}",
                Direction::ALL.map(Direction::as_str).join("|")
            ),
        ));
    };
    // A `global` rule targets the whole fleet, so it has nothing to point at (ADR-075). Pinning
    // the set empty here rather than trusting the caller is what keeps `AlertConfig::applies`
    // able to ignore it for this level: two global rules that differed only in a stray target
    // would both apply, look identical in the list, and be impossible to tell apart.
    //
    // `scope_ids` wins over `scope_id` when both arrive, and `scope_id` alone still works — it is
    // what a caller written before ADR-078 sends, and refusing those would break the contract for
    // no gain (the single-target case is exactly a set of one).
    let scope_ids: Vec<String> = if level == ScopeLevel::Global {
        Vec::new()
    } else {
        match (&body.scope_ids, &body.scope_id) {
            (Some(ids), _) => ids.iter().map(|s| s.trim().to_owned()).collect(),
            (None, Some(one)) => vec![one.trim().to_owned()],
            (None, None) => Vec::new(),
        }
    };
    if level != ScopeLevel::Global {
        if scope_ids.is_empty() {
            return Err(ApiError::bad_request(
                "invalid_scope_id",
                "a rule at this scope level must name at least one target",
            ));
        }
        if scope_ids.len() > MAX_SCOPE_IDS {
            return Err(ApiError::bad_request(
                "invalid_scope_id",
                format!("a rule may name at most {MAX_SCOPE_IDS} targets"),
            ));
        }
        // A repeated target is not merely redundant: the rule would be listed as covering N
        // things while covering N-1, which is the kind of quiet miscount an operator only finds
        // when the one they thought they added never fires.
        let mut seen = std::collections::BTreeSet::new();
        if !scope_ids.iter().all(|s| seen.insert(s.as_str())) {
            return Err(ApiError::bad_request(
                "invalid_scope_id",
                "the same target is named twice",
            ));
        }
        // One port of one node. A fleet-wide port picker does not exist, so a rule at this level
        // is created from the port being looked at (ADR-076 増分 5) and there is nothing that
        // could have supplied a second one — accepting a set here would be a shape only a
        // hand-written call can produce, with no screen able to show or edit it.
        if level == ScopeLevel::Interface && scope_ids.len() != 1 {
            return Err(ApiError::bad_request(
                "invalid_scope_id",
                "an interface rule names exactly one port",
            ));
        }
    }
    // The id's *shape* is checked here, and was not before. An unparseable id is not a rule that
    // does something slightly wrong — `AlertConfig::applies` compares it against a uuid or a tag
    // and simply never matches, so the rule is created, listed, and silently evaluates for no
    // node at all (the `StoredThreshold` docs say so). Since ADR-075 増分 3 the WebUI picks these
    // from a list, so a malformed one now only arrives from a hand-written call.
    for scope_id in &scope_ids {
        let scope_id = scope_id.as_str();
        match level {
            ScopeLevel::Global => {}
            ScopeLevel::Profile | ScopeLevel::FolderGroup | ScopeLevel::Node => {
                if Uuid::parse_str(scope_id).is_err() {
                    return Err(ApiError::bad_request(
                        "invalid_scope_id",
                        "scope_id must be a uuid for the profile, group_id and node levels",
                    ));
                }
            }
            // One port of one node (ADR-076), spelled `<node-uuid>:<ifindex>`. Parsed through the one
            // codec the alert engine's `applies` also uses, so "the API accepted it" and "the engine
            // can match it" cannot come apart.
            ScopeLevel::Interface => {
                if yagra_common::parse_interface_scope_id(scope_id).is_none() {
                    return Err(ApiError::bad_request(
                        "invalid_scope_id",
                        "scope_id must be <node-uuid>:<ifindex> for the interface level",
                    ));
                }
            }
            // A tag value is free-form text, so only emptiness is checkable — and an empty one is the
            // same silent no-op as a malformed uuid.
            ScopeLevel::Group => {
                if scope_id.trim().is_empty() {
                    return Err(ApiError::bad_request(
                        "invalid_scope_id",
                        "scope_id must name a node tag value for the group level",
                    ));
                }
            }
        }
    }
    Ok(ParsedThreshold {
        scope_level: &body.scope_level,
        scope_ids,
        direction: &body.direction,
    })
}

/// Whether `metric` may not carry a threshold because its samples only ever increase.
///
/// A raw counter's sampled value rises until it wraps or the device reboots, so a fixed bound
/// cannot be evaluated against it: `above` latches permanently and `below` fires on every reset.
/// Rates come from the TSDB at query time (ADR-012). The engine also refuses to evaluate counter
/// samples, so this is the operator-facing half of one rule — and it is a `SELECT`, which is why
/// it is not part of [`parse_threshold_body`].
async fn reject_counter_metric(admin: &super::AdminState, metric: &str, op: &str) -> ApiResult<()> {
    // A derived metric (interface utilisation, ADR-076) is a gauge Yagra computes rather than
    // collects, so it appears in neither the built-in catalogue nor either item table. Answering
    // from the derived list first is what stops the two database probes from running for it, and
    // is also the only place that can answer at all — `metric_declared_counter` would say "not a
    // counter" for a typo just as readily.
    if crate::interface_util::derived_metric_kind(metric).is_some() {
        return Ok(());
    }
    let is_counter = is_builtin_counter(metric)
        || admin
            .collection
            .metric_declared_counter(metric)
            .await
            .map_err(|e| ApiError::from_internal(e.as_ref(), op, "failed to save threshold"))?;
    if is_counter {
        return Err(ApiError::bad_request(
            "counter_metric",
            "the metric is a raw counter; its sampled value only ever increases, so a fixed threshold cannot be evaluated against it",
        ));
    }
    Ok(())
}

#[utoipa::path(
    post, path = "/api/v1/thresholds", tag = "thresholds",
    request_body = ThresholdBody,
    responses(
        (status = 201, description = "Rule created", body = CreatedId),
        (status = 400, description = "The metric is not an identifier, scope_level/direction is outside its vocabulary, or the metric is a raw counter (a monotonic value has no meaningful fixed bound). A `global` rule ignores `scope_id` — it targets every node", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn create_threshold(
    _guard: RequireManageConfig,
    admin: Admin,
    Json(body): Json<ThresholdBody>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    let p = parse_threshold_body(&body)?;
    reject_counter_metric(&admin, &body.metric, "create threshold").await?;
    let id = admin
        .thresholds
        .create(crate::thresholds::ThresholdWrite {
            scope_level: p.scope_level,
            scope_ids: &p.scope_ids,
            metric: &body.metric,
            direction: p.direction,
            warning: body.warning,
            critical: body.critical,
            dwell_samples: body.dwell_samples.unwrap_or(3),
        })
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "create threshold", "failed to create threshold")
        })?;
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

#[utoipa::path(
    put, path = "/api/v1/thresholds/{id}", tag = "thresholds",
    params(("id" = Uuid, Path, description = "Threshold rule id")),
    request_body = ThresholdBody,
    responses(
        (status = 204, description = "Rule updated"),
        (status = 400, description = "The metric is not an identifier, scope_level/direction is outside its vocabulary, or the metric is a raw counter. A `global` rule ignores `scope_id` — it targets every node", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such rule", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn update_threshold(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<ThresholdBody>,
) -> ApiResult<StatusCode> {
    let p = parse_threshold_body(&body)?;
    reject_counter_metric(&admin, &body.metric, "update threshold").await?;
    let updated = admin
        .thresholds
        .update(
            id,
            crate::thresholds::ThresholdWrite {
                scope_level: p.scope_level,
                scope_ids: &p.scope_ids,
                metric: &body.metric,
                direction: p.direction,
                warning: body.warning,
                critical: body.critical,
                dwell_samples: body.dwell_samples.unwrap_or(3),
            },
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "update threshold", "failed to update threshold")
        })?;
    if updated {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            "threshold_not_found",
            format!("no threshold {id}"),
        ))
    }
}

#[utoipa::path(
    delete, path = "/api/v1/thresholds/{id}", tag = "thresholds",
    params(("id" = Uuid, Path, description = "Threshold rule id")),
    responses(
        (status = 204, description = "Rule deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such rule", body = super::error::ErrorBody),
        (status = 503, description = "Skeleton mode has no write side", body = super::error::ErrorBody),
    ),
)]
async fn delete_threshold(
    _guard: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    match admin.thresholds.delete(id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "threshold_not_found",
            format!("no threshold {id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "delete threshold",
            "failed to delete threshold",
        )),
    }
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

    const ID: &str = "00000000-0000-0000-0000-000000000001";

    /// Every method/path this module serves, so the guard tests below cannot silently skip one.
    fn routes_under_test() -> Vec<(&'static str, String)> {
        vec![
            ("GET", "/api/v1/thresholds".to_owned()),
            ("POST", "/api/v1/thresholds".to_owned()),
            ("PUT", format!("/api/v1/thresholds/{ID}")),
            ("DELETE", format!("/api/v1/thresholds/{ID}")),
            // The per-port read is `ManageConfig` too, for the same reason the list is: it is the
            // ruleset, sliced. Listed here so the two guard tests below cover it without being
            // told about it separately — the failure they exist to catch is a route that answers
            // 503 before it answers 401, and a new route is exactly where that reappears.
            ("GET", format!("/api/v1/nodes/{ID}/interfaces/7/thresholds")),
        ]
    }

    async fn status_of(st: ApiState, method: &str, path: &str, token: Option<&str>) -> StatusCode {
        let mut b = Request::builder().method(method).uri(path);
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        let body = if method == "POST" || method == "PUT" {
            b = b.header("content-type", "application/json");
            Body::from("{}")
        } else {
            Body::empty()
        };
        router(st)
            .oneshot(b.body(body).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn an_anonymous_caller_is_told_it_is_anonymous_and_nothing_else() {
        // `Require*` before `Admin`: 401, never the 503 that would reveal whether this deployment
        // has a write side. All three answered 503 first before the migration.
        for (method, path) in routes_under_test() {
            assert_eq!(
                status_of(private_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn reading_thresholds_needs_manage_config_even_on_a_public_dashboard() {
        // Unlike most reads, the list is `ManageConfig`, not `View`: the ruleset is what decides
        // when the fleet pages someone, and a public dashboard must not expose it.
        assert_eq!(
            status_of(public_state(), "GET", "/api/v1/thresholds", None).await,
            StatusCode::UNAUTHORIZED,
        );
        // A viewer is refused the ruleset entirely; an operator owns it (ADR-057).
        let st = private_state();
        for (role, want) in [
            (Role::Viewer, StatusCode::FORBIDDEN),
            (Role::Operator, StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), "u");
            for (method, path) in routes_under_test() {
                // 403, not 401 — "not allowed" and "not logged in" are different fixes.
                assert_eq!(
                    status_of(st.clone(), method, &path, Some(&token)).await,
                    want,
                    "{role:?} {method} {path}"
                );
            }
        }
    }

    #[test]
    fn builtin_counters_are_rejected_gauges_and_unknowns_pass() {
        // The catalog's counters can never take a threshold — the sampled value is monotonic.
        assert!(is_builtin_counter("if_hc_in_octets"));
        assert!(is_builtin_counter("if_out_errors"));
        // sysUpTime is deliberately a gauge: reboot detection via `below` must keep working.
        assert!(!is_builtin_counter("snmp_sys_uptime_ticks"));
        // A name outside the catalog is not rejected here — the stored-item check owns it.
        assert!(!is_builtin_counter("icmp_rtt_ms"));
    }

    /// A body naming one target through the **legacy** `scope_id` field, so every existing case
    /// below keeps testing the pre-ADR-078 shape a client may still send.
    fn body(scope_level: &str, scope_id: &str, metric: &str, direction: &str) -> ThresholdBody {
        ThresholdBody {
            scope_level: scope_level.to_owned(),
            scope_id: Some(scope_id.to_owned()),
            scope_ids: None,
            metric: metric.to_owned(),
            direction: direction.to_owned(),
            warning: None,
            critical: None,
            dwell_samples: None,
        }
    }

    /// A body naming a set through `scope_ids`, the shape the WebUI sends since ADR-078.
    fn body_multi(scope_level: &str, ids: &[&str]) -> ThresholdBody {
        ThresholdBody {
            scope_level: scope_level.to_owned(),
            scope_id: None,
            scope_ids: Some(ids.iter().map(|s| (*s).to_owned()).collect()),
            metric: "icmp_rtt_ms".to_owned(),
            direction: "above".to_owned(),
            warning: None,
            critical: None,
            dwell_samples: None,
        }
    }

    #[test]
    fn a_global_rule_has_its_scope_id_pinned_empty_and_every_other_level_keeps_what_was_sent() {
        // The pinning half: a stray id on a global rule would produce two rules that both apply to
        // every node, look identical in the list, and cannot be told apart.
        let b = body("global", "not-a-real-id", "icmp_rtt_ms", "above");
        assert!(parse_threshold_body(&b).unwrap().scope_ids.is_empty());
        // The receiving half, and it is the one that makes the test mean something: a normalizer
        // that emptied every scope id would pass the assertion above while silently making every
        // profile/group/node rule fleet-wide.
        let id = Uuid::from_u128(7).to_string();
        for (level, sent) in [
            ("profile", id.as_str()),
            ("group", "keep-me"),
            ("group_id", id.as_str()),
            ("node", id.as_str()),
        ] {
            let b = body(level, sent, "icmp_rtt_ms", "above");
            let p = parse_threshold_body(&b).unwrap();
            assert_eq!(p.scope_ids, vec![sent.to_owned()], "{level}");
            assert_eq!(p.scope_level, level);
        }
    }

    #[test]
    fn a_scope_id_that_cannot_match_anything_is_refused_instead_of_stored() {
        // Before this check a malformed id was accepted, listed, and evaluated for no node at all
        // — `applies` compares it against a uuid or a tag and simply never matches. A rule that
        // exists and does nothing is worse than one that was refused, because the operator has
        // been told it is in place.
        let id = Uuid::from_u128(9).to_string();
        for level in ["profile", "group_id", "node"] {
            for bad in ["", "not-a-uuid", "00000000-0000-0000-0000"] {
                let err =
                    parse_threshold_body(&body(level, bad, "icmp_rtt_ms", "above")).unwrap_err();
                assert!(
                    format!("{err:?}").contains("invalid_scope_id"),
                    "{level} accepted {bad:?}"
                );
            }
            // The accepting side — without it, a validator that refused everything would pass.
            assert!(parse_threshold_body(&body(level, &id, "icmp_rtt_ms", "above")).is_ok());
        }
        // A tag value is free text, so only emptiness is checkable. Both directions again.
        assert!(parse_threshold_body(&body("group", "  ", "icmp_rtt_ms", "above")).is_err());
        assert!(parse_threshold_body(&body("group", "osaka", "icmp_rtt_ms", "above")).is_ok());
        // And `global` still keeps its exemption: it has nothing to point at.
        assert!(parse_threshold_body(&body("global", "", "icmp_rtt_ms", "above")).is_ok());
    }

    #[test]
    fn a_token_outside_either_vocabulary_is_refused_rather_than_quietly_defaulted() {
        // Dropping an unknown token would *widen* the rule: `parse_level` on the read side falls
        // back to `Profile`, so a rule stored with a level nobody admits would come back as a
        // profile rule with an empty scope id — inert, and indistinguishable from a typo.
        for (level, dir) in [
            ("galaxy", "above"),
            ("global", "sideways"),
            ("", "above"),
            ("node", ""),
        ] {
            let b = body(level, "x", "icmp_rtt_ms", dir);
            let err = parse_threshold_body(&b).unwrap_err();
            assert!(
                format!("{err:?}").contains("invalid_threshold"),
                "{level}/{dir}"
            );
        }
        // A metric name that is not an identifier is refused by its own code, so the operator is
        // told which field is wrong.
        let b = body("node", "x", "not a metric", "above");
        let err = parse_threshold_body(&b).unwrap_err();
        assert!(format!("{err:?}").contains("invalid_metric_name"));
    }

    #[test]
    fn create_and_update_run_the_same_two_checks_in_the_same_order() {
        // Two writers of one rule is how the DNS/URL CRUD pair shipped with the "a node is exactly
        // one kind" guard on only one of them (extensibility.md §3). Both handlers must call the
        // shared validator *and* the counter check, and the counter check must come second — it is
        // a database round trip, and a malformed body should not cost one.
        const SRC: &str = include_str!("thresholds.rs");
        for handler in ["async fn create_threshold", "async fn update_threshold"] {
            let after = SRC.split_once(handler).expect("handler exists").1;
            let body = after.split_once("\n}").map_or(after, |(b, _)| b);
            let parse = body.find("parse_threshold_body(&body)").expect(handler);
            let counter = body.find("reject_counter_metric(").expect(handler);
            assert!(parse < counter, "{handler} must validate before it queries");
        }
    }

    /// ADR-078: a set of targets survives validation intact.
    #[test]
    fn a_multi_target_rule_keeps_every_target_it_was_given() {
        let a = Uuid::from_u128(11).to_string();
        let b = Uuid::from_u128(12).to_string();
        let sent = body_multi("profile", &[&a, &b]);
        let p = parse_threshold_body(&sent).unwrap();
        assert_eq!(p.scope_ids, vec![a.clone(), b.clone()]);
        // And the legacy single field still works, because that is what a caller written before
        // ADR-078 sends. `scope_ids` winning when both arrive is the other half of that rule.
        let mut legacy = body_multi("profile", &[&a]);
        legacy.scope_ids = None;
        legacy.scope_id = Some(b.clone());
        assert_eq!(parse_threshold_body(&legacy).unwrap().scope_ids, vec![b]);
    }

    /// Every way a target set can be wrong — plus one that is right, so "refuse everything" fails.
    #[test]
    fn a_target_set_is_refused_when_empty_duplicated_over_the_cap_or_a_pair_of_ports() {
        let id = Uuid::from_u128(13).to_string();
        let refused = |b: &ThresholdBody| {
            format!("{:?}", parse_threshold_body(b).unwrap_err()).contains("invalid_scope_id")
        };

        // Nothing named at a level that needs a target: the rule would be stored, listed, and
        // match no node — the failure ADR-075 決定 12 closed for the single-id case.
        assert!(refused(&body_multi("profile", &[])));

        // The same target twice would list as covering two things while covering one.
        assert!(refused(&body_multi("profile", &[&id, &id])));

        // Over the cap. `threshold_applies` walks this set for every sample.
        let many: Vec<String> = (0..=MAX_SCOPE_IDS)
            .map(|i| Uuid::from_u128(100 + i as u128).to_string())
            .collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        assert!(refused(&body_multi("profile", &refs)));

        // Two ports in one rule. No screen can create or edit that shape, so accepting it would
        // store a rule only a hand-written call could have made and nothing can show.
        let ports = [
            format!("{}:7", Uuid::from_u128(14)),
            format!("{}:8", Uuid::from_u128(14)),
        ];
        assert!(refused(&body_multi(
            "interface",
            &[ports[0].as_str(), ports[1].as_str()]
        )));

        // The accepting side — without it every assertion above would pass on a validator that
        // refused everything.
        assert!(parse_threshold_body(&body_multi("profile", &[&id])).is_ok());
        assert!(parse_threshold_body(&body_multi("interface", &[ports[0].as_str()])).is_ok());
        // A `global` rule names nothing and is still fine — the emptiness check must not reach it.
        assert!(parse_threshold_body(&body_multi("global", &[])).is_ok());
    }

    #[test]
    fn the_page_limit_is_clamped_to_the_cap_in_both_directions() {
        // The clamp is the DoS guard: `?limit=` is operator-supplied, so an unbounded or zero/
        // negative value must never reach the query.
        let clamp = |n: Option<i64>| n.unwrap_or(THRESHOLDS_MAX).clamp(1, THRESHOLDS_MAX);
        assert_eq!(clamp(None), THRESHOLDS_MAX);
        assert_eq!(clamp(Some(10)), 10);
        assert_eq!(clamp(Some(0)), 1);
        assert_eq!(clamp(Some(-5)), 1);
        assert_eq!(clamp(Some(i64::MAX)), THRESHOLDS_MAX);
    }

    #[test]
    fn truncation_is_derived_from_the_total_not_from_a_full_page() {
        // A page that happens to be exactly `limit` long is not truncated; the server says so
        // rather than leaving the client to infer it from `items.len() == limit`, which is wrong
        // whenever the ruleset is exactly the cap size.
        let truncated = |total: i64, got: usize| total > i64::try_from(got).unwrap_or(i64::MAX);
        assert!(!truncated(500, 500));
        assert!(truncated(501, 500));
        assert!(!truncated(0, 0));
    }
}
