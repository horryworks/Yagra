// SPDX-License-Identifier: AGPL-3.0-only
//! Which subnets a folder's devices carry that its IP ranges do not cover (ADR-170).
//!
//! A folder's ranges (`node_group_prefixes`, normally NetBox's prefixes, ADR-100 decision 10) and
//! the addresses its devices report (`node_l3`, ADR-043/157) are both already stored. This module
//! compares them: every address becomes its subnet, and each subnet no range in the folder's own
//! subtree contains is reported with **why** — so the operator knows what to fix in NetBox.
//!
//! Pure: the stores are read by `api::groups::prefix_gap_report`, which also decides what a
//! scoped caller may be told about the folder that claims a subnet. Everything here is a function
//! of its arguments, so every classification has a unit test.
//!
//! ⚠️ **Containment is computed in Rust here, where folder filing keeps it in SQL**
//! (`groups.rs::match_prefixes`). The reasons that file gives do not apply: the ranges are read
//! whole on the server (no client ever sees a breadcrumb's cleared list), and
//! [`yagra_common::SubnetKey::contains`] is pinned to PostgreSQL's `<<=` by
//! [`tests::the_containment_rule_agrees_with_postgresql`].

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use uuid::Uuid;
use yagra_common::{L3Snapshot, SubnetKey};

/// How many places a subnet was seen are listed with it. The count beside it is never capped —
/// a /24 every access switch carries would otherwise list the whole site.
pub const SEEN_ON_MAX: usize = 5;

/// Where a range's folder sits relative to the folder being checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Relation {
    /// The folder itself or one beneath it. A subnet one of these contains is not a gap.
    Subtree,
    /// A folder above it (a region's aggregate).
    Ancestor,
    /// Any other folder.
    Other,
}

/// One folder range, parsed and placed.
#[derive(Debug, Clone)]
pub struct Range {
    pub group: Uuid,
    pub prefix: SubnetKey,
    pub relation: Relation,
}

/// Why a subnet is reported. Ordered by how directly it names something to fix in NetBox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    /// No range anywhere overlaps it.
    Unregistered,
    /// Ranges exist inside it, but none contains all of it (the device says /23, NetBox has a /24).
    Partial,
    /// The longest range containing it belongs to a folder outside this one's subtree — a prefix
    /// filed under the wrong site, or one private range reused at two sites.
    OtherFolder,
    /// Only a folder above this one has a range containing it: the site's own prefix is missing.
    ParentOnly,
}

impl GapKind {
    /// Every kind, for the token test and the WebUI's mirror.
    #[cfg(test)]
    pub const ALL: [GapKind; 4] = [
        GapKind::Unregistered,
        GapKind::Partial,
        GapKind::OtherFolder,
        GapKind::ParentOnly,
    ];
}

/// One place a subnet was seen: a device, the port it is configured on, and the address itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct SeenOn {
    pub node_id: Uuid,
    pub ifindex: u32,
    /// The port's name when the interface inventory has one. Filled by the caller.
    pub if_name: Option<String>,
    pub ip: String,
}

/// One subnet a folder's devices carry that its own ranges do not cover.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct PrefixGap {
    /// The subnet, as `network/length`.
    pub subnet: String,
    pub kind: GapKind,
    /// The range that contains it (`parent_only`, `other_folder`) or lies inside it (`partial`).
    /// `null` for `unregistered`, **and** when that range belongs to a folder this caller may not
    /// see — a folder's subnet layout is not disclosed past its scope (ADR-014, ADR-100 decision 10).
    pub range: Option<String>,
    /// The folder `range` belongs to, under the same rule.
    pub range_group: Option<Uuid>,
    /// That folder's name, under the same rule.
    pub range_group_name: Option<String>,
    /// How many distinct devices carry an address in this subnet.
    pub node_count: u32,
    /// Up to [`SEEN_ON_MAX`] of the places it was seen, ordered by device then port.
    pub seen_on: Vec<SeenOn>,
}

/// The answer for one folder.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct PrefixGapReport {
    pub group_id: Uuid,
    /// Devices filed in the folder or beneath it.
    pub nodes_total: u32,
    /// Of those, how many have reported their addresses at all. A device with no SNMP, or whose
    /// address walk has never succeeded, contributes nothing — so **no gaps is not the same as
    /// complete** unless this equals `nodes_total`.
    pub nodes_with_addresses: u32,
    /// Of those, how many address lists were cut at the per-device cap.
    pub nodes_truncated: u32,
    /// Distinct subnets compared, covered ones included.
    pub subnets_checked: u32,
    /// Ordered by kind, then subnet.
    pub gaps: Vec<PrefixGap>,
}

/// Compare every subnet the devices carry against the ranges.
///
/// Addresses that cannot form a subnet — host routes, an undecodable length, loopback,
/// link-local, multicast, anycast — are skipped by [`yagra_common::L3Address::subnet`], the rule
/// the network map already uses, so the two never disagree about what a subnet is.
///
/// Returns the gaps and the number of distinct subnets compared.
#[must_use]
pub fn classify(observed: &[(Uuid, &L3Snapshot)], ranges: &[Range]) -> (Vec<PrefixGap>, usize) {
    // subnet → (distinct nodes, every place seen)
    let mut subnets: BTreeMap<SubnetKey, (BTreeSet<Uuid>, Vec<SeenOn>)> = BTreeMap::new();
    for (node, snapshot) in observed {
        for addr in &snapshot.addresses {
            let Some(subnet) = addr.subnet() else {
                continue;
            };
            let entry = subnets.entry(subnet).or_default();
            entry.0.insert(*node);
            entry.1.push(SeenOn {
                node_id: *node,
                ifindex: addr.ifindex,
                if_name: None,
                ip: addr.ip.to_string(),
            });
        }
    }
    let checked = subnets.len();

    let mut gaps: Vec<PrefixGap> = subnets
        .into_iter()
        .filter_map(|(subnet, (nodes, mut seen))| {
            let (kind, range) = judge(&subnet, ranges)?;
            seen.sort_by(|a, b| (a.node_id, a.ifindex, &a.ip).cmp(&(b.node_id, b.ifindex, &b.ip)));
            seen.truncate(SEEN_ON_MAX);
            Some(PrefixGap {
                subnet: subnet.to_string(),
                kind,
                range: range.map(|r| r.prefix.to_string()),
                range_group: range.map(|r| r.group),
                range_group_name: None,
                node_count: u32::try_from(nodes.len()).unwrap_or(u32::MAX),
                seen_on: seen,
            })
        })
        .collect();
    // The map already ordered by subnet, and the sort is stable, so this is kind then subnet.
    gaps.sort_by_key(|g| g.kind);
    (gaps, checked)
}

/// One subnet's verdict: `None` when a range in the subtree covers it.
fn judge<'r>(subnet: &SubnetKey, ranges: &'r [Range]) -> Option<(GapKind, Option<&'r Range>)> {
    if ranges
        .iter()
        .any(|r| r.relation == Relation::Subtree && r.prefix.contains(subnet))
    {
        return None;
    }
    // The longest containing range outside the subtree decides, as the longest match decides
    // folder filing (ADR-124 decision 5). On a tie another folder outranks an ancestor — a sibling
    // site claiming the same length is the more specific thing to fix — and then the lower
    // prefix/group, so the answer does not depend on the order the rows were read in.
    let containing = ranges
        .iter()
        .filter(|r| r.relation != Relation::Subtree && r.prefix.contains(subnet))
        .max_by(|a, b| {
            a.prefix
                .prefix_len
                .cmp(&b.prefix.prefix_len)
                .then_with(|| a.relation.cmp(&b.relation))
                .then_with(|| (b.prefix, b.group).cmp(&(a.prefix, a.group)))
        });
    if let Some(r) = containing {
        let kind = match r.relation {
            Relation::Ancestor => GapKind::ParentOnly,
            Relation::Other => GapKind::OtherFolder,
            // Filtered out above; a subtree range containing the subnet returned `None`.
            Relation::Subtree => return None,
        };
        return Some((kind, Some(r)));
    }
    // Nothing contains it. A range inside it means the device's subnet is wider than what was
    // registered: prefer one of this folder's own, then the lowest, for a stable answer.
    let inside = ranges
        .iter()
        .filter(|r| subnet.contains(&r.prefix))
        .min_by(|a, b| (a.relation, a.prefix, a.group).cmp(&(b.relation, b.prefix, b.group)));
    match inside {
        Some(r) => Some((GapKind::Partial, Some(r))),
        None => Some((GapKind::Unregistered, None)),
    }
}

/// One site's comparison, before names and scope are applied (ADR-170 decision 12).
#[derive(Debug, Clone, Default)]
pub struct SiteComparison {
    pub gaps: Vec<PrefixGap>,
    /// Distinct subnets compared, covered ones included.
    pub subnets_checked: usize,
    /// Ranges filed in the site's folder or beneath it.
    pub prefixes: usize,
}

/// Compare each site's devices against the ranges, the site's folder standing where [`classify`]'s
/// folder stands: ranges in its subtree cover, a folder above is `parent_only`, any other folder is
/// `other_folder`. Every site in `sites` gets an answer, an empty one when none of its devices
/// reported an address.
///
/// `None` is the root site — every root-level node together (ADR-187 decision 1). No folder is
/// above or beneath it, so every range there is another folder's.
///
/// A node `site_of` does not place in one of `sites` is not compared: the caller decides which
/// sites exist, and a node placed nowhere would otherwise be counted against the wrong one.
///
/// `counted` decides which folders' ranges are counted in [`SiteComparison::prefixes`]. Every range
/// still takes part in the comparison — a sibling floor's range covers its site's subnet whoever is
/// asking — but a scoped caller is not told how many ranges a folder it cannot see holds (ADR-014).
#[must_use]
pub fn by_site(
    observed: &[(Uuid, &L3Snapshot)],
    site_of: &HashMap<Uuid, Option<Uuid>>,
    sites: &BTreeSet<Option<Uuid>>,
    edges: &[(Uuid, Option<Uuid>)],
    prefixes: &[(Uuid, SubnetKey)],
    counted: impl Fn(Uuid) -> bool,
) -> BTreeMap<Option<Uuid>, SiteComparison> {
    use crate::groups::{group_ancestors, group_subtree};

    let mut members: BTreeMap<Option<Uuid>, Vec<(Uuid, &L3Snapshot)>> = BTreeMap::new();
    for (node, snapshot) in observed {
        if let Some(site) = site_of.get(node).filter(|s| sites.contains(s)) {
            members.entry(*site).or_default().push((*node, *snapshot));
        }
    }
    sites
        .iter()
        .map(|site| {
            let (subtree, ancestors): (BTreeSet<Uuid>, BTreeSet<Uuid>) = match site {
                Some(id) => (
                    group_subtree(edges, *id).into_iter().collect(),
                    group_ancestors(edges, *id).into_iter().collect(),
                ),
                None => (BTreeSet::new(), BTreeSet::new()),
            };
            let ranges: Vec<Range> = prefixes
                .iter()
                .map(|(group, prefix)| Range {
                    group: *group,
                    prefix: *prefix,
                    relation: if subtree.contains(group) {
                        Relation::Subtree
                    } else if ancestors.contains(group) {
                        Relation::Ancestor
                    } else {
                        Relation::Other
                    },
                })
                .collect();
            let own = ranges
                .iter()
                .filter(|r| r.relation == Relation::Subtree && counted(r.group))
                .count();
            let (gaps, checked) = members
                .get(site)
                .map_or_else(|| (Vec::new(), 0), |m| classify(m, &ranges));
            (
                *site,
                SiteComparison {
                    gaps,
                    subnets_checked: checked,
                    prefixes: own,
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::{L3AddrType, L3Address};

    const SITE: Uuid = Uuid::from_u128(1);
    const SUB: Uuid = Uuid::from_u128(2);
    const REGION: Uuid = Uuid::from_u128(3);
    const ELSEWHERE: Uuid = Uuid::from_u128(4);
    const N1: Uuid = Uuid::from_u128(11);
    const N2: Uuid = Uuid::from_u128(12);

    fn range(group: Uuid, prefix: &str, relation: Relation) -> Range {
        Range {
            group,
            prefix: prefix.parse().unwrap(),
            relation,
        }
    }

    fn snap(addrs: &[(u32, &str, u8)]) -> L3Snapshot {
        L3Snapshot::new(
            addrs
                .iter()
                .map(|(ifx, ip, len)| L3Address::new(*ifx, ip.parse().unwrap(), *len))
                .collect(),
        )
    }

    fn kinds(gaps: &[PrefixGap]) -> Vec<(String, GapKind)> {
        gaps.iter().map(|g| (g.subnet.clone(), g.kind)).collect()
    }

    #[test]
    fn a_subnet_the_subtree_covers_is_not_a_gap() {
        let s = snap(&[(1, "10.1.2.5", 24), (2, "10.1.3.1", 24)]);
        let ranges = [
            range(SITE, "10.1.2.0/24", Relation::Subtree),
            // A child folder's range counts as the site's own.
            range(SUB, "10.1.3.0/24", Relation::Subtree),
        ];
        let (gaps, checked) = classify(&[(N1, &s)], &ranges);
        assert!(gaps.is_empty(), "{:?}", kinds(&gaps));
        assert_eq!(checked, 2);
        // A wider range of the site's own covers a narrower subnet too.
        let (gaps, _) = classify(
            &[(N1, &s)],
            &[range(SITE, "10.1.0.0/16", Relation::Subtree)],
        );
        assert!(gaps.is_empty());
    }

    #[test]
    fn each_of_the_four_kinds_is_reported_with_its_range() {
        let s = snap(&[
            (1, "10.9.9.1", 24),   // nothing anywhere
            (2, "10.7.4.1", 23),   // wider than the site's /24
            (3, "10.2.0.1", 24),   // another site's range
            (4, "10.1.200.1", 24), // only the region's /16
        ]);
        let ranges = [
            range(SITE, "10.7.4.0/24", Relation::Subtree),
            range(REGION, "10.1.0.0/16", Relation::Ancestor),
            range(ELSEWHERE, "10.2.0.0/24", Relation::Other),
        ];
        let (gaps, checked) = classify(&[(N1, &s)], &ranges);
        assert_eq!(checked, 4);
        assert_eq!(
            kinds(&gaps),
            vec![
                ("10.9.9.0/24".to_owned(), GapKind::Unregistered),
                ("10.7.4.0/23".to_owned(), GapKind::Partial),
                ("10.2.0.0/24".to_owned(), GapKind::OtherFolder),
                ("10.1.200.0/24".to_owned(), GapKind::ParentOnly),
            ],
            "ordered by kind"
        );
        assert_eq!(gaps[0].range, None);
        assert_eq!(gaps[0].range_group, None);
        assert_eq!(gaps[1].range.as_deref(), Some("10.7.4.0/24"));
        assert_eq!(gaps[1].range_group, Some(SITE));
        assert_eq!(gaps[2].range_group, Some(ELSEWHERE));
        assert_eq!(gaps[3].range.as_deref(), Some("10.1.0.0/16"));
        assert_eq!(gaps[3].range_group, Some(REGION));
    }

    #[test]
    fn the_longest_containing_range_decides_between_a_parent_and_another_folder() {
        let s = snap(&[(1, "10.1.7.1", 24)]);
        // The region's /16 and a sibling site's /24 both contain it: the sibling is more specific.
        let ranges = [
            range(REGION, "10.1.0.0/16", Relation::Ancestor),
            range(ELSEWHERE, "10.1.7.0/24", Relation::Other),
        ];
        let (gaps, _) = classify(&[(N1, &s)], &ranges);
        assert_eq!(gaps[0].kind, GapKind::OtherFolder);
        assert_eq!(gaps[0].range_group, Some(ELSEWHERE));
        // …and a longer ancestor range beats a shorter other one.
        let ranges = [
            range(REGION, "10.1.7.0/24", Relation::Ancestor),
            range(ELSEWHERE, "10.0.0.0/8", Relation::Other),
        ];
        let (gaps, _) = classify(&[(N1, &s)], &ranges);
        assert_eq!(gaps[0].kind, GapKind::ParentOnly);
        // At the same length another folder outranks the ancestor, whatever the row order.
        for ranges in [
            [
                range(REGION, "10.1.7.0/24", Relation::Ancestor),
                range(ELSEWHERE, "10.1.7.0/24", Relation::Other),
            ],
            [
                range(ELSEWHERE, "10.1.7.0/24", Relation::Other),
                range(REGION, "10.1.7.0/24", Relation::Ancestor),
            ],
        ] {
            let (gaps, _) = classify(&[(N1, &s)], &ranges);
            assert_eq!(gaps[0].kind, GapKind::OtherFolder);
        }
    }

    #[test]
    fn two_other_folders_at_the_same_length_give_the_same_answer_in_either_order() {
        let s = snap(&[(1, "10.5.0.1", 24)]);
        let a = range(Uuid::from_u128(20), "10.5.0.0/24", Relation::Other);
        let b = range(Uuid::from_u128(21), "10.5.0.0/24", Relation::Other);
        let (x, _) = classify(&[(N1, &s)], &[a.clone(), b.clone()]);
        let (y, _) = classify(&[(N1, &s)], &[b, a]);
        assert_eq!(x[0].range_group, y[0].range_group);
    }

    #[test]
    fn addresses_that_are_no_subnet_are_skipped() {
        let mut vip = L3Address::new(9, "10.3.3.1".parse().unwrap(), 24);
        vip.addr_type = L3AddrType::Anycast;
        let mut s = snap(&[
            (1, "10.3.0.1", 32),    // host route
            (2, "10.3.1.1", 0),     // undecodable length
            (3, "127.0.0.1", 8),    // loopback
            (4, "169.254.1.1", 16), // link-local
            (5, "fe80::1", 64),     // v6 link-local
            (6, "2001:db8::1", 128),
        ]);
        s.addresses.push(vip);
        let (gaps, checked) = classify(&[(N1, &s)], &[]);
        assert_eq!(checked, 0, "{:?}", kinds(&gaps));
        assert!(gaps.is_empty());
    }

    #[test]
    fn ipv6_is_classified_like_ipv4_and_never_matches_a_v4_range() {
        let s = snap(&[(1, "2001:db8:1::1", 64), (2, "2001:db8:2::1", 64)]);
        let ranges = [
            range(SITE, "2001:db8:1::/48", Relation::Subtree),
            range(SITE, "10.0.0.0/8", Relation::Subtree),
        ];
        let (gaps, _) = classify(&[(N1, &s)], &ranges);
        assert_eq!(
            kinds(&gaps),
            vec![("2001:db8:2::/64".to_owned(), GapKind::Unregistered)]
        );
    }

    #[test]
    fn a_subnet_counts_every_device_but_lists_only_a_few_places() {
        let a = snap(&[(1, "10.8.0.1", 24), (2, "10.8.0.2", 24)]);
        let b = snap(&[(1, "10.8.0.3", 24)]);
        let many: Vec<L3Snapshot> = (0..8u8)
            .map(|i| snap(&[(1, &format!("10.8.0.{}", 10 + i), 24)]))
            .collect();
        let mut observed: Vec<(Uuid, &L3Snapshot)> = vec![(N2, &b), (N1, &a)];
        for (i, s) in many.iter().enumerate() {
            observed.push((Uuid::from_u128(100 + i as u128), s));
        }
        let (gaps, _) = classify(&observed, &[]);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].node_count, 10, "distinct devices, not addresses");
        assert_eq!(gaps[0].seen_on.len(), SEEN_ON_MAX);
        assert_eq!(
            (gaps[0].seen_on[0].node_id, gaps[0].seen_on[0].ifindex),
            (N1, 1),
            "ordered by device then port"
        );
        assert_eq!(gaps[0].seen_on[1].ip, "10.8.0.2");
    }

    /// Two sites under one region: each is compared against its own subtree, so the same range is
    /// covered at the site that owns it and `other_folder` at the one that only carries it — and a
    /// site whose devices reported nothing still answers, with its own ranges counted.
    #[test]
    fn each_site_is_compared_against_its_own_folder() {
        const SITE_B: Uuid = Uuid::from_u128(5);
        const QUIET: Uuid = Uuid::from_u128(6);
        const N3: Uuid = Uuid::from_u128(13);
        let edges = [
            (REGION, None),
            (SITE, Some(REGION)),
            (SUB, Some(SITE)),
            (SITE_B, Some(REGION)),
            (QUIET, Some(REGION)),
        ];
        let p = |g: Uuid, s: &str| (g, s.parse::<SubnetKey>().unwrap());
        let prefixes = [
            p(REGION, "10.0.0.0/8"),
            p(SUB, "10.1.1.0/24"),
            p(SITE_B, "192.168.10.0/24"),
            p(QUIET, "10.3.0.0/24"),
        ];
        let a = snap(&[
            (1, "10.1.1.1", 24),
            (2, "192.168.10.1", 24),
            (3, "10.1.9.1", 24),
        ]);
        let b = snap(&[(1, "192.168.10.2", 24)]);
        let root = snap(&[(1, "10.3.0.9", 24)]);
        let site_of: HashMap<Uuid, Option<Uuid>> =
            [(N1, Some(SITE)), (N2, Some(SITE_B)), (N3, None)].into();
        let sites: BTreeSet<Option<Uuid>> = [Some(SITE), Some(SITE_B), Some(QUIET), None].into();
        let got = by_site(
            &[(N1, &a), (N2, &b), (N3, &root)],
            &site_of,
            &sites,
            &edges,
            &prefixes,
            |_| true,
        );

        let site = &got[&Some(SITE)];
        assert_eq!(
            kinds(&site.gaps),
            vec![
                ("192.168.10.0/24".to_owned(), GapKind::OtherFolder),
                ("10.1.9.0/24".to_owned(), GapKind::ParentOnly),
            ],
            "a subfolder's range is the site's own"
        );
        assert_eq!(site.gaps[0].range_group, Some(SITE_B));
        assert_eq!((site.subnets_checked, site.prefixes), (3, 1));

        let b = &got[&Some(SITE_B)];
        assert!(b.gaps.is_empty(), "{:?}", kinds(&b.gaps));
        assert_eq!((b.subnets_checked, b.prefixes), (1, 1));

        let quiet = &got[&Some(QUIET)];
        assert!(quiet.gaps.is_empty());
        assert_eq!((quiet.subnets_checked, quiet.prefixes), (0, 1));

        // The root has no folder of its own: QUIET's range is another folder's.
        let root = &got[&None];
        assert_eq!(
            kinds(&root.gaps),
            vec![("10.3.0.0/24".to_owned(), GapKind::OtherFolder)]
        );
        assert_eq!(root.prefixes, 0);

        // A caller who cannot see SUB is not told it holds a range — and SUB's range still covers.
        let narrowed = by_site(
            &[(N1, &a)],
            &site_of,
            &[Some(SITE)].into(),
            &edges,
            &prefixes,
            |g| g != SUB,
        );
        let site = &narrowed[&Some(SITE)];
        assert_eq!(site.prefixes, 0);
        assert_eq!(site.gaps.len(), 2, "{:?}", kinds(&site.gaps));
    }

    /// A node placed in a site the caller did not ask about is compared nowhere — never against a
    /// site it is not in.
    #[test]
    fn a_node_outside_the_asked_sites_is_not_compared() {
        let s = snap(&[(1, "10.9.9.1", 24)]);
        let site_of: HashMap<Uuid, Option<Uuid>> = [(N1, Some(ELSEWHERE))].into();
        let sites: BTreeSet<Option<Uuid>> = [Some(SITE)].into();
        let got = by_site(&[(N1, &s)], &site_of, &sites, &[(SITE, None)], &[], |_| {
            true
        });
        assert_eq!(got.len(), 1);
        assert!(got[&Some(SITE)].gaps.is_empty());
        assert_eq!(got[&Some(SITE)].subnets_checked, 0);
    }

    #[test]
    fn every_kind_serializes_to_its_documented_token() {
        let tokens: Vec<String> = GapKind::ALL
            .iter()
            .map(|k| {
                serde_json::to_value(k)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            tokens,
            ["unregistered", "partial", "other_folder", "parent_only"]
        );
    }

    /// The WebUI iterates its own copy of the kinds to build `t()` keys. Pinned here, in order.
    #[test]
    fn the_webuis_kind_list_is_this_enum_in_order() {
        let ts = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../web/src/components/NodeDetail/prefixGaps.ts"
        ))
        .expect("read prefixGaps.ts");
        let line = ts
            .lines()
            .find(|l| l.starts_with("export const PREFIX_GAP_KINDS"))
            .expect("PREFIX_GAP_KINDS is declared on one line");
        let quoted: Vec<&str> = line.split('\'').skip(1).step_by(2).collect();
        let tokens: Vec<String> = GapKind::ALL
            .iter()
            .map(|k| {
                serde_json::to_value(k)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(quoted, tokens);
    }

    /// 🚨 [`SubnetKey::contains`] is a second implementation of PostgreSQL's `<<=`, which folder
    /// filing uses. Every pair below is asked of both; a disagreement means this report and the
    /// importer would file the same address differently.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_containment_rule_agrees_with_postgresql(pool: sqlx::PgPool) {
        let nets = [
            "10.0.0.0/8",
            "10.1.0.0/16",
            "10.1.2.0/24",
            "10.1.2.128/25",
            "10.1.2.192/26",
            "10.1.3.0/24",
            "192.168.0.0/23",
            "192.168.1.0/24",
            "172.16.0.0/12",
            "2001:db8::/32",
            "2001:db8:1::/48",
            "2001:db8:1::/64",
            "2001:db9::/48",
            "::a00:0/104",
        ];
        let mut compared = 0;
        for a in nets {
            for b in nets {
                let pg: bool = sqlx::query_scalar("SELECT $1::cidr <<= $2::cidr")
                    .bind(b)
                    .bind(a)
                    .fetch_one(&pool)
                    .await
                    .expect("ask postgresql");
                let rust = a
                    .parse::<SubnetKey>()
                    .unwrap()
                    .contains(&b.parse().unwrap());
                assert_eq!(rust, pg, "{b} <<= {a}");
                compared += 1;
            }
        }
        assert_eq!(compared, nets.len() * nets.len());
    }
}
