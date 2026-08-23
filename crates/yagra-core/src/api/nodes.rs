// SPDX-License-Identifier: AGPL-3.0-only
//! The node domain: the inventory itself.
//!
//! Everything about a node's identity and its place in the two trees it belongs to — the
//! **folder tree** (`group_id`, `sort_order`, drag placement) and the **dependency tree**
//! (`parent_id`, the edge alert suppression walks). Those are genuinely different graphs over the
//! same nodes, which is why `PUT /nodes/:id/group` and `PUT /nodes/:id/parent` are separate
//! endpoints rather than one "move" — conflating them would make reordering the sidebar rewire
//! root-cause attribution.
//!
//! Two things here are shared rather than local, and both are shared for the same reason: they
//! answer a question more than one surface asks, and the surfaces had drifted.
//!
//!  - [`display_state`] / [`display_states`] — "what state do we show for this node". The alert
//!    engine's opinion when it has one, a coarse recent-RTT probe when it does not. The REST list
//!    applied that fallback and the MCP tools did not, so a just-added node read `ok` on the
//!    dashboard and `unknown` to an AI client asking the same question.
//!  - [`poll_now`] — routing a manual poll to the node's *effective* pool, which may be inherited
//!    from its folder. Resolving that on one surface only publishes to a subject no poller is
//!    listening on, and nothing reports an error.
//!
//! Two node-adjacent reads deliberately live elsewhere: `GET /nodes/:id/assignment` stays with the
//! Pollers view in [`super`] (it answers "which poller holds this", sharing that view's resolution
//! helpers), and `GET /nodes/:id/interfaces` belongs to metrics.

use super::extract::{Admin, RequireManageConfig, RequireView, Scoped, VisibleNode};
use super::util::CreatedId;
use super::{pool_resolver, AdminState, ApiError, ApiResult, ApiState};
use crate::groups::{placement_order, would_create_cycle};
use axum::{
    extract::{Path, Query},
    http::StatusCode,
    routing::{get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::time::Instant;
use uuid::Uuid;
use yagra_common::{DnsCheckConfig, Node, NodeId, NodeKind, NodeRows, NodeState, UrlCheckConfig};

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    list_nodes,
    create_node,
    search_nodes,
    list_group_nodes,
    node_names_batch,
    get_node,
    delete_node,
    get_node_status,
    poll_node_now,
    set_node_bindings,
    set_node_group,
    set_node_pool,
    set_node_parent,
    set_node_suppression_opt_out,
    place_node,
    list_pools
))]
pub(super) struct Doc;

/// The node routes, merged into `/api/v1` by [`super::router`].
pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/nodes", get(list_nodes).post(create_node))
        // The static segments must be registered before `/nodes/:node_id`. `matchit` prioritizes a
        // literal over a parameter regardless of order, but keeping them adjacent is what stops
        // someone "tidying" `search` below the param route and assuming it still works.
        .route("/api/v1/nodes/search", get(search_nodes))
        .route("/api/v1/nodes/by-group", get(list_group_nodes))
        .route("/api/v1/node-names", post(node_names_batch))
        .route("/api/v1/nodes/:node_id", get(get_node).delete(delete_node))
        .route("/api/v1/nodes/:node_id/status", get(get_node_status))
        .route("/api/v1/nodes/:node_id/poll", post(poll_node_now))
        .route("/api/v1/nodes/:node_id/bindings", put(set_node_bindings))
        .route("/api/v1/nodes/:node_id/group", put(set_node_group))
        .route("/api/v1/nodes/:node_id/pool", put(set_node_pool))
        .route("/api/v1/nodes/:node_id/parent", put(set_node_parent))
        .route(
            "/api/v1/nodes/:node_id/suppression-opt-out",
            put(set_node_suppression_opt_out),
        )
        .route("/api/v1/nodes/:node_id/placement", put(place_node))
        .route("/api/v1/pools", get(list_pools))
}

// ── Display state: the one answer to "how is this node doing?" ───────────────

/// Freshness window for the coarse fallback probe: a node with a liveness sample within this
/// window is treated as `ok`, else `unknown` (matches the fleet-coverage staleness horizon).
const FALLBACK_FRESH_SECS: u64 = 600;

/// The metrics the fallback probe asks about: **every node kind's liveness series**, because a URL
/// monitor, a DNS monitor and a Meraki device are never pinged and so have no `icmp_rtt_ms` at all.
/// Asking only about ICMP made those three kinds fall to `unknown` whenever the engine had no
/// opinion yet — the same defect that made fleet coverage report them as silent (ADR-059).
///
/// The union answers without resolving each node's kind, which would put three database reads on
/// the node-list path for an answer that is identical either way.
const FALLBACK_METRICS: [&str; NodeKind::ALL.len()] = NodeKind::LIVENESS_METRICS;

/// **The display rule itself**: the engine's opinion when it has one, otherwise a recent liveness
/// sample means `ok` and silence means `unknown`.
///
/// Pure — every caller brings its own already-batched inputs, and nothing here does I/O. It is a
/// function rather than three lines because it *was* three lines, four times over: the topology
/// graph, the fleet tally, the per-group rollup and the inventory report each restated it, and two
/// of them had dropped the fallback entirely. The visible symptom was a core restart making the
/// dashboard summary report `unknown` for nodes the Nodes page was simultaneously showing as `ok`.
pub(crate) fn state_or_fallback(known: Option<NodeState>, fresh: bool) -> NodeState {
    match known {
        Some(s) => s,
        None if fresh => NodeState::Ok,
        None => NodeState::Unknown,
    }
}

/// The rolled-up state to display for one node.
///
/// The alert engine's opinion when it has one. When it does not — a just-added node, or right
/// after a core restart before the first sweep — fall back to a recent liveness sample meaning
/// `ok`. **Every surface that reports a node's state must go through this or [`display_states`]**,
/// or the same node reads differently depending on which endpoint you ask.
pub(crate) async fn display_state(st: &ApiState, node: NodeId) -> NodeState {
    let known = st.alerts.node_state(node);
    // One node, one round-trip — and only when the engine has nothing to say. Anything holding a
    // page of nodes takes `display_states` instead, which asks once for the whole page (S20).
    //
    // The scoped freshness probe rather than `latest`: `latest` has **no window**, so this path
    // called a node `ok` off a sample from any time in history while the paged path beside it
    // required one inside `FALLBACK_FRESH_SECS`. One rule, two implementations, two answers about
    // the same node depending on which endpoint was asked (ADR-059 decision 4).
    let fresh = known.is_none()
        && !st
            .store
            .fresh_node_ids_scoped(&FALLBACK_METRICS, FALLBACK_FRESH_SECS, &[node.as_uuid()])
            .await
            .is_empty();
    state_or_fallback(known, fresh)
}

/// [`display_state`] for a page of nodes, in one TSDB query instead of N.
///
/// The engine already holds a state for most nodes; only the unobserved remainder needs the probe,
/// and asking `latest()` per node is N sequential HTTP round-trips — worst right after a restart,
/// when `states` is empty and *every* node takes that path. So the unobserved set is answered with
/// a single scoped freshness query (S20 — scoped to this page, never the whole fleet), and the
/// query is skipped entirely when nothing is unobserved, which is the steady state.
pub(crate) async fn display_states(st: &ApiState, nodes: &[NodeId]) -> HashMap<NodeId, NodeState> {
    let known = st.alerts.node_states();
    let unobserved: Vec<Uuid> = nodes
        .iter()
        .filter(|n| !known.contains_key(n))
        .map(NodeId::as_uuid)
        .collect();
    let fresh: HashSet<Uuid> = if unobserved.is_empty() {
        HashSet::new()
    } else {
        st.store
            .fresh_node_ids_scoped(&FALLBACK_METRICS, FALLBACK_FRESH_SECS, &unobserved)
            .await
            .into_iter()
            .collect()
    };
    nodes
        .iter()
        .map(|n| {
            (
                *n,
                state_or_fallback(known.get(n).copied(), fresh.contains(&n.as_uuid())),
            )
        })
        .collect()
}

/// The subset of `unobserved` with a recent RTT sample (⇒ `ok`).
///
/// The raw form of the batched probe, for a caller that already has its own state map and only
/// needs the fallback (the topology graph).
pub(crate) async fn fresh_fallback_ids(st: &ApiState, unobserved: &[NodeId]) -> HashSet<Uuid> {
    if unobserved.is_empty() {
        return HashSet::new();
    }
    let scope: Vec<Uuid> = unobserved.iter().map(NodeId::as_uuid).collect();
    st.store
        .fresh_node_ids_scoped(&FALLBACK_METRICS, FALLBACK_FRESH_SECS, &scope)
        .await
        .into_iter()
        .collect()
}

/// The **whole fleet's** fresh set, for the rollups that hold counts rather than a page of ids.
///
/// [`fresh_fallback_ids`] pushes its id set into the query selector, which is right for a page and
/// wrong for a rollup: the fleet tally, the per-group summary and the inventory report each cover
/// every visible node, and a selector carrying 50,000 UUIDs is not a query. So they share one
/// unscoped freshness query instead — and every caller skips it entirely unless something is
/// actually unobserved, which is the steady state (the engine holds an opinion about every node it
/// has swept). Takes the store rather than `ApiState` because the report renderer has no `ApiState`.
pub(crate) async fn fresh_fleet_ids(store: &dyn crate::store::MetricStore) -> HashSet<Uuid> {
    store
        .fresh_node_ids(&FALLBACK_METRICS, FALLBACK_FRESH_SECS)
        .await
        .into_iter()
        .collect()
}

// ── Reads ────────────────────────────────────────────────────────────────────

/// One inventory row (mirrors the WebUI `NodeSummary`).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct NodeSummary {
    id: NodeId,
    name: String,
    address: String,
    state: NodeState,
    /// Descriptive maker/model for the "name (addr) (vendor) (model)" display.
    vendor: Option<String>,
    model: Option<String>,
    /// The group this node belongs to (for the inventory tree); `null` ⇒ ungrouped.
    group_id: Option<Uuid>,
    /// Manual order within the group (the tree sorts members by this, then by name).
    sort_order: f64,
    /// The node's **own** poll-pool; `null` ⇒ inherited from its folder, else the default pool.
    /// The tree's pool picker edits exactly this value, so it is what marks the active choice —
    /// the *effective* pool (and the poller holding the node) comes from `/nodes/:id/assignment`.
    pool: Option<String>,
    /// **What this node is**, and therefore how it is polled — the value that distinguishes a URL
    /// or DNS monitor from an ordinary ICMP/SNMP device in the inventory.
    ///
    /// Resolved by `NodeKind::resolve`, the same function `GET /nodes/{id}` and the scheduler ask,
    /// so a list row can never disagree with the detail page it opens.
    kind: NodeKind,
}

/// One keyset page of the inventory.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct NodePage {
    nodes: Vec<NodeSummary>,
    /// Pass back as `cursor` for the next page; `null` ⇒ this was the last one. Always `null` in
    /// filter mode, which returns a single capped page by design.
    next_cursor: Option<String>,
    /// Filter mode only: matches exist that this answer does not contain, because the page hit its
    /// cap or the candidate scan hit its ceiling. Always `false` while paging.
    ///
    /// A separate field rather than something the client infers from `nodes.len()`, because once
    /// the server rejects candidates after the query those two stop meaning the same thing: a
    /// filter that scans 5,000 rows and keeps 3 returns three rows and is still incomplete.
    truncated: bool,
}

/// One group's direct members. Not keyset-paged — a folder is loaded whole when it is expanded —
/// so it reports truncation instead of offering a cursor.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct GroupNodes {
    nodes: Vec<NodeSummary>,
    truncated: bool,
}

/// Keyset pagination query for the node list. Any of `search` / `state` / `kind` / `pool`
/// switches the endpoint into **filter mode** — capped, single page, no cursor — so the Nodes
/// tree's filter never full-loads the fleet into the browser (ui-conventions: search is
/// server-side at scale). Both modes return full [`NodeSummary`] rows so the tree can nest and
/// colour them.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct NodePageQuery {
    pub cursor: Option<Uuid>,
    pub limit: Option<i64>,
    /// Case-insensitive substring of the node's name or address.
    pub search: Option<String>,
    /// Comma-separated display states (`ok` | `warning` | `critical` | `unknown` | `unreachable` |
    /// `maintenance`); empty or absent means every state. An unknown token is rejected rather than
    /// ignored.
    pub state: Option<String>,
    /// Comma-separated monitoring kinds (`meraki` | `url` | `dns` | `device`); empty or absent
    /// means every kind.
    pub kind: Option<String>,
    /// Comma-separated **effective** poll pools — a node's own pool when it sets one, otherwise the
    /// nearest folder ancestor that does, otherwise the default pool. Filtering on the stored column
    /// alone would miss every node that inherits, which is most of them. Pools are named by the
    /// operator, so there is no vocabulary to reject against: an unknown name simply matches
    /// nothing.
    pub pool: Option<String>,
}

/// Why three of the four filters are applied in-process rather than as a `WHERE` clause — and it
/// is not for want of trying:
///
/// - **state** is not in PostgreSQL at all. It lives in the alert engine's in-memory map, with a
///   TSDB freshness probe as the fallback for nodes the engine has never observed.
/// - **kind** is not a stored column either — it is derived from which single-purpose side table
///   carries a row, and the precedence lives in exactly one place ([`NodeKind::resolve`]). A `CASE`
///   in SQL would be a second answer to "what is this node", which is the thing that type exists to
///   prevent.
/// - **pool** is inherited from the folder tree and deliberately not materialized (ADR-013), so the
///   answer is [`crate::poolres::PoolResolver`]'s, not a column's.
///
/// So they run over a bounded candidate scan ([`crate::repo::NODE_SCAN_MAX`]), through the same
/// resolvers every other surface asks. What that buys is that a row can never disagree with the
/// filter that selected it. What it costs is the bound: past it the answer is a subset, and
/// `NodePage::truncated` says so rather than letting "no matches" mean "none among the first N".
///
/// Each field is a **set**, empty meaning unfiltered (ADR-053 Inc.6). Selecting several states is
/// how an operator asks the question this screen is for — "show me everything that is not healthy"
/// is three states, and with one value per filter it was three separate looks at the tree.
#[derive(Debug, Default, Clone)]
pub(crate) struct NodeFilter {
    pub state: Vec<NodeState>,
    pub kind: Vec<NodeKind>,
    pub pool: Vec<String>,
}

impl NodeFilter {
    /// Whether anything at all is set. `search` is handled separately — it is the one filter SQL
    /// can serve, so it narrows the scan rather than the survivors.
    pub(crate) fn is_set(&self) -> bool {
        !self.state.is_empty() || !self.kind.is_empty() || !self.pool.is_empty()
    }
}

/// Filter mode's page: the candidate scan, the in-process filters, and whether matches were left
/// out. Returns `(rows, truncated)`.
///
/// Shared by `GET /api/v1/nodes` and the MCP `list_nodes` tool (ADR-042 read parity), because the
/// two answering differently about *which nodes match* is exactly the drift the parity rule
/// exists to stop — and it would be invisible, since each answer looks internally consistent.
pub(crate) async fn filtered_node_page(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    term: &str,
    filter: &NodeFilter,
    limit: i64,
) -> Result<(Vec<Node>, bool), ApiError> {
    // The scan is only widened past one page when something has to reject candidates after the
    // query. A plain text search rejects nothing, so it stays exactly as cheap as it was.
    let scan = if filter.is_set() {
        crate::repo::NODE_SCAN_MAX
    } else {
        limit
    };
    let candidates = st
        .nodes
        .search(scope.group_filter(), term, scan)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "search nodes for list", "failed to list nodes")
        })?;
    let scan_hit_ceiling = i64::try_from(candidates.len()).unwrap_or(i64::MAX) >= scan;
    let mut nodes = apply_node_filter(st, candidates, filter).await;
    let over_page = i64::try_from(nodes.len()).unwrap_or(i64::MAX) > limit;
    if over_page {
        nodes.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    Ok((nodes, scan_hit_ceiling || over_page))
}

/// Which single-purpose rows each of the given nodes carries, resolved into a [`NodeKind`].
///
/// Page-scoped: each read is a `WHERE node_id = ANY($1)` over the ids on this page, not a
/// full-table scan of every monitor in the fleet. A node absent from all three sets is a
/// `Device` — which is also what a *failed* read degrades to, matching [`get_node`] and the
/// scheduler, so a transient database error cannot make the list and the detail page disagree
/// about what a node is.
///
/// Shared with the MCP `list_nodes` / `get_node_status` tools (ADR-042 read parity) so the two
/// surfaces answer "what is this node" from one place rather than two.
pub(crate) async fn node_kinds(admin: &AdminState, ids: &[Uuid]) -> HashMap<Uuid, NodeKind> {
    let (meraki, url, dns) = tokio::join!(
        async {
            admin
                .meraki_devices
                .filter_meraki(ids)
                .await
                .unwrap_or_default()
        },
        async { admin.url_checks.filter_url(ids).await.unwrap_or_default() },
        async { admin.dns_checks.filter_dns(ids).await.unwrap_or_default() },
    );
    resolve_kinds(ids, &meraki, &url, &dns)
}

/// The pure half of [`node_kinds`]: three membership sets into one kind per id.
///
/// Split out so the precedence can be tested without a database. It must stay a *call* to
/// `NodeKind::resolve` — writing the `if meraki … else if url …` chain here would be a second
/// answer to "what is this node", which is the thing `NodeKind` exists to prevent.
fn resolve_kinds(
    ids: &[Uuid],
    meraki: &HashSet<Uuid>,
    url: &HashSet<Uuid>,
    dns: &HashSet<Uuid>,
) -> HashMap<Uuid, NodeKind> {
    ids.iter()
        .map(|id| {
            let kind = NodeKind::resolve(NodeRows {
                meraki: meraki.contains(id),
                url: url.contains(id),
                dns: dns.contains(id),
            });
            (*id, kind)
        })
        .collect()
}

/// Enrich raw `Node` rows into UI [`NodeSummary`] rows: live display state, tree sort order, and
/// the node's resolved kind. Shared by the paged fleet list and the per-group lazy tree load so both
/// paths produce identical rows.
async fn build_node_summaries(st: &ApiState, nodes: Vec<Node>) -> Vec<NodeSummary> {
    let ids: Vec<Uuid> = nodes.iter().map(|n| n.id.as_uuid()).collect();
    let node_ids: Vec<NodeId> = nodes.iter().map(|n| n.id).collect();
    // Skeleton mode has neither ordering nor side tables; a read failure degrades the kind and the
    // ordering, never the list.
    let inventory = async {
        match st.admin.as_ref() {
            Some(admin) => tokio::join!(
                async { admin.repo.node_sort_orders(&ids).await.unwrap_or_default() },
                node_kinds(admin, &ids),
            ),
            None => (HashMap::new(), HashMap::new()),
        }
    };
    // Five independent reads — four PostgreSQL, one TSDB — on the hottest list in the product:
    // every tree page, every debounced search keystroke, every lazy folder expand runs all five.
    // None needs another's answer, so they overlap rather than queue (the shape `interface_heatmap`
    // uses for its per-link fan-out) and the wall clock is the slowest one, not their sum. The two
    // that `node_kinds` added are the price of the list agreeing with the detail page; the cheaper-
    // looking alternative — the three `node_ids()` full-table reads the scheduler uses — is
    // unbounded in how many monitors exist and returns rows this page cannot use.
    let ((orders, kinds), states) = tokio::join!(inventory, display_states(st, &node_ids));
    nodes
        .into_iter()
        .map(|n| NodeSummary {
            state: states.get(&n.id).copied().unwrap_or(NodeState::Unknown),
            sort_order: orders.get(&n.id.as_uuid()).copied().unwrap_or(0.0),
            kind: kinds
                .get(&n.id.as_uuid())
                .copied()
                .unwrap_or(NodeKind::Device),
            id: n.id,
            name: n.name,
            address: n.address.to_string(),
            vendor: n.vendor,
            model: n.model,
            group_id: n.group.map(|g| g.as_uuid()),
            pool: n.pool,
        })
        .collect()
}

#[utoipa::path(
    get, path = "/api/v1/nodes", tag = "nodes",
    params(NodePageQuery),
    responses(
        (status = 200, description = "One keyset page of the inventory, or a single capped page in search mode", body = NodePage),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the View permission", body = super::error::ErrorBody),
    ),
)]
async fn list_nodes(
    _perm: RequireView,
    Scoped(scope): Scoped,
    axum::extract::State(st): axum::extract::State<ApiState>,
    Query(q): Query<NodePageQuery>,
) -> ApiResult<Json<NodePage>> {
    let limit = q
        .limit
        .unwrap_or(100)
        .clamp(1, crate::repo::NODE_SEARCH_MAX);
    let term = q.search.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let filter = parse_node_filter(q.state.as_deref(), q.kind.as_deref(), q.pool.as_deref())?;
    // Filter mode: any of search / state / kind / pool returns a single capped page (no keyset
    // cursor) — the tree narrows the fleet without loading it.
    if term.is_some() || filter.is_set() {
        let (nodes, truncated) =
            filtered_node_page(&st, &scope, term.unwrap_or(""), &filter, limit).await?;
        return Ok(Json(NodePage {
            nodes: build_node_summaries(&st, nodes).await,
            next_cursor: None,
            truncated,
        }));
    }
    // Fetch one extra row to tell "exactly a full page" from "a full page with more after it",
    // so the client never makes a trailing request that returns an empty page at the boundary.
    // Scoping is a `WHERE` predicate, so it composes with the cursor rather than perturbing it —
    // a scoped page is simply shorter, and the cursor still advances by node id.
    let mut nodes = st
        .nodes
        .list_page(scope.group_filter(), q.cursor, limit + 1)
        .await
        .map_err(|e| ApiError::from_internal(e.as_ref(), "list nodes", "failed to list nodes"))?;
    let has_more = i64::try_from(nodes.len()).unwrap_or(i64::MAX) > limit;
    if has_more {
        nodes.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    let next_cursor = if has_more {
        nodes.last().map(|n| n.id.to_string())
    } else {
        None
    };
    Ok(Json(NodePage {
        nodes: build_node_summaries(&st, nodes).await,
        next_cursor,
        // Paging is not truncation: `next_cursor` already says there is more, and a client that
        // read both would show a "results were cut" notice on every page but the last.
        truncated: false,
    }))
}

/// Parse the three set filters, in one place, for both the REST edge and the MCP tool.
///
/// Same spelling as every other set in the API (ADR-053): comma-separated, empty means unfiltered,
/// an unknown token is a 400 rather than being dropped — a filter that silently widens is one an
/// operator reads as "there are no Meraki nodes".
///
/// ⚠️ **`pool` is the exception, and it is not an oversight.** State and kind are closed
/// vocabularies this build owns, so a token outside them is a mistake worth reporting. A pool is a
/// name the operator invented; there is no list to check against, and refusing an unrecognised one
/// would mean an empty pool could not be asked about.
pub(crate) fn parse_node_filter(
    state: Option<&str>,
    kind: Option<&str>,
    pool: Option<&str>,
) -> Result<NodeFilter, ApiError> {
    Ok(NodeFilter {
        state: super::util::parse_set_with(
            "invalid_state",
            "state",
            state,
            |s| {
                format!(
                    "unknown node state {s:?}; must be one of: {}",
                    NodeState::ALL
                        .iter()
                        .map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            },
            NodeState::from_token,
        )?,
        kind: super::util::parse_set_with(
            "invalid_kind",
            "kind",
            kind,
            |s| {
                format!(
                    "unknown node kind {s:?}; must be one of: {}",
                    NodeKind::ALL
                        .iter()
                        .map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            },
            NodeKind::from_token,
        )?,
        pool: pool
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect(),
    })
}

/// Keep the candidates that match `filter`, asking the one resolver that owns each question.
///
/// Each resolver is consulted only when its filter is set — a state filter must not pay for the
/// folder tree, and a pool filter must not pay for a TSDB freshness probe. `search` already
/// applied the caller's scope, so nothing here can widen it.
async fn apply_node_filter(st: &ApiState, nodes: Vec<Node>, filter: &NodeFilter) -> Vec<Node> {
    if !filter.is_set() {
        return nodes;
    }
    let ids: Vec<Uuid> = nodes.iter().map(|n| n.id.as_uuid()).collect();
    let node_ids: Vec<NodeId> = nodes.iter().map(|n| n.id).collect();
    let states = async {
        if filter.state.is_empty() {
            HashMap::new()
        } else {
            display_states(st, &node_ids).await
        }
    };
    let kinds = async {
        match (filter.kind.is_empty(), st.admin.as_ref()) {
            (false, Some(admin)) => node_kinds(admin, &ids).await,
            // Skeleton mode has no side tables, so every node is a plain device — which is what
            // `build_node_summaries` reports for the same rows, so the two still agree.
            _ => HashMap::new(),
        }
    };
    let pools = async {
        match (filter.pool.is_empty(), st.admin.as_ref()) {
            (false, Some(admin)) => Some(super::util::pool_resolver(admin).await),
            _ => None,
        }
    };
    let (states, kinds, pools) = tokio::join!(states, kinds, pools);
    nodes
        .into_iter()
        .filter(|n| {
            // An empty set is "no filter", never "match nothing" — the same rule the SQL sets use
            // (a NULL bind rather than an empty array, `analysis/repo.rs::search_findings`).
            if !filter.state.is_empty() {
                let have = states
                    .get(&n.id)
                    .copied()
                    .unwrap_or(yagra_common::NodeState::Unknown);
                if !filter.state.contains(&have) {
                    return false;
                }
            }
            if !filter.kind.is_empty() {
                let have = kinds
                    .get(&n.id.as_uuid())
                    .copied()
                    .unwrap_or(NodeKind::Device);
                if !filter.kind.contains(&have) {
                    return false;
                }
            }
            if !filter.pool.is_empty() {
                let have = pools
                    .as_ref()
                    .map_or(yagra_bus::DEFAULT_POOL, |r| r.resolve_pool(n));
                if !filter.pool.iter().any(|w| w == have) {
                    return false;
                }
            }
            true
        })
        .collect()
}

/// Query for the node-picker typeahead: `?q=<substr>&limit=<n>`. Empty/absent `q` returns the
/// first page ordered by name.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct NodeSearchQuery {
    q: Option<String>,
    limit: Option<i64>,
}

// The exclusion is security.md's rule; it stays in `//` because `///` on a `ToSchema` is published
// verbatim to API clients, who cannot open a file that is not in the repository.
/// One node-picker result: id + display name + address. Deliberately excludes credentials and
/// bindings — the picker only needs to show and select a node.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct NodeSearchResult {
    id: Uuid,
    name: String,
    address: String,
}

/// Server-side node search for the node-picker typeahead: case-insensitive substring over name or
/// address, capped, so a picker never loads the whole inventory into the browser (ui-conventions:
/// search is server-side at fleet scale). Also backs the Nodes tree name filter and the
/// Troubleshoot scope picker. Routes through the shared `NodeListing`, so it works in skeleton mode.
#[utoipa::path(
    get, path = "/api/v1/nodes/search", tag = "nodes",
    params(NodeSearchQuery),
    responses(
        (status = 200, description = "Matching nodes, capped", body = Vec<NodeSearchResult>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the View permission", body = super::error::ErrorBody),
    ),
)]
async fn search_nodes(
    _perm: RequireView,
    Scoped(scope): Scoped,
    axum::extract::State(st): axum::extract::State<ApiState>,
    Query(q): Query<NodeSearchQuery>,
) -> ApiResult<Json<Vec<NodeSearchResult>>> {
    let term = q.q.unwrap_or_default();
    let limit = q.limit.unwrap_or(50);
    let nodes = st
        .nodes
        .search(scope.group_filter(), term.trim(), limit)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "node search", "failed to search nodes")
        })?;
    Ok(Json(
        nodes
            .into_iter()
            .map(|n| NodeSearchResult {
                id: n.id.as_uuid(),
                name: n.name,
                address: n.address.to_string(),
            })
            .collect(),
    ))
}

/// Query for the per-group lazy tree load: `?group=<uuid>` returns that group's direct members; an
/// absent/empty `group` returns the ungrouped nodes (`group_id IS NULL`).
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct GroupNodesQuery {
    group: Option<Uuid>,
}

/// Backstop cap on one group's direct-member load. The inventory tree lazy-loads a group's members
/// only when it is expanded, so this bounds a single pathologically large group; the client flags a
/// truncated group. Normal groups are far below this.
const GROUP_NODES_CAP: i64 = 2000;

/// A group's direct members for the lazy inventory tree: the nodes whose `group_id` is exactly
/// `group` (or the ungrouped bucket), in tree order, capped. Loaded on demand when a group is
/// expanded, so the initial page never pulls the whole fleet — it fetches the group skeleton plus
/// per-group counts (`/fleet/group-summary`) and streams members per open group.
#[utoipa::path(
    get, path = "/api/v1/nodes/by-group", tag = "nodes",
    params(GroupNodesQuery),
    responses(
        (status = 200, description = "The group's direct members in tree order, flagged if capped", body = GroupNodes),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the View permission", body = super::error::ErrorBody),
    ),
)]
async fn list_group_nodes(
    _perm: RequireView,
    Scoped(scope): Scoped,
    axum::extract::State(st): axum::extract::State<ApiState>,
    Query(q): Query<GroupNodesQuery>,
) -> ApiResult<Json<GroupNodes>> {
    let Some(admin) = st.admin.as_ref() else {
        // Skeleton mode has no group membership; the demo node is ungrouped. Return it for the
        // ungrouped bucket, nothing for a specific group. A scoped caller gets nothing either way:
        // an ungrouped node is outside every group scope, which `search` enforces for us.
        let nodes = if q.group.is_none() {
            st.nodes
                .search(scope.group_filter(), "", GROUP_NODES_CAP)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        return Ok(Json(GroupNodes {
            nodes: build_node_summaries(&st, nodes).await,
            truncated: false,
        }));
    };
    let mut nodes = admin
        .repo
        .list_nodes_in_group(scope.group_filter(), q.group, GROUP_NODES_CAP + 1)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "list group nodes", "failed to load group nodes")
        })?;
    let truncated = i64::try_from(nodes.len()).unwrap_or(i64::MAX) > GROUP_NODES_CAP;
    if truncated {
        nodes.truncate(usize::try_from(GROUP_NODES_CAP).unwrap_or(usize::MAX));
    }
    Ok(Json(GroupNodes {
        nodes: build_node_summaries(&st, nodes).await,
        truncated,
    }))
}

/// Cap on one batch name-resolution request so a client can't force an unbounded `IN (…)` query.
const NODE_NAMES_BATCH_MAX: usize = 1000;

/// Request body for `POST /api/v1/node-names`: the node ids whose display names to resolve.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct NodeNamesReq {
    ids: Vec<Uuid>,
}

/// One resolved node id → display name (unresolved ids are omitted; the caller keeps the raw id).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct NodeNameEntry {
    id: Uuid,
    name: String,
}

/// Resolve a batch of node ids to their display names. The shared `useEntityNames` resolver, and
/// any table rendering a node reference by id, use this so names resolve across the **whole**
/// fleet: the old path resolved against the first page of `list_nodes` (default 100), so a
/// reference to the 101st node silently degraded to a raw UUID.
#[utoipa::path(
    post, path = "/api/v1/node-names", tag = "nodes",
    request_body = NodeNamesReq,
    responses(
        (status = 200, description = "The resolved names; an id with no row is omitted", body = Vec<NodeNameEntry>),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the View permission", body = super::error::ErrorBody),
    ),
)]
async fn node_names_batch(
    _perm: RequireView,
    Scoped(scope): Scoped,
    axum::extract::State(st): axum::extract::State<ApiState>,
    Json(req): Json<NodeNamesReq>,
) -> ApiResult<Json<Vec<NodeNameEntry>>> {
    let mut ids = req.ids;
    ids.truncate(NODE_NAMES_BATCH_MAX);
    let names = resolve_node_names(&st, &scope, ids.iter().copied()).await;
    Ok(Json(
        ids.iter()
            .filter_map(|id| {
                names.get(id).map(|name| NodeNameEntry {
                    id: *id,
                    name: name.clone(),
                })
            })
            .collect(),
    ))
}

/// Resolve node ids → display names.
///
/// This is the ADR-011 join, and it is shared because the rule it encodes is easy to state and
/// easy to get subtly wrong three separate times: **the TSDB carries only node ids**, so anything
/// ranked or sampled out of it has to come back to PostgreSQL for a name. It is best-effort by
/// design — skeleton mode has no repo, and a node deleted since the sample was written has no row
/// — so a missing id is simply absent from the map and the caller falls back to the id string. A
/// Top-N that fails because one node was deleted mid-query would be worse than one that shows a
/// UUID.
///
/// Ids are sorted and deduplicated first: a ranking that mentions the same node twice must not
/// widen the `IN (…)`, and an empty input skips the query entirely.
///
/// **Scoped.** This is both the internal join and the resolver behind `POST /api/v1/node-names`,
/// where the caller supplies the ids — so unscoped it would answer "what is node `<uuid>` called"
/// for the whole fleet. An out-of-scope id is omitted exactly like an unknown one, so the caller's
/// existing id-string fallback covers it and the two cases stay indistinguishable from outside.
pub(crate) async fn resolve_node_names(
    st: &ApiState,
    scope: &super::scope::NodeScope,
    ids: impl IntoIterator<Item = Uuid>,
) -> HashMap<Uuid, String> {
    let mut ids: Vec<Uuid> = ids.into_iter().collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return HashMap::new();
    }
    match st.admin.as_ref() {
        Some(admin) => admin
            .repo
            .node_names(scope.group_filter(), &ids)
            .await
            .unwrap_or_default(),
        None => HashMap::new(),
    }
}

/// One node's configuration detail, including its bindings (profile/credential/parent) so the
/// node-detail page can show and edit them. Live mode only (PostgreSQL inventory).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct NodeDetail {
    id: NodeId,
    name: String,
    address: String,
    profile_id: Option<Uuid>,
    credential_id: Option<Uuid>,
    parent_id: Option<Uuid>,
    /// Descriptive maker/model, editable from the node detail.
    vendor: Option<String>,
    model: Option<String>,
    /// The group this node belongs to; `null` ⇒ ungrouped.
    group_id: Option<Uuid>,
    /// The node's **own** poll-pool (ADR-009/020); `null` ⇒ it inherits from its folder, else the
    /// default pool. Deliberately the raw stored value, not the effective one, so the edit form can
    /// tell an explicit assignment from an inherited one — the *effective* pool (and which poller
    /// currently holds the node) comes from `GET /nodes/:id/assignment`.
    pool: Option<String>,
    /// **What this node is** — the kind the scheduler actually polls it as, resolved by the one
    /// precedence in [`NodeKind::resolve`].
    ///
    /// The three configs below are the raw rows, and a node is not guaranteed to carry only one:
    /// the API edge refuses a second, but rows predating that guard exist. Reading the configs and
    /// concluding a kind from whichever is non-null is how the node page came to show a URL-monitor
    /// health card for a node the poller was treating as a Meraki device. Branch on this instead.
    kind: NodeKind,
    /// URL-monitor config when this node carries a `url_checks` row; `null` otherwise.
    url_check: Option<UrlCheckConfig>,
    /// DNS-monitor config when this node carries a `dns_checks` row; `null` otherwise.
    dns_check: Option<DnsCheckConfig>,
    /// Cisco Meraki binding when this node carries a `meraki_devices` row; `null` otherwise.
    meraki_device: Option<yagra_common::MerakiDeviceConfig>,
}

#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    responses(
        (status = 200, description = "The node's configuration, bindings and resolved kind", body = NodeDetail),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the View permission", body = super::error::ErrorBody),
        (status = 404, description = "No such node, or this deployment has no inventory", body = super::error::ErrorBody),
    ),
)]
async fn get_node(
    _perm: RequireView,
    _visible: VisibleNode,
    axum::extract::State(st): axum::extract::State<ApiState>,
    Path(node_id): Path<Uuid>,
) -> ApiResult<Json<NodeDetail>> {
    // Not the `Admin` extractor: skeleton mode has no inventory at all, so "no write side" and
    // "no such node" are the same answer to this question, and 404 is the truthful one.
    let missing = || ApiError::not_found("node_not_found", format!("no node {node_id}"));
    let admin = st.admin.as_ref().ok_or_else(missing)?;
    let node = admin
        .repo
        .get_node(node_id)
        .await
        .map_err(|e| ApiError::from_internal(e.as_ref(), "get node", "failed to load node"))?
        .ok_or_else(missing)?;
    // Best-effort: a URL/DNS-check or Meraki load failure shouldn't fail the node detail. A failed
    // lookup reads as "no row", which degrades the resolved kind toward `Device` — the same way the
    // scheduler degrades on the same failure, so the two still agree.
    let url_check = admin.url_checks.get(node_id).await.unwrap_or(None);
    let dns_check = admin.dns_checks.get(node_id).await.unwrap_or(None);
    let meraki_device = admin.meraki_devices.get(node_id).await.unwrap_or(None);
    Ok(Json(NodeDetail {
        kind: NodeKind::resolve(NodeRows {
            meraki: meraki_device.is_some(),
            url: url_check.is_some(),
            dns: dns_check.is_some(),
        }),
        url_check,
        dns_check,
        meraki_device,
        id: node.id,
        name: node.name,
        address: node.address.to_string(),
        profile_id: node.profile.map(|p| p.0),
        credential_id: node.credential.map(|c| c.as_uuid()),
        parent_id: node.parent.map(|p| p.as_uuid()),
        vendor: node.vendor,
        model: node.model,
        group_id: node.group.map(|g| g.as_uuid()),
        pool: node.pool,
    }))
}

/// One node's live status: its display state plus the alerts currently attributed to it, so node
/// detail can show *why* it is down without re-deriving from the list.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct NodeStatus {
    node_id: NodeId,
    state: NodeState,
    alerts: Vec<yagra_alert::Alert>,
}

/// Assemble one node's live status. Shared with the MCP `get_node_status` tool so both surfaces
/// apply the same fallback — see [`display_state`].
pub(crate) async fn node_status(st: &ApiState, node_id: Uuid) -> NodeStatus {
    let node = NodeId::from(node_id);
    NodeStatus {
        node_id: node,
        state: display_state(st, node).await,
        alerts: st.alerts.alerts_for(node),
    }
}

#[utoipa::path(
    get, path = "/api/v1/nodes/{node_id}/status", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    responses(
        (status = 200, description = "The node's display state and the alerts attributed to it", body = NodeStatus),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the View permission", body = super::error::ErrorBody),
    ),
)]
async fn get_node_status(
    _perm: RequireView,
    _visible: VisibleNode,
    axum::extract::State(st): axum::extract::State<ApiState>,
    Path(node_id): Path<Uuid>,
) -> ApiResult<Json<NodeStatus>> {
    Ok(Json(node_status(&st, node_id).await))
}

// ── Writes ───────────────────────────────────────────────────────────────────

/// Create-node request body. `profile_id`/`credential_id`/`parent_id` are optional.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct CreateNode {
    name: String,
    address: String,
    pool: Option<String>,
    profile_id: Option<Uuid>,
    credential_id: Option<Uuid>,
    parent_id: Option<Uuid>,
    #[serde(default)]
    vendor: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

/// Trim a descriptive free-text field, treating whitespace-only as absent.
fn trimmed(s: Option<&String>) -> Option<&str> {
    s.map(|v| v.trim()).filter(|v| !v.is_empty())
}

#[utoipa::path(
    post, path = "/api/v1/nodes", tag = "nodes",
    request_body = CreateNode,
    responses(
        (status = 201, description = "Node created", body = CreatedId),
        (status = 400, description = "Empty name, an address that is not an IP, or an illegal pool name", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn create_node(
    _perm: RequireManageConfig,
    admin: Admin,
    Json(body): Json<CreateNode>,
) -> ApiResult<(StatusCode, Json<CreatedId>)> {
    if body.name.trim().is_empty() {
        return Err(ApiError::bad_request(
            "invalid_name",
            "node name must not be empty",
        ));
    }
    let address = body.address.parse::<IpAddr>().map_err(|_| {
        ApiError::bad_request(
            "invalid_address",
            format!("address {:?} is not a valid IP address", body.address),
        )
    })?;
    let pool = validate_pool_create(body.pool)?;
    let id = admin
        .repo
        .create_node(
            body.name.trim(),
            address,
            pool.as_deref(),
            body.profile_id,
            body.credential_id,
            body.parent_id,
            trimmed(body.vendor.as_ref()),
            trimmed(body.model.as_ref()),
        )
        .await
        .map_err(|e| ApiError::from_internal(e.as_ref(), "create node", "failed to create node"))?;
    Ok((StatusCode::CREATED, Json(CreatedId { id })))
}

#[utoipa::path(
    delete, path = "/api/v1/nodes/{node_id}", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    responses(
        (status = 204, description = "Node deleted"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such node", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_node(
    _perm: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let deleted =
        admin.repo.delete_node(id).await.map_err(|e| {
            ApiError::from_internal(e.as_ref(), "delete node", "failed to delete node")
        })?;
    node_write_result(deleted, id)
}

/// The common tail of every single-node write: `204` if the row was there, a typed `404` if not.
///
/// `Ok(false)` from the repository means the node does not exist, and answering `204` to that would
/// tell a client its edit landed when nothing was written.
fn node_write_result(found: bool, id: Uuid) -> ApiResult<StatusCode> {
    if found {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            "node_not_found",
            format!("no node {id}"),
        ))
    }
}

/// Set/clear a node's profile + bound credential and its descriptive maker/model, and optionally
/// move it to a different poll-pool. The node-edit UI loads the current values and resends them, so
/// an unchanged field is preserved.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct NodeBindings {
    profile_id: Option<Uuid>,
    credential_id: Option<Uuid>,
    #[serde(default)]
    vendor: Option<String>,
    #[serde(default)]
    model: Option<String>,
    /// Poll-pool assignment (ADR-009). **Absent** = leave the pool unchanged; `""` (or whitespace)
    /// = clear it to the `default` pool; otherwise move the node to that pool (validated as a
    /// NATS-subject-safe token). See [`validate_pool_update`].
    #[serde(default)]
    pool: Option<String>,
}

#[utoipa::path(
    put, path = "/api/v1/nodes/{node_id}/bindings", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    request_body = NodeBindings,
    responses(
        (status = 204, description = "Bindings updated"),
        (status = 400, description = "Illegal pool name", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such node", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_node_bindings(
    _perm: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<NodeBindings>,
) -> ApiResult<StatusCode> {
    let pool_update = validate_pool_update(body.pool)?;
    let found = admin
        .repo
        .set_node_bindings(
            id,
            body.profile_id,
            body.credential_id,
            trimmed(body.vendor.as_ref()),
            trimmed(body.model.as_ref()),
            pool_update.as_ref().map(|inner| inner.as_deref()),
        )
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "set node bindings", "failed to update node")
        })?;
    node_write_result(found, id)
}

/// Move a node into a group (or `null` to ungroup). Used by the inventory tree (drag/move).
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct NodeGroupAssignment {
    group_id: Option<Uuid>,
}

#[utoipa::path(
    put, path = "/api/v1/nodes/{node_id}/group", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    request_body = NodeGroupAssignment,
    responses(
        (status = 204, description = "Node moved in the folder tree"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such node", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_node_group(
    _perm: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<NodeGroupAssignment>,
) -> ApiResult<StatusCode> {
    let found = admin
        .repo
        .set_node_group(id, body.group_id)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "set node group", "failed to move node")
        })?;
    node_write_result(found, id)
}

/// Whether this node is excluded from derived alert suppression.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct NodeSuppressionOptOut {
    /// `true` ⇒ this node's alerts always stand on their own, whatever the derived graph says.
    opt_out: bool,
}

/// Exclude a node from derived alert suppression, or put it back.
///
/// An excluded node keeps its place in the connectivity graph — everything behind it still resolves
/// through it — but is given no upstream of its own, so its alert is never rolled up under
/// something else. Use it for the one box that must page whatever else is happening.
///
/// This only ever *removes* suppression, so it cannot cause an outage to go unreported. It has no
/// effect while the deployment is on the hand-authored graph.
#[utoipa::path(
    put, path = "/api/v1/nodes/{node_id}/suppression-opt-out", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    request_body = NodeSuppressionOptOut,
    responses(
        (status = 204, description = "The exclusion was set or cleared"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such node", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_node_suppression_opt_out(
    _perm: RequireManageConfig,
    _visible: VisibleNode,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<NodeSuppressionOptOut>,
) -> ApiResult<StatusCode> {
    match admin.repo.set_suppression_opt_out(id, body.opt_out).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(ApiError::not_found(
            "node_not_found",
            format!("no node {id}"),
        )),
        Err(e) => Err(ApiError::from_internal(
            e.as_ref(),
            "set suppression opt-out",
            "failed to update the node",
        )),
    }
}

/// Set (or clear) a node's **dependency parent** (upstream). `parent_id: null` removes the
/// dependency. This is the alert-suppression edge (parent down ⇒ suppress children, ADR-015) —
/// distinct from `PUT /nodes/:id/group`, which moves the node in the inventory folder tree.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct NodeParentAssignment {
    parent_id: Option<Uuid>,
}

#[utoipa::path(
    put, path = "/api/v1/nodes/{node_id}/parent", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    request_body = NodeParentAssignment,
    responses(
        (status = 204, description = "Dependency edge set or cleared"),
        (status = 400, description = "Self-dependency, a parent that does not exist, or an edge that would close a cycle", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such node", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_node_parent(
    _perm: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<NodeParentAssignment>,
) -> ApiResult<StatusCode> {
    // Validate the requested edge before persisting: no self-dependency, the parent must exist,
    // and the new edge must not close a cycle (the dependency graph is a single-parent forest).
    if let Some(parent) = body.parent_id {
        if parent == id {
            return Err(ApiError::bad_request(
                "invalid_dependency",
                "a node cannot depend on itself",
            ));
        }
        let nodes = admin.repo.list_nodes().await.map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "load nodes for dependency check",
                "failed to set dependency",
            )
        })?;
        if !nodes.iter().any(|n| n.id.as_uuid() == parent) {
            return Err(ApiError::bad_request(
                "parent_not_found",
                format!("no node {parent}"),
            ));
        }
        // Reuse the folder-tree cycle guard: the dependency edges have the same (id, parent) shape.
        let edges: Vec<(Uuid, Option<Uuid>)> = nodes
            .iter()
            .map(|n| (n.id.as_uuid(), n.parent.map(|p| p.as_uuid())))
            .collect();
        if would_create_cycle(&edges, id, Some(parent)) {
            return Err(ApiError::bad_request(
                "invalid_dependency",
                "that dependency would create a cycle",
            ));
        }
    }
    let found = admin
        .repo
        .set_node_parent(id, body.parent_id)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "set node parent", "failed to set dependency")
        })?;
    node_write_result(found, id)
}

/// Drag-reorder a node within (or into) a group, positioning it relative to a sibling node.
/// `group_id` is the destination group (`null` ⇒ ungrouped); `before`/`after` name the sibling to
/// land next to (both omitted ⇒ append to the end). At most one of before/after may be set.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct NodePlacement {
    #[serde(default)]
    group_id: Option<Uuid>,
    #[serde(default)]
    before: Option<Uuid>,
    #[serde(default)]
    after: Option<Uuid>,
}

#[utoipa::path(
    put, path = "/api/v1/nodes/{node_id}/placement", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    request_body = NodePlacement,
    responses(
        (status = 204, description = "Node repositioned"),
        (status = 400, description = "Both `before` and `after` were given", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such node", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn place_node(
    _perm: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<NodePlacement>,
) -> ApiResult<StatusCode> {
    if body.before.is_some() && body.after.is_some() {
        return Err(ApiError::bad_request(
            "invalid_placement",
            "specify at most one of before/after",
        ));
    }
    // Order among the destination group's current members, excluding the moving node so it doesn't
    // anchor against itself, then interpolate a fractional order next to the target.
    let siblings: Vec<(Uuid, f64)> = admin
        .repo
        .ordered_nodes_in_group(body.group_id)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "load node siblings", "failed to move node")
        })?
        .into_iter()
        .filter(|(sid, _)| *sid != id)
        .collect();
    let order = placement_order(&siblings, body.before, body.after);
    let found = admin
        .repo
        .place_node(id, body.group_id, order)
        .await
        .map_err(|e| ApiError::from_internal(e.as_ref(), "place node", "failed to move node"))?;
    node_write_result(found, id)
}

// ── Poll pools (ADR-009/020) ─────────────────────────────────────────────────

/// Longest accepted pool name (a single NATS subject token — keep it short and human-manageable).
const MAX_POOL_LEN: usize = 63;

/// Validate an operator-supplied pool name for an **update**, returning the DB update instruction:
/// outer `None` = the field was absent, leave the node's pool unchanged; inner `None` = clear it to
/// NULL (the node falls back to the `default` pool); inner `Some` = set it.
///
/// A pool name becomes the `yagra.jobs.<pool>` / assignment subject, so it must already be a legal
/// single NATS token (`[A-Za-z0-9_-]`). Anything that would sanitize to a *different* string (dots,
/// spaces, slashes, …) is **rejected** rather than silently rewritten, so operator intent stays
/// explicit. Surrounding whitespace is trimmed first (matching the sibling vendor/model fields), and
/// a value that trims to empty clears the pool.
pub(crate) fn validate_pool_update(
    pool: Option<String>,
) -> Result<Option<Option<String>>, ApiError> {
    let Some(raw) = pool else {
        return Ok(None); // field absent → leave the pool as-is
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Some(None)); // explicit clear → NULL (default pool)
    }
    if trimmed.chars().count() > MAX_POOL_LEN {
        return Err(ApiError::bad_request(
            "invalid_pool",
            format!("pool name must be at most {MAX_POOL_LEN} characters"),
        ));
    }
    if yagra_bus::subjects::sanitize_token(trimmed) != trimmed {
        return Err(ApiError::bad_request(
            "invalid_pool",
            "pool name may contain only letters, digits, '_' or '-'",
        ));
    }
    Ok(Some(Some(trimmed.to_owned())))
}

/// [`validate_pool_update`] for a **create** path, where there is no prior value to leave alone:
/// absent and empty both mean "no pool" (inherit from the folder, else the default pool).
///
/// Every path that writes a pool must go through one of these two. The pool name becomes the
/// `yagra.jobs.<pool>` subject verbatim (`subjects::jobs_for_pool` does not sanitize), so an
/// unvalidated value like `tokyo.1` would publish to a subject no poller subscribes to and the
/// node's jobs would be silently discarded.
pub(crate) fn validate_pool_create(pool: Option<String>) -> Result<Option<String>, ApiError> {
    Ok(validate_pool_update(pool)?.flatten())
}

/// Move a node (or a folder) to a poll-pool, or clear it back to inherited. Absent or `""` ⇒ NULL
/// (inherit from the folder, else the default pool).
#[derive(Deserialize, utoipa::ToSchema)]
pub(crate) struct PoolAssignment {
    #[serde(default)]
    pub pool: Option<String>,
}

/// `PUT /api/v1/nodes/:node_id/pool` — set just the node's own pool.
///
/// Deliberately **not** folded into [`set_node_bindings`]: that handler overwrites
/// profile/credential/vendor/model unconditionally (only its `pool` is three-state-gated), so a
/// pool-only caller going through it would silently blank all four.
#[utoipa::path(
    put, path = "/api/v1/nodes/{node_id}/pool", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    request_body = PoolAssignment,
    responses(
        (status = 204, description = "Pool set, or cleared back to inherited"),
        (status = 400, description = "Illegal pool name", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such node", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_node_pool(
    _perm: RequireManageConfig,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(body): Json<PoolAssignment>,
) -> ApiResult<StatusCode> {
    // A single-field endpoint has no "leave unchanged" case, so absent and empty both mean clear.
    let pool = validate_pool_create(body.pool)?;
    let found = admin
        .repo
        .set_node_pool(id, pool.as_deref())
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "set node pool", "failed to set node pool")
        })?;
    node_write_result(found, id)
}

/// One pool offered by the pool picker.
#[derive(Debug, Serialize, PartialEq, Eq, PartialOrd, Ord, utoipa::ToSchema)]
pub(crate) struct PoolOption {
    /// Pool name.
    name: String,
    /// Whether a live poller currently serves it. A pool with none takes the legacy per-job path
    /// onto a subject nothing subscribes to, so its jobs are silently discarded — the picker has to
    /// say so rather than present it as an equivalent choice.
    live: bool,
}

/// The pools that exist, for the assignment picker.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct PoolOptions {
    pools: Vec<PoolOption>,
}

/// Merge the pools nodes use, the pools folders assign, and the pools with a live poller into the
/// picker's option list. Pure, so the union, the default-first ordering, and the `live` flag are
/// unit-testable without a database or a coordinator.
///
/// [`yagra_bus::DEFAULT_POOL`] is always offered (it is where an unassigned node lands, whether or
/// not anything references it explicitly); the rest follow alphabetically.
fn build_pool_options(
    node_pools: Vec<String>,
    group_pools: Vec<String>,
    live: &HashSet<String>,
) -> Vec<PoolOption> {
    let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for p in node_pools.into_iter().chain(group_pools) {
        let trimmed = p.trim();
        if !trimmed.is_empty() {
            names.insert(trimmed.to_owned());
        }
    }
    names.extend(live.iter().cloned());
    names.remove(yagra_bus::DEFAULT_POOL);

    let option = |name: String| PoolOption {
        live: live.contains(&name),
        name,
    };
    std::iter::once(option(yagra_bus::DEFAULT_POOL.to_owned()))
        .chain(names.into_iter().map(option))
        .collect()
}

/// `GET /api/v1/pools` — the pools that exist, for the assignment picker. Names only, no telemetry.
///
/// Deliberately separate from `GET /pollers`, which scans the whole node table to build its
/// per-pool counts; this is two indexed `DISTINCT`s and is loaded by an ordinary page.
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

/// The known poller pools and whether each has a live poller — shared by `GET /api/v1/pools` and
/// the MCP `get_system_health(section="pools")` tool (ADR-042 I3a).
pub(crate) async fn pool_options(admin: &super::AdminState) -> PoolOptions {
    // A read error degrades to "fewer suggestions", never to a failed picker — the operator can
    // still type any pool via Custom.
    let node_pools = admin.repo.distinct_pools().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "listing node pools failed");
        Vec::new()
    });
    let group_pools = admin.groups.distinct_pools().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "listing folder pools failed");
        Vec::new()
    });
    let live = admin.coordinator.live_pools(Instant::now());
    PoolOptions {
        pools: build_pool_options(node_pools, group_pools, &live),
    }
}

// ── Manual poll ──────────────────────────────────────────────────────────────

/// What an out-of-schedule poll dispatched.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct PollNowResult {
    /// How many poll jobs went to the bus. Results arrive asynchronously on the normal result
    /// path, so this confirms dispatch, not completion.
    pub dispatched: usize,
    pub node_id: Uuid,
    /// The pool the jobs were published to — the node's *effective* pool, which may be inherited
    /// from its folder rather than set on the node.
    pub pool: String,
}

/// Dispatch one node's full configured poll set (ICMP liveness + SNMP scalar/table, per its
/// bindings) to the bus immediately, bypassing the scheduler's interval and jitter.
///
/// Shared by the REST handler and the MCP `poll_now` tool. Routing to the node's *effective* pool
/// is the part worth sharing: resolving it on only one of the two surfaces would poke a
/// folder-inherited node on the default pool's subject, where no poller for it is listening.
pub(crate) async fn poll_now(
    admin: &super::AdminState,
    node_id: Uuid,
) -> Result<PollNowResult, ApiError> {
    let node = admin
        .repo
        .get_node(node_id)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "poll-now: load node", "failed to load node")
        })?
        .ok_or_else(|| ApiError::not_found("node_not_found", format!("no node {node_id}")))?;
    let pool = pool_resolver(admin).await.resolve(&node).pool;
    let dispatched = admin.poll.poll_now(&node, &pool).await;
    tracing::info!(node = %node_id, dispatched, pool = %pool, "manual poll dispatched");
    Ok(PollNowResult {
        dispatched,
        node_id,
        pool,
    })
}

/// `ManageConfig` — an operator action, like a discovery scan. Audited by the mutation middleware.
/// `202` because the poll is dispatched, not finished, when this returns.
#[utoipa::path(
    post, path = "/api/v1/nodes/{node_id}/poll", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    responses(
        (status = 202, description = "Jobs dispatched to the node's effective pool", body = PollNowResult),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 404, description = "No such node", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn poll_node_now(
    _perm: RequireManageConfig,
    // An operator action against one node, so it is scoped like a read of that node even though
    // `ManageConfig` is admin-only today. The guard is here rather than resting on "admins are
    // unscoped": that invariant lives in the account writer, and a write path that assumes it
    // would be the thing that breaks when the invariant is relaxed.
    _visible: VisibleNode,
    admin: Admin,
    Path(node_id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<PollNowResult>)> {
    Ok((StatusCode::ACCEPTED, Json(poll_now(&admin, node_id).await?)))
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

    fn poll_request(token: Option<&str>) -> Request<Body> {
        let mut b = Request::builder()
            .method("POST")
            .uri(format!("/api/v1/nodes/{}/poll", Uuid::nil()));
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        b.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn a_write_stays_closed_on_a_public_dashboard() {
        // `public_dashboard` opens reads only. A manual poll is an operator action that reaches
        // real devices, so it must stay authenticated even where the dashboard is open.
        let resp = router(public_state())
            .oneshot(poll_request(None))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_viewer_is_forbidden_not_unauthorized() {
        // The two must stay distinguishable: 401 means "who are you", 403 means "not allowed".
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "viewer1",
        );
        let resp = router(st)
            .oneshot(poll_request(Some(&token)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn an_authorized_caller_in_skeleton_mode_gets_the_availability_error() {
        // Permission first, availability second: an operator who *is* allowed learns that the
        // write side is absent, which is exactly what the anonymous caller above must not.
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Admin, Scope::All),
            "admin1",
        );
        let resp = router(st)
            .oneshot(poll_request(Some(&token)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn the_result_reports_the_pool_the_jobs_actually_went_to() {
        // The pool is in the DTO because it is the non-obvious half of the answer: a node with no
        // pool of its own is polled on its folder's, and "dispatched: 3" alone hides where.
        let json = serde_json::to_value(PollNowResult {
            dispatched: 3,
            node_id: Uuid::nil(),
            pool: "site-osaka".to_owned(),
        })
        .unwrap();
        assert_eq!(json["dispatched"], 3);
        assert_eq!(json["pool"], "site-osaka");
    }

    #[test]
    fn pool_name_validation() {
        // Absent → leave the node's pool unchanged.
        assert_eq!(validate_pool_update(None).ok(), Some(None));
        // Empty / whitespace-only → clear to NULL (the default pool).
        assert_eq!(
            validate_pool_update(Some(String::new())).ok(),
            Some(Some(None))
        );
        assert_eq!(
            validate_pool_update(Some("   ".to_owned())).ok(),
            Some(Some(None))
        );
        // A legal NATS-subject token → set (surrounding whitespace is trimmed).
        assert_eq!(
            validate_pool_update(Some("tokyo".to_owned())).ok(),
            Some(Some(Some("tokyo".to_owned())))
        );
        assert_eq!(
            validate_pool_update(Some("  edge-1_lab  ".to_owned())).ok(),
            Some(Some(Some("edge-1_lab".to_owned())))
        );
        // Rejected: anything that would sanitize to a different subject token (dot / space / slash).
        assert!(validate_pool_update(Some("tokyo.1".to_owned())).is_err());
        assert!(validate_pool_update(Some("east dc".to_owned())).is_err());
        assert!(validate_pool_update(Some("a/b".to_owned())).is_err());
        // Rejected: over the length bound; the bound itself is accepted.
        assert!(validate_pool_update(Some("p".repeat(MAX_POOL_LEN + 1))).is_err());
        assert!(validate_pool_update(Some("p".repeat(MAX_POOL_LEN))).is_ok());
    }

    #[test]
    fn pool_name_validation_on_create_collapses_absent_and_empty() {
        // A create has no prior value to leave alone, so "absent" and "cleared" are the same answer.
        assert_eq!(validate_pool_create(None).ok(), Some(None));
        assert_eq!(validate_pool_create(Some(String::new())).ok(), Some(None));
        assert_eq!(
            validate_pool_create(Some(" tokyo ".to_owned())).ok(),
            Some(Some("tokyo".to_owned()))
        );
        // The create paths used to skip validation entirely: `tokyo.1` reached the DB and its jobs
        // were published to `yagra.jobs.tokyo.1`, a subject no poller subscribes to, and silently
        // discarded (plain NATS, not JetStream).
        assert!(validate_pool_create(Some("tokyo.1".to_owned())).is_err());
        assert!(validate_pool_create(Some("east dc".to_owned())).is_err());
    }

    #[test]
    fn pool_options_union_default_first_and_flag_liveness() {
        let live: HashSet<String> = ["tokyo".to_owned(), "spare".to_owned()]
            .into_iter()
            .collect();
        let opts = build_pool_options(
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
        let opts = build_pool_options(Vec::new(), Vec::new(), &HashSet::new());
        assert_eq!(opts.len(), 1);
        assert_eq!(opts[0].name, yagra_bus::DEFAULT_POOL);
        assert!(!opts[0].live);
    }

    /// A store holding one RTT reading for `node`, and nothing else.
    fn store_with_rtt(node: NodeId) -> std::sync::Arc<dyn crate::store::MetricStore> {
        let sink = crate::sink::InMemorySink::default();
        sink.ingest(&yagra_bus::PollResult {
            job_id: Uuid::nil(),
            node_id: node,
            at_unix_ms: 0,
            outcome: yagra_bus::CheckOutcome::Reachable,
            samples: vec![yagra_bus::Sample::gauge("icmp_rtt_ms", 1.5)],
            interfaces: Vec::new(),
            sys_descr: None,
            dns_chain: None,
            neighbors: None,
            l3: None,
            arp: None,
            routing: None,
            observational: false,
            poller_id: None,
            trace_context: Default::default(),
        });
        std::sync::Arc::new(sink)
    }

    #[tokio::test]
    async fn an_unobserved_node_falls_back_to_a_recent_rtt_sample() {
        // The alert engine has no opinion about a just-added node, and none right after a core
        // restart either. Reporting `unknown` for a node that is plainly answering pings is what
        // this fallback exists to prevent.
        //
        // The two forms must agree: the list uses the batched one and the detail view the single
        // one, and MCP's two node tools used to use neither — which is the drift this shares away.
        let node = NodeId::from(Uuid::new_v4());
        let mut st = private_state();

        assert_eq!(display_state(&st, node).await, NodeState::Unknown);
        assert_eq!(
            display_states(&st, &[node]).await.get(&node).copied(),
            Some(NodeState::Unknown)
        );

        st.store = store_with_rtt(node);
        assert_eq!(display_state(&st, node).await, NodeState::Ok);
        assert_eq!(
            display_states(&st, &[node]).await.get(&node).copied(),
            Some(NodeState::Ok),
            "the batched form must agree with the single-node form"
        );

        // A different node's sample must not make this one look alive.
        let stranger = NodeId::from(Uuid::new_v4());
        assert_eq!(display_state(&st, stranger).await, NodeState::Unknown);
        assert_eq!(
            display_states(&st, &[stranger])
                .await
                .get(&stranger)
                .copied(),
            Some(NodeState::Unknown)
        );
    }

    async fn status_of(st: ApiState, method: &str, path: &str, token: Option<&str>) -> StatusCode {
        let mut b = Request::builder().method(method).uri(path);
        if let Some(t) = token {
            b = b.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        let body = if matches!(method, "POST" | "PUT") {
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

    /// Every config write this module serves. A route missing here is a route whose authorization
    /// nothing checks.
    fn write_routes() -> Vec<(&'static str, String)> {
        let id = Uuid::nil();
        vec![
            ("POST", "/api/v1/nodes".to_owned()),
            ("DELETE", format!("/api/v1/nodes/{id}")),
            ("PUT", format!("/api/v1/nodes/{id}/bindings")),
            ("PUT", format!("/api/v1/nodes/{id}/group")),
            ("PUT", format!("/api/v1/nodes/{id}/pool")),
            ("PUT", format!("/api/v1/nodes/{id}/parent")),
            ("PUT", format!("/api/v1/nodes/{id}/placement")),
            ("POST", format!("/api/v1/nodes/{id}/poll")),
        ]
    }

    #[tokio::test]
    async fn every_inventory_write_is_authenticated_before_anything_else() {
        // Guard order: the permission check runs before the availability check, so an anonymous
        // caller learns only that it is anonymous — never whether this deployment has a write side.
        for (method, path) in write_routes() {
            assert_eq!(
                status_of(public_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
            assert_eq!(
                status_of(private_state(), method, &path, None).await,
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn editing_the_inventory_takes_manage_config() {
        // Reshaping the inventory is an operator's job since ADR-057 — moving a node between
        // folders rewires which alerts an operator sees, and re-parenting rewires root-cause
        // suppression, both of which are the monitoring rather than the deployment. A viewer still
        // changes nothing. 503 = past the guard, into skeleton mode.
        let st = private_state();
        for (role, name, want) in [
            (Role::Viewer, "viewer1", StatusCode::FORBIDDEN),
            (Role::Operator, "op1", StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let token = st
                .sessions
                .issue(Uuid::new_v4(), Principal::new(role, Scope::All), name);
            for (method, path) in write_routes() {
                assert_eq!(
                    status_of(st.clone(), method, &path, Some(&token)).await,
                    want,
                    "{role:?} {method} {path}"
                );
            }
        }
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Admin, Scope::All),
            "admin1",
        );
        for (method, path) in write_routes() {
            assert_eq!(
                status_of(st.clone(), method, &path, Some(&token)).await,
                StatusCode::SERVICE_UNAVAILABLE,
                "admin {method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn a_missing_node_reads_as_missing_not_as_a_configuration_error() {
        // `get_node` deliberately does not take the `Admin` extractor: skeleton mode has no
        // inventory, so "no write side" and "no such node" are the same fact, and 503 would send an
        // operator looking for a broken deployment instead of a wrong id.
        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "viewer1",
        );
        assert_eq!(
            status_of(
                st,
                "GET",
                &format!("/api/v1/nodes/{}", Uuid::nil()),
                Some(&token)
            )
            .await,
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn the_reported_kind_is_the_one_that_wins_not_the_first_row_found() {
        // `get_node` returns every row it finds *and* the resolved kind. The UI branches on the
        // kind; returning the rows as well is what lets an operator see the stray row that is being
        // ignored, instead of it vanishing from the API and staying in the database forever.
        //
        // The pairing is the point: reading the configs and concluding a kind from whichever is
        // non-null is what made the node page show a URL-monitor health card for a node the poller
        // was treating as a Meraki device.
        let both = NodeRows {
            meraki: true,
            url: true,
            dns: false,
        };
        assert_eq!(NodeKind::resolve(both), NodeKind::Meraki);
        assert_eq!(NodeKind::resolve(NodeRows::default()), NodeKind::Device);
    }

    #[test]
    fn a_failed_side_table_read_degrades_the_kind_the_same_way_the_scheduler_does() {
        // Every one of the three lookups in `get_node` is `unwrap_or(None)` — a check-config read
        // failure must not fail the whole node detail. That makes a failed read indistinguishable
        // from "no row", which is only safe because the scheduler degrades identically (its own
        // lookups warn and treat the node as not-that-kind). Both sides fall toward `Device`, so a
        // transient database error cannot make the API and the poller disagree about a node.
        assert_eq!(NodeKind::resolve(NodeRows::default()), NodeKind::Device);
        assert!(NodeKind::Device.is_polled_per_node());
    }

    #[test]
    fn the_inventory_row_reports_the_same_kind_the_node_detail_does() {
        // The list and the detail are two code paths answering one question, and an operator moves
        // between them in one click — a row badged `device` that opens a page saying `url` is the
        // disagreement `NodeKind::resolve` exists to make impossible. `resolve_kinds` therefore
        // delegates, and delegation is cheap to "optimize" back into a local `if` chain, so pin it
        // over every combination of side-table rows rather than over the three that occur in
        // practice — the stray-row cases are exactly where a re-derived precedence would differ.
        let id = Uuid::from_u128(1);
        let ids = [id];
        for bits in 0u8..8 {
            let rows = NodeRows {
                meraki: bits & 1 != 0,
                url: bits & 2 != 0,
                dns: bits & 4 != 0,
            };
            let set = |present: bool| {
                if present {
                    HashSet::from([id])
                } else {
                    HashSet::new()
                }
            };
            let got = resolve_kinds(&ids, &set(rows.meraki), &set(rows.url), &set(rows.dns));
            assert_eq!(
                got.get(&id).copied(),
                Some(NodeKind::resolve(rows)),
                "{rows:?}"
            );
        }
    }

    #[test]
    fn a_failed_side_table_read_degrades_a_list_row_toward_device() {
        // Each of the three reads behind `node_kinds` is `unwrap_or_default()`, so a database
        // hiccup looks exactly like "no row" — the empty-set case. It must land on `Device`, the
        // same direction `get_node` and the scheduler degrade in, or a transient error would badge
        // a URL monitor as a device on one surface and not the other.
        let id = Uuid::from_u128(2);
        let empty = HashSet::new();
        let got = resolve_kinds(&[id], &empty, &empty, &empty);
        assert_eq!(got.get(&id).copied(), Some(NodeKind::Device));
        // An id nobody asked about is not invented, and an empty page is an empty map.
        assert_eq!(got.len(), 1);
        assert!(resolve_kinds(&[], &empty, &empty, &empty).is_empty());
    }

    // ── Filter mode ─────────────────────────────────────────────────────────────────────────────

    fn n(id: u128, name: &str, pool: Option<&str>) -> Node {
        let mut node = Node::new(
            NodeId::from(Uuid::from_u128(id)),
            name,
            std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 1)),
        );
        node.pool = pool.map(str::to_owned);
        node
    }

    #[tokio::test]
    async fn a_filter_that_is_set_widens_the_scan_and_one_that_is_not_does_not() {
        // The cost of filter mode is paid only by the filters that reject rows *after* the query.
        // A plain text search rejects nothing, so widening the scan for it would make every
        // keystroke in the tree read 5,000 rows to show 100.
        let st = public_state();
        let scope = super::super::scope::NodeScope::All;
        let (rows, truncated) =
            filtered_node_page(&st, &scope, "demo", &NodeFilter::default(), 100)
                .await
                .expect("search succeeds");
        assert_eq!(rows.len(), 1, "the skeleton inventory holds one node");
        assert!(!truncated);
    }

    #[tokio::test]
    async fn a_state_filter_selects_on_the_state_the_row_would_display() {
        // The whole reason these run in-process: the filter and the rendered row must come from
        // one resolver, so a row can never disagree with the filter that selected it. In skeleton
        // mode the alert engine has observed nothing and there is no TSDB fallback, so every node
        // displays `unknown` — and therefore matches `unknown` and nothing else.
        let st = public_state();
        let scope = super::super::scope::NodeScope::All;
        let want_unknown = NodeFilter {
            state: vec![NodeState::Unknown],
            ..Default::default()
        };
        let (rows, _) = filtered_node_page(&st, &scope, "", &want_unknown, 100)
            .await
            .expect("filter succeeds");
        assert_eq!(rows.len(), 1);
        let want_ok = NodeFilter {
            state: vec![NodeState::Ok],
            ..Default::default()
        };
        let (rows, _) = filtered_node_page(&st, &scope, "", &want_ok, 100)
            .await
            .expect("filter succeeds");
        assert!(rows.is_empty(), "no node displays ok, so none may match ok");
    }

    #[tokio::test]
    async fn a_kind_filter_agrees_with_what_the_row_is_badged() {
        // Skeleton mode has no side tables, so every node resolves to `Device` — the same answer
        // `build_node_summaries` puts on the row. Asking for anything else must return nothing
        // rather than everything.
        let st = public_state();
        let scope = super::super::scope::NodeScope::All;
        for (kind, expect) in [
            (NodeKind::Device, 1),
            (NodeKind::Url, 0),
            (NodeKind::Dns, 0),
        ] {
            let f = NodeFilter {
                kind: vec![kind],
                ..Default::default()
            };
            let (rows, _) = filtered_node_page(&st, &scope, "", &f, 100)
                .await
                .expect("filter succeeds");
            assert_eq!(rows.len(), expect, "{kind:?}");
        }
    }

    #[tokio::test]
    async fn a_pool_filter_reads_the_effective_pool_not_the_stored_column() {
        // A node that sets no pool of its own is not "pool-less" — it inherits, and with no folder
        // saying otherwise it lands on the default. Filtering the column would find nothing for
        // `default`, which is the pool most of the fleet is actually in.
        let st = public_state();
        let scope = super::super::scope::NodeScope::All;
        let f = NodeFilter {
            pool: vec![yagra_bus::DEFAULT_POOL.to_owned()],
            ..Default::default()
        };
        let (rows, _) = filtered_node_page(&st, &scope, "", &f, 100)
            .await
            .expect("filter succeeds");
        assert_eq!(rows.len(), 1, "the demo node inherits the default pool");
        let f = NodeFilter {
            pool: vec!["tokyo".to_owned()],
            ..Default::default()
        };
        let (rows, _) = filtered_node_page(&st, &scope, "", &f, 100)
            .await
            .expect("filter succeeds");
        assert!(rows.is_empty());
    }

    #[test]
    fn the_three_node_filters_take_sets_and_an_empty_one_means_unfiltered() {
        // The inversion that would be silent: an empty set read as "match nothing" makes the whole
        // tree go blank the moment any *other* filter is set. Every set filter in this codebase
        // spells "no filter" as empty, and this is where the node list agrees.
        let none = parse_node_filter(None, None, None).expect("no filter parses");
        assert!(!none.is_set());
        let empty = parse_node_filter(Some(""), Some(""), Some("")).expect("empty parses");
        assert!(
            !empty.is_set(),
            "an empty value is no filter, not an empty result"
        );

        let f = parse_node_filter(Some("warning,critical,unreachable"), None, None)
            .expect("a set parses");
        assert_eq!(
            f.state,
            vec![
                NodeState::Warning,
                NodeState::Critical,
                NodeState::Unreachable
            ]
        );
        assert!(f.is_set());
    }

    #[test]
    fn an_unknown_state_or_kind_is_rejected_but_an_unknown_pool_is_not() {
        // State and kind are closed vocabularies this build owns, so a token outside them is a
        // mistake worth a 400 — dropping it would widen the answer and read as "there are no Meraki
        // nodes". A pool is a name the operator invented, so there is nothing to check against and
        // an unrecognised one must simply match nothing.
        assert!(parse_node_filter(Some("melted"), None, None).is_err());
        assert!(parse_node_filter(None, Some("toaster"), None).is_err());
        // …and the message names the real vocabulary rather than a copy of it.
        let err = parse_node_filter(Some("melted"), None, None).unwrap_err();
        let body = format!("{err:?}");
        assert!(body.contains("unreachable"), "{body}");

        let f = parse_node_filter(None, None, Some(" tokyo , , osaka ")).expect("pools parse");
        assert_eq!(f.pool, vec!["tokyo".to_owned(), "osaka".to_owned()]);
    }

    #[tokio::test]
    async fn a_state_set_is_a_union_rather_than_an_intersection() {
        // Two states must return the rows matching *either*. In skeleton mode everything displays
        // `unknown`, so a set containing it matches and one that does not must not — which is also
        // the assertion that catches a `contains` written as an `all`.
        let st = public_state();
        let scope = super::super::scope::NodeScope::All;
        let with = NodeFilter {
            state: vec![NodeState::Ok, NodeState::Unknown],
            ..Default::default()
        };
        let (rows, _) = filtered_node_page(&st, &scope, "", &with, 100)
            .await
            .expect("filter succeeds");
        assert_eq!(rows.len(), 1);
        let without = NodeFilter {
            state: vec![NodeState::Ok, NodeState::Warning],
            ..Default::default()
        };
        let (rows, _) = filtered_node_page(&st, &scope, "", &without, 100)
            .await
            .expect("filter succeeds");
        assert!(rows.is_empty());
    }

    #[test]
    fn a_pool_filter_matches_a_nodes_own_pool_over_any_inheritance() {
        // `resolve_pool`'s precedence, pinned where the filter reads it: a node naming its own pool
        // is in that pool whatever its folder says. Pure, so it does not need a state.
        let resolver = crate::poolres::PoolResolver::empty();
        assert_eq!(resolver.resolve_pool(&n(1, "a", Some("osaka"))), "osaka");
        assert_eq!(
            resolver.resolve_pool(&n(2, "b", None)),
            yagra_bus::DEFAULT_POOL
        );
        // A blank string is "unset", not a pool literally named "" — otherwise a cleared field in
        // the UI would put a node into a pool no poller ever registers for.
        assert_eq!(
            resolver.resolve_pool(&n(3, "c", Some(""))),
            yagra_bus::DEFAULT_POOL
        );
    }

    #[tokio::test]
    async fn paging_never_reports_truncation() {
        // `next_cursor` already says there is more. A client reading both would show a "results
        // were cut" notice on every page but the last.
        let st = public_state();
        let resp = router(st)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/nodes?limit=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let page: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(page["truncated"], false);
    }
}
