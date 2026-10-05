// SPDX-License-Identifier: AGPL-3.0-only
//! Which site a node belongs to (ADR-187 decision 1): the nearest folder of type Site above it, else
//! its own folder, and every root-level node together as one site (`None`).
//!
//! One rule for every screen that compares sites — Subnet overlaps (ADR-187) and Missing IP prefixes
//! (ADR-170 decision 12) — so the two cannot disagree about where a device is. Pure.

use std::collections::HashMap;

use uuid::Uuid;

/// The site of every node: the nearest folder of type Site above it, else its own folder.
pub(crate) fn sites_by_node(
    groups: &[crate::groups::GroupSummary],
    nodes: &[(Uuid, String, Option<Uuid>)],
) -> HashMap<Uuid, Option<Uuid>> {
    let folders: HashMap<Uuid, Folder> = groups
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
        .collect();
    sites_from(&folders, nodes)
}

/// What deciding a site needs to know about one folder.
struct Folder {
    is_site: bool,
    parent: Option<Uuid>,
}

/// [`sites_by_node`]'s rule, over just the folder tree's shape.
fn sites_from(
    folders: &HashMap<Uuid, Folder>,
    nodes: &[(Uuid, String, Option<Uuid>)],
) -> HashMap<Uuid, Option<Uuid>> {
    let site_of_folder = |folder: Option<Uuid>| -> Option<Uuid> {
        let mut at = folder;
        // Bounded by the number of folders, so a cycle a bad import left cannot spin forever.
        for _ in 0..=folders.len() {
            let Some(id) = at else { break };
            let Some(f) = folders.get(&id) else { break };
            if f.is_site {
                return Some(id);
            }
            at = f.parent;
        }
        folder
    };
    nodes
        .iter()
        .map(|(id, _, folder)| (*id, site_of_folder(*folder)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(is_site: bool, parent: Option<Uuid>) -> Folder {
        Folder { is_site, parent }
    }

    /// A device's site is the nearest Site folder above it, its own folder when there is none, and
    /// nothing at the root — and a cycle a bad import left ends rather than spinning.
    #[test]
    fn a_site_is_the_nearest_site_folder_above_else_the_devices_own_folder() {
        let (site, sub, plain, loop_a, loop_b) = (
            Uuid::from_u128(1),
            Uuid::from_u128(2),
            Uuid::from_u128(3),
            Uuid::from_u128(4),
            Uuid::from_u128(5),
        );
        let folders: HashMap<Uuid, Folder> = [
            (site, folder(true, None)),
            (sub, folder(false, Some(site))),
            (plain, folder(false, None)),
            (loop_a, folder(false, Some(loop_b))),
            (loop_b, folder(false, Some(loop_a))),
        ]
        .into();
        let node = |n: u128, f: Option<Uuid>| (Uuid::from_u128(100 + n), String::new(), f);
        let nodes = vec![
            node(1, Some(sub)),
            node(2, Some(site)),
            node(3, Some(plain)),
            node(4, None),
            node(5, Some(loop_a)),
        ];
        let got = sites_from(&folders, &nodes);
        assert_eq!(got[&Uuid::from_u128(101)], Some(site), "under a site");
        assert_eq!(got[&Uuid::from_u128(102)], Some(site), "in the site itself");
        assert_eq!(got[&Uuid::from_u128(103)], Some(plain), "no site above");
        assert_eq!(got[&Uuid::from_u128(104)], None, "the root is one site");
        assert_eq!(got[&Uuid::from_u128(105)], Some(loop_a), "a cycle ends");
    }
}
