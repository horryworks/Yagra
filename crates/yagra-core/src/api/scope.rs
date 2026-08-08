// SPDX-License-Identifier: AGPL-3.0-only
//! Group-scoped visibility for the read paths (ADR-014 / ADR-019).
//!
//! A [`Principal`] carries a [`Scope`] — either unrestricted or a set of folder-group ids. Until
//! now nothing consulted it: `Principal::can_see` had no caller anywhere under `api/`, so every
//! list endpoint returned the whole fleet regardless of who asked, and the two surfaces that
//! *could* hand out a group scope refused to rather than mint a credential that meant nothing
//! (`api/api_tokens.rs`, `mcp/mod.rs`). This module is the one place that changes.
//!
//! ## The unit of filtering is the **group** id set, not the node id set
//!
//! The obvious design — resolve the caller's allowed *node* ids once per request and filter on
//! them — is `SELECT id FROM nodes WHERE group_id = ANY(…)` on every request. At the 50k-node
//! target that is exactly the full-fleet scan the scalability audit spent S2/S6/S7 removing, and it
//! would reintroduce it on the hottest path in the API. So nothing here ever enumerates nodes.
//!
//! Instead a scope resolves to a set of **group** ids — bounded by what an operator typed, so
//! hundreds rather than tens of thousands — and:
//!
//!  - SQL paths get one indexed predicate, `AND group_id = ANY($n)` (`nodes_group_idx`, migration
//!    0014), bound rather than interpolated (`security.md`).
//!  - In-memory paths (the alert engine's aggregates, SSE, TSDB/ClickHouse post-filtering) ask
//!    [`NodeScope::allows_node`], a hash lookup into the fleet-wide node→group map the alert config
//!    already maintains, plus a set membership.
//!
//! ## What is cached, and what invalidates it
//!
//! The `(id, parent_id)` **edges** are cached, not the expansion. Expansion is a BFS over a few
//! hundred edges — microseconds, so it runs per request and no invalidation problem exists for it.
//! The edges are re-read only when [`crate::config_gen`] advances, which `audit_mw` bumps on every
//! successful config mutation including all of `/node-groups` (ADR-026, the same idiom `SweepCache`
//! and the alert-config reloader use). Cost is one small query per config generation.
//!
//! ## Fail-closed, in both directions
//!
//! Two inversions would each turn this from a restriction into a privilege escalation, and both are
//! tested below:
//!
//!  - A scope naming only groups that no longer exist must see **nothing**. `Scope::group_uuids`
//!    keeps the empty case as `Some(vec![])`, distinct from `None` (unrestricted).
//!  - A node whose group is unknown — created since the last edge/meta refresh, or genuinely
//!    ungrouped — is **invisible** to a scoped caller, matching `Scope::allows`' own rule that an
//!    ungrouped node is not leaked. So a node can be briefly invisible after creation (until the
//!    next refresh), never briefly visible to someone who should not see it.

use super::{ApiError, ApiState};
use crate::groups::{group_ancestors, group_subtree};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use uuid::Uuid;
use yagra_common::{NodeId, Principal};

/// The groups a scoped principal may see, expanded once per request from the cached edges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeSet {
    /// Every allowed root plus its whole subtree — the set a node's `group_id` must be in. This is
    /// what becomes the SQL predicate and what [`NodeScope::allows_node`] tests.
    pub visible: Vec<Uuid>,
    /// The ancestor chain above each allowed root. **Breadcrumb only** — these groups are named so
    /// the inventory tree has a spine to hang the visible subtree from, and carry no membership.
    pub breadcrumb: Vec<Uuid>,
}

impl ScopeSet {
    /// Whether a node in `group` is inside the scope. `None` (ungrouped, or a group the caller
    /// cannot see) is **not** visible — see the module docs.
    #[must_use]
    fn allows(&self, group: Option<Uuid>) -> bool {
        group.is_some_and(|g| self.visible.contains(&g))
    }
}

/// A resolved visibility scope for one request.
///
/// [`NodeScope::All`] is not merely "a scope containing everything": it is the *absence* of
/// filtering, and every path keeps its pre-scope fast path under it byte-for-byte. That matters
/// because unrestricted is the overwhelmingly common case (every admin, and every deployment that
/// has not assigned a scope to anyone), so scoping must cost it nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeScope {
    /// Unrestricted — no filtering at all.
    All,
    /// Restricted to these groups.
    Groups(Arc<ScopeSet>),
}

impl NodeScope {
    /// The scope that sees nothing.
    ///
    /// A placeholder for a field that must hold a scope before the real one is resolved — see
    /// [`crate::rca::orchestrator::RcaRequest::scope`]. It is empty rather than [`Self::All`] for
    /// the reason `YagraMcp::scope_of`'s fallback is: the value nobody meant to use must be the one
    /// that shows nothing, because the branch nobody expects to reach is the one that becomes
    /// reachable without anyone noticing.
    #[must_use]
    pub fn sees_nothing() -> Self {
        NodeScope::Groups(Arc::new(ScopeSet {
            visible: Vec::new(),
            breadcrumb: Vec::new(),
        }))
    }

    /// Whether this scope restricts anything. Handlers use it to keep the unrestricted fast path.
    #[must_use]
    pub fn is_all(&self) -> bool {
        matches!(self, NodeScope::All)
    }

    /// The group ids to bind into a `group_id = ANY($n)` predicate, or `None` when unrestricted.
    ///
    /// An empty slice is a meaningful answer — "no groups", i.e. match nothing — and callers must
    /// not treat it as "no filter". Postgres evaluates `= ANY('{}')` as false for every row, which
    /// is the correct reading; the danger is a caller that special-cases empty and drops the
    /// predicate. `a_scope_naming_only_deleted_groups_sees_nothing` pins that.
    #[must_use]
    pub fn group_filter(&self) -> Option<&[Uuid]> {
        match self {
            NodeScope::All => None,
            NodeScope::Groups(s) => Some(&s.visible),
        }
    }

    /// Whether a node whose folder group is `group` is visible.
    #[must_use]
    pub fn allows_group(&self, group: Option<Uuid>) -> bool {
        match self {
            NodeScope::All => true,
            NodeScope::Groups(s) => s.allows(group),
        }
    }

    /// Whether `node` is visible, resolving its group through the alert engine's fleet-wide node
    /// metadata (the map is already maintained for threshold resolution, so this adds no I/O).
    ///
    /// A node the map has never seen — created since the last config-generation refresh — is
    /// treated as **not** visible. Fail-closed: briefly invisible to its owner beats briefly
    /// visible to everyone else.
    #[must_use]
    pub fn allows_node(&self, st: &ApiState, node: NodeId) -> bool {
        self.allows_node_in(&st.alerts, node)
    }

    /// [`Self::allows_node`] against the alert engine directly.
    ///
    /// ⚠️ The split exists for the SSE filters, and it is not cosmetic. A long-lived stream that
    /// captured an `ApiState` would hold a strong `Arc` to the very [`crate::alerts::AlertManager`]
    /// whose broadcast sender feeds it — so the sender could never be dropped and **the stream could
    /// never end on its own**. The stream handlers hold a `Weak` and call this instead. (Found by
    /// `route_table::every_listed_route_is_served`, which reads each route's body to completion and
    /// simply hung.)
    #[must_use]
    pub fn allows_node_in(&self, alerts: &crate::alerts::AlertManager, node: NodeId) -> bool {
        match self {
            NodeScope::All => true,
            NodeScope::Groups(s) => s.allows(alerts.node_folder_group(node)),
        }
    }

    /// Whether an alert about `subject` is visible.
    ///
    /// A **node** subject resolves the usual way. A **pool** subject — Yagra alerting on its own
    /// polling coverage (ADR-009) — is visible to a scoped caller when any node they can see is
    /// polled by that pool, which is precisely the operator the alert is *for*: their site is the
    /// one that went dark.
    ///
    /// ⚠️ This is the whole reason the subject is a sum type rather than a synthetic node id. A
    /// made-up id would resolve to no folder group, `ScopeSet::allows` is fail-closed, and the
    /// alert would therefore be hidden from every group-scoped operator while staying visible to
    /// unrestricted ones — the exact inversion of who needs it. See `yagra_alert::Subject`.
    #[must_use]
    pub fn allows_subject(&self, st: &ApiState, subject: &yagra_alert::Subject) -> bool {
        self.allows_subject_in(&st.alerts, subject)
    }

    /// [`Self::allows_subject`] against the alert engine directly — see [`Self::allows_node_in`]
    /// for why the SSE filters need this form.
    #[must_use]
    pub fn allows_subject_in(
        &self,
        alerts: &crate::alerts::AlertManager,
        subject: &yagra_alert::Subject,
    ) -> bool {
        match self {
            NodeScope::All => true,
            NodeScope::Groups(s) => match subject {
                yagra_alert::Subject::Node(n) => s.allows(alerts.node_folder_group(*n)),
                yagra_alert::Subject::Pool(p) => alerts.pool_is_in_any_group(p, &s.visible),
            },
        }
    }

    /// Whether a *group* row itself may be listed. Visible groups carry membership; breadcrumb
    /// ancestors are listed so the tree has a spine, but nothing beneath them is exposed by that.
    #[must_use]
    pub fn allows_group_row(&self, group: Uuid) -> bool {
        match self {
            NodeScope::All => true,
            NodeScope::Groups(s) => s.visible.contains(&group) || s.breadcrumb.contains(&group),
        }
    }
}

/// What a stored row that carries **its own** scope points at.
///
/// Maintenance windows, mutes and analysis runs all persist "what this row is about" as a kind plus
/// an id, and all three then need the same question answered: may a caller with this group scope see
/// it? That decision is here, once ([`NodeScope::allows_target`]).
///
/// ⚠️ **The mapping from a row's columns to this type stays in the row's own module, exhaustively
/// over that module's enum, and must not be shared.** The three spellings genuinely disagree — a
/// maintenance window's `group` scope is a *tag value* while its folder group is `group_id`, and a
/// mute's `group` **is** the folder group. A converter keyed on the serialized string would read a
/// window's tag as a folder id (an operator may legitimately tag a node with a UUID) and hand a
/// scoped caller somebody else's window. What is shared here is only the *decision*, never the
/// reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeTarget {
    /// One node.
    Node(NodeId),
    /// One folder group, and everything beneath it.
    Group(Uuid),
    /// Fleet-wide, a device class, a tag value — or a target that could not be read at all. A set
    /// of nodes that no group scope bounds, so only an unrestricted caller may have it.
    Unbounded,
}

impl NodeScope {
    /// Whether a row aimed at `target` is visible to this scope.
    ///
    /// [`ScopeTarget::Unbounded`] resolves to "unrestricted callers only". That covers the
    /// unreadable case too, deliberately: a row whose target cannot be parsed is *less* bounded than
    /// one that can, so treating it as unbounded is the fail-closed reading. Defaulting it to some
    /// node id would instead expose the row to whoever happens to hold that node.
    #[must_use]
    pub fn allows_target(&self, st: &ApiState, target: ScopeTarget) -> bool {
        self.allows_target_in(&st.alerts, target)
    }

    /// [`Self::allows_target`] against the alert engine directly — see [`Self::allows_node_in`] for
    /// why the SSE filters need this form.
    #[must_use]
    pub fn allows_target_in(
        &self,
        alerts: &crate::alerts::AlertManager,
        target: ScopeTarget,
    ) -> bool {
        match target {
            ScopeTarget::Node(n) => self.allows_node_in(alerts, n),
            ScopeTarget::Group(g) => self.allows_group(Some(g)),
            ScopeTarget::Unbounded => self.is_all(),
        }
    }
}

/// Reject a node id taken from a **request body** that the caller cannot see.
///
/// The path-param case has its own extractor ([`super::extract::VisibleNode`]); this is for the
/// operator actions that name their target in the body — ack, mute, maintenance, RCA, analysis.
/// Those need it as much as the reads do, and for a reason that is easy to miss: they are gated on
/// `AckAlerts`/`ManageMaintenance`, which a **scoped operator holds**, and the 200-vs-404 difference
/// on `POST /alerts/ack` answers "does this invisible node currently have an alert with this
/// (check, severity)". A write oracle is still a read.
///
/// Same 404 as the extractor, for the same reason — see [`super::extract::VisibleNode`].
pub fn require_visible_node(
    st: &ApiState,
    scope: &NodeScope,
    node: NodeId,
) -> Result<(), ApiError> {
    if scope.allows_node(st, node) {
        Ok(())
    } else {
        Err(ApiError::not_found(
            "node_not_found",
            format!("no node {}", node.as_uuid()),
        ))
    }
}

/// Reject a folder group the caller may not act on.
///
/// Stricter than [`NodeScope::allows_group_row`] on purpose: a breadcrumb ancestor may be *named*
/// so the inventory tree has a spine, but acting on it — a group-wide mute or maintenance window —
/// would reach every node beneath it, including the ones outside the scope. Only a group whose
/// membership the caller can actually see qualifies.
pub fn require_visible_group(scope: &NodeScope, group: Uuid) -> Result<(), ApiError> {
    if scope.allows_group(Some(group)) {
        Ok(())
    } else {
        Err(ApiError::not_found(
            "group_not_found",
            format!("no group {group}"),
        ))
    }
}

/// Refuse a group-scoped caller an answer that carries no per-node attribution to narrow.
///
/// Four endpoints reach this rule — a rendered report, the fleet state timeline, fleet throughput,
/// and the fleet-wide flow aggregates. Each was written out separately, and ADR-042's MCP tools
/// were about to write the fifth and sixth. A block that differs only in a sentence, repeated six
/// times, is the shape `extensibility.md` §3 exists to stop: the copy that drifts is always the one
/// written last, and here "drifts" means serving a scoped caller the fleet's numbers.
///
/// `why` is **this** endpoint's reason and is the only part that differs. It is what the operator
/// reads and what the route's [`Scoping::Refused`](super::route_table) ledger line repeats, so it
/// stays at the call site. The code is fixed at `scope_unsupported` because the WebUI branches on
/// exactly one value.
///
/// ⚠️ **Not** the rule in `analysis.rs::require_launchable_scope` or the maintenance-window target
/// check. Those share this error code and say something different — that a scoped caller must name
/// a node or a folder group rather than a profile, a tag, or the whole fleet. Folding them in here
/// would be unifying two rules because their error strings look alike.
pub fn require_fleet_wide(scope: &NodeScope, why: &'static str) -> Result<(), ApiError> {
    if scope.is_all() {
        Ok(())
    } else {
        Err(ApiError::forbidden_code("scope_unsupported", why))
    }
}

/// How much extra a ranking fetches before scope-filtering it.
///
/// A Top-N comes back already ranked by a store that knows nothing about groups (VictoriaMetrics
/// ranks by value, PostgreSQL by fire count), so the filter can only run afterwards — and filtering
/// N rows down would return fewer than N. Asking for `N × this` first makes that unlikely without
/// pretending it is impossible: a scoped caller whose nodes are all below the fleet's top `N × 10`
/// still gets a short list. The alternative — pushing group membership into the TSDB as a label —
/// was rejected on cardinality grounds (CLAUDE.md names it the single biggest design risk).
///
/// Costs the unrestricted caller nothing: [`ranking_fetch_limit`] returns the plain limit for
/// [`NodeScope::All`].
pub const RANKING_OVERFETCH: usize = 10;

/// How many rows to ask a ranking store for, given the caller's requested `limit`.
#[must_use]
pub fn ranking_fetch_limit(scope: &NodeScope, limit: usize) -> usize {
    if scope.is_all() {
        limit
    } else {
        limit.saturating_mul(RANKING_OVERFETCH)
    }
}

/// The `(id, parent_id)` edge list of the folder tree — the shape [`crate::groups::GroupRepo::edges`]
/// returns and both tree walks consume.
type GroupEdges = Arc<Vec<(Uuid, Option<Uuid>)>>;

/// Cached edges plus the config generation they were read at.
static EDGE_CACHE: Mutex<Option<(u64, GroupEdges)>> = Mutex::new(None);

/// The group edges, re-read only when the config generation has advanced (ADR-026).
async fn edges(st: &ApiState) -> Result<GroupEdges, ApiError> {
    let generation = crate::config_gen::current();
    if let Some((at, cached)) = EDGE_CACHE.lock().expect("edge cache poisoned").as_ref() {
        if *at == generation {
            return Ok(cached.clone());
        }
    }
    // Skeleton mode has no group store. A scoped principal therefore resolves to an empty scope
    // and sees nothing, which is the fail-closed direction; an unrestricted one never gets here.
    let Some(admin) = st.admin.as_ref() else {
        return Ok(Arc::new(Vec::new()));
    };
    let fresh = Arc::new(admin.groups.edges().await.map_err(|e| {
        ApiError::from_internal(e.as_ref(), "read group edges", "failed to resolve scope")
    })?);
    // Re-read the generation: if a mutation landed while the query was in flight, store the older
    // generation so the next request re-reads rather than pinning stale edges to a fresh number.
    *EDGE_CACHE.lock().expect("edge cache poisoned") = Some((generation, fresh.clone()));
    Ok(fresh)
}

/// A folder group and everything beneath it, from the same cached edges the scope resolver uses.
///
/// For the endpoints that let a caller *filter by* a group. That is a different thing from the
/// caller's own scope — it narrows a result set the caller was already entitled to — but it wants
/// the same subtree reading: asking for a site means asking for its racks too, and a caller who had
/// to name every descendant would get a different answer from the one the same group id gives a
/// scope. `require_visible_group` is what keeps the two from being confused: an out-of-scope group
/// is refused before it ever reaches this.
pub async fn subtree_of(st: &ApiState, root: Uuid) -> Result<Vec<Uuid>, ApiError> {
    Ok(group_subtree(&edges(st).await?, root))
}

/// Resolve a principal's scope for this request.
pub async fn resolve(st: &ApiState, principal: &Principal) -> Result<NodeScope, ApiError> {
    let Some(roots) = principal.scope.group_uuids() else {
        return Ok(NodeScope::All);
    };
    if let yagra_common::Scope::Groups(raw) = &principal.scope {
        if raw.len() != roots.len() {
            // Not fatal — the parseable entries still apply — but it means a stored scope holds
            // something that is not a group id, which no writer should ever produce.
            tracing::warn!(
                named = raw.len(),
                usable = roots.len(),
                "scope holds entries that are not group ids; they restrict nothing and were dropped"
            );
        }
    }
    let edges = edges(st).await?;
    Ok(NodeScope::Groups(Arc::new(expand(&edges, &roots))))
}

/// Expand allowed roots into the visible subtree and the breadcrumb chain above it. Pure, so the
/// set maths is unit-tested without a database or an `ApiState`.
#[must_use]
pub fn expand(edges: &[(Uuid, Option<Uuid>)], roots: &[Uuid]) -> ScopeSet {
    let mut visible = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        for g in group_subtree(edges, *root) {
            if seen.insert(g) {
                visible.push(g);
            }
        }
    }
    let mut breadcrumb = Vec::new();
    let mut up_seen = HashSet::new();
    for root in roots {
        for g in group_ancestors(edges, *root) {
            // An ancestor that is itself inside the scope (nested allowed roots) belongs to
            // `visible`, not to the breadcrumb — it must not be downgraded to a name-only row.
            if !seen.contains(&g) && up_seen.insert(g) {
                breadcrumb.push(g);
            }
        }
    }
    ScopeSet {
        visible,
        breadcrumb,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::{Role, Scope};

    fn ids(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// tokyo → {edge → {rack}}, osaka (separate root), london (unrelated root).
    fn tree() -> Vec<(Uuid, Option<Uuid>)> {
        vec![
            (ids(1), None),         // tokyo
            (ids(2), Some(ids(1))), // edge (under tokyo)
            (ids(3), Some(ids(2))), // rack (under edge)
            (ids(4), None),         // osaka
            (ids(5), None),         // london
        ]
    }

    fn scope_of(roots: &[Uuid]) -> NodeScope {
        NodeScope::Groups(Arc::new(expand(&tree(), roots)))
    }

    #[test]
    fn require_fleet_wide_admits_only_the_unrestricted_caller() {
        assert!(require_fleet_wide(&NodeScope::All, "because").is_ok());
        let err = require_fleet_wide(&scope_of(&[ids(1)]), "because").expect_err("refused");
        // Built rather than written, so `no_handler_spells_the_scope_refusal_by_hand` below —
        // which reads this very file — does not count this assertion as a hand-spelled refusal.
        assert_eq!(err.code(), format!("{}_unsupported", "scope"));
    }

    /// **The fifth copy is the one that gets it wrong.** This rule was written out four times
    /// before it had a name, and ADR-042's MCP tools were about to make it six. Spelling the
    /// refusal's error code into a handler by hand is now a mistake, not a convention.
    ///
    /// Two sites keep the code and are deliberately exempt: `analysis.rs` and `maintenance.rs`
    /// answer a **different** question with the same code — that a scoped caller must name a node
    /// or a folder group rather than a profile, a tag, or the fleet. They are reached only *after*
    /// an `is_all()` check, so folding them in here would unify two rules because their error
    /// strings look alike (`extensibility.md` §6, the guard-that-enumerates-pairs trap, run
    /// backwards).
    #[test]
    fn no_handler_spells_the_scope_refusal_by_hand() {
        // Needle assembled at runtime: this test lives in a file the scan reads, so a literal
        // would match itself and pass forever — the lesson from `reports.rs`'s run-state SQL test.
        let needle = format!("\"{}_unsupported\"", "scope");
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api");
        let mut sites = Vec::new();
        for entry in std::fs::read_dir(&dir).expect("read src/api") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let src = std::fs::read_to_string(&path).expect("read module");
            let hits = src.matches(&needle).count();
            if hits > 0 {
                let name = path
                    .file_name()
                    .and_then(|f| f.to_str())
                    .unwrap_or("?")
                    .to_owned();
                sites.push((name, hits));
            }
        }
        sites.sort();
        assert_eq!(
            sites,
            vec![
                ("analysis.rs".to_owned(), 1),
                ("maintenance.rs".to_owned(), 1),
                ("scope.rs".to_owned(), 1),
            ],
            "the fleet-wide refusal is spelled somewhere new — call require_fleet_wide instead, or \
             if this really is the analysis/maintenance target rule, say so here"
        );
    }

    #[test]
    fn group_scope_expands_to_the_whole_subtree() {
        // Reuses `group_subtree` rather than reimplementing the walk — a second copy of the
        // descent rule is how a scope and a group mute come to disagree about the same tree.
        let s = expand(&tree(), &[ids(1)]);
        let mut visible = s.visible.clone();
        visible.sort();
        assert_eq!(visible, vec![ids(1), ids(2), ids(3)]);
        assert!(s.breadcrumb.is_empty(), "a root has nothing above it");
    }

    #[test]
    fn a_scope_on_a_nested_group_carries_ancestors_but_not_siblings() {
        // Scoped to `edge`: sees edge + rack, names tokyo so the tree has a spine, and must NOT
        // see osaka/london — nor gain membership of tokyo by being able to name it.
        let s = expand(&tree(), &[ids(2)]);
        let mut visible = s.visible.clone();
        visible.sort();
        assert_eq!(visible, vec![ids(2), ids(3)]);
        assert_eq!(s.breadcrumb, vec![ids(1)]);
        assert!(!s.visible.contains(&ids(4)) && !s.visible.contains(&ids(5)));
    }

    #[test]
    fn a_breadcrumb_ancestor_that_is_also_in_scope_stays_visible() {
        // Allowed roots {tokyo, edge}: tokyo is edge's ancestor *and* in scope. It must keep its
        // membership rather than being downgraded to a name-only breadcrumb row.
        let s = expand(&tree(), &[ids(1), ids(2)]);
        assert!(s.visible.contains(&ids(1)));
        assert!(!s.breadcrumb.contains(&ids(1)));
    }

    #[test]
    fn overlapping_roots_do_not_duplicate_groups() {
        let s = expand(&tree(), &[ids(1), ids(2), ids(3)]);
        let mut visible = s.visible.clone();
        visible.sort();
        assert_eq!(visible, vec![ids(1), ids(2), ids(3)]);
    }

    #[test]
    fn an_ungrouped_node_is_invisible_to_a_scoped_principal() {
        // Mirrors `rbac.rs::group_scope_filters_visibility` at the API edge: a node in no group is
        // visible only under `All`. A scoped caller must not inherit the ungrouped bucket.
        assert!(!scope_of(&[ids(1)]).allows_group(None));
        assert!(NodeScope::All.allows_group(None));
    }

    #[test]
    fn a_scope_naming_only_deleted_groups_sees_nothing() {
        // The inversion that would be a privilege escalation: an empty group set must produce a
        // predicate matching nothing, NOT the absence of a predicate.
        let s = scope_of(&[]);
        assert_eq!(s.group_filter(), Some(&[][..]));
        assert!(!s.is_all());
        assert!(!s.allows_group(Some(ids(1))));
        assert!(!s.allows_group(None));
    }

    #[test]
    fn a_scope_naming_an_unknown_group_still_restricts_to_it() {
        // A group id with no edge row (deleted, or created since the last cache refresh) resolves
        // to itself: the caller sees nodes in that group if any remain, and nothing else.
        let ghost = ids(99);
        let s = scope_of(&[ghost]);
        assert!(s.allows_group(Some(ghost)));
        assert!(!s.allows_group(Some(ids(1))));
    }

    #[tokio::test]
    async fn a_pool_alert_reaches_the_scope_that_owns_a_node_in_that_pool() {
        // ⚠️ The inversion this whole design exists to avoid: with a synthetic node id the alert
        // would resolve to no folder group, `ScopeSet::allows` is fail-closed, and the operator
        // responsible for the site that just went dark would be the one person unable to see it.
        use yagra_alert::Subject;
        let st = crate::api::tests_support::public_state();
        st.alerts.set_config(
            crate::alerts::AlertConfig::new(Vec::new(), std::collections::HashMap::new())
                .with_pool_groups(std::collections::HashMap::from([(
                    "tokyo".to_owned(),
                    std::collections::BTreeSet::from([ids(2)]),
                )])),
        );
        // Scoped to `tokyo` (ids(1)), whose subtree contains the group holding the pool's nodes.
        let mine = scope_of(&[ids(1)]);
        assert!(mine.allows_subject(&st, &Subject::Pool("tokyo".to_owned())));
        // An unrelated root sees nothing of it …
        assert!(!scope_of(&[ids(5)]).allows_subject(&st, &Subject::Pool("tokyo".to_owned())));
        // … nor does a scope with no groups, and nor does a pool the snapshot has never seen.
        assert!(!scope_of(&[]).allows_subject(&st, &Subject::Pool("tokyo".to_owned())));
        assert!(!mine.allows_subject(&st, &Subject::Pool("osaka".to_owned())));
        // Unrestricted sees every subject, as it does for nodes.
        assert!(NodeScope::All.allows_subject(&st, &Subject::Pool("osaka".to_owned())));
    }

    #[test]
    fn the_unrestricted_scope_filters_nothing() {
        assert!(NodeScope::All.is_all());
        assert_eq!(NodeScope::All.group_filter(), None);
        assert!(NodeScope::All.allows_group(Some(ids(1))));
        assert!(NodeScope::All.allows_group_row(ids(5)));
    }

    #[test]
    fn breadcrumb_ancestors_are_listable_but_hold_no_members() {
        let s = scope_of(&[ids(2)]);
        // tokyo may be listed (the tree needs it) …
        assert!(s.allows_group_row(ids(1)));
        // … but a node sitting directly in tokyo is not visible.
        assert!(!s.allows_group(Some(ids(1))));
        // An unrelated root is neither.
        assert!(!s.allows_group_row(ids(5)));
    }

    #[tokio::test]
    async fn an_all_scope_principal_resolves_without_touching_the_group_store() {
        // Skeleton mode has no group store at all; an unrestricted principal must still resolve.
        let st = crate::api::tests_support::public_state();
        let p = Principal::new(Role::Admin, Scope::All);
        assert_eq!(resolve(&st, &p).await.unwrap(), NodeScope::All);
    }

    #[tokio::test]
    async fn a_scoped_principal_in_skeleton_mode_sees_nothing() {
        // No group store ⇒ no edges ⇒ the scope resolves to its bare roots and nothing expands.
        // The fail-closed direction: it must not degrade to `All`.
        let st = crate::api::tests_support::public_state();
        let p = Principal::new(Role::Viewer, Scope::groups([ids(1).to_string()]));
        let resolved = resolve(&st, &p).await.unwrap();
        assert!(!resolved.is_all());
        assert!(!resolved.allows_group(Some(ids(2))));
    }
}
