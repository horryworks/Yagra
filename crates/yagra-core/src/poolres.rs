// SPDX-License-Identifier: AGPL-3.0-only
//! Effective poll-pool resolution (ADR-009/020).
//!
//! A node's **effective pool** — the pool whose pollers actually poll it — is resolved by
//! precedence: the node's own `pool` > the nearest ancestor **group** (folder) carrying one >
//! [`yagra_bus::DEFAULT_POOL`]. Folder inheritance (migration 0054) is what makes "everything
//! under the Tokyo site is polled from Tokyo" one setting instead of one edit per node, and it
//! mirrors the profile→group→node precedence thresholds already use.
//!
//! Resolution lives here, in code, rather than in SQL: `NodeRepo::NODE_COLUMNS` is
//! inlined into many queries, and a per-node recursive walk would be O(nodes × depth) against a
//! table sized for tens of thousands of rows. `node_groups` is small (hundreds of rows) and is
//! read whole once per sweep rebuild, so the whole map is built in one pass with path compression
//! and then answers every node in O(1).
//!
//! Note the node's `parent` (dependency-suppression) hierarchy plays no part — only `group`.

use std::collections::HashMap;

use uuid::Uuid;
use yagra_common::Node;

/// Where a node's effective pool came from, so the UI can say "inherited from the Tokyo folder"
/// rather than just showing a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PoolSource {
    /// The node carries its own `pool`.
    Node,
    /// Inherited from an ancestor group (see [`ResolvedPool::group`]).
    Group,
    /// Nothing set anywhere — the implicit [`yagra_bus::DEFAULT_POOL`].
    Default,
}

/// A node's effective pool plus where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPool {
    /// The effective pool name (never empty).
    pub pool: String,
    /// Which level supplied it.
    pub source: PoolSource,
    /// The group that supplied it, when `source == Group`.
    pub group: Option<Uuid>,
}

/// Trim a stored pool value to its meaningful form: blank/whitespace counts as **unset**, matching
/// the API's "empty clears the pool" semantics, and defends against legacy rows written before
/// validation existed.
fn meaningful(pool: Option<&str>) -> Option<&str> {
    pool.map(str::trim).filter(|p| !p.is_empty())
}

/// Precomputed effective pool per group, built once from the whole `node_groups` table.
///
/// Every group maps to the pool it (or its nearest ancestor) supplies, plus which group that was.
/// A group whose whole chain is unset is absent from the map.
#[derive(Debug, Default, Clone)]
pub struct PoolResolver {
    by_group: HashMap<Uuid, (String, Uuid)>,
}

impl PoolResolver {
    /// A resolver that knows no groups — every node falls back to its own pool or the default.
    /// Used in skeleton mode and wherever the group table isn't wired.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build from the `(id, parent_id, pool)` rows of `node_groups` (see
    /// [`crate::groups::GroupRepo::pool_rows`]).
    ///
    /// The traversal itself — path compression, the cycle guard, the depth bound — is
    /// [`crate::groups::resolve_nearest_ancestor`], shared with map-coordinate inheritance. What
    /// belongs here is only what is specific to pools: [`meaningful`] deciding that a blank value
    /// means "unset" rather than a pool literally named `""`.
    #[must_use]
    pub fn build(rows: Vec<(Uuid, Option<Uuid>, Option<String>)>) -> Self {
        let by_group =
            crate::groups::resolve_nearest_ancestor(rows.into_iter().map(|(id, parent, pool)| {
                (id, parent, meaningful(pool.as_deref()).map(str::to_owned))
            }));
        Self { by_group }
    }

    /// The node's effective pool with provenance.
    #[must_use]
    pub fn resolve(&self, node: &Node) -> ResolvedPool {
        if let Some(own) = meaningful(node.pool.as_deref()) {
            return ResolvedPool {
                pool: own.to_owned(),
                source: PoolSource::Node,
                group: None,
            };
        }
        if let Some((pool, group)) = node.group.and_then(|g| self.by_group.get(&g.as_uuid())) {
            return ResolvedPool {
                pool: pool.clone(),
                source: PoolSource::Group,
                group: Some(*group),
            };
        }
        ResolvedPool {
            pool: yagra_bus::DEFAULT_POOL.to_owned(),
            source: PoolSource::Default,
            group: None,
        }
    }

    /// The node's effective pool as a borrowed name — the hot path (once per node per sweep), so
    /// it allocates nothing.
    #[must_use]
    pub fn resolve_pool<'a>(&'a self, node: &'a Node) -> &'a str {
        if let Some(own) = meaningful(node.pool.as_deref()) {
            return own;
        }
        node.group
            .and_then(|g| self.by_group.get(&g.as_uuid()))
            .map_or(yagra_bus::DEFAULT_POOL, |(pool, _)| pool.as_str())
    }
}

/// What a pool move has to know about the nodes currently resolving to `pool` (ADR-107 増分 3).
///
/// 🚨 **The two fields are not the same question, and conflating them is the defect this type was
/// added to end.** `total` is *who is affected* — what the confirmation dialog counts and what the
/// `409` reports. `fall_through` is *who has to be written* — the nodes that resolve to the pool
/// with no column anywhere naming it, so no `UPDATE … WHERE pool = $1` can reach them. The
/// original move used one literal count for both and therefore moved nothing in the one state
/// every deployment starts in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PoolMembers {
    /// How many nodes this pool is polling, by any of the three routes in [`PoolSource`].
    pub total: usize,
    /// The nodes that resolve here **only** by falling through to the implicit default
    /// ([`PoolSource::Default`]) — nothing on the node and nothing on any ancestor folder names a
    /// pool. Moving the pool has to pin these explicitly; there is no row to rewrite otherwise.
    ///
    /// Necessarily empty unless `pool` is [`yagra_bus::DEFAULT_POOL`], since that is the only
    /// name a fall-through can produce.
    pub fall_through: Vec<Uuid>,
}

impl PoolResolver {
    /// Everything a move of `pool` needs: how many nodes it is polling, and which of them no
    /// column names.
    ///
    /// Takes the node list rather than a repo so it stays pure and testable without a database —
    /// the caller supplies [`crate::pool_coverage::pool_dependent_nodes`], which is also what the
    /// coverage strip counts, so `total` is the number already on the operator’s screen.
    #[must_use]
    pub fn members(&self, nodes: &[Node], pool: &str) -> PoolMembers {
        let mut out = PoolMembers::default();
        for n in nodes {
            let resolved = self.resolve(n);
            if resolved.pool != pool {
                continue;
            }
            out.total += 1;
            if resolved.source == PoolSource::Default {
                out.fall_through.push(n.id.as_uuid());
            }
        }
        out
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::groups::MAX_GROUP_DEPTH;
    use std::net::{IpAddr, Ipv4Addr};
    use yagra_common::GroupId;

    fn g(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// A node with an optional own pool and an optional group.
    fn node(pool: Option<&str>, group: Option<Uuid>) -> Node {
        let mut n = Node::new(
            yagra_common::NodeId::new(),
            "n",
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        );
        n.pool = pool.map(str::to_owned);
        n.group = group.map(GroupId::from);
        n
    }

    fn row(
        id: Uuid,
        parent: Option<Uuid>,
        pool: Option<&str>,
    ) -> (Uuid, Option<Uuid>, Option<String>) {
        (id, parent, pool.map(str::to_owned))
    }

    #[test]
    fn node_pool_overrides_its_group() {
        let r = PoolResolver::build(vec![row(g(1), None, Some("tokyo"))]);
        let resolved = r.resolve(&node(Some("osaka"), Some(g(1))));
        assert_eq!(resolved.pool, "osaka");
        assert_eq!(resolved.source, PoolSource::Node);
        assert_eq!(resolved.group, None);
        assert_eq!(r.resolve_pool(&node(Some("osaka"), Some(g(1)))), "osaka");
    }

    #[test]
    fn nearest_ancestor_wins_over_a_farther_one() {
        // root(tokyo) → mid(none) → leaf(edge) ; a node in `mid` inherits tokyo, one in `leaf` edge.
        let r = PoolResolver::build(vec![
            row(g(1), None, Some("tokyo")),
            row(g(2), Some(g(1)), None),
            row(g(3), Some(g(2)), Some("edge")),
        ]);
        let mid = r.resolve(&node(None, Some(g(2))));
        assert_eq!(mid.pool, "tokyo");
        assert_eq!(mid.source, PoolSource::Group);
        assert_eq!(
            mid.group,
            Some(g(1)),
            "provenance names the supplying group"
        );

        let leaf = r.resolve(&node(None, Some(g(3))));
        assert_eq!(leaf.pool, "edge");
        assert_eq!(leaf.group, Some(g(3)));
    }

    #[test]
    fn unset_chains_and_unknown_groups_fall_back_to_default() {
        let r = PoolResolver::build(vec![
            row(g(1), None, None),
            row(g(2), Some(g(1)), None),
            // A dangling parent_id (the referenced group is gone) must not panic or hang.
            row(g(4), Some(g(9)), None),
        ]);
        for group in [None, Some(g(1)), Some(g(2)), Some(g(4)), Some(g(7))] {
            let resolved = r.resolve(&node(None, group));
            assert_eq!(resolved.pool, yagra_bus::DEFAULT_POOL);
            assert_eq!(resolved.source, PoolSource::Default);
        }
        assert_eq!(
            PoolResolver::empty().resolve_pool(&node(None, Some(g(1)))),
            yagra_bus::DEFAULT_POOL
        );
    }

    #[test]
    fn blank_pool_values_count_as_unset() {
        // Legacy rows written before the API validated pool names, and the API's own "empty string
        // clears the pool" semantics, must both read as "inherit", never as a pool literally named
        // "" (which would publish to `yagra.jobs.`).
        let r = PoolResolver::build(vec![
            row(g(1), None, Some("tokyo")),
            row(g(2), Some(g(1)), Some("   ")),
        ]);
        assert_eq!(r.resolve(&node(None, Some(g(2)))).pool, "tokyo");
        assert_eq!(r.resolve(&node(Some("  "), Some(g(2)))).pool, "tokyo");
        // Surrounding whitespace on a real value is trimmed rather than passed through.
        assert_eq!(r.resolve(&node(Some(" osaka "), None)).pool, "osaka");
    }

    #[test]
    fn cyclic_ancestry_resolves_to_default_without_hanging() {
        // Self-cycle and a 2-cycle. `would_create_cycle` guards the move endpoints but there is no
        // DB constraint, so the resolver must survive one.
        let r = PoolResolver::build(vec![row(g(1), Some(g(1)), None)]);
        assert_eq!(
            r.resolve(&node(None, Some(g(1)))).pool,
            yagra_bus::DEFAULT_POOL
        );

        let r = PoolResolver::build(vec![
            row(g(1), Some(g(2)), None),
            row(g(2), Some(g(1)), None),
        ]);
        assert_eq!(
            r.resolve(&node(None, Some(g(1)))).pool,
            yagra_bus::DEFAULT_POOL
        );
        assert_eq!(
            r.resolve(&node(None, Some(g(2)))).pool,
            yagra_bus::DEFAULT_POOL
        );

        // A cycle *below* a group that does carry a pool still answers for that group itself.
        let r = PoolResolver::build(vec![
            row(g(1), Some(g(2)), None),
            row(g(2), Some(g(1)), None),
            row(g(3), None, Some("tokyo")),
        ]);
        assert_eq!(r.resolve(&node(None, Some(g(3)))).pool, "tokyo");
        assert_eq!(
            r.resolve(&node(None, Some(g(1)))).pool,
            yagra_bus::DEFAULT_POOL
        );
    }

    #[test]
    fn chain_deeper_than_the_bound_resolves_to_default_without_hanging() {
        let deep = MAX_GROUP_DEPTH + 10;
        let mut rows = vec![row(g(0), None, Some("root-pool"))];
        for i in 1..=deep as u128 {
            rows.push(row(g(i), Some(g(i - 1)), None));
        }
        let r = PoolResolver::build(rows);
        // Near the root, inheritance still works.
        assert_eq!(r.resolve(&node(None, Some(g(1)))).pool, "root-pool");
        // Past the bound it degrades to the default rather than walking forever.
        let deepest = r.resolve(&node(None, Some(g(deep as u128))));
        assert!(
            deepest.pool == "root-pool" || deepest.pool == yagra_bus::DEFAULT_POOL,
            "an over-deep chain either resolves via compression or degrades, never hangs"
        );
    }

    #[test]
    fn memoized_answers_match_a_naive_walk() {
        // Property check over a deterministic pseudo-random forest: path compression must not
        // change any answer relative to walking each chain independently.
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let n = 200u128;
        let mut rows = Vec::new();
        for i in 0..n {
            // Parent is always a lower index (acyclic forest); ~1 in 4 groups carries a pool.
            let parent = if i == 0 || next() % 3 == 0 {
                None
            } else {
                Some(g(next() as u128 % i))
            };
            let pool = if next() % 4 == 0 {
                Some(format!("pool-{}", next() % 5))
            } else {
                None
            };
            rows.push((g(i), parent, pool));
        }
        let own: HashMap<Uuid, (Option<Uuid>, Option<String>)> = rows
            .iter()
            .map(|(id, p, pool)| (*id, (*p, pool.clone())))
            .collect();
        let naive = |start: Uuid| -> String {
            let mut cur = Some(start);
            for _ in 0..=own.len() {
                let Some(id) = cur else { break };
                let Some((parent, pool)) = own.get(&id) else {
                    break;
                };
                if let Some(p) = meaningful(pool.as_deref()) {
                    return p.to_owned();
                }
                cur = *parent;
            }
            yagra_bus::DEFAULT_POOL.to_owned()
        };

        let r = PoolResolver::build(rows.clone());
        for (id, _, _) in &rows {
            assert_eq!(
                r.resolve(&node(None, Some(*id))).pool,
                naive(*id),
                "group {id} resolved differently from a naive walk"
            );
        }
    }
    /// The defect ADR-107 増分 3 fixes, stated as the thing that has to stay true.
    ///
    /// 🚨 A pool whose members all *inherit* is the state a fresh deployment is in — nothing writes
    /// a pool on a node until somebody does. `SELECT count(*) FROM nodes WHERE pool = 'default'`
    /// answers 0 there, which is what let the move's safety check pass while the strip beside it
    /// showed 32 and the operator's answer was discarded.
    #[test]
    fn a_pool_of_purely_inheriting_nodes_is_not_empty() {
        // folder 1 says `default`; folder 2 says nothing.
        let r = PoolResolver::build(vec![
            row(g(1), None, Some("default")),
            row(g(2), None, None),
        ]);
        let nodes = [
            node(None, Some(g(1))), // inherits `default` from its folder
            node(None, Some(g(2))), // folder has no pool → falls through
            node(None, None),       // no folder at all → falls through
        ];
        let m = r.members(&nodes, yagra_bus::DEFAULT_POOL);
        assert_eq!(
            m.total, 3,
            "every one of them is polled by the default pool"
        );
        assert_eq!(
            m.fall_through.len(),
            2,
            "only the two with no pool named anywhere have to be pinned; the third travels with \
             its folder"
        );
        assert!(m.fall_through.contains(&nodes[1].id.as_uuid()));
        assert!(m.fall_through.contains(&nodes[2].id.as_uuid()));
        assert!(
            !m.fall_through.contains(&nodes[0].id.as_uuid()),
            "pinning a node whose folder is moving would write a row for nothing"
        );
    }

    /// The acceptance half: a node that names the pool itself is counted but never pinned — the
    /// existing `UPDATE nodes … WHERE pool = $1` already reaches it, and pinning it again would
    /// make `moved.0` count it twice.
    #[test]
    fn a_node_that_names_the_pool_is_counted_but_not_pinned() {
        let r = PoolResolver::empty();
        let nodes = [node(Some("default"), None), node(Some("tokyo"), None)];
        let m = r.members(&nodes, yagra_bus::DEFAULT_POOL);
        assert_eq!(m.total, 1, "the tokyo node is in another pool");
        assert!(m.fall_through.is_empty());
    }

    /// A non-default pool can never have a fall-through member: inheritance bottoming out always
    /// produces `DEFAULT_POOL`. So moving `tokyo` is entirely served by the two column rewrites.
    #[test]
    fn only_the_default_pool_can_have_fall_through_members() {
        let r = PoolResolver::build(vec![row(g(1), None, Some("tokyo"))]);
        let nodes = [
            node(None, Some(g(1))),    // inherits tokyo
            node(Some("tokyo"), None), // names tokyo
            node(None, None),          // falls through to default, not tokyo
        ];
        let m = r.members(&nodes, "tokyo");
        assert_eq!(m.total, 2);
        assert!(
            m.fall_through.is_empty(),
            "nothing falls through to a named pool, so nothing needs pinning"
        );
    }
}
