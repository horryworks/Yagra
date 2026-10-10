// SPDX-License-Identifier: AGPL-3.0-only
//! One folder level of the network map (ADR-191).
//!
//! The map used to draw the whole fleet on one canvas and refused above 2,000 nodes. It now draws
//! one folder at a time: the folder's own nodes one by one, each subfolder as a single box carrying
//! its subtree's counts, every set of links between the same two things bundled into one edge, and
//! every link that leaves the level as a dashed stub naming where it goes.
//!
//! Pure — no I/O, no clock — so every rule below is a unit test away. The handler in
//! `api/topology.rs` reads the inputs, calls [`compute`], resolves the names [`names_needed`]
//! asks for, and hands them to [`apply_names`].
//!
//! **Where a stub sits.** A link from this level to a node elsewhere is drawn against the nearest
//! folder the two have in common (their lowest common ancestor, "L"): the stub is the child of L
//! that contains the far end, so opening the stub lands on L with that child selected — the first
//! level where both ends are visible at once. When the far end sits directly in L, the stub is the
//! far node itself — and so is it when L is drawn flat (below), since L then draws every node of
//! its subtree and has no box for the far end's folder.
//!
//! **Inside a site the level is flat.** A Site folder, and any folder beneath one, does not draw its
//! subfolders as boxes: one site split into floors or buildings is still one network, so every
//! node in the level's subtree is drawn and tagged with the folders down to the one it is filed in
//! (`folder_path`). Whether a
//! level is inside a site is read from the unscoped parent chain, so a caller scoped to a floor
//! still gets the site's drawing; only the yes/no answer reaches the response. A flat drawing over
//! the bounds falls back to boxes, which can still be opened one at a time.
//!
//! **Scope.** A scoped caller's visible folders form subtrees whose roots have an invisible parent.
//! Every parent step goes through [`Tree::pvis`], which answers "no parent" above a scope root, so a
//! scoped caller's "whole network" level is a virtual root holding their scope roots — and the same
//! rules serve both callers. A link is kept only when both of its ends are visible nodes, exactly as
//! `/topology/links` filters.

use std::collections::{BTreeMap, HashMap, HashSet};

use uuid::Uuid;
use yagra_common::{LinkSource, NodeKind, NodeState, ProfileCategory};

use super::fleet::GroupStateCounts;
use super::topology::{
    MapBreadcrumb, MapEdge, MapEdgeMember, MapEndpoint, MapEndpointKind, MapFolder, MapLevel,
    MapNode, MapRole, MapRoleReason, MapStub, MapStubKind,
};
use crate::groups::{GroupType, MAX_GROUP_DEPTH};
use crate::topology_links::StoredLink;

/// Above this many linked nodes on one level the level is not drawn (`overflow`).
pub(crate) const MAP_MAX_NODES: usize = 2000;
/// Above this many bundled edges on one level the level is not drawn (`overflow`).
pub(crate) const MAP_MAX_EDGES: usize = 4000;
/// How many member links one bundled edge lists; `count` still says how many there are.
pub(crate) const MAP_EDGE_MEMBERS_MAX: usize = 50;

/// The part of a folder row a level needs.
#[derive(Debug, Clone)]
pub(crate) struct FolderRow {
    pub id: Uuid,
    pub name: String,
    pub group_type: String,
    pub sort_order: f64,
}

impl From<&crate::groups::GroupSummary> for FolderRow {
    fn from(g: &crate::groups::GroupSummary) -> Self {
        FolderRow {
            id: g.id,
            name: g.name.clone(),
            group_type: g.group_type.clone(),
            sort_order: g.sort_order,
        }
    }
}

/// Everything one level is computed from.
pub(crate) struct LevelInput<'a> {
    /// The folder being drawn; `None` is the whole network.
    pub level: Option<Uuid>,
    /// Every folder's `(id, parent_id)`, unscoped (the tree's shape).
    pub edges: &'a [(Uuid, Option<Uuid>)],
    /// The folder rows the caller may name (visible folders and their breadcrumb ancestors).
    pub groups: &'a [FolderRow],
    /// The folders whose members the caller may see; `None` means every folder.
    pub visible: Option<&'a HashSet<Uuid>>,
    /// Every node the caller may see, with its folder.
    pub node_groups: &'a [(Uuid, Option<Uuid>)],
    /// Each visible node's current state.
    pub states: &'a HashMap<Uuid, NodeState>,
    /// Each node's upstream root cause, when its alert is suppressed under one.
    pub root_causes: &'a HashMap<Uuid, Uuid>,
    /// Every stored link with both ends set, unscoped — filtered here.
    pub links: &'a [StoredLink],
}

/// The folder tree as seen by one caller.
struct Tree<'a> {
    parent: HashMap<Uuid, Option<Uuid>>,
    children: HashMap<Option<Uuid>, Vec<Uuid>>,
    visible: Option<&'a HashSet<Uuid>>,
}

impl<'a> Tree<'a> {
    fn new(edges: &[(Uuid, Option<Uuid>)], visible: Option<&'a HashSet<Uuid>>) -> Self {
        let parent: HashMap<Uuid, Option<Uuid>> = edges.iter().copied().collect();
        let mut tree = Tree {
            parent,
            children: HashMap::new(),
            visible,
        };
        let mut children: HashMap<Option<Uuid>, Vec<Uuid>> = HashMap::new();
        for &(id, _) in edges {
            if tree.is_visible(id) {
                children.entry(tree.pvis(id)).or_default().push(id);
            }
        }
        tree.children = children;
        tree
    }

    fn is_visible(&self, id: Uuid) -> bool {
        self.visible.is_none_or(|v| v.contains(&id))
    }

    /// The parent as this caller sees it: `None` above a scope root, even when the folder has one.
    fn pvis(&self, id: Uuid) -> Option<Uuid> {
        match self.parent.get(&id).copied().flatten() {
            Some(p) if self.is_visible(p) => Some(p),
            _ => None,
        }
    }

    /// Whether `level` is a Site folder or lies beneath one. Walks the unscoped parents, so an
    /// ancestor the caller cannot see still counts.
    fn in_site(&self, level: Option<Uuid>, rows: &HashMap<Uuid, &FolderRow>) -> bool {
        let is_site = |id: Uuid| {
            rows.get(&id)
                .is_some_and(|g| GroupType::from_key(&g.group_type) == Some(GroupType::Site))
        };
        site_of_group(level, &self.parent, &is_site).is_some()
    }

    /// `level` and every visible folder beneath it.
    fn subtree(&self, level: Uuid) -> HashSet<Uuid> {
        let mut out = HashSet::new();
        let mut stack = vec![level];
        while let Some(g) = stack.pop() {
            if !out.insert(g) {
                continue;
            }
            if let Some(kids) = self.children.get(&Some(g)) {
                stack.extend(kids.iter().copied());
            }
        }
        out
    }

    /// `level`, its visible ancestors, and finally `None` — the levels a stub can open onto.
    fn chain(&self, level: Option<Uuid>) -> Vec<Option<Uuid>> {
        let mut out = vec![level];
        let mut cur = level;
        let mut seen = HashSet::new();
        while let Some(id) = cur {
            if !seen.insert(id) || out.len() > MAX_GROUP_DEPTH + 1 {
                break;
            }
            cur = self.pvis(id);
            out.push(cur);
        }
        if out.last() != Some(&None) {
            out.push(None);
        }
        out
    }
}

/// Where one folder's nodes land on the level being drawn.
#[derive(Clone, Copy)]
enum Place {
    /// On the level itself: each node is drawn as a node.
    Direct,
    /// Inside one of the level's subfolders: drawn as that folder's box.
    Folder(Uuid),
    /// Outside the level, directly in the common ancestor `at`: each node is its own stub.
    NodeStub { at: Option<Uuid> },
    /// Outside the level, inside `folder`, a child of the common ancestor `at`.
    FolderStub { folder: Uuid, at: Option<Uuid> },
}

fn kind_rank(k: MapEndpointKind) -> u8 {
    match k {
        MapEndpointKind::Node => 0,
        MapEndpointKind::Folder => 1,
        MapEndpointKind::External => 2,
    }
}

fn kind_token(k: MapEndpointKind) -> &'static str {
    match k {
        MapEndpointKind::Node => "node",
        MapEndpointKind::Folder => "folder",
        MapEndpointKind::External => "external",
    }
}

type EndKey = (u8, Uuid);

struct Bundle {
    a: MapEndpoint,
    b: MapEndpoint,
    members: Vec<MapEdgeMember>,
    sources: Vec<LinkSource>,
}

fn endpoint_key(e: &MapEndpoint) -> EndKey {
    (kind_rank(e.kind), e.id)
}

/// Build one level. Node and node-stub names are left empty; see [`apply_names`].
///
/// A level inside a site is drawn flat; if that drawing is over the bounds, the level is drawn with
/// boxes instead.
pub(crate) fn compute(input: &LevelInput<'_>) -> MapLevel {
    let tree = Tree::new(input.edges, input.visible);
    let rows: HashMap<Uuid, &FolderRow> = input.groups.iter().map(|g| (g.id, g)).collect();
    if tree.in_site(input.level, &rows) {
        if let Some(flat) = compute_with(input, &tree, &rows, true) {
            return flat;
        }
    }
    compute_with(input, &tree, &rows, false)
        .unwrap_or_else(|| unreachable!("only a flat drawing gives up"))
}

/// One level, flat or with boxes. A flat drawing gives up (`None`) the moment it crosses a bound,
/// before building anything it would throw away; a boxed one always answers, with `overflow`.
fn compute_with(
    input: &LevelInput<'_>,
    tree: &Tree<'_>,
    rows: &HashMap<Uuid, &FolderRow>,
    flatten: bool,
) -> Option<MapLevel> {
    // Flat: the whole subtree is the level's own, and there are no boxes.
    let flat: HashSet<Uuid> = match (flatten, input.level) {
        (true, Some(level)) => tree.subtree(level),
        _ => HashSet::new(),
    };
    let own = |g: Option<Uuid>| g == input.level || g.is_some_and(|g| flat.contains(&g));

    // 1. The level's subfolders, in the tree's own order. A flat level counts them but boxes none.
    let children: Vec<&FolderRow> = tree
        .children
        .get(&input.level)
        .into_iter()
        .flatten()
        .filter_map(|id| rows.get(id).copied())
        .collect();
    let subfolder_count = children.len() as i64;
    let mut subfolders = if flatten { Vec::new() } else { children };
    subfolders.sort_by(|x, y| {
        x.sort_order
            .total_cmp(&y.sort_order)
            .then_with(|| x.name.cmp(&y.name))
            .then_with(|| x.id.cmp(&y.id))
    });

    // 2. Every folder in each subfolder's subtree, labelled with the subfolder it rolls up into.
    let mut top_child: HashMap<Uuid, Uuid> = HashMap::new();
    for sub in &subfolders {
        let mut stack = vec![sub.id];
        while let Some(g) = stack.pop() {
            if top_child.insert(g, sub.id).is_some() {
                continue;
            }
            if let Some(kids) = tree.children.get(&Some(g)) {
                stack.extend(kids.iter().copied());
            }
        }
    }

    // 3. One placement per folder, memoized: a fleet has far fewer folders than nodes.
    let chain = tree.chain(input.level);
    let in_chain: HashSet<Option<Uuid>> = chain.iter().copied().collect();
    let mut places: HashMap<Option<Uuid>, Place> = HashMap::new();
    let mut place_of = |g: Option<Uuid>| -> Place {
        if let Some(p) = places.get(&g) {
            return *p;
        }
        let p = if own(g) {
            Place::Direct
        } else if let Some(top) = g.and_then(|id| top_child.get(&id)) {
            Place::Folder(*top)
        } else {
            let mut y = g;
            let mut below: Option<Uuid> = None;
            let mut steps = 0;
            while !in_chain.contains(&y) && steps <= MAX_GROUP_DEPTH + 1 {
                below = y;
                y = y.and_then(|id| tree.pvis(id));
                steps += 1;
            }
            let at = if in_chain.contains(&y) { y } else { None };
            match below {
                None => Place::NodeStub { at },
                // L is drawn flat, so it holds the far node itself and no box for its folder.
                Some(_) if tree.in_site(at, rows) => Place::NodeStub { at },
                Some(folder) => Place::FolderStub { folder, at },
            }
        };
        places.insert(g, p);
        p
    };

    // 4. Per-folder direct tallies, and the level's own nodes.
    let node_group: HashMap<Uuid, Option<Uuid>> = input.node_groups.iter().copied().collect();
    let state_of = |n: Uuid| input.states.get(&n).copied().unwrap_or(NodeState::Unknown);
    let mut direct_counts: HashMap<Uuid, GroupStateCounts> = HashMap::new();
    let mut direct_node_count = 0i64;
    for &(n, g) in input.node_groups {
        if own(g) {
            direct_node_count += 1;
        }
        if let Some(g) = g {
            tally(direct_counts.entry(g).or_default(), state_of(n));
        }
    }

    // 5. Bundle every visible link by the pair of things it joins on this level.
    let mut bundles: BTreeMap<(EndKey, EndKey), Bundle> = BTreeMap::new();
    let mut stubs: BTreeMap<EndKey, MapStub> = BTreeMap::new();
    // A flat drawing's linked nodes so far, to give up as soon as it is too large.
    let mut flat_linked: HashSet<Uuid> = HashSet::new();
    for link in input.links {
        let (Some(a), Some(b)) = (link.a_node, link.b_node) else {
            continue;
        };
        let (a, b) = (a.as_uuid(), b.as_uuid());
        let (Some(&ga), Some(&gb)) = (node_group.get(&a), node_group.get(&b)) else {
            // One end is not a node the caller may see: the whole link is withheld.
            continue;
        };
        if a == b {
            continue;
        }
        let (pa, pb) = (place_of(ga), place_of(gb));
        let ea = endpoint(pa, a);
        let eb = endpoint(pb, b);
        let outside = |p: Place| matches!(p, Place::NodeStub { .. } | Place::FolderStub { .. });
        if (outside(pa) && outside(pb)) || endpoint_key(&ea) == endpoint_key(&eb) {
            // Both ends beyond this level, or both inside the same box: not this level's line.
            continue;
        }
        for (p, e, n) in [(pa, &ea, a), (pb, &eb, b)] {
            if let Some(stub) = stub_of(p, n) {
                stubs.entry(endpoint_key(e)).or_insert(stub);
            }
            if flatten && e.kind == MapEndpointKind::Node {
                flat_linked.insert(e.id);
            }
        }
        let (ka, kb) = (endpoint_key(&ea), endpoint_key(&eb));
        let swap = kb < ka;
        let (first, second) = if swap { (eb, ea) } else { (ea, eb) };
        let key = (endpoint_key(&first), endpoint_key(&second));
        let source = LinkSource::best(&link.sources).unwrap_or(LinkSource::L3Subnet);
        let member = if swap {
            MapEdgeMember {
                link_id: link.id,
                a_node: b,
                b_node: a,
                a_if_name: link.b_if_name.clone(),
                b_if_name: link.a_if_name.clone(),
                source,
                subnet: link.subnet.clone(),
            }
        } else {
            MapEdgeMember {
                link_id: link.id,
                a_node: a,
                b_node: b,
                a_if_name: link.a_if_name.clone(),
                b_if_name: link.b_if_name.clone(),
                source,
                subnet: link.subnet.clone(),
            }
        };
        let bundle = bundles.entry(key).or_insert_with(|| Bundle {
            a: first,
            b: second,
            members: Vec::new(),
            sources: Vec::new(),
        });
        for s in &link.sources {
            if !bundle.sources.contains(s) {
                bundle.sources.push(*s);
            }
        }
        bundle.members.push(member);
        if flatten && (flat_linked.len() > MAP_MAX_NODES || bundles.len() > MAP_MAX_EDGES) {
            return None;
        }
    }

    let mut linked: HashSet<Uuid> = HashSet::new();
    let mut edges: Vec<MapEdge> = bundles
        .into_values()
        .map(|mut b| {
            for e in [&b.a, &b.b] {
                if e.kind == MapEndpointKind::Node {
                    linked.insert(e.id);
                }
            }
            b.members.sort_by_key(|m| (m.source.rank(), m.link_id));
            b.sources.sort_unstable();
            let count = b.members.len() as i64;
            b.members.truncate(MAP_EDGE_MEMBERS_MAX);
            MapEdge {
                id: format!(
                    "{}:{}|{}:{}",
                    kind_token(b.a.kind),
                    b.a.id,
                    kind_token(b.b.kind),
                    b.b.id
                ),
                source: LinkSource::best(&b.sources).unwrap_or(LinkSource::L3Subnet),
                sources: b.sources,
                a: b.a,
                b: b.b,
                count,
                members: b.members,
            }
        })
        .collect();
    edges.sort_by(|x, y| x.id.cmp(&y.id));

    // 6. The boxes, with their subtree's tallies.
    let folders: Vec<MapFolder> = subfolders
        .iter()
        .map(|g| {
            let mut counts = GroupStateCounts::default();
            for (member, top) in &top_child {
                if *top == g.id {
                    if let Some(c) = direct_counts.get(member) {
                        counts.add_assign(c);
                    }
                }
            }
            MapFolder {
                id: g.id,
                name: g.name.clone(),
                group_type: g.group_type.clone(),
                node_count: counts.total(),
                counts,
            }
        })
        .collect();

    let crumb = |id: Uuid| MapBreadcrumb {
        id,
        name: rows.get(&id).map(|g| g.name.clone()).unwrap_or_default(),
    };
    // The folders from just below the level down to a node's own, memoized per folder.
    let mut paths: HashMap<Uuid, Vec<MapBreadcrumb>> = HashMap::new();
    let mut path_of = |g: Option<Uuid>| -> Vec<MapBreadcrumb> {
        let Some(g) = g else { return Vec::new() };
        paths
            .entry(g)
            .or_insert_with(|| {
                let mut out = Vec::new();
                let mut cur = Some(g);
                while let Some(id) = cur {
                    if Some(id) == input.level || out.len() > MAX_GROUP_DEPTH {
                        break;
                    }
                    out.push(crumb(id));
                    cur = tree.pvis(id);
                }
                out.reverse();
                out
            })
            .clone()
    };
    let mut nodes: Vec<MapNode> = linked
        .iter()
        .map(|&id| MapNode {
            id,
            name: String::new(),
            state: state_of(id),
            root_cause: input.root_causes.get(&id).copied(),
            folder_path: path_of(node_group.get(&id).copied().flatten()),
            access_point: false,
            role: MapRole::Other,
            role_reason: MapRoleReason::Default,
            subnet_count: None,
        })
        .collect();
    nodes.sort_by_key(|n| n.id);

    let mut stubs: Vec<MapStub> = stubs.into_values().collect();
    for s in &mut stubs {
        if s.kind == MapStubKind::Folder {
            s.name = rows.get(&s.id).map(|g| g.name.clone()).unwrap_or_default();
        }
    }

    // 7. Where the level sits.
    let breadcrumbs: Vec<MapBreadcrumb> = chain
        .iter()
        .skip(1)
        .rev()
        .filter_map(|g| g.map(crumb))
        .collect();

    // 8. The bound: a level too large to draw keeps its boxes, so the operator can go down a level.
    let linked_node_count = linked.len() as i64;
    let edge_count = edges.len() as i64;
    let overflow = linked.len() > MAP_MAX_NODES || edges.len() > MAP_MAX_EDGES;
    if flatten && overflow {
        return None;
    }
    if overflow {
        nodes.clear();
        stubs.clear();
        edges.clear();
    }
    Some(MapLevel {
        group: input.level.map(crumb),
        breadcrumbs,
        folders,
        subfolder_count,
        nodes,
        stubs,
        edges,
        direct_node_count,
        linked_node_count,
        edge_count,
        isolated_count: direct_node_count - linked_node_count,
        overflow,
        flattened: flatten,
        node_limit: MAP_MAX_NODES as i64,
        edge_limit: MAP_MAX_EDGES as i64,
        derived_at: None,
        summary: Default::default(),
    })
}

fn tally(c: &mut GroupStateCounts, s: NodeState) {
    match s {
        NodeState::Ok => c.ok += 1,
        NodeState::Warning => c.warning += 1,
        NodeState::Critical => c.critical += 1,
        NodeState::Unknown => c.unknown += 1,
        NodeState::Unreachable => c.unreachable += 1,
        NodeState::Maintenance => c.maintenance += 1,
    }
}

fn endpoint(p: Place, node: Uuid) -> MapEndpoint {
    match p {
        Place::Direct => MapEndpoint {
            kind: MapEndpointKind::Node,
            id: node,
        },
        Place::Folder(f) => MapEndpoint {
            kind: MapEndpointKind::Folder,
            id: f,
        },
        Place::NodeStub { .. } => MapEndpoint {
            kind: MapEndpointKind::External,
            id: node,
        },
        Place::FolderStub { folder, .. } => MapEndpoint {
            kind: MapEndpointKind::External,
            id: folder,
        },
    }
}

fn stub_of(p: Place, node: Uuid) -> Option<MapStub> {
    match p {
        Place::NodeStub { at } => Some(MapStub {
            kind: MapStubKind::Node,
            id: node,
            name: String::new(),
            level_group: at,
        }),
        Place::FolderStub { folder, at } => Some(MapStub {
            kind: MapStubKind::Folder,
            id: folder,
            name: String::new(),
            level_group: at,
        }),
        Place::Direct | Place::Folder(_) => None,
    }
}

/// The node ids whose names the level still needs: its own nodes and its node stubs.
pub(crate) fn names_needed(level: &MapLevel) -> Vec<Uuid> {
    level
        .nodes
        .iter()
        .map(|n| n.id)
        .chain(
            level
                .stubs
                .iter()
                .filter(|s| s.kind == MapStubKind::Node)
                .map(|s| s.id),
        )
        .collect()
}

/// Fill in node names. An id the resolver did not answer keeps its id as its name rather than
/// leaving a blank box.
pub(crate) fn apply_names(level: &mut MapLevel, names: &HashMap<Uuid, String>) {
    let name = |id: Uuid| names.get(&id).cloned().unwrap_or_else(|| id.to_string());
    for n in &mut level.nodes {
        n.name = name(n.id);
    }
    for s in &mut level.stubs {
        if s.kind == MapStubKind::Node {
            s.name = name(s.id);
        }
    }
}

/// What is known about one drawn node, for deciding its [`MapRole`] (ADR-191 Inc.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoleFacts {
    pub kind: NodeKind,
    /// A Meraki node's product type (`appliance`, `switch`, `wireless`, ...).
    pub meraki_product: Option<String>,
    /// The category of the node's device profile, when it has one.
    pub category: Option<ProfileCategory>,
    /// How many subnets the node has an address in; `None` when no address walk is recorded.
    pub subnet_count: Option<usize>,
    /// A link touching the node came from OSPF, BGP or the routing table.
    pub routing_adjacency: bool,
    /// Its default route leaves its site (ADR-191 Inc.10): `None` when it was never asked, an older
    /// poller sent no answer, or the node is filed in no site.
    pub exits_site: Option<bool>,
}

/// What a profile category says about a node's place on the map.
enum CategoryClass {
    Edge,
    AccessPoint,
    /// A switch. `promotable` is false for `L2Switch`: an operator or the vendor rule said it
    /// does not route, and that beats an address count.
    Switch {
        promotable: bool,
    },
    /// Says nothing either way, so the observations decide.
    Unclassified,
    /// Not a network device the map ranks.
    Other,
}

fn category_class(c: ProfileCategory) -> CategoryClass {
    use ProfileCategory as C;
    match c {
        C::Router | C::Firewall => CategoryClass::Edge,
        C::WirelessAp => CategoryClass::AccessPoint,
        // The built-in switch profiles are product families, not observations: a Catalyst used as
        // a pure access switch is `L3Switch` too, so only what it was seen doing can promote it.
        C::L3Switch => CategoryClass::Switch { promotable: true },
        C::L2Switch => CategoryClass::Switch { promotable: false },
        C::GenericSnmp => CategoryClass::Unclassified,
        C::WirelessController
        | C::LoadBalancer
        | C::Server
        | C::Hypervisor
        | C::Storage
        | C::Power
        | C::Printer
        | C::PingOnly
        | C::UrlCheck
        | C::DnsCheck => CategoryClass::Other,
    }
}

/// The Site folder `group` is filed under: itself, or its nearest Site ancestor. Walks the
/// unscoped parents; a folder in no site, or a loop, answers `None`. The one walk both the flat
/// drawing (Inc.2) and the way-out judgement (Inc.10) use; `is_site` is the only thing that
/// differs between them.
pub(crate) fn site_of_group(
    group: Option<Uuid>,
    parent: &HashMap<Uuid, Option<Uuid>>,
    is_site: &dyn Fn(Uuid) -> bool,
) -> Option<Uuid> {
    let mut cur = group;
    let mut seen = HashSet::new();
    while let Some(g) = cur {
        if !seen.insert(g) || seen.len() > MAX_GROUP_DEPTH + 1 {
            return None;
        }
        if is_site(g) {
            return Some(g);
        }
        cur = parent.get(&g).copied().flatten();
    }
    None
}

/// Which nodes route out of their site (ADR-191 Inc.10 decision 34): a node whose default route
/// points somewhere that is not inside its site. A hop is inside when a node of the same site claims
/// it (`owners`: as its management address or on a port), **or** when it lies in a subnet another
/// node of the same site has an address in (`subnets`, by node). The second clause is what keeps a
/// core switch pointing at its routers' HSRP or VRRP address from reading as a way out: that shared
/// address is often recorded on neither router, but both sit on its subnet. The asking node's own
/// subnets do not count, since every gateway is on one of them.
///
/// `hops` is each asked node's default next hops (empty = no default route, which is not a way
/// out; the unspecified address = a route out of an interface, which nothing claims, so it leaves).
/// A node in no site is left out, and so reads as unknown.
pub(crate) fn site_exits(
    hops: &HashMap<Uuid, Vec<std::net::IpAddr>>,
    owners: &HashMap<std::net::IpAddr, Vec<Uuid>>,
    subnets: &HashMap<Uuid, std::collections::BTreeSet<yagra_common::SubnetKey>>,
    site: &dyn Fn(Uuid) -> Option<Uuid>,
) -> HashMap<Uuid, bool> {
    hops.iter()
        .filter_map(|(node, hs)| {
            let home = site(*node)?;
            let neighbour_on = |h: &std::net::IpAddr| {
                let Some(host) = yagra_common::subnet_key(*h, yagra_common::host_prefix_len(*h))
                else {
                    return false;
                };
                subnets.iter().any(|(other, nets)| {
                    other != node
                        && site(*other) == Some(home)
                        && nets.iter().any(|net| net.contains(&host))
                })
            };
            let inside = |h: &std::net::IpAddr| {
                owners
                    .get(h)
                    .is_some_and(|o| o.iter().any(|c| site(*c) == Some(home)))
                    || neighbour_on(h)
            };
            Some((*node, hs.iter().any(|h| !inside(h))))
        })
        .collect()
}

/// A node's role on the map and why (ADR-191 Inc.6 decision 19). The first rule that applies wins:
/// what the node *is* (an imported AP, a Meraki product, a router or firewall profile) before what
/// it was *seen doing* (routing adjacencies, addresses in several subnets), before what its switch
/// profile claims. `ipForwarding` is deliberately not an input: an OS that cannot turn routing off
/// answers 1 on every access switch.
pub(crate) fn role_of(f: &RoleFacts) -> (MapRole, MapRoleReason) {
    match f.kind {
        NodeKind::WirelessAp => return (MapRole::AccessPoint, MapRoleReason::WirelessAp),
        NodeKind::Meraki => {
            let role = match f
                .meraki_product
                .as_deref()
                .map(|p| category_class(yagra_common::category_for_product_type(p)))
            {
                Some(CategoryClass::Edge) => MapRole::Edge,
                Some(CategoryClass::AccessPoint) => MapRole::AccessPoint,
                Some(CategoryClass::Switch { .. }) => MapRole::L2Switch,
                Some(CategoryClass::Unclassified | CategoryClass::Other) | None => MapRole::Other,
            };
            return (role, MapRoleReason::MerakiProduct);
        }
        NodeKind::Url | NodeKind::Dns => return (MapRole::Other, MapRoleReason::Default),
        NodeKind::Device => {}
    }
    let (switch, promotable) = match f.category.map(category_class) {
        Some(CategoryClass::Edge) => return (MapRole::Edge, MapRoleReason::ProfileCategory),
        Some(CategoryClass::AccessPoint) => {
            return (MapRole::AccessPoint, MapRoleReason::ProfileCategory)
        }
        // A server speaking BGP (a route reflector, a load balancer's announcer) is still drawn
        // as what it is.
        Some(CategoryClass::Other) => return (MapRole::Other, MapRoleReason::Default),
        Some(CategoryClass::Switch { promotable }) => (true, promotable),
        Some(CategoryClass::Unclassified) | None => (false, true),
    };
    // ADR-191 Inc.10: a device that routes, and whose default route leaves its site, is the site's
    // way out — a router the profile does not call one (a C800 under a generic profile). Only one
    // that routes: an access switch pointing its gateway at an address nobody monitors must not
    // climb to the top row.
    let routes = f.routing_adjacency || f.subnet_count.is_some_and(|n| n >= 2);
    if promotable && routes && f.exits_site == Some(true) {
        return (MapRole::Edge, MapRoleReason::DefaultRoute);
    }
    if f.routing_adjacency {
        return (MapRole::L3Switch, MapRoleReason::RoutingAdjacency);
    }
    if promotable && f.subnet_count.is_some_and(|n| n >= 2) {
        return (MapRole::L3Switch, MapRoleReason::Subnets);
    }
    if switch {
        return (MapRole::L2Switch, MapRoleReason::ProfileCategory);
    }
    (MapRole::Other, MapRoleReason::Default)
}

/// Whether a link came from a routing adjacency, which only a router or an L3 switch holds.
pub(crate) fn is_routing_evidence(source: LinkSource) -> bool {
    match source {
        LinkSource::Ospf | LinkSource::Bgp | LinkSource::Route => true,
        LinkSource::Manual | LinkSource::Lldp | LinkSource::Cdp | LinkSource::L3Subnet => false,
    }
}

/// Give every drawn node its role. A node with no facts is `other`. The access-point mark follows
/// the role, so the symbol and the row can never disagree.
pub(crate) fn apply_roles(level: &mut MapLevel, facts: &HashMap<Uuid, RoleFacts>) {
    for n in &mut level.nodes {
        let f = facts.get(&n.id);
        let (role, reason) = f.map_or((MapRole::Other, MapRoleReason::Default), role_of);
        n.role = role;
        n.role_reason = reason;
        n.subnet_count = f
            .and_then(|f| f.subnet_count)
            .map(|c| u32::try_from(c).unwrap_or(u32::MAX));
        n.access_point = role == MapRole::AccessPoint;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use yagra_common::NodeId;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn folder(
        n: u128,
        name: &str,
        ty: GroupType,
        parent: Option<u128>,
    ) -> (FolderRow, (Uuid, Option<Uuid>)) {
        (
            FolderRow {
                id: id(n),
                name: name.to_owned(),
                group_type: ty.key().to_owned(),
                sort_order: n as f64,
            },
            (id(n), parent.map(id)),
        )
    }

    fn link(n: i64, a: u128, b: u128, src: LinkSource) -> StoredLink {
        StoredLink {
            id: n,
            a_node: Some(NodeId(id(a))),
            b_node: Some(NodeId(id(b))),
            a_ifindex: None,
            b_ifindex: None,
            a_if_name: Some(format!("a{n}")),
            b_if_name: Some(format!("b{n}")),
            sources: vec![src],
            subnet: None,
            forced_parent: None,
            first_seen: Utc::now(),
            last_seen: Utc::now(),
        }
    }

    /// The fixture tree:
    ///
    /// ```text
    /// (whole)                          nodes 100 (ungrouped)
    /// ├─ east   (1, region)            node 110
    /// │  ├─ site-a (2, site)           nodes 120 121
    /// │  │  └─ floor-1 (3, generic)    node 130
    /// │  └─ site-b (4, site)           node 140
    /// └─ west   (5, region)
    ///    └─ site-c (6, site)           node 160
    /// ```
    struct Fx {
        groups: Vec<FolderRow>,
        edges: Vec<(Uuid, Option<Uuid>)>,
        node_groups: Vec<(Uuid, Option<Uuid>)>,
        states: HashMap<Uuid, NodeState>,
    }

    fn fx() -> Fx {
        let (groups, edges) = [
            folder(1, "east", GroupType::Region, None),
            folder(2, "site-a", GroupType::Site, Some(1)),
            folder(3, "floor-1", GroupType::Generic, Some(2)),
            folder(4, "site-b", GroupType::Site, Some(1)),
            folder(5, "west", GroupType::Region, None),
            folder(6, "site-c", GroupType::Site, Some(5)),
        ]
        .into_iter()
        .unzip();
        let node_groups = vec![
            (id(100), None),
            (id(110), Some(id(1))),
            (id(120), Some(id(2))),
            (id(121), Some(id(2))),
            (id(130), Some(id(3))),
            (id(140), Some(id(4))),
            (id(160), Some(id(6))),
        ];
        let mut states: HashMap<Uuid, NodeState> = node_groups
            .iter()
            .map(|(n, _)| (*n, NodeState::Ok))
            .collect();
        states.insert(id(130), NodeState::Critical);
        Fx {
            groups,
            edges,
            node_groups,
            states,
        }
    }

    fn run(
        f: &Fx,
        level: Option<u128>,
        links: &[StoredLink],
        visible: Option<&HashSet<Uuid>>,
    ) -> MapLevel {
        let rc = HashMap::new();
        compute(&LevelInput {
            level: level.map(id),
            edges: &f.edges,
            groups: &f.groups,
            visible,
            node_groups: &f.node_groups,
            states: &f.states,
            root_causes: &rc,
            links,
        })
    }

    fn stub(l: &MapLevel, of: u128) -> &MapStub {
        l.stubs
            .iter()
            .find(|s| s.id == id(of))
            .unwrap_or_else(|| panic!("no stub {of}: {:?}", l.stubs))
    }

    #[test]
    fn the_whole_level_draws_top_folders_as_boxes_with_subtree_counts() {
        let f = fx();
        let l = run(&f, None, &[link(1, 100, 130, LinkSource::Lldp)], None);
        let names: Vec<&str> = l.folders.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["east", "west"]);
        let east = &l.folders[0];
        assert_eq!(east.node_count, 5, "east holds 110, 120, 121, 130, 140");
        assert_eq!(east.counts.critical, 1);
        assert_eq!(l.edges.len(), 1);
        assert_eq!(l.edges[0].a.kind, MapEndpointKind::Node);
        assert_eq!(l.edges[0].b.kind, MapEndpointKind::Folder);
        assert_eq!(l.edges[0].b.id, id(1));
        assert!(l.breadcrumbs.is_empty() && l.group.is_none());
        assert_eq!(l.direct_node_count, 1);
        assert_eq!(l.isolated_count, 0);
    }

    #[test]
    fn a_sibling_folder_is_a_folder_stub_on_the_common_parent() {
        let f = fx();
        // site-a (2) ↔ site-b (4): common ancestor east (1).
        let l = run(&f, Some(2), &[link(1, 120, 140, LinkSource::Cdp)], None);
        let s = stub(&l, 4);
        assert_eq!(s.kind, MapStubKind::Folder);
        assert_eq!(s.name, "site-b");
        assert_eq!(s.level_group, Some(id(1)));
        let crumbs: Vec<&str> = l.breadcrumbs.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(crumbs, ["east"]);
        assert_eq!(l.group.as_ref().map(|g| g.name.as_str()), Some("site-a"));
    }

    #[test]
    fn a_cousin_is_a_stub_for_the_top_folder_under_the_whole_network() {
        let f = fx();
        // site-a (2) ↔ site-c (6): common ancestor is the whole network; the stub is west (5).
        let l = run(&f, Some(2), &[link(1, 121, 160, LinkSource::Lldp)], None);
        let s = stub(&l, 5);
        assert_eq!(s.kind, MapStubKind::Folder);
        assert_eq!(s.level_group, None);
    }

    #[test]
    fn a_peer_directly_in_an_ancestor_or_ungrouped_is_a_node_stub() {
        let f = fx();
        let l = run(
            &f,
            Some(2),
            &[
                link(1, 120, 110, LinkSource::Lldp),
                link(2, 120, 100, LinkSource::Lldp),
            ],
            None,
        );
        assert_eq!(stub(&l, 110).kind, MapStubKind::Node);
        assert_eq!(stub(&l, 110).level_group, Some(id(1)));
        assert_eq!(stub(&l, 100).kind, MapStubKind::Node);
        assert_eq!(stub(&l, 100).level_group, None);
    }

    #[test]
    fn links_are_bundled_with_count_sources_best_and_orientation() {
        let f = fx();
        let links = vec![
            link(3, 130, 120, LinkSource::L3Subnet),
            link(1, 120, 130, LinkSource::Cdp),
            link(2, 121, 130, LinkSource::Lldp),
        ];
        // On east (1): 120/121 and 130 are all inside site-a — nothing to draw.
        assert!(run(&f, Some(1), &links, None).edges.is_empty());
        // On site-a (2), drawn flat: 120 and 121 each to 130 on floor-1.
        let l = run(&f, Some(2), &links, None);
        assert_eq!(l.edges.len(), 2);
        let e = l
            .edges
            .iter()
            .find(|e| e.a.id == id(120))
            .expect("120's bundle");
        assert_eq!(e.count, 2);
        assert_eq!(e.sources, vec![LinkSource::Cdp, LinkSource::L3Subnet]);
        assert_eq!(e.source, LinkSource::Cdp);
        assert!(
            e.members.iter().all(|m| m.a_node == id(120)),
            "every member faces the bundle's `a` end"
        );
        assert_eq!(e.members[0].link_id, 1, "strongest source first");
        let flipped = e.members.iter().find(|m| m.link_id == 3).unwrap();
        assert_eq!(
            flipped.a_if_name.as_deref(),
            Some("b3"),
            "ports follow the flip"
        );
        assert_eq!(l.linked_node_count, 3);
    }

    #[test]
    fn a_bundle_lists_at_most_fifty_members_but_counts_all() {
        let f = fx();
        let links: Vec<StoredLink> = (0..60)
            .map(|n| link(n, 120, 140, LinkSource::Lldp))
            .collect();
        let l = run(&f, Some(1), &links, None);
        assert_eq!(l.edges.len(), 1);
        assert_eq!(l.edges[0].count, 60);
        assert_eq!(l.edges[0].members.len(), MAP_EDGE_MEMBERS_MAX);
    }

    #[test]
    fn a_link_with_an_invisible_end_is_withheld() {
        let mut f = fx();
        // A scoped caller who sees east's subtree only.
        let vis: HashSet<Uuid> = [1, 2, 3, 4].map(id).into_iter().collect();
        f.node_groups
            .retain(|(_, g)| g.is_some_and(|g| vis.contains(&g)));
        let l = run(
            &f,
            Some(1),
            &[link(1, 120, 160, LinkSource::Lldp)],
            Some(&vis),
        );
        assert!(l.edges.is_empty() && l.stubs.is_empty());
    }

    #[test]
    fn a_scoped_callers_whole_level_is_their_scope_roots() {
        let mut f = fx();
        // Visible: site-a's subtree and site-c — two roots whose parents are hidden.
        let vis: HashSet<Uuid> = [2, 3, 6].map(id).into_iter().collect();
        f.node_groups
            .retain(|(_, g)| g.is_some_and(|g| vis.contains(&g)));
        f.groups
            .retain(|g| vis.contains(&g.id) || g.id == id(1) || g.id == id(5));
        let links = [link(1, 120, 160, LinkSource::Lldp)];
        let whole = run(&f, None, &links, Some(&vis));
        let names: Vec<&str> = whole.folders.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["site-a", "site-c"]);
        assert_eq!(whole.edges.len(), 1);
        let inside = run(&f, Some(2), &links, Some(&vis));
        assert!(
            inside.breadcrumbs.is_empty(),
            "the chain stops at the scope root"
        );
        let s = stub(&inside, 6);
        assert_eq!(s.level_group, None);
    }

    #[test]
    fn a_level_over_the_bound_keeps_its_boxes_and_drops_the_rest() {
        let mut f = fx();
        let n = MAP_MAX_NODES as u128 + 1;
        let mut links = Vec::new();
        for i in 0..n {
            f.node_groups.push((id(10_000 + i), None));
            links.push(link(i as i64, 10_000 + i, 100, LinkSource::Lldp));
        }
        let l = run(&f, None, &links, None);
        assert!(l.overflow);
        assert!(l.nodes.is_empty() && l.edges.is_empty());
        assert_eq!(l.folders.len(), 2);
        assert_eq!(l.linked_node_count, n as i64 + 1);
    }

    #[test]
    fn input_order_does_not_change_the_output() {
        let f = fx();
        let mut links = vec![
            link(1, 120, 140, LinkSource::Lldp),
            link(2, 121, 140, LinkSource::Cdp),
            link(3, 110, 160, LinkSource::L3Subnet),
            link(4, 130, 100, LinkSource::Lldp),
        ];
        let one = serde_json::to_value(run(&f, Some(2), &links, None)).unwrap();
        links.reverse();
        let two = serde_json::to_value(run(&f, Some(2), &links, None)).unwrap();
        assert_eq!(one, two);
    }

    /// Adds `room-1` (7, generic) under floor-1, holding node 170.
    fn with_room(f: &mut Fx) {
        let (row, edge) = folder(7, "room-1", GroupType::Generic, Some(3));
        f.groups.push(row);
        f.edges.push(edge);
        f.node_groups.push((id(170), Some(id(7))));
        f.states.insert(id(170), NodeState::Ok);
    }

    fn node(l: &MapLevel, of: u128) -> &MapNode {
        l.nodes
            .iter()
            .find(|n| n.id == id(of))
            .unwrap_or_else(|| panic!("no node {of}: {:?}", l.nodes))
    }

    #[test]
    fn a_site_draws_its_subfolders_nodes_flat_each_tagged_with_its_folder() {
        let f = fx();
        let links = [
            link(1, 120, 130, LinkSource::Lldp),
            link(2, 130, 140, LinkSource::Lldp),
        ];
        let l = run(&f, Some(2), &links, None);
        assert!(l.flattened);
        assert!(l.folders.is_empty(), "no boxes inside a site");
        assert!(node(&l, 120).folder_path.is_empty(), "directly in site-a");
        let path = &node(&l, 130).folder_path;
        assert_eq!(path.len(), 1, "130 is on floor-1");
        assert_eq!((path[0].id, path[0].name.as_str()), (id(3), "floor-1"));
        assert_eq!(
            l.subfolder_count, 1,
            "floor-1 is counted though it is not a box"
        );
        let inside = l.edges.iter().find(|e| e.b.id == id(130)).unwrap();
        assert_eq!(
            (inside.a.kind, inside.b.kind),
            (MapEndpointKind::Node, MapEndpointKind::Node)
        );
        // The link leaving the site is still a stub for site-b, opening on east.
        let s = stub(&l, 4);
        assert_eq!((s.kind, s.level_group), (MapStubKind::Folder, Some(id(1))));
        assert_eq!(l.direct_node_count, 3, "120, 121 and 130");
        assert_eq!(l.isolated_count, 1, "121");
    }

    #[test]
    fn a_folder_beneath_a_site_is_flat_too() {
        let mut f = fx();
        with_room(&mut f);
        let links = [
            link(1, 130, 170, LinkSource::Lldp),
            link(2, 130, 120, LinkSource::Lldp),
        ];
        let l = run(&f, Some(3), &links, None);
        assert!(l.flattened);
        assert_eq!(path_names(node(&l, 170)), ["room-1"]);
        assert!(node(&l, 130).folder_path.is_empty());
        // 120 sits directly in site-a, the common ancestor: a node stub opening there.
        let s = stub(&l, 120);
        assert_eq!((s.kind, s.level_group), (MapStubKind::Node, Some(id(2))));
    }

    #[test]
    fn a_region_still_draws_its_sites_as_boxes() {
        let f = fx();
        let l = run(&f, Some(1), &[link(1, 110, 130, LinkSource::Lldp)], None);
        assert!(!l.flattened);
        let names: Vec<&str> = l.folders.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["site-a", "site-b"]);
        assert_eq!(l.edges[0].b.kind, MapEndpointKind::Folder);
        assert!(l.nodes.iter().all(|n| n.folder_path.is_empty()));
        assert_eq!(l.subfolder_count, 2);
        assert!(!run(&f, None, &[], None).flattened);
    }

    #[test]
    fn a_flat_site_over_the_bound_falls_back_to_boxes() {
        let mut f = fx();
        let n = MAP_MAX_NODES as u128 + 1;
        let mut links = Vec::new();
        for i in 0..n {
            f.node_groups.push((id(10_000 + i), Some(id(3))));
            links.push(link(i as i64, 10_000 + i, 120, LinkSource::Lldp));
        }
        let l = run(&f, Some(2), &links, None);
        assert!(!l.flattened && !l.overflow);
        assert_eq!(l.folders.len(), 1, "floor-1 is a box again");
        assert_eq!(l.edges.len(), 1);
        assert_eq!(l.edges[0].count, n as i64);
        assert_eq!(l.direct_node_count, 2, "only site-a's own nodes");
    }

    #[test]
    fn a_caller_scoped_below_a_hidden_site_still_gets_the_flat_drawing() {
        let mut f = fx();
        with_room(&mut f);
        let vis: HashSet<Uuid> = [3, 7].map(id).into_iter().collect();
        f.node_groups
            .retain(|(_, g)| g.is_some_and(|g| vis.contains(&g)));
        // Rows for the breadcrumb ancestors are what `visible_groups` hands over too.
        f.groups
            .retain(|g| vis.contains(&g.id) || g.id == id(1) || g.id == id(2));
        let l = run(
            &f,
            Some(3),
            &[link(1, 130, 170, LinkSource::Lldp)],
            Some(&vis),
        );
        assert!(l.flattened);
        assert!(
            l.breadcrumbs.is_empty(),
            "the chain still stops at the scope root"
        );
        assert_eq!(l.edges.len(), 1);
    }

    fn path_names(n: &MapNode) -> Vec<&str> {
        n.folder_path.iter().map(|g| g.name.as_str()).collect()
    }

    /// Adds `floor-2` (8, generic) under site-a, holding node 180.
    fn with_floor_2(f: &mut Fx) {
        let (row, edge) = folder(8, "floor-2", GroupType::Generic, Some(2));
        f.groups.push(row);
        f.edges.push(edge);
        f.node_groups.push((id(180), Some(id(8))));
        f.states.insert(id(180), NodeState::Ok);
    }

    #[test]
    fn a_sibling_floor_inside_a_site_is_a_node_stub_opening_on_the_site() {
        let mut f = fx();
        with_floor_2(&mut f);
        // floor-1 (3) ↔ floor-2 (8): L is site-a, drawn flat, which has no box for floor-2.
        let l = run(&f, Some(3), &[link(1, 130, 180, LinkSource::Lldp)], None);
        let s = stub(&l, 180);
        assert_eq!((s.kind, s.level_group), (MapStubKind::Node, Some(id(2))));
        assert!(l.stubs.iter().all(|s| s.id != id(8)), "no stub for floor-2");
        // The site it opens on does draw 180.
        let site = run(&f, Some(2), &[link(1, 130, 180, LinkSource::Lldp)], None);
        assert!(site.flattened);
        assert_eq!(path_names(node(&site, 180)), ["floor-2"]);
    }

    #[test]
    fn a_node_two_folders_deep_carries_the_whole_path() {
        let mut f = fx();
        with_room(&mut f);
        let l = run(&f, Some(2), &[link(1, 120, 170, LinkSource::Lldp)], None);
        assert_eq!(path_names(node(&l, 170)), ["floor-1", "room-1"]);
        assert!(node(&l, 120).folder_path.is_empty());
    }

    #[test]
    fn subfolder_count_is_the_direct_children_on_both_kinds_of_level() {
        let mut f = fx();
        with_room(&mut f);
        with_floor_2(&mut f);
        assert_eq!(run(&f, None, &[], None).subfolder_count, 2, "east, west");
        assert_eq!(
            run(&f, Some(1), &[], None).subfolder_count,
            2,
            "site-a, site-b"
        );
        let site = run(&f, Some(2), &[], None);
        assert!(site.flattened && site.folders.is_empty());
        assert_eq!(site.subfolder_count, 2, "floor-1, floor-2 — not room-1");
        assert_eq!(run(&f, Some(7), &[], None).subfolder_count, 0);
    }

    #[test]
    fn names_fall_back_to_the_id() {
        let f = fx();
        let mut l = run(&f, Some(2), &[link(1, 120, 110, LinkSource::Lldp)], None);
        let need = names_needed(&l);
        assert!(need.contains(&id(120)) && need.contains(&id(110)));
        let names: HashMap<Uuid, String> = [(id(120), "sw-01".to_owned())].into_iter().collect();
        apply_names(&mut l, &names);
        assert_eq!(l.nodes[0].name, "sw-01");
        assert_eq!(stub(&l, 110).name, id(110).to_string());
    }

    fn facts(kind: NodeKind) -> RoleFacts {
        RoleFacts {
            kind,
            meraki_product: None,
            category: None,
            subnet_count: None,
            routing_adjacency: false,
            exits_site: None,
        }
    }

    fn device(category: Option<ProfileCategory>, subnets: Option<usize>) -> RoleFacts {
        RoleFacts {
            category,
            subnet_count: subnets,
            ..facts(NodeKind::Device)
        }
    }

    fn exit(f: RoleFacts, exits_site: Option<bool>) -> RoleFacts {
        RoleFacts { exits_site, ..f }
    }

    #[test]
    fn a_node_routes_out_of_its_site_when_no_node_of_that_site_claims_its_gateway() {
        let (router, core, sw, other_site_router) = (id(1), id(2), id(3), id(4));
        let (site_a, floor, site_b) = (id(900), id(901), id(902));
        let parent: HashMap<Uuid, Option<Uuid>> =
            [(site_a, None), (floor, Some(site_a)), (site_b, None)]
                .into_iter()
                .collect();
        let sites: HashSet<Uuid> = [site_a, site_b].into_iter().collect();
        let is_site = |g: Uuid| sites.contains(&g);
        assert_eq!(site_of_group(Some(floor), &parent, &is_site), Some(site_a));
        assert_eq!(site_of_group(None, &parent, &is_site), None);
        let filed: HashMap<Uuid, Option<Uuid>> = [
            (router, Some(site_a)),
            (core, Some(floor)),
            (sw, None),
            (other_site_router, Some(site_b)),
        ]
        .into_iter()
        .collect();
        let site = |n: Uuid| site_of_group(filed.get(&n).copied().flatten(), &parent, &is_site);
        let ip = |s: &str| s.parse::<std::net::IpAddr>().unwrap();
        let hops: HashMap<Uuid, Vec<std::net::IpAddr>> = [
            // The router's gateway is the carrier's end of the WAN link: nobody's address.
            (router, vec![ip("198.51.100.9")]),
            // The core points at the router, in a subfolder of the same site.
            (core, vec![ip("192.0.2.253")]),
            // A node in no site reads as unknown.
            (sw, vec![ip("198.51.100.9")]),
            // A gateway claimed by a node in another site still leaves this one.
            (other_site_router, vec![ip("192.0.2.253")]),
        ]
        .into_iter()
        .collect();
        let owners: HashMap<std::net::IpAddr, Vec<Uuid>> =
            [(ip("192.0.2.253"), vec![router])].into_iter().collect();
        let none_on = HashMap::new();
        let got = site_exits(&hops, &owners, &none_on, &site);
        assert_eq!(got.get(&router), Some(&true));
        assert_eq!(got.get(&core), Some(&false));
        assert_eq!(got.get(&sw), None);
        assert_eq!(got.get(&other_site_router), Some(&true));
        // No default route at all is not a way out.
        let none: HashMap<Uuid, Vec<std::net::IpAddr>> = [(core, Vec::new())].into_iter().collect();
        assert_eq!(
            site_exits(&none, &owners, &none_on, &site).get(&core),
            Some(&false)
        );
        // A route out of an interface, with no gateway, leaves.
        let iface: HashMap<Uuid, Vec<std::net::IpAddr>> =
            [(router, vec![ip("0.0.0.0")])].into_iter().collect();
        assert_eq!(
            site_exits(&iface, &owners, &none_on, &site).get(&router),
            Some(&true)
        );
        // The core points at the routers' HSRP address, which no router records. A router of the
        // same site has an address on that subnet, so the hop is inside; the core's own subnets
        // would not have been enough.
        let net = |s: &str, len: u8| yagra_common::subnet_key(ip(s), len).unwrap();
        let vip: HashMap<Uuid, Vec<std::net::IpAddr>> =
            [(core, vec![ip("203.0.113.1")])].into_iter().collect();
        let only_own: HashMap<Uuid, std::collections::BTreeSet<yagra_common::SubnetKey>> =
            [(core, [net("203.0.113.0", 24)].into_iter().collect())]
                .into_iter()
                .collect();
        assert_eq!(
            site_exits(&vip, &owners, &only_own, &site).get(&core),
            Some(&true)
        );
        let mut shared = only_own.clone();
        shared.insert(router, [net("203.0.113.0", 24)].into_iter().collect());
        assert_eq!(
            site_exits(&vip, &owners, &shared, &site).get(&core),
            Some(&false)
        );
        // A router of another site on a subnet with the same numbers does not count.
        let mut elsewhere = only_own;
        elsewhere.insert(
            other_site_router,
            [net("203.0.113.0", 24)].into_iter().collect(),
        );
        assert_eq!(
            site_exits(&vip, &owners, &elsewhere, &site).get(&core),
            Some(&true)
        );
    }

    fn meraki(product: &str) -> RoleFacts {
        RoleFacts {
            meraki_product: Some(product.to_owned()),
            ..facts(NodeKind::Meraki)
        }
    }

    #[test]
    fn every_rule_of_the_role_table_decides_what_it_names() {
        use MapRole as R;
        use MapRoleReason as W;
        use ProfileCategory as C;
        let ospf = RoleFacts {
            routing_adjacency: true,
            ..device(Some(C::GenericSnmp), Some(1))
        };
        let bgp_server = RoleFacts {
            routing_adjacency: true,
            ..device(Some(C::Server), Some(1))
        };
        let cases: Vec<(&str, RoleFacts, (MapRole, MapRoleReason))> = vec![
            (
                "imported AP",
                facts(NodeKind::WirelessAp),
                (R::AccessPoint, W::WirelessAp),
            ),
            ("MR", meraki("wireless"), (R::AccessPoint, W::MerakiProduct)),
            ("MX", meraki("appliance"), (R::Edge, W::MerakiProduct)),
            ("MS", meraki("switch"), (R::L2Switch, W::MerakiProduct)),
            ("MV", meraki("camera"), (R::Other, W::MerakiProduct)),
            ("URL", facts(NodeKind::Url), (R::Other, W::Default)),
            ("DNS", facts(NodeKind::Dns), (R::Other, W::Default)),
            (
                "router",
                device(Some(C::Router), Some(1)),
                (R::Edge, W::ProfileCategory),
            ),
            (
                "firewall",
                device(Some(C::Firewall), None),
                (R::Edge, W::ProfileCategory),
            ),
            (
                "SNMP AP",
                device(Some(C::WirelessAp), None),
                (R::AccessPoint, W::ProfileCategory),
            ),
            (
                "OSPF speaker",
                ospf.clone(),
                (R::L3Switch, W::RoutingAdjacency),
            ),
            ("BGP server", bgp_server, (R::Other, W::Default)),
            (
                "core",
                device(Some(C::L3Switch), Some(3)),
                (R::L3Switch, W::Subnets),
            ),
            (
                "access",
                device(Some(C::L3Switch), Some(1)),
                (R::L2Switch, W::ProfileCategory),
            ),
            (
                "declared L2",
                device(Some(C::L2Switch), Some(3)),
                (R::L2Switch, W::ProfileCategory),
            ),
            (
                "generic, 2 nets",
                device(Some(C::GenericSnmp), Some(2)),
                (R::L3Switch, W::Subnets),
            ),
            (
                "no profile, 2 nets",
                device(None, Some(2)),
                (R::L3Switch, W::Subnets),
            ),
            (
                "server, 2 nets",
                device(Some(C::Server), Some(2)),
                (R::Other, W::Default),
            ),
            (
                "WLC",
                device(Some(C::WirelessController), Some(4)),
                (R::Other, W::Default),
            ),
            (
                "switch, no walk",
                device(Some(C::L3Switch), None),
                (R::L2Switch, W::ProfileCategory),
            ),
            (
                "generic, 1 net",
                device(Some(C::GenericSnmp), Some(1)),
                (R::Other, W::Default),
            ),
            // ADR-191 Inc.10: the site's way out, told by where its default route points.
            (
                "site router, generic profile",
                exit(device(Some(C::GenericSnmp), Some(2)), Some(true)),
                (R::Edge, W::DefaultRoute),
            ),
            (
                "core, default route inside the site",
                exit(device(Some(C::L3Switch), Some(18)), Some(false)),
                (R::L3Switch, W::Subnets),
            ),
            (
                "never asked",
                exit(device(Some(C::GenericSnmp), Some(2)), None),
                (R::L3Switch, W::Subnets),
            ),
            (
                "OSPF speaker leaving the site",
                exit(ospf.clone(), Some(true)),
                (R::Edge, W::DefaultRoute),
            ),
            (
                "declared L2, gateway outside",
                exit(device(Some(C::L2Switch), Some(3)), Some(true)),
                (R::L2Switch, W::ProfileCategory),
            ),
            (
                "access switch, gateway outside",
                exit(device(Some(C::L3Switch), Some(1)), Some(true)),
                (R::L2Switch, W::ProfileCategory),
            ),
            (
                "server leaving the site",
                exit(device(Some(C::Server), Some(2)), Some(true)),
                (R::Other, W::Default),
            ),
            (
                "router profile wins its own reason",
                exit(device(Some(C::Router), Some(2)), Some(true)),
                (R::Edge, W::ProfileCategory),
            ),
        ];
        for (what, f, want) in cases {
            assert_eq!(role_of(&f), want, "{what}");
        }
    }

    #[test]
    fn only_routing_sources_count_as_routing_evidence() {
        let routing: Vec<LinkSource> = LinkSource::ALL
            .into_iter()
            .filter(|s| is_routing_evidence(*s))
            .collect();
        assert_eq!(
            routing,
            [LinkSource::Ospf, LinkSource::Route, LinkSource::Bgp]
        );
    }

    #[test]
    fn roles_are_applied_and_the_access_point_mark_follows_them() {
        let f = fx();
        let mut l = run(&f, Some(2), &[link(1, 120, 121, LinkSource::Lldp)], None);
        assert!(
            l.nodes
                .iter()
                .all(|n| !n.access_point && n.role == MapRole::Other && n.subnet_count.is_none()),
            "unmarked by default"
        );
        let given: HashMap<Uuid, RoleFacts> = [
            (id(120), device(Some(ProfileCategory::L3Switch), Some(4))),
            (id(121), facts(NodeKind::WirelessAp)),
            (id(999), facts(NodeKind::WirelessAp)),
        ]
        .into_iter()
        .collect();
        apply_roles(&mut l, &given);
        let sw = node(&l, 120);
        assert_eq!(
            (sw.role, sw.role_reason),
            (MapRole::L3Switch, MapRoleReason::Subnets)
        );
        assert_eq!(sw.subnet_count, Some(4));
        assert!(!sw.access_point);
        let ap = node(&l, 121);
        assert_eq!(ap.role, MapRole::AccessPoint);
        assert!(ap.access_point);
        assert_eq!(ap.subnet_count, None);
    }

    fn token<T: serde::Serialize>(v: &T) -> String {
        serde_json::to_value(v)
            .expect("serialize")
            .as_str()
            .expect("a string")
            .to_owned()
    }

    #[test]
    fn roles_serialize_as_the_tokens_the_webui_lists() {
        let roles = [
            MapRole::Edge,
            MapRole::L3Switch,
            MapRole::L2Switch,
            MapRole::AccessPoint,
            MapRole::Other,
        ];
        let got: Vec<String> = roles.iter().map(token).collect();
        assert_eq!(
            got,
            ["edge", "l3_switch", "l2_switch", "access_point", "other"]
        );
        let reasons = [
            MapRoleReason::WirelessAp,
            MapRoleReason::MerakiProduct,
            MapRoleReason::ProfileCategory,
            MapRoleReason::DefaultRoute,
            MapRoleReason::RoutingAdjacency,
            MapRoleReason::Subnets,
            MapRoleReason::Default,
        ];
        let got: Vec<String> = reasons.iter().map(token).collect();
        assert_eq!(
            got,
            [
                "wireless_ap",
                "meraki_product",
                "profile_category",
                "default_route",
                "routing_adjacency",
                "subnets",
                "default"
            ]
        );
    }
}
