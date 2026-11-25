// SPDX-License-Identifier: AGPL-3.0-only
//! Which nodes a setting applies to (ADR-262 decisions 1 and 10): every node, or the nodes inside
//! some folders and/or carrying some tags. A routing rule and an alert-grouping exception both
//! name their nodes this way, and every place that asks "does it reach this node" asks here, so
//! the notifier, the coverage week, the clock line and the grouping cannot disagree. Whether a
//! node carries the tags is [`carries`] (ADR-267), which the All nodes tag filter asks as well.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How a setting's folders and its tags combine when it names both (ADR-262 decision 10). Inside
/// the folder list one entry is always enough, inside the tag list `tag_match` decides (ADR-267),
/// and this joins the two lists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Join {
    /// Inside one of the folders, or carrying one of the tags.
    #[default]
    Or,
    /// Inside one of the folders, and carrying one of the tags.
    And,
}

impl Join {
    /// The stored flag (`join_and`).
    #[must_use]
    pub const fn is_and(self) -> bool {
        match self {
            Self::Or => false,
            Self::And => true,
        }
    }

    /// Read the stored flag.
    #[must_use]
    pub const fn from_and(and: bool) -> Self {
        if and {
            Self::And
        } else {
            Self::Or
        }
    }
}

/// How a setting's tags combine among themselves (ADR-267 decision 1): a node carrying any one of
/// them, or one carrying every one. Only a list of two or more tags can tell the two apart.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TagMatch {
    /// The node carries at least one of the tags. Every setting saved before ADR-267 reads this.
    #[default]
    Any,
    /// The node carries every one of the tags.
    All,
}

impl TagMatch {
    /// The stored flag (`tags_all`).
    #[must_use]
    pub const fn is_all(self) -> bool {
        match self {
            Self::Any => false,
            Self::All => true,
        }
    }

    /// Read the stored flag.
    #[must_use]
    pub const fn from_all(all: bool) -> Self {
        if all {
            Self::All
        } else {
            Self::Any
        }
    }
}

/// Whether a node whose effective tags are `have` carries `want`, combined by `tag_match`
/// (ADR-267). Tags are compared exactly, case included. An empty `want` names no tag and narrows
/// nothing, so it answers true either way.
#[must_use]
pub fn carries<H: AsRef<str>>(have: &[H], want: &[String], tag_match: TagMatch) -> bool {
    let has = |w: &String| have.iter().any(|h| h.as_ref() == w.as_str());
    match tag_match {
        TagMatch::Any => want.is_empty() || want.iter().any(has),
        TagMatch::All => want.iter().all(has),
    }
}

/// The one judgement (ADR-262 decision 10), from what is already known about a node: whether it
/// is inside one of the setting's folders (`None` when the setting names no folder) and whether it
/// carries one of its tags (`None` when it names no tag). Naming neither reaches every node;
/// naming one list, that list decides and `join` does not matter.
#[must_use]
pub const fn joined(by_folder: Option<bool>, by_tag: Option<bool>, join: Join) -> bool {
    match (by_folder, by_tag) {
        (None, None) => true,
        (Some(f), None) => f,
        (None, Some(t)) => t,
        (Some(f), Some(t)) => match join {
            Join::Or => f || t,
            Join::And => f && t,
        },
    }
}

/// Whether a setting naming `group_ids` and `tags` (combined by `tag_match`), the two lists joined
/// by `join`, reaches a node. `in_folder` answers whether the node is inside one of `group_ids`
/// (at any depth) and is asked only when the setting names a folder; `node_tags` are the node's
/// effective labels. A subject that is not a node passes `in_folder` as false and no tags, so it
/// is reached only by a setting that names neither.
#[must_use]
pub fn reaches(
    group_ids: &[Uuid],
    tags: &[String],
    join: Join,
    tag_match: TagMatch,
    in_folder: impl FnOnce() -> bool,
    node_tags: &[String],
) -> bool {
    joined(
        (!group_ids.is_empty()).then(in_folder),
        (!tags.is_empty()).then(|| carries(node_tags, tags, tag_match)),
        join,
    )
}

/// [`reaches`] for a node whose folder chain (its own folder and every folder above it) is at
/// hand.
#[must_use]
pub fn reaches_chain(
    group_ids: &[Uuid],
    tags: &[String],
    join: Join,
    tag_match: TagMatch,
    folder_chain: &[Uuid],
    node_tags: &[String],
) -> bool {
    reaches(
        group_ids,
        tags,
        join,
        tag_match,
        || group_ids.iter().any(|g| folder_chain.contains(g)),
        node_tags,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn folders_and_tags_join_by_or_or_by_and() {
        let site = Uuid::from_u128(1);
        let other = Uuid::from_u128(2);
        let folders = [site];
        let tags = t(&["core"]);
        // (inside the folder, carries the tag) -> (or, and)
        let cases = [
            (&[site][..], t(&["core"]), true, true),
            (&[site][..], t(&[]), true, false),
            (&[other][..], t(&["core"]), true, false),
            (&[other][..], t(&["edge"]), false, false),
        ];
        for (chain, node_tags, or, and) in cases {
            assert_eq!(
                reaches_chain(&folders, &tags, Join::Or, TagMatch::Any, chain, &node_tags),
                or,
                "or: {chain:?} {node_tags:?}"
            );
            assert_eq!(
                reaches_chain(&folders, &tags, Join::And, TagMatch::Any, chain, &node_tags),
                and,
                "and: {chain:?} {node_tags:?}"
            );
        }
    }

    #[test]
    fn one_list_decides_alone_whatever_the_join() {
        let site = Uuid::from_u128(1);
        for join in [Join::Or, Join::And] {
            assert!(reaches_chain(
                &[site],
                &[],
                join,
                TagMatch::Any,
                &[site],
                &[]
            ));
            assert!(!reaches_chain(
                &[site],
                &[],
                join,
                TagMatch::Any,
                &[],
                &t(&["core"])
            ));
            assert!(reaches_chain(
                &[],
                &t(&["core"]),
                join,
                TagMatch::Any,
                &[],
                &t(&["core"])
            ));
            assert!(!reaches_chain(
                &[],
                &t(&["core"]),
                join,
                TagMatch::Any,
                &[site],
                &[]
            ));
            assert!(
                reaches_chain(&[], &[], join, TagMatch::Any, &[], &[]),
                "naming neither reaches all"
            );
        }
    }

    #[test]
    fn the_folder_question_is_asked_only_when_a_folder_is_named() {
        let mut asked = false;
        let hit = reaches(
            &[],
            &t(&["core"]),
            Join::And,
            TagMatch::Any,
            || {
                asked = true;
                false
            },
            &t(&["core"]),
        );
        assert!(hit);
        assert!(!asked, "no folder named, so nothing to ask");
    }

    #[test]
    fn the_stored_flag_round_trips() {
        for j in [Join::Or, Join::And] {
            assert_eq!(Join::from_and(j.is_and()), j);
        }
        assert_eq!(Join::default(), Join::Or, "every existing setting keeps or");
        for m in [TagMatch::Any, TagMatch::All] {
            assert_eq!(TagMatch::from_all(m.is_all()), m);
        }
        assert_eq!(
            TagMatch::default(),
            TagMatch::Any,
            "every existing setting keeps any (ADR-267 decision 1)"
        );
    }

    #[test]
    fn any_needs_one_of_the_tags_and_all_needs_every_one() {
        let want = t(&["core", "24x7"]);
        // (the node's tags) -> (any, all)
        let cases = [
            (t(&["core", "24x7"]), true, true),
            (t(&["core", "24x7", "tokyo"]), true, true),
            (t(&["core"]), true, false),
            (t(&["24x7"]), true, false),
            (t(&["edge"]), false, false),
            (t(&["Core", "24X7"]), false, false),
            (t(&[]), false, false),
        ];
        for (have, any, all) in cases {
            assert_eq!(carries(&have, &want, TagMatch::Any), any, "any: {have:?}");
            assert_eq!(carries(&have, &want, TagMatch::All), all, "all: {have:?}");
        }
        for m in [TagMatch::Any, TagMatch::All] {
            assert!(
                carries(&t(&["edge"]), &[], m),
                "no tag named narrows nothing"
            );
            assert!(carries(&t(&["core"]), &t(&["core"]), m), "one tag: carried");
            assert!(
                !carries(&t(&["edge"]), &t(&["core"]), m),
                "one tag: not carried"
            );
        }
    }

    #[test]
    fn the_tag_match_works_inside_the_folder_join() {
        let site = Uuid::from_u128(1);
        let other = Uuid::from_u128(2);
        let tags = t(&["core", "24x7"]);
        // (folder chain, node tags, join, tag_match) -> reached
        let cases = [
            (&[other][..], t(&["core"]), Join::Or, TagMatch::Any, true),
            (&[other][..], t(&["core"]), Join::Or, TagMatch::All, false),
            (&[site][..], t(&["core"]), Join::Or, TagMatch::All, true),
            (&[site][..], t(&["core"]), Join::And, TagMatch::All, false),
            (
                &[site][..],
                t(&["core", "24x7"]),
                Join::And,
                TagMatch::All,
                true,
            ),
            (
                &[other][..],
                t(&["core", "24x7"]),
                Join::And,
                TagMatch::All,
                false,
            ),
        ];
        for (chain, node_tags, join, m, want) in cases {
            assert_eq!(
                reaches_chain(&[site], &tags, join, m, chain, &node_tags),
                want,
                "{join:?} {m:?} {chain:?} {node_tags:?}"
            );
        }
    }
}
