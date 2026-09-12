// SPDX-License-Identifier: AGPL-3.0-only
//! Effective label resolution (ADR-135 inc. 2).
//!
//! A node's **effective labels** are its own, plus every label its inventory folder and that
//! folder's ancestors supply, minus the ones it refuses. "Tag the Japan site" is then one setting
//! instead of one edit per node — and, unlike a bulk copy onto the nodes that existed at the time,
//! it keeps applying to every node discovered afterwards.
//!
//! Resolution lives here, in code, for the reason [`crate::poolres`] and map-coordinate
//! inheritance both record: `node_groups` is small (hundreds of rows) and is read whole once per
//! rebuild, while `nodes` is sized for tens of thousands, so a per-node recursive walk in SQL
//! would be O(nodes × depth) against the big table. And a materialized copy on the node row is
//! worse than either — it goes stale the moment a parent is edited or a folder is moved.
//!
//! Note the node's `parent` (dependency-suppression) hierarchy plays no part — only `group`.

use std::collections::{BTreeSet, HashMap};

use uuid::Uuid;
use yagra_common::Node;

/// A ceiling on how many labels one node can effectively carry.
///
/// 🚨 **Per-node and per-folder caps do not bound this on their own.** The API refuses more than
/// 32 labels on any one node and any one folder, but a node ten folders deep inherits from all of
/// them — up to 320 — and every one of those would ride into JSM's `tags` array, PagerDuty's
/// `custom_details`, `NodeMeta::tag_groups` (matched against every stored rule) and the MCP DTO.
///
/// ⚠️ **This number is a judgement, not a measurement.** It is set where a page still reads and a
/// rule match is still cheap; nothing has been measured against a real deployment carrying a deep
/// labelled tree, because none exists yet.
pub const EFFECTIVE_LABELS_MAX: usize = 64;

/// Trim a stored label to its meaningful form: blank/whitespace counts as **absent**, matching the
/// API's validation, and defends against rows written before that validation existed (migration
/// 0109 converts from a column that never had any).
fn meaningful(label: &str) -> Option<&str> {
    let t = label.trim();
    (!t.is_empty()).then_some(t)
}

/// Precomputed effective labels per folder, built once from the whole `node_groups` table.
///
/// 🚨 **There is deliberately no `TagSource` enum**, and the absence is the design rather than an
/// omission. [`crate::poolres::PoolSource`] and [`crate::groups::GeoSource`] exist because a pool
/// and a map pin each have exactly **one** supplier, so "where did this come from" has one answer.
/// A label does not: a node and two folders above it can all supply the same one. Provenance here
/// is therefore per label, and the only question a screen actually asks — *which of these are
/// mine* — is `effective − own`, computable on the row with no second walk. An enum would have to
/// pick one supplier and would be wrong whenever it mattered.
#[derive(Debug, Default, Clone)]
pub struct TagResolver {
    by_group: HashMap<Uuid, BTreeSet<String>>,
}

impl TagResolver {
    /// A resolver that knows no folders — every node carries exactly its own labels. Used in
    /// skeleton mode, and wherever a folder read failed and the caller chose to degrade rather
    /// than fail (see `api::util::tag_resolver`).
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build from the `(id, parent_id, tags, tags_excluded)` rows of `node_groups` (see
    /// [`crate::groups::GroupRepo::tag_rows`]).
    ///
    /// The traversal itself — the cycle guard, the depth bound, the memoization — is
    /// [`crate::groups::accumulate_ancestor_labels`]. What belongs here is only what is specific
    /// to labels: [`meaningful`], deciding a blank string is not a label.
    #[must_use]
    pub fn build(rows: Vec<crate::groups::LabelRow>) -> Self {
        let by_group = crate::groups::accumulate_ancestor_labels(rows.into_iter().map(
            |(id, parent, add, remove)| {
                (
                    id,
                    parent,
                    add.iter()
                        .filter_map(|s| meaningful(s))
                        .map(str::to_owned)
                        .collect(),
                    remove
                        .iter()
                        .filter_map(|s| meaningful(s))
                        .map(str::to_owned)
                        .collect(),
                )
            },
        ));
        Self { by_group }
    }

    /// What a folder itself effectively carries: its own labels plus everything above it.
    /// Allocation-free — the empty set for an unknown or ungrouped id.
    #[must_use]
    pub fn for_group(&self, group: Option<Uuid>) -> &BTreeSet<String> {
        static EMPTY: std::sync::LazyLock<BTreeSet<String>> =
            std::sync::LazyLock::new(BTreeSet::new);
        group.and_then(|g| self.by_group.get(&g)).unwrap_or(&EMPTY)
    }

    /// Just the half a node inherits: what its folder chain supplies, minus what it excludes, and
    /// minus anything it already carries itself. This is what the detail pane draws as a second,
    /// marked group of badges.
    #[must_use]
    pub fn inherited(&self, node: &Node) -> Vec<String> {
        let own: BTreeSet<&str> = node.tags.iter().filter_map(|s| meaningful(s)).collect();
        let refused: BTreeSet<&str> = node
            .tags_excluded
            .iter()
            .filter_map(|s| meaningful(s))
            .collect();
        self.for_group(node.group.map(|g| g.as_uuid()))
            .iter()
            .filter(|l| !refused.contains(l.as_str()) && !own.contains(l.as_str()))
            .take(EFFECTIVE_LABELS_MAX)
            .cloned()
            .collect()
    }

    /// The node's effective labels: **its own first, then the inherited ones**, each half sorted.
    ///
    /// 🚨 **The ordering is load-bearing, not cosmetic.** Two consumers take a prefix of this list
    /// rather than the whole of it — `rca::context::cap_tags` keeps the first 8 for the LLM
    /// prompt, and [`EFFECTIVE_LABELS_MAX`] bounds the rest — so whatever sorts last is what gets
    /// dropped. Sorting the union as one list would let three tagged folders push a node's *own*
    /// labels out of its own incident prompt, silently. Own-first means a node never loses what
    /// somebody typed onto it specifically.
    ///
    /// It is still fully deterministic (each half is sorted), which is what the prompt fingerprint
    /// needs; it is simply not globally sorted.
    #[must_use]
    pub fn effective(&self, node: &Node) -> Vec<String> {
        let mut own: Vec<String> = node
            .tags
            .iter()
            .filter_map(|s| meaningful(s))
            .map(str::to_owned)
            .collect();
        own.sort();
        own.dedup();
        let room = EFFECTIVE_LABELS_MAX.saturating_sub(own.len());
        let mut out = own;
        if room > 0 {
            let inherited = self.inherited(node);
            if inherited.len() > room {
                tracing::debug!(
                    node = %node.id,
                    dropped = inherited.len() - room,
                    "node inherits more labels than the effective cap allows; keeping its own \
                     labels and the first inherited ones"
                );
            }
            out.extend(inherited.into_iter().take(room));
        }
        out.truncate(EFFECTIVE_LABELS_MAX);
        out
    }

    /// The same answer as a set, for the callers that only ask "does it carry X" — the threshold
    /// scope and the maintenance-window scope.
    #[must_use]
    pub fn effective_set(&self, node: &Node) -> BTreeSet<String> {
        self.effective(node).into_iter().collect()
    }

    /// The effective labels of a node the resolver cannot see as a [`Node`] — the notification
    /// path, which reads a narrower row. Same fold, same ordering, same cap.
    #[must_use]
    pub fn effective_parts(
        &self,
        group: Option<Uuid>,
        own: &[String],
        excluded: &[String],
    ) -> Vec<String> {
        let mut mine: Vec<String> = own
            .iter()
            .filter_map(|s| meaningful(s))
            .map(str::to_owned)
            .collect();
        mine.sort();
        mine.dedup();
        let refused: BTreeSet<&str> = excluded.iter().filter_map(|s| meaningful(s)).collect();
        let already: BTreeSet<&str> = mine.iter().map(String::as_str).collect();
        let room = EFFECTIVE_LABELS_MAX.saturating_sub(mine.len());
        let inherited: Vec<String> = self
            .for_group(group)
            .iter()
            .filter(|l| !refused.contains(l.as_str()) && !already.contains(l.as_str()))
            .take(room)
            .cloned()
            .collect();
        let mut out = mine;
        out.extend(inherited);
        out.truncate(EFFECTIVE_LABELS_MAX);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use yagra_common::{GroupId, NodeId};

    fn node(group: Option<Uuid>, own: &[&str], excluded: &[&str]) -> Node {
        let mut n = Node::new(NodeId::new(), "n", IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
        n.group = group.map(GroupId::from);
        n.tags = own.iter().map(|s| (*s).to_owned()).collect();
        n.tags_excluded = excluded.iter().map(|s| (*s).to_owned()).collect();
        n
    }

    fn row(
        id: Uuid,
        parent: Option<Uuid>,
        add: &[&str],
        remove: &[&str],
    ) -> crate::groups::LabelRow {
        (
            id,
            parent,
            add.iter().map(|s| (*s).to_owned()).collect(),
            remove.iter().map(|s| (*s).to_owned()).collect(),
        )
    }

    /// 🚨 **The test that must differ from `poolres`.** Its twin,
    /// `nearest_ancestor_wins_over_a_farther_one`, asserts the opposite: there the nearer folder
    /// replaces what the farther one said. Here both contribute, and that is the whole reason
    /// `accumulate_ancestor_labels` exists beside `resolve_nearest_ancestor`.
    #[test]
    fn every_ancestor_contributes_not_just_the_nearest() {
        let (region, site, rack) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let r = TagResolver::build(vec![
            row(region, None, &["JAPAN"], &[]),
            row(site, Some(region), &["matsuyama"], &[]),
            row(rack, Some(site), &["rack-3"], &[]),
        ]);
        assert_eq!(
            r.effective(&node(Some(rack), &[], &[])),
            vec!["JAPAN", "matsuyama", "rack-3"]
        );
    }

    /// `poolres` has `node_pool_overrides_its_group`; nothing overrides here, so this is the twin
    /// that had to be written the other way up.
    #[test]
    fn a_node_and_its_folder_both_contribute() {
        let g = Uuid::new_v4();
        let r = TagResolver::build(vec![row(g, None, &["JAPAN"], &[])]);
        // Own first, then inherited — see `effective`'s doc for why that ordering is not cosmetic.
        assert_eq!(
            r.effective(&node(Some(g), &["spare"], &[])),
            vec!["spare", "JAPAN"]
        );
    }

    #[test]
    fn an_unset_chain_and_a_dangling_parent_resolve_to_no_labels() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let r = TagResolver::build(vec![
            row(a, None, &[], &[]),
            row(b, Some(Uuid::new_v4()), &[], &[]), // parent not in the table
        ]);
        assert!(r.effective(&node(Some(a), &[], &[])).is_empty());
        assert!(r.effective(&node(Some(b), &[], &[])).is_empty());
        // An id the resolver has never heard of, and the ungrouped bucket.
        assert!(r
            .effective(&node(Some(Uuid::new_v4()), &[], &[]))
            .is_empty());
        assert!(r.effective(&node(None, &[], &[])).is_empty());
    }

    /// The twin of `blank_pool_values_count_as_unset`, plus the de-duplication `jsonb` used to do
    /// implicitly: one badge, not two, when a node and its folder name the same label.
    #[test]
    fn blank_and_duplicate_labels_are_dropped() {
        let g = Uuid::new_v4();
        let r = TagResolver::build(vec![row(g, None, &["JAPAN", "  ", ""], &[])]);
        assert_eq!(
            r.effective(&node(Some(g), &["JAPAN", " ", "JAPAN"], &[])),
            vec!["JAPAN"]
        );
    }

    #[test]
    fn a_descendant_can_remove_an_inherited_label() {
        let g = Uuid::new_v4();
        let r = TagResolver::build(vec![row(g, None, &["JAPAN", "core"], &[])]);
        assert_eq!(
            r.effective(&node(Some(g), &[], &["JAPAN"])),
            vec!["core"],
            "the node refused one of the two its folder supplies"
        );
        assert_eq!(
            r.effective(&node(Some(g), &[], &[])),
            vec!["JAPAN", "core"],
            "and its sibling, which refused nothing, still carries both"
        );
    }

    /// Excluding at a folder takes it away from everything underneath, because the descendants
    /// inherit the already-reduced set rather than re-deriving it.
    #[test]
    fn a_folder_exclusion_reaches_the_whole_subtree() {
        let (region, site) = (Uuid::new_v4(), Uuid::new_v4());
        let r = TagResolver::build(vec![
            row(region, None, &["JAPAN"], &[]),
            row(site, Some(region), &["matsuyama"], &["JAPAN"]),
        ]);
        assert_eq!(r.effective(&node(Some(site), &[], &[])), vec!["matsuyama"]);
        assert_eq!(r.effective(&node(Some(region), &[], &[])), vec!["JAPAN"]);
    }

    /// Remove-then-add at one level, so a folder that both refuses and sets a label keeps it. The
    /// fold order is what makes this well-defined instead of a contradiction.
    #[test]
    fn a_label_set_and_excluded_at_the_same_level_is_kept() {
        let (region, site) = (Uuid::new_v4(), Uuid::new_v4());
        let r = TagResolver::build(vec![
            row(region, None, &["JAPAN"], &[]),
            row(site, Some(region), &["JAPAN"], &["JAPAN"]),
        ]);
        assert_eq!(r.effective(&node(Some(site), &[], &[])), vec!["JAPAN"]);
    }

    /// The twin of `cyclic_ancestry_resolves_to_default_without_hanging` — and it pins the
    /// **chosen** degradation, not merely that the walk terminates. Asserting only termination
    /// would pass equally on an implementation that threw the labels away, which is the failure
    /// this one is here to refuse.
    #[test]
    fn cyclic_ancestry_keeps_what_was_collected_without_hanging() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let r = TagResolver::build(vec![
            row(a, Some(b), &["JAPAN"], &[]),
            row(b, Some(a), &["core"], &[]),
        ]);
        let from_a = r.effective(&node(Some(a), &[], &[]));
        assert!(
            from_a.contains(&"JAPAN".to_owned()),
            "a cycle must not cost the group its own label: got {from_a:?}"
        );
    }

    #[test]
    fn a_chain_deeper_than_the_bound_stops_at_the_bound() {
        let ids: Vec<Uuid> = (0..crate::groups::MAX_GROUP_DEPTH + 20)
            .map(|_| Uuid::new_v4())
            .collect();
        let rows: Vec<_> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let parent = (i > 0).then(|| ids[i - 1]);
                (*id, parent, vec![format!("g{i}")], Vec::new())
            })
            .collect();
        let r = TagResolver::build(rows);
        let deepest = r.effective(&node(Some(*ids.last().expect("non-empty")), &[], &[]));
        assert!(
            !deepest.is_empty() && deepest.len() <= EFFECTIVE_LABELS_MAX,
            "the walk stopped at the bound and kept what it had: {} labels",
            deepest.len()
        );
    }

    /// 🚨 **The most important one.** The memoized answer is compared against a naive per-group
    /// walk over the same forest. An unwind-order bug — filling the chain deepest-first, or
    /// applying a level's exclusions after its own additions — is invisible to every other test
    /// here, because each of those checks one chain at a time.
    #[test]
    fn memoized_answers_match_a_naive_walk() {
        // A deterministic pseudo-random forest: each group's parent is an earlier one or none,
        // and its labels/exclusions are derived from its index.
        let n = 60usize;
        let ids: Vec<Uuid> = (0..n).map(|_| Uuid::new_v4()).collect();
        let mut rows = Vec::new();
        let mut parents: Vec<Option<usize>> = Vec::new();
        for i in 0..n {
            let parent = if i == 0 || i % 7 == 0 {
                None
            } else {
                Some((i * 13 + 5) % i)
            };
            parents.push(parent);
            let add = if i % 3 == 0 {
                vec![format!("l{}", i % 11)]
            } else {
                Vec::new()
            };
            let remove = if i % 5 == 0 {
                vec![format!("l{}", (i + 4) % 11)]
            } else {
                Vec::new()
            };
            rows.push((ids[i], parent.map(|p| ids[p]), add, remove));
        }
        let resolved = crate::groups::accumulate_ancestor_labels(rows.clone());

        for (i, id) in ids.iter().enumerate() {
            // Naive: collect the chain to the root, then fold it shallowest-first.
            let mut chain = Vec::new();
            let mut cur = Some(i);
            while let Some(c) = cur {
                chain.push(c);
                cur = parents[c];
            }
            let mut want: BTreeSet<String> = BTreeSet::new();
            for c in chain.into_iter().rev() {
                for r in &rows[c].3 {
                    want.remove(r);
                }
                want.extend(rows[c].2.iter().cloned());
            }
            assert_eq!(
                resolved.get(id),
                Some(&want),
                "group {i} disagrees with a naive walk"
            );
        }
    }

    #[test]
    fn the_effective_cap_keeps_the_nodes_own_labels_first() {
        let g = Uuid::new_v4();
        let many: Vec<String> = (0..EFFECTIVE_LABELS_MAX + 30)
            .map(|i| format!("inherited-{i:03}"))
            .collect();
        let r = TagResolver::build(vec![(g, None, many, Vec::new())]);
        let out = r.effective(&node(Some(g), &["mine"], &[]));
        assert_eq!(out.len(), EFFECTIVE_LABELS_MAX);
        assert_eq!(
            out.first().map(String::as_str),
            Some("mine"),
            "the node's own label must survive a cap it did not cause"
        );
    }

    /// `effective_parts` is the notification path's spelling of the same fold. Two spellings of
    /// one rule is exactly the shape that rots, so this pins them to each other.
    #[test]
    fn the_two_spellings_of_the_fold_agree() {
        let (region, site) = (Uuid::new_v4(), Uuid::new_v4());
        let r = TagResolver::build(vec![
            row(region, None, &["JAPAN", "gone"], &[]),
            row(site, Some(region), &["matsuyama"], &[]),
        ]);
        let n = node(Some(site), &["mine"], &["gone"]);
        assert_eq!(
            r.effective(&n),
            r.effective_parts(n.group.map(|g| g.as_uuid()), &n.tags, &n.tags_excluded)
        );
    }
}
