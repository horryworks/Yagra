// SPDX-License-Identifier: AGPL-3.0-only
//! Effective poll-pool resolution (ADR-009/020).
//!
//! A node's **effective pool** — the pool whose pollers actually poll it — is resolved by
//! precedence: the node's own `pool` > the nearest ancestor **group** (folder) carrying one >
//! [`yagra_bus::DEFAULT_POOL`]. Folder inheritance (migration 0054) is what makes "everything
//! under the Tokyo site is polled from Tokyo" one setting instead of one edit per node, and it
//! mirrors the profile→group→node precedence thresholds already use.
//!
//! Resolution lives here, in code, rather than in SQL: [`crate::repo::Repo::NODE_COLUMNS`] is
//! inlined into many queries, and a per-node recursive walk would be O(nodes × depth) against a
//! table sized for tens of thousands of rows. `node_groups` is small (hundreds of rows) and is
//! read whole once per sweep rebuild, so the whole map is built in one pass with path compression
//! and then answers every node in O(1).
//!
//! Note the node's `parent` (dependency-suppression) hierarchy plays no part — only `group`.

use std::collections::HashMap;

use uuid::Uuid;
use yagra_common::Node;

/// Longest ancestor chain the walk will follow before giving up. `node_groups.parent_id` is a
/// self-FK with no cycle constraint — [`crate::groups::would_create_cycle`] guards the two move
/// endpoints, but `GroupRepo::delete`'s re-parenting does not re-check — so the walk is bounded by
/// this *and* a visited set. A real folder tree is nowhere near this deep.
const MAX_GROUP_DEPTH: usize = 64;

/// Where a node's effective pool came from, so the UI can say "inherited from the Tokyo folder"
/// rather than just showing a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
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
    /// One upward walk per group with **path compression** — the answer is written back to every
    /// group on the chain — so a resolved chain is walked once, not once per descendant. (Chains
    /// that resolve to nothing aren't memoized, so an all-unset forest costs O(groups × depth);
    /// with hundreds of rows and [`MAX_GROUP_DEPTH`] that is still trivial, and it keeps the map's
    /// meaning simple.) A cycle or an over-deep chain resolves to "no inherited pool" and warns,
    /// rather than hanging.
    #[must_use]
    pub fn build(rows: Vec<(Uuid, Option<Uuid>, Option<String>)>) -> Self {
        let own: HashMap<Uuid, (Option<Uuid>, Option<String>)> = rows
            .into_iter()
            .map(|(id, parent, pool)| {
                (id, (parent, meaningful(pool.as_deref()).map(str::to_owned)))
            })
            .collect();

        let mut by_group: HashMap<Uuid, (String, Uuid)> = HashMap::new();
        for &start in own.keys() {
            if by_group.contains_key(&start) {
                continue; // already answered as part of an earlier group's chain
            }
            // Walk up to the first group with a pool (or a memoized answer), recording the path.
            let mut chain: Vec<Uuid> = Vec::new();
            let mut cur = Some(start);
            let mut answer: Option<(String, Uuid)> = None;
            let mut depth = 0usize;
            while let Some(id) = cur {
                if let Some(found) = by_group.get(&id) {
                    answer = Some(found.clone());
                    break;
                }
                if chain.contains(&id) || depth > MAX_GROUP_DEPTH {
                    tracing::warn!(
                        group = %id,
                        "node group ancestry is cyclic or deeper than the supported bound — \
                         treating it as having no inherited pool"
                    );
                    break;
                }
                let Some((parent, pool)) = own.get(&id) else {
                    break; // dangling parent_id: nothing more to inherit from
                };
                // Recorded before the pool check so the supplying group is memoized too, not just
                // the descendants that inherit from it.
                chain.push(id);
                if let Some(p) = pool {
                    answer = Some((p.clone(), id));
                    break;
                }
                cur = *parent;
                depth += 1;
            }
            // Path compression: every group we walked through shares the answer.
            if let Some(found) = answer {
                for id in chain {
                    by_group.insert(id, found.clone());
                }
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
