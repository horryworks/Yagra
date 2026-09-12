// SPDX-License-Identifier: AGPL-3.0-only
//! Hierarchical node groups (the inventory folder tree).
//!
//! A group has a [`GroupType`] (rendered with its own icon in the UI) and an optional parent,
//! forming a tree. Nodes reference a group via `nodes.group_id` (see [`crate::repo`]). This
//! module owns group CRUD; node↔group assignment lives on [`crate::repo::NodeRepo`].
//!
//! **Delete is non-destructive to nodes:** [`GroupRepo::delete`] re-parents a group's direct
//! child groups and member nodes up to the group's own parent (NULL ⇒ root) in one transaction,
//! then removes the row. Re-parenting a group guards against cycles via [`would_create_cycle`].

use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use uuid::Uuid;

/// Longest ancestor chain any group walk will follow before giving up.
///
/// `node_groups.parent_id` is a self-FK with no cycle constraint — [`would_create_cycle`] guards
/// the three endpoints that can set a parent, but `GroupRepo::delete`'s re-parenting does not
/// re-check — so every upward walk is bounded by this *and* a visited set. A real folder tree is
/// nowhere near this deep. Lives here rather than beside either caller because the bound is a
/// property of the group tree, not of what is being inherited along it.
pub const MAX_GROUP_DEPTH: usize = 64;

/// The kind of a group — drives the icon and is purely organizational (not a polling concept).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupType {
    /// A physical site / location.
    Site,
    /// A geographic region (a set of sites).
    Region,
    /// A class of device (routers, switches, firewalls, …).
    DeviceType,
    /// A logical service the nodes deliver.
    Service,
    /// A generic folder with no special meaning.
    Generic,
}

impl GroupType {
    /// Every group type, for the type picker.
    pub const ALL: [GroupType; 5] = [
        GroupType::Site,
        GroupType::Region,
        GroupType::DeviceType,
        GroupType::Service,
        GroupType::Generic,
    ];

    /// Stable snake_case key (matches the serde representation and the stored value).
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            GroupType::Site => "site",
            GroupType::Region => "region",
            GroupType::DeviceType => "device_type",
            GroupType::Service => "service",
            GroupType::Generic => "generic",
        }
    }

    /// Parse a stored/edge key back into a type (validation at the API edge).
    #[must_use]
    pub fn from_key(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.key() == s)
    }
}

/// Which way a "sort this folder's children by name" command orders them (ADR-130).
///
/// Two spellings and they must not drift: [`SortDirection::sql`] is the SQL keyword spliced into
/// the `ORDER BY`, and the serde tag is what the API edge parses out of the request body. A test
/// below pins both, because they are produced by different mechanisms and nothing else compares
/// them (`testing.md`, "an enum's token and its serde tag").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SortDirection {
    /// A → Z.
    Asc,
    /// Z → A.
    Desc,
}

impl SortDirection {
    /// Every direction, so the agreement test iterates rather than naming them — a third variant
    /// is then covered without anyone remembering to extend the test.
    #[cfg(test)]
    pub const ALL: [SortDirection; 2] = [SortDirection::Asc, SortDirection::Desc];

    /// The SQL keyword.
    ///
    /// 🚨 **This is the only thing that reaches the statement.** The request body's string is
    /// parsed into this enum at the API edge and then dropped; nothing operator-supplied is ever
    /// interpolated into SQL (`security.md`).
    #[must_use]
    pub const fn sql(self) -> &'static str {
        match self {
            SortDirection::Asc => "ASC",
            SortDirection::Desc => "DESC",
        }
    }
}

/// One group row returned by the API. `group_type` is the snake_case key.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct GroupSummary {
    pub id: Uuid,
    pub name: String,
    pub group_type: String,
    pub parent_id: Option<Uuid>,
    /// Manual order within the parent scope (the UI sorts siblings by this, then by name).
    pub sort_order: f64,
    /// The group's own geo coordinates, as stored (both set ⇒ drawn as a pin). A descendant
    /// folder normally leaves these null and inherits — see the `effective_*` pair below.
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// Where this group sits on the map after inheritance: its own coordinates, else the nearest
    /// ancestor's, else null. Computed on every read; never stored.
    ///
    /// **These do not add a pin.** A group is drawn on the map only when `geo_source` is `own`;
    /// for every other group this says which pin its nodes are counted at (`geo_group`).
    // Resolved by `resolve_group_geo`, which is also where the "why" lives.
    pub effective_latitude: Option<f64>,
    pub effective_longitude: Option<f64>,
    /// Whether the effective position is the group's own, inherited from an ancestor, or absent.
    pub geo_source: GeoSource,
    /// The group that supplied the effective position: this group when `geo_source` is `own`, the
    /// ancestor it inherited from when `inherited`, null when `unset`. This is the pin the group's
    /// nodes belong to, so a client never has to walk the folder tree itself.
    pub geo_group: Option<Uuid>,
    /// Poll-pool this folder assigns to its nodes (ADR-009/020, migration 0054). `null` ⇒ inherit
    /// from the nearest ancestor that sets one, else the default pool. A node's own `pool` still
    /// wins — see [`crate::poolres`].
    pub pool: Option<String>,
    /// Labels stored **on this folder** (ADR-135 inc. 2, migration 0110). Every folder and node
    /// beneath it carries them too — see `effective_tags`.
    pub tags: Vec<String>,
    /// Labels this folder refuses to inherit from its own ancestors. Shown in full, including
    /// entries naming a label nothing currently supplies: an exclusion that cannot be seen cannot
    /// be undone.
    pub tags_excluded: Vec<String>,
    /// This folder's labels **plus every ancestor's, minus its exclusions** — what it effectively
    /// carries, and therefore what everything beneath it inherits. Resolved on every read and
    /// never stored, the same call `effective_latitude`/`effective_longitude` above make and for
    /// the same reason.
    ///
    /// 🚨 **Shipped resolved on the row, following geo rather than pool.** `pool` is *not*
    /// resolved here, and the cost of that is visible: `web/src/lib/pool.ts` has to re-walk the
    /// folder tree client-side, carrying a warning that it is only safe for form previews. A
    /// client walk is also wrong for a group-scoped caller, whose breadcrumb ancestors arrive as
    /// names with their content cleared.
    ///
    /// There is deliberately no `tag_source` beside this: unlike a pin or a pool, a label has no
    /// single supplier, and the one question a screen asks — *which of these are mine* — is
    /// `effective_tags` minus `tags`, on the row, with no walk.
    pub effective_tags: Vec<String>,
    /// The IP prefixes in use at this folder (ADR-100 decision 10, migration 0104). Empty for a
    /// folder nothing has attached one to, which is every folder in a deployment with no NetBox.
    ///
    /// 🚨 **Empty also means "you may not see them".** [`crate::api::groups::visible_groups`]
    /// clears this on a row a scoped caller receives only as a breadcrumb ancestor: such a row is
    /// listed so the tree has a spine, and handing over the subnet layout of a site whose
    /// membership the caller cannot see would be a leak the folder's *name* does not constitute.
    pub prefixes: Vec<GroupPrefix>,
}

/// Who put a prefix row on a folder (ADR-131 決定 9).
///
/// This is not decoration: it decides what the editor may offer. A row a NetBox sync owns is
/// listed read-only — `PUT /node-groups/{id}/prefixes` deliberately cannot touch it — so a UI
/// that could not tell the two apart would draw a remove button that does nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrefixSource {
    /// Typed by an operator here (`netbox_server_id IS NULL`).
    Manual,
    /// Written by a NetBox sync, and swept by it when NetBox stops mentioning it.
    Sync,
}

/// One IP prefix attached to a folder.
///
/// Three fields, and the third was added under the rule the original two were chosen by
/// (ADR-131 決定 9). NetBox's prefix rows also carry `status`, `vrf`, `is_pool`, `role` and a
/// tenant, and none of them has a reader here — the bar for a field is a real reader, not
/// availability. `source` cleared that bar when two appeared at once: the range editor must not
/// offer to delete a row it cannot delete, and the folder detail pane says where a range came
/// from. It costs one column on a SELECT `attach_prefixes` already runs.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct GroupPrefix {
    /// Canonical CIDR, e.g. `"192.168.1.0/24"`. PostgreSQL's `cidr` type rendered as text, so the
    /// mask is always present — unlike `inet`, where a host address would print bare.
    pub prefix: String,
    /// NetBox's description of the range ("Matsuyama LAN"), or what the operator typed, or empty.
    pub description: String,
    /// Whether an operator typed this row or a sync wrote it.
    pub source: PrefixSource,
}

/// One "this thing's address falls inside this folder's range" hit (ADR-124, generalised by
/// ADR-131).
///
/// Already narrowed to the longest prefix that contains the address, so two rows for the same
/// key mean two *different folders* claim it equally well — the ambiguous case, which is
/// reported and never resolved automatically.
///
/// `K` is what the caller asked about: a `Uuid` when the subject is an existing node
/// ([`GroupRepo::match_prefixes`]), an `IpAddr` when it is an address that is not a node yet
/// ([`GroupRepo::match_address_prefixes`]). The two queries differ in what they join against and
/// in how far the scope reaches; everything downstream of them is identical, which is why
/// [`fold_prefix_matches`] is written once rather than twice.
#[derive(Debug, Clone)]
pub struct PrefixHit<K> {
    pub key: K,
    pub group: Uuid,
    /// The range that matched, for showing the operator *why* this folder was proposed.
    pub prefix: String,
}

/// The three answers a prefix match can give about one key.
///
/// 🚨 **Three, not two.** "No folder's range covers this" and "two folders claim it equally well"
/// both end with the caller falling back to whatever it had planned — but they are different
/// facts about the deployment, and folding them loses the one that is actionable: an ambiguous
/// address means two folders have overlapping ranges configured, which is a thing to go and fix.
#[derive(Debug, Clone, Default)]
pub struct PrefixFold<K> {
    /// Exactly one folder claims this key, with the range that did it.
    pub matched: Vec<(K, Uuid, String)>,
    /// Two or more folders claim it at the same prefix length. Never resolved here.
    pub ambiguous: Vec<(K, Vec<Uuid>)>,
    /// No folder's range contains it.
    pub unmatched: Vec<K>,
}

/// Fold the flat longest-prefix hits into the three answers above.
///
/// Pure, so the part that decides *meaning* is testable without a database. The SQL decides which
/// rows come back; this decides whether one folder claims a key or two do, and that boundary is
/// where a mistake would file a device into a site nobody chose.
///
/// ⚠️ Rows must arrive **ordered by key then group**, so a duplicate folder (one folder carrying
/// two ranges that both contain the address at the same length — impossible for a canonical CIDR,
/// but not worth trusting) collapses with `dedup` before the count is read. Both queries that
/// feed this carry that `ORDER BY`; a third one owes it too.
///
/// ⚠️ A key the caller listed twice is answered once. A key the query dropped — because the scope
/// refused it, or because nothing matched — is `unmatched`, and those two are deliberately
/// indistinguishable here: fail-closed, and the caller reports both the same way.
#[must_use]
pub fn fold_prefix_matches<K>(requested: &[K], hits: Vec<PrefixHit<K>>) -> PrefixFold<K>
where
    K: Copy + Eq + std::hash::Hash,
{
    let mut by_key: HashMap<K, Vec<PrefixHit<K>>> = HashMap::new();
    for h in hits {
        by_key.entry(h.key).or_default().push(h);
    }
    let mut fold = PrefixFold {
        matched: Vec::new(),
        ambiguous: Vec::new(),
        unmatched: Vec::new(),
    };
    let mut seen = HashSet::new();
    for key in requested {
        if !seen.insert(*key) {
            continue;
        }
        let Some(rows) = by_key.remove(key) else {
            fold.unmatched.push(*key);
            continue;
        };
        let mut groups: Vec<Uuid> = rows.iter().map(|h| h.group).collect();
        groups.dedup();
        if groups.len() == 1 {
            fold.matched
                .push((*key, rows[0].group, rows[0].prefix.clone()));
        } else {
            fold.ambiguous.push((*key, groups));
        }
    }
    fold
}

/// A fractional sort_order that places an item between `prev` and `next` — the order values of
/// its new neighbours in the destination scope (either side absent at an edge). Midpoint inserts
/// keep reordering to a single-row update; values are seeded with integer spacing (migration
/// 0015) so a long run of midpoints stays well within `f64` precision. Pure for unit tests.
#[must_use]
pub fn order_between(prev: Option<f64>, next: Option<f64>) -> f64 {
    match (prev, next) {
        (Some(p), Some(n)) => (p + n) / 2.0,
        (Some(p), None) => p + 1.0,
        (None, Some(n)) => n - 1.0,
        (None, None) => 0.0,
    }
}

/// The new sort_order for an item dropped into `siblings` — the destination scope's current
/// items, ordered ascending and **not** including the moving item. `before`/`after` name the
/// drop target (at most one is set); if neither matches a sibling the item is appended after the
/// last one. Pure so the placement maths is unit-tested without a database.
#[must_use]
pub fn placement_order(siblings: &[(Uuid, f64)], before: Option<Uuid>, after: Option<Uuid>) -> f64 {
    let pos = |id: Uuid| siblings.iter().position(|(s, _)| *s == id);
    if let Some(i) = before.and_then(pos) {
        let prev = i.checked_sub(1).map(|j| siblings[j].1);
        order_between(prev, Some(siblings[i].1))
    } else if let Some(i) = after.and_then(pos) {
        let next = siblings.get(i + 1).map(|(_, o)| *o);
        order_between(Some(siblings[i].1), next)
    } else {
        order_between(siblings.last().map(|(_, o)| *o), None)
    }
}

/// Whether re-parenting `moving` under `new_parent` would create a cycle, given the current
/// `(id, parent_id)` edges. A group cannot become its own ancestor (or its own parent). Pure so
/// it can be unit-tested without a database; the API calls it before persisting a move.
#[must_use]
pub fn would_create_cycle(
    edges: &[(Uuid, Option<Uuid>)],
    moving: Uuid,
    new_parent: Option<Uuid>,
) -> bool {
    let parent_of = |id: Uuid| edges.iter().find(|(e, _)| *e == id).and_then(|(_, p)| *p);
    let mut cur = new_parent;
    // Bound the walk by the edge count so malformed (already-cyclic) data can't loop forever.
    for _ in 0..=edges.len() {
        match cur {
            None => return false,
            Some(p) if p == moving => return true,
            Some(p) => cur = parent_of(p),
        }
    }
    true
}

/// The `root` group plus every group beneath it, via BFS over `(id, parent_id)` edges (the shape
/// [`GroupRepo::edges`] returns). Always includes `root`; a visited set bounds the walk so cyclic
/// or malformed data can't loop forever. Pure, so the subtree maths is unit-tested without a DB.
/// Used to resolve a Troubleshoot "group" scope to the group + all its descendant subgroups.
#[must_use]
pub fn group_subtree(edges: &[(Uuid, Option<Uuid>)], root: Uuid) -> Vec<Uuid> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut queue = VecDeque::new();
    queue.push_back(root);
    seen.insert(root);
    while let Some(cur) = queue.pop_front() {
        out.push(cur);
        for (id, parent) in edges {
            if *parent == Some(cur) && seen.insert(*id) {
                queue.push_back(*id);
            }
        }
    }
    out
}

/// The chain of groups **above** `start`, nearest parent first, over the same `(id, parent_id)`
/// edges [`group_subtree`] walks. Excludes `start` itself. A visited set bounds the walk so cyclic
/// or malformed data can't loop forever.
///
/// This is what keeps a group-scoped inventory tree from rendering as orphans: the WebUI builds the
/// forest from `parent_id`, so handing it a scoped subtree without the ancestors leaves every
/// visible root pointing at a parent that is not in the response. The ancestors are breadcrumb
/// only — they carry no membership, and being able to *name* the group above yours is not the same
/// as being able to see what is in it.
#[must_use]
pub fn group_ancestors(edges: &[(Uuid, Option<Uuid>)], start: Uuid) -> Vec<Uuid> {
    let parent_of = |id: Uuid| edges.iter().find(|(e, _)| *e == id).and_then(|(_, p)| *p);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    seen.insert(start);
    let mut cur = parent_of(start);
    while let Some(p) = cur {
        if !seen.insert(p) {
            break;
        }
        out.push(p);
        cur = parent_of(p);
    }
    out
}

/// For every group, the nearest group at-or-above it that carries a value — the shared engine
/// behind *both* inheritable folder attributes (poll pool, ADR-009/020; map coordinates).
///
/// `rows` are `(id, parent_id, own value)`; the result maps a group to the value it resolves to
/// **and the group that supplied it** (itself, when it carries its own). A group whose whole chain
/// is unset is absent from the map.
///
/// One upward walk per group with **path compression** — the answer is written back to every group
/// on the chain — so a resolved chain is walked once, not once per descendant. (Chains that resolve
/// to nothing aren't memoized, so an all-unset forest costs O(groups × depth); with hundreds of rows
/// and [`MAX_GROUP_DEPTH`] that is still trivial, and it keeps the map's meaning simple.) A cycle or
/// an over-deep chain resolves to "nothing inherited" and warns, rather than hanging.
///
/// **One walk, not one per attribute.** The cycle guard, the depth bound and the compression are
/// the parts that are easy to get subtly wrong, and a second copy would be the one that drifts —
/// so callers supply the payload and the fallback rule, never the traversal.
pub fn resolve_nearest_ancestor<T: Clone>(
    rows: impl IntoIterator<Item = (Uuid, Option<Uuid>, Option<T>)>,
) -> HashMap<Uuid, (T, Uuid)> {
    let own: HashMap<Uuid, (Option<Uuid>, Option<T>)> = rows
        .into_iter()
        .map(|(id, parent, value)| (id, (parent, value)))
        .collect();

    let mut resolved: HashMap<Uuid, (T, Uuid)> = HashMap::new();
    for &start in own.keys() {
        if resolved.contains_key(&start) {
            continue; // already answered as part of an earlier group's chain
        }
        // Walk up to the first group with a value (or a memoized answer), recording the path.
        let mut chain: Vec<Uuid> = Vec::new();
        let mut cur = Some(start);
        let mut answer: Option<(T, Uuid)> = None;
        let mut depth = 0usize;
        while let Some(id) = cur {
            if let Some(found) = resolved.get(&id) {
                answer = Some(found.clone());
                break;
            }
            if chain.contains(&id) || depth > MAX_GROUP_DEPTH {
                tracing::warn!(
                    group = %id,
                    "node group ancestry is cyclic or deeper than the supported bound — \
                     treating it as having nothing to inherit"
                );
                break;
            }
            let Some((parent, value)) = own.get(&id) else {
                break; // dangling parent_id: nothing more to inherit from
            };
            // Recorded before the value check so the supplying group is memoized too, not just
            // the descendants that inherit from it.
            chain.push(id);
            if let Some(v) = value {
                answer = Some((v.clone(), id));
                break;
            }
            cur = *parent;
            depth += 1;
        }
        // Path compression: every group we walked through shares the answer.
        if let Some(found) = answer {
            for id in chain {
                resolved.insert(id, found.clone());
            }
        }
    }
    resolved
}

/// One folder's label row as every reader of the folder tree sees it:
/// `(id, parent_id, labels added here, labels refused here)`.
///
/// An alias rather than the tuple spelled out, because it crosses four boundaries —
/// [`GroupRepo::tag_rows`], the `AlertConfigSources` seam, [`crate::tagres::TagResolver::build`]
/// and [`accumulate_ancestor_labels`] — and a four-element tuple written four times is four places
/// to get the order wrong with no compiler help (the last two elements are the same type).
pub type LabelRow = (Uuid, Option<Uuid>, Vec<String>, Vec<String>);

/// For every group, the labels it and **every** ancestor supply, minus the ones each level
/// excludes — the accumulating twin of [`resolve_nearest_ancestor`] (ADR-135 inc. 2).
///
/// `rows` are `(id, parent_id, labels added here, labels refused here)`. The result maps every
/// group to the set it effectively carries, including groups whose whole chain is empty.
///
/// 🚨 **A second function rather than a flag on the first, and the reason is structural.**
/// [`resolve_nearest_ancestor`] stops walking at the first group carrying a value, and that stop
/// *is* "nearest wins" — there is no argument that makes one function also accumulate. What must
/// not be written twice is the part that is easy to get subtly wrong, so the cycle guard, the
/// [`MAX_GROUP_DEPTH`] bound and the memoization are written the same way here, and
/// `the_two_resolvers_disagree_about_a_farther_ancestor` runs both over one forest so the reason
/// two exist is executable rather than asserted in this comment.
///
/// **The fold at each level is `(what came from above − excluded here) ∪ added here`** — remove
/// first, add second. That ordering is what makes "exclude a label and also set it here" mean the
/// obvious thing instead of being a contradiction, and it is why a group cannot exclude its own
/// label (it would be re-added immediately; the way to drop one is to stop adding it).
///
/// Memoization is by **unwind** rather than path compression: every group on a chain resolves to a
/// *different* set, so the walk goes up to the first memoized ancestor and then fills back down,
/// which is one walk amortized per group. Every group is inserted **including the empty set** —
/// an all-empty forest is the state every deployment starts in, and skipping empties there would
/// re-walk every chain (the cost `resolve_nearest_ancestor`'s doc admits to).
///
/// 🚨 **A cycle or an over-deep chain keeps what was collected below it** and warns, rather than
/// degrading to "nothing inherited" the way pool resolution does. Deliberate: a label decides
/// which threshold rules and which maintenance windows apply to a node, so silently dropping a
/// site's labels because somebody made a loop three levels up is the *narrowing* failure ADR-080
/// names — while inventing labels nobody set would be the widening one. Keeping exactly what the
/// bounded walk saw does neither.
pub fn accumulate_ancestor_labels(
    rows: impl IntoIterator<Item = LabelRow>,
) -> HashMap<Uuid, BTreeSet<String>> {
    /// One row, keyed out of its id: the parent to walk to, and the two lists to fold.
    type Own = (Option<Uuid>, Vec<String>, Vec<String>);
    let own: HashMap<Uuid, Own> = rows
        .into_iter()
        .map(|(id, parent, add, remove)| (id, (parent, add, remove)))
        .collect();

    let mut resolved: HashMap<Uuid, BTreeSet<String>> = HashMap::new();
    for &start in own.keys() {
        if resolved.contains_key(&start) {
            continue; // already answered while unwinding an earlier group's chain
        }
        // Walk up, recording the path, until a memoized ancestor or the top.
        let mut chain: Vec<Uuid> = Vec::new();
        let mut seen: HashSet<Uuid> = HashSet::new();
        let mut acc: BTreeSet<String> = BTreeSet::new();
        let mut cur = Some(start);
        let mut depth = 0usize;
        while let Some(id) = cur {
            if let Some(found) = resolved.get(&id) {
                acc = found.clone();
                break;
            }
            if !seen.insert(id) || depth > MAX_GROUP_DEPTH {
                tracing::warn!(
                    group = %id,
                    "node group ancestry is cyclic or deeper than the supported bound — \
                     resolving labels from the part of the chain already walked"
                );
                break;
            }
            let Some((parent, _, _)) = own.get(&id) else {
                break; // dangling parent_id: nothing more to inherit from
            };
            chain.push(id);
            cur = *parent;
            depth += 1;
        }
        // Shallowest first, so each group sees everything above it before its own turn.
        for id in chain.into_iter().rev() {
            if let Some((_, add, remove)) = own.get(&id) {
                for r in remove {
                    acc.remove(r);
                }
                acc.extend(add.iter().cloned());
            }
            resolved.insert(id, acc.clone());
        }
    }
    resolved
}

/// Where a group's effective map position came from.
// The geo twin of `crate::poolres::PoolSource`, minus a node level (nodes have no coordinates)
// and minus a default (there is no implicit place on Earth).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GeoSource {
    /// The group carries its own coordinates and is drawn as a pin.
    Own,
    /// Inherited from the nearest ancestor that carries coordinates, named by `geo_group`. The
    /// group is not drawn as its own pin; its nodes are counted at that ancestor's.
    Inherited,
    /// Neither this group nor any ancestor is placed — it is not on the map.
    Unset,
}

/// Fill every row's effective coordinates from the nearest ancestor that carries them.
///
/// **This is the whole of "geo inheritance", and it deliberately does not create pins.** A pin is
/// drawn for a group that carries its *own* coordinates; inheritance means a descendant resolves
/// *to* that pin, so its nodes are counted there. Placing a pin per inheriting group would draw
/// thirty exactly-overlapping pins for thirty racks in one building and hide the site behind them.
///
/// The map therefore has the same number of pins before and after this runs — what changes is what
/// each pin counts, from "the folder's direct members" to "everything that resolves here". That is
/// the substance: a site pin whose nodes all live in rack sub-folders showed nothing at all before.
///
/// Pure (the resolution rule is unit-tested without a database) and resolved on read rather than
/// materialized, for the reason threshold and pool inheritance are (ADR-013): a stored copy goes
/// stale the moment a parent is edited or a folder is moved.
pub fn resolve_group_geo(groups: &mut [GroupSummary]) {
    let resolved = resolve_nearest_ancestor(
        groups
            .iter()
            .map(|g| (g.id, g.parent_id, coords_of(g.latitude, g.longitude))),
    );
    for g in groups.iter_mut() {
        match resolved.get(&g.id) {
            Some(((lat, lon), from)) => {
                g.effective_latitude = Some(*lat);
                g.effective_longitude = Some(*lon);
                g.geo_group = Some(*from);
                g.geo_source = if *from == g.id {
                    GeoSource::Own
                } else {
                    GeoSource::Inherited
                };
            }
            None => {
                g.effective_latitude = None;
                g.effective_longitude = None;
                g.geo_group = None;
                g.geo_source = GeoSource::Unset;
            }
        }
    }
}

/// Fill every row's `effective_tags` from its own labels plus every ancestor's (ADR-135 inc. 2).
///
/// The label twin of [`resolve_group_geo`], and it follows that one rather than `pool` on purpose:
/// resolving here means every client gets the answer on the row and none of them has to walk the
/// folder tree. `pool` did not, and `web/src/lib/pool.ts` is the cost — a second implementation of
/// the inheritance rule, in another language, that is only safe for form previews.
///
/// Pure, and resolved on read rather than materialized, for the same reason: a stored copy goes
/// stale the moment a parent is edited or a folder is moved.
pub fn resolve_group_tags(groups: &mut [GroupSummary]) {
    let resolved = accumulate_ancestor_labels(
        groups
            .iter()
            .map(|g| (g.id, g.parent_id, g.tags.clone(), g.tags_excluded.clone())),
    );
    for g in groups.iter_mut() {
        g.effective_tags = resolved
            .get(&g.id)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default();
    }
}

/// A placement is both coordinates or neither — a row with only one is unplaced, not half-placed.
/// The write path (`PUT /node-groups/{id}/geo`) sets and clears them together, so a lone value is
/// legacy or hand-edited data; treating it as placed would put a pin on the prime meridian.
fn coords_of(lat: Option<f64>, lon: Option<f64>) -> Option<(f64, f64)> {
    match (lat, lon) {
        // NaN/±inf would project to nowhere and poison the fit-to-view bounds for every other pin.
        (Some(la), Some(lo)) if la.is_finite() && lo.is_finite() => Some((la, lo)),
        _ => None,
    }
}

/// PostgreSQL-backed group store.
pub struct GroupRepo {
    pool: PgPool,
}

impl GroupRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// All groups (the UI builds the tree from the flat list). Ordered by the manual sort_order
    /// within each parent scope, then name — the same order the tree renders.
    ///
    /// Geo inheritance is resolved here rather than by the caller, so there is exactly one place
    /// that answers "where is this folder on the map" — see [`resolve_group_geo`]. It needs the
    /// whole table, which is precisely what this query already returns.
    pub async fn list(&self) -> anyhow::Result<Vec<GroupSummary>> {
        let rows = sqlx::query(
            "SELECT id, name, group_type, parent_id, sort_order, latitude, longitude, pool, \
                    tags, tags_excluded \
             FROM node_groups ORDER BY sort_order, name, id",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut groups: Vec<GroupSummary> = rows
            .into_iter()
            .map(|row| {
                Ok(GroupSummary {
                    id: row.try_get("id")?,
                    name: row.try_get("name")?,
                    group_type: row.try_get("group_type")?,
                    parent_id: row.try_get("parent_id")?,
                    sort_order: row.try_get("sort_order")?,
                    latitude: row.try_get("latitude")?,
                    longitude: row.try_get("longitude")?,
                    // Overwritten wholesale by `resolve_group_geo` below; the row carries no
                    // stored answer for these.
                    effective_latitude: None,
                    effective_longitude: None,
                    geo_source: GeoSource::Unset,
                    geo_group: None,
                    pool: row.try_get("pool")?,
                    tags: row.try_get("tags")?,
                    tags_excluded: row.try_get("tags_excluded")?,
                    // Overwritten wholesale by `resolve_group_tags` below, like the geo pair.
                    effective_tags: Vec::new(),
                    // Filled from the second query below: one round trip for the whole tree
                    // rather than a lateral join, because most deployments have no rows here at
                    // all and the empty answer is then a single index-less scan of nothing.
                    prefixes: Vec::new(),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        resolve_group_geo(&mut groups);
        resolve_group_tags(&mut groups);
        self.attach_prefixes(&mut groups).await?;
        Ok(groups)
    }

    /// Fold `node_group_prefixes` into an already-built group list.
    ///
    /// ⚠️ `prefix::TEXT` — the column is `cidr`, and sqlx has no mapping for it without the
    /// `ipnetwork` feature. Casting in the query keeps that feature (and a crate) out of the
    /// build, and for `cidr` the text form is exactly what was stored: the mask is always
    /// rendered, so `192.168.1.0/24` round-trips. (`inet` would **add** a `/32` to a host
    /// address, which is the trap `dns_check.rs` records.)
    async fn attach_prefixes(&self, groups: &mut [GroupSummary]) -> anyhow::Result<()> {
        let rows = sqlx::query(
            "SELECT group_id, prefix::TEXT AS prefix, description, \
                    netbox_server_id IS NULL AS manual \
             FROM node_group_prefixes ORDER BY prefix",
        )
        .fetch_all(&self.pool)
        .await?;
        if rows.is_empty() {
            return Ok(());
        }
        let mut by_group: std::collections::HashMap<Uuid, Vec<GroupPrefix>> =
            std::collections::HashMap::new();
        for row in rows {
            let group_id: Uuid = row.try_get("group_id")?;
            // `IS NULL` is never itself NULL, so this is a plain bool rather than an Option.
            let manual: bool = row.try_get("manual")?;
            by_group.entry(group_id).or_default().push(GroupPrefix {
                prefix: row.try_get("prefix")?,
                description: row.try_get("description")?,
                source: if manual {
                    PrefixSource::Manual
                } else {
                    PrefixSource::Sync
                },
            });
        }
        for g in groups.iter_mut() {
            if let Some(v) = by_group.remove(&g.id) {
                g.prefixes = v;
            }
        }
        Ok(())
    }

    /// Which folder's IP range each of these nodes falls inside, narrowed to the **longest**
    /// prefix that contains the address (ADR-124 決定 5). A node with no hit is simply absent
    /// from the result; a node with two hits of the same length is **ambiguous** and appears
    /// twice, because choosing between two sites on the caller's behalf is exactly the decision
    /// this feature refuses to make.
    ///
    /// 🚨 **The containment test lives here, in PostgreSQL, and must not move into Rust.** Two
    /// reasons, and the second is the one that bites: there is no CIDR parser in this workspace
    /// (migration 0104 chose the `cidr` column type precisely so the *write* is the validation),
    /// and [`crate::api::groups::visible_groups`] **clears `prefixes` on breadcrumb ancestors** —
    /// so a client computing this from the group list it was served would silently miss every
    /// range it was allowed to match against but not to read. `<<=` is the containment operator;
    /// an IPv4 address against an IPv6 prefix is simply `false`, never an error.
    ///
    /// `scope` is the caller's visible group ids (`None` ⇒ unrestricted), and it narrows
    /// **both sides**: the candidate folders, and the nodes themselves. Without the first, the
    /// reply would describe the subnet layout of sites the caller may not see — the leak
    /// `visible_groups` exists to prevent. Without the second, naming a node id would answer a
    /// question about a node the caller cannot read.
    ///
    /// ⚠️ A node the scope refuses is **absent from the result, exactly like one that matched
    /// nothing**. The caller reports it as unmatched. That is fail-closed and it is also the
    /// honest limit of this shape: the UI can only name nodes the tree already showed, so the
    /// case is unreachable from the product and unmeasured anywhere else.
    pub async fn match_prefixes(
        &self,
        nodes: &[Uuid],
        scope: Option<&[Uuid]>,
    ) -> anyhow::Result<Vec<PrefixHit<Uuid>>> {
        if nodes.is_empty() {
            return Ok(Vec::new());
        }
        let scope_bind: Option<Vec<Uuid>> = scope.map(<[Uuid]>::to_vec);
        let rows = sqlx::query(
            "SELECT n.id AS node_id, p.group_id AS group_id, p.prefix::TEXT AS prefix \
             FROM nodes n \
             JOIN node_group_prefixes p ON n.address <<= p.prefix \
             WHERE n.id = ANY($1) \
               AND ($2::uuid[] IS NULL OR n.group_id = ANY($2)) \
               AND ($2::uuid[] IS NULL OR p.group_id = ANY($2)) \
               AND masklen(p.prefix) = ( \
                     SELECT MAX(masklen(q.prefix)) FROM node_group_prefixes q \
                     WHERE n.address <<= q.prefix \
                       AND ($2::uuid[] IS NULL OR q.group_id = ANY($2))) \
             ORDER BY n.id, p.group_id",
        )
        .bind(nodes)
        .bind(&scope_bind)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(PrefixHit {
                    key: row.try_get::<Uuid, _>("node_id")?,
                    group: row.try_get("group_id")?,
                    prefix: row.try_get("prefix")?,
                })
            })
            .collect()
    }

    /// The same question as [`GroupRepo::match_prefixes`], asked about **addresses that are not
    /// nodes yet** — the candidates a discovery sweep just found (ADR-131 決定 4).
    ///
    /// 🚨 **`$1` is `text[]`, not `inet[]`.** sqlx has no `inet` mapping without the `ipnetwork`
    /// feature, which this module keeps out of the build on purpose (see `attach_prefixes`). The
    /// cast happens in SQL, exactly as `NodeRepo::import_nodes` binds `$3::inet`.
    /// ⚠️ **Only ever hand this parsed addresses.** The caller takes `IpAddr`, so raw request text
    /// cannot reach the cast — a value that is not an address would fail the *statement*, which is
    /// an internal error rather than the named 400 the edge already produces.
    ///
    /// 🚨 **`host(addr)`, never `addr::TEXT`.** Casting an `inet` to text **adds the mask**
    /// (`10.0.0.1` becomes `10.0.0.1/32`), which is the defect `arp.rs` shipped for a release.
    /// `host()` renders the bare address, and the caller parses it back into an `IpAddr` before
    /// using it as a key — so no text-canonicalisation difference (`2001:DB8::1` against
    /// `2001:db8::1`) can split one address into two answers.
    ///
    /// ⚠️ **The scope narrows the folder side only, and that is the difference from the node
    /// version.** There, both sides narrow, because naming a node id asks a question about a row
    /// that already exists and the caller may not be allowed to read. Here there is no such row:
    /// the addresses are ones the caller's own sweep just found and already holds, so there is
    /// nothing to withhold about them. The folders still narrow, for the reason
    /// [`crate::api::groups::visible_groups`] clears prefixes — answering with a folder the caller
    /// cannot see hands over the subnet layout of a site whose membership they were refused.
    /// A future reader "fixing" the asymmetry would be adding a filter to data the client supplied.
    ///
    /// `DISTINCT` in the CTE so fifty candidates in one /24 do not re-run the `MAX(masklen)`
    /// subquery fifty times; [`fold_prefix_matches`] still answers per requested address.
    pub async fn match_address_prefixes(
        &self,
        addresses: &[IpAddr],
        scope: Option<&[Uuid]>,
    ) -> anyhow::Result<Vec<PrefixHit<IpAddr>>> {
        if addresses.is_empty() {
            return Ok(Vec::new());
        }
        let text: Vec<String> = addresses.iter().map(ToString::to_string).collect();
        let scope_bind: Option<Vec<Uuid>> = scope.map(<[Uuid]>::to_vec);
        let rows = sqlx::query(
            "WITH addrs AS (SELECT DISTINCT a.txt::inet AS addr FROM unnest($1::text[]) AS a(txt)) \
             SELECT host(addrs.addr) AS address, p.group_id AS group_id, \
                    p.prefix::TEXT AS prefix \
             FROM addrs \
             JOIN node_group_prefixes p ON addrs.addr <<= p.prefix \
             WHERE ($2::uuid[] IS NULL OR p.group_id = ANY($2)) \
               AND masklen(p.prefix) = ( \
                     SELECT MAX(masklen(q.prefix)) FROM node_group_prefixes q \
                     WHERE addrs.addr <<= q.prefix \
                       AND ($2::uuid[] IS NULL OR q.group_id = ANY($2))) \
             ORDER BY addrs.addr, p.group_id",
        )
        .bind(&text)
        .bind(&scope_bind)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let rendered: String = row.try_get("address")?;
                let key: IpAddr = rendered.parse().map_err(|_| {
                    // Unreachable: `host()` of an `inet` PostgreSQL accepted is always parseable.
                    // Named rather than unwrapped so a future projection change fails loudly.
                    anyhow::anyhow!("prefix match returned an unparseable address")
                })?;
                Ok(PrefixHit {
                    key,
                    group: row.try_get("group_id")?,
                    prefix: row.try_get("prefix")?,
                })
            })
            .collect()
    }

    /// Whether any folder the caller may see carries an IP range at all.
    ///
    /// Exists so "nothing matched" can be told apart from "there was nothing to match against"
    /// (ADR-124 決定 6). Folding the two into one message is the shape that ships an inert
    /// feature looking like a working one: a deployment with no NetBox would report every node as
    /// unmatched and give the operator no way to learn that the answer was never possible.
    pub async fn any_prefixes(&self, scope: Option<&[Uuid]>) -> anyhow::Result<bool> {
        let scope_bind: Option<Vec<Uuid>> = scope.map(<[Uuid]>::to_vec);
        let found: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM node_group_prefixes \
             WHERE ($1::uuid[] IS NULL OR group_id = ANY($1)))",
        )
        .bind(&scope_bind)
        .fetch_one(&self.pool)
        .await?;
        Ok(found)
    }

    /// Canonicalise one operator-typed range, or `None` when it is not an address at all.
    ///
    /// 🚨 **`network($1::inet)::cidr`, not `$1::cidr`.** A plain `cidr` cast **rejects** a value
    /// with host bits set, so `192.168.1.5/24` — which is what a person reading a device's config
    /// types — would be refused as malformed. `network()` canonicalises it to `192.168.1.0/24`.
    /// Migration 0104's header records this; the NetBox writer uses the same spelling, and so must
    /// anything else that stores one.
    ///
    /// **The database is the validator, because there is no CIDR parser in this workspace** and
    /// `web/src/lib/cidr.ts` — the only one in the repository — is IPv4-only while IPv6 is in
    /// scope. This method exists so a caller can turn PostgreSQL's refusal into a 400 naming the
    /// offending string, rather than letting a failed statement become a 500 naming nothing.
    ///
    /// ⚠️ **Call it outside a transaction, one row at a time.** A failed statement poisons its
    /// transaction, so probing inside one would abort the write it was meant to guard. And one
    /// `unnest` covering the batch cannot say *which* value failed: PostgreSQL does not promise
    /// row-evaluation order, so the only honest message would be "one of these is wrong". At
    /// form-submit pace the round trips are free; the named row is not.
    pub async fn canonical_prefix(&self, raw: &str) -> anyhow::Result<Option<String>> {
        let canonical: Result<String, _> =
            sqlx::query_scalar("SELECT network($1::inet)::cidr::TEXT")
                .bind(raw)
                .fetch_one(&self.pool)
                .await;
        match canonical {
            Ok(v) => Ok(Some(v)),
            // Any database error here means the cast refused the value. Distinguishing a genuine
            // outage would need the SQLSTATE, and the caller's fallback for both is the same 400 —
            // a real outage fails again on the very next statement of the write itself.
            Err(_) => Ok(None),
        }
    }

    /// Replace this folder's **hand-made** ranges with `rows` (ADR-131 決定 5).
    ///
    /// 🚨 **The `DELETE` is scoped to `netbox_server_id IS NULL`, and that is the whole safety
    /// property.** A plain "replace the folder's list" would let an operator delete rows a NetBox
    /// sync owns — rows the sync would then re-create on its next run, so the edit would appear to
    /// work and silently undo itself. Scoping the delete means this endpoint can only touch what
    /// it created, and `delete_stale_prefixes`' ownership model is never disturbed.
    ///
    /// 🚨 **`ON CONFLICT DO NOTHING`, never `DO UPDATE`.** Taking over a sync-owned row would
    /// clear its `netbox_server_id` and put it permanently out of the stale sweep's reach. The
    /// normal path for a collision is the caller's named 400; `DO NOTHING` is what keeps the race
    /// (a sync landing between the check and the commit) harmless instead of aborting the
    /// transaction.
    ///
    /// Every `prefix` must already have been through [`GroupRepo::canonical_prefix`] — the
    /// `network()` cast is repeated here so there is one spelling of migration 0104's rule, not so
    /// that an unvalidated value may be passed.
    pub async fn set_manual_prefixes(
        &self,
        group: Uuid,
        rows: &[(String, String)],
    ) -> anyhow::Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "DELETE FROM node_group_prefixes WHERE group_id = $1 AND netbox_server_id IS NULL",
        )
        .bind(group)
        .execute(&mut *tx)
        .await?;
        for (prefix, description) in rows {
            sqlx::query(
                "INSERT INTO node_group_prefixes \
                   (group_id, prefix, description, netbox_server_id) \
                 VALUES ($1, network($2::inet)::cidr, $3, NULL) \
                 ON CONFLICT (group_id, prefix) DO NOTHING",
            )
            .bind(group)
            .bind(prefix)
            .bind(description)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// The canonical ranges on this folder that a **sync** owns, so a write path can refuse to
    /// take one over with a 400 that names it.
    ///
    /// Refusing is more honest than silently dropping the row: the operator typed it, and nothing
    /// on screen would otherwise say it did not land.
    pub async fn sync_owned_prefixes(&self, group: Uuid) -> anyhow::Result<Vec<String>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT prefix::TEXT FROM node_group_prefixes \
             WHERE group_id = $1 AND netbox_server_id IS NOT NULL",
        )
        .bind(group)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Whether a folder with this id exists.
    ///
    /// Exists so a write path can refuse an unknown folder with a 400 that names the problem,
    /// instead of letting the foreign key turn it into a 500 that names nothing.
    pub async fn exists(&self, id: Uuid) -> anyhow::Result<bool> {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM node_groups WHERE id = $1")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        Ok(n > 0)
    }

    /// The `(id, sort_order)` of the groups directly under `parent` (NULL ⇒ top level), ordered.
    /// Feeds [`placement_order`] when a drag drops a group before/after a sibling.
    pub async fn ordered_siblings(&self, parent: Option<Uuid>) -> anyhow::Result<Vec<(Uuid, f64)>> {
        let rows = sqlx::query(
            "SELECT id, sort_order FROM node_groups \
             WHERE parent_id IS NOT DISTINCT FROM $1::uuid ORDER BY sort_order, name, id",
        )
        .bind(parent)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("id")?, row.try_get("sort_order")?)))
            .collect()
    }

    /// Re-parent a group and set its order in one update (drag reorder/nest). The caller must
    /// have rejected a cycle-inducing `parent` (see [`would_create_cycle`]). Returns existence.
    pub async fn place(&self, id: Uuid, parent: Option<Uuid>, order: f64) -> anyhow::Result<bool> {
        let res =
            sqlx::query("UPDATE node_groups SET parent_id = $2, sort_order = $3 WHERE id = $1")
                .bind(id)
                .bind(parent)
                .bind(order)
                .execute(&self.pool)
                .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The `(id, parent_id)` edges, for cycle checks before a move.
    pub async fn edges(&self) -> anyhow::Result<Vec<(Uuid, Option<Uuid>)>> {
        let rows = sqlx::query("SELECT id, parent_id FROM node_groups")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("id")?, row.try_get("parent_id")?)))
            .collect()
    }

    /// The `(id, parent_id, tags, tags_excluded)` rows, for building a
    /// [`crate::tagres::TagResolver`] — the twin of [`Self::pool_rows`], read whole for the same
    /// reason (ADR-135 inc. 2).
    pub async fn tag_rows(&self) -> anyhow::Result<Vec<LabelRow>> {
        let rows = sqlx::query("SELECT id, parent_id, tags, tags_excluded FROM node_groups")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    row.try_get("id")?,
                    row.try_get("parent_id")?,
                    row.try_get("tags")?,
                    row.try_get("tags_excluded")?,
                ))
            })
            .collect()
    }

    /// Replace a folder's own labels and the ones it refuses to inherit, as one whole value
    /// (ADR-135 inc. 2). Returns whether the folder exists.
    ///
    /// A whole-value write for the reason `set_prefixes` records: the editor is a dialog with a
    /// Save button, so a per-label `DELETE` would act the moment ✕ is clicked — before Save, and
    /// with no way back.
    pub async fn set_tags(
        &self,
        id: Uuid,
        tags: &[String],
        excluded: &[String],
    ) -> anyhow::Result<bool> {
        let res = sqlx::query("UPDATE node_groups SET tags = $2, tags_excluded = $3 WHERE id = $1")
            .bind(id)
            .bind(tags)
            .bind(excluded)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The `(id, parent_id, pool)` rows, for building a [`crate::poolres::PoolResolver`]. Read
    /// whole (the table is small) so effective-pool resolution costs one query, not one per node.
    pub async fn pool_rows(&self) -> anyhow::Result<Vec<(Uuid, Option<Uuid>, Option<String>)>> {
        let rows = sqlx::query("SELECT id, parent_id, pool FROM node_groups")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                Ok((
                    row.try_get("id")?,
                    row.try_get("parent_id")?,
                    row.try_get("pool")?,
                ))
            })
            .collect()
    }

    /// Set (or clear with `None`) just this folder's poll-pool, leaving name/type/parent alone —
    /// the inventory tree's context-menu action. Returns whether the group exists.
    pub async fn set_pool(&self, id: Uuid, pool: Option<&str>) -> anyhow::Result<bool> {
        let res = sqlx::query("UPDATE node_groups SET pool = $2 WHERE id = $1")
            .bind(id)
            .bind(pool)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Set (or clear with a `None` pair) this folder's map pin. Returns whether the group exists.
    ///
    /// The caller is responsible for the both-or-neither and range rules — half a coordinate pair
    /// is not a location. Written by `PUT /api/v1/node-groups/{id}/geo`, read back by
    /// [`Self::list`] and rendered by the dashboard's Geo map widget.
    pub async fn set_geo(
        &self,
        id: Uuid,
        latitude: Option<f64>,
        longitude: Option<f64>,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query("UPDATE node_groups SET latitude = $2, longitude = $3 WHERE id = $1")
            .bind(id)
            .bind(latitude)
            .bind(longitude)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The distinct non-empty pools folders assign. Feeds the pool picker together with
    /// [`crate::repo::NodeRepo::distinct_pools`].
    pub async fn distinct_pools(&self) -> anyhow::Result<Vec<String>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT pool FROM node_groups WHERE pool IS NOT NULL AND pool <> ''",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Create a group; returns its id. `pool` is the folder's poll-pool assignment (`None` ⇒
    /// inherit), already validated by the caller.
    pub async fn create(
        &self,
        name: &str,
        group_type: GroupType,
        parent: Option<Uuid>,
        pool: Option<&str>,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        // Append to the end of the parent scope (max sort_order + 1) so a new group lands at the
        // bottom of its siblings rather than jumping to the top (the DEFAULT 0).
        sqlx::query(
            "INSERT INTO node_groups (id, name, group_type, parent_id, sort_order, pool) VALUES \
             ($1, $2, $3, $4, \
              (SELECT COALESCE(MAX(sort_order), 0) + 1 FROM node_groups \
               WHERE parent_id IS NOT DISTINCT FROM $4::uuid), $5)",
        )
        .bind(id)
        .bind(name)
        .bind(group_type.key())
        .bind(parent)
        .bind(pool)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Rename / re-type / re-parent a group, and optionally move its poll-pool. Returns whether
    /// the group exists. The caller must have already rejected a cycle-inducing `parent` (see
    /// [`would_create_cycle`]).
    ///
    /// `pool` is three-state, matching `Repo::set_node_bindings`: outer `None` leaves the column
    /// alone, `Some(None)` clears it to NULL (inherit), `Some(Some(p))` sets it.
    pub async fn update(
        &self,
        id: Uuid,
        name: &str,
        group_type: GroupType,
        parent: Option<Uuid>,
        pool: Option<Option<&str>>,
    ) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE node_groups SET name = $2, group_type = $3, parent_id = $4, \
                    pool = CASE WHEN $5 THEN $6 ELSE pool END \
             WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(group_type.key())
        .bind(parent)
        .bind(pool.is_some())
        .bind(pool.flatten())
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Delete a group, re-parenting its direct child groups and member nodes up to the group's
    /// own parent (NULL ⇒ root) so **no node is ever deleted**. Atomic. Returns whether the
    /// group existed.
    pub async fn delete(&self, id: Uuid) -> anyhow::Result<bool> {
        let mut tx = self.pool.begin().await?;
        // Resolve the group's parent (and confirm it exists). `query_scalar` over the nullable
        // column yields Option<Option<Uuid>>: outer = row found, inner = the parent value.
        let found: Option<Option<Uuid>> =
            sqlx::query_scalar("SELECT parent_id FROM node_groups WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(parent) = found else {
            return Ok(false);
        };
        // Child groups move up to the parent.
        sqlx::query("UPDATE node_groups SET parent_id = $2 WHERE parent_id = $1")
            .bind(id)
            .bind(parent)
            .execute(&mut *tx)
            .await?;
        // Member nodes move up to the parent (never deleted).
        sqlx::query("UPDATE nodes SET group_id = $2, updated_at = now() WHERE group_id = $1")
            .bind(id)
            .bind(parent)
            .execute(&mut *tx)
            .await?;
        let res = sqlx::query("DELETE FROM node_groups WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(res.rows_affected() > 0)
    }

    /// Renumber one folder's **direct** children by name (ADR-130). Returns whether the folder
    /// existed.
    ///
    /// Two scopes, not one: subfolders are `node_groups` sharing this `parent_id`, member nodes
    /// are `nodes` sharing this `group_id`. They are separate sibling sets by construction, which
    /// is why folders can never interleave with nodes in the tree however this is called — the
    /// renderer walks the two lists in turn (`web/src/lib/nodeTree.ts`). Grandchildren are not
    /// touched: the operator right-clicked one folder and that is the scope that changes.
    ///
    /// The statements are the ones migration `0015_tree_ordering.sql` already uses to seed this
    /// column, so nothing new is invented here — `row_number()` over the sibling scope. Two
    /// consequences worth knowing: the values land as **integers from 1**, which re-spaces a scope
    /// whose fractions have been squeezed by a long run of midpoint drags; and `lower(name)` is
    /// what makes `SW-01` and `sw-02` fall where a person expects, with `id` last so the same
    /// input always produces the same output.
    ///
    /// 🚨 **This overwrites a hand-arranged order and there is no undo** (ADR-130 decision 5:
    /// no confirmation dialog — the caller named the folder by right-clicking it).
    ///
    /// ⚠️ `dir.sql()` is a `&'static str` from an enum parsed at the API edge. Nothing an operator
    /// typed reaches the statement (`security.md`).
    pub async fn sort_children(&self, id: Uuid, dir: SortDirection) -> anyhow::Result<bool> {
        let mut tx = self.pool.begin().await?;
        // Confirm the folder exists and hold it for the length of the transaction, so a concurrent
        // delete cannot leave this renumbering half-applied against a folder that has gone.
        let found: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM node_groups WHERE id = $1 FOR UPDATE")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        if found.is_none() {
            return Ok(false);
        }
        let keyword = dir.sql();
        sqlx::query(&format!(
            "UPDATE node_groups g SET sort_order = s.rn FROM ( \
               SELECT id, row_number() OVER (ORDER BY lower(name) {keyword}, id) AS rn \
               FROM node_groups WHERE parent_id = $1 \
             ) s WHERE g.id = s.id"
        ))
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(&format!(
            "UPDATE nodes n SET sort_order = s.rn, updated_at = now() FROM ( \
               SELECT id, row_number() OVER (ORDER BY lower(name) {keyword}, id) AS rn \
               FROM nodes WHERE group_id = $1 \
             ) s WHERE n.id = s.id"
        ))
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_direction_token_and_serde_agree() {
        // Two spellings of one value, produced by different mechanisms: `sql()` is a hand-written
        // match and the JSON tag comes from `rename_all`. Nothing but this test compares them, and
        // a disagreement would mean the API edge accepts a word the statement never sees
        // (`testing.md`, "an enum's token and its serde tag").
        for d in SortDirection::ALL {
            let json = serde_json::to_string(&d).expect("serialize");
            let back: SortDirection = serde_json::from_str(&json).expect("round trip");
            assert_eq!(back, d);
            assert_eq!(json.to_uppercase(), format!("\"{}\"", d.sql()));
        }
        // Pinned as literals too: the assertion above would pass if both sides were renamed
        // together, and `ASC`/`DESC` are SQL keywords that may not be renamed at all.
        assert_eq!(SortDirection::Asc.sql(), "ASC");
        assert_eq!(SortDirection::Desc.sql(), "DESC");
        assert_eq!(
            serde_json::to_string(&SortDirection::Asc).unwrap(),
            "\"asc\""
        );
        assert_eq!(
            serde_json::to_string(&SortDirection::Desc).unwrap(),
            "\"desc\""
        );
    }

    #[test]
    fn group_type_keys_round_trip() {
        for t in GroupType::ALL {
            assert_eq!(GroupType::from_key(t.key()), Some(t));
        }
        assert_eq!(GroupType::from_key("nope"), None);
        // Keys agree with the serde wire form.
        assert_eq!(
            serde_json::to_string(&GroupType::DeviceType).unwrap(),
            "\"device_type\""
        );
    }

    /// A group row with only the fields geo resolution reads.
    fn geo_row(id: u128, parent: Option<u128>, coords: Option<(f64, f64)>) -> GroupSummary {
        GroupSummary {
            id: Uuid::from_u128(id),
            name: format!("g{id}"),
            group_type: GroupType::Generic.key().to_owned(),
            parent_id: parent.map(Uuid::from_u128),
            sort_order: 0.0,
            latitude: coords.map(|c| c.0),
            longitude: coords.map(|c| c.1),
            effective_latitude: None,
            effective_longitude: None,
            geo_source: GeoSource::Unset,
            geo_group: None,
            pool: None,
            tags: Vec::new(),
            tags_excluded: Vec::new(),
            effective_tags: Vec::new(),
            prefixes: Vec::new(),
        }
    }

    /// `(effective lat, effective lon, source, supplying group)` for one row, for terse asserts.
    fn geo_of(
        groups: &[GroupSummary],
        id: u128,
    ) -> (Option<f64>, Option<f64>, GeoSource, Option<u128>) {
        let g = groups
            .iter()
            .find(|g| g.id == Uuid::from_u128(id))
            .expect("row present");
        (
            g.effective_latitude,
            g.effective_longitude,
            g.geo_source,
            g.geo_group.map(|id| id.as_u128()),
        )
    }

    #[test]
    fn a_subgroup_inherits_the_nearest_placed_ancestors_position() {
        // tokyo(placed) → floor2(unplaced) → rack7(unplaced): the whole chain resolves to tokyo's
        // pin. This is the bug the feature exists for — nodes live in racks, the operator places
        // the site, and before inheritance the site pin counted nothing.
        let mut groups = vec![
            geo_row(1, None, Some((35.68, 139.76))),
            geo_row(2, Some(1), None),
            geo_row(3, Some(2), None),
        ];
        resolve_group_geo(&mut groups);
        assert_eq!(
            geo_of(&groups, 1),
            (Some(35.68), Some(139.76), GeoSource::Own, Some(1)),
            "a placed group supplies its own position and names itself as the pin"
        );
        for id in [2, 3] {
            assert_eq!(
                geo_of(&groups, id),
                (Some(35.68), Some(139.76), GeoSource::Inherited, Some(1)),
                "group {id} resolves to the site pin"
            );
        }
    }

    #[test]
    fn the_nearest_placed_ancestor_wins_over_a_farther_one() {
        // region(placed) → site(placed) → rack(unplaced): the rack belongs to the site's pin, not
        // the region's. Nearest wins, exactly as pool and threshold inheritance do.
        let mut groups = vec![
            geo_row(1, None, Some((10.0, 10.0))),
            geo_row(2, Some(1), Some((20.0, 20.0))),
            geo_row(3, Some(2), None),
        ];
        resolve_group_geo(&mut groups);
        assert_eq!(
            geo_of(&groups, 3),
            (Some(20.0), Some(20.0), GeoSource::Inherited, Some(2))
        );
    }

    #[test]
    fn inheritance_never_adds_a_pin() {
        // The load-bearing property: however many groups inherit, the number of groups drawn is
        // still the number carrying their own coordinates. Thirty racks under one building must
        // not become thirty exactly-overlapping pins that hide the building.
        let mut groups = vec![geo_row(1, None, Some((35.0, 139.0)))];
        for i in 2..=31u128 {
            groups.push(geo_row(i, Some(1), None));
        }
        resolve_group_geo(&mut groups);
        assert_eq!(
            groups
                .iter()
                .filter(|g| g.geo_source == GeoSource::Own)
                .count(),
            1,
            "one placed group ⇒ one pin, regardless of how many descendants inherit"
        );
        assert_eq!(
            groups
                .iter()
                .filter(|g| g.geo_group == Some(Uuid::from_u128(1)))
                .count(),
            31,
            "every descendant is counted at that one pin"
        );
    }

    #[test]
    fn an_unplaced_chain_stays_off_the_map() {
        // Nothing placed anywhere, and a dangling parent_id, must both read as "not on the map" —
        // never as (0, 0), which is a real place in the Gulf of Guinea.
        let mut groups = vec![
            geo_row(1, None, None),
            geo_row(2, Some(1), None),
            geo_row(4, Some(9), None),
        ];
        resolve_group_geo(&mut groups);
        for id in [1, 2, 4] {
            assert_eq!(geo_of(&groups, id), (None, None, GeoSource::Unset, None));
        }
    }

    #[test]
    fn a_half_set_or_non_finite_coordinate_is_not_a_placement() {
        // A lone latitude is legacy or hand-edited data (the write path sets both or clears both);
        // treating it as placed would pin the group on the prime meridian. NaN/inf would project
        // to nowhere *and* poison the fit-to-view bounds computed across every other pin.
        let mut groups = vec![
            geo_row(1, None, None),
            geo_row(2, Some(1), None),
            geo_row(3, Some(1), None),
        ];
        groups[0].latitude = Some(35.0); // longitude left null
        groups[1].latitude = Some(f64::NAN);
        groups[1].longitude = Some(139.0);
        groups[2].latitude = Some(35.0);
        groups[2].longitude = Some(f64::INFINITY);
        resolve_group_geo(&mut groups);
        for id in [1, 2, 3] {
            assert_eq!(geo_of(&groups, id), (None, None, GeoSource::Unset, None));
        }
    }

    #[test]
    fn cyclic_ancestry_resolves_to_unplaced_without_hanging() {
        // `would_create_cycle` guards the endpoints that set a parent, but `delete`'s re-parenting
        // does not re-check and there is no DB constraint — so the resolver must survive one.
        let mut groups = vec![geo_row(1, Some(1), None)];
        resolve_group_geo(&mut groups);
        assert_eq!(geo_of(&groups, 1).2, GeoSource::Unset);

        let mut groups = vec![
            geo_row(1, Some(2), None),
            geo_row(2, Some(1), None),
            geo_row(3, None, Some((1.0, 2.0))),
        ];
        resolve_group_geo(&mut groups);
        assert_eq!(geo_of(&groups, 1).2, GeoSource::Unset);
        assert_eq!(geo_of(&groups, 2).2, GeoSource::Unset);
        assert_eq!(
            geo_of(&groups, 3),
            (Some(1.0), Some(2.0), GeoSource::Own, Some(3)),
            "a cycle elsewhere in the forest does not affect a healthy branch"
        );
    }

    #[test]
    fn resolution_is_idempotent() {
        // `list()` fills these on every read; running twice must not drift, and re-resolving rows
        // that already carry an answer must not mistake an inherited value for an own one.
        let mut groups = vec![
            geo_row(1, None, Some((35.0, 139.0))),
            geo_row(2, Some(1), None),
        ];
        resolve_group_geo(&mut groups);
        let once: Vec<_> = groups.iter().map(|g| (g.geo_source, g.geo_group)).collect();
        resolve_group_geo(&mut groups);
        let twice: Vec<_> = groups.iter().map(|g| (g.geo_source, g.geo_group)).collect();
        assert_eq!(once, twice);
        assert_eq!(geo_of(&groups, 2).2, GeoSource::Inherited);
    }

    #[test]
    fn order_between_interpolates_and_extends() {
        // Between two neighbours → midpoint.
        assert_eq!(order_between(Some(1.0), Some(3.0)), 2.0);
        // Append after the last → +1.
        assert_eq!(order_between(Some(5.0), None), 6.0);
        // Prepend before the first → -1.
        assert_eq!(order_between(None, Some(2.0)), 1.0);
        // Only element.
        assert_eq!(order_between(None, None), 0.0);
    }

    #[test]
    fn placement_order_targets_neighbours() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        // Siblings already ordered, NOT including the moving item.
        let sibs = [(a, 1.0), (b, 2.0), (c, 3.0)];

        // Drop before b → midpoint of a and b.
        assert_eq!(placement_order(&sibs, Some(b), None), 1.5);
        // Drop after b → midpoint of b and c.
        assert_eq!(placement_order(&sibs, None, Some(b)), 2.5);
        // Drop before the first → below a.
        assert_eq!(placement_order(&sibs, Some(a), None), 0.0);
        // Drop after the last → above c.
        assert_eq!(placement_order(&sibs, None, Some(c)), 4.0);
        // No / unknown target → append to the end.
        assert_eq!(placement_order(&sibs, None, None), 4.0);
        assert_eq!(placement_order(&sibs, Some(Uuid::from_u128(9)), None), 4.0);
        // Empty scope → 0.
        assert_eq!(placement_order(&[], None, None), 0.0);
    }

    #[test]
    fn cycle_detection() {
        // a → b → c  (c's parent is b, b's parent is a, a is root)
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let edges = vec![(a, None), (b, Some(a)), (c, Some(b))];

        // Moving a under c would make a a descendant of itself → cycle.
        assert!(would_create_cycle(&edges, a, Some(c)));
        // A group cannot be its own parent.
        assert!(would_create_cycle(&edges, b, Some(b)));
        // Moving c under a (a is not below c) is fine.
        assert!(!would_create_cycle(&edges, c, Some(a)));
        // Moving to root is always fine.
        assert!(!would_create_cycle(&edges, b, None));
    }

    #[test]
    fn group_subtree_collects_root_and_descendants() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let d = Uuid::from_u128(4);
        // a → {b, c}, b → d.
        let edges = vec![(a, None), (b, Some(a)), (c, Some(a)), (d, Some(b))];

        let mut from_a = group_subtree(&edges, a);
        from_a.sort();
        assert_eq!(from_a, vec![a, b, c, d]); // whole subtree, incl. the root

        let mut from_b = group_subtree(&edges, b);
        from_b.sort();
        assert_eq!(from_b, vec![b, d]); // a branch

        assert_eq!(group_subtree(&edges, c), vec![c]); // a leaf is just itself
    }

    #[test]
    fn group_subtree_unknown_root_is_just_itself() {
        let x = Uuid::from_u128(9);
        assert_eq!(group_subtree(&[], x), vec![x]);
    }

    #[test]
    fn group_subtree_terminates_on_a_cycle() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        // Malformed data: a ↔ b parent each other. The visited set must stop the walk.
        let edges = vec![(a, Some(b)), (b, Some(a))];
        let mut sub = group_subtree(&edges, a);
        sub.sort();
        assert_eq!(sub, vec![a, b]);
    }

    #[test]
    fn group_ancestors_walks_up_nearest_first_and_excludes_the_start() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let d = Uuid::from_u128(4);
        // a → b → d.
        let edges = vec![(a, None), (b, Some(a)), (d, Some(b))];
        assert_eq!(group_ancestors(&edges, d), vec![b, a]);
        assert_eq!(group_ancestors(&edges, b), vec![a]);
        assert_eq!(group_ancestors(&edges, a), Vec::<Uuid>::new()); // a root has none
    }

    #[test]
    fn group_ancestors_of_an_unknown_group_is_empty() {
        assert_eq!(group_ancestors(&[], Uuid::from_u128(9)), Vec::<Uuid>::new());
    }

    #[test]
    fn group_ancestors_terminates_on_a_cycle() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let edges = vec![(a, Some(b)), (b, Some(a))];
        // Walking up from `a` reaches `b`, then `a` again — which is the start, already seen.
        assert_eq!(group_ancestors(&edges, a), vec![b]);
    }

    // The two walks must agree about direction: nothing above a group may also be below it, or a
    // scoped caller's breadcrumb would quietly hand them a sibling subtree's membership.
    #[test]
    fn a_groups_ancestors_and_its_subtree_never_overlap() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let d = Uuid::from_u128(4);
        let edges = vec![(a, None), (b, Some(a)), (c, Some(a)), (d, Some(b))];
        let sub = group_subtree(&edges, b);
        for up in group_ancestors(&edges, b) {
            assert!(!sub.contains(&up), "{up} is both above and below b");
        }
    }

    /// The longest prefix containing the address wins.
    ///
    /// A site inside a region: both ranges contain the node, and the answer is the site.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_longest_matching_prefix_wins(pool: sqlx::PgPool) {
        let repo = GroupRepo::new(pool.clone());
        let region = crate::pgtest::group(&pool, "Japan").await;
        let site = crate::pgtest::group(&pool, "Tokyo").await;
        crate::pgtest::prefix(&pool, region, "10.0.0.0/8").await;
        crate::pgtest::prefix(&pool, site, "10.1.2.0/24").await;
        let node =
            crate::pgtest::node_at(&pool, "sw", "10.1.2.7".parse().expect("addr"), None).await;

        let hits = repo.match_prefixes(&[node], None).await.expect("match");
        assert_eq!(
            hits.len(),
            1,
            "more than the longest match came back: {hits:?}"
        );
        assert_eq!(hits[0].group, site);
        assert_eq!(hits[0].prefix, "10.1.2.0/24");
    }

    /// Two folders claiming the same address at the same length is ambiguous — both come back, so
    /// the caller can show the choice rather than making it.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn two_folders_at_the_same_length_both_come_back(pool: sqlx::PgPool) {
        let repo = GroupRepo::new(pool.clone());
        let a = crate::pgtest::group(&pool, "Site A").await;
        let b = crate::pgtest::group(&pool, "Site B").await;
        crate::pgtest::prefix(&pool, a, "10.1.2.0/24").await;
        crate::pgtest::prefix(&pool, b, "10.1.2.0/24").await;
        let node =
            crate::pgtest::node_at(&pool, "sw", "10.1.2.7".parse().expect("addr"), None).await;

        let hits = repo.match_prefixes(&[node], None).await.expect("match");
        assert_eq!(hits.len(), 2, "the tie was resolved somewhere: {hits:?}");
    }

    /// An address inside no range is simply absent, and a v4 node never matches a v6 range.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_address_outside_every_range_matches_nothing(pool: sqlx::PgPool) {
        let repo = GroupRepo::new(pool.clone());
        let site = crate::pgtest::group(&pool, "Tokyo").await;
        crate::pgtest::prefix(&pool, site, "10.1.2.0/24").await;
        crate::pgtest::prefix(&pool, site, "2001:db8::/32").await;
        let outside =
            crate::pgtest::node_at(&pool, "far", "192.168.9.9".parse().expect("addr"), None).await;
        let v6 =
            crate::pgtest::node_at(&pool, "v6", "2001:db8::1".parse().expect("addr"), None).await;

        let hits = repo
            .match_prefixes(&[outside, v6], None)
            .await
            .expect("match");
        // The v6 node matches its own family's range; the v4 one matches nothing. `<<=` across
        // families is false rather than an error, which is what makes storing both safe.
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].key, v6);
    }

    /// 🚨 The scope narrows the candidate folders.
    ///
    /// Answering with a folder the caller may not see would hand over the subnet layout of a site
    /// whose membership they were refused — the leak `api::groups::visible_groups` clears
    /// `prefixes` to prevent, met again on a different route.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_scoped_caller_is_not_told_about_a_folder_they_cannot_see(pool: sqlx::PgPool) {
        let repo = GroupRepo::new(pool.clone());
        let mine = crate::pgtest::group(&pool, "Mine").await;
        let theirs = crate::pgtest::group(&pool, "Theirs").await;
        crate::pgtest::prefix(&pool, theirs, "10.1.2.0/24").await;
        let node =
            crate::pgtest::node_at(&pool, "sw", "10.1.2.7".parse().expect("addr"), Some(mine))
                .await;

        assert_eq!(
            repo.match_prefixes(&[node], None)
                .await
                .expect("match")
                .len(),
            1,
            "unrestricted, the folder does claim this node"
        );
        assert!(
            repo.match_prefixes(&[node], Some(&[mine]))
                .await
                .expect("match")
                .is_empty(),
            "a scoped caller was told about a folder outside their scope"
        );
    }

    /// And it narrows the nodes: naming an id the caller cannot read answers nothing.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_scoped_caller_learns_nothing_about_a_node_outside_their_scope(pool: sqlx::PgPool) {
        let repo = GroupRepo::new(pool.clone());
        let mine = crate::pgtest::group(&pool, "Mine").await;
        let theirs = crate::pgtest::group(&pool, "Theirs").await;
        crate::pgtest::prefix(&pool, mine, "10.1.2.0/24").await;
        let hidden =
            crate::pgtest::node_at(&pool, "sw", "10.1.2.7".parse().expect("addr"), Some(theirs))
                .await;

        assert!(
            repo.match_prefixes(&[hidden], Some(&[mine]))
                .await
                .expect("match")
                .is_empty(),
            "a node outside the scope was answered for"
        );
    }

    /// A prefix written with host bits set is stored as its network, so it still contains the
    /// address someone typed it from.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_prefix_with_host_bits_still_matches_its_own_network(pool: sqlx::PgPool) {
        let repo = GroupRepo::new(pool.clone());
        let site = crate::pgtest::group(&pool, "Tokyo").await;
        crate::pgtest::prefix(&pool, site, "10.1.2.5/24").await;
        let node =
            crate::pgtest::node_at(&pool, "sw", "10.1.2.9".parse().expect("addr"), None).await;

        let hits = repo.match_prefixes(&[node], None).await.expect("match");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].prefix, "10.1.2.0/24", "the host bits were kept");
    }

    /// `any_prefixes` tells "nothing matched" apart from "there was nothing to match against".
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn any_prefixes_answers_for_the_callers_scope(pool: sqlx::PgPool) {
        let repo = GroupRepo::new(pool.clone());
        let mine = crate::pgtest::group(&pool, "Mine").await;
        let theirs = crate::pgtest::group(&pool, "Theirs").await;
        assert!(
            !repo.any_prefixes(None).await.expect("read"),
            "a fresh database has none"
        );

        crate::pgtest::prefix(&pool, theirs, "10.1.2.0/24").await;
        assert!(repo.any_prefixes(None).await.expect("read"));
        assert!(
            !repo.any_prefixes(Some(&[mine])).await.expect("read"),
            "a range outside the scope was counted"
        );
    }

    // ── The shared fold (moved from `api/nodes.rs` by ADR-131) ──────────────────────────

    /// The fold, without a database: which of the three answers each key lands in.
    #[test]
    fn folding_hits_separates_one_folder_from_two_and_from_none() {
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let (g1, g2) = (Uuid::new_v4(), Uuid::new_v4());
        let hit = |key, group| PrefixHit {
            key,
            group,
            prefix: "10.0.0.0/24".to_string(),
        };
        // `a` is claimed once, `b` twice, `c` not at all — and `a` is named twice in the request.
        let fold = fold_prefix_matches(&[a, b, c, a], vec![hit(a, g1), hit(b, g1), hit(b, g2)]);
        assert_eq!(fold.matched.len(), 1);
        assert_eq!(fold.matched[0].0, a);
        assert_eq!(fold.ambiguous.len(), 1);
        assert_eq!(fold.ambiguous[0].1, vec![g1, g2]);
        assert_eq!(fold.unmatched, vec![c], "a repeated id was answered twice");
    }

    /// One folder carrying two ranges that both contain the address is still one folder.
    #[test]
    fn folding_hits_does_not_call_one_folder_a_tie() {
        let node = Uuid::new_v4();
        let group = Uuid::new_v4();
        let hit = |prefix: &str| PrefixHit {
            key: node,
            group,
            prefix: prefix.to_string(),
        };
        let fold = fold_prefix_matches(&[node], vec![hit("10.0.0.0/24"), hit("10.0.0.0/24")]);
        assert_eq!(fold.matched.len(), 1, "one folder read as a tie");
        assert!(fold.ambiguous.is_empty());
    }

    /// The same fold, keyed by an address — the shape ADR-131's import path uses.
    ///
    /// Worth its own test rather than trusting the generic: `IpAddr` is the key type where a
    /// mixed-family list is normal, and an IPv6 key must not collide with an IPv4 one.
    #[test]
    fn folding_hits_works_the_same_when_the_key_is_an_address() {
        let v4: IpAddr = "192.168.1.10".parse().expect("v4");
        let v6: IpAddr = "2001:db8::1".parse().expect("v6");
        let absent: IpAddr = "10.9.9.9".parse().expect("v4");
        let (g1, g2) = (Uuid::new_v4(), Uuid::new_v4());
        let hit = |key, group, prefix: &str| PrefixHit {
            key,
            group,
            prefix: prefix.to_string(),
        };
        let fold = fold_prefix_matches(
            &[v4, v6, absent],
            vec![
                hit(v4, g1, "192.168.1.0/24"),
                hit(v6, g1, "2001:db8::/32"),
                hit(v6, g2, "2001:db8::/32"),
            ],
        );
        assert_eq!(
            fold.matched.len(),
            1,
            "only the v4 address landed on one folder"
        );
        assert_eq!(fold.matched[0].0, v4);
        assert_eq!(
            fold.matched[0].2, "192.168.1.0/24",
            "the range that matched is reported"
        );
        assert_eq!(fold.ambiguous.len(), 1);
        assert_eq!(fold.ambiguous[0].0, v6);
        assert_eq!(fold.unmatched, vec![absent]);
    }

    /// `PrefixSource` round-trips through serde with the tokens the WebUI branches on.
    ///
    /// The TypeScript side has a `PREFIX_SOURCES` array and builds `t()` keys from it, so a
    /// rename here that nothing compared would render a raw key in both locales at once.
    #[test]
    fn prefix_source_serializes_as_the_tokens_the_web_ui_expects() {
        assert_eq!(
            serde_json::to_string(&PrefixSource::Manual).expect("manual"),
            "\"manual\""
        );
        assert_eq!(
            serde_json::to_string(&PrefixSource::Sync).expect("sync"),
            "\"sync\""
        );
    }

    // ── ADR-131: matching addresses that are not nodes yet, and hand-made ranges ─────────

    /// Longest prefix wins, a same-length tie comes back twice, and an address outside every
    /// range is simply absent.
    ///
    /// The address-keyed twin of `match_prefixes`' own tests. Worth repeating rather than trusting
    /// the shared fold: this query joins against `unnest`, not `nodes`, so the containment and the
    /// `MAX(masklen)` correlation are a second implementation of the same rule.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn matching_addresses_narrows_to_the_longest_prefix(pool: sqlx::PgPool) {
        let repo = GroupRepo::new(pool.clone());
        let broad = crate::pgtest::group(&pool, "broad").await;
        let narrow = crate::pgtest::group(&pool, "narrow").await;
        crate::pgtest::prefix(&pool, broad, "10.0.0.0/8").await;
        crate::pgtest::prefix(&pool, narrow, "10.1.2.0/24").await;

        let inside: IpAddr = "10.1.2.5".parse().expect("addr");
        let broader: IpAddr = "10.9.9.9".parse().expect("addr");
        let outside: IpAddr = "192.0.2.1".parse().expect("addr");
        let hits = repo
            .match_address_prefixes(&[inside, broader, outside], None)
            .await
            .expect("match");
        let fold = fold_prefix_matches(&[inside, broader, outside], hits);
        assert_eq!(fold.matched.len(), 2, "{fold:?}");
        let by_key: HashMap<IpAddr, Uuid> = fold.matched.iter().map(|(k, g, _)| (*k, *g)).collect();
        assert_eq!(by_key[&inside], narrow, "the /24 beat the /8");
        assert_eq!(by_key[&broader], broad);
        assert_eq!(fold.unmatched, vec![outside]);
        assert!(fold.ambiguous.is_empty());
    }

    /// Two folders claiming an address at the same length is reported, never resolved.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn matching_addresses_reports_a_tie_rather_than_choosing(pool: sqlx::PgPool) {
        let repo = GroupRepo::new(pool.clone());
        let a = crate::pgtest::group(&pool, "a").await;
        let b = crate::pgtest::group(&pool, "b").await;
        crate::pgtest::prefix(&pool, a, "192.168.1.0/24").await;
        crate::pgtest::prefix(&pool, b, "192.168.1.0/24").await;

        let addr: IpAddr = "192.168.1.50".parse().expect("addr");
        let hits = repo
            .match_address_prefixes(&[addr], None)
            .await
            .expect("match");
        let fold = fold_prefix_matches(&[addr], hits);
        assert!(fold.matched.is_empty(), "a tie must not be filed");
        assert_eq!(fold.ambiguous.len(), 1);
        let mut claimed = fold.ambiguous[0].1.clone();
        claimed.sort();
        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(claimed, expected);
    }

    /// 🚨 The key that comes back parses into the address that was sent.
    ///
    /// The projection is `host(addr)`, not `addr::TEXT` — the latter **adds a mask**
    /// (`10.0.0.1/32`), which is the defect `arp.rs` shipped for a release and which here would
    /// make every candidate read as filed nowhere. IPv6 is in the fixture because that is where a
    /// text round trip is most likely to change spelling.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn matching_addresses_returns_keys_that_are_the_addresses_sent(pool: sqlx::PgPool) {
        let repo = GroupRepo::new(pool.clone());
        let g = crate::pgtest::group(&pool, "mixed").await;
        crate::pgtest::prefix(&pool, g, "192.168.1.0/24").await;
        crate::pgtest::prefix(&pool, g, "2001:db8::/32").await;

        // Deliberately upper-case, so a text comparison rather than a parse would fail here.
        let v6: IpAddr = "2001:DB8::1".parse().expect("v6");
        let v4: IpAddr = "192.168.1.1".parse().expect("v4");
        let hits = repo
            .match_address_prefixes(&[v4, v6], None)
            .await
            .expect("match");
        assert_eq!(hits.len(), 2, "{hits:?}");
        let keys: HashSet<IpAddr> = hits.iter().map(|h| h.key).collect();
        assert!(keys.contains(&v4), "{keys:?}");
        assert!(keys.contains(&v6), "{keys:?}");
        // And the fold agrees, which is the property the import path actually depends on.
        let fold = fold_prefix_matches(&[v4, v6], hits);
        assert_eq!(fold.matched.len(), 2, "{fold:?}");
        assert!(fold.unmatched.is_empty());
    }

    /// An IPv4 address against an IPv6 range is `false`, never an error — so storing both is safe.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn matching_addresses_does_not_cross_address_families(pool: sqlx::PgPool) {
        let repo = GroupRepo::new(pool.clone());
        let g = crate::pgtest::group(&pool, "v6 only").await;
        crate::pgtest::prefix(&pool, g, "2001:db8::/32").await;

        let v4: IpAddr = "192.168.1.1".parse().expect("v4");
        let v6: IpAddr = "2001:db8::5".parse().expect("v6");
        let hits = repo
            .match_address_prefixes(&[v4, v6], None)
            .await
            .expect("match");
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].key, v6);
    }

    /// 🚨 The scope narrows the **folder** side, and nothing else.
    ///
    /// Unlike `match_prefixes` there is no node side to narrow: the addresses are the caller's own
    /// sweep results, which they already hold. What must still be true is that a folder outside the
    /// scope never appears in the answer — that is the leak `visible_groups` clears prefixes to
    /// prevent, and it would be handed over here instead.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn matching_addresses_narrows_the_folder_side_only(pool: sqlx::PgPool) {
        let repo = GroupRepo::new(pool.clone());
        let mine = crate::pgtest::group(&pool, "mine").await;
        let theirs = crate::pgtest::group(&pool, "theirs").await;
        crate::pgtest::prefix(&pool, mine, "10.0.0.0/8").await;
        crate::pgtest::prefix(&pool, theirs, "10.1.2.0/24").await;

        let addr: IpAddr = "10.1.2.5".parse().expect("addr");
        // Unscoped, the /24 wins.
        let unscoped = repo
            .match_address_prefixes(&[addr], None)
            .await
            .expect("match");
        assert_eq!(unscoped.len(), 1);
        assert_eq!(unscoped[0].group, theirs);

        // Scoped to `mine`, the other folder's range is invisible — and the answer falls back to
        // the longest prefix *among the folders the caller may see*, rather than to nothing.
        let scoped = repo
            .match_address_prefixes(&[addr], Some(&[mine]))
            .await
            .expect("match");
        assert_eq!(scoped.len(), 1, "{scoped:?}");
        assert_eq!(scoped[0].group, mine);
        // The address itself was never filtered: it is answered for, which is the asymmetry.
        assert_eq!(scoped[0].key, addr);
    }

    /// `canonical_prefix` accepts host bits and normalises them; a non-address is `None`.
    ///
    /// 🚨 And the pool is still usable afterwards — that is the whole reason the probe runs outside
    /// a transaction. A failed statement poisons its transaction, so probing inside the write would
    /// abort the very thing it guards.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn canonicalising_a_prefix_accepts_host_bits_and_refuses_a_non_address(
        pool: sqlx::PgPool,
    ) {
        let repo = GroupRepo::new(pool.clone());
        assert_eq!(
            repo.canonical_prefix("192.168.1.5/24")
                .await
                .expect("probe"),
            Some("192.168.1.0/24".to_string()),
            "a plain ::cidr cast would have rejected this"
        );
        assert_eq!(
            repo.canonical_prefix("2001:db8::1/64")
                .await
                .expect("probe"),
            Some("2001:db8::/64".to_string())
        );
        assert_eq!(
            repo.canonical_prefix("not-an-address")
                .await
                .expect("probe"),
            None
        );
        // The refusal did not take the connection with it.
        assert_eq!(
            repo.canonical_prefix("10.0.0.0/8").await.expect("probe"),
            Some("10.0.0.0/8".to_string())
        );
    }

    /// 🚨 Replacing the hand-made ranges leaves a sync's rows exactly where they are.
    ///
    /// The failure this exists for is not a crash: a plain "replace the folder's list" would delete
    /// rows NetBox owns, NetBox would re-create them on its next run, and the edit would appear to
    /// work and silently undo itself.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn setting_manual_prefixes_never_touches_a_syncs_rows(pool: sqlx::PgPool) {
        let repo = GroupRepo::new(pool.clone());
        let g = crate::pgtest::group(&pool, "site").await;
        let server = crate::pgtest::netbox_server(&pool, "nb").await;
        sqlx::query(
            "INSERT INTO node_group_prefixes (group_id, prefix, description, netbox_server_id) \
             VALUES ($1, network($2::inet)::cidr, 'from netbox', $3)",
        )
        .bind(g)
        .bind("172.16.0.0/12")
        .bind(server)
        .execute(&pool)
        .await
        .expect("seed sync row");

        repo.set_manual_prefixes(
            g,
            &[
                ("192.168.1.0/24".into(), "office".into()),
                ("10.0.0.0/8".into(), String::new()),
            ],
        )
        .await
        .expect("set");
        assert_eq!(crate::pgtest::rows(&pool, "node_group_prefixes").await, 3);

        // A second write replaces only what this endpoint created.
        repo.set_manual_prefixes(g, &[("192.168.2.0/24".into(), "annex".into())])
            .await
            .expect("set again");
        let listed = repo.list().await.expect("list");
        let folder = listed.first().expect("one folder");
        let mut seen: Vec<(String, PrefixSource)> = folder
            .prefixes
            .iter()
            .map(|p| (p.prefix.clone(), p.source))
            .collect();
        seen.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            seen,
            vec![
                ("172.16.0.0/12".to_string(), PrefixSource::Sync),
                ("192.168.2.0/24".to_string(), PrefixSource::Manual),
            ],
            "the sync row survived both writes and the first manual pair was replaced"
        );

        // Clearing removes every manual row and still leaves the sync's.
        repo.set_manual_prefixes(g, &[]).await.expect("clear");
        assert_eq!(crate::pgtest::rows(&pool, "node_group_prefixes").await, 1);
    }

    /// A hand-made row is **not** swept when a sync prunes what NetBox no longer mentions.
    ///
    /// Migration 0104's header claims this and nothing tested it. It holds because the sweep is
    /// `WHERE netbox_server_id = $1` and `NULL = $1` is never true.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_hand_made_range_survives_a_syncs_stale_sweep(pool: sqlx::PgPool) {
        let repo = GroupRepo::new(pool.clone());
        let g = crate::pgtest::group(&pool, "site").await;
        let server = crate::pgtest::netbox_server(&pool, "nb").await;
        repo.set_manual_prefixes(g, &[("192.168.1.0/24".into(), "typed".into())])
            .await
            .expect("set");

        // The sweep's own statement, with a cutoff in the future so it would delete anything it
        // was entitled to delete.
        let swept = sqlx::query(
            "DELETE FROM node_group_prefixes \
             WHERE netbox_server_id = $1 AND last_seen_at < now() + interval '1 hour'",
        )
        .bind(server)
        .execute(&pool)
        .await
        .expect("sweep");
        assert_eq!(swept.rows_affected(), 0);
        assert_eq!(crate::pgtest::rows(&pool, "node_group_prefixes").await, 1);
    }

    /// The other direction, decided by ADR-131 決定 6: a sync that learns a hand-made CIDR
    /// **takes the row over**, and it then becomes sweepable.
    ///
    /// This was already how `netbox.rs`'s `ON CONFLICT ... DO UPDATE SET netbox_server_id =
    /// EXCLUDED.netbox_server_id` behaved; it was an accident of the clause rather than a decision.
    /// Pinned here so changing it is deliberate.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_sync_that_learns_a_hand_made_range_takes_it_over(pool: sqlx::PgPool) {
        let repo = GroupRepo::new(pool.clone());
        let g = crate::pgtest::group(&pool, "site").await;
        let server = crate::pgtest::netbox_server(&pool, "nb").await;
        repo.set_manual_prefixes(g, &[("192.168.1.0/24".into(), "typed".into())])
            .await
            .expect("set");

        // `netbox.rs::upsert_prefix`'s statement, verbatim in shape.
        sqlx::query(
            "INSERT INTO node_group_prefixes \
               (group_id, prefix, description, netbox_server_id, last_seen_at) \
             VALUES ($1, network($2::inet)::cidr, $3, $4, now()) \
             ON CONFLICT (group_id, prefix) DO UPDATE SET \
               description = EXCLUDED.description, \
               netbox_server_id = EXCLUDED.netbox_server_id, \
               last_seen_at = now()",
        )
        .bind(g)
        .bind("192.168.1.0/24")
        .bind("Matsuyama LAN")
        .bind(server)
        .execute(&pool)
        .await
        .expect("sync upsert");

        let listed = repo.list().await.expect("list");
        let folder = listed.first().expect("one folder");
        assert_eq!(folder.prefixes.len(), 1);
        assert_eq!(
            folder.prefixes[0].source,
            PrefixSource::Sync,
            "the row is the sync's now"
        );
        // And `set_manual_prefixes` can no longer remove it — which is what the API's
        // `prefix_owned_by_sync` refusal exists to explain rather than to hide.
        repo.set_manual_prefixes(g, &[]).await.expect("clear");
        assert_eq!(crate::pgtest::rows(&pool, "node_group_prefixes").await, 1);
    }

    /// `sync_owned_prefixes` names only the sync's rows, canonicalised.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn sync_owned_prefixes_lists_only_what_a_sync_wrote(pool: sqlx::PgPool) {
        let repo = GroupRepo::new(pool.clone());
        let g = crate::pgtest::group(&pool, "site").await;
        let server = crate::pgtest::netbox_server(&pool, "nb").await;
        repo.set_manual_prefixes(g, &[("10.0.0.0/8".into(), String::new())])
            .await
            .expect("set");
        sqlx::query(
            "INSERT INTO node_group_prefixes (group_id, prefix, description, netbox_server_id) \
             VALUES ($1, network($2::inet)::cidr, '', $3)",
        )
        .bind(g)
        .bind("172.16.0.0/12")
        .bind(server)
        .execute(&pool)
        .await
        .expect("seed");
        assert_eq!(
            repo.sync_owned_prefixes(g).await.expect("read"),
            vec!["172.16.0.0/12".to_string()]
        );
    }
}
