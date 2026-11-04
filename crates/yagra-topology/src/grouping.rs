// SPDX-License-Identifier: AGPL-3.0-only
//! Group the alerts of an outage at the moment it happens (ADR-236).
//!
//! Nothing here decides a parent in advance. When nodes go down, the down nodes that are linked to
//! one another form a **region**. A down node with a live neighbour is on the region's **border**:
//! it is where reachability actually stopped, so it is a cause. A down node none of whose
//! neighbours is alive sits **inside** the region: it is unreachable because of the border, and
//! its notification is rolled up into the border's (its `root_cause` names the border node).
//!
//! A live node is the evidence. The poller polled it and it answered, so the poller reaches it,
//! which is why no anchor (where the poller attaches) is needed. A site with two ways out that
//! loses one keeps answering, so nothing groups; a site that loses both goes dark as one region.
//! The same rule covers a single-exit branch, a dual-exit branch and a data centre.
//!
//! **How strongly to group is the operator's choice** ([`Level`]), per node, because it trades
//! fewer notifications against the risk that a missing link makes a real fault look like it sits
//! behind another one. Whatever the level, grouping only ever withholds a *notification*: the
//! alert itself stands, and when the border recovers every node still down is judged again and
//! pages on its own. The worst a wrong judgement can do is delay a page, not lose it.
//!
//! **Pure**: no database, no clock, no bus, like [`crate::derive`].

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use yagra_common::{GroupId, NodeId, ProfileCategory};

/// How strongly a node's alerts are grouped (ADR-236 decision 2). Ordered by strength.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Level {
    /// Never grouped. The default.
    #[default]
    Off,
    /// Only on physical links (CDP/LLDP or a pinned link), and only for a node every one of whose
    /// neighbour rows matched a node — one that may have a link nobody can see is never inside.
    Certain,
    /// Physical links where a node has any, otherwise the logical ones (a shared subnet, a routing
    /// adjacency). Unmatched neighbour rows are ignored.
    Links,
    /// [`Level::Links`], and every node down in the same Site folder joins the region whether or
    /// not a link connects them. This is a guess, and the operator chooses it (ADR-043 decision 6
    /// is overruled only here).
    Site,
}

impl Level {
    /// Every level, weakest first.
    pub const ALL: [Level; 4] = [Level::Off, Level::Certain, Level::Links, Level::Site];

    /// The stored number (`0..=3`).
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        match self {
            Level::Off => 0,
            Level::Certain => 1,
            Level::Links => 2,
            Level::Site => 3,
        }
    }

    /// The level a stored number names, or `None` for anything outside `0..=3`.
    #[must_use]
    pub const fn from_u8(n: u8) -> Option<Self> {
        match n {
            0 => Some(Level::Off),
            1 => Some(Level::Certain),
            2 => Some(Level::Links),
            3 => Some(Level::Site),
            _ => None,
        }
    }
}

/// Which node of a region speaks for it (ADR-236 decision 3): the way out of a site first, then the
/// switches that route, the switches that do not, access points, and everything else. Ties go to
/// the lower id, so the choice is stable across refreshes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Rank {
    /// A router or a firewall.
    Edge,
    /// A switch that may route.
    L3Switch,
    /// A switch that does not route.
    L2Switch,
    /// A Wi-Fi access point.
    AccessPoint,
    /// Anything else.
    #[default]
    Other,
}

impl Rank {
    /// What a device profile's category says about a node. A Meraki device is asked through its
    /// product type's category, so the same table answers for it.
    #[must_use]
    pub const fn for_category(category: ProfileCategory) -> Self {
        use ProfileCategory as C;
        match category {
            C::Router | C::Firewall => Rank::Edge,
            C::L3Switch => Rank::L3Switch,
            C::L2Switch => Rank::L2Switch,
            C::WirelessAp => Rank::AccessPoint,
            C::WirelessController
            | C::LoadBalancer
            | C::Server
            | C::Hypervisor
            | C::Storage
            | C::Power
            | C::Printer
            | C::GenericSnmp
            | C::PingOnly
            | C::UrlCheck
            | C::DnsCheck => Rank::Other,
        }
    }
}

/// The links between nodes, and what the operator chose for each node.
#[derive(Debug, Default, Clone)]
pub struct LinkGraph {
    /// CDP/LLDP neighbours and pinned links, both directions.
    physical: BTreeMap<NodeId, BTreeSet<NodeId>>,
    /// Shared-subnet and routing links, both directions. They jump over the switches between two
    /// nodes, so a node that has a physical link never uses them.
    logical: BTreeMap<NodeId, BTreeSet<NodeId>>,
    /// Nodes that reported a neighbour row nobody could be matched to.
    unsure: BTreeSet<NodeId>,
    /// The Site folder each node is filed under, when it is filed under one.
    site: BTreeMap<NodeId, GroupId>,
    /// The other side of `site`.
    site_members: BTreeMap<GroupId, BTreeSet<NodeId>>,
    rank: BTreeMap<NodeId, Rank>,
    level: BTreeMap<NodeId, Level>,
    default_level: Level,
}

impl LinkGraph {
    /// An empty graph whose nodes all group at `default_level` until told otherwise.
    #[must_use]
    pub fn new(default_level: Level) -> Self {
        Self {
            default_level,
            ..Self::default()
        }
    }

    /// Add an undirected link. `physical` is true for CDP, LLDP and a pinned link.
    pub fn add_link(&mut self, a: NodeId, b: NodeId, physical: bool) {
        if a == b {
            return;
        }
        let side = if physical {
            &mut self.physical
        } else {
            &mut self.logical
        };
        side.entry(a).or_default().insert(b);
        side.entry(b).or_default().insert(a);
    }

    /// Record that `node` reported a neighbour row that matched no node.
    pub fn mark_unsure(&mut self, node: NodeId) {
        self.unsure.insert(node);
    }

    /// File `node` under a Site folder.
    pub fn set_site(&mut self, node: NodeId, site: GroupId) {
        if let Some(old) = self.site.insert(node, site) {
            if let Some(m) = self.site_members.get_mut(&old) {
                m.remove(&node);
            }
        }
        self.site_members.entry(site).or_default().insert(node);
    }

    /// Say what kind of node `node` is, for choosing who speaks for a region.
    pub fn set_rank(&mut self, node: NodeId, rank: Rank) {
        self.rank.insert(node, rank);
    }

    /// Set `node`'s own level, overriding the default.
    pub fn set_level(&mut self, node: NodeId, level: Level) {
        self.level.insert(node, level);
    }

    /// The same graph with a different deployment level; folder levels set on a node stay. For a
    /// preview of what each level would do to the outage open now.
    #[must_use]
    pub fn with_default_level(&self, level: Level) -> Self {
        Self {
            default_level: level,
            ..self.clone()
        }
    }

    /// The level `node` groups at.
    #[must_use]
    pub fn level_of(&self, node: NodeId) -> Level {
        self.level.get(&node).copied().unwrap_or(self.default_level)
    }

    /// Every node one link away, physical or logical. For a diagnostic that wants a neighbourhood
    /// (the Troubleshoot incident correlation), not for judging an outage.
    #[must_use]
    pub fn neighbours(&self, node: NodeId) -> BTreeSet<NodeId> {
        let mut out = BTreeSet::new();
        for side in [&self.physical, &self.logical] {
            if let Some(n) = side.get(&node) {
                out.extend(n.iter().copied());
            }
        }
        out
    }

    /// How many undirected links the graph holds.
    #[must_use]
    pub fn link_count(&self) -> usize {
        (self.physical.values().map(BTreeSet::len).sum::<usize>()
            + self.logical.values().map(BTreeSet::len).sum::<usize>())
            / 2
    }

    /// The nodes `node` is judged by at its own level — the neighbours whose being alive makes it a
    /// border. Empty when its level is [`Level::Off`] or it has no link the level accepts. What the
    /// engine looks at before holding a notification (ADR-236 decision 9).
    #[must_use]
    pub fn judging_neighbours(&self, node: NodeId) -> Vec<NodeId> {
        self.links_at(node, self.level_of(node))
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }

    /// The links `node` is judged by at `level`. `None` when the level says nothing about it.
    fn links_at(&self, node: NodeId, level: Level) -> Option<&BTreeSet<NodeId>> {
        let physical = self.physical.get(&node).filter(|s| !s.is_empty());
        match level {
            Level::Off => None,
            Level::Certain => physical,
            Level::Links | Level::Site => {
                physical.or_else(|| self.logical.get(&node).filter(|s| !s.is_empty()))
            }
        }
    }

    /// Whether `node`, down, may be judged to sit inside a region at all (before looking at who is
    /// alive): it needs evidence about its surroundings that the level accepts.
    fn may_be_inside(&self, node: NodeId, level: Level) -> bool {
        match level {
            Level::Off => false,
            Level::Certain => self.links_at(node, level).is_some() && !self.unsure.contains(&node),
            Level::Links => self.links_at(node, level).is_some(),
            Level::Site => self.links_at(node, level).is_some() || self.site.contains_key(&node),
        }
    }

    /// Whether `node` has a live neighbour among the links `level` judges it by.
    fn touches_a_live_node(&self, node: NodeId, level: Level, down: &BTreeSet<NodeId>) -> bool {
        self.links_at(node, level)
            .is_some_and(|links| links.iter().any(|n| !down.contains(n)))
    }

    /// The down nodes linked to `start` at `level`, `start` included.
    ///
    /// The walk follows every link the level accepts, in both directions, rather than each node's
    /// preferred side ([`Self::links_at`]): that preference is asymmetric (a node with only a subnet
    /// link points at one with physical links, which does not point back), and a region grown from
    /// either end has to be the same region, or two nodes of one outage would name two speakers.
    /// The preference decides only who is a border.
    fn region(&self, start: NodeId, level: Level, down: &BTreeSet<NodeId>) -> BTreeSet<NodeId> {
        let mut seen = BTreeSet::from([start]);
        let mut queue = VecDeque::from([start]);
        let walks_logical = matches!(level, Level::Links | Level::Site);
        // A site's members are walked once, by the first of them taken off the queue: every
        // later member would list the same set again, and a site that goes dark as one region
        // would cost its size squared.
        let mut sites_walked: BTreeSet<GroupId> = BTreeSet::new();
        while let Some(n) = queue.pop_front() {
            let physical = self.physical.get(&n).into_iter().flatten();
            let logical = walks_logical
                .then(|| self.logical.get(&n))
                .flatten()
                .into_iter()
                .flatten();
            let linked = physical.chain(logical);
            let same_site = (level == Level::Site)
                .then(|| self.site.get(&n))
                .flatten()
                .filter(|s| sites_walked.insert(**s))
                .and_then(|s| self.site_members.get(s))
                .into_iter()
                .flatten();
            for &m in linked.chain(same_site) {
                if down.contains(&m) && seen.insert(m) {
                    queue.push_back(m);
                }
            }
        }
        seen
    }

    /// Who speaks for `region` (decision 3): a border node when there is one, otherwise the whole
    /// region competes; lowest rank wins, then lowest id.
    fn speaker(&self, region: &BTreeSet<NodeId>, level: Level, down: &BTreeSet<NodeId>) -> NodeId {
        let borders: Vec<NodeId> = region
            .iter()
            .copied()
            .filter(|n| self.touches_a_live_node(*n, level, down))
            .collect();
        let pool: Vec<NodeId> = if borders.is_empty() {
            region.iter().copied().collect()
        } else {
            borders
        };
        pool.into_iter()
            .min_by_key(|n| (self.rank.get(n).copied().unwrap_or_default(), *n))
            .expect("a region always holds the node it was grown from")
    }

    /// The node `node`'s notification is rolled up into, or `None` when it pages on its own.
    ///
    /// `None` for a node that is not down, for one whose level is [`Level::Off`], for a border node,
    /// and for the node that speaks for a region with no border.
    #[must_use]
    pub fn root_cause(&self, node: NodeId, down: &BTreeSet<NodeId>) -> Option<NodeId> {
        if !down.contains(&node) {
            return None;
        }
        let level = self.level_of(node);
        if !self.may_be_inside(node, level) || self.touches_a_live_node(node, level, down) {
            return None;
        }
        let region = self.region(node, level, down);
        let speaker = self.speaker(&region, level, down);
        (speaker != node).then_some(speaker)
    }

    /// [`Self::root_cause`] for every down node at once, computing each region once per level
    /// rather than once per node. Nodes that page on their own are absent.
    #[must_use]
    pub fn attribute(&self, down: &BTreeSet<NodeId>) -> BTreeMap<NodeId, NodeId> {
        let mut speaker_of: BTreeMap<(Level, NodeId), NodeId> = BTreeMap::new();
        let mut out = BTreeMap::new();
        for &node in down {
            let level = self.level_of(node);
            if !self.may_be_inside(node, level) || self.touches_a_live_node(node, level, down) {
                continue;
            }
            let speaker = match speaker_of.get(&(level, node)) {
                Some(s) => *s,
                None => {
                    let region = self.region(node, level, down);
                    let s = self.speaker(&region, level, down);
                    for m in region {
                        speaker_of.insert((level, m), s);
                    }
                    s
                }
            };
            if speaker != node {
                out.insert(node, speaker);
            }
        }
        out
    }

    /// How many of `down` would page if they went down together now: the ones not rolled up.
    #[must_use]
    pub fn notified_count(&self, down: &BTreeSet<NodeId>) -> usize {
        down.len() - self.attribute(down).len()
    }
}

/// One stretch of time a node was down, as the alert history recorded it: from the fire that
/// committed it to the resolve that closed it, or still open (ADR-236 decision 13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outage {
    pub node: NodeId,
    /// When it committed, unix ms.
    pub from_ms: i64,
    /// When it recovered, unix ms, or `None` while it is still down.
    pub to_ms: Option<i64>,
}

impl Outage {
    fn down_at(&self, t: i64) -> bool {
        self.from_ms <= t && self.to_ms.is_none_or(|to| t < to)
    }
}

/// What one level would have done to the replayed fires (ADR-236 decision 13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayLevel {
    pub level: Level,
    /// Fires that would have paged, sending each one the moment it committed.
    pub notified: usize,
    /// Fires that would have paged waiting for a failing neighbour to commit first.
    pub notified_waiting: usize,
    /// Of the fires that waited and then paged, the median and the longest wait, ms.
    pub delay_median_ms: Option<i64>,
    pub delay_max_ms: Option<i64>,
}

/// [`LinkGraph::replay`]'s answer: how many fires there were, and what each level made of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaySummary {
    pub fired: usize,
    /// One per [`Level::ALL`], weakest first.
    pub levels: Vec<ReplayLevel>,
}

/// The nodes down at each of `times`, in the order given: one pass over the outages' starts and
/// ends, so a week of history costs a sort rather than a scan per fire. `times` must be sorted.
fn sweep(outages: &[Outage], times: &[i64], mut at: impl FnMut(usize, &BTreeSet<NodeId>)) {
    let mut edges: Vec<(i64, NodeId, bool)> = Vec::with_capacity(outages.len() * 2);
    for o in outages {
        edges.push((o.from_ms, o.node, true));
        if let Some(to) = o.to_ms {
            edges.push((to, o.node, false));
        }
    }
    edges.sort_by_key(|e| e.0);
    // A count per node rather than a set: two recorded outages of one node can touch.
    let mut count: BTreeMap<NodeId, u32> = BTreeMap::new();
    let mut down: BTreeSet<NodeId> = BTreeSet::new();
    let mut next = 0;
    for (i, &t) in times.iter().enumerate() {
        while next < edges.len() && edges[next].0 <= t {
            let (_, node, starts) = edges[next];
            let c = count.entry(node).or_default();
            if starts {
                *c += 1;
                down.insert(node);
            } else {
                *c = c.saturating_sub(1);
                if *c == 0 {
                    down.remove(&node);
                }
            }
            next += 1;
        }
        at(i, &down);
    }
}

impl LinkGraph {
    /// Replay recorded outages against this graph at every level (ADR-236 decision 13): for each
    /// fire at or after `since_ms`, whether it would have paged, judged with every node down at
    /// that moment — and, waiting, judged again when the last neighbour it is judged by that was
    /// failing commits. A neighbour counts as failing at `t` when it commits within
    /// `lookahead_ms(neighbour)` after `t`, which is how long its run had left at most.
    ///
    /// ⚠️ An approximation, and the screen must not call it more: the links are today's, the
    /// history keeps commit times only, and a page re-sent when a border recovers is not counted.
    #[must_use]
    pub fn replay(
        &self,
        outages: &[Outage],
        since_ms: i64,
        lookahead_ms: impl Fn(NodeId) -> i64,
    ) -> ReplaySummary {
        let mut fires: Vec<usize> = (0..outages.len())
            .filter(|&i| outages[i].from_ms >= since_ms)
            .collect();
        fires.sort_by_key(|&i| outages[i].from_ms);
        let times: Vec<i64> = fires.iter().map(|&i| outages[i].from_ms).collect();
        let mut starts: BTreeMap<NodeId, Vec<i64>> = BTreeMap::new();
        for o in outages {
            starts.entry(o.node).or_default().push(o.from_ms);
        }
        for s in starts.values_mut() {
            s.sort_unstable();
        }
        // The first commit of `n` in (t, t + lookahead].
        let commits_soon = |n: NodeId, t: i64| -> Option<i64> {
            let s = starts.get(&n)?;
            let first = s.get(s.partition_point(|&x| x <= t)).copied()?;
            (first <= t.saturating_add(lookahead_ms(n))).then_some(first)
        };

        let levels = Level::ALL
            .iter()
            .map(|&level| {
                let g = self.with_default_level(level);
                let mut notified = 0;
                // (fire, when to judge it again) for each one that would have waited.
                let mut waits: Vec<(usize, i64)> = Vec::new();
                sweep(outages, &times, |k, down| {
                    let o = &outages[fires[k]];
                    if g.root_cause(o.node, down).is_some() {
                        return;
                    }
                    notified += 1;
                    let until = g
                        .judging_neighbours(o.node)
                        .into_iter()
                        .filter(|n| !down.contains(n))
                        .filter_map(|n| commits_soon(n, o.from_ms))
                        .max();
                    if let Some(t2) = until {
                        waits.push((fires[k], t2));
                    }
                });
                waits.sort_by_key(|w| w.1);
                let again: Vec<i64> = waits.iter().map(|w| w.1).collect();
                let mut silenced = 0;
                let mut delays: Vec<i64> = Vec::new();
                sweep(outages, &again, |k, down| {
                    let (i, t2) = waits[k];
                    let o = &outages[i];
                    if !o.down_at(t2) || g.root_cause(o.node, down).is_some() {
                        silenced += 1;
                    } else {
                        delays.push(t2 - o.from_ms);
                    }
                });
                delays.sort_unstable();
                ReplayLevel {
                    level,
                    notified,
                    notified_waiting: notified - silenced,
                    delay_median_ms: delays.get(delays.len() / 2).copied(),
                    delay_max_ms: delays.last().copied(),
                }
            })
            .collect();
        ReplaySummary {
            fired: fires.len(),
            levels,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn id(n: u128) -> NodeId {
        NodeId(Uuid::from_u128(n))
    }

    fn set(ids: &[NodeId]) -> BTreeSet<NodeId> {
        ids.iter().copied().collect()
    }

    /// One branch office, shaped like a real one: an access switch with three access points and a
    /// second switch on it, and two gateways (an HA pair) that no link connects to the switches —
    /// the WAN side of a site is invisible to CDP/LLDP. A core switch outside the site is alive.
    struct Branch {
        g: LinkGraph,
        core: NodeId,
        sw1: NodeId,
        sw2: NodeId,
        aps: [NodeId; 3],
        gws: [NodeId; 2],
    }

    fn branch(level: Level) -> Branch {
        let (core, sw1, sw2) = (id(1), id(10), id(11));
        let aps = [id(20), id(21), id(22)];
        let gws = [id(30), id(31)];
        let mut g = LinkGraph::new(level);
        g.add_link(sw1, sw2, true);
        for ap in aps {
            g.add_link(ap, sw1, true);
            g.set_rank(ap, Rank::AccessPoint);
        }
        g.set_rank(sw1, Rank::L2Switch);
        g.set_rank(sw2, Rank::L2Switch);
        let site = GroupId(Uuid::from_u128(900));
        for n in [sw1, sw2, aps[0], aps[1], aps[2], gws[0], gws[1]] {
            g.set_site(n, site);
        }
        for gw in gws {
            g.set_rank(gw, Rank::Edge);
        }
        Branch {
            g,
            core,
            sw1,
            sw2,
            aps,
            gws,
        }
    }

    fn whole_branch_down(b: &Branch) -> BTreeSet<NodeId> {
        let mut d = set(&[b.sw1, b.sw2]);
        d.extend(b.aps);
        d.extend(b.gws);
        d
    }

    #[test]
    fn off_groups_nothing() {
        let b = branch(Level::Off);
        let down = whole_branch_down(&b);
        assert!(b.g.attribute(&down).is_empty());
        assert_eq!(b.g.notified_count(&down), 7);
    }

    #[test]
    fn a_branch_that_goes_dark_pages_once_per_linked_region_and_once_per_lone_gateway() {
        // Certain and Links agree here: every linked node has a physical link and nobody reported a
        // row they could not match. The switches and access points are one region with no border,
        // so the lower-ranked switch speaks for it; the gateways have no link, so they page alone.
        for level in [Level::Certain, Level::Links] {
            let b = branch(level);
            let down = whole_branch_down(&b);
            let rc = b.g.attribute(&down);
            for n in [b.sw2, b.aps[0], b.aps[1], b.aps[2]] {
                assert_eq!(rc.get(&n), Some(&b.sw1), "{level:?}");
            }
            assert!(!rc.contains_key(&b.sw1));
            assert!(!rc.contains_key(&b.gws[0]) && !rc.contains_key(&b.gws[1]));
            assert_eq!(b.g.notified_count(&down), 3, "{level:?}");
        }
    }

    #[test]
    fn site_level_folds_the_unlinked_gateways_into_the_same_page() {
        let b = branch(Level::Site);
        let down = whole_branch_down(&b);
        let rc = b.g.attribute(&down);
        // The gateways rank first, so the lower-id one speaks for the whole site.
        for n in [b.sw1, b.sw2, b.aps[0], b.aps[1], b.aps[2], b.gws[1]] {
            assert_eq!(rc.get(&n), Some(&b.gws[0]));
        }
        assert_eq!(b.g.notified_count(&down), 1);
    }

    /// A site walked once still reaches every member: a large site that goes dark with no link
    /// between its nodes is one region with one speaker, and a member of another site is not in it.
    #[test]
    fn a_large_dark_site_is_one_region_however_its_members_are_reached() {
        let mut g = LinkGraph::new(Level::Site);
        let site = GroupId(Uuid::from_u128(0x5_17e));
        let other = GroupId(Uuid::from_u128(0x07_4e5));
        let members: Vec<NodeId> = (1..=400).map(id).collect();
        for &n in &members {
            g.set_site(n, site);
        }
        // Two members linked to each other, so the walk enters the site from more than one node.
        g.add_link(members[10], members[20], false);
        let outsider = id(1_000);
        g.set_site(outsider, other);
        let mut down = set(&members);
        down.insert(outsider);
        let rc = g.attribute(&down);
        assert_eq!(
            rc.len(),
            members.len() - 1,
            "every member but the speaker is rolled up"
        );
        assert!(
            rc.values().all(|s| *s == members[0]),
            "the lowest id speaks"
        );
        assert!(
            !rc.contains_key(&outsider),
            "another site is another region"
        );
    }

    #[test]
    fn a_switch_whose_uplink_is_alive_is_a_border_and_speaks_for_what_hangs_off_it() {
        let mut b = branch(Level::Certain);
        b.g.add_link(b.sw1, b.core, true);
        let mut down = set(&[b.sw1]);
        down.extend(b.aps);
        let rc = b.g.attribute(&down);
        assert_eq!(rc.get(&b.aps[0]), Some(&b.sw1));
        assert!(!rc.contains_key(&b.sw1));
        assert_eq!(b.g.root_cause(b.sw1, &down), None);
        assert_eq!(b.g.root_cause(b.aps[2], &down), Some(b.sw1));
    }

    #[test]
    fn a_node_with_a_second_live_path_is_never_rolled_up() {
        let mut b = branch(Level::Certain);
        // aps[0] is dual-homed: sw1 died, sw2 is alive.
        b.g.add_link(b.aps[0], b.sw2, true);
        let down = set(&[b.sw1, b.aps[0]]);
        assert_eq!(b.g.root_cause(b.aps[0], &down), None);
    }

    #[test]
    fn certain_never_puts_inside_a_node_that_reported_a_row_it_could_not_match() {
        let mut b = branch(Level::Certain);
        b.g.mark_unsure(b.sw2);
        let down = whole_branch_down(&b);
        assert_eq!(b.g.root_cause(b.sw2, &down), None);
        // At Links the same node is judged by the links it has.
        b.g.set_level(b.sw2, Level::Links);
        assert_eq!(b.g.root_cause(b.sw2, &down), Some(b.sw1));
    }

    #[test]
    fn subnet_links_count_only_for_a_node_with_no_physical_one() {
        let mut g = LinkGraph::new(Level::Links);
        let (router, sw, server, phone) = (id(1), id(2), id(3), id(4));
        // The server hangs off the switch physically and shares a subnet with the router.
        g.add_link(server, sw, true);
        g.add_link(server, router, false);
        // The phone has no physical link anyone saw, only the subnet.
        g.add_link(phone, router, false);
        let down = set(&[sw, server, phone]);
        // Physical first: the live router does not make the server a border.
        assert_eq!(g.root_cause(server, &down), Some(sw));
        // No physical link: the subnet speaks, and the router is alive.
        assert_eq!(g.root_cause(phone, &down), None);
        // Certain does not use the subnet at all, so the phone is not judged.
        g.set_level(phone, Level::Certain);
        g.set_level(server, Level::Certain);
        assert_eq!(g.root_cause(phone, &down), None);
        assert_eq!(g.root_cause(server, &down), Some(sw));
    }

    #[test]
    fn a_node_with_no_links_and_no_site_is_never_inside() {
        let g = LinkGraph::new(Level::Site);
        let lone = id(5);
        assert_eq!(g.root_cause(lone, &set(&[lone])), None);
    }

    #[test]
    fn a_node_that_is_up_has_no_root_cause() {
        let b = branch(Level::Links);
        assert_eq!(b.g.root_cause(b.aps[0], &set(&[b.sw1])), None);
    }

    #[test]
    fn each_node_is_judged_at_its_own_level() {
        let mut b = branch(Level::Off);
        b.g.set_level(b.aps[0], Level::Certain);
        let down = whole_branch_down(&b);
        let rc = b.g.attribute(&down);
        assert_eq!(rc.len(), 1);
        assert_eq!(rc.get(&b.aps[0]), Some(&b.sw1));
    }

    #[test]
    fn the_one_by_one_answer_and_the_whole_answer_agree() {
        for level in Level::ALL {
            let mut b = branch(level);
            b.g.add_link(b.sw2, b.core, false);
            b.g.mark_unsure(b.aps[1]);
            let down = whole_branch_down(&b);
            let all = b.g.attribute(&down);
            for &n in &down {
                assert_eq!(
                    b.g.root_cause(n, &down),
                    all.get(&n).copied(),
                    "{level:?} {n:?}"
                );
            }
        }
    }

    #[test]
    fn levels_round_trip_through_their_number() {
        for level in Level::ALL {
            assert_eq!(Level::from_u8(level.as_u8()), Some(level));
        }
        assert_eq!(Level::from_u8(4), None);
    }
    // -- replay (ADR-236 decision 13) --------------------------------------------------------------

    const LOOK: i64 = 600_000;

    /// core (never down) -- sw -- ap1, ap2. The access points commit first, the switch 200 s later.
    fn cascade() -> (LinkGraph, NodeId, NodeId, NodeId) {
        let (core, sw, ap1, ap2) = (id(1), id(10), id(20), id(21));
        let mut g = LinkGraph::new(Level::Off);
        g.add_link(sw, core, true);
        g.add_link(ap1, sw, true);
        g.add_link(ap2, sw, true);
        (g, sw, ap1, ap2)
    }

    fn down_from(node: NodeId, from_ms: i64) -> Outage {
        Outage {
            node,
            from_ms,
            to_ms: None,
        }
    }

    fn at(s: &ReplaySummary, level: Level) -> ReplayLevel {
        *s.levels.iter().find(|l| l.level == level).unwrap()
    }

    #[test]
    fn replaying_a_cascade_counts_the_early_pages_and_waiting_removes_them() {
        let (g, sw, ap1, ap2) = cascade();
        let outages = [
            down_from(ap1, 1_000),
            down_from(ap2, 1_000),
            down_from(sw, 201_000),
        ];
        let s = g.replay(&outages, 0, |_| LOOK);
        assert_eq!(s.fired, 3);
        assert_eq!(s.levels.len(), 4);
        let off = at(&s, Level::Off);
        assert_eq!((off.notified, off.notified_waiting), (3, 3));
        // The access points were borders when they committed, so they paged; waiting for the
        // switch rolls them up and only the switch pages, at once.
        let certain = at(&s, Level::Certain);
        assert_eq!((certain.notified, certain.notified_waiting), (3, 1));
        assert_eq!(certain.delay_max_ms, None, "nothing waited and then paged");
    }

    #[test]
    fn a_node_back_before_its_neighbour_commits_is_not_counted_waiting() {
        let (g, sw, ap1, _) = cascade();
        let outages = [
            Outage {
                node: ap1,
                from_ms: 1_000,
                to_ms: Some(150_000),
            },
            down_from(sw, 201_000),
        ];
        let c = at(&g.replay(&outages, 0, |_| LOOK), Level::Certain);
        assert_eq!((c.notified, c.notified_waiting), (2, 1));
    }

    #[test]
    fn a_wait_that_ends_as_a_border_pages_late_and_the_delay_is_reported() {
        let (mut g, sw, ap1, ap2) = cascade();
        // ap1 has a second way out that stays alive, so it is still a border when sw commits.
        g.add_link(ap1, id(2), true);
        let outages = [
            down_from(ap1, 1_000),
            down_from(ap2, 51_000),
            down_from(sw, 201_000),
        ];
        let c = at(&g.replay(&outages, 0, |_| LOOK), Level::Certain);
        assert_eq!(
            (c.notified, c.notified_waiting),
            (3, 2),
            "ap1 late, sw at once; ap2 rolled up"
        );
        assert_eq!(c.delay_max_ms, Some(200_000));
        assert_eq!(c.delay_median_ms, Some(200_000));
    }

    #[test]
    fn a_neighbour_committing_past_its_lookahead_is_not_waited_for() {
        let (g, sw, ap1, _) = cascade();
        let outages = [down_from(ap1, 1_000), down_from(sw, 1_000 + LOOK + 1)];
        let c = at(&g.replay(&outages, 0, |_| LOOK), Level::Certain);
        assert_eq!((c.notified, c.notified_waiting), (2, 2));
    }

    #[test]
    fn a_fire_before_since_is_not_counted_but_its_outage_still_counts_as_down() {
        let (g, sw, ap1, _) = cascade();
        let outages = [down_from(sw, 0), down_from(ap1, 10_000)];
        let s = g.replay(&outages, 5_000, |_| LOOK);
        assert_eq!(s.fired, 1);
        let c = at(&s, Level::Certain);
        assert_eq!(c.notified, 0, "ap1 sits behind the switch already down");
    }
}
