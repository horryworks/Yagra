// SPDX-License-Identifier: AGPL-3.0-only
//! Which site a node belongs to. Two answers, one walk:
//!
//! - [`nearest_site_by_node`] (ADR-248 decision 2): the nearest folder of type Site above it, or
//!   nothing. What an alert row's Site column shows.
//! - [`sites_by_node`] (ADR-187 decision 1): the same, else its own folder, and every root-level
//!   node together as one site (`None`). What Subnet overlaps (ADR-187) and Missing IP prefixes
//!   (ADR-170 decision 12) compare, so the two cannot disagree about where a device is.
//!
//! The walk is written once, here, so the alert column and the comparisons cannot drift. Pure.

use std::collections::HashMap;

use uuid::Uuid;

/// The site of every node: the nearest folder of type Site above it, else its own folder.
pub(crate) fn sites_by_node(
    groups: &[yagra_base::groups::GroupSummary],
    nodes: &[(Uuid, String, Option<Uuid>)],
) -> HashMap<Uuid, Option<Uuid>> {
    let folders = folders_of(groups);
    nodes
        .iter()
        .map(|(id, _, folder)| (*id, nearest_site(&folders, *folder).or(*folder)))
        .collect()
}

/// The nearest folder of type Site above each node (its own folder counts), or `None` when no
/// folder on the way up is a Site. Keyed by the node id the caller passed with its folder.
pub(crate) fn nearest_site_by_node(
    groups: &[yagra_base::groups::GroupSummary],
    nodes: impl IntoIterator<Item = (Uuid, Option<Uuid>)>,
) -> HashMap<Uuid, Option<Uuid>> {
    let folders = folders_of(groups);
    nodes
        .into_iter()
        .map(|(id, folder)| (id, nearest_site(&folders, folder)))
        .collect()
}

/// Every folder whose nearest Site (itself included) is one of `sites` (ADR-248 Inc.2): the
/// folders a node must be filed in for its Site column to name one of them. A Site folder nested
/// under another belongs to itself, never to the outer one, so the two columns and the filter
/// cannot disagree. Sorted, so the answer is stable.
pub(crate) fn folders_of_sites(
    groups: &[yagra_base::groups::GroupSummary],
    sites: &std::collections::HashSet<Uuid>,
) -> Vec<Uuid> {
    under_sites(&folders_of(groups), sites)
}

fn under_sites(
    folders: &HashMap<Uuid, Folder>,
    sites: &std::collections::HashSet<Uuid>,
) -> Vec<Uuid> {
    let mut out: Vec<Uuid> = folders
        .keys()
        .copied()
        .filter(|g| nearest_site(folders, Some(*g)).is_some_and(|s| sites.contains(&s)))
        .collect();
    out.sort_unstable();
    out
}

/// Whether `id` names a folder of type Site.
pub(crate) fn is_site(groups: &[yagra_base::groups::GroupSummary], id: Uuid) -> bool {
    groups.iter().any(|g| g.id == id && g.group_type == "site")
}

/// What deciding a site needs to know about one folder.
struct Folder {
    is_site: bool,
    parent: Option<Uuid>,
}

fn folders_of(groups: &[yagra_base::groups::GroupSummary]) -> HashMap<Uuid, Folder> {
    groups
        .iter()
        .map(|g| {
            (
                g.id,
                Folder {
                    is_site: g.group_type == "site",
                    parent: g.parent_id,
                },
            )
        })
        .collect()
}

/// The first Site folder met walking up from `folder`, itself included.
fn nearest_site(folders: &HashMap<Uuid, Folder>, folder: Option<Uuid>) -> Option<Uuid> {
    let mut at = folder;
    // Bounded by the number of folders, so a cycle a bad import left cannot spin forever.
    for _ in 0..=folders.len() {
        let id = at?;
        let f = folders.get(&id)?;
        if f.is_site {
            return Some(id);
        }
        at = f.parent;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(is_site: bool, parent: Option<Uuid>) -> Folder {
        Folder { is_site, parent }
    }

    /// The shared tree: a site with a floor under it, a plain folder, and a cycle a bad import left.
    fn tree() -> (HashMap<Uuid, Folder>, [Uuid; 5]) {
        let ids = [1, 2, 3, 4, 5].map(Uuid::from_u128);
        let [site, sub, plain, loop_a, loop_b] = ids;
        let folders: HashMap<Uuid, Folder> = [
            (site, folder(true, None)),
            (sub, folder(false, Some(site))),
            (plain, folder(false, None)),
            (loop_a, folder(false, Some(loop_b))),
            (loop_b, folder(false, Some(loop_a))),
        ]
        .into();
        (folders, ids)
    }

    /// ADR-187's rule: the nearest Site folder above, its own folder when there is none, and
    /// nothing at the root — and a cycle ends rather than spinning.
    #[test]
    fn a_site_is_the_nearest_site_folder_above_else_the_devices_own_folder() {
        let (folders, [site, sub, plain, loop_a, _]) = tree();
        let site_or_own = |f: Option<Uuid>| nearest_site(&folders, f).or(f);
        assert_eq!(site_or_own(Some(sub)), Some(site), "under a site");
        assert_eq!(site_or_own(Some(site)), Some(site), "in the site itself");
        assert_eq!(site_or_own(Some(plain)), Some(plain), "no site above");
        assert_eq!(site_or_own(None), None, "the root is one site");
        assert_eq!(site_or_own(Some(loop_a)), Some(loop_a), "a cycle ends");
    }

    /// ADR-248's rule: the same walk, but no Site folder above means no site at all — never the
    /// device's own folder, which would put a rack's name in a column headed Site.
    #[test]
    fn the_nearest_site_is_none_when_no_site_folder_is_above() {
        let (folders, [site, sub, plain, loop_a, _]) = tree();
        assert_eq!(
            nearest_site(&folders, Some(sub)),
            Some(site),
            "under a site"
        );
        assert_eq!(
            nearest_site(&folders, Some(site)),
            Some(site),
            "in the site itself"
        );
        assert_eq!(nearest_site(&folders, Some(plain)), None, "no site above");
        assert_eq!(nearest_site(&folders, None), None, "at the root");
        assert_eq!(nearest_site(&folders, Some(loop_a)), None, "a cycle ends");
        let gone = Uuid::from_u128(99);
        assert_eq!(
            nearest_site(&folders, Some(gone)),
            None,
            "a folder that is not listed"
        );
    }

    /// ADR-248 Inc.2: a Site's folders are itself and everything under it that no nearer Site
    /// claims; a plain folder, a cycle and an unlisted id belong to none.
    #[test]
    fn a_sites_folders_are_those_whose_nearest_site_it_is() {
        let (mut folders, [site, sub, plain, _, _]) = tree();
        let inner = Uuid::from_u128(6);
        let under_inner = Uuid::from_u128(7);
        folders.insert(inner, folder(true, Some(sub)));
        folders.insert(under_inner, folder(false, Some(inner)));
        let pick = |ids: &[Uuid]| under_sites(&folders, &ids.iter().copied().collect());

        let mut outer = vec![site, sub];
        outer.sort_unstable();
        assert_eq!(
            pick(&[site]),
            outer,
            "a nested site is not the outer site's"
        );
        assert_eq!(pick(&[inner]), vec![inner, under_inner]);
        assert_eq!(pick(&[site, inner]).len(), 4, "several sites at once");
        assert!(pick(&[plain]).is_empty(), "a plain folder is no site");
        assert!(pick(&[Uuid::from_u128(99)]).is_empty(), "an unknown id");
        assert!(pick(&[]).is_empty());
    }
}
