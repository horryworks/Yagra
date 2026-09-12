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

use super::extract::{Admin, ListSlot, RequireManageConfig, RequireView, Scoped, VisibleNode};
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
    bulk_tag_nodes,
    set_node_group,
    move_nodes,
    preview_move_by_prefix,
    set_node_pool,
    set_node_parent,
    set_node_suppression_opt_out,
    place_node
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
        .route("/api/v1/nodes/move", post(move_nodes))
        .route("/api/v1/nodes/tags", post(bulk_tag_nodes))
        .route("/api/v1/nodes/move-preview", post(preview_move_by_prefix))
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
    // ⚠️ `node_states_for`, not `node_states` (ADR-125). The latter clones the whole fleet's state
    // map, and this function is on the hottest read in the product — S20 scoped the TSDB query to
    // the page and left the clone fleet-wide, which is a copy of thousands of entries to look up a
    // few dozen, per request, under the lock the poll ingest path shares.
    let known = st.alerts.node_states_for(nodes);
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

/// One group's direct members, or several groups' when `groups=` was used. Not keyset-paged — a
/// folder is loaded whole when it is expanded — so it reports truncation instead of offering a
/// cursor.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct GroupNodes {
    nodes: Vec<NodeSummary>,
    truncated: bool,
    /// Which groups this answer actually covers — present only when `groups=` was understood.
    ///
    /// 🚨 **This field is what makes the batch form safe against an older core** (ADR-125), and
    /// without it the failure is silent and wrong rather than loud. `GroupNodesQuery` is a plain
    /// `Deserialize` with no `deny_unknown_fields`, so a core that predates `groups=` **ignores it**
    /// — and with no `group=` either, it falls through to the ungrouped bucket and returns those
    /// nodes with a perfectly ordinary 200. A newer WebUI would read that as "here are the members
    /// of the thirty folders you asked about" and file every ungrouped node under all of them.
    ///
    /// ⚠️ **Inferring coverage from the rows cannot work**: a folder with no members and a folder
    /// that was never asked about both come back as no rows. The set has to be stated.
    ///
    /// `None` for the single-group form, so an older WebUI sees exactly the response it always did.
    #[serde(skip_serializing_if = "Option::is_none")]
    answered: Option<Vec<Uuid>>,
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
async fn build_node_summaries(
    st: &ApiState,
    nodes: Vec<Node>,
    known_orders: HashMap<Uuid, f64>,
) -> Vec<NodeSummary> {
    let ids: Vec<Uuid> = nodes.iter().map(|n| n.id.as_uuid()).collect();
    let node_ids: Vec<NodeId> = nodes.iter().map(|n| n.id).collect();
    // 🚨 **Only the rows whose order is not already in hand** (ADR-133). The by-group reads
    // `ORDER BY sort_order` and now project it, so they arrive with the answer and this asks
    // nothing — one fewer PostgreSQL connection held concurrently on the hottest list in the
    // product, which is the arithmetic `LIST_SEATS` is sized against. The paged and search paths
    // still come through `NODE_COLUMNS` alone and still pay for the second read; widening that
    // projection would put `sort_order` on every row the MCP `list_nodes` tool serves, for a field
    // it does not have.
    let missing: Vec<Uuid> = ids
        .iter()
        .copied()
        .filter(|id| !known_orders.contains_key(id))
        .collect();
    // Skeleton mode has neither ordering nor side tables; a read failure degrades the kind and the
    // ordering, never the list.
    let inventory = async {
        match st.admin.as_ref() {
            Some(admin) => tokio::join!(
                async {
                    if missing.is_empty() {
                        known_orders
                    } else {
                        let mut orders = admin
                            .repo
                            .node_sort_orders(&missing)
                            .await
                            .unwrap_or_default();
                        orders.extend(known_orders);
                        orders
                    }
                },
                node_kinds(admin, &ids),
            ),
            None => (known_orders, HashMap::new()),
        }
    };
    // Up to five independent reads — four PostgreSQL, one TSDB — on the hottest list in the
    // product: every tree page, every debounced search keystroke, every lazy folder expand runs
    // them. None needs another's answer, so they overlap rather than queue (the shape
    // `interface_heatmap` uses for its per-link fan-out) and the wall clock is the slowest one, not
    // their sum. The two that `node_kinds` added are the price of the list agreeing with the detail
    // page; the cheaper-looking alternative — the three `node_ids()` full-table reads the scheduler
    // uses — is unbounded in how many monitors exist and returns rows this page cannot use.
    //
    // ⚠️ **"Up to", since ADR-133**: a lazy folder expand arrives with its orders already read and
    // spends four, not five. The wall clock barely moves — they were always parallel — but the
    // number of connections one request holds at its peak does, and that is what a pool of 20
    // against eight `ListSlot` seats is measured against.
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
        (status = 503, description = "Too many inventory reads in flight — retry shortly (`list_busy`)", body = super::error::ErrorBody),
    ),
)]
async fn list_nodes(
    _perm: RequireView,
    // A seat at the fleet-scale reads (ADR-125). After the permission guard, before the work: an
    // unauthenticated caller must not be able to occupy one.
    _seat: ListSlot,
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
            nodes: build_node_summaries(&st, nodes, HashMap::new()).await,
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
        nodes: build_node_summaries(&st, nodes, HashMap::new()).await,
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

/// Query for the per-group lazy tree load.
///
/// - `?groups=<uuid>,<uuid>,…` — several folders' direct members in one answer (ADR-125). The
///   response echoes the set it covered in `answered`.
/// - `?group=<uuid>` — one folder's direct members.
/// - neither — the ungrouped nodes (`group_id IS NULL`).
///
/// ⚠️ `groups` wins when both are given. They are never sent together by this product; the rule
/// exists so the behaviour is decided rather than incidental.
#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(super) struct GroupNodesQuery {
    group: Option<Uuid>,
    /// Comma-separated group ids. Bounded by [`BY_GROUP_BATCH_MAX`].
    groups: Option<String>,
}

/// How many folders one batch may name.
///
/// Twice the largest window the inventory tree can ask for (its viewport holds ~30 folder rows at
/// 1080p), so the client never has to split a request it can legitimately make.
const BY_GROUP_BATCH_MAX: usize = 64;

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
        (status = 503, description = "Too many inventory reads in flight — retry shortly (`list_busy`)", body = super::error::ErrorBody),
    ),
)]
async fn list_group_nodes(
    _perm: RequireView,
    // The other seat holder, and the one the burst actually came through (ADR-125).
    _seat: ListSlot,
    Scoped(scope): Scoped,
    axum::extract::State(st): axum::extract::State<ApiState>,
    Query(q): Query<GroupNodesQuery>,
) -> ApiResult<Json<GroupNodes>> {
    // The batch form. Parsed at the edge into typed ids, so an unparseable one is a 400 rather than
    // a folder silently missing from an answer that still looks complete.
    let batch: Option<Vec<Uuid>> =
        match q.groups.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            None => None,
            Some(raw) => {
                let ids = raw
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(Uuid::parse_str)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| {
                        ApiError::bad_request(
                            "invalid_group_id",
                            "`groups` must be comma-separated UUIDs",
                        )
                    })?;
                // 🚨 Refused, never truncated. Silently dropping the tail would answer with a set the
                // caller did not ask for while looking exactly like success — the same failure the
                // `answered` echo exists to prevent, arriving by a different door (ADR-124 決定 7).
                if ids.len() > BY_GROUP_BATCH_MAX {
                    return Err(ApiError::bad_request(
                        "too_many_groups",
                        format!("at most {BY_GROUP_BATCH_MAX} groups per request"),
                    ));
                }
                Some(ids)
            }
        };

    let Some(admin) = st.admin.as_ref() else {
        // Skeleton mode has no group membership; the demo node is ungrouped. Return it for the
        // ungrouped bucket, nothing for a specific group. A scoped caller gets nothing either way:
        // an ungrouped node is outside every group scope, which `search` enforces for us.
        let nodes = if q.group.is_none() && batch.is_none() {
            st.nodes
                .search(scope.group_filter(), "", GROUP_NODES_CAP)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        return Ok(Json(GroupNodes {
            nodes: build_node_summaries(&st, nodes, HashMap::new()).await,
            truncated: false,
            // Still echoed in skeleton mode: the answer is empty because there is no inventory,
            // which is a different statement from "this core does not understand the question".
            answered: batch,
        }));
    };
    let mut nodes = match &batch {
        Some(ids) => {
            admin
                .repo
                .list_nodes_in_groups(scope.group_filter(), ids, GROUP_NODES_CAP + 1)
                .await
        }
        None => {
            admin
                .repo
                .list_nodes_in_group(scope.group_filter(), q.group, GROUP_NODES_CAP + 1)
                .await
        }
    }
    .map_err(|e| {
        ApiError::from_internal(e.as_ref(), "list group nodes", "failed to load group nodes")
    })?;
    let truncated = i64::try_from(nodes.len()).unwrap_or(i64::MAX) > GROUP_NODES_CAP;
    if truncated {
        nodes.truncate(usize::try_from(GROUP_NODES_CAP).unwrap_or(usize::MAX));
    }
    // Both reads above `ORDER BY sort_order` and project it, so the order arrives with the rows and
    // `build_node_summaries` asks no second question about them (ADR-133). Split AFTER the
    // truncation, so the map describes the rows that are actually being returned.
    let orders: HashMap<Uuid, f64> = nodes
        .iter()
        .map(|o| (o.node.id.as_uuid(), o.sort_order))
        .collect();
    let nodes: Vec<Node> = nodes.into_iter().map(|o| o.node).collect();
    Ok(Json(GroupNodes {
        nodes: build_node_summaries(&st, nodes, orders).await,
        truncated,
        answered: batch,
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
    /// Whether SNMP polling is **configured** for this node — not whether it is answering.
    ///
    /// 🚨 **Do not re-derive this from `credential_id`.** The scheduler falls back to the
    /// deployment-wide `YAGRA_SNMP_COMMUNITY` for every node with no bound credential, so on such a
    /// deployment a `credential_id` of `null` still means a device that is walked, has interface
    /// rows and has neighbours. The WebUI uses this to hide the tabs whose only data source is an
    /// SNMP walk (Interfaces, Neighbors — ADR-119); deriving it client-side would hide them on
    /// exactly the nodes that have the data.
    ///
    /// ⚠️ Over-reports rather than under-reports — see
    /// `PollDispatcher::snmp_configured_for`, which is the one place the rule lives.
    snmp_configured: bool,
    /// The operator's free-text note about this node; `null` ⇒ none (ADR-135).
    ///
    /// ⚠️ **Detail only — deliberately not on `NodeSummary`.** The inventory tree fetches one
    /// summary per node and ADR-133 had just made that response smaller; a note is up to 2,000
    /// characters that the tree does not draw.
    notes: Option<String>,
    /// The labels stored **on this node** (ADR-135). Empty when it has none, sorted.
    ///
    /// Also detail-only, and for a second reason beyond size: the tree does not display them, and
    /// `NodeSummaryDto` on the MCP side has carried them since before any writer existed.
    ///
    /// ⚠️ This is what the edit dialog writes back, so it is the node's OWN set — not what it
    /// effectively carries. The inherited half is `inherited_tags` below.
    tags: Vec<String>,
    /// The labels this node gets from its inventory folder and that folder's ancestors, already
    /// minus the ones it excludes and minus anything it carries itself (ADR-135 inc. 2). Sorted.
    ///
    /// Resolved on every read and never stored — a copy written onto the node row would go stale
    /// the moment a parent is edited or a folder is moved, which is the same call folder-pool and
    /// map-coordinate inheritance already made.
    ///
    /// The screen draws `tags` and these as two marked groups; a chip here is removed by adding it
    /// to `tags_excluded`, not by editing `tags`.
    inherited_tags: Vec<String>,
    /// Labels this node refuses to inherit (ADR-135 inc. 2). Sorted.
    ///
    /// Shown in full, including entries naming a label nothing currently supplies: an exclusion
    /// that cannot be seen cannot be undone, and it stays meaningful because an ancestor may
    /// re-add that label later.
    tags_excluded: Vec<String>,
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
    // `get_node_with_notes`, not `get_node`: the note is not in `NODE_COLUMNS` and this is one of
    // the two surfaces allowed to ask for it (ADR-135 decision 2).
    let crate::repo::NodeWithNotes { mut node, notes } = admin
        .repo
        .get_node_with_notes(node_id)
        .await
        .map_err(|e| ApiError::from_internal(e.as_ref(), "get node", "failed to load node"))?
        .ok_or_else(missing)?;
    // Best-effort: a URL/DNS-check or Meraki load failure shouldn't fail the node detail. A failed
    // lookup reads as "no row", which degrades the resolved kind toward `Device` — the same way the
    // scheduler degrades on the same failure, so the two still agree.
    let url_check = admin.url_checks.get(node_id).await.unwrap_or(None);
    let dns_check = admin.dns_checks.get(node_id).await.unwrap_or(None);
    let meraki_device = admin.meraki_devices.get(node_id).await.unwrap_or(None);
    // Asked of the dispatcher, which is the only holder of the environment community — before the
    // struct literal below moves `node`'s fields out.
    let snmp_configured = admin.dispatcher.snmp_configured_for(&node);
    // One whole-table read of `node_groups` per detail GET — the same profile the pool fact beside
    // it already accepts, against a table of hundreds of rows.
    let inherited_tags = super::util::tag_resolver(admin).await.inherited(&node);
    // Taken before the literal below moves the node's fields out.
    let tags = std::mem::take(&mut node.tags);
    let tags_excluded = std::mem::take(&mut node.tags_excluded);
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
        snmp_configured,
        notes,
        tags,
        inherited_tags,
        tags_excluded,
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

/// Longest note a node may carry (ADR-135). Enforced here rather than as a `CHECK` constraint:
/// a violation answers 400 with a message an operator can act on, where a constraint would come
/// back through `from_internal` as an opaque 500 — and narrowing the column later on a deployment
/// that already holds a longer value is a migration that fails, which is a core that will not start.
pub(crate) const NOTES_MAX: usize = 2000;

/// A node's name, trimmed, or the 400 that says it was blank.
///
/// One helper rather than a fourth copy: [`create_node`] and [`set_node_bindings`] share it.
/// ⚠️ **`api/discovery.rs` and `api/checks.rs` keep their own** — the three had already drifted on
/// the error *code* (`invalid_node` there, `invalid_name` here), and folding them together would
/// change a published response code for a reason that is only tidiness.
fn validated_name(raw: &str) -> ApiResult<&str> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_name",
            "node name must not be empty",
        ));
    }
    Ok(name)
}

/// Label limits (ADR-135). This endpoint is the **first validator `nodes.tags` has ever had** —
/// the column carried no constraint, and its only writer before ADR-135 was a config-bundle import
/// that checked nothing.
///
/// ⚠️ `LABELS_MAX` is 32 *per node and per folder*, while the RCA prompt silently keeps only the
/// first **8** (`rca/context.rs::MAX_TAGS`) and a node's effective set is capped at
/// `tagres::EFFECTIVE_LABELS_MAX`. None of those are contradictions to fix here: a node may
/// legitimately carry more labels than an LLM prompt should be spent on. They are written down
/// because "the AI did not see my label" otherwise has no discoverable cause.
pub(crate) const LABEL_MAX: usize = 64;
pub(crate) const LABELS_MAX: usize = 32;

/// Validate and normalize one whole label set.
///
/// **Deliberately unrestricted in character** beyond the length and a ban on control characters: a
/// label is a word a person reads off a badge (`JAPAN`, `松山本社`, `spare parts`), and it is also
/// what a `ScopeLevel::Group` threshold and a `WindowScope::Group` window match on. The old
/// key/value shape restricted the *key* to `[A-Za-z0-9_.:-]` and left the *value* free; with one
/// string, the free rule is the one that survives — a badge is prose.
///
/// 🚨 **An empty entry is refused, where the key/value validator silently dropped one.** That drop
/// existed because the old editor sent a blank row for every unfilled "add tag" click. The chip
/// input cannot produce one, so a blank arriving here means a client built a bad request, and
/// swallowing it is how "my label did not save" becomes unexplainable.
///
/// ⚠️ **Do not route a *removal* list through this.** A label longer than [`LABEL_MAX`] can exist
/// in the database — migration 0109 converts from a column whose values could be 128 characters —
/// and length-checking a removal would make exactly the labels somebody wants gone impossible to
/// remove. See [`normalized_removals`].
pub(super) fn validated_labels(raw: Vec<String>) -> ApiResult<Vec<String>> {
    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    for label in raw {
        let label = label.trim().to_owned();
        if label.is_empty() {
            return Err(ApiError::bad_request(
                "invalid_tag",
                "a tag cannot be empty".to_owned(),
            ));
        }
        if label.chars().count() > LABEL_MAX {
            return Err(ApiError::bad_request(
                "invalid_tag",
                format!("tag {label:?} is longer than {LABEL_MAX} characters"),
            ));
        }
        if label.chars().any(char::is_control) {
            return Err(ApiError::bad_request(
                "invalid_tag",
                format!("tag {label:?} contains a control character"),
            ));
        }
        if !out.contains(&label) {
            out.push(label);
        }
    }
    if out.len() > LABELS_MAX {
        return Err(ApiError::bad_request(
            "invalid_tag",
            format!("at most {LABELS_MAX} tags are allowed, got {}", out.len()),
        ));
    }
    out.sort();
    Ok(out)
}

/// Trim and de-duplicate a list of labels to *remove* or to *exclude*, with **no** length or
/// character check.
///
/// 🚨 The asymmetry with [`validated_labels`] is the point. Those rules police what can be
/// created; a label already in the database may predate them (0109 converts values that could be
/// twice as long, from a column that never rejected a control character). Applying them here would
/// mean the only labels an operator cannot delete are the ones they most want to.
pub(super) fn normalized_removals(raw: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    for label in raw {
        let label = label.trim().to_owned();
        if !label.is_empty() && !out.contains(&label) {
            out.push(label);
        }
    }
    out.sort();
    out
}

/// A three-state free-text update: `None` = leave the column alone, `Some(None)` = clear it,
/// `Some(Some(v))` = set it. Mirrors [`validate_pool_update`]'s shape, for the same reason.
///
/// 🚨 **Not `trimmed`.** That helper maps `""` to `None`, which is exactly the distinction this
/// field turns on — "the client did not mention notes" and "the client asked to clear the notes"
/// would become the same value, and the first would start destroying text.
fn free_text_update<'a>(
    raw: Option<&'a String>,
    max: usize,
    code: &'static str,
    what: &str,
) -> ApiResult<Option<Option<&'a str>>> {
    let Some(raw) = raw else { return Ok(None) };
    let v = raw.trim();
    if v.chars().count() > max {
        return Err(ApiError::bad_request(
            code,
            format!("{what} may be at most {max} characters"),
        ));
    }
    Ok(Some(if v.is_empty() { None } else { Some(v) }))
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
    let name = validated_name(&body.name)?;
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
            name,
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

/// One "Edit node" save: the node's own name and note, its profile + bound credential and
/// descriptive maker/model, and optionally a move to a different poll-pool.
///
/// 🚨 **Two different readings of "absent" live in this one body, and the split is deliberate.**
/// `profile_id`/`credential_id`/`vendor`/`model` are **replaced**: the node-edit UI loads the
/// current values and resends them, so omitting one CLEARS it. `pool`/`name`/`notes` are
/// **three-state**: omitting one LEAVES IT ALONE.
///
/// The asymmetry is not history, it is what the columns can survive. A blanked `vendor`/`model` is
/// refilled from the next poll's `sysDescr` (`fill_node_identity_batch`). **Nothing refills a name
/// or a note** — so an older client that has never heard of those fields must not be able to
/// destroy them by saving a form (ADR-135 decision 4; the trap `026e1ef8` paid for).
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
    /// The node's display name. **Absent** = leave it unchanged; otherwise rename the node.
    /// `""` (or whitespace) is **400**, not a clear — `nodes.name` is `NOT NULL`.
    ///
    /// Renaming is safe for everything downstream: `Node::id` is the identity every store is keyed
    /// by, so metric series and alert history follow the node across a rename. Nothing else in the
    /// product writes this column — no poll, no sweep, no classifier — so a hand-edited name stays.
    #[serde(default)]
    name: Option<String>,
    /// The operator's free-text note about this node. **Absent** = leave it unchanged; `""` (or
    /// whitespace) = clear it; otherwise set it. At most 2,000 characters.
    ///
    /// Spelled `notes` rather than `description` on purpose: in this product "Description" already
    /// means what a *device* reports about one of its ports (`interfaces.if_alias`).
    #[serde(default)]
    notes: Option<String>,
    /// The node's own labels. **Absent** = leave them unchanged; otherwise **the whole list is
    /// replaced** by what is sent — the edit dialog shows every label and resends every label, so
    /// a replacement is what the operator sees. `[]` clears them all.
    ///
    /// Free-form strings of at most 64 characters, at most 32 of them; trimmed, de-duplicated and
    /// sorted by the server. A label is what a `ScopeLevel::Group` threshold and a
    /// `WindowScope::Group` maintenance window match on.
    ///
    /// ⚠️ This sets only what the node itself carries. Labels it inherits from its folder are
    /// changed on the folder (`PUT /api/v1/node-groups/{id}/tags`) or refused here with
    /// `tags_excluded`.
    ///
    /// ⚠️ To add one label to many nodes without knowing what else they carry, use
    /// `POST /api/v1/nodes/tags`, which **merges**. Replacing from a bulk caller would silently
    /// wipe labels it never saw.
    #[serde(default)]
    tags: Option<Vec<String>>,
    /// Labels this node refuses to inherit from its folder chain. Same three-state and
    /// whole-value contract as `tags`; `[]` clears the refusals.
    ///
    /// ⚠️ **Not length- or character-checked**, unlike `tags`. A label already in the database may
    /// predate those rules, and refusing to let one be excluded because it is too long would make
    /// exactly the wrong labels unrefusable. Only trimmed and de-duplicated.
    ///
    /// An entry naming a label no folder currently supplies is kept, not dropped: if an ancestor
    /// re-adds it later, the refusal still holds.
    #[serde(default)]
    tags_excluded: Option<Vec<String>>,
}

#[utoipa::path(
    put, path = "/api/v1/nodes/{node_id}/bindings", tag = "nodes",
    params(("node_id" = Uuid, Path, description = "Node id")),
    request_body = NodeBindings,
    responses(
        (status = 204, description = "Bindings updated"),
        (status = 400, description = "Illegal pool name, an empty name, or a note over 2,000 characters", body = super::error::ErrorBody),
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
    // A rename is present-or-absent, never a clear: `nodes.name` is NOT NULL, so an empty string is
    // a mistake to report rather than an instruction to obey.
    let name_update = body
        .name
        .as_deref()
        .map(validated_name)
        .transpose()?
        .map(Some);
    let notes_update = free_text_update(body.notes.as_ref(), NOTES_MAX, "invalid_notes", "notes")?;
    let tags_update = body.tags.map(validated_labels).transpose()?;
    let excluded_update = body.tags_excluded.map(normalized_removals);
    let found = admin
        .repo
        .set_node_bindings(
            id,
            crate::repo::NodeBindingUpdate {
                profile: body.profile_id,
                credential: body.credential_id,
                vendor: trimmed(body.vendor.as_ref()),
                model: trimmed(body.model.as_ref()),
                pool: pool_update.as_ref().map(|inner| inner.as_deref()),
                name: name_update,
                notes: notes_update,
                tags: tags_update.as_deref(),
                tags_excluded: excluded_update.as_deref(),
            },
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
        (status = 400, description = "The destination folder does not exist", body = super::error::ErrorBody),
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
    // A folder that does not exist is a 400 that names it, rather than the foreign key turning
    // into a 500 that names nothing. Shared with the import and the bulk move (ADR-124 決定 1).
    super::groups::require_group_exists(&admin, body.group_id).await?;
    let found = admin
        .repo
        .set_node_group(id, body.group_id)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "set node group", "failed to move node")
        })?;
    node_write_result(found, id)
}

// ── Moving MANY nodes at once (ADR-124) ─────────────────────────────────────

/// Ceiling on one bulk move / preview.
///
/// 🚨 **Over the ceiling is a refusal, not a truncation** — deliberately unlike
/// [`NODE_NAMES_BATCH_MAX`] beside it, which silently drops the tail because a name that does not
/// come back falls back to the raw id and nothing is lost. Truncating a *write* would answer
/// "moved" while leaving everything past the cut where it was, with no way for the operator to see
/// which half (ADR-124 決定 7).
const NODE_MOVE_BATCH_MAX: usize = 1000;

/// Move many nodes into one folder (or `null` to ungroup them all).
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct BulkNodeMove {
    node_ids: Vec<Uuid>,
    #[serde(default)]
    group_id: Option<Uuid>,
}

/// What a bulk move actually did.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct BulkMoveResult {
    /// Distinct ids the request named, after de-duplication.
    requested: usize,
    /// Rows that actually moved. **Lower than `requested` is normal**: an id can name a node that
    /// has since been deleted, or one outside the caller's scope. The two are not distinguished.
    moved: u64,
}

/// The nodes to examine.
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct NodeIdBatch {
    node_ids: Vec<Uuid>,
}

/// One node, and the single folder whose IP range contains its address.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct PrefixProposal {
    node_id: Uuid,
    group_id: Uuid,
    /// The range that matched — shown so the operator can see *why* this folder is proposed.
    prefix: String,
}

/// One node claimed equally well by two or more folders. Never moved automatically.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct PrefixAmbiguity {
    node_id: Uuid,
    group_ids: Vec<Uuid>,
}

/// What the IP-range match proposes. **A proposal, not an action** — nothing is written by the
/// endpoint that returns this (ADR-124 決定 6).
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct MovePreviewResult {
    matched: Vec<PrefixProposal>,
    ambiguous: Vec<PrefixAmbiguity>,
    /// Ids whose address falls inside no visible folder's range.
    unmatched: Vec<Uuid>,
    /// Whether **any** folder this caller can see carries a range at all.
    ///
    /// Without this, a deployment with no NetBox reports every node as unmatched and the operator
    /// cannot tell "these addresses are not covered" from "there was never anything to match
    /// against" — one message for two situations is how an inert feature looks like a working one.
    any_prefixes: bool,
}

/// Shape this domain's DTOs from the shared fold (`crate::groups::fold_prefix_matches`).
///
/// The fold itself moved to `crate::groups` in ADR-131, because a second caller appeared that asks
/// the same question about **addresses that are not nodes yet**. What is left here is the part
/// that is genuinely about this domain: turning `(key, group, prefix)` into the node-shaped DTOs
/// this endpoint publishes.
fn node_prefix_dtos(
    fold: crate::groups::PrefixFold<Uuid>,
) -> (Vec<PrefixProposal>, Vec<PrefixAmbiguity>, Vec<Uuid>) {
    (
        fold.matched
            .into_iter()
            .map(|(node_id, group_id, prefix)| PrefixProposal {
                node_id,
                group_id,
                prefix,
            })
            .collect(),
        fold.ambiguous
            .into_iter()
            .map(|(node_id, group_ids)| PrefixAmbiguity { node_id, group_ids })
            .collect(),
        fold.unmatched,
    )
}

/// Add and/or remove tags across many nodes at once (ADR-135).
#[derive(Deserialize, utoipa::ToSchema)]
pub(super) struct BulkNodeTags {
    node_ids: Vec<Uuid>,
    /// Labels to add to every named node. One a node already carries is a no-op, and one not named
    /// here is left alone. Same validation as the single-node edit.
    ///
    /// ⚠️ Adds to each node's **own** labels. A label a node inherits from its folder is not
    /// touched by this, and cannot be removed by it.
    #[serde(default)]
    add: Vec<String>,
    /// Labels to take off every named node. One a node does not carry is not an error.
    ///
    /// ⚠️ Not length- or character-checked, for the reason `normalized_removals` records: a label
    /// already stored may predate the rules that now apply to new ones.
    #[serde(default)]
    remove: Vec<String>,
}

/// What a bulk tag edit actually did.
#[derive(Serialize, utoipa::ToSchema)]
pub(super) struct BulkTagResult {
    /// Distinct ids the request named, after de-duplication.
    requested: usize,
    /// Rows that were actually written. **Lower than `requested` is normal**: an id can name a node
    /// that has since been deleted, or one outside the caller's scope. The two are not
    /// distinguished — saying which would confirm that a node the caller may not see exists.
    applied: u64,
}

/// Label many nodes at once — the operation that makes tagging usable at all.
///
/// 🚨 **This MERGES; it does not replace.** The caller picked rows in the inventory tree and knows
/// one label it wants on all of them; it has no idea what else each of them carries. A replacing
/// bulk write would silently wipe every other label on every selected node
/// (`PUT /nodes/{id}/bindings` replaces, and that is correct there because the dialog shows the
/// whole map).
///
/// ⚠️ **Scoped via `Scoped`, not `Admin` alone.** `manage_config` is held by Operator, and an
/// Operator can be group-scoped, so a bulk write that skipped the scope would let one site's
/// operator relabel another's. This is the shape `POST /nodes/move` chose deliberately (ADR-124
/// decision 8) rather than inheriting the single-node writes' known-wrong `ADMIN_CFG` claim.
#[utoipa::path(
    post, path = "/api/v1/nodes/tags", tag = "nodes",
    request_body = BulkNodeTags,
    responses(
        (status = 200, description = "How many of the named nodes were relabelled", body = BulkTagResult),
        (status = 400, description = "An illegal tag key or value, or more ids than one request may carry", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn bulk_tag_nodes(
    _perm: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<BulkNodeTags>,
) -> ApiResult<Json<BulkTagResult>> {
    if body.node_ids.len() > NODE_MOVE_BATCH_MAX {
        return Err(ApiError::bad_request(
            "too_many_nodes",
            format!(
                "at most {NODE_MOVE_BATCH_MAX} nodes may be relabelled in one request, got {}",
                body.node_ids.len()
            ),
        ));
    }
    let add = validated_labels(body.add)?;
    let remove = normalized_removals(body.remove);
    let (requested, applied) = admin
        .repo
        .merge_node_tags(&body.node_ids, &add, &remove, scope.group_filter())
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "bulk tag nodes", "failed to tag nodes")
        })?;
    // The audit middleware records method and path only, so without this the log says that someone
    // relabelled something and never how much (the bulk move does the same).
    tracing::info!(
        requested,
        applied,
        added = add.len(),
        removed = remove.len(),
        "bulk node tag"
    );
    Ok(Json(BulkTagResult { requested, applied }))
}

#[utoipa::path(
    post, path = "/api/v1/nodes/move", tag = "nodes",
    request_body = BulkNodeMove,
    responses(
        (status = 200, description = "How many of the named nodes moved", body = BulkMoveResult),
        (status = 400, description = "Unknown destination folder, or more ids than one request may carry", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig, or the caller cannot see ungrouped nodes", body = super::error::ErrorBody),
        (status = 404, description = "The destination folder is not one this caller may act on", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn move_nodes(
    _perm: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<BulkNodeMove>,
) -> ApiResult<Json<BulkMoveResult>> {
    if body.node_ids.len() > NODE_MOVE_BATCH_MAX {
        return Err(ApiError::bad_request(
            "too_many_nodes",
            format!(
                "at most {NODE_MOVE_BATCH_MAX} nodes may be moved in one request, got {}",
                body.node_ids.len()
            ),
        ));
    }
    // The destination is checked before the ids: moving nodes *into* a folder this caller may not
    // act on would put them where that caller can no longer reach them.
    match body.group_id {
        Some(group) => super::scope::require_visible_group(&scope, group)?,
        None if !scope.allows_group(None) => {
            return Err(ApiError::forbidden_code(
                "out_of_scope",
                "this token cannot see ungrouped nodes",
            ))
        }
        None => {}
    }
    super::groups::require_group_exists(&admin, body.group_id).await?;
    let (requested, moved) = admin
        .repo
        .set_node_group_batch(&body.node_ids, body.group_id, scope.group_filter())
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "bulk move nodes", "failed to move nodes")
        })?;
    // The audit middleware records method and path only, so without this the log says that someone
    // moved something and never how much (`api/maintenance.rs` does the same for its bulk clear).
    tracing::info!(requested, moved, group = ?body.group_id, "bulk node move");
    Ok(Json(BulkMoveResult { requested, moved }))
}

#[utoipa::path(
    post, path = "/api/v1/nodes/move-preview", tag = "nodes",
    request_body = NodeIdBatch,
    responses(
        (status = 200, description = "Which folder's IP range contains each node's address", body = MovePreviewResult),
        (status = 400, description = "More ids than one request may carry", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn preview_move_by_prefix(
    _perm: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<NodeIdBatch>,
) -> ApiResult<Json<MovePreviewResult>> {
    if body.node_ids.len() > NODE_MOVE_BATCH_MAX {
        return Err(ApiError::bad_request(
            "too_many_nodes",
            format!(
                "at most {NODE_MOVE_BATCH_MAX} nodes may be examined in one request, got {}",
                body.node_ids.len()
            ),
        ));
    }
    let hits = admin
        .groups
        .match_prefixes(&body.node_ids, scope.group_filter())
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "match node prefixes",
                "failed to match prefixes",
            )
        })?;
    let any_prefixes = admin
        .groups
        .any_prefixes(scope.group_filter())
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "read group prefixes", "failed to read prefixes")
        })?;
    let (matched, ambiguous, unmatched) =
        node_prefix_dtos(crate::groups::fold_prefix_matches(&body.node_ids, hits));
    Ok(Json(MovePreviewResult {
        matched,
        ambiguous,
        unmatched,
        any_prefixes,
    }))
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
    let dispatched = admin.dispatcher.poll_now(&node, &pool).await;
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

    /// The batch form's edge validation (ADR-125). Both of these are refusals a caller can reach
    /// without a database, and both are refusals rather than quiet repairs: a request that names
    /// more folders than allowed, or names one that is not a UUID, must not come back looking like
    /// a complete answer to a smaller question.
    #[tokio::test]
    async fn the_batch_form_refuses_a_bad_group_list_rather_than_trimming_it() {
        use crate::api::tests_support::send;

        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "viewer1",
        );

        // 🚨 Over the cap: refused, not truncated. Truncating would answer about the first 64 of
        // the 65 folders asked for, with a 200 and no way for the caller to tell.
        let too_many = (0..=BY_GROUP_BATCH_MAX)
            .map(|_| Uuid::new_v4().to_string())
            .collect::<Vec<_>>()
            .join(",");
        let (status, body) = send(
            &st,
            "GET",
            &format!("/api/v1/nodes/by-group?groups={too_many}"),
            &token,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "too_many_groups");

        // An unparseable id is the same shape of mistake: it would otherwise be dropped by the
        // filter and the folder would simply be missing from the answer.
        let (status, body) = send(
            &st,
            "GET",
            "/api/v1/nodes/by-group?groups=not-a-uuid",
            &token,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_group_id");

        // ⚠️ And the single-group form is untouched — an older WebUI keeps working unchanged.
        let (status, _) = send(
            &st,
            "GET",
            &format!("/api/v1/nodes/by-group?group={}", Uuid::new_v4()),
            &token,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    /// The N-1 contract, from the side this core can actually demonstrate: the single-group form
    /// carries no `answered`, so a newer WebUI talking to a core that predates the batch form sees
    /// its absence and falls back. `skip_serializing_if` is what makes that absence real rather
    /// than a `null` the client would have to special-case.
    #[tokio::test]
    async fn the_single_group_form_carries_no_answered_echo() {
        use crate::api::tests_support::send;

        let st = private_state();
        let token = st.sessions.issue(
            Uuid::new_v4(),
            Principal::new(Role::Viewer, Scope::All),
            "viewer1",
        );
        let (status, body) = send(
            &st,
            "GET",
            &format!("/api/v1/nodes/by-group?group={}", Uuid::new_v4()),
            &token,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.get("answered").is_none(),
            "the single-group form must look exactly as it did before ADR-125"
        );

        // The batch form echoes the set even when it can answer nothing, which is the whole point:
        // "no rows" and "this core does not understand the question" must not look alike.
        let g = Uuid::new_v4();
        let (status, body) = send(
            &st,
            "GET",
            &format!("/api/v1/nodes/by-group?groups={g}"),
            &token,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["answered"], serde_json::json!([g.to_string()]));
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
    // ── An accepted write (ADR-115) ──────────────────────────────────────────────────

    /// Creating a node answers 201, writes the row, and the node is then listed.
    ///
    /// The whole point of ADR-115: before it, no test in this module had ever seen a 201 from any
    /// endpoint, because every fixture was skeleton mode and this handler answered 503.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn creating_a_node_writes_the_row_and_then_lists_it(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/nodes",
            &tok,
            Some(serde_json::json!({ "name": "core-sw-01", "address": "10.0.0.1" })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");
        assert_eq!(crate::pgtest::rows(&pool, "nodes").await, 1);

        let (status, list) = send(&st, "GET", "/api/v1/nodes", &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{list}");
        assert!(list.to_string().contains("core-sw-01"), "{list}");
    }

    /// Renaming a node and writing its note are **accepted**, and the detail shows both (ADR-135).
    ///
    /// ⚠️ The status is named rather than `is_success()`: this route documents **204**, and a check
    /// that cannot tell 204 from 200 would pass against the wrong one (ADR-115).
    ///
    /// 🚨 **The second save is the half that matters.** It sends only `pool`, which is what every
    /// client written before this change sends — and asserts the name and the note survived it. A
    /// version of this test with only the first save passes against an implementation that blanks
    /// both on every write, which is the data-loss bug the three-state reading exists to prevent.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_rename_and_a_note_are_accepted_and_survive_an_unrelated_save(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let id = crate::pgtest::node(&pool, "typo-sw-01", 1, None).await;
        let path = format!("/api/v1/nodes/{id}/bindings");

        let (status, body) = send(
            &st,
            "PUT",
            &path,
            &tok,
            Some(serde_json::json!({
                "name": "core-sw-01",
                "notes": "in the ceiling void; needs a ladder",
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");

        let (status, detail) = send(&st, "GET", &format!("/api/v1/nodes/{id}"), &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{detail}");
        assert_eq!(detail["name"], "core-sw-01", "{detail}");
        assert_eq!(
            detail["notes"], "in the ceiling void; needs a ladder",
            "{detail}"
        );

        // What an N-1 client sends: a save that has never heard of either field.
        let (status, body) = send(
            &st,
            "PUT",
            &path,
            &tok,
            Some(serde_json::json!({ "pool": "edge" })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");

        let (_, detail) = send(&st, "GET", &format!("/api/v1/nodes/{id}"), &tok, None).await;
        assert_eq!(detail["name"], "core-sw-01", "the rename was undone");
        assert_eq!(
            detail["notes"], "in the ceiling void; needs a ladder",
            "the note was destroyed by an unrelated save"
        );
        assert_eq!(detail["pool"], "edge", "{detail}");

        // An empty name is refused rather than obeyed — the column is NOT NULL.
        let (status, body) = send(
            &st,
            "PUT",
            &path,
            &tok,
            Some(serde_json::json!({ "name": "   " })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "invalid_name", "{body}");

        // An empty note is obeyed: that is how an operator deletes one.
        let (status, _) = send(
            &st,
            "PUT",
            &path,
            &tok,
            Some(serde_json::json!({ "notes": "" })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT);
        let (_, detail) = send(&st, "GET", &format!("/api/v1/nodes/{id}"), &tok, None).await;
        assert_eq!(detail["notes"], serde_json::Value::Null, "{detail}");
    }

    /// A bulk tag edit is **accepted**, it **merges**, and the node detail shows the result.
    ///
    /// ⚠️ The status is named, not `is_success()`: this route documents 200 with a body.
    ///
    /// 🚨 The first node is seeded with a label the bulk call never mentions, and that label is
    /// asserted afterwards. Without it, an implementation that replaced the whole map would pass —
    /// and would silently strip every other label off every node an operator ever bulk-tags.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_bulk_tag_is_accepted_and_merges_into_what_is_already_there(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let a = crate::pgtest::node(&pool, "a", 1, None).await;
        let b = crate::pgtest::node(&pool, "b", 2, None).await;

        // Give `a` a label through the single-node path, which replaces.
        let (status, body) = send(
            &st,
            "PUT",
            &format!("/api/v1/nodes/{a}/bindings"),
            &tok,
            Some(serde_json::json!({ "tags": { "role": "core" } })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");

        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/nodes/tags",
            &tok,
            Some(serde_json::json!({
                "node_ids": [a, b],
                "add": { "region": "JAPAN" },
            })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["requested"], 2, "{body}");
        assert_eq!(body["applied"], 2, "{body}");

        let (_, detail) = send(&st, "GET", &format!("/api/v1/nodes/{a}"), &tok, None).await;
        assert_eq!(detail["tags"]["region"], "JAPAN", "{detail}");
        assert_eq!(
            detail["tags"]["role"], "core",
            "the bulk add replaced the map instead of merging into it: {detail}"
        );

        // A key that is not a legal tag key is refused rather than stored.
        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/nodes/tags",
            &tok,
            Some(serde_json::json!({ "node_ids": [a], "add": { "has space": "x" } })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "invalid_tag", "{body}");
    }

    /// A bulk move is **accepted** and the rows actually move (ADR-115's shape, ADR-124's route).
    ///
    /// ⚠️ The status is named, not `is_success()`: this endpoint documents 200 with a body, and
    /// nine of the write routes measured in ADR-115 are 204s — a check that cannot tell them apart
    /// would pass on the wrong one.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_bulk_move_is_accepted_and_the_nodes_land_in_the_folder(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let dest = crate::pgtest::group(&pool, "Tokyo").await;
        let a = crate::pgtest::node(&pool, "a", 1, None).await;
        let b = crate::pgtest::node(&pool, "b", 2, None).await;

        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/nodes/move",
            &tok,
            Some(serde_json::json!({ "node_ids": [a, b], "group_id": dest })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["requested"], 2, "{body}");
        assert_eq!(body["moved"], 2, "{body}");

        let repo = crate::pgtest::repo(pool);
        for id in [a, b] {
            let node = repo.get_node(id).await.expect("read").expect("the node");
            assert_eq!(node.group.map(|g| g.0), Some(dest), "{id} did not move");
        }
    }

    /// An unknown destination is a 400 that names it, not the foreign key's 500 that names nothing.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn moving_into_a_folder_that_does_not_exist_is_refused_by_name(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let a = crate::pgtest::node(&pool, "a", 1, None).await;
        let ghost = uuid::Uuid::new_v4();

        // Both paths share one helper, so both are checked here — the single-node route was the
        // one that used to 500 (ADR-124 決定 1).
        for (method, path) in [
            ("POST", "/api/v1/nodes/move".to_string()),
            ("PUT", format!("/api/v1/nodes/{a}/group")),
        ] {
            let body = if method == "POST" {
                serde_json::json!({ "node_ids": [a], "group_id": ghost })
            } else {
                serde_json::json!({ "group_id": ghost })
            };
            let (status, out) = send(&st, method, &path, &tok, Some(body)).await;
            assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{path}: {out}");
            assert_eq!(out["error"]["code"], "invalid_group", "{path}: {out}");
        }
    }

    /// Over the ceiling is refused outright — a truncated *write* would report a move it did not
    /// make for everything past the cut.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_batch_over_the_ceiling_is_refused_rather_than_truncated(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let ids: Vec<uuid::Uuid> = (0..=NODE_MOVE_BATCH_MAX)
            .map(|_| uuid::Uuid::new_v4())
            .collect();

        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/nodes/move",
            &tok,
            Some(serde_json::json!({ "node_ids": ids, "group_id": null })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "too_many_nodes", "{body}");
    }

    /// A viewer may read the inventory and may not rearrange it.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_viewer_cannot_move_nodes(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let a = crate::pgtest::node(&pool, "a", 1, None).await;
        let body = serde_json::json!({ "node_ids": [a], "group_id": null });

        let viewer = token(&st, yagra_common::Role::Viewer);
        for path in ["/api/v1/nodes/move", "/api/v1/nodes/move-preview"] {
            let (status, out) = send(&st, "POST", path, &viewer, Some(body.clone())).await;
            assert_eq!(status, axum::http::StatusCode::FORBIDDEN, "{path}: {out}");
        }
    }

    /// The preview proposes and **writes nothing** — the property the whole feature rests on.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_preview_proposes_a_folder_and_moves_nothing(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let site = crate::pgtest::group(&pool, "Tokyo").await;
        crate::pgtest::prefix(&pool, site, "10.0.0.0/24").await;
        let inside = crate::pgtest::node(&pool, "inside", 7, None).await;
        let outside =
            crate::pgtest::node_at(&pool, "outside", "192.168.9.9".parse().expect("addr"), None)
                .await;

        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/nodes/move-preview",
            &tok,
            Some(serde_json::json!({ "node_ids": [inside, outside] })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["matched"][0]["node_id"], inside.to_string(), "{body}");
        assert_eq!(body["matched"][0]["group_id"], site.to_string(), "{body}");
        assert_eq!(body["unmatched"][0], outside.to_string(), "{body}");
        assert_eq!(body["any_prefixes"], true, "{body}");

        let repo = crate::pgtest::repo(pool);
        let node = repo
            .get_node(inside)
            .await
            .expect("read")
            .expect("the node");
        assert_eq!(node.group, None, "the preview moved a node");
    }

    /// `any_prefixes` is false where no folder carries a range — the difference between "your
    /// addresses do not match" and "there was nothing to match against".
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_preview_says_when_there_were_no_ranges_at_all(pool: sqlx::PgPool) {
        use crate::api::tests_support::{live_state, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let a = crate::pgtest::node(&pool, "a", 1, None).await;

        let (status, body) = send(
            &st,
            "POST",
            "/api/v1/nodes/move-preview",
            &tok,
            Some(serde_json::json!({ "node_ids": [a] })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["any_prefixes"], false, "{body}");
        assert_eq!(body["unmatched"][0], a.to_string(), "{body}");
    }
}
